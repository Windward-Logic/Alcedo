//! Opening messages: body fetch and caching, attachments, inline images, raw source and BodyReady events.

use super::drafts::open_imap_draft;
use super::mailbox_sync::{ensure_selected, folder_is_syncable};
use super::pgp::{IncomingPgp, process_incoming_pgp};
use super::reactions::maybe_persist_reaction_from_full;
use super::*;

pub(crate) fn stored_has_local_body(stored: &alcedo_store::StoredMessage) -> bool {
    stored.body_text.is_some() || stored.body_html.is_some()
}

/// Cached copy was decoded without mail-parser's charset tables (`full_encoding`),
/// so declared non-UTF-8 bytes became U+FFFD. Re-fetch and decode again.
pub(crate) fn stored_body_needs_charset_rescan(stored: &alcedo_store::StoredMessage) -> bool {
    if stored.pgp_status.as_ref().is_some_and(|pgp| pgp.encrypted) {
        return false;
    }
    contains_replacement_char(stored.body_text.as_deref())
        || contains_replacement_char(stored.body_html.as_deref())
        || stored.subject.contains('\u{FFFD}')
        || stored.snippet.contains('\u{FFFD}')
}

pub(super) fn contains_replacement_char(s: Option<&str>) -> bool {
    s.is_some_and(|t| t.contains('\u{FFFD}'))
}

pub(super) fn refresh_subject_from_full(
    envelope: &mut alcedo_core::Envelope,
    full: &alcedo_imap::FullMessage,
) {
    if envelope.subject.contains('\u{FFFD}')
        && !full.subject.is_empty()
        && !full.subject.contains('\u{FFFD}')
    {
        envelope.subject = full.subject.clone();
    }
}

/// Cached body with no downloadable file parts.
///
/// Incoming rules used to drop files (ZIP-sniffed `.docx`, old allowlists,
/// and similar) before they were persisted. The local-body fast path would
/// then skip IMAP forever — including when nothing was saved at all. Re-fetch
/// once (`claim_attachment_rescan`) so `save_attachments` can pick up the file.
pub(crate) fn needs_attachment_rescan(stored: &alcedo_store::StoredMessage) -> bool {
    stored_has_local_body(stored) && file_attachments_for(stored).is_empty()
}

pub(crate) fn needs_attachment_bytes(stored: &alcedo_store::StoredMessage) -> bool {
    stored.has_attachments && file_attachments_for(stored).is_empty()
}

pub(super) async fn cached_has_undecoded_binary_attachment(
    store: &StoreHandle,
    stored: &alcedo_store::StoredMessage,
) -> Result<bool> {
    for att in file_attachments_for(stored) {
        if att.size > u64::from(alcedo_imap::DISPLAY_FULL_MAX_BYTES) {
            continue;
        }
        let Ok(bytes) = store.attachment_bytes(&att.sha256).await else {
            continue;
        };
        if alcedo_imap::attachment_bytes_are_undecoded_base64(&att.mime, &bytes) {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(super) async fn drop_undecoded_binary_attachments(
    store: &StoreHandle,
    stored: &alcedo_store::StoredMessage,
) -> Result<bool> {
    let mut dropped = false;
    for att in file_attachments_for(stored) {
        if att.size > u64::from(alcedo_imap::DISPLAY_FULL_MAX_BYTES) {
            continue;
        }
        let Ok(bytes) = store.attachment_bytes(&att.sha256).await else {
            continue;
        };
        if alcedo_imap::attachment_bytes_are_undecoded_base64(&att.mime, &bytes) {
            store.delete_attachment(stored.id, &att.sha256).await?;
            dropped = true;
        }
    }
    Ok(dropped)
}

pub(crate) async fn emit_local_only_message_body(
    store: &StoreHandle,
    stored: &alcedo_store::StoredMessage,
    events: &flume::Sender<SyncEvent>,
) -> Result<()> {
    emit_body_ready_from_stored(store, stored, events).await;
    Ok(())
}

pub(crate) async fn emit_body_ready_from_stored(
    store: &StoreHandle,
    stored: &alcedo_store::StoredMessage,
    events: &flume::Sender<SyncEvent>,
) {
    emit_body_ready(store, stored, events, false, false).await;
}

pub(crate) async fn emit_body_ready_from_stored_with_pending(
    store: &StoreHandle,
    stored: &alcedo_store::StoredMessage,
    events: &flume::Sender<SyncEvent>,
    attachments_pending: bool,
) {
    emit_body_ready(store, stored, events, attachments_pending, false).await;
}

pub(super) fn attachments_pending_after_display(
    deferred_rest: bool,
    files_pending: bool,
    stored: &alcedo_store::StoredMessage,
    attachments_empty: bool,
) -> bool {
    attachments_empty && (files_pending || (deferred_rest && stored.has_attachments))
}

pub(super) async fn emit_body_ready(
    store: &StoreHandle,
    stored: &alcedo_store::StoredMessage,
    events: &flume::Sender<SyncEvent>,
    attachments_pending: bool,
    attachments_failed: bool,
) {
    let cids = cid_inlines_for(store, stored).await;
    let attachments = file_attachments_for(stored);
    let text = stored.body_text.clone().unwrap_or_default();
    let html = recover_html_body(stored.body_html.as_deref().unwrap_or(""), &text);
    let _ = events.send(SyncEvent::BodyReady {
        id: stored.id,
        html,
        text,
        cids,
        attachments,
        allow_remote_images: stored.allow_remote_images,
        list_unsubscribe: stored.list_unsubscribe.clone(),
        message_id_hdr: stored.message_id_hdr.clone(),
        in_reply_to: stored.in_reply_to.clone(),
        references_hdr: stored.references_hdr.clone(),
        pgp: stored.pgp_status.clone().unwrap_or_default(),
        attachments_pending,
        attachments_failed,
    });
}

pub(super) async fn emit_body_ready_failed(
    store: &StoreHandle,
    stored: &alcedo_store::StoredMessage,
    events: &flume::Sender<SyncEvent>,
) {
    emit_body_ready(store, stored, events, false, true).await;
}

pub(super) fn list_unsubscribe_for_persist(list: Option<ListUnsubscribe>) -> ListUnsubscribe {
    list.unwrap_or(ListUnsubscribe {
        https: None,
        mailto: None,
        one_click: false,
    })
}

pub(super) enum OpenMode {
    /// Paint text first. Attachment bytes become a follow-up command.
    Interactive,
    /// Fill the reader cache only. Do not queue an attachment download.
    Prefetch,
    /// Download whatever an interactive open left on the server.
    Remainder,
    /// Background offline cache. Like `Prefetch`, but never sends `BodyReady`.
    Background,
}

pub(super) async fn open_message(
    state: &mut WorkerImapState,
    account: &Account,
    id: MessageId,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
    mode: OpenMode,
) -> Result<Option<WorkerCommand>> {
    let mut stored = store
        .get_message(id)
        .await?
        .ok_or_else(|| Error::Store("message not found".to_string()))?;

    let folder = store
        .get_folder(stored.folder_id)
        .await?
        .ok_or_else(|| Error::Store("folder not found for message".to_string()))?;
    let emit_body = !matches!(mode, OpenMode::Background);
    if matches!(mode, OpenMode::Background)
        && (folder.special == SpecialUse::Drafts || stored.draft || !folder_is_syncable(&folder))
    {
        return Ok(None);
    }
    if folder.special == SpecialUse::Drafts || stored.draft {
        return open_imap_draft(state, account, id, &stored, &folder, store, events)
            .await
            .map(|()| None);
    }
    if !folder_is_syncable(&folder) {
        emit_local_only_message_body(store, &stored, events).await?;
        return Ok(None);
    }

    let body_already_saved = stored_has_local_body(&stored);
    let redecode_charset = stored_body_needs_charset_rescan(&stored);
    let schedule_remainder = matches!(mode, OpenMode::Interactive);
    let force_full = matches!(mode, OpenMode::Remainder);
    if body_already_saved && !redecode_charset && !force_full {
        if matches!(mode, OpenMode::Interactive)
            && !crate::runtime::attachment_rescan_is_claimed(id)
            && cached_has_undecoded_binary_attachment(store, &stored).await?
            && crate::runtime::claim_attachment_rescan(id)
        {
            let repaired = match drop_undecoded_binary_attachments(store, &stored).await {
                Ok(repaired) => repaired,
                Err(err) => {
                    crate::runtime::release_attachment_rescan(id);
                    return Err(err);
                }
            };
            stored = match store.get_message(id).await {
                Ok(Some(stored)) => stored,
                Ok(None) => {
                    crate::runtime::release_attachment_rescan(id);
                    return Err(Error::Store(
                        "message not found after attachment repair".to_string(),
                    ));
                }
                Err(err) => {
                    crate::runtime::release_attachment_rescan(id);
                    return Err(err);
                }
            };
            if repaired {
                if emit_body {
                    emit_body_ready(store, &stored, events, true, false).await;
                }
                return Ok(Some(WorkerCommand::FetchMessageRemainder(id)));
            }
            crate::runtime::release_attachment_rescan(id);
        }
        if needs_attachment_bytes(&stored) {
            if emit_body && matches!(mode, OpenMode::Interactive | OpenMode::Prefetch) {
                let pending = stored.has_attachments && file_attachments_for(&stored).is_empty();
                emit_body_ready(store, &stored, events, pending, false).await;
            }
            return Ok(Some(WorkerCommand::FetchMessageRemainder(id)));
        }
        if needs_attachment_rescan(&stored) && matches!(mode, OpenMode::Interactive) {
            if !crate::runtime::claim_attachment_rescan(id) {
                if emit_body {
                    emit_body_ready_from_stored(store, &stored, events).await;
                }
                return Ok(None);
            }
            emit_body_ready_from_stored(store, &stored, events).await;
            return Ok(Some(WorkerCommand::FetchMessageRemainder(id)));
        }
        if emit_body {
            emit_body_ready_from_stored(store, &stored, events).await;
        }
        return Ok(None);
    }

    ensure_selected(state, &folder.imap_name).await?;
    let display = if force_full {
        alcedo_imap::DisplayFetch {
            message: with_fetch_timeout(alcedo_imap::uid_fetch_full_message(
                state.client_mut(),
                stored.uid,
            ))
            .await?,
            deferred_rest: false,
            files_pending: false,
        }
    } else {
        // The list sync recorded the size; a large message then skips the raw
        // prefix of the probe. A failed read just falls back to the full probe.
        let known_size = store.message_rfc822_size(id).await.ok().flatten();
        with_fetch_timeout(alcedo_imap::uid_fetch_display_message(
            state.client_mut(),
            stored.uid,
            known_size,
        ))
        .await?
    };
    // Parts that arrived cut off or undecodable are not stored; fetch them
    // again by section (the remainder path) instead of keeping bad bytes.
    let incomplete_parts = &display.message.incomplete_attachments;
    let display_has_incomplete = !incomplete_parts.is_empty();
    if display_has_incomplete {
        warn!(
            uid = stored.uid,
            parts = ?incomplete_parts,
            "attachment parts incomplete after display fetch; refetching by section"
        );
    }
    let display_deferred_rest = display.deferred_rest || display_has_incomplete;
    let display_files_pending = display.files_pending || display_has_incomplete;
    let deferred = display_deferred_rest && schedule_remainder;
    let full = display.message;
    let from_email = stored
        .from
        .first()
        .map(|addr| addr.email.as_str())
        .unwrap_or("");
    let from_name = stored
        .from
        .first()
        .map(|addr| addr.display_name())
        .unwrap_or_default();
    maybe_persist_reaction_from_full(
        account,
        &folder,
        id,
        stored.seen,
        &from_name,
        from_email,
        &full,
        store,
        events,
    )
    .await?;

    if display_files_pending {
        store.set_has_attachments_true(id).await?;
    }
    crate::pgp::maybe_emit_autocrypt_offer(account.id, id, &full.raw, store, events).await;

    let cid_map = save_attachments(&stored, &full, store).await?;
    if body_already_saved && !redecode_charset {
        let list = list_unsubscribe_for_persist(full.list_unsubscribe.clone());
        store.save_list_unsubscribe(id, Some(&list)).await?;
        let stored = store
            .get_message(id)
            .await?
            .ok_or_else(|| Error::Store("message not found after attachment rescan".to_string()))?;
        if emit_body {
            emit_body_ready_from_stored(store, &stored, events).await;
        }
        return open_followup(deferred, id);
    }

    if let Some(IncomingPgp {
        text: pgp_text,
        html: pgp_html,
        status: pgp_status,
        snippet_override,
        persist,
    }) = process_incoming_pgp(account.id, &full.raw, store).await?
    {
        let text = if pgp_text.is_empty() && pgp_html.is_empty() && pgp_status.encrypted {
            String::new()
        } else {
            pgp_text
        };
        let html = recover_html_body(&pgp_html, &text);
        let stored_after_save = store
            .get_message(id)
            .await?
            .ok_or_else(|| Error::Store("message not found after attachments".to_string()))?;
        if emit_body {
            let cids = cid_inlines_for(store, &stored_after_save).await;
            let attachments = file_attachments_for(&stored_after_save);
            let attachments_pending = attachments_pending_after_display(
                display_deferred_rest,
                display_files_pending,
                &stored_after_save,
                attachments.is_empty(),
            );
            let _ = events.send(SyncEvent::BodyReady {
                id,
                html: html.clone(),
                text: text.clone(),
                cids,
                attachments,
                allow_remote_images: stored_after_save.allow_remote_images,
                list_unsubscribe: full.list_unsubscribe.clone(),
                message_id_hdr: stored_after_save.message_id_hdr.clone(),
                in_reply_to: stored_after_save.in_reply_to.clone(),
                references_hdr: stored_after_save.references_hdr.clone(),
                pgp: pgp_status.clone(),
                attachments_pending,
                attachments_failed: false,
            });
        }
        let mut envelope = fetched_to_envelope_from_stored(&stored_after_save, account);
        if let Some(snippet) = snippet_override {
            envelope.snippet = snippet;
        } else if !text.is_empty() {
            envelope.snippet = preview_snippet(&text);
        } else if pgp_status.encrypted && !pgp_status.decrypted {
            envelope.snippet = "Encrypted message".to_string();
        }
        refresh_subject_from_full(&mut envelope, &full);
        // A verdict made while the OpenPGP library was unavailable is shown but
        // not stored, so the next open fetches and verifies again. The envelope
        // upsert gets no body text then, which leaves the stored body untouched.
        if persist {
            store
                .save_message_body(id, Some(&text), Some(&html), Some(&pgp_status))
                .await?;
        }
        store
            .upsert_envelopes(&[envelope.clone()], persist.then_some(text.as_str()))
            .await?;
        let _ = events.send(SyncEvent::EnvelopeUpdated(envelope));
        let list = list_unsubscribe_for_persist(full.list_unsubscribe.clone());
        store.save_list_unsubscribe(id, Some(&list)).await?;
        return open_followup(deferred, id);
    }

    // Keep plaintext as plaintext. Never store raw HTML in body_text — the reader
    // would otherwise fall back to showing markup as source.
    let text = if !full.text.is_empty() {
        full.text.clone()
    } else if !full.html.is_empty() {
        html_to_snippet(&full.html, 50_000)
    } else {
        String::new()
    };

    // Persist HTML with cid: rewritten; sanitize only at display time.
    // Recruiter mail often puts an HTML fragment in text/plain; mail-parser then
    // stores an escaped <html><body>…&lt;br&gt; conversion as the HTML part.
    let parsed_html = if full.html.is_empty() {
        String::new()
    } else {
        rewrite_cids(&full.html, &cid_map)
    };
    let html = recover_html_body(&parsed_html, &text);
    let html = if html == parsed_html {
        html
    } else {
        rewrite_cids(&html, &cid_map)
    };

    let stored_after_save = store
        .get_message(id)
        .await?
        .ok_or_else(|| Error::Store("message not found after attachments".to_string()))?;
    if emit_body {
        let cids = cid_inlines_for(store, &stored_after_save).await;
        let attachments = file_attachments_for(&stored_after_save);
        let attachments_pending = attachments_pending_after_display(
            display_deferred_rest,
            display_files_pending,
            &stored_after_save,
            attachments.is_empty(),
        );
        let _ = events.send(SyncEvent::BodyReady {
            id,
            html: html.clone(),
            text: text.clone(),
            cids,
            attachments,
            allow_remote_images: stored_after_save.allow_remote_images,
            list_unsubscribe: full.list_unsubscribe.clone(),
            message_id_hdr: stored_after_save.message_id_hdr.clone(),
            in_reply_to: stored_after_save.in_reply_to.clone(),
            references_hdr: stored_after_save.references_hdr.clone(),
            pgp: Default::default(),
            attachments_pending,
            attachments_failed: false,
        });
    }
    let mut envelope = fetched_to_envelope_from_stored(&stored_after_save, account);
    if !text.is_empty() {
        envelope.snippet = preview_snippet(&text);
    }
    refresh_subject_from_full(&mut envelope, &full);
    store
        .save_message_body(id, Some(&text), Some(&html), None)
        .await?;
    store
        .upsert_envelopes(&[envelope.clone()], Some(&text))
        .await?;
    let _ = events.send(SyncEvent::EnvelopeUpdated(envelope));
    let list = list_unsubscribe_for_persist(full.list_unsubscribe.clone());
    store.save_list_unsubscribe(id, Some(&list)).await?;

    open_followup(deferred, id)
}

pub(super) fn open_followup(deferred: bool, id: MessageId) -> Result<Option<WorkerCommand>> {
    if deferred {
        Ok(Some(WorkerCommand::FetchMessageRemainder(id)))
    } else {
        Ok(None)
    }
}

pub(super) fn remainder_skips_attachment_download(
    plan: &alcedo_imap::DisplayPlan,
    bytes_allowed: bool,
    charset_only: bool,
) -> bool {
    if bytes_allowed || charset_only {
        return false;
    }
    matches!(
        plan,
        alcedo_imap::DisplayPlan::Parts {
            has_extra: true,
            ..
        } | alcedo_imap::DisplayPlan::Whole
    )
}

pub(super) fn plan_has_file_leaf(plan: &alcedo_imap::DisplayPlan) -> bool {
    match plan {
        alcedo_imap::DisplayPlan::Whole => true,
        alcedo_imap::DisplayPlan::Parts { extras, .. } => extras.iter().any(|section| {
            alcedo_core::attachment_is_downloadable(
                &section.filename,
                &format!(
                    "{}/{}",
                    section.mime_type.to_ascii_lowercase(),
                    section.mime_subtype.to_ascii_lowercase()
                ),
                section.cid.as_deref(),
                &AttachmentLists::defaults(),
            )
        }),
    }
}

pub(super) async fn fail_attachment_remainder(
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
    id: MessageId,
    stored: &alcedo_store::StoredMessage,
    plan: &alcedo_imap::DisplayPlan,
    bytes_allowed: bool,
) {
    if bytes_allowed && (stored.has_attachments || plan_has_file_leaf(plan)) {
        if let Ok(Some(stored_now)) = store.get_message(id).await {
            emit_body_ready_failed(store, &stored_now, events).await;
            if needs_attachment_rescan(&stored_now) && !needs_attachment_bytes(&stored_now) {
                crate::runtime::release_attachment_rescan(id);
            }
        } else if needs_attachment_rescan(stored) && !needs_attachment_bytes(stored) {
            crate::runtime::release_attachment_rescan(id);
        }
    } else if needs_attachment_rescan(stored) && !needs_attachment_bytes(stored) {
        crate::runtime::release_attachment_rescan(id);
    }
}

pub(super) async fn fetch_message_remainder(
    state: &mut WorkerImapState,
    account: &Account,
    id: MessageId,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
    bytes_allowed: bool,
) -> Result<()> {
    let stored = store
        .get_message(id)
        .await?
        .ok_or_else(|| Error::Store("message not found".to_string()))?;
    let folder = store
        .get_folder(stored.folder_id)
        .await?
        .ok_or_else(|| Error::Store("folder not found for message".to_string()))?;
    if folder.special == SpecialUse::Drafts || stored.draft || !folder_is_syncable(&folder) {
        return Ok(());
    }

    ensure_selected(state, &folder.imap_name).await?;
    let plan = with_fetch_timeout(alcedo_imap::uid_fetch_display_plan(
        state.client_mut(),
        stored.uid,
    ))
    .await?;
    let redecode_charset = stored_body_needs_charset_rescan(&stored);
    if let alcedo_imap::DisplayPlan::Parts {
        has_extra: false, ..
    } = plan
    {
        if !redecode_charset {
            return Ok(());
        }
        // Charset repair only needs the text parts, not the attachment bytes.
        let _ = open_message(state, account, id, store, events, OpenMode::Prefetch).await?;
        return Ok(());
    }

    if remainder_skips_attachment_download(&plan, bytes_allowed, false) {
        return Ok(());
    }

    if let alcedo_imap::DisplayPlan::Parts { extras, .. } = &plan
        && !extras.is_empty()
    {
        let attachments = match with_fetch_timeout(alcedo_imap::uid_fetch_attachment_sections(
            state.client_mut(),
            stored.uid,
            extras,
        ))
        .await
        {
            Ok(attachments) if !attachments.is_empty() => attachments,
            section_result => {
                if let Err(err) = &section_result {
                    warn!(
                        uid = stored.uid,
                        error = %err,
                        "attachment section fetch failed; falling back to full message"
                    );
                }
                match with_fetch_timeout(alcedo_imap::uid_fetch_full_message(
                    state.client_mut(),
                    stored.uid,
                ))
                .await
                {
                    Ok(full) => {
                        if !full.incomplete_attachments.is_empty() {
                            warn!(
                                uid = stored.uid,
                                parts = ?full.incomplete_attachments,
                                "full message still has incomplete attachment parts; not storing them"
                            );
                        }
                        full.attachments
                    }
                    Err(err) => {
                        fail_attachment_remainder(store, events, id, &stored, &plan, bytes_allowed)
                            .await;
                        return Err(err);
                    }
                }
            }
        };
        let full = alcedo_imap::FullMessage {
            raw: Vec::new(),
            subject: stored.subject.clone(),
            text: stored.body_text.clone().unwrap_or_default(),
            html: stored.body_html.clone().unwrap_or_default(),
            list_unsubscribe: stored.list_unsubscribe.clone(),
            attachments,
            reaction: None,
            incomplete_attachments: Vec::new(),
        };
        if let Err(err) = save_attachments(&stored, &full, store).await {
            fail_attachment_remainder(store, events, id, &stored, &plan, bytes_allowed).await;
            return Err(err);
        }
        let reloaded = match store.get_message(id).await {
            Ok(Some(stored_now)) => stored_now,
            Ok(None) => {
                let err = Error::Store("message not found after attachment fetch".to_string());
                if needs_attachment_rescan(&stored) && !needs_attachment_bytes(&stored) {
                    crate::runtime::release_attachment_rescan(id);
                }
                return Err(err);
            }
            Err(err) => {
                fail_attachment_remainder(store, events, id, &stored, &plan, bytes_allowed).await;
                return Err(err);
            }
        };
        if file_attachments_for(&reloaded).is_empty()
            && (stored.has_attachments || plan_has_file_leaf(&plan))
        {
            emit_body_ready_failed(store, &reloaded, events).await;
        } else {
            emit_body_ready_from_stored(store, &reloaded, events).await;
        }
        return Ok(());
    }

    if let Err(err) = open_message(state, account, id, store, events, OpenMode::Remainder).await {
        fail_attachment_remainder(store, events, id, &stored, &plan, bytes_allowed).await;
        return Err(err);
    }
    Ok(())
}

pub(super) async fn fetch_raw_message(
    state: &mut WorkerImapState,
    account: &Account,
    id: MessageId,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
) -> Result<()> {
    let stored = store
        .get_message(id)
        .await?
        .ok_or_else(|| Error::Store("message not found".to_string()))?;

    let folders = store.list_folders(account.id).await?;
    let folder = folders
        .into_iter()
        .find(|f| f.id == stored.folder_id)
        .ok_or_else(|| Error::Store("folder not found for message".to_string()))?;

    if !folder_is_syncable(&folder) {
        return Err(Error::Store(
            "this message is stored locally and has no IMAP copy".to_string(),
        ));
    }

    ensure_selected(state, &folder.imap_name).await?;
    let full = with_fetch_timeout(alcedo_imap::uid_fetch_full_message(
        state.client_mut(),
        stored.uid,
    ))
    .await?;

    let _ = events.send(SyncEvent::RawMessageReady { id, raw: full.raw });
    Ok(())
}

/// Prefer HTML-looking plaintext over mail-parser's escaped `text_to_html` wrapper.
pub(super) fn recover_html_body(html: &str, text: &str) -> String {
    if looks_like_html(text) && (html.trim().is_empty() || looks_like_escaped_html(html)) {
        text.to_string()
    } else {
        html.to_string()
    }
}

pub(super) fn fetched_to_envelope_from_stored(
    stored: &alcedo_store::StoredMessage,
    account: &Account,
) -> alcedo_core::Envelope {
    alcedo_core::Envelope {
        id: stored.id,
        account_id: stored.account_id,
        folder_id: stored.folder_id,
        uid: stored.uid,
        thread_id: stored.thread_id,
        date: stored.date,
        subject: stored.subject.clone(),
        from: stored.from.clone(),
        to: stored.to.clone(),
        cc: stored.cc.clone(),
        reply_to: stored.reply_to.clone(),
        sender: stored.sender.clone(),
        routing: None,
        snippet: stored.snippet.clone(),
        seen: stored.seen,
        flagged: stored.flagged,
        answered: stored.answered,
        draft: stored.draft,
        has_attachments: stored.has_attachments,
        deleted_at: None,
        account_accent: account.accent_color.clone(),
        account_avatar: account.avatar_text.clone(),
        size_bytes: 0,
    }
}

pub(super) async fn save_attachments(
    stored: &alcedo_store::StoredMessage,
    full: &alcedo_imap::FullMessage,
    store: &StoreHandle,
) -> Result<HashMap<String, String>> {
    let mut cid_map = HashMap::new();
    let mut failures = 0usize;
    let mut attempted = 0usize;

    for attachment in &full.attachments {
        if attachment.bytes.is_empty() {
            continue;
        }
        attempted += 1;
        let filename = fetched_attachment_filename(attachment);
        // Last guard before the cache: bytes that are still the base64 text
        // of a binary file must never be stored as that file (both reader
        // save buttons write the cached blob verbatim).
        let mime = format!(
            "{}/{}",
            attachment.content_type.0, attachment.content_type.1
        )
        .to_ascii_lowercase();
        if alcedo_imap::attachment_bytes_are_undecoded_base64(&mime, &attachment.bytes) {
            failures += 1;
            warn!(filename = %filename, "attachment bytes are undecoded base64; not storing");
            continue;
        }
        let stored_cid = if attachment.disposition_attachment
            && attachment.content_type.0.eq_ignore_ascii_case("image")
        {
            None
        } else {
            attachment.cid.clone()
        };
        let meta = AttachmentMeta {
            message_id: stored.id,
            filename,
            cid: stored_cid.clone(),
        };
        match store.save_attachment(meta, &attachment.bytes).await {
            Ok(path) => {
                let sha256 = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or_default()
                    .to_string();
                if let Some(cid) = stored_cid.as_deref() {
                    cid_map.insert(normalize_cid(cid), sha256);
                }
            }
            Err(err) => {
                failures += 1;
                warn!(error = %err, "attachment save skipped");
            }
        }
    }

    if attempted > 0 && failures == attempted {
        warn!(count = failures, "all attachment saves failed");
    }

    Ok(cid_map)
}

pub(super) fn fetched_attachment_filename(attachment: &alcedo_imap::FetchedAttachment) -> String {
    if !attachment.filename.is_empty() {
        return attachment.filename.clone();
    }
    default_attachment_filename_from_types(&attachment.content_type.0, &attachment.content_type.1)
}

/// Fetch may leave `filename` empty when a part has no `Content-Disposition`
/// filename and no `Content-Type` name parameter — notably forwarded emails
/// (`message/rfc822`/`message/global` parts). Prefer a real extension when the
/// MIME type is known so the reader chip is named usefully.
pub(super) fn default_attachment_filename_from_types(main: &str, sub: &str) -> String {
    if main.eq_ignore_ascii_case("message")
        && (sub.eq_ignore_ascii_case("rfc822") || sub.eq_ignore_ascii_case("global"))
    {
        "forwarded-message.eml".to_string()
    } else if main.eq_ignore_ascii_case("text") && sub.eq_ignore_ascii_case("calendar")
        || main.eq_ignore_ascii_case("application")
            && (sub.eq_ignore_ascii_case("ics") || sub.eq_ignore_ascii_case("x-ics"))
    {
        "invite.ics".to_string()
    } else {
        "attachment".to_string()
    }
}

pub(super) fn normalize_cid(cid: &str) -> String {
    cid.trim()
        .trim_start_matches('<')
        .trim_end_matches('>')
        .to_string()
}

pub(super) fn file_attachments_for(stored: &alcedo_store::StoredMessage) -> Vec<BodyAttachment> {
    let lists = AttachmentLists::defaults();
    stored
        .attachments
        .iter()
        .filter(|att| {
            attachment_is_downloadable(&att.filename, &att.mime, att.cid.as_deref(), &lists)
        })
        .map(|att| BodyAttachment {
            filename: att.filename.clone(),
            mime: att.mime.clone(),
            sha256: att.sha256.clone(),
            size: att.size,
        })
        .collect()
}

pub(super) async fn cid_inlines_for(
    store: &StoreHandle,
    stored: &alcedo_store::StoredMessage,
) -> Vec<CidInline> {
    let mut cids = Vec::new();
    for att in &stored.attachments {
        match store.attachment_bytes(&att.sha256).await {
            Ok(bytes) => {
                let cid = att
                    .cid
                    .as_deref()
                    .map(normalize_cid)
                    .filter(|cid| !cid.is_empty());
                cids.push(CidInline {
                    sha256: att.sha256.clone(),
                    mime: att.mime.clone(),
                    bytes,
                    cid,
                });
            }
            Err(err) => {
                warn!(error = %err, sha256 = %att.sha256, "cid attachment bytes missing");
            }
        }
    }
    cids
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worker::mailbox_sync::emit_folder_envelopes;
    use crate::worker::sent_copy::apply_sent_parts;
    use alcedo_core::{AccountId, FolderId, MessageId};
    use uuid::Uuid;

    #[test]
    fn fetched_attachment_filename_named_passthrough() {
        let attachment = alcedo_imap::FetchedAttachment {
            filename: "report.docx".to_string(),
            cid: None,
            bytes: vec![1, 2, 3],
            content_type: ("application".to_string(), "octet-stream".to_string()),
            disposition_attachment: false,
        };
        assert_eq!(fetched_attachment_filename(&attachment), "report.docx");
    }

    #[test]
    fn fetched_attachment_filename_forwarded_message_eml() {
        let attachment = alcedo_imap::FetchedAttachment {
            filename: String::new(),
            cid: None,
            bytes: vec![1],
            content_type: ("message".to_string(), "rfc822".to_string()),
            disposition_attachment: false,
        };
        assert_eq!(
            fetched_attachment_filename(&attachment),
            "forwarded-message.eml"
        );
    }

    #[test]
    fn fetched_attachment_filename_calendar_invite() {
        let attachment = alcedo_imap::FetchedAttachment {
            filename: String::new(),
            cid: None,
            bytes: vec![1],
            content_type: ("text".to_string(), "calendar".to_string()),
            disposition_attachment: false,
        };
        assert_eq!(fetched_attachment_filename(&attachment), "invite.ics");
    }

    #[test]
    fn fetched_attachment_filename_octet_stream() {
        let attachment = alcedo_imap::FetchedAttachment {
            filename: String::new(),
            cid: None,
            bytes: vec![1],
            content_type: ("application".to_string(), "octet-stream".to_string()),
            disposition_attachment: false,
        };
        assert_eq!(fetched_attachment_filename(&attachment), "attachment");
    }

    #[test]
    fn recover_html_body_uses_html_fragment_in_plaintext() {
        let text = "Hi&nbsp;Carter,<br><span>Hope you are doing well.</span>";
        let escaped = "<html><body>Hi&nbsp;Carter,&lt;br&gt;&lt;span&gt;Hope you are doing well.&lt;/span&gt;</body></html>";
        assert_eq!(recover_html_body("", text), text);
        assert_eq!(recover_html_body(escaped, text), text);
        assert_eq!(
            recover_html_body("<p>Real HTML</p>", "plain"),
            "<p>Real HTML</p>"
        );
        let sent = "<html><body><p>Thanks</p><p>On Jan 1, Alice &lt;alice@example.com&gt; wrote:</p><blockquote>Hi</blockquote></body></html>";
        let plain = "Thanks\n\nOn Jan 1, Alice <alice@example.com> wrote:\n\nHi";
        assert_eq!(recover_html_body(sent, plain), sent);
    }

    fn minimal_stored_message(
        body_text: Option<&str>,
        body_html: Option<&str>,
    ) -> alcedo_store::StoredMessage {
        alcedo_store::StoredMessage {
            id: MessageId(1),
            account_id: AccountId(Uuid::nil()),
            folder_id: FolderId(1),
            uid: 1,
            thread_id: alcedo_core::ThreadId(Uuid::nil()),
            date: chrono::Utc::now(),
            subject: "subj".to_string(),
            from: vec![],
            to: vec![],
            cc: vec![],
            reply_to: Vec::new(),
            sender: Vec::new(),
            snippet: "snippet".to_string(),
            body_text: body_text.map(|s| s.to_string()),
            body_html: body_html.map(|s| s.to_string()),
            message_id_hdr: None,
            in_reply_to: None,
            references_hdr: None,
            seen: false,
            flagged: false,
            answered: false,
            draft: false,
            has_attachments: false,
            allow_remote_images: false,
            list_unsubscribe: None,
            account_accent: "#3584e4".to_string(),
            account_avatar: "AB".to_string(),
            attachments: vec![],
            pgp_status: None,
        }
    }

    #[test]
    fn stored_has_local_body_none() {
        assert!(!stored_has_local_body(&minimal_stored_message(None, None)));
    }

    #[test]
    fn stored_has_local_body_text_only() {
        assert!(stored_has_local_body(&minimal_stored_message(
            Some("hi"),
            None
        )));
    }

    #[test]
    fn stored_has_local_body_html_only() {
        assert!(stored_has_local_body(&minimal_stored_message(
            None,
            Some("<p>hi</p>")
        )));
    }

    #[test]
    fn stored_has_local_body_both() {
        assert!(stored_has_local_body(&minimal_stored_message(
            Some("hi"),
            Some("<p>hi</p>")
        )));
    }

    #[test]
    fn stored_has_local_body_empty_text_counts() {
        assert!(stored_has_local_body(&minimal_stored_message(
            Some(""),
            None
        )));
    }

    #[test]
    fn charset_rescan_detects_replacement_in_text() {
        assert!(stored_body_needs_charset_rescan(&minimal_stored_message(
            Some("Sep 2025\u{FFFD}VFeb"),
            None
        )));
        assert!(!stored_body_needs_charset_rescan(&minimal_stored_message(
            Some("Sep 2025–Feb"),
            None
        )));
    }

    #[test]
    fn charset_rescan_detects_replacement_in_subject_or_snippet() {
        let mut stored = minimal_stored_message(Some("plain"), None);
        stored.subject = "Request \u{FFFD}X bank".to_string();
        assert!(stored_body_needs_charset_rescan(&stored));
        stored.subject = "Request — bank".to_string();
        stored.snippet = "Sep 2025\u{FFFD}VFeb".to_string();
        assert!(stored_body_needs_charset_rescan(&stored));
    }

    #[test]
    fn charset_rescan_skips_encrypted_pgp() {
        let mut stored = minimal_stored_message(Some("\u{FFFD}X bank"), None);
        stored.pgp_status = Some(alcedo_core::PgpStatus {
            encrypted: true,
            decrypted: true,
            signature: alcedo_core::PgpSignatureState::None,
        });
        assert!(!stored_body_needs_charset_rescan(&stored));
    }

    #[test]
    fn needs_attachment_rescan_cid_only_with_body() {
        let mut stored = minimal_stored_message(Some("hi"), Some("<p>hi</p>"));
        stored.attachments = vec![sample_stored_att("logo.png", "image/png", Some("logo@x"))];
        assert!(needs_attachment_rescan(&stored));
    }

    #[test]
    fn needs_attachment_rescan_skips_when_file_present() {
        let mut stored = minimal_stored_message(Some("hi"), None);
        stored.attachments = vec![
            sample_stored_att("logo.png", "image/png", Some("logo@x")),
            sample_stored_att(
                "minutes.docx",
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
                None,
            ),
        ];
        assert!(!needs_attachment_rescan(&stored));
    }

    #[test]
    fn needs_attachment_rescan_plain_body_without_parts() {
        assert!(needs_attachment_rescan(&minimal_stored_message(
            Some("hi"),
            None
        )));
    }

    #[test]
    fn needs_attachment_rescan_skips_body_without_local_cache() {
        assert!(!needs_attachment_rescan(&minimal_stored_message(
            None, None
        )));
    }

    fn sample_stored_att(
        filename: &str,
        mime: &str,
        cid: Option<&str>,
    ) -> alcedo_store::StoredAttachment {
        alcedo_store::StoredAttachment {
            id: 1,
            filename: filename.to_string(),
            mime: mime.to_string(),
            sha256: "aa".repeat(32),
            size: 12,
            cid: cid.map(str::to_string),
        }
    }

    #[test]
    fn needs_attachment_bytes_when_flag_set_and_no_files() {
        let mut stored = minimal_stored_message(Some("hi"), None);
        stored.has_attachments = true;
        assert!(needs_attachment_bytes(&stored));
    }

    #[test]
    fn display_fetch_pending_when_deferred_rest_and_flag() {
        let mut stored = minimal_stored_message(Some("hi"), None);
        stored.has_attachments = true;
        assert!(attachments_pending_after_display(
            true, false, &stored, true
        ));
    }

    #[test]
    fn display_fetch_pending_false_for_text_only() {
        let stored = minimal_stored_message(Some("hi"), None);
        assert!(!attachments_pending_after_display(
            false, false, &stored, true
        ));
    }

    #[test]
    fn needs_attachment_bytes_false_when_files_present() {
        let mut stored = minimal_stored_message(Some("hi"), None);
        stored.has_attachments = true;
        stored.attachments = vec![sample_stored_att("report.pdf", "application/pdf", None)];
        assert!(!needs_attachment_bytes(&stored));
    }

    #[test]
    fn remainder_skips_attachment_download_when_bytes_not_allowed() {
        let plan = alcedo_imap::DisplayPlan::Parts {
            sections: Vec::new(),
            has_extra: true,
            extras: vec![alcedo_imap::AttachmentSection {
                path: vec![2],
                filename: "clip.mov".to_string(),
                mime_type: "video".to_string(),
                mime_subtype: "quicktime".to_string(),
                cid: None,
                encoding: "base64".to_string(),
                disposition_attachment: false,
                octets: 0,
            }],
        };
        assert!(remainder_skips_attachment_download(&plan, false, false));
        assert!(!remainder_skips_attachment_download(&plan, true, false));
        assert!(!remainder_skips_attachment_download(
            &alcedo_imap::DisplayPlan::Parts {
                sections: Vec::new(),
                has_extra: false,
                extras: Vec::new(),
            },
            false,
            true,
        ));
    }

    #[test]
    fn file_attachments_include_outlook_cid_pdf() {
        let mut stored = minimal_stored_message(None, None);
        stored.attachments = vec![sample_stored_att(
            "rules.pdf",
            "application/pdf",
            Some("cid@outlook.com"),
        )];
        let files = file_attachments_for(&stored);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].filename, "rules.pdf");
    }

    #[test]
    fn file_attachments_exclude_cid_png() {
        let mut stored = minimal_stored_message(None, None);
        stored.attachments = vec![sample_stored_att("logo.png", "image/png", Some("logo@x"))];
        assert!(file_attachments_for(&stored).is_empty());
    }

    #[test]
    fn file_attachments_include_risky_cid_svg() {
        let mut stored = minimal_stored_message(None, None);
        stored.attachments = vec![sample_stored_att(
            "logo.svg",
            "image/svg+xml",
            Some("logo@x"),
        )];
        let files = file_attachments_for(&stored);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].filename, "logo.svg");
    }

    mod emit_body_ready_tests {
        use std::sync::Arc;

        use chrono::Utc;
        use tempfile::TempDir;
        use uuid::Uuid;

        use alcedo_core::{
            Account, Address, AuthMethod, Envelope, Folder, FolderId, MessageId, MessageListMode,
            OAuthProvider, SpecialUse, SyncEvent, ThreadId,
        };
        use alcedo_store::{AttachmentMeta, StoreHandle};

        use super::*;

        fn sample_account(id: Uuid, email: &str) -> Account {
            Account {
                id: AccountId(id),
                email: email.to_string(),
                username: email.to_string(),
                split_credentials: false,
                smtp_username: String::new(),
                display_name: email.to_string(),
                sender_name: String::new(),
                imap_host: "imap.example.com".to_string(),
                imap_port: 993,
                smtp_host: "smtp.example.com".to_string(),
                smtp_port: 465,
                smtp_starttls: false,
                auth: AuthMethod::OAuth2 {
                    provider: OAuthProvider::Google,
                },
                accent_color: "#3584e4".to_string(),
                avatar_text: "AB".to_string(),
                avatar_image: Vec::new(),
            }
        }

        fn sample_folder(account_id: AccountId, name: &str) -> Folder {
            Folder {
                id: FolderId(0),
                account_id,
                imap_name: name.to_string(),
                delimiter: "/".to_string(),
                special: SpecialUse::Inbox,
                selectable: true,
                uidvalidity: Some(1),
                uidnext: Some(2),
                highest_modseq: None,
                unread: 0,
                total: 0,
            }
        }

        fn sample_envelope(
            account: &Account,
            folder_id: FolderId,
            uid: u32,
            subject: &str,
        ) -> Envelope {
            Envelope {
                id: MessageId(0),
                account_id: account.id,
                folder_id,
                uid,
                thread_id: ThreadId(Uuid::new_v4()),
                date: Utc::now(),
                subject: subject.to_string(),
                from: vec![Address {
                    name: Some("Sender".to_string()),
                    email: "sender@example.com".to_string(),
                }],
                to: vec![Address {
                    name: None,
                    email: account.email.clone(),
                }],
                cc: vec![],
                reply_to: Vec::new(),
                sender: Vec::new(),
                routing: None,
                snippet: "snippet".to_string(),
                seen: false,
                flagged: false,
                answered: false,
                draft: false,
                has_attachments: false,
                deleted_at: None,
                account_accent: account.accent_color.clone(),
                account_avatar: account.avatar_text.clone(),
                size_bytes: 0,
            }
        }

        async fn open_test_store() -> (Arc<StoreHandle>, TempDir) {
            let dir = TempDir::new().expect("tempdir");
            let db_path = dir.path().join("mail.db");
            let store = Arc::new(StoreHandle::open_path(&db_path).expect("open store"));
            (store, dir)
        }

        async fn setup_message(store: &StoreHandle) -> MessageId {
            let account = sample_account(Uuid::new_v4(), "body@example.com");
            store
                .upsert_account(&account)
                .await
                .expect("upsert account");
            let folders = store
                .replace_folders(account.id, &[sample_folder(account.id, "INBOX")])
                .await
                .expect("folders");
            store
                .upsert_envelopes(
                    &[sample_envelope(&account, folders[0].id, 1, "msg")],
                    Some("snippet"),
                )
                .await
                .expect("envelope");
            let messages = store
                .folder_messages(folders[0].id, 10, 0)
                .await
                .expect("list");
            messages[0].id
        }

        fn full_message_with_parsed_attachments(raw: &str) -> alcedo_imap::FullMessage {
            alcedo_imap::parse_full_message(raw.as_bytes()).expect("parse raw")
        }

        #[tokio::test]
        async fn emit_body_ready_pending_when_cached_and_needs_bytes() {
            let (store, _dir) = open_test_store().await;
            let id = setup_message(&store).await;
            store
                .save_message_body(id, Some("plain"), None, None)
                .await
                .expect("save body");
            let mut stored = store.get_message(id).await.expect("get").expect("exists");
            stored.has_attachments = true;
            assert!(needs_attachment_bytes(&stored));
            let (tx, rx) = flume::unbounded();
            emit_body_ready_from_stored_with_pending(&store, &stored, &tx, true).await;
            let event = rx.recv_async().await.expect("event");
            match event {
                SyncEvent::BodyReady {
                    attachments_pending,
                    ..
                } => assert!(attachments_pending),
                other => panic!("expected BodyReady, got {other:?}"),
            }
        }

        #[tokio::test]
        async fn emit_body_ready_from_stored_plaintext_and_html() {
            let (store, _dir) = open_test_store().await;
            let id = setup_message(&store).await;
            store
                .save_message_body(id, Some("plain"), Some("<p>hi</p>"), None)
                .await
                .expect("save body");
            let stored = store.get_message(id).await.expect("get").expect("exists");
            let (tx, rx) = flume::unbounded();
            emit_body_ready_from_stored(&store, &stored, &tx).await;
            let event = rx.recv_async().await.expect("event");
            match event {
                SyncEvent::BodyReady {
                    id: event_id,
                    html,
                    text,
                    cids,
                    attachments,
                    allow_remote_images,
                    list_unsubscribe,
                    ..
                } => {
                    assert_eq!(event_id, id);
                    assert_eq!(text, "plain");
                    assert_eq!(html, "<p>hi</p>");
                    assert!(cids.is_empty());
                    assert!(attachments.is_empty());
                    assert!(!allow_remote_images);
                    assert!(list_unsubscribe.is_none());
                }
                other => panic!("expected BodyReady, got {other:?}"),
            }
        }

        #[tokio::test]
        async fn emit_body_ready_from_stored_file_attachment() {
            let (store, _dir) = open_test_store().await;
            let id = setup_message(&store).await;
            store
                .save_message_body(id, Some("plain"), None, None)
                .await
                .expect("save body");
            store
                .save_attachment(
                    AttachmentMeta {
                        message_id: id,
                        filename: "note.txt".to_string(),
                        cid: None,
                    },
                    b"file bytes",
                )
                .await
                .expect("save attachment");
            let stored = store.get_message(id).await.expect("get").expect("exists");
            let (tx, rx) = flume::unbounded();
            emit_body_ready_from_stored(&store, &stored, &tx).await;
            let event = rx.recv_async().await.expect("event");
            match event {
                SyncEvent::BodyReady {
                    cids, attachments, ..
                } => {
                    assert!(
                        !cids.is_empty(),
                        "file bytes should be in cids for Save/Open"
                    );
                    assert!(!attachments.is_empty(), "file should appear in attachments");
                }
                other => panic!("expected BodyReady, got {other:?}"),
            }
        }

        #[tokio::test]
        async fn emit_body_ready_from_stored_cid_only_attachment() {
            let (store, _dir) = open_test_store().await;
            let id = setup_message(&store).await;
            store
                .save_message_body(id, Some("plain"), None, None)
                .await
                .expect("save body");
            store
                .save_attachment(
                    AttachmentMeta {
                        message_id: id,
                        filename: "inline.png".to_string(),
                        cid: Some("img@cid".to_string()),
                    },
                    &[0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a],
                )
                .await
                .expect("save attachment");
            let stored = store.get_message(id).await.expect("get").expect("exists");
            let (tx, rx) = flume::unbounded();
            emit_body_ready_from_stored(&store, &stored, &tx).await;
            let event = rx.recv_async().await.expect("event");
            match event {
                SyncEvent::BodyReady {
                    cids, attachments, ..
                } => {
                    assert!(!cids.is_empty());
                    assert_eq!(cids[0].cid.as_deref(), Some("img@cid"));
                    assert!(attachments.is_empty());
                }
                other => panic!("expected BodyReady, got {other:?}"),
            }
        }

        #[tokio::test]
        async fn emit_body_ready_from_stored_outlook_cid_file() {
            let (store, _dir) = open_test_store().await;
            let id = setup_message(&store).await;
            store
                .save_message_body(id, Some("plain"), None, None)
                .await
                .expect("save body");
            store
                .save_attachment(
                    AttachmentMeta {
                        message_id: id,
                        filename: "rules.txt".to_string(),
                        cid: Some("1DC8@namprd19.prod.outlook.com".to_string()),
                    },
                    b"hoa rules",
                )
                .await
                .expect("save attachment");
            let stored = store.get_message(id).await.expect("get").expect("exists");
            assert!(stored.has_attachments);
            let (tx, rx) = flume::unbounded();
            emit_body_ready_from_stored(&store, &stored, &tx).await;
            let event = rx.recv_async().await.expect("event");
            match event {
                SyncEvent::BodyReady {
                    cids, attachments, ..
                } => {
                    assert!(!cids.is_empty(), "bytes stay available for Save/Open");
                    assert_eq!(attachments.len(), 1);
                    assert_eq!(attachments[0].filename, "rules.txt");
                }
                other => panic!("expected BodyReady, got {other:?}"),
            }
        }

        #[tokio::test]
        async fn drop_undecoded_binary_attachments_returns_true_and_removes_row() {
            let (store, _dir) = open_test_store().await;
            let id = setup_message(&store).await;
            store
                .save_message_body(id, Some("plain"), None, None)
                .await
                .expect("save body");
            store
                .save_attachment(
                    AttachmentMeta {
                        message_id: id,
                        filename: "photo.jpg".to_string(),
                        cid: None,
                    },
                    b"/9j/4AAQSkZJRgABAQAAAQABAAD/2wBD",
                )
                .await
                .expect("save corrupt attachment");
            store
                .save_attachment(
                    AttachmentMeta {
                        message_id: id,
                        filename: "inline.png".to_string(),
                        cid: Some("img@cid".to_string()),
                    },
                    &[0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a],
                )
                .await
                .expect("save cid attachment");
            let stored = store.get_message(id).await.expect("get").expect("exists");
            let repaired = drop_undecoded_binary_attachments(&store, &stored)
                .await
                .expect("drop");
            assert!(repaired);
            let updated = store.get_message(id).await.expect("get").expect("exists");
            assert_eq!(updated.attachments.len(), 1);
            assert_eq!(updated.attachments[0].filename, "inline.png");
            assert_eq!(updated.attachments[0].cid.as_deref(), Some("img@cid"));
        }

        #[tokio::test]
        async fn save_attachments_accepts_docx_attachment() {
            let (store, _dir) = open_test_store().await;
            let id = setup_message(&store).await;
            let stored = store.get_message(id).await.expect("get").expect("exists");

            let raw = concat!(
                "From: sender@example.com\r\n",
                "To: recipient@example.com\r\n",
                "Subject: docx test\r\n",
                "MIME-Version: 1.0\r\n",
                "Content-Type: multipart/mixed; boundary=\"bnd\"\r\n",
                "\r\n",
                "--bnd\r\n",
                "Content-Type: text/plain; charset=utf-8\r\n",
                "\r\n",
                "Hello\r\n",
                "--bnd\r\n",
                "Content-Type: application/vnd.openxmlformats-officedocument.wordprocessingml.document; name=\"report.docx\"\r\n",
                "Content-Disposition: attachment; filename=\"report.docx\"\r\n",
                "Content-Transfer-Encoding: base64\r\n",
                "\r\n",
                "YXJiaXRyYXJ5IGJ5dGVz\r\n",
                "--bnd--\r\n",
            );
            let full = full_message_with_parsed_attachments(raw);

            save_attachments(&stored, &full, &store)
                .await
                .expect("save attachments");

            let updated = store.get_message(id).await.expect("get").expect("exists");
            assert_eq!(updated.attachments.len(), 1);
            assert_eq!(updated.attachments[0].filename, "report.docx");
        }

        /// Regression: a DOCX part cut off by the 256 KiB display window
        /// was stored as its base64 text (`UEsDBBQACAgI…`) and both reader
        /// save buttons wrote that text to disk.
        #[tokio::test]
        async fn save_attachments_never_stores_truncated_docx_base64() {
            let (store, _dir) = open_test_store().await;
            let id = setup_message(&store).await;
            let stored = store.get_message(id).await.expect("get").expect("exists");

            let mut docx = b"PK\x03\x04\x14\x00\x08\x08\x08\x00".to_vec();
            docx.resize(423_417, 0x5a);
            let encoded = {
                const ALPHABET: &[u8; 64] =
                    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
                let mut out = String::new();
                for chunk in docx.chunks(3) {
                    let v = (u32::from(chunk[0]) << 16)
                        | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
                        | u32::from(*chunk.get(2).unwrap_or(&0));
                    for i in 0..4 {
                        if i <= chunk.len() {
                            out.push(ALPHABET[((v >> (18 - 6 * i)) & 63) as usize] as char);
                        } else {
                            out.push('=');
                        }
                    }
                }
                out
            };
            let wrapped = encoded
                .as_bytes()
                .chunks(76)
                .map(|line| std::str::from_utf8(line).expect("ascii"))
                .collect::<Vec<_>>()
                .join("\r\n");
            let raw = format!(
                concat!(
                    "From: sender@example.com\r\n",
                    "Subject: job\r\n",
                    "MIME-Version: 1.0\r\n",
                    "Content-Type: multipart/mixed; boundary=\"bnd\"\r\n\r\n",
                    "--bnd\r\nContent-Type: text/plain\r\n\r\nHi\r\n",
                    "--bnd\r\n",
                    "Content-Type: application/vnd.openxmlformats-officedocument.wordprocessingml.document; name=\"job.docx\"\r\n",
                    "Content-Disposition: attachment; filename=\"job.docx\"\r\n",
                    "Content-Transfer-Encoding: base64\r\n\r\n",
                    "{}\r\n--bnd--\r\n"
                ),
                wrapped
            );
            let prefix = &raw.as_bytes()[..alcedo_imap::DISPLAY_FULL_MAX_BYTES as usize];
            let full = alcedo_imap::parse_full_message(prefix).expect("parse prefix");
            assert_eq!(full.incomplete_attachments, vec!["job.docx"]);

            save_attachments(&stored, &full, &store)
                .await
                .expect("save attachments");
            let updated = store.get_message(id).await.expect("get").expect("exists");
            assert!(file_attachments_for(&updated).is_empty());

            // Even if a caller hands over the raw text directly, the store
            // guard refuses it.
            let bad = alcedo_imap::FullMessage {
                raw: Vec::new(),
                subject: String::new(),
                text: String::new(),
                html: String::new(),
                list_unsubscribe: None,
                attachments: vec![alcedo_imap::FetchedAttachment {
                    filename: "job.docx".to_string(),
                    cid: None,
                    bytes: wrapped.as_bytes()[..225_550].to_vec(),
                    content_type: (
                        "application".to_string(),
                        "vnd.openxmlformats-officedocument.wordprocessingml.document".to_string(),
                    ),
                    disposition_attachment: true,
                }],
                reaction: None,
                incomplete_attachments: Vec::new(),
            };
            save_attachments(&stored, &bad, &store)
                .await
                .expect("save attachments");
            let updated = store.get_message(id).await.expect("get").expect("exists");
            assert!(file_attachments_for(&updated).is_empty());

            // The complete message stores the exact decoded bytes.
            let whole = alcedo_imap::parse_full_message(raw.as_bytes()).expect("parse whole");
            save_attachments(&stored, &whole, &store)
                .await
                .expect("save attachments");
            let updated = store.get_message(id).await.expect("get").expect("exists");
            let files = file_attachments_for(&updated);
            assert_eq!(files.len(), 1);
            assert_eq!(files[0].size, 423_417);
            let bytes = store
                .attachment_bytes(&files[0].sha256)
                .await
                .expect("bytes");
            assert_eq!(bytes, docx);
        }

        #[tokio::test]
        async fn save_attachments_accepts_gmail_sql_attachment() {
            let (store, _dir) = open_test_store().await;
            let id = setup_message(&store).await;
            let stored = store.get_message(id).await.expect("get").expect("exists");

            let raw = concat!(
                "From: sender@example.com\r\n",
                "To: recipient@example.com\r\n",
                "Subject: script to update fields to match live\r\n",
                "MIME-Version: 1.0\r\n",
                "Content-Type: multipart/mixed; boundary=\"bnd\"\r\n",
                "\r\n",
                "--bnd\r\n",
                "Content-Type: text/plain; charset=utf-8\r\n",
                "\r\n",
                "see attached\r\n",
                "--bnd\r\n",
                "Content-Type: application/sql; name=\"live fields 2026-09-15.sql\"\r\n",
                "Content-Disposition: attachment; filename=\"live fields 2026-09-15.sql\"\r\n",
                "Content-Transfer-Encoding: base64\r\n",
                "Content-ID: <f_mu2rugzc0>\r\n",
                "\r\n",
                "VVNFIFtHaXNCZW5lZml0c1NpdGVdDQpHTw0K\r\n",
                "--bnd--\r\n",
            );
            let full = full_message_with_parsed_attachments(raw);

            save_attachments(&stored, &full, &store)
                .await
                .expect("save attachments");

            let updated = store.get_message(id).await.expect("get").expect("exists");
            assert_eq!(updated.attachments.len(), 1);
            assert_eq!(
                updated.attachments[0].filename,
                "live fields 2026-09-15.sql"
            );
            assert!(updated.has_attachments);
            let files = file_attachments_for(&updated);
            assert_eq!(files.len(), 1);
            assert_eq!(files[0].filename, "live fields 2026-09-15.sql");
        }

        #[tokio::test]
        async fn save_attachments_names_forwarded_message_eml() {
            let (store, _dir) = open_test_store().await;
            let id = setup_message(&store).await;
            let stored = store.get_message(id).await.expect("get").expect("exists");

            let raw = concat!(
                "From: sender@example.com\r\n",
                "To: recipient@example.com\r\n",
                "Subject: fwd test\r\n",
                "MIME-Version: 1.0\r\n",
                "Content-Type: multipart/mixed; boundary=\"bnd\"\r\n",
                "\r\n",
                "--bnd\r\n",
                "Content-Type: text/plain\r\n",
                "\r\n",
                "See attached\r\n",
                "--bnd\r\n",
                "Content-Type: message/rfc822\r\n",
                "\r\n",
                "From: inner@example.com\r\n",
                "To: someone@example.com\r\n",
                "Subject: nested\r\n",
                "\r\n",
                "body text\r\n",
                "--bnd--\r\n",
            );
            let full = full_message_with_parsed_attachments(raw);

            save_attachments(&stored, &full, &store)
                .await
                .expect("save attachments");

            let updated = store.get_message(id).await.expect("get").expect("exists");
            assert_eq!(updated.attachments.len(), 1);
            assert_eq!(updated.attachments[0].filename, "forwarded-message.eml");
        }

        #[tokio::test]
        async fn save_attachments_names_unnamed_calendar_invite() {
            let (store, _dir) = open_test_store().await;
            let id = setup_message(&store).await;
            let stored = store.get_message(id).await.expect("get").expect("exists");

            let raw = concat!(
                "From: sender@example.com\r\n",
                "To: recipient@example.com\r\n",
                "Subject: meeting invite\r\n",
                "MIME-Version: 1.0\r\n",
                "Content-Type: multipart/alternative; boundary=\"alt\"\r\n",
                "\r\n",
                "--alt\r\n",
                "Content-Type: text/plain; charset=utf-8\r\n",
                "\r\n",
                "Team standup tomorrow\r\n",
                "--alt\r\n",
                "Content-Type: text/html; charset=utf-8\r\n",
                "\r\n",
                "<p>Team standup tomorrow</p>\r\n",
                "--alt\r\n",
                "Content-Type: text/calendar; method=REQUEST; charset=utf-8\r\n",
                "\r\n",
                "BEGIN:VCALENDAR\r\n",
                "VERSION:2.0\r\n",
                "METHOD:REQUEST\r\n",
                "BEGIN:VEVENT\r\n",
                "UID:standup@example.com\r\n",
                "DTSTART:20260821T150000Z\r\n",
                "DTEND:20260821T160000Z\r\n",
                "SUMMARY:Team standup\r\n",
                "END:VEVENT\r\n",
                "END:VCALENDAR\r\n",
                "--alt--\r\n",
            );
            let full = full_message_with_parsed_attachments(raw);

            save_attachments(&stored, &full, &store)
                .await
                .expect("save attachments");

            let updated = store.get_message(id).await.expect("get").expect("exists");
            assert_eq!(updated.attachments.len(), 1);
            assert_eq!(updated.attachments[0].filename, "invite.ics");
            assert_eq!(updated.attachments[0].mime, "text/calendar");
        }

        #[tokio::test]
        async fn save_attachments_names_unnamed_x_ics_calendar_invite() {
            let (store, _dir) = open_test_store().await;
            let id = setup_message(&store).await;
            let stored = store.get_message(id).await.expect("get").expect("exists");

            let raw = concat!(
                "From: sender@example.com\r\n",
                "To: recipient@example.com\r\n",
                "Subject: meeting invite\r\n",
                "MIME-Version: 1.0\r\n",
                "Content-Type: multipart/alternative; boundary=\"alt\"\r\n",
                "\r\n",
                "--alt\r\n",
                "Content-Type: text/plain; charset=utf-8\r\n",
                "\r\n",
                "Team standup tomorrow\r\n",
                "--alt\r\n",
                "Content-Type: text/html; charset=utf-8\r\n",
                "\r\n",
                "<p>Team standup tomorrow</p>\r\n",
                "--alt\r\n",
                "Content-Type: application/x-ics; charset=utf-8\r\n",
                "\r\n",
                "BEGIN:VCALENDAR\r\n",
                "VERSION:2.0\r\n",
                "METHOD:REQUEST\r\n",
                "BEGIN:VEVENT\r\n",
                "UID:standup@example.com\r\n",
                "DTSTART:20260821T150000Z\r\n",
                "DTEND:20260821T160000Z\r\n",
                "SUMMARY:Team standup\r\n",
                "END:VEVENT\r\n",
                "END:VCALENDAR\r\n",
                "--alt--\r\n",
            );
            let full = full_message_with_parsed_attachments(raw);

            save_attachments(&stored, &full, &store)
                .await
                .expect("save attachments");

            let updated = store.get_message(id).await.expect("get").expect("exists");
            assert_eq!(updated.attachments.len(), 1);
            assert_eq!(updated.attachments[0].filename, "invite.ics");
        }

        #[tokio::test]
        async fn save_attachments_nested_related_keeps_docx() {
            // Outlook-style multipart/mixed > related > alternative + CID png,
            // plus a sibling .docx — the Spending Resolution shape.
            let raw = concat!(
                "From: sender@example.com\r\n",
                "To: recipient@example.com\r\n",
                "Subject: Re: Spending Resolution\r\n",
                "MIME-Version: 1.0\r\n",
                "Content-Type: multipart/mixed; boundary=\"mix\"\r\n",
                "\r\n",
                "--mix\r\n",
                "Content-Type: multipart/related; boundary=\"rel\"; type=\"multipart/alternative\"\r\n",
                "\r\n",
                "--rel\r\n",
                "Content-Type: multipart/alternative; boundary=\"alt\"\r\n",
                "\r\n",
                "--alt\r\n",
                "Content-Type: text/plain; charset=\"utf-8\"\r\n",
                "\r\n",
                "Hello\r\n",
                "--alt\r\n",
                "Content-Type: text/html; charset=\"utf-8\"\r\n",
                "\r\n",
                "<p>Hello</p>\r\n",
                "--alt--\r\n",
                "--rel\r\n",
                "Content-Type: image/png; name=\"logo.png\"\r\n",
                "Content-Disposition: inline; filename=\"logo.png\"\r\n",
                "Content-ID: <logo@example.com>\r\n",
                "Content-Transfer-Encoding: base64\r\n",
                "\r\n",
                "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVQI12P4z8AAAAMBAQAY3Y20AAAAAElFTkSuQmCC\r\n",
                "--rel--\r\n",
                "--mix\r\n",
                "Content-Type: application/vnd.openxmlformats-officedocument.wordprocessingml.document; name=\"HBV_Board Minutes_8.18.2026.docx\"\r\n",
                "Content-Disposition: attachment; filename=\"HBV_Board Minutes_8.18.2026.docx\"\r\n",
                "Content-Transfer-Encoding: base64\r\n",
                "\r\n",
                "YXJiaXRyYXJ5IGJ5dGVz\r\n",
                "--mix--\r\n",
            );
            let (store, _dir) = open_test_store().await;
            let id = setup_message(&store).await;
            let stored = store.get_message(id).await.expect("get").expect("exists");
            let full = full_message_with_parsed_attachments(raw);
            save_attachments(&stored, &full, &store)
                .await
                .expect("save attachments");

            let updated = store.get_message(id).await.expect("get").expect("exists");
            assert_eq!(updated.attachments.len(), 2);
            assert!(
                updated
                    .attachments
                    .iter()
                    .any(|a| a.filename == "HBV_Board Minutes_8.18.2026.docx")
            );
            assert!(updated.has_attachments);
            assert_eq!(file_attachments_for(&updated).len(), 1);
        }

        #[tokio::test]
        async fn save_attachments_zip_sniffed_docx_is_kept() {
            // PK\x03\x04 — same shape as Outlook's HAMBLIN VILLAGE 2026 CONTRACT.docx:
            // infer reports application/zip, not the OOXML MIME.
            let raw = concat!(
                "From: sender@example.com\r\n",
                "To: recipient@example.com\r\n",
                "Subject: contract\r\n",
                "MIME-Version: 1.0\r\n",
                "Content-Type: multipart/mixed; boundary=\"mix\"\r\n",
                "\r\n",
                "--mix\r\n",
                "Content-Type: text/plain; charset=\"utf-8\"\r\n",
                "\r\n",
                "Please see attached.\r\n",
                "--mix\r\n",
                "Content-Type: application/vnd.openxmlformats-officedocument.wordprocessingml.document; name=\"HAMBLIN VILLAGE 2026 CONTRACT.docx\"\r\n",
                "Content-Disposition: attachment; filename=\"HAMBLIN VILLAGE 2026 CONTRACT.docx\"\r\n",
                "Content-Transfer-Encoding: base64\r\n",
                "\r\n",
                "UEsDBBQABgAIAAAAAAAAAAAAAAAAAAAAAA==\r\n",
                "--mix--\r\n",
            );
            let (store, _dir) = open_test_store().await;
            let id = setup_message(&store).await;
            let stored = store.get_message(id).await.expect("get").expect("exists");
            let full = full_message_with_parsed_attachments(raw);
            save_attachments(&stored, &full, &store)
                .await
                .expect("save attachments");

            let updated = store.get_message(id).await.expect("get").expect("exists");
            assert_eq!(updated.attachments.len(), 1);
            assert_eq!(
                updated.attachments[0].filename,
                "HAMBLIN VILLAGE 2026 CONTRACT.docx"
            );
            assert!(updated.has_attachments);
            assert_eq!(file_attachments_for(&updated).len(), 1);
        }

        #[tokio::test]
        async fn save_attachments_image_disposition_with_cid_is_file() {
            let (store, _dir) = open_test_store().await;
            let id = setup_message(&store).await;
            let stored = store.get_message(id).await.expect("get").expect("exists");
            let full = alcedo_imap::FullMessage {
                raw: Vec::new(),
                subject: String::new(),
                text: String::new(),
                html: String::new(),
                list_unsubscribe: None,
                attachments: vec![alcedo_imap::FetchedAttachment {
                    filename: "A01_sample.jpg".to_string(),
                    cid: Some("photo-d@codeinfusion.com".to_string()),
                    bytes: b"jpeg-a".to_vec(),
                    content_type: ("image".to_string(), "jpeg".to_string()),
                    disposition_attachment: true,
                }],
                reaction: None,
                incomplete_attachments: Vec::new(),
            };
            save_attachments(&stored, &full, &store)
                .await
                .expect("save attachments");
            let updated = store.get_message(id).await.expect("get").expect("exists");
            assert!(updated.has_attachments);
            assert_eq!(file_attachments_for(&updated).len(), 1);
            assert!(updated.attachments[0].cid.is_none());
        }

        #[tokio::test]
        async fn save_attachments_inline_image_cid_is_not_file() {
            let (store, _dir) = open_test_store().await;
            let id = setup_message(&store).await;
            let stored = store.get_message(id).await.expect("get").expect("exists");
            let full = alcedo_imap::FullMessage {
                raw: Vec::new(),
                subject: String::new(),
                text: String::new(),
                html: String::new(),
                list_unsubscribe: None,
                attachments: vec![alcedo_imap::FetchedAttachment {
                    filename: "logo.jpg".to_string(),
                    cid: Some("logo@example.com".to_string()),
                    bytes: b"jpeg-a".to_vec(),
                    content_type: ("image".to_string(), "jpeg".to_string()),
                    disposition_attachment: false,
                }],
                reaction: None,
                incomplete_attachments: Vec::new(),
            };
            save_attachments(&stored, &full, &store)
                .await
                .expect("save attachments");
            let updated = store.get_message(id).await.expect("get").expect("exists");
            assert!(!updated.has_attachments);
            assert!(file_attachments_for(&updated).is_empty());
        }

        #[tokio::test]
        async fn persist_sent_copy_writes_rows() {
            let (store, _dir) = open_test_store().await;
            let id = setup_message(&store).await;
            let stored = store.get_message(id).await.expect("get").expect("exists");
            store
                .set_message_id_hdrs(
                    stored.folder_id,
                    &[(
                        stored.uid,
                        "<fixture-mixed-jpegs@codeinfusion.com>".to_string(),
                    )],
                )
                .await
                .expect("message-id hdr");
            let raw = include_str!("../../../alcedo-imap/tests/fixtures/multipart-mixed-jpegs.eml");
            let attachments = alcedo_imap::parse_full_message(raw.as_bytes())
                .expect("parse")
                .attachments;
            apply_sent_parts(&store, id, &attachments)
                .await
                .expect("apply sent parts");
            let updated = store.get_message(id).await.expect("get").expect("exists");
            assert!(updated.has_attachments);
            assert_eq!(updated.attachments.len(), 2);
            assert_eq!(updated.attachments[0].filename, "A01_sample.jpg");
            assert_eq!(updated.attachments[1].filename, "D01_sample.jpg");
        }

        #[tokio::test]
        async fn emit_folder_envelopes_conversation_mode_emits_grouped_list() {
            let (store, _dir) = open_test_store().await;
            let account = sample_account(Uuid::new_v4(), "conv-loop@example.com");
            store
                .upsert_account(&account)
                .await
                .expect("upsert account");
            let folders = store
                .replace_folders(account.id, &[sample_folder(account.id, "INBOX")])
                .await
                .expect("folders");
            let folder_id = folders[0].id;
            store
                .upsert_envelopes(&[sample_envelope(&account, folder_id, 1, "Hello")], None)
                .await
                .expect("envelope");

            crate::aggregator::set_list_mode(MessageListMode::Conversation);
            let (tx, rx) = flume::unbounded();
            emit_folder_envelopes(&store, folder_id, &tx)
                .await
                .expect("emit");

            match rx.recv_async().await.expect("appended") {
                SyncEvent::EnvelopesAppended { folder, envelopes } => {
                    assert_eq!(folder, folder_id);
                    assert_eq!(envelopes.len(), 1);
                }
                other => panic!("expected EnvelopesAppended, got {other:?}"),
            }
            match rx.recv_async().await.expect("conversations") {
                SyncEvent::ConversationsReplaced {
                    conversations,
                    kind,
                } => {
                    assert_eq!(conversations.len(), 1);
                    assert_eq!(kind, alcedo_core::EnvelopeListKind::Folder(folder_id));
                }
                other => panic!("expected ConversationsReplaced, got {other:?}"),
            }
            assert!(rx.try_recv().is_err(), "must not emit a third list event");
        }
    }
}
