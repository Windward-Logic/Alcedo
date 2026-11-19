//! Send pipeline: validation, queueing and the sent toast.

use alcedo_core::QueuedSendId;
use alcedo_i18n::{tr, trf, trs};

use super::*;

impl MailModel {
    pub(super) fn update_send(&mut self, msg: MailMsg, sender: ComponentSender<MailModel>) {
        match msg {
            MailMsg::SendCompose => {
                self.proceed_send_compose(&sender);
            }
            MailMsg::SendComposerWindow(id) => {
                self.proceed_send_composer_window(id, &sender);
            }
            MailMsg::QueuedComposeSend { draft, window_id } => {
                if self.present_send_from_mismatch_if_needed(&draft, window_id, &sender) {
                    return;
                }
                if self.present_pre_send_warning_if_needed(&draft, window_id) {
                    return;
                }
                self.start_queued_send(draft, window_id);
            }
            MailMsg::ConfirmedComposeSend { draft, window_id } => {
                if self.present_pre_send_warning_if_needed(&draft, window_id) {
                    return;
                }
                self.start_queued_send(draft, window_id);
            }
            MailMsg::PreSendConfirmed { draft, window_id } => {
                self.start_queued_send(draft, window_id);
            }
            MailMsg::SendWithExtension { choice, window_id } => {
                self.send_with_extension(choice, window_id, &sender);
            }
            MailMsg::SendWithExtensionDraft {
                draft,
                choice,
                window_id,
            } => self.finish_send_with_extension(draft, choice, window_id),
            MailMsg::UndoSend => self.undo_delayed_send(&sender),
            MailMsg::DelayedSendElapsed => self.release_delayed_send(),
            _ => unreachable!("message not routed to send"),
        }
    }

    /// The "sent" toast, with an extension's button when one offers it.
    pub(super) fn show_sent_toast(
        &mut self,
        thread_id: Option<ThreadId>,
        account_id: Option<AccountId>,
        message_id_hdr: String,
    ) {
        let from_email = account_id.and_then(|id| {
            self.accounts
                .iter()
                .find(|account| account.id == id)
                .map(|account| account.email.as_str())
        });
        let toast = crate::toast::plain(&sent_toast_title(from_email));
        if let Some(account_id) = account_id {
            let sent = crate::extension::SentMessage {
                account_id,
                thread_id,
                message_id_hdr,
            };
            let action = crate::extension::registered()
                .iter()
                .find_map(|extension| extension.sent_toast_action(&sent));
            if let Some(action) = action {
                toast.set_button_label(Some(&action.label));
                let ctx = self.ext_ctx.clone();
                toast.connect_button_clicked(move |_| (action.activate)(&ctx));
            }
        }
        self.toasts.add_toast(toast);
    }

    /// Check and flush a composer, then hand its draft to the extension's Send
    /// menu choice.
    fn send_with_extension(
        &mut self,
        choice: String,
        window_id: Option<u64>,
        sender: &ComponentSender<MailModel>,
    ) {
        self.send_in_flight.set(true);
        let widgets = match window_id {
            Some(id) => match self.composer_windows.get(&id) {
                Some(window) => window.widgets.clone(),
                None => {
                    self.send_in_flight.set(false);
                    return;
                }
            },
            None => {
                if self.mail_content_pane != MailContentPane::Composer {
                    self.send_in_flight.set(false);
                    return;
                }
                self.composer.clone()
            }
        };
        if let Err(message) = ready_to_send(&widgets) {
            self.send_in_flight.set(false);
            self.toasts.add_toast(crate::toast::plain(message));
            return;
        }
        let sender = sender.clone();
        flush_compose_draft_then(&widgets, move |draft| {
            sender.input(MailMsg::SendWithExtensionDraft {
                draft,
                choice: choice.clone(),
                window_id,
            });
        });
    }

    fn finish_send_with_extension(
        &mut self,
        draft: ComposeDraft,
        choice: String,
        window_id: Option<u64>,
    ) {
        self.send_in_flight.set(false);
        if !compose_draft_has_recipients(&draft) {
            self.toasts
                .add_toast(crate::toast::plain(&tr!("Add at least one recipient")));
            return;
        }
        let Some(menu) = crate::extension::registered()
            .iter()
            .find_map(|extension| extension.send_menu())
        else {
            return;
        };
        (menu.send)(&self.ext_ctx, draft, choice);
        match window_id {
            None => self.composer.clear_local_autosave(),
            Some(id) => clear_compose_draft_slot(DraftSlot::window(id)),
        }
        if let Some(id) = window_id {
            if let Some(window) = self.composer_windows.remove(&id) {
                window.close_after_send();
            }
        } else {
            clear_composer(&self.composer);
            self.show_reader();
        }
    }

    pub(super) fn apply_reader_after_inline_send(&mut self) {
        let action =
            reader_after_inline_send(self.reopen_reader_after_send, self.selected.is_some());
        if !matches!(action, ReaderAfterInlineSend::ShowExisting) {
            self.reopen_reader_after_send = false;
        }
        match action {
            ReaderAfterInlineSend::ShowExisting => self.show_reader(),
            ReaderAfterInlineSend::OpenSelected => {
                if let Some(id) = self.selected {
                    self.open_row_for_latest(id, None);
                }
            }
            ReaderAfterInlineSend::Clear => {
                clear_reader(&self.reader);
                self.show_reader();
            }
        }
    }

    pub(super) fn proceed_send_compose(&mut self, sender: &ComponentSender<Self>) {
        if self.mail_content_pane != MailContentPane::Composer {
            return;
        }
        if let Err(message) = ready_to_send(&self.composer) {
            self.toasts.add_toast(crate::toast::plain(message));
            return;
        }
        if self.mock {
            if crate::settings::collect_outgoing_contacts() {
                let composer = self.composer.clone();
                let book = self.contact_book.clone();
                let accounts = self.accounts.clone();
                flush_compose_draft_then(&composer, move |draft| {
                    let account_email = accounts
                        .iter()
                        .find(|account| account.id == draft.account_id)
                        .map(|account| account.email.clone())
                        .unwrap_or_default();
                    let mut addresses = Vec::new();
                    for field in [&draft.to, &draft.cc, &draft.bcc] {
                        for token in field {
                            if let Some(addr) = parse_address_token(token) {
                                addresses.push(addr);
                            }
                        }
                    }
                    let mut book = book.borrow_mut();
                    let ContactBook {
                        contacts, groups, ..
                    } = &mut *book;
                    collect_outgoing_into(
                        contacts,
                        groups,
                        &account_email,
                        &addresses,
                        chrono::Utc::now(),
                    );
                });
            }
            tracing::info!("Send compose (mock — not delivered)");
            let draft = compose_draft_from_widgets(&self.composer);
            self.record(
                activity_log::KIND_MAIL,
                ActivityOutcome::Success,
                tr!("Message sent (mock)"),
                self.send_detail_from_draft(&draft, None),
            );
            self.toasts
                .add_toast(crate::toast::plain(&tr!("Message sent (mock)")));
            self.show_reader();
        } else if self.bind_resolved_composer_from(&self.composer).is_none() {
        } else {
            let composer = self.composer.clone();
            let sender = sender.clone();
            flush_compose_draft_then(&composer, move |draft| {
                sender.input(MailMsg::QueuedComposeSend {
                    draft,
                    window_id: None,
                });
            });
        }
    }

    pub(super) fn proceed_send_composer_window(&mut self, id: u64, sender: &ComponentSender<Self>) {
        if !self.composer_windows.contains_key(&id) {
            return;
        }
        let window = self.composer_windows.get(&id).expect("checked above");
        if let Err(message) = ready_to_send(&window.widgets) {
            self.toasts.add_toast(crate::toast::plain(message));
            return;
        }
        if self.mock {
            tracing::info!("Send compose (mock — not delivered)");
            let window = self.composer_windows.get(&id).expect("checked above");
            let draft = compose_draft_from_widgets(&window.widgets);
            self.record(
                activity_log::KIND_MAIL,
                ActivityOutcome::Success,
                tr!("Message sent (mock)"),
                self.send_detail_from_draft(&draft, None),
            );
            self.toasts
                .add_toast(crate::toast::plain(&tr!("Message sent (mock)")));
            if let Some(window) = self.composer_windows.remove(&id) {
                window.close_after_send();
            }
            return;
        }
        let window = self.composer_windows.get(&id).expect("checked above");
        let widgets = window.widgets.clone();
        if self.bind_resolved_composer_from(&widgets).is_none() {
            return;
        }
        let sender = sender.clone();
        flush_compose_draft_then(&widgets, move |draft| {
            sender.input(MailMsg::QueuedComposeSend {
                draft,
                window_id: Some(id),
            });
        });
    }

    pub(super) fn start_queued_send(&mut self, draft: ComposeDraft, window_id: Option<u64>) {
        if self.queue_outgoing_send(draft, window_id) {
            self.release_composer_for_queued_send(window_id);
        }
    }

    /// Leave the compose surface as soon as the message is queued. SMTP runs on
    /// the account worker; keeping the composer open and insensitive froze the
    /// mail view until `SendState::Sent`.
    pub(super) fn release_composer_for_queued_send(&mut self, window_id: Option<u64>) {
        self.send_surface_released = true;
        if let Some(id) = window_id {
            if let Some(window) = self.composer_windows.remove(&id) {
                window.widgets.suspend_autosave();
                clear_compose_draft_slot(DraftSlot::window(id));
                window.close_after_send();
            }
            return;
        }
        ignore_focus_selection_briefly();
        self.composer.suspend_autosave();
        clear_composer(&self.composer);
        self.composer_send.set_sending(&self.composer, false);
        self.show_reader();
        self.list_view.view.grab_focus();
    }

    /// Put a queued draft back into a composer after Undo or a failed send.
    /// A compose the user started in the meantime stays put; the restored draft
    /// opens in its own window.
    pub(super) fn restore_unsent_draft(
        &mut self,
        draft: ComposeDraft,
        window_id: Option<u64>,
        sender: &ComponentSender<MailModel>,
    ) {
        let window_still_open = window_id.is_some_and(|id| self.composer_windows.contains_key(&id));
        let composer_open = self.mail_content_pane == MailContentPane::Composer;
        let inline_busy = composer_open && composer_has_draft(&self.composer);
        match unsent_draft_restore(window_still_open, window_id.is_some(), inline_busy) {
            UnsentDraftRestore::ExistingWindow => {
                let Some(id) = window_id else {
                    return;
                };
                let Some(window) = self.composer_windows.get(&id) else {
                    return;
                };
                window.widgets.resume_autosave();
                self.bind_composer_from(&window.widgets, draft.account_id);
                apply_compose_draft(&window.widgets, &draft, false);
                window.set_sending(false);
                window.present();
            }
            UnsentDraftRestore::NewWindow => {
                let title = if draft.in_reply_to.is_some() {
                    trs!("Reply")
                } else {
                    trs!("Compose")
                };
                let focus = if draft.in_reply_to.is_some() {
                    DetachedComposerFocus::Body
                } else {
                    DetachedComposerFocus::To
                };
                self.open_detached_composer(draft, title, sender, focus);
            }
            UnsentDraftRestore::Inline => {
                if !composer_open {
                    self.show_composer();
                    present_main_window();
                } else {
                    self.composer.resume_autosave();
                }
                self.composer_send.set_sending(&self.composer, false);
                self.bind_composer_from(&self.composer, draft.account_id);
                apply_compose_draft(&self.composer, &draft, draft.in_reply_to.is_some());
            }
        }
    }

    pub(super) fn persist_queued_send(&mut self, draft: &ComposeDraft) {
        if self.mock {
            return;
        }
        let id = QueuedSendId(uuid::Uuid::new_v4());
        self.pending_send_queue_id = Some(id);
        self.cmd.emit(UiCommand::PersistQueuedSend {
            id,
            draft: draft.clone(),
        });
    }

    pub(super) fn forget_queued_send(&mut self) {
        if let Some(id) = self.pending_send_queue_id.take() {
            self.cmd.emit(UiCommand::ForgetQueuedSend { id });
        }
    }

    pub(super) fn queue_outgoing_send(
        &mut self,
        draft: ComposeDraft,
        window_id: Option<u64>,
    ) -> bool {
        if self.pending_send_draft.is_some() {
            self.toasts
                .add_toast(crate::toast::plain(&tr!("A message is already sending")));
            return false;
        }
        if !compose_draft_has_recipients(&draft) {
            self.toasts
                .add_toast(crate::toast::plain(&tr!("Add at least one recipient")));
            return false;
        }
        self.pending_send_draft = Some(draft.clone());
        self.pending_send_window = window_id;
        self.send_in_flight.set(true);
        self.persist_queued_send(&draft);
        let delay_secs = crate::settings::send_delay_seconds();
        if delay_secs > 0 {
            self.hold_send_for_undo(delay_secs);
            return true;
        }
        self.cmd.emit(UiCommand::Send(draft));
        true
    }

    pub(super) fn send_detail_from_draft(
        &self,
        draft: &ComposeDraft,
        error: Option<&str>,
    ) -> String {
        let from = self
            .accounts
            .iter()
            .find(|account| account.id == draft.account_id)
            .map(|account| account.email.as_str());
        activity_log::format_send_detail(
            from,
            &draft.to,
            &draft.cc,
            &draft.bcc,
            &draft.subject,
            error,
        )
    }
}

/// Where a draft goes when Send is undone or the server rejects it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum UnsentDraftRestore {
    /// Detached composer is still on screen.
    ExistingWindow,
    /// Inline composer is already a different draft.
    NewWindow,
    /// Main composer is free.
    Inline,
}

pub(super) fn unsent_draft_restore(
    window_still_open: bool,
    sent_from_window: bool,
    inline_busy: bool,
) -> UnsentDraftRestore {
    if window_still_open {
        UnsentDraftRestore::ExistingWindow
    } else if sent_from_window || inline_busy {
        UnsentDraftRestore::NewWindow
    } else {
        UnsentDraftRestore::Inline
    }
}

/// Success UI already ran when the composer was closed at queue time.
pub(super) fn sent_should_close_composer(already_released: bool) -> bool {
    !already_released
}

pub(super) fn compose_draft_has_recipients(draft: &ComposeDraft) -> bool {
    !draft.to.is_empty() || !draft.cc.is_empty() || !draft.bcc.is_empty()
}

pub(super) fn send_from_mismatches_selected_account(
    draft_account: AccountId,
    selected_account: Option<AccountId>,
) -> bool {
    selected_account.is_some_and(|id| id != draft_account)
}

pub(super) fn composer_send_in_progress(composer_open: bool, pending_send: bool) -> bool {
    composer_open && pending_send
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ReaderAfterInlineSend {
    ShowExisting,
    OpenSelected,
    Clear,
}

pub(super) fn reader_after_inline_send(reopen: bool, has_selection: bool) -> ReaderAfterInlineSend {
    if !reopen {
        ReaderAfterInlineSend::ShowExisting
    } else if has_selection {
        ReaderAfterInlineSend::OpenSelected
    } else {
        ReaderAfterInlineSend::Clear
    }
}

pub(super) fn sent_toast_title(from_email: Option<&str>) -> String {
    match from_email {
        Some(email) if !email.is_empty() => trf!("Message sent from {email}", email = email),
        _ => tr!("Message sent"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn send_from_mismatch_when_viewing_other_account() {
        let sending = AccountId(Uuid::from_u128(1));
        let viewing = AccountId(Uuid::from_u128(2));
        assert!(send_from_mismatches_selected_account(
            sending,
            Some(viewing)
        ));
        assert!(!send_from_mismatches_selected_account(
            sending,
            Some(sending)
        ));
        assert!(!send_from_mismatches_selected_account(sending, None));
    }

    #[test]
    fn composer_send_in_progress_only_when_composer_and_queued() {
        assert!(composer_send_in_progress(true, true));
        assert!(!composer_send_in_progress(true, false));
        assert!(!composer_send_in_progress(false, true));
        assert!(!composer_send_in_progress(false, false));
    }

    #[test]
    fn delete_cancels_compose_only_when_idle() {
        let cancel = |composer_open: bool, pending_send: bool| {
            composer_open && !composer_send_in_progress(composer_open, pending_send)
        };
        assert!(cancel(true, false));
        assert!(!cancel(true, true));
        assert!(!cancel(false, false));
        assert!(!cancel(false, true));
    }

    #[test]
    fn delete_during_send_does_not_cancel() {
        let composer_open = true;
        let pending_send = true;
        let cancel = composer_open && !composer_send_in_progress(composer_open, pending_send);
        assert!(!cancel);
    }

    #[test]
    fn reader_after_inline_send_opens_when_selection_moved() {
        assert_eq!(
            reader_after_inline_send(false, true),
            ReaderAfterInlineSend::ShowExisting
        );
        assert_eq!(
            reader_after_inline_send(false, false),
            ReaderAfterInlineSend::ShowExisting
        );
        assert_eq!(
            reader_after_inline_send(true, true),
            ReaderAfterInlineSend::OpenSelected
        );
        assert_eq!(
            reader_after_inline_send(true, false),
            ReaderAfterInlineSend::Clear
        );
    }

    #[test]
    fn unsent_draft_stays_in_open_window() {
        assert_eq!(
            unsent_draft_restore(true, true, true),
            UnsentDraftRestore::ExistingWindow
        );
        assert_eq!(
            unsent_draft_restore(true, true, false),
            UnsentDraftRestore::ExistingWindow
        );
    }

    #[test]
    fn unsent_draft_opens_beside_a_new_compose() {
        assert_eq!(
            unsent_draft_restore(false, false, true),
            UnsentDraftRestore::NewWindow
        );
    }

    #[test]
    fn unsent_window_draft_reopens_a_window() {
        assert_eq!(
            unsent_draft_restore(false, true, false),
            UnsentDraftRestore::NewWindow
        );
    }

    #[test]
    fn unsent_draft_returns_to_inline_composer_when_it_is_free() {
        assert_eq!(
            unsent_draft_restore(false, false, false),
            UnsentDraftRestore::Inline
        );
    }

    #[test]
    fn sent_progress_does_not_close_a_composer_already_released() {
        assert!(!sent_should_close_composer(true));
        assert!(sent_should_close_composer(false));
    }

    #[test]
    fn sent_toast_names_from_account() {
        assert_eq!(
            sent_toast_title(Some("admin@alcedo.email")),
            "Message sent from admin@alcedo.email"
        );
        assert_eq!(sent_toast_title(None), "Message sent");
        assert_eq!(sent_toast_title(Some("")), "Message sent");
    }
}
