//! Compile-time choice of the OpenPGP backend.
//!
//! `pgp-static` wins when both features are on. Builds that must not contain
//! Sequoia rely on a `cargo tree` check rather than on this precedence.

#[cfg(feature = "pgp-static")]
pub(crate) type Pgp = alcedo_pgp::SequoiaPgp;
#[cfg(all(feature = "pgp-shared", not(feature = "pgp-static")))]
pub(crate) type Pgp = alcedo_pgp_shared::SharedPgp;
#[cfg(not(any(feature = "pgp-static", feature = "pgp-shared")))]
compile_error!("alcedo-sync needs an OpenPGP backend: enable feature `pgp-static` or `pgp-shared`");
