//! Reader-facing updates: bodies, reactions, unsubscribe, raw source and remote-image senders.

use super::*;
use alcedo_i18n::trf;

impl MailModel {
    pub(super) fn on_reader_sync_event(&mut self, event: SyncEvent, root: &adw::ApplicationWindow) {
        match event {
            SyncEvent::ReactionsLoaded { target_id, people } => {
                self.reactions_by_target.insert(target_id, people);
                self.refresh_reaction_chips_for(target_id);
            }
            SyncEvent::ReactionOriginalReady {
                reaction_id,
                target_id,
                subject,
                text,
                html,
            } => {
                let missing = text.trim().is_empty() && html.trim().is_empty();
                self.reaction_message_ids.insert(reaction_id);
                self.remember_reaction_original(reaction_id, target_id, subject, text, html);
                self.refresh_react_enabled_for(reaction_id);
                if missing && !self.mock {
                    self.cmd.emit(UiCommand::PrefetchMessage(target_id));
                }
            }
            SyncEvent::ReactionReceived {
                reaction_id,
                target_id,
                emoji,
                from_name,
                subject,
                notify,
            } => {
                self.reaction_message_ids.insert(reaction_id);
                if let Some(target_id) = target_id {
                    self.cmd.emit(UiCommand::LoadReactions { target_id });
                    if notify && !notifications::is_attending_main(root) {
                        notifications::send_named_notification(
                            &format!("reaction-{}", reaction_id.0),
                            &trf!("{name} reacted {emoji}", name = from_name, emoji = emoji),
                            &subject,
                            Some(target_id),
                        );
                    }
                }
                self.refresh_react_enabled_for(reaction_id);
            }
            SyncEvent::BodyReady {
                id,
                html,
                text,
                cids,
                attachments,
                allow_remote_images,
                list_unsubscribe,
                message_id_hdr,
                in_reply_to: _,
                references_hdr,
                pgp,
                attachments_pending,
                attachments_failed,
            } => {
                self.pgp_by_message.insert(id, pgp);
                if message_id_hdr.is_some() || references_hdr.is_some() {
                    self.threading_hdrs
                        .insert(id, (message_id_hdr, references_hdr));
                }
                {
                    let mut cache = self.cid_cache.borrow_mut();
                    let mut cid_sha = HashMap::new();
                    for inline in cids {
                        if let Some(cid) = inline.cid.as_deref() {
                            let key = cid_map_key(cid);
                            if !key.is_empty() {
                                cid_sha.insert(key, inline.sha256.clone());
                            }
                        }
                        cache.insert(inline.sha256, (inline.bytes, inline.mime));
                    }
                    if cid_sha.is_empty() {
                        self.inline_cid_sha.remove(&id);
                    } else {
                        self.inline_cid_sha.insert(id, cid_sha);
                    }
                }
                if allow_remote_images {
                    self.remote_allowed_messages.borrow_mut().insert(id);
                }
                self.bodies.insert(id, text.clone());
                if html.trim().is_empty() {
                    self.html_bodies.remove(&id);
                } else {
                    self.html_bodies.insert(id, html.clone());
                }
                let show_paperclip =
                    !attachments.is_empty() || attachments_pending || attachments_failed;
                self.attachments_by_id.insert(id, attachments);
                if attachments_pending {
                    self.attachments_pending_ids.insert(id);
                } else {
                    self.attachments_pending_ids.remove(&id);
                }
                if attachments_failed {
                    self.attachments_failed_ids.insert(id);
                } else {
                    self.attachments_failed_ids.remove(&id);
                }
                if show_paperclip {
                    self.set_listed_has_attachments(id, true);
                }
                if let Some(list) = list_unsubscribe {
                    if list.has_action() {
                        self.unsubscribe_by_id.insert(id, list);
                    } else {
                        self.unsubscribe_by_id.remove(&id);
                    }
                } else if !self.mock {
                    self.unsubscribe_by_id.remove(&id);
                }
                self.update_readers_for_message(id);
                self.note_reaction_original_body(id, &text, &html);
            }
            SyncEvent::UnsubscribeFinished { id, ok, message } => {
                self.unsubscribing_ids.remove(&id);
                if ok {
                    self.unsubscribed_ids.insert(id);
                }
                self.toasts.add_toast(crate::toast::plain(&message));
                self.record(
                    activity_log::KIND_MAIL,
                    if ok {
                        ActivityOutcome::Success
                    } else {
                        ActivityOutcome::Failure
                    },
                    message.clone(),
                    "",
                );
                self.refresh_unsubscribe_button_for_id(id);
                self.update_readers_for_message(id);
            }
            SyncEvent::RawMessageReady { id, raw } => {
                self.raw_messages.insert(id, raw.clone());
                if self
                    .pending_raw_action
                    .is_some_and(|(pending_id, _)| pending_id == id)
                {
                    let (_, action) = self.pending_raw_action.take().unwrap();
                    self.complete_raw_action(id, action, raw, root);
                }
            }
            SyncEvent::ThreadReady { .. } => {}
            SyncEvent::RemoteImageSendersChanged { emails } => {
                {
                    let mut senders = self.remote_image_senders.borrow_mut();
                    senders.clear();
                    for email in emails {
                        if let Some(normalized) = normalize_sender_email(&email) {
                            senders.insert(normalized);
                        }
                    }
                }
                self.refill_open_readers();
            }
            SyncEvent::PgpAutocryptOffer {
                message_id,
                email,
                fingerprint: _,
                armored,
            } => {
                let account_id = self
                    .envelope_for(message_id)
                    .map(|envelope| envelope.account_id);
                if let Some(account_id) = account_id {
                    self.autocrypt_by_message
                        .insert(message_id, (account_id, email, armored));
                    if self.selected == Some(message_id)
                        && let Some(envelope) = self.envelope_for(message_id)
                    {
                        self.fill_reader(&self.reader, message_id, envelope);
                    }
                }
            }
            _ => unreachable!("sync event not routed to reader"),
        }
    }
}
