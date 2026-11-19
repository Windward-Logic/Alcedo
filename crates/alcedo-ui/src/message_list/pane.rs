//! Messages pane chrome: selection bar, new-mail chip, empty state, view options, and pane assembly.

use super::*;
use alcedo_i18n::{ntr, tr, trs};

pub(super) fn sync_empty_state(content_stack: &gtk::Stack, list_view: &gtk::ColumnView) {
    let empty = list_view
        .model()
        .map(|model| model.n_items() == 0)
        .unwrap_or(true);
    content_stack.set_visible_child_name(if empty { "empty" } else { "list" });
}

pub(super) fn wire_empty_state(content_stack: &gtk::Stack, list_view: &gtk::ColumnView) {
    // Initial sync only; `populate_list_preserved` drives visibility after that.
    sync_empty_state(content_stack, list_view);
}

pub(super) fn action_icon_button(icon: &str, tooltip: &str, action: &str) -> gtk::Button {
    gtk::Button::builder()
        .icon_name(icon)
        .tooltip_text(tooltip)
        .css_classes(["flat"])
        .focus_on_click(false)
        .action_name(action)
        .build()
}

pub(super) fn action_icon_toggle(icon: &str, tooltip: &str) -> gtk::ToggleButton {
    gtk::ToggleButton::builder()
        .icon_name(icon)
        .tooltip_text(tooltip)
        .css_classes(["flat"])
        .focus_on_click(false)
        .build()
}

pub(super) fn connect_state_toggle(
    btn: &gtk::ToggleButton,
    syncing: &Rc<Cell<bool>>,
    mixed: &Rc<Cell<bool>>,
    activate_action: &'static str,
    deactivate_action: &'static str,
) {
    let syncing = syncing.clone();
    let mixed = mixed.clone();
    btn.connect_toggled(move |btn| {
        if syncing.get() {
            return;
        }
        if mixed.get() {
            mixed.set(false);
            btn.remove_css_class("mixed");
        }
        let action = if btn.is_active() {
            activate_action
        } else {
            deactivate_action
        };
        let _ = btn.activate_action(action, None);
    });
}

pub(super) fn sync_toggle_visual(
    btn: &gtk::ToggleButton,
    active: bool,
    on_icon: &str,
    off_icon: &str,
    on_tooltip: &str,
    off_tooltip: &str,
) {
    btn.remove_css_class("mixed");
    btn.set_active(active);
    btn.set_icon_name(if active { on_icon } else { off_icon });
    btn.set_tooltip_text(Some(if active { on_tooltip } else { off_tooltip }));
}

pub(super) fn sync_tri_state_toggle(
    btn: &gtk::ToggleButton,
    all_on: bool,
    mixed: bool,
    on_icon: &str,
    off_icon: &str,
    on_tooltip: &str,
    off_tooltip: &str,
    mixed_tooltip: &str,
) {
    if mixed {
        btn.add_css_class("mixed");
        btn.set_active(false);
        btn.set_icon_name(off_icon);
        btn.set_tooltip_text(Some(mixed_tooltip));
    } else {
        sync_toggle_visual(btn, all_on, on_icon, off_icon, on_tooltip, off_tooltip);
    }
}

pub(super) fn action_label_button(label: &str, tooltip: &str, action: &str) -> gtk::Button {
    gtk::Button::builder()
        .label(label)
        .tooltip_text(tooltip)
        .css_classes(["flat"])
        .focus_on_click(false)
        .action_name(action)
        .build()
}

/// Bulk-selection action bar shown under the messages header.
pub struct SelectionBar {
    pub revealer: gtk::Revealer,
    count_label: gtk::Label,
    read_btn: gtk::ToggleButton,
    star_btn: gtk::ToggleButton,
    delete_btn: gtk::Button,
    select_all_btn: gtk::Button,
    syncing: Rc<Cell<bool>>,
    read_mixed: Rc<Cell<bool>>,
    star_mixed: Rc<Cell<bool>>,
}

impl SelectionBar {
    fn new() -> Self {
        let count_label = gtk::Label::builder()
            .css_classes(["caption", "dim-label"])
            .margin_end(8)
            .build();

        let syncing = Rc::new(Cell::new(false));
        let read_mixed = Rc::new(Cell::new(false));
        let star_mixed = Rc::new(Cell::new(false));
        let read_btn = action_icon_toggle("mail-unread-symbolic", &tr!("Mark as Read"));
        read_btn.add_css_class("alcedo-sel-read");
        let star_btn = action_icon_toggle("non-starred-symbolic", &tr!("Star"));
        star_btn.add_css_class("alcedo-sel-star");
        connect_state_toggle(
            &read_btn,
            &syncing,
            &read_mixed,
            "win.mail-mark-read",
            "win.mail-mark-unread",
        );
        connect_state_toggle(
            &star_btn,
            &syncing,
            &star_mixed,
            "win.mail-star",
            "win.mail-unstar",
        );
        let delete_btn =
            action_icon_button("user-trash-symbolic", &tr!("Delete"), "win.mail-delete");
        let archive_btn =
            action_icon_button(archive_icon_name(), &tr!("Archive"), "win.mail-archive");
        let select_all_btn = action_label_button(
            &tr!("Select All"),
            &tr!("Select All"),
            "win.mail-select-all",
        );

        let action_bar = gtk::ActionBar::builder()
            .css_classes(["alcedo-selection-bar"])
            .build();
        action_bar.pack_start(&count_label);
        action_bar.pack_start(&read_btn);
        action_bar.pack_start(&star_btn);
        action_bar.pack_start(&archive_btn);
        action_bar.pack_start(&delete_btn);
        action_bar.pack_end(&select_all_btn);

        let revealer = gtk::Revealer::builder()
            .transition_type(gtk::RevealerTransitionType::SlideDown)
            .transition_duration(180)
            .reveal_child(false)
            .child(&action_bar)
            .build();
        crate::collapsing_revealer::collapse_while_closed(&revealer);

        Self {
            revealer,
            count_label,
            read_btn,
            star_btn,
            delete_btn,
            select_all_btn,
            syncing,
            read_mixed,
            star_mixed,
        }
    }

    pub fn sync(
        &self,
        count: u32,
        total: u32,
        any_unseen: bool,
        any_seen: bool,
        any_unstarred: bool,
        any_starred: bool,
    ) {
        let label = ntr!("{n} selected", "{n} selected", count);
        self.count_label.set_label(&label);
        self.revealer.set_reveal_child(count > 1);

        let all_read = any_seen && !any_unseen;
        let read_mixed = any_seen && any_unseen;
        let all_starred = any_starred && !any_unstarred;
        let star_mixed = any_starred && any_unstarred;
        self.syncing.set(true);
        self.read_mixed.set(read_mixed);
        self.star_mixed.set(star_mixed);
        sync_tri_state_toggle(
            &self.read_btn,
            all_read,
            read_mixed,
            "mail-read-symbolic",
            "mail-unread-symbolic",
            &tr!("Mark as Unread"),
            &tr!("Mark as Read"),
            &tr!("Mixed read state — click to mark all read or unread"),
        );
        sync_tri_state_toggle(
            &self.star_btn,
            all_starred,
            star_mixed,
            "starred-symbolic",
            "non-starred-symbolic",
            &tr!("Unstar"),
            &tr!("Star"),
            &tr!("Mixed star state — click to star or unstar all"),
        );
        self.syncing.set(false);

        self.read_btn.set_sensitive(count > 0);
        self.star_btn.set_sensitive(count > 0);
        self.delete_btn.set_sensitive(count > 0);
        self.select_all_btn
            .set_sensitive(count < total && total > 0);
    }
}

/// Gmail-style overlay chip shown when new mail arrives while scrolled away.
pub struct NewMailChip {
    pub revealer: gtk::Revealer,
    pub(super) button: gtk::Button,
    ids: Rc<RefCell<HashSet<MessageId>>>,
    pub(super) newest_id: Rc<Cell<Option<MessageId>>>,
    suppress_edge_hide: Rc<Cell<u32>>,
}

impl NewMailChip {
    pub(super) fn new() -> Self {
        let button = gtk::Button::builder()
            .css_classes(["pill", "suggested-action", "alcedo-new-mail-chip"])
            .focus_on_click(true)
            .build();

        let revealer = gtk::Revealer::builder()
            .transition_type(gtk::RevealerTransitionType::Crossfade)
            .transition_duration(180)
            .reveal_child(false)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Start)
            .hexpand(false)
            .vexpand(false)
            .can_target(true)
            .margin_top(32)
            .margin_bottom(10)
            .child(&button)
            .build();

        Self {
            revealer,
            button,
            ids: Rc::new(RefCell::new(HashSet::new())),
            newest_id: Rc::new(Cell::new(None)),
            suppress_edge_hide: Rc::new(Cell::new(0)),
        }
    }

    pub fn bind_list(&self, list: &MessageListView, scrolled: &gtk::ScrolledWindow) {
        let view = list.view.clone();
        let selection_model = list.selection_model.clone();
        let ids = self.ids.clone();
        let newest_id = self.newest_id.clone();
        let revealer = self.revealer.clone();

        self.button.connect_clicked({
            let ids = ids.clone();
            let newest_id = newest_id.clone();
            let revealer = revealer.clone();
            let view = view.clone();
            let selection_model = selection_model.clone();
            move |_| {
                if let Some(id) = newest_id.get()
                    && let Some(model) = selection_model.model()
                {
                    reveal_new_mail_at(&model, &view, id);
                }
                ids.borrow_mut().clear();
                newest_id.set(None);
                revealer.set_reveal_child(false);
            }
        });

        let suppress_edge_hide = self.suppress_edge_hide.clone();
        scrolled.vadjustment().connect_value_changed({
            let ids = ids.clone();
            let newest_id = newest_id.clone();
            let revealer = revealer.clone();
            let selection_model = selection_model.clone();
            let suppress_edge_hide = suppress_edge_hide.clone();
            move |adj| {
                if suppress_edge_hide.get() > 0 {
                    return;
                }
                if ids.borrow().is_empty() {
                    return;
                }
                let Some(id) = newest_id.get() else {
                    return;
                };
                let Some(model) = selection_model.model() else {
                    return;
                };
                let Some(pos) = find_visible_position_in_model(&model, id) else {
                    return;
                };
                let n = model.n_items();
                if at_arrival_edge(adj, pos, n) {
                    ids.borrow_mut().clear();
                    newest_id.set(None);
                    revealer.set_reveal_child(false);
                }
            }
        });

        list.selection_model.connect_selection_changed({
            let ids = ids.clone();
            let newest_id = newest_id.clone();
            let revealer = revealer.clone();
            move |model, _, _| {
                if ids.borrow().is_empty() || list_selection_suppressed() {
                    return;
                }
                let tracked = ids.borrow().clone();
                let Some(list_model) = model.model() else {
                    return;
                };
                let selection = model.selection();
                for i in 0..selection.size() {
                    let pos = selection.nth(i as u32);
                    if let Some(id) = visible_message_id(&list_model, pos)
                        && tracked.contains(&id)
                    {
                        ids.borrow_mut().clear();
                        newest_id.set(None);
                        revealer.set_reveal_child(false);
                        return;
                    }
                }
            }
        });
    }

    pub fn show(&self, added: &HashSet<MessageId>, envelopes: &[Envelope], dock_end: bool) {
        {
            let mut ids = self.ids.borrow_mut();
            ids.extend(added.iter().copied());
        }
        let ids = self.ids.borrow();
        let count = ids.len() as u32;
        let label = new_mail_chip_label(count);
        self.button.set_label(&label);
        self.button.set_tooltip_text(Some(&label));
        self.button
            .update_property(&[gtk::accessible::Property::Label(label.as_str())]);
        if let Some(newest) = newest_added_id(envelopes, &ids) {
            self.newest_id.set(Some(newest));
        }
        if dock_end {
            self.revealer.set_valign(gtk::Align::End);
            self.revealer.set_margin_top(10);
            self.revealer.set_margin_bottom(10);
        } else {
            self.revealer.set_valign(gtk::Align::Start);
            self.revealer.set_margin_top(32);
            self.revealer.set_margin_bottom(10);
        }
        self.revealer.set_reveal_child(true);
    }

    pub fn hide(&self) {
        self.ids.borrow_mut().clear();
        self.newest_id.set(None);
        self.revealer.set_reveal_child(false);
    }

    pub fn remove_ids(&self, removed: &[MessageId], envelopes: &[Envelope]) {
        {
            let mut ids = self.ids.borrow_mut();
            for id in removed {
                ids.remove(id);
            }
        }
        if self.ids.borrow().is_empty() {
            self.hide();
            return;
        }
        let ids = self.ids.borrow();
        let count = ids.len() as u32;
        let label = new_mail_chip_label(count);
        self.button.set_label(&label);
        self.button.set_tooltip_text(Some(&label));
        self.button
            .update_property(&[gtk::accessible::Property::Label(label.as_str())]);
        self.newest_id.set(newest_added_id(envelopes, &ids));
    }

    pub(super) fn hold_edge_hide(&self) {
        self.suppress_edge_hide
            .set(self.suppress_edge_hide.get() + 1);
    }

    pub(super) fn release_edge_hide_after_idle(&self) {
        let flag = self.suppress_edge_hide.clone();
        glib::idle_add_local_once(move || {
            flag.set(flag.get().saturating_sub(1));
        });
    }

    pub fn visible_count(&self) -> u32 {
        self.ids.borrow().len() as u32
    }
}

/// Messages pane with search header and column list.
pub struct MessagesPane {
    pub root: gtk::Box,
    pub search_entry: gtk::SearchEntry,
    pub scrolled_window: gtk::ScrolledWindow,
    pub body_stack: gtk::Stack,
    pub empty_status: adw::StatusPage,
    pub selection_bar: SelectionBar,
    pub new_mail_chip: NewMailChip,
    pub search_filter_bar: SearchFilterBar,
    pub sync_btn: gtk::Button,
    /// Header toggle for the shown extension view, hidden otherwise.
    pub view_toggle: gtk::ToggleButton,
    pub header: adw::HeaderBar,
    pub folder_toggle: gtk::ToggleButton,
    pub compose_btn: gtk::Button,
    pub list_start: gtk::Box,
}

/// Build the messages navigation page (search bar + list).
pub fn build_messages_pane(
    list: &MessageListView,
    on_search: impl Fn(String) + 'static,
    on_toggle_folders: impl Fn(bool) + 'static,
    on_search_scope: Rc<dyn Fn(crate::search_bar::SearchScopeKind)>,
    on_query_rewrite: Rc<dyn Fn(String)>,
) -> MessagesPane {
    let list_view = &list.view;
    let search_entry = gtk::SearchEntry::builder()
            .placeholder_text(tr!("Search this folder"))
            .tooltip_text(
                tr!("Search this folder. Switch to All mail in the filter bar, or use operators: from: to: subject: folder: is: has:"),
            )
        .hexpand(true)
        .build();

    let folder_toggle = gtk::ToggleButton::builder().active(true).build();
    icons::set_button_icon(
        &folder_toggle,
        icons::FOLDER_PANE,
        icons::TOOLBAR_PIXEL_SIZE,
    );
    a11y::label_icon_control(&folder_toggle, &tr!("Hide Folders"));
    folder_toggle.connect_clicked(move |btn| {
        a11y::label_icon_control(
            btn,
            if btn.is_active() {
                trs!("Hide Folders")
            } else {
                trs!("Show Folders")
            },
        );
        on_toggle_folders(btn.is_active());
    });

    let compose_btn = gtk::Button::builder()
        .css_classes(["flat"])
        .action_name(crate::menu::ACTION_COMPOSE)
        .visible(false)
        .build();
    icons::set_button_icon(
        &compose_btn,
        icons::COMPOSE_SYMBOLIC,
        icons::TOOLBAR_PIXEL_SIZE,
    );
    a11y::name_icon_control(
        &compose_btn,
        &tr!("New Message"),
        &commands::command_tooltip(crate::menu::ACTION_COMPOSE, &tr!("New Message")),
    );

    let header = adw::HeaderBar::new();
    header.set_show_start_title_buttons(false);
    header.set_show_end_title_buttons(false);
    header.set_show_back_button(false);
    let sync_btn = action_icon_button(
        "view-refresh-symbolic",
        &commands::command_tooltip(crate::menu::ACTION_SYNC_FOLDER, &tr!("Sync Folder")),
        "win.mail-sync-folder",
    );
    a11y::set_label(&sync_btn, &tr!("Sync Folder"));
    sync_btn.add_css_class("alcedo-sync-btn");

    let view_toggle = gtk::ToggleButton::builder()
        .icon_name(icons::SERVER_SCAN)
        .css_classes(["flat", "alcedo-view-toggle"])
        .sensitive(false)
        .visible(false)
        .build();
    icons::set_button_icon(&view_toggle, icons::SERVER_SCAN, icons::TOOLBAR_PIXEL_SIZE);
    a11y::name_icon_control(
        &view_toggle,
        &tr!("Scan server"),
        &tr!("Search the server for more messages"),
    );

    let list_start = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    list_start.append(&folder_toggle);
    list_start.append(&compose_btn);
    list_start.append(&sync_btn);
    list_start.append(&view_toggle);
    header.pack_start(&list_start);
    header.set_title_widget(Some(&search_entry));

    list_view.set_show_row_separators(true);
    list_view.set_show_column_separators(false);
    list_view.set_single_click_activate(false);
    list_view.set_enable_rubberband(true);
    list_view.set_hexpand(true);
    list_view.set_halign(gtk::Align::Fill);
    list_view.add_css_class("alcedo-msg-list");

    let show_preview = show_message_preview();
    if !show_preview {
        list_view.add_css_class("alcedo-hide-preview");
    }
    apply_preview_visibility(list_view);

    on_show_message_preview_changed({
        let list_view = list_view.clone();
        move |show| {
            if show {
                list_view.remove_css_class("alcedo-hide-preview");
            } else {
                list_view.add_css_class("alcedo-hide-preview");
            }
            apply_preview_visibility(&list_view);
        }
    });

    let view_options = build_view_options_menu();
    header.pack_end(&help::help_button(TOPIC_SEARCH));
    header.pack_end(&view_options.button);

    let scrolled = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .hexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .child(list_view)
        .build();
    scrolled.add_css_class("alcedo-msg-list-scroll");

    let new_mail_chip = NewMailChip::new();
    new_mail_chip.bind_list(list, &scrolled);

    let list_overlay = gtk::Overlay::builder()
        .vexpand(true)
        .hexpand(true)
        .child(&scrolled)
        .build();
    list_overlay.add_overlay(&new_mail_chip.revealer);

    let empty_page = adw::StatusPage::builder()
        .icon_name(empty_list_icon(false, false, ListEmptyView::Default))
        .title(empty_list_copy(false, false, false, ListEmptyView::Default).0)
        .build();

    let content_stack = gtk::Stack::new();
    content_stack.set_vexpand(true);
    content_stack.add_named(&list_overlay, Some("list"));
    content_stack.add_named(&empty_page, Some("empty"));
    wire_empty_state(&content_stack, list_view);

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    let search_filter_bar = SearchFilterBar::new(on_search_scope, on_query_rewrite);
    toolbar.add_top_bar(&search_filter_bar.revealer);
    let selection_bar = SelectionBar::new();
    toolbar.add_top_bar(&selection_bar.revealer);
    toolbar.set_content(Some(&content_stack));
    toolbar.set_vexpand(true);

    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.set_vexpand(true);
    root.append(&toolbar);

    install_column_picker(
        list,
        root.upcast_ref(),
        &view_options.popover,
        &view_options.columns_box,
    );

    wire_debounced_search(&search_entry, on_search);

    MessagesPane {
        root,
        search_entry,
        scrolled_window: scrolled,
        body_stack: content_stack,
        empty_status: empty_page,
        selection_bar,
        new_mail_chip,
        search_filter_bar,
        sync_btn,
        view_toggle,
        header,
        folder_toggle,
        compose_btn,
        list_start,
    }
}

pub(super) struct ViewOptionsMenu {
    button: gtk::MenuButton,
    popover: gtk::Popover,
    columns_box: gtk::Box,
}

pub(super) fn build_view_options_menu() -> ViewOptionsMenu {
    let button = gtk::MenuButton::builder()
        .icon_name("view-more-symbolic")
        .build();
    a11y::label_icon_control(&button, &tr!("View Options"));

    let pane = reading_pane();
    let right_pane = gtk::CheckButton::builder()
        .label(tr!("Reading Pane on the Right"))
        .active(pane == ReadingPane::Right)
        .build();
    let bottom_pane = gtk::CheckButton::builder()
        .label(tr!("Reading Pane Below"))
        .active(pane == ReadingPane::Bottom)
        .build();
    bottom_pane.set_group(Some(&right_pane));
    right_pane.connect_active_notify(|btn| {
        if btn.is_active() {
            set_reading_pane(ReadingPane::Right);
        }
    });
    bottom_pane.connect_active_notify(|btn| {
        if btn.is_active() {
            set_reading_pane(ReadingPane::Bottom);
        }
    });
    on_reading_pane_changed({
        let right_pane = right_pane.clone();
        let bottom_pane = bottom_pane.clone();
        move |pane| {
            right_pane.set_active(pane == ReadingPane::Right);
            bottom_pane.set_active(pane == ReadingPane::Bottom);
        }
    });

    let preview_check = gtk::CheckButton::builder()
        .label(tr!("Show Message Previews"))
        .build();
    bind_show_message_preview(&preview_check);

    let content = gtk::Box::new(gtk::Orientation::Vertical, 6);
    content.set_margin_top(12);
    content.set_margin_bottom(12);
    content.set_margin_start(12);
    content.set_margin_end(12);
    content.append(&view_options_heading(&tr!("Reading Pane")));
    content.append(&right_pane);
    content.append(&bottom_pane);
    content.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    content.append(&preview_check);
    content.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    content.append(&view_options_heading(&tr!("Columns")));
    let columns_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
    content.append(&columns_box);

    let popover = gtk::Popover::new();
    popover.set_child(Some(&content));
    button.set_popover(Some(&popover));
    ViewOptionsMenu {
        button,
        popover,
        columns_box,
    }
}

pub(super) fn view_options_heading(text: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .xalign(0.0)
        .css_classes(["heading"])
        .build()
}

pub(super) fn wire_debounced_search<F: Fn(String) + 'static>(
    entry: &gtk::SearchEntry,
    on_search: F,
) {
    let debounce_source = Rc::new(Cell::new(None::<glib::SourceId>));
    let on_search = Rc::new(on_search);

    entry.connect_search_changed({
        let entry = entry.clone();
        let debounce_source = debounce_source.clone();
        let on_search = on_search.clone();
        move |_| {
            crate::glib_source::cancel_source(&debounce_source);
            let query = entry.text().to_string();
            let on_search = on_search.clone();
            let debounce_source_cb = debounce_source.clone();
            let id = glib::timeout_add_local(Duration::from_millis(300), move || {
                let _ = debounce_source_cb.take();
                on_search(query.clone());
                glib::ControlFlow::Break
            });
            debounce_source.set(Some(id));
        }
    });

    entry.connect_activate({
        let entry = entry.clone();
        let debounce_source = debounce_source.clone();
        let on_search = on_search.clone();
        move |_| {
            crate::glib_source::cancel_source(&debounce_source);
            on_search(entry.text().to_string());
        }
    });
}
