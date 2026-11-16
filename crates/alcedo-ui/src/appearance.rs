//! Preferences → Appearance page (color scheme, accent color, font sizes).

use relm4::adw;
use relm4::adw::prelude::*;
use relm4::gtk;

use crate::a11y;
use crate::help::{self, TOPIC_APPEARANCE};
use crate::settings::{self, ACCENT_COLORS, AccentColor, on_accent_color_changed};
use crate::theme_switcher::build_scheme_toggle;

/// Build the Appearance preferences page (color scheme, accent color, font sizes).
pub fn build_appearance_page() -> adw::PreferencesPage {
    // Same System / Light / Dark control as the primary menu; it keeps itself in sync.
    let scheme_toggle = build_scheme_toggle();
    scheme_toggle.set_hexpand(false);
    scheme_toggle.set_valign(gtk::Align::Center);

    let scheme_row = adw::ActionRow::builder()
        .title("Color scheme")
        .subtitle("Follow the system, or always use light or dark")
        .activatable_widget(&scheme_toggle)
        .build();
    scheme_row.add_suffix(&scheme_toggle);

    let scheme_group = adw::PreferencesGroup::builder().title("Theme").build();
    help::attach_group_help(&scheme_group, TOPIC_APPEARANCE);
    scheme_group.add(&scheme_row);

    let accent_group = adw::PreferencesGroup::builder()
        .title("Accent color")
        .description("Applies to buttons, links, and highlights throughout Alcedo.")
        .build();
    accent_group.add(&build_accent_row());

    let app_font_row = adw::SpinRow::with_range(10.0, 24.0, 1.0);
    app_font_row.set_title("Application");
    app_font_row.set_subtitle("Folders, lists, and chrome");
    app_font_row.set_digits(0);
    settings::bind_application_font_size(&app_font_row);

    let reader_font_row = adw::SpinRow::with_range(10.0, 24.0, 1.0);
    reader_font_row.set_title("Reader");
    reader_font_row.set_subtitle("Mail in the reading pane");
    reader_font_row.set_digits(0);
    settings::bind_reader_font_size(&reader_font_row);

    let editor_font_row = adw::SpinRow::with_range(10.0, 24.0, 1.0);
    editor_font_row.set_title("Editor");
    editor_font_row.set_subtitle("Plain, HTML, and Markdown compose");
    editor_font_row.set_digits(0);
    settings::bind_editor_font_size(&editor_font_row);

    let font_group = adw::PreferencesGroup::builder().title("Font size").build();
    font_group.add(&app_font_row);
    font_group.add(&reader_font_row);
    font_group.add(&editor_font_row);

    let page = adw::PreferencesPage::builder()
        .title("Appearance")
        .icon_name("preferences-desktop-appearance-symbolic")
        .build();
    page.add(&scheme_group);
    page.add(&accent_group);
    page.add(&font_group);
    page
}

/// One row of round swatches (Default first). Radio behavior comes from the
/// toggle-button group; GSettings changes made elsewhere move the selection.
fn build_accent_row() -> gtk::ListBoxRow {
    let swatches = adw::WrapBox::builder()
        .child_spacing(8)
        .line_spacing(8)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(12)
        .margin_end(12)
        .build();

    let mut buttons: Vec<(AccentColor, gtk::ToggleButton)> = Vec::new();
    for accent in std::iter::once(AccentColor::Default).chain(ACCENT_COLORS) {
        let button = accent_swatch_button(accent);
        if let Some((_, first)) = buttons.first() {
            button.set_group(Some(first));
        }
        swatches.append(&button);
        buttons.push((accent, button));
    }

    let current = settings::accent_color();
    for (accent, button) in &buttons {
        button.set_active(*accent == current);
    }
    for (accent, button) in &buttons {
        let accent = *accent;
        button.connect_toggled(move |button| {
            if button.is_active() {
                settings::set_accent_color(accent);
                crate::theme::apply_accent_color(accent);
            }
        });
    }

    on_accent_color_changed(move |accent| {
        for (candidate, button) in &buttons {
            if *candidate == accent {
                button.set_active(true);
            }
        }
    });

    gtk::ListBoxRow::builder()
        .activatable(false)
        .selectable(false)
        .focusable(false)
        .child(&swatches)
        .build()
}

fn accent_swatch_button(accent: AccentColor) -> gtk::ToggleButton {
    let class = format!("alcedo-swatch-{}", accent.as_str());
    let swatch = gtk::Box::builder()
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .css_classes(["alcedo-swatch", &class])
        .build();
    if accent == AccentColor::Default {
        // The system accent has no fixed color, so mark it with a gear instead.
        let icon = gtk::Image::from_icon_name("preferences-system-symbolic");
        icon.set_pixel_size(12);
        icon.set_hexpand(true);
        icon.set_vexpand(true);
        swatch.append(&icon);
    }

    let button = gtk::ToggleButton::builder()
        .child(&swatch)
        .valign(gtk::Align::Center)
        .css_classes(["flat", "circular", "alcedo-swatch-btn"])
        .build();
    let tooltip = if accent == AccentColor::Default {
        "Default (system accent)"
    } else {
        accent.label()
    };
    a11y::name_icon_control(&button, accent.label(), tooltip);
    button
}
