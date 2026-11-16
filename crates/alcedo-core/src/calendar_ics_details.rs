//! Read and write extended ICS properties on `VEVENT`.

use chrono::{DateTime, NaiveDate, NaiveDateTime, TimeZone, Utc};
use icalendar::{
    CalendarDateTime, Component, DatePerhapsTime, Event, EventLike, Property, ValueType,
};

use crate::CalendarEventDraft;
use crate::calendar::{marker_midnight_utc, parse_ics_iso_duration};
use crate::calendar_details::{
    Attendee, EventDetails, EventPerson, MAX_ATTENDEES, ParticipationStatus, find_join_url,
};
use crate::calendar_zone::{ZoneResolver, wall_to_utc};

fn normalize_email(raw: &str) -> String {
    let t = raw.trim();
    let rest = t
        .strip_prefix("mailto:")
        .or_else(|| t.strip_prefix("MAILTO:"))
        .unwrap_or(t);
    rest.split(';')
        .next()
        .unwrap_or(rest)
        .trim()
        .to_ascii_lowercase()
}

fn person_from_organizer_prop(prop: &Property) -> EventPerson {
    let email = normalize_email(prop.value());
    let name = prop
        .params()
        .get("CN")
        .map(|p| p.value().trim().to_string())
        .filter(|s| !s.is_empty());
    EventPerson { email, name }
}

fn attendee_from_property(prop: &Property) -> Option<Attendee> {
    let email = normalize_email(prop.value());
    if email.is_empty() {
        return None;
    }
    let name = prop
        .params()
        .get("CN")
        .map(|p| p.value().trim().to_string())
        .filter(|s| !s.is_empty());
    let status = prop
        .params()
        .get("PARTSTAT")
        .map(|p| ParticipationStatus::from_ics(p.value()))
        .unwrap_or_default();
    Some(Attendee {
        person: EventPerson { email, name },
        status,
    })
}

fn parse_duration_minutes_abs(trigger: &str) -> Option<u32> {
    let s = trigger.trim();
    if s.is_empty() {
        return None;
    }
    let (neg, body) = if let Some(rest) = s.strip_prefix('-') {
        (true, rest)
    } else {
        (false, s)
    };
    let dur = parse_ics_iso_duration(body)?;
    let secs = dur.num_seconds().unsigned_abs();
    if secs == 0 {
        return Some(0);
    }
    if !neg {
        return None;
    }
    let mins = secs / 60;
    Some(u32::try_from(mins).unwrap_or(0))
}

fn parse_exr_instant<Z: TimeZone>(
    token: &str,
    prop: &Property,
    is_date_only: bool,
    all_day: bool,
    resolver: &ZoneResolver,
    event_zone: Option<&crate::calendar_zone::ResolvedZone>,
    floating_zone: &Z,
) -> Option<DateTime<Utc>> {
    let t = token.trim();
    if is_date_only {
        let d = NaiveDate::parse_from_str(t, "%Y%m%d").ok()?;
        if all_day {
            return Some(marker_midnight_utc(d));
        }
        let naive = d.and_hms_opt(0, 0, 0)?;
        return Some(resolve_wall(
            naive,
            prop,
            resolver,
            event_zone,
            floating_zone,
        ));
    }
    if t.ends_with('Z') {
        let naive = NaiveDateTime::parse_from_str(t.trim_end_matches('Z'), "%Y%m%dT%H%M%S").ok()?;
        return Some(naive.and_utc());
    }
    let naive = NaiveDateTime::parse_from_str(t, "%Y%m%dT%H%M%S").ok()?;
    Some(resolve_wall(
        naive,
        prop,
        resolver,
        event_zone,
        floating_zone,
    ))
}

fn resolve_wall<Z: TimeZone>(
    naive: NaiveDateTime,
    prop: &Property,
    resolver: &ZoneResolver,
    event_zone: Option<&crate::calendar_zone::ResolvedZone>,
    floating_zone: &Z,
) -> DateTime<Utc> {
    if let Some(tzid) = prop.params().get("TZID") {
        let id = tzid.value().trim();
        if let Some(r) = resolver.resolve_sender_zone(id) {
            return r.wall_to_utc(naive);
        }
    }
    if let Some(z) = event_zone {
        z.wall_to_utc(naive)
    } else {
        wall_to_utc(naive, floating_zone)
    }
}

fn merge_sorted_unique_minutes(vec: &mut Vec<u32>) {
    vec.sort_unstable();
    vec.dedup();
}

fn collect_multi_or_single(event: &Event, key: &str, mut f: impl FnMut(&Property)) {
    if let Some(v) = event.multi_properties().get(key) {
        for p in v {
            f(p);
        }
    } else if let Some(p) = event.properties().get(key) {
        f(p);
    }
}

fn push_exdates_like<Z: TimeZone>(
    prop: &Property,
    out: &mut Vec<DateTime<Utc>>,
    all_day: bool,
    resolver: &ZoneResolver,
    event_zone: Option<&crate::calendar_zone::ResolvedZone>,
    floating_zone: &Z,
) {
    let is_date = prop.value_type() == Some(ValueType::Date);
    for token in prop.value().split(',') {
        if let Some(dt) = parse_exr_instant(
            token,
            prop,
            is_date,
            all_day,
            resolver,
            event_zone,
            floating_zone,
        ) {
            out.push(dt);
        }
    }
}

fn alarm_minutes_before_start<Z: TimeZone, C: Component>(
    alarm: &C,
    event_start: DateTime<Utc>,
    resolver: &ZoneResolver,
    event_wall_zone: Option<&crate::calendar_zone::ResolvedZone>,
    floating_zone: &Z,
) -> Option<u32> {
    let trigger_prop = alarm.properties().get("TRIGGER")?;
    if trigger_prop
        .params()
        .get("RELATED")
        .is_some_and(|r| r.value().eq_ignore_ascii_case("END"))
    {
        return None;
    }
    let raw = trigger_prop.value();
    let trimmed = raw.trim();
    if trimmed.ends_with('Z')
        || trigger_prop.value_type() == Some(ValueType::DateTime)
        || trigger_prop.params().get("TZID").is_some()
        || trigger_prop
            .params()
            .get("VALUE")
            .is_some_and(|v| v.value().eq_ignore_ascii_case("DATE-TIME"))
    {
        let p = DatePerhapsTime::from_property(trigger_prop)?;
        let dt = match p {
            DatePerhapsTime::Date(d) => d.and_hms_opt(0, 0, 0)?.and_utc(),
            DatePerhapsTime::DateTime(ct) => match ct {
                CalendarDateTime::Utc(u) => u,
                CalendarDateTime::Floating(n) => {
                    if let Some(z) = event_wall_zone {
                        z.wall_to_utc(n)
                    } else {
                        wall_to_utc(n, floating_zone)
                    }
                }
                CalendarDateTime::WithTimezone { date_time, tzid } => {
                    if let Some(r) = resolver.resolve_sender_zone(&tzid) {
                        r.wall_to_utc(date_time)
                    } else if let Some(z) = event_wall_zone {
                        z.wall_to_utc(date_time)
                    } else {
                        wall_to_utc(date_time, floating_zone)
                    }
                }
            },
        };
        if event_start >= dt {
            let mins = event_start.signed_duration_since(dt).num_minutes();
            return u32::try_from(mins).ok();
        }
        return None;
    }
    parse_duration_minutes_abs(trimmed)
}

/// Read extended fields into [`EventDetails`].
pub(crate) fn read_details<Z: TimeZone>(
    event: &Event,
    resolver: &ZoneResolver,
    start_zone_name: Option<String>,
    event_wall_zone: Option<&crate::calendar_zone::ResolvedZone>,
    floating_zone: &Z,
    event_start: DateTime<Utc>,
    all_day: bool,
) -> EventDetails {
    let mut details = EventDetails::default();

    if let Some(o) = event.properties().get("ORGANIZER") {
        details.organizer = Some(person_from_organizer_prop(o));
    }

    let mut count = 0usize;
    if let Some(list) = event.multi_properties().get("ATTENDEE") {
        for prop in list {
            if count >= MAX_ATTENDEES {
                break;
            }
            if let Some(a) = attendee_from_property(prop) {
                details.attendees.push(a);
                count += 1;
            }
        }
    } else if let Some(prop) = event.properties().get("ATTENDEE")
        && count < MAX_ATTENDEES
        && let Some(a) = attendee_from_property(prop)
    {
        details.attendees.push(a);
    }

    for child in event.components() {
        if child.component_kind() == "VALARM"
            && let Some(m) = alarm_minutes_before_start(
                child,
                event_start,
                resolver,
                event_wall_zone,
                floating_zone,
            )
        {
            details.reminders.push(m);
        }
    }
    merge_sorted_unique_minutes(&mut details.reminders);

    if let Some(u) = event.property_value("URL") {
        let t = u.trim();
        if !t.is_empty() {
            details.url = Some(t.to_string());
        }
    }

    let mut conference_explicit: Option<&str> = None;
    if let Some(c) = event.property_value("CONFERENCE") {
        let t = c.trim();
        if t.starts_with("https://") {
            conference_explicit = Some(t);
        }
    }
    if conference_explicit.is_none()
        && let Some(c) = event.property_value("X-GOOGLE-CONFERENCE")
    {
        let t = c.trim();
        if t.starts_with("https://") {
            conference_explicit = Some(t);
        }
    }
    if conference_explicit.is_none()
        && let Some(c) = event.property_value("X-MICROSOFT-SKYPETEAMSMEETINGURL")
    {
        let t = c.trim();
        if t.starts_with("https://") {
            conference_explicit = Some(t);
        }
    }
    if conference_explicit.is_none() {
        for (k, p) in event.properties() {
            if k.to_ascii_lowercase()
                .starts_with("x-microsoft-onlinemeeting")
            {
                let t = p.value().trim();
                if t.starts_with("https://") {
                    conference_explicit = Some(t);
                    break;
                }
            }
        }
    }

    details.join_url = find_join_url(
        conference_explicit,
        event.get_location().map(str::trim),
        event.get_description().map(str::trim),
    );

    if event.get_status() == Some(icalendar::EventStatus::Tentative) {
        details.tentative = true;
    }

    if let Some(transp) = event.property_value("TRANSP")
        && transp.eq_ignore_ascii_case("TRANSPARENT")
    {
        details.busy = false;
    }

    if let Some(seq) = event.property_value("SEQUENCE")
        && let Ok(n) = seq.trim().parse::<u32>()
    {
        details.sequence = n;
    }

    details.time_zone = start_zone_name;

    collect_multi_or_single(event, "EXDATE", |prop| {
        push_exdates_like(
            prop,
            &mut details.exdates,
            all_day,
            resolver,
            event_wall_zone,
            floating_zone,
        )
    });
    collect_multi_or_single(event, "RDATE", |prop| {
        push_exdates_like(
            prop,
            &mut details.rdates,
            all_day,
            resolver,
            event_wall_zone,
            floating_zone,
        )
    });

    details
}

fn append_multi(event: &mut Event, key: &str, val: &str) {
    event.append_multi_property(Property::new(key, val));
}

/// Write organizer, attendees, alarms, URL, CONFERENCE, sequence, transp, status, exdate, rdate.
pub(crate) fn write_details(event: &mut Event, draft: &CalendarEventDraft) {
    let d = &draft.details;

    if let Some(org) = &d.organizer {
        let mail = if org.email.contains('@') && !org.email.contains(':') {
            format!("mailto:{}", org.email)
        } else {
            org.email.clone()
        };
        let mut p = Property::new("ORGANIZER", mail);
        if let Some(n) = &org.name {
            p.add_parameter("CN", n);
        }
        event.append_property(p.done());
    }

    for att in &d.attendees {
        let mail = if att.person.email.contains('@') && !att.person.email.contains(':') {
            format!("mailto:{}", att.person.email)
        } else {
            att.person.email.clone()
        };
        let mut p = Property::new("ATTENDEE", mail);
        p.add_parameter("PARTSTAT", att.status.ics_value());
        if let Some(n) = &att.person.name {
            p.add_parameter("CN", n);
        }
        event.append_multi_property(p.done());
    }

    event.add_property("SEQUENCE", d.sequence.to_string());

    if d.tentative {
        event.status(icalendar::EventStatus::Tentative);
    }

    event.add_property("TRANSP", if d.busy { "OPAQUE" } else { "TRANSPARENT" });

    if let Some(u) = &d.url {
        event.add_property("URL", u);
    }

    if let Some(j) = &d.join_url {
        let mut p = Property::new("CONFERENCE", j.as_str());
        p.add_parameter("VALUE", "URI");
        p.add_parameter("FEATURE", "VIDEO");
        event.append_property(p.done());
    }

    use chrono::Duration as ChronoDur;
    for minutes in &d.reminders {
        let delta = if *minutes == 0 {
            ChronoDur::zero()
        } else {
            -ChronoDur::minutes(i64::from(*minutes))
        };
        let alarm = icalendar::Alarm::display("Reminder", delta).done();
        event.append_component(alarm);
    }

    if draft.all_day {
        for ex in &d.exdates {
            let val = ex.naive_utc().date().format("%Y%m%d").to_string();
            let mut p = Property::new("EXDATE", &val);
            p.add_parameter("VALUE", "DATE");
            event.append_multi_property(p.done());
        }
        for rd in &d.rdates {
            let val = rd.naive_utc().date().format("%Y%m%d").to_string();
            let mut p = Property::new("RDATE", &val);
            p.add_parameter("VALUE", "DATE");
            event.append_multi_property(p.done());
        }
    } else {
        fn fmt_exdate_utc(dt: DateTime<Utc>) -> String {
            dt.format("%Y%m%dT%H%M%SZ").to_string()
        }
        for ex in &d.exdates {
            append_multi(event, "EXDATE", &fmt_exdate_utc(*ex));
        }
        for rd in &d.rdates {
            append_multi(event, "RDATE", &fmt_exdate_utc(*rd));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use icalendar::Calendar;

    fn parse_one_event(ics: &str) -> (Event, ZoneResolver) {
        let cal: Calendar = ics.parse().expect("parse");
        let resolver = ZoneResolver::from_calendar(&cal);
        let ev = cal
            .components
            .iter()
            .find_map(|c| c.as_event())
            .expect("event")
            .clone();
        (ev, resolver)
    }

    #[test]
    fn reads_organizer_attendees_alarms_status_sequence() {
        let ics = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\n\
            BEGIN:VEVENT\r\n\
            UID:test-1@alcedo\r\n\
            SUMMARY:Meet\r\n\
            SEQUENCE:3\r\n\
            STATUS:TENTATIVE\r\n\
            TRANSP:TRANSPARENT\r\n\
            DTSTART:20250821T150000Z\r\n\
            DTEND:20250821T160000Z\r\n\
            ORGANIZER;CN=Alice:mailto:alice@example.com\r\n\
            ATTENDEE;CN=Bob;PARTSTAT=ACCEPTED:mailto:bob@example.com\r\n\
            ATTENDEE;PARTSTAT=NEEDS-ACTION:mailto:carol@example.com\r\n\
            URL:https://example.com/event\r\n\
            BEGIN:VALARM\r\n\
            TRIGGER:-PT15M\r\n\
            ACTION:DISPLAY\r\n\
            DESCRIPTION:Reminder\r\n\
            END:VALARM\r\n\
            END:VEVENT\r\nEND:VCALENDAR\r\n";
        let (ev, res) = parse_one_event(ics);
        let start = Utc.with_ymd_and_hms(2025, 8, 21, 15, 0, 0).unwrap();
        let d = read_details(
            &ev,
            &res,
            None,
            None,
            &chrono_tz::Europe::London,
            start,
            false,
        );
        assert_eq!(d.sequence, 3);
        assert!(d.tentative);
        assert!(!d.busy);
        assert_eq!(d.organizer.as_ref().unwrap().email, "alice@example.com");
        assert_eq!(d.attendees.len(), 2);
        assert_eq!(d.reminders, vec![15]);
        assert_eq!(d.url.as_deref(), Some("https://example.com/event"));
    }

    #[test]
    fn attendee_cap_200() {
        let mut body = String::from(
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:c@alcedo\r\nSUMMARY:S\r\nDTSTART:20250821T150000Z\r\nDTEND:20250821T160000Z\r\n",
        );
        for i in 0..250 {
            body.push_str(&format!("ATTENDEE:mailto:user{}@example.com\r\n", i));
        }
        body.push_str("END:VEVENT\r\nEND:VCALENDAR\r\n");
        let (ev, res) = parse_one_event(&body);
        let start = Utc.with_ymd_and_hms(2025, 8, 21, 15, 0, 0).unwrap();
        let d = read_details(&ev, &res, None, None, &chrono_tz::UTC, start, false);
        assert_eq!(d.attendees.len(), MAX_ATTENDEES);
    }

    #[test]
    fn absolute_trigger_with_tzid_is_minutes_before_start() {
        let ics = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\n\
            BEGIN:VEVENT\r\n\
            UID:alarm-tz@alcedo\r\n\
            SUMMARY:Alarms\r\n\
            DTSTART:20261102T160000Z\r\n\
            DTEND:20261102T170000Z\r\n\
            BEGIN:VALARM\r\n\
            TRIGGER;TZID=America/Chicago:20261102T090000\r\n\
            ACTION:DISPLAY\r\n\
            END:VALARM\r\n\
            END:VEVENT\r\nEND:VCALENDAR\r\n";
        let (ev, res) = parse_one_event(ics);
        let start = Utc.with_ymd_and_hms(2026, 11, 2, 16, 0, 0).unwrap();
        let d = read_details(&ev, &res, None, None, &chrono_tz::UTC, start, false);
        assert_eq!(d.reminders, vec![60]);
    }

    #[test]
    fn pt0s_alarm_is_zero_minutes() {
        let ics = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\n\
            BEGIN:VEVENT\r\n\
            UID:pt0s@alcedo\r\n\
            SUMMARY:At start\r\n\
            DTSTART:20261102T160000Z\r\n\
            DTEND:20261102T170000Z\r\n\
            BEGIN:VALARM\r\n\
            TRIGGER:PT0S\r\n\
            ACTION:DISPLAY\r\n\
            END:VALARM\r\n\
            END:VEVENT\r\nEND:VCALENDAR\r\n";
        let (ev, res) = parse_one_event(ics);
        let start = Utc.with_ymd_and_hms(2026, 11, 2, 16, 0, 0).unwrap();
        let d = read_details(&ev, &res, None, None, &chrono_tz::UTC, start, false);
        assert_eq!(d.reminders, vec![0]);
    }

    #[test]
    fn microsoft_online_meeting_property_is_join_url() {
        let ics = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\n\
            BEGIN:VEVENT\r\n\
            UID:ms-teams@alcedo\r\n\
            SUMMARY:Teams\r\n\
            DTSTART:20261102T160000Z\r\n\
            DTEND:20261102T170000Z\r\n\
            X-MICROSOFT-ONLINEMEETINGCONFURL:https://teams.microsoft.com/l/meetup-join/abc\r\n\
            END:VEVENT\r\nEND:VCALENDAR\r\n";
        let (ev, res) = parse_one_event(ics);
        let start = Utc.with_ymd_and_hms(2026, 11, 2, 16, 0, 0).unwrap();
        let d = read_details(&ev, &res, None, None, &chrono_tz::UTC, start, false);
        assert_eq!(
            d.join_url.as_deref(),
            Some("https://teams.microsoft.com/l/meetup-join/abc")
        );
    }

    #[test]
    fn duration_replaces_missing_dtend() {
        let ics = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:d@alcedo\r\n\
                   DTSTART:20250821T150000Z\r\nDURATION:PT90M\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let drafts = crate::parse_ics(ics.as_bytes()).expect("parse");
        assert_eq!(
            drafts[0].end,
            drafts[0].start + chrono::Duration::minutes(90)
        );
    }
}
