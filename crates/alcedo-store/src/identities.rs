//! Extra From identities per account.

use chrono::Utc;
use rusqlite::{Connection, params};

use alcedo_core::{AccountId, AccountIdentity, IdentityId};

use crate::StoreError;
use crate::StoreResult;
use crate::format_ts;

fn row_to_identity(
    id: String,
    account_id: String,
    display_name: String,
    email: String,
) -> StoreResult<AccountIdentity> {
    Ok(AccountIdentity {
        id: IdentityId(uuid::Uuid::parse_str(&id)?),
        account_id: AccountId(uuid::Uuid::parse_str(&account_id)?),
        display_name,
        email,
    })
}

pub fn list_identities(conn: &Connection, account: AccountId) -> StoreResult<Vec<AccountIdentity>> {
    let mut stmt = conn.prepare(
        "SELECT id, account_id, display_name, email
         FROM account_identities
         WHERE account_id = ?1
         ORDER BY created_at ASC",
    )?;
    let rows = stmt.query_map(params![account.0.to_string()], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
        ))
    })?;
    rows.map(|row| {
        let (id, account_id, display_name, email) = row?;
        row_to_identity(id, account_id, display_name, email)
    })
    .collect()
}

pub fn upsert_identity(
    conn: &Connection,
    identity: &AccountIdentity,
) -> StoreResult<AccountIdentity> {
    let now = Utc::now();
    let created_exists: bool = conn
        .query_row(
            "SELECT 1 FROM account_identities WHERE id = ?1",
            params![identity.id.0.to_string()],
            |_| Ok(()),
        )
        .is_ok();
    let created_at = if created_exists {
        conn.query_row(
            "SELECT created_at FROM account_identities WHERE id = ?1",
            params![identity.id.0.to_string()],
            |row| row.get::<_, String>(0),
        )?
    } else {
        format_ts(now)
    };
    conn.execute(
        "INSERT INTO account_identities (id, account_id, display_name, email, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(id) DO UPDATE SET
            display_name = excluded.display_name,
            email = excluded.email,
            updated_at = excluded.updated_at",
        params![
            identity.id.0.to_string(),
            identity.account_id.0.to_string(),
            identity.display_name,
            identity.email,
            created_at,
            format_ts(now),
        ],
    )
    .map_err(|e| {
        if let rusqlite::Error::SqliteFailure(err, _) = &e
            && err.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE {
                return StoreError::Other("identity email already exists".to_string());
            }
        StoreError::from(e)
    })?;
    Ok(identity.clone())
}

pub fn delete_identity(conn: &Connection, id: IdentityId) -> StoreResult<()> {
    conn.execute(
        "DELETE FROM account_identities WHERE id = ?1",
        params![id.0.to_string()],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
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
    fn identity_unique_email_per_account() {
        let dir = TempDir::new().expect("tempdir");
        let conn = schema::open_connection(&dir.path().join("alcedo.db")).expect("open");
        let account_id = Uuid::new_v4();
        accounts::upsert_account(&conn, &sample_account(account_id)).expect("account");
        let account = AccountId(account_id);

        let first = AccountIdentity {
            id: IdentityId(Uuid::new_v4()),
            account_id: account,
            display_name: "Alias".to_string(),
            email: "alias@example.com".to_string(),
        };
        upsert_identity(&conn, &first).expect("first");

        let dup = AccountIdentity {
            id: IdentityId(Uuid::new_v4()),
            account_id: account,
            display_name: "Other".to_string(),
            email: "alias@example.com".to_string(),
        };
        assert!(upsert_identity(&conn, &dup).is_err());

        accounts::delete_account(&conn, account).expect("delete");
        assert!(list_identities(&conn, account).expect("list").is_empty());
    }
}
