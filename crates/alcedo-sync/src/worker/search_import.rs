//! Search every folder on the server and store the messages not yet local.

use super::backfill::{persist_fetched_message_ids, persist_fetched_sizes};
use super::mailbox_sync::{ensure_selected, fetched_to_envelope, partition_imap_deleted};
use super::*;

async fn import_folder_search(
    state: &mut WorkerImapState,
    account: &Account,
    folder: &Folder,
    store: &StoreHandle,
    criteria: &str,
) -> Result<()> {
    ensure_selected(state, &folder.imap_name).await?;
    let uids = with_imap_timeout(
        IMAP_COMMAND_TIMEOUT,
        state.client_mut().uid_search(criteria),
    )
    .await?;
    if uids.is_empty() {
        return Ok(());
    }
    let stored = store.messages_by_uids(folder.id, &uids).await?;
    let have: HashSet<u32> = stored.iter().map(|(_, uid, _)| *uid).collect();
    let missing: Vec<u32> = uids.into_iter().filter(|uid| !have.contains(uid)).collect();
    if missing.is_empty() {
        return Ok(());
    }
    for chunk_set in format_uid_set_chunks(&missing, 250) {
        let fetched = with_fetch_timeout(uid_fetch_envelopes(
            state.client_mut(),
            folder.id,
            &chunk_set,
            None,
        ))
        .await?;
        let deleted_uids: Vec<u32> = fetched
            .iter()
            .filter(|item| item.imap_deleted)
            .map(|item| item.uid)
            .collect();
        let envelopes: Vec<alcedo_core::Envelope> = fetched
            .iter()
            .map(|item| fetched_to_envelope(item, account, folder.id))
            .collect();
        store.upsert_envelopes(&envelopes, None).await?;
        if !deleted_uids.is_empty() {
            store.mark_deleted_by_uids(folder.id, deleted_uids).await?;
        }
        let (live, _) = partition_imap_deleted(fetched);
        if !live.is_empty() {
            persist_fetched_message_ids(store, folder.id, &live).await?;
            persist_fetched_sizes(store, folder.id, &live).await?;
        }
    }
    Ok(())
}

pub(super) async fn import_server_search(
    state: &mut WorkerImapState,
    account: &Account,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
    criteria: &str,
    generation: u64,
    progress: crate::extension::SearchProgress,
) -> Result<()> {
    let cancelled = |state: &WorkerImapState| {
        state.search_import_generation.load(Ordering::Relaxed) != generation
    };
    if state.client.is_none() {
        emit_error(events, None, "This account isn't connected".to_string());
        progress.0.finished().await;
        return Ok(());
    }
    let folders = match store.list_folders(account.id).await {
        Ok(folders) => folders,
        Err(err) => {
            emit_error(events, None, err.to_string());
            if !cancelled(state) {
                progress.0.finished().await;
            }
            return Ok(());
        }
    };
    for folder in folders.into_iter().filter(|folder| folder.selectable) {
        if cancelled(state) {
            return Ok(());
        }
        if let Err(err) = import_folder_search(state, account, &folder, store, criteria).await {
            emit_error(
                events,
                None,
                format!("Couldn't search {}: {err}", folder_display_name(&folder)),
            );
        }
        if cancelled(state) {
            return Ok(());
        }
        progress.0.folder_done().await;
    }
    progress.0.finished().await;
    Ok(())
}
