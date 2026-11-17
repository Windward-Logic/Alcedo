//! Classify an incoming RFC822 message by its OpenPGP structure.
//!
//! Pure MIME inspection, no cryptography, so every backend shares it.

use mail_parser::{MessageParser, MimeHeaders};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum IncomingKind {
    None,
    SignedCleartext,
    SignedMime,
    EncryptedMime,
    InlineEncrypted,
}

pub fn inspect_incoming(raw_rfc822: &[u8]) -> IncomingKind {
    if let Some(msg) = MessageParser::default().parse(raw_rfc822) {
        if msg.is_content_type("multipart", "encrypted") {
            return IncomingKind::EncryptedMime;
        }
        if msg.is_content_type("multipart", "signed") {
            return IncomingKind::SignedMime;
        }
    }
    let text = String::from_utf8_lossy(raw_rfc822);
    if text.contains("-----BEGIN PGP SIGNED MESSAGE-----") {
        return IncomingKind::SignedCleartext;
    }
    if text.contains("-----BEGIN PGP MESSAGE-----") {
        return IncomingKind::InlineEncrypted;
    }
    IncomingKind::None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_message_is_none() {
        let raw = b"From: a@example.com\r\nSubject: hi\r\n\r\nhello\r\n";
        assert_eq!(inspect_incoming(raw), IncomingKind::None);
    }

    #[test]
    fn multipart_encrypted_is_detected() {
        let raw = b"Content-Type: multipart/encrypted; protocol=\"application/pgp-encrypted\"; boundary=\"b\"\r\n\r\n--b\r\n\r\nVersion: 1\r\n--b--\r\n";
        assert_eq!(inspect_incoming(raw), IncomingKind::EncryptedMime);
    }

    #[test]
    fn multipart_signed_is_detected() {
        let raw = b"Content-Type: multipart/signed; protocol=\"application/pgp-signature\"; boundary=\"b\"\r\n\r\n--b\r\n\r\nbody\r\n--b--\r\n";
        assert_eq!(inspect_incoming(raw), IncomingKind::SignedMime);
    }

    #[test]
    fn inline_armour_is_detected() {
        let signed = b"Subject: x\r\n\r\n-----BEGIN PGP SIGNED MESSAGE-----\r\nHash: SHA256\r\n";
        assert_eq!(inspect_incoming(signed), IncomingKind::SignedCleartext);
        let encrypted = b"Subject: x\r\n\r\n-----BEGIN PGP MESSAGE-----\r\n";
        assert_eq!(inspect_incoming(encrypted), IncomingKind::InlineEncrypted);
    }
}
