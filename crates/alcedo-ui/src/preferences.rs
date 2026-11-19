//! Preferences dialog: General, Appearance, Composing, Privacy, then the
//! per-account pages (Accounts, extension pages, Contacts) and Keyboard.

use crate::account_settings;
use crate::appearance;
use crate::compose_snippets::build_snippets_prefs_group;
use crate::contacts;
use crate::help::{TOPIC_COMPOSE, TOPIC_NOTIFICATIONS, TOPIC_PRIVACY, TOPIC_READING, TOPIC_RULES};
use crate::icons::{RULES_SYMBOLIC, set_image};
use crate::keyboard_prefs;
use crate::mail::MailMsg;
use crate::settings::{
    bind_cleanup_verification_codes, bind_close_to_tray, bind_compose_format,
    bind_folder_pane_accounts, bind_insert_tracking_pixel, bind_list_density,
    bind_load_remote_images, bind_message_list_mode, bind_request_mdn, bind_show_message_preview,
    bind_show_verification_codes, bind_strv_as_csv, bind_tracking_pixel_url,
};
use alcedo_i18n::{ntr, tr, trs};
use relm4::Sender;
use relm4::adw;
use relm4::adw::prelude::*;
use relm4::gtk;
use tokio::runtime::Handle;

/// `AdwPreferencesPage` name for Preferences → Composing.
pub const COMPOSING_PAGE_NAME: &str = "composing";
pub const ACCOUNTS_PAGE_NAME: &str = "accounts";

/// Preferences dialog plus page handles shared by the app shell and mail child.
pub struct PreferencesUi {
    pub dialog: adw::PreferencesDialog,
    pub accounts: account_settings::AccountsPage,
    pub contacts_page: adw::PreferencesPage,
    pub snippets: crate::compose_snippets::SnippetsPrefs,
}

/// Build the application preferences dialog.
pub fn build_preferences_dialog(
    app_sender: Sender<crate::app::AppMsg>,
    mail_sender: Sender<MailMsg>,
    runtime: Handle,
) -> PreferencesUi {
    let (accounts_page, accounts) = account_settings::build_accounts_page(mail_sender.clone());
    accounts_page.set_name(Some(ACCOUNTS_PAGE_NAME));
    let contacts_page = contacts::build_contacts_prefs_page(app_sender);

    let dialog = adw::PreferencesDialog::builder()
        .search_enabled(true)
        .build();

    let general_page = build_general_page(&mail_sender);
    let appearance_page = appearance::build_appearance_page();
    let (composing_page, snippets) = build_composing_page(&mail_sender, runtime.clone(), &dialog);
    let privacy_page = build_privacy_page();
    let keyboard_page = keyboard_prefs::build_keyboard_page();

    for page in PreferencesPageOrder::ALL {
        match page {
            PreferencesPageOrder::General => dialog.add(&general_page),
            PreferencesPageOrder::Appearance => dialog.add(&appearance_page),
            PreferencesPageOrder::Composing => dialog.add(&composing_page),
            PreferencesPageOrder::Privacy => dialog.add(&privacy_page),
            PreferencesPageOrder::Accounts => dialog.add(&accounts_page),
            PreferencesPageOrder::Extensions => {
                for ext in crate::extension::registered() {
                    for page in ext.preferences_pages(&runtime, &dialog) {
                        dialog.add(&page);
                    }
                }
            }
            PreferencesPageOrder::Contacts => dialog.add(&contacts_page),
            PreferencesPageOrder::Keyboard => dialog.add(&keyboard_page),
        }
    }

    PreferencesUi {
        dialog,
        accounts,
        contacts_page,
        snippets,
    }
}

/// A combo row backed by a plain string list.
fn string_combo_row(title: &str, subtitle: &str, labels: &[&str]) -> adw::ComboRow {
    adw::ComboRow::builder()
        .title(title)
        .subtitle(subtitle)
        .model(&gtk::StringList::new(labels))
        .expression(gtk::PropertyExpression::new(
            gtk::StringObject::static_type(),
            None::<&gtk::Expression>,
            "string",
        ))
        .build()
}

/// General: how the app behaves on the desktop, the message list, and tools.
fn build_general_page(mail_sender: &Sender<MailMsg>) -> adw::PreferencesPage {
    let close_to_tray_row = adw::SwitchRow::builder()
        .title(tr!("Close to tray"))
        .subtitle(tr!(
            "Window hides in the notification area instead of quitting"
        ))
        .build();
    bind_close_to_tray(&close_to_tray_row);

    let desktop_group = adw::PreferencesGroup::builder()
        .title(tr!("Desktop"))
        .build();
    crate::help::attach_group_help(&desktop_group, TOPIC_NOTIFICATIONS);
    desktop_group.add(&close_to_tray_row);

    let list_mode_row = string_combo_row(
        &tr!("Grouping"),
        &tr!("Group replies into conversations, or show one row per message"),
        &[trs!("Conversations"), trs!("One message per row")],
    );
    bind_message_list_mode(&list_mode_row);

    let preview_row = adw::SwitchRow::builder()
        .title(tr!("Show message previews"))
        .subtitle(tr!("A line of the message body under each subject"))
        .build();
    bind_show_message_preview(&preview_row);

    let density_row = string_combo_row(
        &tr!("List density"),
        &tr!("Spacing in the message list and reading pane"),
        &[trs!("Compact"), trs!("Comfortable"), trs!("Relaxed")],
    );
    bind_list_density(&density_row);

    let list_group = adw::PreferencesGroup::builder()
        .title(tr!("Message list"))
        .build();
    crate::help::attach_group_help(&list_group, TOPIC_READING);
    list_group.add(&list_mode_row);
    list_group.add(&preview_row);
    list_group.add(&density_row);

    let folder_pane_accounts_row = string_combo_row(
        &tr!("Accounts"),
        &tr!("Show one account at a time, or every account with its folders"),
        &[trs!("Account picker"), trs!("All accounts stacked")],
    );
    bind_folder_pane_accounts(&folder_pane_accounts_row);

    let folder_pane_group = adw::PreferencesGroup::builder()
        .title(tr!("Folder pane"))
        .build();
    folder_pane_group.add(&folder_pane_accounts_row);

    let show_codes_row = adw::SwitchRow::builder()
        .title(tr!("Show verification codes"))
        .subtitle(tr!(
            "When a message includes a sign-in code, show it in the reader so you can copy it"
        ))
        .build();
    bind_show_verification_codes(&show_codes_row);

    let cleanup_codes_row = adw::SwitchRow::builder()
        .title(tr!("Clean up after copy"))
        .subtitle(tr!(
            "Move that message to Trash after you copy the code. Undo from the toast."
        ))
        .build();
    bind_cleanup_verification_codes(&cleanup_codes_row);
    show_codes_row
        .bind_property("active", &cleanup_codes_row, "sensitive")
        .sync_create()
        .build();

    let codes_group = adw::PreferencesGroup::builder()
        .title(tr!("Verification codes"))
        .build();
    codes_group.add(&show_codes_row);
    codes_group.add(&cleanup_codes_row);

    let rules_icon = gtk::Image::new();
    set_image(&rules_icon, RULES_SYMBOLIC, 16);
    let rules_row = adw::ActionRow::builder()
        .title(tr!("Message rules"))
        .subtitle(tr!("Automatically file, star, or delete mail"))
        .activatable(true)
        .build();
    rules_row.add_prefix(&rules_icon);
    rules_row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
    let mail_sender_rules = mail_sender.clone();
    rules_row.connect_activated(move |_| {
        mail_sender_rules.emit(MailMsg::OpenRules);
    });

    let automation_group = adw::PreferencesGroup::builder()
        .title(tr!("Automation"))
        .build();
    crate::help::attach_group_help(&automation_group, TOPIC_RULES);
    automation_group.add(&rules_row);

    let activity_log_limit_row = adw::SpinRow::with_range(50.0, 2000.0, 10.0);
    activity_log_limit_row.set_title(&tr!("Maximum lines"));
    activity_log_limit_row.set_subtitle(&tr!("Oldest entries are dropped when the log is full"));
    activity_log_limit_row.set_digits(0);
    crate::settings::bind_activity_log_limit(&activity_log_limit_row);

    let activity_log_group = adw::PreferencesGroup::builder()
        .title(tr!("Activity log"))
        .description(
            tr!("Keeps a session history of sends, sync, and other actions. Open it from Idle on the status bar."),
        )
        .build();
    activity_log_group.add(&activity_log_limit_row);

    let page = adw::PreferencesPage::builder()
        .title(tr!("General"))
        .icon_name("preferences-system-symbolic")
        .build();
    page.add(&desktop_group);
    page.add(&list_group);
    page.add(&folder_pane_group);
    page.add(&codes_group);
    page.add(&automation_group);
    page.add(&activity_log_group);
    for ext in crate::extension::registered() {
        ext.extend_general_preferences(&page);
    }
    page
}

/// Composing: defaults for new messages and snippets, plus groups from extensions.
fn build_composing_page(
    mail_sender: &Sender<MailMsg>,
    runtime: Handle,
    dialog: &adw::PreferencesDialog,
) -> (adw::PreferencesPage, crate::compose_snippets::SnippetsPrefs) {
    let format_row = string_combo_row(
        &tr!("Default format"),
        &tr!("Used for new messages and replies"),
        &[trs!("Plain text"), "HTML", trs!("Markdown")],
    );
    bind_compose_format(&format_row);

    let send_delay_row = build_send_delay_row();

    let typical_inline_row = adw::EntryRow::builder()
        .title(tr!("Typical inline images"))
        .tooltip_text(tr!(
            "Comma-separated extensions most email clients can display, for example png, jpeg"
        ))
        .build();
    bind_strv_as_csv("typical-inline-image-extensions", &typical_inline_row);

    let messages_group = adw::PreferencesGroup::builder()
        .title(tr!("Messages"))
        .build();
    crate::help::attach_group_help(&messages_group, TOPIC_COMPOSE);
    messages_group.add(&format_row);
    messages_group.add(&send_delay_row);
    messages_group.add(&typical_inline_row);

    let snippets = build_snippets_prefs_group(mail_sender.clone());

    let page = adw::PreferencesPage::builder()
        .name(COMPOSING_PAGE_NAME)
        .title(tr!("Composing"))
        .icon_name("document-edit-symbolic")
        .build();
    page.add(&messages_group);
    page.add(&snippets.group);
    for ext in crate::extension::registered() {
        ext.extend_composing_preferences(&runtime, dialog, &page);
    }
    (page, snippets)
}

/// Privacy: what incoming mail may load, which attachments get a warning, and
/// what outgoing mail tells recipients.
fn build_privacy_page() -> adw::PreferencesPage {
    let load_remote_row = adw::SwitchRow::builder()
        .title(tr!("Load remote images"))
        .subtitle(
            tr!("HTML mail loads remote images automatically. You can still allow a message or sender from the reader."),
        )
        .build();
    bind_load_remote_images(&load_remote_row);

    let remote_group = adw::PreferencesGroup::builder()
        .title(tr!("Incoming mail"))
        .build();
    crate::help::attach_group_help(&remote_group, TOPIC_PRIVACY);
    remote_group.add(&load_remote_row);

    let risky_ext_row = adw::EntryRow::builder()
        .title(tr!("Risky extensions"))
        .tooltip_text(tr!("Comma-separated, for example exe, html, svg"))
        .build();
    bind_strv_as_csv("risky-attachment-extensions", &risky_ext_row);

    let risky_mime_row = adw::EntryRow::builder()
        .title(tr!("Risky MIME types"))
        .tooltip_text(tr!("Comma-separated, for example text/html, image/svg+xml"))
        .build();
    bind_strv_as_csv("risky-attachment-mime-types", &risky_mime_row);

    let attachments_group = adw::PreferencesGroup::builder()
        .title(tr!("Attachments"))
        .description(
            tr!("Risky types stay stored, but opening or saving them asks for confirmation. Edit the lists to match what you want flagged."),
        )
        .build();
    attachments_group.add(&risky_ext_row);
    attachments_group.add(&risky_mime_row);

    let mdn_row = adw::SwitchRow::builder()
        .title(tr!("Request read receipts"))
        .subtitle(tr!("Recipients may be notified when this is read."))
        .build();
    bind_request_mdn(&mdn_row);

    let pixel_row = adw::SwitchRow::builder()
        .title(tr!("Insert tracking pixel"))
        .subtitle(
            "Adds a hidden image from your URL to outgoing HTML mail so you can \
             see when it is opened. Recipients are tracked without being asked.",
        )
        .build();
    bind_insert_tracking_pixel(&pixel_row);

    let pixel_url_row = adw::EntryRow::builder()
        .title(tr!("Tracking pixel URL"))
        .tooltip_text(tr!("Must start with https://"))
        .build();
    bind_tracking_pixel_url(&pixel_url_row);
    pixel_row
        .bind_property("active", &pixel_url_row, "sensitive")
        .sync_create()
        .build();

    let tracking_expander = adw::ExpanderRow::builder()
        .title(tr!("Open tracking"))
        .subtitle(tr!(
            "Know when recipients open your mail. Most people should leave this off."
        ))
        .expanded(false)
        .build();
    tracking_expander.add_row(&pixel_row);
    tracking_expander.add_row(&pixel_url_row);

    let outgoing_group = adw::PreferencesGroup::builder()
        .title(tr!("Outgoing mail"))
        .build();
    outgoing_group.add(&mdn_row);
    outgoing_group.add(&tracking_expander);

    let page = adw::PreferencesPage::builder()
        .title(tr!("Privacy"))
        .icon_name("channel-secure-symbolic")
        .build();
    page.add(&remote_group);
    page.add(&attachments_group);
    page.add(&outgoing_group);
    page
}

/// Preferences pages, in the order shown: everyday settings first,
/// per-account and reference pages after.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PreferencesPageOrder {
    General,
    Appearance,
    Composing,
    Privacy,
    Accounts,
    /// Pages extensions add.
    Extensions,
    Contacts,
    Keyboard,
}

impl PreferencesPageOrder {
    const ALL: [Self; 8] = [
        Self::General,
        Self::Appearance,
        Self::Composing,
        Self::Privacy,
        Self::Accounts,
        Self::Extensions,
        Self::Contacts,
        Self::Keyboard,
    ];
}

/// Undo-send delay choices, in seconds (0 = off).
const SEND_DELAY_CHOICES: [u32; 4] = [0, 5, 10, 20];

fn send_delay_label(seconds: u32) -> String {
    if seconds == 0 {
        tr!("Off")
    } else {
        ntr!("{n} second", "{n} seconds", seconds)
    }
}

/// Index of the closest choice at or below `seconds` (unknown values snap down).
fn send_delay_index(seconds: u32) -> u32 {
    SEND_DELAY_CHOICES
        .iter()
        .rposition(|&choice| choice <= seconds)
        .unwrap_or(0) as u32
}

fn build_send_delay_row() -> adw::ComboRow {
    let labels: Vec<String> = SEND_DELAY_CHOICES
        .iter()
        .map(|&seconds| send_delay_label(seconds))
        .collect();
    let label_refs: Vec<&str> = labels.iter().map(String::as_str).collect();
    let model = gtk::StringList::new(&label_refs);
    let row = adw::ComboRow::builder()
        .title(tr!("Undo send"))
        .subtitle(tr!("Wait before sending so you can take a message back"))
        .model(&model)
        .build();
    row.set_selected(send_delay_index(crate::settings::send_delay_seconds()));
    row.connect_selected_notify(|row| {
        let index = row.selected() as usize;
        if let Some(&seconds) = SEND_DELAY_CHOICES.get(index) {
            crate::settings::set_send_delay_seconds(seconds);
        }
    });
    row
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn everyday_pages_come_first() {
        assert_eq!(PreferencesPageOrder::ALL[0], PreferencesPageOrder::General);
        assert_eq!(
            PreferencesPageOrder::ALL[1],
            PreferencesPageOrder::Appearance
        );
        assert_eq!(
            PreferencesPageOrder::ALL[2],
            PreferencesPageOrder::Composing
        );
        assert_eq!(PreferencesPageOrder::ALL[3], PreferencesPageOrder::Privacy);
    }

    #[test]
    fn send_delay_choices_map_to_labels_and_indexes() {
        assert_eq!(send_delay_label(0), "Off");
        assert_eq!(send_delay_label(10), "10 seconds");
        assert_eq!(send_delay_index(0), 0);
        assert_eq!(send_delay_index(10), 2);
        assert_eq!(send_delay_index(15), 2);
        assert_eq!(send_delay_index(99), 3);
    }
}
