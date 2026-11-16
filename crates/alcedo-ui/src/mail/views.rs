//! Virtual views: the unified inbox and mock list filtering.

use super::*;

impl MailModel {
    pub(super) fn update_views(&mut self, msg: MailMsg, sender: ComponentSender<MailModel>) {
        match msg {
            MailMsg::SelectUnifiedInbox => {
                self.select_virtual_inbox(MailView::UnifiedInbox, &sender);
            }
            _ => unreachable!("message not routed to views"),
        }
    }

    pub(super) fn filter_mock_virtual_inbox_unscoped(&self) -> Vec<Envelope> {
        self.source_envelopes
            .iter()
            .filter(|env| {
                !self.folder_is_special(env.folder_id, &SpecialUse::Trash)
                    && !self.folder_is_special(env.folder_id, &SpecialUse::Junk)
            })
            .cloned()
            .collect()
    }

    pub(super) fn select_virtual_inbox(
        &mut self,
        view: MailView,
        sender: &ComponentSender<MailModel>,
    ) {
        self.clear_search();
        self.pending_account_folder_select = None;
        self.pending_folder_sync = None;
        self.leave_extension_views(ViewLeave::OtherList);
        let already = self.mail_view == view;
        self.mail_view = view;
        self.sync_column_layout();
        if !already {
            self.pending_virtual_sync = if self.mock { None } else { Some(view) };
            self.reset_list_selection();
            self.conversations.clear();
            self.envelopes.clear();
            self.refresh_list();
        }
        self.persist_current_folder();
        self.sync_deleted_column();
        self.sync_extension_chrome();
        if self.mock {
            self.apply_mock_list();
        } else {
            self.start_sync_anim_hold(sender);
            if view == MailView::UnifiedInbox {
                self.cmd.emit(UiCommand::SelectUnifiedInbox {
                    mode: self.list_mode,
                });
            }
        }
        if !already {
            self.rebuild_folder_bar(sender);
        }
        self.refresh_status_bar();
        self.notify_reader_selection();
    }

    pub(super) fn mock_filter_envelopes(&self) -> Vec<Envelope> {
        let envelopes = match self.mail_view {
            MailView::Search => {
                let query = self.search_entry.text().to_string();
                let parsed = parse_search_query(&query);
                self.source_envelopes
                    .iter()
                    .filter(|env| self.mock_envelope_matches_search(env, &parsed, None))
                    .cloned()
                    .collect()
            }
            MailView::UnifiedInbox => self.filter_mock_virtual_inbox_unscoped(),
            MailView::Folder(folder_id) => self
                .source_envelopes
                .iter()
                .filter(|env| env.folder_id == folder_id)
                .cloned()
                .collect(),
            // Extension views have no mock data.
            MailView::Extension(_) => Vec::new(),
        };
        let mut envelopes = envelopes;
        if matches!(self.mail_view, MailView::Folder(folder_id) if self.folder_is_trash(folder_id))
        {
            envelopes.sort_by(|a, b| {
                b.deleted_at
                    .cmp(&a.deleted_at)
                    .then_with(|| b.date.cmp(&a.date))
            });
        } else {
            envelopes.sort_by(|a, b| b.date.cmp(&a.date));
        }
        envelopes
    }

    pub(super) fn apply_mock_list(&mut self) {
        let flat = self.mock_filter_envelopes();
        if self.list_uses_conversations() {
            self.conversations = group_envelopes(&flat);
            self.envelopes = Self::conversation_lookup_envelopes(&self.conversations);
        } else {
            self.conversations.clear();
            self.envelopes = flat;
        }
        self.refresh_list();
    }
}
