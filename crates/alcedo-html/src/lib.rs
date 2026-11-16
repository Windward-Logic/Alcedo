//! HTML sanitization for Alcedo mail bodies.

pub mod cid;
pub mod compose;
pub mod dark_mode;
pub mod otp;
pub mod pixel;
pub mod plaintext;
pub mod sanitize;
pub mod signature;
pub mod trackers;

pub use cid::{cid_map_key, rewrite_cids};
pub use compose::{
    OutboundBodies, StyledRun, cid_imgs_for_display, cid_imgs_for_send, convert_compose_format,
    html_to_plain_email, html_to_styled_runs, looks_like_markdown, markdown_to_html,
    render_outbound, sanitize_compose_html, styled_runs_to_html, wrap_html_document,
};
pub use dark_mode::adapt_html_for_dark_mode;
pub use otp::{VerificationCode, extract_verification_code, format_code_display};
pub use pixel::inject_tracking_pixel;
pub use plaintext::{autolink_html_fragment, plaintext_to_fragment};
pub use sanitize::{
    SANITIZER_VERSION, SanitizeOptions, html_has_remote_images, html_to_snippet,
    looks_like_escaped_html, looks_like_generated_plaintext_html, looks_like_html, sanitize_html,
};
pub use signature::{
    apply_signature, remove_inline_imgs, render_signature_for_format, sanitize_signature_html,
    signature_inlines_for_body, signature_plain_mail_preview, strip_signature,
};
pub use trackers::strip_trackers;
