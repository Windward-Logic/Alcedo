//! FTS search with folder and flag filters.

use rusqlite::{Connection, types::Value};

use alcedo_core::{
    Conversation, Envelope, FolderId, FolderToken, fts_match_clause, parse_search_query,
};

use crate::StoreResult;
use crate::messages::{conversations_from_scoped, envelope_list_select, map_envelope_row};

fn build_search_where(
    query: &str,
    folder_ids: Option<&[FolderId]>,
) -> StoreResult<(String, String, Vec<Value>)> {
    let parsed = parse_search_query(query);
    if matches!(folder_ids, Some([])) && parsed.folder.is_none() {
        return Ok((String::new(), String::new(), Vec::new()));
    }

    let mut where_parts = vec!["m.deleted = 0".to_string()];
    let mut values: Vec<Value> = Vec::new();

    let fts_clause = fts_match_clause(&parsed);
    if let Some(fts) = fts_clause.as_ref() {
        where_parts.push("messages_fts MATCH ?".to_string());
        values.push(Value::from(fts.clone()));
    }

    for term in &parsed.from {
        where_parts.push("lower(m.from_json) LIKE '%' || lower(?) || '%' ESCAPE '\\'".to_string());
        values.push(Value::from(escape_like(term)));
    }
    for term in &parsed.to {
        where_parts.push("lower(m.to_json) LIKE '%' || lower(?) || '%' ESCAPE '\\'".to_string());
        values.push(Value::from(escape_like(term)));
    }
    if parsed.has_attachment {
        where_parts.push("m.list_has_attachments = 1".to_string());
    }
    match parsed.unread {
        Some(true) => where_parts.push("m.seen = 0".to_string()),
        Some(false) => where_parts.push("m.seen = 1".to_string()),
        None => {}
    }
    match parsed.starred {
        Some(true) => where_parts.push("m.flagged = 1".to_string()),
        Some(false) => where_parts.push("m.flagged = 0".to_string()),
        None => {}
    }
    if let Some(after) = &parsed.after {
        where_parts.push("m.date_utc >= ?".to_string());
        values.push(Value::from(format!("{after}T00:00:00+00:00")));
    }
    if let Some(before) = &parsed.before {
        where_parts.push("m.date_utc < ?".to_string());
        values.push(Value::from(format!("{before}T00:00:00+00:00")));
    }
    if let Some(min) = parsed
        .larger_than
        .and_then(|bytes| i64::try_from(bytes).ok())
    {
        where_parts.push("m.list_size_bytes > ?".to_string());
        values.push(Value::from(min));
    }
    if let Some(max) = parsed
        .smaller_than
        .and_then(|bytes| i64::try_from(bytes).ok())
    {
        where_parts.push("m.list_size_bytes < ?".to_string());
        values.push(Value::from(max));
    }

    match &parsed.folder {
        Some(FolderToken::All) => {}
        Some(FolderToken::Name(name)) => {
            where_parts.push(folder_sql_predicate());
            values.push(Value::from(normalize_folder_name(name)));
            values.push(Value::from(name.clone()));
            values.push(Value::from(escape_like(name)));
        }
        None => {
            if let Some(ids) = folder_ids {
                if ids.is_empty() {
                    return Ok((String::new(), String::new(), Vec::new()));
                }
                let placeholders = std::iter::repeat_n("?", ids.len())
                    .collect::<Vec<_>>()
                    .join(", ");
                where_parts.push(format!("m.folder_id IN ({placeholders})"));
                for id in ids {
                    values.push(Value::from(id.0));
                }
            } else {
                // Unread / all-mail searches omit Trash and Junk unless the
                // query names a folder (`in:trash`, `in:anywhere`).
                where_parts.push("f.special NOT IN ('trash', 'junk')".to_string());
            }
        }
    }

    let from_clause = if fts_clause.is_some() {
        "FROM messages_fts
         JOIN messages m ON m.id = messages_fts.rowid
         JOIN accounts a ON a.id = m.account_id
         JOIN folders f ON f.id = m.folder_id"
            .to_string()
    } else {
        "FROM messages m
         JOIN accounts a ON a.id = m.account_id
         JOIN folders f ON f.id = m.folder_id"
            .to_string()
    };

    Ok((from_clause, where_parts.join(" AND "), values))
}

pub fn count_unread(conn: &Connection, query: &str) -> StoreResult<u32> {
    let span = tracing::info_span!("count_unread");
    let _guard = span.enter();
    let (from_clause, where_sql, mut values) = build_search_where(query, None)?;
    if from_clause.is_empty() {
        return Ok(0);
    }
    let where_sql = format!("{where_sql} AND m.seen = 0");
    let sql = format!("SELECT COUNT(*) {from_clause} WHERE {where_sql}");
    let count: i64 = conn.query_row(&sql, rusqlite::params_from_iter(values.drain(..)), |row| {
        row.get(0)
    })?;
    tracing::debug!(unread_count = count);
    Ok(u32::try_from(count).unwrap_or(0))
}

pub fn search(
    conn: &Connection,
    query: &str,
    folder_ids: Option<&[FolderId]>,
    limit: u32,
) -> StoreResult<Vec<Envelope>> {
    let (from_clause, where_sql, mut values) = build_search_where(query, folder_ids)?;
    if from_clause.is_empty() {
        return Ok(Vec::new());
    }

    let sql = format!(
        "SELECT {}
         {from_clause}
         WHERE {where_sql}
         ORDER BY COALESCE(m.list_bump_utc, m.date_utc) DESC
         LIMIT ?",
        envelope_list_select("m", "a.accent_color", "a.avatar_text")
    );
    values.push(Value::from(limit));

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(values), map_envelope_row)?;
    let mut envelopes = Vec::new();
    for row in rows {
        envelopes.push(row?);
    }
    Ok(envelopes)
}

pub fn search_conversations(
    conn: &Connection,
    query: &str,
    folder_ids: Option<&[FolderId]>,
    limit: u32,
) -> StoreResult<Vec<Conversation>> {
    let (from_clause, where_sql, values) = build_search_where(query, folder_ids)?;
    if from_clause.is_empty() {
        return Ok(Vec::new());
    }

    let scoped = format!(
        "SELECT {}
         {from_clause}
         WHERE {where_sql}",
        envelope_list_select("m", "a.accent_color", "a.avatar_text")
    );
    conversations_from_scoped(conn, &scoped, &values, "LIMIT ?", &[Value::from(limit)])
}

fn folder_sql_predicate() -> String {
    "(
        (lower(f.special) = lower(?) AND f.special != 'other')
        OR lower(f.imap_name) = lower(?)
        OR (
            f.delimiter != ''
            AND lower(f.imap_name) LIKE '%' || f.delimiter || lower(?) ESCAPE '\\'
        )
    )"
    .to_string()
}

fn normalize_folder_name(name: &str) -> String {
    match name.to_ascii_lowercase().as_str() {
        "spam" => "junk".to_string(),
        "deleted" | "bin" => "trash".to_string(),
        other => other.to_string(),
    }
}

fn escape_like(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\\' | '%' | '_' => {
                escaped.push('\\');
                escaped.push(ch);
            }
            other => escaped.push(other),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::{set_rfc822_sizes, uids_missing_rfc822_size};
    use crate::schema;
    use rusqlite::{Connection, params};
    use uuid::Uuid;

    fn open_test_db() -> Connection {
        let conn = Connection::open_in_memory().expect("open");
        schema::apply_pragmas(&conn).expect("pragmas");
        schema::migrate(&conn).expect("migrate");
        conn
    }

    fn seed_folder(conn: &Connection) -> i64 {
        let account_id = Uuid::new_v4();
        conn.execute(
            "INSERT INTO accounts (id, email, display_name, imap_host, imap_port, smtp_host, smtp_port, smtp_starttls, auth_method, accent_color, avatar_text)
             VALUES (?1, 'a@b.com', 'A', 'imap', 993, 'smtp', 587, 0, 'password', '#000', 'A')",
            params![account_id.to_string()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO folders (account_id, imap_name, delimiter, special, selectable)
             VALUES (?1, 'INBOX', '/', 'inbox', 1)",
            params![account_id.to_string()],
        )
        .unwrap();
        conn.query_row(
            "SELECT id FROM folders WHERE account_id = ?1",
            params![account_id.to_string()],
            |row| row.get(0),
        )
        .unwrap()
    }

    fn insert_message(conn: &Connection, folder_id: i64, id: i64, uid: u32, subject: &str) {
        conn.execute(
            "INSERT INTO messages (id, account_id, folder_id, uid, thread_id, date_utc, subject, from_json, to_json, cc_json, snippet, seen, flagged, answered, draft, has_attachments)
             SELECT ?1, account_id, ?2, ?3, ?4, '2024-01-01T00:00:00Z', ?5, '[]', '[]', '[]', '', 1, 0, 0, 0, 0
             FROM folders WHERE id = ?2",
            params![id, folder_id, uid, Uuid::new_v4().to_string(), subject],
        )
        .unwrap();
    }

    #[test]
    fn larger_matches_server_size_or_saved_attachments() {
        let conn = open_test_db();
        let folder_id = seed_folder(&conn);
        insert_message(&conn, folder_id, 1, 1, "server-big");
        insert_message(&conn, folder_id, 2, 2, "attachment-big");
        insert_message(&conn, folder_id, 3, 3, "tiny");

        set_rfc822_sizes(
            &conn,
            alcedo_core::FolderId(folder_id),
            &[(1, 2_000_000), (3, 800)],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO attachments (message_id, filename, mime, sha256, size)
             VALUES (2, 'clip.bin', 'application/octet-stream', 'abc', 3000000)",
            [],
        )
        .unwrap();
        crate::messages::refresh_message_list_denorm(&conn, alcedo_core::MessageId(2)).unwrap();

        let missing =
            uids_missing_rfc822_size(&conn, alcedo_core::FolderId(folder_id), 10).unwrap();
        assert_eq!(missing, vec![2]);

        let large = search(&conn, "larger:1M", None, 10).unwrap();
        let mut subjects: Vec<_> = large.iter().map(|env| env.subject.clone()).collect();
        subjects.sort();
        assert_eq!(
            subjects,
            vec!["attachment-big".to_string(), "server-big".to_string()]
        );
        let sizes: std::collections::HashMap<String, i64> = large
            .iter()
            .map(|env| (env.subject.clone(), env.size_bytes))
            .collect();
        assert_eq!(sizes.get("server-big"), Some(&2_000_000));
        assert_eq!(sizes.get("attachment-big"), Some(&3_000_000));

        let small = search(&conn, "smaller:1K", None, 10).unwrap();
        assert_eq!(small.len(), 1);
        assert_eq!(small[0].subject, "tiny");
        assert_eq!(small[0].size_bytes, 800);
    }

    #[test]
    fn normalize_folder_aliases() {
        assert_eq!(normalize_folder_name("spam"), "junk");
        assert_eq!(normalize_folder_name("Deleted"), "trash");
        assert_eq!(normalize_folder_name("Inbox"), "inbox");
    }
}
