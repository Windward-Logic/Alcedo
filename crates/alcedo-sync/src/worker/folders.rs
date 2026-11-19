//! Folder operations: create, rename, delete, empty and delete-on-or-before.
#![allow(clippy::too_many_arguments)]

use super::delete::{
    DeletePlan, apply_folder_uid_deletes, apply_folder_uid_deletes_with_plan, plan_message_delete,
};
use super::idle::{commit_worker_inbox, park_idle_for_mailbox};
use super::mailbox_sync::{
    emit_account_folders, ensure_inbox_selected, ensure_selected, persist_folder_unread_and_total,
    sync_folders, sync_selected_mailbox,
};
use super::session::is_recoverable_disconnect;
use super::*;
use alcedo_i18n::{tr, trf};

pub(super) fn is_selected_or_descendant(selected: &str, imap_name: &str, delimiter: &str) -> bool {
    selected == imap_name
        || (!delimiter.is_empty() && selected.starts_with(&format!("{imap_name}{delimiter}")))
}

pub(super) async fn ensure_not_selected_prefix(
    state: &mut WorkerImapState,
    imap_name: &str,
    delimiter: &str,
) -> Result<()> {
    if is_selected_or_descendant(state.selected_on_server(), imap_name, delimiter) {
        ensure_inbox_selected(state).await?;
    }
    Ok(())
}

pub(super) fn resolve_parent_imap(
    folders: &[Folder],
    parent: Option<FolderId>,
) -> Result<Option<String>> {
    match parent {
        None => Ok(None),
        Some(id) => folders
            .iter()
            .find(|folder| folder.id == id)
            .map(|folder| folder.imap_name.clone())
            .ok_or_else(|| Error::Other(tr!("Folder not found")))
            .map(Some),
    }
}

pub(super) fn rename_failure_name_gone(err: &Error) -> bool {
    match err {
        Error::Imap(msg) => {
            let lower = msg.to_ascii_lowercase();
            lower.contains("nonexistent")
                || lower.contains("does not exist")
                || lower.contains("no such")
                || lower.contains("unknown mailbox")
                || lower.contains("[nonexistent]")
        }
        _ => false,
    }
}

pub(super) async fn create_folder(
    state: &mut WorkerImapState,
    account: &Account,
    parent: Option<FolderId>,
    name: String,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
) -> Result<()> {
    let folders = store.list_folders(account.id).await?;
    let delimiter = folder_delimiter_for_parent(&folders, parent);
    let leaf = name.trim();
    if let Err(message) = validate_folder_leaf(leaf, &delimiter) {
        emit_error(events, Some(account.id), message);
        return Ok(());
    }

    let parent_imap = match resolve_parent_imap(&folders, parent) {
        Ok(name) => name,
        Err(err) => {
            emit_error(events, Some(account.id), err.to_string());
            return Ok(());
        }
    };
    let imap_path = join_mailbox_path(parent_imap.as_deref(), &delimiter, leaf);
    if imap_name_exists(&folders, &imap_path) {
        emit_error(
            events,
            Some(account.id),
            tr!("A folder with this name already exists"),
        );
        return Ok(());
    }

    emit_status(events, account, trf!("Creating {folder}…", folder = leaf));

    if let Err(err) = state.client_mut().create_mailbox(&imap_path).await {
        emit_error(events, Some(account.id), err.to_string());
    }

    state.inbox = sync_folders(state.client_mut(), account, store, events).await?;

    if let Some(new_folder) = store
        .list_folders(account.id)
        .await?
        .into_iter()
        .find(|folder| folder.imap_name == imap_path)
        && new_folder.selectable
    {
        emit_status(
            events,
            account,
            trf!(
                "Syncing {folder}…",
                folder = folder_display_name(&new_folder)
            ),
        );
        sync_selected_mailbox(
            state,
            account,
            &new_folder,
            store,
            events,
            &ServerRulesSession::default(),
            false,
        )
        .await?;
    }

    Ok(())
}

pub(super) async fn rename_folder(
    state: &mut WorkerImapState,
    account: &Account,
    id: FolderId,
    parent: Option<FolderId>,
    name: String,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
    split: &SplitImapContext,
) -> Result<()> {
    let folders = store.list_folders(account.id).await?;
    let Some(folder) = folders.iter().find(|folder| folder.id == id) else {
        emit_error(events, Some(account.id), tr!("Folder not found"));
        return Ok(());
    };
    if !folder_is_mutable(folder) {
        emit_error(
            events,
            Some(account.id),
            tr!("This folder cannot be modified"),
        );
        return Ok(());
    }

    let delimiter = folder_delimiter_for_parent(&folders, parent);
    let leaf = name.trim();
    if let Err(message) = validate_folder_leaf(leaf, &delimiter) {
        emit_error(events, Some(account.id), message);
        return Ok(());
    }

    let parent_imap = match resolve_parent_imap(&folders, parent) {
        Ok(name) => name,
        Err(err) => {
            emit_error(events, Some(account.id), err.to_string());
            return Ok(());
        }
    };
    let new_imap = join_mailbox_path(parent_imap.as_deref(), &delimiter, leaf);
    if new_imap == folder.imap_name {
        return Ok(());
    }
    if folders
        .iter()
        .any(|candidate| candidate.imap_name == new_imap && candidate.id != id)
    {
        emit_error(
            events,
            Some(account.id),
            tr!("A folder with this name already exists"),
        );
        return Ok(());
    }

    let old_imap = folder.imap_name.clone();
    let all_names: Vec<String> = folders
        .iter()
        .map(|folder| folder.imap_name.clone())
        .collect();
    let rename_pairs = rename_imap_map(&old_imap, &new_imap, &delimiter, &all_names);

    park_idle_for_mailbox(&split.idle_ctrl, &state.inbox, &old_imap, &delimiter).await?;
    ensure_not_selected_prefix(state, &old_imap, &delimiter).await?;

    emit_status(
        events,
        account,
        trf!("Renaming {folder}…", folder = folder_display_name(folder)),
    );

    let mut imap_renames = rename_pairs.clone();
    imap_renames.sort_by_key(|(from, _)| from.len());
    let mut parent_renamed = true;
    for (from, to) in imap_renames {
        if let Err(err) = state.client_mut().rename_mailbox(&from, &to).await {
            if from != old_imap && rename_failure_name_gone(&err) {
                continue;
            }
            emit_error(events, Some(account.id), err.to_string());
            if from == old_imap {
                parent_renamed = false;
                break;
            }
        }
    }

    if parent_renamed {
        store.rename_folder_tree(account.id, &rename_pairs).await?;
    }
    let inbox = sync_folders(state.client_mut(), account, store, events).await?;
    commit_worker_inbox(state, inbox, Some(split)).await;
    Ok(())
}

pub(super) async fn delete_folder(
    state: &mut WorkerImapState,
    account: &Account,
    folder_id: FolderId,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
    split: &SplitImapContext,
) -> Result<()> {
    let folders = store.list_folders(account.id).await?;
    let Some(folder) = folders.iter().find(|folder| folder.id == folder_id) else {
        emit_error(events, Some(account.id), tr!("Folder not found"));
        return Ok(());
    };
    if !folder_is_mutable(folder) {
        emit_error(
            events,
            Some(account.id),
            tr!("This folder cannot be modified"),
        );
        return Ok(());
    }

    let delimiter = effective_mailbox_delimiter(&folder.delimiter).to_string();

    park_idle_for_mailbox(
        &split.idle_ctrl,
        &state.inbox,
        &folder.imap_name,
        &delimiter,
    )
    .await?;
    // Only Gmail (`X-GM-EXT-1`) needs this. It answers OK to DELETE and keeps
    // the label while any mailbox is selected, and it also keeps a label that
    // still has messages. A conforming server deletes the mailbox with DELETE
    // once the mailbox itself is not selected.
    let gmail_labels = state.client_mut().capabilities().gmail_ext;
    if gmail_labels {
        deselect_for_mailbox_delete(state).await?;
    } else {
        ensure_not_selected_prefix(state, &folder.imap_name, &delimiter).await?;
    }

    let mut names = descendant_imap_names(&folders, &folder.imap_name, &delimiter);
    names.sort_by_key(|name| std::cmp::Reverse(name.len()));

    emit_status(
        events,
        account,
        trf!("Deleting {folder}…", folder = folder_display_name(folder)),
    );

    let mut failure = None;
    for name in &names {
        if let Err(err) = state.client_mut().delete_mailbox(name).await {
            debug!(mailbox = %name, %err, "DELETE mailbox failed");
            if failure.is_none() {
                failure = Some(err);
            }
        }
    }

    state.inbox = sync_folders(state.client_mut(), account, store, events).await?;
    if gmail_labels {
        let mut stored = store.list_folders(account.id).await?;
        let mut stuck = imap_names_still_present(&stored, &names);
        if !stuck.is_empty() {
            debug!(
                ?stuck,
                "Gmail label still listed after DELETE; expunging and retrying"
            );
            for name in &stuck {
                if let Err(err) = delete_mailbox_after_expunge(state, name).await {
                    debug!(mailbox = %name, %err, "retry mailbox delete failed");
                    failure = Some(err);
                }
            }
            state.inbox = sync_folders(state.client_mut(), account, store, events).await?;
            stored = store.list_folders(account.id).await?;
            stuck = imap_names_still_present(&stored, &names);
        }
        if !stuck.is_empty() {
            let detail = failure
                .map(|err| err.to_string())
                .unwrap_or_else(|| tr!("the server kept the folder"));
            emit_error(
                events,
                Some(account.id),
                trf!(
                    "Couldn't delete {folders}: {reason}",
                    folders = stuck.join(", "),
                    reason = detail
                ),
            );
        }
    } else if let Some(err) = failure {
        emit_error(events, Some(account.id), err.to_string());
    }
    Ok(())
}

/// Leave Selected state before `DELETE` on a Gmail (`X-GM-EXT-1`) session.
///
/// Gmail returns `OK` and does not remove a user label when any mailbox is
/// selected, including Inbox. `UNSELECT` is the command that drops selection.
/// Servers without it SELECT Inbox instead, which moves the selection off the
/// mailbox being deleted (a no-op when Inbox is already selected).
pub(super) async fn deselect_for_mailbox_delete(state: &mut WorkerImapState) -> Result<()> {
    if state.client_mut().capabilities().unselect {
        match state.client_mut().unselect().await {
            Ok(()) => return Ok(()),
            Err(err) if is_recoverable_disconnect(&err) => return Err(err),
            Err(err) => {
                debug!(%err, "UNSELECT before mailbox delete was rejected");
                return Ok(());
            }
        }
    }
    let inbox_name = state.inbox.imap_name.clone();
    ensure_selected(state, &inbox_name).await
}

/// Gmail retry when `DELETE` is accepted but `LIST` still returns the label.
///
/// A label that still has messages is kept. Expunge removes that label from
/// those messages, then `UNSELECT` + `DELETE`. Not used for other servers.
pub(super) async fn delete_mailbox_after_expunge(
    state: &mut WorkerImapState,
    name: &str,
) -> Result<()> {
    ensure_selected(state, name).await?;
    if let Err(err) = state.client_mut().uid_expunge_all().await {
        debug!(mailbox = %name, %err, "expunge before mailbox delete failed");
    }
    deselect_for_mailbox_delete(state).await?;
    state.client_mut().delete_mailbox(name).await
}

pub(super) fn imap_names_still_present(folders: &[Folder], names: &[String]) -> Vec<String> {
    names
        .iter()
        .filter(|name| folders.iter().any(|folder| &folder.imap_name == *name))
        .cloned()
        .collect()
}

pub(super) async fn empty_folder(
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
        trf!("Emptying {folder}…", folder = folder_display_name(&folder)),
    );

    if matches!(folder.special, SpecialUse::Trash | SpecialUse::Junk) {
        ensure_selected(state, &folder.imap_name).await?;
        state.client_mut().uid_expunge_all().await?;
        store.mark_folder_deleted(folder_id).await?;

        let folders = store.list_folders(account.id).await?;
        let delimiter = effective_mailbox_delimiter(&folder.delimiter).to_string();
        let nested = nested_imap_names(&folders, &folder.imap_name, &delimiter);
        if !nested.is_empty() {
            if nested
                .iter()
                .any(|name| is_selected_or_descendant(state.selected_on_server(), name, &delimiter))
            {
                ensure_inbox_selected(state).await?;
            }
            let mut first_err = None;
            for name in nested {
                let selected = match ensure_selected(state, &name).await {
                    Ok(()) => true,
                    Err(err) => {
                        if first_err.is_none() {
                            first_err = Some(err);
                        }
                        false
                    }
                };
                if selected
                    && let Err(err) = state.client_mut().uid_expunge_all().await
                    && first_err.is_none()
                {
                    first_err = Some(err);
                }
                ensure_inbox_selected(state).await?;
                if let Err(err) = state.client_mut().delete_mailbox(&name).await
                    && first_err.is_none()
                {
                    first_err = Some(err);
                }
            }
            if let Some(err) = first_err {
                emit_error(events, Some(account.id), err.to_string());
            }
            state.inbox = sync_folders(state.client_mut(), account, store, events).await?;
            ensure_selected(state, &folder.imap_name).await?;
        }

        let loaded = store.get_folder(folder_id).await?.unwrap_or(folder);
        store
            .replace_folders_with_mode(
                account.id,
                &[Folder {
                    unread: 0,
                    total: 0,
                    ..loaded
                }],
                Some(FolderReplaceMode::SelectRefresh),
            )
            .await?;
        emit_account_folders(account.id, store, events).await?;

        select_folder(store, folder_id, list_mode(), events).await?;

        emit_unread_counts(store, events).await?;
        return Ok(());
    }

    ensure_selected(state, &folder.imap_name).await?;
    let folders = store.list_folders(account.id).await?;
    let delete_plan = plan_message_delete(&folders, &folder, false);
    let uids = state.client_mut().uid_search_all().await?;
    // RFC 3501 COPY and RFC 6851 MOVE keep flags and internal date. One
    // `UID STORE 1:* +FLAGS.SILENT (\Seen)` on the source (already selected)
    // marks every trash copy read. `.SILENT` skips a FETCH per message, and
    // `+FLAGS` leaves `\Flagged` and keywords alone. A later STORE on Trash
    // would need another SELECT plus the destination UIDs.
    if !uids.is_empty() && matches!(&delete_plan, DeletePlan::MoveToTrash { .. }) {
        state.client_mut().uid_store_seen_all().await?;
        store.mark_folder_seen(folder_id).await?;
    }
    apply_folder_uid_deletes_with_plan(
        state,
        account,
        &folder,
        folder_id,
        uids,
        delete_plan,
        &folders,
        store,
        events,
    )
    .await?;
    Ok(())
}

pub(super) fn imap_error_text(err: &Error) -> String {
    match err {
        Error::Imap(message) => message.clone(),
        other => other.to_string(),
    }
}

pub(super) fn format_delete_through_date(through: NaiveDate) -> String {
    through.format("%A %-d %B %Y").to_string()
}

pub(super) async fn replace_folder_envelopes_after_delete(
    account: &Account,
    folder: &Folder,
    folder_id: FolderId,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
) -> Result<()> {
    persist_folder_unread_and_total(account, folder_id, store, events).await?;
    select_folder(store, folder_id, list_mode(), events).await?;
    if matches!(folder.special, SpecialUse::Inbox) {
        refresh_virtual_inboxes(store, events).await?;
    }
    Ok(())
}

pub(super) async fn delete_folder_on_or_before(
    state: &mut WorkerImapState,
    account: &Account,
    folder_id: FolderId,
    through: NaiveDate,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
) -> Result<()> {
    let folder = match store.get_folder(folder_id).await? {
        Some(folder) => folder,
        None => {
            let _ = events.send(SyncEvent::FolderDeleteFinished {
                folder: folder_id,
                removed: 0,
                moved_to_trash: false,
            });
            return Ok(());
        }
    };
    if folder.account_id != account.id || !folder.selectable {
        let _ = events.send(SyncEvent::FolderDeleteFinished {
            folder: folder_id,
            removed: 0,
            moved_to_trash: false,
        });
        return Ok(());
    }

    let date_text = format_delete_through_date(through);
    emit_status(events, account, tr!("Deleting older mail…"));

    ensure_selected(state, &folder.imap_name).await?;
    let uids = state
        .client_mut()
        .uid_search(&imap_search_before_inclusive(through))
        .await?;

    if uids.is_empty() {
        let folders = store.list_folders(account.id).await?;
        let delete_plan = plan_message_delete(&folders, &folder, false);
        let moved_to_trash = matches!(delete_plan, DeletePlan::MoveToTrash { .. });
        emit_status(
            events,
            account,
            trf!("No messages on or before {date}.", date = date_text),
        );
        let _ = events.send(SyncEvent::FolderDeleteFinished {
            folder: folder_id,
            removed: 0,
            moved_to_trash,
        });
        return Ok(());
    }

    let (total_removed, moved_to_trash) =
        apply_folder_uid_deletes(state, account, &folder, folder_id, uids, store, events).await?;
    let _ = events.send(SyncEvent::FolderDeleteFinished {
        folder: folder_id,
        removed: total_removed,
        moved_to_trash,
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worker::test_support::folder_with;

    #[test]
    fn imap_names_still_present_keeps_only_listed_names() {
        let mut inbox = folder_with(None, None);
        inbox.imap_name = "INBOX".to_string();
        let mut label = folder_with(None, None);
        label.imap_name = "test".to_string();
        let folders = vec![inbox, label];
        let names = vec!["test".to_string(), "gone".to_string()];
        assert_eq!(
            imap_names_still_present(&folders, &names),
            vec!["test".to_string()]
        );
    }

    #[test]
    fn rename_failure_name_gone_detects_missing_mailbox() {
        assert!(rename_failure_name_gone(&Error::Imap(
            "RENAME Projects/Work Archive/Work: NO [NONEXISTENT] Unknown Mailbox".to_string()
        )));
        assert!(!rename_failure_name_gone(&Error::Imap(
            "RENAME Projects Archive: NO [ALREADYEXISTS]".to_string()
        )));
    }

    #[test]
    fn is_selected_or_descendant_matches_prefix() {
        assert!(is_selected_or_descendant("Projects/Work", "Projects", "/"));
        assert!(is_selected_or_descendant("Projects", "Projects", "/"));
        assert!(!is_selected_or_descendant("Archive", "Projects", "/"));
        assert!(!is_selected_or_descendant("ProjectsExtra", "Projects", "/"));
    }
}
