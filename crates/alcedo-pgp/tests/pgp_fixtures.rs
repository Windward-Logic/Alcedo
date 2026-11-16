//! Fixture-backed integration tests for sign/verify/decrypt.

use alcedo_pgp::{IncomingKind, decrypt, inspect_incoming, parse_public, parse_secret, verify};

const PASSPHRASE: &str = "alcedo-test";

#[test]
fn fixture_keys_roundtrip() {
    let public = include_str!("fixtures/test-public.asc");
    let secret = include_str!("fixtures/test-secret.asc");
    let summary = parse_public(public).expect("parse public fixture");
    assert!(
        summary
            .emails
            .iter()
            .any(|e| e.contains("testy@example.org"))
    );
    let imported = parse_secret(secret, Some(PASSPHRASE)).expect("parse secret fixture");
    assert_eq!(imported.fingerprint, summary.fingerprint);
}

/// Signed by GnuPG itself (`gpg --detach-sign` over the part without the CRLF
/// before the boundary), so this covers real-world mail and not only messages
/// this crate produced.
#[test]
fn fixture_gnupg_mime_signed_verifies() {
    let public = include_str!("fixtures/gpg-public.asc");
    let raw = include_bytes!("fixtures/mime-signed-gpg.eml");
    assert_eq!(inspect_incoming(raw), IncomingKind::SignedMime);
    match verify(raw, &[public.to_string()]).signature {
        alcedo_core::PgpSignatureState::Valid { .. } => {}
        other => panic!("expected valid signature from GnuPG, got {other:?}"),
    }

    let tampered = String::from_utf8_lossy(raw).replace("Hello from gpg", "Hello from eve");
    match verify(tampered.as_bytes(), &[public.to_string()]).signature {
        alcedo_core::PgpSignatureState::Invalid { .. } => {}
        other => panic!("expected a tampered message to be invalid, got {other:?}"),
    }
}

/// GnuPG-signed multipart/alternative: the signed part ends with the inner
/// closing delimiter line `--alt--\r\n`, so its final CRLF is signed data.
#[test]
fn fixture_gnupg_nested_mime_signed_verifies() {
    let public = include_str!("fixtures/gpg-nested-public.asc");
    let raw = include_bytes!("fixtures/mime-signed-gpg-nested.eml");
    assert_eq!(inspect_incoming(raw), IncomingKind::SignedMime);
    match verify(raw, &[public.to_string()]).signature {
        alcedo_core::PgpSignatureState::Valid { .. } => {}
        other => panic!("expected valid nested signature from GnuPG, got {other:?}"),
    }
    let tampered = String::from_utf8_lossy(raw)
        .replace("<p>Hello from gpg nested</p>", "<p>Hello from eve</p>");
    match verify(tampered.as_bytes(), &[public.to_string()]).signature {
        alcedo_core::PgpSignatureState::Invalid { .. } => {}
        other => panic!("expected tampered nested message to be invalid, got {other:?}"),
    }
}

#[test]
fn fixture_mime_signed_verifies() {
    let public = include_str!("fixtures/test-public.asc");
    let raw = include_bytes!("fixtures/mime-signed.eml");
    assert_eq!(inspect_incoming(raw), IncomingKind::SignedMime);
    let status = verify(raw, &[public.to_string()]);
    match status.signature {
        alcedo_core::PgpSignatureState::Valid { .. } => {}
        other => panic!("expected valid signature, got {other:?}"),
    }
}

#[test]
fn fixture_encrypted_decrypts() {
    let public = include_str!("fixtures/test-public.asc");
    let secret = include_str!("fixtures/test-secret.asc");
    let raw = include_bytes!("fixtures/encrypted.eml");
    assert_eq!(inspect_incoming(raw), IncomingKind::EncryptedMime);
    let decrypted = decrypt(raw, secret, PASSPHRASE, &[public.to_string()]).expect("decrypt");
    assert!(decrypted.status.decrypted);
    assert!(String::from_utf8_lossy(&decrypted.inner_rfc822).contains("Hello from fixture"));
}

#[test]
fn fixture_cleartext_signed_detected() {
    let raw = include_bytes!("fixtures/cleartext-signed.eml");
    assert_eq!(inspect_incoming(raw), IncomingKind::SignedCleartext);
}
