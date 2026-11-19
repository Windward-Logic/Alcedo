//! IMAP Drafts: open, save (APPEND), UID resolution, replacement and delete.
#![allow(clippy::too_many_arguments)]

use super::bodies::save_attachments;
use super::mailbox_sync::{
    emit_folder_envelopes, ensure_selected, folder_is_syncable, select_mailbox_if_needed,
    sync_selected_mailbox,
};
use super::moves::resolve_moved_uid;
use super::send::{envelope_email_for_compose, resolve_compose_from, validate_compose_draft};
use super::*;
use alcedo_i18n::tr;

/// A freshly APPENDed draft row carries the bracketed header we sent, while
/// synced rows carry the parsed (unbracketed) form. Compare on this key.
pub(crate) fn normalize_draft_message_id(id: &str) -> String {
    let id = id.trim();
    let id = id.strip_prefix('<').unwrap_or(id);
    let id = id.strip_suffix('>').unwrap_or(id);
    id.trim().to_ascii_lowercase()
}

pub(super) fn draft_message_id_eq(left: &str, right: &str) -> bool {
    let left = normalize_draft_message_id(left);
    let right = normalize_draft_message_id(right);
    !left.is_empty() && left == right
}

pub(super) fn recent_append_matches(
    recent: &RecentDraftAppend,
    folder: &Folder,
    message_id_hdr: Option<&str>,
    uid: u32,
) -> bool {
    if recent.folder_id != folder.id || recent.at.elapsed() > Duration::from_secs(120) {
        return false;
    }
    if recent.uid == uid {
        return true;
    }
    let hdr = message_id_hdr.unwrap_or("");
    !hdr.is_empty() && draft_message_id_eq(&recent.message_id, hdr)
}

pub(super) async fn resolve_relocated_draft_uid(
    state: &mut WorkerImapState,
    stored: &alcedo_store::StoredMessage,
    folder: &Folder,
    current_uid: u32,
) -> Result<Option<u32>> {
    if state.client_mut().uid_is_present(current_uid).await? {
        return Ok(None);
    }
    if let Some(recent) = state.recent_draft_append.as_ref()
        && recent_append_matches(
            recent,
            folder,
            stored.message_id_hdr.as_deref(),
            current_uid,
        )
        && recent.uid != current_uid
    {
        let recent_uid = recent.uid;
        if state.client_mut().uid_is_present(recent_uid).await? {
            return Ok(Some(recent_uid));
        }
    }
    if let Some(hdr) = stored
        .message_id_hdr
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        let criteria = imap_header_message_id_search(hdr);
        let uids = state.client_mut().uid_search(&criteria).await?;
        if let Some(uid) = uids.iter().copied().max()
            && uid != current_uid
            && state.client_mut().uid_is_present(uid).await?
        {
            return Ok(Some(uid));
        }
    }
    Ok(None)
}

pub(super) async fn draft_uid_confirmed_absent(
    state: &mut WorkerImapState,
    stored: &alcedo_store::StoredMessage,
    folder: &Folder,
    purge_uid: u32,
) -> Result<bool> {
    if let Some(recent) = &state.recent_draft_append
        && recent_append_matches(recent, folder, stored.message_id_hdr.as_deref(), purge_uid)
    {
        return Ok(false);
    }
    if state.client_mut().uid_is_present(purge_uid).await? {
        return Ok(false);
    }
    if let Some(hdr) = stored
        .message_id_hdr
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        let criteria = imap_header_message_id_search(hdr);
        let uids = state.client_mut().uid_search(&criteria).await?;
        if !uids.is_empty() {
            return Ok(false);
        }
    }
    Ok(true)
}

pub(super) async fn open_imap_draft(
    state: &mut WorkerImapState,
    account: &Account,
    id: MessageId,
    stored: &alcedo_store::StoredMessage,
    folder: &Folder,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
) -> Result<()> {
    if !folder_is_syncable(folder) {
        return Err(Error::Store(
            "draft resume requires a syncable folder".to_string(),
        ));
    }

    let selected_now = select_mailbox_if_needed(state, &folder.imap_name).await?;
    let uidvalidity = selected_now
        .as_ref()
        .and_then(|selected| selected.uidvalidity)
        .or(folder.uidvalidity)
        .unwrap_or(0);
    let mut fetch_uid = stored.uid;
    let fetched = loop {
        match with_fetch_timeout(alcedo_imap::uid_fetch_full_message_with_date(
            state.client_mut(),
            fetch_uid,
        ))
        .await
        {
            Ok(fetched) => break fetched,
            Err(err) => {
                warn!(
                    uid = fetch_uid,
                    folder = folder.id.0,
                    error = %err,
                    "draft UID FETCH failed"
                );
                if let Some(new_uid) =
                    resolve_relocated_draft_uid(state, stored, folder, fetch_uid).await?
                    && new_uid != fetch_uid
                {
                    store.rehome_message(id, folder.id, new_uid, None).await?;
                    fetch_uid = new_uid;
                    continue;
                }
                if draft_uid_confirmed_absent(state, stored, folder, fetch_uid).await? {
                    warn!(
                        uid = fetch_uid,
                        "draft UID confirmed absent after FETCH miss; purging local row"
                    );
                    hide_local_draft_uid(store, folder.id, fetch_uid, events).await?;
                } else {
                    warn!(
                        uid = fetch_uid,
                        "draft FETCH miss without confirmed absence; keeping local row"
                    );
                }
                return Ok(());
            }
        }
    };
    let internaldate = fetched.internaldate;
    let full = fetched.message;

    let _ = save_attachments(stored, &full, store).await?;
    let stored = store
        .get_message(id)
        .await?
        .ok_or_else(|| Error::Store("message not found after draft attachments".to_string()))?;

    let mut draft = alcedo_imap::compose_draft_from_full_message(&full.raw, account.id)?;
    draft.thread_id = Some(stored.thread_id);
    let (attachments, inlines) = compose_parts_from_stored(store, &stored).await;
    draft.attachments = attachments;
    draft.inlines = inlines;

    let message_id_hdr = stored
        .message_id_hdr
        .clone()
        .or_else(|| {
            MessageParser::default()
                .parse(&full.raw)
                .and_then(|msg| msg.message_id().map(str::to_string))
        })
        .unwrap_or_default();
    draft.imap_draft = Some(ImapDraftRef {
        folder_id: folder.id,
        uid: fetch_uid,
        uidvalidity,
        message_id: message_id_hdr,
        last_internaldate: internaldate,
    });

    let text = if !full.text.is_empty() {
        full.text.clone()
    } else if !full.html.is_empty() {
        html_to_snippet(&full.html, 50_000)
    } else {
        String::new()
    };
    let html = if full.html.is_empty() {
        String::new()
    } else {
        full.html.clone()
    };
    store
        .save_message_body(id, Some(&text), Some(&html), None)
        .await?;

    let _ = events.send(SyncEvent::ImapDraftReady {
        message_id: id,
        draft,
    });
    Ok(())
}

pub(super) async fn compose_parts_from_stored(
    store: &StoreHandle,
    stored: &alcedo_store::StoredMessage,
) -> (Vec<ComposeAttachment>, Vec<ComposeInline>) {
    let mut attachments = Vec::new();
    let mut inlines = Vec::new();
    for att in &stored.attachments {
        let Ok(bytes) = store.attachment_bytes(&att.sha256).await else {
            continue;
        };
        if let Some(cid) = att.cid.as_deref() {
            inlines.push(ComposeInline {
                content_id: cid.to_string(),
                filename: att.filename.clone(),
                mime: att.mime.clone(),
                bytes,
            });
        } else if part_is_file_attachment(&att.mime, None) {
            attachments.push(ComposeAttachment {
                filename: att.filename.clone(),
                mime: att.mime.clone(),
                bytes,
            });
        }
    }
    (attachments, inlines)
}

pub(super) fn drafts_folder(folders: &[Folder]) -> Option<&Folder> {
    folders
        .iter()
        .find(|folder| folder.special == SpecialUse::Drafts && folder.selectable)
}

pub(super) fn imap_draft_has_server_uid(imap: &ImapDraftRef) -> bool {
    imap.uid != 0
}

pub(crate) fn draft_uids_to_replace_after_append(found: &[u32], keep: u32) -> Vec<u32> {
    let mut stale: Vec<u32> = found.iter().copied().filter(|uid| *uid != keep).collect();
    stale.sort_unstable();
    stale.dedup();
    stale
}

/// Local rows of the same draft (same Message-ID) other than the one just
/// appended. These are hidden even when the server copy is already gone.
pub(crate) fn local_draft_uids_to_hide(
    live_rows: &[(u32, String)],
    message_id_hdr: &str,
    keep: u32,
) -> Vec<u32> {
    live_rows
        .iter()
        .filter(|(uid, hdr)| *uid != keep && draft_message_id_eq(hdr, message_id_hdr))
        .map(|(uid, _)| *uid)
        .collect()
}

pub(super) fn compose_draft_list_envelope(
    account: &Account,
    folder_id: FolderId,
    draft: &ComposeDraft,
    uid: u32,
    from_email: &str,
    from_display_name: &str,
) -> alcedo_core::Envelope {
    let to_addrs: Vec<Address> = draft
        .to
        .iter()
        .filter_map(|token| parse_address_token(token))
        .collect();
    alcedo_core::Envelope {
        id: MessageId(0),
        account_id: account.id,
        folder_id,
        uid,
        thread_id: draft
            .thread_id
            .unwrap_or_else(|| alcedo_core::ThreadId(uuid::Uuid::new_v4())),
        date: Utc::now(),
        subject: draft.subject.clone(),
        from: vec![Address {
            name: if from_display_name.is_empty() {
                None
            } else {
                Some(from_display_name.to_string())
            },
            email: from_email.to_string(),
        }],
        to: to_addrs,
        cc: vec![],
        reply_to: Vec::new(),
        sender: Vec::new(),
        routing: None,
        snippet: preview_snippet(&draft.body_text),
        seen: true,
        flagged: false,
        answered: false,
        draft: true,
        has_attachments: !draft.attachments.is_empty(),
        deleted_at: None,
        account_accent: account.accent_color.clone(),
        account_avatar: account.avatar_text.clone(),
        size_bytes: 0,
    }
}

pub(super) async fn persist_appended_imap_draft(
    account: &Account,
    folder: &Folder,
    draft: &ComposeDraft,
    uid: u32,
    message_id_hdr: &str,
    store: &StoreHandle,
) -> Result<()> {
    let (from_email, from_display_name) = resolve_compose_from(account, draft, store).await;
    let envelope = compose_draft_list_envelope(
        account,
        folder.id,
        draft,
        uid,
        &from_email,
        &from_display_name,
    );
    store
        .upsert_envelopes(&[envelope], Some(&draft.body_text))
        .await?;
    if !message_id_hdr.trim().is_empty() {
        store
            .set_message_id_hdrs(folder.id, &[(uid, message_id_hdr.to_string())])
            .await?;
    }
    Ok(())
}

pub(super) fn session_draft_uid(
    state: &WorkerImapState,
    folder_id: FolderId,
    message_id: &str,
) -> Option<u32> {
    let recent = state.recent_draft_append.as_ref()?;
    if recent.folder_id != folder_id || recent.at.elapsed() > COMPOSE_SESSION_WINDOW {
        return None;
    }
    draft_message_id_eq(&recent.message_id, message_id).then_some(recent.uid)
}

/// UID to replace on the next APPEND, and whether that previous copy is gone
/// for a reason the user should hear about.
///
/// `session_uid` is a copy this worker appended moments ago. The command may
/// still name the UID from before that APPEND. UIDVALIDITY `0` means the
/// previous save never learned a generation, so a later SELECT value is not a
/// mailbox reset.
pub(crate) fn prior_draft_uid(
    old: Option<&ImapDraftRef>,
    selected_uidvalidity: Option<u32>,
    session_uid: Option<u32>,
) -> (Option<u32>, bool) {
    if let Some(uid) = session_uid {
        return (Some(uid), false);
    }
    let Some(old) = old.filter(|imap| imap_draft_has_server_uid(imap)) else {
        return (None, false);
    };
    let same_generation = selected_uidvalidity.is_none()
        || old.uidvalidity == 0
        || selected_uidvalidity == Some(old.uidvalidity);
    if same_generation {
        (Some(old.uid), false)
    } else {
        (None, true)
    }
}

/// The prior UID is gone, but this worker caused it: a stale command naming a
/// UID this session already replaced, or an earlier APPEND whose UID we never learned.
pub(crate) fn prior_uid_missing_is_expected(
    session_uid: Option<u32>,
    command_uid: Option<u32>,
    uid: u32,
    has_unresolved: bool,
) -> bool {
    (session_uid == Some(uid) && command_uid != Some(uid)) || has_unresolved
}

/// Activity-log detail for "Original draft was removed": what the composer
/// believed, what this worker remembered, and what the server answered.
pub(crate) fn describe_missing_prior_draft(
    composer_ref: Option<&ImapDraftRef>,
    server_uidvalidity: Option<u32>,
    session_uid: Option<u32>,
    command_uid: Option<u32>,
    checked_uid: Option<u32>,
    checked_uid_present: Option<bool>,
    new_uid: u32,
) -> String {
    let uid = |value: Option<u32>| value.map_or("none".to_string(), |uid| uid.to_string());
    let composer = composer_ref.map_or("none".to_string(), |imap| {
        format!("UID {} (UIDVALIDITY {})", imap.uid, imap.uidvalidity)
    });
    let present = match checked_uid_present {
        Some(true) => "present",
        Some(false) => "absent",
        None => "not checked",
    };
    format!(
        "composer copy: {composer}\nserver UIDVALIDITY: {}\nthis session's last copy: UID {}\nUID named by composer: {}\nUID checked on server: {} ({present})\nnew UID: {new_uid}",
        uid(server_uidvalidity),
        uid(session_uid),
        uid(command_uid),
        uid(checked_uid),
    )
}

/// Pick the UID of a draft we just appended when SEARCH could not name it.
pub(crate) fn pick_fetched_draft_uid(
    fetched: &[(u32, String)],
    wanted_message_id: &str,
    uidnext: Option<u32>,
) -> Option<u32> {
    let at_or_above = |uid: u32| uidnext.is_none_or(|next| uid >= next);
    let wanted = normalize_draft_message_id(wanted_message_id);
    if !wanted.is_empty() {
        let matched = fetched
            .iter()
            .filter(|(uid, id)| at_or_above(*uid) && draft_message_id_eq(id, &wanted))
            .map(|(uid, _)| *uid);
        if let Some(uid) = matched.max() {
            return Some(uid);
        }
    }
    let mut in_range: Vec<u32> = fetched
        .iter()
        .map(|(uid, _)| *uid)
        .filter(|uid| at_or_above(*uid))
        .collect();
    if in_range.len() == 1 {
        return in_range.pop();
    }
    None
}

pub(super) async fn resolve_appended_draft_uid(
    state: &mut WorkerImapState,
    folder_id: FolderId,
    message_id_hdr: &str,
    dest_uidnext: Option<u32>,
) -> Result<Option<u32>> {
    if let Some(uid) = resolve_moved_uid(state, Some(message_id_hdr), dest_uidnext).await? {
        return Ok(Some(uid));
    }
    // HEADER search is often empty until the server indexes the append.
    let _ = state.client_mut().noop().await;
    if let Some(uid) = resolve_moved_uid(state, Some(message_id_hdr), dest_uidnext).await? {
        return Ok(Some(uid));
    }
    let Some(from_uid) = dest_uidnext else {
        return Ok(None);
    };
    let fetched = alcedo_imap::uid_fetch_envelopes(
        state.client_mut(),
        folder_id,
        &format!("{from_uid}:*"),
        None,
    )
    .await?;
    let rows: Vec<(u32, String)> = fetched
        .into_iter()
        .map(|env| (env.uid, env.message_id.unwrap_or_default()))
        .collect();
    Ok(pick_fetched_draft_uid(&rows, message_id_hdr, dest_uidnext))
}

/// UID of a newer server copy that should be kept instead of appending again.
///
/// `peers` are other UIDs of this Message-ID. `subject_matches` are later UIDs
/// with the same subject, used when our UID is already gone and the other
/// client saved under a new Message-ID.
pub(crate) fn replacement_uid_for_draft(
    last_saved: Option<DateTime<Utc>>,
    peers: &[(u32, Option<DateTime<Utc>>)],
    our_uid_missing: bool,
    subject_matches: &[u32],
) -> Option<u32> {
    if let Some(saved) = last_saved
        && let Some(uid) = peers
            .iter()
            .filter_map(|(uid, date)| date.filter(|date| *date > saved).map(|date| (*uid, date)))
            .max_by_key(|(_, date)| *date)
            .map(|(uid, _)| uid)
    {
        return Some(uid);
    }
    if our_uid_missing {
        subject_matches.iter().copied().max()
    } else {
        None
    }
}

/// Point the composer at another client's newer Drafts copy instead of
/// appending a second message.
pub(super) async fn adopt_newer_draft(
    state: &mut WorkerImapState,
    folder: &Folder,
    draft: &ComposeDraft,
    old: &ImapDraftRef,
    our_uid: Option<u32>,
    our_uid_missing: bool,
    uidvalidity: u32,
) -> Result<Option<ImapDraftRef>> {
    let hdr = old.message_id.trim();
    let mut peers = Vec::new();
    if !hdr.is_empty() {
        let criteria = imap_header_message_id_search(hdr);
        match state.client_mut().uid_search(&criteria).await {
            Ok(found) => {
                for uid in found {
                    if Some(uid) == our_uid {
                        continue;
                    }
                    let date = state.client_mut().uid_fetch_internaldate(uid).await?;
                    peers.push((uid, date));
                }
            }
            Err(err) => {
                warn!(error = %err, "draft Message-ID SEARCH failed before save");
            }
        }
    }

    let mut subject_matches = Vec::new();
    if our_uid_missing && let Some(uid) = our_uid {
        let from = uid.saturating_add(1);
        let wanted = draft.subject.trim();
        if !wanted.is_empty() {
            let fetched =
                uid_fetch_envelopes(state.client_mut(), folder.id, &format!("{from}:*"), None)
                    .await?;
            subject_matches = fetched
                .into_iter()
                .filter(|env| !env.imap_deleted && env.subject.trim() == wanted)
                .map(|env| env.uid)
                .collect();
        }
    }

    let Some(uid) = replacement_uid_for_draft(
        old.last_internaldate,
        &peers,
        our_uid_missing,
        &subject_matches,
    ) else {
        return Ok(None);
    };

    let message_id = if peers.iter().any(|(peer, _)| *peer == uid) {
        old.message_id.clone()
    } else {
        let fetched =
            uid_fetch_envelopes(state.client_mut(), folder.id, &uid.to_string(), None).await?;
        fetched
            .into_iter()
            .find(|env| env.uid == uid)
            .and_then(|env| env.message_id)
            .filter(|id| !id.trim().is_empty())
            .unwrap_or_else(|| old.message_id.clone())
    };
    let last_internaldate = peers
        .iter()
        .find(|(peer, _)| *peer == uid)
        .and_then(|(_, date)| *date)
        .or(state.client_mut().uid_fetch_internaldate(uid).await?);

    Ok(Some(ImapDraftRef {
        folder_id: folder.id,
        uid,
        uidvalidity,
        message_id,
        last_internaldate,
    }))
}

pub(super) async fn save_imap_draft(
    state: &mut WorkerImapState,
    account: &Account,
    mut draft: ComposeDraft,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
    server_rules: &ServerRulesSession,
) -> Result<()> {
    // An autosave flushed just before Send can reach us after the send finished.
    if let Some(imap) = draft.imap_draft.as_ref()
        && state.compose_session_sent(&imap.message_id)
    {
        debug!("skipping Drafts save for a compose session that already sent");
        return Ok(());
    }

    let folders = store.list_folders(account.id).await?;
    let Some(drafts) = drafts_folder(&folders) else {
        let _ = events.send(SyncEvent::ImapDraftSaveFailed {
            message: tr!("This account has no Drafts folder"),
        });
        return Ok(());
    };

    if let Err(err) = validate_compose_draft(&mut draft) {
        let _ = events.send(SyncEvent::ImapDraftSaveFailed {
            message: err.to_string(),
        });
        return Ok(());
    }

    ensure_selected(state, &drafts.imap_name).await?;
    let selected = state.client_mut().select_mailbox(&drafts.imap_name).await?;

    let mut replaced_newer = false;
    let old_ref = draft.imap_draft.clone();
    let session_uid = old_ref
        .as_ref()
        .and_then(|old| session_draft_uid(state, drafts.id, &old.message_id));
    let (old_uid, mut removed_missing) =
        prior_draft_uid(old_ref.as_ref(), selected.uidvalidity, session_uid);
    let command_uid = prior_draft_uid(old_ref.as_ref(), selected.uidvalidity, None).0;

    let mut our_uid_missing = false;
    let mut old_uid_present = None;
    if let Some(uid) = old_uid {
        let present = state.client_mut().uid_is_present(uid).await?;
        old_uid_present = Some(present);
        if present {
            let named_by_command =
                session_uid.is_none() || old_ref.as_ref().is_some_and(|old| old.uid == uid);
            if named_by_command
                && let Some(server_date) = state.client_mut().uid_fetch_internaldate(uid).await?
                && let Some(local_date) = old_ref.as_ref().and_then(|old| old.last_internaldate)
                && server_date > local_date
            {
                replaced_newer = true;
            }
        } else {
            let has_unresolved = old_ref.as_ref().is_some_and(|old| {
                state
                    .unresolved_draft_append_for(drafts.id, &old.message_id)
                    .is_some()
            });
            if !prior_uid_missing_is_expected(session_uid, command_uid, uid, has_unresolved) {
                removed_missing = true;
                our_uid_missing = true;
            }
        }
    }

    if let Some(old) = old_ref.as_ref()
        && let Some(adopted) = adopt_newer_draft(
            state,
            drafts,
            &draft,
            old,
            old_uid,
            our_uid_missing,
            selected.uidvalidity.unwrap_or(0),
        )
        .await?
    {
        draft.imap_draft = Some(adopted);
        let _ = events.send(SyncEvent::ImapDraftSaved {
            account: account.id,
            draft,
            replaced_newer: false,
            removed_missing: false,
            kept_newer: true,
            save_detail: String::new(),
        });
        sync_selected_mailbox(state, account, drafts, store, events, server_rules, false).await?;
        return Ok(());
    }

    let mut mime_draft = draft.clone();
    mime_draft.request_mdn = false;
    mime_draft.tracking_pixel_url = None;
    let outbound = render_outbound(mime_draft.format, &mime_draft.body_text, None);
    let bodies = RenderedBodies {
        plain: outbound.plain,
        html: outbound.html,
    };
    let (from_email, from_display_name) = resolve_compose_from(account, &mime_draft, store).await;
    let identities = store.list_identities(account.id).await.unwrap_or_default();
    let envelope_email =
        envelope_email_for_compose(account, &mime_draft, &from_email, &identities, store).await?;
    let opts = SmtpOptions {
        host: account.smtp_host.clone(),
        port: account.smtp_port,
        starttls: account.smtp_starttls,
        email: account.email.clone(),
        username: account.smtp_login_name().to_string(),
        display_name: account.display_name.clone(),
        from_email: Some(from_email),
        from_display_name: Some(from_display_name),
        envelope_email: Some(envelope_email),
        auth: SmtpAuth::Password(String::new()),
    };
    let stable_id = old_ref
        .as_ref()
        .map(|old| old.message_id.as_str())
        .filter(|id| !id.trim().is_empty());
    let (message, message_id_hdr) = build_draft_message(&opts, &mime_draft, &bodies, stable_id)?;
    let rfc822 = message_to_rfc822(&message);

    // Bounded like the Sent copy's APPEND: a server that stalls mid-upload must
    // not hold the command connection indefinitely.
    let appended = with_imap_timeout(
        super::sent_copy::append_timeout_for_len(rfc822.len()),
        state.client_mut().append_rfc822(&drafts.imap_name, &rfc822),
    )
    .await?;

    let new_uid = if let Some(uid) = appended.uid {
        debug!(uid, "draft APPENDUID");
        uid
    } else {
        state.note_unresolved_draft_append(
            drafts.id,
            &message_id_hdr,
            selected.uidnext.unwrap_or(1),
        );
        match resolve_appended_draft_uid(state, drafts.id, &message_id_hdr, selected.uidnext)
            .await?
        {
            Some(uid) => uid,
            None => {
                // The message is on the server, we just cannot name it yet. The unresolved record above lets a later delete (post-send or discard) still find it.
                warn!(
                    folder = %drafts.imap_name,
                    "APPEND succeeded but draft UID not found"
                );
                return Err(Error::Imap(
                    "APPEND succeeded but draft UID not found".to_string(),
                ));
            }
        }
    };

    let mut stale = {
        let hdr = message_id_hdr.trim();
        if hdr.is_empty() {
            Vec::new()
        } else {
            let criteria = imap_header_message_id_search(hdr);
            match state.client_mut().uid_search(&criteria).await {
                Ok(found) => draft_uids_to_replace_after_append(&found, new_uid),
                Err(err) => {
                    warn!(error = %err, "draft Message-ID SEARCH failed after APPEND");
                    Vec::new()
                }
            }
        }
    };
    if let Some(uid) = old_uid
        && uid != new_uid
        && !stale.contains(&uid)
        && state.client_mut().uid_is_present(uid).await?
    {
        stale.push(uid);
    }
    if let Some(uid) = command_uid
        && uid != new_uid
        && !stale.contains(&uid)
        && state.client_mut().uid_is_present(uid).await?
    {
        stale.push(uid);
    }
    if !message_id_hdr.trim().is_empty() {
        match recover_appended_draft_uids(state, drafts, &message_id_hdr).await {
            Ok(found) => {
                for uid in found {
                    if uid != new_uid && !stale.contains(&uid) {
                        stale.push(uid);
                    }
                }
            }
            Err(err) => {
                warn!(error = %err, "failed to recover unresolved draft copies");
            }
        }
    }
    stale.sort_unstable();
    stale.dedup();

    persist_appended_imap_draft(account, drafts, &draft, new_uid, &message_id_hdr, store).await?;
    state.recent_draft_append = Some(RecentDraftAppend {
        folder_id: drafts.id,
        uid: new_uid,
        message_id: message_id_hdr.clone(),
        at: Instant::now(),
    });
    // Hide every older local row of this draft now, including copies the server
    // no longer has. Otherwise the list shows old and new side by side until the
    // follow-up sync removes the old row.
    let mut hide_locally = stale.clone();
    match store.live_message_id_hdrs(drafts.id).await {
        Ok(rows) => hide_locally.extend(local_draft_uids_to_hide(&rows, &message_id_hdr, new_uid)),
        Err(err) => warn!(error = %err, "could not list local draft rows to hide"),
    }
    hide_locally.sort_unstable();
    hide_locally.dedup();
    if !hide_locally.is_empty() {
        store.mark_deleted_by_uids(drafts.id, hide_locally).await?;
    }

    let last_internaldate = state.client_mut().uid_fetch_internaldate(new_uid).await?;

    draft.imap_draft = Some(ImapDraftRef {
        folder_id: drafts.id,
        uid: new_uid,
        uidvalidity: appended.uidvalidity.or(selected.uidvalidity).unwrap_or(0),
        message_id: message_id_hdr.clone(),
        last_internaldate,
    });

    let save_detail = if removed_missing {
        describe_missing_prior_draft(
            old_ref.as_ref(),
            selected.uidvalidity,
            session_uid,
            command_uid,
            old_uid,
            old_uid_present,
            new_uid,
        )
    } else {
        String::new()
    };
    let _ = events.send(SyncEvent::ImapDraftSaved {
        account: account.id,
        draft: draft.clone(),
        replaced_newer,
        removed_missing,
        kept_newer: false,
        save_detail,
    });

    if !stale.is_empty() {
        for chunk_set in format_uid_set_chunks(&stale, 250) {
            state.client_mut().uid_delete_set(&chunk_set).await?;
        }
    }

    emit_folder_envelopes(store, drafts.id, events).await?;
    sync_selected_mailbox(state, account, drafts, store, events, server_rules, false).await?;

    Ok(())
}

pub(super) async fn delete_imap_draft(
    state: &mut WorkerImapState,
    account: &Account,
    folder_id: FolderId,
    uid: u32,
    message_id: &str,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
    server_rules: &ServerRulesSession,
) -> Result<()> {
    let folder = store
        .get_folder(folder_id)
        .await?
        .ok_or_else(|| Error::Store("draft folder not found".to_string()))?;
    if folder.account_id != account.id || !folder_is_syncable(&folder) {
        return Ok(());
    }
    ensure_selected(state, &folder.imap_name).await?;

    // Drafts are saved by APPEND+DELETE, so stale UI UIDs can race with autosave.
    // When that happens, the same logical draft (same Message-ID) may temporarily
    // exist at multiple UIDs. Delete *all* matching UIDs to avoid orphans that
    // reappear on the next sync.
    let hdr = message_id.trim();
    let mut targets = if hdr.is_empty() {
        Vec::new()
    } else {
        let criteria = imap_header_message_id_search(hdr);
        with_imap_timeout(
            IMAP_COMMAND_TIMEOUT,
            state.client_mut().uid_search(&criteria),
        )
        .await?
    };
    // An earlier APPEND for this session may have landed without ever yielding a
    // UID, or after the caller captured its UID; SEARCH may miss both. Fold in those copies.
    targets.extend(recover_appended_draft_uids(state, &folder, hdr).await?);
    targets.extend(session_draft_uid(state, folder.id, hdr));
    targets.sort_unstable();
    targets.dedup();
    if !targets.is_empty() {
        for chunk_set in format_uid_set_chunks(&targets, 250) {
            with_imap_timeout(
                IMAP_COMMAND_TIMEOUT,
                state.client_mut().uid_delete_set(&chunk_set),
            )
            .await?;
        }
        sync_selected_mailbox(state, account, &folder, store, events, server_rules, false).await?;
        return Ok(());
    }

    // Fallback: either Message-ID is missing, or this draft is already gone from the server.
    if with_imap_timeout(IMAP_COMMAND_TIMEOUT, state.client_mut().uid_is_present(uid)).await? {
        with_imap_timeout(IMAP_COMMAND_TIMEOUT, state.client_mut().uid_delete(uid)).await?;
        sync_selected_mailbox(state, account, &folder, store, events, server_rules, false).await?;
    } else {
        hide_local_draft_uid(store, folder.id, uid, events).await?;
    }

    Ok(())
}

/// Find a draft whose APPEND never produced a UID by re-fetching the UID range
/// recorded at APPEND time and matching on Message-ID.
pub(super) async fn recover_appended_draft_uids(
    state: &mut WorkerImapState,
    folder: &Folder,
    message_id: &str,
) -> Result<Vec<u32>> {
    let Some((wanted, from_uid)) = state
        .unresolved_draft_append_for(folder.id, message_id)
        .map(|record| {
            (
                normalize_draft_message_id(&record.message_id),
                record.from_uid,
            )
        })
    else {
        return Ok(Vec::new());
    };

    let uid_set = format!("{from_uid}:*");
    let fetched = with_imap_timeout(
        IMAP_COMMAND_TIMEOUT,
        alcedo_imap::uid_fetch_envelopes(state.client_mut(), folder.id, &uid_set, None),
    )
    .await?;
    let mut uids: Vec<u32> = fetched
        .iter()
        .filter(|env| {
            env.message_id
                .as_deref()
                .is_some_and(|id| normalize_draft_message_id(id) == wanted)
        })
        .map(|env| env.uid)
        .collect();
    uids.sort_unstable();
    uids.dedup();
    if !uids.is_empty() {
        state.unresolved_draft_append = None;
    }
    Ok(uids)
}

pub(super) async fn hide_local_draft_uid(
    store: &StoreHandle,
    folder_id: FolderId,
    uid: u32,
    events: &flume::Sender<SyncEvent>,
) -> Result<()> {
    let rows = store.messages_by_uids(folder_id, &[uid]).await?;
    if rows.is_empty() {
        return Ok(());
    }
    store.mark_deleted_by_uids(folder_id, vec![uid]).await?;
    for (id, _, _) in rows {
        let _ = events.send(SyncEvent::EnvelopeRemoved { id });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worker::test_support::sample_imap_draft;
    use alcedo_core::{AccountId, FolderId, SpecialUse};
    use uuid::Uuid;

    #[test]
    fn replacement_uid_keeps_a_newer_copy() {
        let saved = DateTime::parse_from_rfc3339("2026-09-29T20:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let newer = saved + chrono::Duration::seconds(30);
        let older = saved - chrono::Duration::seconds(30);
        assert_eq!(
            replacement_uid_for_draft(
                Some(saved),
                &[(4, Some(older)), (9, Some(newer))],
                false,
                &[]
            ),
            Some(9)
        );
        assert_eq!(
            replacement_uid_for_draft(Some(saved), &[(4, Some(older))], false, &[11, 12]),
            None
        );
        assert_eq!(
            replacement_uid_for_draft(Some(saved), &[], true, &[11, 12]),
            Some(12)
        );
        assert_eq!(
            replacement_uid_for_draft(Some(saved), &[], false, &[12]),
            None
        );
    }

    #[test]
    fn draft_message_id_eq_normalizes_brackets() {
        assert!(draft_message_id_eq(
            "<draft@example.com>",
            "draft@example.com"
        ));
        assert!(draft_message_id_eq(
            " <Draft@Example.com> ",
            "<draft@example.com>"
        ));
        assert!(!draft_message_id_eq(
            "<draft@example.com>",
            "<other@example.com>"
        ));
    }

    #[test]
    fn recent_append_matches_uid_or_message_id() {
        let folder_id = FolderId(9);
        let folder = Folder {
            id: folder_id,
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
        let recent = RecentDraftAppend {
            folder_id,
            uid: 112,
            message_id: "<session@example.com>".to_string(),
            at: Instant::now(),
        };
        assert!(recent_append_matches(
            &recent,
            &folder,
            Some("<session@example.com>"),
            106,
        ));
        assert!(recent_append_matches(&recent, &folder, None, 112));
        assert!(!recent_append_matches(
            &recent,
            &folder,
            Some("<other@example.com>"),
            106,
        ));
    }

    #[test]
    fn prior_draft_uid_ignores_unknown_uidvalidity() {
        let old = sample_imap_draft(10, 0);
        assert_eq!(
            prior_draft_uid(Some(&old), Some(42), None),
            (Some(10), false)
        );
        assert_eq!(prior_draft_uid(Some(&old), None, None), (Some(10), false));
        assert_eq!(
            prior_draft_uid(Some(&old), Some(0), None),
            (Some(10), false)
        );
    }

    #[test]
    fn prior_draft_uid_reports_generation_change_only() {
        let old = sample_imap_draft(10, 1);
        assert_eq!(
            prior_draft_uid(Some(&old), Some(1), None),
            (Some(10), false)
        );
        assert_eq!(prior_draft_uid(Some(&old), Some(2), None), (None, true));
        assert_eq!(prior_draft_uid(None, Some(1), None), (None, false));
        let placeholder = sample_imap_draft(0, 0);
        assert_eq!(
            prior_draft_uid(Some(&placeholder), Some(1), None),
            (None, false)
        );
    }

    #[test]
    fn prior_draft_uid_prefers_this_sessions_append() {
        let old = sample_imap_draft(10, 1);
        assert_eq!(
            prior_draft_uid(Some(&old), Some(1), Some(11)),
            (Some(11), false)
        );
        assert_eq!(
            prior_draft_uid(Some(&old), Some(9), Some(11)),
            (Some(11), false)
        );
    }

    #[test]
    fn prior_uid_missing_is_expected_only_for_stale_commands() {
        assert!(prior_uid_missing_is_expected(Some(11), Some(10), 11, false));
        assert!(!prior_uid_missing_is_expected(
            Some(11),
            Some(11),
            11,
            false
        ));
        assert!(!prior_uid_missing_is_expected(None, Some(10), 10, false));
        assert!(prior_uid_missing_is_expected(None, Some(10), 10, true));
        assert!(prior_uid_missing_is_expected(Some(11), None, 11, false));
    }

    #[test]
    fn pick_fetched_draft_uid_matches_message_id_then_single_new_uid() {
        let rows = vec![
            (8, "<session@example.com>".to_string()),
            (12, "session@example.com".to_string()),
            (15, "<other@example.com>".to_string()),
        ];
        assert_eq!(
            pick_fetched_draft_uid(&rows, "<session@example.com>", Some(10)),
            Some(12)
        );
        let only_new = vec![(12, String::new())];
        assert_eq!(
            pick_fetched_draft_uid(&only_new, "<session@example.com>", Some(10)),
            Some(12)
        );
        assert_eq!(
            pick_fetched_draft_uid(&rows, "<missing@example.com>", Some(10)),
            None
        );
    }

    #[test]
    fn pick_fetched_draft_uid_ignores_old_copy_below_uidnext() {
        let rows = vec![(8, "<session@example.com>".to_string())];
        assert_eq!(
            pick_fetched_draft_uid(&rows, "<session@example.com>", Some(10)),
            None
        );
        assert_eq!(
            pick_fetched_draft_uid(&rows, "<session@example.com>", None),
            Some(8)
        );
        let other = vec![(9, "<other@example.com>".to_string())];
        assert_eq!(
            pick_fetched_draft_uid(&other, "<other@example.com>", Some(10)),
            None
        );
    }

    #[test]
    fn local_draft_rows_with_same_message_id_are_hidden_except_the_new_one() {
        let rows = vec![
            (604, "session@example.com".to_string()),
            (605, "<session@example.com>".to_string()),
            (700, "other@example.com".to_string()),
            (701, String::new()),
        ];
        assert_eq!(
            local_draft_uids_to_hide(&rows, "<session@example.com>", 605),
            vec![604]
        );
        assert!(local_draft_uids_to_hide(&rows, "", 605).is_empty());
    }

    #[test]
    fn draft_append_deletes_every_other_uid_with_same_message_id() {
        assert_eq!(
            draft_uids_to_replace_after_append(&[8, 9, 12], 12),
            vec![8, 9]
        );
        assert!(draft_uids_to_replace_after_append(&[12], 12).is_empty());
        assert_eq!(
            draft_uids_to_replace_after_append(&[8, 8, 9], 12),
            vec![8, 9]
        );
    }
}
