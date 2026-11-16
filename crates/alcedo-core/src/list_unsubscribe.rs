//! RFC 2369 / RFC 8058 `List-Unsubscribe` parsing.

use serde::{Deserialize, Serialize};

use crate::envelope::parse_address_token;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListUnsubscribe {
    pub https: Option<String>,
    pub mailto: Option<MailtoUnsubscribe>,
    pub one_click: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MailtoUnsubscribe {
    pub address: String,
    pub subject: Option<String>,
    pub body: Option<String>,
}

impl ListUnsubscribe {
    pub fn has_action(&self) -> bool {
        self.one_click || self.mailto.is_some() || self.https.is_some()
    }
}

/// Parse `List-Unsubscribe` and optional `List-Unsubscribe-Post` header values.
pub fn parse_list_unsubscribe(
    list_unsubscribe: Option<&str>,
    list_unsubscribe_post: Option<&str>,
) -> Option<ListUnsubscribe> {
    let raw = list_unsubscribe?;
    let normalized = unfold_header_value(raw);
    if normalized.trim().is_empty() {
        return None;
    }

    let mut https = None;
    let mut mailto = None;

    for token in angle_bracket_tokens(&normalized) {
        let Some(scheme_end) = token.find(':') else {
            continue;
        };
        let scheme = &token[..scheme_end];
        let rest = &token[scheme_end + 1..];

        if scheme.eq_ignore_ascii_case("https") {
            if https.is_none() && !rest.is_empty() {
                let uri = format!("https:{rest}");
                if let Some(host) = https_uri_host(&uri)
                    && !https_host_is_blocked(&host)
                {
                    https = Some(uri);
                }
            }
        } else if scheme.eq_ignore_ascii_case("mailto") && mailto.is_none() {
            mailto = parse_mailto_unsubscribe(rest);
        }
    }

    let post_one_click = list_unsubscribe_post
        .map(post_header_is_one_click)
        .unwrap_or(false);
    let one_click = post_one_click
        && https
            .as_ref()
            .and_then(|uri| https_uri_host(uri))
            .is_some_and(|host| !https_host_is_blocked(&host));

    let parsed = ListUnsubscribe {
        https,
        mailto,
        one_click,
    };
    if parsed.has_action() {
        Some(parsed)
    } else {
        None
    }
}

fn unfold_header_value(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\r' {
            if chars.peek() == Some(&'\n') {
                chars.next();
            }
            if matches!(chars.peek(), Some(' ' | '\t')) {
                while matches!(chars.peek(), Some(' ' | '\t')) {
                    chars.next();
                }
                continue;
            }
            out.push(' ');
            continue;
        }
        if ch == '\n' && matches!(chars.peek(), Some(' ' | '\t')) {
            while matches!(chars.peek(), Some(' ' | '\t')) {
                chars.next();
            }
            continue;
        }
        out.push(ch);
    }
    out
}

fn angle_bracket_tokens(value: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let bytes = value.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'<' {
            let start = i + 1;
            if let Some(rel_end) = value[start..].find('>') {
                let end = start + rel_end;
                let token = value[start..end].trim();
                if !token.is_empty() {
                    tokens.push(token.to_string());
                }
                i = end + 1;
                continue;
            }
        }
        i += 1;
    }
    tokens
}

fn post_header_is_one_click(value: &str) -> bool {
    let mut trimmed = value.trim();
    if trimmed.len() >= 2 {
        let bytes = trimmed.as_bytes();
        if (bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[bytes.len() - 1] == b'\'')
        {
            trimmed = &trimmed[1..trimmed.len() - 1];
        }
    }
    trimmed.eq_ignore_ascii_case("list-unsubscribe=one-click")
}

fn parse_mailto_unsubscribe(rest: &str) -> Option<MailtoUnsubscribe> {
    let (path, query) = match rest.split_once('?') {
        Some((path, query)) => (path, Some(query)),
        None => (rest, None),
    };
    let address = parse_address_token(path)?.email;
    let mut subject = None;
    let mut body = None;
    if let Some(query) = query {
        for pair in query.split('&') {
            let Some((key, value)) = pair.split_once('=') else {
                continue;
            };
            if key.eq_ignore_ascii_case("subject") && subject.is_none() {
                subject = Some(percent_decode_utf8(value));
            } else if key.eq_ignore_ascii_case("body") && body.is_none() {
                body = Some(percent_decode_utf8(value));
            }
        }
    }
    Some(MailtoUnsubscribe {
        address,
        subject,
        body,
    })
}

fn percent_decode_utf8(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(byte) = u8::from_str_radix(&input[i + 1..i + 3], 16)
        {
            out.push(byte);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Extract the host from an `https:` URI, rejecting userinfo.
pub fn https_uri_host(uri: &str) -> Option<String> {
    let rest = uri
        .strip_prefix("https:")
        .or_else(|| uri.strip_prefix("HTTPS:"))?;
    let authority = rest.strip_prefix("//")?;
    let host_port = authority.split(&['/', '?', '#'][..]).next()?;
    if host_port.contains('@') {
        return None;
    }
    let host = host_port
        .rsplit_once(':')
        .filter(|(h, port)| !h.is_empty() && port.chars().all(|c| c.is_ascii_digit()))
        .map(|(h, _)| h)
        .unwrap_or(host_port);
    if host.is_empty() {
        None
    } else {
        Some(host.to_string())
    }
}

/// Literal host / name rules for unsubscribe targets (DNS checks are sync-side).
pub fn https_host_is_blocked(host: &str) -> bool {
    let host = host.trim().trim_end_matches('.');
    if host.is_empty() {
        return true;
    }
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    let lower = host.to_ascii_lowercase();
    if lower.ends_with(".localhost") || lower.ends_with(".local") || lower.ends_with(".onion") {
        return true;
    }
    if let Ok(addr) = lower.parse::<std::net::IpAddr>() {
        return ip_addr_is_blocked(&addr);
    }
    if host.starts_with('[') && host.ends_with(']') {
        let inner = &host[1..host.len() - 1];
        if let Ok(addr) = inner.parse::<std::net::IpAddr>() {
            return ip_addr_is_blocked(&addr);
        }
    }
    false
}

pub fn ip_addr_is_blocked(addr: &std::net::IpAddr) -> bool {
    match addr {
        std::net::IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_documentation()
                || v4.octets()[0] == 0
        }
        std::net::IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return ip_addr_is_blocked(&std::net::IpAddr::V4(v4));
            }
            v6.is_loopback()
                || v6.is_unspecified()
                || is_ipv6_ula(v6)
                || v6.is_unique_local()
                || is_ipv6_link_local(v6)
        }
    }
}

fn is_ipv6_ula(addr: &std::net::Ipv6Addr) -> bool {
    (addr.segments()[0] & 0xfe00) == 0xfc00
}

fn is_ipv6_link_local(addr: &std::net::Ipv6Addr) -> bool {
    (addr.segments()[0] & 0xffc0) == 0xfe80
}

#[cfg(test)]
mod tests {
    use super::*;

    const RFC_EXAMPLE: &str = "<https://example.com/unsubscribe>, <mailto:unsub@example.com>";

    #[test]
    fn rfc_example_with_post_is_one_click() {
        let parsed = parse_list_unsubscribe(Some(RFC_EXAMPLE), Some("List-Unsubscribe=One-Click"))
            .expect("parse");
        assert!(parsed.one_click);
        assert_eq!(
            parsed.https.as_deref(),
            Some("https://example.com/unsubscribe")
        );
        assert_eq!(parsed.mailto.as_ref().unwrap().address, "unsub@example.com");
    }

    #[test]
    fn https_without_post_still_has_action() {
        let parsed = parse_list_unsubscribe(Some(RFC_EXAMPLE), None).expect("parse");
        assert!(!parsed.one_click);
        assert!(parsed.https.is_some());
        assert!(parsed.has_action());
    }

    #[test]
    fn mailto_only() {
        let parsed =
            parse_list_unsubscribe(Some("<mailto:list@example.com>"), None).expect("parse");
        assert!(!parsed.one_click);
        assert!(parsed.https.is_none());
        assert_eq!(parsed.mailto.as_ref().unwrap().address, "list@example.com");
    }

    #[test]
    fn post_without_https_is_not_one_click() {
        let parsed = parse_list_unsubscribe(
            Some("<mailto:list@example.com>"),
            Some("List-Unsubscribe=One-Click"),
        )
        .expect("parse");
        assert!(!parsed.one_click);
        assert!(parsed.mailto.is_some());
    }

    #[test]
    fn empty_or_missing_tokens_returns_none() {
        assert!(parse_list_unsubscribe(None, None).is_none());
        assert!(parse_list_unsubscribe(Some(""), None).is_none());
        assert!(parse_list_unsubscribe(Some("no angle brackets"), None).is_none());
    }

    #[test]
    fn http_only_returns_none() {
        assert!(parse_list_unsubscribe(Some("<http://example.com/unsub>"), None).is_none());
    }

    #[test]
    fn javascript_scheme_ignored() {
        assert!(parse_list_unsubscribe(Some("<javascript:alert(1)>"), None).is_none());
    }

    #[test]
    fn folded_whitespace_inside_header() {
        let folded = "<https://example.com/unsub>\r\n , <mailto:a@b.com>";
        let parsed = parse_list_unsubscribe(Some(folded), None).expect("parse");
        assert_eq!(parsed.https.as_deref(), Some("https://example.com/unsub"));
        assert_eq!(parsed.mailto.as_ref().unwrap().address, "a@b.com");
    }

    #[test]
    fn insider_one_click_folded_https_and_mailto() {
        let list = concat!(
            "<https://mail.useinsider.com/user/v1/scu/casetifyhk/email/1054/id>,\r\n",
            " <mailto:unsubscribe@useinsider.com?subject=unsubscribe_from||abc>"
        );
        let parsed =
            parse_list_unsubscribe(Some(list), Some("List-Unsubscribe=One-Click")).expect("parse");
        assert!(parsed.one_click);
        assert_eq!(
            parsed.https.as_deref(),
            Some("https://mail.useinsider.com/user/v1/scu/casetifyhk/email/1054/id")
        );
        assert_eq!(
            parsed.mailto.as_ref().unwrap().address,
            "unsubscribe@useinsider.com"
        );
    }

    #[test]
    fn mailto_subject_and_body_decoded() {
        let parsed = parse_list_unsubscribe(
            Some("<mailto:list@example.com?subject=unsubscribe&body=please%20remove>"),
            None,
        )
        .expect("parse");
        let mailto = parsed.mailto.expect("mailto");
        assert_eq!(mailto.subject.as_deref(), Some("unsubscribe"));
        assert_eq!(mailto.body.as_deref(), Some("please remove"));
    }

    #[test]
    fn blocked_loopback_https_dropped() {
        assert!(
            parse_list_unsubscribe(
                Some("<https://127.0.0.1/x>"),
                Some("List-Unsubscribe=One-Click"),
            )
            .is_none()
        );
    }

    #[test]
    fn blocked_https_with_mailto_fallback() {
        let parsed = parse_list_unsubscribe(
            Some("<https://127.0.0.1/x>, <mailto:list@example.com>"),
            Some("List-Unsubscribe=One-Click"),
        )
        .expect("parse");
        assert!(!parsed.one_click);
        assert!(parsed.https.is_none());
        assert!(parsed.mailto.is_some());
    }

    #[test]
    fn post_header_case_insensitive_with_quotes() {
        assert!(post_header_is_one_click("\"LIST-unsubscribe=ONE-CLICK\""));
    }

    #[test]
    fn colonless_token_skipped_valid_https_parsed() {
        let parsed = parse_list_unsubscribe(Some("<not-a-uri>, <https://example.com/unsub>"), None)
            .expect("parse");
        assert_eq!(parsed.https.as_deref(), Some("https://example.com/unsub"));
    }

    #[test]
    fn ipv4_mapped_loopback_and_private_blocked() {
        assert!(ip_addr_is_blocked(
            &"::ffff:127.0.0.1".parse().expect("addr")
        ));
        assert!(ip_addr_is_blocked(
            &"::ffff:10.0.0.1".parse().expect("addr")
        ));
        assert!(!ip_addr_is_blocked(
            &"::ffff:8.8.8.8".parse().expect("addr")
        ));
    }
}
