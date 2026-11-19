//! Application menus built from a `gio::Menu`.
//!
//! The window uses [`build_primary_menu_button`] (F10), built from
//! [`primary_menu_model`].
//!
//! Menu items reference GIO action names (`app.*` on the application,
//! `win.*` on the active window). The integration layer in `app.rs` must
//! register matching [`gio::SimpleAction`]s and wire them to [`AppMsg`]
//! variants (`Compose`, `AddAccount`, etc.).
//!
//! ## Stateful preview toggle (`app.toggle-preview`)
//!
//! The View menu item uses action `app.toggle-preview`. Register a
//! stateful [`gio::SimpleAction`] named `toggle-preview` on the
//! application with a `bool` state, then bind its `state` property to the
//! `show-message-preview` GSettings key (see [`crate::settings`]) so the
//! menu checkmark stays in sync with the list-toolbar toggle and
//! Preferences switch.
//!
//! ## Stateful app rail toggle (`app.toggle-rail`)
//!
//! The View menu item uses action `app.toggle-rail`. Register a stateful
//! [`gio::SimpleAction`] named `toggle-rail` on the application; the
//! `rail-visible` GSettings key and header toggle stay in sync via
//! [`AppMsg::SetRailVisible`].

use alcedo_i18n::{tr, trf, trs};
use relm4::gtk;
use relm4::gtk::gio;
use relm4::gtk::prelude::*;

use alcedo_core::SpecialUse;

/// GIO action for opening the composer (`AppMsg::Compose`).
pub const ACTION_COMPOSE: &str = "app.compose";
/// GIO action for the Add Account dialog (`AppMsg::AddAccount`).
pub const ACTION_ADD_ACCOUNT: &str = "app.add-account";
/// GIO action for closing the window (`win.close`).
pub const ACTION_CLOSE: &str = "win.close";
/// GIO action for quitting the application (`app.quit`).
pub const ACTION_QUIT: &str = "app.quit";
/// GIO action for presenting the main window (`app.show-window`).
pub const ACTION_SHOW_WINDOW: &str = "app.show-window";
/// GIO action for opening a message from a notification (`app.open-message`).
pub const ACTION_OPEN_MESSAGE: &str = "app.open-message";
/// GIO action for copying a verification code from a notification (`app.copy-verification-code`).
pub const ACTION_COPY_VERIFICATION_CODE: &str = "app.copy-verification-code";
/// GIO action for marking a notified message read (`app.mark-notified-message-read`).
pub const ACTION_MARK_NOTIFIED_MESSAGE_READ: &str = "app.mark-notified-message-read";
/// GIO action for deleting a notified message (`app.delete-notified-message`).
pub const ACTION_DELETE_NOTIFIED_MESSAGE: &str = "app.delete-notified-message";
/// GIO action for opening Preferences (`app.preferences`).
pub const ACTION_PREFERENCES: &str = "app.preferences";
/// GIO action for opening Preferences on the Composing page (`app.preferences-composing`).
pub const ACTION_PREFERENCES_COMPOSING: &str = "app.preferences-composing";
/// GIO action for opening the Contacts canvas (`app.contacts` → `AppMsg::OpenContacts`, same as the rail).
pub const ACTION_CONTACTS: &str = "app.contacts";
/// Stateful GIO action toggling message previews (`show-message-preview`).
pub const ACTION_TOGGLE_PREVIEW: &str = "app.toggle-preview";
/// Stateful GIO action toggling conversation list mode (`message-list-mode`).
pub const ACTION_MESSAGE_LIST_MODE: &str = "app.message-list-mode";
/// Stateful GIO action toggling app rail visibility (`rail-visible`).
pub const ACTION_TOGGLE_RAIL: &str = "app.toggle-rail";
/// GIO action for the About dialog (`app.about`).
pub const ACTION_ABOUT: &str = "app.about";
/// GIO action for Help contents (`app.help`).
pub const ACTION_HELP: &str = "app.help";
/// GIO action for the shortcuts dialog (`app.keyboard-shortcuts`).
pub const ACTION_KEYBOARD_SHORTCUTS: &str = "app.keyboard-shortcuts";
/// GIO action for a help topic (`app.help-topic` with a section id).
pub const ACTION_HELP_TOPIC: &str = "app.help-topic";
/// GIO action for selecting all loaded messages (`win.mail-select-all`).
pub const ACTION_SELECT_ALL: &str = "win.mail-select-all";
/// GIO action for marking the selection read (`win.mail-mark-read`).
pub const ACTION_MARK_READ: &str = "win.mail-mark-read";
/// GIO action for marking the selection unread (`win.mail-mark-unread`).
pub const ACTION_MARK_UNREAD: &str = "win.mail-mark-unread";
/// GIO action for starring the selection (`win.mail-star`).
pub const ACTION_STAR: &str = "win.mail-star";
/// GIO action for unstarring the selection (`win.mail-unstar`).
pub const ACTION_UNSTAR: &str = "win.mail-unstar";
/// GIO action for deleting the selection (`win.mail-delete`).
pub const ACTION_DELETE: &str = "win.mail-delete";
/// GIO action for permanently deleting the selection (`win.mail-delete-permanently`).
pub const ACTION_DELETE_PERMANENTLY: &str = "win.mail-delete-permanently";
/// GIO action for archiving the selection (`win.mail-archive`).
pub const ACTION_ARCHIVE: &str = "win.mail-archive";
/// GIO action for Reply in a detached composer (`AppMsg::ReplyInWindow`).
pub const ACTION_REPLY_WINDOW: &str = "win.mail-reply-window";
/// GIO action for Reply All in a detached composer (`AppMsg::ReplyAllInWindow`).
pub const ACTION_REPLY_ALL_WINDOW: &str = "win.mail-reply-all-window";
/// GIO action for Reply in the reader pane (`AppMsg::Reply`).
pub const ACTION_REPLY: &str = "win.mail-reply";
/// GIO action for Reply All in the reader pane (`AppMsg::ReplyAll`).
pub const ACTION_REPLY_ALL: &str = "win.mail-reply-all";
/// GIO action for Forward in the reader pane (`AppMsg::Forward`).
pub const ACTION_FORWARD: &str = "win.mail-forward";
/// GIO action for Forward in a detached composer (`AppMsg::ForwardInWindow`).
pub const ACTION_FORWARD_WINDOW: &str = "win.mail-forward-window";
/// GIO action for marking the selection as spam (`win.mail-mark-spam`).
pub const ACTION_MARK_SPAM: &str = "win.mail-mark-spam";
/// GIO action for marking the selection as not spam (`win.mail-mark-not-spam`).
pub const ACTION_MARK_NOT_SPAM: &str = "win.mail-mark-not-spam";
/// GIO action for choosing a folder to move messages into (`win.mail-choose-move-folder`).
pub const ACTION_CHOOSE_MOVE_FOLDER: &str = "win.mail-choose-move-folder";
/// GIO action for choosing a folder to copy messages into (`win.mail-choose-copy-folder`).
pub const ACTION_CHOOSE_COPY_FOLDER: &str = "win.mail-choose-copy-folder";
/// GIO action for syncing the current folder (`win.mail-sync-folder`).
pub const ACTION_SYNC_FOLDER: &str = "win.mail-sync-folder";
pub const ACTION_ACTIVITY_LOG: &str = "win.activity-log";
/// GIO action for syncing a folder from the sidebar context menu.
pub const ACTION_FOLDER_CTX_SYNC: &str = "win.folder-ctx-sync";
/// GIO action for marking all messages in a folder read from the sidebar context menu.
pub const ACTION_FOLDER_CTX_MARK_READ: &str = "win.folder-ctx-mark-read";
/// GIO action for searching within a folder from the sidebar context menu.
pub const ACTION_FOLDER_CTX_SEARCH: &str = "win.folder-ctx-search";
/// GIO action for expanding or collapsing a folder tree row from the sidebar context menu.
pub const ACTION_FOLDER_CTX_TOGGLE_EXPAND: &str = "win.folder-ctx-toggle-expand";
/// GIO action for emptying a folder from the sidebar context menu.
pub const ACTION_FOLDER_CTX_EMPTY: &str = "win.folder-ctx-empty";
/// GIO action for deleting messages on or before a date from the sidebar context menu.
pub const ACTION_FOLDER_CTX_DELETE_ON_OR_BEFORE: &str = "win.folder-ctx-delete-on-or-before";
/// GIO action for creating a folder from the File menu (`AppMsg::NewFolder`).
pub const ACTION_NEW_FOLDER: &str = "win.mail-new-folder";
/// GIO action for editing a folder from the File menu (`AppMsg::EditFolder`).
pub const ACTION_EDIT_FOLDER: &str = "win.mail-edit-folder";
/// GIO action for creating a folder from the sidebar context menu.
pub const ACTION_FOLDER_CTX_NEW: &str = "win.folder-ctx-new";
pub const ACTION_FOLDER_CTX_ACCOUNT_SETTINGS: &str = "win.folder-ctx-account-settings";
/// GIO action that hides or shows an account in the stacked folder pane.
pub const ACTION_FOLDER_CTX_TOGGLE_ACCOUNT_HIDDEN: &str = "win.folder-ctx-toggle-account-hidden";
/// GIO action for renaming a folder from the sidebar context menu.
pub const ACTION_FOLDER_CTX_RENAME: &str = "win.folder-ctx-rename";
/// GIO action for deleting a folder from the sidebar context menu.
pub const ACTION_FOLDER_CTX_DELETE: &str = "win.folder-ctx-delete";
/// GIO action for opening message rules (`win.mail-message-rules`).
pub const ACTION_MESSAGE_RULES: &str = "win.mail-message-rules";
/// GIO action for creating a rule from a message (`win.mail-create-rule`).
pub const ACTION_CREATE_RULE: &str = "win.mail-create-rule";
/// GIO action for opening the command palette (`app.command-palette`).
pub const ACTION_COMMAND_PALETTE: &str = "app.command-palette";
/// GIO action for showing the Mail space (`app.show-mail`).
pub const ACTION_SHOW_MAIL: &str = "app.show-mail";
/// GIO action for selecting Unified Inbox (`win.mail-go-unified`).
pub const ACTION_GO_UNIFIED: &str = "win.mail-go-unified";

/// Which folder-bar row opened the context menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FolderMenuKind {
    Unified,
    Selectable,
    Inert,
    /// Empty space in the folder pane (create at account root).
    AccountRoot,
    /// Account header row in the stacked folder pane.
    AccountHeader,
    /// Account header row inside the expanded "Hidden Accounts" area.
    HiddenAccountHeader,
}

/// Inputs for building a folder sidebar context menu.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FolderMenuSpec {
    pub kind: FolderMenuKind,
    pub special: SpecialUse,
    pub unread: u32,
    pub total: u32,
    /// `Some(is_expanded)` when the row has child folders.
    pub expand: Option<bool>,
    /// Sidebar label for the folder row (empty when the menu has no empty action).
    pub display_name: String,
}

fn folder_crud_section(spec: &FolderMenuSpec) -> Vec<(String, &'static str)> {
    match spec.kind {
        FolderMenuKind::Selectable if spec.special == SpecialUse::Other => vec![
            (tr!("_New Folder…"), ACTION_FOLDER_CTX_NEW),
            ("_Rename…".to_string(), ACTION_FOLDER_CTX_RENAME),
            ("_Delete…".to_string(), ACTION_FOLDER_CTX_DELETE),
        ],
        FolderMenuKind::Selectable => vec![(tr!("_New Folder…"), ACTION_FOLDER_CTX_NEW)],
        FolderMenuKind::Unified
        | FolderMenuKind::Inert
        | FolderMenuKind::AccountRoot
        | FolderMenuKind::AccountHeader
        | FolderMenuKind::HiddenAccountHeader => {
            vec![(tr!("_New Folder…"), ACTION_FOLDER_CTX_NEW)]
        }
    }
}

/// Context-menu sections for a folder sidebar row.
pub fn folder_menu_entries(spec: &FolderMenuSpec) -> Vec<Vec<(String, &'static str)>> {
    let mut sections = Vec::new();
    sections.push(folder_crud_section(spec));

    match spec.kind {
        FolderMenuKind::Unified => {
            sections.push(vec![(tr!("_Sync Unified Inbox"), ACTION_FOLDER_CTX_SYNC)]);
            sections.push(vec![
                (tr!("Mark All as _Read"), ACTION_FOLDER_CTX_MARK_READ),
                (tr!("_Search Unified Inbox"), ACTION_FOLDER_CTX_SEARCH),
            ]);
        }
        FolderMenuKind::Selectable => {
            sections.push(vec![(tr!("_Sync This Folder"), ACTION_FOLDER_CTX_SYNC)]);
            sections.push(vec![
                (tr!("Mark All as _Read"), ACTION_FOLDER_CTX_MARK_READ),
                (tr!("_Search This Folder"), ACTION_FOLDER_CTX_SEARCH),
            ]);
            // TRANSLATORS: menu item; {folder} is a folder name such as "Trash". Keep one underscore for the mnemonic.
            let empty_label = trf!("E_mpty {folder}", folder = spec.display_name);
            sections.push(vec![
                (empty_label, ACTION_FOLDER_CTX_EMPTY),
                (
                    tr!("_Delete All From Date…"),
                    ACTION_FOLDER_CTX_DELETE_ON_OR_BEFORE,
                ),
            ]);
            if let Some(expanded) = spec.expand {
                let label = if expanded {
                    "_Collapse".to_string()
                } else {
                    "_Expand".to_string()
                };
                sections.push(vec![(label, ACTION_FOLDER_CTX_TOGGLE_EXPAND)]);
            }
        }
        FolderMenuKind::Inert => {
            if let Some(expanded) = spec.expand {
                let label = if expanded {
                    "_Collapse".to_string()
                } else {
                    "_Expand".to_string()
                };
                sections.push(vec![(label, ACTION_FOLDER_CTX_TOGGLE_EXPAND)]);
            }
        }
        FolderMenuKind::AccountHeader => sections.push(vec![
            (
                tr!("Account _Settings…"),
                ACTION_FOLDER_CTX_ACCOUNT_SETTINGS,
            ),
            (
                tr!("_Hide Account"),
                ACTION_FOLDER_CTX_TOGGLE_ACCOUNT_HIDDEN,
            ),
        ]),
        FolderMenuKind::HiddenAccountHeader => sections.push(vec![
            (
                tr!("Account _Settings…"),
                ACTION_FOLDER_CTX_ACCOUNT_SETTINGS,
            ),
            (
                tr!("Un_hide Account"),
                ACTION_FOLDER_CTX_TOGGLE_ACCOUNT_HIDDEN,
            ),
        ]),
        FolderMenuKind::AccountRoot => {}
    }

    sections.retain(|section| !section.is_empty());
    sections
}

/// A section with each extension message action, if there are any.
fn append_extension_actions(menu: &gio::Menu) {
    let extension_items: Vec<(&str, String)> = crate::extension::message_actions()
        .iter()
        .map(|action| (action.label, format!("win.{}", action.action_name)))
        .collect();
    if extension_items.is_empty() {
        return;
    }
    let items: Vec<(&str, &str)> = extension_items
        .iter()
        .map(|(label, action)| (*label, action.as_str()))
        .collect();
    append_section(menu, &items);
}

fn message_menu() -> gio::Menu {
    let message = gio::Menu::new();
    append_section(&message, &[(trs!("Select _All"), ACTION_SELECT_ALL)]);
    append_section(
        &message,
        &[
            ("_Reply", ACTION_REPLY),
            (trs!("Reply _All"), ACTION_REPLY_ALL),
            ("_Forward", ACTION_FORWARD),
        ],
    );
    append_extension_actions(&message);
    append_section(
        &message,
        &[
            (trs!("Mark as _Read"), ACTION_MARK_READ),
            (trs!("Mark as _Unread"), ACTION_MARK_UNREAD),
            ("_Star", ACTION_STAR),
            ("_Unstar", ACTION_UNSTAR),
        ],
    );
    append_section(
        &message,
        &[
            (trs!("Mark as _Spam"), ACTION_MARK_SPAM),
            (trs!("Mark as _Not Spam"), ACTION_MARK_NOT_SPAM),
        ],
    );
    append_section(
        &message,
        &[
            (trs!("_Move to Folder…"), ACTION_CHOOSE_MOVE_FOLDER),
            (trs!("_Copy to Folder…"), ACTION_CHOOSE_COPY_FOLDER),
            (trs!("_Create Rule…"), ACTION_CREATE_RULE),
        ],
    );
    append_section(
        &message,
        &[
            ("_Archive", ACTION_ARCHIVE),
            ("_Delete", ACTION_DELETE),
            (trs!("Delete _Permanently…"), ACTION_DELETE_PERMANENTLY),
        ],
    );
    message
}

fn view_menu() -> gio::Menu {
    let view = gio::Menu::new();
    append_section(
        &view,
        &[
            (trs!("_Command Palette…"), ACTION_COMMAND_PALETTE),
            (trs!("_Sync Folder"), ACTION_SYNC_FOLDER),
            (trs!("Activity _Log"), ACTION_ACTIVITY_LOG),
        ],
    );
    append_section(
        &view,
        &[
            (trs!("Show App _Rail"), ACTION_TOGGLE_RAIL),
            (trs!("Show Message _Previews"), ACTION_TOGGLE_PREVIEW),
            (trs!("Group by _Conversation"), ACTION_MESSAGE_LIST_MODE),
        ],
    );
    view
}

/// Context menu and bulk-action items (no Select All).
pub fn mail_selection_menu() -> gio::Menu {
    let menu = gio::Menu::new();
    append_section(
        &menu,
        &[
            ("_Reply", ACTION_REPLY_WINDOW),
            (trs!("Reply _All"), ACTION_REPLY_ALL_WINDOW),
            ("_Forward", ACTION_FORWARD_WINDOW),
        ],
    );
    append_extension_actions(&menu);
    append_section(
        &menu,
        &[
            (trs!("Mark as _Read"), ACTION_MARK_READ),
            (trs!("Mark as _Unread"), ACTION_MARK_UNREAD),
            ("_Star", ACTION_STAR),
            ("_Unstar", ACTION_UNSTAR),
        ],
    );
    append_section(
        &menu,
        &[
            (trs!("Mark as _Spam"), ACTION_MARK_SPAM),
            (trs!("Mark as _Not Spam"), ACTION_MARK_NOT_SPAM),
        ],
    );
    append_section(
        &menu,
        &[
            (trs!("_Move to Folder…"), ACTION_CHOOSE_MOVE_FOLDER),
            (trs!("_Copy to Folder…"), ACTION_CHOOSE_COPY_FOLDER),
            (trs!("_Create Rule…"), ACTION_CREATE_RULE),
        ],
    );
    append_section(
        &menu,
        &[
            ("_Archive", ACTION_ARCHIVE),
            ("_Delete", ACTION_DELETE),
            (trs!("Delete _Permanently…"), ACTION_DELETE_PERMANENTLY),
        ],
    );
    menu
}

/// Group items into a `gio::Menu` section so GTK draws a separator, not a blank row.
fn append_section(parent: &gio::Menu, items: &[(&str, &str)]) {
    let section = gio::Menu::new();
    for (label, action) in items {
        section.append(Some(*label), Some(*action));
    }
    parent.append_section(None, &section);
}

/// Hamburger model: theme, Go to, Message, View, then app actions.
pub fn primary_menu_model() -> gio::Menu {
    let menu = gio::Menu::new();

    let theme = gio::Menu::new();
    let theme_item = gio::MenuItem::new(None, None);
    let custom = relm4::gtk::glib::Variant::from("theme");
    theme_item.set_attribute_value("custom", Some(&custom));
    theme.append_item(&theme_item);
    menu.append_section(None, &theme);

    let go = gio::Menu::new();
    go.append(Some(&tr!("Mail")), Some(ACTION_SHOW_MAIL));
    for space in crate::extension::space_entries() {
        go.append(Some(space.title), Some(space.action));
    }
    go.append(Some(&tr!("Contacts")), Some(ACTION_CONTACTS));
    menu.append_section(Some(&tr!("Go To")), &go);

    menu.append_submenu(Some(&tr!("Message")), &message_menu());
    menu.append_submenu(Some(&tr!("View")), &view_menu());

    append_section(
        &menu,
        &[
            (trs!("New Folder…"), ACTION_NEW_FOLDER),
            (trs!("Edit Folder…"), ACTION_EDIT_FOLDER),
            (trs!("Add Account…"), ACTION_ADD_ACCOUNT),
        ],
    );

    let app = gio::Menu::new();
    app.append(Some(&tr!("Preferences")), Some(ACTION_PREFERENCES));
    app.append(Some(&tr!("Rules")), Some(ACTION_MESSAGE_RULES));
    app.append(
        Some(&tr!("Keyboard Shortcuts")),
        Some(ACTION_KEYBOARD_SHORTCUTS),
    );
    app.append(Some(&tr!("Help")), Some(ACTION_HELP));
    app.append(Some(&tr!("About")), Some(ACTION_ABOUT));
    app.append(Some(&tr!("Quit")), Some(ACTION_QUIT));
    menu.append_section(None, &app);

    menu
}

/// Primary menu button. `set_primary` makes F10 open it.
pub fn build_primary_menu_button() -> gtk::MenuButton {
    let popover = gtk::PopoverMenu::from_model(Some(&primary_menu_model()));
    let scheme = crate::theme_switcher::build_scheme_toggle();
    let theme_row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    theme_row.set_margin_top(6);
    theme_row.set_margin_bottom(6);
    theme_row.set_margin_start(12);
    theme_row.set_margin_end(12);
    theme_row.append(&scheme);
    popover.add_child(&theme_row, "theme");

    let button = gtk::MenuButton::builder()
        .icon_name("open-menu-symbolic")
        .build();
    button.set_popover(Some(&popover));
    button.set_primary(true);
    crate::a11y::name_icon_control(
        &button,
        &tr!("Main Menu"),
        &crate::commands::tooltip_with_shortcut(&tr!("Main Menu"), Some("F10")),
    );
    button
}

/// Install application accelerators from the command catalog and user overrides.
///
/// Ctrl+A stays on the window ShortcutController so text fields still select text.
pub fn install_accels(app: &gtk::Application) {
    use crate::commands::{ShortcutScope, command_catalog, effective_accel};

    for entry in command_catalog() {
        if entry.scope != ShortcutScope::AppAccel {
            continue;
        }
        let Some(gio_action) = entry.gio_action else {
            continue;
        };
        let Some(trigger) = effective_accel(entry.id) else {
            continue;
        };
        app.set_accels_for_action(gio_action, &[trigger.as_str()]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn menu_action_names(model: &impl IsA<gio::MenuModel>) -> Vec<String> {
        let model = model.as_ref();
        let mut names = Vec::new();
        let n = model.n_items();
        for i in 0..n {
            if let Some(action) = model.item_attribute_value(i, gio::MENU_ATTRIBUTE_ACTION, None)
                && let Some(name) = action.get::<String>()
            {
                names.push(name);
            }
            if let Some(section) = model.item_link(i, gio::MENU_LINK_SECTION) {
                names.extend(menu_action_names(&section));
            }
            if let Some(submenu) = model.item_link(i, gio::MENU_LINK_SUBMENU) {
                names.extend(menu_action_names(&submenu));
            }
        }
        names
    }

    fn menu_item_labels(model: &impl IsA<gio::MenuModel>) -> Vec<String> {
        let model = model.as_ref();
        let mut labels = Vec::new();
        let n = model.n_items();
        for i in 0..n {
            if let Some(label) = model.item_attribute_value(i, gio::MENU_ATTRIBUTE_LABEL, None)
                && let Some(label) = label.get::<String>()
            {
                labels.push(label.replace('_', ""));
            }
            if let Some(section) = model.item_link(i, gio::MENU_LINK_SECTION) {
                labels.extend(menu_item_labels(&section));
            }
            if let Some(submenu) = model.item_link(i, gio::MENU_LINK_SUBMENU) {
                labels.extend(menu_item_labels(&submenu));
            }
        }
        labels
    }

    /// Header capitalization: every word starts with a capital except short
    /// articles, coordinating conjunctions, and short prepositions. The first
    /// and last words stay capitalized.
    fn header_capitalized(label: &str) -> bool {
        const MINOR: &[&str] = &[
            "a", "an", "the", "and", "but", "or", "nor", "as", "at", "by", "for", "in", "of", "on",
            "to", "up", "via", "vs",
        ];
        let words: Vec<&str> = label.split_whitespace().collect();
        if words.is_empty() {
            return false;
        }
        let last = words.len() - 1;
        words.iter().enumerate().all(|(index, word)| {
            let bare = word.trim_end_matches(['…', '.', '?', '!']);
            if bare.is_empty() {
                return true;
            }
            let lower = bare.to_lowercase();
            let minor = index != 0 && index != last && MINOR.contains(&lower.as_str());
            if minor {
                bare.chars().all(|ch| !ch.is_uppercase())
            } else {
                bare.chars().next().is_some_and(|ch| ch.is_uppercase())
            }
        })
    }

    fn labels(sections: &[Vec<(String, &'static str)>]) -> Vec<String> {
        sections
            .iter()
            .flat_map(|section| section.iter().map(|(label, _)| label.replace('_', "")))
            .collect()
    }

    #[test]
    fn folder_menu_inbox_has_new_not_rename_delete() {
        let entries = folder_menu_entries(&FolderMenuSpec {
            kind: FolderMenuKind::Selectable,
            special: SpecialUse::Inbox,
            unread: 3,
            total: 10,
            expand: None,
            display_name: "Inbox".to_string(),
        });
        let labels = labels(&entries);
        assert!(labels.iter().any(|label| label.contains("New Folder")));
        assert!(!labels.iter().any(|label| label.contains("Rename")));
        assert!(!labels.iter().any(|label| label == "Delete…"));
    }

    #[test]
    fn folder_menu_inbox_has_sync_mark_search_empty_folder() {
        let entries = folder_menu_entries(&FolderMenuSpec {
            kind: FolderMenuKind::Selectable,
            special: SpecialUse::Inbox,
            unread: 3,
            total: 10,
            expand: None,
            display_name: "Inbox".to_string(),
        });
        let labels = labels(&entries);
        assert!(
            labels
                .iter()
                .any(|label| label.contains("Sync This Folder"))
        );
        assert!(labels.iter().any(|label| label.contains("Mark All as")));
        assert!(
            labels
                .iter()
                .any(|label| label.contains("Search This Folder"))
        );
        assert!(labels.iter().any(|label| label == "Empty Inbox"));
        assert!(!labels.iter().any(|label| label.contains("Empty Junk")));
        assert!(!labels.iter().any(|label| label.contains("Empty Trash")));
        assert!(
            labels
                .iter()
                .any(|label| label.contains("Delete All From Date"))
        );
        assert!(entries.iter().any(|section| {
            let section_labels: Vec<String> = section
                .iter()
                .map(|(label, _)| label.replace('_', ""))
                .collect();
            section_labels.iter().any(|label| label == "Empty Inbox")
                && section_labels
                    .iter()
                    .any(|label| label.contains("Delete All From Date"))
        }));
    }

    #[test]
    fn folder_menu_other_selectable_has_new_rename_delete() {
        let entries = folder_menu_entries(&FolderMenuSpec {
            kind: FolderMenuKind::Selectable,
            special: SpecialUse::Other,
            unread: 0,
            total: 0,
            expand: None,
            display_name: "Work".to_string(),
        });
        let labels = labels(&entries);
        assert!(labels.iter().any(|label| label.contains("New Folder")));
        assert!(labels.iter().any(|label| label.contains("Rename")));
        assert!(labels.iter().any(|label| label.contains("Delete")));
        assert!(labels.iter().any(|label| label == "Empty Work"));
        assert!(
            labels
                .iter()
                .any(|label| label.contains("Delete All From Date"))
        );
    }

    #[test]
    fn folder_menu_trash_has_empty_trash_not_junk() {
        let entries = folder_menu_entries(&FolderMenuSpec {
            kind: FolderMenuKind::Selectable,
            special: SpecialUse::Trash,
            unread: 0,
            total: 0,
            expand: None,
            display_name: "Trash".to_string(),
        });
        let labels = labels(&entries);
        assert!(labels.iter().any(|label| label.contains("New Folder")));
        assert!(!labels.iter().any(|label| label.contains("Rename")));
        assert!(labels.iter().any(|label| label.contains("Empty Trash")));
        assert!(!labels.iter().any(|label| label.contains("Empty Junk")));
        assert!(!labels.iter().any(|label| label == "Empty folder"));
        assert!(
            labels
                .iter()
                .any(|label| label.contains("Delete All From Date"))
        );
    }

    #[test]
    fn folder_menu_junk_has_empty_junk() {
        let entries = folder_menu_entries(&FolderMenuSpec {
            kind: FolderMenuKind::Selectable,
            special: SpecialUse::Junk,
            unread: 0,
            total: 0,
            expand: None,
            display_name: "Junk".to_string(),
        });
        let labels = labels(&entries);
        assert!(labels.iter().any(|label| label.contains("Empty Junk")));
        assert!(!labels.iter().any(|label| label == "Empty folder"));
        assert!(
            labels
                .iter()
                .any(|label| label.contains("Delete All From Date"))
        );
    }

    #[test]
    fn folder_menu_inert_parent_has_new_and_expand() {
        let entries = folder_menu_entries(&FolderMenuSpec {
            kind: FolderMenuKind::Inert,
            special: SpecialUse::Other,
            unread: 0,
            total: 0,
            expand: Some(false),
            display_name: String::new(),
        });
        let labels = labels(&entries);
        assert!(labels.iter().any(|label| label.contains("New Folder")));
        assert!(labels.iter().any(|label| label == "Expand"));
        assert!(
            !labels
                .iter()
                .any(|label| label.contains("Delete All From Date"))
        );
    }

    #[test]
    fn folder_menu_inert_leaf_has_new_only() {
        let entries = folder_menu_entries(&FolderMenuSpec {
            kind: FolderMenuKind::Inert,
            special: SpecialUse::Other,
            unread: 0,
            total: 0,
            expand: None,
            display_name: String::new(),
        });
        let labels = labels(&entries);
        assert_eq!(labels, vec!["New Folder…".to_string()]);
        assert!(
            !labels
                .iter()
                .any(|label| label.contains("Delete All From Date"))
        );
    }

    #[test]
    fn folder_menu_account_root_has_new_only() {
        let entries = folder_menu_entries(&FolderMenuSpec {
            kind: FolderMenuKind::AccountRoot,
            special: SpecialUse::Other,
            unread: 0,
            total: 0,
            expand: None,
            display_name: String::new(),
        });
        let labels = labels(&entries);
        assert_eq!(labels, vec!["New Folder…".to_string()]);
        assert!(!labels.iter().any(|label| label.contains("Sync")));
        assert!(!labels.iter().any(|label| label.contains("Search")));
        assert!(!labels.iter().any(|label| label.contains("Rename")));
    }

    #[test]
    fn account_header_menu_has_new_folder_and_settings() {
        let entries = folder_menu_entries(&FolderMenuSpec {
            kind: FolderMenuKind::AccountHeader,
            special: SpecialUse::Other,
            unread: 0,
            total: 0,
            expand: None,
            display_name: String::new(),
        });
        assert_eq!(
            entries,
            vec![
                vec![("_New Folder…".to_string(), ACTION_FOLDER_CTX_NEW,)],
                vec![
                    (
                        "Account _Settings…".to_string(),
                        ACTION_FOLDER_CTX_ACCOUNT_SETTINGS,
                    ),
                    (
                        "_Hide Account".to_string(),
                        ACTION_FOLDER_CTX_TOGGLE_ACCOUNT_HIDDEN,
                    ),
                ],
            ]
        );
    }

    #[test]
    fn hidden_account_header_menu_offers_unhide() {
        let entries = folder_menu_entries(&FolderMenuSpec {
            kind: FolderMenuKind::HiddenAccountHeader,
            special: SpecialUse::Other,
            unread: 0,
            total: 0,
            expand: None,
            display_name: String::new(),
        });
        let labels = labels(&entries);
        assert!(labels.iter().any(|label| label == "Unhide Account"));
        assert!(!labels.iter().any(|label| label == "Hide Account"));
    }

    #[test]
    fn folder_menu_unified_has_sync_unified_no_empty() {
        let entries = folder_menu_entries(&FolderMenuSpec {
            kind: FolderMenuKind::Unified,
            special: SpecialUse::Inbox,
            unread: 2,
            total: 0,
            expand: None,
            display_name: String::new(),
        });
        let labels = labels(&entries);
        assert!(labels.iter().any(|label| label.contains("New Folder")));
        assert!(
            labels
                .iter()
                .any(|label| label.contains("Sync Unified Inbox"))
        );
        assert!(
            labels
                .iter()
                .any(|label| label.contains("Search Unified Inbox"))
        );
        assert!(!labels.iter().any(|label| label.contains("Empty")));
        assert!(
            !labels
                .iter()
                .any(|label| label.contains("Delete All From Date"))
        );
    }

    struct OneAction;

    impl crate::extension::UiExtension for OneAction {
        fn message_actions(&self) -> Vec<crate::extension::MessageAction> {
            vec![crate::extension::MessageAction {
                action_name: "mail-test-action",
                label: "_Test",
                enabled: None,
                activate: std::rc::Rc::new(|_, _| {}),
            }]
        }
    }

    #[test]
    fn extension_message_actions_join_both_message_menus() {
        let action = "win.mail-test-action".to_string();
        assert!(!menu_action_names(&primary_menu_model()).contains(&action));
        assert!(!menu_action_names(&mail_selection_menu()).contains(&action));
        // The registry is per thread, so this does not leak into other tests.
        crate::extension::register(std::rc::Rc::new(OneAction));
        assert!(menu_action_names(&primary_menu_model()).contains(&action));
        assert!(menu_action_names(&mail_selection_menu()).contains(&action));
    }

    #[test]
    fn primary_menu_has_message_rules() {
        let actions = menu_action_names(&primary_menu_model());
        assert!(actions.contains(&ACTION_MESSAGE_RULES.to_string()));
    }

    #[test]
    fn primary_menu_message_submenu_has_create_rule() {
        let actions = menu_action_names(&primary_menu_model());
        assert!(actions.contains(&ACTION_CREATE_RULE.to_string()));
    }

    #[test]
    fn mail_selection_menu_has_create_rule() {
        let actions = menu_action_names(&mail_selection_menu());
        assert!(actions.contains(&ACTION_CREATE_RULE.to_string()));
    }

    #[test]
    fn primary_menu_has_new_and_edit_folder() {
        let actions = menu_action_names(&primary_menu_model());
        assert!(actions.contains(&ACTION_NEW_FOLDER.to_string()));
        assert!(actions.contains(&ACTION_EDIT_FOLDER.to_string()));
    }

    #[test]
    fn primary_menu_view_submenu_has_toggle_rail() {
        let actions = menu_action_names(&primary_menu_model());
        assert!(actions.contains(&ACTION_TOGGLE_RAIL.to_string()));
    }

    #[test]
    fn primary_menu_has_help_before_about() {
        let actions = menu_action_names(&primary_menu_model());
        assert!(actions.contains(&ACTION_HELP.to_string()));
        assert!(actions.contains(&ACTION_ABOUT.to_string()));
        let help_pos = actions
            .iter()
            .position(|action| action == ACTION_HELP)
            .expect("help action");
        let about_pos = actions
            .iter()
            .position(|action| action == ACTION_ABOUT)
            .expect("about action");
        assert!(help_pos < about_pos);
    }

    #[test]
    fn mail_selection_menu_starts_with_reply_window_actions() {
        let actions = menu_action_names(&mail_selection_menu());
        assert_eq!(
            &actions[..3],
            &[
                ACTION_REPLY_WINDOW.to_string(),
                ACTION_REPLY_ALL_WINDOW.to_string(),
                ACTION_FORWARD_WINDOW.to_string(),
            ]
        );
        assert!(actions.contains(&ACTION_MARK_READ.to_string()));
        assert!(actions.contains(&ACTION_CHOOSE_MOVE_FOLDER.to_string()));
        assert!(actions.contains(&ACTION_CHOOSE_COPY_FOLDER.to_string()));
        let delete_pos = actions
            .iter()
            .position(|action| action == ACTION_DELETE)
            .expect("delete action");
        let archive_pos = actions
            .iter()
            .position(|action| action == ACTION_ARCHIVE)
            .expect("archive action");
        let move_pos = actions
            .iter()
            .position(|action| action == ACTION_CHOOSE_MOVE_FOLDER)
            .expect("move action");
        let copy_pos = actions
            .iter()
            .position(|action| action == ACTION_CHOOSE_COPY_FOLDER)
            .expect("copy action");
        assert!(move_pos < delete_pos);
        assert!(copy_pos < delete_pos);
        assert!(archive_pos < delete_pos);
        assert!(move_pos < copy_pos);
        assert!(actions.contains(&ACTION_DELETE.to_string()));
        let permanent_pos = actions
            .iter()
            .position(|action| action == ACTION_DELETE_PERMANENTLY)
            .expect("permanent delete action");
        assert!(delete_pos < permanent_pos);
        assert!(actions.contains(&ACTION_DELETE_PERMANENTLY.to_string()));
    }

    #[test]
    fn primary_menu_message_submenu_has_move_copy_before_delete() {
        let actions = menu_action_names(&primary_menu_model());
        assert!(!actions.contains(&ACTION_REPLY_WINDOW.to_string()));
        assert!(!actions.contains(&ACTION_REPLY_ALL_WINDOW.to_string()));
        assert!(actions.contains(&ACTION_CHOOSE_MOVE_FOLDER.to_string()));
        assert!(actions.contains(&ACTION_CHOOSE_COPY_FOLDER.to_string()));
        let delete_pos = actions
            .iter()
            .position(|action| action == ACTION_DELETE)
            .expect("delete action");
        let archive_pos = actions
            .iter()
            .position(|action| action == ACTION_ARCHIVE)
            .expect("archive action");
        let move_pos = actions
            .iter()
            .position(|action| action == ACTION_CHOOSE_MOVE_FOLDER)
            .expect("move action");
        assert!(move_pos < delete_pos);
        assert!(archive_pos < delete_pos);
        let permanent_pos = actions
            .iter()
            .position(|action| action == ACTION_DELETE_PERMANENTLY)
            .expect("permanent delete action");
        assert!(delete_pos < permanent_pos);
        assert!(actions.contains(&ACTION_DELETE_PERMANENTLY.to_string()));
    }

    #[test]
    fn primary_menu_message_submenu_has_no_reply_window_actions() {
        let actions = menu_action_names(&primary_menu_model());
        assert!(!actions.contains(&ACTION_REPLY_WINDOW.to_string()));
        assert!(!actions.contains(&ACTION_REPLY_ALL_WINDOW.to_string()));
    }

    #[test]
    fn primary_menu_message_submenu_starts_with_in_pane_reply_forward() {
        let actions = menu_action_names(&primary_menu_model());
        let select_all_pos = actions
            .iter()
            .position(|action| action == ACTION_SELECT_ALL)
            .expect("select all action");
        let reply_pos = actions
            .iter()
            .position(|action| action == ACTION_REPLY)
            .expect("reply action");
        let reply_all_pos = actions
            .iter()
            .position(|action| action == ACTION_REPLY_ALL)
            .expect("reply all action");
        let forward_pos = actions
            .iter()
            .position(|action| action == ACTION_FORWARD)
            .expect("forward action");
        let mark_read_pos = actions
            .iter()
            .position(|action| action == ACTION_MARK_READ)
            .expect("mark read action");
        assert!(select_all_pos < reply_pos);
        assert!(reply_pos < reply_all_pos);
        assert!(reply_all_pos < forward_pos);
        assert!(forward_pos < mark_read_pos);
    }

    #[test]
    fn folder_menu_collapsed_parent_label_expand() {
        let entries = folder_menu_entries(&FolderMenuSpec {
            kind: FolderMenuKind::Selectable,
            special: SpecialUse::Other,
            unread: 0,
            total: 0,
            expand: Some(false),
            display_name: "Projects".to_string(),
        });
        let labels = labels(&entries);
        assert!(labels.iter().any(|label| label == "Expand"));
    }

    #[test]
    fn primary_menu_has_go_to_and_shortcuts() {
        let actions = menu_action_names(&primary_menu_model());
        assert!(actions.contains(&ACTION_SHOW_MAIL.to_string()));
        assert!(actions.contains(&ACTION_CONTACTS.to_string()));
        assert!(actions.contains(&ACTION_KEYBOARD_SHORTCUTS.to_string()));
        assert!(actions.contains(&ACTION_PREFERENCES.to_string()));
        assert!(actions.contains(&ACTION_MESSAGE_RULES.to_string()));
        assert!(actions.contains(&ACTION_REPLY.to_string()));
        assert!(actions.contains(&ACTION_TOGGLE_RAIL.to_string()));
        assert!(actions.contains(&ACTION_ADD_ACCOUNT.to_string()));
        assert!(actions.contains(&ACTION_QUIT.to_string()));
    }

    #[test]
    fn menu_labels_use_ellipsis_and_header_case() {
        let mut labels = menu_item_labels(&primary_menu_model());
        labels.extend(menu_item_labels(&mail_selection_menu()));
        assert!(!labels.is_empty());
        for label in labels {
            assert!(!label.contains("..."), "{label}");
            assert!(header_capitalized(&label), "{label}");
        }
    }
}
