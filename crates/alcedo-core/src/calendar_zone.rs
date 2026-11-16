//! Wall-clock times, zone name resolution, and `VTIMEZONE` lookup for calendar parsing.

use std::collections::HashMap;

use chrono::{DateTime, Duration, LocalResult, NaiveDate, NaiveDateTime, Offset, TimeZone, Utc};
use icalendar::{Calendar, CalendarComponent, Component};

use crate::calendar_vtimezone::CustomZone;
use crate::calendar_windows_zones::windows_zone_to_iana;

/// Convert a wall-clock time in `zone` to UTC. Never panics; follows the DST gap/ambiguity policy.
pub fn wall_to_utc<Z: TimeZone>(wall: NaiveDateTime, zone: &Z) -> DateTime<Utc> {
    match zone.from_local_datetime(&wall) {
        LocalResult::Single(dt) => dt.with_timezone(&Utc),
        LocalResult::Ambiguous(first, _) => first.with_timezone(&Utc),
        LocalResult::None => {
            let shifted = wall - Duration::hours(3);
            match zone.from_local_datetime(&shifted).earliest() {
                Some(dt) => {
                    let off = dt.offset().fix().local_minus_utc();
                    (wall - Duration::seconds(off as i64)).and_utc()
                }
                None => wall.and_utc(),
            }
        }
    }
}

/// Start of the local calendar `day` in `zone`, as UTC (`wall_to_utc` of 00:00).
pub fn local_day_start_utc<Z: TimeZone>(day: NaiveDate, zone: &Z) -> DateTime<Utc> {
    let wall = day
        .and_hms_opt(0, 0, 0)
        .unwrap_or_else(|| day.and_hms_opt(1, 0, 0).expect("valid day"));
    wall_to_utc(wall, zone)
}

/// System IANA zone from `iana-time-zone`, kept only when it parses as [`chrono_tz::Tz`].
pub fn system_zone_name() -> Option<String> {
    let name = iana_time_zone::get_timezone().ok()?;
    name.parse::<chrono_tz::Tz>().ok()?;
    Some(name)
}

fn normalize_iana_path(name: &str) -> String {
    let parts: Vec<&str> = name.split('/').filter(|p| !p.is_empty()).collect();
    match parts.len() {
        0 => String::new(),
        1 => parts[0].to_string(),
        _ => format!("{}/{}", parts[parts.len() - 2], parts[parts.len() - 1]),
    }
}

fn normalize_tzid_lookup_key(name: &str) -> String {
    name.trim().to_ascii_lowercase()
}

/// Resolve a `TZID` string or Windows name to a canonical IANA identifier when possible.
pub fn zone_name_to_iana(name: &str) -> Option<String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(tz) = trimmed.parse::<chrono_tz::Tz>() {
        return Some(tz.name().to_string());
    }
    let path_norm = normalize_iana_path(trimmed);
    if path_norm.parse::<chrono_tz::Tz>().is_ok() {
        return Some(path_norm);
    }
    windows_zone_to_iana(trimmed).map(str::to_owned)
}

/// Time-zone abbreviation (e.g. `EST`) at `instant` for IANA `iana`, if known.
pub fn zone_abbreviation(instant: DateTime<Utc>, iana: &str) -> Option<String> {
    let tz: chrono_tz::Tz = iana.parse().ok()?;
    let local = instant.with_timezone(&tz);
    let s = local.format("%Z").to_string();
    if s.is_empty() { None } else { Some(s) }
}

/// Resolved sender zone: named IANA ([`chrono_tz::Tz`]) or custom `VTIMEZONE` rules.
#[derive(Clone, Debug)]
pub enum ResolvedZone {
    Named(chrono_tz::Tz),
    Custom(CustomZone),
}

impl ResolvedZone {
    pub fn wall_to_utc(&self, wall: NaiveDateTime) -> DateTime<Utc> {
        match self {
            ResolvedZone::Named(tz) => wall_to_utc(wall, tz),
            ResolvedZone::Custom(c) => c.wall_to_utc(wall),
        }
    }

    /// IANA name for named zones only; custom definitions have no single IANA id.
    pub fn iana_name(&self) -> Option<String> {
        match self {
            ResolvedZone::Named(tz) => Some(tz.name().to_string()),
            ResolvedZone::Custom(_) => None,
        }
    }
}

fn resolve_vtimezone_component<C: Component>(other: &C) -> Option<ResolvedZone> {
    if let Some(lic) = other
        .property_value("X-LIC-LOCATION")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        && let Some(iana) = zone_name_to_iana(lic)
        && let Ok(tz) = iana.parse::<chrono_tz::Tz>()
    {
        return Some(ResolvedZone::Named(tz));
    }
    CustomZone::from_vtimezone(other).map(ResolvedZone::Custom)
}

/// Indexes `VTIMEZONE` definitions from a parsed [`icalendar::Calendar`].
pub struct ZoneResolver {
    by_tzid: HashMap<String, ResolvedZone>,
}

impl ZoneResolver {
    pub fn from_calendar(calendar: &Calendar) -> Self {
        let mut by_tzid = HashMap::new();
        for component in &calendar.components {
            let CalendarComponent::Other(other) = component else {
                continue;
            };
            if other.component_kind() != "VTIMEZONE" {
                continue;
            };
            let Some(tzid) = other
                .property_value("TZID")
                .map(str::trim)
                .filter(|s| !s.is_empty())
            else {
                continue;
            };
            let key = normalize_tzid_lookup_key(tzid);
            if let Some(resolved) = resolve_vtimezone_component(other) {
                by_tzid.insert(key, resolved);
            }
        }
        Self { by_tzid }
    }

    pub fn resolve(&self, tzid: &str) -> Option<&ResolvedZone> {
        self.by_tzid.get(&normalize_tzid_lookup_key(tzid))
    }

    /// Full `TZID` resolution: IANA (incl. path) and Windows names first, then embedded `VTIMEZONE`.
    pub fn resolve_sender_zone(&self, tzid: &str) -> Option<ResolvedZone> {
        let trimmed = tzid.trim();
        if let Some(iana) = zone_name_to_iana(trimmed)
            && let Ok(tz) = iana.parse::<chrono_tz::Tz>()
        {
            return Some(ResolvedZone::Named(tz));
        }
        self.resolve(trimmed).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn wall_to_utc_ambiguous_picks_first() {
        let tz = chrono_tz::America::New_York;
        let wall = NaiveDate::from_ymd_opt(2026, 11, 1)
            .unwrap()
            .and_hms_opt(1, 30, 0)
            .unwrap();
        let utc = wall_to_utc(wall, &tz);
        assert_eq!(utc, Utc.with_ymd_and_hms(2026, 11, 1, 5, 30, 0).unwrap());
    }

    #[test]
    fn wall_to_utc_gap_uses_pre_transition_offset() {
        let tz = chrono_tz::America::New_York;
        let wall = NaiveDate::from_ymd_opt(2026, 3, 8)
            .unwrap()
            .and_hms_opt(2, 30, 0)
            .unwrap();
        let utc = wall_to_utc(wall, &tz);
        assert_eq!(utc, Utc.with_ymd_and_hms(2026, 3, 8, 7, 30, 0).unwrap());
    }

    #[test]
    fn resolves_windows_names() {
        assert_eq!(
            zone_name_to_iana("Eastern Standard Time").as_deref(),
            Some("America/New_York")
        );
    }

    #[test]
    fn unknown_zone_falls_back_to_viewer() {
        let tz = chrono_tz::Europe::Paris;
        let wall = NaiveDate::from_ymd_opt(2026, 6, 1)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap();
        let utc = wall_to_utc(wall, &tz);
        assert_eq!(
            utc,
            tz.from_local_datetime(&wall).unwrap().with_timezone(&Utc)
        );
    }

    #[test]
    fn local_day_start_exists_in_havana_gap() {
        let tz = chrono_tz::America::Havana;
        let day = NaiveDate::from_ymd_opt(2026, 3, 8).unwrap();
        let start = local_day_start_utc(day, &tz);
        let wall = day
            .and_hms_opt(0, 0, 0)
            .unwrap_or_else(|| day.and_hms_opt(1, 0, 0).unwrap());
        assert_eq!(start, wall_to_utc(wall, &tz));
        assert_eq!(start.with_timezone(&tz).date_naive(), day);
    }

    #[test]
    fn three_part_iana_resolves() {
        assert_eq!(
            zone_name_to_iana("America/Indiana/Indianapolis").as_deref(),
            Some("America/Indiana/Indianapolis")
        );
        assert_eq!(
            zone_name_to_iana("/mozilla.org/20050126_1/America/New_York").as_deref(),
            Some("America/New_York")
        );
    }

    #[test]
    fn gap_and_overlap_never_error() {
        let zones: &[chrono_tz::Tz] = &[
            chrono_tz::America::New_York,
            chrono_tz::Europe::London,
            chrono_tz::Pacific::Apia,
            chrono_tz::Australia::Lord_Howe,
            chrono_tz::Asia::Kathmandu,
        ];
        for tz in zones {
            for month in 1..=12u32 {
                if let Some(day) = NaiveDate::from_ymd_opt(2026, month, 15) {
                    for hour in 0..24u32 {
                        for min in [0u32, 30] {
                            if let Some(wall) = day.and_hms_opt(hour, min, 0) {
                                let _ = wall_to_utc(wall, tz);
                            }
                        }
                    }
                }
            }
        }
    }
}
