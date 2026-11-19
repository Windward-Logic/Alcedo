//! Relm4 UI shell (P4).
#![allow(
    clippy::too_many_arguments,
    clippy::type_complexity,
    clippy::large_enum_variant
)]

pub mod a11y;
pub mod account_chip;
pub mod account_settings;
pub(crate) mod account_validation;
pub mod activity_log;
pub mod add_account;
pub mod address_popover;
pub mod app;
pub mod appearance;
pub(crate) mod attachment_save;
pub mod auth_banner;
pub(crate) mod blocking_io;
pub mod calendar_clock;
pub mod calendar_day_select;
pub mod command_palette;
pub mod commands;
pub mod compose_attachments;
pub mod compose_draft_store;
pub mod compose_html;
pub mod compose_snippets;
pub mod compose_toolbar;
pub mod composer;
pub mod contacts;
pub mod extension;
pub mod folder_bar;
pub mod folder_bar_accounts;
pub mod folder_bar_personal;
pub mod folder_editor;
pub mod folder_pane_lists;
pub mod folder_picker;
pub(crate) mod glib_source;
pub mod help;
pub mod icons;
pub mod keyboard_prefs;
pub mod local_time;
pub mod mail;
pub mod meeting_invite;
pub mod meeting_rsvp;
pub mod menu;
pub mod message_list;
pub mod message_window;
pub mod mock;
pub mod notifications;
pub mod overflow_tooltip;
pub mod pgp_banner;
pub mod preferences;
pub mod rail;
pub mod reader;
pub mod recipient_picker;
pub mod recipient_row;
pub mod rule_editor;
pub mod rules;
pub mod search_autocomplete;
pub mod search_bar;
pub mod settings;
pub mod signature_dialog;
pub mod source_viewer;
pub mod status_bar;
pub mod theme;
pub mod theme_switcher;
pub mod toast;
pub mod tray;
pub mod verification_code;

mod collapsing_revealer;
#[cfg(test)]
mod test_helpers;
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
mod widget_lifetime;

pub use app::{AppInit, AppModel as App, present_main_window};

/// gettext domain of the public app's text, read by the `alcedo_i18n` macros.
pub(crate) const I18N_DOMAIN: &str = "alcedo";
