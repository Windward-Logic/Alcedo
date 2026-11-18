//! Shared fixtures for the worker module tests.

use super::*;
use alcedo_core::{
    Account, AccountId, Address, AuthMethod, Envelope, FolderId, SpecialUse, ThreadId,
};
use uuid::Uuid;

pub(super) fn folder_with(uidvalidity: Option<u32>, highest_modseq: Option<u64>) -> Folder {
    Folder {
        id: FolderId(1),
        account_id: AccountId(Uuid::nil()),
        imap_name: "INBOX".to_string(),
        delimiter: "/".to_string(),
        special: SpecialUse::Inbox,
        selectable: true,
        uidvalidity,
        uidnext: Some(10),
        highest_modseq,
        unread: 0,
        total: 0,
    }
}

pub(super) fn sample_imap_draft(uid: u32, uidvalidity: u32) -> ImapDraftRef {
    ImapDraftRef {
        folder_id: FolderId(9),
        uid,
        uidvalidity,
        message_id: "<session@example.com>".to_string(),
        last_internaldate: None,
    }
}

pub(super) fn folder_named(
    id: i64,
    special: SpecialUse,
    imap_name: &str,
    selectable: bool,
) -> Folder {
    Folder {
        id: FolderId(id),
        account_id: AccountId(Uuid::nil()),
        imap_name: imap_name.to_string(),
        delimiter: "/".to_string(),
        special,
        selectable,
        uidvalidity: None,
        uidnext: None,
        highest_modseq: None,
        unread: 0,
        total: 0,
    }
}

pub(super) fn account_with_imap_host(host: &str) -> Account {
    Account {
        id: AccountId(Uuid::nil()),
        email: "a@b.com".to_string(),
        username: "a@b.com".to_string(),
        split_credentials: false,
        smtp_username: String::new(),
        display_name: String::new(),
        sender_name: String::new(),
        imap_host: host.to_string(),
        imap_port: 993,
        smtp_host: "smtp.example.com".to_string(),
        smtp_port: 465,
        smtp_starttls: false,
        auth: AuthMethod::Password,
        accent_color: "#000000".to_string(),
        avatar_text: String::new(),
        avatar_image: Vec::new(),
    }
}

pub(super) const COPY_MESSAGE_ID: &str = "<hello@example.com>";

pub(super) fn copy_envelope(
    account: &Account,
    folder_id: FolderId,
    uid: u32,
    age: i64,
) -> Envelope {
    Envelope {
        id: MessageId(0),
        account_id: account.id,
        folder_id,
        uid,
        thread_id: ThreadId(Uuid::new_v4()),
        date: DateTime::from_timestamp(1_700_000_000 - age, 0).expect("date"),
        subject: "Hello".to_string(),
        from: vec![Address {
            name: None,
            email: "sender@example.com".to_string(),
        }],
        to: Vec::new(),
        cc: Vec::new(),
        reply_to: Vec::new(),
        sender: Vec::new(),
        routing: None,
        snippet: String::new(),
        seen: false,
        flagged: false,
        answered: false,
        draft: false,
        has_attachments: false,
        deleted_at: None,
        account_accent: String::new(),
        account_avatar: String::new(),
        size_bytes: 0,
    }
}

/// One Gmail message shown in Inbox and All Mail, plus a separate All Mail
/// message with the same date, subject and sender but its own Message-ID.
pub(super) struct CopyFixture {
    pub(super) _dir: tempfile::TempDir,
    pub(super) store: StoreHandle,
    pub(super) account: Account,
    pub(super) inbox: FolderId,
    pub(super) all_mail: FolderId,
    pub(super) original: MessageId,
    pub(super) copy: MessageId,
    pub(super) lookalike: MessageId,
}

impl CopyFixture {
    pub(super) async fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = StoreHandle::open_path(&dir.path().join("store.db")).expect("store");
        let account = account_with_imap_host("imap.gmail.com");
        store.upsert_account(&account).await.expect("account");
        let folder = |name: &str, special| Folder {
            account_id: account.id,
            id: FolderId(0),
            uidvalidity: Some(1),
            ..folder_named(0, special, name, true)
        };
        let folders = store
            .replace_folders(
                account.id,
                &[
                    folder("INBOX", SpecialUse::Inbox),
                    folder("[Gmail]/All Mail", SpecialUse::Other),
                ],
            )
            .await
            .expect("folders");
        let inbox = folders[0].id;
        let all_mail = folders[1].id;
        store
            .upsert_envelopes(
                &[
                    copy_envelope(&account, inbox, 1, 0),
                    copy_envelope(&account, all_mail, 7, 0),
                    copy_envelope(&account, all_mail, 8, 0),
                ],
                None,
            )
            .await
            .expect("envelopes");
        store
            .set_message_id_hdrs(inbox, &[(1, COPY_MESSAGE_ID.to_string())])
            .await
            .expect("inbox header");
        store
            .set_message_id_hdrs(
                all_mail,
                &[
                    (7, COPY_MESSAGE_ID.to_string()),
                    (8, "<lookalike@example.com>".to_string()),
                ],
            )
            .await
            .expect("all mail headers");
        let inbox_rows = store.folder_messages(inbox, 10, 0).await.expect("inbox");
        let all_rows = store.folder_messages(all_mail, 10, 0).await.expect("all");
        let id_of = |uid| all_rows.iter().find(|e| e.uid == uid).expect("row").id;
        Self {
            original: inbox_rows[0].id,
            copy: id_of(7),
            lookalike: id_of(8),
            _dir: dir,
            store,
            account,
            inbox,
            all_mail,
        }
    }

    pub(super) async fn envelope(&self, id: MessageId) -> Envelope {
        self.store
            .envelopes_by_ids(&[id])
            .await
            .expect("load")
            .remove(0)
    }

    pub(super) async fn is_seen(&self, id: MessageId) -> bool {
        self.envelope(id).await.seen
    }

    pub(super) async fn stored_unread(&self, folder: FolderId) -> u32 {
        self.store
            .get_folder(folder)
            .await
            .expect("get folder")
            .expect("folder")
            .unread
    }
}
