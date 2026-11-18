//! Revealers that give their width back while closed.

use relm4::gtk;
use relm4::gtk::prelude::*;

/// A closed `GtkRevealer` still reports its child's width, which widens the
/// pane around it. Keep `revealer` out of the layout until it opens, and until
/// its closing animation ends.
pub(crate) fn collapse_while_closed(revealer: &gtk::Revealer) {
    sync_visibility(revealer);
    revealer.connect_reveal_child_notify(sync_visibility);
    revealer.connect_child_revealed_notify(sync_visibility);
}

fn sync_visibility(revealer: &gtk::Revealer) {
    revealer.set_visible(revealer.reveals_child() || revealer.is_child_revealed());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::gtk_test_run;

    fn wide_revealer() -> gtk::Revealer {
        let child = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        child.set_width_request(400);
        gtk::Revealer::builder().child(&child).build()
    }

    #[test]
    fn closed_revealer_reserves_no_width() {
        gtk_test_run(|| {
            let revealer = wide_revealer();
            collapse_while_closed(&revealer);
            assert!(!revealer.is_visible());
            let parent = gtk::Box::new(gtk::Orientation::Vertical, 0);
            parent.append(&revealer);
            assert_eq!(parent.measure(gtk::Orientation::Horizontal, -1).0, 0);
        });
    }

    #[test]
    fn opening_brings_the_revealer_back() {
        gtk_test_run(|| {
            let revealer = wide_revealer();
            collapse_while_closed(&revealer);
            revealer.set_reveal_child(true);
            assert!(revealer.is_visible());
        });
    }

    #[test]
    fn closing_removes_the_revealer_again() {
        gtk_test_run(|| {
            let revealer = wide_revealer();
            collapse_while_closed(&revealer);
            revealer.set_reveal_child(true);
            revealer.set_reveal_child(false);
            assert!(!revealer.is_visible());
        });
    }
}
