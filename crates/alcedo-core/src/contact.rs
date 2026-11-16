//! Local address book types (contacts and groups).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::envelope::Address;
use crate::ids::{ContactId, GroupId};

pub const UNAPPROVED_GROUP_ID: GroupId = GroupId(1);
pub const UNAPPROVED_GROUP_NAME: &str = "Unapproved";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ContactStatus {
    Approved,
    Suggested,
    Blocked,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Contact {
    pub id: ContactId,
    pub email: String,
    pub display_name: String,
    pub nickname: String,
    pub organization: String,
    pub notes: String,
    pub status: ContactStatus,
    pub group_ids: Vec<GroupId>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ContactGroup {
    pub id: GroupId,
    pub name: String,
    pub is_system: bool,
    pub member_ids: Vec<ContactId>,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ContactDraft {
    pub id: Option<ContactId>,
    pub email: String,
    pub display_name: String,
    pub nickname: String,
    pub organization: String,
    pub notes: String,
    pub status: ContactStatus,
    pub group_ids: Vec<GroupId>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GroupDraft {
    pub id: Option<GroupId>,
    pub name: String,
    pub member_ids: Vec<ContactId>,
}

impl GroupDraft {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.name.trim().is_empty() {
            return Err("Group name is required");
        }
        if self.name.eq_ignore_ascii_case(UNAPPROVED_GROUP_NAME) {
            return Err("Cannot use the name Unapproved");
        }
        Ok(())
    }
}

/// Thunderbird-style collection of outgoing recipients into the address book.
pub fn collect_outgoing_into(
    contacts: &mut Vec<Contact>,
    groups: &mut [ContactGroup],
    account_email: &str,
    addresses: &[Address],
    now: DateTime<Utc>,
) {
    let account_lower = account_email.to_ascii_lowercase();

    for addr in addresses {
        let email_lower = addr.email.to_ascii_lowercase();
        if email_lower == account_lower {
            continue;
        }

        if let Some(existing) = contacts
            .iter_mut()
            .find(|c| c.email.eq_ignore_ascii_case(&addr.email))
        {
            existing.last_used_at = Some(now);
            existing.updated_at = now;
            if existing.display_name.is_empty()
                && let Some(name) = addr
                    .name
                    .as_deref()
                    .map(str::trim)
                    .filter(|n| !n.is_empty())
            {
                existing.display_name = name.to_string();
            }
            if existing.status == ContactStatus::Blocked {
                continue;
            }
            if existing.status == ContactStatus::Suggested
                && !existing.group_ids.contains(&UNAPPROVED_GROUP_ID)
            {
                existing.group_ids.push(UNAPPROVED_GROUP_ID);
            }
            continue;
        }

        let display_name = addr
            .name
            .as_deref()
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .unwrap_or("")
            .to_string();

        let next_id = contacts
            .iter()
            .map(|c| c.id.0)
            .max()
            .unwrap_or(0)
            .saturating_add(1);

        let contact = Contact {
            id: ContactId(next_id),
            email: email_lower,
            display_name,
            nickname: String::new(),
            organization: String::new(),
            notes: String::new(),
            status: ContactStatus::Suggested,
            group_ids: vec![UNAPPROVED_GROUP_ID],
            created_at: now,
            updated_at: now,
            last_used_at: Some(now),
        };
        contacts.push(contact);

        if let Some(group) = groups.iter_mut().find(|g| g.id == UNAPPROVED_GROUP_ID) {
            let contact_id = contacts.last().expect("just pushed").id;
            if !group.member_ids.contains(&contact_id) {
                group.member_ids.push(contact_id);
            }
        }
    }
}

/// Mark a contact blocked and drop Unapproved membership.
pub fn block_contact_in_memory(
    contacts: &mut [Contact],
    groups: &mut [ContactGroup],
    id: ContactId,
) {
    if let Some(contact) = contacts.iter_mut().find(|c| c.id == id) {
        contact.status = ContactStatus::Blocked;
        contact.group_ids.retain(|gid| *gid != UNAPPROVED_GROUP_ID);
    }
    if let Some(group) = groups.iter_mut().find(|g| g.id == UNAPPROVED_GROUP_ID) {
        group.member_ids.retain(|cid| *cid != id);
    }
}

/// Remove Unapproved membership when promoting a suggested contact.
pub fn promote_contact_in_memory(
    contacts: &mut [Contact],
    groups: &mut [ContactGroup],
    id: ContactId,
) {
    if let Some(contact) = contacts.iter_mut().find(|c| c.id == id) {
        contact.status = ContactStatus::Approved;
        contact.group_ids.retain(|gid| *gid != UNAPPROVED_GROUP_ID);
    }
    if let Some(group) = groups.iter_mut().find(|g| g.id == UNAPPROVED_GROUP_ID) {
        group.member_ids.retain(|cid| *cid != id);
    }
}

/// Drop contacts and prune group memberships after a local delete.
pub fn remove_contacts_in_memory(
    contacts: &mut Vec<Contact>,
    groups: &mut Vec<ContactGroup>,
    ids: &[ContactId],
) {
    contacts.retain(|c| !ids.contains(&c.id));
    for group in groups {
        group.member_ids.retain(|cid| !ids.contains(cid));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn sample_time() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2025, 1, 1, 12, 0, 0).unwrap()
    }

    fn approved_contact(email: &str, name: &str) -> Contact {
        Contact {
            id: ContactId(10),
            email: email.to_string(),
            display_name: name.to_string(),
            nickname: String::new(),
            organization: String::new(),
            notes: String::new(),
            status: ContactStatus::Approved,
            group_ids: vec![],
            created_at: sample_time(),
            updated_at: sample_time(),
            last_used_at: None,
        }
    }

    fn unapproved_group() -> ContactGroup {
        ContactGroup {
            id: UNAPPROVED_GROUP_ID,
            name: UNAPPROVED_GROUP_NAME.to_string(),
            is_system: true,
            member_ids: vec![],
            created_at: sample_time(),
        }
    }

    #[test]
    fn collect_outgoing_skips_self() {
        let mut contacts = vec![];
        let mut groups = vec![unapproved_group()];
        let now = sample_time();
        collect_outgoing_into(
            &mut contacts,
            &mut groups,
            "me@example.com",
            &[Address {
                name: None,
                email: "me@example.com".to_string(),
            }],
            now,
        );
        assert!(contacts.is_empty());
    }

    #[test]
    fn collect_outgoing_inserts_suggested() {
        let mut contacts = vec![];
        let mut groups = vec![unapproved_group()];
        let now = sample_time();
        collect_outgoing_into(
            &mut contacts,
            &mut groups,
            "me@example.com",
            &[Address {
                name: Some("Bob".to_string()),
                email: "bob@example.com".to_string(),
            }],
            now,
        );
        assert_eq!(contacts.len(), 1);
        assert_eq!(contacts[0].status, ContactStatus::Suggested);
        assert!(contacts[0].group_ids.contains(&UNAPPROVED_GROUP_ID));
        assert_eq!(groups[0].member_ids.len(), 1);
    }

    #[test]
    fn collect_outgoing_does_not_demote_approved() {
        let mut contacts = vec![approved_contact("bob@example.com", "Bob")];
        let mut groups = vec![unapproved_group()];
        let now = sample_time();
        collect_outgoing_into(
            &mut contacts,
            &mut groups,
            "me@example.com",
            &[Address {
                name: None,
                email: "bob@example.com".to_string(),
            }],
            now,
        );
        assert_eq!(contacts[0].status, ContactStatus::Approved);
        assert!(contacts[0].last_used_at.is_some());
        assert!(contacts[0].group_ids.is_empty());
    }

    #[test]
    fn collect_outgoing_fills_empty_name() {
        let mut contacts = vec![Contact {
            display_name: String::new(),
            ..approved_contact("bob@example.com", "")
        }];
        let mut groups = vec![unapproved_group()];
        let now = sample_time();
        collect_outgoing_into(
            &mut contacts,
            &mut groups,
            "me@example.com",
            &[Address {
                name: Some("Robert".to_string()),
                email: "bob@example.com".to_string(),
            }],
            now,
        );
        assert_eq!(contacts[0].display_name, "Robert");
    }

    #[test]
    fn promote_removes_unapproved_membership() {
        let mut contacts = vec![Contact {
            id: ContactId(5),
            email: "bob@example.com".to_string(),
            display_name: "Bob".to_string(),
            nickname: String::new(),
            organization: String::new(),
            notes: String::new(),
            status: ContactStatus::Suggested,
            group_ids: vec![UNAPPROVED_GROUP_ID],
            created_at: sample_time(),
            updated_at: sample_time(),
            last_used_at: None,
        }];
        let mut groups = vec![ContactGroup {
            member_ids: vec![ContactId(5)],
            ..unapproved_group()
        }];
        promote_contact_in_memory(&mut contacts, &mut groups, ContactId(5));
        assert_eq!(contacts[0].status, ContactStatus::Approved);
        assert!(!contacts[0].group_ids.contains(&UNAPPROVED_GROUP_ID));
        assert!(groups[0].member_ids.is_empty());
    }

    #[test]
    fn remove_contacts_prunes_memberships() {
        let mut contacts = vec![
            Contact {
                id: ContactId(5),
                email: "bob@example.com".to_string(),
                display_name: "Bob".to_string(),
                nickname: String::new(),
                organization: String::new(),
                notes: String::new(),
                status: ContactStatus::Suggested,
                group_ids: vec![UNAPPROVED_GROUP_ID],
                created_at: sample_time(),
                updated_at: sample_time(),
                last_used_at: None,
            },
            Contact {
                id: ContactId(10),
                email: "alice@example.com".to_string(),
                display_name: "Alice".to_string(),
                nickname: String::new(),
                organization: String::new(),
                notes: String::new(),
                status: ContactStatus::Approved,
                group_ids: vec![GroupId(3)],
                created_at: sample_time(),
                updated_at: sample_time(),
                last_used_at: None,
            },
        ];
        let mut groups = vec![
            ContactGroup {
                member_ids: vec![ContactId(5)],
                ..unapproved_group()
            },
            ContactGroup {
                id: GroupId(3),
                name: "Family".to_string(),
                is_system: false,
                member_ids: vec![ContactId(10)],
                created_at: sample_time(),
            },
        ];
        remove_contacts_in_memory(&mut contacts, &mut groups, &[ContactId(5), ContactId(10)]);
        assert!(contacts.is_empty());
        assert!(groups[0].member_ids.is_empty());
        assert!(groups[1].member_ids.is_empty());
    }

    #[test]
    fn collect_outgoing_does_not_unblock() {
        let mut contacts = vec![Contact {
            id: ContactId(7),
            email: "blocked@example.com".to_string(),
            display_name: String::new(),
            nickname: String::new(),
            organization: String::new(),
            notes: String::new(),
            status: ContactStatus::Blocked,
            group_ids: vec![],
            created_at: sample_time(),
            updated_at: sample_time(),
            last_used_at: None,
        }];
        let mut groups = vec![unapproved_group()];
        let now = sample_time();
        collect_outgoing_into(
            &mut contacts,
            &mut groups,
            "me@example.com",
            &[Address {
                name: Some("Blocked".to_string()),
                email: "blocked@example.com".to_string(),
            }],
            now,
        );
        assert_eq!(contacts[0].status, ContactStatus::Blocked);
        assert!(contacts[0].last_used_at.is_some());
        assert!(contacts[0].group_ids.is_empty());
        assert!(groups[0].member_ids.is_empty());
    }

    #[test]
    fn block_removes_unapproved_membership() {
        let mut contacts = vec![Contact {
            id: ContactId(5),
            email: "bob@example.com".to_string(),
            display_name: "Bob".to_string(),
            nickname: String::new(),
            organization: String::new(),
            notes: String::new(),
            status: ContactStatus::Suggested,
            group_ids: vec![UNAPPROVED_GROUP_ID],
            created_at: sample_time(),
            updated_at: sample_time(),
            last_used_at: None,
        }];
        let mut groups = vec![ContactGroup {
            member_ids: vec![ContactId(5)],
            ..unapproved_group()
        }];
        block_contact_in_memory(&mut contacts, &mut groups, ContactId(5));
        assert_eq!(contacts[0].status, ContactStatus::Blocked);
        assert!(!contacts[0].group_ids.contains(&UNAPPROVED_GROUP_ID));
        assert!(groups[0].member_ids.is_empty());
    }

    #[test]
    fn group_draft_rejects_unapproved_name() {
        let draft = GroupDraft {
            id: None,
            name: "Unapproved".to_string(),
            member_ids: vec![],
        };
        assert!(draft.validate().is_err());
    }
}
