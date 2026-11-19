//! Composer status copy: draft save state, the undo-send countdown, and the
//! unsaved-account prompt. Pure so the wording stays tested.

use alcedo_i18n::{n_, ntr, tr, trf, trs};
/// Where the composer's current content is saved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DraftStatus {
    /// Nothing typed yet, or just sent / discarded.
    New,
    /// Crash-recovery copy written on this computer.
    SavedLocally,
    /// Saved to the account's Drafts folder.
    SavedToServer,
    /// The Drafts folder save failed; only the local copy exists.
    ServerSaveFailed,
}

/// Composer title subtitle for a draft status. Empty hides the subtitle.
pub fn draft_status_subtitle(status: DraftStatus) -> &'static str {
    match status {
        DraftStatus::New => "",
        DraftStatus::SavedLocally => trs!("Draft saved on this computer"),
        DraftStatus::SavedToServer => trs!("Draft saved to Drafts"),
        DraftStatus::ServerSaveFailed => trs!("Couldn't save to Drafts — kept on this computer"),
    }
}

/// Toast title while an outgoing message is held for undo.
pub fn delayed_send_toast_title(delay_secs: u32) -> String {
    ntr!(
        "Sending in {n} second",
        "Sending in {n} seconds",
        delay_secs
    )
}

/// Toast after the user takes a delayed send back.
pub fn undo_delayed_send_toast() -> &'static str {
    trs!("Sending canceled — message kept as a draft")
}

pub fn queued_send_recovery_text(count: usize) -> (String, String) {
    let heading = if count == 1 {
        tr!("Finish sending this message?")
    } else {
        ntr!(
            "Finish sending {n} message?",
            "Finish sending {n} messages?",
            count
        )
    };
    let body = tr!(
        "Alcedo closed before this send finished. It may or may not have been delivered, so check Sent before sending again."
    );
    (heading, body)
}

/// Heading for the account-settings discard prompt. `account` is the name
/// the user sees (display name, or the address when there is no name).
pub fn unsaved_account_heading(account: &str) -> String {
    let account = account.trim();
    let account = if account.is_empty() {
        trs!("this account")
    } else {
        account
    };
    trf!("Save Changes to {account}?", account = account)
}

/// Body for the account-settings discard prompt.
/// Marked for translation; show it with `alcedo_i18n::trm!`.
pub const UNSAVED_ACCOUNT_BODY: &str = n_!("Your changes will be lost if you don't save them.");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draft_status_subtitles() {
        assert_eq!(draft_status_subtitle(DraftStatus::New), "");
        assert_eq!(
            draft_status_subtitle(DraftStatus::SavedLocally),
            "Draft saved on this computer"
        );
        assert_eq!(
            draft_status_subtitle(DraftStatus::SavedToServer),
            "Draft saved to Drafts"
        );
        assert_eq!(
            draft_status_subtitle(DraftStatus::ServerSaveFailed),
            "Couldn't save to Drafts — kept on this computer"
        );
    }

    #[test]
    fn delayed_send_title_counts_seconds() {
        assert_eq!(delayed_send_toast_title(1), "Sending in 1 second");
        assert_eq!(delayed_send_toast_title(10), "Sending in 10 seconds");
    }

    #[test]
    fn unsaved_account_heading_uses_the_account_name() {
        assert_eq!(
            unsaved_account_heading("Ada Lovelace"),
            "Save Changes to Ada Lovelace?"
        );
        assert_eq!(
            unsaved_account_heading("  "),
            "Save Changes to this account?"
        );
    }

    #[test]
    fn queued_send_recovery_text_one_and_many() {
        let (h1, b1) = queued_send_recovery_text(1);
        assert_eq!(h1, "Finish sending this message?");
        let (h3, b3) = queued_send_recovery_text(3);
        assert_eq!(h3, "Finish sending 3 messages?");
        assert_eq!(b1, b3);
    }
}
