//! The Sequoia + Nettle OpenPGP backend for Alcedo.

mod autocrypt;
mod backend;
mod generate;
mod inspect;
mod protect;
mod util;

pub use autocrypt::{
    AutocryptPeer, autocrypt_header_value, autocrypt_peer_from_rfc822, parse_autocrypt_from_rfc822,
};
pub use backend::SequoiaPgp;
pub use generate::{CertSummary, GeneratedCert, generate, parse_public, parse_secret};
pub use inspect::{DecryptResult, IncomingKind, decrypt, inspect_incoming, verify};
pub use protect::{
    PgpBodyParts, PgpMimeKind, ProtectRequest, ProtectResult, SignKey, protect_outbound,
};

pub use alcedo_core::PgpStatus;
pub use alcedo_pgp_api::PgpBackend;
