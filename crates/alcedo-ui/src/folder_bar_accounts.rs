//! Stacked account sections in the folder pane (Betterbird-style layout).

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use alcedo_core::{Account, AccountId, Folder};
use relm4::adw;
use relm4::adw::prelude::*;
use relm4::gtk;
use relm4::gtk::gdk;
use relm4::gtk::glib;

use crate::account_chip::apply_account_chip;
use crate::folder_bar::{
    FolderRowCallbacks, MailView, attach_drop_expand_when, attach_row_click,
    attach_secondary_click, waiting_row,
};
use crate::folder_bar_personal::{append_grouped_folder_rows, build_grouped_folder_tree};
use crate::icons;
use crate::overflow_tooltip::install_overflow_tooltip;
use crate::settings::FolderPaneAccounts;

const ROW_ACCOUNT_PREFIX: &str = "alcedo-account-";
const ROW_ADD_ACCOUNT: &str = "alcedo-add-account";
const ROW_HIDDEN_ACCOUNTS: &str = "alcedo-hidden-accounts";
const ACCOUNT_DRAG_PREFIX: &str = "alcedo-account:";
const ACCOUNT_HEADER_CHIP_SIZE: i32 = 20;
const HEADER_CLASS: &str = "alcedo-account-header";
const SECTION_START_CLASS: &str = "alcedo-account-section-start";
const ADD_ACCOUNT_CLASS: &str = "alcedo-add-account-row";
const HIDDEN_ACCOUNTS_CLASS: &str = "alcedo-hidden-accounts-row";

/// What the folder pane shows between the cross-account rows and the bottom sections.
pub enum AccountBlock<'a> {
    /// Account picker layout: the current account's folder tree.
    Single {
        folders: &'a [Folder],
        waiting_for_folders: bool,
        collapsed: HashSet<String>,
        personal_expanded: bool,
    },
    /// Stacked layout: one collapsible section per account.
    Stacked {
        sections: Vec<AccountSection<'a>>,
        hidden: HiddenAccounts<'a>,
        actions: AccountHeaderActions,
    },
}

/// Accounts tucked away in a collapsible area below the visible ones.
pub struct HiddenAccounts<'a> {
    pub sections: Vec<AccountSection<'a>>,
    pub expanded: bool,
}

pub struct AccountSection<'a> {
    pub account: &'a Account,
    pub expanded: bool,
    pub folders: &'a [Folder],
    pub collapsed: HashSet<String>,
    pub personal_expanded: bool,
    pub inbox_unread: u32,
    pub needs_sign_in: bool,
}

pub struct AccountHeaderActions {
    pub on_toggle: Rc<dyn Fn(AccountId)>,
    pub on_context: Rc<dyn Fn(AccountId, f64, f64)>,
    pub on_reorder: Rc<dyn Fn(AccountId, AccountId)>,
    pub on_add_account: Rc<dyn Fn()>,
    pub on_toggle_hidden_area: Rc<dyn Fn()>,
}

/// Stacked only pays off with two or more accounts; one account uses the picker.
pub(crate) fn uses_stacked_accounts(mode: FolderPaneAccounts, account_count: usize) -> bool {
    mode == FolderPaneAccounts::Stacked && account_count >= 2
}

/// Saved order first; accounts missing from it follow in their existing order.
pub(crate) fn order_accounts<'a>(accounts: &'a [Account], order: &[AccountId]) -> Vec<&'a Account> {
    let rank: HashMap<AccountId, usize> = order
        .iter()
        .enumerate()
        .map(|(index, id)| (*id, index))
        .collect();
    let mut ordered: Vec<(usize, &Account)> = accounts.iter().enumerate().collect();
    ordered.sort_by_key(|(index, account)| {
        (rank.get(&account.id).copied().unwrap_or(usize::MAX), *index)
    });
    ordered.into_iter().map(|(_, account)| account).collect()
}

/// Move `dragged` to `target`'s position.
pub(crate) fn reorder_account_ids(
    ids: &[AccountId],
    dragged: AccountId,
    target: AccountId,
) -> Option<Vec<AccountId>> {
    if dragged == target {
        return None;
    }
    let mut ids = ids.to_vec();
    let from = ids.iter().position(|id| *id == dragged)?;
    ids.remove(from);
    let to = ids.iter().position(|id| *id == target)?;
    ids.insert(to, dragged);
    Some(ids)
}

pub(crate) fn account_header_widget_name(account_id: AccountId) -> String {
    format!("{ROW_ACCOUNT_PREFIX}{}", account_id.0)
}

pub(crate) fn is_account_pane_row(widget_name: &str) -> bool {
    widget_name.starts_with(ROW_ACCOUNT_PREFIX)
        || widget_name == ROW_ADD_ACCOUNT
        || widget_name == ROW_HIDDEN_ACCOUNTS
}

pub(crate) fn hidden_accounts_label(count: usize) -> String {
    format!("Hidden Accounts ({count})")
}

pub(crate) fn account_header_tooltip(email: &str, needs_sign_in: bool) -> String {
    if needs_sign_in {
        format!("{email}\nSign-in required")
    } else {
        email.to_string()
    }
}

pub(crate) fn account_header_accessible_label(label: &str) -> String {
    format!("{label} account")
}

/// Enter/Space toggle; Left only collapses; Right only expands.
pub(crate) fn header_key_toggles(key: gdk::Key, expanded: bool) -> bool {
    match key {
        gdk::Key::Return | gdk::Key::KP_Enter | gdk::Key::space | gdk::Key::KP_Space => true,
        gdk::Key::Left | gdk::Key::KP_Left => expanded,
        gdk::Key::Right | gdk::Key::KP_Right => !expanded,
        _ => false,
    }
}

/// Visible accounts, then the Hidden Accounts row with its accounts beneath it, go in
/// `scrolling_list` so the hidden area reads as the tail of the account list.
/// Add Account goes in `pinned_list`, outside the scroller.
pub(crate) fn append_account_sections(
    scrolling_list: &gtk::ListBox,
    pinned_list: &gtk::ListBox,
    sections: &[AccountSection<'_>],
    hidden: &HiddenAccounts<'_>,
    actions: &AccountHeaderActions,
    mail_view: MailView,
    syncing: Option<MailView>,
    folder_callbacks: &FolderRowCallbacks,
) {
    for section in sections {
        append_account_section(
            scrolling_list,
            section,
            actions,
            mail_view,
            syncing,
            folder_callbacks,
        );
    }
    pinned_list.append(&add_account_row(actions.on_add_account.clone()));

    if hidden.sections.is_empty() {
        return;
    }
    scrolling_list.append(&hidden_accounts_row(
        hidden,
        actions.on_toggle_hidden_area.clone(),
    ));
    if hidden.expanded {
        for section in &hidden.sections {
            append_account_section(
                scrolling_list,
                section,
                actions,
                mail_view,
                syncing,
                folder_callbacks,
            );
        }
    }
}

fn append_account_section(
    list_box: &gtk::ListBox,
    section: &AccountSection<'_>,
    actions: &AccountHeaderActions,
    mail_view: MailView,
    syncing: Option<MailView>,
    folder_callbacks: &FolderRowCallbacks,
) {
    account_header_row(section, list_box, actions);
    if !section.expanded {
        return;
    }
    if section.folders.is_empty() {
        list_box.append(&waiting_row());
    } else {
        let grouped = build_grouped_folder_tree(section.folders, &section.collapsed);
        append_grouped_folder_rows(
            list_box,
            &grouped,
            section.personal_expanded,
            1,
            mail_view,
            syncing,
            folder_callbacks,
        );
    }
}

fn hidden_accounts_row(hidden: &HiddenAccounts<'_>, on_toggle: Rc<dyn Fn()>) -> gtk::ListBoxRow {
    let expanded = hidden.expanded;
    let row = gtk::ListBoxRow::new();
    row.set_widget_name(ROW_HIDDEN_ACCOUNTS);
    row.set_activatable(false);
    row.set_selectable(false);
    row.set_can_focus(true);
    row.add_css_class(HIDDEN_ACCOUNTS_CLASS);
    row.set_tooltip_text(Some(if expanded {
        "Collapse hidden accounts"
    } else {
        "Show hidden accounts"
    }));
    row.update_property(&[gtk::accessible::Property::Label(&hidden_accounts_label(
        hidden.sections.len(),
    ))]);
    row.update_state(&[gtk::accessible::State::Expanded(Some(expanded))]);

    let expand_icon = if expanded {
        "pan-down-symbolic"
    } else {
        "pan-end-symbolic"
    };
    let expand_btn = gtk::Button::from_icon_name(expand_icon);
    expand_btn.add_css_class("flat");
    expand_btn.add_css_class("alcedo-folder-expand");
    expand_btn.set_has_frame(false);
    expand_btn.set_focus_on_click(false);
    let on_toggle_btn = on_toggle.clone();
    expand_btn.connect_clicked(move |_| on_toggle_btn());

    let icon = icons::sidebar_image("view-conceal-symbolic");
    icon.add_css_class("alcedo-folder-icon");

    let label = gtk::Label::builder()
        .label(hidden_accounts_label(hidden.sections.len()))
        .xalign(0.0)
        .hexpand(true)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .build();
    install_overflow_tooltip(&label);

    let row_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row_box.add_css_class("alcedo-folder-row");
    row_box.set_margin_start(8);
    row_box.set_margin_end(8);
    row_box.append(&expand_btn);
    row_box.append(&icon);
    row_box.append(&label);

    let hidden_unread: u32 = hidden
        .sections
        .iter()
        .map(|section| section.inbox_unread)
        .sum();
    if !expanded && hidden_unread > 0 {
        let unread = gtk::Label::new(Some(&hidden_unread.to_string()));
        unread.add_css_class("alcedo-folder-unread");
        unread.set_halign(gtk::Align::End);
        unread.set_xalign(1.0);
        row_box.append(&unread);
    }

    let on_toggle_click = on_toggle.clone();
    attach_row_click(&row_box, move || on_toggle_click());

    let key_controller = gtk::EventControllerKey::new();
    key_controller.connect_key_pressed(move |_, key, _, _| {
        if header_key_toggles(key, expanded) {
            on_toggle();
            glib::Propagation::Stop
        } else {
            glib::Propagation::Proceed
        }
    });
    row.add_controller(key_controller);
    row.set_child(Some(&row_box));
    row
}

fn account_header_row(
    section: &AccountSection<'_>,
    list_box: &gtk::ListBox,
    actions: &AccountHeaderActions,
) {
    let account = section.account;
    let id = account.id;
    let expanded = section.expanded;
    let needs_sign_in = section.needs_sign_in;
    let inbox_unread = section.inbox_unread;

    let row = gtk::ListBoxRow::new();
    row.set_widget_name(&account_header_widget_name(id));
    row.set_activatable(false);
    row.set_selectable(false);
    row.set_can_focus(true);
    row.add_css_class(HEADER_CLASS);
    row.add_css_class(SECTION_START_CLASS);
    row.set_tooltip_text(Some(&account_header_tooltip(&account.email, needs_sign_in)));
    row.update_property(&[gtk::accessible::Property::Label(
        &account_header_accessible_label(account.label()),
    )]);
    row.update_state(&[gtk::accessible::State::Expanded(Some(expanded))]);

    let expand_icon = if expanded {
        "pan-down-symbolic"
    } else {
        "pan-end-symbolic"
    };
    let expand_btn = gtk::Button::from_icon_name(expand_icon);
    expand_btn.add_css_class("flat");
    expand_btn.add_css_class("alcedo-folder-expand");
    expand_btn.set_has_frame(false);
    expand_btn.set_focus_on_click(false);
    expand_btn.set_tooltip_text(Some(if expanded {
        "Collapse account"
    } else {
        "Expand account"
    }));
    let on_toggle_btn = actions.on_toggle.clone();
    expand_btn.connect_clicked(move |_| on_toggle_btn(id));

    let chip = adw::Avatar::new(ACCOUNT_HEADER_CHIP_SIZE, None, true);
    chip.set_show_initials(true);
    chip.set_can_focus(false);
    chip.set_valign(gtk::Align::Center);
    apply_account_chip(&chip, account);

    let label = gtk::Label::builder()
        .label(account.label())
        .xalign(0.0)
        .hexpand(true)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .build();
    label.add_css_class("heading");
    install_overflow_tooltip(&label);

    let row_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row_box.add_css_class("alcedo-folder-row");
    row_box.set_margin_start(8);
    row_box.set_margin_end(8);
    row_box.set_margin_top(4);
    row_box.set_margin_bottom(4);
    row_box.append(&expand_btn);
    row_box.append(&chip);
    row_box.append(&label);

    if needs_sign_in {
        let warning = gtk::Image::from_icon_name("dialog-warning-symbolic");
        warning.add_css_class("warning");
        warning.set_tooltip_text(Some("Sign-in required"));
        row_box.append(&warning);
    }

    if !expanded && inbox_unread > 0 {
        let unread = gtk::Label::new(Some(&inbox_unread.to_string()));
        unread.add_css_class("alcedo-folder-unread");
        unread.set_halign(gtk::Align::End);
        unread.set_xalign(1.0);
        row_box.append(&unread);
    }

    let on_toggle_click = actions.on_toggle.clone();
    attach_row_click(&row_box, move || on_toggle_click(id));
    let on_context = actions.on_context.clone();
    attach_secondary_click(&row_box, move |x, y| on_context(id, x, y));

    let key_controller = gtk::EventControllerKey::new();
    let on_toggle_key = actions.on_toggle.clone();
    key_controller.connect_key_pressed(move |_, key, _, _| {
        if header_key_toggles(key, expanded) {
            on_toggle_key(id);
            glib::Propagation::Stop
        } else {
            glib::Propagation::Proceed
        }
    });
    row.add_controller(key_controller);

    attach_account_drag_source(&row, id);
    attach_account_drop_target(&row, id, actions.on_reorder.clone());
    // Spring-load collapsed accounts so a dragged message can reach their folders.
    // Account reorder drags pass over headers too and must not open them.
    if !expanded {
        let on_toggle = actions.on_toggle.clone();
        attach_drop_expand_when(
            &row,
            |drop| !is_account_reorder_drag(drop),
            Rc::new(move || on_toggle(id)),
        );
    }

    row.set_child(Some(&row_box));
    list_box.append(&row);
}

fn is_account_reorder_payload(payload: &str) -> bool {
    payload.starts_with(ACCOUNT_DRAG_PREFIX)
}

/// True for an in-app account reorder drag. Reading the payload this way only works
/// for drags started in this process, which account drags always are.
fn is_account_reorder_drag(drop: &gdk::Drop) -> bool {
    drop.drag()
        .and_then(|drag| drag.content().value(glib::Type::STRING).ok())
        .and_then(|value| value.get::<String>().ok())
        .is_some_and(|payload| is_account_reorder_payload(&payload))
}

fn attach_account_drag_source(row: &gtk::ListBoxRow, id: AccountId) {
    let source = gtk::DragSource::new();
    source.set_actions(gdk::DragAction::MOVE);
    source.connect_prepare(move |_, _, _| {
        let payload = format!("{ACCOUNT_DRAG_PREFIX}{}", id.0);
        Some(gdk::ContentProvider::for_value(&payload.to_value()))
    });
    row.add_controller(source);
}

fn attach_account_drop_target(
    row: &gtk::ListBoxRow,
    target_id: AccountId,
    on_reorder: Rc<dyn Fn(AccountId, AccountId)>,
) {
    let drop_target = gtk::DropTarget::new(glib::Type::STRING, gdk::DragAction::MOVE);
    let on_reorder_drop = on_reorder.clone();
    drop_target.connect_enter(|target, _, _| {
        crate::folder_bar::set_drop_highlight(target, true);
        gdk::DragAction::MOVE
    });
    drop_target.connect_motion(|target, _, _| {
        crate::folder_bar::set_drop_highlight(target, true);
        gdk::DragAction::MOVE
    });
    drop_target.connect_leave(|target| crate::folder_bar::set_drop_highlight(target, false));
    drop_target.connect_drop(move |target, value, _, _| {
        crate::folder_bar::set_drop_highlight(target, false);
        let Ok(text) = value.get::<String>() else {
            return false;
        };
        let Some(uuid_str) = text.strip_prefix(ACCOUNT_DRAG_PREFIX) else {
            return false;
        };
        let Ok(uuid) = uuid::Uuid::parse_str(uuid_str) else {
            return false;
        };
        let dragged = AccountId(uuid);
        if dragged == target_id {
            return false;
        }
        on_reorder_drop(dragged, target_id);
        true
    });
    row.add_controller(drop_target);
}

fn add_account_row(on_add: Rc<dyn Fn()>) -> gtk::ListBoxRow {
    let row = gtk::ListBoxRow::new();
    row.set_widget_name(ROW_ADD_ACCOUNT);
    row.set_activatable(false);
    row.set_selectable(false);
    row.set_can_focus(true);
    row.add_css_class(ADD_ACCOUNT_CLASS);
    row.set_tooltip_text(Some("Add another email account"));
    row.update_property(&[gtk::accessible::Property::Label("Add Account")]);

    let icon = icons::sidebar_image("list-add-symbolic");
    icon.add_css_class("alcedo-folder-icon");

    let label = gtk::Label::new(Some("Add Account"));
    label.set_halign(gtk::Align::Start);
    label.set_hexpand(true);

    let row_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row_box.add_css_class("alcedo-folder-row");
    row_box.set_margin_start(8);
    row_box.set_margin_end(8);
    row_box.append(&icon);
    row_box.append(&label);

    let on_add_click = on_add.clone();
    attach_row_click(&row_box, move || on_add_click());

    let key_controller = gtk::EventControllerKey::new();
    key_controller.connect_key_pressed(move |_, key, _, _| match key {
        gdk::Key::Return | gdk::Key::KP_Enter | gdk::Key::space | gdk::Key::KP_Space => {
            on_add();
            glib::Propagation::Stop
        }
        _ => glib::Propagation::Proceed,
    });
    row.add_controller(key_controller);
    row.set_child(Some(&row_box));
    row
}

pub(crate) fn focus_account_header(list_box: &gtk::ListBox, account_id: AccountId) {
    let name = account_header_widget_name(account_id);
    let mut child = list_box.first_child();
    while let Some(widget) = child {
        if widget.widget_name().as_str() == name {
            widget.grab_focus();
            return;
        }
        child = widget.next_sibling();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alcedo_core::{AuthMethod, FolderId, SpecialUse};
    use uuid::Uuid;

    fn sample_account(id: u128, display_name: &str, accent: &str) -> Account {
        Account {
            id: AccountId(Uuid::from_u128(id)),
            email: format!("user{id}@example.com"),
            username: String::new(),
            split_credentials: false,
            smtp_username: String::new(),
            display_name: display_name.to_string(),
            sender_name: String::new(),
            imap_host: String::new(),
            imap_port: 993,
            smtp_host: String::new(),
            smtp_port: 465,
            smtp_starttls: false,
            auth: AuthMethod::Password,
            accent_color: accent.to_string(),
            avatar_text: String::new(),
            avatar_image: Vec::new(),
        }
    }

    fn sample_folder(id: i64, account_id: AccountId, name: &str) -> Folder {
        Folder {
            id: FolderId(id),
            account_id,
            imap_name: name.to_string(),
            delimiter: "/".to_string(),
            selectable: true,
            special: SpecialUse::Other,
            uidvalidity: None,
            uidnext: None,
            highest_modseq: None,
            unread: 0,
            total: 0,
        }
    }

    #[test]
    fn uses_stacked_accounts_needs_two_accounts() {
        assert!(!uses_stacked_accounts(FolderPaneAccounts::Picker, 5));
        assert!(!uses_stacked_accounts(FolderPaneAccounts::Stacked, 0));
        assert!(!uses_stacked_accounts(FolderPaneAccounts::Stacked, 1));
        assert!(uses_stacked_accounts(FolderPaneAccounts::Stacked, 2));
    }

    #[test]
    fn order_accounts_follows_saved_order() {
        let a = sample_account(1, "A", "#111111");
        let b = sample_account(2, "B", "#222222");
        let c = sample_account(3, "C", "#333333");
        let accounts = [a.clone(), b.clone(), c.clone()];
        let order = [c.id, a.id];
        let ordered = order_accounts(&accounts, &order);
        let ids: Vec<AccountId> = ordered.iter().map(|account| account.id).collect();
        assert_eq!(ids, vec![c.id, a.id, b.id]);
    }

    #[test]
    fn order_accounts_appends_unknown_in_existing_order() {
        let a = sample_account(1, "A", "");
        let b = sample_account(2, "B", "");
        let accounts = [a.clone(), b.clone()];
        let ordered = order_accounts(&accounts, &[]);
        let ids: Vec<AccountId> = ordered.iter().map(|account| account.id).collect();
        assert_eq!(ids, vec![a.id, b.id]);
    }

    #[test]
    fn order_accounts_ignores_stale_ids() {
        let a = sample_account(1, "A", "");
        let stale = AccountId(Uuid::from_u128(99));
        let accounts = [a.clone()];
        let ordered = order_accounts(&accounts, &[stale, a.id]);
        let ids: Vec<AccountId> = ordered.iter().map(|account| account.id).collect();
        assert_eq!(ids, vec![a.id]);
    }

    #[test]
    fn reorder_account_ids_moves_down_and_up() {
        let a = AccountId(Uuid::from_u128(1));
        let b = AccountId(Uuid::from_u128(2));
        let c = AccountId(Uuid::from_u128(3));
        let ids = [a, b, c];
        assert_eq!(reorder_account_ids(&ids, c, a), Some(vec![c, a, b]));
        assert_eq!(reorder_account_ids(&ids, a, c), Some(vec![b, a, c]));
    }

    #[test]
    fn reorder_account_ids_same_or_unknown_is_none() {
        let a = AccountId(Uuid::from_u128(1));
        let b = AccountId(Uuid::from_u128(2));
        assert!(reorder_account_ids(&[a, b], a, a).is_none());
        let missing = AccountId(Uuid::from_u128(9));
        assert!(reorder_account_ids(&[a, b], missing, a).is_none());
    }

    #[test]
    fn header_key_toggles_rules() {
        assert!(header_key_toggles(gdk::Key::Return, true));
        assert!(header_key_toggles(gdk::Key::space, false));
        assert!(header_key_toggles(gdk::Key::Left, true));
        assert!(!header_key_toggles(gdk::Key::Left, false));
        assert!(header_key_toggles(gdk::Key::Right, false));
        assert!(!header_key_toggles(gdk::Key::Right, true));
        assert!(!header_key_toggles(gdk::Key::a, true));
    }

    #[test]
    fn account_header_tooltip_mentions_sign_in() {
        assert_eq!(
            account_header_tooltip("a@b.com", true),
            "a@b.com\nSign-in required"
        );
        assert_eq!(account_header_tooltip("a@b.com", false), "a@b.com");
    }

    #[test]
    fn account_header_accessible_label_suffix() {
        assert_eq!(account_header_accessible_label("Work"), "Work account");
    }

    #[test]
    fn is_account_pane_row_matches_headers_and_add_row() {
        let id = AccountId(Uuid::from_u128(1));
        assert!(is_account_pane_row(&account_header_widget_name(id)));
        assert!(is_account_pane_row(ROW_ADD_ACCOUNT));
        assert!(is_account_pane_row(ROW_HIDDEN_ACCOUNTS));
        assert!(!is_account_pane_row("alcedo-folder-1"));
    }

    #[test]
    fn hidden_accounts_label_includes_count() {
        assert_eq!(hidden_accounts_label(2), "Hidden Accounts (2)");
    }

    /// Row widget names of a stacked block with no visible accounts and one hidden account.
    fn rows_with_one_hidden_account(expanded: bool) -> (RowNames, AccountId) {
        let hidden_account = sample_account(7, "Hidden", "#999999");
        let actions = AccountHeaderActions {
            on_toggle: Rc::new(|_| {}),
            on_context: Rc::new(|_, _, _| {}),
            on_reorder: Rc::new(|_, _| {}),
            on_add_account: Rc::new(|| {}),
            on_toggle_hidden_area: Rc::new(|| {}),
        };
        let hidden = HiddenAccounts {
            sections: vec![AccountSection {
                account: &hidden_account,
                expanded: false,
                folders: &[],
                collapsed: HashSet::new(),
                personal_expanded: true,
                inbox_unread: 0,
                needs_sign_in: false,
            }],
            expanded,
        };
        let folder_callbacks = FolderRowCallbacks {
            on_select: Rc::new(|_| {}),
            on_toggle: Rc::new(|_, _| {}),
            on_toggle_personal: Rc::new(|_| {}),
            on_context: Rc::new(|_, _, _| {}),
            can_drop: Rc::new(|_, _| false),
            on_drop: Rc::new(|_, _, _| {}),
        };
        let scrolling_list = gtk::ListBox::new();
        let pinned_list = gtk::ListBox::new();
        append_account_sections(
            &scrolling_list,
            &pinned_list,
            &[],
            &hidden,
            &actions,
            MailView::UnifiedInbox,
            None,
            &folder_callbacks,
        );
        (
            RowNames {
                scrolling: row_names(&scrolling_list),
                pinned: row_names(&pinned_list),
            },
            hidden_account.id,
        )
    }

    struct RowNames {
        scrolling: Vec<String>,
        pinned: Vec<String>,
    }

    fn row_names(list_box: &gtk::ListBox) -> Vec<String> {
        let mut names = Vec::new();
        let mut child = list_box.first_child();
        while let Some(widget) = child {
            names.push(widget.widget_name().to_string());
            child = widget.next_sibling();
        }
        names
    }

    #[test]
    fn hidden_accounts_collapsed_show_only_area_row_in_scroller() {
        crate::test_helpers::gtk_test_run(|| {
            let (rows, _) = rows_with_one_hidden_account(false);
            assert_eq!(rows.scrolling, vec![ROW_HIDDEN_ACCOUNTS.to_string()]);
            assert_eq!(rows.pinned, vec![ROW_ADD_ACCOUNT.to_string()]);
        });
    }

    #[test]
    fn hidden_accounts_expanded_list_beneath_area_row_in_scroller() {
        crate::test_helpers::gtk_test_run(|| {
            let (rows, hidden_id) = rows_with_one_hidden_account(true);
            assert_eq!(
                rows.scrolling,
                vec![
                    ROW_HIDDEN_ACCOUNTS.to_string(),
                    account_header_widget_name(hidden_id)
                ]
            );
            assert_eq!(rows.pinned, vec![ROW_ADD_ACCOUNT.to_string()]);
        });
    }

    #[test]
    fn stacked_sections_build_expected_rows() {
        crate::test_helpers::gtk_test_run(|| {
            let a = sample_account(1, "A", "#3584e4");
            let b = sample_account(2, "B", "#e4e435");
            let c = sample_account(3, "C", "#35e4a4");
            let f1 = sample_folder(10, a.id, "One");
            let f2 = sample_folder(11, a.id, "Two");
            let f1_id = f1.id.0;
            let f2_id = f2.id.0;
            let noop_toggle: Rc<dyn Fn(AccountId)> = Rc::new(|_| {});
            let noop_ctx: Rc<dyn Fn(AccountId, f64, f64)> = Rc::new(|_, _, _| {});
            let noop_reorder: Rc<dyn Fn(AccountId, AccountId)> = Rc::new(|_, _| {});
            let noop_add: Rc<dyn Fn()> = Rc::new(|| {});
            let actions = AccountHeaderActions {
                on_toggle: noop_toggle,
                on_context: noop_ctx,
                on_reorder: noop_reorder,
                on_add_account: noop_add,
                on_toggle_hidden_area: Rc::new(|| {}),
            };
            let sections = [
                AccountSection {
                    account: &a,
                    expanded: true,
                    folders: &[f1, f2],
                    collapsed: HashSet::new(),
                    personal_expanded: true,
                    inbox_unread: 0,
                    needs_sign_in: false,
                },
                AccountSection {
                    account: &b,
                    expanded: false,
                    folders: &[],
                    collapsed: HashSet::new(),
                    personal_expanded: true,
                    inbox_unread: 3,
                    needs_sign_in: false,
                },
                AccountSection {
                    account: &c,
                    expanded: true,
                    folders: &[],
                    collapsed: HashSet::new(),
                    personal_expanded: true,
                    inbox_unread: 0,
                    needs_sign_in: false,
                },
            ];
            let list_box = gtk::ListBox::new();
            let pinned_list = gtk::ListBox::new();
            let folder_callbacks = FolderRowCallbacks {
                on_select: Rc::new(|_| {}),
                on_toggle: Rc::new(|_, _| {}),
                on_toggle_personal: Rc::new(|_| {}),
                on_context: Rc::new(|_, _, _| {}),
                can_drop: Rc::new(|_, _| false),
                on_drop: Rc::new(|_, _, _| {}),
            };
            append_account_sections(
                &list_box,
                &pinned_list,
                &sections,
                &HiddenAccounts {
                    sections: Vec::new(),
                    expanded: false,
                },
                &actions,
                MailView::UnifiedInbox,
                None,
                &folder_callbacks,
            );

            let mut names = Vec::new();
            let mut child = list_box.first_child();
            while let Some(widget) = child {
                names.push(widget.widget_name().to_string());
                child = widget.next_sibling();
            }
            assert_eq!(
                names,
                vec![
                    account_header_widget_name(a.id),
                    crate::folder_bar_personal::personal_header_widget_name(a.id),
                    format!("alcedo-folder-{f1_id}"),
                    format!("alcedo-folder-{f2_id}"),
                    account_header_widget_name(b.id),
                    account_header_widget_name(c.id),
                    "alcedo-waiting".to_string(),
                ]
            );
            assert_eq!(row_names(&pinned_list), vec![ROW_ADD_ACCOUNT.to_string()]);

            let header_a = list_box
                .row_at_index(0)
                .expect("header a")
                .downcast::<gtk::ListBoxRow>()
                .unwrap();
            assert!(!header_a.is_selectable());
            assert!(header_a.has_css_class(SECTION_START_CLASS));

            let header_b = list_box.row_at_index(4).expect("header b");
            let label = header_b
                .child()
                .and_then(|box_| {
                    box_.downcast::<gtk::Box>()
                        .ok()
                        .and_then(|row_box| row_box.last_child())
                })
                .and_then(|w| w.downcast::<gtk::Label>().ok());
            assert_eq!(label.map(|l| l.text().to_string()), Some("3".to_string()));

            // Only the collapsed account opens when a drag hovers it.
            assert!(!crate::test_helpers::expands_on_drag_hover(&header_a));
            assert!(crate::test_helpers::expands_on_drag_hover(&header_b));
        });
    }

    #[test]
    fn account_reorder_payload_is_told_apart_from_message_payloads() {
        let account = format!("{ACCOUNT_DRAG_PREFIX}{}", Uuid::from_u128(1));
        assert!(is_account_reorder_payload(&account));
        assert!(!is_account_reorder_payload("42,43"));
        assert!(!is_account_reorder_payload("alcedo-ext:1"));
    }

    /// Weak refs to every widget under the folder pane lists' rows.
    fn weak_sidebar_widgets(
        lists: &crate::folder_pane_lists::FolderPaneLists,
    ) -> Vec<glib::WeakRef<gtk::Widget>> {
        fn walk(widget: &gtk::Widget, out: &mut Vec<glib::WeakRef<gtk::Widget>>) {
            out.push(widget.downgrade());
            let mut child = widget.first_child();
            while let Some(current) = child {
                walk(&current, out);
                child = current.next_sibling();
            }
        }
        let mut out = Vec::new();
        for list in lists.all() {
            let mut index = 0;
            while let Some(row) = list.row_at_index(index) {
                walk(row.upcast_ref(), &mut out);
                index += 1;
            }
        }
        out
    }

    /// Populate the real sidebar, remove every row, and report whether all of
    /// its widgets and every callback it was handed were freed.
    fn sidebar_is_freed_after_rebuild(stacked: bool) -> bool {
        let lists = crate::folder_pane_lists::FolderPaneLists {
            top: gtk::ListBox::new(),
            middle: gtk::ListBox::new(),
            pinned: gtk::ListBox::new(),
            bottom: gtk::ListBox::new(),
        };
        let first = sample_account(1, "Work", "blue");
        let second = sample_account(2, "Home", "green");
        let hidden_account = sample_account(3, "Old", "red");
        let folders_for = |account: &Account, base: i64| {
            let mut inbox = sample_folder(base, account.id, "INBOX");
            inbox.special = SpecialUse::Inbox;
            vec![
                inbox,
                sample_folder(base + 1, account.id, "Projects"),
                sample_folder(base + 2, account.id, "Projects/Alpha"),
            ]
        };
        let first_folders = folders_for(&first, 10);
        let second_folders = folders_for(&second, 20);
        let hidden_folders = folders_for(&hidden_account, 30);
        let sentinel = Rc::new(());
        let section = |account, folders| AccountSection {
            account,
            expanded: true,
            folders,
            collapsed: HashSet::new(),
            personal_expanded: true,
            inbox_unread: 2,
            needs_sign_in: false,
        };
        let block = if stacked {
            let s = sentinel.clone();
            let actions = AccountHeaderActions {
                on_toggle: Rc::new({
                    let s = s.clone();
                    move |_| drop(s.clone())
                }),
                on_context: Rc::new({
                    let s = s.clone();
                    move |_, _, _| drop(s.clone())
                }),
                on_reorder: Rc::new({
                    let s = s.clone();
                    move |_, _| drop(s.clone())
                }),
                on_add_account: Rc::new({
                    let s = s.clone();
                    move || drop(s.clone())
                }),
                on_toggle_hidden_area: Rc::new(move || drop(s.clone())),
            };
            AccountBlock::Stacked {
                sections: vec![
                    section(&first, first_folders.as_slice()),
                    section(&second, second_folders.as_slice()),
                ],
                hidden: HiddenAccounts {
                    sections: vec![section(&hidden_account, hidden_folders.as_slice())],
                    expanded: true,
                },
                actions,
            }
        } else {
            AccountBlock::Single {
                folders: first_folders.as_slice(),
                waiting_for_folders: false,
                collapsed: HashSet::new(),
                personal_expanded: true,
            }
        };
        macro_rules! cb {
            (($($arg:pat_param),*) => $ret:expr) => {{
                let s = sentinel.clone();
                move |$($arg),*| {
                    let _ = &s;
                    $ret
                }
            }};
        }
        crate::folder_bar::populate_folder_bar(
            &lists,
            block,
            MailView::Folder(FolderId(11)),
            3,
            &[crate::extension::MailViewSpec {
                id: alcedo_core::ExtensionListId(uuid::Uuid::from_u128(9)),
                title: "Extension".into(),
                icon_name: "edit-find-symbolic",
                tooltip: "Extension view".into(),
                unread: 1,
                status_detail: String::new(),
                column_scope: "extension".into(),
                search_query: Some("is:unread".into()),
                section: Some(crate::extension::ViewSection {
                    title: "Saved".into(),
                    collapsed: false,
                }),
                reorderable: true,
                restore_on_start: true,
                inbox_like: false,
                sort_newest_first: true,
                loading: false,
                empty_title: "Nothing here",
                empty_description: None,
                header_toggle: None,
                handles_delete: false,
            }],
            Some(MailView::Folder(FolderId(10))),
            cb!(() => ()),
            cb!((_id) => ()),
            cb!((_a, _b) => ()),
            cb!((_id) => ()),
            cb!((_account, _name) => ()),
            cb!((_account) => ()),
            cb!((_title) => ()),
            cb!((_view, _x, _y) => ()),
            cb!((_dest, _ids) => true),
            cb!((_dest, _ids, _copy) => ()),
        );
        let widgets = weak_sidebar_widgets(&lists);
        assert!(
            widgets.len() > 20,
            "sidebar built only {} widgets",
            widgets.len()
        );
        lists.remove_all_rows();
        let leaked = widgets.iter().filter(|w| w.upgrade().is_some()).count();
        leaked == 0 && Rc::strong_count(&sentinel) == 1
    }

    #[test]
    fn single_account_sidebar_is_freed_after_rebuild() {
        crate::test_helpers::gtk_test_run(|| assert!(sidebar_is_freed_after_rebuild(false)));
    }

    #[test]
    fn stacked_accounts_sidebar_is_freed_after_rebuild() {
        crate::test_helpers::gtk_test_run(|| assert!(sidebar_is_freed_after_rebuild(true)));
    }

    #[test]
    fn account_row_with_drop_target_is_freed_after_removal() {
        crate::test_helpers::gtk_test_run(|| {
            assert!(crate::test_helpers::row_is_freed_after_removal(
                |row, sentinel| {
                    attach_account_drop_target(
                        row,
                        AccountId(Uuid::nil()),
                        Rc::new(move |_, _| {
                            let _ = &sentinel;
                        }),
                    );
                }
            ));
        });
    }
}
