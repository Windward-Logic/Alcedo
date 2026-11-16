//! Id lookups for one bulk operation on a selection.
//!
//! `MailModel::envelope_for` and `member_location` scan the whole list, so
//! calling them once per selected message is quadratic. Folder lists are not
//! paged, so select-all in a large folder would stall the window. Build one
//! of these per operation and look each id up in constant time instead.

use std::collections::HashMap;

use alcedo_core::{AccountId, Conversation, Envelope, FolderId, MessageId};

/// First envelope for each id, in list order (what a `find` would return).
pub(super) fn index_envelopes<'a>(
    envelopes: impl IntoIterator<Item = &'a Envelope>,
) -> HashMap<MessageId, &'a Envelope> {
    let mut index = HashMap::new();
    for env in envelopes {
        index.entry(env.id).or_insert(env);
    }
    index
}

/// Snapshot of where every listed message lives.
pub(super) struct MessageLookup<'a> {
    envelopes: HashMap<MessageId, &'a Envelope>,
    members: HashMap<MessageId, (AccountId, FolderId)>,
}

impl<'a> MessageLookup<'a> {
    /// `listed` rows win over `source` rows, matching `envelope_for`.
    pub(super) fn build(
        listed: &'a [Envelope],
        source: &'a [Envelope],
        conversations: &[Conversation],
    ) -> Self {
        let envelopes = index_envelopes(listed.iter().chain(source));
        let mut members = HashMap::new();
        for conversation in conversations {
            for (id, location) in conversation
                .member_ids
                .iter()
                .zip(&conversation.member_locations)
            {
                members.entry(*id).or_insert(*location);
            }
        }
        Self { envelopes, members }
    }

    pub(super) fn envelope(&self, id: MessageId) -> Option<&'a Envelope> {
        self.envelopes.get(&id).copied()
    }

    /// Account and folder of a message: from its row, or from the
    /// conversation it is a collapsed member of.
    pub(super) fn location(&self, id: MessageId) -> Option<(AccountId, FolderId)> {
        self.envelope(id)
            .map(|env| (env.account_id, env.folder_id))
            .or_else(|| self.members.get(&id).copied())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mail::test_support::envelope_with_id;
    use alcedo_core::group_envelopes;
    use uuid::Uuid;

    fn in_folder(id: i64, folder: i64) -> Envelope {
        let mut env = envelope_with_id(id);
        env.folder_id = FolderId(folder);
        env
    }

    #[test]
    fn listed_row_wins_over_source_row_with_the_same_id() {
        let listed = [in_folder(1, 10)];
        let source = [in_folder(1, 20), in_folder(2, 20)];
        let lookup = MessageLookup::build(&listed, &source, &[]);
        assert_eq!(
            lookup.envelope(MessageId(1)).unwrap().folder_id,
            FolderId(10)
        );
        assert_eq!(
            lookup.envelope(MessageId(2)).unwrap().folder_id,
            FolderId(20)
        );
        assert!(lookup.envelope(MessageId(3)).is_none());
    }

    #[test]
    fn first_duplicate_in_a_list_wins_like_find() {
        let listed = [in_folder(1, 10), in_folder(1, 11)];
        let index = index_envelopes(&listed);
        assert_eq!(index[&MessageId(1)].folder_id, FolderId(10));
    }

    #[test]
    fn location_falls_back_to_conversation_membership() {
        let mut first = in_folder(1, 10);
        let mut second = in_folder(2, 11);
        let thread = alcedo_core::ThreadId(Uuid::from_u128(7));
        first.thread_id = thread;
        second.thread_id = thread;
        let conversations = group_envelopes(&[first.clone(), second.clone()]);
        // Only the conversation knows about message 2.
        let listed = [first];
        let lookup = MessageLookup::build(&listed, &[], &conversations);
        assert_eq!(
            lookup.location(MessageId(2)),
            Some((second.account_id, FolderId(11)))
        );
        assert_eq!(
            lookup.location(MessageId(1)),
            Some((listed[0].account_id, FolderId(10)))
        );
        assert_eq!(lookup.location(MessageId(9)), None);
    }
}
