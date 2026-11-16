//! Folder bar (P4).

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::Duration;

use alcedo_core::{Account, AccountId, ExtensionListId, Folder, FolderId, MessageId, SpecialUse};
use relm4::adw;
use relm4::adw::prelude::*;
use relm4::gtk;
use relm4::gtk::gdk;
use relm4::gtk::glib;

use crate::account_chip::apply_account_chip;
use crate::extension::{MailViewSpec, ViewSection};
use crate::folder_bar_accounts::{AccountBlock, append_account_sections};
use crate::folder_bar_personal::{append_grouped_folder_rows, build_grouped_folder_tree};
use crate::folder_pane_lists::FolderPaneLists;
use crate::icons;

use crate::message_list::parse_message_id_payload;
use crate::overflow_tooltip::install_overflow_tooltip;
use crate::rail::set_badge_count;
use crate::settings::LastFolder;

/// Default folder title for the mock unified inbox.
pub const UNIFIED_INBOX_TITLE: &str = "Everything";
pub const EVERYTHING_TITLE: &str = "Everything";
const UNIFIED_INBOX_TOOLTIP: &str = "Unified inbox across accounts";

const ROW_EXTENSION_PREFIX: &str = "alcedo-ext-";
const ROW_UNIFIED: &str = "alcedo-unified";
const ROW_FOLDER_PREFIX: &str = "alcedo-folder-";
const ROW_SECTION_PREFIX: &str = "alcedo-section-";
const EXT_DRAG_PREFIX: &str = "alcedo-ext:";
const ACCOUNT_AVATAR_SIZE: i32 = 32;
/// Hover delay before expanding a collapsed parent during a drag. Matches GTK
/// TreeView's classic `AUTO_EXPAND_TIMEOUT`: stay on the row to open children,
/// pass over it without expanding.
const FOLDER_DROP_EXPAND_MS: u64 = 500;

/// Which mail collection is shown in the message list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MailView {
    UnifiedInbox,
    Folder(FolderId),
    Search,
    /// A list shown by an extension.
    Extension(ExtensionListId),
}

impl MailView {}

/// Planned folder-bar rows for tests and populate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FolderBarSection {
    Extension(ExtensionListId),
    Unified,
    Separator,
    /// Header of an extension view section, by title.
    SectionHeader(String),
    Waiting,
    Folder(FolderId),
}

pub fn folder_bar_sections(
    folders: &[Folder],
    collapsed: &HashSet<String>,
    waiting_for_folders: bool,
    extension_views: &[MailViewSpec],
) -> Vec<FolderBarSection> {
    let mut sections = Vec::new();
    if waiting_for_folders {
        sections.push(FolderBarSection::Waiting);
    }
    sections.push(FolderBarSection::Unified);
    sections.extend(
        extension_views
            .iter()
            .filter(|spec| spec.section.is_none())
            .map(|spec| FolderBarSection::Extension(spec.id)),
    );
    let tree = build_folder_tree(folders, collapsed);
    if !tree.is_empty() {
        sections.push(FolderBarSection::Separator);
        for entry in &tree {
            sections.push(FolderBarSection::Folder(entry.folder.id));
        }
    }
    for (section, specs) in extension_sections(extension_views) {
        sections.push(FolderBarSection::Separator);
        if section.title.is_empty() {
            sections.extend(
                specs
                    .iter()
                    .map(|spec| FolderBarSection::Extension(spec.id)),
            );
            continue;
        }
        sections.push(FolderBarSection::SectionHeader(section.title.clone()));
        if !section.collapsed {
            sections.extend(
                specs
                    .iter()
                    .map(|spec| FolderBarSection::Extension(spec.id)),
            );
        }
    }
    sections
}

/// Sectioned extension views grouped by section title, in first-seen order.
/// A section is collapsed when its first view says so.
pub fn extension_sections(views: &[MailViewSpec]) -> Vec<(&ViewSection, Vec<&MailViewSpec>)> {
    let mut groups: Vec<(&ViewSection, Vec<&MailViewSpec>)> = Vec::new();
    for spec in views {
        let Some(section) = &spec.section else {
            continue;
        };
        match groups
            .iter_mut()
            .find(|(seen, _)| seen.title == section.title)
        {
            Some((_, specs)) => specs.push(spec),
            None => groups.push((section, vec![spec])),
        }
    }
    groups
}

/// One sidebar row after IMAP paths are expanded into a tree.
#[derive(Clone, Debug)]
pub struct FolderTreeEntry {
    pub folder: Folder,
    pub depth: u32,
    pub has_children: bool,
    pub is_expanded: bool,
    /// Unread inside subfolders that a collapsed row hides; 0 when expanded.
    pub hidden_unread: u32,
}

fn special_use_rank(special: &SpecialUse) -> u8 {
    match special {
        SpecialUse::Inbox => 0,
        SpecialUse::Drafts => 1,
        SpecialUse::Sent => 2,
        SpecialUse::Archive => 3,
        SpecialUse::Snoozed => 4,
        SpecialUse::Outbox => 5,
        SpecialUse::Junk => 6,
        SpecialUse::Trash => 7,
        SpecialUse::All => 8,
        SpecialUse::Other => 9,
    }
}

/// Human-readable folder label for the sidebar.
pub use alcedo_core::folder_display_name;

/// Preferred Archive glyph: a filing box, not `folder-download-symbolic` (reads as Save).
pub const ARCHIVE_ICON_NAME: &str = "mail-archive-symbolic";
const ARCHIVE_ICON_FALLBACK: &str = "package-x-generic-symbolic";

/// Icon for the Archive action and Archive special-use folder.
pub fn archive_icon_name() -> &'static str {
    let Some(display) = gtk::gdk::Display::default() else {
        return ARCHIVE_ICON_NAME;
    };
    if gtk::IconTheme::for_display(&display).has_icon(ARCHIVE_ICON_NAME) {
        ARCHIVE_ICON_NAME
    } else {
        ARCHIVE_ICON_FALLBACK
    }
}

/// Icon name for a folder row.
pub fn folder_icon_name(special: &SpecialUse) -> &'static str {
    match special {
        SpecialUse::Inbox => "mail-inbox-symbolic",
        SpecialUse::Sent => "mail-sent-symbolic",
        SpecialUse::Drafts => "mail-drafts-symbolic",
        SpecialUse::Trash => "user-trash-symbolic",
        SpecialUse::Junk => "mail-mark-junk-symbolic",
        SpecialUse::Snoozed => "alarm-symbolic",
        SpecialUse::Outbox => "document-send-symbolic",
        SpecialUse::Archive => archive_icon_name(),
        SpecialUse::All | SpecialUse::Other => "folder-symbolic",
    }
}

/// CSS class for tinting a folder icon by special-use.
pub fn folder_icon_css_class(special: &SpecialUse) -> &'static str {
    match special {
        SpecialUse::Inbox => "alcedo-folder-icon-inbox",
        SpecialUse::Drafts => "alcedo-folder-icon-drafts",
        SpecialUse::Sent => "alcedo-folder-icon-sent",
        SpecialUse::Junk => "alcedo-folder-icon-junk",
        SpecialUse::Trash => "alcedo-folder-icon-trash",
        SpecialUse::Snoozed => "alcedo-folder-icon-snoozed",
        SpecialUse::Outbox => "alcedo-folder-icon-outbox",
        SpecialUse::Archive => "alcedo-folder-icon-archive",
        SpecialUse::All | SpecialUse::Other => "alcedo-folder-icon-folder",
    }
}

/// Immediate parent IMAP name, if `imap_name` contains `delimiter`.
pub fn folder_parent_name(imap_name: &str, delimiter: &str) -> Option<String> {
    if delimiter.is_empty() {
        return None;
    }
    let (parent, child) = imap_name.rsplit_once(delimiter)?;
    if parent.is_empty() || child.is_empty() {
        None
    } else {
        Some(parent.to_string())
    }
}

/// Nearest ancestor that exists in `known_names` (skips missing intermediate paths).
pub fn folder_effective_parent(
    imap_name: &str,
    delimiter: &str,
    known_names: &HashSet<String>,
) -> Option<String> {
    let mut current = folder_parent_name(imap_name, delimiter)?;
    loop {
        if known_names.contains(&current) {
            return Some(current);
        }
        current = folder_parent_name(&current, delimiter)?;
    }
}

/// Sort siblings: special-use rank, then display name.
pub fn sort_folders(folders: &mut [Folder]) {
    folders.sort_by(|a, b| {
        special_use_rank(&a.special)
            .cmp(&special_use_rank(&b.special))
            .then_with(|| folder_display_name(a).cmp(&folder_display_name(b)))
            .then_with(|| a.imap_name.cmp(&b.imap_name))
    });
}

/// True when `child_imap` is nested under `ancestor_imap` using `delimiter`.
pub fn folder_is_under(child_imap: &str, ancestor_imap: &str, delimiter: &str) -> bool {
    if delimiter.is_empty() || ancestor_imap.is_empty() || child_imap == ancestor_imap {
        return false;
    }
    child_imap
        .strip_prefix(ancestor_imap)
        .is_some_and(|rest| rest.starts_with(delimiter))
}

/// IMAP names of existing ancestors, nearest parent first.
pub fn imap_ancestor_names(
    imap_name: &str,
    delimiter: &str,
    known_names: &HashSet<String>,
) -> Vec<String> {
    let mut names = Vec::new();
    let mut current = folder_effective_parent(imap_name, delimiter, known_names);
    while let Some(parent) = current {
        names.push(parent.clone());
        current = folder_effective_parent(&parent, delimiter, known_names);
    }
    names
}

/// Depth-first tree order so children sit directly under their IMAP parent.
pub fn build_folder_tree(folders: &[Folder], collapsed: &HashSet<String>) -> Vec<FolderTreeEntry> {
    let visible: Vec<Folder> = folders
        .iter()
        .filter(|folder| !matches!(folder.special, SpecialUse::Snoozed | SpecialUse::Outbox))
        .cloned()
        .collect();
    let known_names: HashSet<String> = visible.iter().map(|f| f.imap_name.clone()).collect();
    let mut children: HashMap<Option<String>, Vec<Folder>> = HashMap::new();
    for folder in &visible {
        let parent = folder_effective_parent(&folder.imap_name, &folder.delimiter, &known_names);
        children.entry(parent).or_default().push(folder.clone());
    }
    for siblings in children.values_mut() {
        sort_folders(siblings);
    }

    let mut out = Vec::with_capacity(visible.len());
    walk_folder_tree(&children, None, 0, collapsed, &mut out);
    out
}

fn walk_folder_tree(
    children: &HashMap<Option<String>, Vec<Folder>>,
    parent: Option<String>,
    depth: u32,
    collapsed: &HashSet<String>,
    out: &mut Vec<FolderTreeEntry>,
) {
    let Some(siblings) = children.get(&parent) else {
        return;
    };
    for folder in siblings.iter() {
        let has_children = children
            .get(&Some(folder.imap_name.clone()))
            .is_some_and(|kids| !kids.is_empty());
        let is_expanded = !collapsed.contains(&folder.imap_name);
        let hidden_unread = if has_children && !is_expanded {
            descendant_unread(children, &folder.imap_name)
        } else {
            0
        };
        out.push(FolderTreeEntry {
            folder: folder.clone(),
            depth,
            has_children,
            is_expanded,
            hidden_unread,
        });
        if has_children && is_expanded {
            walk_folder_tree(
                children,
                Some(folder.imap_name.clone()),
                depth + 1,
                collapsed,
                out,
            );
        }
    }
}

/// Unread across every folder nested under `imap_name`, at any depth.
fn descendant_unread(children: &HashMap<Option<String>, Vec<Folder>>, imap_name: &str) -> u32 {
    let Some(kids) = children.get(&Some(imap_name.to_string())) else {
        return 0;
    };
    kids.iter()
        .map(|kid| {
            kid.unread
                .saturating_add(descendant_unread(children, &kid.imap_name))
        })
        .fold(0, u32::saturating_add)
}

/// Inbox if selectable, otherwise the first selectable folder.
pub fn find_default_folder(folders: &[Folder]) -> Option<FolderId> {
    folders
        .iter()
        .find(|folder| folder.selectable && matches!(folder.special, SpecialUse::Inbox))
        .or_else(|| folders.iter().find(|folder| folder.selectable))
        .map(|folder| folder.id)
}

fn folder_imap_name_matches(folder: &Folder, imap_name: &str) -> bool {
    folder.imap_name.eq_ignore_ascii_case(imap_name)
}

pub fn find_restore_folder<'a>(folders: &'a [Folder], imap_name: &str) -> Option<&'a Folder> {
    folders
        .iter()
        .find(|folder| folder.imap_name == imap_name)
        .or_else(|| {
            folders
                .iter()
                .find(|folder| folder_imap_name_matches(folder, imap_name))
        })
}

/// How a deferred last-folder restore should react to one account folder snapshot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FolderRestoreMatch {
    Found(FolderId),
    Wait,
}

/// Keep waiting until the saved IMAP name appears. Do not substitute Inbox or
/// Unified Inbox — that would persist a different folder than the user left on.
pub fn match_pending_folder_restore(folders: &[Folder], imap_name: &str) -> FolderRestoreMatch {
    match find_restore_folder(folders, imap_name) {
        Some(folder) => FolderRestoreMatch::Found(folder.id),
        None => FolderRestoreMatch::Wait,
    }
}

/// True when hydrate has started and the restore account is not among the loaded set.
/// An empty list means accounts have not arrived yet — not that the account was deleted.
pub fn should_abandon_pending_restore(restore_account: AccountId, known_ids: &[AccountId]) -> bool {
    !known_ids.is_empty() && !known_ids.contains(&restore_account)
}

/// Startup mail view, rail account, and optional restore wait.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StartupSelection {
    pub mail_view: MailView,
    pub selected_account: Option<AccountId>,
    pub pending_restore: Option<(AccountId, String)>,
}

/// Restore [`LastFolder`] against accounts/folders already loaded (mock) or
/// defer until `FoldersUpdated` (live, empty maps).
///
/// `last_account` is the switcher account saved separately from the folder, so
/// views that span accounts keep it. While `account_ids` is empty (live accounts
/// have not arrived) it is trusted as-is; removing an account clears it.
pub fn resolve_startup_selection(
    last: &LastFolder,
    last_account: Option<AccountId>,
    account_ids: &[AccountId],
    folders: &HashMap<AccountId, Vec<Folder>>,
) -> StartupSelection {
    let first_account = account_ids.first().copied();
    let rail_account = last_account
        .filter(|id| account_ids.is_empty() || account_ids.contains(id))
        .or(first_account);
    let account_view = |mail_view: MailView| StartupSelection {
        mail_view,
        selected_account: rail_account,
        pending_restore: None,
    };
    match last {
        LastFolder::UnifiedInbox => account_view(MailView::UnifiedInbox),
        // Extension views come back once their extension lists them.
        LastFolder::Extension { .. } => account_view(MailView::UnifiedInbox),
        LastFolder::Mailbox { account, imap_name } => {
            if let Some(folder) = folders
                .get(account)
                .and_then(|list| find_restore_folder(list, imap_name))
            {
                return StartupSelection {
                    mail_view: MailView::Folder(folder.id),
                    selected_account: Some(*account),
                    pending_restore: None,
                };
            }

            let account_known = account_ids.contains(account);
            if !account_ids.is_empty() && !account_known {
                return StartupSelection {
                    mail_view: MailView::UnifiedInbox,
                    selected_account: first_account,
                    pending_restore: None,
                };
            }

            if folders.get(account).is_some() {
                return StartupSelection {
                    mail_view: MailView::UnifiedInbox,
                    selected_account: Some(*account),
                    pending_restore: None,
                };
            }

            StartupSelection {
                mail_view: MailView::UnifiedInbox,
                selected_account: Some(*account),
                pending_restore: Some((*account, imap_name.clone())),
            }
        }
    }
}

/// Activate the mail view encoded in a folder-bar row's widget name.
fn activate_folder_row(
    row: &gtk::ListBoxRow,
    on_unified_inbox: impl Fn(),
    on_extension: impl Fn(ExtensionListId),
    on_select_folder: impl Fn(FolderId),
) {
    match parse_folder_row(row) {
        Some(MailView::UnifiedInbox) => on_unified_inbox(),
        Some(MailView::Extension(id)) => on_extension(id),
        Some(MailView::Folder(id)) => on_select_folder(id),
        Some(MailView::Search) | None => {}
    }
}

fn click_is_on_expand_control(gesture: &gtk::GestureClick, x: f64, y: f64) -> bool {
    let Some(widget) = gesture.widget() else {
        return false;
    };
    let Some(picked) = widget.pick(x, y, gtk::PickFlags::DEFAULT) else {
        return false;
    };
    if picked.has_css_class("alcedo-folder-expand") {
        return true;
    }
    picked
        .ancestor(gtk::Button::static_type())
        .is_some_and(|btn| btn.has_css_class("alcedo-folder-expand"))
}

pub(crate) fn attach_row_click(widget: &impl IsA<gtk::Widget>, on_activate: impl Fn() + 'static) {
    let gesture = gtk::GestureClick::new();
    gesture.set_button(gdk::BUTTON_PRIMARY);
    // Capture on the row contents so the click is claimed even when ListBox
    // `get_row_at_y` misses (GTK 4.22 + `.navigation-sidebar` padding).
    gesture.set_propagation_phase(gtk::PropagationPhase::Capture);
    gesture.connect_pressed(move |gesture, _, x, y| {
        if click_is_on_expand_control(gesture, x, y) {
            return;
        }
        gesture.set_state(gtk::EventSequenceState::Claimed);
        on_activate();
    });
    widget.as_ref().add_controller(gesture);
}

fn click_is_on_mailbox_row(widget: &gtk::Widget, x: f64, y: f64) -> bool {
    let Some(picked) = widget.pick(x, y, gtk::PickFlags::DEFAULT) else {
        return false;
    };
    picked
        .ancestor(gtk::ListBoxRow::static_type())
        .is_some_and(|row| {
            let name = row.widget_name();
            parse_folder_widget_name(name.as_str()).is_some()
                || crate::folder_bar_accounts::is_account_pane_row(name.as_str())
        })
}

/// A point on `widget` in window coordinates. Menus are placed from the window,
/// so rows open them correctly from whichever folder-pane list they are in.
pub(crate) fn point_in_window(widget: &gtk::Widget, x: f64, y: f64) -> (f64, f64) {
    widget
        .root()
        .and_then(|root| {
            widget.compute_point(
                root.upcast_ref::<gtk::Widget>(),
                &gtk::graphene::Point::new(x as f32, y as f32),
            )
        })
        .map(|point| (f64::from(point.x()), f64::from(point.y())))
        .unwrap_or((x, y))
}

/// Right-click empty folder-pane space (not a mailbox row): New Folder at account root.
pub fn bind_folder_pane_empty_context(
    scroll: &gtk::ScrolledWindow,
    on_empty_context: impl Fn(f64, f64) + 'static,
) {
    let gesture = gtk::GestureClick::new();
    gesture.set_button(3);
    gesture.set_propagation_phase(gtk::PropagationPhase::Bubble);
    gesture.connect_released(move |gesture, _, x, y| {
        let Some(widget) = gesture.widget() else {
            return;
        };
        if click_is_on_mailbox_row(&widget, x, y) {
            return;
        }
        let (popup_x, popup_y) = point_in_window(&widget, x, y);
        on_empty_context(popup_x, popup_y);
    });
    scroll.add_controller(gesture);
}

pub(crate) fn attach_secondary_click(
    widget: &impl IsA<gtk::Widget>,
    on_context: impl Fn(f64, f64) + 'static,
) {
    let gesture = gtk::GestureClick::new();
    gesture.set_button(3);
    gesture.set_propagation_phase(gtk::PropagationPhase::Capture);
    gesture.connect_pressed(|gesture, _, _, _| {
        gesture.set_state(gtk::EventSequenceState::Claimed);
    });
    // Read the widget from the gesture: capturing it would form a
    // widget -> gesture -> closure -> widget cycle that is never freed.
    gesture.connect_released(move |gesture, _, x, y| {
        let Some(widget) = gesture.widget() else {
            return;
        };
        let (popup_x, popup_y) = point_in_window(&widget, x, y);
        on_context(popup_x, popup_y);
    });
    widget.as_ref().add_controller(gesture);
}

fn attach_row_context_menu(
    widget: &impl IsA<gtk::Widget>,
    view: MailView,
    on_context: impl Fn(MailView, f64, f64) + 'static,
) {
    attach_secondary_click(widget, move |x, y| on_context(view, x, y));
}

fn ctrl_pressed() -> bool {
    gtk::gdk::Display::default()
        .and_then(|display| display.default_seat())
        .and_then(|seat| seat.pointer())
        .map(|device| {
            device
                .modifier_state()
                .contains(gdk::ModifierType::CONTROL_MASK)
        })
        .unwrap_or(false)
}

/// Plain drop is MOVE. COPY when Ctrl is held (and the source offers it) or
/// when the source is COPY-only (right-click-drag from the message handle).
fn preferred_drop_action(available: gdk::DragAction, ctrl: bool) -> gdk::DragAction {
    if ctrl && available.contains(gdk::DragAction::COPY) {
        gdk::DragAction::COPY
    } else if available.contains(gdk::DragAction::MOVE) {
        gdk::DragAction::MOVE
    } else if available.contains(gdk::DragAction::COPY) {
        gdk::DragAction::COPY
    } else {
        gdk::DragAction::empty()
    }
}

/// Toggle the drop highlight on the row that owns `target`. The row is read
/// from the controller rather than captured: a captured row would form a
/// row -> controller -> closure -> row cycle, so rows removed by a folder bar
/// rebuild (and everything their callbacks hold) would never be freed.
pub(crate) fn set_drop_highlight(target: &gtk::DropTarget, highlighted: bool) {
    let Some(row) = target.widget() else {
        return;
    };
    if highlighted {
        row.add_css_class("alcedo-folder-drop-target");
    } else {
        row.remove_css_class("alcedo-folder-drop-target");
    }
}

fn drop_target_action(target: &gtk::DropTarget) -> gdk::DragAction {
    let available = target
        .current_drop()
        .map(|drop| drop.actions())
        .unwrap_or(gdk::DragAction::MOVE | gdk::DragAction::COPY);
    preferred_drop_action(available, ctrl_pressed())
}

fn attach_folder_drop_target(
    row: &gtk::ListBoxRow,
    folder_id: FolderId,
    can_drop: Rc<dyn Fn(FolderId, &[MessageId]) -> bool>,
    on_drop: Rc<dyn Fn(FolderId, Vec<MessageId>, bool)>,
) {
    let drop_target = gtk::DropTarget::new(
        glib::Type::STRING,
        gdk::DragAction::MOVE | gdk::DragAction::COPY,
    );
    let can_drop_enter = can_drop.clone();
    let can_drop_motion = can_drop.clone();
    let can_drop_drop = can_drop.clone();
    let on_drop = on_drop.clone();
    let preferred_action = Rc::new(Cell::new(gdk::DragAction::MOVE));
    let preferred_enter = preferred_action.clone();

    drop_target.connect_enter(move |target, _, _| {
        if can_drop_enter(folder_id, &[]) {
            set_drop_highlight(target, true);
            let action = drop_target_action(target);
            preferred_enter.set(action);
            action
        } else {
            gdk::DragAction::empty()
        }
    });
    let preferred_motion = preferred_action.clone();
    drop_target.connect_motion(move |target, _, _| {
        if !can_drop_motion(folder_id, &[]) {
            return gdk::DragAction::empty();
        }
        set_drop_highlight(target, true);
        let action = drop_target_action(target);
        preferred_motion.set(action);
        action
    });
    drop_target.connect_leave(|target| set_drop_highlight(target, false));
    drop_target.connect_drop(move |target, value, _, _| {
        set_drop_highlight(target, false);
        let Ok(text) = value.get::<String>() else {
            return false;
        };
        let ids = parse_message_id_payload(&text);
        if ids.is_empty() || !can_drop_drop(folder_id, &ids) {
            return false;
        }
        let copy = preferred_action.get() == gdk::DragAction::COPY;
        on_drop(folder_id, ids, copy);
        true
    });
    row.add_controller(drop_target);
}

fn should_expand_on_drop_hover(has_children: bool, is_expanded: bool) -> bool {
    has_children && !is_expanded
}

fn cancel_drop_expand_timeout(timeout: &Cell<Option<glib::SourceId>>) {
    crate::glib_source::cancel_source(timeout);
}

/// Expand a collapsed parent after a hover delay so nested folders can be
/// drop targets. Uses `DropControllerMotion` so this still runs when the row
/// itself rejects the drop (source folder, non-selectable namespace).
pub(crate) fn attach_folder_drop_expand(row: &gtk::ListBoxRow, on_expand: Rc<dyn Fn()>) {
    attach_drop_expand_when(row, |_| true, on_expand);
}

/// Like [`attach_folder_drop_expand`], but only for drags `accepts` approves, so
/// a row can ignore drags that are not meant for its children.
pub(crate) fn attach_drop_expand_when(
    row: &gtk::ListBoxRow,
    accepts: impl Fn(&gdk::Drop) -> bool + 'static,
    on_expand: Rc<dyn Fn()>,
) {
    let timeout = Rc::new(Cell::new(None::<glib::SourceId>));
    let motion = gtk::DropControllerMotion::new();

    let timeout_enter = timeout.clone();
    let on_expand_enter = on_expand.clone();
    motion.connect_enter(move |motion, _, _| {
        cancel_drop_expand_timeout(&timeout_enter);
        if !motion.drop().is_some_and(|drop| accepts(&drop)) {
            return;
        }
        let timeout_cb = timeout_enter.clone();
        let on_expand = on_expand_enter.clone();
        timeout_enter.set(Some(glib::timeout_add_local_once(
            Duration::from_millis(FOLDER_DROP_EXPAND_MS),
            move || {
                let _ = timeout_cb.take();
                on_expand();
            },
        )));
    });

    let timeout_leave = timeout.clone();
    motion.connect_leave(move |_| {
        cancel_drop_expand_timeout(&timeout_leave);
    });

    let timeout_destroy = timeout;
    row.connect_destroy(move |_| {
        cancel_drop_expand_timeout(&timeout_destroy);
    });
    row.add_controller(motion);
}

/// Callbacks shared by every folder row, in either folder pane layout.
pub(crate) struct FolderRowCallbacks {
    pub on_select: Rc<dyn Fn(FolderId)>,
    pub on_toggle: Rc<dyn Fn(AccountId, String)>,
    pub on_toggle_personal: Rc<dyn Fn(AccountId)>,
    pub on_context: Rc<dyn Fn(MailView, f64, f64)>,
    pub can_drop: Rc<dyn Fn(FolderId, &[MessageId]) -> bool>,
    pub on_drop: Rc<dyn Fn(FolderId, Vec<MessageId>, bool)>,
}

pub(crate) fn append_folder_rows(
    list_box: &gtk::ListBox,
    tree: &[FolderTreeEntry],
    base_depth: u32,
    mail_view: MailView,
    syncing: Option<MailView>,
    callbacks: &FolderRowCallbacks,
) {
    for entry in tree {
        let entry = FolderTreeEntry {
            depth: entry.depth + base_depth,
            ..entry.clone()
        };
        let folder_id = entry.folder.id;
        let account_id = entry.folder.account_id;
        let imap_name = entry.folder.imap_name.clone();
        let on_toggle = entry.has_children.then(|| {
            let on_toggle = callbacks.on_toggle.clone();
            Rc::new(move || on_toggle(account_id, imap_name.clone())) as Rc<dyn Fn()>
        });
        let on_select = callbacks.on_select.clone();
        let on_context = callbacks.on_context.clone();
        let row = folder_row(
            &entry,
            syncing == Some(MailView::Folder(folder_id)),
            move || on_select(folder_id),
            on_toggle,
            move |view, x, y| on_context(view, x, y),
            callbacks.can_drop.clone(),
            callbacks.on_drop.clone(),
        );
        list_box.append(&row);
        if mail_view == MailView::Folder(folder_id) {
            list_box.select_row(Some(&row));
        }
    }
}

/// Total shown on a folder row: its own unread plus any hidden by collapsing.
fn folder_badge_total(entry: &FolderTreeEntry) -> u32 {
    entry.folder.unread.saturating_add(entry.hidden_unread)
}

fn folder_badge_tooltip(entry: &FolderTreeEntry) -> Option<String> {
    (entry.hidden_unread > 0).then(|| {
        format!(
            "{} unread in this folder, {} in collapsed subfolders",
            entry.folder.unread, entry.hidden_unread
        )
    })
}

fn folder_unread_badge(entry: &FolderTreeEntry) -> Option<gtk::Label> {
    let total = folder_badge_total(entry);
    if total == 0 {
        return None;
    }
    let badge = gtk::Label::new(Some(&total.to_string()));
    badge.add_css_class("alcedo-folder-unread");
    badge.set_halign(gtk::Align::End);
    badge.set_xalign(1.0);
    if let Some(tooltip) = folder_badge_tooltip(entry) {
        badge.add_css_class("alcedo-folder-unread-nested");
        badge.set_tooltip_text(Some(&tooltip));
    }
    Some(badge)
}

fn folder_row(
    entry: &FolderTreeEntry,
    syncing: bool,
    on_select: impl Fn() + 'static,
    on_toggle_expand: Option<Rc<dyn Fn()>>,
    on_context: impl Fn(MailView, f64, f64) + Clone + 'static,
    can_drop: Rc<dyn Fn(FolderId, &[MessageId]) -> bool>,
    on_drop: Rc<dyn Fn(FolderId, Vec<MessageId>, bool)>,
) -> gtk::ListBoxRow {
    let folder = &entry.folder;
    let row = gtk::ListBoxRow::new();
    row.set_widget_name(&format!("{ROW_FOLDER_PREFIX}{}", folder.id.0));

    let icon = icons::sidebar_image(folder_icon_name(&folder.special));
    icon.add_css_class("alcedo-folder-icon");
    icon.add_css_class(folder_icon_css_class(&folder.special));
    let label = gtk::Label::new(Some(&folder_display_name(folder)));
    label.set_halign(gtk::Align::Start);
    label.set_hexpand(true);
    label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    install_overflow_tooltip(&label);

    let row_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row_box.add_css_class("alcedo-folder-row");
    row_box.set_margin_start(8 + entry.depth as i32 * 16);
    row_box.set_margin_end(8);
    row_box.set_margin_top(0);
    row_box.set_margin_bottom(0);
    row_box.set_hexpand(true);
    row_box.set_halign(gtk::Align::Fill);
    if entry.has_children {
        let expand_icon = if entry.is_expanded {
            "pan-down-symbolic"
        } else {
            "pan-end-symbolic"
        };
        let expand_btn = gtk::Button::from_icon_name(expand_icon);
        expand_btn.add_css_class("flat");
        expand_btn.add_css_class("alcedo-folder-expand");
        expand_btn.set_tooltip_text(Some(if entry.is_expanded {
            "Collapse folder"
        } else {
            "Expand folder"
        }));
        expand_btn.set_has_frame(false);
        expand_btn.set_focus_on_click(false);
        if let Some(on_toggle) = on_toggle_expand.clone() {
            expand_btn.connect_clicked(move |_| on_toggle());
        }
        row_box.append(&expand_btn);
    } else {
        let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        spacer.add_css_class("alcedo-folder-expand-slot");
        spacer.set_size_request(16, -1);
        spacer.set_can_target(false);
        row_box.append(&spacer);
    }
    row_box.append(&icon);
    row_box.append(&label);

    if syncing {
        let spinner = adw::Spinner::new();
        spinner.add_css_class("alcedo-sync-spinner");
        spinner.set_tooltip_text(Some("Syncing…"));
        crate::a11y::set_label(&spinner, "Syncing");
        row_box.append(&spinner);
    } else if let Some(badge) = folder_unread_badge(entry) {
        row_box.append(&badge);
    }

    row.set_child(Some(&row_box));

    let folder_id = folder.id;
    attach_row_context_menu(&row_box, MailView::Folder(folder_id), on_context);

    if folder.selectable {
        row.set_activatable(true);
        attach_row_click(&row_box, on_select);
        attach_folder_drop_target(&row, folder_id, can_drop, on_drop);
    } else {
        // Keep the row sensitive so parent expand/collapse still works
        // (e.g. non-selectable IMAP namespaces like `[Gmail]`).
        row.set_activatable(false);
        row.add_css_class("alcedo-folder-inert");
        label.add_css_class("dim-label");
    }

    if should_expand_on_drop_hover(entry.has_children, entry.is_expanded)
        && let Some(on_expand) = on_toggle_expand
    {
        attach_folder_drop_expand(&row, on_expand);
    }

    row
}

fn virtual_mail_row(
    widget_name: &str,
    title: &str,
    tooltip: &str,
    icon_name: &str,
    icon_class: &str,
    count: u32,
    syncing: bool,
    view: MailView,
    on_select: impl Fn() + 'static,
    on_context: impl Fn(MailView, f64, f64) + Clone + 'static,
) -> gtk::ListBoxRow {
    let row = gtk::ListBoxRow::new();
    row.set_widget_name(widget_name);
    row.set_activatable(true);
    row.set_tooltip_text(Some(tooltip));

    let icon = icons::sidebar_image(icon_name);
    icon.add_css_class("alcedo-folder-icon");
    icon.add_css_class(icon_class);
    let label = gtk::Label::new(Some(title));
    label.set_halign(gtk::Align::Start);
    label.set_hexpand(true);
    label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    install_overflow_tooltip(&label);

    let row_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row_box.add_css_class("alcedo-folder-row");
    row_box.set_margin_start(8);
    row_box.set_margin_end(8);
    row_box.set_margin_top(0);
    row_box.set_margin_bottom(0);
    row_box.set_hexpand(true);
    row_box.append(&icon);
    row_box.append(&label);

    if syncing {
        let spinner = adw::Spinner::new();
        spinner.add_css_class("alcedo-sync-spinner");
        spinner.set_tooltip_text(Some("Syncing…"));
        crate::a11y::set_label(&spinner, "Syncing");
        row_box.append(&spinner);
    } else if count > 0 {
        let unread = gtk::Label::new(Some(&count.to_string()));
        unread.add_css_class("alcedo-folder-unread");
        unread.set_halign(gtk::Align::End);
        unread.set_xalign(1.0);
        row_box.append(&unread);
    }

    attach_row_click(&row_box, on_select);
    attach_row_context_menu(&row_box, view, on_context);
    row.set_child(Some(&row_box));
    row
}

fn section_header_row(
    title: &str,
    expanded: bool,
    collapsed_unread: u32,
    on_toggle: impl Fn() + 'static,
) -> gtk::ListBoxRow {
    let row = gtk::ListBoxRow::new();
    row.set_widget_name(&format!("{ROW_SECTION_PREFIX}{title}"));
    row.set_activatable(false);
    row.set_can_focus(false);
    row.add_css_class("alcedo-folder-section");

    let tooltip = if expanded {
        format!("Collapse {}", title.to_lowercase())
    } else {
        format!("Expand {}", title.to_lowercase())
    };
    let expand_icon = if expanded {
        "pan-down-symbolic"
    } else {
        "pan-end-symbolic"
    };
    let expand_btn = gtk::Button::from_icon_name(expand_icon);
    expand_btn.add_css_class("flat");
    expand_btn.add_css_class("alcedo-folder-expand");
    expand_btn.set_tooltip_text(Some(&tooltip));
    expand_btn.set_has_frame(false);
    expand_btn.set_focus_on_click(false);
    let on_toggle = Rc::new(on_toggle);
    {
        let on_toggle = on_toggle.clone();
        expand_btn.connect_clicked(move |_| on_toggle());
    }

    let label = gtk::Label::builder()
        .label(title)
        .xalign(0.0)
        .hexpand(true)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .build();
    label.add_css_class("caption");
    label.add_css_class("dim-label");

    let row_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row_box.add_css_class("alcedo-folder-row");
    row_box.set_margin_start(8);
    row_box.set_margin_end(8);
    row_box.set_margin_top(4);
    row_box.set_margin_bottom(4);
    row_box.set_tooltip_text(Some(&tooltip));
    row_box.append(&expand_btn);
    row_box.append(&label);

    if !expanded && collapsed_unread > 0 {
        let unread = gtk::Label::new(Some(&collapsed_unread.to_string()));
        unread.add_css_class("alcedo-folder-unread");
        unread.set_halign(gtk::Align::End);
        unread.set_xalign(1.0);
        row_box.append(&unread);
    }

    attach_row_click(&row_box, {
        let on_toggle = on_toggle.clone();
        move || on_toggle()
    });
    row.set_child(Some(&row_box));
    row
}

fn attach_extension_drag_source(row: &gtk::ListBoxRow, id: ExtensionListId) {
    let source = gtk::DragSource::new();
    source.set_actions(gdk::DragAction::MOVE);
    source.connect_prepare(move |_, _, _| {
        let payload = format!("{EXT_DRAG_PREFIX}{}", id.0);
        Some(gdk::ContentProvider::for_value(&payload.to_value()))
    });
    row.add_controller(source);
}

fn attach_extension_drop_target(
    row: &gtk::ListBoxRow,
    target_id: ExtensionListId,
    on_reorder: Rc<dyn Fn(ExtensionListId, ExtensionListId)>,
) {
    let drop_target = gtk::DropTarget::new(glib::Type::STRING, gdk::DragAction::MOVE);
    drop_target.connect_enter(|target, _, _| {
        set_drop_highlight(target, true);
        gdk::DragAction::MOVE
    });
    drop_target.connect_motion(|target, _, _| {
        set_drop_highlight(target, true);
        gdk::DragAction::MOVE
    });
    drop_target.connect_leave(|target| set_drop_highlight(target, false));
    drop_target.connect_drop(move |target, value, _, _| {
        set_drop_highlight(target, false);
        let Ok(text) = value.get::<String>() else {
            return false;
        };
        let Some(uuid_str) = text.strip_prefix(EXT_DRAG_PREFIX) else {
            return false;
        };
        let Ok(uuid) = uuid::Uuid::parse_str(uuid_str) else {
            return false;
        };
        let dragged = ExtensionListId(uuid);
        if dragged == target_id {
            return false;
        }
        on_reorder(dragged, target_id);
        true
    });
    row.add_controller(drop_target);
}

/// Sidebar row for an extension view.
fn extension_view_row(
    spec: &MailViewSpec,
    syncing: Option<MailView>,
    on_extension: impl Fn(ExtensionListId) + Clone + 'static,
    on_context: impl Fn(MailView, f64, f64) + Clone + 'static,
    on_reorder: &Rc<dyn Fn(ExtensionListId, ExtensionListId)>,
) -> gtk::ListBoxRow {
    let view = MailView::Extension(spec.id);
    let row = virtual_mail_row(
        &format!("{ROW_EXTENSION_PREFIX}{}", spec.id.0),
        &spec.title,
        &spec.tooltip,
        spec.icon_name,
        "alcedo-folder-icon-extension",
        spec.unread,
        syncing == Some(view),
        view,
        {
            let id = spec.id;
            move || on_extension(id)
        },
        on_context,
    );
    if spec.reorderable {
        attach_extension_drag_source(&row, spec.id);
        attach_extension_drop_target(&row, spec.id, on_reorder.clone());
    }
    row
}

fn unified_inbox_row(
    unified_unread: u32,
    syncing: bool,
    on_select: impl Fn() + 'static,
    on_context: impl Fn(MailView, f64, f64) + Clone + 'static,
) -> gtk::ListBoxRow {
    virtual_mail_row(
        ROW_UNIFIED,
        EVERYTHING_TITLE,
        UNIFIED_INBOX_TOOLTIP,
        icons::EVERYTHING_SIDEBAR,
        "alcedo-folder-icon-everything",
        unified_unread,
        syncing,
        MailView::UnifiedInbox,
        on_select,
        on_context,
    )
}

pub(crate) fn separator_row() -> gtk::ListBoxRow {
    let row = gtk::ListBoxRow::new();
    row.set_widget_name("alcedo-sep");
    row.set_sensitive(false);
    row.set_activatable(false);
    let sep = gtk::Separator::new(gtk::Orientation::Horizontal);
    sep.add_css_class("alcedo-folder-sep");
    row.set_child(Some(&sep));
    row
}

pub(crate) fn waiting_row() -> gtk::ListBoxRow {
    let row = gtk::ListBoxRow::new();
    row.set_widget_name("alcedo-waiting");
    row.set_sensitive(false);
    row.set_activatable(false);
    let spinner = adw::Spinner::new();
    spinner.add_css_class("alcedo-sync-spinner");
    crate::a11y::set_label(&spinner, "Waiting for folders");
    let label = gtk::Label::new(Some("Waiting for folders…"));
    label.add_css_class("dim-label");
    label.set_halign(gtk::Align::Start);
    let row_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row_box.add_css_class("alcedo-folder-row");
    row_box.set_margin_start(12);
    row_box.append(&spinner);
    row_box.append(&label);
    row.set_child(Some(&row_box));
    row
}

fn parse_folder_widget_name(name: &str) -> Option<MailView> {
    if let Some(uuid_str) = name.strip_prefix(ROW_EXTENSION_PREFIX) {
        return uuid::Uuid::parse_str(uuid_str)
            .ok()
            .map(|id| MailView::Extension(ExtensionListId(id)));
    }
    if name == ROW_UNIFIED {
        return Some(MailView::UnifiedInbox);
    }
    if name.starts_with(ROW_SECTION_PREFIX) {
        return None;
    }
    let id = name
        .strip_prefix(ROW_FOLDER_PREFIX)
        .or_else(|| name.strip_prefix("alcedo-folder:"))?;
    id.parse::<i64>()
        .ok()
        .map(|id| MailView::Folder(FolderId(id)))
}

fn parse_folder_row(row: &gtk::ListBoxRow) -> Option<MailView> {
    parse_folder_widget_name(row.widget_name().as_str())
}

/// Keyboard activation. Mouse clicks are bound per row in [`populate_folder_bar`].
pub fn bind_folder_bar<FU, FRl, FF, FC>(
    lists: &FolderPaneLists,
    on_unified_inbox: FU,
    on_extension: FRl,
    on_select_folder: FF,
    on_context_menu: FC,
) where
    FU: Fn() + Clone + 'static,
    FRl: Fn(ExtensionListId) + Clone + 'static,
    FF: Fn(FolderId) + Clone + 'static,
    FC: Fn(MailView, f64, f64) + Clone + 'static,
{
    lists.keep_single_selection();
    for list in lists.all() {
        list.set_activate_on_single_click(true);
        list.set_selection_mode(gtk::SelectionMode::Browse);
        list.connect_row_activated({
            let on_unified_inbox = on_unified_inbox.clone();
            let on_extension = on_extension.clone();
            let on_select_folder = on_select_folder.clone();
            move |_, row| {
                activate_folder_row(row, &on_unified_inbox, &on_extension, &on_select_folder);
            }
        });
        list.add_controller(context_menu_key_controller(list, on_context_menu.clone()));
    }
}

/// Menu / Shift+F10 opens the context menu for the selected row of `list`.
fn context_menu_key_controller(
    list: &gtk::ListBox,
    on_context: impl Fn(MailView, f64, f64) + 'static,
) -> gtk::EventControllerKey {
    let key_controller = gtk::EventControllerKey::new();
    let list = list.clone();
    key_controller.connect_key_pressed(move |_, key, _, modifiers| {
        let menu_key = key == gdk::Key::Menu
            || (key == gdk::Key::F10 && modifiers.contains(gdk::ModifierType::SHIFT_MASK));
        if !menu_key {
            return glib::Propagation::Proceed;
        }
        let Some(row) = list.selected_row() else {
            return glib::Propagation::Stop;
        };
        let Some(view) = parse_folder_row(&row) else {
            return glib::Propagation::Stop;
        };
        let bounds = row
            .compute_bounds(&list)
            .unwrap_or(gtk::graphene::Rect::new(0.0, 0.0, 0.0, 0.0));
        let (x, y) = point_in_window(
            list.upcast_ref::<gtk::Widget>(),
            f64::from(bounds.x()) + 8.0,
            f64::from(bounds.y()) + f64::from(bounds.height()) / 2.0,
        );
        on_context(view, x, y);
        glib::Propagation::Stop
    });
    key_controller
}

/// Rebuild folder sidebar rows.
pub fn populate_folder_bar(
    lists: &FolderPaneLists,
    account_block: AccountBlock<'_>,
    mail_view: MailView,
    unified_unread: u32,
    extension_views: &[MailViewSpec],
    syncing: Option<MailView>,
    on_unified_inbox: impl Fn() + Clone + 'static,
    on_extension: impl Fn(ExtensionListId) + Clone + 'static,
    on_reorder_extension: impl Fn(ExtensionListId, ExtensionListId) + 'static,
    on_select_folder: impl Fn(FolderId) + Clone + 'static,
    on_toggle_folder: impl Fn(AccountId, String) + Clone + 'static,
    on_toggle_personal: impl Fn(AccountId) + Clone + 'static,
    on_toggle_section: impl Fn(String) + Clone + 'static,
    on_context_menu: impl Fn(MailView, f64, f64) + Clone + 'static,
    can_drop: impl Fn(FolderId, &[MessageId]) -> bool + Clone + 'static,
    on_drop: impl Fn(FolderId, Vec<MessageId>, bool) + Clone + 'static,
) {
    lists.remove_all_rows();
    for list in lists.all() {
        list.add_css_class("navigation-sidebar");
        list.add_css_class("alcedo-folder-list");
    }
    let top = &lists.top;
    let middle = &lists.middle;
    let bottom = &lists.bottom;

    let on_context = on_context_menu.clone();

    if matches!(
        &account_block,
        AccountBlock::Single {
            waiting_for_folders: true,
            ..
        }
    ) {
        middle.append(&waiting_row());
    }

    let folder_callbacks = FolderRowCallbacks {
        on_select: Rc::new(on_select_folder),
        on_toggle: Rc::new(on_toggle_folder),
        on_toggle_personal: Rc::new(on_toggle_personal),
        on_context: Rc::new(on_context.clone()),
        can_drop: Rc::new(can_drop),
        on_drop: Rc::new(on_drop),
    };

    let on_unified = Rc::new(on_unified_inbox);
    let unified_row = unified_inbox_row(
        unified_unread,
        syncing == Some(MailView::UnifiedInbox),
        {
            let on_unified = on_unified.clone();
            move || on_unified()
        },
        on_context.clone(),
    );
    top.append(&unified_row);
    if mail_view == MailView::UnifiedInbox {
        top.select_row(Some(&unified_row));
    }

    let on_reorder_extension: Rc<dyn Fn(ExtensionListId, ExtensionListId)> =
        Rc::new(on_reorder_extension);
    for spec in extension_views.iter().filter(|spec| spec.section.is_none()) {
        let row = extension_view_row(
            spec,
            syncing,
            on_extension.clone(),
            on_context.clone(),
            &on_reorder_extension,
        );
        top.append(&row);
        if mail_view == MailView::Extension(spec.id) {
            top.select_row(Some(&row));
        }
    }

    match account_block {
        AccountBlock::Single {
            folders,
            collapsed,
            personal_expanded,
            ..
        } => {
            let grouped = build_grouped_folder_tree(folders, &collapsed);
            if !grouped.system.is_empty() || !grouped.personal.is_empty() {
                middle.append(&separator_row());
                append_grouped_folder_rows(
                    middle,
                    &grouped,
                    personal_expanded,
                    0,
                    mail_view,
                    syncing,
                    &folder_callbacks,
                );
            }
        }
        AccountBlock::Stacked {
            sections,
            hidden,
            actions,
        } => {
            middle.append(&separator_row());
            append_account_sections(
                middle,
                &lists.pinned,
                &sections,
                &hidden,
                &actions,
                mail_view,
                syncing,
                &folder_callbacks,
            );
        }
    }

    for (section, specs) in extension_sections(extension_views) {
        bottom.append(&separator_row());
        let collapsed_unread = specs.iter().map(|spec| spec.unread).sum::<u32>();
        // An untitled section is a plain group of rows after a separator.
        if section.title.is_empty() {
            for spec in specs {
                let row = extension_view_row(
                    spec,
                    syncing,
                    on_extension.clone(),
                    on_context.clone(),
                    &on_reorder_extension,
                );
                bottom.append(&row);
                if mail_view == MailView::Extension(spec.id) {
                    bottom.select_row(Some(&row));
                }
            }
            continue;
        }
        bottom.append(&section_header_row(
            &section.title,
            !section.collapsed,
            collapsed_unread,
            {
                let on_toggle_section = on_toggle_section.clone();
                let title = section.title.clone();
                move || on_toggle_section(title.clone())
            },
        ));
        if section.collapsed {
            continue;
        }
        for spec in specs {
            let row = extension_view_row(
                spec,
                syncing,
                on_extension.clone(),
                on_context.clone(),
                &on_reorder_extension,
            );
            bottom.append(&row);
            if mail_view == MailView::Extension(spec.id) {
                bottom.select_row(Some(&row));
            }
        }
    }

    if mail_view == MailView::Search {
        lists.unselect_all();
    }
    if matches!(mail_view, MailView::Extension(_)) && lists.selected_row().is_none() {
        lists.unselect_all();
    }
    lists.sync_pinned_visibility();
}

pub(crate) fn account_switcher_caption(account_count: usize) -> Option<String> {
    (account_count >= 2).then(|| format!("{account_count} accounts"))
}

/// Second line on the account picker button.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct SwitcherCaption {
    pub text: String,
    /// True when an account needs the person to sign in, so the line is colored.
    pub needs_attention: bool,
}

/// Caption for the picker button. A broken account wins over the plain count,
/// whether or not it is the selected one, so the problem is visible without
/// opening the picker.
pub(crate) fn account_switcher_status(
    accounts: &[Account],
    selected: Option<AccountId>,
    auth_required: &HashMap<AccountId, String>,
) -> Option<SwitcherCaption> {
    let broken: Vec<&Account> = accounts
        .iter()
        .filter(|account| auth_required.contains_key(&account.id))
        .collect();
    let attention = |text: String| SwitcherCaption {
        text,
        needs_attention: true,
    };
    match broken.as_slice() {
        [] => account_switcher_caption(accounts.len()).map(|text| SwitcherCaption {
            text,
            needs_attention: false,
        }),
        [only] if selected == Some(only.id) => Some(attention("Sign-in required".to_string())),
        [only] => Some(attention(format!("{} needs sign-in", only.label()))),
        several => Some(attention(format!(
            "{} accounts need sign-in",
            several.len()
        ))),
    }
}

pub(crate) fn account_added_toast_title(email: &str) -> String {
    format!("{email} added")
}

#[cfg(test)]
pub(crate) fn from_selected_index(ids: &[AccountId], selected: AccountId) -> u32 {
    ids.iter().position(|id| *id == selected).unwrap_or(0) as u32
}

/// Account switcher pinned above the folder list.
pub struct AccountSwitcher {
    pub button: gtk::MenuButton,
    chip: adw::Avatar,
    email: gtk::Label,
    caption: gtk::Label,
    button_warning: gtk::Image,
    button_badge: gtk::Label,
    rows: gtk::Box,
    row_badges: HashMap<AccountId, gtk::Label>,
}

/// Build the folder-pane account switcher (`MenuButton` + popover rows).
pub fn build_account_switcher() -> AccountSwitcher {
    let chip = adw::Avatar::builder()
        .size(ACCOUNT_AVATAR_SIZE)
        .show_initials(true)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .can_focus(false)
        .build();

    let email = gtk::Label::builder()
        .xalign(0.0)
        .halign(gtk::Align::Start)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .build();

    let caption = gtk::Label::builder()
        .xalign(0.0)
        .halign(gtk::Align::Start)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .visible(false)
        .css_classes(["caption", "dim-label"])
        .build();

    let text_col = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(0)
        .hexpand(true)
        .halign(gtk::Align::Start)
        .valign(gtk::Align::Center)
        .build();
    text_col.append(&email);
    text_col.append(&caption);

    let button_badge = gtk::Label::new(None);
    button_badge.add_css_class("alcedo-folder-unread");
    button_badge.set_visible(false);

    let button_warning = gtk::Image::from_icon_name("dialog-warning-symbolic");
    button_warning.add_css_class("warning");
    button_warning.set_visible(false);

    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .build();
    content.append(&chip);
    content.append(&text_col);
    content.append(&button_warning);
    content.append(&button_badge);

    let rows = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .margin_top(6)
        .margin_bottom(6)
        .margin_start(6)
        .margin_end(6)
        .build();

    let popover = gtk::Popover::new();
    popover.set_child(Some(&rows));

    let button = gtk::MenuButton::builder()
        .child(&content)
        .popover(&popover)
        .tooltip_text("Switch account")
        .always_show_arrow(true)
        .halign(gtk::Align::Fill)
        .hexpand(true)
        .build();
    button.add_css_class("flat");
    button.add_css_class("alcedo-account-switcher");
    button.update_property(&[gtk::accessible::Property::Label("Switch account")]);

    AccountSwitcher {
        button,
        chip,
        email,
        caption,
        button_warning,
        button_badge,
        rows,
        row_badges: HashMap::new(),
    }
}

/// Rebuild popover rows and sync the switcher button face.
pub fn refresh_account_switcher(
    switcher: &mut AccountSwitcher,
    accounts: &[Account],
    selected: Option<AccountId>,
    unread_by_account: &HashMap<AccountId, (u32, u32)>,
    auth_required: &HashMap<AccountId, String>,
    on_select: impl Fn(AccountId) + Clone + 'static,
    on_add: impl Fn() + Clone + 'static,
) {
    while let Some(child) = switcher.rows.first_child() {
        switcher.rows.remove(&child);
    }
    switcher.row_badges.clear();

    for account in accounts {
        let account_id = account.id;
        let inbox_unread = unread_by_account
            .get(&account_id)
            .map(|(inbox, _)| *inbox)
            .unwrap_or(0);

        let row_chip = adw::Avatar::builder()
            .size(ACCOUNT_AVATAR_SIZE)
            .show_initials(true)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .can_focus(false)
            .build();
        apply_account_chip(&row_chip, account);

        let needs_sign_in = auth_required.contains_key(&account_id);
        let row_email = gtk::Label::builder()
            .label(account.label())
            .xalign(0.0)
            .halign(gtk::Align::Start)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build();
        let row_text = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .hexpand(true)
            .halign(gtk::Align::Fill)
            .build();
        row_text.append(&row_email);
        if needs_sign_in {
            let row_status = gtk::Label::builder()
                .label("Sign-in required")
                .xalign(0.0)
                .halign(gtk::Align::Start)
                .css_classes(["caption", "warning"])
                .build();
            row_text.append(&row_status);
        }

        let row_badge = gtk::Label::new(None);
        row_badge.add_css_class("alcedo-folder-unread");
        set_badge_count(&row_badge, inbox_unread);

        let check = gtk::Image::from_icon_name("object-select-symbolic");
        check.set_visible(selected == Some(account_id));

        let warning = gtk::Image::from_icon_name("dialog-warning-symbolic");
        warning.add_css_class("warning");
        warning.set_visible(needs_sign_in);

        let row_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .build();
        row_box.append(&row_chip);
        row_box.append(&row_text);
        row_box.append(&row_badge);
        row_box.append(&warning);
        row_box.append(&check);

        let row_btn = gtk::Button::builder()
            .child(&row_box)
            .tooltip_text(&account.email)
            .build();
        row_btn.add_css_class("flat");
        row_btn.add_css_class("alcedo-account-row");

        let menu_button = switcher.button.clone();
        let on_select = on_select.clone();
        row_btn.connect_clicked(move |_| {
            menu_button.popdown();
            on_select(account_id);
        });

        switcher.rows.append(&row_btn);
        switcher.row_badges.insert(account_id, row_badge);
    }

    if !accounts.is_empty() {
        switcher
            .rows
            .append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    }

    let add_icon = gtk::Image::from_icon_name("list-add-symbolic");
    let add_label = gtk::Label::new(Some("Add Account"));
    let add_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .build();
    add_box.append(&add_icon);
    add_box.append(&add_label);

    let add_btn = gtk::Button::builder().child(&add_box).build();
    add_btn.add_css_class("flat");
    add_btn.add_css_class("alcedo-account-row");

    let menu_button = switcher.button.clone();
    let on_add = on_add.clone();
    add_btn.connect_clicked(move |_| {
        menu_button.popdown();
        on_add();
    });
    switcher.rows.append(&add_btn);

    let selected_account = selected.and_then(|id| accounts.iter().find(|account| account.id == id));
    match selected_account {
        Some(account) => {
            switcher.chip.set_visible(true);
            apply_account_chip(&switcher.chip, account);
            switcher.email.set_label(account.label());
            switcher
                .button
                .set_tooltip_text(Some(account.email.as_str()));
            let inbox_unread = unread_by_account
                .get(&account.id)
                .map(|(inbox, _)| *inbox)
                .unwrap_or(0);
            set_badge_count(&switcher.button_badge, inbox_unread);
        }
        None => {
            switcher.chip.set_visible(false);
            switcher.email.set_label("Select account");
            switcher.button.set_tooltip_text(Some("Switch account"));
            set_badge_count(&switcher.button_badge, 0);
        }
    }

    let status = account_switcher_status(accounts, selected, auth_required);
    paint_switcher_status(switcher, status.as_ref());
}

/// Show the caption, color it when an account needs attention, and mirror the
/// warning as an icon so it stays visible when the caption is cut short.
fn paint_switcher_status(switcher: &AccountSwitcher, status: Option<&SwitcherCaption>) {
    let needs_attention = status.is_some_and(|status| status.needs_attention);
    switcher
        .caption
        .set_label(status.map_or("", |status| &status.text));
    switcher.caption.set_visible(status.is_some());
    switcher.caption.set_css_classes(if needs_attention {
        &["caption", "warning"]
    } else {
        &["caption", "dim-label"]
    });
    switcher.button_warning.set_visible(needs_attention);
    if let Some(status) = status.filter(|status| status.needs_attention) {
        switcher.button.set_tooltip_text(Some(status.text.as_str()));
    }
}

/// Update unread pills on the switcher button and popover rows.
pub fn set_switcher_unread(
    switcher: &AccountSwitcher,
    selected: Option<AccountId>,
    unread_by_account: &HashMap<AccountId, (u32, u32)>,
) {
    for (account_id, badge) in &switcher.row_badges {
        let inbox_unread = unread_by_account
            .get(account_id)
            .map(|(inbox, _)| *inbox)
            .unwrap_or(0);
        set_badge_count(badge, inbox_unread);
    }
    let button_unread = selected
        .and_then(|id| unread_by_account.get(&id).map(|(inbox, _)| *inbox))
        .unwrap_or(0);
    set_badge_count(&switcher.button_badge, button_unread);
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn sample_folder(special: SpecialUse, imap_name: &str, delimiter: &str) -> Folder {
        Folder {
            id: FolderId(1),
            account_id: AccountId(Uuid::nil()),
            imap_name: imap_name.to_string(),
            delimiter: delimiter.to_string(),
            special,
            selectable: true,
            uidvalidity: None,
            uidnext: None,
            highest_modseq: None,
            unread: 0,
            total: 0,
        }
    }

    fn named(id: i64, special: SpecialUse, imap_name: &str) -> Folder {
        let mut folder = sample_folder(special, imap_name, "/");
        folder.id = FolderId(id);
        folder
    }

    #[test]
    fn folder_icon_css_class_maps_special_use() {
        assert_eq!(
            folder_icon_css_class(&SpecialUse::Inbox),
            "alcedo-folder-icon-inbox"
        );
        assert_eq!(
            folder_icon_css_class(&SpecialUse::Drafts),
            "alcedo-folder-icon-drafts"
        );
        assert_eq!(
            folder_icon_css_class(&SpecialUse::Sent),
            "alcedo-folder-icon-sent"
        );
        assert_eq!(
            folder_icon_css_class(&SpecialUse::Junk),
            "alcedo-folder-icon-junk"
        );
        assert_eq!(
            folder_icon_css_class(&SpecialUse::Trash),
            "alcedo-folder-icon-trash"
        );
        assert_eq!(
            folder_icon_css_class(&SpecialUse::Archive),
            "alcedo-folder-icon-archive"
        );
        assert_eq!(
            folder_icon_css_class(&SpecialUse::Other),
            "alcedo-folder-icon-folder"
        );
    }

    #[test]
    fn archive_icon_is_not_download() {
        crate::test_helpers::gtk_test_run(|| {
            let icon = archive_icon_name();
            assert_ne!(icon, "folder-download-symbolic");
            assert!(
                icon == ARCHIVE_ICON_NAME || icon == ARCHIVE_ICON_FALLBACK,
                "{icon}"
            );
            assert_eq!(folder_icon_name(&SpecialUse::Archive), icon);
        });
    }

    #[test]
    fn folder_display_name_inbox_special_use() {
        let folder = sample_folder(SpecialUse::Inbox, "INBOX", "/");
        assert_eq!(folder_display_name(&folder), "Inbox");
    }

    #[test]
    fn folder_display_name_last_segment() {
        let folder = sample_folder(SpecialUse::Other, "[Gmail]/Sent Mail", "/");
        assert_eq!(folder_display_name(&folder), "Sent Mail");
    }

    #[test]
    fn folder_display_name_empty_delimiter() {
        let folder = sample_folder(SpecialUse::Other, "INBOX", "");
        assert_eq!(folder_display_name(&folder), "INBOX");
    }

    #[test]
    fn folder_parent_name_nested_and_root() {
        assert_eq!(folder_parent_name("INBOX", "/"), None);
        assert_eq!(
            folder_parent_name("[Gmail]/Sent Mail", "/").as_deref(),
            Some("[Gmail]")
        );
        assert_eq!(folder_parent_name("a/b/c", "/").as_deref(), Some("a/b"));
        assert_eq!(
            folder_parent_name("INBOX.Sent", ".").as_deref(),
            Some("INBOX")
        );
    }

    #[test]
    fn sort_folders_special_use_order() {
        let mut folders = vec![
            sample_folder(SpecialUse::Other, "Misc", "/"),
            sample_folder(SpecialUse::Sent, "Sent", "/"),
            sample_folder(SpecialUse::Inbox, "INBOX", "/"),
        ];
        folders[0].id = FolderId(3);
        folders[1].id = FolderId(2);
        folders[2].id = FolderId(1);

        sort_folders(&mut folders);

        assert_eq!(folders[0].id, FolderId(1));
        assert_eq!(folders[1].id, FolderId(2));
        assert_eq!(folders[2].id, FolderId(3));
    }

    #[test]
    fn build_folder_tree_nests_under_existing_parents_only() {
        let folders = vec![
            named(1, SpecialUse::Inbox, "INBOX"),
            named(2, SpecialUse::Sent, "[Gmail]/Sent Mail"),
            named(3, SpecialUse::Other, "[Gmail]"),
            named(4, SpecialUse::Other, "[Gmail]/All Mail"),
            named(5, SpecialUse::Trash, "Trash"),
            named(6, SpecialUse::Other, "Projects/Work"),
            named(7, SpecialUse::Other, "Projects"),
        ];
        let mut gmail = folders[2].clone();
        gmail.selectable = false;

        let mut input = folders;
        input[2] = gmail;

        let tree = build_folder_tree(&input, &HashSet::new());
        let names: Vec<&str> = tree.iter().map(|e| e.folder.imap_name.as_str()).collect();
        assert_eq!(
            names,
            [
                "INBOX",
                "Trash",
                "Projects",
                "Projects/Work",
                "[Gmail]",
                "[Gmail]/Sent Mail",
                "[Gmail]/All Mail",
            ]
        );

        let by_name: HashMap<&str, &FolderTreeEntry> = tree
            .iter()
            .map(|e| (e.folder.imap_name.as_str(), e))
            .collect();
        assert_eq!(by_name["INBOX"].depth, 0);
        assert_eq!(by_name["[Gmail]"].depth, 0);
        assert_eq!(by_name["[Gmail]/Sent Mail"].depth, 1);
        assert_eq!(by_name["[Gmail]/All Mail"].depth, 1);
        assert_eq!(by_name["Projects/Work"].depth, 1);
        assert!(by_name["[Gmail]"].has_children);
        assert!(by_name["Projects"].has_children);
        assert!(by_name["[Gmail]"].is_expanded);
        assert!(!by_name["INBOX"].has_children);
    }

    #[test]
    fn build_folder_tree_hides_children_when_collapsed() {
        let folders = vec![
            named(1, SpecialUse::Inbox, "INBOX"),
            named(2, SpecialUse::Other, "Projects"),
            named(3, SpecialUse::Other, "Projects/Work"),
            named(4, SpecialUse::Other, "[Gmail]"),
            named(5, SpecialUse::Sent, "[Gmail]/Sent Mail"),
        ];
        let collapsed = HashSet::from(["Projects".to_string(), "[Gmail]".to_string()]);
        let tree = build_folder_tree(&folders, &collapsed);
        let names: Vec<&str> = tree.iter().map(|e| e.folder.imap_name.as_str()).collect();
        assert_eq!(names, ["INBOX", "Projects", "[Gmail]"]);
        let by_name: HashMap<&str, &FolderTreeEntry> = tree
            .iter()
            .map(|e| (e.folder.imap_name.as_str(), e))
            .collect();
        assert!(by_name["Projects"].has_children);
        assert!(!by_name["Projects"].is_expanded);
        assert!(by_name["[Gmail]"].has_children);
        assert!(!by_name["[Gmail]"].is_expanded);
    }

    fn named_with_unread(id: i64, imap_name: &str, unread: u32) -> Folder {
        let mut folder = named(id, SpecialUse::Other, imap_name);
        folder.unread = unread;
        folder
    }

    #[test]
    fn collapsed_parent_reports_unread_from_all_hidden_depths() {
        let folders = vec![
            named_with_unread(1, "Projects", 1),
            named_with_unread(2, "Projects/Work", 2),
            named_with_unread(3, "Projects/Work/Old", 4),
            named_with_unread(4, "Projects/Home", 0),
        ];
        let collapsed = HashSet::from(["Projects".to_string()]);
        let tree = build_folder_tree(&folders, &collapsed);

        assert_eq!(tree.len(), 1);
        assert_eq!(tree[0].hidden_unread, 6);
        assert_eq!(folder_badge_total(&tree[0]), 7);
        assert!(folder_badge_tooltip(&tree[0]).is_some());
    }

    #[test]
    fn expanded_parent_has_no_hidden_unread() {
        let folders = vec![
            named_with_unread(1, "Projects", 1),
            named_with_unread(2, "Projects/Work", 2),
        ];
        let tree = build_folder_tree(&folders, &HashSet::new());

        assert_eq!(tree[0].hidden_unread, 0);
        assert_eq!(folder_badge_total(&tree[0]), 1);
        assert!(folder_badge_tooltip(&tree[0]).is_none());
    }

    #[test]
    fn collapsed_parent_with_read_children_has_no_hidden_unread() {
        let folders = vec![
            named_with_unread(1, "Projects", 0),
            named_with_unread(2, "Projects/Work", 0),
        ];
        let collapsed = HashSet::from(["Projects".to_string()]);
        let tree = build_folder_tree(&folders, &collapsed);

        assert_eq!(folder_badge_total(&tree[0]), 0);
    }

    #[test]
    fn folder_is_under_nested_imap_paths() {
        assert!(folder_is_under("Projects/Work", "Projects", "/"));
        assert!(folder_is_under("[Gmail]/Sent Mail", "[Gmail]", "/"));
        assert!(!folder_is_under("Projects", "Projects", "/"));
        assert!(!folder_is_under("INBOX", "Projects", "/"));
        assert!(!folder_is_under("ProjectsWork", "Projects", "/"));
    }

    #[test]
    fn imap_ancestor_names_skips_missing_intermediates() {
        let known = HashSet::from(["[Gmail]".to_string(), "[Gmail]/Sent Mail".to_string()]);
        assert_eq!(
            imap_ancestor_names("[Gmail]/Sent Mail", "/", &known),
            vec!["[Gmail]".to_string()]
        );
        assert!(imap_ancestor_names("INBOX", "/", &known).is_empty());
    }

    #[test]
    fn build_folder_tree_orphan_nested_path_is_root() {
        let folders = vec![named(1, SpecialUse::Other, "MissingParent/Child")];
        let tree = build_folder_tree(&folders, &HashSet::new());
        assert_eq!(tree.len(), 1);
        assert_eq!(tree[0].depth, 0);
        assert_eq!(tree[0].folder.imap_name, "MissingParent/Child");
    }

    #[test]
    fn find_default_folder_prefers_selectable_inbox() {
        let mut folders = vec![
            sample_folder(SpecialUse::Other, "[Gmail]", "/"),
            sample_folder(SpecialUse::Sent, "Sent", "/"),
            sample_folder(SpecialUse::Inbox, "INBOX", "/"),
        ];
        folders[0].selectable = false;
        folders[0].id = FolderId(4);
        folders[1].id = FolderId(2);
        folders[2].id = FolderId(1);

        assert_eq!(find_default_folder(&folders), Some(FolderId(1)));
    }

    #[test]
    fn find_default_folder_falls_back_to_first_selectable() {
        let mut folder = sample_folder(SpecialUse::Sent, "Sent", "/");
        folder.id = FolderId(2);
        assert_eq!(find_default_folder(&[folder]), Some(FolderId(2)));
    }

    fn ext_spec(n: u128, section: Option<(&str, bool)>) -> MailViewSpec {
        MailViewSpec {
            id: ExtensionListId(Uuid::from_u128(n)),
            title: format!("View {n}"),
            icon_name: "folder-symbolic",
            tooltip: String::new(),
            unread: 1,
            status_detail: String::new(),
            column_scope: format!("ext:{n}"),
            search_query: None,
            section: section.map(|(title, collapsed)| ViewSection {
                title: title.to_string(),
                collapsed,
            }),
            reorderable: false,
            restore_on_start: false,
            inbox_like: false,
            sort_newest_first: false,
            loading: false,
            empty_title: "Empty",
            empty_description: None,
            header_toggle: None,
            handles_delete: false,
        }
    }

    #[test]
    fn folder_bar_sections_puts_extension_sections_last_with_header() {
        let views = [
            ext_spec(1, Some(("Saved", false))),
            ext_spec(2, Some(("Saved", false))),
        ];
        let sections = folder_bar_sections(&[], &HashSet::new(), false, &views);
        let unified = sections
            .iter()
            .position(|section| matches!(section, FolderBarSection::Unified))
            .expect("unified");
        let header = sections
            .iter()
            .position(|section| *section == FolderBarSection::SectionHeader("Saved".into()))
            .expect("section header");
        assert!(unified < header);
        assert_eq!(sections[header - 1], FolderBarSection::Separator);
        assert_eq!(
            &sections[header + 1..],
            [
                FolderBarSection::Extension(views[0].id),
                FolderBarSection::Extension(views[1].id),
            ]
        );
    }

    #[test]
    fn untitled_section_has_rows_without_header() {
        let views = [
            ext_spec(1, Some(("", false))),
            ext_spec(2, Some(("", false))),
        ];
        let sections = folder_bar_sections(&[], &HashSet::new(), false, &views);
        assert_eq!(
            sections,
            [
                FolderBarSection::Unified,
                FolderBarSection::Separator,
                FolderBarSection::Extension(views[0].id),
                FolderBarSection::Extension(views[1].id),
            ]
        );
    }

    #[test]
    fn folder_bar_sections_collapsed_section_keeps_header_hides_rows() {
        let views = [ext_spec(1, Some(("Saved", true)))];
        let sections = folder_bar_sections(&[], &HashSet::new(), false, &views);
        assert_eq!(
            sections.last(),
            Some(&FolderBarSection::SectionHeader("Saved".into()))
        );
        assert!(!sections.contains(&FolderBarSection::Extension(views[0].id)));
    }

    #[test]
    fn extension_sections_group_by_title_in_first_seen_order() {
        let views = [
            ext_spec(1, Some(("B", false))),
            ext_spec(2, None),
            ext_spec(3, Some(("A", false))),
            ext_spec(4, Some(("B", false))),
        ];
        let groups = extension_sections(&views);
        let titles: Vec<&str> = groups.iter().map(|(s, _)| s.title.as_str()).collect();
        assert_eq!(titles, ["B", "A"]);
        assert_eq!(groups[0].1.len(), 2);
        assert_eq!(groups[1].1.len(), 1);
    }

    #[test]
    fn folder_bar_sections_without_extensions_has_no_section_header() {
        let sections = folder_bar_sections(&[], &HashSet::new(), false, &[]);
        assert!(
            sections
                .iter()
                .all(|section| !matches!(section, FolderBarSection::SectionHeader(_)))
        );
        assert_eq!(sections, [FolderBarSection::Unified]);
    }

    #[test]
    fn folder_bar_sections_extension_views_after_unified() {
        let without = folder_bar_sections(&[], &HashSet::new(), false, &[]);
        assert!(
            !without
                .iter()
                .any(|section| matches!(section, FolderBarSection::Extension(_)))
        );
        let spec = ext_spec(7, None);
        let id = spec.id;
        let with = folder_bar_sections(&[], &HashSet::new(), false, &[spec]);
        let unified = with
            .iter()
            .position(|section| matches!(section, FolderBarSection::Unified))
            .expect("unified");
        assert_eq!(with[unified + 1], FolderBarSection::Extension(id));
    }

    #[test]
    fn extension_row_name_round_trips() {
        let id = ExtensionListId(uuid::Uuid::from_u128(42));
        let name = format!("{ROW_EXTENSION_PREFIX}{}", id.0);
        assert_eq!(
            parse_folder_widget_name(&name),
            Some(MailView::Extension(id))
        );
    }

    #[test]
    fn folder_bar_sections_puts_imap_folders_after_unified() {
        let mut inbox = sample_folder(SpecialUse::Inbox, "INBOX", "/");
        inbox.id = FolderId(1);
        let sections = folder_bar_sections(&[inbox], &HashSet::new(), false, &[]);
        let unified = sections
            .iter()
            .position(|section| matches!(section, FolderBarSection::Unified))
            .expect("unified");
        let folder = sections
            .iter()
            .position(|section| matches!(section, FolderBarSection::Folder(_)))
            .expect("imap folder");
        assert_eq!(unified, 0);
        assert!(unified < folder);
        assert!(matches!(sections[folder - 1], FolderBarSection::Separator));
    }

    #[test]
    fn section_header_row_name_is_not_a_view() {
        assert_eq!(
            parse_folder_widget_name(&format!("{ROW_SECTION_PREFIX}Saved")),
            None
        );
    }

    #[test]
    fn find_default_folder_skips_non_selectable_only() {
        let mut folder = sample_folder(SpecialUse::Other, "[Gmail]", "/");
        folder.selectable = false;
        assert_eq!(find_default_folder(&[folder]), None);
    }

    #[test]
    fn resolve_startup_unified_uses_first_account() {
        let account = AccountId(Uuid::parse_str("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa").unwrap());
        let resolved =
            resolve_startup_selection(&LastFolder::UnifiedInbox, None, &[account], &HashMap::new());
        assert_eq!(
            resolved,
            StartupSelection {
                mail_view: MailView::UnifiedInbox,
                selected_account: Some(account),
                pending_restore: None,
            }
        );
    }

    #[test]
    fn resolve_startup_virtual_view_restores_last_account() {
        let first = AccountId(Uuid::from_u128(1));
        let second = AccountId(Uuid::from_u128(2));
        let resolved = resolve_startup_selection(
            &LastFolder::UnifiedInbox,
            Some(second),
            &[first, second],
            &HashMap::new(),
        );
        assert_eq!(resolved.selected_account, Some(second));
    }

    #[test]
    fn resolve_startup_virtual_view_trusts_last_account_before_accounts_load() {
        let second = AccountId(Uuid::from_u128(2));
        let resolved = resolve_startup_selection(
            &LastFolder::UnifiedInbox,
            Some(second),
            &[],
            &HashMap::new(),
        );
        assert_eq!(resolved.selected_account, Some(second));
    }

    #[test]
    fn resolve_startup_virtual_view_ignores_removed_last_account() {
        let first = AccountId(Uuid::from_u128(1));
        let gone = AccountId(Uuid::from_u128(2));
        let resolved = resolve_startup_selection(
            &LastFolder::UnifiedInbox,
            Some(gone),
            &[first],
            &HashMap::new(),
        );
        assert_eq!(resolved.selected_account, Some(first));
    }

    #[test]
    fn resolve_startup_mailbox_when_folders_loaded() {
        let account = AccountId(Uuid::parse_str("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa").unwrap());
        let mut inbox = sample_folder(SpecialUse::Inbox, "INBOX", "/");
        inbox.id = FolderId(7);
        inbox.account_id = account;
        let folders = HashMap::from([(account, vec![inbox])]);
        let resolved = resolve_startup_selection(
            &LastFolder::Mailbox {
                account,
                imap_name: "INBOX".to_string(),
            },
            None,
            &[account],
            &folders,
        );
        assert_eq!(
            resolved,
            StartupSelection {
                mail_view: MailView::Folder(FolderId(7)),
                selected_account: Some(account),
                pending_restore: None,
            }
        );
    }

    #[test]
    fn resolve_startup_mailbox_waits_when_folders_empty() {
        let account = AccountId(Uuid::parse_str("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa").unwrap());
        let resolved = resolve_startup_selection(
            &LastFolder::Mailbox {
                account,
                imap_name: "Sent".to_string(),
            },
            None,
            &[],
            &HashMap::new(),
        );
        assert_eq!(
            resolved,
            StartupSelection {
                mail_view: MailView::UnifiedInbox,
                selected_account: Some(account),
                pending_restore: Some((account, "Sent".to_string())),
            }
        );
    }

    #[test]
    fn resolve_startup_mailbox_falls_back_when_account_gone() {
        let gone = AccountId(Uuid::parse_str("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa").unwrap());
        let live = AccountId(Uuid::parse_str("bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb").unwrap());
        let resolved = resolve_startup_selection(
            &LastFolder::Mailbox {
                account: gone,
                imap_name: "INBOX".to_string(),
            },
            None,
            &[live],
            &HashMap::new(),
        );
        assert_eq!(
            resolved,
            StartupSelection {
                mail_view: MailView::UnifiedInbox,
                selected_account: Some(live),
                pending_restore: None,
            }
        );
    }

    #[test]
    fn find_restore_folder_matches_imap_case_insensitively() {
        let mut inbox = sample_folder(SpecialUse::Inbox, "Inbox", "/");
        inbox.id = FolderId(3);
        assert_eq!(
            find_restore_folder(&[inbox], "INBOX").map(|folder| folder.id),
            Some(FolderId(3))
        );
    }

    #[test]
    fn should_abandon_pending_restore_waits_until_accounts_arrive() {
        let restore = AccountId(Uuid::parse_str("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa").unwrap());
        let other = AccountId(Uuid::parse_str("bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb").unwrap());
        assert!(!should_abandon_pending_restore(restore, &[]));
        assert!(!should_abandon_pending_restore(restore, &[restore, other]));
        assert!(should_abandon_pending_restore(restore, &[other]));
    }

    #[test]
    fn match_pending_folder_restore_waits_on_empty_snapshot() {
        assert_eq!(
            match_pending_folder_restore(&[], "INBOX"),
            FolderRestoreMatch::Wait
        );
    }

    #[test]
    fn match_pending_folder_restore_waits_when_saved_mailbox_absent() {
        let mut sent = sample_folder(SpecialUse::Sent, "Sent", "/");
        sent.id = FolderId(2);
        let mut inbox = sample_folder(SpecialUse::Inbox, "INBOX", "/");
        inbox.id = FolderId(1);
        assert_eq!(
            match_pending_folder_restore(&[sent.clone(), inbox.clone()], "Archive"),
            FolderRestoreMatch::Wait
        );
        assert_eq!(
            match_pending_folder_restore(&[sent, inbox], "Sent"),
            FolderRestoreMatch::Found(FolderId(2))
        );
    }

    #[test]
    fn resolve_startup_mailbox_falls_back_when_imap_missing() {
        let account = AccountId(Uuid::parse_str("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa").unwrap());
        let mut inbox = sample_folder(SpecialUse::Inbox, "INBOX", "/");
        inbox.account_id = account;
        let folders = HashMap::from([(account, vec![inbox])]);
        let resolved = resolve_startup_selection(
            &LastFolder::Mailbox {
                account,
                imap_name: "Sent".to_string(),
            },
            None,
            &[account],
            &folders,
        );
        assert_eq!(
            resolved,
            StartupSelection {
                mail_view: MailView::UnifiedInbox,
                selected_account: Some(account),
                pending_restore: None,
            }
        );
    }

    #[test]
    fn account_switcher_caption_hidden_for_zero_or_one() {
        assert_eq!(account_switcher_caption(0), None);
        assert_eq!(account_switcher_caption(1), None);
    }

    #[test]
    fn account_switcher_caption_shows_plural_count() {
        assert_eq!(account_switcher_caption(2).as_deref(), Some("2 accounts"));
        assert_eq!(account_switcher_caption(12).as_deref(), Some("12 accounts"));
    }

    fn sample_account(id: u128, display_name: &str) -> Account {
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
            auth: alcedo_core::AuthMethod::Password,
            accent_color: String::new(),
            avatar_text: String::new(),
            avatar_image: Vec::new(),
        }
    }

    fn offline(accounts: &[&Account]) -> HashMap<AccountId, String> {
        accounts
            .iter()
            .map(|account| (account.id, "auth".to_string()))
            .collect()
    }

    #[test]
    fn switcher_status_selected_broken_account_says_sign_in_required() {
        let gmail = sample_account(1, "Gmail");
        let status = account_switcher_status(
            std::slice::from_ref(&gmail),
            Some(gmail.id),
            &offline(&[&gmail]),
        );
        assert_eq!(
            status,
            Some(SwitcherCaption {
                text: "Sign-in required".to_string(),
                needs_attention: true,
            })
        );
    }

    #[test]
    fn switcher_status_names_a_broken_account_that_is_not_selected() {
        let work = sample_account(1, "Work");
        let gmail = sample_account(2, "Gmail");
        let status = account_switcher_status(
            &[work.clone(), gmail.clone()],
            Some(work.id),
            &offline(&[&gmail]),
        );
        assert_eq!(
            status,
            Some(SwitcherCaption {
                text: "Gmail needs sign-in".to_string(),
                needs_attention: true,
            })
        );
    }

    #[test]
    fn switcher_status_counts_several_broken_accounts() {
        let work = sample_account(1, "Work");
        let gmail = sample_account(2, "Gmail");
        let status = account_switcher_status(
            &[work.clone(), gmail.clone()],
            Some(work.id),
            &offline(&[&work, &gmail]),
        );
        assert_eq!(
            status,
            Some(SwitcherCaption {
                text: "2 accounts need sign-in".to_string(),
                needs_attention: true,
            })
        );
    }

    #[test]
    fn switcher_status_falls_back_to_the_plain_count_without_problems() {
        let work = sample_account(1, "Work");
        let gmail = sample_account(2, "Gmail");
        let status =
            account_switcher_status(&[work.clone(), gmail], Some(work.id), &HashMap::new());
        assert_eq!(
            status,
            Some(SwitcherCaption {
                text: "2 accounts".to_string(),
                needs_attention: false,
            })
        );
    }

    #[test]
    fn account_added_toast_title_includes_email() {
        assert_eq!(
            account_added_toast_title("carol@gmail.com"),
            "carol@gmail.com added"
        );
    }

    #[test]
    fn from_selected_index_finds_match_or_defaults_to_zero() {
        let first = AccountId(Uuid::from_u128(1));
        let second = AccountId(Uuid::from_u128(2));
        let ids = vec![first, second];
        assert_eq!(from_selected_index(&ids, second), 1);
        assert_eq!(from_selected_index(&ids, AccountId(Uuid::from_u128(99))), 0);
    }

    #[test]
    fn parse_folder_widget_name_unified_and_folder_ids() {
        assert_eq!(
            parse_folder_widget_name(ROW_UNIFIED),
            Some(MailView::UnifiedInbox)
        );
        assert_eq!(
            parse_folder_widget_name("alcedo-folder-42"),
            Some(MailView::Folder(FolderId(42)))
        );
        assert_eq!(
            parse_folder_widget_name("alcedo-folder:7"),
            Some(MailView::Folder(FolderId(7)))
        );
        assert_eq!(parse_folder_widget_name("alcedo-sep"), None);
    }

    #[test]
    fn build_folder_tree_hides_snoozed_and_outbox() {
        let folders = vec![
            named(1, SpecialUse::Inbox, "INBOX"),
            named(2, SpecialUse::Snoozed, "Snoozed"),
            named(3, SpecialUse::Outbox, "Alcedo/Outbox"),
            named(4, SpecialUse::Sent, "Sent"),
        ];
        let tree = build_folder_tree(&folders, &HashSet::new());
        let names: Vec<&str> = tree.iter().map(|e| e.folder.imap_name.as_str()).collect();
        assert_eq!(names, ["INBOX", "Sent"]);
    }

    #[test]
    fn preferred_drop_action_move_unless_ctrl_or_copy_only() {
        let both = gdk::DragAction::MOVE | gdk::DragAction::COPY;
        assert_eq!(preferred_drop_action(both, false), gdk::DragAction::MOVE);
        assert_eq!(preferred_drop_action(both, true), gdk::DragAction::COPY);
        assert_eq!(
            preferred_drop_action(gdk::DragAction::COPY, false),
            gdk::DragAction::COPY
        );
        assert_eq!(
            preferred_drop_action(gdk::DragAction::MOVE, true),
            gdk::DragAction::MOVE
        );
    }

    #[test]
    fn should_expand_on_drop_hover_only_collapsed_parents() {
        assert!(should_expand_on_drop_hover(true, false));
        assert!(!should_expand_on_drop_hover(true, true));
        assert!(!should_expand_on_drop_hover(false, false));
        assert!(!should_expand_on_drop_hover(false, true));
    }

    #[test]
    fn folder_row_with_drop_target_is_freed_after_removal() {
        crate::test_helpers::gtk_test_run(|| {
            assert!(crate::test_helpers::row_is_freed_after_removal(
                |row, sentinel| {
                    let on_drop_sentinel = sentinel.clone();
                    attach_folder_drop_target(
                        row,
                        FolderId(1),
                        Rc::new(move |_, _| {
                            let _ = &sentinel;
                            true
                        }),
                        Rc::new(move |_, _, _| {
                            let _ = &on_drop_sentinel;
                        }),
                    );
                }
            ));
        });
    }

    #[test]
    fn extension_row_with_drop_target_is_freed_after_removal() {
        crate::test_helpers::gtk_test_run(|| {
            assert!(crate::test_helpers::row_is_freed_after_removal(
                |row, sentinel| {
                    attach_extension_drop_target(
                        row,
                        ExtensionListId(uuid::Uuid::nil()),
                        Rc::new(move |_, _| {
                            let _ = &sentinel;
                        }),
                    );
                }
            ));
        });
    }

    #[test]
    fn row_with_secondary_click_is_freed_after_removal() {
        crate::test_helpers::gtk_test_run(|| {
            assert!(crate::test_helpers::row_is_freed_after_removal(
                |row, sentinel| {
                    attach_secondary_click(row, move |_, _| {
                        let _ = &sentinel;
                    });
                }
            ));
        });
    }
}
