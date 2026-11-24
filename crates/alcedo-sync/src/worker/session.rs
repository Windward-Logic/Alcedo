//! Account worker lifecycle: auth parking, the command session loop, the SMTP task and the initial connect.
#![allow(clippy::too_many_arguments)]

use super::auth::connect_imap;
use super::idle::{idle_session_loop, idle_wait_cycle, poll_wait_cycle};
use super::mailbox_sync::{UnchangedEmit, sync_folders, sync_inbox};
use super::queue::{command_batch_reselects_inbox, drain_worker_msgs, run_command_batch};
use super::rules::probe_and_maybe_upload;
use super::send::smtp_send_compose;
use super::watched::{poll_watched_folders, prime_watched_folders, watched_poll_delay};
use super::*;
use alcedo_i18n::{tr, trf};

pub(super) fn is_recoverable_disconnect(err: &Error) -> bool {
    let message = match err {
        Error::Imap(msg) | Error::Network(msg) => msg.to_ascii_lowercase(),
        _ => return false,
    };
    message.contains("connection reset")
        || message.contains("broken pipe")
        || message.contains("connection timed out")
        || message.contains("connection aborted")
        || message.contains("unexpected eof")
        || message.contains("unexpected-eof")
        || message.contains("close_notify")
        || message.contains("peer closed connection")
        || message.contains("idle done failed")
        // A server BYE, idle timeout or restart that closes TLS cleanly reads
        // as EOF: async-imap reports `ConnectionLost`, tokio reports EOF.
        || message.contains("connection lost")
        || message.contains("unexpected end of file")
}

#[cfg(test)]
mod disconnect_tests {
    use super::*;

    #[test]
    fn fetch_timeout_is_recognised_without_being_a_disconnect() {
        let err = Error::Imap(FETCH_TIMED_OUT.to_string());
        assert!(is_fetch_timeout(&err));
        assert!(!is_recoverable_disconnect(&err));
        assert!(!is_fetch_timeout(&Error::Imap(
            "connection timed out".into()
        )));
    }

    #[test]
    fn clean_server_close_is_recoverable() {
        assert!(is_recoverable_disconnect(&Error::Imap(
            "UID FETCH 5: connection lost".into()
        )));
        assert!(is_recoverable_disconnect(&Error::Imap(
            "APPEND Sent: connection lost".into()
        )));
        assert!(is_recoverable_disconnect(&Error::Imap(
            "UID STORE 5: io: unexpected end of file".into()
        )));
        assert!(!is_recoverable_disconnect(&Error::Imap(
            "UID STORE 5: no response: invalid flag".into()
        )));
    }
}

/// True when a FETCH was cancelled by its timeout.
///
/// Not a lost connection, so the command is not retried (that would loop on a
/// message that never arrives). But the cancelled command's responses are
/// still on the wire, and the next command would have to read through them,
/// so the session must be dropped and reconnected.
pub(super) fn is_fetch_timeout(err: &Error) -> bool {
    matches!(err, Error::Imap(msg) if msg == FETCH_TIMED_OUT)
}

/// True when the session has been quiet long enough that a sleep/wake or a NAT
/// timeout may have killed it without the kernel noticing.
pub(super) fn liveness_probe_due(last_io: SystemTime, now: SystemTime) -> bool {
    // A clock that went backwards is treated as "probe", it costs one NOOP.
    now.duration_since(last_io)
        .map_or(true, |idle| idle >= LIVENESS_IDLE_THRESHOLD)
}

pub(super) fn is_smtp_worker_command(cmd: &WorkerCommand) -> bool {
    matches!(cmd, WorkerCommand::Send(_))
}

pub(super) fn idle_task_needs_spawn(task: &Option<tokio::task::JoinHandle<()>>) -> bool {
    task.as_ref().is_none_or(|t| t.is_finished())
}

pub(super) fn session_ready_status(auth_notified: bool, idle_supported: bool) -> String {
    if auth_notified {
        "Sign-in required".to_string()
    } else if idle_supported {
        "IDLE".to_string()
    } else {
        "Polling".to_string()
    }
}

pub(super) fn replace_imap_client(state: &mut WorkerImapState, client: ImapClient) {
    state.client = Some(client);
    state.touch_io();
}

pub(super) enum AuthPark {
    Shutdown,
    Continue,
}

/// Park after a sign-in failure.
///
/// A command wakes the worker immediately and may prompt to unlock the keyring.
/// A timer wake retries without another keyring dialog, so a keyring that
/// unlocks after login can recover on its own.
pub(super) async fn park_for_auth_retry(
    msg_rx: &mut mpsc::Receiver<WorkerMsg>,
    email: &str,
    retry_cmd: &mut Vec<WorkerCommand>,
    attempt: &mut u32,
    keyring_quiet: &mut bool,
    keyring_locked: bool,
) -> AuthPark {
    // A locked keyring is a local check, so keep retrying quickly. A rejected
    // password backs off so the provider is not hit on a tight loop.
    let delay = if keyring_locked {
        Duration::from_secs(15)
    } else {
        auth_retry_delay(*attempt)
    };
    tokio::select! {
        biased;
        msg = msg_rx.recv() => match msg {
            None | Some(WorkerMsg::Shutdown) => return AuthPark::Shutdown,
            Some(WorkerMsg::Command(cmd)) => {
                *retry_cmd = vec![cmd];
                *attempt = 0;
                *keyring_quiet = false;
            }
        },
        _ = tokio::time::sleep(delay) => {
            debug!(
                account = email,
                attempt = *attempt,
                delay_secs = delay.as_secs(),
                keyring_locked,
                "retrying after sign-in failure"
            );
            if !keyring_locked {
                *attempt = attempt.saturating_add(1);
            }
            *keyring_quiet = true;
        }
    }
    AuthPark::Continue
}

pub(super) async fn account_worker_loop(
    account: Account,
    mut msg_rx: mpsc::Receiver<WorkerMsg>,
    idle_ctrl_rx: mpsc::Receiver<IdleControl>,
    idle_ctrl: Arc<Mutex<Option<mpsc::Sender<IdleControl>>>>,
    store: StoreHandle,
    events: flume::Sender<SyncEvent>,
    token_cache: Arc<Mutex<Option<String>>>,
    auth_notified: Arc<AtomicBool>,
    server_rules: Arc<Mutex<ServerRulesSession>>,
    runtime_handle: Handle,
    command_tx: mpsc::WeakSender<WorkerMsg>,
    search_import_generation: Arc<AtomicU64>,
) {
    let mut retry_cmd: Vec<WorkerCommand> = Vec::new();
    let mut watched = WatchedFolders::default();
    let mut idle_split;
    let mut idle_task: Option<tokio::task::JoinHandle<()>> = None;
    let mut inbox_shared_slot: Option<Arc<RwLock<Folder>>> = None;
    let mut idle_ctrl_rx = Some(idle_ctrl_rx);
    let mut auth_attempt: u32 = 0;
    let mut keyring_quiet = false;

    loop {
        emit_status(&events, &account, tr!("Syncing…"));

        let mut token_guard = token_cache.lock().await;
        let mut rules_guard = server_rules.lock().await;
        let connect = connect_and_initial_sync(
            &account,
            &store,
            &events,
            &mut token_guard,
            &mut rules_guard,
        );
        let connected = if keyring_quiet {
            with_quiet_keyring(connect).await
        } else {
            connect.await
        };
        let mut state = match connected {
            Ok(mut state) => {
                state.search_import_generation = search_import_generation.clone();
                auth_notified.store(false, Ordering::Relaxed);
                auth_attempt = 0;
                keyring_quiet = false;
                state
            }
            Err(err) if is_auth_failure(&err) => {
                if !auth_notified.load(Ordering::Relaxed) {
                    let _ = events.send(SyncEvent::AuthRequired {
                        account: account.id,
                        message: err.to_string(),
                    });
                    auth_notified.store(true, Ordering::Relaxed);
                }
                emit_status(&events, &account, "Sign-in required".to_string());
                drop(rules_guard);
                drop(token_guard);
                if matches!(
                    park_for_auth_retry(
                        &mut msg_rx,
                        &account.email,
                        &mut retry_cmd,
                        &mut auth_attempt,
                        &mut keyring_quiet,
                        is_keyring_auth_message(&err.to_string()),
                    )
                    .await,
                    AuthPark::Shutdown
                ) {
                    return;
                }
                continue;
            }
            Err(err) => {
                emit_error(&events, Some(account.id), err.to_string());
                drop(rules_guard);
                drop(token_guard);
                tokio::time::sleep(RECONNECT_DELAY).await;
                continue;
            }
        };
        state.watched = watched.clone();
        drop(rules_guard);
        drop(token_guard);

        retry_cmd.push(WorkerCommand::PrefetchBodies {
            folder: state.inbox.id,
        });

        emit_status(
            &events,
            &account,
            session_ready_status(
                auth_notified.load(Ordering::Relaxed),
                state.client_mut().capabilities().idle,
            ),
        );

        if let Err(err) = prime_watched_folders(&mut state, &account, &store, &events, true).await {
            warn!(account = %account.email, %err, "startup watched folder sweep failed");
            if is_recoverable_disconnect(&err) {
                emit_status(&events, &account, tr!("Reconnecting…"));
                tokio::time::sleep(RECONNECT_DELAY).await;
                continue;
            }
            if is_auth_failure(&err) {
                if !auth_notified.load(Ordering::Relaxed) {
                    let _ = events.send(SyncEvent::AuthRequired {
                        account: account.id,
                        message: err.to_string(),
                    });
                    auth_notified.store(true, Ordering::Relaxed);
                }
                emit_status(&events, &account, "Sign-in required".to_string());
                if matches!(
                    park_for_auth_retry(
                        &mut msg_rx,
                        &account.email,
                        &mut retry_cmd,
                        &mut auth_attempt,
                        &mut keyring_quiet,
                        is_keyring_auth_message(&err.to_string()),
                    )
                    .await,
                    AuthPark::Shutdown
                ) {
                    return;
                }
                continue;
            }
        }

        let inbox_shared = match inbox_shared_slot.clone() {
            Some(shared) => {
                *shared.write().await = state.inbox.clone();
                shared
            }
            None => {
                let shared = Arc::new(RwLock::new(state.inbox.clone()));
                inbox_shared_slot = Some(shared.clone());
                shared
            }
        };
        if idle_task_needs_spawn(&idle_task) && idle_task.is_some() {
            idle_task = None;
        }
        if idle_task_needs_spawn(&idle_task) {
            match connect_imap_shared(&account, &token_cache).await {
                Ok(idle_client) => {
                    info!(
                        account = %account.email,
                        "second IMAP session connected; split idle/command"
                    );
                    idle_split = true;
                    let idle_state = WorkerImapState {
                        client: Some(idle_client),
                        inbox: state.inbox.clone(),
                        recent_draft_append: None,
                        unresolved_draft_append: None,
                        closed_compose_sessions: Vec::new(),
                        pending_sent: Vec::new(),
                        watched: WatchedFolders::default(),
                        search_import_generation: search_import_generation.clone(),
                        last_io: SystemTime::now(),
                    };
                    let rx = if let Some(rx) = idle_ctrl_rx.take() {
                        rx
                    } else {
                        let (tx, rx) = mpsc::channel(4);
                        *idle_ctrl.lock().await = Some(tx);
                        rx
                    };
                    let task = runtime_handle.spawn(idle_session_loop(
                        account.clone(),
                        rx,
                        idle_state,
                        inbox_shared.clone(),
                        store.clone(),
                        events.clone(),
                        token_cache.clone(),
                        auth_notified.clone(),
                        server_rules.clone(),
                        idle_ctrl.clone(),
                        command_tx.clone(),
                    ));
                    idle_task = Some(task);
                }
                Err(err) => {
                    info!(
                        account = %account.email,
                        %err,
                        "second IMAP connect failed; single-session fallback"
                    );
                    idle_split = false;
                    *idle_ctrl.lock().await = None;
                    idle_ctrl_rx = None;
                }
            }
        } else {
            idle_split = true;
        }

        let split_ctx = SplitImapContext {
            idle_ctrl: idle_ctrl.clone(),
            inbox_shared: inbox_shared.clone(),
            reselect_inbox: command_batch_reselects_inbox(idle_split),
        };

        if !retry_cmd.is_empty() {
            let mut rules_guard = server_rules.lock().await;
            let mut auth_flag = auth_notified.load(Ordering::Relaxed);
            match run_command_batch(
                std::mem::take(&mut retry_cmd),
                &mut msg_rx,
                &mut state,
                &account,
                &store,
                &events,
                &mut auth_flag,
                &mut rules_guard,
                &split_ctx,
            )
            .await
            {
                Ok(WaitOutcome::Continue) => {}
                Ok(WaitOutcome::Shutdown) => return,
                Ok(WaitOutcome::Reconnect { retry }) => {
                    info!(
                        account = %account.email,
                        retry = !retry.is_empty(),
                        "IMAP still down after reconnect; retrying"
                    );
                    retry_cmd = retry;
                    emit_status(&events, &account, tr!("Reconnecting…"));
                    auth_notified.store(auth_flag, Ordering::Relaxed);
                    drop(rules_guard);
                    tokio::time::sleep(RECONNECT_DELAY).await;
                    continue;
                }
                Err(err) => {
                    if is_auth_failure(&err) && !auth_notified.load(Ordering::Relaxed) {
                        let _ = events.send(SyncEvent::AuthRequired {
                            account: account.id,
                            message: err.to_string(),
                        });
                        auth_notified.store(true, Ordering::Relaxed);
                    } else if !is_auth_failure(&err) {
                        emit_error(&events, Some(account.id), err.to_string());
                    }
                }
            }
            auth_notified.store(auth_flag, Ordering::Relaxed);
            drop(rules_guard);
        }

        loop {
            let idle_supported = state.client_mut().capabilities().idle;
            emit_status(
                &events,
                &account,
                session_ready_status(auth_notified.load(Ordering::Relaxed), idle_supported),
            );
            let wait_result = if idle_split {
                command_session_loop(
                    &mut msg_rx,
                    &mut state,
                    &account,
                    &store,
                    &events,
                    &auth_notified,
                    &server_rules,
                    &split_ctx,
                )
                .await
            } else {
                let mut rules_guard = server_rules.lock().await;
                let mut auth_flag = auth_notified.load(Ordering::Relaxed);
                let result = if idle_supported {
                    idle_wait_cycle(
                        &mut state,
                        &account,
                        &mut msg_rx,
                        &store,
                        &events,
                        &mut auth_flag,
                        &mut rules_guard,
                        &split_ctx,
                    )
                    .await
                } else {
                    poll_wait_cycle(
                        &mut state,
                        &account,
                        &mut msg_rx,
                        &store,
                        &events,
                        &mut auth_flag,
                        &mut rules_guard,
                        &split_ctx,
                    )
                    .await
                };
                auth_notified.store(auth_flag, Ordering::Relaxed);
                drop(rules_guard);
                result
            };

            match wait_result {
                Ok(WaitOutcome::Shutdown) => return,
                Ok(WaitOutcome::Continue) => {}
                Ok(WaitOutcome::Reconnect { retry }) => {
                    info!(
                        account = %account.email,
                        retry = !retry.is_empty(),
                        "IMAP session dead; reconnecting"
                    );
                    retry_cmd = retry;
                    emit_status(&events, &account, tr!("Reconnecting…"));
                    tokio::time::sleep(RECONNECT_DELAY).await;
                    break;
                }
                Err(err) if is_auth_failure(&err) => {
                    if !auth_notified.load(Ordering::Relaxed) {
                        let _ = events.send(SyncEvent::AuthRequired {
                            account: account.id,
                            message: err.to_string(),
                        });
                        auth_notified.store(true, Ordering::Relaxed);
                    }
                    emit_status(&events, &account, "Sign-in required".to_string());
                    if matches!(
                        park_for_auth_retry(
                            &mut msg_rx,
                            &account.email,
                            &mut retry_cmd,
                            &mut auth_attempt,
                            &mut keyring_quiet,
                            is_keyring_auth_message(&err.to_string()),
                        )
                        .await,
                        AuthPark::Shutdown
                    ) {
                        return;
                    }
                    break;
                }
                Err(err) => {
                    if is_recoverable_disconnect(&err) {
                        info!(
                            account = %account.email,
                            %err,
                            "IMAP connection dropped; reconnecting"
                        );
                        emit_status(&events, &account, tr!("Reconnecting…"));
                    } else {
                        emit_error(&events, Some(account.id), err.to_string());
                    }
                    tokio::time::sleep(RECONNECT_DELAY).await;
                    break;
                }
            }
        }
        watched = state.watched.clone();
    }
}

pub(super) async fn connect_imap_shared(
    account: &Account,
    token_cache: &Arc<Mutex<Option<String>>>,
) -> Result<ImapClient> {
    let mut cache = token_cache.lock().await;
    connect_imap(account, &mut cache).await
}

pub(super) async fn smtp_worker_loop(
    account: Account,
    mut smtp_rx: mpsc::Receiver<WorkerMsg>,
    imap_tx: mpsc::Sender<WorkerMsg>,
    store: StoreHandle,
    events: flume::Sender<SyncEvent>,
    token_cache: Arc<Mutex<Option<String>>>,
    auth_notified: Arc<AtomicBool>,
) {
    while let Some(msg) = smtp_rx.recv().await {
        match msg {
            WorkerMsg::Shutdown => return,
            WorkerMsg::Command(cmd) => match cmd {
                WorkerCommand::Send(draft) => {
                    let mut cache = token_cache.lock().await.clone();
                    let mut auth_flag = auth_notified.load(Ordering::Relaxed);
                    match smtp_send_compose(
                        &account,
                        draft,
                        &store,
                        &events,
                        &mut cache,
                        &mut auth_flag,
                    )
                    .await
                    {
                        Ok(finish) => {
                            auth_notified.store(auth_flag, Ordering::Relaxed);
                            *token_cache.lock().await = cache;
                            if imap_tx
                                .send(WorkerMsg::Command(WorkerCommand::FinishSend {
                                    draft: finish.draft,
                                    rfc822: finish.rfc822,
                                    message_id_hdr: finish.message_id_hdr,
                                }))
                                .await
                                .is_err()
                            {
                                warn!(
                                    account = %account.email,
                                    "IMAP worker gone; FinishSend dropped after SMTP success"
                                );
                            }
                        }
                        Err(_) => {
                            auth_notified.store(auth_flag, Ordering::Relaxed);
                            *token_cache.lock().await = cache;
                        }
                    }
                }
                other => {
                    warn!(
                        account = %account.email,
                        command = ?other,
                        "unexpected command on SMTP worker channel"
                    );
                }
            },
        }
    }
}

pub(super) async fn command_session_loop(
    msg_rx: &mut mpsc::Receiver<WorkerMsg>,
    state: &mut WorkerImapState,
    account: &Account,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
    auth_notified: &Arc<AtomicBool>,
    server_rules: &Arc<Mutex<ServerRulesSession>>,
    split: &SplitImapContext,
) -> Result<WaitOutcome> {
    loop {
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
                        let mut rules_guard = server_rules.lock().await;
                        let mut auth_flag = auth_notified.load(Ordering::Relaxed);
                        let outcome = run_command_batch(
                            cmds,
                            msg_rx,
                            state,
                            account,
                            store,
                            events,
                            &mut auth_flag,
                            &mut rules_guard,
                            split,
                        )
                        .await;
                        auth_notified.store(auth_flag, Ordering::Relaxed);
                        if matches!(outcome, Ok(WaitOutcome::Continue)) {
                            state.touch_io();
                        }
                        return outcome;
                    }
                }
            }
            _ = tokio::time::sleep(poll_delay) => {
                if auth_notified.load(Ordering::Relaxed) {
                    state.watched.last_poll = Some(Instant::now());
                    continue;
                }
                match with_imap_timeout(IMAP_COMMAND_TIMEOUT, state.client_mut().noop()).await {
                    Ok(()) => {
                        state.touch_io();
                        poll_watched_folders(state, account, store, events, false).await?;
                    }
                    Err(err) if is_recoverable_disconnect(&err) => {
                        return Ok(WaitOutcome::Reconnect { retry: Vec::new() });
                    }
                    Err(err) if is_auth_failure(&err) => {
                        state.watched.last_poll = Some(Instant::now());
                        if !auth_notified.load(Ordering::Relaxed) {
                            let _ = events.send(SyncEvent::AuthRequired {
                                account: account.id,
                                message: err.to_string(),
                            });
                            auth_notified.store(true, Ordering::Relaxed);
                        }
                    }
                    Err(err) => {
                        state.watched.last_poll = Some(Instant::now());
                        emit_error(events, Some(account.id), err.to_string());
                    }
                }
            }
        }
    }
}

pub(super) async fn connect_and_initial_sync(
    account: &Account,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
    token_cache: &mut Option<String>,
    server_rules: &mut ServerRulesSession,
) -> Result<WorkerImapState> {
    emit_status(events, account, tr!("Connecting…"));
    let mut client = connect_imap(account, token_cache).await?;
    emit_status(events, account, tr!("Listing folders…"));
    let inbox = sync_folders(&mut client, account, store, events).await?;
    let mut state = WorkerImapState {
        inbox: inbox.clone(),
        client: Some(client),
        recent_draft_append: None,
        unresolved_draft_append: None,
        closed_compose_sessions: Vec::new(),
        pending_sent: Vec::new(),
        watched: WatchedFolders::default(),
        search_import_generation: Arc::new(AtomicU64::new(0)),
        last_io: SystemTime::now(),
    };

    if !server_rules.probed {
        probe_and_maybe_upload(account, store, events, server_rules).await?;
    }

    emit_status(
        events,
        account,
        trf!("Syncing {folder}…", folder = folder_display_name(&inbox)),
    );
    sync_inbox(
        &mut state,
        account,
        &inbox,
        store,
        events,
        server_rules,
        UnchangedEmit::Always,
    )
    .await?;
    emit_unread_counts(store, events).await?;

    Ok(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worker::queue::command_batch_reselects_inbox;
    use alcedo_core::{AccountId, MessageId};
    use uuid::Uuid;

    #[test]
    fn idle_connection_reset_is_recoverable() {
        let err = Error::Imap(
            "IDLE DONE failed: io: Connection reset by peer (os error 104)".to_string(),
        );
        assert!(is_recoverable_disconnect(&err));
        assert!(is_recoverable_disconnect(&Error::Imap(
            "connection timed out".into()
        )));
        assert!(is_recoverable_disconnect(&Error::Network(
            "connection reset by peer".into()
        )));
        assert!(is_recoverable_disconnect(&Error::Imap(
            "NOOP: io: peer closed connection without sending TLS close_notify: https://docs.rs/rustls/latest/rustls/manual/_03_howto/index.html#unexpected-eof".into()
        )));
        assert!(!is_recoverable_disconnect(&Error::Imap(
            "FETCH timed out".into()
        )));
        assert!(!is_recoverable_disconnect(&Error::Smtp(
            "connection reset by peer".into()
        )));
        assert!(!is_recoverable_disconnect(&Error::Store(
            "broken pipe".into()
        )));
        assert!(!is_recoverable_disconnect(&Error::Auth("expired".into())));
    }

    #[test]
    fn liveness_probe_due_only_after_idle_threshold() {
        let last = SystemTime::now();
        assert!(!liveness_probe_due(last, last + Duration::from_secs(5)));
        assert!(liveness_probe_due(last, last + LIVENESS_IDLE_THRESHOLD));
        assert!(liveness_probe_due(last, last + Duration::from_secs(3600)));
        // Clock stepped backwards: probe rather than trust the session.
        assert!(liveness_probe_due(last + Duration::from_secs(60), last));
    }

    #[test]
    fn is_smtp_worker_command_send_only() {
        assert!(is_smtp_worker_command(&WorkerCommand::Send(ComposeDraft {
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
        })));
        assert!(!is_smtp_worker_command(&WorkerCommand::OpenMessage(
            MessageId(1)
        )));
        assert!(!is_smtp_worker_command(&WorkerCommand::FinishSend {
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
        }));
    }

    #[test]
    fn command_batch_reselects_inbox_when_not_split() {
        assert!(command_batch_reselects_inbox(false));
        assert!(idle_task_needs_spawn(&None));
    }

    #[test]
    fn session_ready_status_maps_auth_and_idle() {
        assert_eq!(session_ready_status(false, true), "IDLE");
        assert_eq!(session_ready_status(false, false), "Polling");
        assert_eq!(session_ready_status(true, true), "Sign-in required");
    }
}
