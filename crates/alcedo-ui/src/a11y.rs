//! Accessibility helpers for icon-only controls.
//!
//! A tooltip is not an accessible name: screen readers announce icon-only
//! buttons by their icon name ("edit-delete-symbolic") unless the widget also
//! carries `GtkAccessibleProperty::Label`. Set both through these helpers.

use relm4::gtk;
use relm4::gtk::prelude::*;

/// Give `widget` an accessible name.
pub fn set_label(widget: &impl IsA<gtk::Accessible>, label: &str) {
    widget.update_property(&[gtk::accessible::Property::Label(label)]);
}

/// Name an icon-only control: the accessible label is `label`; the tooltip
/// is `tooltip` (which may add a shortcut hint, e.g. "Reply (Ctrl+R)").
pub fn name_icon_control(widget: &impl IsA<gtk::Widget>, label: &str, tooltip: &str) {
    widget.set_tooltip_text(Some(tooltip));
    set_label(widget.upcast_ref::<gtk::Widget>(), label);
}

/// Name an icon-only control whose tooltip is just its label.
pub fn label_icon_control(widget: &impl IsA<gtk::Widget>, label: &str) {
    name_icon_control(widget, label, label);
}
