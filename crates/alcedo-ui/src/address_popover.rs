//! Reader address chip popover (add / view / send / copy).

use alcedo_i18n::{tr, trs};
use std::rc::Rc;

use alcedo_core::{Address, ContactDraft, ContactStatus};
use relm4::Sender;
use relm4::adw;
use relm4::adw::prelude::*;
use relm4::gtk;

use crate::app::AppMsg;
use crate::compose_attachments::show_composer_toast;
use crate::notifications::copy_text_to_clipboard;
use crate::recipient_row::ContactBook;

/// Right-click menu to copy name, formatted address, or email from a chip.
pub fn attach_address_copy_context_menu(
    anchor: &impl IsA<gtk::Widget>,
    addr: Address,
    toast_host: &gtk::Widget,
) {
    let menu = gtk::gio::Menu::new();
    menu.append(Some(&tr!("Copy Name")), Some("addrchip.copy-name"));
    menu.append(
        Some(&tr!("Copy Name and Email")),
        Some("addrchip.copy-formatted"),
    );
    menu.append(Some(&tr!("Copy Email")), Some("addrchip.copy-email"));

    let name = addr.display_name();
    let formatted = addr.formatted();
    let email = addr.email.clone();
    // Weak: these actions are owned by `anchor`, and `toast_host` is usually
    // one of its ancestors (the reader box, which holds the WebView).
    let toast_host = toast_host.downgrade();
    let show_toast = move |message: &str| {
        if let Some(toast_host) = toast_host.upgrade() {
            show_composer_toast(&toast_host, message);
        }
    };

    let actions = gtk::gio::SimpleActionGroup::new();

    let copy_name = gtk::gio::SimpleAction::new("copy-name", None);
    copy_name.connect_activate({
        let name = name.clone();
        let show_toast = show_toast.clone();
        move |_, _| {
            copy_text_to_clipboard(&name);
            show_toast(&tr!("Name copied"));
        }
    });
    actions.add_action(&copy_name);

    let copy_formatted = gtk::gio::SimpleAction::new("copy-formatted", None);
    copy_formatted.connect_activate({
        let formatted = formatted.clone();
        let show_toast = show_toast.clone();
        move |_, _| {
            copy_text_to_clipboard(&formatted);
            show_toast(&tr!("Name and email copied"));
        }
    });
    actions.add_action(&copy_formatted);

    let copy_email = gtk::gio::SimpleAction::new("copy-email", None);
    copy_email.connect_activate({
        let email = email.clone();
        let show_toast = show_toast.clone();
        move |_, _| {
            copy_text_to_clipboard(&email);
            show_toast(&tr!("Email copied"));
        }
    });
    actions.add_action(&copy_email);

    anchor.insert_action_group("addrchip", Some(&actions));

    let popover = gtk::PopoverMenu::from_model(Some(&menu));
    popover.set_has_arrow(false);
    popover.set_parent(anchor);
    alcedo_widgets::unparent_popover_on_destroy(anchor, &popover);

    let gesture = gtk::GestureClick::new();
    gesture.set_button(3);
    gesture.connect_pressed(move |gesture, _, x, y| {
        gesture.set_state(gtk::EventSequenceState::Claimed);
        let rect = gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1);
        popover.set_pointing_to(Some(&rect));
        popover.popup();
    });
    anchor.add_controller(gesture);
}

pub fn show_address_popover(
    anchor: &impl IsA<gtk::Widget>,
    addr: Address,
    book: &ContactBook,
    sender_allowed: bool,
    sender: Sender<AppMsg>,
    on_address_action: Rc<dyn Fn(usize, String)>,
) {
    let existing = book
        .contacts
        .iter()
        .find(|c| c.email.eq_ignore_ascii_case(&addr.email));

    let popover = gtk::Popover::builder().autohide(true).build();
    popover.set_parent(anchor);

    let box_ = gtk::Box::new(gtk::Orientation::Vertical, 12);
    box_.set_margin_top(12);
    box_.set_margin_bottom(12);
    box_.set_margin_start(12);
    box_.set_margin_end(12);

    let heading = gtk::Label::builder()
        .label(addr.display_name())
        .css_classes(["title-4"])
        .xalign(0.0)
        .wrap(true)
        .max_width_chars(36)
        .build();
    box_.append(&heading);
    append_copyable_email(&box_, &addr.email);

    let send_btn = gtk::Button::with_label(&tr!("Send message"));
    send_btn.connect_clicked({
        let email = addr.email.clone();
        let sender = sender.clone();
        let popover = popover.clone();
        move |_| {
            popover.popdown();
            sender.send(AppMsg::ComposeMailto(email.clone())).ok();
        }
    });
    box_.append(&send_btn);

    let remote_label = if sender_allowed {
        trs!("Stop loading remote images")
    } else {
        trs!("Always load remote images")
    };
    let remote_btn = gtk::Button::with_label(remote_label);
    remote_btn.connect_clicked({
        let email = addr.email.clone();
        let sender = sender.clone();
        let popover = popover.clone();
        move |_| {
            popover.popdown();
            sender
                .send(AppMsg::SetSenderRemoteImages {
                    email: email.clone(),
                    allow: !sender_allowed,
                })
                .ok();
        }
    });
    box_.append(&remote_btn);

    append_address_action_buttons(&box_, &popover, &addr.email, &on_address_action);

    match existing {
        Some(contact) if contact.status == ContactStatus::Approved => {
            let view_btn = gtk::Button::with_label(&tr!("View contact"));
            view_btn.connect_clicked({
                let id = contact.id;
                let sender = sender.clone();
                let popover = popover.clone();
                move |_| {
                    popover.popdown();
                    sender.send(AppMsg::ViewContact(id)).ok();
                }
            });
            box_.append(&view_btn);
        }
        _ => {
            let add_btn = gtk::Button::with_label(&tr!("Add to Contacts"));
            add_btn.connect_clicked({
                let addr = addr.clone();
                let sender = sender.clone();
                let popover = popover.clone();
                let anchor = anchor.clone();
                move |_| {
                    popover.popdown();
                    prompt_add_contact(&anchor, &addr, sender.clone());
                }
            });
            box_.append(&add_btn);
        }
    }

    popover.set_child(Some(&box_));
    popover.popup();
}

/// Compact name + copy-email popover for compose recipient chips.
pub fn show_recipient_chip_popover(
    anchor: &impl IsA<gtk::Widget>,
    addr: Address,
    on_address_action: Rc<dyn Fn(usize, String)>,
) {
    let popover = gtk::Popover::builder().autohide(true).build();
    popover.set_parent(anchor);

    let box_ = gtk::Box::new(gtk::Orientation::Vertical, 8);
    box_.set_margin_top(12);
    box_.set_margin_bottom(12);
    box_.set_margin_start(12);
    box_.set_margin_end(12);

    let heading = gtk::Label::builder()
        .label(addr.display_name())
        .css_classes(["title-4"])
        .xalign(0.0)
        .wrap(true)
        .max_width_chars(36)
        .build();
    box_.append(&heading);
    append_copyable_email(&box_, &addr.email);

    append_address_action_buttons(&box_, &popover, &addr.email, &on_address_action);

    popover.set_child(Some(&box_));
    popover.popup();
}

fn append_copyable_email(parent: &gtk::Box, email: &str) {
    let row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .css_classes(["alcedo-addr-email-row"])
        .build();

    let email_label = gtk::Label::builder()
        .label(email)
        .css_classes(["dim-label", "alcedo-addr-email"])
        .xalign(0.0)
        .hexpand(true)
        .selectable(true)
        .wrap(true)
        .max_width_chars(36)
        .build();

    let copy_btn = gtk::Button::builder()
        .icon_name("edit-copy-symbolic")
        .tooltip_text(tr!("Copy email address"))
        .valign(gtk::Align::Start)
        .css_classes(["flat", "circular", "alcedo-addr-copy"])
        .build();
    let email = email.to_string();
    copy_btn.connect_clicked(move |btn| {
        btn.clipboard().set_text(&email);
        show_composer_toast(btn.upcast_ref(), &tr!("Email copied"));
    });

    row.append(&email_label);
    row.append(&copy_btn);
    parent.append(&row);
}

fn prompt_add_contact(anchor: &impl IsA<gtk::Widget>, addr: &Address, sender: Sender<AppMsg>) {
    let dialog = adw::AlertDialog::builder()
        .heading(tr!("Add contact"))
        .build();

    let name_row = adw::EntryRow::builder().title(tr!("Name")).build();
    if let Some(name) = &addr.name {
        name_row.set_text(name);
    }
    let email_row = adw::EntryRow::builder()
        .title(tr!("Email"))
        .editable(false)
        .build();
    email_row.set_text(&addr.email);

    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.append(&name_row);
    content.append(&email_row);
    dialog.set_extra_child(Some(&content));

    dialog.add_response("cancel", &tr!("Cancel"));
    dialog.add_response("add", &tr!("Add"));
    dialog.set_default_response(Some("add"));
    dialog.set_close_response("cancel");

    let addr = addr.clone();
    dialog.choose(
        Some(anchor),
        None::<&gtk::gio::Cancellable>,
        move |response| {
            if response.as_str() != "add" {
                return;
            }
            sender
                .send(AppMsg::UpsertContact(ContactDraft {
                    id: None,
                    email: addr.email.clone(),
                    display_name: name_row.text().to_string(),
                    nickname: String::new(),
                    organization: String::new(),
                    notes: String::new(),
                    status: ContactStatus::Approved,
                    group_ids: vec![],
                }))
                .ok();
        },
    );
}

/// One button per extension address action, each closing the popover first.
fn append_address_action_buttons(
    box_: &gtk::Box,
    popover: &gtk::Popover,
    email: &str,
    on_address_action: &Rc<dyn Fn(usize, String)>,
) {
    for (index, action) in crate::extension::address_actions().iter().enumerate() {
        let button = gtk::Button::with_label(action.label);
        button.connect_clicked({
            let email = email.to_string();
            let on_address_action = on_address_action.clone();
            let popover = popover.clone();
            move |_| {
                popover.popdown();
                on_address_action(index, email.clone());
            }
        });
        box_.append(&button);
    }
}
