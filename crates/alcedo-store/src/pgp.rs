//! Public OpenPGP key storage (no secret material).

use chrono::Utc;
use rusqlite::{Connection, params};

use alcedo_core::{AccountId, PgpKeySource, PgpPeerInfo, email_from_address_token};

use crate::{StoreError, StoreResult};

fn source_to_str(source: PgpKeySource) -> &'static str {
    match source {
        PgpKeySource::Own => "own",
        PgpKeySource::Import => "import",
        PgpKeySource::Autocrypt => "autocrypt",
    }
}

fn source_from_str(value: &str) -> StoreResult<PgpKeySource> {
    match value {
        "own" => Ok(PgpKeySource::Own),
        "import" => Ok(PgpKeySource::Import),
        "autocrypt" => Ok(PgpKeySource::Autocrypt),
        other => Err(StoreError::Other(format!(
            "unknown pgp key source: {other}"
        ))),
    }
}

pub fn upsert_key(
    conn: &Connection,
    fingerprint: &str,
    emails: &[String],
    armored: &str,
    source: PgpKeySource,
    account_id: Option<AccountId>,
) -> StoreResult<()> {
    let fingerprint = fingerprint.to_ascii_lowercase();
    let now = Utc::now().to_rfc3339();
    let emails_json = serde_json::to_string(emails)?;
    let account = account_id.map(|id| id.0.to_string());
    conn.execute(
        "INSERT INTO pgp_keys (fingerprint, emails_json, armored, source, account_id, created_utc, last_seen_utc)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(fingerprint) DO UPDATE SET
            emails_json = excluded.emails_json,
            armored = excluded.armored,
            source = excluded.source,
            account_id = COALESCE(excluded.account_id, pgp_keys.account_id),
            last_seen_utc = excluded.last_seen_utc",
        params![
            fingerprint,
            emails_json,
            armored,
            source_to_str(source),
            account,
            now,
            now,
        ],
    )?;
    Ok(())
}

pub fn delete_own_for_account(conn: &Connection, account_id: AccountId) -> StoreResult<()> {
    conn.execute(
        "DELETE FROM pgp_keys WHERE account_id = ?1 AND source = 'own'",
        params![account_id.0.to_string()],
    )?;
    Ok(())
}

pub fn delete_all_for_account(conn: &Connection, account_id: AccountId) -> StoreResult<()> {
    conn.execute(
        "DELETE FROM pgp_keys WHERE account_id = ?1",
        params![account_id.0.to_string()],
    )?;
    Ok(())
}

pub fn keys_for_emails(conn: &Connection, emails: &[String]) -> StoreResult<Vec<PgpPeerInfo>> {
    let mut out = Vec::new();
    for token in emails {
        let Some(email) = email_from_address_token(token) else {
            continue;
        };
        let mut stmt = conn.prepare(
            "SELECT fingerprint, emails_json, armored, source
             FROM pgp_keys
             WHERE emails_json LIKE ?1",
        )?;
        let pattern = format!("%{email}%");
        let rows = stmt.query_map(params![pattern], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?;
        for row in rows {
            let (fingerprint, emails_json, armored, source_str) = row?;
            let stored_emails: Vec<String> = serde_json::from_str(&emails_json)?;
            if !stored_emails.iter().any(|e| e.eq_ignore_ascii_case(&email)) {
                continue;
            }
            out.push(PgpPeerInfo {
                email: email.clone(),
                fingerprint,
                source: source_from_str(&source_str)?,
                armored: Some(armored),
            });
        }
    }
    Ok(out)
}

pub fn list_for_account(conn: &Connection, account_id: AccountId) -> StoreResult<Vec<PgpPeerInfo>> {
    let mut stmt = conn.prepare(
        "SELECT fingerprint, emails_json, armored, source
         FROM pgp_keys
         WHERE account_id = ?1
         ORDER BY source, fingerprint",
    )?;
    let rows = stmt.query_map(params![account_id.0.to_string()], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
        ))
    })?;
    let mut peers = Vec::new();
    for row in rows {
        let (fingerprint, emails_json, armored, source_str) = row?;
        let emails: Vec<String> = serde_json::from_str(&emails_json)?;
        let email = emails
            .first()
            .cloned()
            .unwrap_or_else(|| fingerprint.clone());
        peers.push(PgpPeerInfo {
            email,
            fingerprint,
            source: source_from_str(&source_str)?,
            armored: Some(armored),
        });
    }
    Ok(peers)
}

pub fn get_sign_outgoing(conn: &Connection, account_id: AccountId) -> StoreResult<bool> {
    let value: i64 = conn.query_row(
        "SELECT pgp_sign_outgoing FROM accounts WHERE id = ?1",
        params![account_id.0.to_string()],
        |row| row.get(0),
    )?;
    Ok(value != 0)
}

pub fn set_sign_outgoing(
    conn: &Connection,
    account_id: AccountId,
    enabled: bool,
) -> StoreResult<()> {
    conn.execute(
        "UPDATE accounts SET pgp_sign_outgoing = ?1 WHERE id = ?2",
        params![enabled as i64, account_id.0.to_string()],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema;
    use rusqlite::Connection;
    use uuid::Uuid;

    fn test_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        schema::apply_pragmas(&conn).unwrap();
        schema::migrate(&conn).unwrap();
        conn
    }

    #[test]
    fn upsert_key_is_idempotent() {
        let conn = test_conn();
        conn.execute_batch(
            "INSERT INTO accounts (id, email, display_name, imap_host, imap_port, smtp_host, smtp_port, smtp_starttls, auth_method, accent_color, avatar_text)
             VALUES ('aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa', 'me@example.com', 'Me', 'imap', 993, 'smtp', 465, 0, 'password', '#3584e4', 'ME');",
        )
        .unwrap();
        let account = AccountId(Uuid::parse_str("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa").unwrap());
        upsert_key(
            &conn,
            "abc123",
            &["peer@example.com".to_string()],
            "pub",
            PgpKeySource::Import,
            Some(account),
        )
        .unwrap();
        upsert_key(
            &conn,
            "abc123",
            &["peer@example.com".to_string()],
            "pub2",
            PgpKeySource::Import,
            Some(account),
        )
        .unwrap();
        let keys = list_for_account(&conn, account).unwrap();
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].armored.as_deref(), Some("pub2"));
    }

    #[test]
    fn own_keys_persist_until_delete_own() {
        let conn = test_conn();
        conn.execute_batch(
            "INSERT INTO accounts (id, email, display_name, imap_host, imap_port, smtp_host, smtp_port, smtp_starttls, auth_method, accent_color, avatar_text)
             VALUES ('aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa', 'me@example.com', 'Me', 'imap', 993, 'smtp', 465, 0, 'password', '#3584e4', 'ME');",
        )
        .unwrap();
        let account = AccountId(Uuid::parse_str("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa").unwrap());
        upsert_key(
            &conn,
            "own111",
            &["me@example.com".to_string()],
            "own-one",
            PgpKeySource::Own,
            Some(account),
        )
        .unwrap();
        upsert_key(
            &conn,
            "own222",
            &["me@example.com".to_string()],
            "own-two",
            PgpKeySource::Own,
            Some(account),
        )
        .unwrap();
        upsert_key(
            &conn,
            "peer333",
            &["peer@example.com".to_string()],
            "peer",
            PgpKeySource::Autocrypt,
            Some(account),
        )
        .unwrap();
        let keys = list_for_account(&conn, account).unwrap();
        assert_eq!(keys.len(), 3);
        delete_own_for_account(&conn, account).unwrap();
        let keys = list_for_account(&conn, account).unwrap();
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].source, PgpKeySource::Autocrypt);
    }

    #[test]
    fn sign_outgoing_roundtrip() {
        let conn = test_conn();
        conn.execute_batch(
            "INSERT INTO accounts (id, email, display_name, imap_host, imap_port, smtp_host, smtp_port, smtp_starttls, auth_method, accent_color, avatar_text)
             VALUES ('aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa', 'me@example.com', 'Me', 'imap', 993, 'smtp', 465, 0, 'password', '#3584e4', 'ME');",
        )
        .unwrap();
        let account = AccountId(Uuid::parse_str("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa").unwrap());
        assert!(!get_sign_outgoing(&conn, account).unwrap());
        set_sign_outgoing(&conn, account, true).unwrap();
        assert!(get_sign_outgoing(&conn, account).unwrap());
    }
}
