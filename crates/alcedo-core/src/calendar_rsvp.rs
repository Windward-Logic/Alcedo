//! iTIP REPLY builders and attendee participation updates.

use chrono::Utc;
use icalendar::{Calendar, Component, Event, Property};

use crate::calendar::CalendarEventDraft;
use crate::calendar::build_reply_vevent_core;
use crate::calendar_details::{Attendee, EventPerson, MAX_ATTENDEES, ParticipationStatus};

fn mailto(addr: &str) -> String {
    let a = addr.trim();
    if a.contains('@') && !a.contains(':') {
        format!("mailto:{a}")
    } else {
        a.to_string()
    }
}

/// Build a METHOD:REPLY calendar with one VEVENT per draft.
pub fn build_reply_ics(
    drafts: &[CalendarEventDraft],
    responder_email: &str,
    responder_name: Option<&str>,
    status: ParticipationStatus,
) -> String {
    let organizer_mail = drafts
        .iter()
        .find_map(|d| d.details.organizer.as_ref())
        .map(|o| mailto(&o.email))
        .unwrap_or_else(|| "mailto:unknown@invalid".into());

    let responder = mailto(responder_email);

    let mut calendar = Calendar::empty();
    calendar.append_property(Property::new("VERSION", "2.0"));
    calendar.append_property(Property::new("PRODID", "-//Alcedo//Calendar RSVP//EN"));
    calendar.append_property(Property::new("METHOD", "REPLY"));
    calendar.append_property(Property::new("CALSCALE", "GREGORIAN"));

    for draft in drafts {
        let base = build_reply_vevent_core(draft);
        let mut ev = Event::new();
        for p in base.properties().values() {
            let prop: icalendar::Property = p.clone();
            ev.append_property(prop);
        }
        ev.timestamp(Utc::now())
            .add_property("SEQUENCE", draft.details.sequence.to_string());

        let mut org = Property::new("ORGANIZER", organizer_mail.as_str());
        if let Some(n) = drafts
            .iter()
            .find_map(|d| d.details.organizer.as_ref())
            .and_then(|o| o.name.as_deref())
        {
            org.add_parameter("CN", n);
        }
        ev.append_property(org);

        let mut att = Property::new("ATTENDEE", responder.as_str());
        att.add_parameter("PARTSTAT", status.ics_value());
        if let Some(n) = responder_name {
            att.add_parameter("CN", n);
        }
        ev.append_property(att);

        calendar.push(ev.done());
    }

    calendar.to_string()
}

/// Set or append the responder's attendee line on a draft.
pub fn apply_participation(
    draft: &mut CalendarEventDraft,
    email: &str,
    name: Option<&str>,
    status: ParticipationStatus,
) {
    let e = email.trim();
    if e.is_empty() {
        return;
    }
    let lower = e.to_ascii_lowercase();
    if let Some(a) = draft
        .details
        .attendees
        .iter_mut()
        .find(|a| a.person.email.to_ascii_lowercase() == lower)
    {
        a.status = status;
        if name.is_some() {
            a.person.name = name.map(str::to_string);
        }
        return;
    }
    if draft.details.attendees.len() >= MAX_ATTENDEES {
        return;
    }
    draft.details.attendees.push(Attendee {
        person: EventPerson {
            email: e.to_string(),
            name: name.map(str::to_string),
        },
        status,
    });
}

/// Email subject for an RSVP reply.
pub fn reply_subject(status: ParticipationStatus, summary: &str) -> String {
    let prefix = match status {
        ParticipationStatus::Accepted => "Accepted",
        ParticipationStatus::Tentative => "Tentative",
        ParticipationStatus::Declined => "Declined",
        _ => "Accepted",
    };
    format!("{prefix}: {summary}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calendar_details::EventDetails;
    use chrono::TimeZone;

    #[test]
    fn reply_ics_has_method_reply_and_partstat() {
        let mut d = CalendarEventDraft {
            uid: "uid@x".into(),
            summary: "Meet".into(),
            location: Some("Here".into()),
            description: Some("Notes".into()),
            start: Utc.with_ymd_and_hms(2026, 8, 1, 15, 0, 0).unwrap(),
            end: Utc.with_ymd_and_hms(2026, 8, 1, 16, 0, 0).unwrap(),
            all_day: false,
            recurrence_id: Some(Utc.with_ymd_and_hms(2026, 8, 1, 15, 0, 0).unwrap()),
            rrule: None,
            source_message_id: None,
            cancelled: false,
            details: EventDetails::default(),
        };
        d.details.time_zone = Some("America/New_York".to_string());
        d.details.organizer = Some(EventPerson {
            email: "org@example.com".into(),
            name: Some("Org".into()),
        });
        let ics = build_reply_ics(
            &[d],
            "you@example.com",
            Some("You"),
            ParticipationStatus::Tentative,
        );
        assert!(ics.to_ascii_uppercase().contains("METHOD:REPLY"), "{ics}");
        assert!(ics.contains("PARTSTAT=TENTATIVE"), "{ics}");
        assert!(!ics.to_ascii_uppercase().contains("\nLOCATION"), "{ics}");
        assert!(!ics.to_ascii_uppercase().contains("\nDESCRIPTION"), "{ics}");
    }

    #[test]
    fn apply_participation_updates_or_appends() {
        let mut d = CalendarEventDraft {
            uid: "u".into(),
            summary: "S".into(),
            location: None,
            description: None,
            start: Utc::now(),
            end: Utc::now(),
            all_day: false,
            recurrence_id: None,
            rrule: None,
            source_message_id: None,
            cancelled: false,
            details: EventDetails::default(),
        };
        d.details.attendees.push(Attendee {
            person: EventPerson {
                email: "A@Example.COM".into(),
                name: None,
            },
            status: ParticipationStatus::NeedsAction,
        });
        apply_participation(&mut d, "a@example.com", None, ParticipationStatus::Accepted);
        assert_eq!(d.details.attendees[0].status, ParticipationStatus::Accepted);
        apply_participation(
            &mut d,
            "new@x.test",
            Some("N"),
            ParticipationStatus::Declined,
        );
        assert!(
            d.details
                .attendees
                .iter()
                .any(|a| a.person.email == "new@x.test")
        );
    }
}
