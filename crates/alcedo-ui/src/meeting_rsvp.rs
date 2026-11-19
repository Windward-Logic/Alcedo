//! RSVP controls for meeting invites, and handing an invite to "Add to calendar".

use alcedo_i18n::tr;
use std::cell::RefCell;
use std::rc::Rc;

use alcedo_core::{
    CalendarEventDraft, InviteMethod, ParticipationStatus, build_ics,
    draft_from_google_template_url,
};
use alcedo_widgets::CidCache;
use relm4::adw::prelude::*;
use relm4::gtk;
use relm4::gtk::prelude::IsA;

use crate::extension::CalendarInvite;

/// Let an extension add `invite` to one of its calendars.
pub(crate) fn add_to_calendar_with_extension(
    anchor: &impl IsA<gtk::Widget>,
    invite: &CalendarInvite,
) -> bool {
    let anchor = anchor.as_ref();
    crate::extension::registered()
        .iter()
        .any(|extension| extension.add_invite_to_calendar(anchor, invite))
}

/// "Add to Calendar": an extension's calendars when one handles it, else the
/// invitation file opens in the system's calendar app.
pub(crate) fn offer_add_to_calendar(
    anchor: &impl IsA<gtk::Widget>,
    invite: &CalendarInvite,
    cid_cache: Option<&CidCache>,
) {
    if add_to_calendar_with_extension(anchor, invite) {
        return;
    }
    let Some(cache) = cid_cache.filter(|_| !invite.sha256.is_empty()) else {
        return;
    };
    let parent = anchor
        .as_ref()
        .root()
        .and_then(|root| root.downcast::<gtk::Window>().ok());
    crate::reader::open_attachment_file(cache, &invite.sha256, &invite.filename, parent);
}

/// True when Accept / Tentative / Decline should appear.
pub fn rsvp_available(
    method: InviteMethod,
    drafts: &[CalendarEventDraft],
    account_email: &str,
) -> bool {
    if method != InviteMethod::Request {
        return false;
    }
    let Some(d) = drafts.first() else {
        return false;
    };
    if d.cancelled {
        return false;
    }
    let Some(org) = d.details.organizer.as_ref() else {
        return false;
    };
    !org.email.eq_ignore_ascii_case(account_email.trim())
}

pub fn build_rsvp_buttons(on_choice: Rc<dyn Fn(ParticipationStatus)>) -> gtk::Box {
    let bx = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(0)
        .css_classes(["linked"])
        .build();

    let accept = gtk::Button::builder()
        .label(tr!("Accept"))
        .css_classes(["suggested-action"])
        .tooltip_text(tr!("Accept this invitation"))
        .build();
    let tentative = gtk::Button::builder()
        .label(tr!("Tentative"))
        .tooltip_text(tr!("Reply that you might attend"))
        .build();
    let decline = gtk::Button::builder()
        .label(tr!("Decline"))
        .tooltip_text(tr!("Decline this invitation"))
        .build();

    accept.connect_clicked({
        let on_choice = on_choice.clone();
        move |_| on_choice(ParticipationStatus::Accepted)
    });
    tentative.connect_clicked({
        let on_choice = on_choice.clone();
        move |_| on_choice(ParticipationStatus::Tentative)
    });
    decline.connect_clicked(move |_| on_choice(ParticipationStatus::Declined));

    bx.append(&accept);
    bx.append(&tentative);
    bx.append(&decline);
    bx
}

/// Handles for calendar links in the reader (same handling as “Add to Calendar”).
pub(crate) struct MeetingBannerHandles {
    anchor: gtk::Button,
    banner_root: gtk::Box,
    state: Rc<RefCell<Option<CalendarInvite>>>,
}

impl MeetingBannerHandles {
    pub(crate) fn new(
        anchor: gtk::Button,
        banner_root: gtk::Box,
        state: Rc<RefCell<Option<CalendarInvite>>>,
    ) -> Self {
        Self {
            anchor,
            banner_root,
            state,
        }
    }

    /// Offer a calendar link (or the shown invitation) to an extension's
    /// calendars. `false` leaves the link to open as usual.
    pub(crate) fn try_present_calendar_link(
        &self,
        uri: &str,
        fallback_anchor: &impl IsA<gtk::Widget>,
    ) -> bool {
        let Some(invite) = self.popover_state_for_uri(uri) else {
            return false;
        };
        if self.banner_root.is_visible() {
            add_to_calendar_with_extension(&self.anchor, &invite)
        } else {
            add_to_calendar_with_extension(fallback_anchor, &invite)
        }
    }

    fn popover_state_for_uri(&self, uri: &str) -> Option<CalendarInvite> {
        if let Some(state) = self.state.borrow().clone()
            && !state.drafts.is_empty()
        {
            return Some(state);
        }
        let draft = draft_from_google_template_url(uri)?;
        Some(CalendarInvite {
            drafts: vec![draft.clone()],
            ics: build_ics(&draft),
            sha256: String::new(),
            filename: String::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alcedo_core::EventDetails;

    fn draft_with_org(email: &str) -> CalendarEventDraft {
        CalendarEventDraft {
            uid: "u".into(),
            summary: "s".into(),
            location: None,
            description: None,
            start: chrono::Utc::now(),
            end: chrono::Utc::now(),
            all_day: false,
            recurrence_id: None,
            rrule: None,
            source_message_id: None,
            cancelled: false,
            details: EventDetails {
                organizer: Some(alcedo_core::EventPerson {
                    email: email.into(),
                    name: None,
                }),
                ..Default::default()
            },
        }
    }

    #[test]
    fn rsvp_available_rules() {
        let d = draft_with_org("org@x.com");
        assert!(rsvp_available(
            InviteMethod::Request,
            std::slice::from_ref(&d),
            "me@x.com"
        ));
    }

    #[test]
    fn rsvp_hidden_when_you_are_organizer_or_cancelled_or_publish() {
        let d = draft_with_org("me@x.com");
        assert!(!rsvp_available(InviteMethod::Request, &[d], "me@x.com"));
        let mut cancelled = draft_with_org("org@x.com");
        cancelled.cancelled = true;
        assert!(!rsvp_available(
            InviteMethod::Request,
            &[cancelled],
            "me@x.com"
        ));
        let d2 = draft_with_org("org@x.com");
        assert!(!rsvp_available(InviteMethod::Publish, &[d2], "me@x.com"));
    }
}
