//! Rewrite `cid:` references to the internal `alcedo-cid://` scheme.

use std::collections::HashMap;

/// Rewrite `cid:` URLs using `map` (`cid` value → attachment SHA-256 hex).
pub fn rewrite_cids(html: &str, map: &HashMap<String, String>) -> String {
    if map.is_empty() {
        return html.to_string();
    }

    let mut out = String::with_capacity(html.len());
    let lower = html.to_ascii_lowercase();
    let mut cursor = 0;

    while let Some(rel) = lower[cursor..].find("cid:") {
        let start = cursor + rel;
        out.push_str(&html[cursor..start]);

        let value_start = start + 4;
        let value_end = cid_value_end(&html[value_start..]);
        let cid_value = &html[value_start..value_start + value_end];
        let lookup_key = cid_map_key(cid_value);

        if let Some(sha256) = map.get(&lookup_key).or_else(|| map.get(cid_value)) {
            out.push_str("alcedo-cid://");
            out.push_str(sha256);
        } else {
            out.push_str("cid:");
            out.push_str(cid_value);
        }

        cursor = value_start + value_end;
    }

    out.push_str(&html[cursor..]);
    out
}

fn cid_value_end(rest: &str) -> usize {
    rest.char_indices()
        .find_map(|(idx, ch)| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '@' | '+' | '%') {
                None
            } else {
                Some(idx)
            }
        })
        .unwrap_or(rest.len())
}

/// Map key for a Content-ID: trim and drop one layer of angle brackets.
pub fn cid_map_key(cid_value: &str) -> String {
    cid_value
        .trim()
        .trim_start_matches('<')
        .trim_end_matches('>')
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrites_cid_src() {
        let mut map = HashMap::new();
        map.insert("logo@example.com".to_string(), "abc123".to_string());

        let html = r#"<img src="cid:logo@example.com" alt="logo">"#;
        let rewritten = rewrite_cids(html, &map);
        assert!(rewritten.contains(r#"src="alcedo-cid://abc123""#));
        assert!(!rewritten.contains("cid:logo@example.com"));
    }

    #[test]
    fn rewrites_outlook_image_filename_cid() {
        let mut map = HashMap::new();
        let cid = "image002.png@01DD652A.A3420E70";
        map.insert(cid.to_string(), "abc123".to_string());

        let html = r#"<img width="569" height="275" src="cid:image002.png@01DD652A.A3420E70">"#;
        let rewritten = rewrite_cids(html, &map);
        assert!(
            rewritten.contains(r#"src="alcedo-cid://abc123""#),
            "{rewritten}"
        );
        assert!(!rewritten.contains("cid:image002.png"));
    }
}
