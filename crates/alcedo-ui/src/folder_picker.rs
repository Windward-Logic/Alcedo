//! Folder picker dialog for move/copy operations.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use alcedo_core::{Folder, FolderId};
use relm4::adw;
use relm4::adw::prelude::*;
use relm4::gtk;

use crate::folder_bar::{
    FolderTreeEntry, build_folder_tree, folder_display_name, folder_icon_name,
};
use crate::overflow_tooltip::install_overflow_tooltip;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FolderPickerMode {
    Move,
    Copy,
    Run,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FolderPickerRow {
    pub id: Option<FolderId>,
    pub label: String,
    pub depth: u32,
    pub selectable: bool,
    pub sensitive: bool,
    pub icon: Option<&'static str>,
    pub has_children: bool,
    pub is_expanded: bool,
    pub imap_name: Option<String>,
}

#[derive(Clone, Debug)]
pub struct FolderPickerSpec {
    pub mode: FolderPickerMode,
    pub accounts: Vec<(String, Vec<Folder>)>,
    pub disabled: HashSet<i64>,
    pub recents: Vec<FolderId>,
}

fn folder_lookup(accounts: &[(String, Vec<Folder>)]) -> HashMap<FolderId, (&str, &Folder)> {
    let mut map = HashMap::new();
    for (email, folders) in accounts {
        for folder in folders {
            map.insert(folder.id, (email.as_str(), folder));
        }
    }
    map
}

fn push_header(rows: &mut Vec<FolderPickerRow>, label: &str) {
    rows.push(FolderPickerRow {
        id: None,
        label: label.to_string(),
        depth: 0,
        selectable: false,
        sensitive: false,
        icon: None,
        has_children: false,
        is_expanded: false,
        imap_name: None,
    });
}

fn push_folder_row(
    rows: &mut Vec<FolderPickerRow>,
    entry: &FolderTreeEntry,
    disabled: &HashSet<i64>,
) {
    let folder = &entry.folder;
    let selectable = folder.selectable;
    rows.push(FolderPickerRow {
        id: Some(folder.id),
        label: folder_display_name(folder),
        depth: entry.depth,
        selectable,
        sensitive: selectable && !disabled.contains(&folder.id.0),
        icon: Some(folder_icon_name(&folder.special)),
        has_children: entry.has_children,
        is_expanded: entry.is_expanded,
        imap_name: Some(folder.imap_name.clone()),
    });
}

fn dest_row(
    id: FolderId,
    label: String,
    folder: &Folder,
    disabled: &HashSet<i64>,
) -> FolderPickerRow {
    FolderPickerRow {
        id: Some(id),
        label,
        depth: 0,
        selectable: true,
        sensitive: !disabled.contains(&id.0),
        icon: Some(folder_icon_name(&folder.special)),
        has_children: false,
        is_expanded: false,
        imap_name: None,
    }
}

pub fn folder_picker_rows(
    accounts: &[(String, Vec<Folder>)],
    query: &str,
    disabled: &HashSet<i64>,
    recents: &[FolderId],
    collapsed: &HashSet<String>,
) -> Vec<FolderPickerRow> {
    let q = query.trim().to_ascii_lowercase();
    if !q.is_empty() {
        let mut rows = Vec::new();
        let multi_account = accounts.len() > 1;
        for (email, folders) in accounts {
            for folder in folders.iter().filter(|folder| folder.selectable) {
                let display = folder_display_name(folder);
                let matches = display.to_ascii_lowercase().contains(&q)
                    || folder.imap_name.to_ascii_lowercase().contains(&q);
                if !matches {
                    continue;
                }
                let label = if multi_account {
                    format!("{email} / {display}")
                } else {
                    display
                };
                rows.push(dest_row(folder.id, label, folder, disabled));
            }
        }
        return rows;
    }

    let lookup = folder_lookup(accounts);
    let mut rows = Vec::new();

    let recent_rows: Vec<_> = recents
        .iter()
        .filter_map(|folder_id| {
            lookup
                .get(folder_id)
                .map(|(_, folder)| (*folder_id, *folder))
        })
        .collect();
    if !recent_rows.is_empty() {
        push_header(&mut rows, "Recent");
        for (folder_id, folder) in recent_rows {
            rows.push(dest_row(
                folder_id,
                folder_display_name(folder),
                folder,
                disabled,
            ));
        }
    }

    let multi_account = accounts.len() > 1;
    for (email, folders) in accounts {
        if multi_account {
            push_header(&mut rows, email);
        }
        for entry in build_folder_tree(folders, collapsed) {
            push_folder_row(&mut rows, &entry, disabled);
        }
    }

    rows
}

fn mode_labels(mode: FolderPickerMode) -> (&'static str, &'static str) {
    match mode {
        FolderPickerMode::Move => ("Move to Folder", "Move"),
        FolderPickerMode::Copy => ("Copy to Folder", "Copy"),
        FolderPickerMode::Run => ("Run Rules", "Run"),
    }
}

fn picker_row_id(row: &gtk::ListBoxRow) -> Option<FolderId> {
    let name = row.widget_name();
    let id_str = name.strip_prefix("alcedo-fp-")?;
    id_str.parse::<i64>().ok().map(FolderId)
}

fn build_folder_picker_row(
    row: &FolderPickerRow,
    on_toggle: &Rc<dyn Fn(String)>,
) -> gtk::ListBoxRow {
    let list_row = gtk::ListBoxRow::new();
    if let Some(id) = row.id {
        list_row.set_widget_name(&format!("alcedo-fp-{}", id.0));
    }
    list_row.set_activatable(row.selectable && row.sensitive);
    list_row.set_sensitive(row.sensitive || !row.selectable);
    if !row.selectable {
        list_row.set_activatable(false);
    }

    let row_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row_box.set_margin_start(8 + row.depth as i32 * 16);
    row_box.set_margin_end(8);

    if row.has_children {
        let expand_icon = if row.is_expanded {
            "pan-down-symbolic"
        } else {
            "pan-end-symbolic"
        };
        let expand_btn = gtk::Button::from_icon_name(expand_icon);
        expand_btn.add_css_class("flat");
        expand_btn.add_css_class("alcedo-folder-expand");
        expand_btn.set_tooltip_text(Some(if row.is_expanded {
            "Collapse folder"
        } else {
            "Expand folder"
        }));
        expand_btn.set_has_frame(false);
        expand_btn.set_focus_on_click(false);
        expand_btn.set_can_focus(false);
        if let Some(imap_name) = row.imap_name.clone() {
            let on_toggle = on_toggle.clone();
            expand_btn.connect_clicked(move |_| on_toggle(imap_name.clone()));
        }
        row_box.append(&expand_btn);
    } else if row.icon.is_some() {
        let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        spacer.add_css_class("alcedo-folder-expand-slot");
        spacer.set_size_request(16, -1);
        spacer.set_can_target(false);
        row_box.append(&spacer);
    }

    if let Some(icon_name) = row.icon {
        let icon = gtk::Image::from_icon_name(icon_name);
        icon.set_pixel_size(16);
        row_box.append(&icon);
    }

    let label = gtk::Label::new(Some(&row.label));
    label.set_halign(gtk::Align::Start);
    label.set_hexpand(true);
    label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    if !row.selectable {
        label.add_css_class("dim-label");
    }
    install_overflow_tooltip(&label);
    row_box.append(&label);

    list_row.set_child(Some(&row_box));
    list_row
}

pub fn open_folder_picker(
    parent: &impl IsA<gtk::Widget>,
    spec: FolderPickerSpec,
    on_choose: impl Fn(FolderId) + 'static,
) {
    let (dialog_title, confirm_label) = mode_labels(spec.mode);
    let accounts = spec.accounts;
    let disabled = spec.disabled;
    let recents = spec.recents;
    let on_choose = Rc::new(on_choose);

    let search = gtk::SearchEntry::builder()
        .placeholder_text("Filter folders")
        .hexpand(true)
        .build();

    let listbox = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::Single)
        .activate_on_single_click(false)
        .build();

    let empty_page = adw::StatusPage::builder().title("No folders").build();
    let content_stack = gtk::Stack::new();
    let scroller = gtk::ScrolledWindow::builder()
        .child(&listbox)
        .vexpand(true)
        .build();
    content_stack.set_vexpand(true);
    content_stack.add_named(&scroller, Some("list"));
    content_stack.add_named(&empty_page, Some("empty"));

    let cancel_btn = gtk::Button::builder().label("Cancel").build();
    let confirm_btn = gtk::Button::builder()
        .label(confirm_label)
        .css_classes(["suggested-action"])
        .sensitive(false)
        .build();

    let selected_folder: Rc<RefCell<Option<FolderId>>> = Rc::new(RefCell::new(None));
    let collapsed: Rc<RefCell<HashSet<String>>> = Rc::new(RefCell::new(HashSet::new()));
    let rebuild_slot: Rc<RefCell<Option<Rc<dyn Fn()>>>> = Rc::new(RefCell::new(None));

    let update_confirm = {
        let confirm_btn = confirm_btn.clone();
        let selected_folder = selected_folder.clone();
        let disabled = disabled.clone();
        Rc::new(move || {
            let enabled = selected_folder
                .borrow()
                .is_some_and(|id| !disabled.contains(&id.0));
            confirm_btn.set_sensitive(enabled);
        })
    };

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
        let content_stack = content_stack.clone();
        let empty_page = empty_page.clone();
        let search = search.clone();
        let accounts = accounts.clone();
        let disabled = disabled.clone();
        let recents = recents.clone();
        let collapsed = collapsed.clone();
        let selected_folder = selected_folder.clone();
        let update_confirm = update_confirm.clone();
        let toggle_expand = toggle_expand.clone();
        Rc::new(move || {
            let previous = *selected_folder.borrow();
            let query = search.text().to_string();
            let rows =
                folder_picker_rows(&accounts, &query, &disabled, &recents, &collapsed.borrow());

            while let Some(row) = listbox.row_at_index(0) {
                listbox.remove(&row);
            }
            *selected_folder.borrow_mut() = None;
            update_confirm();

            if rows.is_empty() {
                if query.trim().is_empty() {
                    empty_page.set_title("No folders");
                    empty_page.set_description(None);
                } else {
                    empty_page.set_title("No matching folders");
                    empty_page.set_description(Some("Try a different folder name."));
                }
                content_stack.set_visible_child_name("empty");
                return;
            }

            content_stack.set_visible_child_name("list");
            for row in rows {
                listbox.append(&build_folder_picker_row(&row, &toggle_expand));
            }

            if let Some(id) = previous {
                let mut index = 0;
                while let Some(row) = listbox.row_at_index(index) {
                    if row.is_activatable() && picker_row_id(&row) == Some(id) {
                        listbox.select_row(Some(&row));
                        break;
                    }
                    index += 1;
                }
            }
        })
    };
    *rebuild_slot.borrow_mut() = Some(rebuild.clone());

    rebuild();

    search.connect_changed({
        let rebuild = rebuild.clone();
        move |_| rebuild()
    });

    listbox.connect_row_selected({
        let selected_folder = selected_folder.clone();
        let update_confirm = update_confirm.clone();
        move |_, row| {
            let folder_id = row.and_then(|row| {
                if !row.is_activatable() {
                    return None;
                }
                picker_row_id(row)
            });
            *selected_folder.borrow_mut() = folder_id;
            update_confirm();
        }
    });

    let header = adw::HeaderBar::new();
    header.set_show_start_title_buttons(false);
    header.set_show_end_title_buttons(false);
    let title_label = gtk::Label::builder()
        .label(dialog_title)
        .css_classes(["title"])
        .build();
    header.set_title_widget(Some(&title_label));
    header.pack_start(&cancel_btn);
    header.pack_end(&confirm_btn);

    let body = gtk::Box::new(gtk::Orientation::Vertical, 12);
    body.set_margin_top(12);
    body.set_margin_bottom(12);
    body.set_margin_start(12);
    body.set_margin_end(12);
    body.append(&search);
    body.append(&content_stack);

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&body));

    let dialog = adw::Dialog::builder()
        .title(dialog_title)
        .content_width(440)
        .content_height(520)
        .default_widget(&confirm_btn)
        .focus_widget(&search)
        .child(&toolbar)
        .build();

    let choose_and_close = {
        let dialog = dialog.clone();
        let on_choose = on_choose.clone();
        Rc::new(move |folder_id: FolderId| {
            on_choose(folder_id);
            dialog.close();
        })
    };

    listbox.connect_row_activated({
        let choose_and_close = choose_and_close.clone();
        move |_, row| {
            if !row.is_activatable() {
                return;
            }
            let Some(folder_id) = picker_row_id(row) else {
                return;
            };
            choose_and_close(folder_id);
        }
    });

    cancel_btn.connect_clicked({
        let dialog = dialog.clone();
        move |_| {
            dialog.close();
        }
    });

    confirm_btn.connect_clicked({
        let selected_folder = selected_folder.clone();
        let choose_and_close = choose_and_close.clone();
        move |_| {
            if let Some(folder_id) = *selected_folder.borrow() {
                choose_and_close(folder_id);
            }
        }
    });

    dialog.present(Some(parent));
}

#[cfg(test)]
mod tests {
    use super::*;
    use alcedo_core::{AccountId, SpecialUse};
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

    fn alice_folders() -> Vec<Folder> {
        vec![
            {
                let mut inbox = sample_folder(1, "INBOX", true);
                inbox.special = SpecialUse::Inbox;
                inbox
            },
            sample_folder(2, "Projects", true),
            sample_folder(3, "Projects/Work", true),
            { sample_folder(4, "[Gmail]", false) },
        ]
    }

    #[test]
    fn folder_picker_rows_includes_recent_header() {
        let accounts = vec![("alice@example.com".to_string(), alice_folders())];
        let rows = folder_picker_rows(
            &accounts,
            "",
            &HashSet::new(),
            &[FolderId(3)],
            &HashSet::new(),
        );
        assert!(rows.iter().any(|row| row.label == "Recent"));
        assert!(rows.iter().any(|row| row.id == Some(FolderId(3))));
    }

    #[test]
    fn folder_picker_rows_filter_work_matches_display_name() {
        let accounts = vec![("alice@example.com".to_string(), alice_folders())];
        let rows = folder_picker_rows(&accounts, "work", &HashSet::new(), &[], &HashSet::new());
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].label, "Work");
        assert_eq!(rows[0].id, Some(FolderId(3)));
    }

    #[test]
    fn folder_picker_rows_disabled_is_insensitive() {
        let accounts = vec![("alice@example.com".to_string(), alice_folders())];
        let disabled = HashSet::from([2_i64]);
        let rows = folder_picker_rows(&accounts, "", &disabled, &[], &HashSet::new());
        let projects = rows
            .iter()
            .find(|row| row.id == Some(FolderId(2)))
            .expect("projects row");
        assert!(!projects.sensitive);
    }

    #[test]
    fn folder_picker_rows_multi_account_has_email_headers() {
        let mut bob_inbox = sample_folder(10, "INBOX", true);
        bob_inbox.account_id = AccountId(Uuid::from_u128(2));
        bob_inbox.special = SpecialUse::Inbox;
        let accounts = vec![
            ("alice@example.com".to_string(), alice_folders()),
            ("bob@example.com".to_string(), vec![bob_inbox]),
        ];
        let rows = folder_picker_rows(&accounts, "", &HashSet::new(), &[], &HashSet::new());
        assert!(rows.iter().any(|row| row.label == "alice@example.com"));
        assert!(rows.iter().any(|row| row.label == "bob@example.com"));
    }

    #[test]
    fn folder_picker_rows_collapsed_hides_children() {
        let accounts = vec![("alice@example.com".to_string(), alice_folders())];
        let collapsed = HashSet::from(["Projects".to_string()]);
        let rows = folder_picker_rows(&accounts, "", &HashSet::new(), &[], &collapsed);
        let projects = rows
            .iter()
            .find(|row| row.id == Some(FolderId(2)))
            .expect("projects row");
        assert!(projects.has_children);
        assert!(!projects.is_expanded);
        assert!(!rows.iter().any(|row| row.id == Some(FolderId(3))));
    }
}
