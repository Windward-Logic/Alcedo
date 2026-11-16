//! Toast construction.
//!
//! Toast titles carry account names, folder names, subjects and server
//! errors. `adw::Toast` parses Pango markup by default, so a stray `&` or `<`
//! would render blank or garbled; every toast is built here as plain text.

use relm4::adw;

/// A toast whose title is shown verbatim (no Pango markup).
pub fn plain(title: &str) -> adw::Toast {
    adw::Toast::builder().title(title).use_markup(false).build()
}

/// A plain-text toast with an explicit timeout in seconds (`0` = until dismissed).
pub fn plain_with_timeout(title: &str, timeout: u32) -> adw::Toast {
    adw::Toast::builder()
        .title(title)
        .use_markup(false)
        .timeout(timeout)
        .build()
}
