//! Outbound tracking pixel injection (opt-in only).

/// Append a 1×1 tracking image to outbound HTML or plaintext bodies.
pub fn inject_tracking_pixel(html_or_text: &str, pixel_url: &str) -> String {
    let pixel = format!(
        r#"<img src="{}" width="1" height="1" alt="">"#,
        escape_attr(pixel_url)
    );

    if looks_like_html(html_or_text) {
        inject_into_html(html_or_text, &pixel)
    } else {
        format!(
            "<div>{text}</div>{pixel}",
            text = ammonia::clean_text(html_or_text)
        )
    }
}

fn escape_attr(value: &str) -> String {
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

fn looks_like_html(input: &str) -> bool {
    crate::sanitize::looks_like_html(input)
}

fn inject_into_html(html: &str, pixel: &str) -> String {
    if let Some(idx) = html.to_ascii_lowercase().rfind("</body>") {
        let mut out = String::with_capacity(html.len() + pixel.len());
        out.push_str(&html[..idx]);
        out.push_str(pixel);
        out.push_str(&html[idx..]);
        return out;
    }

    if let Some(idx) = html.to_ascii_lowercase().rfind("</html>") {
        let mut out = String::with_capacity(html.len() + pixel.len());
        out.push_str(&html[..idx]);
        out.push_str(pixel);
        out.push_str(&html[idx..]);
        return out;
    }

    let mut out = html.to_string();
    out.push_str(pixel);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_pixel_to_plaintext() {
        let result = inject_tracking_pixel("Hello", "https://example.com/p.gif");
        assert!(result.contains("<div>Hello</div>"));
        assert!(result.contains(r#"width="1" height="1""#));
        assert!(result.contains(r#"src="https://example.com/p.gif""#));
    }

    #[test]
    fn appends_pixel_before_body_close() {
        let html = "<html><body><p>Hi</p></body></html>";
        let result = inject_tracking_pixel(html, "https://example.com/p.gif");
        assert!(result.contains("<p>Hi</p><img"));
        assert!(result.contains("</body></html>"));
    }
}
