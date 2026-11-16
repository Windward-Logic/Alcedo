//! Sender address helpers: canonical form, no-reply detection, blocked senders.

use crate::contact::{Contact, ContactStatus};

/// Trim, lowercase, strip `+tag` from the local part. Returns `None` if invalid.
pub fn canonical_email(email: &str) -> Option<String> {
    let email = email.trim();
    if email.is_empty() {
        return None;
    }
    let email = email.to_ascii_lowercase();
    let (local, domain) = email.split_once('@')?;
    if local.is_empty() || domain.is_empty() {
        return None;
    }
    let local = local.split('+').next().unwrap_or(local);
    if local.is_empty() {
        return None;
    }
    Some(format!("{local}@{domain}"))
}

/// True when the local part (after plus-strip) matches a no-reply pattern.
pub fn is_noreply_address(email: &str) -> bool {
    let Some(canonical) = canonical_email(email) else {
        return false;
    };
    let local = canonical.split('@').next().unwrap_or("");
    matches!(
        local,
        "noreply"
            | "no-reply"
            | "no_reply"
            | "donotreply"
            | "do-not-reply"
            | "nobody"
            | "mailer-daemon"
    ) || local.starts_with("noreply")
        || local.starts_with("no-reply")
}

/// True when the contact book blocks this sender (matched on canonical form).
pub fn sender_is_blocked(from_email: &str, contacts: &[Contact]) -> bool {
    let Some(from) = canonical_email(from_email) else {
        return false;
    };
    contacts.iter().any(|contact| {
        contact.status == ContactStatus::Blocked
            && canonical_email(&contact.email).as_deref() == Some(from.as_str())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    use crate::ids::ContactId;

    fn sample_time() -> chrono::DateTime<Utc> {
        Utc.with_ymd_and_hms(2025, 1, 1, 12, 0, 0).unwrap()
    }

    fn contact(email: &str, status: ContactStatus) -> Contact {
        Contact {
            id: ContactId(1),
            email: email.to_string(),
            display_name: String::new(),
            nickname: String::new(),
            organization: String::new(),
            notes: String::new(),
            status,
            group_ids: vec![],
            created_at: sample_time(),
            updated_at: sample_time(),
            last_used_at: None,
        }
    }

    #[test]
    fn canonical_strips_plus_tag() {
        assert_eq!(canonical_email("A+list@X.com"), Some("a@x.com".to_string()));
        assert_ne!(canonical_email("a@x.com"), canonical_email("a@y.com"));
        assert!(canonical_email("").is_none());
        assert!(canonical_email("@x.com").is_none());
    }

    #[test]
    fn noreply_patterns() {
        assert!(is_noreply_address("noreply@example.com"));
        assert!(is_noreply_address("no-reply+foo@example.com"));
        assert!(is_noreply_address("mailer-daemon@example.com"));
        assert!(!is_noreply_address("ada@example.com"));
    }

    #[test]
    fn blocked_sender_matches_canonical_address() {
        let contacts = vec![
            contact("bob@example.com", ContactStatus::Approved),
            contact("spam@example.com", ContactStatus::Blocked),
        ];
        assert!(sender_is_blocked("spam@example.com", &contacts));
        assert!(sender_is_blocked("Spam+promo@Example.com", &contacts));
        assert!(!sender_is_blocked("bob@example.com", &contacts));
        assert!(!sender_is_blocked("unknown@example.com", &contacts));
        assert!(!sender_is_blocked("", &contacts));
    }
}
