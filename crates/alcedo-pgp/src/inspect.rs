//! Inspect, verify, and decrypt incoming RFC822 / PGP messages.

use std::collections::HashMap;
use std::io::Read;
use std::sync::Arc;

use alcedo_core::{Error, PgpSignatureState, PgpStatus, Result};
use mail_parser::{Message, MessageParser, MimeHeaders};
use sequoia_openpgp::crypto::{KeyPair, SessionKey};
use sequoia_openpgp::parse::Parse;
use sequoia_openpgp::parse::stream::{
    DecryptionHelper, DecryptorBuilder, DetachedVerifierBuilder, GoodChecksum, MessageLayer,
    MessageStructure, VerificationHelper,
};
use sequoia_openpgp::policy::StandardPolicy;
use sequoia_openpgp::serialize::Serialize;
use sequoia_openpgp::types::SymmetricAlgorithm;
use sequoia_openpgp::{Cert, KeyHandle, KeyID};

use alcedo_core::crlf_line_endings;

use crate::util::{
    armored_public_to_cert, armored_secret_to_cert, cert_fingerprint_hex, normalize_mime_crlf,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IncomingKind {
    None,
    SignedCleartext,
    SignedMime,
    EncryptedMime,
    InlineEncrypted,
}

#[derive(Clone, Debug)]
pub struct DecryptResult {
    pub inner_rfc822: Vec<u8>,
    pub status: PgpStatus,
}

struct PublicKeyHelper {
    keys: Vec<Cert>,
    fingerprint: Option<String>,
    valid: bool,
    reason: Option<String>,
}

impl VerificationHelper for PublicKeyHelper {
    fn get_certs(&mut self, _ids: &[KeyHandle]) -> sequoia_openpgp::Result<Vec<Cert>> {
        Ok(self.keys.clone())
    }

    fn check(&mut self, structure: MessageStructure) -> sequoia_openpgp::Result<()> {
        for layer in structure.iter() {
            if let MessageLayer::SignatureGroup { results } = layer {
                for result in results {
                    match result {
                        Ok(GoodChecksum { ka, .. }) => {
                            self.fingerprint = Some(cert_fingerprint_hex(ka.cert()));
                            self.valid = true;
                        }
                        Err(err) => {
                            self.reason = Some(err.to_string());
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

struct DecryptHelper {
    keys: HashMap<KeyID, (Arc<Cert>, KeyPair)>,
    publics: Vec<Cert>,
    fingerprint: Option<String>,
    valid: bool,
    reason: Option<String>,
}

impl DecryptHelper {
    fn new(secret: Cert, passphrase: &str, publics: Vec<Cert>) -> Result<Self> {
        let pass: sequoia_openpgp::crypto::Password = passphrase.into();
        let p = &StandardPolicy::new();
        let mut keys = HashMap::new();
        let secret = Arc::new(secret);
        for ka in secret
            .keys()
            .secret()
            .with_policy(p, None)
            .supported()
            .alive()
            .revoked(false)
            .for_transport_encryption()
        {
            let key = ka
                .key()
                .clone()
                .decrypt_secret(&pass)
                .map_err(|e| Error::Pgp(e.to_string()))?;
            let pair = key.into_keypair().map_err(|e| Error::Pgp(e.to_string()))?;
            keys.insert(pair.public().keyid(), (secret.clone(), pair));
        }
        Ok(Self {
            keys,
            publics,
            fingerprint: None,
            valid: false,
            reason: None,
        })
    }
}

impl DecryptionHelper for DecryptHelper {
    fn decrypt(
        &mut self,
        pkesks: &[sequoia_openpgp::packet::PKESK],
        _skesks: &[sequoia_openpgp::packet::SKESK],
        sym_algo: Option<SymmetricAlgorithm>,
        decrypt: &mut dyn FnMut(Option<SymmetricAlgorithm>, &SessionKey) -> bool,
    ) -> sequoia_openpgp::Result<Option<Cert>> {
        let mut matched = None;
        for pkesk in pkesks {
            let key_id = KeyID::from(pkesk.recipient());
            let candidates: Vec<&mut (Arc<Cert>, KeyPair)> = if key_id.is_wildcard() {
                self.keys.values_mut().collect()
            } else if let Some(pair) = self.keys.get_mut(&key_id) {
                vec![pair]
            } else {
                continue;
            };
            for (cert, pair) in candidates {
                if pkesk
                    .decrypt(pair, sym_algo)
                    .map(|(algo, session_key)| decrypt(algo, &session_key))
                    .unwrap_or(false)
                {
                    matched = Some(cert.as_ref().clone());
                    break;
                }
            }
            if matched.is_some() {
                break;
            }
        }
        Ok(matched)
    }
}

impl VerificationHelper for DecryptHelper {
    fn get_certs(&mut self, _ids: &[KeyHandle]) -> sequoia_openpgp::Result<Vec<Cert>> {
        Ok(self.publics.clone())
    }

    fn check(&mut self, structure: MessageStructure) -> sequoia_openpgp::Result<()> {
        for layer in structure.iter() {
            if let MessageLayer::SignatureGroup { results } = layer {
                for result in results {
                    match result {
                        Ok(GoodChecksum { ka, .. }) => {
                            self.fingerprint = Some(cert_fingerprint_hex(ka.cert()));
                            self.valid = true;
                        }
                        Err(err) => {
                            self.reason = Some(err.to_string());
                        }
                    }
                }
            }
        }
        Ok(())
    }
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

pub fn verify(raw: &[u8], public_armoreds: &[String]) -> PgpStatus {
    let certs: Vec<Cert> = public_armoreds
        .iter()
        .filter_map(|armored| armored_public_to_cert(armored).ok())
        .collect();
    match inspect_incoming(raw) {
        IncomingKind::SignedCleartext => verify_cleartext(raw, &certs),
        IncomingKind::SignedMime => verify_mime_signed(raw, &certs),
        IncomingKind::EncryptedMime | IncomingKind::InlineEncrypted => PgpStatus {
            encrypted: true,
            decrypted: false,
            signature: PgpSignatureState::None,
        },
        IncomingKind::None => PgpStatus::default(),
    }
}

pub fn decrypt(
    raw: &[u8],
    secret_armored: &str,
    passphrase: &str,
    extra_publics: &[String],
) -> Result<DecryptResult> {
    let secret = armored_secret_to_cert(secret_armored, Some(passphrase))?;
    let publics: Vec<Cert> = extra_publics
        .iter()
        .filter_map(|a| armored_public_to_cert(a).ok())
        .collect();

    let kind = inspect_incoming(raw);
    let ciphertext = match kind {
        IncomingKind::EncryptedMime => extract_encrypted_payload(raw)?,
        IncomingKind::InlineEncrypted => {
            extract_armored_block(raw, "PGP MESSAGE").unwrap_or_else(|| raw.to_vec())
        }
        _ => return Err(Error::Pgp("message is not encrypted".into())),
    };

    let policy = StandardPolicy::new();
    let helper = DecryptHelper::new(secret, passphrase, publics)?;
    let mut plaintext = Vec::new();
    let mut decryptor = DecryptorBuilder::from_bytes(&ciphertext)
        .map_err(|e| Error::Pgp(e.to_string()))?
        .with_policy(&policy, None, helper)
        .map_err(|e| Error::Pgp(e.to_string()))?;
    decryptor
        .read_to_end(&mut plaintext)
        .map_err(|e| Error::Pgp(e.to_string()))?;
    let helper = decryptor.helper_ref();

    let inner_kind = inspect_incoming(&plaintext);
    let mut status = PgpStatus {
        encrypted: true,
        decrypted: true,
        signature: PgpSignatureState::None,
    };

    if helper.valid {
        status.signature = signature_from_helper(
            helper.fingerprint.as_deref(),
            helper.reason.as_deref(),
            true,
            &helper.publics,
            None,
        );
    } else if matches!(
        inner_kind,
        IncomingKind::SignedMime | IncomingKind::SignedCleartext
    ) {
        let public_armoreds: Vec<String> = helper
            .publics
            .iter()
            .filter_map(|c| {
                let mut buf = Vec::new();
                c.armored().serialize(&mut buf).ok()?;
                String::from_utf8(buf).ok()
            })
            .collect();
        let verified = verify(&plaintext, &public_armoreds);
        status.signature = verified.signature;
    }

    Ok(DecryptResult {
        inner_rfc822: plaintext,
        status,
    })
}

fn verify_cleartext(raw: &[u8], certs: &[Cert]) -> PgpStatus {
    let text = String::from_utf8_lossy(raw);
    let Some(body_start) = text.find("\n\n").or_else(|| text.find("\r\n\r\n")) else {
        return invalid_status("cleartext signed message missing body");
    };
    let body = text[body_start..].trim_start_matches(['\r', '\n']);
    let normalized = normalize_mime_crlf(body.as_bytes());
    let signature = extract_armored_block(raw, "PGP SIGNATURE");
    let Some(signature) = signature else {
        return invalid_status("cleartext signed message missing signature");
    };
    match verify_detached(&normalized, &signature, certs) {
        Ok((fp, valid, reason)) => PgpStatus {
            encrypted: false,
            decrypted: false,
            signature: signature_from_helper(fp.as_deref(), reason.as_deref(), valid, certs, None),
        },
        Err(reason) => invalid_status(&reason),
    }
}

fn verify_mime_signed(raw: &[u8], certs: &[Cert]) -> PgpStatus {
    let parsed = match MessageParser::default().parse(raw) {
        Some(msg) => msg,
        None => return invalid_status("invalid MIME message"),
    };
    let root = parsed.root_part();
    let Some(sub_ids) = root.sub_parts() else {
        return invalid_status("multipart/signed missing parts");
    };
    if sub_ids.len() < 2 {
        return invalid_status("multipart/signed missing parts");
    }
    let Some(inner_part) = parsed.part(sub_ids[0]) else {
        return invalid_status("multipart/signed missing inner part");
    };
    let Some(sig_part) = parsed.part(sub_ids[1]) else {
        return invalid_status("multipart/signed missing signature part");
    };
    let inner = part_raw(&parsed, inner_part);
    let sig_part_bytes = sig_part.contents();
    // mail-parser's part range already stops before the CRLF that belongs
    // to the boundary delimiter (RFC 2046), which is exactly what RFC 3156
    // signs. Only bare LFs (mail stored with Unix line endings) are fixed
    // up; no line break is added or removed.
    let signed_part = crlf_line_endings(inner);
    match verify_detached(&signed_part, sig_part_bytes, certs) {
        Ok((fp, valid, reason)) => PgpStatus {
            encrypted: false,
            decrypted: false,
            signature: signature_from_helper(fp.as_deref(), reason.as_deref(), valid, certs, None),
        },
        Err(reason) => invalid_status(&reason),
    }
}

fn verify_detached(
    data: &[u8],
    signature: &[u8],
    certs: &[Cert],
) -> std::result::Result<(Option<String>, bool, Option<String>), String> {
    let policy = StandardPolicy::new();
    let helper = PublicKeyHelper {
        keys: certs.to_vec(),
        fingerprint: None,
        valid: false,
        reason: None,
    };
    let mut verifier = DetachedVerifierBuilder::from_bytes(signature)
        .map_err(|e| e.to_string())?
        .with_policy(&policy, None, helper)
        .map_err(|e| e.to_string())?;
    verifier.verify_bytes(data).map_err(|e| e.to_string())?;
    let helper = verifier.helper_ref();
    Ok((
        helper.fingerprint.clone(),
        helper.valid,
        helper.reason.clone(),
    ))
}

fn signature_from_helper(
    fingerprint: Option<&str>,
    reason: Option<&str>,
    valid: bool,
    certs: &[Cert],
    signer_email: Option<String>,
) -> PgpSignatureState {
    let Some(fp) = fingerprint else {
        return if valid {
            PgpSignatureState::Valid {
                fingerprint: String::new(),
                signer_email,
            }
        } else {
            PgpSignatureState::Invalid {
                reason: reason.unwrap_or("signature invalid").to_string(),
            }
        };
    };
    let known = certs.iter().any(|c| cert_fingerprint_hex(c) == fp);
    if valid && known {
        PgpSignatureState::Valid {
            fingerprint: fp.to_string(),
            signer_email,
        }
    } else if valid {
        PgpSignatureState::UnknownKey {
            fingerprint: fp.to_string(),
        }
    } else {
        PgpSignatureState::Invalid {
            reason: reason.unwrap_or("signature invalid").to_string(),
        }
    }
}

fn invalid_status(reason: &str) -> PgpStatus {
    PgpStatus {
        encrypted: false,
        decrypted: false,
        signature: PgpSignatureState::Invalid {
            reason: reason.to_string(),
        },
    }
}

fn extract_armored_block(raw: &[u8], kind: &str) -> Option<Vec<u8>> {
    let text = String::from_utf8_lossy(raw);
    let begin = format!("-----BEGIN {kind}-----");
    let end = format!("-----END {kind}-----");
    let start = text.find(&begin)?;
    let rest = &text[start..];
    let end_pos = rest.find(&end)? + end.len();
    Some(rest.as_bytes()[..end_pos].to_vec())
}

fn extract_encrypted_payload(raw: &[u8]) -> Result<Vec<u8>> {
    let parsed = MessageParser::default()
        .parse(raw)
        .ok_or_else(|| Error::Pgp("invalid encrypted MIME".into()))?;
    if let Some(root) = parsed.root_part().sub_parts() {
        for id in root {
            let Some(part) = parsed.part(*id) else {
                continue;
            };
            if part.is_content_type("application", "octet-stream") {
                return Ok(part.contents().to_vec());
            }
        }
    }
    Err(Error::Pgp("encrypted MIME missing payload".into()))
}

fn part_raw<'x>(message: &'x Message<'x>, part: &mail_parser::MessagePart<'x>) -> &'x [u8] {
    let raw = message.raw_message();
    let start = part.raw_header_offset() as usize;
    let end = part.raw_end_offset() as usize;
    raw.get(start..end).unwrap_or(&[])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generate::generate;
    use crate::protect::{PgpBodyParts, ProtectRequest, SignKey, protect_outbound};

    #[test]
    fn sign_verify_roundtrip() {
        let cert = generate("Alice", "alice@example.com", "alcedo-test").unwrap();
        let inner = b"Content-Type: text/plain; charset=utf-8\r\n\r\nHello\r\n".to_vec();
        let protected = protect_outbound(ProtectRequest {
            inner_mime: inner.clone(),
            sign: Some(SignKey {
                secret_armored: cert.secret_armored.clone(),
                passphrase: "alcedo-test".into(),
            }),
            encrypt_to: vec![],
        })
        .unwrap();
        let sig = protected.body_parts.signature.expect("signature");
        // The signed data is the part without the CRLF before the delimiter.
        assert_eq!(
            protected.body_parts.inner_mime,
            normalize_mime_crlf(&inner)
                .strip_suffix(b"\r\n")
                .unwrap()
                .to_vec()
        );
        let status = verify_detached(
            &protected.body_parts.inner_mime,
            &sig,
            &[armored_public_to_cert(&cert.public_armored).unwrap()],
        )
        .unwrap();
        assert!(status.1);
    }

    #[test]
    fn outbound_signed_mime_verifies_as_received() {
        let cert = generate("Alice", "alice@example.com", "alcedo-test").unwrap();
        let inner = b"Content-Type: text/plain; charset=utf-8\r\n\r\nHello\r\n".to_vec();
        let protected = protect_outbound(ProtectRequest {
            inner_mime: inner,
            sign: Some(SignKey {
                secret_armored: cert.secret_armored.clone(),
                passphrase: "alcedo-test".into(),
            }),
            encrypt_to: vec![],
        })
        .unwrap();
        let parts = protected.body_parts;
        assert!(
            String::from_utf8_lossy(parts.signature.as_ref().unwrap())
                .starts_with("-----BEGIN PGP SIGNATURE-----")
        );
        let raw = signed_mime_as_sent(&parts);
        let status = verify(&raw, std::slice::from_ref(&cert.public_armored));
        assert!(
            matches!(status.signature, PgpSignatureState::Valid { .. }),
            "got {:?}",
            status.signature
        );
    }

    /// The message as Alcedo writes it: the signed part verbatim, then the CRLF
    /// that belongs to the delimiter, then the signature part.
    fn signed_mime_as_sent(parts: &PgpBodyParts) -> Vec<u8> {
        let signature = parts.signature.as_ref().expect("signature");
        let mut raw = b"From: a@example.com\r\nMIME-Version: 1.0\r\nContent-Type: multipart/signed; protocol=\"application/pgp-signature\"; micalg=\"pgp-sha256\"; boundary=\"alcedo-test-boundary\"\r\n\r\n--alcedo-test-boundary\r\n".to_vec();
        raw.extend_from_slice(&parts.inner_mime);
        raw.extend_from_slice(
            b"\r\n--alcedo-test-boundary\r\nContent-Type: application/pgp-signature\r\n\r\n",
        );
        raw.extend_from_slice(signature);
        if !signature.ends_with(b"\r\n") {
            raw.extend_from_slice(b"\r\n");
        }
        raw.extend_from_slice(b"--alcedo-test-boundary--\r\n");
        raw
    }

    #[test]
    fn signed_part_ending_in_a_line_break_verifies_as_received() {
        // What lettre emits for a plain body "Hello\n": the body's own CRLF,
        // then the CRLF that serialize_inner_body ends every part with.
        let cert = generate("Alice", "alice@example.com", "alcedo-test").unwrap();
        let inner = b"Content-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: 7bit\r\n\r\nHello\r\n\r\n".to_vec();
        let protected = protect_outbound(ProtectRequest {
            inner_mime: inner,
            sign: Some(SignKey {
                secret_armored: cert.secret_armored.clone(),
                passphrase: "alcedo-test".into(),
            }),
            encrypt_to: vec![],
        })
        .unwrap();
        let raw = signed_mime_as_sent(&protected.body_parts);
        let status = verify(&raw, std::slice::from_ref(&cert.public_armored));
        assert!(
            matches!(status.signature, PgpSignatureState::Valid { .. }),
            "got {:?}",
            status.signature
        );
    }

    #[test]
    fn encrypt_decrypt_roundtrip() {
        let cert = generate("Alice", "alice@example.com", "alcedo-test").unwrap();
        let inner = b"Content-Type: text/plain; charset=utf-8\r\n\r\nHello\r\n".to_vec();
        let protected = protect_outbound(ProtectRequest {
            inner_mime: inner,
            sign: None,
            encrypt_to: vec![cert.public_armored.clone()],
        })
        .unwrap();
        let payload = protected
            .body_parts
            .encrypted_payload
            .expect("encrypted payload");
        let wrapped = wrap_encrypted_mime(&payload);
        let decrypted = decrypt(
            &wrapped,
            &cert.secret_armored,
            "alcedo-test",
            &[cert.public_armored],
        )
        .expect("decrypt");
        assert!(decrypted.status.decrypted);
    }

    #[test]
    fn sign_then_encrypt_decrypts_to_a_verifiable_signed_entity() {
        let alice = generate("Alice", "alice@example.com", "alcedo-test").unwrap();
        let bob = generate("Bob", "bob@example.com", "alcedo-test").unwrap();
        // A part that ends without a line break, and one whose content does
        // (lettre's output for a plain body ending in "\n").
        for inner in [
            b"Content-Type: text/plain; charset=utf-8\r\n\r\nHello\r\n".to_vec(),
            b"Content-Type: text/plain; charset=utf-8\r\n\r\nHello\r\n\r\n".to_vec(),
        ] {
            let protected = protect_outbound(ProtectRequest {
                inner_mime: inner,
                sign: Some(SignKey {
                    secret_armored: alice.secret_armored.clone(),
                    passphrase: "alcedo-test".into(),
                }),
                encrypt_to: vec![bob.public_armored.clone(), alice.public_armored.clone()],
            })
            .unwrap();
            let payload = protected
                .body_parts
                .encrypted_payload
                .expect("encrypted payload");
            let decrypted = decrypt(
                &wrap_encrypted_mime(&payload),
                &bob.secret_armored,
                "alcedo-test",
                std::slice::from_ref(&alice.public_armored),
            )
            .expect("decrypt");
            assert_eq!(
                inspect_incoming(&decrypted.inner_rfc822),
                IncomingKind::SignedMime,
                "the decrypted entity must carry its multipart/signed header"
            );
            assert!(
                matches!(decrypted.status.signature, PgpSignatureState::Valid { .. }),
                "got {:?}",
                decrypted.status.signature
            );
        }
    }

    fn wrap_encrypted_mime(payload: &[u8]) -> Vec<u8> {
        let mut out = b"Content-Type: multipart/encrypted; protocol=\"application/pgp-encrypted\"; boundary=\"enc\"\r\n\r\n--enc\r\nContent-Type: application/pgp-encrypted\r\n\r\nVersion: 1\r\n--enc\r\nContent-Type: application/octet-stream\r\n\r\n".to_vec();
        out.extend_from_slice(payload);
        out.extend_from_slice(b"\r\n--enc--\r\n");
        out
    }

    #[test]
    fn extract_armored_block_from_inline_message() {
        let raw = b"From: a@example.com\r\n\r\n-----BEGIN PGP MESSAGE-----\nabc\n-----END PGP MESSAGE-----";
        let block = extract_armored_block(raw, "PGP MESSAGE").expect("armor block");
        let text = String::from_utf8_lossy(&block);
        assert!(text.starts_with("-----BEGIN PGP MESSAGE-----"));
        assert!(text.contains("abc"));
        assert!(!text.contains("From:"));
    }
}
