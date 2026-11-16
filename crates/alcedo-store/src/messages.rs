//! Message persistence (P2).

use std::collections::HashSet;

use rusqlite::{Connection, ErrorCode, OptionalExtension, params};

use alcedo_core::{
    AccountId, Address, AttachmentLists, Conversation, DEFAULT_RISKY_ATTACHMENT_EXTENSIONS,
    DEFAULT_RISKY_ATTACHMENT_MIME_TYPES, Envelope, FolderId, ListUnsubscribe, MessageId, PgpStatus,
    ReactionPerson, RoutingHeaders, ThreadId, attachment_is_downloadable, orphan_thread_id,
    sentinel_thread_id,
};

use crate::{MessageLocator, StoreResult, StoredAttachment, StoredMessage};

/// SQL boolean for a downloadable file attachment (not benign CID inline images).
///
/// Keep the predicate aligned with
/// [`alcedo_core::attachment_is_downloadable`] and
/// [`AttachmentLists::defaults`]: trim CID, treat blank CID as missing, treat
/// `image/*` CID parts as inline unless they are on the default risky lists.
///
/// If parts have been stored, the denormalized `has_attachments` column is
/// ignored so a later envelope FETCH cannot hide a paperclip. Unopened
/// messages (no `attachments` rows) still use the column.
pub(crate) fn message_display_size_expr(alias: &str) -> String {
    format!(
        "CASE WHEN COALESCE({alias}.rfc822_size, 0) > 0 THEN {alias}.rfc822_size \
              ELSE COALESCE((SELECT SUM(a.size) FROM attachments a WHERE a.message_id = {alias}.id), 0) \
         END"
    )
}

pub(crate) fn envelope_has_attachments_expr(id_col: &str, flag_col: &str) -> String {
    let risky_mimes = DEFAULT_RISKY_ATTACHMENT_MIME_TYPES
        .iter()
        .map(|mime| format!("'{}'", mime))
        .collect::<Vec<_>>()
        .join(", ");
    let risky_exts = DEFAULT_RISKY_ATTACHMENT_EXTENSIONS
        .iter()
        .map(|ext| format!("lower(att.filename) LIKE '%.{}'", ext))
        .collect::<Vec<_>>()
        .join(" OR ");
    format!(
        "(CASE WHEN EXISTS (SELECT 1 FROM attachments att WHERE att.message_id = {id_col}) THEN \
           EXISTS (SELECT 1 FROM attachments att WHERE att.message_id = {id_col} \
             AND (att.cid IS NULL OR trim(att.cid) = '' \
               OR lower(trim(att.mime)) NOT LIKE 'image/%' \
               OR lower(trim(att.mime)) IN ({risky_mimes}) \
               OR ({risky_exts}))) \
         ELSE {flag_col} END)"
    )
}

fn message_has_file_attachments(attachments: &[StoredAttachment], column: bool) -> bool {
    if attachments.is_empty() {
        column
    } else {
        attachments.iter().any(|att| {
            attachment_is_downloadable(
                &att.filename,
                &att.mime,
                att.cid.as_deref(),
                &AttachmentLists::defaults(),
            )
        })
    }
}

fn addresses_to_json(addrs: &[Address]) -> StoreResult<String> {
    Ok(serde_json::to_string(addrs)?)
}

fn addresses_from_json(json: &str) -> StoreResult<Vec<Address>> {
    Ok(serde_json::from_str(json)?)
}

pub const SENT_SCAN_MESSAGE_LIMIT: u32 = 2000;
pub const SENT_RECIPIENT_LIMIT: usize = 500;

/// Unique To/Cc addresses from synced Sent folders, most recent first.
pub fn list_sent_recipients(conn: &Connection) -> StoreResult<Vec<Address>> {
    use std::collections::HashSet;

    let mut stmt = conn.prepare(
        "SELECT m.to_json, m.cc_json, a.email
         FROM messages m
         JOIN folders f ON f.id = m.folder_id
         JOIN accounts a ON a.id = m.account_id
         WHERE f.special = 'sent' AND m.deleted = 0
         ORDER BY m.date_utc DESC
         LIMIT ?1",
    )?;
    let rows = stmt.query_map(params![SENT_SCAN_MESSAGE_LIMIT], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;

    let mut seen: HashSet<String> = HashSet::new();
    let mut result = Vec::new();

    for row in rows {
        let (to_json, cc_json, account_email) = row?;
        let to_addrs = addresses_from_json(&to_json)?;
        let cc_addrs = addresses_from_json(&cc_json)?;
        for addr in to_addrs.iter().chain(cc_addrs.iter()) {
            if addr.email.is_empty() {
                continue;
            }
            if addr.email.eq_ignore_ascii_case(&account_email) {
                continue;
            }
            let key = addr.email.to_ascii_lowercase();
            if seen.insert(key) {
                result.push(addr.clone());
                if result.len() >= SENT_RECIPIENT_LIMIT {
                    return Ok(result);
                }
            }
        }
    }

    Ok(result)
}

pub fn upsert_envelopes(
    conn: &Connection,
    envs: &[Envelope],
    body_text: Option<&str>,
) -> StoreResult<()> {
    if envs.is_empty() {
        return Ok(());
    }

    let span = tracing::info_span!("upsert_envelopes", env_count = envs.len());
    let _guard = span.enter();

    let tx = conn.unchecked_transaction()?;
    let mut select_id = tx.prepare("SELECT id FROM messages WHERE folder_id = ?1 AND uid = ?2")?;
    let mut update = tx.prepare(
        "UPDATE messages SET
            account_id = ?1,
            thread_id = ?2,
            date_utc = ?3,
            subject = ?4,
            from_json = ?5,
            to_json = ?6,
            cc_json = ?7,
            reply_to_json = CASE WHEN reply_to_json IS NULL AND ?8 = '[]' AND ?9 = '[]' THEN NULL ELSE ?8 END,
            sender_json = CASE WHEN reply_to_json IS NULL AND ?8 = '[]' AND ?9 = '[]' THEN NULL ELSE ?9 END,
            snippet = ?10,
            seen = ?11,
            flagged = ?12,
            answered = ?13,
            draft = ?14,
            has_attachments = CASE WHEN ?15 THEN 1 ELSE has_attachments END,
            body_text = COALESCE(?16, body_text),
            deleted = 0,
            routing_json = COALESCE(?18, routing_json)
         WHERE id = ?17",
    )?;
    let mut insert = tx.prepare(
        "INSERT INTO messages (
            account_id, folder_id, uid, thread_id, date_utc, subject,
            from_json, to_json, cc_json, reply_to_json, sender_json, snippet, body_text,
            seen, flagged, answered, draft, has_attachments, routing_json
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)",
    )?;
    let list_denorm_sql = list_denorm_update_sql();
    let mut refresh_list = tx.prepare(&list_denorm_sql)?;

    for env in envs {
        let account_str = env.account_id.0.to_string();
        let thread_str = env.thread_id.0.to_string();
        let date_str = env.date.to_rfc3339();
        let from_json = addresses_to_json(&env.from)?;
        let to_json = addresses_to_json(&env.to)?;
        let cc_json = addresses_to_json(&env.cc)?;
        let reply_to_json = addresses_to_json(&env.reply_to)?;
        let sender_json = addresses_to_json(&env.sender)?;
        let routing_json = routing_to_json(env.routing.as_ref())?;

        let existing_id: Option<i64> = select_id
            .query_row(params![env.folder_id.0, env.uid], |row| row.get(0))
            .optional()?;

        let message_id = if let Some(id) = existing_id {
            update.execute(params![
                account_str,
                thread_str,
                date_str,
                env.subject,
                from_json,
                to_json,
                cc_json,
                reply_to_json,
                sender_json,
                env.snippet,
                env.seen,
                env.flagged,
                env.answered,
                env.draft,
                env.has_attachments,
                body_text,
                id,
                routing_json,
            ])?;
            id
        } else {
            insert.execute(params![
                account_str,
                env.folder_id.0,
                env.uid,
                thread_str,
                date_str,
                env.subject,
                from_json,
                to_json,
                cc_json,
                reply_to_json,
                sender_json,
                env.snippet,
                body_text,
                env.seen,
                env.flagged,
                env.answered,
                env.draft,
                env.has_attachments,
                routing_json,
            ])?;
            tx.last_insert_rowid()
        };
        refresh_list.execute(params![message_id])?;
    }
    drop(refresh_list);
    drop(insert);
    drop(update);
    drop(select_id);
    tx.commit()?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn row_to_envelope(
    id: i64,
    account_id_str: &str,
    folder_id: i64,
    uid: u32,
    thread_id_str: &str,
    date_str: &str,
    subject: String,
    from_json: &str,
    to_json: &str,
    cc_json: &str,
    snippet: String,
    seen: bool,
    flagged: bool,
    answered: bool,
    draft: bool,
    has_attachments: bool,
    deleted_at: Option<chrono::DateTime<chrono::Utc>>,
    account_accent: String,
    account_avatar: String,
    size_bytes: i64,
    reply_to_json: &str,
    sender_json: &str,
    routing_json: Option<&str>,
) -> StoreResult<Envelope> {
    Ok(Envelope {
        id: MessageId(id),
        account_id: AccountId(uuid::Uuid::parse_str(account_id_str)?),
        folder_id: FolderId(folder_id),
        uid,
        thread_id: ThreadId(uuid::Uuid::parse_str(thread_id_str)?),
        date: chrono::DateTime::parse_from_rfc3339(date_str)?.with_timezone(&chrono::Utc),
        subject,
        from: addresses_from_json(from_json)?,
        to: addresses_from_json(to_json)?,
        cc: addresses_from_json(cc_json)?,
        reply_to: addresses_from_json(reply_to_json)?,
        sender: addresses_from_json(sender_json)?,
        routing: routing_from_json(routing_json),
        snippet,
        seen,
        flagged,
        answered,
        draft,
        has_attachments,
        deleted_at,
        account_accent,
        account_avatar,
        size_bytes,
    })
}

fn list_denorm_update_sql() -> String {
    format!(
        "UPDATE messages SET
            list_has_attachments = CAST({} AS INTEGER),
            list_size_bytes = {}
         WHERE id = ?1",
        envelope_has_attachments_expr("messages.id", "messages.has_attachments"),
        message_display_size_expr("messages"),
    )
}

pub(crate) fn backfill_all_list_denorm_columns(conn: &Connection) -> StoreResult<()> {
    let sql = format!(
        "UPDATE messages SET
            list_has_attachments = CAST({} AS INTEGER),
            list_size_bytes = {}",
        envelope_has_attachments_expr("messages.id", "messages.has_attachments"),
        message_display_size_expr("messages"),
    );
    conn.execute_batch(&sql)?;
    Ok(())
}

pub(crate) fn refresh_message_list_denorm(conn: &Connection, id: MessageId) -> StoreResult<()> {
    let sql = list_denorm_update_sql();
    conn.execute(&sql, params![id.0])?;
    Ok(())
}

#[cfg(test)]
pub(crate) fn refresh_message_list_denorm_many(
    conn: &Connection,
    ids: &[MessageId],
) -> StoreResult<()> {
    if ids.is_empty() {
        return Ok(());
    }
    let sql = list_denorm_update_sql();
    for id in ids {
        conn.execute(&sql, params![id.0])?;
    }
    Ok(())
}

fn list_order(alias: &str) -> String {
    format!("COALESCE({alias}.list_bump_utc, {alias}.date_utc) DESC")
}

fn unified_inbox_select() -> String {
    virtual_inbox_select("")
}

fn virtual_inbox_select(extra_where: &str) -> String {
    format!(
        "SELECT id, account_id, folder_id, uid, thread_id, date_utc,
                subject, from_json, to_json, cc_json, snippet,
                seen, flagged, answered, draft, list_has_attachments AS has_attachments,
                deleted_at, account_accent, account_avatar,
                list_size_bytes AS size_bytes, reply_to_json, sender_json, routing_json
         FROM unified_inbox
         WHERE 1=1{extra_where}
         ORDER BY {}
         LIMIT ?1 OFFSET ?2",
        list_order("unified_inbox")
    )
}

fn folder_messages_select() -> String {
    format!(
        "SELECT m.id, m.account_id, m.folder_id, m.uid, m.thread_id, m.date_utc,
                m.subject, m.from_json, m.to_json, m.cc_json, m.snippet,
                m.seen, m.flagged, m.answered, m.draft, m.list_has_attachments AS has_attachments,
                m.deleted_at, a.accent_color, a.avatar_text,
                m.list_size_bytes AS size_bytes, m.reply_to_json, m.sender_json, m.routing_json
         FROM messages m
         JOIN accounts a ON a.id = m.account_id
         WHERE m.folder_id = ?1 AND m.deleted = 0
         ORDER BY m.deleted_at DESC, {}
         LIMIT ?2 OFFSET ?3",
        list_order("m")
    )
}

pub(crate) fn map_envelope_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Envelope> {
    let deleted_at_str: Option<String> = row.get(16)?;
    let deleted_at = deleted_at_str
        .as_deref()
        .map(|s| chrono::DateTime::parse_from_rfc3339(s).map(|dt| dt.with_timezone(&chrono::Utc)))
        .transpose()
        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
    row_to_envelope(
        row.get(0)?,
        &row.get::<_, String>(1)?,
        row.get(2)?,
        row.get(3)?,
        &row.get::<_, String>(4)?,
        &row.get::<_, String>(5)?,
        row.get(6)?,
        &row.get::<_, String>(7)?,
        &row.get::<_, String>(8)?,
        &row.get::<_, String>(9)?,
        row.get(10)?,
        row.get(11)?,
        row.get(12)?,
        row.get(13)?,
        row.get(14)?,
        row.get(15)?,
        deleted_at,
        row.get(17)?,
        row.get(18)?,
        row.get(19)?,
        row.get::<_, Option<String>>(20)?.as_deref().unwrap_or("[]"),
        row.get::<_, Option<String>>(21)?.as_deref().unwrap_or("[]"),
        row.get::<_, Option<String>>(22)?.as_deref(),
    )
    .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))
}

pub fn unified_inbox(conn: &Connection, limit: u32, offset: u32) -> StoreResult<Vec<Envelope>> {
    let mut stmt = conn.prepare(&unified_inbox_select())?;
    let rows = stmt.query_map(params![limit, offset], map_envelope_row)?;
    let mut envelopes = Vec::new();
    for row in rows {
        envelopes.push(row?);
    }
    Ok(envelopes)
}

/// Unified inbox rows that also satisfy `condition`, newest first. The
/// condition is an SQL boolean expression over `unified_inbox` columns
/// written by an extension; it must never contain user input.
pub fn unified_inbox_matching(
    conn: &Connection,
    condition: &str,
    limit: u32,
    offset: u32,
) -> StoreResult<Vec<Envelope>> {
    let sql = virtual_inbox_select(&format!(" AND ({condition})"));
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![limit, offset], map_envelope_row)?;
    let mut envelopes = Vec::new();
    for row in rows {
        envelopes.push(row?);
    }
    Ok(envelopes)
}

/// Unread unified inbox rows that satisfy `condition` (see
/// [`unified_inbox_matching`]).
pub fn unified_inbox_unread_matching(conn: &Connection, condition: &str) -> StoreResult<u32> {
    let sql = format!("SELECT COUNT(*) FROM unified_inbox WHERE seen = 0 AND ({condition})");
    Ok(conn.query_row(&sql, [], |row| row.get(0))?)
}

/// Live messages in any folder that satisfy `condition`, sorted by `order_by`.
/// Both are SQL over the `messages` table aliased `m`, written by an
/// extension (for example a join against its own table); they must never
/// contain user input.
pub fn messages_matching(
    conn: &Connection,
    condition: &str,
    order_by: &str,
    limit: u32,
    offset: u32,
) -> StoreResult<Vec<Envelope>> {
    let sql = format!(
        "SELECT m.id, m.account_id, m.folder_id, m.uid, m.thread_id, m.date_utc,
                m.subject, m.from_json, m.to_json, m.cc_json, m.snippet,
                m.seen, m.flagged, m.answered, m.draft, m.list_has_attachments AS has_attachments,
                m.deleted_at, a.accent_color, a.avatar_text,
                m.list_size_bytes AS size_bytes, m.reply_to_json, m.sender_json, m.routing_json
         FROM messages m
         JOIN accounts a ON a.id = m.account_id
         WHERE m.deleted = 0 AND ({condition})
         ORDER BY {order_by}
         LIMIT ?1 OFFSET ?2"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![limit, offset], map_envelope_row)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

pub fn folder_messages(
    conn: &Connection,
    folder: FolderId,
    limit: u32,
    offset: u32,
) -> StoreResult<Vec<Envelope>> {
    let mut stmt = conn.prepare(&folder_messages_select())?;
    let rows = stmt.query_map(params![folder.0, limit, offset], map_envelope_row)?;
    let mut envelopes = Vec::new();
    for row in rows {
        envelopes.push(row?);
    }
    Ok(envelopes)
}

pub fn folder_messages_all(conn: &Connection, folder: FolderId) -> StoreResult<Vec<Envelope>> {
    let sql = "SELECT m.id, m.account_id, m.folder_id, m.uid, m.thread_id, m.date_utc,
                m.subject, m.from_json, m.to_json, m.cc_json, m.snippet,
                m.seen, m.flagged, m.answered, m.draft, m.list_has_attachments AS has_attachments,
                m.deleted_at, a.accent_color, a.avatar_text,
                m.list_size_bytes AS size_bytes, m.reply_to_json, m.sender_json, m.routing_json
         FROM messages m
         JOIN accounts a ON a.id = m.account_id
         WHERE m.folder_id = ?1 AND m.deleted = 0
         ORDER BY m.date_utc DESC";
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map(params![folder.0], map_envelope_row)?;
    let mut envelopes = Vec::new();
    for row in rows {
        envelopes.push(row?);
    }
    Ok(envelopes)
}

fn load_attachments(
    conn: &Connection,
    message_id: MessageId,
) -> StoreResult<Vec<StoredAttachment>> {
    let mut stmt = conn.prepare(
        "SELECT id, filename, mime, sha256, size, cid
         FROM attachments WHERE message_id = ?1",
    )?;
    let rows = stmt.query_map(params![message_id.0], |row| {
        Ok(StoredAttachment {
            id: row.get(0)?,
            filename: row.get(1)?,
            mime: row.get(2)?,
            sha256: row.get(3)?,
            size: row.get::<_, i64>(4)? as u64,
            cid: row.get(5)?,
        })
    })?;
    let mut attachments = Vec::new();
    for row in rows {
        attachments.push(row?);
    }
    Ok(attachments)
}

pub fn get_message(conn: &Connection, id: MessageId) -> StoreResult<Option<StoredMessage>> {
    let tx = conn.unchecked_transaction()?;
    let row = tx
        .query_row(
            "SELECT m.id, m.account_id, m.folder_id, m.uid, m.thread_id, m.date_utc,
                    m.subject, m.from_json, m.to_json, m.cc_json, m.snippet,
                    m.body_text, m.body_html, m.message_id_hdr, m.in_reply_to, m.references_hdr,
                    m.seen, m.flagged, m.answered, m.draft, m.has_attachments, m.allow_remote_images,
                    m.list_unsubscribe_json, m.pgp_status_json,
                    m.reply_to_json, m.sender_json,
                    a.accent_color, a.avatar_text
             FROM messages m
             JOIN accounts a ON a.id = m.account_id
             WHERE m.id = ?1",
            params![id.0],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, u32>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, String>(9)?,
                    row.get::<_, String>(10)?,
                    row.get::<_, Option<String>>(11)?,
                    row.get::<_, Option<String>>(12)?,
                    row.get::<_, Option<String>>(13)?,
                    row.get::<_, Option<String>>(14)?,
                    row.get::<_, Option<String>>(15)?,
                    row.get::<_, bool>(16)?,
                    row.get::<_, bool>(17)?,
                    row.get::<_, bool>(18)?,
                    row.get::<_, bool>(19)?,
                    row.get::<_, bool>(20)?,
                    row.get::<_, bool>(21)?,
                    row.get::<_, Option<String>>(22)?,
                    row.get::<_, Option<String>>(23)?,
                    row.get::<_, Option<String>>(24)?,
                    row.get::<_, Option<String>>(25)?,
                    row.get::<_, String>(26)?,
                    row.get::<_, String>(27)?,
                ))
            },
        )
        .optional()?;

    if let Some((
        mid,
        account_id_str,
        folder_id,
        uid,
        thread_id_str,
        date_str,
        subject,
        from_json,
        to_json,
        cc_json,
        snippet,
        body_text,
        body_html,
        message_id_hdr,
        in_reply_to,
        references_hdr,
        seen,
        flagged,
        answered,
        draft,
        has_attachments,
        allow_remote_images,
        list_unsubscribe_json,
        pgp_status_json,
        reply_to_json,
        sender_json,
        account_accent,
        account_avatar,
    )) = row
    {
        let attachments = load_attachments(&tx, MessageId(mid))?;
        let has_attachments = message_has_file_attachments(&attachments, has_attachments);
        let list_unsubscribe = list_unsubscribe_json
            .as_deref()
            .map(list_unsubscribe_from_json)
            .transpose()?;
        let pgp_status = pgp_status_json
            .as_deref()
            .map(serde_json::from_str::<PgpStatus>)
            .transpose()?;
        let stored = StoredMessage {
            id: MessageId(mid),
            account_id: AccountId(uuid::Uuid::parse_str(&account_id_str)?),
            folder_id: FolderId(folder_id),
            uid,
            thread_id: ThreadId(uuid::Uuid::parse_str(&thread_id_str)?),
            date: chrono::DateTime::parse_from_rfc3339(&date_str)?.with_timezone(&chrono::Utc),
            subject,
            from: addresses_from_json(&from_json)?,
            to: addresses_from_json(&to_json)?,
            cc: addresses_from_json(&cc_json)?,
            reply_to: addresses_from_json(reply_to_json.as_deref().unwrap_or("[]"))?,
            sender: addresses_from_json(sender_json.as_deref().unwrap_or("[]"))?,
            snippet,
            body_text,
            body_html,
            message_id_hdr,
            in_reply_to,
            references_hdr,
            seen,
            flagged,
            answered,
            draft,
            has_attachments,
            allow_remote_images,
            list_unsubscribe,
            pgp_status,
            account_accent,
            account_avatar,
            attachments,
        };
        tx.commit()?;
        Ok(Some(stored))
    } else {
        tx.commit()?;
        Ok(None)
    }
}

pub fn set_allow_remote_images(conn: &Connection, id: MessageId, allow: bool) -> StoreResult<()> {
    conn.execute(
        "UPDATE messages SET allow_remote_images = ?1 WHERE id = ?2",
        params![allow, id.0],
    )?;
    Ok(())
}

fn list_unsubscribe_from_json(json: &str) -> StoreResult<ListUnsubscribe> {
    Ok(serde_json::from_str(json)?)
}

pub fn save_reaction(
    conn: &Connection,
    id: MessageId,
    emoji: Option<&str>,
    target_hdr: Option<&str>,
) -> StoreResult<()> {
    conn.execute(
        "UPDATE messages SET reaction_emoji = ?1, reaction_target_hdr = ?2 WHERE id = ?3",
        params![emoji, target_hdr, id.0],
    )?;
    Ok(())
}

pub fn reaction_target_hdr(conn: &Connection, id: MessageId) -> StoreResult<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT reaction_target_hdr FROM messages WHERE id = ?1",
            params![id.0],
            |row| row.get(0),
        )
        .optional()?
        .flatten())
}

pub fn reaction_target_message(
    conn: &Connection,
    account: AccountId,
    target_hdr: &str,
) -> StoreResult<Option<(MessageId, String)>> {
    Ok(conn
        .query_row(
            "SELECT id, subject FROM messages
             WHERE account_id = ?1 AND message_id_hdr = ?2 AND deleted = 0
             LIMIT 1",
            params![account.0.to_string(), target_hdr],
            |row| Ok((MessageId(row.get(0)?), row.get(1)?)),
        )
        .optional()?)
}

pub fn reactions_for_target(
    conn: &Connection,
    account: AccountId,
    message_id_hdr: &str,
) -> StoreResult<Vec<ReactionPerson>> {
    let mut stmt = conn.prepare(
        "SELECT id, reaction_emoji, from_json, message_id_hdr
         FROM messages
         WHERE account_id = ?1 AND reaction_target_hdr = ?2
           AND reaction_emoji IS NOT NULL AND deleted = 0",
    )?;
    let rows = stmt.query_map(params![account.0.to_string(), message_id_hdr], |row| {
        let row_id: i64 = row.get(0)?;
        let emoji: String = row.get(1)?;
        let from_json: String = row.get(2)?;
        let message_id_hdr: Option<String> = row.get(3)?;
        let from = addresses_from_json(&from_json)
            .map_err(|err| rusqlite::Error::ToSqlConversionFailure(Box::new(err)))?;
        let (from_name, from_email) = from
            .first()
            .map(|addr| (addr.display_name(), addr.email.clone()))
            .unwrap_or_default();
        Ok(ReactionPerson {
            emoji,
            from_name,
            from_email,
            message_id_hdr,
            row_id,
        })
    })?;
    let mut people = Vec::new();
    for row in rows {
        people.push(row?);
    }
    Ok(people)
}

pub fn save_list_unsubscribe(
    conn: &Connection,
    id: MessageId,
    value: Option<&ListUnsubscribe>,
) -> StoreResult<()> {
    let json = match value {
        Some(value) => Some(serde_json::to_string(value)?),
        None => None,
    };
    conn.execute(
        "UPDATE messages SET list_unsubscribe_json = ?1 WHERE id = ?2",
        params![json, id.0],
    )?;
    Ok(())
}

pub fn save_message_body(
    conn: &Connection,
    id: MessageId,
    body_text: Option<&str>,
    body_html: Option<&str>,
    pgp_status: Option<&PgpStatus>,
) -> StoreResult<()> {
    if body_text.is_none() && body_html.is_none() && pgp_status.is_none() {
        return Ok(());
    }

    let pgp_json = match pgp_status {
        Some(status) => Some(serde_json::to_string(status)?),
        None => None,
    };

    conn.execute(
        "UPDATE messages SET
            body_text = COALESCE(?1, body_text),
            body_html = COALESCE(?2, body_html),
            pgp_status_json = COALESCE(?3, pgp_status_json)
         WHERE id = ?4",
        params![body_text, body_html, pgp_json, id.0],
    )?;
    Ok(())
}

pub fn purge_missing_uids(
    conn: &Connection,
    folder: FolderId,
    live_uids: &[u32],
) -> StoreResult<Vec<MessageId>> {
    let uid_json = serde_json::to_string(live_uids)?;
    let mut stmt = conn.prepare(
        "SELECT id FROM messages
         WHERE folder_id = ?1 AND deleted = 0
           AND uid NOT IN (SELECT CAST(value AS INTEGER) FROM json_each(?2))",
    )?;
    let rows = stmt.query_map(params![folder.0, uid_json], |row| row.get::<_, i64>(0))?;
    let mut removed = Vec::new();
    for row in rows {
        removed.push(MessageId(row?));
    }
    if !removed.is_empty() {
        conn.execute(
            "UPDATE messages SET deleted = 1
             WHERE folder_id = ?1 AND deleted = 0
               AND uid NOT IN (SELECT CAST(value AS INTEGER) FROM json_each(?2))",
            params![folder.0, uid_json],
        )?;
    }

    // A no-op UID MOVE still reports OK. Local rows must reappear if the UID
    // is still in this mailbox.
    conn.execute(
        "UPDATE messages SET deleted = 0
         WHERE folder_id = ?1 AND deleted = 1
           AND uid IN (SELECT CAST(value AS INTEGER) FROM json_each(?2))",
        params![folder.0, uid_json],
    )?;
    Ok(removed)
}

pub fn mark_deleted_by_uids(
    conn: &Connection,
    folder: FolderId,
    uids: &[u32],
) -> StoreResult<usize> {
    if uids.is_empty() {
        return Ok(0);
    }
    let uid_json = serde_json::to_string(uids)?;
    let count = conn.execute(
        "UPDATE messages SET deleted = 1
         WHERE folder_id = ?1 AND deleted = 0
           AND uid IN (SELECT CAST(value AS INTEGER) FROM json_each(?2))",
        params![folder.0, uid_json],
    )?;
    Ok(count)
}

/// Apply flags the server reported to existing live rows of `folder`.
///
/// Returns the ids whose flags actually changed. Unknown UIDs are skipped, and
/// rows are only written when a flag differs, so a no-op poll writes nothing.
pub fn apply_server_flags(
    conn: &Connection,
    folder: FolderId,
    flags: &[alcedo_core::ServerFlags],
) -> StoreResult<Vec<MessageId>> {
    if flags.is_empty() {
        return Ok(Vec::new());
    }
    let tx = conn.unchecked_transaction()?;
    let mut changed = Vec::new();
    {
        let mut update = tx.prepare(
            "UPDATE messages SET seen = ?3, flagged = ?4, answered = ?5, draft = ?6
             WHERE folder_id = ?1 AND uid = ?2 AND deleted = 0
               AND (seen != ?3 OR flagged != ?4 OR answered != ?5 OR draft != ?6)
             RETURNING id",
        )?;
        for item in flags {
            let id = update
                .query_row(
                    params![
                        folder.0,
                        item.uid,
                        item.seen,
                        item.flagged,
                        item.answered,
                        item.draft
                    ],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?;
            if let Some(id) = id {
                changed.push(MessageId(id));
            }
        }
    }
    tx.commit()?;
    Ok(changed)
}

pub fn mark_deleted(conn: &Connection, id: MessageId) -> StoreResult<()> {
    conn.execute(
        "UPDATE messages SET deleted = 1 WHERE id = ?1",
        params![id.0],
    )?;
    Ok(())
}

pub fn set_rfc822_sizes(
    conn: &Connection,
    folder: FolderId,
    rows: &[(u32, i64)],
) -> StoreResult<()> {
    if rows.is_empty() {
        return Ok(());
    }
    let tx = conn.unchecked_transaction()?;
    let list_denorm_sql = list_denorm_update_sql();
    let mut update_size = tx.prepare(
        "UPDATE messages SET rfc822_size = ?1
         WHERE folder_id = ?2 AND uid = ?3 AND IFNULL(rfc822_size, -1) != ?1",
    )?;
    let mut select_id = tx.prepare("SELECT id FROM messages WHERE folder_id = ?1 AND uid = ?2")?;
    let mut refresh_list = tx.prepare(&list_denorm_sql)?;
    for (uid, size) in rows {
        let changed = update_size.execute(params![size, folder.0, uid])?;
        if changed > 0
            && let Some(id) = select_id
                .query_row(params![folder.0, uid], |row| row.get::<_, i64>(0))
                .optional()?
        {
            refresh_list.execute(params![id])?;
        }
    }
    drop(refresh_list);
    drop(select_id);
    drop(update_size);
    tx.commit()?;
    Ok(())
}

pub fn uids_missing_rfc822_size(
    conn: &Connection,
    folder: FolderId,
    limit: u32,
) -> StoreResult<Vec<u32>> {
    let mut stmt = conn.prepare(
        "SELECT uid FROM messages
         WHERE folder_id = ?1 AND deleted = 0 AND rfc822_size IS NULL
         ORDER BY uid
         LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![folder.0, limit], |row| row.get(0))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

pub fn uids_missing_reply_headers(
    conn: &Connection,
    folder: FolderId,
    limit: u32,
) -> StoreResult<Vec<u32>> {
    let mut stmt = conn.prepare(
        "SELECT uid FROM messages
         WHERE folder_id = ?1 AND deleted = 0 AND reply_to_json IS NULL
         ORDER BY uid DESC
         LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![folder.0, limit], |row| row.get(0))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

pub fn set_reply_headers(
    conn: &Connection,
    folder: FolderId,
    rows: &[(u32, String, String)],
) -> StoreResult<()> {
    if rows.is_empty() {
        return Ok(());
    }
    let tx = conn.unchecked_transaction()?;
    for (uid, reply_to_json, sender_json) in rows {
        tx.execute(
            "UPDATE messages
             SET reply_to_json = ?1, sender_json = ?2
             WHERE folder_id = ?3 AND uid = ?4 AND reply_to_json IS NULL",
            params![reply_to_json, sender_json, folder.0, uid],
        )?;
    }
    tx.commit()?;
    Ok(())
}

/// UIDs whose routing headers were never fetched, newest first.
pub fn uids_missing_routing_headers(
    conn: &Connection,
    folder: FolderId,
    limit: u32,
) -> StoreResult<Vec<u32>> {
    let mut stmt = conn.prepare(
        "SELECT uid FROM messages
         WHERE folder_id = ?1 AND deleted = 0 AND routing_json IS NULL
         ORDER BY uid DESC
         LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![folder.0, limit], |row| row.get(0))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// Fill routing headers for rows that have none yet. An empty
/// [`RoutingHeaders`] is stored too, so the UID is not fetched again.
pub fn set_routing_headers(
    conn: &Connection,
    folder: FolderId,
    rows: &[(u32, RoutingHeaders)],
) -> StoreResult<()> {
    if rows.is_empty() {
        return Ok(());
    }
    let tx = conn.unchecked_transaction()?;
    for (uid, routing) in rows {
        tx.execute(
            "UPDATE messages
             SET routing_json = ?1
             WHERE folder_id = ?2 AND uid = ?3 AND routing_json IS NULL",
            params![serde_json::to_string(routing)?, folder.0, uid],
        )?;
    }
    tx.commit()?;
    Ok(())
}

fn routing_to_json(routing: Option<&RoutingHeaders>) -> StoreResult<Option<String>> {
    Ok(routing.map(serde_json::to_string).transpose()?)
}

/// Unreadable JSON reads as "not fetched", so the backfill can repair it.
fn routing_from_json(json: Option<&str>) -> Option<RoutingHeaders> {
    json.and_then(|json| serde_json::from_str(json).ok())
}

pub fn set_message_id_hdrs(
    conn: &Connection,
    folder: FolderId,
    rows: &[(u32, String)],
) -> StoreResult<()> {
    let extended: Vec<(u32, String, Option<String>, Option<String>)> = rows
        .iter()
        .map(|(uid, hdr)| (*uid, hdr.clone(), None, None))
        .collect();
    set_threading_hdrs(conn, folder, &extended)
}

pub fn set_threading_hdrs(
    conn: &Connection,
    folder: FolderId,
    rows: &[(u32, String, Option<String>, Option<String>)],
) -> StoreResult<()> {
    if rows.is_empty() {
        return Ok(());
    }
    let tx = conn.unchecked_transaction()?;
    for (uid, message_id, in_reply_to, references) in rows {
        let irt = in_reply_to
            .as_deref()
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let refs = references
            .as_deref()
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        tx.execute(
            "UPDATE messages SET message_id_hdr = ?1, in_reply_to = ?2, references_hdr = ?3
             WHERE folder_id = ?4 AND uid = ?5",
            params![message_id, irt, refs, folder.0, uid],
        )?;
    }
    tx.commit()?;
    Ok(())
}

/// Backfill rows that shared the legacy `<no-message-id>` thread hash.
pub fn split_sentinel_thread_ids(conn: &Connection) -> StoreResult<()> {
    let sentinel = sentinel_thread_id().0.to_string();
    let mut stmt = conn.prepare("SELECT folder_id, uid FROM messages WHERE thread_id = ?1")?;
    let rows: Vec<(i64, u32)> = stmt
        .query_map(params![sentinel], |row| Ok((row.get(0)?, row.get(1)?)))?
        .filter_map(|r| r.ok())
        .collect();
    if rows.is_empty() {
        return Ok(());
    }
    let tx = conn.unchecked_transaction()?;
    for (folder_id, uid) in rows {
        let tid = orphan_thread_id(FolderId(folder_id), uid).0.to_string();
        tx.execute(
            "UPDATE messages SET thread_id = ?1 WHERE folder_id = ?2 AND uid = ?3",
            params![tid, folder_id, uid],
        )?;
    }
    tx.commit()?;
    Ok(())
}

fn is_unique_constraint(err: &rusqlite::Error) -> bool {
    matches!(
        err,
        rusqlite::Error::SqliteFailure(f, _) if f.code == ErrorCode::ConstraintViolation
    )
}

pub fn rehome_message(
    conn: &Connection,
    id: MessageId,
    folder: FolderId,
    uid: u32,
    deleted_at: Option<chrono::DateTime<chrono::Utc>>,
) -> StoreResult<bool> {
    let deleted_at_str = deleted_at.map(|dt| dt.to_rfc3339());
    let write = |conn: &Connection| {
        conn.execute(
            "UPDATE messages SET folder_id = ?1, uid = ?2, deleted = 0, deleted_at = ?3
             WHERE id = ?4",
            params![folder.0, uid, deleted_at_str, id.0],
        )
    };
    match write(conn) {
        Ok(_) => Ok(true),
        Err(err) if is_unique_constraint(&err) => {
            conn.execute(
                "DELETE FROM messages WHERE folder_id = ?1 AND uid = ?2 AND deleted = 1 AND id != ?3",
                params![folder.0, uid, id.0],
            )?;
            match write(conn) {
                Ok(_) => Ok(true),
                Err(err) if is_unique_constraint(&err) => {
                    conn.execute(
                        "UPDATE messages SET deleted = 1 WHERE id = ?1",
                        params![id.0],
                    )?;
                    Ok(false)
                }
                Err(err) => Err(err.into()),
            }
        }
        Err(err) => Err(err.into()),
    }
}

pub fn messages_by_uids(
    conn: &Connection,
    folder: FolderId,
    uids: &[u32],
) -> StoreResult<Vec<(MessageId, u32, Option<String>)>> {
    if uids.is_empty() {
        return Ok(Vec::new());
    }
    let uid_json = serde_json::to_string(uids)?;
    let mut stmt = conn.prepare(
        "SELECT id, uid, message_id_hdr FROM messages
         WHERE folder_id = ?1 AND deleted = 0
           AND uid IN (SELECT CAST(value AS INTEGER) FROM json_each(?2))",
    )?;
    let rows = stmt.query_map(params![folder.0, uid_json], |row| {
        Ok((
            MessageId(row.get::<_, i64>(0)?),
            row.get::<_, u32>(1)?,
            row.get::<_, Option<String>>(2)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// Stored `RFC822.SIZE` of a message, when the list sync recorded one.
pub fn message_rfc822_size(conn: &Connection, id: MessageId) -> StoreResult<Option<u32>> {
    let size: Option<Option<i64>> = conn
        .query_row(
            "SELECT rfc822_size FROM messages WHERE id = ?1",
            params![id.0],
            |row| row.get(0),
        )
        .optional()?;
    Ok(size
        .flatten()
        .filter(|size| *size > 0)
        .and_then(|size| u32::try_from(size).ok()))
}

pub fn message_locators(conn: &Connection, ids: &[MessageId]) -> StoreResult<Vec<MessageLocator>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let id_json = serde_json::to_string(&ids.iter().map(|id| id.0).collect::<Vec<_>>())?;
    let mut stmt = conn.prepare(
        "SELECT id, account_id, folder_id, uid, message_id_hdr
         FROM messages
         WHERE deleted = 0
           AND id IN (SELECT CAST(value AS INTEGER) FROM json_each(?1))",
    )?;
    let rows = stmt.query_map(params![id_json], |row| {
        let account_id_str: String = row.get(1)?;
        Ok(MessageLocator {
            id: MessageId(row.get::<_, i64>(0)?),
            account_id: AccountId(
                uuid::Uuid::parse_str(&account_id_str)
                    .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?,
            ),
            folder_id: FolderId(row.get(2)?),
            uid: row.get(3)?,
            message_id_hdr: row.get(4)?,
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

pub fn live_message_id_hdrs(
    conn: &Connection,
    folder: FolderId,
) -> StoreResult<Vec<(u32, String)>> {
    let mut stmt = conn.prepare(
        "SELECT uid, message_id_hdr FROM messages
         WHERE folder_id = ?1 AND deleted = 0
           AND message_id_hdr IS NOT NULL AND TRIM(message_id_hdr) != ''",
    )?;
    let rows = stmt.query_map(params![folder.0], |row| {
        Ok((row.get::<_, u32>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// True when another live row looks like an IMAP/Gmail copy of this message.
///
/// Gmail `UID COPY` only adds a label, so deleting the dest with `UID MOVE` to
/// Trash would also remove the source. Callers should expunge this mailbox
/// only when a sibling copy still exists.
pub fn has_live_copy_elsewhere(conn: &Connection, id: MessageId) -> StoreResult<bool> {
    let exists: i64 = conn.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM messages other
            JOIN messages mine ON mine.id = ?1
            WHERE other.account_id = mine.account_id
              AND other.deleted = 0
              AND other.id != mine.id
              AND other.folder_id != mine.folder_id
              AND other.date_utc = mine.date_utc
              AND other.subject = mine.subject
              AND other.from_json = mine.from_json
         )",
        params![id.0],
        |row| row.get(0),
    )?;
    Ok(exists != 0)
}

pub fn live_sibling_ids(conn: &Connection, id: MessageId) -> StoreResult<Vec<MessageId>> {
    let mut stmt = conn.prepare(
        "SELECT other.id FROM messages other
         JOIN messages mine ON mine.id = ?1
         WHERE other.account_id = mine.account_id
           AND other.deleted = 0
           AND other.id != mine.id
           AND other.folder_id != mine.folder_id
           AND other.date_utc = mine.date_utc
           AND other.subject = mine.subject
           AND other.from_json = mine.from_json",
    )?;
    let rows = stmt.query_map(params![id.0], |row| row.get::<_, i64>(0))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(MessageId(row?));
    }
    Ok(out)
}

pub fn set_flags(
    conn: &Connection,
    id: MessageId,
    seen: Option<bool>,
    flagged: Option<bool>,
) -> StoreResult<()> {
    if seen.is_none() && flagged.is_none() {
        return Ok(());
    }

    if let Some(seen_val) = seen {
        conn.execute(
            "UPDATE messages SET seen = ?1 WHERE id = ?2",
            params![seen_val, id.0],
        )?;
    }
    if let Some(flagged_val) = flagged {
        conn.execute(
            "UPDATE messages SET flagged = ?1 WHERE id = ?2",
            params![flagged_val, id.0],
        )?;
    }
    Ok(())
}

pub fn set_flags_many(
    conn: &Connection,
    ids: &[MessageId],
    seen: Option<bool>,
    flagged: Option<bool>,
) -> StoreResult<()> {
    if ids.is_empty() || (seen.is_none() && flagged.is_none()) {
        return Ok(());
    }
    let id_json = serde_json::to_string(&ids.iter().map(|id| id.0).collect::<Vec<_>>())?;
    match (seen, flagged) {
        (Some(seen_val), Some(flagged_val)) => {
            conn.execute(
                "UPDATE messages SET seen = ?1, flagged = ?2
                 WHERE deleted = 0
                   AND id IN (SELECT CAST(value AS INTEGER) FROM json_each(?3))",
                params![seen_val, flagged_val, id_json],
            )?;
        }
        (Some(seen_val), None) => {
            conn.execute(
                "UPDATE messages SET seen = ?1
                 WHERE deleted = 0
                   AND id IN (SELECT CAST(value AS INTEGER) FROM json_each(?2))",
                params![seen_val, id_json],
            )?;
        }
        (None, Some(flagged_val)) => {
            conn.execute(
                "UPDATE messages SET flagged = ?1
                 WHERE deleted = 0
                   AND id IN (SELECT CAST(value AS INTEGER) FROM json_each(?2))",
                params![flagged_val, id_json],
            )?;
        }
        (None, None) => {}
    }
    Ok(())
}

pub fn mark_folder_seen(conn: &Connection, folder_id: FolderId) -> StoreResult<()> {
    conn.execute(
        "UPDATE messages SET seen = 1 WHERE folder_id = ?1 AND deleted = 0 AND seen = 0",
        params![folder_id.0],
    )?;
    Ok(())
}

pub fn mark_folder_deleted(conn: &Connection, folder_id: FolderId) -> StoreResult<()> {
    conn.execute(
        "UPDATE messages SET deleted = 1 WHERE folder_id = ?1 AND deleted = 0",
        params![folder_id.0],
    )?;
    Ok(())
}

pub fn folder_unread_count(conn: &Connection, folder_id: FolderId) -> StoreResult<u32> {
    let sql = "SELECT COUNT(*) FROM messages m
         WHERE m.folder_id = ?1 AND m.deleted = 0 AND m.seen = 0";
    Ok(conn.query_row(sql, params![folder_id.0], |row| row.get(0))?)
}

pub fn folder_live_count(conn: &Connection, folder_id: FolderId) -> StoreResult<u32> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM messages
         WHERE folder_id = ?1 AND deleted = 0",
        params![folder_id.0],
        |row| row.get(0),
    )?)
}

/// Newest `window` live rows in `folder` that still have no cached body.
pub fn messages_missing_body(
    conn: &Connection,
    folder_id: FolderId,
    window: u32,
) -> StoreResult<Vec<MessageId>> {
    let mut stmt = conn.prepare(
        "SELECT id FROM (
             SELECT id, draft, body_text, body_html, date_utc FROM messages
             WHERE folder_id = ?1 AND deleted = 0
             ORDER BY date_utc DESC
             LIMIT ?2
         )
         WHERE draft = 0 AND body_text IS NULL AND body_html IS NULL
         ORDER BY date_utc DESC",
    )?;
    let rows = stmt.query_map(params![folder_id.0, window], |row| {
        Ok(MessageId(row.get::<_, i64>(0)?))
    })?;
    let mut ids = Vec::new();
    for row in rows {
        ids.push(row?);
    }
    Ok(ids)
}

pub fn unread_counts(conn: &Connection) -> StoreResult<Vec<(AccountId, u32, u32)>> {
    let sql = "SELECT a.id,
                COALESCE(SUM(CASE WHEN f.special = 'inbox' AND m.seen = 0 THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN m.seen = 0 THEN 1 ELSE 0 END), 0)
         FROM accounts a
         LEFT JOIN messages m ON m.account_id = a.id AND m.deleted = 0
         LEFT JOIN folders f ON f.id = m.folder_id
         GROUP BY a.id
         ORDER BY a.email";
    let mut stmt = conn.prepare(sql)?;

    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, u32>(1)?,
            row.get::<_, u32>(2)?,
        ))
    })?;

    let mut counts = Vec::new();
    for row in rows {
        let (id_str, inbox, total) = row?;
        counts.push((AccountId(uuid::Uuid::parse_str(&id_str)?), inbox, total));
    }
    Ok(counts)
}

struct ConversationRep {
    envelope: Envelope,
    message_count: u32,
    unread_count: u32,
    any_flagged: bool,
}

pub(crate) fn envelope_list_select(alias: &str, accent: &str, avatar: &str) -> String {
    format!(
        "{alias}.id, {alias}.account_id, {alias}.folder_id, {alias}.uid, {alias}.thread_id, \
         {alias}.date_utc, {alias}.subject, {alias}.from_json, {alias}.to_json, {alias}.cc_json, \
         {alias}.snippet, {alias}.seen, {alias}.flagged, {alias}.answered, {alias}.draft, \
         {alias}.list_has_attachments AS has_attachments, {alias}.deleted_at, \
         {accent} AS account_accent, {avatar} AS account_avatar, \
         {alias}.list_size_bytes AS size_bytes, {alias}.reply_to_json, {alias}.sender_json, {alias}.routing_json"
    )
}

/// Re-select envelope columns from a scoped CTE that already computed `size_bytes`.
#[cfg(test)]
pub(crate) fn envelope_list_reselect(alias: &str, accent: &str, avatar: &str) -> String {
    format!(
        "{alias}.id, {alias}.account_id, {alias}.folder_id, {alias}.uid, {alias}.thread_id, \
         {alias}.date_utc, {alias}.subject, {alias}.from_json, {alias}.to_json, {alias}.cc_json, \
         {alias}.snippet, {alias}.seen, {alias}.flagged, {alias}.answered, {alias}.draft, \
         {alias}.has_attachments AS has_attachments, {alias}.deleted_at, \
         {accent} AS account_accent, {avatar} AS account_avatar, {alias}.size_bytes AS size_bytes, \
         {alias}.reply_to_json, {alias}.sender_json, {alias}.routing_json"
    )
}

fn unified_inbox_scoped_sql() -> String {
    format!(
        "SELECT {}
         FROM unified_inbox",
        envelope_list_select("unified_inbox", "account_accent", "account_avatar")
    )
}

fn folder_scoped_sql() -> String {
    format!(
        "SELECT {}
         FROM messages m
         JOIN accounts a ON a.id = m.account_id
         WHERE m.folder_id = ?1 AND m.deleted = 0",
        envelope_list_select("m", "a.accent_color", "a.avatar_text"),
    )
}

#[cfg(test)]
fn conversation_representatives_sql(scoped_from: &str, limit_clause: &str) -> String {
    format!(
        "WITH scoped AS ({scoped_from}),
         ranked AS (
           SELECT scoped.*,
             ROW_NUMBER() OVER (PARTITION BY thread_id ORDER BY date_utc DESC, id DESC) AS rn,
             COUNT(*) OVER (PARTITION BY thread_id) AS thread_count,
             SUM(CASE WHEN seen = 0 THEN 1 ELSE 0 END) OVER (PARTITION BY thread_id) AS unread_count,
             MAX(flagged) OVER (PARTITION BY thread_id) AS any_flagged
           FROM scoped
         )
         SELECT id, account_id, folder_id, uid, thread_id, date_utc, subject,
                from_json, to_json, cc_json, snippet, seen, flagged, answered, draft,
                has_attachments, deleted_at, account_accent, account_avatar,
                size_bytes, reply_to_json, sender_json, routing_json, thread_count, unread_count, any_flagged
         FROM ranked
         WHERE rn = 1
         ORDER BY date_utc DESC
         {limit_clause}"
    )
}

#[cfg(test)]
fn map_conversation_rep_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ConversationRep> {
    let envelope = map_envelope_row(row)?;
    Ok(ConversationRep {
        envelope,
        message_count: row.get(23)?,
        unread_count: row.get(24)?,
        any_flagged: row.get(25)?,
    })
}

#[cfg(test)]
fn load_scoped_members(
    conn: &Connection,
    scoped_sql: &str,
    scope_values: &[rusqlite::types::Value],
    thread_ids: Option<&[String]>,
) -> StoreResult<std::collections::HashMap<String, Vec<Envelope>>> {
    use std::collections::HashMap;
    let (filter, extra): (String, Vec<rusqlite::types::Value>) = match thread_ids {
        Some([]) => return Ok(HashMap::new()),
        Some(ids) => (
            format!(
                "WHERE scoped.thread_id IN ({})",
                std::iter::repeat_n("?", ids.len())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            ids.iter()
                .map(|tid| rusqlite::types::Value::Text(tid.clone()))
                .collect(),
        ),
        None => (String::new(), Vec::new()),
    };
    let sql = format!(
        "WITH scoped AS ({scoped_sql})
         SELECT {}
         FROM scoped
         {filter}
         ORDER BY scoped.date_utc ASC, scoped.id ASC",
        envelope_list_reselect("scoped", "scoped.account_accent", "scoped.account_avatar")
    );
    let mut values = scope_values.to_vec();
    values.extend(extra);
    let mut stmt = conn.prepare(&sql)?;
    let mut by_thread: HashMap<String, Vec<Envelope>> = HashMap::new();
    let rows = stmt.query_map(rusqlite::params_from_iter(values), map_envelope_row)?;
    for row in rows {
        let envelope = row?;
        let tid = envelope.thread_id.0.to_string();
        by_thread.entry(tid).or_default().push(envelope);
    }
    Ok(by_thread)
}

/// One statement so a `query_only` reader still evaluates `scoped_sql` once.
///
/// `AS MATERIALIZED` keeps SQLite from running the inbox scan again for the
/// representative window and the member rows. Placeholder order is the scoped
/// query's binds, then the limit binds, matching today's callers (`?` / `?1`).
fn conversation_scope_once_sql(scoped_sql: &str, limit_clause: &str) -> String {
    format!(
        "WITH scoped AS MATERIALIZED ({scoped_sql}),
         ranked AS MATERIALIZED (
           SELECT scoped.*,
             ROW_NUMBER() OVER (PARTITION BY thread_id ORDER BY date_utc DESC, id DESC) AS rn,
             COUNT(*) OVER (PARTITION BY thread_id) AS thread_count,
             SUM(CASE WHEN seen = 0 THEN 1 ELSE 0 END) OVER (PARTITION BY thread_id) AS unread_count,
             MAX(flagged) OVER (PARTITION BY thread_id) AS any_flagged
           FROM scoped
         ),
         kept AS (
           SELECT thread_id FROM ranked
           WHERE rn = 1
           ORDER BY date_utc DESC, id DESC
           {limit_clause}
         )
         SELECT id, account_id, folder_id, uid, thread_id, date_utc, subject,
                from_json, to_json, cc_json, snippet, seen, flagged, answered, draft,
                has_attachments, deleted_at, account_accent, account_avatar,
                size_bytes, reply_to_json, sender_json, routing_json,
                thread_count, unread_count, any_flagged, rn
         FROM ranked
         WHERE thread_id IN (SELECT thread_id FROM kept)
         ORDER BY date_utc ASC, id ASC"
    )
}

struct ScopedThreadMember {
    envelope: Envelope,
    message_count: u32,
    unread_count: u32,
    any_flagged: bool,
    rn: i64,
}

fn map_scoped_thread_member(row: &rusqlite::Row<'_>) -> rusqlite::Result<ScopedThreadMember> {
    let envelope = map_envelope_row(row)?;
    Ok(ScopedThreadMember {
        envelope,
        message_count: row.get(23)?,
        unread_count: row.get(24)?,
        any_flagged: row.get(25)?,
        rn: row.get(26)?,
    })
}

#[cfg(test)]
fn conversations_from_scoped_double_cte(
    conn: &Connection,
    scoped_sql: &str,
    scope_values: &[rusqlite::types::Value],
    limit_clause: &str,
    limit_values: &[rusqlite::types::Value],
) -> StoreResult<Vec<Conversation>> {
    let tx = conn.unchecked_transaction()?;
    let sql = conversation_representatives_sql(scoped_sql, limit_clause);
    let mut values = scope_values.to_vec();
    values.extend_from_slice(limit_values);
    let reps: Vec<ConversationRep> = {
        let mut stmt = tx.prepare(&sql)?;
        stmt.query_map(rusqlite::params_from_iter(values), map_conversation_rep_row)?
            .filter_map(|r| r.ok())
            .collect()
    };
    if reps.is_empty() {
        let out = reps_to_conversations(reps, std::collections::HashMap::new());
        tx.commit()?;
        return Ok(out);
    }
    let members = if limit_clause.is_empty() {
        let rep_threads: std::collections::HashSet<String> = reps
            .iter()
            .map(|r| r.envelope.thread_id.0.to_string())
            .collect();
        let mut all = load_scoped_members(&tx, scoped_sql, scope_values, None)?;
        all.retain(|tid, _| rep_threads.contains(tid));
        all
    } else {
        let thread_ids: Vec<String> = reps
            .iter()
            .map(|r| r.envelope.thread_id.0.to_string())
            .collect();
        load_scoped_members(&tx, scoped_sql, scope_values, Some(&thread_ids))?
    };
    let out = reps_to_conversations(reps, members);
    tx.commit()?;
    Ok(out)
}

pub(crate) fn conversations_from_scoped(
    conn: &Connection,
    scoped_sql: &str,
    scope_values: &[rusqlite::types::Value],
    limit_clause: &str,
    limit_values: &[rusqlite::types::Value],
) -> StoreResult<Vec<Conversation>> {
    let span = tracing::info_span!(
        "conversations_from_scoped",
        limit_clause = limit_clause,
        scope_values = scope_values.len()
    );
    let _guard = span.enter();

    let sql = conversation_scope_once_sql(scoped_sql, limit_clause);
    let mut values = scope_values.to_vec();
    values.extend_from_slice(limit_values);
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(values), map_scoped_thread_member)?;
    let mut members_by_thread: std::collections::HashMap<String, Vec<Envelope>> =
        std::collections::HashMap::new();
    let mut reps = Vec::new();
    for row in rows {
        let loaded = row?;
        let tid = loaded.envelope.thread_id.0.to_string();
        if loaded.rn == 1 {
            reps.push(ConversationRep {
                envelope: loaded.envelope.clone(),
                message_count: loaded.message_count,
                unread_count: loaded.unread_count,
                any_flagged: loaded.any_flagged,
            });
        }
        members_by_thread
            .entry(tid)
            .or_default()
            .push(loaded.envelope);
    }
    // Representative query ordered by date descending. Equal dates keep the
    // higher message id so the page order is stable.
    reps.sort_by(|a, b| {
        b.envelope
            .date
            .cmp(&a.envelope.date)
            .then(b.envelope.id.0.cmp(&a.envelope.id.0))
    });
    tracing::debug!(thread_reps = reps.len());
    Ok(reps_to_conversations(reps, members_by_thread))
}

#[cfg(test)]
pub(crate) fn conversations_from_scoped_legacy(
    conn: &Connection,
    scoped_sql: &str,
    scope_values: &[rusqlite::types::Value],
    limit_clause: &str,
    limit_values: &[rusqlite::types::Value],
) -> StoreResult<Vec<Conversation>> {
    conversations_from_scoped_double_cte(conn, scoped_sql, scope_values, limit_clause, limit_values)
}

fn members_to_participants(members: &[Envelope]) -> Vec<Address> {
    let mut seen = std::collections::HashSet::new();
    let mut participants = Vec::new();
    for member in members {
        for addr in &member.from {
            if seen.insert(addr.email.clone()) {
                participants.push(addr.clone());
            }
        }
    }
    participants
}

fn reps_to_conversations(
    reps: Vec<ConversationRep>,
    members_by_thread: std::collections::HashMap<String, Vec<Envelope>>,
) -> Vec<Conversation> {
    reps.into_iter()
        .map(|rep| {
            let tid_str = rep.envelope.thread_id.0.to_string();
            let members = members_by_thread.get(&tid_str).cloned().unwrap_or_default();
            let member_ids: Vec<MessageId> = members.iter().map(|m| m.id).collect();
            let member_locations: Vec<(AccountId, FolderId)> = members
                .iter()
                .map(|m| (m.account_id, m.folder_id))
                .collect();
            let any_attachments = members.iter().any(|m| m.has_attachments);
            let participants = members_to_participants(&members);
            let latest = members
                .last()
                .cloned()
                .unwrap_or_else(|| rep.envelope.clone());
            Conversation {
                thread_id: rep.envelope.thread_id,
                latest,
                members,
                message_count: rep.message_count,
                unread_count: rep.unread_count,
                any_flagged: rep.any_flagged,
                any_attachments,
                member_ids,
                member_locations,
                participants,
            }
        })
        .collect()
}

fn unified_inbox_matching_scoped_sql(condition: &str) -> String {
    format!("{} WHERE ({condition})", unified_inbox_scoped_sql())
}

/// Conversations among unified inbox rows that satisfy `condition` (see
/// [`unified_inbox_matching`]).
pub fn unified_inbox_conversations_matching(
    conn: &Connection,
    condition: &str,
    limit: u32,
    offset: u32,
) -> StoreResult<Vec<Conversation>> {
    let span = tracing::info_span!("unified_inbox_conversations_matching", limit, offset);
    let _guard = span.enter();
    conversations_from_scoped(
        conn,
        &unified_inbox_matching_scoped_sql(condition),
        &[],
        "LIMIT ?1 OFFSET ?2",
        &[
            rusqlite::types::Value::from(limit),
            rusqlite::types::Value::from(offset),
        ],
    )
}

pub fn unified_inbox_conversations(
    conn: &Connection,
    limit: u32,
    offset: u32,
) -> StoreResult<Vec<Conversation>> {
    let span = tracing::info_span!("unified_inbox_conversations", limit, offset);
    let _guard = span.enter();
    conversations_from_scoped(
        conn,
        &unified_inbox_scoped_sql(),
        &[],
        "LIMIT ?1 OFFSET ?2",
        &[
            rusqlite::types::Value::from(limit),
            rusqlite::types::Value::from(offset),
        ],
    )
}

pub fn folder_conversations(conn: &Connection, folder: FolderId) -> StoreResult<Vec<Conversation>> {
    conversations_from_scoped(
        conn,
        &folder_scoped_sql(),
        &[rusqlite::types::Value::from(folder.0)],
        "",
        &[],
    )
}

fn cross_folder_list_select(accent: &str, avatar: &str) -> String {
    envelope_list_select("m", accent, avatar)
}

/// Same-account live copies of a conversation (`thread_id`). Deleted rows are
/// hidden, matching every folder list.
pub fn thread_messages_all_folders(
    conn: &Connection,
    account_id: AccountId,
    thread_id: ThreadId,
) -> StoreResult<Vec<Envelope>> {
    let sql = format!(
        "SELECT {}
         FROM messages m
         JOIN accounts a ON a.id = m.account_id
         WHERE m.account_id = ?1 AND m.thread_id = ?2 AND m.deleted = 0
         ORDER BY m.date_utc DESC, m.id DESC",
        cross_folder_list_select("a.accent_color", "a.avatar_text"),
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(
        params![account_id.0.to_string(), thread_id.0.to_string()],
        map_envelope_row,
    )?;
    let mut mapped = Vec::new();
    for row in rows {
        mapped.push(row?);
    }
    Ok(mapped)
}

/// Address lists checked by "Find all messages". A bare `To` and
/// `Name <email>` share the same canonical address.
fn address_match_sql(column: &str) -> String {
    format!(
        "EXISTS (
           SELECT 1 FROM json_each({column}) j
           WHERE alcedo_canonical_email(json_extract(j.value, '$.email'))
                 = alcedo_canonical_email(?2)
         )"
    )
}

/// Envelopes in one account that include this address in From, To, Cc,
/// Reply-To, or Sender, newest first. Display name is ignored.
pub fn messages_with_address(
    conn: &Connection,
    account_id: AccountId,
    email: &str,
) -> StoreResult<Vec<Envelope>> {
    let address_match = [
        "m.from_json",
        "m.to_json",
        "m.cc_json",
        "m.reply_to_json",
        "m.sender_json",
    ]
    .map(address_match_sql)
    .join(" OR ");
    let sql = format!(
        "SELECT {}
         FROM messages m
         JOIN accounts a ON a.id = m.account_id
         WHERE m.account_id = ?1 AND m.deleted = 0
           AND ({address_match})
         ORDER BY m.date_utc DESC, m.id DESC",
        cross_folder_list_select("a.accent_color", "a.avatar_text"),
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![account_id.0.to_string(), email], map_envelope_row)?;
    let mut mapped = Vec::new();
    for row in rows {
        mapped.push(row?);
    }
    Ok(mapped)
}

/// Live envelopes in a thread (date ascending). Phase 2 reader stack.
pub fn thread_envelopes(
    conn: &Connection,
    thread_id: ThreadId,
    folder_ids: Option<&[FolderId]>,
    include_sent: bool,
) -> StoreResult<Vec<Envelope>> {
    let tid = thread_id.0.to_string();
    let mut where_parts = vec!["m.deleted = 0".to_string(), "m.thread_id = ?1".to_string()];
    let mut values: Vec<rusqlite::types::Value> = vec![rusqlite::types::Value::Text(tid.clone())];

    if let Some(ids) = folder_ids {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = std::iter::repeat_n("?", ids.len())
            .collect::<Vec<_>>()
            .join(", ");
        where_parts.push(format!("m.folder_id IN ({placeholders})"));
        for id in ids {
            values.push(rusqlite::types::Value::from(id.0));
        }
    } else if include_sent {
        where_parts.push("(f.special = 'inbox' OR f.special = 'sent')".to_string());
    } else {
        where_parts.push("f.special = 'inbox'".to_string());
    }

    if !include_sent && folder_ids.is_none() {
        // already restricted to inbox above
    } else if include_sent && folder_ids.is_none() {
        // inbox + sent filter already applied
    }

    let sql = format!(
        "SELECT {}, m.message_id_hdr, f.special
         FROM messages m
         JOIN accounts a ON a.id = m.account_id
         JOIN folders f ON f.id = m.folder_id
         WHERE {}
         ORDER BY CASE WHEN f.special = 'inbox' THEN 0 ELSE 1 END, m.date_utc ASC, m.id ASC",
        envelope_list_select("m", "a.accent_color", "a.avatar_text"),
        where_parts.join(" AND ")
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(values), |row| {
        let envelope = map_envelope_row(row)?;
        let message_id_hdr: Option<String> = row.get(23)?;
        Ok((envelope, message_id_hdr))
    })?;
    let mut mapped = Vec::new();
    for row in rows {
        mapped.push(row?);
    }
    Ok(dedupe_thread_envelopes(mapped))
}

fn dedupe_thread_envelopes(
    rows: impl IntoIterator<Item = (Envelope, Option<String>)>,
) -> Vec<Envelope> {
    let mut seen_ids = HashSet::new();
    let mut seen_hdrs = HashSet::new();
    let mut envelopes = Vec::new();
    for (envelope, hdr) in rows {
        if !seen_ids.insert(envelope.id) {
            continue;
        }
        if let Some(hdr) = hdr.filter(|value| !value.is_empty())
            && !seen_hdrs.insert(hdr.to_ascii_lowercase())
        {
            continue;
        }
        envelopes.push(envelope);
    }
    envelopes.sort_by(|a, b| a.date.cmp(&b.date).then_with(|| a.id.0.cmp(&b.id.0)));
    envelopes
}

pub fn set_list_bump_utc(
    conn: &Connection,
    id: MessageId,
    when: chrono::DateTime<chrono::Utc>,
) -> StoreResult<()> {
    conn.execute(
        "UPDATE messages SET list_bump_utc = ?1 WHERE id = ?2",
        params![when.to_rfc3339(), id.0],
    )?;
    Ok(())
}

pub fn set_has_attachments_true(conn: &Connection, id: MessageId) -> StoreResult<()> {
    conn.execute(
        "UPDATE messages SET has_attachments = 1 WHERE id = ?1",
        params![id.0],
    )?;
    refresh_message_list_denorm(conn, id)
}

pub fn sent_structure_scan_done(conn: &Connection, account: AccountId) -> StoreResult<bool> {
    let value: Option<i64> = conn
        .query_row(
            "SELECT sent_structure_scan FROM attachment_repair WHERE account_id = ?1",
            params![account.0.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    Ok(value.unwrap_or(0) != 0)
}

pub fn mark_sent_structure_scan_done(conn: &Connection, account: AccountId) -> StoreResult<()> {
    conn.execute(
        "INSERT INTO attachment_repair (account_id, sent_structure_scan) VALUES (?1, 1)
         ON CONFLICT(account_id) DO UPDATE SET sent_structure_scan = 1",
        params![account.0.to_string()],
    )?;
    Ok(())
}

pub fn sent_uids_missing_attachment_flag(
    conn: &Connection,
    folder: FolderId,
) -> StoreResult<Vec<(MessageId, u32)>> {
    let mut stmt = conn.prepare(
        "SELECT m.id, m.uid FROM messages m
         WHERE m.folder_id = ?1 AND m.deleted = 0 AND m.has_attachments = 0
           AND NOT EXISTS (SELECT 1 FROM attachments att WHERE att.message_id = m.id)",
    )?;
    let rows = stmt.query_map(params![folder.0], |row| {
        Ok((MessageId(row.get(0)?), row.get::<_, u32>(1)?))
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// Next UID for a row that exists only in SQLite, in a folder no server syncs.
pub fn next_local_uid(conn: &Connection, folder_id: FolderId) -> StoreResult<u32> {
    let next: i64 = conn.query_row(
        "SELECT COALESCE(MAX(uid), 0) + 1 FROM messages WHERE folder_id = ?1",
        params![folder_id.0],
        |row| row.get(0),
    )?;
    Ok(std::cmp::max(1, next as u32))
}

#[cfg(test)]
mod local_uid_tests {
    use super::*;
    use crate::schema;
    use alcedo_core::AccountId;
    use uuid::Uuid;

    fn open_test_db() -> Connection {
        let conn = Connection::open_in_memory().expect("open");
        schema::apply_pragmas(&conn).expect("pragmas");
        schema::migrate(&conn).expect("migrate");
        conn
    }

    #[test]
    fn messages_matching_filters_sorts_and_skips_deleted() {
        let conn = open_test_db();
        let account_id = Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO accounts (id, email, display_name, imap_host, imap_port, smtp_host, smtp_port, smtp_starttls, auth_method, accent_color, avatar_text)
             VALUES (?1, 'a@b.com', 'A', 'imap', 993, 'smtp', 587, 0, 'password', '#000', 'A')",
            params![account_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO folders (account_id, imap_name, delimiter, special, selectable)
             VALUES (?1, 'INBOX', '/', 'inbox', 1)",
            params![account_id],
        )
        .unwrap();
        let folder_id = conn.last_insert_rowid();
        for (id, subject, deleted) in [(1, "b", 0), (2, "a", 0), (3, "a", 1), (4, "c", 0)] {
            conn.execute(
                "INSERT INTO messages (id, account_id, folder_id, uid, thread_id, date_utc, subject, from_json, to_json, cc_json, snippet, seen, flagged, answered, draft, has_attachments, deleted)
                 VALUES (?1, ?2, ?3, ?1, ?4, '2024-01-01T00:00:00Z', ?5, '[]', '[]', '[]', '', 0, 0, 0, 0, 0, ?6)",
                params![id, account_id, folder_id, Uuid::new_v4().to_string(), subject, deleted],
            )
            .unwrap();
        }
        let found = messages_matching(&conn, "m.subject IN ('a', 'b')", "m.subject ASC", 10, 0)
            .expect("query");
        let ids: Vec<i64> = found.iter().map(|env| env.id.0).collect();
        assert_eq!(ids, vec![2, 1]);
    }

    #[test]
    fn set_has_attachments_true_is_idempotent() {
        let conn = open_test_db();
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
        let folder_id: i64 = conn
            .query_row(
                "SELECT id FROM folders WHERE account_id = ?1",
                params![account_id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        conn.execute(
            "INSERT INTO messages (id, account_id, folder_id, uid, thread_id, date_utc, subject, from_json, to_json, cc_json, snippet, seen, flagged, answered, draft, has_attachments)
             VALUES (50, ?1, ?2, 1, ?3, '2024-01-01T00:00:00Z', 't', '[]', '[]', '[]', '', 0, 0, 0, 0, 0)",
            params![
                account_id.to_string(),
                folder_id,
                Uuid::new_v4().to_string(),
            ],
        )
        .unwrap();
        set_has_attachments_true(&conn, MessageId(50)).expect("set");
        let once: i64 = conn
            .query_row(
                "SELECT has_attachments FROM messages WHERE id = 50",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(once, 1);
        set_has_attachments_true(&conn, MessageId(50)).expect("set again");
        let twice: i64 = conn
            .query_row(
                "SELECT has_attachments FROM messages WHERE id = 50",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(twice, 1);
    }

    #[test]
    fn sent_uids_missing_attachment_flag_filters_rows() {
        let conn = open_test_db();
        let account_id = Uuid::new_v4();
        conn.execute(
            "INSERT INTO accounts (id, email, display_name, imap_host, imap_port, smtp_host, smtp_port, smtp_starttls, auth_method, accent_color, avatar_text)
             VALUES (?1, 'a@b.com', 'A', 'imap', 993, 'smtp', 587, 0, 'password', '#000', 'A')",
            params![account_id.to_string()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO folders (account_id, imap_name, delimiter, special, selectable)
             VALUES (?1, 'Sent', '/', 'sent', 1)",
            params![account_id.to_string()],
        )
        .unwrap();
        let folder_id = FolderId(
            conn.query_row(
                "SELECT id FROM folders WHERE account_id = ?1",
                params![account_id.to_string()],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        );
        let thread = Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO messages (id, account_id, folder_id, uid, thread_id, date_utc, subject, from_json, to_json, cc_json, snippet, seen, flagged, answered, draft, has_attachments)
             VALUES (60, ?1, ?2, 10, ?3, '2024-01-01T00:00:00Z', 'plain', '[]', '[]', '[]', '', 0, 0, 0, 0, 0),
                    (61, ?1, ?2, 11, ?3, '2024-01-01T00:00:00Z', 'file', '[]', '[]', '[]', '', 0, 0, 0, 0, 0)",
            params![account_id.to_string(), folder_id.0, thread],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO attachments (message_id, filename, mime, sha256, size, cid)
             VALUES (61, 'notes.pdf', 'application/pdf', ?, 100, NULL)",
            params!["aa".repeat(32)],
        )
        .unwrap();
        let missing = sent_uids_missing_attachment_flag(&conn, folder_id).expect("query");
        assert_eq!(missing, vec![(MessageId(60), 10)]);
    }

    #[test]
    fn sent_structure_scan_is_per_account() {
        let conn = open_test_db();
        let account_a = Uuid::new_v4();
        let account_b = Uuid::new_v4();
        for account_id in [account_a, account_b] {
            conn.execute(
                "INSERT INTO accounts (id, email, display_name, imap_host, imap_port, smtp_host, smtp_port, smtp_starttls, auth_method, accent_color, avatar_text)
                 VALUES (?1, 'a@b.com', 'A', 'imap', 993, 'smtp', 587, 0, 'password', '#000', 'A')",
                params![account_id.to_string()],
            )
            .unwrap();
        }
        let account_a_id = AccountId(account_a);
        let account_b_id = AccountId(account_b);
        assert!(!sent_structure_scan_done(&conn, account_a_id).unwrap());
        assert!(!sent_structure_scan_done(&conn, account_b_id).unwrap());
        mark_sent_structure_scan_done(&conn, account_a_id).unwrap();
        assert!(sent_structure_scan_done(&conn, account_a_id).unwrap());
        assert!(!sent_structure_scan_done(&conn, account_b_id).unwrap());
        mark_sent_structure_scan_done(&conn, account_b_id).unwrap();
        assert!(sent_structure_scan_done(&conn, account_b_id).unwrap());
    }

    #[test]
    fn next_local_uid_increments_per_folder() {
        let conn = open_test_db();
        let account_id = Uuid::new_v4();
        conn.execute(
            "INSERT INTO accounts (id, email, display_name, imap_host, imap_port, smtp_host, smtp_port, smtp_starttls, auth_method, accent_color, avatar_text)
             VALUES (?1, 'a@b.com', 'A', 'imap', 993, 'smtp', 587, 0, 'password', '#000', 'A')",
            params![account_id.to_string()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO folders (account_id, imap_name, delimiter, special, selectable, unread, total)
             VALUES (?1, 'Local', '/', 'other', 0, 0, 0)",
            params![account_id.to_string()],
        )
        .unwrap();
        let outbox = crate::folders::get_folder(&conn, FolderId(conn.last_insert_rowid()))
            .unwrap()
            .expect("local folder");
        let uid1 = next_local_uid(&conn, outbox.id).expect("uid1");
        conn.execute(
            "INSERT INTO messages (account_id, folder_id, uid, thread_id, date_utc, subject, from_json, to_json, cc_json, snippet, seen, flagged, answered, draft, has_attachments)
             VALUES (?1, ?2, ?3, ?4, '2024-01-01T00:00:00Z', '', '[]', '[]', '[]', '', 1, 0, 0, 0, 0)",
            params![
                account_id.to_string(),
                outbox.id.0,
                uid1,
                Uuid::new_v4().to_string(),
            ],
        )
        .unwrap();
        let uid2 = next_local_uid(&conn, outbox.id).expect("uid2");
        assert_ne!(uid1, uid2);
        assert_eq!(uid2, uid1 + 1);
    }

    #[test]
    fn message_locators_returns_live_identity_fields() {
        let conn = open_test_db();
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
        let folder_id: i64 = conn
            .query_row(
                "SELECT id FROM folders WHERE account_id = ?1",
                params![account_id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        conn.execute(
            "INSERT INTO messages (id, account_id, folder_id, uid, thread_id, date_utc, subject, from_json, to_json, cc_json, snippet, seen, flagged, answered, draft, has_attachments)
             VALUES (3, ?1, ?2, 1, ?3, '2024-01-01T00:00:00Z', 'Hidden', '[]', '[]', '[]', '', 0, 0, 0, 0, 0)",
            params![account_id.to_string(), folder_id, Uuid::new_v4().to_string()],
        )
        .unwrap();
        let locators =
            message_locators(&conn, &[MessageId(3), MessageId(99999)]).expect("locators");
        assert_eq!(locators.len(), 1);
        assert_eq!(locators[0].id, MessageId(3));
        assert_eq!(locators[0].account_id, AccountId(account_id));
        assert_eq!(locators[0].folder_id, FolderId(folder_id));
        assert_eq!(locators[0].uid, 1);
        assert!(locators[0].message_id_hdr.is_none());
    }

    #[test]
    fn set_flags_many_updates_seen_and_flagged_together() {
        let conn = open_test_db();
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
        let folder_id: i64 = conn
            .query_row(
                "SELECT id FROM folders WHERE account_id = ?1",
                params![account_id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        conn.execute(
            "INSERT INTO messages (id, account_id, folder_id, uid, thread_id, date_utc, subject, from_json, to_json, cc_json, snippet, seen, flagged, answered, draft, has_attachments)
             VALUES (3, ?1, ?2, 1, ?3, '2024-01-01T00:00:00Z', 'Hidden', '[]', '[]', '[]', '', 0, 0, 0, 0, 0)",
            params![account_id.to_string(), folder_id, Uuid::new_v4().to_string()],
        )
        .unwrap();
        set_flags_many(&conn, &[MessageId(3)], Some(true), Some(true)).expect("set flags");
        let (seen, flagged): (i64, i64) = conn
            .query_row(
                "SELECT seen, flagged FROM messages WHERE id = 3",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(seen, 1);
        assert_eq!(flagged, 1);
    }

    #[test]
    fn folder_live_seen_index_exists() {
        let conn = open_test_db();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = 'idx_messages_folder_live_seen'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn thread_messages_all_folders_hides_deleted() {
        let conn = open_test_db();
        let account_id = Uuid::new_v4();
        let other_account = Uuid::new_v4();
        conn.execute(
            "INSERT INTO accounts (id, email, display_name, imap_host, imap_port, smtp_host, smtp_port, smtp_starttls, auth_method, accent_color, avatar_text)
             VALUES (?1, 'a@b.com', 'A', 'imap', 993, 'smtp', 587, 0, 'password', '#000', 'A'),
                    (?2, 'c@d.com', 'C', 'imap', 993, 'smtp', 587, 0, 'password', '#000', 'C')",
            params![account_id.to_string(), other_account.to_string()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO folders (account_id, imap_name, delimiter, special, selectable)
             VALUES (?1, 'INBOX', '/', 'inbox', 1),
                    (?1, 'Sent', '/', 'sent', 1)",
            params![account_id.to_string()],
        )
        .unwrap();
        let inbox_id: i64 = conn
            .query_row(
                "SELECT id FROM folders WHERE account_id = ?1 AND special = 'inbox'",
                params![account_id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        let sent_id: i64 = conn
            .query_row(
                "SELECT id FROM folders WHERE account_id = ?1 AND special = 'sent'",
                params![account_id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        let shared = Uuid::new_v4();
        let other_thread = Uuid::new_v4();
        conn.execute(
            "INSERT INTO messages (id, account_id, folder_id, uid, thread_id, date_utc, subject, from_json, to_json, cc_json, snippet, seen, flagged, answered, draft, has_attachments, deleted)
             VALUES (1, ?1, ?2, 1, ?3, '2024-01-03T00:00:00Z', 'new', '[]', '[]', '[]', '', 0, 0, 0, 0, 0, 0),
                    (2, ?1, ?4, 2, ?3, '2024-01-02T00:00:00Z', 'mid', '[]', '[]', '[]', '', 0, 0, 0, 0, 0, 1),
                    (3, ?1, ?2, 3, ?5, '2024-01-04T00:00:00Z', 'other', '[]', '[]', '[]', '', 0, 0, 0, 0, 0, 0)",
            params![
                account_id.to_string(),
                inbox_id,
                shared.to_string(),
                sent_id,
                other_thread.to_string(),
            ],
        )
        .unwrap();

        let rows = thread_messages_all_folders(&conn, AccountId(account_id), ThreadId(shared))
            .expect("thread_messages_all_folders");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, MessageId(1));
        let deleted_flag: i64 = conn
            .query_row("SELECT deleted FROM messages WHERE id = 2", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(deleted_flag, 1);
    }

    #[test]
    fn messages_with_address_matches_any_role_and_canonical() {
        let conn = open_test_db();
        let account_id = Uuid::new_v4();
        let other_account = Uuid::new_v4();
        conn.execute(
            "INSERT INTO accounts (id, email, display_name, imap_host, imap_port, smtp_host, smtp_port, smtp_starttls, auth_method, accent_color, avatar_text)
             VALUES (?1, 'a@b.com', 'A', 'imap', 993, 'smtp', 587, 0, 'password', '#000', 'A'),
                    (?2, 'c@d.com', 'C', 'imap', 993, 'smtp', 587, 0, 'password', '#000', 'C')",
            params![account_id.to_string(), other_account.to_string()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO folders (account_id, imap_name, delimiter, special, selectable)
             VALUES (?1, 'INBOX', '/', 'inbox', 1),
                    (?2, 'INBOX', '/', 'inbox', 1)",
            params![account_id.to_string(), other_account.to_string()],
        )
        .unwrap();
        let folder_id: i64 = conn
            .query_row(
                "SELECT id FROM folders WHERE account_id = ?1",
                params![account_id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        let other_folder: i64 = conn
            .query_row(
                "SELECT id FROM folders WHERE account_id = ?1",
                params![other_account.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        let thread = Uuid::new_v4();
        conn.execute(
            "INSERT INTO messages (id, account_id, folder_id, uid, thread_id, date_utc, subject, from_json, to_json, cc_json, reply_to_json, snippet, seen, flagged, answered, draft, has_attachments)
             VALUES (10, ?1, ?2, 1, ?3, '2024-01-02T00:00:00Z', 'second from', '[{\"email\":\"b@x.com\"},{\"email\":\"a@x.com\"}]', '[]', '[]', '[]', '', 0, 0, 0, 0, 0),
                    (11, ?1, ?2, 2, ?3, '2024-01-03T00:00:00Z', 'tagged', '[{\"email\":\"A+list@X.com\"}]', '[]', '[]', '[]', '', 0, 0, 0, 0, 0),
                    (12, ?1, ?2, 3, ?3, '2024-01-04T00:00:00Z', 'named to', '[]', '[{\"name\":\"Paul Smith\",\"email\":\"a@x.com\"}]', '[]', '[]', '', 0, 0, 0, 0, 0),
                    (13, ?4, ?5, 1, ?3, '2024-01-05T00:00:00Z', 'other acct', '[{\"email\":\"a@x.com\"}]', '[]', '[]', '[]', '', 0, 0, 0, 0, 0),
                    (14, ?1, ?2, 4, ?3, '2024-01-06T00:00:00Z', 'cc only', '[]', '[]', '[{\"email\":\"a@x.com\"}]', '[]', '', 0, 0, 0, 0, 0),
                    (15, ?1, ?2, 5, ?3, '2024-01-01T00:00:00Z', 'bare to', '[]', '[{\"email\":\"a@x.com\"}]', '[]', '[]', '', 0, 0, 0, 0, 0),
                    (16, ?1, ?2, 6, ?3, '2024-01-07T00:00:00Z', 'someone else', '[{\"email\":\"z@x.com\"}]', '[{\"email\":\"y@x.com\"}]', '[]', '[]', '', 0, 0, 0, 0, 0),
                    (17, ?1, ?2, 7, ?3, '2024-01-08T00:00:00Z', 'reply-to', '[]', '[]', '[]', '[{\"email\":\"a@x.com\"}]', '', 0, 0, 0, 0, 0)",
            params![
                account_id.to_string(),
                folder_id,
                thread.to_string(),
                other_account.to_string(),
                other_folder,
            ],
        )
        .unwrap();

        let rows = messages_with_address(&conn, AccountId(account_id), "a@x.com")
            .expect("messages_with_address");
        let ids: Vec<_> = rows.iter().map(|row| row.id).collect();
        assert_eq!(
            ids,
            vec![
                MessageId(17),
                MessageId(14),
                MessageId(12),
                MessageId(11),
                MessageId(10),
                MessageId(15),
            ]
        );
    }

    #[test]
    fn messages_with_address_hides_tombstone_after_move() {
        let conn = open_test_db();
        let account_id = Uuid::new_v4();
        conn.execute(
            "INSERT INTO accounts (id, email, display_name, imap_host, imap_port, smtp_host, smtp_port, smtp_starttls, auth_method, accent_color, avatar_text)
             VALUES (?1, 'a@b.com', 'A', 'imap', 993, 'smtp', 587, 0, 'password', '#000', 'A')",
            params![account_id.to_string()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO folders (account_id, imap_name, delimiter, special, selectable)
             VALUES (?1, 'INBOX', '/', 'inbox', 1),
                    (?1, 'Jobs', '/', 'none', 1)",
            params![account_id.to_string()],
        )
        .unwrap();
        let inbox_id: i64 = conn
            .query_row(
                "SELECT id FROM folders WHERE account_id = ?1 AND special = 'inbox'",
                params![account_id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        let jobs_id: i64 = conn
            .query_row(
                "SELECT id FROM folders WHERE account_id = ?1 AND imap_name = 'Jobs'",
                params![account_id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        let thread = Uuid::new_v4();
        conn.execute(
            "INSERT INTO messages (id, account_id, folder_id, uid, thread_id, date_utc, subject, from_json, to_json, cc_json, snippet, seen, flagged, answered, draft, has_attachments, deleted, message_id_hdr)
             VALUES (20, ?1, ?2, 1, ?3, '2024-01-02T00:00:00Z', 'moved', '[{\"email\":\"sender@x.com\"}]', '[]', '[]', '', 0, 0, 0, 0, 0, 1, '<same@msg>'),
                    (21, ?1, ?4, 2, ?3, '2024-01-02T00:00:00Z', 'moved', '[{\"email\":\"sender@x.com\"}]', '[]', '[]', '', 0, 0, 0, 0, 0, 0, '<same@msg>')",
            params![
                account_id.to_string(),
                inbox_id,
                thread.to_string(),
                jobs_id,
            ],
        )
        .unwrap();

        let rows = messages_with_address(&conn, AccountId(account_id), "sender@x.com")
            .expect("messages_with_address");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, MessageId(21));
        assert_eq!(rows[0].folder_id, FolderId(jobs_id));
    }

    #[test]
    fn upsert_update_preserves_null_reply_headers_when_empty() {
        use alcedo_core::{Address, Envelope, MessageId, ThreadId};
        use chrono::Utc;

        let conn = open_test_db();
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
        let folder_id = FolderId(
            conn.query_row(
                "SELECT id FROM folders WHERE account_id = ?1",
                params![account_id.to_string()],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        );
        let thread = Uuid::new_v4();
        conn.execute(
            "INSERT INTO messages (id, account_id, folder_id, uid, thread_id, date_utc, subject, from_json, to_json, cc_json, snippet, seen, flagged, answered, draft, has_attachments)
             VALUES (70, ?1, ?2, 5, ?3, '2024-01-01T00:00:00Z', 'hdr', '[]', '[]', '[]', '', 0, 0, 0, 0, 0)",
            params![account_id.to_string(), folder_id.0, thread.to_string()],
        )
        .unwrap();
        conn.execute(
            "UPDATE messages SET reply_to_json = NULL, sender_json = NULL WHERE id = 70",
            [],
        )
        .unwrap();

        let account = AccountId(account_id);
        let mut env = Envelope {
            id: MessageId(70),
            account_id: account,
            folder_id,
            uid: 5,
            thread_id: ThreadId(thread),
            date: Utc::now(),
            subject: "hdr".to_string(),
            from: vec![Address {
                name: None,
                email: "from@example.com".to_string(),
            }],
            to: vec![],
            cc: vec![],
            reply_to: Vec::new(),
            sender: Vec::new(),
            routing: None,
            snippet: "".to_string(),
            seen: false,
            flagged: false,
            answered: false,
            draft: false,
            has_attachments: false,
            deleted_at: None,
            account_accent: "#000".to_string(),
            account_avatar: "A".to_string(),
            size_bytes: 0,
        };
        upsert_envelopes(&conn, &[env.clone()], None).expect("upsert empty headers");
        let missing =
            uids_missing_reply_headers(&conn, folder_id, 10).expect("missing reply headers");
        assert_eq!(missing, vec![5]);

        env.reply_to = vec![Address {
            name: None,
            email: "reply@example.com".to_string(),
        }];
        upsert_envelopes(&conn, &[env], None).expect("upsert with reply-to");
        let missing_after =
            uids_missing_reply_headers(&conn, folder_id, 10).expect("missing after fill");
        assert!(missing_after.is_empty());
        let reply_json: Option<String> = conn
            .query_row(
                "SELECT reply_to_json FROM messages WHERE folder_id = ?1 AND uid = 5",
                params![folder_id.0],
                |row| row.get(0),
            )
            .unwrap();
        assert!(reply_json.is_some());
        assert_ne!(reply_json.unwrap(), "[]");
    }
    #[test]
    fn routing_headers_roundtrip_and_survive_upsert_without_them() {
        use alcedo_core::{Address, Envelope, MessageId, ThreadId};
        use chrono::Utc;

        let conn = open_test_db();
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
        let folder_id = FolderId(
            conn.query_row(
                "SELECT id FROM folders WHERE account_id = ?1",
                params![account_id.to_string()],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        );
        let thread = Uuid::new_v4();
        conn.execute(
            "INSERT INTO messages (id, account_id, folder_id, uid, thread_id, date_utc, subject, from_json, to_json, cc_json, snippet, seen, flagged, answered, draft, has_attachments)
             VALUES (70, ?1, ?2, 5, ?3, '2024-01-01T00:00:00Z', 'hdr', '[]', '[]', '[]', '', 0, 0, 0, 0, 0)",
            params![account_id.to_string(), folder_id.0, thread.to_string()],
        )
        .unwrap();
        conn.execute(
            "UPDATE messages SET reply_to_json = NULL, sender_json = NULL WHERE id = 70",
            [],
        )
        .unwrap();

        let account = AccountId(account_id);
        let env = Envelope {
            id: MessageId(70),
            account_id: account,
            folder_id,
            uid: 5,
            thread_id: ThreadId(thread),
            date: Utc::now(),
            subject: "hdr".to_string(),
            from: vec![Address {
                name: None,
                email: "from@example.com".to_string(),
            }],
            to: vec![],
            cc: vec![],
            reply_to: Vec::new(),
            sender: Vec::new(),
            routing: None,
            snippet: "".to_string(),
            seen: false,
            flagged: false,
            answered: false,
            draft: false,
            has_attachments: false,
            deleted_at: None,
            account_accent: "#000".to_string(),
            account_avatar: "A".to_string(),
            size_bytes: 0,
        };
        upsert_envelopes(&conn, std::slice::from_ref(&env), None).expect("upsert without routing");
        assert_eq!(
            uids_missing_routing_headers(&conn, folder_id, 10).unwrap(),
            vec![5]
        );
        assert!(
            folder_messages_all(&conn, folder_id).unwrap()[0]
                .routing
                .is_none()
        );

        let routing = RoutingHeaders {
            list_post: Some("dev@lists.example.org".to_string()),
            delivered_to: vec!["alias@b.com".to_string()],
            ..RoutingHeaders::default()
        };
        set_routing_headers(&conn, folder_id, &[(5, routing.clone())]).expect("set routing");
        assert!(
            uids_missing_routing_headers(&conn, folder_id, 10)
                .unwrap()
                .is_empty()
        );
        upsert_envelopes(&conn, &[env], None).expect("upsert keeps routing");
        let listed = folder_messages_all(&conn, folder_id).unwrap();
        assert_eq!(listed[0].routing.as_ref(), Some(&routing));
    }

    #[test]
    fn upsert_has_attachments_only_upgrades() {
        use alcedo_core::{Address, Envelope, MessageId, ThreadId};
        use chrono::Utc;

        let conn = open_test_db();
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
        let folder_id = FolderId(
            conn.query_row(
                "SELECT id FROM folders WHERE account_id = ?1",
                params![account_id.to_string()],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        );
        let thread = Uuid::new_v4();
        let account = AccountId(account_id);
        let mut env = Envelope {
            id: MessageId(0),
            account_id: account,
            folder_id,
            uid: 9,
            thread_id: ThreadId(thread),
            date: Utc::now(),
            subject: "att".to_string(),
            from: vec![Address {
                name: None,
                email: "from@example.com".to_string(),
            }],
            to: vec![],
            cc: vec![],
            reply_to: vec![],
            sender: vec![],
            routing: None,
            snippet: String::new(),
            seen: true,
            flagged: false,
            answered: false,
            draft: false,
            has_attachments: false,
            deleted_at: None,
            account_accent: "#000".to_string(),
            account_avatar: "A".to_string(),
            size_bytes: 0,
        };
        upsert_envelopes(&conn, &[env.clone()], None).expect("insert");
        env.has_attachments = true;
        upsert_envelopes(&conn, &[env.clone()], None).expect("upgrade");
        let flag: i64 = conn
            .query_row(
                "SELECT has_attachments FROM messages WHERE folder_id = ?1 AND uid = 9",
                params![folder_id.0],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(flag, 1);
        env.has_attachments = false;
        upsert_envelopes(&conn, &[env], None).expect("downgrade attempt");
        let flag_after: i64 = conn
            .query_row(
                "SELECT has_attachments FROM messages WHERE folder_id = ?1 AND uid = 9",
                params![folder_id.0],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(flag_after, 1);
    }
}

#[cfg(test)]
mod performance_parity_tests {
    use super::*;
    use crate::schema;
    use alcedo_core::{FolderId, MessageId};
    use uuid::Uuid;

    fn open_test_db() -> Connection {
        let conn = Connection::open_in_memory().expect("open");
        schema::apply_pragmas(&conn).expect("pragmas");
        schema::migrate(&conn).expect("migrate");
        conn
    }

    fn computed_list_row(conn: &Connection, id: MessageId) -> (i64, i64) {
        let sql = format!(
            "SELECT CAST({} AS INTEGER), {}",
            envelope_has_attachments_expr("messages.id", "messages.has_attachments"),
            message_display_size_expr("messages"),
        );
        conn.query_row(
            &format!("{sql} FROM messages WHERE id = ?1"),
            params![id.0],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap()
    }

    #[test]
    fn list_denorm_columns_match_legacy_expressions() {
        let conn = open_test_db();
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
        let folder_id: i64 = conn
            .query_row(
                "SELECT id FROM folders WHERE account_id = ?1",
                params![account_id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        let thread = Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO messages (id, account_id, folder_id, uid, thread_id, date_utc, subject, from_json, to_json, cc_json, snippet, seen, flagged, answered, draft, has_attachments, rfc822_size)
             VALUES (1, ?1, ?2, 1, ?3, '2024-01-02T00:00:00Z', 'server', '[]', '[]', '[]', '', 1, 0, 0, 0, 0, 5000),
                    (2, ?1, ?2, 2, ?3, '2024-01-01T00:00:00Z', 'parts', '[]', '[]', '[]', '', 1, 0, 0, 0, 0, NULL),
                    (3, ?1, ?2, 3, ?3, '2024-01-03T00:00:00Z', 'inline', '[]', '[]', '[]', '', 1, 0, 0, 0, 1, NULL)",
            params![account_id.to_string(), folder_id, thread],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO attachments (message_id, filename, mime, sha256, size, cid)
             VALUES (2, 'big.bin', 'application/octet-stream', ?, 9000, NULL),
                    (3, 'photo.png', 'image/png', ?, 100, '<cid>')",
            params!["b".repeat(64), "c".repeat(64)],
        )
        .unwrap();
        refresh_message_list_denorm_many(&conn, &[MessageId(1), MessageId(2), MessageId(3)])
            .unwrap();

        for id in [1, 2, 3] {
            let stored: (i64, i64) = conn
                .query_row(
                    "SELECT list_has_attachments, list_size_bytes FROM messages WHERE id = ?1",
                    params![id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            assert_eq!(stored, computed_list_row(&conn, MessageId(id)));
        }
    }

    fn conversation_fingerprint(c: &Conversation) -> (String, u32, u32, bool, i64, Vec<i64>) {
        (
            c.thread_id.0.to_string(),
            c.message_count,
            c.unread_count,
            c.any_flagged,
            c.latest.id.0,
            c.members.iter().map(|m| m.id.0).collect(),
        )
    }

    fn assert_conversations_match(new: &[Conversation], legacy: &[Conversation]) {
        let mut new_fps: Vec<_> = new.iter().map(conversation_fingerprint).collect();
        let mut legacy_fps: Vec<_> = legacy.iter().map(conversation_fingerprint).collect();
        new_fps.sort_by(|a, b| a.0.cmp(&b.0));
        legacy_fps.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(new_fps, legacy_fps);
    }

    #[test]
    fn conversations_materialized_scope_matches_legacy_cte() {
        let conn = open_test_db();
        let account_id = Uuid::new_v4();
        conn.execute(
            "INSERT INTO accounts (id, email, display_name, imap_host, imap_port, smtp_host, smtp_port, smtp_starttls, auth_method, accent_color, avatar_text)
             VALUES (?1, 'friend@example.com', 'Friend', 'imap', 993, 'smtp', 587, 0, 'password', '#000', 'F')",
            params![account_id.to_string()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO folders (account_id, imap_name, delimiter, special, selectable)
             VALUES (?1, 'INBOX', '/', 'inbox', 1)",
            params![account_id.to_string()],
        )
        .unwrap();
        let folder_id: i64 = conn
            .query_row(
                "SELECT id FROM folders WHERE account_id = ?1",
                params![account_id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        conn.execute(
            "INSERT INTO contacts (email, status, created_at, updated_at)
             VALUES ('friend@example.com', 'approved', '2024-01-01T00:00:00Z', '2024-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        let t1 = Uuid::new_v4().to_string();
        let t2 = Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO messages (id, account_id, folder_id, uid, thread_id, date_utc, subject, from_json, to_json, cc_json, snippet, seen, flagged, answered, draft, has_attachments)
             VALUES (10, ?1, ?2, 1, ?3, '2024-02-02T00:00:00Z', 't1-new', '[{\"email\":\"friend@example.com\"}]', '[]', '[]', '', 0, 1, 0, 0, 0),
                    (11, ?1, ?2, 2, ?3, '2024-02-01T00:00:00Z', 't1-old', '[{\"email\":\"friend@example.com\"}]', '[]', '[]', '', 1, 0, 0, 0, 0),
                    (20, ?1, ?2, 3, ?4, '2024-02-03T00:00:00Z', 't2', '[{\"email\":\"stranger@example.com\"}]', '[]', '[]', '', 0, 0, 0, 0, 0)",
            params![account_id.to_string(), folder_id, t1, t2],
        )
        .unwrap();
        backfill_all_list_denorm_columns(&conn).unwrap();

        let unified_new = unified_inbox_conversations(&conn, 10, 0).expect("unified new");
        let unified_legacy = conversations_from_scoped_legacy(
            &conn,
            &unified_inbox_scoped_sql(),
            &[],
            "LIMIT ?1 OFFSET ?2",
            &[
                rusqlite::types::Value::from(10),
                rusqlite::types::Value::from(0),
            ],
        )
        .expect("unified legacy");
        assert_conversations_match(&unified_new, &unified_legacy);

        let from_friend = "from_json LIKE '%friend@example.com%'";
        let matching_new =
            unified_inbox_conversations_matching(&conn, from_friend, 10, 0).expect("matching new");
        let matching_legacy = conversations_from_scoped_legacy(
            &conn,
            &unified_inbox_matching_scoped_sql(from_friend),
            &[],
            "LIMIT ?1 OFFSET ?2",
            &[
                rusqlite::types::Value::from(10),
                rusqlite::types::Value::from(0),
            ],
        )
        .expect("matching legacy");
        assert_conversations_match(&matching_new, &matching_legacy);
        assert_eq!(matching_new.len(), 1, "only the friend's thread matches");
        assert_eq!(
            unified_inbox_unread_matching(&conn, from_friend).expect("unread"),
            1
        );
        assert_eq!(
            unified_inbox_matching(&conn, from_friend, 10, 0)
                .expect("rows")
                .len(),
            2
        );

        let folder_new = folder_conversations(&conn, FolderId(folder_id)).expect("folder new");
        let folder_legacy = conversations_from_scoped_legacy(
            &conn,
            &folder_scoped_sql(),
            &[rusqlite::types::Value::from(folder_id)],
            "",
            &[],
        )
        .expect("folder legacy");
        assert_conversations_match(&folder_new, &folder_legacy);

        let unread = crate::search::count_unread(&conn, "is:unread").expect("unread");
        let expected_unread: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM messages m
                 JOIN folders f ON f.id = m.folder_id
                 WHERE m.deleted = 0 AND m.seen = 0 AND f.special NOT IN ('trash', 'junk')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(unread, u32::try_from(expected_unread).unwrap());

        conn.pragma_update(None, "query_only", "ON").unwrap();
        let readonly = unified_inbox_conversations(&conn, 10, 0).expect("readonly unified");
        assert_conversations_match(&readonly, &unified_legacy);
    }
}
