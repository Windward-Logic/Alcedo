//! Per-account compose signatures.

use rusqlite::{Connection, params};

use alcedo_core::{AccountId, AccountSignature, ComposeInline};

use crate::StoreResult;
use crate::{compose_format_from_str, format_ts};

pub fn get_signature(
    conn: &Connection,
    account: AccountId,
) -> StoreResult<Option<AccountSignature>> {
    let mut stmt =
        conn.prepare("SELECT format, body FROM account_signatures WHERE account_id = ?1")?;
    let mut rows = stmt.query_map(params![account.0.to_string()], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let Some(row) = rows.next() else {
        return Ok(None);
    };
    let (format, body) = row?;
    let mut inline_stmt = conn.prepare(
        "SELECT content_id, filename, mime, bytes FROM account_signature_inlines
         WHERE account_id = ?1 ORDER BY position ASC, content_id ASC",
    )?;
    let inlines = inline_stmt
        .query_map(params![account.0.to_string()], |row| {
            Ok(ComposeInline {
                content_id: row.get(0)?,
                filename: row.get(1)?,
                mime: row.get(2)?,
                bytes: row.get(3)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Some(AccountSignature {
        account_id: account,
        format: compose_format_from_str(&format)?,
        body,
        inlines,
    }))
}

pub fn upsert_signature(
    conn: &Connection,
    sig: &AccountSignature,
) -> StoreResult<AccountSignature> {
    let now = chrono::Utc::now();
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "INSERT INTO account_signatures (account_id, format, body, updated_at)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(account_id) DO UPDATE SET
            format = excluded.format,
            body = excluded.body,
            updated_at = excluded.updated_at",
        params![
            sig.account_id.0.to_string(),
            sig.format.as_str(),
            sig.body,
            format_ts(now),
        ],
    )?;
    tx.execute(
        "DELETE FROM account_signature_inlines WHERE account_id = ?1",
        params![sig.account_id.0.to_string()],
    )?;
    for (position, inline) in sig.inlines.iter().enumerate() {
        tx.execute(
            "INSERT INTO account_signature_inlines
             (account_id, content_id, filename, mime, bytes, position)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                sig.account_id.0.to_string(),
                inline.content_id,
                inline.filename,
                inline.mime,
                inline.bytes,
                position as i64,
            ],
        )?;
    }
    tx.commit()?;
    Ok(sig.clone())
}

pub fn delete_signature(conn: &Connection, account: AccountId) -> StoreResult<()> {
    conn.execute(
        "DELETE FROM account_signatures WHERE account_id = ?1",
        params![account.0.to_string()],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use alcedo_core::ComposeFormat;
    use rusqlite::Connection;
    use tempfile::TempDir;
    use uuid::Uuid;

    use crate::accounts;
    use crate::schema;

    fn sample_account(id: Uuid) -> alcedo_core::Account {
        alcedo_core::Account {
            id: AccountId(id),
            email: "user@example.com".to_string(),
            username: "user@example.com".to_string(),
            split_credentials: false,
            smtp_username: String::new(),
            display_name: "User".to_string(),
            sender_name: String::new(),
            imap_host: "imap.example.com".to_string(),
            imap_port: 993,
            smtp_host: "smtp.example.com".to_string(),
            smtp_port: 587,
            smtp_starttls: true,
            auth: alcedo_core::AuthMethod::Password,
            accent_color: String::new(),
            avatar_text: String::new(),
            avatar_image: Vec::new(),
        }
    }

    #[test]
    fn signature_upsert_replace() {
        let dir = TempDir::new().expect("tempdir");
        let conn = schema::open_connection(&dir.path().join("alcedo.db")).expect("open");
        let account_id = Uuid::new_v4();
        accounts::upsert_account(&conn, &sample_account(account_id)).expect("account");

        let sig = AccountSignature {
            account_id: AccountId(account_id),
            format: ComposeFormat::Plain,
            body: "Cheers".to_string(),
            inlines: Vec::new(),
        };
        upsert_signature(&conn, &sig).expect("upsert");
        let loaded = get_signature(&conn, AccountId(account_id))
            .expect("get")
            .expect("some");
        assert_eq!(loaded.body, "Cheers");

        let inline_a = ComposeInline {
            content_id: "a@example.com".to_string(),
            filename: "a.png".to_string(),
            mime: "image/png".to_string(),
            bytes: vec![1, 2],
        };
        let inline_b = ComposeInline {
            content_id: "b@example.com".to_string(),
            filename: "b.png".to_string(),
            mime: "image/png".to_string(),
            bytes: vec![3, 4],
        };
        let updated = AccountSignature {
            body: "Best".to_string(),
            format: ComposeFormat::Html,
            inlines: vec![inline_a.clone(), inline_b.clone()],
            ..sig
        };
        upsert_signature(&conn, &updated).expect("replace");
        let loaded = get_signature(&conn, AccountId(account_id))
            .expect("get")
            .expect("some");
        assert_eq!(loaded.body, "Best");
        assert_eq!(loaded.format, ComposeFormat::Html);
        assert_eq!(loaded.inlines.len(), 2);
        assert_eq!(loaded.inlines[0].content_id, "a@example.com");
        assert_eq!(loaded.inlines[1].content_id, "b@example.com");

        let one_inline = AccountSignature {
            inlines: vec![inline_a],
            ..loaded
        };
        upsert_signature(&conn, &one_inline).expect("one inline");
        let loaded = get_signature(&conn, AccountId(account_id))
            .expect("get")
            .expect("some");
        assert_eq!(loaded.inlines.len(), 1);
        assert_eq!(loaded.inlines[0].content_id, "a@example.com");
    }

    #[test]
    fn fresh_schema_has_inlines_table_no_logo_column() {
        let dir = TempDir::new().expect("tempdir");
        let conn = schema::open_connection(&dir.path().join("alcedo.db")).expect("open");
        let cols: Vec<String> = conn
            .prepare("PRAGMA table_info(account_signatures)")
            .expect("pragma")
            .query_map([], |row| row.get(1))
            .expect("map")
            .filter_map(Result::ok)
            .collect();
        assert!(!cols.iter().any(|c| c == "logo_png"));
        conn.query_row(
            "SELECT name FROM sqlite_master WHERE type='table' AND name='account_signature_inlines'",
            [],
            |row| row.get::<_, String>(0),
        )
        .expect("inlines table");
    }

    #[test]
    fn migration_031_logo_to_inline() {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("migrate.db");
        let conn = Connection::open(&path).expect("open");
        conn.execute_batch(
            "CREATE TABLE accounts (id TEXT PRIMARY KEY, email TEXT NOT NULL);
             CREATE TABLE account_signatures (
               account_id TEXT PRIMARY KEY,
               format TEXT NOT NULL,
               body TEXT NOT NULL,
               logo_png BLOB NOT NULL DEFAULT x'',
               updated_at TEXT NOT NULL DEFAULT ''
             );",
        )
        .expect("v30 shape");

        let html_id = Uuid::new_v4();
        let plain_id = Uuid::new_v4();
        let logo_bytes: Vec<u8> = vec![0x89, 0x50, 0x4e, 0x47];
        let html_cid = format!("signature-logo-{html_id}@alcedo.mail");
        let html_body = format!(r#"<img src="cid:{html_cid}">"#);
        conn.execute(
            "INSERT INTO account_signatures (account_id, format, body, logo_png) VALUES (?1, 'html', ?2, ?3)",
            params![html_id.to_string(), html_body, logo_bytes.clone()],
        )
        .expect("html sig");
        conn.execute(
            "INSERT INTO account_signatures (account_id, format, body, logo_png) VALUES (?1, 'plain', 'Cheers', ?2)",
            params![plain_id.to_string(), logo_bytes],
        )
        .expect("plain sig");

        conn.execute_batch(include_str!("../migrations/031_signature_inlines.sql"))
            .expect("031");

        let migrated_cid: String = conn
            .query_row(
                "SELECT content_id FROM account_signature_inlines WHERE account_id = ?1",
                params![html_id.to_string()],
                |row| row.get(0),
            )
            .expect("html inline");
        assert_eq!(migrated_cid, html_cid);
        let filename: String = conn
            .query_row(
                "SELECT filename FROM account_signature_inlines WHERE account_id = ?1",
                params![html_id.to_string()],
                |row| row.get(0),
            )
            .expect("filename");
        assert_eq!(filename, "signature-logo.png");
        let bytes: Vec<u8> = conn
            .query_row(
                "SELECT bytes FROM account_signature_inlines WHERE account_id = ?1",
                params![html_id.to_string()],
                |row| row.get(0),
            )
            .expect("bytes");
        assert_eq!(bytes, vec![0x89, 0x50, 0x4e, 0x47]);

        let plain_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM account_signature_inlines WHERE account_id = ?1",
                params![plain_id.to_string()],
                |row| row.get(0),
            )
            .expect("count");
        assert_eq!(plain_count, 0);

        let cols: Vec<String> = conn
            .prepare("PRAGMA table_info(account_signatures)")
            .expect("pragma")
            .query_map([], |row| row.get(1))
            .expect("map")
            .filter_map(Result::ok)
            .collect();
        assert!(!cols.iter().any(|c| c == "logo_png"));
        assert!(alcedo_core::content_id_is_safe(&migrated_cid));
    }
}
