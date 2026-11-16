//! OpenPGP operations for Alcedo using Sequoia + Nettle.

mod autocrypt;
mod generate;
mod inspect;
mod protect;
mod util;

pub use autocrypt::{
    AutocryptPeer, autocrypt_header_value, autocrypt_peer_from_rfc822, parse_autocrypt_from_rfc822,
};
pub use generate::{CertSummary, GeneratedCert, generate, parse_public, parse_secret};
pub use inspect::{IncomingKind, decrypt, inspect_incoming, verify};
pub use protect::{
    PgpBodyParts, PgpMimeKind, ProtectRequest, ProtectResult, SignKey, protect_outbound,
};

pub use alcedo_core::PgpStatus;
