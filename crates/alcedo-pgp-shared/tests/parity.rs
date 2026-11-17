//! Parity between the run-time loaded library (`SharedPgp`) and the static
//! Sequoia backend (`SequoiaPgp`): same results, same error text, and messages
//! produced by one are readable by the other.
//!
//! Never `use alcedo_pgp_ffi` here: it is a dev-dependency only so cargo builds
//! the shared library, and linking it would bypass the loader.

use std::path::Path;

use alcedo_core::{PgpSignatureState, Result};
use alcedo_pgp::SequoiaPgp;
use alcedo_pgp_shared::wire::ABI_VERSION;
use alcedo_pgp_shared::{
    PgpBackend, PgpBodyParts, PgpStatus, ProtectRequest, SharedPgp, SignKey, library_info,
};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../alcedo-pgp/tests/fixtures");
const PASSPHRASE: &str = "alcedo-test";
const INNER_MIME: &[u8] = b"Content-Type: text/plain; charset=utf-8\r\n\r\nHello parity\r\n";

fn fixture_bytes(name: &str) -> Vec<u8> {
    let path = Path::new(FIXTURES).join(name);
    std::fs::read(&path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()))
}

fn fixture(name: &str) -> String {
    String::from_utf8(fixture_bytes(name)).expect("fixture is UTF-8")
}

/// Makes results comparable by turning the error into the text a user sees.
fn comparable<T>(result: Result<T>) -> std::result::Result<T, String> {
    result.map_err(|err| err.to_string())
}

fn error_text<T: std::fmt::Debug>(result: Result<T>) -> String {
    result.expect_err("expected an error").to_string()
}

fn assert_valid(status: &PgpStatus) {
    assert!(
        matches!(status.signature, PgpSignatureState::Valid { .. }),
        "got {:?}",
        status.signature
    );
}

fn fixture_sign_key() -> SignKey {
    SignKey {
        secret_armored: fixture("test-secret.asc"),
        passphrase: PASSPHRASE.into(),
    }
}

/// The message as Alcedo sends it (see `crates/alcedo-pgp/src/inspect.rs`).
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

fn wrap_encrypted_mime(payload: &[u8]) -> Vec<u8> {
    let mut out = b"Content-Type: multipart/encrypted; protocol=\"application/pgp-encrypted\"; boundary=\"enc\"\r\n\r\n--enc\r\nContent-Type: application/pgp-encrypted\r\n\r\nVersion: 1\r\n--enc\r\nContent-Type: application/octet-stream\r\n\r\n".to_vec();
    out.extend_from_slice(payload);
    out.extend_from_slice(b"\r\n--enc--\r\n");
    out
}

fn gpg_signed_tampered() -> Vec<u8> {
    String::from_utf8_lossy(&fixture_bytes("mime-signed-gpg.eml"))
        .replace("Hello from gpg", "Hello from eve")
        .into_bytes()
}

#[test]
fn library_loads_with_expected_abi() {
    let info = library_info().expect("library loads");
    assert_eq!(info.abi_version, ABI_VERSION);
    assert!(info.path.is_file(), "{}", info.path.display());
}

/// The relink script sets `ALCEDO_PGP_EXPECT_BUILD_ID` to prove its own build
/// of the library is the one that loaded.
#[test]
fn reports_expected_build_id_when_requested() {
    let expected =
        std::env::var("ALCEDO_PGP_EXPECT_BUILD_ID").unwrap_or_else(|_| "unmodified".into());
    assert_eq!(library_info().expect("library loads").build_id, expected);
}

#[test]
fn parse_public_matches_static() {
    for name in ["test-public.asc", "gpg-public.asc", "gpg-nested-public.asc"] {
        let armored = fixture(name);
        assert_eq!(
            SharedPgp::parse_public(&armored).expect(name),
            SequoiaPgp::parse_public(&armored).expect(name),
            "{name}"
        );
    }
    assert_eq!(
        error_text(SharedPgp::parse_public("garbage")),
        error_text(SequoiaPgp::parse_public("garbage"))
    );
}

#[test]
fn parse_secret_matches_static() {
    let secret = fixture("test-secret.asc");
    assert_eq!(
        SharedPgp::parse_secret(&secret, Some(PASSPHRASE)).expect("shared"),
        SequoiaPgp::parse_secret(&secret, Some(PASSPHRASE)).expect("static")
    );
    assert_eq!(
        error_text(SharedPgp::parse_secret(&secret, Some("wrong"))),
        error_text(SequoiaPgp::parse_secret(&secret, Some("wrong")))
    );
    assert_eq!(
        comparable(SharedPgp::parse_secret(&secret, None)),
        comparable(SequoiaPgp::parse_secret(&secret, None))
    );
}

#[test]
fn verify_matches_static() {
    let cases = [
        (
            "mime-signed.eml",
            fixture_bytes("mime-signed.eml"),
            "test-public.asc",
        ),
        (
            "mime-signed-gpg.eml",
            fixture_bytes("mime-signed-gpg.eml"),
            "gpg-public.asc",
        ),
        (
            "mime-signed-gpg-nested.eml",
            fixture_bytes("mime-signed-gpg-nested.eml"),
            "gpg-nested-public.asc",
        ),
        (
            "cleartext-signed.eml",
            fixture_bytes("cleartext-signed.eml"),
            "test-public.asc",
        ),
        (
            "tampered mime-signed-gpg.eml",
            gpg_signed_tampered(),
            "gpg-public.asc",
        ),
    ];
    for (label, raw, key) in cases {
        let publics = [fixture(key)];
        assert_eq!(
            SharedPgp::verify(&raw, &publics),
            SequoiaPgp::verify(&raw, &publics),
            "{label}"
        );
    }
}

#[test]
fn decrypt_matches_static() {
    let raw = fixture_bytes("encrypted.eml");
    let secret = fixture("test-secret.asc");
    let publics = [fixture("test-public.asc")];
    let shared = SharedPgp::decrypt(&raw, &secret, PASSPHRASE, &publics).expect("shared");
    let local = SequoiaPgp::decrypt(&raw, &secret, PASSPHRASE, &publics).expect("static");
    assert_eq!(shared.inner_rfc822, local.inner_rfc822);
    assert_eq!(shared.status, local.status);
    assert_eq!(
        error_text(SharedPgp::decrypt(&raw, &secret, "wrong", &publics)),
        error_text(SequoiaPgp::decrypt(&raw, &secret, "wrong", &publics))
    );
}

#[test]
fn protect_outbound_interoperates() {
    let public = fixture("test-public.asc");
    let secret = fixture("test-secret.asc");
    let publics = std::slice::from_ref(&public);

    let signed = SharedPgp::protect_outbound(ProtectRequest {
        inner_mime: INNER_MIME.to_vec(),
        sign: Some(fixture_sign_key()),
        encrypt_to: vec![],
    })
    .expect("shared sign");
    assert_valid(&SequoiaPgp::verify(
        &signed_mime_as_sent(&signed.body_parts),
        publics,
    ));

    let encrypted = SharedPgp::protect_outbound(ProtectRequest {
        inner_mime: INNER_MIME.to_vec(),
        sign: None,
        encrypt_to: vec![public.clone()],
    })
    .expect("shared encrypt");
    let payload = encrypted.body_parts.encrypted_payload.expect("payload");
    let decrypted =
        SequoiaPgp::decrypt(&wrap_encrypted_mime(&payload), &secret, PASSPHRASE, publics)
            .expect("static decrypt");
    assert!(decrypted.status.decrypted);
    assert!(String::from_utf8_lossy(&decrypted.inner_rfc822).contains("Hello parity"));

    let signed_and_encrypted = SequoiaPgp::protect_outbound(ProtectRequest {
        inner_mime: INNER_MIME.to_vec(),
        sign: Some(fixture_sign_key()),
        encrypt_to: vec![public.clone()],
    })
    .expect("static sign+encrypt");
    let payload = signed_and_encrypted
        .body_parts
        .encrypted_payload
        .expect("payload");
    let decrypted =
        SharedPgp::decrypt(&wrap_encrypted_mime(&payload), &secret, PASSPHRASE, publics)
            .expect("shared decrypt");
    assert!(decrypted.status.decrypted);
    assert_valid(&decrypted.status);
}

#[test]
fn generate_interoperates() {
    let generated = SharedPgp::generate("Ada", "ada@example.com", "parity-pass").expect("generate");
    let parsed = SequoiaPgp::parse_secret(&generated.secret_armored, Some("parity-pass"))
        .expect("static parse");
    assert_eq!(parsed.fingerprint, generated.fingerprint);
}

#[test]
fn autocrypt_matches_static() {
    let public = fixture("test-public.asc");
    let fingerprint = SequoiaPgp::parse_public(&public)
        .expect("parse")
        .fingerprint;

    let value =
        SharedPgp::autocrypt_header_value("testy@example.org", &public, true).expect("header");
    let raw = format!("From: Testy <testy@example.org>\r\nAutocrypt: {value}\r\n\r\n");
    let local_peer = SequoiaPgp::parse_autocrypt_from_rfc822(raw.as_bytes()).expect("static peer");
    assert_eq!(local_peer.fingerprint, fingerprint);
    assert_eq!(local_peer.email, "testy@example.org");
    assert_eq!(
        SharedPgp::parse_autocrypt_from_rfc822(raw.as_bytes()),
        Some(local_peer.clone())
    );
    assert_eq!(
        SharedPgp::autocrypt_peer_from_rfc822(raw.as_bytes()),
        SequoiaPgp::autocrypt_peer_from_rfc822(raw.as_bytes())
    );

    let offer =
        SharedPgp::autocrypt_header_value("victim@example.org", &public, false).expect("header");
    let mismatched = format!("From: Testy <testy@example.org>\r\nAutocrypt: {offer}\r\n\r\n");
    assert_eq!(
        SharedPgp::autocrypt_peer_from_rfc822(mismatched.as_bytes()),
        None
    );
    assert_eq!(
        SequoiaPgp::autocrypt_peer_from_rfc822(mismatched.as_bytes()),
        None
    );
}

#[test]
fn errors_match_static() {
    let request = || ProtectRequest {
        inner_mime: INNER_MIME.to_vec(),
        sign: None,
        encrypt_to: vec!["not a key".into()],
    };
    assert_eq!(
        error_text(SharedPgp::protect_outbound(request())),
        error_text(SequoiaPgp::protect_outbound(request()))
    );
}

#[test]
fn large_payload_roundtrip() {
    const BODY_SIZE: usize = 5 * 1024 * 1024;
    let line = b"The quick brown fox jumps over the lazy dog 0123456789.\r\n";
    let mut inner = b"Content-Type: text/plain; charset=utf-8\r\n\r\n".to_vec();
    while inner.len() < BODY_SIZE {
        inner.extend_from_slice(line);
    }
    let public = fixture("test-public.asc");
    let encrypted = SharedPgp::protect_outbound(ProtectRequest {
        inner_mime: inner.clone(),
        sign: None,
        encrypt_to: vec![public.clone()],
    })
    .expect("shared encrypt");
    let payload = encrypted.body_parts.encrypted_payload.expect("payload");
    let decrypted = SharedPgp::decrypt(
        &wrap_encrypted_mime(&payload),
        &fixture("test-secret.asc"),
        PASSPHRASE,
        &[public],
    )
    .expect("shared decrypt");
    assert!(decrypted.status.decrypted);
    assert_eq!(decrypted.inner_rfc822.len(), inner.len());
    assert!(decrypted.inner_rfc822 == inner, "decrypted body differs");
}

#[test]
fn concurrent_calls_are_safe() {
    let raw = fixture_bytes("mime-signed-gpg.eml");
    let publics = [fixture("gpg-public.asc")];
    std::thread::scope(|scope| {
        for _ in 0..8 {
            scope.spawn(|| {
                for _ in 0..20 {
                    assert_valid(&SharedPgp::verify(&raw, &publics));
                }
            });
        }
    });
}
