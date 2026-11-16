//! Pre-send checks for outgoing mail: empty subject and a body that mentions
//! an attachment when none is attached. Pure so the heuristics are tested.

/// Something worth confirming before a message goes out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreSendWarning {
    EmptySubject,
    MissingAttachment,
}

impl PreSendWarning {
    /// Dialog heading for this warning.
    pub fn heading(self) -> &'static str {
        match self {
            Self::EmptySubject => "Send Without a Subject?",
            Self::MissingAttachment => "Send Without an Attachment?",
        }
    }

    /// Dialog body for this warning.
    pub fn body(self) -> &'static str {
        match self {
            Self::EmptySubject => "This message has no subject. Recipients may overlook it.",
            Self::MissingAttachment => {
                "The message mentions an attachment, but nothing is attached."
            }
        }
    }
}

/// Warnings for a draft about to be sent, most important first.
///
/// `body` may be plain text or HTML; quoted reply text is ignored so an
/// "attached" in the message being replied to does not trigger a warning.
pub fn pre_send_warnings(
    subject: &str,
    body: &str,
    attachment_count: usize,
) -> Vec<PreSendWarning> {
    let mut warnings = Vec::new();
    if attachment_count == 0 && mentions_attachment(body) {
        warnings.push(PreSendWarning::MissingAttachment);
    }
    if subject.trim().is_empty() {
        warnings.push(PreSendWarning::EmptySubject);
    }
    warnings
}

/// Whether the author's own text (quotes excluded) talks about an attachment.
pub fn mentions_attachment(body: &str) -> bool {
    let own_text = authored_text(body);
    words(&own_text).any(|word| is_attachment_word(&word))
}

const ATTACHMENT_STEMS: &[&str] = &["attach", "enclosed"];

fn is_attachment_word(word: &str) -> bool {
    ATTACHMENT_STEMS.iter().any(|stem| word.starts_with(stem))
}

fn words(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
}

/// Strip HTML quotes and tags, then cut plain-text quotes and the quoted
/// header of a reply or forward.
fn authored_text(body: &str) -> String {
    let without_quotes = strip_html_blockquotes(body);
    let text = strip_tags(&without_quotes);
    let mut kept = String::new();
    for line in text.lines() {
        let trimmed = line.trim_start();
        if is_quote_boundary(trimmed) {
            break;
        }
        if trimmed.starts_with('>') {
            continue;
        }
        kept.push_str(line);
        kept.push('\n');
    }
    kept
}

fn is_quote_boundary(line: &str) -> bool {
    let lower = line.to_lowercase();
    lower.starts_with("-----original message-----")
        || lower.starts_with("---------- forwarded message")
        || (lower.starts_with("on ") && lower.trim_end().ends_with("wrote:"))
}

fn strip_html_blockquotes(body: &str) -> String {
    let lower = body.to_ascii_lowercase();
    let mut out = String::with_capacity(body.len());
    let mut cursor = 0;
    let mut depth = 0usize;
    while cursor < body.len() {
        let rest = &lower[cursor..];
        if rest.starts_with("<blockquote") {
            depth += 1;
            cursor += tag_len(&lower[cursor..]);
            continue;
        }
        if rest.starts_with("</blockquote") {
            depth = depth.saturating_sub(1);
            cursor += tag_len(&lower[cursor..]);
            continue;
        }
        let ch = body[cursor..].chars().next().expect("cursor inside body");
        if depth == 0 {
            out.push(ch);
        }
        cursor += ch.len_utf8();
    }
    out
}

fn tag_len(rest: &str) -> usize {
    rest.find('>').map(|end| end + 1).unwrap_or(rest.len())
}

fn strip_tags(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    let mut in_tag = false;
    for ch in body.chars() {
        match ch {
            '<' => in_tag = true,
            '>' if in_tag => {
                in_tag = false;
                out.push(' ');
            }
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_subject_is_flagged() {
        assert_eq!(
            pre_send_warnings("  ", "Hello", 0),
            vec![PreSendWarning::EmptySubject]
        );
        assert!(pre_send_warnings("Hi", "Hello", 0).is_empty());
    }

    #[test]
    fn attachment_mention_without_attachment_is_flagged() {
        assert_eq!(
            pre_send_warnings("Report", "Please see the attached report.", 0),
            vec![PreSendWarning::MissingAttachment]
        );
        assert!(pre_send_warnings("Report", "Please see the attached report.", 1).is_empty());
    }

    #[test]
    fn both_warnings_missing_attachment_first() {
        assert_eq!(
            pre_send_warnings("", "I've attached it", 0),
            vec![
                PreSendWarning::MissingAttachment,
                PreSendWarning::EmptySubject
            ]
        );
    }

    #[test]
    fn detects_common_forms() {
        assert!(mentions_attachment("Attachment: budget"));
        assert!(mentions_attachment("I'm attaching the slides"));
        assert!(mentions_attachment("Enclosed is the invoice"));
        assert!(!mentions_attachment("Let's meet at the cafe"));
        assert!(!mentions_attachment("We were detached from reality"));
    }

    #[test]
    fn ignores_plain_text_quotes() {
        assert!(!mentions_attachment("Thanks!\n> see attached file"));
        assert!(!mentions_attachment(
            "Thanks!\nOn Mon, 3 Jun 2026, Ann wrote:\nsee attached file"
        ));
        assert!(!mentions_attachment(
            "FYI\n---------- Forwarded message ---------\nattached"
        ));
    }

    #[test]
    fn ignores_html_blockquotes_and_tags() {
        assert!(!mentions_attachment(
            "<p>Thanks</p><blockquote type=\"cite\"><p>see attached</p></blockquote>"
        ));
        assert!(mentions_attachment("<p>See <b>attached</b></p>"));
        assert!(!mentions_attachment(
            "<img alt=\"x\" src=\"attachment.png\">"
        ));
    }

    #[test]
    fn handles_non_ascii_text() {
        assert!(!mentions_attachment("Grüße — bis bald"));
        assert!(mentions_attachment("Grüße, attached ✓"));
    }
}
