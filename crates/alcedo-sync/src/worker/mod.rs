//! Per-account IMAP worker (P7).
#![allow(clippy::too_many_arguments, clippy::large_enum_variant)]

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime};

use alcedo_core::{
    Account, AccountId, AccountIdentity, AccountSignature, Address, AttachmentLists, AuthMethod,
    BodyAttachment, CidInline, ComposeAttachment, ComposeDraft, ComposeFormat, ComposeInline,
    Error, Folder, FolderId, IdentityId, ImapDraftRef, ListUnsubscribe, MessageId, MessageListMode,
    MessageRule, OAuthProvider, PgpKeySource, PgpStatus, Result, RuleAction, RuleExecution, RuleId,
    RulePlan, SendState, ServerFlags, ServerRulesPreview, ServerRulesStatus, SpecialUse, SyncEvent,
    attachment_is_downloadable, auth_retry_delay, descendant_imap_names,
    effective_mailbox_delimiter, email_from_address_token, folder_delimiter_for_parent,
    folder_display_name, folder_is_mutable, imap_name_exists, is_auth_failure,
    is_keyring_auth_message, is_single_reaction_emoji, join_mailbox_path, nested_imap_names,
    parse_address_token, part_is_file_attachment, plan_rules, preview_snippet,
    reaction_from_display_name, rename_imap_map, rule_has_server_capable_conditions,
    smtp_mail_from, validate_folder_leaf,
};
use alcedo_html::{
    html_to_snippet, looks_like_escaped_html, looks_like_html, render_outbound, rewrite_cids,
};
use alcedo_imap::{
    FetchedAttachment, IdleNotification, IdleSession, ImapAuth, ImapClient, ImapConnectOptions,
    MailboxStatus, SelectedMailbox, fetch::FetchedEnvelope, fetch_envelope_meta_by_seq,
    fetch_server_flags, format_uid_set_chunks, idle::IdleWaitResult, imap_header_message_id_search,
    imap_search_before_inclusive, uid_fetch_attachment_flags, uid_fetch_envelope_meta,
    uid_fetch_envelopes, uid_fetch_full_messages, uid_fetch_reply_headers, uid_fetch_rfc822_sizes,
    uid_fetch_routing_headers,
};
use alcedo_oauth::{
    SecretKind, get_secret, load_oauth_config_cached, refresh_google, refresh_microsoft,
    store_secret, with_quiet_keyring,
};
use alcedo_pgp_api::{
    IncomingKind, PgpBackend, PgpMimeKind, ProtectRequest, SignKey, inspect_incoming,
};
use alcedo_smtp::{
    PgpMimeBody, RenderedBodies, SmtpAuth, SmtpOptions, build_draft_message, message_to_rfc822,
    send_pgp_message, send_rendered, serialize_inner_body, validate_https_pixel_url,
};
use alcedo_store::{AttachmentMeta, FolderReplaceMode, MessageLocator, StoreHandle};
use chrono::{DateTime, NaiveDate, Utc};
use mail_parser::MessageParser;
use tokio::runtime::Handle;
use tokio::sync::{Mutex, RwLock, mpsc, oneshot};

use crate::oauth_token_cache::{TokenPurpose, cached_access_token, invalidate_access_token};
use tracing::{debug, info, warn};

use crate::aggregator::{
    emit_error, emit_unread_counts, list_mode, load_folder_envelopes, refresh_virtual_inboxes,
    select_folder,
};
use crate::pgp_backend::Pgp;
use crate::server_rules::ServerRulesBackend;

mod auth;
pub(crate) use self::auth::oauth_access_token;
mod backfill;
mod bodies;
mod delete;
mod drafts;
mod flag_copies;
mod flags;
mod folders;
mod idle;
mod mailbox_sync;
mod moves;
mod pgp;
mod queue;
mod reactions;
pub(crate) use self::reactions::emit_reaction_original;
mod rules;
mod search_import;
mod send;
mod sent_copy;
mod session;
#[cfg(test)]
mod test_support;
mod watched;

#[cfg(test)]
pub(crate) use self::bodies::stored_body_needs_charset_rescan;
use self::bodies::{OpenMode, fetch_message_remainder, fetch_raw_message, open_message};
pub(crate) use self::bodies::{
    emit_body_ready_from_stored_with_pending, emit_local_only_message_body, needs_attachment_bytes,
    needs_attachment_rescan, stored_has_local_body,
};
use self::delete::{
    delete_message, delete_messages, expunge_messages, reconcile_delete_messages, undelete_message,
};
use self::drafts::{
    delete_imap_draft, draft_message_id_eq, normalize_draft_message_id, save_imap_draft,
};
use self::flags::{mark_folder_read, set_flag, set_flags_batch};
use self::folders::{
    create_folder, delete_folder, delete_folder_on_or_before, empty_folder, rename_folder,
};
use self::idle::commit_worker_inbox;
pub(crate) use self::mailbox_sync::folder_is_syncable;
use self::mailbox_sync::{UnchangedEmit, sync_folders, sync_inbox};
use self::moves::{
    copy_message_to_folder, move_message, move_message_to_folder, move_messages_batch,
    move_messages_to_folder_batch,
};
use self::rules::{adopt_server_rules, after_rule_mutation, emit_rules, run_message_rules};
use self::search_import::import_server_search;
use self::send::{emit_identities, emit_signature, file_sent_on_imap, finish_send_on_imap};
use self::session::{
    account_worker_loop, is_recoverable_disconnect, is_smtp_worker_command, liveness_probe_due,
    smtp_worker_loop,
};
use self::watched::{backfill_folder_page, prefetch_folder_bodies, sync_folder_recent};

const POLL_INTERVAL: Duration = Duration::from_secs(60);
/// DONE + inbox sync while IDLE when EXISTS never arrives (Gmail often stays
/// quiet on a second session). Shorter than [`POLL_INTERVAL`]: Inbox is not
/// covered by the command session's watched-folder STATUS poll.
const INBOX_IDLE_CATCHUP_INTERVAL: Duration = Duration::from_secs(20);
const WATCHED_STATUS_BATCH: usize = 8;
const WATCHED_LIST_CAP: usize = 64;
/// Short pause before reconnecting after a dropped IMAP session (e.g. sleep/wake).
const RECONNECT_DELAY: Duration = Duration::from_secs(2);
/// Bound IDLE interrupt/`DONE` so a half-open socket after suspend fails fast.
const IDLE_DONE_TIMEOUT: Duration = Duration::from_secs(5);
/// A session quiet this long gets a NOOP probe before user-driven commands.
const LIVENESS_IDLE_THRESHOLD: Duration = Duration::from_secs(20);
/// A healthy server answers NOOP in well under a second; a dead peer never does.
const LIVENESS_PROBE_TIMEOUT: Duration = Duration::from_secs(5);
/// Bound SELECT after wake so a dead peer does not hang the worker.
const IMAP_COMMAND_TIMEOUT: Duration = Duration::from_secs(45);
/// Body FETCH can be slow on large messages; TCP keepalive still fails a dead peer.
/// This timeout is not treated as a recoverable disconnect (no OpenMessage retry loop).
const IMAP_FETCH_TIMEOUT: Duration = Duration::from_secs(300);
/// Message of the error `with_fetch_timeout` returns; see `is_fetch_timeout`.
const FETCH_TIMED_OUT: &str = "FETCH timed out";
const LIST_LIMIT: u32 = 100;
const BODY_PREFETCH_WINDOW: u32 = 50;
/// Pause before an attachment download so a click that arrives in this window
/// paints its text first.
const REMAINDER_GRACE: Duration = Duration::from_millis(400);
/// How long a compose session stays interesting after its last APPEND or send.
const COMPOSE_SESSION_WINDOW: Duration = Duration::from_secs(120);

/// Per-account worker state for running rules on the server.
#[derive(Clone, Default)]
struct ServerRulesSession {
    status: ServerRulesStatus,
    probed: bool,
    /// `None` when no backend is registered: every rule runs in Alcedo.
    backend: Option<Arc<dyn ServerRulesBackend>>,
}

impl ServerRulesSession {
    fn new(backend: Option<Arc<dyn ServerRulesBackend>>) -> Self {
        Self {
            backend,
            ..Self::default()
        }
    }
}

async fn with_imap_timeout<T, F>(timeout: Duration, fut: F) -> Result<T>
where
    F: std::future::Future<Output = Result<T>>,
{
    with_imap_timeout_err(timeout, fut, "connection timed out").await
}

async fn with_fetch_timeout<T, F>(fut: F) -> Result<T>
where
    F: std::future::Future<Output = Result<T>>,
{
    with_imap_timeout_err(IMAP_FETCH_TIMEOUT, fut, FETCH_TIMED_OUT).await
}

async fn with_imap_timeout_err<T, F>(timeout: Duration, fut: F, timed_out: &str) -> Result<T>
where
    F: std::future::Future<Output = Result<T>>,
{
    match tokio::time::timeout(timeout, fut).await {
        Ok(result) => result,
        Err(_) => Err(Error::Imap(timed_out.to_string())),
    }
}

/// Sent row written from the SMTP bytes before the server assigns a UID.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalSentRow {
    pub message_id: MessageId,
    pub folder_id: FolderId,
    pub uid: u32,
}

/// Commands routed to a single account worker.
#[derive(Clone, Debug)]
pub enum WorkerCommand {
    Refresh,
    SyncFolder(FolderId),
    /// One envelope-backfill page. Ranked below other work so `OpenMessage`
    /// can run while a large folder is still loading older list rows.
    BackfillFolder {
        folder: FolderId,
        have: u32,
    },
    OpenMessage(MessageId),
    /// Warm the reader cache for the next row. Text only — no attachment download.
    PrefetchMessage(MessageId),
    /// Attachment bytes for a message whose text is already on screen.
    /// Ranked under [`WorkerCommand::OpenMessage`] so a new click is not stuck
    /// behind a multi-megabyte FETCH.
    FetchMessageRemainder(MessageId),
    /// Download one missing body among the newest [`BODY_PREFETCH_WINDOW`] rows,
    /// then re-queue. Ranked last so clicks and list backfill run first.
    PrefetchBodies {
        folder: FolderId,
    },
    FetchRawMessage(MessageId),
    SetFlag {
        id: MessageId,
        seen: Option<bool>,
        flagged: Option<bool>,
    },
    SetFlags {
        ids: Vec<MessageId>,
        seen: Option<bool>,
        flagged: Option<bool>,
    },
    DeleteMessage(MessageId),
    DeleteMessages {
        ids: Vec<MessageId>,
    },
    /// One post-disconnect check of those ids. Not a blind MOVE.
    ReconcileDeleteMessages {
        ids: Vec<MessageId>,
    },
    ExpungeMessages {
        ids: Vec<MessageId>,
    },
    ReconcileExpungeMessages {
        ids: Vec<MessageId>,
    },
    UndeleteMessage {
        id: MessageId,
        dest: FolderId,
    },
    MoveMessage {
        id: MessageId,
        dest: SpecialUse,
    },
    MoveMessageToFolder {
        id: MessageId,
        dest: FolderId,
    },
    MoveMessages {
        ids: Vec<MessageId>,
        dest: SpecialUse,
    },
    MoveMessagesToFolder {
        ids: Vec<MessageId>,
        dest: FolderId,
    },
    CopyMessageToFolder {
        id: MessageId,
        dest: FolderId,
    },
    MarkFolderRead(FolderId),
    EmptyFolder(FolderId),
    DeleteFolderOnOrBefore {
        folder_id: FolderId,
        through: NaiveDate,
    },
    CreateFolder {
        parent: Option<FolderId>,
        name: String,
    },
    RenameFolder {
        id: FolderId,
        parent: Option<FolderId>,
        name: String,
    },
    DeleteFolder(FolderId),
    Send(ComposeDraft),
    UpsertSignature(AccountSignature),
    UpsertIdentity(AccountIdentity),
    DeleteIdentity {
        id: IdentityId,
    },
    SaveImapDraft(ComposeDraft),
    DeleteImapDraft {
        folder: FolderId,
        uid: u32,
        message_id: String,
    },
    UpsertMessageRule(MessageRule),
    DeleteMessageRule {
        id: RuleId,
    },
    ReorderMessageRules {
        ids: Vec<RuleId>,
    },
    RunMessageRules {
        folder: FolderId,
        rule_ids: Option<Vec<RuleId>>,
    },
    AdoptServerRules,
    /// Show the sent message from the bytes we already have, then queue
    /// [`WorkerCommand::FileSentOnImap`] for the server copy.
    FinishSend {
        draft: ComposeDraft,
        rfc822: Vec<u8>,
        message_id_hdr: String,
    },
    /// Draft delete + Sent APPEND. Ranked under `OpenMessage` so a click is
    /// not stuck behind the upload of a large attachment.
    FileSentOnImap {
        draft: ComposeDraft,
        rfc822: Vec<u8>,
        message_id_hdr: String,
        placeholder: Option<LocalSentRow>,
    },
    /// Search every folder on the server and store messages not yet local.
    ImportServerSearch {
        criteria: String,
        generation: u64,
        progress: crate::extension::SearchProgress,
    },
}

enum WorkerMsg {
    Command(WorkerCommand),
    Shutdown,
}

enum IdleControl {
    Shutdown,
    UnselectIf {
        imap_name: String,
        delimiter: String,
        ack: oneshot::Sender<()>,
    },
}

/// Split IMAP mode: idle session owns Inbox SELECT; command session skips re-SELECT.
struct SplitImapContext {
    idle_ctrl: Arc<Mutex<Option<mpsc::Sender<IdleControl>>>>,
    inbox_shared: Arc<RwLock<Folder>>,
    reselect_inbox: bool,
}

/// Draft APPEND from the current worker session; do not purge on transient FETCH miss.
struct RecentDraftAppend {
    folder_id: FolderId,
    uid: u32,
    message_id: String,
    at: Instant,
}

/// A Drafts APPEND that landed but never yielded a UID (no APPENDUID and an
/// empty `HEADER Message-ID` SEARCH). Kept so a later delete can still find the
/// orphan by re-fetching the UID range that was appended after it.
struct UnresolvedDraftAppend {
    folder_id: FolderId,
    message_id: String,
    from_uid: u32,
    at: Instant,
}

/// A compose session (draft Message-ID) whose message was sent; a late autosave
/// for it must not put the row back in Drafts.
struct SentComposeSession {
    message_id: String,
    at: Instant,
}

struct PendingSentAttachments {
    folder_id: FolderId,
    message_id: String,
    attachments: Vec<FetchedAttachment>,
}

const MAX_PENDING_SENT_ATTACHMENTS: usize = 8;

#[derive(Clone, Debug, Default)]
struct WatchedFolders {
    cursor: usize,
    last_poll: Option<Instant>,
}

/// One long-lived IMAP connection. The client itself records which mailbox is
/// SELECTed (see [`WorkerImapState::selected_on_server`]).
struct WorkerImapState {
    client: Option<ImapClient>,
    inbox: Folder,
    recent_draft_append: Option<RecentDraftAppend>,
    unresolved_draft_append: Option<UnresolvedDraftAppend>,
    sent_compose_sessions: Vec<SentComposeSession>,
    pending_sent: Vec<PendingSentAttachments>,
    watched: WatchedFolders,
    search_import_generation: Arc<AtomicU64>,
    /// Wall-clock time of the last server reply on this session. Wall clock on
    /// purpose: tokio's `Instant` stops during suspend, so it cannot see a sleep.
    last_io: SystemTime,
}

impl WorkerImapState {
    fn client_mut(&mut self) -> &mut ImapClient {
        self.client
            .as_mut()
            .expect("imap client should be present outside IDLE handoff")
    }

    fn touch_io(&mut self) {
        self.last_io = SystemTime::now();
    }

    /// Mailbox SELECTed on this connection, or "" when none is known.
    ///
    /// The client updates this on every SELECT (cleared first, so a failed or
    /// cancelled one leaves nothing), UNSELECT and IDLE hand-off. A copy kept
    /// here drifted after a sync failed past its SELECT, which let a STORE,
    /// MOVE or DELETE skip the SELECT it needed.
    fn selected_on_server(&self) -> &str {
        self.client
            .as_ref()
            .and_then(|client| client.selected_mailbox())
            .unwrap_or("")
    }

    /// Cheap NOOP before user-driven work on a session that has been quiet.
    /// After suspend the socket looks open but the peer is gone; without this
    /// the first click waits for the TCP user timeout (~30s). Returns false
    /// when the session is dead and the caller should reconnect.
    async fn session_alive_for_command(&mut self, account: &Account) -> bool {
        if !liveness_probe_due(self.last_io, SystemTime::now()) {
            return true;
        }
        match with_imap_timeout(LIVENESS_PROBE_TIMEOUT, self.client_mut().noop()).await {
            Ok(()) => {
                self.touch_io();
                true
            }
            Err(err) => {
                info!(
                    account = %account.email,
                    %err,
                    "IMAP liveness probe failed after idle; reconnecting"
                );
                false
            }
        }
    }

    fn note_unresolved_draft_append(
        &mut self,
        folder_id: FolderId,
        message_id: &str,
        from_uid: u32,
    ) {
        if normalize_draft_message_id(message_id).is_empty() {
            return;
        }
        // Repeated failed saves reuse the session Message-ID; keep the earliest
        // range so every orphan copy stays inside it.
        if let Some(existing) = self.unresolved_draft_append.as_mut()
            && existing.folder_id == folder_id
            && draft_message_id_eq(&existing.message_id, message_id)
        {
            existing.at = Instant::now();
            return;
        }
        self.unresolved_draft_append = Some(UnresolvedDraftAppend {
            folder_id,
            message_id: message_id.to_string(),
            from_uid,
            at: Instant::now(),
        });
    }

    fn unresolved_draft_append_for(
        &self,
        folder_id: FolderId,
        message_id: &str,
    ) -> Option<&UnresolvedDraftAppend> {
        self.unresolved_draft_append.as_ref().filter(|record| {
            record.folder_id == folder_id
                && record.at.elapsed() <= COMPOSE_SESSION_WINDOW
                && (message_id.trim().is_empty()
                    || draft_message_id_eq(&record.message_id, message_id))
        })
    }

    fn note_sent_compose_session(&mut self, message_id: &str) {
        let message_id = normalize_draft_message_id(message_id);
        if message_id.is_empty() {
            return;
        }
        self.sent_compose_sessions.retain(|session| {
            session.at.elapsed() <= COMPOSE_SESSION_WINDOW && session.message_id != message_id
        });
        self.sent_compose_sessions.push(SentComposeSession {
            message_id,
            at: Instant::now(),
        });
    }

    fn compose_session_sent(&self, message_id: &str) -> bool {
        let message_id = normalize_draft_message_id(message_id);
        !message_id.is_empty()
            && self.sent_compose_sessions.iter().any(|session| {
                session.message_id == message_id && session.at.elapsed() <= COMPOSE_SESSION_WINDOW
            })
    }
}

enum WaitOutcome {
    Continue,
    Shutdown,
    /// Drop the IMAP session and reconnect; optionally re-run a command that was
    /// interrupted by the dead connection (typical after sleep/wake).
    Reconnect {
        retry: Vec<WorkerCommand>,
    },
}

fn emit_status(events: &flume::Sender<SyncEvent>, account: &Account, text: impl Into<String>) {
    let text = text.into();
    info!(
        account = %account.email,
        imap = %account.imap_host,
        status = %text,
        "account status"
    );
    let _ = events.send(SyncEvent::Status {
        account: account.id,
        text,
    });
}

/// Handle for sending commands to a running account worker.
#[derive(Clone)]
pub struct WorkerHandle {
    imap_tx: mpsc::Sender<WorkerMsg>,
    smtp_tx: mpsc::Sender<WorkerMsg>,
    idle_ctrl: Arc<Mutex<Option<mpsc::Sender<IdleControl>>>>,
    search_import_generation: Arc<AtomicU64>,
}

impl WorkerHandle {
    pub(crate) fn set_search_import_generation(&self, generation: u64) {
        self.search_import_generation
            .store(generation, Ordering::Relaxed);
    }
    fn route_tx(&self, command: &WorkerCommand) -> &mpsc::Sender<WorkerMsg> {
        if is_smtp_worker_command(command) {
            &self.smtp_tx
        } else {
            &self.imap_tx
        }
    }

    pub async fn send_async(&self, command: WorkerCommand) -> bool {
        let tx = self.route_tx(&command);
        match tx.send(WorkerMsg::Command(command)).await {
            Ok(()) => true,
            Err(_) => {
                warn!("account worker channel closed or full");
                false
            }
        }
    }

    pub fn send(&self, command: WorkerCommand) {
        let tx = self.route_tx(&command);
        if tx.try_send(WorkerMsg::Command(command)).is_err() {
            warn!("account worker channel closed or full");
        }
    }

    pub fn shutdown(&self) {
        let _ = self.imap_tx.try_send(WorkerMsg::Shutdown);
        let _ = self.smtp_tx.try_send(WorkerMsg::Shutdown);
        if let Ok(guard) = self.idle_ctrl.try_lock()
            && let Some(ctrl) = guard.as_ref()
        {
            let _ = ctrl.try_send(IdleControl::Shutdown);
        }
    }
}

/// Spawn one Tokio task per account (command IMAP + serial SMTP; optional idle IMAP).
pub fn spawn_account_worker(
    account: Account,
    store: StoreHandle,
    events: flume::Sender<SyncEvent>,
    handle: Handle,
    server_rules_backend: Option<Arc<dyn ServerRulesBackend>>,
) -> WorkerHandle {
    let (imap_tx, imap_rx) = mpsc::channel(64);
    let (smtp_tx, smtp_rx) = mpsc::channel(8);
    let (idle_ctrl_tx, idle_ctrl_rx) = mpsc::channel(4);
    let idle_ctrl = Arc::new(Mutex::new(Some(idle_ctrl_tx)));
    let token_cache = Arc::new(Mutex::new(None));
    let auth_notified = Arc::new(AtomicBool::new(false));
    let search_import_generation = Arc::new(AtomicU64::new(0));
    let server_rules = Arc::new(Mutex::new(ServerRulesSession::new(server_rules_backend)));

    let worker_handle = WorkerHandle {
        imap_tx: imap_tx.clone(),
        smtp_tx: smtp_tx.clone(),
        idle_ctrl: idle_ctrl.clone(),
        search_import_generation: search_import_generation.clone(),
    };

    handle.spawn(smtp_worker_loop(
        account.clone(),
        smtp_rx,
        imap_tx.clone(),
        store.clone(),
        events.clone(),
        token_cache.clone(),
        auth_notified.clone(),
    ));

    handle.spawn(account_worker_loop(
        account,
        imap_rx,
        idle_ctrl_rx,
        idle_ctrl,
        store,
        events,
        token_cache,
        auth_notified,
        server_rules,
        handle.clone(),
        imap_tx.downgrade(),
        search_import_generation,
    ));

    worker_handle
}

async fn handle_worker_command(
    cmd: WorkerCommand,
    state: &mut WorkerImapState,
    account: &Account,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
    _auth_notified: &mut bool,
    server_rules: &mut ServerRulesSession,
    split: &SplitImapContext,
) -> Result<Option<WorkerCommand>> {
    match cmd {
        WorkerCommand::Refresh => {
            let inbox = sync_folders(state.client_mut(), account, store, events).await?;
            commit_worker_inbox(state, inbox, Some(split)).await;
            let inbox = state.inbox.clone();
            sync_inbox(
                state,
                account,
                &inbox,
                store,
                events,
                server_rules,
                UnchangedEmit::Always,
            )
            .await?;
            emit_unread_counts(store, events).await?;
            Ok(None)
        }
        WorkerCommand::SyncFolder(folder_id) => {
            sync_folder_recent(state, account, folder_id, store, events, server_rules).await
        }
        WorkerCommand::BackfillFolder { folder, have } => {
            backfill_folder_page(state, account, folder, store, events, have).await
        }
        WorkerCommand::OpenMessage(id) => {
            open_message(state, account, id, store, events, OpenMode::Interactive).await
        }
        WorkerCommand::PrefetchMessage(id) => {
            open_message(state, account, id, store, events, OpenMode::Prefetch).await
        }
        WorkerCommand::FetchMessageRemainder(id) => {
            fetch_message_remainder(state, account, id, store, events, true)
                .await
                .map(|()| None)
        }
        WorkerCommand::PrefetchBodies { folder } => {
            prefetch_folder_bodies(state, account, folder, store, events).await
        }
        WorkerCommand::FetchRawMessage(id) => fetch_raw_message(state, account, id, store, events)
            .await
            .map(|()| None),
        WorkerCommand::SetFlag { id, seen, flagged } => {
            set_flag(state, account, id, seen, flagged, store, events)
                .await
                .map(|()| None)
        }
        WorkerCommand::SetFlags { ids, seen, flagged } => {
            set_flags_batch(state, account, ids, seen, flagged, store, events)
                .await
                .map(|()| None)
        }
        WorkerCommand::DeleteMessage(id) => delete_message(state, account, id, store, events)
            .await
            .map(|()| None),
        WorkerCommand::DeleteMessages { ids } => {
            delete_messages(state, account, ids, store, events)
                .await
                .map(|()| None)
        }
        WorkerCommand::ReconcileDeleteMessages { ids } => {
            reconcile_delete_messages(state, account, ids, store, events)
                .await
                .map(|()| None)
        }
        WorkerCommand::ExpungeMessages { ids } => {
            expunge_messages(state, account, ids, store, events, false)
                .await
                .map(|()| None)
        }
        WorkerCommand::ReconcileExpungeMessages { ids } => {
            expunge_messages(state, account, ids, store, events, true)
                .await
                .map(|()| None)
        }
        WorkerCommand::UndeleteMessage { id, dest } => {
            undelete_message(state, account, id, dest, store, events)
                .await
                .map(|()| None)
        }
        WorkerCommand::MoveMessage { id, dest } => {
            move_message(state, account, id, dest, store, events)
                .await
                .map(|()| None)
        }
        WorkerCommand::MoveMessages { ids, dest } => {
            move_messages_batch(state, account, ids, dest, store, events)
                .await
                .map(|()| None)
        }
        WorkerCommand::MoveMessageToFolder { id, dest } => {
            move_message_to_folder(state, account, id, dest, store, events, true)
                .await
                .map(|()| None)
        }
        WorkerCommand::MoveMessagesToFolder { ids, dest } => {
            move_messages_to_folder_batch(state, account, ids, dest, store, events, true)
                .await
                .map(|()| None)
        }
        WorkerCommand::CopyMessageToFolder { id, dest } => {
            copy_message_to_folder(state, account, id, dest, store, events, true)
                .await
                .map(|()| None)
        }
        WorkerCommand::MarkFolderRead(folder_id) => {
            mark_folder_read(state, account, folder_id, store, events)
                .await
                .map(|()| None)
        }
        WorkerCommand::EmptyFolder(folder_id) => {
            empty_folder(state, account, folder_id, store, events)
                .await
                .map(|()| None)
        }
        WorkerCommand::DeleteFolderOnOrBefore { folder_id, through } => {
            delete_folder_on_or_before(state, account, folder_id, through, store, events)
                .await
                .map(|()| None)
        }
        WorkerCommand::CreateFolder { parent, name } => {
            create_folder(state, account, parent, name, store, events)
                .await
                .map(|()| None)
        }
        WorkerCommand::RenameFolder { id, parent, name } => {
            rename_folder(state, account, id, parent, name, store, events, split)
                .await
                .map(|()| None)
        }
        WorkerCommand::DeleteFolder(folder_id) => {
            delete_folder(state, account, folder_id, store, events, split)
                .await
                .map(|()| None)
        }
        WorkerCommand::Send(_) => {
            warn!(
                account = %account.email,
                "Send reached IMAP worker; should be routed to SMTP"
            );
            Ok(None)
        }
        WorkerCommand::FinishSend {
            draft,
            rfc822,
            message_id_hdr,
        } => {
            finish_send_on_imap(
                state,
                account,
                draft,
                rfc822,
                message_id_hdr,
                store,
                events,
                server_rules,
            )
            .await
        }
        WorkerCommand::FileSentOnImap {
            draft,
            rfc822,
            message_id_hdr,
            placeholder,
        } => file_sent_on_imap(
            state,
            account,
            draft,
            rfc822,
            message_id_hdr,
            placeholder,
            store,
            events,
            server_rules,
        )
        .await
        .map(|()| None),
        WorkerCommand::UpsertSignature(signature) => {
            if let Err(message) = signature.validate() {
                emit_error(events, Some(account.id), message.to_string());
                return Ok(None);
            }
            store.upsert_signature(&signature).await?;
            emit_signature(account, store, events).await;
            Ok(None)
        }
        WorkerCommand::UpsertIdentity(identity) => {
            if let Err(message) = identity.validate(&account.email) {
                emit_error(events, Some(account.id), message.to_string());
                return Ok(None);
            }
            store.upsert_identity(&identity).await?;
            emit_identities(account, store, events).await;
            Ok(None)
        }
        WorkerCommand::DeleteIdentity { id } => {
            store.delete_identity(id).await?;
            emit_identities(account, store, events).await;
            Ok(None)
        }
        WorkerCommand::SaveImapDraft(draft) => {
            match save_imap_draft(state, account, draft, store, events, server_rules).await {
                Ok(()) => Ok(None),
                Err(err) if is_recoverable_disconnect(&err) => Err(err),
                Err(err) => {
                    let _ = events.send(SyncEvent::ImapDraftSaveFailed {
                        message: err.to_string(),
                    });
                    Ok(None)
                }
            }
        }
        WorkerCommand::DeleteImapDraft {
            folder,
            uid,
            message_id,
        } => delete_imap_draft(
            state,
            account,
            folder,
            uid,
            &message_id,
            store,
            events,
            server_rules,
        )
        .await
        .map(|()| None),
        WorkerCommand::UpsertMessageRule(rule) => {
            store.upsert_rule(&rule).await?;
            after_rule_mutation(account, store, events, server_rules)
                .await
                .map(|()| None)
        }
        WorkerCommand::DeleteMessageRule { id } => {
            store.delete_rule(id).await?;
            after_rule_mutation(account, store, events, server_rules)
                .await
                .map(|()| None)
        }
        WorkerCommand::ReorderMessageRules { ids } => {
            store.reorder_rules(account.id, &ids).await?;
            after_rule_mutation(account, store, events, server_rules)
                .await
                .map(|()| None)
        }
        WorkerCommand::RunMessageRules { folder, rule_ids } => {
            run_message_rules(state, account, folder, rule_ids, store, events)
                .await
                .map(|()| None)
        }
        WorkerCommand::AdoptServerRules => {
            adopt_server_rules(account, store, server_rules).await?;
            emit_rules(account, store, server_rules, events).await;
            Ok(None)
        }
        WorkerCommand::ImportServerSearch {
            criteria,
            generation,
            progress,
        } => import_server_search(
            state, account, store, events, &criteria, generation, progress,
        )
        .await
        .map(|()| None),
    }
}

struct SmtpSendResult {
    draft: ComposeDraft,
    rfc822: Vec<u8>,
    message_id_hdr: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worker::test_support::folder_with;
    use alcedo_core::FolderId;

    fn imap_state_without_client() -> WorkerImapState {
        WorkerImapState {
            client: None,
            inbox: folder_with(Some(1), None),
            recent_draft_append: None,
            unresolved_draft_append: None,
            sent_compose_sessions: Vec::new(),
            pending_sent: Vec::new(),
            watched: WatchedFolders::default(),
            search_import_generation: Arc::new(AtomicU64::new(0)),
            last_io: SystemTime::now(),
        }
    }

    #[test]
    fn unresolved_draft_append_keeps_earliest_uid_range_per_session() {
        let drafts = FolderId(9);
        let mut state = imap_state_without_client();
        state.note_unresolved_draft_append(drafts, "<session@example.com>", 40);
        state.note_unresolved_draft_append(drafts, "session@example.com", 55);

        let record = state
            .unresolved_draft_append_for(drafts, "<session@example.com>")
            .expect("record for this session");
        assert_eq!(record.from_uid, 40);
        assert!(
            state
                .unresolved_draft_append_for(drafts, "<other@example.com>")
                .is_none()
        );
        assert!(
            state
                .unresolved_draft_append_for(FolderId(10), "<session@example.com>")
                .is_none()
        );
    }

    #[test]
    fn sent_compose_session_blocks_late_autosave_for_that_draft_only() {
        let mut state = imap_state_without_client();
        state.note_sent_compose_session("<session@example.com>");

        assert!(state.compose_session_sent("session@example.com"));
        assert!(!state.compose_session_sent("<other@example.com>"));
        assert!(!state.compose_session_sent(""));
    }

    #[test]
    fn undecoded_jpeg_base64_is_not_the_file() {
        let encoded = b"/9j/4AAQSkZJRgABAQAAAQABAAD/2wBD";
        assert!(alcedo_imap::attachment_bytes_are_undecoded_base64(
            "image/jpeg",
            encoded
        ));
        assert!(!alcedo_imap::attachment_bytes_are_undecoded_base64(
            "text/plain",
            encoded
        ));
        assert!(!alcedo_imap::attachment_bytes_are_undecoded_base64(
            "image/jpeg",
            b"\xFF\xD8\xFF\xE0\x00\x10JFIF\x00\x01\x01\x00\x00\x01\x00\x01\x00\x00",
        ));
    }
}
