//! User-facing confirmation copy for mail actions (toasts after archive,
//! move, spam). Pure so wording and pluralisation stay consistent and tested.

/// What a completed move did, from the user's point of view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MoveFeedback<'a> {
    /// Moved to the account's Archive folder.
    Archived,
    /// Moved to Junk.
    MarkedSpam,
    /// Moved from Junk back to the Inbox.
    NotSpam,
    /// Moved to a named folder.
    MovedTo(&'a str),
    /// Copied to a named folder (originals stay put).
    CopiedTo(&'a str),
}

/// Toast title confirming a move of `count` messages.
pub fn move_toast_title(feedback: MoveFeedback<'_>, count: usize) -> String {
    let subject = if count == 1 {
        "Message".to_string()
    } else {
        format!("{count} messages")
    };
    match feedback {
        MoveFeedback::Archived => format!("{subject} archived"),
        MoveFeedback::MarkedSpam => format!("{subject} marked as spam"),
        MoveFeedback::NotSpam => format!("{subject} moved to Inbox"),
        MoveFeedback::MovedTo(folder) => format!("{subject} moved to {}", folder_label(folder)),
        MoveFeedback::CopiedTo(folder) => format!("{subject} copied to {}", folder_label(folder)),
    }
}

/// Toast title after the user undoes a move.
pub fn undo_move_toast_title(count: usize) -> String {
    if count == 1 {
        "Message moved back".to_string()
    } else {
        format!("{count} messages moved back")
    }
}

fn folder_label(folder: &str) -> &str {
    let trimmed = folder.trim();
    if trimmed.is_empty() {
        "folder"
    } else {
        trimmed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_message_titles() {
        assert_eq!(
            move_toast_title(MoveFeedback::Archived, 1),
            "Message archived"
        );
        assert_eq!(
            move_toast_title(MoveFeedback::MarkedSpam, 1),
            "Message marked as spam"
        );
        assert_eq!(
            move_toast_title(MoveFeedback::NotSpam, 1),
            "Message moved to Inbox"
        );
        assert_eq!(
            move_toast_title(MoveFeedback::MovedTo("Receipts"), 1),
            "Message moved to Receipts"
        );
        assert_eq!(
            move_toast_title(MoveFeedback::CopiedTo("Receipts"), 1),
            "Message copied to Receipts"
        );
    }

    #[test]
    fn plural_titles_include_count() {
        assert_eq!(
            move_toast_title(MoveFeedback::Archived, 3),
            "3 messages archived"
        );
        assert_eq!(
            move_toast_title(MoveFeedback::MovedTo("Q&A <team>"), 2),
            "2 messages moved to Q&A <team>"
        );
    }

    #[test]
    fn blank_folder_name_falls_back() {
        assert_eq!(
            move_toast_title(MoveFeedback::MovedTo("  "), 1),
            "Message moved to folder"
        );
    }

    #[test]
    fn undo_titles() {
        assert_eq!(undo_move_toast_title(1), "Message moved back");
        assert_eq!(undo_move_toast_title(4), "4 messages moved back");
    }
}
