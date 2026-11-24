//! Compose drafts: local autosave, crash recovery and IMAP Drafts sync.

use alcedo_core::QueuedSendId;
use alcedo_core::compose_status::{DraftStatus, queued_send_recovery_text};
use alcedo_i18n::{ntr, tr, trf};

use super::*;

/// Response id for closing a recovery dialog without choosing (Escape).
const RECOVERY_DECIDE_LATER: &str = "later";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RecoveryChoice {
    Restore,
    Discard,
    /// The dialog was dismissed. Unsent mail is only deleted on an explicit
    /// Discard, so it is kept and offered again on the next launch.
    DecideLater,
}

pub(super) fn recovery_choice(response: &str) -> RecoveryChoice {
    match response {
        "restore" => RecoveryChoice::Restore,
        "discard" => RecoveryChoice::Discard,
        _ => RecoveryChoice::DecideLater,
    }
}

impl MailModel {
    pub(super) fn update_drafts(&mut self, msg: MailMsg, sender: ComponentSender<MailModel>) {
        match msg {
            MailMsg::RestoreSavedComposeDrafts(drafts) => {
                if drafts.is_empty() {
                    return;
                }
                let (heading, body) = if drafts.len() == 1 {
                    (
                        tr!("Restore unsent message?"),
                        tr!("Alcedo saved a draft from your last session."),
                    )
                } else {
                    (
                        ntr!(
                            "Restore {n} unsent message?",
                            "Restore {n} unsent messages?",
                            drafts.len()
                        ),
                        tr!("Alcedo saved drafts from your last session."),
                    )
                };
                let dialog = adw::AlertDialog::builder()
                    .heading(heading)
                    .body(body)
                    .build();
                dialog.add_response("discard", &tr!("Discard"));
                dialog.add_response("restore", &tr!("Restore"));
                dialog.set_response_appearance("restore", adw::ResponseAppearance::Suggested);
                dialog.set_default_response(Some("restore"));
                dialog.set_close_response(RECOVERY_DECIDE_LATER);
                let sender = sender.clone();
                dialog.choose(
                    Some(&self.window),
                    None::<&gtk::gio::Cancellable>,
                    move |response| match recovery_choice(response.as_str()) {
                        RecoveryChoice::Restore => {
                            sender.input(MailMsg::ApplyRestoredComposeDrafts(drafts));
                        }
                        RecoveryChoice::Discard => forget_recovered_drafts(&drafts),
                        RecoveryChoice::DecideLater => {}
                    },
                );
            }
            MailMsg::RestoreQueuedSends(sends) => {
                if sends.is_empty() {
                    return;
                }
                let (heading, body) = queued_send_recovery_text(sends.len());
                let dialog = adw::AlertDialog::builder()
                    .heading(heading)
                    .body(body)
                    .build();
                dialog.add_response("discard", &tr!("Discard"));
                dialog.add_response("restore", &tr!("Restore"));
                dialog.set_response_appearance("restore", adw::ResponseAppearance::Suggested);
                dialog.set_default_response(Some("restore"));
                dialog.set_close_response(RECOVERY_DECIDE_LATER);
                let ids: Vec<QueuedSendId> = sends.iter().map(|q| q.id).collect();
                let sends_for_restore = sends.clone();
                let cmd = self.cmd.clone();
                let sender = sender.clone();
                dialog.choose(
                    Some(&self.window),
                    None::<&gtk::gio::Cancellable>,
                    move |response| {
                        let choice = recovery_choice(response.as_str());
                        if choice == RecoveryChoice::DecideLater {
                            return;
                        }
                        // Restored sends reopen as composers, which autosave
                        // under their own slots; either way the queue entry
                        // is finished with.
                        for id in &ids {
                            cmd.emit(UiCommand::ForgetQueuedSend { id: *id });
                        }
                        if choice == RecoveryChoice::Restore {
                            sender.input(MailMsg::ApplyQueuedSends(sends_for_restore));
                        }
                    },
                );
            }
            MailMsg::ApplyQueuedSends(sends) => {
                for send in sends {
                    if !self
                        .accounts
                        .iter()
                        .any(|account| account.id == send.draft.account_id)
                    {
                        continue;
                    }
                    let mut draft = send.draft;
                    self.restore_signature_inlines(&mut draft);
                    self.open_detached_composer(
                        draft,
                        &tr!("Compose"),
                        &sender,
                        DetachedComposerFocus::None,
                    );
                }
            }
            MailMsg::ApplyRestoredComposeDrafts(recovered) => {
                // Restored composers re-save under this session's slots.
                forget_recovered_drafts(&recovered);
                let mut missing: Vec<String> = Vec::new();
                let mut inline_used = false;
                for item in &recovered {
                    let mut draft = item.draft.clone();
                    if !self
                        .accounts
                        .iter()
                        .any(|account| account.id == draft.account_id)
                    {
                        continue;
                    }
                    missing.extend(item.missing_files.iter().cloned());
                    self.restore_signature_inlines(&mut draft);
                    if inline_used {
                        self.open_detached_composer(
                            draft,
                            &tr!("Compose"),
                            &sender,
                            DetachedComposerFocus::None,
                        );
                        continue;
                    }
                    inline_used = true;
                    if self.selected_account != Some(draft.account_id) {
                        sender.input(MailMsg::SelectAccount(draft.account_id));
                    }
                    self.cmd.emit(UiCommand::Compose(draft.clone()));
                    self.show_composer();
                    self.bind_composer_from(&self.composer, draft.account_id);
                    apply_compose_draft(&self.composer, &draft, false);
                    present_main_window();
                }
                if !missing.is_empty() {
                    let message = trf!("Re-attach files: {files}", files = missing.join(", "));
                    self.toasts.add_toast(crate::toast::plain(&message));
                }
            }
            MailMsg::ComposeDraftJsonPersist => {
                if self.mail_content_pane == MailContentPane::Composer
                    && self.bind_resolved_composer_from(&self.composer).is_some()
                    && composer_has_draft(&self.composer)
                {
                    self.composer
                        .autosave_local(&compose_draft_from_widgets(&self.composer));
                }
                for window in self.composer_windows.values() {
                    if self.bind_resolved_composer_from(&window.widgets).is_some()
                        && composer_has_draft(&window.widgets)
                    {
                        window
                            .widgets
                            .autosave_local(&compose_draft_from_widgets(&window.widgets));
                    }
                }
            }
            MailMsg::ComposeDraftImapPersist => {
                self.emit_save_imap_draft();
            }
            _ => unreachable!("message not routed to drafts"),
        }
    }

    pub(super) fn persist_inline_compose_draft(&self, incoming: Option<&Envelope>) {
        if incoming.is_some_and(|env| self.is_editing_imap_draft(env)) {
            return;
        }
        if self.bind_resolved_composer_from(&self.composer).is_none() {
            return;
        }
        if !composer_has_draft(&self.composer) {
            self.composer.clear_local_autosave();
            return;
        }
        let draft = compose_draft_from_widgets(&self.composer);
        self.composer.autosave_local(&draft);
        if self.pending_send_draft.is_none() {
            self.emit_save_imap_draft_inner(&self.composer);
        }
    }

    pub(super) fn is_editing_listed_draft(&self, id: MessageId) -> bool {
        self.envelope_for(id)
            .is_some_and(|env| self.is_imap_draft_envelope(env) && self.is_editing_imap_draft(env))
    }

    pub(super) fn is_editing_imap_draft(&self, envelope: &Envelope) -> bool {
        let envelope_message_id = self
            .threading_hdrs
            .get(&envelope.id)
            .and_then(|(hdr, _)| hdr.as_deref());
        is_same_imap_draft_session(
            self.mail_content_pane == MailContentPane::Composer,
            self.composer.imap_draft.borrow().as_ref(),
            self.pending_imap_draft_open,
            self.selected,
            envelope,
            envelope_message_id,
        )
    }

    pub(super) fn emit_save_imap_draft(&self) {
        let inline_is_sending = self.pending_send_draft.is_some()
            && self.pending_send_window.is_none()
            && !self.send_surface_released;
        if self.mail_content_pane == MailContentPane::Composer && !inline_is_sending {
            self.emit_save_imap_draft_inner(&self.composer);
        }
        for window in self.composer_windows.values() {
            if !window.is_sending() {
                self.emit_save_imap_draft_inner(&window.widgets);
            }
        }
    }

    pub(super) fn emit_save_imap_draft_inner(&self, widgets: &ComposerWidgets) {
        if self.bind_resolved_composer_from(widgets).is_none() {
            return;
        }
        if !composer_has_draft(widgets) || widgets.autosave_is_suspended() {
            return;
        }
        let cmd = self.cmd.clone();
        let widgets_for_flush = widgets.clone();
        flush_compose_draft_then(widgets, move |draft| {
            if !flushed_draft_still_current(
                widgets_for_flush.autosave_is_suspended(),
                draft.account_id,
                widgets_for_flush.from_account_id(),
            ) {
                return;
            }
            // Compose, then click elsewhere, must not file a signature-only draft.
            if is_untouched_new_compose(&draft, widgets_for_flush.draft_saved_account()) {
                return;
            }
            widgets_for_flush.note_draft_saved_to(draft.account_id);
            cmd.emit(UiCommand::SaveImapDraft(draft));
        });
    }

    pub(super) fn is_imap_draft_envelope(&self, envelope: &Envelope) -> bool {
        if envelope.draft {
            return true;
        }
        self.folders
            .get(&envelope.account_id)
            .and_then(|folders| {
                folders
                    .iter()
                    .find(|folder| folder.id == envelope.folder_id)
            })
            .is_some_and(|folder| matches!(folder.special, SpecialUse::Drafts))
    }

    pub(super) fn should_hold_compose_draft_list(&self, folder_id: FolderId) -> bool {
        self.mail_content_pane == MailContentPane::Composer
            && self.folder_is_special(folder_id, &SpecialUse::Drafts)
            && matches!(self.mail_view, MailView::Folder(id) if id == folder_id)
            && composer_has_draft(&self.composer)
            && self
                .composer
                .imap_draft
                .borrow()
                .as_ref()
                .is_some_and(|imap| {
                    imap_draft_has_server_uid(imap)
                        && (imap.folder_id == folder_id || imap.folder_id.0 == 0)
                })
    }

    pub(super) fn compose_draft_row_matches(
        &self,
        imap: &ImapDraftRef,
        envelope: &Envelope,
    ) -> bool {
        imap_draft_ref_matches_envelope(imap, envelope)
            || self
                .threading_hdrs
                .get(&envelope.id)
                .and_then(|(hdr, _)| hdr.as_deref())
                .is_some_and(|hdr| imap_draft_message_id_eq(&imap.message_id, hdr))
    }

    /// Keep the in-progress compose draft visible when IMAP APPEND rotates UIDs.
    pub(super) fn merge_compose_draft_pin(
        &self,
        folder_id: FolderId,
        mut envelopes: Vec<Envelope>,
    ) -> Vec<Envelope> {
        if !self.should_hold_compose_draft_list(folder_id) {
            return envelopes;
        }
        let imap = self.composer.imap_draft.borrow().clone().unwrap();
        if envelopes
            .iter()
            .any(|env| self.compose_draft_row_matches(&imap, env))
        {
            return envelopes;
        }
        let Some(mut pinned) = self
            .envelopes
            .iter()
            .find(|env| {
                self.is_imap_draft_envelope(env) && self.compose_draft_row_matches(&imap, env)
            })
            .cloned()
        else {
            return envelopes;
        };
        pinned.uid = imap.uid;
        pinned.folder_id = imap.folder_id;
        envelopes.push(pinned);
        envelopes.sort_by(|a, b| b.date.cmp(&a.date));
        envelopes
    }

    pub(super) fn stamp_imap_draft_on_widgets(
        widgets: &ComposerWidgets,
        account_id: AccountId,
        saved: &ImapDraftRef,
    ) {
        if widgets.from_account_id() != account_id {
            return;
        }
        let should_stamp = match &*widgets.imap_draft.borrow() {
            None => true,
            Some(old) => imap_draft_message_id_eq(&old.message_id, &saved.message_id),
        };
        if should_stamp {
            *widgets.imap_draft.borrow_mut() = Some(saved.clone());
            widgets.note_draft_saved_to(account_id);
        }
    }

    pub(super) fn stamp_imap_draft_ref(&self, draft: &ComposeDraft) {
        if let Some(saved) = draft.imap_draft.as_ref() {
            Self::stamp_imap_draft_on_widgets(&self.composer, draft.account_id, saved);
            for window in self.composer_windows.values() {
                Self::stamp_imap_draft_on_widgets(&window.widgets, draft.account_id, saved);
            }
        }
    }

    pub(super) fn apply_imap_draft_ready(
        &mut self,
        message_id: MessageId,
        draft: ComposeDraft,
        sender: &ComponentSender<MailModel>,
    ) {
        if self.pending_popout_draft == Some(message_id) {
            self.pending_popout_draft = None;
            self.open_detached_composer(draft, &tr!("Draft"), sender, DetachedComposerFocus::Body);
            return;
        }
        if should_skip_imap_draft_reapply(
            self.mail_content_pane == MailContentPane::Composer,
            composer_has_draft(&self.composer),
            self.composer.imap_draft.borrow().as_ref(),
            draft.imap_draft.as_ref(),
        ) {
            if self.pending_imap_draft_open == Some(message_id) {
                self.pending_imap_draft_open = None;
            }
            if self.pending_popout_draft == Some(message_id) {
                self.pending_popout_draft = None;
            }
            return;
        }
        if self.pending_imap_draft_open == Some(message_id) {
            self.pending_imap_draft_open = None;
            // Moved on to another message while the fetch was in flight; a
            // re-select reopens rather than yanking the pane back.
            if self.selected.is_some_and(|selected| selected != message_id) {
                return;
            }
            // Message-ID keyed on the row so later selections still recognise
            // this session after an autosave APPEND rotates the UID.
            if let Some(imap) = draft.imap_draft.as_ref() {
                let references = self
                    .threading_hdrs
                    .get(&message_id)
                    .and_then(|(_, refs)| refs.clone());
                self.threading_hdrs
                    .insert(message_id, (Some(imap.message_id.clone()), references));
            }
            self.cmd.emit(UiCommand::Compose(draft.clone()));
            self.show_composer();
            self.bind_composer_from(&self.composer, draft.account_id);
            apply_compose_draft(&self.composer, &draft, false);
            present_main_window();
        }
    }

    pub(super) fn clear_autosave_for_imap_draft(&self, saved: &ComposeDraft) {
        let Some(imap) = saved.imap_draft.as_ref() else {
            return;
        };
        let matches = |widgets: &ComposerWidgets| {
            widgets
                .imap_draft
                .borrow()
                .as_ref()
                .is_some_and(|current| current.message_id == imap.message_id)
        };
        if matches(&self.composer) {
            self.composer.clear_local_autosave();
            self.composer.set_draft_status(DraftStatus::SavedToServer);
        }
        for window in self.composer_windows.values() {
            if matches(&window.widgets) {
                window.widgets.clear_local_autosave();
                window.widgets.set_draft_status(DraftStatus::SavedToServer);
            }
        }
    }

    /// A Drafts-folder save failed. The event does not say which composer, so
    /// flag every open composer that still holds a draft.
    pub(super) fn mark_drafts_server_save_failed(&self) {
        if self.mail_content_pane == MailContentPane::Composer && composer_has_draft(&self.composer)
        {
            self.composer
                .set_draft_status(DraftStatus::ServerSaveFailed);
        }
        for window in self.composer_windows.values() {
            if composer_has_draft(&window.widgets) {
                window
                    .widgets
                    .set_draft_status(DraftStatus::ServerSaveFailed);
            }
        }
    }
}

/// Whether a save whose editor flush just finished may still go to the server.
/// The composer may have been sent or discarded meanwhile (autosave suspended),
/// or From moved to another account, whose old copy was just retired.
pub(super) fn flushed_draft_still_current(
    autosave_suspended: bool,
    draft_account: AccountId,
    composer_account: AccountId,
) -> bool {
    !autosave_suspended && draft_account == composer_account
}

pub(super) fn normalize_message_id(id: &str) -> String {
    let id = id.trim();
    let id = id.strip_prefix('<').unwrap_or(id);
    let id = id.strip_suffix('>').unwrap_or(id);
    id.trim().to_ascii_lowercase()
}

pub(super) fn imap_draft_message_id_eq(left: &str, right: &str) -> bool {
    let left = normalize_message_id(left);
    let right = normalize_message_id(right);
    !left.is_empty() && left == right
}

pub(super) fn imap_draft_ref_matches_envelope(imap: &ImapDraftRef, envelope: &Envelope) -> bool {
    imap.uid != 0
        && imap.uid == envelope.uid
        && (imap.folder_id == envelope.folder_id || imap.folder_id.0 == 0)
}

pub(super) fn is_same_imap_draft_session(
    composer_open: bool,
    composer_ref: Option<&ImapDraftRef>,
    pending_open: Option<MessageId>,
    _selected: Option<MessageId>,
    envelope: &Envelope,
    envelope_message_id: Option<&str>,
) -> bool {
    // A pending open is a live session even before the composer becomes the
    // visible pane; the draft is still in flight from the server.
    if pending_open == Some(envelope.id) {
        return true;
    }
    if !composer_open {
        return false;
    }
    let Some(imap) = composer_ref else {
        return false;
    };
    if imap_draft_ref_matches_envelope(imap, envelope) {
        return true;
    }
    envelope_message_id.is_some_and(|hdr| imap_draft_message_id_eq(&imap.message_id, hdr))
}

pub(super) fn should_skip_imap_draft_reapply(
    composer_open: bool,
    composer_has_content: bool,
    open_ref: Option<&ImapDraftRef>,
    incoming: Option<&ImapDraftRef>,
) -> bool {
    if !composer_open || !composer_has_content {
        return false;
    }
    match (open_ref, incoming) {
        (Some(open), Some(incoming)) => {
            imap_draft_message_id_eq(&open.message_id, &incoming.message_id)
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mail::test_support::envelope;

    #[test]
    fn dismissing_a_recovery_dialog_never_discards_unsent_mail() {
        assert_eq!(recovery_choice("restore"), RecoveryChoice::Restore);
        assert_eq!(recovery_choice("discard"), RecoveryChoice::Discard);
        // Escape, the window close button, or any id the toolkit reports.
        assert_eq!(
            recovery_choice(RECOVERY_DECIDE_LATER),
            RecoveryChoice::DecideLater
        );
        assert_eq!(recovery_choice("close"), RecoveryChoice::DecideLater);
        assert_eq!(recovery_choice(""), RecoveryChoice::DecideLater);
    }

    fn draft_ref(folder: i64, uid: u32, message_id: &str) -> ImapDraftRef {
        ImapDraftRef {
            folder_id: FolderId(folder),
            uid,
            uidvalidity: 1,
            message_id: message_id.to_string(),
            last_internaldate: None,
        }
    }

    #[test]
    fn signature_only_compose_stays_off_drafts_until_it_has_a_server_copy() {
        let draft: ComposeDraft = serde_json::from_value(serde_json::json!({
            "account_id": "00000000-0000-0000-0000-000000000001", "to": [], "cc": [], "bcc": [],
            "subject": "", "body_text": "-- \nCarol", "format": "plain",
            "request_mdn": false, "tracking_pixel_url": null
        }))
        .unwrap();
        assert!(is_untouched_new_compose(&draft, None));
        // A copy already on the server keeps receiving saves, or it would go stale.
        assert!(!is_untouched_new_compose(
            &draft,
            Some(AccountId(Uuid::from_u128(1)))
        ));
        let typed = ComposeDraft {
            body_text: "Hi\n-- \nCarol".to_string(),
            ..draft
        };
        assert!(!is_untouched_new_compose(&typed, None));
    }

    #[test]
    fn a_flush_finishing_after_discard_or_account_switch_is_not_saved() {
        let first = AccountId(Uuid::from_u128(1));
        let second = AccountId(Uuid::from_u128(2));
        assert!(flushed_draft_still_current(false, first, first));
        assert!(!flushed_draft_still_current(true, first, first));
        assert!(!flushed_draft_still_current(false, first, second));
    }

    #[test]
    fn imap_draft_message_id_eq_normalizes_brackets() {
        assert!(imap_draft_message_id_eq(
            "<draft@example.com>",
            "draft@example.com"
        ));
        assert!(imap_draft_message_id_eq(
            " <Draft@Example.com> ",
            "<draft@example.com>"
        ));
        assert!(!imap_draft_message_id_eq(
            "<draft@example.com>",
            "<other@example.com>"
        ));
    }

    #[test]
    fn same_draft_session_skips_open_when_composer_already_has_it() {
        let env = Envelope {
            id: MessageId(7),
            draft: true,
            uid: 42,
            folder_id: FolderId(9),
            ..envelope("me@example.com", &[], &[])
        };
        let imap = draft_ref(9, 42, "<draft@example.com>");
        assert!(is_same_imap_draft_session(
            true,
            Some(&imap),
            None,
            None,
            &env,
            None,
        ));
        assert!(is_same_imap_draft_session(
            true,
            None,
            Some(MessageId(7)),
            None,
            &env,
            None,
        ));
        assert!(is_same_imap_draft_session(
            true,
            Some(&draft_ref(9, 0, "<draft@example.com>")),
            None,
            None,
            &env,
            Some("draft@example.com"),
        ));
        assert!(!is_same_imap_draft_session(
            true,
            None,
            None,
            Some(MessageId(7)),
            &env,
            None,
        ));
        // The pane is still the reader placeholder until ImapDraftReady lands.
        assert!(is_same_imap_draft_session(
            false,
            None,
            Some(MessageId(7)),
            None,
            &env,
            None,
        ));
        assert!(!is_same_imap_draft_session(
            false,
            Some(&imap),
            None,
            None,
            &env,
            None,
        ));
        assert!(!is_same_imap_draft_session(
            true,
            Some(&draft_ref(9, 99, "<other@example.com>")),
            None,
            None,
            &env,
            Some("<draft@example.com>"),
        ));
    }

    #[test]
    fn skip_imap_draft_reapply_only_for_in_progress_same_id() {
        let open = draft_ref(9, 42, "<draft@example.com>");
        let incoming = draft_ref(9, 43, "draft@example.com");
        assert!(should_skip_imap_draft_reapply(
            true,
            true,
            Some(&open),
            Some(&incoming),
        ));
        assert!(!should_skip_imap_draft_reapply(
            true,
            false,
            Some(&open),
            Some(&incoming),
        ));
        assert!(!should_skip_imap_draft_reapply(
            true,
            true,
            Some(&open),
            Some(&draft_ref(9, 43, "<other@example.com>")),
        ));
    }

    #[test]
    fn placeholder_imap_draft_is_not_server_saved() {
        use crate::composer::{imap_draft_has_server_uid, placeholder_imap_draft};

        let placeholder = placeholder_imap_draft("<session@example.com>".to_string());
        assert!(!imap_draft_has_server_uid(&placeholder));
        assert!(imap_draft_has_server_uid(&draft_ref(
            9,
            42,
            "<draft@example.com>"
        )));
    }
}
