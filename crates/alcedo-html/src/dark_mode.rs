//! Adapt sanitized HTML fragments so inline colors stay readable on a dark canvas.
//!
//! Mail authors hard-code `color: #333` / `<font color="black">` for light paper.
//! The reader paints a dark WebView, so those colors must be remapped — but not by
//! forcing every run to white (that blows out yellow callouts, colored buttons,
//! and other islands that already specify a non-paper background).

const MIN_CONTRAST: f32 = 4.5;
const PAPER_WHITE_MIN: u8 = 0xF0;
const DARK_TEXT_ON_LIGHT: Rgb = Rgb {
    r: 0x11,
    g: 0x11,
    b: 0x11,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Rgb {
    r: u8,
    g: u8,
    b: u8,
}

#[derive(Clone, Copy, Debug)]
struct Palette {
    canvas: Rgb,
    text: Rgb,
    link: Rgb,
}

#[derive(Clone, Debug)]
struct Frame {
    bg: Rgb,
    fg: Rgb,
    tag: String,
    in_link: bool,
}

/// Rewrite inline `color` / `background-color` / `bgcolor` / `<font color>` so
/// text contrasts with `canvas` in dark mode.
///
/// Near-white backgrounds are treated as page paper and dropped (the canvas
/// shows through). Tinted or dark backgrounds are kept as islands.
pub fn adapt_html_for_dark_mode(html: &str, canvas: &str, text: &str, link: &str) -> String {
    let Some(palette) = parse_palette(canvas, text, link) else {
        return html.to_string();
    };
    adapt_fragment(html, palette)
}

fn parse_palette(canvas: &str, text: &str, link: &str) -> Option<Palette> {
    Some(Palette {
        canvas: parse_css_color(canvas)?,
        text: parse_css_color(text)?,
        link: parse_css_color(link)?,
    })
}

fn adapt_fragment(html: &str, palette: Palette) -> String {
    let mut out = String::with_capacity(html.len() + 32);
    let mut rest = html;
    let mut stack: Vec<Frame> = Vec::new();
    let root = Frame {
        bg: palette.canvas,
        fg: palette.text,
        tag: String::new(),
        in_link: false,
    };

    while let Some(lt) = rest.find('<') {
        out.push_str(&rest[..lt]);
        let tag_src = &rest[lt..];
        let Some(end) = find_tag_end(tag_src) else {
            out.push_str(tag_src);
            return out;
        };
        let tag = &tag_src[..=end];
        rest = &tag_src[end + 1..];

        if tag.starts_with("<!--") || tag.starts_with("<!") {
            out.push_str(tag);
            continue;
        }

        let inner = tag[1..tag.len() - 1].trim();
        if inner.starts_with('/') {
            pop_until(&mut stack, close_tag_name(inner));
            out.push_str(tag);
            continue;
        }

        let parent = stack.last().cloned().unwrap_or_else(|| root.clone());
        let Some(parsed) = parse_start_tag(tag) else {
            out.push_str(tag);
            continue;
        };

        let (emitted, frame) = adapt_start_tag(tag, &parsed, &parent, &palette);
        out.push_str(&emitted);
        if !parsed.void {
            stack.push(frame);
        }
    }

    out.push_str(rest);
    out
}

fn close_tag_name(inner: &str) -> &str {
    inner
        .trim_start_matches('/')
        .split(|ch: char| ch.is_ascii_whitespace() || ch == '/')
        .next()
        .unwrap_or("")
}

fn pop_until(stack: &mut Vec<Frame>, name: &str) {
    if name.is_empty() {
        return;
    }
    if let Some(idx) = stack
        .iter()
        .rposition(|frame| frame.tag.eq_ignore_ascii_case(name))
    {
        stack.truncate(idx);
    }
}

fn find_tag_end(html: &str) -> Option<usize> {
    if html.starts_with("<!--") {
        return html.find("-->").map(|idx| idx + 2);
    }
    let bytes = html.as_bytes();
    let mut quote: Option<u8> = None;
    for (idx, &b) in bytes.iter().enumerate() {
        if let Some(q) = quote {
            if b == q {
                quote = None;
            }
            continue;
        }
        match b {
            b'"' | b'\'' => quote = Some(b),
            b'>' => return Some(idx),
            _ => {}
        }
    }
    None
}

struct ParsedTag {
    name: String,
    attrs: Vec<(String, String)>,
    void: bool,
}

fn parse_start_tag(tag: &str) -> Option<ParsedTag> {
    let inner = tag.strip_prefix('<')?.strip_suffix('>')?.trim();
    if inner.is_empty() || inner.starts_with('/') || inner.starts_with('!') {
        return None;
    }
    let self_closing = inner.ends_with('/');
    let inner = inner.trim_end_matches('/').trim();
    let mut rest = inner;
    let name_end = rest
        .find(|ch: char| ch.is_ascii_whitespace() || ch == '/')
        .unwrap_or(rest.len());
    let name = rest[..name_end].to_ascii_lowercase();
    rest = rest[name_end..].trim_start();
    let mut attrs = Vec::new();
    while !rest.is_empty() {
        let (attr, next) = take_attr(rest);
        if attr.is_empty() {
            break;
        }
        if let Some((key, value)) = attr.split_once('=') {
            attrs.push((key.trim().to_ascii_lowercase(), unquote_attr(value.trim())));
        } else {
            attrs.push((attr.to_ascii_lowercase(), String::new()));
        }
        rest = next;
    }
    Some(ParsedTag {
        void: self_closing || is_void(&name),
        name,
        attrs,
    })
}

fn take_attr(input: &str) -> (&str, &str) {
    let s = input.trim_start();
    if s.is_empty() {
        return ("", s);
    }
    let bytes = s.as_bytes();
    let mut idx = 0usize;
    while idx < bytes.len() && bytes[idx] != b'=' && !bytes[idx].is_ascii_whitespace() {
        idx += 1;
    }
    if idx < bytes.len() && bytes[idx] == b'=' {
        idx += 1;
        if idx < bytes.len() && (bytes[idx] == b'"' || bytes[idx] == b'\'') {
            let quote = bytes[idx];
            idx += 1;
            while idx < bytes.len() && bytes[idx] != quote {
                idx += 1;
            }
            if idx < bytes.len() {
                idx += 1;
            }
        } else {
            while idx < bytes.len() && !bytes[idx].is_ascii_whitespace() {
                idx += 1;
            }
        }
    }
    (&s[..idx], s[idx..].trim_start())
}

fn unquote_attr(value: &str) -> String {
    let value = value.trim();
    if value.len() >= 2 {
        let bytes = value.as_bytes();
        if (bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[bytes.len() - 1] == b'\'')
        {
            return decode_attr(&value[1..value.len() - 1]);
        }
    }
    decode_attr(value)
}

fn decode_attr(value: &str) -> String {
    value
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

fn encode_attr(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(ch),
        }
    }
    out
}

fn is_void(name: &str) -> bool {
    matches!(
        name,
        "area"
            | "base"
            | "br"
            | "col"
            | "embed"
            | "hr"
            | "img"
            | "input"
            | "link"
            | "meta"
            | "source"
            | "track"
            | "wbr"
    )
}

fn adapt_start_tag(
    raw: &str,
    tag: &ParsedTag,
    parent: &Frame,
    palette: &Palette,
) -> (String, Frame) {
    let mut style = style_props(attr_value(&tag.attrs, "style"));
    let font_color = attr_value(&tag.attrs, "color").filter(|_| tag.name == "font");
    let own_fg = style_color(&style).or_else(|| font_color.and_then(parse_css_color));
    let own_bg = style_background(&style)
        .or_else(|| attr_value(&tag.attrs, "bgcolor").and_then(parse_css_color));

    let mut strip_paper = false;
    let effective_bg = match own_bg {
        Some(bg) if is_paper_white(bg) => {
            strip_paper = true;
            parent.bg
        }
        Some(bg) => bg,
        None => parent.bg,
    };

    let in_link = tag.name == "a" || parent.in_link;
    let proposed_fg = own_fg.unwrap_or(parent.fg);
    let adapted_fg = ensure_contrast(proposed_fg, effective_bg, in_link, palette);

    let mut write_color = false;
    if let Some(own) = own_fg {
        write_color = adapted_fg != own;
    } else if adapted_fg != parent.fg {
        write_color = true;
    }

    if write_color {
        upsert_style(&mut style, "color", &hex_rgb(adapted_fg));
    }
    if strip_paper {
        remove_style(&mut style, "background-color");
        remove_style(&mut style, "background");
    }

    let emitted = if write_color || strip_paper {
        emit_tag(tag, &style, strip_paper, write_color, adapted_fg)
    } else {
        raw.to_string()
    };
    let frame = Frame {
        bg: effective_bg,
        fg: adapted_fg,
        tag: tag.name.clone(),
        in_link,
    };
    (emitted, frame)
}

fn attr_value<'a>(attrs: &'a [(String, String)], name: &str) -> Option<&'a str> {
    attrs
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.as_str())
        .filter(|value| !value.is_empty())
}

fn style_props(style: Option<&str>) -> Vec<(String, String)> {
    let Some(style) = style else {
        return Vec::new();
    };
    style
        .split(';')
        .filter_map(|part| {
            let part = part.trim();
            if part.is_empty() {
                return None;
            }
            let (key, value) = part.split_once(':')?;
            let key = key.trim();
            let value = value.trim();
            if key.is_empty() || value.is_empty() {
                return None;
            }
            Some((key.to_string(), value.to_string()))
        })
        .collect()
}

fn style_color(style: &[(String, String)]) -> Option<Rgb> {
    style
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("color"))
        .and_then(|(_, value)| parse_css_color(value))
}

fn style_background(style: &[(String, String)]) -> Option<Rgb> {
    if let Some(value) = style
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("background-color"))
        .map(|(_, value)| value.as_str())
    {
        return parse_css_color(value);
    }
    style
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("background"))
        .and_then(|(_, value)| color_from_background_shorthand(value))
}

fn color_from_background_shorthand(value: &str) -> Option<Rgb> {
    value.split_whitespace().find_map(parse_css_color)
}

fn upsert_style(style: &mut Vec<(String, String)>, key: &str, value: &str) {
    if let Some(entry) = style
        .iter_mut()
        .find(|(existing, _)| existing.eq_ignore_ascii_case(key))
    {
        entry.1 = value.to_string();
        return;
    }
    style.push((key.to_string(), value.to_string()));
}

fn remove_style(style: &mut Vec<(String, String)>, key: &str) {
    style.retain(|(existing, _)| !existing.eq_ignore_ascii_case(key));
}

fn serialize_style(style: &[(String, String)]) -> String {
    let mut out = String::new();
    for (idx, (key, value)) in style.iter().enumerate() {
        if idx > 0 {
            out.push_str("; ");
        }
        out.push_str(key);
        out.push_str(": ");
        out.push_str(value);
    }
    out
}

fn emit_tag(
    tag: &ParsedTag,
    style: &[(String, String)],
    strip_paper: bool,
    write_color: bool,
    adapted_fg: Rgb,
) -> String {
    let mut out = String::from("<");
    out.push_str(&tag.name);
    let mut saw_style = false;
    for (key, value) in &tag.attrs {
        if key == "style" {
            saw_style = true;
            if style.is_empty() {
                continue;
            }
            out.push(' ');
            out.push_str(key);
            out.push_str("=\"");
            out.push_str(&encode_attr(&serialize_style(style)));
            out.push('"');
            continue;
        }
        if strip_paper && key == "bgcolor" {
            continue;
        }
        if key == "color" && tag.name == "font" && write_color {
            out.push(' ');
            out.push_str(key);
            out.push_str("=\"");
            out.push_str(&hex_rgb(adapted_fg));
            out.push('"');
            continue;
        }
        out.push(' ');
        out.push_str(key);
        if !value.is_empty() {
            out.push_str("=\"");
            out.push_str(&encode_attr(value));
            out.push('"');
        }
    }
    if !saw_style && !style.is_empty() {
        out.push_str(" style=\"");
        out.push_str(&encode_attr(&serialize_style(style)));
        out.push('"');
    }
    if tag.void && matches!(tag.name.as_str(), "br" | "img" | "hr") {
        out.push_str(" />");
    } else {
        out.push('>');
    }
    out
}

fn is_paper_white(color: Rgb) -> bool {
    color.r >= PAPER_WHITE_MIN && color.g >= PAPER_WHITE_MIN && color.b >= PAPER_WHITE_MIN
}

fn ensure_contrast(fg: Rgb, bg: Rgb, in_link: bool, palette: &Palette) -> Rgb {
    if contrast_ratio(fg, bg) >= MIN_CONTRAST {
        return fg;
    }
    let bg_dark = relative_luminance(bg) < 0.5;
    if bg_dark {
        if in_link && (is_near_gray(fg) || is_link_blue(fg)) {
            return palette.link;
        }
        if is_near_gray(fg) {
            return palette.text;
        }
        let mut hsl = rgb_to_hsl(fg);
        hsl.2 = (1.0 - hsl.2).max(0.78);
        let lifted = hsl_to_rgb(hsl);
        if contrast_ratio(lifted, bg) >= MIN_CONTRAST {
            return lifted;
        }
        if in_link { palette.link } else { palette.text }
    } else {
        if is_near_gray(fg) {
            return DARK_TEXT_ON_LIGHT;
        }
        let mut hsl = rgb_to_hsl(fg);
        hsl.2 = (1.0 - hsl.2).min(0.25);
        let darkened = hsl_to_rgb(hsl);
        if contrast_ratio(darkened, bg) >= MIN_CONTRAST {
            return darkened;
        }
        DARK_TEXT_ON_LIGHT
    }
}

fn is_near_gray(color: Rgb) -> bool {
    let max = color.r.max(color.g).max(color.b);
    let min = color.r.min(color.g).min(color.b);
    max.saturating_sub(min) < 30
}

fn is_link_blue(color: Rgb) -> bool {
    let (h, s, _) = rgb_to_hsl(color);
    s > 0.35 && (0.52..0.78).contains(&h)
}

fn hex_rgb(color: Rgb) -> String {
    format!("#{:02x}{:02x}{:02x}", color.r, color.g, color.b)
}

fn relative_luminance(color: Rgb) -> f32 {
    let lin = |channel: u8| {
        let c = f32::from(channel) / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * lin(color.r) + 0.7152 * lin(color.g) + 0.0722 * lin(color.b)
}

fn contrast_ratio(a: Rgb, b: Rgb) -> f32 {
    let (l1, l2) = {
        let la = relative_luminance(a);
        let lb = relative_luminance(b);
        if la > lb { (la, lb) } else { (lb, la) }
    };
    (l1 + 0.05) / (l2 + 0.05)
}

fn rgb_to_hsl(color: Rgb) -> (f32, f32, f32) {
    let r = f32::from(color.r) / 255.0;
    let g = f32::from(color.g) / 255.0;
    let b = f32::from(color.b) / 255.0;
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    if (max - min).abs() < f32::EPSILON {
        return (0.0, 0.0, l);
    }
    let d = max - min;
    let s = if l > 0.5 {
        d / (2.0 - max - min)
    } else {
        d / (max + min)
    };
    let h = if (max - r).abs() < f32::EPSILON {
        let mut v = (g - b) / d;
        if g < b {
            v += 6.0;
        }
        v
    } else if (max - g).abs() < f32::EPSILON {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    } / 6.0;
    (h, s, l)
}

fn hsl_to_rgb((h, s, l): (f32, f32, f32)) -> Rgb {
    if s.abs() < f32::EPSILON {
        let v = (l * 255.0).round().clamp(0.0, 255.0) as u8;
        return Rgb { r: v, g: v, b: v };
    }
    let hue_to_rgb = |p: f32, q: f32, mut t: f32| {
        if t < 0.0 {
            t += 1.0;
        }
        if t > 1.0 {
            t -= 1.0;
        }
        if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 1.0 / 2.0 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        }
    };
    let q = if l < 0.5 {
        l * (1.0 + s)
    } else {
        l + s - l * s
    };
    let p = 2.0 * l - q;
    let r = hue_to_rgb(p, q, h + 1.0 / 3.0);
    let g = hue_to_rgb(p, q, h);
    let b = hue_to_rgb(p, q, h - 1.0 / 3.0);
    Rgb {
        r: (r * 255.0).round().clamp(0.0, 255.0) as u8,
        g: (g * 255.0).round().clamp(0.0, 255.0) as u8,
        b: (b * 255.0).round().clamp(0.0, 255.0) as u8,
    }
}

fn parse_css_color(value: &str) -> Option<Rgb> {
    let value = value.trim().trim_end_matches("!important").trim();
    let lower = value.to_ascii_lowercase();
    if lower == "transparent" || lower == "inherit" || lower == "currentcolor" {
        return None;
    }
    if let Some(rgb) = parse_named_color(&lower) {
        return Some(rgb);
    }
    if let Some(hex) = lower.strip_prefix('#') {
        return parse_hex_color(hex);
    }
    if let Some(args) = lower
        .strip_prefix("rgb(")
        .or_else(|| lower.strip_prefix("rgba("))
        .and_then(|rest| rest.strip_suffix(')'))
    {
        return parse_rgb_args(args);
    }
    None
}

fn parse_hex_color(hex: &str) -> Option<Rgb> {
    match hex.len() {
        3 => {
            let r = u8::from_str_radix(&hex[0..1].repeat(2), 16).ok()?;
            let g = u8::from_str_radix(&hex[1..2].repeat(2), 16).ok()?;
            let b = u8::from_str_radix(&hex[2..3].repeat(2), 16).ok()?;
            Some(Rgb { r, g, b })
        }
        6 | 8 => {
            let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
            let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
            let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
            Some(Rgb { r, g, b })
        }
        _ => None,
    }
}

fn parse_rgb_args(args: &str) -> Option<Rgb> {
    let mut parts = args
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty());
    let r = parse_rgb_channel(parts.next()?)?;
    let g = parse_rgb_channel(parts.next()?)?;
    let b = parse_rgb_channel(parts.next()?)?;
    if let Some(alpha) = parts.next() {
        let alpha = alpha
            .strip_suffix('%')
            .unwrap_or(alpha)
            .parse::<f32>()
            .ok()?;
        if alpha <= 0.0 {
            return None;
        }
    }
    Some(Rgb { r, g, b })
}

fn parse_rgb_channel(value: &str) -> Option<u8> {
    if let Some(pct) = value.strip_suffix('%') {
        let pct = pct.parse::<f32>().ok()?;
        return Some((pct / 100.0 * 255.0).round().clamp(0.0, 255.0) as u8);
    }
    value
        .parse::<f32>()
        .ok()
        .map(|n| n.round().clamp(0.0, 255.0) as u8)
}

fn parse_named_color(name: &str) -> Option<Rgb> {
    let rgb = match name {
        "black" | "windowtext" | "text" => [0x00, 0x00, 0x00],
        "white" => [0xff, 0xff, 0xff],
        "silver" => [0xc0, 0xc0, 0xc0],
        "gray" | "grey" => [0x80, 0x80, 0x80],
        "darkgray" | "darkgrey" => [0xa9, 0xa9, 0xa9],
        "lightgray" | "lightgrey" => [0xd3, 0xd3, 0xd3],
        "maroon" => [0x80, 0x00, 0x00],
        "red" => [0xff, 0x00, 0x00],
        "purple" => [0x80, 0x00, 0x80],
        "fuchsia" | "magenta" => [0xff, 0x00, 0xff],
        "green" => [0x00, 0x80, 0x00],
        "lime" => [0x00, 0xff, 0x00],
        "olive" => [0x80, 0x80, 0x00],
        "yellow" => [0xff, 0xff, 0x00],
        "navy" => [0x00, 0x00, 0x80],
        "blue" => [0x00, 0x00, 0xff],
        "teal" => [0x00, 0x80, 0x80],
        "aqua" | "cyan" => [0x00, 0xff, 0xff],
        "orange" => [0xff, 0xa5, 0x00],
        "navyblue" => [0x00, 0x00, 0x80],
        _ => return None,
    };
    Some(Rgb {
        r: rgb[0],
        g: rgb[1],
        b: rgb[2],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const CANVAS: &str = "#242424";
    const TEXT: &str = "#eeeeee";
    const LINK: &str = "#62a0ea";

    fn adapt(html: &str) -> String {
        adapt_html_for_dark_mode(html, CANVAS, TEXT, LINK)
    }

    #[test]
    fn dark_inline_text_on_canvas_becomes_light() {
        let out = adapt(r#"<p style="color:#333333">Hi Christiane</p>"#);
        assert!(out.contains("color: #eeeeee"), "{out}");
        assert!(!out.contains("#333333"), "{out}");
    }

    #[test]
    fn font_color_black_becomes_light() {
        let out = adapt(r##"<font color="#000000">Had a busy day</font>"##);
        assert!(
            out.contains(r##"color="#eeeeee""##) || out.contains("color: #eeeeee"),
            "{out}"
        );
        assert!(!out.to_ascii_lowercase().contains("#000000"), "{out}");
    }

    #[test]
    fn rgb_black_body_text_is_remapped() {
        let out = adapt(r#"<div style="color: rgb(0, 0, 0)">You sent $120.00</div>"#);
        assert!(out.contains("color: #eeeeee"), "{out}");
    }

    #[test]
    fn paper_white_background_is_dropped_and_text_lightened() {
        let out = adapt(r#"<div style="background-color:#ffffff; color:#000000">signature</div>"#);
        let lower = out.to_ascii_lowercase();
        assert!(!lower.contains("background-color"), "{out}");
        assert!(out.contains("color: #eeeeee"), "{out}");
    }

    #[test]
    fn paper_white_bgcolor_is_dropped() {
        let out = adapt(r##"<td bgcolor="#FFFFFF" style="color:#111111">cell</td>"##);
        let lower = out.to_ascii_lowercase();
        assert!(!lower.contains("bgcolor"), "{out}");
        assert!(out.contains("color: #eeeeee"), "{out}");
    }

    #[test]
    fn tinted_callout_keeps_background_and_dark_text() {
        let out = adapt(
            r#"<div style="background-color:#fff3cd; color:#664d03">Remote images blocked</div>"#,
        );
        assert!(out.contains("#fff3cd"), "{out}");
        assert!(
            out.contains("#664d03") || out.contains("color: #664d03"),
            "{out}"
        );
    }

    #[test]
    fn dark_blue_banner_keeps_light_text() {
        let out = adapt(
            r#"<div style="background-color:#1c71d8; color:#ffffff">Verification code</div>"#,
        );
        assert!(out.contains("#1c71d8"), "{out}");
        assert!(out.contains("#ffffff"), "{out}");
    }

    #[test]
    fn light_island_injects_dark_text_when_parent_was_lightened() {
        let out = adapt(
            r#"<div style="color:#333333">body<div style="background-color:#fff8e7">island</div></div>"#,
        );
        assert!(out.contains("color: #eeeeee"), "{out}");
        assert!(out.contains("#fff8e7"), "{out}");
        assert!(
            out.contains("color: #111111") || out.contains("color: #664d03"),
            "expected contrasting color on cream island: {out}"
        );
    }

    #[test]
    fn dark_link_on_canvas_uses_link_color() {
        let out = adapt(r#"<a href="https://example.com" style="color:#0000ee">Pay</a>"#);
        assert!(out.contains("color: #62a0ea"), "{out}");
        assert!(!out.contains("#0000ee"), "{out}");
    }

    #[test]
    fn unstyled_markup_is_unchanged() {
        let html = "<p>Hello <b>World</b><br></p>";
        assert_eq!(adapt(html), html);
    }

    #[test]
    fn named_black_is_remapped() {
        let out = adapt(r#"<span style="color:black">navy heading</span>"#);
        assert!(out.contains("color: #eeeeee"), "{out}");
    }

    #[test]
    fn parse_hex_and_rgb() {
        assert_eq!(
            parse_css_color("#333"),
            Some(Rgb {
                r: 0x33,
                g: 0x33,
                b: 0x33
            })
        );
        assert_eq!(
            parse_css_color("rgb(51,51,51)"),
            Some(Rgb {
                r: 51,
                g: 51,
                b: 51
            })
        );
        assert_eq!(parse_css_color("Black"), Some(Rgb { r: 0, g: 0, b: 0 }));
    }
}
