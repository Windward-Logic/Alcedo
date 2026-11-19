//! Message list (P4) — ColumnView with a flex Subject column and fixed From/Date.

use alcedo_i18n::{ntr, tr, trs};
use std::cell::{Cell, RefCell};
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use alcedo_core::{
    AccountId, Address, Conversation, Envelope, FolderId, MessageId, RowSummary, SpecialUse,
    ThreadId, avatar_from_label, participant_summary, row_accessible_summary,
};
use chrono::{DateTime, Datelike, Local, Utc};
use relm4::adw;
use relm4::adw::prelude::*;
use relm4::gtk;
use relm4::gtk::gio;
use relm4::gtk::glib;
use relm4::gtk::prelude::{
    ButtonExt, Cast, ListModelExt, ObjectExt, SelectionModelExt, ToggleButtonExt, WidgetExt,
};
use relm4::typed_view::column::{RelmColumn, TypedColumnView};

use crate::a11y;
use crate::account_chip::apply_account_chip_appearance;
use crate::commands;
use crate::folder_bar::{archive_icon_name, folder_icon_css_class, folder_icon_name};
use crate::help::{self, TOPIC_SEARCH};
use crate::icons;
use crate::overflow_tooltip::install_overflow_tooltip;
use crate::search_bar::SearchFilterBar;
#[cfg(not(test))]
use crate::settings::set_message_list_column_widths_for;
use crate::settings::{
    MESSAGE_LIST_CONTEXT_COLUMNS, MESSAGE_LIST_RESIZABLE_COLUMNS, MESSAGE_LIST_TEXT_COLUMNS,
    MESSAGE_LIST_USER_COLUMNS, MessageListColumnScope, ReadingPane, bind_show_message_preview,
    message_list_column_order_for, message_list_column_visible_for, message_list_column_widths_for,
    move_message_list_column, on_reading_pane_changed, on_show_message_preview_changed,
    reading_pane, set_message_list_column_order_for, set_message_list_column_visible_for,
    set_reading_pane, show_message_preview,
};
#[cfg(not(test))]
use crate::settings::{
    message_list_sort_column, message_list_sort_order, set_message_list_sort_column,
    set_message_list_sort_order,
};

mod cells;
mod column_picker;
mod columns;
mod drag;
mod pane;
mod sort;
pub(crate) use self::cells::*;
use self::column_picker::*;
use self::columns::*;
pub use self::drag::*;
pub use self::pane::*;
pub(crate) use self::sort::*;

/// Which virtual mail view drives empty-list copy.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ListEmptyView {
    #[default]
    Default,
    /// Copy supplied by an extension view.
    Extension {
        title: &'static str,
        description: Option<&'static str>,
    },
}

/// Wide enough that an end-aligned grip clears the selected row's left accent
/// bar. The whole column stays the grab target.
const DRAG_WIDTH: i32 = 48;
const UNREAD_WIDTH: i32 = 24;
const ICON_WIDTH: i32 = 16;
/// Nested offset for expanded thread children (matches folder-pane depth step).
const THREAD_CHILD_INDENT: i32 = 16;
const ATTACHMENTS_WIDTH: i32 = 24;
const STAR_WIDTH: i32 = 24;
const LAST_COLUMN_PAD: i32 = 14;
const ICON_COLUMNS: &[&str] = &["unread", "attachments", "star", "account-chip"];
const FROM_MIN: i32 = 80;
const FROM_DEFAULT: i32 = 160;
const FOLDER_MIN: i32 = 80;
const FOLDER_DEFAULT: i32 = 120;
const ACCOUNT_MIN: i32 = 80;
const ACCOUNT_DEFAULT: i32 = 120;
const ACCOUNT_CHIP_WIDTH: i32 = 32;
const ACCOUNT_CHIP_SIZE: i32 = 24;
const SUBJECT_MIN: i32 = 120;
const SUBJECT_DEFAULT: i32 = 220;
const DATE_MIN: i32 = 96;
const DATE_DEFAULT: i32 = 120;
const SIZE_MIN: i32 = 96;
const SIZE_DEFAULT: i32 = 96;

static PIP_ACCENT_CSS: LazyLock<Mutex<std::collections::HashSet<String>>> =
    LazyLock::new(|| Mutex::new(std::collections::HashSet::new()));

type FlagToggleHandler = Rc<dyn Fn(MessageId, bool)>;
type ContextMessageHandler = Rc<dyn Fn(MessageId, f64, f64)>;
type ThreadExpandHandler = Rc<dyn Fn(ThreadId)>;

thread_local! {
    static ON_TOGGLE_FLAG: RefCell<Option<FlagToggleHandler>> = const { RefCell::new(None) };
    static ON_TOGGLE_THREAD_EXPAND: RefCell<Option<ThreadExpandHandler>> = const { RefCell::new(None) };
    static ON_CONTEXT_MESSAGE: RefCell<Option<ContextMessageHandler>> = const { RefCell::new(None) };
    static SUPPRESS_LIST_SELECTION: Cell<u32> = const { Cell::new(0) };
    static IGNORE_FOCUS_SELECTION: Cell<u32> = const { Cell::new(0) };
    static LIST_SELECTION_GEN: Cell<u64> = const { Cell::new(0) };
    static SUPPRESS_COLUMN_ORDER_PERSIST: Cell<bool> = const { Cell::new(false) };
    static SUPPRESS_COLUMN_WIDTH_PERSIST: Cell<bool> = const { Cell::new(false) };
    static COLUMN_SCOPE: RefCell<MessageListColumnScope> =
        const { RefCell::new(MessageListColumnScope::UnifiedInbox) };
    static PREFERRED_COLUMN_WIDTHS: RefCell<Option<Rc<RefCell<HashMap<String, i32>>>>> =
        const { RefCell::new(None) };
    static FOLDER_LABELS: RefCell<HashMap<FolderId, FolderLabel>> = RefCell::new(HashMap::new());
    static ACCOUNT_LABELS: RefCell<HashMap<AccountId, String>> = RefCell::new(HashMap::new());
    static ACCOUNT_CHIPS: RefCell<HashMap<AccountId, AccountChipSnap>> = RefCell::new(HashMap::new());
    static COLUMN_PICKER_ACTIONS: RefCell<Option<Rc<RefCell<HashMap<String, gio::SimpleAction>>>>> =
        const { RefCell::new(None) };
}

#[derive(Clone, Debug)]
pub struct FolderLabel {
    pub name: String,
    pub special: SpecialUse,
}

pub fn set_folder_labels(map: HashMap<FolderId, FolderLabel>) {
    FOLDER_LABELS.with(|slot| *slot.borrow_mut() = map);
}

pub fn set_account_labels(map: HashMap<AccountId, String>) {
    ACCOUNT_LABELS.with(|slot| *slot.borrow_mut() = map);
}

#[derive(Clone, Debug)]
pub struct AccountChipSnap {
    pub initials: String,
    pub accent: String,
    pub image: Vec<u8>,
}

pub fn set_account_chips(map: HashMap<AccountId, AccountChipSnap>) {
    ACCOUNT_CHIPS.with(|slot| *slot.borrow_mut() = map);
}

fn current_column_scope() -> MessageListColumnScope {
    COLUMN_SCOPE.with(|slot| slot.borrow().clone())
}

/// Load this mailbox's column order, visibility, and widths.
/// A mailbox with no saved layout keeps the shared column settings.
pub fn apply_message_list_columns_for_scope(list: &MessageListView, scope: MessageListColumnScope) {
    let changed = COLUMN_SCOPE.with(|slot| {
        if *slot.borrow() == scope {
            return false;
        }
        *slot.borrow_mut() = scope.clone();
        true
    });
    if !changed {
        return;
    }
    let widths = message_list_column_widths_for(&scope);
    PREFERRED_COLUMN_WIDTHS.with(|slot| {
        if let Some(preferred) = slot.borrow().as_ref() {
            *preferred.borrow_mut() = widths;
        }
    });
    let order_was = SUPPRESS_COLUMN_ORDER_PERSIST.get();
    let width_was = SUPPRESS_COLUMN_WIDTH_PERSIST.get();
    SUPPRESS_COLUMN_ORDER_PERSIST.set(true);
    SUPPRESS_COLUMN_WIDTH_PERSIST.set(true);
    apply_column_order_for_view(&list.view, &message_list_column_order_for(&scope));
    reapply_saved_user_column_visibility(list, false);
    apply_saved_column_widths(&list.view);
    SUPPRESS_COLUMN_ORDER_PERSIST.set(order_was);
    SUPPRESS_COLUMN_WIDTH_PERSIST.set(width_was);
}

/// Reapply picker column visibility from GSettings. Folder and account stay on while `searching`.
pub fn reapply_message_list_column_visibility(list: &MessageListView, searching: bool) {
    reapply_saved_user_column_visibility(list, searching);
}

pub fn set_deleted_column_visible(list: &MessageListView, visible: bool) {
    if let Some(col) = column_by_id(&list.view, "deleted") {
        col.set_visible(visible);
        apply_column_expand_for_view(&list.view);
    }
}

pub fn apply_trash_deleted_sort(list: &MessageListView) {
    if let Some(col) = column_by_id(&list.view, "deleted") {
        list.view
            .sort_by_column(Some(&col), gtk::SortType::Descending);
    }
}

pub fn apply_newest_first_sort(list: &MessageListView) {
    if let Some(col) = column_by_id(&list.view, "date") {
        list.view
            .sort_by_column(Some(&col), gtk::SortType::Descending);
    }
}

#[cfg(not(test))]
pub fn apply_saved_message_list_sort(list: &MessageListView) {
    apply_saved_sort(list);
}

#[cfg(test)]
pub fn apply_saved_message_list_sort(list: &MessageListView) {
    let _ = list;
}

pub fn set_flag_toggle_handler(handler: FlagToggleHandler) {
    ON_TOGGLE_FLAG.with(|slot| {
        *slot.borrow_mut() = Some(handler);
    });
}

pub fn set_thread_expand_handler(handler: ThreadExpandHandler) {
    ON_TOGGLE_THREAD_EXPAND.with(|slot| {
        *slot.borrow_mut() = Some(handler);
    });
}

pub fn set_context_message_handler(handler: ContextMessageHandler) {
    ON_CONTEXT_MESSAGE.with(|slot| {
        *slot.borrow_mut() = Some(handler);
    });
}

/// True while the list is being mutated in place so GTK selection signals
/// are not treated as user navigation.
pub fn list_selection_suppressed() -> bool {
    SUPPRESS_LIST_SELECTION.with(|flag| flag.get() > 0)
}

/// Ignore GTK selection signals while mutating or refocusing the list.
pub fn suppress_list_selection() -> impl Drop {
    SuppressListSelection::enter()
}

const FOCUS_SELECTION_IGNORE_MS: u64 = 400;

/// Ignore list selection changes from a focus grab (FileLauncher, grab_focus)
/// long enough for Relm to process the GTK signal.
pub fn ignore_focus_selection_briefly() {
    IGNORE_FOCUS_SELECTION.with(|flag| flag.set(flag.get() + 1));
    glib::timeout_add_local_once(Duration::from_millis(FOCUS_SELECTION_IGNORE_MS), || {
        IGNORE_FOCUS_SELECTION.with(|flag| flag.set(flag.get().saturating_sub(1)));
    });
}

pub fn focus_selection_ignored() -> bool {
    IGNORE_FOCUS_SELECTION.with(|flag| flag.get() > 0)
}

/// Bump when the bitset commits a real selection so a later idle cannot
/// restore a captured reader id the list has already left.
pub(crate) fn note_list_selection_commit() {
    LIST_SELECTION_GEN.with(|cell| cell.set(cell.get().wrapping_add(1)));
}

fn list_selection_generation() -> u64 {
    LIST_SELECTION_GEN.with(|cell| cell.get())
}

/// Focus-grab echoes look like an empty bitset or the same single id.
/// A different single id or a multi-set is a user commit.
pub(crate) fn focus_ignore_is_commit(gtk_ids: &[MessageId], previous: Option<MessageId>) -> bool {
    match gtk_ids {
        [] => false,
        [id] if previous == Some(*id) => false,
        _ => true,
    }
}

/// Restore the reader row after refresh only when GTK has nothing selected.
pub(crate) fn refresh_should_restore_reader(
    gtk_ids: &[MessageId],
    selected: Option<MessageId>,
) -> bool {
    gtk_ids.is_empty() && selected.is_some()
}

/// Non-empty bitset that does not include `selected` is newer; do not overwrite it.
pub(crate) fn refresh_follows_newer_bitset(
    gtk_ids: &[MessageId],
    selected: Option<MessageId>,
) -> bool {
    !gtk_ids.is_empty() && selected.is_some_and(|id| !gtk_ids.contains(&id))
}

/// If the bitset already committed a different single id, open that instead of
/// a stale `requested` id. Empty and multi bitsets keep `requested`.
pub(crate) fn open_id_following_bitset(requested: MessageId, gtk_ids: &[MessageId]) -> MessageId {
    match gtk_ids {
        [other] if *other != requested => *other,
        _ => requested,
    }
}

/// Idle restore applies only when the bitset is still empty and selection has
/// not committed a newer generation since this sync was scheduled.
fn idle_sync_applies(scheduled_gen: u64, current_gen: u64, gtk_empty: bool) -> bool {
    scheduled_gen == current_gen && gtk_empty
}

/// Keep GTK's current selection unless it was cleared; then fall back to the
/// message still shown in the reader.
pub fn ids_to_preserve_on_refresh(
    gtk_selected: &[MessageId],
    reader_id: Option<MessageId>,
) -> Vec<MessageId> {
    if gtk_selected.is_empty() {
        reader_id.into_iter().collect()
    } else {
        gtk_selected.to_vec()
    }
}

/// Selection to restore after a list refresh. Empty lists drop every id so a
/// stale reader message is not re-selected when search or a folder has no rows.
pub fn ids_to_restore_on_refresh(
    rows: &[MessageRow],
    gtk_selected: &[MessageId],
    reader_id: Option<MessageId>,
    _conversation_mode: bool,
) -> Vec<MessageId> {
    if rows.is_empty() {
        return Vec::new();
    }
    let preserved = ids_to_preserve_on_refresh(gtk_selected, reader_id);
    preserved
        .into_iter()
        .filter(|id| rows.iter().any(|row| row.envelope.id == *id))
        .collect()
}

struct SuppressListSelection;

impl SuppressListSelection {
    fn enter() -> Self {
        SUPPRESS_LIST_SELECTION.with(|flag| flag.set(flag.get() + 1));
        Self
    }
}

impl Drop for SuppressListSelection {
    fn drop(&mut self) {
        SUPPRESS_LIST_SELECTION.with(|flag| flag.set(flag.get().saturating_sub(1)));
    }
}

fn apply_pip_accent(pip: &gtk::Box, account_id: AccountId, accent: &str) {
    let class = format!("alcedo-pip-{}", account_id.0.as_simple());
    pip.set_css_classes(&["alcedo-pip", &class]);

    let mut registered = PIP_ACCENT_CSS.lock().expect("pip accent css lock");
    if registered.insert(class.clone()) {
        let css = format!(".{class} {{ background-color: {accent}; }}");
        let provider = gtk::CssProvider::new();
        provider.load_from_string(&css);
        gtk::style_context_add_provider_for_display(
            &pip.display(),
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}

/// Row model for the unified inbox list.
#[derive(Clone, Debug)]
pub struct MessageRow {
    pub envelope: Envelope,
    pub thread_id: ThreadId,
    /// 0 = collapsed thread root; 1 = expanded child.
    pub depth: u8,
    /// Latest thread date for roots (column sort); own date for flat rows/children.
    pub sort_date: DateTime<Utc>,
    /// Latest thread size for roots (column sort); own size for flat rows/children.
    pub sort_size: i64,
    pub message_count: u32,
    pub unread_count: u32,
    pub any_flagged: bool,
    pub any_attachments: bool,
    pub member_ids: Vec<MessageId>,
    pub participants: Vec<Address>,
    pub show_expand: bool,
    pub thread_expanded: bool,
    /// Shared subject key for thread column sort (root + children).
    pub sort_subject: String,
}

impl MessageRow {
    pub fn from_envelope(envelope: Envelope) -> Self {
        let id = envelope.id;
        let date = envelope.date;
        let sort_subject = envelope.display_subject();
        Self {
            thread_id: envelope.thread_id,
            depth: 0,
            sort_date: date,
            sort_size: envelope.size_bytes,
            participants: envelope.from.clone(),
            member_ids: vec![id],
            message_count: 1,
            unread_count: u32::from(!envelope.seen),
            any_flagged: envelope.flagged,
            any_attachments: envelope.has_attachments,
            show_expand: false,
            thread_expanded: false,
            sort_subject,
            envelope,
        }
    }

    pub fn from_conversation_member(
        envelope: &Envelope,
        conversation: &Conversation,
        depth: u8,
        sort_date: DateTime<Utc>,
        sort_size: i64,
        show_expand: bool,
        thread_expanded: bool,
    ) -> Self {
        let unread_count = if depth == 0 {
            conversation.unread_count
        } else {
            u32::from(!envelope.seen)
        };
        let any_flagged = if depth == 0 {
            conversation.any_flagged
        } else {
            envelope.flagged
        };
        let any_attachments = if depth == 0 {
            conversation.any_attachments
        } else {
            envelope.has_attachments
        };
        let sort_subject = conversation
            .members
            .first()
            .unwrap_or(&conversation.latest)
            .display_subject();
        Self {
            envelope: envelope.clone(),
            thread_id: conversation.thread_id,
            depth,
            sort_date,
            sort_size,
            message_count: conversation.message_count,
            unread_count,
            any_flagged,
            any_attachments,
            member_ids: conversation.member_ids.clone(),
            participants: conversation.participants.clone(),
            show_expand,
            thread_expanded,
            sort_subject,
        }
    }

    pub fn from_conversation(conversation: &Conversation) -> Self {
        let members = conversation_members_ordered(conversation);
        let root = members.first().expect("conversation has a member");
        Self::from_conversation_member(
            root,
            conversation,
            0,
            conversation.latest.date,
            conversation.latest.size_bytes,
            false,
            false,
        )
    }
}

/// Flatten grouped conversations into list rows (collapsed root + optional children).
pub fn flatten_conversation_rows(
    conversations: &[Conversation],
    expanded: &HashSet<ThreadId>,
) -> Vec<MessageRow> {
    let mut rows = Vec::new();
    for conversation in conversations {
        let members = conversation_members_ordered(conversation);
        if members.is_empty() {
            continue;
        }
        let sort_date = conversation.latest.date;
        let sort_size = conversation.latest.size_bytes;
        let is_expanded = expanded.contains(&conversation.thread_id);
        let show_expand = conversation.message_count > 1;
        rows.push(MessageRow::from_conversation_member(
            &members[0],
            conversation,
            0,
            sort_date,
            sort_size,
            show_expand,
            is_expanded,
        ));
        if is_expanded && members.len() > 1 {
            for child in &members[1..] {
                rows.push(MessageRow::from_conversation_member(
                    child,
                    conversation,
                    1,
                    sort_date,
                    sort_size,
                    false,
                    false,
                ));
            }
        }
    }
    rows
}

fn conversation_members_ordered(conversation: &Conversation) -> Vec<Envelope> {
    if !conversation.members.is_empty() {
        return conversation.members.clone();
    }
    vec![conversation.latest.clone()]
}

pub type MessageListView = TypedColumnView<MessageRow, gtk::MultiSelection>;

pub fn selected_positions(list: &MessageListView) -> Vec<u32> {
    let bitset = list.selection_model.selection();
    let size = bitset.size() as u32;
    (0..size).map(|i| bitset.nth(i)).collect()
}

pub fn selected_ids_from_selection(selection_model: &gtk::MultiSelection) -> Vec<MessageId> {
    let Some(model) = selection_model.model() else {
        return Vec::new();
    };
    let bitset = selection_model.selection();
    (0..bitset.size() as u32)
        .filter_map(|i| visible_message_id(&model, bitset.nth(i)))
        .collect()
}

pub fn selected_ids(list: &MessageListView) -> Vec<MessageId> {
    selected_ids_from_selection(&list.selection_model)
}

pub fn selected_member_ids(list: &MessageListView) -> Vec<MessageId> {
    selected_ids(list)
}

pub fn member_ids_for_row(list: &MessageListView, message_id: MessageId) -> Vec<MessageId> {
    find_row_by_message_id(list, message_id)
        .map(|row| vec![row.envelope.id])
        .unwrap_or_else(|| vec![message_id])
}

pub fn row_for_latest_id(list: &MessageListView, message_id: MessageId) -> Option<MessageRow> {
    find_row_by_message_id(list, message_id)
}

fn find_row_by_message_id(list: &MessageListView, message_id: MessageId) -> Option<MessageRow> {
    (0..list.len()).find_map(|i| {
        list.get(i).and_then(|item| {
            let row = item.borrow();
            (row.envelope.id == message_id).then(|| row.clone())
        })
    })
}

pub fn selection_count(list: &MessageListView) -> u32 {
    list.selection_model.selection().size() as u32
}

#[cfg(not(test))]
fn restore_selection_on_model(selection_model: &gtk::MultiSelection, ids: &[MessageId]) {
    let Some(model) = selection_model.model() else {
        return;
    };
    let _guard = SuppressListSelection::enter();
    selection_model.unselect_all();
    for id in ids {
        let position =
            (0..model.n_items()).find(|&pos| visible_message_id(&model, pos) == Some(*id));
        if let Some(pos) = position {
            selection_model.select_item(pos, false);
        }
    }
}

pub fn restore_selection(list: &MessageListView, ids: &[MessageId]) {
    list.selection_model.unselect_all();
    for id in ids {
        if let Some(pos) = find_visible_position(list, *id) {
            list.selection_model.select_item(pos, false);
        }
    }
}

pub fn select_ids(list: &MessageListView, ids: &[MessageId]) {
    restore_selection(list, ids);
}

/// Put the reader target back in the list after a focus-driven GTK reset.
pub fn restore_reader_row(list: &MessageListView, id: MessageId) {
    restore_reader_row_for_model(&list.selection_model, &list.view, id);
}

fn visible_message_id(model: &gio::ListModel, position: u32) -> Option<MessageId> {
    let item = model
        .item(position)?
        .downcast::<glib::BoxedAnyObject>()
        .ok()?;
    item.try_borrow::<MessageRow>()
        .ok()
        .map(|row| row.envelope.id)
}

fn restore_reader_row_for_model(
    selection_model: &gtk::MultiSelection,
    view: &gtk::ColumnView,
    id: MessageId,
) {
    let position = selection_model.model().and_then(|model| {
        (0..model.n_items()).find(|&pos| visible_message_id(&model, pos) == Some(id))
    });
    if let Some(pos) = position {
        let _guard = SuppressListSelection::enter();
        selection_model.unselect_all();
        selection_model.select_item(pos, false);
        view.scroll_to(pos, None, gtk::ListScrollFlags::NONE, None);
    }
}

/// True when the visible row already shows the same read/star flags.
pub fn list_row_shows_flags(
    list: &MessageListView,
    id: MessageId,
    seen: bool,
    flagged: bool,
) -> bool {
    find_visible_position(list, id)
        .and_then(|pos| list.get_visible(pos))
        .is_some_and(|item| {
            let env = &item.borrow().envelope;
            env.seen == seen && env.flagged == flagged
        })
}

/// Write the reader id into the list only when the bitset is empty.
pub fn sync_list_selection_to_reader(list: &MessageListView, reader_id: Option<MessageId>) {
    restore_reader_selection_if_cleared(&list.selection_model, &list.view, reader_id);
}

fn restore_reader_selection_if_cleared(
    selection_model: &gtk::MultiSelection,
    view: &gtk::ColumnView,
    reader_id: Option<MessageId>,
) {
    if selection_model.selection().is_empty()
        && let Some(id) = reader_id
    {
        restore_reader_row_for_model(selection_model, view, id);
    }
}

/// Re-sync list selection on the next main-loop turn, after GTK settles.
/// A newer bitset commit invalidates the captured reader id.
pub fn schedule_sync_list_selection_to_reader(
    list: &MessageListView,
    reader_id: Option<MessageId>,
) {
    let selection_model = list.selection_model.clone();
    let view = list.view.clone();
    let scheduled = list_selection_generation();
    glib::idle_add_local_once(move || {
        if !idle_sync_applies(
            scheduled,
            list_selection_generation(),
            selection_model.selection().is_empty(),
        ) {
            return;
        }
        restore_reader_selection_if_cleared(&selection_model, &view, reader_id);
    });
}

pub fn new_list_view() -> MessageListView {
    let mut list = MessageListView::new();
    list.append_column::<DragHandleColumn>();
    list.append_column::<UnreadColumn>();
    list.append_column::<FromColumn>();
    list.append_column::<FolderColumn>();
    list.append_column::<AccountColumn>();
    list.append_column::<AccountChipColumn>();
    list.append_column::<SubjectColumn>();
    list.append_column::<AttachmentsColumn>();
    list.append_column::<StarColumn>();
    list.append_column::<SizeColumn>();
    list.append_column::<DateColumn>();
    list.append_column::<DeletedColumn>();
    for (id, col) in list.get_columns() {
        col.set_id(Some(*id));
        if *id == "deleted" {
            col.set_visible(false);
        }
    }
    list.view.set_reorderable(true);
    list.view.set_hexpand(true);
    list.view.set_halign(gtk::Align::Fill);
    list.view.set_overflow(gtk::Overflow::Hidden);
    apply_column_size_constraints(&list);
    #[cfg(not(test))]
    {
        apply_saved_column_order(&list);
        apply_saved_column_visibility(&list);
        apply_column_expand(&list);
        apply_saved_column_widths(&list.view);
        apply_saved_sort(&list);
        let preferred_widths = Rc::new(RefCell::new(message_list_column_widths_for(
            &current_column_scope(),
        )));
        PREFERRED_COLUMN_WIDTHS.with(|slot| *slot.borrow_mut() = Some(preferred_widths.clone()));
        wire_column_width_persist(&list, preferred_widths);
        wire_column_order_persist(&list);
        wire_sort_persist(&list);
        wire_header_alignment(&list);
        list.view.set_enable_rubberband(true);
        install_column_view_press_snapshot(&list.view);
    }
    list
}

pub fn find_visible_position(list: &MessageListView, id: MessageId) -> Option<u32> {
    let mut i = 0;
    while let Some(item) = list.get_visible(i) {
        if item.borrow().envelope.id == id {
            return Some(i);
        }
        i += 1;
    }
    None
}

pub fn mutate_row_at(list: &MessageListView, pos: u32, row: &MessageRow) {
    let _guard = SuppressListSelection::enter();
    let mut sort_keys_changed = false;
    if let Some(item) = list.get(pos) {
        let mut existing = item.borrow_mut();
        sort_keys_changed = row_sort_keys_changed(&existing, row);
        *existing = row.clone();
    }
    if sort_keys_changed && let Some(sorter) = list.view.sorter() {
        sorter.changed(gtk::SorterChange::Different);
    }
    notify_store_item_changed(list, pos);
    apply_row_visuals(&list.view, row);
}

/// Interior mutates of `BoxedAnyObject` do not notify Gio. Tell the store the
/// item at `pos` changed so ColumnView factories rebind unread/star cells.
fn underlying_list_store(model: gio::ListModel) -> Option<gio::ListStore> {
    let mut model = model;
    loop {
        if let Ok(store) = model.clone().downcast::<gio::ListStore>() {
            return Some(store);
        }
        let next = if let Ok(sort) = model.clone().downcast::<gtk::SortListModel>() {
            sort.model()
        } else if let Ok(filter) = model.clone().downcast::<gtk::FilterListModel>() {
            filter.model()
        } else {
            None
        };
        match next {
            Some(inner) => model = inner,
            None => return None,
        }
    }
}

/// Replace every row in one `ListStore` splice, then the caller restores
/// selection and scroll. `extend_from_iter` is the fallback if the store
/// cannot be reached.
fn replace_message_rows(list: &mut MessageListView, rows: &[MessageRow]) {
    let objects: Vec<glib::BoxedAnyObject> = rows
        .iter()
        .cloned()
        .map(glib::BoxedAnyObject::new)
        .collect();
    if let Some(model) = list.selection_model.model()
        && let Some(store) = underlying_list_store(model)
    {
        let removals = store.n_items();
        store.splice(0, removals, &objects);
        return;
    }
    list.clear();
    list.extend_from_iter(rows.iter().cloned());
}

fn notify_store_item_changed(list: &MessageListView, pos: u32) {
    let Some(model) = list.selection_model.model() else {
        return;
    };
    let Some(store) = underlying_list_store(model) else {
        return;
    };
    if pos < store.n_items() {
        store.items_changed(pos, 1, 1);
    }
}

fn sync_row_flags_from_envelope(row: &mut MessageRow, envelope: &Envelope) {
    row.envelope = envelope.clone();
    if row.message_count == 1 || row.depth > 0 {
        row.unread_count = u32::from(!envelope.seen);
        row.any_flagged = envelope.flagged;
        row.any_attachments = envelope.has_attachments;
    }
    if row.message_count == 1 && row.depth == 0 {
        row.sort_subject = envelope.display_subject();
    }
}

pub fn update_envelope_in_list(
    list: &mut MessageListView,
    pos: u32,
    envelope: &Envelope,
    _scrolled_window: Option<&gtk::ScrolledWindow>,
    _reader_id: Option<MessageId>,
) {
    let _guard = SuppressListSelection::enter();
    let row = list
        .get(pos)
        .map(|item| item.borrow().clone())
        .map(|mut row| {
            sync_row_flags_from_envelope(&mut row, envelope);
            row
        })
        .unwrap_or_else(|| MessageRow::from_envelope(envelope.clone()));
    mutate_row_at(list, pos, &row);
}

pub fn find_message_position(list: &MessageListView, id: MessageId) -> Option<u32> {
    (0..list.len()).find(|&i| {
        list.get(i)
            .map(|item| item.borrow().envelope.id == id)
            .unwrap_or(false)
    })
}

/// Remove rows by `MessageId`, highest store index first so later positions stay valid.
pub fn remove_ids_from_list(list: &mut MessageListView, ids: &[MessageId]) {
    let _guard = SuppressListSelection::enter();
    let mut targets: HashSet<MessageId> = ids.iter().copied().collect();
    let mut positions: Vec<u32> = Vec::with_capacity(targets.len());
    for i in 0..list.len() {
        if targets.is_empty() {
            break;
        }
        if list
            .get(i)
            .is_some_and(|item| targets.remove(&item.borrow().envelope.id))
        {
            positions.push(i);
        }
    }
    for pos in positions.into_iter().rev() {
        list.remove(pos);
    }
}

fn list_message_ids(list: &MessageListView) -> HashSet<MessageId> {
    (0..list.len())
        .filter_map(|i| list.get(i).map(|item| item.borrow().envelope.id))
        .collect()
}

fn restore_selection_and_scroll(
    list: &MessageListView,
    selected_ids: &[MessageId],
    scrolled_window: Option<&gtk::ScrolledWindow>,
    scroll_value: Option<f64>,
) {
    restore_selection(list, selected_ids);
    if let (Some(window), Some(value)) = (scrolled_window, scroll_value) {
        restore_scroll_value(window, value);
    }
}

fn restore_scroll_value(window: &gtk::ScrolledWindow, value: f64) {
    let adj = window.vadjustment();
    if adj.upper() - adj.page_size() >= value {
        adj.set_value(value);
    }
    // ColumnView may clamp or scroll-to-selected during this frame; re-apply
    // after layout so a rebuild cannot leave the list pinned at the top.
    let window = window.clone();
    glib::idle_add_local_once(move || {
        window.vadjustment().set_value(value);
    });
}

fn should_reveal_new_mail(old_ids: &HashSet<MessageId>, new_ids: &HashSet<MessageId>) -> bool {
    if old_ids.is_empty() || old_ids.is_disjoint(new_ids) {
        return false;
    }
    new_ids.difference(old_ids).next().is_some()
}

fn newest_added_id(envelopes: &[Envelope], added: &HashSet<MessageId>) -> Option<MessageId> {
    envelopes
        .iter()
        .filter(|env| added.contains(&env.id))
        .max_by(|a, b| a.date.cmp(&b.date).then_with(|| a.id.0.cmp(&b.id.0)))
        .map(|env| env.id)
}

fn at_arrival_edge(adj: &gtk::Adjustment, newest_visible_pos: u32, n_items: u32) -> bool {
    at_arrival_edge_values(
        adj.value(),
        adj.upper(),
        adj.page_size(),
        newest_visible_pos,
        n_items,
    )
}

fn at_arrival_edge_values(
    value: f64,
    upper: f64,
    page_size: f64,
    newest_visible_pos: u32,
    n_items: u32,
) -> bool {
    const SLOP: f64 = 48.0;
    let at_start = newest_visible_pos <= n_items.saturating_sub(1) / 2;
    if at_start {
        value <= SLOP
    } else {
        upper - page_size - value <= SLOP
    }
}

fn new_mail_chip_label(count: u32) -> String {
    match count {
        1 => tr!("New message"),
        n => ntr!("{n} new message", "{n} new messages", n),
    }
}

fn find_visible_position_in_model(model: &gio::ListModel, id: MessageId) -> Option<u32> {
    (0..model.n_items()).find(|&pos| visible_message_id(model, pos) == Some(id))
}

fn reveal_new_mail_at(model: &gio::ListModel, view: &gtk::ColumnView, id: MessageId) {
    if let Some(pos) = find_visible_position_in_model(model, id) {
        view.scroll_to(pos, None, gtk::ListScrollFlags::NONE, None);
        let view = view.clone();
        glib::idle_add_local_once(move || {
            view.scroll_to(pos, None, gtk::ListScrollFlags::NONE, None);
        });
    }
}

fn reveal_new_mail_in_list(list: &MessageListView, id: MessageId) {
    if let Some(model) = list.selection_model.model() {
        reveal_new_mail_at(&model, &list.view, id);
    }
}

/// Repopulate the list while preserving selection and scroll when possible.
pub fn populate_list_preserved(
    list: &mut MessageListView,
    rows: &[MessageRow],
    selected_ids: &[MessageId],
    scrolled_window: Option<&gtk::ScrolledWindow>,
    body_stack: Option<&gtk::Stack>,
    empty_status: Option<&adw::StatusPage>,
    loading: bool,
    searching: bool,
    conversation_mode: bool,
    list_empty_view: ListEmptyView,
    chip: Option<&NewMailChip>,
) {
    let _guard = SuppressListSelection::enter();
    let old_ids = list_message_ids(list);
    let new_ids: HashSet<MessageId> = rows.iter().map(|row| row.envelope.id).collect();

    if old_ids.is_empty() && new_ids.is_empty() {
        if let Some(chip) = chip {
            chip.hide();
        }
        sync_empty_state_from_count(
            body_stack,
            empty_status,
            true,
            loading,
            searching,
            conversation_mode,
            list_empty_view,
        );
        return;
    }

    let identity_changed = old_ids.is_disjoint(&new_ids);

    if identity_changed {
        if let Some(chip) = chip {
            chip.hide();
        }
        let scroll_value = scrolled_window.map(|window| window.vadjustment().value());
        replace_message_rows(list, rows);
        restore_selection_and_scroll(list, selected_ids, scrolled_window, scroll_value);
        sync_empty_state_from_count(
            body_stack,
            empty_status,
            rows.is_empty(),
            loading,
            searching,
            conversation_mode,
            list_empty_view,
        );
        return;
    }

    let to_remove: Vec<MessageId> = old_ids.difference(&new_ids).copied().collect();
    let will_remove = !to_remove.is_empty();
    let added: HashSet<MessageId> = new_ids.difference(&old_ids).copied().collect();
    let will_append = !added.is_empty();
    let reveal = should_reveal_new_mail(&old_ids, &new_ids);
    let envelopes: Vec<Envelope> = if will_remove || reveal {
        rows.iter().map(|row| row.envelope.clone()).collect()
    } else {
        Vec::new()
    };
    let scroll_snapshot = if will_remove || will_append {
        scrolled_window.map(|window| {
            let adj = window.vadjustment();
            (adj.value(), adj.upper(), adj.page_size())
        })
    } else {
        None
    };
    let scroll_value = scroll_snapshot.map(|(value, _, _)| value);

    if will_remove {
        remove_ids_from_list(list, &to_remove);
        if let Some(chip) = chip {
            chip.remove_ids(&to_remove, &envelopes);
        }
    }
    let mut initial_positions: HashMap<MessageId, u32> =
        HashMap::with_capacity(list.len() as usize);
    for i in 0..list.len() {
        if let Some(item) = list.get(i) {
            initial_positions
                .entry(item.borrow().envelope.id)
                .or_insert(i);
        }
    }
    // Update surviving rows before inserts: nothing is shifted yet, so initial indices stay valid.
    for row in rows {
        let Some(&pos) = initial_positions.get(&row.envelope.id) else {
            continue;
        };
        let changed = list.get(pos).is_some_and(|item| {
            let existing = item.borrow();
            existing.envelope != row.envelope
                || existing.depth != row.depth
                || existing.sort_date != row.sort_date
                || existing.message_count != row.message_count
                || existing.unread_count != row.unread_count
                || existing.any_flagged != row.any_flagged
                || existing.any_attachments != row.any_attachments
                || existing.participants != row.participants
                || existing.member_ids != row.member_ids
                || existing.show_expand != row.show_expand
                || existing.thread_expanded != row.thread_expanded
        });
        if changed {
            mutate_row_at(list, pos, row);
        }
    }
    // Inserts in row order; `index.min(list.len())` matches the interleaved algorithm because
    // earlier target slots are already filled by survivors or prior inserts in this pass.
    for (index, row) in rows.iter().enumerate() {
        if initial_positions.contains_key(&row.envelope.id) {
            continue;
        }
        let at = (index as u32).min(list.len());
        list.insert(at, row.clone());
    }

    if reveal {
        restore_selection(list, selected_ids);
        if let Some(newest) = newest_added_id(&envelopes, &added)
            && let Some(pos) = find_visible_position(list, newest)
        {
            let n = list.len();
            let at_edge = scroll_snapshot
                .map(|(value, upper, page_size)| {
                    at_arrival_edge_values(value, upper, page_size, pos, n)
                })
                .unwrap_or(true);
            if at_edge {
                reveal_new_mail_in_list(list, newest);
                if let Some(chip) = chip {
                    chip.hide();
                }
            } else {
                if let (Some(window), Some(value)) = (scrolled_window, scroll_value) {
                    restore_scroll_value(window, value);
                }
                if let Some(chip) = chip {
                    let dock_end = pos > (n.saturating_sub(1) / 2);
                    chip.hold_edge_hide();
                    chip.show(&added, &envelopes, dock_end);
                    chip.release_edge_hide_after_idle();
                }
            }
        } else {
            restore_selection_and_scroll(list, selected_ids, scrolled_window, scroll_value);
        }
    } else if will_remove || will_append {
        restore_selection_and_scroll(list, selected_ids, scrolled_window, scroll_value);
    } else {
        restore_selection(list, selected_ids);
    }

    if rows.is_empty()
        && let Some(chip) = chip
    {
        chip.hide();
    }

    sync_empty_state_from_count(
        body_stack,
        empty_status,
        rows.is_empty(),
        loading,
        searching,
        conversation_mode,
        list_empty_view,
    );
}

fn first_available_icon(names: &'static [&'static str]) -> &'static str {
    let theme = gtk::IconTheme::default();
    names
        .iter()
        .copied()
        .find(|name| theme.has_icon(name))
        .or_else(|| names.first().copied())
        .unwrap_or("")
}

/// Title and optional description for an empty messages pane.
pub fn empty_list_copy(
    loading: bool,
    searching: bool,
    conversation_mode: bool,
    list_empty_view: ListEmptyView,
) -> (&'static str, Option<&'static str>) {
    if loading {
        if conversation_mode {
            (trs!("Loading conversations…"), None)
        } else {
            (trs!("Loading messages…"), None)
        }
    } else if searching {
        if conversation_mode {
            (
                trs!("No matching conversations"),
                Some(trs!("Try folder:inbox from:name, or switch to All mail.")),
            )
        } else {
            (
                trs!("No matching messages"),
                Some(trs!("Try folder:inbox from:name, or switch to All mail.")),
            )
        }
    } else {
        match list_empty_view {
            ListEmptyView::Extension { title, description } => (title, description),
            ListEmptyView::Default if conversation_mode => (trs!("No conversations"), None),
            ListEmptyView::Default => (trs!("No messages"), None),
        }
    }
}

fn empty_list_icon(loading: bool, searching: bool, list_empty_view: ListEmptyView) -> &'static str {
    if loading {
        first_available_icon(&["content-loading-symbolic", "mail-inbox-symbolic"])
    } else if searching {
        first_available_icon(&[
            "edit-find-symbolic",
            "system-search-symbolic",
            "mail-inbox-symbolic",
        ])
    } else {
        match list_empty_view {
            ListEmptyView::Extension { .. } => {
                first_available_icon(&["edit-find-symbolic", "mail-inbox-symbolic"])
            }
            ListEmptyView::Default => {
                first_available_icon(&["mail-inbox-symbolic", "folder-symbolic"])
            }
        }
    }
}

pub(crate) fn sync_empty_state_from_count(
    body_stack: Option<&gtk::Stack>,
    empty_status: Option<&adw::StatusPage>,
    empty: bool,
    loading: bool,
    searching: bool,
    conversation_mode: bool,
    list_empty_view: ListEmptyView,
) {
    if let Some(status) = empty_status {
        let (title, description) =
            empty_list_copy(loading, searching, conversation_mode, list_empty_view);
        status.set_title(title);
        status.set_description(description);
        status.set_icon_name(Some(empty_list_icon(loading, searching, list_empty_view)));
    }
    if let Some(stack) = body_stack {
        stack.set_visible_child_name(if empty { "empty" } else { "list" });
    }
}

pub fn populate_list(list: &mut MessageListView, rows: &[MessageRow]) {
    populate_list_preserved(
        list,
        rows,
        &[],
        None,
        None,
        None,
        false,
        false,
        false,
        ListEmptyView::Default,
        None,
    );
}

pub fn message_at(list: &MessageListView, position: u32) -> Option<MessageId> {
    list.get_visible(position)
        .map(|item| item.borrow().envelope.id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alcedo_core::{AccountId, FolderId, MessageId, ThreadId};
    use chrono::Utc;
    use uuid::Uuid;

    fn sample_envelope(id: i64, seen: bool, flagged: bool) -> Envelope {
        Envelope {
            id: MessageId(id),
            account_id: AccountId(Uuid::from_u128(1)),
            folder_id: FolderId(2),
            uid: 1,
            thread_id: ThreadId(Uuid::from_u128(3)),
            date: Utc::now(),
            subject: "Subject".to_string(),
            from: vec![],
            to: vec![],
            cc: vec![],
            reply_to: Vec::new(),
            sender: Vec::new(),
            routing: None,
            snippet: "Preview text".to_string(),
            seen,
            flagged,
            answered: false,
            draft: false,
            has_attachments: false,
            deleted_at: None,
            account_accent: "#3584e4".to_string(),
            account_avatar: String::new(),
            size_bytes: 0,
        }
    }

    fn sample_rows(envelopes: &[Envelope]) -> Vec<MessageRow> {
        envelopes
            .iter()
            .cloned()
            .map(MessageRow::from_envelope)
            .collect()
    }

    use crate::test_helpers::gtk_test_run;

    fn assert_populate_list_preserved_diff_keeps_order_and_flags(list: &mut MessageListView) {
        let e1 = sample_envelope(1, false, false);
        let e2 = sample_envelope(2, false, false);
        let e3 = sample_envelope(3, false, false);
        populate_list(
            &mut *list,
            &sample_rows(&[e1.clone(), e2.clone(), e3.clone()]),
        );

        let e0_new = sample_envelope(0, false, false);
        let e1_seen = sample_envelope(1, true, false);
        populate_list_preserved(
            list,
            &sample_rows(&[e0_new.clone(), e1_seen.clone(), e3.clone()]),
            &[],
            None,
            None,
            None,
            false,
            false,
            false,
            ListEmptyView::Default,
            None,
        );

        assert_eq!(list.len(), 3);
        assert!(find_message_position(list, e2.id).is_none());
        let order_ids: Vec<i64> = (0..list.len())
            .map(|i| list.get(i).unwrap().borrow().envelope.id.0)
            .collect();
        assert_eq!(order_ids, vec![0, 1, 3]);
        let e1_pos = find_message_position(list, e1.id).unwrap();
        assert!(list.get(e1_pos).unwrap().borrow().envelope.seen);

        let e4 = sample_envelope(4, false, false);
        populate_list_preserved(
            list,
            &sample_rows(&[e0_new, e1_seen, e3, e4]),
            &[],
            None,
            None,
            None,
            false,
            false,
            false,
            ListEmptyView::Default,
            None,
        );
        assert_eq!(list.len(), 4);
        let order_ids: Vec<i64> = (0..list.len())
            .map(|i| list.get(i).unwrap().borrow().envelope.id.0)
            .collect();
        assert_eq!(order_ids, vec![0, 1, 3, 4]);
    }

    #[test]
    fn refresh_keeps_gtk_selection_and_falls_back_to_reader() {
        let a = MessageId(1);
        let b = MessageId(2);
        assert_eq!(ids_to_preserve_on_refresh(&[a, b], Some(a)), vec![a, b]);
        assert_eq!(ids_to_preserve_on_refresh(&[b], Some(a)), vec![b]);
        assert_eq!(ids_to_preserve_on_refresh(&[], Some(a)), vec![a]);
        assert!(ids_to_preserve_on_refresh(&[], None).is_empty());
    }

    #[test]
    fn refresh_does_not_clobber_newer_bitset() {
        let a = MessageId(1);
        let b = MessageId(2);
        assert!(refresh_should_restore_reader(&[], Some(a)));
        assert!(!refresh_should_restore_reader(&[b], Some(a)));
        assert!(!refresh_should_restore_reader(&[a], Some(a)));
        assert!(!refresh_should_restore_reader(&[a, b], Some(a)));
        assert!(refresh_follows_newer_bitset(&[b], Some(a)));
        assert!(refresh_follows_newer_bitset(&[a, b], Some(MessageId(9))));
        assert!(!refresh_follows_newer_bitset(&[], Some(a)));
        assert!(!refresh_follows_newer_bitset(&[a], Some(a)));
    }

    #[test]
    fn ignore_focus_does_not_revert_committed_id() {
        let a = MessageId(1);
        let b = MessageId(2);
        assert!(!focus_ignore_is_commit(&[], Some(a)));
        assert!(!focus_ignore_is_commit(&[a], Some(a)));
        assert!(focus_ignore_is_commit(&[b], Some(a)));
        assert!(focus_ignore_is_commit(&[a, b], Some(a)));
        assert_eq!(open_id_following_bitset(a, &[b]), b);
        assert_eq!(open_id_following_bitset(a, &[a]), a);
        assert_eq!(open_id_following_bitset(a, &[a, b]), a);
        assert_eq!(open_id_following_bitset(a, &[]), a);
        assert!(idle_sync_applies(1, 1, true));
        assert!(!idle_sync_applies(1, 2, true));
        assert!(!idle_sync_applies(1, 1, false));
    }

    #[test]
    fn restore_ids_drop_missing_and_empty_list() {
        let envelopes = vec![
            sample_envelope(1, false, false),
            sample_envelope(2, true, false),
        ];
        let a = MessageId(1);
        let gone = MessageId(99);
        assert!(ids_to_restore_on_refresh(&[], &[a], Some(a), false).is_empty());
        assert_eq!(
            ids_to_restore_on_refresh(&sample_rows(&envelopes), &[a, gone], Some(a), false),
            vec![a]
        );
        assert!(
            ids_to_restore_on_refresh(&sample_rows(&envelopes), &[], Some(gone), false).is_empty()
        );
        assert_eq!(
            ids_to_restore_on_refresh(&sample_rows(&envelopes), &[], Some(a), false),
            vec![a]
        );
    }

    #[test]
    fn expand_column_is_subject_when_visible() {
        let default_order = ["unread", "from", "subject", "attachments", "star", "date"];
        assert_eq!(
            preferred_expand_column(&default_order).as_deref(),
            Some("subject")
        );

        let subject_hidden = ["unread", "from", "attachments", "star", "date"];
        assert_eq!(
            preferred_expand_column(&subject_hidden).as_deref(),
            Some("from")
        );

        let only_date = ["unread", "attachments", "star", "date"];
        assert_eq!(preferred_expand_column(&only_date).as_deref(), Some("date"));
    }

    #[test]
    fn message_list_multi_selection() {
        gtk_test_run(message_list_multi_selection_body);
    }

    fn message_list_multi_selection_body() {
        let long = "Re: a very long subject line that would overflow a narrow messages pane";
        let natural = gtk::Label::new(Some(long));
        let (natural_min, _, _, _) = natural.measure(gtk::Orientation::Horizontal, -1);
        let shrunk = gtk::Label::new(Some(long));
        shrink_ellipsize_label(&shrunk);
        let (shrunk_min, _, _, _) = shrunk.measure(gtk::Orientation::Horizontal, -1);
        assert!(
            natural_min > 200,
            "control label should still want its full text ({natural_min})"
        );
        assert!(
            shrunk_min < 64,
            "ellipsized column labels must measure at ~1ch so ColumnView rows fit the viewport ({shrunk_min})"
        );

        let mut list = new_list_view();
        let order = vec![
            "date".to_string(),
            "subject".to_string(),
            "from".to_string(),
            "unread".to_string(),
            "attachments".to_string(),
            "star".to_string(),
            "size".to_string(),
            "folder".to_string(),
            "account".to_string(),
            "account-chip".to_string(),
        ];
        apply_column_order(&list, &order);
        assert_eq!(persistable_column_ids(&list.view), order);
        assert_eq!(
            visual_column_ids(&list.view).first().map(String::as_str),
            Some("drag")
        );
        assert_eq!(
            visual_column_ids(&list.view).get(1).map(String::as_str),
            Some("date")
        );
        apply_last_column_layout(&list.view);
        assert_eq!(
            column_by_id(&list.view, "star").unwrap().fixed_width(),
            STAR_WIDTH
        );
        assert!(
            !column_by_id(&list.view, "attachments")
                .unwrap()
                .is_resizable()
        );
        assert!(!column_by_id(&list.view, "star").unwrap().is_resizable());
        assert!(list.view.has_css_class("alcedo-last-col-account-chip"));

        apply_column_order(
            &list,
            &[
                "unread".into(),
                "from".into(),
                "folder".into(),
                "account".into(),
                "account-chip".into(),
                "subject".into(),
                "size".into(),
                "date".into(),
                "attachments".into(),
                "star".into(),
            ],
        );
        apply_last_column_layout(&list.view);
        assert_eq!(
            column_by_id(&list.view, "star").unwrap().fixed_width(),
            STAR_WIDTH + LAST_COLUMN_PAD
        );
        assert_eq!(
            column_by_id(&list.view, "attachments")
                .unwrap()
                .fixed_width(),
            ATTACHMENTS_WIDTH
        );
        assert!(
            !column_by_id(&list.view, "attachments")
                .unwrap()
                .is_resizable()
        );
        assert!(!column_by_id(&list.view, "star").unwrap().is_resizable());
        assert!(list.view.has_css_class("alcedo-last-col-star"));
        assert!(!list.view.has_css_class("alcedo-last-col-date"));
        apply_column_order(&list, &order);
        apply_last_column_layout(&list.view);
        assert!(list.get_columns().get("date-handle").is_none());
        assert!(list.get_columns().get("flex-spacer").is_none());
        assert!(list.get_columns().get("drag").is_some());
        assert!(!list.get_columns().get("drag").unwrap().is_resizable());
        assert!(!list.get_columns().get("subject").unwrap().is_resizable());
        assert!(list.get_columns().get("subject").unwrap().expands());
        assert!(!list.get_columns().get("unread").unwrap().is_resizable());
        assert!(
            !list
                .get_columns()
                .get("attachments")
                .unwrap()
                .is_resizable()
        );
        assert!(!list.get_columns().get("star").unwrap().is_resizable());
        assert_eq!(
            list.get_columns().get("unread").unwrap().title().as_deref(),
            Some("Unread")
        );
        assert_eq!(
            list.get_columns()
                .get("attachments")
                .unwrap()
                .title()
                .as_deref(),
            Some("Attachments")
        );
        assert_eq!(
            list.get_columns().get("star").unwrap().title().as_deref(),
            Some("Starred")
        );
        assert_eq!(
            list.get_columns().get("size").unwrap().title().as_deref(),
            Some("Size")
        );
        assert!(list.get_columns().get("size").unwrap().is_resizable());
        assert_eq!(
            list.get_columns()
                .get("deleted")
                .unwrap()
                .title()
                .as_deref(),
            Some("Deleted")
        );
        assert!(!column_by_id(&list.view, "deleted").unwrap().is_visible());
        set_deleted_column_visible(&list, true);
        let ids = visual_column_ids(&list.view);
        let date_pos = ids.iter().position(|id| id == "date").expect("date column");
        assert_eq!(ids.get(date_pos + 1).map(String::as_str), Some("deleted"));
        assert!(
            !persistable_column_ids(&list.view)
                .iter()
                .any(|id| id == "deleted")
        );
        set_deleted_column_visible(&list, false);
        assert!(!column_by_id(&list.view, "deleted").unwrap().is_visible());
        assert!(list.get_columns().get("from").unwrap().is_resizable());
        assert!(list.get_columns().get("date").unwrap().is_resizable());
        assert!(!list.get_columns().get("date").unwrap().expands());
        assert!(selected_ids(&list).is_empty());
        assert_eq!(selection_count(&list), 0);

        let envelopes = vec![
            sample_envelope(10, false, false),
            sample_envelope(20, true, false),
        ];
        populate_list(&mut list, &sample_rows(&envelopes));
        assert_eq!(find_visible_position(&list, envelopes[1].id), Some(1));
        assert!(find_visible_position(&list, MessageId(99)).is_none());

        let mut updated = envelopes.clone();
        updated[0].seen = true;
        populate_list_preserved(
            &mut list,
            &sample_rows(&updated),
            &[updated[0].id],
            None,
            None,
            None,
            false,
            false,
            false,
            ListEmptyView::Default,
            None,
        );

        assert_eq!(list.len(), 2);
        assert!(list.get(0).unwrap().borrow().envelope.seen);
        assert!(list.selection_model.is_selected(0));

        list.selection_model.select_item(1, true);
        assert_eq!(selection_count(&list), 1);
        assert!(list.selection_model.is_selected(1));

        let selected = selected_ids(&list);
        update_envelope_in_list(&mut list, 0, &updated[0], None, None);
        assert!(list.selection_model.is_selected(1));
        assert_eq!(selected_ids(&list), selected);

        let three = vec![
            sample_envelope(10, false, false),
            sample_envelope(20, true, false),
            sample_envelope(30, true, true),
        ];
        populate_list(&mut list, &sample_rows(&three));
        list.selection_model.select_item(0, true);
        list.selection_model.select_item(2, false);
        let ids = selected_ids(&list);
        assert_eq!(ids.len(), 2);

        let mut refreshed = three.clone();
        refreshed[0].seen = true;
        populate_list_preserved(
            &mut list,
            &sample_rows(&refreshed),
            &ids,
            None,
            None,
            None,
            false,
            false,
            false,
            ListEmptyView::Default,
            None,
        );
        assert_eq!(selected_ids(&list), ids);

        list.selection_model.select_item(2, true);
        assert_eq!(selected_ids(&list), vec![refreshed[2].id]);
        list.selection_model.select_item(0, true);
        restore_reader_row(&list, refreshed[2].id);
        assert_eq!(selected_ids(&list), vec![refreshed[2].id]);

        populate_list(&mut list, &sample_rows(&three));
        list.selection_model.select_item(1, true);
        let gtk_ids = selected_ids(&list);
        let stale = Some(three[0].id);
        populate_list_preserved(
            &mut list,
            &sample_rows(&three),
            &gtk_ids,
            None,
            None,
            None,
            false,
            false,
            false,
            ListEmptyView::Default,
            None,
        );
        let after = selected_ids(&list);
        if refresh_should_restore_reader(&after, stale) {
            restore_reader_row(&list, three[0].id);
        }
        assert_eq!(selected_ids(&list), vec![three[1].id]);
        sync_list_selection_to_reader(&list, stale);
        assert_eq!(selected_ids(&list), vec![three[1].id]);

        list.selection_model.select_item(0, true);
        list.selection_model.select_item(1, true);
        let previous = Some(three[0].id);
        let ids = selected_ids(&list);
        if !focus_ignore_is_commit(&ids, previous) {
            restore_reader_row(&list, three[0].id);
        }
        assert_eq!(selected_ids(&list), vec![three[1].id]);

        populate_list(&mut list, &sample_rows(&three));
        list.selection_model.select_item(1, true);
        let selected = selected_ids(&list);
        let len = list.len();
        let mut unseen = three[1].clone();
        unseen.seen = false;
        update_envelope_in_list(&mut list, 1, &unseen, None, None);
        assert_eq!(list.len(), len);
        assert_eq!(selected_ids(&list), selected);
        assert!(!list.get(1).unwrap().borrow().envelope.seen);
        assert_eq!(list.get(1).unwrap().borrow().unread_count, 1);

        populate_list(&mut list, &sample_rows(&three));
        let mut seen = three[1].clone();
        seen.seen = true;
        update_envelope_in_list(&mut list, 1, &seen, None, None);
        assert!(list.get(1).unwrap().borrow().envelope.seen);
        assert_eq!(list.get(1).unwrap().borrow().unread_count, 0);

        populate_list(&mut list, &sample_rows(&three));
        list.selection_model.select_item(1, true);
        let selected = selected_ids(&list);
        let mut seen_flip = three.clone();
        seen_flip[0].seen = true;
        populate_list_preserved(
            &mut list,
            &sample_rows(&seen_flip),
            &selected,
            None,
            None,
            None,
            false,
            false,
            false,
            ListEmptyView::Default,
            None,
        );
        assert_eq!(list.len(), 3);
        assert_eq!(selected_ids(&list), selected);
        assert!(list.get(0).unwrap().borrow().envelope.seen);

        populate_list(&mut list, &sample_rows(&three));
        list.selection_model.select_item(0, true);
        list.selection_model.select_item(1, false);
        let remaining = vec![three[0].id, three[1].id];
        populate_list_preserved(
            &mut list,
            &sample_rows(&three[..2]),
            &remaining,
            None,
            None,
            None,
            false,
            false,
            false,
            ListEmptyView::Default,
            None,
        );
        assert_eq!(list.len(), 2);
        assert_eq!(selected_ids(&list), remaining);
        assert!(find_message_position(&list, three[2].id).is_none());

        populate_list(&mut list, &sample_rows(&three));
        list.selection_model.select_item(0, true);
        list.selection_model.select_item(1, false);
        remove_ids_from_list(&mut list, &[three[0].id]);
        assert_eq!(list.len(), 2);
        assert!(!selected_ids(&list).contains(&three[0].id));
        assert!(find_message_position(&list, three[0].id).is_none());

        populate_list(
            &mut list,
            &sample_rows(&[
                sample_envelope(10, false, false),
                sample_envelope(20, true, false),
            ]),
        );
        list.selection_model.select_item(0, true);
        let next = vec![
            sample_envelope(40, false, false),
            sample_envelope(50, true, false),
            sample_envelope(60, true, true),
        ];
        populate_list_preserved(
            &mut list,
            &sample_rows(&next),
            &[next[0].id],
            None,
            None,
            None,
            false,
            false,
            false,
            ListEmptyView::Default,
            None,
        );
        assert_eq!(list.len(), 3);
        assert_eq!(list.get(0).unwrap().borrow().envelope.id, next[0].id);
        assert_eq!(selected_ids(&list), vec![next[0].id]);
        assert!(find_message_position(&list, MessageId(10)).is_none());

        populate_list(
            &mut list,
            &sample_rows(&[sample_envelope(10, false, false)]),
        );
        let stack = gtk::Stack::new();
        let list_page = gtk::Label::new(Some("list"));
        let empty_page = gtk::Label::new(Some("empty"));
        stack.add_named(&list_page, Some("list"));
        stack.add_named(&empty_page, Some("empty"));
        stack.set_visible_child_name("list");
        populate_list_preserved(
            &mut list,
            &[],
            &[],
            None,
            Some(&stack),
            None,
            false,
            false,
            false,
            ListEmptyView::Default,
            None,
        );
        assert_eq!(list.len(), 0);
        assert_eq!(stack.visible_child_name().as_deref(), Some("empty"));
        assert!(
            ids_to_restore_on_refresh(&[], &[MessageId(10)], Some(MessageId(10)), false).is_empty()
        );

        let subject_column = column_by_id(&list.view, "subject").expect("subject column");
        list.view
            .sort_by_column(Some(&subject_column), gtk::SortType::Ascending);
        let mut alpha = sample_envelope(1, false, false);
        alpha.subject = "Alpha".to_string();
        let mut beta = sample_envelope(2, false, false);
        beta.subject = "Beta".to_string();
        populate_list(&mut list, &sample_rows(&[alpha.clone(), beta.clone()]));
        assert_eq!(message_at(&list, 0), Some(alpha.id));
        alpha.subject = "Zulu".to_string();
        let pos = find_message_position(&list, alpha.id).expect("alpha position");
        update_envelope_in_list(&mut list, pos, &alpha, None, None);
        assert_eq!(message_at(&list, 0), Some(beta.id));
        assert_eq!(message_at(&list, 1), Some(alpha.id));
        list.selection_model.select_item(0, true);
        restore_reader_selection_if_cleared(&list.selection_model, &list.view, Some(alpha.id));
        assert_eq!(selected_ids(&list), vec![beta.id]);
        sync_list_selection_to_reader(&list, Some(alpha.id));
        assert_eq!(selected_ids(&list), vec![beta.id]);
        restore_reader_row_for_model(&list.selection_model, &list.view, MessageId(999));
        assert_eq!(selected_ids(&list), vec![beta.id]);

        let label = gtk::Label::new(None);
        apply_from_visuals(
            &label,
            &MessageRow::from_envelope(sample_envelope(1, false, false)),
        );
        assert!(label.has_css_class("unread-sender"));
        apply_from_visuals(
            &label,
            &MessageRow::from_envelope(sample_envelope(1, true, false)),
        );
        assert!(!label.has_css_class("unread-sender"));

        let pip = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        let count = gtk::Label::new(None);
        apply_unread_visuals(
            &pip,
            &count,
            &MessageRow::from_envelope(sample_envelope(1, false, false)),
        );
        assert!(pip.is_visible());
        apply_unread_visuals(
            &pip,
            &count,
            &MessageRow::from_envelope(sample_envelope(1, true, false)),
        );
        assert!(!pip.is_visible());

        let scrolled = gtk::ScrolledWindow::new();
        scrolled.set_child(Some(&list.view));
        let chip = NewMailChip::new();
        chip.bind_list(&list, &scrolled);
        let date_col = list.get_columns().get("date").unwrap();
        list.view
            .sort_by_column(Some(date_col), gtk::SortType::Descending);

        let base = sample_envelope_dated(1, Utc::now() - chrono::Duration::days(1));
        let selected = sample_envelope_dated(2, Utc::now() - chrono::Duration::hours(2));
        populate_list(&mut list, &sample_rows(&[base.clone(), selected.clone()]));
        list.selection_model.select_item(1, true);
        let preserve_ids = vec![selected.id];
        let arrival = sample_envelope_dated(3, Utc::now());
        populate_list_preserved(
            &mut list,
            &sample_rows(&[arrival.clone(), base, selected]),
            &preserve_ids,
            Some(&scrolled),
            None,
            None,
            false,
            false,
            false,
            ListEmptyView::Default,
            Some(&chip),
        );
        assert_eq!(selected_ids(&list), preserve_ids);
        assert_eq!(list.get(0).unwrap().borrow().envelope.id, arrival.id);
        assert_eq!(find_visible_position(&list, arrival.id), Some(0));
        assert!(!chip.revealer.reveals_child());

        scrolled.vadjustment().set_upper(2000.0);
        scrolled.vadjustment().set_page_size(200.0);
        scrolled.vadjustment().set_value(800.0);
        let rows: Vec<_> = (0..20)
            .map(|i| sample_envelope_dated(i + 10, Utc::now() - chrono::Duration::hours(i)))
            .collect();
        populate_list(&mut list, &sample_rows(&rows));
        list.selection_model.select_item(10, true);
        let preserve_ids = vec![rows[10].id];
        scrolled.vadjustment().set_value(800.0);
        let arrival = sample_envelope_dated(99, Utc::now());
        let mut all = vec![arrival.clone()];
        all.extend(rows);
        populate_list_preserved(
            &mut list,
            &sample_rows(&all),
            &preserve_ids,
            Some(&scrolled),
            None,
            None,
            false,
            false,
            false,
            ListEmptyView::Default,
            Some(&chip),
        );
        assert_eq!(selected_ids(&list), preserve_ids);
        assert!(chip.revealer.reveals_child());
        assert_eq!(chip.visible_count(), 1);
        assert_eq!(new_mail_chip_label(chip.visible_count()), "New message");
        chip.hide();
        assert!(!chip.revealer.reveals_child());

        let env = sample_envelope(1, false, false);
        let added = HashSet::from([env.id]);
        chip.show(&added, &[env], false);
        assert!(chip.revealer.reveals_child());
        assert_eq!(chip.button.label().as_deref(), Some("New message"));
        chip.hide();
        assert_eq!(chip.visible_count(), 0);

        let now = Utc::now();
        let older = sample_envelope_dated(101, now - chrono::Duration::hours(2));
        let newer = sample_envelope_dated(102, now - chrono::Duration::hours(1));
        let two_added = HashSet::from([older.id, newer.id]);
        chip.show(&two_added, &[older.clone(), newer.clone()], false);
        chip.remove_ids(&[newer.id], std::slice::from_ref(&older));
        assert_eq!(chip.newest_id.get(), Some(older.id));
        assert_eq!(chip.visible_count(), 1);
        chip.hide();

        assert_eq!(list_drag_payload(None, &[MessageId(1)], &list), None);
        assert_eq!(
            list_drag_payload(Some(MessageId(20)), &[MessageId(10), MessageId(20)], &list,),
            Some(vec![MessageId(10), MessageId(20)])
        );
        assert_eq!(
            handle_drag_payload(Some(MessageId(20)), &[], &[MessageId(20)], &list),
            Some(vec![MessageId(20)])
        );
    }

    #[test]
    fn populate_list_preserved_diff_keeps_order_and_flags() {
        gtk_test_run(|| {
            let mut list = new_list_view();
            let order = vec![
                "date".to_string(),
                "subject".to_string(),
                "from".to_string(),
                "unread".to_string(),
                "attachments".to_string(),
                "star".to_string(),
                "folder".to_string(),
                "account".to_string(),
                "account-chip".to_string(),
            ];
            apply_column_order(&list, &order);
            apply_last_column_layout(&list.view);
            apply_column_order(
                &list,
                &[
                    "unread".into(),
                    "from".into(),
                    "folder".into(),
                    "account".into(),
                    "account-chip".into(),
                    "subject".into(),
                    "date".into(),
                    "attachments".into(),
                    "star".into(),
                ],
            );
            apply_last_column_layout(&list.view);
            apply_column_order(&list, &order);
            apply_last_column_layout(&list.view);
            assert_populate_list_preserved_diff_keeps_order_and_flags(&mut list);
        });
    }

    #[test]
    fn cmp_from_is_case_insensitive() {
        let mut a = sample_envelope(1, false, false);
        let mut b = sample_envelope(2, false, false);
        a.from = vec![alcedo_core::Address {
            name: Some("Alice".to_string()),
            email: "a@example.com".to_string(),
        }];
        b.from = vec![alcedo_core::Address {
            name: Some("alice".to_string()),
            email: "b@example.com".to_string(),
        }];
        assert_eq!(
            cmp_from(&MessageRow::from_envelope(a), &MessageRow::from_envelope(b)),
            Ordering::Equal
        );
    }

    #[test]
    fn cmp_subject_is_case_insensitive() {
        let mut a = sample_envelope(1, false, false);
        let mut b = sample_envelope(2, false, false);
        a.subject = "Hello".to_string();
        b.subject = "hello".to_string();
        assert_eq!(
            cmp_subject(&MessageRow::from_envelope(a), &MessageRow::from_envelope(b)),
            Ordering::Equal,
        );
    }

    #[test]
    fn cmp_date_newer_is_greater() {
        let mut older = sample_envelope(1, false, false);
        let mut newer = sample_envelope(2, false, false);
        older.date = Utc::now() - chrono::Duration::days(1);
        newer.date = Utc::now();
        assert_eq!(
            cmp_date(
                &MessageRow::from_envelope(older.clone()),
                &MessageRow::from_envelope(newer.clone()),
            ),
            Ordering::Less,
        );
        assert_eq!(
            cmp_date(
                &MessageRow::from_envelope(newer),
                &MessageRow::from_envelope(older)
            ),
            Ordering::Greater,
        );
    }

    #[test]
    fn cmp_date_keeps_parent_above_children_when_inverted() {
        let conversation = sample_conversation(&[1, 2, 3], 10);
        let rows = flatten_conversation_rows(
            std::slice::from_ref(&conversation),
            &HashSet::from([conversation.thread_id]),
        );
        let parent = &rows[0];
        let child1 = &rows[1];
        let child2 = &rows[2];

        assert_eq!(cmp_date(parent, child1).reverse(), Ordering::Less);
        assert_eq!(cmp_date(parent, child2).reverse(), Ordering::Less);
        assert_eq!(cmp_date(child1, child2).reverse(), Ordering::Less);
    }

    #[test]
    fn format_message_size_unknown_is_em_dash() {
        assert_eq!(format_message_size(0), "—");
        assert_eq!(format_message_size(-1), "—");
    }

    #[test]
    fn format_message_size_known_uses_glib() {
        assert_eq!(
            format_message_size(2048),
            glib::format_size(2048).to_string()
        );
    }

    #[test]
    fn cmp_size_larger_is_greater() {
        let mut smaller = sample_envelope(1, false, false);
        let mut larger = sample_envelope(2, false, false);
        smaller.size_bytes = 100;
        larger.size_bytes = 500;
        let small_row = MessageRow::from_envelope(smaller);
        let large_row = MessageRow::from_envelope(larger);
        assert_eq!(cmp_size(&small_row, &large_row), Ordering::Less);
        assert_eq!(cmp_size(&large_row, &small_row), Ordering::Greater);
    }

    #[test]
    fn cmp_size_keeps_parent_above_children_when_inverted() {
        let conversation = sample_conversation(&[1, 2, 3], 10);
        let rows = flatten_conversation_rows(
            std::slice::from_ref(&conversation),
            &HashSet::from([conversation.thread_id]),
        );
        let parent = &rows[0];
        let child1 = &rows[1];
        let child2 = &rows[2];

        assert_eq!(cmp_size(parent, child1).reverse(), Ordering::Less);
        assert_eq!(cmp_size(parent, child2).reverse(), Ordering::Less);
        assert_eq!(cmp_size(child1, child2).reverse(), Ordering::Less);
    }

    #[test]
    fn cmp_from_and_subject_share_thread_sort_keys() {
        let conversation = sample_conversation(&[1, 2, 3], 10);
        let rows = flatten_conversation_rows(
            std::slice::from_ref(&conversation),
            &HashSet::from([conversation.thread_id]),
        );
        let parent = &rows[0];
        let child = &rows[1];

        assert_eq!(from_thread_sort_key(parent), from_thread_sort_key(child),);
        assert_eq!(parent.sort_subject, child.sort_subject);
        assert_eq!(cmp_from(parent, child), Ordering::Less);
        assert_eq!(cmp_subject(parent, child), Ordering::Less);
    }

    fn sample_conversation(ids: &[i64], thread: u128) -> Conversation {
        let now = Utc::now();
        let envs: Vec<Envelope> = ids
            .iter()
            .enumerate()
            .map(|(index, id)| {
                let mut env = sample_envelope(*id, false, false);
                env.thread_id = ThreadId(uuid::Uuid::from_u128(thread));
                env.date = now - chrono::Duration::hours((ids.len() - index) as i64);
                env
            })
            .collect();
        alcedo_core::group_envelopes(&envs).remove(0)
    }

    #[test]
    fn flatten_collapsed_list_length_matches_conversation_count() {
        let conversations = vec![
            sample_conversation(&[1, 2], 10),
            sample_conversation(&[3], 11),
        ];
        let rows = flatten_conversation_rows(&conversations, &HashSet::new());
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|row| row.depth == 0));
    }

    #[test]
    fn flatten_inserts_children_only_when_expanded() {
        let conversations = vec![sample_conversation(&[1, 2, 3], 10)];
        let collapsed = flatten_conversation_rows(&conversations, &HashSet::new());
        assert_eq!(collapsed.len(), 1);
        assert_eq!(collapsed[0].envelope.id, MessageId(1));

        let expanded =
            flatten_conversation_rows(&conversations, &HashSet::from([conversations[0].thread_id]));
        assert_eq!(expanded.len(), 3);
        assert_eq!(expanded[0].depth, 0);
        assert!(expanded[1..].iter().all(|row| row.depth == 1));
        assert_eq!(expanded[1].envelope.id, MessageId(2));
        assert_eq!(expanded[2].envelope.id, MessageId(3));
    }

    #[test]
    fn flatten_root_uses_oldest_member_envelope() {
        let conversations = vec![sample_conversation(&[1, 2], 10)];
        let rows = flatten_conversation_rows(&conversations, &HashSet::new());
        assert_eq!(rows[0].envelope.id, MessageId(1));
        assert_eq!(rows[0].sort_date, conversations[0].latest.date);
    }

    #[test]
    fn cmp_deleted_stamped_sorts_before_unstamped_and_newer_first() {
        let unstamped = MessageRow::from_envelope(sample_envelope(1, false, false));
        let older = MessageRow::from_envelope({
            let mut env = sample_envelope(2, false, false);
            env.deleted_at = Some(Utc::now() - chrono::Duration::hours(2));
            env
        });
        let newer = MessageRow::from_envelope({
            let mut env = sample_envelope(3, false, false);
            env.deleted_at = Some(Utc::now());
            env
        });
        assert_eq!(cmp_deleted(&newer, &unstamped), Ordering::Greater);
        assert_eq!(cmp_deleted(&unstamped, &newer), Ordering::Less);
        assert_eq!(cmp_deleted(&newer, &older), Ordering::Greater);
    }

    #[test]
    fn header_title_maps_deleted_column() {
        let columns = [("from", "De"), ("date", "Date"), ("deleted", "Supprimé")];
        assert_eq!(column_id_for_title(columns, "Supprimé"), Some("deleted"));
        assert_eq!(column_id_for_title(columns, "De"), Some("from"));
        assert_eq!(column_id_for_title(columns, "From"), None);
        assert_eq!(column_id_for_title(columns, ""), None);
    }

    #[test]
    fn empty_search_copy_explains_no_matches() {
        assert_eq!(
            empty_list_copy(false, true, false, ListEmptyView::Default),
            (
                "No matching messages",
                Some("Try folder:inbox from:name, or switch to All mail.")
            )
        );
        assert_eq!(
            empty_list_copy(true, true, false, ListEmptyView::Default),
            ("Loading messages…", None)
        );
        assert_eq!(
            empty_list_copy(false, false, false, ListEmptyView::Default),
            ("No messages", None)
        );
        assert_eq!(
            empty_list_copy(
                false,
                false,
                false,
                ListEmptyView::Extension {
                    title: "Nothing here",
                    description: Some("Try again later."),
                }
            ),
            ("Nothing here", Some("Try again later."))
        );
    }

    #[test]
    fn today_shows_local_clock_only() {
        let now = Utc::now();
        let label = format_relative_time(now);
        assert!(!label.contains('·'), "{label}");
        // Follows the desktop clock style (12- or 24-hour), so compare with it.
        assert_eq!(
            label,
            crate::calendar_clock::format_time_of_day(&now.with_timezone(&chrono::Local))
        );
    }

    #[test]
    fn older_mail_includes_date_and_clock() {
        let date = Utc::now() - chrono::Duration::days(40);
        let label = format_relative_time(date);
        assert!(label.contains('·'), "{label}");
        assert!(label.contains(':'), "{label}");
    }

    #[test]
    fn message_id_payload_empty() {
        assert_eq!(format_message_id_payload(&[]), "");
        assert!(parse_message_id_payload("").is_empty());
    }

    #[test]
    fn message_id_payload_one_id() {
        let ids = vec![MessageId(42)];
        assert_eq!(format_message_id_payload(&ids), "42");
        assert_eq!(parse_message_id_payload("42"), ids);
    }

    #[test]
    fn message_id_payload_skips_junk() {
        assert_eq!(
            parse_message_id_payload("1, bad ,3"),
            vec![MessageId(1), MessageId(3)]
        );
    }

    #[test]
    fn list_drag_hit_skips_star_cell() {
        assert_eq!(list_drag_hit(Some(MessageId(1)), true), None);
        assert_eq!(list_drag_hit(Some(MessageId(1)), false), Some(MessageId(1)));
        assert_eq!(list_drag_hit(None, false), None);
    }

    #[test]
    fn list_drag_prepare_keeps_press_row() {
        assert_eq!(
            list_drag_prepare_hit(Some(MessageId(3)), None),
            Some(MessageId(3))
        );
        assert_eq!(
            list_drag_prepare_hit(None, Some(MessageId(4))),
            Some(MessageId(4))
        );
        assert_eq!(list_drag_prepare_hit(None, None), None);
    }

    fn sample_envelope_dated(id: i64, date: chrono::DateTime<Utc>) -> Envelope {
        let mut env = sample_envelope(id, false, false);
        env.date = date;
        env
    }

    #[test]
    fn should_reveal_new_mail_only_on_incremental_overlap() {
        let a = MessageId(1);
        let b = MessageId(2);
        let c = MessageId(3);
        assert!(!should_reveal_new_mail(
            &HashSet::new(),
            &HashSet::from([a])
        ));
        assert!(!should_reveal_new_mail(
            &HashSet::from([a]),
            &HashSet::from([b])
        ));
        assert!(!should_reveal_new_mail(
            &HashSet::from([a, b]),
            &HashSet::from([a, b])
        ));
        assert!(should_reveal_new_mail(
            &HashSet::from([a]),
            &HashSet::from([a, b])
        ));
        assert!(should_reveal_new_mail(
            &HashSet::from([a, b]),
            &HashSet::from([a, b, c])
        ));
    }

    #[test]
    fn newest_added_id_picks_latest_date_then_id() {
        let now = Utc::now();
        let older = sample_envelope_dated(1, now - chrono::Duration::hours(2));
        let newer = sample_envelope_dated(2, now - chrono::Duration::hours(1));
        let tie_low = sample_envelope_dated(3, now);
        let tie_high = sample_envelope_dated(4, now);
        let added = HashSet::from([older.id, newer.id, tie_low.id, tie_high.id]);
        assert_eq!(
            newest_added_id(&[older, newer, tie_low, tie_high], &added),
            Some(MessageId(4))
        );
    }

    #[test]
    fn new_mail_chip_label_copy() {
        assert_eq!(new_mail_chip_label(1), "New message");
        assert_eq!(new_mail_chip_label(3), "3 new messages");
    }

    #[test]
    fn at_arrival_edge_start_and_end() {
        assert!(at_arrival_edge_values(0.0, 2000.0, 200.0, 0, 10));
        assert!(!at_arrival_edge_values(400.0, 2000.0, 200.0, 0, 10));
        assert!(!at_arrival_edge_values(800.0, 2000.0, 200.0, 0, 20));
        assert!(at_arrival_edge_values(1800.0, 2000.0, 200.0, 9, 10));
    }
}
