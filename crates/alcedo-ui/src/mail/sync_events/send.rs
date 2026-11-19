//! Send progress and IMAP draft save results.

use super::super::send::sent_should_close_composer;
use super::*;
use crate::mail::drafts::imap_draft_ref_matches_envelope;
use alcedo_i18n::{trf, trs};

impl MailModel {
    pub(super) fn on_send_sync_event(
        &mut self,
        event: SyncEvent,
        sender: &ComponentSender<MailModel>,
    ) {
        match event {
            SyncEvent::SendProgress { state, .. } => match state {
                SendState::Queued | SendState::Sending => {}
                SendState::Sent { message_id_hdr } => {
                    if self.pending_send_draft.is_some() {
                        let pending = self.pending_send_draft.take();
                        let is_reaction = pending
                            .as_ref()
                            .and_then(|draft| draft.reaction_emoji.as_ref())
                            .is_some();
                        let invite_organizer_toast = self.pending_invite_response_organizer.take();

                        if let Some(draft) = pending.as_ref() {
                            let from = self
                                .accounts
                                .iter()
                                .find(|account| account.id == draft.account_id)
                                .map(|account| account.email.as_str())
                                .unwrap_or("");
                            let success_kind = if is_reaction {
                                trs!("Reaction sent")
                            } else if invite_organizer_toast.is_some() {
                                trs!("Invite response sent")
                            } else {
                                trs!("Message sent")
                            };
                            self.push_activity(
                                activity_log::KIND_MAIL,
                                ActivityOutcome::Success,
                                success_kind,
                                self.send_detail_from_draft(draft, None),
                                from,
                                "",
                            );
                        }

                        if is_reaction {
                            self.pending_send_window.take();
                            let sent_emoji = pending
                                .as_ref()
                                .and_then(|draft| draft.reaction_emoji.as_deref());
                            if let Some(emoji) = sent_emoji {
                                self.toasts.add_toast(crate::toast::plain(&trf!(
                                    "Sent reaction {emoji}",
                                    emoji = emoji
                                )));
                            }
                            if let Some(target_id) = self.pending_reaction_target.take() {
                                if let Some(emoji) = sent_emoji {
                                    if let Some(chips) =
                                        self.optimistic_reactions.get_mut(&target_id)
                                    {
                                        chips.retain(|chip| chip.emoji != emoji);
                                    }
                                    if self
                                        .optimistic_reactions
                                        .get(&target_id)
                                        .is_some_and(|chips| chips.is_empty())
                                    {
                                        self.optimistic_reactions.remove(&target_id);
                                    }
                                }
                                if !self.mock {
                                    self.cmd.emit(UiCommand::LoadReactions { target_id });
                                }
                                self.refresh_reaction_chips_for(target_id);
                            }
                        } else {
                            self.forget_queued_send();
                            let released = self.send_surface_released;
                            self.send_surface_released = false;
                            self.composer_send.set_sending(&self.composer, false);
                            if sent_should_close_composer(released) {
                                if let Some(id) = self.pending_send_window.take() {
                                    clear_compose_draft_slot(DraftSlot::window(id));
                                    if let Some(window) = self.composer_windows.remove(&id) {
                                        window.close_after_send();
                                    }
                                } else {
                                    clear_composer(&self.composer);
                                    self.apply_reader_after_inline_send();
                                }
                            } else {
                                self.pending_send_window.take();
                                if self.reopen_reader_after_send
                                    && self.mail_content_pane != MailContentPane::Composer
                                {
                                    self.apply_reader_after_inline_send();
                                }
                            }
                            self.show_sent_toast(
                                pending.as_ref().and_then(|draft| draft.thread_id),
                                pending.as_ref().map(|draft| draft.account_id),
                                message_id_hdr,
                            );
                            if let Some(label) = invite_organizer_toast {
                                self.toasts.add_toast(crate::toast::plain(&trf!(
                                    "Response sent to {organizer}",
                                    organizer = label
                                )));
                            }
                        }
                    }
                    self.pending_reaction_target = None;
                    self.send_in_flight.set(false);
                }
                SendState::Failed(msg) => {
                    self.pending_invite_response_organizer = None;
                    // Always unlock compose surfaces; a detached-window send must not leave
                    // the inline composer stuck in "Sending…" with fields disabled.
                    self.composer_send.set_sending(&self.composer, false);
                    for window in self.composer_windows.values() {
                        window.set_sending(false);
                    }
                    self.send_surface_released = false;
                    let draft = self.pending_send_draft.take();
                    let is_reaction = draft
                        .as_ref()
                        .and_then(|draft| draft.reaction_emoji.as_ref())
                        .is_some();
                    if is_reaction {
                        let failed_emoji = draft
                            .as_ref()
                            .and_then(|draft| draft.reaction_emoji.clone());
                        if let Some(target_id) = self.pending_reaction_target.take() {
                            if let Some(emoji) = failed_emoji {
                                if let Some(chips) = self.optimistic_reactions.get_mut(&target_id) {
                                    chips.retain(|chip| chip.emoji != emoji);
                                }
                                if self
                                    .optimistic_reactions
                                    .get(&target_id)
                                    .is_some_and(|chips| chips.is_empty())
                                {
                                    self.optimistic_reactions.remove(&target_id);
                                }
                            }
                            self.refresh_reaction_chips_for(target_id);
                        }
                    } else {
                        self.forget_queued_send();
                        self.pending_reaction_target = None;
                        if let Some(draft) = draft.as_ref() {
                            let from = self
                                .accounts
                                .iter()
                                .find(|account| account.id == draft.account_id)
                                .map(|account| account.email.as_str())
                                .unwrap_or("");
                            let debug = self
                                .accounts
                                .iter()
                                .find(|account| account.id == draft.account_id)
                                .map(|account| self.account_debug(account, &msg))
                                .unwrap_or_else(|| format!("status: {msg}"));
                            self.push_activity(
                                activity_log::KIND_MAIL,
                                ActivityOutcome::Failure,
                                trs!("Send failed"),
                                self.send_detail_from_draft(draft, Some(&msg)),
                                from,
                                debug,
                            );
                        }
                        if let Some(draft) = draft {
                            let window_id = self.pending_send_window.take();
                            self.restore_unsent_draft(draft, window_id, sender);
                        } else {
                            self.pending_send_window.take();
                        }
                    }
                    self.toasts.add_toast(crate::toast::plain(&trf!(
                        "Send failed: {reason}",
                        reason = msg
                    )));
                    self.send_in_flight.set(false);
                }
            },
            SyncEvent::ImapDraftSaved {
                account: _,
                draft,
                replaced_newer,
                removed_missing,
                kept_newer,
                save_detail,
            } => {
                self.stamp_imap_draft_ref(&draft);
                if let Some(imap) = draft.imap_draft.as_ref()
                    && let Some(env) = self
                        .envelopes
                        .iter()
                        .find(|env| imap_draft_ref_matches_envelope(imap, env))
                {
                    self.threading_hdrs
                        .insert(env.id, (Some(imap.message_id.clone()), None));
                }
                // The server copy now covers this composer; drop its local file.
                self.clear_autosave_for_imap_draft(&draft);
                if kept_newer {
                    let message =
                        trs!("Kept the newer copy of this draft from the other mail program.");
                    self.toasts.add_toast(crate::toast::plain(message));
                    self.record(activity_log::KIND_MAIL, ActivityOutcome::Info, message, "");
                }
                if replaced_newer {
                    let message = trs!("Replaced a newer copy of this draft on the server.");
                    self.toasts.add_toast(crate::toast::plain(message));
                    self.record(activity_log::KIND_MAIL, ActivityOutcome::Info, message, "");
                }
                if removed_missing {
                    let message = trs!("Original draft was removed; saved a new copy.");
                    self.toasts.add_toast(crate::toast::plain(message));
                    self.record(
                        activity_log::KIND_MAIL,
                        ActivityOutcome::Info,
                        message,
                        save_detail,
                    );
                }
            }
            SyncEvent::ImapDraftSaveFailed { message } => {
                let title = trf!("Couldn't save to Drafts: {reason}", reason = message);
                self.toasts.add_toast(crate::toast::plain(&title));
                self.record(activity_log::KIND_MAIL, ActivityOutcome::Failure, title, "");
                self.mark_drafts_server_save_failed();
            }
            SyncEvent::ImapDraftReady { message_id, draft } => {
                self.apply_imap_draft_ready(message_id, draft, sender);
            }
            SyncEvent::QueuedSendsRecovered(sends) => {
                sender.input(MailMsg::RestoreQueuedSends(sends));
            }
            _ => unreachable!("sync event not routed to send"),
        }
    }
}
