//! RFC 5322 §3.6.4 msg-id lists for In-Reply-To and References.
//!
//! mail-parser strips the angle brackets when it parses these headers, so the
//! stored values are bare (`id@host`). Outgoing headers must carry `<id@host>`
//! or other clients fail to thread the reply.

/// Bracketed, space-separated msg-id list. Accepts bare or bracketed ids
/// separated by whitespace or commas; drops tokens that are not ids.
pub fn msg_id_list(raw: &str) -> String {
    msg_ids(raw)
        .map(|id| format!("<{id}>"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// `References` for a reply: the parent's References plus the parent's own
/// Message-ID, each id once, all bracketed. `None` when there is no id at all.
pub fn reply_references(
    parent_references: Option<&str>,
    parent_message_id: Option<&str>,
) -> Option<String> {
    let mut ids: Vec<&str> = Vec::new();
    let sources = [
        parent_references.unwrap_or(""),
        parent_message_id.unwrap_or(""),
    ];
    for id in sources.iter().flat_map(|source| msg_ids(source)) {
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    if ids.is_empty() {
        None
    } else {
        Some(
            ids.iter()
                .map(|id| format!("<{id}>"))
                .collect::<Vec<_>>()
                .join(" "),
        )
    }
}

/// Bracketed `In-Reply-To` for a reply, or `None` when the parent has no id.
pub fn reply_in_reply_to(parent_message_id: Option<&str>) -> Option<String> {
    parent_message_id
        .map(msg_id_list)
        .filter(|list| !list.is_empty())
}

fn msg_ids(raw: &str) -> impl Iterator<Item = &str> {
    raw.split(|c: char| c.is_whitespace() || c == ',')
        .map(|token| token.trim_matches(|c| c == '<' || c == '>'))
        .filter(|id| !id.is_empty() && id.contains('@') && !id.contains(['<', '>']))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn msg_id_list_brackets_bare_ids() {
        assert_eq!(
            msg_id_list("root@example.com parent@example.com"),
            "<root@example.com> <parent@example.com>"
        );
    }

    #[test]
    fn msg_id_list_keeps_bracketed_ids_and_accepts_commas() {
        assert_eq!(msg_id_list("<a@x>,<b@x>\r\n\t c@x"), "<a@x> <b@x> <c@x>");
    }

    #[test]
    fn msg_id_list_drops_non_ids_and_empty_input() {
        assert_eq!(msg_id_list(""), "");
        assert_eq!(msg_id_list("garbage <> <a@x>"), "<a@x>");
    }

    #[test]
    fn reply_references_appends_parent_id_once() {
        assert_eq!(
            reply_references(Some("root@x parent@x"), Some("parent@x")).as_deref(),
            Some("<root@x> <parent@x>")
        );
    }

    #[test]
    fn reply_references_compares_whole_ids_not_substrings() {
        // `a@x` is a substring of `aa@x`; it must still be appended.
        assert_eq!(
            reply_references(Some("aa@x"), Some("a@x")).as_deref(),
            Some("<aa@x> <a@x>")
        );
    }

    #[test]
    fn reply_references_from_message_id_only_or_nothing() {
        assert_eq!(
            reply_references(None, Some("m@x")).as_deref(),
            Some("<m@x>")
        );
        assert_eq!(reply_references(None, None), None);
        assert_eq!(reply_in_reply_to(Some("m@x")).as_deref(), Some("<m@x>"));
        assert_eq!(reply_in_reply_to(Some("  ")), None);
        assert_eq!(reply_in_reply_to(None), None);
    }
}
