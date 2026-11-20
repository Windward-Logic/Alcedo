//! Sandboxed WebKit helpers for mail reading and composing, and small GTK
//! widget helpers shared with extensions.

pub mod css_color;
pub mod sandbox;
pub mod widget_lifetime;

pub use css_color::{lookup_css_color, rgba_hex};
pub use sandbox::{
    CID_CACHE_MAX_BYTES, CidCache, WidgetMessage, compose_editor_settings, create_isolated_context,
    ephemeral_network_session, mail_settings, new_cid_cache, new_unbounded_cid_cache,
    register_cid_scheme,
};
pub use widget_lifetime::unparent_popover_on_destroy;
