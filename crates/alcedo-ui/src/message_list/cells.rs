//! Row cell factories (`RelmColumn` impls) and the per-row visual updates they share.

use super::*;

/// Ellipsized labels otherwise report their full text as min width, so
/// ColumnView rows cannot be measured at the viewport while resizing.
pub(super) fn shrink_ellipsize_label(label: &gtk::Label) {
    label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    label.set_single_line_mode(true);
    label.set_width_chars(1);
}

pub(super) fn install_row_context_menu(
    root: &gtk::Widget,
    message_id: &Rc<Cell<Option<MessageId>>>,
) {
    let gesture = gtk::GestureClick::new();
    gesture.set_button(3);
    let message_id = message_id.clone();
    gesture.connect_pressed(|gesture, _, _, _| {
        gesture.set_state(gtk::EventSequenceState::Claimed);
    });
    // Read the cell from the gesture: capturing it would be a cycle that keeps
    // cells alive after a column is removed and rebuilt.
    gesture.connect_released(move |gesture, _, x, y| {
        let Some(root_widget) = gesture.widget() else {
            return;
        };
        if let Some(id) = message_id.get() {
            let (popup_x, popup_y) = root_widget
                .ancestor(gtk::ColumnView::static_type())
                .and_then(|ancestor| ancestor.downcast::<gtk::ColumnView>().ok())
                .and_then(|list_view| {
                    root_widget
                        .compute_point(&list_view, &gtk::graphene::Point::new(x as f32, y as f32))
                })
                .map(|point| (f64::from(point.x()), f64::from(point.y())))
                .unwrap_or((x, y));
            ON_CONTEXT_MESSAGE.with(|slot| {
                if let Some(callback) = slot.borrow().as_ref() {
                    callback(id, popup_x, popup_y);
                }
            });
        }
    });
    root.add_controller(gesture);
}

pub(super) fn tag_row_widget(root: &gtk::Widget, message_id: &Rc<Cell<Option<MessageId>>>) {
    // SAFETY: GObject data is untyped; every read of "alcedo-mid" (`tagged_message_id`)
    // uses this same `Rc<Cell<Option<MessageId>>>` type. Relm4 TypedColumnView does the same.
    unsafe {
        root.set_data("alcedo-mid", message_id.clone());
    }
}

pub(super) fn tagged_message_id(widget: &gtk::Widget) -> Option<MessageId> {
    // SAFETY: "alcedo-mid" is only ever set by `tag_row_widget`, with this exact type.
    unsafe {
        widget
            .data::<Rc<Cell<Option<MessageId>>>>("alcedo-mid")
            .and_then(|ptr| ptr.as_ref().get())
    }
}

pub(super) fn tagged_flagged_cell(widget: &gtk::Widget) -> Option<Rc<Cell<bool>>> {
    // SAFETY: "alcedo-flagged" is only ever set in the star cell factory, with this exact type.
    unsafe {
        widget
            .data::<Rc<Cell<bool>>>("alcedo-flagged")
            .map(|ptr| ptr.as_ref().clone())
    }
}

pub(super) fn find_child_with_class(widget: &gtk::Widget, class: &str) -> Option<gtk::Widget> {
    let mut child = widget.first_child();
    while let Some(next) = child {
        if next.has_css_class(class) {
            return Some(next);
        }
        if let Some(found) = find_child_with_class(&next, class) {
            return Some(found);
        }
        child = next.next_sibling();
    }
    None
}

pub(super) fn apply_unread_visuals(pip: &gtk::Box, count_label: &gtk::Label, row: &MessageRow) {
    if row.unread_count == 0 {
        pip.set_visible(false);
        count_label.set_visible(false);
        return;
    }
    pip.set_visible(true);
    apply_pip_accent(pip, row.envelope.account_id, &row.envelope.account_accent);
    if row.unread_count == 1 {
        count_label.set_visible(false);
    } else {
        count_label.set_visible(true);
        count_label.set_label(&row.unread_count.to_string());
        let unread_label = format!("{} unread", row.unread_count);
        count_label.update_property(&[gtk::accessible::Property::Label(unread_label.as_str())]);
    }
}

pub(super) fn row_sender_name(row: &MessageRow) -> String {
    if row.depth == 0 && row.message_count > 1 {
        participant_summary(&row.participants, 3)
    } else {
        row.envelope
            .from
            .first()
            .map(|addr| addr.display_name())
            .unwrap_or_else(|| "Unknown".to_string())
    }
}

pub(super) fn apply_from_visuals(label: &gtk::Label, row: &MessageRow) {
    let sender_name = row_sender_name(row);
    label.set_label(&sender_name);
    if row.unread_count > 0 {
        label.add_css_class("unread-sender");
    } else {
        label.remove_css_class("unread-sender");
    }
    apply_thread_row_style(label.upcast_ref(), row, true);
}

pub(super) fn apply_folder_visuals(icon: &gtk::Image, label: &gtk::Label, envelope: &Envelope) {
    let (name, special) = FOLDER_LABELS.with(|slot| {
        slot.borrow()
            .get(&envelope.folder_id)
            .map(|folder| (folder.name.clone(), folder.special.clone()))
            .unwrap_or_else(|| ("Unknown".to_string(), SpecialUse::Other))
    });
    label.set_label(&name);
    icon.set_icon_name(Some(folder_icon_name(&special)));
    for class in [
        "alcedo-folder-icon-inbox",
        "alcedo-folder-icon-drafts",
        "alcedo-folder-icon-sent",
        "alcedo-folder-icon-junk",
        "alcedo-folder-icon-trash",
        "alcedo-folder-icon-archive",
        "alcedo-folder-icon-folder",
    ] {
        icon.remove_css_class(class);
    }
    icon.add_css_class(folder_icon_css_class(&special));
}

pub(super) fn apply_account_visuals(label: &gtk::Label, envelope: &Envelope) {
    let name = ACCOUNT_LABELS.with(|slot| {
        slot.borrow()
            .get(&envelope.account_id)
            .cloned()
            .unwrap_or_else(|| "Unknown".to_string())
    });
    label.set_label(&name);
}

pub(super) fn account_chip_tooltip(envelope: &Envelope) -> String {
    ACCOUNT_LABELS.with(|slot| {
        slot.borrow()
            .get(&envelope.account_id)
            .cloned()
            .unwrap_or_else(|| "Unknown account".to_string())
    })
}

pub(super) fn apply_account_chip_visuals(chip: &adw::Avatar, envelope: &Envelope) {
    let (initials, accent, image) = ACCOUNT_CHIPS.with(|slot| {
        if let Some(snap) = slot.borrow().get(&envelope.account_id) {
            (
                snap.initials.clone(),
                snap.accent.clone(),
                snap.image.clone(),
            )
        } else {
            let initials = if envelope.account_avatar.is_empty() {
                ACCOUNT_LABELS.with(|labels| {
                    labels
                        .borrow()
                        .get(&envelope.account_id)
                        .map(|label| avatar_from_label(label))
                        .unwrap_or_else(|| "?".to_string())
                })
            } else {
                envelope.account_avatar.clone()
            };
            (initials, envelope.account_accent.clone(), Vec::new())
        }
    });
    apply_account_chip_appearance(chip, &initials, &accent, &image);
    chip.set_tooltip_text(Some(&account_chip_tooltip(envelope)));
}

pub(super) fn apply_subject_visuals(
    subject_cell: &gtk::Box,
    subject: &gtk::Label,
    preview: &gtk::Label,
    expand_btn: &gtk::Button,
    expand_slot: &gtk::Box,
    row: &MessageRow,
) {
    let envelope = &row.envelope;
    subject.set_label(&envelope.display_subject());
    let snippet = envelope.display_snippet();
    preview.set_label(&snippet);
    preview.set_visible(preview_visible() && !snippet.is_empty());
    if row.unread_count > 0 {
        subject.add_css_class("unread-sender");
    } else {
        subject.remove_css_class("unread-sender");
    }
    if row.show_expand {
        expand_btn.set_visible(true);
        expand_slot.set_visible(false);
        let icon = if row.thread_expanded {
            "pan-down-symbolic"
        } else {
            "pan-end-symbolic"
        };
        expand_btn.set_icon_name(icon);
        expand_btn.set_tooltip_text(Some(if row.thread_expanded {
            "Collapse thread"
        } else {
            "Expand thread"
        }));
    } else {
        expand_btn.set_visible(false);
        expand_slot.set_visible(row.depth == 1);
    }
    apply_thread_row_style(subject_cell.upcast_ref(), row, true);
}

/// Indent and CSS on the cell itself (safe during Relm4 bind). Row grouping
/// classes go on the ColumnView row via idle — mutating that ancestor during
/// bind re-enters the factory and panics on Relm4's `steal_data("widgets")`.
pub(super) fn apply_thread_row_style(widget: &gtk::Widget, row: &MessageRow, indent: bool) {
    if indent {
        widget.set_margin_start(if row.depth == 1 {
            THREAD_CHILD_INDENT
        } else {
            0
        });
    }
    let is_child = row.depth == 1;
    let is_expanded_parent = row.depth == 0 && row.show_expand && row.thread_expanded;
    if is_child {
        widget.add_css_class("alcedo-msg-thread-child");
        widget.remove_css_class("alcedo-msg-thread-parent");
    } else if is_expanded_parent {
        widget.add_css_class("alcedo-msg-thread-parent");
        widget.remove_css_class("alcedo-msg-thread-child");
    } else {
        widget.remove_css_class("alcedo-msg-thread-child");
        widget.remove_css_class("alcedo-msg-thread-parent");
    }
    schedule_thread_row_classes(widget, row);
}

pub(super) fn schedule_thread_row_classes(from: &gtk::Widget, row: &MessageRow) {
    let Some(expected_id) = tagged_message_id(from) else {
        return;
    };
    if !glib::MainContext::ref_thread_default().is_owner() {
        return;
    }
    let from = from.clone();
    let depth = row.depth;
    let show_expand = row.show_expand;
    let thread_expanded = row.thread_expanded;
    glib::idle_add_local_once(move || {
        if tagged_message_id(&from) != Some(expected_id) {
            return;
        }
        apply_thread_row_classes_now(&from, depth, show_expand, thread_expanded);
    });
}

pub(super) fn apply_thread_row_classes_now(
    from: &gtk::Widget,
    depth: u8,
    show_expand: bool,
    thread_expanded: bool,
) {
    let Some(list_row) = column_view_row_widget(from) else {
        return;
    };
    let is_child = depth == 1;
    let is_expanded_parent = depth == 0 && show_expand && thread_expanded;
    if is_child {
        list_row.add_css_class("alcedo-msg-thread-child");
        list_row.remove_css_class("alcedo-msg-thread-parent");
    } else if is_expanded_parent {
        list_row.add_css_class("alcedo-msg-thread-parent");
        list_row.remove_css_class("alcedo-msg-thread-child");
    } else {
        list_row.remove_css_class("alcedo-msg-thread-child");
        list_row.remove_css_class("alcedo-msg-thread-parent");
    }
}

pub(super) fn column_view_row_widget(from: &gtk::Widget) -> Option<gtk::Widget> {
    let mut current = from.parent();
    while let Some(widget) = current {
        if widget.css_name().as_str() == "row" {
            return Some(widget);
        }
        current = widget.parent();
    }
    None
}

pub(super) fn column_view_cell_widget(from: &gtk::Widget) -> Option<gtk::Widget> {
    let mut current = from.parent();
    while let Some(widget) = current {
        if widget.css_name().as_str() == "cell" {
            return Some(widget);
        }
        current = widget.parent();
    }
    None
}

/// Adwaita pads every columnview cell; zero it on the drag column via CSS.
/// Mutating the cell during `parent_notify` re-enters ColumnView bind and segfaults.
pub(super) fn schedule_drag_column_cell_class(from: &gtk::Widget) {
    if !glib::MainContext::ref_thread_default().is_owner() {
        return;
    }
    let from = from.clone();
    glib::idle_add_local_once(move || {
        let Some(cell) = column_view_cell_widget(&from) else {
            return;
        };
        if !cell.has_css_class("alcedo-msg-drag-column") {
            cell.add_css_class("alcedo-msg-drag-column");
        }
    });
}

pub(super) fn apply_attachments_visuals(attachment: &gtk::Image, row: &MessageRow) {
    attachment.set_visible(row.any_attachments);
    attachment.set_tooltip_text(if row.any_attachments {
        Some("Has attachments")
    } else {
        None
    });
}

pub(super) fn apply_star_visuals(star: &gtk::Image, flagged_cell: &Cell<bool>, row: &MessageRow) {
    flagged_cell.set(row.any_flagged);
    star.set_icon_name(Some(if row.any_flagged {
        "starred-symbolic"
    } else {
        "non-starred-symbolic"
    }));
    star.set_tooltip_text(Some(if row.any_flagged {
        "Starred"
    } else {
        "Not starred"
    }));
}

pub(super) fn row_display_date(row: &MessageRow) -> DateTime<Utc> {
    if row.depth == 0 && row.message_count > 1 {
        row.sort_date
    } else {
        row.envelope.date
    }
}

pub(super) fn apply_date_visuals(label: &gtk::Label, row: &MessageRow) {
    label.set_label(&format_relative_time(row_display_date(row)));
}

pub(super) fn row_summary_text(row: &MessageRow) -> String {
    let from = row_sender_name(row);
    let subject = row.envelope.display_subject();
    let when = format_relative_time(row_display_date(row));
    row_accessible_summary(&RowSummary {
        unread_count: row.unread_count,
        starred: row.any_flagged,
        from: &from,
        subject: &subject,
        when: &when,
        has_attachments: row.any_attachments,
    })
}

/// ColumnView row accessible name. Deferred so it does not re-enter the factory.
pub(super) fn schedule_row_accessible(from: &gtk::Widget, row: &MessageRow) {
    let Some(expected_id) = tagged_message_id(from) else {
        return;
    };
    if !glib::MainContext::ref_thread_default().is_owner() {
        return;
    }
    let summary = row_summary_text(row);
    let description = row.envelope.display_snippet();
    let from = from.clone();
    glib::idle_add_local_once(move || {
        if tagged_message_id(&from) != Some(expected_id) {
            return;
        }
        let Some(list_row) = column_view_row_widget(&from) else {
            return;
        };
        list_row.update_property(&[
            gtk::accessible::Property::Label(&summary),
            gtk::accessible::Property::Description(&description),
        ]);
    });
}

pub(super) fn row_display_size(row: &MessageRow) -> i64 {
    if row.depth == 0 && row.message_count > 1 {
        row.sort_size
    } else {
        row.envelope.size_bytes
    }
}

pub(super) fn format_message_size(bytes: i64) -> String {
    if bytes <= 0 {
        return "—".to_string();
    }
    glib::format_size(bytes as u64).to_string()
}

pub(super) fn apply_size_visuals(label: &gtk::Label, row: &MessageRow) {
    let bytes = row_display_size(row);
    label.set_label(&format_message_size(bytes));
    let tip = if bytes <= 0 {
        "Size unknown".to_string()
    } else {
        format!("{bytes} bytes")
    };
    label.set_tooltip_text(Some(&tip));
}

pub(super) fn apply_deleted_visuals(label: &gtk::Label, envelope: &Envelope) {
    let text = envelope
        .deleted_at
        .map(format_relative_time)
        .unwrap_or_else(|| "—".to_string());
    label.set_label(&text);
}

pub(super) fn apply_row_visuals(view: &gtk::ColumnView, row: &MessageRow) {
    let envelope = &row.envelope;
    fn visit(widget: &gtk::Widget, row: &MessageRow, envelope: &Envelope) {
        if tagged_message_id(widget) == Some(envelope.id) {
            if widget.has_css_class("alcedo-msg-unread-cell") {
                if let (Some(pip), Some(count)) = (
                    find_child_with_class(widget, "alcedo-pip")
                        .and_then(|child| child.downcast::<gtk::Box>().ok()),
                    find_child_with_class(widget, "alcedo-unread-count")
                        .and_then(|child| child.downcast::<gtk::Label>().ok()),
                ) {
                    apply_unread_visuals(&pip, &count, row);
                }
                apply_thread_row_style(widget, row, false);
            } else if widget.has_css_class("alcedo-msg-from") {
                if let Ok(label) = widget.clone().downcast::<gtk::Label>() {
                    apply_from_visuals(&label, row);
                }
            } else if widget.has_css_class("alcedo-msg-folder-cell") {
                if widget.clone().downcast::<gtk::Box>().is_ok() {
                    let icon = find_child_with_class(widget, "alcedo-msg-folder-icon")
                        .and_then(|child| child.downcast::<gtk::Image>().ok());
                    let label = find_child_with_class(widget, "alcedo-msg-folder")
                        .and_then(|child| child.downcast::<gtk::Label>().ok());
                    if let (Some(icon), Some(label)) = (icon, label) {
                        apply_folder_visuals(&icon, &label, envelope);
                    }
                    apply_thread_row_style(widget, row, true);
                }
            } else if widget.has_css_class("alcedo-msg-account") {
                if let Ok(label) = widget.clone().downcast::<gtk::Label>() {
                    apply_account_visuals(&label, envelope);
                    apply_thread_row_style(widget, row, true);
                }
            } else if widget.has_css_class("alcedo-msg-account-chip-cell") {
                if let Some(chip) = find_child_with_class(widget, "alcedo-msg-account-chip")
                    .and_then(|child| child.downcast::<adw::Avatar>().ok())
                {
                    apply_account_chip_visuals(&chip, envelope);
                    apply_thread_row_style(widget, row, false);
                }
            } else if widget.has_css_class("alcedo-msg-subject-cell") {
                let subject = find_child_with_class(widget, "alcedo-msg-subject")
                    .and_then(|child| child.downcast::<gtk::Label>().ok());
                let preview = find_child_with_class(widget, "alcedo-msg-preview")
                    .and_then(|child| child.downcast::<gtk::Label>().ok());
                let expand_btn = find_child_with_class(widget, "alcedo-msg-thread-expand")
                    .and_then(|child| child.downcast::<gtk::Button>().ok());
                let expand_slot = find_child_with_class(widget, "alcedo-msg-thread-expand-slot")
                    .and_then(|child| child.downcast::<gtk::Box>().ok());
                if let (
                    Some(subject_cell),
                    Some(subject),
                    Some(preview),
                    Some(expand_btn),
                    Some(expand_slot),
                ) = (
                    widget.clone().downcast::<gtk::Box>().ok(),
                    subject,
                    preview,
                    expand_btn,
                    expand_slot,
                ) {
                    apply_subject_visuals(
                        &subject_cell,
                        &subject,
                        &preview,
                        &expand_btn,
                        &expand_slot,
                        row,
                    );
                }
            } else if widget.has_css_class("alcedo-msg-attachments-cell") {
                let attachment = find_child_with_class(widget, "alcedo-msg-attachment")
                    .and_then(|child| child.downcast::<gtk::Image>().ok());
                if let Some(attachment) = attachment {
                    apply_attachments_visuals(&attachment, row);
                }
            } else if widget.has_css_class("alcedo-msg-star-cell") {
                let star = find_child_with_class(widget, "alcedo-msg-star")
                    .and_then(|child| child.downcast::<gtk::Image>().ok());
                let flagged = tagged_flagged_cell(widget);
                if let (Some(star), Some(flagged)) = (star, flagged) {
                    apply_star_visuals(&star, flagged.as_ref(), row);
                }
            } else if widget.has_css_class("alcedo-msg-deleted") {
                if let Ok(label) = widget.clone().downcast::<gtk::Label>() {
                    apply_deleted_visuals(&label, envelope);
                }
            } else if widget.has_css_class("alcedo-msg-date") {
                if let Ok(label) = widget.clone().downcast::<gtk::Label>() {
                    apply_date_visuals(&label, row);
                }
            } else if widget.has_css_class("alcedo-msg-size")
                && let Ok(label) = widget.clone().downcast::<gtk::Label>()
            {
                apply_size_visuals(&label, row);
            }
        }
        let mut child = widget.first_child();
        while let Some(next) = child {
            visit(&next, row, envelope);
            child = next.next_sibling();
        }
    }
    visit(view.upcast_ref(), row, envelope);
}

pub(super) struct UnreadWidgets {
    pip: gtk::Box,
    count: gtk::Label,
    message_id: Rc<Cell<Option<MessageId>>>,
}

pub(super) struct DragHandleWidgets {
    message_id: Rc<Cell<Option<MessageId>>>,
}

pub(super) struct DragHandleColumn;

impl RelmColumn for DragHandleColumn {
    type Root = gtk::Box;
    type Widgets = DragHandleWidgets;
    type Item = MessageRow;

    const COLUMN_NAME: &'static str = "drag";

    fn header_title() -> String {
        String::new()
    }

    fn setup(_item: &gtk::ListItem) -> (Self::Root, Self::Widgets) {
        let handle = gtk::Image::builder()
            .icon_name("list-drag-handle-symbolic")
            .icon_size(gtk::IconSize::Normal)
            .pixel_size(ICON_WIDTH)
            .valign(gtk::Align::Center)
            .halign(gtk::Align::End)
            .margin_end(4)
            .build();
        handle.add_css_class("alcedo-msg-drag-handle");
        handle.update_property(&[gtk::accessible::Property::Label("Drag to a folder")]);
        // Clicks go to the filling cell so the whole drag column is a hit target.
        handle.set_can_target(false);

        // GtkBox gives a non-expanding child only its natural width and packs
        // it at the start, so halign on the icon never leaves the accent bar.
        let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        spacer.set_hexpand(true);
        spacer.set_can_target(false);

        let root = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        root.add_css_class("alcedo-msg-drag-cell");
        root.set_width_request(DRAG_WIDTH);
        root.set_hexpand(true);
        root.set_vexpand(true);
        root.set_halign(gtk::Align::Fill);
        root.set_valign(gtk::Align::Fill);
        root.set_can_target(true);
        root.set_tooltip_text(Some("Drag to a folder"));
        root.set_cursor_from_name(Some("grab"));
        root.append(&spacer);
        root.append(&handle);

        let message_id = Rc::new(Cell::new(None::<MessageId>));
        install_row_context_menu(root.upcast_ref(), &message_id);
        tag_row_widget(root.upcast_ref(), &message_id);
        #[cfg(not(test))]
        install_handle_cell_drag_source(&root, &message_id);
        (root, DragHandleWidgets { message_id })
    }

    fn bind(item: &mut Self::Item, widgets: &mut Self::Widgets, root: &mut Self::Root) {
        widgets.message_id.set(Some(item.envelope.id));
        schedule_drag_column_cell_class(root.upcast_ref());
        schedule_row_accessible(root.upcast_ref(), item);
    }
}

pub(super) struct UnreadColumn;

impl RelmColumn for UnreadColumn {
    type Root = gtk::Box;
    type Widgets = UnreadWidgets;
    type Item = MessageRow;

    const COLUMN_NAME: &'static str = "unread";
    // Fixed pip width — not user-resizable.

    fn header_title() -> String {
        "Unread".to_string()
    }

    fn setup(_item: &gtk::ListItem) -> (Self::Root, Self::Widgets) {
        let pip = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        pip.add_css_class("alcedo-pip");
        pip.set_valign(gtk::Align::Center);
        let count = gtk::Label::new(None);
        count.add_css_class("caption");
        count.add_css_class("dim-label");
        count.add_css_class("alcedo-unread-count");
        count.set_visible(false);
        let root = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        root.add_css_class("alcedo-msg-unread-cell");
        root.set_halign(gtk::Align::Center);
        root.append(&pip);
        root.append(&count);
        let message_id = Rc::new(Cell::new(None::<MessageId>));
        install_row_context_menu(root.upcast_ref(), &message_id);
        tag_row_widget(root.upcast_ref(), &message_id);
        (
            root,
            UnreadWidgets {
                pip,
                count,
                message_id,
            },
        )
    }

    fn bind(item: &mut Self::Item, widgets: &mut Self::Widgets, root: &mut Self::Root) {
        widgets.message_id.set(Some(item.envelope.id));
        apply_unread_visuals(&widgets.pip, &widgets.count, item);
        apply_thread_row_style(root.upcast_ref(), item, false);
    }
}

pub(super) struct FromWidgets {
    message_id: Rc<Cell<Option<MessageId>>>,
}

pub(super) struct FromColumn;

impl RelmColumn for FromColumn {
    type Root = gtk::Label;
    type Widgets = FromWidgets;
    type Item = MessageRow;

    const COLUMN_NAME: &'static str = "from";
    const ENABLE_RESIZE: bool = true;

    fn header_title() -> String {
        "From".to_string()
    }

    fn setup(_item: &gtk::ListItem) -> (Self::Root, Self::Widgets) {
        let label = gtk::Label::new(None);
        label.set_xalign(0.0);
        label.set_halign(gtk::Align::Start);
        shrink_ellipsize_label(&label);
        label.add_css_class("alcedo-msg-from");
        install_overflow_tooltip(&label);
        let message_id = Rc::new(Cell::new(None::<MessageId>));
        install_row_context_menu(label.upcast_ref(), &message_id);
        tag_row_widget(label.upcast_ref(), &message_id);
        (label, FromWidgets { message_id })
    }

    fn bind(item: &mut Self::Item, widgets: &mut Self::Widgets, label: &mut Self::Root) {
        widgets.message_id.set(Some(item.envelope.id));
        apply_from_visuals(label, item);
    }

    fn sort_fn() -> relm4::typed_view::OrdFn<Self::Item> {
        Some(Box::new(cmp_from))
    }
}

pub(super) struct FolderWidgets {
    icon: gtk::Image,
    label: gtk::Label,
    message_id: Rc<Cell<Option<MessageId>>>,
}

pub(super) struct FolderColumn;

impl RelmColumn for FolderColumn {
    type Root = gtk::Box;
    type Widgets = FolderWidgets;
    type Item = MessageRow;

    const COLUMN_NAME: &'static str = "folder";
    const ENABLE_RESIZE: bool = true;

    fn header_title() -> String {
        "Folder".to_string()
    }

    fn setup(_item: &gtk::ListItem) -> (Self::Root, Self::Widgets) {
        let icon = gtk::Image::new();
        icon.set_pixel_size(16);
        icon.add_css_class("alcedo-msg-folder-icon");
        icon.add_css_class("dim-label");

        let label = gtk::Label::new(None);
        label.set_xalign(0.0);
        label.set_halign(gtk::Align::Start);
        shrink_ellipsize_label(&label);
        label.add_css_class("alcedo-msg-folder");
        label.add_css_class("dim-label");
        install_overflow_tooltip(&label);

        let root = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        root.add_css_class("alcedo-msg-folder-cell");
        root.append(&icon);
        root.append(&label);

        let message_id = Rc::new(Cell::new(None::<MessageId>));
        install_row_context_menu(root.upcast_ref(), &message_id);
        tag_row_widget(root.upcast_ref(), &message_id);

        (
            root,
            FolderWidgets {
                icon,
                label,
                message_id,
            },
        )
    }

    fn bind(item: &mut Self::Item, widgets: &mut Self::Widgets, root: &mut Self::Root) {
        let envelope = &item.envelope;
        widgets.message_id.set(Some(envelope.id));
        apply_folder_visuals(&widgets.icon, &widgets.label, envelope);
        apply_thread_row_style(root.upcast_ref(), item, true);
    }

    fn sort_fn() -> relm4::typed_view::OrdFn<Self::Item> {
        Some(Box::new(cmp_folder))
    }
}

pub(super) fn cmp_folder(a: &MessageRow, b: &MessageRow) -> Ordering {
    let name = |row: &MessageRow| {
        FOLDER_LABELS.with(|slot| {
            slot.borrow()
                .get(&row.envelope.folder_id)
                .map(|label| label.name.to_ascii_lowercase())
                .unwrap_or_else(|| "unknown".to_string())
        })
    };
    cmp_keep_thread(a, b, name(a).cmp(&name(b)), false)
}

pub(super) struct AccountWidgets {
    message_id: Rc<Cell<Option<MessageId>>>,
}

pub(super) struct AccountColumn;

impl RelmColumn for AccountColumn {
    type Root = gtk::Label;
    type Widgets = AccountWidgets;
    type Item = MessageRow;

    const COLUMN_NAME: &'static str = "account";
    const ENABLE_RESIZE: bool = true;

    fn header_title() -> String {
        "Account".to_string()
    }

    fn setup(_item: &gtk::ListItem) -> (Self::Root, Self::Widgets) {
        let label = gtk::Label::new(None);
        label.set_xalign(0.0);
        label.set_halign(gtk::Align::Start);
        shrink_ellipsize_label(&label);
        label.add_css_class("alcedo-msg-account");
        label.add_css_class("dim-label");
        install_overflow_tooltip(&label);
        let message_id = Rc::new(Cell::new(None::<MessageId>));
        install_row_context_menu(label.upcast_ref(), &message_id);
        tag_row_widget(label.upcast_ref(), &message_id);
        (label, AccountWidgets { message_id })
    }

    fn bind(item: &mut Self::Item, widgets: &mut Self::Widgets, label: &mut Self::Root) {
        widgets.message_id.set(Some(item.envelope.id));
        apply_account_visuals(label, &item.envelope);
        apply_thread_row_style(label.upcast_ref(), item, true);
    }

    fn sort_fn() -> relm4::typed_view::OrdFn<Self::Item> {
        Some(Box::new(cmp_account))
    }
}

pub(super) fn cmp_account(a: &MessageRow, b: &MessageRow) -> Ordering {
    let name = |row: &MessageRow| {
        ACCOUNT_LABELS.with(|slot| {
            slot.borrow()
                .get(&row.envelope.account_id)
                .map(|label| label.to_ascii_lowercase())
                .unwrap_or_else(|| "unknown".to_string())
        })
    };
    cmp_keep_thread(a, b, name(a).cmp(&name(b)), false)
}

pub(super) struct AccountChipWidgets {
    chip: adw::Avatar,
    message_id: Rc<Cell<Option<MessageId>>>,
}

pub(super) struct AccountChipColumn;

impl RelmColumn for AccountChipColumn {
    type Root = gtk::Box;
    type Widgets = AccountChipWidgets;
    type Item = MessageRow;

    const COLUMN_NAME: &'static str = "account-chip";

    fn header_title() -> String {
        "Chip".to_string()
    }

    fn setup(_item: &gtk::ListItem) -> (Self::Root, Self::Widgets) {
        let chip = adw::Avatar::builder()
            .size(ACCOUNT_CHIP_SIZE)
            .show_initials(true)
            .can_focus(false)
            .build();
        chip.add_css_class("alcedo-msg-account-chip");

        let root = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        root.add_css_class("alcedo-msg-account-chip-cell");
        root.set_halign(gtk::Align::Center);
        root.set_valign(gtk::Align::Center);
        root.append(&chip);

        let message_id = Rc::new(Cell::new(None::<MessageId>));
        install_row_context_menu(root.upcast_ref(), &message_id);
        tag_row_widget(root.upcast_ref(), &message_id);

        (root, AccountChipWidgets { chip, message_id })
    }

    fn bind(item: &mut Self::Item, widgets: &mut Self::Widgets, root: &mut Self::Root) {
        widgets.message_id.set(Some(item.envelope.id));
        apply_account_chip_visuals(&widgets.chip, &item.envelope);
        apply_thread_row_style(root.upcast_ref(), item, false);
    }

    fn sort_fn() -> relm4::typed_view::OrdFn<Self::Item> {
        Some(Box::new(cmp_account))
    }
}

pub(super) struct SubjectWidgets {
    subject_cell: gtk::Box,
    subject: gtk::Label,
    preview: gtk::Label,
    expand_btn: gtk::Button,
    expand_slot: gtk::Box,
    message_id: Rc<Cell<Option<MessageId>>>,
    thread_id: Rc<Cell<Option<ThreadId>>>,
}

pub(super) struct SubjectColumn;

impl RelmColumn for SubjectColumn {
    type Root = gtk::Box;
    type Widgets = SubjectWidgets;
    type Item = MessageRow;

    const COLUMN_NAME: &'static str = "subject";
    const ENABLE_EXPAND: bool = true;

    fn header_title() -> String {
        "Subject".to_string()
    }

    fn setup(_item: &gtk::ListItem) -> (Self::Root, Self::Widgets) {
        let expand_btn = gtk::Button::from_icon_name("pan-end-symbolic");
        expand_btn.add_css_class("flat");
        expand_btn.add_css_class("alcedo-msg-thread-expand");
        expand_btn.set_has_frame(false);
        expand_btn.set_focus_on_click(false);
        expand_btn.set_valign(gtk::Align::Start);
        expand_btn.set_visible(false);

        let expand_slot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        expand_slot.add_css_class("alcedo-msg-thread-expand-slot");
        expand_slot.set_size_request(ICON_WIDTH, -1);
        expand_slot.set_can_target(false);
        expand_slot.set_visible(false);

        let subject = gtk::Label::new(None);
        subject.set_xalign(0.0);
        subject.set_halign(gtk::Align::Start);
        shrink_ellipsize_label(&subject);
        subject.set_hexpand(true);
        subject.add_css_class("alcedo-msg-subject");
        install_overflow_tooltip(&subject);

        let preview = gtk::Label::new(None);
        preview.set_xalign(0.0);
        preview.set_halign(gtk::Align::Start);
        shrink_ellipsize_label(&preview);
        preview.set_hexpand(true);
        preview.add_css_class("dim-label");
        preview.add_css_class("alcedo-msg-preview");
        install_overflow_tooltip(&preview);

        let text_col = gtk::Box::new(gtk::Orientation::Vertical, 2);
        text_col.set_hexpand(true);
        text_col.set_halign(gtk::Align::Fill);
        text_col.set_overflow(gtk::Overflow::Hidden);
        text_col.append(&subject);
        text_col.append(&preview);

        let subject_cell = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        subject_cell.add_css_class("alcedo-msg-subject-cell");
        subject_cell.set_hexpand(true);
        subject_cell.set_halign(gtk::Align::Fill);
        subject_cell.set_overflow(gtk::Overflow::Hidden);
        subject_cell.append(&expand_btn);
        subject_cell.append(&expand_slot);
        subject_cell.append(&text_col);

        let message_id = Rc::new(Cell::new(None::<MessageId>));
        let thread_id = Rc::new(Cell::new(None::<ThreadId>));
        install_row_context_menu(subject_cell.upcast_ref(), &message_id);
        tag_row_widget(subject_cell.upcast_ref(), &message_id);

        let thread_id_for_gesture = thread_id.clone();
        let expand_gesture = gtk::GestureClick::new();
        expand_gesture.connect_pressed(|gesture, _, _, _| {
            gesture.set_state(gtk::EventSequenceState::Claimed);
        });
        expand_gesture.connect_released(move |_, _, _, _| {
            if let Some(tid) = thread_id_for_gesture.get() {
                ON_TOGGLE_THREAD_EXPAND.with(|slot| {
                    if let Some(callback) = slot.borrow().as_ref() {
                        callback(tid);
                    }
                });
            }
        });
        expand_btn.add_controller(expand_gesture);

        (
            subject_cell.clone(),
            SubjectWidgets {
                subject_cell,
                subject,
                preview,
                expand_btn,
                expand_slot,
                message_id,
                thread_id,
            },
        )
    }

    fn bind(item: &mut Self::Item, widgets: &mut Self::Widgets, _root: &mut Self::Root) {
        widgets.message_id.set(Some(item.envelope.id));
        widgets.thread_id.set(Some(item.thread_id));
        apply_subject_visuals(
            &widgets.subject_cell,
            &widgets.subject,
            &widgets.preview,
            &widgets.expand_btn,
            &widgets.expand_slot,
            item,
        );
    }

    fn sort_fn() -> relm4::typed_view::OrdFn<Self::Item> {
        Some(Box::new(cmp_subject))
    }
}

pub(super) struct AttachmentsWidgets {
    message_id: Rc<Cell<Option<MessageId>>>,
    attachment: gtk::Image,
}

pub(super) struct AttachmentsColumn;

impl RelmColumn for AttachmentsColumn {
    type Root = gtk::Box;
    type Widgets = AttachmentsWidgets;
    type Item = MessageRow;

    const COLUMN_NAME: &'static str = "attachments";
    // Paperclip is a fixed-size icon — not user-resizable.

    fn header_title() -> String {
        "Attachments".to_string()
    }

    fn setup(_item: &gtk::ListItem) -> (Self::Root, Self::Widgets) {
        let attachment = gtk::Image::builder()
            .icon_name("mail-attachment-symbolic")
            .icon_size(gtk::IconSize::Normal)
            .pixel_size(ICON_WIDTH)
            .valign(gtk::Align::Center)
            .build();
        attachment.add_css_class("dim-label");
        attachment.add_css_class("alcedo-msg-attachment");

        let cell = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        cell.add_css_class("alcedo-msg-attachments-cell");
        cell.append(&attachment);

        let message_id = Rc::new(Cell::new(None::<MessageId>));
        install_row_context_menu(cell.upcast_ref(), &message_id);
        tag_row_widget(cell.upcast_ref(), &message_id);
        (
            cell,
            AttachmentsWidgets {
                message_id,
                attachment,
            },
        )
    }

    fn bind(item: &mut Self::Item, widgets: &mut Self::Widgets, _cell: &mut Self::Root) {
        widgets.message_id.set(Some(item.envelope.id));
        apply_attachments_visuals(&widgets.attachment, item);
    }
}

pub(super) struct StarWidgets {
    star: gtk::Image,
    message_id: Rc<Cell<Option<MessageId>>>,
    flagged: Rc<Cell<bool>>,
}

pub(super) struct StarColumn;

impl RelmColumn for StarColumn {
    type Root = gtk::Box;
    type Widgets = StarWidgets;
    type Item = MessageRow;

    const COLUMN_NAME: &'static str = "star";
    // Star is a fixed-size icon — not user-resizable.

    fn header_title() -> String {
        "Starred".to_string()
    }

    fn setup(_item: &gtk::ListItem) -> (Self::Root, Self::Widgets) {
        let message_id = Rc::new(Cell::new(None::<MessageId>));
        let flagged = Rc::new(Cell::new(false));

        let star = gtk::Image::builder()
            .icon_name("non-starred-symbolic")
            .icon_size(gtk::IconSize::Normal)
            .pixel_size(ICON_WIDTH)
            .valign(gtk::Align::Center)
            .build();
        star.add_css_class("alcedo-msg-star");

        let root = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        root.add_css_class("alcedo-msg-star-cell");
        root.set_valign(gtk::Align::Center);
        root.append(&star);

        let id_for_gesture = message_id.clone();
        let flagged_for_gesture = flagged.clone();
        let gesture = gtk::GestureClick::new();
        gesture.connect_pressed(|gesture, _, _, _| {
            gesture.set_state(gtk::EventSequenceState::Claimed);
        });
        gesture.connect_released(move |_, _, _, _| {
            if let Some(id) = id_for_gesture.get() {
                let next = !flagged_for_gesture.get();
                ON_TOGGLE_FLAG.with(|slot| {
                    if let Some(callback) = slot.borrow().as_ref() {
                        callback(id, next);
                    }
                });
            }
        });
        star.add_controller(gesture);
        install_row_context_menu(root.upcast_ref(), &message_id);
        tag_row_widget(root.upcast_ref(), &message_id);
        // SAFETY: the only reader, `tagged_flagged_cell`, uses this same `Rc<Cell<bool>>` type.
        unsafe {
            root.set_data("alcedo-flagged", flagged.clone());
        }

        (
            root,
            StarWidgets {
                star,
                message_id,
                flagged,
            },
        )
    }

    fn bind(item: &mut Self::Item, widgets: &mut Self::Widgets, _root: &mut Self::Root) {
        widgets.message_id.set(Some(item.envelope.id));
        apply_star_visuals(&widgets.star, widgets.flagged.as_ref(), item);
    }
}

pub(super) struct SizeWidgets {
    message_id: Rc<Cell<Option<MessageId>>>,
}

pub(super) struct SizeColumn;

impl RelmColumn for SizeColumn {
    type Root = gtk::Label;
    type Widgets = SizeWidgets;
    type Item = MessageRow;

    const COLUMN_NAME: &'static str = "size";
    const ENABLE_RESIZE: bool = true;

    fn header_title() -> String {
        "Size".to_string()
    }

    fn setup(_item: &gtk::ListItem) -> (Self::Root, Self::Widgets) {
        let label = gtk::Label::new(None);
        label.set_xalign(1.0);
        label.set_halign(gtk::Align::End);
        shrink_ellipsize_label(&label);
        label.add_css_class("dim-label");
        label.add_css_class("caption");
        label.add_css_class("numeric");
        label.add_css_class("alcedo-msg-size");
        install_overflow_tooltip(&label);
        let message_id = Rc::new(Cell::new(None::<MessageId>));
        install_row_context_menu(label.upcast_ref(), &message_id);
        tag_row_widget(label.upcast_ref(), &message_id);
        (label, SizeWidgets { message_id })
    }

    fn bind(item: &mut Self::Item, widgets: &mut Self::Widgets, label: &mut Self::Root) {
        widgets.message_id.set(Some(item.envelope.id));
        apply_size_visuals(label, item);
    }

    fn sort_fn() -> relm4::typed_view::OrdFn<Self::Item> {
        Some(Box::new(cmp_size))
    }
}

pub(super) struct DateWidgets {
    message_id: Rc<Cell<Option<MessageId>>>,
}

pub(super) struct DateColumn;

impl RelmColumn for DateColumn {
    type Root = gtk::Label;
    type Widgets = DateWidgets;
    type Item = MessageRow;

    const COLUMN_NAME: &'static str = "date";
    const ENABLE_RESIZE: bool = true;

    fn header_title() -> String {
        "Date".to_string()
    }

    fn setup(_item: &gtk::ListItem) -> (Self::Root, Self::Widgets) {
        let label = gtk::Label::new(None);
        label.set_xalign(1.0);
        label.set_halign(gtk::Align::End);
        shrink_ellipsize_label(&label);
        label.add_css_class("dim-label");
        label.add_css_class("caption");
        label.add_css_class("numeric");
        label.add_css_class("alcedo-msg-date");
        install_overflow_tooltip(&label);
        let message_id = Rc::new(Cell::new(None::<MessageId>));
        install_row_context_menu(label.upcast_ref(), &message_id);
        tag_row_widget(label.upcast_ref(), &message_id);
        (label, DateWidgets { message_id })
    }

    fn bind(item: &mut Self::Item, widgets: &mut Self::Widgets, label: &mut Self::Root) {
        widgets.message_id.set(Some(item.envelope.id));
        apply_date_visuals(label, item);
    }

    fn sort_fn() -> relm4::typed_view::OrdFn<Self::Item> {
        Some(Box::new(cmp_date))
    }
}

pub(super) struct DeletedWidgets {
    message_id: Rc<Cell<Option<MessageId>>>,
}

pub(super) struct DeletedColumn;

impl RelmColumn for DeletedColumn {
    type Root = gtk::Label;
    type Widgets = DeletedWidgets;
    type Item = MessageRow;

    const COLUMN_NAME: &'static str = "deleted";
    const ENABLE_RESIZE: bool = true;

    fn header_title() -> String {
        "Deleted".to_string()
    }

    fn setup(_item: &gtk::ListItem) -> (Self::Root, Self::Widgets) {
        let label = gtk::Label::new(None);
        label.set_xalign(1.0);
        label.set_halign(gtk::Align::End);
        shrink_ellipsize_label(&label);
        label.add_css_class("dim-label");
        label.add_css_class("caption");
        label.add_css_class("numeric");
        label.add_css_class("alcedo-msg-deleted");
        install_overflow_tooltip(&label);
        let message_id = Rc::new(Cell::new(None::<MessageId>));
        install_row_context_menu(label.upcast_ref(), &message_id);
        tag_row_widget(label.upcast_ref(), &message_id);
        (label, DeletedWidgets { message_id })
    }

    fn bind(item: &mut Self::Item, widgets: &mut Self::Widgets, label: &mut Self::Root) {
        widgets.message_id.set(Some(item.envelope.id));
        apply_deleted_visuals(label, &item.envelope);
    }

    fn sort_fn() -> relm4::typed_view::OrdFn<Self::Item> {
        Some(Box::new(cmp_deleted))
    }
}

pub(crate) fn cmp_deleted(a: &MessageRow, b: &MessageRow) -> Ordering {
    cmp_keep_thread(
        a,
        b,
        a.envelope.deleted_at.cmp(&b.envelope.deleted_at),
        false,
    )
}

pub(super) fn preview_visible() -> bool {
    show_message_preview()
}

pub(super) fn apply_preview_visibility(root: &impl IsA<gtk::Widget>) {
    let show = preview_visible();
    fn visit(widget: &gtk::Widget, show: bool) {
        if widget.has_css_class("alcedo-msg-preview")
            && let Ok(label) = widget.clone().downcast::<gtk::Label>()
        {
            label.set_visible(show && !label.text().is_empty());
        }
        let mut child = widget.first_child();
        while let Some(next) = child {
            visit(&next, show);
            child = next.next_sibling();
        }
    }
    visit(root.upcast_ref(), show);
}

pub(super) fn format_relative_time(date: DateTime<Utc>) -> String {
    let local = date.with_timezone(&Local);
    let now = Local::now();
    let time = local.format("%-I:%M %p").to_string();
    if local.date_naive() == now.date_naive() {
        time
    } else if local.year() == now.year() {
        format!("{} · {time}", local.format("%b %-d"))
    } else {
        format!("{} · {time}", local.format("%b %-d, %Y"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cell_with_context_menu_is_freed_after_removal() {
        crate::test_helpers::gtk_test_run(|| {
            assert!(crate::test_helpers::row_is_freed_after_removal(
                |row, sentinel| {
                    install_row_context_menu(row.upcast_ref(), &Rc::new(Cell::new(None)));
                    drop(sentinel);
                }
            ));
        });
    }
}
