//! Key generation and armored parsing.

use alcedo_core::{Error, Result};
use sequoia_openpgp::Profile;
use sequoia_openpgp::cert::prelude::*;
use sequoia_openpgp::parse::Parse;
use sequoia_openpgp::serialize::Serialize;
use sequoia_openpgp::types::Features;

use crate::util::{armored_secret_to_cert, cert_fingerprint_hex, emails_from_cert};

pub use alcedo_pgp_api::{CertSummary, GeneratedCert};

/// Generate a new Ed25519 + Cv25519 key with the given user ID and passphrase.
pub fn generate(display_name: &str, email: &str, passphrase: &str) -> Result<GeneratedCert> {
    if passphrase.is_empty() {
        return Err(Error::Pgp("passphrase is required".into()));
    }
    let userid = if display_name.trim().is_empty() {
        format!("<{email}>")
    } else {
        format!("{} <{email}>", display_name.trim())
    };

    // A v4 key that advertises SEIPDv1 only. Sequoia's default also advertises
    // SEIPDv2, so mail encrypted to the key (by Alcedo, including the
    // encrypt-to-self copy) used RFC 9580 packets that GnuPG 2.4 cannot read.
    let (cert, _) = CertBuilder::general_purpose([userid])
        .set_profile(Profile::RFC4880)
        .and_then(|builder| builder.set_features(Features::empty().set_seipdv1()))
        .map_err(|e| Error::Pgp(e.to_string()))?
        .set_password(Some(passphrase.into()))
        .generate()
        .map_err(|e| Error::Pgp(e.to_string()))?;

    cert_to_generated(&cert)
}

pub fn parse_public(armored: &str) -> Result<CertSummary> {
    let cert = Cert::from_bytes(armored.as_bytes()).map_err(|e| Error::Pgp(e.to_string()))?;
    Ok(CertSummary {
        fingerprint: cert_fingerprint_hex(&cert),
        emails: emails_from_cert(&cert),
    })
}

pub fn parse_secret(armored: &str, passphrase: Option<&str>) -> Result<GeneratedCert> {
    let cert = armored_secret_to_cert(armored, passphrase)?;
    cert_to_generated(&cert)
}

fn cert_to_generated(cert: &Cert) -> Result<GeneratedCert> {
    let mut public_armored = Vec::new();
    cert.armored()
        .serialize(&mut public_armored)
        .map_err(|e| Error::Pgp(e.to_string()))?;
    let mut secret_armored = Vec::new();
    cert.as_tsk()
        .armored()
        .serialize(&mut secret_armored)
        .map_err(|e| Error::Pgp(e.to_string()))?;
    Ok(GeneratedCert {
        fingerprint: cert_fingerprint_hex(cert),
        public_armored: String::from_utf8(public_armored)
            .map_err(|e| Error::Pgp(format!("public key is not UTF-8: {e}")))?,
        secret_armored: String::from_utf8(secret_armored)
            .map_err(|e| Error::Pgp(format!("secret key is not UTF-8: {e}")))?,
    })
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Write;
    use std::path::PathBuf;

    use crate::protect::{ProtectRequest, SignKey, protect_outbound};

    use super::*;

    fn fixture_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
    }

    #[test]
    #[ignore = "run once to regenerate committed fixture files: cargo test -p alcedo-pgp write_fixture_files -- --ignored"]
    fn write_fixture_files() {
        let root = fixture_dir();
        fs::create_dir_all(&root).unwrap();
        let cert = generate("Testy McTestface", "testy@example.org", "alcedo-test").unwrap();
        fs::write(root.join("test-public.asc"), &cert.public_armored).unwrap();
        fs::write(root.join("test-secret.asc"), &cert.secret_armored).unwrap();

        let inner =
            b"Content-Type: text/plain; charset=utf-8\r\n\r\nHello from fixture\r\n".to_vec();
        let signed = protect_outbound(ProtectRequest {
            inner_mime: inner.clone(),
            sign: Some(SignKey {
                secret_armored: cert.secret_armored.clone(),
                passphrase: "alcedo-test".into(),
            }),
            encrypt_to: vec![],
        })
        .unwrap();
        let sig = signed.body_parts.signature.as_ref().unwrap();
        let boundary = "alcedo-fixture-signed";
        let mime_signed = format!(
            "From: Testy <testy@example.org>\r\nSubject: MIME signed fixture\r\nMIME-Version: 1.0\r\nContent-Type: multipart/signed; protocol=\"application/pgp-signature\"; micalg=\"pgp-sha256\"; boundary=\"{boundary}\"\r\n\r\n--{boundary}\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nHello from fixture\r\n--{boundary}\r\nContent-Type: application/pgp-signature\r\n\r\n",
        );
        let mut mime_signed_bytes = mime_signed.into_bytes();
        mime_signed_bytes.extend_from_slice(sig);
        mime_signed_bytes.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        fs::write(root.join("mime-signed.eml"), &mime_signed_bytes).unwrap();

        let encrypted = protect_outbound(ProtectRequest {
            inner_mime: inner,
            sign: None,
            encrypt_to: vec![cert.public_armored.clone()],
        })
        .unwrap();
        let payload = encrypted.body_parts.encrypted_payload.as_ref().unwrap();
        let enc = "From: Testy <testy@example.org>\r\nSubject: Encrypted fixture\r\nMIME-Version: 1.0\r\nContent-Type: multipart/encrypted; protocol=\"application/pgp-encrypted\"; boundary=\"alcedo-enc\"\r\n\r\n--alcedo-enc\r\nContent-Type: application/pgp-encrypted\r\n\r\nVersion: 1\r\n--alcedo-enc\r\nContent-Type: application/octet-stream\r\n\r\n".to_string();
        let mut enc_bytes = enc.into_bytes();
        enc_bytes.extend_from_slice(payload);
        enc_bytes.push(b'\n');
        enc_bytes.extend_from_slice(b"--alcedo-enc--\r\n");
        fs::write(root.join("encrypted.eml"), &enc_bytes).unwrap();

        let mut cleartext = Vec::new();
        writeln!(cleartext, "From: Testy <testy@example.org>").unwrap();
        writeln!(cleartext, "Subject: Cleartext signed fixture").unwrap();
        writeln!(cleartext).unwrap();
        writeln!(cleartext, "-----BEGIN PGP SIGNED MESSAGE-----").unwrap();
        writeln!(cleartext, "Hash: SHA256").unwrap();
        writeln!(cleartext).unwrap();
        writeln!(cleartext, "Hello from fixture").unwrap();
        writeln!(cleartext, "-----BEGIN PGP SIGNATURE-----").unwrap();
        writeln!(cleartext).unwrap();
        let sig_text = String::from_utf8(sig.clone()).unwrap();
        for line in sig_text.lines() {
            if line.starts_with("-----BEGIN PGP")
                || line.starts_with("-----END PGP")
                || line.is_empty()
            {
                continue;
            }
            writeln!(cleartext, "{line}").unwrap();
        }
        writeln!(cleartext, "-----END PGP SIGNATURE-----").unwrap();
        fs::write(root.join("cleartext-signed.eml"), &cleartext).unwrap();
    }

    #[test]
    fn generate_and_roundtrip() {
        let generated = generate("Alice", "alice@example.com", "alcedo-test").expect("generate");
        assert!(!generated.fingerprint.is_empty());
        assert!(
            generated
                .public_armored
                .contains("BEGIN PGP PUBLIC KEY BLOCK")
        );
        assert!(
            generated
                .secret_armored
                .contains("BEGIN PGP PRIVATE KEY BLOCK")
        );

        let summary = parse_public(&generated.public_armored).expect("parse public");
        assert_eq!(summary.fingerprint, generated.fingerprint);
        assert!(
            summary
                .emails
                .iter()
                .any(|e| e.contains("alice@example.com"))
        );

        let imported =
            parse_secret(&generated.secret_armored, Some("alcedo-test")).expect("parse secret");
        assert_eq!(imported.fingerprint, generated.fingerprint);
    }
}
