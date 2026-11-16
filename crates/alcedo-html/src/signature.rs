//! Compose signature insertion and stripping.

use alcedo_core::{AccountSignature, ComposeFormat, ComposeInline};

use crate::compose::{convert_compose_format, html_to_plain_email, sanitize_compose_html};
use crate::trackers::{find_tag_end, parse_tag_attributes, remove_img_tags};

const HTML_REPLY_MARKER: &str = "<p>────────────────────────</p>";
const HTML_FORWARD_MARKER: &str = "<p>---------- Forwarded message ---------</p>";
const PLAIN_FORWARD_MARKER: &str = "---------- Forwarded message ---------";
const PLAIN_REPLY_SEP: &str = "\n---\n";
const SIGNATURE_WRAPPER_OPEN: &str = "<div class=\"alcedo-signature\">";
const SIGNATURE_WRAPPER_CLOSE: &str = "</div>";

#[derive(Clone, Debug, PartialEq, Eq)]
struct BodyParts {
    draft: String,
    quote: String,
}

fn split_compose_body(format: ComposeFormat, body: &str) -> BodyParts {
    match format {
        ComposeFormat::Html => {
            if let Some(idx) = body.find(HTML_REPLY_MARKER) {
                return BodyParts {
                    draft: body[..idx].trim_end().to_string(),
                    quote: body[idx..].to_string(),
                };
            }
            if let Some(idx) = body.find(HTML_FORWARD_MARKER) {
                return BodyParts {
                    draft: body[..idx].trim_end().to_string(),
                    quote: body[idx..].to_string(),
                };
            }
            BodyParts {
                draft: body.to_string(),
                quote: String::new(),
            }
        }
        ComposeFormat::Plain | ComposeFormat::Markdown => {
            if let Some(idx) = body.find(PLAIN_REPLY_SEP) {
                return BodyParts {
                    draft: body[..idx].trim_end().to_string(),
                    quote: body[idx..].to_string(),
                };
            }
            let forward_sep = format!("\n{PLAIN_FORWARD_MARKER}\n");
            if let Some(idx) = body.find(&forward_sep) {
                return BodyParts {
                    draft: body[..idx].trim_end().to_string(),
                    quote: body[idx..].to_string(),
                };
            }
            if body.starts_with(PLAIN_FORWARD_MARKER) {
                return BodyParts {
                    draft: String::new(),
                    quote: body.to_string(),
                };
            }
            BodyParts {
                draft: body.to_string(),
                quote: String::new(),
            }
        }
    }
}

fn join_compose_body(draft: &str, quote: &str) -> String {
    if quote.is_empty() {
        return draft.to_string();
    }
    if draft.is_empty() {
        return quote.to_string();
    }
    format!("{draft}{quote}")
}

/// Wrap a sanitized HTML fragment in the signature container.
pub fn signature_html_wrapped(sanitized_fragment: &str) -> String {
    format!("{SIGNATURE_WRAPPER_OPEN}{sanitized_fragment}{SIGNATURE_WRAPPER_CLOSE}")
}

/// Convert a stored signature to the target compose format.
pub fn render_signature_for_format(sig: &AccountSignature, target: ComposeFormat) -> String {
    if sig.body.trim().is_empty() {
        return String::new();
    }
    convert_compose_format(sig.format, target, &sig.body)
}

fn strip_plain_signature(draft: &str) -> String {
    let lines: Vec<&str> = draft.lines().collect();
    let mut sig_start = None;
    for (idx, line) in lines.iter().enumerate().rev() {
        let trimmed = line.trim_end();
        if trimmed == "--" || trimmed == "-- " {
            sig_start = Some(idx);
            break;
        }
    }
    match sig_start {
        Some(idx) => lines[..idx].join("\n").trim_end().to_string(),
        None => draft.to_string(),
    }
}

/// Remove an existing signature from the draft portion of the body.
pub fn strip_signature(body: &str, format: ComposeFormat) -> String {
    let parts = split_compose_body(format, body);
    let draft = match format {
        ComposeFormat::Html => {
            if let Some(start) = parts.draft.rfind(SIGNATURE_WRAPPER_OPEN) {
                parts.draft[..start].trim_end().to_string()
            } else {
                parts.draft
            }
        }
        ComposeFormat::Plain | ComposeFormat::Markdown => strip_plain_signature(&parts.draft),
    };
    join_compose_body(&draft, &parts.quote)
}

fn append_signature(draft: &str, format: ComposeFormat, rendered: &str) -> String {
    if rendered.trim().is_empty() {
        return draft.to_string();
    }
    match format {
        ComposeFormat::Html => {
            let fragment = sanitize_compose_html(rendered);
            let wrapped = signature_html_wrapped(&fragment);
            if draft.trim().is_empty() {
                wrapped
            } else {
                format!("{draft}{wrapped}")
            }
        }
        ComposeFormat::Plain | ComposeFormat::Markdown => {
            if draft.trim().is_empty() {
                format!("-- \n{rendered}")
            } else {
                format!("{draft}\n-- \n{rendered}")
            }
        }
    }
}

/// Strip any existing signature, optionally append the account signature, then rejoin quotes.
pub fn apply_signature(
    body: &str,
    format: ComposeFormat,
    sig: Option<&AccountSignature>,
    include: bool,
) -> String {
    let stripped = strip_signature(body, format);
    let parts = split_compose_body(format, &stripped);
    let draft = if include {
        let rendered = sig.map(|s| render_signature_for_format(s, format));
        match rendered {
            Some(text) if !text.trim().is_empty() => append_signature(&parts.draft, format, &text),
            _ => parts.draft,
        }
    } else {
        parts.draft
    };
    join_compose_body(&draft, &parts.quote)
}

fn body_references_cid(body: &str, cid: &str) -> bool {
    let needle = format!("cid:{cid}");
    body.to_ascii_lowercase()
        .contains(&needle.to_ascii_lowercase())
}

pub fn signature_inlines_for_body(
    sig: Option<&AccountSignature>,
    body: &str,
) -> Vec<ComposeInline> {
    let Some(sig) = sig else {
        return Vec::new();
    };
    sig.inlines
        .iter()
        .filter(|inline| body_references_cid(body, &inline.content_id))
        .cloned()
        .collect()
}

fn img_src_matches_cid(src: &str, cid: &str) -> bool {
    let trimmed = src.trim();
    let Some(rest) = trimmed.strip_prefix("cid:") else {
        return false;
    };
    rest.trim().eq_ignore_ascii_case(cid)
}

fn canonicalize_cid_srcs(html: &str, allowed_cids: &[String]) -> String {
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
        let src = attrs.get("src").map(String::as_str).unwrap_or("");
        let rewritten = if let Some(cid) = allowed_cids
            .iter()
            .find(|cid| img_src_matches_cid(src, cid))
        {
            let exact = format!("cid:{cid}");
            if src.trim() == exact {
                full_tag.to_string()
            } else {
                crate::compose::replace_src_attr(full_tag, &exact)
            }
        } else {
            full_tag.to_string()
        };
        out.push_str(&rewritten);

        cursor = start + tag_end;
    }

    out.push_str(&html[cursor..]);
    out
}

pub fn sanitize_signature_html(input: &str, allowed_cids: &[String]) -> String {
    let sanitized = sanitize_compose_html(input);
    let filtered = remove_img_tags(&sanitized, |attrs| {
        let src = attrs.get("src").map(String::as_str).unwrap_or("");
        !allowed_cids.iter().any(|cid| img_src_matches_cid(src, cid))
    });
    let canonical = canonicalize_cid_srcs(&filtered, allowed_cids);
    unwrap_lone_signature_div(&canonical)
}

pub fn remove_inline_imgs(html: &str, cids: &[String]) -> String {
    remove_img_tags(html, |attrs| {
        let src = attrs.get("src").map(String::as_str).unwrap_or("");
        cids.iter().any(|cid| img_src_matches_cid(src, cid))
    })
}

fn unwrap_lone_signature_div(html: &str) -> String {
    let trimmed = html.trim();
    match single_outer_signature_inner(trimmed) {
        Some(inner) => inner.to_string(),
        None => trimmed.to_string(),
    }
}

fn single_outer_signature_inner(trimmed: &str) -> Option<&str> {
    if !trimmed.starts_with(SIGNATURE_WRAPPER_OPEN) {
        return None;
    }
    let base = SIGNATURE_WRAPPER_OPEN.len();
    let lower = trimmed.to_ascii_lowercase();
    let mut depth = 1;
    let mut i = base;
    while i < trimmed.len() {
        if trimmed[i..].starts_with("</div>") {
            depth -= 1;
            if depth == 0 {
                let after = i + SIGNATURE_WRAPPER_CLOSE.len();
                if after == trimmed.len() {
                    return Some(&trimmed[base..i]);
                }
                return None;
            }
            i += SIGNATURE_WRAPPER_CLOSE.len();
            continue;
        }
        if lower[i..].starts_with("<div") {
            let after_div = i + 4;
            if after_div >= trimmed.len() || !trimmed.as_bytes()[after_div].is_ascii_alphanumeric()
            {
                depth += 1;
            }
        }
        i += 1;
    }
    None
}

pub fn signature_plain_mail_preview(sanitized_body: &str) -> String {
    let plain = html_to_plain_email(sanitized_body);
    if plain.trim().is_empty() {
        return "Hi,".to_string();
    }
    format!("Hi,\n-- \n{plain}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use alcedo_core::{AccountId, ComposeFormat};
    use uuid::Uuid;

    fn sig(body: &str, format: ComposeFormat) -> AccountSignature {
        AccountSignature {
            account_id: AccountId(Uuid::new_v4()),
            format,
            body: body.to_string(),
            inlines: Vec::new(),
        }
    }

    fn sig_with_inlines(body: &str, inlines: Vec<ComposeInline>) -> AccountSignature {
        AccountSignature {
            account_id: AccountId(Uuid::new_v4()),
            format: ComposeFormat::Html,
            body: body.to_string(),
            inlines,
        }
    }

    #[test]
    fn sanitize_signature_html_keeps_allowed_cids() {
        let cid_a = "a@example.com".to_string();
        let cid_b = "b@example.com".to_string();
        let input = format!(
            r#"<p>Ada</p><img src="https://evil.example/x.png"><img src="cid:{cid_a}"><img src="cid:{cid_b}"><img src="cid:other@x"><img src="data:image/png;base64,AA==">"#
        );
        let out = sanitize_signature_html(&input, &[cid_a.clone(), cid_b.clone()]);
        assert!(!out.contains("https://"));
        assert!(!out.contains("other@x"));
        assert!(!out.contains("data:"));
        assert!(out.contains(&format!("cid:{cid_a}")));
        assert!(out.contains(&format!("cid:{cid_b}")));
    }

    #[test]
    fn sanitize_signature_html_canonicalizes_cid_case() {
        let cid = "a@example.com".to_string();
        let input = r#"<img src="cid:A@example.com">"#;
        let out = sanitize_signature_html(input, &[cid]);
        assert!(out.contains("cid:a@example.com"));
        assert!(!out.contains("A@example.com"));
    }

    #[test]
    fn sanitize_signature_html_unwraps_lone_wrapper() {
        let cid = "logo@example.com".to_string();
        let input =
            format!(r#"<div class="alcedo-signature"><p>Ada</p><img src="cid:{cid}"></div>"#);
        let out = sanitize_signature_html(&input, &[cid]);
        assert!(!out.starts_with("<div class=\"alcedo-signature\">"));
        assert!(out.contains("<p>Ada</p>"));
    }

    #[test]
    fn sanitize_signature_html_does_not_unwrap_two_wrappers() {
        let input = r#"<div class="alcedo-signature"><p>Ada</p></div><div><p>Bee</p></div>"#;
        let out = sanitize_signature_html(input, &[]);
        assert!(out.contains("<p>Ada</p>"));
        assert!(out.contains("<p>Bee</p>"));
        assert_eq!(out.matches("<div").count(), out.matches("</div>").count());
    }

    #[test]
    fn signature_inlines_for_body_order_and_filter() {
        let a = ComposeInline {
            content_id: "a@x".to_string(),
            filename: "a.png".to_string(),
            mime: "image/png".to_string(),
            bytes: vec![1],
        };
        let b = ComposeInline {
            content_id: "b@x".to_string(),
            filename: "b.png".to_string(),
            mime: "image/png".to_string(),
            bytes: vec![2],
        };
        let sig = sig_with_inlines(
            r#"<img src="cid:b@x"><img src="cid:a@x">"#,
            vec![a.clone(), b.clone()],
        );
        let inlines = signature_inlines_for_body(Some(&sig), &sig.body);
        assert_eq!(inlines.len(), 2);
        assert_eq!(inlines[0].content_id, "a@x");
        assert_eq!(inlines[1].content_id, "b@x");
        assert!(signature_inlines_for_body(Some(&sig), "no images").is_empty());
    }

    #[test]
    fn remove_inline_imgs_drops_listed_cids() {
        let html = r#"<p>x</p><img src="cid:a@x"><img src="cid:b@x">"#;
        let out = remove_inline_imgs(html, &["a@x".to_string()]);
        assert!(!out.contains("cid:a@x"));
        assert!(out.contains("cid:b@x"));
    }

    #[test]
    fn plain_render_has_no_cid_inlines() {
        let sig = sig("Alice\nExample", ComposeFormat::Plain);
        let rendered = render_signature_for_format(&sig, ComposeFormat::Plain);
        assert!(!rendered.contains("cid:"));
        assert!(signature_inlines_for_body(Some(&sig), &rendered).is_empty());
    }

    #[test]
    fn signature_plain_mail_preview_ignores_images() {
        let html = r#"<p>Ada</p><img alt="Logo" src="cid:x@y">"#;
        let plain = signature_plain_mail_preview(html);
        assert_eq!(plain, "Hi,\n-- \nAda");
        assert!(!plain.contains("Logo"));
    }

    #[test]
    fn html_reply_signature_before_marker() {
        let body = apply_signature(
            "<p>Hi</p><p>────────────────────────</p><p>old</p>",
            ComposeFormat::Html,
            Some(&sig("<p>Best</p>", ComposeFormat::Html)),
            true,
        );
        assert!(
            body.find("<div class=\"alcedo-signature\">").unwrap()
                < body.find("────────────────────────").unwrap()
        );
    }

    #[test]
    fn plain_reply_signature_before_quote_sep() {
        let body = apply_signature(
            "Thanks\n---\n> quote",
            ComposeFormat::Plain,
            Some(&sig("Regards", ComposeFormat::Plain)),
            true,
        );
        let sig_pos = body.find("-- \nRegards").expect("signature");
        let quote_pos = body.find("\n---\n").expect("quote");
        assert!(sig_pos < quote_pos);
    }

    #[test]
    fn toggle_off_strips_without_eating_quote_dashes() {
        let input = "Line\n-- \nSig\n---\n> quote";
        let out = apply_signature(input, ComposeFormat::Plain, None, false);
        assert_eq!(out, "Line\n---\n> quote");
    }

    #[test]
    fn html_stored_plain_to_html_target() {
        let rendered = render_signature_for_format(
            &sig("Alice\nExample", ComposeFormat::Plain),
            ComposeFormat::Html,
        );
        assert!(!rendered.contains("alcedo-signature"));
        assert!(rendered.contains("Alice"));

        let body = apply_signature(
            "<p>Hi</p>",
            ComposeFormat::Html,
            Some(&sig("Alice\nExample", ComposeFormat::Plain)),
            true,
        );
        assert_eq!(body.matches("alcedo-signature").count(), 1);

        let toggled_off = apply_signature(&body, ComposeFormat::Html, None, false);
        assert!(!toggled_off.contains("alcedo-signature"));
        assert!(toggled_off.contains("Hi"));
    }

    #[test]
    fn html_strip_signature_with_inner_div() {
        let sig_html = "<div>inner</div>";
        let with_sig = apply_signature(
            "<p>User draft</p><p>────────────────────────</p><p>quote</p>",
            ComposeFormat::Html,
            Some(&sig(sig_html, ComposeFormat::Html)),
            true,
        );
        let stripped = apply_signature(&with_sig, ComposeFormat::Html, None, false);
        assert!(!stripped.contains("alcedo-signature"));
        assert!(!stripped.contains("inner"));
        assert!(stripped.contains("User draft"));
        assert!(stripped.contains("────────────────────────"));
    }
}
