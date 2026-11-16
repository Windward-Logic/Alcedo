//! Local verification / OTP code extraction from mail subject and body text.

use crate::sanitize::html_to_plaintext;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerificationCode {
    pub code: String,
}

const POSITIVE_KEYWORDS: &[&str] = &[
    "verification",
    "verify",
    "passcode",
    "one-time",
    "onetime",
    "one time",
    "otp",
    "2fa",
    "mfa",
    "security code",
    "sign-in",
    "sign in",
    "signin",
    "authentication",
    "auth code",
    "login code",
    "confirmation",
    "confirm",
    "código",
    "codigo",
    "code de",
    "bestätigung",
    "verifizierung",
    "codice",
    "код",
    "验证码",
    "確認",
];

const NEGATIVE_KEYWORDS: &[&str] = &[
    "phone", "tel.", "fax", "order", "invoice", "tracking", "zip", "postal", "ticket", "http",
    "www.", "ext.", "sku",
];

pub fn extract_verification_code(
    subject: &str,
    text: &str,
    html: Option<&str>,
) -> Option<VerificationCode> {
    let body = body_plaintext(text, html);
    if subject.is_empty() && body.is_empty() {
        return None;
    }

    let truncated = truncate_scalars(&body, 8000);
    let haystack = if subject.is_empty() {
        truncated.to_string()
    } else if truncated.is_empty() {
        subject.to_string()
    } else {
        format!("{subject}\n{truncated}")
    };

    let subject_lower = subject.to_ascii_lowercase();
    let mut best: Option<(usize, i32, String)> = None;

    for (start, end, code) in collect_candidates(&haystack) {
        if reject_candidate(&haystack, start, end, &code) {
            continue;
        }
        let window = keyword_window(&haystack, start, end);
        let window_lower = window.to_ascii_lowercase();
        let (positive, lexical) = lexical_score(&window_lower, &subject_lower);
        let shape = shape_score(&code, &haystack, start, end);
        let total = shape + lexical;
        if lexical >= 3 && total >= 6 {
            match &best {
                Some((best_start, best_score, _)) if *best_score > total => continue,
                Some((best_start, best_score, _))
                    if *best_score == total && *best_start < start =>
                {
                    continue;
                }
                _ => best = Some((start, total, code)),
            }
        }
        let _ = positive;
    }

    best.map(|(_, _, code)| VerificationCode { code })
}

pub fn format_code_display(code: &str) -> String {
    if code.chars().all(|c| c.is_ascii_digit()) {
        match code.len() {
            6 => format!("{} {}", &code[..3], &code[3..]),
            8 => format!("{} {}", &code[..4], &code[4..]),
            _ => code.to_string(),
        }
    } else {
        code.to_string()
    }
}

fn body_plaintext(text: &str, html: Option<&str>) -> String {
    if !text.trim().is_empty() {
        return text.to_string();
    }
    if let Some(html) = html.filter(|h| !h.is_empty()) {
        return html_to_plaintext(html);
    }
    String::new()
}

fn truncate_scalars(s: &str, max: usize) -> &str {
    if s.chars().count() <= max {
        return s;
    }
    for (count, (i, _)) in s.char_indices().enumerate() {
        if count >= max {
            return &s[..i];
        }
    }
    s
}

fn collect_candidates(haystack: &str) -> Vec<(usize, usize, String)> {
    let mut candidates = Vec::new();
    let bytes = haystack.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    while i < len {
        let ch = bytes[i];
        if ch.is_ascii_digit() {
            let start = i;
            while i < len && bytes[i].is_ascii_digit() {
                i += 1;
            }
            let run_len = i - start;
            if (4..=8).contains(&run_len) {
                candidates.push((start, i, haystack[start..i].to_string()));
            }
        } else if ch.is_ascii_alphanumeric() {
            let body_start = i;
            while i < len && bytes[i].is_ascii_alphanumeric() {
                i += 1;
            }
            let run_len = i - body_start;
            if (6..=12).contains(&run_len) {
                let body = &haystack[body_start..i];
                let has_digit = body.chars().any(|c| c.is_ascii_digit());
                let has_letter = body.chars().any(|c| c.is_ascii_alphabetic());
                if has_digit && has_letter {
                    let start = if body_start > 0 && bytes[body_start - 1] == b'+' {
                        body_start - 1
                    } else {
                        body_start
                    };
                    candidates.push((start, i, haystack[start..i].to_string()));
                }
            }
        } else {
            i += 1;
        }
    }
    candidates
}

fn reject_candidate(haystack: &str, start: usize, end: usize, code: &str) -> bool {
    let body = code.strip_prefix('+').unwrap_or(code);

    if body.chars().all(|c| c.is_ascii_digit()) {
        if code.starts_with('+') {
            return true;
        }
        if !(4..=8).contains(&body.len()) {
            return true;
        }
        if let Ok(year) = code.parse::<u32>()
            && (1900..=2099).contains(&year)
        {
            return true;
        }
        if code.len() == 8
            && let Ok(year) = code[..4].parse::<u32>()
            && (1900..=2099).contains(&year)
        {
            return true;
        }
        if start > 0 {
            let before = haystack.as_bytes()[start - 1];
            if before == b'$' || before == b'#' || before == b'%' {
                return true;
            }
        }
    } else if body.chars().any(|c| c.is_ascii_digit())
        && body.chars().any(|c| c.is_ascii_alphabetic())
    {
        if !(6..=12).contains(&body.len()) {
            return true;
        }
    } else {
        return true;
    }

    if code.chars().all(|c| c.is_ascii_alphabetic()) {
        return true;
    }

    let window = keyword_window(haystack, start, end);
    if window.contains("://") || inside_url(haystack, start) {
        return true;
    }

    if start > 0 && haystack.as_bytes()[start - 1] == b'@' {
        return true;
    }
    if end < haystack.len() && haystack.as_bytes()[end] == b'@' {
        return true;
    }

    false
}

fn keyword_window(haystack: &str, start: usize, end: usize) -> &str {
    let window_start = haystack[..start]
        .char_indices()
        .rev()
        .nth(47)
        .map(|(i, _)| i)
        .unwrap_or(0);
    let window_end = haystack[end..]
        .char_indices()
        .nth(48)
        .map(|(i, _)| end + i)
        .unwrap_or(haystack.len());
    &haystack[window_start..window_end]
}

fn lexical_score(window_lower: &str, subject_lower: &str) -> (i32, i32) {
    let mut positive = 0i32;
    let mut seen_positive: Vec<&str> = Vec::new();

    for keyword in POSITIVE_KEYWORDS {
        if keyword_in_window(window_lower, keyword) && !seen_positive.contains(keyword) {
            seen_positive.push(keyword);
            positive += 3;
            if keyword_in_window(subject_lower, keyword) {
                positive += 2;
            }
        }
    }

    if has_whole_word(window_lower, "code") && !seen_positive.contains(&"code") {
        seen_positive.push("code");
        positive += 3;
        if has_whole_word(subject_lower, "code") {
            positive += 2;
        }
    }

    if has_whole_word(window_lower, "pin") && !seen_positive.contains(&"pin") {
        seen_positive.push("pin");
        positive += 3;
        if has_whole_word(subject_lower, "pin") {
            positive += 2;
        }
    }

    let mut negative = 0i32;
    let mut seen_negative: Vec<&str> = Vec::new();
    for keyword in NEGATIVE_KEYWORDS {
        if window_lower.contains(keyword) && !seen_negative.contains(keyword) {
            seen_negative.push(keyword);
            negative += 4;
        }
    }

    let lexical = (positive - negative).max(0);
    (positive, lexical)
}

/// Short tokens match inside longer words (`otpToken`). Require a word break
/// so a query parameter is not a verification keyword.
fn keyword_in_window(window_lower: &str, keyword: &str) -> bool {
    if matches!(keyword, "otp" | "2fa" | "mfa") {
        has_whole_word(window_lower, keyword)
    } else {
        window_lower.contains(keyword)
    }
}

/// True when `start` sits in a URL. LinkedIn job alerts put `otpToken` and
/// tracking ids far past `://`, outside the 48-character keyword window.
fn inside_url(haystack: &str, start: usize) -> bool {
    let before = &haystack[..start];
    // `rfind` returns the first byte of the whitespace char. NBSP (U+00A0)
    // and other non-ASCII spaces are more than one byte; stepping one byte
    // lands inside the character and panics on the slice below.
    let span_start = before
        .rfind(char::is_whitespace)
        .map(|index| index + before[index..].chars().next().map_or(0, char::len_utf8))
        .unwrap_or(0);
    haystack[span_start..start].contains("://")
}

fn has_whole_word(text: &str, word: &str) -> bool {
    let bytes = text.as_bytes();
    let word_bytes = word.as_bytes();
    let word_len = word_bytes.len();
    if word_len == 0 || bytes.len() < word_len {
        return false;
    }
    let mut i = 0;
    while i + word_len <= bytes.len() {
        if bytes[i..i + word_len] == *word_bytes {
            let before_ok = i == 0 || !bytes[i - 1].is_ascii_alphabetic();
            let after_ok =
                i + word_len >= bytes.len() || !bytes[i + word_len].is_ascii_alphabetic();
            if before_ok && after_ok {
                return true;
            }
        }
        i += 1;
    }
    false
}

fn shape_score(code: &str, haystack: &str, start: usize, end: usize) -> i32 {
    let body_len = code
        .strip_prefix('+')
        .map(|body| body.len())
        .unwrap_or(code.len());
    let mut score = match body_len {
        6 => 4,
        8..=10 => 2,
        4 | 5 | 7 | 11 | 12 => 1,
        _ => 0,
    };
    if is_own_line(haystack, start, end) {
        score += 3;
    }
    score
}

fn is_own_line(haystack: &str, start: usize, end: usize) -> bool {
    let line_start = haystack[..start].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let line_end = haystack[end..]
        .find('\n')
        .map(|i| end + i)
        .unwrap_or(haystack.len());
    let line = &haystack[line_start..line_end];
    line.trim() == &haystack[start..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verification_code_in_sentence() {
        let result = extract_verification_code("", "Your verification code is 482193", None);
        assert_eq!(
            result,
            Some(VerificationCode {
                code: "482193".to_string()
            })
        );
    }

    #[test]
    fn subject_plus_body_own_line() {
        let result = extract_verification_code("GitHub verification code", "482193", None);
        assert_eq!(
            result,
            Some(VerificationCode {
                code: "482193".to_string()
            })
        );
    }

    #[test]
    fn code_with_colon() {
        let result = extract_verification_code("", "Your code is: 847291", None);
        assert_eq!(
            result,
            Some(VerificationCode {
                code: "847291".to_string()
            })
        );
    }

    #[test]
    fn html_only_body() {
        let result = extract_verification_code(
            "",
            "",
            Some("<p>Your verification code is <b>123456</b></p>"),
        );
        assert_eq!(
            result,
            Some(VerificationCode {
                code: "123456".to_string()
            })
        );
    }

    #[test]
    fn order_number_rejected() {
        assert!(extract_verification_code("", "Order #482193 shipped", None).is_none());
    }

    #[test]
    fn phone_number_rejected() {
        assert!(extract_verification_code("", "Call 555-1212", None).is_none());
    }

    #[test]
    fn year_rejected() {
        assert!(extract_verification_code("", "See you in 2024", None).is_none());
    }

    #[test]
    fn encode_and_tracking_url_rejected() {
        let body = "The encode step failed https://tracking.example.com?id=123456";
        assert!(extract_verification_code("", body, None).is_none());
    }

    #[test]
    fn nonbreaking_space_before_code_does_not_panic() {
        // Gmail HTML often uses U+00A0 between words. Slicing one byte past it panics.
        let body = "Your verification code is\u{a0}482193";
        let result = extract_verification_code("", body, None);
        assert_eq!(
            result,
            Some(VerificationCode {
                code: "482193".to_string()
            })
        );
    }

    #[test]
    fn url_after_nonbreaking_space_is_still_a_url() {
        let haystack = "See\u{a0}https://tracking.example.com?id=123456";
        let start = haystack.find("123456").unwrap();
        assert!(inside_url(haystack, start));
    }

    #[test]
    fn linkedin_job_alert_tracking_token_rejected() {
        let subject = "Data Scientist role and 1 other job you may be interested in";
        let body = "Your job alert for Artificial Intelligence Engineer \
Manage your job alerts: https://www.linkedin.com/comm/jobs/alerts?trk=eml-email_job_alert_digest_01&eid=l66q7t-mv8fwjcd-72&otpToken=ODE2ZjQxMjhkODYwMzNhN2RmZGExZTZl \
View job: https://www.linkedin.com/comm/jobs/view/4478137018/?otpToken=ODE2ZjQxMjhkODYw";
        assert!(extract_verification_code(subject, body, None).is_none());
    }

    #[test]
    fn otp_word_still_matches() {
        let result = extract_verification_code("", "Your OTP is 482193", None);
        assert_eq!(
            result,
            Some(VerificationCode {
                code: "482193".to_string()
            })
        );
    }

    #[test]
    fn alphanumeric_code() {
        let result = extract_verification_code("", "Your code is A1B2C3", None);
        assert_eq!(
            result,
            Some(VerificationCode {
                code: "A1B2C3".to_string()
            })
        );
    }

    #[test]
    fn date_like_without_lexical() {
        let result = extract_verification_code("Please confirm attendance", "20240115", None);
        assert!(result.is_none());
    }

    #[test]
    fn empty_subject_and_body() {
        assert!(extract_verification_code("", "", None).is_none());
    }

    #[test]
    fn format_code_display_six_digit() {
        assert_eq!(format_code_display("482193"), "482 193");
    }

    #[test]
    fn format_code_display_eight_digit() {
        assert_eq!(format_code_display("12345678"), "1234 5678");
    }

    #[test]
    fn format_code_display_other() {
        assert_eq!(format_code_display("A1B2C3"), "A1B2C3");
    }

    #[test]
    fn spanish_verification_code() {
        let result = extract_verification_code("", "Tu código de verificación es 482193", None);
        assert_eq!(
            result,
            Some(VerificationCode {
                code: "482193".to_string()
            })
        );
    }

    #[test]
    fn cjk_verification_code() {
        let result = extract_verification_code("", "您的验证码是 482193", None);
        assert_eq!(
            result,
            Some(VerificationCode {
                code: "482193".to_string()
            })
        );
    }

    #[test]
    fn microsoft_style_code_after_branding() {
        let branding = "Microsoft account privacy and security notice. ".repeat(8);
        let body = format!("{branding} Your code is 482193");
        let result = extract_verification_code(
            "Your AJM Software Group LLC account verification code",
            &body,
            None,
        );
        assert_eq!(
            result,
            Some(VerificationCode {
                code: "482193".to_string()
            })
        );
    }

    #[test]
    fn greenhouse_security_code_html() {
        let result = extract_verification_code(
            "Security code for your application to Archera",
            "",
            Some(
                "<p>Copy and paste this code into the security code field on your application:</p><h1>EQq1Pmy3</h1><p>After you enter the code, resubmit your application.</p>",
            ),
        );
        assert_eq!(
            result,
            Some(VerificationCode {
                code: "EQq1Pmy3".to_string()
            })
        );
    }

    #[test]
    fn unitedhealth_access_code_plaintext() {
        let result = extract_verification_code(
            "Multifactor authentication access code",
            "To better secure your account with UnitedHealth Group, multifactor authentication is enabled. Please use the following access code to log in to your account. Access Code: +MYK7Ahy3u",
            None,
        );
        assert_eq!(
            result,
            Some(VerificationCode {
                code: "+MYK7Ahy3u".to_string()
            })
        );
    }

    #[test]
    fn unitedhealth_access_code_html() {
        let result = extract_verification_code(
            "Multifactor authentication access code",
            "",
            Some(
                "<p>To better secure your account with UnitedHealth Group, multifactor authentication is enabled. Please use the following access code to log in to your account.</p><p>Access Code:<br /><strong>+MYK7Ahy3u</strong></p>",
            ),
        );
        assert_eq!(
            result,
            Some(VerificationCode {
                code: "+MYK7Ahy3u".to_string()
            })
        );
    }

    #[test]
    fn mixed_code_length_12() {
        let result = extract_verification_code("Security code", "Your code is Abcdef123456", None);
        assert_eq!(
            result,
            Some(VerificationCode {
                code: "Abcdef123456".to_string()
            })
        );
    }

    #[test]
    fn mixed_code_length_13_rejected() {
        assert!(
            extract_verification_code("Security code", "Your code is Abcdefg123456", None)
                .is_none()
        );
    }

    #[test]
    fn plus_on_digits_stays_off_code() {
        let result = extract_verification_code("", "Your verification code is +482193", None);
        assert_eq!(
            result,
            Some(VerificationCode {
                code: "482193".to_string()
            })
        );
        let result = extract_verification_code("", "Your verification code is +15551212", None);
        assert_eq!(
            result,
            Some(VerificationCode {
                code: "15551212".to_string()
            })
        );
    }

    #[test]
    fn format_code_display_greenhouse_and_unitedhealth() {
        assert_eq!(format_code_display("EQq1Pmy3"), "EQq1Pmy3");
        assert_eq!(format_code_display("+MYK7Ahy3u"), "+MYK7Ahy3u");
        assert_eq!(format_code_display("482193"), "482 193");
    }
}
