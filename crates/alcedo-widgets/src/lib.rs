//! Sandboxed WebKit helpers for mail reading and composing.

pub mod sandbox;

pub use sandbox::{
    CID_CACHE_MAX_BYTES, CidCache, WidgetMessage, compose_editor_settings, create_isolated_context,
    ephemeral_network_session, mail_settings, new_cid_cache, new_unbounded_cid_cache,
    register_cid_scheme,
};
