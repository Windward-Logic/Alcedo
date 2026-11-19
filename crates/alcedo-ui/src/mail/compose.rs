//! Inline and detached composer lifecycle, From binding, signatures and snippets.

use super::drafts::imap_draft_ref_matches_envelope;
use super::list_actions::adjacent_after_removed;
use super::send::send_from_mismatches_selected_account;
use super::*;
use alcedo_i18n::{tr, trf, trs};

impl MailModel {
    pub(super) fn update_compose(&mut self, msg: MailMsg, sender: ComponentSender<MailModel>) {
        match msg {
            MailMsg::Compose => {
                if self.mail_content_pane == MailContentPane::Composer {
                    if let Some(account) = self.compose_account() {
                        self.sync_inline_composer_from_account(account.id);
                    }
                    present_main_window();
                    focus_composer_to(&self.composer);
                    return;
                }
                if let Some(account) = self.compose_account() {
                    let account_id = account.id;
                    let mut draft = ComposeDraft {
                        account_id,
                        to: Vec::new(),
                        cc: Vec::new(),
                        bcc: Vec::new(),
                        subject: String::new(),
                        body_text: String::new(),
                        format: compose_format_for_account(account_id),
                        request_mdn: request_mdn(),
                        tracking_pixel_url: effective_tracking_pixel_url(),
                        attachments: Vec::new(),
                        inlines: Vec::new(),
                        collect_outgoing_contacts: crate::settings::collect_outgoing_contacts(),
                        in_reply_to: None,
                        references: None,
                        thread_id: None,
                        approve_contact_email: None,
                        sign: false,
                        encrypt: false,
                        autocrypt_header: None,
                        identity_id: None,
                        custom_from: None,
                        imap_draft: None,
                        reaction_emoji: None,
                    };
                    draft.body_text = apply_signature(
                        &draft.body_text,
                        draft.format,
                        self.signatures.get(&account_id),
                        true,
                    );
                    draft.inlines = signature_inlines_for_body(
                        self.signatures.get(&account_id),
                        &draft.body_text,
                    );
                    self.cmd.emit(UiCommand::Compose(draft.clone()));
                    self.show_composer();
                    self.bind_composer_from(&self.composer, account_id);
                    apply_compose_draft(&self.composer, &draft, false);
                } else {
                    self.show_composer();
                }
                present_main_window();
                focus_composer_to(&self.composer);
            }
            MailMsg::CancelCompose => {
                if self.pending_send_draft.is_some() && !self.send_surface_released {
                    return;
                }
                let imap_draft_cell = self.composer.imap_draft.clone();
                let account_id = self.composer.from_account_id();
                let cmd = self.cmd.clone();
                let sender = sender.clone();
                confirm_discard_composer(&self.composer, Some(&self.window), move || {
                    if let Some(draft_ref) = imap_draft_cell.borrow().clone()
                        && imap_draft_has_server_uid(&draft_ref)
                    {
                        cmd.emit(UiCommand::DeleteImapDraft {
                            account: account_id,
                            folder: draft_ref.folder_id,
                            uid: draft_ref.uid,
                            message_id: draft_ref.message_id,
                        });
                    }
                    sender.input(MailMsg::ComposerDiscarded);
                });
            }
            MailMsg::ComposerDiscarded => {
                self.finish_composer_discard();
            }
            MailMsg::ShowComposer => {
                self.show_composer();
            }
            MailMsg::ComposeMailto(address) => {
                if let Some(account) = self.compose_account() {
                    let account_id = account.id;
                    let mut draft = ComposeDraft {
                        account_id,
                        to: vec![address.clone()],
                        cc: Vec::new(),
                        bcc: Vec::new(),
                        subject: String::new(),
                        body_text: String::new(),
                        format: compose_format_for_account(account_id),
                        request_mdn: request_mdn(),
                        tracking_pixel_url: effective_tracking_pixel_url(),
                        attachments: Vec::new(),
                        inlines: Vec::new(),
                        collect_outgoing_contacts: crate::settings::collect_outgoing_contacts(),
                        in_reply_to: None,
                        references: None,
                        thread_id: None,
                        approve_contact_email: None,
                        sign: false,
                        encrypt: false,
                        autocrypt_header: None,
                        identity_id: None,
                        custom_from: None,
                        imap_draft: None,
                        reaction_emoji: None,
                    };
                    draft.body_text = apply_signature(
                        &draft.body_text,
                        draft.format,
                        self.signatures.get(&account_id),
                        true,
                    );
                    draft.inlines = signature_inlines_for_body(
                        self.signatures.get(&account_id),
                        &draft.body_text,
                    );
                    self.cmd.emit(UiCommand::Compose(draft.clone()));
                    self.show_composer();
                    self.bind_composer_from(&self.composer, account_id);
                    apply_compose_draft(&self.composer, &draft, false);
                } else {
                    self.show_composer();
                }
                present_main_window();
                focus_composer_to(&self.composer);
            }
            MailMsg::ComposerWindowClosed(id) => {
                if let Some(window) = self.composer_windows.get(&id)
                    && let Some(ref imap_draft) = *window.widgets.imap_draft.borrow()
                    && imap_draft_has_server_uid(imap_draft)
                {
                    self.cmd.emit(UiCommand::DeleteImapDraft {
                        account: window.widgets.from_account_id(),
                        folder: imap_draft.folder_id,
                        uid: imap_draft.uid,
                        message_id: imap_draft.message_id.clone(),
                    });
                }
                if let Some(window) = self.composer_windows.remove(&id) {
                    window.widgets.clear_local_autosave();
                }
            }
            MailMsg::PopOutComposer => {
                if self.mail_content_pane != MailContentPane::Composer {
                    return;
                }
                if self.bind_resolved_composer_from(&self.composer).is_none() {
                    return;
                }
                let composer = self.composer.clone();
                flush_compose_draft_then(&composer, move |draft| {
                    sender.input(MailMsg::PopOutComposerWithDraft(draft));
                });
            }
            MailMsg::PopOutComposerWithDraft(draft) => {
                self.composer.clear_local_autosave();
                self.open_detached_composer(
                    draft,
                    &tr!("Compose"),
                    &sender,
                    DetachedComposerFocus::None,
                );
                self.show_reader();
            }
            MailMsg::OpenSnippetEditor(existing) => {
                open_snippet_editor(
                    &self.window,
                    existing.as_ref(),
                    sender.input_sender().clone(),
                );
            }
            MailMsg::IncludeSignatureToggled(window_id, include) => {
                if let Some(widgets) = self.composer_widgets(window_id) {
                    self.reapply_composer_signature(widgets, include);
                }
            }
            MailMsg::InsertSnippet(window_id, id) => {
                if let Some(widgets) = self.composer_widgets(window_id) {
                    self.insert_snippet(widgets, id);
                }
            }
            MailMsg::ComposeFromChanged(window_id) => {
                if let Some(widgets) = self.composer_widgets(window_id) {
                    self.reapply_composer_signature(widgets, widgets.include_signature.get());
                    self.reapply_composer_account_format(widgets);
                }
            }
            _ => unreachable!("message not routed to compose"),
        }
    }

    pub(super) fn preferences_ui(&self) -> &PreferencesUi {
        self.preferences.as_deref().expect("preferences not set")
    }

    pub(super) fn compose_account(&self) -> Option<&Account> {
        self.selected_account
            .and_then(|id| self.accounts.iter().find(|account| account.id == id))
            .or_else(|| self.accounts.first())
    }

    pub(super) fn resolved_composer_account_id(
        &self,
        widgets: &ComposerWidgets,
    ) -> Option<AccountId> {
        let id = widgets.from_account_id();
        if self.accounts.iter().any(|account| account.id == id) {
            Some(id)
        } else {
            self.compose_account().map(|account| account.id)
        }
    }

    pub(super) fn bind_composer_from(&self, widgets: &ComposerWidgets, selected: AccountId) {
        let choice = widgets.from_choice.borrow().clone();
        self.bind_composer_from_choice(widgets, selected, choice);
    }

    pub(super) fn bind_composer_from_identity(
        &self,
        widgets: &ComposerWidgets,
        selected: AccountId,
        identity: Option<IdentityId>,
    ) {
        let choice = match identity {
            Some(id) => ComposeFromChoice::Identity(id),
            None => ComposeFromChoice::Account,
        };
        self.bind_composer_from_choice(widgets, selected, choice);
    }

    pub(super) fn bind_composer_from_choice(
        &self,
        widgets: &ComposerWidgets,
        selected: AccountId,
        choice: ComposeFromChoice,
    ) {
        sync_composer_from_accounts(widgets, &self.accounts, &self.identities, selected, choice);
        widgets.toolbar.refresh_snippets(&self.snippets);
        *self.compose_hooks.snippets.borrow_mut() = self.snippets.clone();
        let state = self.pgp_by_account.get(&selected).cloned();
        *widgets.pgp_state.borrow_mut() = state.clone();
        if state.as_ref().is_some_and(|s| s.sign_outgoing) {
            widgets.set_sign_toggle_active(true);
        }
        refresh_pgp_disclosure(widgets, state.as_ref());
    }

    pub(super) fn sync_inline_composer_from_account(&self, account_id: AccountId) {
        if self.mail_content_pane != MailContentPane::Composer
            || (self.pending_send_draft.is_some() && !self.send_surface_released)
        {
            return;
        }
        if !should_retarget_composer_from(
            self.composer.from_account_id(),
            account_id,
            self.composer.is_reply_or_forward(),
        ) {
            return;
        }
        let previous = self.composer.from_account_id();
        if let Some(imap) = self.composer.imap_draft.borrow_mut().take()
            && imap_draft_has_server_uid(&imap)
        {
            self.cmd.emit(UiCommand::DeleteImapDraft {
                account: previous,
                folder: imap.folder_id,
                uid: imap.uid,
                message_id: imap.message_id.clone(),
            });
        }
        self.bind_composer_from_identity(&self.composer, account_id, None);
        self.reapply_composer_signature(&self.composer, self.composer.include_signature.get());
        self.reapply_composer_account_format(&self.composer);
    }

    pub(super) fn compose_send_from_email(&self, draft: &ComposeDraft) -> String {
        let account = self
            .accounts
            .iter()
            .find(|account| account.id == draft.account_id);
        let identities = self
            .identities
            .get(&draft.account_id)
            .map(|values| values.as_slice())
            .unwrap_or(&[]);
        mime_from_for_draft(
            account.map(|account| account.email.as_str()).unwrap_or(""),
            account
                .map(|account| account.display_name.as_str())
                .unwrap_or(""),
            draft.identity_id,
            draft.custom_from.as_deref(),
            identities,
        )
        .0
    }

    pub(super) fn present_send_from_mismatch_if_needed(
        &self,
        draft: &ComposeDraft,
        window_id: Option<u64>,
        sender: &ComponentSender<Self>,
    ) -> bool {
        if !send_from_mismatches_selected_account(draft.account_id, self.selected_account) {
            return false;
        }
        let from_email = self.compose_send_from_email(draft);
        let viewing = self
            .selected_account
            .and_then(|id| self.accounts.iter().find(|account| account.id == id))
            .map(|account| account.email.as_str())
            .unwrap_or_else(|| trs!("another account"));
        let dialog = adw::AlertDialog::builder()
            .heading(tr!("Send from a different account?"))
            .body(trf!(
                "This will send from {from_email}. You're viewing {viewing}.",
                from_email = from_email,
                viewing = viewing
            ))
            .build();
        dialog.add_response("cancel", &tr!("Cancel"));
        dialog.add_response("send", &tr!("Send anyway"));
        dialog.set_response_appearance("send", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        let sender = sender.clone();
        let draft = draft.clone();
        dialog.choose(
            Some(&self.window),
            None::<&gtk::gio::Cancellable>,
            move |response| {
                if response.as_str() == "send" {
                    sender.input(MailMsg::ConfirmedComposeSend { draft, window_id });
                }
            },
        );
        true
    }

    pub(super) fn bind_resolved_composer_from(
        &self,
        widgets: &ComposerWidgets,
    ) -> Option<AccountId> {
        let id = self.resolved_composer_account_id(widgets)?;
        if widgets.from_account_id() != id {
            self.bind_composer_from(widgets, id);
        }
        Some(id)
    }

    pub(super) fn refresh_all_composer_from(&self) {
        let inline_id = self.composer.from_account_id();
        let inline_keep = self
            .accounts
            .iter()
            .any(|account| account.id == inline_id)
            .then_some(inline_id)
            .or_else(|| self.compose_account().map(|account| account.id))
            .unwrap_or(inline_id);
        self.bind_composer_from(&self.composer, inline_keep);
        for window in self.composer_windows.values() {
            let id = window.account_id();
            let keep = self
                .accounts
                .iter()
                .any(|account| account.id == id)
                .then_some(id)
                .or_else(|| self.compose_account().map(|account| account.id))
                .unwrap_or(id);
            self.bind_composer_from(&window.widgets, keep);
        }
    }

    /// Close the composer without writing Drafts. Used after an explicit discard.
    pub(super) fn finish_composer_discard(&mut self) {
        let remove_id = self
            .composer
            .imap_draft
            .borrow()
            .as_ref()
            .filter(|imap| imap_draft_has_server_uid(imap))
            .and_then(|imap| {
                self.envelopes
                    .iter()
                    .find(|env| imap_draft_ref_matches_envelope(imap, env))
                    .map(|env| env.id)
            })
            .or_else(|| {
                self.pending_imap_draft_open.and_then(|id| {
                    self.envelope_for(id).and_then(|env| {
                        (env.uid != 0 && self.is_imap_draft_envelope(env)).then_some(id)
                    })
                })
            });
        self.pending_imap_draft_open = None;
        clear_composer(&self.composer);
        self.mail_content_pane = MailContentPane::Reader;
        self.content_stack.set_visible_child_name("reader");
        if let Some(id) = remove_id {
            self.reopen_reader_after_send = false;
            let delete_set: HashSet<MessageId> = [id].into_iter().collect();
            let next_id = adjacent_after_removed(&self.envelopes, &delete_set);
            self.remove_envelopes_from_ui(&[id], next_id);
        } else {
            self.apply_reader_after_inline_send();
        }
        self.sync_detail_after_compose();
    }

    pub(super) fn show_composer(&mut self) {
        self.composer.resume_autosave();
        let entering = self.mail_content_pane != MailContentPane::Composer;
        if entering {
            clear_composer(&self.composer);
        }
        self.mail_content_pane = MailContentPane::Composer;
        self.content_stack.set_visible_child_name("composer");
        self.note_single_pane_detail();
    }

    pub(super) fn composer_widgets(&self, window_id: Option<u64>) -> Option<&ComposerWidgets> {
        match window_id {
            None => Some(&self.composer),
            Some(id) => self.composer_windows.get(&id).map(|window| &window.widgets),
        }
    }

    pub(super) fn reapply_composer_signature(&self, widgets: &ComposerWidgets, include: bool) {
        widgets.include_signature.set(include);
        let account_id = widgets.from_account_id();
        let format = widgets.toolbar.current_format();
        let body = widgets.toolbar.body_text_for_send(&widgets.body_view);
        let foreign_cids: Vec<String> = self
            .signatures
            .iter()
            .filter(|(id, _)| **id != account_id)
            .flat_map(|(_, sig)| sig.inlines.iter().map(|inline| inline.content_id.clone()))
            .collect();
        let signature = self.signatures.get(&account_id);
        let mut updated = apply_signature(&body, format, signature, include);
        let mut dropped = false;
        {
            let mut inlines = widgets.inlines_mut();
            let updated_lower = updated.to_ascii_lowercase();
            for cid in &foreign_cids {
                let needle = format!("cid:{cid}").to_ascii_lowercase();
                if !updated_lower.contains(&needle) {
                    inlines.remove(cid);
                }
            }
            if let Some(sig) = signature {
                for inline in &sig.inlines {
                    inlines.remove(&inline.content_id);
                }
            }
            for inline in signature_inlines_for_body(signature, &updated) {
                if inlines.len() >= MAX_COMPOSE_INLINES {
                    updated =
                        remove_inline_imgs(&updated, std::slice::from_ref(&inline.content_id));
                    dropped = true;
                } else {
                    inlines.insert(inline.content_id.clone(), inline);
                }
            }
        }
        if dropped {
            crate::compose_attachments::show_composer_toast(
                widgets.toolbar.root.upcast_ref(),
                &tr!("Signature images were left out because this message already has 10 images."),
            );
        }
        widgets.toolbar.apply_body(
            &widgets.body_view,
            &widgets.body_frame,
            format,
            &updated,
            false,
        );
    }

    pub(super) fn reapply_composer_account_format(&self, widgets: &ComposerWidgets) {
        if widgets.is_reply_or_forward() {
            return;
        }
        let format = compose_format_for_account(widgets.from_account_id());
        if widgets.toolbar.current_format() != format {
            widgets.toolbar.select_format(format);
        }
    }

    pub(super) fn insert_snippet(&self, widgets: &ComposerWidgets, id: SnippetId) {
        let Some(snippet) = self.snippets.iter().find(|snippet| snippet.id == id) else {
            return;
        };
        let to_strings = widgets.to_field.strings();
        let first_email = to_strings
            .first()
            .and_then(|s| parse_address_token(s))
            .map(|a| a.email);
        let contact = first_email.as_deref().and_then(|email| {
            self.contact_book
                .borrow()
                .contacts
                .iter()
                .find(|contact| contact.email.eq_ignore_ascii_case(email))
                .cloned()
        });
        let text =
            expand_snippet_placeholders(&snippet.body, first_email.as_deref(), contact.as_ref());
        let format = widgets.toolbar.current_format();
        if format == ComposeFormat::Html {
            widgets.html_editor.insert_html(&text);
            widgets.html_editor.focus_editor();
        } else {
            let buffer = widgets.body_view.buffer();
            let insert = buffer.get_insert();
            let mut iter = buffer.iter_at_mark(&insert);
            buffer.insert(&mut iter, &text);
            widgets.body_view.grab_focus();
        }
        (self.compose_hooks.on_compose_changed)();
    }

    pub(super) fn open_detached_composer(
        &mut self,
        draft: ComposeDraft,
        fallback_title: &str,
        sender: &ComponentSender<MailModel>,
        focus: DetachedComposerFocus,
    ) {
        let id = self.next_composer_window_id;
        self.next_composer_window_id += 1;
        let title = if draft.subject.is_empty() {
            fallback_title.to_string()
        } else {
            draft.subject.clone()
        };
        let window = open_composer_window(
            self.contact_book.clone(),
            self.sent_recipients.clone(),
            self.runtime.clone(),
            &self.accounts,
            &self.identities,
            &title,
            &draft,
            matches!(focus, DetachedComposerFocus::Body),
            {
                let sender = sender.clone();
                let on_compose_changed = self.compose_hooks.on_compose_changed.clone();
                ComposerHooks {
                    on_include_signature_toggled: Rc::new({
                        let sender = sender.clone();
                        let on_compose_changed = on_compose_changed.clone();
                        let window_id = id;
                        move |include| {
                            sender
                                .input(MailMsg::IncludeSignatureToggled(Some(window_id), include));
                            on_compose_changed();
                        }
                    }),
                    on_snippet_chosen: Rc::new({
                        let sender = sender.clone();
                        let window_id = id;
                        move |snippet_id| {
                            sender.input(MailMsg::InsertSnippet(Some(window_id), snippet_id))
                        }
                    }),
                    on_manage_snippets: self.compose_hooks.on_manage_snippets.clone(),
                    on_from_changed: Rc::new({
                        let sender = sender.clone();
                        let window_id = id;
                        move || sender.input(MailMsg::ComposeFromChanged(Some(window_id)))
                    }),
                    on_compose_changed,
                    on_address_action: Rc::new({
                        let sender = sender.clone();
                        let window_id = id;
                        move |index, email| {
                            sender.input(MailMsg::ComposerAddressAction {
                                index,
                                window_id: Some(window_id),
                                email,
                            })
                        }
                    }),
                    snippets: self.compose_hooks.snippets.clone(),
                }
            },
            {
                let sender = sender.clone();
                move || sender.input(MailMsg::SendComposerWindow(id))
            },
            {
                let sender = sender.clone();
                move |choice| {
                    sender.input(MailMsg::SendWithExtension {
                        choice,
                        window_id: Some(id),
                    })
                }
            },
            {
                let sender = sender.clone();
                move || sender.input(MailMsg::ComposerWindowClosed(id))
            },
        );
        window.widgets.set_draft_slot(DraftSlot::window(id));
        window.present();
        if matches!(focus, DetachedComposerFocus::To) {
            focus_composer_to(&window.widgets);
        }
        self.composer_windows.insert(id, window);
    }

    /// Re-add signature images a recovered body references (autosave drops inline bytes).
    pub(super) fn restore_signature_inlines(&self, draft: &mut ComposeDraft) {
        let Some(signature) = self.signatures.get(&draft.account_id) else {
            return;
        };
        for inline in &signature.inlines {
            let referenced = draft
                .body_text
                .contains(&format!("cid:{}", inline.content_id));
            let present = draft
                .inlines
                .iter()
                .any(|existing| existing.content_id == inline.content_id);
            if referenced && !present {
                draft.inlines.push(inline.clone());
            }
        }
    }
}

pub(super) fn should_retarget_composer_from(
    composer_account: AccountId,
    selected_account: AccountId,
    is_reply_or_forward: bool,
) -> bool {
    !is_reply_or_forward && composer_account != selected_account
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retarget_new_compose_when_account_changes() {
        let alcedo = AccountId(Uuid::from_u128(1));
        let codeinfusion = AccountId(Uuid::from_u128(2));
        assert!(should_retarget_composer_from(alcedo, codeinfusion, false));
        assert!(!should_retarget_composer_from(alcedo, alcedo, false));
        assert!(!should_retarget_composer_from(alcedo, codeinfusion, true));
    }
}
