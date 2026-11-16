//! Tie objects' lifetimes to a widget without reference cycles.
//!
//! GTK4 only disposes a widget once its last reference is gone, so a callback
//! owned by a widget that holds that widget (or a struct holding it) strongly
//! keeps it alive forever. These helpers give such callbacks a weak handle and
//! let the widget own the strong one.

use std::rc::{Rc, Weak};

use relm4::gtk;
use relm4::gtk::prelude::*;

/// Keep `value` alive exactly as long as `owner`, and return a weak handle for
/// callbacks that `owner` or its descendants own. The strong reference lives
/// in a signal handler on `owner`, which GTK drops when `owner` is disposed.
///
/// `value` must not hold `owner` or any of its ancestors strongly (that is a
/// cycle again): for a component that stores its own root, pass an ancestor.
pub(crate) fn weak_owned_by<T: 'static>(owner: &impl IsA<gtk::Widget>, value: T) -> Weak<T> {
    let value = Rc::new(value);
    let weak = Rc::downgrade(&value);
    owner.as_ref().connect_destroy(move |_| {
        let _keep_alive = &value;
    });
    weak
}

/// Unparent `popover` when `owner` is destroyed. GTK never unparents a
/// `set_parent` popover itself, so it would outlive its owner, and through its
/// action muxer keep the owner's ancestors' action groups (and whatever their
/// callbacks hold) alive.
pub(crate) fn unparent_popover_on_destroy(
    owner: &impl IsA<gtk::Widget>,
    popover: &impl IsA<gtk::Widget>,
) {
    let popover = popover.as_ref().downgrade();
    owner.as_ref().connect_destroy(move |owner| {
        if let Some(popover) = popover.upgrade()
            && popover.parent().as_ref() == Some(owner)
        {
            popover.unparent();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owned_value_lives_until_its_owner_is_freed() {
        crate::test_helpers::gtk_test_run(|| {
            let owner = gtk::Button::new();
            let weak = weak_owned_by(&owner, owner.clone().downgrade());
            assert!(weak.upgrade().is_some());
            // A callback on the owner holding the handle is not a cycle.
            owner.connect_clicked({
                let weak = weak.clone();
                move |_| {
                    let _ = weak.upgrade();
                }
            });
            let weak_owner = owner.downgrade();
            drop(owner);
            assert!(weak_owner.upgrade().is_none());
            assert!(weak.upgrade().is_none());
        });
    }

    #[test]
    fn popover_is_unparented_and_freed_with_its_owner() {
        crate::test_helpers::gtk_test_run(|| {
            let owner = gtk::Button::new();
            let popover = gtk::Popover::new();
            popover.set_parent(&owner);
            unparent_popover_on_destroy(&owner, &popover);
            let weak_popover = popover.downgrade();
            drop(popover);
            drop(owner);
            assert!(weak_popover.upgrade().is_none());
        });
    }
}
