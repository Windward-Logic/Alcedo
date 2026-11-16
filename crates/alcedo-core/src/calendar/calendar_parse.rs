//! ICS parse / build helpers for [`super::CalendarEventDraft`].

use chrono::{DateTime, Datelike, Duration, Local, NaiveDateTime, TimeZone, Utc};
use icalendar::{
    Calendar, CalendarComponent, CalendarDateTime, Component, DatePerhapsTime, Event, EventLike,
    Property, ValueType,
};
use uuid::Uuid;

use crate::calendar_ics_details::{read_details, write_details};
use crate::calendar_zone::{ResolvedZone, ZoneResolver, wall_to_utc};

use super::{CalendarEventDraft, InviteMethod, ParsedInvite, marker_midnight_utc};

/// ICS instant with optional resolved IANA id and zone (alias for clippy::type_complexity).
type ResolvedInstant = (DateTime<Utc>, Option<String>, Option<ResolvedZone>);
/// Full parsed instant including all-day flag.
type ParsedInstantFull = (DateTime<Utc>, bool, Option<String>, Option<ResolvedZone>);

pub(crate) const PARSE_ERR: &str = "Could not read this calendar invitation";

/// Last year an iCalendar date can express.
const MAX_ICS_YEAR: i32 = 9999;

pub(super) fn unfold_ics(text: &str) -> String {
    let mut out = String::new();
    for line in text.split('\n') {
        let line = line.trim_end_matches('\r');
        if line.starts_with(' ') || line.starts_with('\t') {
            out.push_str(&line[1..]);
        } else {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(line);
        }
    }
    out
}

fn parse_invite_method(calendar: &Calendar) -> InviteMethod {
    let Some(m) = calendar.property_value("METHOD") else {
        return InviteMethod::Publish;
    };
    if m.eq_ignore_ascii_case("REQUEST") {
        InviteMethod::Request
    } else if m.eq_ignore_ascii_case("PUBLISH") {
        InviteMethod::Publish
    } else if m.eq_ignore_ascii_case("CANCEL") {
        InviteMethod::Cancel
    } else if m.eq_ignore_ascii_case("REPLY") {
        InviteMethod::Reply
    } else {
        InviteMethod::Other
    }
}

/// Parse ICS including calendar `METHOD`.
pub fn parse_ics_with_method(bytes: &[u8]) -> Result<ParsedInvite, String> {
    parse_ics_in(bytes, &Local)
}

pub(crate) fn parse_ics_in<Z: TimeZone>(
    bytes: &[u8],
    floating_zone: &Z,
) -> Result<ParsedInvite, String> {
    let text = std::str::from_utf8(bytes).map_err(|_| PARSE_ERR.to_string())?;
    let unfolded = unfold_ics(text);
    let calendar: Calendar = unfolded.parse().map_err(|_| PARSE_ERR.to_string())?;
    let method = parse_invite_method(&calendar);
    let resolver = ZoneResolver::from_calendar(&calendar);

    let events: Vec<_> = calendar
        .components
        .iter()
        .filter_map(CalendarComponent::as_event)
        .collect();

    if events.is_empty() {
        return Err(PARSE_ERR.to_string());
    }

    let method_cancel = matches!(method, InviteMethod::Cancel);

    let mut drafts = Vec::new();
    for event in events {
        let cancelled =
            method_cancel || event.get_status() == Some(icalendar::EventStatus::Cancelled);
        if cancelled && event.get_start().is_none() {
            continue;
        }
        drafts.push(event_to_draft(
            event,
            method_cancel,
            &calendar,
            &resolver,
            floating_zone,
        )?);
    }
    Ok(ParsedInvite { method, drafts })
}

/// Parse all `VEVENT` components (floating times use `Local`).
pub fn parse_ics(bytes: &[u8]) -> Result<Vec<CalendarEventDraft>, String> {
    Ok(parse_ics_with_method(bytes)?.drafts)
}

fn event_to_draft<Z: TimeZone>(
    event: &icalendar::Event,
    method_cancel: bool,
    _calendar: &Calendar,
    resolver: &ZoneResolver,
    floating_zone: &Z,
) -> Result<CalendarEventDraft, String> {
    let start_prop = event
        .properties()
        .get("DTSTART")
        .ok_or_else(|| PARSE_ERR.to_string())?;
    let start_raw =
        DatePerhapsTime::from_property(start_prop).ok_or_else(|| PARSE_ERR.to_string())?;
    let (start, all_day, start_zone_name, primary_wall) =
        parse_instant_full(start_prop, start_raw, resolver, floating_zone)?;

    let end = if let Some(dur_prop) = event.properties().get("DURATION") {
        let d = parse_ics_iso_duration(dur_prop.value()).ok_or_else(|| PARSE_ERR.to_string())?;
        // iCalendar dates stop at year 9999; a duration reaching past that
        // is not a real event, and bounding it here keeps later date
        // arithmetic (recurrence expansion, reminders) in range.
        start
            .checked_add_signed(d)
            .filter(|end| end.year() <= MAX_ICS_YEAR)
            .ok_or_else(|| PARSE_ERR.to_string())?
    } else if let Some(end_prop) = event.properties().get("DTEND") {
        let end_raw =
            DatePerhapsTime::from_property(end_prop).ok_or_else(|| PARSE_ERR.to_string())?;
        parse_instant_end(
            end_prop,
            end_raw,
            resolver,
            floating_zone,
            primary_wall.as_ref(),
            all_day,
        )?
    } else if all_day {
        start + Duration::days(1)
    } else {
        start + Duration::hours(1)
    };

    let summary = event
        .get_summary()
        .map(str::trim)
        .filter(|summary| !summary.is_empty())
        .unwrap_or("Calendar event")
        .to_string();

    let location = event
        .get_location()
        .map(str::trim)
        .filter(|location| !location.is_empty())
        .map(str::to_owned);

    let uid = event
        .get_uid()
        .map(str::to_owned)
        .filter(|uid| !uid.is_empty())
        .unwrap_or_else(|| {
            let key = format!(
                "{}|{}|{}|{}",
                start.to_rfc3339(),
                end.to_rfc3339(),
                summary,
                location.as_deref().unwrap_or("")
            );
            Uuid::new_v5(&Uuid::NAMESPACE_OID, key.as_bytes()).to_string()
        });

    let description = event
        .get_description()
        .map(str::trim)
        .filter(|description| !description.is_empty())
        .map(str::to_owned);

    let rrule = event
        .property_value("RRULE")
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);

    let recurrence_id = parse_recurrence_id(event, resolver, floating_zone, primary_wall.as_ref())?;

    let cancelled = method_cancel || event.get_status() == Some(icalendar::EventStatus::Cancelled);

    let details_tz = match &start_zone_name {
        Some(name) if matches!(primary_wall, Some(ResolvedZone::Custom(_))) => None,
        Some(name) if name.parse::<chrono_tz::Tz>().is_ok() => start_zone_name.clone(),
        _ => None,
    };

    let mut details = read_details(
        event,
        resolver,
        details_tz,
        primary_wall.as_ref(),
        floating_zone,
        start,
        all_day,
    );
    if details.join_url.is_none() {
        details.join_url = crate::calendar_details::find_join_url(
            None,
            location.as_deref(),
            description.as_deref(),
        );
    }

    Ok(CalendarEventDraft {
        uid,
        summary,
        location,
        description,
        start,
        end,
        all_day,
        recurrence_id,
        rrule,
        source_message_id: None,
        cancelled,
        details,
    })
}

fn parse_recurrence_id<Z: TimeZone>(
    event: &Event,
    resolver: &ZoneResolver,
    floating_zone: &Z,
    primary_wall: Option<&ResolvedZone>,
) -> Result<Option<DateTime<Utc>>, String> {
    let Some(prop) = event.properties().get("RECURRENCE-ID") else {
        return Ok(None);
    };
    let raw = DatePerhapsTime::from_property(prop).ok_or_else(|| PARSE_ERR.to_string())?;
    let all_day_rid =
        matches!(&raw, DatePerhapsTime::Date(_)) || prop.value_type() == Some(ValueType::Date);
    let dt = if all_day_rid {
        let (utc, _, _, _) = parse_instant_full(prop, raw, resolver, floating_zone)?;
        utc
    } else {
        parse_instant_end(prop, raw, resolver, floating_zone, primary_wall, false)?
    };
    Ok(Some(dt))
}

/// ICS `DURATION` / ISO-8601 duration (`PT1H30M`, `P1D`, `P1W`, `P1DT12H` …).
pub(crate) fn parse_ics_iso_duration(s: &str) -> Option<Duration> {
    let s = s.trim().strip_prefix('P')?;
    let mut dur = Duration::zero();
    let mut time_section = false;
    let mut num = String::new();
    for ch in s.chars() {
        if ch.is_ascii_digit() {
            num.push(ch);
            continue;
        }
        if num.is_empty() && ch == 'T' {
            time_section = true;
            continue;
        }
        let n: i64 = num.parse().ok()?;
        num.clear();
        // Invites arrive in ordinary mail: the `try_` constructors and the
        // checked add turn an absurd value into "not a duration" where the
        // plain ones panic.
        let part = match ch {
            'W' if !time_section => Duration::try_weeks(n),
            'D' if !time_section => Duration::try_days(n),
            'T' => {
                time_section = true;
                continue;
            }
            'H' if time_section => Duration::try_hours(n),
            'M' if time_section => Duration::try_minutes(n),
            'S' if time_section => Duration::try_seconds(n),
            _ => return None,
        }?;
        dur = dur.checked_add(&part)?;
    }
    if !num.is_empty() {
        return None;
    }
    Some(dur)
}

fn parse_instant_full<Z: TimeZone>(
    _prop: &Property,
    value: DatePerhapsTime,
    resolver: &ZoneResolver,
    floating_zone: &Z,
) -> Result<ParsedInstantFull, String> {
    match value {
        DatePerhapsTime::Date(date) => Ok((marker_midnight_utc(date), true, None, None)),
        DatePerhapsTime::DateTime(cal) => match cal {
            CalendarDateTime::Utc(dt) => Ok((dt, false, None, None)),
            CalendarDateTime::Floating(naive) => {
                Ok((wall_to_utc(naive, floating_zone), false, None, None))
            }
            CalendarDateTime::WithTimezone { date_time, tzid } => {
                let resolved = resolve_tzid_wall(&tzid, resolver, floating_zone, date_time)?;
                Ok((resolved.0, false, resolved.1, resolved.2))
            }
        },
    }
}

fn parse_instant_end<Z: TimeZone>(
    prop: &Property,
    value: DatePerhapsTime,
    resolver: &ZoneResolver,
    floating_zone: &Z,
    primary: Option<&ResolvedZone>,
    all_day: bool,
) -> Result<DateTime<Utc>, String> {
    match value {
        DatePerhapsTime::Date(date) => {
            if all_day {
                Ok(marker_midnight_utc(date))
            } else {
                let naive = date
                    .and_hms_opt(0, 0, 0)
                    .ok_or_else(|| PARSE_ERR.to_string())?;
                Ok(resolve_tzid_wall_for(prop, naive, resolver, floating_zone, primary)?.0)
            }
        }
        DatePerhapsTime::DateTime(cal) => match cal {
            CalendarDateTime::Utc(dt) => Ok(dt),
            CalendarDateTime::Floating(naive) => Ok(if let Some(z) = primary {
                z.wall_to_utc(naive)
            } else {
                wall_to_utc(naive, floating_zone)
            }),
            CalendarDateTime::WithTimezone { date_time, tzid } => {
                Ok(resolve_tzid_wall(&tzid, resolver, floating_zone, date_time)?.0)
            }
        },
    }
}

fn resolve_tzid_wall_for<Z: TimeZone>(
    prop: &Property,
    naive: NaiveDateTime,
    resolver: &ZoneResolver,
    floating_zone: &Z,
    primary: Option<&ResolvedZone>,
) -> Result<ResolvedInstant, String> {
    if let Some(tzid) = prop.params().get("TZID") {
        return resolve_tzid_wall(tzid.value(), resolver, floating_zone, naive);
    }
    if let Some(z) = primary {
        return Ok((z.wall_to_utc(naive), None, None));
    }
    Ok((wall_to_utc(naive, floating_zone), None, None))
}

fn resolve_tzid_wall<Z: TimeZone>(
    tzid: &str,
    resolver: &ZoneResolver,
    floating_zone: &Z,
    naive: NaiveDateTime,
) -> Result<ResolvedInstant, String> {
    if let Some(r) = resolver.resolve_sender_zone(tzid) {
        let iana = r.iana_name();
        return Ok((r.wall_to_utc(naive), iana, Some(r)));
    }
    Ok((wall_to_utc(naive, floating_zone), None, None))
}

/// ICS `RECURRENCE-ID` serialization for matching.
fn recurrence_id_ics_value(rid: Option<DateTime<Utc>>, all_day: bool) -> String {
    let Some(r) = rid else {
        return String::new();
    };
    if all_day {
        r.format("%Y%m%d").to_string()
    } else {
        r.format("%Y%m%dT%H%M%SZ").to_string()
    }
}

/// Whether `event`'s `RECURRENCE-ID` names the occurrence that started at
/// `original_start`.
pub fn recurrence_id_equals(
    event: &Event,
    original_start: DateTime<Utc>,
    all_day: bool,
    resolver: &ZoneResolver,
) -> bool {
    let Some(prop) = event.properties().get("RECURRENCE-ID") else {
        return false;
    };
    let expected = recurrence_id_ics_value(Some(original_start), all_day);
    let raw = prop.value().trim();
    let digit_expected = expected
        .chars()
        .filter(|c| c.is_ascii_digit())
        .collect::<String>();
    let digit_raw: String = raw.chars().filter(|c| c.is_ascii_digit()).collect();
    if !digit_expected.is_empty() && digit_raw == digit_expected {
        return true;
    }
    if let Ok(Some(rid)) = parse_recurrence_id(event, resolver, &Utc, None) {
        if all_day {
            return rid.naive_utc().date() == original_start.naive_utc().date();
        }
        return rid == original_start;
    }
    false
}

/// Build a single `VEVENT` from `draft` (used by ICS merge).
pub fn build_vevent_from_draft(draft: &CalendarEventDraft) -> Event {
    let mut event = Event::new();
    event.uid(&draft.uid).summary(&draft.summary);
    if draft.cancelled {
        event.status(icalendar::EventStatus::Cancelled);
    }
    if let Some(location) = &draft.location {
        event.location(location);
    }
    if let Some(description) = &draft.description {
        event.description(description);
    }
    if let Some(rrule) = &draft.rrule {
        event.append_property(Property::new("RRULE", rrule));
    }
    if let Some(rec) = draft.recurrence_id {
        if draft.all_day {
            let d = rec.naive_utc().date();
            event.append_property(DatePerhapsTime::Date(d).to_property("RECURRENCE-ID"));
        } else {
            event.append_property(Property::new(
                "RECURRENCE-ID",
                recurrence_id_ics_value(Some(rec), false),
            ));
        }
    }

    if draft.all_day {
        let (sd, ed_ex) = draft.all_day_dates();
        event.starts(sd).ends(ed_ex);
    } else if let Some(ref iana) = draft.details.time_zone {
        if let Ok(tz) = iana.parse::<chrono_tz::Tz>() {
            let s = draft.start.with_timezone(&tz).naive_local();
            let e = draft.end.with_timezone(&tz).naive_local();
            let dt_s = CalendarDateTime::from((s, tz));
            let dt_e = CalendarDateTime::from((e, tz));
            event
                .append_property(DatePerhapsTime::DateTime(dt_s).to_property("DTSTART"))
                .append_property(DatePerhapsTime::DateTime(dt_e).to_property("DTEND"));
        } else {
            event.starts(draft.start).ends(draft.end);
        }
    } else {
        event.starts(draft.start).ends(draft.end);
    }

    write_details(&mut event, draft);
    event.done()
}

/// Minimal `VEVENT` shell for iTIP `REPLY` (no description, location, alarms, or `RRULE`).
pub(crate) fn build_reply_vevent_core(draft: &CalendarEventDraft) -> Event {
    let mut event = Event::new();
    event.uid(&draft.uid);

    if let Some(rec) = draft.recurrence_id {
        if draft.all_day {
            let d = rec.naive_utc().date();
            event.append_property(DatePerhapsTime::Date(d).to_property("RECURRENCE-ID"));
        } else {
            event.append_property(Property::new(
                "RECURRENCE-ID",
                recurrence_id_ics_value(Some(rec), false),
            ));
        }
    }

    if draft.all_day {
        let (sd, ed_ex) = draft.all_day_dates();
        event.starts(sd).ends(ed_ex);
    } else if let Some(ref iana) = draft.details.time_zone {
        if let Ok(tz) = iana.parse::<chrono_tz::Tz>() {
            let s = draft.start.with_timezone(&tz).naive_local();
            let e = draft.end.with_timezone(&tz).naive_local();
            let dt_s = CalendarDateTime::from((s, tz));
            let dt_e = CalendarDateTime::from((e, tz));
            event
                .append_property(DatePerhapsTime::DateTime(dt_s).to_property("DTSTART"))
                .append_property(DatePerhapsTime::DateTime(dt_e).to_property("DTEND"));
        } else {
            event.starts(draft.start).ends(draft.end);
        }
    } else {
        event.starts(draft.start).ends(draft.end);
    }

    event.done()
}

/// Build full calendar ICS text from a draft.
pub fn build_ics(draft: &CalendarEventDraft) -> String {
    let ev = build_vevent_from_draft(draft);
    Calendar::new().push(ev).to_string()
}
