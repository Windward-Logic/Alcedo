//! Per-sender remote image allowlist.

use rusqlite::{Connection, params};

use alcedo_core::normalize_sender_email;

use crate::{StoreError, StoreResult};

pub fn list_senders(conn: &Connection) -> StoreResult<Vec<String>> {
    let mut stmt = conn.prepare("SELECT email FROM remote_image_senders ORDER BY email")?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    let mut emails = Vec::new();
    for row in rows {
        emails.push(row?);
    }
    Ok(emails)
}

pub fn set_sender(conn: &Connection, email: &str, allow: bool) -> StoreResult<()> {
    let Some(normalized) = normalize_sender_email(email) else {
        return Err(StoreError::Other("email is required".to_string()));
    };
    if allow {
        conn.execute(
            "INSERT OR IGNORE INTO remote_image_senders (email) VALUES (?1)",
            params![normalized],
        )?;
    } else {
        conn.execute(
            "DELETE FROM remote_image_senders WHERE email = ?1",
            params![normalized],
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    use super::*;
    use crate::messages::{get_message, set_allow_remote_images, upsert_envelopes};
    use crate::schema;
    use alcedo_core::{AccountId, Address, Envelope, FolderId, MessageId, ThreadId};
    use chrono::Utc;
    use uuid::Uuid;

    fn test_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        schema::apply_pragmas(&conn).unwrap();
        schema::migrate(&conn).unwrap();
        conn
    }

    #[test]
    fn migrate_twice_is_idempotent() {
        let conn = test_conn();
        schema::migrate(&conn).unwrap();
        let version: i64 = conn
            .query_row("SELECT MAX(version) FROM schema_migrations", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(version, 37);
    }

    #[test]
    fn set_sender_case_fold_and_idempotent() {
        let conn = test_conn();
        set_sender(&conn, "Alice@Example.com", true).unwrap();
        set_sender(&conn, "alice@example.com", true).unwrap();
        let senders = list_senders(&conn).unwrap();
        assert_eq!(senders, vec!["alice@example.com".to_string()]);
    }

    #[test]
    fn delete_sender() {
        let conn = test_conn();
        set_sender(&conn, "a@example.com", true).unwrap();
        set_sender(&conn, "a@example.com", false).unwrap();
        assert!(list_senders(&conn).unwrap().is_empty());
    }

    #[test]
    fn empty_email_errors() {
        let conn = test_conn();
        let err = set_sender(&conn, "  ", true).unwrap_err();
        assert!(err.to_string().contains("email is required"));
    }

    #[test]
    fn upsert_envelope_does_not_clear_allow_flag() {
        let conn = test_conn();
        conn.execute_batch(
            "INSERT INTO accounts (id, email, display_name, imap_host, imap_port, smtp_host, smtp_port, smtp_starttls, auth_method, accent_color, avatar_text)
             VALUES ('aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa', 'me@example.com', 'Me', 'imap', 993, 'smtp', 465, 0, 'password', '#3584e4', 'ME');
             INSERT INTO folders (id, account_id, imap_name, delimiter, special, selectable)
             VALUES (1, 'aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa', 'INBOX', '/', 'inbox', 1);
             INSERT INTO messages (id, account_id, folder_id, uid, thread_id, date_utc, subject, from_json, to_json, cc_json, snippet, seen, flagged, answered, draft, has_attachments, allow_remote_images)
             VALUES (1, 'aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa', 1, 1, 'bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb', '2024-01-01T00:00:00Z', 'Hi', '[]', '[]', '[]', 'snip', 0, 0, 0, 0, 0, 1);",
        )
        .unwrap();

        let account_id =
            AccountId(Uuid::parse_str("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa").unwrap());
        let folder_id = FolderId(1);
        let env = Envelope {
            id: MessageId(1),
            account_id,
            folder_id,
            uid: 1,
            thread_id: ThreadId(Uuid::parse_str("bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb").unwrap()),
            date: Utc::now(),
            subject: "Updated".to_string(),
            from: vec![Address {
                name: None,
                email: "sender@example.com".to_string(),
            }],
            to: vec![],
            cc: vec![],
            reply_to: Vec::new(),
            sender: Vec::new(),
            routing: None,
            snippet: "updated".to_string(),
            seen: true,
            flagged: false,
            answered: false,
            draft: false,
            has_attachments: false,
            deleted_at: None,
            account_accent: "#3584e4".to_string(),
            account_avatar: "ME".to_string(),
            size_bytes: 0,
        };
        upsert_envelopes(&conn, &[env], None).unwrap();

        let stored = get_message(&conn, MessageId(1)).unwrap().expect("message");
        assert!(stored.allow_remote_images);
    }

    #[test]
    fn set_allow_remote_images_persists() {
        let conn = test_conn();
        conn.execute_batch(
            "INSERT INTO accounts (id, email, display_name, imap_host, imap_port, smtp_host, smtp_port, smtp_starttls, auth_method, accent_color, avatar_text)
             VALUES ('aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa', 'me@example.com', 'Me', 'imap', 993, 'smtp', 465, 0, 'password', '#3584e4', 'ME');
             INSERT INTO folders (id, account_id, imap_name, delimiter, special, selectable)
             VALUES (1, 'aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa', 'INBOX', '/', 'inbox', 1);
             INSERT INTO messages (id, account_id, folder_id, uid, thread_id, date_utc, subject, from_json, to_json, cc_json, snippet, seen, flagged, answered, draft, has_attachments)
             VALUES (1, 'aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa', 1, 1, 'bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb', '2024-01-01T00:00:00Z', 'Hi', '[]', '[]', '[]', 'snip', 0, 0, 0, 0, 0);",
        )
        .unwrap();

        set_allow_remote_images(&conn, MessageId(1), true).unwrap();
        let stored = get_message(&conn, MessageId(1)).unwrap().expect("message");
        assert!(stored.allow_remote_images);
    }
}
