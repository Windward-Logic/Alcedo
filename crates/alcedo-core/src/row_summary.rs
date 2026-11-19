//! Accessible name for one message-list row.

use crate::subject::NO_SUBJECT;
use alcedo_i18n::{ntr, tr, trf, trm, trs};

/// Fields a screen reader needs from a message row. The list only knows
/// whether a row has attachments, not how many.
pub struct RowSummary<'a> {
    pub unread_count: u32,
    pub starred: bool,
    pub from: &'a str,
    pub subject: &'a str,
    pub when: &'a str,
    pub has_attachments: bool,
}

/// One spoken summary, for example
/// "Unread, starred, from Ada Lovelace, ‘Quarterly report’, 10:42, has attachments".
pub fn row_accessible_summary(row: &RowSummary<'_>) -> String {
    let mut parts = Vec::new();
    match row.unread_count {
        0 => {}
        1 => parts.push(tr!("Unread")),
        n => parts.push(ntr!("{n} unread", "{n} unread", n)),
    }
    if row.starred {
        parts.push(tr!("starred"));
    }
    let from = row.from.trim();
    let from = if from.is_empty() {
        trs!("Unknown")
    } else {
        from
    };
    // TRANSLATORS: part of a spoken message-row summary, e.g. "from Ada Lovelace".
    parts.push(trf!("from {sender}", sender = from));

    let subject = row.subject.trim();
    let subject = if subject.is_empty() {
        trm!(NO_SUBJECT)
    } else {
        subject
    };
    parts.push(format!("‘{subject}’"));

    let when = row.when.trim();
    if !when.is_empty() {
        parts.push(when.to_string());
    }
    if row.has_attachments {
        parts.push(tr!("has attachments"));
    }
    parts.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_includes_unread_star_sender_subject_time_and_attachments() {
        let summary = row_accessible_summary(&RowSummary {
            unread_count: 1,
            starred: true,
            from: "Ada Lovelace",
            subject: "Quarterly report",
            when: "10:42",
            has_attachments: true,
        });
        assert_eq!(
            summary,
            "Unread, starred, from Ada Lovelace, ‘Quarterly report’, 10:42, has attachments"
        );
    }

    #[test]
    fn read_unstarred_row_omits_those_words() {
        let summary = row_accessible_summary(&RowSummary {
            unread_count: 0,
            starred: false,
            from: "Grace Hopper",
            subject: "Hello",
            when: "Mon 9:00 AM",
            has_attachments: false,
        });
        assert_eq!(summary, "from Grace Hopper, ‘Hello’, Mon 9:00 AM");
    }

    #[test]
    fn thread_unread_count_and_empty_fields() {
        let summary = row_accessible_summary(&RowSummary {
            unread_count: 3,
            starred: false,
            from: "",
            subject: "  ",
            when: "",
            has_attachments: false,
        });
        assert_eq!(summary, format!("3 unread, from Unknown, ‘{NO_SUBJECT}’"));
    }
}
