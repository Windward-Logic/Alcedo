//! SMTP send (P6).

pub mod send;
mod xoauth2;

pub use send::{
    PgpMimeBody, RenderedBodies, SmtpAuth, SmtpOptions, build_draft_message, build_pgp_message,
    message_to_rfc822, send_message, send_pgp_message, send_rendered, serialize_inner_body,
    validate_https_pixel_url,
};
