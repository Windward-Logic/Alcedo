//! XOAUTH2 SASL authenticator for `async-imap`.

use async_imap::Authenticator;
use base64::{Engine as _, engine::general_purpose::STANDARD};

/// Build the raw XOAUTH2 SASL string (before base64 encoding).
pub fn xoauth2_sasl_string(email: &str, access_token: &str) -> String {
    format!("user={email}\x01auth=Bearer {access_token}\x01\x01")
}

/// Base64-encoded XOAUTH2 initial client response.
pub fn encode_xoauth2(email: &str, access_token: &str) -> String {
    STANDARD.encode(xoauth2_sasl_string(email, access_token))
}

/// SASL authenticator for IMAP `AUTHENTICATE XOAUTH2`.
#[derive(Clone)]
pub struct Xoauth2Authenticator {
    email: String,
    access_token: String,
    called: bool,
}

impl std::fmt::Debug for Xoauth2Authenticator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Xoauth2Authenticator")
            .field("email", &self.email)
            .field("access_token", &"***")
            .field("called", &self.called)
            .finish()
    }
}

impl Xoauth2Authenticator {
    pub fn new(email: impl Into<String>, access_token: impl Into<String>) -> Self {
        Self {
            email: email.into(),
            access_token: access_token.into(),
            called: false,
        }
    }
}

impl Authenticator for Xoauth2Authenticator {
    type Response = Vec<u8>;

    fn process(&mut self, _challenge: &[u8]) -> Self::Response {
        if self.called {
            return Vec::new();
        }
        self.called = true;
        xoauth2_sasl_string(&self.email, &self.access_token).into_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xoauth2_encoding_matches_spec() {
        let email = "user@example.com";
        let token = "ya29.test-token";
        let raw = xoauth2_sasl_string(email, token);
        assert_eq!(
            raw,
            "user=user@example.com\x01auth=Bearer ya29.test-token\x01\x01"
        );

        let encoded = encode_xoauth2(email, token);
        assert_eq!(
            encoded,
            STANDARD.encode("user=user@example.com\x01auth=Bearer ya29.test-token\x01\x01")
        );
    }

    #[test]
    fn authenticator_returns_sasl_bytes() {
        let mut auth = Xoauth2Authenticator::new("alice@example.org", "tok123");
        let response = auth.process(b"");
        assert_eq!(
            String::from_utf8_lossy(&response),
            "user=alice@example.org\x01auth=Bearer tok123\x01\x01"
        );
    }

    #[test]
    fn authenticator_returns_empty_on_continuation() {
        let mut auth = Xoauth2Authenticator::new("alice@example.org", "tok123");
        assert!(!auth.process(b"").is_empty());
        assert!(auth.process(b"error").is_empty());
    }

    #[test]
    fn authenticator_debug_redacts_access_token() {
        let auth = Xoauth2Authenticator::new("alice@example.org", "ya29.secret-token");
        let debug = format!("{auth:?}");
        assert!(debug.contains("***"));
        assert!(!debug.contains("ya29.secret-token"));
        assert!(debug.contains("alice@example.org"));
    }
}
