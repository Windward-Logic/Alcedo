//! Reply subject prefixing.

use alcedo_i18n::n_;
/// Shown when a message has no subject. One spelling everywhere.
/// Marked for translation; show it with `alcedo_i18n::trm!`.
pub const NO_SUBJECT: &str = n_!("(No Subject)");

/// `Re: <subject>` without stacking prefixes. Leading reply markers in common
/// languages and forms (`Re:`, `RE :`, `Aw:`, `Antw:`, `SV:`, `Re[2]:`) are
/// collapsed into a single `Re: `.
pub fn reply_subject(subject: &str) -> String {
    let mut rest = subject.trim_start();
    while let Some(after) = strip_reply_marker(rest) {
        rest = after.trim_start();
    }
    format!("Re: {rest}")
}

const REPLY_MARKERS: &[&str] = &["re", "aw", "antw", "sv", "vs", "ref", "rif", "odp", "res"];

fn strip_reply_marker(input: &str) -> Option<&str> {
    let word_len = input
        .char_indices()
        .find(|(_, c)| !c.is_ascii_alphabetic())
        .map(|(index, _)| index)?;
    let word = &input[..word_len];
    if !REPLY_MARKERS
        .iter()
        .any(|marker| marker.eq_ignore_ascii_case(word))
    {
        return None;
    }
    let mut rest = &input[word_len..];
    if let Some(counted) = rest.strip_prefix('[') {
        let close = counted.find(']')?;
        if !counted[..close].chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        rest = &counted[close + 1..];
    }
    rest.trim_start_matches(' ').strip_prefix(':')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adds_prefix_once() {
        assert_eq!(reply_subject("Hello"), "Re: Hello");
        assert_eq!(reply_subject("Re: Hello"), "Re: Hello");
        assert_eq!(reply_subject("  re:Hello"), "Re: Hello");
    }

    #[test]
    fn collapses_variants_and_chains() {
        assert_eq!(reply_subject("RE : Hello"), "Re: Hello");
        assert_eq!(reply_subject("Aw: Re: SV: Hello"), "Re: Hello");
        assert_eq!(reply_subject("Antw: Hello"), "Re: Hello");
        assert_eq!(reply_subject("Re[2]: Hello"), "Re: Hello");
    }

    #[test]
    fn leaves_words_that_only_look_like_markers() {
        assert_eq!(reply_subject("Regarding: plans"), "Re: Regarding: plans");
        assert_eq!(reply_subject("Re[x]: odd"), "Re: Re[x]: odd");
        assert_eq!(reply_subject("Svelte tips"), "Re: Svelte tips");
        assert_eq!(reply_subject(""), "Re: ");
    }
}
