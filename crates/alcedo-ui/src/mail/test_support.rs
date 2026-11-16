//! Shared fixtures for the mail module tests.

use super::*;

pub(super) fn addr(email: &str) -> Address {
    Address {
        name: None,
        email: email.to_string(),
    }
}

pub(super) fn envelope(from: &str, to: &[&str], cc: &[&str]) -> Envelope {
    envelope_with_reply_to(from, &[], to, cc)
}

pub(super) fn envelope_with_reply_to(
    from: &str,
    reply_to: &[&str],
    to: &[&str],
    cc: &[&str],
) -> Envelope {
    Envelope {
        id: MessageId(1),
        account_id: AccountId(Uuid::from_u128(1)),
        folder_id: FolderId(1),
        uid: 1,
        thread_id: ThreadId(Uuid::from_u128(2)),
        date: Utc::now(),
        subject: "Hello".to_string(),
        from: vec![addr(from)],
        to: to.iter().map(|email| addr(email)).collect(),
        cc: cc.iter().map(|email| addr(email)).collect(),
        reply_to: reply_to.iter().map(|email| addr(email)).collect(),
        sender: Vec::new(),
        routing: None,
        snippet: String::new(),
        seen: true,
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

pub(super) fn envelope_with_id(id: i64) -> Envelope {
    Envelope {
        id: MessageId(id),
        account_id: AccountId(Uuid::from_u128(1)),
        folder_id: FolderId(1),
        uid: id as u32,
        thread_id: ThreadId(Uuid::from_u128(2)),
        date: Utc::now(),
        subject: String::new(),
        from: vec![],
        to: vec![],
        cc: vec![],
        reply_to: Vec::new(),
        sender: Vec::new(),
        routing: None,
        snippet: String::new(),
        seen: true,
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

pub(super) fn sample_folder(id: i64, special: SpecialUse, selectable: bool) -> Folder {
    Folder {
        id: FolderId(id),
        account_id: AccountId(Uuid::from_u128(1)),
        imap_name: match special {
            SpecialUse::Junk => "Junk".to_string(),
            SpecialUse::Inbox => "INBOX".to_string(),
            _ => "Folder".to_string(),
        },
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
