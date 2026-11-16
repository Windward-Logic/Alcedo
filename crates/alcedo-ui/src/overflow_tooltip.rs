//! Overflow-only native GTK tooltips for ellipsized labels.

use relm4::gtk;
use relm4::gtk::prelude::*;

pub fn overflow_tooltip_text(full_text: &str, ellipsized: bool) -> Option<&str> {
    if full_text.is_empty() || !ellipsized {
        None
    } else {
        Some(full_text)
    }
}

pub fn install_overflow_tooltip(label: &gtk::Label) {
    label.set_has_tooltip(true);
    label.connect_query_tooltip(|label, _x, _y, _keyboard, tooltip| {
        let text = label.text();
        if overflow_tooltip_text(text.as_str(), label.layout().is_ellipsized()).is_none() {
            return false;
        }
        tooltip.set_text(Some(text.as_str()));
        true
    });
}

#[cfg(test)]
mod tests {
    use super::overflow_tooltip_text;

    #[test]
    fn empty_text_never_shows_tooltip() {
        assert_eq!(overflow_tooltip_text("", false), None);
        assert_eq!(overflow_tooltip_text("", true), None);
    }

    #[test]
    fn visible_text_never_shows_tooltip() {
        assert_eq!(overflow_tooltip_text("Inbox", false), None);
    }

    #[test]
    fn ellipsized_text_shows_tooltip() {
        assert_eq!(
            overflow_tooltip_text("Very Long Folder Name", true),
            Some("Very Long Folder Name")
        );
    }

    #[test]
    fn whitespace_only_ellipsized_still_shows() {
        assert_eq!(overflow_tooltip_text("   ", true), Some("   "));
    }
}
