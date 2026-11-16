//! Meeting-invite mock data.

use super::*;

pub(super) fn mock_meeting_invite_ics() -> String {
    use chrono::{Datelike, Duration, Weekday};

    let today = chrono::Utc::now().date_naive();
    let mut days_ahead = (Weekday::Thu.num_days_from_monday() as i64
        - today.weekday().num_days_from_monday() as i64
        + 7)
        % 7;
    if days_ahead == 0 {
        days_ahead = 7;
    }
    let thursday = today + Duration::days(days_ahead);
    let start = thursday.and_hms_opt(15, 0, 0).unwrap().and_utc();
    let end = thursday.and_hms_opt(16, 0, 0).unwrap().and_utc();
    format!(
        "BEGIN:VCALENDAR\r\n\
         VERSION:2.0\r\n\
         METHOD:REQUEST\r\n\
         PRODID:-//Alcedo//Mock//EN\r\n\
         BEGIN:VEVENT\r\n\
         UID:mock-standup@alcedo.mail\r\n\
         SUMMARY:Team standup\r\n\
         ORGANIZER;CN=Alex:mailto:organizer@example.com\r\n\
         ATTENDEE;CN=You;PARTSTAT=NEEDS-ACTION:mailto:you@example.com\r\n\
         DTSTART:{}\r\n\
         DTEND:{}\r\n\
         LOCATION:Conference Room B\r\n\
         END:VEVENT\r\n\
         END:VCALENDAR\r\n",
        start.format("%Y%m%dT%H%M%SZ"),
        end.format("%Y%m%dT%H%M%SZ"),
    )
}

pub fn mock_seed_meeting_invite(
    envelopes: &[Envelope],
    attachments_by_id: &mut HashMap<MessageId, Vec<BodyAttachment>>,
    cid_cache: &CidCache,
) {
    use sha2::{Digest, Sha256};

    let Some(envelope) = envelopes
        .iter()
        .find(|env| env.subject == "Meeting moved to Thursday")
    else {
        return;
    };
    let ics = mock_meeting_invite_ics();
    let bytes = ics.into_bytes();
    let sha256 = format!("{:x}", Sha256::digest(&bytes));
    let size = bytes.len() as u64;
    cid_cache
        .borrow_mut()
        .insert(sha256.clone(), (bytes, "text/calendar".to_string()));
    attachments_by_id.insert(
        envelope.id,
        vec![BodyAttachment {
            filename: "invite.ics".to_string(),
            mime: "text/calendar".to_string(),
            sha256,
            size,
        }],
    );
}
