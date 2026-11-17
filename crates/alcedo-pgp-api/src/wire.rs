//! The C ABI between `alcedo-pgp-shared` (the client inside the app) and
//! `alcedo-pgp-ffi` (the replaceable shared library that links Sequoia).
//!
//! One entry point carries every operation: the client sends a JSON
//! [`Request`], the library answers with a JSON [`Response`] in a buffer it
//! allocated, and the client hands that buffer back to the library's free
//! function. Bump [`ABI_VERSION`] on any incompatible change to these types,
//! the symbols or the buffer rules.

use std::ffi::c_char;

use alcedo_core::PgpStatus;
use serde::{Deserialize, Serialize};

use crate::types::{
    AutocryptPeer, CertSummary, DecryptResult, GeneratedCert, ProtectRequest, ProtectResult,
};

pub const ABI_VERSION: u32 = 1;

/// File name cargo gives the cdylib.
pub const LIBRARY_FILE_NAME: &str = "libalcedo_pgp_ffi.so";
/// File name the packages install (SONAME carries the ABI version).
pub const INSTALLED_LIBRARY_FILE_NAME: &str = "libalcedo_pgp_ffi.so.1";
/// Environment variable holding an explicit path to the library.
pub const LIBRARY_PATH_ENV: &str = "ALCEDO_PGP_LIB";

pub const SYM_ABI_VERSION: &[u8] = b"alcedo_pgp_ffi_abi_version\0";
pub const SYM_BUILD_ID: &[u8] = b"alcedo_pgp_ffi_build_id\0";
pub const SYM_CALL: &[u8] = b"alcedo_pgp_ffi_call\0";
pub const SYM_FREE: &[u8] = b"alcedo_pgp_ffi_free\0";

/// A byte buffer owned by the library. Only the library's free function may
/// release it, because the client and library may use different allocators.
#[repr(C)]
#[derive(Debug)]
pub struct FfiBuffer {
    pub ptr: *mut u8,
    pub len: usize,
    pub cap: usize,
}

impl FfiBuffer {
    pub const EMPTY: FfiBuffer = FfiBuffer {
        ptr: std::ptr::null_mut(),
        len: 0,
        cap: 0,
    };
}

pub type AbiVersionFn = unsafe extern "C" fn() -> u32;
/// Returns a static, NUL-terminated ASCII string identifying the build.
pub type BuildIdFn = unsafe extern "C" fn() -> *const c_char;
pub type CallFn =
    unsafe extern "C" fn(request: *const u8, request_len: usize, out: *mut FfiBuffer) -> i32;
pub type FreeFn = unsafe extern "C" fn(buffer: FfiBuffer);

/// `out` holds a JSON [`Response`].
pub const CALL_OK: i32 = 0;
/// A pointer was null or the request JSON did not parse; `out` holds a UTF-8
/// message or is empty.
pub const CALL_BAD_REQUEST: i32 = 1;
/// The library caught a panic; `out` holds a UTF-8 message or is empty.
pub const CALL_PANIC: i32 = 2;

/// One variant per [`crate::PgpBackend`] function, with the same argument names.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    Generate {
        display_name: String,
        email: String,
        passphrase: String,
    },
    ParsePublic {
        armored: String,
    },
    ParseSecret {
        armored: String,
        passphrase: Option<String>,
    },
    Verify {
        #[serde(with = "crate::b64")]
        raw: Vec<u8>,
        public_armoreds: Vec<String>,
    },
    Decrypt {
        #[serde(with = "crate::b64")]
        raw: Vec<u8>,
        secret_armored: String,
        passphrase: String,
        extra_publics: Vec<String>,
    },
    ProtectOutbound {
        request: ProtectRequest,
    },
    AutocryptHeaderValue {
        addr: String,
        public_armored: String,
        prefer_encrypt_mutual: bool,
    },
    ParseAutocryptFromRfc822 {
        #[serde(with = "crate::b64")]
        raw: Vec<u8>,
    },
    AutocryptPeerFromRfc822 {
        #[serde(with = "crate::b64")]
        raw: Vec<u8>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum Reply {
    GeneratedCert(GeneratedCert),
    CertSummary(CertSummary),
    PgpStatus(PgpStatus),
    DecryptResult(DecryptResult),
    ProtectResult(ProtectResult),
    Text(String),
    AutocryptPeer(Option<AutocryptPeer>),
}

/// `Err` carries the inner message of `Error::Pgp`; the client rebuilds
/// `Error::Pgp(message)`, so error text matches the static backend exactly.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Response {
    Ok(Reply),
    Err(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{PgpBodyParts, PgpMimeKind, SignKey};
    use alcedo_core::PgpSignatureState;

    fn round_trip_request(request: &Request) -> Request {
        let json = serde_json::to_string(request).unwrap();
        serde_json::from_str(&json).unwrap()
    }

    fn round_trip_response(response: &Response) -> Response {
        let json = serde_json::to_string(response).unwrap();
        serde_json::from_str(&json).unwrap()
    }

    fn all_requests() -> Vec<Request> {
        vec![
            Request::Generate {
                display_name: "Ada".into(),
                email: "ada@example.com".into(),
                passphrase: "pw".into(),
            },
            Request::ParsePublic {
                armored: "pub".into(),
            },
            Request::ParseSecret {
                armored: "sec".into(),
                passphrase: None,
            },
            Request::ParseSecret {
                armored: "sec".into(),
                passphrase: Some("pw".into()),
            },
            Request::Verify {
                raw: vec![0, 1, 2, 255],
                public_armoreds: vec!["a".into(), "b".into()],
            },
            Request::Decrypt {
                raw: b"msg".to_vec(),
                secret_armored: "sec".into(),
                passphrase: "pw".into(),
                extra_publics: vec![],
            },
            Request::ProtectOutbound {
                request: ProtectRequest {
                    inner_mime: b"body\r\n".to_vec(),
                    sign: Some(SignKey {
                        secret_armored: "sec".into(),
                        passphrase: "pw".into(),
                    }),
                    encrypt_to: vec!["pub".into()],
                },
            },
            Request::AutocryptHeaderValue {
                addr: "ada@example.com".into(),
                public_armored: "pub".into(),
                prefer_encrypt_mutual: true,
            },
            Request::ParseAutocryptFromRfc822 {
                raw: b"Autocrypt: x".to_vec(),
            },
            Request::AutocryptPeerFromRfc822 { raw: vec![] },
        ]
    }

    #[test]
    fn every_request_variant_round_trips() {
        for request in all_requests() {
            let before = serde_json::to_value(&request).unwrap();
            let after = serde_json::to_value(round_trip_request(&request)).unwrap();
            assert_eq!(before, after, "{before}");
        }
    }

    #[test]
    fn request_tags_use_snake_case_ops() {
        let ops: Vec<String> = all_requests()
            .iter()
            .map(|r| {
                serde_json::to_value(r).unwrap()["op"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect();
        for expected in [
            "generate",
            "parse_public",
            "parse_secret",
            "verify",
            "decrypt",
            "protect_outbound",
            "autocrypt_header_value",
            "parse_autocrypt_from_rfc822",
            "autocrypt_peer_from_rfc822",
        ] {
            assert!(ops.iter().any(|op| op == expected), "missing op {expected}");
        }
    }

    #[test]
    fn request_raw_bytes_travel_as_base64() {
        let json = serde_json::to_value(Request::Verify {
            raw: b"hi".to_vec(),
            public_armoreds: vec![],
        })
        .unwrap();
        assert_eq!(json["raw"], "aGk=");
        match round_trip_request(&Request::Verify {
            raw: vec![0xff, 0x00, 0x7f],
            public_armoreds: vec![],
        }) {
            Request::Verify { raw, .. } => assert_eq!(raw, vec![0xff, 0x00, 0x7f]),
            other => panic!("wrong variant {other:?}"),
        }
    }

    fn all_replies() -> Vec<Reply> {
        let status = PgpStatus {
            signature: PgpSignatureState::Valid {
                fingerprint: "FP".into(),
                signer_email: Some("ada@example.com".into()),
            },
            ..PgpStatus::default()
        };
        vec![
            Reply::GeneratedCert(GeneratedCert {
                fingerprint: "FP".into(),
                public_armored: "pub".into(),
                secret_armored: "sec".into(),
            }),
            Reply::CertSummary(CertSummary {
                fingerprint: "FP".into(),
                emails: vec!["ada@example.com".into()],
            }),
            Reply::PgpStatus(status.clone()),
            Reply::DecryptResult(DecryptResult {
                inner_rfc822: vec![1, 2, 3],
                status,
            }),
            Reply::ProtectResult(ProtectResult {
                mime_kind: PgpMimeKind::Encrypted,
                body_parts: PgpBodyParts {
                    inner_mime: b"body".to_vec(),
                    signature: None,
                    encrypted_payload: Some(b"ct".to_vec()),
                },
            }),
            Reply::Text("Autocrypt value".into()),
            Reply::AutocryptPeer(None),
            Reply::AutocryptPeer(Some(AutocryptPeer {
                email: "ada@example.com".into(),
                fingerprint: "FP".into(),
                armored: "pub".into(),
            })),
        ]
    }

    #[test]
    fn every_reply_variant_round_trips_inside_ok() {
        for reply in all_replies() {
            let response = Response::Ok(reply);
            let before = serde_json::to_value(&response).unwrap();
            let after = serde_json::to_value(round_trip_response(&response)).unwrap();
            assert_eq!(before, after, "{before}");
        }
    }

    #[test]
    fn err_response_keeps_message_text() {
        let response = round_trip_response(&Response::Err("bad passphrase".into()));
        match response {
            Response::Err(message) => assert_eq!(message, "bad passphrase"),
            other => panic!("wrong variant {other:?}"),
        }
    }

    #[test]
    fn reply_uses_adjacent_kind_value_tags() {
        let json = serde_json::to_value(Response::Ok(Reply::Text("x".into()))).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"ok": {"kind": "text", "value": "x"}})
        );
    }

    #[test]
    fn symbols_are_nul_terminated() {
        for sym in [SYM_ABI_VERSION, SYM_BUILD_ID, SYM_CALL, SYM_FREE] {
            assert_eq!(sym.last(), Some(&0));
            assert_eq!(sym.iter().filter(|b| **b == 0).count(), 1);
        }
    }
}
