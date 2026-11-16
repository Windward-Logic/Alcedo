//! Safety nets around Send: pre-send confirmation (empty subject, forgotten
//! attachment) and an optional undo-send delay.

use alcedo_core::compose_checks::pre_send_warnings;
use alcedo_core::compose_status::{delayed_send_toast_title, undo_delayed_send_toast};

use super::*;

/// A send held back by the undo-send delay.
pub(super) struct DelayedSend {
    timer: glib::SourceId,
    toast: adw::Toast,
}

impl MailModel {
    /// Ask before sending a draft that looks unfinished. Returns `true` when a
    /// dialog is up; the send continues with [`MailMsg::PreSendConfirmed`].
    pub(super) fn present_pre_send_warning_if_needed(
        &self,
        draft: &ComposeDraft,
        window_id: Option<u64>,
    ) -> bool {
        let warnings = pre_send_warnings(&draft.subject, &draft.body_text, draft.attachments.len());
        let Some(first) = warnings.first().copied() else {
            return false;
        };
        let body = warnings
            .iter()
            .map(|warning| warning.body())
            .collect::<Vec<_>>()
            .join("\n\n");
        let dialog = adw::AlertDialog::new(Some(first.heading()), Some(&body));
        dialog.set_heading_use_markup(false);
        dialog.set_body_use_markup(false);
        dialog.set_heading(Some(first.heading()));
        dialog.set_body(&body);
        dialog.add_response("cancel", "Keep Editing");
        dialog.add_response("send", "Send Anyway");
        dialog.set_response_appearance("send", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        let sender = self.mail_sender.clone();
        let draft = draft.clone();
        let parent = match window_id.and_then(|id| self.composer_windows.get(&id)) {
            Some(window) => window.dialog_parent().clone().upcast::<gtk::Widget>(),
            None => self.window.clone().upcast::<gtk::Widget>(),
        };
        dialog.choose(
            Some(&parent),
            None::<&gtk::gio::Cancellable>,
            move |response| {
                if response.as_str() == "send" {
                    sender
                        .send(MailMsg::PreSendConfirmed { draft, window_id })
                        .ok();
                }
            },
        );
        true
    }

    /// Hold the queued draft for `delay_secs` with an Undo toast. The draft is
    /// already recorded in `pending_send_draft`.
    pub(super) fn hold_send_for_undo(&mut self, delay_secs: u32) {
        let sender = self.mail_sender.clone();
        let timer = glib::timeout_add_seconds_local_once(delay_secs, move || {
            sender.send(MailMsg::DelayedSendElapsed).ok();
        });
        let toast =
            crate::toast::plain_with_timeout(&delayed_send_toast_title(delay_secs), delay_secs);
        toast.set_button_label(Some("Undo"));
        let sender = self.mail_sender.clone();
        toast.connect_button_clicked(move |_| {
            sender.send(MailMsg::UndoSend).ok();
        });
        self.toasts.add_toast(toast.clone());
        self.delayed_send = Some(DelayedSend { timer, toast });
    }

    pub(super) fn release_delayed_send(&mut self) {
        let Some(delayed) = self.delayed_send.take() else {
            return;
        };
        delayed.toast.dismiss();
        let Some(draft) = self.pending_send_draft.clone() else {
            return;
        };
        self.cmd.emit(UiCommand::Send(draft));
    }

    pub(super) fn undo_delayed_send(&mut self, sender: &ComponentSender<MailModel>) {
        let Some(delayed) = self.delayed_send.take() else {
            return;
        };
        delayed.timer.remove();
        delayed.toast.dismiss();
        let draft = self.pending_send_draft.take();
        let window_id = self.pending_send_window.take();
        self.send_in_flight.set(false);
        self.send_surface_released = false;
        self.forget_queued_send();
        if let Some(draft) = draft {
            self.restore_unsent_draft(draft, window_id, sender);
        }
        self.toasts
            .add_toast(crate::toast::plain(undo_delayed_send_toast()));
    }
}
