//! Application preferences backed by GSettings.

use alcedo_i18n::{trf, trs};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::Duration;

use alcedo_core::{
    AccountId, AttachmentLists, ComposeFormat, ExtensionListId, FolderId, MessageListMode,
    format_type_list, parse_type_list, tokens_from_strv,
};
use relm4::adw;
use relm4::adw::prelude::{ComboRowExt, EntryRowExt, PreferencesDialogExt, PreferencesRowExt};
use relm4::gtk;
use relm4::gtk::gio;
use relm4::gtk::glib;
use relm4::gtk::prelude::*;
use uuid::Uuid;

pub(crate) const COLLAPSED_SEP: char = '\u{1f}';

const SCHEMA_ID: &str = "dev.alcedo.Mail";

thread_local! {
    static SETTINGS: gio::Settings = load_settings();
}

fn load_settings() -> gio::Settings {
    #[cfg(test)]
    if let Some(settings) = settings_from_compiled_schema() {
        return settings;
    }
    gio::Settings::new(SCHEMA_ID)
}

/// Tests run without the schema installed. Load the copy `build.rs` compiled
/// into `OUT_DIR` so `gio::Settings::new` does not abort.
#[cfg(test)]
fn settings_from_compiled_schema() -> Option<gio::Settings> {
    let source = gio::SettingsSchemaSource::from_directory(
        env!("ALCEDO_SCHEMA_DIR"),
        gio::SettingsSchemaSource::default().as_ref(),
        true,
    )
    .ok()?;
    let schema = source.lookup(SCHEMA_ID, true)?;
    Some(gio::Settings::new_full(
        &schema,
        Some(&gio::memory_settings_backend_new()),
        None,
    ))
}

fn with_settings<F, R>(f: F) -> R
where
    F: FnOnce(&gio::Settings) -> R,
{
    SETTINGS.with(|settings| f(settings))
}

/// GSettings rejects writes to keys an administrator has locked (dconf
/// lockdown) or that the backend cannot store. A preference that does not stick
/// is not worth crashing the UI over, so log it and carry on.
fn log_write_failure(key: &str, err: &glib::BoolError) {
    tracing::warn!(key, %err, "could not save preference");
}

pub fn show_message_preview() -> bool {
    with_settings(|settings| settings.boolean("show-message-preview"))
}

pub fn bind_show_message_preview(widget: &impl IsA<glib::Object>) {
    with_settings(|settings| {
        settings
            .bind("show-message-preview", widget, "active")
            .flags(gio::SettingsBindFlags::DEFAULT)
            .build();
    });
}

pub fn on_show_message_preview_changed<F: Fn(bool) + 'static>(callback: F) {
    with_settings(|settings| {
        settings.connect_changed(Some("show-message-preview"), move |settings, _| {
            callback(settings.boolean("show-message-preview"));
        });
    });
}

pub fn set_show_message_preview(enabled: bool) {
    with_settings(|settings| {
        settings
            .set_boolean("show-message-preview", enabled)
            .unwrap_or_else(|err| log_write_failure("show-message-preview", &err));
    });
}

pub fn load_remote_images() -> bool {
    with_settings(|settings| settings.boolean("load-remote-images"))
}

pub fn bind_load_remote_images(widget: &impl IsA<glib::Object>) {
    with_settings(|settings| {
        settings
            .bind("load-remote-images", widget, "active")
            .flags(gio::SettingsBindFlags::DEFAULT)
            .build();
    });
}

pub fn on_load_remote_images_changed<F: Fn(bool) + 'static>(callback: F) {
    with_settings(|settings| {
        settings.connect_changed(Some("load-remote-images"), move |settings, _| {
            callback(settings.boolean("load-remote-images"));
        });
    });
}

pub fn attachment_lists() -> AttachmentLists {
    with_settings(|settings| AttachmentLists {
        risky_extensions: tokens_from_strv(settings.strv("risky-attachment-extensions")),
        risky_mime_types: tokens_from_strv(settings.strv("risky-attachment-mime-types")),
        typical_inline_image_extensions: tokens_from_strv(
            settings.strv("typical-inline-image-extensions"),
        ),
    })
}

pub fn bind_strv_as_csv(key: &'static str, entry: &adw::EntryRow) {
    with_settings(|settings| {
        let tokens = tokens_from_strv(settings.strv(key));
        entry.set_text(&format_type_list(&tokens));
    });
    entry.set_show_apply_button(false);

    let pending: Rc<RefCell<Option<glib::SourceId>>> = Rc::default();
    let last_toast: Rc<RefCell<Option<adw::Toast>>> = Rc::default();
    let save = {
        let entry = entry.clone();
        let pending = pending.clone();
        move |tidy: bool| {
            crate::glib_source::cancel_source_option(&mut pending.borrow_mut());
            save_strv_csv(key, &entry, tidy, &last_toast);
        }
    };
    let save = Rc::new(save);

    entry.connect_changed({
        let save = save.clone();
        move |_| {
            crate::glib_source::cancel_source_option(&mut pending.borrow_mut());
            let save = save.clone();
            let pending_inner = pending.clone();
            let source = glib::timeout_add_local_once(CSV_SAVE_DELAY, move || {
                pending_inner.borrow_mut().take();
                save(false);
            });
            *pending.borrow_mut() = Some(source);
        }
    });
    entry.connect_entry_activated({
        let save = save.clone();
        move |_| save(true)
    });
    let focus = gtk::EventControllerFocus::new();
    focus.connect_leave(move |_| save(true));
    entry.add_controller(focus);
}

const CSV_SAVE_DELAY: Duration = Duration::from_millis(800);

fn save_strv_csv(
    key: &'static str,
    entry: &adw::EntryRow,
    tidy: bool,
    last_toast: &RefCell<Option<adw::Toast>>,
) {
    let parsed = parse_type_list(&entry.text());
    let formatted = format_type_list(&parsed);
    if tidy && entry.text() != formatted {
        entry.set_text(&formatted);
    }
    let current = with_settings(|settings| tokens_from_strv(settings.strv(key)));
    if parsed == current {
        return;
    }
    let refs: Vec<&str> = parsed.iter().map(String::as_str).collect();
    with_settings(|settings| {
        settings
            .set_strv(key, refs)
            .unwrap_or_else(|err| log_write_failure(key, &err));
    });
    let Some(dialog) = entry
        .ancestor(adw::PreferencesDialog::static_type())
        .and_downcast::<adw::PreferencesDialog>()
    else {
        return;
    };
    if let Some(previous) = last_toast.borrow_mut().take() {
        previous.dismiss();
    }
    // TRANSLATORS: toast; {setting} is the name of a preference.
    let toast = crate::toast::plain(&trf!("{setting} saved", setting = entry.title()));
    dialog.add_toast(toast.clone());
    *last_toast.borrow_mut() = Some(toast);
}

pub fn show_verification_codes() -> bool {
    with_settings(|settings| settings.boolean("show-verification-codes"))
}

pub fn bind_show_verification_codes(widget: &impl IsA<glib::Object>) {
    with_settings(|settings| {
        settings
            .bind("show-verification-codes", widget, "active")
            .flags(gio::SettingsBindFlags::DEFAULT)
            .build();
    });
}

pub fn on_show_verification_codes_changed<F: Fn(bool) + 'static>(callback: F) {
    with_settings(|settings| {
        settings.connect_changed(Some("show-verification-codes"), move |settings, _| {
            callback(settings.boolean("show-verification-codes"));
        });
    });
}

pub fn cleanup_verification_codes() -> bool {
    with_settings(|settings| settings.boolean("cleanup-verification-codes"))
}

pub fn bind_cleanup_verification_codes(widget: &impl IsA<glib::Object>) {
    with_settings(|settings| {
        settings
            .bind("cleanup-verification-codes", widget, "active")
            .flags(gio::SettingsBindFlags::DEFAULT)
            .build();
    });
}

pub fn close_to_tray() -> bool {
    with_settings(|settings| settings.boolean("close-to-tray"))
}

pub fn bind_close_to_tray(widget: &impl IsA<glib::Object>) {
    with_settings(|settings| {
        settings
            .bind("close-to-tray", widget, "active")
            .flags(gio::SettingsBindFlags::DEFAULT)
            .build();
    });
}

pub fn on_close_to_tray_changed<F: Fn(bool) + 'static>(callback: F) {
    with_settings(|settings| {
        settings.connect_changed(Some("close-to-tray"), move |settings, _| {
            callback(settings.boolean("close-to-tray"));
        });
    });
}

pub fn folders_docked() -> bool {
    with_settings(|settings| settings.boolean("folders-docked"))
}

pub fn set_folders_docked(docked: bool) {
    with_settings(|settings| {
        settings
            .set_boolean("folders-docked", docked)
            .unwrap_or_else(|err| log_write_failure("folders-docked", &err));
    });
}

pub fn rail_expanded() -> bool {
    with_settings(|settings| settings.boolean("rail-expanded"))
}

pub fn set_rail_expanded(expanded: bool) {
    with_settings(|settings| {
        settings
            .set_boolean("rail-expanded", expanded)
            .unwrap_or_else(|err| log_write_failure("rail-expanded", &err));
    });
}

pub const RAIL_WIDTH_MIN: i32 = 56;
pub const RAIL_WIDTH_DEFAULT: i32 = 56;
pub const RAIL_WIDTH_MAX: i32 = 280;
pub const RAIL_LABEL_THRESHOLD: i32 = 120;

pub fn clamp_rail_width(width: i32) -> i32 {
    width.clamp(RAIL_WIDTH_MIN, RAIL_WIDTH_MAX)
}

/// `stored == 0` migrates from `rail-expanded` (true → 240, false → 56).
pub fn resolved_rail_width(stored: i32, expanded: bool) -> i32 {
    if stored == 0 {
        if expanded { 240 } else { RAIL_WIDTH_DEFAULT }
    } else {
        clamp_rail_width(stored)
    }
}

pub fn rail_width() -> i32 {
    with_settings(|settings| {
        resolved_rail_width(
            settings.int("rail-width"),
            settings.boolean("rail-expanded"),
        )
    })
}

pub fn set_rail_width(width: i32) {
    let width = clamp_rail_width(width);
    with_settings(|settings| {
        settings
            .set_int("rail-width", width)
            .unwrap_or_else(|err| log_write_failure("rail-width", &err));
        settings
            .set_boolean("rail-expanded", width >= RAIL_LABEL_THRESHOLD)
            .unwrap_or_else(|err| log_write_failure("rail-expanded", &err));
    });
}

pub fn rail_visible() -> bool {
    with_settings(|settings| settings.boolean("rail-visible"))
}

pub fn set_rail_visible(visible: bool) {
    with_settings(|settings| {
        settings
            .set_boolean("rail-visible", visible)
            .unwrap_or_else(|err| log_write_failure("rail-visible", &err));
    });
}

pub fn compose_format() -> ComposeFormat {
    with_settings(|settings| ComposeFormat::from_gsettings(&settings.string("compose-format")))
}

pub fn compose_format_for_account(account_id: AccountId) -> ComposeFormat {
    compose_format_overrides()
        .get(&account_id)
        .copied()
        .unwrap_or_else(compose_format)
}

pub fn set_compose_format_for_account(account_id: AccountId, format: ComposeFormat) {
    let mut overrides = compose_format_overrides();
    if format == compose_format() {
        overrides.remove(&account_id);
    } else {
        overrides.insert(account_id, format);
    }
    set_compose_format_overrides(&overrides);
}

pub fn set_compose_format(format: ComposeFormat) {
    with_settings(|settings| {
        settings
            .set_string("compose-format", format.as_str())
            .unwrap_or_else(|err| log_write_failure("compose-format", &err));
    });
    let global = compose_format();
    let mut overrides = compose_format_overrides();
    overrides.retain(|_, value| *value != global);
    set_compose_format_overrides(&overrides);
}

pub fn bind_compose_format(row: &relm4::adw::ComboRow) {
    row.set_selected(compose_format().dropdown_index());
    row.connect_notify_local(Some("selected"), move |row, _| {
        set_compose_format(ComposeFormat::from_dropdown_index(row.selected()));
    });
}

/// Disconnect settings listeners when `owner` is destroyed. Listeners live on
/// the process-wide settings object, so one registered per widget instance
/// (such as each composer) would otherwise keep what it captured alive for
/// the rest of the run. The listeners must not capture
/// `owner` strongly, or it is never destroyed.
pub fn disconnect_on_destroy(
    owner: &impl IsA<gtk::Widget>,
    handlers: impl IntoIterator<Item = glib::SignalHandlerId>,
) {
    let handlers = RefCell::new(handlers.into_iter().collect::<Vec<_>>());
    owner.as_ref().connect_destroy(move |_| {
        with_settings(|settings| {
            for handler in handlers.borrow_mut().drain(..) {
                settings.disconnect(handler);
            }
        });
    });
}

const SEND_DELAY_KEY: &str = "send-delay-seconds";

/// Undo-send delay in seconds; 0 sends immediately. A missing key (an older
/// installed schema) reads as off rather than aborting.
pub fn send_delay_seconds() -> u32 {
    with_settings(|settings| {
        let has_key = settings
            .settings_schema()
            .is_some_and(|schema| schema.has_key(SEND_DELAY_KEY));
        if has_key {
            settings.int(SEND_DELAY_KEY).clamp(0, 60) as u32
        } else {
            0
        }
    })
}

pub fn set_send_delay_seconds(seconds: u32) {
    let value = i32::try_from(seconds).unwrap_or(60).clamp(0, 60);
    with_settings(|settings| {
        let has_key = settings
            .settings_schema()
            .is_some_and(|schema| schema.has_key(SEND_DELAY_KEY));
        if !has_key {
            return;
        }
        if let Err(err) = settings.set_int(SEND_DELAY_KEY, value) {
            tracing::warn!(%err, "could not save send delay");
        }
    });
}

pub fn request_mdn() -> bool {
    with_settings(|settings| settings.boolean("request-mdn"))
}

pub fn bind_request_mdn(widget: &impl IsA<glib::Object>) {
    with_settings(|settings| {
        settings
            .bind("request-mdn", widget, "active")
            .flags(gio::SettingsBindFlags::DEFAULT)
            .build();
    });
}

pub fn on_request_mdn_changed<F: Fn(bool) + 'static>(callback: F) -> glib::SignalHandlerId {
    with_settings(|settings| {
        settings.connect_changed(Some("request-mdn"), move |settings, _| {
            callback(settings.boolean("request-mdn"));
        })
    })
}

pub fn collect_outgoing_contacts() -> bool {
    with_settings(|settings| settings.boolean("collect-outgoing-contacts"))
}

pub fn set_collect_outgoing_contacts(value: bool) {
    with_settings(|settings| {
        settings
            .set_boolean("collect-outgoing-contacts", value)
            .unwrap_or_else(|err| log_write_failure("collect-outgoing-contacts", &err));
    });
}

pub fn bind_collect_outgoing_contacts(widget: &impl IsA<glib::Object>) {
    with_settings(|settings| {
        settings
            .bind("collect-outgoing-contacts", widget, "active")
            .flags(gio::SettingsBindFlags::DEFAULT)
            .build();
    });
}

pub fn insert_tracking_pixel() -> bool {
    with_settings(|settings| settings.boolean("insert-tracking-pixel"))
}

pub fn bind_insert_tracking_pixel(widget: &impl IsA<glib::Object>) {
    with_settings(|settings| {
        settings
            .bind("insert-tracking-pixel", widget, "active")
            .flags(gio::SettingsBindFlags::DEFAULT)
            .build();
    });
}

pub fn on_insert_tracking_pixel_changed<F: Fn(bool) + 'static>(
    callback: F,
) -> glib::SignalHandlerId {
    with_settings(|settings| {
        settings.connect_changed(Some("insert-tracking-pixel"), move |settings, _| {
            callback(settings.boolean("insert-tracking-pixel"));
        })
    })
}

pub fn tracking_pixel_url() -> String {
    with_settings(|settings| settings.string("tracking-pixel-url").to_string())
}

pub fn bind_tracking_pixel_url(widget: &impl IsA<glib::Object>) {
    with_settings(|settings| {
        settings
            .bind("tracking-pixel-url", widget, "text")
            .flags(gio::SettingsBindFlags::DEFAULT)
            .build();
    });
}

/// Effective tracking pixel URL when insert is enabled and URL is HTTPS.
pub fn effective_tracking_pixel_url() -> Option<String> {
    if !insert_tracking_pixel() {
        return None;
    }
    let url = tracking_pixel_url();
    let trimmed = url.trim();
    if trimmed.starts_with("https://") {
        Some(trimmed.to_string())
    } else {
        None
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadingPane {
    Right,
    Bottom,
}

impl ReadingPane {
    fn from_gsettings(value: &str) -> Self {
        match value {
            "bottom" => Self::Bottom,
            _ => Self::Right,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Right => "right",
            Self::Bottom => "bottom",
        }
    }
}

pub fn reading_pane() -> ReadingPane {
    with_settings(|settings| ReadingPane::from_gsettings(&settings.string("reading-pane")))
}

pub fn set_reading_pane(pane: ReadingPane) {
    with_settings(|settings| {
        settings
            .set_string("reading-pane", pane.as_str())
            .unwrap_or_else(|err| log_write_failure("reading-pane", &err));
    });
}

pub fn on_reading_pane_changed<F: Fn(ReadingPane) + 'static>(callback: F) {
    with_settings(|settings| {
        settings.connect_changed(Some("reading-pane"), move |settings, _| {
            callback(ReadingPane::from_gsettings(
                &settings.string("reading-pane"),
            ));
        });
    });
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FolderPaneAccounts {
    Picker,
    Stacked,
}

impl FolderPaneAccounts {
    fn from_gsettings(value: &str) -> Self {
        match value {
            "stacked" => Self::Stacked,
            _ => Self::Picker,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Picker => "picker",
            Self::Stacked => "stacked",
        }
    }

    pub fn dropdown_index(self) -> u32 {
        match self {
            Self::Picker => 0,
            Self::Stacked => 1,
        }
    }

    pub fn from_dropdown_index(index: u32) -> Self {
        match index {
            1 => Self::Stacked,
            _ => Self::Picker,
        }
    }
}

pub fn folder_pane_accounts() -> FolderPaneAccounts {
    with_settings(|settings| {
        FolderPaneAccounts::from_gsettings(&settings.string("folder-pane-accounts"))
    })
}

fn set_folder_pane_accounts(mode: FolderPaneAccounts) {
    with_settings(|settings| {
        settings
            .set_string("folder-pane-accounts", mode.as_str())
            .unwrap_or_else(|err| log_write_failure("folder-pane-accounts", &err));
    });
}

pub fn bind_folder_pane_accounts(row: &relm4::adw::ComboRow) {
    row.set_selected(folder_pane_accounts().dropdown_index());
    row.connect_notify_local(Some("selected"), move |row, _| {
        set_folder_pane_accounts(FolderPaneAccounts::from_dropdown_index(row.selected()));
    });
}

pub fn on_folder_pane_accounts_changed<F: Fn(FolderPaneAccounts) + 'static>(callback: F) {
    with_settings(|settings| {
        settings.connect_changed(Some("folder-pane-accounts"), move |settings, _| {
            callback(FolderPaneAccounts::from_gsettings(
                &settings.string("folder-pane-accounts"),
            ));
        });
    });
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListDensity {
    Compact,
    Comfortable,
    Relaxed,
}

impl ListDensity {
    fn from_gsettings(value: &str) -> Self {
        match value {
            "compact" => Self::Compact,
            "relaxed" => Self::Relaxed,
            _ => Self::Comfortable,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Compact => "compact",
            Self::Comfortable => "comfortable",
            Self::Relaxed => "relaxed",
        }
    }

    pub fn dropdown_index(self) -> u32 {
        match self {
            Self::Compact => 0,
            Self::Comfortable => 1,
            Self::Relaxed => 2,
        }
    }

    pub fn from_dropdown_index(index: u32) -> Self {
        match index {
            0 => Self::Compact,
            2 => Self::Relaxed,
            _ => Self::Comfortable,
        }
    }

    pub fn css_class(self) -> &'static str {
        match self {
            Self::Compact => "alcedo-density-compact",
            Self::Comfortable => "alcedo-density-comfortable",
            Self::Relaxed => "alcedo-density-relaxed",
        }
    }

    pub const ALL_CSS_CLASSES: [&'static str; 3] = [
        "alcedo-density-compact",
        "alcedo-density-comfortable",
        "alcedo-density-relaxed",
    ];
}

pub fn list_density() -> ListDensity {
    with_settings(|settings| ListDensity::from_gsettings(&settings.string("list-density")))
}

pub fn set_list_density(density: ListDensity) {
    with_settings(|settings| {
        settings
            .set_string("list-density", density.as_str())
            .unwrap_or_else(|err| log_write_failure("list-density", &err));
    });
}

pub fn bind_list_density(row: &relm4::adw::ComboRow) {
    row.set_selected(list_density().dropdown_index());
    row.connect_notify_local(Some("selected"), move |row, _| {
        set_list_density(ListDensity::from_dropdown_index(row.selected()));
    });
}

pub fn on_list_density_changed<F: Fn(ListDensity) + 'static>(callback: F) {
    with_settings(|settings| {
        settings.connect_changed(Some("list-density"), move |settings, _| {
            callback(ListDensity::from_gsettings(
                &settings.string("list-density"),
            ));
        });
    });
}

pub fn apply_list_density(widget: &impl IsA<relm4::gtk::Widget>, density: ListDensity) {
    for class in ListDensity::ALL_CSS_CLASSES {
        widget.remove_css_class(class);
    }
    widget.add_css_class(density.css_class());
}

pub fn message_list_mode() -> MessageListMode {
    with_settings(|settings| MessageListMode::from_gsettings(&settings.string("message-list-mode")))
}

pub fn set_message_list_mode(mode: MessageListMode) {
    with_settings(|settings| {
        settings
            .set_string("message-list-mode", mode.as_str())
            .unwrap_or_else(|err| log_write_failure("message-list-mode", &err));
    });
}

pub fn bind_message_list_mode(row: &relm4::adw::ComboRow) {
    row.set_selected(message_list_mode_dropdown_index(message_list_mode()));
    row.connect_notify_local(Some("selected"), move |row, _| {
        set_message_list_mode(message_list_mode_from_dropdown_index(row.selected()));
    });
}

pub fn on_message_list_mode_changed<F: Fn(MessageListMode) + 'static>(callback: F) {
    with_settings(|settings| {
        settings.connect_changed(Some("message-list-mode"), move |settings, _| {
            callback(MessageListMode::from_gsettings(
                &settings.string("message-list-mode"),
            ));
        });
    });
}

/// One-shot default for existing installs: flat when accounts exist, else conversation.
pub fn ensure_message_list_mode_default(has_accounts: bool) {
    with_settings(|settings| {
        if settings.user_value("message-list-mode").is_some() {
            return;
        }
        let value = if has_accounts {
            MessageListMode::Flat
        } else {
            MessageListMode::Conversation
        };
        settings
            .set_string("message-list-mode", value.as_str())
            .unwrap_or_else(|err| log_write_failure("message-list-mode", &err));
    });
}

fn message_list_mode_dropdown_index(mode: MessageListMode) -> u32 {
    match mode {
        MessageListMode::Conversation => 0,
        MessageListMode::Flat => 1,
    }
}

fn message_list_mode_from_dropdown_index(index: u32) -> MessageListMode {
    match index {
        1 => MessageListMode::Flat,
        _ => MessageListMode::Conversation,
    }
}

pub const FONT_SIZE_MIN: i32 = 10;
pub const FONT_SIZE_DEFAULT: i32 = 14;
pub const FONT_SIZE_MAX: i32 = 24;

pub fn clamp_font_size(size: i32) -> i32 {
    size.clamp(FONT_SIZE_MIN, FONT_SIZE_MAX)
}

pub fn application_font_size() -> i32 {
    with_settings(|settings| clamp_font_size(settings.int("application-font-size")))
}

pub fn set_application_font_size(size: i32) {
    let size = clamp_font_size(size);
    with_settings(|settings| {
        settings
            .set_int("application-font-size", size)
            .unwrap_or_else(|err| log_write_failure("application-font-size", &err));
    });
}

pub fn on_application_font_size_changed<F: Fn(i32) + 'static>(callback: F) {
    with_settings(|settings| {
        settings.connect_changed(Some("application-font-size"), move |settings, _| {
            callback(clamp_font_size(settings.int("application-font-size")));
        });
    });
}

pub fn bind_application_font_size(row: &relm4::adw::SpinRow) {
    row.set_digits(0);
    row.set_value(application_font_size() as f64);
    row.connect_notify_local(Some("value"), move |row, _| {
        set_application_font_size(row.value() as i32);
    });
}

pub fn reader_font_size() -> i32 {
    with_settings(|settings| clamp_font_size(settings.int("reader-font-size")))
}

pub fn set_reader_font_size(size: i32) {
    let size = clamp_font_size(size);
    with_settings(|settings| {
        settings
            .set_int("reader-font-size", size)
            .unwrap_or_else(|err| log_write_failure("reader-font-size", &err));
    });
}

pub fn on_reader_font_size_changed<F: Fn(i32) + 'static>(callback: F) {
    with_settings(|settings| {
        settings.connect_changed(Some("reader-font-size"), move |settings, _| {
            callback(clamp_font_size(settings.int("reader-font-size")));
        });
    });
}

pub fn bind_reader_font_size(row: &relm4::adw::SpinRow) {
    row.set_digits(0);
    row.set_value(reader_font_size() as f64);
    row.connect_notify_local(Some("value"), move |row, _| {
        set_reader_font_size(row.value() as i32);
    });
}

pub fn editor_font_size() -> i32 {
    with_settings(|settings| clamp_font_size(settings.int("editor-font-size")))
}

pub fn set_editor_font_size(size: i32) {
    let size = clamp_font_size(size);
    with_settings(|settings| {
        settings
            .set_int("editor-font-size", size)
            .unwrap_or_else(|err| log_write_failure("editor-font-size", &err));
    });
}

pub fn on_editor_font_size_changed<F: Fn(i32) + 'static>(callback: F) {
    with_settings(|settings| {
        settings.connect_changed(Some("editor-font-size"), move |settings, _| {
            callback(clamp_font_size(settings.int("editor-font-size")));
        });
    });
}

pub fn bind_editor_font_size(row: &relm4::adw::SpinRow) {
    row.set_digits(0);
    row.set_value(editor_font_size() as f64);
    row.connect_notify_local(Some("value"), move |row, _| {
        set_editor_font_size(row.value() as i32);
    });
}

pub const ACTIVITY_LOG_LIMIT_MIN: i32 = 50;
pub const ACTIVITY_LOG_LIMIT_DEFAULT: i32 = 200;
pub const ACTIVITY_LOG_LIMIT_MAX: i32 = 2000;
pub const ACTIVITY_LOG_FILTER_ALL: i32 = 0x3F; // mail 1, sync 2, accounts 4, (unused 8), contacts 16, failures 32

pub fn clamp_activity_log_limit(limit: i32) -> i32 {
    limit.clamp(ACTIVITY_LOG_LIMIT_MIN, ACTIVITY_LOG_LIMIT_MAX)
}

pub fn activity_log_limit() -> i32 {
    with_settings(|settings| clamp_activity_log_limit(settings.int("activity-log-limit")))
}

pub fn set_activity_log_limit(limit: i32) {
    let limit = clamp_activity_log_limit(limit);
    with_settings(|settings| {
        settings
            .set_int("activity-log-limit", limit)
            .unwrap_or_else(|err| log_write_failure("activity-log-limit", &err));
    });
}

pub fn on_activity_log_limit_changed<F: Fn() + 'static>(callback: F) {
    with_settings(|settings| {
        settings.connect_changed(Some("activity-log-limit"), move |_, _| {
            callback();
        });
    });
}

pub fn bind_activity_log_limit(row: &relm4::adw::SpinRow) {
    row.set_digits(0);
    row.set_value(activity_log_limit() as f64);
    row.connect_notify_local(Some("value"), move |row, _| {
        set_activity_log_limit(row.value() as i32);
    });
}

pub fn clamp_activity_log_filter(mask: i32) -> i32 {
    mask & ACTIVITY_LOG_FILTER_ALL
}

pub fn activity_log_filter() -> i32 {
    with_settings(|settings| clamp_activity_log_filter(settings.int("activity-log-filter")))
}

pub fn set_activity_log_filter(mask: i32) {
    let mask = clamp_activity_log_filter(mask);
    with_settings(|settings| {
        settings
            .set_int("activity-log-filter", mask)
            .unwrap_or_else(|err| log_write_failure("activity-log-filter", &err));
    });
}

pub fn on_activity_log_filter_changed<F: Fn() + 'static>(callback: F) {
    with_settings(|settings| {
        settings.connect_changed(Some("activity-log-filter"), move |_, _| {
            callback();
        });
    });
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorScheme {
    System,
    Light,
    Dark,
}

impl ColorScheme {
    fn from_gsettings(value: &str) -> Self {
        match value {
            "light" => Self::Light,
            "dark" => Self::Dark,
            _ => Self::System,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }

    pub fn to_adw(self) -> adw::ColorScheme {
        match self {
            Self::System => adw::ColorScheme::Default,
            Self::Light => adw::ColorScheme::ForceLight,
            Self::Dark => adw::ColorScheme::ForceDark,
        }
    }

    pub fn dropdown_index(self) -> u32 {
        match self {
            Self::System => 0,
            Self::Light => 1,
            Self::Dark => 2,
        }
    }

    pub fn from_dropdown_index(index: u32) -> Self {
        match index {
            1 => Self::Light,
            2 => Self::Dark,
            _ => Self::System,
        }
    }
}

pub fn color_scheme() -> ColorScheme {
    with_settings(|settings| ColorScheme::from_gsettings(&settings.string("color-scheme")))
}

pub fn set_color_scheme(scheme: ColorScheme) {
    with_settings(|settings| {
        settings
            .set_string("color-scheme", scheme.as_str())
            .unwrap_or_else(|err| log_write_failure("color-scheme", &err));
    });
}

pub fn on_color_scheme_changed<F: Fn(ColorScheme) + 'static>(callback: F) {
    with_settings(|settings| {
        settings.connect_changed(Some("color-scheme"), move |settings, _| {
            callback(ColorScheme::from_gsettings(
                &settings.string("color-scheme"),
            ));
        });
    });
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccentColor {
    Default,
    Blue,
    Teal,
    Green,
    Yellow,
    Orange,
    Red,
    Pink,
    Purple,
    Slate,
}

impl AccentColor {
    fn from_gsettings(value: &str) -> Self {
        match value {
            "blue" => Self::Blue,
            "teal" => Self::Teal,
            "green" => Self::Green,
            "yellow" => Self::Yellow,
            "orange" => Self::Orange,
            "red" => Self::Red,
            "pink" => Self::Pink,
            "purple" => Self::Purple,
            "slate" => Self::Slate,
            _ => Self::Default,
        }
    }

    /// GSettings value; also the `--accent-<name>` CSS variable suffix and the
    /// `.alcedo-swatch-<name>` CSS class suffix.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Blue => "blue",
            Self::Teal => "teal",
            Self::Green => "green",
            Self::Yellow => "yellow",
            Self::Orange => "orange",
            Self::Red => "red",
            Self::Pink => "pink",
            Self::Purple => "purple",
            Self::Slate => "slate",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Default => trs!("Default"),
            Self::Blue => trs!("Blue"),
            Self::Teal => trs!("Teal"),
            Self::Green => trs!("Green"),
            Self::Yellow => trs!("Yellow"),
            Self::Orange => trs!("Orange"),
            Self::Red => trs!("Red"),
            Self::Pink => trs!("Pink"),
            Self::Purple => trs!("Purple"),
            Self::Slate => trs!("Slate"),
        }
    }
}

/// The nine named accent choices, excluding [`AccentColor::Default`].
pub const ACCENT_COLORS: [AccentColor; 9] = [
    AccentColor::Blue,
    AccentColor::Teal,
    AccentColor::Green,
    AccentColor::Yellow,
    AccentColor::Orange,
    AccentColor::Red,
    AccentColor::Pink,
    AccentColor::Purple,
    AccentColor::Slate,
];

pub fn accent_color() -> AccentColor {
    with_settings(|settings| AccentColor::from_gsettings(&settings.string("accent-color")))
}

pub fn set_accent_color(color: AccentColor) {
    with_settings(|settings| {
        settings
            .set_string("accent-color", color.as_str())
            .unwrap_or_else(|err| log_write_failure("accent-color", &err));
    });
}

pub fn on_accent_color_changed<F: Fn(AccentColor) + 'static>(callback: F) {
    with_settings(|settings| {
        settings.connect_changed(Some("accent-color"), move |settings, _| {
            callback(AccentColor::from_gsettings(
                &settings.string("accent-color"),
            ));
        });
    });
}

pub(crate) fn encode_collapsed_folders(map: &HashMap<AccountId, HashSet<String>>) -> Vec<String> {
    let mut entries: Vec<String> = map
        .iter()
        .flat_map(|(account_id, folders)| {
            folders
                .iter()
                .filter(|name| !name.is_empty())
                .map(move |name| format!("{}{COLLAPSED_SEP}{name}", account_id.0))
        })
        .collect();
    entries.sort();
    entries
}

pub(crate) fn decode_collapsed_folders(
    entries: impl IntoIterator<Item = impl AsRef<str>>,
) -> HashMap<AccountId, HashSet<String>> {
    let mut map: HashMap<AccountId, HashSet<String>> = HashMap::new();
    for entry in entries {
        let entry = entry.as_ref();
        let Some((account_str, imap_name)) = entry.split_once(COLLAPSED_SEP) else {
            continue;
        };
        if imap_name.is_empty() {
            continue;
        }
        let Ok(uuid) = Uuid::parse_str(account_str) else {
            continue;
        };
        map.entry(AccountId(uuid))
            .or_default()
            .insert(imap_name.to_string());
    }
    map
}

/// Last mail collection shown in the message list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LastFolder {
    UnifiedInbox,
    Mailbox {
        account: AccountId,
        imap_name: String,
    },
    /// A view an extension shows, by its list id.
    Extension {
        id: ExtensionListId,
    },
}

pub(crate) fn encode_last_folder(folder: &LastFolder) -> String {
    match folder {
        LastFolder::UnifiedInbox => "unified".to_string(),
        LastFolder::Extension { id } => format!("view{COLLAPSED_SEP}{}", id.0),
        LastFolder::Mailbox { account, imap_name } if !imap_name.is_empty() => {
            format!("{}{COLLAPSED_SEP}{imap_name}", account.0)
        }
        LastFolder::Mailbox { .. } => "unified".to_string(),
    }
}

pub(crate) fn decode_last_folder(value: &str) -> LastFolder {
    if value.is_empty() {
        return LastFolder::UnifiedInbox;
    }
    if value == "unified" {
        return LastFolder::UnifiedInbox;
    }
    // "smart" is what earlier builds wrote for the same kind of view.
    let view_id = value
        .strip_prefix(&format!("view{COLLAPSED_SEP}"))
        .or_else(|| value.strip_prefix(&format!("smart{COLLAPSED_SEP}")));
    if let Some(uuid_str) = view_id {
        if let Ok(uuid) = Uuid::parse_str(uuid_str) {
            return LastFolder::Extension {
                id: ExtensionListId(uuid),
            };
        }
        return LastFolder::UnifiedInbox;
    }
    let Some((account_str, imap_name)) = value.split_once(COLLAPSED_SEP) else {
        return LastFolder::UnifiedInbox;
    };
    if imap_name.is_empty() {
        return LastFolder::UnifiedInbox;
    }
    let Ok(uuid) = Uuid::parse_str(account_str) else {
        return LastFolder::UnifiedInbox;
    };
    LastFolder::Mailbox {
        account: AccountId(uuid),
        imap_name: imap_name.to_string(),
    }
}

pub fn last_folder() -> LastFolder {
    with_settings(|settings| decode_last_folder(&settings.string("last-folder")))
}

pub fn set_last_folder(folder: &LastFolder) {
    let encoded = encode_last_folder(folder);
    with_settings(|settings| {
        settings
            .set_string("last-folder", &encoded)
            .unwrap_or_else(|err| log_write_failure("last-folder", &err));
    });
    gio::Settings::sync();
}

const LAST_ACCOUNT_KEY: &str = "last-account";

pub(crate) fn decode_last_account(value: &str) -> Option<AccountId> {
    Uuid::parse_str(value).ok().map(AccountId)
}

/// Account last shown in the switcher. A missing key (an older installed
/// schema) reads as none.
pub fn last_account() -> Option<AccountId> {
    with_settings(|settings| {
        let has_key = settings
            .settings_schema()
            .is_some_and(|schema| schema.has_key(LAST_ACCOUNT_KEY));
        has_key
            .then(|| decode_last_account(&settings.string(LAST_ACCOUNT_KEY)))
            .flatten()
    })
}

/// Remember the switcher account; `None` forgets it.
pub fn set_last_account(account: Option<AccountId>) {
    let encoded = account.map(|id| id.0.to_string()).unwrap_or_default();
    with_settings(|settings| {
        let has_key = settings
            .settings_schema()
            .is_some_and(|schema| schema.has_key(LAST_ACCOUNT_KEY));
        if !has_key {
            return;
        }
        if let Err(err) = settings.set_string(LAST_ACCOUNT_KEY, &encoded) {
            tracing::warn!(%err, "could not save last account");
        }
    });
    gio::Settings::sync();
}

pub fn collapsed_folders() -> HashMap<AccountId, HashSet<String>> {
    with_settings(|settings| decode_collapsed_folders(settings.strv("collapsed-folders")))
}

pub fn set_collapsed_folders(map: &HashMap<AccountId, HashSet<String>>) {
    let encoded = encode_collapsed_folders(map);
    let refs: Vec<&str> = encoded.iter().map(String::as_str).collect();
    with_settings(|settings| {
        settings
            .set_strv("collapsed-folders", refs)
            .unwrap_or_else(|err| log_write_failure("collapsed-folders", &err));
    });
}

pub fn collapsed_accounts() -> HashSet<AccountId> {
    with_settings(|settings| decode_account_ids(settings.strv("collapsed-accounts")))
        .into_iter()
        .collect()
}

pub fn set_collapsed_accounts(accounts: &HashSet<AccountId>) {
    let mut ids: Vec<AccountId> = accounts.iter().copied().collect();
    ids.sort_by_key(|id| id.0);
    write_account_ids("collapsed-accounts", &ids);
}

pub fn collapsed_personal_folders() -> HashSet<AccountId> {
    with_settings(|settings| decode_account_ids(settings.strv("collapsed-personal-folders")))
        .into_iter()
        .collect()
}

pub fn set_collapsed_personal_folders(accounts: &HashSet<AccountId>) {
    let mut ids: Vec<AccountId> = accounts.iter().copied().collect();
    ids.sort_by_key(|id| id.0);
    write_account_ids("collapsed-personal-folders", &ids);
}

pub fn hidden_accounts() -> HashSet<AccountId> {
    with_settings(|settings| decode_account_ids(settings.strv("hidden-accounts")))
        .into_iter()
        .collect()
}

pub fn set_hidden_accounts(accounts: &HashSet<AccountId>) {
    let mut ids: Vec<AccountId> = accounts.iter().copied().collect();
    ids.sort_by_key(|id| id.0);
    write_account_ids("hidden-accounts", &ids);
}

pub fn account_order() -> Vec<AccountId> {
    with_settings(|settings| decode_account_ids(settings.strv("account-order")))
}

pub fn set_account_order(order: &[AccountId]) {
    write_account_ids("account-order", order);
}

fn write_account_ids(key: &str, ids: &[AccountId]) {
    let encoded = encode_account_ids(ids);
    let refs: Vec<&str> = encoded.iter().map(String::as_str).collect();
    with_settings(|settings| {
        settings
            .set_strv(key, refs)
            .unwrap_or_else(|err| log_write_failure(key, &err));
    });
}

pub(crate) fn encode_account_ids(ids: &[AccountId]) -> Vec<String> {
    ids.iter().map(|id| id.0.to_string()).collect()
}

/// Valid UUIDs in stored order; junk and repeats are dropped.
pub(crate) fn decode_account_ids(
    entries: impl IntoIterator<Item = impl AsRef<str>>,
) -> Vec<AccountId> {
    let mut seen = HashSet::new();
    entries
        .into_iter()
        .filter_map(|entry| decode_last_account(entry.as_ref()))
        .filter(|id| seen.insert(*id))
        .collect()
}

fn compose_format_overrides() -> HashMap<AccountId, ComposeFormat> {
    with_settings(|settings| {
        decode_compose_format_overrides(settings.strv("compose-format-by-account"))
    })
}

fn set_compose_format_overrides(map: &HashMap<AccountId, ComposeFormat>) {
    let encoded = encode_compose_format_overrides(map);
    let refs: Vec<&str> = encoded.iter().map(String::as_str).collect();
    with_settings(|settings| {
        settings
            .set_strv("compose-format-by-account", refs)
            .unwrap_or_else(|err| log_write_failure("compose-format-by-account", &err));
    });
}

pub(crate) fn encode_compose_format_overrides(
    map: &HashMap<AccountId, ComposeFormat>,
) -> Vec<String> {
    let mut entries: Vec<String> = map
        .iter()
        .map(|(account_id, format)| format!("{}{COLLAPSED_SEP}{}", account_id.0, format.as_str()))
        .collect();
    entries.sort();
    entries
}

pub(crate) fn decode_compose_format_overrides(
    entries: impl IntoIterator<Item = impl AsRef<str>>,
) -> HashMap<AccountId, ComposeFormat> {
    let mut map = HashMap::new();
    for entry in entries {
        let entry = entry.as_ref();
        let Some((account_str, format_str)) = entry.split_once(COLLAPSED_SEP) else {
            continue;
        };
        if format_str.is_empty() {
            continue;
        }
        let Ok(uuid) = Uuid::parse_str(account_str) else {
            continue;
        };
        map.insert(AccountId(uuid), ComposeFormat::from_gsettings(format_str));
    }
    map
}

pub fn keyboard_shortcut_overrides() -> HashMap<String, String> {
    with_settings(|settings| {
        decode_keyboard_shortcut_overrides(settings.strv("keyboard-shortcut-overrides"))
    })
}

pub fn set_keyboard_shortcut_overrides(map: &HashMap<String, String>) {
    let encoded = encode_keyboard_shortcut_overrides(map);
    let refs: Vec<&str> = encoded.iter().map(String::as_str).collect();
    with_settings(|settings| {
        settings
            .set_strv("keyboard-shortcut-overrides", refs)
            .unwrap_or_else(|err| log_write_failure("keyboard-shortcut-overrides", &err));
    });
}

pub fn clear_keyboard_shortcut_overrides() {
    with_settings(|settings| {
        settings
            .set_strv("keyboard-shortcut-overrides", &[] as &[&str])
            .unwrap_or_else(|err| log_write_failure("keyboard-shortcut-overrides", &err));
    });
}

pub fn on_keyboard_shortcut_overrides_changed<F: Fn() + 'static>(callback: F) {
    with_settings(|settings| {
        settings.connect_changed(Some("keyboard-shortcut-overrides"), move |_, _| {
            callback();
        });
    });
}

pub(crate) fn encode_keyboard_shortcut_overrides(map: &HashMap<String, String>) -> Vec<String> {
    let mut entries: Vec<String> = map
        .iter()
        .filter(|(id, trigger)| !id.is_empty() && !trigger.is_empty())
        .map(|(id, trigger)| format!("{id}{COLLAPSED_SEP}{trigger}"))
        .collect();
    entries.sort();
    entries
}

pub(crate) fn decode_keyboard_shortcut_overrides(
    entries: impl IntoIterator<Item = impl AsRef<str>>,
) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for entry in entries {
        let entry = entry.as_ref();
        let Some((id, trigger)) = entry.split_once(COLLAPSED_SEP) else {
            continue;
        };
        if id.is_empty() || trigger.is_empty() {
            continue;
        }
        map.insert(id.to_string(), trigger.to_string());
    }
    map
}

pub fn recent_move_folders() -> Vec<i64> {
    with_settings(|settings| {
        parse_recent_move_folder_ids(
            settings
                .strv("recent-move-folders")
                .iter()
                .map(|value| value.as_str()),
        )
    })
}

pub fn push_recent_move_folder(id: i64) {
    let mut recents = recent_move_folders();
    push_recent_move_folder_in_place(&mut recents, id);
    let owned: Vec<String> = recents
        .iter()
        .map(|folder_id| folder_id.to_string())
        .collect();
    let refs: Vec<&str> = owned.iter().map(String::as_str).collect();
    with_settings(|settings| {
        settings
            .set_strv("recent-move-folders", refs)
            .unwrap_or_else(|err| log_write_failure("recent-move-folders", &err));
    });
}

fn parse_recent_move_folder_ids(values: impl IntoIterator<Item = impl AsRef<str>>) -> Vec<i64> {
    values
        .into_iter()
        .filter_map(|value| value.as_ref().parse::<i64>().ok())
        .collect()
}

fn push_recent_move_folder_in_place(recents: &mut Vec<i64>, id: i64) {
    recents.retain(|existing| *existing != id);
    recents.insert(0, id);
    recents.truncate(5);
}

fn message_list_column_min_width(id: &str) -> Option<i32> {
    match id {
        "from" => Some(80),
        "folder" => Some(80),
        "account" => Some(80),
        "subject" => Some(120),
        "date" => Some(96),
        "size" => Some(96),
        "deleted" => Some(96),
        "attachments" | "star" => Some(24),
        _ => None,
    }
}

pub(crate) fn encode_message_list_column_widths(map: &HashMap<String, i32>) -> Vec<String> {
    let mut entries: Vec<String> = map
        .iter()
        .map(|(id, width)| format!("{id}:{width}"))
        .collect();
    entries.sort();
    entries
}

pub(crate) fn decode_message_list_column_widths(
    entries: impl IntoIterator<Item = impl AsRef<str>>,
) -> HashMap<String, i32> {
    let mut map = HashMap::new();
    for entry in entries {
        let entry = entry.as_ref();
        let Some((id, width_str)) = entry.split_once(':') else {
            continue;
        };
        if id.is_empty() {
            continue;
        }
        let Ok(width) = width_str.parse::<i32>() else {
            continue;
        };
        if width <= 0 {
            continue;
        }
        let Some(min) = message_list_column_min_width(id) else {
            continue;
        };
        if width < min {
            continue;
        }
        map.insert(id.to_string(), width);
    }
    map
}

pub fn message_list_column_widths() -> HashMap<String, i32> {
    with_settings(|settings| {
        decode_message_list_column_widths(settings.strv("message-list-column-widths"))
    })
}

pub fn set_message_list_column_widths(map: &HashMap<String, i32>) {
    let mut rows = encode_message_list_column_widths(map);
    rows.extend(scoped_column_rows(&column_setting_rows(
        "message-list-column-widths",
    )));
    set_column_setting_rows("message-list-column-widths", &rows);
}

/// Mailbox or virtual view whose message-list columns are stored separately.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MessageListColumnScope {
    UnifiedInbox,
    Search,
    /// An extension view, by the scope key the extension chose.
    Extension(String),
    Mailbox {
        account: AccountId,
        imap_name: String,
    },
    /// Folder row id when the mailbox record is not loaded yet.
    Folder(FolderId),
}

pub(crate) fn encode_message_list_column_scope(scope: &MessageListColumnScope) -> String {
    match scope {
        MessageListColumnScope::UnifiedInbox => "unified".to_string(),
        MessageListColumnScope::Search => "search".to_string(),
        MessageListColumnScope::Extension(key) => key.clone(),
        MessageListColumnScope::Mailbox { account, imap_name } => {
            format!(
                "mbox:{}:{}",
                account.0,
                percent_encode_column_scope(imap_name)
            )
        }
        MessageListColumnScope::Folder(id) => format!("fid:{}", id.0),
    }
}

pub(crate) fn percent_encode_column_scope(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[cfg(test)]
pub(crate) fn percent_decode_column_scope(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut out = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                return None;
            }
            let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).ok()?;
            let byte = u8::from_str_radix(hex, 16).ok()?;
            out.push(byte);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn column_scope_prefix(scope: &MessageListColumnScope) -> String {
    format!("{}{COLLAPSED_SEP}", encode_message_list_column_scope(scope))
}

/// Keep per-mailbox rows. Drop one scope when `payload` is empty.
pub(crate) fn upsert_scoped_column_row(
    rows: &mut Vec<String>,
    scope: &MessageListColumnScope,
    payload: &str,
) {
    let prefix = column_scope_prefix(scope);
    rows.retain(|row| !row.starts_with(&prefix));
    if !payload.is_empty() {
        rows.push(format!("{prefix}{payload}"));
    }
}

pub(crate) fn scoped_column_payload(
    rows: &[String],
    scope: &MessageListColumnScope,
) -> Option<String> {
    let prefix = column_scope_prefix(scope);
    rows.iter().find_map(|row| {
        row.strip_prefix(&prefix)
            .filter(|payload| !payload.is_empty())
            .map(str::to_string)
    })
}

fn scoped_column_rows(rows: &[String]) -> Vec<String> {
    let mut scoped: Vec<String> = rows
        .iter()
        .filter(|row| row.contains(COLLAPSED_SEP))
        .cloned()
        .collect();
    scoped.sort();
    scoped
}

fn column_setting_rows(key: &str) -> Vec<String> {
    with_settings(|settings| {
        settings
            .strv(key)
            .iter()
            .map(|row| row.to_string())
            .collect()
    })
}

fn set_column_setting_rows(key: &str, rows: &[String]) {
    let refs: Vec<&str> = rows.iter().map(String::as_str).collect();
    with_settings(|settings| {
        settings
            .set_strv(key, refs)
            .unwrap_or_else(|err| log_write_failure(key, &err));
    });
}

fn write_scoped_column_payload(key: &str, scope: &MessageListColumnScope, payload: &str) {
    let mut rows = column_setting_rows(key);
    upsert_scoped_column_row(&mut rows, scope, payload);
    set_column_setting_rows(key, &rows);
}

pub fn message_list_column_widths_for(scope: &MessageListColumnScope) -> HashMap<String, i32> {
    let rows = column_setting_rows("message-list-column-widths");
    match scoped_column_payload(&rows, scope) {
        Some(payload) => decode_message_list_column_widths(payload.split(',')),
        None => decode_message_list_column_widths(
            rows.iter().filter(|row| !row.contains(COLLAPSED_SEP)),
        ),
    }
}

pub fn set_message_list_column_widths_for(
    scope: &MessageListColumnScope,
    map: &HashMap<String, i32>,
) {
    let payload = encode_message_list_column_widths(map).join(",");
    write_scoped_column_payload("message-list-column-widths", scope, &payload);
}

pub const FOLDER_PANE_MIN: i32 = 180;
pub const FOLDER_PANE_DEFAULT: i32 = 240;
pub const FOLDER_PANE_MAX: i32 = 560;

pub const MESSAGE_LIST_USER_COLUMNS: &[&str] = &[
    "unread",
    "from",
    "folder",
    "account",
    "account-chip",
    "subject",
    "attachments",
    "star",
    "size",
    "date",
];

pub const MESSAGE_LIST_CONTEXT_COLUMNS: &[&str] = &["folder", "account", "account-chip"];
pub const MESSAGE_LIST_DEFAULT_VISIBLE: &[&str] = &[
    "unread",
    "from",
    "subject",
    "attachments",
    "star",
    "size",
    "date",
];
pub const MESSAGE_LIST_TEXT_COLUMNS: &[&str] = &[
    "from", "folder", "account", "subject", "size", "date", "deleted",
];
pub const MESSAGE_LIST_RESIZABLE_COLUMNS: &[&str] =
    &["from", "folder", "account", "size", "date", "deleted"];

pub fn clamp_folder_pane_width(width: i32) -> i32 {
    width.clamp(FOLDER_PANE_MIN, FOLDER_PANE_MAX)
}

pub fn folder_pane_width() -> i32 {
    with_settings(|settings| clamp_folder_pane_width(settings.int("folder-pane-width")))
}

pub fn set_folder_pane_width(width: i32) {
    let width = clamp_folder_pane_width(width);
    with_settings(|settings| {
        settings
            .set_int("folder-pane-width", width)
            .unwrap_or_else(|err| log_write_failure("folder-pane-width", &err));
    });
}

pub fn decode_message_list_column_order(
    entries: impl IntoIterator<Item = impl AsRef<str>>,
) -> Vec<String> {
    let known: HashSet<&str> = MESSAGE_LIST_USER_COLUMNS.iter().copied().collect();
    let mut result = Vec::new();
    let mut seen = HashSet::new();
    let mut had_folder = false;
    let mut had_account = false;
    let mut had_account_chip = false;
    let mut had_size = false;
    for entry in entries {
        let id = entry.as_ref();
        if id == "folder" {
            had_folder = true;
        }
        if id == "account" {
            had_account = true;
        }
        if id == "account-chip" {
            had_account_chip = true;
        }
        if id == "size" {
            had_size = true;
        }
        if !known.contains(id) || !seen.insert(id.to_string()) {
            continue;
        }
        result.push(id.to_string());
    }
    for id in MESSAGE_LIST_USER_COLUMNS {
        if !seen.contains(*id) {
            result.push(id.to_string());
        }
    }
    if !had_folder
        && let Some(from_idx) = result.iter().position(|id| id == "from")
        && let Some(folder_idx) = result.iter().position(|id| id == "folder")
        && folder_idx != from_idx + 1
    {
        result.remove(folder_idx);
        let insert_at = result
            .iter()
            .position(|id| id == "from")
            .map(|idx| idx + 1)
            .unwrap_or(result.len());
        result.insert(insert_at, "folder".to_string());
    }
    if !had_account {
        result.retain(|id| id != "account");
        if let Some(folder_idx) = result.iter().position(|id| id == "folder") {
            result.insert(folder_idx + 1, "account".to_string());
        }
    }
    if !had_account_chip {
        result.retain(|id| id != "account-chip");
        if let Some(account_idx) = result.iter().position(|id| id == "account") {
            result.insert(account_idx + 1, "account-chip".to_string());
        }
    }
    if !had_size {
        if let Some(size_idx) = result.iter().position(|id| id == "size") {
            result.remove(size_idx);
        }
        if let Some(date_idx) = result.iter().position(|id| id == "date") {
            result.insert(date_idx, "size".to_string());
        } else {
            result.push("size".to_string());
        }
    }
    result
}

pub fn decode_message_list_column_visible(
    entries: impl IntoIterator<Item = impl AsRef<str>>,
) -> HashSet<String> {
    let known: HashSet<&str> = MESSAGE_LIST_USER_COLUMNS.iter().copied().collect();
    let text: HashSet<&str> = MESSAGE_LIST_TEXT_COLUMNS.iter().copied().collect();
    let visible: HashSet<String> = entries
        .into_iter()
        .map(|entry| entry.as_ref().to_string())
        .filter(|id| known.contains(id.as_str()))
        .collect();
    if visible.is_empty() || visible.iter().all(|id| !text.contains(id.as_str())) {
        return MESSAGE_LIST_DEFAULT_VISIBLE
            .iter()
            .map(|id| (*id).to_string())
            .collect();
    }
    visible
}

pub fn message_list_column_order() -> Vec<String> {
    with_settings(|settings| {
        decode_message_list_column_order(settings.strv("message-list-column-order"))
    })
}

pub fn set_message_list_column_order(order: &[String]) {
    let mut rows = decode_message_list_column_order(order.iter().map(String::as_str));
    rows.extend(scoped_column_rows(&column_setting_rows(
        "message-list-column-order",
    )));
    set_column_setting_rows("message-list-column-order", &rows);
}

pub fn message_list_column_order_for(scope: &MessageListColumnScope) -> Vec<String> {
    let rows = column_setting_rows("message-list-column-order");
    match scoped_column_payload(&rows, scope) {
        Some(payload) => decode_message_list_column_order(payload.split(',')),
        None => {
            decode_message_list_column_order(rows.iter().filter(|row| !row.contains(COLLAPSED_SEP)))
        }
    }
}

pub fn set_message_list_column_order_for(scope: &MessageListColumnScope, order: &[String]) {
    let order = decode_message_list_column_order(order.iter().map(String::as_str));
    write_scoped_column_payload("message-list-column-order", scope, &order.join(","));
}

pub fn move_message_list_column(order: &[String], id: &str, delta: i32) -> Vec<String> {
    let mut order = decode_message_list_column_order(order.iter().map(String::as_str));
    let Some(idx) = order.iter().position(|col| col == id) else {
        return order;
    };
    let new_idx = idx as i32 + delta;
    if new_idx < 0 || new_idx >= order.len() as i32 {
        return order;
    }
    order.swap(idx, new_idx as usize);
    order
}

pub fn message_list_column_visible() -> HashSet<String> {
    with_settings(|settings| {
        decode_message_list_column_visible(settings.strv("message-list-column-visible"))
    })
}

pub fn set_message_list_column_visible(visible: &HashSet<String>) {
    let mut rows: Vec<String> = MESSAGE_LIST_USER_COLUMNS
        .iter()
        .copied()
        .filter(|id| visible.contains(*id))
        .map(str::to_string)
        .collect();
    rows.extend(scoped_column_rows(&column_setting_rows(
        "message-list-column-visible",
    )));
    set_column_setting_rows("message-list-column-visible", &rows);
}

pub fn message_list_column_visible_for(scope: &MessageListColumnScope) -> HashSet<String> {
    let rows = column_setting_rows("message-list-column-visible");
    match scoped_column_payload(&rows, scope) {
        Some(payload) => decode_message_list_column_visible(payload.split(',')),
        None => decode_message_list_column_visible(
            rows.iter().filter(|row| !row.contains(COLLAPSED_SEP)),
        ),
    }
}

pub fn set_message_list_column_visible_for(
    scope: &MessageListColumnScope,
    visible: &HashSet<String>,
) {
    let payload = MESSAGE_LIST_USER_COLUMNS
        .iter()
        .copied()
        .filter(|id| visible.contains(*id))
        .collect::<Vec<_>>()
        .join(",");
    write_scoped_column_payload("message-list-column-visible", scope, &payload);
}

/// Merge one column picker toggle into the saved visible set (read → merge → write).
pub fn toggle_message_list_column_visible(id: &str, visible: bool) {
    if !MESSAGE_LIST_USER_COLUMNS.contains(&id) {
        return;
    }
    let mut saved = message_list_column_visible();
    if visible {
        saved.insert(id.to_string());
    } else {
        saved.remove(id);
    }
    set_message_list_column_visible(&saved);
}

/// Folder, account, and account-chip columns stay on during cross-folder views; otherwise honor the picker.
pub fn message_list_context_column_effective_visible(
    id: &str,
    searching: bool,
    saved: &HashSet<String>,
) -> bool {
    debug_assert!(MESSAGE_LIST_CONTEXT_COLUMNS.contains(&id));
    searching || saved.contains(id)
}

/// Folder column is always shown during search; otherwise honor the column picker.
pub fn folder_column_effective_visible(searching: bool, saved: &HashSet<String>) -> bool {
    message_list_context_column_effective_visible("folder", searching, saved)
}

pub fn decode_message_list_sort_column(value: &str) -> String {
    match value {
        "from" | "subject" | "date" | "size" => value.to_string(),
        _ => "date".to_string(),
    }
}

pub fn decode_message_list_sort_order(value: &str) -> String {
    match value {
        "ascending" | "descending" => value.to_string(),
        _ => "descending".to_string(),
    }
}

pub fn message_list_sort_column() -> String {
    with_settings(|settings| {
        decode_message_list_sort_column(&settings.string("message-list-sort-column"))
    })
}

pub fn message_list_sort_order() -> String {
    with_settings(|settings| {
        decode_message_list_sort_order(&settings.string("message-list-sort-order"))
    })
}

pub fn set_message_list_sort_column(column: &str) {
    let column = decode_message_list_sort_column(column);
    with_settings(|settings| {
        settings
            .set_string("message-list-sort-column", &column)
            .unwrap_or_else(|err| log_write_failure("message-list-sort-column", &err));
    });
}

pub fn set_message_list_sort_order(order: &str) {
    let order = decode_message_list_sort_order(order);
    with_settings(|settings| {
        settings
            .set_string("message-list-sort-order", &order)
            .unwrap_or_else(|err| log_write_failure("message-list-sort-order", &err));
    });
}

pub fn wire_paned_persist<F, G>(
    paned: &gtk::Paned,
    skip_if_paned_hidden: bool,
    clamp: fn(i32) -> i32,
    on_position: F,
    persist: G,
) where
    F: Fn(i32) + 'static,
    G: Fn(i32) + 'static,
{
    let on_position = Rc::new(on_position);
    let persist = Rc::new(persist);
    let debounce = Rc::new(Cell::new(None::<glib::SourceId>));
    let clamping = Rc::new(Cell::new(false));
    paned.connect_notify_local(Some("position"), {
        let debounce = debounce.clone();
        let clamping = clamping.clone();
        let on_position = on_position.clone();
        let persist = persist.clone();
        move |paned, _| {
            if (skip_if_paned_hidden && !paned.is_visible())
                || paned.start_child().is_none_or(|child| !child.is_visible())
            {
                return;
            }
            let size = clamp(paned.position());
            on_position(size);
            if paned.position() != size {
                if !clamping.get() {
                    clamping.set(true);
                    paned.set_position(size);
                    clamping.set(false);
                }
                return;
            }
            crate::glib_source::cancel_source(&debounce);
            let debounce_cb = debounce.clone();
            let persist = persist.clone();
            let source_id = glib::timeout_add_local(Duration::from_millis(200), move || {
                let _ = debounce_cb.take();
                persist(size);
                glib::ControlFlow::Break
            });
            debounce.set(Some(source_id));
        }
    });
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};

    use alcedo_core::{AccountId, ComposeFormat, ExtensionListId};
    use uuid::Uuid;

    #[test]
    fn compiled_schema_is_readable_without_system_install() {
        assert_eq!(super::send_delay_seconds(), 0);
        assert!(!super::insert_tracking_pixel());
    }

    /// Privacy defaults (L7): outbound tracking and read receipts stay off
    /// until the user turns them on.
    #[test]
    fn schema_keeps_outbound_tracking_off_by_default() {
        let schema = include_str!("../../../data/dev.alcedo.Mail.gschema.xml");
        for key in ["insert-tracking-pixel", "request-mdn"] {
            let start = schema
                .find(&format!("<key name=\"{key}\""))
                .unwrap_or_else(|| panic!("{key} missing from schema"));
            let block = &schema[start..start + schema[start..].find("</key>").unwrap()];
            assert!(
                block.contains("<default>false</default>"),
                "{key} must default to false"
            );
        }
    }

    #[test]
    fn schema_send_delay_defaults_off() {
        let schema = include_str!("../../../data/dev.alcedo.Mail.gschema.xml");
        let start = schema
            .find("<key name=\"send-delay-seconds\"")
            .expect("send-delay-seconds missing from schema");
        let block = &schema[start..start + schema[start..].find("</key>").unwrap()];
        assert!(block.contains("<range min=\"0\" max=\"60\"/>"));
        assert!(block.contains("<default>0</default>"));
    }

    use super::{
        ACCENT_COLORS, AccentColor, COLLAPSED_SEP, ColorScheme, FolderPaneAccounts, LastFolder,
        ListDensity, MESSAGE_LIST_USER_COLUMNS, ReadingPane, clamp_activity_log_filter,
        clamp_activity_log_limit, clamp_folder_pane_width, clamp_font_size, clamp_rail_width,
        decode_account_ids, decode_collapsed_folders, decode_compose_format_overrides,
        decode_keyboard_shortcut_overrides, decode_last_account, decode_last_folder,
        decode_message_list_column_order, decode_message_list_column_visible,
        decode_message_list_column_widths, decode_message_list_sort_column,
        decode_message_list_sort_order, encode_account_ids, encode_collapsed_folders,
        encode_compose_format_overrides, encode_keyboard_shortcut_overrides, encode_last_folder,
        encode_message_list_column_scope, encode_message_list_column_widths,
        move_message_list_column, percent_decode_column_scope, resolved_rail_width,
        scoped_column_payload, upsert_scoped_column_row,
    };

    #[test]
    fn reading_pane_from_gsettings() {
        assert_eq!(ReadingPane::from_gsettings("right"), ReadingPane::Right);
        assert_eq!(ReadingPane::from_gsettings("bottom"), ReadingPane::Bottom);
        assert_eq!(ReadingPane::from_gsettings(""), ReadingPane::Right);
        assert_eq!(ReadingPane::from_gsettings("left"), ReadingPane::Right);
    }

    #[test]
    fn list_density_from_gsettings() {
        assert_eq!(ListDensity::from_gsettings("compact"), ListDensity::Compact);
        assert_eq!(
            ListDensity::from_gsettings("comfortable"),
            ListDensity::Comfortable,
        );
        assert_eq!(ListDensity::from_gsettings("relaxed"), ListDensity::Relaxed);
        assert_eq!(ListDensity::from_gsettings(""), ListDensity::Comfortable);
        assert_eq!(
            ListDensity::from_gsettings("tight"),
            ListDensity::Comfortable
        );
    }

    #[test]
    fn color_scheme_from_gsettings() {
        assert_eq!(ColorScheme::from_gsettings("system"), ColorScheme::System);
        assert_eq!(ColorScheme::from_gsettings("light"), ColorScheme::Light);
        assert_eq!(ColorScheme::from_gsettings("dark"), ColorScheme::Dark);
        assert_eq!(ColorScheme::from_gsettings(""), ColorScheme::System);
        assert_eq!(ColorScheme::from_gsettings("auto"), ColorScheme::System);
    }

    #[test]
    fn color_scheme_dropdown_round_trip() {
        assert_eq!(ColorScheme::System.dropdown_index(), 0);
        assert_eq!(ColorScheme::Light.dropdown_index(), 1);
        assert_eq!(ColorScheme::Dark.dropdown_index(), 2);
        assert_eq!(ColorScheme::from_dropdown_index(0), ColorScheme::System);
        assert_eq!(ColorScheme::from_dropdown_index(1), ColorScheme::Light);
        assert_eq!(ColorScheme::from_dropdown_index(2), ColorScheme::Dark);
        assert_eq!(ColorScheme::from_dropdown_index(99), ColorScheme::System);
    }

    #[test]
    fn accent_color_from_gsettings_round_trip() {
        assert_eq!(
            AccentColor::from_gsettings(AccentColor::Default.as_str()),
            AccentColor::Default
        );
        for accent in ACCENT_COLORS {
            assert_eq!(AccentColor::from_gsettings(accent.as_str()), accent);
            assert_eq!(accent.as_str(), accent.as_str());
        }
        assert_eq!(AccentColor::from_gsettings(""), AccentColor::Default);
        assert_eq!(AccentColor::from_gsettings("unknown"), AccentColor::Default);
    }

    #[test]
    fn list_density_dropdown_round_trip() {
        assert_eq!(ListDensity::Compact.dropdown_index(), 0);
        assert_eq!(ListDensity::Comfortable.dropdown_index(), 1);
        assert_eq!(ListDensity::Relaxed.dropdown_index(), 2);
        assert_eq!(ListDensity::from_dropdown_index(0), ListDensity::Compact);
        assert_eq!(
            ListDensity::from_dropdown_index(1),
            ListDensity::Comfortable
        );
        assert_eq!(ListDensity::from_dropdown_index(2), ListDensity::Relaxed);
        assert_eq!(
            ListDensity::from_dropdown_index(99),
            ListDensity::Comfortable
        );
    }

    #[test]
    fn last_folder_virtual_views_decode() {
        let view_id = ExtensionListId(Uuid::from_u128(16));
        assert_eq!(
            decode_last_folder(&format!("view{COLLAPSED_SEP}{}", view_id.0)),
            LastFolder::Extension { id: view_id }
        );
        assert_eq!(
            decode_last_folder(&format!("smart{COLLAPSED_SEP}{}", view_id.0)),
            LastFolder::Extension { id: view_id },
            "values written by earlier builds still restore"
        );
        assert_eq!(
            encode_last_folder(&LastFolder::Extension { id: view_id }),
            format!("view{COLLAPSED_SEP}{}", view_id.0)
        );
    }

    #[test]
    fn last_folder_round_trip() {
        let account = AccountId(Uuid::parse_str("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa").unwrap());
        let mailbox = LastFolder::Mailbox {
            account,
            imap_name: "INBOX".to_string(),
        };
        assert_eq!(decode_last_folder(&encode_last_folder(&mailbox)), mailbox);
        assert_eq!(
            decode_last_folder(&encode_last_folder(&LastFolder::UnifiedInbox)),
            LastFolder::UnifiedInbox
        );
    }

    #[test]
    fn last_account_decodes_uuid_and_rejects_junk() {
        let id = Uuid::parse_str("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa").unwrap();
        assert_eq!(decode_last_account(&id.to_string()), Some(AccountId(id)));
        assert_eq!(decode_last_account(""), None);
        assert_eq!(decode_last_account("nope"), None);
    }

    #[test]
    fn last_folder_empty_defaults_to_unified_inbox() {
        assert_eq!(decode_last_folder(""), LastFolder::UnifiedInbox);
        assert_eq!(decode_last_folder("unified"), LastFolder::UnifiedInbox);
        assert_eq!(
            decode_last_folder("no-such-view"),
            LastFolder::UnifiedInbox,
            "unknown names fall back to Everything"
        );
        assert_eq!(decode_last_folder("not-a-uuid"), LastFolder::UnifiedInbox);
        assert_eq!(
            decode_last_folder("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa\u{1f}"),
            LastFolder::UnifiedInbox
        );
    }

    #[test]
    fn last_folder_empty_imap_encodes_as_unified() {
        let account = AccountId(Uuid::parse_str("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa").unwrap());
        assert_eq!(
            encode_last_folder(&LastFolder::Mailbox {
                account,
                imap_name: String::new(),
            }),
            "unified"
        );
    }

    #[test]
    fn collapsed_folders_round_trip() {
        let account_a = AccountId(Uuid::parse_str("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa").unwrap());
        let account_b = AccountId(Uuid::parse_str("bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb").unwrap());
        let mut map = HashMap::new();
        map.insert(
            account_a,
            HashSet::from(["[Gmail]".to_string(), "Projects".to_string()]),
        );
        map.insert(account_b, HashSet::from(["INBOX".to_string()]));

        let encoded = encode_collapsed_folders(&map);
        let decoded = decode_collapsed_folders(&encoded);
        assert_eq!(decoded, map);
    }

    #[test]
    fn folder_pane_accounts_from_gsettings_defaults_to_picker() {
        assert_eq!(
            FolderPaneAccounts::from_gsettings("stacked"),
            FolderPaneAccounts::Stacked
        );
        assert_eq!(
            FolderPaneAccounts::from_gsettings("picker"),
            FolderPaneAccounts::Picker
        );
        assert_eq!(
            FolderPaneAccounts::from_gsettings(""),
            FolderPaneAccounts::Picker
        );
        assert_eq!(
            FolderPaneAccounts::from_gsettings("junk"),
            FolderPaneAccounts::Picker
        );
    }

    #[test]
    fn folder_pane_accounts_dropdown_round_trip() {
        assert_eq!(FolderPaneAccounts::Picker.dropdown_index(), 0);
        assert_eq!(FolderPaneAccounts::Stacked.dropdown_index(), 1);
        assert_eq!(
            FolderPaneAccounts::from_dropdown_index(0),
            FolderPaneAccounts::Picker
        );
        assert_eq!(
            FolderPaneAccounts::from_dropdown_index(1),
            FolderPaneAccounts::Stacked
        );
        assert_eq!(
            FolderPaneAccounts::from_dropdown_index(7),
            FolderPaneAccounts::Picker
        );
    }

    #[test]
    fn account_ids_round_trip_keeps_order() {
        let a = AccountId(Uuid::parse_str("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa").unwrap());
        let b = AccountId(Uuid::parse_str("bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb").unwrap());
        let c = AccountId(Uuid::parse_str("cccccccc-cccc-cccc-cccc-cccccccccccc").unwrap());
        let ids = [c, a, b];
        let encoded = encode_account_ids(&ids);
        let decoded = decode_account_ids(&encoded);
        assert_eq!(decoded, ids);
    }

    #[test]
    fn decode_account_ids_skips_junk_and_repeats() {
        let a = AccountId(Uuid::parse_str("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa").unwrap());
        let b = AccountId(Uuid::parse_str("bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb").unwrap());
        let decoded = decode_account_ids([
            a.0.to_string(),
            "not-a-uuid".to_string(),
            b.0.to_string(),
            a.0.to_string(),
        ]);
        assert_eq!(decoded, vec![a, b]);
    }

    #[test]
    fn collapsed_folders_empty_map() {
        let map: HashMap<AccountId, HashSet<String>> = HashMap::new();
        assert!(encode_collapsed_folders(&map).is_empty());
        assert!(decode_collapsed_folders([] as [&str; 0]).is_empty());
    }

    #[test]
    fn collapsed_folders_skips_malformed_rows() {
        let account = AccountId(Uuid::parse_str("cccccccc-cccc-cccc-cccc-cccccccccccc").unwrap());
        let valid = format!("{}{COLLAPSED_SEP}INBOX", account.0);
        let empty_imap = format!("{}{COLLAPSED_SEP}", account.0);
        let decoded = decode_collapsed_folders([
            "no-separator",
            "not-a-uuid\u{1f}INBOX",
            empty_imap.as_str(),
            valid.as_str(),
        ]);
        assert_eq!(
            decoded,
            HashMap::from([(account, HashSet::from(["INBOX".to_string()]))]),
        );
    }

    #[test]
    fn collapsed_folders_omits_empty_sets() {
        let account = AccountId(Uuid::parse_str("dddddddd-dddd-dddd-dddd-dddddddddddd").unwrap());
        let mut map = HashMap::new();
        map.insert(account, HashSet::new());
        assert!(encode_collapsed_folders(&map).is_empty());
    }

    #[test]
    fn compose_format_overrides_round_trip() {
        let account_a = AccountId(Uuid::parse_str("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa").unwrap());
        let account_b = AccountId(Uuid::parse_str("bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb").unwrap());
        let map = HashMap::from([
            (account_a, ComposeFormat::Html),
            (account_b, ComposeFormat::Markdown),
        ]);
        let decoded = decode_compose_format_overrides(encode_compose_format_overrides(&map));
        assert_eq!(decoded, map);
    }

    #[test]
    fn compose_format_overrides_skips_malformed_rows() {
        let account = AccountId(Uuid::parse_str("cccccccc-cccc-cccc-cccc-cccccccccccc").unwrap());
        let valid = format!("{}{COLLAPSED_SEP}html", account.0);
        let decoded = decode_compose_format_overrides([
            "no-separator",
            "not-a-uuid\u{1f}html",
            format!("{}{COLLAPSED_SEP}", account.0).as_str(),
            valid.as_str(),
        ]);
        assert_eq!(decoded, HashMap::from([(account, ComposeFormat::Html)]));
    }

    #[test]
    fn collapsed_folders_encode_is_sorted() {
        let account = AccountId(Uuid::parse_str("eeeeeeee-eeee-eeee-eeee-eeeeeeeeeeee").unwrap());
        let mut map = HashMap::new();
        map.insert(
            account,
            HashSet::from(["Zebra".to_string(), "Alpha".to_string()]),
        );
        let encoded = encode_collapsed_folders(&map);
        let mut sorted = encoded.clone();
        sorted.sort();
        assert_eq!(encoded, sorted);
    }

    #[test]
    fn message_list_column_scope_round_trip_mailbox_name() {
        let account = AccountId(Uuid::from_u128(7));
        let scope = super::MessageListColumnScope::Mailbox {
            account,
            imap_name: "INBOX/Sub folder\u{1f}".to_string(),
        };
        let encoded = encode_message_list_column_scope(&scope);
        let imap = encoded
            .strip_prefix(&format!("mbox:{}:", account.0))
            .expect("mailbox prefix");
        assert_eq!(
            percent_decode_column_scope(imap).as_deref(),
            Some("INBOX/Sub folder\u{1f}")
        );
    }

    #[test]
    fn per_view_column_rows_do_not_change_the_shared_layout() {
        let inbox = super::MessageListColumnScope::Mailbox {
            account: AccountId(Uuid::from_u128(1)),
            imap_name: "INBOX".to_string(),
        };
        let everything = super::MessageListColumnScope::UnifiedInbox;
        let mut widths = vec!["from:180".to_string(), "date:120".to_string()];
        upsert_scoped_column_row(&mut widths, &everything, "from:240,date:140");
        upsert_scoped_column_row(&mut widths, &inbox, "from:200");
        assert_eq!(
            decode_message_list_column_widths(
                widths.iter().filter(|row| !row.contains(COLLAPSED_SEP))
            ),
            HashMap::from([("from".to_string(), 180), ("date".to_string(), 120)])
        );
        assert_eq!(
            decode_message_list_column_widths(
                scoped_column_payload(&widths, &everything)
                    .unwrap()
                    .split(',')
            ),
            HashMap::from([("from".to_string(), 240), ("date".to_string(), 140)])
        );
        assert_eq!(
            scoped_column_payload(&widths, &inbox).as_deref(),
            Some("from:200")
        );

        let mut order = vec!["unread".to_string(), "from".to_string(), "date".to_string()];
        upsert_scoped_column_row(&mut order, &everything, "date,from,unread");
        assert_eq!(
            order.iter().take(3).map(String::as_str).collect::<Vec<_>>(),
            vec!["unread", "from", "date"]
        );
        assert!(order.iter().any(|row| row.contains(COLLAPSED_SEP)));

        upsert_scoped_column_row(&mut widths, &inbox, "");
        assert!(scoped_column_payload(&widths, &inbox).is_none());
        assert!(scoped_column_payload(&widths, &everything).is_some());
    }

    #[test]
    fn message_list_column_widths_round_trip() {
        let mut map = HashMap::new();
        map.insert("from".to_string(), 180);
        map.insert("subject".to_string(), 240);
        map.insert("date".to_string(), 140);

        let encoded = encode_message_list_column_widths(&map);
        let decoded = decode_message_list_column_widths(&encoded);
        assert_eq!(decoded, map);
    }

    #[test]
    fn message_list_column_widths_skips_malformed_rows() {
        let decoded = decode_message_list_column_widths([
            "",
            "from",
            "from:abc",
            "from:0",
            "from:-1",
            "from:50",
            "unknown:180",
            "from:180",
        ]);
        assert_eq!(decoded, HashMap::from([("from".to_string(), 180)]));
    }

    #[test]
    fn clamp_folder_pane_width_bounds() {
        assert_eq!(clamp_folder_pane_width(0), 180);
        assert_eq!(clamp_folder_pane_width(240), 240);
        assert_eq!(clamp_folder_pane_width(9999), 560);
        assert_eq!(clamp_folder_pane_width(180), 180);
        assert_eq!(clamp_folder_pane_width(560), 560);
    }

    #[test]
    fn clamp_rail_width_bounds() {
        assert_eq!(clamp_rail_width(0), 56);
        assert_eq!(clamp_rail_width(56), 56);
        assert_eq!(clamp_rail_width(200), 200);
        assert_eq!(clamp_rail_width(9999), 280);
        assert_eq!(clamp_rail_width(9), 56);
    }

    #[test]
    fn clamp_font_size_bounds() {
        assert_eq!(clamp_font_size(9), 10);
        assert_eq!(clamp_font_size(14), 14);
        assert_eq!(clamp_font_size(24), 24);
        assert_eq!(clamp_font_size(25), 24);
    }

    #[test]
    fn clamp_activity_log_limit_bounds() {
        assert_eq!(clamp_activity_log_limit(49), 50);
        assert_eq!(clamp_activity_log_limit(2000), 2000);
        assert_eq!(clamp_activity_log_limit(2001), 2000);
    }

    #[test]
    fn clamp_activity_log_filter_mask() {
        assert_eq!(clamp_activity_log_filter(63), 63);
        assert_eq!(clamp_activity_log_filter(0), 0);
        assert_eq!(clamp_activity_log_filter(64), 0);
        assert_eq!(clamp_activity_log_filter(64 | 1), 1);
    }

    #[test]
    fn resolved_rail_width_migration() {
        assert_eq!(resolved_rail_width(0, false), 56);
        assert_eq!(resolved_rail_width(0, true), 240);
        assert_eq!(resolved_rail_width(200, false), 200);
        assert_eq!(resolved_rail_width(9, true), 56);
    }

    #[test]
    fn message_list_column_order_decode() {
        let decoded = decode_message_list_column_order(["date", "from", "unknown", "from"]);
        assert_eq!(
            decoded,
            vec![
                "size".to_string(),
                "date".to_string(),
                "from".to_string(),
                "folder".to_string(),
                "account".to_string(),
                "account-chip".to_string(),
                "unread".to_string(),
                "subject".to_string(),
                "attachments".to_string(),
                "star".to_string(),
            ],
        );
    }

    #[test]
    fn message_list_column_order_inserts_folder_after_from() {
        let decoded = decode_message_list_column_order(["unread", "from", "subject", "date"]);
        assert_eq!(decoded.iter().position(|id| id == "from"), Some(1),);
        assert_eq!(
            decoded
                .get(decoded.iter().position(|id| id == "from").unwrap() + 1)
                .map(String::as_str),
            Some("folder"),
        );
    }

    #[test]
    fn message_list_column_order_inserts_account_after_folder() {
        let decoded = decode_message_list_column_order(["unread", "from", "subject", "date"]);
        let folder_idx = decoded
            .iter()
            .position(|id| id == "folder")
            .expect("folder");
        assert_eq!(
            decoded.get(folder_idx + 1).map(String::as_str),
            Some("account")
        );
    }

    #[test]
    fn message_list_column_order_inserts_account_chip_after_account() {
        let decoded = decode_message_list_column_order(["unread", "from", "subject", "date"]);
        let account_idx = decoded
            .iter()
            .position(|id| id == "account")
            .expect("account");
        assert_eq!(
            decoded.get(account_idx + 1).map(String::as_str),
            Some("account-chip")
        );
    }

    #[test]
    fn message_list_column_order_inserts_size_before_date() {
        let decoded = decode_message_list_column_order(["unread", "from", "subject", "date"]);
        let date_idx = decoded.iter().position(|id| id == "date").expect("date");
        assert_eq!(decoded.get(date_idx - 1).map(String::as_str), Some("size"));
    }

    #[test]
    fn move_message_list_column_swaps_adjacent() {
        let default: Vec<String> = MESSAGE_LIST_USER_COLUMNS
            .iter()
            .map(|id| (*id).to_string())
            .collect();
        let moved = move_message_list_column(&default, "subject", -1);
        assert_eq!(
            moved,
            vec![
                "unread".to_string(),
                "from".to_string(),
                "folder".to_string(),
                "account".to_string(),
                "subject".to_string(),
                "account-chip".to_string(),
                "attachments".to_string(),
                "star".to_string(),
                "size".to_string(),
                "date".to_string(),
            ],
        );
    }

    #[test]
    fn move_message_list_column_first_cannot_move_up() {
        let default: Vec<String> = MESSAGE_LIST_USER_COLUMNS
            .iter()
            .map(|id| (*id).to_string())
            .collect();
        let moved = move_message_list_column(&default, "unread", -1);
        assert_eq!(moved, default);
    }

    #[test]
    fn move_message_list_column_last_cannot_move_down() {
        let default: Vec<String> = MESSAGE_LIST_USER_COLUMNS
            .iter()
            .map(|id| (*id).to_string())
            .collect();
        let moved = move_message_list_column(&default, "date", 1);
        assert_eq!(moved, default);
    }

    #[test]
    fn move_message_list_column_unknown_id_noops() {
        let default: Vec<String> = MESSAGE_LIST_USER_COLUMNS
            .iter()
            .map(|id| (*id).to_string())
            .collect();
        let moved = move_message_list_column(&default, "flags", -1);
        assert_eq!(moved, default);
    }

    #[test]
    fn move_message_list_column_canonicalizes_before_swap() {
        let garbled = vec![
            "date".to_string(),
            "from".to_string(),
            "unknown".to_string(),
            "from".to_string(),
        ];
        let moved = move_message_list_column(&garbled, "subject", -1);
        assert_eq!(
            moved,
            vec![
                "size".to_string(),
                "date".to_string(),
                "from".to_string(),
                "folder".to_string(),
                "account".to_string(),
                "account-chip".to_string(),
                "subject".to_string(),
                "unread".to_string(),
                "attachments".to_string(),
                "star".to_string(),
            ],
        );
    }

    #[test]
    fn message_list_sort_column_decode() {
        assert_eq!(decode_message_list_sort_column("date"), "date");
        assert_eq!(decode_message_list_sort_column("from"), "from");
        assert_eq!(decode_message_list_sort_column("subject"), "subject");
        assert_eq!(decode_message_list_sort_column("size"), "size");
        assert_eq!(decode_message_list_sort_column(""), "date");
        assert_eq!(decode_message_list_sort_column("flags"), "date");
        assert_eq!(decode_message_list_sort_column("deleted"), "date");
    }

    #[test]
    fn message_list_sort_order_decode() {
        assert_eq!(decode_message_list_sort_order("ascending"), "ascending");
        assert_eq!(decode_message_list_sort_order("descending"), "descending");
        assert_eq!(decode_message_list_sort_order(""), "descending");
        assert_eq!(decode_message_list_sort_order("random"), "descending");
    }

    #[test]
    fn message_list_column_widths_date_min_96() {
        let decoded = decode_message_list_column_widths(["date:80", "date:96", "from:180"]);
        assert_eq!(
            decoded,
            HashMap::from([("date".to_string(), 96), ("from".to_string(), 180),]),
        );
    }

    #[test]
    fn message_list_column_widths_icon_min_24() {
        let decoded = decode_message_list_column_widths([
            "attachments:16",
            "attachments:32",
            "star:20",
            "star:40",
        ]);
        assert_eq!(
            decoded,
            HashMap::from([("attachments".to_string(), 32), ("star".to_string(), 40),]),
        );
    }

    #[test]
    fn message_list_column_widths_folder_min_80() {
        assert_eq!(super::message_list_column_min_width("folder"), Some(80));
        assert_eq!(super::message_list_column_min_width("account"), Some(80));
        assert_eq!(super::message_list_column_min_width("size"), Some(96));
        assert_eq!(super::message_list_column_min_width("deleted"), Some(96));
    }

    #[test]
    fn message_list_column_visible_decode() {
        let all: HashSet<String> = super::MESSAGE_LIST_DEFAULT_VISIBLE
            .iter()
            .map(|id| (*id).to_string())
            .collect();
        assert_eq!(decode_message_list_column_visible([] as [&str; 0]), all);
        assert!(!decode_message_list_column_visible([] as [&str; 0]).contains("folder"));
        assert_eq!(decode_message_list_column_visible(["unread", "star"]), all,);
        assert_eq!(
            decode_message_list_column_visible(["from", "flags"]),
            HashSet::from(["from".to_string()]),
        );
        assert_eq!(
            decode_message_list_column_visible(["from"]),
            HashSet::from(["from".to_string()]),
        );
    }

    #[test]
    fn folder_column_effective_visible_honors_picker_outside_search() {
        let with_folder = HashSet::from(["folder".to_string()]);
        let without_folder = HashSet::from(["from".to_string()]);
        assert!(super::folder_column_effective_visible(
            true,
            &without_folder
        ));
        assert!(super::folder_column_effective_visible(false, &with_folder));
        assert!(!super::folder_column_effective_visible(
            false,
            &without_folder
        ));
    }

    #[test]
    fn toggle_message_list_column_visible_merge() {
        let default: HashSet<String> = super::MESSAGE_LIST_DEFAULT_VISIBLE
            .iter()
            .map(|id| (*id).to_string())
            .collect();
        let mut with_folder = default.clone();
        with_folder.insert("folder".to_string());
        let mut saved = default;
        saved.insert("folder".to_string());
        assert_eq!(saved, with_folder);
        saved.remove("folder");
        assert!(!saved.contains("folder"));
    }

    #[test]
    fn recent_move_folders_parse_skips_non_integers() {
        let parsed = super::parse_recent_move_folder_ids(["42", "bad", "7"]);
        assert_eq!(parsed, vec![42, 7]);
    }

    #[test]
    fn recent_move_folders_push_prepend_dedupe_and_cap() {
        let mut recents = vec![2, 3, 4, 5, 6];
        super::push_recent_move_folder_in_place(&mut recents, 7);
        assert_eq!(recents, vec![7, 2, 3, 4, 5]);

        super::push_recent_move_folder_in_place(&mut recents, 3);
        assert_eq!(recents, vec![3, 7, 2, 4, 5]);
    }

    #[test]
    fn recent_move_folders_round_trip_strings() {
        let ids = vec![11_i64, 22, 33];
        let encoded = ["11", "22", "33"];
        let parsed = super::parse_recent_move_folder_ids(encoded);
        assert_eq!(parsed, ids);
    }

    #[test]
    fn keyboard_shortcut_overrides_round_trip() {
        let mut map = HashMap::new();
        map.insert(
            "app.command-palette".to_string(),
            "<Primary><Shift>p".to_string(),
        );
        let encoded = encode_keyboard_shortcut_overrides(&map);
        let decoded = decode_keyboard_shortcut_overrides(encoded);
        assert_eq!(decoded, map);
    }

    #[test]
    fn keyboard_shortcut_overrides_skip_malformed() {
        let decoded = decode_keyboard_shortcut_overrides([
            "app.compose\u{1f}<Primary>n",
            "no-separator",
            "\u{1f}trigger-only",
            "empty-trigger\u{1f}",
        ]);
        assert_eq!(decoded.len(), 1);
        assert_eq!(
            decoded.get("app.compose").map(String::as_str),
            Some("<Primary>n")
        );
    }
}
