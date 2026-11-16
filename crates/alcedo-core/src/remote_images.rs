//! Remote image allowlist resolution (privacy-first).

use std::collections::HashSet;

use crate::envelope::Address;

pub fn normalize_sender_email(email: &str) -> Option<String> {
    let e = email.trim().to_ascii_lowercase();
    if e.is_empty() { None } else { Some(e) }
}

pub fn should_load_remote_images(
    global: bool,
    message_allowed: bool,
    from: &[Address],
    allowed_senders: &HashSet<String>,
) -> bool {
    if global || message_allowed {
        return true;
    }
    from.iter()
        .any(|a| normalize_sender_email(&a.email).is_some_and(|e| allowed_senders.contains(&e)))
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;
    use crate::envelope::Address;

    fn addr(email: &str) -> Address {
        Address {
            name: None,
            email: email.to_string(),
        }
    }

    #[test]
    fn normalize_sender_email_trims_and_lowercases() {
        assert_eq!(
            normalize_sender_email("  Alice@Example.COM  "),
            Some("alice@example.com".to_string())
        );
    }

    #[test]
    fn normalize_sender_email_empty_is_none() {
        assert_eq!(normalize_sender_email(""), None);
        assert_eq!(normalize_sender_email("   "), None);
    }

    #[test]
    fn should_load_when_global() {
        assert!(should_load_remote_images(true, false, &[], &HashSet::new(),));
    }

    #[test]
    fn should_load_when_message_allowed() {
        assert!(should_load_remote_images(false, true, &[], &HashSet::new(),));
    }

    #[test]
    fn should_load_when_sender_allowed() {
        let mut senders = HashSet::new();
        senders.insert("sender@example.com".to_string());
        assert!(should_load_remote_images(
            false,
            false,
            &[addr("Sender@Example.com")],
            &senders,
        ));
    }

    #[test]
    fn should_not_load_when_none_apply() {
        assert!(!should_load_remote_images(
            false,
            false,
            &[addr("other@example.com")],
            &HashSet::new(),
        ));
    }

    #[test]
    fn should_not_load_when_from_empty() {
        let mut senders = HashSet::new();
        senders.insert("sender@example.com".to_string());
        assert!(!should_load_remote_images(false, false, &[], &senders));
    }
}
