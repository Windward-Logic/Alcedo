//! The contract between Alcedo and an OpenPGP backend.
//!
//! This crate holds no cryptography. It defines the data types every backend
//! exchanges, the [`PgpBackend`] trait a backend implements, and the [`wire`]
//! format the shared-library backend speaks across its C ABI. Keeping it free
//! of Sequoia and Nettle lets the app link against the LGPL crypto stack only
//! through a replaceable shared library.

mod b64;
mod backend;
mod inspect;
mod types;
pub mod wire;

pub use alcedo_core::PgpStatus;
pub use backend::PgpBackend;
pub use inspect::{IncomingKind, inspect_incoming};
pub use types::{
    AutocryptPeer, CertSummary, DecryptResult, GeneratedCert, PgpBodyParts, PgpMimeKind,
    ProtectRequest, ProtectResult, SignKey,
};
