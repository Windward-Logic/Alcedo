//! Flag changes: single and batched STORE, and mark folder read.

use alcedo_i18n::trf;
use alcedo_store::{FlagMirrorSource, MirroredCopy, MirroredFlags};

use super::flag_copies::{FlagScope, mirror_flags, restore_mirrored_flags};
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

    let scope = FlagScope::of(state.client_mut().capabilities());
    let target = FlagTarget {
        scope,
        account,
        id,
        folder_id: stored.folder_id,
    };
    let mirrored = apply_flag_change_locally(&target, seen, flagged, store, events).await?;

    if let Err(err) = ensure_selected(state, &folder.imap_name).await {
        restore_mirrored_flags(account, &mirrored, store, events).await;
        return Err(err);
    }
    if let Err(store_err) = state
        .client_mut()
        .uid_store_flags(stored.uid, seen, flagged)
        .await
    {
        let server_flags = state.client_mut().uid_fetch_flags(stored.uid).await;
        settle_rejected_flag_change(&target, server_flags, &mirrored, store, events).await?;
        return Err(store_err);
    }
    Ok(())
}

/// The message a single flag change applies to.
struct FlagTarget<'a> {
    scope: FlagScope,
    account: &'a Account,
    id: MessageId,
    folder_id: FolderId,
}

/// Publish updated rows, whatever their position in the folder.
async fn emit_envelope_updates(
    ids: &[MessageId],
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
) -> Result<()> {
    for updated in store.envelopes_by_ids(ids).await? {
        let _ = events.send(SyncEvent::EnvelopeUpdated(updated));
    }
    Ok(())
}

/// Show a flag change locally before the server confirms it, on this row and on
/// its copies. Returns the copies changed so a rejected write can undo them.
async fn apply_flag_change_locally(
    target: &FlagTarget<'_>,
    seen: Option<bool>,
    flagged: Option<bool>,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
) -> Result<Vec<MirroredCopy>> {
    let FlagTarget {
        scope,
        account,
        id,
        folder_id,
    } = *target;
    store.set_flags(id, seen, flagged).await?;
    emit_envelope_updates(&[id], store, events).await?;
    persist_folder_unread(account, folder_id, store, events).await?;
    let mirrored = mirror_flags(
        scope,
        account,
        FlagMirrorSource::Messages(vec![id]),
        MirroredFlags::requested(seen, flagged),
        store,
        events,
    )
    .await;
    if let Err(err) = emit_unread_counts(store, events).await {
        restore_mirrored_flags(account, &mirrored, store, events).await;
        return Err(err);
    }
    Ok(mirrored)
}

/// Bring local state back in line after the server rejected a flag change.
/// `server_flags` is the follow-up FETCH: the message's flags, `None` when it is
/// gone from this folder, or the error when the FETCH failed too.
async fn settle_rejected_flag_change(
    target: &FlagTarget<'_>,
    server_flags: Result<Option<(bool, bool)>>,
    mirrored: &[MirroredCopy],
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
) -> Result<()> {
    let FlagTarget {
        scope,
        account,
        id,
        folder_id,
    } = *target;
    match server_flags {
        Ok(Some((srv_seen, srv_flagged))) => {
            store
                .set_flags(id, Some(srv_seen), Some(srv_flagged))
                .await?;
            emit_envelope_updates(&[id], store, events).await?;
            persist_folder_unread(account, folder_id, store, events).await?;
            mirror_flags(
                scope,
                account,
                FlagMirrorSource::Messages(vec![id]),
                MirroredFlags::ALL,
                store,
                events,
            )
            .await;
            emit_unread_counts(store, events).await?;
        }
        Ok(None) => {
            restore_mirrored_flags(account, mirrored, store, events).await;
            store.mark_deleted(id).await?;
            let _ = events.send(SyncEvent::EnvelopeRemoved { id });
            persist_folder_unread(account, folder_id, store, events).await?;
            emit_unread_counts(store, events).await?;
        }
        Err(fetch_err) => {
            warn!(
                error = %fetch_err,
                "flag reconcile FETCH failed; keeping optimistic flags on this row only"
            );
            restore_mirrored_flags(account, mirrored, store, events).await;
            if let Err(err) = emit_envelope_updates(&[id], store, events).await {
                warn!(error = %err, "could not reload the message after a failed flag change");
            }
        }
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

/// One folder's share of a batch flag change, applied locally and awaiting the
/// server write.
struct PendingFolderStore {
    imap_name: String,
    uids: Vec<u32>,
    mirrored: Vec<MirroredCopy>,
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

    let scope = FlagScope::of(state.client_mut().capabilities());
    let mut errors: Vec<String> = Vec::new();
    let pending = apply_batch_locally(
        scope,
        account,
        &ids,
        seen,
        flagged,
        store,
        events,
        &mut errors,
    )
    .await?;

    for folder_store in pending {
        let server_errors = store_batch_on_server(state, &folder_store, seen, flagged).await;
        if !server_errors.is_empty() {
            errors.extend(server_errors);
            restore_mirrored_flags(account, &folder_store.mirrored, store, events).await;
        }
    }

    if !errors.is_empty() {
        return Err(Error::Imap(errors.join("; ")));
    }
    Ok(())
}

/// Show a batch flag change locally, on the rows and their copies, before any
/// network round trip, so indicators do not wait on the server.
async fn apply_batch_locally(
    scope: FlagScope,
    account: &Account,
    ids: &[MessageId],
    seen: Option<bool>,
    flagged: Option<bool>,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
    errors: &mut Vec<String>,
) -> Result<Vec<PendingFolderStore>> {
    let folders = store.list_folders(account.id).await?;
    let by_folder = match locators_by_folder(store, ids, "set flags").await {
        Ok(map) => map,
        Err(err) => {
            errors.push(err.to_string());
            HashMap::new()
        }
    };

    let mut pending = Vec::with_capacity(by_folder.len());
    for (folder_id, locators) in by_folder {
        let Some(folder) = folders.iter().find(|f| f.id == folder_id) else {
            errors.push("folder not found for message".to_string());
            continue;
        };

        let changed_ids: Vec<MessageId> = locators.iter().map(|locator| locator.id).collect();
        let mut mirrored = Vec::new();
        match store.set_flags_many(&changed_ids, seen, flagged).await {
            Ok(()) => {
                if let Err(err) = emit_envelope_updates(&changed_ids, store, events).await {
                    errors.push(err.to_string());
                }
                mirrored = mirror_flags(
                    scope,
                    account,
                    FlagMirrorSource::Messages(changed_ids),
                    MirroredFlags::requested(seen, flagged),
                    store,
                    events,
                )
                .await;
            }
            Err(err) => errors.push(err.to_string()),
        }

        if let Err(err) = persist_folder_unread(account, folder_id, store, events).await {
            errors.push(err.to_string());
        }

        pending.push(PendingFolderStore {
            imap_name: folder.imap_name.clone(),
            uids: locators.iter().map(|locator| locator.uid).collect(),
            mirrored,
        });
    }

    if !pending.is_empty()
        && let Err(err) = emit_unread_counts(store, events).await
    {
        errors.push(err.to_string());
    }
    Ok(pending)
}

/// Write one folder's flags to the server. Returns what went wrong, if anything.
async fn store_batch_on_server(
    state: &mut WorkerImapState,
    folder_store: &PendingFolderStore,
    seen: Option<bool>,
    flagged: Option<bool>,
) -> Vec<String> {
    if let Err(err) = ensure_selected(state, &folder_store.imap_name).await {
        return vec![err.to_string()];
    }
    let mut errors = Vec::new();
    for chunk_set in format_uid_set_chunks(&folder_store.uids, 250) {
        if let Err(err) = state
            .client_mut()
            .uid_store_flags_set(&chunk_set, seen, flagged)
            .await
        {
            errors.push(err.to_string());
        }
    }
    errors
}

/// Record a whole folder as read, here and on its messages' copies elsewhere.
async fn apply_folder_read_locally(
    scope: FlagScope,
    account: &Account,
    folder_id: FolderId,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
) -> Result<()> {
    store.mark_folder_seen(folder_id).await?;
    persist_folder_unread(account, folder_id, store, events).await?;
    mirror_flags(
        scope,
        account,
        FlagMirrorSource::Folder(folder_id),
        MirroredFlags::SEEN,
        store,
        events,
    )
    .await;
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
        trf!("Marking {folder} read…", folder = folder.imap_name),
    );

    ensure_selected(state, &folder.imap_name).await?;
    state.client_mut().uid_store_seen_all().await?;
    let scope = FlagScope::of(state.client_mut().capabilities());
    apply_folder_read_locally(scope, account, folder_id, store, events).await?;

    select_folder(store, folder_id, list_mode(), events).await?;

    if matches!(folder.special, SpecialUse::Inbox) {
        refresh_virtual_inboxes(store, events).await?;
    }

    emit_unread_counts(store, events).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worker::test_support::CopyFixture;

    fn target(fx: &CopyFixture, scope: FlagScope) -> FlagTarget<'_> {
        FlagTarget {
            scope,
            account: &fx.account,
            id: fx.original,
            folder_id: fx.inbox,
        }
    }

    async fn read_original(fx: &CopyFixture, tx: &flume::Sender<SyncEvent>) -> Vec<MirroredCopy> {
        let target = target(fx, FlagScope::SharedAcrossLabels);
        apply_flag_change_locally(&target, Some(true), None, &fx.store, tx)
            .await
            .expect("apply")
    }

    #[tokio::test]
    async fn reading_a_message_marks_its_copy_read_and_refreshes_the_indicators() {
        let fx = CopyFixture::new().await;
        let (tx, rx) = flume::unbounded::<SyncEvent>();

        read_original(&fx, &tx).await;

        assert!(fx.is_seen(fx.copy).await);
        assert_eq!(fx.stored_unread(fx.all_mail).await, 1);
        let events: Vec<SyncEvent> = rx.try_iter().collect();
        assert!(events.iter().any(
            |event| matches!(event, SyncEvent::EnvelopeUpdated(envelope) if envelope.id == fx.copy)
        ));
        assert!(matches!(
            events.last(),
            Some(SyncEvent::UnreadCounts { .. })
        ));
    }

    #[tokio::test]
    async fn separate_message_with_the_same_date_subject_and_sender_stays_unread() {
        let fx = CopyFixture::new().await;
        let (tx, _rx) = flume::unbounded::<SyncEvent>();

        read_original(&fx, &tx).await;

        assert!(!fx.is_seen(fx.lookalike).await);
    }

    #[tokio::test]
    async fn servers_with_per_folder_flags_leave_the_other_folder_alone() {
        let fx = CopyFixture::new().await;
        let (tx, _rx) = flume::unbounded::<SyncEvent>();

        let target = target(&fx, FlagScope::PerFolder);
        let mirrored = apply_flag_change_locally(&target, Some(true), None, &fx.store, &tx)
            .await
            .expect("apply");

        assert!(mirrored.is_empty());
        assert!(!fx.is_seen(fx.copy).await);
    }

    #[tokio::test]
    async fn starring_a_message_does_not_touch_the_copy_read_state() {
        let fx = CopyFixture::new().await;
        let (tx, _rx) = flume::unbounded::<SyncEvent>();
        fx.store
            .set_flags(fx.copy, Some(true), None)
            .await
            .expect("copy already read elsewhere");

        let target = target(&fx, FlagScope::SharedAcrossLabels);
        apply_flag_change_locally(&target, None, Some(true), &fx.store, &tx)
            .await
            .expect("apply");

        let copy = fx.envelope(fx.copy).await;
        assert!(copy.flagged);
        assert!(
            copy.seen,
            "the stale source row must not mark the copy unread"
        );
    }

    #[tokio::test]
    async fn rejected_change_takes_the_server_flags_onto_the_copy() {
        let fx = CopyFixture::new().await;
        let (tx, _rx) = flume::unbounded::<SyncEvent>();
        let mirrored = read_original(&fx, &tx).await;

        let target = target(&fx, FlagScope::SharedAcrossLabels);
        settle_rejected_flag_change(&target, Ok(Some((false, true))), &mirrored, &fx.store, &tx)
            .await
            .expect("settle");

        let copy = fx.envelope(fx.copy).await;
        assert!(!copy.seen);
        assert!(copy.flagged);
    }

    #[tokio::test]
    async fn rejected_change_restores_the_copy_when_the_message_left_the_folder() {
        let fx = CopyFixture::new().await;
        let (tx, _rx) = flume::unbounded::<SyncEvent>();
        let mirrored = read_original(&fx, &tx).await;

        let target = target(&fx, FlagScope::SharedAcrossLabels);
        settle_rejected_flag_change(&target, Ok(None), &mirrored, &fx.store, &tx)
            .await
            .expect("settle");

        assert!(!fx.is_seen(fx.copy).await);
        assert_eq!(fx.stored_unread(fx.all_mail).await, 2);
    }

    #[tokio::test]
    async fn rejected_change_restores_the_copy_when_the_follow_up_fetch_fails() {
        let fx = CopyFixture::new().await;
        let (tx, _rx) = flume::unbounded::<SyncEvent>();
        let mirrored = read_original(&fx, &tx).await;

        let target = target(&fx, FlagScope::SharedAcrossLabels);
        let fetch_failed = Err(Error::Imap("connection lost".to_string()));
        settle_rejected_flag_change(&target, fetch_failed, &mirrored, &fx.store, &tx)
            .await
            .expect("settle");

        assert!(!fx.is_seen(fx.copy).await);
    }

    #[tokio::test]
    async fn batch_shows_copies_read_before_any_server_write() {
        let fx = CopyFixture::new().await;
        let (tx, rx) = flume::unbounded::<SyncEvent>();
        let mut errors = Vec::new();

        let pending = apply_batch_locally(
            FlagScope::SharedAcrossLabels,
            &fx.account,
            &[fx.original],
            Some(true),
            None,
            &fx.store,
            &tx,
            &mut errors,
        )
        .await
        .expect("apply");

        assert!(errors.is_empty());
        assert_eq!(pending.len(), 1);
        assert_eq!(
            pending[0].mirrored.len(),
            1,
            "kept so a failed write can undo it"
        );
        assert!(fx.is_seen(fx.copy).await);
        assert!(
            rx.try_iter()
                .any(|event| matches!(event, SyncEvent::UnreadCounts { .. }))
        );
    }

    #[tokio::test]
    async fn marking_a_folder_read_marks_its_copies_in_other_folders_read() {
        let fx = CopyFixture::new().await;
        let (tx, _rx) = flume::unbounded::<SyncEvent>();

        apply_folder_read_locally(
            FlagScope::SharedAcrossLabels,
            &fx.account,
            fx.inbox,
            &fx.store,
            &tx,
        )
        .await
        .expect("apply");

        assert!(fx.is_seen(fx.original).await);
        assert!(fx.is_seen(fx.copy).await);
        assert!(!fx.is_seen(fx.lookalike).await);
        assert_eq!(fx.stored_unread(fx.all_mail).await, 1);
    }
}
