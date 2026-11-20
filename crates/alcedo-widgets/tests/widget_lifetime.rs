//! `unparent_popover_on_destroy` as other crates call it. One test per binary:
//! GTK must be used from the thread that initialised it.

use alcedo_widgets::unparent_popover_on_destroy;
use gtk4 as gtk;
use gtk4::prelude::*;

#[test]
fn popover_goes_with_its_owner_but_not_after_moving_elsewhere() {
    gtk::init().expect("gtk init (needs a display)");

    // Freed with its owner.
    let owner = gtk::Button::new();
    let popover = gtk::Popover::new();
    popover.set_parent(&owner);
    unparent_popover_on_destroy(&owner, &popover);
    let weak_popover = popover.downgrade();
    drop(popover);
    drop(owner);
    assert!(weak_popover.upgrade().is_none());

    // Moved to another parent first: the first owner's destruction leaves it there.
    let first = gtk::Button::new();
    let second = gtk::Button::new();
    let popover = gtk::Popover::new();
    popover.set_parent(&first);
    unparent_popover_on_destroy(&first, &popover);
    popover.unparent();
    popover.set_parent(&second);
    drop(first);
    assert_eq!(popover.parent().as_ref(), Some(second.upcast_ref()));
    popover.unparent();
}
