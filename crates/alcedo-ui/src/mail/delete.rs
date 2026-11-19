//! Delete, permanent delete, expunge and the pending/undo delete toasts.

use super::list_actions::adjacent_after_removed;
use super::message_lookup::MessageLookup;
use super::send::composer_send_in_progress;
use super::*;
use alcedo_i18n::{ntr, tr};

impl MailModel {
    pub(super) fn update_delete(&mut self, msg: MailMsg, sender: ComponentSender<MailModel>) {
        match msg {
            MailMsg::CopiedVerificationCode(id) => {
                if !cleanup_verification_codes() {
                    return;
                }
                self.cleanup_verification_message(id, &sender);
            }
            MailMsg::DeleteSelected => {
                self.delete_selected_messages(&sender);
            }
            MailMsg::DeletePermanentlySelected => {
                self.prompt_permanent_delete(&sender);
            }
            MailMsg::ConfirmPermanentDelete(ids) => {
                self.expunge_message_ids(&ids, &sender, DeleteToast::Permanent);
            }
            MailMsg::UndoDelete => {
                let items = std::mem::take(&mut self.undo_delete_items);
                for (id, _) in &items {
                    self.pending_deleted_messages.remove(id);
                }
                self.pending_delete_confirm = None;
                let restored = std::mem::take(&mut self.undo_delete_envelopes);
                self.dismiss_undo_delete_toast();
                for (id, dest) in items {
                    if self.mock {
                        self.mock_undelete_message(id, dest);
                    } else {
                        self.cmd.emit(UiCommand::UndeleteMessage { id, dest });
                    }
                }
                if self.mock {
                    self.apply_mock_list();
                } else {
                    self.restore_undeleted_envelopes(restored);
                }
            }
            _ => unreachable!("message not routed to delete"),
        }
    }

    pub(super) fn sync_deleted_column(&mut self) {
        let trash_visible = matches!(
            self.mail_view,
            MailView::Folder(folder_id) if self.folder_is_trash(folder_id)
        );
        set_deleted_column_visible(&self.list_view, trash_visible);
        if trash_visible {
            apply_trash_deleted_sort(&self.list_view);
            self.deleted_column_on = true;
        } else if self.deleted_column_on {
            set_deleted_column_visible(&self.list_view, false);
            if !self.newest_first_sort_on {
                apply_saved_message_list_sort(&self.list_view);
            }
            self.deleted_column_on = false;
        }
    }

    pub(super) fn classify_list_delete(
        &self,
        ids: &[MessageId],
    ) -> (usize, usize, Vec<(MessageId, FolderId)>) {
        let mut moved = 0usize;
        let mut permanent = 0usize;
        let mut undoable = Vec::new();
        let lookup = self.message_lookup();
        for id in ids {
            let Some((account_id, folder_id)) = lookup.location(*id) else {
                continue;
            };
            match delete_disposition(
                self.folder_is_trash(folder_id),
                self.trash_folder_for(account_id).is_some(),
            ) {
                DeleteDisposition::MoveToTrash => {
                    moved += 1;
                    undoable.push((*id, folder_id));
                }
                DeleteDisposition::Permanent => permanent += 1,
            }
        }
        (moved, permanent, undoable)
    }

    pub(super) fn dismiss_undo_delete_toast(&mut self) {
        if let Some(toast) = self.undo_delete_toast.take() {
            toast.dismiss();
        }
        self.undo_delete_items.clear();
        self.undo_delete_envelopes.clear();
    }

    pub(super) fn show_pending_delete_toast(
        &mut self,
        sender: &ComponentSender<MailModel>,
        moved: usize,
        permanent: usize,
        undoable: Vec<(MessageId, FolderId)>,
        undo_envelopes: Vec<Envelope>,
        toast_kind: DeleteToast,
    ) {
        self.dismiss_undo_delete_toast();
        if moved == 0 && permanent == 0 {
            return;
        }
        let title = delete_pending_title(toast_kind, moved, permanent);
        let toast = crate::toast::plain(&title);
        toast.set_timeout(0);
        if !undoable.is_empty() {
            toast.set_button_label(Some(&tr!("Undo")));
            let sender = sender.clone();
            toast.connect_button_clicked(move |_| {
                sender.input(MailMsg::UndoDelete);
            });
            self.undo_delete_items = undoable;
            self.undo_delete_envelopes = undo_envelopes;
        }
        self.undo_delete_toast = Some(toast.clone());
        self.toasts.add_toast(toast);
    }

    pub(super) fn show_undo_delete_toast(
        &mut self,
        sender: &ComponentSender<MailModel>,
        moved: usize,
        permanent: usize,
        undoable: Vec<(MessageId, FolderId)>,
        undo_envelopes: Vec<Envelope>,
        toast_kind: DeleteToast,
    ) {
        self.dismiss_undo_delete_toast();
        if moved == 0 && permanent == 0 {
            return;
        }
        let title = delete_success_title(toast_kind, moved, permanent);
        let toast = crate::toast::plain(&title);
        toast.set_timeout(8);
        if !undoable.is_empty() {
            toast.set_button_label(Some(&tr!("Undo")));
            let sender = sender.clone();
            toast.connect_button_clicked(move |_| {
                sender.input(MailMsg::UndoDelete);
            });
            self.undo_delete_items = undoable;
            self.undo_delete_envelopes = undo_envelopes;
        }
        self.undo_delete_toast = Some(toast.clone());
        self.toasts.add_toast(toast);
        self.record(
            activity_log::KIND_MAIL,
            ActivityOutcome::Success,
            title.clone(),
            "",
        );
    }

    pub(super) fn confirm_deleted_ids(
        &mut self,
        released: &[MessageId],
        sender: &ComponentSender<MailModel>,
    ) {
        let Some(confirm) = self.pending_delete_confirm.as_mut() else {
            return;
        };
        let confirm_ids: HashSet<MessageId> = confirm.ids.iter().copied().collect();
        let hit_set: HashSet<MessageId> = released
            .iter()
            .copied()
            .filter(|id| confirm_ids.contains(id))
            .collect();
        if hit_set.is_empty() {
            return;
        }

        let moved_count = confirm
            .moved_ids
            .iter()
            .filter(|id| hit_set.contains(id))
            .count();
        let permanent_count = confirm
            .permanent_ids
            .iter()
            .filter(|id| hit_set.contains(id))
            .count();
        let toast_kind = confirm.toast_kind;
        let released_undoable: Vec<(MessageId, FolderId)> = confirm
            .undoable
            .iter()
            .filter(|(id, _)| hit_set.contains(id))
            .copied()
            .collect();
        let released_envelopes: Vec<Envelope> = confirm
            .undo_envelopes
            .iter()
            .filter(|env| hit_set.contains(&env.id))
            .cloned()
            .collect();

        confirm.ids.retain(|id| !hit_set.contains(id));
        confirm.moved_ids.retain(|id| !hit_set.contains(id));
        confirm.permanent_ids.retain(|id| !hit_set.contains(id));
        confirm.undoable.retain(|(id, _)| !hit_set.contains(id));
        confirm
            .undo_envelopes
            .retain(|env| !hit_set.contains(&env.id));

        let is_last = confirm.ids.is_empty();
        if is_last {
            self.pending_delete_confirm = None;
        }

        self.show_undo_delete_toast(
            sender,
            moved_count,
            permanent_count,
            released_undoable,
            released_envelopes,
            toast_kind,
        );
    }

    /// Swap the pinned "Moving to Trash…" toast for the timed result when a
    /// full list replacement shows those messages are gone.
    ///
    /// Runs for every replacement, including ones for a folder the user is
    /// not looking at. Search and other views never reload after a delete, so
    /// waiting for a matching list left the pinned toast up.
    pub(super) fn confirm_deleted_from_list_snapshot<I>(
        &mut self,
        kind: EnvelopeListKind,
        rows: I,
        sender: &ComponentSender<MailModel>,
    ) where
        I: IntoIterator<Item = (MessageId, FolderId)>,
    {
        if self.pending_delete_confirm.is_none() {
            return;
        }
        let mut snapshot_ids = HashSet::new();
        let mut snapshot_folders = HashSet::new();
        for (id, folder) in rows {
            snapshot_ids.insert(id);
            snapshot_folders.insert(folder);
        }
        if list_replacement_is_placeholder(
            kind,
            snapshot_ids.is_empty(),
            self.pending_folder_sync,
            self.pending_virtual_sync,
        ) {
            return;
        }
        let pending_inboxes: Vec<FolderId> = self
            .pending_deleted_messages
            .values()
            .copied()
            .filter(|folder| self.folder_is_special(*folder, &SpecialUse::Inbox))
            .collect();
        let scope = scope_for_delete_confirm(kind, snapshot_folders, pending_inboxes);
        let confirm_ids = self
            .pending_delete_confirm
            .as_ref()
            .map(|confirm| confirm.ids.clone())
            .unwrap_or_default();
        release_pending_replaced(&mut self.pending_deleted_messages, &snapshot_ids, &scope);
        let released = pending_ids_released(&confirm_ids, &self.pending_deleted_messages);
        self.confirm_deleted_ids(&released, sender);
    }

    pub(super) fn restore_undeleted_envelopes(&mut self, restored: Vec<Envelope>) {
        if restored.is_empty() {
            return;
        }
        for env in restored {
            let show = match self.mail_view {
                MailView::Folder(folder_id) => env.folder_id == folder_id,
                MailView::UnifiedInbox => {
                    !self.folder_is_special(env.folder_id, &SpecialUse::Trash)
                        && !self.folder_is_special(env.folder_id, &SpecialUse::Junk)
                }
                MailView::Search | MailView::Extension(_) => true,
            };
            if show && !self.envelopes.iter().any(|existing| existing.id == env.id) {
                self.envelopes.push(env);
            }
        }
        if matches!(self.mail_view, MailView::Folder(folder_id) if self.folder_is_trash(folder_id))
        {
            self.envelopes.sort_by(|a, b| {
                b.deleted_at
                    .cmp(&a.deleted_at)
                    .then_with(|| b.date.cmp(&a.date))
            });
        } else {
            self.envelopes.sort_by(|a, b| b.date.cmp(&a.date));
        }
        if self.list_uses_conversations() {
            self.conversations = group_envelopes(&self.envelopes);
            self.envelopes = Self::conversation_lookup_envelopes(&self.conversations);
        }
        self.refresh_list();
    }

    pub(super) fn delete_message_ids(
        &mut self,
        ids: &[MessageId],
        sender: &ComponentSender<MailModel>,
        toast_kind: DeleteToast,
    ) {
        if ids.is_empty() {
            return;
        }
        // Permanent deletes never run silently: they go through the confirm
        // dialog, and only the rest continues down the undoable path.
        let permanent_ids = self.permanent_delete_ids(ids);
        if !permanent_ids.is_empty() {
            self.confirm_permanent_delete(permanent_ids.clone(), sender);
            let permanent_set: HashSet<MessageId> = permanent_ids.into_iter().collect();
            let rest: Vec<MessageId> = ids
                .iter()
                .copied()
                .filter(|id| !permanent_set.contains(id))
                .collect();
            if rest.is_empty() {
                return;
            }
            return self.delete_message_ids(&rest, sender, toast_kind);
        }
        let (moved, permanent, undoable) = self.classify_list_delete(ids);
        let (undo_envelopes, source_folders) = {
            let lookup = self.message_lookup();
            let undo_envelopes: Vec<Envelope> = undoable
                .iter()
                .filter_map(|(id, _)| lookup.envelope(*id).cloned())
                .collect();
            (undo_envelopes, listed_source_folders(&lookup, ids))
        };
        let delete_set: HashSet<MessageId> = ids.iter().copied().collect();
        for id in ids {
            if self.mock {
                self.mock_delete_message(*id);
            }
        }
        if !self.mock {
            self.pending_deleted_messages.extend(source_folders);
        }
        let next_id = adjacent_after_removed(&self.envelopes, &delete_set);
        self.remove_envelopes_from_ui(ids, next_id);
        if !self.mock {
            self.cmd
                .emit(UiCommand::DeleteMessages { ids: ids.to_vec() });
        }
        self.dismiss_undo_delete_toast();
        let moved_ids: Vec<MessageId> = undoable.iter().map(|(id, _)| *id).collect();
        let moved_set: HashSet<MessageId> = moved_ids.iter().copied().collect();
        let permanent_ids: Vec<MessageId> = ids
            .iter()
            .copied()
            .filter(|id| !moved_set.contains(id))
            .collect();
        if self.mock {
            // Nothing will confirm the delete later, so show the timed toast now.
            self.show_undo_delete_toast(
                sender,
                moved,
                permanent,
                undoable,
                undo_envelopes,
                toast_kind,
            );
        } else {
            self.show_pending_delete_toast(
                sender,
                moved,
                permanent,
                undoable.clone(),
                undo_envelopes.clone(),
                toast_kind,
            );
            self.pending_delete_confirm = Some(PendingDeleteConfirm {
                ids: ids.to_vec(),
                moved_ids,
                permanent_ids,
                undoable,
                undo_envelopes,
                toast_kind,
            });
        }
        if notifications::is_attending_main(&self.window) {
            ignore_focus_selection_briefly();
            {
                let _guard = suppress_list_selection();
                self.list_view.view.grab_focus();
                if let Some(id) = self.selected {
                    restore_reader_row(&self.list_view, id);
                }
            }
        }
    }

    pub(super) fn prompt_permanent_delete(&mut self, sender: &ComponentSender<MailModel>) {
        if self.view_handles_delete() {
            return;
        }
        let ids = selected_member_ids(&self.list_view);
        if ids.is_empty() {
            return;
        }
        self.confirm_permanent_delete(ids, sender);
    }

    /// Ids among `ids` whose delete would be permanent (no Trash folder, or
    /// already in Trash).
    fn permanent_delete_ids(&self, ids: &[MessageId]) -> Vec<MessageId> {
        let lookup = self.message_lookup();
        ids.iter()
            .copied()
            .filter(|id| {
                lookup.location(*id).is_some_and(|(account_id, folder_id)| {
                    delete_disposition(
                        self.folder_is_trash(folder_id),
                        self.trash_folder_for(account_id).is_some(),
                    ) == DeleteDisposition::Permanent
                })
            })
            .collect()
    }

    fn confirm_permanent_delete(&self, ids: Vec<MessageId>, sender: &ComponentSender<MailModel>) {
        let (heading, body, confirm_label) = permanent_delete_dialog_copy(ids.len());
        let confirm = adw::AlertDialog::builder()
            .heading(heading)
            .body(body)
            .build();
        confirm.add_response("cancel", &tr!("Cancel"));
        confirm.add_response("delete", &confirm_label);
        confirm.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
        confirm.set_close_response("cancel");
        confirm.set_default_response(Some("cancel"));
        let sender = sender.clone();
        confirm.choose(
            Some(&self.window),
            None::<&gtk::gio::Cancellable>,
            move |response| {
                if response.as_str() == "delete" {
                    sender.input(MailMsg::ConfirmPermanentDelete(ids));
                }
            },
        );
    }

    pub(super) fn expunge_message_ids(
        &mut self,
        ids: &[MessageId],
        sender: &ComponentSender<MailModel>,
        toast_kind: DeleteToast,
    ) {
        if ids.is_empty() {
            return;
        }
        let moved = 0usize;
        let permanent = ids.len();
        let undoable: Vec<(MessageId, FolderId)> = Vec::new();
        let undo_envelopes: Vec<Envelope> = Vec::new();
        let delete_set: HashSet<MessageId> = ids.iter().copied().collect();
        let source_folders = listed_source_folders(&self.message_lookup(), ids);
        for id in ids {
            if self.mock {
                self.mock_delete_message(*id);
            }
        }
        if !self.mock {
            self.pending_deleted_messages.extend(source_folders);
        }
        let next_id = adjacent_after_removed(&self.envelopes, &delete_set);
        self.remove_envelopes_from_ui(ids, next_id);
        if !self.mock {
            self.cmd
                .emit(UiCommand::ExpungeMessages { ids: ids.to_vec() });
        }
        self.dismiss_undo_delete_toast();
        let moved_ids: Vec<MessageId> = Vec::new();
        let permanent_ids: Vec<MessageId> = ids.to_vec();
        if self.mock {
            self.show_undo_delete_toast(
                sender,
                moved,
                permanent,
                undoable,
                undo_envelopes,
                toast_kind,
            );
        } else {
            self.show_pending_delete_toast(
                sender,
                moved,
                permanent,
                undoable.clone(),
                undo_envelopes.clone(),
                toast_kind,
            );
            self.pending_delete_confirm = Some(PendingDeleteConfirm {
                ids: ids.to_vec(),
                moved_ids,
                permanent_ids,
                undoable,
                undo_envelopes,
                toast_kind,
            });
        }
        if notifications::is_attending_main(&self.window) {
            ignore_focus_selection_briefly();
            {
                let _guard = suppress_list_selection();
                self.list_view.view.grab_focus();
                if let Some(id) = self.selected {
                    restore_reader_row(&self.list_view, id);
                }
            }
        }
    }

    pub(super) fn cleanup_verification_message(
        &mut self,
        id: MessageId,
        sender: &ComponentSender<MailModel>,
    ) {
        if self
            .undo_delete_items
            .iter()
            .any(|(existing, _)| *existing == id)
        {
            return;
        }
        let Some(env) = self
            .envelope_for(id)
            .cloned()
            .or_else(|| self.notified_otp.get(&id).map(|(_, env)| env.clone()))
        else {
            self.toasts
                .add_toast(crate::toast::plain(&tr!("Code copied")));
            return;
        };
        if self.folder_is_trash(env.folder_id) || self.trash_folder_for(env.account_id).is_none() {
            self.toasts
                .add_toast(crate::toast::plain(&tr!("Code copied")));
            return;
        }
        self.delete_message_ids(&[id], sender, DeleteToast::AfterVerificationCopy);
    }

    pub(super) fn delete_selected_messages(&mut self, sender: &ComponentSender<MailModel>) {
        if self.mail_content_pane == MailContentPane::Composer
            && !composer_send_in_progress(true, self.pending_send_draft.is_some())
        {
            sender.input(MailMsg::CancelCompose);
            return;
        }
        let ids = selected_member_ids(&self.list_view);
        if ids.is_empty() {
            return;
        }
        if self.view_handles_delete() {
            self.delete_in_extension_view(&ids);
            return;
        }
        self.delete_message_ids(&ids, sender, DeleteToast::Default);
    }

    pub(super) fn mock_delete_message(&mut self, id: MessageId) {
        let Some(env) = self.source_envelopes.iter().find(|e| e.id == id).cloned() else {
            return;
        };
        if self.folder_is_trash(env.folder_id) {
            self.source_envelopes.retain(|e| e.id != id);
            self.bodies.remove(&id);
            self.html_bodies.remove(&id);
            self.attachments_by_id.remove(&id);
            return;
        }
        if let Some(trash_id) = self.trash_folder_for(env.account_id) {
            for envelope in self.source_envelopes.iter_mut().filter(|e| e.id == id) {
                envelope.folder_id = trash_id;
                envelope.deleted_at = Some(Utc::now());
            }
        } else {
            self.source_envelopes.retain(|e| e.id != id);
            self.bodies.remove(&id);
            self.html_bodies.remove(&id);
        }
    }

    pub(super) fn mock_undelete_message(&mut self, id: MessageId, dest: FolderId) {
        for envelope in self.source_envelopes.iter_mut().filter(|e| e.id == id) {
            envelope.folder_id = dest;
            envelope.deleted_at = None;
        }
    }
}

/// Folder of each message that has a list row, captured before the rows
/// leave the list.
fn listed_source_folders(
    lookup: &MessageLookup<'_>,
    ids: &[MessageId],
) -> Vec<(MessageId, FolderId)> {
    ids.iter()
        .filter_map(|id| lookup.envelope(*id).map(|env| (*id, env.folder_id)))
        .collect()
}

pub(super) fn strip_pending_envelopes(
    envelopes: &mut Vec<Envelope>,
    pending: &HashMap<MessageId, FolderId>,
) {
    if pending.is_empty() {
        return;
    }
    envelopes.retain(|env| !pending.contains_key(&env.id));
}

pub(super) fn pending_ids_released(
    before: &[MessageId],
    pending: &HashMap<MessageId, FolderId>,
) -> Vec<MessageId> {
    before
        .iter()
        .copied()
        .filter(|id| !pending.contains_key(id))
        .collect()
}

pub(super) fn confirm_ids_surviving_failure(
    confirm_ids: &[MessageId],
    hit: &[MessageId],
) -> Vec<MessageId> {
    let hit: HashSet<MessageId> = hit.iter().copied().collect();
    confirm_ids
        .iter()
        .copied()
        .filter(|id| !hit.contains(id))
        .collect()
}

/// What deleting a message does, decided by where it lives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DeleteDisposition {
    MoveToTrash,
    Permanent,
}

pub(super) fn delete_disposition(in_trash: bool, account_has_trash: bool) -> DeleteDisposition {
    if in_trash || !account_has_trash {
        DeleteDisposition::Permanent
    } else {
        DeleteDisposition::MoveToTrash
    }
}

pub(super) fn permanent_delete_dialog_copy(count: usize) -> (String, String, String) {
    let (heading, body) = if count == 1 {
        (
            tr!("Delete this message permanently?"),
            tr!("This cannot be undone. The message is erased from the mail server."),
        )
    } else {
        (
            ntr!(
                "Delete {n} message permanently?",
                "Delete {n} messages permanently?",
                count
            ),
            tr!("This cannot be undone. These messages are erased from the mail server."),
        )
    };
    (heading, body, tr!("Delete Permanently"))
}

pub(super) fn delete_pending_title(
    toast_kind: DeleteToast,
    moved: usize,
    permanent: usize,
) -> String {
    if toast_kind == DeleteToast::Permanent {
        return if permanent == 1 {
            tr!("Deleting permanently…")
        } else {
            ntr!(
                "Deleting {n} message permanently…",
                "Deleting {n} messages permanently…",
                permanent
            )
        };
    }
    if toast_kind == DeleteToast::AfterVerificationCopy && moved >= 1 && permanent == 0 {
        return tr!("Moving to Trash…");
    }
    if moved > 0 && permanent == 0 {
        return if moved == 1 {
            tr!("Moving to Trash…")
        } else {
            ntr!(
                "Moving {n} message to Trash…",
                "Moving {n} messages to Trash…",
                moved
            )
        };
    }
    if permanent > 0 && moved == 0 {
        return if permanent == 1 {
            tr!("Deleting permanently…")
        } else {
            ntr!(
                "Deleting {n} message permanently…",
                "Deleting {n} messages permanently…",
                permanent
            )
        };
    }
    ntr!(
        "Deleting {n} message…",
        "Deleting {n} messages…",
        moved + permanent
    )
}

pub(super) fn delete_success_title(
    toast_kind: DeleteToast,
    moved: usize,
    permanent: usize,
) -> String {
    if toast_kind == DeleteToast::AfterVerificationCopy && moved >= 1 && permanent == 0 {
        tr!("Code copied. Moved to Trash.")
    } else if moved > 0 && permanent == 0 {
        if moved == 1 {
            tr!("Moved to Trash.")
        } else {
            ntr!(
                "Moved {n} message to Trash.",
                "Moved {n} messages to Trash.",
                moved
            )
        }
    } else if permanent > 0 && moved == 0 {
        if permanent == 1 {
            tr!("Permanently deleted.")
        } else {
            ntr!(
                "Permanently deleted {n} message.",
                "Permanently deleted {n} messages.",
                permanent
            )
        }
    } else {
        ntr!(
            "Deleted {n} message.",
            "Deleted {n} messages.",
            moved + permanent
        )
    }
}

pub(super) fn delete_failure_title(toast_kind: DeleteToast, moved: usize, reason: &str) -> String {
    let network = looks_like_network_failure(reason);
    if toast_kind == DeleteToast::Permanent {
        return if network {
            tr!("Couldn't permanently delete — connection lost")
        } else {
            tr!("Couldn't permanently delete.")
        };
    }
    if toast_kind == DeleteToast::AfterVerificationCopy {
        return if network {
            tr!("Code copied. Couldn't move to Trash — connection lost")
        } else {
            tr!("Code copied. Couldn't move to Trash.")
        };
    }
    if moved > 0 {
        return if network {
            tr!("Couldn't move to Trash — connection lost")
        } else {
            tr!("Couldn't move to Trash.")
        };
    }
    if network {
        tr!("Couldn't delete message — connection lost")
    } else {
        tr!("Couldn't delete message.")
    }
}

/// Drop holds whose folder is in `scope_folders` and whose id is absent.
/// No-op when `snapshot_ids` is empty, so a blank appended page is not
/// treated as "the folder is empty".
pub(super) fn release_pending_absent(
    pending: &mut HashMap<MessageId, FolderId>,
    snapshot_ids: &HashSet<MessageId>,
    scope_folders: &HashSet<FolderId>,
) {
    if snapshot_ids.is_empty() || scope_folders.is_empty() {
        return;
    }
    pending.retain(|id, folder| !scope_folders.contains(folder) || snapshot_ids.contains(id));
}

/// Folders a full list replacement may release pending deletes from.
///
/// A folder list always includes that folder, even when the list is now
/// empty. A virtual inbox also includes every inbox that still has a pending
/// delete, because that inbox may have no rows left in the snapshot.
pub(super) fn scope_for_delete_confirm(
    kind: EnvelopeListKind,
    snapshot_folders: HashSet<FolderId>,
    pending_inbox_folders: impl IntoIterator<Item = FolderId>,
) -> HashSet<FolderId> {
    let mut scope = snapshot_folders;
    if let EnvelopeListKind::Folder(folder_id) = kind {
        scope.insert(folder_id);
    }
    if matches!(kind, EnvelopeListKind::UnifiedInbox) {
        scope.extend(pending_inbox_folders);
    }
    scope
}

/// True for the empty list a folder or virtual inbox can emit before its
/// fetch finishes. That result must not complete a delete.
pub(super) fn list_replacement_is_placeholder(
    kind: EnvelopeListKind,
    snapshot_empty: bool,
    pending_folder_sync: Option<FolderId>,
    pending_virtual_sync: Option<MailView>,
) -> bool {
    if !snapshot_empty {
        return false;
    }
    match kind {
        EnvelopeListKind::Folder(folder_id) => pending_folder_sync == Some(folder_id),
        EnvelopeListKind::UnifiedInbox => pending_virtual_sync == Some(MailView::UnifiedInbox),
        _ => false,
    }
}

/// Drop holds in `scope_folders` that are missing from a full replacement.
/// An empty snapshot drops every hold in scope: the folder or view has no
/// rows left.
pub(super) fn release_pending_replaced(
    pending: &mut HashMap<MessageId, FolderId>,
    snapshot_ids: &HashSet<MessageId>,
    scope_folders: &HashSet<FolderId>,
) {
    if scope_folders.is_empty() {
        return;
    }
    pending.retain(|id, folder| !scope_folders.contains(folder) || snapshot_ids.contains(id));
}

pub(super) fn strip_pending_conversations(
    conversations: &mut Vec<Conversation>,
    pending: &HashMap<MessageId, FolderId>,
) {
    if pending.is_empty() {
        return;
    }
    let mut flat = Vec::new();
    for conversation in conversations.iter() {
        flat.extend(conversation.members.iter().cloned());
    }
    strip_pending_envelopes(&mut flat, pending);
    *conversations = group_envelopes(&flat);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mail::test_support::envelope_with_id;

    #[test]
    fn pending_delete_released_ids_are_those_missing_from_the_map() {
        let before = [MessageId(1), MessageId(2)];
        let pending = HashMap::from([(MessageId(1), FolderId(1))]);
        assert_eq!(pending_ids_released(&before, &pending), vec![MessageId(2)]);
    }

    #[test]
    fn pending_delete_failure_keeps_ids_not_in_the_hit() {
        assert_eq!(
            confirm_ids_surviving_failure(&[MessageId(1), MessageId(2)], &[MessageId(1)]),
            vec![MessageId(2)]
        );
    }

    #[test]
    fn delete_is_permanent_when_in_trash_or_account_has_no_trash() {
        assert_eq!(
            delete_disposition(false, true),
            DeleteDisposition::MoveToTrash
        );
        assert_eq!(delete_disposition(true, true), DeleteDisposition::Permanent);
        assert_eq!(
            delete_disposition(false, false),
            DeleteDisposition::Permanent
        );
        assert_eq!(
            delete_disposition(true, false),
            DeleteDisposition::Permanent
        );
    }

    #[test]
    fn permanent_only_default_delete_says_deleting_permanently() {
        assert_eq!(
            delete_pending_title(DeleteToast::Default, 0, 1),
            "Deleting permanently…"
        );
        assert_eq!(
            delete_pending_title(DeleteToast::Default, 0, 3),
            "Deleting 3 messages permanently…"
        );
    }

    #[test]
    fn pending_delete_pending_title_single_move() {
        assert_eq!(
            delete_pending_title(DeleteToast::default(), 1, 0),
            "Moving to Trash…"
        );
    }

    #[test]
    fn permanent_delete_pending_and_failure_titles() {
        assert_eq!(
            delete_pending_title(DeleteToast::Permanent, 0, 1),
            "Deleting permanently…"
        );
        assert_eq!(
            delete_pending_title(DeleteToast::Permanent, 0, 2),
            "Deleting 2 messages permanently…"
        );
        assert_eq!(
            delete_failure_title(DeleteToast::Permanent, 0, "no"),
            "Couldn't permanently delete."
        );
        assert_eq!(
            delete_failure_title(DeleteToast::Permanent, 0, "broken pipe"),
            "Couldn't permanently delete — connection lost"
        );
        let (heading, body, confirm) = permanent_delete_dialog_copy(1);
        assert_eq!(heading, "Delete this message permanently?");
        assert_eq!(
            body,
            "This cannot be undone. The message is erased from the mail server."
        );
        assert_eq!(confirm, "Delete Permanently");
        let (heading, body, confirm) = permanent_delete_dialog_copy(2);
        assert_eq!(heading, "Delete 2 messages permanently?");
        assert_eq!(
            body,
            "This cannot be undone. These messages are erased from the mail server."
        );
        assert_eq!(confirm, "Delete Permanently");
    }

    #[test]
    fn pending_delete_success_title_matches_trash_copy() {
        assert_eq!(
            delete_success_title(DeleteToast::default(), 1, 0),
            "Moved to Trash."
        );
        assert_eq!(
            delete_success_title(DeleteToast::AfterVerificationCopy, 1, 0),
            "Code copied. Moved to Trash."
        );
    }

    #[test]
    fn pending_delete_failure_title_broken_pipe() {
        assert_eq!(
            delete_failure_title(
                DeleteToast::default(),
                1,
                "imap: UID SEARCH UID 483: io: Broken pipe (os error 32)"
            ),
            "Couldn't move to Trash — connection lost"
        );
    }

    #[test]
    fn pending_delete_failure_title_other() {
        assert_eq!(
            delete_failure_title(DeleteToast::default(), 1, "folder not found for message"),
            "Couldn't move to Trash."
        );
    }

    #[test]
    fn pending_delete_failure_title_permanent_network() {
        assert_eq!(
            delete_failure_title(DeleteToast::default(), 0, "broken pipe"),
            "Couldn't delete message — connection lost"
        );
    }

    #[test]
    fn pending_delete_strip_removes_held_id_only() {
        let mut envelopes = vec![
            envelope_with_id(1),
            envelope_with_id(2),
            envelope_with_id(3),
        ];
        let pending = HashMap::from([(MessageId(2), FolderId(1))]);
        strip_pending_envelopes(&mut envelopes, &pending);
        assert_eq!(envelopes.len(), 2);
        assert!(envelopes.iter().all(|env| env.id != MessageId(2)));
    }

    #[test]
    fn pending_delete_release_drops_id_missing_from_same_folder() {
        let mut pending = HashMap::from([(MessageId(2), FolderId(1))]);
        let snapshot_ids = HashSet::from([MessageId(1), MessageId(3)]);
        let scope_folders = HashSet::from([FolderId(1)]);
        release_pending_absent(&mut pending, &snapshot_ids, &scope_folders);
        assert!(!pending.contains_key(&MessageId(2)));
    }

    #[test]
    fn pending_delete_release_keeps_other_folder() {
        let mut pending = HashMap::from([(MessageId(2), FolderId(9))]);
        let snapshot_ids = HashSet::from([MessageId(1), MessageId(3)]);
        let scope_folders = HashSet::from([FolderId(1)]);
        release_pending_absent(&mut pending, &snapshot_ids, &scope_folders);
        assert!(pending.contains_key(&MessageId(2)));
    }

    #[test]
    fn pending_delete_release_ignores_empty_snapshot() {
        let mut pending = HashMap::from([(MessageId(2), FolderId(1))]);
        let snapshot_ids = HashSet::new();
        let scope_folders = HashSet::from([FolderId(1)]);
        release_pending_absent(&mut pending, &snapshot_ids, &scope_folders);
        assert!(pending.contains_key(&MessageId(2)));
    }

    #[test]
    fn empty_folder_replacement_releases_that_folders_pending_delete() {
        let scope =
            scope_for_delete_confirm(EnvelopeListKind::Folder(FolderId(4)), HashSet::new(), []);
        assert_eq!(scope, HashSet::from([FolderId(4)]));
        let mut pending = HashMap::from([(MessageId(2), FolderId(4))]);
        release_pending_replaced(&mut pending, &HashSet::new(), &scope);
        assert!(pending.is_empty());
    }

    #[test]
    fn virtual_inbox_replacement_releases_an_inbox_that_left_the_snapshot() {
        let scope = scope_for_delete_confirm(
            EnvelopeListKind::UnifiedInbox,
            HashSet::from([FolderId(2)]),
            [FolderId(1)],
        );
        assert!(scope.contains(&FolderId(1)));
        let mut pending = HashMap::from([(MessageId(7), FolderId(1))]);
        let snapshot_ids = HashSet::from([MessageId(8)]);
        release_pending_replaced(&mut pending, &snapshot_ids, &scope);
        assert!(pending.is_empty());
    }

    #[test]
    fn folder_replacement_does_not_release_a_different_folder() {
        let scope = scope_for_delete_confirm(
            EnvelopeListKind::Folder(FolderId(9)),
            HashSet::from([FolderId(9)]),
            [FolderId(1)],
        );
        assert_eq!(scope, HashSet::from([FolderId(9)]));
        let mut pending = HashMap::from([(MessageId(7), FolderId(1))]);
        release_pending_replaced(&mut pending, &HashSet::new(), &scope);
        assert!(pending.contains_key(&MessageId(7)));
    }

    #[test]
    fn placeholder_empty_list_does_not_count_as_a_finished_delete() {
        assert!(list_replacement_is_placeholder(
            EnvelopeListKind::Folder(FolderId(4)),
            true,
            Some(FolderId(4)),
            None,
        ));
        assert!(!list_replacement_is_placeholder(
            EnvelopeListKind::Folder(FolderId(4)),
            true,
            None,
            None,
        ));
        assert!(!list_replacement_is_placeholder(
            EnvelopeListKind::Folder(FolderId(4)),
            false,
            Some(FolderId(4)),
            None,
        ));
        assert!(list_replacement_is_placeholder(
            EnvelopeListKind::UnifiedInbox,
            true,
            None,
            Some(MailView::UnifiedInbox),
        ));
    }

    #[test]
    fn pending_delete_conversation_drops_only_held_member() {
        let mut conversations = group_envelopes(&[envelope_with_id(1), envelope_with_id(2)]);
        assert_eq!(conversations.len(), 1);
        assert_eq!(conversations[0].members.len(), 2);

        let pending = HashMap::from([(MessageId(2), FolderId(1))]);
        strip_pending_conversations(&mut conversations, &pending);
        assert_eq!(conversations.len(), 1);
        assert_eq!(conversations[0].members.len(), 1);
        assert_eq!(conversations[0].members[0].id, MessageId(1));
        assert_eq!(conversations[0].member_ids, vec![MessageId(1)]);

        let mut all_held = group_envelopes(&[envelope_with_id(1)]);
        strip_pending_conversations(&mut all_held, &HashMap::from([(MessageId(1), FolderId(1))]));
        assert!(all_held.is_empty());
    }
}
