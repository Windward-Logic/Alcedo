//! Unified inbox and folder message queries (P7).

use alcedo_core::{EnvelopeListKind, FolderId, MessageListMode, Result, SyncEvent};
use alcedo_store::StoreHandle;
use std::sync::Mutex;
use tracing::warn;

const LIST_LIMIT: u32 = 100;

static LIST_MODE: Mutex<MessageListMode> = Mutex::new(MessageListMode::Conversation);

pub fn set_list_mode(mode: MessageListMode) {
    if let Ok(mut guard) = LIST_MODE.lock() {
        *guard = mode;
    }
}

pub fn list_mode() -> MessageListMode {
    LIST_MODE
        .lock()
        .map(|g| *g)
        .unwrap_or(MessageListMode::Conversation)
}

/// Load the unified inbox view and emit list results.
pub async fn select_unified_inbox(
    store: &StoreHandle,
    mode: MessageListMode,
    events: &flume::Sender<SyncEvent>,
) -> Result<()> {
    emit_virtual_inbox_list(store, mode, EnvelopeListKind::UnifiedInbox, events).await
}

async fn emit_virtual_inbox_list(
    store: &StoreHandle,
    mode: MessageListMode,
    kind: EnvelopeListKind,
    events: &flume::Sender<SyncEvent>,
) -> Result<()> {
    match mode {
        MessageListMode::Conversation => {
            let conversations = match kind {
                EnvelopeListKind::UnifiedInbox => {
                    store.unified_inbox_conversations(LIST_LIMIT, 0).await?
                }
                _ => Vec::new(),
            };
            let _ = events.send(SyncEvent::ConversationsReplaced {
                conversations,
                kind,
            });
        }
        MessageListMode::Flat => {
            let envelopes = match kind {
                EnvelopeListKind::UnifiedInbox => store.unified_inbox(LIST_LIMIT, 0).await?,
                _ => Vec::new(),
            };
            let _ = events.send(SyncEvent::EnvelopesReplaced { envelopes, kind });
        }
    }
    Ok(())
}

/// Refresh the unified inbox after inbox mutations.
pub async fn refresh_virtual_inboxes(
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
) -> Result<()> {
    let mode = list_mode();
    select_unified_inbox(store, mode, events).await
}

/// Every live envelope in `folder`, newest first.
pub async fn load_folder_envelopes(
    store: &StoreHandle,
    folder: FolderId,
) -> Result<Vec<alcedo_core::Envelope>> {
    let count = store.folder_live_count(folder).await?;
    if count == 0 {
        return Ok(Vec::new());
    }
    store.folder_messages(folder, count, 0).await
}

/// Load messages for a single folder and emit list results.
pub async fn select_folder(
    store: &StoreHandle,
    folder: alcedo_core::FolderId,
    mode: MessageListMode,
    events: &flume::Sender<SyncEvent>,
) -> Result<()> {
    match mode {
        MessageListMode::Conversation => {
            let conversations = store.folder_conversations(folder).await?;
            let _ = events.send(SyncEvent::ConversationsReplaced {
                conversations,
                kind: alcedo_core::EnvelopeListKind::Folder(folder),
            });
        }
        MessageListMode::Flat => {
            let envelopes = load_folder_envelopes(store, folder).await?;
            let _ = events.send(SyncEvent::EnvelopesReplaced {
                envelopes,
                kind: alcedo_core::EnvelopeListKind::Folder(folder),
            });
        }
    }
    Ok(())
}

/// FTS search and emit results.
pub async fn search(
    store: &StoreHandle,
    query: &str,
    folder_ids: Option<&[FolderId]>,
    mode: MessageListMode,
    events: &flume::Sender<SyncEvent>,
) -> Result<()> {
    match mode {
        MessageListMode::Conversation => {
            let conversations = store
                .search_conversations(query, folder_ids, LIST_LIMIT)
                .await?;
            let _ = events.send(SyncEvent::ConversationsReplaced {
                conversations,
                kind: alcedo_core::EnvelopeListKind::Search,
            });
        }
        MessageListMode::Flat => {
            let envelopes = store.search(query, folder_ids, LIST_LIMIT).await?;
            let _ = events.send(SyncEvent::EnvelopesReplaced {
                envelopes,
                kind: alcedo_core::EnvelopeListKind::Search,
            });
        }
    }
    Ok(())
}

/// Emit unread counts for every account in the store.
pub async fn emit_unread_counts(
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
) -> Result<()> {
    let counts = store.unread_counts().await?;
    for (account, inbox, total) in counts {
        let _ = events.send(SyncEvent::UnreadCounts {
            account,
            inbox,
            total,
        });
    }
    Ok(())
}

/// Log and emit a sync error to the UI.
pub fn emit_error(
    events: &flume::Sender<SyncEvent>,
    account: Option<alcedo_core::AccountId>,
    message: String,
) {
    warn!(?account, %message, "sync error");
    let _ = events.send(SyncEvent::Error { account, message });
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use chrono::Utc;
    use tempfile::TempDir;
    use uuid::Uuid;

    use alcedo_core::{
        Account, Address, AuthMethod, Envelope, Folder, FolderId, MessageId, OAuthProvider,
        SpecialUse, SyncEvent, ThreadId,
    };
    use alcedo_store::StoreHandle;

    use super::*;

    fn sample_account(id: Uuid, email: &str) -> Account {
        Account {
            id: alcedo_core::AccountId(id),
            email: email.to_string(),
            username: email.to_string(),
            split_credentials: false,
            smtp_username: String::new(),
            display_name: email.to_string(),
            sender_name: String::new(),
            imap_host: "imap.example.com".to_string(),
            imap_port: 993,
            smtp_host: "smtp.example.com".to_string(),
            smtp_port: 465,
            smtp_starttls: false,
            auth: AuthMethod::OAuth2 {
                provider: OAuthProvider::Google,
            },
            accent_color: "#3584e4".to_string(),
            avatar_text: "AB".to_string(),
            avatar_image: Vec::new(),
        }
    }

    fn sample_folder(account_id: alcedo_core::AccountId, name: &str) -> Folder {
        Folder {
            id: FolderId(0),
            account_id,
            imap_name: name.to_string(),
            delimiter: "/".to_string(),
            special: SpecialUse::Inbox,
            selectable: true,
            uidvalidity: Some(1),
            uidnext: Some(2),
            highest_modseq: None,
            unread: 0,
            total: 0,
        }
    }

    fn sample_envelope(
        account: &Account,
        folder_id: FolderId,
        uid: u32,
        subject: &str,
    ) -> Envelope {
        Envelope {
            id: MessageId(0),
            account_id: account.id,
            folder_id,
            uid,
            thread_id: ThreadId(Uuid::new_v4()),
            date: Utc::now(),
            subject: subject.to_string(),
            from: vec![Address {
                name: Some("Sender".to_string()),
                email: "sender@example.com".to_string(),
            }],
            to: vec![Address {
                name: None,
                email: account.email.clone(),
            }],
            cc: vec![],
            reply_to: Vec::new(),
            sender: Vec::new(),
            routing: None,
            snippet: "snippet".to_string(),
            seen: false,
            flagged: false,
            answered: false,
            draft: false,
            has_attachments: false,
            deleted_at: None,
            account_accent: account.accent_color.clone(),
            account_avatar: account.avatar_text.clone(),
            size_bytes: 0,
        }
    }

    async fn open_test_store() -> (Arc<StoreHandle>, TempDir) {
        let dir = TempDir::new().expect("tempdir");
        let db_path = dir.path().join("mail.db");
        let store = Arc::new(StoreHandle::open_path(&db_path).expect("open store"));
        (store, dir)
    }

    #[tokio::test]
    async fn select_unified_inbox_emits_replaced() {
        let (store, _dir) = open_test_store().await;
        let a1 = sample_account(Uuid::new_v4(), "one@example.com");
        let a2 = sample_account(Uuid::new_v4(), "two@example.com");
        store.upsert_account(&a1).await.expect("upsert a1");
        store.upsert_account(&a2).await.expect("upsert a2");

        let f1 = store
            .replace_folders(a1.id, &[sample_folder(a1.id, "INBOX")])
            .await
            .expect("folders a1");
        let f2 = store
            .replace_folders(a2.id, &[sample_folder(a2.id, "INBOX")])
            .await
            .expect("folders a2");

        store
            .upsert_envelopes(
                &[
                    sample_envelope(&a1, f1[0].id, 1, "First"),
                    sample_envelope(&a2, f2[0].id, 1, "Second"),
                ],
                None,
            )
            .await
            .expect("upsert envelopes");

        let (tx, rx) = flume::unbounded::<SyncEvent>();
        select_unified_inbox(&store, MessageListMode::Flat, &tx)
            .await
            .expect("select unified");

        let event = rx.recv_async().await.expect("event");
        match event {
            SyncEvent::EnvelopesReplaced { envelopes, kind } => {
                assert_eq!(envelopes.len(), 2);
                assert_eq!(kind, alcedo_core::EnvelopeListKind::UnifiedInbox);
            }
            other => panic!("expected EnvelopesReplaced, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn select_folder_emits_folder_messages() {
        let (store, _dir) = open_test_store().await;
        let account = sample_account(Uuid::new_v4(), "folder@example.com");
        store.upsert_account(&account).await.expect("upsert");
        let folders = store
            .replace_folders(account.id, &[sample_folder(account.id, "INBOX")])
            .await
            .expect("folders");
        let folder_id = folders[0].id;

        store
            .upsert_envelopes(&[sample_envelope(&account, folder_id, 1, "Only one")], None)
            .await
            .expect("upsert");

        let (tx, rx) = flume::unbounded::<SyncEvent>();
        select_folder(&store, folder_id, MessageListMode::Flat, &tx)
            .await
            .expect("select folder");

        let event = rx.recv_async().await.expect("event");
        match event {
            SyncEvent::EnvelopesReplaced { envelopes, kind } => {
                assert_eq!(envelopes.len(), 1);
                assert_eq!(envelopes[0].subject, "Only one");
                assert_eq!(kind, alcedo_core::EnvelopeListKind::Folder(folder_id));
            }
            other => panic!("expected EnvelopesReplaced, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn select_folder_conversation_emits_conversations_replaced() {
        let (store, _dir) = open_test_store().await;
        let account = sample_account(Uuid::new_v4(), "conv@example.com");
        store.upsert_account(&account).await.expect("upsert");
        let folders = store
            .replace_folders(account.id, &[sample_folder(account.id, "INBOX")])
            .await
            .expect("folders");
        let folder_id = folders[0].id;
        let thread_id = ThreadId(Uuid::new_v4());
        let mut first = sample_envelope(&account, folder_id, 1, "Thread");
        first.thread_id = thread_id;
        first.date = Utc::now() - chrono::Duration::minutes(5);
        let mut second = sample_envelope(&account, folder_id, 2, "Thread");
        second.thread_id = thread_id;

        store
            .upsert_envelopes(&[first, second], None)
            .await
            .expect("upsert");

        let (tx, rx) = flume::unbounded::<SyncEvent>();
        select_folder(&store, folder_id, MessageListMode::Conversation, &tx)
            .await
            .expect("select folder");

        match rx.recv_async().await.expect("event") {
            SyncEvent::ConversationsReplaced {
                conversations,
                kind,
            } => {
                assert_eq!(conversations.len(), 1);
                assert_eq!(conversations[0].message_count, 2);
                assert_eq!(kind, alcedo_core::EnvelopeListKind::Folder(folder_id));
            }
            other => panic!("expected ConversationsReplaced, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn select_folder_emits_all_local_messages_not_a_page() {
        let (store, _dir) = open_test_store().await;
        let account = sample_account(Uuid::new_v4(), "paged@example.com");
        store.upsert_account(&account).await.expect("upsert");
        let folders = store
            .replace_folders(account.id, &[sample_folder(account.id, "Sent")])
            .await
            .expect("folders");
        let folder_id = folders[0].id;

        let envelopes: Vec<Envelope> = (1..=120)
            .map(|uid| {
                let mut env = sample_envelope(&account, folder_id, uid, &format!("m{uid}"));
                env.date = Utc::now() - chrono::Duration::seconds(uid as i64);
                env
            })
            .collect();
        store
            .upsert_envelopes(&envelopes, None)
            .await
            .expect("upsert");

        let loaded = load_folder_envelopes(&store, folder_id)
            .await
            .expect("load");
        assert_eq!(loaded.len(), 120);

        let (tx, rx) = flume::unbounded::<SyncEvent>();
        select_folder(&store, folder_id, MessageListMode::Flat, &tx)
            .await
            .expect("select folder");
        match rx.recv_async().await.expect("event") {
            SyncEvent::EnvelopesReplaced { envelopes, .. } => {
                assert_eq!(envelopes.len(), 120);
            }
            other => panic!("expected EnvelopesReplaced, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn search_emits_matching_envelopes() {
        let (store, _dir) = open_test_store().await;
        let account = sample_account(Uuid::new_v4(), "search@example.com");
        store.upsert_account(&account).await.expect("upsert");
        let folders = store
            .replace_folders(account.id, &[sample_folder(account.id, "INBOX")])
            .await
            .expect("folders");
        let folder_id = folders[0].id;

        store
            .upsert_envelopes(
                &[
                    sample_envelope(&account, folder_id, 1, "hello world"),
                    sample_envelope(&account, folder_id, 2, "goodbye"),
                ],
                None,
            )
            .await
            .expect("upsert");

        let (tx, rx) = flume::unbounded::<SyncEvent>();
        search(&store, "subject:hello", None, MessageListMode::Flat, &tx)
            .await
            .expect("search");

        let event = rx.recv_async().await.expect("event");
        match event {
            SyncEvent::EnvelopesReplaced { envelopes, kind } => {
                assert_eq!(envelopes.len(), 1);
                assert_eq!(envelopes[0].subject, "hello world");
                assert_eq!(kind, alcedo_core::EnvelopeListKind::Search);
            }
            other => panic!("expected EnvelopesReplaced, got {other:?}"),
        }
    }
}
