//! Data types shared by every OpenPGP backend.

use alcedo_core::PgpStatus;
use serde::{Deserialize, Serialize};

/// Parsed Autocrypt sender key from an incoming message.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutocryptPeer {
    pub email: String,
    pub fingerprint: String,
    pub armored: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeneratedCert {
    pub fingerprint: String,
    pub public_armored: String,
    pub secret_armored: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CertSummary {
    pub fingerprint: String,
    pub emails: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DecryptResult {
    #[serde(with = "crate::b64")]
    pub inner_rfc822: Vec<u8>,
    pub status: PgpStatus,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct SignKey {
    pub secret_armored: String,
    pub passphrase: String,
}

/// Never print the private key or its passphrase.
impl std::fmt::Debug for SignKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SignKey")
            .field("secret_armored", &"***")
            .field("passphrase", &"***")
            .finish()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProtectRequest {
    #[serde(with = "crate::b64")]
    pub inner_mime: Vec<u8>,
    pub sign: Option<SignKey>,
    pub encrypt_to: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PgpMimeKind {
    Signed,
    Encrypted,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PgpBodyParts {
    #[serde(with = "crate::b64")]
    pub inner_mime: Vec<u8>,
    #[serde(with = "crate::b64::option")]
    pub signature: Option<Vec<u8>>,
    #[serde(with = "crate::b64::option")]
    pub encrypted_payload: Option<Vec<u8>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProtectResult {
    pub mime_kind: PgpMimeKind,
    pub body_parts: PgpBodyParts,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_key_debug_hides_secrets() {
        let key = SignKey {
            secret_armored: "-----BEGIN PGP PRIVATE KEY BLOCK-----".into(),
            passphrase: "hunter2".into(),
        };
        let printed = format!("{key:?}");
        assert!(!printed.contains("PRIVATE KEY"));
        assert!(!printed.contains("hunter2"));
        assert!(printed.contains("***"));
    }

    #[test]
    fn request_debug_hides_sign_key_secrets() {
        let request = ProtectRequest {
            inner_mime: b"body".to_vec(),
            sign: Some(SignKey {
                secret_armored: "secret-block".into(),
                passphrase: "hunter2".into(),
            }),
            encrypt_to: vec![],
        };
        let printed = format!("{request:?}");
        assert!(!printed.contains("secret-block"));
        assert!(!printed.contains("hunter2"));
    }

    #[test]
    fn byte_fields_serialize_as_base64() {
        let parts = PgpBodyParts {
            inner_mime: b"hi".to_vec(),
            signature: Some(vec![0xff, 0x00]),
            encrypted_payload: None,
        };
        let json = serde_json::to_value(&parts).unwrap();
        assert_eq!(json["inner_mime"], "aGk=");
        assert_eq!(json["signature"], "/wA=");
        assert!(json["encrypted_payload"].is_null());
        let back: PgpBodyParts = serde_json::from_value(json).unwrap();
        assert_eq!(back.inner_mime, b"hi");
        assert_eq!(back.signature, Some(vec![0xff, 0x00]));
        assert_eq!(back.encrypted_payload, None);
    }

    #[test]
    fn invalid_base64_is_rejected() {
        let result: Result<DecryptResult, _> =
            serde_json::from_str(r#"{"inner_rfc822":"not base64!","status":{}}"#);
        assert!(result.is_err());
    }
}
