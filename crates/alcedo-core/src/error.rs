#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("store: {0}")]
    Store(String),
    #[error("imap: {0}")]
    Imap(String),
    #[error("smtp: {0}")]
    Smtp(String),
    #[error("auth: {0}")]
    Auth(String),
    #[error("network: {0}")]
    Network(String),
    #[error("pgp: {0}")]
    Pgp(String),
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, Error>;

use std::time::Duration;

/// True when a message is a transport/DNS blip rather than revoked credentials.
pub fn looks_like_network_failure(msg: &str) -> bool {
    let lower = msg.to_ascii_lowercase();
    NETWORK_MARKERS.iter().any(|marker| lower.contains(marker))
}

pub const SYNC_NETWORK_TOAST_GRACE: Duration = Duration::from_secs(30);

/// Toast policy for background sync/calendar errors.
/// Non-network messages always toast. Network blips toast only after startup
/// grace, while the OS reports a network, and once per outage.
pub fn should_toast_sync_failure(
    message: &str,
    network_available: bool,
    elapsed: Duration,
    already_toasted_this_outage: bool,
) -> bool {
    if !looks_like_network_failure(message) {
        return true;
    }
    network_available && elapsed >= SYNC_NETWORK_TOAST_GRACE && !already_toasted_this_outage
}

const NETWORK_MARKERS: &[&str] = &[
    "temporary failure in name resolution",
    "failed to lookup",
    "name or service not known",
    "error sending request",
    "connection reset",
    "broken pipe",
    "connection timed out",
    "connection aborted",
    "unexpected eof",
    "unexpected end of file",
    "connection lost",
    "connect timeout",
    "tcp connect",
    "connection refused",
    "network is unreachable",
    "no route to host",
    "dns error",
    "timed out",
];

/// True when the stored sign-in error is the system keyring, not a rejected password.
pub fn is_keyring_auth_message(message: &str) -> bool {
    message.to_ascii_lowercase().contains("keyring")
}

/// Delay before the next automatic retry after a sign-in failure.
///
/// The first waits are short so a keyring that unlocks just after login can
/// recover without a click. Later waits back off so a bad password is not
/// retried constantly.
pub fn auth_retry_delay(attempt: u32) -> Duration {
    const STEPS_SECS: [u64; 5] = [15, 30, 60, 120, 300];
    let idx = (attempt as usize).min(STEPS_SECS.len() - 1);
    Duration::from_secs(STEPS_SECS[idx])
}

/// Server wording for a credential rejection that lacks "authenticate".
const AUTH_REJECTION_PHRASES: &[&str] = &[
    "authentication required",
    "authentication failed",
    "authentication unsuccessful",
    "authentication credentials invalid",
    "username and password not accepted",
];

/// Words that mark a message-authentication (domain policy) rejection.
const POLICY_REJECTION_MARKERS: &[&str] = &["dmarc", "spf", "dkim"];

/// True when the failure is revoked/expired credentials or an auth rejection, not a network blip.
pub fn is_auth_failure(err: &Error) -> bool {
    match err {
        Error::Auth(msg) => !looks_like_network_failure(msg),
        Error::Imap(msg) | Error::Smtp(msg) => {
            if looks_like_network_failure(msg) {
                return false;
            }
            let lower = msg.to_ascii_lowercase();
            // Sender-domain policy rejections ("Unauthenticated email ... DMARC
            // policy", "SPF and DKIM both do not pass") name authentication but
            // are about the domain's DNS records, not this account's sign-in.
            if POLICY_REJECTION_MARKERS
                .iter()
                .any(|marker| lower.contains(marker))
            {
                return false;
            }
            // lettre reports SMTP 530/535 as "Authentication required" /
            // "Username and Password not accepted", which lack "authenticate".
            // Match whole phrases: a bare "authentication" also hits policy
            // rejections such as "does not pass authentication checks"
            // (SPF/DKIM), which a fresh token or password cannot fix.
            lower.contains("authenticate")
                || AUTH_REJECTION_PHRASES
                    .iter()
                    .any(|phrase| lower.contains(phrase))
                || lower.contains("login failed")
                || lower.contains("authenticationfailed")
                || lower.contains("invalid credentials")
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_auth_failure_matches_auth_and_imap_smtp_rejections() {
        assert!(is_auth_failure(&Error::Auth("revoked".into())));
        assert!(is_auth_failure(&Error::Imap("AUTHENTICATE failed".into())));
        assert!(is_auth_failure(&Error::Smtp(
            "XOAUTH2 authenticate failed: invalid_grant".into()
        )));
        assert!(is_auth_failure(&Error::Imap(
            "XOAUTH2 authenticate failed: [AUTHENTICATIONFAILED] Invalid credentials (Failure)"
                .into()
        )));
    }

    #[test]
    fn is_auth_failure_matches_lettre_smtp_rejections() {
        assert!(is_auth_failure(&Error::Smtp(
            "send failed via smtp.example.com:587: permanent error (535): 5.7.8 Username and Password not accepted".into()
        )));
        assert!(is_auth_failure(&Error::Smtp(
            "send failed via smtp.example.com:587: permanent error (530): 5.7.0 Authentication Required".into()
        )));
    }

    #[test]
    fn is_auth_failure_ignores_policy_rejections_that_mention_authentication() {
        assert!(!is_auth_failure(&Error::Smtp(
            "send failed via smtp.example.com:587: permanent error (550): 5.7.26 This message does not pass authentication checks (SPF and DKIM both do not pass)".into()
        )));
        assert!(!is_auth_failure(&Error::Smtp(
            "send failed via smtp.example.com:587: permanent error (550): 5.7.26 Unauthenticated email from example.org is not accepted due to domain's DMARC policy".into()
        )));
        assert!(is_auth_failure(&Error::Smtp(
            "send failed via smtp.office365.com:587: permanent error (530): 5.7.57 Client not authenticated to send mail".into()
        )));
        assert!(is_auth_failure(&Error::Smtp(
            "send failed via smtp.office365.com:587: permanent error (535): 5.7.139 Authentication unsuccessful, the user credentials were incorrect".into()
        )));
    }

    #[test]
    fn graceful_close_is_a_network_failure_not_an_auth_failure() {
        assert!(looks_like_network_failure("UID FETCH 5: connection lost"));
        assert!(looks_like_network_failure("io: unexpected end of file"));
        assert!(!is_auth_failure(&Error::Imap(
            "authenticate failed: connection lost".into()
        )));
    }

    #[test]
    fn should_toast_sync_failure_non_network_always_toasts() {
        assert!(should_toast_sync_failure(
            "Folder not found",
            false,
            Duration::ZERO,
            false,
        ));
    }

    #[test]
    fn should_toast_sync_failure_network_blip_suppressed_during_grace() {
        let msg = "connection reset by peer";
        assert!(!should_toast_sync_failure(
            msg,
            true,
            Duration::from_secs(5),
            false,
        ));
        assert!(!should_toast_sync_failure(
            "Temporary failure in name resolution",
            true,
            Duration::from_secs(29),
            false,
        ));
    }

    #[test]
    fn should_toast_sync_failure_network_blip_never_toasts_while_offline() {
        let msg = "connection reset by peer";
        assert!(!should_toast_sync_failure(
            msg,
            false,
            SYNC_NETWORK_TOAST_GRACE,
            false,
        ));
    }

    #[test]
    fn should_toast_sync_failure_network_blip_toasts_once_after_grace() {
        let msg = "connection reset by peer";
        assert!(should_toast_sync_failure(
            msg,
            true,
            SYNC_NETWORK_TOAST_GRACE,
            false,
        ));
        assert!(!should_toast_sync_failure(
            msg,
            true,
            SYNC_NETWORK_TOAST_GRACE,
            true,
        ));
    }

    #[test]
    fn keyring_messages_match_locked_and_cancelled_unlock() {
        assert!(is_keyring_auth_message(
            "auth: system keyring is locked; unlock it and try again"
        ));
        assert!(is_keyring_auth_message(
            "auth: keyring unlock was cancelled"
        ));
        assert!(!is_keyring_auth_message(
            "auth: AUTHENTICATE failed: invalid credentials"
        ));
    }

    #[test]
    fn auth_retry_delay_starts_fast_and_caps_at_five_minutes() {
        assert_eq!(auth_retry_delay(0), Duration::from_secs(15));
        assert_eq!(auth_retry_delay(1), Duration::from_secs(30));
        assert_eq!(auth_retry_delay(2), Duration::from_secs(60));
        assert_eq!(auth_retry_delay(3), Duration::from_secs(120));
        assert_eq!(auth_retry_delay(4), Duration::from_secs(300));
        assert_eq!(auth_retry_delay(99), Duration::from_secs(300));
    }

    #[test]
    fn is_auth_failure_rejects_network_and_transport_imap_errors() {
        assert!(!is_auth_failure(&Error::Network("host unreachable".into())));
        assert!(!is_auth_failure(&Error::Imap("connection reset".into())));
        assert!(!is_auth_failure(&Error::Auth(
            "google token refresh failed: request failed: error sending request for url (https://oauth2.googleapis.com/token): dns error: failed to lookup address information: Temporary failure in name resolution".into()
        )));
        assert!(!is_auth_failure(&Error::Imap(
            "XOAUTH2 authenticate failed: io: Connection reset by peer (os error 104)".into()
        )));
    }
}
