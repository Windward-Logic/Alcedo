//! Operator and value autocomplete for the mail search field.

use alcedo_i18n::{n_, tr, trf, trm, trs};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use alcedo_core::{Address, ContactStatus, Folder};
use relm4::gtk;
use relm4::gtk::glib;
use relm4::gtk::prelude::*;

use crate::commands::search_operator_tokens;
use crate::folder_bar::{folder_display_name, folder_icon_name};
use crate::recipient_row::ContactBook;

const MAX_SUGGESTIONS: usize = 8;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Suggestion {
    pub insert: String,
    pub label: String,
    pub subtitle: Option<String>,
    pub icon: &'static str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TokenSpan {
    pub start: usize,
    pub end: usize,
}

struct CompleteState {
    suggestions: Vec<Suggestion>,
    span: TokenSpan,
}

/// GtkSearchEntry is not itself focusable; typing happens on the inner GtkText.
fn search_text_widget(entry: &gtk::SearchEntry) -> gtk::Widget {
    entry
        .delegate()
        .and_then(|delegate| delegate.dynamic_cast::<gtk::Widget>().ok())
        .unwrap_or_else(|| entry.clone().upcast())
}

fn widget_contains_focus(widget: &impl IsA<gtk::Widget>) -> bool {
    let widget = widget.as_ref();
    if widget.has_focus() {
        return true;
    }
    let mut child = widget.first_child();
    while let Some(node) = child {
        if widget_contains_focus(&node) {
            return true;
        }
        child = node.next_sibling();
    }
    false
}

fn search_field_is_active(entry: &gtk::SearchEntry) -> bool {
    search_text_widget(entry).has_focus() || widget_contains_focus(entry)
}

pub fn attach_search_autocomplete(
    entry: &gtk::SearchEntry,
    book: Rc<RefCell<ContactBook>>,
    sent: Rc<RefCell<Vec<Address>>>,
    folders: Rc<RefCell<Vec<(String, Vec<Folder>)>>>,
) {
    let listbox = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::Single)
        .activate_on_single_click(true)
        .can_focus(true)
        .css_classes(["alcedo-autocomplete"])
        .build();
    listbox.update_property(&[gtk::accessible::Property::Label(&tr!(
        "Search operator suggestions"
    ))]);

    let popover = gtk::Popover::builder()
        .autohide(false)
        .can_focus(false)
        .has_arrow(false)
        .css_classes(["alcedo-autocomplete-popover"])
        .child(&listbox)
        .build();
    popover.set_parent(entry);

    let state = Rc::new(RefCell::new(CompleteState {
        suggestions: Vec::new(),
        span: TokenSpan { start: 0, end: 0 },
    }));
    let inserting = Rc::new(Cell::new(false));
    let ignore_focus_leave = Rc::new(Cell::new(false));

    let refresh = {
        let entry = entry.clone();
        let popover = popover.clone();
        let listbox = listbox.clone();
        let book = book.clone();
        let sent = sent.clone();
        let folders = folders.clone();
        let state = state.clone();
        let inserting = inserting.clone();
        Rc::new(move || {
            if inserting.get() || !search_field_is_active(&entry) {
                return;
            }
            let text = entry.text().to_string();
            let cursor = entry.position().max(0) as usize;
            let span = token_span(&text, cursor);
            let token = char_slice(&text, span.start, span.end);
            let today = chrono::Local::now()
                .date_naive()
                .format("%Y-%m-%d")
                .to_string();
            let ctx = SuggestionContext {
                book: &book.borrow(),
                sent: &sent.borrow(),
                folders: &folders.borrow(),
                today: &today,
            };
            let items = suggestions_for_token(&token, &ctx);
            while let Some(row) = listbox.row_at_index(0) {
                listbox.remove(&row);
            }
            if items.is_empty() {
                popover.popdown();
                state.borrow_mut().suggestions.clear();
                return;
            }
            let highlight = highlight_needle(&token);
            for item in &items {
                listbox.append(&suggestion_row(item, &highlight));
            }
            listbox.unselect_all();
            state.borrow_mut().suggestions = items;
            state.borrow_mut().span = span;
            popover.popup();
        })
    };

    let apply_index = {
        let entry = entry.clone();
        let popover = popover.clone();
        let state = state.clone();
        let inserting = inserting.clone();
        let refresh = refresh.clone();
        Rc::new(move |index: usize| {
            let (insert, span) = {
                let state = state.borrow();
                let Some(item) = state.suggestions.get(index) else {
                    return false;
                };
                (item.insert.clone(), state.span)
            };
            let text = entry.text().to_string();
            let (rewritten, cursor) = apply_insert(&text, span, &insert);
            inserting.set(true);
            entry.set_text(&rewritten);
            entry.set_position(cursor);
            inserting.set(false);
            popover.popdown();
            refresh();
            true
        })
    };

    let text_widget = search_text_widget(entry);

    entry.connect_changed({
        let refresh = refresh.clone();
        move |_| refresh()
    });
    entry.connect_search_changed({
        let refresh = refresh.clone();
        move |_| refresh()
    });
    text_widget.connect_notify_local(Some("cursor-position"), {
        let refresh = refresh.clone();
        move |_, _| refresh()
    });

    let key_controller = gtk::EventControllerKey::new();
    key_controller.set_propagation_phase(gtk::PropagationPhase::Capture);
    key_controller.connect_key_pressed({
        let popover = popover.clone();
        let listbox = listbox.clone();
        let apply_index = apply_index.clone();
        move |_, key, _, modifiers| {
            if !popover.is_visible() {
                return glib::Propagation::Proceed;
            }
            if key == gtk::gdk::Key::Escape {
                popover.popdown();
                return glib::Propagation::Stop;
            }
            if matches!(
                key,
                gtk::gdk::Key::Up
                    | gtk::gdk::Key::Down
                    | gtk::gdk::Key::KP_Up
                    | gtk::gdk::Key::KP_Down
            ) {
                navigate_list(&listbox, key);
                return glib::Propagation::Stop;
            }
            let shift = modifiers.contains(gtk::gdk::ModifierType::SHIFT_MASK);
            if key == gtk::gdk::Key::Tab && !shift {
                let index = listbox
                    .selected_row()
                    .map(|row| row.index() as usize)
                    .unwrap_or(0);
                if apply_index(index) {
                    return glib::Propagation::Stop;
                }
            }
            if matches!(key, gtk::gdk::Key::Return | gtk::gdk::Key::KP_Enter)
                && let Some(row) = listbox.selected_row()
                && apply_index(row.index() as usize)
            {
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        }
    });
    text_widget.add_controller(key_controller);

    let focus = gtk::EventControllerFocus::new();
    focus.connect_leave({
        let popover = popover.clone();
        let listbox = listbox.clone();
        let entry = entry.clone();
        let ignore_focus_leave = ignore_focus_leave.clone();
        move |_| {
            if ignore_focus_leave.get() {
                return;
            }
            let popover = popover.clone();
            let listbox = listbox.clone();
            let entry = entry.clone();
            glib::idle_add_local(move || {
                if search_field_is_active(&entry) || listbox.has_focus() {
                    return glib::ControlFlow::Break;
                }
                if popover.is_visible() {
                    popover.popdown();
                }
                glib::ControlFlow::Break
            });
        }
    });
    text_widget.add_controller(focus);

    listbox.connect_row_activated({
        let apply_index = apply_index.clone();
        let ignore_focus_leave = ignore_focus_leave.clone();
        move |_, row| {
            ignore_focus_leave.set(true);
            let index = row.index();
            if index >= 0 {
                apply_index(index as usize);
            }
            ignore_focus_leave.set(false);
        }
    });
}

pub struct SuggestionContext<'a> {
    pub book: &'a ContactBook,
    pub sent: &'a [Address],
    pub folders: &'a [(String, Vec<Folder>)],
    pub today: &'a str,
}

pub fn token_span(text: &str, cursor: usize) -> TokenSpan {
    let chars: Vec<char> = text.chars().collect();
    let cursor = cursor.min(chars.len());
    let mut start = cursor;
    while start > 0 {
        let prev = start - 1;
        if chars[prev].is_whitespace() && !in_quotes(&chars, prev) {
            break;
        }
        start = prev;
    }
    let mut end = cursor;
    while end < chars.len() {
        if chars[end].is_whitespace() && !in_quotes(&chars, end) {
            break;
        }
        end += 1;
    }
    TokenSpan { start, end }
}

pub fn apply_insert(text: &str, span: TokenSpan, insert: &str) -> (String, i32) {
    let chars: Vec<char> = text.chars().collect();
    let start = span.start.min(chars.len());
    let end = span.end.min(chars.len()).max(start);
    let mut out: String = chars[..start].iter().collect();
    out.push_str(insert);
    let rest: String = chars[end..].iter().collect();
    if !insert.ends_with(':') && !rest.starts_with(' ') {
        out.push(' ');
    }
    let cursor = out.chars().count() as i32;
    out.push_str(&rest);
    (out, cursor)
}

pub fn suggestions_for_token(token: &str, ctx: &SuggestionContext<'_>) -> Vec<Suggestion> {
    if token.is_empty() {
        return Vec::new();
    }
    if let Some((key, value)) = split_operator(token) {
        return value_suggestions(&key, value, ctx);
    }
    operator_suggestions(token)
}

fn operator_suggestions(prefix: &str) -> Vec<Suggestion> {
    let needle = prefix.to_ascii_lowercase();
    let mut items = Vec::new();
    for token in search_operator_tokens() {
        if !token.to_ascii_lowercase().starts_with(&needle) {
            continue;
        }
        items.push(Suggestion {
            insert: token.to_string(),
            label: token.to_string(),
            subtitle: operator_meaning(token).map(str::to_string),
            icon: "edit-find-symbolic",
        });
        if items.len() == MAX_SUGGESTIONS {
            break;
        }
    }
    items
}

fn value_suggestions(key: &str, value: &str, ctx: &SuggestionContext<'_>) -> Vec<Suggestion> {
    match key {
        "from" | "to" => person_suggestions(key, value, ctx),
        "folder" | "in" => folder_suggestions(key, value, ctx),
        "is" => flag_suggestions("is", value, IS_VALUES),
        "has" => flag_suggestions("has", value, HAS_VALUES),
        "after" | "before" => date_suggestions(key, value, ctx.today),
        "larger" | "smaller" | "bigger" => size_suggestions(key, value),
        _ => Vec::new(),
    }
}

fn person_suggestions(key: &str, value: &str, ctx: &SuggestionContext<'_>) -> Vec<Suggestion> {
    let needle = strip_quotes(value).to_ascii_lowercase();
    let mut items = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for contact in &ctx.book.contacts {
        if contact.status != ContactStatus::Approved {
            continue;
        }
        if !person_matches(
            &contact.email,
            &contact.display_name,
            &contact.nickname,
            &needle,
        ) {
            continue;
        }
        let email_key = contact.email.to_ascii_lowercase();
        if !seen.insert(email_key) {
            continue;
        }
        items.push(person_item(key, &contact.email, &contact.display_name));
    }
    for addr in ctx.sent {
        if !person_matches(&addr.email, addr.name.as_deref().unwrap_or(""), "", &needle) {
            continue;
        }
        let email_key = addr.email.to_ascii_lowercase();
        if !seen.insert(email_key) {
            continue;
        }
        items.push(person_item(
            key,
            &addr.email,
            addr.name.as_deref().unwrap_or(""),
        ));
    }
    items.sort_by_key(|item| {
        (
            std::cmp::Reverse(person_rank_from_item(item, &needle)),
            item.label.clone(),
        )
    });
    items.truncate(MAX_SUGGESTIONS);
    items
}

fn person_item(key: &str, email: &str, name: &str) -> Suggestion {
    let label = if name.is_empty() || name.eq_ignore_ascii_case(email) {
        email.to_string()
    } else {
        format!("{name} <{email}>")
    };
    Suggestion {
        insert: format!("{key}:{}", quote_value(email)),
        label,
        subtitle: None,
        icon: "avatar-default-symbolic",
    }
}

fn person_rank_from_item(item: &Suggestion, needle: &str) -> bool {
    item.label.to_ascii_lowercase().starts_with(needle)
        || item
            .insert
            .to_ascii_lowercase()
            .split_once(':')
            .is_some_and(|(_, value)| value.starts_with(needle))
}

fn person_matches(email: &str, name: &str, nick: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    email.to_ascii_lowercase().contains(needle)
        || name.to_ascii_lowercase().contains(needle)
        || nick.to_ascii_lowercase().contains(needle)
}

fn folder_suggestions(key: &str, value: &str, ctx: &SuggestionContext<'_>) -> Vec<Suggestion> {
    let needle = strip_quotes(value).to_ascii_lowercase();
    let mut items = Vec::new();
    for (keyword, meaning) in FOLDER_KEYWORDS {
        if !keyword.starts_with(needle.as_str()) && !needle.is_empty() {
            continue;
        }
        items.push(Suggestion {
            insert: format!("{key}:{keyword}"),
            label: format!("{key}:{keyword}"),
            subtitle: Some(trm!(meaning).to_string()),
            icon: "folder-symbolic",
        });
    }
    let multi_account = ctx.folders.len() > 1;
    for (email, folders) in ctx.folders {
        for folder in folders.iter().filter(|folder| folder.selectable) {
            let display = folder_display_name(folder);
            if !folder_matches(&display, &folder.imap_name, &needle) {
                continue;
            }
            let label = if multi_account {
                format!("{display} ({email})")
            } else {
                display.clone()
            };
            items.push(Suggestion {
                insert: format!("{key}:{}", quote_value(&folder.imap_name)),
                label,
                subtitle: if display == folder.imap_name {
                    None
                } else {
                    Some(folder.imap_name.clone())
                },
                icon: folder_icon_name(&folder.special),
            });
        }
    }
    items.sort_by(|a, b| a.label.cmp(&b.label));
    items.truncate(MAX_SUGGESTIONS);
    items
}

fn folder_matches(display: &str, imap: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    display.to_ascii_lowercase().contains(needle) || imap.to_ascii_lowercase().contains(needle)
}

// (operator value, meaning shown under it); meanings are translated on display.
const IS_VALUES: &[(&str, &str)] = &[
    ("unread", n_!("Unread")),
    ("read", n_!("Read")),
    ("starred", n_!("Starred")),
    ("unstarred", n_!("Not starred")),
];

const HAS_VALUES: &[(&str, &str)] = &[("attachment", n_!("Has a file attached"))];

const FOLDER_KEYWORDS: &[(&str, &str)] = &[
    ("anywhere", n_!("All folders")),
    ("all", n_!("All folders")),
];

fn flag_suggestions(key: &str, value: &str, values: &[(&str, &'static str)]) -> Vec<Suggestion> {
    let needle = value.to_ascii_lowercase();
    values
        .iter()
        .filter(|(flag, _)| needle.is_empty() || flag.starts_with(&needle))
        .map(|(flag, meaning)| Suggestion {
            insert: format!("{key}:{flag}"),
            label: format!("{key}:{flag}"),
            subtitle: Some(trm!(meaning).to_string()),
            icon: "edit-find-symbolic",
        })
        .take(MAX_SUGGESTIONS)
        .collect()
}

// (operator value, size shown to the person); sizes are translated on display.
const SIZE_PRESETS: &[(&str, &str)] = &[
    ("1M", n_!("1 MB")),
    ("5M", n_!("5 MB")),
    ("10M", n_!("10 MB")),
    ("25M", n_!("25 MB")),
    ("50M", n_!("50 MB")),
    ("100M", n_!("100 MB")),
];

fn size_suggestions(key: &str, value: &str) -> Vec<Suggestion> {
    let needle = value.trim().to_ascii_lowercase();
    let mut items: Vec<Suggestion> = SIZE_PRESETS
        .iter()
        .filter(|(token, _)| needle.is_empty() || token.to_ascii_lowercase().starts_with(&needle))
        .map(|(token, label)| size_item(key, token, trm!(label)))
        .collect();
    if items.is_empty()
        && !needle.is_empty()
        && needle.chars().all(|ch| ch.is_ascii_digit() || ch == '.')
    {
        let typed = [
            ("K", trf!("{size} KB", size = needle)),
            ("M", trf!("{size} MB", size = needle)),
            ("G", trf!("{size} GB", size = needle)),
        ];
        for (unit, size) in typed {
            items.push(size_item(key, &format!("{needle}{unit}"), &size));
        }
    }
    items.truncate(MAX_SUGGESTIONS);
    items
}

fn size_item(key: &str, token: &str, size: &str) -> Suggestion {
    let subtitle = if key == "smaller" {
        trf!("Smaller than {size}", size = size)
    } else {
        trf!("Larger than {size}", size = size)
    };
    Suggestion {
        insert: format!("{key}:{token}"),
        label: format!("{key}:{token}"),
        subtitle: Some(subtitle),
        icon: "drive-harddisk-symbolic",
    }
}

fn date_suggestions(key: &str, value: &str, today: &str) -> Vec<Suggestion> {
    if !today
        .to_ascii_lowercase()
        .starts_with(&value.to_ascii_lowercase())
        && !value.is_empty()
    {
        return Vec::new();
    }
    vec![Suggestion {
        insert: format!("{key}:{today}"),
        label: format!("{key}:{today}"),
        subtitle: Some(tr!("Today")),
        icon: "x-office-calendar-symbolic",
    }]
}

fn operator_meaning(token: &str) -> Option<&'static str> {
    Some(match token {
        "from:" => trs!("Sender"),
        "to:" => trs!("Recipient"),
        "subject:" => trs!("Subject text"),
        "folder:" | "in:" => trs!("Folder"),
        "in:anywhere" => trs!("All folders"),
        "after:" => trs!("After date"),
        "before:" => trs!("Before date"),
        "larger:" => trs!("Larger than a size"),
        "smaller:" => trs!("Smaller than a size"),
        "is:unread" => trs!("Unread"),
        "is:read" => trs!("Read"),
        "is:starred" => trs!("Starred"),
        "is:unstarred" => trs!("Not starred"),
        "has:attachment" => trs!("Has a file attached"),
        _ => return None,
    })
}

fn split_operator(token: &str) -> Option<(String, &str)> {
    let (key, value) = token.split_once(':')?;
    if key.is_empty() {
        return None;
    }
    Some((key.to_ascii_lowercase(), value))
}

fn strip_quotes(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.len() >= 2 && trimmed.starts_with('"') && trimmed.ends_with('"') {
        trimmed[1..trimmed.len() - 1].replace("\\\"", "\"")
    } else if let Some(rest) = trimmed.strip_prefix('"') {
        rest.replace("\\\"", "\"")
    } else {
        trimmed.to_string()
    }
}

fn quote_value(value: &str) -> String {
    if value.contains(' ') || value.contains(':') || value.contains('"') {
        let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
        format!("\"{escaped}\"")
    } else {
        value.to_string()
    }
}

fn in_quotes(chars: &[char], pos: usize) -> bool {
    let mut in_q = false;
    for (i, &ch) in chars.iter().enumerate() {
        if i >= pos {
            break;
        }
        if ch == '"' && (i == 0 || chars[i - 1] != '\\') {
            in_q = !in_q;
        }
    }
    in_q
}

fn char_slice(text: &str, start: usize, end: usize) -> String {
    text.chars()
        .skip(start)
        .take(end.saturating_sub(start))
        .collect()
}

fn highlight_needle(token: &str) -> String {
    if let Some((_, value)) = split_operator(token) {
        strip_quotes(value)
    } else {
        token.to_string()
    }
}

fn suggestion_row(item: &Suggestion, needle: &str) -> gtk::ListBoxRow {
    let row = gtk::ListBoxRow::new();
    row.set_can_focus(false);
    let box_ = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    box_.add_css_class("alcedo-autocomplete-row");
    box_.set_valign(gtk::Align::Center);

    let icon = gtk::Image::from_icon_name(item.icon);
    icon.add_css_class("alcedo-autocomplete-icon");
    icon.set_valign(gtk::Align::Center);
    box_.append(&icon);

    let text_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    text_box.set_hexpand(true);
    let label = gtk::Label::builder()
        .use_markup(true)
        .label(highlight_query(&item.label, needle))
        .xalign(0.0)
        .hexpand(true)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .css_classes(["alcedo-autocomplete-label"])
        .build();
    text_box.append(&label);
    if let Some(subtitle) = &item.subtitle {
        let sub = gtk::Label::builder()
            .label(subtitle)
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["dim-label", "caption"])
            .build();
        text_box.append(&sub);
    }
    box_.append(&text_box);
    row.set_child(Some(&box_));
    row
}

pub(crate) fn highlight_query(text: &str, query: &str) -> String {
    let q = query.trim();
    if q.is_empty() {
        return pango_escape(text);
    }
    let lower_text = text.to_ascii_lowercase();
    let lower_q = q.to_ascii_lowercase();
    let mut out = String::new();
    let mut i = 0;
    while let Some(pos) = lower_text[i..].find(&lower_q) {
        let start = i + pos;
        let end = start + lower_q.len();
        out.push_str(&pango_escape(&text[i..start]));
        out.push_str("<b>");
        out.push_str(&pango_escape(&text[start..end]));
        out.push_str("</b>");
        i = end;
    }
    out.push_str(&pango_escape(&text[i..]));
    out
}

pub(crate) fn pango_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '\'' => out.push_str("&apos;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(ch),
        }
    }
    out
}

fn navigate_list(listbox: &gtk::ListBox, key: gtk::gdk::Key) {
    let mut count = 0;
    while listbox.row_at_index(count).is_some() {
        count += 1;
    }
    if count == 0 {
        return;
    }
    let current = listbox.selected_row().map(|row| row.index()).unwrap_or(-1);
    let next = match key {
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
    };
    match next {
        Some(index) => {
            if let Some(row) = listbox.row_at_index(index) {
                listbox.select_row(Some(&row));
            }
        }
        None => listbox.unselect_all(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alcedo_core::{AccountId, Contact, ContactId, FolderId, GroupId, SpecialUse};
    use chrono::Utc;
    use uuid::Uuid;

    fn empty_ctx() -> (ContactBook, Vec<Address>, Vec<(String, Vec<Folder>)>) {
        (
            ContactBook {
                contacts: Vec::new(),
                groups: Vec::new(),
            },
            Vec::new(),
            Vec::new(),
        )
    }

    fn ctx_from<'a>(
        book: &'a ContactBook,
        sent: &'a [Address],
        folders: &'a [(String, Vec<Folder>)],
    ) -> SuggestionContext<'a> {
        SuggestionContext {
            book,
            sent,
            folders,
            today: "2026-09-02",
        }
    }

    fn approved(email: &str, name: &str) -> Contact {
        Contact {
            id: ContactId(1),
            email: email.to_string(),
            display_name: name.to_string(),
            nickname: String::new(),
            organization: String::new(),
            notes: String::new(),
            status: ContactStatus::Approved,
            group_ids: vec![GroupId(2)],
            created_at: Utc::now(),
            updated_at: Utc::now(),
            last_used_at: None,
        }
    }

    fn folder(imap: &str, special: SpecialUse) -> Folder {
        Folder {
            id: FolderId(1),
            account_id: AccountId(Uuid::nil()),
            imap_name: imap.to_string(),
            delimiter: "/".to_string(),
            special,
            selectable: true,
            uidvalidity: None,
            uidnext: None,
            highest_modseq: None,
            unread: 0,
            total: 0,
        }
    }

    #[test]
    fn token_span_last_word() {
        let text = "invoice from:ad";
        let span = token_span(text, text.chars().count());
        assert_eq!(char_slice(text, span.start, span.end), "from:ad");
    }

    #[test]
    fn token_span_quoted_value() {
        let text = r#"subject:"q1 plan"#;
        let span = token_span(text, text.chars().count());
        assert_eq!(
            char_slice(text, span.start, span.end),
            r#"subject:"q1 plan"#
        );
    }

    #[test]
    fn token_span_middle_of_query() {
        let text = "from:ada is:unread";
        let span = token_span(text, 8);
        assert_eq!(char_slice(text, span.start, span.end), "from:ada");
    }

    #[test]
    fn larger_completes_sizes() {
        let (book, sent, folders) = empty_ctx();
        let ctx = ctx_from(&book, &sent, &folders);
        let presets: Vec<_> = suggestions_for_token("larger:", &ctx)
            .into_iter()
            .map(|item| item.insert)
            .collect();
        assert!(presets.contains(&"larger:10M".to_string()));
        let typed: Vec<_> = suggestions_for_token("larger:3", &ctx)
            .into_iter()
            .map(|item| item.insert)
            .collect();
        assert_eq!(typed, vec!["larger:3K", "larger:3M", "larger:3G"]);
    }

    #[test]
    fn operators_match_prefix() {
        let (book, sent, folders) = empty_ctx();
        let ctx = ctx_from(&book, &sent, &folders);
        let labels: Vec<_> = suggestions_for_token("fr", &ctx)
            .into_iter()
            .map(|s| s.insert)
            .collect();
        assert_eq!(labels, vec!["from:"]);
        let is_ops: Vec<_> = suggestions_for_token("is", &ctx)
            .into_iter()
            .map(|s| s.insert)
            .collect();
        assert!(is_ops.contains(&"is:unread".to_string()));
        assert!(is_ops.contains(&"is:read".to_string()));
        assert!(suggestions_for_token("invoice", &ctx).is_empty());
    }

    #[test]
    fn from_completes_contacts() {
        let book = ContactBook {
            contacts: vec![
                approved("ada@example.com", "Ada"),
                approved("bob@example.com", "Bob"),
            ],
            groups: Vec::new(),
        };
        let sent = vec![];
        let folders = vec![];
        let ctx = ctx_from(&book, &sent, &folders);
        let items = suggestions_for_token("from:ad", &ctx);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].insert, "from:ada@example.com");
    }

    #[test]
    fn is_completes_flags() {
        let (book, sent, folders) = empty_ctx();
        let ctx = ctx_from(&book, &sent, &folders);
        let items: Vec<_> = suggestions_for_token("is:un", &ctx)
            .into_iter()
            .map(|s| s.insert)
            .collect();
        assert_eq!(items, vec!["is:unread", "is:unstarred"]);
    }

    #[test]
    fn folder_quotes_imap_with_spaces() {
        let (book, sent, _) = empty_ctx();
        let folders = vec![(
            "user@example.com".to_string(),
            vec![folder("[Gmail]/Sent Mail", SpecialUse::Sent)],
        )];
        let ctx = ctx_from(&book, &sent, &folders);
        let items = suggestions_for_token("in:sent", &ctx);
        assert!(
            items
                .iter()
                .any(|item| item.insert == r#"in:"[Gmail]/Sent Mail""#)
        );
    }

    #[test]
    fn apply_insert_replaces_token_and_pads() {
        let (out, cursor) = apply_insert("invoice fr", TokenSpan { start: 8, end: 10 }, "from:");
        assert_eq!(out, "invoice from:");
        assert_eq!(cursor, "invoice from:".len() as i32);

        let (out, _) = apply_insert(
            "from:ad",
            TokenSpan { start: 0, end: 7 },
            "from:ada@example.com",
        );
        assert_eq!(out, "from:ada@example.com ");
    }

    #[test]
    fn after_suggests_today() {
        let (book, sent, folders) = empty_ctx();
        let ctx = ctx_from(&book, &sent, &folders);
        let items = suggestions_for_token("after:", &ctx);
        assert_eq!(items[0].insert, "after:2026-09-02");
    }
}
