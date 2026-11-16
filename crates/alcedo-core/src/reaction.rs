//! Gmail / RFC 9078 email reaction parsing and grouping.

use mail_parser::{HeaderValue, MessageParser, MimeHeaders};
use serde::{Deserialize, Serialize};
use unicode_segmentation::UnicodeSegmentation;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmailReaction {
    pub emoji: String,
    /// Same shape as `messages.message_id_hdr` (no angle brackets).
    pub target_message_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReactionPerson {
    pub emoji: String,
    pub from_name: String,
    pub from_email: String,
    /// `Some` when the reaction row has a Message-ID. Used only for dedupe.
    pub message_id_hdr: Option<String>,
    pub row_id: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupedReaction {
    pub emoji: String,
    pub count: u32,
    pub names: Vec<String>,
}

/// Non-empty, 1–64 bytes, all codepoints >= 0x80, exactly one grapheme.
pub fn is_single_reaction_emoji(s: &str) -> bool {
    let trimmed = s.trim();
    if trimmed.is_empty() || trimmed.len() > 64 {
        return false;
    }
    if trimmed != s {
        return false;
    }
    if !trimmed.chars().all(|ch| (ch as u32) >= 0x80) {
        return false;
    }
    trimmed.graphemes(true).count() == 1
}

pub fn parse_email_reaction(raw: &[u8]) -> Option<EmailReaction> {
    let parsed = MessageParser::default().parse(raw)?;
    parse_email_reaction_from_message(&parsed)
}

/// [`parse_email_reaction`] for a caller that has already parsed the message,
/// so the MIME tree is not built twice.
pub fn parse_email_reaction_from_message(
    parsed: &mail_parser::Message<'_>,
) -> Option<EmailReaction> {
    let target_message_id = single_in_reply_to(parsed)?;

    let mut gmail_emoji = None;
    let mut rfc9078_emoji = None;

    walk_message_parts(parsed, 0u32, &mut |leaf| {
        if disposition_is_attachment(leaf) {
            return;
        }
        let (ty, sub) = part_content_type(leaf);
        if ty.eq_ignore_ascii_case("text")
            && sub.eq_ignore_ascii_case("vnd.google.email-reaction+json")
        {
            if let Some(emoji) = parse_gmail_reaction_json(leaf.contents()) {
                gmail_emoji = Some(emoji);
            }
            return;
        }
        if ty.eq_ignore_ascii_case("text")
            && sub.eq_ignore_ascii_case("plain")
            && let Some(disp) = leaf.content_disposition()
            && disp.c_type.eq_ignore_ascii_case("reaction")
        {
            let text = String::from_utf8_lossy(leaf.contents()).trim().to_string();
            if is_single_reaction_emoji(&text) {
                rfc9078_emoji = Some(text);
            }
        }
    });

    let emoji = gmail_emoji.or(rfc9078_emoji)?;
    Some(EmailReaction {
        emoji,
        target_message_id,
    })
}

fn walk_message_parts(
    message: &mail_parser::Message<'_>,
    part_id: u32,
    visit: &mut impl FnMut(&mail_parser::MessagePart<'_>),
) {
    let Some(part) = message.part(part_id) else {
        return;
    };
    if let Some(sub_ids) = part.sub_parts() {
        for sub_id in sub_ids {
            walk_message_parts(message, *sub_id, visit);
        }
    } else {
        visit(part);
    }
}

/// True when the part is marked `Content-Disposition: attachment`.
pub fn disposition_is_attachment(part: &mail_parser::MessagePart<'_>) -> bool {
    part.content_disposition()
        .is_some_and(|ct| ct.c_type.eq_ignore_ascii_case("attachment"))
}

fn part_content_type(part: &mail_parser::MessagePart<'_>) -> (String, String) {
    part.content_type()
        .map(|ct| {
            (
                ct.c_type.to_string(),
                ct.c_subtype
                    .as_ref()
                    .map(|sub| sub.to_string())
                    .unwrap_or_else(|| "plain".to_string()),
            )
        })
        .unwrap_or_else(|| ("text".to_string(), "plain".to_string()))
}

fn parse_gmail_reaction_json(bytes: &[u8]) -> Option<String> {
    let value: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    let obj = value.as_object()?;
    let version = obj.get("version")?;
    if !version.is_number() || version.as_i64() != Some(1) {
        return None;
    }
    let emoji = obj.get("emoji")?.as_str()?;
    if is_single_reaction_emoji(emoji) {
        Some(emoji.to_string())
    } else {
        None
    }
}

fn single_in_reply_to(msg: &mail_parser::Message<'_>) -> Option<String> {
    match msg.in_reply_to() {
        HeaderValue::Text(text) => normalize_message_id_hdr(text.as_ref()),
        HeaderValue::TextList(list) if list.len() == 1 => normalize_message_id_hdr(&list[0]),
        _ => None,
    }
}

fn normalize_message_id_hdr(id: &str) -> Option<String> {
    let id = id.trim();
    if id.is_empty() || id.split_whitespace().count() > 1 {
        return None;
    }
    let id = id.strip_prefix('<').unwrap_or(id);
    let id = id.strip_suffix('>').unwrap_or(id);
    let id = id.trim();
    if id.is_empty() {
        None
    } else {
        Some(id.to_string())
    }
}

pub fn sanitize_sender_name(raw: &str) -> String {
    raw.replace(['\r', '\n'], "").trim().to_string()
}

pub fn reaction_from_display_name(sender_name: &str, mime_display_name: &str) -> String {
    let personal = sanitize_sender_name(sender_name);
    if personal.is_empty() {
        mime_display_name.trim().to_string()
    } else {
        personal
    }
}

pub fn escape_reaction_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Message-id stored on `reaction_target_hdr`: bare, no angle brackets.
pub fn bare_reaction_target_hdr(raw: &str) -> Option<String> {
    let token = raw.split_whitespace().next()?.trim();
    let id = token.trim_start_matches('<').trim_end_matches('>').trim();
    if id.is_empty() {
        None
    } else {
        Some(id.to_string())
    }
}

/// Reaction line plus the message it targeted, for the reader body.
///
/// The reaction HTML stays first. The original subject and body follow it.
pub fn reaction_reader_body(
    reaction_html: Option<&str>,
    reaction_text: &str,
    original_subject: &str,
    original_html: Option<&str>,
    original_text: &str,
) -> (String, String) {
    let reaction = match reaction_html.map(str::trim).filter(|html| !html.is_empty()) {
        Some(html) => html.to_string(),
        None => plain_as_html(reaction_text),
    };
    let original_body = match original_html.map(str::trim).filter(|html| !html.is_empty()) {
        Some(html) => html.to_string(),
        None if !original_text.trim().is_empty() => plain_as_html(original_text),
        None => String::new(),
    };
    let subject = escape_reaction_html(original_subject.trim());
    let html = format!(
        "{reaction}<div style=\"margin-top:16px;padding-top:12px;border-top:1px solid #c0c0c0\"><p style=\"margin:0 0 8px;font-weight:600\">{subject}</p>{original_body}</div>"
    );
    let text = format!(
        "{}\n\n{}\n\n{}",
        reaction_text.trim(),
        original_subject.trim(),
        original_text.trim()
    );
    (html, text)
}

fn plain_as_html(text: &str) -> String {
    escape_reaction_html(text).replace('\n', "<br>")
}

pub fn reaction_bodies(
    from_display_name: &str,
    from_email: &str,
    emoji: &str,
) -> (String, String, String) {
    let name = if from_display_name.trim().is_empty() {
        from_email
    } else {
        from_display_name
    };
    let plain = format!("{name} reacted {emoji}\n");
    let html = format!(
        "<p style=\"font-size:32px;margin:0 0 8px\">{emoji}</p><p style=\"margin:0\">{} reacted</p>",
        escape_reaction_html(name)
    );
    let json = serde_json::json!({"emoji": emoji, "version": 1}).to_string();
    (plain, html, json)
}

pub fn group_reactions(people: &[ReactionPerson], self_emails: &[String]) -> Vec<GroupedReaction> {
    let mut by_emoji: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    for person in people {
        let key = person
            .message_id_hdr
            .clone()
            .map(|hdr| format!("hdr:{hdr}"))
            .unwrap_or_else(|| format!("row:{}", person.row_id));
        if !seen.insert(key) {
            continue;
        }
        let display = if self_emails
            .iter()
            .any(|email| email.eq_ignore_ascii_case(&person.from_email))
        {
            "You".to_string()
        } else if !person.from_name.trim().is_empty() {
            person.from_name.clone()
        } else {
            person.from_email.clone()
        };
        by_emoji
            .entry(person.emoji.clone())
            .or_default()
            .push(display);
    }

    let mut chips: Vec<GroupedReaction> = by_emoji
        .into_iter()
        .map(|(emoji, mut names)| {
            names.sort();
            if let Some(you_idx) = names.iter().position(|name| name == "You") {
                let you = names.remove(you_idx);
                names.insert(0, you);
            }
            GroupedReaction {
                emoji,
                count: names.len() as u32,
                names,
            }
        })
        .collect();

    chips.sort_by(|left, right| {
        right
            .count
            .cmp(&left.count)
            .then_with(|| left.emoji.cmp(&right.emoji))
    });
    chips
}

/// Why a reaction can't be sent because the target is encrypted. Reactions
/// go out unencrypted, so they would leak that a reply happened in the clear.
pub fn encrypted_reaction_block(is_encrypted: bool) -> Option<&'static str> {
    is_encrypted.then_some("You can't react to an encrypted message")
}

pub fn reaction_block_reason(
    is_draft: bool,
    is_reaction: bool,
    has_message_id: bool,
    list_has_action: bool,
    to_cc_count: usize,
    reply_all_recipient_count: usize,
    own_reaction_count: usize,
) -> Option<&'static str> {
    if is_draft {
        return Some("You can't react to a draft");
    }
    if is_reaction {
        return Some("You can't react to a reaction");
    }
    if !has_message_id {
        return Some("This message can't be reacted to");
    }
    if list_has_action {
        return Some("Reactions aren't available on mailing lists");
    }
    if to_cc_count > 20 {
        return Some("Too many recipients to react");
    }
    if reply_all_recipient_count == 0 {
        return Some("Nobody to send this reaction to");
    }
    if own_reaction_count >= 20 {
        return Some("You've already sent 20 reactions to this message");
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gmail_reaction_raw(in_reply_to: &str, emoji: &str) -> Vec<u8> {
        format!(
            "From: Sender <sender@example.com>\r\n\
To: Reader <reader@example.com>\r\n\
Subject: Hello\r\n\
In-Reply-To: {in_reply_to}\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/alternative; boundary=\"b\"\r\n\
\r\n\
--b\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
👍 Sender reacted via Gmail\r\n\
--b\r\n\
Content-Type: text/vnd.google.email-reaction+json; charset=utf-8\r\n\
\r\n\
{{\"version\":1,\"emoji\":\"{emoji}\"}}\r\n\
--b\r\n\
Content-Type: text/html; charset=utf-8\r\n\
\r\n\
<p>👍 Sender reacted via Gmail</p>\r\n\
--b--\r\n"
        )
        .into_bytes()
    }

    #[test]
    fn parse_gmail_reaction_shape() {
        let raw = gmail_reaction_raw("<parent@example.com>", "👍");
        let reaction = parse_email_reaction(&raw).expect("reaction");
        assert_eq!(reaction.emoji, "👍");
        assert_eq!(reaction.target_message_id, "parent@example.com");
    }

    #[test]
    fn parse_rejects_version_string() {
        let raw = "In-Reply-To: <parent@example.com>\r\n\
Content-Type: text/vnd.google.email-reaction+json\r\n\
\r\n\
{\"version\":\"1\",\"emoji\":\"👍\"}\r\n"
            .to_string();
        assert!(parse_email_reaction(raw.as_bytes()).is_none());
    }

    #[test]
    fn parse_rejects_invalid_json_and_emoji() {
        assert!(parse_email_reaction(b"In-Reply-To: <p@x>\r\nContent-Type: text/vnd.google.email-reaction+json\r\n\r\n{\"version\":1").is_none());
        let bad_emoji = gmail_reaction_raw("<p@x>", "A");
        assert!(parse_email_reaction(&bad_emoji).is_none());
        let two_emoji = gmail_reaction_raw("<p@x>", "👍👍");
        assert!(parse_email_reaction(&two_emoji).is_none());
        let empty = gmail_reaction_raw("<p@x>", "");
        assert!(parse_email_reaction(&empty).is_none());
    }

    #[test]
    fn parse_rejects_json_attachment() {
        let raw = "In-Reply-To: <parent@example.com>\r\n\
Content-Type: text/vnd.google.email-reaction+json\r\n\
Content-Disposition: attachment\r\n\
\r\n\
{\"version\":1,\"emoji\":\"👍\"}\r\n"
            .to_string();
        assert!(parse_email_reaction(raw.as_bytes()).is_none());
    }

    #[test]
    fn parse_rfc9078_reaction() {
        let raw = "In-Reply-To: <parent@example.com>\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
Content-Disposition: reaction\r\n\
\r\n\
👍\r\n";
        let reaction = parse_email_reaction(raw.as_bytes()).expect("rfc9078");
        assert_eq!(reaction.emoji, "👍");
        assert_eq!(reaction.target_message_id, "parent@example.com");
    }

    #[test]
    fn gmail_wins_when_both_parts_present() {
        let raw = "In-Reply-To: <parent@example.com>\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/alternative; boundary=\"b\"\r\n\
\r\n\
--b\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
Content-Disposition: reaction\r\n\
\r\n\
❤️\r\n\
--b\r\n\
Content-Type: text/vnd.google.email-reaction+json; charset=utf-8\r\n\
\r\n\
{\"version\":1,\"emoji\":\"👍\"}\r\n\
--b--\r\n"
            .to_string();
        let reaction = parse_email_reaction(raw.as_bytes()).expect("both");
        assert_eq!(reaction.emoji, "👍");
    }

    #[test]
    fn parse_rejects_two_in_reply_to_ids() {
        let raw = "In-Reply-To: <a@x> <b@x>\r\n\
Content-Type: text/vnd.google.email-reaction+json\r\n\
\r\n\
{\"version\":1,\"emoji\":\"👍\"}\r\n";
        assert!(parse_email_reaction(raw.as_bytes()).is_none());
    }

    #[test]
    fn group_reactions_dedupes_and_sorts() {
        let people = vec![
            ReactionPerson {
                emoji: "👍".to_string(),
                from_name: "Alice".to_string(),
                from_email: "alice@example.com".to_string(),
                message_id_hdr: Some("dup@x".to_string()),
                row_id: 1,
            },
            ReactionPerson {
                emoji: "👍".to_string(),
                from_name: "Bob".to_string(),
                from_email: "bob@example.com".to_string(),
                message_id_hdr: Some("dup@x".to_string()),
                row_id: 2,
            },
            ReactionPerson {
                emoji: "❤️".to_string(),
                from_name: "".to_string(),
                from_email: "me@example.com".to_string(),
                message_id_hdr: None,
                row_id: 3,
            },
            ReactionPerson {
                emoji: "❤️".to_string(),
                from_name: "Carol".to_string(),
                from_email: "carol@example.com".to_string(),
                message_id_hdr: None,
                row_id: 4,
            },
        ];
        let grouped = group_reactions(&people, &["me@example.com".to_string()]);
        assert_eq!(grouped.len(), 2);
        assert_eq!(grouped[0].emoji, "❤️");
        assert_eq!(grouped[0].count, 2);
        assert_eq!(grouped[0].names[0], "You");
        assert_eq!(grouped[1].emoji, "👍");
        assert_eq!(grouped[1].count, 1);
    }

    #[test]
    fn reaction_block_reason_order() {
        assert_eq!(
            reaction_block_reason(true, false, true, true, 21, 1, 0),
            Some("You can't react to a draft")
        );
        assert_eq!(
            reaction_block_reason(false, false, true, true, 21, 1, 0),
            Some("Reactions aren't available on mailing lists")
        );
        assert_eq!(
            reaction_block_reason(false, false, true, false, 21, 1, 0),
            Some("Too many recipients to react")
        );
        assert_eq!(
            reaction_block_reason(false, false, true, false, 1, 0, 0),
            Some("Nobody to send this reaction to")
        );
        assert_eq!(
            reaction_block_reason(false, false, true, false, 1, 1, 20),
            Some("You've already sent 20 reactions to this message")
        );
    }

    #[test]
    fn encrypted_message_blocks_reaction() {
        assert_eq!(
            encrypted_reaction_block(true),
            Some("You can't react to an encrypted message")
        );
    }

    #[test]
    fn unencrypted_message_does_not_block_reaction() {
        assert_eq!(encrypted_reaction_block(false), None);
    }

    #[test]
    fn reaction_bodies_match_locked_templates() {
        let (plain, html, json) = reaction_bodies("Pat <dev>", "pat@example.com", "👍");
        assert_eq!(plain, "Pat <dev> reacted 👍\n");
        assert!(html.contains("&lt;"));
        assert!(html.contains("Pat &lt;dev&gt; reacted"));
        assert!(!plain.contains("Alcedo"));
        assert!(!html.contains("Alcedo"));
        assert_eq!(json, "{\"emoji\":\"👍\",\"version\":1}");
    }

    #[test]
    fn sanitize_sender_name_strips_newlines_and_trims() {
        assert_eq!(sanitize_sender_name("  Carter\n"), "Carter");
        assert_eq!(sanitize_sender_name("A\r\nB"), "AB");
        assert_eq!(sanitize_sender_name("  "), "");
    }

    #[test]
    fn reaction_reader_body_places_original_under_the_reaction_line() {
        let (html, text) = reaction_reader_body(
            Some("<p>👍</p><p>Fusion reacted</p>"),
            "Fusion reacted 👍",
            "Optivations - Video Interview",
            None,
            "Hi Carter,\nCan you meet?",
        );
        let reaction_at = html.find("Fusion reacted").expect("reaction line");
        let subject_at = html.find("Optivations - Video Interview").expect("subject");
        let body_at = html.find("Can you meet?").expect("original body");
        assert!(reaction_at < subject_at && subject_at < body_at);
        assert!(text.contains("Fusion reacted"));
        assert!(text.contains("Optivations - Video Interview"));
        assert!(text.contains("Can you meet?"));
    }

    #[test]
    fn bare_reaction_target_hdr_strips_brackets() {
        assert_eq!(
            bare_reaction_target_hdr("<parent@example.com>"),
            Some("parent@example.com".to_string())
        );
        assert_eq!(
            bare_reaction_target_hdr("parent@example.com"),
            Some("parent@example.com".to_string())
        );
    }

    #[test]
    fn reaction_from_display_name_prefers_personal_name() {
        assert_eq!(
            reaction_from_display_name("  Carter\n", "CodeInfusion"),
            "Carter"
        );
        assert_eq!(
            reaction_from_display_name("", "CodeInfusion"),
            "CodeInfusion"
        );
        assert_eq!(reaction_from_display_name("  ", ""), "");
    }
}
