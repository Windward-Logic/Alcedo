//! Emoji reactions: chips, button eligibility and sending.

use super::reply::apply_reply_from;
use super::*;

/// The stored message a reaction row points at.
#[derive(Clone)]
pub(super) struct ReactionOriginal {
    target_id: MessageId,
    subject: String,
    text: String,
    html: String,
}

/// Reaction messages. `MessageId(0)` targets the in-pane reader's selection.
#[derive(Clone)]
pub enum ReactionMsg {
    /// Open the emoji picker for a message.
    Open(MessageId),
    /// Send `emoji` as a reaction to a message.
    Send { id: MessageId, emoji: String },
}

impl MailModel {
    pub(super) fn remember_reaction_original(
        &mut self,
        reaction_id: MessageId,
        target_id: MessageId,
        subject: String,
        text: String,
        html: String,
    ) {
        self.reaction_originals.insert(
            reaction_id,
            ReactionOriginal {
                target_id,
                subject,
                text,
                html,
            },
        );
        self.update_readers_for_message(reaction_id);
    }

    /// Copy a freshly loaded body into any reaction that is showing that message.
    pub(super) fn note_reaction_original_body(
        &mut self,
        target_id: MessageId,
        text: &str,
        html: &str,
    ) {
        let reaction_ids: Vec<MessageId> = self
            .reaction_originals
            .iter()
            .filter(|(_, original)| original.target_id == target_id)
            .map(|(id, _)| *id)
            .collect();
        for reaction_id in reaction_ids {
            if let Some(original) = self.reaction_originals.get_mut(&reaction_id) {
                original.text = text.to_string();
                original.html = html.to_string();
            }
            self.update_readers_for_message(reaction_id);
        }
    }

    pub(super) fn with_reaction_original(
        &self,
        id: MessageId,
        text: Option<String>,
        html: Option<String>,
    ) -> (Option<String>, Option<String>) {
        let Some(original) = self.reaction_originals.get(&id) else {
            return (text, html);
        };
        let original_html = if original.html.trim().is_empty() {
            None
        } else {
            Some(self.html_with_inline_images(original.target_id, original.html.clone()))
        };
        let (html, text) = reaction_reader_body(
            html.as_deref(),
            text.as_deref().unwrap_or(""),
            &original.subject,
            original_html.as_deref(),
            &original.text,
        );
        (Some(text), Some(html))
    }

    pub(super) fn update_reactions(&mut self, msg: ReactionMsg) {
        match msg {
            ReactionMsg::Open(id) => {
                let id = if id.0 == 0 { self.selected } else { Some(id) };
                if let Some(id) = id {
                    self.open_reaction_popover(id);
                }
            }
            ReactionMsg::Send { id, emoji } => {
                let id = if id.0 == 0 { self.selected } else { Some(id) };
                if let Some(id) = id {
                    self.send_reaction(id, emoji);
                }
            }
        }
    }

    /// The account's own addresses for the message a reaction targets.
    fn self_emails_for_target(&self, target_id: MessageId) -> Vec<String> {
        self.envelopes
            .iter()
            .find(|env| env.id == target_id)
            .map(|env| self.self_emails_for(env.account_id))
            .unwrap_or_default()
    }

    pub(super) fn grouped_reactions_for(&self, target_id: MessageId) -> Vec<GroupedReaction> {
        let self_emails = self.self_emails_for_target(target_id);
        let mut chips = group_reactions(
            self.reactions_by_target
                .get(&target_id)
                .map(Vec::as_slice)
                .unwrap_or(&[]),
            &self_emails,
        );
        if let Some(optimistic) = self.optimistic_reactions.get(&target_id) {
            for chip in optimistic {
                if !chips.iter().any(|existing| existing.emoji == chip.emoji) {
                    chips.push(chip.clone());
                }
            }
        }
        chips
    }

    pub(super) fn own_reaction_emojis(&self, target_id: MessageId) -> Vec<String> {
        let self_emails = self.self_emails_for_target(target_id);
        let mut emojis = Vec::new();
        let mut seen = HashSet::new();
        if let Some(people) = self.reactions_by_target.get(&target_id) {
            for person in people {
                if !self_emails
                    .iter()
                    .any(|email| email.eq_ignore_ascii_case(&person.from_email))
                {
                    continue;
                }
                let key = person
                    .message_id_hdr
                    .clone()
                    .map(|hdr| format!("hdr:{hdr}"))
                    .unwrap_or_else(|| format!("row:{}", person.row_id));
                if !seen.insert(key) {
                    continue;
                }
                if !emojis.contains(&person.emoji) {
                    emojis.push(person.emoji.clone());
                }
            }
        }
        if let Some(optimistic) = self.optimistic_reactions.get(&target_id) {
            for chip in optimistic {
                if !emojis.contains(&chip.emoji) {
                    emojis.push(chip.emoji.clone());
                }
            }
        }
        emojis
    }

    pub(super) fn refresh_reaction_chips_for(&self, target_id: MessageId) {
        let chips = self.grouped_reactions_for(target_id);
        if self.selected == Some(target_id) {
            show_reaction_chips(&self.reader, &chips);
        }
        if let Some(window) = self.message_windows.get(&target_id) {
            show_reaction_chips(&window.reader, &chips);
        }
    }

    pub(super) fn refresh_react_enabled_for(&self, id: MessageId) {
        let Some(envelope) = self.envelopes.iter().find(|env| env.id == id).cloned() else {
            return;
        };
        if self.selected == Some(id) {
            self.update_reaction_chrome(&self.reader, id, &envelope);
        }
        if let Some(window) = self.message_windows.get(&id) {
            self.update_reaction_chrome(&window.reader, id, &envelope);
        }
    }

    pub(super) fn update_reaction_chrome(
        &self,
        widgets: &ReaderWidgets,
        id: MessageId,
        envelope: &Envelope,
    ) {
        let chips = self.grouped_reactions_for(id);
        show_reaction_chips(widgets, &chips);
        let (enabled, tooltip) = match self.reaction_block_for(id, envelope) {
            Some(reason) => (false, reason),
            None => (true, "Add reaction"),
        };
        set_react_enabled(widgets, enabled, tooltip);
    }

    /// Why a reaction to `id` can't be sent, shared by the button state and
    /// the send path so they never disagree.
    pub(super) fn reaction_block_for(
        &self,
        id: MessageId,
        envelope: &Envelope,
    ) -> Option<&'static str> {
        let has_message_id = self
            .threading_hdrs
            .get(&id)
            .and_then(|(hdr, _)| hdr.as_ref())
            .is_some_and(|hdr| !hdr.trim().is_empty());
        let list_has_action = self
            .unsubscribe_by_id
            .get(&id)
            .is_some_and(|list| list.has_action());
        let to_cc_count = envelope
            .to
            .iter()
            .chain(envelope.cc.iter())
            .map(|addr| addr.email.to_ascii_lowercase())
            .collect::<HashSet<_>>()
            .len();
        let plan = self.reply_planner_for(envelope).plan(ReplyMode::All);
        reaction_block_reason(
            envelope.draft,
            self.reaction_message_ids.contains(&id),
            has_message_id,
            list_has_action,
            to_cc_count,
            plan.to.len() + plan.cc.len(),
            self.own_reaction_emojis(id).len(),
        )
        .or({
            if !self.mock && !self.pgp_by_message.contains_key(&id) {
                Some("Wait until the message finishes loading")
            } else {
                None
            }
        })
        .or_else(|| encrypted_reaction_block(self.is_encrypted(id)))
    }

    fn is_encrypted(&self, id: MessageId) -> bool {
        self.pgp_by_message
            .get(&id)
            .is_some_and(|status| status.encrypted)
    }

    pub(super) fn open_reaction_popover(&self, id: MessageId) {
        let widgets = if self.selected == Some(id) {
            &self.reader
        } else if let Some(window) = self.message_windows.get(&id) {
            &window.reader
        } else {
            return;
        };
        prepare_reaction_popover(widgets, &self.own_reaction_emojis(id));
        popup_reaction_popover(widgets);
    }

    pub(super) fn send_reaction(&mut self, id: MessageId, emoji: String) {
        if self.send_in_flight.get() {
            self.toasts.add_toast(crate::toast::plain(
                "Wait until the current message finishes sending.",
            ));
            return;
        }
        if !is_single_reaction_emoji(&emoji) {
            self.toasts.add_toast(crate::toast::plain(
                "That emoji can't be sent as a reaction.",
            ));
            return;
        }
        let Some(envelope) = self.envelopes.iter().find(|env| env.id == id).cloned() else {
            return;
        };
        if let Some(reason) = self.reaction_block_for(id, &envelope) {
            self.toasts.add_toast(crate::toast::plain(reason));
            return;
        }
        if self
            .own_reaction_emojis(id)
            .iter()
            .any(|sent| sent == &emoji)
        {
            return;
        }
        let Some(account) = self
            .accounts
            .iter()
            .find(|account| account.id == envelope.account_id)
        else {
            return;
        };
        let plan = self.reply_planner_for(&envelope).plan(ReplyMode::All);
        let (in_reply_to, references) = self.threading_for_reply(id);
        let mut draft = build_reaction_draft(
            &envelope,
            envelope.account_id,
            &account.email,
            &account.sender_name,
            &account.display_name,
            plan.to,
            plan.cc,
            emoji.clone(),
            in_reply_to,
            references,
        );
        apply_reply_from(&mut draft, &plan.from, account);
        self.optimistic_reactions
            .entry(id)
            .or_default()
            .push(GroupedReaction {
                emoji: emoji.clone(),
                count: 1,
                names: vec!["You".to_string()],
            });
        self.refresh_reaction_chips_for(id);
        self.pending_reaction_target = Some(id);
        self.pending_send_draft = Some(draft.clone());
        self.send_in_flight.set(true);
        self.cmd.emit(UiCommand::Send(draft));
    }
}

pub(super) fn build_reaction_draft(
    envelope: &Envelope,
    account_id: AccountId,
    account_email: &str,
    sender_name: &str,
    account_display_name: &str,
    to: Vec<String>,
    cc: Vec<String>,
    emoji: String,
    in_reply_to: Option<String>,
    references: Option<String>,
) -> ComposeDraft {
    let decoded_subject = envelope.display_subject();
    let subject = if decoded_subject.is_empty() {
        "Reaction".to_string()
    } else {
        decoded_subject
    };
    let reaction_name = reaction_from_display_name(sender_name, account_display_name);
    let (plain, _, _) = reaction_bodies(&reaction_name, account_email, &emoji);
    ComposeDraft {
        account_id,
        to,
        cc,
        bcc: Vec::new(),
        subject,
        body_text: plain,
        format: ComposeFormat::Plain,
        request_mdn: false,
        tracking_pixel_url: None,
        attachments: Vec::new(),
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
        reaction_emoji: Some(emoji),
    }
}

#[cfg(test)]
mod tests {
    use alcedo_core::{MessageId, reaction_block_reason};

    #[test]
    fn reaction_block_for_blocks_when_reaction_message_ids_contains_row() {
        let reaction_id = MessageId(99);
        let mut reaction_message_ids = std::collections::HashSet::new();
        let eligible = |ids: &std::collections::HashSet<MessageId>| {
            reaction_block_reason(false, ids.contains(&reaction_id), true, false, 1, 1, 0)
        };
        assert!(eligible(&reaction_message_ids).is_none());
        // Same as `ReactionOriginalReady` inserting into `reaction_message_ids`.
        reaction_message_ids.insert(reaction_id);
        assert_eq!(
            eligible(&reaction_message_ids),
            Some("You can't react to a reaction")
        );
    }
}
