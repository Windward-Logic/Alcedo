//! Autocrypt Level 1 header encoding and parsing.

use alcedo_core::{Error, Result, canonical_email};
use mail_parser::MessageParser;
use sequoia_autocrypt::{AutocryptHeader, AutocryptHeaderType, AutocryptHeaders};
use sequoia_openpgp::cert::prelude::*;
use sequoia_openpgp::parse::Parse;
use sequoia_openpgp::policy::StandardPolicy;
use sequoia_openpgp::serialize::Serialize;

use crate::util::cert_fingerprint_hex;

/// Parsed Autocrypt sender key from an incoming message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AutocryptPeer {
    pub email: String,
    pub fingerprint: String,
    pub armored: String,
}

/// Build the Autocrypt header value (without the `Autocrypt:` name).
pub fn autocrypt_header_value(
    addr: &str,
    public_armored: &str,
    prefer_encrypt_mutual: bool,
) -> Result<String> {
    let cert =
        Cert::from_bytes(public_armored.as_bytes()).map_err(|e| Error::Pgp(e.to_string()))?;
    let policy = &StandardPolicy::new();
    let prefer = if prefer_encrypt_mutual {
        Some("mutual")
    } else {
        None
    };
    let header = AutocryptHeader::new_sender(policy, &cert, addr, prefer)
        .map_err(|e| Error::Pgp(e.to_string()))?;
    let mut buf = Vec::new();
    header
        .serialize(&mut buf)
        .map_err(|e| Error::Pgp(e.to_string()))?;
    String::from_utf8(buf).map_err(|e| Error::Pgp(format!("autocrypt header is not UTF-8: {e}")))
}

/// Parse the sender Autocrypt header from a full RFC822 message.
pub fn parse_autocrypt_from_rfc822(raw: &[u8]) -> Option<AutocryptPeer> {
    let parsed = AutocryptHeaders::from_bytes(raw).ok()?;
    let header = parsed
        .headers
        .iter()
        .find(|header| header.header_type == AutocryptHeaderType::Sender && header.key.is_some())?;
    let email = header.get("addr")?.value.trim().to_ascii_lowercase();
    if email.is_empty() {
        return None;
    }
    let cert = header.key.as_ref()?;
    let mut armored = Vec::new();
    cert.armored().serialize(&mut armored).ok()?;
    let armored = String::from_utf8(armored).ok()?;
    Some(AutocryptPeer {
        fingerprint: cert_fingerprint_hex(cert),
        email,
        armored,
    })
}

/// Like [`parse_autocrypt_from_rfc822`], but only when Autocrypt `addr` matches `From`.
pub fn autocrypt_peer_from_rfc822(raw: &[u8]) -> Option<AutocryptPeer> {
    let peer = parse_autocrypt_from_rfc822(raw)?;
    let parsed = MessageParser::default().parse(raw)?;
    let from = parsed.from()?.first()?.address()?;
    let from = canonical_email(from)?;
    let peer_addr = canonical_email(&peer.email)?;
    if from != peer_addr {
        return None;
    }
    Some(peer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generate::generate;

    #[test]
    fn roundtrip_autocrypt_header() {
        let generated = generate("Alice", "alice@example.com", "alcedo-test").expect("generate");
        let value = autocrypt_header_value("alice@example.com", &generated.public_armored, true)
            .expect("header value");
        assert!(value.contains("addr=alice@example.com"));
        assert!(value.contains("prefer-encrypt=mutual"));

        let mut raw = format!("From: Alice <alice@example.com>\r\nAutocrypt: {value}\r\n\r\n");
        let peer = parse_autocrypt_from_rfc822(raw.as_bytes()).expect("parse peer");
        assert_eq!(peer.email, "alice@example.com");
        assert_eq!(peer.fingerprint, generated.fingerprint);
        assert!(peer.armored.contains("BEGIN PGP PUBLIC KEY BLOCK"));

        raw = format!(
            "From: Alice <alice@example.com>\r\nAutocrypt: {}\r\n\r\n",
            autocrypt_header_value("alice@example.com", &generated.public_armored, false)
                .expect("nopref header")
        );
        let peer = parse_autocrypt_from_rfc822(raw.as_bytes()).expect("parse nopref");
        assert_eq!(peer.fingerprint, generated.fingerprint);
    }

    #[test]
    fn autocrypt_offer_rejects_mismatched_from() {
        let generated = generate("Alice", "alice@example.com", "alcedo-test").expect("generate");
        let value = autocrypt_header_value("victim@example.com", &generated.public_armored, false)
            .expect("header value");
        let raw = format!("From: Alice <alice@example.com>\r\nAutocrypt: {value}\r\n\r\n",);
        assert!(autocrypt_peer_from_rfc822(raw.as_bytes()).is_none());
        assert!(parse_autocrypt_from_rfc822(raw.as_bytes()).is_some());
    }

    #[test]
    fn autocrypt_offer_accepts_matching_from() {
        let generated = generate("Alice", "alice@example.com", "alcedo-test").expect("generate");
        let value = autocrypt_header_value("alice@example.com", &generated.public_armored, false)
            .expect("header value");
        let raw = format!("From: Alice <alice@example.com>\r\nAutocrypt: {value}\r\n\r\n",);
        let peer = autocrypt_peer_from_rfc822(raw.as_bytes()).expect("matching peer");
        assert_eq!(peer.email, "alice@example.com");
    }
}
