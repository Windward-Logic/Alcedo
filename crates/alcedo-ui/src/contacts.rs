//! Contacts canvas and preferences helpers.

use alcedo_i18n::{ntr, tr, trf, trs};
use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::rc::Rc;

use alcedo_core::{
    Address, Contact, ContactDraft, ContactGroup, ContactId, ContactStatus, GroupDraft, GroupId,
    UNAPPROVED_GROUP_ID, UiCommand, normalize_sender_email, parse_address_token,
    promote_contact_in_memory, remove_contacts_in_memory,
};
use relm4::Sender;
use relm4::adw;
use relm4::adw::prelude::*;
use relm4::gtk;
use relm4::gtk::glib;
use relm4::prelude::*;

use crate::activity_log::{self, ActivityLog, ActivityOutcome};
use crate::help::{self, TOPIC_CONTACTS};
use crate::recipient_row::ContactBook;
use crate::settings::{bind_collect_outgoing_contacts, collect_outgoing_contacts};

/// Contacts-space messages handled by [`ContactsModel`].
#[derive(Debug)]
pub enum ContactsMsg {
    /// Pop the contacts navigation stack back to the list view.
    Show,
    RefreshContacts,
    NewGroup,
    UpsertContact(ContactDraft),
    DeleteContacts(Vec<ContactId>),
    UpsertGroup(GroupDraft),
    DeleteGroup(GroupId),
    PromoteContact(ContactId),
    SetSenderRemoteImages {
        email: String,
        allow: bool,
    },
    ContactsChanged {
        contacts: Vec<Contact>,
        groups: Vec<ContactGroup>,
    },
    ViewContact(ContactId),
    AddContactFromAddress(Address),
}

/// Shell routing output from the contacts child.
#[derive(Debug)]
pub enum ContactsOutput {
    ShowMail,
    ComposeMailto(String),
}

impl ContactsOutput {
    /// Map to app-shell routing messages.
    pub fn into_app_msg(self) -> crate::app::AppMsg {
        use crate::app::AppMsg;
        match self {
            Self::ShowMail => AppMsg::ShowMail,
            Self::ComposeMailto(address) => AppMsg::ComposeMailto(address),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ContactFilter {
    All,
    Approved,
    Unapproved,
    Blocked,
    Group(GroupId),
}

/// Contacts canvas on the main content stack (list, search, groups).
pub struct ContactsCanvas {
    pub root: gtk::Box,
    pub nav: adw::NavigationView,
    title: gtk::Label,
    select_btn: gtk::Button,
    selection_revealer: gtk::Revealer,
    selection_bar: gtk::ActionBar,
    delete_selected_btn: gtk::Button,
    select_all_btn: gtk::Button,
    selecting: Rc<Cell<bool>>,
    selected: Rc<RefCell<HashSet<ContactId>>>,
    visible_ids: Rc<RefCell<Vec<ContactId>>>,
    list_group: adw::PreferencesGroup,
    groups_group: adw::PreferencesGroup,
    search: gtk::SearchEntry,
    filter: adw::ComboRow,
    open: Rc<RefCell<Option<OpenEditor>>>,
    syncing: Rc<Cell<bool>>,
}

/// Navigation host for contact/group editor subpages.
pub struct ContactNav {
    pub view: adw::NavigationView,
    pub parent: gtk::Widget,
}

impl ContactsCanvas {
    /// Build a [`ContactNav`] for pushing editor pages and modal dialogs.
    pub fn contact_nav(&self) -> ContactNav {
        ContactNav {
            view: self.nav.clone(),
            parent: self.root.clone().upcast(),
        }
    }

    pub fn show_list(&self) {
        self.nav.pop_to_tag("list");
        *self.open.borrow_mut() = None;
    }

    pub fn exit_selection_mode(&self, sender: &Sender<ContactsMsg>) {
        if self.selecting.get() {
            set_selection_mode(self, false, sender);
        }
    }

    pub fn is_selecting(&self) -> bool {
        self.selecting.get()
    }

    fn clone_for_handlers(&self) -> Self {
        Self {
            root: self.root.clone(),
            nav: self.nav.clone(),
            title: self.title.clone(),
            select_btn: self.select_btn.clone(),
            selection_revealer: self.selection_revealer.clone(),
            selection_bar: self.selection_bar.clone(),
            delete_selected_btn: self.delete_selected_btn.clone(),
            select_all_btn: self.select_all_btn.clone(),
            selecting: self.selecting.clone(),
            selected: self.selected.clone(),
            visible_ids: self.visible_ids.clone(),
            list_group: self.list_group.clone(),
            groups_group: self.groups_group.clone(),
            search: self.search.clone(),
            filter: self.filter.clone(),
            open: self.open.clone(),
            syncing: self.syncing.clone(),
        }
    }
}

/// The editor subpage currently pushed. Nothing reads these fields: holding
/// them keeps the draft and page alive exactly until the editor is closed and
/// `open` is reset to `None`.
enum OpenEditor {
    Contact {
        _draft: Rc<RefCell<ContactDraft>>,
        _nav_page: adw::NavigationPage,
    },
    Group {
        _draft: Rc<RefCell<GroupDraft>>,
        _nav_page: adw::NavigationPage,
    },
}

/// Build the contacts canvas with header chrome and back navigation.
pub fn build_contacts_canvas(
    on_back: impl Fn() + 'static,
    sender: Sender<ContactsMsg>,
) -> ContactsCanvas {
    let title = gtk::Label::builder()
        .label(tr!("Contacts"))
        .css_classes(["title"])
        .build();
    let header = adw::HeaderBar::builder().title_widget(&title).build();
    header.pack_end(&crate::menu::build_primary_menu_button());

    let back_btn = gtk::Button::builder()
        .icon_name("go-previous-symbolic")
        .tooltip_text(tr!("Back"))
        .build();
    back_btn.connect_clicked(move |_| on_back());
    header.pack_start(&back_btn);

    let selecting = Rc::new(Cell::new(false));
    let selected = Rc::new(RefCell::new(HashSet::new()));

    let select_btn = gtk::Button::builder()
        .label(tr!("Select"))
        .tooltip_text(tr!("Select contacts"))
        .css_classes(["flat"])
        .build();
    header.pack_end(&select_btn);
    header.pack_end(&help::help_button(TOPIC_CONTACTS));

    let search = gtk::SearchEntry::builder()
        .placeholder_text(tr!("Search contacts"))
        .hexpand(true)
        .build();

    let filter_model = gtk::StringList::new(&[
        tr!("All").as_str(),
        tr!("Approved").as_str(),
        tr!("Unapproved").as_str(),
        tr!("Blocked").as_str(),
    ]);
    let filter = adw::ComboRow::builder()
        .title(tr!("Show"))
        .model(&filter_model)
        .build();

    let filter_group = adw::PreferencesGroup::new();
    filter_group.add(&filter);

    let list_group = adw::PreferencesGroup::builder()
        .title(tr!("Contacts"))
        .description(tr!("No contacts yet. Add someone or send a message."))
        .build();

    let groups_group = adw::PreferencesGroup::builder()
        .title(tr!("Groups"))
        .build();

    let add_contact = adw::ActionRow::builder()
        .title(tr!("Add contact"))
        .activatable(true)
        .build();
    add_contact.add_prefix(&gtk::Image::from_icon_name("list-add-symbolic"));
    add_contact.connect_activated({
        let sender = sender.clone();
        move |_| {
            sender
                .send(ContactsMsg::AddContactFromAddress(Address {
                    name: None,
                    email: String::new(),
                }))
                .ok();
        }
    });

    let add_group = adw::ActionRow::builder()
        .title(tr!("Add group"))
        .activatable(true)
        .build();
    add_group.add_prefix(&gtk::Image::from_icon_name("list-add-symbolic"));
    add_group.connect_activated({
        let sender = sender.clone();
        move |_| {
            sender.send(ContactsMsg::NewGroup).ok();
        }
    });

    let add_group_prefs = adw::PreferencesGroup::builder().title(tr!("Add")).build();
    add_group_prefs.add(&add_contact);
    add_group_prefs.add(&add_group);

    let body = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .css_classes(["alcedo-contacts-page"])
        .build();
    body.append(&search);
    body.append(&filter_group);
    body.append(&list_group);
    body.append(&groups_group);
    body.append(&add_group_prefs);

    let scrolled = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .child(&body)
        .build();

    let delete_selected_btn = gtk::Button::builder()
        .icon_name("user-trash-symbolic")
        .tooltip_text(tr!("Delete"))
        .css_classes(["flat"])
        .focus_on_click(false)
        .build();
    let select_all_btn = gtk::Button::builder()
        .label(tr!("Select All"))
        .tooltip_text(tr!("Select All"))
        .css_classes(["flat"])
        .focus_on_click(false)
        .build();
    let selection_bar = gtk::ActionBar::builder()
        .css_classes(["alcedo-selection-bar"])
        .build();
    selection_bar.pack_start(&delete_selected_btn);
    selection_bar.pack_end(&select_all_btn);

    let selection_revealer = gtk::Revealer::builder()
        .transition_type(gtk::RevealerTransitionType::SlideUp)
        .transition_duration(180)
        .reveal_child(false)
        .child(&selection_bar)
        .build();

    let list_page = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .build();
    list_page.append(&scrolled);
    list_page.append(&selection_revealer);

    let list_root_page = adw::NavigationPage::builder()
        .title(tr!("Contacts"))
        .tag("list")
        .child(&list_page)
        .build();

    let nav = adw::NavigationView::new();
    nav.push(&list_root_page);

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&nav));
    toolbar.set_vexpand(true);

    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.set_vexpand(true);
    root.append(&toolbar);

    let syncing = Rc::new(Cell::new(false));
    let visible_ids = Rc::new(RefCell::new(Vec::new()));
    search.connect_search_changed({
        let sender = sender.clone();
        let syncing = syncing.clone();
        move |_| {
            if syncing.get() {
                return;
            }
            sender.send(ContactsMsg::RefreshContacts).ok();
        }
    });
    filter.connect_selected_notify({
        let sender = sender.clone();
        let syncing = syncing.clone();
        move |_| {
            if syncing.get() {
                return;
            }
            sender.send(ContactsMsg::RefreshContacts).ok();
        }
    });

    let open = Rc::new(RefCell::new(None));
    let canvas = ContactsCanvas {
        root: root.clone(),
        nav,
        title,
        select_btn: select_btn.clone(),
        selection_revealer: selection_revealer.clone(),
        selection_bar,
        delete_selected_btn: delete_selected_btn.clone(),
        select_all_btn: select_all_btn.clone(),
        selecting: selecting.clone(),
        selected: selected.clone(),
        visible_ids: visible_ids.clone(),
        list_group,
        groups_group,
        search,
        filter,
        open: open.clone(),
        syncing: syncing.clone(),
    };

    select_btn.connect_clicked({
        let canvas = canvas.clone_for_handlers();
        let sender = sender.clone();
        move |_| {
            if canvas.selecting.get() {
                set_selection_mode(&canvas, false, &sender);
            } else {
                canvas.show_list();
                set_selection_mode(&canvas, true, &sender);
            }
        }
    });

    delete_selected_btn.connect_clicked({
        let selected = selected.clone();
        let sender = sender.clone();
        let parent = root.clone().upcast::<gtk::Widget>();
        move |_| {
            let ids: Vec<_> = selected.borrow().iter().copied().collect();
            if ids.is_empty() {
                return;
            }
            let count = ids.len();
            let sender = sender.clone();
            confirm_delete_contacts(&parent, count, move || {
                sender.send(ContactsMsg::DeleteContacts(ids)).ok();
            });
        }
    });

    select_all_btn.connect_clicked({
        let selected = selected.clone();
        let visible_ids = visible_ids.clone();
        let sender = sender.clone();
        move |_| {
            for id in visible_ids.borrow().iter().copied() {
                selected.borrow_mut().insert(id);
            }
            sender.send(ContactsMsg::RefreshContacts).ok();
        }
    });

    let key = gtk::EventControllerKey::new();
    key.connect_key_pressed({
        let canvas = canvas.clone_for_handlers();
        let sender = sender.clone();
        move |_, key, _, _| {
            if key == gtk::gdk::Key::Escape && canvas.selecting.get() {
                set_selection_mode(&canvas, false, &sender);
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        }
    });
    root.add_controller(key);

    canvas
}

/// Slim Contacts preferences page (open canvas + outgoing switch).
pub fn build_contacts_prefs_page(sender: Sender<crate::app::AppMsg>) -> adw::PreferencesPage {
    let open_row = adw::ActionRow::builder()
        .title(tr!("Open Contacts"))
        .subtitle(tr!("Search, add, and edit people in the Contacts view."))
        .activatable(true)
        .build();
    open_row.add_prefix(&gtk::Image::from_icon_name(
        "x-office-address-book-symbolic",
    ));
    open_row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
    open_row.connect_activated({
        let sender = sender.clone();
        move |_| {
            sender.send(crate::app::AppMsg::OpenContacts).ok();
        }
    });

    let collect_row = adw::SwitchRow::builder()
        .title(tr!("Add outgoing addresses to Unapproved"))
        .subtitle(tr!(
            "Addresses you send to are saved as unapproved contacts until you keep them."
        ))
        .active(collect_outgoing_contacts())
        .build();
    bind_collect_outgoing_contacts(&collect_row);

    let contacts_group = adw::PreferencesGroup::builder()
        .title(tr!("Address book"))
        .build();
    help::attach_group_help(&contacts_group, TOPIC_CONTACTS);
    contacts_group.add(&open_row);
    contacts_group.add(&collect_row);

    let page = adw::PreferencesPage::builder()
        .title(tr!("Contacts"))
        .icon_name("x-office-address-book-symbolic")
        .build();
    page.add(&contacts_group);
    page
}

pub fn refresh_contact_rows(
    canvas: &ContactsCanvas,
    nav: &ContactNav,
    book: &ContactBook,
    sender: &Sender<ContactsMsg>,
    remote_image_senders: &std::collections::HashSet<String>,
) {
    let query = canvas.search.text().to_lowercase();
    let filter = current_filter(&canvas.filter, book);
    let selecting = canvas.selecting.get();

    canvas
        .selected
        .borrow_mut()
        .retain(|id| book.contacts.iter().any(|c| c.id == *id));

    canvas.syncing.set(true);

    while let Some(row) = canvas.list_group.row(0) {
        canvas.list_group.remove(&row);
    }
    while let Some(row) = canvas.groups_group.row(0) {
        canvas.groups_group.remove(&row);
    }

    update_filter_model(&canvas.filter, book);
    canvas.syncing.set(false);

    let contacts: Vec<_> = book
        .contacts
        .iter()
        .filter(|c| matches_filter(c, filter))
        .filter(|c| contact_matches_search(c, &query, book))
        .collect();

    *canvas.visible_ids.borrow_mut() = contacts.iter().map(|c| c.id).collect();
    sync_selection_chrome(canvas, contacts.len());

    if contacts.is_empty() {
        canvas.list_group.set_description(Some(if query.is_empty() {
            trs!("No contacts yet. Add someone or send a message.")
        } else {
            trs!("No matching contacts")
        }));
    } else {
        canvas.list_group.set_description(None::<&str>);
    }

    let parent = canvas.root.clone().upcast::<gtk::Widget>();

    for contact in contacts {
        let is_selected = canvas.selected.borrow().contains(&contact.id);
        let ContactListRow { row, check, trash } =
            contact_list_row(contact, book, selecting, is_selected);
        let contact = contact.clone();
        let nav = ContactNav {
            view: nav.view.clone(),
            parent: nav.parent.clone(),
        };
        let sender = sender.clone();
        let open = canvas.open.clone();
        let senders = remote_image_senders.clone();
        let canvas_handlers = canvas.clone_for_handlers();
        let selected = canvas.selected.clone();
        let contact_id = contact.id;

        if let Some(check) = check {
            let title = if contact.display_name.is_empty() {
                contact.email.clone()
            } else {
                contact.display_name.clone()
            };
            check.update_property(&[gtk::accessible::Property::Label(title.as_str())]);
            let toggling = Rc::new(Cell::new(false));
            check.connect_toggled({
                let canvas_handlers = canvas_handlers.clone_for_handlers();
                let selected = selected.clone();
                let toggling = toggling.clone();
                move |check| {
                    if toggling.get() {
                        return;
                    }
                    if check.is_active() {
                        selected.borrow_mut().insert(contact_id);
                    } else {
                        selected.borrow_mut().remove(&contact_id);
                    }
                    sync_selection_chrome(
                        &canvas_handlers,
                        canvas_handlers.visible_ids.borrow().len(),
                    );
                }
            });
            row.connect_activated({
                let check = check.clone();
                let toggling = toggling.clone();
                let selected = selected.clone();
                let canvas_handlers = canvas_handlers.clone_for_handlers();
                move |_| {
                    let next = !check.is_active();
                    toggling.set(true);
                    check.set_active(next);
                    toggling.set(false);
                    if next {
                        selected.borrow_mut().insert(contact_id);
                    } else {
                        selected.borrow_mut().remove(&contact_id);
                    }
                    sync_selection_chrome(
                        &canvas_handlers,
                        canvas_handlers.visible_ids.borrow().len(),
                    );
                }
            });
        } else if let Some(trash) = trash {
            let parent = parent.clone();
            let trash_sender = sender.clone();
            trash.connect_clicked(move |_| {
                let sender = trash_sender.clone();
                confirm_delete_contacts(&parent, 1, move || {
                    sender
                        .send(ContactsMsg::DeleteContacts(vec![contact_id]))
                        .ok();
                });
            });
            row.connect_activated(move |_| {
                push_contact_editor(&nav, Some(&contact), &sender, &open, &senders);
            });
        }
        canvas.list_group.add(&row);
    }

    for group in &book.groups {
        let row = group_list_row(group);
        if group.is_system {
            let filter = canvas.filter.clone();
            row.connect_activated(move |_| {
                filter.set_selected(2);
            });
        } else {
            let group = group.clone();
            let book = book.clone();
            let nav = ContactNav {
                view: nav.view.clone(),
                parent: nav.parent.clone(),
            };
            let sender = sender.clone();
            let open = canvas.open.clone();
            row.connect_activated(move |_| {
                push_group_editor(&nav, Some(&group), &book, &sender, &open);
            });
        }
        canvas.groups_group.add(&row);
    }
}

pub fn open_contact(
    canvas: &ContactsCanvas,
    nav: &ContactNav,
    book: &ContactBook,
    id: ContactId,
    sender: &Sender<ContactsMsg>,
    remote_image_senders: &std::collections::HashSet<String>,
) {
    if let Some(contact) = book.contacts.iter().find(|c| c.id == id) {
        push_contact_editor(
            nav,
            Some(contact),
            sender,
            &canvas.open,
            remote_image_senders,
        );
    }
}

pub fn open_new_contact(
    canvas: &ContactsCanvas,
    nav: &ContactNav,
    sender: &Sender<ContactsMsg>,
    remote_image_senders: &std::collections::HashSet<String>,
) {
    push_contact_editor(nav, None, sender, &canvas.open, remote_image_senders);
}

pub fn open_new_group(
    canvas: &ContactsCanvas,
    nav: &ContactNav,
    book: &ContactBook,
    sender: &Sender<ContactsMsg>,
) {
    push_group_editor(nav, None, book, sender, &canvas.open);
}

struct ContactListRow {
    row: adw::ActionRow,
    check: Option<gtk::CheckButton>,
    trash: Option<gtk::Button>,
}

fn contact_list_row(
    contact: &Contact,
    book: &ContactBook,
    selecting: bool,
    is_selected: bool,
) -> ContactListRow {
    let title = if contact.display_name.is_empty() {
        contact.email.clone()
    } else {
        contact.display_name.clone()
    };
    let group_names: Vec<_> = book
        .groups
        .iter()
        .filter(|g| !g.is_system && contact.group_ids.contains(&g.id))
        .map(|g| g.name.as_str())
        .collect();
    let mut subtitle = contact.email.clone();
    if !group_names.is_empty() {
        subtitle.push_str(", ");
        subtitle.push_str(&group_names.join(", "));
    }
    if contact.status == ContactStatus::Suggested {
        subtitle = trf!("Unapproved, {details}", details = subtitle);
    } else if contact.status == ContactStatus::Blocked {
        subtitle = trf!("Blocked, {details}", details = subtitle);
    }
    let row = adw::ActionRow::builder()
        .use_markup(false)
        .title(&title)
        .subtitle(&subtitle)
        .activatable(true)
        .build();
    let check = if selecting {
        let btn = gtk::CheckButton::builder()
            .active(is_selected)
            .can_focus(false)
            .build();
        row.add_prefix(&btn);
        Some(btn)
    } else {
        None
    };
    let avatar = adw::Avatar::builder()
        .size(32)
        .show_initials(true)
        .text(&title)
        .build();
    avatar.set_can_focus(false);
    row.add_prefix(&avatar);
    let mut trash = None;
    if !selecting {
        let btn = gtk::Button::builder()
            .icon_name("user-trash-symbolic")
            .tooltip_text(tr!("Delete contact"))
            .css_classes(["flat"])
            .valign(gtk::Align::Center)
            .focus_on_click(false)
            .build();
        row.add_suffix(&btn);
        trash = Some(btn);
    }
    ContactListRow { row, check, trash }
}

fn delete_contacts_heading(n: usize) -> String {
    if n == 1 {
        tr!("Delete contact?")
    } else {
        ntr!("Delete {n} contact?", "Delete {n} contacts?", n)
    }
}

fn confirm_delete_contacts(parent: &gtk::Widget, n: usize, on_ok: impl FnOnce() + 'static) {
    let confirm = adw::AlertDialog::builder()
        .heading(delete_contacts_heading(n))
        .body(tr!("This cannot be undone."))
        .build();
    confirm.add_response("cancel", &tr!("Cancel"));
    confirm.add_response("delete", &tr!("Delete"));
    confirm.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
    confirm.set_close_response("cancel");
    confirm.choose(
        Some(parent),
        None::<&gtk::gio::Cancellable>,
        move |response| {
            if response.as_str() == "delete" {
                on_ok();
            }
        },
    );
}

fn set_selection_mode(canvas: &ContactsCanvas, on: bool, sender: &Sender<ContactsMsg>) {
    canvas.selecting.set(on);
    if !on {
        canvas.selected.borrow_mut().clear();
        canvas.select_btn.set_label(&tr!("Select"));
        canvas
            .select_btn
            .set_tooltip_text(Some(&tr!("Select contacts")));
        canvas.selection_revealer.set_reveal_child(false);
        canvas.title.set_label(&tr!("Contacts"));
    } else {
        canvas.select_btn.set_label(&tr!("Cancel"));
        canvas
            .select_btn
            .set_tooltip_text(Some(&tr!("Cancel selection")));
        canvas.selection_revealer.set_reveal_child(true);
        sync_selection_chrome(canvas, canvas.visible_ids.borrow().len());
    }
    sender.send(ContactsMsg::RefreshContacts).ok();
}

fn sync_selection_chrome(canvas: &ContactsCanvas, visible_count: usize) {
    if canvas.selecting.get() {
        let n = canvas.selected.borrow().len();
        let title = ntr!("{n} selected", "{n} selected", n);
        canvas.title.set_label(&title);
    } else {
        canvas.title.set_label(&tr!("Contacts"));
    }
    canvas
        .delete_selected_btn
        .set_sensitive(!canvas.selected.borrow().is_empty());
    let visible_ids = canvas.visible_ids.borrow();
    let all_selected = !visible_ids.is_empty()
        && visible_ids
            .iter()
            .all(|id| canvas.selected.borrow().contains(id));
    canvas
        .select_all_btn
        .set_sensitive(!visible_ids.is_empty() && !all_selected);
    canvas
        .select_btn
        .set_sensitive(canvas.selecting.get() || visible_count > 0);
}

fn group_list_row(group: &ContactGroup) -> adw::ActionRow {
    let members = group.member_ids.len();
    let subtitle = ntr!("{n} member", "{n} members", members);
    adw::ActionRow::builder()
        .use_markup(false)
        .title(&group.name)
        .subtitle(&subtitle)
        .activatable(!group.is_system || group.id == UNAPPROVED_GROUP_ID)
        .build()
}

fn current_filter(filter: &adw::ComboRow, book: &ContactBook) -> ContactFilter {
    let index = filter.selected() as usize;
    if index == 0 {
        ContactFilter::All
    } else if index == 1 {
        ContactFilter::Approved
    } else if index == 2 {
        ContactFilter::Unapproved
    } else if index == 3 {
        ContactFilter::Blocked
    } else {
        let offset = index - 4;
        book.groups
            .iter()
            .filter(|g| !g.is_system)
            .nth(offset)
            .map(|g| ContactFilter::Group(g.id))
            .unwrap_or(ContactFilter::All)
    }
}

fn update_filter_model(filter: &adw::ComboRow, book: &ContactBook) {
    let selected = filter.selected();
    let mut labels = vec![
        tr!("All"),
        tr!("Approved"),
        tr!("Unapproved"),
        tr!("Blocked"),
    ];
    for group in book.groups.iter().filter(|g| !g.is_system) {
        labels.push(group.name.clone());
    }
    let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
    let model = gtk::StringList::new(&refs);
    filter.set_model(Some(&model));
    filter.set_selected(selected.min((labels.len() as u32).saturating_sub(1)));
}

fn matches_filter(contact: &Contact, filter: ContactFilter) -> bool {
    match filter {
        ContactFilter::All => true,
        ContactFilter::Approved => contact.status == ContactStatus::Approved,
        ContactFilter::Unapproved => contact.status == ContactStatus::Suggested,
        ContactFilter::Blocked => contact.status == ContactStatus::Blocked,
        ContactFilter::Group(id) => contact.group_ids.contains(&id),
    }
}

fn contact_matches_search(contact: &Contact, query: &str, book: &ContactBook) -> bool {
    if query.is_empty() {
        return true;
    }
    if contact.email.to_lowercase().contains(query)
        || contact.display_name.to_lowercase().contains(query)
        || contact.nickname.to_lowercase().contains(query)
    {
        return true;
    }
    book.groups
        .iter()
        .any(|g| contact.group_ids.contains(&g.id) && g.name.to_lowercase().contains(query))
}

fn push_contact_editor(
    nav: &ContactNav,
    contact: Option<&Contact>,
    sender: &Sender<ContactsMsg>,
    open: &Rc<RefCell<Option<OpenEditor>>>,
    remote_image_senders: &std::collections::HashSet<String>,
) {
    let existing_id = contact.map(|c| c.id);
    let draft = Rc::new(RefCell::new(ContactDraft {
        id: existing_id,
        email: contact.map(|c| c.email.clone()).unwrap_or_default(),
        display_name: contact.map(|c| c.display_name.clone()).unwrap_or_default(),
        nickname: contact.map(|c| c.nickname.clone()).unwrap_or_default(),
        organization: contact.map(|c| c.organization.clone()).unwrap_or_default(),
        notes: contact.map(|c| c.notes.clone()).unwrap_or_default(),
        status: contact.map(|c| c.status).unwrap_or(ContactStatus::Approved),
        group_ids: contact.map(|c| c.group_ids.clone()).unwrap_or_default(),
    }));

    let name_row = adw::EntryRow::builder().title(tr!("Name")).build();
    name_row.set_text(contact.map(|c| c.display_name.as_str()).unwrap_or(""));
    let email_row = adw::EntryRow::builder().title(tr!("Email")).build();
    email_row.set_text(contact.map(|c| c.email.as_str()).unwrap_or(""));
    let nickname_row = adw::EntryRow::builder().title(tr!("Nickname")).build();
    nickname_row.set_text(contact.map(|c| c.nickname.as_str()).unwrap_or(""));
    let org_row = adw::EntryRow::builder().title(tr!("Organization")).build();
    org_row.set_text(contact.map(|c| c.organization.as_str()).unwrap_or(""));
    let notes_row = adw::EntryRow::builder().title(tr!("Notes")).build();
    notes_row.set_text(contact.map(|c| c.notes.as_str()).unwrap_or(""));

    let save_row = adw::ButtonRow::builder()
        .title(tr!("Save"))
        .sensitive(false)
        .build();
    let delete_row = adw::ButtonRow::builder()
        .title(tr!("Delete contact"))
        .css_classes(["destructive-action"])
        .build();

    let page = adw::PreferencesPage::new();
    let fields = adw::PreferencesGroup::new();
    fields.add(&name_row);
    fields.add(&email_row);
    fields.add(&nickname_row);
    fields.add(&org_row);
    fields.add(&notes_row);
    page.add(&fields);

    let sender_allowed = contact.is_some_and(|c| {
        alcedo_core::normalize_sender_email(&c.email)
            .is_some_and(|email| remote_image_senders.contains(&email))
    });
    let remote_row = adw::SwitchRow::builder()
        .title(tr!("Load remote images"))
        .subtitle(tr!(
            "HTML mail from this address loads remote images automatically."
        ))
        .active(sender_allowed)
        .build();
    let remote_group = adw::PreferencesGroup::new();
    remote_group.add(&remote_row);
    page.add(&remote_group);

    if contact.is_some_and(|c| c.status == ContactStatus::Suggested) {
        let promote_row = adw::ButtonRow::builder()
            .title(tr!("Save to contacts"))
            .build();
        let promote_group = adw::PreferencesGroup::new();
        promote_group.add(&promote_row);
        page.add(&promote_group);
        if let Some(id) = existing_id {
            promote_row.connect_activated({
                let sender = sender.clone();
                move |_| {
                    sender.send(ContactsMsg::PromoteContact(id)).ok();
                }
            });
        }
    }

    let save_group = adw::PreferencesGroup::new();
    save_group.add(&save_row);
    page.add(&save_group);
    if existing_id.is_some() {
        let delete_group = adw::PreferencesGroup::new();
        delete_group.add(&delete_row);
        page.add(&delete_group);
    }

    let title = contact
        .map(|c| {
            if c.display_name.is_empty() {
                c.email.clone()
            } else {
                c.display_name.clone()
            }
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Add contact".to_string());
    let mut nav_page = adw::NavigationPage::builder().title(&title).child(&page);
    if let Some(id) = existing_id {
        nav_page = nav_page.tag(id.0.to_string());
    }
    let nav_page = nav_page.build();

    let mark_dirty = Rc::new({
        let draft = draft.clone();
        let save_row = save_row.clone();
        let email_row = email_row.clone();
        let name_row = name_row.clone();
        let nickname_row = nickname_row.clone();
        let org_row = org_row.clone();
        let notes_row = notes_row.clone();
        move || {
            draft.borrow_mut().display_name = name_row.text().to_string();
            draft.borrow_mut().email = email_row.text().to_string();
            draft.borrow_mut().nickname = nickname_row.text().to_string();
            draft.borrow_mut().organization = org_row.text().to_string();
            draft.borrow_mut().notes = notes_row.text().to_string();
            let valid = parse_address_token(&email_row.text()).is_some();
            save_row.set_sensitive(valid);
            if !valid {
                email_row.add_css_class("error");
            } else {
                email_row.remove_css_class("error");
            }
        }
    });
    for row in [&name_row, &email_row, &nickname_row, &org_row, &notes_row] {
        let mark_dirty = mark_dirty.clone();
        row.connect_changed(move |_| mark_dirty());
    }

    let remote_syncing = Rc::new(Cell::new(false));
    let remote_senders = Rc::new(RefCell::new(remote_image_senders.clone()));
    email_row.connect_changed({
        let remote_row = remote_row.clone();
        let remote_syncing = remote_syncing.clone();
        let remote_senders = remote_senders.clone();
        move |email_row| {
            remote_syncing.set(true);
            let allowed = alcedo_core::normalize_sender_email(&email_row.text())
                .is_some_and(|email| remote_senders.borrow().contains(&email));
            remote_row.set_active(allowed);
            remote_syncing.set(false);
        }
    });
    remote_row.connect_notify_local(Some("active"), {
        let sender = sender.clone();
        let email_row = email_row.clone();
        let remote_syncing = remote_syncing.clone();
        let remote_senders = remote_senders.clone();
        move |row, _| {
            if remote_syncing.get() {
                return;
            }
            let email = email_row.text().to_string();
            if email.is_empty() {
                return;
            }
            if let Some(normalized) = alcedo_core::normalize_sender_email(&email) {
                if row.is_active() {
                    remote_senders.borrow_mut().insert(normalized);
                } else {
                    remote_senders.borrow_mut().remove(&normalized);
                }
            }
            sender
                .send(ContactsMsg::SetSenderRemoteImages {
                    email,
                    allow: row.is_active(),
                })
                .ok();
        }
    });

    save_row.connect_activated({
        let sender = sender.clone();
        let draft = draft.clone();
        move |_| {
            let mut value = draft.borrow().clone();
            value.display_name = name_row.text().to_string();
            value.email = email_row.text().to_string();
            value.nickname = nickname_row.text().to_string();
            value.organization = org_row.text().to_string();
            value.notes = notes_row.text().to_string();
            sender.send(ContactsMsg::UpsertContact(value)).ok();
        }
    });

    if let Some(id) = existing_id {
        delete_row.connect_activated({
            let sender = sender.clone();
            let parent = nav.parent.clone();
            move |_| {
                let sender = sender.clone();
                confirm_delete_contacts(&parent, 1, move || {
                    sender.send(ContactsMsg::DeleteContacts(vec![id])).ok();
                });
            }
        });
    }

    *open.borrow_mut() = Some(OpenEditor::Contact {
        _draft: draft,
        _nav_page: nav_page.clone(),
    });
    nav.view.push(&nav_page);
}

fn push_group_editor(
    nav: &ContactNav,
    group: Option<&ContactGroup>,
    book: &ContactBook,
    sender: &Sender<ContactsMsg>,
    open: &Rc<RefCell<Option<OpenEditor>>>,
) {
    let existing_id = group.map(|g| g.id);
    let draft = Rc::new(RefCell::new(GroupDraft {
        id: existing_id,
        name: group.map(|g| g.name.clone()).unwrap_or_default(),
        member_ids: group.map(|g| g.member_ids.clone()).unwrap_or_default(),
    }));

    let name_row = adw::EntryRow::builder().title(tr!("Name")).build();
    name_row.set_text(group.map(|g| g.name.as_str()).unwrap_or(""));
    let members_group = adw::PreferencesGroup::builder()
        .title(tr!("Members"))
        .build();
    for member_id in group.map(|g| g.member_ids.as_slice()).unwrap_or(&[]) {
        if let Some(contact) = book.contacts.iter().find(|c| c.id == *member_id) {
            let title = if contact.display_name.is_empty() {
                contact.email.clone()
            } else {
                contact.display_name.clone()
            };
            members_group.add(&adw::ActionRow::builder().title(&title).build());
        }
    }

    let save_row = adw::ButtonRow::builder()
        .title(tr!("Save"))
        .sensitive(group.is_some())
        .build();
    let delete_row = adw::ButtonRow::builder()
        .title(tr!("Delete group"))
        .css_classes(["destructive-action"])
        .build();

    let page = adw::PreferencesPage::new();
    let name_group = adw::PreferencesGroup::new();
    name_group.add(&name_row);
    page.add(&name_group);
    page.add(&members_group);
    let save_group = adw::PreferencesGroup::new();
    save_group.add(&save_row);
    page.add(&save_group);
    if existing_id.is_some() {
        let delete_group = adw::PreferencesGroup::new();
        delete_group.add(&delete_row);
        page.add(&delete_group);
    }

    let title = group
        .map(|g| g.name.clone())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Add group".to_string());
    let nav_page = adw::NavigationPage::builder()
        .title(&title)
        .child(&page)
        .build();

    name_row.connect_changed({
        let save_row = save_row.clone();
        move |row| {
            let draft = GroupDraft {
                id: None,
                name: row.text().to_string(),
                member_ids: vec![],
            };
            save_row.set_sensitive(draft.validate().is_ok());
        }
    });

    save_row.connect_activated({
        let sender = sender.clone();
        let draft = draft.clone();
        move |_| {
            let mut value = draft.borrow().clone();
            value.name = name_row.text().to_string();
            if value.validate().is_err() {
                return;
            }
            sender.send(ContactsMsg::UpsertGroup(value)).ok();
        }
    });

    if let Some(id) = existing_id {
        delete_row.connect_activated({
            let sender = sender.clone();
            let parent = nav.parent.clone();
            move |_| {
                let sender = sender.clone();
                let confirm = adw::AlertDialog::builder()
                    .heading(tr!("Delete group?"))
                    .body(tr!("Contacts are not deleted."))
                    .build();
                confirm.add_response("cancel", &tr!("Cancel"));
                confirm.add_response("delete", &tr!("Delete"));
                confirm.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
                confirm.set_close_response("cancel");
                confirm.choose(Some(&parent), None::<&gtk::gio::Cancellable>, {
                    let sender = sender.clone();
                    move |response| {
                        if response.as_str() == "delete" {
                            sender.send(ContactsMsg::DeleteGroup(id)).ok();
                        }
                    }
                });
            }
        });
    }

    *open.borrow_mut() = Some(OpenEditor::Group {
        _draft: draft,
        _nav_page: nav_page.clone(),
    });
    nav.view.push(&nav_page);
}

pub fn mock_contact_book() -> ContactBook {
    use chrono::Utc;
    let now = Utc::now();
    let alice = Contact {
        id: ContactId(2),
        email: "alice@example.com".to_string(),
        display_name: "Alice".to_string(),
        nickname: String::new(),
        organization: String::new(),
        notes: String::new(),
        status: ContactStatus::Approved,
        group_ids: vec![GroupId(3)],
        created_at: now,
        updated_at: now,
        last_used_at: None,
    };
    let bob = Contact {
        id: ContactId(4),
        email: "bob@example.com".to_string(),
        display_name: "Bob".to_string(),
        nickname: String::new(),
        organization: String::new(),
        notes: String::new(),
        status: ContactStatus::Suggested,
        group_ids: vec![UNAPPROVED_GROUP_ID],
        created_at: now,
        updated_at: now,
        last_used_at: None,
    };
    let carol = Contact {
        id: ContactId(5),
        email: "carol@example.com".to_string(),
        display_name: "Carol".to_string(),
        nickname: String::new(),
        organization: String::new(),
        notes: String::new(),
        status: ContactStatus::Approved,
        group_ids: vec![GroupId(3)],
        created_at: now,
        updated_at: now,
        last_used_at: None,
    };
    ContactBook {
        contacts: vec![alice, bob, carol],
        groups: vec![
            ContactGroup {
                id: UNAPPROVED_GROUP_ID,
                name: "Unapproved".to_string(),
                is_system: true,
                member_ids: vec![ContactId(4)],
                created_at: now,
            },
            ContactGroup {
                id: GroupId(3),
                name: "Family".to_string(),
                is_system: false,
                member_ids: vec![ContactId(2), ContactId(5)],
                created_at: now,
            },
        ],
    }
}

/// Initialization for the contacts Relm4 child component.
pub struct ContactsInit {
    pub mock: bool,
    pub cmd: Sender<UiCommand>,
    pub toasts: adw::ToastOverlay,
    pub contact_book: Rc<RefCell<ContactBook>>,
    pub remote_image_senders: Rc<RefCell<HashSet<String>>>,
    pub activity: Rc<ActivityLog>,
}

pub struct ContactsModel {
    mock: bool,
    cmd: Sender<UiCommand>,
    toasts: adw::ToastOverlay,
    activity: Rc<ActivityLog>,
    contact_book: Rc<RefCell<ContactBook>>,
    remote_image_senders: Rc<RefCell<HashSet<String>>>,
    contacts_canvas: ContactsCanvas,
    pending_deleted_contacts: HashSet<ContactId>,
}

#[relm4::component(pub)]
impl Component for ContactsModel {
    type Init = ContactsInit;
    type Input = ContactsMsg;
    type Output = ContactsOutput;
    type CommandOutput = ();

    view! {
        #[root]
        root = gtk::Box {
            set_hexpand: true,
            set_vexpand: true,
        }
    }

    fn init(
        init: Self::Init,
        _root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let widgets = view_output!();
        let contacts_canvas = build_contacts_canvas(
            {
                let sender = sender.clone();
                move || {
                    let _ = sender.output(ContactsOutput::ShowMail);
                }
            },
            sender.input_sender().clone(),
        );
        widgets.root.append(&contacts_canvas.root);

        let model = ContactsModel {
            mock: init.mock,
            cmd: init.cmd,
            toasts: init.toasts,
            activity: init.activity,
            contact_book: init.contact_book,
            remote_image_senders: init.remote_image_senders,
            contacts_canvas,
            pending_deleted_contacts: HashSet::new(),
        };

        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>, _root: &Self::Root) {
        match msg {
            ContactsMsg::Show => {
                self.contacts_canvas.show_list();
            }
            ContactsMsg::RefreshContacts => {
                self.refresh_contacts_ui(&sender);
            }
            ContactsMsg::NewGroup => {
                self.refresh_contacts_ui(&sender);
                let nav = self.contacts_canvas.contact_nav();
                open_new_group(
                    &self.contacts_canvas,
                    &nav,
                    &self.contact_book.borrow(),
                    sender.input_sender(),
                );
            }
            ContactsMsg::UpsertContact(draft) => {
                if self.mock {
                    if self.mock_upsert_contact(draft) {
                        self.refresh_contacts_ui(&sender);
                        self.toasts
                            .add_toast(crate::toast::plain(&tr!("Contact saved")));
                        self.activity.push(activity_log::entry(
                            activity_log::KIND_CONTACTS,
                            ActivityOutcome::Success,
                            tr!("Contact saved"),
                            "",
                        ));
                    }
                } else {
                    self.cmd.emit(UiCommand::UpsertContact(draft));
                }
            }
            ContactsMsg::DeleteContacts(ids) => {
                if ids.is_empty() {
                    return;
                }
                {
                    let mut book = self.contact_book.borrow_mut();
                    let ContactBook {
                        contacts, groups, ..
                    } = &mut *book;
                    remove_contacts_in_memory(contacts, groups, &ids);
                }
                for id in &ids {
                    self.pending_deleted_contacts.insert(*id);
                }
                self.contacts_canvas
                    .exit_selection_mode(sender.input_sender());
                self.contacts_canvas.show_list();
                self.refresh_contacts_ui(&sender);
                let toast = if ids.len() == 1 {
                    tr!("Contact deleted")
                } else {
                    ntr!("{n} contact deleted", "{n} contacts deleted", ids.len())
                };
                self.toasts.add_toast(crate::toast::plain(&toast));
                self.activity.push(activity_log::entry(
                    activity_log::KIND_CONTACTS,
                    ActivityOutcome::Success,
                    toast.clone(),
                    "",
                ));
                if !self.mock {
                    self.cmd.emit(UiCommand::DeleteContacts(ids));
                }
            }
            ContactsMsg::UpsertGroup(draft) => {
                if self.mock {
                    self.mock_upsert_group(draft);
                    self.refresh_contacts_ui(&sender);
                    self.toasts
                        .add_toast(crate::toast::plain(&tr!("Group saved")));
                    self.activity.push(activity_log::entry(
                        activity_log::KIND_CONTACTS,
                        ActivityOutcome::Success,
                        tr!("Group saved"),
                        "",
                    ));
                } else {
                    self.cmd.emit(UiCommand::UpsertGroup(draft));
                }
            }
            ContactsMsg::DeleteGroup(id) => {
                if self.mock {
                    let mut book = self.contact_book.borrow_mut();
                    book.groups.retain(|g| g.id != id);
                    for contact in &mut book.contacts {
                        contact.group_ids.retain(|gid| *gid != id);
                    }
                    self.refresh_contacts_ui(&sender);
                    self.toasts
                        .add_toast(crate::toast::plain(&tr!("Group deleted")));
                    self.activity.push(activity_log::entry(
                        activity_log::KIND_CONTACTS,
                        ActivityOutcome::Success,
                        tr!("Group deleted"),
                        "",
                    ));
                } else {
                    self.cmd.emit(UiCommand::DeleteGroup(id));
                }
            }
            ContactsMsg::PromoteContact(id) => {
                if self.mock {
                    {
                        let mut book = self.contact_book.borrow_mut();
                        let ContactBook {
                            contacts, groups, ..
                        } = &mut *book;
                        promote_contact_in_memory(contacts, groups, id);
                    }
                    self.refresh_contacts_ui(&sender);
                    self.toasts
                        .add_toast(crate::toast::plain(&tr!("Contact saved")));
                    self.activity.push(activity_log::entry(
                        activity_log::KIND_CONTACTS,
                        ActivityOutcome::Success,
                        tr!("Contact saved"),
                        "",
                    ));
                } else {
                    self.cmd.emit(UiCommand::PromoteContact(id));
                }
            }
            ContactsMsg::SetSenderRemoteImages { email, allow } => {
                if let Some(normalized) = normalize_sender_email(&email) {
                    if allow {
                        self.remote_image_senders.borrow_mut().insert(normalized);
                    } else {
                        self.remote_image_senders.borrow_mut().remove(&normalized);
                    }
                    if !self.mock {
                        self.cmd.emit(UiCommand::SetSenderRemoteImages {
                            emails: vec![email],
                            allow,
                        });
                    }
                }
            }
            ContactsMsg::ContactsChanged {
                mut contacts,
                mut groups,
            } => {
                self.pending_deleted_contacts
                    .retain(|id| contacts.iter().any(|c| c.id == *id));
                if !self.pending_deleted_contacts.is_empty() {
                    let pending: Vec<_> = self.pending_deleted_contacts.iter().copied().collect();
                    remove_contacts_in_memory(&mut contacts, &mut groups, &pending);
                }
                *self.contact_book.borrow_mut() = ContactBook { contacts, groups };
                self.refresh_contacts_ui(&sender);
            }
            ContactsMsg::ViewContact(id) => {
                self.refresh_contacts_ui(&sender);
                let nav = self.contacts_canvas.contact_nav();
                open_contact(
                    &self.contacts_canvas,
                    &nav,
                    &self.contact_book.borrow(),
                    id,
                    sender.input_sender(),
                    &self.remote_image_senders.borrow(),
                );
            }
            ContactsMsg::AddContactFromAddress(addr) => {
                if addr.email.is_empty() {
                    let nav = self.contacts_canvas.contact_nav();
                    open_new_contact(
                        &self.contacts_canvas,
                        &nav,
                        sender.input_sender(),
                        &self.remote_image_senders.borrow(),
                    );
                } else {
                    sender.input(ContactsMsg::UpsertContact(ContactDraft {
                        id: None,
                        email: addr.email,
                        display_name: addr.name.unwrap_or_default(),
                        nickname: String::new(),
                        organization: String::new(),
                        notes: String::new(),
                        status: ContactStatus::Approved,
                        group_ids: vec![],
                    }));
                }
            }
        }
    }
}

impl ContactsModel {
    fn refresh_contacts_ui(&self, sender: &ComponentSender<Self>) {
        let nav = self.contacts_canvas.contact_nav();
        refresh_contact_rows(
            &self.contacts_canvas,
            &nav,
            &self.contact_book.borrow(),
            sender.input_sender(),
            &self.remote_image_senders.borrow(),
        );
    }

    fn mock_upsert_contact(&mut self, draft: ContactDraft) -> bool {
        let mut book = self.contact_book.borrow_mut();
        let email = draft.email.to_ascii_lowercase();
        if draft.id.is_none()
            && book
                .contacts
                .iter()
                .any(|c| c.email.eq_ignore_ascii_case(&email))
        {
            drop(book);
            self.toasts
                .add_toast(crate::toast::plain(&tr!("Already in contacts")));
            return false;
        }
        if let Some(id) = draft.id
            && book
                .contacts
                .iter()
                .any(|c| c.id != id && c.email.eq_ignore_ascii_case(&email))
        {
            drop(book);
            self.toasts
                .add_toast(crate::toast::plain(&tr!("Already in contacts")));
            return false;
        }
        if let Some(existing) = book
            .contacts
            .iter()
            .position(|c| draft.id.map(|id| c.id == id).unwrap_or(false))
        {
            let id = book.contacts[existing].id;
            book.contacts[existing] = Contact {
                id,
                email,
                display_name: draft.display_name,
                nickname: draft.nickname,
                organization: draft.organization,
                notes: draft.notes,
                status: draft.status,
                group_ids: draft.group_ids,
                created_at: book.contacts[existing].created_at,
                updated_at: chrono::Utc::now(),
                last_used_at: book.contacts[existing].last_used_at,
            };
        } else {
            let id = ContactId(
                book.contacts
                    .iter()
                    .map(|c| c.id.0)
                    .max()
                    .unwrap_or(1)
                    .saturating_add(1),
            );
            book.contacts.push(Contact {
                id,
                email,
                display_name: draft.display_name,
                nickname: draft.nickname,
                organization: draft.organization,
                notes: draft.notes,
                status: draft.status,
                group_ids: draft.group_ids,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
                last_used_at: None,
            });
        }
        true
    }

    fn mock_upsert_group(&mut self, draft: GroupDraft) {
        if let Err(msg) = draft.validate() {
            self.toasts.add_toast(crate::toast::plain(msg));
            return;
        }
        let mut book = self.contact_book.borrow_mut();
        if let Some(id) = draft.id {
            if let Some(group) = book.groups.iter_mut().find(|g| g.id == id) {
                group.name = draft.name;
                group.member_ids = draft.member_ids;
            }
        } else {
            let id = GroupId(
                book.groups
                    .iter()
                    .map(|g| g.id.0)
                    .max()
                    .unwrap_or(1)
                    .saturating_add(1),
            );
            book.groups.push(ContactGroup {
                id,
                name: draft.name,
                is_system: false,
                member_ids: draft.member_ids,
                created_at: chrono::Utc::now(),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delete_contacts_heading_single_and_multi() {
        assert_eq!(delete_contacts_heading(1), "Delete contact?");
        assert_eq!(delete_contacts_heading(3), "Delete 3 contacts?");
    }

    #[test]
    fn group_name_with_ampersand_is_shown_verbatim() {
        crate::test_helpers::gtk_test_run(|| {
            let group = ContactGroup {
                id: GroupId(1),
                name: "R&D".to_string(),
                is_system: false,
                member_ids: Vec::new(),
                created_at: chrono::DateTime::UNIX_EPOCH,
            };
            let row = group_list_row(&group);
            let texts = crate::test_helpers::displayed_label_texts(&row);
            assert!(texts.iter().any(|text| text == "R&D"), "{texts:?}");
        });
    }
}
