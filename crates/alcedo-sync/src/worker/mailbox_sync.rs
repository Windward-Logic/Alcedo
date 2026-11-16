//! Folder list and mailbox sync: SELECT, sync planning, envelope fetch, purge and folder metadata.

use super::backfill::{
    backfill_missing_reply_headers, backfill_missing_rfc822_sizes,
    backfill_missing_routing_headers, persist_fetched_list_unsubscribe,
    persist_fetched_message_ids, persist_fetched_sizes,
};
use super::reactions::peek_reaction_bodies;
use super::rules::auto_apply_inbox_rules;
use super::sent_copy::{
    apply_pending_sent_attachments, maybe_dedupe_message_ids,
    run_sent_attachment_structure_backfill,
};
use super::session::is_recoverable_disconnect;
use super::*;

pub(super) fn mailbox_select_is_needed(cached: &str, mailbox: &str) -> bool {
    cached != mailbox
}

/// SELECT unless the connection already has `mailbox` selected. `Some` is the
/// fresh SELECT data.
pub(super) async fn select_mailbox_if_needed(
    state: &mut WorkerImapState,
    mailbox: &str,
) -> Result<Option<SelectedMailbox>> {
    if mailbox_select_is_needed(state.selected_on_server(), mailbox) {
        let selected = with_imap_timeout(
            IMAP_COMMAND_TIMEOUT,
            state.client_mut().select_mailbox(mailbox),
        )
        .await?;
        Ok(Some(selected))
    } else {
        Ok(None)
    }
}

pub(super) async fn ensure_selected(state: &mut WorkerImapState, mailbox: &str) -> Result<()> {
    select_mailbox_if_needed(state, mailbox).await?;
    Ok(())
}

/// Whether a sync that finds nothing new still re-sends the folder list to the UI.
///
/// A background catch-up (the 20 s IDLE wake) has nothing to repaint when the
/// mailbox is unchanged. A folder the user just opened does, so it repaints.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum UnchangedEmit {
    Always,
    Skip,
}

/// SELECT + sync `folder`, then apply inbox rules to new mail when asked.
///
/// Which mailbox ends up selected is recorded by the client on the SELECT
/// itself, so a sync that fails after its SELECT cannot leave a stale record.
pub(super) async fn sync_selected_mailbox(
    state: &mut WorkerImapState,
    account: &Account,
    folder: &Folder,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
    server_rules: &ServerRulesSession,
    apply_inbox_rules: bool,
) -> Result<()> {
    let auto_apply =
        sync_mailbox(state, account, folder, store, events, UnchangedEmit::Always).await?;
    if apply_inbox_rules && let Some(data) = auto_apply {
        auto_apply_inbox_rules(
            state,
            account,
            folder,
            &data.envelopes,
            &data.existing_uids,
            store,
            events,
            server_rules,
        )
        .await?;
    }
    Ok(())
}

pub(super) async fn ensure_inbox_selected(state: &mut WorkerImapState) -> Result<()> {
    let inbox_name = state.inbox.imap_name.clone();
    ensure_selected(state, &inbox_name).await
}

pub(super) async fn sync_folders(
    client: &mut ImapClient,
    account: &Account,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
) -> Result<Folder> {
    let listed = client.list_folders().await?;
    let folders: Vec<Folder> = listed
        .into_iter()
        .map(|lf| Folder {
            id: FolderId(0),
            account_id: account.id,
            imap_name: lf.imap_name,
            delimiter: lf.delimiter,
            special: lf.special,
            selectable: lf.selectable,
            uidvalidity: None,
            uidnext: None,
            highest_modseq: None,
            unread: 0,
            total: 0,
        })
        .collect();

    store
        .replace_folders_with_mode(account.id, &folders, Some(FolderReplaceMode::ListSnapshot))
        .await?;
    // Emit the full account folder list from the store (not just this upsert batch).
    let stored = store.list_folders(account.id).await?;
    let _ = events.send(SyncEvent::FoldersUpdated {
        account: account.id,
        folders: stored.clone(),
    });

    stored
        .into_iter()
        .find(|f| matches!(&f.special, SpecialUse::Inbox))
        .ok_or_else(|| Error::Imap("INBOX folder not found".to_string()))
}

/// Envelope data for post-sync Inbox auto-apply (caller runs rules).
pub(super) struct InboxAutoApplyData {
    envelopes: Vec<alcedo_core::Envelope>,
    existing_uids: HashSet<u32>,
}

pub(super) async fn sync_inbox(
    state: &mut WorkerImapState,
    account: &Account,
    inbox: &Folder,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
    server_rules: &ServerRulesSession,
    unchanged: UnchangedEmit,
) -> Result<()> {
    if let Some(data) = sync_mailbox(state, account, inbox, store, events, unchanged).await? {
        auto_apply_inbox_rules(
            state,
            account,
            inbox,
            &data.envelopes,
            &data.existing_uids,
            store,
            events,
            server_rules,
        )
        .await?;
    }
    Ok(())
}

pub(super) async fn sync_mailbox(
    state: &mut WorkerImapState,
    account: &Account,
    folder: &Folder,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
    unchanged: UnchangedEmit,
) -> Result<Option<InboxAutoApplyData>> {
    if !folder_is_syncable(folder) {
        emit_folder_envelopes(store, folder.id, events).await?;
        return Ok(None);
    }

    let fetch_result = {
        let client = state.client_mut();
        let selected = with_imap_timeout(
            IMAP_COMMAND_TIMEOUT,
            client.select_mailbox(&folder.imap_name),
        )
        .await?;
        if uidvalidity_changed(folder, &selected) {
            let removed = store.purge_missing_uids(folder.id, &[]).await?;
            for id in removed {
                let _ = events.send(SyncEvent::EnvelopeRemoved { id });
            }
        }
        let mut plan = plan_mailbox_sync(
            folder,
            &selected,
            LIST_LIMIT,
            client.capabilities().condstore,
        );
        debug!(
            mailbox = %folder.imap_name,
            ?plan,
            exists = selected.exists,
            "mailbox sync plan"
        );

        if matches!(plan, MailboxSyncPlan::Unchanged) {
            purge_if_needed(client, folder, &selected, store, events).await?;
            let live_count = store.folder_live_count(folder.id).await?;
            if live_count >= selected.exists {
                // Dedupe hides rows without sending `EnvelopeRemoved`, so the
                // list must be repainted even on a background catch-up.
                let deduped = maybe_dedupe_message_ids(client, folder, store).await;
                if deduped {
                    let selected = with_imap_timeout(
                        IMAP_COMMAND_TIMEOUT,
                        client.select_mailbox(&folder.imap_name),
                    )
                    .await?;
                    persist_folder_meta(account, folder, &selected, store, events).await?;
                }
                // Without CONDSTORE nothing else notices a message read or
                // starred on another device while the mailbox is unchanged.
                let flags_changed =
                    refresh_window_flags(account, client, folder, &selected, store, events).await?;
                backfill_missing_reply_headers(client, folder.id, store).await?;
                backfill_missing_routing_headers(client, folder.id, store).await?;
                if flags_changed || deduped || unchanged == UnchangedEmit::Always {
                    emit_folder_envelopes(store, folder.id, events).await?;
                }
                backfill_missing_rfc822_sizes(client, folder.id, store).await?;
                return Ok(None);
            }
            plan = MailboxSyncPlan::RecentWindow {
                start_seq: envelope_fetch_start_seq(selected.exists, LIST_LIMIT),
            };
            debug!(
                mailbox = %folder.imap_name,
                live_count,
                exists = selected.exists,
                ?plan,
                "local rows short despite unchanged meta; refetching"
            );
        }

        if matches!(plan, MailboxSyncPlan::Empty) {
            purge_if_needed(client, folder, &selected, store, events).await?;
            persist_folder_meta(account, folder, &selected, store, events).await?;
            maybe_dedupe_message_ids(client, folder, store).await;
            backfill_missing_reply_headers(client, folder.id, store).await?;
            backfill_missing_routing_headers(client, folder.id, store).await?;
            emit_folder_envelopes(store, folder.id, events).await?;
            return Ok(None);
        }

        if let Some(early) = early_list_fetch(folder, plan, &selected) {
            let preview = match early {
                EarlyListFetch::Uid(set) => {
                    with_imap_timeout(
                        IMAP_COMMAND_TIMEOUT,
                        uid_fetch_envelope_meta(client, folder.id, &set),
                    )
                    .await?
                }
                EarlyListFetch::Seq(set) => {
                    with_imap_timeout(
                        IMAP_COMMAND_TIMEOUT,
                        fetch_envelope_meta_by_seq(client, folder.id, &set),
                    )
                    .await?
                }
            };
            // Paint new rows before BODYSTRUCTURE / text peek / size backfill.
            publish_envelope_preview(account, folder, preview, store, events).await?;
        }

        let live_count = store.folder_live_count(folder.id).await?;
        let fetched =
            fetch_envelopes_for_plan(client, folder, &selected, plan, live_count, store, events)
                .await?;
        if !matches!(plan, MailboxSyncPlan::RecentWindow { .. }) {
            // A window fetch already carries flags; other plans only fetch new mail.
            refresh_window_flags(account, client, folder, &selected, store, events).await?;
        }

        debug!(
            mailbox = %folder.imap_name,
            fetched = fetched.len(),
            "mailbox fetch complete"
        );

        purge_if_needed(client, folder, &selected, store, events).await?;

        if uidvalidity_changed(folder, &selected) {
            persist_folder_meta(account, folder, &selected, store, events).await?;
        }

        Ok::<_, Error>((selected, fetched, plan))
    }?;

    let (mut selected, fetched, plan) = fetch_result;
    let (fetched, deleted_uids) = partition_imap_deleted(fetched);
    if let Err(err) = hide_imap_deleted_uids(store, folder.id, &deleted_uids, events).await {
        warn!(error = %err, "failed to hide IMAP \\Deleted messages");
    }

    let mut auto_apply = None;
    let mut existing_reaction_uids = HashSet::new();
    if !fetched.is_empty() {
        let reaction_uids: Vec<u32> = fetched
            .iter()
            .filter(|item| item.reaction_candidate)
            .map(|item| item.uid)
            .collect();
        if !reaction_uids.is_empty() {
            existing_reaction_uids = store
                .messages_by_uids(folder.id, &reaction_uids)
                .await?
                .into_iter()
                .map(|(_, uid, _)| uid)
                .collect();
        }
        let envelopes: Vec<alcedo_core::Envelope> = fetched
            .iter()
            .map(|f| fetched_to_envelope(f, account, folder.id))
            .collect();
        if folder.special == SpecialUse::Inbox && inbox_auto_apply_for_plan(plan) {
            let uids: Vec<u32> = envelopes.iter().map(|e| e.uid).collect();
            let existing_rows = store.messages_by_uids(folder.id, &uids).await?;
            auto_apply = Some(InboxAutoApplyData {
                existing_uids: existing_rows
                    .iter()
                    .map(|(_, uid, _)| *uid)
                    .collect::<HashSet<u32>>(),
                envelopes: envelopes.clone(),
            });
        }
        store.upsert_envelopes(&envelopes, None).await?;
        persist_fetched_list_unsubscribe(store, folder.id, &fetched).await?;
        persist_fetched_message_ids(store, folder.id, &fetched).await?;
        persist_fetched_sizes(store, folder.id, &fetched).await?;
        if maybe_dedupe_message_ids(state.client_mut(), folder, store).await {
            selected = with_imap_timeout(
                IMAP_COMMAND_TIMEOUT,
                state.client_mut().select_mailbox(&folder.imap_name),
            )
            .await?;
        }
    }

    if folder.special == SpecialUse::Sent {
        if let Err(err) = apply_pending_sent_attachments(state, folder, store).await {
            warn!(error = %err, "failed to apply pending sent attachments");
        }
        if let Err(err) = run_sent_attachment_structure_backfill(state, folder, store).await {
            if is_recoverable_disconnect(&err) || is_auth_failure(&err) {
                return Err(err);
            }
            warn!(error = %err, "Sent attachment structure backfill failed");
        }
    }

    persist_folder_meta(account, folder, &selected, store, events).await?;
    backfill_missing_reply_headers(state.client_mut(), folder.id, store).await?;
    backfill_missing_routing_headers(state.client_mut(), folder.id, store).await?;
    emit_folder_envelopes(store, folder.id, events).await?;
    if !fetched.is_empty()
        && let Err(err) = peek_reaction_bodies(
            state.client_mut(),
            account,
            folder,
            &fetched,
            &existing_reaction_uids,
            store,
            events,
        )
        .await
    {
        warn!(error = %err, "failed to peek reaction bodies");
    }
    // Sizes fill the list column later. Measuring older rows here used to
    // hold the new Drafts APPEND off the screen for several round trips.
    backfill_missing_rfc822_sizes(state.client_mut(), folder.id, store).await?;
    Ok(auto_apply)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MailboxSyncPlan {
    Empty,
    Unchanged,
    Incremental { changed_since: u64 },
    NewUids { from_uid: u32 },
    RecentWindow { start_seq: u32 },
}

pub(super) fn inbox_auto_apply_for_plan(plan: MailboxSyncPlan) -> bool {
    matches!(
        plan,
        MailboxSyncPlan::Incremental { .. } | MailboxSyncPlan::NewUids { .. }
    )
}

pub(super) fn uidvalidity_matches(stored: &Folder, selected: &SelectedMailbox) -> bool {
    matches!(
        (stored.uidvalidity, selected.uidvalidity),
        (Some(prev), Some(now)) if prev == now
    )
}

pub(super) fn uidvalidity_changed(stored: &Folder, selected: &SelectedMailbox) -> bool {
    matches!(
        (stored.uidvalidity, selected.uidvalidity),
        (Some(prev), Some(now)) if prev != now
    )
}

pub(super) fn mailbox_identity_unchanged(
    stored: &Folder,
    selected: &SelectedMailbox,
    condstore: bool,
) -> bool {
    uidvalidity_matches(stored, selected)
        && stored.uidnext == selected.uidnext
        && stored.total == selected.exists
        && (!condstore || stored.highest_modseq == selected.highest_modseq)
}

pub(super) fn plan_mailbox_sync(
    stored: &Folder,
    selected: &SelectedMailbox,
    list_limit: u32,
    condstore: bool,
) -> MailboxSyncPlan {
    if selected.exists == 0 {
        if mailbox_identity_unchanged(stored, selected, condstore) {
            return MailboxSyncPlan::Unchanged;
        }
        return MailboxSyncPlan::Empty;
    }
    if mailbox_identity_unchanged(stored, selected, condstore) {
        return MailboxSyncPlan::Unchanged;
    }
    if uidvalidity_matches(stored, selected) {
        if condstore && let Some(changed_since) = stored.highest_modseq {
            return MailboxSyncPlan::Incremental { changed_since };
        }
        if let Some(from_uid) = stored.uidnext {
            return MailboxSyncPlan::NewUids { from_uid };
        }
    }
    MailboxSyncPlan::RecentWindow {
        start_seq: envelope_fetch_start_seq(selected.exists, list_limit),
    }
}

pub(super) fn envelope_fetch_start_seq(exists: u32, limit: u32) -> u32 {
    exists.saturating_sub(limit.saturating_sub(1)).max(1)
}

pub(super) fn new_uid_fetch_start(stored: &Folder, selected: &SelectedMailbox) -> Option<u32> {
    match (stored.uidnext, selected.uidnext) {
        (Some(stored_next), Some(sel_next)) if sel_next > stored_next => Some(stored_next),
        _ => None,
    }
}

/// Where to ask for headers before the slow envelope body fetch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum EarlyListFetch {
    /// UID set such as `"120:*"`.
    Uid(String),
    /// Sequence set such as `"4901:*"`.
    Seq(String),
}

/// Header-only range for a folder that just gained mail.
///
/// Inbox is left out: filter rules have to see a message before it is stored,
/// and the early publish would mark it as already known.
pub(super) fn early_list_fetch(
    folder: &Folder,
    plan: MailboxSyncPlan,
    selected: &SelectedMailbox,
) -> Option<EarlyListFetch> {
    if folder.special == SpecialUse::Inbox {
        return None;
    }
    match plan {
        MailboxSyncPlan::Incremental { .. } => new_uid_fetch_start(folder, selected)
            .map(|from_uid| EarlyListFetch::Uid(format!("{from_uid}:*"))),
        MailboxSyncPlan::NewUids { from_uid } => Some(EarlyListFetch::Uid(format!("{from_uid}:*"))),
        MailboxSyncPlan::RecentWindow { start_seq } => {
            Some(EarlyListFetch::Seq(format!("{start_seq}:*")))
        }
        MailboxSyncPlan::Unchanged | MailboxSyncPlan::Empty => None,
    }
}

/// Store brand-new rows and tell the UI. Known UIDs are skipped so a header
/// fetch cannot blank a snippet that the full fetch has not replaced yet.
pub(super) async fn publish_envelope_preview(
    account: &Account,
    folder: &Folder,
    fetched: Vec<FetchedEnvelope>,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
) -> Result<()> {
    let (fetched, deleted_uids) = partition_imap_deleted(fetched);
    if let Err(err) = hide_imap_deleted_uids(store, folder.id, &deleted_uids, events).await {
        warn!(error = %err, "failed to hide IMAP \\Deleted messages");
    }
    if fetched.is_empty() {
        return Ok(());
    }
    let uids: Vec<u32> = fetched.iter().map(|item| item.uid).collect();
    let known: HashSet<u32> = store
        .messages_by_uids(folder.id, &uids)
        .await?
        .into_iter()
        .map(|(_, uid, _)| uid)
        .collect();
    let fetched: Vec<FetchedEnvelope> = fetched
        .into_iter()
        .filter(|item| !known.contains(&item.uid))
        .collect();
    if fetched.is_empty() {
        return Ok(());
    }
    let envelopes: Vec<alcedo_core::Envelope> = fetched
        .iter()
        .map(|item| fetched_to_envelope(item, account, folder.id))
        .collect();
    store.upsert_envelopes(&envelopes, None).await?;
    persist_fetched_list_unsubscribe(store, folder.id, &fetched).await?;
    persist_fetched_message_ids(store, folder.id, &fetched).await?;
    persist_fetched_sizes(store, folder.id, &fetched).await?;
    emit_folder_envelopes(store, folder.id, events).await
}

pub(super) async fn fetch_envelopes_for_plan(
    client: &mut ImapClient,
    folder: &Folder,
    selected: &SelectedMailbox,
    plan: MailboxSyncPlan,
    live_count: u32,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
) -> Result<Vec<FetchedEnvelope>> {
    // True when CONDSTORE reported changes that were handled without producing
    // an envelope (flag updates on messages we already store).
    let mut server_changes_seen = false;
    let mut fetched = match plan {
        MailboxSyncPlan::Incremental { changed_since } => {
            let (new_envelopes, saw_changes) =
                sync_condstore_changes(client, folder, store, events, changed_since).await?;
            server_changes_seen = saw_changes;
            new_envelopes
        }
        MailboxSyncPlan::NewUids { from_uid } => {
            with_imap_timeout(
                IMAP_COMMAND_TIMEOUT,
                alcedo_imap::uid_fetch_envelopes(client, folder.id, &format!("{from_uid}:*"), None),
            )
            .await?
        }
        MailboxSyncPlan::RecentWindow { start_seq } => {
            with_imap_timeout(
                IMAP_COMMAND_TIMEOUT,
                alcedo_imap::fetch_envelopes_by_seq(client, folder.id, &format!("{start_seq}:*")),
            )
            .await?
        }
        MailboxSyncPlan::Unchanged | MailboxSyncPlan::Empty => unreachable!(),
    };

    if fetched.is_empty() && !server_changes_seen {
        if let Some(from_uid) = new_uid_fetch_start(folder, selected) {
            debug!(
                mailbox = %folder.imap_name,
                from_uid,
                "CONDSTORE fetch empty; trying UID range"
            );
            fetched = with_imap_timeout(
                IMAP_COMMAND_TIMEOUT,
                alcedo_imap::uid_fetch_envelopes(client, folder.id, &format!("{from_uid}:*"), None),
            )
            .await?;
        } else if live_count < selected.exists {
            let start = envelope_fetch_start_seq(selected.exists, LIST_LIMIT);
            debug!(
                mailbox = %folder.imap_name,
                start,
                live_count,
                exists = selected.exists,
                "fetch empty; trying recent sequence window"
            );
            fetched = with_imap_timeout(
                IMAP_COMMAND_TIMEOUT,
                alcedo_imap::fetch_envelopes_by_seq(client, folder.id, &format!("{start}:*")),
            )
            .await?;
        }
    }

    Ok(fetched)
}

/// CONDSTORE incremental sync: flag changes are applied straight to the rows we
/// already store, and only new mail gets the full envelope fetch (ENVELOPE,
/// BODYSTRUCTURE and a 32 KiB text peek; see [`condstore_uids_to_fetch`]).
/// Marking thousands of messages read on another device no longer downloads
/// any of them.
///
/// Returns the new envelopes and whether the server reported any change.
async fn sync_condstore_changes(
    client: &mut ImapClient,
    folder: &Folder,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
    changed_since: u64,
) -> Result<(Vec<FetchedEnvelope>, bool)> {
    let flags = with_imap_timeout(
        IMAP_COMMAND_TIMEOUT,
        fetch_server_flags(client, "1:*", true, Some(changed_since)),
    )
    .await?;
    if flags.is_empty() {
        return Ok((Vec::new(), false));
    }
    let uids: Vec<u32> = flags.iter().map(|item| item.uid).collect();
    let stored: HashSet<u32> = store
        .messages_by_uids(folder.id, &uids)
        .await?
        .into_iter()
        .map(|(_, uid, _)| uid)
        .collect();
    let (known, unknown): (Vec<ServerFlags>, Vec<ServerFlags>) = flags
        .into_iter()
        .partition(|item| stored.contains(&item.uid));
    apply_flag_updates(folder, known, store, events).await?;

    let new_uids = condstore_uids_to_fetch(&unknown, folder.uidnext);
    let mut envelopes = Vec::new();
    for uid_set in format_uid_set_chunks(&new_uids, 250) {
        envelopes.extend(
            with_imap_timeout(
                IMAP_COMMAND_TIMEOUT,
                uid_fetch_envelopes(client, folder.id, &uid_set, None),
            )
            .await?,
        );
    }
    Ok((envelopes, true))
}

/// Changed UIDs with no live local row that still deserve a full fetch: new
/// mail (at or above the UIDNEXT recorded by the last sync) not already
/// `\Deleted`.
///
/// An older UID without a live row is outside the synced window, or hidden
/// locally (deleted, or waiting for a queued delete or move). Fetching it would
/// insert old mail, bring a hidden row back (`upsert_envelopes` clears
/// `deleted`), and run inbox rules on it as if it had just arrived, all
/// because its flags changed on another device. A folder with no recorded
/// UIDNEXT keeps the old behaviour and fetches every unknown UID.
/// O(n) in the changed set.
pub(super) fn condstore_uids_to_fetch(
    unknown: &[ServerFlags],
    stored_uidnext: Option<u32>,
) -> Vec<u32> {
    unknown
        .iter()
        .filter(|item| !item.deleted)
        .filter(|item| stored_uidnext.is_none_or(|next| item.uid >= next))
        .map(|item| item.uid)
        .collect()
}

/// Write server flags onto stored rows and hide `\Deleted` ones. True when
/// anything visible changed.
async fn apply_flag_updates(
    folder: &Folder,
    flags: Vec<ServerFlags>,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
) -> Result<bool> {
    let (deleted, live): (Vec<ServerFlags>, Vec<ServerFlags>) =
        flags.into_iter().partition(|item| item.deleted);
    let deleted_uids: Vec<u32> = deleted.iter().map(|item| item.uid).collect();
    let hidden = hide_imap_deleted_uids(store, folder.id, &deleted_uids, events).await?;
    let changed = store.apply_server_flags(folder.id, live).await?;
    Ok(hidden || !changed.is_empty())
}

/// Servers without CONDSTORE cannot say what changed, so re-read the flags of
/// the newest page and apply the differences. One small FETCH of the page that
/// is on screen, only when the mailbox identity looks unchanged or only new
/// mail arrived. True when a flag or `\Deleted` state changed.
async fn refresh_window_flags(
    account: &Account,
    client: &mut ImapClient,
    folder: &Folder,
    selected: &SelectedMailbox,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
) -> Result<bool> {
    if client.capabilities().condstore || selected.exists == 0 {
        return Ok(false);
    }
    let set = format!(
        "{}:*",
        envelope_fetch_start_seq(selected.exists, LIST_LIMIT)
    );
    let flags = match with_imap_timeout(
        IMAP_COMMAND_TIMEOUT,
        fetch_server_flags(client, &set, false, None),
    )
    .await
    {
        Ok(flags) => flags,
        Err(err) if is_recoverable_disconnect(&err) => return Err(err),
        Err(err) => {
            warn!(mailbox = %folder.imap_name, error = %err, "flag refresh failed; skipping");
            return Ok(false);
        }
    };
    let changed = apply_flag_updates(folder, flags, store, events).await?;
    if changed {
        persist_folder_unread(account, folder.id, store, events).await?;
    }
    Ok(changed)
}

/// Next older IMAP sequence window below the newest `have` messages.
///
/// Sequence 1 is oldest; `exists` is newest. After a `RecentWindow` of 100 in a
/// 2630-message mailbox, `have` is 100 and this returns `2431:2530`.
pub(super) fn older_envelope_seq_range(exists: u32, have: u32, limit: u32) -> Option<(u32, u32)> {
    if have >= exists || limit == 0 {
        return None;
    }
    let end = exists - have;
    let start = end.saturating_sub(limit.saturating_sub(1)).max(1);
    Some((start, end))
}

pub(super) fn imap_seq_set(start: u32, end: u32) -> String {
    if start == end {
        start.to_string()
    } else {
        format!("{start}:{end}")
    }
}

/// True when messages may have been expunged since the last sync, so a UID
/// SEARCH of the whole mailbox is needed to find the local rows to drop.
///
/// New mail alone raises UIDNEXT, and used to force that O(mailbox) search on
/// every arrival. Pure growth is recognised exactly: every UID in
/// `stored.uidnext..selected.uidnext` was issued to a message, so EXISTS equals
/// the old total plus that gap only when nothing old or new was expunged.
/// That holds only while `stored.total` is never below the server's count for
/// `stored.uidnext`; local updates go through [`total_after_local_change`].
pub(super) fn mailbox_needs_purge(stored: &Folder, selected: &SelectedMailbox) -> bool {
    uidvalidity_matches(stored, selected)
        && (selected.exists < stored.total
            || (stored.uidnext != selected.uidnext && !is_pure_growth(stored, selected)))
}

fn is_pure_growth(stored: &Folder, selected: &SelectedMailbox) -> bool {
    let (Some(stored_next), Some(selected_next)) = (stored.uidnext, selected.uidnext) else {
        return false;
    };
    selected_next > stored_next
        && u64::from(selected.exists)
            == u64::from(stored.total) + u64::from(selected_next - stored_next)
}

/// Local live rows can fall behind IMAP EXISTS when a no-op MOVE still
/// soft-deleted the source copy. Reconcile whenever the visible page is short.
pub(super) fn mailbox_needs_uid_reconcile(
    stored: &Folder,
    selected: &SelectedMailbox,
    live_count: u32,
    list_limit: u32,
) -> bool {
    mailbox_needs_purge(stored, selected) || live_count < selected.exists.min(list_limit)
}

pub(super) async fn purge_if_needed(
    client: &mut ImapClient,
    folder: &Folder,
    selected: &SelectedMailbox,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
) -> Result<()> {
    let live_count = store.folder_live_count(folder.id).await?;
    let needs_reconcile = mailbox_needs_uid_reconcile(folder, selected, live_count, LIST_LIMIT);
    debug!(
        folder = folder.id.0,
        imap_name = %folder.imap_name,
        needs_reconcile,
        needs_purge = mailbox_needs_purge(folder, selected),
        live_count,
        stored_total = folder.total,
        stored_uidnext = ?folder.uidnext,
        stored_uidvalidity = ?folder.uidvalidity,
        selected_exists = selected.exists,
        selected_uidnext = ?selected.uidnext,
        selected_uidvalidity = ?selected.uidvalidity,
        list_limit = LIST_LIMIT,
        "purge_if_needed: reconcile check"
    );
    if !needs_reconcile {
        return Ok(());
    }

    let live_uids = if selected.exists == 0 {
        Vec::new()
    } else {
        // UNDELETED omits messages flagged `\Deleted` but not yet expunged.
        // Betterbird leaves the previous draft that way when it saves a new one.
        match with_imap_timeout(IMAP_COMMAND_TIMEOUT, client.uid_search("UNDELETED")).await {
            Ok(uids) => uids,
            Err(err) if is_recoverable_disconnect(&err) => return Err(err),
            Err(err) => {
                warn!(error = %err, "UID SEARCH ALL failed; skipping purge");
                return Ok(());
            }
        }
    };
    debug!(
        folder = folder.id.0,
        imap_name = %folder.imap_name,
        live_uids = ?live_uids,
        "purge_if_needed: live UID SEARCH ALL result"
    );

    let removed = store.purge_missing_uids(folder.id, &live_uids).await?;
    if !removed.is_empty() {
        debug!(
            folder = folder.id.0,
            imap_name = %folder.imap_name,
            removed = ?removed.iter().map(|id| id.0).collect::<Vec<_>>(),
            "purge_if_needed: removed rows not in live UID set"
        );
    }
    for id in removed {
        let _ = events.send(SyncEvent::EnvelopeRemoved { id });
    }
    Ok(())
}

pub(super) async fn persist_folder_meta(
    account: &Account,
    folder: &Folder,
    selected: &SelectedMailbox,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
) -> Result<()> {
    let unread = store.folder_unread_count(folder.id).await?;
    let folder_meta = merge_folder_metadata(folder, selected, unread);
    store
        .replace_folders_with_mode(
            account.id,
            &[folder_meta],
            Some(FolderReplaceMode::SelectRefresh),
        )
        .await?;
    emit_account_folders(account.id, store, events).await
}

pub(super) async fn emit_account_folders(
    account_id: AccountId,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
) -> Result<()> {
    let stored = store.list_folders(account_id).await?;
    let _ = events.send(SyncEvent::FoldersUpdated {
        account: account_id,
        folders: stored,
    });
    Ok(())
}

pub(super) async fn persist_folder_unread(
    account: &Account,
    folder_id: FolderId,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
) -> Result<()> {
    let unread = store.folder_unread_count(folder_id).await?;
    let Some(folder) = store.get_folder(folder_id).await? else {
        return Ok(());
    };
    if folder.unread != unread {
        store
            .replace_folders_with_mode(
                account.id,
                &[Folder { unread, ..folder }],
                Some(FolderReplaceMode::SelectRefresh),
            )
            .await?;
        emit_account_folders(account.id, store, events).await?;
    }
    Ok(())
}

/// Folder total to store after a local move or delete, before the next SELECT.
///
/// `stored_total` is the server's EXISTS from the SELECT that also recorded
/// `uidnext`, and [`is_pure_growth`] relies on that pair: an understated total
/// lets an expunge pass as new mail and leaves ghost rows. The live row count
/// covers only the synced window, so it may raise the total (mail moved in)
/// but never lower it; the next SELECT writes the exact figure.
pub(super) fn total_after_local_change(stored_total: u32, live: u32) -> u32 {
    stored_total.max(live)
}

pub(super) async fn persist_folder_unread_and_total(
    account: &Account,
    folder_id: FolderId,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
) -> Result<()> {
    let unread = store.folder_unread_count(folder_id).await?;
    let live = store.folder_live_count(folder_id).await?;
    let Some(folder) = store.get_folder(folder_id).await? else {
        return Ok(());
    };
    let total = total_after_local_change(folder.total, live);
    if folder.unread != unread || folder.total != total {
        store
            .replace_folders_with_mode(
                account.id,
                &[Folder {
                    unread,
                    total,
                    ..folder
                }],
                Some(FolderReplaceMode::SelectRefresh),
            )
            .await?;
        emit_account_folders(account.id, store, events).await?;
    }
    Ok(())
}

pub(super) async fn emit_folder_envelopes(
    store: &StoreHandle,
    folder_id: FolderId,
    events: &flume::Sender<SyncEvent>,
) -> Result<()> {
    let stored_envelopes = load_folder_envelopes(store, folder_id).await?;
    let _ = events.send(SyncEvent::EnvelopesAppended {
        folder: folder_id,
        envelopes: stored_envelopes,
    });
    // Conversation lists are grouped in SQLite. Re-query here instead of having
    // the UI SelectFolder (that IMAP-syncs again and loops with this event).
    if list_mode() == MessageListMode::Conversation {
        select_folder(store, folder_id, MessageListMode::Conversation, events).await?;
    }
    if let Ok(Some(folder)) = store.get_folder(folder_id).await
        && folder.special == SpecialUse::Sent
    {
        emit_sent_recipients(store, events).await;
    }
    Ok(())
}

pub(super) async fn emit_sent_recipients(store: &StoreHandle, events: &flume::Sender<SyncEvent>) {
    match store.list_sent_recipients().await {
        Ok(addresses) => {
            let _ = events.send(SyncEvent::SentRecipientsChanged { addresses });
        }
        Err(err) => warn!(error = %err, "failed to list sent recipients"),
    }
}

pub(super) fn merge_folder_metadata(
    folder: &Folder,
    selected: &SelectedMailbox,
    unread: u32,
) -> Folder {
    Folder {
        id: folder.id,
        account_id: folder.account_id,
        imap_name: folder.imap_name.clone(),
        delimiter: folder.delimiter.clone(),
        special: folder.special.clone(),
        selectable: folder.selectable,
        uidvalidity: selected.uidvalidity,
        uidnext: selected.uidnext,
        highest_modseq: selected.highest_modseq,
        unread,
        total: selected.exists,
    }
}

/// Live envelopes, and UIDs flagged `\Deleted` that should leave the list.
pub(crate) fn partition_imap_deleted(
    fetched: Vec<FetchedEnvelope>,
) -> (Vec<FetchedEnvelope>, Vec<u32>) {
    let mut deleted_uids = Vec::new();
    let live = fetched
        .into_iter()
        .filter(|env| {
            if env.imap_deleted {
                deleted_uids.push(env.uid);
                false
            } else {
                true
            }
        })
        .collect();
    (live, deleted_uids)
}

pub(super) async fn hide_imap_deleted_uids(
    store: &StoreHandle,
    folder_id: FolderId,
    uids: &[u32],
    events: &flume::Sender<SyncEvent>,
) -> Result<bool> {
    if uids.is_empty() {
        return Ok(false);
    }
    let rows = store.messages_by_uids(folder_id, uids).await?;
    store.mark_deleted_by_uids(folder_id, uids.to_vec()).await?;
    let hidden = !rows.is_empty();
    for (id, _, _) in rows {
        let _ = events.send(SyncEvent::EnvelopeRemoved { id });
    }
    Ok(hidden)
}

pub(super) fn fetched_to_envelope(
    fetched: &FetchedEnvelope,
    account: &Account,
    folder_id: FolderId,
) -> alcedo_core::Envelope {
    alcedo_core::Envelope {
        id: MessageId(0),
        account_id: account.id,
        folder_id,
        uid: fetched.uid,
        thread_id: fetched.thread_id,
        date: fetched.date,
        subject: fetched.subject.clone(),
        from: fetched.from.clone(),
        to: fetched.to.clone(),
        cc: fetched.cc.clone(),
        reply_to: fetched.reply_to.clone(),
        sender: fetched.sender.clone(),
        routing: Some(fetched.routing.clone()),
        snippet: fetched.snippet.clone(),
        seen: fetched.seen,
        flagged: fetched.flagged,
        answered: fetched.answered,
        draft: fetched.draft,
        has_attachments: fetched.has_attachments,
        deleted_at: None,
        account_accent: account.accent_color.clone(),
        account_avatar: account.avatar_text.clone(),
        size_bytes: 0,
    }
}

pub(crate) fn folder_is_syncable(folder: &Folder) -> bool {
    folder.selectable && !matches!(folder.special, SpecialUse::Outbox)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worker::test_support::folder_with;
    use alcedo_core::{AccountId, FolderId, SpecialUse};
    use alcedo_imap::SelectedMailbox;
    use uuid::Uuid;

    fn selected(uidvalidity: Option<u32>, highest_modseq: Option<u64>) -> SelectedMailbox {
        SelectedMailbox {
            uidvalidity,
            uidnext: Some(20),
            highest_modseq,
            exists: 5,
        }
    }

    #[test]
    fn inbox_auto_apply_only_for_incremental_and_new_uids() {
        assert!(inbox_auto_apply_for_plan(MailboxSyncPlan::Incremental {
            changed_since: 40
        }));
        assert!(inbox_auto_apply_for_plan(MailboxSyncPlan::NewUids {
            from_uid: 10
        }));
        assert!(!inbox_auto_apply_for_plan(MailboxSyncPlan::RecentWindow {
            start_seq: 1
        }));
        assert!(!inbox_auto_apply_for_plan(MailboxSyncPlan::Unchanged));
        assert!(!inbox_auto_apply_for_plan(MailboxSyncPlan::Empty));
    }

    #[test]
    fn first_sync_uses_recent_window() {
        let stored = folder_with(None, None);
        let selected = selected(Some(1), Some(99));
        assert_eq!(
            plan_mailbox_sync(&stored, &selected, 100, true),
            MailboxSyncPlan::RecentWindow { start_seq: 1 }
        );
    }

    #[test]
    fn large_mailbox_windows_to_last_page() {
        let stored = folder_with(None, None);
        let selected = SelectedMailbox {
            uidvalidity: Some(1),
            uidnext: Some(20),
            highest_modseq: Some(99),
            exists: 5000,
        };
        assert_eq!(
            plan_mailbox_sync(&stored, &selected, 100, true),
            MailboxSyncPlan::RecentWindow { start_seq: 4901 }
        );
        assert_eq!(envelope_fetch_start_seq(50, 100), 1);
        assert_eq!(envelope_fetch_start_seq(100, 100), 1);
        assert_eq!(envelope_fetch_start_seq(101, 100), 2);
    }

    #[test]
    fn older_window_walks_down_from_recent_page() {
        assert_eq!(older_envelope_seq_range(2630, 100, 100), Some((2431, 2530)));
        assert_eq!(older_envelope_seq_range(2630, 200, 100), Some((2331, 2430)));
        assert_eq!(older_envelope_seq_range(2630, 2600, 100), Some((1, 30)));
        assert_eq!(older_envelope_seq_range(2630, 2630, 100), None);
        assert_eq!(older_envelope_seq_range(50, 0, 100), Some((1, 50)));
        assert_eq!(older_envelope_seq_range(5, 4, 100), Some((1, 1)));
        assert_eq!(imap_seq_set(2431, 2530), "2431:2530");
        assert_eq!(imap_seq_set(1, 1), "1");
    }

    #[test]
    fn unchanged_mailbox_skips_fetch() {
        let stored = Folder {
            uidnext: Some(20),
            total: 5,
            ..folder_with(Some(1), Some(40))
        };
        let selected = selected(Some(1), Some(40));
        assert_eq!(
            plan_mailbox_sync(&stored, &selected, 100, true),
            MailboxSyncPlan::Unchanged
        );
    }

    #[test]
    fn condstore_flag_only_change_uses_changedsince() {
        let stored = Folder {
            uidnext: Some(20),
            total: 5,
            ..folder_with(Some(1), Some(40))
        };
        let selected = SelectedMailbox {
            uidvalidity: Some(1),
            uidnext: Some(20),
            highest_modseq: Some(99),
            exists: 5,
        };
        assert_eq!(
            plan_mailbox_sync(&stored, &selected, 100, true),
            MailboxSyncPlan::Incremental { changed_since: 40 }
        );
        assert_eq!(
            plan_mailbox_sync(&stored, &selected, 100, false),
            MailboxSyncPlan::Unchanged
        );
    }

    #[test]
    fn uidvalidity_changed_detects_new_generation() {
        let stored = folder_with(Some(1), Some(40));
        assert!(uidvalidity_changed(&stored, &selected(Some(2), Some(99))));
        assert!(!uidvalidity_changed(&stored, &selected(Some(1), Some(99))));
        assert!(!uidvalidity_changed(
            &folder_with(None, None),
            &selected(Some(1), Some(99))
        ));
        assert!(!uidvalidity_changed(
            &folder_with(Some(1), Some(40)),
            &selected(None, Some(99))
        ));
    }

    #[test]
    fn condstore_new_mail_uses_changedsince() {
        let stored = Folder {
            uidnext: Some(10),
            total: 5,
            ..folder_with(Some(1), Some(40))
        };
        let selected = SelectedMailbox {
            uidvalidity: Some(1),
            uidnext: Some(20),
            highest_modseq: Some(99),
            exists: 6,
        };
        assert_eq!(
            plan_mailbox_sync(&stored, &selected, 100, true),
            MailboxSyncPlan::Incremental { changed_since: 40 }
        );
        assert_eq!(
            plan_mailbox_sync(&stored, &selected, 100, false),
            MailboxSyncPlan::NewUids { from_uid: 10 }
        );
    }

    #[test]
    fn uidvalidity_change_forces_windowed_refetch() {
        let stored = Folder {
            uidnext: Some(10),
            total: 5,
            ..folder_with(Some(1), Some(40))
        };
        let selected = selected(Some(2), Some(99));
        assert_eq!(
            plan_mailbox_sync(&stored, &selected, 100, true),
            MailboxSyncPlan::RecentWindow { start_seq: 1 }
        );
    }

    #[test]
    fn empty_mailbox_is_empty_plan() {
        let stored = folder_with(Some(1), Some(40));
        let selected = SelectedMailbox {
            uidvalidity: Some(1),
            uidnext: Some(20),
            highest_modseq: Some(99),
            exists: 0,
        };
        assert_eq!(
            plan_mailbox_sync(&stored, &selected, 100, true),
            MailboxSyncPlan::Empty
        );
        assert!(mailbox_needs_purge(
            &Folder { total: 5, ..stored },
            &selected
        ));
        let swapped = Folder {
            uidnext: Some(10),
            total: 5,
            ..folder_with(Some(1), Some(40))
        };
        let swapped_selected = SelectedMailbox {
            uidvalidity: Some(1),
            uidnext: Some(20),
            highest_modseq: Some(99),
            exists: 5,
        };
        assert!(
            mailbox_needs_purge(&swapped, &swapped_selected),
            "uidnext advanced with stable exists still needs a UID SEARCH purge"
        );
    }

    fn stored_envelope(account: &Account, folder_id: FolderId, uid: u32) -> alcedo_core::Envelope {
        alcedo_core::Envelope {
            id: MessageId(0),
            account_id: account.id,
            folder_id,
            uid,
            thread_id: alcedo_core::ThreadId(Uuid::new_v4()),
            date: chrono::Utc::now(),
            subject: format!("message {uid}"),
            from: Vec::new(),
            to: Vec::new(),
            cc: Vec::new(),
            reply_to: Vec::new(),
            sender: Vec::new(),
            routing: None,
            snippet: String::new(),
            seen: false,
            flagged: false,
            answered: false,
            draft: false,
            has_attachments: false,
            deleted_at: None,
            account_accent: String::new(),
            account_avatar: String::new(),
            size_bytes: 0,
        }
    }

    fn flags(uid: u32, seen: bool, deleted: bool) -> ServerFlags {
        ServerFlags {
            uid,
            seen,
            flagged: false,
            answered: false,
            draft: false,
            deleted,
        }
    }

    #[tokio::test]
    async fn flag_updates_apply_to_stored_rows_and_hide_deleted() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = StoreHandle::open_path(&dir.path().join("store.db")).expect("open store");
        let account = crate::worker::test_support::account_with_imap_host("imap.example.com");
        store.upsert_account(&account).await.expect("account");
        let folder = Folder {
            account_id: account.id,
            ..folder_with(Some(1), None)
        };
        let folder = store
            .replace_folders(
                account.id,
                &[Folder {
                    id: FolderId(0),
                    ..folder
                }],
            )
            .await
            .expect("folders")
            .remove(0);
        store
            .upsert_envelopes(
                &[
                    stored_envelope(&account, folder.id, 1),
                    stored_envelope(&account, folder.id, 2),
                    stored_envelope(&account, folder.id, 3),
                ],
                None,
            )
            .await
            .expect("envelopes");
        let (tx, rx) = flume::unbounded::<SyncEvent>();

        let changed = apply_flag_updates(
            &folder,
            vec![
                flags(1, true, false),
                flags(2, false, true),
                flags(3, false, false),
            ],
            &store,
            &tx,
        )
        .await
        .expect("apply");
        assert!(changed);

        let rows = store.folder_messages(folder.id, 10, 0).await.expect("rows");
        assert_eq!(rows.len(), 2, "uid 2 is \\Deleted and leaves the list");
        assert!(rows.iter().find(|row| row.uid == 1).expect("uid 1").seen);
        assert!(!rows.iter().find(|row| row.uid == 3).expect("uid 3").seen);
        assert!(matches!(
            rx.try_recv(),
            Ok(SyncEvent::EnvelopeRemoved { .. })
        ));

        let again = apply_flag_updates(
            &folder,
            vec![flags(1, true, false), flags(2, false, true)],
            &store,
            &tx,
        )
        .await
        .expect("apply again");
        assert!(!again, "an unchanged poll must not report a change");
    }

    #[test]
    fn local_count_after_a_move_cannot_hide_a_later_expunge() {
        // Server: 5 messages up to UID 9, one of them \Deleted and so hidden
        // locally (4 live rows). A move into this folder refreshes the total.
        let stored = Folder {
            uidnext: Some(10),
            total: total_after_local_change(5, 4),
            ..folder_with(Some(1), Some(40))
        };
        assert_eq!(stored.total, 5, "the window count must not lower the total");
        // Another client expunges one message and one new message arrives.
        let selected = SelectedMailbox {
            uidvalidity: Some(1),
            uidnext: Some(11),
            highest_modseq: Some(99),
            exists: 5,
        };
        assert!(
            mailbox_needs_purge(&stored, &selected),
            "an understated total (4) would make this look like pure growth"
        );
        // Mail moved in still raises the total.
        assert_eq!(total_after_local_change(5, 7), 7);
    }

    #[test]
    fn condstore_fetches_only_new_mail_not_old_or_hidden_rows() {
        let unknown = [
            flags(5, true, false),   // old: outside the window or hidden locally
            flags(12, false, false), // new since the last sync
            flags(13, false, true),  // new but already \Deleted
        ];
        assert_eq!(condstore_uids_to_fetch(&unknown, Some(10)), vec![12]);
        assert_eq!(
            condstore_uids_to_fetch(&unknown, None),
            vec![5, 12],
            "no recorded UIDNEXT keeps fetching every live unknown UID"
        );
    }

    #[test]
    fn new_mail_alone_does_not_force_a_purge_search() {
        let stored = Folder {
            uidnext: Some(10),
            total: 5,
            ..folder_with(Some(1), Some(40))
        };
        let grew_by_two = SelectedMailbox {
            uidvalidity: Some(1),
            uidnext: Some(12),
            highest_modseq: Some(99),
            exists: 7,
        };
        assert!(!mailbox_needs_purge(&stored, &grew_by_two));

        // Two arrived but one old message was expunged: EXISTS is one short.
        let grew_and_expunged = SelectedMailbox {
            exists: 6,
            ..grew_by_two.clone()
        };
        assert!(mailbox_needs_purge(&stored, &grew_and_expunged));

        // Two arrived, one of the new ones was expunged, one old one too.
        let both_expunged = SelectedMailbox {
            exists: 5,
            ..grew_by_two
        };
        assert!(mailbox_needs_purge(&stored, &both_expunged));
    }

    #[test]
    fn new_uid_fetch_start_when_uidnext_advanced() {
        let stored = Folder {
            uidnext: Some(10),
            total: 5,
            ..folder_with(Some(1), Some(40))
        };
        let selected = SelectedMailbox {
            uidvalidity: Some(1),
            uidnext: Some(20),
            highest_modseq: Some(99),
            exists: 6,
        };
        assert_eq!(new_uid_fetch_start(&stored, &selected), Some(10));
        assert_eq!(
            new_uid_fetch_start(
                &Folder {
                    uidnext: Some(20),
                    ..stored.clone()
                },
                &selected
            ),
            None
        );
    }

    #[test]
    fn early_list_fetch_targets_new_drafts_not_inbox() {
        let selected = SelectedMailbox {
            uidvalidity: Some(1),
            uidnext: Some(20),
            highest_modseq: Some(99),
            exists: 6,
        };
        let drafts = Folder {
            imap_name: "Drafts".to_string(),
            special: SpecialUse::Drafts,
            uidnext: Some(10),
            total: 5,
            ..folder_with(Some(1), Some(40))
        };
        assert_eq!(
            early_list_fetch(
                &drafts,
                MailboxSyncPlan::Incremental { changed_since: 40 },
                &selected
            ),
            Some(EarlyListFetch::Uid("10:*".to_string()))
        );
        assert_eq!(
            early_list_fetch(
                &drafts,
                MailboxSyncPlan::NewUids { from_uid: 10 },
                &selected
            ),
            Some(EarlyListFetch::Uid("10:*".to_string()))
        );
        assert_eq!(
            early_list_fetch(
                &drafts,
                MailboxSyncPlan::RecentWindow { start_seq: 1 },
                &selected
            ),
            Some(EarlyListFetch::Seq("1:*".to_string()))
        );
        let caught_up = Folder {
            uidnext: Some(20),
            ..drafts.clone()
        };
        assert!(
            early_list_fetch(
                &caught_up,
                MailboxSyncPlan::Incremental { changed_since: 40 },
                &selected
            )
            .is_none()
        );
        let inbox = Folder {
            special: SpecialUse::Inbox,
            ..drafts
        };
        assert!(
            early_list_fetch(
                &inbox,
                MailboxSyncPlan::Incremental { changed_since: 40 },
                &selected
            )
            .is_none()
        );
    }

    #[test]
    fn short_local_page_reconciles_even_when_imap_meta_matches() {
        let stored = Folder {
            uidnext: Some(20),
            total: 18,
            ..folder_with(Some(1), Some(40))
        };
        let selected = SelectedMailbox {
            uidvalidity: Some(1),
            uidnext: Some(20),
            highest_modseq: Some(40),
            exists: 18,
        };
        assert!(!mailbox_needs_purge(&stored, &selected));
        assert!(mailbox_needs_uid_reconcile(&stored, &selected, 8, 100));
        assert!(!mailbox_needs_uid_reconcile(&stored, &selected, 18, 100));
        assert!(!mailbox_needs_uid_reconcile(&stored, &selected, 100, 100));
    }

    #[test]
    fn folder_is_syncable_skips_outbox() {
        let outbox = Folder {
            id: FolderId(1),
            account_id: AccountId(Uuid::nil()),
            imap_name: "Alcedo/Outbox".to_string(),
            delimiter: "/".to_string(),
            special: SpecialUse::Outbox,
            selectable: false,
            uidvalidity: None,
            uidnext: None,
            highest_modseq: None,
            unread: 0,
            total: 0,
        };
        assert!(!folder_is_syncable(&outbox));

        let inbox = folder_with(Some(1), None);
        assert!(folder_is_syncable(&inbox));
    }

    #[test]
    fn mailbox_select_is_needed_when_cache_empty() {
        assert!(mailbox_select_is_needed("", "INBOX"));
        assert!(!mailbox_select_is_needed("INBOX", "INBOX"));
    }

    #[test]
    fn dest_sync_must_update_cache_or_next_copy_skips_select() {
        // Copy 1: SELECT INBOX, UID COPY dest, dest sync SELECTs dest.
        // If the recorded selection still says INBOX, copy 2 skips SELECT and COPY dest
        // from dest → "[CANNOT] UID COPY not allowed for same folder".
        assert!(!mailbox_select_is_needed("INBOX", "INBOX"));
        assert!(mailbox_select_is_needed("test2", "INBOX"));
        assert!(mailbox_select_is_needed("INBOX", "test2"));
    }
}
