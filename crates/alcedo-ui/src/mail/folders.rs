//! Folder pane: selection, tree, badges and the folder context menu.

use super::window_actions::can_drop_messages_on_folder_data;
use super::*;

impl MailModel {
    pub(super) fn update_folders(&mut self, msg: MailMsg, sender: ComponentSender<MailModel>) {
        match msg {
            MailMsg::SelectFolder(folder_id) => {
                self.clear_search();
                self.pending_account_folder_select = None;
                if self.mail_view == MailView::Folder(folder_id) {
                    return;
                }
                self.pending_virtual_sync = None;
                self.leave_extension_views(ViewLeave::OtherList, None);
                self.mail_view = MailView::Folder(folder_id);
                self.sync_column_layout();
                if let Some(account_id) = self.folder(folder_id).and_then(|folder| {
                    (self.selected_account != Some(folder.account_id)).then_some(folder.account_id)
                }) {
                    self.make_account_current(account_id, &sender);
                }
                self.persist_current_folder();
                self.reset_list_selection();
                self.sync_deleted_column();
                self.sync_extension_chrome();
                if self.mock {
                    self.apply_mock_list();
                } else {
                    self.pending_folder_sync = Some(folder_id);
                    self.start_sync_anim_hold(&sender);
                    self.envelopes.clear();
                    self.conversations.clear();
                    self.refresh_list();
                    self.cmd.emit(UiCommand::SelectFolder {
                        folder: folder_id,
                        mode: self.list_mode,
                        sync: true,
                    });
                }

                self.rebuild_folder_bar(&sender);
            }
            MailMsg::ToggleFolderExpanded { account, imap_name } => {
                self.toggle_folder_expanded(account, imap_name, &sender);
            }
            MailMsg::TogglePersonalFolders(account_id) => {
                self.toggle_personal_folders(account_id, &sender);
            }
            MailMsg::ToggleExtensionSection(title) => {
                for extension in crate::extension::registered() {
                    extension.section_toggled(&title, &self.ext_ctx);
                }
            }
            MailMsg::ReorderExtensionViews { dragged, target } => {
                if let Some((extension, _)) = self.extension_view_spec(dragged) {
                    extension.views_reordered(dragged, target, &self.ext_ctx);
                }
            }
            MailMsg::ExtensionViewMenuItem(index) => {
                let item = usize::try_from(index)
                    .ok()
                    .and_then(|index| self.view_menu_items.get(index));
                if let Some(item) = item {
                    (item.activate)(&self.ext_ctx);
                }
            }
            MailMsg::RefreshFolderBadges => {
                self.rebuild_folder_bar(&sender);
                self.refresh_unread_badges(&sender);
                self.refresh_status_bar();
            }
            MailMsg::FolderContextMenu { view, x, y } => {
                self.popup_folder_context_menu(FolderMenuTarget::View(view), x, y);
            }
            MailMsg::FolderPaneContextMenu { x, y } => {
                self.popup_folder_context_menu(FolderMenuTarget::AccountRoot, x, y);
            }
            MailMsg::FolderMenuClosed => {
                // Leave folder_menu_target set: popdown can queue before activate.
                // Disable actions so a dismissed menu cannot fire with a stale target.
                self.disable_folder_context_actions();
            }
            MailMsg::FolderContextSync => {
                // take(): popdown may queue after activate; do not clear target on closed.
                let Some(FolderMenuTarget::View(view)) = self.folder_menu_target.take() else {
                    return;
                };
                self.request_sync_view(view, &sender);
            }
            MailMsg::FolderContextMarkRead => {
                let Some(FolderMenuTarget::View(view)) = self.folder_menu_target.take() else {
                    return;
                };
                self.mark_folder_read(view, &sender);
            }
            MailMsg::FolderContextSearch => {
                let Some(FolderMenuTarget::View(view)) = self.folder_menu_target.take() else {
                    return;
                };
                match view {
                    MailView::UnifiedInbox if self.mail_view != MailView::UnifiedInbox => {
                        sender.input(MailMsg::SelectUnifiedInbox);
                    }
                    MailView::Folder(folder_id)
                        if self.mail_view != MailView::Folder(folder_id) =>
                    {
                        sender.input(MailMsg::SelectFolder(folder_id));
                    }
                    _ => {}
                }
                self.search_scope = SearchScopeKind::Current;
                self.search_filter_bar.set_scope(SearchScopeKind::Current);
                sender.input(MailMsg::FocusSearch);
            }
            MailMsg::FolderContextToggleExpand => {
                let Some(FolderMenuTarget::View(MailView::Folder(folder_id))) =
                    self.folder_menu_target.take()
                else {
                    return;
                };
                let Some(account_id) = self
                    .folder_for_id(folder_id)
                    .map(|folder| folder.account_id)
                else {
                    return;
                };
                let folders = self.folders.get(&account_id).cloned().unwrap_or_default();
                let collapsed = self.collapsed_for_display(account_id, &folders);
                let Some(entry) = build_folder_tree(&folders, &collapsed)
                    .into_iter()
                    .find(|entry| entry.folder.id == folder_id)
                else {
                    return;
                };
                if entry.has_children {
                    sender.input(MailMsg::ToggleFolderExpanded {
                        account: account_id,
                        imap_name: entry.folder.imap_name,
                    });
                }
            }
            _ => unreachable!("message not routed to folders"),
        }
    }

    pub(super) fn folder(&self, folder_id: FolderId) -> Option<&Folder> {
        self.folders
            .values()
            .flatten()
            .find(|folder| folder.id == folder_id)
    }

    pub(super) fn persist_selected_account(&self) {
        set_last_account(self.selected_account);
    }

    pub(super) fn persist_current_folder(&mut self) {
        self.pending_restore = None;
        self.persist_selected_account();
        match self.mail_view {
            MailView::Search => {}
            MailView::Extension(id) => {
                let restorable = self
                    .extension_view_spec(id)
                    .is_some_and(|(_, spec)| spec.restore_on_start);
                if restorable {
                    set_last_folder(&LastFolder::Extension { id });
                }
            }
            MailView::UnifiedInbox => set_last_folder(&LastFolder::UnifiedInbox),
            MailView::Folder(folder_id) => {
                if let Some(folder) = self.folder(folder_id) {
                    set_last_folder(&LastFolder::Mailbox {
                        account: folder.account_id,
                        imap_name: folder.imap_name.clone(),
                    });
                }
            }
        }
    }

    pub(super) fn folder_belongs_to_account(
        &self,
        folder_id: FolderId,
        account_id: AccountId,
    ) -> bool {
        self.folders
            .values()
            .flatten()
            .any(|folder| folder.id == folder_id && folder.account_id == account_id)
    }

    pub(super) fn unified_unread(&self) -> u32 {
        self.unread_by_account
            .values()
            .map(|(inbox, _)| inbox)
            .sum()
    }

    pub(super) fn folder_exists(&self, folder_id: FolderId) -> bool {
        self.folders
            .values()
            .flatten()
            .any(|folder| folder.id == folder_id)
    }

    pub(super) fn folder_for_id(&self, folder_id: FolderId) -> Option<&Folder> {
        self.folders
            .values()
            .flatten()
            .find(|folder| folder.id == folder_id)
    }

    pub(super) fn special_folder_for(
        &self,
        account_id: AccountId,
        special: &SpecialUse,
    ) -> Option<FolderId> {
        special_folder_id(&self.folders, account_id, special)
    }

    pub(super) fn trash_folder_for(&self, account_id: AccountId) -> Option<FolderId> {
        self.special_folder_for(account_id, &SpecialUse::Trash)
    }

    pub(super) fn folder_is_special(&self, folder_id: FolderId, special: &SpecialUse) -> bool {
        self.folders
            .values()
            .flatten()
            .any(|folder| folder.id == folder_id && &folder.special == special)
    }

    pub(super) fn folder_is_trash(&self, folder_id: FolderId) -> bool {
        self.folder_is_special(folder_id, &SpecialUse::Trash)
    }

    pub(super) fn folder_is_sent(&self, folder_id: FolderId) -> bool {
        self.folders
            .values()
            .flatten()
            .any(|folder| folder.id == folder_id && matches!(folder.special, SpecialUse::Sent))
    }

    pub(super) fn emit_unified_unread(&self, sender: &ComponentSender<MailModel>) {
        let count = self.unified_unread();
        let _ = sender.output(MailOutput::UnifiedUnread(count));
    }

    pub(super) fn popup_folder_context_menu(&mut self, target: FolderMenuTarget, x: f64, y: f64) {
        let spec = match target {
            FolderMenuTarget::AccountRoot => FolderMenuSpec {
                kind: FolderMenuKind::AccountRoot,
                special: SpecialUse::Other,
                unread: 0,
                total: 0,
                expand: None,
                display_name: String::new(),
            },
            FolderMenuTarget::Account(account_id) => FolderMenuSpec {
                kind: if self.hidden_accounts.contains(&account_id) {
                    FolderMenuKind::HiddenAccountHeader
                } else {
                    FolderMenuKind::AccountHeader
                },
                special: SpecialUse::Other,
                unread: 0,
                total: 0,
                expand: None,
                display_name: String::new(),
            },
            FolderMenuTarget::View(MailView::Extension(id)) => {
                self.popup_extension_view_menu(id, x, y);
                return;
            }
            FolderMenuTarget::View(view) => {
                if matches!(view, MailView::Search) {
                    return;
                }
                let account = match view {
                    MailView::Folder(folder_id) => self
                        .folder_for_id(folder_id)
                        .map(|folder| folder.account_id),
                    _ => self.selected_account,
                };
                let folders = account
                    .and_then(|id| self.folders.get(&id))
                    .cloned()
                    .unwrap_or_default();
                let collapsed = account
                    .map(|id| self.collapsed_for_display(id, &folders))
                    .unwrap_or_default();
                let Some(spec) = self.folder_menu_spec(view, &folders, &collapsed) else {
                    return;
                };
                spec
            }
        };
        if folder_menu_entries(&spec).is_empty() {
            return;
        }
        let view = match target {
            FolderMenuTarget::AccountRoot | FolderMenuTarget::Account(_) => MailView::Search,
            FolderMenuTarget::View(view) => view,
        };
        self.set_folder_context_action_enabled(&spec, view);
        self.folder_menu_target = Some(target);
        fill_folder_context_popover(&self.folder_context_menu, &spec);
        self.show_folder_context_popover_at(x, y);
    }

    /// Context menu for an extension view row, built from its extension.
    fn popup_extension_view_menu(&mut self, id: ExtensionListId, x: f64, y: f64) {
        let Some((extension, _)) = self.extension_view_spec(id) else {
            return;
        };
        let items = extension.view_menu(id);
        if items.is_empty() {
            return;
        }
        self.disable_folder_context_actions();
        self.mail_actions.ext_view_item.set_enabled(true);
        let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
        for (index, item) in items.iter().enumerate() {
            let button = folder_menu_button(&self.folder_context_menu, &item.label, None, None);
            button.set_detailed_action_name(&format!("win.ext-view-item({index})"));
            if let Some(label) = button.child().and_downcast::<gtk::Label>() {
                label.set_use_underline(true);
            }
            list.append(&button);
        }
        self.folder_context_menu.set_child(Some(&list));
        self.view_menu_items = items;
        self.folder_menu_target = None;
        self.show_folder_context_popover_at(x, y);
    }

    fn show_folder_context_popover_at(&self, x: f64, y: f64) {
        let rect = self
            .toasts
            .root()
            .and_then(|root| {
                root.upcast_ref::<gtk::Widget>()
                    .compute_point(&self.toasts, &gtk::graphene::Point::new(x as f32, y as f32))
            })
            .map(|point| gtk::gdk::Rectangle::new(point.x() as i32, point.y() as i32, 1, 1))
            .unwrap_or_else(|| gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1));
        self.folder_context_menu.set_pointing_to(Some(&rect));
        self.folder_context_menu.popup();
    }

    pub(super) fn folder_menu_spec(
        &self,
        view: MailView,
        folders: &[Folder],
        collapsed: &HashSet<String>,
    ) -> Option<FolderMenuSpec> {
        match self.as_inbox(view) {
            MailView::Search | MailView::Extension(_) => None,
            MailView::UnifiedInbox => Some(FolderMenuSpec {
                kind: FolderMenuKind::Unified,
                special: SpecialUse::Inbox,
                unread: self.unified_unread(),
                total: 0,
                expand: None,
                display_name: String::new(),
            }),
            MailView::Folder(id) => {
                let folder = folders.iter().find(|folder| folder.id == id)?;
                let entry = build_folder_tree(folders, collapsed)
                    .into_iter()
                    .find(|entry| entry.folder.id == id)?;
                let kind = if folder.selectable {
                    FolderMenuKind::Selectable
                } else {
                    FolderMenuKind::Inert
                };
                Some(FolderMenuSpec {
                    kind,
                    special: folder.special.clone(),
                    unread: folder.unread,
                    total: folder.total,
                    expand: entry.has_children.then_some(entry.is_expanded),
                    display_name: folder_display_name(folder),
                })
            }
        }
    }

    pub(super) fn disable_folder_context_actions(&self) {
        self.mail_actions
            .folder_ctx_account_settings
            .set_enabled(false);
        self.mail_actions
            .folder_ctx_toggle_account_hidden
            .set_enabled(false);
        self.mail_actions.folder_ctx_new.set_enabled(false);
        self.mail_actions.folder_ctx_rename.set_enabled(false);
        self.mail_actions.folder_ctx_delete.set_enabled(false);
        self.mail_actions.folder_ctx_sync.set_enabled(false);
        self.mail_actions.folder_ctx_mark_read.set_enabled(false);
        self.mail_actions.folder_ctx_search.set_enabled(false);
        self.mail_actions
            .folder_ctx_toggle_expand
            .set_enabled(false);
        self.mail_actions.folder_ctx_empty.set_enabled(false);
        self.mail_actions
            .folder_ctx_delete_on_or_before
            .set_enabled(false);
        self.mail_actions.ext_view_item.set_enabled(false);
    }

    pub(super) fn set_folder_context_action_enabled(&self, spec: &FolderMenuSpec, view: MailView) {
        if matches!(
            spec.kind,
            FolderMenuKind::AccountHeader | FolderMenuKind::HiddenAccountHeader
        ) {
            self.disable_folder_context_actions();
            self.mail_actions.folder_ctx_new.set_enabled(true);
            self.mail_actions
                .folder_ctx_account_settings
                .set_enabled(true);
            self.mail_actions
                .folder_ctx_toggle_account_hidden
                .set_enabled(true);
            return;
        }
        if spec.kind == FolderMenuKind::AccountRoot {
            self.disable_folder_context_actions();
            self.mail_actions
                .folder_ctx_new
                .set_enabled(self.selected_account.is_some());
            return;
        }
        let account_selected = self.selected_account.is_some();
        self.mail_actions
            .folder_ctx_new
            .set_enabled(account_selected);
        let mutable_target = matches!(view, MailView::Folder(folder_id) if self
            .folder_for_id(folder_id)
            .is_some_and(folder_is_mutable));
        self.mail_actions
            .folder_ctx_rename
            .set_enabled(mutable_target);
        self.mail_actions
            .folder_ctx_delete
            .set_enabled(mutable_target);
        let sync_enabled = matches!(
            spec.kind,
            FolderMenuKind::Unified | FolderMenuKind::Selectable
        ) && self.syncing_mail_view() != Some(view);
        self.mail_actions.folder_ctx_sync.set_enabled(sync_enabled);
        self.mail_actions
            .folder_ctx_mark_read
            .set_enabled(spec.unread > 0);
        self.mail_actions.folder_ctx_search.set_enabled(matches!(
            spec.kind,
            FolderMenuKind::Unified | FolderMenuKind::Selectable
        ));
        self.mail_actions
            .folder_ctx_toggle_expand
            .set_enabled(spec.expand.is_some());
        let delete_on_or_before_blocked = matches!(view, MailView::Folder(folder_id) if {
            self.pending_delete_folder == Some(folder_id)
                || self.context_sync_target == Some(MailView::Folder(folder_id))
        });
        self.mail_actions
            .folder_ctx_empty
            .set_enabled(spec.kind == FolderMenuKind::Selectable && !delete_on_or_before_blocked);
        self.mail_actions
            .folder_ctx_delete_on_or_before
            .set_enabled(spec.kind == FolderMenuKind::Selectable && !delete_on_or_before_blocked);
    }

    pub(super) fn inbox_folder_ids(&self) -> Vec<FolderId> {
        self.folders
            .values()
            .flatten()
            .filter(|folder| folder.selectable && matches!(folder.special, SpecialUse::Inbox))
            .map(|folder| folder.id)
            .collect()
    }

    pub(super) fn collapsed_for_display(
        &self,
        account_id: AccountId,
        folders: &[Folder],
    ) -> HashSet<String> {
        let mut collapsed = self
            .collapsed_folders
            .get(&account_id)
            .cloned()
            .unwrap_or_default();
        let MailView::Folder(id) = self.mail_view else {
            return collapsed;
        };
        let Some(current) = folders.iter().find(|folder| folder.id == id) else {
            return collapsed;
        };
        let known_names: HashSet<String> = folders.iter().map(|f| f.imap_name.clone()).collect();
        for ancestor in imap_ancestor_names(&current.imap_name, &current.delimiter, &known_names) {
            collapsed.remove(&ancestor);
        }
        collapsed
    }

    pub(super) fn toggle_folder_expanded(
        &mut self,
        account_id: AccountId,
        imap_name: String,
        sender: &ComponentSender<MailModel>,
    ) {
        let folders = self.folders.get(&account_id).cloned().unwrap_or_default();
        let already_collapsed = self
            .collapsed_folders
            .get(&account_id)
            .is_some_and(|set| set.contains(&imap_name));
        if already_collapsed {
            if let Some(collapsed) = self.collapsed_folders.get_mut(&account_id) {
                collapsed.remove(&imap_name);
            }
            set_collapsed_folders(&self.collapsed_folders);
            self.rebuild_folder_bar(sender);
            return;
        }

        let hides_current = match self.mail_view {
            MailView::Folder(id) => folders.iter().any(|folder| {
                folder.id == id && folder_is_under(&folder.imap_name, &imap_name, &folder.delimiter)
            }),
            _ => false,
        };
        if hides_current {
            match folders.iter().find(|folder| folder.imap_name == imap_name) {
                Some(parent) if parent.selectable => {
                    self.collapsed_folders
                        .entry(account_id)
                        .or_default()
                        .insert(imap_name);
                    set_collapsed_folders(&self.collapsed_folders);
                    sender.input(MailMsg::SelectFolder(parent.id));
                    return;
                }
                _ => {
                    // Non-selectable namespace folders stay expanded so the
                    // active child row remains visible.
                    self.rebuild_folder_bar(sender);
                    return;
                }
            }
        }

        self.collapsed_folders
            .entry(account_id)
            .or_default()
            .insert(imap_name);
        set_collapsed_folders(&self.collapsed_folders);
        self.rebuild_folder_bar(sender);
    }

    pub(super) fn toggle_personal_folders(
        &mut self,
        account_id: AccountId,
        sender: &ComponentSender<MailModel>,
    ) {
        if !self.collapsed_personal_folders.remove(&account_id) {
            self.collapsed_personal_folders.insert(account_id);
        }
        set_collapsed_personal_folders(&self.collapsed_personal_folders);
        self.rebuild_folder_bar(sender);
    }

    pub(super) fn rebuild_folder_bar(&self, sender: &ComponentSender<MailModel>) {
        *self.search_complete_folders.borrow_mut() = self.command_palette_spec().folders;
        let account_folders = self
            .selected_account
            .and_then(|account_id| self.folders.get(&account_id))
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let account_block = if self.stacked_accounts_active() {
            crate::folder_bar_accounts::AccountBlock::Stacked {
                sections: self.account_sections(),
                hidden: self.hidden_account_sections(),
                actions: self.account_header_actions(sender),
            }
        } else {
            crate::folder_bar_accounts::AccountBlock::Single {
                folders: account_folders,
                waiting_for_folders: self.selected_account.is_some() && account_folders.is_empty(),
                collapsed: self
                    .selected_account
                    .map(|account_id| self.collapsed_for_display(account_id, account_folders))
                    .unwrap_or_default(),
                personal_expanded: self.selected_account.is_none_or(|account_id| {
                    !self.collapsed_personal_folders.contains(&account_id)
                }),
            }
        };
        let sender_extension = sender.clone();
        let sender_everything = sender.clone();
        let sender_folder = sender.clone();
        let sender_toggle = sender.clone();
        let sender_context = sender.clone();
        let sender_drop = sender.clone();
        let selection_model = self.list_view.selection_model.clone();
        let all_folders = self.folders.clone();
        let envelopes = self.envelopes.clone();
        let source_envelopes = self.source_envelopes.clone();

        let sender_reorder = sender.clone();

        populate_folder_bar(
            &self.folder_lists,
            account_block,
            self.mail_view,
            self.unified_unread(),
            &self.extension_views(),
            self.syncing_mail_view(),
            move || sender_everything.input(MailMsg::SelectUnifiedInbox),
            move |id| sender_extension.input(MailMsg::ShowExtensionView(id)),
            move |dragged, target| {
                sender_reorder.input(MailMsg::ReorderExtensionViews { dragged, target })
            },
            move |folder_id| sender_folder.input(MailMsg::SelectFolder(folder_id)),
            move |account, imap_name| {
                sender_toggle.input(MailMsg::ToggleFolderExpanded { account, imap_name })
            },
            {
                let sender_personal = sender.clone();
                move |account| sender_personal.input(MailMsg::TogglePersonalFolders(account))
            },
            {
                let sender_section = sender.clone();
                move |title| sender_section.input(MailMsg::ToggleExtensionSection(title))
            },
            move |view, x, y| sender_context.input(MailMsg::FolderContextMenu { view, x, y }),
            {
                let all_folders = all_folders.clone();
                let envelopes = envelopes.clone();
                let source_envelopes = source_envelopes.clone();
                move |dest, ids| {
                    let ids_to_check = if ids.is_empty() {
                        selected_ids_from_selection(&selection_model)
                    } else {
                        ids.to_vec()
                    };
                    if ids_to_check.is_empty() {
                        return false;
                    }
                    can_drop_messages_on_folder_data(
                        &all_folders,
                        &envelopes,
                        &source_envelopes,
                        dest,
                        &ids_to_check,
                    )
                }
            },
            move |dest, ids, copy| {
                sender_drop.input(MailMsg::DropMessagesOnFolder { dest, ids, copy });
            },
        );
    }

    pub(super) fn refresh_unread_badges(&self, sender: &ComponentSender<MailModel>) {
        self.emit_unified_unread(sender);
        set_switcher_unread(
            &self.account_switcher,
            self.selected_account,
            &self.unread_by_account,
        );
    }
}

pub(super) fn fill_folder_context_popover(popover: &gtk::Popover, spec: &FolderMenuSpec) {
    let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
    for (index, section) in folder_menu_entries(spec).iter().enumerate() {
        if index > 0 {
            list.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        }
        for (label_text, action) in section {
            list.append(&folder_menu_button(
                popover,
                label_text,
                Some(*action),
                None,
            ));
        }
    }
    popover.set_child(Some(&list));
}

pub(super) fn folder_menu_button(
    popover: &gtk::Popover,
    label_text: &str,
    action: Option<&str>,
    widget_name: Option<&str>,
) -> gtk::Button {
    let label = gtk::Label::new(Some(label_text));
    label.set_use_underline(action.is_some());
    label.set_halign(gtk::Align::Start);
    label.set_hexpand(true);
    let btn = gtk::Button::new();
    btn.set_child(Some(&label));
    btn.set_has_frame(false);
    btn.add_css_class("flat");
    btn.set_hexpand(true);
    if let Some(action) = action {
        btn.set_action_name(Some(action));
    }
    if let Some(name) = widget_name {
        btn.set_widget_name(name);
    }
    let popover = popover.clone();
    btn.connect_clicked(move |_| popover.popdown());
    btn
}

pub(super) fn add_unread(count: u32, delta: i32) -> u32 {
    if delta < 0 {
        count.saturating_sub(delta.unsigned_abs())
    } else {
        count.saturating_add(delta as u32)
    }
}

pub(super) fn special_folder_id(
    folders: &HashMap<AccountId, Vec<Folder>>,
    account_id: AccountId,
    special: &SpecialUse,
) -> Option<FolderId> {
    folders.get(&account_id).and_then(|folders| {
        folders
            .iter()
            .find(|folder| folder.selectable && &folder.special == special)
            .map(|folder| folder.id)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mail::test_support::sample_folder;

    #[test]
    fn special_folder_id_finds_junk() {
        let account_id = AccountId(Uuid::from_u128(1));
        let mut folders = HashMap::new();
        folders.insert(
            account_id,
            vec![
                sample_folder(1, SpecialUse::Inbox, true),
                sample_folder(2, SpecialUse::Junk, true),
            ],
        );
        assert_eq!(
            special_folder_id(&folders, account_id, &SpecialUse::Junk),
            Some(FolderId(2))
        );
    }

    #[test]
    fn special_folder_id_returns_none_when_absent() {
        let account_id = AccountId(Uuid::from_u128(1));
        let mut folders = HashMap::new();
        folders.insert(account_id, vec![sample_folder(1, SpecialUse::Inbox, true)]);
        assert_eq!(
            special_folder_id(&folders, account_id, &SpecialUse::Junk),
            None
        );
    }

    #[test]
    fn special_folder_id_skips_non_selectable() {
        let account_id = AccountId(Uuid::from_u128(1));
        let mut folders = HashMap::new();
        folders.insert(
            account_id,
            vec![
                sample_folder(1, SpecialUse::Inbox, true),
                sample_folder(2, SpecialUse::Junk, false),
                sample_folder(3, SpecialUse::Junk, true),
            ],
        );
        assert_eq!(
            special_folder_id(&folders, account_id, &SpecialUse::Junk),
            Some(FolderId(3))
        );
    }
}
