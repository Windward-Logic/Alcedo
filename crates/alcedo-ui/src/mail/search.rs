//! Search entry, scope chips and mock search matching.

use super::folder_ops::mock_folder_name_matches;
use super::*;
use alcedo_i18n::tr;

impl MailModel {
    pub(super) fn update_search(&mut self, msg: MailMsg, sender: ComponentSender<MailModel>) {
        match msg {
            MailMsg::Search(query) => {
                let query = query.trim().to_string();
                if query.is_empty() {
                    // A view backed by a saved query goes back to that query.
                    if let MailView::Extension(id) = self.mail_view
                        && self.is_search_backed(self.mail_view)
                    {
                        sender.input(MailMsg::ShowExtensionView(id));
                        return;
                    }
                    if self.mail_view == MailView::Search {
                        let restore = self
                            .pre_search_mail_view
                            .take()
                            .unwrap_or(MailView::UnifiedInbox);
                        self.mail_view = restore;
                        self.sync_column_layout();
                        self.clear_search();
                        match restore {
                            MailView::UnifiedInbox => sender.input(MailMsg::SelectUnifiedInbox),
                            MailView::Folder(folder_id) => {
                                sender.input(MailMsg::SelectFolder(folder_id))
                            }
                            MailView::Search => sender.input(MailMsg::SelectUnifiedInbox),
                            MailView::Extension(id) => sender.input(MailMsg::ShowExtensionView(id)),
                        }
                    }
                    return;
                }

                let on_smart = self.is_search_backed(self.mail_view);
                let leaving_extension_for_search =
                    !on_smart && matches!(self.mail_view, MailView::Extension(_));
                if !on_smart {
                    let entering_search = self.mail_view != MailView::Search;
                    if entering_search {
                        self.pre_search_mail_view = Some(self.mail_view);
                        self.search_scope = SearchScopeKind::Current;
                        if leaving_extension_for_search {
                            self.leave_extension_views(ViewLeave::Search, None);
                        }
                    }
                    self.mail_view = MailView::Search;
                    self.sync_column_layout();
                }

                let parsed = parse_search_query(&query);
                let folder_ids = if on_smart {
                    None
                } else {
                    self.search_folder_ids(&parsed)
                };
                if self.mock {
                    self.envelopes = self
                        .source_envelopes
                        .iter()
                        .filter(|env| {
                            self.mock_envelope_matches_search(env, &parsed, folder_ids.as_deref())
                        })
                        .cloned()
                        .collect();
                    self.envelopes.sort_by(|a, b| b.date.cmp(&a.date));
                    if self.list_mode == MessageListMode::Conversation {
                        self.conversations = group_envelopes(&self.envelopes);
                        self.envelopes = Self::conversation_lookup_envelopes(&self.conversations);
                    } else {
                        self.conversations.clear();
                    }
                    self.refresh_list();
                } else {
                    self.cmd.emit(UiCommand::Search {
                        query: query.clone(),
                        folder_ids,
                        mode: self.list_mode,
                    });
                }
                self.sync_search_chrome(&query, Some(&sender));
                if leaving_extension_for_search {
                    self.sync_extension_chrome();
                }
                self.sync_deleted_column();
                self.rebuild_folder_bar(&sender);
            }
            MailMsg::SearchScope(scope) => {
                self.search_scope = scope;
                let query = self.search_entry.text().to_string();
                if query.trim().is_empty() {
                    return;
                }
                sender.input(MailMsg::Search(query));
            }
            MailMsg::SearchQueryRewrite(query) => {
                self.search_entry.set_text(&query);
                sender.input(MailMsg::Search(query));
            }
            MailMsg::FocusSearch => {
                self.search_entry.grab_focus();
            }
            MailMsg::InsertSearchOperator(token) => {
                let raw = self.search_entry.text().to_string();
                let rewritten = match token {
                    "is:unread" | "is:starred" | "has:attachment" => {
                        apply_is_token(&raw, token, true)
                    }
                    "is:read" => {
                        let without = apply_is_token(&raw, "is:unread", false);
                        if without.is_empty() {
                            "is:read".to_string()
                        } else {
                            format!("{without} is:read")
                        }
                    }
                    "is:unstarred" => {
                        let without = apply_is_token(&raw, "is:starred", false);
                        if without.is_empty() {
                            "is:unstarred".to_string()
                        } else {
                            format!("{without} is:unstarred")
                        }
                    }
                    _ if token.ends_with(':') => {
                        if raw.trim().is_empty() {
                            token.to_string()
                        } else {
                            format!("{raw} {token}")
                        }
                    }
                    _ => {
                        if raw.trim().is_empty() {
                            token.to_string()
                        } else {
                            format!("{raw} {token}")
                        }
                    }
                };
                self.search_entry.set_text(&rewritten);
                self.search_entry.set_position(-1);
                self.search_entry.grab_focus();
                sender.input(MailMsg::Search(rewritten));
            }
            _ => unreachable!("message not routed to search"),
        }
    }

    pub(super) fn clear_search(&mut self) {
        self.search_entry.set_text("");
        self.pre_search_mail_view = None;
        self.search_scope = SearchScopeKind::Current;
        self.search_filter_bar.set_revealed(false);
        reapply_message_list_column_visibility(&self.list_view, false);
        self.sync_deleted_column();
    }

    pub(super) fn message_list_searching_for_columns(&self, query: &str) -> bool {
        matches!(self.mail_view, MailView::Search | MailView::Extension(_))
            && !query.trim().is_empty()
    }

    pub(super) fn sync_search_chrome(
        &mut self,
        query: &str,
        sender: Option<&ComponentSender<MailModel>>,
    ) {
        set_folder_labels(self.folder_label_map());
        set_account_labels(self.account_label_map());
        set_account_chips(self.account_chip_map());
        let searching = self.message_list_searching_for_columns(query);
        // Reapply all picker columns from GSettings; folder stays on during active search.
        reapply_message_list_column_visibility(&self.list_view, searching);
        self.search_filter_bar.set_revealed(searching);
        if searching {
            self.search_filter_bar
                .set_current_label(&self.current_view_scope_label());
            self.search_filter_bar.set_scope(self.search_scope);
            self.search_filter_bar
                .set_folder_operator_active(query_has_folder_operator(query));
            self.search_filter_bar
                .set_scope_locked(self.is_search_backed(self.mail_view));
            self.search_filter_bar.sync_toggles_from_query(query);
            set_last_query_for_toggles(query);
            if let Some(sender) = sender {
                self.sync_save_menu(sender);
            }
        }
    }

    /// Offer the extensions' save actions for the current query.
    pub(super) fn sync_save_menu(&mut self, sender: &ComponentSender<MailModel>) {
        let query = self.search_entry.text().to_string();
        let items = if query.trim().is_empty() {
            Vec::new()
        } else {
            let view = match self.mail_view {
                MailView::Extension(id) => Some(id),
                MailView::Search => None,
                // Saving is offered from a search or an extension view only.
                _ => {
                    self.search_filter_bar.wire_save_menu(&[], |_| {});
                    self.search_save_items.clear();
                    return;
                }
            };
            let ctx = crate::extension::SearchSaveContext {
                view,
                query: query.clone(),
                saved_query: self.query_with_scope_for_saving(),
            };
            crate::extension::registered()
                .iter()
                .flat_map(|extension| extension.search_save_actions(&ctx))
                .collect()
        };
        let labels: Vec<String> = items
            .iter()
            .map(|item: &crate::extension::SearchSaveAction| item.label.clone())
            .collect();
        self.search_filter_bar.wire_save_menu(&labels, {
            let sender = sender.clone();
            move |index| sender.input(MailMsg::SearchSaveAction(index))
        });
        self.search_save_items = items;
    }

    /// The typed query with the chosen search scope written in, so a saved
    /// copy finds the same mail later.
    pub(super) fn query_with_scope_for_saving(&self) -> String {
        let raw = self.search_entry.text().to_string();
        let (scope, folder_imap) = self.scope_bake_for_save();
        ensure_scope_in_query(&raw, scope, folder_imap.as_deref())
    }

    fn scope_bake_for_save(&self) -> (ScopeBake, Option<String>) {
        if self.search_scope == SearchScopeKind::All {
            return (ScopeBake::All, None);
        }
        let view = self.as_inbox(self.pre_search_mail_view.unwrap_or(self.mail_view));
        match view {
            MailView::UnifiedInbox => (ScopeBake::CurrentUnified, None),
            MailView::Folder(id) => {
                let imap = self.folder(id).map(|folder| folder.imap_name.clone());
                (ScopeBake::CurrentFolder, imap)
            }
            MailView::Search | MailView::Extension(_) => (ScopeBake::All, None),
        }
    }

    pub(super) fn search_folder_ids(&self, parsed: &SearchQuery) -> Option<Vec<FolderId>> {
        if parsed.folder.is_some() {
            return None;
        }
        if self.search_scope == SearchScopeKind::All {
            return None;
        }
        let view = self.as_inbox(self.pre_search_mail_view.unwrap_or(self.mail_view));
        match view {
            MailView::UnifiedInbox => {
                let ids = self
                    .folders
                    .values()
                    .flatten()
                    .filter(|folder| folder.special == SpecialUse::Inbox)
                    .map(|folder| folder.id)
                    .collect::<Vec<_>>();
                Some(ids)
            }
            MailView::Folder(id) => Some(vec![id]),
            MailView::Search | MailView::Extension(_) => None,
        }
    }

    pub(super) fn current_view_scope_label(&self) -> String {
        let view = self.pre_search_mail_view.unwrap_or(self.mail_view);
        match view {
            MailView::UnifiedInbox => everything_title(),
            MailView::Folder(id) => self
                .folders
                .values()
                .flatten()
                .find(|folder| folder.id == id)
                .map(folder_display_name)
                .unwrap_or_else(|| "Folder".to_string()),
            MailView::Search => tr!("All mail"),
            MailView::Extension(id) => self
                .extension_view_spec(id)
                .map(|(_, spec)| spec.title)
                .unwrap_or_else(|| "All mail".to_string()),
        }
    }

    pub(super) fn search_status_scope(&self, parsed: &SearchQuery) -> String {
        match &parsed.folder {
            Some(FolderToken::All) => tr!("All mail"),
            Some(FolderToken::Name(name)) => name.clone(),
            None if self.search_scope == SearchScopeKind::All => tr!("All mail"),
            None => self.current_view_scope_label(),
        }
    }

    pub(super) fn mock_envelope_matches_search(
        &self,
        env: &Envelope,
        parsed: &SearchQuery,
        folder_ids: Option<&[FolderId]>,
    ) -> bool {
        if let Some(ids) = folder_ids {
            if ids.is_empty() || !ids.contains(&env.folder_id) {
                return false;
            }
        } else if parsed.folder.is_none() {
            let hidden = self
                .folders
                .values()
                .flatten()
                .find(|folder| folder.id == env.folder_id)
                .is_some_and(|folder| hide_special_in_unscoped_search(&folder.special));
            if hidden {
                return false;
            }
        }
        if let Some(folder_token) = &parsed.folder {
            let folder = self
                .folders
                .values()
                .flatten()
                .find(|folder| folder.id == env.folder_id);
            let Some(folder) = folder else {
                return false;
            };
            match folder_token {
                FolderToken::All => {}
                FolderToken::Name(name) => {
                    if !mock_folder_name_matches(folder, name) {
                        return false;
                    }
                }
            }
        }
        if parsed.has_attachment && !env.has_attachments {
            return false;
        }
        match parsed.unread {
            Some(true) if env.seen => return false,
            Some(false) if !env.seen => return false,
            _ => {}
        }
        match parsed.starred {
            Some(true) if !env.flagged => return false,
            Some(false) if env.flagged => return false,
            _ => {}
        }
        if let Some(after) = &parsed.after {
            let cutoff = chrono::NaiveDate::parse_from_str(after, "%Y-%m-%d")
                .ok()
                .and_then(|date| date.and_hms_opt(0, 0, 0))
                .map(|dt| {
                    chrono::DateTime::<chrono::Utc>::from_naive_utc_and_offset(dt, chrono::Utc)
                });
            if cutoff.is_some_and(|cutoff| env.date < cutoff) {
                return false;
            }
        }
        if let Some(before) = &parsed.before {
            let cutoff = chrono::NaiveDate::parse_from_str(before, "%Y-%m-%d")
                .ok()
                .and_then(|date| date.and_hms_opt(0, 0, 0))
                .map(|dt| {
                    chrono::DateTime::<chrono::Utc>::from_naive_utc_and_offset(dt, chrono::Utc)
                });
            if cutoff.is_some_and(|cutoff| env.date >= cutoff) {
                return false;
            }
        }
        for term in &parsed.from {
            if !address_json_contains(env, term, true) {
                return false;
            }
        }
        for term in &parsed.to {
            if !address_json_contains(env, term, false) {
                return false;
            }
        }
        for term in &parsed.subject {
            if !env
                .display_subject()
                .to_ascii_lowercase()
                .contains(&term.to_ascii_lowercase())
            {
                return false;
            }
        }
        for term in &parsed.free {
            if !mock_free_text_matches(env, term) {
                return false;
            }
        }
        for term in &parsed.exclude {
            if mock_free_text_matches(env, term) {
                return false;
            }
        }
        true
    }
}

pub(super) fn mock_free_text_matches(env: &Envelope, term: &str) -> bool {
    let needle = term.to_ascii_lowercase();
    env.display_subject().to_ascii_lowercase().contains(&needle)
        || env.snippet.to_ascii_lowercase().contains(&needle)
        || env.from.iter().any(|addr| {
            addr.email.to_ascii_lowercase().contains(&needle)
                || addr
                    .name
                    .as_ref()
                    .is_some_and(|name| name.to_ascii_lowercase().contains(&needle))
        })
}

pub(super) fn address_json_contains(env: &Envelope, term: &str, from: bool) -> bool {
    let needle = term.to_ascii_lowercase();
    let addrs = if from { &env.from } else { &env.to };
    addrs.iter().any(|addr| {
        addr.email.to_ascii_lowercase().contains(&needle)
            || addr
                .name
                .as_ref()
                .is_some_and(|name| name.to_ascii_lowercase().contains(&needle))
    })
}

#[cfg(test)]
mod tests {
    use crate::mail::folders::add_unread;

    #[test]
    fn add_unread_never_goes_below_zero() {
        assert_eq!(add_unread(1, -1), 0);
        assert_eq!(add_unread(0, -1), 0);
        assert_eq!(add_unread(2, 1), 3);
    }
}
