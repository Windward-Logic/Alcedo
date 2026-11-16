//! Interpret `VTIMEZONE` `STANDARD` / `DAYLIGHT` observances for sender zone resolution.

use chrono::{DateTime, Duration, FixedOffset, LocalResult, NaiveDateTime, Offset, TimeZone, Utc};
use icalendar::{CalendarDateTime, Component, DatePerhapsTime};
use rrule::{RRuleSet, Tz as RRuleTz};

fn wall_as_utc_marker(wall: NaiveDateTime) -> DateTime<Utc> {
    wall.and_utc()
}

fn parse_tzoffset_to_east_secs(value: &str) -> Option<i32> {
    let s = value.trim();
    if s.is_empty() {
        return None;
    }
    let neg = s.starts_with('-');
    let s = s.trim_start_matches(['+', '-']);
    let (hours, minutes, seconds) = if s.contains(':') {
        let mut parts = s.split(':');
        let h: i32 = parts.next()?.parse().ok()?;
        let m: i32 = parts.next().unwrap_or("0").parse().ok()?;
        let sec: i32 = parts.next().unwrap_or("0").parse().ok()?;
        (h, m, sec)
    } else if s.len() >= 4 {
        let h: i32 = s.get(..2)?.parse().ok()?;
        let m: i32 = s.get(2..4)?.parse().ok()?;
        let sec = if s.len() >= 6 {
            s.get(4..6)?.parse().ok()?
        } else {
            0
        };
        (h, m, sec)
    } else {
        return None;
    };
    let mut total = hours * 3600 + minutes * 60 + seconds;
    if neg {
        total = -total;
    }
    Some(total)
}

fn naive_from_dateperhaps(dt: DatePerhapsTime) -> Option<NaiveDateTime> {
    match dt {
        DatePerhapsTime::Date(d) => d.and_hms_opt(0, 0, 0),
        DatePerhapsTime::DateTime(ct) => match ct {
            CalendarDateTime::Floating(n) | CalendarDateTime::WithTimezone { date_time: n, .. } => {
                Some(n)
            }
            CalendarDateTime::Utc(u) => Some(u.naive_utc()),
        },
    }
}

/// One `STANDARD` or `DAYLIGHT` definition inside `VTIMEZONE`.
#[derive(Clone, Debug)]
pub struct Observance {
    pub dtstart: NaiveDateTime,
    pub offset_to_secs: i32,
    pub rrule: Option<String>,
}

/// Custom time zone from fixed-offset observances and optional `RRULE` on `DTSTART`.
#[derive(Clone, Debug)]
pub struct CustomZone {
    observances: Vec<Observance>,
}

impl CustomZone {
    pub fn from_vtimezone<C: Component>(other: &C) -> Option<Self> {
        if other.component_kind() != "VTIMEZONE" {
            return None;
        }
        let mut observances = Vec::new();
        for child in other.components() {
            let kind = child.component_kind();
            if kind != "STANDARD" && kind != "DAYLIGHT" {
                continue;
            }
            let tz_to = child.property_value("TZOFFSETTO")?;
            let offset_to_secs = parse_tzoffset_to_east_secs(tz_to)?;
            let start_prop = child.properties().get("DTSTART")?;
            let dpt = DatePerhapsTime::from_property(start_prop)?;
            let dtstart = naive_from_dateperhaps(dpt)?;
            let rrule = child
                .property_value("RRULE")
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned);
            observances.push(Observance {
                dtstart,
                offset_to_secs,
                rrule,
            });
        }
        if observances.is_empty() {
            None
        } else {
            Some(Self { observances })
        }
    }

    fn latest_onset_marker(&self, obs: &Observance, wall: NaiveDateTime) -> Option<DateTime<Utc>> {
        let marker = wall_as_utc_marker(wall);
        if let Some(rule) = &obs.rrule {
            let dtstart_line = obs.dtstart.format("%Y%m%dT%H%M%SZ").to_string();
            let body = format!("DTSTART:{dtstart_line}\nRRULE:{rule}");
            let set: RRuleSet = body.parse().ok()?;
            let naive = marker.naive_utc();
            let marker_rr = RRuleTz::UTC
                .from_local_datetime(&naive)
                .single()
                .unwrap_or_else(|| RRuleTz::UTC.from_utc_datetime(&naive));
            let window_start = marker - Duration::days(8 * 365);
            let window_naive = window_start.naive_utc();
            let window_rr = RRuleTz::UTC
                .from_local_datetime(&window_naive)
                .single()
                .unwrap_or_else(|| RRuleTz::UTC.from_utc_datetime(&window_naive));
            let res = set.after(window_rr).before(marker_rr).all(400);
            res.dates
                .into_iter()
                .rfind(|d| *d <= marker)
                .map(|d| d.with_timezone(&Utc))
        } else if obs.dtstart <= wall {
            Some(obs.dtstart.and_utc())
        } else {
            None
        }
    }

    pub fn wall_to_utc(&self, wall: NaiveDateTime) -> DateTime<Utc> {
        let mut best: Option<(&Observance, DateTime<Utc>)> = None;
        for obs in &self.observances {
            if let Some(onset) = self.latest_onset_marker(obs, wall) {
                let replace = match best {
                    None => true,
                    Some((_, prev_on)) => onset > prev_on,
                };
                if replace {
                    best = Some((obs, onset));
                }
            }
        }
        let Some((obs, _)) = best else {
            return wall.and_utc();
        };
        let offset = match FixedOffset::east_opt(obs.offset_to_secs) {
            Some(o) => o,
            None => FixedOffset::east_opt(0).expect("zero offset"),
        };
        match offset.from_local_datetime(&wall) {
            LocalResult::Single(dt) => dt.with_timezone(&Utc),
            LocalResult::Ambiguous(first, _) => first.with_timezone(&Utc),
            LocalResult::None => match offset
                .from_local_datetime(&(wall - Duration::hours(3)))
                .earliest()
            {
                Some(dt) => {
                    let fix = dt.offset().fix().local_minus_utc();
                    (wall - Duration::seconds(fix as i64)).and_utc()
                }
                None => wall.and_utc(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    use chrono::TimeZone;

    #[test]
    fn custom_rules_cross_dst_boundary() {
        let ics = "BEGIN:VTIMEZONE\r\n\
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
        let parsed: icalendar::Calendar = ics.parse().expect("parse vtimezone");
        let other = parsed
            .components
            .iter()
            .find_map(|c| match c {
                icalendar::CalendarComponent::Other(o) if o.component_kind() == "VTIMEZONE" => {
                    Some(o)
                }
                _ => None,
            })
            .expect("vtimezone");
        let cz = CustomZone::from_vtimezone(other).expect("custom zone");
        let summer = NaiveDate::from_ymd_opt(2026, 7, 15)
            .unwrap()
            .and_hms_opt(10, 0, 0)
            .unwrap();
        let winter = NaiveDate::from_ymd_opt(2026, 1, 15)
            .unwrap()
            .and_hms_opt(10, 0, 0)
            .unwrap();
        let u_summer = cz.wall_to_utc(summer);
        let u_winter = cz.wall_to_utc(winter);
        assert_eq!(
            u_summer,
            Utc.with_ymd_and_hms(2026, 7, 15, 14, 0, 0).unwrap()
        );
        assert_eq!(
            u_winter,
            Utc.with_ymd_and_hms(2026, 1, 15, 15, 0, 0).unwrap()
        );
        assert!(u_summer != u_winter);
    }

    #[test]
    fn fixed_onset_after_wall_is_ignored() {
        let ics = "BEGIN:VTIMEZONE\r\n\
            TZID:Custom-Future\r\n\
            BEGIN:STANDARD\r\n\
            DTSTART:20071104T020000\r\n\
            TZOFFSETFROM:-0400\r\n\
            TZOFFSETTO:-0500\r\n\
            END:STANDARD\r\n\
            BEGIN:DAYLIGHT\r\n\
            DTSTART:20070311T020000\r\n\
            TZOFFSETFROM:-0500\r\n\
            TZOFFSETTO:-0400\r\n\
            END:DAYLIGHT\r\n\
            END:VTIMEZONE\r\n";
        let parsed: icalendar::Calendar = ics.parse().expect("parse vtimezone");
        let other = parsed
            .components
            .iter()
            .find_map(|c| match c {
                icalendar::CalendarComponent::Other(o) if o.component_kind() == "VTIMEZONE" => {
                    Some(o)
                }
                _ => None,
            })
            .expect("vtimezone");
        let cz = CustomZone::from_vtimezone(other).expect("custom zone");
        let wall = NaiveDate::from_ymd_opt(2007, 6, 15)
            .unwrap()
            .and_hms_opt(10, 0, 0)
            .unwrap();
        assert_eq!(
            cz.wall_to_utc(wall),
            Utc.with_ymd_and_hms(2007, 6, 15, 14, 0, 0).unwrap()
        );
    }

    #[test]
    fn yearly_rule_from_1601_picks_latest_onset() {
        let ics = "BEGIN:VTIMEZONE\r\n\
            TZID:Custom-OldStart\r\n\
            BEGIN:STANDARD\r\n\
            DTSTART:16010101T000000\r\n\
            RRULE:FREQ=YEARLY;BYMONTH=11;BYDAY=1SU\r\n\
            TZOFFSETFROM:-0400\r\n\
            TZOFFSETTO:-0500\r\n\
            END:STANDARD\r\n\
            BEGIN:DAYLIGHT\r\n\
            DTSTART:16010101T000000\r\n\
            RRULE:FREQ=YEARLY;BYMONTH=3;BYDAY=2SU\r\n\
            TZOFFSETFROM:-0500\r\n\
            TZOFFSETTO:-0400\r\n\
            END:DAYLIGHT\r\n\
            END:VTIMEZONE\r\n";
        let parsed: icalendar::Calendar = ics.parse().expect("parse vtimezone");
        let other = parsed
            .components
            .iter()
            .find_map(|c| match c {
                icalendar::CalendarComponent::Other(o) if o.component_kind() == "VTIMEZONE" => {
                    Some(o)
                }
                _ => None,
            })
            .expect("vtimezone");
        let cz = CustomZone::from_vtimezone(other).expect("custom zone");
        let wall = NaiveDate::from_ymd_opt(2026, 7, 15)
            .unwrap()
            .and_hms_opt(10, 0, 0)
            .unwrap();
        assert_eq!(
            cz.wall_to_utc(wall),
            Utc.with_ymd_and_hms(2026, 7, 15, 14, 0, 0).unwrap()
        );
    }
}
