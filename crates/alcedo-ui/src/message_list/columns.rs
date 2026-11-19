//! Message-list column layout: order, visibility, widths, expand column, saved sort, and header chrome.

use super::*;
use alcedo_i18n::trs;

pub(super) fn apply_column_size_constraints(list: &MessageListView) {
    let columns = list.get_columns();
    if let Some(col) = columns.get("drag") {
        col.set_fixed_width(DRAG_WIDTH);
        col.set_resizable(false);
        col.set_expand(false);
    }
    if let Some(col) = columns.get("unread") {
        col.set_fixed_width(UNREAD_WIDTH);
    }
    if let Some(col) = columns.get("from") {
        col.set_fixed_width(FROM_DEFAULT);
    }
    if let Some(col) = columns.get("folder") {
        col.set_fixed_width(FOLDER_DEFAULT);
    }
    if let Some(col) = columns.get("account") {
        col.set_fixed_width(ACCOUNT_DEFAULT);
    }
    if let Some(col) = columns.get("account-chip") {
        col.set_fixed_width(ACCOUNT_CHIP_WIDTH);
        col.set_resizable(false);
    }
    if let Some(col) = columns.get("attachments") {
        col.set_fixed_width(ATTACHMENTS_WIDTH);
    }
    if let Some(col) = columns.get("star") {
        col.set_fixed_width(STAR_WIDTH);
    }
    if let Some(col) = columns.get("size") {
        col.set_fixed_width(SIZE_DEFAULT);
    }
    if let Some(col) = columns.get("date") {
        col.set_fixed_width(DATE_DEFAULT);
    }
    if let Some(col) = columns.get("deleted") {
        col.set_fixed_width(DATE_DEFAULT);
        col.set_resizable(true);
        col.set_expand(false);
    }
}

pub(super) fn apply_column_order_for_view(view: &gtk::ColumnView, order: &[String]) {
    for (i, id) in order.iter().enumerate() {
        if id == "drag" {
            continue;
        }
        if let Some(col) = column_by_id(view, id) {
            view.remove_column(&col);
            view.insert_column(i as u32, &col);
        }
    }
    pin_drag_column(view);
    pin_deleted_column(view);
}

pub(super) fn pin_deleted_column(view: &gtk::ColumnView) {
    let Some(deleted) = column_by_id(view, "deleted") else {
        return;
    };
    let ids = visual_column_ids(view);
    let date_pos = ids.iter().position(|id| id == "date");
    if date_pos.is_none() {
        return;
    }
    let target = date_pos.unwrap() + 1;
    if ids.get(target).map(String::as_str) == Some("deleted") {
        return;
    }
    view.remove_column(&deleted);
    view.insert_column(target as u32, &deleted);
}

pub(super) fn pin_drag_column(view: &gtk::ColumnView) {
    let Some(col) = column_by_id(view, "drag") else {
        return;
    };
    col.set_fixed_width(DRAG_WIDTH);
    col.set_resizable(false);
    col.set_expand(false);
    if visual_column_ids(view).first().map(String::as_str) == Some("drag") {
        return;
    }
    view.remove_column(&col);
    view.insert_column(0, &col);
}

pub(super) fn persistable_column_ids(view: &gtk::ColumnView) -> Vec<String> {
    visual_column_ids(view)
        .into_iter()
        .filter(|id| id != "drag" && id != "deleted")
        .collect()
}

#[cfg(test)]
pub(super) fn apply_column_order(list: &MessageListView, order: &[String]) {
    apply_column_order_for_view(&list.view, order);
}

pub(super) fn apply_saved_column_order_for_view(view: &gtk::ColumnView) {
    SUPPRESS_COLUMN_ORDER_PERSIST.set(true);
    apply_column_order_for_view(
        view,
        &message_list_column_order_for(&current_column_scope()),
    );
    SUPPRESS_COLUMN_ORDER_PERSIST.set(false);
    apply_column_expand_for_view(view);
}

#[cfg(not(test))] // only called from the real (non-test) list setup
pub(super) fn apply_saved_column_order(list: &MessageListView) {
    apply_saved_column_order_for_view(&list.view);
}

#[cfg(not(test))] // only called from the real (non-test) list setup
pub(super) fn apply_saved_column_visibility(list: &MessageListView) {
    reapply_saved_user_column_visibility(list, false);
}

pub(super) fn reapply_saved_user_column_visibility(list: &MessageListView, searching: bool) {
    use crate::settings::message_list_context_column_effective_visible;
    let saved = message_list_column_visible_for(&current_column_scope());
    let columns = list.get_columns();
    for id in MESSAGE_LIST_USER_COLUMNS {
        let show = if MESSAGE_LIST_CONTEXT_COLUMNS.contains(id) {
            message_list_context_column_effective_visible(id, searching, &saved)
        } else {
            saved.contains(*id)
        };
        if let Some(col) = columns.get(id) {
            col.set_visible(show);
        }
    }
    apply_column_expand(list);
    sync_column_picker_action_states(&list.view);
}

pub(super) fn persist_picked_column_visibility(id: &str, visible: bool) {
    if !MESSAGE_LIST_USER_COLUMNS.contains(&id) {
        return;
    }
    let scope = current_column_scope();
    let mut saved = message_list_column_visible_for(&scope);
    if visible {
        saved.insert(id.to_string());
    } else {
        saved.remove(id);
    }
    set_message_list_column_visible_for(&scope, &saved);
}

pub(super) fn sync_column_picker_action_states(view: &gtk::ColumnView) {
    COLUMN_PICKER_ACTIONS.with(|slot| {
        let holder = slot.borrow();
        let Some(actions) = holder.as_ref() else {
            return;
        };
        let actions = actions.borrow();
        for (id, _) in COLUMN_PICKER_ITEMS {
            let Some(action) = actions.get(*id) else {
                continue;
            };
            let show = column_by_id(view, id).is_some_and(|col| col.is_visible());
            action.set_state(&show.to_variant());
        }
        update_text_column_actions_enabled(view, &actions);
    });
}

pub(super) fn apply_picked_column_visibility(
    view: &gtk::ColumnView,
    id: &str,
    visible: bool,
    action: &gio::SimpleAction,
    actions: &HashMap<String, gio::SimpleAction>,
) {
    if !visible && would_hide_last_text_column_from_view(view, id) {
        return;
    }
    if let Some(col) = column_by_id(view, id) {
        col.set_visible(visible);
    }
    action.set_state(&visible.to_variant());
    persist_picked_column_visibility(id, visible);
    apply_column_expand_for_view(view);
    update_text_column_actions_enabled(view, actions);
}

pub(super) fn apply_column_expand(list: &MessageListView) {
    apply_column_expand_for_view(&list.view);
}

/// Subject flexes when visible so leftover pane width does not sit after Date.
/// If Subject is hidden, From flexes; Date only flexes as a last resort.
pub(super) fn preferred_expand_column(visible_ids: &[impl AsRef<str>]) -> Option<String> {
    let visible: Vec<&str> = visible_ids.iter().map(AsRef::as_ref).collect();
    if visible.contains(&"subject") {
        return Some("subject".to_string());
    }
    if visible.contains(&"from") {
        return Some("from".to_string());
    }
    visible
        .iter()
        .copied()
        .rev()
        .find(|id| MESSAGE_LIST_TEXT_COLUMNS.contains(id))
        .map(str::to_string)
}

pub(super) fn expand_column_id(view: &gtk::ColumnView) -> Option<String> {
    let visible: Vec<String> = visual_column_ids(view)
        .into_iter()
        .filter(|id| column_by_id(view, id).is_some_and(|col| col.is_visible()))
        .collect();
    preferred_expand_column(&visible)
}

pub(super) fn column_is_expand(view: &gtk::ColumnView, id: &str) -> bool {
    expand_column_id(view).is_some_and(|expand_id| expand_id == id)
}

pub(super) fn column_min(id: &str) -> i32 {
    match id {
        "from" => FROM_MIN,
        "folder" => FOLDER_MIN,
        "account" => ACCOUNT_MIN,
        "subject" => SUBJECT_MIN,
        "date" => DATE_MIN,
        "size" => SIZE_MIN,
        "deleted" => DATE_MIN,
        "attachments" => ATTACHMENTS_WIDTH,
        "star" => STAR_WIDTH,
        "unread" => UNREAD_WIDTH,
        "account-chip" => ACCOUNT_CHIP_WIDTH,
        _ => 0,
    }
}

pub(super) fn column_default(id: &str) -> i32 {
    match id {
        "from" => FROM_DEFAULT,
        "folder" => FOLDER_DEFAULT,
        "account" => ACCOUNT_DEFAULT,
        "subject" => SUBJECT_DEFAULT,
        "date" => DATE_DEFAULT,
        "size" => SIZE_DEFAULT,
        "deleted" => DATE_DEFAULT,
        "attachments" => ATTACHMENTS_WIDTH,
        "star" => STAR_WIDTH,
        _ => 0,
    }
}

pub(super) fn text_column_min(id: &str) -> i32 {
    column_min(id)
}

pub(super) fn text_column_default(id: &str) -> i32 {
    column_default(id)
}

pub(super) fn visual_column_ids(view: &gtk::ColumnView) -> Vec<String> {
    let cols = view.columns();
    let mut ids = Vec::new();
    for i in 0..cols.n_items() {
        let Some(col) = cols
            .item(i)
            .and_then(|item| item.downcast::<gtk::ColumnViewColumn>().ok())
        else {
            continue;
        };
        if let Some(id) = col.id() {
            ids.push(id.to_string());
        }
    }
    ids
}

pub(super) fn visible_column_ids(view: &gtk::ColumnView) -> Vec<String> {
    visual_column_ids(view)
        .into_iter()
        .filter(|id| column_by_id(view, id).is_some_and(|col| col.is_visible()))
        .collect()
}

pub(super) fn last_visible_column_id(view: &gtk::ColumnView) -> Option<String> {
    visible_column_ids(view).pop()
}

pub(super) fn apply_last_column_layout(view: &gtk::ColumnView) {
    let last = last_visible_column_id(view);
    for id in ICON_COLUMNS {
        let Some(col) = column_by_id(view, id) else {
            continue;
        };
        let pad = if last.as_deref() == Some(*id) {
            LAST_COLUMN_PAD
        } else {
            0
        };
        col.set_fixed_width(column_min(id) + pad);
        col.set_resizable(false);
    }
    for id in MESSAGE_LIST_USER_COLUMNS {
        view.remove_css_class(&format!("alcedo-last-col-{id}"));
    }
    view.remove_css_class("alcedo-last-col-deleted");
    view.remove_css_class("alcedo-last-col-drag");
    if let Some(id) = &last {
        view.add_css_class(&format!("alcedo-last-col-{id}"));
    }
    sync_header_alignment(view);
}

#[cfg(not(test))] // only called from the real (non-test) list setup
pub(super) fn wire_column_order_persist(list: &MessageListView) {
    let view = list.view.clone();
    view.columns().connect_items_changed(move |_, _, _, _| {
        if SUPPRESS_COLUMN_ORDER_PERSIST.get() {
            return;
        }
        SUPPRESS_COLUMN_ORDER_PERSIST.set(true);
        pin_drag_column(&view);
        pin_deleted_column(&view);
        SUPPRESS_COLUMN_ORDER_PERSIST.set(false);
        set_message_list_column_order_for(&current_column_scope(), &persistable_column_ids(&view));
        apply_column_expand_for_view(&view);
    });
}

pub(super) fn column_by_id(view: &gtk::ColumnView, id: &str) -> Option<gtk::ColumnViewColumn> {
    let cols = view.columns();
    for i in 0..cols.n_items() {
        let col = cols.item(i)?.downcast::<gtk::ColumnViewColumn>().ok()?;
        if col.id().as_deref() == Some(id) {
            return Some(col);
        }
    }
    None
}

pub(super) fn apply_saved_column_widths(view: &gtk::ColumnView) {
    let saved = message_list_column_widths_for(&current_column_scope());
    for id in MESSAGE_LIST_RESIZABLE_COLUMNS {
        if column_is_expand(view, id) {
            continue;
        }
        let Some(col) = column_by_id(view, id) else {
            continue;
        };
        let min = column_min(id);
        let default = column_default(id);
        let width = saved.get(*id).copied().unwrap_or(default);
        col.set_fixed_width(width.max(min));
    }
}

#[cfg(not(test))] // only called from the real (non-test) list setup
pub(super) fn wire_column_width_persist(
    list: &MessageListView,
    preferred_widths: Rc<RefCell<HashMap<String, i32>>>,
) {
    let debounce_source = Rc::new(Cell::new(None::<glib::SourceId>));

    let schedule_persist = {
        let debounce_source = debounce_source.clone();
        let preferred_widths = preferred_widths.clone();
        Rc::new(move || {
            if SUPPRESS_COLUMN_WIDTH_PERSIST.get() {
                return;
            }
            crate::glib_source::cancel_source(&debounce_source);
            let widths = preferred_widths.borrow().clone();
            let scope = current_column_scope();
            let debounce_source_cb = debounce_source.clone();
            let source_id = glib::timeout_add_local(Duration::from_millis(200), move || {
                let _ = debounce_source_cb.take();
                set_message_list_column_widths_for(&scope, &widths);
                glib::ControlFlow::Break
            });
            debounce_source.set(Some(source_id));
        })
    };

    for id in MESSAGE_LIST_RESIZABLE_COLUMNS {
        if let Some(col) = column_by_id(&list.view, id) {
            let schedule_persist = schedule_persist.clone();
            let preferred_widths = preferred_widths.clone();
            let id = (*id).to_string();
            col.connect_notify_local(Some("fixed-width"), move |col, _| {
                if SUPPRESS_COLUMN_WIDTH_PERSIST.get() {
                    return;
                }
                let width = col.fixed_width();
                if width >= column_min(&id) {
                    preferred_widths.borrow_mut().insert(id.clone(), width);
                }
                schedule_persist();
            });
        }
    }
}

#[cfg(not(test))]
pub(super) fn apply_saved_sort(list: &MessageListView) {
    let col_id = message_list_sort_column();
    let order = match message_list_sort_order().as_str() {
        "ascending" => gtk::SortType::Ascending,
        _ => gtk::SortType::Descending,
    };
    let col = list
        .get_columns()
        .get(col_id.as_str())
        .or_else(|| list.get_columns().get("date"));
    if let Some(col) = col {
        list.view.sort_by_column(Some(col), order);
    }
}

#[cfg(not(test))]
pub(super) fn wire_sort_persist(list: &MessageListView) {
    let view = list.view.clone();
    let debounce_source = Rc::new(Cell::new(None::<glib::SourceId>));
    let schedule_persist = {
        let debounce_source = debounce_source.clone();
        let view = view.clone();
        Rc::new(move || {
            crate::glib_source::cancel_source(&debounce_source);
            let debounce_source_cb = debounce_source.clone();
            let view = view.clone();
            let source_id = glib::timeout_add_local(Duration::from_millis(200), move || {
                let _ = debounce_source_cb.take();
                let Some(sorter) = view.sorter().and_downcast::<gtk::ColumnViewSorter>() else {
                    return glib::ControlFlow::Break;
                };
                if let Some(col) = sorter.primary_sort_column() {
                    let id = col.id().unwrap_or_default();
                    if id != "deleted" {
                        set_message_list_sort_column(&id);
                        let order = match sorter.primary_sort_order() {
                            gtk::SortType::Ascending => "ascending",
                            _ => "descending",
                        };
                        set_message_list_sort_order(order);
                    }
                }
                glib::ControlFlow::Break
            });
            debounce_source.set(Some(source_id));
        })
    };

    if let Some(sorter) = view.sorter().and_downcast::<gtk::ColumnViewSorter>() {
        sorter.connect_primary_sort_column_notify({
            let schedule_persist = schedule_persist.clone();
            move |_| schedule_persist()
        });
        sorter.connect_primary_sort_order_notify({
            let schedule_persist = schedule_persist.clone();
            move |_| schedule_persist()
        });
    }
}

#[cfg(not(test))]
pub(super) fn wire_header_alignment(list: &MessageListView) {
    let view = list.view.clone();
    let schedule = {
        let view = view.clone();
        Rc::new(move || {
            sync_header_alignment(&view);
            let view = view.clone();
            glib::idle_add_local_once(move || sync_header_alignment(&view));
        })
    };
    view.connect_map({
        let schedule = schedule.clone();
        move |_| schedule()
    });
    view.columns().connect_items_changed({
        let schedule = schedule.clone();
        move |_, _, _, _| schedule()
    });
    for id in MESSAGE_LIST_USER_COLUMNS {
        if let Some(col) = column_by_id(&view, id) {
            let schedule = schedule.clone();
            col.connect_notify_local(Some("visible"), move |_, _| schedule());
        }
    }
    if let Some(col) = column_by_id(&view, "deleted") {
        let schedule = schedule.clone();
        col.connect_notify_local(Some("visible"), move |_, _| schedule());
    }
    schedule();
}

pub(super) fn column_header_widgets(view: &gtk::ColumnView) -> Vec<gtk::Widget> {
    let Some(header) = view.first_child() else {
        return Vec::new();
    };
    if header.css_name().as_str() != "header" {
        return Vec::new();
    }
    let mut headers = Vec::new();
    let mut child = header.first_child();
    while let Some(next) = child {
        if next.is_visible() {
            headers.push(next.clone());
        }
        child = next.next_sibling();
    }
    headers
}

pub(super) fn header_title_label(widget: &gtk::Widget) -> Option<gtk::Label> {
    if let Ok(label) = widget.clone().downcast::<gtk::Label>()
        && !label.label().is_empty()
    {
        return Some(label);
    }
    let mut child = widget.first_child();
    while let Some(next) = child {
        if let Some(label) = header_title_label(&next) {
            return Some(label);
        }
        child = next.next_sibling();
    }
    None
}

pub(super) fn apply_header_alignment(widget: &gtk::Widget, is_last: bool, is_date: bool) {
    if is_last {
        widget.add_css_class("alcedo-col-header-end");
    } else {
        widget.remove_css_class("alcedo-col-header-end");
    }
    let align_end = is_date && is_last;
    if let Some(label) = header_title_label(widget) {
        label.set_xalign(if align_end { 1.0 } else { 0.0 });
        label.set_halign(if align_end {
            gtk::Align::End
        } else {
            gtk::Align::Start
        });
        shrink_ellipsize_label(&label);
    }
    if let Some(box_widget) = widget.first_child() {
        // GTK CSS has no `justify-content`, so we can't push the label+sort-icon
        // group to the end via CSS on a filled box. Instead, keep the box at its
        // natural (shrink-wrapped) width and align that whole group to the end.
        box_widget.set_halign(if align_end {
            gtk::Align::End
        } else {
            gtk::Align::Start
        });
        box_widget.set_hexpand(false);
    }
}

pub(super) fn column_header_icon(id: &str) -> Option<(&'static str, &'static str)> {
    match id {
        "unread" => Some(("mail-unread-symbolic", trs!("Unread"))),
        "attachments" => Some(("mail-attachment-symbolic", trs!("Attachments"))),
        "star" => Some(("non-starred-symbolic", trs!("Starred"))),
        _ => None,
    }
}

pub(super) fn header_content_box(widget: &gtk::Widget) -> Option<gtk::Box> {
    widget.first_child()?.downcast::<gtk::Box>().ok()
}

pub(super) fn header_icon_image(widget: &gtk::Widget) -> Option<gtk::Image> {
    if let Ok(image) = widget.clone().downcast::<gtk::Image>()
        && image.has_css_class("alcedo-col-header-icon")
    {
        return Some(image);
    }
    let mut child = widget.first_child();
    while let Some(next) = child {
        if let Some(image) = header_icon_image(&next) {
            return Some(image);
        }
        child = next.next_sibling();
    }
    None
}

pub(super) fn apply_header_icon(widget: &gtk::Widget, id: &str) {
    let Some((icon_name, tooltip)) = column_header_icon(id) else {
        widget.set_tooltip_text(None);
        if let Some(image) = header_icon_image(widget) {
            image.set_visible(false);
        }
        if let Some(label) = header_title_label(widget) {
            label.set_visible(true);
        }
        return;
    };

    widget.set_tooltip_text(Some(tooltip));
    widget.update_property(&[gtk::accessible::Property::Label(tooltip)]);
    if let Some(label) = header_title_label(widget) {
        label.set_visible(false);
    }
    if let Some(box_widget) = widget.first_child() {
        box_widget.set_halign(gtk::Align::Center);
    }
    if let Some(image) = header_icon_image(widget) {
        image.set_icon_name(Some(icon_name));
        image.set_visible(true);
        return;
    }
    let Some(box_widget) = header_content_box(widget) else {
        return;
    };
    let image = gtk::Image::builder()
        .icon_name(icon_name)
        .pixel_size(ICON_WIDTH)
        .css_classes(["dim-label", "alcedo-col-header-icon"])
        .valign(gtk::Align::Center)
        .halign(gtk::Align::Center)
        .build();
    box_widget.prepend(&image);
}

pub(super) fn apply_header_chrome(widget: &gtk::Widget, id: &str, is_last: bool) {
    apply_header_alignment(
        widget,
        is_last,
        id == "date" || id == "deleted" || id == "size",
    );
    apply_header_icon(widget, id);
}

/// The id of the column whose (translated) title is `title`, from the view's
/// own `(id, title)` pairs, so nothing depends on the English header text.
pub(super) fn column_id_for_title<'a>(
    columns: impl IntoIterator<Item = (&'a str, &'a str)>,
    title: &str,
) -> Option<&'a str> {
    if title.is_empty() {
        return None;
    }
    columns
        .into_iter()
        .find(|(_, column_title)| *column_title == title)
        .map(|(id, _)| id)
}

pub(super) fn header_id_from_title(view: &gtk::ColumnView, widget: &gtk::Widget) -> Option<String> {
    let label = header_title_label(widget)?;
    let cols = view.columns();
    let pairs: Vec<(String, String)> = (0..cols.n_items())
        .filter_map(|i| cols.item(i)?.downcast::<gtk::ColumnViewColumn>().ok())
        .filter_map(|col| Some((col.id()?.to_string(), col.title()?.to_string())))
        .collect();
    column_id_for_title(
        pairs
            .iter()
            .map(|(id, title)| (id.as_str(), title.as_str())),
        label.label().as_str(),
    )
    .map(str::to_string)
}

pub(super) fn sync_header_alignment(view: &gtk::ColumnView) {
    let visible_ids = visible_column_ids(view);
    let last = visible_ids.last().map(String::as_str);
    // Match each header by its title, not by child index. The drag column has
    // an empty title and GTK may insert extra header widgets, so a zip against
    // visible column ids paints unread/attachment/star icons onto From/Deleted.
    for widget in column_header_widgets(view) {
        let id = header_id_from_title(view, &widget).unwrap_or_default();
        apply_header_chrome(&widget, &id, last == Some(id.as_str()));
    }
}

pub(super) fn apply_column_expand_for_view(view: &gtk::ColumnView) {
    let saved = message_list_column_widths_for(&current_column_scope());
    let expand_id = expand_column_id(view);
    for id in MESSAGE_LIST_TEXT_COLUMNS {
        let Some(col) = column_by_id(view, id) else {
            continue;
        };
        if expand_id.as_deref() == Some(id) {
            col.set_expand(true);
            col.set_fixed_width(-1);
        } else {
            col.set_expand(false);
            if col.fixed_width() < 0 {
                let width = saved
                    .get(*id)
                    .copied()
                    .unwrap_or_else(|| text_column_default(id));
                col.set_fixed_width(width.max(text_column_min(id)));
            }
        }
    }
    apply_last_column_layout(view);
}
