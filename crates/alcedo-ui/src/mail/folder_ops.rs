//! Folder mutations: create, rename, delete, empty, mark read and delete-on-or-before.

use super::*;
use crate::calendar_day_select::{calendar_naive_date, select_calendar_day};
use alcedo_i18n::{ntr, tr, trc, trf, trs};

impl MailModel {
    pub(super) fn update_folder_ops(&mut self, msg: MailMsg, sender: ComponentSender<MailModel>) {
        match msg {
            MailMsg::FolderContextEmpty => {
                let Some(FolderMenuTarget::View(MailView::Folder(folder_id))) =
                    self.folder_menu_target.take()
                else {
                    return;
                };
                let folders = self
                    .selected_account
                    .and_then(|account_id| self.folders.get(&account_id))
                    .cloned()
                    .unwrap_or_default();
                let Some(folder) = folders
                    .iter()
                    .find(|folder| folder.id == folder_id && folder.selectable)
                else {
                    return;
                };
                let display = folder_display_name(folder);
                let trash_display = folders
                    .iter()
                    .find(|folder| folder.selectable && matches!(folder.special, SpecialUse::Trash))
                    .map(folder_display_name);
                let heading = trf!("Empty {folder}?", folder = display);
                // TRANSLATORS: button; {folder} is a folder name such as "Trash".
                let button = trf!("Empty {folder}", folder = display);
                let body = if matches!(folder.special, SpecialUse::Trash | SpecialUse::Junk) {
                    trf!(
                        "All messages and folders in {folder} will be permanently deleted.",
                        folder = display
                    )
                } else if let Some(trash_display) = trash_display {
                    trf!(
                        "All messages in {folder} will be moved to {trash}, the same as deleting them in the list.",
                        folder = display,
                        trash = trash_display
                    )
                } else {
                    trf!(
                        "All messages in {folder} will be permanently deleted.",
                        folder = display
                    )
                };
                let confirm = adw::AlertDialog::builder()
                    .heading(heading)
                    .body(body)
                    .build();
                confirm.add_response("cancel", &tr!("Cancel"));
                confirm.add_response("empty", &button);
                confirm.set_response_appearance("empty", adw::ResponseAppearance::Destructive);
                confirm.set_close_response("cancel");
                let sender = sender.clone();
                confirm.choose(
                    Some(&self.window),
                    None::<&gtk::gio::Cancellable>,
                    move |response| {
                        if response.as_str() == "empty" {
                            sender.input(MailMsg::ConfirmEmptyFolder(folder_id));
                        }
                    },
                );
            }
            MailMsg::FolderContextDeleteOnOrBefore => {
                let Some(FolderMenuTarget::View(MailView::Folder(folder_id))) =
                    self.folder_menu_target.take()
                else {
                    return;
                };
                let folders = self
                    .selected_account
                    .and_then(|account_id| self.folders.get(&account_id))
                    .cloned()
                    .unwrap_or_default();
                let Some(folder) = folders
                    .iter()
                    .find(|folder| folder.id == folder_id && folder.selectable)
                else {
                    return;
                };
                let trash_display = folders
                    .iter()
                    .find(|folder| folder.selectable && matches!(folder.special, SpecialUse::Trash))
                    .map(folder_display_name);
                prompt_delete_folder_on_or_before(
                    &self.window,
                    folder,
                    trash_display.as_deref(),
                    sender,
                );
            }
            MailMsg::FolderContextNew => {
                let Some(target) = self.folder_menu_target.take() else {
                    return;
                };
                let account_id = match target {
                    FolderMenuTarget::Account(account_id) => Some(account_id),
                    FolderMenuTarget::View(MailView::Folder(folder_id)) => self
                        .folder_for_id(folder_id)
                        .map(|folder| folder.account_id),
                    FolderMenuTarget::View(_) | FolderMenuTarget::AccountRoot => {
                        self.selected_account
                    }
                };
                let Some(account_id) = account_id else {
                    return;
                };
                let parent = match target {
                    FolderMenuTarget::View(MailView::Folder(folder_id)) => Some(folder_id),
                    FolderMenuTarget::View(_)
                    | FolderMenuTarget::AccountRoot
                    | FolderMenuTarget::Account(_) => None,
                };
                self.open_create_folder_editor(account_id, parent, &sender);
            }
            MailMsg::FolderContextRename => {
                let Some(FolderMenuTarget::View(MailView::Folder(folder_id))) =
                    self.folder_menu_target.take()
                else {
                    return;
                };
                self.open_edit_folder_editor(folder_id, &sender);
            }
            MailMsg::FolderContextDelete => {
                let Some(FolderMenuTarget::View(MailView::Folder(folder_id))) =
                    self.folder_menu_target.take()
                else {
                    return;
                };
                self.confirm_delete_folder(folder_id, &self.window, &sender);
            }
            MailMsg::NewFolder => {
                let Some(account_id) = self.selected_account else {
                    return;
                };
                let parent = match self.mail_view {
                    MailView::Folder(folder_id) => Some(folder_id),
                    MailView::UnifiedInbox | MailView::Search | MailView::Extension(_) => None,
                };
                self.open_create_folder_editor(account_id, parent, &sender);
            }
            MailMsg::EditFolder => {
                if let MailView::Folder(folder_id) = self.mail_view {
                    self.open_edit_folder_editor(folder_id, &sender);
                }
            }
            MailMsg::ConfirmDeleteFolder(folder_id) => {
                self.delete_folder(folder_id, &sender);
            }
            MailMsg::FolderEditorSubmitCreate(submit) => {
                self.submit_folder_create(submit, &sender);
            }
            MailMsg::FolderEditorSubmitEdit { folder_id, submit } => {
                self.submit_folder_edit(folder_id, submit, &sender);
            }
            MailMsg::ConfirmEmptyFolder(folder_id) => {
                self.empty_folder(folder_id, &sender);
            }
            MailMsg::ConfirmDeleteOnOrBefore { folder_id, through } => {
                if self.mock {
                    let (removed, moved_to_trash) =
                        self.mock_delete_folder_on_or_before(folder_id, through);
                    if self.mail_view == MailView::Folder(folder_id)
                        || (self.mail_view == MailView::UnifiedInbox
                            && self.folder_is_special(folder_id, &SpecialUse::Inbox))
                    {
                        self.apply_mock_list();
                    }
                    self.rebuild_folder_bar(&sender);
                    self.refresh_status_bar();
                    self.show_delete_folder_result_toast(through, removed, moved_to_trash);
                    return;
                }
                let date_text = format_local_date(through);
                let title = trf!("Deleting messages on or before {date}…", date = date_text);
                let toast = crate::toast::plain_with_timeout(&title, 0);
                self.pending_delete_toast = Some(toast.clone());
                self.toasts.add_toast(toast);
                self.pending_delete_folder = Some(folder_id);
                self.pending_delete_through = Some(through);
                self.context_sync_target = Some(MailView::Folder(folder_id));
                self.rebuild_folder_bar(&sender);
                self.set_sync_activity(Some(trs!("Deleting older mail…")), &sender);
                self.cmd.emit(UiCommand::DeleteFolderOnOrBefore {
                    folder: folder_id,
                    through,
                });
            }
            _ => unreachable!("message not routed to folder_ops"),
        }
    }

    pub(super) fn mark_folder_read(&mut self, view: MailView, sender: &ComponentSender<MailModel>) {
        let view = self.as_inbox(view);
        match view {
            MailView::UnifiedInbox => {
                let inbox_ids: HashSet<FolderId> = self.inbox_folder_ids().into_iter().collect();
                if self.mock {
                    for env in self.source_envelopes.iter_mut() {
                        if inbox_ids.contains(&env.folder_id) {
                            env.seen = true;
                        }
                    }
                }
                for folders in self.folders.values_mut() {
                    for folder in folders.iter_mut() {
                        if folder.selectable && matches!(folder.special, SpecialUse::Inbox) {
                            folder.unread = 0;
                        }
                    }
                }
                for env in self
                    .envelopes
                    .iter_mut()
                    .filter(|env| inbox_ids.contains(&env.folder_id))
                {
                    env.seen = true;
                }
            }
            MailView::Folder(folder_id) => {
                if self.mock {
                    for env in self
                        .source_envelopes
                        .iter_mut()
                        .filter(|env| env.folder_id == folder_id)
                    {
                        env.seen = true;
                    }
                }
                for folders in self.folders.values_mut() {
                    if let Some(folder) = folders.iter_mut().find(|folder| folder.id == folder_id) {
                        folder.unread = 0;
                    }
                }
                for env in self
                    .envelopes
                    .iter_mut()
                    .filter(|env| env.folder_id == folder_id)
                {
                    env.seen = true;
                }
            }
            MailView::Search | MailView::Extension(_) => return,
        }

        let refresh = match view {
            MailView::UnifiedInbox => self.is_inbox_like(self.mail_view),
            MailView::Folder(folder_id) => {
                self.mail_view == MailView::Folder(folder_id)
                    || (self.is_inbox_like(self.mail_view)
                        && self.folders.values().flatten().any(|folder| {
                            folder.id == folder_id && matches!(folder.special, SpecialUse::Inbox)
                        }))
            }
            MailView::Search | MailView::Extension(_) => false,
        };
        if self.mock && refresh {
            self.apply_mock_list();
            return;
        }
        if refresh {
            self.refresh_list();
        }
        self.rebuild_folder_bar(sender);
        self.refresh_status_bar();

        if self.mock {
            return;
        }

        match view {
            MailView::UnifiedInbox => {
                for folder_id in self.inbox_folder_ids() {
                    self.cmd.emit(UiCommand::MarkFolderRead(folder_id));
                }
            }
            MailView::Folder(folder_id) => {
                let selectable = self
                    .folders
                    .values()
                    .flatten()
                    .any(|folder| folder.id == folder_id && folder.selectable);
                if selectable {
                    self.cmd.emit(UiCommand::MarkFolderRead(folder_id));
                }
            }
            MailView::Search | MailView::Extension(_) => {}
        }
    }

    pub(super) fn empty_folder(
        &mut self,
        folder_id: FolderId,
        sender: &ComponentSender<MailModel>,
    ) {
        if self.mock {
            let special = self
                .folder_for_id(folder_id)
                .map(|folder| folder.special.clone());
            let trash_id = self
                .folder_for_id(folder_id)
                .and_then(|folder| self.trash_folder_for(folder.account_id));
            if matches!(special, Some(SpecialUse::Trash | SpecialUse::Junk)) {
                let folder = self.folder_for_id(folder_id).cloned();
                if let Some(folder) = folder {
                    let account_id = folder.account_id;
                    let folders = self.folders.get(&account_id).cloned().unwrap_or_default();
                    let nested = nested_imap_names(
                        &folders,
                        &folder.imap_name,
                        effective_mailbox_delimiter(&folder.delimiter),
                    );
                    let remove_ids: HashSet<FolderId> = folders
                        .iter()
                        .filter(|child| nested.contains(&child.imap_name))
                        .map(|child| child.id)
                        .collect();
                    let viewing_nested =
                        matches!(self.mail_view, MailView::Folder(id) if remove_ids.contains(&id));
                    self.source_envelopes.retain(|env| {
                        env.folder_id != folder_id && !remove_ids.contains(&env.folder_id)
                    });
                    if let Some(account_folders) = self.folders.get_mut(&account_id) {
                        account_folders.retain(|child| !remove_ids.contains(&child.id));
                        if let Some(target) = account_folders
                            .iter_mut()
                            .find(|child| child.id == folder_id)
                        {
                            target.unread = 0;
                            target.total = 0;
                        }
                    }
                    if viewing_nested {
                        sender.input(MailMsg::SelectFolder(folder_id));
                    }
                }
            } else {
                let ids: Vec<MessageId> = self
                    .source_envelopes
                    .iter()
                    .filter(|env| env.folder_id == folder_id)
                    .map(|env| env.id)
                    .collect();
                for id in ids {
                    self.mock_delete_message(id);
                }
                self.mock_refresh_folder_counts(folder_id);
                if let Some(trash_id) = trash_id {
                    self.mock_refresh_folder_counts(trash_id);
                }
            }
            if self.mail_view == MailView::Folder(folder_id)
                || (self.mail_view == MailView::UnifiedInbox
                    && self.folder_is_special(folder_id, &SpecialUse::Inbox))
            {
                self.apply_mock_list();
            }
            self.rebuild_folder_bar(sender);
            self.refresh_status_bar();
            return;
        }
        self.cmd.emit(UiCommand::EmptyFolder(folder_id));
    }

    pub(super) fn dismiss_pending_delete_toast(&mut self) {
        if let Some(toast) = self.pending_delete_toast.take() {
            toast.dismiss();
        }
    }

    pub(super) fn show_delete_folder_result_toast(
        &self,
        through: NaiveDate,
        removed: usize,
        moved_to_trash: bool,
    ) {
        let date_text = format_local_date(through);
        let title = if removed == 0 {
            trf!("No messages on or before {date}.", date = date_text)
        } else if moved_to_trash {
            ntr!(
                "Moved {n} message to Trash.",
                "Moved {n} messages to Trash.",
                removed
            )
        } else {
            ntr!(
                "Permanently deleted {n} message.",
                "Permanently deleted {n} messages.",
                removed
            )
        };
        let toast = crate::toast::plain(&title);
        toast.set_timeout(4);
        self.toasts.add_toast(toast);
        self.record(activity_log::KIND_MAIL, ActivityOutcome::Success, title, "");
    }

    pub(super) fn mock_refresh_folder_counts(&mut self, folder_id: FolderId) {
        let envelopes: Vec<&Envelope> = self
            .source_envelopes
            .iter()
            .filter(|env| env.folder_id == folder_id)
            .collect();
        let total = envelopes.len() as u32;
        let unread = envelopes.iter().filter(|env| !env.seen).count() as u32;
        for folders in self.folders.values_mut() {
            if let Some(folder) = folders.iter_mut().find(|folder| folder.id == folder_id) {
                folder.total = total;
                folder.unread = unread;
            }
        }
    }

    pub(super) fn mock_delete_folder_on_or_before(
        &mut self,
        folder_id: FolderId,
        through: NaiveDate,
    ) -> (usize, bool) {
        let Some(folder) = self.folder_for_id(folder_id).cloned() else {
            return (0, false);
        };
        let is_trash = matches!(folder.special, SpecialUse::Trash);
        let trash_id = self.trash_folder_for(folder.account_id);
        let moved_to_trash = !is_trash && trash_id.is_some();

        let ids: Vec<MessageId> = self
            .source_envelopes
            .iter()
            .filter(|env| env.folder_id == folder_id)
            .filter(|env| env.date.with_timezone(&Local).date_naive() <= through)
            .map(|env| env.id)
            .collect();
        let removed = ids.len();
        for id in ids {
            self.mock_delete_message(id);
        }
        self.mock_refresh_folder_counts(folder_id);
        if let Some(trash_id) = trash_id {
            self.mock_refresh_folder_counts(trash_id);
        }
        (removed, moved_to_trash)
    }

    pub(super) fn folder_delimiter_for_parent(
        &self,
        account_id: AccountId,
        parent: Option<FolderId>,
    ) -> String {
        let folders = self
            .folders
            .get(&account_id)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        alcedo_core::folder_delimiter_for_parent(folders, parent)
    }

    pub(super) fn max_folder_id(&self) -> i64 {
        self.folders
            .values()
            .flatten()
            .map(|folder| folder.id.0)
            .max()
            .unwrap_or(0)
    }

    pub(super) fn open_create_folder_editor(
        &self,
        account_id: AccountId,
        parent: Option<FolderId>,
        sender: &ComponentSender<MailModel>,
    ) {
        let folders = self.folders.get(&account_id).cloned().unwrap_or_default();
        let collapsed = if self.selected_account == Some(account_id) {
            self.collapsed_for_display(account_id, &folders)
        } else {
            self.collapsed_folders
                .get(&account_id)
                .cloned()
                .unwrap_or_default()
        };
        let spec = FolderEditorSpec {
            mode: FolderEditorMode::Create {
                account: account_id,
                parent,
            },
            account_email: self.account_email(account_id),
            folders,
            collapsed,
        };
        let sender = sender.clone();
        open_folder_editor(&self.toasts, spec, move |submit| {
            sender.input(MailMsg::FolderEditorSubmitCreate(submit));
        });
    }

    pub(super) fn open_edit_folder_editor(
        &self,
        folder_id: FolderId,
        sender: &ComponentSender<MailModel>,
    ) {
        let Some(folder) = self.folder_for_id(folder_id).cloned() else {
            return;
        };
        let account_id = folder.account_id;
        let folders = self.folders.get(&account_id).cloned().unwrap_or_default();
        let collapsed = if self.selected_account == Some(account_id) {
            self.collapsed_for_display(account_id, &folders)
        } else {
            self.collapsed_folders
                .get(&account_id)
                .cloned()
                .unwrap_or_default()
        };
        let spec = FolderEditorSpec {
            mode: FolderEditorMode::Edit { folder_id },
            account_email: self.account_email(account_id),
            folders,
            collapsed,
        };
        let sender = sender.clone();
        open_folder_editor(&self.toasts, spec, move |submit| {
            sender.input(MailMsg::FolderEditorSubmitEdit { folder_id, submit });
        });
    }

    pub(super) fn submit_folder_create(
        &mut self,
        submit: FolderEditorSubmit,
        sender: &ComponentSender<MailModel>,
    ) {
        let name = submit.name.trim().to_string();
        if self.mock {
            self.mock_create_folder(submit.account, submit.parent, name, sender);
            return;
        }
        let delimiter = self.folder_delimiter_for_parent(submit.account, submit.parent);
        let parent_imap = submit.parent.and_then(|parent_id| {
            self.folders
                .get(&submit.account)?
                .iter()
                .find(|folder| folder.id == parent_id)
                .map(|folder| folder.imap_name.as_str())
        });
        let imap_name = join_mailbox_path(parent_imap, &delimiter, &name);
        self.pending_select_imap = Some((submit.account, imap_name));
        self.cmd.emit(UiCommand::CreateFolder {
            account: submit.account,
            parent: submit.parent,
            name,
        });
    }

    pub(super) fn submit_folder_edit(
        &mut self,
        folder_id: FolderId,
        submit: FolderEditorSubmit,
        sender: &ComponentSender<MailModel>,
    ) {
        let name = submit.name.trim().to_string();
        if self.mock {
            self.mock_rename_folder(folder_id, submit.parent, name, sender);
            return;
        }
        self.cmd.emit(UiCommand::RenameFolder {
            id: folder_id,
            parent: submit.parent,
            name,
        });
    }

    pub(super) fn confirm_delete_folder(
        &self,
        folder_id: FolderId,
        _root: &adw::ApplicationWindow,
        sender: &ComponentSender<MailModel>,
    ) {
        let folders = self
            .selected_account
            .and_then(|account_id| self.folders.get(&account_id))
            .cloned()
            .unwrap_or_default();
        let Some(folder) = folders.iter().find(|folder| folder.id == folder_id) else {
            return;
        };
        let display = folder_display_name(folder);
        let delimiter = effective_mailbox_delimiter(&folder.delimiter);
        let prefix = format!("{}{delimiter}", folder.imap_name);
        let has_children = folders.iter().any(|child| {
            child.imap_name != folder.imap_name && child.imap_name.starts_with(&prefix)
        });
        let body = if has_children {
            tr!(
                "This folder and its subfolders, including all messages, will be permanently deleted from the server."
            )
        } else {
            tr!("All messages in this folder will be permanently deleted from the server.")
        };
        let confirm = adw::AlertDialog::builder()
            .heading(trf!("Delete “{folder}”?", folder = display))
            .body(body)
            .build();
        confirm.add_response("cancel", &tr!("Cancel"));
        confirm.add_response("delete", &tr!("Delete"));
        confirm.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
        confirm.set_close_response("cancel");
        let sender = sender.clone();
        confirm.choose(
            Some(&self.window),
            None::<&gtk::gio::Cancellable>,
            move |response| {
                if response.as_str() == "delete" {
                    sender.input(MailMsg::ConfirmDeleteFolder(folder_id));
                }
            },
        );
    }

    pub(super) fn delete_folder(
        &mut self,
        folder_id: FolderId,
        sender: &ComponentSender<MailModel>,
    ) {
        if self.mock {
            self.mock_delete_folder(folder_id, sender);
            return;
        }
        self.cmd.emit(UiCommand::DeleteFolder(folder_id));
    }

    pub(super) fn mock_create_folder(
        &mut self,
        account_id: AccountId,
        parent: Option<FolderId>,
        name: String,
        sender: &ComponentSender<MailModel>,
    ) {
        let delimiter = self.folder_delimiter_for_parent(account_id, parent);
        let parent_imap = parent.and_then(|parent_id| {
            self.folders
                .get(&account_id)?
                .iter()
                .find(|folder| folder.id == parent_id)
                .map(|folder| folder.imap_name.as_str())
        });
        let imap_name = join_mailbox_path(parent_imap, &delimiter, &name);
        let new_id = FolderId(self.max_folder_id() + 1);
        let folder = Folder {
            id: new_id,
            account_id,
            imap_name,
            delimiter,
            special: SpecialUse::Other,
            selectable: true,
            uidvalidity: None,
            uidnext: None,
            highest_modseq: None,
            unread: 0,
            total: 0,
        };
        self.folders.entry(account_id).or_default().push(folder);
        sender.input(MailMsg::SelectFolder(new_id));
        self.rebuild_folder_bar(sender);
        self.refresh_status_bar();
    }

    pub(super) fn mock_rename_folder(
        &mut self,
        folder_id: FolderId,
        parent: Option<FolderId>,
        name: String,
        sender: &ComponentSender<MailModel>,
    ) {
        let Some(folder) = self.folder_for_id(folder_id).cloned() else {
            return;
        };
        let account_id = folder.account_id;
        let delimiter = self.folder_delimiter_for_parent(account_id, parent);
        let parent_imap = parent.and_then(|parent_id| {
            self.folders
                .get(&account_id)?
                .iter()
                .find(|f| f.id == parent_id)
                .map(|f| f.imap_name.as_str())
        });
        let new_imap_name = join_mailbox_path(parent_imap, &delimiter, &name);
        if new_imap_name == folder.imap_name {
            return;
        }
        let folders = self.folders.get(&account_id).cloned().unwrap_or_default();
        let names: Vec<String> = folders.iter().map(|f| f.imap_name.clone()).collect();
        let pairs = rename_imap_map(
            &folder.imap_name,
            &new_imap_name,
            effective_mailbox_delimiter(&folder.delimiter),
            &names,
        );
        if let Some(account_folders) = self.folders.get_mut(&account_id) {
            for (from, to) in pairs {
                for f in account_folders.iter_mut() {
                    if f.imap_name == from {
                        f.imap_name = to.clone();
                    }
                }
            }
        }
        self.rebuild_folder_bar(sender);
        self.refresh_status_bar();
    }

    pub(super) fn mock_delete_folder(
        &mut self,
        folder_id: FolderId,
        sender: &ComponentSender<MailModel>,
    ) {
        let Some(folder) = self.folder_for_id(folder_id).cloned() else {
            return;
        };
        let account_id = folder.account_id;
        let folders = self.folders.get(&account_id).cloned().unwrap_or_default();
        let imap_names = descendant_imap_names(
            &folders,
            &folder.imap_name,
            effective_mailbox_delimiter(&folder.delimiter),
        );
        let remove_ids: HashSet<FolderId> = folders
            .iter()
            .filter(|f| imap_names.contains(&f.imap_name))
            .map(|f| f.id)
            .collect();
        let viewing_deleted =
            matches!(self.mail_view, MailView::Folder(id) if remove_ids.contains(&id));
        if let Some(account_folders) = self.folders.get_mut(&account_id) {
            account_folders.retain(|f| !remove_ids.contains(&f.id));
        }
        self.source_envelopes
            .retain(|env| !remove_ids.contains(&env.folder_id));
        if viewing_deleted {
            let inbox_id = self
                .folders
                .get(&account_id)
                .and_then(|folders| find_default_folder(folders));
            if let Some(inbox_id) = inbox_id {
                sender.input(MailMsg::SelectFolder(inbox_id));
            }
        } else if self.mail_view == MailView::Folder(folder_id) {
            self.apply_mock_list();
        }
        self.rebuild_folder_bar(sender);
        self.refresh_status_bar();
    }
}

pub(super) fn format_local_date(date: NaiveDate) -> String {
    crate::local_time::format_date(
        date,
        // TRANSLATORS: strftime pattern for a date like "Monday 7 September 2026".
        &trc!(
            "strftime pattern: weekday, day, month, year",
            "%A %-d %B %Y"
        ),
    )
}

pub(super) fn prompt_delete_folder_on_or_before(
    window: &adw::ApplicationWindow,
    folder: &Folder,
    trash_display: Option<&str>,
    sender: ComponentSender<MailModel>,
) {
    let calendar = gtk::Calendar::builder()
        .halign(gtk::Align::Fill)
        .valign(gtk::Align::Start)
        .hexpand(false)
        .vexpand(false)
        .width_request(340)
        .show_heading(true)
        .show_day_names(true)
        .show_week_numbers(false)
        .build();
    calendar.update_property(&[gtk::accessible::Property::Label(&tr!(
        "Delete through date"
    ))]);
    select_calendar_day(&calendar, Local::now().date_naive());

    let extra = gtk::Box::new(gtk::Orientation::Vertical, 12);
    extra.append(&calendar);

    let body = if folder.special == SpecialUse::Trash {
        tr!("Messages on or before the selected date will be permanently deleted.")
    } else if let Some(trash_display) = trash_display {
        trf!(
            "Messages on or before the selected date will be moved to {trash}, the same as deleting them in the list.",
            trash = trash_display
        )
    } else {
        tr!("Messages on or before the selected date will be permanently deleted.")
    };

    let dialog = adw::AlertDialog::builder()
        .heading(tr!("Delete messages through this date?"))
        .body(body)
        .extra_child(&extra)
        .build();
    dialog.add_response("cancel", &tr!("Cancel"));
    dialog.add_response("delete", &tr!("Delete"));
    dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
    dialog.set_close_response("cancel");

    let folder_id = folder.id;
    dialog.choose(
        Some(window),
        None::<&gtk::gio::Cancellable>,
        move |response| {
            if response.as_str() == "delete" {
                let through = calendar_naive_date(&calendar);
                sender.input(MailMsg::ConfirmDeleteOnOrBefore { folder_id, through });
            }
        },
    );
}

pub(super) fn mock_folder_name_matches(folder: &Folder, name: &str) -> bool {
    let lowered = name.to_ascii_lowercase();
    let normalized = match lowered.as_str() {
        "spam" => "junk",
        "deleted" | "bin" => "trash",
        other => other,
    };
    let special = match folder.special {
        SpecialUse::Inbox => "inbox",
        SpecialUse::Sent => "sent",
        SpecialUse::Drafts => "drafts",
        SpecialUse::Trash => "trash",
        SpecialUse::Junk => "junk",
        SpecialUse::Archive => "archive",
        SpecialUse::All => "all",
        SpecialUse::Snoozed => "snoozed",
        SpecialUse::Outbox => "outbox",
        SpecialUse::Other => "other",
    };
    if special != "other" && special == normalized {
        return true;
    }
    if folder.imap_name.eq_ignore_ascii_case(name) {
        return true;
    }
    if !folder.delimiter.is_empty() {
        let suffix = format!("{}{}", folder.delimiter, name);
        if folder.imap_name.eq_ignore_ascii_case(&suffix)
            || folder
                .imap_name
                .to_ascii_lowercase()
                .ends_with(&suffix.to_ascii_lowercase())
        {
            return true;
        }
    }
    folder_display_name(folder).eq_ignore_ascii_case(name)
}
