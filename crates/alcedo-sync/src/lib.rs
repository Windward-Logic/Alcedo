//! Sync runtime and workers (P7).

/// gettext domain of the text shown to people, read by the `alcedo_i18n` macros.
const I18N_DOMAIN: &str = "alcedo";

pub mod aggregator;
pub mod extension;
pub mod oauth_token_cache;
pub mod pgp;
mod pgp_backend;
pub mod runtime;
pub mod server_rules;
pub mod unsubscribe;
pub mod worker;

pub use extension::{SyncContext, SyncExtension, SyncExtensions};
pub use runtime::SyncRuntime;
