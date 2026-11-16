//! Ammonia-based HTML sanitizer for mail bodies.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use alcedo_core::{contains_html_tag, html_tag_prefix};
use ammonia::Builder;

/// Bump when the inbound allowlist, tracker stripping, or dark-mode inputs change.
/// The reader caches wrapped HTML keyed on this value.
pub const SANITIZER_VERSION: u32 = 1;

static MAIL_BUILDER_BLOCK_REMOTE: LazyLock<Builder<'static>> =
    LazyLock::new(|| build_mail_builder(false));
static MAIL_BUILDER_ALLOW_REMOTE: LazyLock<Builder<'static>> =
    LazyLock::new(|| build_mail_builder(true));

use crate::trackers::{contains_matching_img, is_remote_img, remove_img_tags, strip_trackers};

/// Options controlling how incoming HTML is sanitized for display.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Default)]
pub struct SanitizeOptions {
    /// When `false`, `<img>` tags with `http`/`https` sources are removed.
    pub allow_remote_images: bool,
}

/// Sanitize untrusted mail HTML for safe display.
pub fn sanitize_html(input: &str, opts: SanitizeOptions) -> String {
    let without_trackers = strip_trackers(input);
    let without_remote = if opts.allow_remote_images {
        without_trackers
    } else {
        remove_img_tags(&without_trackers, is_remote_img)
    };

    mail_builder(opts.allow_remote_images)
        .clean(&without_remote)
        .to_string()
}

fn mail_builder(allow_remote_images: bool) -> &'static Builder<'static> {
    if allow_remote_images {
        &MAIL_BUILDER_ALLOW_REMOTE
    } else {
        &MAIL_BUILDER_BLOCK_REMOTE
    }
}

fn br_tag_len(html: &str) -> Option<usize> {
    let bytes = html.as_bytes();
    if bytes.len() < 3 || bytes[0] != b'<' {
        return None;
    }
    if !html.get(1..3).is_some_and(|s| s.eq_ignore_ascii_case("br")) {
        return None;
    }
    let after = html.get(3..)?;
    let next = after.chars().next()?;
    if next != '>' && next != '/' && !next.is_whitespace() {
        return None;
    }
    html.find('>').map(|idx| idx + 1)
}

/// True when HTML has remote `http`/`https` images that stay hidden until allowed.
///
/// Tracker pixels are ignored because they are stripped even when remote images
/// are loaded.
pub fn html_has_remote_images(input: &str) -> bool {
    let without_trackers = strip_trackers(input);
    contains_matching_img(&without_trackers, is_remote_img)
}

/// Returns true when `input` appears to contain HTML markup.
///
/// Recruiter and newsletter mail often puts an HTML fragment in a `text/plain`
/// part that starts with prose (`Hi&nbsp;Carter,<br>…`) rather than a tag.
pub fn looks_like_html(input: &str) -> bool {
    let trimmed = strip_bom(input).trim_start();
    if trimmed.is_empty() {
        return false;
    }
    let lower = trimmed.to_ascii_lowercase();
    lower.starts_with("<!doctype")
        || lower.starts_with("<!--")
        || lower.starts_with("<?xml")
        || html_tag_prefix(trimmed)
        || contains_html_tag(trimmed)
}

/// True when `html` is mail-parser's `text_to_html` conversion of a text part:
/// an `<html><body>` wrapper whose only tags are `<br/>` (escaped `&lt;` in
/// text does not count). Alcedo outbound HTML uses the same wrapper, so a bare
/// `&lt;` (quoted `Name &lt;email&gt;`) must not mark real markup as generated.
pub fn looks_like_generated_plaintext_html(html: &str) -> bool {
    let trimmed = html.trim();
    let Some(inner) = trimmed
        .strip_prefix("<html><body>")
        .and_then(|value| value.strip_suffix("</body></html>"))
    else {
        return false;
    };
    !inner_has_html_structure_besides_br(inner)
}

/// True when `html` looks like plaintext that was wrapped/escaped into HTML
/// (mail-parser `text_to_html`: `<html><body>…&lt;br&gt;…</body></html>`).
pub fn looks_like_escaped_html(html: &str) -> bool {
    looks_like_generated_plaintext_html(html) && html.contains("&lt;")
}

fn inner_has_html_structure_besides_br(inner: &str) -> bool {
    let mut rest = inner;
    while let Some(idx) = rest.find('<') {
        if let Some(len) = br_tag_len(&rest[idx..]) {
            rest = &rest[idx + len..];
            continue;
        }
        if html_tag_prefix(&rest[idx..]) {
            return true;
        }
        rest = &rest[idx + 1..];
    }
    false
}

fn strip_bom(input: &str) -> &str {
    input.strip_prefix('\u{feff}').unwrap_or(input)
}

/// True when an inline `style` value could load a resource or run code, so
/// the whole attribute must be dropped. Comments and whitespace are removed
/// first, and any backslash is rejected because CSS escapes can spell
/// `url(` in ways a substring check would miss.
pub(crate) fn style_is_unsafe(value: &str) -> bool {
    if value.contains('\\') {
        return true;
    }
    let mut normalized = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(start) = rest.find("/*") {
        normalized.push_str(&rest[..start]);
        rest = match rest[start + 2..].find("*/") {
            Some(end) => &rest[start + 2 + end + 2..],
            None => "",
        };
    }
    normalized.push_str(rest);
    let compact: String = normalized
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_ascii_lowercase();
    const BLOCKED: &[&str] = &[
        "url(",
        "image(",
        "image-set(",
        "cross-fade(",
        "element(",
        "src(",
        "expression(",
        "@import",
        "-moz-binding",
        "behavior:",
        "javascript:",
    ];
    BLOCKED.iter().any(|needle| compact.contains(needle))
}

/// Visual CSS kept on mail HTML. A `style` for which [`style_is_unsafe`]
/// is true is dropped entirely by the attribute filter.
pub(crate) fn visual_style_properties() -> HashSet<&'static str> {
    [
        "color",
        "background-color",
        "background",
        "font",
        "font-size",
        "font-weight",
        "font-style",
        "font-family",
        "text-align",
        "text-decoration",
        "text-indent",
        "line-height",
        "letter-spacing",
        "width",
        "height",
        "max-width",
        "min-width",
        "margin",
        "margin-top",
        "margin-right",
        "margin-bottom",
        "margin-left",
        "padding",
        "padding-top",
        "padding-right",
        "padding-bottom",
        "padding-left",
        "border",
        "border-width",
        "border-style",
        "border-color",
        "border-top",
        "border-right",
        "border-bottom",
        "border-left",
        "border-collapse",
        "border-spacing",
        "border-radius",
        "display",
        "vertical-align",
        "white-space",
        "overflow-wrap",
        "word-break",
        "opacity",
    ]
    .into_iter()
    .collect()
}

fn build_mail_builder(allow_remote_images: bool) -> Builder<'static> {
    let tags: HashSet<&'static str> = [
        "a",
        "p",
        "br",
        "div",
        "span",
        "strong",
        "em",
        "b",
        "i",
        "u",
        "s",
        "ul",
        "ol",
        "li",
        "blockquote",
        "pre",
        "code",
        "h1",
        "h2",
        "h3",
        "h4",
        "h5",
        "h6",
        "table",
        "thead",
        "tbody",
        "tr",
        "td",
        "th",
        "img",
        "hr",
        "font",
        "center",
        "section",
        "article",
        "header",
        "footer",
        "main",
    ]
    .into_iter()
    .collect();

    let mut tag_attributes: HashMap<&'static str, HashSet<&'static str>> = HashMap::new();
    tag_attributes.insert("a", ["href", "title"].into_iter().collect());
    tag_attributes.insert(
        "img",
        ["src", "alt", "width", "height"].into_iter().collect(),
    );
    tag_attributes.insert("font", ["color", "face", "size"].into_iter().collect());

    // Prefer richer HTML attributes used by real-world mail.
    tag_attributes.insert(
        "td",
        ["colspan", "rowspan", "align", "valign", "bgcolor", "width"]
            .into_iter()
            .collect(),
    );
    tag_attributes.insert(
        "th",
        ["colspan", "rowspan", "align", "valign", "bgcolor", "width"]
            .into_iter()
            .collect(),
    );
    tag_attributes.insert(
        "table",
        ["border", "cellpadding", "cellspacing", "bgcolor", "width"]
            .into_iter()
            .collect(),
    );
    tag_attributes.insert("tr", ["align", "valign", "bgcolor"].into_iter().collect());

    let mut generic_attributes: HashSet<&'static str> = HashSet::new();
    generic_attributes.insert("style");
    generic_attributes.insert("class");
    generic_attributes.insert("align");
    generic_attributes.insert("dir");

    let style_properties = visual_style_properties();

    let url_schemes: HashSet<&'static str> = ["http", "https", "mailto", "cid", "alcedo-cid"]
        .into_iter()
        .collect();

    let clean_content_tags: HashSet<&'static str> = ["script", "style"].into_iter().collect();

    let mut builder = Builder::default();
    builder
        .tags(tags)
        .tag_attributes(tag_attributes)
        .generic_attributes(generic_attributes)
        .filter_style_properties(style_properties)
        .url_schemes(url_schemes)
        .clean_content_tags(clean_content_tags)
        .link_rel(Some("noopener noreferrer"));

    builder.attribute_filter(move |element, attribute, value| {
        if attribute == "style" && style_is_unsafe(value) {
            return None;
        }
        if !allow_remote_images
            && element == "img"
            && attribute == "src"
            && crate::trackers::is_remote_url(value)
        {
            return None;
        }
        Some(Cow::Borrowed(value))
    });

    builder
}

/// Extract a plaintext snippet from HTML, truncated to `max_chars` Unicode scalars.
pub fn html_to_snippet(html: &str, max_chars: usize) -> String {
    let text = html_to_plaintext(html);
    truncate_chars(&text, max_chars)
}

pub(crate) fn html_to_plaintext(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    let mut tag_name = String::new();
    let mut reading_name = false;

    for ch in html.chars() {
        match ch {
            '<' => {
                in_tag = true;
                reading_name = true;
                tag_name.clear();
            }
            '>' if in_tag => {
                if matches!(
                    tag_name.to_ascii_lowercase().as_str(),
                    "br" | "p" | "div" | "tr" | "li"
                ) {
                    out.push(' ');
                }
                in_tag = false;
                reading_name = false;
            }
            _ if in_tag => {
                if reading_name {
                    if ch.is_ascii_alphabetic() || ch == '/' {
                        if ch != '/' {
                            tag_name.push(ch);
                        }
                    } else {
                        reading_name = false;
                    }
                }
            }
            _ => out.push(ch),
        }
    }

    decode_basic_entities(&collapse_whitespace(&out))
}

fn collapse_whitespace(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut prev_space = false;

    for ch in input.chars() {
        if ch.is_whitespace() {
            if !prev_space {
                out.push(' ');
                prev_space = true;
            }
        } else {
            out.push(ch);
            prev_space = false;
        }
    }

    out.trim().to_string()
}

fn decode_basic_entities(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch != '&' {
            out.push(ch);
            continue;
        }

        let mut entity = String::new();
        while let Some(&next) = chars.peek() {
            if next == ';' {
                chars.next();
                break;
            }
            if next.is_ascii_alphanumeric() || next == '#' {
                entity.push(next);
                chars.next();
            } else {
                break;
            }
        }

        out.push_str(decode_entity(&entity));
    }

    out
}

fn decode_entity(entity: &str) -> &str {
    match entity {
        "amp" => "&",
        "lt" => "<",
        "gt" => ">",
        "quot" => "\"",
        "apos" => "'",
        "nbsp" => " ",
        _ if entity.starts_with("#x") => "",
        _ if entity.starts_with('#') => "",
        _ => "",
    }
}

fn truncate_chars(input: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }

    for (count, (idx, _)) in input.char_indices().enumerate() {
        if count == max_chars {
            let mut truncated = input[..idx].to_string();
            truncated.push('…');
            return truncated;
        }
    }

    input.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn style_with_css_escapes_or_image_functions_is_dropped() {
        for style in [
            r"background:u\72l(https://evil.example/a.gif)",
            "background-image:image-set(\"https://evil.example/a.png\" 1x)",
            "background:image(https://evil.example/a.png)",
            "background:URL (https://evil.example/a.png)",
            "background:ur/**/l(https://evil.example/a.png)",
            "color:red;-moz-binding:x",
        ] {
            assert!(style_is_unsafe(style), "{style}");
            let html = format!("<p style='{style}'>Hi</p>");
            let out = sanitize_html(&html, SanitizeOptions::default());
            assert!(!out.contains("style="), "{style} -> {out}");
        }
        assert!(!style_is_unsafe("color:#333;font-family:Curlz MT"));
        let kept = sanitize_html("<p style=\"color:red\">Hi</p>", SanitizeOptions::default());
        assert!(kept.contains("color:red"), "{kept}");
    }

    #[test]
    fn snippet_truncates_with_ellipsis() {
        let html = "<p>Hello <strong>world</strong> from Alcedo</p>";
        let snippet = html_to_snippet(html, 8);
        assert_eq!(snippet, "Hello wo…");
    }

    #[test]
    fn looks_like_html_ignores_leading_whitespace_and_bom() {
        assert!(looks_like_html("\n\n<div>Hi</div>"));
        assert!(looks_like_html("\u{feff}<p>Hi</p>"));
        assert!(looks_like_html("<!DOCTYPE html><html></html>"));
        assert!(!looks_like_html("Hello\nWorld"));
        assert!(!looks_like_html("<3 thanks"));
        assert!(!looks_like_html("<https://example.com>"));
    }

    #[test]
    fn looks_like_html_detects_fragments_that_start_with_text() {
        assert!(looks_like_html(
            "Hi&nbsp;Carter,<br><span>Hope you are doing well.</span>"
        ));
        assert!(looks_like_html("Hello <strong>world</strong>"));
        assert!(!looks_like_html("See <https://example.com> for details"));
    }

    #[test]
    fn looks_like_escaped_html_detects_mailparser_text_to_html() {
        assert!(looks_like_escaped_html(
            "<html><body>Hi&nbsp;Carter,&lt;br&gt;&lt;span&gt;Hope&lt;/span&gt;</body></html>"
        ));
        assert!(!looks_like_escaped_html(
            "<html><body><p>Hi Carter</p></body></html>"
        ));
        assert!(!looks_like_escaped_html(
            "<html><body><p>On Jan 1, Alice &lt;alice@example.com&gt; wrote:</p><blockquote>Hi</blockquote></body></html>"
        ));
    }

    #[test]
    fn looks_like_generated_plaintext_html_detects_br_only_wrapper() {
        assert!(looks_like_generated_plaintext_html(
            "<html><body>Hello Guys,<br/>Nice talking</body></html>"
        ));
        assert!(looks_like_generated_plaintext_html(
            "<html><body>Hi&nbsp;Carter,&lt;br&gt;</body></html>"
        ));
        assert!(!looks_like_generated_plaintext_html(
            "<html><body><p>Hello Guys,</p><p>Nice talking</p></body></html>"
        ));
        assert!(!looks_like_generated_plaintext_html(
            "<html><body><div>Hello Guys,</div></body></html>"
        ));
        assert!(!looks_like_generated_plaintext_html(
            "<html><body><p>Thanks</p><p>On Jan 1, Alice &lt;alice@example.com&gt; wrote:</p><b>Hi</b></body></html>"
        ));
    }

    #[test]
    fn sanitize_strips_script_and_blocks_remote_images_by_default() {
        let html = r#"<p>Hi</p><script>alert(1)</script><img src="https://cdn.example.com/x.png" alt="x">"#;
        let out = sanitize_html(html, SanitizeOptions::default());
        assert!(!out.to_ascii_lowercase().contains("<script"));
        assert!(!out.contains("alert"));
        assert!(!out.contains("cdn.example.com"), "{out}");
        assert!(out.contains("Hi"), "{out}");
    }

    #[test]
    fn sanitize_keeps_remote_images_when_allowed() {
        let html = r#"<img src="https://cdn.example.com/x.png" alt="x">"#;
        let out = sanitize_html(
            html,
            SanitizeOptions {
                allow_remote_images: true,
            },
        );
        assert!(out.contains("cdn.example.com"), "{out}");
    }

    #[test]
    fn sanitize_keeps_basic_mail_markup() {
        let html =
            "<div style=\"color:red\"><p><b>Hello</b> <font color=\"#333\">there</font></p></div>";
        let out = sanitize_html(html, SanitizeOptions::default());
        assert!(out.contains("<div"), "{out}");
        assert!(out.contains("Hello"), "{out}");
        assert!(out.contains("there"), "{out}");
        assert!(out.contains("<b>") || out.contains("<strong>"), "{out}");
        assert!(out.contains("color"), "{out}");
    }

    #[test]
    fn html_has_remote_images_ignores_cid_and_trackers() {
        assert!(html_has_remote_images(
            r#"<p>Hi</p><img src="https://cdn.example.com/photo.jpg" alt="photo">"#
        ));
        assert!(!html_has_remote_images(
            r#"<p>Hi</p><img src="cid:logo@example.com" alt="logo">"#
        ));
        assert!(!html_has_remote_images("<p>No images here</p>"));
        assert!(!html_has_remote_images(
            r#"<img src="https://pixel.example.com/open" width="1" height="1" alt="">"#
        ));
    }

    #[test]
    fn sanitize_strips_remote_css_urls() {
        let html = "<div style=\"color:red;background-image:url(https://evil.example/pixel.gif)\">Hi</div>";
        let out = sanitize_html(html, SanitizeOptions::default());
        assert!(out.contains("Hi"), "{out}");
        assert!(
            !out.to_ascii_lowercase().contains("url("),
            "remote css url leaked: {out}"
        );
        assert!(
            !out.to_ascii_lowercase().contains("background-image"),
            "background-image leaked: {out}"
        );
    }

    #[test]
    fn sanitize_keeps_table_bgcolor() {
        let html = r##"<table bgcolor="#ffffff"><tr><td bgcolor="#1c71d8">Hi</td></tr></table>"##;
        let out = sanitize_html(html, SanitizeOptions::default());
        let lower = out.to_ascii_lowercase();
        assert!(lower.contains("bgcolor"), "{out}");
        assert!(
            lower.contains("#ffffff") || lower.contains("#1c71d8"),
            "{out}"
        );
    }

    #[test]
    fn sanitize_keeps_gmail_quote_border_and_table_width() {
        let html = concat!(
            r#"<blockquote class="gmail_quote" style="margin:0 0 0 0.8ex;border-left:1px solid rgb(204,204,204);padding-left:1ex">"#,
            r#"<p dir="auto">quoted</p>"#,
            r#"<table width="700" border="0"><tr><td>layout</td></tr></table>"#,
            "</blockquote>",
        );
        let out = sanitize_html(html, SanitizeOptions::default());
        let lower = out.to_ascii_lowercase();
        assert!(lower.contains("blockquote"), "{out}");
        assert!(lower.contains("border-left"), "{out}");
        assert!(lower.contains("dir="), "{out}");
        assert!(
            lower.contains(r#"width="700""#)
                || lower.contains("width='700'")
                || lower.contains("width=700"),
            "{out}"
        );
    }
}
