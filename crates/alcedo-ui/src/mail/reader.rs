//! Reader pane and message windows: bodies, inline images, remote content, unsubscribe and raw source.

use super::*;

impl MailModel {
    pub(super) fn update_reader(&mut self, msg: MailMsg, sender: ComponentSender<MailModel>) {
        match msg {
            MailMsg::OpenMessage(id) => {
                let id = open_id_following_bitset(id, &selected_ids(&self.list_view));
                if self.is_editing_listed_draft(id) {
                    self.pending_popout_draft = None;
                    self.selected = Some(id);
                    self.sync_selection_chrome();
                    self.refresh_status_bar();
                    return;
                }
                self.queue_open_and_prefetch(id);
                self.open_message(id);
            }
            MailMsg::OpenMessageWindow(id) => {
                self.open_message_window(id, &sender);
            }
            MailMsg::MessageWindowClosed(id) => {
                self.message_windows.remove(&id);
            }
            MailMsg::ShowReader => {
                self.show_reader();
            }
            MailMsg::LoadRemoteImages => {
                if selection_count(&self.list_view) == 1
                    && let Some(id) = self.selected
                {
                    self.remote_allowed_messages.borrow_mut().insert(id);
                    if !self.mock {
                        self.cmd.emit(UiCommand::LoadRemoteImages(id));
                    }
                }
            }
            MailMsg::LoadRemoteImagesFor(id) => {
                self.remote_allowed_messages.borrow_mut().insert(id);
                if !self.mock {
                    self.cmd.emit(UiCommand::LoadRemoteImages(id));
                }
            }
            MailMsg::AlwaysLoadRemoteFromSender => {
                if selection_count(&self.list_view) == 1
                    && let Some(id) = self.selected
                {
                    self.always_load_remote_from_sender(id);
                }
            }
            MailMsg::AlwaysLoadRemoteFromSenderFor(id) => {
                self.always_load_remote_from_sender(id);
            }
            MailMsg::Unsubscribe => {
                if selection_count(&self.list_view) == 1
                    && let Some(id) = self.selected
                {
                    self.confirm_unsubscribe(id, &sender);
                }
            }
            MailMsg::UnsubscribeFor(id) => {
                self.confirm_unsubscribe(id, &sender);
            }
            MailMsg::ExecuteUnsubscribe {
                id,
                list,
                mock,
                cmd,
            } => {
                self.execute_unsubscribe(id, list, mock, cmd);
            }
            MailMsg::SetSenderRemoteImages { email, allow } => {
                if let Some(normalized) = normalize_sender_email(&email) {
                    if allow {
                        self.remote_image_senders
                            .borrow_mut()
                            .insert(normalized.clone());
                    } else {
                        self.remote_image_senders.borrow_mut().remove(&normalized);
                    }
                    if !self.mock {
                        self.cmd.emit(UiCommand::SetSenderRemoteImages {
                            emails: vec![email.clone()],
                            allow,
                        });
                    }
                }
                self.refill_readers_for_sender(&email);
            }
            MailMsg::RemoteImagesPreferenceChanged => {
                self.refill_open_readers();
            }
            MailMsg::ViewMessageSource => {
                let window = self.window.clone();
                if let Some(id) = self.selected {
                    self.request_raw_message(id, RawMessageAction::View, &window);
                }
            }
            MailMsg::SaveMessageToFile => {
                let window = self.window.clone();
                if let Some(id) = self.selected {
                    self.request_raw_message(id, RawMessageAction::Save, &window);
                }
            }
            MailMsg::ViewMessageSourceFor(id) => {
                let window = self.window.clone();
                self.request_raw_message(id, RawMessageAction::View, &window);
            }
            MailMsg::SaveMessageToFileFor(id) => {
                let window = self.window.clone();
                self.request_raw_message(id, RawMessageAction::Save, &window);
            }
            _ => unreachable!("message not routed to reader"),
        }
    }

    pub(super) fn refresh_reader_headers(&self, id: MessageId, envelope: &Envelope) {
        if self.selected == Some(id) {
            update_reader_headers(&self.reader, envelope, &self.on_address_click);
        }
        if let Some(window) = self.message_windows.get(&id) {
            update_reader_headers(&window.reader, envelope, &self.on_address_click);
        }
    }

    pub(super) fn show_reader(&mut self) {
        if self.mail_content_pane == MailContentPane::Composer {
            self.persist_inline_compose_draft(None);
        }
        self.mail_content_pane = MailContentPane::Reader;
        self.content_stack.set_visible_child_name("reader");
        self.sync_single_pane();
    }

    pub(super) fn open_message(&mut self, id: MessageId) {
        self.reopen_reader_after_send = false;
        let id = open_id_following_bitset(id, &selected_ids(&self.list_view));
        let Some(envelope) = self.envelopes.iter().find(|env| env.id == id).cloned() else {
            return;
        };

        if self.is_imap_draft_envelope(&envelope) {
            if self.is_editing_imap_draft(&envelope) {
                self.selected = Some(id);
                return;
            }
            if self.mail_content_pane == MailContentPane::Composer {
                self.persist_inline_compose_draft(Some(&envelope));
            }
            self.selected = Some(id);
            self.pending_imap_draft_open = Some(id);
            // Hold the reader until ImapDraftReady lands. Switching to the
            // composer here would clear it and expose an editable blank pane
            // that `apply_imap_draft_ready` later overwrites.
            self.mail_content_pane = MailContentPane::Reader;
            self.content_stack.set_visible_child_name("reader");
            show_reader_placeholder(&self.reader, ReaderPlaceholder::OpeningDraft);
            sync_list_selection_to_reader(&self.list_view, Some(id));
            self.sync_selection_chrome();
            self.refresh_status_bar();
            return;
        }

        self.pending_imap_draft_open = None;
        self.selected = Some(id);
        self.mark_message_seen(id);
        self.update_in_pane_reader(id, &envelope);
        self.cmd.emit(UiCommand::LoadReactions { target_id: id });
        self.cmd.emit(UiCommand::LoadReactionOriginal { id });
        self.show_reader();
        sync_list_selection_to_reader(&self.list_view, Some(id));
        schedule_sync_list_selection_to_reader(&self.list_view, Some(id));
        self.sync_selection_chrome();
        self.refresh_status_bar();
    }

    pub(super) fn open_message_window(
        &mut self,
        id: MessageId,
        sender: &ComponentSender<MailModel>,
    ) {
        if let Some(window) = self.message_windows.get(&id) {
            window.present();
            if let Some(envelope) = self.envelopes.iter().find(|env| env.id == id).cloned() {
                self.update_detached_reader(id, &envelope);
            }
            return;
        }

        let Some(envelope) = self.envelopes.iter().find(|env| env.id == id).cloned() else {
            return;
        };

        self.cmd.emit(UiCommand::OpenMessage(id));
        self.mark_message_seen(id);

        let subject = envelope.display_subject();
        let sender = sender.clone();
        let window = open_message_window(
            &subject,
            self.cid_cache.clone(),
            {
                let sender = sender.clone();
                move |address| sender.input(MailMsg::ComposeMailto(address))
            },
            {
                let sender = sender.clone();
                move || sender.input(MailMsg::LoadRemoteImagesFor(id))
            },
            {
                let sender = sender.clone();
                move || sender.input(MailMsg::AlwaysLoadRemoteFromSenderFor(id))
            },
            {
                let sender = sender.clone();
                move || sender.input(MailMsg::ReplyTo(id))
            },
            {
                let sender = sender.clone();
                move || sender.input(MailMsg::ReplyAllTo(id))
            },
            {
                let sender = sender.clone();
                move || sender.input(MailMsg::ReplyToInWindow(id))
            },
            {
                let sender = sender.clone();
                move || sender.input(MailMsg::ReplyAllToInWindow(id))
            },
            {
                let sender = sender.clone();
                move || sender.input(MailMsg::ReplyListTo(id))
            },
            {
                let sender = sender.clone();
                move || sender.input(MailMsg::ForwardTo(id))
            },
            {
                let sender = sender.clone();
                move || sender.input(MailMsg::ForwardToInWindow(id))
            },
            {
                let sender = sender.clone();
                move || sender.input(MailMsg::ViewMessageSourceFor(id))
            },
            {
                let sender = sender.clone();
                move || sender.input(MailMsg::SaveMessageToFileFor(id))
            },
            {
                let sender = sender.clone();
                move || sender.input(MailMsg::UnsubscribeFor(id))
            },
            {
                let sender = sender.clone();
                move |copied_id| sender.input(MailMsg::CopiedVerificationCode(copied_id))
            },
            {
                let sender = sender.clone();
                move || sender.input(MailMsg::Reaction(ReactionMsg::Open(id)))
            },
            {
                let sender = sender.clone();
                move |emoji| sender.input(MailMsg::Reaction(ReactionMsg::Send { id, emoji }))
            },
            {
                let on_address_click = address_chip_popover_handler(
                    self.contact_book.clone(),
                    self.remote_image_senders.clone(),
                    self.msg_sender.clone(),
                    Rc::new({
                        let sender = sender.clone();
                        let account_id = envelope.account_id;
                        move |index, email| {
                            sender.input(MailMsg::AddressAction {
                                index,
                                account_id: Some(account_id),
                                email,
                            })
                        }
                    }),
                );
                move |anchor, addr| on_address_click(anchor, addr)
            },
            {
                let sender = sender.clone();
                move || sender.input(MailMsg::MessageWindowClosed(id))
            },
        );
        self.cmd.emit(UiCommand::LoadReactions { target_id: id });
        self.cmd.emit(UiCommand::LoadReactionOriginal { id });
        self.fill_reader(&window.reader, id, &envelope);
        window.present();
        self.message_windows.insert(id, window);
    }

    pub(super) fn dialog_parent_for_message(
        &self,
        id: MessageId,
        main_root: &adw::ApplicationWindow,
    ) -> gtk::Widget {
        if let Some(window) = self.message_windows.get(&id) {
            window.dialog_parent()
        } else {
            main_root.clone().upcast()
        }
    }

    pub(super) fn mock_raw_message(&self, id: MessageId) -> Option<Vec<u8>> {
        let envelope = self.envelope_for(id)?;
        let body = self
            .bodies
            .get(&id)
            .cloned()
            .unwrap_or_else(|| "Mock message body.".to_string());
        let from = envelope
            .from
            .first()
            .map(Address::formatted)
            .unwrap_or_else(|| "sender@example.com".to_string());
        let to = envelope
            .to
            .iter()
            .map(Address::formatted)
            .collect::<Vec<_>>()
            .join(", ");
        let cc = envelope
            .cc
            .iter()
            .map(Address::formatted)
            .collect::<Vec<_>>()
            .join(", ");
        let mut headers = format!(
            "From: {from}\r\nTo: {to}\r\nSubject: {}\r\nDate: {}\r\nMIME-Version: 1.0\r\nContent-Type: text/plain; charset=utf-8\r\n",
            envelope.subject,
            envelope.date.to_rfc2822(),
        );
        if !cc.is_empty() {
            headers.push_str(&format!("Cc: {cc}\r\n"));
        }
        Some(format!("{headers}\r\n{body}").into_bytes())
    }

    pub(super) fn request_raw_message(
        &mut self,
        id: MessageId,
        action: RawMessageAction,
        root: &adw::ApplicationWindow,
    ) {
        if let Some(raw) = self.raw_messages.get(&id).cloned() {
            self.complete_raw_action(id, action, raw, root);
            return;
        }
        if self.mock {
            if let Some(raw) = self.mock_raw_message(id) {
                self.raw_messages.insert(id, raw.clone());
                self.complete_raw_action(id, action, raw, root);
            } else {
                self.toasts
                    .add_toast(crate::toast::plain("Message not found"));
            }
            return;
        }
        self.pending_raw_action = Some((id, action));
        self.cmd.emit(UiCommand::FetchRawMessage(id));
    }

    pub(super) fn complete_raw_action(
        &self,
        id: MessageId,
        action: RawMessageAction,
        raw: Vec<u8>,
        root: &adw::ApplicationWindow,
    ) {
        let parent = self.dialog_parent_for_message(id, root);
        let filename = self
            .envelope_for(id)
            .map(|env| suggested_eml_filename(&env.subject))
            .unwrap_or_else(|| "message.eml".to_string());
        match action {
            RawMessageAction::View => open_source_viewer(&parent, raw, &filename),
            RawMessageAction::Save => save_message_to_file(&parent, &raw, &filename),
        }
    }

    /// Point `cid:` images at cached bytes.
    ///
    /// A large message stores its HTML before the image parts arrive, so the
    /// saved HTML still says `cid:…` after those parts are on disk. Rewrite
    /// only the copy the reader paints; reply and forward keep the original.
    pub(super) fn html_with_inline_images(&self, id: MessageId, html: String) -> String {
        let Some(map) = self.inline_cid_sha.get(&id) else {
            return html;
        };
        if map.is_empty() {
            return html;
        }
        rewrite_cids(&html, map)
    }

    pub(super) fn reader_body_for(
        &self,
        id: MessageId,
    ) -> (Option<String>, Option<String>, &[BodyAttachment]) {
        let text = self.bodies.get(&id).cloned();
        let html = self.html_bodies.get(&id).cloned();
        let attachments = self
            .attachments_by_id
            .get(&id)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        (text, html, attachments)
    }

    pub(super) fn fill_reader(&self, widgets: &ReaderWidgets, id: MessageId, envelope: &Envelope) {
        let (text, html, attachments) = {
            let (text, html, attachments) = self.reader_body_for(id);
            (text, html, attachments.to_vec())
        };
        let html = html.map(|html| self.html_with_inline_images(id, html));
        let (text, html) = self.with_reaction_original(id, text, html);
        let dummy_attachment = self.mock && envelope.has_attachments && attachments.is_empty();
        let attachments_failed = self.attachments_failed_ids.contains(&id);
        let attachments_pending = reader_attachments_pending(
            &self.attachments_pending_ids,
            &self.attachments_failed_ids,
            id,
            envelope,
            &attachments,
        );
        let allow_remote = should_load_remote_images(
            load_remote_images(),
            self.remote_allowed_messages.borrow().contains(&id),
            &envelope.from,
            &self.remote_image_senders.borrow(),
        );
        let account_email = self.account_email(envelope.account_id);
        if reader_body_is_ready(text.as_ref(), html.as_ref()) {
            update_reader(
                widgets,
                envelope,
                &account_email,
                html.as_deref(),
                text.as_deref().unwrap_or(""),
                &attachments,
                dummy_attachment,
                attachments_pending,
                attachments_failed,
                allow_remote,
                &self.on_address_click,
                &self.msg_sender,
            );
        } else {
            update_reader_headers(widgets, envelope, &self.on_address_click);
            set_reader_actions_sensitive(widgets, true);
            show_reader_body_loading(
                widgets,
                envelope.has_attachments && attachments.is_empty() && !attachments_failed,
            );
        }
        update_unsubscribe_button(
            widgets,
            self.unsubscribe_by_id.get(&id),
            self.unsubscribed_ids.contains(&id),
            self.unsubscribing_ids.contains(&id),
        );
        if let Some((account_id, email, armored)) = self.autocrypt_by_message.get(&id).cloned() {
            let pgp_status = self.pgp_by_message.get(&id);
            let status_empty = pgp_status.is_none_or(|status| {
                !status.encrypted
                    && matches!(status.signature, alcedo_core::PgpSignatureState::None)
            });
            let sender = self.mail_sender.clone();
            let accept_email = email.clone();
            let accept_armored = armored.clone();
            let accept = move || {
                sender
                    .send(MailMsg::PgpAcceptPeer {
                        account: account_id,
                        email: accept_email,
                        armored: accept_armored,
                    })
                    .ok();
            };
            update_reader_pgp_banner(widgets, id, pgp_status);
            if status_empty {
                update_reader_autocrypt_banner(widgets, &email, accept);
            } else {
                wire_reader_pgp_banner_accept(widgets, accept);
            }
        } else {
            update_reader_pgp_banner(widgets, id, self.pgp_by_message.get(&id));
        }
        self.update_reaction_chrome(widgets, id, envelope);
    }

    pub(super) fn refresh_unsubscribe_button_for_id(&self, id: MessageId) {
        let action = self.unsubscribe_by_id.get(&id);
        let done = self.unsubscribed_ids.contains(&id);
        let pending = self.unsubscribing_ids.contains(&id);
        if self.selected == Some(id) {
            update_unsubscribe_button(&self.reader, action, done, pending);
        }
        if let Some(window) = self.message_windows.get(&id) {
            update_unsubscribe_button(&window.reader, action, done, pending);
        }
    }

    pub(super) fn always_load_remote_from_sender(&mut self, id: MessageId) {
        let Some(envelope) = self
            .envelopes
            .iter()
            .find(|env| env.id == id)
            .cloned()
            .or_else(|| {
                self.source_envelopes
                    .iter()
                    .find(|env| env.id == id)
                    .cloned()
            })
        else {
            return;
        };
        self.remote_allowed_messages.borrow_mut().insert(id);
        let mut emails = Vec::new();
        for addr in &envelope.from {
            if let Some(email) = normalize_sender_email(&addr.email) {
                self.remote_image_senders.borrow_mut().insert(email.clone());
                emails.push(addr.email.clone());
            }
        }
        if !self.mock {
            if !emails.is_empty() {
                self.cmd.emit(UiCommand::SetSenderRemoteImages {
                    emails,
                    allow: true,
                });
            }
            self.cmd.emit(UiCommand::LoadRemoteImages(id));
        }
        self.update_readers_for_message(id);
    }

    pub(super) fn refill_open_readers(&self) {
        if let Some(id) = self.selected
            && let Some(envelope) = self.envelopes.iter().find(|env| env.id == id)
        {
            self.fill_reader(&self.reader, id, envelope);
        }
        for (id, window) in &self.message_windows {
            if let Some(envelope) = self.envelopes.iter().find(|env| env.id == *id) {
                self.fill_reader(&window.reader, *id, envelope);
            }
        }
    }

    pub(super) fn refill_readers_for_sender(&self, email: &str) {
        let normalized = normalize_sender_email(email);
        if let Some(id) = self.selected
            && let Some(envelope) = self.envelopes.iter().find(|env| env.id == id)
            && envelope
                .from
                .iter()
                .any(|addr| normalize_sender_email(&addr.email) == normalized)
        {
            self.fill_reader(&self.reader, id, envelope);
        }
        for (id, window) in &self.message_windows {
            if let Some(envelope) = self.envelopes.iter().find(|env| env.id == *id)
                && envelope
                    .from
                    .iter()
                    .any(|addr| normalize_sender_email(&addr.email) == normalized)
            {
                self.fill_reader(&window.reader, *id, envelope);
            }
        }
    }

    pub(super) fn update_in_pane_reader(&self, id: MessageId, envelope: &Envelope) {
        self.fill_reader(&self.reader, id, envelope);
    }

    pub(super) fn update_detached_reader(&self, id: MessageId, envelope: &Envelope) {
        let Some(window) = self.message_windows.get(&id) else {
            return;
        };
        self.fill_reader(&window.reader, id, envelope);
    }

    pub(super) fn update_readers_for_message(&mut self, id: MessageId) {
        let Some(envelope) = self.envelopes.iter().find(|env| env.id == id).cloned() else {
            return;
        };
        if self.selected == Some(id) {
            self.update_in_pane_reader(id, &envelope);
            if self.mail_content_pane == MailContentPane::Reader {
                self.show_reader();
            }
        }
        if self.message_windows.contains_key(&id) {
            self.update_detached_reader(id, &envelope);
        }
    }

    pub(super) fn confirm_unsubscribe(&self, id: MessageId, sender: &ComponentSender<MailModel>) {
        let Some(list) = self.unsubscribe_by_id.get(&id).cloned() else {
            return;
        };
        let Some(envelope) = self.envelopes.iter().find(|env| env.id == id) else {
            return;
        };
        let sender_name = envelope
            .from
            .first()
            .map(|addr| addr.display_name())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| "this mailing list".to_string());
        let body = if list.one_click {
            "Alcedo will send an unsubscribe request for you. You may still receive a few messages while it takes effect.".to_string()
        } else if let Some(mailto) = &list.mailto {
            format!(
                "Alcedo will open a message to {}. Send it to finish unsubscribing.",
                mailto.address
            )
        } else {
            "Alcedo will open the sender’s unsubscribe page in your browser.".to_string()
        };
        let confirm = adw::AlertDialog::builder()
            .heading(format!("Unsubscribe from {sender_name}?"))
            .body(&body)
            .build();
        confirm.add_response("cancel", "Cancel");
        confirm.add_response("unsubscribe", "Unsubscribe");
        confirm.set_default_response(Some("unsubscribe"));
        confirm.set_close_response("cancel");
        let parent = self.dialog_parent_for_message(id, &self.window);
        let sender = sender.clone();
        let cmd = self.cmd.clone();
        let mock = self.mock;
        confirm.choose(
            Some(&parent),
            None::<&gtk::gio::Cancellable>,
            move |response| {
                if response.as_str() != "unsubscribe" {
                    return;
                }
                sender.input(MailMsg::ExecuteUnsubscribe {
                    id,
                    list,
                    mock,
                    cmd,
                });
            },
        );
    }

    pub(super) fn execute_unsubscribe(
        &mut self,
        id: MessageId,
        list: ListUnsubscribe,
        mock: bool,
        cmd: Sender<UiCommand>,
    ) {
        if list.one_click {
            self.unsubscribing_ids.insert(id);
            self.refresh_unsubscribe_button_for_id(id);
            if !mock {
                cmd.emit(UiCommand::OneClickUnsubscribe(id));
            }
            return;
        }
        if let Some(mailto) = list.mailto {
            if let Some(account) = self.compose_account() {
                let email = mailto.address;
                let draft = ComposeDraft {
                    account_id: account.id,
                    to: vec![email.clone()],
                    cc: Vec::new(),
                    bcc: Vec::new(),
                    subject: mailto.subject.unwrap_or_else(|| "unsubscribe".to_string()),
                    body_text: mailto.body.unwrap_or_default(),
                    format: ComposeFormat::Plain,
                    request_mdn: false,
                    tracking_pixel_url: None,
                    attachments: Vec::new(),
                    inlines: Vec::new(),
                    collect_outgoing_contacts: false,
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
                cmd.emit(UiCommand::Compose(draft.clone()));
                self.show_composer();
                self.bind_composer_from(&self.composer, draft.account_id);
                apply_compose_draft(&self.composer, &draft, false);
                self.composer.to_field.set_from_strings(&[email]);
                present_main_window();
                focus_composer_to(&self.composer);
            }
            return;
        }
        if let Some(url) = list.https {
            let parent: Option<gtk::Window> = if let Some(window) = self.message_windows.get(&id) {
                window.dialog_parent().downcast::<gtk::Window>().ok()
            } else {
                Some(self.window.clone().upcast::<gtk::Window>())
            };
            let launcher = gtk::UriLauncher::new(&url);
            launcher.launch(parent.as_ref(), None::<&gtk::gio::Cancellable>, |result| {
                if let Err(err) = result {
                    tracing::warn!(?err, "failed to launch unsubscribe URI");
                }
            });
        }
    }
}

/// Budget for each of the reader's body caches (text and HTML).
pub(super) const BODY_CACHE_MAX_BYTES: usize = 64 * 1024 * 1024;

pub(super) type BodyCache = ByteLru<MessageId, String>;

pub(super) fn body_cache(seed: HashMap<MessageId, String>) -> BodyCache {
    let mut cache = ByteLru::new(BODY_CACHE_MAX_BYTES);
    for (id, body) in seed {
        cache.insert(id, body);
    }
    cache
}

pub(super) fn mock_html_bodies(
    mock: bool,
    bodies: &HashMap<MessageId, String>,
) -> HashMap<MessageId, String> {
    if !mock {
        return HashMap::new();
    }

    bodies
        .iter()
        .map(|(id, body)| {
            (
                *id,
                format!(
                    "<p>{body}</p><script>alert('xss')</script><img src=\"https://example.com/pixel.png\" alt=\"remote\">"
                ),
            )
        })
        .collect()
}

pub(super) fn reader_body_is_ready(text: Option<&String>, html: Option<&String>) -> bool {
    text.is_some() || html.is_some()
}

pub(super) fn reader_attachments_pending(
    pending_ids: &HashSet<MessageId>,
    failed_ids: &HashSet<MessageId>,
    id: MessageId,
    envelope: &Envelope,
    attachments: &[BodyAttachment],
) -> bool {
    pending_ids.contains(&id)
        || (envelope.has_attachments && attachments.is_empty() && !failed_ids.contains(&id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mail::test_support::envelope;

    #[test]
    fn reader_body_is_ready_when_text_cached() {
        let text = "Hello".to_string();
        assert!(reader_body_is_ready(Some(&text), None));
    }

    #[test]
    fn reader_body_is_ready_when_html_cached() {
        let html = "<p>Hi</p>".to_string();
        assert!(reader_body_is_ready(None, Some(&html)));
    }

    #[test]
    fn reader_body_is_ready_false_when_maps_empty() {
        assert!(!reader_body_is_ready(None, None));
    }

    #[test]
    fn fill_reader_pending_from_envelope_when_ids_empty() {
        let pending_ids = HashSet::new();
        let failed_ids = HashSet::new();
        let id = MessageId(42);
        let mut env = envelope("alice@example.com", &["bob@example.com"], &[]);
        env.has_attachments = true;
        assert!(reader_attachments_pending(
            &pending_ids,
            &failed_ids,
            id,
            &env,
            &[],
        ));
    }
}
