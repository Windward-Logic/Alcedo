//! Drag-and-drop of message rows: payload encoding and the handle-cell drag source.

use super::*;

pub fn format_message_id_payload(ids: &[MessageId]) -> String {
    ids.iter()
        .map(|id| id.0.to_string())
        .collect::<Vec<_>>()
        .join(",")
}

pub fn parse_message_id_payload(text: &str) -> Vec<MessageId> {
    text.split(',')
        .filter_map(|part| part.trim().parse().ok())
        .map(MessageId)
        .collect()
}

pub fn list_drag_payload(
    hit: Option<MessageId>,
    selected: &[MessageId],
    _list: &MessageListView,
) -> Option<Vec<MessageId>> {
    let hit = hit?;
    if selected.contains(&hit) {
        Some(selected.to_vec())
    } else {
        Some(vec![hit])
    }
}

/// Handle-cell `prepare`: prefer the ColumnView Capture-press bitset, else
/// the live MultiSelection. Same payload rule.
///
/// Capture runs parent→child. Snapshot on the ColumnView so the bitset is
/// saved before the inner ListView collapses it on button-1. A snapshot on
/// the handle cell is too late (ListView is an ancestor).
pub fn handle_drag_payload(
    row_id: Option<MessageId>,
    press_selected: &[MessageId],
    live_selected: &[MessageId],
    list: &MessageListView,
) -> Option<Vec<MessageId>> {
    let selected = if press_selected.is_empty() {
        live_selected
    } else {
        press_selected
    };
    list_drag_payload(row_id, selected, list)
}

/// Star cells must not start a row drag; click-under-threshold still toggles.
pub fn list_drag_hit(message_id: Option<MessageId>, on_star: bool) -> Option<MessageId> {
    if on_star { None } else { message_id }
}

/// `GtkDragSource::prepare` runs after the drag threshold. Keep the row that
/// was pressed so the left-edge handle still starts a drag.
pub fn list_drag_prepare_hit(
    press_hit: Option<MessageId>,
    pointer_hit: Option<MessageId>,
) -> Option<MessageId> {
    press_hit.or(pointer_hit)
}

#[cfg(not(test))]
pub(super) fn column_view_multi_selection(widget: &gtk::Widget) -> Option<gtk::MultiSelection> {
    widget
        .ancestor(gtk::ColumnView::static_type())
        .and_then(|view| view.downcast::<gtk::ColumnView>().ok())?
        .model()?
        .downcast::<gtk::MultiSelection>()
        .ok()
}

#[cfg(not(test))]
pub(super) fn column_view_selected_ids(widget: &gtk::Widget) -> Vec<MessageId> {
    column_view_multi_selection(widget)
        .map(|model| selected_ids_from_selection(&model))
        .unwrap_or_default()
}

/// Capture-phase snapshot on the ColumnView (parent of ListView). Must not
/// Claim: rubberband still owns empty-background drags; the handle cell owns
/// row DnD. Button 0 so a button-3 copy-drag still sees the multi-select.
#[cfg(not(test))]
pub(super) fn install_column_view_press_snapshot(view: &gtk::ColumnView) {
    let store = Rc::new(RefCell::new(Vec::<MessageId>::new()));
    // SAFETY: the only reader, `column_view_press_ids`, uses this same type.
    unsafe {
        view.set_data("alcedo-press-ids", store.clone());
    }
    let press = gtk::GestureClick::new();
    press.set_button(0);
    press.set_propagation_phase(gtk::PropagationPhase::Capture);
    let view_for_press = view.clone();
    press.connect_pressed(move |_, _, _, _| {
        let ids = view_for_press
            .model()
            .and_then(|model| model.downcast::<gtk::MultiSelection>().ok())
            .map(|model| selected_ids_from_selection(&model))
            .unwrap_or_default();
        *store.borrow_mut() = ids;
    });
    view.add_controller(press);
}

#[cfg(not(test))]
pub(super) fn column_view_press_ids(widget: &gtk::Widget) -> Vec<MessageId> {
    widget
        .ancestor(gtk::ColumnView::static_type())
        // SAFETY: "alcedo-press-ids" is only set by `install_column_view_press_snapshot`,
        // with this exact type.
        .and_then(|view| unsafe {
            view.data::<Rc<RefCell<Vec<MessageId>>>>("alcedo-press-ids")
                .map(|ptr| ptr.as_ref().borrow().clone())
        })
        .unwrap_or_default()
}

/// The handle cell owns the drag. Primary press is claimed in Capture so
/// ColumnView rubberband never sees these drags. Group payload comes from
/// the ColumnView Capture snapshot, not this cell.
/// Button 1 offers MOVE|COPY (Ctrl at the folder selects COPY). Button 3 is
/// COPY-only (right-click-drag).
#[cfg(not(test))]
pub(super) fn install_handle_cell_drag_source(
    root: &gtk::Box,
    message_id: &Rc<Cell<Option<MessageId>>>,
) {
    attach_handle_drag_source(
        root,
        message_id,
        gtk::gdk::BUTTON_PRIMARY,
        gtk::gdk::DragAction::MOVE | gtk::gdk::DragAction::COPY,
    );
    attach_handle_drag_source(
        root,
        message_id,
        gtk::gdk::BUTTON_SECONDARY,
        gtk::gdk::DragAction::COPY,
    );
}

#[cfg(not(test))]
pub(super) fn column_view_drag_payload(
    _widget: &gtk::Widget,
    hit: Option<MessageId>,
    press_selected: &[MessageId],
    live_selected: &[MessageId],
) -> Option<Vec<MessageId>> {
    let hit = hit?;
    let selected = if press_selected.is_empty() {
        live_selected
    } else {
        press_selected
    };
    if selected.contains(&hit) {
        Some(selected.to_vec())
    } else {
        Some(vec![hit])
    }
}

#[cfg(not(test))]
pub(super) fn attach_handle_drag_source(
    root: &gtk::Box,
    message_id: &Rc<Cell<Option<MessageId>>>,
    button: u32,
    actions: gtk::gdk::DragAction,
) {
    let drag_source = gtk::DragSource::new();
    drag_source.set_actions(actions);
    drag_source.set_button(button);
    drag_source.set_propagation_phase(gtk::PropagationPhase::Capture);
    // Claim on press, before the event bubbles. ColumnView rubberband claims
    // at the drag threshold and rewrites a multi-selection. Button 3 stays
    // unclaimed so the handle context menu still opens.
    if button == gtk::gdk::BUTTON_PRIMARY {
        drag_source.connect_begin(|gesture, _| {
            gesture.set_state(gtk::EventSequenceState::Claimed);
        });
    }
    let row_id = message_id.clone();
    let payload_ids = Rc::new(RefCell::new(Vec::<MessageId>::new()));
    let payload_for_prep = payload_ids.clone();
    // The cell is read from the source, not captured: a captured cell would be
    // a cycle that outlives the cell when its column is rebuilt.
    drag_source.connect_prepare(move |source, _, _| {
        let root_for_prep = source.widget()?;
        let press_ids = column_view_press_ids(root_for_prep.upcast_ref());
        let live = column_view_selected_ids(root_for_prep.upcast_ref());
        let ids =
            column_view_drag_payload(root_for_prep.upcast_ref(), row_id.get(), &press_ids, &live)?;
        *payload_for_prep.borrow_mut() = ids.clone();
        if ids.len() > 1
            && let Some(model) = column_view_multi_selection(root_for_prep.upcast_ref())
        {
            restore_selection_on_model(&model, &ids);
        }
        Some(gtk::gdk::ContentProvider::for_value(
            &format_message_id_payload(&ids).to_value(),
        ))
    });
    drag_source.connect_drag_begin(move |source, _| {
        let Some(root_for_begin) = source.widget() else {
            return;
        };
        let ids = payload_ids.borrow().clone();
        if ids.len() > 1
            && let Some(model) = column_view_multi_selection(root_for_begin.upcast_ref())
        {
            restore_selection_on_model(&model, &ids);
        }
    });
    root.add_controller(drag_source);
}
