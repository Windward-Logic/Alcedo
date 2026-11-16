//! Undo for archive / move / spam: a single-level undo offered by toast.
//!
//! Message ids survive IMAP moves (the worker rehomes the store row), so undo
//! is just another move back to each message's source folder.

use alcedo_core::mail_feedback::{MoveFeedback, move_toast_title, undo_move_toast_title};

use super::*;

/// How long the undo toast stays up (seconds), matching undo-delete.
const UNDO_MOVE_TOAST_SECS: u32 = 8;

/// The last move the user can still take back.
pub(super) struct PendingUndoMove {
    toast: adw::Toast,
    /// Each moved message and the folder it came from.
    sources: Vec<(MessageId, FolderId)>,
    /// Rows removed from the list, restored optimistically on undo.
    envelopes: Vec<Envelope>,
}

impl MailModel {
    pub(super) fn update_undo_move(&mut self, msg: MailMsg, _sender: ComponentSender<MailModel>) {
        match msg {
            MailMsg::UndoMove => self.undo_last_move(),
            _ => unreachable!("message not routed to undo_move"),
        }
    }

    /// Source folder and list row for each message about to move. Call before
    /// the rows leave the UI.
    pub(super) fn capture_move_sources(
        &self,
        ids: &[MessageId],
    ) -> (Vec<(MessageId, FolderId)>, Vec<Envelope>) {
        let lookup = self.message_lookup();
        let sources = ids
            .iter()
            .filter_map(|id| lookup.location(*id).map(|(_, folder)| (*id, folder)))
            .collect();
        let envelopes = ids
            .iter()
            .filter_map(|id| lookup.envelope(*id).cloned())
            .collect();
        (sources, envelopes)
    }

    /// Confirm a completed move and offer to undo it.
    pub(super) fn offer_undo_move(
        &mut self,
        feedback: MoveFeedback<'_>,
        sources: Vec<(MessageId, FolderId)>,
        envelopes: Vec<Envelope>,
    ) {
        self.dismiss_undo_move_toast();
        if sources.is_empty() {
            return;
        }
        let title = move_toast_title(feedback, sources.len());
        let toast = crate::toast::plain_with_timeout(&title, UNDO_MOVE_TOAST_SECS);
        toast.set_button_label(Some("Undo"));
        let sender = self.mail_sender.clone();
        toast.connect_button_clicked(move |_| {
            sender.send(MailMsg::UndoMove).ok();
        });
        self.toasts.add_toast(toast.clone());
        self.record(activity_log::KIND_MAIL, ActivityOutcome::Success, title, "");
        self.undo_move = Some(PendingUndoMove {
            toast,
            sources,
            envelopes,
        });
    }

    pub(super) fn dismiss_undo_move_toast(&mut self) {
        if let Some(pending) = self.undo_move.take() {
            pending.toast.dismiss();
        }
    }

    fn undo_last_move(&mut self) {
        let Some(pending) = self.undo_move.take() else {
            return;
        };
        pending.toast.dismiss();
        let count = pending.sources.len();
        for (dest, ids) in group_ids_by_folder(&pending.sources) {
            if self.mock {
                for id in &ids {
                    self.mock_move_message_to_folder(*id, dest);
                }
            } else {
                self.cmd.emit(UiCommand::MoveMessagesToFolder { ids, dest });
            }
        }
        if self.mock {
            self.apply_mock_list();
        } else {
            self.restore_undeleted_envelopes(pending.envelopes);
        }
        self.toasts
            .add_toast(crate::toast::plain(&undo_move_toast_title(count)));
    }
}

/// Group `(message, source folder)` pairs by folder, keeping first-seen order
/// so each folder gets one batched move.
pub(super) fn group_ids_by_folder(
    sources: &[(MessageId, FolderId)],
) -> Vec<(FolderId, Vec<MessageId>)> {
    let mut groups: Vec<(FolderId, Vec<MessageId>)> = Vec::new();
    for (id, folder) in sources {
        match groups.iter_mut().find(|(existing, _)| existing == folder) {
            Some((_, ids)) => ids.push(*id),
            None => groups.push((*folder, vec![*id])),
        }
    }
    groups
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_by_source_folder_in_first_seen_order() {
        let sources = [
            (MessageId(1), FolderId(10)),
            (MessageId(2), FolderId(20)),
            (MessageId(3), FolderId(10)),
        ];
        assert_eq!(
            group_ids_by_folder(&sources),
            vec![
                (FolderId(10), vec![MessageId(1), MessageId(3)]),
                (FolderId(20), vec![MessageId(2)]),
            ]
        );
    }

    #[test]
    fn empty_sources_have_no_groups() {
        assert!(group_ids_by_folder(&[]).is_empty());
    }
}
