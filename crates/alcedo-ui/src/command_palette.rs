//! Modal command palette (`adw::Dialog`).

use alcedo_i18n::{tr, trf};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use alcedo_core::{Folder, FolderId};
use relm4::adw;
use relm4::adw::prelude::*;
use relm4::gtk;
use relm4::gtk::gdk;
use relm4::gtk::glib;

use crate::commands::{
    CommandCategory, CommandDef, command_row_sensitive, effective_accel, palette_static_commands,
};
use crate::folder_bar::{folder_display_name, folder_icon_name};

const PALETTE_NAME: &str = "alcedo-command-palette";
const MAX_ROWS: usize = 50;

#[derive(Clone, Debug)]
pub struct CommandPaletteSpec {
    pub folders: Vec<(String, Vec<Folder>)>,
    pub contacts: Vec<(String, String)>,
}

#[derive(Clone, Debug)]
pub enum PaletteActivation {
    Gio(&'static str),
    GoFolder(FolderId),
    ComposeTo(String),
    InsertSearch(&'static str),
}

struct PaletteRow {
    label: String,
    subtitle: String,
    icon: &'static str,
    accel: Option<String>,
    sensitive: bool,
    activation: PaletteActivation,
}

thread_local! {
    static OPEN_PALETTE: RefCell<Option<adw::Dialog>> = const { RefCell::new(None) };
}

pub fn is_palette_open() -> bool {
    OPEN_PALETTE.with(|slot| slot.borrow().is_some())
}

pub fn close_palette_if_open() {
    let dialog = OPEN_PALETTE.with(|slot| slot.borrow().clone());
    if let Some(dialog) = dialog {
        dialog.close();
    }
}

fn wire_dimming_dismiss(dialog: &adw::Dialog) {
    fn find_dimming(widget: &gtk::Widget) -> Option<gtk::Widget> {
        if widget.css_name().as_str() == "dimming" {
            return Some(widget.clone());
        }
        let mut child = widget.first_child();
        while let Some(node) = child {
            if let Some(found) = find_dimming(&node) {
                return Some(found);
            }
            child = node.next_sibling();
        }
        None
    }

    let Some(dimming) = find_dimming(dialog.upcast_ref()) else {
        return;
    };
    let gesture = gtk::GestureClick::new();
    gesture.set_button(gdk::BUTTON_PRIMARY);
    gesture.connect_released({
        let dialog = dialog.clone();
        move |gesture, n_press, _, _| {
            if n_press != 1 {
                return;
            }
            dialog.close();
            gesture.set_state(gtk::EventSequenceState::Claimed);
        }
    });
    dimming.add_controller(gesture);
}

fn contact_matches(email: &str, display: &str, q: &str) -> bool {
    if q.is_empty() {
        return true;
    }
    email.to_ascii_lowercase().contains(q) || display.to_ascii_lowercase().contains(q)
}

fn folder_rows(
    spec: &CommandPaletteSpec,
    query: &str,
    _window: &impl IsA<gtk::Widget>,
) -> Vec<PaletteRow> {
    let q = query.trim().to_ascii_lowercase();
    if q.is_empty() {
        return Vec::new();
    }
    let multi_account = spec.folders.len() > 1;
    let mut rows = Vec::new();
    for (email, folders) in &spec.folders {
        for folder in folders.iter().filter(|folder| folder.selectable) {
            let display = folder_display_name(folder);
            let haystack = format!("{display} {} {email}", folder.imap_name);
            if crate::commands::fuzzy_score(&haystack, &q).is_none() {
                continue;
            }
            let label = if multi_account {
                format!("{email} / {display}")
            } else {
                display
            };
            rows.push(PaletteRow {
                label,
                subtitle: email.clone(),
                icon: folder_icon_name(&folder.special),
                accel: None,
                sensitive: true,
                activation: PaletteActivation::GoFolder(folder.id),
            });
        }
    }
    rows.sort_by(|a, b| a.label.cmp(&b.label));
    rows.truncate(MAX_ROWS);
    rows
}

fn contact_rows(spec: &CommandPaletteSpec, query: &str) -> Vec<PaletteRow> {
    let q = query.trim().to_ascii_lowercase();
    if q.is_empty() {
        return Vec::new();
    }
    let mut rows = Vec::new();
    for (email, display) in &spec.contacts {
        if !contact_matches(email, display, &q) {
            continue;
        }
        rows.push(PaletteRow {
            label: trf!("Compose to {recipient}", recipient = display),
            subtitle: email.clone(),
            icon: "avatar-default-symbolic",
            accel: None,
            sensitive: true,
            activation: PaletteActivation::ComposeTo(email.clone()),
        });
    }
    rows.sort_by(|a, b| a.label.cmp(&b.label));
    rows.truncate(MAX_ROWS);
    rows
}

fn lookup_action_enabled(window: &adw::ApplicationWindow, action: &str) -> bool {
    let Some((scope, name)) = action.split_once('.') else {
        return true;
    };
    match scope {
        "app" => window
            .application()
            .and_then(|app| app.lookup_action(name))
            .map(|action| action.is_enabled())
            .unwrap_or(false),
        "win" => window
            .lookup_action(name)
            .map(|action| action.is_enabled())
            .unwrap_or(false),
        _ => true,
    }
}

fn static_rows(query: &str, window: &adw::ApplicationWindow) -> Vec<PaletteRow> {
    palette_static_commands(query)
        .into_iter()
        .map(|entry| static_palette_row(entry, window))
        .collect()
}

fn static_palette_row(entry: &CommandDef, window: &adw::ApplicationWindow) -> PaletteRow {
    let activation = if entry.gio_action.is_some() {
        PaletteActivation::Gio(entry.gio_action.unwrap_or(entry.id))
    } else {
        PaletteActivation::InsertSearch(entry.label)
    };
    let sensitive = match &activation {
        PaletteActivation::Gio(action) => {
            command_row_sensitive(lookup_action_enabled(window, action))
        }
        _ => true,
    };
    PaletteRow {
        label: entry.label.to_string(),
        subtitle: entry.category.label().to_string(),
        icon: entry.icon,
        accel: effective_accel(entry.id),
        sensitive,
        activation,
    }
}

/// Go To rows for spaces an extension added.
fn space_rows(query: &str) -> Vec<PaletteRow> {
    let query = query.trim().to_lowercase();
    crate::extension::space_entries()
        .into_iter()
        .filter(|space| query.is_empty() || space.title.to_lowercase().contains(&query))
        .map(|space| PaletteRow {
            label: space.title.to_string(),
            subtitle: CommandCategory::GoTo.label().to_string(),
            icon: space.palette_icon,
            accel: None,
            sensitive: true,
            activation: PaletteActivation::Gio(space.action),
        })
        .collect()
}

fn build_rows(
    spec: &CommandPaletteSpec,
    query: &str,
    window: &adw::ApplicationWindow,
) -> Vec<PaletteRow> {
    let mut rows = static_rows(query, window);
    rows.extend(space_rows(query));
    rows.extend(folder_rows(spec, query, window));
    rows.extend(contact_rows(spec, query));
    rows.truncate(MAX_ROWS);
    rows
}

fn build_palette_row(row: &PaletteRow) -> gtk::ListBoxRow {
    let list_row = gtk::ListBoxRow::new();
    list_row.set_activatable(row.sensitive);
    list_row.set_sensitive(row.sensitive);

    let action_row = adw::ActionRow::builder()
        .use_markup(false)
        .title(&row.label)
        .subtitle(&row.subtitle)
        .activatable(false)
        .build();

    let icon = gtk::Image::from_icon_name(row.icon);
    icon.set_pixel_size(16);
    action_row.add_prefix(&icon);

    if let Some(accel) = &row.accel {
        let label = adw::ShortcutLabel::new(accel);
        label.add_css_class("alcedo-palette-accel");
        label.set_sensitive(false);
        action_row.add_suffix(&label);
    }

    list_row.set_child(Some(&action_row));
    list_row
}

pub fn open_command_palette(
    parent: &impl IsA<gtk::Widget>,
    spec: CommandPaletteSpec,
    on_activate: impl Fn(PaletteActivation) + 'static,
) {
    if is_palette_open() {
        return;
    }

    let on_activate = Rc::new(on_activate);
    let window = parent
        .root()
        .and_then(|root| root.downcast::<adw::ApplicationWindow>().ok())
        .unwrap_or_else(|| {
            relm4::main_application()
                .active_window()
                .and_then(|window| window.downcast::<adw::ApplicationWindow>().ok())
                .expect("command palette parent window")
        });

    let search = gtk::SearchEntry::builder()
        .placeholder_text(tr!("Type a command"))
        .hexpand(true)
        .build();
    search.update_property(&[gtk::accessible::Property::Label(&tr!("Filter commands"))]);

    let listbox = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::Single)
        .activate_on_single_click(true)
        .build();

    let empty_page = adw::StatusPage::builder()
        .title(tr!("No matching commands"))
        .build();
    let content_stack = gtk::Stack::new();
    let scroller = gtk::ScrolledWindow::builder()
        .child(&listbox)
        .vexpand(true)
        .build();
    content_stack.set_vexpand(true);
    content_stack.add_named(&scroller, Some("list"));
    content_stack.add_named(&empty_page, Some("empty"));

    let body = gtk::Box::new(gtk::Orientation::Vertical, 12);
    body.set_margin_top(12);
    body.set_margin_bottom(12);
    body.set_margin_start(12);
    body.set_margin_end(12);
    body.append(&search);
    body.append(&content_stack);

    let dialog = adw::Dialog::builder()
        .title(tr!("Command palette"))
        .content_width(520)
        .content_height(400)
        .focus_widget(&search)
        .presentation_mode(adw::DialogPresentationMode::Floating)
        .child(&body)
        .build();
    dialog.set_widget_name(PALETTE_NAME);
    dialog.add_css_class("alcedo-command-palette");

    let rebuild = {
        let listbox = listbox.clone();
        let content_stack = content_stack.clone();
        let search = search.clone();
        let spec = spec.clone();
        let window = window.clone();
        Rc::new(move || {
            let query = search.text().to_string();
            let rows = build_rows(&spec, &query, &window);
            while let Some(row) = listbox.row_at_index(0) {
                listbox.remove(&row);
            }
            if rows.is_empty() {
                content_stack.set_visible_child_name("empty");
                return;
            }
            content_stack.set_visible_child_name("list");
            for row in rows {
                listbox.append(&build_palette_row(&row));
            }
            let mut index = 0;
            while let Some(row) = listbox.row_at_index(index) {
                if row.is_sensitive() && row.is_activatable() {
                    listbox.select_row(Some(&row));
                    break;
                }
                index += 1;
            }
            if listbox.selected_row().is_none()
                && let Some(row) = listbox.row_at_index(0)
            {
                listbox.select_row(Some(&row));
            }
        })
    };

    rebuild();

    search.connect_changed({
        let rebuild = rebuild.clone();
        move |_| rebuild()
    });

    let activate_row = {
        let on_activate = on_activate.clone();
        let dialog = dialog.clone();
        let search = search.clone();
        let spec = spec.clone();
        let window = window.clone();
        Rc::new(move |row: &gtk::ListBoxRow| {
            if !row.is_activatable() || !row.is_sensitive() {
                return;
            }
            let index = row.index();
            let query = search.text().to_string();
            let rows = build_rows(&spec, &query, &window);
            let Some(palette_row) = rows.get(index as usize) else {
                return;
            };
            if !palette_row.sensitive {
                return;
            }
            on_activate(palette_row.activation.clone());
            dialog.close();
        })
    };

    listbox.connect_row_activated({
        let activate_row = activate_row.clone();
        move |_, row| activate_row(row)
    });

    search.connect_activate({
        let listbox = listbox.clone();
        let activate_row = activate_row.clone();
        move |_| {
            if let Some(row) = listbox.selected_row() {
                activate_row(&row);
            }
        }
    });

    // Capture Escape before SearchEntry's stop-search binding swallows it.
    let escape_capture = gtk::EventControllerKey::new();
    escape_capture.set_propagation_phase(gtk::PropagationPhase::Capture);
    escape_capture.connect_key_pressed({
        let dialog = dialog.clone();
        move |_, key, _, _| {
            if key == gdk::Key::Escape {
                dialog.close();
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        }
    });
    dialog.add_controller(escape_capture);

    let key_capture = gtk::EventControllerKey::new();
    key_capture.set_propagation_phase(gtk::PropagationPhase::Capture);
    key_capture.connect_key_pressed({
        let search = search.clone();
        move |_, key, _, modifier| {
            if modifier != gdk::ModifierType::empty() {
                return glib::Propagation::Proceed;
            }
            let Some(ch) = key.to_unicode().filter(|ch| !ch.is_control()) else {
                return glib::Propagation::Proceed;
            };
            if search.has_focus() {
                return glib::Propagation::Proceed;
            }
            search.grab_focus();
            if ch != '\0' {
                let text = search.text().to_string();
                search.set_text(&(text + &ch.to_string()));
                search.set_position(-1);
            }
            glib::Propagation::Stop
        }
    });
    dialog.add_controller(key_capture);

    dialog.connect_closed({
        let dialog = dialog.clone();
        move |_| {
            OPEN_PALETTE.with(|slot| {
                let mut slot = slot.borrow_mut();
                if slot.as_ref().is_some_and(|open| open == &dialog) {
                    *slot = None;
                }
            });
        }
    });

    OPEN_PALETTE.with(|slot| {
        *slot.borrow_mut() = Some(dialog.clone());
    });

    let dimming_wired = Rc::new(Cell::new(false));
    dialog.connect_map({
        let dialog = dialog.clone();
        let dimming_wired = dimming_wired.clone();
        move |_| {
            if dimming_wired.get() {
                return;
            }
            // Presentation (floating sheet + dimming) is installed during map.
            glib::idle_add_local_once({
                let dialog = dialog.clone();
                let dimming_wired = dimming_wired.clone();
                move || {
                    if dimming_wired.replace(true) {
                        return;
                    }
                    wire_dimming_dismiss(&dialog);
                }
            });
        }
    });

    dialog.present(Some(parent));
}

pub fn window_has_foreign_dialog(window: &impl IsA<gtk::Widget>) -> bool {
    fn walk(widget: &gtk::Widget, found: &mut bool) {
        if *found {
            return;
        }
        if let Some(dialog) = widget.downcast_ref::<adw::Dialog>()
            && dialog.is_mapped()
            && dialog.widget_name().as_str() != PALETTE_NAME
        {
            *found = true;
            return;
        }
        let mut child = widget.first_child();
        while let Some(node) = child {
            walk(&node, found);
            child = node.next_sibling();
        }
    }

    let mut found = false;
    walk(window.upcast_ref(), &mut found);
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contact_matches_email_and_display() {
        assert!(contact_matches("ada@example.com", "Ada Lovelace", "ada"));
        assert!(contact_matches(
            "ada@example.com",
            "Ada Lovelace",
            "lovelace"
        ));
        assert!(!contact_matches("ada@example.com", "Ada Lovelace", "zzz"));
    }
}
