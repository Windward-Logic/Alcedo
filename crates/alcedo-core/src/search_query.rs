//! Gmail-style search query parser shared by store and UI.

use std::fmt::Write;

use crate::account::SpecialUse;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SearchQuery {
    pub free: Vec<String>,
    pub exclude: Vec<String>,
    pub from: Vec<String>,
    pub to: Vec<String>,
    pub subject: Vec<String>,
    pub folder: Option<FolderToken>,
    pub has_attachment: bool,
    pub unread: Option<bool>,
    pub starred: Option<bool>,
    pub after: Option<String>,
    pub before: Option<String>,
    /// Whole-message size in bytes, strictly greater than this (`larger:10M`).
    pub larger_than: Option<u64>,
    /// Whole-message size in bytes, strictly less than this (`smaller:1M`).
    pub smaller_than: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FolderToken {
    All,
    Name(String),
}

pub fn parse_search_query(raw: &str) -> SearchQuery {
    let mut query = SearchQuery::default();
    for token in tokenize(raw) {
        apply_token(&mut query, &token);
    }
    query
}

pub fn fts_match_clause(query: &SearchQuery) -> Option<String> {
    if query.free.is_empty() && query.exclude.is_empty() && query.subject.is_empty() {
        return None;
    }
    let mut parts = Vec::new();
    for term in &query.subject {
        parts.push(format!("subject:{}", quote_fts_term(term)));
    }
    for term in &query.free {
        parts.push(quote_fts_term(term));
    }
    for term in &query.exclude {
        // FTS5 unary NOT must be adjacent to the term (`-"promo"`, not `- "promo"`).
        parts.push(format!("-{}", quote_fts_term(term)));
    }
    Some(parts.join(" "))
}

pub fn apply_is_token(raw: &str, token: &str, enabled: bool) -> String {
    let mut text = raw.trim().to_string();
    match token {
        "is:unread" => {
            text = remove_is_tokens(&text, &["is:unread", "is:read"]);
            if enabled {
                text = append_token(&text, "is:unread");
            }
        }
        "is:starred" => {
            text = remove_is_tokens(&text, &["is:starred", "is:unstarred"]);
            if enabled {
                text = append_token(&text, "is:starred");
            }
        }
        "has:attachment" => {
            text = remove_has_attachment_tokens(&text);
            if enabled {
                text = append_token(&text, "has:attachment");
            }
        }
        _ => {}
    }
    text.trim().to_string()
}

pub fn query_has_folder_operator(raw: &str) -> bool {
    parse_search_query(raw).folder.is_some()
}

/// How to bake folder scope into a query that is saved for later.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScopeBake {
    All,
    CurrentUnified,
    CurrentFolder,
}

/// Append `in:anywhere`, `folder:inbox`, or `in:{imap}` when the query has no folder operator.
pub fn ensure_scope_in_query(raw: &str, scope: ScopeBake, folder_imap: Option<&str>) -> String {
    if query_has_folder_operator(raw) {
        return raw.trim().to_string();
    }
    let token = match scope {
        ScopeBake::All => "in:anywhere".to_string(),
        ScopeBake::CurrentUnified => "folder:inbox".to_string(),
        ScopeBake::CurrentFolder => {
            let name = folder_imap.unwrap_or("INBOX");
            format!("in:{}", quote_folder_imap_name(name))
        }
    };
    append_token(raw.trim(), &token)
}

/// Trash and Junk stay out of all-mail searches (such as `is:unread`)
/// unless the query names a folder (`in:trash`, `in:anywhere`).
pub fn hide_special_in_unscoped_search(special: &SpecialUse) -> bool {
    matches!(special, SpecialUse::Trash | SpecialUse::Junk)
}

fn quote_folder_imap_name(name: &str) -> String {
    if name.contains(' ') || name.contains(':') {
        let escaped = name.replace('\\', "\\\\").replace('"', "\\\"");
        format!("\"{escaped}\"")
    } else {
        name.to_string()
    }
}

fn tokenize(raw: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut chars = raw.chars().peekable();
    while let Some(&ch) = chars.peek() {
        if ch.is_whitespace() {
            chars.next();
            continue;
        }
        if ch == '"' {
            chars.next();
            let mut token = String::new();
            while let Some(next) = chars.next() {
                if next == '\\' {
                    if matches!(chars.peek(), Some('"')) {
                        chars.next();
                        token.push('"');
                    } else {
                        token.push(next);
                    }
                } else if next == '"' {
                    break;
                } else {
                    token.push(next);
                }
            }
            tokens.push(token);
        } else {
            let mut token = String::new();
            while let Some(&next) = chars.peek() {
                if next.is_whitespace() {
                    break;
                }
                if next == '"' {
                    chars.next();
                    while let Some(ch) = chars.next() {
                        if ch == '\\' {
                            if matches!(chars.peek(), Some('"')) {
                                chars.next();
                                token.push('"');
                            } else {
                                token.push(ch);
                            }
                        } else if ch == '"' {
                            break;
                        } else {
                            token.push(ch);
                        }
                    }
                    continue;
                }
                token.push(chars.next().unwrap());
            }
            tokens.push(token);
        }
    }
    tokens
}

fn apply_token(query: &mut SearchQuery, token: &str) {
    let (exclude, body) = if let Some(rest) = token.strip_prefix('-') {
        if rest.contains(':') && operator_key(rest).is_some() {
            (false, token)
        } else {
            (true, rest)
        }
    } else {
        (false, token)
    };

    if exclude {
        if !body.is_empty() {
            query.exclude.push(body.to_string());
        }
        return;
    }

    let Some((key, value)) = split_operator(body) else {
        if !body.is_empty() {
            query.free.push(body.to_string());
        }
        return;
    };

    match key.as_str() {
        "from" if !value.is_empty() => query.from.push(strip_quotes(value)),
        "to" if !value.is_empty() => query.to.push(strip_quotes(value)),
        "subject" if !value.is_empty() => query.subject.push(strip_quotes(value)),
        "folder" | "in" => apply_folder_token(query, value),
        "has"
            if matches!(
                value.to_ascii_lowercase().as_str(),
                "attachment" | "attachments"
            ) =>
        {
            query.has_attachment = true;
        }
        "is" => apply_is_value(query, value),
        "after" => {
            if let Some(date) = parse_date_token(value) {
                query.after = Some(date);
            }
        }
        "before" => {
            if let Some(date) = parse_date_token(value) {
                query.before = Some(date);
            }
        }
        "larger" | "bigger" => {
            if let Some(bytes) = parse_size_token(value) {
                query.larger_than = Some(bytes);
            }
        }
        "smaller" => {
            if let Some(bytes) = parse_size_token(value) {
                query.smaller_than = Some(bytes);
            }
        }
        _ => query.free.push(body.to_string()),
    }
}

fn apply_folder_token(query: &mut SearchQuery, value: &str) {
    if value.is_empty() {
        return;
    }
    let lower = value.to_ascii_lowercase();
    query.folder = Some(if matches!(lower.as_str(), "all" | "anywhere") {
        FolderToken::All
    } else {
        FolderToken::Name(value.to_string())
    });
}

fn apply_is_value(query: &mut SearchQuery, value: &str) {
    match value.to_ascii_lowercase().as_str() {
        "unread" => query.unread = Some(true),
        "read" => query.unread = Some(false),
        "starred" => query.starred = Some(true),
        "unstarred" => query.starred = Some(false),
        _ => {}
    }
}

fn strip_quotes(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.len() >= 2 && trimmed.starts_with('"') && trimmed.ends_with('"') {
        trimmed[1..trimmed.len() - 1].replace("\\\"", "\"")
    } else {
        trimmed.to_string()
    }
}

fn split_operator(token: &str) -> Option<(String, &str)> {
    let (key, value) = token.split_once(':')?;
    if key.is_empty() {
        return None;
    }
    Some((key.to_ascii_lowercase(), value))
}

fn operator_key(token: &str) -> Option<&str> {
    let (key, _) = token.split_once(':')?;
    if key.is_empty() { None } else { Some(key) }
}

/// `10`, `10B`, `500K`, `1.5M`, `2GB`. K/M/G are 1024-based.
fn parse_size_token(value: &str) -> Option<u64> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    let split_at = value
        .char_indices()
        .find(|(_, ch)| ch.is_ascii_alphabetic())
        .map(|(index, _)| index)
        .unwrap_or(value.len());
    let (number, unit) = value.split_at(split_at);
    let number = number.trim();
    if number.is_empty() {
        return None;
    }
    let quantity: f64 = number.parse().ok()?;
    if !quantity.is_finite() || quantity < 0.0 {
        return None;
    }
    let mult: u64 = match unit.trim().to_ascii_lowercase().as_str() {
        "" | "b" => 1,
        "k" | "kb" | "kib" => 1024,
        "m" | "mb" | "mib" => 1024 * 1024,
        "g" | "gb" | "gib" => 1024 * 1024 * 1024,
        _ => return None,
    };
    let bytes = quantity * mult as f64;
    if !bytes.is_finite() || bytes > i64::MAX as f64 {
        return None;
    }
    Some(bytes.round() as u64)
}

fn parse_date_token(value: &str) -> Option<String> {
    let normalized = value.replace('/', "-");
    let mut parts = normalized.split('-');
    let year: i32 = parts.next()?.parse().ok()?;
    let month: u32 = parts.next()?.parse().ok()?;
    let day: u32 = parts.next()?.parse().ok()?;
    if parts.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    Some(format!("{year:04}-{month:02}-{day:02}"))
}

fn quote_fts_term(term: &str) -> String {
    let mut quoted = String::from('"');
    for ch in term.chars() {
        if ch == '"' {
            quoted.push_str("\"\"");
        } else {
            quoted.push(ch);
        }
    }
    quoted.push('"');
    quoted
}

fn append_token(raw: &str, token: &str) -> String {
    if tokenize(raw)
        .iter()
        .any(|part| token_eq_ignore_case(part, token))
    {
        raw.to_string()
    } else if raw.is_empty() {
        token.to_string()
    } else {
        let mut out = raw.to_string();
        let _ = write!(out, " {token}");
        out
    }
}

fn token_eq_ignore_case(left: &str, right: &str) -> bool {
    left.eq_ignore_ascii_case(right)
}

fn remove_is_tokens(raw: &str, tokens: &[&str]) -> String {
    tokenize(raw)
        .into_iter()
        .filter(|part| !tokens.iter().any(|token| token_eq_ignore_case(part, token)))
        .collect::<Vec<_>>()
        .join(" ")
}

fn remove_has_attachment_tokens(raw: &str) -> String {
    tokenize(raw)
        .into_iter()
        .filter(|part| {
            let lower = part.to_ascii_lowercase();
            lower != "has:attachment" && lower != "has:attachments"
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_inbox_michael() {
        let q = parse_search_query("folder:inbox michael");
        assert_eq!(q.folder, Some(FolderToken::Name("inbox".to_string())));
        assert_eq!(q.free, vec!["michael".to_string()]);
    }

    #[test]
    fn in_anywhere_from_ada() {
        let q = parse_search_query("in:anywhere from:ada");
        assert_eq!(q.folder, Some(FolderToken::All));
        assert_eq!(q.from, vec!["ada".to_string()]);
    }

    #[test]
    fn subject_quoted_has_attachment() {
        let q = parse_search_query(r#"subject:"q1 plan" has:attachment"#);
        assert_eq!(q.subject, vec!["q1 plan".to_string()]);
        assert!(q.has_attachment);
    }

    #[test]
    fn exclude_promo_invoice() {
        let q = parse_search_query("-promo invoice");
        assert_eq!(q.exclude, vec!["promo".to_string()]);
        assert_eq!(q.free, vec!["invoice".to_string()]);
    }

    #[test]
    fn larger_and_smaller_parse_binary_units() {
        let q = parse_search_query("larger:10M smaller:1.5K");
        assert_eq!(q.larger_than, Some(10 * 1024 * 1024));
        assert_eq!(q.smaller_than, Some(1536));

        assert_eq!(
            parse_search_query("bigger:500KB").larger_than,
            Some(500 * 1024)
        );
        assert_eq!(
            parse_search_query("larger:2GB").larger_than,
            Some(2 * 1024 * 1024 * 1024)
        );
        assert!(parse_search_query("larger:nope").larger_than.is_none());
        assert!(parse_search_query("larger:").larger_than.is_none());
        let with_from = parse_search_query("larger:10mb from:ada");
        assert_eq!(with_from.larger_than, Some(10 * 1024 * 1024));
        assert_eq!(with_from.from, vec!["ada".to_string()]);
    }

    #[test]
    fn after_valid_ignored_invalid() {
        let valid = parse_search_query("after:2026-01-01");
        assert_eq!(valid.after.as_deref(), Some("2026-01-01"));

        let invalid = parse_search_query("after:nope");
        assert!(invalid.after.is_none());
    }

    #[test]
    fn fts_match_clause_quotes_punctuation() {
        let q = parse_search_query("hello:world");
        assert_eq!(fts_match_clause(&q).as_deref(), Some("\"hello:world\""));
    }

    #[test]
    fn fts_match_clause_exclude_is_adjacent_not() {
        let q = parse_search_query("-promo invoice");
        assert_eq!(
            fts_match_clause(&q).as_deref(),
            Some("\"invoice\" -\"promo\"")
        );
    }

    #[test]
    fn apply_is_token_unread_round_trip() {
        let enabled = apply_is_token("michael", "is:unread", true);
        assert_eq!(enabled, "michael is:unread");
        let disabled = apply_is_token(&enabled, "is:unread", false);
        assert_eq!(disabled, "michael");
    }

    #[test]
    fn query_has_folder_operator_detects_tokens() {
        assert!(query_has_folder_operator("folder:inbox michael"));
        assert!(query_has_folder_operator("in:sent"));
        assert!(!query_has_folder_operator("michael"));
        assert!(!query_has_folder_operator("-folder:inbox"));
    }

    #[test]
    fn ensure_scope_bakes_all_mail() {
        assert_eq!(
            ensure_scope_in_query("michael", ScopeBake::All, None),
            "michael in:anywhere"
        );
    }

    #[test]
    fn ensure_scope_bakes_unified_inbox() {
        assert_eq!(
            ensure_scope_in_query("michael", ScopeBake::CurrentUnified, None),
            "michael folder:inbox"
        );
    }

    #[test]
    fn ensure_scope_quotes_gmail_sent() {
        assert_eq!(
            ensure_scope_in_query(
                "invoice",
                ScopeBake::CurrentFolder,
                Some("[Gmail]/Sent Mail")
            ),
            r#"invoice in:"[Gmail]/Sent Mail""#
        );
    }

    #[test]
    fn ensure_scope_skips_when_operator_present() {
        let raw = "folder:inbox michael";
        assert_eq!(ensure_scope_in_query(raw, ScopeBake::All, None), raw);
        assert_eq!(
            parse_search_query(&ensure_scope_in_query(
                "michael",
                ScopeBake::CurrentUnified,
                None
            )),
            parse_search_query("folder:inbox michael")
        );
    }

    #[test]
    fn unscoped_search_hides_trash_and_junk() {
        assert!(hide_special_in_unscoped_search(&SpecialUse::Trash));
        assert!(hide_special_in_unscoped_search(&SpecialUse::Junk));
        assert!(!hide_special_in_unscoped_search(&SpecialUse::Inbox));
        assert!(!hide_special_in_unscoped_search(&SpecialUse::Sent));
        assert!(!hide_special_in_unscoped_search(&SpecialUse::Archive));
    }
}
