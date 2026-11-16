//! System clock format (12h / 24h) and time-of-day formatting.

use std::cell::Cell;

use chrono::{DateTime, Offset, TimeZone, Timelike, Utc};
use relm4::gtk::gio;
use relm4::gtk::prelude::*;

thread_local! {
    static CLOCK_24H: Cell<bool> = const { Cell::new(true) };
    static SETTINGS_HOOKED: Cell<bool> = const { Cell::new(false) };
}

fn ensure_interface_settings() -> Option<gio::Settings> {
    let source = gio::SettingsSchemaSource::default()?;
    source.lookup("org.gnome.desktop.interface", true)?;
    Some(gio::Settings::new("org.gnome.desktop.interface"))
}

fn refresh_from_settings(s: &gio::Settings) {
    CLOCK_24H.set(s.string("clock-format").as_str() == "24h");
}

/// Whether the desktop uses 24-hour time (`false` = 12-hour AM/PM).
pub fn use_24h() -> bool {
    SETTINGS_HOOKED.with(|hooked| {
        if !hooked.get() {
            hooked.set(true);
            if let Some(settings) = ensure_interface_settings() {
                refresh_from_settings(&settings);
                let changed = settings.connect_changed(Some("clock-format"), move |s, _| {
                    refresh_from_settings(s);
                });
                let _ = changed;
            } else {
                CLOCK_24H.set(true);
            }
        }
    });
    CLOCK_24H.with(|c| c.get())
}

/// Format wall clock parts using the given mode.
pub fn format_clock(hour: u32, minute: u32, use_24h: bool) -> String {
    if use_24h {
        return format!("{hour:02}:{minute:02}");
    }
    let mut h12 = hour % 12;
    if h12 == 0 {
        h12 = 12;
    }
    let ampm = if hour < 12 { "AM" } else { "PM" };
    format!("{h12}:{minute:02} {ampm}")
}

/// Format a zoned instant for display using system clock style.
pub fn format_time_of_day<Z: TimeZone>(dt: &DateTime<Z>) -> String {
    format_clock(dt.hour(), dt.minute(), use_24h())
}

/// "9:00 AM – 10:00 AM EST (event time zone, America/New_York)" when the event's
/// zone differs from the viewer's at `at`; `None` when they agree or the zone
/// is unknown.
pub fn event_time_zone_subline(
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    at: DateTime<Utc>,
    viewer: chrono_tz::Tz,
    event_iana: &str,
) -> Option<String> {
    let event_tz: chrono_tz::Tz = event_iana.parse().ok()?;
    let v_off = at.with_timezone(&viewer).offset().fix().local_minus_utc();
    let e_off = at.with_timezone(&event_tz).offset().fix().local_minus_utc();
    if v_off == e_off {
        return None;
    }
    let s = start.with_timezone(&event_tz);
    let e = end.with_timezone(&event_tz);
    let abbr =
        alcedo_core::zone_abbreviation(start, event_iana).unwrap_or_else(|| event_iana.to_string());
    Some(format!(
        "{} – {} {} (event time zone, {})",
        format_time_of_day(&s),
        format_time_of_day(&e),
        abbr,
        event_iana
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_clock_24h_and_12h() {
        assert_eq!(format_clock(15, 30, true), "15:30");
        assert_eq!(format_clock(15, 30, false), "3:30 PM");
        assert_eq!(format_clock(0, 0, false), "12:00 AM");
        assert_eq!(format_clock(12, 0, false), "12:00 PM");
    }
}
