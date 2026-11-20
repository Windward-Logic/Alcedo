//! Keep a widget's helpers from outliving it.

use gtk4 as gtk;
use gtk4::prelude::*;

/// Unparent `popover` when `owner` is destroyed. GTK never unparents a
/// `set_parent` popover itself, so it would outlive its owner, and through its
/// action muxer keep the owner's ancestors' action groups (and whatever their
/// callbacks hold) alive. A popover that was moved to another parent by then
/// is left where it is.
///
/// Call it right after `popover.set_parent(owner)`; it holds `popover` weakly.
pub fn unparent_popover_on_destroy(owner: &impl IsA<gtk::Widget>, popover: &impl IsA<gtk::Widget>) {
    let popover = popover.as_ref().downgrade();
    owner.as_ref().connect_destroy(move |owner| {
        if let Some(popover) = popover.upgrade()
            && popover.parent().as_ref() == Some(owner)
        {
            popover.unparent();
        }
    });
}
