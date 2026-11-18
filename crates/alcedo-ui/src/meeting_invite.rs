//! Meeting invitation banner: summary, RSVP and "Add to Calendar".

use std::cell::RefCell;
use std::rc::Rc;

use alcedo_core::{BodyAttachment, is_calendar_part, parse_ics_with_method, zone_abbreviation};
use alcedo_widgets::CidCache;
use chrono::{DateTime, Utc};
use relm4::Sender;
use relm4::adw::prelude::*;
use relm4::gtk;

use crate::app::AppMsg;
use crate::calendar_clock::{event_time_zone_subline, format_time_of_day};
use crate::extension::CalendarInvite;
use crate::meeting_rsvp::{
    MeetingBannerHandles, build_rsvp_buttons, offer_add_to_calendar, rsvp_available,
};

/// Reader widgets for a calendar invitation banner.
pub struct MeetingBanner {
    pub root: gtk::Box,
    title: gtk::Label,
    when: gtk::Label,
    where_label: gtk::Label,
    rsvp_box: gtk::Box,
    add_btn: gtk::Button,
    state: Rc<RefCell<Option<CalendarInvite>>>,
    cid_cache: Rc<RefCell<Option<CidCache>>>,
    button_ready: Rc<RefCell<bool>>,
    message_id: Rc<RefCell<Option<alcedo_core::MessageId>>>,
    account_email: Rc<RefCell<String>>,
}

/// Format event start/end for the reader banner (viewer zone, with abbreviation).
pub fn format_invite_when(start: DateTime<Utc>, end: DateTime<Utc>, all_day: bool) -> String {
    format_invite_when_with_zones(start, end, all_day, chrono_tz::Tz::UTC)
}

/// Same as [`format_invite_when`] but uses explicit zones for tests (no machine zone).
pub fn format_invite_when_with_zones(
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    all_day: bool,
    viewer: chrono_tz::Tz,
) -> String {
    if all_day {
        let day = start.naive_utc().date().format("%b %d").to_string();
        return format!("{day} (all day)");
    }

    let start_v = start.with_timezone(&viewer);
    let end_v = end.with_timezone(&viewer);
    let abbr = zone_abbreviation(start, viewer.name()).unwrap_or_else(|| viewer.name().to_string());
    if start_v.date_naive() == end_v.date_naive() {
        let mut s = format!(
            "{}, {} – {}",
            start_v.format("%a, %b %d"),
            format_time_of_day(&start_v),
            format_time_of_day(&end_v)
        );
        s.push_str(" · ");
        s.push_str(&abbr);
        return s;
    }
    format!(
        "{}, {} – {}, {} · {abbr}",
        start_v.format("%a, %b %d"),
        format_time_of_day(&start_v),
        end_v.format("%a, %b %d"),
        format_time_of_day(&end_v),
    )
}

fn append_event_zone_extra(
    base: &str,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    all_day: bool,
    viewer: chrono_tz::Tz,
    event_iana: Option<&str>,
) -> String {
    if all_day {
        return base.to_string();
    }
    let Some(iana) = event_iana.filter(|s| !s.is_empty()) else {
        return base.to_string();
    };
    let Some(extra) = event_time_zone_subline(start, end, start, viewer, iana) else {
        return base.to_string();
    };
    format!("{base} · {}", extra.lines().next().unwrap_or(&extra))
}

/// Build a hidden meeting banner inserted below the reader header zone.
pub fn build_meeting_banner() -> MeetingBanner {
    let icon = gtk::Image::from_icon_name("x-office-calendar-symbolic");
    icon.add_css_class("alcedo-attachment-card-icon");
    icon.set_valign(gtk::Align::Start);

    let title = gtk::Label::builder()
        .css_classes(["alcedo-meeting-banner-title"])
        .xalign(0.0)
        .hexpand(true)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .max_width_chars(40)
        .build();

    let when = gtk::Label::builder()
        .css_classes(["alcedo-meeting-banner-meta", "dim-label"])
        .xalign(0.0)
        .hexpand(true)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .max_width_chars(40)
        .build();

    let where_label = gtk::Label::builder()
        .css_classes(["alcedo-meeting-banner-meta", "dim-label"])
        .xalign(0.0)
        .hexpand(true)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .max_width_chars(40)
        .build();

    let rsvp_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(0)
        .visible(false)
        .build();

    let add_btn = gtk::Button::builder()
        .label("Add to Calendar")
        .css_classes(["suggested-action"])
        .tooltip_text("Add this meeting to a calendar")
        .valign(gtk::Align::Center)
        .halign(gtk::Align::End)
        .build();

    let labels = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .hexpand(true)
        .build();
    labels.append(&title);
    labels.append(&when);
    labels.append(&where_label);

    let root = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .css_classes(["alcedo-meeting-banner"])
        .visible(false)
        .accessible_role(gtk::AccessibleRole::Group)
        .build();
    root.append(&icon);
    root.append(&labels);
    root.append(&rsvp_box);
    root.append(&add_btn);

    let state = Rc::new(RefCell::new(None));
    let cid_cache = Rc::new(RefCell::new(None::<CidCache>));
    let button_ready = Rc::new(RefCell::new(false));
    let message_id = Rc::new(RefCell::new(None));
    let account_email = Rc::new(RefCell::new(String::new()));

    add_btn.connect_clicked({
        let state = state.clone();
        let cid_cache = cid_cache.clone();
        let button_ready = button_ready.clone();
        move |add_btn| {
            if !*button_ready.borrow() {
                return;
            }
            let Some(invite) = state.borrow().clone() else {
                return;
            };
            offer_add_to_calendar(add_btn, &invite, cid_cache.borrow().as_ref());
        }
    });

    MeetingBanner {
        root,
        title,
        when,
        where_label,
        rsvp_box,
        add_btn,
        state,
        cid_cache,
        button_ready,
        message_id,
        account_email,
    }
}

impl MeetingBanner {
    /// Widget the calendar chooser should point at.
    ///
    /// Accept hides "Add to Calendar". A popover parented to that hidden
    /// button never appears, so the chooser uses the RSVP row instead.
    pub fn calendar_destination_anchor(&self) -> gtk::Widget {
        if self.rsvp_box.is_visible() {
            self.rsvp_box.clone().upcast()
        } else {
            self.add_btn.clone().upcast()
        }
    }

    pub(crate) fn mark_rsvp_in_progress(&self) {
        self.rsvp_box.set_sensitive(false);
    }

    pub(crate) fn mark_rsvp_idle(&self) {
        self.rsvp_box.set_sensitive(true);
    }

    pub(crate) fn handles(&self) -> MeetingBannerHandles {
        MeetingBannerHandles::new(self.add_btn.clone(), self.root.clone(), self.state.clone())
    }
}

/// Show or hide the banner based on calendar attachments in the current message.
pub fn update_meeting_banner(
    banner: &MeetingBanner,
    message_id: alcedo_core::MessageId,
    account_email: &str,
    attachments: &[BodyAttachment],
    cid_cache: &CidCache,
    sender: &Sender<AppMsg>,
) {
    *banner.cid_cache.borrow_mut() = Some(cid_cache.clone());
    *banner.message_id.borrow_mut() = Some(message_id);
    *banner.account_email.borrow_mut() = account_email.to_string();

    while let Some(c) = banner.rsvp_box.first_child() {
        banner.rsvp_box.remove(&c);
    }

    let Some(att) = attachments
        .iter()
        .find(|att| is_calendar_part(&att.mime, &att.filename))
    else {
        banner.root.set_visible(false);
        banner
            .root
            .remove_css_class("alcedo-meeting-banner-cancelled");
        *banner.state.borrow_mut() = None;
        return;
    };

    banner.root.set_visible(true);
    banner.title.set_label("Calendar invitation");
    banner.when.set_label("Loading calendar file…");
    banner.where_label.set_label("");
    banner.where_label.set_visible(false);
    banner.add_btn.set_sensitive(false);
    banner.rsvp_box.set_visible(false);
    banner
        .add_btn
        .set_tooltip_text(Some("Loading calendar file…"));
    *banner.button_ready.borrow_mut() = false;
    *banner.state.borrow_mut() = Some(CalendarInvite {
        drafts: Vec::new(),
        ics: String::new(),
        sha256: att.sha256.clone(),
        filename: att.filename.clone(),
    });

    let Some((bytes, _mime)) = cid_cache.borrow().get(&att.sha256).cloned() else {
        return;
    };

    let ics = String::from_utf8_lossy(&bytes).into_owned();
    let viewer = alcedo_core::system_zone_name()
        .as_deref()
        .and_then(|n| n.parse::<chrono_tz::Tz>().ok())
        .unwrap_or(chrono_tz::Tz::UTC);

    match parse_ics_with_method(&bytes) {
        Ok(parsed) if !parsed.drafts.is_empty() => {
            let method = parsed.method;
            let mut drafts = parsed.drafts;
            let extra = drafts.len().saturating_sub(1);
            let first = &drafts[0];
            let mut title = first.summary.clone();
            if extra > 0 {
                title.push_str(&format!(" (+{extra} more)"));
            }
            banner.title.set_label(&title);

            let mut when_text =
                format_invite_when_with_zones(first.start, first.end, first.all_day, viewer);
            when_text = append_event_zone_extra(
                &when_text,
                first.start,
                first.end,
                first.all_day,
                viewer,
                first.details.time_zone.as_deref(),
            );
            banner.when.set_label(&when_text);

            if first.cancelled {
                banner.root.add_css_class("alcedo-meeting-banner-cancelled");
            } else {
                banner
                    .root
                    .remove_css_class("alcedo-meeting-banner-cancelled");
            }

            let show_rsvp = rsvp_available(method, &drafts, account_email) && !first.cancelled;

            if show_rsvp {
                banner.add_btn.set_visible(false);
                banner.rsvp_box.set_visible(true);
                banner.rsvp_box.set_sensitive(true);
                let mid = message_id;
                let ics_for_send = ics.clone();
                let on_choice: Rc<dyn Fn(alcedo_core::ParticipationStatus)> = Rc::new({
                    let sender = sender.clone();
                    move |status| {
                        sender
                            .send(AppMsg::RespondToInvite {
                                message_id: mid,
                                status,
                                ics: ics_for_send.clone(),
                            })
                            .ok();
                    }
                });
                let row = build_rsvp_buttons(on_choice);
                banner.rsvp_box.append(&row);
                banner
                    .add_btn
                    .set_tooltip_text(Some("Respond using the buttons"));
            } else {
                banner.rsvp_box.set_visible(false);
                banner.add_btn.set_visible(true);
                if first.cancelled {
                    banner
                        .add_btn
                        .set_tooltip_text(Some("Add this cancelled meeting to a calendar"));
                } else {
                    banner
                        .add_btn
                        .set_tooltip_text(Some("Add this meeting to a calendar"));
                }
            }

            if let Some(location) = &first.location {
                banner.where_label.set_label(location);
                banner.where_label.set_visible(true);
            } else {
                banner.where_label.set_label("");
                banner.where_label.set_visible(false);
            }
            banner.add_btn.set_sensitive(true);
            *banner.button_ready.borrow_mut() = true;
            if let Some(state) = banner.state.borrow_mut().as_mut() {
                state.drafts = std::mem::take(&mut drafts);
                state.ics = ics;
            }
        }
        Ok(_) | Err(_) => {
            banner.when.set_label("");
            banner.where_label.set_visible(false);
            banner.rsvp_box.set_visible(false);
            banner.add_btn.set_visible(true);
            banner
                .root
                .remove_css_class("alcedo-meeting-banner-cancelled");
            banner.add_btn.set_sensitive(false);
            banner
                .add_btn
                .set_tooltip_text(Some("Could not read this calendar invitation"));
            *banner.button_ready.borrow_mut() = false;
            *banner.state.borrow_mut() = None;
        }
    }
}

pub fn hide_meeting_banner(banner: &MeetingBanner) {
    banner.root.set_visible(false);
    banner
        .root
        .remove_css_class("alcedo-meeting-banner-cancelled");
    *banner.state.borrow_mut() = None;
    *banner.button_ready.borrow_mut() = false;
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use chrono_tz::America::Los_Angeles;
    use chrono_tz::Europe::Paris;

    #[test]
    fn format_invite_when_all_day() {
        let start = Utc.with_ymd_and_hms(2025, 8, 21, 0, 0, 0).unwrap();
        let end = Utc.with_ymd_and_hms(2025, 8, 22, 0, 0, 0).unwrap();
        let formatted = format_invite_when_with_zones(start, end, true, Los_Angeles);
        assert!(formatted.contains("(all day)"));
        assert!(formatted.contains("Aug 21"));
        assert!(!formatted.contains("Aug 20"));
    }

    #[test]
    fn invite_when_includes_zone_abbreviation() {
        let start = Paris.with_ymd_and_hms(2026, 6, 15, 14, 0, 0).unwrap();
        let end = Paris.with_ymd_and_hms(2026, 6, 15, 15, 0, 0).unwrap();
        let s = format_invite_when_with_zones(
            start.with_timezone(&Utc),
            end.with_timezone(&Utc),
            false,
            Paris,
        );
        assert!(s.contains('·'));
    }
}
