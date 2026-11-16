//! Plaintext mail body → HTML fragment (quotes + autolink).

use linkify::{LinkFinder, LinkKind};

/// Wrap bare URLs and emails in sanitized HTML text nodes with `<a href>`.
///
/// Existing `<a>` markup is left alone so the mail WebView can keep author links.
/// Call this after [`crate::sanitize_html`], not on raw input.
pub fn autolink_html_fragment(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    let mut anchor_depth = 0usize;

    while let Some(lt) = rest.find('<') {
        let text = &rest[..lt];
        if anchor_depth == 0 {
            out.push_str(&autolink_html_text(text));
        } else {
            out.push_str(text);
        }

        let tag_src = &rest[lt..];
        let Some(gt) = tag_src.find('>') else {
            if anchor_depth == 0 {
                out.push_str(&autolink_html_text(tag_src));
            } else {
                out.push_str(tag_src);
            }
            return out;
        };
        let tag = &tag_src[..=gt];
        out.push_str(tag);
        rest = &tag_src[gt + 1..];
        match tag_kind(tag) {
            TagKind::AnchorOpen => anchor_depth = anchor_depth.saturating_add(1),
            TagKind::AnchorClose => anchor_depth = anchor_depth.saturating_sub(1),
            TagKind::Other => {}
        }
    }

    if anchor_depth == 0 {
        out.push_str(&autolink_html_text(rest));
    } else {
        out.push_str(rest);
    }
    out
}

enum TagKind {
    AnchorOpen,
    AnchorClose,
    Other,
}

fn tag_kind(tag: &str) -> TagKind {
    let inner = tag
        .strip_prefix('<')
        .and_then(|value| value.strip_suffix('>'))
        .unwrap_or(tag)
        .trim();
    if inner.starts_with("!--") {
        return TagKind::Other;
    }
    let closing = inner.starts_with('/');
    let name = inner
        .trim_start_matches('/')
        .split(|ch: char| ch.is_ascii_whitespace() || ch == '/')
        .next()
        .unwrap_or("");
    if !name.eq_ignore_ascii_case("a") {
        return TagKind::Other;
    }
    if closing {
        TagKind::AnchorClose
    } else {
        TagKind::AnchorOpen
    }
}

/// Render plaintext as an HTML fragment: nested `>` quotes and autolinked URLs/emails.
///
/// Also un-wraps RFC 3676 `format=flowed` soft breaks (see [`join_flowed_lines`])
/// so long paragraphs reflow to the reader's width instead of staying stuck at
/// the sender's original wrap column.
pub fn plaintext_to_fragment(text: &str) -> String {
    let text = normalize_line_endings(text);
    let logical_lines = join_flowed_lines(&text);
    let mut html = String::new();
    let mut quote_depth = 0usize;

    for (line_index, (depth, content)) in logical_lines.iter().enumerate() {
        let depth = *depth;

        while quote_depth > depth {
            html.push_str("</blockquote>");
            quote_depth -= 1;
        }

        if line_index > 0 && !html.is_empty() {
            html.push_str("<br>");
        }

        while quote_depth < depth {
            html.push_str(r#"<blockquote class="alcedo-quote">"#);
            quote_depth += 1;
        }

        html.push_str(&autolink_line(content));
    }

    while quote_depth > 0 {
        html.push_str("</blockquote>");
        quote_depth -= 1;
    }

    if html.is_empty() {
        html.push_str("<p></p>");
    }

    html
}

/// Join RFC 3676 `format=flowed` soft-wrapped lines into logical lines.
///
/// A generator using `format=flowed` ends every "soft" (reflowable) line with
/// exactly one trailing space; a real paragraph/line break has no trailing
/// space. Clients are expected to strip that space and join the line with the
/// next one, then let their own renderer re-wrap the result to its available
/// width — which is exactly why the same message looks tightly re-flowed in
/// flowed-aware clients but stays stuck at the sender's ~72-column wrap in
/// clients that treat every newline as a hard break.
///
/// The RFC also carves out one exception: a line that is exactly the
/// signature separator (`"-- "`) is never treated as flowed, even though it
/// ends in a space. Joining only happens between lines at the same quote
/// depth, so a soft-wrapped line is never merged across a quote boundary.
fn join_flowed_lines(text: &str) -> Vec<(usize, String)> {
    let mut lines: Vec<(usize, String)> = Vec::new();

    for line in text.split('\n') {
        let (depth, content) = quote_depth_and_content(line);
        if let Some((prev_depth, prev_content)) = lines.last_mut()
            && *prev_depth == depth
            && is_flowed_soft_break(prev_content)
        {
            prev_content.push_str(content);
            continue;
        }
        lines.push((depth, content.to_string()));
    }

    lines
}

fn is_flowed_soft_break(line: &str) -> bool {
    line.ends_with(' ') && line != "-- "
}

/// Mail bodies (and `mail-parser`'s `body_text`) commonly keep RFC 5322 `\r\n`
/// line endings verbatim. Left as-is, the leftover `\r` survives into the
/// rendered `white-space: pre-wrap` fragment, where HTML5 newline
/// normalization turns it into a second, phantom line break stacked on top
/// of our explicit `<br>` — doubling the visual gap between every line.
fn normalize_line_endings(text: &str) -> String {
    if text.contains('\r') {
        text.replace("\r\n", "\n").replace('\r', "\n")
    } else {
        text.to_string()
    }
}

fn quote_depth_and_content(line: &str) -> (usize, &str) {
    let mut rest = line;
    let mut depth = 0usize;
    while let Some(after) = rest.strip_prefix('>') {
        depth += 1;
        rest = after.strip_prefix(' ').unwrap_or(after);
    }
    (depth, rest)
}

fn autolink_line(line: &str) -> String {
    let mut finder = LinkFinder::new();
    finder.url_must_have_scheme(false);
    let mut out = String::new();
    let mut last = 0usize;

    for link in finder.links(line) {
        out.push_str(&escape_html(&line[last..link.start()]));
        let display = escape_html(link.as_str());
        if let Some(href) = link_href(&link) {
            out.push_str(&anchor_html(&href, &display));
        } else {
            out.push_str(&display);
        }
        last = link.end();
    }

    out.push_str(&escape_html(&line[last..]));
    out
}

fn autolink_html_text(escaped: &str) -> String {
    if escaped.is_empty() {
        return String::new();
    }

    let (decoded, map) = decode_html_text(escaped);
    let mut finder = LinkFinder::new();
    finder.url_must_have_scheme(false);
    let mut out = String::new();
    let mut last = 0usize;

    for link in finder.links(&decoded) {
        let orig_start = map[link.start()];
        let orig_end = map[link.end()];
        out.push_str(&escaped[map[last]..orig_start]);
        let display = &escaped[orig_start..orig_end];
        if let Some(href) = link_href(&link) {
            out.push_str(&anchor_html(&href, display));
        } else {
            out.push_str(display);
        }
        last = link.end();
    }

    out.push_str(&escaped[map[last]..]);
    out
}

fn decode_html_text(input: &str) -> (String, Vec<usize>) {
    let mut decoded = String::with_capacity(input.len());
    let mut map = Vec::with_capacity(input.len() + 1);
    map.push(0);

    let mut orig = 0usize;
    while orig < input.len() {
        let rest = &input[orig..];
        let (consumed, replacement) = if rest.starts_with('&') {
            decode_entity_at(rest)
        } else {
            let ch = rest.chars().next().expect("non-empty rest");
            (ch.len_utf8(), ch.to_string())
        };
        let orig_end = orig + consumed;
        push_decoded(&mut decoded, &mut map, orig, orig_end, &replacement);
        orig = orig_end;
    }

    debug_assert_eq!(map.len(), decoded.len() + 1);
    (decoded, map)
}

fn push_decoded(
    decoded: &mut String,
    map: &mut Vec<usize>,
    orig_start: usize,
    orig_end: usize,
    replacement: &str,
) {
    let start_len = decoded.len();
    decoded.push_str(replacement);
    let added = decoded.len() - start_len;
    if added == 0 {
        return;
    }
    for _ in 0..added.saturating_sub(1) {
        map.push(orig_start);
    }
    map.push(orig_end);
}

fn decode_entity_at(input: &str) -> (usize, String) {
    let Some(body) = input.strip_prefix('&') else {
        return (1, "&".to_string());
    };
    let Some(end) = body.find(';') else {
        return (1, "&".to_string());
    };
    let name = &body[..end];
    let consumed = end + 2;
    let replacement = match name {
        "amp" => Some("&".to_string()),
        "lt" => Some("<".to_string()),
        "gt" => Some(">".to_string()),
        "quot" => Some("\"".to_string()),
        "apos" => Some("'".to_string()),
        "nbsp" => Some(" ".to_string()),
        _ => decode_numeric_entity(name),
    };
    match replacement {
        Some(text) => (consumed, text),
        None => (1, "&".to_string()),
    }
}

fn decode_numeric_entity(name: &str) -> Option<String> {
    let code = if let Some(hex) = name.strip_prefix("#x").or_else(|| name.strip_prefix("#X")) {
        u32::from_str_radix(hex, 16).ok()?
    } else {
        let digits = name.strip_prefix('#')?;
        digits.parse::<u32>().ok()?
    };
    char::from_u32(code).map(String::from)
}

fn anchor_html(href: &str, display: &str) -> String {
    let href_attr = escape_attr(href);
    format!(r#"<a href="{href_attr}" rel="noopener noreferrer">{display}</a>"#)
}

fn link_href(link: &linkify::Link<'_>) -> Option<String> {
    match link.kind() {
        LinkKind::Email => {
            let address = link.as_str();
            if address.is_empty() {
                return None;
            }
            Some(format!("mailto:{address}"))
        }
        LinkKind::Url => {
            let raw = link.as_str();
            // "St.Clair" matches the loose dotted-word rule. A schemeless URL
            // with a capital letter is a name, not a host.
            if !raw.contains("://") && raw.chars().any(|ch| ch.is_ascii_uppercase()) {
                return None;
            }
            let href = if raw.starts_with("http://") || raw.starts_with("https://") {
                raw.to_string()
            } else {
                format!("https://{raw}")
            };
            let parsed = url::Url::parse(&href).ok()?;
            match parsed.scheme() {
                "http" | "https" => Some(href),
                _ => None,
            }
        }
        _ => None,
    }
}

fn escape_attr(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
}

fn escape_html(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn does_not_autolink_capitalized_name() {
        let html = autolink_html_fragment("<div>Carter St.Clair</div>");
        assert!(!html.contains("<a"), "{html}");
        assert!(html.contains("Carter St.Clair"), "{html}");
        let site = autolink_html_fragment("<div>windwardlogic.com</div>");
        assert!(site.contains("https://windwardlogic.com"), "{site}");
    }

    #[test]
    fn autolinks_bare_url_and_email() {
        let html = plaintext_to_fragment("Visit https://example.com or email me@example.com");
        assert!(html.contains(r#"href="https://example.com""#));
        assert!(html.contains(r#"href="mailto:me@example.com""#));
        assert!(html.contains(">https://example.com<"));
        assert!(html.contains(">me@example.com<"));
    }

    #[test]
    fn www_url_gets_https_href() {
        let html = plaintext_to_fragment("See www.example.com for details");
        assert!(html.contains(r#"href="https://www.example.com""#));
    }

    #[test]
    fn preserves_explicit_https_href() {
        let html = plaintext_to_fragment("Visit https://example.org/path");
        assert!(html.contains(r#"href="https://example.org/path""#));
    }

    #[test]
    fn quote_lines_nest_blockquotes() {
        let html = plaintext_to_fragment("> line one\n>> line two\n> line three");
        assert_eq!(
            html,
            concat!(
                r#"<blockquote class="alcedo-quote">line one<br>"#,
                r#"<blockquote class="alcedo-quote">line two</blockquote><br>line three</blockquote>"#
            )
        );
    }

    #[test]
    fn spaced_quote_markers_nest() {
        let html = plaintext_to_fragment("> outer\n> > inner");
        assert_eq!(
            html,
            concat!(
                r#"<blockquote class="alcedo-quote">outer<br>"#,
                r#"<blockquote class="alcedo-quote">inner</blockquote></blockquote>"#
            )
        );
    }

    #[test]
    fn flowed_soft_break_joins_into_one_reflowable_line() {
        let html = plaintext_to_fragment(
            "* I like the compliance trends - could a walk-through schedule be \r\nincluded, or do you prefer to keep it quiet?\r\n* Could we leverage online polls?",
        );
        assert_eq!(
            html,
            concat!(
                "* I like the compliance trends - could a walk-through schedule be ",
                "included, or do you prefer to keep it quiet?<br>",
                "* Could we leverage online polls?"
            )
        );
    }

    #[test]
    fn flowed_soft_break_joins_across_multiple_lines() {
        let html = plaintext_to_fragment(
            "Good afternoon, see below and attached from the fine hearing committee \r\nand advise if the board is ok sending out the attached once completed \r\nto the residents.",
        );
        assert_eq!(
            html,
            "Good afternoon, see below and attached from the fine hearing committee and advise if the board is ok sending out the attached once completed to the residents."
        );
    }

    #[test]
    fn flowed_soft_break_joins_within_quoted_lines_only() {
        let html = plaintext_to_fragment(
            "> Good afternoon, see below and attached from the fine hearing committee \r\n> and advise if the board is ok sending out the attached once completed \r\n> to the residents.",
        );
        assert_eq!(
            html,
            concat!(
                r#"<blockquote class="alcedo-quote">"#,
                "Good afternoon, see below and attached from the fine hearing committee and advise if the board is ok sending out the attached once completed to the residents.",
                "</blockquote>"
            )
        );
    }

    #[test]
    fn signature_separator_is_never_treated_as_flowed() {
        let html = plaintext_to_fragment("Thanks,\r\n-- \r\nCarter");
        assert_eq!(html, "Thanks,<br>-- <br>Carter");
    }

    #[test]
    fn blank_lines_still_mark_paragraph_breaks() {
        let html =
            plaintext_to_fragment("Could we leverage online polls?\r\n\r\nThanks,\r\n-Carter");
        assert_eq!(
            html,
            "Could we leverage online polls?<br><br>Thanks,<br>-Carter"
        );
    }

    #[test]
    fn crlf_line_endings_do_not_double_line_breaks() {
        let html = plaintext_to_fragment("line one\r\nline two\r\n\r\nparagraph two\r\n");
        assert_eq!(
            html, "line one<br>line two<br><br>paragraph two<br>",
            "leftover \\r must not survive as a phantom extra line break: {html}"
        );
        assert!(!html.contains('\r'));
    }

    #[test]
    fn escapes_raw_angle_brackets() {
        let html = plaintext_to_fragment("<script>alert(1)</script>");
        assert!(!html.contains("<script>"));
        assert!(html.contains("&lt;script&gt;"));
    }

    #[test]
    fn malicious_href_is_not_linked() {
        let html = plaintext_to_fragment("click javascript:alert(1) now");
        assert!(!html.contains("href=\"javascript:"));
        assert!(html.contains("javascript:alert(1)"));
    }

    #[test]
    fn html_fragment_autolinks_bare_url_and_email() {
        let html = autolink_html_fragment(
            "<p>Visit https://example.com/mock or email mock-links@example.com</p>",
        );
        assert!(html.contains(r#"href="https://example.com/mock""#));
        assert!(html.contains(r#"href="mailto:mock-links@example.com""#));
        assert!(html.contains("<p>"));
        assert!(html.contains("</p>"));
    }

    #[test]
    fn html_fragment_preserves_existing_anchors() {
        let html = autolink_html_fragment(
            r#"<p><a href="https://kept.example/path">https://kept.example/path</a> and https://new.example</p>"#,
        );
        assert!(html.contains(r#"href="https://kept.example/path""#));
        assert!(html.contains(r#"href="https://new.example""#));
        assert_eq!(html.matches("<a ").count(), 2);
        assert!(!html.contains("<a href=\"https://kept.example/path\"><a "));
    }

    #[test]
    fn html_fragment_preserves_query_ampersands() {
        let html = autolink_html_fragment("<span>See https://example.com?a=1&amp;b=2</span>");
        assert!(html.contains(r#"href="https://example.com?a=1&amp;b=2""#));
        assert!(html.contains("https://example.com?a=1&amp;b=2</a>"));
    }
}
