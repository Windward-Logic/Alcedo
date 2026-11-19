//! Preferences → Accounts page (list, add, edit, remove).

use crate::account_validation::validation_text;
use alcedo_i18n::{tr, trf, trm, trs};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use alcedo_core::{
    Account, AccountId, AccountIdentity, AccountSettingsUpdate, AccountSignature, AuthMethod,
    ComposeFormat, Error, OAuthProvider, OAuthTokens, PgpAccountState, SmtpEnvelopePolicy,
    avatar_from_label, format_fingerprint_display, sanitize_accent_color, sanitize_avatar_text,
    sanitize_sender_name,
};
use alcedo_html::html_to_plain_email;
use alcedo_oauth::oauth_flow;
use relm4::Sender;
use relm4::adw;
use relm4::adw::prelude::*;
use relm4::gtk;
use relm4::gtk::glib;
use tokio::runtime::Handle;
use tokio::sync::oneshot;
use uuid::Uuid;

use crate::account_chip::{
    apply_account_chip, apply_account_chip_appearance, crop_avatar_image, hex_from_rgba,
    rgba_from_hex,
};
use crate::add_account::{launch_auth_url, oauth_provider_config};
use crate::help::{self, TOPIC_ACCOUNTS, TOPIC_IDENTITIES, TOPIC_OPENPGP};
use crate::mail::MailMsg;
use crate::signature_dialog::open_signature_dialog;

const SMTP_SSL_TLS: u32 = 0;
const SMTP_STARTTLS: u32 = 1;
/// Dummy value shown in the password row. Never submitted; the real secret stays in the keyring.
const SAVED_PASSWORD_MASK: &str = "********";

/// The boxed-list group listing configured accounts.
pub struct AccountsPage {
    pub group: adw::PreferencesGroup,
    open: Rc<RefCell<Option<OpenSubpage>>>,
}

struct OpenSubpage {
    id: AccountId,
    baseline: Rc<RefCell<Account>>,
    widgets: Rc<SubpageWidgets>,
    state: Rc<RefCell<SubpageState>>,
    update_dirty: Rc<dyn Fn()>,
    nav_page: adw::NavigationPage,
    dialog: adw::PreferencesDialog,
    leave_after_save: Rc<Cell<Option<AccountLeave>>>,
    leaving: Rc<Cell<bool>>,
    signature_row: adw::ActionRow,
    from_addresses_group: adw::PreferencesGroup,
    add_identity_row: adw::ButtonRow,
    envelope_policy_row: adw::SwitchRow,
    catch_all_row: adw::EntryRow,
    signature: Rc<RefCell<Option<AccountSignature>>>,
}

struct SubpageState {
    syncing_smtp: bool,
    syncing_password: bool,
    syncing_smtp_password: bool,
    syncing_credentials: bool,
    password_pristine: bool,
    smtp_password_pristine: bool,
    smtp_username_touched: bool,
    oauth_in_flight: bool,
    oauth_generation: u64,
    pgp_syncing: bool,
    envelope_syncing: bool,
    envelope_known: bool,
    /// Last saved value of the "send from the saved address" switch.
    saved_envelope: bool,
    /// Last saved catch-all domains, used to tell if the field was edited.
    saved_catch_all: Vec<String>,
    syncing_appearance: bool,
}

struct SubpageWidgets {
    envelope_policy_row: adw::SwitchRow,
    catch_all_row: adw::EntryRow,
    name_row: adw::EntryRow,
    email_row: adw::EntryRow,
    your_name_row: adw::EntryRow,
    chip_row: adw::EntryRow,
    username_row: adw::EntryRow,
    imap_host_row: adw::EntryRow,
    imap_port_row: adw::SpinRow,
    smtp_host_row: adw::EntryRow,
    smtp_port_row: adw::SpinRow,
    smtp_security_row: adw::ComboRow,
    auth_group: adw::PreferencesGroup,
    split_row: adw::SwitchRow,
    password_row: adw::PasswordEntryRow,
    smtp_username_row: adw::EntryRow,
    smtp_password_row: adw::PasswordEntryRow,
    oauth_row: adw::ActionRow,
    pgp_key_row: adw::ActionRow,
    pgp_sign_outgoing_row: adw::SwitchRow,
    pgp_generate_row: adw::ActionRow,
    pgp_import_row: adw::ActionRow,
    pgp_export_row: adw::ActionRow,
    pgp_remove_row: adw::ButtonRow,
    save_row: adw::ButtonRow,
    hero_avatar: adw::Avatar,
    hero_name: gtk::Label,
    hero_email: gtk::Label,
    color_button: gtk::ColorDialogButton,
    photo_row: adw::ActionRow,
    photo_clear: gtk::Button,
    pending_image: Rc<RefCell<Vec<u8>>>,
}

/// Build the Accounts preferences page and its list group handle.
pub fn build_accounts_page(sender: Sender<MailMsg>) -> (adw::PreferencesPage, AccountsPage) {
    let list_group = adw::PreferencesGroup::builder()
        .title(tr!("Accounts"))
        .description(tr!("Add an email account to send and receive mail."))
        .build();
    help::attach_group_help(&list_group, TOPIC_ACCOUNTS);

    let add_row = adw::ActionRow::builder()
        .title(tr!("Add Account"))
        .activatable(true)
        .build();
    let add_icon = gtk::Image::from_icon_name("list-add-symbolic");
    add_row.add_prefix(&add_icon);
    add_row.connect_activated({
        let sender = sender.clone();
        move |_| {
            sender.send(MailMsg::AddAccount).ok();
        }
    });

    let add_group = adw::PreferencesGroup::new();
    add_group.add(&add_row);

    let page = adw::PreferencesPage::builder()
        .title(tr!("Accounts"))
        .icon_name("avatar-default-symbolic")
        .build();
    page.add(&list_group);
    page.add(&add_group);

    (
        page,
        AccountsPage {
            group: list_group,
            open: Rc::new(RefCell::new(None)),
        },
    )
}

/// Rebuild account rows from the current model list.
pub fn refresh_account_rows(
    page: &AccountsPage,
    accounts: &[Account],
    dialog: &adw::PreferencesDialog,
    sender: &Sender<MailMsg>,
    runtime: &Handle,
    auth_required: &HashMap<AccountId, String>,
    signatures: &HashMap<AccountId, AccountSignature>,
) {
    if accounts.is_empty() {
        page.group
            .set_description(Some(&tr!("Add an email account to send and receive mail.")));
    } else {
        page.group.set_description(None::<&str>);
    }

    while let Some(row) = page.group.row(0) {
        page.group.remove(&row);
    }

    for account in accounts {
        let subtitle = if auth_required.contains_key(&account.id) {
            tr!("Sign-in required")
        } else {
            account.email.clone()
        };
        let row = adw::ActionRow::builder()
            .use_markup(false)
            .title(account.label())
            .subtitle(&subtitle)
            .activatable(true)
            .build();

        let avatar = adw::Avatar::builder().size(32).show_initials(true).build();
        avatar.set_can_focus(false);
        apply_account_chip(&avatar, account);
        row.add_prefix(&avatar);
        if auth_required.contains_key(&account.id) {
            let warning = gtk::Image::from_icon_name("dialog-warning-symbolic");
            warning.add_css_class("warning");
            row.add_suffix(&warning);
        }
        row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));

        let account_id = account.id;
        let account = account.clone();
        let dialog = dialog.clone();
        let sender = sender.clone();
        let runtime = runtime.clone();
        let open = page.open.clone();
        let stored_signature = signatures.get(&account_id).cloned();
        row.connect_activated(move |_| {
            push_account_subpage(
                &dialog,
                &account,
                account_id,
                &sender,
                &runtime,
                &open,
                stored_signature.as_ref(),
            );
        });

        page.group.add(&row);
    }
}

/// Mark the open editor clean after a successful save (mock or `AccountUpdated`).
pub(crate) fn mark_account_saved(page: &AccountsPage, account: &Account) {
    let pending_leave = {
        let open_guard = page.open.borrow();
        let Some(open) = open_guard.as_ref() else {
            return;
        };
        if open.id != account.id {
            return;
        }
        *open.baseline.borrow_mut() = account.clone();
        open.widgets.name_row.set_text(&account.display_name);
        open.widgets.email_row.set_text(&account.email);
        open.widgets.hero_email.set_text(&account.email);
        open.widgets.your_name_row.set_text(&account.sender_name);
        open.widgets
            .chip_row
            .set_text(&sanitize_avatar_text(&account.avatar_text));
        open.widgets.username_row.set_text(account.login_name());
        open.state.borrow_mut().syncing_credentials = true;
        open.widgets
            .split_row
            .set_active(account.uses_split_credentials());
        open.widgets
            .smtp_username_row
            .set_text(account.smtp_login_name());
        {
            let mut st = open.state.borrow_mut();
            st.smtp_username_touched = false;
            st.syncing_credentials = false;
        }
        open.widgets.hero_name.set_text(account.label());
        open.state.borrow_mut().syncing_appearance = true;
        open.widgets
            .color_button
            .set_rgba(&rgba_from_hex(&account.accent_color));
        *open.widgets.pending_image.borrow_mut() = account.avatar_image.clone();
        apply_photo_row(
            &open.widgets.photo_row,
            &open.widgets.photo_clear,
            &account.avatar_image,
        );
        apply_account_chip(&open.widgets.hero_avatar, account);
        open.state.borrow_mut().syncing_appearance = false;
        open.nav_page.set_title(account.label());
        restore_saved_password_mask(&open.widgets, &open.state, account);
        restore_saved_smtp_password_mask(&open.widgets, &open.state, account);
        apply_credential_mode(&open.widgets, account);
        (open.update_dirty)();
        open.leave_after_save.replace(None).map(|intent| {
            (
                intent,
                open.dialog.clone(),
                open.nav_page.clone(),
                open.leaving.clone(),
            )
        })
    };
    if let Some((intent, dialog, nav_page, leaving)) = pending_leave {
        finish_account_leave(&dialog, &nav_page, &leaving, intent);
    }
}

pub(crate) fn open_account_id(page: &AccountsPage) -> Option<AccountId> {
    page.open.borrow().as_ref().map(|open| open.id)
}

/// Refresh OpenPGP rows on the open account subpage.
pub fn refresh_pgp_subpage(page: &AccountsPage, state: Option<&PgpAccountState>) {
    let open_guard = page.open.borrow();
    let Some(open) = open_guard.as_ref() else {
        return;
    };
    let widgets = &open.widgets;
    // Set the flag in its own statement: `set_active` below runs the row's
    // notify handler synchronously, and that handler borrows the state.
    open.state.borrow_mut().pgp_syncing = true;

    let has_key = state.and_then(|s| s.own.as_ref()).is_some();
    if let Some(own) = state.and_then(|s| s.own.as_ref()) {
        let grouped = format_fingerprint_display(&own.fingerprint);
        widgets.pgp_key_row.set_title(&tr!("OpenPGP key"));
        widgets
            .pgp_key_row
            .set_subtitle(&trf!("Fingerprint {fingerprint}", fingerprint = grouped));
    } else {
        widgets.pgp_key_row.set_title(&tr!("OpenPGP key"));
        widgets
            .pgp_key_row
            .set_subtitle(&tr!("No key configured for this account"));
    }

    let sign_outgoing = state.map(|s| s.sign_outgoing).unwrap_or(false);
    widgets.pgp_sign_outgoing_row.set_active(sign_outgoing);
    widgets.pgp_sign_outgoing_row.set_sensitive(has_key);

    widgets.pgp_generate_row.set_visible(!has_key);
    widgets.pgp_import_row.set_visible(!has_key);
    widgets.pgp_export_row.set_sensitive(has_key);
    widgets.pgp_remove_row.set_sensitive(has_key);

    open.state.borrow_mut().pgp_syncing = false;
}

pub(crate) fn present_account_editor(
    dialog: &adw::PreferencesDialog,
    page: &AccountsPage,
    account: &Account,
    sender: &Sender<MailMsg>,
    runtime: &Handle,
    signature: Option<&AccountSignature>,
) {
    push_account_subpage(
        dialog, account, account.id, sender, runtime, &page.open, signature,
    );
}

/// Delay that lets the dialog finish presenting before focus moves into it.
///
/// The preferences dialog is presented by a queued app message, so a focus
/// request made in the same turn would land on a window that is not showing.
const FOCUS_AFTER_PRESENT: std::time::Duration = std::time::Duration::from_millis(200);

/// Put the cursor in the password field of the editor that is open, so a
/// person who was sent here to sign in again can type right away.
pub(crate) fn focus_open_account_password(page: &AccountsPage) {
    let Some(widgets) = page
        .open
        .borrow()
        .as_ref()
        .map(|open| Rc::clone(&open.widgets))
    else {
        return;
    };
    glib::timeout_add_local_once(FOCUS_AFTER_PRESENT, move || {
        widgets.password_row.grab_focus();
    });
}

pub(crate) async fn mail_oauth_pkce(account: &Account) -> Result<OAuthTokens, Error> {
    let oauth_provider = match &account.auth {
        AuthMethod::OAuth2 { provider } => *provider,
        AuthMethod::Password => {
            return Err(Error::Auth("account is not OAuth".into()));
        }
    };
    let provider_cfg = oauth_provider_config(&oauth_provider).map_err(Error::Auth)?;
    oauth_flow(oauth_provider)
        .sign_in(&provider_cfg, Box::new(launch_auth_url))
        .await
}

fn push_account_subpage(
    dialog: &adw::PreferencesDialog,
    account: &Account,
    account_id: AccountId,
    sender: &Sender<MailMsg>,
    runtime: &Handle,
    open: &Rc<RefCell<Option<OpenSubpage>>>,
    signature: Option<&AccountSignature>,
) {
    let baseline = Rc::new(RefCell::new(account.clone()));
    let state = Rc::new(RefCell::new(SubpageState {
        syncing_smtp: false,
        syncing_password: false,
        syncing_smtp_password: false,
        syncing_credentials: false,
        password_pristine: true,
        smtp_password_pristine: true,
        smtp_username_touched: false,
        oauth_in_flight: false,
        oauth_generation: 0,
        pgp_syncing: false,
        envelope_syncing: false,
        envelope_known: false,
        saved_envelope: false,
        saved_catch_all: Vec::new(),
        syncing_appearance: false,
    }));

    let hero_avatar = adw::Avatar::builder().size(72).show_initials(true).build();
    hero_avatar.set_can_focus(false);
    hero_avatar.set_margin_bottom(6);
    apply_account_chip(&hero_avatar, account);

    let hero_name = gtk::Label::builder()
        .label(account.label())
        .css_classes(["title-1"])
        .halign(gtk::Align::Center)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .max_width_chars(25)
        .build();

    let hero_email = gtk::Label::builder()
        .label(&account.email)
        .css_classes(["dim-label"])
        .halign(gtk::Align::Center)
        .selectable(true)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .build();

    let hero_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .halign(gtk::Align::Center)
        .margin_top(18)
        .margin_bottom(18)
        .build();
    hero_box.append(&hero_avatar);
    hero_box.append(&hero_name);
    hero_box.append(&hero_email);

    let hero_group = adw::PreferencesGroup::new();
    hero_group.add(&hero_box);

    let name_row = adw::EntryRow::builder()
        .title(tr!("Account name"))
        .show_apply_button(false)
        .build();
    name_row.set_text(&account.display_name);

    let email_row = adw::EntryRow::builder()
        .title(tr!("Email"))
        .show_apply_button(false)
        .build();
    email_row.set_text(&account.email);

    let your_name_row = adw::EntryRow::builder()
        .title(tr!("Your name"))
        .show_apply_button(false)
        .build();
    your_name_row.set_text(&account.sender_name);

    let chip_row = adw::EntryRow::builder()
        .title(tr!("Chip"))
        .show_apply_button(false)
        .build();
    chip_row.set_text(&sanitize_avatar_text(&account.avatar_text));

    let color_dialog = gtk::ColorDialog::new();
    color_dialog.set_title(&tr!("Chip color"));
    color_dialog.set_with_alpha(false);
    let color_button = gtk::ColorDialogButton::new(Some(color_dialog));
    color_button.set_rgba(&rgba_from_hex(&account.accent_color));
    color_button.set_valign(gtk::Align::Center);
    let color_row = adw::ActionRow::builder()
        .title(tr!("Background"))
        .subtitle(tr!("Color behind the chip initials"))
        .activatable(false)
        .build();
    color_row.add_suffix(&color_button);

    let pending_image = Rc::new(RefCell::new(account.avatar_image.clone()));
    let photo_row = adw::ActionRow::builder()
        .title(tr!("Photo"))
        .activatable(true)
        .build();
    let photo_clear = gtk::Button::builder()
        .icon_name("edit-clear-symbolic")
        .tooltip_text(tr!("Remove photo"))
        .valign(gtk::Align::Center)
        .css_classes(["flat"])
        .build();
    photo_row.add_suffix(&photo_clear);
    apply_photo_row(&photo_row, &photo_clear, &account.avatar_image);

    let username_row = adw::EntryRow::builder()
        .title(tr!("Username"))
        .show_apply_button(false)
        .build();
    username_row.set_text(account.login_name());

    let sign_in_subtitle = match &account.auth {
        AuthMethod::OAuth2 {
            provider: OAuthProvider::Google,
        } => tr!("Google (browser sign-in)"),
        AuthMethod::OAuth2 {
            provider: OAuthProvider::Microsoft,
        } => tr!("Microsoft (browser sign-in)"),
        AuthMethod::Password => tr!("Username and password (already saved)"),
    };
    let sign_in_row = adw::ActionRow::builder()
        .title(tr!("Sign-in method"))
        .subtitle(sign_in_subtitle)
        .activatable(false)
        .build();

    let identity_group = adw::PreferencesGroup::builder()
        .title(tr!("Identity"))
        .description(
            tr!("Account name is only a label inside Alcedo. Email is the mailbox address and the default From. Your name is what people see when you react. Leave Chip blank to use initials from the account name."),
        )
        .build();
    identity_group.add(&name_row);
    identity_group.add(&email_row);
    identity_group.add(&your_name_row);
    identity_group.add(&chip_row);
    identity_group.add(&color_row);
    identity_group.add(&photo_row);
    identity_group.add(&sign_in_row);

    let signature_row = adw::ActionRow::builder()
        .use_markup(false)
        .title(tr!("Signature"))
        .subtitle(tr!("None"))
        .activatable(true)
        .build();
    let signature_edit_btn = gtk::Button::builder()
        .icon_name("document-edit-symbolic")
        .tooltip_text(tr!("Edit"))
        .css_classes(["flat"])
        .build();
    signature_row.add_suffix(&signature_edit_btn);
    identity_group.add(&signature_row);

    let from_addresses_group = adw::PreferencesGroup::builder()
        .title(tr!("From addresses"))
        .description(from_addresses_helper(&account.auth))
        .build();
    help::attach_group_help(&from_addresses_group, TOPIC_IDENTITIES);
    let envelope_policy_row = adw::SwitchRow::builder()
        .title(tr!("Send from the saved address itself"))
        .subtitle(tr!("When you send from a saved address on this account’s domain, the mail server also treats that address as the sender, and failed-delivery notices come back to it. Turn off if your provider rejects those messages. Press Save to apply."))
        .active(false)
        .build();
    let add_identity_row = adw::ButtonRow::builder()
        .title(tr!("Add Address"))
        .start_icon_name("list-add-symbolic")
        .build();
    let catch_all_row = catch_all_domains_row();
    from_addresses_group.add(&envelope_policy_row);
    from_addresses_group.add(&catch_all_row);
    from_addresses_group.add(&add_identity_row);

    let imap_host_row = adw::EntryRow::builder()
        .title(tr!("IMAP host"))
        .show_apply_button(false)
        .build();
    imap_host_row.set_text(&account.imap_host);
    if matches!(account.auth, AuthMethod::OAuth2 { .. }) {
        imap_host_row.set_sensitive(false);
    }
    let imap_port_row = adw::SpinRow::with_range(1.0, 65535.0, 1.0);
    imap_port_row.set_title(&tr!("IMAP port"));
    imap_port_row.set_value(f64::from(account.imap_port));

    let incoming_group = adw::PreferencesGroup::builder()
        .title(tr!("Incoming mail"))
        .build();
    incoming_group.add(&imap_host_row);
    incoming_group.add(&imap_port_row);

    let smtp_host_row = adw::EntryRow::builder()
        .title(tr!("SMTP host"))
        .show_apply_button(false)
        .build();
    smtp_host_row.set_text(&account.smtp_host);
    if matches!(account.auth, AuthMethod::OAuth2 { .. }) {
        smtp_host_row.set_sensitive(false);
    }
    let smtp_port_row = adw::SpinRow::with_range(1.0, 65535.0, 1.0);
    smtp_port_row.set_title(&tr!("SMTP port"));
    smtp_port_row.set_value(f64::from(account.smtp_port));

    let security_model = gtk::StringList::new(&["SSL/TLS", "STARTTLS"]);
    let smtp_security_row = adw::ComboRow::builder()
        .title(tr!("SMTP security"))
        .subtitle(tr!(
            "SSL/TLS (port 465) or STARTTLS (port 587). Unencrypted SMTP is not supported."
        ))
        .model(&security_model)
        .expression(gtk::PropertyExpression::new(
            gtk::StringObject::static_type(),
            None::<&gtk::Expression>,
            "string",
        ))
        .build();
    smtp_security_row.set_selected(if account.smtp_starttls {
        SMTP_STARTTLS
    } else {
        SMTP_SSL_TLS
    });

    let outgoing_group = adw::PreferencesGroup::builder()
        .title(tr!("Outgoing mail"))
        .build();
    outgoing_group.add(&smtp_host_row);
    outgoing_group.add(&smtp_port_row);
    outgoing_group.add(&smtp_security_row);

    let split_row = adw::SwitchRow::builder()
        .title(tr!("Different SMTP sign-in"))
        .subtitle(tr!(
            "Use a separate username and password for outgoing mail."
        ))
        .active(account.uses_split_credentials())
        .build();
    let password_row = adw::PasswordEntryRow::builder()
        .title(tr!("Password"))
        .show_apply_button(false)
        .build();
    let smtp_username_row = adw::EntryRow::builder()
        .title(tr!("SMTP username"))
        .show_apply_button(false)
        .build();
    let smtp_password_row = adw::PasswordEntryRow::builder()
        .title(tr!("SMTP password"))
        .show_apply_button(false)
        .build();
    let oauth_row = adw::ActionRow::builder()
        .title(tr!("Sign in again"))
        .subtitle(tr!("Opens the browser to refresh this account."))
        .activatable(true)
        .build();
    let oauth_spinner = adw::Spinner::new();
    oauth_spinner.set_visible(false);

    let auth_group = match &account.auth {
        AuthMethod::Password => adw::PreferencesGroup::builder()
            .title(tr!("Authentication"))
            .description(tr!(
                "The saved password appears as dots. Replace the dots only to change it."
            ))
            .build(),
        AuthMethod::OAuth2 { .. } => adw::PreferencesGroup::builder()
            .title(tr!("Authentication"))
            .build(),
    };
    match &account.auth {
        AuthMethod::Password => {
            smtp_username_row.set_text(account.smtp_login_name());
            smtp_password_row.set_text(SAVED_PASSWORD_MASK);
            auth_group.add(&split_row);
            auth_group.add(&username_row);
            password_row.set_text(SAVED_PASSWORD_MASK);
            auth_group.add(&password_row);
            auth_group.add(&smtp_username_row);
            auth_group.add(&smtp_password_row);
        }
        AuthMethod::OAuth2 { .. } => {
            oauth_row.add_suffix(&oauth_spinner);
            auth_group.add(&oauth_row);
        }
    }

    let save_row = adw::ButtonRow::builder().title(tr!("Save")).build();
    save_row.add_css_class("suggested-action");
    let save_group = adw::PreferencesGroup::new();
    save_group.add(&save_row);

    let pgp_key_row = adw::ActionRow::builder()
        .title(tr!("OpenPGP key"))
        .subtitle(tr!("No key configured for this account"))
        .activatable(false)
        .build();

    let pgp_sign_outgoing_row = adw::SwitchRow::builder()
        .title(tr!("Sign outgoing mail"))
        .subtitle(tr!("Attach an OpenPGP signature to messages you send"))
        .sensitive(false)
        .build();

    let pgp_generate_row = adw::ActionRow::builder()
        .title(tr!("Generate key"))
        .subtitle(tr!("Create a new OpenPGP key for this account"))
        .activatable(true)
        .build();
    let pgp_generate_icon = gtk::Image::from_icon_name("system-run-symbolic");
    pgp_generate_row.add_prefix(&pgp_generate_icon);

    let pgp_import_row = adw::ActionRow::builder()
        .title(tr!("Import key"))
        .subtitle(tr!("Import an existing secret key from a file"))
        .activatable(true)
        .build();
    let pgp_import_icon = gtk::Image::from_icon_name("document-open-symbolic");
    pgp_import_row.add_prefix(&pgp_import_icon);

    let pgp_export_row = adw::ActionRow::builder()
        .title(tr!("Export public key"))
        .subtitle(tr!("Copy your public key to the clipboard"))
        .activatable(true)
        .build();
    let pgp_export_icon = gtk::Image::from_icon_name("edit-copy-symbolic");
    pgp_export_row.add_prefix(&pgp_export_icon);

    let pgp_remove_row = adw::ButtonRow::builder()
        .title(tr!("Remove OpenPGP key"))
        .build();
    pgp_remove_row.add_css_class("destructive-action");

    let openpgp_group = adw::PreferencesGroup::builder()
        .title(tr!("OpenPGP"))
        .description(tr!("Sign and encrypt mail with OpenPGP."))
        .build();
    help::attach_group_help(&openpgp_group, TOPIC_OPENPGP);
    openpgp_group.add(&pgp_key_row);
    openpgp_group.add(&pgp_sign_outgoing_row);
    openpgp_group.add(&pgp_generate_row);
    openpgp_group.add(&pgp_import_row);
    openpgp_group.add(&pgp_export_row);
    openpgp_group.add(&pgp_remove_row);

    let remove_row = adw::ButtonRow::builder()
        .title(tr!("Remove Account"))
        .build();
    remove_row.add_css_class("destructive-action");
    let remove_group = adw::PreferencesGroup::new();
    remove_group.add(&remove_row);

    let page = adw::PreferencesPage::new();
    page.add(&hero_group);
    page.add(&identity_group);
    page.add(&from_addresses_group);
    page.add(&incoming_group);
    page.add(&outgoing_group);
    page.add(&auth_group);
    page.add(&openpgp_group);
    page.add(&save_group);
    page.add(&remove_group);

    let nav_title = account.label();
    let header = adw::HeaderBar::new();
    // The stock back button hides while can-pop is false. This one stays so a
    // dirty page can ask before leaving.
    header.set_show_back_button(false);
    let back_btn = gtk::Button::builder()
        .icon_name("go-previous-symbolic")
        .css_classes(["back", "flat"])
        .build();
    crate::a11y::name_icon_control(&back_btn, &tr!("Back"), &tr!("Back"));
    header.pack_start(&back_btn);
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&page));
    let nav_page = adw::NavigationPage::builder()
        .title(nav_title)
        .child(&toolbar)
        .build();

    let widgets = Rc::new(SubpageWidgets {
        name_row: name_row.clone(),
        email_row: email_row.clone(),
        your_name_row: your_name_row.clone(),
        chip_row: chip_row.clone(),
        username_row: username_row.clone(),
        imap_host_row: imap_host_row.clone(),
        imap_port_row: imap_port_row.clone(),
        smtp_host_row: smtp_host_row.clone(),
        smtp_port_row: smtp_port_row.clone(),
        smtp_security_row: smtp_security_row.clone(),
        auth_group: auth_group.clone(),
        split_row: split_row.clone(),
        password_row: password_row.clone(),
        smtp_username_row: smtp_username_row.clone(),
        smtp_password_row: smtp_password_row.clone(),
        oauth_row: oauth_row.clone(),
        envelope_policy_row: envelope_policy_row.clone(),
        catch_all_row: catch_all_row.clone(),
        pgp_key_row: pgp_key_row.clone(),
        pgp_sign_outgoing_row: pgp_sign_outgoing_row.clone(),
        pgp_generate_row: pgp_generate_row.clone(),
        pgp_import_row: pgp_import_row.clone(),
        pgp_export_row: pgp_export_row.clone(),
        pgp_remove_row: pgp_remove_row.clone(),
        save_row: save_row.clone(),
        hero_avatar: hero_avatar.clone(),
        hero_name: hero_name.clone(),
        hero_email: hero_email.clone(),
        color_button: color_button.clone(),
        photo_row: photo_row.clone(),
        photo_clear: photo_clear.clone(),
        pending_image: pending_image.clone(),
    });

    let leaving = Rc::new(Cell::new(false));
    let leave_after_save = Rc::new(Cell::new(None));
    let prompt_open = Rc::new(Cell::new(false));
    let update_dirty: Rc<dyn Fn()> = {
        let widgets = widgets.clone();
        let state = state.clone();
        let baseline = baseline.clone();
        let nav_page = nav_page.clone();
        let dialog = dialog.clone();
        let leaving = leaving.clone();
        Rc::new(move || {
            if leaving.get() {
                return;
            }
            let dirty = subpage_is_dirty(&widgets, &baseline.borrow(), &state.borrow());
            let in_flight = state.borrow().oauth_in_flight;
            widgets.save_row.set_sensitive(dirty && !in_flight);
            nav_page.set_can_pop(!dirty);
            dialog.set_can_close(!dirty);
        })
    };
    let signature_cell = Rc::new(RefCell::new(signature.cloned()));
    *open.borrow_mut() = Some(OpenSubpage {
        id: account_id,
        baseline: baseline.clone(),
        widgets: widgets.clone(),
        state: state.clone(),
        update_dirty: update_dirty.clone(),
        nav_page: nav_page.clone(),
        dialog: dialog.clone(),
        leave_after_save: leave_after_save.clone(),
        leaving: leaving.clone(),
        signature_row: signature_row.clone(),
        from_addresses_group: from_addresses_group.clone(),
        add_identity_row: add_identity_row.clone(),
        envelope_policy_row: envelope_policy_row.clone(),
        catch_all_row: catch_all_row.clone(),
        signature: signature_cell.clone(),
    });

    signature_row.connect_activated({
        let sender = sender.clone();
        let dialog = dialog.clone();
        let signature_cell = signature_cell.clone();
        move |_| {
            let sig = signature_cell.borrow().clone();
            open_signature_dialog(&dialog, account_id, &sender, sig);
        }
    });
    signature_edit_btn.connect_clicked({
        let signature_row = signature_row.clone();
        move |_| {
            adw::prelude::ActionRowExt::activate(&signature_row);
        }
    });
    add_identity_row.connect_activated({
        let sender = sender.clone();
        let account = baseline.borrow().clone();
        let dialog = dialog.clone();
        move |_| {
            open_add_identity_dialog(&dialog, &account, &sender, None);
        }
    });

    name_row.connect_notify_local(Some("text"), {
        let widgets = widgets.clone();
        let update_dirty = update_dirty.clone();
        move |_, _| {
            refresh_hero_from_rows(&widgets);
            update_dirty();
        }
    });
    email_row.connect_notify_local(Some("text"), {
        let widgets = widgets.clone();
        let update_dirty = update_dirty.clone();
        move |_, _| {
            refresh_hero_from_rows(&widgets);
            update_dirty();
        }
    });
    your_name_row.connect_notify_local(Some("text"), {
        let update_dirty = update_dirty.clone();
        move |_, _| {
            update_dirty();
        }
    });
    chip_row.connect_notify_local(Some("text"), {
        let widgets = widgets.clone();
        let update_dirty = update_dirty.clone();
        move |row, _| {
            let sanitized = sanitize_avatar_text(&row.text());
            if row.text().as_str() != sanitized {
                row.set_text(&sanitized);
            }
            refresh_hero_from_rows(&widgets);
            update_dirty();
        }
    });
    color_button.connect_notify_local(Some("rgba"), {
        let widgets = widgets.clone();
        let state = state.clone();
        let update_dirty = update_dirty.clone();
        move |_, _| {
            if state.borrow().syncing_appearance {
                return;
            }
            refresh_hero_from_rows(&widgets);
            update_dirty();
        }
    });
    photo_row.connect_activated({
        let widgets = widgets.clone();
        let dialog = dialog.clone();
        let update_dirty = update_dirty.clone();
        move |_| {
            pick_chip_photo(&dialog, {
                let widgets = widgets.clone();
                let dialog = dialog.clone();
                let update_dirty = update_dirty.clone();
                move |result| match result {
                    Ok(bytes) => {
                        *widgets.pending_image.borrow_mut() = bytes;
                        apply_photo_row(
                            &widgets.photo_row,
                            &widgets.photo_clear,
                            &widgets.pending_image.borrow(),
                        );
                        refresh_hero_from_rows(&widgets);
                        update_dirty();
                    }
                    Err(message) => {
                        dialog.add_toast(crate::toast::plain(message));
                    }
                }
            });
        }
    });
    photo_clear.connect_clicked({
        let widgets = widgets.clone();
        let update_dirty = update_dirty.clone();
        move |_| {
            widgets.pending_image.borrow_mut().clear();
            apply_photo_row(
                &widgets.photo_row,
                &widgets.photo_clear,
                &widgets.pending_image.borrow(),
            );
            refresh_hero_from_rows(&widgets);
            update_dirty();
        }
    });
    username_row.connect_notify_local(Some("text"), {
        let update_dirty = update_dirty.clone();
        move |_, _| update_dirty()
    });
    imap_host_row.connect_notify_local(Some("text"), {
        let update_dirty = update_dirty.clone();
        move |_, _| update_dirty()
    });
    smtp_host_row.connect_notify_local(Some("text"), {
        let update_dirty = update_dirty.clone();
        move |_, _| update_dirty()
    });
    imap_port_row.connect_notify_local(Some("value"), {
        let update_dirty = update_dirty.clone();
        move |_, _| update_dirty()
    });
    smtp_port_row.connect_notify_local(Some("value"), {
        let update_dirty = update_dirty.clone();
        move |_, _| update_dirty()
    });
    smtp_security_row.connect_notify_local(Some("selected"), {
        let update_dirty = update_dirty.clone();
        move |_, _| update_dirty()
    });
    password_row.connect_changed({
        let widgets = widgets.clone();
        let state = state.clone();
        let update_dirty = update_dirty.clone();
        move |_| {
            {
                let mut st = state.borrow_mut();
                if st.syncing_password {
                    return;
                }
                let text = widgets.password_row.text();
                st.password_pristine = text.as_str() == SAVED_PASSWORD_MASK;
            }
            update_dirty();
        }
    });
    if matches!(account.auth, AuthMethod::Password) {
        let focus = gtk::EventControllerFocus::new();
        focus.connect_enter({
            let widgets = widgets.clone();
            let state = state.clone();
            move |_| {
                if !state.borrow().password_pristine {
                    return;
                }
                widgets.password_row.select_region(0, -1);
            }
        });
        focus.connect_leave({
            let widgets = widgets.clone();
            let state = state.clone();
            let baseline = baseline.clone();
            let update_dirty = update_dirty.clone();
            move |_| {
                if !widgets.password_row.text().is_empty() {
                    return;
                }
                restore_saved_password_mask(&widgets, &state, &baseline.borrow());
                update_dirty();
            }
        });
        password_row.add_controller(focus);
    }

    if matches!(account.auth, AuthMethod::Password) {
        apply_credential_mode(&widgets, account);

        split_row.connect_active_notify({
            let widgets = widgets.clone();
            let state = state.clone();
            let baseline = baseline.clone();
            let update_dirty = update_dirty.clone();
            move |_| {
                if state.borrow().syncing_credentials {
                    return;
                }
                let turning_on = widgets.split_row.is_active();
                if turning_on
                    && !state.borrow().smtp_username_touched
                    && !baseline.borrow().uses_split_credentials()
                {
                    state.borrow_mut().syncing_credentials = true;
                    widgets
                        .smtp_username_row
                        .set_text(&widgets.username_row.text());
                    state.borrow_mut().syncing_credentials = false;
                }
                apply_credential_mode(&widgets, &baseline.borrow());
                update_dirty();
            }
        });

        smtp_username_row.connect_notify_local(Some("text"), {
            let state = state.clone();
            let update_dirty = update_dirty.clone();
            move |_, _| {
                if !state.borrow().syncing_credentials {
                    state.borrow_mut().smtp_username_touched = true;
                }
                update_dirty();
            }
        });

        smtp_password_row.connect_changed({
            let widgets = widgets.clone();
            let state = state.clone();
            let update_dirty = update_dirty.clone();
            move |_| {
                {
                    let mut st = state.borrow_mut();
                    if st.syncing_smtp_password {
                        return;
                    }
                    let text = widgets.smtp_password_row.text();
                    st.smtp_password_pristine = text.as_str() == SAVED_PASSWORD_MASK;
                }
                update_dirty();
            }
        });

        let smtp_focus = gtk::EventControllerFocus::new();
        smtp_focus.connect_enter({
            let widgets = widgets.clone();
            let state = state.clone();
            move |_| {
                if !state.borrow().smtp_password_pristine {
                    return;
                }
                widgets.smtp_password_row.select_region(0, -1);
            }
        });
        smtp_focus.connect_leave({
            let widgets = widgets.clone();
            let state = state.clone();
            let baseline = baseline.clone();
            let update_dirty = update_dirty.clone();
            move |_| {
                if !widgets.smtp_password_row.text().is_empty() {
                    return;
                }
                restore_saved_smtp_password_mask(&widgets, &state, &baseline.borrow());
                update_dirty();
            }
        });
        smtp_password_row.add_controller(smtp_focus);
    }

    smtp_security_row.connect_notify_local(Some("selected"), {
        let widgets = widgets.clone();
        let state = state.clone();
        move |_, _| {
            if !smtp_sync_begin(&state) {
                return;
            }
            apply_smtp_port_from_security(&widgets);
            smtp_sync_end(&state);
        }
    });

    smtp_port_row.connect_notify_local(Some("value"), {
        let widgets = widgets.clone();
        let state = state.clone();
        move |_, _| {
            if !smtp_sync_begin(&state) {
                return;
            }
            apply_smtp_security_from_port(&widgets);
            smtp_sync_end(&state);
        }
    });

    let save_handler = {
        let widgets = widgets.clone();
        let state = state.clone();
        let baseline = baseline.clone();
        let dialog = dialog.clone();
        let sender = sender.clone();
        move || -> bool {
            clear_subpage_errors(&widgets);
            let account = baseline.borrow();
            let update = build_settings_update(&widgets, &account, &state.borrow());
            if let Err(message) = update.validate_for_auth(&account.auth) {
                apply_subpage_validation_error(&widgets, message);
                dialog.add_toast(crate::toast::plain(&validation_text(message)));
                return false;
            }
            let envelope_on = widgets.envelope_policy_row.is_active();
            let catch_all = alcedo_core::parse_catch_all_domains(&widgets.catch_all_row.text());
            {
                let mut st = state.borrow_mut();
                if st.envelope_known && envelope_on != st.saved_envelope {
                    st.saved_envelope = envelope_on;
                    sender
                        .send(MailMsg::SetSmtpEnvelopePolicy {
                            account: account_id,
                            same_domain_identity: envelope_on,
                        })
                        .ok();
                }
                if catch_all != st.saved_catch_all {
                    st.saved_catch_all = catch_all.clone();
                    sender
                        .send(MailMsg::SetCatchAllDomains {
                            account: account_id,
                            text: catch_all.join(", "),
                        })
                        .ok();
                }
            }
            sender
                .send(MailMsg::UpdateAccount {
                    id: account_id,
                    update,
                })
                .is_ok()
        }
    };

    save_row.connect_activated({
        let save_handler = save_handler.clone();
        let leave_after_save = leave_after_save.clone();
        move |_| {
            // A plain Save stays on the page, even if an earlier
            // Save-and-leave from the prompt failed on the server.
            leave_after_save.set(None);
            save_handler();
        }
    });

    back_btn.connect_clicked({
        let widgets = widgets.clone();
        let baseline = baseline.clone();
        let state = state.clone();
        let dialog = dialog.clone();
        let nav_page = nav_page.clone();
        let prompt_open = prompt_open.clone();
        let leaving = leaving.clone();
        let leave_after_save = leave_after_save.clone();
        let save_handler = save_handler.clone();
        move |_| {
            if !subpage_is_dirty(&widgets, &baseline.borrow(), &state.borrow()) {
                finish_account_leave(&dialog, &nav_page, &leaving, AccountLeave::Pop);
                return;
            }
            let label = account_edit_label(&widgets, &baseline.borrow());
            present_unsaved_account_changes(
                &dialog,
                &nav_page,
                &label,
                AccountLeave::Pop,
                &prompt_open,
                &leaving,
                &leave_after_save,
                save_handler.clone(),
            );
        }
    });

    let escape = gtk::ShortcutController::new();
    escape.set_scope(gtk::ShortcutScope::Managed);
    escape.set_propagation_phase(gtk::PropagationPhase::Capture);
    escape.add_shortcut(gtk::Shortcut::new(
        gtk::ShortcutTrigger::parse_string("Escape"),
        Some(gtk::CallbackAction::new({
            let widgets = widgets.clone();
            let baseline = baseline.clone();
            let state = state.clone();
            let dialog = dialog.clone();
            let nav_page = nav_page.clone();
            let prompt_open = prompt_open.clone();
            let leaving = leaving.clone();
            let leave_after_save = leave_after_save.clone();
            let save_handler = save_handler.clone();
            move |_, _| {
                if prompt_open.get() {
                    return glib::Propagation::Proceed;
                }
                if !subpage_is_dirty(&widgets, &baseline.borrow(), &state.borrow()) {
                    return glib::Propagation::Proceed;
                }
                let label = account_edit_label(&widgets, &baseline.borrow());
                present_unsaved_account_changes(
                    &dialog,
                    &nav_page,
                    &label,
                    AccountLeave::Pop,
                    &prompt_open,
                    &leaving,
                    &leave_after_save,
                    save_handler.clone(),
                );
                glib::Propagation::Stop
            }
        })),
    ));
    nav_page.add_controller(escape);

    let close_attempt = Rc::new(RefCell::new(Some(dialog.connect_close_attempt({
        let open = open.clone();
        let widgets = widgets.clone();
        let baseline = baseline.clone();
        let state = state.clone();
        let nav_page = nav_page.clone();
        let prompt_open = prompt_open.clone();
        let leaving = leaving.clone();
        let leave_after_save = leave_after_save.clone();
        let save_handler = save_handler.clone();
        move |dialog| {
            let still_open = open
                .borrow()
                .as_ref()
                .is_some_and(|sub| sub.id == account_id);
            if !still_open {
                return;
            }
            if prompt_open.get() {
                return;
            }
            if !subpage_is_dirty(&widgets, &baseline.borrow(), &state.borrow()) {
                finish_account_leave(dialog, &nav_page, &leaving, AccountLeave::Close);
                return;
            }
            let label = account_edit_label(&widgets, &baseline.borrow());
            present_unsaved_account_changes(
                dialog,
                &nav_page,
                &label,
                AccountLeave::Close,
                &prompt_open,
                &leaving,
                &leave_after_save,
                save_handler.clone(),
            );
        }
    }))));

    if matches!(account.auth, AuthMethod::OAuth2 { .. }) {
        oauth_row.connect_activated({
            let widgets = widgets.clone();
            let state = state.clone();
            let account = account.clone();
            let dialog = dialog.clone();
            let sender = sender.clone();
            let runtime = runtime.clone();
            let update_dirty = update_dirty.clone();
            let oauth_spinner = oauth_spinner.clone();
            move |_| {
                if state.borrow().oauth_in_flight {
                    return;
                }
                state.borrow_mut().oauth_in_flight = true;
                update_dirty();
                widgets.oauth_row.set_sensitive(false);
                oauth_spinner.set_visible(true);

                let oauth_provider = match &account.auth {
                    AuthMethod::OAuth2 { provider } => *provider,
                    AuthMethod::Password => unreachable!(),
                };

                let _oauth_provider = match oauth_provider_config(&oauth_provider) {
                    Ok(cfg) => cfg,
                    Err(message) => {
                        dialog.add_toast(crate::toast::plain(&message));
                        state.borrow_mut().oauth_in_flight = false;
                        oauth_spinner.set_visible(false);
                        widgets.oauth_row.set_sensitive(true);
                        update_dirty();
                        return;
                    }
                };

                dialog.add_toast(crate::toast::plain(&tr!("Opening browser for sign-in…")));

                let oauth_generation = {
                    let mut st = state.borrow_mut();
                    st.oauth_generation = st.oauth_generation.wrapping_add(1);
                    st.oauth_generation
                };

                let account_for_oauth = account.clone();
                let (tx, rx) = oneshot::channel();
                runtime.spawn(async move {
                    let auth_result = mail_oauth_pkce(&account_for_oauth).await;
                    let _ = tx.send(auth_result);
                });

                let widgets = widgets.clone();
                let state = state.clone();
                let baseline = baseline.clone();
                let dialog = dialog.clone();
                let sender = sender.clone();
                let update_dirty = update_dirty.clone();
                let oauth_spinner = oauth_spinner.clone();
                glib::spawn_future_local(async move {
                    let auth_result = rx
                        .await
                        .unwrap_or_else(|_| Err(Error::Auth(tr!("OAuth sign-in was interrupted"))));

                    let cancelled = {
                        let mut st = state.borrow_mut();
                        let cancelled = st.oauth_generation != oauth_generation;
                        st.oauth_in_flight = false;
                        cancelled
                    };
                    oauth_spinner.set_visible(false);
                    widgets.oauth_row.set_sensitive(true);
                    update_dirty();
                    if cancelled {
                        return;
                    }

                    match auth_result {
                        Ok(tokens) => {
                            let saved = baseline.borrow();
                            let mut update =
                                build_settings_update(&widgets, &saved, &state.borrow());
                            update.oauth_tokens = Some(tokens);
                            if let Err(message) = update.validate_for_auth(&saved.auth) {
                                apply_subpage_validation_error(&widgets, message);
                                dialog.add_toast(crate::toast::plain(&validation_text(message)));
                                return;
                            }
                            sender
                                .send(MailMsg::UpdateAccount {
                                    id: account_id,
                                    update,
                                })
                                .ok();
                        }
                        Err(err) => {
                            dialog.add_toast(crate::toast::plain(&err.to_string()));
                        }
                    }
                });
            }
        });
    }

    pgp_sign_outgoing_row.connect_active_notify({
        let sender = sender.clone();
        let state = state.clone();
        move |row| {
            if state.borrow().pgp_syncing {
                return;
            }
            sender
                .send(MailMsg::PgpSetSignOutgoing {
                    account: account_id,
                    enabled: row.is_active(),
                })
                .ok();
        }
    });

    envelope_policy_row.connect_active_notify({
        let state = state.clone();
        let update_dirty = update_dirty.clone();
        move |_| {
            {
                let st = state.borrow();
                if st.envelope_syncing || !st.envelope_known {
                    return;
                }
            }
            update_dirty();
        }
    });
    catch_all_row.connect_notify_local(Some("text"), {
        let update_dirty = update_dirty.clone();
        move |_, _| update_dirty()
    });

    pgp_generate_row.connect_activated({
        let dialog = dialog.clone();
        let sender = sender.clone();
        move |_| {
            let sender = sender.clone();
            prompt_pgp_passphrase(&dialog, &tr!("Choose a passphrase"), move |passphrase| {
                sender
                    .send(MailMsg::PgpGenerate {
                        account: account_id,
                        passphrase,
                    })
                    .ok();
            });
        }
    });

    pgp_import_row.connect_activated({
        let dialog = dialog.clone();
        let sender = sender.clone();
        move |_| {
            let dialog_for_passphrase = dialog.clone();
            let sender_for_import = sender.clone();
            pick_pgp_key_file(&dialog, move |armored| {
                let sender = sender_for_import.clone();
                prompt_pgp_import_passphrase(&dialog_for_passphrase, move |passphrase| {
                    sender
                        .send(MailMsg::PgpImport {
                            account: account_id,
                            armored,
                            passphrase,
                        })
                        .ok();
                });
            });
        }
    });

    pgp_export_row.connect_activated({
        let sender = sender.clone();
        move |_| {
            sender.send(MailMsg::PgpExportPublic(account_id)).ok();
        }
    });

    pgp_remove_row.connect_activated({
        let prefs_dialog = dialog.clone();
        let sender = sender.clone();
        move |_| {
            let alert = adw::AlertDialog::builder()
                .heading(tr!("Remove OpenPGP key?"))
                .body(tr!(
                    "The secret key will be deleted from this device. Sent mail is not affected."
                ))
                .build();
            alert.add_response("cancel", &tr!("Cancel"));
            alert.add_response("remove", &tr!("Remove"));
            alert.set_response_appearance("remove", adw::ResponseAppearance::Destructive);
            alert.set_default_response(Some("cancel"));
            alert.set_close_response("cancel");
            let sender = sender.clone();
            alert.choose(
                Some(&prefs_dialog),
                None::<&gtk::gio::Cancellable>,
                move |response| {
                    if response.as_str() == "remove" {
                        sender.send(MailMsg::PgpDeleteKey(account_id)).ok();
                    }
                },
            );
        }
    });

    remove_row.connect_activated({
        let prefs_dialog = dialog.clone();
        let sender = sender.clone();
        let email = account.email.clone();
        move |_| {
            let alert = adw::AlertDialog::builder()
                .heading(tr!("Remove account?"))
                .body(trf!(
                    "Mail for {email} will be deleted from this computer. The mailbox on the server is not deleted.",
                    email = email
                ))
                .build();
            alert.add_response("cancel", &tr!("Cancel"));
            alert.add_response("remove", &tr!("Remove"));
            alert.set_response_appearance("remove", adw::ResponseAppearance::Destructive);
            alert.set_default_response(Some("cancel"));
            alert.set_close_response("cancel");
            let parent = prefs_dialog.clone();
            let sender = sender.clone();
            alert.choose(
                Some(&parent),
                None::<&gtk::gio::Cancellable>,
                move |response| {
                    if response.as_str() == "remove" {
                        sender.send(MailMsg::RemoveAccount(account_id)).ok();
                    }
                },
            );
        }
    });

    update_dirty();

    nav_page.connect_hidden({
        let state = state.clone();
        let open = open.clone();
        let dialog = dialog.clone();
        let nav_page = nav_page.clone();
        let leaving = leaving.clone();
        let close_attempt = close_attempt.clone();
        move |_| {
            leaving.set(true);
            nav_page.set_can_pop(true);
            dialog.set_can_close(true);
            if let Some(handler) = close_attempt.borrow_mut().take() {
                dialog.disconnect(handler);
            }
            let mut st = state.borrow_mut();
            st.oauth_generation = st.oauth_generation.wrapping_add(1);
            let mut open = open.borrow_mut();
            if open
                .as_ref()
                .is_some_and(|subpage| subpage.id == account_id)
            {
                *open = None;
            }
        }
    });
    dialog.push_subpage(&nav_page);
    sender.send(MailMsg::RefreshAccountPgp(account_id)).ok();
    sender
        .send(MailMsg::PaintAccountIdentities(account_id))
        .ok();
    sender
        .send(MailMsg::RefreshSmtpEnvelopePolicy(account_id))
        .ok();
}

fn refresh_hero_from_rows(widgets: &SubpageWidgets) {
    let email_text = widgets.email_row.text();
    let email = email_text.trim();
    widgets.hero_email.set_text(email);
    let name = widgets.name_row.text();
    let label = if name.trim().is_empty() {
        if email.is_empty() {
            trs!("Account")
        } else {
            email
        }
    } else {
        name.trim()
    };
    let custom = sanitize_avatar_text(&widgets.chip_row.text());
    let chip = if custom.is_empty() {
        avatar_from_label(label)
    } else {
        custom
    };
    widgets.hero_name.set_text(label);
    apply_account_chip_appearance(
        &widgets.hero_avatar,
        &chip,
        &hex_from_rgba(&widgets.color_button.rgba()),
        &widgets.pending_image.borrow(),
    );
}

pub(crate) fn apply_settings_update_to_account(
    account: &mut Account,
    update: &AccountSettingsUpdate,
) -> Result<(), &'static str> {
    update.validate_for_auth(&account.auth)?;
    account.email = update.email.trim().to_string();
    let display_name = update.display_name.trim();
    account.display_name = if display_name.is_empty() {
        alcedo_core::display_name_from_email(&account.email)
    } else {
        display_name.to_string()
    };
    account.sender_name = sanitize_sender_name(&update.sender_name);
    account.avatar_text = sanitize_avatar_text(&update.avatar_text);
    account.accent_color = sanitize_accent_color(&update.accent_color);
    account.avatar_image = update.avatar_image.clone();
    account.username = alcedo_core::login_name_from(&update.username, &account.email);
    account.imap_host = update.imap_host.clone();
    account.imap_port = update.imap_port;
    account.smtp_host = update.smtp_host.clone();
    account.smtp_port = update.smtp_port;
    account.smtp_starttls = update.smtp_starttls;
    match &account.auth {
        AuthMethod::OAuth2 { .. } => {
            account.split_credentials = false;
            account.smtp_username.clear();
        }
        AuthMethod::Password if update.split_credentials => {
            account.split_credentials = true;
            account.smtp_username =
                alcedo_core::login_name_from(&update.smtp_username, &account.email);
        }
        AuthMethod::Password => {
            account.split_credentials = false;
            account.smtp_username.clear();
        }
    }
    Ok(())
}

fn build_settings_update(
    widgets: &SubpageWidgets,
    account: &Account,
    state: &SubpageState,
) -> AccountSettingsUpdate {
    let password_auth = matches!(account.auth, AuthMethod::Password);
    let split_credentials = password_auth && widgets.split_row.is_active();

    let password = if password_auth {
        let value = widgets.password_row.text().to_string();
        if state.password_pristine || value.is_empty() || value == SAVED_PASSWORD_MASK {
            None
        } else {
            Some(value)
        }
    } else {
        None
    };

    let smtp_password = if split_credentials {
        let value = widgets.smtp_password_row.text().to_string();
        if state.smtp_password_pristine || value.is_empty() || value == SAVED_PASSWORD_MASK {
            None
        } else {
            Some(value)
        }
    } else {
        None
    };

    let smtp_username = if split_credentials {
        alcedo_core::username_for_email_change(
            widgets.smtp_username_row.text().as_ref(),
            &account.email,
            widgets.email_row.text().trim(),
        )
    } else {
        String::new()
    };

    AccountSettingsUpdate {
        email: widgets.email_row.text().trim().to_string(),
        display_name: widgets.name_row.text().to_string(),
        sender_name: widgets.your_name_row.text().to_string(),
        avatar_text: sanitize_avatar_text(&widgets.chip_row.text()),
        accent_color: hex_from_rgba(&widgets.color_button.rgba()),
        avatar_image: widgets.pending_image.borrow().clone(),
        username: alcedo_core::username_for_email_change(
            widgets.username_row.text().as_ref(),
            &account.email,
            widgets.email_row.text().trim(),
        ),
        imap_host: widgets.imap_host_row.text().to_string(),
        imap_port: widgets.imap_port_row.value() as u16,
        smtp_host: widgets.smtp_host_row.text().to_string(),
        smtp_port: widgets.smtp_port_row.value() as u16,
        smtp_starttls: widgets.smtp_security_row.selected() == SMTP_STARTTLS,
        password,
        oauth_tokens: None,
        split_credentials,
        smtp_username,
        smtp_password,
    }
}

fn subpage_is_dirty(widgets: &SubpageWidgets, account: &Account, state: &SubpageState) -> bool {
    if widgets.name_row.text().as_str() != account.display_name {
        return true;
    }
    if widgets.email_row.text().trim() != account.email {
        return true;
    }
    if sanitize_sender_name(&widgets.your_name_row.text()) != account.sender_name {
        return true;
    }
    if sanitize_avatar_text(&widgets.chip_row.text()) != sanitize_avatar_text(&account.avatar_text)
    {
        return true;
    }
    if hex_from_rgba(&widgets.color_button.rgba()) != sanitize_accent_color(&account.accent_color) {
        return true;
    }
    if widgets.pending_image.borrow().as_slice() != account.avatar_image.as_slice() {
        return true;
    }
    if widgets.username_row.text().as_str() != account.login_name() {
        return true;
    }
    if widgets.imap_host_row.text().as_str() != account.imap_host {
        return true;
    }
    if widgets.imap_port_row.value() as u16 != account.imap_port {
        return true;
    }
    if widgets.smtp_host_row.text().as_str() != account.smtp_host {
        return true;
    }
    if widgets.smtp_port_row.value() as u16 != account.smtp_port {
        return true;
    }
    let starttls = widgets.smtp_security_row.selected() == SMTP_STARTTLS;
    if starttls != account.smtp_starttls {
        return true;
    }
    if state.envelope_known && widgets.envelope_policy_row.is_active() != state.saved_envelope {
        return true;
    }
    if alcedo_core::parse_catch_all_domains(&widgets.catch_all_row.text()) != state.saved_catch_all
    {
        return true;
    }
    if matches!(account.auth, AuthMethod::Password) {
        if widgets.split_row.is_active() != account.uses_split_credentials() {
            return true;
        }
        if widgets.split_row.is_active()
            && widgets.smtp_username_row.text().as_str() != account.smtp_login_name()
        {
            return true;
        }
        if !state.password_pristine
            && !widgets.password_row.text().is_empty()
            && widgets.password_row.text().as_str() != SAVED_PASSWORD_MASK
        {
            return true;
        }
        if widgets.split_row.is_active()
            && !state.smtp_password_pristine
            && !widgets.smtp_password_row.text().is_empty()
            && widgets.smtp_password_row.text().as_str() != SAVED_PASSWORD_MASK
        {
            return true;
        }
    }
    false
}

#[derive(Clone, Copy)]
enum AccountLeave {
    Pop,
    Close,
}

fn account_edit_label(widgets: &SubpageWidgets, account: &Account) -> String {
    let name = widgets.name_row.text();
    let name = name.trim();
    if !name.is_empty() {
        return name.to_string();
    }
    let email = widgets.email_row.text();
    let email = email.trim();
    if !email.is_empty() {
        return email.to_string();
    }
    account.label().to_string()
}

fn finish_account_leave(
    dialog: &adw::PreferencesDialog,
    nav_page: &adw::NavigationPage,
    leaving: &Cell<bool>,
    intent: AccountLeave,
) {
    leaving.set(true);
    nav_page.set_can_pop(true);
    dialog.set_can_close(true);
    match intent {
        AccountLeave::Pop => {
            dialog.pop_subpage();
        }
        AccountLeave::Close => {
            dialog.close();
        }
    }
}

/// Discard leaves now. Save waits for AccountUpdated via mark_account_saved.
fn unsaved_alert_leaves_now(response: &str) -> bool {
    response == "discard"
}

fn present_unsaved_account_changes(
    dialog: &adw::PreferencesDialog,
    nav_page: &adw::NavigationPage,
    account_label: &str,
    intent: AccountLeave,
    prompt_open: &Rc<Cell<bool>>,
    leaving: &Rc<Cell<bool>>,
    leave_after_save: &Rc<Cell<Option<AccountLeave>>>,
    save: impl Fn() -> bool + 'static,
) {
    if prompt_open.get() {
        return;
    }
    prompt_open.set(true);
    let heading = alcedo_core::compose_status::unsaved_account_heading(account_label);
    let alert = adw::AlertDialog::new(
        Some(&heading),
        Some(trm!(alcedo_core::compose_status::UNSAVED_ACCOUNT_BODY)),
    );
    alert.set_heading_use_markup(false);
    alert.set_body_use_markup(false);
    alert.set_heading(Some(&heading));
    alert.set_body(trm!(alcedo_core::compose_status::UNSAVED_ACCOUNT_BODY));
    alert.add_response("cancel", &tr!("Cancel"));
    alert.add_response("discard", &tr!("Discard"));
    alert.add_response("save", &tr!("Save"));
    alert.set_response_appearance("discard", adw::ResponseAppearance::Destructive);
    alert.set_response_appearance("save", adw::ResponseAppearance::Suggested);
    alert.set_default_response(Some("cancel"));
    alert.set_close_response("cancel");
    let parent = dialog.clone();
    let dialog = dialog.clone();
    let nav_page = nav_page.clone();
    let prompt_open = prompt_open.clone();
    let leaving = leaving.clone();
    let leave_after_save = leave_after_save.clone();
    alert.choose(
        Some(&parent),
        None::<&gtk::gio::Cancellable>,
        move |response| {
            prompt_open.set(false);
            let response = response.as_str();
            if unsaved_alert_leaves_now(response) {
                leave_after_save.set(None);
                finish_account_leave(&dialog, &nav_page, &leaving, intent);
            } else if response == "save" {
                leave_after_save.set(Some(intent));
                if !save() {
                    leave_after_save.set(None);
                }
            } else {
                leave_after_save.set(None);
            }
        },
    );
}

fn smtp_sync_begin(state: &Rc<RefCell<SubpageState>>) -> bool {
    let mut st = state.borrow_mut();
    if st.syncing_smtp {
        return false;
    }
    st.syncing_smtp = true;
    true
}

fn smtp_sync_end(state: &Rc<RefCell<SubpageState>>) {
    state.borrow_mut().syncing_smtp = false;
}

fn apply_smtp_port_from_security(widgets: &SubpageWidgets) {
    let port = widgets.smtp_port_row.value() as u16;
    let new_port = match widgets.smtp_security_row.selected() {
        SMTP_STARTTLS if port == 465 || port == 587 => 587,
        SMTP_SSL_TLS if port == 465 || port == 587 => 465,
        _ => port,
    };
    widgets.smtp_port_row.set_value(f64::from(new_port));
}

fn apply_smtp_security_from_port(widgets: &SubpageWidgets) {
    match widgets.smtp_port_row.value() as u16 {
        465 => widgets.smtp_security_row.set_selected(SMTP_SSL_TLS),
        587 => widgets.smtp_security_row.set_selected(SMTP_STARTTLS),
        _ => {}
    }
}

fn clear_subpage_errors(widgets: &SubpageWidgets) {
    for row in [
        widgets.name_row.upcast_ref::<gtk::Widget>(),
        widgets.email_row.upcast_ref::<gtk::Widget>(),
        widgets.username_row.upcast_ref::<gtk::Widget>(),
        widgets.imap_host_row.upcast_ref::<gtk::Widget>(),
        widgets.smtp_host_row.upcast_ref::<gtk::Widget>(),
        widgets.imap_port_row.upcast_ref::<gtk::Widget>(),
        widgets.smtp_port_row.upcast_ref::<gtk::Widget>(),
        widgets.password_row.upcast_ref::<gtk::Widget>(),
        widgets.smtp_username_row.upcast_ref::<gtk::Widget>(),
        widgets.smtp_password_row.upcast_ref::<gtk::Widget>(),
    ] {
        row.remove_css_class("error");
    }
}

fn apply_subpage_validation_error(widgets: &SubpageWidgets, message: &str) {
    match message {
        "Email is required" | "Email must contain @" | "Email is invalid" => {
            widgets.email_row.add_css_class("error")
        }
        "IMAP host is required" | "IMAP host must match the OAuth provider" => {
            widgets.imap_host_row.add_css_class("error")
        }
        "SMTP host is required" | "SMTP host must match the OAuth provider" => {
            widgets.smtp_host_row.add_css_class("error")
        }
        "IMAP port must be between 1 and 65535" => widgets.imap_port_row.add_css_class("error"),
        "SMTP port must be between 1 and 65535" => widgets.smtp_port_row.add_css_class("error"),
        _ => {}
    }
}

fn restore_saved_password_mask(
    widgets: &SubpageWidgets,
    state: &Rc<RefCell<SubpageState>>,
    account: &Account,
) {
    if !matches!(account.auth, AuthMethod::Password) {
        return;
    }
    let mut st = state.borrow_mut();
    st.syncing_password = true;
    st.password_pristine = true;
    drop(st);
    widgets.password_row.set_text(SAVED_PASSWORD_MASK);
    state.borrow_mut().syncing_password = false;
}

fn restore_saved_smtp_password_mask(
    widgets: &SubpageWidgets,
    state: &Rc<RefCell<SubpageState>>,
    account: &Account,
) {
    if !matches!(account.auth, AuthMethod::Password) {
        return;
    }
    let mut st = state.borrow_mut();
    st.syncing_smtp_password = true;
    st.smtp_password_pristine = true;
    drop(st);
    widgets.smtp_password_row.set_text(SAVED_PASSWORD_MASK);
    state.borrow_mut().syncing_smtp_password = false;
}

fn apply_credential_mode(widgets: &SubpageWidgets, account: &Account) {
    if !matches!(account.auth, AuthMethod::Password) {
        return;
    }
    let split = widgets.split_row.is_active();
    widgets.smtp_username_row.set_visible(split);
    widgets.smtp_password_row.set_visible(split);
    if split {
        widgets.username_row.set_title(&tr!("IMAP username"));
        widgets.password_row.set_title(&tr!("IMAP password"));
        widgets.smtp_username_row.set_title(&tr!("SMTP username"));
        widgets.smtp_password_row.set_title(&tr!("SMTP password"));
        widgets.auth_group.set_description(Some(&tr!("Each saved password appears as dots. Replace the dots only to change that password. Mail filters keep using the incoming sign-in.")));
    } else {
        widgets.username_row.set_title(&tr!("Username"));
        widgets.password_row.set_title(&tr!("Password"));
        widgets.auth_group.set_description(Some(&tr!(
            "The saved password appears as dots. Replace the dots only to change it."
        )));
    }
}

fn prompt_pgp_passphrase(
    parent: &impl IsA<gtk::Widget>,
    heading: &str,
    on_ok: impl FnOnce(String) + 'static,
) {
    let pass_row = adw::PasswordEntryRow::builder()
        .title(tr!("Passphrase"))
        .show_apply_button(false)
        .build();
    let confirm_row = adw::PasswordEntryRow::builder()
        .title(tr!("Confirm passphrase"))
        .show_apply_button(false)
        .build();
    let box_ = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .margin_top(6)
        .margin_bottom(6)
        .build();
    box_.append(&pass_row);
    box_.append(&confirm_row);

    let dialog = adw::AlertDialog::builder()
        .heading(heading)
        .body(tr!(
            "Use a strong passphrase. Alcedo stores it in your system keyring."
        ))
        .extra_child(&box_)
        .build();
    dialog.add_response("cancel", &tr!("Cancel"));
    dialog.add_response("ok", &tr!("Continue"));
    dialog.set_response_appearance("ok", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("ok"));
    dialog.set_close_response("cancel");

    dialog.choose(
        Some(parent),
        None::<&gtk::gio::Cancellable>,
        move |response| {
            if response.as_str() != "ok" {
                return;
            }
            let pass = pass_row.text().to_string();
            let confirm = confirm_row.text().to_string();
            if pass.is_empty() {
                return;
            }
            if pass != confirm {
                return;
            }
            on_ok(pass);
        },
    );
}

fn prompt_pgp_import_passphrase(
    parent: &impl IsA<gtk::Widget>,
    on_ok: impl FnOnce(Option<String>) + 'static,
) {
    let pass_row = adw::PasswordEntryRow::builder()
        .title(tr!("Passphrase"))
        .show_apply_button(false)
        .build();
    let dialog = adw::AlertDialog::builder()
        .heading(tr!("Import OpenPGP key"))
        .body(tr!(
            "Leave the passphrase blank if the key is not encrypted."
        ))
        .extra_child(&pass_row)
        .build();
    dialog.add_response("cancel", &tr!("Cancel"));
    dialog.add_response("import", &tr!("Import"));
    dialog.set_response_appearance("import", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("import"));
    dialog.set_close_response("cancel");

    dialog.choose(
        Some(parent),
        None::<&gtk::gio::Cancellable>,
        move |response| {
            if response.as_str() != "import" {
                return;
            }
            let pass = pass_row.text().to_string();
            let passphrase = if pass.is_empty() { None } else { Some(pass) };
            on_ok(passphrase);
        },
    );
}

fn apply_photo_row(row: &adw::ActionRow, clear: &gtk::Button, image: &[u8]) {
    if image.is_empty() {
        row.set_subtitle(&tr!("None"));
        clear.set_visible(false);
    } else {
        row.set_subtitle(&tr!("Cropped to a circle"));
        clear.set_visible(true);
    }
}

fn pick_chip_photo(
    _parent: &adw::PreferencesDialog,
    on_picked: impl FnOnce(Result<Vec<u8>, &'static str>) + 'static,
) {
    let dialog = gtk::FileDialog::new();
    dialog.set_title(&tr!("Choose chip photo"));
    let filter = gtk::FileFilter::new();
    filter.set_name(Some(&tr!("Images")));
    filter.add_mime_type("image/*");
    filter.add_mime_type("image/jpeg");
    filter.add_mime_type("image/png");
    filter.add_mime_type("image/gif");
    filter.add_mime_type("image/webp");
    dialog.set_default_filter(Some(&filter));

    let parent = relm4::main_application().active_window();
    dialog.open(
        parent.as_ref(),
        None::<&gtk::gio::Cancellable>,
        move |result| {
            let Ok(file) = result else {
                return;
            };
            let Some(path) = file.path() else {
                return;
            };
            glib::spawn_future_local(async move {
                match crate::blocking_io::run(move || std::fs::read(&path)).await {
                    Some(Ok(bytes)) => on_picked(crop_avatar_image(&bytes)),
                    _ => on_picked(Err(trs!("Could not read image"))),
                }
            });
        },
    );
}

fn pick_pgp_key_file(_parent: &adw::PreferencesDialog, on_picked: impl FnOnce(String) + 'static) {
    let dialog = gtk::FileDialog::new();
    dialog.set_title(&tr!("Import OpenPGP key"));
    let filter = gtk::FileFilter::new();
    filter.set_name(Some(&tr!("OpenPGP keys")));
    filter.add_mime_type("application/pgp-keys");
    filter.add_pattern("*.asc");
    filter.add_pattern("*.gpg");
    filter.add_pattern("*.pgp");
    dialog.set_default_filter(Some(&filter));

    let parent = relm4::main_application().active_window();
    dialog.open(
        parent.as_ref(),
        None::<&gtk::gio::Cancellable>,
        move |result| {
            let Ok(file) = result else {
                return;
            };
            let Some(path) = file.path() else {
                return;
            };
            glib::spawn_future_local(async move {
                match crate::blocking_io::run(move || std::fs::read_to_string(path)).await {
                    Some(Ok(armored)) => on_picked(armored),
                    Some(Err(err)) => {
                        tracing::warn!(error = %err, "failed to read OpenPGP key file");
                    }
                    None => {}
                }
            });
        },
    );
}

fn from_addresses_helper(auth: &AuthMethod) -> &'static str {
    match auth {
        AuthMethod::OAuth2 {
            provider: OAuthProvider::Google,
        } => {
            trs!(
                "Extra From addresses only work if you have added them in Gmail under Settings → Accounts → Send mail as. Plus-addresses of this account (like you+shop@gmail.com) usually work. Keep “Send from the saved address itself” off unless Gmail has accepted that address."
            )
        }
        AuthMethod::OAuth2 {
            provider: OAuthProvider::Microsoft,
        } => {
            trs!(
                "Extra From addresses only work if Outlook allows this mailbox to send as them. Keep “Send from the saved address itself” off unless Outlook has accepted that address."
            )
        }
        AuthMethod::Password => {
            trs!(
                "Add extra addresses you can choose as the From address when writing. Saved addresses on this account’s domain can be used as the real sender too (see the switch below). An address you type while writing, or one on another domain, is still sent through this account. Some mail servers reject mail sent from an address they don’t know."
            )
        }
    }
}

fn signature_row_subtitle(sig: &AccountSignature) -> Option<String> {
    let plain = match sig.format {
        ComposeFormat::Html => html_to_plain_email(&sig.body),
        _ => sig.body.clone(),
    };
    let first = plain.lines().map(str::trim).find(|line| !line.is_empty())?;
    if first.chars().count() > 40 {
        Some(format!("{}…", first.chars().take(40).collect::<String>()))
    } else {
        Some(first.to_string())
    }
}

/// Refresh the signature row on the open account subpage.
pub fn refresh_signature_subpage(page: &AccountsPage, signature: Option<&AccountSignature>) {
    let open = page.open.borrow();
    let Some(open) = open.as_ref() else {
        return;
    };
    *open.signature.borrow_mut() = signature.cloned();
    let subtitle = signature
        .and_then(signature_row_subtitle)
        .unwrap_or_else(|| "None".to_string());
    open.signature_row.set_subtitle(&subtitle);
}

/// Rebuild extra From identity rows on the open account subpage.
pub fn refresh_identities_subpage(
    page: &AccountsPage,
    identities: &[AccountIdentity],
    sender: &Sender<MailMsg>,
) {
    let open = page.open.borrow();
    let Some(open) = open.as_ref() else {
        return;
    };
    let group = &open.from_addresses_group;
    while group.row(0).is_some() {
        group.remove(&group.row(0).expect("row exists"));
    }
    group.add(&open.envelope_policy_row);
    group.add(&open.catch_all_row);
    let dialog = open.dialog.clone();
    let account = open.baseline.borrow().clone();
    for identity in identities {
        let row = adw::ActionRow::builder().activatable(true).build();
        row.set_use_markup(false);
        row.set_title(&identity.email);
        let subtitle = identity.display_name.trim();
        if !subtitle.is_empty() {
            row.set_subtitle(subtitle);
        }
        let accessible_label = trf!("From address {email}", email = identity.email);
        row.update_property(&[gtk::accessible::Property::Label(&accessible_label)]);
        let delete_btn = gtk::Button::builder()
            .icon_name("edit-delete-symbolic")
            .tooltip_text(tr!("Delete"))
            .css_classes(["flat"])
            .build();
        let identity_id = identity.id;
        let sender_delete = sender.clone();
        delete_btn.connect_clicked(move |_| {
            sender_delete
                .send(MailMsg::DeleteIdentity(identity_id))
                .ok();
        });
        row.add_suffix(&delete_btn);
        let identity = identity.clone();
        let sender_edit = sender.clone();
        let account = account.clone();
        let dialog = dialog.clone();
        row.connect_activated(move |_| {
            open_add_identity_dialog(&dialog, &account, &sender_edit, Some(&identity));
        });
        group.add(&row);
    }
    group.add(&open.add_identity_row);
}

/// "Catch-all domains" entry: every address on these domains reaches this
/// mailbox, so replies to mail sent to one of them come from that address and
/// Reply All never copies it back to the user.
fn catch_all_domains_row() -> adw::EntryRow {
    let row = adw::EntryRow::builder()
        .title(tr!("Catch-all domains (comma separated)"))
        .show_apply_button(false)
        .build();
    row.set_tooltip_text(Some(&tr!(
        "Every address on these domains is yours. Replies use the address the mail was sent to."
    )));
    row
}

/// Show the saved catch-all domains on the open account subpage.
pub fn refresh_catch_all_subpage(page: &AccountsPage, account_id: AccountId, domains: &[String]) {
    let open_guard = page.open.borrow();
    let Some(open) = open_guard.as_ref() else {
        return;
    };
    if open.id == account_id {
        open.state.borrow_mut().saved_catch_all = domains.to_vec();
        open.catch_all_row.set_text(&domains.join(", "));
        (open.update_dirty)();
    }
}

/// Refresh the envelope-policy switch on the open account subpage.
pub fn refresh_envelope_policy_subpage(
    page: &AccountsPage,
    account_id: AccountId,
    policy: SmtpEnvelopePolicy,
) {
    let open_guard = page.open.borrow();
    let Some(open) = open_guard.as_ref() else {
        return;
    };
    if open.id != account_id {
        return;
    }
    {
        let mut st = open.state.borrow_mut();
        st.envelope_syncing = true;
    }
    let on = policy == SmtpEnvelopePolicy::SameDomainIdentity;
    open.envelope_policy_row.set_active(on);
    {
        let mut st = open.state.borrow_mut();
        st.envelope_known = true;
        st.envelope_syncing = false;
        st.saved_envelope = on;
    }
    (open.update_dirty)();
}

pub fn open_add_identity_dialog(
    parent: &adw::PreferencesDialog,
    account: &Account,
    sender: &Sender<MailMsg>,
    existing: Option<&AccountIdentity>,
) {
    let name_row = adw::EntryRow::builder().title(tr!("Name")).build();
    let email_row = adw::EntryRow::builder().title(tr!("Email")).build();
    let identity_id = existing
        .map(|identity| identity.id)
        .unwrap_or_else(|| alcedo_core::IdentityId(Uuid::new_v4()));
    if let Some(identity) = existing {
        name_row.set_text(&identity.display_name);
        email_row.set_text(&identity.email);
    }

    let cancel_btn = gtk::Button::builder().label(tr!("Cancel")).build();
    let add_btn = gtk::Button::builder()
        .label(if existing.is_some() {
            tr!("Save")
        } else {
            tr!("Add")
        })
        .css_classes(["suggested-action"])
        .build();
    let header = adw::HeaderBar::new();
    header.pack_start(&cancel_btn);
    header.pack_end(&add_btn);

    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .margin_top(18)
        .margin_bottom(18)
        .margin_start(18)
        .margin_end(18)
        .build();
    content.append(&name_row);
    content.append(&email_row);

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&content));

    let dialog = adw::Dialog::builder()
        .title(tr!("From address"))
        .content_width(480)
        .default_widget(&add_btn)
        .child(&toolbar)
        .build();

    let account_id = account.id;
    let account_email = account.email.clone();
    let sender = sender.clone();
    let toast_parent = parent.clone();
    cancel_btn.connect_clicked({
        let dialog = dialog.clone();
        move |_| {
            dialog.close();
        }
    });
    let name_row_focus = name_row.clone();
    let email_row_focus = email_row.clone();
    add_btn.connect_clicked({
        let dialog = dialog.clone();
        let name_row = name_row.clone();
        let email_row = email_row.clone();
        move |_| {
            let email = email_row.text().trim().to_string();
            if email.is_empty() {
                toast_parent.add_toast(crate::toast::plain(&tr!("Email is required")));
                return;
            }
            if !email.contains('@') {
                toast_parent.add_toast(crate::toast::plain(&tr!("Email must contain @")));
                return;
            }
            if email.eq_ignore_ascii_case(&account_email) {
                toast_parent.add_toast(crate::toast::plain(&tr!(
                    "This address is already the account address"
                )));
                return;
            }
            let identity = AccountIdentity {
                id: identity_id,
                account_id,
                display_name: name_row.text().trim().to_string(),
                email: email.clone(),
            };
            if let Err(message) = identity.validate(&account_email) {
                toast_parent.add_toast(crate::toast::plain(&validation_text(message)));
                return;
            }
            sender.send(MailMsg::UpsertIdentity(identity)).ok();
            dialog.close();
        }
    });
    dialog.present(Some(parent));
    if existing.is_some() {
        name_row_focus.grab_focus();
    } else {
        email_row_focus.grab_focus();
    }
}

#[cfg(test)]
mod tests {
    use super::unsaved_alert_leaves_now;

    #[test]
    fn save_on_unsaved_alert_does_not_leave_immediately() {
        assert!(!unsaved_alert_leaves_now("save"));
        assert!(unsaved_alert_leaves_now("discard"));
        assert!(!unsaved_alert_leaves_now("cancel"));
    }
}
