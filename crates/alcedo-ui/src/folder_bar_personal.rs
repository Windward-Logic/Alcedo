//! "Folders" group: one collapsible row holding an account's user-created IMAP folders.

use alcedo_i18n::{n_, trm, trs};
use std::collections::HashSet;
use std::rc::Rc;

use alcedo_core::{AccountId, Folder, SpecialUse};
use relm4::gtk;
use relm4::gtk::prelude::*;

use crate::folder_bar::{
    FolderRowCallbacks, FolderTreeEntry, MailView, append_folder_rows, attach_folder_drop_expand,
    attach_row_click, build_folder_tree, folder_icon_css_class,
};
use crate::icons;

const ROW_PERSONAL_PREFIX: &str = "alcedo-personal-folders-";
const GROUP_LABEL: &str = n_!("Folders");

/// An account's folder tree split into system rows and the user-created group.
pub struct GroupedFolderTree {
    pub system: Vec<FolderTreeEntry>,
    pub personal: Vec<FolderTreeEntry>,
    /// Unread across every personal folder, including ones hidden by a per-folder collapse.
    pub personal_unread: u32,
}

struct PersonalRoots {
    names: HashSet<String>,
    unread: u32,
}

/// A top-level folder tree is personal when none of its folders has a special-use role.
/// A namespace parent such as `[Gmail]` that holds Sent, Trash or All Mail stays with the
/// system rows, and `INBOX/Work` stays nested under Inbox.
fn find_personal_roots(folders: &[Folder]) -> PersonalRoots {
    let full_tree = build_folder_tree(folders, &HashSet::new());
    let mut roots = PersonalRoots {
        names: HashSet::new(),
        unread: 0,
    };
    for tree in full_tree.chunk_by(|_, next| next.depth > 0) {
        let all_user_created = tree
            .iter()
            .all(|entry| matches!(entry.folder.special, SpecialUse::Other));
        if !all_user_created {
            continue;
        }
        roots.names.insert(tree[0].folder.imap_name.clone());
        roots.unread = tree.iter().fold(roots.unread, |sum, entry| {
            sum.saturating_add(entry.folder.unread)
        });
    }
    roots
}

pub fn build_grouped_folder_tree(
    folders: &[Folder],
    collapsed: &HashSet<String>,
) -> GroupedFolderTree {
    let roots = find_personal_roots(folders);
    let mut grouped = GroupedFolderTree {
        system: Vec::new(),
        personal: Vec::new(),
        personal_unread: roots.unread,
    };
    let mut in_personal_tree = false;
    for entry in build_folder_tree(folders, collapsed) {
        if entry.depth == 0 {
            in_personal_tree = roots.names.contains(&entry.folder.imap_name);
        }
        if in_personal_tree {
            grouped.personal.push(entry);
        } else {
            grouped.system.push(entry);
        }
    }
    grouped
}

pub(crate) fn personal_header_widget_name(account_id: AccountId) -> String {
    format!("{ROW_PERSONAL_PREFIX}{}", account_id.0)
}

/// System folders, then the Folders group row, then its folders one level deeper.
pub(crate) fn append_grouped_folder_rows(
    list_box: &gtk::ListBox,
    grouped: &GroupedFolderTree,
    personal_expanded: bool,
    base_depth: u32,
    mail_view: MailView,
    syncing: Option<MailView>,
    callbacks: &FolderRowCallbacks,
) {
    append_folder_rows(
        list_box,
        &grouped.system,
        base_depth,
        mail_view,
        syncing,
        callbacks,
    );
    let Some(first) = grouped.personal.first() else {
        return;
    };
    let account_id = first.folder.account_id;
    list_box.append(&personal_header_row(
        account_id,
        personal_expanded,
        grouped.personal_unread,
        base_depth,
        callbacks,
    ));
    if personal_expanded {
        append_folder_rows(
            list_box,
            &grouped.personal,
            base_depth + 1,
            mail_view,
            syncing,
            callbacks,
        );
    }
}

fn personal_header_row(
    account_id: AccountId,
    expanded: bool,
    unread: u32,
    depth: u32,
    callbacks: &FolderRowCallbacks,
) -> gtk::ListBoxRow {
    let action = if expanded {
        trs!("Collapse folders")
    } else {
        trs!("Expand folders")
    };
    let row = gtk::ListBoxRow::new();
    row.set_widget_name(&personal_header_widget_name(account_id));
    row.set_activatable(false);
    row.set_selectable(false);
    row.set_can_focus(false);
    row.set_tooltip_text(Some(action));
    row.update_property(&[gtk::accessible::Property::Label(trm!(GROUP_LABEL))]);
    row.update_state(&[gtk::accessible::State::Expanded(Some(expanded))]);

    let expand_btn = gtk::Button::from_icon_name(if expanded {
        "pan-down-symbolic"
    } else {
        "pan-end-symbolic"
    });
    expand_btn.add_css_class("flat");
    expand_btn.add_css_class("alcedo-folder-expand");
    expand_btn.set_has_frame(false);
    expand_btn.set_focus_on_click(false);
    expand_btn.set_tooltip_text(Some(action));
    expand_btn.connect_clicked({
        let on_toggle = callbacks.on_toggle_personal.clone();
        move |_| on_toggle(account_id)
    });

    let icon = icons::sidebar_image("folder-symbolic");
    icon.add_css_class("alcedo-folder-icon");
    icon.add_css_class(folder_icon_css_class(&SpecialUse::Other));

    let label = gtk::Label::builder()
        .label(trm!(GROUP_LABEL))
        .xalign(0.0)
        .hexpand(true)
        .build();

    let row_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row_box.add_css_class("alcedo-folder-row");
    row_box.set_margin_start(8 + depth as i32 * 16);
    row_box.set_margin_end(8);
    row_box.append(&expand_btn);
    row_box.append(&icon);
    row_box.append(&label);
    if !expanded && unread > 0 {
        let badge = gtk::Label::new(Some(&unread.to_string()));
        badge.add_css_class("alcedo-folder-unread");
        badge.set_halign(gtk::Align::End);
        badge.set_xalign(1.0);
        row_box.append(&badge);
    }

    attach_row_click(&row_box, {
        let on_toggle = callbacks.on_toggle_personal.clone();
        move || on_toggle(account_id)
    });
    // Spring-load the group so a dragged message can reach the folders inside it,
    // matching collapsed parent folders. Only collapsed rows get this: the toggle
    // callback would otherwise collapse an open group under the pointer.
    if !expanded {
        let on_toggle = callbacks.on_toggle_personal.clone();
        attach_folder_drop_expand(&row, Rc::new(move || on_toggle(account_id)));
    }
    row.set_child(Some(&row_box));
    row
}

#[cfg(test)]
mod tests {
    use alcedo_core::FolderId;
    use uuid::Uuid;

    use super::*;
    use crate::test_helpers::expands_on_drag_hover;

    fn folder(id: i64, imap_name: &str, special: SpecialUse, unread: u32) -> Folder {
        Folder {
            id: FolderId(id),
            account_id: AccountId(Uuid::from_u128(1)),
            imap_name: imap_name.to_string(),
            delimiter: "/".to_string(),
            special,
            selectable: true,
            uidvalidity: None,
            uidnext: None,
            highest_modseq: None,
            unread,
            total: 0,
        }
    }

    fn names(entries: &[FolderTreeEntry]) -> Vec<&str> {
        entries
            .iter()
            .map(|entry| entry.folder.imap_name.as_str())
            .collect()
    }

    #[test]
    fn user_created_folders_group_apart_from_special_use_folders() {
        let folders = vec![
            folder(1, "INBOX", SpecialUse::Inbox, 0),
            folder(2, "Sent", SpecialUse::Sent, 0),
            folder(3, "Business", SpecialUse::Other, 0),
            folder(4, "Business/Banking", SpecialUse::Other, 0),
            folder(5, "Olympia", SpecialUse::Other, 0),
        ];
        let grouped = build_grouped_folder_tree(&folders, &HashSet::new());
        assert_eq!(names(&grouped.system), ["INBOX", "Sent"]);
        assert_eq!(
            names(&grouped.personal),
            ["Business", "Business/Banking", "Olympia"]
        );
    }

    #[test]
    fn user_folder_nested_under_inbox_stays_with_inbox() {
        let folders = vec![
            folder(1, "INBOX", SpecialUse::Inbox, 0),
            folder(2, "INBOX/Work", SpecialUse::Other, 0),
        ];
        let grouped = build_grouped_folder_tree(&folders, &HashSet::new());
        assert_eq!(names(&grouped.system), ["INBOX", "INBOX/Work"]);
        assert!(grouped.personal.is_empty());
    }

    #[test]
    fn namespace_parent_holding_special_folders_stays_system() {
        let mut gmail = folder(1, "[Gmail]", SpecialUse::Other, 0);
        gmail.selectable = false;
        let folders = vec![
            gmail,
            folder(2, "[Gmail]/Sent Mail", SpecialUse::Sent, 0),
            folder(3, "Receipts", SpecialUse::Other, 0),
        ];
        let grouped = build_grouped_folder_tree(&folders, &HashSet::new());
        assert_eq!(names(&grouped.system), ["[Gmail]", "[Gmail]/Sent Mail"]);
        assert_eq!(names(&grouped.personal), ["Receipts"]);
    }

    #[test]
    fn per_folder_collapse_applies_inside_the_group() {
        let folders = vec![
            folder(1, "Business", SpecialUse::Other, 0),
            folder(2, "Business/Banking", SpecialUse::Other, 0),
        ];
        let collapsed: HashSet<String> = ["Business".to_string()].into();
        let grouped = build_grouped_folder_tree(&folders, &collapsed);
        assert_eq!(names(&grouped.personal), ["Business"]);
        assert!(grouped.personal[0].has_children);
        assert!(!grouped.personal[0].is_expanded);
    }

    #[test]
    fn group_unread_counts_folders_hidden_by_per_folder_collapse() {
        let folders = vec![
            folder(1, "Business", SpecialUse::Other, 2),
            folder(2, "Business/Banking", SpecialUse::Other, 5),
            folder(3, "INBOX", SpecialUse::Inbox, 9),
        ];
        let collapsed: HashSet<String> = ["Business".to_string()].into();
        let grouped = build_grouped_folder_tree(&folders, &collapsed);
        assert_eq!(grouped.personal_unread, 7);
    }

    fn noop_callbacks() -> FolderRowCallbacks {
        FolderRowCallbacks {
            on_select: Rc::new(|_| {}),
            on_toggle: Rc::new(|_, _| {}),
            on_toggle_personal: Rc::new(|_| {}),
            on_context: Rc::new(|_, _, _| {}),
            can_drop: Rc::new(|_, _| false),
            on_drop: Rc::new(|_, _, _| {}),
        }
    }

    #[test]
    fn collapsed_group_expands_when_a_drag_hovers_it() {
        crate::test_helpers::gtk_test_run(|| {
            let account_id = AccountId(Uuid::from_u128(1));
            let callbacks = noop_callbacks();
            let collapsed = personal_header_row(account_id, false, 0, 0, &callbacks);
            let expanded = personal_header_row(account_id, true, 0, 0, &callbacks);
            assert!(expands_on_drag_hover(&collapsed));
            assert!(!expands_on_drag_hover(&expanded));
        });
    }

    #[test]
    fn account_without_user_folders_has_no_group() {
        let folders = vec![folder(1, "INBOX", SpecialUse::Inbox, 0)];
        let grouped = build_grouped_folder_tree(&folders, &HashSet::new());
        assert!(grouped.personal.is_empty());
        assert_eq!(grouped.personal_unread, 0);
    }
}
