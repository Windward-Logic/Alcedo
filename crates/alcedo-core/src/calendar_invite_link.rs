//! Calendar invite URI detection and Google Calendar template URL parsing.

use crate::calendar::marker_midnight_utc;
use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};
use uuid::Uuid;

use crate::CalendarEventDraft;
use crate::calendar_details::EventDetails;

/// True when a URI points at a calendar invitation (ICS, webcal, or Google template).
pub fn is_calendar_invite_uri(uri: &str) -> bool {
    let uri = uri.trim();
    if uri.is_empty() || uri.starts_with("mailto:") {
        return false;
    }

    let lower = uri.to_ascii_lowercase();
    if lower.starts_with("webcal:") || lower.starts_with("webcals:") {
        return true;
    }

    let path_part = uri.split(['?', '#']).next().unwrap_or(uri);
    if path_part.to_ascii_lowercase().ends_with(".ics") {
        return true;
    }

    if lower.starts_with("http://") || lower.starts_with("https://") {
        return is_google_calendar_template(uri);
    }

    false
}

pub(crate) fn is_google_calendar_template(uri: &str) -> bool {
    let Some(rest) = uri.split("://").nth(1) else {
        return false;
    };
    let host_end = rest.find('/').unwrap_or(rest.len());
    let host = rest[..host_end].to_ascii_lowercase();
    if host != "calendar.google.com" && host != "www.google.com" {
        return false;
    }
    let path = if host_end < rest.len() {
        rest[host_end..].split(['?', '#']).next().unwrap_or("")
    } else {
        ""
    };
    if !path.starts_with("/calendar/") {
        return false;
    }
    let query = rest.split('?').nth(1).unwrap_or("");
    query.split('&').any(|pair| {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        key.eq_ignore_ascii_case("action") && value.eq_ignore_ascii_case("TEMPLATE")
    })
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && index + 2 < bytes.len()
            && let Ok(byte) = u8::from_str_radix(
                std::str::from_utf8(&bytes[index + 1..index + 3]).unwrap_or(""),
                16,
            )
        {
            out.push(byte);
            index += 3;
            continue;
        }
        if bytes[index] == b'+' {
            out.push(b' ');
            index += 1;
            continue;
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn query_param(query: &str, name: &str) -> Option<String> {
    for pair in query.split('&') {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        if key.eq_ignore_ascii_case(name) {
            return Some(percent_decode(value));
        }
    }
    None
}

fn parse_google_dates(dates: &str) -> Option<(DateTime<Utc>, DateTime<Utc>, bool)> {
    let (start_str, end_str) = dates.split_once('/')?;
    if start_str.len() == 8 && end_str.len() == 8 {
        let start_date = NaiveDate::parse_from_str(start_str, "%Y%m%d").ok()?;
        let end_date = NaiveDate::parse_from_str(end_str, "%Y%m%d").ok()?;
        return Some((
            marker_midnight_utc(start_date),
            marker_midnight_utc(end_date),
            true,
        ));
    }
    if start_str.ends_with('Z') && end_str.ends_with('Z') {
        let start = NaiveDateTime::parse_from_str(start_str, "%Y%m%dT%H%M%SZ")
            .ok()?
            .and_utc();
        let end = NaiveDateTime::parse_from_str(end_str, "%Y%m%dT%H%M%SZ")
            .ok()?
            .and_utc();
        return Some((start, end, false));
    }
    None
}

/// Build an event draft from a Google Calendar template URL.
pub fn draft_from_google_template_url(uri: &str) -> Option<CalendarEventDraft> {
    if !is_google_calendar_template(uri) {
        return None;
    }
    let query = uri.split('?').nth(1).unwrap_or("");
    let dates = query_param(query, "dates")?;
    let (start, end, all_day) = parse_google_dates(&dates)?;

    let summary = query_param(query, "text")
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| "Appointment".to_string());

    let location = query_param(query, "location")
        .map(|location| location.trim().to_string())
        .filter(|location| !location.is_empty());

    let description = query_param(query, "details")
        .map(|details| details.trim().to_string())
        .filter(|details| !details.is_empty());

    Some(CalendarEventDraft {
        uid: format!("{}@alcedo.mail", Uuid::new_v4()),
        summary,
        location,
        description,
        start,
        end,
        all_day,
        recurrence_id: None,
        rrule: None,
        source_message_id: None,
        cancelled: false,
        details: EventDetails::default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn is_calendar_invite_uri_detects_invite_links() {
        assert!(is_calendar_invite_uri("webcal://cal.example/x.ics"));
        assert!(is_calendar_invite_uri(
            "https://host/AddToCalendar.ics?foo=bar"
        ));
        assert!(is_calendar_invite_uri(
            "https://calendar.google.com/calendar/render?action=TEMPLATE&text=A&dates=20261013T184500Z/20261013T191500Z"
        ));
        assert!(!is_calendar_invite_uri("https://maps.google.com/?q=1"));
        assert!(!is_calendar_invite_uri("mailto:a@b.c"));
        assert!(!is_calendar_invite_uri(""));
    }

    #[test]
    fn draft_from_google_template_url_parses_fields() {
        let uri = "https://calendar.google.com/calendar/render?action=TEMPLATE&text=Service%20visit&dates=20261013T184500Z/20261013T191500Z&location=Shop&details=Bring%20ID";
        let draft = draft_from_google_template_url(uri).expect("draft");
        assert_eq!(draft.summary, "Service visit");
        assert_eq!(draft.location.as_deref(), Some("Shop"));
        assert_eq!(draft.description.as_deref(), Some("Bring ID"));
        assert!(!draft.all_day);
        assert_eq!(
            draft.start,
            Utc.with_ymd_and_hms(2026, 10, 13, 18, 45, 0).unwrap()
        );
        assert_eq!(
            draft.end,
            Utc.with_ymd_and_hms(2026, 10, 13, 19, 15, 0).unwrap()
        );
        assert!(draft.uid.ends_with("@alcedo.mail"));
    }

    #[test]
    fn draft_from_google_template_url_decodes_plus_as_space() {
        let uri = "https://calendar.google.com/calendar/render?action=TEMPLATE&text=Service+visit&dates=20261013T184500Z/20261013T191500Z&location=Main+St&details=Bring+ID";
        let draft = draft_from_google_template_url(uri).expect("draft");
        assert_eq!(draft.summary, "Service visit");
        assert_eq!(draft.location.as_deref(), Some("Main St"));
        assert_eq!(draft.description.as_deref(), Some("Bring ID"));

        let uri = "https://calendar.google.com/calendar/render?action=TEMPLATE&text=Service%2Bvisit&dates=20261013T184500Z/20261013T191500Z";
        let draft = draft_from_google_template_url(uri).expect("draft");
        assert_eq!(draft.summary, "Service+visit");
    }

    #[test]
    fn draft_from_google_template_url_all_day() {
        let uri =
            "https://calendar.google.com/calendar/render?action=TEMPLATE&dates=20261013/20261014";
        let draft = draft_from_google_template_url(uri).expect("draft");
        assert!(draft.all_day);
    }

    #[test]
    fn draft_from_google_template_url_missing_dates() {
        let uri = "https://calendar.google.com/calendar/render?action=TEMPLATE&text=No%20dates";
        assert!(draft_from_google_template_url(uri).is_none());
    }
}
