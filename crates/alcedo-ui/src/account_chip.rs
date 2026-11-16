//! Account chip appearance (initials, background color, cropped photo).

use std::f64::consts::TAU;
use std::io::Cursor;

use alcedo_core::{Account, DEFAULT_ACCENT_COLOR, avatar_chip_words, sanitize_accent_color};
use relm4::adw;
use relm4::adw::prelude::*;
use relm4::gtk::cairo;
use relm4::gtk::gdk;
use relm4::gtk::gdk_pixbuf;
use relm4::gtk::glib;

const AVATAR_IMAGE_SIZE: i32 = 256;
const MAX_AVATAR_SOURCE_BYTES: usize = 10 * 1024 * 1024;
const COLOR_TEXTURE_SIZE: i32 = 128;

/// Apply initials, background color, and optional photo to an account chip.
pub fn apply_account_chip(chip: &adw::Avatar, account: &Account) {
    apply_account_chip_appearance(
        chip,
        &account.avatar_chip(),
        &account.accent_color,
        &account.avatar_image,
    );
}

pub fn apply_account_chip_appearance(
    chip: &adw::Avatar,
    initials: &str,
    accent: &str,
    image: &[u8],
) {
    let words = avatar_chip_words(initials);
    chip.set_text(Some(&words));
    chip.set_show_initials(true);
    if let Some(texture) = texture_from_png(image) {
        chip.set_custom_image(Some(&texture));
        return;
    }
    if let Some(texture) = color_chip_texture(accent, initials) {
        chip.set_custom_image(Some(&texture));
    } else {
        chip.set_custom_image(None::<&gdk::Paintable>);
    }
}

pub fn rgba_from_hex(hex: &str) -> gdk::RGBA {
    gdk::RGBA::parse(sanitize_accent_color(hex))
        .unwrap_or_else(|_| gdk::RGBA::parse(DEFAULT_ACCENT_COLOR).unwrap_or(gdk::RGBA::BLUE))
}

pub fn hex_from_rgba(rgba: &gdk::RGBA) -> String {
    sanitize_accent_color(&format!(
        "#{:02x}{:02x}{:02x}",
        (rgba.red().clamp(0.0, 1.0) * 255.0).round() as u8,
        (rgba.green().clamp(0.0, 1.0) * 255.0).round() as u8,
        (rgba.blue().clamp(0.0, 1.0) * 255.0).round() as u8,
    ))
}

/// Center-crop to a square, scale, and clip to a circle for `AdwAvatar`.
pub fn crop_avatar_image(bytes: &[u8]) -> Result<Vec<u8>, &'static str> {
    if bytes.is_empty() {
        return Err("Could not read image");
    }
    if bytes.len() > MAX_AVATAR_SOURCE_BYTES {
        return Err("Image is too large (max 10 MB)");
    }
    let pixbuf = gdk_pixbuf::Pixbuf::from_read(Cursor::new(bytes.to_vec()))
        .map_err(|_| "Could not read image")?;
    let pixbuf = pixbuf.apply_embedded_orientation().unwrap_or(pixbuf);
    let width = pixbuf.width();
    let height = pixbuf.height();
    if width < 1 || height < 1 {
        return Err("Could not read image");
    }
    let side = width.min(height);
    let x = (width - side) / 2;
    let y = (height - side) / 2;
    let square = pixbuf.new_subpixbuf(x, y, side, side);
    let sized = if side == AVATAR_IMAGE_SIZE {
        square.copy().ok_or("Could not crop image")?
    } else {
        square
            .scale_simple(
                AVATAR_IMAGE_SIZE,
                AVATAR_IMAGE_SIZE,
                gdk_pixbuf::InterpType::Bilinear,
            )
            .ok_or("Could not crop image")?
    };
    let sized = if sized.has_alpha() {
        sized
    } else {
        sized
            .add_alpha(false, 0, 0, 0)
            .map_err(|_| "Could not crop image")?
    };
    apply_circle_alpha(&sized);
    sized
        .save_to_bufferv("png", &[])
        .map_err(|_| "Could not save image")
}

fn apply_circle_alpha(pixbuf: &gdk_pixbuf::Pixbuf) {
    let width = pixbuf.width();
    let height = pixbuf.height();
    let cx = (f64::from(width) - 1.0) / 2.0;
    let cy = (f64::from(height) - 1.0) / 2.0;
    let radius = f64::from(width.min(height)) / 2.0;
    let radius_sq = radius * radius;
    for y in 0..height {
        for x in 0..width {
            let dx = f64::from(x) - cx;
            let dy = f64::from(y) - cy;
            if dx * dx + dy * dy > radius_sq {
                pixbuf.put_pixel(x as u32, y as u32, 0, 0, 0, 0);
            }
        }
    }
}

fn texture_from_png(bytes: &[u8]) -> Option<gdk::Texture> {
    if bytes.is_empty() {
        return None;
    }
    gdk::Texture::from_bytes(&glib::Bytes::from(bytes)).ok()
}

fn color_chip_texture(accent: &str, initials: &str) -> Option<gdk::Texture> {
    let hex = sanitize_accent_color(accent);
    let (red, green, blue) = hex_rgb(&hex)?;
    let size = COLOR_TEXTURE_SIZE;
    let mut surface = cairo::ImageSurface::create(cairo::Format::ARgb32, size, size).ok()?;
    {
        let cr = cairo::Context::new(&surface).ok()?;
        let radius = f64::from(size) / 2.0;
        cr.arc(radius, radius, radius, 0.0, TAU);
        cr.set_source_rgb(red, green, blue);
        cr.fill().ok()?;

        let label = initials.trim();
        if !label.is_empty() {
            let (fr, fg, fb) = contrast_fg(red, green, blue);
            cr.set_source_rgb(fr, fg, fb);
            cr.select_font_face("Sans", cairo::FontSlant::Normal, cairo::FontWeight::Bold);
            let font_size = if label.chars().count() > 1 {
                f64::from(size) * 0.36
            } else {
                f64::from(size) * 0.46
            };
            cr.set_font_size(font_size);
            let extents = cr.text_extents(label).ok()?;
            let x = (f64::from(size) - extents.width()) / 2.0 - extents.x_bearing();
            let y = (f64::from(size) - extents.height()) / 2.0 - extents.y_bearing();
            cr.move_to(x, y);
            cr.show_text(label).ok()?;
        }
    }
    surface.flush();
    let stride = surface.stride() as usize;
    let width = surface.width();
    let height = surface.height();
    let data = surface.data().ok()?;
    let bytes = glib::Bytes::from(&data[..]);
    drop(data);
    Some(
        gdk::MemoryTexture::new(
            width,
            height,
            gdk::MemoryFormat::B8g8r8a8Premultiplied,
            &bytes,
            stride,
        )
        .upcast(),
    )
}

fn hex_rgb(hex: &str) -> Option<(f64, f64, f64)> {
    let digits = hex.strip_prefix('#')?;
    if digits.len() != 6 {
        return None;
    }
    let value = u32::from_str_radix(digits, 16).ok()?;
    Some((
        f64::from((value >> 16) & 0xff) / 255.0,
        f64::from((value >> 8) & 0xff) / 255.0,
        f64::from(value & 0xff) / 255.0,
    ))
}

fn contrast_fg(red: f64, green: f64, blue: f64) -> (f64, f64, f64) {
    let luminance = 0.2126 * red + 0.7152 * green + 0.0722 * blue;
    if luminance > 0.55 {
        (0.12, 0.12, 0.14)
    } else {
        (1.0, 1.0, 1.0)
    }
}
