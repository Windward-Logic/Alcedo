//! Contact and group persistence.

use std::collections::HashSet;

use chrono::{DateTime, Utc};
use rusqlite::{Connection, params};

use alcedo_core::{
    Address, Contact, ContactDraft, ContactGroup, ContactId, ContactStatus, GroupDraft, GroupId,
    UNAPPROVED_GROUP_ID, collect_outgoing_into,
};

use crate::StoreError;
use crate::StoreResult;
use crate::format_ts;

fn status_to_str(status: ContactStatus) -> &'static str {
    match status {
        ContactStatus::Approved => "approved",
        ContactStatus::Suggested => "suggested",
        ContactStatus::Blocked => "blocked",
    }
}

fn status_from_str(s: &str) -> StoreResult<ContactStatus> {
    match s {
        "approved" => Ok(ContactStatus::Approved),
        "suggested" => Ok(ContactStatus::Suggested),
        "blocked" => Ok(ContactStatus::Blocked),
        other => Err(StoreError::Other(format!(
            "unknown contact status: {other}"
        ))),
    }
}

fn parse_ts(value: Option<String>) -> StoreResult<Option<DateTime<Utc>>> {
    match value {
        None => Ok(None),
        Some(s) if s.is_empty() => Ok(None),
        Some(s) => Ok(Some(DateTime::parse_from_rfc3339(&s)?.with_timezone(&Utc))),
    }
}

fn load_group_members(conn: &Connection, group_id: GroupId) -> StoreResult<Vec<ContactId>> {
    let mut stmt = conn.prepare(
        "SELECT contact_id FROM contact_group_members WHERE group_id = ?1 ORDER BY contact_id",
    )?;
    let rows = stmt.query_map(params![group_id.0], |row| row.get::<_, i64>(0))?;
    Ok(rows
        .map(|id| id.map(ContactId))
        .collect::<Result<Vec<_>, _>>()?)
}

pub fn list_contacts(conn: &Connection) -> StoreResult<Vec<Contact>> {
    let mut stmt = conn.prepare(
        "SELECT id, email, display_name, nickname, organization, notes, status,
                created_at, updated_at, last_used_at
         FROM contacts
         ORDER BY display_name COLLATE NOCASE, email COLLATE NOCASE",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, String>(5)?,
            row.get::<_, String>(6)?,
            row.get::<_, String>(7)?,
            row.get::<_, String>(8)?,
            row.get::<_, Option<String>>(9)?,
        ))
    })?;

    let mut contacts = Vec::new();
    for row in rows {
        let (
            id,
            email,
            display_name,
            nickname,
            organization,
            notes,
            status,
            created_at,
            updated_at,
            last_used_at,
        ) = row?;
        let group_ids = load_contact_group_ids(conn, ContactId(id))?;
        contacts.push(Contact {
            id: ContactId(id),
            email,
            display_name,
            nickname,
            organization,
            notes,
            status: status_from_str(&status)?,
            group_ids,
            created_at: DateTime::parse_from_rfc3339(&created_at)?.with_timezone(&Utc),
            updated_at: DateTime::parse_from_rfc3339(&updated_at)?.with_timezone(&Utc),
            last_used_at: parse_ts(last_used_at)?,
        });
    }
    Ok(contacts)
}

fn load_contact_group_ids(conn: &Connection, contact_id: ContactId) -> StoreResult<Vec<GroupId>> {
    let mut stmt = conn.prepare(
        "SELECT group_id FROM contact_group_members WHERE contact_id = ?1 ORDER BY group_id",
    )?;
    let rows = stmt.query_map(params![contact_id.0], |row| row.get::<_, i64>(0))?;
    Ok(rows
        .map(|id| id.map(GroupId))
        .collect::<Result<Vec<_>, _>>()?)
}

pub fn list_groups(conn: &Connection) -> StoreResult<Vec<ContactGroup>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, is_system, created_at FROM contact_groups ORDER BY is_system DESC, name COLLATE NOCASE",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, String>(3)?,
        ))
    })?;

    let mut groups = Vec::new();
    for row in rows {
        let (id, name, is_system, created_at) = row?;
        let member_ids = load_group_members(conn, GroupId(id))?;
        groups.push(ContactGroup {
            id: GroupId(id),
            name,
            is_system: is_system != 0,
            member_ids,
            created_at: DateTime::parse_from_rfc3339(&created_at)?.with_timezone(&Utc),
        });
    }
    Ok(groups)
}

pub fn snapshot(conn: &Connection) -> StoreResult<(Vec<Contact>, Vec<ContactGroup>)> {
    Ok((list_contacts(conn)?, list_groups(conn)?))
}

fn email_taken_by_other(
    conn: &Connection,
    email: &str,
    exclude: Option<ContactId>,
) -> StoreResult<bool> {
    let count: i64 = match exclude {
        Some(id) => conn.query_row(
            "SELECT COUNT(*) FROM contacts WHERE email = ?1 COLLATE NOCASE AND id != ?2",
            params![email, id.0],
            |row| row.get(0),
        )?,
        None => conn.query_row(
            "SELECT COUNT(*) FROM contacts WHERE email = ?1 COLLATE NOCASE",
            params![email],
            |row| row.get(0),
        )?,
    };
    Ok(count > 0)
}

fn replace_contact_groups(
    conn: &Connection,
    contact_id: ContactId,
    group_ids: &[GroupId],
) -> StoreResult<()> {
    conn.execute(
        "DELETE FROM contact_group_members WHERE contact_id = ?1",
        params![contact_id.0],
    )?;
    for group_id in group_ids {
        conn.execute(
            "INSERT OR IGNORE INTO contact_group_members (group_id, contact_id) VALUES (?1, ?2)",
            params![group_id.0, contact_id.0],
        )?;
    }
    Ok(())
}

pub fn upsert_contact(conn: &Connection, draft: ContactDraft) -> StoreResult<Contact> {
    let email = draft.email.trim().to_ascii_lowercase();
    if email.is_empty() {
        return Err(StoreError::Other("email is required".to_string()));
    }

    if email_taken_by_other(conn, &email, draft.id)? {
        return Err(StoreError::Other("email already exists".to_string()));
    }

    let now = Utc::now();
    let contact_id = match draft.id {
        Some(id) => {
            conn.execute(
                "UPDATE contacts SET email = ?2, display_name = ?3, nickname = ?4,
                 organization = ?5, notes = ?6, status = ?7, updated_at = ?8
                 WHERE id = ?1",
                params![
                    id.0,
                    email,
                    draft.display_name,
                    draft.nickname,
                    draft.organization,
                    draft.notes,
                    status_to_str(draft.status),
                    format_ts(now),
                ],
            )?;
            replace_contact_groups(conn, id, &draft.group_ids)?;
            id
        }
        None => {
            conn.execute(
                "INSERT INTO contacts (email, display_name, nickname, organization, notes, status,
                 created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    email,
                    draft.display_name,
                    draft.nickname,
                    draft.organization,
                    draft.notes,
                    status_to_str(draft.status),
                    format_ts(now),
                    format_ts(now),
                ],
            )?;
            let id = ContactId(conn.last_insert_rowid());
            replace_contact_groups(conn, id, &draft.group_ids)?;
            id
        }
    };

    list_contacts(conn)?
        .into_iter()
        .find(|c| c.id == contact_id)
        .ok_or_else(|| StoreError::Other("contact not found after upsert".to_string()))
}

pub fn delete_contacts(conn: &Connection, ids: &[ContactId]) -> StoreResult<()> {
    let tx = conn.unchecked_transaction()?;
    for id in ids {
        tx.execute("DELETE FROM contacts WHERE id = ?1", params![id.0])?;
    }
    tx.commit()?;
    Ok(())
}

pub fn delete_contact(conn: &Connection, id: ContactId) -> StoreResult<()> {
    delete_contacts(conn, &[id])
}

pub fn upsert_group(conn: &Connection, draft: GroupDraft) -> StoreResult<ContactGroup> {
    draft
        .validate()
        .map_err(|msg| StoreError::Other(msg.to_string()))?;

    let name = draft.name.trim().to_string();
    if name.is_empty() {
        return Err(StoreError::Other("group name is required".to_string()));
    }

    if let Some(id) = draft.id {
        let is_system: i64 = conn.query_row(
            "SELECT is_system FROM contact_groups WHERE id = ?1",
            params![id.0],
            |row| row.get(0),
        )?;
        if is_system != 0 {
            return Err(StoreError::Other("cannot modify system group".to_string()));
        }
        let taken: i64 = conn.query_row(
            "SELECT COUNT(*) FROM contact_groups WHERE name = ?1 COLLATE NOCASE AND id != ?2",
            params![name, id.0],
            |row| row.get(0),
        )?;
        if taken > 0 {
            return Err(StoreError::Other("group name already exists".to_string()));
        }
        conn.execute(
            "UPDATE contact_groups SET name = ?2 WHERE id = ?1",
            params![id.0, name],
        )?;
        replace_group_members(conn, id, &draft.member_ids)?;
        return list_groups(conn)?
            .into_iter()
            .find(|g| g.id == id)
            .ok_or_else(|| StoreError::Other("group not found after update".to_string()));
    }

    let taken: i64 = conn.query_row(
        "SELECT COUNT(*) FROM contact_groups WHERE name = ?1 COLLATE NOCASE",
        params![name],
        |row| row.get(0),
    )?;
    if taken > 0 {
        return Err(StoreError::Other("group name already exists".to_string()));
    }

    let now = format_ts(Utc::now());
    conn.execute(
        "INSERT INTO contact_groups (name, is_system, created_at) VALUES (?1, 0, ?2)",
        params![name, now],
    )?;
    let id = GroupId(conn.last_insert_rowid());
    replace_group_members(conn, id, &draft.member_ids)?;
    list_groups(conn)?
        .into_iter()
        .find(|g| g.id == id)
        .ok_or_else(|| StoreError::Other("group not found after insert".to_string()))
}

fn replace_group_members(
    conn: &Connection,
    group_id: GroupId,
    member_ids: &[ContactId],
) -> StoreResult<()> {
    conn.execute(
        "DELETE FROM contact_group_members WHERE group_id = ?1",
        params![group_id.0],
    )?;
    for contact_id in member_ids {
        conn.execute(
            "INSERT OR IGNORE INTO contact_group_members (group_id, contact_id) VALUES (?1, ?2)",
            params![group_id.0, contact_id.0],
        )?;
    }
    Ok(())
}

pub fn delete_group(conn: &Connection, id: GroupId) -> StoreResult<()> {
    if id == UNAPPROVED_GROUP_ID {
        return Err(StoreError::Other(
            "Cannot delete the Unapproved group".to_string(),
        ));
    }
    let is_system: i64 = conn
        .query_row(
            "SELECT is_system FROM contact_groups WHERE id = ?1",
            params![id.0],
            |row| row.get(0),
        )
        .unwrap_or(0);
    if is_system != 0 {
        return Err(StoreError::Other("cannot delete system group".to_string()));
    }
    conn.execute("DELETE FROM contact_groups WHERE id = ?1", params![id.0])?;
    Ok(())
}

pub fn promote_contact(conn: &Connection, id: ContactId) -> StoreResult<Contact> {
    let now = format_ts(Utc::now());
    conn.execute(
        "UPDATE contacts SET status = 'approved', updated_at = ?2 WHERE id = ?1",
        params![id.0, now],
    )?;
    conn.execute(
        "DELETE FROM contact_group_members WHERE group_id = ?1 AND contact_id = ?2",
        params![UNAPPROVED_GROUP_ID.0, id.0],
    )?;
    list_contacts(conn)?
        .into_iter()
        .find(|c| c.id == id)
        .ok_or_else(|| StoreError::Other("contact not found".to_string()))
}

pub fn block_contact(conn: &Connection, id: ContactId) -> StoreResult<Contact> {
    let now = format_ts(Utc::now());
    conn.execute(
        "UPDATE contacts SET status = 'blocked', updated_at = ?2 WHERE id = ?1",
        params![id.0, now],
    )?;
    conn.execute(
        "DELETE FROM contact_group_members WHERE group_id = ?1 AND contact_id = ?2",
        params![UNAPPROVED_GROUP_ID.0, id.0],
    )?;
    list_contacts(conn)?
        .into_iter()
        .find(|c| c.id == id)
        .ok_or_else(|| StoreError::Other("contact not found".to_string()))
}

pub fn allow_sender(conn: &Connection, email: &str, display_name: &str) -> StoreResult<Contact> {
    let existing: Option<(i64, String)> = conn
        .query_row(
            "SELECT id, display_name FROM contacts
             WHERE alcedo_canonical_email(email) = alcedo_canonical_email(?1)
             LIMIT 1",
            params![email],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .ok();
    if let Some((id, existing_name)) = existing {
        let name = if display_name.trim().is_empty() {
            existing_name
        } else {
            display_name.to_string()
        };
        return promote_contact(conn, ContactId(id)).map(|mut contact| {
            if !name.is_empty() && contact.display_name.is_empty() {
                contact.display_name = name;
            }
            contact
        });
    }
    upsert_contact(
        conn,
        ContactDraft {
            id: None,
            email: email.to_string(),
            display_name: display_name.to_string(),
            nickname: String::new(),
            organization: String::new(),
            notes: String::new(),
            status: ContactStatus::Approved,
            group_ids: vec![],
        },
    )
}

pub fn collect_outgoing(
    conn: &Connection,
    account_email: &str,
    addresses: &[Address],
) -> StoreResult<()> {
    let mut contacts = list_contacts(conn)?;
    let mut groups = list_groups(conn)?;
    let now = Utc::now();
    collect_outgoing_into(&mut contacts, &mut groups, account_email, addresses, now);

    let account_lower = account_email.to_ascii_lowercase();
    let touched: HashSet<String> = addresses
        .iter()
        .map(|addr| addr.email.to_ascii_lowercase())
        .filter(|email| email != &account_lower)
        .collect();

    for contact in &contacts {
        if !touched.contains(&contact.email.to_ascii_lowercase()) {
            continue;
        }
        let existing: Option<i64> = conn
            .query_row(
                "SELECT id FROM contacts WHERE email = ?1 COLLATE NOCASE",
                params![contact.email],
                |row| row.get(0),
            )
            .ok();
        if existing.is_none() {
            let created = upsert_contact(
                conn,
                ContactDraft {
                    id: None,
                    email: contact.email.clone(),
                    display_name: contact.display_name.clone(),
                    nickname: contact.nickname.clone(),
                    organization: contact.organization.clone(),
                    notes: contact.notes.clone(),
                    status: contact.status,
                    group_ids: contact.group_ids.clone(),
                },
            )?;
            conn.execute(
                "UPDATE contacts SET last_used_at = ?2 WHERE id = ?1",
                params![created.id.0, format_ts(now)],
            )?;
        } else if let Some(id) = existing {
            conn.execute(
                "UPDATE contacts SET display_name = CASE WHEN display_name = '' THEN ?2 ELSE display_name END,
                 last_used_at = ?3, updated_at = ?3 WHERE id = ?1",
                params![id, contact.display_name, format_ts(now)],
            )?;
            for group_id in &contact.group_ids {
                conn.execute(
                    "INSERT OR IGNORE INTO contact_group_members (group_id, contact_id) VALUES (?1, ?2)",
                    params![group_id.0, id],
                )?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    use alcedo_core::UNAPPROVED_GROUP_NAME;

    fn test_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::schema::apply_pragmas(&conn).unwrap();
        crate::schema::migrate(&conn).unwrap();
        conn
    }

    #[test]
    fn migrate_twice_is_idempotent() {
        let conn = test_conn();
        crate::schema::migrate(&conn).unwrap();
        let version: i64 = conn
            .query_row("SELECT MAX(version) FROM schema_migrations", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(version, 37);
    }

    #[test]
    fn canonical_email_udf_handles_null_and_normalizes() {
        let conn = test_conn();
        let null: Option<String> = conn
            .query_row("SELECT alcedo_canonical_email(NULL)", [], |r| r.get(0))
            .unwrap();
        assert_eq!(null, None);
        let canonical: Option<String> = conn
            .query_row("SELECT alcedo_canonical_email('A+x@B.com')", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(canonical, Some("a@b.com".to_string()));
    }

    #[test]
    fn unique_email_case_insensitive() {
        let conn = test_conn();
        upsert_contact(
            &conn,
            ContactDraft {
                id: None,
                email: "Alice@Example.com".to_string(),
                display_name: "Alice".to_string(),
                nickname: String::new(),
                organization: String::new(),
                notes: String::new(),
                status: ContactStatus::Approved,
                group_ids: vec![],
            },
        )
        .unwrap();
        let err = upsert_contact(
            &conn,
            ContactDraft {
                id: None,
                email: "alice@example.com".to_string(),
                display_name: "Alice".to_string(),
                nickname: String::new(),
                organization: String::new(),
                notes: String::new(),
                status: ContactStatus::Approved,
                group_ids: vec![],
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("email already exists"));
    }

    #[test]
    fn cannot_delete_unapproved() {
        let conn = test_conn();
        let err = delete_group(&conn, UNAPPROVED_GROUP_ID).unwrap_err();
        assert!(err.to_string().contains("Unapproved"));
    }

    #[test]
    fn cannot_rename_unapproved_via_upsert() {
        let conn = test_conn();
        let err = upsert_group(
            &conn,
            GroupDraft {
                id: Some(UNAPPROVED_GROUP_ID),
                name: "Other".to_string(),
                member_ids: vec![],
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("system group"));
    }

    #[test]
    fn collect_outgoing_and_snapshot() {
        let conn = test_conn();
        collect_outgoing(
            &conn,
            "me@example.com",
            &[Address {
                name: Some("Bob".to_string()),
                email: "bob@example.com".to_string(),
            }],
        )
        .unwrap();
        let (contacts, groups) = snapshot(&conn).unwrap();
        assert_eq!(contacts.len(), 1);
        assert_eq!(contacts[0].status, ContactStatus::Suggested);
        let unapproved = groups
            .iter()
            .find(|g| g.name == UNAPPROVED_GROUP_NAME)
            .expect("unapproved");
        assert_eq!(unapproved.member_ids.len(), 1);
    }

    #[test]
    fn collect_outgoing_does_not_touch_other_contacts() {
        let conn = test_conn();
        upsert_contact(
            &conn,
            ContactDraft {
                id: None,
                email: "alice@example.com".to_string(),
                display_name: "Alice".to_string(),
                nickname: String::new(),
                organization: String::new(),
                notes: String::new(),
                status: ContactStatus::Approved,
                group_ids: vec![],
            },
        )
        .unwrap();
        collect_outgoing(
            &conn,
            "me@example.com",
            &[Address {
                name: Some("Bob".to_string()),
                email: "bob@example.com".to_string(),
            }],
        )
        .unwrap();
        let (contacts, _) = snapshot(&conn).unwrap();
        let alice = contacts
            .iter()
            .find(|c| c.email == "alice@example.com")
            .expect("alice");
        let bob = contacts
            .iter()
            .find(|c| c.email == "bob@example.com")
            .expect("bob");
        assert!(alice.last_used_at.is_none());
        assert!(bob.last_used_at.is_some());
    }

    #[test]
    fn delete_contacts_batch_approved_and_suggested() {
        let conn = test_conn();
        let approved = upsert_contact(
            &conn,
            ContactDraft {
                id: None,
                email: "alice@example.com".to_string(),
                display_name: "Alice".to_string(),
                nickname: String::new(),
                organization: String::new(),
                notes: String::new(),
                status: ContactStatus::Approved,
                group_ids: vec![],
            },
        )
        .unwrap();
        let suggested = upsert_contact(
            &conn,
            ContactDraft {
                id: None,
                email: "bob@example.com".to_string(),
                display_name: "Bob".to_string(),
                nickname: String::new(),
                organization: String::new(),
                notes: String::new(),
                status: ContactStatus::Suggested,
                group_ids: vec![UNAPPROVED_GROUP_ID],
            },
        )
        .unwrap();
        delete_contacts(&conn, &[approved.id, suggested.id]).unwrap();
        let (contacts, groups) = snapshot(&conn).unwrap();
        assert!(contacts.is_empty());
        let unapproved = groups.iter().find(|g| g.id == UNAPPROVED_GROUP_ID).unwrap();
        assert!(unapproved.member_ids.is_empty());
    }

    #[test]
    fn delete_contact_cascades_memberships() {
        let conn = test_conn();
        let contact = upsert_contact(
            &conn,
            ContactDraft {
                id: None,
                email: "bob@example.com".to_string(),
                display_name: "Bob".to_string(),
                nickname: String::new(),
                organization: String::new(),
                notes: String::new(),
                status: ContactStatus::Suggested,
                group_ids: vec![UNAPPROVED_GROUP_ID],
            },
        )
        .unwrap();
        delete_contact(&conn, contact.id).unwrap();
        let groups = list_groups(&conn).unwrap();
        let unapproved = groups.iter().find(|g| g.id == UNAPPROVED_GROUP_ID).unwrap();
        assert!(unapproved.member_ids.is_empty());
    }
}
