//! An OpenPGP backend that loads the replaceable `alcedo-pgp-ffi` shared
//! library at run time.
//!
//! This crate links no cryptography. [`SharedPgp`] turns each
//! [`PgpBackend`] call into a [`wire::Request`], sends it through the loaded
//! library and maps the [`wire::Response`] back, so its results and error text
//! match the static backend.

mod loader;

use std::path::PathBuf;
use std::sync::Once;

use alcedo_core::{Error, PgpSignatureState, Result};
use alcedo_pgp_api::wire::{CALL_OK, Reply, Request, Response};

pub use alcedo_pgp_api::*;

const UNEXPECTED_REPLY: &str = "OpenPGP library returned an unexpected reply";

/// Where the loaded OpenPGP library came from and what it reports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LibraryInfo {
    pub path: PathBuf,
    pub abi_version: u32,
    pub build_id: String,
}

/// Loads the library if needed and describes it.
pub fn library_info() -> Result<LibraryInfo> {
    let loaded = loader::loaded().map_err(unavailable)?;
    Ok(LibraryInfo {
        path: loaded.path().to_path_buf(),
        abi_version: loaded.abi_version(),
        build_id: loaded.build_id(),
    })
}

/// The OpenPGP backend backed by the run-time loaded library.
pub struct SharedPgp;

impl PgpBackend for SharedPgp {
    fn generate(display_name: &str, email: &str, passphrase: &str) -> Result<GeneratedCert> {
        let request = Request::Generate {
            display_name: display_name.to_owned(),
            email: email.to_owned(),
            passphrase: passphrase.to_owned(),
        };
        call_expecting(&request, |reply| match reply {
            Reply::GeneratedCert(cert) => Some(cert),
            _ => None,
        })
    }

    fn parse_public(armored: &str) -> Result<CertSummary> {
        let request = Request::ParsePublic {
            armored: armored.to_owned(),
        };
        call_expecting(&request, |reply| match reply {
            Reply::CertSummary(summary) => Some(summary),
            _ => None,
        })
    }

    fn parse_secret(armored: &str, passphrase: Option<&str>) -> Result<GeneratedCert> {
        let request = Request::ParseSecret {
            armored: armored.to_owned(),
            passphrase: passphrase.map(str::to_owned),
        };
        call_expecting(&request, |reply| match reply {
            Reply::GeneratedCert(cert) => Some(cert),
            _ => None,
        })
    }

    fn verify(raw: &[u8], public_armoreds: &[String]) -> PgpStatus {
        let request = Request::Verify {
            raw: raw.to_vec(),
            public_armoreds: public_armoreds.to_vec(),
        };
        let result = call_expecting(&request, |reply| match reply {
            Reply::PgpStatus(status) => Some(status),
            _ => None,
        });
        result.unwrap_or_else(|err| PgpStatus {
            encrypted: false,
            decrypted: false,
            signature: PgpSignatureState::Invalid {
                reason: pgp_message(err),
            },
        })
    }

    fn decrypt(
        raw: &[u8],
        secret_armored: &str,
        passphrase: &str,
        extra_publics: &[String],
    ) -> Result<DecryptResult> {
        let request = Request::Decrypt {
            raw: raw.to_vec(),
            secret_armored: secret_armored.to_owned(),
            passphrase: passphrase.to_owned(),
            extra_publics: extra_publics.to_vec(),
        };
        call_expecting(&request, |reply| match reply {
            Reply::DecryptResult(result) => Some(result),
            _ => None,
        })
    }

    fn protect_outbound(request: ProtectRequest) -> Result<ProtectResult> {
        let request = Request::ProtectOutbound { request };
        call_expecting(&request, |reply| match reply {
            Reply::ProtectResult(result) => Some(result),
            _ => None,
        })
    }

    fn autocrypt_header_value(
        addr: &str,
        public_armored: &str,
        prefer_encrypt_mutual: bool,
    ) -> Result<String> {
        let request = Request::AutocryptHeaderValue {
            addr: addr.to_owned(),
            public_armored: public_armored.to_owned(),
            prefer_encrypt_mutual,
        };
        call_expecting(&request, |reply| match reply {
            Reply::Text(value) => Some(value),
            _ => None,
        })
    }

    fn parse_autocrypt_from_rfc822(raw: &[u8]) -> Option<AutocryptPeer> {
        static WARNED: Once = Once::new();
        let request = Request::ParseAutocryptFromRfc822 { raw: raw.to_vec() };
        autocrypt_peer_or_warn(&request, &WARNED, "parse_autocrypt_from_rfc822")
    }

    fn autocrypt_peer_from_rfc822(raw: &[u8]) -> Option<AutocryptPeer> {
        static WARNED: Once = Once::new();
        let request = Request::AutocryptPeerFromRfc822 { raw: raw.to_vec() };
        autocrypt_peer_or_warn(&request, &WARNED, "autocrypt_peer_from_rfc822")
    }
}

/// Runs an infallible Autocrypt lookup; on failure returns `None` and warns at
/// most once per process for each operation, since these run for every
/// incoming message.
fn autocrypt_peer_or_warn(
    request: &Request,
    warned: &Once,
    operation: &'static str,
) -> Option<AutocryptPeer> {
    let result = call_expecting(request, |reply| match reply {
        Reply::AutocryptPeer(peer) => Some(peer),
        _ => None,
    });
    result.unwrap_or_else(|err| {
        warned.call_once(|| {
            tracing::warn!(operation, error = %err, "Autocrypt lookup failed; ignoring Autocrypt headers");
        });
        None
    })
}

/// Sends `request` and extracts the reply variant that matches it.
fn call_expecting<T>(request: &Request, pick: impl FnOnce(Reply) -> Option<T>) -> Result<T> {
    pick(call(request)?).ok_or_else(|| Error::Pgp(UNEXPECTED_REPLY.into()))
}

fn call(request: &Request) -> Result<Reply> {
    let loaded = loader::loaded().map_err(unavailable)?;
    let json = serde_json::to_vec(request)
        .map_err(|err| Error::Pgp(format!("cannot encode OpenPGP request: {err}")))?;
    // `reply` frees the library's buffer when it goes out of scope, on every
    // return path below.
    let (code, reply) = loaded.call(&json);
    if code != CALL_OK {
        return Err(library_error(code, reply.bytes()));
    }
    // The parse error is not surfaced: its text can quote reply contents,
    // such as decrypted plaintext.
    match serde_json::from_slice(reply.bytes()) {
        Ok(Response::Ok(reply)) => Ok(reply),
        Ok(Response::Err(message)) => Err(Error::Pgp(message)),
        Err(_) => Err(Error::Pgp(UNEXPECTED_REPLY.into())),
    }
}

fn library_error(code: i32, message: &[u8]) -> Error {
    if message.is_empty() {
        return Error::Pgp(format!("OpenPGP library error: code {code}"));
    }
    Error::Pgp(format!(
        "OpenPGP library error: {}",
        String::from_utf8_lossy(message)
    ))
}

fn unavailable(reason: &str) -> Error {
    Error::Pgp(format!("OpenPGP library unavailable: {reason}"))
}

/// The text a caller would see for `err`, without the `pgp: ` prefix.
fn pgp_message(err: Error) -> String {
    match err {
        Error::Pgp(message) => message,
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn library_error_prefixes_the_message() {
        let err = library_error(wire::CALL_PANIC, b"boom");
        assert_eq!(pgp_message(err), "OpenPGP library error: boom");
    }

    #[test]
    fn library_error_without_message_names_the_code() {
        let err = library_error(wire::CALL_BAD_REQUEST, b"");
        assert_eq!(pgp_message(err), "OpenPGP library error: code 1");
    }

    #[test]
    fn unavailable_names_the_reason() {
        let err = unavailable("not found");
        assert_eq!(pgp_message(err), "OpenPGP library unavailable: not found");
    }
}
