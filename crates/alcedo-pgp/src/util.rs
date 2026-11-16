//! Shared helpers for certs, fingerprints, and line endings.

use alcedo_core::{Error, Result};
use sequoia_openpgp::cert::prelude::*;
use sequoia_openpgp::parse::Parse;

pub fn cert_fingerprint_hex(cert: &Cert) -> String {
    cert.fingerprint().to_string()
}

pub fn emails_from_cert(cert: &Cert) -> Vec<String> {
    cert.userids()
        .map(|uid| uid.userid().to_string())
        .flat_map(|uid| extract_emails_from_userid(&uid))
        .collect()
}

pub fn extract_emails_from_userid(userid: &str) -> Vec<String> {
    let mut emails = Vec::new();
    let mut rest = userid;
    while let Some(start) = rest.find('<') {
        if let Some(end) = rest[start + 1..].find('>') {
            let email = rest[start + 1..start + 1 + end].trim();
            if email.contains('@') {
                emails.push(email.to_ascii_lowercase());
            }
            rest = &rest[start + 1 + end + 1..];
        } else {
            break;
        }
    }
    if emails.is_empty() && userid.contains('@') {
        emails.push(userid.trim().to_ascii_lowercase());
    }
    emails
}

pub fn armored_public_to_cert(armored: &str) -> Result<Cert> {
    Cert::from_bytes(armored.as_bytes()).map_err(|e| Error::Pgp(e.to_string()))
}

pub fn armored_secret_to_cert(armored: &str, passphrase: Option<&str>) -> Result<Cert> {
    let cert = Cert::from_bytes(armored.as_bytes()).map_err(|e| Error::Pgp(e.to_string()))?;
    if let Some(pass) = passphrase.filter(|p| !p.is_empty()) {
        let pass: sequoia_openpgp::crypto::Password = pass.into();
        if cert
            .keys()
            .secret()
            .next()
            .is_some_and(|ka| ka.key().has_secret())
        {
            let _ = cert
                .keys()
                .secret()
                .next()
                .expect("checked")
                .key()
                .clone()
                .decrypt_secret(&pass)
                .map_err(|e| Error::Pgp(e.to_string()))?;
        }
    }
    Ok(cert)
}

pub fn signing_keypair(
    cert: &Cert,
    passphrase: Option<&str>,
) -> Result<sequoia_openpgp::crypto::KeyPair> {
    use sequoia_openpgp::policy::StandardPolicy;
    let p = &StandardPolicy::new();
    let pass = passphrase
        .filter(|p| !p.is_empty())
        .ok_or_else(|| Error::Pgp("passphrase required for signing key".into()))?;
    let pass: sequoia_openpgp::crypto::Password = pass.into();
    let ka = cert
        .keys()
        .secret()
        .with_policy(p, None)
        .supported()
        .alive()
        .revoked(false)
        .for_signing()
        .next()
        .ok_or_else(|| Error::Pgp("no signing key found".into()))?;
    ka.key()
        .clone()
        .decrypt_secret(&pass)
        .map_err(|e| Error::Pgp(e.to_string()))?
        .into_keypair()
        .map_err(|e| Error::Pgp(e.to_string()))
}

/// RFC 3156 canonical line endings for detached signatures.
pub fn normalize_mime_crlf(data: &[u8]) -> Vec<u8> {
    let text = String::from_utf8_lossy(data);
    let mut out = String::new();
    for line in text.split_inclusive('\n') {
        let line = line.strip_suffix('\n').unwrap_or(line);
        let line = line.strip_suffix('\r').unwrap_or(line);
        out.push_str(line);
        out.push_str("\r\n");
    }
    out.into_bytes()
}
