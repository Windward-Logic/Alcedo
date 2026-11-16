//! Store handle, writer thread, and read-only reader thread (P2).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;

use tokio::sync::oneshot;

use alcedo_core::{
    Account, AccountId, AccountIdentity, AccountSignature, Address, Contact, ContactDraft,
    ContactGroup, ContactId, Conversation, Envelope, Folder, FolderId, GroupDraft, GroupId,
    IdentityId, MessageId, MessageRule, PgpPeerInfo, PgpStatus, QueuedSend, QueuedSendId, Result,
    RoutingHeaders, RuleExecution, RuleId, SmtpEnvelopePolicy, Snippet, SnippetId, ThreadId,
};

use crate::schema;
use crate::{AttachmentMeta, MessageLocator, StoreError, StoreResult, StoredMessage, map_core_err};

mod accounts;
mod attachments;
mod contacts;
mod dispatch;
mod extension;
mod folders;
mod identities;
mod messages;
mod pgp;
mod queued_sends;
mod remote_images;
mod requests;
mod rules;
mod search;
mod signatures;
mod snippets;

use self::dispatch::{dispatch_read, dispatch_write};
use self::requests::{ReadRequest, WriteRequest};

type MessageUidRow = (MessageId, u32, Option<String>);

struct Queued<T> {
    enqueued_at: std::time::Instant,
    kind: &'static str,
    req: T,
}

struct WriterState {
    conn: rusqlite::Connection,
    attachments_dir: PathBuf,
}

struct ReaderState {
    conn: rusqlite::Connection,
    attachments_dir: PathBuf,
}

fn spawn_writer(
    conn: rusqlite::Connection,
    attachments_dir: PathBuf,
) -> mpsc::Sender<Queued<WriteRequest>> {
    let (tx, rx) = mpsc::channel::<Queued<WriteRequest>>();
    thread::Builder::new()
        .name("alcedo-store-writer".to_string())
        .spawn(move || {
            let mut state = WriterState {
                conn,
                attachments_dir,
            };
            while let Ok(queued) = rx.recv() {
                let queue_ms = queued.enqueued_at.elapsed().as_secs_f64() * 1000.0;
                let started = std::time::Instant::now();
                dispatch_write(&mut state, queued.req);
                tracing::debug!(
                    lane = "writer",
                    kind = queued.kind,
                    queue_ms,
                    exec_ms = started.elapsed().as_secs_f64() * 1000.0,
                    "store request"
                );
            }
        })
        .expect("spawn alcedo-store-writer");
    tx
}

fn spawn_reader(
    conn: rusqlite::Connection,
    attachments_dir: PathBuf,
) -> mpsc::Sender<Queued<ReadRequest>> {
    let (tx, rx) = mpsc::channel::<Queued<ReadRequest>>();
    thread::Builder::new()
        .name("alcedo-store-reader".to_string())
        .spawn(move || {
            let mut reader = ReaderState {
                conn,
                attachments_dir,
            };
            while let Ok(queued) = rx.recv() {
                let queue_ms = queued.enqueued_at.elapsed().as_secs_f64() * 1000.0;
                let started = std::time::Instant::now();
                dispatch_read(&mut reader, queued.req);
                tracing::debug!(
                    lane = "reader",
                    kind = queued.kind,
                    queue_ms,
                    exec_ms = started.elapsed().as_secs_f64() * 1000.0,
                    "store request"
                );
            }
        })
        .expect("spawn alcedo-store-reader");
    tx
}

fn xdg_alcedo_dir() -> StoreResult<PathBuf> {
    let base = directories::BaseDirs::new()
        .ok_or_else(|| StoreError::Other("could not resolve XDG base directories".to_string()))?;
    Ok(base.data_dir().join("alcedo"))
}

fn attachments_dir_for_db(db_path: &Path) -> PathBuf {
    if std::env::var("ALCEDO_DB").is_ok() {
        db_path
            .parent()
            .map(|p| p.join("attachments"))
            .unwrap_or_else(|| PathBuf::from("attachments"))
    } else if let Ok(dir) = xdg_alcedo_dir() {
        dir.join("attachments")
    } else {
        db_path
            .parent()
            .map(|p| p.join("attachments"))
            .unwrap_or_else(|| PathBuf::from("attachments"))
    }
}

async fn send_read<T, F>(tx: &mpsc::Sender<Queued<ReadRequest>>, build: F) -> Result<T>
where
    F: FnOnce(oneshot::Sender<StoreResult<T>>) -> ReadRequest,
{
    let (resp_tx, resp_rx) = oneshot::channel();
    let req = build(resp_tx);
    tx.send(Queued {
        enqueued_at: std::time::Instant::now(),
        kind: req.kind(),
        req,
    })
    .map_err(|e| map_core_err(StoreError::Other(e.to_string())))?;
    let inner = resp_rx
        .await
        .map_err(|e| map_core_err(StoreError::Other(e.to_string())))?;
    inner.map_err(map_core_err)
}

async fn send_write<T, F>(tx: &mpsc::Sender<Queued<WriteRequest>>, build: F) -> Result<T>
where
    F: FnOnce(oneshot::Sender<StoreResult<T>>) -> WriteRequest,
{
    let (resp_tx, resp_rx) = oneshot::channel();
    let req = build(resp_tx);
    tx.send(Queued {
        enqueued_at: std::time::Instant::now(),
        kind: req.kind(),
        req,
    })
    .map_err(|e| map_core_err(StoreError::Other(e.to_string())))?;
    let inner = resp_rx
        .await
        .map_err(|e| map_core_err(StoreError::Other(e.to_string())))?;
    inner.map_err(map_core_err)
}

/// Async handle to the dedicated SQLite writer and read-only reader threads.
#[derive(Clone)]
pub struct StoreHandle {
    write_tx: mpsc::Sender<Queued<WriteRequest>>,
    read_tx: mpsc::Sender<Queued<ReadRequest>>,
}

impl StoreHandle {
    pub fn open_xdg() -> Result<Self> {
        let db_path = if let Ok(path) = std::env::var("ALCEDO_DB") {
            PathBuf::from(path)
        } else {
            xdg_alcedo_dir().map_err(map_core_err)?.join("mail.db")
        };
        Self::open_path(&db_path)
    }

    pub fn open_path(p: &Path) -> Result<Self> {
        let writer_conn = schema::open_connection(p).map_err(map_core_err)?;
        let reader_conn = schema::open_reader(p).map_err(map_core_err)?;
        let attachments_dir = attachments_dir_for_db(p);
        if let Err(err) = crate::attachments::secure_attachments_dir(&attachments_dir) {
            tracing::warn!(?err, "could not restrict attachment directory permissions");
        }
        let write_tx = spawn_writer(writer_conn, attachments_dir.clone());
        let read_tx = spawn_reader(reader_conn, attachments_dir);
        Ok(Self { write_tx, read_tx })
    }
}
