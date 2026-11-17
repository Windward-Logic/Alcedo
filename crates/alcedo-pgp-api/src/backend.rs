//! The trait every OpenPGP implementation satisfies.

use alcedo_core::{PgpStatus, Result};

use crate::types::{
    AutocryptPeer, CertSummary, DecryptResult, GeneratedCert, ProtectRequest, ProtectResult,
};

/// One OpenPGP implementation. Associated functions only: a backend is chosen
/// at compile time and holds no per-call state.
///
/// Every error is `alcedo_core::Error::Pgp(message)`, and every backend must
/// produce the same message for the same failure so callers cannot tell which
/// backend is linked.
pub trait PgpBackend {
    fn generate(display_name: &str, email: &str, passphrase: &str) -> Result<GeneratedCert>;
    fn parse_public(armored: &str) -> Result<CertSummary>;
    fn parse_secret(armored: &str, passphrase: Option<&str>) -> Result<GeneratedCert>;
    fn verify(raw: &[u8], public_armoreds: &[String]) -> PgpStatus;
    fn decrypt(
        raw: &[u8],
        secret_armored: &str,
        passphrase: &str,
        extra_publics: &[String],
    ) -> Result<DecryptResult>;
    fn protect_outbound(request: ProtectRequest) -> Result<ProtectResult>;
    fn autocrypt_header_value(
        addr: &str,
        public_armored: &str,
        prefer_encrypt_mutual: bool,
    ) -> Result<String>;
    fn parse_autocrypt_from_rfc822(raw: &[u8]) -> Option<AutocryptPeer>;
    fn autocrypt_peer_from_rfc822(raw: &[u8]) -> Option<AutocryptPeer>;
}
