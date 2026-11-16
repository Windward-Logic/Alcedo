//! Sent folder copies: APPEND, Message-ID dedupe and attachment structure for sent rows.

use std::time::Duration;

use super::backfill::{persist_fetched_message_ids, persist_fetched_sizes};
use super::bodies::{
    emit_body_ready_from_stored, recover_html_body, save_attachments, stored_has_local_body,
};
use super::drafts::{draft_message_id_eq, normalize_draft_message_id};
use super::mailbox_sync::{emit_folder_envelopes, ensure_selected, fetched_to_envelope};
use super::moves::sync_destination_folder;
use super::send::resolve_compose_from;
use super::session::is_recoverable_disconnect;
use super::*;

/// How long an APPEND may take before the command connection is given up.
///
/// Waiting with no limit is what leaves Sent empty and the reader on "Loading
/// message…": both need this connection. But a limit that a slow uplink cannot
/// meet is worse: the upload is cut off mid-literal, the session reconnects,
/// the copy is retried, and it is cut off again. This allows at least 32 KiB/s
/// (the same assumption as the SMTP send that produced these bytes) and caps
/// at ten minutes.
pub(super) fn append_timeout_for_len(bytes: usize) -> Duration {
    const BASE_SECS: u64 = 45;
    const CAP_SECS: u64 = 600;
    const MIN_BYTES_PER_SEC: usize = 32 * 1024;
    let extra = (bytes / MIN_BYTES_PER_SEC) as u64;
    Duration::from_secs((BASE_SECS + extra).min(CAP_SECS))
}

/// Placeholder UID for a sent row that does not have a server UID yet.
///
/// Real IMAP UIDs grow from 1. `u32::MAX` is not a UID a server will hand out,
/// so a later sync cannot overwrite this row with a different message.
pub(super) fn pick_local_sent_uid(taken: &[u32]) -> u32 {
    let mut uid = u32::MAX;
    while taken.contains(&uid) {
        uid = uid.saturating_sub(1);
        if uid == 0 {
            break;
        }
    }
    uid
}

pub(super) fn sent_folder(folders: &[Folder]) -> Option<&Folder> {
    folders
        .iter()
        .find(|folder| folder.special == SpecialUse::Sent && folder.selectable)
}

/// APPEND only when Sent has no UID for this Message-ID (provider auto-save
/// already landed). Empty hits include "SEARCH not possible" so generic IMAP
/// still files a copy.
pub(crate) fn should_append_sent_copy(header_hits: &[u32]) -> bool {
    header_hits.is_empty()
}

/// Which copy of a duplicated Message-ID is the one worth keeping.
///
/// Sent keeps the provider auto-save that landed first. Drafts saves by
/// APPEND + delete, so the highest UID is the newest body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KeepDupe {
    LowestUid,
    HighestUid,
}

/// Extra UIDs for each Message-ID; these are APPEND/auto-save dupes.
pub(crate) fn extra_uids_for_message_id_dupes(rows: &[(u32, String)], keep: KeepDupe) -> Vec<u32> {
    let mut by_hdr: HashMap<String, Vec<u32>> = HashMap::new();
    for (uid, hdr) in rows {
        let key = normalize_draft_message_id(hdr);
        if key.is_empty() {
            continue;
        }
        by_hdr.entry(key).or_default().push(*uid);
    }
    let mut extra = Vec::new();
    for uids in by_hdr.values_mut() {
        uids.sort_unstable();
        match keep {
            KeepDupe::LowestUid => extra.extend(uids.iter().copied().skip(1)),
            KeepDupe::HighestUid => extra.extend(uids.iter().copied().take(uids.len() - 1)),
        }
    }
    extra.sort_unstable();
    extra
}

pub(super) fn message_id_dupe_policy(folder: &Folder) -> Option<KeepDupe> {
    match folder.special {
        SpecialUse::Sent => Some(KeepDupe::LowestUid),
        SpecialUse::Drafts => Some(KeepDupe::HighestUid),
        _ => None,
    }
}

pub(super) async fn maybe_dedupe_message_ids(
    client: &mut ImapClient,
    folder: &Folder,
    store: &StoreHandle,
) -> bool {
    let Some(keep) = message_id_dupe_policy(folder) else {
        return false;
    };
    match dedupe_message_ids(client, folder, store, keep).await {
        Ok(n) if n > 0 => true,
        Ok(_) => false,
        Err(err) => {
            warn!(
                error = %err,
                folder = %folder.imap_name,
                "failed to dedupe Message-IDs"
            );
            false
        }
    }
}

pub(super) async fn dedupe_message_ids(
    client: &mut ImapClient,
    folder: &Folder,
    store: &StoreHandle,
    keep: KeepDupe,
) -> Result<usize> {
    let rows = store.live_message_id_hdrs(folder.id).await?;
    let extra = extra_uids_for_message_id_dupes(&rows, keep);
    if extra.is_empty() {
        return Ok(0);
    }
    debug!(
        folder = %folder.imap_name,
        extra = extra.len(),
        ?keep,
        "expunging duplicate Message-IDs"
    );
    let count = extra.len();
    for chunk_set in format_uid_set_chunks(&extra, 250) {
        client.uid_delete_set(&chunk_set).await?;
    }
    store.mark_deleted_by_uids(folder.id, extra).await?;
    Ok(count)
}

pub(super) fn take_parsed_file_parts(rfc822: &[u8]) -> Vec<FetchedAttachment> {
    alcedo_imap::parse_full_message(rfc822)
        .map(|message| message.attachments)
        .unwrap_or_default()
}

pub(super) async fn sent_row_for_message_id(
    store: &StoreHandle,
    folder_id: FolderId,
    hdr: &str,
) -> Result<Option<MessageId>> {
    let rows = store.live_message_id_hdrs(folder_id).await?;
    let Some(uid) = rows
        .iter()
        .find(|(_, id)| draft_message_id_eq(id, hdr))
        .map(|(uid, _)| *uid)
    else {
        return Ok(None);
    };
    let messages = store.messages_by_uids(folder_id, &[uid]).await?;
    Ok(messages.first().map(|(id, _, _)| *id))
}

pub(super) async fn apply_sent_parts(
    store: &StoreHandle,
    message_id: MessageId,
    attachments: &[FetchedAttachment],
) -> Result<()> {
    let stored = store
        .get_message(message_id)
        .await?
        .ok_or_else(|| Error::Store("message not found for sent parts".to_string()))?;
    let full = alcedo_imap::FullMessage {
        raw: Vec::new(),
        subject: String::new(),
        text: String::new(),
        html: String::new(),
        list_unsubscribe: None,
        attachments: attachments.to_vec(),
        reaction: None,
        incomplete_attachments: Vec::new(),
    };
    let _ = save_attachments(&stored, &full, store).await?;
    Ok(())
}

pub(super) fn push_pending_sent_attachments(
    state: &mut WorkerImapState,
    folder_id: FolderId,
    message_id: String,
    attachments: Vec<FetchedAttachment>,
) {
    state.pending_sent.push(PendingSentAttachments {
        folder_id,
        message_id,
        attachments,
    });
    while state.pending_sent.len() > MAX_PENDING_SENT_ATTACHMENTS {
        state.pending_sent.remove(0);
    }
}

pub(super) async fn apply_sent_copy_attachments(
    state: &mut WorkerImapState,
    account: &Account,
    sent: &Folder,
    rfc822: &[u8],
    message_id_hdr: &str,
    store: &StoreHandle,
) -> Result<()> {
    let attachments = take_parsed_file_parts(rfc822);
    if attachments.is_empty() {
        return Ok(());
    }
    let hdr = normalize_draft_message_id(message_id_hdr);
    if hdr.is_empty() {
        return Ok(());
    }
    match sent_row_for_message_id(store, sent.id, &hdr).await {
        Ok(Some(message_id)) => {
            if let Err(err) = apply_sent_parts(store, message_id, &attachments).await {
                warn!(?message_id, error = %err, "failed to apply sent attachment parts");
            }
            return Ok(());
        }
        Ok(None) => {}
        Err(err) => {
            warn!(error = %err, "failed to resolve sent row for attachment apply");
        }
    }

    let hits = match search_sent_message_id(state, &hdr).await {
        Ok(hits) => hits,
        Err(err) if is_recoverable_disconnect(&err) || is_auth_failure(&err) => return Err(err),
        Err(err) => {
            warn!(error = %err, "sent Message-ID search failed during attachment apply");
            Vec::new()
        }
    };
    if !hits.is_empty() {
        let mut resolved = false;
        for uid_set in format_uid_set_chunks(&hits, 250) {
            match with_imap_timeout(
                IMAP_COMMAND_TIMEOUT,
                uid_fetch_envelopes(state.client_mut(), sent.id, &uid_set, None),
            )
            .await
            {
                Ok(fetched) => {
                    let envelopes: Vec<alcedo_core::Envelope> = fetched
                        .iter()
                        .map(|f| fetched_to_envelope(f, account, sent.id))
                        .collect();
                    if let Err(err) = store.upsert_envelopes(&envelopes, None).await {
                        warn!(error = %err, "failed to upsert sent envelopes for attachment apply");
                        continue;
                    }
                    if let Err(err) = persist_fetched_message_ids(store, sent.id, &fetched).await {
                        warn!(error = %err, "failed to persist sent Message-IDs for attachment apply");
                    }
                    if let Err(err) = persist_fetched_sizes(store, sent.id, &fetched).await {
                        warn!(error = %err, "failed to persist sent message sizes");
                    }
                    if let Ok(Some(message_id)) =
                        sent_row_for_message_id(store, sent.id, &hdr).await
                    {
                        if let Err(err) = apply_sent_parts(store, message_id, &attachments).await {
                            warn!(?message_id, error = %err, "failed to apply sent attachment parts after fetch");
                        }
                        resolved = true;
                        break;
                    }
                }
                Err(err) if is_recoverable_disconnect(&err) || is_auth_failure(&err) => {
                    return Err(err);
                }
                Err(err) => {
                    warn!(error = %err, "UID FETCH sent envelopes for attachment apply failed");
                }
            }
        }
        if resolved {
            return Ok(());
        }
    }

    push_pending_sent_attachments(state, sent.id, hdr, attachments);
    Ok(())
}

pub(super) async fn apply_pending_sent_attachments(
    state: &mut WorkerImapState,
    folder: &Folder,
    store: &StoreHandle,
) -> Result<()> {
    let mut remaining = Vec::new();
    for pending in state.pending_sent.drain(..) {
        if pending.folder_id != folder.id {
            remaining.push(pending);
            continue;
        }
        match sent_row_for_message_id(store, folder.id, &pending.message_id).await {
            Ok(Some(message_id)) => {
                if let Err(err) = apply_sent_parts(store, message_id, &pending.attachments).await {
                    warn!(?message_id, error = %err, "failed to apply pending sent attachments");
                }
            }
            Ok(None) => remaining.push(pending),
            Err(err) => {
                warn!(error = %err, "pending sent attachment resolve failed");
                remaining.push(pending);
            }
        }
    }
    state.pending_sent = remaining;
    Ok(())
}

pub(super) async fn run_sent_attachment_structure_backfill(
    state: &mut WorkerImapState,
    folder: &Folder,
    store: &StoreHandle,
) -> Result<()> {
    if store.sent_structure_scan_done(folder.account_id).await? {
        return Ok(());
    }
    let rows = store.sent_uids_missing_attachment_flag(folder.id).await?;
    if rows.is_empty() {
        store
            .mark_sent_structure_scan_done(folder.account_id)
            .await?;
        return Ok(());
    }
    let uids: Vec<u32> = rows.iter().map(|(_, uid)| *uid).collect();
    let id_by_uid: HashMap<u32, MessageId> = rows.into_iter().map(|(id, uid)| (uid, id)).collect();

    for chunk in format_uid_set_chunks(&uids, 250) {
        match uid_fetch_attachment_flags(state.client_mut(), &chunk).await {
            Ok(flags) => {
                for (uid, has) in flags {
                    if has
                        && let Some(id) = id_by_uid.get(&uid)
                        && let Err(err) = store.set_has_attachments_true(*id).await
                    {
                        warn!(?id, error = %err, "failed to set has_attachments from Sent scan");
                    }
                }
            }
            Err(err) if is_recoverable_disconnect(&err) || is_auth_failure(&err) => {
                return Err(err);
            }
            Err(err) => {
                warn!(%chunk, error = %err, "Sent BODYSTRUCTURE chunk failed; continuing");
            }
        }
    }

    store
        .mark_sent_structure_scan_done(folder.account_id)
        .await?;
    Ok(())
}

/// Store the message we just sent and tell the UI, before any IMAP round trip.
pub(super) async fn publish_local_sent_copy(
    account: &Account,
    draft: &ComposeDraft,
    rfc822: &[u8],
    message_id_hdr: &str,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
) -> Option<LocalSentRow> {
    match publish_local_sent_copy_inner(account, draft, rfc822, message_id_hdr, store, events).await
    {
        Ok(row) => row,
        Err(err) => {
            warn!(error = %err, "failed to show the sent message before the server copy");
            None
        }
    }
}

async fn publish_local_sent_copy_inner(
    account: &Account,
    draft: &ComposeDraft,
    rfc822: &[u8],
    message_id_hdr: &str,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
) -> Result<Option<LocalSentRow>> {
    let hdr = normalize_draft_message_id(message_id_hdr);
    if hdr.is_empty() {
        return Ok(None);
    }
    let folders = store.list_folders(account.id).await?;
    let Some(sent) = sent_folder(&folders) else {
        return Ok(None);
    };

    if let Some(message_id) = sent_row_for_message_id(store, sent.id, &hdr).await? {
        let Some(stored) = store.get_message(message_id).await? else {
            return Ok(None);
        };
        if stored_has_local_body(&stored) {
            emit_body_ready_from_stored(store, &stored, events).await;
        }
        emit_folder_envelopes(store, sent.id, events).await?;
        return Ok(Some(LocalSentRow {
            message_id,
            folder_id: sent.id,
            uid: stored.uid,
        }));
    }

    let uid = allocate_local_sent_uid(store, sent.id).await?;
    let full = full_message_for_local_sent(rfc822, draft);
    let (from_email, from_name) = resolve_compose_from(account, draft, store).await;
    let text = sent_body_text(draft, &full);
    let thread_id = draft
        .thread_id
        .unwrap_or_else(|| alcedo_core::orphan_thread_id(sent.id, uid));
    let envelope = alcedo_core::Envelope {
        id: MessageId(0),
        account_id: account.id,
        folder_id: sent.id,
        uid,
        thread_id,
        date: chrono::Utc::now(),
        subject: if draft.subject.is_empty() {
            full.subject.clone()
        } else {
            draft.subject.clone()
        },
        from: vec![alcedo_core::Address {
            name: (!from_name.is_empty()).then(|| from_name.clone()),
            email: from_email,
        }],
        to: addresses_from_draft_field(&draft.to),
        cc: addresses_from_draft_field(&draft.cc),
        reply_to: Vec::new(),
        sender: Vec::new(),
        routing: None,
        snippet: preview_snippet(&text),
        seen: true,
        flagged: false,
        answered: false,
        draft: false,
        has_attachments: !draft.attachments.is_empty() || !full.attachments.is_empty(),
        deleted_at: None,
        account_accent: account.accent_color.clone(),
        account_avatar: account.avatar_text.clone(),
        size_bytes: rfc822.len() as i64,
    };
    store
        .upsert_envelopes(&[envelope], Some(text.as_str()))
        .await?;
    store
        .set_threading_hdrs(
            sent.id,
            &[(
                uid,
                hdr.clone(),
                draft.in_reply_to.clone(),
                draft.references.clone(),
            )],
        )
        .await?;
    store
        .set_rfc822_sizes(sent.id, &[(uid, rfc822.len() as i64)])
        .await?;

    let Some(stored) = store
        .get_message(
            sent_row_for_message_id(store, sent.id, &hdr)
                .await?
                .ok_or_else(|| Error::Store("local sent row missing after insert".to_string()))?,
        )
        .await?
    else {
        return Err(Error::Store(
            "local sent row missing after insert".to_string(),
        ));
    };
    let message_id = stored.id;
    if let Some(emoji) = draft
        .reaction_emoji
        .as_deref()
        .filter(|emoji| alcedo_core::is_single_reaction_emoji(emoji))
    {
        let target = draft
            .in_reply_to
            .as_deref()
            .and_then(alcedo_core::bare_reaction_target_hdr);
        store
            .save_reaction(message_id, Some(emoji), target.as_deref())
            .await?;
    }
    let cid_map = save_attachments(&stored, &full, store).await?;
    let html_source = if !full.html.is_empty() {
        full.html.clone()
    } else if matches!(draft.format, ComposeFormat::Html) {
        draft.body_text.clone()
    } else {
        String::new()
    };
    let parsed_html = if html_source.is_empty() {
        String::new()
    } else {
        rewrite_cids(&html_source, &cid_map)
    };
    let html = recover_html_body(&parsed_html, &text);
    let html = if html == parsed_html {
        html
    } else {
        rewrite_cids(&html, &cid_map)
    };
    store
        .save_message_body(message_id, Some(&text), Some(&html), None)
        .await?;
    if let Some(list) = full.list_unsubscribe.as_ref() {
        store.save_list_unsubscribe(message_id, Some(list)).await?;
    }
    let stored = store
        .get_message(message_id)
        .await?
        .ok_or_else(|| Error::Store("local sent row missing after body save".to_string()))?;
    emit_body_ready_from_stored(store, &stored, events).await;
    emit_folder_envelopes(store, sent.id, events).await?;
    debug!(
        folder = %sent.imap_name,
        uid,
        bytes = rfc822.len(),
        "showed sent message from the SMTP bytes"
    );
    Ok(Some(LocalSentRow {
        message_id,
        folder_id: sent.id,
        uid,
    }))
}

async fn allocate_local_sent_uid(store: &StoreHandle, folder_id: FolderId) -> Result<u32> {
    let mut taken = Vec::new();
    let mut candidate = u32::MAX;
    for _ in 0..8 {
        let rows = store.messages_by_uids(folder_id, &[candidate]).await?;
        if rows.is_empty() {
            return Ok(candidate);
        }
        taken.push(candidate);
        candidate = pick_local_sent_uid(&taken);
        if candidate == 0 {
            break;
        }
    }
    store.next_local_uid(folder_id).await
}

fn sent_body_text(draft: &ComposeDraft, full: &alcedo_imap::FullMessage) -> String {
    if !full.text.is_empty() {
        return full.text.clone();
    }
    if !draft.body_text.is_empty() && !matches!(draft.format, ComposeFormat::Html) {
        return draft.body_text.clone();
    }
    if !full.html.is_empty() {
        return html_to_snippet(&full.html, 50_000);
    }
    if matches!(draft.format, ComposeFormat::Html) {
        return html_to_snippet(&draft.body_text, 50_000);
    }
    String::new()
}

fn full_message_for_local_sent(rfc822: &[u8], draft: &ComposeDraft) -> alcedo_imap::FullMessage {
    match alcedo_imap::parse_full_message(rfc822) {
        Ok(mut full) => {
            if full.attachments.is_empty() {
                full.attachments = draft_file_parts(draft);
            }
            full
        }
        Err(err) => {
            warn!(error = %err, "could not parse sent message; showing the composed body");
            alcedo_imap::FullMessage {
                raw: rfc822.to_vec(),
                subject: draft.subject.clone(),
                text: draft.body_text.clone(),
                html: String::new(),
                list_unsubscribe: None,
                attachments: draft_file_parts(draft),
                reaction: None,
                incomplete_attachments: Vec::new(),
            }
        }
    }
}

fn draft_file_parts(draft: &ComposeDraft) -> Vec<alcedo_imap::FetchedAttachment> {
    let mut parts = Vec::new();
    for attachment in &draft.attachments {
        parts.push(alcedo_imap::FetchedAttachment {
            filename: attachment.filename.clone(),
            cid: None,
            bytes: attachment.bytes.clone(),
            content_type: mime_pair(&attachment.mime),
            disposition_attachment: true,
        });
    }
    for inline in &draft.inlines {
        parts.push(alcedo_imap::FetchedAttachment {
            filename: inline.filename.clone(),
            cid: Some(inline.content_id.clone()),
            bytes: inline.bytes.clone(),
            content_type: mime_pair(&inline.mime),
            disposition_attachment: false,
        });
    }
    parts
}

fn mime_pair(mime: &str) -> (String, String) {
    match mime.split_once('/') {
        Some((ty, sub)) if !ty.is_empty() && !sub.is_empty() => (ty.to_string(), sub.to_string()),
        _ => ("application".to_string(), "octet-stream".to_string()),
    }
}

fn addresses_from_draft_field(field: &[String]) -> Vec<alcedo_core::Address> {
    field
        .iter()
        .filter_map(|token| alcedo_core::parse_address_token(token))
        .collect()
}

pub(super) fn should_sync_sent_after_file(
    placeholder: Option<&LocalSentRow>,
    resolved: Option<u32>,
    rehomed: bool,
) -> bool {
    match placeholder {
        None => true,
        Some(_) => resolved.is_some() && rehomed,
    }
}

pub(super) async fn file_sent_copy(
    state: &mut WorkerImapState,
    account: &Account,
    rfc822: &[u8],
    message_id_hdr: &str,
    placeholder: Option<LocalSentRow>,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
) -> Result<()> {
    let Ok(folders) = store.list_folders(account.id).await else {
        return Ok(());
    };
    let Some(sent) = sent_folder(&folders).cloned() else {
        debug!("no selectable Sent folder; skip sent copy");
        return Ok(());
    };

    emit_status(events, account, "Saving to Sent…".to_string());
    let resolved = match ensure_sent_copy(state, &sent, rfc822, message_id_hdr).await {
        Ok(uid) => uid,
        Err(err) if is_recoverable_disconnect(&err) || is_auth_failure(&err) => return Err(err),
        Err(err) => {
            warn!(
                error = %err,
                folder = %sent.imap_name,
                "failed to ensure sent copy"
            );
            None
        }
    };
    let mut rehomed = false;
    if let (Some(placeholder), Some(uid)) = (placeholder.as_ref(), resolved) {
        if placeholder.folder_id == sent.id && placeholder.uid != uid {
            match store
                .rehome_message(placeholder.message_id, sent.id, uid, None)
                .await
            {
                Ok(true) => {
                    rehomed = true;
                }
                Ok(false) => {
                    warn!(uid, "sent placeholder collided with an existing row");
                    rehomed = true;
                }
                Err(err) => {
                    warn!(error = %err, uid, "failed to move local sent row onto the server UID")
                }
            }
        } else {
            rehomed = true;
        }
    }

    if should_sync_sent_after_file(placeholder.as_ref(), resolved, rehomed) {
        sync_destination_folder(state, account, sent.id, store, events).await;
    }
    apply_sent_copy_attachments(state, account, &sent, rfc822, message_id_hdr, store).await
}

pub(super) async fn ensure_sent_copy(
    state: &mut WorkerImapState,
    sent: &Folder,
    rfc822: &[u8],
    message_id_hdr: &str,
) -> Result<Option<u32>> {
    ensure_selected(state, &sent.imap_name).await?;
    let hits = search_sent_message_id(state, message_id_hdr).await?;
    if !should_append_sent_copy(&hits) {
        let uid = hits.iter().copied().min();
        debug!(
            folder = %sent.imap_name,
            ?uid,
            "Sent already has Message-ID; skip APPEND"
        );
        return Ok(uid);
    }
    let appended = append_sent_copy(state, sent, rfc822).await?;
    if appended.uid.is_some() {
        return Ok(appended.uid);
    }
    let hits = search_sent_message_id(state, message_id_hdr).await?;
    Ok(hits.iter().copied().max())
}

pub(super) async fn search_sent_message_id(
    state: &mut WorkerImapState,
    message_id_hdr: &str,
) -> Result<Vec<u32>> {
    let hdr = message_id_hdr.trim();
    if hdr.is_empty() {
        return Ok(Vec::new());
    }
    let criteria = imap_header_message_id_search(hdr);
    with_imap_timeout(
        IMAP_COMMAND_TIMEOUT,
        state.client_mut().uid_search(&criteria),
    )
    .await
}

pub(super) async fn append_sent_copy(
    state: &mut WorkerImapState,
    sent: &Folder,
    rfc822: &[u8],
) -> Result<alcedo_imap::AppendedMail> {
    debug!(
        mailbox = %sent.imap_name,
        bytes = rfc822.len(),
        "APPEND sent copy"
    );
    with_imap_timeout(
        append_timeout_for_len(rfc822.len()),
        state
            .client_mut()
            .append_rfc822_with_flags(&sent.imap_name, rfc822, r"(\Seen)"),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use alcedo_core::{AccountId, FolderId, SpecialUse};
    use uuid::Uuid;

    #[test]
    fn append_timeout_grows_with_the_message_and_caps() {
        assert_eq!(append_timeout_for_len(0).as_secs(), 45);
        assert_eq!(append_timeout_for_len(32 * 1024).as_secs(), 46);
        assert_eq!(append_timeout_for_len(5 * 1024 * 1024).as_secs(), 205);
        assert_eq!(append_timeout_for_len(100 * 1024 * 1024).as_secs(), 600);
    }

    #[test]
    fn local_sent_uid_stays_above_real_imap_uids() {
        assert_eq!(pick_local_sent_uid(&[]), u32::MAX);
        assert_eq!(pick_local_sent_uid(&[u32::MAX]), u32::MAX - 1);
        assert_eq!(pick_local_sent_uid(&[12, 40]), u32::MAX);
    }

    #[test]
    fn should_append_sent_copy_when_header_search_misses() {
        assert!(should_append_sent_copy(&[]));
        assert!(!should_append_sent_copy(&[12]));
        assert!(!should_append_sent_copy(&[8, 12]));
    }

    #[test]
    fn extra_sent_uids_keep_lowest_uid_per_message_id() {
        let keep = KeepDupe::LowestUid;
        assert!(extra_uids_for_message_id_dupes(&[], keep).is_empty());
        assert!(extra_uids_for_message_id_dupes(&[(10, "<a@b>".into())], keep).is_empty());
        assert_eq!(
            extra_uids_for_message_id_dupes(
                &[
                    (12, "<a@b>".into()),
                    (10, " <a@b> ".into()),
                    (8, "<c@d>".into()),
                    (3, String::new()),
                    (4, "  ".into()),
                ],
                keep
            ),
            vec![12]
        );
        assert_eq!(
            extra_uids_for_message_id_dupes(
                &[
                    (7, "<a@b>".into()),
                    (5, "<a@b>".into()),
                    (11, "<c@d>".into()),
                    (9, "<c@d>".into()),
                ],
                keep
            ),
            vec![7, 11]
        );
    }

    #[test]
    fn extra_draft_uids_keep_highest_uid_per_message_id() {
        let keep = KeepDupe::HighestUid;
        assert!(extra_uids_for_message_id_dupes(&[], keep).is_empty());
        assert!(extra_uids_for_message_id_dupes(&[(124, "<a@b>".into())], keep).is_empty());
        // One compose session: every autosave APPENDed a new UID.
        assert_eq!(
            extra_uids_for_message_id_dupes(
                &[
                    (121, "8d90c177@example.com".into()),
                    (122, "8d90c177@example.com".into()),
                    (123, "8d90c177@example.com".into()),
                    (124, "8d90c177@example.com".into()),
                    (7, String::new()),
                ],
                keep
            ),
            vec![121, 122, 123]
        );
    }

    #[test]
    fn extra_draft_uids_group_bracketed_with_parsed_header() {
        // persist_appended_imap_draft writes "<id>"; sync rewrites it unbracketed.
        assert_eq!(
            extra_uids_for_message_id_dupes(
                &[(11, "a@b.com".into()), (12, "<A@B.com>".into()),],
                KeepDupe::HighestUid
            ),
            vec![11]
        );
    }

    #[test]
    fn message_id_dupe_policy_covers_sent_and_drafts_only() {
        let folder_for = |special: SpecialUse| Folder {
            id: FolderId(3),
            account_id: AccountId(Uuid::nil()),
            imap_name: "box".to_string(),
            delimiter: "/".to_string(),
            special,
            selectable: true,
            uidvalidity: None,
            uidnext: None,
            highest_modseq: None,
            unread: 0,
            total: 0,
        };
        assert_eq!(
            message_id_dupe_policy(&folder_for(SpecialUse::Sent)),
            Some(KeepDupe::LowestUid)
        );
        assert_eq!(
            message_id_dupe_policy(&folder_for(SpecialUse::Drafts)),
            Some(KeepDupe::HighestUid)
        );
        assert_eq!(message_id_dupe_policy(&folder_for(SpecialUse::Inbox)), None);
        assert_eq!(message_id_dupe_policy(&folder_for(SpecialUse::Trash)), None);
    }

    #[test]
    fn sent_folder_requires_selectable_sent() {
        let sent = Folder {
            id: FolderId(2),
            account_id: AccountId(Uuid::nil()),
            imap_name: "Sent".to_string(),
            delimiter: "/".to_string(),
            special: SpecialUse::Sent,
            selectable: true,
            uidvalidity: None,
            uidnext: None,
            highest_modseq: None,
            unread: 0,
            total: 0,
        };
        let drafts = Folder {
            id: FolderId(3),
            account_id: AccountId(Uuid::nil()),
            imap_name: "Drafts".to_string(),
            delimiter: "/".to_string(),
            special: SpecialUse::Drafts,
            selectable: true,
            uidvalidity: None,
            uidnext: None,
            highest_modseq: None,
            unread: 0,
            total: 0,
        };
        assert_eq!(
            sent_folder(&[drafts.clone(), sent.clone()]).map(|f| f.id),
            Some(FolderId(2))
        );
        let unselectable = Folder {
            selectable: false,
            ..sent
        };
        assert!(sent_folder(&[unselectable, drafts]).is_none());
    }

    #[test]
    fn should_sync_sent_after_file_skips_when_placeholder_unresolved() {
        let placeholder = LocalSentRow {
            message_id: MessageId(1),
            folder_id: FolderId(2),
            uid: u32::MAX,
        };
        assert!(!should_sync_sent_after_file(
            Some(&placeholder),
            None,
            false
        ));
        assert!(!should_sync_sent_after_file(
            Some(&placeholder),
            Some(42),
            false
        ));
        assert!(should_sync_sent_after_file(
            Some(&placeholder),
            Some(42),
            true
        ));
        assert!(should_sync_sent_after_file(None, None, false));
    }
}
