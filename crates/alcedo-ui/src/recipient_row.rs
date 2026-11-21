//! Compose To/Cc/Bcc recipient chips with autocomplete.

use alcedo_i18n::{n_, ntr, tr, trf, trm, trs};
use std::cell::{Cell, RefCell};
use std::cmp::Ordering;
use std::collections::HashSet;
use std::rc::Rc;

use alcedo_core::{
    Address, Contact, ContactGroup, ContactStatus, GroupId, collect_outgoing_into,
    parse_address_token,
};
use relm4::adw;
use relm4::adw::prelude::*;
use relm4::gtk;
use relm4::gtk::glib;

use crate::address_popover::{attach_address_copy_context_menu, show_recipient_chip_popover};
use crate::compose_attachments::show_composer_toast;
use crate::recipient_picker::{present_recipient_picker, sent_person_matches};
use crate::search_autocomplete::highlight_query;

const MAX_AUTOCOMPLETE: usize = 8;
const RECIPIENT_HINT: &str = n_!("Name or email");
/// Long enough to read a name or local-part; wrap instead of shrinking further.
const CHIP_MAX_CHARS: i32 = 20;

#[derive(Clone, Debug, PartialEq)]
pub struct ContactBook {
    pub contacts: Vec<Contact>,
    pub groups: Vec<ContactGroup>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecipientToken {
    Person { email: String, name: Option<String> },
    Group { id: GroupId, name: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum AutocompleteItem {
    Group {
        id: GroupId,
        name: String,
        members: usize,
    },
    Person {
        email: String,
        title: String,
    },
}

impl ContactBook {
    pub fn collect_outgoing(
        &mut self,
        account_email: &str,
        addresses: &[Address],
        now: chrono::DateTime<chrono::Utc>,
    ) {
        collect_outgoing_into(
            &mut self.contacts,
            &mut self.groups,
            account_email,
            addresses,
            now,
        );
    }
}

#[derive(Clone)]
pub struct RecipientField {
    tokens: Rc<RefCell<Vec<RecipientToken>>>,
    book: Rc<RefCell<ContactBook>>,
    sent: Rc<RefCell<Vec<Address>>>,
    field_title: String,
    chip_box: adw::WrapBox,
    entry: gtk::Entry,
    popover: gtk::Popover,
    listbox: gtk::ListBox,
    toast_host: gtk::Widget,
    picker_btn: gtk::Button,
    widget: gtk::Box,
    ignore_focus_leave: Rc<Cell<bool>>,
    on_changed: Rc<RefCell<Option<Rc<dyn Fn()>>>>,
    on_address_action: Rc<RefCell<Rc<dyn Fn(usize, String)>>>,
}

#[derive(Clone)]
struct WeakRecipientField {
    tokens: Rc<RefCell<Vec<RecipientToken>>>,
    book: Rc<RefCell<ContactBook>>,
    sent: Rc<RefCell<Vec<Address>>>,
    field_title: String,
    chip_box: glib::WeakRef<adw::WrapBox>,
    entry: glib::WeakRef<gtk::Entry>,
    popover: glib::WeakRef<gtk::Popover>,
    listbox: glib::WeakRef<gtk::ListBox>,
    toast_host: glib::WeakRef<gtk::Widget>,
    picker_btn: glib::WeakRef<gtk::Button>,
    widget: glib::WeakRef<gtk::Box>,
    ignore_focus_leave: Rc<Cell<bool>>,
    on_changed: Rc<RefCell<Option<Rc<dyn Fn()>>>>,
    on_address_action: Rc<RefCell<Rc<dyn Fn(usize, String)>>>,
}

impl WeakRecipientField {
    fn upgrade(&self) -> Option<RecipientField> {
        Some(RecipientField {
            tokens: self.tokens.clone(),
            book: self.book.clone(),
            sent: self.sent.clone(),
            field_title: self.field_title.clone(),
            chip_box: self.chip_box.upgrade()?,
            entry: self.entry.upgrade()?,
            popover: self.popover.upgrade()?,
            listbox: self.listbox.upgrade()?,
            toast_host: self.toast_host.upgrade()?,
            picker_btn: self.picker_btn.upgrade()?,
            widget: self.widget.upgrade()?,
            ignore_focus_leave: self.ignore_focus_leave.clone(),
            on_changed: self.on_changed.clone(),
            on_address_action: self.on_address_action.clone(),
        })
    }
}

impl RecipientField {
    pub fn new(
        title: &str,
        book: Rc<RefCell<ContactBook>>,
        sent: Rc<RefCell<Vec<Address>>>,
        toast_host: gtk::Widget,
    ) -> Self {
        let tokens = Rc::new(RefCell::new(Vec::new()));
        let chip_box = adw::WrapBox::builder()
            .hexpand(true)
            .halign(gtk::Align::Fill)
            .child_spacing(6)
            .line_spacing(4)
            .line_homogeneous(false)
            .justify(adw::JustifyMode::None)
            .wrap_policy(adw::WrapPolicy::Natural)
            .vexpand(false)
            .valign(gtk::Align::Start)
            .css_classes(["alcedo-recipient-row"])
            .build();

        let entry = gtk::Entry::builder()
            .hexpand(true)
            .has_frame(false)
            .valign(gtk::Align::Center)
            .width_chars(8)
            .placeholder_text(trm!(RECIPIENT_HINT))
            .css_classes(["alcedo-recipient-entry"])
            .build();

        let listbox = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::Single)
            .can_focus(true)
            .css_classes(["alcedo-autocomplete"])
            .build();
        // Autohide takes a seat grab and steals keys from the entry after the
        // first matching character. Keep focus in the entry while typing; Tab
        // moves focus into the list. Hide on blur/Esc.
        let popover = gtk::Popover::builder()
            .autohide(false)
            .can_focus(false)
            .has_arrow(false)
            .css_classes(["alcedo-autocomplete-popover"])
            .child(&listbox)
            .build();
        popover.set_parent(&entry);
        alcedo_widgets::unparent_popover_on_destroy(&entry, &popover);

        chip_box.append(&entry);

        let widget = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(6)
            .hexpand(true)
            .vexpand(false)
            .valign(gtk::Align::Start)
            .css_classes(["alcedo-recipient-field"])
            .build();

        let title_label = gtk::Label::builder()
            .label(title)
            .xalign(0.0)
            .valign(gtk::Align::Start)
            .css_classes(["alcedo-recipient-label"])
            .build();
        title_label.set_width_chars(7);

        let picker_btn = gtk::Button::builder()
            .icon_name("avatar-default-symbolic")
            .tooltip_text(tr!("Choose recipients"))
            .valign(gtk::Align::Start)
            .css_classes(["flat", "alcedo-recipient-picker-btn"])
            // Mouse-only, like the compose toolbar buttons: as a Tab stop it sat
            // between the address and Subject on every message. The entry's
            // autocomplete is the keyboard route to the same contacts.
            .can_focus(false)
            .focus_on_click(false)
            .build();

        widget.append(&title_label);
        widget.append(&chip_box);
        widget.append(&picker_btn);

        let field = Self {
            tokens: tokens.clone(),
            book: book.clone(),
            sent: sent.clone(),
            field_title: title.to_string(),
            chip_box: chip_box.clone(),
            entry: entry.clone(),
            popover: popover.clone(),
            listbox: listbox.clone(),
            toast_host: toast_host.clone(),
            picker_btn: picker_btn.clone(),
            widget,
            ignore_focus_leave: Rc::new(Cell::new(false)),
            on_changed: Rc::new(RefCell::new(None)),
            on_address_action: Rc::new(RefCell::new(Rc::new(|_, _| {}))),
        };

        picker_btn.connect_clicked({
            let field_ref = field.downgrade();
            move |_| {
                let Some(field_ref) = field_ref.upgrade() else {
                    return;
                };
                let field_for_add = field_ref.clone_refs();
                let book = field_ref.book.borrow();
                let sent = field_ref.sent.borrow();
                let already = field_ref.tokens.borrow().clone();
                present_recipient_picker(
                    field_ref.widget(),
                    &field_ref.field_title,
                    &book,
                    &sent,
                    &already,
                    move |tokens| {
                        field_for_add.add_tokens(&tokens);
                    },
                );
            }
        });

        let refresh = field.build_refresh();
        let commit = field.build_commit(refresh.clone());

        entry.connect_changed({
            let field_ref = field.downgrade();
            move |entry| {
                if let Some(field_ref) = field_ref.upgrade() {
                    field_ref.update_autocomplete(entry.text().as_ref());
                }
            }
        });
        entry.connect_activate({
            let commit = commit.clone();
            move |_| commit()
        });

        let key_controller = gtk::EventControllerKey::new();
        key_controller.set_propagation_phase(gtk::PropagationPhase::Capture);
        let commit_for_keys = commit.clone();
        let tokens_for_keys = tokens.clone();
        let refresh_for_keys = refresh.clone();
        let entry_for_keys = entry.downgrade();
        let popover_for_keys = popover.downgrade();
        let listbox_for_keys = listbox.downgrade();
        let ignore_focus_leave_for_keys = field.ignore_focus_leave.clone();
        key_controller.connect_key_pressed(move |_, key, _, modifiers| {
            let (Some(entry_for_keys), Some(popover_for_keys), Some(listbox_for_keys)) = (
                entry_for_keys.upgrade(),
                popover_for_keys.upgrade(),
                listbox_for_keys.upgrade(),
            ) else {
                return glib::Propagation::Proceed;
            };
            if key == gtk::gdk::Key::Escape && popover_for_keys.is_visible() {
                popover_for_keys.popdown();
                return glib::Propagation::Stop;
            }
            if key == gtk::gdk::Key::BackSpace && entry_for_keys.text().is_empty() {
                let popped = {
                    let mut list = tokens_for_keys.borrow_mut();
                    list.pop().is_some()
                };
                if popped {
                    refresh_for_keys();
                }
                return glib::Propagation::Stop;
            }
            if matches!(key, gtk::gdk::Key::comma | gtk::gdk::Key::semicolon) {
                commit_for_keys();
                return glib::Propagation::Stop;
            }
            if key == gtk::gdk::Key::Tab {
                let shift = modifiers.contains(gtk::gdk::ModifierType::SHIFT_MASK);
                if popover_for_keys.is_visible() && !shift {
                    focus_autocomplete_list(&listbox_for_keys, &ignore_focus_leave_for_keys);
                    return glib::Propagation::Stop;
                }
                if !shift {
                    commit_for_keys();
                } else if popover_for_keys.is_visible() {
                    popover_for_keys.popdown();
                }
                return glib::Propagation::Proceed;
            }
            if popover_for_keys.is_visible()
                && matches!(
                    key,
                    gtk::gdk::Key::Up
                        | gtk::gdk::Key::Down
                        | gtk::gdk::Key::KP_Up
                        | gtk::gdk::Key::KP_Down
                )
            {
                navigate_autocomplete_list(&listbox_for_keys, key);
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        entry.add_controller(key_controller);

        let focus = gtk::EventControllerFocus::new();
        focus.connect_leave({
            let commit = commit.clone();
            let listbox = listbox.downgrade();
            let ignore_focus_leave = field.ignore_focus_leave.clone();
            move |_| {
                if ignore_focus_leave.get() {
                    return;
                }
                let commit = commit.clone();
                let listbox = listbox.clone();
                glib::idle_add_local(move || {
                    let Some(listbox) = listbox.upgrade() else {
                        return glib::ControlFlow::Break;
                    };
                    if listbox.has_focus() {
                        return glib::ControlFlow::Break;
                    }
                    commit();
                    glib::ControlFlow::Break
                });
            }
        });
        entry.add_controller(focus);

        let listbox_focus = gtk::EventControllerFocus::new();
        listbox_focus.connect_leave({
            let popover = popover.downgrade();
            let listbox = listbox.downgrade();
            let entry = entry.downgrade();
            move |_| {
                let popover = popover.clone();
                let listbox = listbox.clone();
                let entry = entry.clone();
                glib::idle_add_local(move || {
                    let (Some(popover), Some(listbox), Some(entry)) =
                        (popover.upgrade(), listbox.upgrade(), entry.upgrade())
                    else {
                        return glib::ControlFlow::Break;
                    };
                    if entry.has_focus() || listbox.has_focus() {
                        return glib::ControlFlow::Break;
                    }
                    if popover.is_visible() {
                        popover.popdown();
                    }
                    glib::ControlFlow::Break
                });
            }
        });
        listbox.add_controller(listbox_focus);

        let listbox_key = gtk::EventControllerKey::new();
        listbox_key.connect_key_pressed({
            let field_ref = field.downgrade();
            let refresh = refresh.clone();
            move |_, key, _, modifiers| {
                let Some(field_ref) = field_ref.upgrade() else {
                    return glib::Propagation::Proceed;
                };
                let (popover, entry) = (&field_ref.popover, &field_ref.entry);
                let shift = modifiers.contains(gtk::gdk::ModifierType::SHIFT_MASK);
                match key {
                    gtk::gdk::Key::Escape => {
                        popover.popdown();
                        entry.grab_focus();
                        glib::Propagation::Stop
                    }
                    gtk::gdk::Key::Return | gtk::gdk::Key::KP_Enter => {
                        field_ref.try_activate_autocomplete_selection(&refresh, true);
                        glib::Propagation::Stop
                    }
                    gtk::gdk::Key::Tab if shift => {
                        entry.grab_focus();
                        glib::Propagation::Stop
                    }
                    gtk::gdk::Key::Tab => {
                        // The popover closes on accept, taking the focus the default
                        // Tab handling would start from. Hand focus back to the
                        // entry and advance from there so one Tab moves on.
                        if field_ref.try_activate_autocomplete_selection(&refresh, true) {
                            entry.emit_move_focus(gtk::DirectionType::TabForward);
                            return glib::Propagation::Stop;
                        }
                        glib::Propagation::Proceed
                    }
                    _ => glib::Propagation::Proceed,
                }
            }
        });
        listbox.add_controller(listbox_key);

        listbox.connect_row_activated({
            let field_ref = field.downgrade();
            let refresh = refresh.clone();
            move |_, row| {
                let index = row.index();
                if let Some(field_ref) = field_ref.upgrade()
                    && index >= 0
                {
                    field_ref.activate_autocomplete_index(index as usize, &refresh, true);
                }
            }
        });

        refresh();
        field
    }

    pub fn set_on_changed(&self, callback: impl Fn() + 'static) {
        *self.on_changed.borrow_mut() = Some(Rc::new(callback));
    }

    pub fn widget(&self) -> &gtk::Widget {
        self.widget.upcast_ref()
    }

    pub fn set_on_address_action(&self, callback: Rc<dyn Fn(usize, String)>) {
        *self.on_address_action.borrow_mut() = callback;
    }

    pub fn focus_entry(&self) {
        self.entry.grab_focus();
    }

    pub fn has_typed_or_chips(&self) -> bool {
        !self.tokens.borrow().is_empty() || !self.entry.text().trim().is_empty()
    }

    pub fn strings(&self) -> Vec<String> {
        let _ = self.commit_pending();
        let book = self.book.borrow();
        tokens_to_strings(&self.tokens.borrow(), &book)
    }

    fn commit_pending(&self) -> Result<(), &'static str> {
        let trimmed = self
            .entry
            .text()
            .trim()
            .trim_end_matches([',', ';'])
            .trim()
            .to_string();
        if trimmed.is_empty() {
            return Ok(());
        }
        let result = {
            let book = self.book.borrow();
            commit_text(&trimmed, &mut self.tokens.borrow_mut(), &book)
        };
        match result {
            Ok(()) => {
                self.entry.set_text("");
                self.popover.popdown();
                self.rebuild_chips();
                Ok(())
            }
            Err(message) => {
                show_composer_toast(&self.toast_host, message);
                Err(message)
            }
        }
    }

    pub fn set_from_strings(&self, strings: &[String]) {
        {
            let book = self.book.borrow();
            *self.tokens.borrow_mut() = strings_to_tokens(strings, &book);
        }
        self.entry.set_text("");
        self.rebuild_chips();
    }

    pub fn clear(&self) {
        self.tokens.borrow_mut().clear();
        self.entry.set_text("");
        self.rebuild_chips();
    }

    pub fn add_tokens(&self, extra: &[RecipientToken]) {
        let book = self.book.borrow();
        let mut tokens = self.tokens.borrow_mut();
        for token in extra {
            match token {
                RecipientToken::Person { email, name } => {
                    insert_person_token(&mut tokens, email, name.as_deref());
                }
                RecipientToken::Group { id, name } => {
                    insert_group_token(&mut tokens, *id, name);
                }
            }
        }
        drop(tokens);
        drop(book);
        self.rebuild_chips();
        self.focus_entry();
    }

    fn rebuild_chips(&self) {
        self.show_chips();
        self.notify_changed();
    }

    /// Replace the chips in front of the entry. The entry stays parented: detaching
    /// and re-adding it drops keyboard focus, so the next Tab restarted from the
    /// chips (name, remove button, entry) instead of moving on to the next field.
    fn show_chips(&self) {
        let entry: &gtk::Widget = self.entry.upcast_ref();
        let mut child = self.chip_box.first_child();
        while let Some(widget) = child {
            child = widget.next_sibling();
            if &widget != entry {
                self.chip_box.remove(&widget);
            }
        }
        let list = self.tokens.borrow().clone();
        let book = self.book.borrow();
        let mut previous: Option<gtk::Widget> = None;
        for token in list.iter() {
            let chip = build_chip(
                token,
                &book,
                self.tokens.clone(),
                self.toast_host.clone(),
                self.downgrade(),
            );
            self.chip_box.insert_child_after(&chip, previous.as_ref());
            previous = Some(chip.upcast());
        }
        self.sync_entry_hint();
    }

    fn sync_entry_hint(&self) {
        if self.tokens.borrow().is_empty() {
            self.entry.set_placeholder_text(Some(trm!(RECIPIENT_HINT)));
        } else {
            self.entry.set_placeholder_text(None);
        }
    }

    fn build_refresh(&self) -> Rc<dyn Fn()> {
        let weak_field = self.downgrade();
        Rc::new(move || {
            let Some(field) = weak_field.upgrade() else {
                return;
            };
            field.show_chips();
        })
    }

    fn build_commit(&self, refresh: Rc<dyn Fn()>) -> Rc<dyn Fn()> {
        let weak_field = self.downgrade();
        Rc::new(move || {
            let Some(field) = weak_field.upgrade() else {
                return;
            };
            let (tokens, book, entry, popover, toast_host) = (
                &field.tokens,
                &field.book,
                &field.entry,
                &field.popover,
                &field.toast_host,
            );
            if field.try_activate_autocomplete_selection(&refresh, true) {
                return;
            }

            let trimmed = entry
                .text()
                .trim()
                .trim_end_matches([',', ';'])
                .trim()
                .to_string();
            if trimmed.is_empty() {
                popover.popdown();
                return;
            }

            let result = {
                let book_ref = book.borrow();
                commit_text(&trimmed, &mut tokens.borrow_mut(), &book_ref)
            };
            match result {
                Ok(()) => {
                    entry.set_text("");
                    popover.popdown();
                    refresh();
                }
                Err(message) => show_composer_toast(toast_host, message),
            }
        })
    }

    fn clone_refs(&self) -> Self {
        Self {
            tokens: self.tokens.clone(),
            book: self.book.clone(),
            sent: self.sent.clone(),
            field_title: self.field_title.clone(),
            chip_box: self.chip_box.clone(),
            entry: self.entry.clone(),
            popover: self.popover.clone(),
            listbox: self.listbox.clone(),
            toast_host: self.toast_host.clone(),
            picker_btn: self.picker_btn.clone(),
            widget: self.widget.clone(),
            ignore_focus_leave: self.ignore_focus_leave.clone(),
            on_changed: self.on_changed.clone(),
            on_address_action: self.on_address_action.clone(),
        }
    }

    /// Weak handle for callbacks owned by this field's widgets (entry,
    /// suggestion list, chips): a strong field there is a cycle that keeps a
    /// closed composer alive.
    fn downgrade(&self) -> WeakRecipientField {
        WeakRecipientField {
            tokens: self.tokens.clone(),
            book: self.book.clone(),
            sent: self.sent.clone(),
            field_title: self.field_title.clone(),
            chip_box: self.chip_box.downgrade(),
            entry: self.entry.downgrade(),
            popover: self.popover.downgrade(),
            listbox: self.listbox.downgrade(),
            toast_host: self.toast_host.downgrade(),
            picker_btn: self.picker_btn.downgrade(),
            widget: self.widget.downgrade(),
            ignore_focus_leave: self.ignore_focus_leave.clone(),
            on_changed: self.on_changed.clone(),
            on_address_action: self.on_address_action.clone(),
        }
    }

    fn notify_changed(&self) {
        if let Some(callback) = self.on_changed.borrow().as_ref() {
            callback();
        }
    }

    fn update_autocomplete(&self, query: &str) {
        let items = autocomplete_items(query.trim(), &self.book.borrow(), &self.sent.borrow());
        while let Some(row) = self.listbox.row_at_index(0) {
            self.listbox.remove(&row);
        }
        if items.is_empty() {
            self.popover.popdown();
            return;
        }
        let query = query.trim();
        for item in items.iter() {
            let row = gtk::ListBoxRow::new();
            row.set_can_focus(false);
            let box_ = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            box_.add_css_class("alcedo-autocomplete-row");
            box_.set_valign(gtk::Align::Center);

            let icon_name = match item {
                AutocompleteItem::Group { .. } => "system-users-symbolic",
                AutocompleteItem::Person { .. } => "x-office-address-book-symbolic",
            };
            let icon = gtk::Image::from_icon_name(icon_name);
            icon.add_css_class("alcedo-autocomplete-icon");
            icon.set_valign(gtk::Align::Center);
            box_.append(&icon);

            let label = gtk::Label::builder()
                .use_markup(true)
                .label(highlight_query(&autocomplete_plain_label(item), query))
                .xalign(0.0)
                .hexpand(true)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .css_classes(["alcedo-autocomplete-label"])
                .build();
            if let AutocompleteItem::Group { members, .. } = item {
                let tooltip = ntr!("{n} member", "{n} members", *members);
                label.set_tooltip_text(Some(&tooltip));
            }
            box_.append(&label);
            row.set_child(Some(&box_));
            self.listbox.append(&row);
        }
        self.listbox.unselect_all();
        self.ignore_focus_leave.set(true);
        self.popover.popup();
        self.entry.grab_focus_without_selecting();
        self.ignore_focus_leave.set(false);
    }

    fn activate_autocomplete_index(
        &self,
        index: usize,
        refresh: &Rc<dyn Fn()>,
        refocus_entry: bool,
    ) {
        let query = self.entry.text();
        let items = autocomplete_items(query.trim(), &self.book.borrow(), &self.sent.borrow());
        let Some(item) = items.get(index) else {
            return;
        };
        match item {
            AutocompleteItem::Group { id, name, .. } => {
                insert_group_token(&mut self.tokens.borrow_mut(), *id, name);
            }
            AutocompleteItem::Person { email, title } => {
                insert_person_token(
                    &mut self.tokens.borrow_mut(),
                    email,
                    if title == email {
                        None
                    } else {
                        Some(title.as_str())
                    },
                );
            }
        }
        self.entry.set_text("");
        self.popover.popdown();
        refresh();
        if refocus_entry {
            self.entry.grab_focus_without_selecting();
        }
    }

    fn try_activate_autocomplete_selection(
        &self,
        refresh: &Rc<dyn Fn()>,
        refocus_entry: bool,
    ) -> bool {
        if !self.popover.is_visible() {
            return false;
        }
        // Only the highlighted row. Up from the first item unselects so Enter/comma
        // can commit the typed address instead of the first suggestion.
        let Some(row) = self.listbox.selected_row() else {
            return false;
        };
        let index = row.index();
        if index < 0 {
            return false;
        }
        self.activate_autocomplete_index(index as usize, refresh, refocus_entry);
        true
    }
}

fn focus_autocomplete_list(listbox: &gtk::ListBox, ignore_focus_leave: &Rc<Cell<bool>>) {
    if let Some(row) = listbox.row_at_index(0) {
        listbox.select_row(Some(&row));
    } else {
        listbox.unselect_all();
    }
    ignore_focus_leave.set(true);
    listbox.grab_focus();
    let ignore_focus_leave = ignore_focus_leave.clone();
    glib::idle_add_local(move || {
        ignore_focus_leave.set(false);
        glib::ControlFlow::Break
    });
}

fn navigate_autocomplete_list(listbox: &gtk::ListBox, key: gtk::gdk::Key) {
    let count = listbox_row_count(listbox);
    if count == 0 {
        return;
    }
    let current = listbox.selected_row().map(|row| row.index()).unwrap_or(-1);
    let Some(next) = next_autocomplete_index(current, count, key) else {
        listbox.unselect_all();
        return;
    };
    if let Some(row) = listbox.row_at_index(next) {
        listbox.select_row(Some(&row));
    }
}

fn listbox_row_count(listbox: &gtk::ListBox) -> i32 {
    let mut count = 0;
    while listbox.row_at_index(count).is_some() {
        count += 1;
    }
    count
}

fn next_autocomplete_index(current: i32, count: i32, key: gtk::gdk::Key) -> Option<i32> {
    if count == 0 {
        return None;
    }
    match key {
        gtk::gdk::Key::Down | gtk::gdk::Key::KP_Down => {
            if current < 0 {
                Some(0)
            } else {
                Some((current + 1).min(count - 1))
            }
        }
        gtk::gdk::Key::Up | gtk::gdk::Key::KP_Up => {
            if current <= 0 {
                None
            } else {
                Some(current - 1)
            }
        }
        _ => None,
    }
}

fn build_chip(
    token: &RecipientToken,
    book: &ContactBook,
    tokens: Rc<RefCell<Vec<RecipientToken>>>,
    toast_host: gtk::Widget,
    field: WeakRecipientField,
) -> gtk::Box {
    let (label, tooltip, is_group, person) = match token {
        RecipientToken::Person { email, name } => {
            let addr = Address {
                email: email.clone(),
                name: name.clone(),
            };
            (
                addr.display_name(),
                Some(addr.formatted()),
                false,
                Some(addr),
            )
        }
        RecipientToken::Group { id, name } => {
            let members = book
                .groups
                .iter()
                .find(|g| g.id == *id)
                .map(|g| g.member_ids.len())
                .unwrap_or(0);
            let tooltip = if members == 1 {
                trf!("{name} — 1 member — click to expand", name = name)
            } else {
                ntr!(
                    "{name} — {n} member — click to expand",
                    "{name} — {n} members — click to expand",
                    members,
                    name = name
                )
            };
            (name.clone(), Some(tooltip), true, None)
        }
    };

    let chip = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(0)
        .hexpand(false)
        .halign(gtk::Align::Start)
        .valign(gtk::Align::Center)
        .css_classes(["alcedo-recipient-chip"])
        .build();
    if let Some(tooltip) = tooltip {
        chip.set_tooltip_text(Some(&tooltip));
    }
    if is_group {
        chip.add_css_class("alcedo-recipient-chip-group");
    } else {
        chip.add_css_class("alcedo-recipient-chip-person");
    }
    chip.set_cursor_from_name(Some("pointer"));

    // The name is a real (flat) button so Tab reaches it and Enter/Space
    // open the contact card or expand the group, same as a click.
    let name_btn = gtk::Button::builder()
        .has_frame(false)
        .valign(gtk::Align::Center)
        .css_classes(["flat", "alcedo-recipient-chip-name"])
        .build();
    crate::a11y::set_label(
        &name_btn,
        &if is_group {
            trf!("{label}, group, press Enter to expand", label = label)
        } else {
            trf!("{label}, recipient", label = label)
        },
    );

    let name_label = gtk::Label::builder()
        .label(&label)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .max_width_chars(CHIP_MAX_CHARS)
        .single_line_mode(true)
        .hexpand(false)
        .valign(gtk::Align::Center)
        .css_classes(["alcedo-recipient-chip-label"])
        .build();

    if let RecipientToken::Group { id, .. } = token {
        let group_id = *id;
        let tokens = tokens.clone();
        let field = field.clone();
        name_btn.connect_clicked(move |_| {
            let Some(field) = field.upgrade() else {
                return;
            };
            {
                let book = field.book.borrow();
                expand_group_chip(group_id, &book, &mut tokens.borrow_mut(), &field.toast_host);
            }
            glib::idle_add_local_once(move || field.rebuild_chips());
        });
    } else if let Some(addr) = person {
        let anchor = name_label.clone();
        let addr_for_popover = addr.clone();
        let addr_for_menu = addr.clone();
        let on_address_action = field.on_address_action.borrow().clone();
        name_btn.connect_clicked(move |_| {
            show_recipient_chip_popover(
                &anchor,
                addr_for_popover.clone(),
                on_address_action.clone(),
            );
        });
        attach_address_copy_context_menu(&chip, addr_for_menu, &toast_host);
    }

    let close_btn = gtk::Button::builder()
        .icon_name("window-close-symbolic")
        .has_frame(false)
        .valign(gtk::Align::Center)
        .css_classes(["flat", "circular", "alcedo-recipient-chip-remove"])
        .build();
    crate::a11y::label_icon_control(&close_btn, &trf!("Remove {recipient}", recipient = label));
    let remove_chip: Rc<dyn Fn()> = {
        let tokens = tokens.clone();
        let field = field.clone();
        let remove_token = token.clone();
        Rc::new(move || {
            {
                let mut list = tokens.borrow_mut();
                remove_matching_token(&mut list, &remove_token);
            }
            let Some(field) = field.upgrade() else {
                return;
            };
            glib::idle_add_local_once(move || {
                field.rebuild_chips();
                field.entry.grab_focus();
            });
        })
    };
    {
        let remove_chip = remove_chip.clone();
        close_btn.connect_clicked(move |_| remove_chip());
    }

    let keys = gtk::EventControllerKey::new();
    {
        // Read the chip from the controller; capturing it would be a cycle.
        keys.connect_key_pressed(move |keys, key, _, modifiers| {
            let Some(chip) = keys.widget() else {
                return glib::Propagation::Proceed;
            };
            if !modifiers.is_empty() {
                return glib::Propagation::Proceed;
            }
            match chip_key_action(key) {
                ChipKeyAction::Remove => remove_chip(),
                ChipKeyAction::FocusPrevious => {
                    if let Some(prev) = chip.prev_sibling() {
                        prev.child_focus(gtk::DirectionType::TabForward);
                    }
                }
                ChipKeyAction::FocusNext => {
                    if let Some(next) = chip.next_sibling() {
                        next.child_focus(gtk::DirectionType::TabForward);
                    }
                }
                ChipKeyAction::None => return glib::Propagation::Proceed,
            }
            glib::Propagation::Stop
        });
    }
    chip.add_controller(keys);

    name_btn.set_child(Some(&name_label));
    chip.append(&name_btn);
    chip.append(&close_btn);
    chip
}

/// What a key press on a focused recipient chip does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ChipKeyAction {
    Remove,
    FocusPrevious,
    FocusNext,
    None,
}

fn chip_key_action(key: gtk::gdk::Key) -> ChipKeyAction {
    use gtk::gdk::Key;
    match key {
        Key::Delete | Key::KP_Delete | Key::BackSpace => ChipKeyAction::Remove,
        Key::Left | Key::KP_Left => ChipKeyAction::FocusPrevious,
        Key::Right | Key::KP_Right => ChipKeyAction::FocusNext,
        _ => ChipKeyAction::None,
    }
}

fn same_recipient(a: &RecipientToken, b: &RecipientToken) -> bool {
    match (a, b) {
        (RecipientToken::Person { email: e1, .. }, RecipientToken::Person { email: e2, .. }) => {
            e1.eq_ignore_ascii_case(e2)
        }
        (RecipientToken::Group { id: id1, .. }, RecipientToken::Group { id: id2, .. }) => {
            id1 == id2
        }
        _ => false,
    }
}

fn remove_matching_token(tokens: &mut Vec<RecipientToken>, target: &RecipientToken) {
    if let Some(pos) = tokens.iter().position(|t| same_recipient(t, target)) {
        tokens.remove(pos);
    }
}

fn expand_group_chip(
    group_id: GroupId,
    book: &ContactBook,
    tokens: &mut Vec<RecipientToken>,
    toast_host: &gtk::Widget,
) {
    let Some(pos) = tokens
        .iter()
        .position(|t| matches!(t, RecipientToken::Group { id, .. } if *id == group_id))
    else {
        return;
    };
    let Some(group) = book.groups.iter().find(|g| g.id == group_id) else {
        return;
    };
    if group.member_ids.is_empty() {
        show_composer_toast(toast_host, &tr!("This group has no members"));
        return;
    }

    let existing: std::collections::HashSet<String> = tokens
        .iter()
        .filter_map(|t| match t {
            RecipientToken::Person { email, .. } => Some(email.to_ascii_lowercase()),
            _ => None,
        })
        .collect();

    let mut members = Vec::new();
    for contact_id in &group.member_ids {
        let Some(contact) = book.contacts.iter().find(|c| c.id == *contact_id) else {
            continue;
        };
        if existing.contains(&contact.email.to_ascii_lowercase()) {
            continue;
        }
        members.push(RecipientToken::Person {
            email: contact.email.clone(),
            name: if contact.display_name.is_empty() {
                None
            } else {
                Some(contact.display_name.clone())
            },
        });
    }

    tokens.remove(pos);
    for (offset, member) in members.into_iter().enumerate() {
        tokens.insert(pos + offset, member);
    }
}

pub fn tokens_to_strings(tokens: &[RecipientToken], book: &ContactBook) -> Vec<String> {
    expand_groups(tokens, book)
        .into_iter()
        .map(|token| match token {
            RecipientToken::Person { email, name } => Address { name, email }.formatted(),
            RecipientToken::Group { name, .. } => name,
        })
        .collect()
}

fn expand_groups(tokens: &[RecipientToken], book: &ContactBook) -> Vec<RecipientToken> {
    let mut out = Vec::new();
    for token in tokens {
        match token {
            RecipientToken::Group { id, .. } => {
                if let Some(group) = book.groups.iter().find(|g| g.id == *id) {
                    for contact_id in &group.member_ids {
                        if let Some(contact) = book.contacts.iter().find(|c| c.id == *contact_id) {
                            out.push(RecipientToken::Person {
                                email: contact.email.clone(),
                                name: if contact.display_name.is_empty() {
                                    None
                                } else {
                                    Some(contact.display_name.clone())
                                },
                            });
                        }
                    }
                }
            }
            RecipientToken::Person { email, name } => {
                out.push(RecipientToken::Person {
                    email: email.clone(),
                    name: name.clone(),
                });
            }
        }
    }
    out
}

pub fn strings_to_tokens(strings: &[String], book: &ContactBook) -> Vec<RecipientToken> {
    let mut tokens = Vec::new();
    for s in strings {
        if let Some(group) = find_user_group_by_name(s, book) {
            insert_group_token(&mut tokens, group.id, &group.name);
            continue;
        }
        if let Some(addr) = parse_address_token(s) {
            insert_person_token(&mut tokens, &addr.email, addr.name.as_deref());
        }
    }
    tokens
}

fn find_user_group_by_name<'a>(name: &str, book: &'a ContactBook) -> Option<&'a ContactGroup> {
    book.groups
        .iter()
        .find(|g| !g.is_system && g.name.eq_ignore_ascii_case(name.trim()))
}

pub(crate) fn insert_group_token(tokens: &mut Vec<RecipientToken>, id: GroupId, name: &str) {
    if tokens
        .iter()
        .any(|t| matches!(t, RecipientToken::Group { id: gid, .. } if *gid == id))
    {
        return;
    }
    tokens.push(RecipientToken::Group {
        id,
        name: name.to_string(),
    });
}

pub(crate) fn insert_person_token(
    tokens: &mut Vec<RecipientToken>,
    email: &str,
    name: Option<&str>,
) {
    let lower = email.to_ascii_lowercase();
    if tokens.iter().any(|t| match t {
        RecipientToken::Person { email, .. } => email.eq_ignore_ascii_case(&lower),
        _ => false,
    }) {
        return;
    }
    tokens.push(RecipientToken::Person {
        email: lower,
        name: name.map(str::to_string),
    });
}

fn commit_text(
    text: &str,
    tokens: &mut Vec<RecipientToken>,
    book: &ContactBook,
) -> Result<(), &'static str> {
    let parts: Vec<&str> = text
        .split([',', ';'])
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect();
    if parts.is_empty() {
        return Ok(());
    }

    for part in parts {
        if let Some(group) = find_user_group_by_name(part, book) {
            insert_group_token(tokens, group.id, &group.name);
            continue;
        }
        let Some(addr) = parse_address_token(part) else {
            return Err(trs!("Invalid address"));
        };
        insert_person_token(tokens, &addr.email, addr.name.as_deref());
    }
    Ok(())
}

fn autocomplete_items(query: &str, book: &ContactBook, sent: &[Address]) -> Vec<AutocompleteItem> {
    if query.is_empty() {
        return Vec::new();
    }
    let q = query.to_ascii_lowercase();
    let mut items = Vec::new();
    let mut seen_emails: HashSet<String> = HashSet::new();

    let mut groups: Vec<_> = book
        .groups
        .iter()
        .filter(|g| !g.is_system && g.name.to_ascii_lowercase().contains(&q))
        .collect();
    groups.sort_by(|a, b| {
        let a_prefix = a.name.to_ascii_lowercase().starts_with(&q);
        let b_prefix = b.name.to_ascii_lowercase().starts_with(&q);
        match (a_prefix, b_prefix) {
            (true, false) => Ordering::Less,
            (false, true) => Ordering::Greater,
            _ => a.name.cmp(&b.name),
        }
    });
    for group in groups {
        if items.len() >= MAX_AUTOCOMPLETE {
            break;
        }
        items.push(AutocompleteItem::Group {
            id: group.id,
            name: group.name.clone(),
            members: group.member_ids.len(),
        });
    }

    let mut approved: Vec<_> = book
        .contacts
        .iter()
        .filter(|c| c.status == ContactStatus::Approved && contact_matches_query(c, &q))
        .collect();
    approved.sort_by_key(|a| contact_rank(a, &q));
    for contact in approved {
        if items.len() >= MAX_AUTOCOMPLETE {
            break;
        }
        seen_emails.insert(contact.email.to_ascii_lowercase());
        items.push(person_item(contact));
    }

    if q.len() >= 2 {
        let mut suggested: Vec<_> = book
            .contacts
            .iter()
            .filter(|c| c.status == ContactStatus::Suggested && contact_matches_query(c, &q))
            .collect();
        suggested.sort_by_key(|a| contact_rank(a, &q));
        for contact in suggested {
            if items.len() >= MAX_AUTOCOMPLETE {
                break;
            }
            if seen_emails.insert(contact.email.to_ascii_lowercase()) {
                items.push(person_item(contact));
            }
        }
    }

    let mut sent_matches: Vec<_> = sent
        .iter()
        .filter(|addr| {
            let email = addr.email.to_ascii_lowercase();
            !seen_emails.contains(&email) && sent_person_matches(addr, &q)
        })
        .collect();
    sent_matches.sort_by_key(|a| sent_rank(a, &q));
    for addr in sent_matches {
        if items.len() >= MAX_AUTOCOMPLETE {
            break;
        }
        seen_emails.insert(addr.email.to_ascii_lowercase());
        items.push(AutocompleteItem::Person {
            email: addr.email.clone(),
            title: addr.display_name(),
        });
    }

    items
}

fn person_item(contact: &Contact) -> AutocompleteItem {
    let title = if contact.display_name.is_empty() {
        contact.email.clone()
    } else {
        contact.display_name.clone()
    };
    AutocompleteItem::Person {
        email: contact.email.clone(),
        title,
    }
}

fn autocomplete_plain_label(item: &AutocompleteItem) -> String {
    match item {
        AutocompleteItem::Group { name, .. } => name.clone(),
        AutocompleteItem::Person { email, title } => {
            if title.eq_ignore_ascii_case(email) {
                email.clone()
            } else {
                format!("{title} <{email}>")
            }
        }
    }
}

fn contact_matches_query(contact: &Contact, q: &str) -> bool {
    contact.email.to_ascii_lowercase().contains(q)
        || contact.display_name.to_ascii_lowercase().contains(q)
        || contact.nickname.to_ascii_lowercase().contains(q)
}

fn contact_rank(contact: &Contact, q: &str) -> (bool, String) {
    let email = contact.email.to_ascii_lowercase();
    let name = contact.display_name.to_ascii_lowercase();
    let nick = contact.nickname.to_ascii_lowercase();
    (
        email.starts_with(q) || name.starts_with(q) || nick.starts_with(q),
        contact.display_name.clone(),
    )
}

fn sent_rank(addr: &Address, q: &str) -> (bool, String) {
    let email = addr.email.to_ascii_lowercase();
    let name = addr.display_name().to_ascii_lowercase();
    (email.starts_with(q) || name.starts_with(q), email)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alcedo_core::{ContactId, UNAPPROVED_GROUP_ID};
    use chrono::Utc;

    #[test]
    fn chip_keys_remove_and_navigate() {
        use gtk::gdk::Key;
        assert_eq!(chip_key_action(Key::Delete), ChipKeyAction::Remove);
        assert_eq!(chip_key_action(Key::BackSpace), ChipKeyAction::Remove);
        assert_eq!(chip_key_action(Key::Left), ChipKeyAction::FocusPrevious);
        assert_eq!(chip_key_action(Key::Right), ChipKeyAction::FocusNext);
        assert_eq!(chip_key_action(Key::Return), ChipKeyAction::None);
    }

    #[test]
    fn tab_from_the_address_entry_moves_straight_to_the_next_field() {
        crate::test_helpers::gtk_test_run(|| {
            let field = RecipientField::new(
                "To",
                Rc::new(RefCell::new(sample_book())),
                Rc::new(RefCell::new(Vec::new())),
                gtk::Box::new(gtk::Orientation::Vertical, 0).upcast(),
            );
            let subject = gtk::Entry::new();
            let form = gtk::Box::new(gtk::Orientation::Vertical, 0);
            form.append(field.widget());
            form.append(&subject);
            let window = gtk::Window::builder().child(&form).build();
            window.present();

            field.focus_entry();
            window.child_focus(gtk::DirectionType::TabForward);

            let focused =
                gtk::prelude::GtkWindowExt::focus(&window).expect("a widget has focus after Tab");
            assert!(
                focused.is_ancestor(&subject),
                "Tab from the address entry should land in the next field, not on {focused:?}"
            );
            window.destroy();
        });
    }

    #[test]
    fn adding_a_recipient_keeps_focus_in_the_entry_so_tab_moves_on() {
        crate::test_helpers::gtk_test_run(|| {
            let field = RecipientField::new(
                "To",
                Rc::new(RefCell::new(sample_book())),
                Rc::new(RefCell::new(Vec::new())),
                gtk::Box::new(gtk::Orientation::Vertical, 0).upcast(),
            );
            let subject = gtk::Entry::new();
            let form = gtk::Box::new(gtk::Orientation::Vertical, 0);
            form.append(field.widget());
            form.append(&subject);
            let window = gtk::Window::builder().child(&form).build();
            window.present();

            field.focus_entry();
            field.tokens.borrow_mut().push(RecipientToken::Person {
                email: "alice@example.com".to_string(),
                name: None,
            });
            field.rebuild_chips();

            let focused =
                gtk::prelude::GtkWindowExt::focus(&window).expect("focus survives adding a chip");
            assert!(
                focused.is_ancestor(&field.entry),
                "focus should stay in the entry, not {focused:?}"
            );
            window.child_focus(gtk::DirectionType::TabForward);
            let focused = gtk::prelude::GtkWindowExt::focus(&window).expect("focus after Tab");
            assert!(
                focused.is_ancestor(&subject),
                "Tab after adding a chip should reach the next field, not {focused:?}"
            );
            window.destroy();
        });
    }

    fn sample_book() -> ContactBook {
        let alice = Contact {
            id: ContactId(1),
            email: "alice@example.com".to_string(),
            display_name: "Alice".to_string(),
            nickname: String::new(),
            organization: String::new(),
            notes: String::new(),
            status: ContactStatus::Approved,
            group_ids: vec![GroupId(2)],
            created_at: Utc::now(),
            updated_at: Utc::now(),
            last_used_at: None,
        };
        let carol = Contact {
            id: ContactId(3),
            email: "carol@example.com".to_string(),
            display_name: "Carol".to_string(),
            nickname: String::new(),
            organization: String::new(),
            notes: String::new(),
            status: ContactStatus::Approved,
            group_ids: vec![GroupId(2)],
            created_at: Utc::now(),
            updated_at: Utc::now(),
            last_used_at: None,
        };
        ContactBook {
            contacts: vec![alice, carol],
            groups: vec![
                ContactGroup {
                    id: UNAPPROVED_GROUP_ID,
                    name: "Unapproved".to_string(),
                    is_system: true,
                    member_ids: vec![],
                    created_at: Utc::now(),
                },
                ContactGroup {
                    id: GroupId(2),
                    name: "Family".to_string(),
                    is_system: false,
                    member_ids: vec![ContactId(1), ContactId(3)],
                    created_at: Utc::now(),
                },
            ],
        }
    }

    #[test]
    fn tokens_to_strings_expands_groups() {
        let book = sample_book();
        let tokens = vec![RecipientToken::Group {
            id: GroupId(2),
            name: "Family".to_string(),
        }];
        let strings = tokens_to_strings(&tokens, &book);
        assert_eq!(strings.len(), 2);
        assert!(strings.iter().any(|s| s.contains("alice@example.com")));
        assert!(strings.iter().any(|s| s.contains("carol@example.com")));
    }

    #[test]
    fn duplicate_person_ignored() {
        let mut tokens = Vec::new();
        insert_person_token(&mut tokens, "alice@example.com", Some("Alice"));
        insert_person_token(&mut tokens, "Alice@example.com", None);
        assert_eq!(tokens.len(), 1);
    }

    #[test]
    fn remove_matching_token_ignores_stale_index_after_prior_remove() {
        let mut tokens = vec![
            RecipientToken::Person {
                email: "alice@example.com".to_string(),
                name: Some("Alice".to_string()),
            },
            RecipientToken::Person {
                email: "bob@example.com".to_string(),
                name: Some("Bob".to_string()),
            },
            RecipientToken::Person {
                email: "carol@example.com".to_string(),
                name: Some("Carol".to_string()),
            },
        ];
        let alice = tokens[0].clone();
        let bob = tokens[1].clone();
        remove_matching_token(&mut tokens, &alice);
        remove_matching_token(&mut tokens, &bob);
        assert_eq!(tokens.len(), 1);
        assert!(matches!(
            &tokens[0],
            RecipientToken::Person { email, .. } if email == "carol@example.com"
        ));
    }

    #[test]
    fn autocomplete_plain_label_uses_name_and_email() {
        let named = AutocompleteItem::Person {
            email: "alice@example.com".to_string(),
            title: "Alice".to_string(),
        };
        assert_eq!(
            autocomplete_plain_label(&named),
            "Alice <alice@example.com>"
        );
    }

    #[test]
    fn autocomplete_plain_label_skips_redundant_email() {
        let email_only = AutocompleteItem::Person {
            email: "carter@codeinfusion.com".to_string(),
            title: "carter@codeinfusion.com".to_string(),
        };
        assert_eq!(
            autocomplete_plain_label(&email_only),
            "carter@codeinfusion.com"
        );
        let cased = AutocompleteItem::Person {
            email: "Carter@codeinfusion.com".to_string(),
            title: "carter@codeinfusion.com".to_string(),
        };
        assert_eq!(autocomplete_plain_label(&cased), "Carter@codeinfusion.com");
    }

    #[test]
    fn autocomplete_plain_label_group_is_name_only() {
        let group = AutocompleteItem::Group {
            id: GroupId(2),
            name: "Family".to_string(),
            members: 2,
        };
        assert_eq!(autocomplete_plain_label(&group), "Family");
    }

    #[test]
    fn highlight_query_bolds_case_insensitive_match() {
        assert_eq!(
            highlight_query("Carter St.Clair <carter@codeinfusion.com>", "car"),
            "<b>Car</b>ter St.Clair &lt;<b>car</b>ter@codeinfusion.com&gt;"
        );
    }

    #[test]
    fn highlight_query_escapes_markup() {
        assert_eq!(
            highlight_query("A & B <c@d.com>", "z"),
            "A &amp; B &lt;c@d.com&gt;"
        );
        assert_eq!(highlight_query("foo", ""), "foo");
    }

    #[test]
    fn autocomplete_includes_sent_addresses_matching_query() {
        let book = sample_book();
        let sent = vec![
            Address {
                name: Some("Jordan".to_string()),
                email: "jordan@example.com".to_string(),
            },
            Address {
                name: None,
                email: "bob@example.com".to_string(),
            },
        ];
        let items = autocomplete_items("example.com", &book, &sent);
        let emails: Vec<_> = items
            .iter()
            .filter_map(|item| match item {
                AutocompleteItem::Person { email, .. } => Some(email.as_str()),
                _ => None,
            })
            .collect();
        assert!(emails.contains(&"alice@example.com"));
        assert!(emails.contains(&"carol@example.com"));
        assert!(emails.contains(&"jordan@example.com"));
        assert!(emails.contains(&"bob@example.com"));
    }

    #[test]
    fn autocomplete_dedupes_sent_against_contacts() {
        let book = sample_book();
        let sent = vec![Address {
            name: Some("Alice Again".to_string()),
            email: "alice@example.com".to_string(),
        }];
        let items = autocomplete_items("alice", &book, &sent);
        let alice_count = items
            .iter()
            .filter(|item| {
                matches!(
                    item,
                    AutocompleteItem::Person { email, .. } if email == "alice@example.com"
                )
            })
            .count();
        assert_eq!(alice_count, 1);
    }

    #[test]
    fn next_autocomplete_index_moves_down_from_unselected() {
        assert_eq!(next_autocomplete_index(-1, 3, gtk::gdk::Key::Down), Some(0));
        assert_eq!(next_autocomplete_index(0, 3, gtk::gdk::Key::Down), Some(1));
        assert_eq!(next_autocomplete_index(2, 3, gtk::gdk::Key::Down), Some(2));
    }

    #[test]
    fn next_autocomplete_index_moves_up_to_unselected() {
        assert_eq!(next_autocomplete_index(1, 3, gtk::gdk::Key::Up), Some(0));
        assert_eq!(next_autocomplete_index(0, 3, gtk::gdk::Key::Up), None);
        assert_eq!(next_autocomplete_index(-1, 3, gtk::gdk::Key::Up), None);
    }

    #[test]
    fn commit_text_splits_comma_separated_addresses() {
        let book = sample_book();
        let mut tokens = Vec::new();
        super::commit_text("alice@example.com, carol@example.com", &mut tokens, &book)
            .expect("valid addresses");
        assert_eq!(tokens.len(), 2);
    }
}
