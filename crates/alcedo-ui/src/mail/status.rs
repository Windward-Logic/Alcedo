//! Status bar, activity log and sync chrome.

use super::*;

impl MailModel {
    pub(super) fn update_status(&mut self, msg: MailMsg, sender: ComponentSender<MailModel>) {
        match msg {
            MailMsg::RefreshStatusBar => self.refresh_status_bar(),
            MailMsg::RequestSync => {
                self.request_folder_sync(&sender);
            }
            MailMsg::SyncAnimHoldElapsed => {
                self.finish_sync_anim_hold(&sender);
            }
            _ => unreachable!("message not routed to status"),
        }
    }

    pub(super) fn clear_pending_folder_sync(
        &mut self,
        folder_id: FolderId,
        sender: &ComponentSender<MailModel>,
    ) {
        if self.pending_folder_sync == Some(folder_id) {
            self.pending_folder_sync = None;
            self.rebuild_folder_bar(sender);
        }
    }

    pub(super) fn clear_pending_virtual_sync(&mut self, sender: &ComponentSender<MailModel>) {
        if self.pending_virtual_sync == Some(self.mail_view) {
            self.pending_virtual_sync = None;
            self.rebuild_folder_bar(sender);
        }
    }

    pub(super) fn record(
        &self,
        category: u32,
        outcome: ActivityOutcome,
        title: impl Into<String>,
        detail: impl Into<String>,
    ) {
        self.push_activity(category, outcome, title, detail, "", "");
    }

    pub(super) fn push_activity(
        &self,
        category: u32,
        outcome: ActivityOutcome,
        title: impl Into<String>,
        detail: impl Into<String>,
        account: impl Into<String>,
        debug: impl Into<String>,
    ) {
        let mut entry = activity_log::entry(category, outcome, title, detail);
        entry.account = account.into();
        entry.debug = debug.into();
        self.activity.push(entry);
    }

    pub(super) fn set_sync_activity(
        &mut self,
        text: Option<&str>,
        sender: &ComponentSender<MailModel>,
    ) {
        if let Some(text) = text.filter(|text| !text.is_empty()) {
            if text.eq_ignore_ascii_case("IDLE") || text.eq_ignore_ascii_case("Polling") {
                self.last_sync_at = Some(chrono::Utc::now());
            }
            self.sync_text = Some(text.to_string());
        } else if !self.mock {
            self.sync_text = None;
        }
        if crate::status_bar::sync_is_busy(self.sync_text.as_deref()) {
            self.start_sync_anim_hold(sender);
        } else {
            self.clear_context_sync_if_idle(sender);
            self.refresh_status_bar();
            self.rebuild_folder_bar(sender);
        }
    }

    pub(super) fn start_sync_anim_hold(&mut self, sender: &ComponentSender<MailModel>) {
        let started = !self.sync_anim_hold;
        if started {
            self.sync_anim_hold = true;
            self.clear_sync_anim_timeout();
            let timeout_sender = sender.clone();
            self.sync_anim_timeout = Some(glib::timeout_add_local_once(
                crate::status_bar::MIN_SYNC_ANIMATION,
                move || timeout_sender.input(MailMsg::SyncAnimHoldElapsed),
            ));
        }
        self.refresh_status_bar();
        if started {
            self.rebuild_folder_bar(sender);
        }
    }

    pub(super) fn finish_sync_anim_hold(&mut self, sender: &ComponentSender<MailModel>) {
        self.sync_anim_timeout = None;
        self.sync_anim_hold = false;
        self.refresh_status_bar();
        self.sync_selection_chrome();
        self.rebuild_folder_bar(sender);
    }

    pub(super) fn clear_sync_anim_timeout(&mut self) {
        crate::glib_source::cancel_source_option(&mut self.sync_anim_timeout);
    }

    pub(super) fn refresh_status_bar(&self) {
        let sync = if self.mock {
            Some("Mock data")
        } else if self.accounts.is_empty() {
            Some("No accounts")
        } else {
            self.sync_text.as_deref()
        };
        let busy = crate::status_bar::sync_chrome_busy(sync, self.sync_anim_hold);
        let account_email = self.selected_account.and_then(|id| {
            self.accounts
                .iter()
                .find(|account| account.id == id)
                .map(|account| account.email.as_str())
        });
        self.status_bar.set_sync(sync, busy, account_email);
        self.status_bar.set_folder(&self.status_folder());
        self.status_bar.set_stats(self.status_stats());
        self.status_bar.set_selection(self.status_selection());
        self.status_bar
            .set_last_sync(self.last_sync_at, chrono::Utc::now());
        if header_sync_icon_spins(self.mail_view, busy) {
            self.sync_btn.add_css_class("alcedo-sync-spinning");
        } else {
            self.sync_btn.remove_css_class("alcedo-sync-spinning");
        }
        self.mail_actions
            .sync_folder
            .set_enabled(self.can_sync_folder());
        self.sync_file_folder_actions();
    }

    pub(super) fn sync_file_folder_actions(&self) {
        self.mail_actions
            .new_folder
            .set_enabled(self.selected_account.is_some());
        let edit_enabled = matches!(self.mail_view, MailView::Folder(folder_id) if self
            .folder_for_id(folder_id)
            .is_some_and(folder_is_mutable));
        self.mail_actions.edit_folder.set_enabled(edit_enabled);
    }

    pub(super) fn sync_mail_view(&self) -> MailView {
        match self.mail_view {
            MailView::Search => self.pre_search_mail_view.unwrap_or(MailView::Search),
            other => other,
        }
    }

    pub(super) fn syncing_mail_view(&self) -> Option<MailView> {
        if let MailView::Extension(id) = self.mail_view
            && self
                .extension_view_spec(id)
                .is_some_and(|(_, spec)| spec.loading || spec.header_toggle.is_some_and(|t| t.busy))
        {
            return Some(self.mail_view);
        }
        let current = self.sync_mail_view();
        syncing_mail_view_for(
            self.context_sync_target,
            self.pending_folder_sync,
            self.pending_virtual_sync,
            current,
            crate::status_bar::sync_is_busy(self.sync_text.as_deref()),
        )
    }

    pub(super) fn can_sync_folder(&self) -> bool {
        if self.sync_anim_hold {
            return false;
        }
        if self.mock {
            return true;
        }
        if self.accounts.is_empty() {
            return false;
        }
        if crate::status_bar::sync_is_busy(self.sync_text.as_deref()) {
            return false;
        }
        match self.as_inbox(self.sync_mail_view()) {
            MailView::Folder(id) => self
                .folders
                .values()
                .flatten()
                .any(|folder| folder.id == id && folder.selectable),
            MailView::UnifiedInbox => true,
            MailView::Search | MailView::Extension(_) => false,
        }
    }

    pub(super) fn request_folder_sync(&mut self, sender: &ComponentSender<MailModel>) {
        self.request_sync_view(self.sync_mail_view(), sender);
    }

    pub(super) fn request_sync_view(
        &mut self,
        view: MailView,
        sender: &ComponentSender<MailModel>,
    ) {
        if matches!(view, MailView::Search | MailView::Extension(_)) {
            return;
        }
        let is_current = self.sync_mail_view() == view;
        if is_current && !self.can_sync_folder() {
            return;
        }
        if is_current {
            self.start_sync_anim_hold(sender);
        } else {
            self.context_sync_target = Some(view);
            if let MailView::Folder(id) = view {
                self.pending_folder_sync = Some(id);
            }
            self.rebuild_folder_bar(sender);
        }
        if self.mock {
            self.last_sync_at = Some(chrono::Utc::now());
            self.context_sync_target = None;
            if let MailView::Folder(id) = view
                && self.mail_view != MailView::Folder(id)
            {
                self.pending_folder_sync = None;
            }
            self.refresh_status_bar();
            self.rebuild_folder_bar(sender);
            return;
        }
        match self.as_inbox(view) {
            MailView::Folder(id) => self.cmd.emit(UiCommand::SyncFolder(id)),
            MailView::UnifiedInbox => {
                for account in &self.accounts {
                    self.cmd.emit(UiCommand::RefreshAccount(account.id));
                }
            }
            MailView::Search | MailView::Extension(_) => {}
        }
    }

    pub(super) fn clear_context_sync_if_idle(&mut self, sender: &ComponentSender<MailModel>) {
        if self.context_sync_target.is_some()
            && !crate::status_bar::sync_is_busy(self.sync_text.as_deref())
        {
            self.context_sync_target = None;
            if let Some(pending_id) = self.pending_folder_sync
                && self.mail_view != MailView::Folder(pending_id)
            {
                self.pending_folder_sync = None;
            }
            self.rebuild_folder_bar(sender);
        }
    }

    pub(super) fn status_folder(&self) -> StatusFolder {
        match self.mail_view {
            MailView::UnifiedInbox => StatusFolder::UnifiedInbox,
            MailView::Search => {
                let query = self.search_entry.text().to_string();
                let parsed = parse_search_query(&query);
                StatusFolder::Search {
                    scope: self.search_status_scope(&parsed),
                }
            }
            MailView::Extension(id) => match self.extension_view_spec(id) {
                Some((_, spec)) => StatusFolder::Extension {
                    title: spec.title,
                    detail: spec.status_detail,
                },
                None => StatusFolder::None,
            },
            MailView::Folder(id) => {
                let folder = self
                    .folders
                    .values()
                    .flatten()
                    .find(|folder| folder.id == id);
                let name = folder
                    .map(folder_display_name)
                    .unwrap_or_else(|| "Folder".to_string());
                let account = folder.and_then(|folder| {
                    self.accounts
                        .iter()
                        .find(|account| account.id == folder.account_id)
                        .map(|account| account.label().to_string())
                });
                StatusFolder::Folder { name, account }
            }
        }
    }

    pub(super) fn status_stats(&self) -> FolderStats {
        let listed = self.envelopes.len() as u32;
        let listed_unread = self.envelopes.iter().filter(|env| !env.seen).count() as u32;
        let starred = self.envelopes.iter().filter(|env| env.flagged).count() as u32;
        match self.mail_view {
            MailView::Folder(id) => {
                let meta = self
                    .folders
                    .values()
                    .flatten()
                    .find(|folder| folder.id == id);
                match meta {
                    Some(folder) if folder.total > listed => FolderStats {
                        total: folder.total,
                        unread: folder.unread,
                        starred,
                    },
                    _ => FolderStats {
                        total: listed,
                        unread: listed_unread,
                        starred,
                    },
                }
            }
            MailView::UnifiedInbox => FolderStats {
                total: listed,
                unread: self.unified_unread().max(listed_unread),
                starred,
            },
            MailView::Search | MailView::Extension(_) => FolderStats {
                total: listed,
                unread: listed_unread,
                starred,
            },
        }
    }

    pub(super) fn status_selection(&self) -> Option<SelectionInfo> {
        let count = selection_count(&self.list_view);
        if count == 0 {
            return None;
        }
        if count > 1 {
            return Some(SelectionInfo::Count(count));
        }
        let ids = selected_ids(&self.list_view);
        let total = self.envelopes.len() as u32;
        ids.first()
            .and_then(|id| find_visible_position(&self.list_view, *id))
            .map(|pos| SelectionInfo::Position {
                index: pos + 1,
                total,
            })
    }
}

/// Sidebar spinner target. A virtual inbox load (such as Everything)
/// shows on that row until its list arrives, unless a folder sync is pending.
pub(super) fn syncing_mail_view_for(
    context_sync_target: Option<MailView>,
    pending_folder_sync: Option<FolderId>,
    pending_virtual_sync: Option<MailView>,
    current: MailView,
    sync_busy: bool,
) -> Option<MailView> {
    if let Some(target) = context_sync_target {
        return Some(target);
    }
    if let Some(id) = pending_folder_sync {
        return Some(MailView::Folder(id));
    }
    if let Some(view) = pending_virtual_sync.filter(|view| *view == current) {
        return Some(view);
    }
    if sync_busy && current_view_uses_account_sync_spinner(current) {
        Some(current)
    } else {
        None
    }
}

/// The list-header refresh icon. Extension views report their own progress,
/// so account sync must not leave that refresh icon spinning for the view.
pub(super) fn header_sync_icon_spins(view: MailView, busy: bool) -> bool {
    busy && !matches!(view, MailView::Extension(_))
}

fn current_view_uses_account_sync_spinner(view: MailView) -> bool {
    !matches!(view, MailView::Extension(_))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unified_row_shows_pull_spinner_while_its_list_loads() {
        assert_eq!(
            syncing_mail_view_for(
                None,
                None,
                Some(MailView::UnifiedInbox),
                MailView::UnifiedInbox,
                false
            ),
            Some(MailView::UnifiedInbox)
        );
        assert_eq!(
            syncing_mail_view_for(
                None,
                Some(FolderId(4)),
                Some(MailView::UnifiedInbox),
                MailView::Folder(FolderId(4)),
                false,
            ),
            Some(MailView::Folder(FolderId(4)))
        );
    }

    #[test]
    fn extension_row_ignores_account_sync() {
        let ext = MailView::Extension(alcedo_core::ExtensionListId(Uuid::from_u128(3)));
        assert_eq!(syncing_mail_view_for(None, None, None, ext, true), None);
        assert_eq!(
            syncing_mail_view_for(None, None, None, MailView::UnifiedInbox, true),
            Some(MailView::UnifiedInbox)
        );
        assert!(!header_sync_icon_spins(ext, true));
        assert!(header_sync_icon_spins(MailView::Folder(FolderId(1)), true));
        assert!(!header_sync_icon_spins(
            MailView::Folder(FolderId(1)),
            false
        ));
    }
}
