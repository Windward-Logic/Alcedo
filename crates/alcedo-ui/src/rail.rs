//! App rail sidebar (P4).

use std::cell::Cell;
use std::rc::Rc;

use relm4::gtk;
use relm4::gtk::prelude::*;

pub const RAIL_COLLAPSED_WIDTH: f64 = 56.0;
pub const RAIL_EXPANDED_WIDTH: f64 = 240.0;
pub const RAIL_EXPANDED_CLASS: &str = "alcedo-rail-expanded";

const RAIL_ACTIVE_CLASS: &str = "alcedo-rail-active";
const RAIL_LABEL_CLASS: &str = "alcedo-rail-label";
const RAIL_CONTENT_CLASS: &str = "alcedo-rail-content";

/// Which primary app space is active in the rail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RailSpace {
    Mail,
    Contacts,
    /// A space an extension added, by name.
    Space(&'static str),
}

/// A rail button with an icon and optional text label.
pub struct RailItem {
    pub button: gtk::Button,
    pub label: gtk::Label,
}

/// Build a flat rail button with an icon and label (label hidden until expanded).
pub fn rail_item(icon: &str, label: &str, tooltip: &str) -> RailItem {
    let image = gtk::Image::from_icon_name(icon);

    let label_widget = gtk::Label::builder()
        .label(label)
        .xalign(0.0)
        .halign(gtk::Align::Start)
        .hexpand(true)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .build();
    label_widget.add_css_class(RAIL_LABEL_CLASS);
    label_widget.set_visible(false);

    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(10)
        .build();
    content.append(&image);
    content.append(&label_widget);

    let accessible_label = if tooltip != label { tooltip } else { label };

    let button = gtk::Button::builder()
        .child(&content)
        .tooltip_text(accessible_label)
        .build();
    button.add_css_class("flat");
    button.add_css_class("alcedo-rail-btn");
    button.update_property(&[gtk::accessible::Property::Label(accessible_label)]);

    RailItem {
        button,
        label: label_widget,
    }
}

/// Apply or remove the rail active highlight on a button.
pub fn set_rail_active(button: &gtk::Button, active: bool) {
    if active {
        button.add_css_class(RAIL_ACTIVE_CLASS);
    } else {
        button.remove_css_class(RAIL_ACTIVE_CLASS);
    }
}

/// Format an unread count for display on a rail badge (capped at 99+).
pub fn format_badge_count(count: u32) -> String {
    if count > 99 {
        "99+".to_string()
    } else {
        count.to_string()
    }
}

/// Wrap an icon in an overlay with a top-right unread badge label.
pub fn wrap_with_badge(host_box: &gtk::Box, icon: &impl IsA<gtk::Widget>) -> gtk::Label {
    let badge = gtk::Label::new(None);
    badge.add_css_class("alcedo-rail-badge");
    badge.set_halign(gtk::Align::End);
    badge.set_valign(gtk::Align::Start);
    badge.set_can_target(false);
    badge.set_visible(false);

    let overlay = gtk::Overlay::new();
    host_box.remove(icon);
    overlay.set_child(Some(icon));
    overlay.add_overlay(&badge);
    host_box.prepend(&overlay);
    badge
}

/// Show or hide a rail badge and set its label from an unread count.
pub fn set_badge_count(badge: &gtk::Label, count: u32) {
    badge.set_visible(count > 0);
    badge.set_label(&format_badge_count(count));
}

/// Highlight exactly one space button.
pub fn update_space_selection(
    mail: &gtk::Button,
    contacts: &gtk::Button,
    spaces: &[(&'static str, gtk::Button)],
    space: RailSpace,
) {
    set_rail_active(mail, space == RailSpace::Mail);
    set_rail_active(contacts, space == RailSpace::Contacts);
    for (name, button) in spaces {
        set_rail_active(button, space == RailSpace::Space(name));
    }
}

/// A space button laid out like the built-in Mail and Contacts buttons.
pub fn space_rail_button(title: &str, icon: &gtk::gio::Icon) -> gtk::Button {
    let image = gtk::Image::from_gicon(icon);
    image.set_pixel_size(24);
    let label = gtk::Label::builder()
        .label(title)
        .xalign(0.0)
        .halign(gtk::Align::Start)
        .hexpand(true)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .visible(false)
        .build();
    label.add_css_class(RAIL_LABEL_CLASS);
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    content.add_css_class(RAIL_CONTENT_CLASS);
    content.append(&image);
    content.append(&label);
    let button = gtk::Button::builder()
        .child(&content)
        .tooltip_text(title)
        .hexpand(true)
        .build();
    button.add_css_class("flat");
    button.add_css_class("alcedo-rail-btn");
    button.update_property(&[gtk::accessible::Property::Label(title)]);
    button
}

/// Fraction of the rail's width, from its right edge (the side facing the mail
/// view), where resting the pointer reveals a hidden folder sidebar.
pub const FOLDER_REVEAL_FRACTION: f64 = 0.25;

/// Horizontal extents, in the rail splitter's coordinates, of the rail and of
/// the content beside it. The gap between them is the drag handle.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RailSpan {
    pub left: f64,
    pub right: f64,
    pub content_left: f64,
}

/// Where a pointer at `x` is relative to the rail.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RailPointer {
    /// Over the rail or its drag handle.
    pub on_rail: bool,
    /// In the right quarter of the rail, or on the drag handle beside it.
    pub in_reveal_zone: bool,
}

/// The drag handle belongs to the rail here: it sits between the rail and the
/// content and takes the pointer from both, so leaving it out would leave a dead
/// strip right where the pointer arrives from the content.
pub fn rail_pointer(x: f64, span: RailSpan) -> RailPointer {
    let width = span.right - span.left;
    if width <= 0.0 || x < span.left || x >= span.content_left {
        return RailPointer::default();
    }
    RailPointer {
        on_rail: true,
        in_reveal_zone: x >= span.right - width * FOLDER_REVEAL_FRACTION,
    }
}

/// Report where the pointer is on the rail so a hidden folder sidebar can slide
/// out. `on_reveal_zone` fires when the pointer enters or leaves the reveal
/// zone, and `on_inside` when it enters or leaves the rail (handle included).
/// Both stay false while `active` says the mail view is not the one on screen.
///
/// The controller sits on the splitter, not the rail: the drag handle's hit
/// area overlaps the rail's right edge, so events there never reach the rail.
pub fn watch_folder_reveal(
    split: &gtk::Paned,
    rail: &gtk::Box,
    active: impl Fn() -> bool + 'static,
    on_reveal_zone: impl Fn(bool) + 'static,
    on_inside: impl Fn(bool) + 'static,
) {
    let last = Rc::new(Cell::new(RailPointer::default()));
    let report = Rc::new(move |now: RailPointer, last: &Cell<RailPointer>| {
        let before = last.replace(now);
        if before.in_reveal_zone != now.in_reveal_zone {
            on_reveal_zone(now.in_reveal_zone);
        }
        if before.on_rail != now.on_rail {
            on_inside(now.on_rail);
        }
    });

    // Capture, so a child that handles motion (the web view) cannot hide the
    // pointer leaving the rail.
    let motion = gtk::EventControllerMotion::new();
    motion.set_propagation_phase(gtk::PropagationPhase::Capture);
    motion.connect_motion({
        let split = split.downgrade();
        let rail = rail.downgrade();
        let last = last.clone();
        let report = report.clone();
        move |_, x, _| {
            let pointer = match (split.upgrade(), rail.upgrade()) {
                (Some(split), Some(rail)) if active() => rail_span(&split, &rail)
                    .map_or_else(RailPointer::default, |s| rail_pointer(x, s)),
                _ => RailPointer::default(),
            };
            report(pointer, &last);
        }
    });
    motion.connect_leave(move |_| report(RailPointer::default(), &last));
    split.add_controller(motion);
}

fn rail_span(split: &gtk::Paned, rail: &gtk::Box) -> Option<RailSpan> {
    if !rail.is_visible() {
        return None;
    }
    let rail_bounds = rail.compute_bounds(split)?;
    let content_bounds = split.end_child()?.compute_bounds(split)?;
    Some(RailSpan {
        left: f64::from(rail_bounds.x()),
        right: f64::from(rail_bounds.x() + rail_bounds.width()),
        content_left: f64::from(content_bounds.x()),
    })
}

/// Apply rail layout from paned width: label visibility and expanded styling.
pub fn apply_rail_layout(rail: &gtk::Box, width: i32) {
    let expanded = width >= crate::settings::RAIL_LABEL_THRESHOLD;
    if expanded {
        rail.add_css_class(RAIL_EXPANDED_CLASS);
    } else {
        rail.remove_css_class(RAIL_EXPANDED_CLASS);
    }

    rail.set_halign(if expanded {
        gtk::Align::Fill
    } else {
        gtk::Align::Center
    });
    rail.set_width_request(RAIL_COLLAPSED_WIDTH as i32);
    sync_rail_layout(rail.upcast_ref(), expanded);
}

fn sync_rail_layout(widget: &gtk::Widget, expanded: bool) {
    if widget.has_css_class(RAIL_LABEL_CLASS) {
        widget.set_visible(expanded);
    }
    if widget.has_css_class(RAIL_CONTENT_CLASS) {
        widget.set_halign(if expanded {
            gtk::Align::Fill
        } else {
            gtk::Align::Center
        });
    }

    let mut child = widget.first_child();
    while let Some(current) = child {
        sync_rail_layout(&current, expanded);
        child = current.next_sibling();
    }
}

#[cfg(test)]
mod tests {
    use super::{
        RAIL_COLLAPSED_WIDTH, RAIL_EXPANDED_WIDTH, RailPointer, RailSpan, format_badge_count,
        rail_pointer,
    };
    use crate::settings::RAIL_LABEL_THRESHOLD;

    #[test]
    fn rail_collapsed_width_is_56() {
        assert_eq!(RAIL_COLLAPSED_WIDTH, 56.0);
    }

    #[test]
    fn rail_expanded_width_is_240() {
        assert_eq!(RAIL_EXPANDED_WIDTH, 240.0);
    }

    #[test]
    fn rail_labels_threshold_is_120() {
        assert_eq!(RAIL_LABEL_THRESHOLD, 120);
    }

    #[test]
    fn format_badge_count_zero() {
        assert_eq!(format_badge_count(0), "0");
    }

    #[test]
    fn format_badge_count_small() {
        assert_eq!(format_badge_count(5), "5");
    }

    #[test]
    fn format_badge_count_at_cap() {
        assert_eq!(format_badge_count(99), "99");
    }

    #[test]
    fn format_badge_count_over_cap() {
        assert_eq!(format_badge_count(100), "99+");
        assert_eq!(format_badge_count(250), "99+");
    }

    const SPAN: RailSpan = RailSpan {
        left: 0.0,
        right: 56.0,
        content_left: 64.0,
    };

    #[test]
    fn reveal_zone_is_the_right_quarter_of_the_rail() {
        assert!(rail_pointer(42.0, SPAN).in_reveal_zone);
        assert!(rail_pointer(55.9, SPAN).in_reveal_zone);
        assert!(!rail_pointer(41.9, SPAN).in_reveal_zone);
        assert!(!rail_pointer(0.0, SPAN).in_reveal_zone);
    }

    #[test]
    fn reveal_zone_scales_with_an_expanded_rail() {
        let wide = RailSpan {
            left: 0.0,
            right: 240.0,
            content_left: 248.0,
        };
        assert!(rail_pointer(180.0, wide).in_reveal_zone);
        assert!(!rail_pointer(179.9, wide).in_reveal_zone);
    }

    #[test]
    fn the_drag_handle_counts_as_the_reveal_zone() {
        let on_handle = rail_pointer(60.0, SPAN);
        assert!(on_handle.on_rail && on_handle.in_reveal_zone);
    }

    #[test]
    fn a_pointer_over_the_content_or_beside_the_rail_is_not_on_it() {
        assert_eq!(rail_pointer(64.0, SPAN), RailPointer::default());
        assert_eq!(rail_pointer(300.0, SPAN), RailPointer::default());
        assert_eq!(rail_pointer(-1.0, SPAN), RailPointer::default());
    }

    #[test]
    fn a_collapsed_rail_width_of_zero_never_reveals() {
        let none = RailSpan {
            left: 0.0,
            right: 0.0,
            content_left: 8.0,
        };
        assert_eq!(rail_pointer(0.0, none), RailPointer::default());
    }
}
