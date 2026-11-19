//! Desktop-notification follow-ups: open, mark read, delete and copy code.

use super::*;
use alcedo_i18n::tr;

impl MailModel {
    pub(super) fn update_notify(&mut self, msg: MailMsg, sender: ComponentSender<MailModel>) {
        match msg {
            MailMsg::OpenNotificationMessage(id) => {
                self.show_reader();
                let Some(folder) = self.last_notified.get(&id).copied().or_else(|| {
                    self.envelopes
                        .iter()
                        .find(|env| env.id == id)
                        .map(|env| env.folder_id)
                }) else {
                    return;
                };
                self.try_open_notification_message(id, folder, &sender);
            }
            MailMsg::CopyNotifiedVerificationCode(id) => {
                let code = self
                    .notified_otp
                    .get(&id)
                    .map(|(code, _)| code.clone())
                    .or_else(|| notifications::pending_notification_otp(id.0));
                let Some(code) = code else {
                    notifications::clear_pending_notification_otp_if(id.0);
                    notifications::withdraw_message_notification(id);
                    return;
                };
                notifications::copy_text_to_clipboard(&code);
                notifications::clear_pending_notification_otp_if(id.0);
                notifications::withdraw_message_notification(id);
                if cleanup_verification_codes() {
                    self.cleanup_verification_message(id, &sender);
                } else {
                    self.toasts
                        .add_toast(crate::toast::plain(&tr!("Code copied")));
                }
                self.notified_otp.remove(&id);
            }
            MailMsg::MarkNotifiedMessageRead(id) => {
                self.clear_notification_pending(id);
                self.mark_message_seen(id);
                self.sync_selection_chrome();
                self.refresh_status_bar();
            }
            MailMsg::DeleteNotifiedMessage(id) => {
                self.clear_notification_pending(id);
                self.mark_message_seen(id);
                self.delete_message_ids(&[id], &sender, DeleteToast::Default);
            }
            _ => unreachable!("message not routed to notify"),
        }
    }

    pub(super) fn clear_notification_pending(&mut self, id: MessageId) {
        self.pending_notification_message = None;
        self.pending_notification_tried_folder = false;
        self.last_notified.remove(&id);
        self.notified_otp.remove(&id);
        notifications::clear_pending_notification_otp_if(id.0);
    }

    pub(super) fn open_pending_notification_message(&mut self, id: MessageId) {
        select_ids(&self.list_view, &[id]);
        self.queue_open_and_prefetch(id);
        self.open_message(id);
        restore_reader_row(&self.list_view, id);
        self.clear_notification_pending(id);
        notifications::withdraw_message_notification(id);
    }

    pub(super) fn try_open_notification_message(
        &mut self,
        id: MessageId,
        folder: FolderId,
        sender: &ComponentSender<MailModel>,
    ) {
        if self.envelopes.iter().any(|env| env.id == id) {
            self.open_pending_notification_message(id);
            return;
        }
        self.pending_notification_message = Some((id, folder));
        if self.mail_view != MailView::UnifiedInbox {
            sender.input(MailMsg::SelectUnifiedInbox);
            return;
        }
        if self.pending_folder_sync.is_none() && !self.envelopes.is_empty() {
            if !self.pending_notification_tried_folder {
                self.pending_notification_tried_folder = true;
                sender.input(MailMsg::SelectFolder(folder));
            } else {
                self.clear_notification_pending(id);
            }
        }
    }

    pub(super) fn finish_pending_notification(&mut self, sender: &ComponentSender<MailModel>) {
        let Some((id, folder)) = self.pending_notification_message else {
            return;
        };
        if self.envelopes.iter().any(|env| env.id == id) {
            self.open_pending_notification_message(id);
            return;
        }
        if self.mail_view != MailView::UnifiedInbox {
            sender.input(MailMsg::SelectUnifiedInbox);
            return;
        }
        if self.pending_folder_sync.is_none() && !self.envelopes.is_empty() {
            if !self.pending_notification_tried_folder {
                self.pending_notification_tried_folder = true;
                sender.input(MailMsg::SelectFolder(folder));
            } else {
                self.clear_notification_pending(id);
            }
        }
    }

    pub(super) fn observe_inbox_envelopes(
        &mut self,
        folder: FolderId,
        incoming: &[Envelope],
        root: &adw::ApplicationWindow,
        complete_folder: bool,
    ) {
        if !self.folder_is_special(folder, &SpecialUse::Inbox) {
            return;
        }
        let new_unseen = notifications::observe_inbox_batch(
            &mut self.known_inbox_ids,
            &mut self.inbox_notify_ready,
            folder,
            incoming,
            complete_folder,
        );
        if new_unseen.is_empty() || notifications::is_attending_main(root) {
            return;
        }
        let newest_id = new_unseen.last().map(|env| env.id);
        for envelope in new_unseen {
            if self.reaction_message_ids.contains(&envelope.id) {
                continue;
            }
            let prominent = Some(envelope.id) == newest_id;
            let otp = if prominent && show_verification_codes() {
                notifications::notification_otp_code(&envelope.display_subject(), &envelope.snippet)
            } else {
                None
            };
            if !notifications::should_notify(
                envelope,
                otp.as_deref(),
                &self.contact_book.borrow().contacts,
            ) {
                continue;
            }
            if let Some(ref code) = otp {
                self.notified_otp
                    .insert(envelope.id, (code.clone(), envelope.clone()));
            }
            self.last_notified.insert(envelope.id, envelope.folder_id);
            notifications::send_new_mail_notification(envelope, otp.as_deref(), prominent);
        }
    }
}
