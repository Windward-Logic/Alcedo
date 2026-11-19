//! New / Edit Folder dialog.

use alcedo_i18n::{tr, trf, trs};
use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use alcedo_core::{
    AccountId, Folder, FolderId, folder_delimiter_for_parent, imap_name_exists, join_mailbox_path,
    validate_folder_leaf,
};
use relm4::adw;
use relm4::adw::prelude::*;
use relm4::gtk;
use relm4::gtk::glib;

use crate::folder_bar::{
    build_folder_tree, folder_display_name, folder_effective_parent, folder_icon_css_class,
    folder_icon_name, folder_is_under, imap_ancestor_names,
};
use crate::overflow_tooltip::install_overflow_tooltip;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FolderEditorMode {
    Create {
        account: AccountId,
        parent: Option<FolderId>,
    },
    Edit {
        folder_id: FolderId,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FolderEditorSubmit {
    pub account: AccountId,
    pub parent: Option<FolderId>,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FolderEditorLocRow {
    pub id: Option<FolderId>,
    pub label: String,
    pub depth: usize,
    pub imap_name: Option<String>,
}

#[derive(Clone, Debug)]
pub struct FolderEditorSpec {
    pub mode: FolderEditorMode,
    pub account_email: String,
    pub folders: Vec<Folder>,
    pub collapsed: HashSet<String>,
}

fn account_for_mode(mode: &FolderEditorMode, folders: &[Folder]) -> Option<AccountId> {
    match mode {
        FolderEditorMode::Create { account, .. } => Some(*account),
        FolderEditorMode::Edit { folder_id } => folders
            .iter()
            .find(|folder| folder.id == *folder_id)
            .map(|folder| folder.account_id),
    }
}

fn parent_imap(folders: &[Folder], parent: Option<FolderId>) -> Option<&str> {
    parent.and_then(|id| {
        folders
            .iter()
            .find(|folder| folder.id == id)
            .map(|folder| folder.imap_name.as_str())
    })
}

/// Editable name: the last path segment, decoded from modified UTF-7 because
/// saving encodes it again.
fn folder_leaf_name(folder: &Folder) -> String {
    let leaf = if folder.delimiter.is_empty() {
        folder.imap_name.as_str()
    } else {
        folder
            .imap_name
            .rsplit(&folder.delimiter)
            .next()
            .unwrap_or(&folder.imap_name)
    };
    alcedo_core::decode_mailbox_name(leaf)
}

fn parent_folder_id(folders: &[Folder], folder: &Folder) -> Option<FolderId> {
    let known_names: HashSet<String> = folders.iter().map(|f| f.imap_name.clone()).collect();
    let parent_imap = folder_effective_parent(&folder.imap_name, &folder.delimiter, &known_names)?;
    folders
        .iter()
        .find(|f| f.imap_name == parent_imap)
        .map(|f| f.id)
}

fn should_exclude_from_location(
    folder: &Folder,
    exclude_id: FolderId,
    exclude_imap: &str,
    delimiter: &str,
) -> bool {
    folder.id == exclude_id || folder_is_under(&folder.imap_name, exclude_imap, delimiter)
}

/// Location rows for the folder editor: account root, then the folder tree.
pub fn location_rows(
    mode: &FolderEditorMode,
    folders: &[Folder],
    collapsed: &HashSet<String>,
) -> Vec<FolderEditorLocRow> {
    let mut rows = vec![FolderEditorLocRow {
        id: None,
        label: tr!("Account root"),
        depth: 0,
        imap_name: None,
    }];

    let exclude = match mode {
        FolderEditorMode::Edit { folder_id } => folders
            .iter()
            .find(|folder| folder.id == *folder_id)
            .map(|folder| {
                (
                    folder.id,
                    folder.imap_name.clone(),
                    folder.delimiter.clone(),
                )
            }),
        FolderEditorMode::Create { .. } => None,
    };

    let filtered: Vec<Folder> = if let Some((exclude_id, exclude_imap, delimiter)) = exclude {
        folders
            .iter()
            .filter(|folder| {
                !should_exclude_from_location(folder, exclude_id, &exclude_imap, &delimiter)
            })
            .cloned()
            .collect()
    } else {
        folders.to_vec()
    };

    for entry in build_folder_tree(&filtered, collapsed) {
        let folder = &entry.folder;
        rows.push(FolderEditorLocRow {
            id: Some(folder.id),
            label: folder_display_name(folder),
            depth: entry.depth as usize,
            imap_name: Some(folder.imap_name.clone()),
        });
    }

    rows
}

/// Ensure a folder row is visible in the location tree by expanding its ancestors.
pub fn expand_ancestors_for_location(
    folders: &[Folder],
    collapsed: &mut HashSet<String>,
    folder_id: FolderId,
) {
    let Some(folder) = folders.iter().find(|entry| entry.id == folder_id) else {
        return;
    };
    let known_names: HashSet<String> = folders
        .iter()
        .map(|entry| entry.imap_name.clone())
        .collect();
    for ancestor in imap_ancestor_names(&folder.imap_name, &folder.delimiter, &known_names) {
        collapsed.remove(&ancestor);
    }
}

fn find_location_row(
    listbox: &gtk::ListBox,
    target_parent: Option<FolderId>,
) -> Option<gtk::ListBoxRow> {
    let mut index = 0;
    while let Some(row) = listbox.row_at_index(index) {
        if location_row_id(&row) == Some(target_parent) {
            return Some(row);
        }
        index += 1;
    }
    None
}

fn select_location_row(listbox: &gtk::ListBox, target_parent: Option<FolderId>) {
    if let Some(row) = find_location_row(listbox, target_parent) {
        listbox.select_row(Some(&row));
    }
}

/// Scroll offset that centers `row_y` in the viewport, clamped to the adjustment.
fn location_scroll_value(
    row_y: f64,
    row_height: f64,
    page: f64,
    lower: f64,
    upper: f64,
) -> Option<f64> {
    if row_height <= 0.0 || page <= 1.0 {
        return None;
    }
    let max = (upper - page).max(lower);
    Some((row_y - (page - row_height) / 2.0).clamp(lower, max))
}

fn scroll_row_into_view(
    scroller: &gtk::ScrolledWindow,
    listbox: &gtk::ListBox,
    row: &gtk::ListBoxRow,
) -> bool {
    let Some(bounds) = row.compute_bounds(listbox) else {
        return false;
    };
    let adj = scroller.vadjustment();
    let Some(value) = location_scroll_value(
        f64::from(bounds.y()),
        f64::from(bounds.height()),
        adj.page_size(),
        adj.lower(),
        adj.upper(),
    ) else {
        return false;
    };
    adj.set_value(value);
    true
}

fn entry_contains_focus(entry: &adw::EntryRow) -> bool {
    let widget = entry.upcast_ref::<gtk::Widget>();
    if widget.has_focus() {
        return true;
    }
    widget
        .root()
        .and_then(|root| root.focus())
        .is_some_and(|focus| focus.is_ancestor(widget))
}

/// Select and scroll the parent row into view, then place the caret in the name field.
fn schedule_reveal_location(
    scroller: gtk::ScrolledWindow,
    listbox: gtk::ListBox,
    name_row: adw::EntryRow,
    target_parent: Option<FolderId>,
) {
    let mut attempts = 0u8;
    glib::idle_add_local(move || {
        attempts += 1;
        let scrolled = find_location_row(&listbox, target_parent).is_some_and(|row| {
            listbox.select_row(Some(&row));
            scroll_row_into_view(&scroller, &listbox, &row)
        });
        if !entry_contains_focus(&name_row) {
            name_row.grab_focus();
        }
        if (scrolled && entry_contains_focus(&name_row)) || attempts >= 12 {
            glib::ControlFlow::Break
        } else {
            glib::ControlFlow::Continue
        }
    });
}

/// Default parent folder for create/edit (`None` = account root).
pub fn default_location(mode: &FolderEditorMode, folders: &[Folder]) -> Option<FolderId> {
    match mode {
        FolderEditorMode::Create { parent, .. } => *parent,
        FolderEditorMode::Edit { folder_id } => {
            let folder = folders.iter().find(|folder| folder.id == *folder_id)?;
            parent_folder_id(folders, folder)
        }
    }
}

/// Validate a folder name and parent before submit.
pub fn validate_submit(
    mode: &FolderEditorMode,
    folders: &[Folder],
    name: &str,
    parent: Option<FolderId>,
) -> Result<FolderEditorSubmit, String> {
    let account = account_for_mode(mode, folders).ok_or_else(|| "Folder not found".to_string())?;
    let delimiter = folder_delimiter_for_parent(folders, parent);
    validate_folder_leaf(name, &delimiter)?;
    let leaf = name.trim();
    let imap_name = join_mailbox_path(parent_imap(folders, parent), &delimiter, leaf);

    let own_imap = match mode {
        FolderEditorMode::Edit { folder_id } => folders
            .iter()
            .find(|folder| folder.id == *folder_id)
            .map(|folder| folder.imap_name.as_str()),
        FolderEditorMode::Create { .. } => None,
    };
    if imap_name_exists(folders, &imap_name) && own_imap != Some(imap_name.as_str()) {
        return Err(tr!("A folder with this name already exists"));
    }

    Ok(FolderEditorSubmit {
        account,
        parent,
        name: leaf.to_string(),
    })
}

fn mode_labels(mode: &FolderEditorMode) -> (&'static str, &'static str) {
    match mode {
        FolderEditorMode::Create { .. } => (trs!("New Folder"), trs!("Create")),
        FolderEditorMode::Edit { .. } => (trs!("Edit Folder"), trs!("Save")),
    }
}

fn location_row_id(row: &gtk::ListBoxRow) -> Option<Option<FolderId>> {
    let name = row.widget_name();
    if name == "alcedo-fe-root" {
        return Some(None);
    }
    let id_str = name.strip_prefix("alcedo-fe-")?;
    Some(Some(FolderId(id_str.parse().ok()?)))
}

fn build_location_row_with_folder(
    row: &FolderEditorLocRow,
    folder: Option<&Folder>,
    has_children: bool,
    is_expanded: bool,
    on_toggle: &Rc<dyn Fn(String)>,
) -> gtk::ListBoxRow {
    let list_row = gtk::ListBoxRow::new();
    if let Some(id) = row.id {
        list_row.set_widget_name(&format!("alcedo-fe-{}", id.0));
    } else {
        list_row.set_widget_name("alcedo-fe-root");
    }
    list_row.set_activatable(true);

    let row_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row_box.set_margin_start(8 + row.depth as i32 * 16);
    row_box.set_margin_end(8);

    if has_children {
        let expand_icon = if is_expanded {
            "pan-down-symbolic"
        } else {
            "pan-end-symbolic"
        };
        let expand_btn = gtk::Button::from_icon_name(expand_icon);
        expand_btn.add_css_class("flat");
        expand_btn.add_css_class("alcedo-folder-expand");
        expand_btn.set_tooltip_text(Some(if is_expanded {
            trs!("Collapse folder")
        } else {
            trs!("Expand folder")
        }));
        expand_btn.set_has_frame(false);
        expand_btn.set_focus_on_click(false);
        expand_btn.set_can_focus(false);
        if let Some(imap_name) = row.imap_name.clone() {
            let on_toggle = on_toggle.clone();
            expand_btn.connect_clicked(move |_| on_toggle(imap_name.clone()));
        }
        row_box.append(&expand_btn);
    } else if row.id.is_some() {
        let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        spacer.add_css_class("alcedo-folder-expand-slot");
        spacer.set_size_request(16, -1);
        spacer.set_can_target(false);
        row_box.append(&spacer);
    }

    if let Some(folder) = folder {
        let icon = gtk::Image::from_icon_name(folder_icon_name(&folder.special));
        icon.add_css_class("alcedo-folder-icon");
        icon.add_css_class(folder_icon_css_class(&folder.special));
        icon.set_pixel_size(16);
        row_box.append(&icon);
    }

    let label = gtk::Label::new(Some(&row.label));
    label.set_halign(gtk::Align::Start);
    label.set_hexpand(true);
    label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    if row.id.is_none() {
        label.add_css_class("dim-label");
    }
    install_overflow_tooltip(&label);
    row_box.append(&label);

    list_row.set_child(Some(&row_box));
    list_row
}

fn show_validation_toast(overlay: &adw::ToastOverlay, message: &str) {
    overlay.add_toast(crate::toast::plain(message));
}

fn submit_unchanged(
    mode: &FolderEditorMode,
    folders: &[Folder],
    name: &str,
    parent: Option<FolderId>,
) -> bool {
    let FolderEditorMode::Edit { folder_id } = mode else {
        return false;
    };
    let Some(folder) = folders.iter().find(|folder| folder.id == *folder_id) else {
        return false;
    };
    name.trim() == folder_leaf_name(folder) && parent == parent_folder_id(folders, folder)
}

/// Open the New / Edit Folder dialog.
pub fn open_folder_editor(
    parent: &impl IsA<gtk::Widget>,
    spec: FolderEditorSpec,
    on_submit: impl Fn(FolderEditorSubmit) + 'static,
) {
    let mode = spec.mode;
    let folders = spec.folders;
    let (dialog_title, primary_label) = mode_labels(&mode);
    let on_submit = Rc::new(on_submit);

    let name_row = adw::EntryRow::builder().title(tr!("Name")).build();
    if let FolderEditorMode::Edit { folder_id } = &mode
        && let Some(folder) = folders.iter().find(|folder| folder.id == *folder_id)
    {
        name_row.set_text(&folder_leaf_name(folder));
    }

    let details_group = adw::PreferencesGroup::new();
    details_group.add(&name_row);

    let info_group = adw::PreferencesGroup::new();
    let mut show_info_group = false;
    if let FolderEditorMode::Edit { folder_id } = &mode
        && let Some(folder) = folders.iter().find(|folder| folder.id == *folder_id)
    {
        let imap_row = adw::ActionRow::builder()
            .use_markup(false)
            .title(tr!("IMAP path"))
            .subtitle(&folder.imap_name)
            .build();
        let messages_row = adw::ActionRow::builder()
            .title(tr!("Messages"))
            .subtitle(trf!(
                "{unread} unread, {total} total",
                unread = folder.unread,
                total = folder.total
            ))
            .build();
        info_group.add(&imap_row);
        info_group.add(&messages_row);
        show_info_group = true;
    }

    let location_label = gtk::Label::builder()
        .label(tr!("Location"))
        .css_classes(["dim-label"])
        .halign(gtk::Align::Start)
        .build();

    let listbox = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::Single)
        .css_classes(["navigation-sidebar"])
        .activate_on_single_click(true)
        .build();

    let scroller = gtk::ScrolledWindow::builder()
        .child(&listbox)
        .vexpand(true)
        .build();

    let body = gtk::Box::new(gtk::Orientation::Vertical, 12);
    body.set_margin_top(12);
    body.set_margin_bottom(12);
    body.set_margin_start(12);
    body.set_margin_end(12);
    body.append(&details_group);
    if show_info_group {
        body.append(&info_group);
    }
    body.append(&location_label);
    body.append(&scroller);

    let toast_overlay = adw::ToastOverlay::new();
    toast_overlay.set_child(Some(&body));

    let cancel_btn = gtk::Button::builder().label(tr!("Cancel")).build();
    let primary_btn = gtk::Button::builder()
        .label(primary_label)
        .css_classes(["suggested-action"])
        .sensitive(false)
        .build();

    let header = adw::HeaderBar::new();
    header.set_show_start_title_buttons(false);
    header.set_show_end_title_buttons(false);
    let title_label = gtk::Label::builder()
        .label(dialog_title)
        .css_classes(["title"])
        .build();
    header.set_title_widget(Some(&title_label));
    header.pack_start(&cancel_btn);
    header.pack_end(&primary_btn);

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&toast_overlay));

    let dialog = adw::Dialog::builder()
        .title(dialog_title)
        .content_width(440)
        .content_height(520)
        .default_widget(&primary_btn)
        .focus_widget(&name_row)
        .child(&toolbar)
        .build();

    let selected_parent: Rc<RefCell<Option<FolderId>>> =
        Rc::new(RefCell::new(default_location(&mode, &folders)));
    let mut initial_collapsed = spec.collapsed;
    if let FolderEditorMode::Create {
        parent: Some(parent_id),
        ..
    } = &mode
    {
        expand_ancestors_for_location(&folders, &mut initial_collapsed, *parent_id);
    } else if let FolderEditorMode::Edit { folder_id } = &mode {
        expand_ancestors_for_location(&folders, &mut initial_collapsed, *folder_id);
    }
    let collapsed: Rc<RefCell<HashSet<String>>> = Rc::new(RefCell::new(initial_collapsed));
    let rebuild_slot: Rc<RefCell<Option<Rc<dyn Fn()>>>> = Rc::new(RefCell::new(None));

    let update_primary = {
        let primary_btn = primary_btn.clone();
        let name_row = name_row.clone();
        Rc::new(move || {
            primary_btn.set_sensitive(!name_row.text().trim().is_empty());
        })
    };
    update_primary();

    let toggle_expand: Rc<dyn Fn(String)> = {
        let collapsed = collapsed.clone();
        let rebuild_slot = rebuild_slot.clone();
        Rc::new(move |imap_name: String| {
            {
                let mut set = collapsed.borrow_mut();
                if !set.remove(&imap_name) {
                    set.insert(imap_name);
                }
            }
            if let Some(rebuild) = rebuild_slot.borrow().clone() {
                rebuild();
            }
        })
    };

    let rebuild: Rc<dyn Fn()> = {
        let listbox = listbox.clone();
        let folders = folders.clone();
        let mode = mode.clone();
        let collapsed = collapsed.clone();
        let selected_parent = selected_parent.clone();
        let toggle_expand = toggle_expand.clone();
        Rc::new(move || {
            let previous = *selected_parent.borrow();
            let tree_entries = build_folder_tree(
                &match &mode {
                    FolderEditorMode::Edit { folder_id } => {
                        if let Some(folder) = folders.iter().find(|f| f.id == *folder_id) {
                            let exclude_imap = folder.imap_name.clone();
                            let delimiter = folder.delimiter.clone();
                            folders
                                .iter()
                                .filter(|f| {
                                    !should_exclude_from_location(
                                        f,
                                        *folder_id,
                                        &exclude_imap,
                                        &delimiter,
                                    )
                                })
                                .cloned()
                                .collect()
                        } else {
                            folders.clone()
                        }
                    }
                    FolderEditorMode::Create { .. } => folders.clone(),
                },
                &collapsed.borrow(),
            );
            let mut entry_meta: std::collections::HashMap<FolderId, (bool, bool, Folder)> =
                std::collections::HashMap::new();
            for entry in &tree_entries {
                entry_meta.insert(
                    entry.folder.id,
                    (entry.has_children, entry.is_expanded, entry.folder.clone()),
                );
            }

            let rows = location_rows(&mode, &folders, &collapsed.borrow());

            while let Some(row) = listbox.row_at_index(0) {
                listbox.remove(&row);
            }

            for row in &rows {
                let meta = row.id.and_then(|id| entry_meta.get(&id));
                let list_row = build_location_row_with_folder(
                    row,
                    meta.map(|(_, _, folder)| folder),
                    meta.map(|(has_children, _, _)| *has_children)
                        .unwrap_or(false),
                    meta.map(|(_, is_expanded, _)| *is_expanded).unwrap_or(true),
                    &toggle_expand,
                );
                listbox.append(&list_row);
            }

            select_location_row(&listbox, previous);
        })
    };
    *rebuild_slot.borrow_mut() = Some(rebuild.clone());
    rebuild();

    listbox.connect_row_activated({
        let selected_parent = selected_parent.clone();
        move |list, row| {
            let parent_id = location_row_id(row);
            if parent_id.is_none() {
                return;
            }
            *selected_parent.borrow_mut() = parent_id.unwrap();
            list.select_row(Some(row));
        }
    });

    listbox.connect_row_selected({
        let selected_parent = selected_parent.clone();
        move |_, row| {
            if let Some(row) = row
                && let Some(parent_id) = location_row_id(row)
            {
                *selected_parent.borrow_mut() = parent_id;
            }
        }
    });

    name_row.connect_changed({
        let update_primary = update_primary.clone();
        move |_| update_primary()
    });

    let submit_and_close = {
        let dialog = dialog.clone();
        let on_submit = on_submit.clone();
        let toast_overlay = toast_overlay.clone();
        let name_row = name_row.clone();
        let folders = folders.clone();
        let mode = mode.clone();
        let selected_parent = selected_parent.clone();
        Rc::new(move || {
            let parent_id = *selected_parent.borrow();
            let name = name_row.text().to_string();
            if submit_unchanged(&mode, &folders, &name, parent_id) {
                dialog.close();
                return;
            }
            match validate_submit(&mode, &folders, &name, parent_id) {
                Ok(submit) => {
                    on_submit(submit);
                    dialog.close();
                }
                Err(message) => show_validation_toast(&toast_overlay, &message),
            }
        })
    };

    primary_btn.connect_clicked({
        let submit_and_close = submit_and_close.clone();
        move |_| submit_and_close()
    });
    name_row.connect_entry_activated({
        let submit_and_close = submit_and_close.clone();
        move |_| submit_and_close()
    });

    cancel_btn.connect_clicked({
        let dialog = dialog.clone();
        move |_| {
            dialog.close();
        }
    });

    let initial_location = *selected_parent.borrow();
    dialog.present(Some(parent));
    name_row.grab_focus();
    schedule_reveal_location(scroller, listbox, name_row, initial_location);
}

#[cfg(test)]
mod tests {
    use super::*;
    use alcedo_core::SpecialUse;
    use uuid::Uuid;

    fn sample_folder(id: i64, imap_name: &str, selectable: bool) -> Folder {
        Folder {
            id: FolderId(id),
            account_id: AccountId(Uuid::from_u128(1)),
            imap_name: imap_name.to_string(),
            delimiter: "/".to_string(),
            special: SpecialUse::Other,
            selectable,
            uidvalidity: None,
            uidnext: None,
            highest_modseq: None,
            unread: 0,
            total: 0,
        }
    }

    fn sample_folders() -> Vec<Folder> {
        vec![
            {
                let mut inbox = sample_folder(1, "INBOX", true);
                inbox.special = SpecialUse::Inbox;
                inbox
            },
            sample_folder(2, "Projects", true),
            sample_folder(3, "Projects/Work", true),
            sample_folder(4, "Projects/Personal", true),
            sample_folder(5, "Archive", true),
        ]
    }

    #[test]
    fn location_rows_start_with_account_root() {
        let folders = sample_folders();
        let mode = FolderEditorMode::Create {
            account: AccountId(Uuid::from_u128(1)),
            parent: None,
        };
        let rows = location_rows(&mode, &folders, &HashSet::new());
        assert!(!rows.is_empty());
        assert_eq!(rows[0].label, "Account root");
        assert_eq!(rows[0].id, None);
        assert_eq!(rows[0].depth, 0);
    }

    #[test]
    fn location_rows_edit_omits_self_and_children() {
        let folders = sample_folders();
        let mode = FolderEditorMode::Edit {
            folder_id: FolderId(2),
        };
        let rows = location_rows(&mode, &folders, &HashSet::new());
        assert!(rows.iter().all(|row| row.id != Some(FolderId(2))));
        assert!(rows.iter().all(|row| row.id != Some(FolderId(3))));
        assert!(rows.iter().all(|row| row.id != Some(FolderId(4))));
        assert!(rows.iter().any(|row| row.id == Some(FolderId(5))));
    }

    #[test]
    fn validate_submit_rejects_empty_name() {
        let folders = sample_folders();
        let mode = FolderEditorMode::Create {
            account: AccountId(Uuid::from_u128(1)),
            parent: None,
        };
        let err = validate_submit(&mode, &folders, "   ", None).expect_err("empty");
        assert_eq!(err, "Enter a folder name");
    }

    #[test]
    fn validate_submit_collision_on_sibling() {
        let folders = sample_folders();
        let mode = FolderEditorMode::Create {
            account: AccountId(Uuid::from_u128(1)),
            parent: None,
        };
        let err = validate_submit(&mode, &folders, "Projects", None).expect_err("collision");
        assert_eq!(err, "A folder with this name already exists");
    }

    #[test]
    fn validate_submit_edit_allows_own_name() {
        let folders = sample_folders();
        let mode = FolderEditorMode::Edit {
            folder_id: FolderId(2),
        };
        let submit = validate_submit(&mode, &folders, "Projects", None).expect("own name");
        assert_eq!(submit.name, "Projects");
        assert_eq!(submit.parent, None);
    }

    #[test]
    fn default_location_create_uses_parent_hint() {
        let folders = sample_folders();
        let mode = FolderEditorMode::Create {
            account: AccountId(Uuid::from_u128(1)),
            parent: Some(FolderId(2)),
        };
        assert_eq!(default_location(&mode, &folders), Some(FolderId(2)));
    }

    #[test]
    fn default_location_edit_uses_current_parent() {
        let folders = sample_folders();
        let mode = FolderEditorMode::Edit {
            folder_id: FolderId(3),
        };
        assert_eq!(default_location(&mode, &folders), Some(FolderId(2)));
    }

    #[test]
    fn expand_ancestors_for_location_reveals_nested_default() {
        let folders = sample_folders();
        let mut collapsed = HashSet::from(["Projects".to_string()]);
        expand_ancestors_for_location(&folders, &mut collapsed, FolderId(3));
        let mode = FolderEditorMode::Create {
            account: AccountId(Uuid::from_u128(1)),
            parent: Some(FolderId(3)),
        };
        let rows = location_rows(&mode, &folders, &collapsed);
        assert!(rows.iter().any(|row| row.id == Some(FolderId(3))));
    }

    #[test]
    fn location_scroll_value_centers_parent_and_clamps() {
        let centered = location_scroll_value(400.0, 40.0, 200.0, 0.0, 1000.0).expect("page");
        assert!((centered - 320.0).abs() < f64::EPSILON);

        let top = location_scroll_value(10.0, 40.0, 200.0, 0.0, 1000.0).expect("top");
        assert!((top - 0.0).abs() < f64::EPSILON);

        let bottom = location_scroll_value(900.0, 40.0, 200.0, 0.0, 1000.0).expect("bottom");
        assert!((bottom - 800.0).abs() < f64::EPSILON);

        assert!(location_scroll_value(10.0, 0.0, 200.0, 0.0, 1000.0).is_none());
        assert!(location_scroll_value(10.0, 40.0, 0.0, 0.0, 1000.0).is_none());
    }

    #[test]
    fn location_rows_hide_nested_folder_when_ancestor_collapsed() {
        let folders = sample_folders();
        let collapsed = HashSet::from(["Projects".to_string()]);
        let mode = FolderEditorMode::Create {
            account: AccountId(Uuid::from_u128(1)),
            parent: Some(FolderId(3)),
        };
        let rows = location_rows(&mode, &folders, &collapsed);
        assert!(!rows.iter().any(|row| row.id == Some(FolderId(3))));
    }
}
