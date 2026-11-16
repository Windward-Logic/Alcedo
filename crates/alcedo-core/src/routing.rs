//! Reply-routing headers beyond From/To/Cc/Reply-To: Mail-Reply-To,
//! Mail-Followup-To (draft-ietf-drums-mail-followup-to), RFC 2369 List-Post,
//! RFC 2919 List-Id, and the delivery headers that name the address a message
//! actually reached (X-Original-To, Delivered-To).

use serde::{Deserialize, Serialize};

use crate::envelope::Address;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RoutingHeaders {
    #[serde(default)]
    pub mail_reply_to: Vec<Address>,
    #[serde(default)]
    pub mail_followup_to: Vec<Address>,
    /// Posting address from `List-Post: <mailto:…>`. `None` for `NO` or no header.
    #[serde(default)]
    pub list_post: Option<String>,
    /// The `<list.id>` part of `List-Id`, without brackets.
    #[serde(default)]
    pub list_id: Option<String>,
    /// Addresses the message was delivered to: X-Original-To values first,
    /// then Delivered-To. On a catch-all these name the alias that was used.
    #[serde(default)]
    pub delivered_to: Vec<String>,
}

static NO_ROUTING_HEADERS: RoutingHeaders = RoutingHeaders {
    mail_reply_to: Vec::new(),
    mail_followup_to: Vec::new(),
    list_post: None,
    list_id: None,
    delivered_to: Vec::new(),
};

impl RoutingHeaders {
    /// Shared empty value for envelopes whose headers were never fetched.
    pub fn none() -> &'static RoutingHeaders {
        &NO_ROUTING_HEADERS
    }
}

/// Posting address from a raw `List-Post` value (`<mailto:list@host?subject=x>`).
pub fn parse_list_post(raw: &str) -> Option<String> {
    raw.split('<')
        .skip(1)
        .filter_map(|part| part.split_once('>').map(|(uri, _)| uri.trim()))
        .find_map(|uri| {
            let rest = uri
                .get(..7)?
                .eq_ignore_ascii_case("mailto:")
                .then(|| &uri[7..])?;
            let address = rest.split('?').next().unwrap_or("").trim();
            (address.contains('@') && !address.contains(char::is_whitespace))
                .then(|| address.to_string())
        })
}

/// The `list.id` token from a raw `List-Id` value (`Name <list.id.host>`).
pub fn parse_list_id(raw: &str) -> Option<String> {
    let raw = raw.trim();
    let id = match (raw.rfind('<'), raw.rfind('>')) {
        (Some(open), Some(close)) if open < close => &raw[open + 1..close],
        _ => raw,
    };
    let id = id.trim();
    (!id.is_empty() && !id.contains(char::is_whitespace)).then(|| id.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_post_extracts_mailto_address() {
        assert_eq!(
            parse_list_post("<mailto:dev@lists.example.org>").as_deref(),
            Some("dev@lists.example.org")
        );
        assert_eq!(
            parse_list_post("<https://x.org/post>, <MAILTO:dev@x.org?subject=hi>").as_deref(),
            Some("dev@x.org")
        );
    }

    #[test]
    fn list_post_no_or_garbage_is_none() {
        assert_eq!(parse_list_post("NO"), None);
        assert_eq!(parse_list_post("NO (posting not allowed)"), None);
        assert_eq!(parse_list_post("<https://x.org/post>"), None);
        assert_eq!(parse_list_post(""), None);
    }

    #[test]
    fn list_id_takes_bracketed_token() {
        assert_eq!(
            parse_list_id("Rust Users <Rust-Users.lists.example.org>").as_deref(),
            Some("rust-users.lists.example.org")
        );
        assert_eq!(
            parse_list_id("plain.list.id").as_deref(),
            Some("plain.list.id")
        );
        assert_eq!(parse_list_id("  "), None);
    }

    #[test]
    fn routing_headers_serde_defaults() {
        let parsed: RoutingHeaders = serde_json::from_str("{}").expect("parse");
        assert_eq!(&parsed, RoutingHeaders::none());
    }
}
