//! Preferences → Keyboard page.

use alcedo_i18n::{tr, trf};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use relm4::adw;
use relm4::adw::prelude::*;
use relm4::gtk;
use relm4::gtk::gdk;
use relm4::gtk::glib;

use crate::commands::{
    CommandCategory, accel_in_use, effective_accel, format_accel_for_display, remappable_commands,
    scope_subtitle,
};
use crate::help::{self, TOPIC_KEYBOARD};
use crate::settings::{self, keyboard_shortcut_overrides};

pub fn build_keyboard_page() -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder()
        .name("keyboard")
        .title(tr!("Keyboard"))
        .icon_name("input-keyboard-symbolic")
        .build();

    let mut groups: HashMap<CommandCategory, adw::PreferencesGroup> = HashMap::new();
    for category in [
        CommandCategory::Mail,
        CommandCategory::GoTo,
        CommandCategory::Compose,
        CommandCategory::Search,
        CommandCategory::View,
    ] {
        let group = adw::PreferencesGroup::builder()
            .title(category.label())
            .build();
        if category == CommandCategory::Mail {
            group.set_description(Some(&tr!("Select a command to change its shortcut.")));
            help::attach_group_help(&group, TOPIC_KEYBOARD);
        }
        page.add(&group);
        groups.insert(category, group);
    }

    let refreshers: Rc<RefCell<Vec<Rc<dyn Fn()>>>> = Rc::new(RefCell::new(Vec::new()));
    let refresh_all = {
        let refreshers = refreshers.clone();
        Rc::new(move || {
            for refresh in refreshers.borrow().iter() {
                refresh();
            }
        })
    };

    for entry in remappable_commands() {
        let Some(group) = groups.get(&entry.category) else {
            continue;
        };
        let subtitle = scope_subtitle(entry.scope);
        let row = adw::ActionRow::builder()
            .title(entry.label)
            .activatable(true)
            .build();
        if !subtitle.is_empty() {
            row.set_subtitle(subtitle);
        }
        let accel_label = gtk::Label::new(None);
        accel_label.add_css_class("dim-label");
        accel_label.add_css_class("numeric");
        accel_label.set_valign(gtk::Align::Center);
        row.add_suffix(&accel_label);

        let refresh_accel = {
            let accel_label = accel_label.clone();
            let entry_id = entry.id;
            Rc::new(move || {
                if let Some(trigger) = effective_accel(entry_id) {
                    accel_label.set_label(&format_accel_for_display(&trigger));
                    accel_label.set_visible(true);
                } else {
                    accel_label.set_visible(false);
                }
            })
        };
        refresh_accel();
        refreshers.borrow_mut().push(refresh_accel);

        row.connect_activated({
            let refresh_all = refresh_all.clone();
            move |row| {
                open_set_shortcut_dialog(entry, row, refresh_all.clone());
            }
        });

        group.add(&row);
    }

    let restore_group = adw::PreferencesGroup::new();
    let restore_btn = adw::ButtonRow::builder()
        .title(tr!("Restore Defaults"))
        .css_classes(["destructive-action"])
        .build();
    restore_group.add(&restore_btn);
    page.add(&restore_group);

    settings::on_keyboard_shortcut_overrides_changed({
        let refresh_all = refresh_all.clone();
        move || refresh_all()
    });

    restore_btn.connect_activated({
        let refresh_all = refresh_all.clone();
        move |btn| {
            let confirm = adw::AlertDialog::builder()
                .heading(tr!("Restore default shortcuts?"))
                .body(tr!("This resets all custom keyboard shortcuts."))
                .build();
            confirm.add_response("cancel", &tr!("Cancel"));
            confirm.add_response("restore", &tr!("Restore"));
            confirm.set_response_appearance("restore", adw::ResponseAppearance::Destructive);
            confirm.set_default_response(Some("cancel"));
            confirm.set_close_response("cancel");
            let refresh_all = refresh_all.clone();
            confirm.connect_response(None, move |_, response| {
                if response == "restore" {
                    settings::clear_keyboard_shortcut_overrides();
                    refresh_all();
                }
            });
            confirm.choose(btn.root().as_ref(), None::<&gtk::gio::Cancellable>, |_| {});
        }
    });

    page
}

fn open_set_shortcut_dialog(
    entry: &crate::commands::CommandDef,
    parent: &impl IsA<gtk::Widget>,
    refresh_all: Rc<dyn Fn()>,
) {
    let capture_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let conflict_label = gtk::Label::new(None);
    conflict_label.add_css_class("error");
    conflict_label.set_wrap(true);
    conflict_label.set_visible(false);
    capture_box.append(&conflict_label);

    let dialog = adw::AlertDialog::builder()
        .heading(tr!("Set shortcut"))
        .body(trf!(
            "Press a new shortcut for {command}. Press Escape to cancel, or Backspace to restore the default.",
            command = entry.label
        ))
        .extra_child(&capture_box)
        .build();
    dialog.add_response("cancel", &tr!("Cancel"));

    let entry_id = entry.id.to_string();
    let key_controller = gtk::EventControllerKey::new();
    key_controller.set_propagation_phase(gtk::PropagationPhase::Capture);
    key_controller.connect_key_pressed({
        let dialog = dialog.clone();
        let conflict_label = conflict_label.clone();
        let refresh_all = refresh_all.clone();
        move |_, key, _, modifiers| {
            let keyval = key;
            if keyval == gdk::Key::Escape {
                dialog.close();
                return glib::Propagation::Stop;
            }
            if keyval == gdk::Key::BackSpace && modifiers.is_empty() {
                let mut overrides = keyboard_shortcut_overrides();
                overrides.remove(&entry_id);
                settings::set_keyboard_shortcut_overrides(&overrides);
                refresh_all();
                dialog.close();
                return glib::Propagation::Stop;
            }
            let trigger = gtk::KeyvalTrigger::new(keyval, modifiers);
            let trigger_string = trigger.to_string();
            if trigger_string.is_empty() {
                return glib::Propagation::Proceed;
            }
            if let Some(other) = accel_in_use(&trigger_string, &entry_id) {
                conflict_label.set_label(&trf!(
                    "That shortcut is already used by {command}.",
                    command = other.label
                ));
                conflict_label.set_visible(true);
                return glib::Propagation::Stop;
            }
            let mut overrides = keyboard_shortcut_overrides();
            overrides.insert(entry_id.clone(), trigger_string);
            settings::set_keyboard_shortcut_overrides(&overrides);
            refresh_all();
            dialog.close();
            glib::Propagation::Stop
        }
    });
    dialog.add_controller(key_controller);

    dialog.choose(Some(parent), None::<&gtk::gio::Cancellable>, |_| {});
}
