//! Extension points for features that live outside the public crates.
//!
//! An extension receives the [`UiCommand::Extension`] messages addressed to it
//! and can reply with [`SyncEvent`]s through its [`SyncContext`].
//!
//! [`UiCommand::Extension`]: alcedo_core::UiCommand::Extension

use std::sync::Arc;

use alcedo_core::{AccountId, SyncEvent};
use alcedo_store::StoreHandle;

use crate::runtime::SharedWorkers;
use crate::server_rules::{BoxFuture, ServerRulesBackend};
use crate::worker::WorkerCommand;

/// Handles commands for one extension, on the sync runtime.
pub trait SyncExtension: Send + Sync + 'static {
    /// Must match the `extension` field of the messages it handles.
    fn name(&self) -> &'static str;

    /// Called once when the sync runtime starts, before UI commands are
    /// handled: create tables, start background tasks, send initial state.
    fn start(&self, _ctx: SyncContext) -> BoxFuture<'_, ()> {
        Box::pin(std::future::ready(()))
    }

    /// Handle one command. Commands are handled in the order they arrive;
    /// spawn on the runtime for long work.
    fn handle_command(&self, payload: serde_json::Value, ctx: SyncContext) -> BoxFuture<'_, ()>;
}

/// Progress callbacks for [`SyncContext::import_server_search`].
pub trait ServerSearchProgress: Send + Sync + 'static {
    /// Called after each folder is searched (and any new messages stored).
    fn folder_done(&self) -> BoxFuture<'_, ()>;
    /// Called once when the search ends without being cancelled.
    fn finished(&self) -> BoxFuture<'_, ()>;
}

/// [`ServerSearchProgress`] as a worker command field.
#[derive(Clone)]
pub struct SearchProgress(pub Arc<dyn ServerSearchProgress>);

impl std::fmt::Debug for SearchProgress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SearchProgress")
    }
}

/// What an extension can reach on the sync side.
#[derive(Clone)]
pub struct SyncContext {
    store: StoreHandle,
    events: flume::Sender<SyncEvent>,
    workers: SharedWorkers,
}

impl SyncContext {
    pub(crate) fn new(
        store: StoreHandle,
        events: flume::Sender<SyncEvent>,
        workers: SharedWorkers,
    ) -> Self {
        Self {
            store,
            events,
            workers,
        }
    }

    pub fn store(&self) -> &StoreHandle {
        &self.store
    }

    /// The list layout the user chose (flat or conversations).
    pub fn list_mode(&self) -> alcedo_core::MessageListMode {
        crate::aggregator::list_mode()
    }

    pub fn emit(&self, event: SyncEvent) {
        let _ = self.events.send(event);
    }

    /// Queue `command` as if the UI had sent it (moves, flags, sends, …). It
    /// runs after the command being handled now.
    pub fn dispatch(&self, command: alcedo_core::UiCommand) {
        let _ = self.workers.command_tx.send(command);
    }

    /// Show an error to the user (toast and activity log).
    pub fn emit_error(&self, message: impl Into<String>) {
        self.emit(SyncEvent::Error {
            account: None,
            message: message.into(),
        });
    }

    /// Search every selectable folder of `account` on the server with the IMAP
    /// SEARCH `criteria`, storing messages that are not local yet. A later
    /// call or [`Self::cancel_server_search`] with a new `generation` stops it.
    /// Returns false when the account has no running worker.
    pub async fn import_server_search(
        &self,
        account: AccountId,
        criteria: String,
        generation: u64,
        progress: Arc<dyn ServerSearchProgress>,
    ) -> bool {
        self.workers
            .set_search_import_generation(account, generation);
        self.workers
            .send(
                account,
                WorkerCommand::ImportServerSearch {
                    criteria,
                    generation,
                    progress: SearchProgress(progress),
                },
            )
            .await
    }

    /// Stop a running [`Self::import_server_search`] on `account`.
    pub fn cancel_server_search(&self, account: AccountId, generation: u64) {
        self.workers
            .set_search_import_generation(account, generation);
    }
}

/// Sync-layer extensions passed to [`crate::SyncRuntime::start`].
#[derive(Clone, Default)]
pub struct SyncExtensions {
    pub server_rules: Option<Arc<dyn ServerRulesBackend>>,
    pub commands: Vec<Arc<dyn SyncExtension>>,
}

impl SyncExtensions {
    pub(crate) fn command_handler(&self, name: &str) -> Option<Arc<dyn SyncExtension>> {
        self.commands.iter().find(|ext| ext.name() == name).cloned()
    }
}
