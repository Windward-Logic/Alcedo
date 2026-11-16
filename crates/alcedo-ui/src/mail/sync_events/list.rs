//! List, folder-count and schedule updates from sync.

use super::*;
use crate::mail::delete::{
    confirm_ids_surviving_failure, delete_failure_title, pending_ids_released,
    release_pending_absent, strip_pending_conversations, strip_pending_envelopes,
};
use crate::mail::list::{kind_is_extension_list, list_event_matches_view};
use crate::mail::list_actions::adjacent_after_removed;

impl MailModel {
    pub(super) fn on_list_sync_event(
        &mut self,
        event: SyncEvent,
        sender: &ComponentSender<MailModel>,
        root: &adw::ApplicationWindow,
    ) {
        match event {
            SyncEvent::EnvelopesReplaced { envelopes, kind } => {
                tracing::debug!(
                    ?kind,
                    count = envelopes.len(),
                    mail_view = ?self.mail_view,
                    "sync event: EnvelopesReplaced"
                );
                if matches!(kind, EnvelopeListKind::UnifiedInbox) {
                    let inbox_folders: HashSet<FolderId> = envelopes
                        .iter()
                        .map(|env| env.folder_id)
                        .filter(|folder_id| self.folder_is_special(*folder_id, &SpecialUse::Inbox))
                        .collect();
                    for folder_id in inbox_folders {
                        let folder_envelopes: Vec<Envelope> = envelopes
                            .iter()
                            .filter(|env| env.folder_id == folder_id)
                            .cloned()
                            .collect();
                        self.observe_inbox_envelopes(folder_id, &folder_envelopes, root, false);
                    }
                } else if let EnvelopeListKind::Folder(folder_id) = kind
                    && self.folder_is_special(folder_id, &SpecialUse::Inbox)
                {
                    self.observe_inbox_envelopes(folder_id, &envelopes, root, true);
                }

                self.abandon_pending_restore_if_account_gone();

                self.confirm_deleted_from_list_snapshot(
                    kind,
                    envelopes.iter().map(|env| (env.id, env.folder_id)),
                    sender,
                );

                let matches_view = list_event_matches_view(
                    self.mail_view,
                    kind,
                    self.pending_restore.is_some(),
                    self.is_search_backed(self.mail_view),
                );
                if !matches_view {
                    return;
                }
                if self.list_mode != MessageListMode::Flat && !kind_is_extension_list(kind) {
                    return;
                }
                if let (MailView::Folder(folder_id), EnvelopeListKind::Folder(_)) =
                    (self.mail_view, kind)
                {
                    // Store may emit an empty result before the worker fetch completes.
                    if !envelopes.is_empty() || self.pending_folder_sync != Some(folder_id) {
                        self.clear_pending_folder_sync(folder_id, sender);
                    }
                }
                self.clear_pending_virtual_sync(sender);
                self.conversations.clear();
                let mut envelopes = if let EnvelopeListKind::Folder(folder_id) = kind {
                    self.merge_compose_draft_pin(folder_id, envelopes)
                } else {
                    envelopes
                };
                strip_pending_envelopes(&mut envelopes, &self.pending_deleted_messages);
                self.envelopes = envelopes;
                self.refresh_list();
                self.finish_pending_notification(sender);
            }
            SyncEvent::ConversationsReplaced {
                conversations,
                kind,
            } => {
                let envelopes = Self::conversation_lookup_envelopes(&conversations);
                tracing::debug!(
                    ?kind,
                    count = conversations.len(),
                    mail_view = ?self.mail_view,
                    "sync event: ConversationsReplaced"
                );
                if matches!(kind, EnvelopeListKind::UnifiedInbox) {
                    let inbox_folders: HashSet<FolderId> = envelopes
                        .iter()
                        .map(|env| env.folder_id)
                        .filter(|folder_id| self.folder_is_special(*folder_id, &SpecialUse::Inbox))
                        .collect();
                    for folder_id in inbox_folders {
                        let folder_envelopes: Vec<Envelope> = envelopes
                            .iter()
                            .filter(|env| env.folder_id == folder_id)
                            .cloned()
                            .collect();
                        self.observe_inbox_envelopes(folder_id, &folder_envelopes, root, false);
                    }
                } else if let EnvelopeListKind::Folder(folder_id) = kind
                    && self.folder_is_special(folder_id, &SpecialUse::Inbox)
                {
                    self.observe_inbox_envelopes(folder_id, &envelopes, root, true);
                }

                self.abandon_pending_restore_if_account_gone();

                self.confirm_deleted_from_list_snapshot(
                    kind,
                    envelopes.iter().map(|env| (env.id, env.folder_id)),
                    sender,
                );

                let matches_view = list_event_matches_view(
                    self.mail_view,
                    kind,
                    self.pending_restore.is_some(),
                    self.is_search_backed(self.mail_view),
                );
                if !matches_view {
                    return;
                }
                if self.list_mode != MessageListMode::Conversation {
                    return;
                }
                if let (MailView::Folder(folder_id), EnvelopeListKind::Folder(_)) =
                    (self.mail_view, kind)
                    && (!envelopes.is_empty() || self.pending_folder_sync != Some(folder_id))
                {
                    self.clear_pending_folder_sync(folder_id, sender);
                }
                self.clear_pending_virtual_sync(sender);
                let mut conversations = conversations;
                strip_pending_conversations(&mut conversations, &self.pending_deleted_messages);
                self.envelopes = Self::conversation_lookup_envelopes(&conversations);
                self.conversations = conversations;
                self.refresh_list();
                self.finish_pending_notification(sender);
            }
            SyncEvent::EnvelopesAppended { folder, envelopes } => {
                tracing::debug!(
                    folder = folder.0,
                    count = envelopes.len(),
                    mail_view = ?self.mail_view,
                    "sync event: EnvelopesAppended"
                );
                self.observe_inbox_envelopes(folder, &envelopes, root, true);
                match self.mail_view {
                    MailView::Extension(_) if self.is_inbox_like(self.mail_view) => {
                        if self.folder_is_special(folder, &SpecialUse::Inbox) {
                            self.reissue_list_select();
                            self.finish_pending_notification(sender);
                        }
                    }
                    MailView::Search | MailView::Extension(_) => {}
                    MailView::UnifiedInbox => {
                        if self.folder_is_special(folder, &SpecialUse::Inbox) {
                            if self.list_mode == MessageListMode::Conversation {
                                self.reissue_list_select();
                            } else {
                                let mut envelopes = envelopes;
                                let snapshot_ids: HashSet<MessageId> =
                                    envelopes.iter().map(|env| env.id).collect();
                                let scope_folders = HashSet::from([folder]);
                                let confirm_ids = self
                                    .pending_delete_confirm
                                    .as_ref()
                                    .map(|confirm| confirm.ids.clone())
                                    .unwrap_or_default();
                                release_pending_absent(
                                    &mut self.pending_deleted_messages,
                                    &snapshot_ids,
                                    &scope_folders,
                                );
                                let released = pending_ids_released(
                                    &confirm_ids,
                                    &self.pending_deleted_messages,
                                );
                                self.confirm_deleted_ids(&released, sender);
                                strip_pending_envelopes(
                                    &mut envelopes,
                                    &self.pending_deleted_messages,
                                );
                                self.merge_appended_envelopes(folder, envelopes);
                                self.refresh_list();
                            }
                            self.finish_pending_notification(sender);
                        }
                    }
                    MailView::Folder(id) if id == folder => {
                        self.clear_pending_folder_sync(folder, sender);
                        if self.list_mode != MessageListMode::Conversation {
                            let mut envelopes = self.merge_compose_draft_pin(folder, envelopes);
                            let snapshot_ids: HashSet<MessageId> =
                                envelopes.iter().map(|env| env.id).collect();
                            let scope_folders = HashSet::from([folder]);
                            let confirm_ids = self
                                .pending_delete_confirm
                                .as_ref()
                                .map(|confirm| confirm.ids.clone())
                                .unwrap_or_default();
                            release_pending_absent(
                                &mut self.pending_deleted_messages,
                                &snapshot_ids,
                                &scope_folders,
                            );
                            let released =
                                pending_ids_released(&confirm_ids, &self.pending_deleted_messages);
                            self.confirm_deleted_ids(&released, sender);
                            strip_pending_envelopes(&mut envelopes, &self.pending_deleted_messages);
                            self.envelopes = envelopes;
                            if self.folder_is_trash(folder) {
                                self.envelopes.sort_by(|a, b| {
                                    b.deleted_at
                                        .cmp(&a.deleted_at)
                                        .then_with(|| b.date.cmp(&a.date))
                                });
                            } else {
                                self.envelopes.sort_by(|a, b| b.date.cmp(&a.date));
                            }
                            self.refresh_list();
                        }
                        self.finish_pending_notification(sender);
                    }
                    MailView::Folder(_) => {}
                }
            }
            SyncEvent::EnvelopeRemoved { id } => {
                let had_pending = self.pending_deleted_messages.contains_key(&id);
                self.pending_deleted_messages.remove(&id);
                if had_pending {
                    self.confirm_deleted_ids(&[id], sender);
                }
                tracing::debug!(id = id.0, mail_view = ?self.mail_view, "sync event: EnvelopeRemoved");
                if self.pending_imap_draft_open == Some(id) {
                    self.pending_imap_draft_open = None;
                }
                if self.pending_popout_draft == Some(id) {
                    self.pending_popout_draft = None;
                }
                self.known_inbox_ids.remove(&id);
                self.last_notified.remove(&id);
                self.notified_otp.remove(&id);
                notifications::clear_pending_notification_otp_if(id.0);
                notifications::withdraw_message_notification(id);
                if self
                    .pending_notification_message
                    .is_some_and(|(pending_id, _)| pending_id == id)
                {
                    self.pending_notification_message = None;
                    self.pending_notification_tried_folder = false;
                }
                let next_id = if self.selected == Some(id) {
                    adjacent_after_removed(&self.envelopes, &HashSet::from([id]))
                } else {
                    None
                };
                self.remove_envelopes_from_ui(&[id], next_id);
            }
            SyncEvent::FolderDeleteFinished {
                folder,
                removed,
                moved_to_trash,
            } => {
                if self.pending_delete_folder == Some(folder) {
                    self.dismiss_pending_delete_toast();
                    let through = self
                        .pending_delete_through
                        .unwrap_or_else(|| Local::now().date_naive());
                    self.pending_delete_folder = None;
                    self.pending_delete_through = None;
                    if self.context_sync_target == Some(MailView::Folder(folder)) {
                        self.context_sync_target = None;
                    }
                    self.show_delete_folder_result_toast(through, removed, moved_to_trash);
                    self.rebuild_folder_bar(sender);
                    self.refresh_status_bar();
                }
            }
            SyncEvent::EnvelopeUpdated(env) => {
                self.patch_listed_envelope(env);
            }
            SyncEvent::UnreadCounts {
                account,
                inbox,
                total,
            } => {
                // UnreadCounts is one event per account. The first event for an
                // account raises unified unread without new mail arriving.
                let first_seen_account = !self.unread_by_account.contains_key(&account);
                self.unread_by_account.insert(account, (inbox, total));
                self.notify_mail_changed();
                self.rebuild_folder_bar(sender);
                self.emit_unified_unread(sender);
                self.refresh_unread_badges(sender);
                let current = self.unified_unread();
                let previous = if first_seen_account {
                    None
                } else {
                    self.last_unread_indicator
                };
                let _ = notifications::apply_unread(root, previous, current);
                self.last_unread_indicator = Some(current);
                self.refresh_status_bar();
            }
            SyncEvent::MessageDeleteFailed { ids, reason } => {
                for id in &ids {
                    self.pending_deleted_messages.remove(id);
                }
                self.reissue_list_select();

                let hit: Vec<MessageId> = self
                    .pending_delete_confirm
                    .as_ref()
                    .map(|confirm| {
                        let confirm_ids: HashSet<MessageId> = confirm.ids.iter().copied().collect();
                        ids.iter()
                            .copied()
                            .filter(|id| confirm_ids.contains(id))
                            .collect()
                    })
                    .unwrap_or_default();
                if !hit.is_empty() {
                    let hit_set: HashSet<MessageId> = hit.iter().copied().collect();
                    let confirm = self.pending_delete_confirm.as_ref().unwrap();
                    let moved = confirm
                        .moved_ids
                        .iter()
                        .filter(|id| hit_set.contains(id))
                        .count();
                    let toast_kind = confirm.toast_kind;
                    let title = delete_failure_title(toast_kind, moved, &reason);
                    let email = hit
                        .first()
                        .and_then(|id| self.envelope_for(*id))
                        .and_then(|env| {
                            self.accounts
                                .iter()
                                .find(|account| account.id == env.account_id)
                                .map(|account| account.email.clone())
                        })
                        .unwrap_or_default();
                    let surviving = confirm_ids_surviving_failure(&confirm.ids, &hit);
                    let confirm = self.pending_delete_confirm.as_mut().unwrap();
                    confirm.ids.retain(|id| !hit_set.contains(id));
                    confirm.moved_ids.retain(|id| !hit_set.contains(id));
                    confirm.permanent_ids.retain(|id| !hit_set.contains(id));
                    confirm
                        .undoable
                        .retain(|(existing, _)| !hit_set.contains(existing));
                    confirm
                        .undo_envelopes
                        .retain(|env| !hit_set.contains(&env.id));
                    self.undo_delete_items
                        .retain(|(existing, _)| !hit_set.contains(existing));
                    self.undo_delete_envelopes
                        .retain(|env| !hit_set.contains(&env.id));
                    self.push_activity(
                        activity_log::KIND_MAIL,
                        ActivityOutcome::Failure,
                        title.clone(),
                        activity_log::redact_url_userinfo(&reason),
                        email,
                        "",
                    );
                    let success_toast_up = self
                        .undo_delete_toast
                        .as_ref()
                        .is_some_and(|toast| toast.timeout() == 8);
                    if surviving.is_empty() {
                        self.pending_delete_confirm = None;
                        if self.undo_delete_items.is_empty() {
                            self.dismiss_undo_delete_toast();
                        }
                    } else if !success_toast_up {
                        let confirm = self.pending_delete_confirm.as_ref().unwrap();
                        self.show_pending_delete_toast(
                            sender,
                            confirm.moved_ids.len(),
                            confirm.permanent_ids.len(),
                            confirm.undoable.clone(),
                            confirm.undo_envelopes.clone(),
                            confirm.toast_kind,
                        );
                    }
                    let toast = crate::toast::plain(&title);
                    toast.set_timeout(8);
                    self.toasts.add_toast(toast);
                }
            }
            _ => unreachable!("sync event not routed to list"),
        }
    }
}
