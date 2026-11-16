//! Account add, edit, removal, re-auth, PGP and identity glue.

use super::*;

impl MailModel {
    pub(super) fn update_accounts(&mut self, msg: MailMsg, sender: ComponentSender<MailModel>) {
        match msg {
            MailMsg::SelectAccount(account_id) => {
                if self.is_search_backed(self.mail_view) {
                    self.make_account_current(account_id, &sender);
                    self.rebuild_folder_bar(&sender);
                    return;
                }
                self.make_account_current(account_id, &sender);

                let folders = self.folders.get(&account_id).cloned().unwrap_or_default();
                if let Some(folder_id) = find_default_folder(&folders) {
                    sender.input(MailMsg::SelectFolder(folder_id));
                } else {
                    self.pending_account_folder_select = Some(account_id);
                    self.rebuild_folder_bar(&sender);
                }
            }
            MailMsg::AddAccount => {
                self.add_account_dialog.present(Some(&self.window));
            }
            MailMsg::CloseAddAccount => {
                self.add_account_dialog.close();
            }
            MailMsg::SubmitAddAccount(draft) => {
                if self.mock {
                    tracing::info!(
                        email = %draft.email,
                        imap_host = %draft.imap_host,
                        smtp_host = %draft.smtp_host,
                        "Add account (mock — not persisted)"
                    );
                } else {
                    self.pending_account_add_email = Some(draft.email.clone());
                    self.cmd.emit(UiCommand::AddAccount(draft));
                }
                self.add_account_dialog.close();
            }
            MailMsg::UpdateAccount { id, update } => {
                if self.mock {
                    let Some(account) = self.accounts.iter_mut().find(|account| account.id == id)
                    else {
                        return;
                    };
                    if let Err(message) = apply_settings_update_to_account(account, &update) {
                        self.preferences_ui()
                            .dialog
                            .add_toast(crate::toast::plain(message));
                        return;
                    }
                    let saved = account.clone();
                    refresh_account_rows(
                        &self.preferences_ui().accounts,
                        &self.accounts,
                        &self.preferences_ui().dialog,
                        sender.input_sender(),
                        &self.runtime,
                        &self.auth_required,
                        &self.signatures,
                    );
                    mark_account_saved(&self.preferences_ui().accounts, &saved);
                    self.sync_account_switcher(&sender);
                    self.rebuild_folder_bar(&sender);
                    self.preferences_ui()
                        .dialog
                        .add_toast(crate::toast::plain("Account saved"));
                    self.push_activity(
                        activity_log::KIND_ACCOUNT,
                        ActivityOutcome::Success,
                        "Account saved",
                        "",
                        &saved.email,
                        "",
                    );
                } else {
                    self.cmd.emit(UiCommand::UpdateAccount { id, update });
                }
            }
            MailMsg::ReconnectBrokenAccount => {
                let Some(account_id) = self.primary_broken_account_id() else {
                    return;
                };
                let Some(account) = self
                    .accounts
                    .iter()
                    .find(|account| account.id == account_id)
                    .cloned()
                else {
                    return;
                };
                match &account.auth {
                    AuthMethod::Password => {
                        self.open_account_editor(&account, &sender);
                        if self.preferences.is_some() {
                            focus_open_account_password(&self.preferences_ui().accounts);
                        }
                    }
                    AuthMethod::OAuth2 { .. } => {
                        if self.oauth_reauth_in_flight {
                            return;
                        }
                        self.oauth_reauth_in_flight = true;
                        self.refresh_auth_chrome(&sender);
                        self.toasts
                            .add_toast(crate::toast::plain("Opening browser for sign-in…"));
                        let input = sender.input_sender().clone();
                        let account = account.clone();
                        let runtime = self.runtime.clone();
                        let account_for_oauth = account.clone();
                        let (tx, rx) = oneshot::channel();
                        runtime.spawn(async move {
                            let _ = tx.send(mail_oauth_pkce(&account_for_oauth).await);
                        });
                        glib::spawn_future_local(async move {
                            match rx.await.unwrap_or_else(|_| {
                                Err(alcedo_core::Error::Auth(
                                    "OAuth sign-in was interrupted".to_string(),
                                ))
                            }) {
                                Ok(tokens) => {
                                    let update = AccountSettingsUpdate {
                                        email: account.email.clone(),
                                        display_name: account.display_name.clone(),
                                        sender_name: account.sender_name.clone(),
                                        avatar_text: account.avatar_text.clone(),
                                        accent_color: account.accent_color.clone(),
                                        avatar_image: account.avatar_image.clone(),
                                        username: account.username.clone(),
                                        imap_host: account.imap_host.clone(),
                                        imap_port: account.imap_port,
                                        smtp_host: account.smtp_host.clone(),
                                        smtp_port: account.smtp_port,
                                        smtp_starttls: account.smtp_starttls,
                                        password: None,
                                        oauth_tokens: Some(tokens),
                                        split_credentials: false,
                                        smtp_username: String::new(),
                                        smtp_password: None,
                                    };
                                    let _ = input.send(MailMsg::UpdateAccount {
                                        id: account_id,
                                        update,
                                    });
                                }
                                Err(err) => {
                                    let _ = input.send(MailMsg::OAuthReauthFailed(err.to_string()));
                                }
                            }
                            let _ = input.send(MailMsg::OAuthReauthFinished);
                        });
                    }
                }
            }
            MailMsg::OAuthReauthFinished => {
                self.oauth_reauth_in_flight = false;
                self.refresh_auth_chrome(&sender);
            }
            MailMsg::OAuthReauthFailed(message) => {
                self.toasts.add_toast(crate::toast::plain(&message));
            }
            MailMsg::RemoveAccount(account_id) => {
                if self.mock {
                    let email = self
                        .accounts
                        .iter()
                        .find(|account| account.id == account_id)
                        .map(|account| account.email.clone())
                        .unwrap_or_default();
                    self.finish_account_removal(account_id, &sender);
                    if self.preferences_ui().dialog.is_mapped() {
                        self.preferences_ui()
                            .dialog
                            .add_toast(crate::toast::plain("Account removed"));
                    } else {
                        self.toasts
                            .add_toast(crate::toast::plain("Account removed"));
                    }
                    self.push_activity(
                        activity_log::KIND_ACCOUNT,
                        ActivityOutcome::Success,
                        "Account removed",
                        "",
                        email,
                        "",
                    );
                } else {
                    self.cmd.emit(UiCommand::RemoveAccount(account_id));
                }
            }
            MailMsg::PgpGenerate {
                account,
                passphrase,
            } => {
                self.cmd.emit(UiCommand::PgpGenerate {
                    account,
                    passphrase,
                });
            }
            MailMsg::PgpImport {
                account,
                armored,
                passphrase,
            } => {
                self.cmd.emit(UiCommand::PgpImport {
                    account,
                    armored,
                    passphrase,
                });
            }
            MailMsg::PgpExportPublic(account) => {
                self.cmd.emit(UiCommand::PgpExportPublic { account });
            }
            MailMsg::PgpDeleteKey(account) => {
                self.cmd.emit(UiCommand::PgpDeleteKey { account });
            }
            MailMsg::PgpSetSignOutgoing { account, enabled } => {
                self.cmd
                    .emit(UiCommand::PgpSetSignOutgoing { account, enabled });
            }
            MailMsg::PgpAcceptPeer {
                account,
                email,
                armored,
            } => {
                self.autocrypt_by_message
                    .retain(|_, (_, offer_email, _)| offer_email != &email);
                self.cmd.emit(UiCommand::PgpAcceptPeer {
                    account,
                    email,
                    armored,
                });
            }
            MailMsg::RefreshAccountPgp(account) => {
                let state = self.pgp_by_account.get(&account);
                refresh_pgp_subpage(&self.preferences_ui().accounts, state);
                self.cmd.emit(UiCommand::PgpRefreshState { account });
            }
            MailMsg::PaintAccountIdentities(account_id) => {
                if open_account_id(&self.preferences_ui().accounts) == Some(account_id) {
                    let identities = self
                        .identities
                        .get(&account_id)
                        .map(|values| values.as_slice())
                        .unwrap_or(&[]);
                    refresh_identities_subpage(
                        &self.preferences_ui().accounts,
                        identities,
                        sender.input_sender(),
                    );
                    refresh_catch_all_subpage(
                        &self.preferences_ui().accounts,
                        account_id,
                        self.catch_all_domains
                            .get(&account_id)
                            .map(Vec::as_slice)
                            .unwrap_or(&[]),
                    );
                }
            }
            MailMsg::SetSmtpEnvelopePolicy {
                account,
                same_domain_identity,
            } => {
                self.cmd.emit(UiCommand::SetSmtpEnvelopePolicy {
                    account,
                    same_domain_identity,
                });
            }
            MailMsg::SetCatchAllDomains { account, text } => {
                self.cmd.emit(UiCommand::SetCatchAllDomains {
                    account,
                    domains: alcedo_core::parse_catch_all_domains(&text),
                });
            }
            MailMsg::RefreshSmtpEnvelopePolicy(account) => {
                self.cmd
                    .emit(UiCommand::RefreshSmtpEnvelopePolicy { account });
            }
            MailMsg::UpsertSignature(signature) => {
                self.cmd.emit(UiCommand::UpsertSignature(signature));
            }
            MailMsg::UpsertIdentity(identity) => {
                self.cmd.emit(UiCommand::UpsertIdentity(identity));
            }
            MailMsg::DeleteIdentity(id) => {
                self.cmd.emit(UiCommand::DeleteIdentity { id });
            }
            MailMsg::UpsertSnippet(snippet) => {
                self.cmd.emit(UiCommand::UpsertSnippet(snippet));
            }
            MailMsg::DeleteSnippet(id) => {
                self.cmd.emit(UiCommand::DeleteSnippet(id));
            }
            _ => unreachable!("message not routed to accounts"),
        }
    }

    pub(super) fn should_toast_background_error(
        &self,
        account: Option<AccountId>,
        message: &str,
    ) -> bool {
        let already = self.network_error_toasted.contains(&account);
        let available = gio::NetworkMonitor::default().is_network_available();
        should_toast_sync_failure(message, available, self.started_at.elapsed(), already)
    }

    pub(super) fn show_account_added_toast(
        &self,
        email: &str,
        account_id: AccountId,
        sender: &ComponentSender<MailModel>,
    ) {
        if self.accounts.len() < 2 {
            return;
        }
        let title = account_added_toast_title(email);
        self.record(
            activity_log::KIND_ACCOUNT,
            ActivityOutcome::Success,
            title.clone(),
            "",
        );
        let toast = crate::toast::plain(&title);
        toast.set_timeout(8);
        toast.set_button_label(Some("Open inbox"));
        let sender = sender.clone();
        toast.connect_button_clicked(move |_| {
            sender.input(MailMsg::SelectAccount(account_id));
        });
        if self.preferences_ui().dialog.is_mapped() {
            self.preferences_ui().dialog.add_toast(toast);
        } else {
            self.toasts.add_toast(toast);
        }
    }

    pub(super) fn finish_account_removal(
        &mut self,
        account_id: AccountId,
        sender: &ComponentSender<MailModel>,
    ) {
        let was_selected_account = self.selected_account == Some(account_id);
        let viewing_account_folder = matches!(
            self.mail_view,
            MailView::Folder(folder_id) if self.folder_belongs_to_account(folder_id, account_id)
        );

        self.accounts.retain(|acct| acct.id != account_id);
        self.folders.remove(&account_id);
        self.collapsed_folders.remove(&account_id);
        set_collapsed_folders(&self.collapsed_folders);
        if self.collapsed_accounts.remove(&account_id) {
            set_collapsed_accounts(&self.collapsed_accounts);
        }
        if self.collapsed_personal_folders.remove(&account_id) {
            set_collapsed_personal_folders(&self.collapsed_personal_folders);
        }
        if self.hidden_accounts.remove(&account_id) {
            set_hidden_accounts(&self.hidden_accounts);
            if self.hidden_accounts.is_empty() {
                self.hidden_accounts_expanded = false;
            }
        }
        if self.account_order.contains(&account_id) {
            self.account_order.retain(|id| *id != account_id);
            set_account_order(&self.account_order);
        }
        if self.pending_account_folder_select == Some(account_id) {
            self.pending_account_folder_select = None;
        }
        if self
            .pending_restore
            .as_ref()
            .is_some_and(|(id, _)| *id == account_id)
        {
            self.pending_restore = None;
        }
        if was_selected_account {
            self.selected_account = None;
            self.persist_selected_account();
        }
        self.unread_by_account.remove(&account_id);
        self.sync_account_switcher(sender);
        self.refresh_unread_badges(sender);
        self.emit_unified_unread(sender);

        let selected_from_account = self.selected.is_some_and(|id| {
            self.envelopes
                .iter()
                .find(|env| env.id == id)
                .is_some_and(|env| env.account_id == account_id)
        });
        self.envelopes.retain(|env| env.account_id != account_id);
        self.source_envelopes
            .retain(|env| env.account_id != account_id);
        if selected_from_account {
            self.selected = None;
            clear_reader(&self.reader);
        }
        self.refresh_list();

        refresh_account_rows(
            &self.preferences_ui().accounts,
            &self.accounts,
            &self.preferences_ui().dialog,
            sender.input_sender(),
            &self.runtime,
            &self.auth_required,
            &self.signatures,
        );
        if open_account_id(&self.preferences_ui().accounts) == Some(account_id) {
            self.preferences_ui().dialog.pop_subpage();
        }

        if was_selected_account || viewing_account_folder {
            sender.input(MailMsg::SelectUnifiedInbox);
        }

        self.rebuild_folder_bar(sender);
        self.refresh_all_composer_from();
    }

    pub(super) fn abandon_pending_restore_if_account_gone(&mut self) {
        let Some((restore_account, _)) = self.pending_restore else {
            return;
        };
        let known: Vec<AccountId> = self.accounts.iter().map(|account| account.id).collect();
        if should_abandon_pending_restore(restore_account, &known) {
            self.pending_restore = None;
            set_last_folder(&LastFolder::UnifiedInbox);
        }
    }

    pub(super) fn mail_auth_label(auth: &AuthMethod) -> &'static str {
        match auth {
            AuthMethod::Password => "password",
            AuthMethod::OAuth2 { .. } => "oauth",
        }
    }

    pub(super) fn account_debug(&self, account: &Account, raw: &str) -> String {
        activity_log::format_account_debug(
            &account.email,
            &account.imap_host,
            account.imap_port,
            &account.smtp_host,
            account.smtp_port,
            Self::mail_auth_label(&account.auth),
            raw,
        )
    }

    pub(super) fn record_account_status(&mut self, account_id: AccountId, raw: &str) {
        let previous = self.last_logged_status.get(&account_id).cloned();
        self.last_logged_status.insert(account_id, raw.to_string());
        if !activity_log::should_log_status(previous.as_deref(), raw) {
            return;
        }

        let account = self.accounts.iter().find(|acct| acct.id == account_id);
        let account_label = account.map(|acct| acct.email.as_str()).unwrap_or("Account");
        let title = crate::status_bar::format_sync_status(Some(raw));
        let outcome = if raw.eq_ignore_ascii_case("IDLE") || raw.eq_ignore_ascii_case("Polling") {
            ActivityOutcome::Success
        } else {
            ActivityOutcome::Info
        };
        let debug = account
            .map(|acct| self.account_debug(acct, raw))
            .unwrap_or_else(|| format!("status: {raw}"));
        self.push_activity(
            activity_log::KIND_SYNC,
            outcome,
            title,
            "",
            account_label,
            debug,
        );
    }

    pub(super) fn account_email(&self, account_id: AccountId) -> String {
        self.accounts
            .iter()
            .find(|account| account.id == account_id)
            .map(|account| account.email.clone())
            .unwrap_or_default()
    }

    pub(super) fn make_account_current(
        &mut self,
        account_id: AccountId,
        sender: &ComponentSender<MailModel>,
    ) {
        self.selected_account = Some(account_id);
        self.persist_selected_account();
        self.sync_account_switcher(sender);
        self.sync_inline_composer_from_account(account_id);
    }

    pub(super) fn open_account_editor(
        &self,
        account: &Account,
        sender: &ComponentSender<MailModel>,
    ) {
        self.msg_sender
            .emit(crate::app::AppMsg::OpenAccountsPreferences);
        if self.preferences.is_none() {
            return;
        }
        let page = &self.preferences_ui().accounts;
        if open_account_id(page) == Some(account.id) {
            return;
        }
        if open_account_id(page).is_some() {
            self.preferences_ui().dialog.pop_subpage();
        }
        present_account_editor(
            &self.preferences_ui().dialog,
            page,
            account,
            sender.input_sender(),
            &self.runtime,
            self.signatures.get(&account.id),
        );
    }

    pub(super) fn sync_account_switcher(&mut self, sender: &ComponentSender<MailModel>) {
        let ordered: Vec<Account> = self.ordered_accounts().into_iter().cloned().collect();
        refresh_account_switcher(
            &mut self.account_switcher,
            &ordered,
            self.selected_account,
            &self.unread_by_account,
            &self.auth_required,
            {
                let sender = sender.clone();
                move |account_id| sender.input(MailMsg::SelectAccount(account_id))
            },
            {
                let sender = sender.clone();
                move || sender.input(MailMsg::AddAccount)
            },
        );
        self.account_switcher
            .button
            .set_visible(!self.stacked_accounts_active());
        self.paint_auth_banner();
    }

    pub(super) fn primary_broken_account_id(&self) -> Option<AccountId> {
        if let Some(selected) = self.selected_account
            && self.auth_required.contains_key(&selected)
        {
            return Some(selected);
        }
        self.accounts
            .iter()
            .find(|account| self.auth_required.contains_key(&account.id))
            .map(|account| account.id)
    }

    pub(super) fn offline_account_names(&self) -> Vec<String> {
        self.accounts
            .iter()
            .filter(|account| self.auth_required.contains_key(&account.id))
            .map(|account| account.label().to_string())
            .collect()
    }

    fn offline_keyring_is_locked(&self) -> bool {
        !self.auth_required.is_empty()
            && self
                .auth_required
                .values()
                .all(|message| is_keyring_auth_message(message))
    }

    fn offline_banner_title_text(&self, keyring_locked: bool) -> String {
        let names = self.offline_account_names();
        if names.is_empty() {
            return unnamed_offline_banner_title(self.auth_required.len(), keyring_locked);
        }
        let list_hint = if keyring_locked {
            MAIL_KEYRING_LIST_HINT
        } else {
            MAIL_SIGN_IN_LIST_HINT
        };
        offline_banner_title(&names, "account", keyring_locked, list_hint)
    }

    /// Banner button text for the account it will act on, or `None` when
    /// nothing the person can click would help (the keyring must be unlocked).
    fn sign_in_banner_button_label(&self, keyring_locked: bool) -> Option<String> {
        if keyring_locked {
            return None;
        }
        let account = self
            .primary_broken_account_id()
            .and_then(|id| self.accounts.iter().find(|account| account.id == id))?;
        if self.oauth_reauth_in_flight {
            return Some("Waiting for browser…".to_string());
        }
        let step = match account.auth {
            AuthMethod::OAuth2 { .. } => SignInStep::Browser,
            AuthMethod::Password => SignInStep::Password,
        };
        Some(sign_in_button_label(
            step,
            self.offline_account_names().len(),
            account.label(),
        ))
    }

    fn paint_auth_banner(&self) {
        if self.auth_required.is_empty() {
            self.auth_banner.set_revealed(false);
            return;
        }
        let keyring_locked = self.offline_keyring_is_locked();
        self.auth_banner
            .set_title(&self.offline_banner_title_text(keyring_locked));
        self.auth_banner
            .set_button_label(self.sign_in_banner_button_label(keyring_locked).as_deref());
        self.auth_banner.set_revealed(true);
    }

    pub(super) fn refresh_auth_chrome(&mut self, sender: &ComponentSender<MailModel>) {
        self.paint_auth_banner();
        self.sync_account_switcher(sender);
        self.rebuild_folder_bar(sender);
        if self.preferences.is_some() {
            refresh_account_rows(
                &self.preferences_ui().accounts,
                &self.accounts,
                &self.preferences_ui().dialog,
                sender.input_sender(),
                &self.runtime,
                &self.auth_required,
                &self.signatures,
            );
        }
        self.refresh_status_bar();
    }
}
