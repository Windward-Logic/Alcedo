//! Calendar invitation types and ICS parsing.

use chrono::{DateTime, Duration, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

use crate::calendar_details::EventDetails;
use crate::ids::MessageId;

mod calendar_parse;

pub use calendar_parse::{
    build_ics, build_vevent_from_draft, parse_ics, parse_ics_with_method, recurrence_id_equals,
};
pub(crate) use calendar_parse::{build_reply_vevent_core, parse_ics_iso_duration};

use crate::attachment::extension_of;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CalendarEventDraft {
    pub uid: String,
    pub summary: String,
    pub location: Option<String>,
    pub description: Option<String>,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub all_day: bool,
    pub recurrence_id: Option<DateTime<Utc>>,
    pub rrule: Option<String>,
    pub source_message_id: Option<MessageId>,
    pub cancelled: bool,
    pub details: EventDetails,
}

impl CalendarEventDraft {
    pub fn all_day_dates(&self) -> (NaiveDate, NaiveDate) {
        all_day_dates_for(self.start, self.end, self.all_day)
    }
}

fn all_day_dates_for(
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    all_day: bool,
) -> (NaiveDate, NaiveDate) {
    let sd = start.naive_utc().date();
    if !all_day {
        return (sd, end.naive_utc().date());
    }
    let ed = end.naive_utc().date();
    let exclusive = if ed > sd { ed } else { sd + Duration::days(1) };
    (sd, exclusive)
}

pub(crate) fn marker_midnight_utc(date: NaiveDate) -> DateTime<Utc> {
    date.and_hms_opt(0, 0, 0).expect("valid").and_utc()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InviteMethod {
    Request,
    Publish,
    Cancel,
    Reply,
    Other,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ParsedInvite {
    pub method: InviteMethod,
    pub drafts: Vec<CalendarEventDraft>,
}

/// Returns true when a MIME part looks like a calendar invitation.
pub fn is_calendar_part(mime: &str, filename: &str) -> bool {
    let mime = mime.trim().to_ascii_lowercase();
    if matches!(
        mime.as_str(),
        "text/calendar" | "application/ics" | "application/x-ics"
    ) {
        return true;
    }
    extension_of(filename).is_some_and(|ext| ext == "ics")
}

#[cfg(test)]
mod tests {
    use super::calendar_parse::parse_ics_in;
    use super::*;
    use crate::calendar_details::EventDetails;
    use chrono::TimeZone;
    use chrono_tz::America::New_York;

    const ICS_HEADER: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Alcedo//Test//EN\r\n";

    fn wrap_vevent(body: &str) -> String {
        format!("{ICS_HEADER}BEGIN:VEVENT\r\n{body}END:VEVENT\r\nEND:VCALENDAR\r\n")
    }

    #[test]
    fn is_calendar_part_mime_and_extension() {
        assert!(is_calendar_part("application/ics", "attachment"));
        assert!(is_calendar_part("text/plain", "invite.ICS"));
        assert!(!is_calendar_part("text/plain", "notes.txt"));
    }

    #[test]
    fn parse_folded_summary() {
        let ics = wrap_vevent(
            "UID:test-folded@alcedo\r\nSUMMARY:Design\r\n  review\r\nDTSTART:20250821T150000Z\r\nDTEND:20250821T160000Z\r\n",
        );
        let events = parse_ics(ics.as_bytes()).expect("parse");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].summary, "Design review");
    }

    #[test]
    fn parse_utc_z_suffix() {
        let ics = wrap_vevent(
            "UID:test-utc@alcedo\r\n\
             SUMMARY:UTC meeting\r\n\
             DTSTART:20250821T150000Z\r\n\
             DTEND:20250821T160000Z\r\n",
        );
        let events = parse_ics(ics.as_bytes()).expect("parse");
        assert!(!events[0].all_day);
        assert!(events[0].details.time_zone.is_none());
        assert_eq!(
            events[0].start,
            Utc.with_ymd_and_hms(2025, 8, 21, 15, 0, 0).unwrap()
        );
    }

    #[test]
    fn parse_all_day_value_date() {
        let ics = wrap_vevent(
            "UID:test-allday@alcedo\r\n\
             SUMMARY:Holiday\r\n\
             DTSTART;VALUE=DATE:20250821\r\n\
             DTEND;VALUE=DATE:20250822\r\n",
        );
        let events = parse_ics(ics.as_bytes()).expect("parse");
        assert!(events[0].all_day);
        let (s, e) = events[0].all_day_dates();
        assert_eq!(s, NaiveDate::from_ymd_opt(2025, 8, 21).unwrap());
        assert_eq!(e, NaiveDate::from_ymd_opt(2025, 8, 22).unwrap());
        assert!(events[0].details.time_zone.is_none());
    }

    #[test]
    fn all_day_value_date_is_utc_marker() {
        let ics = wrap_vevent(
            "UID:ad@alcedo\r\nSUMMARY:A\r\n\
             DTSTART;VALUE=DATE:20250821\r\nDTEND;VALUE=DATE:20250822\r\n",
        );
        let d = parse_ics(ics.as_bytes()).expect("parse");
        assert_eq!(
            d[0].start,
            NaiveDate::from_ymd_opt(2025, 8, 21)
                .unwrap()
                .and_hms_opt(0, 0, 0)
                .unwrap()
                .and_utc()
        );
    }

    #[test]
    fn tzid_chicago_invite_is_16z_regardless_of_viewer() {
        let ics = wrap_vevent(
            "UID:chi@alcedo\r\nSUMMARY:C\r\n\
             DTSTART;TZID=America/Chicago:20261102T100000\r\n\
             DTEND;TZID=America/Chicago:20261102T110000\r\n",
        );
        let paris = parse_ics_in(ics.as_bytes(), &chrono_tz::Europe::Paris).expect("parse");
        let ny = parse_ics_in(ics.as_bytes(), &New_York).expect("parse");
        assert_eq!(paris.drafts[0].start, ny.drafts[0].start);
        assert_eq!(
            paris.drafts[0].start,
            Utc.with_ymd_and_hms(2026, 11, 2, 16, 0, 0).unwrap()
        );
        assert_eq!(
            paris.drafts[0].details.time_zone.as_deref(),
            Some("America/Chicago")
        );
    }

    #[test]
    fn tzid_london_during_us_dst_gap_week() {
        let ics = wrap_vevent(
            "UID:lon@alcedo\r\nSUMMARY:L\r\n\
             DTSTART;TZID=Europe/London:20261029T150000\r\n\
             DTEND;TZID=Europe/London:20261029T160000\r\n",
        );
        let d = parse_ics_in(ics.as_bytes(), &New_York).expect("parse");
        assert_eq!(
            d.drafts[0].start,
            Utc.with_ymd_and_hms(2026, 10, 29, 15, 0, 0).unwrap()
        );
    }

    #[test]
    fn ambiguous_hour_invite_parses() {
        let ics = wrap_vevent(
            "UID:amb@alcedo\r\nSUMMARY:A\r\n\
             DTSTART;TZID=America/New_York:20261101T013000\r\n\
             DTEND;TZID=America/New_York:20261101T023000\r\n",
        );
        let d = parse_ics_in(ics.as_bytes(), &chrono_tz::Europe::London).expect("parse");
        assert_eq!(
            d.drafts[0].start,
            Utc.with_ymd_and_hms(2026, 11, 1, 5, 30, 0).unwrap()
        );
    }

    #[test]
    fn time_zone_name_set_only_for_named_zone() {
        let ics_z =
            wrap_vevent("UID:z@alcedo\r\nDTSTART:20250821T150000Z\r\nDTEND:20250821T160000Z\r\n");
        assert!(
            parse_ics(ics_z.as_bytes()).unwrap()[0]
                .details
                .time_zone
                .is_none()
        );

        let ics_float =
            wrap_vevent("UID:f@alcedo\r\nDTSTART:20250821T150000\r\nDTEND:20250821T160000\r\n");
        assert!(
            parse_ics(ics_float.as_bytes()).unwrap()[0]
                .details
                .time_zone
                .is_none()
        );
    }

    #[test]
    fn parse_recurrence_id_compact_datetime() {
        let ics = wrap_vevent(
            "UID:series@alcedo\r\n\
             RECURRENCE-ID:20250821T150000Z\r\n\
             DTSTART:20250821T160000Z\r\n\
             DTEND:20250821T170000Z\r\n\
             SUMMARY:Exception\r\n",
        );
        let events = parse_ics(ics.as_bytes()).expect("parse");
        assert_eq!(
            events[0].recurrence_id,
            Some(Utc.with_ymd_and_hms(2025, 8, 21, 15, 0, 0).unwrap())
        );
    }

    #[test]
    fn parse_status_cancelled_sets_cancelled() {
        let ics = wrap_vevent(
            "UID:cancelled@alcedo\r\nSTATUS:CANCELLED\r\n\
             DTSTART:20250821T150000Z\r\nDTEND:20250821T160000Z\r\n",
        );
        assert!(parse_ics(ics.as_bytes()).unwrap()[0].cancelled);
    }

    #[test]
    fn build_ics_cancelled_includes_status() {
        let draft = CalendarEventDraft {
            uid: "cancelled@alcedo".into(),
            summary: "Cancelled".into(),
            location: None,
            description: None,
            start: Utc.with_ymd_and_hms(2025, 8, 21, 15, 0, 0).unwrap(),
            end: Utc.with_ymd_and_hms(2025, 8, 21, 16, 0, 0).unwrap(),
            all_day: false,
            recurrence_id: None,
            rrule: None,
            source_message_id: None,
            cancelled: true,
            details: EventDetails::default(),
        };
        let ics = build_ics(&draft);
        assert!(ics.contains("STATUS:CANCELLED"));
    }

    #[test]
    fn all_day_exdate_value_date_is_utc_marker() {
        let ics = format!(
            "{ICS_HEADER}BEGIN:VEVENT\r\nUID:ad-ex@alcedo\r\nSUMMARY:A\r\n\
             DTSTART;VALUE=DATE:20250821\r\nDTEND;VALUE=DATE:20250822\r\n\
             EXDATE;VALUE=DATE:20250821\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
        );
        let paris = parse_ics_in(ics.as_bytes(), &chrono_tz::Europe::Paris).expect("parse");
        let ny = parse_ics_in(ics.as_bytes(), &New_York).expect("parse");
        let marker = Utc.with_ymd_and_hms(2025, 8, 21, 0, 0, 0).unwrap();
        assert_eq!(paris.drafts[0].details.exdates, vec![marker]);
        assert_eq!(ny.drafts[0].details.exdates, vec![marker]);
    }

    #[test]
    fn build_ics_all_day_exdate_is_value_date() {
        let marker = Utc.with_ymd_and_hms(2025, 8, 21, 0, 0, 0).unwrap();
        let mut details = EventDetails::default();
        details.exdates.push(marker);
        let draft = CalendarEventDraft {
            uid: "ad-w@alcedo".into(),
            summary: "All day".into(),
            location: None,
            description: None,
            start: marker,
            end: marker + Duration::days(1),
            all_day: true,
            recurrence_id: None,
            rrule: None,
            source_message_id: None,
            cancelled: false,
            details,
        };
        let ics = build_ics(&draft);
        assert!(ics.contains("EXDATE;VALUE=DATE:20250821"));
        assert!(!ics.contains("EXDATE:20250821T000000Z"));
    }

    #[test]
    fn exdate_custom_tzid_uses_vtimezone() {
        let body = "UID:ex-vtz@alcedo\r\nSUMMARY:X\r\n\
             DTSTART:20260715T120000Z\r\nDTEND:20260715T130000Z\r\n\
             EXDATE;TZID=Custom-Test:20260715T100000\r\n";
        let vtz = "BEGIN:VTIMEZONE\r\n\
            TZID:Custom-Test\r\n\
            BEGIN:STANDARD\r\n\
            DTSTART:20071104T020000\r\n\
            RRULE:FREQ=YEARLY;BYMONTH=11;BYDAY=1SU\r\n\
            TZOFFSETFROM:-0400\r\n\
            TZOFFSETTO:-0500\r\n\
            END:STANDARD\r\n\
            BEGIN:DAYLIGHT\r\n\
            DTSTART:20070311T020000\r\n\
            RRULE:FREQ=YEARLY;BYMONTH=3;BYDAY=2SU\r\n\
            TZOFFSETFROM:-0500\r\n\
            TZOFFSETTO:-0400\r\n\
            END:DAYLIGHT\r\n\
            END:VTIMEZONE\r\n";
        let ics = format!("{ICS_HEADER}{vtz}BEGIN:VEVENT\r\n{body}END:VEVENT\r\nEND:VCALENDAR\r\n");
        let expected = Utc.with_ymd_and_hms(2026, 7, 15, 14, 0, 0).unwrap();
        let paris = parse_ics_in(ics.as_bytes(), &chrono_tz::Europe::Paris).expect("parse");
        let ny = parse_ics_in(ics.as_bytes(), &New_York).expect("parse");
        assert_eq!(paris.drafts[0].details.exdates, vec![expected]);
        assert_eq!(ny.drafts[0].details.exdates, vec![expected]);
    }

    /// An invite arrives in ordinary mail, so its values are hostile input:
    /// absurd durations must be a parse error, never a panic.
    #[test]
    fn oversized_ics_duration_is_rejected_without_panicking() {
        for duration in [
            "P99999999999999999W",
            "P9223372036854775807D",
            "PT9223372036854775807S",
            "P106751991167D",
            "P5000000000W",
        ] {
            let ics = format!(
                "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:x@example\r\n\
                 DTSTART:20250821T150000Z\r\nDURATION:{duration}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
            );
            assert!(parse_ics(ics.as_bytes()).is_err(), "{duration}");
        }
    }

    #[test]
    fn duration_replaces_missing_dtend_coverage() {
        let ics = format!(
            "{ICS_HEADER}BEGIN:VEVENT\r\nUID:d@alcedo\r\n\
             DTSTART:20250821T150000Z\r\nDURATION:PT90M\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
        );
        let d = parse_ics(ics.as_bytes()).expect("parse");
        assert_eq!(d[0].end, d[0].start + Duration::minutes(90));
    }
}
