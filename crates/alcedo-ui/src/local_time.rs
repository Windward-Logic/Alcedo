//! Dates and times in the user's language.
//!
//! chrono's `format` always writes English day and month names. These helpers
//! hand the same strftime pattern to GLib, which uses the process locale (set
//! by `alcedo_i18n::init` at startup). The pattern itself is translated at each call
//! site (see `trc!` uses), so a language can also reorder or drop parts, for
//! example 24-hour time instead of `%p`.

use std::fmt::Write as _;
use std::sync::atomic::{AtomicBool, Ordering};

use chrono::{Datelike, Duration, NaiveDate, NaiveDateTime, Timelike, Weekday};
use relm4::gtk::glib;

/// Used when a (translated) pattern is invalid for both GLib and chrono.
const ISO_DATE: &str = "%Y-%m-%d";
const ISO_DATE_TIME: &str = "%Y-%m-%d %H:%M";

/// Set once the invalid-pattern warning has been logged, so a calendar redraw
/// does not log once per cell.
static WARNED_INVALID_PATTERN: AtomicBool = AtomicBool::new(false);

/// Format a calendar date with a strftime `pattern` in the user's locale.
pub fn format_date(date: NaiveDate, pattern: &str) -> String {
    format_naive(
        date.and_hms_opt(0, 0, 0).expect("midnight exists"),
        pattern,
        ISO_DATE,
    )
}

/// Format a local date and time with a strftime `pattern` in the user's locale.
/// Falls back to chrono (English names) if GLib cannot build or format it, and
/// to an ISO date if neither accepts the pattern.
pub fn format_date_time(local: NaiveDateTime, pattern: &str) -> String {
    format_naive(local, pattern, ISO_DATE_TIME)
}

/// The weekday's abbreviated name in the user's locale ("Mon", "lun.", "Mo").
pub fn weekday_short(weekday: Weekday) -> String {
    format_date(reference_date(weekday), "%a")
}

/// The weekday's full name in the user's locale ("Monday", "lundi", "Montag").
pub fn weekday_long(weekday: Weekday) -> String {
    format_date(reference_date(weekday), "%A")
}

/// Any date that falls on `weekday`; only its weekday matters.
fn reference_date(weekday: Weekday) -> NaiveDate {
    let monday = NaiveDate::from_ymd_opt(2024, 1, 1).expect("valid date");
    monday + Duration::days(i64::from(weekday.num_days_from_monday()))
}

fn format_naive(local: NaiveDateTime, pattern: &str, fallback: &'static str) -> String {
    if let Some(text) = glib_format(local, pattern).or_else(|| chrono_format(local, pattern)) {
        return text;
    }
    if !WARNED_INVALID_PATTERN.swap(true, Ordering::Relaxed) {
        tracing::warn!(pattern, "date pattern is not valid; showing an ISO date");
    }
    chrono_format(local, fallback).expect("ISO patterns are valid")
}

/// chrono's `to_string` panics on an invalid pattern; writing returns an error.
fn chrono_format(local: NaiveDateTime, pattern: &str) -> Option<String> {
    let mut text = String::new();
    write!(text, "{}", local.format(pattern)).ok()?;
    Some(text)
}

fn glib_format(local: NaiveDateTime, pattern: &str) -> Option<String> {
    let year = local.year();
    let month = i32::try_from(local.month()).ok()?;
    let day = i32::try_from(local.day()).ok()?;
    let hour = i32::try_from(local.hour()).ok()?;
    let minute = i32::try_from(local.minute()).ok()?;
    let moment = glib::DateTime::new(
        &glib::TimeZone::local(),
        year,
        month,
        day,
        hour,
        minute,
        f64::from(local.second()),
    )
    .ok()?;
    moment.format(pattern).ok().map(|text| text.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(year, month, day)
            .and_then(|d| d.and_hms_opt(hour, minute, 0))
            .expect("valid date")
    }

    #[test]
    fn formats_numeric_fields_like_chrono() {
        let text = format_date_time(at(2026, 9, 3, 9, 5), "%Y-%m-%d %H:%M");
        assert_eq!(text, "2026-09-03 09:05");
    }

    #[test]
    fn unpadded_day_and_names_follow_the_c_locale_in_tests() {
        // Tests run without a locale set, so GLib answers in English.
        let text = format_date(
            NaiveDate::from_ymd_opt(2026, 9, 3).expect("date"),
            "%A, %-d %B",
        );
        assert_eq!(text, "Thursday, 3 September");
    }

    #[test]
    fn weekday_names_follow_the_locale_in_the_c_locale() {
        assert_eq!(weekday_long(Weekday::Sun), "Sunday");
        assert_eq!(weekday_short(Weekday::Wed), "Wed");
    }

    #[test]
    fn locale_modifiers_go_through_glib() {
        // chrono has no `%O`, so this only passes when GLib does the formatting.
        let text = format_date(NaiveDate::from_ymd_opt(2026, 9, 3).expect("date"), "%Od");
        assert_eq!(text, "03");
    }

    #[test]
    fn invalid_pattern_falls_back_to_iso_instead_of_panicking() {
        let day = NaiveDate::from_ymd_opt(2026, 9, 3).expect("date");
        assert_eq!(format_date(day, "%A %Q"), "2026-09-03");
        assert_eq!(
            format_date_time(at(2026, 9, 3, 9, 5), "%Q"),
            "2026-09-03 09:05"
        );
    }

    #[test]
    fn escaped_percent_is_kept() {
        let text = format_date_time(at(2026, 9, 3, 9, 5), "%H:%M %%");
        assert_eq!(text, "09:05 %");
    }
}
