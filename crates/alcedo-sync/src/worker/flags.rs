//! Flag changes: single and batched STORE, and mark folder read.

use super::mailbox_sync::{ensure_selected, persist_folder_unread};
use super::*;

pub(super) async fn set_flag(
    state: &mut WorkerImapState,
    account: &Account,
    id: MessageId,
    seen: Option<bool>,
    flagged: Option<bool>,
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

    store.set_flags(id, seen, flagged).await?;

    let envelopes = store
        .folder_messages(stored.folder_id, LIST_LIMIT, 0)
        .await?;
    if let Some(updated) = envelopes.into_iter().find(|e| e.id == id) {
        let _ = events.send(SyncEvent::EnvelopeUpdated(updated));
    }

    persist_folder_unread(account, stored.folder_id, store, events).await?;
    emit_unread_counts(store, events).await?;

    ensure_selected(state, &folder.imap_name).await?;
    if let Err(store_err) = state
        .client_mut()
        .uid_store_flags(stored.uid, seen, flagged)
        .await
    {
        match state.client_mut().uid_fetch_flags(stored.uid).await {
            Ok(Some((srv_seen, srv_flagged))) => {
                store
                    .set_flags(id, Some(srv_seen), Some(srv_flagged))
                    .await?;
                let envelopes = store
                    .folder_messages(stored.folder_id, LIST_LIMIT, 0)
                    .await?;
                if let Some(updated) = envelopes.into_iter().find(|e| e.id == id) {
                    let _ = events.send(SyncEvent::EnvelopeUpdated(updated));
                }
                persist_folder_unread(account, stored.folder_id, store, events).await?;
                emit_unread_counts(store, events).await?;
            }
            Ok(None) => {
                store.mark_deleted(id).await?;
                let _ = events.send(SyncEvent::EnvelopeRemoved { id });
                persist_folder_unread(account, stored.folder_id, store, events).await?;
                emit_unread_counts(store, events).await?;
            }
            Err(fetch_err) => {
                warn!(
                    error = %fetch_err,
                    "flag reconcile FETCH failed; keeping optimistic flags"
                );
                if let Ok(envelopes) = store.folder_messages(stored.folder_id, LIST_LIMIT, 0).await
                    && let Some(updated) = envelopes.into_iter().find(|e| e.id == id)
                {
                    let _ = events.send(SyncEvent::EnvelopeUpdated(updated));
                }
            }
        }
        return Err(store_err);
    }
    Ok(())
}

pub(super) async fn locators_by_folder(
    store: &StoreHandle,
    ids: &[MessageId],
    what: &str,
) -> Result<HashMap<FolderId, Vec<MessageLocator>>> {
    let mut by_folder: HashMap<FolderId, Vec<MessageLocator>> = HashMap::new();
    let locators = match store.message_locators(ids).await {
        Ok(locators) => locators,
        Err(err) => {
            warn!(error = %err, "{what}: message_locators failed");
            return Err(err);
        }
    };
    let found: HashSet<MessageId> = locators.iter().map(|locator| locator.id).collect();
    for id in ids {
        if !found.contains(id) {
            warn!(?id, "{what}: message not in store");
        }
    }
    for locator in locators {
        by_folder
            .entry(locator.folder_id)
            .or_default()
            .push(locator);
    }
    Ok(by_folder)
}

pub(super) async fn set_flags_batch(
    state: &mut WorkerImapState,
    account: &Account,
    ids: Vec<MessageId>,
    seen: Option<bool>,
    flagged: Option<bool>,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
) -> Result<()> {
    if ids.is_empty() {
        return Ok(());
    }

    let folders = store.list_folders(account.id).await?;
    let mut errors: Vec<String> = Vec::new();
    let by_folder = match locators_by_folder(store, &ids, "set flags").await {
        Ok(map) => map,
        Err(err) => {
            errors.push(err.to_string());
            HashMap::new()
        }
    };
    let mut any_folder = false;

    for (folder_id, locators) in by_folder {
        any_folder = true;
        let folder = match folders.iter().find(|f| f.id == folder_id) {
            Some(folder) => folder,
            None => {
                errors.push("folder not found for message".to_string());
                continue;
            }
        };

        let changed_ids: Vec<MessageId> = locators.iter().map(|locator| locator.id).collect();
        if let Err(err) = store.set_flags_many(&changed_ids, seen, flagged).await {
            errors.push(err.to_string());
        }

        let envelopes = store.folder_messages(folder_id, LIST_LIMIT, 0).await?;
        for id in &changed_ids {
            if let Some(updated) = envelopes.iter().find(|e| e.id == *id) {
                let _ = events.send(SyncEvent::EnvelopeUpdated(updated.clone()));
            }
        }

        if let Err(err) = persist_folder_unread(account, folder_id, store, events).await {
            errors.push(err.to_string());
        }

        let uids: Vec<u32> = locators.iter().map(|locator| locator.uid).collect();
        if uids.is_empty() {
            continue;
        }

        if let Err(err) = ensure_selected(state, &folder.imap_name).await {
            errors.push(err.to_string());
            continue;
        }

        for chunk_set in format_uid_set_chunks(&uids, 250) {
            if let Err(err) = state
                .client_mut()
                .uid_store_flags_set(&chunk_set, seen, flagged)
                .await
            {
                errors.push(err.to_string());
            }
        }
    }

    if any_folder && let Err(err) = emit_unread_counts(store, events).await {
        errors.push(err.to_string());
    }

    if !errors.is_empty() {
        return Err(Error::Imap(errors.join("; ")));
    }
    Ok(())
}

pub(super) async fn mark_folder_read(
    state: &mut WorkerImapState,
    account: &Account,
    folder_id: FolderId,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
) -> Result<()> {
    let folder = match store.get_folder(folder_id).await? {
        Some(folder) => folder,
        None => return Ok(()),
    };
    if folder.account_id != account.id || !folder.selectable {
        return Ok(());
    }

    emit_status(
        events,
        account,
        format!("Marking {} read…", folder.imap_name),
    );

    ensure_selected(state, &folder.imap_name).await?;
    state.client_mut().uid_store_seen_all().await?;
    store.mark_folder_seen(folder_id).await?;
    persist_folder_unread(account, folder_id, store, events).await?;

    select_folder(store, folder_id, list_mode(), events).await?;

    if matches!(folder.special, SpecialUse::Inbox) {
        refresh_virtual_inboxes(store, events).await?;
    }

    emit_unread_counts(store, events).await?;
    Ok(())
}
