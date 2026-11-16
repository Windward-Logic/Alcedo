//! Schema migrations (P2).

use alcedo_core::private_fs::{
    create_new_private_file, create_private_dir_all, restrict_file_permissions,
};
use rusqlite::Connection;

use crate::StoreResult;
use crate::messages;

const MIGRATIONS: &[(i64, &str)] = &[
    (1, include_str!("../migrations/001_init.sql")),
    (2, include_str!("../migrations/002_contacts.sql")),
    (3, include_str!("../migrations/003_remote_images.sql")),
    (4, include_str!("../migrations/004_reserved.sql")),
    (5, include_str!("../migrations/005_reserved.sql")),
    (6, include_str!("../migrations/006_reserved.sql")),
    (7, include_str!("../migrations/007_reserved.sql")),
    (8, include_str!("../migrations/008_messages_deleted_at.sql")),
    (9, include_str!("../migrations/009_list_unsubscribe.sql")),
    (
        10,
        include_str!("../migrations/010_list_unsubscribe_repeek.sql"),
    ),
    (11, include_str!("../migrations/011_message_rules.sql")),
    (12, include_str!("../migrations/012_messages_thread_id.sql")),
    (13, include_str!("../migrations/013_list_bump.sql")),
    (14, include_str!("../migrations/014_contact_blocked.sql")),
    (15, include_str!("../migrations/015_openpgp.sql")),
    (16, include_str!("../migrations/016_reserved.sql")),
    (17, include_str!("../migrations/017_compose_identity.sql")),
    (18, include_str!("../migrations/018_account_username.sql")),
    (
        19,
        include_str!("../migrations/019_clear_generated_avatar_text.sql"),
    ),
    (
        20,
        include_str!("../migrations/020_account_avatar_image.sql"),
    ),
    (
        21,
        include_str!("../migrations/021_messages_folder_live_seen.sql"),
    ),
    (
        22,
        include_str!("../migrations/022_smtp_envelope_policy.sql"),
    ),
    (
        23,
        include_str!("../migrations/023_repair_has_attachments.sql"),
    ),
    (
        24,
        include_str!("../migrations/024_attachment_repair_per_account.sql"),
    ),
    (
        25,
        include_str!("../migrations/025_messages_rfc822_size.sql"),
    ),
    (26, include_str!("../migrations/026_message_reactions.sql")),
    (
        27,
        include_str!("../migrations/027_reactions_visible_in_lists.sql"),
    ),
    (
        28,
        include_str!("../migrations/028_account_sender_name.sql"),
    ),
    (
        29,
        include_str!("../migrations/029_split_mail_credentials.sql"),
    ),
    (30, include_str!("../migrations/030_signature_logo.sql")),
    (31, include_str!("../migrations/031_signature_inlines.sql")),
    (
        32,
        include_str!("../migrations/032_reply_sender_headers.sql"),
    ),
    (
        33,
        include_str!("../migrations/033_routing_headers_catch_all.sql"),
    ),
    (
        34,
        include_str!("../migrations/034_list_denorm_columns.sql"),
    ),
    (35, include_str!("../migrations/035_queued_sends.sql")),
    (36, include_str!("../migrations/036_reserved.sql")),
    (
        37,
        include_str!("../migrations/037_unified_inbox_core_only.sql"),
    ),
];

fn latest_known_version() -> i64 {
    MIGRATIONS.last().map_or(0, |&(version, _)| version)
}

/// Apply migrations up to and including `max_version` (use `i64::MAX` for all).
pub fn migrate_to_version(conn: &Connection, max_version: i64) -> StoreResult<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (version INTEGER PRIMARY KEY);",
    )?;
    let current: i64 = conn.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
        [],
        |row| row.get(0),
    )?;
    // A database written by a newer Alcedo has tables and columns this build
    // does not know; writing to it could corrupt it. Refuse instead.
    if current > latest_known_version() {
        return Err(crate::StoreError::Other(format!(
            "the mail database was created by a newer version of Alcedo \
             (schema {current}, this version supports up to {}); \
             update Alcedo to open it",
            latest_known_version()
        )));
    }

    for &(version, sql) in MIGRATIONS {
        if version > max_version {
            break;
        }
        if current >= version {
            continue;
        }
        if version == 14 {
            conn.pragma_update(None, "foreign_keys", "OFF")?;
        }
        let tx = conn.unchecked_transaction()?;
        if version == 1 {
            // 001_init.sql still creates schema_migrations; the table already exists.
            let sql = sql
                .strip_prefix("CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY);\n\n")
                .unwrap_or(sql);
            tx.execute_batch(sql)?;
        } else {
            tx.execute_batch(sql)?;
        }
        if version == 34 {
            messages::backfill_all_list_denorm_columns(&tx)?;
        }
        tx.execute(
            "INSERT INTO schema_migrations (version) VALUES (?1)",
            [version],
        )?;
        tx.commit()?;
        if version == 14 {
            conn.pragma_update(None, "foreign_keys", "ON")?;
        }
    }
    register_sql_functions(conn)?;
    Ok(())
}

/// Apply an extension's own migrations, tracked separately from the app's in
/// `extension_migrations` so app versions never collide with extension ones.
/// Each migration runs once, in order, inside its own transaction.
pub fn apply_extension_migrations(
    conn: &Connection,
    extension: &str,
    migrations: &[(i64, &str)],
) -> StoreResult<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS extension_migrations (
            extension TEXT NOT NULL,
            version INTEGER NOT NULL,
            PRIMARY KEY (extension, version)
        );",
    )?;
    for &(version, sql) in migrations {
        let applied: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM extension_migrations WHERE extension = ?1 AND version = ?2)",
            rusqlite::params![extension, version],
            |row| row.get(0),
        )?;
        if applied {
            continue;
        }
        let tx = conn.unchecked_transaction()?;
        tx.execute_batch(sql)?;
        tx.execute(
            "INSERT INTO extension_migrations (extension, version) VALUES (?1, ?2)",
            rusqlite::params![extension, version],
        )?;
        tx.commit()?;
        tracing::info!(extension, version, "applied extension migration");
    }
    Ok(())
}

pub fn migrate(conn: &Connection) -> StoreResult<()> {
    migrate_to_version(conn, i64::MAX)
}

pub(crate) fn register_sql_functions(conn: &Connection) -> StoreResult<()> {
    conn.create_scalar_function(
        "alcedo_canonical_email",
        1,
        rusqlite::functions::FunctionFlags::SQLITE_UTF8
            | rusqlite::functions::FunctionFlags::SQLITE_DETERMINISTIC,
        |ctx: &mut rusqlite::functions::Context<'_>| {
            let email: Option<String> = ctx.get(0)?;
            Ok(email.as_deref().and_then(alcedo_core::canonical_email))
        },
    )?;
    Ok(())
}

pub fn apply_pragmas(conn: &Connection) -> StoreResult<()> {
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.pragma_update(None, "busy_timeout", 5000)?;
    Ok(())
}

pub fn open_connection(path: &std::path::Path) -> StoreResult<Connection> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        create_private_dir_all(parent)?;
    }
    // Pre-create the database `0600`; SQLite gives its -wal/-shm files the
    // same mode as the main file.
    match create_new_private_file(path) {
        Ok(_) => {}
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(err) => return Err(err.into()),
    }
    let conn = Connection::open(path)?;
    apply_pragmas(&conn)?;
    restrict_database_files(path)?;
    migrate(&conn)?;
    messages::split_sentinel_thread_ids(&conn)?;
    Ok(conn)
}

/// Tighten an existing database (and its WAL/SHM side files) to `0600`.
fn restrict_database_files(path: &std::path::Path) -> StoreResult<()> {
    restrict_file_permissions(path)?;
    for suffix in ["-wal", "-shm"] {
        let mut side = path.as_os_str().to_owned();
        side.push(suffix);
        restrict_file_permissions(std::path::Path::new(&side))?;
    }
    Ok(())
}

pub fn open_reader(path: &std::path::Path) -> StoreResult<Connection> {
    let conn = Connection::open(path)?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.pragma_update(None, "busy_timeout", 5000)?;
    conn.pragma_update(None, "query_only", "ON")?;
    register_sql_functions(&conn)?;
    Ok(conn)
}

#[cfg(test)]
mod migration_tests {
    use super::*;

    #[test]
    fn refuses_database_from_a_newer_version() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        let newer = latest_known_version() + 1;
        conn.execute(
            "INSERT INTO schema_migrations (version) VALUES (?1)",
            [newer],
        )
        .unwrap();
        let err = migrate(&conn).unwrap_err().to_string();
        assert!(err.contains("newer version of Alcedo"), "{err}");
    }

    #[test]
    fn extension_migrations_run_once_per_extension() {
        let conn = Connection::open_in_memory().expect("open");
        let migrations = [
            (1, "CREATE TABLE demo (n INTEGER);"),
            (2, "INSERT INTO demo VALUES (7);"),
        ];
        apply_extension_migrations(&conn, "demo", &migrations).expect("first run");
        apply_extension_migrations(&conn, "demo", &migrations).expect("second run");
        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM demo", [], |row| row.get(0))
            .expect("count");
        assert_eq!(rows, 1);
        // The same version number under another extension is separate.
        apply_extension_migrations(&conn, "other", &[(1, "CREATE TABLE other (n INTEGER);")])
            .expect("other extension");
        let tables: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name IN ('demo', 'other')",
                [],
                |row| row.get(0),
            )
            .expect("tables");
        assert_eq!(tables, 2);
    }

    #[test]
    fn failed_extension_migration_is_not_recorded() {
        let conn = Connection::open_in_memory().expect("open");
        assert!(apply_extension_migrations(&conn, "demo", &[(1, "NOT SQL;")]).is_err());
        assert!(
            apply_extension_migrations(&conn, "demo", &[(1, "CREATE TABLE demo (n INTEGER);")])
                .is_ok()
        );
    }

    #[test]
    fn migrating_twice_is_a_no_op() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        migrate(&conn).unwrap();
    }
}

#[cfg(all(test, unix))]
mod permission_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn mode(path: &std::path::Path) -> u32 {
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn new_database_and_dir_are_owner_only() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("alcedo");
        let db = dir.join("mail.db");
        let _conn = open_connection(&db).unwrap();
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&db), 0o600);
        let mut wal = db.as_os_str().to_owned();
        wal.push("-wal");
        let wal = std::path::PathBuf::from(wal);
        if wal.exists() {
            assert_eq!(mode(&wal), 0o600);
        }
    }

    #[test]
    fn existing_world_readable_database_is_tightened() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("mail.db");
        drop(Connection::open(&db).unwrap());
        std::fs::set_permissions(&db, std::fs::Permissions::from_mode(0o644)).unwrap();
        let _conn = open_connection(&db).unwrap();
        assert_eq!(mode(&db), 0o600);
    }
}
