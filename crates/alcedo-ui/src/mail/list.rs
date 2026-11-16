//! Message list: population, selection, conversations and auto-open.

use super::list_actions::adjacent_after_removed;
use super::message_lookup::{MessageLookup, index_envelopes};
use super::*;

impl MailModel {
    pub(super) fn update_list(&mut self, msg: MailMsg, sender: ComponentSender<MailModel>) {
        match msg {
            MailMsg::ListSelectionChanged => {
                if list_selection_suppressed() {
                    return;
                }
                let ids = selected_ids(&self.list_view);
                if focus_selection_ignored() && !focus_ignore_is_commit(&ids, self.selected) {
                    if ids.is_empty()
                        && let Some(id) = self.selected
                    {
                        restore_reader_row(&self.list_view, id);
                    }
                    self.sync_selection_chrome();
                    self.refresh_status_bar();
                    return;
                }
                if !ids.is_empty() {
                    note_list_selection_commit();
                }
                let count = ids.len() as u32;

                if count == 0 {
                    self.selected = None;
                    clear_reader(&self.reader);
                    if self.layout.single_pane {
                        self.back_to_messages();
                    }
                    self.sync_selection_chrome();
                    self.refresh_status_bar();
                    return;
                }

                if count == 1 {
                    self.note_single_pane_detail();
                    let id = ids[0];
                    // Multi-select keeps `selected` as the last reader target while
                    // showing a placeholder. Collapsing back to that row must reopen.
                    let showing_body = self.reader.content_stack.visible_child_name().as_deref()
                        == Some("content");
                    if self.is_editing_listed_draft(id) {
                        self.selected = Some(id);
                        self.sync_selection_chrome();
                        self.refresh_status_bar();
                    } else if self.selected != Some(id) || !showing_body {
                        sender.input(MailMsg::OpenMessage(id));
                    } else {
                        self.sync_selection_chrome();
                        self.refresh_status_bar();
                    }
                    return;
                }

                if self.layout.single_pane {
                    self.back_to_messages();
                }
                show_reader_placeholder(&self.reader, ReaderPlaceholder::Multi { count });
                if let Some(selected) = self.selected
                    && !ids.contains(&selected)
                {
                    self.selected = None;
                }
                self.sync_selection_chrome();
                self.refresh_status_bar();
            }
            MailMsg::ListActivated(position) => {
                if list_selection_suppressed() {
                    return;
                }
                if let Some(message_id) = message_at(&self.list_view, position) {
                    if self.activate_in_extension_view(message_id) {
                        return;
                    }
                    if self
                        .envelopes
                        .iter()
                        .find(|env| env.id == message_id)
                        .is_some_and(|env| self.is_imap_draft_envelope(env))
                    {
                        self.pending_popout_draft = Some(message_id);
                        sender.input(MailMsg::OpenMessage(message_id));
                    } else {
                        sender.input(MailMsg::OpenMessageWindow(message_id));
                    }
                }
            }
            MailMsg::ListDensityChanged(density) => {
                reload_reader_document(&self.reader);
                for window in self.message_windows.values() {
                    window.apply_density(density);
                }
            }
            MailMsg::ListModeChanged(mode) => {
                if self.list_mode == mode {
                    return;
                }
                self.list_mode = mode;
                self.selected = None;
                self.reissue_list_select();
            }
            MailMsg::ToggleThreadExpand(thread_id) => {
                if self.list_mode != MessageListMode::Conversation {
                    return;
                }
                if self.expanded_threads.contains(&thread_id) {
                    self.expanded_threads.remove(&thread_id);
                } else {
                    self.expanded_threads.insert(thread_id);
                }
                self.refresh_list();
            }
            MailMsg::SelectAllMessages => {
                self.list_view.selection_model.select_all();
            }
            MailMsg::RowContextMenu { id, x, y } => {
                self.context_message_id = Some(id);
                let ids = selected_ids(&self.list_view);
                if !ids.contains(&id)
                    && let Some(pos) = find_visible_position(&self.list_view, id)
                {
                    {
                        let _guard = suppress_list_selection();
                        self.list_view.selection_model.select_item(pos, true);
                    }
                    self.cmd.emit(UiCommand::OpenMessage(id));
                    self.open_message(id);
                }
                self.sync_selection_chrome();
                let rect = gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1);
                self.mail_context_menu.set_pointing_to(Some(&rect));
                self.mail_context_menu.popup();
            }
            _ => unreachable!("message not routed to list"),
        }
    }

    pub(super) fn folder_label_map(&self) -> HashMap<FolderId, FolderLabel> {
        let mut map = HashMap::new();
        for folder in self.folders.values().flatten() {
            map.insert(
                folder.id,
                FolderLabel {
                    name: folder_display_name(folder),
                    special: folder.special.clone(),
                },
            );
        }
        map
    }

    pub(super) fn account_label_map(&self) -> HashMap<AccountId, String> {
        self.accounts
            .iter()
            .map(|account| (account.id, account.label().to_string()))
            .collect()
    }

    pub(super) fn account_chip_map(&self) -> HashMap<AccountId, AccountChipSnap> {
        self.accounts
            .iter()
            .map(|account| {
                (
                    account.id,
                    AccountChipSnap {
                        initials: account.avatar_chip(),
                        accent: account.accent_color.clone(),
                        image: account.avatar_image.clone(),
                    },
                )
            })
            .collect()
    }

    pub(super) fn column_scope(&self) -> MessageListColumnScope {
        match self.mail_view {
            MailView::UnifiedInbox => MessageListColumnScope::UnifiedInbox,
            MailView::Search => MessageListColumnScope::Search,
            MailView::Extension(id) => self
                .extension_view_spec(id)
                .map(|(_, spec)| MessageListColumnScope::Extension(spec.column_scope))
                .unwrap_or(MessageListColumnScope::Search),
            MailView::Folder(folder_id) => self
                .folder(folder_id)
                .map(|folder| MessageListColumnScope::Mailbox {
                    account: folder.account_id,
                    imap_name: folder.imap_name.clone(),
                })
                .unwrap_or(MessageListColumnScope::Folder(folder_id)),
        }
    }

    pub(super) fn sync_column_layout(&self) {
        apply_message_list_columns_for_scope(&self.list_view, self.column_scope());
    }

    /// Conversation grouping applies to IMAP folders and searches, not to
    /// lists an extension fills.
    pub(super) fn list_uses_conversations(&self) -> bool {
        self.list_mode == MessageListMode::Conversation
            && (!matches!(self.mail_view, MailView::Extension(_))
                || self.is_search_backed(self.mail_view))
    }

    pub(super) fn envelope_for(&self, id: MessageId) -> Option<&Envelope> {
        self.envelopes
            .iter()
            .find(|env| env.id == id)
            .or_else(|| self.source_envelopes.iter().find(|env| env.id == id))
    }

    /// Lookups for one bulk operation; see [`MessageLookup`].
    pub(super) fn message_lookup(&self) -> MessageLookup<'_> {
        MessageLookup::build(&self.envelopes, &self.source_envelopes, &self.conversations)
    }

    pub(super) fn sync_selection_chrome(&self) {
        let ids = selected_ids(&self.list_view);
        let count = ids.len() as u32;
        let total = self.envelopes.len() as u32;
        // One pass over the list, then one over the selection: this runs on
        // every selection change, including select-all in a large folder.
        let by_id = index_envelopes(&self.envelopes);
        // One entry per selected id; `None` is an id with no row in the list.
        let rows: Vec<Option<&Envelope>> = ids.iter().map(|id| by_id.get(id).copied()).collect();
        let selected: Vec<&Envelope> = rows.iter().flatten().copied().collect();
        let in_special =
            |env: &Envelope, special: &SpecialUse| self.folder_is_special(env.folder_id, special);
        let any_unseen = selected.iter().any(|env| !env.seen);
        let any_seen = selected.iter().any(|env| env.seen);
        let any_unstarred = selected.iter().any(|env| !env.flagged);
        let any_starred = selected.iter().any(|env| env.flagged);
        let any_not_junk = selected
            .iter()
            .any(|env| !in_special(env, &SpecialUse::Junk));
        let any_junk = selected
            .iter()
            .any(|env| in_special(env, &SpecialUse::Junk));
        let any_not_archive = selected
            .iter()
            .any(|env| !in_special(env, &SpecialUse::Archive));
        let flagged = count == 1
            && rows
                .first()
                .copied()
                .flatten()
                .is_some_and(|env| env.flagged);

        self.selection_bar.sync(
            count,
            total,
            any_unseen,
            any_seen,
            any_unstarred,
            any_starred,
        );

        self.mail_actions
            .select_all
            .set_enabled(count < total && total > 0);
        self.mail_actions.mark_read.set_enabled(any_unseen);
        self.mail_actions.mark_unread.set_enabled(any_seen);
        self.mail_actions.star.set_enabled(any_unstarred);
        self.mail_actions.unstar.set_enabled(any_starred);
        self.mail_actions.mark_spam.set_enabled(any_not_junk);
        self.mail_actions.mark_not_spam.set_enabled(any_junk);
        self.mail_actions.choose_move_folder.set_enabled(count > 0);
        self.mail_actions.choose_copy_folder.set_enabled(count > 0);
        self.mail_actions.archive.set_enabled(any_not_archive);
        self.mail_actions.delete.set_enabled(count > 0);
        self.mail_actions
            .delete_permanently
            .set_enabled(count > 0 && !self.view_handles_delete());
        self.mail_actions.reply.set_enabled(count == 1);
        self.mail_actions.reply_all.set_enabled(count == 1);
        self.mail_actions.forward.set_enabled(count == 1);
        self.mail_actions.forward_window.set_enabled(count == 1);
        self.mail_actions.reply_window.set_enabled(count == 1);
        self.mail_actions.reply_all_window.set_enabled(count == 1);
        self.mail_actions.create_rule.set_enabled(count == 1);
        self.sync_extension_actions(&selected);
        self.mail_actions
            .sync_folder
            .set_enabled(self.can_sync_folder());
        self.sync_file_folder_actions();

        set_reader_actions_sensitive(&self.reader, count == 1);
        if count == 1 {
            sync_reader_star(&self.reader, flagged);
        }
        self.notify_reader_selection();
    }

    pub(super) fn reset_list_selection(&mut self) {
        self.selected = None;
        self.list_view.selection_model.unselect_all();
        clear_reader(&self.reader);
        self.messages_scrolled.vadjustment().set_value(0.0);
        self.sync_selection_chrome();
    }

    pub(super) fn list_rows(&self) -> Vec<MessageRow> {
        if self.list_uses_conversations() {
            flatten_conversation_rows(&self.conversations, &self.expanded_threads)
        } else {
            self.envelopes
                .iter()
                .cloned()
                .map(MessageRow::from_envelope)
                .collect()
        }
    }

    pub(super) fn conversation_lookup_envelopes(conversations: &[Conversation]) -> Vec<Envelope> {
        let mut envelopes = Vec::new();
        let mut seen = HashSet::new();
        for conversation in conversations {
            for member in &conversation.members {
                if seen.insert(member.id) {
                    envelopes.push(member.clone());
                }
            }
        }
        envelopes.sort_by(|a, b| b.date.cmp(&a.date));
        envelopes
    }

    pub(super) fn reply_message_id(&self) -> Option<MessageId> {
        self.selected
    }

    pub(super) fn sync_conversation_row(&mut self, thread_id: ThreadId) {
        let Some(index) = self
            .conversations
            .iter()
            .position(|conversation| conversation.thread_id == thread_id)
        else {
            return;
        };
        let member_ids = self.conversations[index].member_ids.clone();
        for member_id in &member_ids {
            if let Some(env) = self.envelopes.iter().find(|env| env.id == *member_id)
                && let Some(member) = self.conversations[index]
                    .members
                    .iter_mut()
                    .find(|m| m.id == *member_id)
            {
                member.seen = env.seen;
                member.flagged = env.flagged;
            }
        }
        let conversation = &mut self.conversations[index];
        conversation.latest = conversation
            .members
            .last()
            .cloned()
            .unwrap_or_else(|| conversation.latest.clone());
        conversation.unread_count = conversation.members.iter().filter(|m| !m.seen).count() as u32;
        conversation.any_flagged = conversation.members.iter().any(|m| m.flagged);
        let rows = flatten_conversation_rows(
            std::slice::from_ref(&self.conversations[index]),
            &self.expanded_threads,
        );
        for row in rows {
            if let Some(pos) = find_message_position(&self.list_view, row.envelope.id) {
                mutate_row_at(&self.list_view, pos, &row);
            } else {
                self.refresh_list();
                return;
            }
        }
    }

    pub(super) fn reissue_list_select(&mut self) {
        match self.mail_view {
            MailView::UnifiedInbox => {
                self.cmd.emit(UiCommand::SelectUnifiedInbox {
                    mode: self.list_mode,
                });
            }
            MailView::Folder(folder_id) => {
                self.cmd.emit(UiCommand::SelectFolder {
                    folder: folder_id,
                    mode: self.list_mode,
                    sync: false,
                });
            }
            MailView::Search => {
                let query = self.search_entry.text().to_string();
                let parsed = parse_search_query(&query);
                self.cmd.emit(UiCommand::Search {
                    query,
                    folder_ids: self.search_folder_ids(&parsed),
                    mode: self.list_mode,
                });
            }
            MailView::Extension(_) if self.is_search_backed(self.mail_view) => {
                let query = self.search_entry.text().to_string();
                self.cmd.emit(UiCommand::Search {
                    query,
                    folder_ids: None,
                    mode: self.list_mode,
                });
            }
            MailView::Extension(id) => {
                if let Some((extension, _)) = self.extension_view_spec(id) {
                    extension.mail_view_opened(id, &self.ext_ctx);
                }
            }
        }
    }

    pub(super) fn list_loading(&self) -> bool {
        let conversation_mode = self.list_uses_conversations();
        let list_empty = if conversation_mode {
            self.conversations.is_empty()
        } else {
            self.envelopes.is_empty()
        };
        let extension_loading = match self.mail_view {
            MailView::Extension(id) => {
                self.envelopes.is_empty()
                    && !self.mock
                    && self
                        .extension_view_spec(id)
                        .is_some_and(|(_, spec)| spec.loading)
            }
            _ => false,
        };
        message_list_loading(
            self.pending_folder_sync.is_some(),
            self.pending_virtual_sync == Some(self.mail_view),
            list_empty,
            extension_loading,
        )
    }

    /// Empty-list copy for the shown view.
    pub(super) fn list_empty_view(&self) -> ListEmptyView {
        let MailView::Extension(id) = self.mail_view else {
            return ListEmptyView::Default;
        };
        self.extension_view_spec(id)
            .map(|(_, spec)| ListEmptyView::Extension {
                title: spec.empty_title,
                description: spec.empty_description,
            })
            .unwrap_or_default()
    }

    pub(super) fn refresh_list(&mut self) {
        let loading = self.list_loading();
        let searching = self.mail_view == MailView::Search || self.is_search_backed(self.mail_view);
        let conversation_mode = self.list_uses_conversations();
        let list_empty_view = self.list_empty_view();
        let rows = self.list_rows();
        let selected = ids_to_restore_on_refresh(
            &rows,
            &selected_ids(&self.list_view),
            self.selected,
            conversation_mode,
        );
        populate_list_preserved(
            &mut self.list_view,
            &rows,
            &selected,
            Some(&self.messages_scrolled),
            Some(&self.messages_body_stack),
            Some(&self.empty_status),
            loading,
            searching,
            conversation_mode,
            list_empty_view,
            Some(&self.new_mail_chip),
        );
        let still_visible = self
            .selected
            .is_some_and(|id| self.envelopes.iter().any(|env| env.id == id));
        if !still_visible {
            self.selected = None;
            clear_reader(&self.reader);
        }
        if let Some((pending_id, _)) = self.pending_notification_message {
            if self.envelopes.iter().any(|env| env.id == pending_id) {
                self.open_pending_notification_message(pending_id);
            }
        } else if self.selected.is_none() {
            self.open_autoselected_message_without_sender();
        } else if let Some(id) = self.selected {
            let gtk_ids = selected_ids(&self.list_view);
            if refresh_should_restore_reader(&gtk_ids, self.selected) {
                restore_reader_row(&self.list_view, id);
            } else if refresh_follows_newer_bitset(&gtk_ids, self.selected) {
                note_list_selection_commit();
                if gtk_ids.len() == 1 {
                    let follow = gtk_ids[0];
                    self.open_row_for_latest(follow, None);
                } else {
                    show_reader_placeholder(
                        &self.reader,
                        ReaderPlaceholder::Multi {
                            count: gtk_ids.len() as u32,
                        },
                    );
                    self.selected = None;
                }
            }
        }
        self.sync_selection_chrome();
        let query = self.search_entry.text().to_string();
        self.sync_search_chrome(&query, None);
        self.refresh_status_bar();
    }

    pub(super) fn patch_listed_envelope(&mut self, env: Envelope) {
        let id = env.id;
        if let Some(existing) = self.source_envelopes.iter_mut().find(|item| item.id == id) {
            *existing = env.clone();
        }
        if let Some(existing) = self.envelopes.iter_mut().find(|item| item.id == id) {
            *existing = env.clone();
        } else {
            return;
        }
        let conversation_thread = if self.list_uses_conversations() {
            self.conversations.iter_mut().find_map(|conversation| {
                if !conversation.member_ids.contains(&id) {
                    return None;
                }
                if let Some(member) = conversation.members.iter_mut().find(|m| m.id == id) {
                    *member = env.clone();
                }
                if conversation
                    .members
                    .last()
                    .is_some_and(|member| member.id == id)
                {
                    conversation.latest = env.clone();
                }
                conversation.unread_count =
                    conversation.members.iter().filter(|m| !m.seen).count() as u32;
                conversation.any_flagged = conversation.members.iter().any(|m| m.flagged);
                conversation.any_attachments =
                    conversation.members.iter().any(|m| m.has_attachments);
                Some(conversation.thread_id)
            })
        } else {
            None
        };
        let Some(listed) = self.envelopes.iter().find(|item| item.id == id).cloned() else {
            return;
        };
        if let Some(thread_id) = conversation_thread {
            self.sync_conversation_row(thread_id);
        } else if let Some(pos) = find_message_position(&self.list_view, id) {
            update_envelope_in_list(
                &mut self.list_view,
                pos,
                &listed,
                Some(&self.messages_scrolled),
                self.selected,
            );
        } else {
            self.refresh_list();
            return;
        }
        sync_list_selection_to_reader(&self.list_view, self.selected);
        schedule_sync_list_selection_to_reader(&self.list_view, self.selected);
        self.sync_selection_chrome();
        self.refresh_reader_headers(id, &listed);
    }

    pub(super) fn set_listed_has_attachments(&mut self, id: MessageId, has: bool) {
        let Some(env) = self
            .envelopes
            .iter()
            .find(|item| item.id == id)
            .cloned()
            .or_else(|| {
                self.source_envelopes
                    .iter()
                    .find(|item| item.id == id)
                    .cloned()
            })
        else {
            return;
        };
        if env.has_attachments == has {
            return;
        }
        let mut updated = env;
        updated.has_attachments = has;
        self.patch_listed_envelope(updated);
    }

    /// If nothing is open yet, autoselect and open the first visible row.
    pub(super) fn open_autoselected_message(&mut self, sender: &ComponentSender<MailModel>) {
        if self.selected.is_some() {
            return;
        }
        let count = selection_count(&self.list_view);
        let len = self.list_view.len();
        if count == 0 && len > 0 {
            self.list_view.selection_model.select_item(0, true);
            if let Some(id) = message_at(&self.list_view, 0) {
                self.open_row_for_latest(id, Some(sender));
            }
            return;
        }
        if count == 1 {
            let ids = selected_ids(&self.list_view);
            if let Some(id) = ids.first() {
                self.open_row_for_latest(*id, Some(sender));
            }
        }
    }

    pub(super) fn open_autoselected_message_without_sender(&mut self) {
        if self.selected.is_some() {
            return;
        }
        let count = selection_count(&self.list_view);
        let len = self.list_view.len();
        if count == 0 && len > 0 {
            self.list_view.selection_model.select_item(0, true);
            if let Some(id) = message_at(&self.list_view, 0) {
                self.open_row_for_latest(id, None);
            }
            return;
        }
        if count == 1 {
            let ids = selected_ids(&self.list_view);
            if let Some(id) = ids.first() {
                self.open_row_for_latest(*id, None);
            }
        }
    }

    pub(super) fn open_row_for_latest(
        &mut self,
        id: MessageId,
        sender: Option<&ComponentSender<MailModel>>,
    ) {
        if self.should_skip_autolist_open(id) {
            return;
        }
        if let Some(sender) = sender {
            sender.input(MailMsg::OpenMessage(id));
        } else {
            self.queue_open_and_prefetch(id);
            self.open_message(id);
        }
    }

    pub(super) fn autoselect_neighbor_id(&self, id: MessageId) -> Option<MessageId> {
        let conversation_envelopes: Vec<Envelope>;
        let envelopes: &[Envelope] = if self.list_uses_conversations() {
            conversation_envelopes = self
                .list_rows()
                .iter()
                .map(|row| row.envelope.clone())
                .collect();
            &conversation_envelopes
        } else {
            &self.envelopes
        };
        adjacent_after_removed(envelopes, &HashSet::from([id]))
    }

    pub(super) fn queue_open_and_prefetch(&mut self, id: MessageId) {
        if self.mock {
            return;
        }
        self.cmd.emit(UiCommand::OpenMessage(id));
        let Some(neighbor) = self.autoselect_neighbor_id(id) else {
            return;
        };
        if self.bodies.contains_key(&neighbor) || self.html_bodies.contains_key(&neighbor) {
            return;
        }
        if self
            .envelope_for(neighbor)
            .is_some_and(|env| self.is_imap_draft_envelope(env))
        {
            return;
        }
        self.cmd.emit(UiCommand::PrefetchMessage(neighbor));
    }

    pub(super) fn should_skip_autolist_open(&self, id: MessageId) -> bool {
        if self.pending_imap_draft_open == Some(id) {
            return true;
        }
        if self.mail_content_pane != MailContentPane::Composer {
            return false;
        }
        self.envelope_for(id)
            .is_some_and(|env| self.is_imap_draft_envelope(env) && self.is_editing_imap_draft(env))
    }

    pub(super) fn merge_appended_envelopes(&mut self, folder: FolderId, envelopes: Vec<Envelope>) {
        self.envelopes.retain(|env| env.folder_id != folder);
        self.envelopes.extend(envelopes);
        self.envelopes.sort_by(|a, b| b.date.cmp(&a.date));
    }
}

pub(super) fn kind_is_extension_list(kind: EnvelopeListKind) -> bool {
    matches!(kind, EnvelopeListKind::Extension(_))
}

/// Empty message pane should show the pull icon while a folder or virtual
/// inbox list is still on its way.
pub(super) fn message_list_loading(
    pending_folder: bool,
    pending_virtual: bool,
    list_empty: bool,
    extension_loading: bool,
) -> bool {
    ((pending_folder || pending_virtual) && list_empty) || extension_loading
}

pub(super) fn list_event_matches_view(
    mail_view: MailView,
    kind: EnvelopeListKind,
    pending_imap_restore: bool,
    view_is_search_backed: bool,
) -> bool {
    match (mail_view, kind) {
        (MailView::UnifiedInbox, EnvelopeListKind::UnifiedInbox) => true,
        (MailView::Search, EnvelopeListKind::Search) => !pending_imap_restore,
        (MailView::Extension(view), EnvelopeListKind::Extension(list)) => view == list,
        (MailView::Extension(_), EnvelopeListKind::Search) => {
            view_is_search_backed && !pending_imap_restore
        }
        (MailView::Folder(id), EnvelopeListKind::Folder(folder_id)) => id == folder_id,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn review_list_shows_pull_icon_until_the_list_arrives() {
        assert!(message_list_loading(false, true, true, false));
        assert!(!message_list_loading(false, true, false, false));
        assert!(message_list_loading(true, false, true, false));
    }

    #[test]
    fn list_event_matches_view_search_backed_extension() {
        let saved = MailView::Extension(alcedo_core::ExtensionListId(Uuid::from_u128(5)));
        assert!(list_event_matches_view(
            saved,
            EnvelopeListKind::Search,
            false,
            true
        ));
        assert!(!list_event_matches_view(
            saved,
            EnvelopeListKind::Search,
            true,
            true
        ));
        assert!(
            !list_event_matches_view(saved, EnvelopeListKind::Search, false, false),
            "a view the extension fills ignores search results"
        );
        assert!(list_event_matches_view(
            MailView::Search,
            EnvelopeListKind::Search,
            false,
            false
        ));
        assert!(!list_event_matches_view(
            saved,
            EnvelopeListKind::UnifiedInbox,
            false,
            true
        ));
        assert!(list_event_matches_view(
            MailView::UnifiedInbox,
            EnvelopeListKind::UnifiedInbox,
            true,
            false
        ));
        let ext = alcedo_core::ExtensionListId(Uuid::from_u128(1));
        let other = alcedo_core::ExtensionListId(Uuid::from_u128(2));
        assert!(list_event_matches_view(
            MailView::Extension(ext),
            EnvelopeListKind::Extension(ext),
            true,
            false
        ));
        assert!(!list_event_matches_view(
            MailView::Extension(ext),
            EnvelopeListKind::Extension(other),
            false,
            false
        ));
    }
}
