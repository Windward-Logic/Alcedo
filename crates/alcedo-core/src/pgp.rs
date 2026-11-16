//! OpenPGP domain types (no crypto backend).

use serde::{Deserialize, Serialize};

use crate::ids::AccountId;

/// Lowercase hex fingerprint (40 chars for v4 keys).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PgpFingerprint(pub String);

/// Where a stored public key came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PgpKeySource {
    Own,
    Import,
    Autocrypt,
}

/// Verification outcome for a signed message.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PgpSignatureState {
    #[default]
    None,
    Valid {
        fingerprint: String,
        signer_email: Option<String>,
    },
    Invalid {
        reason: String,
    },
    UnknownKey {
        fingerprint: String,
    },
}

/// Persisted / UI-facing PGP status for a message.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PgpStatus {
    #[serde(default)]
    pub encrypted: bool,
    #[serde(default)]
    pub decrypted: bool,
    #[serde(default)]
    pub signature: PgpSignatureState,
}

/// A public key associated with an email address.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PgpPeerInfo {
    pub email: String,
    pub fingerprint: String,
    pub source: PgpKeySource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub armored: Option<String>,
}

/// Per-account OpenPGP configuration and known keys (not part of [`crate::Account`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PgpAccountState {
    pub account_id: AccountId,
    pub sign_outgoing: bool,
    pub own: Option<PgpPeerInfo>,
    pub peers: Vec<PgpPeerInfo>,
}

/// Whether compose encrypt can be enabled for the current recipients.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EncryptReadiness {
    Ready,
    MissingOwn,
    Missing(Vec<String>),
}

/// Format a lowercase hex fingerprint for display (`XXXX XXXX …`).
pub fn format_fingerprint_display(hex: &str) -> String {
    let hex: String = hex.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    let hex = hex.to_ascii_lowercase();
    if hex.is_empty() {
        return String::new();
    }
    hex.as_bytes()
        .chunks(4)
        .map(|chunk| std::str::from_utf8(chunk).unwrap_or(""))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Normalize an email for PGP peer lookup.
fn normalize_email(email: &str) -> Option<String> {
    let email = email.trim();
    if email.is_empty() || !email.contains('@') {
        return None;
    }
    Some(email.to_ascii_lowercase())
}

/// Extract the email from a compose address token (`Name <a@b>` or bare email).
pub fn email_from_address_token(token: &str) -> Option<String> {
    let token = token.trim();
    if let Some(start) = token.rfind('<') {
        let end = token.rfind('>')?;
        if end > start {
            return normalize_email(&token[start + 1..end]);
        }
    }
    normalize_email(token)
}

/// Whether every To/Cc/Bcc address has an accepted public key and the account has its own key.
pub fn recipient_encrypt_ready(
    to_cc_bcc: &[String],
    peers: &[PgpPeerInfo],
    has_own: bool,
) -> EncryptReadiness {
    if !has_own {
        return EncryptReadiness::MissingOwn;
    }
    let mut missing = Vec::new();
    for token in to_cc_bcc {
        let Some(email) = email_from_address_token(token) else {
            continue;
        };
        let found = peers
            .iter()
            .any(|peer| peer.email.eq_ignore_ascii_case(&email));
        if !found {
            missing.push(email);
        }
    }
    missing.sort();
    missing.dedup();
    if missing.is_empty() {
        EncryptReadiness::Ready
    } else {
        EncryptReadiness::Missing(missing)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn format_fingerprint_groups_by_four() {
        let hex = "aabbccddeeff00112233445566778899aabbccdd";
        assert_eq!(
            format_fingerprint_display(hex),
            "aabb ccdd eeff 0011 2233 4455 6677 8899 aabb ccdd"
        );
    }

    #[test]
    fn recipient_encrypt_ready_needs_own_key() {
        let peers = vec![PgpPeerInfo {
            email: "peer@example.com".to_string(),
            fingerprint: "aa".to_string(),
            source: PgpKeySource::Import,
            armored: None,
        }];
        assert!(matches!(
            recipient_encrypt_ready(&["peer@example.com".to_string()], &peers, false),
            EncryptReadiness::MissingOwn
        ));
    }

    #[test]
    fn recipient_encrypt_ready_lists_missing() {
        let peers = vec![PgpPeerInfo {
            email: "peer@example.com".to_string(),
            fingerprint: "aa".to_string(),
            source: PgpKeySource::Import,
            armored: None,
        }];
        match recipient_encrypt_ready(
            &[
                "peer@example.com".to_string(),
                "Other <missing@example.com>".to_string(),
            ],
            &peers,
            true,
        ) {
            EncryptReadiness::Missing(m) => assert_eq!(m, vec!["missing@example.com".to_string()]),
            other => panic!("expected Missing, got {other:?}"),
        }
    }

    #[test]
    fn pgp_status_defaults_empty() {
        let status = PgpStatus::default();
        assert!(!status.encrypted);
        assert!(!status.decrypted);
        assert_eq!(status.signature, PgpSignatureState::None);
    }

    #[test]
    fn pgp_account_state_roundtrip() {
        let state = PgpAccountState {
            account_id: AccountId(Uuid::nil()),
            sign_outgoing: true,
            own: None,
            peers: vec![],
        };
        let json = serde_json::to_string(&state).unwrap();
        let back: PgpAccountState = serde_json::from_str(&json).unwrap();
        assert_eq!(state, back);
    }
}
