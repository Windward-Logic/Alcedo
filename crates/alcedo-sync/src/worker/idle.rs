//! Inbox IDLE session, IDLE/poll wait cycles and IDLE notifications.
#![allow(clippy::too_many_arguments)]

use super::folders::is_selected_or_descendant;
use super::mailbox_sync::{UnchangedEmit, ensure_inbox_selected, ensure_selected, sync_inbox};
use super::queue::{command_from_msg, drain_worker_msgs, run_command_batch};
use super::session::{connect_imap_shared, is_recoverable_disconnect, replace_imap_client};
use super::watched::{poll_watched_folders, prefetch_folder_bodies, watched_poll_delay};
use super::*;

pub(super) enum IdleLoopOutcome {
    Shutdown,
    Reconnect,
    Continue,
}

pub(super) enum IdleCtrlWait {
    Shutdown,
    Continue,
}

pub(super) async fn idle_wait_reconnect_or_ctrl(
    idle_ctrl_rx: &mut mpsc::Receiver<IdleControl>,
    account: &Account,
    token_cache: &Arc<Mutex<Option<String>>>,
    state: &mut WorkerImapState,
    idle_ctrl: &Arc<Mutex<Option<mpsc::Sender<IdleControl>>>>,
) -> IdleCtrlWait {
    tokio::select! {
        ctrl = idle_ctrl_rx.recv() => {
            match ctrl {
                None | Some(IdleControl::Shutdown) => {
                    *idle_ctrl.lock().await = None;
                    IdleCtrlWait::Shutdown
                }
                Some(IdleControl::UnselectIf { ack, .. }) => {
                    let _ = ack.send(());
                    IdleCtrlWait::Continue
                }
            }
        }
        _ = tokio::time::sleep(RECONNECT_DELAY) => {
            match with_quiet_keyring(connect_imap_shared(account, token_cache)).await {
                Ok(client) => replace_imap_client(state, client),
                Err(err) => {
                    warn!(account = %account.email, %err, "idle IMAP reconnect failed");
                    state.client = None;
                }
            }
            IdleCtrlWait::Continue
        }
    }
}

pub(super) async fn wait_inbox_committed(inbox_shared: &Arc<RwLock<Folder>>, parked_name: &str) {
    let deadline = tokio::time::Instant::now() + IMAP_COMMAND_TIMEOUT;
    while tokio::time::Instant::now() < deadline {
        if inbox_shared.read().await.imap_name != parked_name {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

pub(super) async fn idle_session_loop(
    account: Account,
    mut idle_ctrl_rx: mpsc::Receiver<IdleControl>,
    mut state: WorkerImapState,
    inbox_shared: Arc<RwLock<Folder>>,
    store: StoreHandle,
    events: flume::Sender<SyncEvent>,
    token_cache: Arc<Mutex<Option<String>>>,
    _auth_notified: Arc<AtomicBool>,
    server_rules: Arc<Mutex<ServerRulesSession>>,
    idle_ctrl: Arc<Mutex<Option<mpsc::Sender<IdleControl>>>>,
    command_tx: mpsc::WeakSender<WorkerMsg>,
) {
    loop {
        if state.client.is_none() {
            match idle_wait_reconnect_or_ctrl(
                &mut idle_ctrl_rx,
                &account,
                &token_cache,
                &mut state,
                &idle_ctrl,
            )
            .await
            {
                IdleCtrlWait::Shutdown => return,
                IdleCtrlWait::Continue => continue,
            }
        }

        let inbox = inbox_shared.read().await.clone();
        state.inbox = inbox.clone();
        if let Err(err) = ensure_inbox_selected(&mut state).await {
            warn!(account = %account.email, %err, "idle session SELECT failed; reconnecting");
            match idle_wait_reconnect_or_ctrl(
                &mut idle_ctrl_rx,
                &account,
                &token_cache,
                &mut state,
                &idle_ctrl,
            )
            .await
            {
                IdleCtrlWait::Shutdown => return,
                IdleCtrlWait::Continue => continue,
            }
        }

        let idle_supported = state.client_mut().capabilities().idle;
        emit_status(
            &events,
            &account,
            if idle_supported {
                "IDLE".to_string()
            } else {
                "Polling".to_string()
            },
        );

        match idle_session_run(
            &account,
            &mut state,
            &mut idle_ctrl_rx,
            &inbox_shared,
            &store,
            &events,
            &server_rules,
            idle_supported,
        )
        .await
        {
            IdleLoopOutcome::Shutdown => {
                *idle_ctrl.lock().await = None;
                return;
            }
            IdleLoopOutcome::Continue => {
                if let Some(tx) = command_tx.upgrade() {
                    let _ = tx.try_send(WorkerMsg::Command(WorkerCommand::PrefetchBodies {
                        folder: inbox.id,
                    }));
                }
            }
            IdleLoopOutcome::Reconnect => {
                tokio::time::sleep(RECONNECT_DELAY).await;
                match connect_imap_shared(&account, &token_cache).await {
                    Ok(client) => replace_imap_client(&mut state, client),
                    Err(err) => {
                        warn!(account = %account.email, %err, "idle IMAP reconnect failed");
                        state.client = None;
                    }
                }
            }
        }
    }
}

pub(super) async fn idle_session_run(
    account: &Account,
    state: &mut WorkerImapState,
    idle_ctrl_rx: &mut mpsc::Receiver<IdleControl>,
    inbox_shared: &Arc<RwLock<Folder>>,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
    server_rules: &Arc<Mutex<ServerRulesSession>>,
    idle_supported: bool,
) -> IdleLoopOutcome {
    if idle_supported {
        idle_session_idle_cycle(
            account,
            state,
            idle_ctrl_rx,
            inbox_shared,
            store,
            events,
            server_rules,
        )
        .await
    } else {
        idle_session_poll_cycle(
            account,
            state,
            idle_ctrl_rx,
            inbox_shared,
            store,
            events,
            server_rules,
        )
        .await
    }
}

pub(super) async fn idle_session_idle_cycle(
    account: &Account,
    state: &mut WorkerImapState,
    idle_ctrl_rx: &mut mpsc::Receiver<IdleControl>,
    inbox_shared: &Arc<RwLock<Folder>>,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
    server_rules: &Arc<Mutex<ServerRulesSession>>,
) -> IdleLoopOutcome {
    let caps = state.client_mut().capabilities().clone();
    let email = state.client_mut().email().to_string();
    let selected = state
        .client
        .as_ref()
        .and_then(|client| client.selected_mailbox().map(str::to_string));
    let session = state.client.take().expect("imap client").into_session();

    let mut idle = IdleSession::new(session);
    if let Err(err) = with_imap_timeout(IDLE_DONE_TIMEOUT, idle.init()).await {
        if is_recoverable_disconnect(&err) {
            return IdleLoopOutcome::Reconnect;
        }
        warn!(account = %account.email, %err, "idle init failed");
        return IdleLoopOutcome::Reconnect;
    }

    let (wait_fut, stop) = match idle.wait() {
        Ok(pair) => pair,
        Err(_) => return IdleLoopOutcome::Reconnect,
    };
    let mut idle_res = None;
    let mut catchup = false;
    let mut ctrl_msg = None;

    {
        tokio::pin!(wait_fut);
        tokio::select! {
            ctrl = idle_ctrl_rx.recv() => ctrl_msg = ctrl,
            res = &mut wait_fut => {
                idle_res = match res {
                    Ok(value) => Some(value),
                    Err(_) => return IdleLoopOutcome::Reconnect,
                };
            }
            _ = tokio::time::sleep(INBOX_IDLE_CATCHUP_INTERVAL) => catchup = true,
        }

        drop(stop);
        if idle_res.is_none() && ctrl_msg.is_none() {
            match tokio::time::timeout(IDLE_DONE_TIMEOUT, wait_fut.as_mut()).await {
                Ok(Ok(_)) => {}
                Ok(Err(_)) | Err(_) => return IdleLoopOutcome::Reconnect,
            }
        }
    }

    if let Some(ctrl) = ctrl_msg {
        let session = match tokio::time::timeout(IDLE_DONE_TIMEOUT, idle.done()).await {
            Ok(Ok(session)) => session,
            _ => return IdleLoopOutcome::Reconnect,
        };
        state.client = Some(ImapClient::from_session(session, email, caps, selected));
        match ctrl {
            IdleControl::Shutdown => return IdleLoopOutcome::Shutdown,
            IdleControl::UnselectIf {
                imap_name,
                delimiter,
                ack,
            } => {
                if let Err(err) =
                    unselect_idle_mailbox(state, &imap_name, &delimiter, store, account.id).await
                {
                    warn!(account = %account.email, %err, "idle unselect failed");
                }
                let parked_name = inbox_shared.read().await.imap_name.clone();
                let _ = ack.send(());
                wait_inbox_committed(inbox_shared, &parked_name).await;
                return IdleLoopOutcome::Continue;
            }
        }
    }

    let session = match tokio::time::timeout(IDLE_DONE_TIMEOUT, idle.done()).await {
        Ok(Ok(session)) => session,
        Ok(Err(err)) if is_recoverable_disconnect(&err) => return IdleLoopOutcome::Reconnect,
        Ok(Err(err)) => {
            warn!(account = %account.email, %err, "idle done failed");
            return IdleLoopOutcome::Reconnect;
        }
        Err(_) => return IdleLoopOutcome::Reconnect,
    };
    state.client = Some(ImapClient::from_session(session, email, caps, selected));

    let notification = idle_sync_notification(idle_res, catchup);
    if let Some(notification) = notification {
        let inbox = inbox_shared.read().await.clone();
        let synced = {
            let rules_guard = server_rules.lock().await;
            handle_idle_notification(
                state,
                notification,
                account,
                &inbox,
                store,
                events,
                &rules_guard,
            )
            .await
            .is_ok()
        };
        if synced
            && let Err(err) = prefetch_folder_bodies(state, account, inbox.id, store, events).await
        {
            warn!(
                account = %account.email,
                %err,
                "idle inbox body prefetch after sync failed"
            );
        }
    }

    IdleLoopOutcome::Continue
}

pub(super) async fn idle_session_poll_cycle(
    account: &Account,
    state: &mut WorkerImapState,
    idle_ctrl_rx: &mut mpsc::Receiver<IdleControl>,
    inbox_shared: &Arc<RwLock<Folder>>,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
    server_rules: &Arc<Mutex<ServerRulesSession>>,
) -> IdleLoopOutcome {
    tokio::select! {
        ctrl = idle_ctrl_rx.recv() => {
            match ctrl {
                None | Some(IdleControl::Shutdown) => IdleLoopOutcome::Shutdown,
                Some(IdleControl::UnselectIf { imap_name, delimiter, ack }) => {
                    if let Err(err) = unselect_idle_mailbox(
                        state,
                        &imap_name,
                        &delimiter,
                        store,
                        account.id,
                    )
                    .await
                    {
                        warn!(account = %account.email, %err, "idle unselect failed");
                    }
                    let parked_name = inbox_shared.read().await.imap_name.clone();
                    let _ = ack.send(());
                    wait_inbox_committed(inbox_shared, &parked_name).await;
                    IdleLoopOutcome::Continue
                }
            }
        }
        _ = tokio::time::sleep(POLL_INTERVAL) => {
            let inbox = inbox_shared.read().await.clone();
            let rules_guard = server_rules.lock().await;
            if ensure_inbox_selected(state).await.is_ok() {
                let sync_result =
                    sync_inbox(
                        state,
                        account,
                        &inbox,
                        store,
                        events,
                        &rules_guard,
                        UnchangedEmit::Skip,
                    )
                    .await;
                if let Err(err) = sync_result {
                    warn!(account = %account.email, %err, "idle poll sync failed");
                    return IdleLoopOutcome::Reconnect;
                }
                let _ = emit_unread_counts(store, events).await;
            }
            IdleLoopOutcome::Continue
        }
    }
}

pub(super) async fn unselect_idle_mailbox(
    state: &mut WorkerImapState,
    imap_name: &str,
    delimiter: &str,
    store: &StoreHandle,
    account_id: AccountId,
) -> Result<()> {
    if !is_selected_or_descendant(state.selected_on_server(), imap_name, delimiter) {
        return Ok(());
    }
    if state.client_mut().capabilities().unselect && state.client_mut().unselect().await.is_ok() {
        return Ok(());
    }
    let folders = store.list_folders(account_id).await?;
    let selected = state.selected_on_server().to_string();
    if let Some(alt) = folders
        .iter()
        .find(|folder| folder.selectable && folder.imap_name != selected)
    {
        ensure_selected(state, &alt.imap_name).await?;
    }
    Ok(())
}

pub(super) async fn park_idle_for_mailbox(
    idle_ctrl: &Arc<Mutex<Option<mpsc::Sender<IdleControl>>>>,
    idle_inbox: &Folder,
    imap_name: &str,
    delimiter: &str,
) -> Result<()> {
    let tx = idle_ctrl.lock().await.clone();
    let Some(tx) = tx else {
        return Ok(());
    };
    if !is_selected_or_descendant(&idle_inbox.imap_name, imap_name, delimiter) {
        return Ok(());
    }
    let (ack_tx, ack_rx) = oneshot::channel();
    tx.send(IdleControl::UnselectIf {
        imap_name: imap_name.to_string(),
        delimiter: delimiter.to_string(),
        ack: ack_tx,
    })
    .await
    .map_err(|_| Error::Imap("idle control channel closed".to_string()))?;
    match tokio::time::timeout(IMAP_COMMAND_TIMEOUT, ack_rx).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) => Err(Error::Imap("idle unselect ack dropped".to_string())),
        Err(_) => Err(Error::Imap("idle unselect timed out".to_string())),
    }
}

pub(super) async fn commit_worker_inbox(
    state: &mut WorkerImapState,
    inbox: Folder,
    split: Option<&SplitImapContext>,
) {
    state.inbox = inbox.clone();
    if let Some(split) = split {
        *split.inbox_shared.write().await = inbox;
    }
}

pub(super) async fn idle_wait_cycle(
    state: &mut WorkerImapState,
    account: &Account,
    msg_rx: &mut mpsc::Receiver<WorkerMsg>,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
    auth_notified: &mut bool,
    server_rules: &mut ServerRulesSession,
    split: &SplitImapContext,
) -> Result<WaitOutcome> {
    let caps = state.client_mut().capabilities().clone();
    let email = state.client_mut().email().to_string();
    let selected = state
        .client
        .as_ref()
        .and_then(|client| client.selected_mailbox().map(str::to_string));
    let session = state.client.take().expect("imap client").into_session();

    let mut idle = IdleSession::new(session);
    with_imap_timeout(IDLE_DONE_TIMEOUT, idle.init()).await?;

    let (wait_fut, stop) = idle.wait()?;

    let mut ui_msg = None;
    let mut idle_res = None;
    let mut got_channel_event = false;
    let mut catchup = false;

    {
        tokio::pin!(wait_fut);

        tokio::select! {
            msg = msg_rx.recv() => {
                ui_msg = msg;
                got_channel_event = true;
            }
            res = &mut wait_fut => idle_res = Some(res?),
            _ = tokio::time::sleep(INBOX_IDLE_CATCHUP_INTERVAL) => {
                catchup = true;
            }
        }

        drop(stop);
        if idle_res.is_none() {
            match tokio::time::timeout(IDLE_DONE_TIMEOUT, wait_fut.as_mut()).await {
                Ok(Ok(_)) => {}
                Ok(Err(_err)) => {
                    return Ok(WaitOutcome::Reconnect {
                        retry: command_from_msg(ui_msg).into_iter().collect(),
                    });
                }
                Err(_) => {
                    debug!(
                        account = %account.email,
                        "IDLE interrupt timed out; treating session as dead"
                    );
                    return Ok(WaitOutcome::Reconnect {
                        retry: command_from_msg(ui_msg).into_iter().collect(),
                    });
                }
            }
        }
    }

    let session = match tokio::time::timeout(IDLE_DONE_TIMEOUT, idle.done()).await {
        Ok(Ok(session)) => session,
        Ok(Err(err)) => {
            if is_recoverable_disconnect(&err) {
                return Ok(WaitOutcome::Reconnect {
                    retry: command_from_msg(ui_msg).into_iter().collect(),
                });
            }
            return Err(err);
        }
        Err(_) => {
            return Ok(WaitOutcome::Reconnect {
                retry: command_from_msg(ui_msg).into_iter().collect(),
            });
        }
    };
    state.client = Some(ImapClient::from_session(session, email, caps, selected));

    if got_channel_event {
        match ui_msg {
            Some(WorkerMsg::Shutdown) | None => return Ok(WaitOutcome::Shutdown),
            Some(WorkerMsg::Command(cmd)) => {
                let cmds = match drain_worker_msgs(msg_rx, cmd) {
                    Ok(cmds) => cmds,
                    Err(outcome) => return Ok(outcome),
                };
                return run_command_batch(
                    cmds,
                    msg_rx,
                    state,
                    account,
                    store,
                    events,
                    auth_notified,
                    server_rules,
                    split,
                )
                .await;
            }
        }
    }

    let notification = idle_sync_notification(idle_res, catchup);
    if let Some(notification) = notification {
        if catchup {
            debug!(account = %account.email, "idle catch-up sync");
        }
        let inbox = state.inbox.clone();
        let idle_result = handle_idle_notification(
            state,
            notification,
            account,
            &inbox,
            store,
            events,
            server_rules,
        )
        .await;
        if idle_result.is_ok()
            && let Err(err) = prefetch_folder_bodies(state, account, inbox.id, store, events).await
        {
            warn!(
                account = %account.email,
                %err,
                "idle inbox body prefetch after sync failed"
            );
        }
        idle_result?;
        if catchup {
            poll_watched_folders(state, account, store, events, true).await?;
        }
    }

    Ok(WaitOutcome::Continue)
}

pub(super) fn idle_sync_notification(
    idle_res: Option<IdleWaitResult>,
    catchup: bool,
) -> Option<IdleNotification> {
    match idle_res {
        Some(IdleWaitResult::Notification(notification)) => Some(notification),
        Some(IdleWaitResult::Timeout) => Some(IdleNotification::Updated),
        Some(IdleWaitResult::Interrupted) | None if catchup => Some(IdleNotification::Updated),
        Some(IdleWaitResult::Interrupted) | None => None,
    }
}

pub(super) async fn poll_wait_cycle(
    state: &mut WorkerImapState,
    account: &Account,
    msg_rx: &mut mpsc::Receiver<WorkerMsg>,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
    auth_notified: &mut bool,
    server_rules: &mut ServerRulesSession,
    split: &SplitImapContext,
) -> Result<WaitOutcome> {
    let poll_delay = watched_poll_delay(&state.watched);
    tokio::select! {
        msg = msg_rx.recv() => {
            match msg {
                Some(WorkerMsg::Shutdown) | None => return Ok(WaitOutcome::Shutdown),
                Some(WorkerMsg::Command(cmd)) => {
                    let cmds = match drain_worker_msgs(msg_rx, cmd) {
                        Ok(cmds) => cmds,
                        Err(outcome) => return Ok(outcome),
                    };
                    if !state.session_alive_for_command(account).await {
                        return Ok(WaitOutcome::Reconnect { retry: cmds });
                    }
                    let outcome = run_command_batch(
                        cmds,
                        msg_rx,
                        state,
                        account,
                        store,
                        events,
                        auth_notified,
                        server_rules,
                        split,
                    )
                    .await;
                    if matches!(outcome, Ok(WaitOutcome::Continue)) {
                        state.touch_io();
                    }
                    return outcome;
                }
            }
        }
        _ = tokio::time::sleep(poll_delay) => {
            ensure_inbox_selected(state).await?;
            let inbox = state.inbox.clone();
            let sync_result =
                sync_inbox(
                state,
                account,
                &inbox,
                store,
                events,
                server_rules,
                UnchangedEmit::Skip,
            )
            .await;
            sync_result?;
            emit_unread_counts(store, events).await?;
            poll_watched_folders(state, account, store, events, true).await?;
            state.touch_io();
        }
    }

    Ok(WaitOutcome::Continue)
}

pub(super) async fn handle_idle_notification(
    state: &mut WorkerImapState,
    notification: IdleNotification,
    account: &Account,
    inbox: &Folder,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
    server_rules: &ServerRulesSession,
) -> Result<()> {
    debug!(?notification, account = %account.email, "idle notification");
    let _ = notification;
    sync_inbox(
        state,
        account,
        inbox,
        store,
        events,
        server_rules,
        UnchangedEmit::Skip,
    )
    .await?;
    emit_unread_counts(store, events).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_timeout_and_catchup_sync_inbox() {
        assert!(idle_sync_notification(Some(IdleWaitResult::Timeout), false).is_some());
        assert!(idle_sync_notification(Some(IdleWaitResult::Interrupted), true).is_some());
        assert!(idle_sync_notification(None, true).is_some());
        assert!(idle_sync_notification(Some(IdleWaitResult::Interrupted), false).is_none());
        assert!(idle_sync_notification(None, false).is_none());
        assert_eq!(
            idle_sync_notification(
                Some(IdleWaitResult::Notification(IdleNotification::Exists(3))),
                false
            ),
            Some(IdleNotification::Exists(3))
        );
    }
}
