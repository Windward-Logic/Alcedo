//! Hover-to-reveal for the folder sidebar while it is an overlay.
//!
//! A thin strip on the left edge of the window opens the sidebar once the
//! pointer rests on it, and the sidebar slides back out shortly after the
//! pointer leaves. Only a sidebar the strip opened closes by itself: one the
//! user opened with the toggle stays until they dismiss it.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use relm4::adw;
use relm4::adw::prelude::*;
use relm4::gtk;
use relm4::gtk::glib;

use crate::glib_source::cancel_source;

/// Width of the hover strip, in pixels. Narrow enough not to steal clicks
/// from the list's first column.
const EDGE_WIDTH: i32 = 6;
/// The pointer must rest on the edge this long, so a pass to the window
/// border does not open the sidebar.
const OPEN_DELAY: Duration = Duration::from_millis(120);
/// Grace period after the pointer leaves, so a slip off the sidebar's edge
/// does not close it.
const CLOSE_DELAY: Duration = Duration::from_millis(400);

/// A place where the pointer keeps a revealed sidebar open.
#[derive(Clone, Copy)]
pub(super) enum KeepZone {
    Sidebar = 0b01,
    /// The app rail, which sits outside the mail view.
    Rail = 0b10,
}

struct PeekState {
    overlay: glib::WeakRef<adw::OverlaySplitView>,
    strip: glib::WeakRef<gtk::Box>,
    /// The strip can reveal the sidebar (it is an overlay, not a column).
    enabled: Cell<bool>,
    /// The strip, not the user, opened the sidebar that is showing.
    peeking: Cell<bool>,
    /// Bit set of `KeepZone`s the pointer is in.
    inside: Cell<u8>,
    open_timer: Cell<Option<glib::SourceId>>,
    close_timer: Cell<Option<glib::SourceId>>,
}

impl PeekState {
    fn sync_strip(&self) {
        let (Some(strip), Some(overlay)) = (self.strip.upgrade(), self.overlay.upgrade()) else {
            return;
        };
        strip.set_visible(self.enabled.get() && !overlay.shows_sidebar());
    }

    fn pointer_in_keep_zone(&self) -> bool {
        self.inside.get() != 0
    }

    fn open_now(&self) {
        let Some(overlay) = self.overlay.upgrade() else {
            return;
        };
        if overlay.shows_sidebar() {
            return;
        }
        self.peeking.set(true);
        overlay.set_show_sidebar(true);
    }

    fn close_now(&self) {
        if !self.peeking.get() {
            return;
        }
        if let Some(overlay) = self.overlay.upgrade() {
            overlay.set_show_sidebar(false);
        }
    }

    fn cancel_timers(&self) {
        cancel_source(&self.open_timer);
        cancel_source(&self.close_timer);
    }
}

fn is_armed(slot: &Cell<Option<glib::SourceId>>) -> bool {
    let id = slot.take();
    let armed = id.is_some();
    slot.set(id);
    armed
}

/// Run `action` after `delay` unless the slot is cancelled first.
fn arm(
    state: &Rc<PeekState>,
    slot: fn(&PeekState) -> &Cell<Option<glib::SourceId>>,
    delay: Duration,
    action: fn(&Rc<PeekState>),
) {
    cancel_source(slot(state));
    let fired = state.clone();
    let id = glib::timeout_add_local_once(delay, move || {
        // The source is gone once it fires; forget its id so it is not removed twice.
        let _ = slot(&fired).take();
        action(&fired);
    });
    slot(state).set(Some(id));
}

fn schedule_open(state: &Rc<PeekState>) {
    if is_armed(&state.open_timer) {
        return;
    }
    arm(state, |s| &s.open_timer, OPEN_DELAY, open_and_arm_close);
}

fn schedule_close(state: &Rc<PeekState>) {
    arm(state, |s| &s.close_timer, CLOSE_DELAY, |s| s.close_now());
}

/// Open the sidebar, then start the close countdown unless the pointer is
/// already somewhere that keeps it open. If the pointer never lands on the
/// sidebar it does not stay stuck open.
fn open_and_arm_close(state: &Rc<PeekState>) {
    state.open_now();
    if state.peeking.get() && !state.pointer_in_keep_zone() {
        schedule_close(state);
    }
}

fn set_inside(state: &Rc<PeekState>, zone: KeepZone, inside: bool) {
    let bit = zone as u8;
    state.inside.set(if inside {
        state.inside.get() | bit
    } else {
        state.inside.get() & !bit
    });
    if inside {
        cancel_source(&state.close_timer);
    } else if state.peeking.get() && !state.pointer_in_keep_zone() {
        schedule_close(state);
    }
}

pub(super) struct FolderPeek {
    state: Rc<PeekState>,
}

impl FolderPeek {
    /// Lay the hover strip over `host`'s left edge and watch `sidebar` for
    /// the pointer. `overlay` is the split view inside `host`.
    pub(super) fn install(
        host: &gtk::Overlay,
        overlay: &adw::OverlaySplitView,
        sidebar: &impl IsA<gtk::Widget>,
    ) -> Self {
        let strip = gtk::Box::builder()
            .halign(gtk::Align::Start)
            .valign(gtk::Align::Fill)
            .width_request(EDGE_WIDTH)
            .visible(false)
            .build();
        host.add_overlay(&strip);
        host.set_measure_overlay(&strip, false);

        let state = Rc::new(PeekState {
            overlay: overlay.downgrade(),
            strip: strip.downgrade(),
            enabled: Cell::new(false),
            peeking: Cell::new(false),
            inside: Cell::new(0),
            open_timer: Cell::new(None),
            close_timer: Cell::new(None),
        });

        let edge = gtk::EventControllerMotion::new();
        edge.connect_enter({
            let state = state.clone();
            move |_, _, _| schedule_open(&state)
        });
        edge.connect_leave({
            let state = state.clone();
            move |_| cancel_source(&state.open_timer)
        });
        strip.add_controller(edge);

        // `contains_pointer` stays true over popovers anchored in the
        // sidebar, so a context menu does not count as leaving.
        let inside = gtk::EventControllerMotion::new();
        inside.connect_contains_pointer_notify({
            let state = state.clone();
            move |controller| set_inside(&state, KeepZone::Sidebar, controller.contains_pointer())
        });
        sidebar.add_controller(inside);

        overlay.connect_show_sidebar_notify({
            let state = state.clone();
            move |overlay| {
                if !overlay.shows_sidebar() {
                    state.peeking.set(false);
                    state.cancel_timers();
                }
                state.sync_strip();
            }
        });

        Self { state }
    }

    /// Turn the strip on while the folders are an overlay and off while they
    /// are a column.
    pub(super) fn set_enabled(&self, enabled: bool) {
        self.state.enabled.set(enabled);
        self.state.sync_strip();
    }

    /// The pointer is (or is no longer) on a spot outside the mail view that
    /// should reveal the sidebar, such as the right edge of the app rail.
    pub(super) fn set_trigger_hovered(&self, hovered: bool) {
        if !hovered {
            cancel_source(&self.state.open_timer);
        } else if self.state.enabled.get() {
            schedule_open(&self.state);
        }
    }

    /// The pointer entered or left a place outside the mail view that keeps a
    /// revealed sidebar open.
    pub(super) fn set_keep_zone(&self, zone: KeepZone, inside: bool) {
        set_inside(&self.state, zone, inside);
    }

    /// Forget a pending reveal or auto-close, for when the sidebar changes
    /// role or is reparented.
    pub(super) fn reset(&self) {
        self.state.peeking.set(false);
        self.state.cancel_timers();
    }

    #[cfg(test)]
    fn reveal_now(&self) {
        self.state.open_now();
    }

    #[cfg(test)]
    fn is_peeking(&self) -> bool {
        self.state.peeking.get()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (gtk::Overlay, adw::OverlaySplitView, gtk::Box, FolderPeek) {
        let overlay = adw::OverlaySplitView::new();
        overlay.set_collapsed(true);
        let sidebar = gtk::Box::new(gtk::Orientation::Vertical, 0);
        overlay.set_sidebar(Some(&sidebar));
        let host = gtk::Overlay::new();
        host.set_child(Some(&overlay));
        let peek = FolderPeek::install(&host, &overlay, &sidebar);
        (host, overlay, sidebar, peek)
    }

    fn strip_of(host: &gtk::Overlay) -> gtk::Widget {
        let child = host.child().expect("host child");
        child.next_sibling().expect("strip is the overlay child")
    }

    #[test]
    fn strip_shows_only_while_enabled_and_the_sidebar_is_closed() {
        crate::test_helpers::gtk_test_run(|| {
            let (host, overlay, _sidebar, peek) = fixture();
            let strip = strip_of(&host);
            assert!(!strip.is_visible(), "off until the folders are an overlay");

            peek.set_enabled(true);
            assert!(strip.is_visible());

            overlay.set_show_sidebar(true);
            assert!(!strip.is_visible(), "an open sidebar covers the edge");

            overlay.set_show_sidebar(false);
            assert!(strip.is_visible());

            peek.set_enabled(false);
            assert!(!strip.is_visible());
        });
    }

    #[test]
    fn a_sidebar_the_strip_opened_is_marked_until_it_closes() {
        crate::test_helpers::gtk_test_run(|| {
            let (_host, overlay, _sidebar, peek) = fixture();
            peek.set_enabled(true);
            peek.reveal_now();
            assert!(overlay.shows_sidebar());
            assert!(peek.is_peeking());

            overlay.set_show_sidebar(false);
            assert!(!peek.is_peeking());
        });
    }

    #[test]
    fn a_sidebar_the_user_opened_is_not_marked_as_peeking() {
        crate::test_helpers::gtk_test_run(|| {
            let (_host, overlay, _sidebar, peek) = fixture();
            peek.set_enabled(true);
            overlay.set_show_sidebar(true);
            assert!(!peek.is_peeking());
            peek.reveal_now();
            assert!(!peek.is_peeking(), "already open, nothing to reveal");
        });
    }

    #[test]
    fn reset_clears_the_peeking_mark() {
        crate::test_helpers::gtk_test_run(|| {
            let (_host, _overlay, _sidebar, peek) = fixture();
            peek.set_enabled(true);
            peek.reveal_now();
            peek.reset();
            assert!(!peek.is_peeking());
        });
    }

    /// Run the default main context until `done` holds or `limit` passes.
    fn spin_until(limit: Duration, done: impl Fn() -> bool) -> bool {
        let ctx = glib::MainContext::default();
        let deadline = std::time::Instant::now() + limit;
        while std::time::Instant::now() < deadline {
            if done() {
                return true;
            }
            ctx.iteration(false);
            std::thread::sleep(Duration::from_millis(5));
        }
        done()
    }

    #[test]
    fn hovering_the_trigger_opens_and_keeps_open_while_in_a_keep_zone() {
        crate::test_helpers::gtk_test_run(|| {
            let (_host, overlay, _sidebar, peek) = fixture();
            peek.set_enabled(true);
            peek.set_keep_zone(KeepZone::Rail, true);
            peek.set_trigger_hovered(true);
            assert!(spin_until(Duration::from_secs(2), || overlay.shows_sidebar()));
            assert!(peek.is_peeking());

            // Past the close delay, still open because the pointer is on the rail.
            assert!(!spin_until(CLOSE_DELAY * 2, || !overlay.shows_sidebar()));

            peek.set_keep_zone(KeepZone::Rail, false);
            assert!(spin_until(Duration::from_secs(2), || !overlay.shows_sidebar()));
            assert!(!peek.is_peeking());
        });
    }

    #[test]
    fn a_revealed_sidebar_closes_when_the_pointer_is_nowhere_that_keeps_it() {
        crate::test_helpers::gtk_test_run(|| {
            let (_host, overlay, _sidebar, peek) = fixture();
            peek.set_enabled(true);
            peek.set_trigger_hovered(true);
            assert!(spin_until(Duration::from_secs(2), || overlay.shows_sidebar()));
            assert!(spin_until(Duration::from_secs(2), || !overlay.shows_sidebar()));
        });
    }

    #[test]
    fn leaving_the_trigger_before_the_delay_does_not_open() {
        crate::test_helpers::gtk_test_run(|| {
            let (_host, overlay, _sidebar, peek) = fixture();
            peek.set_enabled(true);
            peek.set_trigger_hovered(true);
            peek.set_trigger_hovered(false);
            assert!(!spin_until(OPEN_DELAY * 3, || overlay.shows_sidebar()));
        });
    }

    #[test]
    fn a_disabled_peek_ignores_the_trigger() {
        crate::test_helpers::gtk_test_run(|| {
            let (_host, overlay, _sidebar, peek) = fixture();
            peek.set_trigger_hovered(true);
            assert!(!spin_until(OPEN_DELAY * 3, || overlay.shows_sidebar()));
        });
    }
}
