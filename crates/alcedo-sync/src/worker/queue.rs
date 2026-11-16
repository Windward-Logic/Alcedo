//! Command queue: ordering, coalescing, batching and retry after a dropped connection.
#![allow(clippy::too_many_arguments)]

use super::drafts::normalize_draft_message_id;
use super::mailbox_sync::ensure_inbox_selected;
use super::session::{is_fetch_timeout, is_recoverable_disconnect};
use super::watched::body_prefetch_after;
use super::*;

pub(super) fn command_batch_reselects_inbox(idle_split: bool) -> bool {
    !idle_split
}

pub(super) fn worker_command_rank(cmd: &WorkerCommand) -> u8 {
    match cmd {
        WorkerCommand::OpenMessage(_) | WorkerCommand::FetchRawMessage(_) => 0,
        // Filing the server copy can upload megabytes. A click paints first.
        WorkerCommand::FileSentOnImap { .. } => 3,
        WorkerCommand::PrefetchMessage(_)
        | WorkerCommand::SetFlag { .. }
        | WorkerCommand::SetFlags { .. } => 1,
        WorkerCommand::FetchMessageRemainder(_) => 2,
        WorkerCommand::BackfillFolder { .. } => 3,
        WorkerCommand::PrefetchBodies { .. } => 4,
        _ => 2,
    }
}

pub(super) fn coalesce_open_messages(cmds: &mut Vec<WorkerCommand>) {
    let mut opened = std::collections::HashSet::new();
    for cmd in cmds.iter() {
        if let WorkerCommand::OpenMessage(id) = cmd {
            opened.insert(*id);
        }
    }
    let mut seen_open = std::collections::HashSet::new();
    let mut seen_prefetch = std::collections::HashSet::new();
    cmds.retain(|cmd| match cmd {
        WorkerCommand::OpenMessage(id) => seen_open.insert(*id),
        // A real open of this row already carries the attachment follow-up.
        WorkerCommand::PrefetchMessage(id) => !opened.contains(id) && seen_prefetch.insert(*id),
        _ => true,
    });
}

pub(super) fn coalesce_backfill_folders(cmds: &mut Vec<WorkerCommand>) {
    let mut best_have: std::collections::HashMap<FolderId, u32> = std::collections::HashMap::new();
    for cmd in cmds.iter() {
        if let WorkerCommand::BackfillFolder { folder, have } = cmd {
            let entry = best_have.entry(*folder).or_insert(*have);
            *entry = (*entry).max(*have);
        }
    }
    let mut seen = std::collections::HashSet::new();
    cmds.retain(|cmd| match cmd {
        WorkerCommand::BackfillFolder { folder, have } => {
            best_have.get(folder) == Some(have) && seen.insert(*folder)
        }
        _ => true,
    });
}

pub(super) fn coalesce_prefetch_bodies(cmds: &mut Vec<WorkerCommand>) {
    let mut seen = HashSet::new();
    cmds.retain(|cmd| match cmd {
        WorkerCommand::PrefetchBodies { folder } => seen.insert(*folder),
        _ => true,
    });
}

pub(super) fn order_pending_worker_commands(cmds: &mut Vec<WorkerCommand>) {
    // Latest body wins. A second autosave often arrives while the first APPEND
    // is still in this batch, still carrying the UID the first save is about to delete.
    coalesce_imap_draft_saves(cmds);
    cmds.sort_by_key(worker_command_rank);
    coalesce_open_messages(cmds);
    coalesce_backfill_folders(cmds);
    coalesce_prefetch_bodies(cmds);
}

pub(super) fn coalesce_imap_draft_saves(cmds: &mut Vec<WorkerCommand>) {
    let mut last_index: HashMap<String, usize> = HashMap::new();
    for (index, cmd) in cmds.iter().enumerate() {
        if let WorkerCommand::SaveImapDraft(draft) = cmd {
            let Some(key) = draft
                .imap_draft
                .as_ref()
                .map(|imap| normalize_draft_message_id(&imap.message_id))
                .filter(|id| !id.is_empty())
            else {
                continue;
            };
            last_index.insert(key, index);
        }
    }
    if last_index.is_empty() {
        return;
    }
    let mut index = 0;
    cmds.retain(|cmd| {
        let keep = match cmd {
            WorkerCommand::SaveImapDraft(draft) => {
                let key = draft
                    .imap_draft
                    .as_ref()
                    .map(|imap| normalize_draft_message_id(&imap.message_id))
                    .filter(|id| !id.is_empty());
                match key {
                    Some(key) => last_index.get(&key) == Some(&index),
                    None => true,
                }
            }
            _ => true,
        };
        index += 1;
        keep
    });
}

pub(super) fn drain_worker_msgs(
    rx: &mut mpsc::Receiver<WorkerMsg>,
    first: WorkerCommand,
) -> std::result::Result<Vec<WorkerCommand>, WaitOutcome> {
    let mut cmds = vec![first];
    loop {
        match rx.try_recv() {
            Ok(WorkerMsg::Shutdown) => return Err(WaitOutcome::Shutdown),
            Ok(WorkerMsg::Command(cmd)) => cmds.push(cmd),
            Err(_) => break,
        }
    }
    order_pending_worker_commands(&mut cmds);
    Ok(cmds)
}

pub(super) fn fold_channel_commands(
    rx: &mut mpsc::Receiver<WorkerMsg>,
    cmds: &mut Vec<WorkerCommand>,
) -> std::result::Result<(), WaitOutcome> {
    loop {
        match rx.try_recv() {
            Ok(WorkerMsg::Shutdown) => return Err(WaitOutcome::Shutdown),
            Ok(WorkerMsg::Command(cmd)) => cmds.push(cmd),
            Err(_) => return Ok(()),
        }
    }
}

pub(super) async fn ensure_inbox_after_batch(state: &mut WorkerImapState) -> Result<WaitOutcome> {
    match ensure_inbox_selected(state).await {
        Ok(()) => Ok(WaitOutcome::Continue),
        Err(err) if is_recoverable_disconnect(&err) => {
            Ok(WaitOutcome::Reconnect { retry: Vec::new() })
        }
        Err(err) => Err(err),
    }
}

pub(super) async fn run_command_batch(
    mut cmds: Vec<WorkerCommand>,
    msg_rx: &mut mpsc::Receiver<WorkerMsg>,
    state: &mut WorkerImapState,
    account: &Account,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
    auth_notified: &mut bool,
    server_rules: &mut ServerRulesSession,
    split: &SplitImapContext,
) -> Result<WaitOutcome> {
    loop {
        if let Err(outcome) = fold_channel_commands(msg_rx, &mut cmds) {
            return Ok(outcome);
        }
        if cmds.is_empty() {
            break;
        }
        order_pending_worker_commands(&mut cmds);
        let cmd = cmds.remove(0);
        // Attachment bytes are optional for the first paint. If a click lands
        // while we are about to start that download, do the click first.
        let cmd = if let WorkerCommand::FetchMessageRemainder(_) = &cmd {
            tokio::select! {
                biased;
                msg = msg_rx.recv() => {
                    match msg {
                        Some(WorkerMsg::Shutdown) | None => return Ok(WaitOutcome::Shutdown),
                        Some(WorkerMsg::Command(next)) => {
                            cmds.push(cmd);
                            cmds.push(next);
                            continue;
                        }
                    }
                }
                _ = tokio::time::sleep(REMAINDER_GRACE) => cmd,
            }
        } else {
            cmd
        };
        match handle_worker_command(
            cmd.clone(),
            state,
            account,
            store,
            events,
            auth_notified,
            server_rules,
            split,
        )
        .await
        {
            Ok(followup) => {
                if let Some(followup) = followup {
                    cmds.push(followup);
                }
                if let Some(prefetch) = body_prefetch_after(&cmd, state.inbox.id) {
                    cmds.push(prefetch);
                }
            }
            Err(err) if is_recoverable_disconnect(&err) => {
                if retry_after_command_failure(cmd.clone()).is_none()
                    && !delete_failure_reports_itself(&cmd)
                {
                    emit_error(events, Some(account.id), err.to_string());
                }
                return Ok(WaitOutcome::Reconnect {
                    retry: reconnect_retry_cmds(cmd, cmds),
                });
            }
            Err(err) if is_auth_failure(&err) => {
                if !*auth_notified {
                    let _ = events.send(SyncEvent::AuthRequired {
                        account: account.id,
                        message: err.to_string(),
                    });
                    *auth_notified = true;
                }
                continue;
            }
            Err(err) if is_fetch_timeout(&err) => {
                emit_error(events, Some(account.id), err.to_string());
                // The cancelled FETCH is still streaming into the socket.
                // Reconnect, but do not retry the command that timed out.
                return Ok(WaitOutcome::Reconnect { retry: cmds });
            }
            Err(err) => {
                emit_error(events, Some(account.id), err.to_string());
                if cmds.is_empty()
                    && msg_rx.is_empty()
                    && let Err(select_err) = ensure_inbox_selected(state).await
                {
                    if is_recoverable_disconnect(&select_err) {
                        return Ok(WaitOutcome::Reconnect { retry: cmds });
                    }
                    return Err(select_err);
                }
                continue;
            }
        }
    }

    if msg_rx.is_empty() {
        if split.reselect_inbox {
            ensure_inbox_after_batch(state).await
        } else {
            Ok(WaitOutcome::Continue)
        }
    } else {
        Ok(WaitOutcome::Continue)
    }
}

pub(super) fn command_from_msg(msg: Option<WorkerMsg>) -> Option<WorkerCommand> {
    match msg {
        Some(WorkerMsg::Command(cmd)) => Some(cmd),
        _ => None,
    }
}

pub(super) fn delete_failure_reports_itself(cmd: &WorkerCommand) -> bool {
    matches!(
        cmd,
        WorkerCommand::DeleteMessages { .. }
            | WorkerCommand::ReconcileDeleteMessages { .. }
            | WorkerCommand::ExpungeMessages { .. }
            | WorkerCommand::ReconcileExpungeMessages { .. }
    )
}

/// Commands that already ran and failed with a dropped socket. Mutations may
/// have completed on the server before the client saw the error (NO /
/// ALREADYEXISTS on a second attempt is not always harmless).
pub(super) fn reconnect_retry_cmds(
    failed: WorkerCommand,
    remaining: Vec<WorkerCommand>,
) -> Vec<WorkerCommand> {
    let retry = match failed {
        WorkerCommand::DeleteMessages { ids } => {
            Some(WorkerCommand::ReconcileDeleteMessages { ids })
        }
        WorkerCommand::ExpungeMessages { ids } => {
            Some(WorkerCommand::ReconcileExpungeMessages { ids })
        }
        other => retry_after_command_failure(other),
    };
    match retry {
        Some(retry) => std::iter::once(retry).chain(remaining).collect(),
        None => remaining,
    }
}

pub(super) fn retry_after_command_failure(cmd: WorkerCommand) -> Option<WorkerCommand> {
    match cmd {
        WorkerCommand::Send(_)
        | WorkerCommand::SaveImapDraft(_)
        | WorkerCommand::DeleteImapDraft { .. }
        | WorkerCommand::FinishSend { .. }
        | WorkerCommand::CopyMessageToFolder { .. }
        | WorkerCommand::MoveMessage { .. }
        | WorkerCommand::MoveMessageToFolder { .. }
        | WorkerCommand::MoveMessages { .. }
        | WorkerCommand::MoveMessagesToFolder { .. }
        | WorkerCommand::DeleteMessage(_)
        | WorkerCommand::DeleteMessages { .. }
        | WorkerCommand::ReconcileDeleteMessages { .. }
        | WorkerCommand::ExpungeMessages { .. }
        | WorkerCommand::ReconcileExpungeMessages { .. }
        | WorkerCommand::UndeleteMessage { .. }
        | WorkerCommand::CreateFolder { .. }
        | WorkerCommand::RenameFolder { .. }
        | WorkerCommand::DeleteFolder(_)
        | WorkerCommand::UpsertMessageRule(_)
        | WorkerCommand::DeleteMessageRule { .. }
        | WorkerCommand::ReorderMessageRules { .. }
        | WorkerCommand::AdoptServerRules => None,
        cmd => Some(cmd),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worker::test_support::sample_imap_draft;
    use alcedo_core::{AccountId, FolderId, MessageId, SpecialUse};
    use uuid::Uuid;

    #[test]
    fn reconnect_retry_cmds_open_disconnect_keeps_remaining_delete() {
        let retry = reconnect_retry_cmds(
            WorkerCommand::OpenMessage(MessageId(2)),
            vec![WorkerCommand::DeleteMessage(MessageId(1))],
        );
        assert_eq!(retry.len(), 2);
        assert!(matches!(
            &retry[0],
            WorkerCommand::OpenMessage(MessageId(2))
        ));
        assert!(matches!(
            &retry[1],
            WorkerCommand::DeleteMessage(MessageId(1))
        ));
    }

    #[test]
    fn reconnect_retry_cmds_delete_disconnect_keeps_remaining_open() {
        let retry = reconnect_retry_cmds(
            WorkerCommand::DeleteMessage(MessageId(1)),
            vec![WorkerCommand::OpenMessage(MessageId(2))],
        );
        assert_eq!(retry.len(), 1);
        assert!(matches!(
            &retry[0],
            WorkerCommand::OpenMessage(MessageId(2))
        ));
    }

    #[test]
    fn reconnect_retry_cmds_delete_messages_becomes_reconcile() {
        let retry = reconnect_retry_cmds(
            WorkerCommand::DeleteMessages {
                ids: vec![MessageId(483)],
            },
            vec![WorkerCommand::OpenMessage(MessageId(2))],
        );
        assert_eq!(retry.len(), 2);
        assert!(matches!(
            &retry[0],
            WorkerCommand::ReconcileDeleteMessages { ids } if *ids == vec![MessageId(483)]
        ));
        assert!(matches!(
            &retry[1],
            WorkerCommand::OpenMessage(MessageId(2))
        ));
    }

    #[test]
    fn reconnect_retry_cmds_expunge_messages_becomes_reconcile() {
        let retry = reconnect_retry_cmds(
            WorkerCommand::ExpungeMessages {
                ids: vec![MessageId(483)],
            },
            vec![WorkerCommand::OpenMessage(MessageId(2))],
        );
        assert_eq!(retry.len(), 2);
        assert!(matches!(
            &retry[0],
            WorkerCommand::ReconcileExpungeMessages { ids } if *ids == vec![MessageId(483)]
        ));
        assert!(matches!(
            &retry[1],
            WorkerCommand::OpenMessage(MessageId(2))
        ));
        assert!(
            retry_after_command_failure(WorkerCommand::ReconcileExpungeMessages {
                ids: vec![MessageId(1)],
            })
            .is_none()
        );
        assert!(
            retry_after_command_failure(WorkerCommand::ExpungeMessages {
                ids: vec![MessageId(1)],
            })
            .is_none()
        );
    }

    #[test]
    fn retry_after_command_failure_skips_reconcile_delete() {
        assert!(
            retry_after_command_failure(WorkerCommand::ReconcileDeleteMessages {
                ids: vec![MessageId(1)],
            })
            .is_none()
        );
    }

    #[test]
    fn reconnect_retry_cmds_reconcile_disconnect_drops_itself() {
        let retry = reconnect_retry_cmds(
            WorkerCommand::ReconcileDeleteMessages {
                ids: vec![MessageId(1)],
            },
            vec![WorkerCommand::OpenMessage(MessageId(2))],
        );
        assert_eq!(retry.len(), 1);
        assert!(matches!(
            &retry[0],
            WorkerCommand::OpenMessage(MessageId(2))
        ));
    }

    #[test]
    fn worker_command_rank_open_before_attachment_remainder() {
        let mut cmds = vec![
            WorkerCommand::FetchMessageRemainder(MessageId(1)),
            WorkerCommand::BackfillFolder {
                folder: alcedo_core::FolderId(1),
                have: 100,
            },
            WorkerCommand::OpenMessage(MessageId(2)),
        ];
        order_pending_worker_commands(&mut cmds);
        assert!(matches!(&cmds[0], WorkerCommand::OpenMessage(MessageId(2))));
        assert!(matches!(
            &cmds[1],
            WorkerCommand::FetchMessageRemainder(MessageId(1))
        ));
        assert!(matches!(&cmds[2], WorkerCommand::BackfillFolder { .. }));
    }

    #[test]
    fn open_message_runs_before_filing_the_sent_copy() {
        let mut cmds = vec![
            WorkerCommand::FileSentOnImap {
                draft: ComposeDraft {
                    account_id: AccountId(Uuid::nil()),
                    to: Vec::new(),
                    cc: Vec::new(),
                    bcc: Vec::new(),
                    subject: String::new(),
                    body_text: String::new(),
                    format: Default::default(),
                    request_mdn: false,
                    tracking_pixel_url: None,
                    attachments: Vec::new(),
                    inlines: Vec::new(),
                    collect_outgoing_contacts: false,
                    in_reply_to: None,
                    references: None,
                    thread_id: None,
                    approve_contact_email: None,
                    sign: false,
                    encrypt: false,
                    autocrypt_header: None,
                    identity_id: None,
                    custom_from: None,
                    imap_draft: None,
                    reaction_emoji: None,
                },
                rfc822: vec![1, 2, 3],
                message_id_hdr: "<a@b>".to_string(),
                placeholder: None,
            },
            WorkerCommand::OpenMessage(MessageId(4)),
        ];
        order_pending_worker_commands(&mut cmds);
        assert!(matches!(&cmds[0], WorkerCommand::OpenMessage(MessageId(4))));
        assert!(matches!(&cmds[1], WorkerCommand::FileSentOnImap { .. }));
        assert!(
            retry_after_command_failure(WorkerCommand::FileSentOnImap {
                draft: ComposeDraft {
                    account_id: AccountId(Uuid::nil()),
                    to: Vec::new(),
                    cc: Vec::new(),
                    bcc: Vec::new(),
                    subject: String::new(),
                    body_text: String::new(),
                    format: Default::default(),
                    request_mdn: false,
                    tracking_pixel_url: None,
                    attachments: Vec::new(),
                    inlines: Vec::new(),
                    collect_outgoing_contacts: false,
                    in_reply_to: None,
                    references: None,
                    thread_id: None,
                    approve_contact_email: None,
                    sign: false,
                    encrypt: false,
                    autocrypt_header: None,
                    identity_id: None,
                    custom_from: None,
                    imap_draft: None,
                    reaction_emoji: None,
                },
                rfc822: Vec::new(),
                message_id_hdr: String::new(),
                placeholder: None,
            })
            .is_some()
        );
    }

    #[test]
    fn worker_command_rank_open_message_first() {
        let mut cmds = vec![
            WorkerCommand::DeleteMessage(MessageId(1)),
            WorkerCommand::SetFlag {
                id: MessageId(2),
                seen: Some(true),
                flagged: None,
            },
            WorkerCommand::OpenMessage(MessageId(2)),
        ];
        order_pending_worker_commands(&mut cmds);
        assert!(matches!(&cmds[0], WorkerCommand::OpenMessage(MessageId(2))));
        assert!(matches!(
            &cmds[1],
            WorkerCommand::SetFlag {
                id: MessageId(2),
                ..
            }
        ));
        assert!(matches!(
            &cmds[2],
            WorkerCommand::DeleteMessage(MessageId(1))
        ));
    }

    #[test]
    fn worker_command_rank_set_flags_is_one() {
        assert_eq!(
            worker_command_rank(&WorkerCommand::SetFlags {
                ids: vec![MessageId(1)],
                seen: Some(true),
                flagged: None,
            }),
            1
        );
    }

    #[test]
    fn worker_command_coalesce_duplicate_open_message() {
        let mut cmds = vec![
            WorkerCommand::OpenMessage(MessageId(2)),
            WorkerCommand::OpenMessage(MessageId(2)),
            WorkerCommand::DeleteMessage(MessageId(1)),
        ];
        order_pending_worker_commands(&mut cmds);
        assert_eq!(cmds.len(), 2);
        assert!(matches!(&cmds[0], WorkerCommand::OpenMessage(MessageId(2))));
        assert!(matches!(
            &cmds[1],
            WorkerCommand::DeleteMessage(MessageId(1))
        ));
    }

    #[test]
    fn worker_command_rank_open_before_backfill() {
        let mut cmds = vec![
            WorkerCommand::BackfillFolder {
                folder: FolderId(1),
                have: 100,
            },
            WorkerCommand::SyncFolder(FolderId(1)),
            WorkerCommand::OpenMessage(MessageId(2)),
        ];
        order_pending_worker_commands(&mut cmds);
        assert!(matches!(&cmds[0], WorkerCommand::OpenMessage(MessageId(2))));
        assert!(matches!(&cmds[1], WorkerCommand::SyncFolder(FolderId(1))));
        assert!(matches!(
            &cmds[2],
            WorkerCommand::BackfillFolder {
                folder: FolderId(1),
                have: 100
            }
        ));
    }

    #[test]
    fn worker_command_rank_open_before_neighbor_prefetch() {
        let mut cmds = vec![
            WorkerCommand::PrefetchMessage(MessageId(1)),
            WorkerCommand::OpenMessage(MessageId(2)),
        ];
        order_pending_worker_commands(&mut cmds);
        assert!(matches!(&cmds[0], WorkerCommand::OpenMessage(MessageId(2))));
        assert!(matches!(
            &cmds[1],
            WorkerCommand::PrefetchMessage(MessageId(1))
        ));
    }

    #[test]
    fn worker_command_rank_prefetch_bodies_last() {
        let mut cmds = vec![
            WorkerCommand::PrefetchBodies {
                folder: FolderId(1),
            },
            WorkerCommand::BackfillFolder {
                folder: FolderId(1),
                have: 100,
            },
            WorkerCommand::FetchMessageRemainder(MessageId(3)),
            WorkerCommand::OpenMessage(MessageId(2)),
        ];
        order_pending_worker_commands(&mut cmds);
        assert!(matches!(&cmds[0], WorkerCommand::OpenMessage(MessageId(2))));
        assert!(matches!(
            &cmds[1],
            WorkerCommand::FetchMessageRemainder(MessageId(3))
        ));
        assert!(matches!(
            &cmds[2],
            WorkerCommand::BackfillFolder {
                folder: FolderId(1),
                have: 100
            }
        ));
        assert!(matches!(
            &cmds[3],
            WorkerCommand::PrefetchBodies {
                folder: FolderId(1)
            }
        ));
    }

    #[test]
    fn worker_command_coalesce_prefetch_bodies_per_folder() {
        let mut cmds = vec![
            WorkerCommand::PrefetchBodies {
                folder: FolderId(1),
            },
            WorkerCommand::PrefetchBodies {
                folder: FolderId(2),
            },
            WorkerCommand::PrefetchBodies {
                folder: FolderId(1),
            },
        ];
        coalesce_prefetch_bodies(&mut cmds);
        assert_eq!(cmds.len(), 2);
        assert!(matches!(
            &cmds[0],
            WorkerCommand::PrefetchBodies {
                folder: FolderId(1)
            }
        ));
        assert!(matches!(
            &cmds[1],
            WorkerCommand::PrefetchBodies {
                folder: FolderId(2)
            }
        ));
    }

    #[test]
    fn worker_command_coalesce_backfill_keeps_higher_have() {
        let mut cmds = vec![
            WorkerCommand::BackfillFolder {
                folder: FolderId(1),
                have: 100,
            },
            WorkerCommand::OpenMessage(MessageId(2)),
            WorkerCommand::BackfillFolder {
                folder: FolderId(1),
                have: 200,
            },
        ];
        order_pending_worker_commands(&mut cmds);
        assert_eq!(cmds.len(), 2);
        assert!(matches!(&cmds[0], WorkerCommand::OpenMessage(MessageId(2))));
        assert!(matches!(
            &cmds[1],
            WorkerCommand::BackfillFolder {
                folder: FolderId(1),
                have: 200
            }
        ));
    }

    #[test]
    fn coalesce_imap_draft_saves_keeps_latest_body_per_message_id() {
        let older = ComposeDraft {
            account_id: AccountId(Uuid::nil()),
            to: Vec::new(),
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "older".to_string(),
            body_text: String::new(),
            format: Default::default(),
            request_mdn: false,
            tracking_pixel_url: None,
            attachments: Vec::new(),
            inlines: Vec::new(),
            collect_outgoing_contacts: true,
            in_reply_to: None,
            references: None,
            thread_id: None,
            approve_contact_email: None,
            sign: false,
            encrypt: false,
            autocrypt_header: None,
            identity_id: None,
            custom_from: None,
            imap_draft: Some(sample_imap_draft(10, 1)),
            reaction_emoji: None,
        };
        let mut newer = older.clone();
        newer.subject = "newer".to_string();
        let mut other = older.clone();
        other.subject = "other".to_string();
        other.imap_draft = Some(ImapDraftRef {
            message_id: "<other@example.com>".to_string(),
            uid: 3,
            ..sample_imap_draft(3, 1)
        });
        let mut cmds = vec![
            WorkerCommand::SaveImapDraft(older),
            WorkerCommand::OpenMessage(MessageId(1)),
            WorkerCommand::SaveImapDraft(newer),
            WorkerCommand::SaveImapDraft(other),
        ];
        coalesce_imap_draft_saves(&mut cmds);
        assert_eq!(cmds.len(), 3);
        assert!(matches!(&cmds[0], WorkerCommand::OpenMessage(MessageId(1))));
        match &cmds[1] {
            WorkerCommand::SaveImapDraft(draft) => assert_eq!(draft.subject, "newer"),
            _ => panic!("expected newest save for the session"),
        }
        match &cmds[2] {
            WorkerCommand::SaveImapDraft(draft) => assert_eq!(draft.subject, "other"),
            _ => panic!("expected the other draft to stay"),
        }
    }

    #[test]
    fn finish_send_not_retried_after_disconnect() {
        assert!(
            retry_after_command_failure(WorkerCommand::FinishSend {
                draft: ComposeDraft {
                    account_id: AccountId(Uuid::nil()),
                    to: Vec::new(),
                    cc: Vec::new(),
                    bcc: Vec::new(),
                    subject: String::new(),
                    body_text: String::new(),
                    format: Default::default(),
                    request_mdn: false,
                    tracking_pixel_url: None,
                    attachments: Vec::new(),
                    inlines: Vec::new(),
                    collect_outgoing_contacts: true,
                    in_reply_to: None,
                    references: None,
                    thread_id: None,
                    approve_contact_email: None,
                    sign: false,
                    encrypt: false,
                    autocrypt_header: None,
                    identity_id: None,
                    custom_from: None,
                    imap_draft: None,
                    reaction_emoji: None,
                },
                rfc822: Vec::new(),
                message_id_hdr: String::new(),
            })
            .is_none()
        );
    }

    #[test]
    fn command_batch_skips_inbox_reselect_when_split() {
        assert!(!command_batch_reselects_inbox(true));
    }

    #[test]
    fn retry_after_command_failure_skips_send_copy_and_folder_mutations() {
        assert!(retry_after_command_failure(WorkerCommand::OpenMessage(MessageId(1))).is_some());
        assert!(retry_after_command_failure(WorkerCommand::Refresh).is_some());
        assert!(
            retry_after_command_failure(WorkerCommand::Send(ComposeDraft {
                account_id: AccountId(Uuid::nil()),
                to: Vec::new(),
                cc: Vec::new(),
                bcc: Vec::new(),
                subject: String::new(),
                body_text: String::new(),
                format: Default::default(),
                request_mdn: false,
                tracking_pixel_url: None,
                attachments: Vec::new(),
                inlines: Vec::new(),
                collect_outgoing_contacts: true,
                in_reply_to: None,
                references: None,
                thread_id: None,
                approve_contact_email: None,
                sign: false,
                encrypt: false,
                autocrypt_header: None,
                identity_id: None,
                custom_from: None,
                imap_draft: None,
                reaction_emoji: None,
            }))
            .is_none()
        );
        assert!(
            retry_after_command_failure(WorkerCommand::SaveImapDraft(ComposeDraft {
                account_id: AccountId(Uuid::nil()),
                to: Vec::new(),
                cc: Vec::new(),
                bcc: Vec::new(),
                subject: String::new(),
                body_text: String::new(),
                format: Default::default(),
                request_mdn: false,
                tracking_pixel_url: None,
                attachments: Vec::new(),
                inlines: Vec::new(),
                collect_outgoing_contacts: true,
                in_reply_to: None,
                references: None,
                thread_id: None,
                approve_contact_email: None,
                sign: false,
                encrypt: false,
                autocrypt_header: None,
                identity_id: None,
                custom_from: None,
                imap_draft: None,
                reaction_emoji: None,
            }))
            .is_none()
        );
        assert!(
            retry_after_command_failure(WorkerCommand::CopyMessageToFolder {
                id: MessageId(1),
                dest: FolderId(2),
            })
            .is_none()
        );
        assert!(
            retry_after_command_failure(WorkerCommand::MoveMessageToFolder {
                id: MessageId(1),
                dest: FolderId(2),
            })
            .is_none()
        );
        assert!(
            retry_after_command_failure(WorkerCommand::MoveMessage {
                id: MessageId(1),
                dest: SpecialUse::Junk,
            })
            .is_none()
        );
        assert!(retry_after_command_failure(WorkerCommand::DeleteMessage(MessageId(1))).is_none());
        assert!(
            retry_after_command_failure(WorkerCommand::UndeleteMessage {
                id: MessageId(1),
                dest: FolderId(2),
            })
            .is_none()
        );
        assert!(
            retry_after_command_failure(WorkerCommand::CreateFolder {
                parent: None,
                name: "Projects".into(),
            })
            .is_none()
        );
        assert!(
            retry_after_command_failure(WorkerCommand::RenameFolder {
                id: FolderId(2),
                parent: None,
                name: "Work".into(),
            })
            .is_none()
        );
        assert!(retry_after_command_failure(WorkerCommand::DeleteFolder(FolderId(2))).is_none());
        assert!(
            retry_after_command_failure(WorkerCommand::DeleteFolderOnOrBefore {
                folder_id: FolderId(1),
                through: NaiveDate::from_ymd_opt(2026, 8, 24).unwrap(),
            })
            .is_some()
        );
    }
}
