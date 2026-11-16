//! Light / dark / system toggle used by the primary menu.
//!
//! Accent color stays in Preferences → Appearance.

use relm4::adw;
use relm4::adw::prelude::*;

use crate::a11y;
use crate::settings::{self, ColorScheme, on_color_scheme_changed};

/// System / Light / Dark toggle. Selecting a row updates GSettings and the style manager.
pub fn build_scheme_toggle() -> adw::ToggleGroup {
    let scheme_group = adw::ToggleGroup::builder().homogeneous(true).build();
    scheme_group.set_hexpand(true);
    scheme_group.add(
        adw::Toggle::builder()
            .name("system")
            .label("System")
            .build(),
    );
    scheme_group.add(adw::Toggle::builder().name("light").label("Light").build());
    scheme_group.add(adw::Toggle::builder().name("dark").label("Dark").build());
    scheme_group.set_active(settings::color_scheme().dropdown_index());
    a11y::set_label(&scheme_group, "Theme");
    scheme_group.connect_notify_local(Some("active"), |group, _| {
        let scheme = ColorScheme::from_dropdown_index(group.active());
        settings::set_color_scheme(scheme);
        crate::theme::apply_color_scheme(scheme);
    });

    let scheme_group_for_sync = scheme_group.clone();
    on_color_scheme_changed(move |scheme| {
        scheme_group_for_sync.set_active(scheme.dropdown_index());
    });

    scheme_group
}
