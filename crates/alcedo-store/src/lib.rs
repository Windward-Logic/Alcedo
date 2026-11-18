//! SQLite mail store (P2).

pub mod accounts;
pub mod attachments;
pub mod contacts;
pub mod folders;
pub mod handle;
pub mod identities;
pub mod messages;
pub mod pgp;
pub mod queued_sends;
pub mod remote_images;
pub mod rules;
pub mod schema;
pub mod search;
pub mod signatures;
pub mod snippets;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use alcedo_core::{AccountId, Address, FolderId, ListUnsubscribe, MessageId, PgpStatus, ThreadId};

pub use folders::FolderReplaceMode;
pub use handle::StoreHandle;

/// Store-specific error type (mapped to `alcedo_core::Error::Store` at the handle boundary).
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("uuid: {0}")]
    Uuid(#[from] uuid::Error),
    #[error("chrono: {0}")]
    Chrono(#[from] chrono::ParseError),
    #[error("attachment rejected: {0}")]
    AttachmentRejected(String),
    #[error("{0}")]
    Other(String),
}

pub type StoreResult<T> = std::result::Result<T, StoreError>;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AttachmentMeta {
    pub message_id: MessageId,
    pub filename: String,
    pub cid: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StoredAttachment {
    pub id: i64,
    pub filename: String,
    pub mime: String,
    pub sha256: String,
    pub size: u64,
    pub cid: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StoredMessage {
    pub id: MessageId,
    pub account_id: AccountId,
    pub folder_id: FolderId,
    pub uid: u32,
    pub thread_id: ThreadId,
    pub date: DateTime<Utc>,
    pub subject: String,
    pub from: Vec<Address>,
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    pub reply_to: Vec<Address>,
    pub sender: Vec<Address>,
    pub snippet: String,
    pub body_text: Option<String>,
    pub body_html: Option<String>,
    pub message_id_hdr: Option<String>,
    pub in_reply_to: Option<String>,
    pub references_hdr: Option<String>,
    pub seen: bool,
    pub flagged: bool,
    pub answered: bool,
    pub draft: bool,
    pub has_attachments: bool,
    pub allow_remote_images: bool,
    pub list_unsubscribe: Option<ListUnsubscribe>,
    #[serde(default)]
    pub pgp_status: Option<PgpStatus>,
    pub account_accent: String,
    pub account_avatar: String,
    pub attachments: Vec<StoredAttachment>,
}

#[derive(Clone, Debug)]
pub struct MessageLocator {
    pub id: MessageId,
    pub account_id: AccountId,
    pub folder_id: FolderId,
    pub uid: u32,
    pub message_id_hdr: Option<String>,
}

/// Which rows a flag change is copied from when mirroring it onto their copies.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FlagMirrorSource {
    Messages(Vec<MessageId>),
    Folder(FolderId),
}

/// Which flags a mirror copies. A flag left out keeps the copy's own value, so
/// a stale source row cannot overwrite a newer value on the copy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MirroredFlags {
    pub seen: bool,
    pub flagged: bool,
}

impl MirroredFlags {
    pub const ALL: Self = Self {
        seen: true,
        flagged: true,
    };
    pub const SEEN: Self = Self {
        seen: true,
        flagged: false,
    };

    /// The flags a user-requested change actually sets.
    pub fn requested(seen: Option<bool>, flagged: Option<bool>) -> Self {
        Self {
            seen: seen.is_some(),
            flagged: flagged.is_some(),
        }
    }
}

/// A copy a mirror changed, with the flags it held before, so the change can be
/// undone when the server rejects the write it anticipated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MirroredCopy {
    pub id: MessageId,
    pub folder_id: FolderId,
    pub previous_seen: bool,
    pub previous_flagged: bool,
}

/// Timestamps are stored as RFC 3339 text.
pub(crate) fn format_ts(ts: chrono::DateTime<chrono::Utc>) -> String {
    ts.to_rfc3339()
}

pub(crate) fn compose_format_from_str(s: &str) -> StoreResult<alcedo_core::ComposeFormat> {
    use alcedo_core::ComposeFormat;
    match s {
        "plain" => Ok(ComposeFormat::Plain),
        "html" => Ok(ComposeFormat::Html),
        "markdown" => Ok(ComposeFormat::Markdown),
        other => Err(StoreError::Other(format!(
            "unknown compose format: {other}"
        ))),
    }
}

pub(crate) fn map_core_err(err: StoreError) -> alcedo_core::Error {
    alcedo_core::Error::Store(err.to_string())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use chrono::Utc;
    use tempfile::TempDir;
    use uuid::Uuid;

    use alcedo_core::{
        Account, Address, AuthMethod, Envelope, Folder, FolderId, ListUnsubscribe, MessageId,
        OAuthProvider, SpecialUse, ThreadId,
    };

    use super::*;

    fn sample_account(id: Uuid, email: &str, accent: &str) -> Account {
        Account {
            id: AccountId(id),
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
            accent_color: accent.to_string(),
            avatar_text: "AB".to_string(),
            avatar_image: Vec::new(),
        }
    }

    const TINY_PNG: &[u8] = &[
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90,
        0x77, 0x53, 0xde, 0x00, 0x00, 0x00, 0x0c, 0x49, 0x44, 0x41, 0x54, 0x08, 0xd7, 0x63, 0xf8,
        0xcf, 0xc0, 0x00, 0x00, 0x03, 0x01, 0x01, 0x00, 0x18, 0xdd, 0x8d, 0xb4, 0x00, 0x00, 0x00,
        0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ];

    const TINY_SVG: &[u8] =
        br#"<svg xmlns="http://www.w3.org/2000/svg" width="1" height="1"></svg>"#;

    fn sample_folder(account_id: AccountId, name: &str) -> Folder {
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
    async fn reader_sees_commit_after_upsert_reply() {
        let (store, _dir) = open_test_store().await;

        let account = sample_account(Uuid::new_v4(), "reader@example.com", "#3584e4");
        store
            .upsert_account(&account)
            .await
            .expect("upsert account");
        let folders = store
            .replace_folders(account.id, &[sample_folder(account.id, "INBOX")])
            .await
            .expect("folders");
        let folder_id = folders[0].id;
        let subject = "Reader visibility";
        store
            .upsert_envelopes(&[sample_envelope(&account, folder_id, 1, subject)], None)
            .await
            .expect("upsert envelope");

        let messages = store
            .folder_messages(folder_id, 50, 0)
            .await
            .expect("folder messages");
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].subject, subject);

        let matching = store
            .extension_read("test_unified_matching", |conn| {
                crate::messages::unified_inbox_matching(conn, "1 = 1", 10, 0)
            })
            .await
            .expect("unified inbox matching");
        assert!(matching.len() <= 1);
    }

    const READER_OVERLAP_BATCH: usize = 12_000;

    #[tokio::test]
    async fn reads_overlap_upsert_batch() {
        let (store, _dir) = open_test_store().await;
        let store = Arc::clone(&store);

        let account = sample_account(Uuid::new_v4(), "overlap@example.com", "#3584e4");
        store
            .upsert_account(&account)
            .await
            .expect("upsert account");
        let folders = store
            .replace_folders(account.id, &[sample_folder(account.id, "INBOX")])
            .await
            .expect("folders");
        let folder_id = folders[0].id;
        let seeded_subject = "Seeded overlap row";
        store
            .upsert_envelopes(
                &[sample_envelope(&account, folder_id, 1, seeded_subject)],
                None,
            )
            .await
            .expect("seed envelope");

        let batch: Vec<Envelope> = (2..=READER_OVERLAP_BATCH as u32 + 1)
            .map(|uid| sample_envelope(&account, folder_id, uid, "Batch row"))
            .collect();
        let upsert_store = Arc::clone(&store);
        let upsert = tokio::spawn(async move {
            let started = std::time::Instant::now();
            upsert_store
                .upsert_envelopes(&batch, None)
                .await
                .expect("batch upsert");
            started.elapsed()
        });

        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        if upsert.is_finished() {
            panic!("batch finished before read; increase READER_OVERLAP_BATCH");
        }

        let read_started = std::time::Instant::now();
        let messages = store
            .folder_messages(folder_id, 50, 0)
            .await
            .expect("folder messages during upsert");
        let messages_elapsed = read_started.elapsed();

        let conv_started = std::time::Instant::now();
        let conversations = store
            .folder_conversations(folder_id)
            .await
            .expect("folder conversations during upsert");
        let conversations_elapsed = conv_started.elapsed();

        let upsert_elapsed = upsert.await.expect("upsert join");

        assert!(
            messages_elapsed < upsert_elapsed / 2,
            "folder_messages took {:?}, upsert took {:?}",
            messages_elapsed,
            upsert_elapsed
        );
        assert!(
            conversations_elapsed < upsert_elapsed / 2,
            "folder_conversations took {:?}, upsert took {:?}",
            conversations_elapsed,
            upsert_elapsed
        );
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].subject, seeded_subject);
        assert!(!conversations.is_empty());
    }

    #[tokio::test]
    async fn migrate_and_unified_inbox_two_accounts() {
        let (store, _dir) = open_test_store().await;

        let a1 = sample_account(Uuid::new_v4(), "one@example.com", "#3584e4");
        let a2 = sample_account(Uuid::new_v4(), "two@example.com", "#ff0000");
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

        let env1 = sample_envelope(&a1, f1[0].id, 1, "First");
        let env2 = sample_envelope(&a2, f2[0].id, 1, "Second");
        store
            .upsert_envelopes(&[env1, env2], None)
            .await
            .expect("upsert envelopes");

        let inbox = store.unified_inbox(10, 0).await.expect("unified inbox");
        assert_eq!(inbox.len(), 2);
    }

    #[tokio::test]
    async fn delete_account_cascades_folders_and_messages() {
        let (store, _dir) = open_test_store().await;

        let account = sample_account(Uuid::new_v4(), "delete@example.com", "#3584e4");
        store
            .upsert_account(&account)
            .await
            .expect("upsert account");
        let folders = store
            .replace_folders(account.id, &[sample_folder(account.id, "INBOX")])
            .await
            .expect("folders");
        let folder_id = folders[0].id;
        let envelope = sample_envelope(&account, folder_id, 1, "To delete");
        store
            .upsert_envelopes(&[envelope], None)
            .await
            .expect("upsert envelope");

        store
            .delete_account(account.id)
            .await
            .expect("delete account");

        assert!(
            store
                .list_accounts()
                .await
                .expect("list accounts")
                .is_empty()
        );
        assert!(
            store
                .list_folders(account.id)
                .await
                .expect("list folders")
                .is_empty()
        );
        assert!(
            store
                .unified_inbox(10, 0)
                .await
                .expect("unified inbox")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn delete_account_removes_orphan_attachment_files() {
        let (store, _dir) = open_test_store().await;

        let account = sample_account(Uuid::new_v4(), "files@example.com", "#3584e4");
        store
            .upsert_account(&account)
            .await
            .expect("upsert account");
        let folders = store
            .replace_folders(account.id, &[sample_folder(account.id, "INBOX")])
            .await
            .expect("folders");
        store
            .upsert_envelopes(
                &[sample_envelope(&account, folders[0].id, 1, "Attachment")],
                None,
            )
            .await
            .expect("envelope");
        let messages = store
            .folder_messages(folders[0].id, 10, 0)
            .await
            .expect("list");

        let path = store
            .save_attachment(
                AttachmentMeta {
                    message_id: messages[0].id,
                    filename: "note.txt".to_string(),
                    cid: None,
                },
                b"delete me from disk",
            )
            .await
            .expect("save attachment");
        assert!(path.exists(), "attachment file should exist before delete");

        store
            .delete_account(account.id)
            .await
            .expect("delete account");
        assert!(
            !path.exists(),
            "attachment file should be removed with the account"
        );
    }

    #[tokio::test]
    async fn delete_account_keeps_shared_attachment_files() {
        let (store, _dir) = open_test_store().await;

        let account_a = sample_account(Uuid::new_v4(), "a@example.com", "#3584e4");
        let account_b = sample_account(Uuid::new_v4(), "b@example.com", "#7d4cdb");
        store.upsert_account(&account_a).await.expect("upsert a");
        store.upsert_account(&account_b).await.expect("upsert b");

        let folders_a = store
            .replace_folders(account_a.id, &[sample_folder(account_a.id, "INBOX")])
            .await
            .expect("folders a");
        let folders_b = store
            .replace_folders(account_b.id, &[sample_folder(account_b.id, "INBOX")])
            .await
            .expect("folders b");

        store
            .upsert_envelopes(
                &[sample_envelope(&account_a, folders_a[0].id, 1, "A")],
                None,
            )
            .await
            .expect("envelope a");
        store
            .upsert_envelopes(
                &[sample_envelope(&account_b, folders_b[0].id, 1, "B")],
                None,
            )
            .await
            .expect("envelope b");

        let messages_a = store
            .folder_messages(folders_a[0].id, 10, 0)
            .await
            .expect("list a");
        let messages_b = store
            .folder_messages(folders_b[0].id, 10, 0)
            .await
            .expect("list b");

        let bytes = b"shared attachment bytes";
        let path = store
            .save_attachment(
                AttachmentMeta {
                    message_id: messages_a[0].id,
                    filename: "note.txt".to_string(),
                    cid: None,
                },
                bytes,
            )
            .await
            .expect("save a");
        store
            .save_attachment(
                AttachmentMeta {
                    message_id: messages_b[0].id,
                    filename: "note.txt".to_string(),
                    cid: None,
                },
                bytes,
            )
            .await
            .expect("save b");

        store.delete_account(account_a.id).await.expect("delete a");
        assert!(
            path.exists(),
            "shared attachment file should remain while account b still references it"
        );

        store.delete_account(account_b.id).await.expect("delete b");
        assert!(
            !path.exists(),
            "attachment file should be removed once no account references it"
        );
    }

    #[tokio::test]
    async fn fts_subject_hello_search() {
        let (store, _dir) = open_test_store().await;

        let account = sample_account(Uuid::new_v4(), "search@example.com", "#3584e4");
        store
            .upsert_account(&account)
            .await
            .expect("upsert account");
        let folders = store
            .replace_folders(account.id, &[sample_folder(account.id, "INBOX")])
            .await
            .expect("folders");
        let folder_id = folders[0].id;

        let hello = sample_envelope(&account, folder_id, 1, "hello world");
        let other = sample_envelope(&account, folder_id, 2, "goodbye");
        store
            .upsert_envelopes(&[hello, other], None)
            .await
            .expect("upsert envelopes");

        let hits = store
            .search("subject:hello", None, 10)
            .await
            .expect("search");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].subject, "hello world");
    }

    fn sample_folder_named(account_id: AccountId, name: &str, special: SpecialUse) -> Folder {
        Folder {
            id: FolderId(0),
            account_id,
            imap_name: name.to_string(),
            delimiter: "/".to_string(),
            special,
            selectable: true,
            uidvalidity: Some(1),
            uidnext: Some(2),
            highest_modseq: None,
            unread: 0,
            total: 0,
        }
    }

    fn sample_envelope_with_flags(
        account: &Account,
        folder_id: FolderId,
        uid: u32,
        subject: &str,
        has_attachments: bool,
    ) -> Envelope {
        let mut env = sample_envelope(account, folder_id, uid, subject);
        env.has_attachments = has_attachments;
        env
    }

    #[tokio::test]
    async fn search_folder_ids_restricts_results() {
        let (store, _dir) = open_test_store().await;
        let account = sample_account(Uuid::new_v4(), "search@example.com", "#3584e4");
        store
            .upsert_account(&account)
            .await
            .expect("upsert account");
        let folders = store
            .replace_folders(
                account.id,
                &[
                    sample_folder_named(account.id, "INBOX", SpecialUse::Inbox),
                    sample_folder_named(account.id, "Sent", SpecialUse::Sent),
                ],
            )
            .await
            .expect("folders");
        let inbox_id = folders
            .iter()
            .find(|folder| folder.special == SpecialUse::Inbox)
            .expect("inbox")
            .id;
        let sent_id = folders
            .iter()
            .find(|folder| folder.special == SpecialUse::Sent)
            .expect("sent")
            .id;

        store
            .upsert_envelopes(
                &[
                    sample_envelope(&account, inbox_id, 1, "michael inbox"),
                    sample_envelope(&account, sent_id, 2, "michael sent"),
                ],
                None,
            )
            .await
            .expect("upsert envelopes");

        let scoped = store
            .search("michael", Some(&[inbox_id]), 10)
            .await
            .expect("search scoped");
        assert_eq!(scoped.len(), 1);
        assert_eq!(scoped[0].folder_id, inbox_id);

        let all = store.search("michael", None, 10).await.expect("search all");
        assert_eq!(all.len(), 2);
    }

    #[tokio::test]
    async fn search_folder_operator_inbox() {
        let (store, _dir) = open_test_store().await;
        let account = sample_account(Uuid::new_v4(), "search@example.com", "#3584e4");
        store
            .upsert_account(&account)
            .await
            .expect("upsert account");
        let folders = store
            .replace_folders(
                account.id,
                &[
                    sample_folder_named(account.id, "INBOX", SpecialUse::Inbox),
                    sample_folder_named(account.id, "Sent", SpecialUse::Sent),
                ],
            )
            .await
            .expect("folders");
        let inbox_id = folders
            .iter()
            .find(|folder| folder.special == SpecialUse::Inbox)
            .expect("inbox")
            .id;
        let sent_id = folders
            .iter()
            .find(|folder| folder.special == SpecialUse::Sent)
            .expect("sent")
            .id;

        store
            .upsert_envelopes(
                &[
                    sample_envelope(&account, inbox_id, 1, "michael inbox"),
                    sample_envelope(&account, sent_id, 2, "michael sent"),
                ],
                None,
            )
            .await
            .expect("upsert envelopes");

        let hits = store
            .search("folder:inbox michael", None, 10)
            .await
            .expect("search");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].folder_id, inbox_id);
    }

    #[tokio::test]
    async fn search_has_attachment_without_free_text() {
        let (store, _dir) = open_test_store().await;
        let account = sample_account(Uuid::new_v4(), "search@example.com", "#3584e4");
        store
            .upsert_account(&account)
            .await
            .expect("upsert account");
        let folders = store
            .replace_folders(account.id, &[sample_folder(account.id, "INBOX")])
            .await
            .expect("folders");
        let folder_id = folders[0].id;

        store
            .upsert_envelopes(
                &[
                    sample_envelope_with_flags(&account, folder_id, 1, "plain", false),
                    sample_envelope_with_flags(&account, folder_id, 2, "attached", true),
                ],
                None,
            )
            .await
            .expect("upsert envelopes");

        let hits = store
            .search("has:attachment", None, 10)
            .await
            .expect("search");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].subject, "attached");
    }

    #[tokio::test]
    async fn search_folder_all_ignores_folder_ids() {
        let (store, _dir) = open_test_store().await;
        let account = sample_account(Uuid::new_v4(), "search@example.com", "#3584e4");
        store
            .upsert_account(&account)
            .await
            .expect("upsert account");
        let folders = store
            .replace_folders(
                account.id,
                &[
                    sample_folder_named(account.id, "INBOX", SpecialUse::Inbox),
                    sample_folder_named(account.id, "Sent", SpecialUse::Sent),
                ],
            )
            .await
            .expect("folders");
        let inbox_id = folders
            .iter()
            .find(|folder| folder.special == SpecialUse::Inbox)
            .expect("inbox")
            .id;
        let sent_id = folders
            .iter()
            .find(|folder| folder.special == SpecialUse::Sent)
            .expect("sent")
            .id;

        store
            .upsert_envelopes(
                &[
                    sample_envelope(&account, inbox_id, 1, "michael inbox"),
                    sample_envelope(&account, sent_id, 2, "michael sent"),
                ],
                None,
            )
            .await
            .expect("upsert envelopes");

        let hits = store
            .search("folder:all michael", Some(&[inbox_id]), 10)
            .await
            .expect("search");
        assert_eq!(hits.len(), 2);
    }

    #[tokio::test]
    async fn search_unscoped_excludes_trash_and_junk() {
        let (store, _dir) = open_test_store().await;
        let account = sample_account(Uuid::new_v4(), "search@example.com", "#3584e4");
        store
            .upsert_account(&account)
            .await
            .expect("upsert account");
        let folders = store
            .replace_folders(
                account.id,
                &[
                    sample_folder_named(account.id, "INBOX", SpecialUse::Inbox),
                    sample_folder_named(account.id, "Trash", SpecialUse::Trash),
                    sample_folder_named(account.id, "Junk", SpecialUse::Junk),
                ],
            )
            .await
            .expect("folders");
        let inbox_id = folders
            .iter()
            .find(|folder| folder.special == SpecialUse::Inbox)
            .expect("inbox")
            .id;
        let trash_id = folders
            .iter()
            .find(|folder| folder.special == SpecialUse::Trash)
            .expect("trash")
            .id;
        let junk_id = folders
            .iter()
            .find(|folder| folder.special == SpecialUse::Junk)
            .expect("junk")
            .id;

        store
            .upsert_envelopes(
                &[
                    sample_envelope(&account, inbox_id, 1, "keep me"),
                    sample_envelope(&account, trash_id, 2, "deleted unread"),
                    sample_envelope(&account, junk_id, 3, "spam unread"),
                ],
                None,
            )
            .await
            .expect("upsert envelopes");

        let unread = store
            .search("is:unread", None, 10)
            .await
            .expect("unread search");
        assert_eq!(unread.len(), 1);
        assert_eq!(unread[0].subject, "keep me");

        let bare = store.search("unread", None, 10).await.expect("bare");
        assert!(bare.is_empty());

        let anywhere = store
            .search("is:unread in:anywhere", None, 10)
            .await
            .expect("anywhere");
        assert_eq!(anywhere.len(), 3);

        let trash = store
            .search("is:unread in:trash", None, 10)
            .await
            .expect("trash search");
        assert_eq!(trash.len(), 1);
        assert_eq!(trash[0].folder_id, trash_id);

        let scoped_trash = store
            .search("is:unread", Some(&[trash_id]), 10)
            .await
            .expect("scoped trash");
        assert_eq!(scoped_trash.len(), 1);
        assert_eq!(scoped_trash[0].folder_id, trash_id);

        let unread = store
            .extension_read("test_count_unread", |conn| {
                crate::search::count_unread(conn, "is:unread")
            })
            .await
            .expect("count unread through the extension lane");
        assert_eq!(unread, 1);
    }

    #[tokio::test]
    async fn uidvalidity_change_wipes_messages() {
        let (store, _dir) = open_test_store().await;

        let account = sample_account(Uuid::new_v4(), "wipe@example.com", "#3584e4");
        store
            .upsert_account(&account)
            .await
            .expect("upsert account");
        let folders = store
            .replace_folders(account.id, &[sample_folder(account.id, "INBOX")])
            .await
            .expect("folders");
        let folder_id = folders[0].id;

        let env = sample_envelope(&account, folder_id, 1, "to be wiped");
        store
            .upsert_envelopes(&[env], None)
            .await
            .expect("upsert envelope");

        let before = store.unified_inbox(10, 0).await.expect("inbox before");
        assert_eq!(before.len(), 1);

        let changed = Folder {
            id: folder_id,
            account_id: account.id,
            imap_name: "INBOX".to_string(),
            delimiter: "/".to_string(),
            special: SpecialUse::Inbox,
            selectable: true,
            uidvalidity: Some(99),
            uidnext: Some(1),
            highest_modseq: None,
            unread: 0,
            total: 0,
        };
        store
            .replace_folders(account.id, &[changed])
            .await
            .expect("replace folders");

        let after = store.unified_inbox(10, 0).await.expect("inbox after");
        assert_eq!(after.len(), 0);
    }

    #[tokio::test]
    async fn list_style_replace_preserves_uidvalidity() {
        let (store, _dir) = open_test_store().await;
        let account = sample_account(Uuid::new_v4(), "list@example.com", "#3584e4");
        store
            .upsert_account(&account)
            .await
            .expect("upsert account");

        let selected = Folder {
            id: FolderId(0),
            account_id: account.id,
            imap_name: "INBOX".to_string(),
            delimiter: "/".to_string(),
            special: SpecialUse::Inbox,
            selectable: true,
            uidvalidity: Some(7),
            uidnext: Some(12),
            highest_modseq: Some(44),
            unread: 3,
            total: 9,
        };
        let stored = store
            .replace_folders(account.id, &[selected])
            .await
            .expect("select-style replace");
        assert_eq!(stored[0].uidvalidity, Some(7));
        assert_eq!(stored[0].highest_modseq, Some(44));

        let listed = Folder {
            id: FolderId(0),
            account_id: account.id,
            imap_name: "INBOX".to_string(),
            delimiter: "/".to_string(),
            special: SpecialUse::Inbox,
            selectable: true,
            uidvalidity: None,
            uidnext: None,
            highest_modseq: None,
            unread: 0,
            total: 0,
        };
        let after_list = store
            .replace_folders(account.id, &[listed])
            .await
            .expect("list-style replace");
        assert_eq!(after_list[0].uidvalidity, Some(7));
        assert_eq!(after_list[0].uidnext, Some(12));
        assert_eq!(after_list[0].highest_modseq, Some(44));
        assert_eq!(after_list[0].unread, 3);
        assert_eq!(after_list[0].total, 9);
    }

    #[tokio::test]
    async fn select_style_replace_after_list_null_uidvalidity() {
        let (store, _dir) = open_test_store().await;
        let account = sample_account(Uuid::new_v4(), "nulluid@example.com", "#3584e4");
        store
            .upsert_account(&account)
            .await
            .expect("upsert account");

        let listed = Folder {
            id: FolderId(0),
            account_id: account.id,
            imap_name: "INBOX".to_string(),
            delimiter: "/".to_string(),
            special: SpecialUse::Inbox,
            selectable: true,
            uidvalidity: None,
            uidnext: None,
            highest_modseq: None,
            unread: 0,
            total: 0,
        };
        let after_list = store
            .replace_folders(account.id, &[listed])
            .await
            .expect("list-style insert");
        assert_eq!(after_list[0].uidvalidity, None);

        let selected = Folder {
            id: after_list[0].id,
            account_id: account.id,
            imap_name: "INBOX".to_string(),
            delimiter: "/".to_string(),
            special: SpecialUse::Inbox,
            selectable: true,
            uidvalidity: Some(42),
            uidnext: Some(3),
            highest_modseq: Some(9),
            unread: 0,
            total: 2,
        };
        let after_select = store
            .replace_folders(account.id, &[selected])
            .await
            .expect("select-style update after null uidvalidity");
        assert_eq!(after_select[0].uidvalidity, Some(42));
        assert_eq!(after_select[0].uidnext, Some(3));
        assert_eq!(after_select[0].highest_modseq, Some(9));
        assert_eq!(after_select[0].total, 2);
    }

    #[tokio::test]
    async fn two_messages_can_share_attachment_hash() {
        let (store, _dir) = open_test_store().await;
        let account = sample_account(Uuid::new_v4(), "att@example.com", "#3584e4");
        store
            .upsert_account(&account)
            .await
            .expect("upsert account");
        let folders = store
            .replace_folders(account.id, &[sample_folder(account.id, "INBOX")])
            .await
            .expect("folders");
        let folder_id = folders[0].id;

        store
            .upsert_envelopes(
                &[
                    sample_envelope(&account, folder_id, 1, "one"),
                    sample_envelope(&account, folder_id, 2, "two"),
                ],
                None,
            )
            .await
            .expect("envelopes");
        let messages = store.folder_messages(folder_id, 10, 0).await.expect("list");
        assert_eq!(messages.len(), 2);

        let bytes = b"hello attachment";
        store
            .save_attachment(
                AttachmentMeta {
                    message_id: messages[0].id,
                    filename: "note.txt".to_string(),
                    cid: None,
                },
                bytes,
            )
            .await
            .expect("first attachment");
        store
            .save_attachment(
                AttachmentMeta {
                    message_id: messages[1].id,
                    filename: "note.txt".to_string(),
                    cid: None,
                },
                bytes,
            )
            .await
            .expect("second attachment");

        let first = store
            .get_message(messages[0].id)
            .await
            .expect("get first")
            .expect("first exists");
        let second = store
            .get_message(messages[1].id)
            .await
            .expect("get second")
            .expect("second exists");
        assert_eq!(first.attachments.len(), 1);
        assert_eq!(second.attachments.len(), 1);
        assert_eq!(first.attachments[0].sha256, second.attachments[0].sha256);
    }

    #[tokio::test]
    async fn envelope_upsert_does_not_clear_saved_attachment_flag() {
        let (store, _dir) = open_test_store().await;
        let account = sample_account(Uuid::new_v4(), "att@example.com", "#3584e4");
        store
            .upsert_account(&account)
            .await
            .expect("upsert account");
        let folders = store
            .replace_folders(account.id, &[sample_folder(account.id, "INBOX")])
            .await
            .expect("folders");
        let folder_id = folders[0].id;
        store
            .upsert_envelopes(&[sample_envelope(&account, folder_id, 1, "fwd")], None)
            .await
            .expect("envelopes");
        let id = store.folder_messages(folder_id, 10, 0).await.expect("list")[0].id;

        store
            .save_attachment(
                AttachmentMeta {
                    message_id: id,
                    filename: "rules.txt".to_string(),
                    cid: Some("1DC8@outlook.example".to_string()),
                },
                b"hoa rules",
            )
            .await
            .expect("save attachment");

        store
            .upsert_envelopes(
                &[sample_envelope_with_flags(
                    &account, folder_id, 1, "fwd", false,
                )],
                None,
            )
            .await
            .expect("clobbering envelope upsert");

        let listed = store.folder_messages(folder_id, 10, 0).await.expect("list");
        assert!(listed[0].has_attachments, "list should keep the paperclip");
        let stored = store.get_message(id).await.expect("get").expect("exists");
        assert!(stored.has_attachments);
        let hits = store
            .search("has:attachment", None, 10)
            .await
            .expect("search");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, id);
    }

    #[tokio::test]
    async fn cid_inline_image_is_not_listed_as_file_attachment() {
        let (store, _dir) = open_test_store().await;
        let account = sample_account(Uuid::new_v4(), "img@example.com", "#3584e4");
        store
            .upsert_account(&account)
            .await
            .expect("upsert account");
        let folders = store
            .replace_folders(account.id, &[sample_folder(account.id, "INBOX")])
            .await
            .expect("folders");
        let folder_id = folders[0].id;
        store
            .upsert_envelopes(
                &[sample_envelope_with_flags(
                    &account, folder_id, 1, "logo", true,
                )],
                None,
            )
            .await
            .expect("envelopes");
        let id = store.folder_messages(folder_id, 10, 0).await.expect("list")[0].id;

        store
            .save_attachment(
                AttachmentMeta {
                    message_id: id,
                    filename: "logo.png".to_string(),
                    cid: Some("logo@example.com".to_string()),
                },
                TINY_PNG,
            )
            .await
            .expect("save inline png");

        let listed = store.folder_messages(folder_id, 10, 0).await.expect("list");
        assert!(
            !listed[0].has_attachments,
            "CID images should not show a paperclip"
        );
        let stored = store.get_message(id).await.expect("get").expect("exists");
        assert!(!stored.has_attachments);
        let hits = store
            .search("has:attachment", None, 10)
            .await
            .expect("search");
        assert!(hits.is_empty());
    }

    #[tokio::test]
    async fn cid_risky_svg_is_listed_as_file_attachment() {
        let (store, _dir) = open_test_store().await;
        let account = sample_account(Uuid::new_v4(), "svg@example.com", "#3584e4");
        store
            .upsert_account(&account)
            .await
            .expect("upsert account");
        let folders = store
            .replace_folders(account.id, &[sample_folder(account.id, "INBOX")])
            .await
            .expect("folders");
        let folder_id = folders[0].id;
        store
            .upsert_envelopes(
                &[sample_envelope_with_flags(
                    &account, folder_id, 1, "logo", true,
                )],
                None,
            )
            .await
            .expect("envelopes");
        let id = store.folder_messages(folder_id, 10, 0).await.expect("list")[0].id;

        store
            .save_attachment(
                AttachmentMeta {
                    message_id: id,
                    filename: "logo.svg".to_string(),
                    cid: Some("logo@example.com".to_string()),
                },
                TINY_SVG,
            )
            .await
            .expect("save inline svg");

        let listed = store.folder_messages(folder_id, 10, 0).await.expect("list");
        assert!(
            listed[0].has_attachments,
            "risky CID SVG should show a paperclip"
        );
        let stored = store.get_message(id).await.expect("get").expect("exists");
        assert!(stored.has_attachments);
        let hits = store
            .search("has:attachment", None, 10)
            .await
            .expect("search");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, id);
    }

    #[tokio::test]
    async fn whitespace_cid_image_is_listed_as_file_attachment() {
        let (store, _dir) = open_test_store().await;
        let account = sample_account(Uuid::new_v4(), "ws@example.com", "#3584e4");
        store
            .upsert_account(&account)
            .await
            .expect("upsert account");
        let folders = store
            .replace_folders(account.id, &[sample_folder(account.id, "INBOX")])
            .await
            .expect("folders");
        let folder_id = folders[0].id;
        store
            .upsert_envelopes(&[sample_envelope(&account, folder_id, 1, "pic")], None)
            .await
            .expect("envelopes");
        let id = store.folder_messages(folder_id, 10, 0).await.expect("list")[0].id;

        store
            .save_attachment(
                AttachmentMeta {
                    message_id: id,
                    filename: "pic.png".to_string(),
                    cid: Some("  ".to_string()),
                },
                TINY_PNG,
            )
            .await
            .expect("save png with blank cid");

        let listed = store.folder_messages(folder_id, 10, 0).await.expect("list");
        assert!(
            listed[0].has_attachments,
            "whitespace CID should match part_is_file_attachment"
        );
        let stored = store.get_message(id).await.expect("get").expect("exists");
        assert!(stored.has_attachments);
        let hits = store
            .search("has:attachment", None, 10)
            .await
            .expect("search");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, id);
    }

    #[tokio::test]
    async fn extensionless_executable_is_stored() {
        let (store, _dir) = open_test_store().await;
        let account = sample_account(Uuid::new_v4(), "rej@example.com", "#3584e4");
        store
            .upsert_account(&account)
            .await
            .expect("upsert account");
        let folders = store
            .replace_folders(account.id, &[sample_folder(account.id, "INBOX")])
            .await
            .expect("folders");
        store
            .upsert_envelopes(&[sample_envelope(&account, folders[0].id, 1, "msg")], None)
            .await
            .expect("envelope");
        let messages = store
            .folder_messages(folders[0].id, 10, 0)
            .await
            .expect("list");

        store
            .save_attachment(
                AttachmentMeta {
                    message_id: messages[0].id,
                    filename: "payload".to_string(),
                    cid: None,
                },
                b"MZ\x90\x00",
            )
            .await
            .expect("executable stored");
        let stored = store
            .get_message(messages[0].id)
            .await
            .expect("get")
            .expect("exists");
        assert_eq!(stored.attachments.len(), 1);
        assert!(stored.has_attachments);
        assert!(
            alcedo_core::AttachmentLists::defaults().mime_is_risky(&stored.attachments[0].mime)
        );
    }

    #[tokio::test]
    async fn save_message_body_persists_html() {
        let (store, _dir) = open_test_store().await;
        let account = sample_account(Uuid::new_v4(), "body@example.com", "#3584e4");
        store
            .upsert_account(&account)
            .await
            .expect("upsert account");
        let folders = store
            .replace_folders(account.id, &[sample_folder(account.id, "INBOX")])
            .await
            .expect("folders");
        store
            .upsert_envelopes(
                &[sample_envelope(&account, folders[0].id, 1, "msg")],
                Some("plain"),
            )
            .await
            .expect("envelope");
        let messages = store
            .folder_messages(folders[0].id, 10, 0)
            .await
            .expect("list");
        store
            .save_message_body(messages[0].id, Some("plain"), Some("<p>hi</p>"), None)
            .await
            .expect("save body");
        let stored = store
            .get_message(messages[0].id)
            .await
            .expect("get")
            .expect("exists");
        assert_eq!(stored.body_text.as_deref(), Some("plain"));
        assert_eq!(stored.body_html.as_deref(), Some("<p>hi</p>"));
    }

    #[tokio::test]
    async fn messages_missing_body_skips_cached_and_respects_window() {
        let (store, _dir) = open_test_store().await;
        let account = sample_account(Uuid::new_v4(), "missing@example.com", "#3584e4");
        store
            .upsert_account(&account)
            .await
            .expect("upsert account");
        let folders = store
            .replace_folders(account.id, &[sample_folder(account.id, "INBOX")])
            .await
            .expect("folders");
        let folder_id = folders[0].id;
        store
            .upsert_envelopes(
                &[
                    sample_envelope(&account, folder_id, 1, "one"),
                    sample_envelope(&account, folder_id, 2, "two"),
                    sample_envelope(&account, folder_id, 3, "three"),
                ],
                None,
            )
            .await
            .expect("envelopes");
        let messages = store.folder_messages(folder_id, 10, 0).await.expect("list");
        let saved_id = messages[0].id;
        store
            .save_message_body(saved_id, Some("plain"), Some("<p>hi</p>"), None)
            .await
            .expect("save body");

        let missing = store
            .messages_missing_body(folder_id, 10)
            .await
            .expect("missing");
        assert_eq!(missing.len(), 2);
        assert!(!missing.contains(&saved_id));

        let windowed = store
            .messages_missing_body(folder_id, 1)
            .await
            .expect("windowed");
        assert!(windowed.len() <= 1);
    }

    #[tokio::test]
    async fn purge_missing_uids_soft_deletes() {
        let (store, _dir) = open_test_store().await;
        let account = sample_account(Uuid::new_v4(), "purge@example.com", "#3584e4");
        store
            .upsert_account(&account)
            .await
            .expect("upsert account");
        let folders = store
            .replace_folders(account.id, &[sample_folder(account.id, "INBOX")])
            .await
            .expect("folders");
        let folder_id = folders[0].id;
        store
            .upsert_envelopes(
                &[
                    sample_envelope(&account, folder_id, 1, "keep"),
                    sample_envelope(&account, folder_id, 2, "drop"),
                ],
                None,
            )
            .await
            .expect("envelopes");

        let removed = store
            .purge_missing_uids(folder_id, &[1])
            .await
            .expect("purge");
        assert_eq!(removed.len(), 1);

        let remaining = store.folder_messages(folder_id, 10, 0).await.expect("list");
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].subject, "keep");
        assert_eq!(remaining[0].uid, 1);
    }

    #[tokio::test]
    async fn purge_missing_uids_restores_soft_deleted_live_uids() {
        let (store, _dir) = open_test_store().await;
        let account = sample_account(Uuid::new_v4(), "restore@example.com", "#3584e4");
        store
            .upsert_account(&account)
            .await
            .expect("upsert account");
        let folders = store
            .replace_folders(account.id, &[sample_folder(account.id, "INBOX")])
            .await
            .expect("folders");
        let folder_id = folders[0].id;
        store
            .upsert_envelopes(&[sample_envelope(&account, folder_id, 1, "ghost")], None)
            .await
            .expect("envelopes");
        let messages = store.folder_messages(folder_id, 10, 0).await.expect("list");
        store.mark_deleted(messages[0].id).await.expect("delete");
        assert!(
            store
                .folder_messages(folder_id, 10, 0)
                .await
                .expect("hidden")
                .is_empty()
        );

        store
            .purge_missing_uids(folder_id, &[1])
            .await
            .expect("restore");

        let restored = store.folder_messages(folder_id, 10, 0).await.expect("list");
        assert_eq!(restored.len(), 1);
        assert_eq!(restored[0].subject, "ghost");
    }

    #[tokio::test]
    async fn upsert_envelopes_clears_soft_delete() {
        let (store, _dir) = open_test_store().await;
        let account = sample_account(Uuid::new_v4(), "upsert-del@example.com", "#3584e4");
        store
            .upsert_account(&account)
            .await
            .expect("upsert account");
        let folders = store
            .replace_folders(account.id, &[sample_folder(account.id, "INBOX")])
            .await
            .expect("folders");
        let folder_id = folders[0].id;
        store
            .upsert_envelopes(&[sample_envelope(&account, folder_id, 1, "live")], None)
            .await
            .expect("envelopes");
        let messages = store.folder_messages(folder_id, 10, 0).await.expect("list");
        store.mark_deleted(messages[0].id).await.expect("delete");

        store
            .upsert_envelopes(&[sample_envelope(&account, folder_id, 1, "live")], None)
            .await
            .expect("upsert");

        let listed = store.folder_messages(folder_id, 10, 0).await.expect("list");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].uid, 1);
    }

    #[tokio::test]
    async fn message_rfc822_size_reads_recorded_size_only() {
        let (store, _dir) = open_test_store().await;
        let account = sample_account(Uuid::new_v4(), "size@example.com", "#3584e4");
        store.upsert_account(&account).await.expect("account");
        let folders = store
            .replace_folders(account.id, &[sample_folder(account.id, "INBOX")])
            .await
            .expect("folders");
        let folder_id = folders[0].id;
        store
            .upsert_envelopes(&[sample_envelope(&account, folder_id, 1, "sized")], None)
            .await
            .expect("envelopes");
        let id = store.folder_messages(folder_id, 10, 0).await.expect("rows")[0].id;
        assert_eq!(store.message_rfc822_size(id).await.expect("read"), None);
        store
            .set_rfc822_sizes(folder_id, &[(1, 1_048_576)])
            .await
            .expect("set size");
        assert_eq!(
            store.message_rfc822_size(id).await.expect("read"),
            Some(1_048_576)
        );
    }

    #[tokio::test]
    async fn apply_server_flags_updates_only_rows_that_differ() {
        use alcedo_core::ServerFlags;

        let (store, _dir) = open_test_store().await;
        let account = sample_account(Uuid::new_v4(), "flags@example.com", "#3584e4");
        store.upsert_account(&account).await.expect("account");
        let folders = store
            .replace_folders(account.id, &[sample_folder(account.id, "INBOX")])
            .await
            .expect("folders");
        let folder_id = folders[0].id;
        store
            .upsert_envelopes(
                &[
                    sample_envelope(&account, folder_id, 1, "unchanged"),
                    sample_envelope(&account, folder_id, 2, "read elsewhere"),
                ],
                None,
            )
            .await
            .expect("envelopes");

        let unchanged = ServerFlags {
            uid: 1,
            seen: false,
            flagged: false,
            answered: false,
            draft: false,
            deleted: false,
        };
        let read = ServerFlags {
            uid: 2,
            seen: true,
            answered: true,
            ..unchanged
        };
        let unknown = ServerFlags { uid: 99, ..read };
        let changed = store
            .apply_server_flags(folder_id, vec![unchanged, read, unknown])
            .await
            .expect("apply");
        assert_eq!(changed.len(), 1, "only uid 2 differs and exists");

        let rows = store.folder_messages(folder_id, 10, 0).await.expect("list");
        let by_uid = |uid| rows.iter().find(|row| row.uid == uid).expect("row");
        assert!(by_uid(2).seen && by_uid(2).answered);
        assert!(!by_uid(1).seen);

        let again = store
            .apply_server_flags(folder_id, vec![read])
            .await
            .expect("apply again");
        assert!(again.is_empty(), "second identical poll writes nothing");
    }

    #[tokio::test]
    async fn mark_deleted_by_uids_hides_listed_uids_only() {
        let (store, _dir) = open_test_store().await;
        let account = sample_account(Uuid::new_v4(), "uids@example.com", "#3584e4");
        store
            .upsert_account(&account)
            .await
            .expect("upsert account");
        let folders = store
            .replace_folders(account.id, &[sample_folder(account.id, "INBOX")])
            .await
            .expect("folders");
        let folder_id = folders[0].id;
        store
            .upsert_envelopes(
                &[
                    sample_envelope(&account, folder_id, 1, "keep"),
                    sample_envelope(&account, folder_id, 2, "drop"),
                ],
                None,
            )
            .await
            .expect("envelopes");

        let affected = store
            .mark_deleted_by_uids(folder_id, vec![2])
            .await
            .expect("mark deleted by uids");
        assert_eq!(affected, 1);

        let remaining = store.folder_messages(folder_id, 10, 0).await.expect("list");
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].uid, 1);
        assert_eq!(remaining[0].subject, "keep");

        assert_eq!(
            store
                .mark_deleted_by_uids(folder_id, vec![])
                .await
                .expect("empty"),
            0
        );
    }

    #[tokio::test]
    async fn mark_deleted_hides_from_folder_list() {
        let (store, _dir) = open_test_store().await;
        let account = sample_account(Uuid::new_v4(), "del@example.com", "#3584e4");
        store
            .upsert_account(&account)
            .await
            .expect("upsert account");
        let folders = store
            .replace_folders(account.id, &[sample_folder(account.id, "INBOX")])
            .await
            .expect("folders");
        let folder_id = folders[0].id;
        store
            .upsert_envelopes(&[sample_envelope(&account, folder_id, 1, "gone")], None)
            .await
            .expect("envelopes");
        let messages = store.folder_messages(folder_id, 10, 0).await.expect("list");
        assert_eq!(messages.len(), 1);
        assert_eq!(
            store.folder_unread_count(folder_id).await.expect("unread"),
            1
        );

        store.mark_deleted(messages[0].id).await.expect("delete");

        let remaining = store.folder_messages(folder_id, 10, 0).await.expect("list");
        assert!(remaining.is_empty());
        assert_eq!(store.folder_live_count(folder_id).await.expect("live"), 0);
        assert!(
            store
                .get_message(messages[0].id)
                .await
                .expect("get")
                .is_some(),
            "soft-delete keeps the row"
        );
        assert_eq!(
            store.folder_unread_count(folder_id).await.expect("unread"),
            0
        );
    }

    #[tokio::test]
    async fn has_live_copy_elsewhere_detects_same_message_in_other_folder() {
        let (store, _dir) = open_test_store().await;
        let account = sample_account(Uuid::new_v4(), "copy@example.com", "#3584e4");
        store
            .upsert_account(&account)
            .await
            .expect("upsert account");
        let mut inbox = sample_folder(account.id, "INBOX");
        inbox.special = SpecialUse::Inbox;
        let mut projects = sample_folder(account.id, "Projects");
        projects.special = SpecialUse::Other;
        let folders = store
            .replace_folders(account.id, &[inbox, projects])
            .await
            .expect("folders");
        let inbox_id = folders
            .iter()
            .find(|folder| folder.imap_name == "INBOX")
            .map(|folder| folder.id)
            .expect("inbox");
        let projects_id = folders
            .iter()
            .find(|folder| folder.imap_name == "Projects")
            .map(|folder| folder.id)
            .expect("projects");

        let inbox_env = sample_envelope(&account, inbox_id, 1, "Copied");
        let mut copy_env = inbox_env.clone();
        copy_env.folder_id = projects_id;
        copy_env.uid = 9;
        store
            .upsert_envelopes(&[inbox_env, copy_env], None)
            .await
            .expect("envelopes");

        let inbox_messages = store.folder_messages(inbox_id, 10, 0).await.expect("inbox");
        let project_messages = store
            .folder_messages(projects_id, 10, 0)
            .await
            .expect("projects");
        assert!(
            store
                .has_live_copy_elsewhere(project_messages[0].id)
                .await
                .expect("copy check")
        );
        assert!(
            store
                .has_live_copy_elsewhere(inbox_messages[0].id)
                .await
                .expect("inbox check")
        );

        store
            .mark_deleted(inbox_messages[0].id)
            .await
            .expect("delete inbox copy");
        assert!(
            !store
                .has_live_copy_elsewhere(project_messages[0].id)
                .await
                .expect("no sibling after inbox delete")
        );
    }

    #[tokio::test]
    async fn live_sibling_ids_returns_other_folder_copies() {
        let (store, _dir) = open_test_store().await;
        let account = sample_account(Uuid::new_v4(), "sibling@example.com", "#3584e4");
        store
            .upsert_account(&account)
            .await
            .expect("upsert account");
        let mut inbox = sample_folder(account.id, "INBOX");
        inbox.special = SpecialUse::Inbox;
        let mut projects = sample_folder(account.id, "Projects");
        projects.special = SpecialUse::Other;
        let folders = store
            .replace_folders(account.id, &[inbox, projects])
            .await
            .expect("folders");
        let inbox_id = folders
            .iter()
            .find(|folder| folder.imap_name == "INBOX")
            .map(|folder| folder.id)
            .expect("inbox");
        let projects_id = folders
            .iter()
            .find(|folder| folder.imap_name == "Projects")
            .map(|folder| folder.id)
            .expect("projects");

        let inbox_env = sample_envelope(&account, inbox_id, 1, "Same subject");
        let mut copy_env = inbox_env.clone();
        copy_env.folder_id = projects_id;
        copy_env.uid = 9;
        let mut other_subject = inbox_env.clone();
        other_subject.folder_id = projects_id;
        other_subject.uid = 10;
        other_subject.subject = "Different subject".into();
        store
            .upsert_envelopes(&[inbox_env, copy_env, other_subject], None)
            .await
            .expect("envelopes");

        let inbox_messages = store.folder_messages(inbox_id, 10, 0).await.expect("inbox");
        let project_messages = store
            .folder_messages(projects_id, 10, 0)
            .await
            .expect("projects");
        let inbox_id_msg = inbox_messages[0].id;
        let copy_id = project_messages
            .iter()
            .find(|m| m.subject == "Same subject")
            .map(|m| m.id)
            .expect("copy");
        let other_id = project_messages
            .iter()
            .find(|m| m.subject == "Different subject")
            .map(|m| m.id)
            .expect("other subject");

        let from_inbox = store
            .live_sibling_ids(inbox_id_msg)
            .await
            .expect("siblings from inbox");
        assert_eq!(from_inbox, vec![copy_id]);

        let from_copy = store
            .live_sibling_ids(copy_id)
            .await
            .expect("siblings from copy");
        assert_eq!(from_copy, vec![inbox_id_msg]);

        assert!(
            store
                .live_sibling_ids(other_id)
                .await
                .expect("different subject")
                .is_empty()
        );

        store
            .mark_deleted(inbox_id_msg)
            .await
            .expect("delete inbox copy");
        assert!(
            store
                .live_sibling_ids(copy_id)
                .await
                .expect("deleted sibling")
                .is_empty()
        );
    }

    /// The same message in two folders, plus a row in the second folder that
    /// shares its date, subject and sender but has `other_hdr` as its Message-ID.
    async fn flag_copy_rows(
        store: &StoreHandle,
        other_hdr: Option<&str>,
    ) -> (FolderId, MessageId, MessageId, MessageId) {
        let account = sample_account(Uuid::new_v4(), "copies@example.com", "#3584e4");
        store.upsert_account(&account).await.expect("account");
        let mut label = sample_folder(account.id, "Label");
        label.special = SpecialUse::Other;
        let folders = store
            .replace_folders(account.id, &[sample_folder(account.id, "INBOX"), label])
            .await
            .expect("folders");
        let (inbox, label) = (folders[0].id, folders[1].id);
        let original = sample_envelope(&account, inbox, 1, "Same");
        let copy = Envelope {
            folder_id: label,
            uid: 2,
            ..original.clone()
        };
        let other = Envelope {
            folder_id: label,
            uid: 3,
            ..original.clone()
        };
        store
            .upsert_envelopes(&[original, copy, other], None)
            .await
            .expect("envelopes");
        let hdr = "<same@example.com>".to_string();
        store
            .set_message_id_hdrs(inbox, &[(1, hdr.clone())])
            .await
            .expect("inbox header");
        let mut label_hdrs = vec![(2, hdr)];
        label_hdrs.extend(other_hdr.map(|value| (3, value.to_string())));
        store
            .set_message_id_hdrs(label, &label_hdrs)
            .await
            .expect("label headers");
        let inbox_rows = store.folder_messages(inbox, 10, 0).await.expect("inbox");
        let label_rows = store.folder_messages(label, 10, 0).await.expect("label");
        let id_of = |uid| label_rows.iter().find(|e| e.uid == uid).expect("row").id;
        (inbox, inbox_rows[0].id, id_of(2), id_of(3))
    }

    async fn flags_of(store: &StoreHandle, id: MessageId) -> (bool, bool) {
        let row = store.envelopes_by_ids(&[id]).await.expect("load").remove(0);
        (row.seen, row.flagged)
    }

    #[tokio::test]
    async fn mirror_skips_a_different_message_id_with_the_same_date_subject_and_sender() {
        let (store, _dir) = open_test_store().await;
        let (_, original, copy, other) = flag_copy_rows(&store, Some("<other@example.com>")).await;
        store
            .set_flags(original, Some(true), None)
            .await
            .expect("read");

        let mirrored = store
            .mirror_flags_to_copies(
                FlagMirrorSource::Messages(vec![original]),
                MirroredFlags::SEEN,
            )
            .await
            .expect("mirror");

        assert_eq!(mirrored.len(), 1);
        assert_eq!(mirrored[0].id, copy);
        assert_eq!(flags_of(&store, copy).await, (true, false));
        assert_eq!(flags_of(&store, other).await, (false, false));
    }

    #[tokio::test]
    async fn mirror_skips_a_row_without_a_message_id() {
        let (store, _dir) = open_test_store().await;
        let (_, original, _, other) = flag_copy_rows(&store, None).await;
        store
            .set_flags(original, Some(true), None)
            .await
            .expect("read");

        store
            .mirror_flags_to_copies(
                FlagMirrorSource::Messages(vec![original]),
                MirroredFlags::SEEN,
            )
            .await
            .expect("mirror");

        assert_eq!(flags_of(&store, other).await, (false, false));
    }

    #[tokio::test]
    async fn mirror_copies_only_the_requested_flags() {
        let (store, _dir) = open_test_store().await;
        let (_, original, copy, _) = flag_copy_rows(&store, None).await;
        store
            .set_flags(copy, None, Some(true))
            .await
            .expect("star copy");
        store
            .set_flags(original, Some(true), None)
            .await
            .expect("read");

        store
            .mirror_flags_to_copies(
                FlagMirrorSource::Messages(vec![original]),
                MirroredFlags::SEEN,
            )
            .await
            .expect("mirror");

        assert_eq!(flags_of(&store, copy).await, (true, true));
    }

    #[tokio::test]
    async fn mirror_reports_nothing_when_the_copy_already_matches() {
        let (store, _dir) = open_test_store().await;
        let (_, original, copy, _) = flag_copy_rows(&store, None).await;
        store
            .set_flags_many(&[original, copy], Some(true), None)
            .await
            .expect("read both");

        let mirrored = store
            .mirror_flags_to_copies(
                FlagMirrorSource::Messages(vec![original, copy]),
                MirroredFlags::ALL,
            )
            .await
            .expect("mirror");

        assert!(mirrored.is_empty());
    }

    #[tokio::test]
    async fn mirror_from_a_folder_reaches_every_copy_of_its_messages() {
        let (store, _dir) = open_test_store().await;
        let (inbox, _, copy, other) = flag_copy_rows(&store, Some("<other@example.com>")).await;
        store.mark_folder_seen(inbox).await.expect("folder read");

        store
            .mirror_flags_to_copies(FlagMirrorSource::Folder(inbox), MirroredFlags::SEEN)
            .await
            .expect("mirror");

        assert_eq!(flags_of(&store, copy).await, (true, false));
        assert_eq!(flags_of(&store, other).await, (false, false));
    }

    #[tokio::test]
    async fn restore_puts_back_the_flags_a_mirror_replaced() {
        let (store, _dir) = open_test_store().await;
        let (_, original, copy, _) = flag_copy_rows(&store, None).await;
        store
            .set_flags(copy, None, Some(true))
            .await
            .expect("star copy");
        store
            .set_flags(original, Some(true), Some(false))
            .await
            .expect("source");
        let mirrored = store
            .mirror_flags_to_copies(
                FlagMirrorSource::Messages(vec![original]),
                MirroredFlags::ALL,
            )
            .await
            .expect("mirror");
        assert_eq!(flags_of(&store, copy).await, (true, false));

        store
            .restore_mirrored_copies(&mirrored)
            .await
            .expect("restore");

        assert_eq!(flags_of(&store, copy).await, (false, true));
    }

    #[tokio::test]
    async fn envelopes_by_ids_leaves_out_deleted_rows() {
        let (store, _dir) = open_test_store().await;
        let (_, original, copy, _) = flag_copy_rows(&store, None).await;
        store.mark_deleted(copy).await.expect("delete");

        let rows = store
            .envelopes_by_ids(&[original, copy])
            .await
            .expect("load");

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, original);
    }

    #[tokio::test]
    async fn has_live_copy_elsewhere_ignores_different_subject() {
        let (store, _dir) = open_test_store().await;
        let account = sample_account(Uuid::new_v4(), "other@example.com", "#3584e4");
        store
            .upsert_account(&account)
            .await
            .expect("upsert account");
        let mut inbox = sample_folder(account.id, "INBOX");
        inbox.special = SpecialUse::Inbox;
        let mut projects = sample_folder(account.id, "Projects");
        projects.special = SpecialUse::Other;
        let folders = store
            .replace_folders(account.id, &[inbox, projects])
            .await
            .expect("folders");
        let inbox_id = folders[0].id;
        let projects_id = folders[1].id;
        store
            .upsert_envelopes(
                &[
                    sample_envelope(&account, inbox_id, 1, "One"),
                    sample_envelope(&account, projects_id, 1, "Two"),
                ],
                None,
            )
            .await
            .expect("envelopes");
        let project_messages = store
            .folder_messages(projects_id, 10, 0)
            .await
            .expect("projects");
        assert!(
            !store
                .has_live_copy_elsewhere(project_messages[0].id)
                .await
                .expect("copy check")
        );
    }

    #[tokio::test]
    async fn mark_folder_seen_clears_unread() {
        let (store, _dir) = open_test_store().await;
        let account = sample_account(Uuid::new_v4(), "seen@example.com", "#3584e4");
        store
            .upsert_account(&account)
            .await
            .expect("upsert account");
        let folders = store
            .replace_folders(account.id, &[sample_folder(account.id, "INBOX")])
            .await
            .expect("folders");
        let folder_id = folders[0].id;
        store
            .upsert_envelopes(
                &[
                    sample_envelope(&account, folder_id, 1, "one"),
                    sample_envelope(&account, folder_id, 2, "two"),
                ],
                None,
            )
            .await
            .expect("envelopes");
        assert_eq!(
            store.folder_unread_count(folder_id).await.expect("unread"),
            2
        );

        store
            .mark_folder_seen(folder_id)
            .await
            .expect("mark folder seen");

        assert_eq!(
            store.folder_unread_count(folder_id).await.expect("unread"),
            0
        );
        let messages = store.folder_messages(folder_id, 10, 0).await.expect("list");
        assert_eq!(messages.len(), 2);
        assert!(messages.iter().all(|env| env.seen));
    }

    #[tokio::test]
    async fn mark_folder_deleted_hides_messages() {
        let (store, _dir) = open_test_store().await;
        let account = sample_account(Uuid::new_v4(), "empty@example.com", "#3584e4");
        store
            .upsert_account(&account)
            .await
            .expect("upsert account");
        let folders = store
            .replace_folders(account.id, &[sample_folder(account.id, "Trash")])
            .await
            .expect("folders");
        let folder_id = folders[0].id;
        store
            .upsert_envelopes(
                &[
                    sample_envelope(&account, folder_id, 1, "gone"),
                    sample_envelope(&account, folder_id, 2, "also gone"),
                ],
                None,
            )
            .await
            .expect("envelopes");
        assert_eq!(
            store
                .folder_messages(folder_id, 10, 0)
                .await
                .expect("list")
                .len(),
            2
        );

        store
            .mark_folder_deleted(folder_id)
            .await
            .expect("mark folder deleted");

        assert!(
            store
                .folder_messages(folder_id, 10, 0)
                .await
                .expect("list")
                .is_empty()
        );
    }

    fn sent_folder(account_id: AccountId) -> Folder {
        Folder {
            id: FolderId(0),
            account_id,
            imap_name: "Sent".to_string(),
            delimiter: "/".to_string(),
            special: SpecialUse::Sent,
            selectable: true,
            uidvalidity: Some(1),
            uidnext: Some(2),
            highest_modseq: None,
            unread: 0,
            total: 0,
        }
    }

    #[tokio::test]
    async fn list_sent_recipients_scans_sent_only() {
        let (store, _dir) = open_test_store().await;

        let account = sample_account(Uuid::new_v4(), "me@example.com", "#3584e4");
        store
            .upsert_account(&account)
            .await
            .expect("upsert account");

        let folders = store
            .replace_folders(
                account.id,
                &[sample_folder(account.id, "INBOX"), sent_folder(account.id)],
            )
            .await
            .expect("folders");
        let inbox_id = folders
            .iter()
            .find(|f| f.special == SpecialUse::Inbox)
            .map(|f| f.id)
            .expect("inbox id");
        let sent_id = folders
            .iter()
            .find(|f| f.special == SpecialUse::Sent)
            .map(|f| f.id)
            .expect("sent id");

        let inbox_only = Envelope {
            id: MessageId(0),
            account_id: account.id,
            folder_id: inbox_id,
            uid: 1,
            thread_id: ThreadId(Uuid::new_v4()),
            date: Utc::now(),
            subject: "Inbox".to_string(),
            from: vec![Address {
                name: Some("Sender".to_string()),
                email: "sender@example.com".to_string(),
            }],
            to: vec![Address {
                name: None,
                email: "inbox-only@example.com".to_string(),
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
        };

        let older_jordan = Envelope {
            id: MessageId(0),
            account_id: account.id,
            folder_id: sent_id,
            uid: 1,
            thread_id: ThreadId(Uuid::new_v4()),
            date: Utc::now() - chrono::Duration::hours(2),
            subject: "Older".to_string(),
            from: vec![Address {
                name: Some(account.display_name.clone()),
                email: account.email.clone(),
            }],
            to: vec![Address {
                name: Some("Jordan Old".to_string()),
                email: "jordan@example.com".to_string(),
            }],
            cc: vec![],
            reply_to: Vec::new(),
            sender: Vec::new(),
            routing: None,
            snippet: "snippet".to_string(),
            seen: true,
            flagged: false,
            answered: false,
            draft: false,
            has_attachments: false,
            deleted_at: None,
            account_accent: account.accent_color.clone(),
            account_avatar: account.avatar_text.clone(),
            size_bytes: 0,
        };

        let newer_jordan = Envelope {
            id: MessageId(0),
            account_id: account.id,
            folder_id: sent_id,
            uid: 2,
            thread_id: ThreadId(Uuid::new_v4()),
            date: Utc::now() - chrono::Duration::hours(1),
            subject: "Newer".to_string(),
            from: vec![Address {
                name: Some(account.display_name.clone()),
                email: account.email.clone(),
            }],
            to: vec![Address {
                name: Some("Jordan New".to_string()),
                email: "jordan@example.com".to_string(),
            }],
            cc: vec![Address {
                name: Some("Priya".to_string()),
                email: "priya@company.io".to_string(),
            }],
            reply_to: Vec::new(),
            sender: Vec::new(),
            routing: None,
            snippet: "snippet".to_string(),
            seen: true,
            flagged: false,
            answered: false,
            draft: false,
            has_attachments: false,
            deleted_at: None,
            account_accent: account.accent_color.clone(),
            account_avatar: account.avatar_text.clone(),
            size_bytes: 0,
        };

        let self_to = Envelope {
            id: MessageId(0),
            account_id: account.id,
            folder_id: sent_id,
            uid: 3,
            thread_id: ThreadId(Uuid::new_v4()),
            date: Utc::now(),
            subject: "Self".to_string(),
            from: vec![Address {
                name: Some(account.display_name.clone()),
                email: account.email.clone(),
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
            seen: true,
            flagged: false,
            answered: false,
            draft: false,
            has_attachments: false,
            deleted_at: None,
            account_accent: account.accent_color.clone(),
            account_avatar: account.avatar_text.clone(),
            size_bytes: 0,
        };

        store
            .upsert_envelopes(&[inbox_only, older_jordan, newer_jordan, self_to], None)
            .await
            .expect("upsert envelopes");

        let messages = store
            .folder_messages(sent_id, 10, 0)
            .await
            .expect("sent list");
        let deleted_id = messages
            .iter()
            .find(|env| env.uid == 1)
            .map(|env| env.id)
            .expect("older message id");
        store.mark_deleted(deleted_id).await.expect("mark deleted");

        let recipients = store
            .list_sent_recipients()
            .await
            .expect("list sent recipients");
        assert_eq!(recipients.len(), 2);
        assert_eq!(recipients[0].email, "jordan@example.com");
        assert_eq!(recipients[0].name.as_deref(), Some("Jordan New"));
        assert_eq!(recipients[1].email, "priya@company.io");
        assert!(
            !recipients
                .iter()
                .any(|addr| addr.email.eq_ignore_ascii_case("inbox-only@example.com"))
        );
        assert!(
            !recipients
                .iter()
                .any(|addr| addr.email.eq_ignore_ascii_case(&account.email))
        );
    }

    #[tokio::test]
    async fn trash_deleted_at_sorts_stamped_first() {
        let (store, _dir) = open_test_store().await;
        let account = sample_account(Uuid::new_v4(), "one@example.com", "#3584e4");
        store.upsert_account(&account).await.expect("upsert");
        let folders = store
            .replace_folders(
                account.id,
                &[sample_folder_named(account.id, "Trash", SpecialUse::Trash)],
            )
            .await
            .expect("folders");
        let trash_id = folders[0].id;
        let older = sample_envelope(&account, trash_id, 1, "old");
        let newer = sample_envelope(&account, trash_id, 2, "new");
        store
            .upsert_envelopes(&[older, newer], None)
            .await
            .expect("upsert");
        let listed = store.folder_messages(trash_id, 10, 0).await.expect("list");
        let newer_id = listed.iter().find(|e| e.uid == 2).map(|e| e.id).unwrap();
        let stamp = Utc::now() - chrono::Duration::hours(1);
        assert!(
            store
                .rehome_message(newer_id, trash_id, 2, Some(stamp))
                .await
                .expect("stamp")
        );
        let sorted = store
            .folder_messages(trash_id, 10, 0)
            .await
            .expect("sorted");
        assert_eq!(sorted[0].id, newer_id);
        assert_eq!(sorted[0].deleted_at, Some(stamp));
    }

    #[tokio::test]
    async fn rehome_message_updates_folder_uid_and_stamp() {
        let (store, _dir) = open_test_store().await;
        let account = sample_account(Uuid::new_v4(), "one@example.com", "#3584e4");
        store.upsert_account(&account).await.expect("upsert");
        let folders = store
            .replace_folders(
                account.id,
                &[
                    sample_folder(account.id, "INBOX"),
                    sample_folder_named(account.id, "Trash", SpecialUse::Trash),
                ],
            )
            .await
            .expect("folders");
        let inbox_id = folders[0].id;
        let trash_id = folders[1].id;
        store
            .upsert_envelopes(&[sample_envelope(&account, inbox_id, 1, "msg")], None)
            .await
            .expect("upsert");
        let listed = store.folder_messages(inbox_id, 10, 0).await.expect("list");
        let id = listed[0].id;
        let stamp = Utc::now();
        assert!(
            store
                .rehome_message(id, trash_id, 42, Some(stamp))
                .await
                .expect("rehome")
        );
        let trash = store.folder_messages(trash_id, 10, 0).await.expect("trash");
        assert_eq!(trash.len(), 1);
        assert_eq!(trash[0].id, id);
        assert_eq!(trash[0].uid, 42);
        assert_eq!(trash[0].deleted_at, Some(stamp));
        assert!(
            store
                .folder_messages(inbox_id, 10, 0)
                .await
                .expect("inbox")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn rehome_message_reuses_deleted_uid_slot_and_stamps() {
        let (store, _dir) = open_test_store().await;
        let account = sample_account(Uuid::new_v4(), "one@example.com", "#3584e4");
        store.upsert_account(&account).await.expect("upsert");
        let folders = store
            .replace_folders(
                account.id,
                &[
                    sample_folder(account.id, "INBOX"),
                    sample_folder_named(account.id, "Trash", SpecialUse::Trash),
                ],
            )
            .await
            .expect("folders");
        let inbox_id = folders[0].id;
        let trash_id = folders[1].id;
        store
            .upsert_envelopes(
                &[
                    sample_envelope(&account, inbox_id, 1, "keep"),
                    sample_envelope(&account, trash_id, 42, "gone"),
                ],
                None,
            )
            .await
            .expect("upsert");
        let trash = store.folder_messages(trash_id, 10, 0).await.expect("trash");
        store
            .mark_deleted(trash[0].id)
            .await
            .expect("hide occupant");
        let inbox = store.folder_messages(inbox_id, 10, 0).await.expect("inbox");
        let stamp = Utc::now();
        assert!(
            store
                .rehome_message(inbox[0].id, trash_id, 42, Some(stamp))
                .await
                .expect("rehome")
        );
        let trash = store
            .folder_messages(trash_id, 10, 0)
            .await
            .expect("listed");
        assert_eq!(trash.len(), 1);
        assert_eq!(trash[0].id, inbox[0].id);
        assert_eq!(trash[0].uid, 42);
        assert_eq!(trash[0].deleted_at, Some(stamp));
    }

    #[tokio::test]
    async fn rehome_message_live_uid_collision_hides_mover() {
        let (store, _dir) = open_test_store().await;
        let account = sample_account(Uuid::new_v4(), "one@example.com", "#3584e4");
        store.upsert_account(&account).await.expect("upsert");
        let folders = store
            .replace_folders(
                account.id,
                &[
                    sample_folder(account.id, "INBOX"),
                    sample_folder_named(account.id, "Trash", SpecialUse::Trash),
                ],
            )
            .await
            .expect("folders");
        let inbox_id = folders[0].id;
        let trash_id = folders[1].id;
        store
            .upsert_envelopes(
                &[
                    sample_envelope(&account, inbox_id, 1, "mover"),
                    sample_envelope(&account, trash_id, 42, "occupant"),
                ],
                None,
            )
            .await
            .expect("upsert");
        let occupant_id = store.folder_messages(trash_id, 10, 0).await.expect("trash")[0].id;
        let occupant_stamp = Utc::now() - chrono::Duration::hours(1);
        assert!(
            store
                .rehome_message(occupant_id, trash_id, 42, Some(occupant_stamp))
                .await
                .expect("stamp occupant")
        );
        let mover_id = store.folder_messages(inbox_id, 10, 0).await.expect("inbox")[0].id;
        assert!(
            !store
                .rehome_message(mover_id, trash_id, 42, Some(Utc::now()))
                .await
                .expect("collision")
        );
        assert!(
            store
                .folder_messages(inbox_id, 10, 0)
                .await
                .expect("inbox")
                .is_empty()
        );
        let trash = store.folder_messages(trash_id, 10, 0).await.expect("trash");
        assert_eq!(trash.len(), 1);
        assert_eq!(trash[0].id, occupant_id);
        assert_eq!(trash[0].deleted_at, Some(occupant_stamp));
    }

    #[tokio::test]
    async fn upsert_envelopes_preserves_deleted_at() {
        let (store, _dir) = open_test_store().await;
        let account = sample_account(Uuid::new_v4(), "one@example.com", "#3584e4");
        store.upsert_account(&account).await.expect("upsert");
        let folders = store
            .replace_folders(
                account.id,
                &[sample_folder_named(account.id, "Trash", SpecialUse::Trash)],
            )
            .await
            .expect("folders");
        let trash_id = folders[0].id;
        store
            .upsert_envelopes(&[sample_envelope(&account, trash_id, 1, "msg")], None)
            .await
            .expect("upsert");
        let listed = store.folder_messages(trash_id, 10, 0).await.expect("list");
        let id = listed[0].id;
        let stamp = Utc::now();
        store
            .rehome_message(id, trash_id, 1, Some(stamp))
            .await
            .expect("stamp");
        let mut env = listed[0].clone();
        env.seen = true;
        store.upsert_envelopes(&[env], None).await.expect("upsert");
        let after = store.folder_messages(trash_id, 10, 0).await.expect("after");
        assert_eq!(after[0].deleted_at, Some(stamp));
        assert!(after[0].seen);
    }

    #[tokio::test]
    async fn save_list_unsubscribe_empty_sentinel_round_trip_and_upsert_preserves() {
        let (store, _dir) = open_test_store().await;
        let account = sample_account(Uuid::new_v4(), "one@example.com", "#3584e4");
        store.upsert_account(&account).await.expect("upsert");
        let folders = store
            .replace_folders(account.id, &[sample_folder(account.id, "INBOX")])
            .await
            .expect("folders");
        let folder_id = folders[0].id;
        store
            .upsert_envelopes(&[sample_envelope(&account, folder_id, 1, "msg")], None)
            .await
            .expect("upsert");
        let listed = store.folder_messages(folder_id, 10, 0).await.expect("list");
        let id = listed[0].id;

        let empty = ListUnsubscribe {
            https: None,
            mailto: None,
            one_click: false,
        };
        store
            .save_list_unsubscribe(id, Some(&empty))
            .await
            .expect("save sentinel");
        let msg = store.get_message(id).await.expect("get").expect("found");
        assert!(msg.list_unsubscribe.is_some());
        assert!(!msg.list_unsubscribe.unwrap().has_action());

        store
            .upsert_envelopes(&[sample_envelope(&account, folder_id, 1, "msg")], None)
            .await
            .expect("re-upsert");
        let msg = store.get_message(id).await.expect("get").expect("found");
        assert!(msg.list_unsubscribe.is_some());
    }

    #[tokio::test]
    async fn set_message_id_hdrs_writes_header() {
        let (store, _dir) = open_test_store().await;
        let account = sample_account(Uuid::new_v4(), "one@example.com", "#3584e4");
        store.upsert_account(&account).await.expect("upsert");
        let folders = store
            .replace_folders(account.id, &[sample_folder(account.id, "INBOX")])
            .await
            .expect("folders");
        let inbox_id = folders[0].id;
        store
            .upsert_envelopes(&[sample_envelope(&account, inbox_id, 7, "msg")], None)
            .await
            .expect("upsert");
        store
            .set_message_id_hdrs(inbox_id, &[(7, "<a@b.com>".to_string())])
            .await
            .expect("hdr");
        let listed = store.folder_messages(inbox_id, 10, 0).await.expect("list");
        let stored = store
            .get_message(listed[0].id)
            .await
            .expect("get")
            .expect("row");
        assert_eq!(stored.message_id_hdr.as_deref(), Some("<a@b.com>"));
    }

    #[tokio::test]
    async fn live_message_id_hdrs_skips_deleted_and_blank() {
        let (store, _dir) = open_test_store().await;
        let account = sample_account(Uuid::new_v4(), "one@example.com", "#3584e4");
        store.upsert_account(&account).await.expect("upsert");
        let folders = store
            .replace_folders(account.id, &[sample_folder(account.id, "Sent")])
            .await
            .expect("folders");
        let folder_id = folders[0].id;
        store
            .upsert_envelopes(
                &[
                    sample_envelope(&account, folder_id, 10, "keep"),
                    sample_envelope(&account, folder_id, 11, "dup"),
                    sample_envelope(&account, folder_id, 12, "gone"),
                ],
                None,
            )
            .await
            .expect("upsert");
        store
            .set_message_id_hdrs(
                folder_id,
                &[
                    (10, "<a@b.com>".to_string()),
                    (11, "  <a@b.com>  ".to_string()),
                    (12, "<gone@b.com>".to_string()),
                ],
            )
            .await
            .expect("hdrs");
        store
            .mark_deleted_by_uids(folder_id, vec![12])
            .await
            .expect("delete");
        let mut rows = store
            .live_message_id_hdrs(folder_id)
            .await
            .expect("live hdrs");
        rows.sort_by_key(|(uid, _)| *uid);
        assert_eq!(
            rows,
            vec![
                (10, "<a@b.com>".to_string()),
                (11, "  <a@b.com>  ".to_string()),
            ]
        );
    }

    #[tokio::test]
    async fn unified_inbox_conversations_groups_shared_thread() {
        let (store, _dir) = open_test_store().await;

        let a1 = sample_account(Uuid::new_v4(), "one@example.com", "#3584e4");
        let a2 = sample_account(Uuid::new_v4(), "two@example.com", "#ff0000");
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
        let inbox_a = f1[0].id;
        let inbox_b = f2[0].id;

        let shared_thread = ThreadId(Uuid::new_v4());
        let mut env1 = sample_envelope(&a1, inbox_a, 1, "Thread one");
        env1.thread_id = shared_thread;
        let mut env2 = sample_envelope(&a2, inbox_b, 1, "Thread two");
        env2.thread_id = shared_thread;
        store
            .upsert_envelopes(&[env1, env2], None)
            .await
            .expect("upsert envelopes");

        let conversations = store
            .unified_inbox_conversations(10, 0)
            .await
            .expect("unified inbox conversations");
        assert_eq!(conversations.len(), 1);
        assert_eq!(conversations[0].message_count, 2);
        assert_eq!(conversations[0].member_locations.len(), 2);
        assert!(
            conversations[0]
                .member_locations
                .iter()
                .any(|(_, folder_id)| *folder_id == inbox_a)
        );

        let folder_conversations = store
            .folder_conversations(inbox_a)
            .await
            .expect("folder conversations");
        assert_eq!(folder_conversations.len(), 1);
        assert_eq!(folder_conversations[0].message_count, 1);
        assert_eq!(folder_conversations[0].member_locations.len(), 1);
        assert_eq!(folder_conversations[0].member_locations[0].1, inbox_a);

        let thread_rows = store
            .thread_envelopes(shared_thread, Some(&[inbox_a]), false)
            .await
            .expect("thread envelopes");
        assert_eq!(thread_rows.len(), 1);
        assert_eq!(thread_rows[0].folder_id, inbox_a);
    }

    #[tokio::test]
    async fn folder_conversations_members_ordered_and_folder_scoped() {
        let (store, _dir) = open_test_store().await;
        let account = sample_account(Uuid::new_v4(), "thread@example.com", "#3584e4");
        store.upsert_account(&account).await.expect("upsert");
        let folders = store
            .replace_folders(
                account.id,
                &[
                    sample_folder_named(account.id, "INBOX", SpecialUse::Inbox),
                    sample_folder_named(account.id, "Sent", SpecialUse::Sent),
                ],
            )
            .await
            .expect("folders");
        let inbox_id = folders
            .iter()
            .find(|folder| folder.special == SpecialUse::Inbox)
            .map(|folder| folder.id)
            .expect("inbox");
        let sent_id = folders
            .iter()
            .find(|folder| folder.special == SpecialUse::Sent)
            .map(|folder| folder.id)
            .expect("sent");

        let thread_id = ThreadId(Uuid::new_v4());
        let mut older = sample_envelope(&account, inbox_id, 1, "Thread start");
        older.thread_id = thread_id;
        older.date = chrono::Utc::now() - chrono::Duration::hours(2);
        let mut newer = sample_envelope(&account, inbox_id, 2, "Re: Thread start");
        newer.thread_id = thread_id;
        newer.date = chrono::Utc::now() - chrono::Duration::hours(1);
        let mut sent_copy = sample_envelope(&account, sent_id, 10, "Re: Thread start");
        sent_copy.thread_id = thread_id;
        sent_copy.date = newer.date;
        store
            .upsert_envelopes(&[older, newer, sent_copy], None)
            .await
            .expect("upsert");

        let conversations = store
            .folder_conversations(inbox_id)
            .await
            .expect("folder conversations");
        assert_eq!(conversations.len(), 1);
        assert_eq!(conversations[0].members.len(), 2);
        let members = &conversations[0].members;
        assert_eq!(members[0].uid, 1, "oldest inbox message first");
        assert_eq!(members[1].uid, 2, "newer inbox message second");
        assert!(members[0].date < members[1].date, "members oldest first");
        assert_eq!(conversations[0].latest.uid, 2);
        assert!(
            conversations[0]
                .member_locations
                .iter()
                .all(|(_, folder_id)| *folder_id == inbox_id),
            "inbox query must not include Sent"
        );
    }

    #[tokio::test]
    async fn thread_envelopes_dedupes_inbox_and_sent_copies() {
        let (store, _dir) = open_test_store().await;
        let account = sample_account(Uuid::new_v4(), "dedupe@example.com", "#3584e4");
        store.upsert_account(&account).await.expect("upsert");
        let folders = store
            .replace_folders(
                account.id,
                &[
                    sample_folder_named(account.id, "INBOX", SpecialUse::Inbox),
                    sample_folder_named(account.id, "Sent", SpecialUse::Sent),
                ],
            )
            .await
            .expect("folders");
        let inbox_id = folders
            .iter()
            .find(|folder| folder.special == SpecialUse::Inbox)
            .map(|folder| folder.id)
            .expect("inbox");
        let sent_id = folders
            .iter()
            .find(|folder| folder.special == SpecialUse::Sent)
            .map(|folder| folder.id)
            .expect("sent");

        let thread_id = ThreadId(Uuid::new_v4());
        let mut inbox_copy = sample_envelope(&account, inbox_id, 1, "Same send");
        inbox_copy.thread_id = thread_id;
        let mut sent_copy = sample_envelope(&account, sent_id, 10, "Same send");
        sent_copy.thread_id = thread_id;
        sent_copy.date = inbox_copy.date;
        store
            .upsert_envelopes(&[inbox_copy, sent_copy], None)
            .await
            .expect("upsert");
        store
            .set_message_id_hdrs(inbox_id, &[(1, "<same@example.com>".to_string())])
            .await
            .expect("inbox hdr");
        store
            .set_message_id_hdrs(sent_id, &[(10, "<same@example.com>".to_string())])
            .await
            .expect("sent hdr");

        let thread_rows = store
            .thread_envelopes(thread_id, Some(&[inbox_id, sent_id]), true)
            .await
            .expect("thread envelopes");
        assert_eq!(thread_rows.len(), 1);
        assert_eq!(thread_rows[0].folder_id, inbox_id);
    }

    #[tokio::test]
    async fn search_conversations_respects_limit() {
        let (store, _dir) = open_test_store().await;

        let account = sample_account(Uuid::new_v4(), "search@example.com", "#3584e4");
        store.upsert_account(&account).await.expect("upsert");
        let folders = store
            .replace_folders(account.id, &[sample_folder(account.id, "INBOX")])
            .await
            .expect("folders");
        let inbox_id = folders[0].id;

        let mut env1 = sample_envelope(&account, inbox_id, 1, "Alpha unique subject");
        env1.thread_id = ThreadId(Uuid::new_v4());
        let mut env2 = sample_envelope(&account, inbox_id, 2, "Beta unique subject");
        env2.thread_id = ThreadId(Uuid::new_v4());
        store
            .upsert_envelopes(&[env1, env2], None)
            .await
            .expect("upsert");

        let conversations = store
            .search_conversations("subject:Alpha", None, 1)
            .await
            .expect("search conversations");
        assert_eq!(conversations.len(), 1);
        assert_eq!(conversations[0].latest.subject, "Alpha unique subject");
    }

    #[tokio::test]
    async fn reaction_stays_in_folder_lists_and_counts() {
        let (store, _dir) = open_test_store().await;
        let account = sample_account(Uuid::new_v4(), "react@example.com", "#3584e4");
        store
            .upsert_account(&account)
            .await
            .expect("upsert account");
        let folders = store
            .replace_folders(account.id, &[sample_folder(account.id, "INBOX")])
            .await
            .expect("folders");
        let folder_id = folders[0].id;

        let parent = sample_envelope(&account, folder_id, 1, "Parent");
        let reaction = sample_envelope(&account, folder_id, 2, "Reaction");
        store
            .upsert_envelopes(&[parent, reaction], None)
            .await
            .expect("upsert");
        let rows = store
            .messages_by_uids(folder_id, &[1, 2])
            .await
            .expect("uids");
        let parent_id = rows
            .iter()
            .find(|(_, uid, _)| *uid == 1)
            .map(|(id, _, _)| *id)
            .expect("parent id");
        let reaction_id = rows
            .iter()
            .find(|(_, uid, _)| *uid == 2)
            .map(|(id, _, _)| *id)
            .expect("reaction id");
        store
            .set_message_id_hdrs(folder_id, &[(1, "parent@example.com".to_string())])
            .await
            .expect("parent hdr");
        store
            .save_reaction(reaction_id, Some("👍"), Some("parent@example.com"))
            .await
            .expect("save reaction");

        let listed = store.folder_messages(folder_id, 10, 0).await.expect("list");
        assert_eq!(listed.len(), 2);
        assert!(listed.iter().any(|env| env.subject == "Reaction"));
        assert_eq!(
            store.folder_unread_count(folder_id).await.expect("unread"),
            2
        );
        assert_eq!(store.folder_live_count(folder_id).await.expect("live"), 2);

        let people = store
            .reactions_for_target(account.id, "parent@example.com")
            .await
            .expect("reactions");
        assert_eq!(people.len(), 1);
        assert_eq!(people[0].from_email, "sender@example.com");

        store.mark_deleted(parent_id).await.expect("delete parent");
        let after_parent_gone = store.folder_messages(folder_id, 10, 0).await.expect("list");
        assert_eq!(after_parent_gone.len(), 1);
        assert_eq!(after_parent_gone[0].subject, "Reaction");
    }
}
