//! Build and send calendar invite RSVP (METHOD:REPLY) messages.

use super::MailModel;
use super::reply::apply_reply_from;
use crate::extension::CalendarInvite;
use crate::meeting_rsvp::add_to_calendar_with_extension;
use alcedo_core::{
    AccountIdentity, ComposeAttachment, ComposeDraft, ComposeFormat, Envelope, ParticipationStatus,
    ReplyFrom, ReplyPlan, apply_participation, build_ics, build_reply_ics,
    calendar_invite_reply_subject, parse_ics_with_method, rsvp_responder_email,
};

#[derive(Clone, Debug)]
pub struct InviteResponsePayload {
    pub message_id: alcedo_core::MessageId,
    pub status: ParticipationStatus,
    pub ics: String,
}

impl MailModel {
    pub(super) fn send_invite_response(&mut self, payload: InviteResponsePayload) {
        if self.send_in_flight.get() {
            self.toasts.add_toast(crate::toast::plain(
                "Wait until the current message finishes sending.",
            ));
            return;
        }
        let InviteResponsePayload {
            message_id,
            status,
            ics,
        } = payload;
        let Some(envelope) = self.envelope_for(message_id).cloned() else {
            self.toasts.add_toast(crate::toast::plain(
                "Couldn't send a response to this invitation",
            ));
            return;
        };
        let Some(account) = self
            .accounts
            .iter()
            .find(|a| a.id == envelope.account_id)
            .cloned()
        else {
            self.toasts.add_toast(crate::toast::plain(
                "Couldn't send a response to this invitation",
            ));
            return;
        };
        let parsed = match parse_ics_with_method(ics.as_bytes()) {
            Ok(p) => p,
            Err(_) => {
                self.toasts.add_toast(crate::toast::plain(
                    "Could not read this calendar invitation",
                ));
                return;
            }
        };
        let mut drafts = parsed.drafts;
        if drafts.is_empty() {
            self.toasts.add_toast(crate::toast::plain(
                "Could not read this calendar invitation",
            ));
            return;
        }

        let (in_reply_to, references) = self.threading_for_reply(message_id);
        let (plan, fallback, responder_email) = {
            let planner = self.reply_planner_for(&envelope);
            let plan = planner.plan(alcedo_core::ReplyMode::Sender);
            let identities = self
                .identities
                .get(&envelope.account_id)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            let fallback = reply_from_address(&plan.from, account.email.as_str(), identities);
            let recipients = invitation_recipient_addresses(&envelope);
            let responder_email = rsvp_responder_email(&drafts, &recipients, &fallback, |email| {
                planner.address_is_self(email)
            });
            (plan, fallback, responder_email)
        };
        let responder_name =
            (!account.display_name.trim().is_empty()).then_some(account.display_name.as_str());
        let organizer_label = organizer_display_for_toast(&mut drafts);

        let mut applied_drafts = drafts.clone();
        for d in &mut applied_drafts {
            apply_participation(d, &responder_email, responder_name, status);
        }

        if matches!(
            status,
            ParticipationStatus::Accepted | ParticipationStatus::Tentative
        ) {
            let mut combined_ics = String::new();
            for d in &applied_drafts {
                combined_ics.push_str(&build_ics(d));
            }
            let invite = CalendarInvite {
                drafts: applied_drafts.clone(),
                ics: combined_ics,
                sha256: String::new(),
                filename: String::new(),
            };
            let anchor = self.invite_banner(message_id).calendar_destination_anchor();
            add_to_calendar_with_extension(&anchor, &invite);
        }
        self.invite_banner(message_id).mark_rsvp_in_progress();

        let reply_ics = build_reply_ics(&applied_drafts, &responder_email, responder_name, status);
        let responder_display = if account.display_name.trim().is_empty() {
            account.email.as_str()
        } else {
            account.display_name.as_str()
        };
        let mut draft = build_invite_reply_draft(
            &envelope,
            account.id,
            &plan,
            status,
            &drafts,
            in_reply_to,
            references,
            reply_ics.into_bytes(),
            responder_display,
        );
        apply_reply_from(&mut draft, &plan.from, &account);
        if !responder_email.eq_ignore_ascii_case(&fallback) {
            apply_reply_from(&mut draft, &ReplyFrom::Alias(responder_email), &account);
        }

        self.pending_invite_response_organizer = Some(organizer_label);
        if !self.queue_outgoing_send(draft, None) {
            self.pending_invite_response_organizer = None;
            self.invite_banner(message_id).mark_rsvp_idle();
        }
    }

    fn invite_banner(
        &self,
        message_id: alcedo_core::MessageId,
    ) -> &crate::meeting_invite::MeetingBanner {
        if self.selected == Some(message_id) {
            &self.reader.meeting_banner
        } else if let Some(window) = self.message_windows.get(&message_id) {
            &window.reader.meeting_banner
        } else {
            &self.reader.meeting_banner
        }
    }
}

fn invitation_recipient_addresses(envelope: &Envelope) -> Vec<&str> {
    let routing = envelope.routing_headers();
    routing
        .delivered_to
        .iter()
        .map(String::as_str)
        .chain(envelope.to.iter().map(|address| address.email.as_str()))
        .chain(envelope.cc.iter().map(|address| address.email.as_str()))
        .filter(|email| !email.trim().is_empty())
        .collect()
}

fn reply_from_address(
    from: &ReplyFrom,
    account_email: &str,
    identities: &[AccountIdentity],
) -> String {
    match from {
        ReplyFrom::AccountDefault => account_email.to_string(),
        ReplyFrom::Alias(email) => email.clone(),
        ReplyFrom::Identity(id) => identities
            .iter()
            .find(|identity| identity.id == *id)
            .map(|identity| identity.email.clone())
            .unwrap_or_else(|| account_email.to_string()),
    }
}

fn organizer_display_for_toast(drafts: &mut [alcedo_core::CalendarEventDraft]) -> String {
    drafts
        .iter_mut()
        .find_map(|d| d.details.organizer.as_mut())
        .map(|o| {
            o.name
                .clone()
                .filter(|n| !n.trim().is_empty())
                .unwrap_or_else(|| o.email.clone())
        })
        .unwrap_or_else(|| "organizer".to_string())
}

fn build_invite_reply_draft(
    envelope: &Envelope,
    account_id: alcedo_core::AccountId,
    plan: &ReplyPlan,
    status: ParticipationStatus,
    drafts: &[alcedo_core::CalendarEventDraft],
    in_reply_to: Option<String>,
    references: Option<String>,
    attachment_bytes: Vec<u8>,
    responder_display_name: &str,
) -> ComposeDraft {
    let summary = drafts
        .first()
        .map(|d| d.summary.as_str())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("Meeting");
    let subject = calendar_invite_reply_subject(status, summary);
    let organizer = drafts.iter().find_map(|d| d.details.organizer.as_ref());
    let to: Vec<String> = organizer
        .map(|o| vec![o.email.clone()])
        .filter(|v| !v[0].trim().is_empty())
        .unwrap_or_else(|| plan.to.clone());
    let who = responder_display_name;
    let body_text = match status {
        ParticipationStatus::Accepted => format!("{who} has accepted: {summary}"),
        ParticipationStatus::Tentative => format!("{who} has tentatively accepted: {summary}"),
        ParticipationStatus::Declined => format!("{who} has declined: {summary}"),
        _ => format!("{who} has responded: {summary}"),
    };
    ComposeDraft {
        account_id,
        to,
        cc: plan.cc.clone(),
        bcc: Vec::new(),
        subject,
        body_text,
        format: ComposeFormat::Plain,
        request_mdn: false,
        tracking_pixel_url: None,
        attachments: vec![ComposeAttachment {
            filename: "invite.ics".into(),
            mime: "text/calendar; method=REPLY; charset=UTF-8".into(),
            bytes: attachment_bytes,
        }],
        inlines: Vec::new(),
        collect_outgoing_contacts: false,
        in_reply_to,
        references,
        thread_id: Some(envelope.thread_id),
        approve_contact_email: None,
        sign: false,
        encrypt: false,
        autocrypt_header: None,
        identity_id: None,
        custom_from: None,
        imap_draft: None,
        reaction_emoji: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alcedo_core::{
        AccountId, Address, EventDetails, EventPerson, FolderId, MessageId, ThreadId,
    };
    use chrono::Utc;
    use uuid::Uuid;

    #[test]
    fn draft_has_organizer_subject_and_ics_attachment() {
        let env = Envelope {
            id: MessageId(1),
            account_id: AccountId(Uuid::nil()),
            folder_id: FolderId(0),
            uid: 0,
            thread_id: ThreadId(Uuid::nil()),
            date: Utc::now(),
            subject: String::new(),
            from: vec![Address {
                name: None,
                email: "o@x.com".into(),
            }],
            to: vec![],
            cc: vec![],
            reply_to: vec![],
            sender: vec![],
            routing: None,
            snippet: String::new(),
            seen: false,
            flagged: false,
            answered: false,
            draft: false,
            has_attachments: false,
            size_bytes: 0,
            deleted_at: None,
            account_accent: String::new(),
            account_avatar: String::new(),
        };
        let plan = ReplyPlan {
            to: vec!["fallback@x.com".into()],
            cc: vec![],
            from: alcedo_core::ReplyFrom::AccountDefault,
        };
        let drafts = vec![alcedo_core::CalendarEventDraft {
            uid: "u".into(),
            summary: "Design sync".into(),
            location: None,
            description: None,
            start: Utc::now(),
            end: Utc::now(),
            all_day: false,
            recurrence_id: None,
            rrule: None,
            source_message_id: None,
            cancelled: false,
            details: EventDetails {
                organizer: Some(EventPerson {
                    email: "org@meet.com".into(),
                    name: Some("Pat".into()),
                }),
                ..Default::default()
            },
        }];
        let d = build_invite_reply_draft(
            &env,
            AccountId(Uuid::nil()),
            &plan,
            ParticipationStatus::Accepted,
            &drafts,
            None,
            None,
            b"ICS".to_vec(),
            "Ada",
        );
        assert_eq!(d.to, vec!["org@meet.com".to_string()]);
        assert!(d.subject.starts_with("Accepted:"));
        assert_eq!(d.body_text, "Ada has accepted: Design sync");
        assert_eq!(d.attachments.len(), 1);
        assert_eq!(d.attachments[0].filename, "invite.ics");
        assert!(
            d.attachments[0]
                .mime
                .to_ascii_lowercase()
                .contains("method=reply")
        );
    }
}
