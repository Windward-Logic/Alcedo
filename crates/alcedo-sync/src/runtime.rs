//! SyncRuntime command dispatch and worker registry (P7).

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock, RwLock};

use alcedo_core::{
    Account, AccountDraft, AccountId, AccountSettingsUpdate, AuthMethod, ContactDraft, ContactId,
    Error, FolderId, GroupDraft, GroupId, IdentityId, MessageId, QueuedSend, Result, RuleId,
    SendState, SmtpEnvelopePolicy, SnippetId, SpecialUse, SyncEvent, ThreadId, UNAPPROVED_GROUP_ID,
    UiCommand, display_name_from_email, folder_is_mutable, sanitize_accent_color,
    sanitize_avatar_text,
};
use alcedo_oauth::{SecretKind, delete_secret, get_secret, relabel_account_secrets, store_secret};
use alcedo_store::StoreHandle;
use chrono::Utc;
use tokio::runtime::Handle;
use tracing::warn;
use uuid::Uuid;

use crate::aggregator::{
    emit_error, emit_unread_counts, list_mode, refresh_virtual_inboxes, search, select_folder,
    select_unified_inbox, set_list_mode,
};
use crate::extension::{SyncContext, SyncExtensions};
#[cfg(test)]
use crate::worker::stored_body_needs_charset_rescan;
use crate::worker::{
    WorkerCommand, WorkerHandle, emit_body_ready_from_stored_with_pending,
    emit_local_only_message_body, folder_is_syncable, needs_attachment_bytes,
    needs_attachment_rescan, spawn_account_worker, stored_has_local_body,
};

const ACCENT_COLORS: [&str; 3] = ["#3584e4", "#7d4cdb", "#6141a5"];

/// Tokio-side sync coordinator: routes UI commands to workers and the aggregator.
pub struct SyncRuntime;

impl SyncRuntime {
    /// Start the sync runtime on the given Tokio handle.
    ///
    /// `events` receives outbound [`SyncEvent`]s (typically bridged to Relm4 in the binary).
    /// `commands` receives inbound [`UiCommand`]s from the UI (typically bridged from Relm4).
    pub fn start(
        handle: Handle,
        store: StoreHandle,
        events: flume::Sender<SyncEvent>,
        commands: flume::Receiver<UiCommand>,
        command_tx: flume::Sender<UiCommand>,
        extensions: SyncExtensions,
    ) -> Self {
        handle.clone().spawn(async move {
            let workers = Arc::new(WorkerRegistry::new(extensions, command_tx));

            match store.list_accounts().await {
                Ok(accounts) => {
                    // Emit every account before any await so the UI hydrates the
                    // full list before folder/list events (startup last-folder restore).
                    for account in &accounts {
                        let _ = events.send(SyncEvent::AccountAdded(account.clone()));
                    }
                    for account in accounts {
                        crate::pgp::emit_pgp_state(&store, account.id, &events).await;
                        if let Ok(folders) = store.list_folders(account.id).await
                            && !folders.is_empty()
                        {
                            let _ = events.send(SyncEvent::FoldersUpdated {
                                account: account.id,
                                folders,
                            });
                        }
                        emit_account_compose_state(&store, account.id, &events).await;
                        workers.spawn(account, store.clone(), events.clone(), handle.clone());
                    }
                }
                Err(err) => {
                    emit_error(&events, None, err.to_string());
                }
            }

            if let Err(err) = select_unified_inbox(&store, list_mode(), &events).await {
                emit_error(&events, None, err.to_string());
            }

            emit_queued_sends_recovered(&store, &events).await;

            emit_contacts_changed(&store, &events).await;
            emit_snippets_changed(&store, &events).await;
            emit_sent_recipients(&store, &events).await;
            emit_remote_image_senders_changed(&store, &events).await;
            let ctx = SyncContext::new(store.clone(), events.clone(), workers.clone());
            for extension in &workers.extensions.commands {
                extension.start(ctx.clone()).await;
            }

            while let Ok(cmd) = commands.recv_async().await {
                dispatch_command(cmd, &store, &events, &workers, &handle).await;
            }
        });

        SyncRuntime
    }
}

pub(crate) struct WorkerRegistry {
    workers: RwLock<HashMap<AccountId, WorkerHandle>>,
    extensions: SyncExtensions,
    /// Sender into this runtime's own command queue, for extensions.
    pub(crate) command_tx: flume::Sender<UiCommand>,
}

pub(crate) type SharedWorkers = Arc<WorkerRegistry>;

impl WorkerRegistry {
    fn new(extensions: SyncExtensions, command_tx: flume::Sender<UiCommand>) -> Self {
        Self {
            workers: RwLock::new(HashMap::new()),
            extensions,
            command_tx,
        }
    }

    fn spawn(
        &self,
        account: Account,
        store: StoreHandle,
        events: flume::Sender<SyncEvent>,
        handle: Handle,
    ) {
        let account_id = account.id;
        let worker = spawn_account_worker(
            account,
            store,
            events,
            handle,
            self.extensions.server_rules.clone(),
        );
        if let Ok(mut guard) = self.workers.write() {
            guard.insert(account_id, worker);
        }
    }

    fn remove(&self, account_id: AccountId) {
        let worker = self
            .workers
            .write()
            .ok()
            .and_then(|mut guard| guard.remove(&account_id));
        if let Some(worker) = worker {
            worker.shutdown();
        }
    }

    pub(crate) async fn send(&self, account_id: AccountId, command: WorkerCommand) -> bool {
        let worker = self
            .workers
            .read()
            .ok()
            .and_then(|guard| guard.get(&account_id).cloned());
        if let Some(worker) = worker {
            worker.send_async(command).await
        } else {
            warn!(?account_id, "no worker for account command");
            false
        }
    }

    pub(crate) fn set_search_import_generation(&self, account_id: AccountId, generation: u64) {
        let worker = self
            .workers
            .read()
            .ok()
            .and_then(|guard| guard.get(&account_id).cloned());
        if let Some(worker) = worker {
            worker.set_search_import_generation(generation);
        }
    }

    fn clone_for_route(&self) -> WorkerRoute {
        let workers = self
            .workers
            .read()
            .map(|guard| guard.clone())
            .unwrap_or_default();
        WorkerRoute { workers }
    }
}

async fn dispatch_command(
    cmd: UiCommand,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
    workers: &SharedWorkers,
    handle: &Handle,
) {
    match cmd {
        UiCommand::AddAccount(draft) => {
            if let Err(err) = add_account(draft, store, events, workers, handle).await {
                emit_error(events, None, err.to_string());
            }
        }
        UiCommand::RemoveAccount(account_id) => {
            if let Err(err) = remove_account(account_id, store, events, workers).await {
                emit_error(events, Some(account_id), err.to_string());
            }
        }
        UiCommand::UpdateAccount { id, update } => {
            if let Err(err) = update_account(id, update, store, events, workers, handle).await {
                emit_error(events, Some(id), err.to_string());
            }
        }
        UiCommand::SelectUnifiedInbox { mode } => {
            set_list_mode(mode);
            if let Err(err) = select_unified_inbox(store, mode, events).await {
                emit_error(events, None, err.to_string());
            }
        }
        UiCommand::SelectFolder { folder, mode, sync } => {
            set_list_mode(mode);
            if let Err(err) = select_folder(store, folder, mode, events).await {
                emit_error(events, None, err.to_string());
            } else if sync {
                request_folder_sync(store, workers, folder).await;
            }
        }
        UiCommand::SyncFolder(folder) => {
            request_folder_sync(store, workers, folder).await;
        }
        UiCommand::MarkFolderRead(folder) => {
            route_selectable_folder(folder, store, workers, |id| {
                WorkerCommand::MarkFolderRead(id)
            });
        }
        UiCommand::EmptyFolder(folder) => {
            route_selectable_folder(folder, store, workers, WorkerCommand::EmptyFolder);
        }
        UiCommand::DeleteFolderOnOrBefore { folder, through } => {
            route_delete_folder_on_or_before(folder, through, store, workers, events);
        }
        UiCommand::CreateFolder {
            account,
            parent,
            name,
        } => {
            workers
                .send(account, WorkerCommand::CreateFolder { parent, name })
                .await;
        }
        UiCommand::RenameFolder { id, parent, name } => {
            route_mutable_folder(id, store, workers, events, move |id| {
                WorkerCommand::RenameFolder { id, parent, name }
            })
            .await;
        }
        UiCommand::DeleteFolder(id) => {
            route_mutable_folder(id, store, workers, events, WorkerCommand::DeleteFolder).await;
        }
        UiCommand::Search {
            query,
            folder_ids,
            mode,
        } => {
            set_list_mode(mode);
            if let Err(err) = search(store, &query, folder_ids.as_deref(), mode, events).await {
                emit_error(events, None, err.to_string());
            }
        }
        UiCommand::OpenMessage(id) => {
            route_open_message(id, store, workers, events).await;
        }
        UiCommand::PrefetchMessage(id) => {
            route_prefetch_message(id, store, workers, events).await;
        }
        UiCommand::OpenThread {
            thread_id,
            folder_ids,
        } => {
            route_open_thread(thread_id, folder_ids, store, workers, events);
        }
        UiCommand::FetchRawMessage(id) => {
            route_message_command(id, store, workers, events, WorkerCommand::FetchRawMessage).await;
        }
        UiCommand::SetFlag { id, seen, flagged } => {
            route_message_command(id, store, workers, events, |id| WorkerCommand::SetFlag {
                id,
                seen,
                flagged,
            })
            .await;
        }
        UiCommand::SetFlags { ids, seen, flagged } => {
            route_set_flags(ids, seen, flagged, store, workers, events).await;
        }
        UiCommand::DeleteMessage(id) => {
            route_message_command(id, store, workers, events, WorkerCommand::DeleteMessage).await;
        }
        UiCommand::DeleteMessages { ids } => {
            route_delete_messages(ids, store, workers, events).await;
        }
        UiCommand::ExpungeMessages { ids } => {
            route_expunge_messages(ids, store, workers, events).await;
        }
        UiCommand::UndeleteMessage { id, dest } => {
            route_message_command(id, store, workers, events, move |id| {
                WorkerCommand::UndeleteMessage { id, dest }
            })
            .await;
        }
        UiCommand::MoveMessage { id, dest } => {
            route_message_command(id, store, workers, events, |id| {
                WorkerCommand::MoveMessage { id, dest }
            })
            .await;
        }
        UiCommand::MoveMessages { ids, dest } => {
            route_move_messages(ids, dest, store, workers, events).await;
        }
        UiCommand::MoveMessageToFolder { id, dest } => {
            route_message_command(id, store, workers, events, move |id| {
                WorkerCommand::MoveMessageToFolder { id, dest }
            })
            .await;
        }
        UiCommand::MoveMessagesToFolder { ids, dest } => {
            route_move_messages_to_folder(ids, dest, store, workers, events).await;
        }
        UiCommand::CopyMessageToFolder { id, dest } => {
            route_message_command(id, store, workers, events, move |id| {
                WorkerCommand::CopyMessageToFolder { id, dest }
            })
            .await;
        }
        UiCommand::RefreshAccount(account_id) => {
            workers.send(account_id, WorkerCommand::Refresh).await;
        }
        UiCommand::Send(draft) => {
            if !workers
                .send(draft.account_id, WorkerCommand::Send(draft))
                .await
            {
                let _ = events.send(SyncEvent::SendProgress {
                    id: MessageId(0),
                    state: SendState::Failed("Could not queue message for sending".to_string()),
                });
            }
        }
        UiCommand::UpsertContact(draft) => {
            if let Err(err) = upsert_contact(draft, store, events).await {
                emit_error(events, None, err.to_string());
            } else if let Err(err) = refresh_virtual_inboxes(store, events).await {
                emit_error(events, None, err.to_string());
            }
        }
        UiCommand::DeleteContacts(ids) => {
            if let Err(err) = delete_contacts(ids, store, events).await {
                emit_error(events, None, err.to_string());
            } else if let Err(err) = refresh_virtual_inboxes(store, events).await {
                emit_error(events, None, err.to_string());
            }
        }
        UiCommand::UpsertGroup(draft) => {
            if let Err(err) = upsert_group(draft, store, events).await {
                emit_error(events, None, err.to_string());
            }
        }
        UiCommand::DeleteGroup(id) => {
            if id == UNAPPROVED_GROUP_ID {
                emit_error(
                    events,
                    None,
                    "Cannot delete the Unapproved group".to_string(),
                );
            } else if let Err(err) = delete_group(id, store, events).await {
                emit_error(events, None, err.to_string());
            }
        }
        UiCommand::PromoteContact(id) => {
            if let Err(err) = promote_contact(id, store, events).await {
                emit_error(events, None, err.to_string());
            } else if let Err(err) = refresh_virtual_inboxes(store, events).await {
                emit_error(events, None, err.to_string());
            }
        }
        UiCommand::BlockContact { id } => {
            if let Err(err) = block_contact(id, store, events).await {
                emit_error(events, None, err.to_string());
            } else if let Err(err) = refresh_virtual_inboxes(store, events).await {
                emit_error(events, None, err.to_string());
            }
        }
        UiCommand::AllowSender {
            email,
            display_name,
        } => {
            if let Err(err) = allow_sender(&email, &display_name, store, events).await {
                emit_error(events, None, err.to_string());
            } else if let Err(err) = refresh_virtual_inboxes(store, events).await {
                emit_error(events, None, err.to_string());
            }
        }
        UiCommand::Compose(_) => {}
        UiCommand::LoadRemoteImages(id) => {
            if let Err(err) = store.set_allow_remote_images(id, true).await {
                emit_error(events, None, err.to_string());
            }
        }
        UiCommand::LoadReactionOriginal { id } => {
            if let Err(err) = crate::worker::emit_reaction_original(store, id, events).await {
                emit_error(events, None, err.to_string());
            }
        }
        UiCommand::LoadReactions { target_id } => match store.get_message(target_id).await {
            Ok(Some(stored)) => {
                let message_id_hdr = stored.message_id_hdr.clone();
                if let Some(hdr) = message_id_hdr.filter(|value| !value.trim().is_empty()) {
                    match store.reactions_for_target(stored.account_id, &hdr).await {
                        Ok(people) => {
                            let _ = events.send(SyncEvent::ReactionsLoaded { target_id, people });
                        }
                        Err(err) => emit_error(events, None, err.to_string()),
                    }
                } else {
                    let _ = events.send(SyncEvent::ReactionsLoaded {
                        target_id,
                        people: Vec::new(),
                    });
                }
            }
            Ok(None) => {
                let _ = events.send(SyncEvent::ReactionsLoaded {
                    target_id,
                    people: Vec::new(),
                });
            }
            Err(err) => emit_error(events, None, err.to_string()),
        },
        UiCommand::OneClickUnsubscribe(id) => {
            let (ok, message) = crate::unsubscribe::one_click_unsubscribe(store, id).await;
            let _ = events.send(SyncEvent::UnsubscribeFinished { id, ok, message });
        }
        UiCommand::SetSenderRemoteImages { emails, allow } => {
            for email in emails {
                if let Err(err) = store.set_remote_image_sender(&email, allow).await {
                    emit_error(events, None, err.to_string());
                    return;
                }
            }
            emit_remote_image_senders_changed(store, events).await;
        }
        UiCommand::UpsertMessageRule(rule) => {
            workers
                .send(rule.account_id, WorkerCommand::UpsertMessageRule(rule))
                .await;
        }
        UiCommand::DeleteMessageRule { id } => {
            route_rule_command(id, store, workers, |id| WorkerCommand::DeleteMessageRule {
                id,
            })
            .await;
        }
        UiCommand::ReorderMessageRules { account, ids } => {
            workers
                .send(account, WorkerCommand::ReorderMessageRules { ids })
                .await;
        }
        UiCommand::RunMessageRules {
            account,
            folder,
            rule_ids,
        } => {
            workers
                .send(account, WorkerCommand::RunMessageRules { folder, rule_ids })
                .await;
        }
        UiCommand::AdoptServerRules(account) => {
            workers.send(account, WorkerCommand::AdoptServerRules).await;
        }
        UiCommand::PersistQueuedSend { id, draft } => {
            let queued = QueuedSend {
                id,
                draft,
                queued_at: Utc::now(),
            };
            if let Err(err) = store.insert_queued_send(&queued).await {
                emit_error(events, None, err.to_string());
            }
        }
        UiCommand::ForgetQueuedSend { id } => {
            if let Err(err) = store.delete_queued_send(id).await {
                emit_error(events, None, err.to_string());
            }
        }
        UiCommand::PgpGenerate {
            account,
            passphrase,
        } => {
            if let Ok(accounts) = store.list_accounts().await
                && let Some(acct) = accounts.iter().find(|a| a.id == account)
                && let Err(err) = crate::pgp::pgp_generate(
                    account,
                    &acct.email,
                    &acct.display_name,
                    passphrase,
                    store,
                    events,
                )
                .await
            {
                emit_error(events, Some(account), err.to_string());
            }
        }
        UiCommand::PgpImport {
            account,
            armored,
            passphrase,
        } => {
            if let Ok(accounts) = store.list_accounts().await
                && let Some(acct) = accounts.iter().find(|a| a.id == account)
                && let Err(err) =
                    crate::pgp::pgp_import(account, &acct.email, armored, passphrase, store, events)
                        .await
            {
                emit_error(events, Some(account), err.to_string());
            }
        }
        UiCommand::PgpExportPublic { account } => {
            if let Err(err) = crate::pgp::pgp_export_public(account, store, events).await {
                emit_error(events, Some(account), err.to_string());
            }
        }
        UiCommand::PgpSetSignOutgoing { account, enabled } => {
            if let Err(err) =
                crate::pgp::pgp_set_sign_outgoing(account, enabled, store, events).await
            {
                emit_error(events, Some(account), err.to_string());
            }
        }
        UiCommand::SetSmtpEnvelopePolicy {
            account,
            same_domain_identity,
        } => {
            let policy = if same_domain_identity {
                SmtpEnvelopePolicy::SameDomainIdentity
            } else {
                SmtpEnvelopePolicy::Mailbox
            };
            if let Err(err) = store.set_smtp_envelope_policy(account, policy).await {
                emit_error(events, Some(account), err.to_string());
            } else {
                let _ = events.send(SyncEvent::SmtpEnvelopePolicy { account, policy });
            }
        }
        UiCommand::SetCatchAllDomains { account, domains } => {
            if let Err(err) = store.set_catch_all_domains(account, domains).await {
                emit_error(events, Some(account), err.to_string());
            } else {
                emit_catch_all_domains(store, account, events).await;
            }
        }
        UiCommand::RefreshSmtpEnvelopePolicy { account } => {
            match store.get_smtp_envelope_policy(account).await {
                Ok(policy) => {
                    let _ = events.send(SyncEvent::SmtpEnvelopePolicy { account, policy });
                }
                Err(err) => emit_error(events, Some(account), err.to_string()),
            }
        }
        UiCommand::PgpDeleteKey { account } => {
            if let Ok(accounts) = store.list_accounts().await
                && let Some(acct) = accounts.iter().find(|a| a.id == account)
                && let Err(err) =
                    crate::pgp::pgp_delete_key(account, &acct.email, store, events).await
            {
                emit_error(events, Some(account), err.to_string());
            }
        }
        UiCommand::PgpAcceptPeer {
            account,
            email,
            armored,
        } => {
            if let Err(err) =
                crate::pgp::pgp_accept_peer(account, email, armored, store, events).await
            {
                emit_error(events, Some(account), err.to_string());
            }
        }
        UiCommand::PgpRefreshState { account } => {
            crate::pgp::emit_pgp_state(store, account, events).await;
        }
        UiCommand::UpsertSignature(signature) => {
            workers
                .send(
                    signature.account_id,
                    WorkerCommand::UpsertSignature(signature),
                )
                .await;
        }
        UiCommand::UpsertIdentity(identity) => {
            workers
                .send(identity.account_id, WorkerCommand::UpsertIdentity(identity))
                .await;
        }
        UiCommand::DeleteIdentity { id } => {
            route_identity_command(id, store, workers).await;
        }
        UiCommand::UpsertSnippet(snippet) => {
            if let Err(err) = upsert_snippet(snippet, store, events).await {
                emit_error(events, None, err.to_string());
            }
        }
        UiCommand::DeleteSnippet(id) => {
            if let Err(err) = delete_snippet(id, store, events).await {
                emit_error(events, None, err.to_string());
            }
        }
        UiCommand::SaveImapDraft(draft) => {
            workers
                .send(draft.account_id, WorkerCommand::SaveImapDraft(draft))
                .await;
        }
        UiCommand::DeleteImapDraft {
            account,
            folder,
            uid,
            message_id,
        } => {
            workers
                .send(
                    account,
                    WorkerCommand::DeleteImapDraft {
                        folder,
                        uid,
                        message_id,
                    },
                )
                .await;
        }
        UiCommand::Extension(msg) => match workers.extensions.command_handler(&msg.extension) {
            Some(extension) => {
                let ctx = SyncContext::new(store.clone(), events.clone(), workers.clone());
                extension.handle_command(msg.payload, ctx).await;
            }
            None => warn!(extension = %msg.extension, "no handler for extension command"),
        },
    }
}

async fn emit_contacts_changed(store: &StoreHandle, events: &flume::Sender<SyncEvent>) {
    match store.contacts_snapshot().await {
        Ok((contacts, groups)) => {
            let _ = events.send(SyncEvent::ContactsChanged { contacts, groups });
        }
        Err(err) => emit_error(events, None, err.to_string()),
    }
}

async fn emit_snippets_changed(store: &StoreHandle, events: &flume::Sender<SyncEvent>) {
    match store.list_snippets().await {
        Ok(snippets) => {
            let _ = events.send(SyncEvent::SnippetsUpdated { snippets });
        }
        Err(err) => emit_error(events, None, err.to_string()),
    }
}

async fn emit_account_compose_state(
    store: &StoreHandle,
    account: AccountId,
    events: &flume::Sender<SyncEvent>,
) {
    match store.get_signature(account).await {
        Ok(signature) => {
            let _ = events.send(SyncEvent::SignaturesUpdated { account, signature });
        }
        Err(err) => emit_error(events, Some(account), err.to_string()),
    }
    match store.list_identities(account).await {
        Ok(identities) => {
            let _ = events.send(SyncEvent::IdentitiesUpdated {
                account,
                identities,
            });
        }
        Err(err) => emit_error(events, Some(account), err.to_string()),
    }
    emit_catch_all_domains(store, account, events).await;
}

async fn emit_catch_all_domains(
    store: &StoreHandle,
    account: AccountId,
    events: &flume::Sender<SyncEvent>,
) {
    match store.get_catch_all_domains(account).await {
        Ok(domains) => {
            let _ = events.send(SyncEvent::CatchAllDomainsUpdated { account, domains });
        }
        Err(err) => emit_error(events, Some(account), err.to_string()),
    }
}

async fn upsert_snippet(
    snippet: alcedo_core::Snippet,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
) -> Result<()> {
    if let Err(message) = snippet.validate() {
        return Err(Error::Store(message.to_string()));
    }
    store.upsert_snippet(&snippet).await?;
    emit_snippets_changed(store, events).await;
    Ok(())
}

async fn delete_snippet(
    id: SnippetId,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
) -> Result<()> {
    store.delete_snippet(id).await?;
    emit_snippets_changed(store, events).await;
    Ok(())
}

async fn route_identity_command(id: IdentityId, store: &StoreHandle, workers: &SharedWorkers) {
    match store.list_accounts().await {
        Ok(accounts) => {
            for account in accounts {
                if let Ok(identities) = store.list_identities(account.id).await
                    && identities.iter().any(|identity| identity.id == id)
                {
                    workers
                        .send(account.id, WorkerCommand::DeleteIdentity { id })
                        .await;
                    return;
                }
            }
            warn!(?id, "delete identity: identity not found");
        }
        Err(err) => warn!(error = %err, "delete identity: list accounts failed"),
    }
}

async fn emit_sent_recipients(store: &StoreHandle, events: &flume::Sender<SyncEvent>) {
    match store.list_sent_recipients().await {
        Ok(addresses) => {
            let _ = events.send(SyncEvent::SentRecipientsChanged { addresses });
        }
        Err(err) => emit_error(events, None, err.to_string()),
    }
}

async fn emit_remote_image_senders_changed(store: &StoreHandle, events: &flume::Sender<SyncEvent>) {
    match store.list_remote_image_senders().await {
        Ok(emails) => {
            let _ = events.send(SyncEvent::RemoteImageSendersChanged { emails });
        }
        Err(err) => emit_error(events, None, err.to_string()),
    }
}

async fn emit_queued_sends_recovered(store: &StoreHandle, events: &flume::Sender<SyncEvent>) {
    match store.list_queued_sends().await {
        Ok(list) => {
            if !list.is_empty() {
                let _ = events.send(SyncEvent::QueuedSendsRecovered(list));
            }
        }
        Err(err) => emit_error(events, None, err.to_string()),
    }
}

async fn upsert_contact(
    draft: ContactDraft,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
) -> Result<()> {
    store.upsert_contact(draft).await?;
    emit_contacts_changed(store, events).await;
    emit_unread_counts(store, events).await?;
    Ok(())
}

async fn delete_contacts(
    ids: Vec<ContactId>,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
) -> Result<()> {
    if ids.is_empty() {
        return Ok(());
    }
    store.delete_contacts(ids).await?;
    emit_contacts_changed(store, events).await;
    emit_unread_counts(store, events).await?;
    Ok(())
}

async fn upsert_group(
    draft: GroupDraft,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
) -> Result<()> {
    store.upsert_group(draft).await?;
    emit_contacts_changed(store, events).await;
    Ok(())
}

async fn delete_group(
    id: GroupId,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
) -> Result<()> {
    store.delete_group(id).await?;
    emit_contacts_changed(store, events).await;
    Ok(())
}

async fn promote_contact(
    id: ContactId,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
) -> Result<()> {
    store.promote_contact(id).await?;
    emit_contacts_changed(store, events).await;
    emit_unread_counts(store, events).await?;
    Ok(())
}

async fn block_contact(
    id: ContactId,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
) -> Result<()> {
    store.block_contact(id).await?;
    emit_contacts_changed(store, events).await;
    emit_unread_counts(store, events).await?;
    Ok(())
}

async fn allow_sender(
    email: &str,
    display_name: &str,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
) -> Result<()> {
    store.allow_sender(email, display_name).await?;
    emit_contacts_changed(store, events).await;
    emit_unread_counts(store, events).await?;
    Ok(())
}

async fn route_rule_command(
    id: RuleId,
    store: &StoreHandle,
    workers: &SharedWorkers,
    build: impl FnOnce(RuleId) -> WorkerCommand,
) {
    match store.list_accounts().await {
        Ok(accounts) => {
            for account in accounts {
                if let Ok(rules) = store.list_rules(account.id).await
                    && rules.iter().any(|rule| rule.id == id)
                {
                    workers.send(account.id, build(id)).await;
                    return;
                }
            }
            warn!(?id, "delete rule: rule not found");
        }
        Err(err) => warn!(error = %err, "delete rule: list accounts failed"),
    }
}

async fn request_folder_sync(store: &StoreHandle, workers: &SharedWorkers, folder: FolderId) {
    match store.get_folder(folder).await {
        Ok(Some(f)) if f.selectable => {
            workers
                .send(f.account_id, WorkerCommand::SyncFolder(folder))
                .await;
        }
        Ok(_) => {}
        Err(err) => warn!(?folder, error = %err, "store get_folder failed"),
    }
}

fn route_selectable_folder(
    folder: FolderId,
    store: &StoreHandle,
    workers: &SharedWorkers,
    build: impl FnOnce(FolderId) -> WorkerCommand,
) {
    let store = store.clone();
    let workers = workers.clone_for_route();
    let command = build(folder);

    tokio::spawn(async move {
        match store.get_folder(folder).await {
            Ok(Some(f)) if f.selectable => {
                workers.send(f.account_id, command).await;
            }
            Ok(_) => {}
            Err(err) => warn!(?folder, error = %err, "store get_folder failed"),
        }
    });
}

fn route_delete_folder_on_or_before(
    folder: FolderId,
    through: chrono::NaiveDate,
    store: &StoreHandle,
    workers: &SharedWorkers,
    events: &flume::Sender<SyncEvent>,
) {
    let store = store.clone();
    let workers = workers.clone_for_route();
    let events = events.clone();

    tokio::spawn(async move {
        match store.get_folder(folder).await {
            Ok(Some(f)) if f.selectable => {
                if !workers
                    .send(
                        f.account_id,
                        WorkerCommand::DeleteFolderOnOrBefore {
                            folder_id: folder,
                            through,
                        },
                    )
                    .await
                {
                    emit_error(
                        &events,
                        Some(f.account_id),
                        "Mail account is not connected".to_string(),
                    );
                }
            }
            Ok(Some(f)) => emit_error(
                &events,
                Some(f.account_id),
                "This folder cannot be modified".to_string(),
            ),
            Ok(None) => emit_error(&events, None, "Folder not found".to_string()),
            Err(err) => emit_error(&events, None, err.to_string()),
        }
    });
}

async fn route_mutable_folder(
    folder_id: FolderId,
    store: &StoreHandle,
    workers: &SharedWorkers,
    events: &flume::Sender<SyncEvent>,
    build: impl FnOnce(FolderId) -> WorkerCommand,
) {
    match store.get_folder(folder_id).await {
        Ok(Some(folder)) if folder_is_mutable(&folder) => {
            workers.send(folder.account_id, build(folder_id)).await;
        }
        Ok(Some(folder)) => {
            emit_error(
                events,
                Some(folder.account_id),
                "This folder cannot be modified".to_string(),
            );
        }
        Ok(None) => emit_error(events, None, "Folder not found".to_string()),
        Err(err) => emit_error(events, None, err.to_string()),
    }
}

fn route_open_thread(
    thread_id: ThreadId,
    folder_ids: Option<Vec<FolderId>>,
    store: &StoreHandle,
    workers: &SharedWorkers,
    events: &flume::Sender<SyncEvent>,
) {
    let store = store.clone();
    let workers = workers.clone_for_route();
    let events = events.clone();

    tokio::spawn(async move {
        match store
            .thread_envelopes(thread_id, folder_ids.as_deref(), true)
            .await
        {
            Ok(envelopes) => {
                if envelopes.is_empty() {
                    warn!(?thread_id, "open thread: no envelopes");
                    return;
                }
                let _ = events.send(SyncEvent::ThreadReady {
                    thread_id,
                    envelopes: envelopes.clone(),
                });
                if let Some(latest) = envelopes.last() {
                    let id = latest.id;
                    let account_id = latest.account_id;
                    if !workers
                        .send(account_id, WorkerCommand::OpenMessage(id))
                        .await
                    {
                        emit_error(
                            &events,
                            Some(account_id),
                            "Account is not connected".to_string(),
                        );
                    }
                }
            }
            Err(err) => warn!(?thread_id, error = %err, "thread_envelopes failed"),
        }
    });
}

async fn route_open_message(
    id: MessageId,
    store: &StoreHandle,
    workers: &SharedWorkers,
    events: &flume::Sender<SyncEvent>,
) {
    match store.get_message(id).await {
        Ok(Some(msg)) => {
            let local_only = match store.get_folder(msg.folder_id).await {
                Ok(Some(folder)) => !folder_is_syncable(&folder),
                Ok(None) => false,
                Err(err) => {
                    warn!(?id, error = %err, "open: get_folder failed");
                    false
                }
            };
            if local_only {
                if let Err(err) = emit_local_only_message_body(store, &msg, events).await {
                    emit_error(events, Some(msg.account_id), err.to_string());
                }
                return;
            }
            if open_message_emits_local_body(&msg) {
                emit_body_ready_from_stored_with_pending(
                    store,
                    &msg,
                    events,
                    needs_attachment_bytes(&msg),
                )
                .await;
                if needs_attachment_bytes(&msg)
                    || (needs_attachment_rescan(&msg) && !attachment_rescan_is_claimed(id))
                {
                    workers
                        .send(msg.account_id, WorkerCommand::OpenMessage(id))
                        .await;
                }
            } else if !workers
                .send(msg.account_id, WorkerCommand::OpenMessage(id))
                .await
            {
                emit_error(
                    events,
                    Some(msg.account_id),
                    "Account is not connected".to_string(),
                );
            }
        }
        Ok(None) => warn!(?id, "open: message not in store"),
        Err(err) => warn!(?id, error = %err, "store get_message failed"),
    }
}

async fn route_prefetch_message(
    id: MessageId,
    store: &StoreHandle,
    workers: &SharedWorkers,
    events: &flume::Sender<SyncEvent>,
) {
    match store.get_message(id).await {
        Ok(Some(msg)) => {
            let local_only = match store.get_folder(msg.folder_id).await {
                Ok(Some(folder)) => !folder_is_syncable(&folder),
                Ok(None) => false,
                Err(err) => {
                    warn!(?id, error = %err, "prefetch: get_folder failed");
                    false
                }
            };
            if local_only || open_message_emits_local_body(&msg) {
                emit_body_ready_from_stored_with_pending(
                    store,
                    &msg,
                    events,
                    needs_attachment_bytes(&msg),
                )
                .await;
                return;
            }
            if !workers
                .send(msg.account_id, WorkerCommand::PrefetchMessage(id))
                .await
            {
                warn!(?id, "prefetch skipped; account is not connected");
            }
        }
        Ok(None) => warn!(?id, "prefetch: message not in store"),
        Err(err) => warn!(?id, error = %err, "prefetch get_message failed"),
    }
}

fn open_message_emits_local_body(msg: &alcedo_store::StoredMessage) -> bool {
    stored_has_local_body(msg)
}

/// Cached-body open already emitted `BodyReady`. Do not follow up on IMAP for a
/// missing List-Unsubscribe JSON — envelope FETCH and full-body open persist it.
/// Re-enter the worker when old incoming-rule saves may have dropped files, or
/// when a cached body/subject was stored with charset replacement characters.
#[cfg(test)]
fn cached_open_needs_imap_followup(msg: &alcedo_store::StoredMessage) -> bool {
    needs_attachment_rescan(msg) || stored_body_needs_charset_rescan(msg)
}

fn attachment_rescan_claims() -> &'static Mutex<HashSet<MessageId>> {
    static CLAIMED: OnceLock<Mutex<HashSet<MessageId>>> = OnceLock::new();
    CLAIMED.get_or_init(|| Mutex::new(HashSet::new()))
}

pub(crate) fn attachment_rescan_is_claimed(id: MessageId) -> bool {
    attachment_rescan_claims()
        .lock()
        .map(|set| set.contains(&id))
        .unwrap_or(false)
}

pub(crate) fn claim_attachment_rescan(id: MessageId) -> bool {
    attachment_rescan_claims()
        .lock()
        .map(|mut set| set.insert(id))
        .unwrap_or(true)
}

pub(crate) fn release_attachment_rescan(id: MessageId) {
    if let Ok(mut set) = attachment_rescan_claims().lock() {
        set.remove(&id);
    }
}

async fn route_message_command(
    id: MessageId,
    store: &StoreHandle,
    workers: &SharedWorkers,
    events: &flume::Sender<SyncEvent>,
    build: impl FnOnce(MessageId) -> WorkerCommand,
) {
    let command = build(id);
    match store.get_message(id).await {
        Ok(Some(msg)) => {
            let account_id = msg.account_id;
            if !workers.send(account_id, command).await {
                emit_error(
                    events,
                    Some(account_id),
                    "Account is not connected".to_string(),
                );
            }
        }
        Ok(None) => {
            warn!(?id, "open/set_flag/delete/move: message not in store");
            emit_error(events, None, "Message not found".to_string());
        }
        Err(err) => {
            warn!(?id, error = %err, "store get_message failed");
            emit_error(events, None, err.to_string());
        }
    }
}

#[derive(Clone)]
struct WorkerRoute {
    workers: HashMap<AccountId, WorkerHandle>,
}

impl WorkerRoute {
    async fn send(&self, account_id: AccountId, command: WorkerCommand) -> bool {
        if let Some(worker) = self.workers.get(&account_id) {
            worker.send_async(command).await
        } else {
            warn!(?account_id, "no worker for routed command");
            false
        }
    }
}

async fn add_account(
    draft: AccountDraft,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
    workers: &SharedWorkers,
    handle: &Handle,
) -> Result<()> {
    match &draft.auth {
        AuthMethod::Password => {
            if let Err(message) = draft.validate_password_auth() {
                return Err(Error::Auth(message.to_string()));
            }
        }
        AuthMethod::OAuth2 { .. } => {
            if let Err(message) = draft.validate_oauth_auth() {
                return Err(Error::Auth(message.to_string()));
            }
        }
    }

    let account_id = AccountId(Uuid::new_v4());
    let password = draft.password.clone();
    let smtp_password = draft.smtp_password.clone();
    let oauth_tokens = draft.oauth_tokens.clone();

    let account = account_from_draft(draft, account_id);

    if let Some(password) = password {
        store_secret(
            account_id.0,
            &account.email,
            SecretKind::Password,
            &password,
        )
        .await?;
    }

    if account.uses_split_credentials() {
        let smtp_password = smtp_password
            .as_deref()
            .filter(|p| !p.is_empty())
            .ok_or_else(|| Error::Auth("SMTP password is required".to_string()))?;
        store_secret(
            account_id.0,
            &account.email,
            SecretKind::SmtpPassword,
            smtp_password,
        )
        .await?;
    }

    if matches!(account.auth, AuthMethod::OAuth2 { .. }) {
        let refresh_token = oauth_tokens
            .as_ref()
            .and_then(|tokens| tokens.refresh_token.as_deref())
            .ok_or_else(|| Error::Auth("OAuth refresh token required".to_string()))?;
        store_secret(
            account_id.0,
            &account.email,
            SecretKind::RefreshToken,
            refresh_token,
        )
        .await?;
    }

    store.upsert_account(&account).await?;
    let _ = events.send(SyncEvent::AccountAdded(account.clone()));
    emit_account_compose_state(store, account_id, events).await;
    workers.spawn(account, store.clone(), events.clone(), handle.clone());
    Ok(())
}

async fn remove_account(
    account_id: AccountId,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
    workers: &SharedWorkers,
) -> Result<()> {
    store.delete_account(account_id).await?;
    workers.remove(account_id);
    delete_account_secrets(account_id).await;
    let _ = events.send(SyncEvent::AccountRemoved(account_id));
    Ok(())
}

async fn delete_account_secrets(account_id: AccountId) {
    for kind in [
        SecretKind::Password,
        SecretKind::SmtpPassword,
        SecretKind::RefreshToken,
        SecretKind::AccessToken,
        SecretKind::PgpSecretKey,
        SecretKind::PgpPassphrase,
    ] {
        if let Err(err) = delete_secret(account_id.0, kind).await {
            warn!(?account_id, ?kind, error = %err, "failed to delete keyring secret");
        }
    }
}

async fn update_account(
    id: AccountId,
    update: AccountSettingsUpdate,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
    workers: &SharedWorkers,
    handle: &Handle,
) -> Result<()> {
    let accounts = store.list_accounts().await?;
    let email = update.email.trim().to_string();
    if accounts
        .iter()
        .any(|other| other.id != id && other.email.eq_ignore_ascii_case(&email))
    {
        return Err(Error::Other(
            "Another account already uses this email".into(),
        ));
    }
    let mut account = accounts
        .into_iter()
        .find(|account| account.id == id)
        .ok_or_else(|| Error::Store("account not found".to_string()))?;

    update
        .validate_for_auth(&account.auth)
        .map_err(|message| Error::Auth(message.to_string()))?;
    let identities = store.list_identities(id).await?;
    if identities
        .iter()
        .any(|identity| identity.email.eq_ignore_ascii_case(&email))
    {
        return Err(Error::Other(
            "This address is already a From address on this account".into(),
        ));
    }
    let email_changed = account.email != email;

    let display_name = update.display_name.trim();
    account.email = email;
    account.display_name = if display_name.is_empty() {
        display_name_from_email(&account.email)
    } else {
        display_name.to_string()
    };
    account.sender_name = alcedo_core::sanitize_sender_name(&update.sender_name);
    account.avatar_text = sanitize_avatar_text(&update.avatar_text);
    account.accent_color = sanitize_accent_color(&update.accent_color);
    account.avatar_image = update.avatar_image;
    account.username = alcedo_core::login_name_from(&update.username, &account.email);
    account.imap_host = update.imap_host;
    account.imap_port = update.imap_port;
    account.smtp_host = update.smtp_host;
    account.smtp_port = update.smtp_port;
    account.smtp_starttls = update.smtp_starttls;

    match &account.auth {
        AuthMethod::OAuth2 { .. } => {
            account.split_credentials = false;
            account.smtp_username.clear();
        }
        AuthMethod::Password if update.split_credentials => {
            account.split_credentials = true;
            account.smtp_username =
                alcedo_core::login_name_from(&update.smtp_username, &account.email);
        }
        AuthMethod::Password => {
            account.split_credentials = false;
            account.smtp_username.clear();
        }
    }

    let replacing_shared = update
        .password
        .as_ref()
        .is_some_and(|password| !password.is_empty());
    let smtp_password_provided = update
        .smtp_password
        .as_ref()
        .is_some_and(|password| !password.is_empty());
    let prior_shared =
        if account.uses_split_credentials() && !smtp_password_provided && replacing_shared {
            get_secret(id.0, SecretKind::Password).await?
        } else {
            None
        };

    if replacing_shared {
        let password = update.password.as_deref().expect("checked non-empty");
        store_secret(id.0, &account.email, SecretKind::Password, password).await?;
    }

    if account.uses_split_credentials() {
        match &update.smtp_password {
            Some(smtp_password) if !smtp_password.is_empty() => {
                store_secret(
                    id.0,
                    &account.email,
                    SecretKind::SmtpPassword,
                    smtp_password,
                )
                .await?;
            }
            _ => {
                if get_secret(id.0, SecretKind::SmtpPassword).await?.is_none() {
                    let shared = match prior_shared {
                        Some(prior) => prior,
                        None => get_secret(id.0, SecretKind::Password)
                            .await?
                            .ok_or_else(|| {
                                Error::Auth("no password stored for account".to_string())
                            })?,
                    };
                    store_secret(id.0, &account.email, SecretKind::SmtpPassword, &shared).await?;
                }
            }
        }
    } else if matches!(account.auth, AuthMethod::Password) {
        delete_secret(id.0, SecretKind::SmtpPassword).await?;
    }

    if let Some(tokens) = &update.oauth_tokens {
        let refresh_token = tokens
            .refresh_token
            .as_deref()
            .filter(|token| !token.is_empty())
            .ok_or_else(|| Error::Auth("OAuth refresh token required".to_string()))?;
        store_secret(
            id.0,
            &account.email,
            SecretKind::RefreshToken,
            refresh_token,
        )
        .await?;
    }

    store.upsert_account(&account).await?;
    if email_changed && let Err(err) = relabel_account_secrets(id.0, &account.email).await {
        warn!(?id, error = %err, "relabel keyring after email change failed");
    }
    workers.remove(id);
    workers.spawn(
        account.clone(),
        store.clone(),
        events.clone(),
        handle.clone(),
    );
    let _ = events.send(SyncEvent::AccountUpdated(account));
    Ok(())
}

fn account_from_draft(draft: AccountDraft, id: AccountId) -> Account {
    let accent_color = accent_for_id(id);
    let username = alcedo_core::login_name_from(&draft.username, &draft.email);

    let split_credentials = matches!(draft.auth, AuthMethod::Password) && draft.split_credentials;
    let smtp_username = if split_credentials {
        alcedo_core::login_name_from(&draft.smtp_username, &draft.email)
    } else {
        String::new()
    };

    Account {
        id,
        email: draft.email,
        username,
        split_credentials,
        smtp_username,
        display_name: draft.display_name,
        sender_name: String::new(),
        imap_host: draft.imap_host,
        imap_port: draft.imap_port,
        smtp_host: draft.smtp_host,
        smtp_port: draft.smtp_port,
        smtp_starttls: draft.smtp_starttls,
        auth: draft.auth,
        accent_color,
        avatar_text: String::new(),
        avatar_image: Vec::new(),
    }
}

fn accent_for_id(id: AccountId) -> String {
    let idx = id.0.as_bytes().first().copied().unwrap_or(0) as usize % ACCENT_COLORS.len();
    ACCENT_COLORS[idx].to_string()
}

async fn group_ids_by_account(
    ids: Vec<MessageId>,
    store: &StoreHandle,
    what: &str,
) -> HashMap<AccountId, Vec<MessageId>> {
    let mut by_account: HashMap<AccountId, Vec<MessageId>> = HashMap::new();
    if ids.is_empty() {
        return by_account;
    }
    match store.message_locators(&ids).await {
        Ok(locators) => {
            let found: HashSet<MessageId> = locators.iter().map(|locator| locator.id).collect();
            for id in ids {
                if !found.contains(&id) {
                    warn!(?id, "{what}: message not in store");
                }
            }
            for locator in locators {
                by_account
                    .entry(locator.account_id)
                    .or_default()
                    .push(locator.id);
            }
        }
        Err(err) => warn!(error = %err, "{what}: message_locators failed"),
    }
    by_account
}

async fn route_delete_messages(
    ids: Vec<MessageId>,
    store: &StoreHandle,
    workers: &SharedWorkers,
    events: &flume::Sender<SyncEvent>,
) {
    let by_account = group_ids_by_account(ids.clone(), store, "delete messages").await;
    if by_account.is_empty() {
        if !ids.is_empty() {
            let _ = events.send(SyncEvent::MessageDeleteFailed {
                ids,
                reason: "Couldn't find messages to delete".to_string(),
            });
        }
        return;
    }
    for (account_id, message_ids) in by_account {
        if !workers
            .send(
                account_id,
                WorkerCommand::DeleteMessages {
                    ids: message_ids.clone(),
                },
            )
            .await
        {
            let _ = events.send(SyncEvent::MessageDeleteFailed {
                ids: message_ids,
                reason: "Account is not connected".to_string(),
            });
        }
    }
}

async fn route_expunge_messages(
    ids: Vec<MessageId>,
    store: &StoreHandle,
    workers: &SharedWorkers,
    events: &flume::Sender<SyncEvent>,
) {
    let by_account = group_ids_by_account(ids.clone(), store, "expunge messages").await;
    if by_account.is_empty() {
        if !ids.is_empty() {
            let _ = events.send(SyncEvent::MessageDeleteFailed {
                ids,
                reason: "Couldn't find messages to delete".to_string(),
            });
        }
        return;
    }
    for (account_id, message_ids) in by_account {
        if !workers
            .send(
                account_id,
                WorkerCommand::ExpungeMessages {
                    ids: message_ids.clone(),
                },
            )
            .await
        {
            let _ = events.send(SyncEvent::MessageDeleteFailed {
                ids: message_ids,
                reason: "Account is not connected".to_string(),
            });
        }
    }
}

async fn route_set_flags(
    ids: Vec<MessageId>,
    seen: Option<bool>,
    flagged: Option<bool>,
    store: &StoreHandle,
    workers: &SharedWorkers,
    events: &flume::Sender<SyncEvent>,
) {
    let by_account = group_ids_by_account(ids, store, "set flags").await;
    for (account_id, message_ids) in by_account {
        if !workers
            .send(
                account_id,
                WorkerCommand::SetFlags {
                    ids: message_ids,
                    seen,
                    flagged,
                },
            )
            .await
        {
            emit_error(
                events,
                Some(account_id),
                "Account is not connected".to_string(),
            );
        }
    }
}

async fn route_move_messages(
    ids: Vec<MessageId>,
    dest: SpecialUse,
    store: &StoreHandle,
    workers: &SharedWorkers,
    events: &flume::Sender<SyncEvent>,
) {
    let by_account = group_ids_by_account(ids, store, "move messages").await;
    for (account_id, message_ids) in by_account {
        if !workers
            .send(
                account_id,
                WorkerCommand::MoveMessages {
                    ids: message_ids,
                    dest: dest.clone(),
                },
            )
            .await
        {
            emit_error(
                events,
                Some(account_id),
                "Account is not connected".to_string(),
            );
        }
    }
}

async fn route_move_messages_to_folder(
    ids: Vec<MessageId>,
    dest: FolderId,
    store: &StoreHandle,
    workers: &SharedWorkers,
    events: &flume::Sender<SyncEvent>,
) {
    let by_account = group_ids_by_account(ids, store, "move messages to folder").await;
    for (account_id, message_ids) in by_account {
        if !workers
            .send(
                account_id,
                WorkerCommand::MoveMessagesToFolder {
                    ids: message_ids,
                    dest,
                },
            )
            .await
        {
            emit_error(
                events,
                Some(account_id),
                "Account is not connected".to_string(),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alcedo_core::{AccountId, FolderId, MessageId, ThreadId};
    use alcedo_store::StoredMessage;
    use chrono::Utc;
    use uuid::Uuid;

    fn minimal_stored_message(body_text: Option<&str>, body_html: Option<&str>) -> StoredMessage {
        StoredMessage {
            id: MessageId(1),
            account_id: AccountId(Uuid::nil()),
            folder_id: FolderId(1),
            uid: 1,
            thread_id: ThreadId(Uuid::nil()),
            date: Utc::now(),
            subject: String::new(),
            from: Vec::new(),
            to: Vec::new(),
            cc: Vec::new(),
            reply_to: Vec::new(),
            sender: Vec::new(),
            snippet: String::new(),
            body_text: body_text.map(str::to_string),
            body_html: body_html.map(str::to_string),
            message_id_hdr: None,
            in_reply_to: None,
            references_hdr: None,
            seen: false,
            flagged: false,
            answered: false,
            draft: false,
            has_attachments: false,
            allow_remote_images: false,
            list_unsubscribe: None,
            pgp_status: None,
            account_accent: String::new(),
            account_avatar: String::new(),
            attachments: vec![],
        }
    }

    #[test]
    fn open_message_emits_local_body_with_cached_text() {
        assert!(open_message_emits_local_body(&minimal_stored_message(
            Some("hello"),
            None
        )));
    }

    #[test]
    fn open_message_emits_local_body_false_without_body() {
        assert!(!open_message_emits_local_body(&minimal_stored_message(
            None, None
        )));
    }

    #[test]
    fn cached_open_does_not_follow_up_for_missing_list_unsubscribe() {
        let mut msg = minimal_stored_message(Some("hello"), None);
        msg.list_unsubscribe = None;
        msg.attachments = vec![alcedo_store::StoredAttachment {
            id: 1,
            filename: "doc.pdf".to_string(),
            mime: "application/pdf".to_string(),
            sha256: "aa".repeat(32),
            size: 12,
            cid: None,
        }];
        assert!(!cached_open_needs_imap_followup(&msg));
    }

    #[test]
    fn cached_open_follows_up_for_attachment_rescan() {
        let msg = minimal_stored_message(Some("hello"), None);
        assert!(cached_open_needs_imap_followup(&msg));
    }

    #[test]
    fn cached_open_follows_up_for_charset_replacement() {
        let mut msg = minimal_stored_message(Some("Sep 2025\u{FFFD}VFeb"), None);
        msg.attachments = vec![alcedo_store::StoredAttachment {
            id: 1,
            filename: "doc.pdf".to_string(),
            mime: "application/pdf".to_string(),
            sha256: "aa".repeat(32),
            size: 12,
            cid: None,
        }];
        assert!(cached_open_needs_imap_followup(&msg));
    }

    #[test]
    fn cached_open_follows_up_for_charset_replacement_in_subject() {
        let mut msg = minimal_stored_message(Some("plain"), None);
        msg.subject = "Request \u{FFFD}X bank".to_string();
        msg.attachments = vec![alcedo_store::StoredAttachment {
            id: 1,
            filename: "doc.pdf".to_string(),
            mime: "application/pdf".to_string(),
            sha256: "aa".repeat(32),
            size: 12,
            cid: None,
        }];
        assert!(cached_open_needs_imap_followup(&msg));
    }

    #[test]
    fn release_attachment_rescan_clears_the_claim() {
        let id = MessageId(9_001_001);
        assert!(claim_attachment_rescan(id));
        assert!(!claim_attachment_rescan(id));
        assert!(attachment_rescan_is_claimed(id));
        release_attachment_rescan(id);
        assert!(!attachment_rescan_is_claimed(id));
        assert!(claim_attachment_rescan(id));
        release_attachment_rescan(id);
    }
}
