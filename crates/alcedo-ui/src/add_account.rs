//! Add Account dialog (Generic IMAP / Yahoo / OAuth).

use std::cell::RefCell;
use std::rc::Rc;

use alcedo_core::{
    Account, AccountDraft, AuthMethod, Error, OAuthProvider, display_name_from_email,
    login_name_from, mail_provider_labels, smtp_security_labels,
};
use alcedo_oauth::{
    GOOGLE_MAIL_SCOPE, MICROSOFT_SCOPES, ProviderOAuthConfig, google_auth_flow, load_oauth_config,
    microsoft_auth_flow,
};
use relm4::Sender;
use relm4::adw;
use relm4::adw::prelude::*;
use relm4::gtk;
use relm4::gtk::glib;
use tokio::runtime::Handle;
use tokio::sync::oneshot;

use crate::mail::MailMsg;

const PROVIDER_GOOGLE: u32 = 0;
const PROVIDER_MICROSOFT: u32 = 1;
const PROVIDER_YAHOO: u32 = 2;
const PROVIDER_GENERIC: u32 = 3;

const SMTP_IMPLICIT_TLS: u32 = 0;
const SMTP_STARTTLS: u32 = 1;

struct AppliedPreset {
    imap_host: String,
    smtp_host: String,
    provider: u32,
}

struct DialogState {
    syncing_smtp: bool,
    syncing_provider: bool,
    /// Provider the user picked by hand. Once set, typed addresses no longer change it.
    user_picked_provider: Option<u32>,
    submitting: bool,
    oauth_generation: u64,
    last_preset: Option<AppliedPreset>,
}

struct DialogWidgets {
    name_row: adw::EntryRow,
    email_row: adw::EntryRow,
    username_row: adw::EntryRow,
    password_row: adw::PasswordEntryRow,
    split_row: adw::SwitchRow,
    smtp_username_row: adw::EntryRow,
    smtp_password_row: adw::PasswordEntryRow,
    provider_row: adw::ComboRow,
    imap_host_row: adw::EntryRow,
    imap_port_row: adw::SpinRow,
    smtp_host_row: adw::EntryRow,
    smtp_port_row: adw::SpinRow,
    smtp_security_row: adw::ComboRow,
    server_group: adw::PreferencesGroup,
    server_expander: adw::ExpanderRow,
    oauth_group: adw::PreferencesGroup,
    toast_overlay: adw::ToastOverlay,
    add_btn: gtk::Button,
    oauth_spinner: adw::Spinner,
}

/// Build the Add Account dialog wired to [`MailMsg`].
pub fn build_add_account_dialog(sender: Sender<MailMsg>, runtime: Handle) -> adw::Dialog {
    let provider_labels = mail_provider_labels();
    let provider_model = gtk::StringList::new(&provider_labels);
    let security_labels = smtp_security_labels();
    let security_model = gtk::StringList::new(&security_labels);

    let name_row = adw::EntryRow::builder().title("Account name").build();
    let email_row = adw::EntryRow::builder().title("Email").build();
    let username_row = adw::EntryRow::builder().title("Username").build();
    let password_row = adw::PasswordEntryRow::builder().title("Password").build();
    let split_row = adw::SwitchRow::builder()
        .title("Different SMTP sign-in")
        .subtitle("Use a separate username and password for outgoing mail.")
        .active(false)
        .build();
    let smtp_username_row = adw::EntryRow::builder().title("SMTP username").build();
    let smtp_password_row = adw::PasswordEntryRow::builder()
        .title("SMTP password")
        .build();
    let provider_row = adw::ComboRow::builder()
        .title("Provider")
        .model(&provider_model)
        .expression(gtk::PropertyExpression::new(
            gtk::StringObject::static_type(),
            None::<&gtk::Expression>,
            "string",
        ))
        .build();
    provider_row.set_selected(PROVIDER_GENERIC);

    let imap_host_row = adw::EntryRow::builder().title("IMAP host").build();
    let imap_port_row = adw::SpinRow::with_range(1.0, 65535.0, 1.0);
    imap_port_row.set_title("IMAP port");
    imap_port_row.set_value(993.0);

    let smtp_host_row = adw::EntryRow::builder().title("SMTP host").build();
    let smtp_port_row = adw::SpinRow::with_range(1.0, 65535.0, 1.0);
    smtp_port_row.set_title("SMTP port");
    smtp_port_row.set_value(465.0);

    let smtp_security_row = adw::ComboRow::builder()
        .title("SMTP security")
        .model(&security_model)
        .expression(gtk::PropertyExpression::new(
            gtk::StringObject::static_type(),
            None::<&gtk::Expression>,
            "string",
        ))
        .build();
    smtp_security_row.set_selected(SMTP_IMPLICIT_TLS);

    let account_group = adw::PreferencesGroup::new();
    account_group.add(&email_row);
    account_group.add(&name_row);
    account_group.add(&provider_row);
    account_group.add(&username_row);
    account_group.add(&password_row);
    account_group.add(&split_row);
    account_group.add(&smtp_username_row);
    account_group.add(&smtp_password_row);

    let server_expander = adw::ExpanderRow::builder()
        .title("Server Settings")
        .subtitle("Filled in from your email address when Alcedo recognizes it.")
        .expanded(false)
        .build();
    server_expander.add_row(&imap_host_row);
    server_expander.add_row(&imap_port_row);
    server_expander.add_row(&smtp_host_row);
    server_expander.add_row(&smtp_port_row);
    server_expander.add_row(&smtp_security_row);
    let server_group = adw::PreferencesGroup::new();
    server_group.add(&server_expander);

    let oauth_info = adw::ActionRow::builder()
        .title("Browser sign-in")
        .subtitle("Click Add to open your browser and complete sign-in.")
        .build();
    let oauth_group = adw::PreferencesGroup::new();
    oauth_group.add(&oauth_info);
    oauth_group.set_visible(false);

    let page = adw::PreferencesPage::new();
    page.add(&account_group);
    page.add(&server_group);
    page.add(&oauth_group);

    let toast_overlay = adw::ToastOverlay::new();
    toast_overlay.set_child(Some(&page));

    let cancel_btn = gtk::Button::builder().label("Cancel").build();
    let oauth_spinner = adw::Spinner::new();
    oauth_spinner.set_visible(false);
    let add_btn = gtk::Button::builder()
        .label("Add")
        .css_classes(["suggested-action"])
        .build();

    let window_title = adw::WindowTitle::builder().title("Add Account").build();
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&window_title));
    header.pack_start(&cancel_btn);
    header.pack_end(&oauth_spinner);
    header.pack_end(&add_btn);

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&toast_overlay));

    let dialog = adw::Dialog::builder()
        .title("Add Account")
        .content_width(480)
        .child(&toolbar)
        .build();

    let widgets = Rc::new(DialogWidgets {
        name_row: name_row.clone(),
        email_row: email_row.clone(),
        username_row: username_row.clone(),
        password_row: password_row.clone(),
        split_row: split_row.clone(),
        smtp_username_row: smtp_username_row.clone(),
        smtp_password_row: smtp_password_row.clone(),
        provider_row: provider_row.clone(),
        imap_host_row: imap_host_row.clone(),
        imap_port_row: imap_port_row.clone(),
        smtp_host_row: smtp_host_row.clone(),
        smtp_port_row: smtp_port_row.clone(),
        smtp_security_row: smtp_security_row.clone(),
        server_group: server_group.clone(),
        server_expander: server_expander.clone(),
        oauth_group: oauth_group.clone(),
        toast_overlay: toast_overlay.clone(),
        add_btn: add_btn.clone(),
        oauth_spinner: oauth_spinner.clone(),
    });

    let state = Rc::new(RefCell::new(DialogState {
        syncing_smtp: false,
        syncing_provider: false,
        user_picked_provider: None,
        submitting: false,
        oauth_generation: 0,
        last_preset: None,
    }));

    let update_add_sensitive = {
        let widgets = widgets.clone();
        let state = state.clone();
        move || {
            if state.borrow().submitting {
                widgets.add_btn.set_sensitive(false);
                let provider = widgets.provider_row.selected();
                let oauth = provider == PROVIDER_GOOGLE || provider == PROVIDER_MICROSOFT;
                if oauth {
                    widgets.oauth_spinner.set_visible(true);
                }
                return;
            }
            widgets.oauth_spinner.set_visible(false);
            let provider = widgets.provider_row.selected();
            let oauth = provider == PROVIDER_GOOGLE || provider == PROVIDER_MICROSOFT;
            if oauth {
                widgets
                    .add_btn
                    .set_sensitive(!widgets.email_row.text().is_empty());
                return;
            }
            let email_ok = !widgets.email_row.text().is_empty();
            let password_ok = !widgets.password_row.text().is_empty();
            let split = widgets.split_row.is_active();
            let smtp_password_ok = !split || !widgets.smtp_password_row.text().is_empty();
            let hosts_ok = !widgets.imap_host_row.text().is_empty()
                && !widgets.smtp_host_row.text().is_empty();
            widgets
                .add_btn
                .set_sensitive(email_ok && password_ok && smtp_password_ok && hosts_ok);
        }
    };

    let apply_provider_visibility = {
        let widgets = widgets.clone();
        let update_add_sensitive = update_add_sensitive.clone();
        move || {
            let provider = widgets.provider_row.selected();
            let oauth = provider == PROVIDER_GOOGLE || provider == PROVIDER_MICROSOFT;
            let yahoo = provider == PROVIDER_YAHOO;

            widgets.password_row.set_visible(!oauth);
            widgets.username_row.set_visible(!oauth);
            widgets.split_row.set_visible(!oauth);
            let split = widgets.split_row.is_active();
            widgets.smtp_username_row.set_visible(!oauth && split);
            widgets.smtp_password_row.set_visible(!oauth && split);
            if oauth {
                widgets.password_row.set_title("Password");
            } else if split {
                widgets.password_row.set_title(if yahoo {
                    "IMAP app password"
                } else {
                    "IMAP password"
                });
                widgets.smtp_password_row.set_title(if yahoo {
                    "SMTP app password"
                } else {
                    "SMTP password"
                });
                widgets.username_row.set_title("IMAP username");
                widgets.smtp_username_row.set_title("SMTP username");
            } else {
                widgets
                    .password_row
                    .set_title(if yahoo { "App password" } else { "Password" });
                widgets.username_row.set_title("Username");
            }
            widgets.server_group.set_visible(!oauth);
            widgets.oauth_group.set_visible(oauth);
            update_add_sensitive();
        }
    };

    apply_provider_visibility();

    let apply_preset_from_email = {
        let widgets = widgets.clone();
        let state = state.clone();
        let apply_provider_visibility = apply_provider_visibility.clone();
        move |email: &str| {
            let Some(preset) = Account::preset_for_email(email) else {
                clear_autofilled_preset(&widgets, &state);
                widgets.server_expander.set_expanded(email.contains('@'));
                apply_provider_visibility();
                return;
            };

            widgets.imap_host_row.set_text(&preset.imap_host);
            widgets.imap_port_row.set_value(preset.imap_port as f64);
            widgets.smtp_host_row.set_text(&preset.smtp_host);

            let entered = smtp_sync_begin(&state);
            widgets.smtp_port_row.set_value(preset.smtp_port as f64);
            widgets
                .smtp_security_row
                .set_selected(if preset.smtp_starttls {
                    SMTP_STARTTLS
                } else {
                    SMTP_IMPLICIT_TLS
                });
            if entered {
                smtp_sync_end(&state);
            }

            let provider = provider_from_preset(&preset);
            state.borrow_mut().last_preset = Some(AppliedPreset {
                imap_host: preset.imap_host,
                smtp_host: preset.smtp_host,
                provider,
            });
            let user_picked = state.borrow().user_picked_provider;
            set_provider_quietly(
                &widgets,
                &state,
                provider_for_address(user_picked, provider),
            );
            widgets.server_expander.set_expanded(false);
            apply_provider_visibility();
        }
    };

    email_row.connect_changed({
        let apply_preset_from_email = apply_preset_from_email.clone();
        let update_add_sensitive = update_add_sensitive.clone();
        move |row| {
            apply_preset_from_email(&row.text());
            update_add_sensitive();
        }
    });

    password_row.connect_changed({
        let update_add_sensitive = update_add_sensitive.clone();
        move |_| update_add_sensitive()
    });
    smtp_password_row.connect_changed({
        let update_add_sensitive = update_add_sensitive.clone();
        move |_| update_add_sensitive()
    });
    split_row.connect_active_notify({
        let apply_provider_visibility = apply_provider_visibility.clone();
        move |_| apply_provider_visibility()
    });
    imap_host_row.connect_changed({
        let update_add_sensitive = update_add_sensitive.clone();
        move |_| update_add_sensitive()
    });
    smtp_host_row.connect_changed({
        let update_add_sensitive = update_add_sensitive.clone();
        move |_| update_add_sensitive()
    });

    provider_row.connect_notify_local(Some("selected-item"), {
        let state = state.clone();
        let apply_provider_visibility = apply_provider_visibility.clone();
        move |row, _| {
            let syncing = state.borrow().syncing_provider;
            if !syncing {
                state.borrow_mut().user_picked_provider = Some(row.selected());
            }
            apply_provider_visibility()
        }
    });

    smtp_security_row.connect_notify_local(Some("selected-item"), {
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

    let sender_cancel = sender.clone();
    cancel_btn.connect_clicked(move |_| {
        sender_cancel.send(MailMsg::CloseAddAccount).ok();
    });

    let sender_add = sender.clone();
    add_btn.connect_clicked({
        let widgets = widgets.clone();
        let state = state.clone();
        let update_add_sensitive = update_add_sensitive.clone();
        let runtime = runtime.clone();
        move |_| {
            if state.borrow().submitting {
                return;
            }
            state.borrow_mut().submitting = true;
            widgets.add_btn.set_sensitive(false);

            clear_row_errors(&widgets);
            let provider = widgets.provider_row.selected();
            if provider == PROVIDER_GOOGLE || provider == PROVIDER_MICROSOFT {
                let email = widgets.email_row.text().to_string();
                let mut draft = match build_oauth_draft(provider, &email) {
                    Ok(draft) => draft,
                    Err(message) => {
                        apply_validation_error(&widgets, message);
                        show_validation_toast(&widgets.toast_overlay, message);
                        state.borrow_mut().submitting = false;
                        update_add_sensitive();
                        return;
                    }
                };
                apply_identity_from_widgets(&widgets, &mut draft);
                if let Err(message) = draft.validate_oauth_email() {
                    apply_validation_error(&widgets, message);
                    show_validation_toast(&widgets.toast_overlay, message);
                    state.borrow_mut().submitting = false;
                    update_add_sensitive();
                    return;
                }

                let oauth_provider = match provider {
                    PROVIDER_GOOGLE => OAuthProvider::Google,
                    PROVIDER_MICROSOFT => OAuthProvider::Microsoft,
                    _ => unreachable!(),
                };

                let provider_cfg = match oauth_provider_config(&oauth_provider) {
                    Ok(cfg) => cfg,
                    Err(message) => {
                        show_validation_toast(&widgets.toast_overlay, &message);
                        state.borrow_mut().submitting = false;
                        update_add_sensitive();
                        return;
                    }
                };

                show_validation_toast(&widgets.toast_overlay, "Opening browser for sign-in…");

                let sender = sender_add.clone();
                let widgets = widgets.clone();
                let state = state.clone();
                let update_add_sensitive = update_add_sensitive.clone();
                let oauth_generation = {
                    let mut st = state.borrow_mut();
                    st.oauth_generation = st.oauth_generation.wrapping_add(1);
                    st.oauth_generation
                };
                let (tx, rx) = oneshot::channel();
                runtime.spawn(async move {
                    let auth_result = match oauth_provider {
                        OAuthProvider::Google => {
                            google_auth_flow(&provider_cfg, &[GOOGLE_MAIL_SCOPE], launch_auth_url)
                                .await
                        }
                        OAuthProvider::Microsoft => {
                            microsoft_auth_flow(&provider_cfg, &MICROSOFT_SCOPES, launch_auth_url)
                                .await
                        }
                    };
                    let _ = tx.send(auth_result);
                });
                glib::spawn_future_local(async move {
                    let auth_result = rx.await.unwrap_or_else(|_| {
                        Err(Error::Auth("OAuth sign-in was interrupted".to_string()))
                    });

                    let cancelled = {
                        let mut st = state.borrow_mut();
                        let cancelled = st.oauth_generation != oauth_generation;
                        st.submitting = false;
                        cancelled
                    };
                    if cancelled {
                        return;
                    }

                    match auth_result {
                        Ok(tokens) => {
                            draft.oauth_tokens = Some(tokens);
                            if let Err(message) = draft.validate_oauth_auth() {
                                show_validation_toast(&widgets.toast_overlay, message);
                                update_add_sensitive();
                                return;
                            }
                            sender.send(MailMsg::SubmitAddAccount(draft)).ok();
                        }
                        Err(err) => {
                            show_validation_toast(&widgets.toast_overlay, &err.to_string());
                            update_add_sensitive();
                        }
                    }
                });
                return;
            }

            let email = widgets.email_row.text().to_string();
            let (display_name, username) = identity_from_widgets(&widgets, &email);

            let split_credentials = widgets.split_row.is_active();
            let draft = AccountDraft {
                email,
                username,
                display_name,
                imap_host: widgets.imap_host_row.text().to_string(),
                imap_port: widgets.imap_port_row.value() as u16,
                smtp_host: widgets.smtp_host_row.text().to_string(),
                smtp_port: widgets.smtp_port_row.value() as u16,
                smtp_starttls: widgets.smtp_security_row.selected() == SMTP_STARTTLS,
                auth: AuthMethod::Password,
                password: Some(widgets.password_row.text().to_string()),
                oauth_tokens: None,
                split_credentials,
                smtp_username: if split_credentials {
                    widgets.smtp_username_row.text().to_string()
                } else {
                    String::new()
                },
                smtp_password: if split_credentials {
                    Some(widgets.smtp_password_row.text().to_string())
                } else {
                    None
                },
            };

            if let Err(message) = draft.validate_password_auth() {
                apply_validation_error(&widgets, message);
                show_validation_toast(&widgets.toast_overlay, message);
                state.borrow_mut().submitting = false;
                update_add_sensitive();
                return;
            }

            sender_add.send(MailMsg::SubmitAddAccount(draft)).ok();
        }
    });

    let widgets_reset = widgets.clone();
    let state_reset = state.clone();
    let apply_provider_visibility_reset = apply_provider_visibility.clone();
    dialog.connect_closed(move |_| {
        reset_dialog(&widgets_reset, &state_reset);
        apply_provider_visibility_reset();
    });

    dialog
}

/// Open an OAuth authorization URL in the browser; safe to call from any thread.
pub fn launch_auth_url(url: String) {
    // google_auth_flow / microsoft_auth_flow invoke this from a Tokio worker.
    // hop to the GTK main context; idle_add_local_once is main-thread-only.
    glib::MainContext::default().invoke(move || {
        let launcher = gtk::UriLauncher::new(&url);
        launcher.launch(
            None::<&gtk::Window>,
            None::<&gtk::gio::Cancellable>,
            |result| {
                if let Err(err) = result {
                    tracing::warn!(?err, "failed to launch OAuth URI");
                }
            },
        );
    });
}

/// OAuth client settings for `provider`, read from `oauth.toml`.
pub fn oauth_provider_config(provider: &OAuthProvider) -> Result<ProviderOAuthConfig, String> {
    let config = load_oauth_config().map_err(|err| err.to_string())?;
    match provider {
        OAuthProvider::Google => {
            let google = config.google.ok_or_else(|| {
                format!(
                    "Add Google client_id to {}",
                    alcedo_oauth::oauth_config_path_display()
                )
            })?;
            if !google.has_client_secret() {
                return Err(alcedo_oauth::google_client_secret_missing_message());
            }
            Ok(google)
        }
        OAuthProvider::Microsoft => config.microsoft.ok_or_else(|| {
            format!(
                "Add Microsoft client_id to {}",
                alcedo_oauth::oauth_config_path_display()
            )
        }),
    }
}

fn build_oauth_draft(provider: u32, email: &str) -> Result<AccountDraft, &'static str> {
    let oauth_provider = match provider {
        PROVIDER_GOOGLE => OAuthProvider::Google,
        PROVIDER_MICROSOFT => OAuthProvider::Microsoft,
        _ => unreachable!(),
    };

    if let Some(mut preset) = Account::preset_for_email(email) {
        match &preset.auth {
            AuthMethod::OAuth2 { provider: expected } if *expected == oauth_provider => {
                preset.oauth_tokens = None;
                return Ok(preset);
            }
            AuthMethod::OAuth2 {
                provider: OAuthProvider::Google,
            } => return Err("This address uses Google OAuth"),
            AuthMethod::OAuth2 {
                provider: OAuthProvider::Microsoft,
            } => return Err("This address uses Microsoft OAuth"),
            AuthMethod::Password => {
                return Err("Use an app password for this address, not OAuth");
            }
        }
    }

    let display_name = display_name_from_email(email);
    Ok(AccountDraft {
        email: email.to_string(),
        username: email.to_string(),
        display_name,
        imap_host: oauth_provider.imap_host().to_string(),
        imap_port: 993,
        smtp_host: oauth_provider.smtp_host().to_string(),
        smtp_port: match oauth_provider {
            OAuthProvider::Google => 465,
            OAuthProvider::Microsoft => 587,
        },
        smtp_starttls: matches!(oauth_provider, OAuthProvider::Microsoft),
        auth: AuthMethod::OAuth2 {
            provider: oauth_provider,
        },
        password: None,
        oauth_tokens: None,
        split_credentials: false,
        smtp_username: String::new(),
        smtp_password: None,
    })
}

fn provider_from_preset(preset: &AccountDraft) -> u32 {
    match &preset.auth {
        AuthMethod::OAuth2 {
            provider: OAuthProvider::Google,
        } => PROVIDER_GOOGLE,
        AuthMethod::OAuth2 {
            provider: OAuthProvider::Microsoft,
        } => PROVIDER_MICROSOFT,
        AuthMethod::Password if preset.imap_host.contains("yahoo") => PROVIDER_YAHOO,
        AuthMethod::Password => PROVIDER_GENERIC,
    }
}

fn clear_autofilled_preset(widgets: &DialogWidgets, state: &RefCell<DialogState>) {
    let Some(last) = state.borrow_mut().last_preset.take() else {
        return;
    };
    if widgets.imap_host_row.text().as_str() == last.imap_host {
        widgets.imap_host_row.set_text("");
        widgets.imap_port_row.set_value(993.0);
    }
    if widgets.smtp_host_row.text().as_str() == last.smtp_host {
        widgets.smtp_host_row.set_text("");
        let entered = smtp_sync_begin(state);
        widgets.smtp_port_row.set_value(465.0);
        widgets.smtp_security_row.set_selected(SMTP_IMPLICIT_TLS);
        if entered {
            smtp_sync_end(state);
        }
    }
    let user_picked = state.borrow().user_picked_provider.is_some();
    if !user_picked && widgets.provider_row.selected() == last.provider {
        set_provider_quietly(widgets, state, PROVIDER_GENERIC);
    }
}

fn identity_from_widgets(widgets: &DialogWidgets, email: &str) -> (String, String) {
    let name = widgets.name_row.text().trim().to_string();
    let display_name = if name.is_empty() {
        Account::preset_for_email(email)
            .map(|preset| preset.display_name)
            .unwrap_or_else(|| display_name_from_email(email))
    } else {
        name
    };
    let username = login_name_from(&widgets.username_row.text(), email);
    (display_name, username)
}

fn apply_identity_from_widgets(widgets: &DialogWidgets, draft: &mut AccountDraft) {
    let (display_name, username) = identity_from_widgets(widgets, &draft.email);
    draft.display_name = display_name;
    draft.username = username;
}

/// Provider to show for an address: the user's own pick wins over the address preset.
fn provider_for_address(user_picked: Option<u32>, preset_provider: u32) -> u32 {
    user_picked.unwrap_or(preset_provider)
}

/// Set the Provider from code without counting it as a user pick.
fn set_provider_quietly(widgets: &DialogWidgets, state: &RefCell<DialogState>, provider: u32) {
    state.borrow_mut().syncing_provider = true;
    widgets.provider_row.set_selected(provider);
    state.borrow_mut().syncing_provider = false;
}

fn smtp_sync_begin(state: &RefCell<DialogState>) -> bool {
    let mut st = state.borrow_mut();
    if st.syncing_smtp {
        return false;
    }
    st.syncing_smtp = true;
    true
}

fn smtp_sync_end(state: &RefCell<DialogState>) {
    state.borrow_mut().syncing_smtp = false;
}

fn apply_smtp_port_from_security(widgets: &DialogWidgets) {
    let port = widgets.smtp_port_row.value() as u16;
    let new_port = match widgets.smtp_security_row.selected() {
        SMTP_STARTTLS if port == 465 || port == 587 => 587,
        SMTP_IMPLICIT_TLS if port == 465 || port == 587 => 465,
        _ => port,
    };
    widgets.smtp_port_row.set_value(new_port as f64);
}

fn apply_smtp_security_from_port(widgets: &DialogWidgets) {
    match widgets.smtp_port_row.value() as u16 {
        465 => widgets.smtp_security_row.set_selected(SMTP_IMPLICIT_TLS),
        587 => widgets.smtp_security_row.set_selected(SMTP_STARTTLS),
        _ => {}
    }
}

fn show_validation_toast(overlay: &adw::ToastOverlay, message: &str) {
    overlay.add_toast(crate::toast::plain(message));
}

fn clear_row_errors(widgets: &DialogWidgets) {
    for row in [
        widgets.email_row.upcast_ref::<gtk::Widget>(),
        widgets.password_row.upcast_ref::<gtk::Widget>(),
        widgets.smtp_password_row.upcast_ref::<gtk::Widget>(),
        widgets.provider_row.upcast_ref::<gtk::Widget>(),
        widgets.imap_host_row.upcast_ref::<gtk::Widget>(),
        widgets.smtp_host_row.upcast_ref::<gtk::Widget>(),
        widgets.imap_port_row.upcast_ref::<gtk::Widget>(),
        widgets.smtp_port_row.upcast_ref::<gtk::Widget>(),
    ] {
        row.remove_css_class("error");
    }
}

fn apply_validation_error(widgets: &DialogWidgets, message: &str) {
    match message {
        "Email is required" | "Email must contain @" | "Email is invalid" => {
            widgets.email_row.add_css_class("error");
        }
        "Password is required" => widgets.password_row.add_css_class("error"),
        "SMTP password is required" => widgets.smtp_password_row.add_css_class("error"),
        "This address uses Google OAuth"
        | "This address uses Microsoft OAuth"
        | "Use an app password for this address, not OAuth" => {
            widgets.provider_row.add_css_class("error");
        }
        "IMAP host is required" => show_server_error(widgets, &widgets.imap_host_row),
        "SMTP host is required" => show_server_error(widgets, &widgets.smtp_host_row),
        "IMAP port must be between 1 and 65535" => {
            show_server_error(widgets, &widgets.imap_port_row)
        }
        "SMTP port must be between 1 and 65535" => {
            show_server_error(widgets, &widgets.smtp_port_row)
        }
        _ => {}
    }
}

/// Server rows sit in a collapsed expander; open it so the error is visible.
fn show_server_error(widgets: &DialogWidgets, row: &impl IsA<gtk::Widget>) {
    widgets.server_expander.set_expanded(true);
    row.add_css_class("error");
}

fn reset_dialog(widgets: &DialogWidgets, state: &RefCell<DialogState>) {
    {
        let mut st = state.borrow_mut();
        st.submitting = false;
        st.oauth_generation = st.oauth_generation.wrapping_add(1);
        st.last_preset = None;
        st.user_picked_provider = None;
    }
    clear_row_errors(widgets);
    widgets.email_row.set_text("");
    widgets.name_row.set_text("");
    widgets.username_row.set_text("");
    widgets.password_row.set_text("");
    widgets.split_row.set_active(false);
    widgets.smtp_username_row.set_text("");
    widgets.smtp_password_row.set_text("");
    set_provider_quietly(widgets, state, PROVIDER_GENERIC);
    widgets.imap_host_row.set_text("");
    widgets.imap_port_row.set_value(993.0);
    widgets.smtp_host_row.set_text("");
    widgets.smtp_port_row.set_value(465.0);
    widgets.smtp_security_row.set_selected(SMTP_IMPLICIT_TLS);
    widgets.password_row.set_title("Password");
    widgets.server_group.set_visible(true);
    widgets.server_expander.set_expanded(false);
    widgets.oauth_group.set_visible(false);
    widgets.add_btn.set_sensitive(false);
    widgets.oauth_spinner.set_visible(false);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_keeps_provider_the_user_picked() {
        // Gmail's preset is Google (OAuth); a user who picked Other email keeps it.
        assert_eq!(
            provider_for_address(Some(PROVIDER_GENERIC), PROVIDER_GOOGLE),
            PROVIDER_GENERIC
        );
    }

    #[test]
    fn address_picks_provider_until_the_user_chooses() {
        assert_eq!(provider_for_address(None, PROVIDER_GOOGLE), PROVIDER_GOOGLE);
    }

    #[test]
    fn google_email_uses_gmail_hosts() {
        let draft = build_oauth_draft(PROVIDER_GOOGLE, "user@gmail.com").expect("google draft");
        assert_eq!(draft.imap_host, "imap.gmail.com");
        assert_eq!(draft.username, "user@gmail.com");
        assert!(matches!(
            draft.auth,
            AuthMethod::OAuth2 {
                provider: OAuthProvider::Google
            }
        ));
    }

    #[test]
    fn microsoft_oauth_with_gmail_address_is_rejected() {
        let err = build_oauth_draft(PROVIDER_MICROSOFT, "user@gmail.com").expect_err("mismatch");
        assert_eq!(err, "This address uses Google OAuth");
    }

    #[test]
    fn google_oauth_with_outlook_address_is_rejected() {
        let err = build_oauth_draft(PROVIDER_GOOGLE, "user@outlook.com").expect_err("mismatch");
        assert_eq!(err, "This address uses Microsoft OAuth");
    }

    #[test]
    fn yahoo_address_rejects_oauth() {
        let err = build_oauth_draft(PROVIDER_GOOGLE, "user@yahoo.com").expect_err("yahoo");
        assert_eq!(err, "Use an app password for this address, not OAuth");
    }

    #[test]
    fn custom_domain_follows_selected_provider() {
        let draft = build_oauth_draft(PROVIDER_GOOGLE, "user@company.example").expect("workspace");
        assert_eq!(draft.imap_host, "imap.gmail.com");
        let draft = build_oauth_draft(PROVIDER_MICROSOFT, "user@company.example").expect("m365");
        assert_eq!(draft.imap_host, "outlook.office365.com");
    }
}
