//! Tracker stripping for incoming mail HTML.

use std::collections::HashMap;

/// Remove 1×1 tracking pixels and images hosted on known tracker domains.
pub fn strip_trackers(html: &str) -> String {
    remove_img_tags(html, is_tracker_img)
}

/// Returns `true` when an `<img>` should be removed as a tracker pixel.
pub(crate) fn is_tracker_img(attrs: &HashMap<String, String>) -> bool {
    if is_tracking_dimensions(attrs) {
        return true;
    }
    if is_remote_img(attrs)
        && attrs
            .get("style")
            .is_some_and(|style| style_hides_image(style))
    {
        return true;
    }

    attrs
        .get("src")
        .is_some_and(|src| src_matches_tracker_host(src))
}

fn is_tracking_dimensions(attrs: &HashMap<String, String>) -> bool {
    let width = attrs.get("width").map(|w| parse_dimension(w));
    let height = attrs.get("height").map(|h| parse_dimension(h));

    match (width, height) {
        (Some(Some(w)), Some(Some(h))) => w <= 1 && h <= 1,
        (Some(Some(w)), None) => w <= 1,
        (None, Some(Some(h))) => h <= 1,
        _ => false,
    }
}

/// True when inline CSS makes an image invisible or at most 1×1 px — the
/// usual way to hide a tracking pixel from attribute-based detection.
fn style_hides_image(style: &str) -> bool {
    let mut width = None;
    let mut height = None;
    for declaration in style.split(';') {
        let Some((name, value)) = declaration.split_once(':') else {
            continue;
        };
        let name = name.trim().to_ascii_lowercase();
        let value = value
            .trim()
            .trim_end_matches("!important")
            .trim()
            .to_ascii_lowercase();
        match name.as_str() {
            "display" if value == "none" => return true,
            "visibility" if value == "hidden" || value == "collapse" => return true,
            "opacity" if value.parse::<f32>().is_ok_and(|o| o <= 0.01) => return true,
            "width" | "max-width" => width = css_pixels(&value).or(width),
            "height" | "max-height" => height = css_pixels(&value).or(height),
            _ => {}
        }
    }
    match (width, height) {
        (Some(w), Some(h)) => w <= 1.0 && h <= 1.0,
        (Some(d), None) | (None, Some(d)) => d <= 1.0,
        (None, None) => false,
    }
}

fn css_pixels(value: &str) -> Option<f32> {
    let numeric = value.strip_suffix("px").unwrap_or(value).trim();
    numeric.parse::<f32>().ok().filter(|n| n.is_finite())
}

fn parse_dimension(value: &str) -> Option<u32> {
    let trimmed = value.trim();
    let numeric = trimmed.trim_end_matches("px").trim_end_matches('%').trim();
    numeric.parse().ok()
}

const TRACKER_MARKERS: &[&str] = &[
    "pixel.",
    "trk.",
    "doubleclick.",
    "google-analytics",
    "facebook.com/tr",
    "list-manage.com",
    "mailchimp",
    "sendgrid.net",
    "sparkpost",
    "mandrill",
];

fn src_matches_tracker_host(src: &str) -> bool {
    let lower = src.to_ascii_lowercase();
    TRACKER_MARKERS.iter().any(|marker| lower.contains(marker))
}

/// Returns `true` when an `<img>` points at a remote `http`/`https` URL.
pub(crate) fn is_remote_img(attrs: &HashMap<String, String>) -> bool {
    attrs.get("src").is_some_and(|src| is_remote_url(src))
}

pub(crate) fn is_remote_url(src: &str) -> bool {
    let lower = src.trim().to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

/// Returns `true` when any `<img>` matches `should_match`.
pub(crate) fn contains_matching_img(
    html: &str,
    should_match: impl Fn(&HashMap<String, String>) -> bool,
) -> bool {
    let lower = html.to_ascii_lowercase();
    let mut cursor = 0;

    while let Some(rel_start) = lower[cursor..].find("<img") {
        let start = cursor + rel_start;
        let Some(tag_end) = find_tag_end(&html[start..]) else {
            return false;
        };

        let full_tag = &html[start..start + tag_end];
        let attrs = parse_tag_attributes(full_tag);
        if should_match(&attrs) {
            return true;
        }
        cursor = start + tag_end;
    }

    false
}

/// Remove `<img>` tags for which `should_remove` returns `true`.
pub(crate) fn remove_img_tags(
    html: &str,
    should_remove: impl Fn(&HashMap<String, String>) -> bool,
) -> String {
    let mut out = String::with_capacity(html.len());
    let mut cursor = 0;
    let lower = html.to_ascii_lowercase();

    while let Some(rel_start) = lower[cursor..].find("<img") {
        let start = cursor + rel_start;
        out.push_str(&html[cursor..start]);

        let Some(tag_end) = find_tag_end(&html[start..]) else {
            out.push_str(&html[start..]);
            return out;
        };

        let full_tag = &html[start..start + tag_end];
        let attrs = parse_tag_attributes(full_tag);

        if !should_remove(&attrs) {
            out.push_str(full_tag);
        }

        cursor = start + tag_end;
    }

    out.push_str(&html[cursor..]);
    out
}

pub(crate) fn find_tag_end(tag: &str) -> Option<usize> {
    let mut in_single = false;
    let mut in_double = false;

    for (idx, ch) in tag.char_indices().skip(1) {
        match ch {
            '"' if !in_single => in_double = !in_double,
            '\'' if !in_double => in_single = !in_single,
            '>' if !in_single && !in_double => return Some(idx + 1),
            _ => {}
        }
    }

    None
}

pub(crate) fn parse_tag_attributes(tag: &str) -> HashMap<String, String> {
    let mut attrs = HashMap::new();
    let bytes = tag.as_bytes();
    let mut idx = tag.find('<').map(|i| i + 1).unwrap_or(0);

    while idx < bytes.len() {
        while idx < bytes.len() && bytes[idx].is_ascii_whitespace() {
            idx += 1;
        }

        if idx >= bytes.len() || bytes[idx] == b'>' || bytes[idx] == b'/' {
            break;
        }

        let name_start = idx;
        while idx < bytes.len() && !bytes[idx].is_ascii_whitespace() && bytes[idx] != b'=' {
            idx += 1;
        }
        let name = tag[name_start..idx].to_ascii_lowercase();

        while idx < bytes.len() && bytes[idx].is_ascii_whitespace() {
            idx += 1;
        }

        if idx >= bytes.len() || bytes[idx] != b'=' {
            if !name.is_empty() && name != "img" {
                attrs.insert(name, String::new());
            }
            continue;
        }

        idx += 1;
        while idx < bytes.len() && bytes[idx].is_ascii_whitespace() {
            idx += 1;
        }

        let value = if idx < bytes.len() && bytes[idx] == b'"' {
            idx += 1;
            let value_start = idx;
            while idx < bytes.len() && bytes[idx] != b'"' {
                idx += 1;
            }
            let value = tag[value_start..idx].to_string();
            if idx < bytes.len() {
                idx += 1;
            }
            value
        } else if idx < bytes.len() && bytes[idx] == b'\'' {
            idx += 1;
            let value_start = idx;
            while idx < bytes.len() && bytes[idx] != b'\'' {
                idx += 1;
            }
            let value = tag[value_start..idx].to_string();
            if idx < bytes.len() {
                idx += 1;
            }
            value
        } else {
            let value_start = idx;
            while idx < bytes.len()
                && !bytes[idx].is_ascii_whitespace()
                && bytes[idx] != b'>'
                && bytes[idx] != b'/'
            {
                idx += 1;
            }
            tag[value_start..idx].to_string()
        };

        if !name.is_empty() && name != "img" {
            attrs.insert(name, value);
        }
    }

    attrs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_one_by_one_dimensions() {
        let mut attrs = HashMap::new();
        attrs.insert(
            "src".to_string(),
            "https://example.com/pixel.gif".to_string(),
        );
        attrs.insert("width".to_string(), "1".to_string());
        attrs.insert("height".to_string(), "1".to_string());
        assert!(is_tracker_img(&attrs));
    }

    #[test]
    fn detects_style_hidden_or_tiny_remote_pixels() {
        for html in [
            r#"<p>a</p><img src="https://t.example/o.gif" style="width:1px;height:1px">"#,
            r#"<img src="https://t.example/o.gif" style="display: none">"#,
            r#"<img src="https://t.example/o.gif" style="visibility:hidden !important">"#,
            r#"<img src="https://t.example/o.gif" style="opacity:0">"#,
            r#"<img src="https://t.example/o.gif" style="max-height:0;max-width:0">"#,
            r#"<img src="https://t.example/o.gif" style="width:0.5px">"#,
        ] {
            assert!(!strip_trackers(html).contains("<img"), "{html}");
        }
        let normal = r#"<img src="https://cdn.example/hero.png" style="width:600px;height:auto">"#;
        assert!(strip_trackers(normal).contains("<img"));
        let local = r#"<img src="cid:logo" style="display:none">"#;
        assert!(strip_trackers(local).contains("<img"));
    }

    #[test]
    fn detects_tracker_host_marker() {
        let mut attrs = HashMap::new();
        attrs.insert(
            "src".to_string(),
            "https://pixel.example.com/track".to_string(),
        );
        assert!(is_tracker_img(&attrs));
    }
}
