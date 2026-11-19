//! Session activity log: ring buffer, formatters, and Activity dialog.

use alcedo_i18n::{tr, trs};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use crate::settings::{activity_log_filter, set_activity_log_filter};
pub use alcedo_core::redact_url_userinfo;
use chrono::{DateTime, Local, Utc};
use relm4::adw;
use relm4::adw::prelude::*;
use relm4::gtk;
use relm4::gtk::gio;

pub const KIND_MAIL: u32 = 1 << 0;
pub const KIND_SYNC: u32 = 1 << 1;
pub const KIND_ACCOUNT: u32 = 1 << 2;
// Bit 3 is unused; saved filter masks from earlier builds may still set it.
pub const KIND_CONTACTS: u32 = 1 << 4;
pub const KIND_FAILURE: u32 = 1 << 5;
pub const KIND_ALL: u32 = KIND_MAIL | KIND_SYNC | KIND_ACCOUNT | KIND_CONTACTS | KIND_FAILURE;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActivityOutcome {
    Info,
    Success,
    Failure,
}

#[derive(Clone, Debug)]
pub struct ActivityEntry {
    pub at: DateTime<Utc>,
    pub kind: u32,
    pub outcome: ActivityOutcome,
    pub title: String,
    pub detail: String,
    pub account: String,
    pub debug: String,
}

pub struct ActivityLog {
    entries: RefCell<VecDeque<ActivityEntry>>,
    on_change: RefCell<Option<Rc<dyn Fn()>>>,
}

impl ActivityLog {
    pub fn new() -> Rc<Self> {
        Rc::new(Self {
            entries: RefCell::new(VecDeque::new()),
            on_change: RefCell::new(None),
        })
    }

    pub fn push(&self, entry: ActivityEntry) {
        {
            let mut entries = self.entries.borrow_mut();
            entries.push_front(entry);
            let limit = crate::settings::activity_log_limit() as usize;
            while entries.len() > limit {
                entries.pop_back();
            }
        }
        if let Some(cb) = self.on_change.borrow().as_ref() {
            cb();
        }
    }

    pub fn clear(&self) {
        self.entries.borrow_mut().clear();
        if let Some(cb) = self.on_change.borrow().as_ref() {
            cb();
        }
    }

    pub fn snapshot(&self) -> Vec<ActivityEntry> {
        self.entries.borrow().iter().cloned().collect()
    }

    pub fn last_sync_title(&self) -> Option<String> {
        self.entries
            .borrow()
            .iter()
            .find(|entry| entry.kind & KIND_SYNC != 0)
            .map(|entry| entry.title.clone())
    }

    pub fn trim_to_limit(&self) {
        {
            let mut entries = self.entries.borrow_mut();
            let limit = crate::settings::activity_log_limit() as usize;
            while entries.len() > limit {
                entries.pop_back();
            }
        }
        if let Some(cb) = self.on_change.borrow().as_ref() {
            cb();
        }
    }

    pub fn is_empty(&self) -> bool {
        self.entries.borrow().is_empty()
    }

    fn set_on_change(&self, callback: Option<Rc<dyn Fn()>>) {
        *self.on_change.borrow_mut() = callback;
    }
}

pub fn entry(
    kind_category: u32,
    outcome: ActivityOutcome,
    title: impl Into<String>,
    detail: impl Into<String>,
) -> ActivityEntry {
    let failure_bit = if outcome == ActivityOutcome::Failure {
        KIND_FAILURE
    } else {
        0
    };
    ActivityEntry {
        at: Utc::now(),
        kind: kind_category | failure_bit,
        outcome,
        title: title.into(),
        detail: detail.into(),
        account: String::new(),
        debug: String::new(),
    }
}

pub fn format_row_title(account: &str, title: &str) -> String {
    if account.is_empty() || title.contains(account) {
        title.to_string()
    } else {
        format!("{account} · {title}")
    }
}

pub fn format_account_debug(
    _email: &str,
    imap_host: &str,
    imap_port: u16,
    smtp_host: &str,
    smtp_port: u16,
    auth_label: &str,
    raw: &str,
) -> String {
    let mut lines = Vec::new();
    if !imap_host.is_empty() {
        let host = server_host(imap_host).unwrap_or_else(|| imap_host.to_string());
        lines.push(format!("imap: {host}:{imap_port}"));
    }
    if !smtp_host.is_empty() {
        let host = server_host(smtp_host).unwrap_or_else(|| smtp_host.to_string());
        lines.push(format!("smtp: {host}:{smtp_port}"));
    }
    if !auth_label.is_empty() {
        lines.push(format!("auth: {auth_label}"));
    }
    if !raw.is_empty() {
        lines.push(format!("status: {}", redact_url_userinfo(raw)));
    }
    lines.join("\n")
}

pub fn server_host(url: &str) -> Option<String> {
    let after_scheme = url.split("://").nth(1).unwrap_or(url);
    let host_part = after_scheme.rsplit('@').next().unwrap_or(after_scheme);
    let host = host_part.split('/').next().unwrap_or(host_part);
    let host = host.split(':').next().unwrap_or(host);
    if host.is_empty() {
        None
    } else {
        Some(host.to_string())
    }
}

pub fn should_log_status(previous: Option<&str>, raw: &str) -> bool {
    if raw.eq_ignore_ascii_case("Sign-in required") {
        return false;
    }
    previous != Some(raw)
}

pub fn matches_filter(kind: u32, mask: u32) -> bool {
    kind & mask != 0
}

pub fn format_send_detail(
    from: Option<&str>,
    to: &[String],
    cc: &[String],
    bcc: &[String],
    subject: &str,
    error: Option<&str>,
) -> String {
    let mut parts = Vec::new();
    if let Some(from) = from.filter(|value| !value.is_empty()) {
        parts.push(format!("From {from}"));
    }
    if !to.is_empty() {
        parts.push(format!("To {}", join_addrs(to)));
    }
    if !cc.is_empty() {
        parts.push(format!("Cc {}", join_addrs(cc)));
    }
    if !bcc.is_empty() {
        parts.push(format!("Bcc {}", join_addrs(bcc)));
    }
    let trimmed = subject.trim();
    let subject_text = if trimmed.is_empty() {
        alcedo_core::NO_SUBJECT
    } else {
        trimmed
    };
    parts.push(subject_text.to_string());
    let mut detail = parts.join(" · ");
    if let Some(error) = error.filter(|value| !value.is_empty()) {
        detail.push_str(&format!(" · {error}"));
    }
    detail
}

pub fn format_activity_time(at: DateTime<Utc>, now_local_tz: DateTime<Local>) -> String {
    let _ = now_local_tz;
    at.with_timezone(&Local).format("%H:%M:%S").to_string()
}

pub fn outcome_label(outcome: ActivityOutcome) -> &'static str {
    match outcome {
        ActivityOutcome::Info => "info",
        ActivityOutcome::Success => "success",
        ActivityOutcome::Failure => "failure",
    }
}

pub fn category_label(kind: u32) -> &'static str {
    if kind & KIND_MAIL != 0 {
        "mail"
    } else if kind & KIND_SYNC != 0 {
        "sync"
    } else if kind & KIND_ACCOUNT != 0 {
        "account"
    } else if kind & KIND_CONTACTS != 0 {
        "contacts"
    } else {
        "other"
    }
}

pub fn format_activity_export(entries: &[ActivityEntry], exported_at: DateTime<Local>) -> String {
    let mut out = String::new();
    out.push_str("Alcedo activity log\n");
    out.push_str(&format!(
        "Exported: {}\n",
        exported_at.format("%Y-%m-%d %H:%M:%S")
    ));
    out.push_str(&format!("Entries: {} (newest first)", entries.len()));

    if entries.is_empty() {
        return out;
    }

    out.push('\n');

    for (index, entry) in entries.iter().enumerate() {
        let local_time = entry.at.with_timezone(&Local).format("%Y-%m-%d %H:%M:%S");
        let account_one_line = entry.account.replace(['\t', '\n', '\r'], " ");
        let title_one_line = entry.title.replace(['\t', '\n', '\r'], " ");
        out.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\n",
            local_time,
            outcome_label(entry.outcome),
            category_label(entry.kind),
            account_one_line,
            title_one_line
        ));
        if !entry.detail.is_empty() {
            out.push_str(&entry.detail);
            out.push('\n');
        }
        if !entry.debug.is_empty() {
            out.push_str(&entry.debug);
            out.push('\n');
        }
        if index + 1 < entries.len() {
            out.push('\n');
        }
    }

    out
}

pub fn suggested_activity_filename(exported_at: DateTime<Local>) -> String {
    exported_at
        .format("alcedo-activity-%Y%m%d-%H%M%S.txt")
        .to_string()
}

fn join_addrs(addrs: &[String]) -> String {
    match addrs.len() {
        0 => String::new(),
        n if n <= 4 => addrs.join(", "),
        n => {
            let more = n - 3;
            format!("{}, and {more} more", addrs[..3].join(", "))
        }
    }
}

fn category_icon(kind: u32) -> &'static str {
    if kind & KIND_MAIL != 0 {
        "mail-sent-symbolic"
    } else if kind & KIND_SYNC != 0 {
        "view-refresh-symbolic"
    } else if kind & KIND_ACCOUNT != 0 {
        "avatar-default-symbolic"
    } else if kind & KIND_CONTACTS != 0 {
        "contact-new-symbolic"
    } else {
        "dialog-information-symbolic"
    }
}

struct ActivityDialogUi {
    stack: gtk::Stack,
    listbox: gtk::ListBox,
    _empty_page: adw::StatusPage,
    _filtered_empty_page: adw::StatusPage,
    clear_btn: gtk::Button,
    copy_btn: gtk::Button,
    save_btn: gtk::Button,
    toasts: adw::ToastOverlay,
    filter_buttons: Vec<(gtk::CheckButton, u32)>,
}

fn dialog_parent(parent: &gtk::Widget) -> Option<gtk::Window> {
    parent
        .root()
        .and_then(|root| root.downcast::<gtk::Window>().ok())
}

thread_local! {
    static OPEN_DIALOG: RefCell<Option<adw::Dialog>> = const { RefCell::new(None) };
}

/// Present the Activity dialog (singleton).
pub fn present_activity_dialog(parent: &impl IsA<gtk::Widget>, log: &Rc<ActivityLog>) {
    if let Some(existing) = OPEN_DIALOG.with(|cell| cell.borrow().clone()) {
        existing.present(Some(parent.as_ref()));
        return;
    }

    let close_btn = gtk::Button::builder()
        .label(tr!("Close"))
        .css_classes(["suggested-action"])
        .build();
    let clear_btn = gtk::Button::builder()
        .label(tr!("Clear"))
        .tooltip_text(tr!("Clear activity log"))
        .build();
    let copy_btn = gtk::Button::builder()
        .label(tr!("Copy"))
        .tooltip_text(tr!("Copy activity log"))
        .build();
    let save_btn = gtk::Button::builder()
        .label(tr!("Save…"))
        .tooltip_text(tr!("Save activity log to a file"))
        .build();

    let header = adw::HeaderBar::new();
    header.set_show_start_title_buttons(false);
    header.set_show_end_title_buttons(false);
    let title_label = gtk::Label::builder()
        .label(tr!("Activity"))
        .css_classes(["title"])
        .build();
    header.set_title_widget(Some(&title_label));
    header.pack_start(&close_btn);
    header.pack_end(&clear_btn);
    header.pack_end(&save_btn);
    header.pack_end(&copy_btn);

    let filter_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(12)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(12)
        .margin_end(12)
        .build();
    filter_box.update_property(&[gtk::accessible::Property::Label(&tr!("Activity filters"))]);

    let filter_specs: [(&str, u32); 5] = [
        (trs!("Mail"), KIND_MAIL),
        (trs!("Sync"), KIND_SYNC),
        (trs!("Accounts"), KIND_ACCOUNT),
        (trs!("Contacts"), KIND_CONTACTS),
        (trs!("Failures"), KIND_FAILURE),
    ];
    let mut filter_buttons = Vec::new();
    for (label, bit) in filter_specs {
        let button = gtk::CheckButton::builder().label(label).build();
        filter_box.append(&button);
        filter_buttons.push((button, bit));
    }

    let listbox = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .activate_on_single_click(false)
        .build();
    listbox.update_property(&[gtk::accessible::Property::Label(&tr!("Activity entries"))]);

    let list_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .vexpand(true)
        .hexpand(true)
        .child(&listbox)
        .build();

    let empty_page = adw::StatusPage::builder()
        .title(tr!("No activity yet"))
        .description(tr!("Actions you take in Alcedo show up here."))
        .build();
    let filtered_empty_page = adw::StatusPage::builder()
        .title(tr!("No matching activity"))
        .description(tr!("Turn on a filter to see those entries."))
        .build();

    let stack = gtk::Stack::new();
    stack.add_named(&list_scroll, Some("list"));
    stack.add_named(&empty_page, Some("empty"));
    stack.add_named(&filtered_empty_page, Some("filtered-empty"));

    let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
    body.append(&filter_box);
    body.append(&stack);

    let toast_overlay = adw::ToastOverlay::new();
    toast_overlay.set_child(Some(&body));

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&toast_overlay));

    let dialog = adw::Dialog::builder()
        .title(tr!("Activity"))
        .content_width(720)
        .content_height(520)
        .default_widget(&close_btn)
        .child(&toolbar)
        .build();

    let ui = Rc::new(ActivityDialogUi {
        stack,
        listbox,
        _empty_page: empty_page,
        _filtered_empty_page: filtered_empty_page,
        clear_btn: clear_btn.clone(),
        copy_btn: copy_btn.clone(),
        save_btn: save_btn.clone(),
        toasts: toast_overlay.clone(),
        filter_buttons,
    });

    let log_for_ui = log.clone();
    let ui_for_rebuild = ui.clone();
    let rebuild = Rc::new(move || rebuild_activity_list(&ui_for_rebuild, &log_for_ui));
    rebuild();

    let rebuild_for_change = rebuild.clone();
    log.set_on_change(Some(Rc::new(move || rebuild_for_change())));

    let log_for_copy = log.clone();
    let toasts_for_copy = ui.toasts.clone();
    copy_btn.connect_clicked(move |_| {
        let text = format_activity_export(&log_for_copy.snapshot(), Local::now());
        crate::notifications::copy_text_to_clipboard(&text);
        toasts_for_copy.add_toast(crate::toast::plain(&tr!("Activity log copied")));
    });

    let log_for_save = log.clone();
    let toasts_for_save = ui.toasts.clone();
    let dialog_for_save = dialog.clone();
    save_btn.connect_clicked(move |_| {
        let exported_at = Local::now();
        let text = format_activity_export(&log_for_save.snapshot(), exported_at);
        let filename = suggested_activity_filename(exported_at);

        let file_dialog = gtk::FileDialog::new();
        file_dialog.set_title(&tr!("Save activity log"));
        file_dialog.set_initial_name(Some(&filename));
        let filter = gtk::FileFilter::new();
        filter.set_name(Some(trs!("Text file")));
        filter.add_mime_type("text/plain");
        filter.add_pattern("*.txt");
        file_dialog.set_default_filter(Some(&filter));

        let parent_window = dialog_parent(dialog_for_save.as_ref());
        let toasts = toasts_for_save.clone();
        file_dialog.save(
            parent_window.as_ref(),
            None::<&gio::Cancellable>,
            move |result| match result {
                Ok(file) => {
                    let Some(path) = file.path() else {
                        toasts.add_toast(crate::toast::plain(&tr!("Could not save activity log")));
                        return;
                    };
                    gtk::glib::spawn_future_local(async move {
                        let written =
                            crate::blocking_io::run(move || std::fs::write(&path, text.as_bytes()));
                        let message = match written.await {
                            Some(Ok(())) => trs!("Activity log saved"),
                            _ => trs!("Could not save activity log"),
                        };
                        toasts.add_toast(crate::toast::plain(message));
                    });
                }
                Err(err) if err.matches(gio::IOErrorEnum::Cancelled) => {}
                Err(_) => {
                    toasts.add_toast(crate::toast::plain(&tr!("Could not save activity log")));
                }
            },
        );
    });

    let log_for_clear = log.clone();
    let rebuild_for_clear = rebuild.clone();
    let dialog_for_clear = dialog.clone();
    clear_btn.connect_clicked(move |_| {
        let confirm = adw::AlertDialog::builder()
            .heading(tr!("Clear activity log?"))
            .body(tr!(
                "This removes all entries for this session. This cannot be undone."
            ))
            .build();
        confirm.add_response("cancel", &tr!("Cancel"));
        confirm.add_response("clear", &tr!("Clear"));
        confirm.set_response_appearance("clear", adw::ResponseAppearance::Destructive);
        confirm.set_default_response(Some("cancel"));
        confirm.set_close_response("cancel");
        let log_for_clear = log_for_clear.clone();
        let rebuild_for_clear = rebuild_for_clear.clone();
        confirm.choose(
            Some(&dialog_for_clear),
            None::<&gtk::gio::Cancellable>,
            move |response| {
                if response.as_str() == "clear" {
                    log_for_clear.clear();
                    rebuild_for_clear();
                }
            },
        );
    });

    sync_filter_buttons(&ui.filter_buttons);
    for (button, bit) in &ui.filter_buttons {
        let bit = *bit;
        let rebuild_for_toggle = rebuild.clone();
        button.connect_toggled(move |btn| {
            let mask = activity_log_filter() as u32;
            let new_mask = if btn.is_active() {
                mask | bit
            } else {
                mask & !bit
            };
            set_activity_log_filter(new_mask as i32);
            rebuild_for_toggle();
        });
    }

    close_btn.connect_clicked({
        let dialog = dialog.clone();
        move |_| {
            dialog.close();
        }
    });

    let log_for_close = log.clone();
    dialog.connect_closed(move |_| {
        log_for_close.set_on_change(None);
        OPEN_DIALOG.with(|cell| *cell.borrow_mut() = None);
    });

    OPEN_DIALOG.with(|cell| *cell.borrow_mut() = Some(dialog.clone()));
    dialog.present(Some(parent.as_ref()));
}

fn sync_filter_buttons(buttons: &[(gtk::CheckButton, u32)]) {
    let mask = activity_log_filter() as u32;
    for (button, bit) in buttons {
        let active = mask & *bit != 0;
        if button.is_active() != active {
            button.set_active(active);
        }
    }
}

fn rebuild_activity_list(ui: &ActivityDialogUi, log: &Rc<ActivityLog>) {
    ui.clear_btn.set_sensitive(!log.is_empty());
    ui.copy_btn.set_sensitive(!log.is_empty());
    ui.save_btn.set_sensitive(!log.is_empty());

    let mask = activity_log_filter() as u32;
    let entries: Vec<_> = log
        .snapshot()
        .into_iter()
        .filter(|entry| matches_filter(entry.kind, mask))
        .collect();

    while let Some(child) = ui.listbox.first_child() {
        ui.listbox.remove(&child);
    }

    if log.is_empty() {
        ui.stack.set_visible_child_name("empty");
        return;
    }

    if entries.is_empty() {
        ui.stack.set_visible_child_name("filtered-empty");
        return;
    }

    ui.stack.set_visible_child_name("list");
    let now_local = Local::now();
    for entry in entries {
        ui.listbox.append(&activity_row(&entry, now_local));
    }
}

fn activity_row(entry: &ActivityEntry, now_local: DateTime<Local>) -> gtk::Widget {
    let title = format_row_title(&entry.account, &entry.title);
    if entry.debug.is_empty() {
        let row = adw::ActionRow::builder()
            .title(&title)
            .use_markup(false)
            .activatable(false)
            .build();
        apply_activity_row_chrome(&row, entry, now_local);
        return row.upcast();
    }

    let row = adw::ExpanderRow::builder()
        .title(&title)
        .use_markup(false)
        .build();
    let debug_label = gtk::Label::builder()
        .label(&entry.debug)
        .xalign(0.0)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .selectable(true)
        .margin_start(12)
        .margin_end(12)
        .margin_bottom(12)
        .css_classes(["caption", "dim-label"])
        .build();
    row.add_row(&debug_label);
    apply_expander_row_chrome(&row, entry, now_local);
    row.upcast()
}

fn apply_activity_row_chrome(
    row: &adw::ActionRow,
    entry: &ActivityEntry,
    now_local: DateTime<Local>,
) {
    if !entry.detail.is_empty() {
        row.set_subtitle(&entry.detail);
        row.set_tooltip_text(Some(&entry.detail));
    }
    let (prefix, ok, time) = activity_row_chrome(entry, now_local);
    row.add_prefix(&prefix);
    if let Some(ok) = ok {
        row.add_suffix(&ok);
    }
    row.add_suffix(&time);
    if entry.outcome == ActivityOutcome::Failure {
        row.add_css_class("error");
    }
}

fn apply_expander_row_chrome(
    row: &adw::ExpanderRow,
    entry: &ActivityEntry,
    now_local: DateTime<Local>,
) {
    if !entry.detail.is_empty() {
        row.set_subtitle(&entry.detail);
        row.set_tooltip_text(Some(&entry.detail));
    }
    let (prefix, ok, time) = activity_row_chrome(entry, now_local);
    row.add_prefix(&prefix);
    if let Some(ok) = ok {
        row.add_suffix(&ok);
    }
    row.add_suffix(&time);
    if entry.outcome == ActivityOutcome::Failure {
        row.add_css_class("error");
    }
}

fn activity_row_chrome(
    entry: &ActivityEntry,
    now_local: DateTime<Local>,
) -> (gtk::Image, Option<gtk::Image>, gtk::Label) {
    let icon_name = if entry.outcome == ActivityOutcome::Failure {
        "dialog-error-symbolic"
    } else {
        category_icon(entry.kind)
    };
    let prefix = gtk::Image::from_icon_name(icon_name);
    prefix.set_pixel_size(16);
    let ok = if entry.outcome == ActivityOutcome::Success {
        let ok = gtk::Image::from_icon_name("emblem-ok-symbolic");
        ok.set_pixel_size(12);
        Some(ok)
    } else {
        None
    };
    let time = gtk::Label::builder()
        .label(format_activity_time(entry.at, now_local))
        .css_classes(["caption", "dim-label"])
        .build();
    (prefix, ok, time)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn kind_all_is_63() {
        assert_eq!(KIND_ALL, 55);
    }

    #[test]
    fn matches_filter_bits() {
        let kind = KIND_MAIL | KIND_FAILURE;
        assert!(matches_filter(kind, KIND_MAIL));
        assert!(matches_filter(kind, KIND_FAILURE));
        assert!(!matches_filter(kind, KIND_CONTACTS));
    }

    #[test]
    fn entry_failure_sets_failure_bit() {
        let failure = entry(KIND_MAIL, ActivityOutcome::Failure, "Send failed", "");
        assert_eq!(failure.kind, KIND_MAIL | KIND_FAILURE);
        let success = entry(KIND_MAIL, ActivityOutcome::Success, "Message sent", "");
        assert_eq!(success.kind, KIND_MAIL);
    }

    #[test]
    fn ring_buffer_keeps_newest() {
        let mut entries = VecDeque::new();
        for title in ["one", "two", "three"] {
            entries.push_front(entry(KIND_MAIL, ActivityOutcome::Info, title, ""));
            while entries.len() > 2 {
                entries.pop_back();
            }
        }
        let snap: Vec<_> = entries.iter().cloned().collect();
        assert_eq!(snap.len(), 2);
        assert_eq!(snap[0].title, "three");
        assert_eq!(snap[1].title, "two");
    }

    #[test]
    fn format_send_detail_recipients_and_subject() {
        let detail = format_send_detail(
            Some("me@example.com"),
            &["a@x.com".into(), "b@x.com".into()],
            &[],
            &[],
            "Hello",
            None,
        );
        assert!(detail.contains("From me@example.com"));
        assert!(detail.contains("To a@x.com, b@x.com"));
        assert!(detail.contains("Hello"));
    }

    #[test]
    fn format_send_detail_many_to() {
        let to: Vec<_> = (0..5).map(|i| format!("u{i}@x.com")).collect();
        let detail = format_send_detail(None, &to, &[], &[], "Subj", None);
        assert!(detail.contains("and 2 more"));
    }

    #[test]
    fn format_send_detail_empty_subject() {
        let detail = format_send_detail(None, &["a@x.com".into()], &[], &[], "  ", None);
        assert!(detail.contains(alcedo_core::NO_SUBJECT));
    }

    #[test]
    fn format_send_detail_error_suffix() {
        let detail = format_send_detail(None, &[], &[], &[], "Hi", Some("timeout"));
        assert!(detail.ends_with("timeout"));
    }

    #[test]
    fn format_send_detail_omits_empty_cc_bcc() {
        let detail = format_send_detail(None, &["a@x.com".into()], &[], &[], "Hi", None);
        assert!(!detail.contains("Cc"));
        assert!(!detail.contains("Bcc"));
    }

    #[test]
    fn format_activity_time_local_hms() {
        let at = Utc.with_ymd_and_hms(2024, 6, 1, 12, 30, 45).unwrap();
        let formatted = format_activity_time(at, Local::now());
        let parts: Vec<_> = formatted.split(':').collect();
        assert_eq!(parts.len(), 3);
        assert!(
            parts
                .iter()
                .all(|part| part.len() == 2 && part.chars().all(|c| c.is_ascii_digit()))
        );
    }

    #[test]
    fn should_log_status_skips_sign_in_and_duplicates() {
        assert!(!should_log_status(None, "Sign-in required"));
        assert!(!should_log_status(Some("IDLE"), "IDLE"));
        assert!(should_log_status(Some("IDLE"), "Connecting…"));
        assert!(should_log_status(None, "IDLE"));
    }

    #[test]
    fn format_row_title_prefixes_and_avoids_double_prefix() {
        assert_eq!(
            format_row_title("ada@example.com", "Up to date"),
            "ada@example.com · Up to date"
        );
        assert_eq!(
            format_row_title("ada@example.com", "Could not sync ada@example.com: timeout"),
            "Could not sync ada@example.com: timeout"
        );
        assert_eq!(format_row_title("", "Up to date"), "Up to date");
    }

    #[test]
    fn format_account_debug_omits_empty_hosts_and_secrets() {
        let debug = format_account_debug(
            "ada@example.com",
            "user:secret@imap.example.com",
            993,
            "https://user:secret@smtp.example.com",
            587,
            "oauth",
            "IDLE",
        );
        assert!(debug.contains("imap: imap.example.com:993"));
        assert!(debug.contains("smtp: smtp.example.com:587"));
        assert!(debug.contains("auth: oauth"));
        assert!(debug.contains("status: IDLE"));
        assert!(!debug.contains("password"));
        assert!(!debug.contains("secret"));
        let sparse = format_account_debug("a@b.com", "", 0, "", 0, "", "Connecting…");
        assert_eq!(sparse, "status: Connecting…");
    }

    #[test]
    fn server_host_strips_userinfo_and_path() {
        assert_eq!(
            server_host("https://user:secret@cal.example/dav").as_deref(),
            Some("cal.example")
        );
        assert_eq!(
            server_host("cal.example:8443/path").as_deref(),
            Some("cal.example")
        );
    }

    #[test]
    fn format_activity_export_header_and_rows() {
        let exported_at = Local.with_ymd_and_hms(2026, 9, 23, 11, 27, 5).unwrap();
        let mut failure = entry(
            KIND_MAIL,
            ActivityOutcome::Failure,
            "Send failed",
            "From me@example.com · To a@x.com · timeout",
        );
        failure.at = Utc.with_ymd_and_hms(2026, 9, 23, 15, 26, 41).unwrap();
        failure.account = "me@example.com".to_string();
        assert_eq!(category_label(failure.kind), "mail");

        let mut success = entry(KIND_SYNC, ActivityOutcome::Success, "Up to date", "");
        success.at = Utc.with_ymd_and_hms(2026, 9, 23, 15, 25, 2).unwrap();
        success.account = "ada@example.com".to_string();
        success.debug = format_account_debug(
            "ada@example.com",
            "imap.example.com",
            993,
            "smtp.example.com",
            587,
            "oauth",
            "IDLE",
        );

        let failure_local = failure
            .at
            .with_timezone(&Local)
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();
        let success_local = success
            .at
            .with_timezone(&Local)
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();

        let text = format_activity_export(&[failure, success], exported_at);
        assert!(text.starts_with(
            "Alcedo activity log\nExported: 2026-09-23 11:27:05\nEntries: 2 (newest first)\n"
        ));
        assert!(text.contains(&format!(
            "{failure_local}\tfailure\tmail\tme@example.com\tSend failed\nFrom me@example.com · To a@x.com · timeout\n\n"
        )));
        assert!(text.contains(&format!(
            "{success_local}\tsuccess\tsync\tada@example.com\tUp to date\nimap: imap.example.com:993\nsmtp: smtp.example.com:587\nauth: oauth\nstatus: IDLE\n"
        )));
        assert!(!text.contains(&format!(
            "{success_local}\tsuccess\tsync\tada@example.com\tUp to date\nimap: imap.example.com:993\nsmtp: smtp.example.com:587\nauth: oauth\nstatus: IDLE\n\n"
        )));
    }

    #[test]
    fn format_activity_export_empty() {
        let exported_at = Local.with_ymd_and_hms(2026, 9, 23, 11, 27, 5).unwrap();
        let text = format_activity_export(&[], exported_at);
        assert_eq!(
            text,
            "Alcedo activity log\nExported: 2026-09-23 11:27:05\nEntries: 0 (newest first)"
        );
    }

    #[test]
    fn format_activity_export_title_one_line() {
        let exported_at = Local.with_ymd_and_hms(2026, 9, 23, 11, 27, 5).unwrap();
        let mut entry = entry(KIND_MAIL, ActivityOutcome::Info, "a\tb\nc", "");
        entry.at = Utc.with_ymd_and_hms(2026, 9, 23, 15, 0, 0).unwrap();
        let text = format_activity_export(&[entry], exported_at);
        assert!(text.contains("\t\ta b c\n"));
    }

    #[test]
    fn suggested_activity_filename_shape() {
        let exported_at = Local.with_ymd_and_hms(2026, 9, 23, 11, 27, 5).unwrap();
        let filename = suggested_activity_filename(exported_at);
        assert!(filename.starts_with("alcedo-activity-"));
        assert!(filename.ends_with(".txt"));
        let middle = filename
            .strip_prefix("alcedo-activity-")
            .and_then(|value| value.strip_suffix(".txt"))
            .expect("filename shape");
        let (date, time) = middle.split_once('-').expect("date-time separator");
        assert_eq!(date.len(), 8);
        assert!(date.chars().all(|c| c.is_ascii_digit()));
        assert_eq!(time.len(), 6);
        assert!(time.chars().all(|c| c.is_ascii_digit()));
    }
}
