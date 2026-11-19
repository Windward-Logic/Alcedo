//! Move and copy: plans, UID MOVE, rehoming moved rows and destination UID resolution.
#![allow(clippy::too_many_arguments)]

use super::delete::resolve_dest_uids_for_trash_chunk;
use super::flags::locators_by_folder;
use super::mailbox_sync::{
    UnchangedEmit, emit_folder_envelopes, ensure_selected, persist_folder_unread,
    persist_folder_unread_and_total, sync_mailbox,
};
use super::*;
use alcedo_i18n::{tr, trf, trs};

#[derive(Debug)]
pub(super) enum MovePlan {
    AlreadyInDest,
    Move { dest_imap_name: String },
}

pub(super) fn missing_dest_message(dest: SpecialUse) -> String {
    match dest {
        SpecialUse::Junk => tr!("No Junk folder on this account"),
        SpecialUse::Archive => tr!("No Archive folder on this account"),
        SpecialUse::Inbox => tr!("No Inbox folder on this account"),
        other => trf!(
            "No {folder} folder on this account",
            folder = format!("{other:?}")
        ),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum JunkKeyword {
    Junk,
    NotJunk,
}

pub(super) fn junk_keyword_for_special_move(
    source: &SpecialUse,
    dest: &SpecialUse,
) -> Option<JunkKeyword> {
    match (source, dest) {
        (&SpecialUse::Junk, &SpecialUse::Inbox) => Some(JunkKeyword::NotJunk),
        (source, &SpecialUse::Junk) if *source != SpecialUse::Junk => Some(JunkKeyword::Junk),
        _ => None,
    }
}

pub(super) fn account_learns_junk_keywords(account: &Account) -> bool {
    !account.imap_host.eq_ignore_ascii_case("imap.gmail.com")
}

pub(super) fn plan_message_move(
    folders: &[Folder],
    source: &Folder,
    dest: SpecialUse,
) -> Result<MovePlan> {
    if source.special == dest {
        return Ok(MovePlan::AlreadyInDest);
    }
    folders
        .iter()
        .find(|f| f.selectable && f.special == dest)
        .map(|f| MovePlan::Move {
            dest_imap_name: f.imap_name.clone(),
        })
        .ok_or_else(|| Error::Other(missing_dest_message(dest)))
}

pub(super) fn plan_message_to_folder(
    folders: &[Folder],
    source: &Folder,
    dest_id: FolderId,
    cross_account_msg: &str,
) -> Result<MovePlan> {
    if source.id == dest_id {
        return Ok(MovePlan::AlreadyInDest);
    }
    let dest = folders
        .iter()
        .find(|f| f.id == dest_id)
        .ok_or_else(|| Error::Other(tr!("Folder not found")))?;
    if dest.account_id != source.account_id {
        return Err(Error::Other(cross_account_msg.into()));
    }
    if !dest.selectable {
        return Err(Error::Other(tr!("Folder cannot hold messages")));
    }
    if dest.imap_name == source.imap_name {
        return Ok(MovePlan::AlreadyInDest);
    }
    Ok(MovePlan::Move {
        dest_imap_name: dest.imap_name.clone(),
    })
}

pub(super) fn plan_message_move_to_folder(
    folders: &[Folder],
    source: &Folder,
    dest_id: FolderId,
) -> Result<MovePlan> {
    plan_message_to_folder(
        folders,
        source,
        dest_id,
        trs!("Cannot move to a folder on another account"),
    )
}

pub(super) fn plan_message_copy_to_folder(
    folders: &[Folder],
    source: &Folder,
    dest_id: FolderId,
) -> Result<MovePlan> {
    plan_message_to_folder(
        folders,
        source,
        dest_id,
        trs!("Cannot copy to a folder on another account"),
    )
}

/// `UID MOVE` of a UID missing from the selected mailbox is OK with no
/// action (RFC 6851). Do not hide the local copy unless the UID is gone.
///
/// Always SELECT the source rather than trusting the current selection: the
/// same UID number in Trash is a different message, so a MOVE against the
/// wrong mailbox no-ops and Inbox keeps the row.
pub(super) async fn uid_move_from_selected(
    state: &mut WorkerImapState,
    source_imap_name: &str,
    uid: u32,
    dest_imap_name: &str,
    account: &Account,
    keywords: Option<JunkKeyword>,
) -> Result<()> {
    with_imap_timeout(
        IMAP_COMMAND_TIMEOUT,
        state.client_mut().select_mailbox(source_imap_name),
    )
    .await?;
    if let Some(keyword) = keywords
        && account_learns_junk_keywords(account)
    {
        let junk = matches!(keyword, JunkKeyword::Junk);
        if let Err(err) = state
            .client_mut()
            .uid_store_junk_keywords(&uid.to_string(), junk)
            .await
        {
            warn!(
                error = %err,
                folder = %source_imap_name,
                uid,
                "failed to store junk keywords before move"
            );
        }
    }
    state
        .client_mut()
        .uid_move_or_copy_delete_verified(uid, dest_imap_name)
        .await
}

/// After IMAP MOVE removed UIDs from the source mailbox, update local rows in
/// place when the destination UID can be resolved. Returns tombstoned message
/// ids when rehome failed (same as the old mark-deleted-only path).
pub(super) async fn rehome_moved_uids(
    state: &mut WorkerImapState,
    store: &StoreHandle,
    source_folder_id: FolderId,
    dest_folder: &Folder,
    dest_uidnext_before: Option<u32>,
    gone_uids: &[u32],
) -> Result<(usize, Vec<MessageId>)> {
    if gone_uids.is_empty() {
        return Ok((0, Vec::new()));
    }
    let records = store.messages_by_uids(source_folder_id, gone_uids).await?;
    let resolved = match ensure_selected(state, &dest_folder.imap_name).await {
        Ok(()) => resolve_dest_uids_for_trash_chunk(state, &records, dest_uidnext_before).await,
        Err(err) => Err(err),
    };
    let dest_uids = match resolved {
        Ok(uids) => uids,
        Err(err) => {
            warn!(
                error = %err,
                folder = %dest_folder.imap_name,
                "dest UID resolve failed after MOVE; tombstone source rows"
            );
            store
                .mark_deleted_by_uids(source_folder_id, gone_uids.to_vec())
                .await?;
            let ids = records.into_iter().map(|(id, _, _)| id).collect();
            return Ok((0, ids));
        }
    };
    let now = Utc::now();
    let mut rehomed_uids = HashSet::new();
    let mut rehomed_count = 0usize;
    let mut failed_ids = Vec::new();
    for (id, uid, _) in &records {
        if let Some(dest_uid) = dest_uids.get(id).copied() {
            let rehomed = store
                .rehome_message(*id, dest_folder.id, dest_uid, Some(now))
                .await?;
            if rehomed {
                rehomed_uids.insert(*uid);
                rehomed_count += 1;
            } else {
                failed_ids.push(*id);
            }
        }
    }

    let mark_uids: Vec<u32> = gone_uids
        .iter()
        .copied()
        .filter(|uid| !rehomed_uids.contains(uid))
        .collect();
    let mut tombstone_ids = Vec::new();
    if !mark_uids.is_empty() {
        let rows = store.messages_by_uids(source_folder_id, &mark_uids).await?;
        store
            .mark_deleted_by_uids(source_folder_id, mark_uids)
            .await?;
        tombstone_ids = rows.into_iter().map(|(id, _, _)| id).collect();
    }
    tombstone_ids.extend(failed_ids);
    Ok((rehomed_count, tombstone_ids))
}

pub(super) async fn apply_planned_move(
    state: &mut WorkerImapState,
    account: &Account,
    stored: &alcedo_store::StoredMessage,
    source_name: &str,
    dest_folder: &Folder,
    dest_imap_name: &str,
    keywords: Option<JunkKeyword>,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
) -> Result<bool> {
    let dest_uidnext_before =
        snapshot_mailbox_uidnext(state, dest_imap_name, dest_folder.uidnext).await?;
    if let Err(err) = uid_move_from_selected(
        state,
        source_name,
        stored.uid,
        dest_imap_name,
        account,
        keywords,
    )
    .await
    {
        // Optimistic UI already hid the row. Put the live store list back.
        if let Err(emit_err) = emit_folder_envelopes(store, stored.folder_id, events).await {
            warn!(error = %emit_err, "failed to restore folder list after move");
        }
        return Err(err);
    }
    let (rehomed, tombstone_ids) = rehome_moved_uids(
        state,
        store,
        stored.folder_id,
        dest_folder,
        dest_uidnext_before,
        &[stored.uid],
    )
    .await?;
    persist_folder_unread(account, stored.folder_id, store, events).await?;
    persist_folder_unread(account, dest_folder.id, store, events).await?;
    emit_unread_counts(store, events).await?;
    if rehomed > 0 {
        return Ok(true);
    }
    for id in tombstone_ids {
        let _ = events.send(SyncEvent::EnvelopeRemoved { id });
    }
    Ok(false)
}

pub(super) async fn move_message(
    state: &mut WorkerImapState,
    account: &Account,
    id: MessageId,
    dest: SpecialUse,
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
    let keywords = junk_keyword_for_special_move(&folder.special, &dest);
    match plan_message_move(&folders, folder, dest.clone())? {
        MovePlan::AlreadyInDest => Ok(()),
        MovePlan::Move { dest_imap_name } => {
            let dest_folder = folders
                .iter()
                .find(|f| f.selectable && f.special == dest)
                .ok_or_else(|| Error::Store("destination folder not found".to_string()))?;
            apply_planned_move(
                state,
                account,
                &stored,
                &source_name,
                dest_folder,
                &dest_imap_name,
                keywords,
                store,
                events,
            )
            .await?;
            Ok(())
        }
    }
}

pub(super) async fn move_message_to_folder(
    state: &mut WorkerImapState,
    account: &Account,
    id: MessageId,
    dest: FolderId,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
    sync_dest: bool,
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
    let keywords = folders
        .iter()
        .find(|f| f.id == dest)
        .and_then(|df| junk_keyword_for_special_move(&folder.special, &df.special));
    let dest_folder = folders
        .iter()
        .find(|f| f.id == dest)
        .ok_or_else(|| Error::Store("destination folder not found".to_string()))?;
    match plan_message_move_to_folder(&folders, folder, dest)? {
        MovePlan::AlreadyInDest => Ok(()),
        MovePlan::Move { dest_imap_name } => {
            let rehomed = apply_planned_move(
                state,
                account,
                &stored,
                &source_name,
                dest_folder,
                &dest_imap_name,
                keywords,
                store,
                events,
            )
            .await?;
            if sync_dest && !rehomed {
                sync_destination_folder(state, account, dest, store, events).await;
            }
            Ok(())
        }
    }
}

pub(super) async fn copy_message_to_folder(
    state: &mut WorkerImapState,
    account: &Account,
    id: MessageId,
    dest: FolderId,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
    sync_dest: bool,
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
    match plan_message_copy_to_folder(&folders, folder, dest)? {
        MovePlan::AlreadyInDest => {
            info!(
                id = id.0,
                dest = dest.0,
                "copy_message_to_folder: already in dest, no-op"
            );
            Ok(())
        }
        MovePlan::Move { dest_imap_name } => {
            ensure_selected(state, &source_name).await?;
            info!(
                id = id.0,
                source = %source_name,
                dest = dest.0,
                dest_imap_name = %dest_imap_name,
                uid = stored.uid,
                "copy_message_to_folder: UID COPY"
            );
            state
                .client_mut()
                .uid_copy(stored.uid, &dest_imap_name)
                .await?;
            if sync_dest {
                sync_destination_folder(state, account, dest, store, events).await;
            }
            Ok(())
        }
    }
}

/// Split a MOVE chunk into UIDs the source mailbox no longer has, and UIDs
/// `UID SEARCH` still returns. A no-op MOVE (RFC 6851: missing UID is success)
/// leaves every requested UID in `stuck`.
pub(super) fn uids_left_in_source(
    requested: &[u32],
    still_present: &[u32],
) -> (Vec<u32>, Vec<u32>) {
    let still: HashSet<u32> = still_present.iter().copied().collect();
    let mut gone = Vec::new();
    let mut stuck = Vec::new();
    for uid in requested {
        if still.contains(uid) {
            stuck.push(*uid);
        } else {
            gone.push(*uid);
        }
    }
    (gone, stuck)
}

pub(super) async fn apply_folder_uid_moves(
    state: &mut WorkerImapState,
    account: &Account,
    folder: &Folder,
    folder_id: FolderId,
    uids: Vec<u32>,
    dest_folder: &Folder,
    dest_imap_name: &str,
    keywords: Option<JunkKeyword>,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
) -> Result<bool> {
    if uids.is_empty() {
        return Ok(false);
    }

    let dest_uidnext_before =
        snapshot_mailbox_uidnext(state, dest_imap_name, dest_folder.uidnext).await?;

    let mut unique = uids;
    unique.sort_unstable();
    unique.dedup();
    let chunk_sets = format_uid_set_chunks(&unique, 250);
    let mut succeeded: Vec<u32> = Vec::new();
    let mut errors: Vec<String> = Vec::new();

    // Always SELECT rather than trusting the current selection. A MOVE against
    // the wrong mailbox is a no-op; callers then hid the local rows, and the
    // next folder sync upserted them back (`deleted = 0`).
    if let Err(err) = with_imap_timeout(
        IMAP_COMMAND_TIMEOUT,
        state.client_mut().select_mailbox(&folder.imap_name),
    )
    .await
    {
        if let Err(emit_err) = emit_folder_envelopes(store, folder_id, events).await {
            warn!(error = %emit_err, "failed to restore folder list after move");
        }
        return Err(err);
    }

    for (chunk_set, chunk_uids) in chunk_sets.iter().zip(unique.chunks(250)) {
        if let Some(keyword) = keywords.as_ref()
            && account_learns_junk_keywords(account)
        {
            let junk = matches!(keyword, JunkKeyword::Junk);
            if let Err(err) = state
                .client_mut()
                .uid_store_junk_keywords(chunk_set, junk)
                .await
            {
                warn!(
                    error = %err,
                    folder = %folder.imap_name,
                    uid_set = %chunk_set,
                    "failed to store junk keywords before move"
                );
            }
        }
        let move_err = state
            .client_mut()
            .uid_move_or_copy_delete_set(chunk_set, dest_imap_name)
            .await
            .err();
        let still_present = match state
            .client_mut()
            .uid_search(&format!("UID {chunk_set}"))
            .await
        {
            Ok(uids) => uids,
            Err(err) => {
                if let Some(move_err) = move_err {
                    errors.push(move_err.to_string());
                }
                errors.push(err.to_string());
                break;
            }
        };
        let (gone, stuck) = uids_left_in_source(chunk_uids, &still_present);
        succeeded.extend(gone);
        if !stuck.is_empty() {
            if let Some(move_err) = move_err {
                errors.push(move_err.to_string());
            }
            errors.push(format!(
                "UID MOVE {chunk_set} to {dest_imap_name} left {} message(s) in {}",
                stuck.len(),
                folder.imap_name
            ));
            break;
        }
    }

    if !succeeded.is_empty() {
        let (_, tombstone_ids) = rehome_moved_uids(
            state,
            store,
            folder_id,
            dest_folder,
            dest_uidnext_before,
            &succeeded,
        )
        .await?;
        for id in tombstone_ids.iter().copied() {
            let _ = events.send(SyncEvent::EnvelopeRemoved { id });
        }
        persist_folder_unread(account, folder_id, store, events).await?;
        persist_folder_unread(account, dest_folder.id, store, events).await?;
        let needs_dest_sync = !tombstone_ids.is_empty();
        if !errors.is_empty() {
            if let Err(emit_err) = emit_folder_envelopes(store, folder_id, events).await {
                warn!(error = %emit_err, "failed to restore folder list after move");
            }
            return Err(Error::Imap(errors.join("; ")));
        }
        return Ok(needs_dest_sync);
    }

    if !errors.is_empty() {
        if let Err(emit_err) = emit_folder_envelopes(store, folder_id, events).await {
            warn!(error = %emit_err, "failed to restore folder list after move");
        }
        return Err(Error::Imap(errors.join("; ")));
    }
    Ok(false)
}

pub(super) async fn move_messages_batch(
    state: &mut WorkerImapState,
    account: &Account,
    ids: Vec<MessageId>,
    dest: SpecialUse,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
) -> Result<()> {
    if ids.is_empty() {
        return Ok(());
    }

    let folders = store.list_folders(account.id).await?;
    let mut errors: Vec<String> = Vec::new();
    let by_folder = match locators_by_folder(store, &ids, "move messages").await {
        Ok(map) => map,
        Err(err) => {
            errors.push(err.to_string());
            HashMap::new()
        }
    };
    let mut dests_to_sync: HashSet<FolderId> = HashSet::new();
    let any_folder = !by_folder.is_empty();

    for (folder_id, locators) in by_folder {
        let folder = match folders.iter().find(|f| f.id == folder_id) {
            Some(folder) => folder,
            None => {
                errors.push("folder not found for message".to_string());
                continue;
            }
        };

        let mut uids: Vec<u32> = Vec::new();
        let mut dest_imap_name: Option<String> = None;

        for locator in locators {
            match plan_message_move(&folders, folder, dest.clone()) {
                Ok(MovePlan::AlreadyInDest) => {}
                Ok(MovePlan::Move {
                    dest_imap_name: name,
                }) => {
                    dest_imap_name.get_or_insert(name);
                    uids.push(locator.uid);
                }
                Err(err) => errors.push(err.to_string()),
            }
        }

        let Some(dest_imap_name) = dest_imap_name else {
            continue;
        };

        let keywords = junk_keyword_for_special_move(&folder.special, &dest);
        let dest_folder = match folders.iter().find(|f| f.selectable && f.special == dest) {
            Some(dest_folder) => dest_folder,
            None => {
                errors.push("destination folder not found".to_string());
                continue;
            }
        };
        match apply_folder_uid_moves(
            state,
            account,
            folder,
            folder_id,
            uids,
            dest_folder,
            &dest_imap_name,
            keywords,
            store,
            events,
        )
        .await
        {
            Ok(needs_dest_sync) => {
                if needs_dest_sync {
                    dests_to_sync.insert(dest_folder.id);
                }
            }
            Err(err) => errors.push(err.to_string()),
        }
    }

    for dest_id in dests_to_sync {
        sync_destination_folder(state, account, dest_id, store, events).await;
    }

    if any_folder && let Err(err) = emit_unread_counts(store, events).await {
        errors.push(err.to_string());
    }

    if !errors.is_empty() {
        return Err(Error::Imap(errors.join("; ")));
    }
    Ok(())
}

pub(super) async fn move_messages_to_folder_batch(
    state: &mut WorkerImapState,
    account: &Account,
    ids: Vec<MessageId>,
    dest: FolderId,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
    sync_dest: bool,
) -> Result<()> {
    if ids.is_empty() {
        return Ok(());
    }

    let folders = store.list_folders(account.id).await?;
    let mut errors: Vec<String> = Vec::new();
    let by_folder = match locators_by_folder(store, &ids, "move messages to folder").await {
        Ok(map) => map,
        Err(err) => {
            errors.push(err.to_string());
            HashMap::new()
        }
    };
    let dest_folder = match folders.iter().find(|f| f.id == dest) {
        Some(folder) => folder,
        None => return Err(Error::Store("destination folder not found".to_string())),
    };
    let mut dests_to_sync: HashSet<FolderId> = HashSet::new();
    let any_folder = !by_folder.is_empty();
    let mut needs_dest_sync_flag = false;

    for (folder_id, locators) in by_folder {
        let folder = match folders.iter().find(|f| f.id == folder_id) {
            Some(folder) => folder,
            None => {
                errors.push("folder not found for message".to_string());
                continue;
            }
        };

        let mut uids: Vec<u32> = Vec::new();
        let mut dest_imap_name: Option<String> = None;

        for locator in locators {
            match plan_message_move_to_folder(&folders, folder, dest) {
                Ok(MovePlan::AlreadyInDest) => {}
                Ok(MovePlan::Move {
                    dest_imap_name: name,
                }) => {
                    dest_imap_name.get_or_insert(name);
                    uids.push(locator.uid);
                }
                Err(err) => errors.push(err.to_string()),
            }
        }

        let Some(dest_imap_name) = dest_imap_name else {
            continue;
        };

        let keywords = junk_keyword_for_special_move(&folder.special, &dest_folder.special);
        match apply_folder_uid_moves(
            state,
            account,
            folder,
            folder_id,
            uids,
            dest_folder,
            &dest_imap_name,
            keywords,
            store,
            events,
        )
        .await
        {
            Ok(needs_dest_sync) => {
                if needs_dest_sync {
                    needs_dest_sync_flag = true;
                }
            }
            Err(err) => errors.push(err.to_string()),
        }
    }

    if sync_dest && needs_dest_sync_flag {
        dests_to_sync.insert(dest);
    }

    for dest_id in dests_to_sync {
        sync_destination_folder(state, account, dest_id, store, events).await;
    }

    if any_folder && let Err(err) = emit_unread_counts(store, events).await {
        errors.push(err.to_string());
    }

    if !errors.is_empty() {
        return Err(Error::Imap(errors.join("; ")));
    }
    Ok(())
}

pub(super) async fn sync_destination_folder(
    state: &mut WorkerImapState,
    account: &Account,
    dest_id: FolderId,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
) {
    let Ok(Some(dest_folder)) = store.get_folder(dest_id).await else {
        return;
    };
    if !dest_folder.selectable {
        return;
    }
    debug!(
        dest = dest_id.0,
        folder = %dest_folder.imap_name,
        "sync_destination_folder: syncing"
    );
    emit_status(
        events,
        account,
        trf!(
            "Syncing {folder}…",
            folder = folder_display_name(&dest_folder)
        ),
    );
    if let Err(err) = sync_mailbox(
        state,
        account,
        &dest_folder,
        store,
        events,
        UnchangedEmit::Always,
    )
    .await
    {
        warn!(
            error = %err,
            folder = %dest_folder.imap_name,
            "failed to sync destination folder"
        );
        return;
    }
    debug!(
        dest = dest_id.0,
        folder = %dest_folder.imap_name,
        "sync_destination_folder: done"
    );
    if let Err(err) = emit_unread_counts(store, events).await {
        warn!(error = %err, "failed to emit unread counts after dest sync");
    }
}

/// Live `STATUS (UIDNEXT)` so MOVE dest UIDs can be resolved even when Trash
/// was never SELECTed (store `uidnext` is then NULL and HEADER has nothing).
pub(super) async fn snapshot_mailbox_uidnext(
    state: &mut WorkerImapState,
    mailbox: &str,
    stored: Option<u32>,
) -> Result<Option<u32>> {
    match with_imap_timeout(
        IMAP_COMMAND_TIMEOUT,
        state.client_mut().status_uidnext(mailbox),
    )
    .await
    {
        Ok(Some(uidnext)) => Ok(Some(uidnext)),
        Ok(None) => Ok(stored),
        Err(err) => {
            warn!(error = %err, mailbox, "STATUS UIDNEXT failed");
            Ok(stored)
        }
    }
}

pub(super) fn pick_resolved_uid(header_hits: &[u32], dest_uidnext: Option<u32>) -> Option<u32> {
    match dest_uidnext {
        Some(uidnext) => header_hits
            .iter()
            .copied()
            .filter(|uid| *uid >= uidnext)
            .max(),
        None => header_hits.iter().copied().max(),
    }
}

/// `UID n:*` returns the highest existing UID when nothing is at or above `n`,
/// so a lone hit below UIDNEXT is an old message, not the new one.
pub(super) fn pick_new_range_uid(range_hits: &[u32], uidnext: u32) -> Option<u32> {
    match range_hits {
        [uid] if *uid >= uidnext => Some(*uid),
        _ => None,
    }
}

pub(super) fn uids_at_or_above(hits: &[u32], uidnext: u32) -> Vec<u32> {
    let mut uids: Vec<u32> = hits.iter().copied().filter(|uid| *uid >= uidnext).collect();
    uids.sort_unstable();
    uids.dedup();
    uids
}

pub(super) async fn resolve_moved_uid(
    state: &mut WorkerImapState,
    message_id_hdr: Option<&str>,
    dest_uidnext: Option<u32>,
) -> Result<Option<u32>> {
    if let Some(hdr) = message_id_hdr.filter(|value| !value.trim().is_empty()) {
        let criteria = imap_header_message_id_search(hdr);
        let uids = state.client_mut().uid_search(&criteria).await?;
        if let Some(uid) = pick_resolved_uid(&uids, dest_uidnext) {
            return Ok(Some(uid));
        }
    }
    if let Some(uidnext) = dest_uidnext {
        let uids = state
            .client_mut()
            .uid_search(&format!("UID {uidnext}:*"))
            .await?;
        if let Some(uid) = pick_new_range_uid(&uids, uidnext) {
            return Ok(Some(uid));
        }
    }
    Ok(None)
}

pub(super) async fn refresh_trash_after_move(
    account: &Account,
    trash: &Folder,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
) -> Result<()> {
    persist_folder_unread_and_total(account, trash.id, store, events).await?;
    select_folder(store, trash.id, list_mode(), events).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worker::test_support::{account_with_imap_host, folder_named};
    use alcedo_core::{AccountId, FolderId, SpecialUse};
    use uuid::Uuid;

    #[test]
    fn pick_resolved_uid_prefers_max_at_or_above_uidnext() {
        assert_eq!(pick_resolved_uid(&[3, 40], Some(10)), Some(40));
        assert_eq!(pick_resolved_uid(&[3], Some(10)), None);
        assert_eq!(pick_resolved_uid(&[3, 9], None), Some(9));
    }

    #[test]
    fn pick_new_range_uid_rejects_star_fallback() {
        assert_eq!(pick_new_range_uid(&[12], 12), Some(12));
        assert_eq!(pick_new_range_uid(&[9], 12), None);
        assert_eq!(pick_new_range_uid(&[], 12), None);
        assert_eq!(pick_new_range_uid(&[12, 13], 12), None);
    }

    #[test]
    fn uids_at_or_above_drops_star_fallback() {
        assert_eq!(uids_at_or_above(&[9], 12), Vec::<u32>::new());
        assert_eq!(uids_at_or_above(&[9, 12, 13, 12], 12), vec![12, 13]);
    }

    #[test]
    fn imap_draft_uid_falls_back_when_header_search_misses() {
        // APPEND + HEADER Message-ID often returns nothing immediately; UID uidnext:*
        // is the same fallback resolve_moved_uid uses after save_imap_draft APPEND.
        assert_eq!(pick_resolved_uid(&[], Some(12)), None);
        assert_eq!(pick_resolved_uid(&[12], Some(12)), Some(12));
        assert_eq!(pick_resolved_uid(&[8, 12], Some(12)), Some(12));
    }

    #[test]
    fn uids_left_in_source_keeps_noop_move_visible() {
        let (gone, stuck) = uids_left_in_source(&[54, 55, 56, 57], &[54, 55, 56, 57]);
        assert!(gone.is_empty());
        assert_eq!(stuck, vec![54, 55, 56, 57]);
    }

    #[test]
    fn uids_left_in_source_drops_only_moved_uids() {
        let (gone, stuck) = uids_left_in_source(&[54, 55, 56], &[55]);
        assert_eq!(gone, vec![54, 56]);
        assert_eq!(stuck, vec![55]);
    }

    #[test]
    fn plan_message_move_already_in_junk() {
        let junk = folder_named(2, SpecialUse::Junk, "Junk", true);
        let folders = vec![
            folder_named(1, SpecialUse::Inbox, "INBOX", true),
            junk.clone(),
        ];
        assert!(matches!(
            plan_message_move(&folders, &junk, SpecialUse::Junk).unwrap(),
            MovePlan::AlreadyInDest
        ));
    }

    #[test]
    fn plan_message_move_inbox_to_junk() {
        let inbox = folder_named(1, SpecialUse::Inbox, "INBOX", true);
        let folders = vec![
            inbox.clone(),
            folder_named(2, SpecialUse::Junk, "Junk", true),
        ];
        assert!(matches!(
            plan_message_move(&folders, &inbox, SpecialUse::Junk).unwrap(),
            MovePlan::Move {
                dest_imap_name
            } if dest_imap_name == "Junk"
        ));
    }

    #[test]
    fn plan_message_move_missing_junk_errors() {
        let inbox = folder_named(1, SpecialUse::Inbox, "INBOX", true);
        let folders = vec![inbox.clone()];
        let err = plan_message_move(&folders, &inbox, SpecialUse::Junk).unwrap_err();
        assert_eq!(err.to_string(), "No Junk folder on this account");
    }

    #[test]
    fn plan_message_move_missing_archive_errors() {
        let inbox = folder_named(1, SpecialUse::Inbox, "INBOX", true);
        let folders = vec![inbox.clone()];
        let err = plan_message_move(&folders, &inbox, SpecialUse::Archive).unwrap_err();
        assert_eq!(err.to_string(), "No Archive folder on this account");
    }

    #[test]
    fn plan_message_move_skips_non_selectable_junk() {
        let inbox = folder_named(1, SpecialUse::Inbox, "INBOX", true);
        let folders = vec![
            inbox.clone(),
            folder_named(2, SpecialUse::Junk, "Junk", false),
            folder_named(3, SpecialUse::Junk, "[Gmail]/Spam", true),
        ];
        assert!(matches!(
            plan_message_move(&folders, &inbox, SpecialUse::Junk).unwrap(),
            MovePlan::Move {
                dest_imap_name
            } if dest_imap_name == "[Gmail]/Spam"
        ));
    }

    #[test]
    fn plan_message_move_junk_to_inbox() {
        let junk = folder_named(2, SpecialUse::Junk, "Junk", true);
        let folders = vec![
            folder_named(1, SpecialUse::Inbox, "INBOX", true),
            junk.clone(),
        ];
        assert!(matches!(
            plan_message_move(&folders, &junk, SpecialUse::Inbox).unwrap(),
            MovePlan::Move {
                dest_imap_name
            } if dest_imap_name == "INBOX"
        ));
    }

    #[test]
    fn junk_keyword_for_special_move_junk_to_inbox_returns_not_junk() {
        assert!(matches!(
            junk_keyword_for_special_move(&SpecialUse::Junk, &SpecialUse::Inbox),
            Some(JunkKeyword::NotJunk)
        ));
    }

    #[test]
    fn junk_keyword_for_special_move_inbox_to_junk_returns_junk() {
        assert!(matches!(
            junk_keyword_for_special_move(&SpecialUse::Inbox, &SpecialUse::Junk),
            Some(JunkKeyword::Junk)
        ));
    }

    #[test]
    fn junk_keyword_for_special_move_junk_to_junk_and_inbox_to_inbox_return_none() {
        assert_eq!(
            junk_keyword_for_special_move(&SpecialUse::Junk, &SpecialUse::Junk),
            None
        );
        assert_eq!(
            junk_keyword_for_special_move(&SpecialUse::Inbox, &SpecialUse::Inbox),
            None
        );
    }

    #[test]
    fn junk_keyword_for_special_move_inbox_to_archive_and_junk_to_trash_return_none() {
        assert_eq!(
            junk_keyword_for_special_move(&SpecialUse::Inbox, &SpecialUse::Archive),
            None
        );
        assert_eq!(
            junk_keyword_for_special_move(&SpecialUse::Junk, &SpecialUse::Trash),
            None
        );
    }

    #[test]
    fn account_learns_junk_keywords_skips_gmail() {
        assert!(!account_learns_junk_keywords(&account_with_imap_host(
            "imap.gmail.com"
        )));
        assert!(!account_learns_junk_keywords(&account_with_imap_host(
            "IMAP.GMAIL.COM"
        )));
    }

    #[test]
    fn account_learns_junk_keywords_for_other_hosts() {
        assert!(account_learns_junk_keywords(&account_with_imap_host(
            "imap.example.com"
        )));
    }

    #[test]
    fn plan_message_move_to_folder_already_in_dest() {
        let inbox = folder_named(1, SpecialUse::Inbox, "INBOX", true);
        let folders = vec![inbox.clone()];
        assert!(matches!(
            plan_message_move_to_folder(&folders, &inbox, FolderId(1)).unwrap(),
            MovePlan::AlreadyInDest
        ));
    }

    #[test]
    fn plan_message_move_to_folder_same_imap_name_is_already_in_dest() {
        let inbox = folder_named(1, SpecialUse::Inbox, "INBOX", true);
        let alias = folder_named(4, SpecialUse::Inbox, "INBOX", true);
        let folders = vec![inbox.clone(), alias];
        assert!(matches!(
            plan_message_move_to_folder(&folders, &inbox, FolderId(4)).unwrap(),
            MovePlan::AlreadyInDest
        ));
    }

    #[test]
    fn plan_message_move_to_folder_inbox_to_projects() {
        let inbox = folder_named(1, SpecialUse::Inbox, "INBOX", true);
        let projects = folder_named(3, SpecialUse::Other, "Projects", true);
        let folders = vec![inbox.clone(), projects];
        assert!(matches!(
            plan_message_move_to_folder(&folders, &inbox, FolderId(3)).unwrap(),
            MovePlan::Move {
                dest_imap_name
            } if dest_imap_name == "Projects"
        ));
    }

    #[test]
    fn plan_message_move_to_folder_missing_dest() {
        let inbox = folder_named(1, SpecialUse::Inbox, "INBOX", true);
        let folders = vec![inbox.clone()];
        let err = plan_message_move_to_folder(&folders, &inbox, FolderId(99)).unwrap_err();
        assert_eq!(err.to_string(), "Folder not found");
    }

    #[test]
    fn plan_message_move_to_folder_other_account() {
        let inbox = folder_named(1, SpecialUse::Inbox, "INBOX", true);
        let mut other = folder_named(2, SpecialUse::Other, "Other", true);
        other.account_id = AccountId(Uuid::from_u128(2));
        let folders = vec![inbox.clone(), other];
        let err = plan_message_move_to_folder(&folders, &inbox, FolderId(2)).unwrap_err();
        assert_eq!(
            err.to_string(),
            "Cannot move to a folder on another account"
        );
    }

    #[test]
    fn plan_message_move_to_folder_non_selectable_dest() {
        let inbox = folder_named(1, SpecialUse::Inbox, "INBOX", true);
        let inert = folder_named(2, SpecialUse::Other, "[Gmail]", false);
        let folders = vec![inbox.clone(), inert];
        let err = plan_message_move_to_folder(&folders, &inbox, FolderId(2)).unwrap_err();
        assert_eq!(err.to_string(), "Folder cannot hold messages");
    }

    #[test]
    fn plan_message_copy_to_folder_shares_already_in_dest() {
        let inbox = folder_named(1, SpecialUse::Inbox, "INBOX", true);
        let folders = vec![inbox.clone()];
        assert!(matches!(
            plan_message_copy_to_folder(&folders, &inbox, FolderId(1)).unwrap(),
            MovePlan::AlreadyInDest
        ));
    }
}
