//! Theme colours in the form web views need.

use gtk4::gdk;
use gtk4::prelude::*;

/// `color` as `#rrggbb`. Alpha is dropped, and out-of-range channels are clamped.
pub fn rgba_hex(color: &gdk::RGBA) -> String {
    format!(
        "#{:02x}{:02x}{:02x}",
        (color.red().clamp(0.0, 1.0) * 255.0).round() as u8,
        (color.green().clamp(0.0, 1.0) * 255.0).round() as u8,
        (color.blue().clamp(0.0, 1.0) * 255.0).round() as u8
    )
}

/// The first of the named theme colours that `widget`'s style defines.
#[allow(deprecated)]
pub fn lookup_css_color(widget: &gtk4::Widget, names: &[&str]) -> Option<gdk::RGBA> {
    // GTK 4.10 deprecated StyleContext colour lookup and did not replace it.
    let context = widget.style_context();
    names.iter().find_map(|name| context.lookup_color(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_channels_as_lowercase_hex() {
        assert_eq!(rgba_hex(&gdk::RGBA::new(1.0, 0.5, 0.0, 1.0)), "#ff8000");
    }

    #[test]
    fn clamps_out_of_range_channels() {
        assert_eq!(rgba_hex(&gdk::RGBA::new(2.0, -1.0, 1.0, 1.0)), "#ff00ff");
    }

    #[test]
    fn ignores_alpha() {
        assert_eq!(rgba_hex(&gdk::RGBA::new(0.0, 0.0, 0.0, 0.25)), "#000000");
    }
}
