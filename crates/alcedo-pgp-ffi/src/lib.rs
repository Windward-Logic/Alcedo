//! The replaceable OpenPGP shared library.
//!
//! Exposes the Sequoia backend ([`alcedo_pgp::SequoiaPgp`]) through the C ABI
//! defined in [`alcedo_pgp_api::wire`]. The app loads this library at run time,
//! so a user can rebuild it from source and swap it in without relinking the
//! app itself.

use std::ffi::c_char;
use std::mem::ManuallyDrop;
use std::panic;

use alcedo_core::Error;
use alcedo_pgp::SequoiaPgp;
use alcedo_pgp_api::PgpBackend;
use alcedo_pgp_api::wire::{
    ABI_VERSION, AbiVersionFn, BuildIdFn, CALL_BAD_REQUEST, CALL_OK, CALL_PANIC, CallFn, FfiBuffer,
    FreeFn, Reply, Request, Response,
};

// The loader casts each symbol to these types; keep the exports in step.
const _: AbiVersionFn = alcedo_pgp_ffi_abi_version;
const _: BuildIdFn = alcedo_pgp_ffi_build_id;
const _: CallFn = alcedo_pgp_ffi_call;
const _: FreeFn = alcedo_pgp_ffi_free;

const NULL_REQUEST_MESSAGE: &str = "request pointer is null but request length is not zero";
/// Deliberately generic: a panic message could quote message or key material.
const PANIC_MESSAGE: &str = "OpenPGP library panicked while handling the request";

/// The ABI revision this build implements.
#[unsafe(no_mangle)]
pub extern "C" fn alcedo_pgp_ffi_abi_version() -> u32 {
    ABI_VERSION
}

/// A static, NUL-terminated ASCII build identifier (`ALCEDO_PGP_BUILD_ID` at
/// build time, `unmodified` by default).
#[unsafe(no_mangle)]
pub extern "C" fn alcedo_pgp_ffi_build_id() -> *const c_char {
    concat!(env!("ALCEDO_PGP_FFI_BUILD_ID"), "\0")
        .as_ptr()
        .cast()
}

/// Handles one JSON [`Request`] and writes the reply buffer to `*out`.
///
/// Returns [`CALL_OK`] with a JSON [`Response`] in `*out`, or
/// [`CALL_BAD_REQUEST`] / [`CALL_PANIC`] with a UTF-8 message (or an empty
/// buffer). Unless `out` is null, `*out` is written exactly once, and the
/// caller must release it with [`alcedo_pgp_ffi_free`].
///
/// # Safety
///
/// - `request` must be null with `request_len == 0`, or point to
///   `request_len` initialised bytes that stay valid and unmodified for the
///   duration of the call.
/// - `out` must be null or valid for a write of one [`FfiBuffer`]; any value it
///   held before is overwritten without being freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn alcedo_pgp_ffi_call(
    request: *const u8,
    request_len: usize,
    out: *mut FfiBuffer,
) -> i32 {
    if out.is_null() {
        return CALL_BAD_REQUEST;
    }
    let (code, message) = if request.is_null() && request_len != 0 {
        (CALL_BAD_REQUEST, NULL_REQUEST_MESSAGE.as_bytes().to_vec())
    } else {
        let request_bytes: &[u8] = if request_len == 0 {
            &[]
        } else {
            // SAFETY: `request` is non-null here (null with a nonzero length
            // was rejected above), and the caller guarantees it points to
            // `request_len` initialised bytes that outlive this call.
            unsafe { std::slice::from_raw_parts(request, request_len) }
        };
        run_guarded(|| handle_request(request_bytes))
    };
    // SAFETY: `out` is non-null (checked above) and the caller guarantees it
    // is valid for a write of one `FfiBuffer`. `write` does not read or drop
    // the previous value, so a stale or uninitialised `*out` is fine.
    unsafe { out.write(into_ffi_buffer(message)) };
    code
}

/// Releases a buffer returned by [`alcedo_pgp_ffi_call`]. A null pointer is a
/// no-op.
///
/// # Safety
///
/// `buffer` must be [`FfiBuffer::EMPTY`] (or have a null `ptr`), or be a value
/// written by [`alcedo_pgp_ffi_call`] in this library, unmodified and not
/// freed before.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn alcedo_pgp_ffi_free(buffer: FfiBuffer) {
    if buffer.ptr.is_null() {
        return;
    }
    // SAFETY: a non-null buffer came from `into_ffi_buffer`, which leaked a
    // `Vec<u8>` allocated by this library's allocator with exactly this
    // ptr/len/cap; the caller guarantees it is unmodified and freed only once.
    drop(unsafe { Vec::from_raw_parts(buffer.ptr, buffer.len, buffer.cap) });
}

/// Runs `handler`, turning a panic into [`CALL_PANIC`] with a fixed message so
/// no payload (which could quote key or message material) leaves the library.
fn run_guarded(handler: impl FnOnce() -> (i32, Vec<u8>) + panic::UnwindSafe) -> (i32, Vec<u8>) {
    panic::catch_unwind(handler).unwrap_or_else(|_| (CALL_PANIC, PANIC_MESSAGE.as_bytes().to_vec()))
}

fn bad_request_message(err: &serde_json::Error) -> String {
    format!(
        "cannot parse OpenPGP request: {:?} error at line {} column {}",
        err.classify(),
        err.line(),
        err.column()
    )
}

fn handle_request(request_bytes: &[u8]) -> (i32, Vec<u8>) {
    let request: Request = match serde_json::from_slice(request_bytes) {
        Ok(request) => request,
        // Position and category only: serde_json's message can quote a field
        // value, such as a passphrase or a secret key.
        Err(err) => return (CALL_BAD_REQUEST, bad_request_message(&err).into_bytes()),
    };
    let response = dispatch(request);
    // Every wire type is plain data with string map keys, so encoding cannot
    // fail; if it ever did, the panic is caught and reported as CALL_PANIC.
    let json = serde_json::to_vec(&response).expect("wire Response always serializes");
    (CALL_OK, json)
}

fn dispatch(request: Request) -> Response {
    let result = match request {
        Request::Generate {
            display_name,
            email,
            passphrase,
        } => SequoiaPgp::generate(&display_name, &email, &passphrase).map(Reply::GeneratedCert),
        Request::ParsePublic { armored } => {
            SequoiaPgp::parse_public(&armored).map(Reply::CertSummary)
        }
        Request::ParseSecret {
            armored,
            passphrase,
        } => SequoiaPgp::parse_secret(&armored, passphrase.as_deref()).map(Reply::GeneratedCert),
        Request::Verify {
            raw,
            public_armoreds,
        } => Ok(Reply::PgpStatus(SequoiaPgp::verify(&raw, &public_armoreds))),
        Request::Decrypt {
            raw,
            secret_armored,
            passphrase,
            extra_publics,
        } => SequoiaPgp::decrypt(&raw, &secret_armored, &passphrase, &extra_publics)
            .map(Reply::DecryptResult),
        Request::ProtectOutbound { request } => {
            SequoiaPgp::protect_outbound(request).map(Reply::ProtectResult)
        }
        Request::AutocryptHeaderValue {
            addr,
            public_armored,
            prefer_encrypt_mutual,
        } => SequoiaPgp::autocrypt_header_value(&addr, &public_armored, prefer_encrypt_mutual)
            .map(Reply::Text),
        Request::ParseAutocryptFromRfc822 { raw } => Ok(Reply::AutocryptPeer(
            SequoiaPgp::parse_autocrypt_from_rfc822(&raw),
        )),
        Request::AutocryptPeerFromRfc822 { raw } => Ok(Reply::AutocryptPeer(
            SequoiaPgp::autocrypt_peer_from_rfc822(&raw),
        )),
    };
    match result {
        Ok(reply) => Response::Ok(reply),
        Err(Error::Pgp(message)) => Response::Err(message),
        Err(other) => Response::Err(other.to_string()),
    }
}

/// Leaks `bytes` into a buffer only [`alcedo_pgp_ffi_free`] may release.
fn into_ffi_buffer(bytes: Vec<u8>) -> FfiBuffer {
    if bytes.is_empty() {
        return FfiBuffer::EMPTY;
    }
    let mut bytes = ManuallyDrop::new(bytes);
    FfiBuffer {
        ptr: bytes.as_mut_ptr(),
        len: bytes.len(),
        cap: bytes.capacity(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CStr;

    use alcedo_pgp_api::CertSummary;

    const TEST_PUBLIC: &str = include_str!("../../alcedo-pgp/tests/fixtures/test-public.asc");

    /// Calls the library and copies the reply out, freeing the buffer.
    fn call(request: &[u8]) -> (i32, Vec<u8>) {
        let mut out = FfiBuffer::EMPTY;
        // SAFETY: `request` is a live slice and `out` a valid local.
        let code = unsafe { alcedo_pgp_ffi_call(request.as_ptr(), request.len(), &mut out) };
        let bytes = if out.ptr.is_null() {
            Vec::new()
        } else {
            // SAFETY: a non-null buffer from `alcedo_pgp_ffi_call` holds `len`
            // initialised bytes until it is freed below.
            unsafe { std::slice::from_raw_parts(out.ptr, out.len) }.to_vec()
        };
        // SAFETY: `out` was written by `alcedo_pgp_ffi_call` and is freed once.
        unsafe { alcedo_pgp_ffi_free(out) };
        (code, bytes)
    }

    #[test]
    fn garbage_json_is_a_bad_request_with_a_message() {
        let (code, message) = call(b"{not json");
        assert_eq!(code, CALL_BAD_REQUEST);
        assert!(!message.is_empty());
        assert!(String::from_utf8(message).is_ok());
    }

    #[test]
    fn bad_request_message_does_not_quote_field_values() {
        let (code, message) = call(
            br#"{"op":"autocrypt_header_value","addr":"a","public_armored":"x","prefer_encrypt_mutual":"hunter2"}"#,
        );
        assert_eq!(code, CALL_BAD_REQUEST);
        let message = String::from_utf8(message).unwrap();
        assert!(!message.contains("hunter2"), "{message}");
    }

    #[test]
    fn panics_become_call_panic_without_the_payload() {
        let (code, message) = run_guarded(|| panic!("secret-key-material"));
        assert_eq!(code, CALL_PANIC);
        assert_eq!(message, PANIC_MESSAGE.as_bytes());
    }

    #[test]
    fn null_out_is_a_bad_request() {
        let request = br#"{"op":"parse_public","armored":"x"}"#;
        // SAFETY: a null `out` is part of the contract and must be rejected.
        let code =
            unsafe { alcedo_pgp_ffi_call(request.as_ptr(), request.len(), std::ptr::null_mut()) };
        assert_eq!(code, CALL_BAD_REQUEST);
    }

    #[test]
    fn null_request_with_length_is_a_bad_request() {
        let mut out = FfiBuffer::EMPTY;
        // SAFETY: a null `request` with a nonzero length must be rejected
        // without being read; `out` is a valid local.
        let code = unsafe { alcedo_pgp_ffi_call(std::ptr::null(), 8, &mut out) };
        assert_eq!(code, CALL_BAD_REQUEST);
        assert!(!out.ptr.is_null());
        // SAFETY: `out` was written by `alcedo_pgp_ffi_call` and is freed once.
        unsafe { alcedo_pgp_ffi_free(out) };
    }

    #[test]
    fn freeing_the_empty_buffer_is_a_no_op() {
        // SAFETY: EMPTY has a null pointer, which the contract allows.
        unsafe { alcedo_pgp_ffi_free(FfiBuffer::EMPTY) };
    }

    #[test]
    fn parse_public_matches_the_static_backend() {
        let request = serde_json::to_vec(&Request::ParsePublic {
            armored: TEST_PUBLIC.to_owned(),
        })
        .unwrap();
        let (code, json) = call(&request);
        assert_eq!(code, CALL_OK);
        let summary: CertSummary = match serde_json::from_slice(&json).unwrap() {
            Response::Ok(Reply::CertSummary(summary)) => summary,
            other => panic!("unexpected response {other:?}"),
        };
        assert_eq!(summary, SequoiaPgp::parse_public(TEST_PUBLIC).unwrap());
    }

    #[test]
    fn pgp_errors_travel_as_err_responses() {
        let request = serde_json::to_vec(&Request::ParsePublic {
            armored: "garbage".into(),
        })
        .unwrap();
        let (code, json) = call(&request);
        assert_eq!(code, CALL_OK);
        let expected = match SequoiaPgp::parse_public("garbage") {
            Err(Error::Pgp(message)) => message,
            other => panic!("static backend should fail with Error::Pgp, got {other:?}"),
        };
        match serde_json::from_slice(&json).unwrap() {
            Response::Err(message) => assert_eq!(message, expected),
            other => panic!("unexpected response {other:?}"),
        }
    }

    #[test]
    fn reports_the_compiled_build_id() {
        // SAFETY: the function returns a static NUL-terminated string.
        let id = unsafe { CStr::from_ptr(alcedo_pgp_ffi_build_id()) };
        assert_eq!(id.to_str().unwrap(), env!("ALCEDO_PGP_FFI_BUILD_ID"));
    }

    #[test]
    fn reports_the_abi_version() {
        assert_eq!(alcedo_pgp_ffi_abi_version(), ABI_VERSION);
    }
}
