//! Outbound compose body rendering (plain / HTML / Markdown).

use alcedo_core::ComposeFormat;
use pulldown_cmark::{Options, Parser, html};

use crate::pixel::inject_tracking_pixel;
use crate::plaintext::plaintext_to_fragment;

/// Rendered plain and optional HTML bodies ready for MIME assembly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutboundBodies {
    pub plain: String,
    pub html: Option<String>,
}

/// A styled text run for GTK TextView round-tripping.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StyledRun {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikethrough: bool,
    pub href: Option<String>,
    pub image_cid: Option<String>,
}

/// Render outbound bodies from the compose buffer and optional tracking pixel URL.
pub fn render_outbound(
    format: ComposeFormat,
    body_text: &str,
    tracking_pixel_url: Option<&str>,
) -> OutboundBodies {
    match format {
        ComposeFormat::Plain => {
            if let Some(url) = tracking_pixel_url {
                let fragment = plaintext_to_fragment(body_text);
                let html = wrap_html_document(&fragment);
                let html = inject_tracking_pixel(&html, url);
                OutboundBodies {
                    plain: body_text.to_string(),
                    html: Some(html),
                }
            } else {
                OutboundBodies {
                    plain: body_text.to_string(),
                    html: None,
                }
            }
        }
        ComposeFormat::Html => {
            let sanitized = sanitize_compose_html(body_text);
            let mut html = wrap_html_document(&sanitized);
            if let Some(url) = tracking_pixel_url {
                html = inject_tracking_pixel(&html, url);
            }
            OutboundBodies {
                plain: html_to_plain_email(&sanitized),
                html: Some(html),
            }
        }
        ComposeFormat::Markdown => {
            let rendered = markdown_to_html(body_text);
            let mut html = wrap_html_document(&rendered);
            if let Some(url) = tracking_pixel_url {
                html = inject_tracking_pixel(&html, url);
            }
            OutboundBodies {
                plain: body_text.to_string(),
                html: Some(html),
            }
        }
    }
}

/// Convert Markdown source to a sanitized HTML fragment.
pub fn markdown_to_html(source: &str) -> String {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TABLES);
    let parser = Parser::new_ext(source, options);
    let mut html_out = String::new();
    html::push_html(&mut html_out, parser);
    sanitize_compose_html(&html_out)
}

/// Convert a compose body between Plain / HTML / Markdown without dropping content.
pub fn convert_compose_format(from: ComposeFormat, to: ComposeFormat, body: &str) -> String {
    if from == to {
        return body.to_string();
    }
    match (from, to) {
        (ComposeFormat::Html, ComposeFormat::Plain)
        | (ComposeFormat::Html, ComposeFormat::Markdown) => html_to_plain_email(body),
        (ComposeFormat::Plain, ComposeFormat::Html) => plaintext_to_fragment(body),
        (ComposeFormat::Markdown, ComposeFormat::Html) => markdown_to_html(body),
        (ComposeFormat::Plain, ComposeFormat::Markdown)
        | (ComposeFormat::Markdown, ComposeFormat::Plain) => body.to_string(),
        _ => body.to_string(),
    }
}

/// Sanitize compose HTML with a compose-specific allowlist (not inbound `sanitize_html`).
pub fn sanitize_compose_html(input: &str) -> String {
    let allowed: std::collections::HashSet<&str> = [
        "p",
        "br",
        "div",
        "span",
        "b",
        "i",
        "u",
        "s",
        "del",
        "strong",
        "em",
        "a",
        "ul",
        "ol",
        "li",
        "blockquote",
        "code",
        "pre",
        "h1",
        "h2",
        "h3",
        "hr",
        "table",
        "thead",
        "tbody",
        "tr",
        "th",
        "td",
        "img",
    ]
    .into_iter()
    .collect();
    let mut tag_attributes = std::collections::HashMap::new();
    tag_attributes.insert("a", ["href", "title"].into_iter().collect());
    tag_attributes.insert(
        "img",
        ["src", "alt", "width", "height"].into_iter().collect(),
    );
    tag_attributes.insert(
        "td",
        ["colspan", "rowspan", "align", "valign"]
            .into_iter()
            .collect(),
    );
    tag_attributes.insert(
        "th",
        ["colspan", "rowspan", "align", "valign"]
            .into_iter()
            .collect(),
    );
    tag_attributes.insert("div", ["class"].into_iter().collect());
    tag_attributes.insert(
        "table",
        ["border", "cellpadding", "cellspacing"]
            .into_iter()
            .collect(),
    );
    let mut builder = ammonia::Builder::default();
    builder.tags(allowed);
    builder.tag_attributes(tag_attributes);
    builder.add_generic_attributes(&["style"]);
    builder.filter_style_properties(crate::sanitize::visual_style_properties());
    builder.url_schemes(["http", "https", "mailto", "cid"].into_iter().collect());
    builder.link_rel(Some("noopener noreferrer"));
    builder.attribute_filter(|element, attribute, value| {
        if attribute == "style" && crate::sanitize::style_is_unsafe(value) {
            return None;
        }
        if element == "div" && attribute == "class" {
            if value.trim() == "alcedo-signature" {
                return Some(std::borrow::Cow::Borrowed(value));
            }
            return None;
        }
        if element == "img" && attribute == "src" {
            let trimmed = value.trim();
            if trimmed.to_ascii_lowercase().starts_with("cid:") {
                return Some(std::borrow::Cow::Borrowed(value));
            }
            return None;
        }
        Some(std::borrow::Cow::Borrowed(value))
    });
    strip_non_cid_img_tags(builder.clean(input).to_string())
}

fn strip_non_cid_img_tags(html: String) -> String {
    let lower = html.to_ascii_lowercase();
    let mut offset = 0;
    let mut result = String::new();
    while let Some(rel) = lower[offset..].find("<img") {
        let start = offset + rel;
        result.push_str(&html[offset..start]);
        let Some(tag_end) = crate::trackers::find_tag_end(&html[start..]) else {
            result.push_str(&html[offset..]);
            return result;
        };
        let end = start + tag_end;
        let tag = &html[start..end];
        if parse_src_attr(tag)
            .is_some_and(|src| src.trim().to_ascii_lowercase().starts_with("cid:"))
        {
            result.push_str(tag);
        }
        offset = end;
    }
    result.push_str(&html[offset..]);
    result
}

/// Wrap an HTML fragment in a minimal document.
pub fn wrap_html_document(fragment: &str) -> String {
    format!("<html><body>{fragment}</body></html>")
}

/// Extract plain text from HTML for the text/plain alternative part.
pub fn html_to_plain_email(html: &str) -> String {
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
                let lower_name = tag_name.to_ascii_lowercase();
                if matches!(lower_name.as_str(), "th" | "td") {
                    out.push('\t');
                } else if matches!(lower_name.as_str(), "br" | "p" | "div" | "tr" | "li") {
                    out.push('\n');
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

    decode_basic_entities(&collapse_whitespace_lines(&out))
}

/// Heuristic: true when the plain-text body likely contains Markdown.
pub fn looks_like_markdown(text: &str) -> bool {
    if text.trim().is_empty() {
        return false;
    }

    let mut weak = 0u32;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('#')
            && trimmed
                .chars()
                .find(|ch| *ch != '#')
                .is_some_and(|ch| ch == ' ' || ch == '\t')
        {
            weak += 1;
        }
        if trimmed.starts_with("- ") || trimmed.starts_with("* ") {
            weak += 1;
        }
        if trimmed.chars().next().is_some_and(|ch| ch.is_ascii_digit()) && trimmed.contains('.') {
            let token = trimmed.split_whitespace().next().unwrap_or("");
            if token.ends_with('.')
                && token[..token.len().saturating_sub(1)]
                    .chars()
                    .all(|ch| ch.is_ascii_digit())
            {
                weak += 1;
            }
        }
        if trimmed.starts_with("> ") || trimmed == ">" {
            weak += 1;
        }
    }
    if text.contains("**") {
        weak += 1;
    }
    if text.contains("](") {
        weak += 1;
    }
    if text.contains("```") {
        return true;
    }
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.contains("|---") || trimmed.contains("| ---") {
            return true;
        }
    }
    weak >= 2
}

/// Rewrite `cid:` image sources to `alcedo-cid://` and stamp `data-cid` for display.
///
/// `data-cid` is required so [`cid_imgs_for_send`] can restore `cid:` after the
/// WebView round-trips `src` through `alcedo-cid://`.
pub fn cid_imgs_for_display(
    html: &str,
    cid_to_sha256: &std::collections::HashMap<String, String>,
) -> String {
    if cid_to_sha256.is_empty() {
        return html.to_string();
    }
    let lower = html.to_ascii_lowercase();
    let mut out = String::with_capacity(html.len() + 32);
    let mut cursor = 0;

    while let Some(rel) = lower[cursor..].find("<img") {
        let start = cursor + rel;
        out.push_str(&html[cursor..start]);
        let end = lower[start..]
            .find('>')
            .map(|index| start + index + 1)
            .unwrap_or(html.len());
        out.push_str(&img_tag_for_display(&html[start..end], cid_to_sha256));
        cursor = end;
    }

    out.push_str(&html[cursor..]);
    out
}

fn img_tag_for_display(
    tag: &str,
    cid_to_sha256: &std::collections::HashMap<String, String>,
) -> String {
    let cid_value = parse_data_cid_attr(tag).or_else(|| {
        parse_src_attr(tag).and_then(|src| {
            src.trim()
                .strip_prefix("cid:")
                .map(|cid| cid.trim().to_string())
        })
    });
    let Some(cid_value) = cid_value else {
        return tag.to_string();
    };
    let lookup = normalize_compose_cid(&cid_value);
    let Some(sha256) = cid_to_sha256
        .get(&lookup)
        .or_else(|| cid_to_sha256.get(&cid_value))
    else {
        return tag.to_string();
    };

    let mut rewritten = replace_src_attr(tag, &format!("alcedo-cid://{sha256}"));
    if parse_data_cid_attr(&rewritten).is_none() {
        rewritten = insert_html_attr(&rewritten, "data-cid", &lookup);
    }
    rewritten
}

fn normalize_compose_cid(cid_value: &str) -> String {
    cid_value
        .trim()
        .trim_start_matches('<')
        .trim_end_matches('>')
        .to_string()
}

fn insert_html_attr(tag: &str, name: &str, value: &str) -> String {
    let attr = format!(" {name}=\"{}\"", escape_html_attr(value));
    if let Some(stripped) = tag.strip_suffix("/>") {
        format!("{stripped}{attr} />")
    } else if let Some(stripped) = tag.strip_suffix('>') {
        format!("{stripped}{attr}>")
    } else {
        tag.to_string()
    }
}

/// Rewrite display `alcedo-cid://` images back to `cid:` for send/source.
pub fn cid_imgs_for_send(html: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let mut out = String::with_capacity(html.len());
    let mut cursor = 0;

    while let Some(rel) = lower[cursor..].find("<img") {
        let start = cursor + rel;
        out.push_str(&html[cursor..start]);
        let end = lower[start..]
            .find('>')
            .map(|index| start + index + 1)
            .unwrap_or(html.len());
        let tag = &html[start..end];

        if let Some(data_cid) = parse_data_cid_attr(tag) {
            let mut rewritten = strip_data_cid_attr(tag);
            rewritten = replace_src_attr(&rewritten, &format!("cid:{data_cid}"));
            out.push_str(&rewritten);
        } else if parse_src_attr(tag)
            .is_some_and(|src| src.trim().to_ascii_lowercase().starts_with("alcedo-cid://"))
        {
            // Drop orphaned alcedo-cid images without data-cid.
        } else {
            out.push_str(tag);
        }

        cursor = end;
    }

    out.push_str(&html[cursor..]);
    out
}

fn parse_data_cid_attr(tag: &str) -> Option<String> {
    parse_attr_value(tag, "data-cid")
}

fn strip_data_cid_attr(tag: &str) -> String {
    let lower = tag.to_ascii_lowercase();
    let mut result = String::with_capacity(tag.len());
    let mut cursor = 0;
    while let Some(rel) = lower[cursor..].find("data-cid") {
        let start = cursor + rel;
        result.push_str(&tag[cursor..start]);
        let rest = &tag[start..];
        let Some(eq_idx) = rest.find('=') else {
            result.push_str(rest);
            return result;
        };
        let after_eq = rest[eq_idx + 1..].trim_start();
        let Some(quote) = after_eq.chars().next() else {
            cursor = start + 1;
            continue;
        };
        if quote != '"' && quote != '\'' {
            cursor = start + 1;
            continue;
        }
        let Some(value_end) = after_eq[1..].find(quote).map(|index| index + 2) else {
            cursor = start + 1;
            continue;
        };
        cursor = start + eq_idx + 1 + value_end;
    }
    result.push_str(&tag[cursor..]);
    result
}

pub(crate) fn replace_src_attr(tag: &str, new_src: &str) -> String {
    let lower = tag.to_ascii_lowercase();
    if let Some(rel) = lower.find("src=") {
        let start = rel;
        let rest = &tag[start..];
        let after_eq = rest.find('=').map(|index| &rest[index + 1..]).unwrap_or("");
        let after_eq = after_eq.trim_start();
        let quote = after_eq.chars().next();
        if let Some(quote) = quote.filter(|ch| *ch == '"' || *ch == '\'')
            && let Some(end) = after_eq[1..].find(quote)
        {
            let end_index = start + (rest.len() - after_eq.len()) + 1 + end + 1;
            let mut out = String::with_capacity(tag.len() + new_src.len());
            out.push_str(&tag[..start]);
            out.push_str(&format!("src=\"{new_src}\""));
            out.push_str(&tag[end_index..]);
            return out;
        }
    }
    if tag.ends_with("/>") {
        let insert_at = tag.len() - 2;
        let mut out = String::with_capacity(tag.len() + new_src.len() + 12);
        out.push_str(&tag[..insert_at]);
        out.push_str(&format!(" src=\"{new_src}\" "));
        out.push_str(&tag[insert_at..]);
        out
    } else if tag.ends_with('>') {
        let insert_at = tag.len() - 1;
        let mut out = String::with_capacity(tag.len() + new_src.len() + 12);
        out.push_str(&tag[..insert_at]);
        out.push_str(&format!(" src=\"{new_src}\""));
        out.push_str(&tag[insert_at..]);
        out
    } else {
        tag.to_string()
    }
}

/// Serialize styled runs to an HTML fragment.
pub fn styled_runs_to_html(runs: &[StyledRun]) -> String {
    let mut out = String::new();
    for run in runs {
        if let Some(cid) = &run.image_cid {
            let cid_attr = escape_html_attr(cid);
            out.push_str(&format!(r#"<img src="cid:{cid_attr}" alt="">"#));
            continue;
        }
        if run.text.is_empty() {
            continue;
        }
        let escaped = escape_html_text(&run.text).replace('\n', "<br>");
        let mut inner = escaped;
        if run.strikethrough {
            inner = format!("<s>{inner}</s>");
        }
        if run.underline {
            inner = format!("<u>{inner}</u>");
        }
        if run.italic {
            inner = format!("<i>{inner}</i>");
        }
        if run.bold {
            inner = format!("<b>{inner}</b>");
        }
        if let Some(href) = &run.href {
            let href_attr = escape_html_attr(href);
            inner = format!(r#"<a href="{href_attr}">{inner}</a>"#);
        }
        out.push_str(&inner);
    }
    sanitize_compose_html(&out)
}

/// Parse a simple HTML fragment into styled runs.
pub fn html_to_styled_runs(html: &str) -> Vec<StyledRun> {
    let mut runs = Vec::new();
    let mut stack: Vec<RunStyle> = Vec::new();
    let mut text = String::new();
    let mut chars = html.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '<' {
            flush_text(&mut runs, &mut text, &stack);
            if let Some(tag) = read_tag(&mut chars) {
                if matches!(tag, TagAction::Newline) {
                    text.push('\n');
                } else if let TagAction::Image { cid } = tag {
                    flush_text(&mut runs, &mut text, &stack);
                    runs.push(StyledRun {
                        text: String::new(),
                        bold: false,
                        italic: false,
                        underline: false,
                        strikethrough: false,
                        href: None,
                        image_cid: Some(cid),
                    });
                } else {
                    apply_tag(&mut stack, tag);
                }
            }
            continue;
        }
        text.push(ch);
    }
    flush_text(&mut runs, &mut text, &stack);
    runs
}

#[derive(Clone, Default)]
struct RunStyle {
    bold: bool,
    italic: bool,
    underline: bool,
    strikethrough: bool,
    href: Option<String>,
}

#[derive(Clone)]
enum TagAction {
    Open { name: String, href: Option<String> },
    Close(String),
    Newline,
    Image { cid: String },
}

fn current_style(stack: &[RunStyle]) -> RunStyle {
    let mut style = RunStyle::default();
    for layer in stack {
        style.bold |= layer.bold;
        style.italic |= layer.italic;
        style.underline |= layer.underline;
        style.strikethrough |= layer.strikethrough;
        if layer.href.is_some() {
            style.href = layer.href.clone();
        }
    }
    style
}

fn flush_text(runs: &mut Vec<StyledRun>, text: &mut String, stack: &[RunStyle]) {
    if text.is_empty() {
        return;
    }
    let style = current_style(stack);
    runs.push(StyledRun {
        text: decode_basic_entities(&std::mem::take(text)),
        bold: style.bold,
        italic: style.italic,
        underline: style.underline,
        strikethrough: style.strikethrough,
        href: style.href,
        image_cid: None,
    });
}

fn apply_tag(stack: &mut Vec<RunStyle>, tag: TagAction) {
    match tag {
        TagAction::Newline => {}
        TagAction::Open { name, href } => {
            let layer = match name.as_str() {
                "b" | "strong" => RunStyle {
                    bold: true,
                    ..RunStyle::default()
                },
                "i" | "em" => RunStyle {
                    italic: true,
                    ..RunStyle::default()
                },
                "u" => RunStyle {
                    underline: true,
                    ..RunStyle::default()
                },
                "s" | "strike" | "del" => RunStyle {
                    strikethrough: true,
                    ..RunStyle::default()
                },
                "a" => RunStyle {
                    href,
                    ..RunStyle::default()
                },
                "br" | "p" | "div" => {
                    return;
                }
                _ => return,
            };
            stack.push(layer);
        }
        TagAction::Image { .. } => {}
        TagAction::Close(name) => {
            let target = match name.as_str() {
                "b" | "strong" => "bold",
                "i" | "em" => "italic",
                "u" => "underline",
                "s" | "strike" | "del" => "strikethrough",
                "a" => "href",
                _ => return,
            };
            for i in (0..stack.len()).rev() {
                let remove = match target {
                    "bold" => stack[i].bold,
                    "italic" => stack[i].italic,
                    "underline" => stack[i].underline,
                    "strikethrough" => stack[i].strikethrough,
                    "href" => stack[i].href.is_some(),
                    _ => false,
                };
                if remove {
                    stack.remove(i);
                    break;
                }
            }
        }
    }
}

fn read_tag(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> Option<TagAction> {
    let mut name = String::new();
    let mut closing = false;

    if chars.peek() == Some(&'/') {
        closing = true;
        chars.next();
    }

    while let Some(&ch) = chars.peek() {
        if ch.is_ascii_alphabetic() {
            name.push(ch);
            chars.next();
        } else {
            break;
        }
    }

    if name.is_empty() {
        skip_to_close(chars);
        return None;
    }

    let lower = name.to_ascii_lowercase();
    if !closing {
        let mut attrs = String::new();
        let mut in_quote: Option<char> = None;
        while let Some(&ch) = chars.peek() {
            if let Some(q) = in_quote {
                attrs.push(ch);
                chars.next();
                if ch == q {
                    in_quote = None;
                }
                continue;
            }
            if ch == '"' || ch == '\'' {
                in_quote = Some(ch);
                attrs.push(ch);
                chars.next();
                continue;
            }
            if ch == '>' {
                chars.next();
                break;
            }
            if ch == '/' {
                chars.next();
                if chars.peek() == Some(&'>') {
                    chars.next();
                }
                break;
            }
            attrs.push(ch);
            chars.next();
        }
        if lower == "br" || lower == "p" || lower == "div" {
            return Some(TagAction::Newline);
        }
        if lower == "img" {
            if let Some(src) = parse_src_attr(&attrs) {
                let trimmed = src.trim();
                if trimmed.to_ascii_lowercase().starts_with("cid:") {
                    let cid = trimmed[4..].trim_start_matches('<').trim_end_matches('>');
                    if !cid.is_empty() {
                        return Some(TagAction::Image {
                            cid: cid.to_string(),
                        });
                    }
                }
            }
            return None;
        }
        return Some(TagAction::Open {
            name: lower,
            href: parse_href_attr(&attrs),
        });
    }

    while chars.peek().is_some_and(|&ch| ch != '>') {
        chars.next();
    }
    if chars.peek() == Some(&'>') {
        chars.next();
    }
    Some(TagAction::Close(lower))
}

fn parse_href_attr(attrs: &str) -> Option<String> {
    parse_attr_value(attrs, "href")
}

fn parse_src_attr(attrs: &str) -> Option<String> {
    parse_attr_value(attrs, "src")
}

fn parse_attr_value(attrs: &str, name: &str) -> Option<String> {
    let lower = attrs.to_ascii_lowercase();
    let name_lower = name.to_ascii_lowercase();
    let mut search_from = 0;
    while let Some(rel) = lower[search_from..].find(&name_lower) {
        let idx = search_from + rel;
        let before_ok = idx == 0 || {
            let prev = lower.as_bytes()[idx - 1];
            prev.is_ascii_whitespace() || prev == b'<' || prev == b'/'
        };
        if !before_ok {
            search_from = idx + 1;
            continue;
        }
        let rest = attrs[idx + name_lower.len()..].trim_start();
        let Some(rest) = rest.strip_prefix('=') else {
            search_from = idx + 1;
            continue;
        };
        let rest = rest.trim_start();
        let quote = rest.chars().next()?;
        if quote == '"' || quote == '\'' {
            let rest = &rest[quote.len_utf8()..];
            let end = rest.find(quote)?;
            return Some(decode_basic_entities(&rest[..end]));
        }
        let end = rest
            .find(|ch: char| ch.is_ascii_whitespace() || ch == '>')
            .unwrap_or(rest.len());
        return Some(decode_basic_entities(&rest[..end]));
    }
    None
}

fn skip_to_close(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    for ch in chars.by_ref() {
        if ch == '>' {
            break;
        }
    }
}

fn escape_html_text(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(ch),
        }
    }
    out
}

fn escape_html_attr(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            '<' => out.push_str("&lt;"),
            _ => out.push(ch),
        }
    }
    out
}

fn collapse_whitespace_lines(input: &str) -> String {
    input
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn decode_basic_entities(input: &str) -> String {
    input
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_bold_renders_strong() {
        let html = markdown_to_html("**x**");
        assert!(html.contains("<strong>") || html.contains("<b>"));
        assert!(html.contains("x"));
    }

    #[test]
    fn markdown_strikethrough() {
        let html = markdown_to_html("~~strike~~");
        assert!(html.contains("<del>") || html.contains("<s>"));
        assert!(html.contains("strike"));
    }

    #[test]
    fn sanitize_compose_keeps_signature_div_class() {
        let out = sanitize_compose_html("<div class=\"alcedo-signature\"><p>sig</p></div>");
        assert!(out.contains("class=\"alcedo-signature\""));
        assert!(out.contains("sig"));
    }

    #[test]
    fn sanitize_compose_drops_other_class() {
        let out = sanitize_compose_html("<div class=\"evil\"><p>x</p></div>");
        assert!(!out.contains("class="));
        assert!(out.contains("x"));
    }

    #[test]
    fn sanitize_compose_drops_script() {
        let out = sanitize_compose_html("<p>ok</p><script>alert(1)</script>");
        assert!(!out.contains("<script>"));
        assert!(out.contains("ok"));
    }

    #[test]
    fn sanitize_compose_keeps_line_height_and_margin() {
        let out = sanitize_compose_html("<p style=\"margin:0;line-height:1.5\">Hi</p>");
        assert!(out.contains("line-height:1.5"));
        assert!(out.contains("margin:0"));
        assert!(out.contains("Hi"));
    }

    #[test]
    fn sanitize_compose_drops_disallowed_style() {
        let out = sanitize_compose_html(
            "<p style=\"color:red;line-height:2;background-image:url(https://evil.example/a.gif)\">Hi</p>",
        );
        assert!(out.contains("Hi"));
        assert!(!out.contains("color"));
        assert!(!out.contains("url("));
        assert!(!out.contains("line-height"));
    }

    #[test]
    fn sanitize_compose_keeps_signature_card_styles() {
        let html = r#"<table cellpadding="0" cellspacing="0" border="0" style="border-collapse:collapse;font-family:Arial,Helvetica,sans-serif;color:#102a43;"><tr><td valign="top" style="padding:0 14px 0 0;"><a href="https://windwardlogic.com/" style="text-decoration:none;"><img src="cid:signature-logo-x@alcedo.mail" width="56" height="56" alt="Windward Logic" style="display:block;border:0;width:56px;height:56px;"></a></td><td valign="top" style="padding:0 0 0 14px;border-left:3px solid #e8b56f;"><div style="font-size:16px;line-height:20px;font-weight:bold;color:#0b2545;">Carter St.Clair</div><div style="font-size:14px;line-height:20px;font-weight:bold;color:#0b2545;">Windward <span style="font-weight:normal;color:#1d6f8f;">Logic</span></div></td></tr></table>"#;
        let out = sanitize_compose_html(html);
        assert!(out.contains("cid:signature-logo-x@alcedo.mail"), "{out}");
        assert!(out.contains("border-left:3px solid #e8b56f"), "{out}");
        assert!(out.contains("color:#0b2545"), "{out}");
        assert!(out.contains("color:#1d6f8f"), "{out}");
        assert!(out.contains("font-weight:bold"), "{out}");
        assert!(out.contains("font-weight:normal"), "{out}");
        assert!(out.contains("font-size:16px"), "{out}");
        assert!(out.contains("font-family:Arial"), "{out}");
        assert!(out.contains("text-decoration:none"), "{out}");
        assert!(out.contains("display:block"), "{out}");
        assert!(out.contains("width:56px"), "{out}");
        assert!(out.contains("Carter St.Clair"), "{out}");
        assert!(out.contains("Logic"), "{out}");
    }

    #[test]
    fn render_outbound_plain_no_pixel_is_text_only() {
        let bodies = render_outbound(ComposeFormat::Plain, "Hello", None);
        assert_eq!(bodies.plain, "Hello");
        assert!(bodies.html.is_none());
    }

    #[test]
    fn render_outbound_plain_pixel_injects_img() {
        let bodies = render_outbound(
            ComposeFormat::Plain,
            "Hello",
            Some("https://example.com/p.gif"),
        );
        let html = bodies.html.expect("html part");
        assert!(html.contains("width=\"1\""));
        assert!(html.contains("https://example.com/p.gif"));
        assert_eq!(bodies.plain, "Hello");
    }

    #[test]
    fn render_outbound_html_sanitizes_before_pixel() {
        let bodies = render_outbound(
            ComposeFormat::Html,
            "<p>Hi</p><script>x</script>",
            Some("https://example.com/p.gif"),
        );
        let html = bodies.html.expect("html");
        assert!(!html.contains("<script>"));
        assert!(html.contains("<img"));
        assert!(html.contains("</body>"));
    }

    #[test]
    fn html_to_styled_runs_round_trips_bold() {
        let runs = html_to_styled_runs("<b>a</b>");
        let html = styled_runs_to_html(&runs);
        assert!(html.contains("a"));
        assert!(html.contains("<b>") || html.contains("<strong>"));
    }

    #[test]
    fn html_to_styled_runs_preserves_href_and_style() {
        let runs = html_to_styled_runs(r#"<a href="https://example.com"><b>a</b></a>"#);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].text, "a");
        assert!(runs[0].bold);
        assert_eq!(runs[0].href.as_deref(), Some("https://example.com"));
        let html = styled_runs_to_html(&runs);
        assert!(html.contains(r#"href="https://example.com""#));
        assert!(html.contains("<b>") || html.contains("<strong>"));
    }

    #[test]
    fn html_to_styled_runs_href_with_slashes() {
        let runs = html_to_styled_runs(r#"<a href="https://example.com/path">link</a>"#);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].text, "link");
        assert_eq!(runs[0].href.as_deref(), Some("https://example.com/path"));
    }

    #[test]
    fn sanitize_compose_keeps_cid_img() {
        let out = sanitize_compose_html("<img src=\"cid:abc@alcedo.mail\" alt=\"\">");
        assert!(out.contains("cid:abc@alcedo.mail"));
    }

    #[test]
    fn sanitize_compose_drops_remote_img() {
        let out = sanitize_compose_html("<img src=\"https://e.example/x.png\">");
        assert!(!out.contains("<img"));
        assert!(!out.contains("e.example"));
    }

    #[test]
    fn sanitize_compose_drops_remote_img_with_cid_in_alt() {
        let out = sanitize_compose_html("<img src=\"https://e.example/x.png\" alt=\"cid:keep\">");
        assert!(!out.contains("<img"));
        assert!(!out.contains("e.example"));
    }

    #[test]
    fn html_to_styled_runs_img_src_after_alt_source() {
        let runs = html_to_styled_runs(r#"<img alt="source" src="cid:abc@alcedo.mail">"#);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].image_cid.as_deref(), Some("abc@alcedo.mail"));
    }

    #[test]
    fn render_outbound_html_pixel_after_cid_sanitize() {
        let bodies = render_outbound(
            ComposeFormat::Html,
            "<img src=\"cid:abc@alcedo.mail\"><img src=\"https://e.example/x.png\">",
            Some("https://example.com/p.gif"),
        );
        let html = bodies.html.expect("html");
        assert!(html.contains("cid:abc@alcedo.mail"));
        assert!(!html.contains("e.example"));
        assert!(html.contains("https://example.com/p.gif"));
    }

    #[test]
    fn styled_runs_image_round_trip() {
        let cid = "abc@alcedo.mail";
        let html = styled_runs_to_html(&[StyledRun {
            text: String::new(),
            bold: false,
            italic: false,
            underline: false,
            strikethrough: false,
            href: None,
            image_cid: Some(cid.to_string()),
        }]);
        assert!(html.contains("cid:abc@alcedo.mail"));
        let runs = html_to_styled_runs(&html);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].image_cid.as_deref(), Some(cid));
    }

    #[test]
    fn styled_runs_convert_newlines_to_br() {
        let html = styled_runs_to_html(&[StyledRun {
            text: "a\nb".to_string(),
            bold: false,
            italic: false,
            underline: false,
            strikethrough: false,
            href: None,
            image_cid: None,
        }]);
        assert!(html.contains("a<br>b") || html.contains("a<br/>b") || html.contains("a<br />b"));
    }

    #[test]
    fn html_to_styled_runs_decodes_entities() {
        let runs = html_to_styled_runs("<p>a &amp; b</p>");
        let joined: String = runs.into_iter().map(|run| run.text).collect();
        assert!(joined.contains("a & b"), "{joined}");
    }

    #[test]
    fn looks_like_markdown_empty_is_false() {
        assert!(!looks_like_markdown(""));
        assert!(!looks_like_markdown("   \n  "));
    }

    #[test]
    fn looks_like_markdown_single_weak_signal_false() {
        assert!(!looks_like_markdown("# title only"));
    }

    #[test]
    fn looks_like_markdown_two_weak_signals_true() {
        assert!(looks_like_markdown("# title\n- item"));
    }

    #[test]
    fn looks_like_markdown_fence_is_strong() {
        assert!(looks_like_markdown("```rust\nlet x = 1;\n```"));
    }

    #[test]
    fn looks_like_markdown_table_separator_is_strong() {
        assert!(looks_like_markdown("| a | b |\n| --- | --- |"));
    }

    #[test]
    fn cid_imgs_for_send_rewrites_data_cid() {
        let html = r#"<img src="alcedo-cid://abc" data-cid="img@mail" alt="">"#;
        let out = cid_imgs_for_send(html);
        assert!(out.contains(r#"src="cid:img@mail""#));
        assert!(!out.contains("data-cid"));
        assert!(!out.contains("alcedo-cid://"));
    }

    #[test]
    fn sanitize_compose_keeps_img_size_attrs_and_style() {
        let html = r#"<img src="cid:a@x" alt="" width="320" height="180" style="max-width:100%;height:auto">"#;
        let out = sanitize_compose_html(html);
        let compact: String = out.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(out.contains(r#"width="320""#));
        assert!(out.contains(r#"height="180""#));
        assert!(compact.contains("max-width:100%"));
        assert!(compact.contains("height:auto"));
        assert!(out.contains(r#"src="cid:a@x""#));
    }

    #[test]
    fn cid_imgs_for_send_keeps_size_attrs_and_style() {
        let html = r#"<img src="alcedo-cid://abc" alt="" width="320" height="180" data-cid="img@mail" style="max-width:100%;height:auto">"#;
        let out = cid_imgs_for_send(html);
        let compact: String = out.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(out.contains(r#"src="cid:img@mail""#));
        assert!(out.contains(r#"width="320""#));
        assert!(out.contains(r#"height="180""#));
        assert!(compact.contains("max-width:100%"));
        assert!(!out.contains("data-cid"));
        assert!(!out.contains("alcedo-cid://"));

        let html_first = r#"<img data-cid="img@mail" src="alcedo-cid://abc" alt="" width="320" height="180" style="max-width:100%;height:auto">"#;
        let out_first = cid_imgs_for_send(html_first);
        let compact_first: String = out_first.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(out_first.contains(r#"src="cid:img@mail""#));
        assert!(out_first.contains(r#"width="320""#));
        assert!(out_first.contains(r#"height="180""#));
        assert!(compact_first.contains("max-width:100%"));
        assert!(!out_first.contains("data-cid"));
        assert!(!out_first.contains("alcedo-cid://"));
    }

    #[test]
    fn compose_img_resize_survives_display_round_trip() {
        let html = r#"<img src="cid:img@mail" alt="" width="200" height="100" style="max-width:100%;height:auto">"#;
        let sanitized = sanitize_compose_html(html);
        let mut map = std::collections::HashMap::new();
        map.insert("img@mail".to_string(), "aa".repeat(32));
        let display = cid_imgs_for_display(&sanitized, &map);
        assert!(display.contains(r#"data-cid="img@mail""#), "{display}");
        let sent = cid_imgs_for_send(&display);
        assert!(sent.contains(r#"width="200""#), "{sent}");
        assert!(sent.contains(r#"height="100""#), "{sent}");
        assert!(sent.contains(r#"src="cid:img@mail""#), "{sent}");
    }

    #[test]
    fn cid_imgs_for_send_drops_orphan_alcedo_cid() {
        let html = r#"<img src="alcedo-cid://abc" alt="">"#;
        let out = cid_imgs_for_send(html);
        assert!(!out.contains("<img"));
    }

    #[test]
    fn cid_imgs_for_display_stamps_data_cid() {
        let mut map = std::collections::HashMap::new();
        map.insert("img@mail".to_string(), "deadbeef".repeat(8));
        let sha = map["img@mail"].clone();
        let out = cid_imgs_for_display(r#"<img src="cid:img@mail" alt="">"#, &map);
        assert!(
            out.contains(&format!(r#"src="alcedo-cid://{sha}""#)),
            "{out}"
        );
        assert!(out.contains(r#"data-cid="img@mail""#), "{out}");
        let sent = cid_imgs_for_send(&out);
        assert!(sent.contains(r#"src="cid:img@mail""#), "{sent}");
        assert!(!sent.contains("alcedo-cid://"), "{sent}");
    }

    #[test]
    fn cid_imgs_for_display_keeps_existing_data_cid() {
        let mut map = std::collections::HashMap::new();
        map.insert("img@mail".to_string(), "aa".repeat(32));
        let html = r#"<img src="cid:img@mail" data-cid="img@mail" alt="">"#;
        let out = cid_imgs_for_display(html, &map);
        assert_eq!(out.matches("data-cid=").count(), 1);
    }

    #[test]
    fn html_to_plain_email_ignores_images() {
        let plain = html_to_plain_email(r#"<p>Hi</p><img src="cid:x@y" alt="Logo"><p>Bye</p>"#);
        assert!(plain.contains("Hi"));
        assert!(plain.contains("Bye"));
        assert!(!plain.contains("Logo"));
        assert!(!plain.contains("cid:"));
    }

    #[test]
    fn html_to_plain_email_table_cells() {
        let plain = html_to_plain_email(
            "<table><tr><th>A</th><th>B</th></tr><tr><td>1</td><td>2</td></tr></table>",
        );
        assert!(plain.contains('A'));
        assert!(plain.contains('B'));
        assert!(plain.contains('1'));
        assert!(plain.contains('2'));
        assert!(!plain.contains("AB12"));
        assert!(plain.contains('\t') || plain.lines().count() > 1);
    }

    #[test]
    fn convert_compose_format_preserves_quoted_reply() {
        let plain = "Thanks\n\n---\n\nOn Jan 1, Alice wrote:\n\n> Hello\n> World\n";
        let html = convert_compose_format(ComposeFormat::Plain, ComposeFormat::Html, plain);
        assert!(html.contains("Thanks"), "{html}");
        assert!(html.contains("Hello"), "{html}");
        assert!(html.contains("World"), "{html}");
        assert!(html.contains("blockquote"), "{html}");
        let roundtrip = convert_compose_format(ComposeFormat::Html, ComposeFormat::Plain, &html);
        assert!(roundtrip.contains("Thanks"), "{roundtrip}");
        assert!(roundtrip.contains("Hello"), "{roundtrip}");
        assert!(roundtrip.contains("World"), "{roundtrip}");
    }
}
