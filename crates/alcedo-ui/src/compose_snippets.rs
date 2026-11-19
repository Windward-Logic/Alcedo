//! Snippet palette (compose toolbar) and Composing preferences list.

use alcedo_i18n::{tr, trs};
use std::cell::RefCell;
use std::rc::Rc;

use alcedo_core::{ComposeFormat, Snippet, SnippetId};
use relm4::Sender;
use relm4::adw;
use relm4::adw::prelude::*;
use relm4::gtk;
use uuid::Uuid;

use crate::help::{self, TOPIC_SNIPPETS};
use crate::mail::MailMsg;

/// Composing preferences snippets group with a refreshable list host.
pub struct SnippetsPrefs {
    pub group: adw::PreferencesGroup,
    list_host: gtk::Box,
}

/// Build the Composing → Snippets preferences group.
pub fn build_snippets_prefs_group(sender: Sender<MailMsg>) -> SnippetsPrefs {
    let list_host = gtk::Box::new(gtk::Orientation::Vertical, 0);

    let add_row = adw::ButtonRow::builder()
        .title(tr!("Add Snippet"))
        .start_icon_name("list-add-symbolic")
        .build();
    add_row.connect_activated({
        let sender = sender.clone();
        move |_| {
            sender.send(MailMsg::OpenSnippetEditor(None)).ok();
        }
    });

    let group = adw::PreferencesGroup::builder()
        .title(tr!("Snippets"))
        .description(tr!(
            "Insert from the compose toolbar. Placeholders: {{first_name}}, {{full_name}}, {{email}}."
        ))
        .build();
    help::attach_group_help(&group, TOPIC_SNIPPETS);
    group.add(&list_host);
    group.add(&add_row);

    SnippetsPrefs { group, list_host }
}

/// Rebuild snippet rows in the preferences group.
pub fn refresh_snippets_prefs(
    handle: &SnippetsPrefs,
    snippets: &[Snippet],
    sender: &Sender<MailMsg>,
) {
    while let Some(child) = handle.list_host.first_child() {
        handle.list_host.remove(&child);
    }

    let mut sorted = snippets.to_vec();
    sorted.sort_by_key(|snippet| snippet.position);

    for snippet in sorted {
        let row = adw::ActionRow::builder()
            .use_markup(false)
            .title(&snippet.name)
            .subtitle(snippet.format.dropdown_label().to_string())
            .activatable(true)
            .build();
        let delete_btn = gtk::Button::builder()
            .icon_name("edit-delete-symbolic")
            .tooltip_text(tr!("Delete"))
            .css_classes(["flat"])
            .build();
        let snippet_id = snippet.id;
        let sender_delete = sender.clone();
        delete_btn.connect_clicked(move |_| {
            sender_delete.send(MailMsg::DeleteSnippet(snippet_id)).ok();
        });
        row.add_suffix(&delete_btn);

        let sender_edit = sender.clone();
        let snippet = snippet.clone();
        row.connect_activated(move |_| {
            sender_edit
                .send(MailMsg::OpenSnippetEditor(Some(snippet.clone())))
                .ok();
        });
        handle.list_host.append(&row);
    }
}

/// Toolbar snippets menu button and popover refresh handle.
pub struct SnippetsPopover {
    pub button: gtk::MenuButton,
    pub refresh: Rc<dyn Fn(&[Snippet])>,
}

/// Build the compose-toolbar snippets menu button.
pub fn build_snippets_popover(
    snippets: &[Snippet],
    on_chosen: Rc<dyn Fn(SnippetId)>,
    on_manage: Rc<dyn Fn()>,
) -> SnippetsPopover {
    let popover = gtk::Popover::new();
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .margin_top(8)
        .margin_bottom(8)
        .margin_start(8)
        .margin_end(8)
        .build();

    let search_entry = gtk::SearchEntry::builder()
        .placeholder_text(tr!("Search snippets"))
        .visible(snippets.len() > 5)
        .build();

    let listbox = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::Single)
        .css_classes(["boxed-list"])
        .build();
    listbox.update_property(&[gtk::accessible::Property::Label(&tr!("Snippets"))]);

    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .max_content_height(280)
        .propagate_natural_height(true)
        .child(&listbox)
        .build();

    let empty_label = gtk::Label::builder()
        .label(tr!("No snippets yet"))
        .css_classes(["dim-label"])
        .margin_top(12)
        .margin_bottom(12)
        .build();

    let manage_row = gtk::Button::builder()
        .label(tr!("Manage snippets…"))
        .halign(gtk::Align::Fill)
        .css_classes(["flat"])
        .build();
    manage_row.connect_clicked(move |_| on_manage());

    content.append(&search_entry);
    content.append(&scroll);
    content.append(&empty_label);
    content.append(&manage_row);
    popover.set_child(Some(&content));

    let button = gtk::MenuButton::builder()
        .icon_name("document-new-symbolic")
        .tooltip_text(tr!("Insert snippet"))
        .css_classes(["flat"])
        .can_focus(false)
        .focus_on_click(false)
        .popover(&popover)
        .build();
    button.update_property(&[gtk::accessible::Property::Label(&tr!("Insert snippet"))]);

    // Weak: `search_entry` owns a handler holding `refresh`, so strong
    // widgets here would keep the popover (and its composer) alive.
    let listbox_refresh = listbox.downgrade();
    let search_entry_refresh = search_entry.downgrade();
    let scroll_refresh = scroll.downgrade();
    let empty_label_refresh = empty_label.downgrade();
    let on_chosen_refresh = on_chosen.clone();

    let refresh: Rc<dyn Fn(&[Snippet])> = Rc::new(move |snippets: &[Snippet]| {
        let (
            Some(listbox_refresh),
            Some(search_entry_refresh),
            Some(scroll_refresh),
            Some(empty_label_refresh),
        ) = (
            listbox_refresh.upgrade(),
            search_entry_refresh.upgrade(),
            scroll_refresh.upgrade(),
            empty_label_refresh.upgrade(),
        )
        else {
            return;
        };
        search_entry_refresh.set_visible(snippets.len() > 5);
        while let Some(child) = listbox_refresh.first_child() {
            listbox_refresh.remove(&child);
        }

        let filter = search_entry_refresh.text().to_string();
        let needle = filter.trim().to_ascii_lowercase();
        let mut sorted = snippets.to_vec();
        sorted.sort_by_key(|snippet| snippet.position);

        let mut visible = 0usize;
        for snippet in sorted {
            if !needle.is_empty() && !snippet.name.to_ascii_lowercase().contains(&needle) {
                continue;
            }
            visible += 1;
            let row = gtk::ListBoxRow::new();
            let label = gtk::Label::builder()
                .label(&snippet.name)
                .xalign(0.0)
                .hexpand(true)
                .build();
            row.set_child(Some(&label));
            let snippet_id = snippet.id;
            let on_chosen = on_chosen_refresh.clone();
            row.connect_activate(move |_| on_chosen(snippet_id));
            listbox_refresh.append(&row);
        }

        let empty = visible == 0;
        empty_label_refresh.set_visible(empty);
        scroll_refresh.set_visible(!empty);
    });

    search_entry.connect_search_changed({
        let refresh = refresh.clone();
        let snippets_store = Rc::new(RefCell::new(snippets.to_vec()));
        move |_| {
            refresh(&snippets_store.borrow());
        }
    });

    refresh(snippets);

    SnippetsPopover { button, refresh }
}

/// Open the snippet editor dialog (new or edit).
pub fn open_snippet_editor(
    parent: &impl IsA<gtk::Widget>,
    existing: Option<&Snippet>,
    sender: Sender<MailMsg>,
) {
    let is_edit = existing.is_some();
    let title = if is_edit {
        trs!("Edit snippet")
    } else {
        trs!("New snippet")
    };

    let name_row = adw::EntryRow::builder().title(tr!("Name")).build();
    if let Some(snippet) = existing {
        name_row.set_text(&snippet.name);
    }

    let format_model = gtk::StringList::new(&[trs!("Plain text"), "HTML", trs!("Markdown")]);
    let format_row = adw::ComboRow::builder()
        .title(tr!("Format"))
        .model(&format_model)
        .expression(gtk::PropertyExpression::new(
            gtk::StringObject::static_type(),
            None::<&gtk::Expression>,
            "string",
        ))
        .build();
    if let Some(snippet) = existing {
        format_row.set_selected(snippet.format.dropdown_index());
    } else {
        format_row.set_selected(ComposeFormat::Plain.dropdown_index());
    }

    let body_view = gtk::TextView::builder()
        .wrap_mode(gtk::WrapMode::Word)
        .vexpand(true)
        .build();
    if let Some(snippet) = existing {
        body_view.buffer().set_text(&snippet.body);
    }
    let body_scroll = gtk::ScrolledWindow::builder()
        .min_content_height(220)
        .vexpand(true)
        .child(&body_view)
        .build();

    let cancel_btn = gtk::Button::builder().label(tr!("Cancel")).build();
    let save_btn = gtk::Button::builder()
        .label(tr!("Save"))
        .css_classes(["suggested-action"])
        .build();

    let header = adw::HeaderBar::new();
    header.pack_start(&cancel_btn);
    header.pack_end(&save_btn);

    let body = gtk::Box::new(gtk::Orientation::Vertical, 12);
    body.set_margin_top(18);
    body.set_margin_bottom(18);
    body.set_margin_start(18);
    body.set_margin_end(18);
    body.append(&name_row);
    body.append(&format_row);
    body.append(&body_scroll);

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&body));

    let dialog = adw::Dialog::builder()
        .title(title)
        .content_width(520)
        .content_height(480)
        .default_widget(&save_btn)
        .child(&toolbar)
        .build();

    let existing_id = existing.map(|snippet| snippet.id);
    let existing_position = existing.map(|snippet| snippet.position).unwrap_or(0);
    cancel_btn.connect_clicked({
        let dialog = dialog.clone();
        move |_| {
            dialog.close();
        }
    });
    save_btn.connect_clicked({
        let dialog = dialog.clone();
        let sender = sender.clone();
        move |_| {
            let name = name_row.text().to_string();
            let format = ComposeFormat::from_dropdown_index(format_row.selected());
            let body_text = body_view
                .buffer()
                .text(
                    &body_view.buffer().start_iter(),
                    &body_view.buffer().end_iter(),
                    false,
                )
                .to_string();
            let snippet = Snippet {
                id: existing_id.unwrap_or_else(|| SnippetId(Uuid::new_v4())),
                name,
                format,
                body: body_text,
                position: existing_position,
            };
            sender.send(MailMsg::UpsertSnippet(snippet)).ok();
            dialog.close();
        }
    });

    dialog.present(Some(parent.as_ref()));
}
