//! Shared fixtures for the worker module tests.

use super::*;
use alcedo_core::{Account, AccountId, AuthMethod, FolderId, SpecialUse};
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
