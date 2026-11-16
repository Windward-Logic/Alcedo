//! Multi-select recipient picker for compose To/Cc/Bcc fields.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use alcedo_core::{
    Account, AccountId, Address, Contact, ContactGroup, ContactStatus, Envelope, Folder, GroupId,
    SpecialUse,
};
use relm4::adw;
use relm4::adw::prelude::*;
use relm4::gtk;
use relm4::gtk::glib;

use crate::recipient_row::{ContactBook, RecipientToken};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PickerKind {
    All,
    Contacts,
    Sent,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PickerItem {
    Group {
        id: GroupId,
        name: String,
        members: usize,
    },
    Person {
        email: String,
        title: String,
        subtitle: String,
        from_sent: bool,
    },
}

pub fn picker_items(
    book: &ContactBook,
    sent: &[Address],
    query: &str,
    kind: PickerKind,
) -> Vec<PickerItem> {
    let q = query.trim().to_ascii_lowercase();

    match kind {
        PickerKind::Contacts => contact_picker_items(book, &q),
        PickerKind::Sent => sent_picker_items(sent, &q),
        PickerKind::All => {
            let contacts = contact_picker_items(book, &q);
            let approved_emails: HashSet<String> = book
                .contacts
                .iter()
                .filter(|c| c.status == ContactStatus::Approved)
                .map(|c| c.email.to_ascii_lowercase())
                .collect();
            let mut items = contacts;
            for addr in sent {
                let email = addr.email.to_ascii_lowercase();
                if approved_emails.contains(&email) {
                    continue;
                }
                if !sent_person_matches(addr, &q) {
                    continue;
                }
                items.push(person_item_from_address(addr, true));
            }
            items
        }
    }
}

fn contact_picker_items(book: &ContactBook, q: &str) -> Vec<PickerItem> {
    let mut items = Vec::new();

    let mut groups: Vec<_> = book
        .groups
        .iter()
        .filter(|g| !g.is_system && group_matches(g, book, q))
        .collect();
    groups.sort_by(|a, b| {
        a.name
            .to_ascii_lowercase()
            .cmp(&b.name.to_ascii_lowercase())
    });
    for group in groups {
        items.push(PickerItem::Group {
            id: group.id,
            name: group.name.clone(),
            members: group.member_ids.len(),
        });
    }

    let mut people: Vec<_> = book
        .contacts
        .iter()
        .filter(|c| c.status == ContactStatus::Approved && contact_matches(c, q))
        .collect();
    people.sort_by(|a, b| {
        let a_name = person_sort_key(a);
        let b_name = person_sort_key(b);
        a_name.cmp(&b_name)
    });
    for contact in people {
        items.push(person_item_from_contact(contact));
    }

    items
}

fn sent_picker_items(sent: &[Address], q: &str) -> Vec<PickerItem> {
    sent.iter()
        .filter(|addr| sent_person_matches(addr, q))
        .map(|addr| person_item_from_address(addr, false))
        .collect()
}

fn person_sort_key(contact: &Contact) -> String {
    if contact.display_name.is_empty() {
        contact.email.to_ascii_lowercase()
    } else {
        contact.display_name.to_ascii_lowercase()
    }
}

fn contact_matches(contact: &Contact, q: &str) -> bool {
    if q.is_empty() {
        return true;
    }
    contact.email.to_ascii_lowercase().contains(q)
        || contact.display_name.to_ascii_lowercase().contains(q)
        || contact.nickname.to_ascii_lowercase().contains(q)
}

fn group_matches(group: &ContactGroup, book: &ContactBook, q: &str) -> bool {
    if q.is_empty() {
        return true;
    }
    if group.name.to_ascii_lowercase().contains(q) {
        return true;
    }
    for contact_id in &group.member_ids {
        if let Some(contact) = book.contacts.iter().find(|c| c.id == *contact_id)
            && contact_matches(contact, q)
        {
            return true;
        }
    }
    false
}

pub(crate) fn sent_person_matches(addr: &Address, q: &str) -> bool {
    if q.is_empty() {
        return true;
    }
    addr.email.to_ascii_lowercase().contains(q)
        || addr.display_name().to_ascii_lowercase().contains(q)
}

fn person_item_from_contact(contact: &Contact) -> PickerItem {
    let title = if contact.display_name.is_empty() {
        contact.email.clone()
    } else {
        contact.display_name.clone()
    };
    PickerItem::Person {
        email: contact.email.clone(),
        title,
        subtitle: contact.email.clone(),
        from_sent: false,
    }
}

fn person_item_from_address(addr: &Address, from_sent: bool) -> PickerItem {
    let title = addr.display_name();
    PickerItem::Person {
        email: addr.email.clone(),
        title,
        subtitle: addr.email.clone(),
        from_sent,
    }
}

/// Build sent-recipient cache from mock envelopes (matches store scan semantics).
pub fn mock_sent_recipients(
    envelopes: &[Envelope],
    folders: &HashMap<AccountId, Vec<Folder>>,
    accounts: &[Account],
) -> Vec<Address> {
    let sent_folder_ids: HashSet<_> = folders
        .values()
        .flatten()
        .filter(|f| f.special == SpecialUse::Sent)
        .map(|f| f.id)
        .collect();
    let account_emails: HashMap<AccountId, String> =
        accounts.iter().map(|a| (a.id, a.email.clone())).collect();

    let mut sent_envs: Vec<&Envelope> = envelopes
        .iter()
        .filter(|e| sent_folder_ids.contains(&e.folder_id))
        .collect();
    sent_envs.sort_by(|a, b| b.date.cmp(&a.date));

    let mut seen: HashSet<String> = HashSet::new();
    let mut result = Vec::new();
    for env in sent_envs {
        let account_email = account_emails
            .get(&env.account_id)
            .map(String::as_str)
            .unwrap_or("");
        for addr in env.to.iter().chain(env.cc.iter()) {
            if addr.email.is_empty() {
                continue;
            }
            if addr.email.eq_ignore_ascii_case(account_email) {
                continue;
            }
            let key = addr.email.to_ascii_lowercase();
            if seen.insert(key) {
                result.push(addr.clone());
            }
        }
    }
    result
}

pub fn present_recipient_picker(
    parent: &impl IsA<gtk::Widget>,
    field_title: &str,
    book: &ContactBook,
    sent: &[Address],
    already: &[RecipientToken],
    on_add: impl Fn(Vec<RecipientToken>) + 'static,
) {
    let dialog_title = format!("Add to {field_title}");
    let cancel_btn = gtk::Button::with_label("Cancel");
    let add_btn = gtk::Button::builder()
        .label("Add")
        .css_classes(["suggested-action"])
        .sensitive(false)
        .build();

    let search = gtk::SearchEntry::builder()
        .placeholder_text("Search contacts and addresses")
        .hexpand(true)
        .build();
    search.set_activates_default(true);

    let kind_group = adw::ToggleGroup::builder().homogeneous(true).build();
    kind_group.add(
        adw::Toggle::builder()
            .label("All")
            .tooltip("All addresses")
            .build(),
    );
    kind_group.add(
        adw::Toggle::builder()
            .label("Contacts")
            .tooltip("Saved contacts and groups")
            .build(),
    );
    kind_group.add(
        adw::Toggle::builder()
            .label("Sent")
            .tooltip("Addresses from sent messages")
            .build(),
    );
    kind_group.set_active(0);

    let listbox = gtk::ListBox::new();
    listbox.set_selection_mode(gtk::SelectionMode::None);

    let empty_label = gtk::Label::builder()
        .label("")
        .css_classes(["dim-label"])
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .justify(gtk::Justification::Center)
        .wrap(true)
        .build();

    let empty_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
    empty_box.set_halign(gtk::Align::Center);
    empty_box.set_valign(gtk::Align::Center);
    empty_box.set_vexpand(true);
    let empty_icon = gtk::Image::from_icon_name("avatar-default-symbolic");
    empty_icon.add_css_class("alcedo-picker-empty");
    empty_icon.set_halign(gtk::Align::Center);
    empty_box.append(&empty_icon);
    empty_box.append(&empty_label);

    let scroller = gtk::ScrolledWindow::builder()
        .min_content_height(280)
        .vexpand(true)
        .child(&listbox)
        .build();

    let content_stack = gtk::Stack::new();
    content_stack.set_vexpand(true);
    content_stack.add_named(&scroller, Some("list"));
    content_stack.add_named(&empty_box, Some("empty"));

    let body = gtk::Box::new(gtk::Orientation::Vertical, 12);
    body.set_margin_top(12);
    body.set_margin_bottom(12);
    body.set_margin_start(12);
    body.set_margin_end(12);
    body.append(&search);
    body.append(&kind_group);
    body.append(&content_stack);

    let header = adw::HeaderBar::new();
    header.set_show_start_title_buttons(false);
    header.set_show_end_title_buttons(false);
    let title_label = gtk::Label::builder()
        .label(&dialog_title)
        .css_classes(["title"])
        .build();
    header.set_title_widget(Some(&title_label));
    header.pack_start(&cancel_btn);
    header.pack_end(&add_btn);

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&body));

    let dialog = adw::Dialog::builder()
        .title(&dialog_title)
        .content_width(440)
        .content_height(520)
        .default_widget(&add_btn)
        .focus_widget(&search)
        .child(&toolbar)
        .build();

    let book = book.clone();
    let sent = sent.to_vec();
    let already_tokens = already.to_vec();

    let kind: Rc<RefCell<PickerKind>> = Rc::new(RefCell::new(PickerKind::All));
    let selected_people: Rc<RefCell<HashSet<String>>> = Rc::new(RefCell::new(HashSet::new()));
    let selected_groups: Rc<RefCell<HashSet<GroupId>>> = Rc::new(RefCell::new(HashSet::new()));

    let update_add_btn = {
        let add_btn = add_btn.clone();
        let selected_people = selected_people.clone();
        let selected_groups = selected_groups.clone();
        let already_tokens = already_tokens.clone();
        Rc::new(move || {
            let n = count_new_selections(
                &selected_people.borrow(),
                &selected_groups.borrow(),
                &already_tokens,
            );
            if n == 0 {
                add_btn.set_label("Add");
                add_btn.set_sensitive(false);
            } else {
                add_btn.set_label(&format!("Add ({n})"));
                add_btn.set_sensitive(true);
            }
        })
    };

    let rebuild = {
        let listbox = listbox.clone();
        let content_stack = content_stack.clone();
        let empty_label = empty_label.clone();
        let search = search.clone();
        let kind = kind.clone();
        let book = book.clone();
        let sent = sent.clone();
        let already_tokens = already_tokens.clone();
        let selected_people = selected_people.clone();
        let selected_groups = selected_groups.clone();
        let update_add_btn = update_add_btn.clone();
        Rc::new(move || {
            let query = search.text().to_string();
            let picker_kind = *kind.borrow();
            let items = picker_items(&book, &sent, &query, picker_kind);

            while let Some(row) = listbox.row_at_index(0) {
                listbox.remove(&row);
            }

            if items.is_empty() {
                let message = if !query.trim().is_empty() {
                    "No matching addresses"
                } else if picker_kind == PickerKind::Sent {
                    "No addresses in Sent yet"
                } else {
                    "No contacts yet. Add someone or send a message."
                };
                empty_label.set_label(message);
                content_stack.set_visible_child_name("empty");
            } else {
                content_stack.set_visible_child_name("list");
                for item in &items {
                    let (title, subtitle) = match item {
                        PickerItem::Group { name, members, .. } => {
                            let subtitle = if *members == 1 {
                                "1 member".to_string()
                            } else {
                                format!("{members} members")
                            };
                            (name.clone(), subtitle)
                        }
                        PickerItem::Person {
                            title,
                            subtitle: sub,
                            from_sent,
                            ..
                        } => {
                            let subtitle = if *from_sent && picker_kind == PickerKind::All {
                                format!("{sub} · Sent")
                            } else {
                                sub.clone()
                            };
                            (title.clone(), subtitle)
                        }
                    };

                    let row = adw::ActionRow::builder()
                        .use_markup(false)
                        .title(&title)
                        .subtitle(&subtitle)
                        .build();
                    match item {
                        PickerItem::Group { .. } => {
                            let badge = gtk::Box::new(gtk::Orientation::Horizontal, 0);
                            badge.add_css_class("alcedo-picker-group-icon");
                            badge.set_halign(gtk::Align::Center);
                            badge.set_valign(gtk::Align::Center);
                            let icon = gtk::Image::from_icon_name("system-users-symbolic");
                            icon.set_halign(gtk::Align::Center);
                            icon.set_valign(gtk::Align::Center);
                            badge.append(&icon);
                            row.add_prefix(&badge);
                        }
                        PickerItem::Person { .. } => {
                            let avatar = adw::Avatar::builder()
                                .size(32)
                                .show_initials(true)
                                .text(&title)
                                .build();
                            avatar.set_can_focus(false);
                            row.add_prefix(&avatar);
                        }
                    }
                    let check = gtk::CheckButton::builder()
                        .valign(gtk::Align::Center)
                        .build();
                    row.add_suffix(&check);

                    let already_in_field = match item {
                        PickerItem::Group { id, .. } => already_tokens.iter().any(
                            |t| matches!(t, RecipientToken::Group { id: gid, .. } if *gid == *id),
                        ),
                        PickerItem::Person { email, .. } => already_tokens.iter().any(|t| {
                            matches!(t, RecipientToken::Person { email: e, .. }
                                if e.eq_ignore_ascii_case(email))
                        }),
                    };

                    if already_in_field {
                        check.set_active(true);
                        check.set_sensitive(false);
                        row.set_activatable(false);
                    } else {
                        let active = match item {
                            PickerItem::Group { id, .. } => selected_groups.borrow().contains(id),
                            PickerItem::Person { email, .. } => selected_people
                                .borrow()
                                .contains(&email.to_ascii_lowercase()),
                        };
                        check.set_active(active);
                        check.set_sensitive(true);

                        let selected_people = selected_people.clone();
                        let selected_groups = selected_groups.clone();
                        let update_add_btn = update_add_btn.clone();
                        match item {
                            PickerItem::Group { id, .. } => {
                                let group_id = *id;
                                check.connect_toggled(move |btn| {
                                    if !btn.is_sensitive() {
                                        return;
                                    }
                                    if btn.is_active() {
                                        selected_groups.borrow_mut().insert(group_id);
                                    } else {
                                        selected_groups.borrow_mut().remove(&group_id);
                                    }
                                    update_add_btn();
                                });
                            }
                            PickerItem::Person { email, .. } => {
                                let key = email.to_ascii_lowercase();
                                check.connect_toggled(move |btn| {
                                    if !btn.is_sensitive() {
                                        return;
                                    }
                                    if btn.is_active() {
                                        selected_people.borrow_mut().insert(key.clone());
                                    } else {
                                        selected_people.borrow_mut().remove(&key);
                                    }
                                    update_add_btn();
                                });
                            }
                        }
                        row.set_activatable_widget(Some(&check));
                    }

                    listbox.append(&row);
                }
            }

            update_add_btn();
        })
    };

    let rebuild_for_search = rebuild.clone();
    search.connect_search_changed(move |_| rebuild_for_search());

    kind_group.connect_active_notify({
        let kind = kind.clone();
        let rebuild = rebuild.clone();
        move |group| {
            let next = match group.active() {
                1 => PickerKind::Contacts,
                2 => PickerKind::Sent,
                _ => PickerKind::All,
            };
            let mut k = kind.borrow_mut();
            if *k != next {
                *k = next;
                drop(k);
                rebuild();
            }
        }
    });

    rebuild();

    let escape_controller = gtk::EventControllerKey::new();
    escape_controller.set_propagation_phase(gtk::PropagationPhase::Capture);
    escape_controller.connect_key_pressed({
        let dialog = dialog.clone();
        let search = search.clone();
        move |_, key, _, _| {
            if key == gtk::gdk::Key::Escape {
                if search.text().is_empty() {
                    dialog.close();
                } else {
                    search.set_text("");
                }
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        }
    });
    dialog.add_controller(escape_controller);

    cancel_btn.connect_clicked({
        let dialog = dialog.clone();
        move |_| {
            dialog.close();
        }
    });

    add_btn.connect_clicked({
        let dialog = dialog.clone();
        let selected_people = selected_people.clone();
        let selected_groups = selected_groups.clone();
        let book = book.clone();
        let sent = sent.clone();
        move |_| {
            let mut tokens = Vec::new();
            for email in selected_people.borrow().iter() {
                if already_has_person(&already_tokens, email) {
                    continue;
                }
                tokens.push(RecipientToken::Person {
                    email: email.clone(),
                    name: person_name_for_add(&book, &sent, email),
                });
            }
            for id in selected_groups.borrow().iter() {
                if already_has_group(&already_tokens, *id) {
                    continue;
                }
                if let Some(group) = book.groups.iter().find(|g| g.id == *id) {
                    tokens.push(RecipientToken::Group {
                        id: *id,
                        name: group.name.clone(),
                    });
                }
            }
            on_add(tokens);
            dialog.close();
        }
    });

    dialog.present(Some(parent));
}

fn count_new_selections(
    selected_people: &HashSet<String>,
    selected_groups: &HashSet<GroupId>,
    already: &[RecipientToken],
) -> usize {
    selected_people
        .iter()
        .filter(|e| !already_has_person(already, e))
        .count()
        + selected_groups
            .iter()
            .filter(|id| !already_has_group(already, **id))
            .count()
}

fn person_name_for_add(book: &ContactBook, sent: &[Address], email: &str) -> Option<String> {
    if let Some(contact) = book
        .contacts
        .iter()
        .find(|c| c.email.eq_ignore_ascii_case(email))
        && !contact.display_name.is_empty()
    {
        return Some(contact.display_name.clone());
    }
    sent.iter()
        .find(|addr| addr.email.eq_ignore_ascii_case(email))
        .and_then(|addr| addr.name.clone())
        .filter(|name| !name.is_empty())
}

fn already_has_person(already: &[RecipientToken], email: &str) -> bool {
    already.iter().any(
        |t| matches!(t, RecipientToken::Person { email: e, .. } if e.eq_ignore_ascii_case(email)),
    )
}

fn already_has_group(already: &[RecipientToken], id: GroupId) -> bool {
    already
        .iter()
        .any(|t| matches!(t, RecipientToken::Group { id: gid, .. } if *gid == id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alcedo_core::{ContactId, UNAPPROVED_GROUP_ID};
    use chrono::Utc;

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
        let bob = Contact {
            id: ContactId(4),
            email: "bob@example.com".to_string(),
            display_name: "Bob".to_string(),
            nickname: String::new(),
            organization: String::new(),
            notes: String::new(),
            status: ContactStatus::Suggested,
            group_ids: vec![UNAPPROVED_GROUP_ID],
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
            contacts: vec![alice, bob, carol],
            groups: vec![
                ContactGroup {
                    id: UNAPPROVED_GROUP_ID,
                    name: "Unapproved".to_string(),
                    is_system: true,
                    member_ids: vec![ContactId(4)],
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

    fn sample_sent() -> Vec<Address> {
        vec![
            Address {
                name: Some("Jordan Lee".to_string()),
                email: "jordan@example.com".to_string(),
            },
            Address {
                name: Some("Alice".to_string()),
                email: "alice@example.com".to_string(),
            },
            Address {
                name: Some("Priya Shah".to_string()),
                email: "priya@company.io".to_string(),
            },
        ]
    }

    fn item_emails(items: &[PickerItem]) -> Vec<String> {
        items
            .iter()
            .filter_map(|item| match item {
                PickerItem::Person { email, .. } => Some(email.clone()),
                _ => None,
            })
            .collect()
    }

    fn item_group_names(items: &[PickerItem]) -> Vec<String> {
        items
            .iter()
            .filter_map(|item| match item {
                PickerItem::Group { name, .. } => Some(name.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn contacts_tab_lists_approved_only() {
        let book = sample_book();
        let items = picker_items(&book, &sample_sent(), "", PickerKind::Contacts);
        assert_eq!(item_group_names(&items), vec!["Family".to_string()]);
        let emails = item_emails(&items);
        assert!(emails.contains(&"alice@example.com".to_string()));
        assert!(emails.contains(&"carol@example.com".to_string()));
        assert!(!emails.contains(&"jordan@example.com".to_string()));
        assert!(!emails.contains(&"priya@company.io".to_string()));
        assert!(!emails.contains(&"bob@example.com".to_string()));
    }

    #[test]
    fn sent_tab_keeps_order() {
        let book = sample_book();
        let items = picker_items(&book, &sample_sent(), "", PickerKind::Sent);
        assert_eq!(
            item_emails(&items),
            vec![
                "jordan@example.com".to_string(),
                "alice@example.com".to_string(),
                "priya@company.io".to_string(),
            ]
        );
    }

    #[test]
    fn all_tab_dedupes_contacts() {
        let book = sample_book();
        let items = picker_items(&book, &sample_sent(), "", PickerKind::All);
        assert_eq!(item_group_names(&items), vec!["Family".to_string()]);
        let emails = item_emails(&items);
        assert!(emails.contains(&"alice@example.com".to_string()));
        assert!(emails.contains(&"carol@example.com".to_string()));
        assert!(emails.contains(&"jordan@example.com".to_string()));
        assert!(emails.contains(&"priya@company.io".to_string()));
        assert_eq!(
            emails.iter().filter(|e| *e == "alice@example.com").count(),
            1
        );
    }

    #[test]
    fn query_fam_matches_family_group() {
        let book = sample_book();
        let items = picker_items(&book, &sample_sent(), "fam", PickerKind::Contacts);
        assert_eq!(item_group_names(&items), vec!["Family".to_string()]);
    }

    #[test]
    fn query_jord_matches_sent() {
        let book = sample_book();
        let sent = picker_items(&book, &sample_sent(), "jord", PickerKind::Sent);
        assert_eq!(item_emails(&sent), vec!["jordan@example.com".to_string()]);
        let all = picker_items(&book, &sample_sent(), "jord", PickerKind::All);
        assert_eq!(item_emails(&all), vec!["jordan@example.com".to_string()]);
        let contacts = picker_items(&book, &sample_sent(), "jord", PickerKind::Contacts);
        assert!(contacts.is_empty());
    }

    #[test]
    fn query_alice_matches_person_and_family() {
        let book = sample_book();
        let contacts = picker_items(&book, &sample_sent(), "alice", PickerKind::Contacts);
        assert_eq!(item_group_names(&contacts), vec!["Family".to_string()]);
        assert!(item_emails(&contacts).contains(&"alice@example.com".to_string()));
        let sent = picker_items(&book, &sample_sent(), "alice", PickerKind::Sent);
        assert_eq!(item_emails(&sent), vec!["alice@example.com".to_string()]);
    }

    #[test]
    fn add_uses_sent_display_name_when_not_a_contact() {
        let book = sample_book();
        let sent = sample_sent();
        assert_eq!(
            person_name_for_add(&book, &sent, "jordan@example.com").as_deref(),
            Some("Jordan Lee")
        );
        assert_eq!(
            person_name_for_add(&book, &sent, "alice@example.com").as_deref(),
            Some("Alice")
        );
    }

    #[test]
    fn count_new_selections_zero_when_all_already_in_field() {
        let already = vec![
            RecipientToken::Person {
                email: "alice@example.com".to_string(),
                name: Some("Alice".to_string()),
            },
            RecipientToken::Group {
                id: GroupId(2),
                name: "Family".to_string(),
            },
        ];
        let mut people = HashSet::new();
        people.insert("alice@example.com".to_string());
        let mut groups = HashSet::new();
        groups.insert(GroupId(2));
        assert_eq!(count_new_selections(&people, &groups, &already), 0);
    }

    #[test]
    fn count_new_selections_counts_people_and_groups() {
        let already = vec![RecipientToken::Person {
            email: "alice@example.com".to_string(),
            name: Some("Alice".to_string()),
        }];
        let mut people = HashSet::new();
        people.insert("alice@example.com".to_string());
        people.insert("carol@example.com".to_string());
        let mut groups = HashSet::new();
        groups.insert(GroupId(2));
        assert_eq!(count_new_selections(&people, &groups, &already), 2);
    }

    #[test]
    fn count_new_selections_ignores_already_added_case_insensitively() {
        let already = vec![RecipientToken::Person {
            email: "Alice@example.com".to_string(),
            name: Some("Alice".to_string()),
        }];
        let mut people = HashSet::new();
        people.insert("alice@example.com".to_string());
        assert_eq!(
            count_new_selections(&people, &groups_default(), &already),
            0
        );
    }

    fn groups_default() -> HashSet<GroupId> {
        HashSet::new()
    }
}
