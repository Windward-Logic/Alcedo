//! Structured event details (organizer, attendees, reminders, join URL, etc.).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Person identified by email with optional display name.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventPerson {
    pub email: String,
    pub name: Option<String>,
}

/// [`ATTENDEE`] participation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ParticipationStatus {
    #[default]
    NeedsAction,
    Accepted,
    Declined,
    Tentative,
    Delegated,
}

impl ParticipationStatus {
    pub fn ics_value(self) -> &'static str {
        match self {
            ParticipationStatus::NeedsAction => "NEEDS-ACTION",
            ParticipationStatus::Accepted => "ACCEPTED",
            ParticipationStatus::Declined => "DECLINED",
            ParticipationStatus::Tentative => "TENTATIVE",
            ParticipationStatus::Delegated => "DELEGATED",
        }
    }

    pub fn from_ics(value: &str) -> Self {
        let v = value.trim().to_ascii_uppercase();
        match v.as_str() {
            "ACCEPTED" => Self::Accepted,
            "DECLINED" => Self::Declined,
            "TENTATIVE" => Self::Tentative,
            "DELEGATED" => Self::Delegated,
            _ => Self::NeedsAction,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attendee {
    pub person: EventPerson,
    pub status: ParticipationStatus,
}

/// Maximum attendees stored per event.
pub const MAX_ATTENDEES: usize = 200;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EventDetails {
    pub time_zone: Option<String>,
    pub organizer: Option<EventPerson>,
    pub attendees: Vec<Attendee>,
    /// Minutes before start, sorted ascending, unique.
    pub reminders: Vec<u32>,
    pub join_url: Option<String>,
    pub url: Option<String>,
    pub tentative: bool,
    pub busy: bool,
    pub sequence: u32,
    pub exdates: Vec<DateTime<Utc>>,
    pub rdates: Vec<DateTime<Utc>>,
}

impl Default for EventDetails {
    fn default() -> Self {
        Self {
            time_zone: None,
            organizer: None,
            attendees: Vec::new(),
            reminders: Vec::new(),
            join_url: None,
            url: None,
            tentative: false,
            busy: true,
            sequence: 0,
            exdates: Vec::new(),
            rdates: Vec::new(),
        }
    }
}

fn strip_trailing_punct(mut s: String) -> String {
    while let Some(c) = s.chars().last() {
        if matches!(c, '.' | ',' | ';' | ')' | '>') {
            s.pop();
        } else {
            break;
        }
    }
    s
}

fn host_matches_join(url: &url::Url, host: &str) -> bool {
    url.host_str()
        .is_some_and(|h| h.eq_ignore_ascii_case(host) || h.ends_with(&format!(".{host}")))
}

fn is_join_meeting_host(url: &url::Url) -> bool {
    const HOSTS: &[&str] = &[
        "zoom.us",
        "meet.google.com",
        "teams.microsoft.com",
        "teams.live.com",
        "webex.com",
        "whereby.com",
    ];
    HOSTS.iter().any(|h| host_matches_join(url, h))
}

/// Join URL from explicit properties first, then `https` in location/description.
pub fn find_join_url(
    explicit: Option<&str>,
    location: Option<&str>,
    description: Option<&str>,
) -> Option<String> {
    if let Some(raw) = explicit
        && let Some(u) = try_https_url(raw)
    {
        return Some(u);
    }
    for text in [location, description].into_iter().flatten() {
        if let Some(u) = first_join_url_in_text(text) {
            return Some(u);
        }
    }
    None
}

fn try_https_url(raw: &str) -> Option<String> {
    let t = raw.trim();
    let u = url::Url::parse(t).ok()?;
    if u.scheme() != "https" {
        return None;
    }
    Some(strip_trailing_punct(u.to_string()))
}

fn first_join_url_in_text(text: &str) -> Option<String> {
    for token in text.split_whitespace() {
        let t = token.trim_matches(|c| matches!(c, '<' | '>' | '"' | '(' | '['));
        if !t
            .get(..8)
            .is_some_and(|p| p.eq_ignore_ascii_case("https://"))
        {
            continue;
        }
        if let Some(u) = try_https_url(t)
            && let Ok(parsed) = url::Url::parse(&u)
            && is_join_meeting_host(&parsed)
        {
            return Some(u);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_url_source_order() {
        assert_eq!(
            find_join_url(
                Some("https://meet.google.com/abc-def"),
                Some("https://zoom.us/j/1"),
                None
            )
            .as_deref(),
            Some("https://meet.google.com/abc-def")
        );
        assert_eq!(
            find_join_url(
                None,
                Some("Room A https://zoom.us/j/99"),
                Some("https://teams.microsoft.com/l/meetup-join/xyz")
            )
            .as_deref(),
            Some("https://zoom.us/j/99")
        );
        let with_trail =
            find_join_url(Some("https://meet.google.com/x,"), None, None).expect("url");
        assert_eq!(with_trail, "https://meet.google.com/x");
    }
}
