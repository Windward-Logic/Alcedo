//! Helpers around `gtk::Calendar` used for the jump-to-date popover and date pickers.
use std::cell::Cell;
use std::thread_local;

use chrono::{Datelike, NaiveDate};
use relm4::gtk;
use relm4::gtk::glib;

pub fn calendar_naive_date(calendar: &gtk::Calendar) -> NaiveDate {
    let dt = calendar.date();
    NaiveDate::from_ymd_opt(dt.year(), dt.month() as u32, dt.day_of_month() as u32)
        .expect("gtk::Calendar date is valid")
}

thread_local! {
    static PROGRAMMATIC_DAY_SELECT: Cell<bool> = const { Cell::new(false) };
}

/// False while `select_calendar_day` is moving a calendar, so `day-selected` handlers can ignore
/// the echo of our own change and only react to the user picking a day.
pub fn day_selected_by_user() -> bool {
    !PROGRAMMATIC_DAY_SELECT.with(Cell::get)
}

/// Noon on `date` as a GLib date-time, or `None` when GLib cannot represent
/// it (it only covers years 1 to 9999, while event data can carry any year).
pub fn glib_local_noon(date: NaiveDate) -> Option<glib::DateTime> {
    glib::DateTime::from_local(
        date.year(),
        date.month() as i32,
        date.day() as i32,
        12,
        0,
        0.0,
    )
    .ok()
}

pub fn select_calendar_day(calendar: &gtk::Calendar, date: NaiveDate) {
    let Some(dt) = glib_local_noon(date) else {
        tracing::debug!(%date, "date is outside the range GTK can show; calendar left unchanged");
        return;
    };
    PROGRAMMATIC_DAY_SELECT.with(|flag| flag.set(true));
    #[allow(deprecated)]
    calendar.select_day(&dt);
    PROGRAMMATIC_DAY_SELECT.with(|flag| flag.set(false));
}
