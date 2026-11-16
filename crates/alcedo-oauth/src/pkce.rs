//! PKCE helpers (P5).

use oauth2::{PkceCodeChallenge, PkceCodeVerifier};

/// Generate a PKCE S256 challenge and verifier pair.
pub fn new_pkce_pair() -> (PkceCodeChallenge, PkceCodeVerifier) {
    PkceCodeChallenge::new_random_sha256()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_verifier_length_within_rfc7636() {
        let (_, verifier) = new_pkce_pair();
        let len = verifier.secret().len();
        assert!(len >= 43, "verifier too short: {len}");
        assert!(len <= 128, "verifier too long: {len}");
    }

    #[test]
    fn pkce_challenge_is_sha256_method() {
        let (challenge, _) = new_pkce_pair();
        assert_eq!(challenge.method().as_str(), "S256");
        assert!(!challenge.as_str().is_empty());
    }
}
