//! Column picker popover: show/hide and reorder message-list columns.

use super::*;

pub(super) const COLUMN_PICKER_ITEMS: &[(&str, &str)] = &[
    ("unread", "Unread"),
    ("from", "From"),
    ("folder", "Folder"),
    ("account", "Account"),
    ("account-chip", "Account chip"),
    ("subject", "Subject"),
    ("attachments", "Attachments"),
    ("star", "Starred"),
    ("size", "Size"),
    ("date", "Date"),
];

pub(super) fn column_picker_menu() -> gio::Menu {
    let menu = gio::Menu::new();
    for (id, label) in COLUMN_PICKER_ITEMS {
        menu.append(Some(label), Some(&format!("cols.{id}")));
    }
    menu
}

pub(super) fn would_hide_last_text_column_from_view(view: &gtk::ColumnView, id: &str) -> bool {
    if !MESSAGE_LIST_TEXT_COLUMNS.contains(&id) {
        return false;
    }
    let visible_text = MESSAGE_LIST_TEXT_COLUMNS
        .iter()
        .filter(|text_id| column_by_id(view, text_id).is_some_and(|col| col.is_visible()))
        .count();
    visible_text == 1 && column_by_id(view, id).is_some_and(|col| col.is_visible())
}

pub(super) fn update_text_column_actions_enabled(
    view: &gtk::ColumnView,
    actions: &HashMap<String, gio::SimpleAction>,
) {
    let visible_text: Vec<&str> = MESSAGE_LIST_TEXT_COLUMNS
        .iter()
        .copied()
        .filter(|id| column_by_id(view, id).is_some_and(|col| col.is_visible()))
        .collect();
    let lock_last = visible_text.len() == 1;
    for id in MESSAGE_LIST_TEXT_COLUMNS {
        if let Some(action) = actions.get(*id) {
            action.set_enabled(!lock_last || visible_text[0] != *id);
        }
    }
}

pub(super) fn column_picker_title(id: &str) -> &str {
    COLUMN_PICKER_ITEMS
        .iter()
        .find(|(column_id, _)| *column_id == id)
        .map(|(_, title)| *title)
        .unwrap_or(id)
}

pub(super) fn find_widget_by_name(root: &gtk::Widget, name: &str) -> Option<gtk::Widget> {
    if root.widget_name() == name {
        return Some(root.clone());
    }
    let mut child = root.first_child();
    while let Some(next) = child {
        if let Some(found) = find_widget_by_name(&next, name) {
            return Some(found);
        }
        child = next.next_sibling();
    }
    None
}

pub(super) fn focus_column_order_button(list_box: &gtk::ListBox, id: &str, prefer_up: bool) {
    let up_name = format!("{id}-up");
    let down_name = format!("{id}-down");
    if prefer_up
        && let Some(up) = find_widget_by_name(list_box.upcast_ref(), &up_name)
        && up.is_sensitive()
    {
        up.grab_focus();
        return;
    }
    if let Some(down) = find_widget_by_name(list_box.upcast_ref(), &down_name) {
        down.grab_focus();
    }
}

pub(super) enum ColumnOrderFocus {
    Up(String),
    Down(String),
}

pub(super) fn fill_column_order_list(
    list_box: &gtk::ListBox,
    view: &gtk::ColumnView,
    actions: &HashMap<String, gio::SimpleAction>,
    focus_after: Option<ColumnOrderFocus>,
) {
    while let Some(row) = list_box.row_at_index(0) {
        list_box.remove(&row);
    }

    let order = message_list_column_order_for(&current_column_scope());
    let last_index = order.len().saturating_sub(1);
    for (index, id) in order.iter().enumerate() {
        let title = column_picker_title(id);
        let col = column_by_id(view, id);
        let row = adw::ActionRow::builder()
            .title(title)
            .activatable(false)
            .build();

        let switch = gtk::Switch::builder()
            .valign(gtk::Align::Center)
            .active(col.as_ref().is_some_and(|column| column.is_visible()))
            .sensitive(!would_hide_last_text_column_from_view(view, id))
            .build();
        switch.connect_state_set({
            let list_view = view.clone();
            let id_owned = id.clone();
            let actions_for_switch = actions.clone();
            move |_, state| {
                let Some(action) = actions_for_switch.get(&id_owned) else {
                    return glib::Propagation::Stop;
                };
                if !state && would_hide_last_text_column_from_view(&list_view, &id_owned) {
                    return glib::Propagation::Stop;
                }
                apply_picked_column_visibility(
                    &list_view,
                    &id_owned,
                    state,
                    action,
                    &actions_for_switch,
                );
                glib::Propagation::Proceed
            }
        });
        row.add_prefix(&switch);

        let btn_box = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        btn_box.add_css_class("linked");

        let up_btn = gtk::Button::builder()
            .name(format!("{id}-up"))
            .icon_name("go-up-symbolic")
            .tooltip_text(format!("Move {title} up"))
            .css_classes(["flat"])
            .sensitive(index > 0)
            .build();
        let down_btn = gtk::Button::builder()
            .name(format!("{id}-down"))
            .icon_name("go-down-symbolic")
            .tooltip_text(format!("Move {title} down"))
            .css_classes(["flat"])
            .sensitive(index < last_index)
            .build();

        let view_for_up = view.clone();
        let list_box_for_up = list_box.clone();
        let actions_for_up = actions.clone();
        let id_for_up = id.clone();
        up_btn.connect_clicked(move |_| {
            let scope = current_column_scope();
            set_message_list_column_order_for(
                &scope,
                &move_message_list_column(&message_list_column_order_for(&scope), &id_for_up, -1),
            );
            apply_saved_column_order_for_view(&view_for_up);
            fill_column_order_list(
                &list_box_for_up,
                &view_for_up,
                &actions_for_up,
                Some(ColumnOrderFocus::Up(id_for_up.clone())),
            );
        });

        let view_for_down = view.clone();
        let list_box_for_down = list_box.clone();
        let actions_for_down = actions.clone();
        let id_for_down = id.clone();
        down_btn.connect_clicked(move |_| {
            let scope = current_column_scope();
            set_message_list_column_order_for(
                &scope,
                &move_message_list_column(&message_list_column_order_for(&scope), &id_for_down, 1),
            );
            apply_saved_column_order_for_view(&view_for_down);
            fill_column_order_list(
                &list_box_for_down,
                &view_for_down,
                &actions_for_down,
                Some(ColumnOrderFocus::Down(id_for_down.clone())),
            );
        });

        btn_box.append(&up_btn);
        btn_box.append(&down_btn);
        row.add_suffix(&btn_box);
        list_box.append(&row);
    }

    let reset_row = adw::ButtonRow::builder().title("Reset order").build();
    let view_for_reset = view.clone();
    let list_box_for_reset = list_box.clone();
    let actions_for_reset = actions.clone();
    reset_row.connect_activated(move |_| {
        let default: Vec<String> = MESSAGE_LIST_USER_COLUMNS
            .iter()
            .map(|column_id| (*column_id).to_string())
            .collect();
        set_message_list_column_order_for(&current_column_scope(), &default);
        apply_saved_column_order_for_view(&view_for_reset);
        fill_column_order_list(
            &list_box_for_reset,
            &view_for_reset,
            &actions_for_reset,
            None,
        );
    });
    list_box.append(&reset_row);

    if let Some(focus) = focus_after {
        match focus {
            ColumnOrderFocus::Up(id) => focus_column_order_button(list_box, &id, true),
            ColumnOrderFocus::Down(id) => focus_column_order_button(list_box, &id, false),
        }
    }
}

pub(super) fn install_column_picker(
    list: &MessageListView,
    pane_root: &gtk::Widget,
    popover: &gtk::Popover,
    content: &gtk::Box,
) {
    let menu = column_picker_menu();
    let visible = message_list_column_visible_for(&current_column_scope());
    let group = gio::SimpleActionGroup::new();
    let actions: Rc<RefCell<HashMap<String, gio::SimpleAction>>> =
        Rc::new(RefCell::new(HashMap::new()));

    for (id, _) in COLUMN_PICKER_ITEMS {
        let _col = list.get_columns().get(id).cloned();
        let actions_for_cb = actions.clone();
        let id_owned = id.to_string();
        let action = gio::SimpleAction::new_stateful(id, None, &visible.contains(*id).to_variant());
        action.connect_change_state({
            let list_view = list.view.clone();
            let id_owned = id_owned.clone();
            move |action, value| {
                let show = value
                    .map(|variant| variant.get::<bool>().unwrap_or(true))
                    .unwrap_or(true);
                apply_picked_column_visibility(
                    &list_view,
                    &id_owned,
                    show,
                    action,
                    &actions_for_cb.borrow(),
                );
            }
        });
        actions.borrow_mut().insert(id.to_string(), action.clone());
        group.add_action(&action);
    }

    list.view.insert_action_group("cols", Some(&group));
    pane_root.insert_action_group("cols", Some(&group));

    for (id, _) in COLUMN_PICKER_ITEMS {
        if let Some(col) = list.get_columns().get(id) {
            col.set_header_menu(Some(&menu));
        }
    }

    update_text_column_actions_enabled(&list.view, &actions.borrow());

    let caption = gtk::Label::new(Some("Top of the list is the leftmost column."));
    caption.add_css_class("dim-label");
    caption.set_wrap(true);
    caption.set_xalign(0.0);
    content.append(&caption);

    let list_box = gtk::ListBox::new();
    list_box.set_selection_mode(gtk::SelectionMode::None);
    list_box.add_css_class("boxed-list");
    content.append(&list_box);

    let view_for_popover = list.view.clone();
    let list_box_for_show = list_box.clone();
    let actions_for_show = actions.clone();
    popover.connect_show(move |_| {
        fill_column_order_list(
            &list_box_for_show,
            &view_for_popover,
            &actions_for_show.borrow(),
            None,
        );
    });

    COLUMN_PICKER_ACTIONS.with(|slot| *slot.borrow_mut() = Some(actions));
}
