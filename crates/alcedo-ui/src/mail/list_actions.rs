//! Message-list actions: move and copy, archive, spam, flags and read state.

use super::folders::add_unread;
use super::send::composer_send_in_progress;
use super::window_actions::can_drop_messages_on_folder_data;
use alcedo_core::mail_feedback::{MoveFeedback, move_toast_title};

use super::*;

impl MailModel {
    pub(super) fn update_list_actions(&mut self, msg: MailMsg, sender: ComponentSender<MailModel>) {
        match msg {
            MailMsg::ToggleFlag(id, flagged) => {
                if let Some(env) = self.envelopes.iter_mut().find(|env| env.id == id) {
                    env.flagged = flagged;
                }
                if let Some(env) = self.source_envelopes.iter_mut().find(|env| env.id == id) {
                    env.flagged = flagged;
                }
                if let Some(conversation) = self
                    .conversations
                    .iter_mut()
                    .find(|conversation| conversation.member_ids.contains(&id))
                {
                    if let Some(member) = conversation.members.iter_mut().find(|m| m.id == id) {
                        member.flagged = flagged;
                    }
                    conversation.any_flagged = conversation.members.iter().any(|m| m.flagged);
                }
                if !self.mock {
                    self.cmd.emit(UiCommand::SetFlag {
                        id,
                        seen: None,
                        flagged: Some(flagged),
                    });
                }
                if !self.refresh_listed_flag_row(id) {
                    self.refresh_list();
                }
            }
            MailMsg::ArchiveSelected => {
                self.move_selected_messages(SpecialUse::Archive);
            }
            MailMsg::MarkSelectionSpam => {
                self.move_selected_messages(SpecialUse::Junk);
            }
            MailMsg::MarkSelectionNotSpam => {
                self.move_selected_messages(SpecialUse::Inbox);
            }
            MailMsg::ChooseMoveFolder => {
                self.choose_move_or_copy_folder(FolderPickerMode::Move, &sender, &self.window);
            }
            MailMsg::ChooseCopyFolder => {
                self.choose_move_or_copy_folder(FolderPickerMode::Copy, &sender, &self.window);
            }
            MailMsg::StartAccountMove { mode, groups } => {
                self.pending_account_moves = Some(PendingAccountMoves {
                    mode,
                    groups,
                    next: 0,
                });
                self.continue_account_move(&sender);
            }
            MailMsg::ApplySelectionToFolder { dest, copy } => {
                let ids = if let Some(queue) = &self.pending_account_moves {
                    queue
                        .groups
                        .get(queue.next)
                        .map(|(_, ids)| ids.clone())
                        .unwrap_or_default()
                } else {
                    selected_member_ids(&self.list_view)
                };
                self.apply_messages_to_folder(dest, ids, copy);
                if let Some(queue) = &mut self.pending_account_moves {
                    queue.next += 1;
                    if queue.next >= queue.groups.len() {
                        self.pending_account_moves = None;
                    } else {
                        self.continue_account_move(&sender);
                    }
                }
            }
            MailMsg::DropMessagesOnFolder { dest, ids, copy } => {
                if self.mail_content_pane == MailContentPane::Composer || ids.is_empty() {
                    return;
                }
                if !can_drop_messages_on_folder_data(
                    &self.folders,
                    &self.envelopes,
                    &self.source_envelopes,
                    dest,
                    &ids,
                ) {
                    return;
                }
                tracing::debug!(
                    dest = dest.0,
                    id_count = ids.len(),
                    copy,
                    "drop on folder: applying"
                );
                self.folder_context_menu.popdown();
                self.apply_messages_to_folder(dest, ids, copy);
            }
            MailMsg::MarkSelectionRead => {
                self.apply_flags_to_selection(Some(true), None);
            }
            MailMsg::MarkSelectionUnread => {
                self.apply_flags_to_selection(Some(false), None);
            }
            MailMsg::StarSelection => {
                self.apply_flags_to_selection(None, Some(true));
            }
            MailMsg::UnstarSelection => {
                self.apply_flags_to_selection(None, Some(false));
            }
            _ => unreachable!("message not routed to list_actions"),
        }
    }

    pub(super) fn move_selected_messages(&mut self, dest: SpecialUse) {
        if self.mail_content_pane == MailContentPane::Composer {
            return;
        }
        let ids = selected_ids(&self.list_view);
        if ids.is_empty() {
            return;
        }

        let mut missing_special = None::<&'static str>;
        let ids_to_move: Vec<MessageId> = ids
            .iter()
            .filter_map(|id| {
                let folder_id = self
                    .envelopes
                    .iter()
                    .find(|env| env.id == *id)
                    .or_else(|| self.source_envelopes.iter().find(|env| env.id == *id))
                    .map(|env| env.folder_id)?;
                let account_id = self
                    .envelopes
                    .iter()
                    .find(|env| env.id == *id)
                    .or_else(|| self.source_envelopes.iter().find(|env| env.id == *id))
                    .map(|env| env.account_id)?;

                match &dest {
                    SpecialUse::Junk | SpecialUse::Archive => {
                        if self.folder_is_special(folder_id, &dest) {
                            return None;
                        }
                        match self.special_folder_for(account_id, &dest) {
                            Some(_) => Some(*id),
                            None => {
                                missing_special = Some(match &dest {
                                    SpecialUse::Junk => "Junk",
                                    _ => "Archive",
                                });
                                None
                            }
                        }
                    }
                    SpecialUse::Inbox => {
                        if !self.folder_is_special(folder_id, &SpecialUse::Junk) {
                            return None;
                        }
                        self.special_folder_for(account_id, &SpecialUse::Inbox)
                            .map(|_| *id)
                    }
                    _ => None,
                }
            })
            .collect();

        if let Some(name) = missing_special {
            self.toasts.add_toast(crate::toast::plain(&format!(
                "No {name} folder on this account"
            )));
        }
        if ids_to_move.is_empty() {
            return;
        }

        let (undo_sources, undo_envelopes) = self.capture_move_sources(&ids_to_move);
        for id in &ids_to_move {
            if self.mock {
                self.mock_move_message(*id, dest.clone());
            }
        }
        let move_set: HashSet<MessageId> = ids_to_move.iter().copied().collect();
        let next_id = adjacent_after_removed(&self.envelopes, &move_set);
        self.remove_envelopes_from_ui(&ids_to_move, next_id);
        if !self.mock {
            self.cmd.emit(UiCommand::MoveMessages {
                ids: ids_to_move.clone(),
                dest: dest.clone(),
            });
        }
        let feedback = match dest {
            SpecialUse::Junk => MoveFeedback::MarkedSpam,
            SpecialUse::Inbox => MoveFeedback::NotSpam,
            _ => MoveFeedback::Archived,
        };
        self.offer_undo_move(feedback, undo_sources, undo_envelopes);
        ignore_focus_selection_briefly();
        {
            let _guard = suppress_list_selection();
            self.list_view.view.grab_focus();
            if let Some(id) = self.selected {
                restore_reader_row(&self.list_view, id);
            }
        }
    }

    pub(super) fn mock_move_message(&mut self, id: MessageId, dest: SpecialUse) {
        let Some(env) = self.source_envelopes.iter().find(|e| e.id == id).cloned() else {
            return;
        };
        if self.folder_is_special(env.folder_id, &dest) {
            return;
        }
        if let Some(dest_id) = self.special_folder_for(env.account_id, &dest) {
            for envelope in self.source_envelopes.iter_mut().filter(|e| e.id == id) {
                envelope.folder_id = dest_id;
            }
        }
    }

    pub(super) fn mock_move_message_to_folder(&mut self, id: MessageId, dest: FolderId) {
        let Some(env) = self.source_envelopes.iter().find(|e| e.id == id).cloned() else {
            return;
        };
        if env.folder_id == dest {
            return;
        }
        if !self.folder_exists(dest) {
            return;
        }
        for envelope in self.source_envelopes.iter_mut().filter(|e| e.id == id) {
            envelope.folder_id = dest;
        }
    }

    pub(super) fn mock_copy_message_to_folder(&mut self, id: MessageId, dest: FolderId) {
        let Some(env) = self.source_envelopes.iter().find(|e| e.id == id).cloned() else {
            return;
        };
        if env.folder_id == dest || !self.folder_exists(dest) {
            return;
        }
        let new_id = MessageId(
            self.source_envelopes
                .iter()
                .map(|envelope| envelope.id.0)
                .max()
                .unwrap_or(0)
                + 1,
        );
        let mut cloned = env;
        cloned.id = new_id;
        cloned.folder_id = dest;
        if let Some(body) = self.bodies.get(&id) {
            self.bodies.insert(new_id, body.clone());
        }
        if let Some(html) = self.html_bodies.get(&id) {
            self.html_bodies.insert(new_id, html.clone());
        }
        if let Some(attachments) = self.attachments_by_id.get(&id) {
            self.attachments_by_id.insert(new_id, attachments.clone());
        }
        self.source_envelopes.push(cloned);
    }

    pub(super) fn accounts_for_selection(&self, ids: &[MessageId]) -> Vec<(String, Vec<Folder>)> {
        let mut account_ids = Vec::new();
        let lookup = self.message_lookup();
        for id in ids {
            let Some(env) = lookup.envelope(*id) else {
                continue;
            };
            if !account_ids.contains(&env.account_id) {
                account_ids.push(env.account_id);
            }
        }
        self.accounts
            .iter()
            .filter(|account| account_ids.contains(&account.id))
            .filter_map(|account| {
                self.folders
                    .get(&account.id)
                    .map(|folders| (account.email.clone(), folders.clone()))
            })
            .collect()
    }

    pub(super) fn homogeneous_selection_folder(&self, ids: &[MessageId]) -> Option<FolderId> {
        let mut folder_id = None;
        let lookup = self.message_lookup();
        for id in ids {
            let env = lookup.envelope(*id)?;
            match folder_id {
                None => folder_id = Some(env.folder_id),
                Some(current) if current != env.folder_id => return None,
                _ => {}
            }
        }
        folder_id
    }

    pub(super) fn choose_move_or_copy_folder(
        &self,
        mode: FolderPickerMode,
        sender: &ComponentSender<MailModel>,
        root: &adw::ApplicationWindow,
    ) {
        if self.mail_content_pane == MailContentPane::Composer {
            return;
        }
        let ids = selected_member_ids(&self.list_view);
        if ids.is_empty() {
            return;
        }
        let groups = self.group_ids_by_account(&ids);
        if groups.len() > 1 {
            sender.input(MailMsg::StartAccountMove { mode, groups });
            return;
        }
        self.open_move_picker_for_ids(mode, &ids, sender, root);
    }

    pub(super) fn group_ids_by_account(
        &self,
        ids: &[MessageId],
    ) -> Vec<(AccountId, Vec<MessageId>)> {
        let mut order = Vec::new();
        let mut map: HashMap<AccountId, Vec<MessageId>> = HashMap::new();
        let lookup = self.message_lookup();
        for id in ids {
            let Some((account_id, _)) = lookup.location(*id) else {
                continue;
            };
            map.entry(account_id).or_default().push(*id);
            if !order.contains(&account_id) {
                order.push(account_id);
            }
        }
        order
            .into_iter()
            .filter_map(|account| map.get(&account).map(|ids| (account, ids.clone())))
            .collect()
    }

    pub(super) fn open_move_picker_for_ids(
        &self,
        mode: FolderPickerMode,
        ids: &[MessageId],
        sender: &ComponentSender<MailModel>,
        root: &adw::ApplicationWindow,
    ) {
        let accounts = self.accounts_for_selection(ids);
        if accounts.is_empty() {
            return;
        }
        let disabled = self
            .homogeneous_selection_folder(ids)
            .map(|folder_id| HashSet::from([folder_id.0]))
            .unwrap_or_default();
        let known_ids: HashSet<i64> = accounts
            .iter()
            .flat_map(|(_, folders)| folders.iter().map(|folder| folder.id.0))
            .collect();
        let recents = recent_move_folders()
            .into_iter()
            .filter(|id| known_ids.contains(id))
            .map(FolderId)
            .collect();
        let spec = FolderPickerSpec {
            mode,
            accounts,
            disabled,
            recents,
        };
        let copy = matches!(mode, FolderPickerMode::Copy);
        let sender = sender.clone();
        open_folder_picker(root, spec, move |dest| {
            sender.input(MailMsg::ApplySelectionToFolder { dest, copy });
        });
    }

    pub(super) fn continue_account_move(&mut self, sender: &ComponentSender<MailModel>) {
        let Some(queue) = self.pending_account_moves.as_ref() else {
            return;
        };
        let Some((account_id, ids)) = queue.groups.get(queue.next) else {
            self.pending_account_moves = None;
            return;
        };
        self.open_move_picker_for_ids(queue.mode, ids, sender, &self.window);
        let _ = account_id;
    }

    pub(super) fn apply_messages_to_folder(
        &mut self,
        dest: FolderId,
        ids: Vec<MessageId>,
        copy: bool,
    ) {
        tracing::debug!(
            dest = dest.0,
            id_count = ids.len(),
            copy,
            "apply_messages_to_folder"
        );
        if self.mail_content_pane == MailContentPane::Composer || ids.is_empty() {
            return;
        }

        let Some(dest_folder) = self.folder_for_id(dest).cloned() else {
            self.toasts
                .add_toast(crate::toast::plain("Folder not found"));
            return;
        };
        if !dest_folder.selectable {
            self.toasts
                .add_toast(crate::toast::plain("Folder cannot hold messages"));
            return;
        }

        let mut skipped_other_account = false;
        let lookup = self.message_lookup();
        let ids_to_apply: Vec<MessageId> = ids
            .iter()
            .filter_map(|id| {
                let (account_id, folder_id) = lookup.location(*id)?;
                if account_id != dest_folder.account_id {
                    skipped_other_account = true;
                    return None;
                }
                if folder_id == dest {
                    return None;
                }
                Some(*id)
            })
            .collect();

        if skipped_other_account {
            let message = if copy {
                "Cannot copy messages to another account"
            } else {
                "Cannot move messages from another account"
            };
            self.toasts.add_toast(crate::toast::plain(message));
        }
        tracing::debug!(
            copy,
            requested = ids.len(),
            applying = ids_to_apply.len(),
            skipped_other_account,
            mock = self.mock,
            "apply_messages_to_folder: filtered ids"
        );
        if ids_to_apply.is_empty() {
            return;
        }

        let (undo_sources, undo_envelopes) = if copy {
            (Vec::new(), Vec::new())
        } else {
            self.capture_move_sources(&ids_to_apply)
        };
        for id in &ids_to_apply {
            if self.mock {
                if copy {
                    self.mock_copy_message_to_folder(*id, dest);
                } else {
                    self.mock_move_message_to_folder(*id, dest);
                }
            } else if copy {
                tracing::debug!(
                    id = id.0,
                    dest = dest.0,
                    "emit UiCommand::CopyMessageToFolder"
                );
                self.cmd
                    .emit(UiCommand::CopyMessageToFolder { id: *id, dest });
            }
        }

        push_recent_move_folder(dest.0);

        if copy {
            if self.mock
                && matches!(self.mail_view, MailView::Folder(folder_id) if folder_id == dest)
            {
                self.refresh_list();
            }
            self.toasts.add_toast(crate::toast::plain(&move_toast_title(
                MoveFeedback::CopiedTo(&folder_display_name(&dest_folder)),
                ids_to_apply.len(),
            )));
            return;
        }

        let move_set: HashSet<MessageId> = ids_to_apply.iter().copied().collect();
        let next_id = adjacent_after_removed(&self.envelopes, &move_set);
        self.remove_envelopes_from_ui(&ids_to_apply, next_id);
        if !self.mock {
            tracing::debug!(
                count = ids_to_apply.len(),
                dest = dest.0,
                "emit UiCommand::MoveMessagesToFolder"
            );
            self.cmd.emit(UiCommand::MoveMessagesToFolder {
                ids: ids_to_apply.clone(),
                dest,
            });
        }
        self.offer_undo_move(
            MoveFeedback::MovedTo(&folder_display_name(&dest_folder)),
            undo_sources,
            undo_envelopes,
        );
        ignore_focus_selection_briefly();
        {
            let _guard = suppress_list_selection();
            self.list_view.view.grab_focus();
            if let Some(id) = self.selected {
                restore_reader_row(&self.list_view, id);
            }
        }
    }

    pub(super) fn remove_envelopes_from_ui(
        &mut self,
        ids: &[MessageId],
        next_id: Option<MessageId>,
    ) {
        tracing::debug!(
            ids = ?ids.iter().map(|id| id.0).collect::<Vec<_>>(),
            "remove_envelopes_from_ui"
        );
        let delete_set: HashSet<MessageId> = ids.iter().copied().collect();
        for id in ids {
            if let Some(window) = self.message_windows.remove(id) {
                window.close();
            }
        }
        let selection_cleared = self
            .selected
            .is_some_and(|selected| delete_set.contains(&selected));
        // Multi-select keeps a placeholder even when `selected` is None (the
        // last reader target was not in the set). Still replace the reader.
        let replace_reader = selection_cleared || ids.len() > 1;
        let visible_envelopes: Vec<Envelope> = if self.list_uses_conversations() {
            self.list_rows()
                .iter()
                .map(|row| row.envelope.clone())
                .collect()
        } else {
            Vec::new()
        };
        self.envelopes.retain(|env| !delete_set.contains(&env.id));
        let mut effective_next_id = next_id;
        if self.list_uses_conversations() {
            self.conversations = group_envelopes(&self.envelopes);
            self.refresh_list();
            if effective_next_id
                .is_some_and(|id| !self.list_rows().iter().any(|row| row.envelope.id == id))
            {
                effective_next_id = adjacent_after_removed(&visible_envelopes, &delete_set);
            }
        } else {
            remove_ids_from_list(&mut self.list_view, ids);
        }
        let send_in_progress = composer_send_in_progress(
            self.mail_content_pane == MailContentPane::Composer,
            self.pending_send_draft.is_some(),
        );
        if send_in_progress && replace_reader {
            self.reopen_reader_after_send = true;
        }
        if replace_reader {
            self.selected = effective_next_id;
            if !send_in_progress && effective_next_id.is_none() {
                clear_reader(&self.reader);
            }
        }
        self.new_mail_chip.remove_ids(ids, &self.envelopes);
        let loading = self.list_loading();
        let searching = self.mail_view == MailView::Search || self.is_search_backed(self.mail_view);
        sync_empty_state_from_count(
            Some(&self.messages_body_stack),
            Some(&self.empty_status),
            self.envelopes.is_empty(),
            loading,
            searching,
            self.list_uses_conversations(),
            self.list_empty_view(),
        );
        if replace_reader {
            if send_in_progress {
                if let Some(next_id) = effective_next_id {
                    let _guard = suppress_list_selection();
                    select_ids(&self.list_view, &[next_id]);
                }
            } else if let Some(next_id) = effective_next_id {
                {
                    let _guard = suppress_list_selection();
                    select_ids(&self.list_view, &[next_id]);
                }
                self.open_row_for_latest(next_id, None);
            }
        }
        self.sync_selection_chrome();
        self.refresh_status_bar();
        let query = self.search_entry.text().to_string();
        self.sync_search_chrome(&query, None);
    }

    pub(super) fn apply_flags_to_selection(&mut self, seen: Option<bool>, flagged: Option<bool>) {
        let ids = selected_member_ids(&self.list_view);
        if ids.is_empty() {
            return;
        }

        let mut changed_ids = Vec::new();
        for id in &ids {
            let id = *id;
            let envelope = self.envelopes.iter().find(|env| env.id == id);
            if let Some(envelope) = envelope {
                if seen.is_some_and(|seen| envelope.seen == seen) {
                    continue;
                }
                if flagged.is_some_and(|flagged| envelope.flagged == flagged) {
                    continue;
                }
            }
            if let Some(seen) = seen {
                self.adjust_unread_badges(id, seen);
                for env in self.envelopes.iter_mut().filter(|env| env.id == id) {
                    env.seen = seen;
                }
                for env in self.source_envelopes.iter_mut().filter(|env| env.id == id) {
                    env.seen = seen;
                }
            }
            if let Some(flagged) = flagged {
                for env in self.envelopes.iter_mut().filter(|env| env.id == id) {
                    env.flagged = flagged;
                }
                for env in self.source_envelopes.iter_mut().filter(|env| env.id == id) {
                    env.flagged = flagged;
                }
            }
            changed_ids.push(id);
        }

        if !self.mock && !changed_ids.is_empty() {
            self.cmd.emit(UiCommand::SetFlags {
                ids: changed_ids.clone(),
                seen,
                flagged,
            });
        }

        if changed_ids.is_empty() {
            return;
        }
        if seen.is_some() {
            self.mail_sender.emit(MailMsg::RefreshFolderBadges);
        }

        for id in &changed_ids {
            if let Some(conversation) = self
                .conversations
                .iter_mut()
                .find(|conversation| conversation.member_ids.contains(id))
            {
                if let Some(member) = conversation.members.iter_mut().find(|m| m.id == *id) {
                    if let Some(seen) = seen {
                        member.seen = seen;
                    }
                    if let Some(flagged) = flagged {
                        member.flagged = flagged;
                    }
                }
                conversation.unread_count =
                    conversation.members.iter().filter(|m| !m.seen).count() as u32;
                conversation.any_flagged = conversation.members.iter().any(|m| m.flagged);
            }
        }

        let mut synced_threads = HashSet::new();
        for id in changed_ids {
            if self.list_uses_conversations()
                && let Some(thread_id) = self
                    .conversations
                    .iter()
                    .find(|conversation| conversation.member_ids.contains(&id))
                    .map(|conversation| conversation.thread_id)
            {
                if synced_threads.insert(thread_id) {
                    self.sync_conversation_row(thread_id);
                }
                continue;
            }
            self.refresh_listed_flag_row(id);
        }
        self.sync_selection_chrome();
        self.refresh_status_bar();
    }

    /// Patch unread/star cells after a local flag change. Flat lists must
    /// update the envelope row; conversation mode rebuilds from the thread.
    /// A list row always has a `thread_id`, so treating that as "sync the
    /// conversation" skips the flat-list patch and leaves unread styling stuck.
    pub(super) fn refresh_listed_flag_row(&mut self, id: MessageId) -> bool {
        if self.list_uses_conversations()
            && let Some(thread_id) = self
                .conversations
                .iter()
                .find(|conversation| conversation.member_ids.contains(&id))
                .map(|conversation| conversation.thread_id)
        {
            self.sync_conversation_row(thread_id);
            return true;
        }
        if let Some(pos) = find_message_position(&self.list_view, id)
            && let Some(env) = self.envelopes.iter().find(|e| e.id == id)
        {
            update_envelope_in_list(
                &mut self.list_view,
                pos,
                env,
                Some(&self.messages_scrolled),
                self.selected,
            );
            return true;
        }
        false
    }

    /// Move sidebar unread totals with a message that was just marked read or unread.
    ///
    /// The worker recounts later. Until then the badge must not keep showing
    /// mail that the list already treats as read.
    pub(super) fn adjust_unread_badges(&mut self, id: MessageId, seen: bool) {
        let Some(env) = self.envelope_for(id).cloned() else {
            return;
        };
        let Some(folder) = self.folder(env.folder_id).cloned() else {
            return;
        };
        let delta: i32 = if seen { -1 } else { 1 };
        for folders in self.folders.values_mut() {
            if let Some(stored) = folders.iter_mut().find(|item| item.id == folder.id) {
                stored.unread = add_unread(stored.unread, delta);
            }
        }
        if let Some((inbox, total)) = self.unread_by_account.get_mut(&folder.account_id) {
            *total = add_unread(*total, delta);
            if folder.special == SpecialUse::Inbox {
                *inbox = add_unread(*inbox, delta);
            }
        }
    }

    pub(super) fn mark_message_seen(&mut self, id: MessageId) {
        notifications::withdraw_message_notification(id);
        let already_seen = self
            .envelopes
            .iter()
            .find(|env| env.id == id)
            .or_else(|| self.source_envelopes.iter().find(|env| env.id == id))
            .is_some_and(|env| env.seen);
        if already_seen {
            return;
        }
        for env in self.envelopes.iter_mut().filter(|env| env.id == id) {
            env.seen = true;
        }
        for env in self.source_envelopes.iter_mut().filter(|env| env.id == id) {
            env.seen = true;
        }
        if let Some(conversation) = self
            .conversations
            .iter_mut()
            .find(|conversation| conversation.member_ids.contains(&id))
        {
            if let Some(member) = conversation.members.iter_mut().find(|m| m.id == id) {
                member.seen = true;
            }
            conversation.unread_count =
                conversation.members.iter().filter(|m| !m.seen).count() as u32;
        }
        self.adjust_unread_badges(id, true);
        if !self.mock {
            self.cmd.emit(UiCommand::SetFlag {
                id,
                seen: Some(true),
                flagged: None,
            });
        }
        self.refresh_listed_flag_row(id);
        self.mail_sender.emit(MailMsg::RefreshFolderBadges);
    }
}

pub(super) fn adjacent_after_removed(
    envelopes: &[Envelope],
    delete_set: &HashSet<MessageId>,
) -> Option<MessageId> {
    let positions: Vec<usize> = envelopes
        .iter()
        .enumerate()
        .filter(|(_, env)| delete_set.contains(&env.id))
        .map(|(index, _)| index)
        .collect();
    if positions.is_empty() {
        return None;
    }
    let last = *positions.iter().max().unwrap();
    let first = *positions.iter().min().unwrap();
    for env in envelopes.iter().skip(last + 1) {
        if !delete_set.contains(&env.id) {
            return Some(env.id);
        }
    }
    for index in (0..first).rev() {
        if !delete_set.contains(&envelopes[index].id) {
            return Some(envelopes[index].id);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mail::test_support::envelope_with_id;

    #[test]
    fn adjacent_after_removed_bottom_row_selects_above() {
        let envelopes = vec![
            envelope_with_id(1),
            envelope_with_id(2),
            envelope_with_id(3),
        ];
        let delete_set = HashSet::from([MessageId(3)]);
        assert_eq!(
            adjacent_after_removed(&envelopes, &delete_set),
            Some(MessageId(2))
        );
    }
}
