use alcedo_core::{PgpStatus, Result};
use alcedo_pgp_api::{
    AutocryptPeer, CertSummary, DecryptResult, GeneratedCert, PgpBackend, ProtectRequest,
    ProtectResult,
};

use crate::{autocrypt, generate, inspect, protect};

/// The in-process Sequoia + Nettle backend (statically linked; used by the free app).
pub struct SequoiaPgp;

impl PgpBackend for SequoiaPgp {
    fn generate(display_name: &str, email: &str, passphrase: &str) -> Result<GeneratedCert> {
        generate::generate(display_name, email, passphrase)
    }

    fn parse_public(armored: &str) -> Result<CertSummary> {
        generate::parse_public(armored)
    }

    fn parse_secret(armored: &str, passphrase: Option<&str>) -> Result<GeneratedCert> {
        generate::parse_secret(armored, passphrase)
    }

    fn verify(raw: &[u8], public_armoreds: &[String]) -> PgpStatus {
        inspect::verify(raw, public_armoreds)
    }

    fn decrypt(
        raw: &[u8],
        secret_armored: &str,
        passphrase: &str,
        extra_publics: &[String],
    ) -> Result<DecryptResult> {
        inspect::decrypt(raw, secret_armored, passphrase, extra_publics)
    }

    fn protect_outbound(request: ProtectRequest) -> Result<ProtectResult> {
        protect::protect_outbound(request)
    }

    fn autocrypt_header_value(
        addr: &str,
        public_armored: &str,
        prefer_encrypt_mutual: bool,
    ) -> Result<String> {
        autocrypt::autocrypt_header_value(addr, public_armored, prefer_encrypt_mutual)
    }

    fn parse_autocrypt_from_rfc822(raw: &[u8]) -> Option<AutocryptPeer> {
        autocrypt::parse_autocrypt_from_rfc822(raw)
    }

    fn autocrypt_peer_from_rfc822(raw: &[u8]) -> Option<AutocryptPeer> {
        autocrypt::autocrypt_peer_from_rfc822(raw)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alcedo_core::PgpSignatureState;
    use alcedo_pgp_api::SignKey;

    #[test]
    fn sequoia_backend_verifies_what_it_signed() {
        let cert = SequoiaPgp::generate("Alice", "alice@example.com", "alcedo-test").unwrap();
        let protected = SequoiaPgp::protect_outbound(ProtectRequest {
            inner_mime: b"Content-Type: text/plain; charset=utf-8\r\n\r\nHello\r\n".to_vec(),
            sign: Some(SignKey {
                secret_armored: cert.secret_armored.clone(),
                passphrase: "alcedo-test".into(),
            }),
            encrypt_to: vec![],
        })
        .unwrap();
        let parts = protected.body_parts;
        let signature = parts.signature.expect("signature");
        let mut raw = b"From: a@example.com\r\nMIME-Version: 1.0\r\nContent-Type: multipart/signed; protocol=\"application/pgp-signature\"; micalg=\"pgp-sha256\"; boundary=\"alcedo-test-boundary\"\r\n\r\n--alcedo-test-boundary\r\n".to_vec();
        raw.extend_from_slice(&parts.inner_mime);
        raw.extend_from_slice(
            b"\r\n--alcedo-test-boundary\r\nContent-Type: application/pgp-signature\r\n\r\n",
        );
        raw.extend_from_slice(&signature);
        if !signature.ends_with(b"\r\n") {
            raw.extend_from_slice(b"\r\n");
        }
        raw.extend_from_slice(b"--alcedo-test-boundary--\r\n");

        let status = SequoiaPgp::verify(&raw, std::slice::from_ref(&cert.public_armored));
        assert!(
            matches!(status.signature, PgpSignatureState::Valid { .. }),
            "got {:?}",
            status.signature
        );
    }
}
