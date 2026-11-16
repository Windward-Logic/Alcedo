//! Delete, permanent delete, expunge, reconcile and undelete.
#![allow(clippy::too_many_arguments)]

use super::flags::locators_by_folder;
use super::folders::{imap_error_text, replace_folder_envelopes_after_delete};
use super::mailbox_sync::{emit_folder_envelopes, ensure_selected, persist_folder_unread};
use super::moves::{
    refresh_trash_after_move, rehome_moved_uids, resolve_moved_uid, snapshot_mailbox_uidnext,
    uid_move_from_selected, uids_at_or_above,
};
use super::session::is_recoverable_disconnect;
use super::*;

#[derive(Debug, PartialEq, Eq)]
pub(super) enum DeletePlan {
    MoveToTrash {
        dest_imap_name: String,
        trash_id: FolderId,
    },
    RemoveFromMailbox,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum PermanentPlan {
    ExpungeHere,
    PurgeViaTrash {
        trash_imap_name: String,
        trash_id: FolderId,
    },
}

pub(super) fn plan_permanent_delete(
    account: &Account,
    folders: &[Folder],
    source: &Folder,
) -> PermanentPlan {
    let gmail = account.imap_host.eq_ignore_ascii_case("imap.gmail.com");
    let trash = folders
        .iter()
        .find(|f| f.selectable && matches!(f.special, SpecialUse::Trash));
    if gmail
        && !matches!(source.special, SpecialUse::Trash | SpecialUse::Junk)
        && let Some(trash) = trash
    {
        PermanentPlan::PurgeViaTrash {
            trash_imap_name: trash.imap_name.clone(),
            trash_id: trash.id,
        }
    } else {
        PermanentPlan::ExpungeHere
    }
}

pub(super) fn plan_message_delete(
    folders: &[Folder],
    source: &Folder,
    has_other_copy: bool,
) -> DeletePlan {
    if has_other_copy || matches!(source.special, SpecialUse::Trash) {
        return DeletePlan::RemoveFromMailbox;
    }
    if matches!(source.special, SpecialUse::Drafts) {
        // Drafts should be expunged instead of moved to Trash, otherwise some
        // servers/UID reconciliation paths can leave copies behind that later
        // reappear in the Drafts list.
        return DeletePlan::RemoveFromMailbox;
    }
    folders
        .iter()
        .find(|f| f.selectable && matches!(f.special, SpecialUse::Trash))
        .map(|f| DeletePlan::MoveToTrash {
            dest_imap_name: f.imap_name.clone(),
            trash_id: f.id,
        })
        .unwrap_or(DeletePlan::RemoveFromMailbox)
}

pub(super) fn assign_remaining_trash_dest_uids(
    map: &mut HashMap<MessageId, u32>,
    mut pending: Vec<(MessageId, u32)>,
    mut available: Vec<u32>,
) {
    pending.sort_by_key(|(_, src_uid)| *src_uid);
    available.sort_unstable();
    for ((id, _), dest_uid) in pending.iter().zip(available.iter()) {
        map.insert(*id, *dest_uid);
    }
}

pub(super) async fn resolve_dest_uids_for_trash_chunk(
    state: &mut WorkerImapState,
    records: &[(MessageId, u32, Option<String>)],
    trash_uidnext_before: Option<u32>,
) -> Result<HashMap<MessageId, u32>> {
    let mut map = HashMap::new();
    let mut pending = Vec::new();
    for (id, uid, hdr) in records {
        match resolve_moved_uid(state, hdr.as_deref(), trash_uidnext_before).await? {
            Some(dest_uid) if !map.values().any(|mapped| *mapped == dest_uid) => {
                map.insert(*id, dest_uid);
            }
            _ => pending.push((*id, *uid)),
        }
    }
    if pending.is_empty() {
        return Ok(map);
    }
    if let Some(uidnext) = trash_uidnext_before {
        let new_uids = state
            .client_mut()
            .uid_search(&format!("UID {uidnext}:*"))
            .await?;
        let available: Vec<u32> = uids_at_or_above(&new_uids, uidnext)
            .into_iter()
            .filter(|uid| !map.values().any(|mapped| mapped == uid))
            .collect();
        assign_remaining_trash_dest_uids(&mut map, pending, available);
    }
    Ok(map)
}

pub(super) async fn finish_trash_move_chunk(
    state: &mut WorkerImapState,
    store: &StoreHandle,
    chunk_set: &str,
    chunk_uids: &[u32],
    source_folder_id: FolderId,
    trash: &Folder,
    trash_uidnext_before: Option<u32>,
) -> Result<(usize, HashSet<u32>)> {
    let still_present = state
        .client_mut()
        .uid_search(&format!("UID {chunk_set}"))
        .await?;
    let still_present_set: HashSet<u32> = still_present.into_iter().collect();
    let chunk_uids_set: HashSet<u32> = chunk_uids.iter().copied().collect();
    let gone: Vec<u32> = chunk_uids_set
        .difference(&still_present_set)
        .copied()
        .collect();
    if gone.is_empty() {
        return Ok((0, still_present_set));
    }

    let (rehomed, tombstone_ids) = rehome_moved_uids(
        state,
        store,
        source_folder_id,
        trash,
        trash_uidnext_before,
        &gone,
    )
    .await?;
    Ok((rehomed + tombstone_ids.len(), still_present_set))
}

pub(super) async fn delete_message(
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
        .iter()
        .find(|f| f.id == stored.folder_id)
        .ok_or_else(|| Error::Store("folder not found for message".to_string()))?;

    let source_name = folder.imap_name.clone();
    let has_other_copy = store.has_live_copy_elsewhere(id).await?;

    ensure_selected(state, &source_name).await?;
    let mut delete_plan = plan_message_delete(&folders, folder, has_other_copy);
    if matches!(folder.special, SpecialUse::Drafts) {
        // Avoid MOVE-to-trash: it only targets a single UID, and stale UI UIDs
        // can otherwise leave the newer APPEND copy behind.
        delete_plan = DeletePlan::RemoveFromMailbox;
    }

    match delete_plan {
        DeletePlan::MoveToTrash {
            dest_imap_name,
            trash_id,
        } => {
            let trash_uidnext = snapshot_mailbox_uidnext(
                state,
                &dest_imap_name,
                folders
                    .iter()
                    .find(|f| f.id == trash_id)
                    .and_then(|f| f.uidnext),
            )
            .await?;
            if let Err(err) = uid_move_from_selected(
                state,
                &source_name,
                stored.uid,
                &dest_imap_name,
                account,
                None,
            )
            .await
            {
                if let Err(emit_err) = emit_folder_envelopes(store, stored.folder_id, events).await
                {
                    warn!(error = %emit_err, "failed to restore folder list after delete");
                }
                let _ = events.send(SyncEvent::MessageDeleteFailed {
                    ids: vec![id],
                    reason: err.to_string(),
                });
                return Err(err);
            }
            ensure_selected(state, &dest_imap_name).await?;
            let dest_uid =
                resolve_moved_uid(state, stored.message_id_hdr.as_deref(), trash_uidnext).await?;
            if let Some(dest_uid) = dest_uid {
                let rehomed = store
                    .rehome_message(id, trash_id, dest_uid, Some(Utc::now()))
                    .await?;
                if !rehomed {
                    store.mark_deleted(id).await?;
                }
            } else {
                store.mark_deleted(id).await?;
            }
        }
        DeletePlan::RemoveFromMailbox => {
            if matches!(folder.special, SpecialUse::Drafts) {
                // Try bulk delete by stable Message-ID first.
                let hdr = stored.message_id_hdr.as_deref().unwrap_or_default().trim();
                if !hdr.is_empty() {
                    let criteria = imap_header_message_id_search(hdr);
                    let uids = state.client_mut().uid_search(&criteria).await?;
                    if !uids.is_empty() {
                        for chunk_set in format_uid_set_chunks(&uids, 250) {
                            state.client_mut().uid_delete_set(&chunk_set).await?;
                        }

                        let rows = store.messages_by_uids(folder.id, &uids).await?;
                        store.mark_deleted_by_uids(folder.id, uids.clone()).await?;
                        for (id, _, _) in rows {
                            let _ = events.send(SyncEvent::EnvelopeRemoved { id });
                        }

                        persist_folder_unread(account, stored.folder_id, store, events).await?;
                        emit_unread_counts(store, events).await?;
                        return Ok(());
                    }
                }

                // Fallback for missing Message-ID: if the UID is already gone
                // from the server, just hide the local row so it can't reappear.
                if state.client_mut().uid_is_present(stored.uid).await?
                    && let Err(err) = state.client_mut().uid_delete(stored.uid).await
                {
                    if let Err(emit_err) =
                        emit_folder_envelopes(store, stored.folder_id, events).await
                    {
                        warn!(
                            error = %emit_err,
                            "failed to restore folder list after delete"
                        );
                    }
                    let _ = events.send(SyncEvent::MessageDeleteFailed {
                        ids: vec![id],
                        reason: err.to_string(),
                    });
                    return Err(err);
                }
                store.mark_deleted(id).await?;
            } else {
                if let Err(err) = state.client_mut().uid_delete(stored.uid).await {
                    if let Err(emit_err) =
                        emit_folder_envelopes(store, stored.folder_id, events).await
                    {
                        warn!(error = %emit_err, "failed to restore folder list after delete");
                    }
                    let _ = events.send(SyncEvent::MessageDeleteFailed {
                        ids: vec![id],
                        reason: err.to_string(),
                    });
                    return Err(err);
                }
                store.mark_deleted(id).await?;
            }
        }
    }

    let _ = events.send(SyncEvent::EnvelopeRemoved { id });
    persist_folder_unread(account, stored.folder_id, store, events).await?;
    emit_unread_counts(store, events).await?;
    Ok(())
}

pub(super) async fn delete_draft_messages_batch(
    state: &mut WorkerImapState,
    account: &Account,
    folder: &Folder,
    messages: Vec<MessageLocator>,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
) -> Result<()> {
    ensure_selected(state, &folder.imap_name).await?;
    let mut all_uids: Vec<u32> = Vec::new();
    let mut headerless: Vec<MessageLocator> = Vec::new();

    for locator in &messages {
        let hdr = locator.message_id_hdr.as_deref().unwrap_or_default().trim();
        if !hdr.is_empty() {
            let criteria = imap_header_message_id_search(hdr);
            match state.client_mut().uid_search(&criteria).await {
                Ok(found) if !found.is_empty() => all_uids.extend(found),
                Ok(_) => headerless.push(locator.clone()),
                Err(err) => return Err(err),
            }
        } else {
            headerless.push(locator.clone());
        }
    }

    for locator in headerless {
        if state.client_mut().uid_is_present(locator.uid).await? {
            all_uids.push(locator.uid);
        } else {
            store.mark_deleted(locator.id).await?;
            let _ = events.send(SyncEvent::EnvelopeRemoved { id: locator.id });
        }
    }

    if all_uids.is_empty() {
        persist_folder_unread(account, folder.id, store, events).await?;
        emit_unread_counts(store, events).await?;
        return Ok(());
    }

    all_uids.sort_unstable();
    all_uids.dedup();
    let chunk_sets = format_uid_set_chunks(&all_uids, 250);
    let mut errors = Vec::new();
    for (chunk_set, chunk_uids) in chunk_sets.iter().zip(all_uids.chunks(250)) {
        if let Err(err) = state.client_mut().uid_delete_set(chunk_set).await {
            let _ = mark_gone_chunk_uids(state, chunk_set, chunk_uids, folder.id, store).await;
            errors.push(err.to_string());
        }
    }

    store
        .mark_deleted_by_uids(folder.id, all_uids.clone())
        .await?;
    let rows = store.messages_by_uids(folder.id, &all_uids).await?;
    for (id, _, _) in rows {
        let _ = events.send(SyncEvent::EnvelopeRemoved { id });
    }
    persist_folder_unread(account, folder.id, store, events).await?;
    emit_unread_counts(store, events).await?;
    if !errors.is_empty() {
        return Err(Error::Imap(errors.join("; ")));
    }
    Ok(())
}

pub(super) async fn delete_messages(
    state: &mut WorkerImapState,
    account: &Account,
    ids: Vec<MessageId>,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
) -> Result<()> {
    if ids.is_empty() {
        return Ok(());
    }

    let folders = match store.list_folders(account.id).await {
        Ok(folders) => folders,
        Err(err) => {
            let _ = events.send(SyncEvent::MessageDeleteFailed {
                ids,
                reason: err.to_string(),
            });
            return Ok(());
        }
    };
    let mut errors: Vec<String> = Vec::new();
    let mut failed: Vec<MessageId> = Vec::new();
    let by_folder = match locators_by_folder(store, &ids, "delete messages").await {
        Ok(map) => map,
        Err(err) => {
            errors.push(err.to_string());
            failed.extend(ids.iter().copied());
            HashMap::new()
        }
    };

    for (folder_id, locators) in by_folder {
        let folder = match folders.iter().find(|f| f.id == folder_id) {
            Some(folder) => folder,
            None => {
                errors.push("folder not found for message".to_string());
                failed.extend(locators.iter().map(|locator| locator.id));
                continue;
            }
        };

        let mut move_to_trash_uids: Vec<u32> = Vec::new();
        let mut move_to_trash_ids: Vec<MessageId> = Vec::new();
        let mut expunge_uids: Vec<u32> = Vec::new();
        let mut expunge_ids: Vec<MessageId> = Vec::new();
        let mut draft_messages: Vec<MessageLocator> = Vec::new();

        for locator in locators {
            if matches!(folder.special, SpecialUse::Drafts) {
                draft_messages.push(locator);
                continue;
            }
            let has_other_copy = match store.has_live_copy_elsewhere(locator.id).await {
                Ok(value) => value,
                Err(err) => {
                    errors.push(err.to_string());
                    failed.push(locator.id);
                    continue;
                }
            };
            match plan_message_delete(&folders, folder, has_other_copy) {
                DeletePlan::MoveToTrash { .. } => {
                    move_to_trash_uids.push(locator.uid);
                    move_to_trash_ids.push(locator.id);
                }
                DeletePlan::RemoveFromMailbox => {
                    expunge_uids.push(locator.uid);
                    expunge_ids.push(locator.id);
                }
            }
        }

        if !draft_messages.is_empty() {
            let draft_ids: Vec<MessageId> =
                draft_messages.iter().map(|locator| locator.id).collect();
            if let Err(err) =
                delete_draft_messages_batch(state, account, folder, draft_messages, store, events)
                    .await
            {
                if is_recoverable_disconnect(&err) {
                    return Err(err);
                }
                errors.push(err.to_string());
                failed.extend(draft_ids);
            }
        }

        if !move_to_trash_uids.is_empty() {
            let delete_plan = folders
                .iter()
                .find(|f| f.selectable && matches!(f.special, SpecialUse::Trash))
                .map(|f| DeletePlan::MoveToTrash {
                    dest_imap_name: f.imap_name.clone(),
                    trash_id: f.id,
                })
                .unwrap_or(DeletePlan::RemoveFromMailbox);
            if let Err(err) = apply_folder_uid_deletes_with_plan(
                state,
                account,
                folder,
                folder_id,
                move_to_trash_uids,
                delete_plan,
                &folders,
                store,
                events,
            )
            .await
            {
                if is_recoverable_disconnect(&err) {
                    return Err(err);
                }
                errors.push(err.to_string());
                failed.extend(move_to_trash_ids);
            }
        }

        if !expunge_uids.is_empty()
            && let Err(err) = apply_folder_uid_deletes_with_plan(
                state,
                account,
                folder,
                folder_id,
                expunge_uids,
                DeletePlan::RemoveFromMailbox,
                &folders,
                store,
                events,
            )
            .await
        {
            if is_recoverable_disconnect(&err) {
                return Err(err);
            }
            errors.push(err.to_string());
            failed.extend(expunge_ids);
        }
    }

    if !failed.is_empty() {
        let _ = events.send(SyncEvent::MessageDeleteFailed {
            ids: failed,
            reason: errors.join("; "),
        });
    }
    Ok(())
}

pub(super) async fn mark_permanent_deleted_with_siblings(
    id: MessageId,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
) -> Result<()> {
    store.mark_deleted(id).await?;
    let siblings = store.live_sibling_ids(id).await?;
    for sibling in siblings {
        store.mark_deleted(sibling).await?;
        let _ = events.send(SyncEvent::EnvelopeRemoved { id: sibling });
    }
    let _ = events.send(SyncEvent::EnvelopeRemoved { id });
    Ok(())
}

pub(super) async fn collect_draft_expunge_uids(
    state: &mut WorkerImapState,
    folder: &Folder,
    messages: &[MessageLocator],
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
) -> Result<(Vec<u32>, Vec<MessageId>)> {
    ensure_selected(state, &folder.imap_name).await?;
    let mut all_uids: Vec<u32> = Vec::new();
    let mut headerless: Vec<MessageLocator> = Vec::new();
    let mut gone_ids: Vec<MessageId> = Vec::new();

    for locator in messages {
        let hdr = locator.message_id_hdr.as_deref().unwrap_or_default().trim();
        if !hdr.is_empty() {
            let criteria = imap_header_message_id_search(hdr);
            match state.client_mut().uid_search(&criteria).await {
                Ok(found) if !found.is_empty() => all_uids.extend(found),
                Ok(_) => headerless.push(locator.clone()),
                Err(err) => return Err(err),
            }
        } else {
            headerless.push(locator.clone());
        }
    }

    for locator in headerless {
        if state.client_mut().uid_is_present(locator.uid).await? {
            all_uids.push(locator.uid);
        } else {
            store.mark_deleted(locator.id).await?;
            let _ = events.send(SyncEvent::EnvelopeRemoved { id: locator.id });
            gone_ids.push(locator.id);
        }
    }

    all_uids.sort_unstable();
    all_uids.dedup();
    Ok((all_uids, gone_ids))
}

pub(super) async fn finish_reconcile_permanent_gone(
    state: &mut WorkerImapState,
    account: &Account,
    id: MessageId,
    locator: &MessageLocator,
    folder: &Folder,
    permanent_plan: PermanentPlan,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
) -> Result<()> {
    match permanent_plan {
        PermanentPlan::ExpungeHere => {
            store.mark_deleted(id).await?;
            let _ = events.send(SyncEvent::EnvelopeRemoved { id });
            persist_folder_unread(account, folder.id, store, events).await?;
            emit_unread_counts(store, events).await?;
        }
        PermanentPlan::PurgeViaTrash {
            trash_imap_name,
            trash_id,
        } => {
            ensure_selected(state, &trash_imap_name).await?;
            if let Ok(Some(dest_uid)) =
                resolve_moved_uid(state, locator.message_id_hdr.as_deref(), None).await
            {
                if let Err(err) = state.client_mut().uid_delete(dest_uid).await {
                    if is_recoverable_disconnect(&err) {
                        return Err(err);
                    }
                    let now = Utc::now();
                    let _ = store
                        .rehome_message(id, trash_id, dest_uid, Some(now))
                        .await;
                    let _ = events.send(SyncEvent::MessageDeleteFailed {
                        ids: vec![id],
                        reason: imap_error_text(&err),
                    });
                    persist_folder_unread(account, folder.id, store, events).await?;
                    persist_folder_unread(account, trash_id, store, events).await?;
                    emit_unread_counts(store, events).await?;
                    return Ok(());
                }
                mark_permanent_deleted_with_siblings(id, store, events).await?;
            } else {
                store.mark_deleted(id).await?;
                let _ = events.send(SyncEvent::EnvelopeRemoved { id });
            }
            persist_folder_unread(account, folder.id, store, events).await?;
            emit_unread_counts(store, events).await?;
        }
    }
    Ok(())
}

pub(super) async fn purge_messages_via_trash(
    state: &mut WorkerImapState,
    account: &Account,
    folder: &Folder,
    folder_id: FolderId,
    locators: &[MessageLocator],
    move_uids: Vec<u32>,
    trash_imap_name: &str,
    trash_id: FolderId,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
) -> Result<()> {
    if locators.is_empty() {
        return Ok(());
    }
    let locator_ids: Vec<MessageId> = locators.iter().map(|locator| locator.id).collect();

    ensure_selected(state, &folder.imap_name).await?;
    let trash_uidnext_before = snapshot_mailbox_uidnext(state, trash_imap_name, None).await?;

    let mut unique = move_uids;
    unique.sort_unstable();
    unique.dedup();
    if !unique.is_empty() {
        for chunk_set in format_uid_set_chunks(&unique, 250) {
            if let Err(err) = state
                .client_mut()
                .uid_move_or_copy_delete_set(&chunk_set, trash_imap_name)
                .await
            {
                if is_recoverable_disconnect(&err) {
                    return Err(err);
                }
                if let Err(emit_err) = emit_folder_envelopes(store, folder_id, events).await {
                    warn!(
                        error = %emit_err,
                        "failed to restore folder list after permanent delete move"
                    );
                }
                let _ = events.send(SyncEvent::MessageDeleteFailed {
                    ids: locator_ids,
                    reason: err.to_string(),
                });
                return Ok(());
            }
        }
    }

    ensure_selected(state, trash_imap_name).await?;
    let records: Vec<(MessageId, u32, Option<String>)> = locators
        .iter()
        .map(|locator| (locator.id, locator.uid, locator.message_id_hdr.clone()))
        .collect();
    let dest_uids =
        resolve_dest_uids_for_trash_chunk(state, &records, trash_uidnext_before).await?;

    if let Some(uidnext_before) = trash_uidnext_before {
        let resolved: HashSet<u32> = dest_uids.values().copied().collect();
        let mut seen_hdrs: HashSet<String> = HashSet::new();
        for locator in locators {
            if let Some(hdr) = locator.message_id_hdr.as_deref().map(str::trim)
                && !hdr.is_empty()
                && seen_hdrs.insert(hdr.to_string())
            {
                let criteria = imap_header_message_id_search(hdr);
                match state.client_mut().uid_search(&criteria).await {
                    Ok(uids) => {
                        let leftover: Vec<u32> = uids
                            .into_iter()
                            .filter(|uid| *uid >= uidnext_before && !resolved.contains(uid))
                            .collect();
                        for chunk_set in format_uid_set_chunks(&leftover, 250) {
                            if let Err(err) = state.client_mut().uid_delete_set(&chunk_set).await {
                                if is_recoverable_disconnect(&err) {
                                    return Err(err);
                                }
                                // No local id exists for these extras; report against the
                                // locators sharing this Message-ID without rehoming.
                                let _ = events.send(SyncEvent::MessageDeleteFailed {
                                    ids: vec![locator.id],
                                    reason: imap_error_text(&err),
                                });
                                break;
                            }
                        }
                    }
                    Err(err) => {
                        if is_recoverable_disconnect(&err) {
                            return Err(err);
                        }
                    }
                }
            }
        }
    }

    let mut pairs: Vec<(MessageId, u32)> = Vec::new();
    for locator in locators {
        if let Some(dest_uid) = dest_uids.get(&locator.id) {
            pairs.push((locator.id, *dest_uid));
        } else {
            let _ = events.send(SyncEvent::MessageDeleteFailed {
                ids: vec![locator.id],
                reason: "moved to Trash but the Trash copy could not be found".to_string(),
            });
        }
    }

    if pairs.is_empty() {
        persist_folder_unread(account, folder_id, store, events).await?;
        persist_folder_unread(account, trash_id, store, events).await?;
        emit_unread_counts(store, events).await?;
        return Ok(());
    }

    let mut dest_only: Vec<u32> = pairs.iter().map(|(_, uid)| *uid).collect();
    dest_only.sort_unstable();
    dest_only.dedup();
    for (chunk_set, chunk_uids) in format_uid_set_chunks(&dest_only, 250)
        .iter()
        .zip(dest_only.chunks(250))
    {
        let chunk_uid_set: HashSet<u32> = chunk_uids.iter().copied().collect();
        let chunk_pairs: Vec<(MessageId, u32)> = pairs
            .iter()
            .filter(|(_, uid)| chunk_uid_set.contains(uid))
            .copied()
            .collect();
        if let Err(err) = state.client_mut().uid_delete_set(chunk_set).await {
            if is_recoverable_disconnect(&err) {
                return Err(err);
            }
            let now = Utc::now();
            for (id, dest_uid) in &chunk_pairs {
                let _ = store
                    .rehome_message(*id, trash_id, *dest_uid, Some(now))
                    .await;
            }
            let failed_ids: Vec<MessageId> = chunk_pairs.iter().map(|(id, _)| *id).collect();
            let _ = events.send(SyncEvent::MessageDeleteFailed {
                ids: failed_ids,
                reason: imap_error_text(&err),
            });
            continue;
        }
        for (id, _) in chunk_pairs {
            mark_permanent_deleted_with_siblings(id, store, events).await?;
        }
    }

    persist_folder_unread(account, folder_id, store, events).await?;
    persist_folder_unread(account, trash_id, store, events).await?;
    emit_unread_counts(store, events).await?;
    Ok(())
}

pub(super) async fn expunge_one_locator(
    state: &mut WorkerImapState,
    account: &Account,
    folder: &Folder,
    folder_id: FolderId,
    locator: MessageLocator,
    permanent_plan: PermanentPlan,
    folders: &[Folder],
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
) -> Result<()> {
    match permanent_plan {
        PermanentPlan::ExpungeHere => {
            if matches!(folder.special, SpecialUse::Drafts) {
                delete_draft_messages_batch(state, account, folder, vec![locator], store, events)
                    .await?;
            } else if let Err(err) = apply_folder_uid_deletes_with_plan(
                state,
                account,
                folder,
                folder_id,
                vec![locator.uid],
                DeletePlan::RemoveFromMailbox,
                folders,
                store,
                events,
            )
            .await
            {
                if is_recoverable_disconnect(&err) {
                    return Err(err);
                }
                let _ = events.send(SyncEvent::MessageDeleteFailed {
                    ids: vec![locator.id],
                    reason: err.to_string(),
                });
            }
        }
        PermanentPlan::PurgeViaTrash {
            trash_imap_name,
            trash_id,
        } => {
            let (move_uids, gone_ids) = if matches!(folder.special, SpecialUse::Drafts) {
                collect_draft_expunge_uids(
                    state,
                    folder,
                    std::slice::from_ref(&locator),
                    store,
                    events,
                )
                .await?
            } else {
                (vec![locator.uid], Vec::new())
            };
            if gone_ids.contains(&locator.id) {
                return Ok(());
            }
            purge_messages_via_trash(
                state,
                account,
                folder,
                folder_id,
                &[locator],
                move_uids,
                &trash_imap_name,
                trash_id,
                store,
                events,
            )
            .await?;
        }
    }
    Ok(())
}

pub(super) async fn expunge_messages(
    state: &mut WorkerImapState,
    account: &Account,
    ids: Vec<MessageId>,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
    reconcile: bool,
) -> Result<()> {
    if ids.is_empty() {
        return Ok(());
    }

    let folders = match store.list_folders(account.id).await {
        Ok(folders) => folders,
        Err(err) => {
            let _ = events.send(SyncEvent::MessageDeleteFailed {
                ids,
                reason: err.to_string(),
            });
            return Ok(());
        }
    };
    let mut errors: Vec<String> = Vec::new();
    let mut failed: Vec<MessageId> = Vec::new();
    let by_folder = match locators_by_folder(store, &ids, "expunge messages").await {
        Ok(map) => map,
        Err(err) => {
            errors.push(err.to_string());
            failed.extend(ids.iter().copied());
            HashMap::new()
        }
    };

    for (folder_id, locators) in by_folder {
        let folder = match folders.iter().find(|f| f.id == folder_id) {
            Some(folder) => folder,
            None => {
                errors.push("folder not found for message".to_string());
                failed.extend(locators.iter().map(|locator| locator.id));
                continue;
            }
        };
        let permanent_plan = plan_permanent_delete(account, &folders, folder);

        if reconcile {
            for locator in locators {
                let locator_id = locator.id;
                let plan = permanent_plan.clone();
                if let Err(err) = ensure_selected(state, &folder.imap_name).await {
                    if is_recoverable_disconnect(&err) {
                        return Err(err);
                    }
                    errors.push(err.to_string());
                    failed.push(locator_id);
                    continue;
                }
                match state.client_mut().uid_is_present(locator.uid).await {
                    Ok(false) => {
                        if let Err(err) = finish_reconcile_permanent_gone(
                            state, account, locator.id, &locator, folder, plan, store, events,
                        )
                        .await
                        {
                            if is_recoverable_disconnect(&err) {
                                return Err(err);
                            }
                            errors.push(err.to_string());
                            failed.push(locator_id);
                        }
                    }
                    Ok(true) => {
                        if let Err(err) = expunge_one_locator(
                            state, account, folder, folder_id, locator, plan, &folders, store,
                            events,
                        )
                        .await
                        {
                            if is_recoverable_disconnect(&err) {
                                return Err(err);
                            }
                            errors.push(err.to_string());
                            failed.push(locator_id);
                        }
                    }
                    Err(err) => {
                        if is_recoverable_disconnect(&err) {
                            return Err(err);
                        }
                        errors.push(err.to_string());
                        failed.push(locator_id);
                    }
                }
            }
            continue;
        }

        match permanent_plan {
            PermanentPlan::ExpungeHere => {
                if matches!(folder.special, SpecialUse::Drafts) {
                    let draft_ids: Vec<MessageId> =
                        locators.iter().map(|locator| locator.id).collect();
                    if let Err(err) =
                        delete_draft_messages_batch(state, account, folder, locators, store, events)
                            .await
                    {
                        if is_recoverable_disconnect(&err) {
                            return Err(err);
                        }
                        errors.push(err.to_string());
                        failed.extend(draft_ids);
                    }
                } else {
                    let expunge_uids: Vec<u32> =
                        locators.iter().map(|locator| locator.uid).collect();
                    let expunge_ids: Vec<MessageId> =
                        locators.iter().map(|locator| locator.id).collect();
                    if let Err(err) = apply_folder_uid_deletes_with_plan(
                        state,
                        account,
                        folder,
                        folder_id,
                        expunge_uids,
                        DeletePlan::RemoveFromMailbox,
                        &folders,
                        store,
                        events,
                    )
                    .await
                    {
                        if is_recoverable_disconnect(&err) {
                            return Err(err);
                        }
                        errors.push(err.to_string());
                        failed.extend(expunge_ids);
                    }
                }
            }
            PermanentPlan::PurgeViaTrash {
                trash_imap_name,
                trash_id,
            } => {
                let (move_uids, gone_ids) = if matches!(folder.special, SpecialUse::Drafts) {
                    match collect_draft_expunge_uids(state, folder, &locators, store, events).await
                    {
                        Ok(pair) => pair,
                        Err(err) => {
                            if is_recoverable_disconnect(&err) {
                                return Err(err);
                            }
                            errors.push(err.to_string());
                            failed.extend(locators.iter().map(|locator| locator.id));
                            continue;
                        }
                    }
                } else {
                    (
                        locators.iter().map(|locator| locator.uid).collect(),
                        Vec::new(),
                    )
                };
                let gone_set: HashSet<MessageId> = gone_ids.into_iter().collect();
                let active_locators: Vec<MessageLocator> = locators
                    .iter()
                    .filter(|locator| !gone_set.contains(&locator.id))
                    .cloned()
                    .collect();
                if active_locators.is_empty() {
                    persist_folder_unread(account, folder_id, store, events).await?;
                    emit_unread_counts(store, events).await?;
                    continue;
                }
                if let Err(err) = purge_messages_via_trash(
                    state,
                    account,
                    folder,
                    folder_id,
                    &active_locators,
                    move_uids,
                    &trash_imap_name,
                    trash_id,
                    store,
                    events,
                )
                .await
                {
                    if is_recoverable_disconnect(&err) {
                        return Err(err);
                    }
                    errors.push(err.to_string());
                    failed.extend(locators.iter().map(|locator| locator.id));
                }
            }
        }
    }

    if !failed.is_empty() {
        let _ = events.send(SyncEvent::MessageDeleteFailed {
            ids: failed,
            reason: errors.join("; "),
        });
    }
    Ok(())
}

pub(super) async fn finish_source_already_gone(
    state: &mut WorkerImapState,
    account: &Account,
    id: MessageId,
    locator: &MessageLocator,
    folder: &Folder,
    delete_plan: DeletePlan,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
) -> Result<()> {
    match delete_plan {
        DeletePlan::MoveToTrash {
            dest_imap_name,
            trash_id,
        } => {
            let trash_uidnext = snapshot_mailbox_uidnext(state, &dest_imap_name, None)
                .await
                .unwrap_or_default();
            match ensure_selected(state, &dest_imap_name).await {
                Ok(()) => {
                    if let Ok(Some(dest_uid)) =
                        resolve_moved_uid(state, locator.message_id_hdr.as_deref(), trash_uidnext)
                            .await
                    {
                        match store
                            .rehome_message(id, trash_id, dest_uid, Some(Utc::now()))
                            .await
                        {
                            Ok(true) => {}
                            Ok(false) => {
                                store.mark_deleted(id).await?;
                            }
                            Err(err) => {
                                warn!(?err, "rehome_message failed after source UID gone");
                                store.mark_deleted(id).await?;
                            }
                        }
                    } else {
                        store.mark_deleted(id).await?;
                    }
                }
                Err(_) => {
                    store.mark_deleted(id).await?;
                }
            }
        }
        DeletePlan::RemoveFromMailbox => {
            store.mark_deleted(id).await?;
        }
    }

    let _ = events.send(SyncEvent::EnvelopeRemoved { id });
    if let Err(err) = persist_folder_unread(account, folder.id, store, events).await {
        warn!(?err, "persist_folder_unread failed after delete reconcile");
    }
    if let Err(err) = emit_unread_counts(store, events).await {
        warn!(?err, "emit_unread_counts failed after delete reconcile");
    }
    Ok(())
}

pub(super) fn reconcile_unconfirmed_ids(
    ids: &[MessageId],
    located: &HashSet<MessageId>,
    confirmed: &HashSet<MessageId>,
) -> Vec<MessageId> {
    ids.iter()
        .copied()
        .filter(|id| located.contains(id) && !confirmed.contains(id))
        .collect()
}

pub(super) async fn reconcile_delete_messages(
    state: &mut WorkerImapState,
    account: &Account,
    ids: Vec<MessageId>,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
) -> Result<()> {
    if ids.is_empty() {
        return Ok(());
    }

    let folders = match store.list_folders(account.id).await {
        Ok(folders) => folders,
        Err(err) => {
            let _ = events.send(SyncEvent::MessageDeleteFailed {
                ids,
                reason: err.to_string(),
            });
            return Ok(());
        }
    };

    let by_folder = match locators_by_folder(store, &ids, "reconcile delete messages").await {
        Ok(map) => map,
        Err(err) => {
            let _ = events.send(SyncEvent::MessageDeleteFailed {
                ids,
                reason: err.to_string(),
            });
            return Ok(());
        }
    };

    let mut confirmed: HashSet<MessageId> = HashSet::new();
    let mut failed: Vec<MessageId> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    let located: HashSet<MessageId> = by_folder
        .values()
        .flat_map(|locators| locators.iter().map(|locator| locator.id))
        .collect();

    for (folder_id, locators) in by_folder {
        let folder = match folders.iter().find(|f| f.id == folder_id) {
            Some(folder) => folder,
            None => {
                errors.push("folder not found for message".to_string());
                failed.extend(locators.iter().map(|locator| locator.id));
                continue;
            }
        };

        for locator in locators {
            let delete_plan = if matches!(folder.special, SpecialUse::Drafts) {
                DeletePlan::RemoveFromMailbox
            } else {
                let has_other_copy = match store.has_live_copy_elsewhere(locator.id).await {
                    Ok(value) => value,
                    Err(err) => {
                        errors.push(err.to_string());
                        failed.push(locator.id);
                        continue;
                    }
                };
                plan_message_delete(&folders, folder, has_other_copy)
            };

            if let Err(err) = ensure_selected(state, &folder.imap_name).await {
                if is_recoverable_disconnect(&err) {
                    let pending = reconcile_unconfirmed_ids(&ids, &located, &confirmed);
                    let _ = events.send(SyncEvent::MessageDeleteFailed {
                        ids: pending,
                        reason: err.to_string(),
                    });
                    return Err(err);
                }
                errors.push(err.to_string());
                failed.push(locator.id);
                continue;
            }

            match state.client_mut().uid_is_present(locator.uid).await {
                Ok(false) => match finish_source_already_gone(
                    state,
                    account,
                    locator.id,
                    &locator,
                    folder,
                    delete_plan,
                    store,
                    events,
                )
                .await
                {
                    Ok(()) => {
                        confirmed.insert(locator.id);
                    }
                    Err(err) => {
                        errors.push(err.to_string());
                        failed.push(locator.id);
                    }
                },
                Ok(true) => {
                    let apply_plan = match delete_plan {
                        DeletePlan::MoveToTrash {
                            dest_imap_name,
                            trash_id,
                        } => DeletePlan::MoveToTrash {
                            dest_imap_name,
                            trash_id,
                        },
                        DeletePlan::RemoveFromMailbox => DeletePlan::RemoveFromMailbox,
                    };
                    if let Err(err) = apply_folder_uid_deletes_with_plan(
                        state,
                        account,
                        folder,
                        folder_id,
                        vec![locator.uid],
                        apply_plan,
                        &folders,
                        store,
                        events,
                    )
                    .await
                    {
                        if is_recoverable_disconnect(&err) {
                            let pending = reconcile_unconfirmed_ids(&ids, &located, &confirmed);
                            let _ = events.send(SyncEvent::MessageDeleteFailed {
                                ids: pending,
                                reason: err.to_string(),
                            });
                            return Err(err);
                        }
                        errors.push(err.to_string());
                        failed.push(locator.id);
                    } else {
                        confirmed.insert(locator.id);
                    }
                }
                Err(err) => {
                    if is_recoverable_disconnect(&err) {
                        let pending = reconcile_unconfirmed_ids(&ids, &located, &confirmed);
                        let _ = events.send(SyncEvent::MessageDeleteFailed {
                            ids: pending,
                            reason: err.to_string(),
                        });
                        return Err(err);
                    }
                    errors.push(err.to_string());
                    failed.push(locator.id);
                }
            }
        }
    }

    if !failed.is_empty() {
        let _ = events.send(SyncEvent::MessageDeleteFailed {
            ids: failed,
            reason: errors.join("; "),
        });
    }
    Ok(())
}

pub(super) async fn undelete_message(
    state: &mut WorkerImapState,
    account: &Account,
    id: MessageId,
    dest: FolderId,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
) -> Result<()> {
    let stored = store
        .get_message(id)
        .await?
        .ok_or_else(|| Error::Store("message not found".to_string()))?;

    if stored.folder_id == dest {
        return Ok(());
    }

    let folders = store.list_folders(account.id).await?;
    let source_folder = folders
        .iter()
        .find(|f| f.id == stored.folder_id)
        .ok_or_else(|| Error::Store("folder not found for message".to_string()))?;
    let dest_folder = folders
        .iter()
        .find(|f| f.id == dest)
        .ok_or_else(|| Error::Store("destination folder not found".to_string()))?;

    let source_name = source_folder.imap_name.clone();
    let dest_name = dest_folder.imap_name.clone();
    let dest_uidnext = snapshot_mailbox_uidnext(state, &dest_name, dest_folder.uidnext).await?;
    let source_folder_id = source_folder.id;

    if let Err(err) =
        uid_move_from_selected(state, &source_name, stored.uid, &dest_name, account, None).await
    {
        if let Err(emit_err) = emit_folder_envelopes(store, source_folder_id, events).await {
            warn!(error = %emit_err, "failed to restore folder list after undelete");
        }
        return Err(err);
    }

    ensure_selected(state, &dest_name).await?;
    let dest_uid = resolve_moved_uid(state, stored.message_id_hdr.as_deref(), dest_uidnext)
        .await?
        .ok_or_else(|| {
            Error::Imap("could not resolve destination UID after undelete MOVE".into())
        })?;
    let rehomed = store.rehome_message(id, dest, dest_uid, None).await?;
    if !rehomed {
        return Err(Error::Store(
            "could not rehome after undelete MOVE".to_string(),
        ));
    }

    let _ = events.send(SyncEvent::EnvelopeRemoved { id });
    select_folder(store, dest, list_mode(), events).await?;
    if matches!(dest_folder.special, SpecialUse::Inbox) {
        refresh_virtual_inboxes(store, events).await?;
    }
    if matches!(source_folder.special, SpecialUse::Trash) {
        select_folder(store, source_folder_id, list_mode(), events).await?;
    }

    persist_folder_unread(account, source_folder_id, store, events).await?;
    persist_folder_unread(account, dest, store, events).await?;
    emit_unread_counts(store, events).await?;
    Ok(())
}

pub(super) async fn mark_gone_chunk_uids(
    state: &mut WorkerImapState,
    chunk_set: &str,
    chunk_uids: &[u32],
    folder_id: FolderId,
    store: &StoreHandle,
) -> Result<(usize, HashSet<u32>)> {
    let still_present = state
        .client_mut()
        .uid_search(&format!("UID {chunk_set}"))
        .await?;
    let still_present_set: HashSet<u32> = still_present.into_iter().collect();
    let chunk_uids_set: HashSet<u32> = chunk_uids.iter().copied().collect();
    let gone: Vec<u32> = chunk_uids_set
        .difference(&still_present_set)
        .copied()
        .collect();
    let removed = if gone.is_empty() {
        0
    } else {
        store.mark_deleted_by_uids(folder_id, gone).await?
    };
    Ok((removed, still_present_set))
}

pub(super) async fn apply_folder_uid_deletes(
    state: &mut WorkerImapState,
    account: &Account,
    folder: &Folder,
    folder_id: FolderId,
    uids: Vec<u32>,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
) -> Result<(usize, bool)> {
    let folders = store.list_folders(account.id).await?;
    let delete_plan = plan_message_delete(&folders, folder, false);
    apply_folder_uid_deletes_with_plan(
        state,
        account,
        folder,
        folder_id,
        uids,
        delete_plan,
        &folders,
        store,
        events,
    )
    .await
}

pub(super) async fn apply_folder_uid_deletes_with_plan(
    state: &mut WorkerImapState,
    account: &Account,
    folder: &Folder,
    folder_id: FolderId,
    uids: Vec<u32>,
    delete_plan: DeletePlan,
    folders: &[Folder],
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
) -> Result<(usize, bool)> {
    let trash_folder = folders
        .iter()
        .find(|f| f.selectable && matches!(f.special, SpecialUse::Trash))
        .cloned();
    let moved_to_trash = matches!(delete_plan, DeletePlan::MoveToTrash { .. });

    if uids.is_empty() {
        return Ok((0, moved_to_trash));
    }

    let mut trash_uidnext = if let Some(trash) = trash_folder.as_ref() {
        snapshot_mailbox_uidnext(state, &trash.imap_name, trash.uidnext).await?
    } else {
        None
    };

    let mut unique = uids;
    unique.sort_unstable();
    unique.dedup();
    let chunk_sets = format_uid_set_chunks(&unique, 250);
    let mut total_removed = 0usize;
    let mut errors: Vec<String> = Vec::new();

    for (chunk_set, chunk_uids) in chunk_sets.iter().zip(unique.chunks(250)) {
        ensure_selected(state, &folder.imap_name).await?;
        let delete_result = match &delete_plan {
            DeletePlan::MoveToTrash { dest_imap_name, .. } => {
                state
                    .client_mut()
                    .uid_move_or_copy_delete_set(chunk_set, dest_imap_name)
                    .await
            }
            DeletePlan::RemoveFromMailbox => state.client_mut().uid_delete_set(chunk_set).await,
        };

        if let Err(err) = delete_result {
            if is_recoverable_disconnect(&err) {
                return Err(err);
            }
            let (removed, still_present) = if let (Some(trash), DeletePlan::MoveToTrash { .. }) =
                (&trash_folder, &delete_plan)
            {
                finish_trash_move_chunk(
                    state,
                    store,
                    chunk_set,
                    chunk_uids,
                    folder_id,
                    trash,
                    trash_uidnext,
                )
                .await?
            } else {
                mark_gone_chunk_uids(state, chunk_set, chunk_uids, folder_id, store).await?
            };
            let _ = (removed, still_present);
            errors.push(imap_error_text(&err));
            continue;
        }

        let (removed, still_present) =
            if let (Some(trash), DeletePlan::MoveToTrash { .. }) = (&trash_folder, &delete_plan) {
                finish_trash_move_chunk(
                    state,
                    store,
                    chunk_set,
                    chunk_uids,
                    folder_id,
                    trash,
                    trash_uidnext,
                )
                .await?
            } else {
                mark_gone_chunk_uids(state, chunk_set, chunk_uids, folder_id, store).await?
            };
        if matches!(delete_plan, DeletePlan::MoveToTrash { .. })
            && let Some(trash) = trash_folder.as_ref()
        {
            trash_uidnext =
                snapshot_mailbox_uidnext(state, &trash.imap_name, trash_uidnext).await?;
        }
        if let DeletePlan::MoveToTrash { dest_imap_name, .. } = &delete_plan
            && removed == 0
            && still_present.len() == chunk_uids.len()
        {
            errors.push(format!(
                "UID MOVE {chunk_set} to {dest_imap_name} left all messages in the source folder"
            ));
            continue;
        }
        total_removed += removed;
    }

    replace_folder_envelopes_after_delete(account, folder, folder_id, store, events).await?;
    if let Some(trash) = trash_folder.as_ref() {
        refresh_trash_after_move(account, trash, store, events).await?;
    }
    emit_unread_counts(store, events).await?;
    if !errors.is_empty() {
        return Err(Error::Imap(errors.join("; ")));
    }
    Ok((total_removed, moved_to_trash))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worker::test_support::{account_with_imap_host, folder_named};
    use alcedo_core::{FolderId, MessageId, SpecialUse};
    use std::collections::{HashMap, HashSet};

    #[test]
    fn reconcile_unconfirmed_ids_skips_missing_locators_and_confirmed() {
        let ids = [MessageId(1), MessageId(2), MessageId(3)];
        let located = HashSet::from([MessageId(2), MessageId(3)]);
        let confirmed = HashSet::from([MessageId(2)]);
        assert_eq!(
            reconcile_unconfirmed_ids(&ids, &located, &confirmed),
            vec![MessageId(3)]
        );
    }

    #[test]
    fn assign_remaining_trash_dest_uids_pairs_by_source_uid() {
        let mut map = HashMap::new();
        assign_remaining_trash_dest_uids(
            &mut map,
            vec![(MessageId(2), 20), (MessageId(1), 10)],
            vec![102, 101],
        );
        assert_eq!(map.get(&MessageId(1)), Some(&101));
        assert_eq!(map.get(&MessageId(2)), Some(&102));
    }

    #[test]
    fn assign_remaining_trash_dest_uids_skips_when_available_shorter() {
        let mut map = HashMap::new();
        assign_remaining_trash_dest_uids(
            &mut map,
            vec![(MessageId(1), 10), (MessageId(2), 20)],
            vec![50],
        );
        assert_eq!(map.get(&MessageId(1)), Some(&50));
        assert!(!map.contains_key(&MessageId(2)));
    }

    #[test]
    fn plan_message_delete_moves_only_copy_to_trash() {
        let inbox = folder_named(1, SpecialUse::Inbox, "INBOX", true);
        let folders = vec![
            inbox.clone(),
            folder_named(2, SpecialUse::Trash, "Trash", true),
        ];
        assert!(matches!(
            plan_message_delete(&folders, &inbox, false),
            DeletePlan::MoveToTrash { dest_imap_name, trash_id: FolderId(2) }
                if dest_imap_name == "Trash"
        ));
    }

    #[test]
    fn plan_message_delete_expunges_drafts() {
        let drafts = folder_named(3, SpecialUse::Drafts, "Drafts", true);
        let folders = vec![
            drafts.clone(),
            folder_named(2, SpecialUse::Trash, "Trash", true),
        ];
        assert_eq!(
            plan_message_delete(&folders, &drafts, false),
            DeletePlan::RemoveFromMailbox
        );
    }

    #[test]
    fn plan_message_delete_expunges_copy_when_sibling_exists() {
        let projects = folder_named(3, SpecialUse::Other, "Projects", true);
        let folders = vec![
            folder_named(1, SpecialUse::Inbox, "INBOX", true),
            folder_named(2, SpecialUse::Trash, "Trash", true),
            projects.clone(),
        ];
        assert_eq!(
            plan_message_delete(&folders, &projects, true),
            DeletePlan::RemoveFromMailbox
        );
    }

    #[test]
    fn plan_message_delete_expunges_in_trash() {
        let trash = folder_named(2, SpecialUse::Trash, "Trash", true);
        let folders = vec![
            folder_named(1, SpecialUse::Inbox, "INBOX", true),
            trash.clone(),
        ];
        assert_eq!(
            plan_message_delete(&folders, &trash, false),
            DeletePlan::RemoveFromMailbox
        );
    }

    #[test]
    fn plan_permanent_delete_routes_by_host_and_folder() {
        let trash = folder_named(2, SpecialUse::Trash, "Trash", true);
        let inbox = folder_named(1, SpecialUse::Inbox, "INBOX", true);
        let drafts = folder_named(3, SpecialUse::Drafts, "Drafts", true);
        let junk = folder_named(4, SpecialUse::Junk, "Junk", true);
        let folders_with_trash = vec![inbox.clone(), trash.clone(), drafts.clone(), junk.clone()];
        let other_account = account_with_imap_host("imap.example.com");
        let gmail_account = account_with_imap_host("imap.gmail.com");

        assert_eq!(
            plan_permanent_delete(&other_account, &folders_with_trash, &inbox),
            PermanentPlan::ExpungeHere
        );
        assert_eq!(
            plan_permanent_delete(&other_account, &folders_with_trash, &trash),
            PermanentPlan::ExpungeHere
        );
        assert_eq!(
            plan_permanent_delete(&other_account, &folders_with_trash, &drafts),
            PermanentPlan::ExpungeHere
        );

        assert!(matches!(
            plan_permanent_delete(&gmail_account, &folders_with_trash, &inbox),
            PermanentPlan::PurgeViaTrash {
                trash_id: FolderId(2),
                trash_imap_name,
            } if trash_imap_name == "Trash"
        ));
        assert_eq!(
            plan_permanent_delete(&gmail_account, &folders_with_trash, &trash),
            PermanentPlan::ExpungeHere
        );
        assert_eq!(
            plan_permanent_delete(&gmail_account, &folders_with_trash, &junk),
            PermanentPlan::ExpungeHere
        );

        let gmail_inbox_no_trash = vec![inbox.clone(), drafts.clone(), junk.clone()];
        assert_eq!(
            plan_permanent_delete(&gmail_account, &gmail_inbox_no_trash, &inbox),
            PermanentPlan::ExpungeHere
        );
    }
}
