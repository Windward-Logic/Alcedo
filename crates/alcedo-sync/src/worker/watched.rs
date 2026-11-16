//! Watched (non-Inbox) folders: STATUS polling, background sync, paging and body prefetch.

use super::backfill::{
    backfill_missing_reply_headers, backfill_missing_rfc822_sizes,
    backfill_missing_routing_headers, persist_fetched_list_unsubscribe,
    persist_fetched_message_ids, persist_fetched_sizes,
};
use super::bodies::{OpenMode, fetch_message_remainder, open_message};
use super::mailbox_sync::{
    emit_account_folders, emit_folder_envelopes, ensure_inbox_selected, ensure_selected,
    fetched_to_envelope, folder_is_syncable, hide_imap_deleted_uids, imap_seq_set,
    older_envelope_seq_range, partition_imap_deleted, persist_folder_unread, sync_selected_mailbox,
};
use super::reactions::peek_reaction_bodies;
use super::sent_copy::maybe_dedupe_message_ids;
use super::session::is_recoverable_disconnect;
use super::*;

pub(super) fn watched_poll_delay(watched: &WatchedFolders) -> Duration {
    let remaining = match watched.last_poll {
        Some(at) => POLL_INTERVAL.saturating_sub(at.elapsed()),
        None => POLL_INTERVAL,
    };
    remaining.max(Duration::from_secs(1))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum WatchedStatusAction {
    Unchanged,
    Badge { unseen: u32 },
    Sync,
}

pub(super) fn watched_special_rank(special: &SpecialUse) -> Option<u8> {
    match special {
        SpecialUse::Sent => Some(0),
        SpecialUse::Drafts => Some(1),
        SpecialUse::Junk => Some(2),
        SpecialUse::Trash => Some(3),
        SpecialUse::Archive => Some(4),
        SpecialUse::Snoozed => Some(5),
        _ => None,
    }
}

pub(super) fn watched_folder_excluded(folder: &Folder) -> bool {
    !folder.selectable
        || matches!(
            folder.special,
            SpecialUse::Inbox | SpecialUse::Outbox | SpecialUse::All
        )
}

pub(super) fn watched_folder_ids(folders: &[Folder]) -> Vec<FolderId> {
    let mut ids = Vec::new();
    let mut seen = HashSet::new();

    let mut specials: Vec<&Folder> = folders
        .iter()
        .filter(|folder| folder.selectable && watched_special_rank(&folder.special).is_some())
        .collect();
    specials.sort_by_key(|folder| watched_special_rank(&folder.special).unwrap_or(u8::MAX));
    for folder in specials {
        if ids.len() >= WATCHED_LIST_CAP {
            break;
        }
        if seen.insert(folder.id) {
            ids.push(folder.id);
        }
    }

    let mut others: Vec<&Folder> = folders
        .iter()
        .filter(|folder| !seen.contains(&folder.id) && !watched_folder_excluded(folder))
        .collect();
    others.sort_by(|a, b| a.imap_name.cmp(&b.imap_name));
    for folder in others {
        if ids.len() >= WATCHED_LIST_CAP {
            break;
        }
        if seen.insert(folder.id) {
            ids.push(folder.id);
        }
    }

    ids
}

pub(super) fn watched_status_batch(
    ids: &[FolderId],
    cursor: usize,
    batch: usize,
) -> (Vec<FolderId>, usize) {
    if ids.is_empty() {
        return (Vec::new(), 0);
    }
    let start = cursor % ids.len();
    let taken = batch.min(ids.len());
    let mut out = Vec::with_capacity(taken);
    for offset in 0..taken {
        out.push(ids[(start + offset) % ids.len()]);
    }
    let next = (start + taken) % ids.len();
    (out, next)
}

pub(super) fn watched_status_action(
    stored: &Folder,
    status: &MailboxStatus,
    condstore: bool,
) -> WatchedStatusAction {
    if stored.uidvalidity.is_none() {
        return match status.unseen {
            Some(unseen) if unseen != stored.unread => WatchedStatusAction::Badge { unseen },
            _ => WatchedStatusAction::Unchanged,
        };
    }
    if status.uidvalidity.is_some() && stored.uidvalidity != status.uidvalidity {
        return WatchedStatusAction::Sync;
    }
    if status.uidnext.is_some() && stored.uidnext != status.uidnext {
        return WatchedStatusAction::Sync;
    }
    if stored.total != status.messages {
        return WatchedStatusAction::Sync;
    }
    if condstore
        && stored.highest_modseq.is_some()
        && status.highest_modseq.is_some()
        && stored.highest_modseq != status.highest_modseq
    {
        return WatchedStatusAction::Sync;
    }
    // STATUS UNSEEN often stays high after every message we store is read
    // (Gmail until SELECT, snoozed mail, anything outside the local set).
    // Painting it makes the mailbox badge lie until the folder is opened.
    // The badge is the local unread count. New mail still syncs via
    // UIDNEXT, EXISTS, or MODSEQ.
    WatchedStatusAction::Unchanged
}

pub(super) async fn fetch_watched_status(
    client: &mut ImapClient,
    imap_name: &str,
    condstore: bool,
) -> Result<Option<MailboxStatus>> {
    match with_imap_timeout(
        IMAP_COMMAND_TIMEOUT,
        client.status_mailbox(imap_name, condstore),
    )
    .await
    {
        Ok(status) => Ok(Some(status)),
        Err(err) if is_recoverable_disconnect(&err) || is_auth_failure(&err) => Err(err),
        Err(_err) if condstore => {
            match with_imap_timeout(
                IMAP_COMMAND_TIMEOUT,
                client.status_mailbox(imap_name, false),
            )
            .await
            {
                Ok(status) => Ok(Some(status)),
                Err(err) if is_recoverable_disconnect(&err) || is_auth_failure(&err) => Err(err),
                Err(err) => {
                    warn!(
                        mailbox = %imap_name,
                        error = %err,
                        "STATUS failed; skipping mailbox"
                    );
                    Ok(None)
                }
            }
        }
        Err(err) => {
            warn!(
                mailbox = %imap_name,
                error = %err,
                "STATUS failed; skipping mailbox"
            );
            Ok(None)
        }
    }
}

pub(super) async fn apply_unseen_badge(
    account: &Account,
    folder: &Folder,
    unseen: u32,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
) -> Result<()> {
    if folder.unread == unseen {
        return Ok(());
    }
    store.set_folder_unread(folder.id, unseen).await?;
    emit_account_folders(account.id, store, events).await
}

pub(super) async fn background_sync_folder(
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
    if folder.account_id != account.id || !folder_is_syncable(&folder) {
        return Ok(());
    }
    let server_rules = ServerRulesSession::default();
    sync_selected_mailbox(state, account, &folder, store, events, &server_rules, false).await?;
    if store.get_folder(folder.id).await?.is_none() {
        return Ok(());
    }
    emit_unread_counts(store, events).await?;
    Ok(())
}

pub(super) async fn poll_watched_folders_batch(
    state: &mut WorkerImapState,
    account: &Account,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
    reselect_inbox: bool,
    batch_size: usize,
) -> Result<()> {
    state.watched.last_poll = Some(Instant::now());
    let folders = store.list_folders(account.id).await?;
    let ids = watched_folder_ids(&folders);
    let (batch, next_cursor) = watched_status_batch(&ids, state.watched.cursor, batch_size);
    state.watched.cursor = next_cursor;
    debug!(
        account = %account.email,
        watched = ids.len(),
        batch = batch.len(),
        cursor = state.watched.cursor,
        "watched folder poll"
    );

    let condstore = state.client_mut().capabilities().condstore;
    let mut sync_target: Option<FolderId> = None;
    for id in batch {
        let Some(folder) = folders.iter().find(|folder| folder.id == id).cloned() else {
            continue;
        };
        let status =
            match fetch_watched_status(state.client_mut(), &folder.imap_name, condstore).await {
                Ok(Some(status)) => status,
                Ok(None) => continue,
                Err(err) => return Err(err),
            };
        match watched_status_action(&folder, &status, condstore) {
            WatchedStatusAction::Sync => {
                if sync_target.is_none() {
                    sync_target = Some(id);
                }
            }
            WatchedStatusAction::Badge { unseen } => {
                if let Err(err) = apply_unseen_badge(account, &folder, unseen, store, events).await
                {
                    if is_recoverable_disconnect(&err) || is_auth_failure(&err) {
                        return Err(err);
                    }
                    warn!(
                        folder = folder.id.0,
                        error = %err,
                        "watched folder badge update failed"
                    );
                }
            }
            WatchedStatusAction::Unchanged => {}
        }
    }

    if let Some(id) = sync_target
        && let Err(err) = background_sync_folder(state, account, id, store, events).await
    {
        if is_recoverable_disconnect(&err) || is_auth_failure(&err) {
            return Err(err);
        }
        warn!(
            folder = id.0,
            error = %err,
            "background folder sync failed"
        );
    }

    if reselect_inbox {
        ensure_inbox_selected(state).await?;
    }
    Ok(())
}

pub(super) async fn poll_watched_folders(
    state: &mut WorkerImapState,
    account: &Account,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
    reselect_inbox: bool,
) -> Result<()> {
    poll_watched_folders_batch(
        state,
        account,
        store,
        events,
        reselect_inbox,
        WATCHED_STATUS_BATCH,
    )
    .await
}

pub(super) async fn prime_watched_folders(
    state: &mut WorkerImapState,
    account: &Account,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
    reselect_inbox: bool,
) -> Result<()> {
    poll_watched_folders_batch(state, account, store, events, reselect_inbox, usize::MAX).await
}

pub(super) async fn sync_folder_recent(
    state: &mut WorkerImapState,
    account: &Account,
    folder_id: FolderId,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
    server_rules: &ServerRulesSession,
) -> Result<Option<WorkerCommand>> {
    let folder = match store.get_folder(folder_id).await? {
        Some(folder) => folder,
        None => {
            emit_folder_envelopes(store, folder_id, events).await?;
            return Ok(None);
        }
    };
    if folder.account_id != account.id || !folder_is_syncable(&folder) {
        emit_folder_envelopes(store, folder.id, events).await?;
        return Ok(None);
    }
    emit_status(
        events,
        account,
        format!("Syncing {}…", folder_display_name(&folder)),
    );
    sync_selected_mailbox(state, account, &folder, store, events, server_rules, true).await?;
    emit_unread_counts(store, events).await?;
    let synced = store.get_folder(folder.id).await?.unwrap_or(folder);
    let have = store.folder_live_count(synced.id).await?;
    Ok(backfill_followup(&synced, have))
}

pub(super) fn backfill_followup(folder: &Folder, have: u32) -> Option<WorkerCommand> {
    if older_envelope_seq_range(folder.total, have, LIST_LIMIT).is_some() {
        Some(WorkerCommand::BackfillFolder {
            folder: folder.id,
            have,
        })
    } else {
        None
    }
}

pub(super) fn body_prefetch_after(cmd: &WorkerCommand, inbox: FolderId) -> Option<WorkerCommand> {
    match cmd {
        WorkerCommand::SyncFolder(folder) => {
            Some(WorkerCommand::PrefetchBodies { folder: *folder })
        }
        WorkerCommand::Refresh => Some(WorkerCommand::PrefetchBodies { folder: inbox }),
        _ => None,
    }
}

/// Fetch one older envelope page, then yield so `OpenMessage` can run.
pub(super) async fn backfill_folder_page(
    state: &mut WorkerImapState,
    account: &Account,
    folder_id: FolderId,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
    have: u32,
) -> Result<Option<WorkerCommand>> {
    let folder = match store.get_folder(folder_id).await? {
        Some(folder) => folder,
        None => return Ok(None),
    };
    if folder.account_id != account.id || !folder_is_syncable(&folder) {
        return Ok(None);
    }
    ensure_selected(state, &folder.imap_name).await?;

    let exists = folder.total;
    let have = have.max(store.folder_live_count(folder.id).await?);
    let Some((start, end)) = older_envelope_seq_range(exists, have, LIST_LIMIT) else {
        return Ok(None);
    };

    emit_status(events, account, format!("Loading {}…", folder.imap_name));

    let seq_set = imap_seq_set(start, end);
    debug!(
        mailbox = %folder.imap_name,
        %seq_set,
        have,
        exists,
        "backfill older envelopes"
    );
    let fetched = with_imap_timeout(
        IMAP_COMMAND_TIMEOUT,
        alcedo_imap::fetch_envelopes_by_seq(state.client_mut(), folder.id, &seq_set),
    )
    .await?;
    let (fetched, deleted_uids) = partition_imap_deleted(fetched);
    if let Err(err) = hide_imap_deleted_uids(store, folder.id, &deleted_uids, events).await {
        warn!(error = %err, "failed to hide IMAP \\Deleted messages");
    }
    if fetched.is_empty() {
        maybe_dedupe_message_ids(state.client_mut(), &folder, store).await;
        persist_folder_unread(account, folder.id, store, events).await?;
        return Ok(None);
    }
    let envelopes: Vec<alcedo_core::Envelope> = fetched
        .iter()
        .map(|fetched| fetched_to_envelope(fetched, account, folder.id))
        .collect();
    let reaction_uids: Vec<u32> = fetched
        .iter()
        .filter(|item| item.reaction_candidate)
        .map(|item| item.uid)
        .collect();
    let existing_reaction_uids = if reaction_uids.is_empty() {
        HashSet::new()
    } else {
        store
            .messages_by_uids(folder.id, &reaction_uids)
            .await?
            .into_iter()
            .map(|(_, uid, _)| uid)
            .collect()
    };
    store.upsert_envelopes(&envelopes, None).await?;
    persist_fetched_list_unsubscribe(store, folder.id, &fetched).await?;
    persist_fetched_message_ids(store, folder.id, &fetched).await?;
    persist_fetched_sizes(store, folder.id, &fetched).await?;
    backfill_missing_reply_headers(state.client_mut(), folder.id, store).await?;
    backfill_missing_routing_headers(state.client_mut(), folder.id, store).await?;
    emit_folder_envelopes(store, folder.id, events).await?;
    if let Err(err) = peek_reaction_bodies(
        state.client_mut(),
        account,
        &folder,
        &fetched,
        &existing_reaction_uids,
        store,
        events,
    )
    .await
    {
        warn!(error = %err, "failed to peek reaction bodies");
    }
    backfill_missing_rfc822_sizes(state.client_mut(), folder.id, store).await?;
    let covered = exists.saturating_sub(start).saturating_add(1);
    if covered <= have {
        maybe_dedupe_message_ids(state.client_mut(), &folder, store).await;
        persist_folder_unread(account, folder.id, store, events).await?;
        return Ok(None);
    }

    let have = covered.max(store.folder_live_count(folder.id).await?);
    let followup = backfill_followup(&folder, have);
    if followup.is_none() {
        maybe_dedupe_message_ids(state.client_mut(), &folder, store).await;
        persist_folder_unread(account, folder.id, store, events).await?;
        emit_unread_counts(store, events).await?;
    }
    Ok(followup)
}

pub(super) fn body_prefetch_claims() -> &'static std::sync::Mutex<HashSet<MessageId>> {
    static CLAIMED: std::sync::OnceLock<std::sync::Mutex<HashSet<MessageId>>> =
        std::sync::OnceLock::new();
    CLAIMED.get_or_init(|| std::sync::Mutex::new(HashSet::new()))
}

pub(super) fn claim_body_prefetch(id: MessageId) -> bool {
    body_prefetch_claims()
        .lock()
        .map(|mut set| set.insert(id))
        .unwrap_or(false)
}

pub(super) fn release_body_prefetch(id: MessageId) {
    if let Ok(mut set) = body_prefetch_claims().lock() {
        set.remove(&id);
    }
}

/// Offline cache for the top of a folder, one body per call so clicks preempt.
pub(super) async fn prefetch_folder_bodies(
    state: &mut WorkerImapState,
    account: &Account,
    folder_id: FolderId,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
) -> Result<Option<WorkerCommand>> {
    let Some(folder) = store.get_folder(folder_id).await? else {
        return Ok(None);
    };
    if folder.account_id != account.id
        || !folder_is_syncable(&folder)
        || matches!(
            folder.special,
            SpecialUse::Drafts | SpecialUse::Trash | SpecialUse::Junk
        )
    {
        return Ok(None);
    }
    let missing = store
        .messages_missing_body(folder.id, BODY_PREFETCH_WINDOW)
        .await?;
    let Some(id) = missing.into_iter().find(|id| claim_body_prefetch(*id)) else {
        return Ok(None);
    };
    match open_message(state, account, id, store, events, OpenMode::Background).await {
        Ok(Some(WorkerCommand::FetchMessageRemainder(remainder_id))) => {
            if let Err(err) =
                fetch_message_remainder(state, account, remainder_id, store, events, false).await
            {
                if is_recoverable_disconnect(&err) || is_auth_failure(&err) {
                    release_body_prefetch(id);
                    return Err(err);
                }
                warn!(?remainder_id, %err, "background attachment remainder failed");
            }
        }
        Ok(_) => {}
        Err(err) if is_recoverable_disconnect(&err) || is_auth_failure(&err) => {
            release_body_prefetch(id);
            return Err(err);
        }
        Err(err) => warn!(?id, %err, "background body prefetch failed"),
    }
    Ok(Some(WorkerCommand::PrefetchBodies { folder: folder_id }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alcedo_core::{AccountId, FolderId, MessageId, SpecialUse};
    use std::collections::HashSet;
    use uuid::Uuid;

    fn watch_folder(
        id: i64,
        imap_name: &str,
        special: SpecialUse,
        selectable: bool,
        unread: u32,
    ) -> Folder {
        Folder {
            id: FolderId(id),
            account_id: AccountId(Uuid::nil()),
            imap_name: imap_name.to_string(),
            delimiter: "/".to_string(),
            special,
            selectable,
            uidvalidity: Some(1),
            uidnext: Some(10),
            highest_modseq: Some(7),
            unread,
            total: 3,
        }
    }

    fn mailbox_status(
        messages: u32,
        uidnext: Option<u32>,
        uidvalidity: Option<u32>,
        unseen: Option<u32>,
        highest_modseq: Option<u64>,
    ) -> MailboxStatus {
        MailboxStatus {
            messages,
            uidnext,
            uidvalidity,
            unseen,
            highest_modseq,
        }
    }

    #[test]
    fn watched_folder_ids_orders_specials_then_alphabetical() {
        let sent = watch_folder(3, "Sent", SpecialUse::Sent, true, 0);
        let drafts = watch_folder(5, "Drafts", SpecialUse::Drafts, true, 0);
        let junk = watch_folder(6, "Junk", SpecialUse::Junk, true, 0);
        let trash = watch_folder(7, "Trash", SpecialUse::Trash, true, 0);
        let archive = watch_folder(8, "Archive", SpecialUse::Archive, true, 0);
        let snoozed = watch_folder(9, "Snoozed", SpecialUse::Snoozed, true, 0);
        let recent_other = watch_folder(14, "Recent", SpecialUse::Other, true, 0);
        let unread_zebra = watch_folder(2, "zebra", SpecialUse::Other, true, 2);
        let unread_alpha = watch_folder(4, "alpha", SpecialUse::Other, true, 1);
        let quiet = watch_folder(13, "quiet", SpecialUse::Other, true, 0);
        let folders = vec![
            watch_folder(1, "INBOX", SpecialUse::Inbox, true, 9),
            unread_zebra.clone(),
            sent.clone(),
            unread_alpha.clone(),
            drafts.clone(),
            junk.clone(),
            trash.clone(),
            archive.clone(),
            snoozed.clone(),
            watch_folder(10, "Outbox", SpecialUse::Outbox, true, 4),
            watch_folder(11, "All", SpecialUse::All, true, 4),
            watch_folder(12, "Hidden", SpecialUse::Other, false, 8),
            quiet.clone(),
            recent_other.clone(),
        ];
        assert_eq!(
            watched_folder_ids(&folders),
            vec![
                sent.id,
                drafts.id,
                junk.id,
                trash.id,
                archive.id,
                snoozed.id,
                recent_other.id,
                unread_alpha.id,
                quiet.id,
                unread_zebra.id,
            ]
        );
    }

    #[test]
    fn watched_list_cap_stops_at_sixty_four() {
        let folders: Vec<Folder> = (0..80)
            .map(|n| watch_folder(n, &format!("u{n:02}"), SpecialUse::Other, true, 1))
            .collect();
        let ids = watched_folder_ids(&folders);
        assert_eq!(ids.len(), WATCHED_LIST_CAP);
        assert_eq!(ids, (0..64).map(FolderId).collect::<Vec<_>>());
    }

    #[test]
    fn watched_status_batch_rotates_without_repeating_in_one_tick() {
        let ids: Vec<FolderId> = (1..=5).map(FolderId).collect();
        let (batch, cursor) = watched_status_batch(&ids, 0, 3);
        assert_eq!(batch, vec![FolderId(1), FolderId(2), FolderId(3)]);
        assert_eq!(cursor, 3);
        let (wrapped, cursor) = watched_status_batch(&ids, cursor, 3);
        assert_eq!(wrapped, vec![FolderId(4), FolderId(5), FolderId(1)]);
        assert_eq!(cursor, 1);

        let short: Vec<FolderId> = (1..=2).map(FolderId).collect();
        let (both, cursor) = watched_status_batch(&short, 0, 3);
        assert_eq!(both, vec![FolderId(1), FolderId(2)]);
        assert_eq!(cursor, 0);

        assert_eq!(watched_status_batch(&[], 4, 3), (Vec::new(), 0));
    }

    #[test]
    fn watched_status_action_sync_badge_and_unchanged() {
        let stored = watch_folder(1, "Sent", SpecialUse::Sent, true, 1);
        let same = mailbox_status(3, Some(10), Some(1), Some(1), Some(7));

        let mut never = stored.clone();
        never.uidvalidity = None;
        never.unread = 1;
        assert_eq!(
            watched_status_action(&never, &same, true),
            WatchedStatusAction::Unchanged
        );
        let badge_never = mailbox_status(3, Some(10), Some(1), Some(4), Some(7));
        assert_eq!(
            watched_status_action(&never, &badge_never, true),
            WatchedStatusAction::Badge { unseen: 4 }
        );

        let mismatch_validity = mailbox_status(3, Some(10), Some(2), Some(1), Some(7));
        assert_eq!(
            watched_status_action(&stored, &mismatch_validity, true),
            WatchedStatusAction::Sync
        );

        let mismatch_uidnext = mailbox_status(3, Some(11), Some(1), Some(1), Some(7));
        assert_eq!(
            watched_status_action(&stored, &mismatch_uidnext, true),
            WatchedStatusAction::Sync
        );

        let mut missing_uidnext = stored.clone();
        missing_uidnext.uidnext = None;
        assert_eq!(
            watched_status_action(&missing_uidnext, &same, true),
            WatchedStatusAction::Sync
        );
        let absent_uidnext = mailbox_status(3, None, Some(1), Some(1), Some(7));
        assert_eq!(
            watched_status_action(&stored, &absent_uidnext, true),
            WatchedStatusAction::Unchanged
        );

        let mismatch_total = mailbox_status(4, Some(10), Some(1), Some(1), Some(7));
        assert_eq!(
            watched_status_action(&stored, &mismatch_total, true),
            WatchedStatusAction::Sync
        );

        let mismatch_modseq = mailbox_status(3, Some(10), Some(1), Some(1), Some(8));
        assert_eq!(
            watched_status_action(&stored, &mismatch_modseq, true),
            WatchedStatusAction::Sync
        );
        assert_ne!(
            watched_status_action(&stored, &mismatch_modseq, false),
            WatchedStatusAction::Sync
        );

        let stale_unseen = mailbox_status(3, Some(10), Some(1), Some(4), Some(7));
        assert_eq!(
            watched_status_action(&stored, &stale_unseen, true),
            WatchedStatusAction::Unchanged,
            "a higher STATUS UNSEEN must not replace the local unread count"
        );

        let missing_unseen = mailbox_status(3, Some(10), Some(1), None, Some(7));
        assert_eq!(
            watched_status_action(&stored, &missing_unseen, true),
            WatchedStatusAction::Unchanged
        );
        assert_eq!(
            watched_status_action(&stored, &same, true),
            WatchedStatusAction::Unchanged
        );

        let mut learning = stored.clone();
        learning.highest_modseq = None;
        let first_modseq = mailbox_status(3, Some(10), Some(1), Some(1), Some(9));
        assert_eq!(
            watched_status_action(&learning, &first_modseq, true),
            WatchedStatusAction::Unchanged
        );
    }

    #[test]
    fn watched_status_batch_covers_every_folder_over_rotations() {
        let ids: Vec<FolderId> = (1..=20).map(FolderId).collect();
        let batch_size = 8;
        let mut cursor = 0;
        let mut seen = HashSet::new();
        for _ in 0..3 {
            let (batch, next_cursor) = watched_status_batch(&ids, cursor, batch_size);
            cursor = next_cursor;
            seen.extend(batch);
        }
        assert_eq!(seen.len(), ids.len());
        assert!(ids.iter().all(|id| seen.contains(id)));
    }

    #[test]
    fn watched_poll_delay_respects_last_poll() {
        assert_eq!(
            watched_poll_delay(&WatchedFolders::default()),
            POLL_INTERVAL
        );

        let overdue = WatchedFolders {
            last_poll: Some(Instant::now() - Duration::from_secs(70)),
            ..WatchedFolders::default()
        };
        assert_eq!(watched_poll_delay(&overdue), Duration::from_secs(1));

        let fresh = WatchedFolders {
            last_poll: Some(Instant::now()),
            ..WatchedFolders::default()
        };
        let delay = watched_poll_delay(&fresh);
        assert!(delay > Duration::from_secs(50));
        assert!(delay <= POLL_INTERVAL);
    }

    #[test]
    fn body_prefetch_after_sync_folder_and_refresh() {
        assert!(matches!(
            body_prefetch_after(&WorkerCommand::SyncFolder(FolderId(7)), FolderId(1)),
            Some(WorkerCommand::PrefetchBodies {
                folder: FolderId(7)
            })
        ));
        assert!(matches!(
            body_prefetch_after(&WorkerCommand::Refresh, FolderId(1)),
            Some(WorkerCommand::PrefetchBodies {
                folder: FolderId(1)
            })
        ));
        assert!(
            body_prefetch_after(&WorkerCommand::OpenMessage(MessageId(1)), FolderId(1)).is_none()
        );
    }

    #[test]
    fn claim_body_prefetch_once_until_released() {
        let id = MessageId(987_654);
        assert!(claim_body_prefetch(id));
        assert!(!claim_body_prefetch(id));
        release_body_prefetch(id);
        assert!(claim_body_prefetch(id));
    }
}
