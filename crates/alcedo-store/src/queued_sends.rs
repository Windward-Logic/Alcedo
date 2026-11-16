//! Persisted outgoing drafts while a send is queued or in flight (crash recovery).

use chrono::{DateTime, Utc};
use rusqlite::{Connection, params};

use alcedo_core::{ComposeDraft, QueuedSend, QueuedSendId};

use crate::StoreResult;

pub fn insert_queued_send(conn: &Connection, queued: &QueuedSend) -> StoreResult<()> {
    let draft_json = serde_json::to_string(&queued.draft)?;
    conn.execute(
        "INSERT INTO queued_sends (id, account_id, draft_json, queued_at)
         VALUES (?1, ?2, ?3, ?4)",
        params![
            queued.id.0.to_string(),
            queued.draft.account_id.0.to_string(),
            draft_json,
            queued.queued_at.to_rfc3339(),
        ],
    )?;
    Ok(())
}

pub fn delete_queued_send(conn: &Connection, id: QueuedSendId) -> StoreResult<()> {
    conn.execute(
        "DELETE FROM queued_sends WHERE id = ?1",
        params![id.0.to_string()],
    )?;
    Ok(())
}

pub fn list_queued_sends(conn: &Connection) -> StoreResult<Vec<QueuedSend>> {
    let mut stmt = conn.prepare(
        "SELECT id, draft_json, queued_at FROM queued_sends ORDER BY queued_at ASC LIMIT 100",
    )?;
    let rows = stmt.query_map([], |row| {
        let id_str: String = row.get(0)?;
        let draft_json: String = row.get(1)?;
        let queued_at_str: String = row.get(2)?;
        Ok((id_str, draft_json, queued_at_str))
    })?;
    let mut out = Vec::new();
    for r in rows {
        let (id_str, draft_json, queued_at_str) = r?;
        let id = QueuedSendId(
            uuid::Uuid::parse_str(&id_str)
                .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?,
        );
        let draft: ComposeDraft = serde_json::from_str(&draft_json)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
        let queued_at = DateTime::parse_from_rfc3339(&queued_at_str)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?
            .with_timezone(&Utc);
        out.push(QueuedSend {
            id,
            draft,
            queued_at,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use rusqlite::params;

    use super::*;
    use crate::schema;
    use alcedo_core::{AccountId, ComposeAttachment, ComposeFormat, IdentityId};

    fn open_test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        schema::migrate(&conn).unwrap();
        conn
    }

    fn sample_draft(account_id: AccountId) -> ComposeDraft {
        ComposeDraft {
            account_id,
            to: vec!["a@example.com".to_string()],
            cc: vec!["cc@example.com".to_string()],
            bcc: Vec::new(),
            subject: "subj".to_string(),
            body_text: "body".to_string(),
            format: ComposeFormat::Plain,
            request_mdn: false,
            tracking_pixel_url: None,
            attachments: vec![ComposeAttachment {
                filename: "f.bin".to_string(),
                mime: "application/octet-stream".to_string(),
                bytes: vec![1, 2, 3, 255],
            }],
            inlines: Vec::new(),
            collect_outgoing_contacts: false,
            in_reply_to: None,
            references: None,
            thread_id: None,
            approve_contact_email: None,
            sign: false,
            encrypt: false,
            autocrypt_header: None,
            identity_id: Option::<IdentityId>::None,
            custom_from: None,
            imap_draft: None,
            reaction_emoji: None,
        }
    }

    #[test]
    fn queued_send_round_trips_with_attachment_bytes() {
        let conn = open_test_db();
        let account_id = AccountId(uuid::Uuid::new_v4());
        conn.execute(
            "INSERT INTO accounts (id, email, display_name, imap_host, imap_port, smtp_host, smtp_port, smtp_starttls, auth_method, accent_color, avatar_text)
             VALUES (?1, 'a@b.com', 'A', 'imap', 993, 'smtp', 587, 0, 'password', '#000', 'A')",
            params![account_id.0.to_string()],
        )
        .unwrap();
        let draft = sample_draft(account_id);
        let id = QueuedSendId(uuid::Uuid::new_v4());
        let queued_at = Utc::now();
        let queued = QueuedSend {
            id,
            draft: draft.clone(),
            queued_at,
        };
        insert_queued_send(&conn, &queued).unwrap();
        let list = list_queued_sends(&conn).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, id);
        assert_eq!(list[0].draft.to, draft.to);
        assert_eq!(list[0].draft.cc, draft.cc);
        assert_eq!(
            list[0].draft.attachments[0].bytes,
            draft.attachments[0].bytes
        );
    }

    #[test]
    fn delete_removes_only_that_row() {
        let conn = open_test_db();
        let account_id = AccountId(uuid::Uuid::new_v4());
        conn.execute(
            "INSERT INTO accounts (id, email, display_name, imap_host, imap_port, smtp_host, smtp_port, smtp_starttls, auth_method, accent_color, avatar_text)
             VALUES (?1, 'a@b.com', 'A', 'imap', 993, 'smtp', 587, 0, 'password', '#000', 'A')",
            params![account_id.0.to_string()],
        )
        .unwrap();
        let a = QueuedSendId(uuid::Uuid::new_v4());
        let b = QueuedSendId(uuid::Uuid::new_v4());
        let t = Utc::now();
        insert_queued_send(
            &conn,
            &QueuedSend {
                id: a,
                draft: sample_draft(account_id),
                queued_at: t,
            },
        )
        .unwrap();
        insert_queued_send(
            &conn,
            &QueuedSend {
                id: b,
                draft: sample_draft(account_id),
                queued_at: t + chrono::Duration::seconds(1),
            },
        )
        .unwrap();
        delete_queued_send(&conn, a).unwrap();
        let list = list_queued_sends(&conn).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, b);
    }

    #[test]
    fn deleting_the_account_cascades() {
        let conn = open_test_db();
        let account_id = AccountId(uuid::Uuid::new_v4());
        conn.execute(
            "INSERT INTO accounts (id, email, display_name, imap_host, imap_port, smtp_host, smtp_port, smtp_starttls, auth_method, accent_color, avatar_text)
             VALUES (?1, 'a@b.com', 'A', 'imap', 993, 'smtp', 587, 0, 'password', '#000', 'A')",
            params![account_id.0.to_string()],
        )
        .unwrap();
        let id = QueuedSendId(uuid::Uuid::new_v4());
        insert_queued_send(
            &conn,
            &QueuedSend {
                id,
                draft: sample_draft(account_id),
                queued_at: Utc::now(),
            },
        )
        .unwrap();
        conn.execute(
            "DELETE FROM accounts WHERE id = ?1",
            params![account_id.0.to_string()],
        )
        .unwrap();
        assert!(list_queued_sends(&conn).unwrap().is_empty());
    }
}
