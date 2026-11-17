//! Outbound signing and encryption.

use std::io::Write;

use alcedo_core::{Error, Result};
use sequoia_openpgp::policy::StandardPolicy;
use sequoia_openpgp::serialize::stream::{
    Armorer, Encryptor, LiteralWriter, Message, Recipient, Signer,
};
use sequoia_openpgp::types::{DataFormat, HashAlgorithm};

use crate::util::{
    armored_public_to_cert, armored_secret_to_cert, normalize_mime_crlf, signing_keypair,
};

pub use alcedo_pgp_api::{PgpBodyParts, PgpMimeKind, ProtectRequest, ProtectResult, SignKey};

pub fn protect_outbound(request: ProtectRequest) -> Result<ProtectResult> {
    let encrypt = !request.encrypt_to.is_empty();
    let sign = request.sign.is_some();

    if !sign && !encrypt {
        return Err(Error::Pgp("neither sign nor encrypt requested".into()));
    }

    let canonical = normalize_mime_crlf(&request.inner_mime);
    // RFC 3156 signs the first body part without the CRLF before the boundary
    // delimiter (RFC 2046: that line break belongs to the delimiter), and a
    // verifier reads it that way. Signing the part with its final CRLF made
    // every signature "BAD" in GnuPG and other clients.
    let signed_part = canonical
        .strip_suffix(b"\r\n")
        .unwrap_or(&canonical)
        .to_vec();
    let signature = if let Some(sign_key) = &request.sign {
        Some(detached_signature(&signed_part, sign_key)?)
    } else {
        None
    };

    if encrypt {
        let payload_source = if let Some(signature) = signature.as_ref() {
            build_signed_mime_bytes(&signed_part, signature)?
        } else {
            canonical.clone()
        };
        let certs: Result<Vec<_>> = request
            .encrypt_to
            .iter()
            .map(|armored| armored_public_to_cert(armored))
            .collect();
        let certs = certs?;
        if certs.is_empty() {
            return Err(Error::Pgp("no recipient public keys".into()));
        }
        let encrypted_payload = encrypt_bytes(&payload_source, &certs)?;
        return Ok(ProtectResult {
            mime_kind: PgpMimeKind::Encrypted,
            body_parts: PgpBodyParts {
                inner_mime: if request.sign.is_some() {
                    signed_part
                } else {
                    canonical
                },
                signature,
                encrypted_payload: Some(encrypted_payload),
            },
        });
    }

    Ok(ProtectResult {
        mime_kind: PgpMimeKind::Signed,
        body_parts: PgpBodyParts {
            inner_mime: signed_part,
            signature,
            encrypted_payload: None,
        },
    })
}

fn detached_signature(data: &[u8], sign_key: &SignKey) -> Result<Vec<u8>> {
    let cert = armored_secret_to_cert(&sign_key.secret_armored, Some(&sign_key.passphrase))?;
    let keypair = signing_keypair(&cert, Some(&sign_key.passphrase))?;
    let mut sig = Vec::new();
    {
        let message = Message::new(&mut sig);
        let mut message = Signer::new(message, keypair)
            .map_err(|e| Error::Pgp(e.to_string()))?
            .detached()
            .hash_algo(HashAlgorithm::SHA256)
            .map_err(|e| Error::Pgp(e.to_string()))?
            .build()
            .map_err(|e| Error::Pgp(e.to_string()))?;
        message
            .write_all(data)
            .map_err(|e| Error::Pgp(e.to_string()))?;
        message.finalize().map_err(|e| Error::Pgp(e.to_string()))?;
    }
    let mut armored = Vec::new();
    {
        let message = Message::new(&mut armored);
        // RFC 3156 wants "BEGIN PGP SIGNATURE"; the default armor kind is
        // "PGP MESSAGE", which stricter clients (and our own verifier) reject.
        let mut message = Armorer::new(message)
            .kind(sequoia_openpgp::armor::Kind::Signature)
            .build()
            .map_err(|e| Error::Pgp(e.to_string()))?;
        message
            .write_all(&sig)
            .map_err(|e| Error::Pgp(e.to_string()))?;
        message.finalize().map_err(|e| Error::Pgp(e.to_string()))?;
    }
    // A MIME body part: CRLF line endings (sequoia's armor writer emits LF).
    Ok(normalize_mime_crlf(&armored))
}

fn encrypt_bytes(plaintext: &[u8], certs: &[sequoia_openpgp::cert::Cert]) -> Result<Vec<u8>> {
    let p = &StandardPolicy::new();
    let recipients: Vec<Recipient<'_>> = certs
        .iter()
        .flat_map(|cert| {
            cert.keys()
                .with_policy(p, None)
                .supported()
                .alive()
                .revoked(false)
                .for_transport_encryption()
                .map(Recipient::from)
        })
        .collect();
    if recipients.is_empty() {
        return Err(Error::Pgp("no encryption-capable recipient keys".into()));
    }
    let mut ciphertext = Vec::new();
    {
        let message = Message::new(&mut ciphertext);
        let message = Encryptor::for_recipients(message, recipients)
            .build()
            .map_err(|e| Error::Pgp(e.to_string()))?;
        let mut writer = LiteralWriter::new(message)
            .format(DataFormat::Binary)
            .build()
            .map_err(|e| Error::Pgp(e.to_string()))?;
        writer
            .write_all(plaintext)
            .map_err(|e| Error::Pgp(e.to_string()))?;
        writer.finalize().map_err(|e| Error::Pgp(e.to_string()))?;
    }
    Ok(ciphertext)
}

/// The `multipart/signed` entity (RFC 3156 §6.1) that sign-then-encrypt
/// encrypts: its own `Content-Type` header, then `signed_part` verbatim.
///
/// Without the header, a recipient decrypts to an untyped body and shows the
/// boundaries and the signature as text, and nobody checks the signature.
/// `signed_part` ends without the CRLF that precedes the next delimiter (the
/// signature does not cover it), so that CRLF is always written here, even
/// when the part's own content ends in a line break.
fn build_signed_mime_bytes(signed_part: &[u8], signature: &[u8]) -> Result<Vec<u8>> {
    let boundary = format!("=_alcedo_{}", uuid::Uuid::new_v4().simple());
    let mut out = Vec::with_capacity(signed_part.len() + signature.len() + 384);
    // micalg matches the SHA-256 hash `detached_signature` signs with.
    write_crlf_line(
        &mut out,
        &format!(
            "Content-Type: multipart/signed; micalg=\"pgp-sha256\"; \
             protocol=\"application/pgp-signature\"; boundary=\"{boundary}\""
        ),
    )?;
    write_crlf_line(&mut out, "")?;
    write_crlf_line(&mut out, &format!("--{boundary}"))?;
    out.extend_from_slice(signed_part);
    out.extend_from_slice(b"\r\n");
    write_crlf_line(&mut out, &format!("--{boundary}"))?;
    write_crlf_line(
        &mut out,
        "Content-Type: application/pgp-signature; name=\"signature.asc\"",
    )?;
    write_crlf_line(&mut out, "Content-Description: OpenPGP digital signature")?;
    write_crlf_line(&mut out, "")?;
    out.extend_from_slice(signature);
    if !signature.ends_with(b"\r\n") {
        out.extend_from_slice(b"\r\n");
    }
    write_crlf_line(&mut out, &format!("--{boundary}--"))?;
    Ok(out)
}

fn write_crlf_line(out: &mut Vec<u8>, line: &str) -> Result<()> {
    out.extend_from_slice(line.as_bytes());
    out.extend_from_slice(b"\r\n");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generate::generate;

    #[test]
    fn sign_key_debug_output_hides_key_and_passphrase() {
        let key = SignKey {
            secret_armored: "PRIVATE-KEY-BLOCK".to_string(),
            passphrase: "hunter2".to_string(),
        };
        let printed = format!("{key:?}");
        assert!(!printed.contains("PRIVATE-KEY-BLOCK") && !printed.contains("hunter2"));
    }

    #[test]
    fn sign_then_encrypt_includes_self() {
        let alice = generate("Alice", "alice@example.com", "alcedo-test").unwrap();
        let bob = generate("Bob", "bob@example.com", "alcedo-test").unwrap();
        let inner = b"Content-Type: text/plain\r\n\r\nHello\r\n".to_vec();
        let result = protect_outbound(ProtectRequest {
            inner_mime: inner,
            sign: Some(SignKey {
                secret_armored: alice.secret_armored.clone(),
                passphrase: "alcedo-test".into(),
            }),
            encrypt_to: vec![bob.public_armored.clone(), alice.public_armored.clone()],
        })
        .expect("protect");
        assert_eq!(result.mime_kind, PgpMimeKind::Encrypted);
        let signature = result.body_parts.signature.as_ref().expect("signature");
        let signed_mime =
            build_signed_mime_bytes(&result.body_parts.inner_mime, signature).expect("signed mime");
        let signed_text = String::from_utf8_lossy(&signed_mime);
        assert!(signed_text.contains("\r\n--"));
        assert!(
            result
                .body_parts
                .encrypted_payload
                .as_ref()
                .is_some_and(|p| !p.is_empty())
        );
    }

    #[test]
    fn mail_encrypted_to_a_new_alcedo_key_uses_packets_gnupg_reads() {
        use sequoia_openpgp::{Packet, PacketPile, parse::Parse};

        let alice = generate("Alice", "alice@example.com", "alcedo-test").unwrap();
        let payload = protect_outbound(ProtectRequest {
            inner_mime: b"Content-Type: text/plain\r\n\r\nHello\r\n".to_vec(),
            sign: None,
            encrypt_to: vec![alice.public_armored.clone()],
        })
        .unwrap()
        .body_parts
        .encrypted_payload
        .expect("encrypted payload");
        // GnuPG 2.4 rejects RFC 9580 packets ("packet(1) with unknown version
        // 6"): the session key must be a v3 PKESK and the data SEIPD v1.
        let pile = PacketPile::from_bytes(&payload).expect("packets");
        let (mut saw_pkesk, mut saw_seip) = (false, false);
        for packet in pile.children() {
            match packet {
                Packet::PKESK(_) => {
                    saw_pkesk = true;
                    assert_eq!(packet.version(), Some(3), "PKESK version");
                }
                Packet::SEIP(_) => {
                    saw_seip = true;
                    assert_eq!(packet.version(), Some(1), "SEIPD version");
                }
                _ => {}
            }
        }
        assert!(
            saw_pkesk && saw_seip,
            "expected a PKESK and an SEIPD packet"
        );
    }

    #[test]
    fn detached_signature_armor_uses_crlf() {
        let alice = generate("Alice", "alice@example.com", "alcedo-test").unwrap();
        let protected = protect_outbound(ProtectRequest {
            inner_mime: b"Content-Type: text/plain\r\n\r\nHello\r\n".to_vec(),
            sign: Some(SignKey {
                secret_armored: alice.secret_armored.clone(),
                passphrase: "alcedo-test".into(),
            }),
            encrypt_to: vec![],
        })
        .unwrap();
        let signature = protected.body_parts.signature.expect("signature");
        assert!(signature.starts_with(b"-----BEGIN PGP SIGNATURE-----\r\n"));
        assert!(
            signature
                .iter()
                .enumerate()
                .all(|(i, &b)| b != b'\n' || (i > 0 && signature[i - 1] == b'\r')),
            "every LF in the armor must be preceded by CR"
        );
    }

    #[test]
    fn signed_mime_bytes_use_crlf_boundaries() {
        let inner = b"Content-Type: text/plain\r\n\r\nHello\r\n".to_vec();
        let signature = b"-----BEGIN PGP SIGNATURE-----\r\nabc\r\n-----END PGP SIGNATURE-----\r\n";
        let bytes = build_signed_mime_bytes(&inner, signature).unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("\r\n--"));
        assert!(text.starts_with("Content-Type: multipart/signed; micalg=\"pgp-sha256\"; "));
    }

    #[test]
    fn signed_mime_bytes_keep_a_part_ending_in_crlf_intact() {
        // The part's own final line break is signed data; the delimiter still
        // needs its own CRLF after it.
        let signed_part = b"Content-Type: text/plain\r\n\r\nHello\r\n";
        let signature = b"-----BEGIN PGP SIGNATURE-----\r\nabc\r\n-----END PGP SIGNATURE-----\r\n";
        let bytes = build_signed_mime_bytes(signed_part, signature).unwrap();
        let mut expected = signed_part.to_vec();
        expected.extend_from_slice(b"\r\n--=_alcedo_");
        assert!(bytes.windows(expected.len()).any(|w| w == expected));
    }

    #[test]
    fn encrypt_without_recipient_keys_fails() {
        let alice = generate("Alice", "alice@example.com", "alcedo-test").unwrap();
        let err = protect_outbound(ProtectRequest {
            inner_mime: b"test".to_vec(),
            sign: None,
            encrypt_to: vec!["not a key".into()],
        })
        .expect_err("bad key");
        assert!(err.to_string().contains("pgp"));

        let err = protect_outbound(ProtectRequest {
            inner_mime: b"test".to_vec(),
            sign: None,
            encrypt_to: vec![alice.public_armored],
        })
        .unwrap();
        assert_eq!(err.mime_kind, PgpMimeKind::Encrypted);
    }
}
