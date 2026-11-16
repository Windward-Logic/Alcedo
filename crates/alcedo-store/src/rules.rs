//! Message rules persistence.

use std::collections::HashSet;

use chrono::{DateTime, Utc};
use rusqlite::{Connection, params};

use alcedo_core::{AccountId, MessageId, MessageRule, RuleExecution, RuleId, RuleMatchMode};

use crate::StoreError;
use crate::StoreResult;
use crate::format_ts;

fn match_mode_to_str(mode: RuleMatchMode) -> &'static str {
    match mode {
        RuleMatchMode::All => "all",
        RuleMatchMode::Any => "any",
    }
}

fn match_mode_from_str(s: &str) -> StoreResult<RuleMatchMode> {
    match s {
        "all" => Ok(RuleMatchMode::All),
        "any" => Ok(RuleMatchMode::Any),
        other => Err(StoreError::Other(format!(
            "unknown rule match mode: {other}"
        ))),
    }
}

fn execution_to_str(exec: RuleExecution) -> &'static str {
    match exec {
        RuleExecution::Client => "client",
        RuleExecution::Server => "server",
    }
}

fn execution_from_str(s: &str) -> StoreResult<RuleExecution> {
    match s {
        "client" => Ok(RuleExecution::Client),
        "server" => Ok(RuleExecution::Server),
        other => Err(StoreError::Other(format!(
            "unknown rule execution: {other}"
        ))),
    }
}

#[allow(clippy::too_many_arguments)]
fn row_to_rule(
    id: String,
    account_id: String,
    name: String,
    enabled: i64,
    position: i32,
    match_mode: String,
    stop_processing: i64,
    conditions_json: String,
    actions_json: String,
    execution: String,
    created_at: String,
    updated_at: String,
) -> StoreResult<MessageRule> {
    Ok(MessageRule {
        id: RuleId(uuid::Uuid::parse_str(&id)?),
        account_id: AccountId(uuid::Uuid::parse_str(&account_id)?),
        name,
        enabled: enabled != 0,
        position,
        match_mode: match_mode_from_str(&match_mode)?,
        stop_processing: stop_processing != 0,
        conditions: serde_json::from_str(&conditions_json)?,
        actions: serde_json::from_str(&actions_json)?,
        execution: execution_from_str(&execution)?,
        created_at: DateTime::parse_from_rfc3339(&created_at)?.with_timezone(&Utc),
        updated_at: DateTime::parse_from_rfc3339(&updated_at)?.with_timezone(&Utc),
    })
}

pub fn list_rules(conn: &Connection, account: AccountId) -> StoreResult<Vec<MessageRule>> {
    let mut stmt = conn.prepare(
        "SELECT id, account_id, name, enabled, position, match_mode, stop_processing,
                conditions_json, actions_json, execution, created_at, updated_at
         FROM message_rules
         WHERE account_id = ?1
         ORDER BY position ASC, created_at ASC",
    )?;
    let rows = stmt.query_map(params![account.0.to_string()], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, i32>(4)?,
            row.get::<_, String>(5)?,
            row.get::<_, i64>(6)?,
            row.get::<_, String>(7)?,
            row.get::<_, String>(8)?,
            row.get::<_, String>(9)?,
            row.get::<_, String>(10)?,
            row.get::<_, String>(11)?,
        ))
    })?;
    rows.map(|row| {
        let (
            id,
            account_id,
            name,
            enabled,
            position,
            match_mode,
            stop_processing,
            conditions_json,
            actions_json,
            execution,
            created_at,
            updated_at,
        ) = row?;
        row_to_rule(
            id,
            account_id,
            name,
            enabled,
            position,
            match_mode,
            stop_processing,
            conditions_json,
            actions_json,
            execution,
            created_at,
            updated_at,
        )
    })
    .collect()
}

pub fn upsert_rule(conn: &Connection, rule: &MessageRule) -> StoreResult<MessageRule> {
    let mut position = rule.position;
    if position < 0 {
        let max: i32 = conn
            .query_row(
                "SELECT COALESCE(MAX(position), -1) FROM message_rules WHERE account_id = ?1",
                params![rule.account_id.0.to_string()],
                |row| row.get(0),
            )
            .unwrap_or(-1);
        position = max + 1;
    }
    let conditions_json = serde_json::to_string(&rule.conditions)?;
    let actions_json = serde_json::to_string(&rule.actions)?;
    let now = Utc::now();
    let created_at = format_ts(rule.created_at);
    let updated_at = format_ts(now);
    conn.execute(
        "INSERT INTO message_rules (
            id, account_id, name, enabled, position, match_mode, stop_processing,
            conditions_json, actions_json, execution, created_at, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
         ON CONFLICT(id) DO UPDATE SET
            name = excluded.name,
            enabled = excluded.enabled,
            position = excluded.position,
            match_mode = excluded.match_mode,
            stop_processing = excluded.stop_processing,
            conditions_json = excluded.conditions_json,
            actions_json = excluded.actions_json,
            execution = excluded.execution,
            updated_at = excluded.updated_at",
        params![
            rule.id.0.to_string(),
            rule.account_id.0.to_string(),
            rule.name,
            i64::from(rule.enabled),
            position,
            match_mode_to_str(rule.match_mode),
            i64::from(rule.stop_processing),
            conditions_json,
            actions_json,
            execution_to_str(rule.execution),
            created_at,
            updated_at,
        ],
    )?;
    Ok(MessageRule {
        position,
        updated_at: now,
        ..rule.clone()
    })
}

pub fn delete_rule(conn: &Connection, id: RuleId) -> StoreResult<()> {
    conn.execute(
        "DELETE FROM message_rules WHERE id = ?1",
        params![id.0.to_string()],
    )?;
    Ok(())
}

pub fn reorder_rules(conn: &Connection, account: AccountId, ids: &[RuleId]) -> StoreResult<()> {
    let tx = conn.unchecked_transaction()?;
    for (position, id) in ids.iter().enumerate() {
        tx.execute(
            "UPDATE message_rules SET position = ?1, updated_at = ?2
             WHERE id = ?3 AND account_id = ?4",
            params![
                i32::try_from(position).unwrap_or(i32::MAX),
                format_ts(Utc::now()),
                id.0.to_string(),
                account.0.to_string(),
            ],
        )?;
    }
    tx.commit()?;
    Ok(())
}

pub fn set_rule_execution(
    conn: &Connection,
    id: RuleId,
    execution: RuleExecution,
) -> StoreResult<()> {
    conn.execute(
        "UPDATE message_rules SET execution = ?1, updated_at = ?2 WHERE id = ?3",
        params![
            execution_to_str(execution),
            format_ts(Utc::now()),
            id.0.to_string(),
        ],
    )?;
    Ok(())
}

pub fn has_rule_run(conn: &Connection, message_id: MessageId) -> StoreResult<bool> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(1) FROM message_rule_runs WHERE message_id = ?1",
        params![message_id.0],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

pub fn mark_rule_run(conn: &Connection, message_id: MessageId) -> StoreResult<()> {
    conn.execute(
        "INSERT OR IGNORE INTO message_rule_runs (message_id) VALUES (?1)",
        params![message_id.0],
    )?;
    Ok(())
}

pub fn rule_runs_for(conn: &Connection, ids: &[MessageId]) -> StoreResult<HashSet<MessageId>> {
    if ids.is_empty() {
        return Ok(HashSet::new());
    }
    let id_json = serde_json::to_string(&ids.iter().map(|id| id.0).collect::<Vec<_>>())?;
    let mut stmt = conn.prepare(
        "SELECT message_id FROM message_rule_runs
         WHERE message_id IN (SELECT CAST(value AS INTEGER) FROM json_each(?1))",
    )?;
    let rows = stmt.query_map(params![id_json], |row| Ok(MessageId(row.get(0)?)))?;
    let mut out = HashSet::new();
    for row in rows {
        out.insert(row?);
    }
    Ok(out)
}

pub fn mark_rule_runs(conn: &Connection, ids: &[MessageId]) -> StoreResult<()> {
    if ids.is_empty() {
        return Ok(());
    }
    let tx = conn.unchecked_transaction()?;
    for id in ids {
        tx.execute(
            "INSERT OR IGNORE INTO message_rule_runs (message_id) VALUES (?1)",
            params![id.0],
        )?;
    }
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use alcedo_core::{RuleAction, RuleCondition, RuleField, RuleMatchMode, RuleOp};
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
            smtp_port: 465,
            smtp_starttls: false,
            auth: alcedo_core::AuthMethod::Password,
            accent_color: "#3584e4".to_string(),
            avatar_text: "U".to_string(),
            avatar_image: Vec::new(),
        }
    }

    fn sample_rule(account_id: AccountId, position: i32) -> MessageRule {
        let now = Utc::now();
        MessageRule {
            id: RuleId(Uuid::new_v4()),
            account_id,
            name: format!("Rule {position}"),
            enabled: true,
            position,
            match_mode: RuleMatchMode::All,
            stop_processing: true,
            conditions: vec![RuleCondition {
                field: RuleField::From,
                op: RuleOp::Contains,
                value: "news".to_string(),
            }],
            actions: vec![RuleAction::Star],
            execution: RuleExecution::Client,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn rules_crud_and_reorder() {
        let dir = TempDir::new().expect("tempdir");
        let db = dir.path().join("alcedo.db");
        let conn = schema::open_connection(&db).expect("open");
        let account_id = Uuid::new_v4();
        accounts::upsert_account(&conn, &sample_account(account_id)).expect("account");

        let r0 = sample_rule(AccountId(account_id), 0);
        let mut r1 = sample_rule(AccountId(account_id), 1);
        r1.name = "Second".to_string();
        upsert_rule(&conn, &r0).expect("upsert0");
        upsert_rule(&conn, &r1).expect("upsert1");

        let listed = list_rules(&conn, AccountId(account_id)).expect("list");
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].name, r0.name);

        reorder_rules(&conn, AccountId(account_id), &[r1.id, r0.id]).expect("reorder");
        let reordered = list_rules(&conn, AccountId(account_id)).expect("list2");
        assert_eq!(reordered[0].id, r1.id);

        delete_rule(&conn, r0.id).expect("delete");
        assert_eq!(
            list_rules(&conn, AccountId(account_id))
                .expect("list3")
                .len(),
            1
        );
    }

    #[test]
    fn rule_runs_and_account_cascade() {
        use crate::folders;
        use crate::messages;
        use alcedo_core::{Address, Envelope, Folder, FolderId, SpecialUse, ThreadId};

        let dir = TempDir::new().expect("tempdir");
        let db = dir.path().join("alcedo.db");
        let conn = schema::open_connection(&db).expect("open");
        let account_id = Uuid::new_v4();
        let account = sample_account(account_id);
        accounts::upsert_account(&conn, &account).expect("account");
        let folder = Folder {
            id: FolderId(0),
            account_id: AccountId(account_id),
            imap_name: "INBOX".to_string(),
            delimiter: "/".to_string(),
            special: SpecialUse::Inbox,
            selectable: true,
            uidvalidity: Some(1),
            uidnext: Some(2),
            highest_modseq: None,
            unread: 0,
            total: 0,
        };
        use crate::folders::FolderReplaceMode;
        let folders = folders::replace_folders(
            &conn,
            AccountId(account_id),
            &[folder],
            FolderReplaceMode::ListSnapshot,
        )
        .expect("folders");
        let inbox_id = folders[0].id;

        let rule = sample_rule(AccountId(account_id), 0);
        upsert_rule(&conn, &rule).expect("upsert");

        let env = Envelope {
            id: MessageId(0),
            account_id: AccountId(account_id),
            folder_id: inbox_id,
            uid: 1,
            thread_id: ThreadId(Uuid::new_v4()),
            date: Utc::now(),
            subject: "hi".to_string(),
            from: vec![Address {
                name: None,
                email: "a@b.com".to_string(),
            }],
            to: vec![],
            cc: vec![],
            reply_to: Vec::new(),
            sender: Vec::new(),
            routing: None,
            snippet: String::new(),
            seen: false,
            flagged: false,
            answered: false,
            draft: false,
            has_attachments: false,
            deleted_at: None,
            account_accent: String::new(),
            account_avatar: String::new(),
            size_bytes: 0,
        };
        messages::upsert_envelopes(&conn, &[env], None).expect("env");
        let listed = messages::folder_messages(&conn, inbox_id, 1, 0).expect("msgs");
        let msg_id = listed[0].id;

        assert!(!has_rule_run(&conn, msg_id).expect("has"));
        mark_rule_run(&conn, msg_id).expect("mark");
        assert!(has_rule_run(&conn, msg_id).expect("has2"));

        accounts::delete_account(&conn, AccountId(account_id)).expect("delete account");
        assert!(
            list_rules(&conn, AccountId(account_id))
                .expect("list")
                .is_empty()
        );
    }

    #[test]
    fn upsert_negative_position_appends() {
        let dir = TempDir::new().expect("tempdir");
        let db = dir.path().join("alcedo.db");
        let conn = schema::open_connection(&db).expect("open");
        let account_id = Uuid::new_v4();
        accounts::upsert_account(&conn, &sample_account(account_id)).expect("account");

        let mut rule = sample_rule(AccountId(account_id), 0);
        upsert_rule(&conn, &rule).expect("upsert");
        rule.id = RuleId(Uuid::new_v4());
        rule.position = -1;
        let saved = upsert_rule(&conn, &rule).expect("append");
        assert_eq!(saved.position, 1);
    }
}
