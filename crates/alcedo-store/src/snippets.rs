//! Global compose snippets.

use chrono::Utc;
use rusqlite::{Connection, params};

use alcedo_core::{Snippet, SnippetId};

use crate::StoreError;
use crate::StoreResult;
use crate::{compose_format_from_str, format_ts};

fn row_to_snippet(
    id: String,
    name: String,
    format: String,
    body: String,
    position: i32,
) -> StoreResult<Snippet> {
    Ok(Snippet {
        id: SnippetId(uuid::Uuid::parse_str(&id)?),
        name,
        format: compose_format_from_str(&format)?,
        body,
        position,
    })
}

pub fn list_snippets(conn: &Connection) -> StoreResult<Vec<Snippet>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, format, body, position
         FROM snippets
         ORDER BY position ASC, created_at ASC",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, i32>(4)?,
        ))
    })?;
    rows.map(|row| {
        let (id, name, format, body, position) = row?;
        row_to_snippet(id, name, format, body, position)
    })
    .collect()
}

pub fn upsert_snippet(conn: &Connection, snippet: &Snippet) -> StoreResult<Snippet> {
    let mut position = snippet.position;
    if position < 0 {
        let max: i32 = conn
            .query_row(
                "SELECT COALESCE(MAX(position), -1) FROM snippets",
                [],
                |row| row.get(0),
            )
            .unwrap_or(-1);
        position = max + 1;
    }
    let now = Utc::now();
    let created_exists: bool = conn
        .query_row(
            "SELECT 1 FROM snippets WHERE id = ?1",
            params![snippet.id.0.to_string()],
            |_| Ok(()),
        )
        .is_ok();
    let created_at = if created_exists {
        conn.query_row(
            "SELECT created_at FROM snippets WHERE id = ?1",
            params![snippet.id.0.to_string()],
            |row| row.get::<_, String>(0),
        )?
    } else {
        format_ts(now)
    };
    conn.execute(
        "INSERT INTO snippets (id, name, format, body, position, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(id) DO UPDATE SET
            name = excluded.name,
            format = excluded.format,
            body = excluded.body,
            position = excluded.position,
            updated_at = excluded.updated_at",
        params![
            snippet.id.0.to_string(),
            snippet.name,
            snippet.format.as_str(),
            snippet.body,
            position,
            created_at,
            format_ts(now),
        ],
    )
    .map_err(|e| {
        if let rusqlite::Error::SqliteFailure(err, _) = &e
            && err.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE
        {
            return StoreError::Other("snippet name already exists".to_string());
        }
        StoreError::from(e)
    })?;
    Ok(Snippet {
        position,
        ..snippet.clone()
    })
}

pub fn delete_snippet(conn: &Connection, id: SnippetId) -> StoreResult<()> {
    conn.execute(
        "DELETE FROM snippets WHERE id = ?1",
        params![id.0.to_string()],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use alcedo_core::ComposeFormat;
    use tempfile::TempDir;
    use uuid::Uuid;

    use crate::schema;

    #[test]
    fn snippet_unique_name() {
        let dir = TempDir::new().expect("tempdir");
        let conn = schema::open_connection(&dir.path().join("alcedo.db")).expect("open");

        let first = Snippet {
            id: SnippetId(Uuid::new_v4()),
            name: "Thanks".to_string(),
            format: ComposeFormat::Plain,
            body: "Thank you!".to_string(),
            position: 0,
        };
        upsert_snippet(&conn, &first).expect("first");

        let dup = Snippet {
            id: SnippetId(Uuid::new_v4()),
            name: "thanks".to_string(),
            format: ComposeFormat::Plain,
            body: "Other".to_string(),
            position: 1,
        };
        assert!(upsert_snippet(&conn, &dup).is_err());
    }
}
