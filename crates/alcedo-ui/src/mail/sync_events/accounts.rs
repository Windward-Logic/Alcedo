//! Account, folder-tree, auth, status, rules, PGP and identity updates from sync.

use super::*;

impl MailModel {
    pub(super) fn on_accounts_sync_event(
        &mut self,
        event: SyncEvent,
        sender: &ComponentSender<MailModel>,
    ) {
        match event {
            SyncEvent::AccountAdded(account) => {
                self.accounts.push(account.clone());
                self.notify_own_addresses();
                if self
                    .pending_restore
                    .as_ref()
                    .is_some_and(|(id, _)| *id == account.id)
                    || (self.pending_restore.is_none() && self.selected_account.is_none())
                {
                    self.selected_account = Some(account.id);
                }
                self.sync_account_switcher(sender);
                self.refresh_all_composer_from();
                // Startup also emits AccountAdded for every saved account; only toast/log
                // when this matches a user Add Account submit in this session.
                let user_added = self
                    .pending_account_add_email
                    .as_ref()
                    .is_some_and(|email| email.eq_ignore_ascii_case(&account.email));
                if user_added {
                    self.pending_account_add_email = None;
                    if self.accounts.len() >= 2 {
                        self.show_account_added_toast(&account.email, account.id, sender);
                    }
                }
                self.refresh_unread_badges(sender);
                self.rebuild_folder_bar(sender);
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
            SyncEvent::FoldersUpdated { account, folders } => {
                self.folders.insert(account, folders.clone());
                *self.search_complete_folders.borrow_mut() = self.command_palette_spec().folders;
                if let Some((restore_account, imap_name)) = self.pending_restore.clone()
                    && restore_account == account
                {
                    match match_pending_folder_restore(&folders, &imap_name) {
                        FolderRestoreMatch::Found(folder_id) => {
                            self.pending_restore = None;
                            self.selected_account = Some(account);
                            sender.input(MailMsg::SelectFolder(folder_id));
                            return;
                        }
                        FolderRestoreMatch::Wait => {}
                    }
                }
                if self.selected_account.is_none() {
                    self.selected_account = Some(account);
                } else if self.selected_account != Some(account) {
                    if self.stacked_accounts_active() {
                        self.rebuild_folder_bar(sender);
                    }
                    return;
                }
                if self.pending_account_folder_select == Some(account)
                    && let Some(folder_id) = find_default_folder(&folders)
                {
                    sender.input(MailMsg::SelectFolder(folder_id));
                    return;
                }
                if let Some((pending_account, imap_name)) = &self.pending_select_imap
                    && *pending_account == account
                    && let Some(folder) =
                        folders.iter().find(|folder| folder.imap_name == *imap_name)
                {
                    self.pending_select_imap = None;
                    sender.input(MailMsg::SelectFolder(folder.id));
                    return;
                }
                if let MailView::Folder(folder_id) = self.mail_view {
                    let folder_missing = !folders.iter().any(|folder| folder.id == folder_id);
                    if folder_missing && let Some(inbox_id) = find_default_folder(&folders) {
                        sender.input(MailMsg::SelectFolder(inbox_id));
                        return;
                    }
                }
                self.rebuild_folder_bar(sender);
            }
            SyncEvent::AccountUpdated(account) => {
                self.auth_required.remove(&account.id);
                if let Some(existing) = self.accounts.iter_mut().find(|acct| acct.id == account.id)
                {
                    *existing = account.clone();
                }
                self.notify_own_addresses();
                self.sync_account_switcher(sender);
                self.refresh_all_composer_from();
                refresh_account_rows(
                    &self.preferences_ui().accounts,
                    &self.accounts,
                    &self.preferences_ui().dialog,
                    sender.input_sender(),
                    &self.runtime,
                    &self.auth_required,
                    &self.signatures,
                );
                mark_account_saved(&self.preferences_ui().accounts, &account);
                self.preferences_ui()
                    .dialog
                    .add_toast(crate::toast::plain("Account saved"));
                self.push_activity(
                    activity_log::KIND_ACCOUNT,
                    ActivityOutcome::Success,
                    "Account saved",
                    "",
                    &account.email,
                    "",
                );
                self.refresh_auth_chrome(sender);
            }
            SyncEvent::AccountRemoved(account_id) => {
                let email = self
                    .accounts
                    .iter()
                    .find(|account| account.id == account_id)
                    .map(|account| account.email.clone())
                    .unwrap_or_default();
                self.auth_required.remove(&account_id);
                self.finish_account_removal(account_id, sender);
                self.notify_own_addresses();
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
                self.refresh_auth_chrome(sender);
            }
            SyncEvent::AuthRequired { account, message } => {
                if self
                    .auth_required
                    .insert(account, message.clone())
                    .is_none()
                {
                    let resolved = self.accounts.iter().find(|acct| acct.id == account);
                    let email = resolved
                        .map(|acct| acct.email.as_str())
                        .unwrap_or("Account");
                    let toast = if is_keyring_auth_message(&message) {
                        format!("{email} is offline until the keyring is unlocked")
                    } else {
                        format!("{email} is offline — reconnect required")
                    };
                    self.toasts.add_toast(crate::toast::plain(&toast));
                    let debug = resolved
                        .map(|acct| self.account_debug(acct, &message))
                        .unwrap_or_else(|| format!("status: {message}"));
                    self.push_activity(
                        activity_log::KIND_SYNC,
                        ActivityOutcome::Failure,
                        &toast,
                        "",
                        email,
                        debug,
                    );
                }
                self.refresh_auth_chrome(sender);
            }
            SyncEvent::Error { account, message } => {
                if let Some(folder_id) = self.pending_delete_folder {
                    let pending_account = self.folder_for_id(folder_id).map(|f| f.account_id);
                    let for_pending = account.is_none()
                        || pending_account.is_none()
                        || pending_account == account;
                    if for_pending {
                        self.dismiss_pending_delete_toast();
                        self.pending_delete_folder = None;
                        self.pending_delete_through = None;
                        if self.context_sync_target.is_some() {
                            self.context_sync_target = None;
                            self.rebuild_folder_bar(sender);
                        }
                    }
                }
                let should_toast = self.should_toast_background_error(account, &message);
                let is_network = looks_like_network_failure(&message);
                let redacted = activity_log::redact_url_userinfo(&message);
                let resolved =
                    account.and_then(|id| self.accounts.iter().find(|acct| acct.id == id));
                let toast_text = resolved
                    .map(|acct| format!("{}: {redacted}", acct.email))
                    .unwrap_or_else(|| redacted.clone());
                let debug = resolved
                    .map(|acct| self.account_debug(acct, &redacted))
                    .unwrap_or_else(|| format!("status: {redacted}"));
                self.push_activity(
                    activity_log::KIND_SYNC,
                    ActivityOutcome::Failure,
                    &redacted,
                    "",
                    resolved.map(|acct| acct.email.as_str()).unwrap_or(""),
                    debug,
                );
                if should_toast {
                    self.toasts.add_toast(crate::toast::plain(&toast_text));
                    if is_network {
                        self.network_error_toasted.insert(account);
                    }
                }
                self.pending_raw_action = None;
                let error_for_current = account.is_none() || account == self.selected_account;
                let cleared_folder = error_for_current && self.pending_folder_sync.take().is_some();
                let cleared_virtual =
                    error_for_current && self.pending_virtual_sync.take().is_some();
                if cleared_folder || cleared_virtual {
                    self.refresh_status_bar();
                    self.rebuild_folder_bar(sender);
                    self.refresh_list();
                }
            }
            SyncEvent::Status { account, text } => {
                let trimmed = text.trim();
                if trimmed.eq_ignore_ascii_case("IDLE") || trimmed.eq_ignore_ascii_case("Polling") {
                    if self.auth_required.remove(&account).is_some() {
                        self.refresh_auth_chrome(sender);
                    }
                    self.network_error_toasted.remove(&Some(account));
                }
                self.record_account_status(account, &text);
                if self.selected_account.is_none() || self.selected_account == Some(account) {
                    self.set_sync_activity(Some(&text), sender);
                }
            }
            SyncEvent::ContactsChanged { contacts, .. } => {
                for extension in crate::extension::registered() {
                    extension.contacts_changed(&contacts);
                }
                self.notify_mail_changed();
                self.rebuild_folder_bar(sender);
            }
            SyncEvent::SentRecipientsChanged { addresses } => {
                *self.sent_recipients.borrow_mut() = addresses;
            }
            SyncEvent::RulesUpdated {
                account,
                rules,
                server_rules,
                preview,
            } => {
                self.rules.insert(account, rules);
                self.server_rules.insert(account, server_rules);
                self.preview.insert(account, preview);
                if self
                    .rules_dialog
                    .as_ref()
                    .is_some_and(|h| h.account == account)
                {
                    self.refresh_rules_dialog(account, sender);
                }
            }
            SyncEvent::PgpState(state) => {
                self.pgp_by_account.insert(state.account_id, state.clone());
                if open_account_id(&self.preferences_ui().accounts) == Some(state.account_id) {
                    refresh_pgp_subpage(&self.preferences_ui().accounts, Some(&state));
                }
                if self.mail_content_pane == MailContentPane::Composer {
                    let account_id = self.composer.from_account_id();
                    if account_id == state.account_id {
                        *self.composer.pgp_state.borrow_mut() = Some(state.clone());
                        refresh_pgp_disclosure(&self.composer, Some(&state));
                    }
                }
                for window in self.composer_windows.values() {
                    if window.account_id() == state.account_id {
                        *window.widgets.pgp_state.borrow_mut() = Some(state.clone());
                        refresh_pgp_disclosure(&window.widgets, Some(&state));
                    }
                }
            }
            SyncEvent::PgpPublicKey {
                account: _,
                armored,
                fingerprint,
            } => {
                notifications::copy_text_to_clipboard(&armored);
                let grouped = alcedo_core::format_fingerprint_display(&fingerprint);
                self.toasts.add_toast(crate::toast::plain(&format!(
                    "Public key copied ({grouped})"
                )));
            }
            SyncEvent::SignaturesUpdated { account, signature } => {
                if let Some(sig) = signature {
                    self.signatures.insert(account, sig);
                } else {
                    self.signatures.remove(&account);
                }
                if let Some(open) = open_account_id(&self.preferences_ui().accounts)
                    && open == account
                {
                    refresh_signature_subpage(
                        &self.preferences_ui().accounts,
                        self.signatures.get(&account),
                    );
                }
            }
            SyncEvent::IdentitiesUpdated {
                account,
                identities,
            } => {
                self.identities.insert(account, identities.clone());
                self.notify_own_addresses();
                self.refresh_all_composer_from();
                if let Some(open) = open_account_id(&self.preferences_ui().accounts)
                    && open == account
                {
                    refresh_identities_subpage(
                        &self.preferences_ui().accounts,
                        &identities,
                        sender.input_sender(),
                    );
                }
            }
            SyncEvent::SmtpEnvelopePolicy { account, policy } => {
                refresh_envelope_policy_subpage(&self.preferences_ui().accounts, account, policy);
            }
            SyncEvent::CatchAllDomainsUpdated { account, domains } => {
                refresh_catch_all_subpage(&self.preferences_ui().accounts, account, &domains);
                self.catch_all_domains.insert(account, domains);
            }
            SyncEvent::SnippetsUpdated { snippets } => {
                self.snippets = snippets.clone();
                *self.compose_hooks.snippets.borrow_mut() = snippets.clone();
                self.composer.toolbar.refresh_snippets(&snippets);
                for window in self.composer_windows.values() {
                    window.widgets.toolbar.refresh_snippets(&snippets);
                }
                if let Some(prefs) = &self.preferences {
                    refresh_snippets_prefs(&prefs.snippets, &snippets, sender.input_sender());
                }
            }
            _ => unreachable!("sync event not routed to accounts"),
        }
    }
}
