//! Keep URL credentials out of text that users or logs can see.

/// Remove the `user:password@` part of every URL in `text`.
pub fn redact_url_userinfo(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    loop {
        let Some(pos) = rest.find("://") else {
            out.push_str(rest);
            break;
        };
        out.push_str(&rest[..pos + 3]);
        let after = &rest[pos + 3..];
        let end = after
            .find(|c: char| c.is_whitespace())
            .unwrap_or(after.len());
        let segment = &after[..end];
        // Credentials end at the last `@` before the path, query or fragment.
        let authority_end = segment.find(['/', '?', '#']).unwrap_or(segment.len());
        let host_start = segment[..authority_end].rfind('@').map_or(0, |at| at + 1);
        out.push_str(&segment[host_start..]);
        rest = &after[end..];
        if rest.is_empty() {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::redact_url_userinfo;

    #[test]
    fn strips_credentials_from_a_url_in_a_message() {
        assert_eq!(
            redact_url_userinfo("error for url (https://user:token@cal.example/feed.ics): timeout"),
            "error for url (https://cal.example/feed.ics): timeout"
        );
    }

    #[test]
    fn strips_credentials_from_every_url() {
        assert_eq!(
            redact_url_userinfo("https://a:1@one.example then imaps://b:2@two.example/INBOX"),
            "https://one.example then imaps://two.example/INBOX"
        );
    }

    #[test]
    fn leaves_text_without_credentials_unchanged() {
        for text in [
            "",
            "connection refused",
            "write to me@example.com",
            "https://cal.example/feed.ics",
        ] {
            assert_eq!(redact_url_userinfo(text), text);
        }
    }

    #[test]
    fn strips_a_password_that_contains_an_at_sign() {
        assert_eq!(
            redact_url_userinfo("https://user:p@ss@cal.example/feed.ics"),
            "https://cal.example/feed.ics"
        );
    }

    #[test]
    fn keeps_an_at_sign_in_the_path_or_query() {
        for url in [
            "https://cal.example/users/me@example.com/feed.ics",
            "https://cal.example/feed.ics?owner=me@example.com",
            "https://cal.example#me@example.com",
        ] {
            assert_eq!(redact_url_userinfo(url), url);
        }
    }
}
