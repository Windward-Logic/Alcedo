//! Folder persistence (P2).

use std::collections::HashSet;

use rusqlite::{Connection, OptionalExtension, params};

use alcedo_core::{AccountId, Folder, FolderId, SpecialUse};

use crate::StoreError;
use crate::StoreResult;

fn modseq_to_sql(modseq: Option<u64>) -> Option<i64> {
    modseq.map(|v| v as i64)
}

fn modseq_from_sql(modseq: Option<i64>) -> Option<u64> {
    modseq.map(|v| v as u64)
}

fn special_to_str(special: &SpecialUse) -> &'static str {
    match special {
        SpecialUse::Inbox => "inbox",
        SpecialUse::Sent => "sent",
        SpecialUse::Drafts => "drafts",
        SpecialUse::Trash => "trash",
        SpecialUse::Junk => "junk",
        SpecialUse::Archive => "archive",
        SpecialUse::Snoozed => "snoozed",
        SpecialUse::Outbox => "outbox",
        SpecialUse::All => "all",
        SpecialUse::Other => "other",
    }
}

fn special_from_str(s: &str) -> StoreResult<SpecialUse> {
    match s {
        "inbox" => Ok(SpecialUse::Inbox),
        "sent" => Ok(SpecialUse::Sent),
        "drafts" => Ok(SpecialUse::Drafts),
        "trash" => Ok(SpecialUse::Trash),
        "junk" => Ok(SpecialUse::Junk),
        "archive" => Ok(SpecialUse::Archive),
        "snoozed" => Ok(SpecialUse::Snoozed),
        "outbox" => Ok(SpecialUse::Outbox),
        "all" => Ok(SpecialUse::All),
        "other" => Ok(SpecialUse::Other),
        other => Err(StoreError::Other(format!(
            "unknown folder special: {other}"
        ))),
    }
}

fn wipe_folder_messages(conn: &Connection, folder_id: FolderId) -> StoreResult<()> {
    conn.execute(
        "DELETE FROM messages WHERE folder_id = ?1",
        params![folder_id.0],
    )?;
    conn.execute(
        "UPDATE folders SET uidnext = NULL, highest_modseq = NULL WHERE id = ?1",
        params![folder_id.0],
    )?;
    Ok(())
}

fn is_list_style(folder: &Folder) -> bool {
    folder.uidvalidity.is_none() && folder.uidnext.is_none() && folder.highest_modseq.is_none()
}

pub fn get_folder(conn: &Connection, id: FolderId) -> StoreResult<Option<Folder>> {
    match load_folder(conn, id.0) {
        Ok(folder) => Ok(Some(folder)),
        Err(StoreError::Sqlite(rusqlite::Error::QueryReturnedNoRows)) => Ok(None),
        Err(err) => Err(err),
    }
}

pub fn set_folder_unread(conn: &Connection, id: FolderId, unread: u32) -> StoreResult<()> {
    conn.execute(
        "UPDATE folders SET unread = ?1 WHERE id = ?2",
        params![unread, id.0],
    )?;
    Ok(())
}

fn load_folder(conn: &Connection, id: i64) -> StoreResult<Folder> {
    conn.query_row(
        "SELECT id, account_id, imap_name, delimiter, special, selectable,
                uidvalidity, uidnext, highest_modseq, unread, total
         FROM folders WHERE id = ?1",
        params![id],
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, bool>(5)?,
                row.get::<_, Option<u32>>(6)?,
                row.get::<_, Option<u32>>(7)?,
                modseq_from_sql(row.get::<_, Option<i64>>(8)?),
                row.get::<_, u32>(9)?,
                row.get::<_, u32>(10)?,
            ))
        },
    )
    .map_err(StoreError::from)
    .and_then(
        |(
            id,
            account_id_str,
            imap_name,
            delimiter,
            special_str,
            selectable,
            uidvalidity,
            uidnext,
            highest_modseq,
            unread,
            total,
        )| {
            Ok(Folder {
                id: FolderId(id),
                account_id: AccountId(uuid::Uuid::parse_str(&account_id_str)?),
                imap_name,
                delimiter,
                special: special_from_str(&special_str)?,
                selectable,
                uidvalidity,
                uidnext,
                highest_modseq,
                unread,
                total,
            })
        },
    )
}

/// How [`replace_folders`] should treat the incoming folder batch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FolderReplaceMode {
    /// Full IMAP LIST snapshot: upsert seen folders and delete locals not in the set.
    ListSnapshot,
    /// SELECT (or similar) metadata refresh for specific folders; never delete.
    SelectRefresh,
}

/// Infer replace mode from folder payload shape (used by sync worker call sites).
pub fn infer_replace_mode(folders: &[Folder]) -> FolderReplaceMode {
    if folders.len() == 1 && !is_list_style(&folders[0]) {
        FolderReplaceMode::SelectRefresh
    } else if !folders.is_empty() && folders.iter().all(is_list_style) {
        FolderReplaceMode::ListSnapshot
    } else {
        FolderReplaceMode::SelectRefresh
    }
}

fn delete_stale_folders(
    conn: &Connection,
    account_str: &str,
    incoming: &[Folder],
) -> StoreResult<()> {
    let keep: HashSet<&str> = incoming.iter().map(|f| f.imap_name.as_str()).collect();
    let mut stmt = conn.prepare("SELECT imap_name, special FROM folders WHERE account_id = ?1")?;
    let stale = stmt
        .query_map(params![account_str], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .filter_map(|row| row.ok())
        .filter(|(name, special)| !keep.contains(name.as_str()) && special != "outbox")
        .map(|(name, _)| name)
        .collect::<Vec<_>>();
    for name in stale {
        conn.execute(
            "DELETE FROM folders WHERE account_id = ?1 AND imap_name = ?2",
            params![account_str, name],
        )?;
    }
    Ok(())
}

pub fn replace_folders(
    conn: &Connection,
    account: AccountId,
    folders: &[Folder],
    mode: FolderReplaceMode,
) -> StoreResult<Vec<Folder>> {
    let account_str = account.0.to_string();
    let mut out = Vec::with_capacity(folders.len());

    if mode == FolderReplaceMode::ListSnapshot {
        delete_stale_folders(conn, &account_str, folders)?;
    }

    for folder in folders {
        // LIST inserts leave uidvalidity NULL; read as Option so SELECT→upsert doesn't fail.
        let existing_uidvalidity: Option<u32> = conn
            .query_row(
                "SELECT uidvalidity FROM folders WHERE account_id = ?1 AND imap_name = ?2",
                params![account_str, folder.imap_name],
                |row| row.get::<_, Option<u32>>(0),
            )
            .optional()?
            .flatten();

        let folder_id: Option<i64> = if folder.id.0 > 0 {
            Some(folder.id.0)
        } else {
            conn.query_row(
                "SELECT id FROM folders WHERE account_id = ?1 AND imap_name = ?2",
                params![account_str, folder.imap_name],
                |row| row.get(0),
            )
            .optional()?
        };

        if let Some(old_uid) = existing_uidvalidity
            && let Some(new_uid) = folder.uidvalidity
            && old_uid != new_uid
            && let Some(fid) = folder_id
        {
            wipe_folder_messages(conn, FolderId(fid))?;
        }

        let fid = if let Some(fid) = folder_id {
            if is_list_style(folder) {
                // LIST has no UIDVALIDITY/modseq; do not clobber SELECT-synced IMAP state.
                conn.execute(
                    "UPDATE folders SET
                        delimiter = ?1,
                        special = ?2,
                        selectable = ?3
                     WHERE id = ?4 AND account_id = ?5",
                    params![
                        folder.delimiter,
                        special_to_str(&folder.special),
                        folder.selectable,
                        fid,
                        account_str,
                    ],
                )?;
            } else {
                conn.execute(
                    "UPDATE folders SET
                        delimiter = ?1,
                        special = ?2,
                        selectable = ?3,
                        uidvalidity = ?4,
                        uidnext = ?5,
                        highest_modseq = ?6,
                        unread = ?7,
                        total = ?8
                     WHERE id = ?9 AND account_id = ?10",
                    params![
                        folder.delimiter,
                        special_to_str(&folder.special),
                        folder.selectable,
                        folder.uidvalidity,
                        folder.uidnext,
                        modseq_to_sql(folder.highest_modseq),
                        folder.unread,
                        folder.total,
                        fid,
                        account_str,
                    ],
                )?;
            }
            fid
        } else {
            conn.execute(
                "INSERT INTO folders (
                    account_id, imap_name, delimiter, special, selectable,
                    uidvalidity, uidnext, highest_modseq, unread, total
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    account_str,
                    folder.imap_name,
                    folder.delimiter,
                    special_to_str(&folder.special),
                    folder.selectable,
                    folder.uidvalidity,
                    folder.uidnext,
                    modseq_to_sql(folder.highest_modseq),
                    folder.unread,
                    folder.total,
                ],
            )?;
            conn.last_insert_rowid()
        };

        out.push(load_folder(conn, fid)?);
    }

    Ok(out)
}

pub fn list_folders(conn: &Connection, account: AccountId) -> StoreResult<Vec<Folder>> {
    let account_str = account.0.to_string();
    let mut stmt = conn.prepare(
        "SELECT id, account_id, imap_name, delimiter, special, selectable,
                uidvalidity, uidnext, highest_modseq, unread, total
         FROM folders
         WHERE account_id = ?1
         ORDER BY imap_name",
    )?;

    let rows = stmt.query_map(params![account_str], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, bool>(5)?,
            row.get::<_, Option<u32>>(6)?,
            row.get::<_, Option<u32>>(7)?,
            modseq_from_sql(row.get::<_, Option<i64>>(8)?),
            row.get::<_, u32>(9)?,
            row.get::<_, u32>(10)?,
        ))
    })?;

    let mut folders = Vec::new();
    for row in rows {
        let (
            id,
            account_id_str,
            imap_name,
            delimiter,
            special_str,
            selectable,
            uidvalidity,
            uidnext,
            highest_modseq,
            unread,
            total,
        ) = row?;
        folders.push(Folder {
            id: FolderId(id),
            account_id: AccountId(uuid::Uuid::parse_str(&account_id_str)?),
            imap_name,
            delimiter,
            special: special_from_str(&special_str)?,
            selectable,
            uidvalidity,
            uidnext,
            highest_modseq,
            unread,
            total,
        });
    }
    Ok(folders)
}

/// Rename local folder rows by IMAP path without changing ids or messages.
///
/// Pairs are applied longest `from` path first so nested mailboxes update before
/// their parents (e.g. `Projects/Work` before `Projects`).
pub fn rename_folder_tree(
    conn: &Connection,
    account: AccountId,
    pairs: &[(String, String)],
) -> StoreResult<()> {
    let account_str = account.0.to_string();
    let mut sorted = pairs.to_vec();
    sorted.sort_by(|a, b| b.0.len().cmp(&a.0.len()));

    let tx = conn.unchecked_transaction()?;
    for (from, to) in &sorted {
        tx.execute(
            "UPDATE folders SET imap_name = ?1 WHERE account_id = ?2 AND imap_name = ?3",
            params![to, account_str, from],
        )?;
    }
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn list_style_folder(account_id: AccountId, name: &str) -> Folder {
        Folder {
            id: FolderId(0),
            account_id,
            imap_name: name.to_string(),
            delimiter: "/".to_string(),
            special: SpecialUse::Inbox,
            selectable: true,
            uidvalidity: None,
            uidnext: None,
            highest_modseq: None,
            unread: 0,
            total: 0,
        }
    }

    fn select_style_folder(account_id: AccountId, name: &str) -> Folder {
        Folder {
            id: FolderId(0),
            account_id,
            imap_name: name.to_string(),
            delimiter: "/".to_string(),
            special: SpecialUse::Inbox,
            selectable: true,
            uidvalidity: Some(1),
            uidnext: Some(2),
            highest_modseq: Some(3),
            unread: 0,
            total: 0,
        }
    }

    #[test]
    fn infer_replace_mode_distinguishes_list_and_select() {
        let account = AccountId(Uuid::new_v4());
        assert_eq!(
            infer_replace_mode(&[list_style_folder(account, "INBOX")]),
            FolderReplaceMode::ListSnapshot
        );
        assert_eq!(
            infer_replace_mode(&[
                list_style_folder(account, "INBOX"),
                list_style_folder(account, "Sent"),
            ]),
            FolderReplaceMode::ListSnapshot
        );
        assert_eq!(
            infer_replace_mode(&[select_style_folder(account, "INBOX")]),
            FolderReplaceMode::SelectRefresh
        );
    }

    #[test]
    fn list_snapshot_deletes_stale_folders() {
        use alcedo_core::{Account, AuthMethod, OAuthProvider};

        let dir = tempfile::tempdir().expect("tempdir");
        let conn = rusqlite::Connection::open(dir.path().join("test.db")).expect("open db");
        crate::schema::migrate(&conn).expect("migrate");
        let account = AccountId(Uuid::new_v4());
        let account_str = account.0.to_string();
        crate::accounts::upsert_account(
            &conn,
            &Account {
                id: account,
                email: "test@example.com".to_string(),
                username: "test@example.com".to_string(),
                split_credentials: false,
                smtp_username: String::new(),
                display_name: "Test".to_string(),
                sender_name: String::new(),
                imap_host: "imap.example.com".to_string(),
                imap_port: 993,
                smtp_host: "smtp.example.com".to_string(),
                smtp_port: 587,
                smtp_starttls: true,
                auth: AuthMethod::OAuth2 {
                    provider: OAuthProvider::Google,
                },
                accent_color: "#3584e4".to_string(),
                avatar_text: "TE".to_string(),
                avatar_image: Vec::new(),
            },
        )
        .expect("upsert account");

        replace_folders(
            &conn,
            account,
            &[
                list_style_folder(account, "INBOX"),
                list_style_folder(account, "Sent"),
            ],
            FolderReplaceMode::ListSnapshot,
        )
        .expect("seed folders");

        replace_folders(
            &conn,
            account,
            &[list_style_folder(account, "INBOX")],
            FolderReplaceMode::ListSnapshot,
        )
        .expect("list snapshot");

        let names: Vec<String> = conn
            .prepare("SELECT imap_name FROM folders WHERE account_id = ?1 ORDER BY imap_name")
            .expect("prepare")
            .query_map(params![account_str], |row| row.get(0))
            .expect("query")
            .filter_map(|row| row.ok())
            .collect();
        assert_eq!(names, vec!["INBOX".to_string()]);
    }

    #[test]
    fn select_refresh_does_not_delete_siblings() {
        use alcedo_core::{Account, AuthMethod, OAuthProvider};

        let dir = tempfile::tempdir().expect("tempdir");
        let conn = rusqlite::Connection::open(dir.path().join("test.db")).expect("open db");
        crate::schema::migrate(&conn).expect("migrate");
        let account = AccountId(Uuid::new_v4());
        let account_str = account.0.to_string();
        crate::accounts::upsert_account(
            &conn,
            &Account {
                id: account,
                email: "test@example.com".to_string(),
                username: "test@example.com".to_string(),
                split_credentials: false,
                smtp_username: String::new(),
                display_name: "Test".to_string(),
                sender_name: String::new(),
                imap_host: "imap.example.com".to_string(),
                imap_port: 993,
                smtp_host: "smtp.example.com".to_string(),
                smtp_port: 587,
                smtp_starttls: true,
                auth: AuthMethod::OAuth2 {
                    provider: OAuthProvider::Google,
                },
                accent_color: "#3584e4".to_string(),
                avatar_text: "TE".to_string(),
                avatar_image: Vec::new(),
            },
        )
        .expect("upsert account");

        replace_folders(
            &conn,
            account,
            &[
                list_style_folder(account, "INBOX"),
                list_style_folder(account, "Sent"),
            ],
            FolderReplaceMode::ListSnapshot,
        )
        .expect("seed folders");

        // A SELECT-driven metadata refresh can look list-style (all IMAP ids None).
        // It must not be treated as a full LIST snapshot.
        replace_folders(
            &conn,
            account,
            &[list_style_folder(account, "INBOX")],
            FolderReplaceMode::SelectRefresh,
        )
        .expect("select refresh");

        let names: Vec<String> = conn
            .prepare("SELECT imap_name FROM folders WHERE account_id = ?1 ORDER BY imap_name")
            .expect("prepare")
            .query_map(params![account_str], |row| row.get(0))
            .expect("query")
            .filter_map(|row| row.ok())
            .collect();
        assert_eq!(names, vec!["INBOX".to_string(), "Sent".to_string()]);
    }

    #[test]
    fn get_folder_roundtrip_and_missing() {
        use alcedo_core::{Account, AuthMethod, OAuthProvider};

        let dir = tempfile::tempdir().expect("tempdir");
        let conn = rusqlite::Connection::open(dir.path().join("test.db")).expect("open db");
        crate::schema::migrate(&conn).expect("migrate");
        let account = AccountId(Uuid::new_v4());
        crate::accounts::upsert_account(
            &conn,
            &Account {
                id: account,
                email: "test@example.com".to_string(),
                username: "test@example.com".to_string(),
                split_credentials: false,
                smtp_username: String::new(),
                display_name: "Test".to_string(),
                sender_name: String::new(),
                imap_host: "imap.example.com".to_string(),
                imap_port: 993,
                smtp_host: "smtp.example.com".to_string(),
                smtp_port: 587,
                smtp_starttls: true,
                auth: AuthMethod::OAuth2 {
                    provider: OAuthProvider::Google,
                },
                accent_color: "#3584e4".to_string(),
                avatar_text: "TE".to_string(),
                avatar_image: Vec::new(),
            },
        )
        .expect("upsert account");

        let stored = replace_folders(
            &conn,
            account,
            &[list_style_folder(account, "INBOX")],
            FolderReplaceMode::ListSnapshot,
        )
        .expect("seed folders");
        let inbox_id = stored[0].id;

        let loaded = get_folder(&conn, inbox_id).expect("get_folder");
        assert_eq!(loaded.as_ref().map(|f| f.imap_name.as_str()), Some("INBOX"));
        assert_eq!(loaded.map(|f| f.id), Some(inbox_id));

        assert!(
            get_folder(&conn, FolderId(999_999))
                .expect("missing folder")
                .is_none()
        );
    }

    #[test]
    fn set_folder_unread_updates_list_style_folder() {
        use alcedo_core::{Account, AuthMethod, OAuthProvider};

        let dir = tempfile::tempdir().expect("tempdir");
        let conn = rusqlite::Connection::open(dir.path().join("test.db")).expect("open db");
        crate::schema::migrate(&conn).expect("migrate");
        let account = AccountId(Uuid::new_v4());
        crate::accounts::upsert_account(
            &conn,
            &Account {
                id: account,
                email: "test@example.com".to_string(),
                username: "test@example.com".to_string(),
                split_credentials: false,
                smtp_username: String::new(),
                display_name: "Test".to_string(),
                sender_name: String::new(),
                imap_host: "imap.example.com".to_string(),
                imap_port: 993,
                smtp_host: "smtp.example.com".to_string(),
                smtp_port: 587,
                smtp_starttls: true,
                auth: AuthMethod::OAuth2 {
                    provider: OAuthProvider::Google,
                },
                accent_color: "#3584e4".to_string(),
                avatar_text: "TE".to_string(),
                avatar_image: Vec::new(),
            },
        )
        .expect("upsert account");

        let stored = replace_folders(
            &conn,
            account,
            &[list_style_folder(account, "Cursor")],
            FolderReplaceMode::ListSnapshot,
        )
        .expect("seed folders");
        let folder_id = stored[0].id;

        set_folder_unread(&conn, folder_id, 5).expect("set_folder_unread");

        let loaded = get_folder(&conn, folder_id)
            .expect("get_folder")
            .expect("folder row");
        assert_eq!(loaded.unread, 5);
        assert!(loaded.uidvalidity.is_none());
    }

    fn other_folder(account_id: AccountId, name: &str) -> Folder {
        Folder {
            id: FolderId(0),
            account_id,
            imap_name: name.to_string(),
            delimiter: "/".to_string(),
            special: SpecialUse::Other,
            selectable: true,
            uidvalidity: None,
            uidnext: None,
            highest_modseq: None,
            unread: 0,
            total: 1,
        }
    }

    #[test]
    fn rename_folder_tree_preserves_ids_and_messages() {
        use crate::messages::upsert_envelopes;
        use alcedo_core::{
            Account, Address, AuthMethod, Envelope, MessageId, OAuthProvider, ThreadId,
        };
        use chrono::Utc;

        let dir = tempfile::tempdir().expect("tempdir");
        let conn = rusqlite::Connection::open(dir.path().join("test.db")).expect("open db");
        crate::schema::migrate(&conn).expect("migrate");
        let account = AccountId(Uuid::new_v4());
        crate::accounts::upsert_account(
            &conn,
            &Account {
                id: account,
                email: "test@example.com".to_string(),
                username: "test@example.com".to_string(),
                split_credentials: false,
                smtp_username: String::new(),
                display_name: "Test".to_string(),
                sender_name: String::new(),
                imap_host: "imap.example.com".to_string(),
                imap_port: 993,
                smtp_host: "smtp.example.com".to_string(),
                smtp_port: 587,
                smtp_starttls: true,
                auth: AuthMethod::OAuth2 {
                    provider: OAuthProvider::Google,
                },
                accent_color: "#3584e4".to_string(),
                avatar_text: "TE".to_string(),
                avatar_image: Vec::new(),
            },
        )
        .expect("upsert account");

        let stored = replace_folders(
            &conn,
            account,
            &[
                other_folder(account, "Projects"),
                other_folder(account, "Projects/Work"),
            ],
            FolderReplaceMode::ListSnapshot,
        )
        .expect("seed folders");
        let projects_id = stored
            .iter()
            .find(|f| f.imap_name == "Projects")
            .expect("Projects")
            .id;
        let work_id = stored
            .iter()
            .find(|f| f.imap_name == "Projects/Work")
            .expect("Projects/Work")
            .id;

        upsert_envelopes(
            &conn,
            &[Envelope {
                id: MessageId(0),
                account_id: account,
                folder_id: work_id,
                uid: 42,
                thread_id: ThreadId(Uuid::new_v4()),
                date: Utc::now(),
                subject: "task".to_string(),
                from: vec![Address {
                    name: None,
                    email: "sender@example.com".to_string(),
                }],
                to: vec![Address {
                    name: None,
                    email: "test@example.com".to_string(),
                }],
                cc: vec![],
                reply_to: Vec::new(),
                sender: Vec::new(),
                routing: None,
                snippet: "snippet".to_string(),
                seen: false,
                flagged: false,
                answered: false,
                draft: false,
                has_attachments: false,
                deleted_at: None,
                account_accent: "#3584e4".to_string(),
                account_avatar: "TE".to_string(),
                size_bytes: 0,
            }],
            None,
        )
        .expect("upsert message");

        let message_folder_id: i64 = conn
            .query_row(
                "SELECT folder_id FROM messages WHERE folder_id = ?1",
                params![work_id.0],
                |row| row.get(0),
            )
            .expect("message folder_id");

        rename_folder_tree(
            &conn,
            account,
            &[
                ("Projects".to_string(), "Archive".to_string()),
                ("Projects/Work".to_string(), "Archive/Work".to_string()),
            ],
        )
        .expect("rename folder tree");

        let projects = get_folder(&conn, projects_id)
            .expect("get Projects")
            .expect("Projects exists");
        let work = get_folder(&conn, work_id)
            .expect("get Work")
            .expect("Work exists");
        assert_eq!(projects.id, projects_id);
        assert_eq!(work.id, work_id);
        assert_eq!(projects.imap_name, "Archive");
        assert_eq!(work.imap_name, "Archive/Work");

        let names: Vec<String> = list_folders(&conn, account)
            .expect("list folders")
            .into_iter()
            .map(|f| f.imap_name)
            .collect();
        assert_eq!(
            names,
            vec!["Archive".to_string(), "Archive/Work".to_string()]
        );

        let folder_id_after: i64 = conn
            .query_row(
                "SELECT folder_id FROM messages WHERE folder_id = ?1",
                params![work_id.0],
                |row| row.get(0),
            )
            .expect("message folder_id after rename");
        assert_eq!(folder_id_after, message_folder_id);
        assert_eq!(folder_id_after, work_id.0);
    }
}
