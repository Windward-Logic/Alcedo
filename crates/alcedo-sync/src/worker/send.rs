//! SMTP send: compose validation, From/SMTP resolution, send and post-send cleanup.
#![allow(clippy::too_many_arguments)]

use super::auth::{forget_rejected_token, smtp_auth};
use super::drafts::{delete_imap_draft, drafts_folder, imap_draft_has_server_uid};
use super::pgp::send_pgp_compose;
use super::sent_copy::{file_sent_copy, publish_local_sent_copy};
use super::*;

pub(super) fn validate_compose_draft(draft: &mut ComposeDraft) -> Result<()> {
    use alcedo_core::{MAX_COMPOSE_INLINES, content_id_is_safe, filename_is_safe};
    use alcedo_store::attachments::outgoing_attachment_mime;

    // No size cap here: the composer warns, and the SMTP server rejects
    // anything over its own limit with a clear error.
    if draft.inlines.len() > MAX_COMPOSE_INLINES {
        return Err(Error::Other("Too many inline images (max 10)".to_string()));
    }

    for attachment in &mut draft.attachments {
        if !filename_is_safe(&attachment.filename) {
            return Err(Error::Store("filename not allowed".to_string()));
        }
        attachment.mime = outgoing_attachment_mime(&attachment.filename, &attachment.bytes);
    }

    for inline in &mut draft.inlines {
        if !filename_is_safe(&inline.filename) {
            return Err(Error::Store("filename not allowed".to_string()));
        }
        if !content_id_is_safe(&inline.content_id) {
            return Err(Error::Store("invalid inline content-id".to_string()));
        }
        inline.mime = outgoing_attachment_mime(&inline.filename, &inline.bytes);
    }

    Ok(())
}

pub(super) async fn maybe_approve_contact(
    draft: &ComposeDraft,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
) {
    let Some(email) = draft.approve_contact_email.as_deref() else {
        return;
    };
    if email.trim().is_empty() {
        return;
    }
    if let Err(err) = store.allow_sender(email, "").await {
        warn!(error = %err, %email, "failed to approve contact after send");
        return;
    }
    match store.contacts_snapshot().await {
        Ok((contacts, groups)) => {
            let _ = events.send(SyncEvent::ContactsChanged { contacts, groups });
        }
        Err(err) => warn!(error = %err, "failed to snapshot contacts after approving a contact"),
    }
    if let Err(err) = refresh_virtual_inboxes(store, events).await {
        warn!(error = %err, "failed to refresh inboxes after approving a contact");
    }
    if let Err(err) = emit_unread_counts(store, events).await {
        warn!(error = %err, "failed to emit unread counts after approving a contact");
    }
}

pub(super) async fn maybe_collect_outgoing(
    account: &Account,
    draft: &ComposeDraft,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
) {
    if !draft.collect_outgoing_contacts {
        return;
    }

    let mut addresses = Vec::new();
    for field in [&draft.to, &draft.cc, &draft.bcc] {
        for token in field {
            if let Some(addr) = parse_address_token(token) {
                addresses.push(addr);
            }
        }
    }

    if let Err(err) = store.collect_outgoing(&account.email, &addresses).await {
        warn!(error = %err, "failed to collect outgoing contacts");
        return;
    }

    match store.contacts_snapshot().await {
        Ok((contacts, groups)) => {
            let _ = events.send(SyncEvent::ContactsChanged { contacts, groups });
        }
        Err(err) => warn!(error = %err, "failed to snapshot contacts after collect"),
    }
}

pub(super) async fn emit_signature(
    account: &Account,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
) {
    let signature = match store.get_signature(account.id).await {
        Ok(signature) => signature,
        Err(err) => {
            warn!(error = %err, "failed to load signature");
            return;
        }
    };
    let _ = events.send(SyncEvent::SignaturesUpdated {
        account: account.id,
        signature,
    });
}

pub(super) async fn emit_identities(
    account: &Account,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
) {
    let identities = match store.list_identities(account.id).await {
        Ok(identities) => identities,
        Err(err) => {
            warn!(error = %err, "failed to list identities");
            return;
        }
    };
    let _ = events.send(SyncEvent::IdentitiesUpdated {
        account: account.id,
        identities,
    });
}

pub(crate) fn should_delete_draft_after_send(draft: &ComposeDraft) -> Option<&ImapDraftRef> {
    draft
        .imap_draft
        .as_ref()
        .filter(|imap| !imap.message_id.trim().is_empty() || imap_draft_has_server_uid(imap))
}

pub(super) async fn resolve_compose_from(
    account: &Account,
    draft: &ComposeDraft,
    store: &StoreHandle,
) -> (String, String) {
    let identities = store.list_identities(account.id).await.unwrap_or_default();
    alcedo_core::mime_from_for_draft(
        &account.email,
        &account.display_name,
        draft.identity_id,
        draft.custom_from.as_deref(),
        &identities,
    )
}

pub(super) async fn envelope_email_for_compose(
    account: &Account,
    draft: &ComposeDraft,
    from_email: &str,
    identities: &[AccountIdentity],
    store: &StoreHandle,
) -> Result<String> {
    let saved_identity = draft.identity_id.is_some_and(|id| {
        identities
            .iter()
            .any(|identity| identity.id == id && identity.email.eq_ignore_ascii_case(from_email))
    });
    let policy = store.get_smtp_envelope_policy(account.id).await?;
    Ok(smtp_mail_from(policy, &account.email, from_email, saved_identity).to_string())
}

pub(super) async fn smtp_options_for_compose(
    account: &Account,
    draft: &ComposeDraft,
    store: &StoreHandle,
    token_cache: &mut Option<String>,
) -> Result<SmtpOptions> {
    let auth = smtp_auth(account, token_cache).await?;
    let identities = store.list_identities(account.id).await.unwrap_or_default();
    let (from_email, mut from_display_name) = alcedo_core::mime_from_for_draft(
        &account.email,
        &account.display_name,
        draft.identity_id,
        draft.custom_from.as_deref(),
        &identities,
    );
    if draft
        .reaction_emoji
        .as_deref()
        .is_some_and(is_single_reaction_emoji)
    {
        from_display_name = reaction_from_display_name(&account.sender_name, &from_display_name);
    }
    let envelope_email =
        envelope_email_for_compose(account, draft, &from_email, &identities, store).await?;
    Ok(SmtpOptions {
        host: account.smtp_host.clone(),
        port: account.smtp_port,
        starttls: account.smtp_starttls,
        email: account.email.clone(),
        username: match &account.auth {
            AuthMethod::Password => account.smtp_login_name().to_string(),
            AuthMethod::OAuth2 { .. } => account.email.clone(),
        },
        display_name: account.display_name.clone(),
        from_email: Some(from_email),
        from_display_name: Some(from_display_name),
        envelope_email: Some(envelope_email),
        auth,
    })
}

pub(super) async fn delete_imap_draft_after_send(
    state: &mut WorkerImapState,
    account: &Account,
    imap: &ImapDraftRef,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
    server_rules: &ServerRulesSession,
) -> Result<()> {
    let folder_id = if imap.folder_id.0 != 0 {
        imap.folder_id
    } else {
        let Ok(folders) = store.list_folders(account.id).await else {
            return Ok(());
        };
        let Some(drafts) = drafts_folder(&folders) else {
            return Ok(());
        };
        drafts.id
    };
    delete_imap_draft(
        state,
        account,
        folder_id,
        imap.uid,
        &imap.message_id,
        store,
        events,
        server_rules,
    )
    .await
}

pub(super) async fn finish_send_on_imap(
    state: &mut WorkerImapState,
    account: &Account,
    draft: ComposeDraft,
    rfc822: Vec<u8>,
    message_id_hdr: String,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
    _server_rules: &ServerRulesSession,
) -> Result<Option<WorkerCommand>> {
    if let Some(imap) = draft.imap_draft.as_ref() {
        state.note_sent_compose_session(&imap.message_id);
    }
    // The SMTP bytes are already the message. Show them before APPEND so Sent
    // and the reader do not wait on the server accepting a large attachment.
    let placeholder =
        publish_local_sent_copy(account, &draft, &rfc822, &message_id_hdr, store, events).await;
    Ok(Some(WorkerCommand::FileSentOnImap {
        draft,
        rfc822,
        message_id_hdr,
        placeholder,
    }))
}

pub(super) async fn file_sent_on_imap(
    state: &mut WorkerImapState,
    account: &Account,
    draft: ComposeDraft,
    rfc822: Vec<u8>,
    message_id_hdr: String,
    placeholder: Option<LocalSentRow>,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
    server_rules: &ServerRulesSession,
) -> Result<()> {
    if let Some(imap) = should_delete_draft_after_send(&draft).cloned()
        && let Err(err) =
            delete_imap_draft_after_send(state, account, &imap, store, events, server_rules).await
    {
        if is_recoverable_disconnect(&err) || is_auth_failure(&err) {
            return Err(err);
        }
        warn!(error = %err, "failed to delete IMAP draft after send");
    }
    file_sent_copy(
        state,
        account,
        &rfc822,
        &message_id_hdr,
        placeholder,
        store,
        events,
    )
    .await
}

pub(super) async fn smtp_send_compose(
    account: &Account,
    mut draft: ComposeDraft,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
    token_cache: &mut Option<String>,
    auth_notified: &mut bool,
) -> Result<SmtpSendResult> {
    let placeholder_id = MessageId(0);
    let _ = events.send(SyncEvent::SendProgress {
        id: placeholder_id,
        state: SendState::Sending,
    });

    let pixel_url = draft.tracking_pixel_url.clone();
    if let Some(url) = pixel_url.as_deref()
        && let Err(err) = validate_https_pixel_url(url)
    {
        let _ = events.send(SyncEvent::SendProgress {
            id: placeholder_id,
            state: SendState::Failed(err.to_string()),
        });
        return Err(err);
    }

    if let Err(err) = validate_compose_draft(&mut draft) {
        let _ = events.send(SyncEvent::SendProgress {
            id: placeholder_id,
            state: SendState::Failed(err.to_string()),
        });
        return Err(err);
    }

    let is_reaction = draft.reaction_emoji.is_some();
    if is_reaction
        && let Some(hdr) = draft
            .in_reply_to
            .as_deref()
            .and_then(alcedo_core::bare_reaction_target_hdr)
        && let Ok(Some((target_id, _))) = store.reaction_target_message(account.id, &hdr).await
        && let Ok(Some(target)) = store.get_message(target_id).await
        && let Some(reason) = alcedo_core::encrypted_reaction_block(
            target.pgp_status.as_ref().is_some_and(|pgp| pgp.encrypted),
        )
    {
        let err = Error::Other(reason.to_string());
        let _ = events.send(SyncEvent::SendProgress {
            id: placeholder_id,
            state: SendState::Failed(err.to_string()),
        });
        return Err(err);
    }
    if !is_reaction
        && let Err(err) =
            crate::pgp::attach_outbound_autocrypt(account.id, &account.email, &mut draft, store)
                .await
    {
        let _ = events.send(SyncEvent::SendProgress {
            id: placeholder_id,
            state: SendState::Failed(err.to_string()),
        });
        return Err(err);
    }

    let pixel = if is_reaction {
        None
    } else {
        pixel_url.as_deref()
    };
    let outbound = render_outbound(draft.format, &draft.body_text, pixel);
    let bodies = RenderedBodies {
        plain: outbound.plain,
        html: outbound.html,
    };

    let mut opts = match smtp_options_for_compose(account, &draft, store, token_cache).await {
        Ok(opts) => opts,
        Err(err) => {
            let _ = events.send(SyncEvent::SendProgress {
                id: placeholder_id,
                state: SendState::Failed(err.to_string()),
            });
            return Err(err);
        }
    };

    let send_result = if draft.sign || draft.encrypt {
        send_pgp_compose(account, &draft, &bodies, store, opts).await
    } else {
        send_rendered(opts, &draft, &bodies).await
    };

    let (message_id_hdr, rfc822) = match send_result {
        Ok(sent) => sent,
        Err(err) if is_auth_failure(&err) => {
            forget_rejected_token(account, token_cache).await;
            opts = match smtp_options_for_compose(account, &draft, store, token_cache).await {
                Ok(opts) => opts,
                Err(err) => {
                    let _ = events.send(SyncEvent::SendProgress {
                        id: placeholder_id,
                        state: SendState::Failed(err.to_string()),
                    });
                    return Err(err);
                }
            };
            let outbound = render_outbound(draft.format, &draft.body_text, pixel);
            let bodies = RenderedBodies {
                plain: outbound.plain,
                html: outbound.html,
            };
            let retry = if draft.sign || draft.encrypt {
                send_pgp_compose(account, &draft, &bodies, store, opts).await
            } else {
                send_rendered(opts, &draft, &bodies).await
            };
            match retry {
                Ok(sent) => sent,
                Err(retry_err) => {
                    let _ = events.send(SyncEvent::SendProgress {
                        id: placeholder_id,
                        state: SendState::Failed(retry_err.to_string()),
                    });
                    if is_auth_failure(&retry_err) && !*auth_notified {
                        let _ = events.send(SyncEvent::AuthRequired {
                            account: account.id,
                            message: retry_err.to_string(),
                        });
                        *auth_notified = true;
                    }
                    return Err(retry_err);
                }
            }
        }
        Err(err) => {
            let _ = events.send(SyncEvent::SendProgress {
                id: placeholder_id,
                state: SendState::Failed(err.to_string()),
            });
            return Err(err);
        }
    };

    let _ = events.send(SyncEvent::SendProgress {
        id: placeholder_id,
        state: SendState::Sent {
            message_id_hdr: message_id_hdr.clone(),
        },
    });
    maybe_collect_outgoing(account, &draft, store, events).await;
    maybe_approve_contact(&draft, store, events).await;
    Ok(SmtpSendResult {
        draft,
        rfc822,
        message_id_hdr,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use alcedo_core::{AccountId, FolderId};
    use uuid::Uuid;

    #[test]
    fn should_delete_draft_after_send_when_imap_ref_set() {
        use alcedo_core::ImapDraftRef;

        let draft = ComposeDraft {
            account_id: AccountId(Uuid::nil()),
            to: Vec::new(),
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: String::new(),
            body_text: String::new(),
            format: Default::default(),
            request_mdn: false,
            tracking_pixel_url: None,
            attachments: Vec::new(),
            inlines: Vec::new(),
            collect_outgoing_contacts: true,
            in_reply_to: None,
            references: None,
            thread_id: None,
            approve_contact_email: None,
            sign: false,
            encrypt: false,
            autocrypt_header: None,
            identity_id: None,
            custom_from: None,
            imap_draft: Some(ImapDraftRef {
                folder_id: FolderId(9),
                uid: 42,
                uidvalidity: 1,
                message_id: "<draft@example.com>".to_string(),
                last_internaldate: None,
            }),
            reaction_emoji: None,
        };
        assert_eq!(
            should_delete_draft_after_send(&draft).map(|imap| (imap.folder_id, imap.uid)),
            Some((FolderId(9), 42))
        );
        assert!(
            should_delete_draft_after_send(&ComposeDraft {
                imap_draft: None,
                ..draft.clone()
            })
            .is_none()
        );
        assert!(
            should_delete_draft_after_send(&ComposeDraft {
                imap_draft: Some(ImapDraftRef {
                    folder_id: FolderId(0),
                    uid: 0,
                    uidvalidity: 0,
                    message_id: "<session@example.com>".to_string(),
                    last_internaldate: None,
                }),
                ..draft
            })
            .is_some()
        );
    }
}
