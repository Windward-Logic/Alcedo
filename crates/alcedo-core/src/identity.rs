use serde::{Deserialize, Serialize};

use crate::envelope::parse_address_token;
use crate::ids::{AccountId, IdentityId};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AccountIdentity {
    pub id: IdentityId,
    pub account_id: AccountId,
    pub display_name: String,
    pub email: String,
}

impl AccountIdentity {
    pub fn validate(&self, account_email: &str) -> Result<(), &'static str> {
        if self.email.is_empty() {
            return Err("Email is required");
        }
        let Some((local, domain)) = self.email.split_once('@') else {
            return Err("Email must contain @");
        };
        if local.is_empty() || domain.is_empty() {
            return Err("Email is invalid");
        }
        if self.email.eq_ignore_ascii_case(account_email) {
            return Err("This address is already the account address");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ComposeFromChoice {
    Account,
    Identity(IdentityId),
    Custom { display_name: String, email: String },
}

pub fn resolve_compose_from_text(
    text: &str,
    account_email: &str,
    identities: &[AccountIdentity],
) -> Result<ComposeFromChoice, &'static str> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(ComposeFromChoice::Account);
    }
    let Some(addr) = parse_address_token(trimmed) else {
        return Err("Email is invalid");
    };
    if addr.email.eq_ignore_ascii_case(account_email) {
        return Ok(ComposeFromChoice::Account);
    }
    if let Some(identity) = identities
        .iter()
        .find(|identity| identity.email.eq_ignore_ascii_case(&addr.email))
    {
        return Ok(ComposeFromChoice::Identity(identity.id));
    }
    Ok(ComposeFromChoice::Custom {
        display_name: addr.name.unwrap_or_default(),
        email: addr.email,
    })
}

pub fn mime_from_for_draft(
    account_email: &str,
    account_display_name: &str,
    identity_id: Option<IdentityId>,
    custom_from: Option<&str>,
    identities: &[AccountIdentity],
) -> (String, String) {
    if let Some(identity_id) = identity_id
        && let Some(identity) = identities
            .iter()
            .find(|identity| identity.id == identity_id)
    {
        return (identity.email.clone(), identity.display_name.clone());
    }
    if identity_id.is_none()
        && let Some(raw) = custom_from.filter(|value| !value.trim().is_empty())
        && let Some(addr) = parse_address_token(raw)
    {
        return (addr.email, addr.name.unwrap_or_default());
    }
    (account_email.to_string(), account_display_name.to_string())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SmtpEnvelopePolicy {
    Mailbox,
    SameDomainIdentity,
}

impl SmtpEnvelopePolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Mailbox => "mailbox",
            Self::SameDomainIdentity => "same_domain_identity",
        }
    }

    pub fn parse(value: &str) -> Self {
        match value {
            "same_domain_identity" => Self::SameDomainIdentity,
            _ => Self::Mailbox,
        }
    }
}

fn email_domain(email: &str) -> Option<&str> {
    let (_, domain) = email.split_once('@')?;
    if domain.is_empty() || domain.contains('@') {
        return None;
    }
    Some(domain)
}

pub fn same_email_domain(account_email: &str, mime_from: &str) -> bool {
    let Some(account_domain) = email_domain(account_email) else {
        return false;
    };
    let Some(mime_domain) = email_domain(mime_from) else {
        return false;
    };
    account_domain.eq_ignore_ascii_case(mime_domain)
}

pub fn smtp_mail_from<'a>(
    policy: SmtpEnvelopePolicy,
    account_email: &'a str,
    mime_from: &'a str,
    saved_identity: bool,
) -> &'a str {
    if mime_from.eq_ignore_ascii_case(account_email) {
        return account_email;
    }
    if policy == SmtpEnvelopePolicy::SameDomainIdentity
        && saved_identity
        && same_email_domain(account_email, mime_from)
    {
        mime_from
    } else {
        account_email
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn sample_identity(id: IdentityId, email: &str) -> AccountIdentity {
        AccountIdentity {
            id,
            account_id: AccountId(Uuid::nil()),
            display_name: "Alias".to_string(),
            email: email.to_string(),
        }
    }

    #[test]
    fn resolve_compose_from_text_account_email() {
        let identities = vec![sample_identity(
            IdentityId(Uuid::new_v4()),
            "alias@example.com",
        )];
        assert_eq!(
            resolve_compose_from_text("user@example.com", "user@example.com", &identities),
            Ok(ComposeFromChoice::Account)
        );
    }

    #[test]
    fn resolve_compose_from_text_saved_identity() {
        let identity_id = IdentityId(Uuid::new_v4());
        let identities = vec![sample_identity(identity_id, "alias@example.com")];
        assert_eq!(
            resolve_compose_from_text("Alias <alias@example.com>", "user@example.com", &identities),
            Ok(ComposeFromChoice::Identity(identity_id))
        );
    }

    #[test]
    fn resolve_compose_from_text_custom_address() {
        let identities = vec![sample_identity(
            IdentityId(Uuid::new_v4()),
            "alias@example.com",
        )];
        assert_eq!(
            resolve_compose_from_text("Other <new@example.com>", "user@example.com", &identities),
            Ok(ComposeFromChoice::Custom {
                display_name: "Other".to_string(),
                email: "new@example.com".to_string(),
            })
        );
    }

    #[test]
    fn resolve_compose_from_text_invalid() {
        assert_eq!(
            resolve_compose_from_text("not-an-email", "user@example.com", &[]),
            Err("Email is invalid")
        );
    }

    #[test]
    fn resolve_compose_from_text_empty() {
        assert_eq!(
            resolve_compose_from_text("", "user@example.com", &[]),
            Ok(ComposeFromChoice::Account)
        );
    }

    #[test]
    fn mime_from_for_draft_identity_ignores_custom() {
        let identity_id = IdentityId(Uuid::new_v4());
        let identities = vec![AccountIdentity {
            id: identity_id,
            account_id: AccountId(Uuid::nil()),
            display_name: "Work".to_string(),
            email: "alias@example.com".to_string(),
        }];
        assert_eq!(
            mime_from_for_draft(
                "user@example.com",
                "User",
                Some(identity_id),
                Some("Pat <pat@other.com>"),
                &identities
            ),
            ("alias@example.com".to_string(), "Work".to_string())
        );
    }

    #[test]
    fn mime_from_for_draft_custom_from() {
        assert_eq!(
            mime_from_for_draft(
                "user@example.com",
                "User",
                None,
                Some("Pat <pat@other.com>"),
                &[]
            ),
            ("pat@other.com".to_string(), "Pat".to_string())
        );
    }

    const ACCOUNT: &str = "user@example.com";

    #[test]
    fn smtp_mail_from_same_domain_saved_identity() {
        assert_eq!(
            smtp_mail_from(
                SmtpEnvelopePolicy::SameDomainIdentity,
                ACCOUNT,
                "alias@example.com",
                true
            ),
            "alias@example.com"
        );
    }

    #[test]
    fn smtp_mail_from_same_domain_case_insensitive() {
        assert_eq!(
            smtp_mail_from(
                SmtpEnvelopePolicy::SameDomainIdentity,
                ACCOUNT,
                "alias@Example.com",
                true
            ),
            "alias@Example.com"
        );
    }

    #[test]
    fn smtp_mail_from_plus_address_same_domain() {
        assert_eq!(
            smtp_mail_from(
                SmtpEnvelopePolicy::SameDomainIdentity,
                ACCOUNT,
                "user+tag@example.com",
                true
            ),
            "user+tag@example.com"
        );
    }

    #[test]
    fn smtp_mail_from_other_domain_saved_identity() {
        assert_eq!(
            smtp_mail_from(
                SmtpEnvelopePolicy::SameDomainIdentity,
                ACCOUNT,
                "alias@other.com",
                true
            ),
            ACCOUNT
        );
    }

    #[test]
    fn smtp_mail_from_subdomain_not_same_domain() {
        assert_eq!(
            smtp_mail_from(
                SmtpEnvelopePolicy::SameDomainIdentity,
                ACCOUNT,
                "alias@a.example.com",
                true
            ),
            ACCOUNT
        );
    }

    #[test]
    fn smtp_mail_from_unsaved_identity() {
        assert_eq!(
            smtp_mail_from(
                SmtpEnvelopePolicy::SameDomainIdentity,
                ACCOUNT,
                "alias@example.com",
                false
            ),
            ACCOUNT
        );
    }

    #[test]
    fn smtp_mail_from_mailbox_policy_saved_identity() {
        assert_eq!(
            smtp_mail_from(
                SmtpEnvelopePolicy::Mailbox,
                ACCOUNT,
                "alias@example.com",
                true
            ),
            ACCOUNT
        );
    }

    #[test]
    fn smtp_mail_from_mailbox_mime_from() {
        assert_eq!(
            smtp_mail_from(SmtpEnvelopePolicy::Mailbox, ACCOUNT, ACCOUNT, false),
            ACCOUNT
        );
        assert_eq!(
            smtp_mail_from(
                SmtpEnvelopePolicy::SameDomainIdentity,
                ACCOUNT,
                ACCOUNT,
                false
            ),
            ACCOUNT
        );
    }

    #[test]
    fn smtp_envelope_policy_parse_and_as_str() {
        assert_eq!(
            SmtpEnvelopePolicy::parse("nope"),
            SmtpEnvelopePolicy::Mailbox
        );
        for policy in [
            SmtpEnvelopePolicy::Mailbox,
            SmtpEnvelopePolicy::SameDomainIdentity,
        ] {
            assert_eq!(SmtpEnvelopePolicy::parse(policy.as_str()), policy);
        }
    }
}
