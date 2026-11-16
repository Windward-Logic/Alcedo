//! App rail sidebar (P4).

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
    use super::{RAIL_COLLAPSED_WIDTH, RAIL_EXPANDED_WIDTH, format_badge_count};
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
}
