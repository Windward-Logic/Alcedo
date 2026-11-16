use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::{AccountId, FolderId, MessageId, ThreadId};
use crate::routing::RoutingHeaders;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Address {
    pub name: Option<String>,
    pub email: String,
}

impl Address {
    /// Display name with IMAP/RFC 5322 quoting cleaned up, falling back to the email.
    pub fn display_name(&self) -> String {
        self.name
            .as_deref()
            .map(clean_display_name)
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| self.email.clone())
    }

    /// `Name <email>` when a name is present, otherwise just the email.
    pub fn formatted(&self) -> String {
        format_mailbox(self.name.as_deref(), &self.email)
    }

    /// Email shown on a name-only chip tooltip. `None` when the chip already shows the email.
    pub fn chip_tooltip(&self) -> Option<String> {
        let label = self.display_name();
        if label.eq_ignore_ascii_case(&self.email) {
            None
        } else {
            Some(self.email.clone())
        }
    }
}

/// Flags the server reports for one UID, without the rest of the envelope.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ServerFlags {
    pub uid: u32,
    pub seen: bool,
    pub flagged: bool,
    pub answered: bool,
    pub draft: bool,
    /// `\Deleted`: waiting to be expunged, so the row should leave the list.
    pub deleted: bool,
}

/// Parse a single address token (`Name <email>` or bare email).
pub fn parse_address_token(input: &str) -> Option<Address> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }

    let (name, email) = if let Some(lt) = trimmed.rfind('<') {
        let gt = trimmed[lt..].find('>')?;
        let email = trimmed[lt + 1..lt + gt].trim();
        let name_part = trimmed[..lt].trim().trim_end_matches(',').trim();
        let name = if name_part.is_empty() {
            None
        } else {
            Some(clean_display_name(name_part))
        };
        (name, email)
    } else {
        (None, trimmed)
    };

    if !email_is_valid(email) {
        return None;
    }

    Some(Address {
        name,
        email: email.to_ascii_lowercase(),
    })
}

fn email_is_valid(email: &str) -> bool {
    let Some((local, domain)) = email.split_once('@') else {
        return false;
    };
    !local.is_empty() && !domain.is_empty()
}

/// RFC 5322 mailbox string: `Name <email>` or bare email, quoting the display name when needed.
pub fn format_mailbox(name: Option<&str>, email: &str) -> String {
    let email = email.trim();
    match name.map(clean_display_name).filter(|name| !name.is_empty()) {
        Some(name) if display_name_needs_quoting(&name) => {
            let escaped = name.replace('\\', "\\\\").replace('"', "\\\"");
            format!("\"{escaped}\" <{email}>")
        }
        Some(name) => format!("{name} <{email}>"),
        None => email.to_string(),
    }
}

fn display_name_needs_quoting(name: &str) -> bool {
    if name.starts_with(' ') || name.ends_with(' ') {
        return true;
    }
    name.chars().any(|ch| {
        ch.is_control()
            || matches!(
                ch,
                '(' | ')' | '<' | '>' | '[' | ']' | ':' | ';' | '@' | '\\' | ',' | '.' | '"' | '#'
            )
    })
}

/// Strip IMAP quoted-string backslashes and surrounding quotes from a display name.
///
/// IMAP ENVELOPE names are often the raw RFC 5322 quoted-string. Some parsers
/// (including imap-proto's nom `escaped`) leave `\"` in the value, so the UI
/// would otherwise show `\"Vanshika Johar\"`. RFC 2047 encoded-words are decoded
/// first so cached envelopes still render readable names.
pub fn clean_display_name(name: &str) -> String {
    let decoded = crate::mime::decode_mime_header(name);
    let unescaped = unescape_quoted_pairs(decoded.trim());
    strip_surrounding_quotes(&unescaped).to_string()
}

fn unescape_quoted_pairs(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            if let Some(next) = chars.next() {
                out.push(next);
            }
        } else {
            out.push(ch);
        }
    }
    out
}

fn strip_surrounding_quotes(input: &str) -> &str {
    let mut value = input.trim();
    while value.len() >= 2 {
        let bytes = value.as_bytes();
        let quote = bytes[0];
        if (quote == b'"' || quote == b'\'') && bytes[bytes.len() - 1] == quote {
            value = value[1..value.len() - 1].trim();
        } else {
            break;
        }
    }
    value
}

fn clean_display_subject(subject: &str) -> String {
    crate::mime::decode_mime_header(subject).replace("\\\"", "\"")
}

/// Maximum Unicode scalars shown in the message-list preview.
pub const PREVIEW_SNIPPET_MAX: usize = 140;

/// Maximum Unicode scalars stored from an IMAP TEXT peek. Matches the reader
/// OTP scan window so verification codes past the list preview still extract.
pub const PREVIEW_SCAN_MAX: usize = 8000;

/// One-line list preview: decode quoted-printable, strip HTML, collapse
/// whitespace, then truncate. IMAP `BODY[TEXT]` peeks for HTML-only mail are
/// often raw markup (sometimes still QP-encoded) because the peek has no
/// `Content-Type` / `Content-Transfer-Encoding` headers.
pub fn preview_snippet(input: &str) -> String {
    truncate_preview_chars(&clean_preview_text(input), PREVIEW_SNIPPET_MAX)
}

/// Cleaned peek text kept on `Envelope.snippet` for search and OTP notify.
pub fn preview_scan(input: &str) -> String {
    truncate_preview_chars(&clean_preview_text(input), PREVIEW_SCAN_MAX)
}

/// Collapse whitespace and drop leftover control characters so list previews
/// stay on one readable line. GTK otherwise paints CR as a `[00 0D]` hex box.
/// HTML-only peeks are converted to plaintext first so the list never shows
/// markup like `<!DOCTYPE html`.
pub fn clean_preview_text(input: &str) -> String {
    let without_mime = preview_without_embedded_mime(input);
    let decoded = maybe_decode_quoted_printable(without_mime);
    let text = if preview_looks_like_html(&decoded) {
        mail_parser::decoders::html::html_to_text(&decoded)
    } else {
        decoded
    };
    collapse_preview_whitespace(&text)
}

/// Drop a trailing MIME part from an IMAP `BODY[TEXT]` peek (or a stored scan).
///
/// Envelope FETCH peeks the body without the top-level `Content-Type`, so
/// multipart mail is often plaintext plus `--boundary` and a `text/html` part.
/// The reader uses this scan as a loading placeholder; leaving the dump in
/// place looks like unparsed source until `OpenMessage` finishes.
pub fn preview_without_embedded_mime(input: &str) -> &str {
    let mut from = 0;
    while let Some(rel) = input[from..].find("--") {
        let abs = from + rel;
        if looks_like_mime_part_start(&input[abs..]) {
            return input[..abs].trim_end();
        }
        from = abs + 2;
    }
    input
}

fn looks_like_mime_part_start(at_dashes: &str) -> bool {
    let Some(rest) = at_dashes.strip_prefix("--") else {
        return false;
    };
    if rest.starts_with([' ', '\t']) {
        return false;
    }
    let mut token = 0usize;
    let mut end = 0usize;
    for (i, ch) in rest.char_indices() {
        if is_mime_boundary_char(ch) {
            token += 1;
            end = i + ch.len_utf8();
            if token > 70 {
                return false;
            }
        } else {
            break;
        }
    }
    if token == 0 {
        return false;
    }
    let after = rest[end..].trim_start_matches([' ', '\t', '\r', '\n']);
    after.len() >= 13 && after.as_bytes()[..13].eq_ignore_ascii_case(b"content-type:")
}

fn is_mime_boundary_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric()
        || matches!(
            ch,
            '\'' | '(' | ')' | '+' | '_' | ',' | '-' | '.' | '/' | ':' | '=' | '?'
        )
}

fn collapse_preview_whitespace(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut prev_space = false;
    for ch in input.chars() {
        if ch.is_whitespace() {
            if !prev_space {
                out.push(' ');
                prev_space = true;
            }
        } else if !ch.is_control() {
            out.push(ch);
            prev_space = false;
        }
    }
    out.trim().to_string()
}

fn maybe_decode_quoted_printable(input: &str) -> String {
    if !looks_like_quoted_printable(input) {
        return input.to_string();
    }
    match mail_parser::decoders::quoted_printable::quoted_printable_decode(input.as_bytes()) {
        Some(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        None => input.to_string(),
    }
}

fn looks_like_quoted_printable(input: &str) -> bool {
    if input.contains("=\r\n") || input.contains("=\n") {
        return true;
    }
    let bytes = input.as_bytes();
    let mut count = 0usize;
    let mut i = 0usize;
    while i + 2 < bytes.len() {
        if bytes[i] == b'=' && bytes[i + 1].is_ascii_hexdigit() && bytes[i + 2].is_ascii_hexdigit()
        {
            count += 1;
            if count >= 3 {
                return true;
            }
            i += 3;
        } else {
            i += 1;
        }
    }
    false
}

/// Newsletter and HTML-only peeks usually start with a comment, doctype, or tag.
/// Rejects plaintext like `<3 thanks` and `<https://example.com>`.
fn preview_looks_like_html(input: &str) -> bool {
    let trimmed = input.trim_start().trim_start_matches('\u{feff}');
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

/// True when `input` starts with `<tag`, `</tag`, or `<tag ` / `<tag>`.
/// Rejects plaintext autolinks like `<https://example.com>` and emoticons like `<3`.
pub fn html_tag_prefix(input: &str) -> bool {
    let Some(after_lt) = input.strip_prefix('<') else {
        return false;
    };
    let after_lt = after_lt.strip_prefix('/').unwrap_or(after_lt);
    let name_len = after_lt
        .chars()
        .take_while(|c| c.is_ascii_alphabetic())
        .count();
    if name_len == 0 {
        return false;
    }
    match after_lt[name_len..].chars().next() {
        None | Some('>') | Some('/') => true,
        Some(c) if c.is_ascii_whitespace() => true,
        _ => false,
    }
}

/// True when any `<` in `input` starts an HTML tag (see [`html_tag_prefix`]).
pub fn contains_html_tag(input: &str) -> bool {
    let mut rest = input;
    while let Some(idx) = rest.find('<') {
        if html_tag_prefix(&rest[idx..]) {
            return true;
        }
        rest = &rest[idx + 1..];
    }
    false
}

fn truncate_preview_chars(input: &str, max_chars: usize) -> String {
    if input.chars().count() <= max_chars {
        return input.to_string();
    }
    let mut out = String::new();
    for ch in input.chars().take(max_chars.saturating_sub(3)) {
        out.push(ch);
    }
    out.push_str("...");
    out
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    pub id: MessageId,
    pub account_id: AccountId,
    pub folder_id: FolderId,
    pub uid: u32,
    pub thread_id: ThreadId,
    pub date: DateTime<Utc>,
    pub subject: String,
    pub from: Vec<Address>,
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    #[serde(default)]
    pub reply_to: Vec<Address>,
    #[serde(default)]
    pub sender: Vec<Address>,
    /// Mail-Reply-To, Mail-Followup-To, List-Post, List-Id, and delivery
    /// addresses. `None` until the headers have been fetched.
    #[serde(default)]
    pub routing: Option<RoutingHeaders>,
    pub snippet: String,
    pub seen: bool,
    pub flagged: bool,
    pub answered: bool,
    pub draft: bool,
    pub has_attachments: bool,
    /// Whole message in bytes, including attachments.
    /// Server RFC822.SIZE when known; otherwise the sum of saved attachment sizes.
    /// 0 means the size is not known yet.
    #[serde(default)]
    pub size_bytes: i64,
    #[serde(default)]
    pub deleted_at: Option<DateTime<Utc>>,
    pub account_accent: String,
    pub account_avatar: String,
}

impl Envelope {
    /// Subject with RFC 2047 encoded-words and escaped quotes decoded for display.
    pub fn display_subject(&self) -> String {
        clean_display_subject(&self.subject)
    }

    /// Routing headers, or the empty set when they have not been fetched.
    pub fn routing_headers(&self) -> &RoutingHeaders {
        self.routing.as_ref().unwrap_or(RoutingHeaders::none())
    }

    /// One-line body preview with CR/LF, HTML markup, and other controls stripped.
    pub fn display_snippet(&self) -> String {
        truncate_preview_chars(&clean_preview_text(&self.snippet), PREVIEW_SNIPPET_MAX)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum MessageListMode {
    #[default]
    Conversation,
    Flat,
}

impl MessageListMode {
    pub fn from_gsettings(value: &str) -> Self {
        match value {
            "flat" => Self::Flat,
            _ => Self::Conversation,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Conversation => "conversation",
            Self::Flat => "flat",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Conversation {
    pub thread_id: ThreadId,
    pub latest: Envelope,
    /// In-scope thread members, oldest first. `latest` is `members.last()`.
    pub members: Vec<Envelope>,
    pub message_count: u32,
    pub unread_count: u32,
    pub any_flagged: bool,
    pub any_attachments: bool,
    pub member_ids: Vec<MessageId>,
    pub member_locations: Vec<(AccountId, FolderId)>,
    pub participants: Vec<Address>,
}

/// Group flat envelopes into conversations (newest thread first).
pub fn group_envelopes(envelopes: &[Envelope]) -> Vec<Conversation> {
    use std::collections::HashMap;

    let mut by_thread: HashMap<ThreadId, Vec<&Envelope>> = HashMap::new();
    for env in envelopes {
        by_thread.entry(env.thread_id).or_default().push(env);
    }

    let mut conversations: Vec<Conversation> = by_thread
        .into_iter()
        .map(|(thread_id, members)| build_conversation(thread_id, &members))
        .collect();

    conversations.sort_by(|a, b| {
        b.latest
            .date
            .cmp(&a.latest.date)
            .then_with(|| b.latest.id.0.cmp(&a.latest.id.0))
    });
    conversations
}

fn build_conversation(thread_id: ThreadId, members: &[&Envelope]) -> Conversation {
    let mut sorted: Vec<&Envelope> = members.to_vec();
    sorted.sort_by(|a, b| a.date.cmp(&b.date).then_with(|| a.id.0.cmp(&b.id.0)));
    sorted.dedup_by_key(|e| e.id);
    let owned: Vec<Envelope> = sorted.iter().map(|e| (*e).clone()).collect();
    let latest = owned
        .last()
        .expect("conversation has at least one member")
        .clone();
    let member_ids: Vec<MessageId> = owned.iter().map(|e| e.id).collect();
    let member_locations: Vec<(AccountId, FolderId)> =
        owned.iter().map(|e| (e.account_id, e.folder_id)).collect();
    let message_count = owned.len() as u32;
    let unread_count = owned.iter().filter(|e| !e.seen).count() as u32;
    let any_flagged = owned.iter().any(|e| e.flagged);
    let any_attachments = owned.iter().any(|e| e.has_attachments);
    let participants = collect_participants(&sorted);

    Conversation {
        thread_id,
        latest,
        members: owned,
        message_count,
        unread_count,
        any_flagged,
        any_attachments,
        member_ids,
        member_locations,
        participants,
    }
}

fn collect_participants(members: &[&Envelope]) -> Vec<Address> {
    let mut seen = std::collections::HashSet::new();
    let mut participants = Vec::new();
    for env in members {
        for addr in &env.from {
            if seen.insert(addr.email.clone()) {
                participants.push(addr.clone());
            }
        }
    }
    participants
}

/// Human-readable participant list for conversation rows (`max` visible names).
pub fn participant_summary(participants: &[Address], max: usize) -> String {
    if participants.is_empty() {
        return "Unknown".to_string();
    }
    let names: Vec<String> = participants
        .iter()
        .take(max)
        .map(|a| a.display_name())
        .collect();
    if participants.len() <= max {
        return names.join(", ");
    }
    let extra = participants.len() - max;
    format!("{}, and {extra} more", names.join(", "))
}

/// Case-insensitive email-set equality. Display names, order, and duplicates are ignored.
pub fn address_emails_equal(left: &[Address], right: &[Address]) -> bool {
    use std::collections::HashSet;
    let left_emails: HashSet<String> = left
        .iter()
        .map(|addr| addr.email.to_ascii_lowercase())
        .collect();
    let right_emails: HashSet<String> = right
        .iter()
        .map(|addr| addr.email.to_ascii_lowercase())
        .collect();
    left_emails == right_emails
}

/// True when the row should show: non-empty, and not the same emails as From.
pub fn show_distinct_address_row(candidates: &[Address], from: &[Address]) -> bool {
    !candidates.is_empty() && !address_emails_equal(candidates, from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_display_name_strips_escaped_quotes() {
        assert_eq!(
            clean_display_name(r#"\"Vanshika Johar (TSI2517)\""#),
            "Vanshika Johar (TSI2517)"
        );
        assert_eq!(
            clean_display_name(r#""Vanshika Johar (TSI2517)""#),
            "Vanshika Johar (TSI2517)"
        );
        assert_eq!(clean_display_name("Erica Long"), "Erica Long");
        assert_eq!(
            clean_display_name("=?UTF-8?Q?Olympia_Master?="),
            "Olympia Master"
        );
        assert_eq!(
            clean_display_name(r#""Bob \"The Builder\" Jones""#),
            r#"Bob "The Builder" Jones"#
        );
    }

    #[test]
    fn address_formatted_uses_cleaned_name() {
        let addr = Address {
            name: Some(r#"\"Vanshika Johar (TSI2517)\""#.to_string()),
            email: "vjohar@tanishasystems.com".to_string(),
        };
        assert_eq!(addr.display_name(), "Vanshika Johar (TSI2517)");
        assert_eq!(
            addr.formatted(),
            "\"Vanshika Johar (TSI2517)\" <vjohar@tanishasystems.com>"
        );
        assert_eq!(
            addr.chip_tooltip().as_deref(),
            Some("vjohar@tanishasystems.com")
        );
    }

    #[test]
    fn address_formatted_quotes_hash_in_display_name() {
        let addr = Address {
            name: Some("Payal k (#TSI2930)".to_string()),
            email: "payal@tanishasystems.com".to_string(),
        };
        assert_eq!(
            addr.formatted(),
            "\"Payal k (#TSI2930)\" <payal@tanishasystems.com>"
        );
    }

    #[test]
    fn address_chip_tooltip_omitted_when_label_is_email() {
        let addr = Address {
            name: None,
            email: "a@b.com".to_string(),
        };
        assert_eq!(addr.display_name(), "a@b.com");
        assert_eq!(addr.chip_tooltip(), None);
    }

    #[test]
    fn parse_address_token_name_angle() {
        let addr = parse_address_token("Alice <a@b.com>").expect("parse");
        assert_eq!(addr.email, "a@b.com");
        assert_eq!(addr.name.as_deref(), Some("Alice"));
    }

    #[test]
    fn parse_address_token_bare_email() {
        let addr = parse_address_token("a@b.com").expect("parse");
        assert_eq!(addr.email, "a@b.com");
        assert!(addr.name.is_none());
    }

    #[test]
    fn parse_address_token_rejects_invalid() {
        assert!(parse_address_token("not-an-email").is_none());
    }

    #[test]
    fn parse_address_token_trims() {
        let addr = parse_address_token("  a@b.com  ").expect("parse");
        assert_eq!(addr.email, "a@b.com");
    }

    #[test]
    fn display_subject_strips_escaped_quotes() {
        let envelope = Envelope {
            id: MessageId(1),
            account_id: AccountId(uuid::Uuid::from_u128(1)),
            folder_id: FolderId(1),
            uid: 1,
            thread_id: ThreadId(uuid::Uuid::from_u128(2)),
            date: Utc::now(),
            subject: r#"Action Required: Transitioning to the New \"Payment\" System"#.to_string(),
            from: vec![],
            to: vec![],
            cc: vec![],
            reply_to: Vec::new(),
            sender: Vec::new(),
            routing: None,
            snippet: String::new(),
            seen: false,
            flagged: false,
            answered: false,
            draft: false,
            has_attachments: false,
            deleted_at: None,
            account_accent: String::new(),
            account_avatar: String::new(),
            size_bytes: 0,
        };
        assert_eq!(
            envelope.display_subject(),
            r#"Action Required: Transitioning to the New "Payment" System"#
        );
        assert_eq!(
            clean_display_subject(r#"Backup C:\Users\Alice"#),
            r#"Backup C:\Users\Alice"#
        );
        assert_eq!(
            clean_display_subject(r#""Quoted subject""#),
            r#""Quoted subject""#
        );
    }

    #[test]
    fn clean_preview_text_collapses_crlf_and_drops_controls() {
        assert_eq!(
            clean_preview_text("\r\n Hello Guys,\r\n\r\nNice talking to you this afternoon"),
            "Hello Guys, Nice talking to you this afternoon"
        );
        assert_eq!(clean_preview_text("Hello\u{0000}Guys"), "HelloGuys");
        assert_eq!(clean_preview_text("\r\n\t"), "");
        assert_eq!(
            Envelope {
                id: MessageId(1),
                account_id: AccountId(uuid::Uuid::from_u128(1)),
                folder_id: FolderId(1),
                uid: 1,
                thread_id: ThreadId(uuid::Uuid::from_u128(2)),
                date: Utc::now(),
                subject: String::new(),
                from: vec![],
                to: vec![],
                cc: vec![],
                reply_to: Vec::new(),
                sender: Vec::new(),
                routing: None,
                snippet: "\r\nHello Guys,\r\n\r\nNice talking".to_string(),
                seen: false,
                flagged: false,
                answered: false,
                draft: false,
                has_attachments: false,
                size_bytes: 0,
                deleted_at: None,
                account_accent: String::new(),
                account_avatar: String::new(),
            }
            .display_snippet(),
            "Hello Guys, Nice talking"
        );
    }

    #[test]
    fn clean_preview_text_strips_html_markup() {
        let html = concat!(
            r#"<!--=E2=84=A2--><!DOCTYPE html PUBLIC "-//W3C//DTD HTML 4.01 Transitional//EN">"#,
            "<html><head><style>body{margin:0}</style></head>",
            "<body><p>August Artist Drop is here</p></body></html>",
        );
        assert_eq!(clean_preview_text(html), "August Artist Drop is here");
        assert!(!clean_preview_text(html).contains("<!DOCTYPE"));
        assert_eq!(clean_preview_text("<3 thanks"), "<3 thanks");
        assert_eq!(
            clean_preview_text("See <https://example.com> for details"),
            "See <https://example.com> for details"
        );
    }

    #[test]
    fn preview_without_embedded_mime_keeps_plaintext_before_html_part() {
        let peek = concat!(
            "Hey Carter, Jordan sent you ticket\r\n",
            "\r\n",
            "--_av-gzJNu-jLZBA7Yc6NnpeKng\r\n",
            "Content-Type: text/html; charset=utf-8\r\n",
            "Content-Transfer-Encoding: quoted-printable\r\n",
            "\r\n",
            "<html><body>Hey Carter, Jordan sent you ticket</body></html>\r\n",
        );
        assert_eq!(
            preview_without_embedded_mime(peek),
            "Hey Carter, Jordan sent you ticket"
        );
        assert_eq!(
            clean_preview_text(peek),
            "Hey Carter, Jordan sent you ticket"
        );
        assert!(!clean_preview_text(peek).contains("Content-Type"));
    }

    #[test]
    fn preview_without_embedded_mime_cuts_collapsed_scan() {
        let scan = concat!(
            "Hey Carter, Jordan sent you ticket ",
            "--_av-gzJNu-jLZBA7Yc6NnpeKng ",
            "Content-Type: text/html; charset=utf-8 ",
            "Content-Transfer-Encoding: quoted-printable",
        );
        assert_eq!(
            preview_without_embedded_mime(scan),
            "Hey Carter, Jordan sent you ticket"
        );
    }

    #[test]
    fn preview_without_embedded_mime_keeps_signature_dashes() {
        let text = "Thanks\n-- \nCarter";
        assert_eq!(preview_without_embedded_mime(text), text);
    }

    #[test]
    fn preview_without_embedded_mime_keeps_zoom_sip_bullet() {
        // "•" is three bytes. After "---", the next 13 bytes land inside it.
        let text = concat!(
            "Join Zoom Meeting\n",
            "---\n",
            "Join by SIP • 123456789@zoomcrc.com\n",
        );
        assert_eq!(preview_without_embedded_mime(text), text);
    }

    #[test]
    fn preview_snippet_truncates_after_stripping_html() {
        let html = format!(
            "<html><body>{}</body></html>",
            "August Artist Drop is here. ".repeat(20)
        );
        let snippet = preview_snippet(&html);
        assert!(snippet.chars().count() <= PREVIEW_SNIPPET_MAX);
        assert!(snippet.starts_with("August Artist Drop"));
        assert!(!snippet.contains('<'), "{snippet}");
    }

    #[test]
    fn preview_scan_keeps_text_past_list_preview() {
        let branding = "Microsoft account privacy notice. ".repeat(10);
        let input = format!("{branding} Your verification code is 482193");
        let scan = preview_scan(&input);
        assert!(scan.contains("482193"));
        assert!(scan.chars().count() > PREVIEW_SNIPPET_MAX);
        let envelope = Envelope {
            id: MessageId(1),
            account_id: AccountId(uuid::Uuid::from_u128(1)),
            folder_id: FolderId(1),
            uid: 1,
            thread_id: ThreadId(uuid::Uuid::from_u128(2)),
            date: Utc::now(),
            subject: String::new(),
            from: vec![],
            to: vec![],
            cc: vec![],
            reply_to: Vec::new(),
            sender: Vec::new(),
            routing: None,
            snippet: scan,
            seen: false,
            flagged: false,
            answered: false,
            draft: false,
            has_attachments: false,
            size_bytes: 0,
            deleted_at: None,
            account_accent: String::new(),
            account_avatar: String::new(),
        };
        let preview = envelope.display_snippet();
        assert!(preview.chars().count() <= PREVIEW_SNIPPET_MAX);
        assert!(!preview.contains("482193"));
    }

    fn sample_env(id: i64, thread: u128, subject: &str, seen: bool, flagged: bool) -> Envelope {
        Envelope {
            id: MessageId(id),
            account_id: AccountId(uuid::Uuid::from_u128(1)),
            folder_id: FolderId(1),
            uid: id as u32,
            thread_id: ThreadId(uuid::Uuid::from_u128(thread)),
            date: Utc::now() - chrono::Duration::seconds(id),
            subject: subject.to_string(),
            from: vec![Address {
                name: Some(format!("User{id}")),
                email: format!("user{id}@example.com"),
            }],
            to: vec![],
            cc: vec![],
            reply_to: Vec::new(),
            sender: Vec::new(),
            routing: None,
            snippet: String::new(),
            seen,
            flagged,
            answered: false,
            draft: false,
            has_attachments: false,
            size_bytes: 0,
            deleted_at: None,
            account_accent: String::new(),
            account_avatar: String::new(),
        }
    }

    #[test]
    fn group_envelopes_merges_same_thread() {
        let envs = vec![
            sample_env(1, 99, "Re: hi", true, false),
            sample_env(2, 99, "hi", false, true),
        ];
        let convs = group_envelopes(&envs);
        assert_eq!(convs.len(), 1);
        assert_eq!(convs[0].message_count, 2);
        assert_eq!(convs[0].members.len(), 2);
        assert_eq!(convs[0].members[0].id, MessageId(2));
        assert_eq!(convs[0].members[1].id, MessageId(1));
        assert_eq!(convs[0].member_locations.len(), 2);
        assert_eq!(convs[0].unread_count, 1);
        assert!(convs[0].any_flagged);
        assert_eq!(convs[0].latest.id, MessageId(1));
    }

    #[test]
    fn group_envelopes_after_removing_oldest_member() {
        let envs = [
            sample_env(1, 99, "Re: hi", true, false),
            sample_env(2, 99, "hi", false, true),
        ];
        let remaining = vec![envs[0].clone()];
        let convs = group_envelopes(&remaining);
        assert_eq!(convs.len(), 1);
        assert_eq!(convs[0].members.len(), 1);
        assert_eq!(convs[0].members[0].id, MessageId(1));
        assert_eq!(convs[0].latest.id, MessageId(1));
    }

    #[test]
    fn participant_summary_formats_counts() {
        let addrs: Vec<Address> = (1..=4)
            .map(|i| Address {
                name: Some(format!("Name{i}")),
                email: format!("u{i}@example.com"),
            })
            .collect();
        assert_eq!(participant_summary(&addrs[..1], 3), "Name1");
        assert_eq!(participant_summary(&addrs[..2], 3), "Name1, Name2");
        assert_eq!(participant_summary(&addrs[..3], 3), "Name1, Name2, Name3");
        assert_eq!(
            participant_summary(&addrs, 3),
            "Name1, Name2, Name3, and 1 more"
        );
    }

    #[test]
    fn address_emails_equal_same_emails_different_names_and_order() {
        let a = Address {
            name: Some("Alice".to_string()),
            email: "test@x.com".to_string(),
        };
        let b = Address {
            name: Some("Bob".to_string()),
            email: "test@x.com".to_string(),
        };
        assert!(address_emails_equal(&[a.clone(), b.clone()], &[b, a]));
    }

    #[test]
    fn address_emails_equal_different_case() {
        let left = Address {
            name: None,
            email: "Test@x.com".to_string(),
        };
        let right = Address {
            name: None,
            email: "test@x.com".to_string(),
        };
        assert!(address_emails_equal(&[left], &[right]));
    }

    #[test]
    fn address_emails_equal_one_extra_address() {
        let shared = Address {
            name: None,
            email: "a@b.com".to_string(),
        };
        let extra = Address {
            name: None,
            email: "c@d.com".to_string(),
        };
        assert!(!address_emails_equal(
            std::slice::from_ref(&shared),
            &[shared.clone(), extra]
        ));
    }

    #[test]
    fn address_emails_equal_empty_vs_empty() {
        assert!(address_emails_equal(&[], &[]));
    }

    #[test]
    fn show_distinct_address_row_empty_candidates() {
        let from = vec![Address {
            name: None,
            email: "a@b.com".to_string(),
        }];
        assert!(!show_distinct_address_row(&[], &from));
    }

    #[test]
    fn show_distinct_address_row_same_emails_as_from() {
        let from = vec![Address {
            name: None,
            email: "noreply@example.com".to_string(),
        }];
        let reply_to = vec![Address {
            name: Some("Human".to_string()),
            email: "noreply@example.com".to_string(),
        }];
        assert!(!show_distinct_address_row(&reply_to, &from));
    }

    #[test]
    fn show_distinct_address_row_different_emails() {
        let from = vec![Address {
            name: None,
            email: "noreply@example.com".to_string(),
        }];
        let reply_to = vec![Address {
            name: None,
            email: "human@example.com".to_string(),
        }];
        assert!(show_distinct_address_row(&reply_to, &from));
    }

    #[test]
    fn message_list_mode_gsettings_round_trip() {
        assert_eq!(
            MessageListMode::from_gsettings("flat"),
            MessageListMode::Flat
        );
        assert_eq!(
            MessageListMode::from_gsettings("conversation"),
            MessageListMode::Conversation
        );
        assert_eq!(MessageListMode::Flat.as_str(), "flat");
        assert_eq!(MessageListMode::Conversation.as_str(), "conversation");
    }
}
