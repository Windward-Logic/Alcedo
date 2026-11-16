//! Bundled Alcedo icons, loaded from gresource (not the icon theme).
//!
//! GTK IconTheme lookup of custom names is unreliable here: Yaru does not
//! pick up extra hicolor action SVGs, and a user `icon-theme.cache` can hide
//! them. `GtkIconPaintable` from a `resource://` URI always paints, and
//! filenames ending in `-symbolic.svg` still recolor with the theme.

use relm4::gtk;
use relm4::gtk::gio;
use relm4::gtk::prelude::*;

const RESOURCE_ROOT: &str = "/dev/alcedo/Mail/icons/hicolor/";

pub const COMPOSE: &str = "alcedo-compose";
pub const MAIL: &str = "alcedo-mail";
pub const CONTACTS: &str = "alcedo-contacts";
pub const COMPOSE_SYMBOLIC: &str = "alcedo-compose-symbolic";
pub const RAIL: &str = "alcedo-rail-symbolic";
pub const RAIL_HIDE: &str = "alcedo-rail-hide-symbolic";
pub const FOLDER_PANE: &str = "alcedo-folder-pane-symbolic";
pub const MAIL_PREVIEW: &str = "alcedo-mail-preview-symbolic";
pub const COLUMNS: &str = "alcedo-columns-symbolic";
pub const READING_LAYOUT: &str = "alcedo-reading-layout-symbolic";
pub const SEND_SYMBOLIC: &str = "alcedo-send-symbolic";
pub const RULES_SYMBOLIC: &str = "alcedo-rules-symbolic";
pub const SERVER_SCAN: &str = "alcedo-server-scan-symbolic";

pub const EVERYTHING_SIDEBAR: &str = "alcedo-everything-symbolic";

/// Logical sizes are multiples of 6 so device pixels stay whole at 100%, 133%, 150% and 200%.
pub const RAIL_PIXEL_SIZE: i32 = 24;
pub const SIDEBAR_PIXEL_SIZE: i32 = 18;
pub const TOOLBAR_PIXEL_SIZE: i32 = 16;

/// Sidebar glyphs, keyed by the freedesktop name the folder code already uses.
const SIDEBAR_ICONS: &[(&str, &str)] = &[
    (EVERYTHING_SIDEBAR, "alcedo-sidebar-everything-symbolic.svg"),
    ("mail-inbox-symbolic", "alcedo-sidebar-inbox-symbolic.svg"),
    (
        "system-users-symbolic",
        "alcedo-sidebar-people-symbolic.svg",
    ),
    ("mail-unread-symbolic", "alcedo-sidebar-unread-symbolic.svg"),
    ("mail-drafts-symbolic", "alcedo-sidebar-drafts-symbolic.svg"),
    ("mail-sent-symbolic", "alcedo-sidebar-sent-symbolic.svg"),
    (
        "mail-archive-symbolic",
        "alcedo-sidebar-archive-symbolic.svg",
    ),
    (
        "package-x-generic-symbolic",
        "alcedo-sidebar-archive-symbolic.svg",
    ),
    (
        "mail-mark-junk-symbolic",
        "alcedo-sidebar-junk-symbolic.svg",
    ),
    ("user-trash-symbolic", "alcedo-sidebar-trash-symbolic.svg"),
    ("alarm-symbolic", "alcedo-sidebar-snooze-symbolic.svg"),
    (
        "document-send-symbolic",
        "alcedo-sidebar-outbox-symbolic.svg",
    ),
    (
        "preferences-system-time-symbolic",
        "alcedo-sidebar-followup-symbolic.svg",
    ),
    ("starred-symbolic", "alcedo-sidebar-star-symbolic.svg"),
    (
        "mail-attachment-symbolic",
        "alcedo-sidebar-attachment-symbolic.svg",
    ),
    ("folder-symbolic", "alcedo-sidebar-folder-symbolic.svg"),
    (
        "folder-saved-search-symbolic",
        "alcedo-sidebar-saved-search-symbolic.svg",
    ),
    ("edit-find-symbolic", "alcedo-sidebar-search-symbolic.svg"),
];

fn resource_rel(name: &str) -> Option<&'static str> {
    Some(match name {
        COMPOSE => "48x48/actions/alcedo-compose.png",
        MAIL => "48x48/actions/alcedo-mail.png",
        CONTACTS => "48x48/actions/alcedo-contacts.png",
        COMPOSE_SYMBOLIC => "scalable/actions/alcedo-compose-symbolic.svg",
        RAIL => "scalable/actions/alcedo-rail-symbolic.svg",
        RAIL_HIDE => "scalable/actions/alcedo-rail-hide-symbolic.svg",
        FOLDER_PANE => "scalable/actions/alcedo-folder-pane-symbolic.svg",
        MAIL_PREVIEW => "scalable/actions/alcedo-mail-preview-symbolic.svg",
        COLUMNS => "scalable/actions/alcedo-columns-symbolic.svg",
        READING_LAYOUT => "scalable/actions/alcedo-reading-layout-symbolic.svg",
        SEND_SYMBOLIC => "scalable/actions/alcedo-send-symbolic.svg",
        RULES_SYMBOLIC => "scalable/actions/alcedo-rules-symbolic.svg",
        SERVER_SCAN => "scalable/actions/alcedo-server-scan-symbolic.svg",
        _ => return None,
    })
}

fn sidebar_resource_rel(name: &str) -> Option<String> {
    SIDEBAR_ICONS
        .iter()
        .find(|(icon_name, _)| *icon_name == name)
        .map(|(_, file)| format!("scalable/actions/{file}"))
}

/// Whole-number scale to rasterize at. Rendering at 1x and stretching to 133% or 150% is what
/// makes icons look soft, so round the display's scale up and let GTK shrink the result.
fn raster_scale() -> i32 {
    gtk::gdk::Display::default()
        .and_then(|display| display.monitors().item(0))
        .and_then(|monitor| monitor.downcast::<gtk::gdk::Monitor>().ok())
        .map_or(1, |monitor| monitor.scale().ceil() as i32)
        .max(1)
}

fn icon_paintable(rel: &str, pixel_size: i32) -> gtk::IconPaintable {
    let uri = format!("resource://{RESOURCE_ROOT}{rel}");
    let file = gio::File::for_uri(&uri);
    gtk::IconPaintable::for_file(&file, pixel_size, raster_scale())
}

/// Paintable for a bundled icon, for widgets that take a paintable (e.g. `adw::StatusPage`).
pub fn paintable(name: &str, pixel_size: i32) -> Option<gtk::IconPaintable> {
    Some(icon_paintable(resource_rel(name)?, pixel_size))
}

/// Paint a bundled icon onto an existing `gtk::Image`.
pub fn set_image(image: &gtk::Image, name: &str, pixel_size: i32) {
    image.set_pixel_size(pixel_size);
    let Some(rel) = resource_rel(name) else {
        tracing::warn!(name, "unknown bundled icon");
        return;
    };
    image.set_paintable(Some(&icon_paintable(rel, pixel_size)));
}

/// Sidebar icon for a freedesktop icon name. Names without a bundled glyph use the icon theme.
pub fn sidebar_image(icon_name: &str) -> gtk::Image {
    let image = gtk::Image::new();
    image.set_pixel_size(SIDEBAR_PIXEL_SIZE);
    match sidebar_resource_rel(icon_name) {
        Some(rel) => image.set_paintable(Some(&icon_paintable(&rel, SIDEBAR_PIXEL_SIZE))),
        None => image.set_icon_name(Some(icon_name)),
    }
    image
}

/// Replace a button or menu-button's icon with a bundled image.
pub fn set_button_icon(widget: &impl IsA<gtk::Widget>, name: &str, pixel_size: i32) {
    let image = gtk::Image::new();
    set_image(&image, name, pixel_size);
    let widget = widget.as_ref();
    if let Ok(btn) = widget.clone().downcast::<gtk::MenuButton>() {
        btn.set_child(Some(&image));
    } else if let Ok(btn) = widget.clone().downcast::<gtk::Button>() {
        btn.set_child(Some(&image));
    } else {
        tracing::warn!("bundled icon target is not a button");
    }
}

/// Apply a bundled icon to the `gtk::Image` that is the first child of a rail button.
pub fn set_rail_button_icon(button: &gtk::Button, name: &str) {
    let Some(child) = button.child() else {
        return;
    };
    let Ok(box_) = child.downcast::<gtk::Box>() else {
        return;
    };
    let Some(first) = box_.first_child() else {
        return;
    };
    if let Ok(image) = first.downcast::<gtk::Image>() {
        set_image(&image, name, RAIL_PIXEL_SIZE);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        COLUMNS, COMPOSE, COMPOSE_SYMBOLIC, CONTACTS, FOLDER_PANE, MAIL, MAIL_PREVIEW, RAIL,
        RAIL_HIDE, READING_LAYOUT, RULES_SYMBOLIC, SEND_SYMBOLIC, SIDEBAR_ICONS, resource_rel,
        sidebar_resource_rel,
    };

    #[test]
    fn bundled_icon_names_resolve_to_resources() {
        for name in [
            COMPOSE,
            MAIL,
            CONTACTS,
            COMPOSE_SYMBOLIC,
            RAIL,
            RAIL_HIDE,
            FOLDER_PANE,
            MAIL_PREVIEW,
            COLUMNS,
            READING_LAYOUT,
            SEND_SYMBOLIC,
            RULES_SYMBOLIC,
        ] {
            assert!(
                resource_rel(name).is_some(),
                "missing resource mapping for {name}"
            );
        }
    }

    #[test]
    fn every_sidebar_icon_name_resolves_to_a_svg() {
        for (name, _) in SIDEBAR_ICONS {
            let rel = sidebar_resource_rel(name).expect("sidebar mapping");
            assert!(rel.ends_with("-symbolic.svg"), "{name} -> {rel}");
        }
    }

    #[test]
    fn unmapped_sidebar_name_falls_back_to_theme() {
        assert!(sidebar_resource_rel("weather-clear-symbolic").is_none());
    }

    #[test]
    fn unknown_icon_name_is_none() {
        assert!(resource_rel("sidebar-show-symbolic").is_none());
    }
}
