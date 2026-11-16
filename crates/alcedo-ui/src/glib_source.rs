//! Helpers for canceling GLib main-loop sources without panicking on stale ids.

use std::cell::Cell;

use relm4::gtk::glib;

/// Remove a main-loop source if it is still registered.
///
/// [`SourceId::remove`] panics when the source already fired or was removed elsewhere.
pub fn remove_source(id: glib::SourceId) {
    // SAFETY: `g_source_remove` only looks the id up in the default main
    // context; an id that is no longer registered logs a GLib critical and
    // returns FALSE without touching memory. Taking `SourceId` by value means
    // this caller cannot remove the same id twice.
    unsafe {
        glib::ffi::g_source_remove(id.as_raw());
    }
}

pub fn cancel_source(slot: &Cell<Option<glib::SourceId>>) {
    if let Some(id) = slot.take() {
        remove_source(id);
    }
}

pub fn cancel_source_option(slot: &mut Option<glib::SourceId>) {
    if let Some(id) = slot.take() {
        remove_source(id);
    }
}
