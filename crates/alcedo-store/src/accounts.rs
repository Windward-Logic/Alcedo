//! Account persistence (P2).

use rusqlite::{Connection, params};

use alcedo_core::{Account, AccountId, AuthMethod, OAuthProvider, SmtpEnvelopePolicy};

use crate::StoreError;
use crate::StoreResult;

fn auth_method_to_str(auth: &AuthMethod) -> &'static str {
    match auth {
        AuthMethod::OAuth2 {
            provider: OAuthProvider::Google,
        } => "oauth2-google",
        AuthMethod::OAuth2 {
            provider: OAuthProvider::Microsoft,
        } => "oauth2-microsoft",
        AuthMethod::Password => "password",
    }
}

fn auth_method_from_str(s: &str) -> StoreResult<AuthMethod> {
    match s {
        "oauth2-google" => Ok(AuthMethod::OAuth2 {
            provider: OAuthProvider::Google,
        }),
        "oauth2-microsoft" => Ok(AuthMethod::OAuth2 {
            provider: OAuthProvider::Microsoft,
        }),
        "password" => Ok(AuthMethod::Password),
        other => Err(StoreError::Other(format!("unknown auth_method: {other}"))),
    }
}

pub fn upsert_account(conn: &Connection, account: &Account) -> StoreResult<()> {
    conn.execute(
        "INSERT INTO accounts (
            id, email, username, split_credentials, smtp_username, display_name, sender_name,
            imap_host, imap_port, smtp_host, smtp_port, smtp_starttls, auth_method,
            accent_color, avatar_text, avatar_image, smtp_envelope_policy
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16,
            CASE WHEN ?13 = 'password' THEN 'same_domain_identity' ELSE 'mailbox' END)
        ON CONFLICT(id) DO UPDATE SET
            email = excluded.email,
            username = excluded.username,
            split_credentials = excluded.split_credentials,
            smtp_username = excluded.smtp_username,
            display_name = excluded.display_name,
            sender_name = excluded.sender_name,
            imap_host = excluded.imap_host,
            imap_port = excluded.imap_port,
            smtp_host = excluded.smtp_host,
            smtp_port = excluded.smtp_port,
            smtp_starttls = excluded.smtp_starttls,
            auth_method = excluded.auth_method,
            accent_color = excluded.accent_color,
            avatar_text = excluded.avatar_text,
            avatar_image = excluded.avatar_image",
        params![
            account.id.0.to_string(),
            account.email,
            account.username,
            account.split_credentials,
            account.smtp_username,
            account.display_name,
            account.sender_name,
            account.imap_host,
            account.imap_port,
            account.smtp_host,
            account.smtp_port,
            account.smtp_starttls,
            auth_method_to_str(&account.auth),
            account.accent_color,
            account.avatar_text,
            account.avatar_image,
        ],
    )?;
    Ok(())
}

pub fn delete_account(conn: &Connection, id: AccountId) -> StoreResult<()> {
    conn.execute(
        "DELETE FROM accounts WHERE id = ?1",
        params![id.0.to_string()],
    )?;
    Ok(())
}

pub fn list_accounts(conn: &Connection) -> StoreResult<Vec<Account>> {
    let mut stmt = conn.prepare(
        "SELECT id, email, username, split_credentials, smtp_username, display_name, sender_name,
                imap_host, imap_port, smtp_host, smtp_port, smtp_starttls, auth_method,
                accent_color, avatar_text, avatar_image
         FROM accounts
         ORDER BY email",
    )?;

    let rows = stmt.query_map([], |row| {
        let id_str: String = row.get(0)?;
        let auth_str: String = row.get(12)?;
        Ok((
            id_str,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, bool>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, String>(5)?,
            row.get::<_, String>(6)?,
            row.get::<_, String>(7)?,
            row.get::<_, u16>(8)?,
            row.get::<_, String>(9)?,
            row.get::<_, u16>(10)?,
            row.get::<_, bool>(11)?,
            auth_str,
            row.get::<_, String>(13)?,
            row.get::<_, String>(14)?,
            row.get::<_, Vec<u8>>(15)?,
        ))
    })?;

    let mut accounts = Vec::new();
    for row in rows {
        let (
            id_str,
            email,
            username,
            split_credentials,
            smtp_username,
            display_name,
            sender_name,
            imap_host,
            imap_port,
            smtp_host,
            smtp_port,
            smtp_starttls,
            auth_str,
            accent_color,
            avatar_text,
            avatar_image,
        ) = row?;
        let id = AccountId(uuid::Uuid::parse_str(&id_str)?);
        let auth = auth_method_from_str(&auth_str)?;
        accounts.push(Account {
            id,
            email,
            username,
            split_credentials,
            smtp_username,
            display_name,
            sender_name,
            imap_host,
            imap_port,
            smtp_host,
            smtp_port,
            smtp_starttls,
            auth,
            accent_color,
            avatar_text,
            avatar_image,
        });
    }
    Ok(accounts)
}

pub fn get_smtp_envelope_policy(
    conn: &Connection,
    account_id: AccountId,
) -> StoreResult<SmtpEnvelopePolicy> {
    let value: String = conn.query_row(
        "SELECT smtp_envelope_policy FROM accounts WHERE id = ?1",
        params![account_id.0.to_string()],
        |row| row.get(0),
    )?;
    Ok(SmtpEnvelopePolicy::parse(&value))
}

pub fn set_smtp_envelope_policy(
    conn: &Connection,
    account_id: AccountId,
    policy: SmtpEnvelopePolicy,
) -> StoreResult<()> {
    conn.execute(
        "UPDATE accounts SET smtp_envelope_policy = ?1 WHERE id = ?2",
        params![policy.as_str(), account_id.0.to_string()],
    )?;
    Ok(())
}

/// Domains where every address reaches this account, normalized.
pub fn get_catch_all_domains(conn: &Connection, account_id: AccountId) -> StoreResult<Vec<String>> {
    let value: String = conn.query_row(
        "SELECT catch_all_domains FROM accounts WHERE id = ?1",
        params![account_id.0.to_string()],
        |row| row.get(0),
    )?;
    Ok(alcedo_core::parse_catch_all_domains(&value))
}

pub fn set_catch_all_domains(
    conn: &Connection,
    account_id: AccountId,
    domains: &[String],
) -> StoreResult<()> {
    let normalized = alcedo_core::parse_catch_all_domains(&domains.join(","));
    conn.execute(
        "UPDATE accounts SET catch_all_domains = ?1 WHERE id = ?2",
        params![normalized.join(","), account_id.0.to_string()],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema;
    use tempfile::TempDir;
    use uuid::Uuid;

    fn sample_account(username: &str) -> Account {
        Account {
            id: AccountId(Uuid::new_v4()),
            email: "user@example.com".to_string(),
            username: username.to_string(),
            split_credentials: false,
            smtp_username: String::new(),
            display_name: "Work".to_string(),
            sender_name: String::new(),
            imap_host: "imap.example.com".to_string(),
            imap_port: 993,
            smtp_host: "smtp.example.com".to_string(),
            smtp_port: 465,
            smtp_starttls: false,
            auth: AuthMethod::Password,
            accent_color: "#3584e4".to_string(),
            avatar_text: "U".to_string(),
            avatar_image: Vec::new(),
        }
    }

    #[test]
    fn username_roundtrip_and_migration_default() {
        let dir = TempDir::new().expect("tempdir");
        let conn = schema::open_connection(&dir.path().join("alcedo.db")).expect("open");
        let account = sample_account("imapuser");
        upsert_account(&conn, &account).expect("upsert");
        let loaded = list_accounts(&conn).expect("list");
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].username, "imapuser");
        assert_eq!(loaded[0].login_name(), "imapuser");
        assert_eq!(loaded[0].label(), "Work");
    }

    #[test]
    fn catch_all_domains_default_empty_and_roundtrip_normalized() {
        let dir = TempDir::new().expect("tempdir");
        let conn = schema::open_connection(&dir.path().join("alcedo.db")).expect("open");
        let account = sample_account("imapuser");
        upsert_account(&conn, &account).expect("upsert");
        assert!(
            get_catch_all_domains(&conn, account.id)
                .expect("get")
                .is_empty()
        );
        set_catch_all_domains(
            &conn,
            account.id,
            &[
                "@CodeInfusion.com".to_string(),
                "codeinfusion.com".to_string(),
                "alcedo.email".to_string(),
            ],
        )
        .expect("set");
        assert_eq!(
            get_catch_all_domains(&conn, account.id).expect("get"),
            vec!["codeinfusion.com", "alcedo.email"]
        );
        upsert_account(&conn, &account).expect("re-upsert keeps setting");
        assert_eq!(
            get_catch_all_domains(&conn, account.id).expect("get").len(),
            2
        );
    }

    #[test]
    fn sender_name_roundtrip() {
        let dir = TempDir::new().expect("tempdir");
        let conn = schema::open_connection(&dir.path().join("alcedo.db")).expect("open");
        let mut account = sample_account("imapuser");
        account.sender_name = "Carter".to_string();
        upsert_account(&conn, &account).expect("upsert");
        let loaded = list_accounts(&conn).expect("list");
        assert_eq!(loaded[0].sender_name, "Carter");
    }

    #[test]
    fn avatar_image_and_accent_roundtrip() {
        let dir = TempDir::new().expect("tempdir");
        let conn = schema::open_connection(&dir.path().join("alcedo.db")).expect("open");
        let mut account = sample_account("imapuser");
        account.accent_color = "#7d4cdb".to_string();
        account.avatar_image = vec![0x89, 0x50, 0x4e, 0x47];
        upsert_account(&conn, &account).expect("upsert");
        let loaded = list_accounts(&conn).expect("list");
        assert_eq!(loaded[0].accent_color, "#7d4cdb");
        assert_eq!(loaded[0].avatar_image, vec![0x89, 0x50, 0x4e, 0x47]);
    }

    #[test]
    fn smtp_envelope_policy_password_default() {
        let dir = TempDir::new().expect("tempdir");
        let conn = schema::open_connection(&dir.path().join("alcedo.db")).expect("open");
        let account = sample_account("imapuser");
        upsert_account(&conn, &account).expect("upsert");
        assert_eq!(
            get_smtp_envelope_policy(&conn, account.id).expect("get"),
            SmtpEnvelopePolicy::SameDomainIdentity
        );
    }

    #[test]
    fn smtp_envelope_policy_oauth_default() {
        let dir = TempDir::new().expect("tempdir");
        let conn = schema::open_connection(&dir.path().join("alcedo.db")).expect("open");
        let mut account = sample_account("imapuser");
        account.auth = AuthMethod::OAuth2 {
            provider: OAuthProvider::Google,
        };
        upsert_account(&conn, &account).expect("upsert");
        assert_eq!(
            get_smtp_envelope_policy(&conn, account.id).expect("get"),
            SmtpEnvelopePolicy::Mailbox
        );
    }

    #[test]
    fn smtp_envelope_policy_survives_upsert_without_clobber() {
        let dir = TempDir::new().expect("tempdir");
        let conn = schema::open_connection(&dir.path().join("alcedo.db")).expect("open");
        let mut account = sample_account("imapuser");
        upsert_account(&conn, &account).expect("upsert");
        set_smtp_envelope_policy(&conn, account.id, SmtpEnvelopePolicy::Mailbox).expect("set");
        account.display_name = "Renamed".to_string();
        upsert_account(&conn, &account).expect("upsert again");
        assert_eq!(
            get_smtp_envelope_policy(&conn, account.id).expect("get"),
            SmtpEnvelopePolicy::Mailbox
        );
        let loaded = list_accounts(&conn).expect("list");
        assert_eq!(loaded[0].display_name, "Renamed");
    }

    #[test]
    fn split_credentials_roundtrip_defaults_off() {
        let dir = TempDir::new().expect("tempdir");
        let conn = schema::open_connection(&dir.path().join("alcedo.db")).expect("open");
        let account = sample_account("imapuser");
        upsert_account(&conn, &account).expect("upsert");
        let loaded = list_accounts(&conn).expect("list");
        assert_eq!(loaded.len(), 1);
        assert!(!loaded[0].split_credentials);
        assert!(loaded[0].smtp_username.is_empty());
        assert_eq!(loaded[0].username, "imapuser");

        let mut updated = account.clone();
        updated.split_credentials = true;
        updated.smtp_username = "smtp-user".to_string();
        upsert_account(&conn, &updated).expect("upsert again");
        let loaded = list_accounts(&conn).expect("list");
        assert!(loaded[0].split_credentials);
        assert_eq!(loaded[0].smtp_username, "smtp-user");
        assert_eq!(loaded[0].username, "imapuser");
    }

    #[test]
    fn smtp_envelope_policy_unknown_value_parses_as_mailbox() {
        let dir = TempDir::new().expect("tempdir");
        let conn = schema::open_connection(&dir.path().join("alcedo.db")).expect("open");
        let account = sample_account("imapuser");
        upsert_account(&conn, &account).expect("upsert");
        conn.execute(
            "UPDATE accounts SET smtp_envelope_policy = 'nope' WHERE id = ?1",
            params![account.id.0.to_string()],
        )
        .expect("update");
        assert_eq!(
            get_smtp_envelope_policy(&conn, account.id).expect("get"),
            SmtpEnvelopePolicy::Mailbox
        );
    }
}
