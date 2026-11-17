//! Theme, app icon, and CSS (P4).

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use relm4::adw;
use relm4::gtk;
use relm4::gtk::gdk;
use relm4::gtk::gdk::prelude::*;
use relm4::gtk::gdk_pixbuf;
use relm4::gtk::gdk_pixbuf::prelude::PixbufLoaderExt;
use relm4::gtk::gio;
use relm4::gtk::glib;
use relm4::gtk::prelude::*;

use crate::settings::{
    AccentColor, ColorScheme, accent_color, application_font_size, color_scheme, editor_font_size,
    on_accent_color_changed, on_application_font_size_changed, on_color_scheme_changed,
    on_editor_font_size_changed,
};

const APP_ID: &str = "dev.alcedo.Mail";
const STYLE_CSS: &str = include_str!("../../../data/resources/style.css");
const APP_ICON_SVG: &str =
    include_str!("../../../data/icons/hicolor/scalable/apps/dev.alcedo.Mail.svg");
const APP_DESKTOP_TEMPLATE: &str = include_str!("../../../data/dev.alcedo.Mail.desktop");
const UNPACKAGED_MARKER: &str = "X-Alcedo-Unpackaged=true";
const PNG_SIZES: [i32; 7] = [16, 22, 24, 32, 48, 64, 128];
const TRAY_PIXMAP_SIZES: [i32; 3] = [22, 32, 48];

/// ARGB32 pixmap for StatusNotifierItem (Ubuntu AppIndicator / taskbar tray).
#[derive(Clone)]
pub struct ArgbIcon {
    pub width: i32,
    pub height: i32,
    pub argb32: Vec<u8>,
}

/// Write the app icon and desktop file into the user data dir so the compositor
/// can resolve `dev.alcedo.Mail` (GTK's icon theme is not visible to the shell).
/// Call before GTK initialization.
pub fn install_xdg_app_icon() {
    if std::env::var_os("FLATPAK_ID").is_some() {
        return;
    }
    let Some(data_home) = xdg_data_home() else {
        return;
    };

    let icon_path = data_home
        .join("icons/hicolor/scalable/apps")
        .join(format!("{APP_ID}.svg"));
    if let Err(error) = write_if_changed(&icon_path, APP_ICON_SVG.as_bytes()) {
        tracing::warn!(%error, path = %icon_path.display(), "failed to install app icon");
    }
    install_bundled_action_icons(&data_home);
    install_raster_icons(&data_home);
    refresh_icon_cache(&data_home.join("icons/hicolor"));

    let desktop_path = data_home
        .join("applications")
        .join(format!("{APP_ID}.desktop"));
    if !should_write_unpackaged_desktop(&desktop_path) {
        return;
    }

    let desktop = unpackaged_desktop_entry(&icon_path);
    if let Err(error) = write_if_changed(&desktop_path, desktop.as_bytes()) {
        tracing::warn!(
            %error,
            path = %desktop_path.display(),
            "failed to install desktop entry"
        );
    }
}

/// Apply GNOME-following color scheme, register the app icon, and load CSS.
pub fn init() {
    register_app_icon();
    apply_color_scheme(color_scheme());
    relm4::set_global_css(&app_css(STYLE_CSS, &extension_stylesheets()));
    apply_accent_color(accent_color());
    apply_application_font_size(application_font_size());
    apply_editor_font_size(editor_font_size());
    on_color_scheme_changed(apply_color_scheme);
    on_accent_color_changed(apply_accent_color);
    on_application_font_size_changed(apply_application_font_size);
    on_editor_font_size_changed(apply_editor_font_size);
}

fn extension_stylesheets() -> Vec<&'static str> {
    crate::extension::registered()
        .iter()
        .filter_map(|extension| extension.stylesheet())
        .collect()
}

/// The app stylesheet followed by each extension's, so an extension's rules
/// win ties with the app's.
fn app_css(base: &str, extensions: &[&str]) -> String {
    let mut css = base.to_string();
    for sheet in extensions {
        css.push('\n');
        css.push_str(sheet);
    }
    css
}

/// Apply the persisted color scheme to the `AdwStyleManager`.
pub fn apply_color_scheme(scheme: ColorScheme) {
    adw::StyleManager::default().set_color_scheme(scheme.to_adw());
}

thread_local! {
    static ACCENT_PROVIDER: std::cell::OnceCell<gtk::CssProvider> = const { std::cell::OnceCell::new() };
}

/// Override `--accent-bg-color` (and `--accent-fg-color` for Yellow) app-wide.
/// `Default` clears the override so libadwaita's system-accent-following
/// default applies. `--accent-color` is derived automatically by libadwaita's
/// own stylesheet via `oklab(from var(--accent-bg-color) ...)`.
pub fn apply_accent_color(accent: AccentColor) {
    ACCENT_PROVIDER.with(|cell| {
        let provider = cell.get_or_init(|| {
            let provider = gtk::CssProvider::new();
            if let Some(display) = gdk::Display::default() {
                gtk::style_context_add_provider_for_display(
                    &display,
                    &provider,
                    gtk::STYLE_PROVIDER_PRIORITY_USER,
                );
            }
            provider
        });
        let css = match accent {
            AccentColor::Default => String::new(),
            AccentColor::Yellow => format!(
                ":root {{ --accent-bg-color: var(--accent-{0}); --accent-fg-color: rgb(0 0 0 / 80%); }}",
                accent.as_str()
            ),
            _ => format!(
                ":root {{ --accent-bg-color: var(--accent-{0}); }}",
                accent.as_str()
            ),
        };
        provider.load_from_string(&css);
    });
}

thread_local! {
    static APPLICATION_FONT_PROVIDER: std::cell::OnceCell<gtk::CssProvider> = const { std::cell::OnceCell::new() };
}

/// CSS class for secondary app windows (compose, message) that should follow
/// Application font size. The main shell uses widget name `alcedo-main`.
pub const APPLICATION_FONT_WINDOW_CLASS: &str = "alcedo-application-font";

pub fn application_font_size_css(px: i32) -> String {
    let px = crate::settings::clamp_font_size(px);
    format!("#alcedo-main, .{APPLICATION_FONT_WINDOW_CLASS} {{ font-size: {px}px; }}")
}

fn apply_application_font_size_now(px: i32) {
    let css = application_font_size_css(px);
    APPLICATION_FONT_PROVIDER.with(|cell| {
        let provider = cell.get_or_init(|| {
            let provider = gtk::CssProvider::new();
            if let Some(display) = gdk::Display::default() {
                gtk::style_context_add_provider_for_display(
                    &display,
                    &provider,
                    gtk::STYLE_PROVIDER_PRIORITY_USER,
                );
            }
            provider
        });
        provider.load_from_string(&css);
    });
}

/// Apply Application font size to mail chrome windows, not preferences overlays.
pub fn apply_application_font_size(px: i32) {
    let px = crate::settings::clamp_font_size(px);
    glib::idle_add_local_once(move || apply_application_font_size_now(px));
}

thread_local! {
    static EDITOR_FONT_PROVIDER: std::cell::OnceCell<gtk::CssProvider> = const { std::cell::OnceCell::new() };
}

pub fn apply_editor_font_size(px: i32) {
    let px = crate::settings::clamp_font_size(px);
    EDITOR_FONT_PROVIDER.with(|cell| {
        let provider = cell.get_or_init(|| {
            let provider = gtk::CssProvider::new();
            if let Some(display) = gdk::Display::default() {
                gtk::style_context_add_provider_for_display(
                    &display,
                    &provider,
                    gtk::STYLE_PROVIDER_PRIORITY_USER,
                );
            }
            provider
        });
        provider.load_from_string(&format!(
            ".alcedo-composer textview, .alcedo-composer-source textview {{ font-size: {px}px; }}"
        ));
    });
}

/// Directory AppIndicator should search for `dev.alcedo.Mail.png`.
pub fn tray_icon_theme_path() -> String {
    xdg_data_home()
        .map(|home| home.join("alcedo/icons").display().to_string())
        .unwrap_or_default()
}

/// Raster ARGB32 icons for the StatusNotifierItem pixmap property.
pub fn app_icon_argb_pixmaps() -> Vec<ArgbIcon> {
    static ICONS: OnceLock<Vec<ArgbIcon>> = OnceLock::new();
    ICONS
        .get_or_init(|| {
            TRAY_PIXMAP_SIZES
                .into_iter()
                .filter_map(|size| rasterize_svg(size).and_then(|pixbuf| pixbuf_to_argb(&pixbuf)))
                .collect()
        })
        .clone()
}

/// Set the Wayland/X11 toplevel icon from an embedded texture (xdg-toplevel-icon).
/// Call after the window is mapped so the compositor can receive the pixmap.
pub fn apply_toplevel_icon(window: &impl IsA<gtk::Native>) {
    let Some(texture) = app_icon_texture() else {
        return;
    };
    let Some(surface) = window.surface() else {
        return;
    };
    let Ok(toplevel) = surface.dynamic_cast::<gdk::Toplevel>() else {
        return;
    };
    toplevel.set_icon_list(&[texture]);
}

fn register_app_icon() {
    gio::resources_register_include!("alcedo.gresource").expect("register Alcedo icon gresource");
    gio::resources_register_include!("alcedo-resources.gresource")
        .expect("register Alcedo resources gresource");

    if let Some(display) = gdk::Display::default() {
        let theme = gtk::IconTheme::for_display(&display);
        // add_resource_path is the hicolor *root*: lookup is {path}/scalable/apps/{name}.svg
        theme.add_resource_path("/dev/alcedo/Mail/icons/hicolor");
        if let Some(icons) = xdg_data_home() {
            theme.add_search_path(icons.join("icons"));
        }
        // Dev convenience: load icons from the source tree. Release binaries must not embed or search the build path.
        #[cfg(debug_assertions)]
        if let Ok(source_icons) = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../data/icons")
            .canonicalize()
        {
            theme.add_search_path(source_icons);
        }
    }
    gtk::Window::set_default_icon_name(APP_ID);
}

fn app_icon_texture() -> Option<gdk::Texture> {
    static TEXTURE: OnceLock<Option<gdk::Texture>> = OnceLock::new();
    TEXTURE
        .get_or_init(|| {
            let pixbuf = rasterize_svg(128)?;
            match pixbuf.save_to_bufferv("png", &[]) {
                Ok(png) => match gdk::Texture::from_bytes(&glib::Bytes::from(png.as_slice())) {
                    Ok(texture) => Some(texture),
                    Err(error) => {
                        tracing::warn!(%error, "failed to decode raster app icon");
                        None
                    }
                },
                Err(error) => {
                    tracing::warn!(%error, "failed to encode raster app icon");
                    None
                }
            }
        })
        .clone()
}

fn rasterize_svg(size: i32) -> Option<gdk_pixbuf::Pixbuf> {
    let loader = match gdk_pixbuf::PixbufLoader::with_mime_type("image/svg+xml") {
        Ok(loader) => loader,
        Err(error) => {
            tracing::warn!(%error, "SVG pixbuf loader unavailable");
            return None;
        }
    };
    loader.set_size(size, size);
    if let Err(error) = loader.write(APP_ICON_SVG.as_bytes()) {
        tracing::warn!(%error, size, "failed to decode app icon SVG");
        return None;
    }
    if let Err(error) = loader.close() {
        tracing::warn!(%error, size, "failed to close app icon loader");
        return None;
    }
    loader.pixbuf()
}

fn pixbuf_to_argb(pixbuf: &gdk_pixbuf::Pixbuf) -> Option<ArgbIcon> {
    let width = pixbuf.width();
    let height = pixbuf.height();
    let rowstride = pixbuf.rowstride();
    let n_channels = pixbuf.n_channels();
    let has_alpha = pixbuf.has_alpha();
    let pixels = pixbuf.pixel_bytes()?;
    let src = pixels.as_ref();
    let mut argb32 = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        let row = (y * rowstride) as usize;
        for x in 0..width {
            let i = row + (x * n_channels) as usize;
            let r = src[i];
            let g = src[i + 1];
            let b = src[i + 2];
            let a = if has_alpha { src[i + 3] } else { 255 };
            argb32.extend_from_slice(&[a, r, g, b]);
        }
    }
    Some(ArgbIcon {
        width,
        height,
        argb32,
    })
}

fn install_bundled_action_icons(data_home: &Path) {
    let hicolor = data_home.join("icons/hicolor");
    for (rel, bytes) in bundled_action_icons() {
        let path = hicolor.join(rel);
        if let Err(error) = write_if_changed(&path, bytes) {
            tracing::warn!(%error, path = %path.display(), "failed to install action icon");
        }
    }
}

fn bundled_action_icons() -> &'static [(&'static str, &'static [u8])] {
    &[
        (
            "scalable/actions/alcedo-rail-symbolic.svg",
            include_bytes!("../../../data/icons/hicolor/scalable/actions/alcedo-rail-symbolic.svg"),
        ),
        (
            "scalable/actions/alcedo-rail-hide-symbolic.svg",
            include_bytes!(
                "../../../data/icons/hicolor/scalable/actions/alcedo-rail-hide-symbolic.svg"
            ),
        ),
        (
            "scalable/actions/alcedo-folder-pane-symbolic.svg",
            include_bytes!(
                "../../../data/icons/hicolor/scalable/actions/alcedo-folder-pane-symbolic.svg"
            ),
        ),
        (
            "scalable/actions/alcedo-compose-symbolic.svg",
            include_bytes!(
                "../../../data/icons/hicolor/scalable/actions/alcedo-compose-symbolic.svg"
            ),
        ),
        (
            "scalable/actions/alcedo-mail-preview-symbolic.svg",
            include_bytes!(
                "../../../data/icons/hicolor/scalable/actions/alcedo-mail-preview-symbolic.svg"
            ),
        ),
        (
            "scalable/actions/alcedo-columns-symbolic.svg",
            include_bytes!(
                "../../../data/icons/hicolor/scalable/actions/alcedo-columns-symbolic.svg"
            ),
        ),
        (
            "scalable/actions/alcedo-reading-layout-symbolic.svg",
            include_bytes!(
                "../../../data/icons/hicolor/scalable/actions/alcedo-reading-layout-symbolic.svg"
            ),
        ),
        (
            "256x256/actions/alcedo-mail.png",
            include_bytes!("../../../data/icons/hicolor/256x256/actions/alcedo-mail.png"),
        ),
        (
            "256x256/actions/alcedo-contacts.png",
            include_bytes!("../../../data/icons/hicolor/256x256/actions/alcedo-contacts.png"),
        ),
        (
            "256x256/actions/alcedo-compose.png",
            include_bytes!("../../../data/icons/hicolor/256x256/actions/alcedo-compose.png"),
        ),
    ]
}

fn install_raster_icons(data_home: &Path) {
    for size in PNG_SIZES {
        let Some(pixbuf) = rasterize_svg(size) else {
            continue;
        };
        let path = data_home.join(format!("icons/hicolor/{size}x{size}/apps/{APP_ID}.png"));
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Err(error) = pixbuf.savev(&path, "png", &[]) {
            tracing::warn!(%error, path = %path.display(), "failed to install raster app icon");
        }
    }

    if let Some(pixbuf) = rasterize_svg(48) {
        let dir = data_home.join("alcedo/icons");
        let _ = fs::create_dir_all(&dir);
        let path = dir.join(format!("{APP_ID}.png"));
        if let Err(error) = pixbuf.savev(&path, "png", &[]) {
            tracing::warn!(
                %error,
                path = %path.display(),
                "failed to install tray icon theme file"
            );
        }
    }
}

fn xdg_data_home() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("XDG_DATA_HOME")
        && !dir.is_empty()
    {
        return Some(PathBuf::from(dir));
    }
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share"))
}

fn should_write_unpackaged_desktop(user_desktop: &Path) -> bool {
    if let Ok(existing) = fs::read_to_string(user_desktop) {
        return existing.contains(UNPACKAGED_MARKER);
    }
    !system_desktop_entry_exists()
}

fn system_desktop_entry_exists() -> bool {
    let dirs =
        std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".into());
    dirs.split(':').filter(|dir| !dir.is_empty()).any(|dir| {
        Path::new(dir)
            .join(format!("applications/{APP_ID}.desktop"))
            .is_file()
    })
}

fn unpackaged_desktop_entry(icon_path: &Path) -> String {
    let exe = std::env::current_exe()
        .ok()
        .and_then(|path| path.canonicalize().ok())
        .map(|path| quote_desktop_exec(&path))
        .unwrap_or_else(|| "alcedo".into());

    let mut entry = APP_DESKTOP_TEMPLATE.to_string();
    if let Some(line) = entry.lines().find(|line| line.starts_with("Exec=")) {
        entry = entry.replace(line, &format!("Exec={exe} %u"));
    }
    if let Some(line) = entry.lines().find(|line| line.starts_with("Icon=")) {
        entry = entry.replace(line, &format!("Icon={}", icon_path.display()));
    }
    if !entry.contains(UNPACKAGED_MARKER) {
        if !entry.ends_with('\n') {
            entry.push('\n');
        }
        entry.push_str(UNPACKAGED_MARKER);
        entry.push('\n');
    }
    entry
}

fn quote_desktop_exec(path: &Path) -> String {
    let text = path.display().to_string();
    if text
        .chars()
        .any(|c| c.is_whitespace() || "\"'\\$`".contains(c))
    {
        format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        text
    }
}

fn write_if_changed(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    if path.is_file()
        && let Ok(existing) = fs::read(path)
        && existing == contents
    {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, contents)
}

fn refresh_icon_cache(hicolor: &Path) {
    let _ = fs::remove_file(hicolor.join("icon-theme.cache"));
    let _ = std::process::Command::new("gtk4-update-icon-cache")
        .args(["-f", "-t", "-q"])
        .arg(hicolor)
        .status();
}

#[cfg(test)]
mod tests {
    #[test]
    fn extension_css_follows_the_app_css() {
        use super::app_css;
        assert_eq!(app_css(".a {}", &[]), ".a {}");
        assert_eq!(app_css(".a {}", &[".b {}", ".c {}"]), ".a {}\n.b {}\n.c {}");
    }

    use super::{APPLICATION_FONT_WINDOW_CLASS, application_font_size_css};

    #[test]
    fn application_font_size_css_targets_app_shell_not_all_windows() {
        let css = application_font_size_css(16);
        assert!(css.contains("#alcedo-main"));
        assert!(css.contains(APPLICATION_FONT_WINDOW_CLASS));
        assert!(!css.contains("window {"));
        assert!(css.contains("font-size: 16px"));
    }
}
