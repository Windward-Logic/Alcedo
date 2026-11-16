//! Mock mail data (P4).

use std::collections::HashMap;

use alcedo_core::{
    Account, AccountId, Address, AuthMethod, Envelope, Folder, FolderId, MessageId, OAuthProvider,
    SpecialUse, ThreadId,
};
use chrono::{Duration, Utc};
use uuid::Uuid;

/// Mock accounts, folders, envelopes, and message bodies for `ALCEDO_MOCK=1`.
pub struct MockData {
    pub accounts: Vec<Account>,
    pub folders: HashMap<AccountId, Vec<Folder>>,
    pub envelopes: Vec<Envelope>,
    pub bodies: HashMap<MessageId, String>,
}

impl Default for MockData {
    fn default() -> Self {
        Self::new()
    }
}

impl MockData {
    pub fn new() -> Self {
        let accounts = mock_accounts();
        let folders = mock_folders(&accounts);
        let (envelopes, bodies) = mock_envelopes(&accounts);
        Self {
            accounts,
            folders,
            envelopes,
            bodies,
        }
    }
}

fn inbox_folder_id(account_index: usize) -> FolderId {
    FolderId((account_index + 1) as i64)
}

fn sent_folder_id(account_index: usize) -> FolderId {
    FolderId(100 + account_index as i64)
}

fn trash_folder_id(account_index: usize) -> FolderId {
    FolderId(200 + account_index as i64)
}

fn junk_folder_id(account_index: usize) -> FolderId {
    FolderId(500 + account_index as i64)
}

fn nosel_folder_id(account_index: usize) -> FolderId {
    FolderId(300 + account_index as i64)
}

fn projects_folder_id(account_index: usize) -> FolderId {
    FolderId(400 + account_index as i64)
}

fn projects_work_folder_id(account_index: usize) -> FolderId {
    FolderId(401 + account_index as i64)
}

fn projects_home_folder_id(account_index: usize) -> FolderId {
    FolderId(402 + account_index as i64)
}

fn is_gmail(account: &Account) -> bool {
    account.imap_host == OAuthProvider::Google.imap_host() || account.email.ends_with("@gmail.com")
}

fn mock_folders(accounts: &[Account]) -> HashMap<AccountId, Vec<Folder>> {
    accounts
        .iter()
        .enumerate()
        .map(|(account_index, account)| {
            let mut folders = Vec::new();

            if is_gmail(account) {
                folders.push(Folder {
                    id: nosel_folder_id(account_index),
                    account_id: account.id,
                    imap_name: "[Gmail]".to_string(),
                    delimiter: "/".to_string(),
                    special: SpecialUse::Other,
                    selectable: false,
                    uidvalidity: None,
                    uidnext: None,
                    highest_modseq: None,
                    unread: 0,
                    total: 0,
                });
            }

            let sent_imap_name = if is_gmail(account) {
                "[Gmail]/Sent Mail".to_string()
            } else {
                "Sent".to_string()
            };
            let junk_imap_name = if is_gmail(account) {
                "[Gmail]/Spam".to_string()
            } else {
                "Junk".to_string()
            };

            let inbox_unread = if account_index == 0 { 7 } else { 10 };
            let inbox_total = if account_index == 0 { 8 } else { 10 };
            let sent_unread = if account_index == 0 { 2 } else { 0 };
            let sent_total = if account_index == 0 { 2 } else { 0 };

            folders.extend([
                Folder {
                    id: inbox_folder_id(account_index),
                    account_id: account.id,
                    imap_name: "INBOX".to_string(),
                    delimiter: "/".to_string(),
                    special: SpecialUse::Inbox,
                    selectable: true,
                    uidvalidity: None,
                    uidnext: None,
                    highest_modseq: None,
                    unread: inbox_unread,
                    total: inbox_total,
                },
                Folder {
                    id: sent_folder_id(account_index),
                    account_id: account.id,
                    imap_name: sent_imap_name,
                    delimiter: "/".to_string(),
                    special: SpecialUse::Sent,
                    selectable: true,
                    uidvalidity: None,
                    uidnext: None,
                    highest_modseq: None,
                    unread: sent_unread,
                    total: sent_total,
                },
                Folder {
                    id: trash_folder_id(account_index),
                    account_id: account.id,
                    imap_name: "Trash".to_string(),
                    delimiter: "/".to_string(),
                    special: SpecialUse::Trash,
                    selectable: true,
                    uidvalidity: None,
                    uidnext: None,
                    highest_modseq: None,
                    unread: 0,
                    total: 0,
                },
                Folder {
                    id: junk_folder_id(account_index),
                    account_id: account.id,
                    imap_name: junk_imap_name,
                    delimiter: "/".to_string(),
                    special: SpecialUse::Junk,
                    selectable: true,
                    uidvalidity: None,
                    uidnext: None,
                    highest_modseq: None,
                    unread: 0,
                    total: 0,
                },
            ]);

            if is_gmail(account) {
                folders.extend([
                    Folder {
                        id: projects_folder_id(account_index),
                        account_id: account.id,
                        imap_name: "Projects".to_string(),
                        delimiter: "/".to_string(),
                        special: SpecialUse::Other,
                        selectable: true,
                        uidvalidity: None,
                        uidnext: None,
                        highest_modseq: None,
                        unread: 0,
                        total: 0,
                    },
                    Folder {
                        id: projects_work_folder_id(account_index),
                        account_id: account.id,
                        imap_name: "Projects/Work".to_string(),
                        delimiter: "/".to_string(),
                        special: SpecialUse::Other,
                        selectable: true,
                        uidvalidity: None,
                        uidnext: None,
                        highest_modseq: None,
                        unread: 1,
                        total: 1,
                    },
                    Folder {
                        id: projects_home_folder_id(account_index),
                        account_id: account.id,
                        imap_name: "Projects/Home".to_string(),
                        delimiter: "/".to_string(),
                        special: SpecialUse::Other,
                        selectable: true,
                        uidvalidity: None,
                        uidnext: None,
                        highest_modseq: None,
                        unread: 0,
                        total: 0,
                    },
                ]);
            }

            (account.id, folders)
        })
        .collect()
}

fn mock_accounts() -> Vec<Account> {
    vec![
        Account {
            id: AccountId(Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap()),
            email: "alice@gmail.com".to_string(),
            username: "alice@gmail.com".to_string(),
            split_credentials: false,
            smtp_username: String::new(),
            display_name: "Alice Gmail".to_string(),
            sender_name: String::new(),
            imap_host: OAuthProvider::Google.imap_host().to_string(),
            imap_port: 993,
            smtp_host: OAuthProvider::Google.smtp_host().to_string(),
            smtp_port: 465,
            smtp_starttls: false,
            auth: AuthMethod::OAuth2 {
                provider: OAuthProvider::Google,
            },
            accent_color: "#3584e4".to_string(),
            avatar_text: String::new(),
            avatar_image: Vec::new(),
        },
        Account {
            id: AccountId(Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap()),
            email: "bob@outlook.com".to_string(),
            username: "bob@outlook.com".to_string(),
            split_credentials: false,
            smtp_username: String::new(),
            display_name: "Bob Outlook".to_string(),
            sender_name: String::new(),
            imap_host: OAuthProvider::Microsoft.imap_host().to_string(),
            imap_port: 993,
            smtp_host: OAuthProvider::Microsoft.smtp_host().to_string(),
            smtp_port: 587,
            smtp_starttls: true,
            auth: AuthMethod::OAuth2 {
                provider: OAuthProvider::Microsoft,
            },
            accent_color: "#8661c5".to_string(),
            avatar_text: String::new(),
            avatar_image: Vec::new(),
        },
        Account {
            id: AccountId(Uuid::parse_str("33333333-3333-3333-3333-333333333333").unwrap()),
            email: "carol@yahoo.com".to_string(),
            username: "carol@yahoo.com".to_string(),
            split_credentials: false,
            smtp_username: String::new(),
            display_name: "Carol Yahoo".to_string(),
            sender_name: String::new(),
            imap_host: "imap.mail.yahoo.com".to_string(),
            imap_port: 993,
            smtp_host: "smtp.mail.yahoo.com".to_string(),
            smtp_port: 465,
            smtp_starttls: false,
            auth: AuthMethod::Password,
            accent_color: "#720e9e".to_string(),
            avatar_text: String::new(),
            avatar_image: Vec::new(),
        },
    ]
}

fn mock_envelopes(accounts: &[Account]) -> (Vec<Envelope>, HashMap<MessageId, String>) {
    let now = Utc::now();
    let subjects = [
        "Project kickoff notes",
        "Re: Design review feedback",
        "Your weekly digest",
        "Invoice #1042 attached",
        "Meeting moved to Thursday",
        "Welcome to Alcedo mock mode",
        "Security alert: new sign-in",
        "Team lunch on Friday?",
        "Fwd: Conference registration",
        "Quick question about the API",
    ];
    let senders = [
        ("Jordan Lee", "jordan@example.com"),
        ("Priya Shah", "priya@company.io"),
        ("Notifications", "noreply@service.dev"),
        ("Sam Rivera", "sam@client.org"),
        ("HR Team", "hr@company.io"),
    ];
    let snippets = [
        "Thanks for joining today — here are the action items we agreed on…",
        "I left a few comments inline. Overall this is looking great.",
        "Top stories from your subscriptions this week.",
        "Please find the PDF attached. Payment is due in 14 days.",
        "Can we push the sync to 3pm? I have a conflict at 2.",
    ];

    let mut envelopes = Vec::new();
    let mut bodies = HashMap::new();
    let mut message_id: i64 = 1;

    for (account_index, account) in accounts.iter().enumerate() {
        let inbox_id = inbox_folder_id(account_index);
        let sent_id = sent_folder_id(account_index);
        let per_account = 10usize;

        for offset in 0..per_account {
            let folder_id = if account_index == 0 && offset < 2 {
                sent_id
            } else {
                inbox_id
            };
            let global = account_index * per_account + offset;
            let (sender_name, sender_email) = senders[global % senders.len()];
            let subject = subjects[global % subjects.len()];
            let snippet = snippets[global % snippets.len()];
            let id = MessageId(message_id);
            message_id += 1;

            let hours_ago = (global as i64 * 3) + account_index as i64;
            let date = now - Duration::hours(hours_ago);
            let seen = !global.is_multiple_of(3);
            let flagged = global.is_multiple_of(7);

            let (from, to, cc) = if folder_id == sent_id {
                let from_addr = Address {
                    name: Some(account.display_name.clone()),
                    email: account.email.clone(),
                };
                if account_index == 0 && offset == 0 {
                    (
                        vec![from_addr],
                        vec![Address {
                            name: Some("Jordan Lee".to_string()),
                            email: "jordan@example.com".to_string(),
                        }],
                        vec![],
                    )
                } else if account_index == 0 && offset == 1 {
                    (
                        vec![from_addr],
                        vec![Address {
                            name: Some("Alice".to_string()),
                            email: "alice@example.com".to_string(),
                        }],
                        vec![Address {
                            name: Some("Priya Shah".to_string()),
                            email: "priya@company.io".to_string(),
                        }],
                    )
                } else {
                    (
                        vec![from_addr],
                        vec![Address {
                            name: Some(sender_name.to_string()),
                            email: sender_email.to_string(),
                        }],
                        vec![],
                    )
                }
            } else {
                (
                    vec![Address {
                        name: Some(sender_name.to_string()),
                        email: sender_email.to_string(),
                    }],
                    vec![Address {
                        name: Some(account.display_name.clone()),
                        email: account.email.clone(),
                    }],
                    vec![],
                )
            };

            let to_line = to
                .iter()
                .map(|addr| addr.formatted())
                .collect::<Vec<_>>()
                .join(", ");

            let body = format!(
                "From: {}\n\
                 To: {to_line}\n\
                 Subject: {subject}\n\
                 Date: {date}\n\n\
                 {snippet}\n\n\
                 Visit https://example.com/mock or email mock-links@example.com for details.\n\n\
                 This is mock message body #{global} from the {} account.",
                from.first().map(|a| a.formatted()).unwrap_or_default(),
                account.display_name
            );
            bodies.insert(id, body);

            envelopes.push(Envelope {
                id,
                account_id: account.id,
                folder_id,
                uid: (offset + 1) as u32,
                thread_id: ThreadId(Uuid::new_v4()),
                date,
                subject: subject.to_string(),
                from,
                to,
                cc,
                reply_to: Vec::new(),
                sender: Vec::new(),
                routing: None,
                snippet: snippet.to_string(),
                seen,
                flagged,
                answered: false,
                draft: false,
                has_attachments: subject == "Meeting moved to Thursday" || global.is_multiple_of(5),
                deleted_at: None,
                account_accent: account.accent_color.clone(),
                account_avatar: account.avatar_chip(),
                size_bytes: 0,
            });
        }
    }

    envelopes.sort_by(|a, b| b.date.cmp(&a.date));
    (envelopes, bodies)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn mock_folder_ids_are_unique_and_match_envelopes() {
        let mock = MockData::new();
        let mut ids = HashSet::new();
        for folders in mock.folders.values() {
            for folder in folders {
                assert!(
                    ids.insert(folder.id),
                    "duplicate mock folder id {:?}",
                    folder.id
                );
            }
        }
        for envelope in &mock.envelopes {
            assert!(
                ids.contains(&envelope.folder_id),
                "envelope folder {:?} is not in mock folders",
                envelope.folder_id
            );
        }
    }

    #[test]
    fn first_account_has_sent_envelopes() {
        let mock = MockData::new();
        let alice = &mock.accounts[0];
        let sent_id = mock
            .folders
            .get(&alice.id)
            .into_iter()
            .flatten()
            .find(|folder| matches!(folder.special, SpecialUse::Sent))
            .map(|folder| folder.id)
            .expect("alice sent folder");
        let sent_count = mock
            .envelopes
            .iter()
            .filter(|envelope| envelope.folder_id == sent_id)
            .count();
        assert!(sent_count >= 1, "expected seeded Sent envelopes");
        let other_inbox = inbox_folder_id(1);
        assert_ne!(sent_id, other_inbox);
    }

    #[test]
    fn sent_envelopes_are_outbound_shaped() {
        let mock = MockData::new();
        let alice = &mock.accounts[0];
        let sent_id = mock
            .folders
            .get(&alice.id)
            .into_iter()
            .flatten()
            .find(|folder| matches!(folder.special, SpecialUse::Sent))
            .map(|folder| folder.id)
            .expect("alice sent folder");
        for envelope in mock.envelopes.iter().filter(|e| e.folder_id == sent_id) {
            assert!(
                !envelope
                    .to
                    .iter()
                    .any(|addr| addr.email.eq_ignore_ascii_case(&alice.email)),
                "Sent envelope To should not be the account email"
            );
        }
    }
}
