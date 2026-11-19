//! Reply, reply-all, list reply and forward drafts.

use super::*;
use alcedo_i18n::tr;

impl MailModel {
    pub(super) fn update_reply(&mut self, msg: MailMsg, sender: ComponentSender<MailModel>) {
        match msg {
            MailMsg::Reply => {
                if selection_count(&self.list_view) == 1
                    && let Some(id) = self.reply_message_id()
                {
                    self.reply_to_message(id, ReplyMode::Sender);
                }
            }
            MailMsg::ReplyAll => {
                if selection_count(&self.list_view) == 1
                    && let Some(id) = self.reply_message_id()
                {
                    self.reply_to_message(id, ReplyMode::All);
                }
            }
            MailMsg::ReplyTo(id) => {
                self.reply_to_message(id, ReplyMode::Sender);
            }
            MailMsg::ReplyList => {
                if selection_count(&self.list_view) == 1
                    && let Some(id) = self.reply_message_id()
                {
                    self.reply_to_message(id, ReplyMode::List);
                }
            }
            MailMsg::ReplyListTo(id) => {
                self.reply_to_message(id, ReplyMode::List);
            }
            MailMsg::ReplyAllTo(id) => {
                self.reply_to_message(id, ReplyMode::All);
            }
            MailMsg::ReplyInWindow => {
                if selection_count(&self.list_view) == 1
                    && let Some(id) = self.selected
                {
                    self.reply_to_message_in_window(id, ReplyMode::Sender, &sender);
                }
            }
            MailMsg::ReplyAllInWindow => {
                if selection_count(&self.list_view) == 1
                    && let Some(id) = self.selected
                {
                    self.reply_to_message_in_window(id, ReplyMode::All, &sender);
                }
            }
            MailMsg::ReplyToInWindow(id) => {
                self.reply_to_message_in_window(id, ReplyMode::Sender, &sender);
            }
            MailMsg::ReplyAllToInWindow(id) => {
                self.reply_to_message_in_window(id, ReplyMode::All, &sender);
            }
            MailMsg::Forward => {
                if selection_count(&self.list_view) == 1
                    && let Some(id) = self.selected
                {
                    self.forward_message(id);
                }
            }
            MailMsg::ForwardTo(id) => {
                self.forward_message(id);
            }
            MailMsg::ForwardInWindow => {
                if selection_count(&self.list_view) == 1
                    && let Some(id) = self.selected
                {
                    self.forward_message_in_window(id, &sender);
                }
            }
            MailMsg::ForwardToInWindow(id) => {
                self.forward_message_in_window(id, &sender);
            }
            _ => unreachable!("message not routed to reply"),
        }
    }

    pub(super) fn reply_to_message(&mut self, id: MessageId, mode: ReplyMode) {
        let Some(draft) = self.reply_draft_for(id, mode) else {
            return;
        };
        self.cmd.emit(UiCommand::Compose(draft.clone()));
        self.show_composer();
        self.bind_composer_from(&self.composer, draft.account_id);
        apply_compose_draft(&self.composer, &draft, true);
        present_main_window();
    }

    pub(super) fn reply_to_message_in_window(
        &mut self,
        id: MessageId,
        mode: ReplyMode,
        sender: &ComponentSender<MailModel>,
    ) {
        let Some(draft) = self.reply_draft_for(id, mode) else {
            return;
        };
        self.cmd.emit(UiCommand::Compose(draft.clone()));
        self.open_detached_composer(draft, &tr!("Reply"), sender, DetachedComposerFocus::Body);
    }

    pub(super) fn forward_message(&mut self, id: MessageId) {
        let Some(draft) = self.forward_draft_for(id) else {
            return;
        };
        self.cmd.emit(UiCommand::Compose(draft.clone()));
        self.show_composer();
        self.bind_composer_from(&self.composer, draft.account_id);
        apply_compose_draft(&self.composer, &draft, false);
        present_main_window();
        focus_composer_to(&self.composer);
    }

    pub(super) fn forward_message_in_window(
        &mut self,
        id: MessageId,
        sender: &ComponentSender<MailModel>,
    ) {
        let Some(draft) = self.forward_draft_for(id) else {
            return;
        };
        self.cmd.emit(UiCommand::Compose(draft.clone()));
        self.open_detached_composer(draft, &tr!("Forward"), sender, DetachedComposerFocus::To);
    }

    pub(super) fn compose_source_for(&self, id: MessageId) -> Option<(&Envelope, &Account, &str)> {
        let envelope = self.envelope_for(id)?;
        let account = self
            .accounts
            .iter()
            .find(|account| account.id == envelope.account_id)
            .or_else(|| self.compose_account())?;
        let body_text = self.bodies.get(&id).map(String::as_str).unwrap_or("");
        Some((envelope, account, body_text))
    }

    pub(super) fn reply_draft_for(&self, id: MessageId, mode: ReplyMode) -> Option<ComposeDraft> {
        let (envelope, account, body_text) = self.compose_source_for(id)?;
        let html = self.html_bodies.get(&id).map(String::as_str);
        let (format, quote_body) = compose_quote_source(html, body_text);
        let (in_reply_to, references) = self.threading_for_reply(id);
        let from_email = envelope
            .from
            .first()
            .map(|a| a.email.as_str())
            .unwrap_or("");
        let empty_unsub = ListUnsubscribe {
            https: None,
            mailto: None,
            one_click: false,
        };
        let list_unsub = match self.unsubscribe_by_id.get(&id) {
            Some(list) => Some(list),
            None if self.bodies.contains_key(&id) => Some(&empty_unsub),
            None => None,
        };
        let contacts = self.contact_book.borrow().contacts.clone();
        let approve = crate::extension::registered()
            .iter()
            .any(|extension| extension.approve_contact_on_reply(from_email, list_unsub, &contacts));
        let approve_contact = if approve {
            Some(from_email.to_string())
        } else {
            None
        };
        let plan = self.reply_planner_for(envelope).plan(mode);
        let mut draft = build_reply_draft(
            envelope,
            &quote_body,
            account.id,
            plan.to,
            plan.cc,
            in_reply_to,
            references,
            approve_contact,
            format,
            self.signatures.get(&account.id),
        );
        apply_reply_from(&mut draft, &plan.from, account);
        Some(draft)
    }

    pub(super) fn threading_for_reply(&self, id: MessageId) -> (Option<String>, Option<String>) {
        let Some((message_id_hdr, references_hdr)) = self.threading_hdrs.get(&id) else {
            return (None, None);
        };
        let parent_id = message_id_hdr.as_deref();
        (
            alcedo_core::reply_in_reply_to(parent_id),
            alcedo_core::reply_references(references_hdr.as_deref(), parent_id),
        )
    }

    pub(super) fn self_emails_for(&self, account_id: AccountId) -> Vec<String> {
        let mut emails = Vec::new();
        if let Some(account) = self
            .accounts
            .iter()
            .find(|account| account.id == account_id)
        {
            emails.push(account.email.clone());
        }
        for identity in self
            .identities
            .get(&account_id)
            .map(Vec::as_slice)
            .unwrap_or(&[])
        {
            emails.push(identity.email.clone());
        }
        emails
    }

    /// Reply rules for `envelope`, with the account's own addresses.
    pub(super) fn reply_planner_for<'a>(&'a self, envelope: &'a Envelope) -> ReplyPlanner<'a> {
        let account_email = self
            .accounts
            .iter()
            .find(|account| account.id == envelope.account_id)
            .map(|account| account.email.as_str())
            .unwrap_or("");
        let identities = self
            .identities
            .get(&envelope.account_id)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let catch_all_domains = self
            .catch_all_domains
            .get(&envelope.account_id)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        ReplyPlanner::new(
            envelope,
            SelfAddresses::new(account_email, identities, catch_all_domains),
            self.folder_is_sent(envelope.folder_id),
        )
    }

    pub(super) fn forward_draft_for(&self, id: MessageId) -> Option<ComposeDraft> {
        let (envelope, account, body_text) = self.compose_source_for(id)?;
        let html = self.html_bodies.get(&id).map(String::as_str);
        let (format, quote_body) = compose_quote_source(html, body_text);
        let mut draft = build_forward_draft(
            envelope,
            &quote_body,
            account.id,
            format,
            self.signatures.get(&account.id),
        );
        let from = self.reply_planner_for(envelope).from_choice();
        apply_reply_from(&mut draft, &from, account);
        Some(draft)
    }
}

/// Point a reply, forward or reaction at the From the planner chose.
pub(super) fn apply_reply_from(draft: &mut ComposeDraft, from: &ReplyFrom, account: &Account) {
    match from {
        ReplyFrom::AccountDefault => {}
        ReplyFrom::Identity(identity_id) => draft.identity_id = Some(*identity_id),
        ReplyFrom::Alias(email) => {
            draft.identity_id = None;
            draft.custom_from = Some(alcedo_core::format_mailbox(
                Some(account.display_name.as_str()),
                email,
            ));
        }
    }
}

pub(super) fn compose_quote_source(html: Option<&str>, text: &str) -> (ComposeFormat, String) {
    let html = html.map(str::trim).filter(|value| !value.is_empty());
    let text_is_html = looks_like_html(text);
    let html_is_generated = html.is_some_and(looks_like_generated_plaintext_html);

    if text_is_html && (html.is_none() || html_is_generated) {
        return (ComposeFormat::Html, sanitize_compose_html(text));
    }
    if let Some(html) = html
        && !html_is_generated
    {
        return (ComposeFormat::Html, sanitize_compose_html(html));
    }
    (ComposeFormat::Plain, text.to_string())
}

pub(super) fn html_quote_fragment(body_text: &str) -> String {
    if looks_like_html(body_text) {
        sanitize_compose_html(body_text)
    } else {
        body_text
            .lines()
            .map(html_escape_for_reply)
            .collect::<Vec<_>>()
            .join("<br>")
    }
}

/// Inline block style for new HTML compose lines. 1.5 is the HTML-mail body
/// default (Gmail’s reading pane, and the usual 1.4–1.6 template range).
/// CSS `line-height: 1` is the em box with no leading, tighter than Outlook
/// “Single”, and it lets descenders collide with the next line.
pub(super) const COMPOSE_BLOCK_STYLE: &str = "margin:0;line-height:1.5";

pub(super) fn build_reply_draft(
    envelope: &Envelope,
    body_text: &str,
    account_id: AccountId,
    to: Vec<String>,
    cc: Vec<String>,
    in_reply_to: Option<String>,
    references: Option<String>,
    approve_contact_email: Option<String>,
    format: ComposeFormat,
    signature: Option<&AccountSignature>,
) -> ComposeDraft {
    use chrono::Local;

    let subject = alcedo_core::reply_subject(&envelope.display_subject());
    let date_str = envelope
        .date
        .with_timezone(&Local)
        .format("%b %d, %Y, %I:%M %p")
        .to_string();
    let from = envelope
        .from
        .first()
        .map(|addr| addr.formatted())
        .unwrap_or_else(|| "Unknown".to_string());
    let intro = format!("On {date_str}, {from} wrote:");
    let body = if format == ComposeFormat::Html {
        let quoted_html = html_quote_fragment(body_text);
        let intro = html_escape_for_reply(&intro);
        let blank = format!("<p style=\"{COMPOSE_BLOCK_STYLE}\"><br></p>");
        format!(
            "{blank}{blank}{blank}<p>────────────────────────</p><p style=\"{COMPOSE_BLOCK_STYLE}\">{intro}</p><blockquote>{quoted_html}</blockquote>\n"
        )
    } else {
        let quoted = body_text
            .lines()
            .map(|line| format!("> {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        format!("\n---\n\n{intro}\n\n{quoted}\n")
    };
    let body_text = apply_signature(&body, format, signature, true);
    let inlines = signature_inlines_for_body(signature, &body_text);
    ComposeDraft {
        account_id,
        to,
        cc,
        bcc: Vec::new(),
        subject,
        body_text,
        format,
        request_mdn: request_mdn(),
        tracking_pixel_url: effective_tracking_pixel_url(),
        attachments: Vec::new(),
        inlines,
        collect_outgoing_contacts: crate::settings::collect_outgoing_contacts(),
        in_reply_to,
        references,
        thread_id: Some(envelope.thread_id),
        approve_contact_email,
        sign: false,
        encrypt: false,
        autocrypt_header: None,
        identity_id: None,
        custom_from: None,
        imap_draft: None,
        reaction_emoji: None,
    }
}

pub(super) fn fwd_subject(decoded: &str) -> String {
    let trimmed = decoded.trim_start();
    let has_fwd = trimmed
        .chars()
        .take(4)
        .collect::<String>()
        .eq_ignore_ascii_case("fwd:");
    let has_fw = trimmed
        .chars()
        .take(3)
        .collect::<String>()
        .eq_ignore_ascii_case("fw:");
    if has_fwd || has_fw {
        decoded.to_string()
    } else {
        format!("Fwd: {decoded}")
    }
}

pub(super) fn format_address_list(addrs: &[Address]) -> String {
    addrs
        .iter()
        .map(|addr| addr.formatted())
        .collect::<Vec<_>>()
        .join(", ")
}

pub(super) fn build_forward_draft(
    envelope: &Envelope,
    body_text: &str,
    account_id: AccountId,
    format: ComposeFormat,
    signature: Option<&AccountSignature>,
) -> ComposeDraft {
    use chrono::Local;

    let decoded_subject = envelope.display_subject();
    let subject = fwd_subject(&decoded_subject);
    let date_str = envelope
        .date
        .with_timezone(&Local)
        .format("%b %d, %Y, %I:%M %p")
        .to_string();
    let from = envelope
        .from
        .first()
        .map(|addr| addr.formatted())
        .unwrap_or_else(|| "Unknown".to_string());
    let to_list = format_address_list(&envelope.to);
    let cc_list = format_address_list(&envelope.cc);
    let body = if format == ComposeFormat::Html {
        let mut header_lines = vec![
            "<p>---------- Forwarded message ---------</p>".to_string(),
            format!(
                "<p style=\"{COMPOSE_BLOCK_STYLE}\">From: {}</p>",
                html_escape_for_reply(&from)
            ),
            format!("<p style=\"{COMPOSE_BLOCK_STYLE}\">Date: {date_str}</p>"),
            format!(
                "<p style=\"{COMPOSE_BLOCK_STYLE}\">Subject: {}</p>",
                html_escape_for_reply(&decoded_subject)
            ),
            format!(
                "<p style=\"{COMPOSE_BLOCK_STYLE}\">To: {}</p>",
                html_escape_for_reply(&to_list)
            ),
        ];
        if !envelope.cc.is_empty() {
            header_lines.push(format!(
                "<p style=\"{COMPOSE_BLOCK_STYLE}\">Cc: {}</p>",
                html_escape_for_reply(&cc_list)
            ));
        }
        let header_html = header_lines.join("");
        let body_html = html_quote_fragment(body_text);
        format!("<p style=\"{COMPOSE_BLOCK_STYLE}\"><br></p>{header_html}{body_html}\n")
    } else {
        let mut header = format!(
            "\n\n---------- Forwarded message ---------\nFrom: {from}\nDate: {date_str}\nSubject: {decoded_subject}\nTo: {to_list}"
        );
        if !envelope.cc.is_empty() {
            header.push_str(&format!("\nCc: {cc_list}"));
        }
        format!("{header}\n\n{body_text}\n")
    };
    let body_text = apply_signature(&body, format, signature, true);
    let inlines = signature_inlines_for_body(signature, &body_text);
    ComposeDraft {
        account_id,
        to: Vec::new(),
        cc: Vec::new(),
        bcc: Vec::new(),
        subject,
        body_text,
        format,
        request_mdn: request_mdn(),
        tracking_pixel_url: effective_tracking_pixel_url(),
        attachments: Vec::new(),
        inlines,
        collect_outgoing_contacts: crate::settings::collect_outgoing_contacts(),
        in_reply_to: None,
        references: None,
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

pub(super) fn html_escape_for_reply(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mail::test_support::{envelope, envelope_with_reply_to};

    // Thin views over the core ReplyPlanner, kept so the reply tests read the
    // same as before the rules moved to alcedo-core.
    fn planner<'a>(
        env: &'a Envelope,
        account_email: &'a str,
        outbound_folder: bool,
        identities: &'a [AccountIdentity],
    ) -> ReplyPlanner<'a> {
        ReplyPlanner::new(
            env,
            SelfAddresses::new(account_email, identities, &[]),
            outbound_folder,
        )
    }

    fn from_is_self(env: &Envelope, account_email: &str, identities: &[AccountIdentity]) -> bool {
        planner(env, account_email, false, identities).is_outbound()
    }

    fn reply_to_addresses(
        env: &Envelope,
        account_email: &str,
        outbound_folder: bool,
        identities: &[AccountIdentity],
    ) -> Vec<String> {
        planner(env, account_email, outbound_folder, identities).reply_to()
    }

    fn reply_all_cc_addresses(
        env: &Envelope,
        account_email: &str,
        outbound_folder: bool,
        to: &[String],
        identities: &[AccountIdentity],
    ) -> Vec<String> {
        planner(env, account_email, outbound_folder, identities).reply_all_cc(to)
    }

    fn compose_identity_for_message(
        env: &Envelope,
        account_email: &str,
        outbound_folder: bool,
        identities: &[AccountIdentity],
    ) -> Option<IdentityId> {
        match planner(env, account_email, outbound_folder, identities).from_choice() {
            ReplyFrom::Identity(id) => Some(id),
            _ => None,
        }
    }

    fn identity(id: u128, email: &str) -> AccountIdentity {
        AccountIdentity {
            id: IdentityId(Uuid::from_u128(id)),
            account_id: AccountId(Uuid::from_u128(1)),
            display_name: String::new(),
            email: email.to_string(),
        }
    }

    #[test]
    fn from_is_self_identity_matches_when_account_email_empty() {
        let identities = [identity(10, "alias@example.com")];
        let env = envelope("alias@example.com", &["bob@example.com"], &[]);
        assert!(from_is_self(&env, "", &identities));
        assert!(!from_is_self(&env, "", &[]));
        assert_eq!(
            reply_to_addresses(&env, "", false, &identities),
            vec!["bob@example.com".to_string()]
        );
        assert_eq!(
            compose_identity_for_message(&env, "", false, &identities),
            Some(IdentityId(Uuid::from_u128(10)))
        );
    }

    #[test]
    fn reply_to_inbound_uses_from() {
        let env = envelope("alice@example.com", &["me@example.com"], &[]);
        assert_eq!(
            reply_to_addresses(&env, "me@example.com", false, &[]),
            vec!["alice@example.com".to_string()]
        );
    }

    #[test]
    fn reply_to_inbound_uses_reply_to_header() {
        let env = envelope_with_reply_to(
            "noreply@codeinfusion.com",
            &["test@codeinfusion.com"],
            &["admin@windwardlogic.com"],
            &[],
        );
        let to = reply_to_addresses(&env, "admin@windwardlogic.com", false, &[]);
        assert_eq!(to, vec!["test@codeinfusion.com".to_string()]);
        assert_eq!(
            reply_all_cc_addresses(&env, "admin@windwardlogic.com", false, &to, &[]),
            Vec::<String>::new()
        );
        assert!(!to.contains(&"noreply@codeinfusion.com".to_string()));
    }

    #[test]
    fn reply_to_reply_all_excludes_from_and_self() {
        let env = envelope_with_reply_to(
            "noreply@example.com",
            &["human@example.com"],
            &["me@example.com", "bob@example.com"],
            &["carol@example.com"],
        );
        let to = reply_to_addresses(&env, "me@example.com", false, &[]);
        assert_eq!(to, vec!["human@example.com".to_string()]);
        assert_eq!(
            reply_all_cc_addresses(&env, "me@example.com", false, &to, &[]),
            vec![
                "bob@example.com".to_string(),
                "carol@example.com".to_string()
            ]
        );
    }

    #[test]
    fn reply_to_sent_ignores_reply_to_header() {
        let env = envelope_with_reply_to(
            "me@example.com",
            &["other@example.com"],
            &["bob@example.com"],
            &[],
        );
        assert_eq!(
            reply_to_addresses(&env, "me@example.com", true, &[]),
            vec!["bob@example.com".to_string()]
        );
    }

    #[test]
    fn reply_to_inbound_empty_reply_to_uses_from() {
        let env = envelope("alice@example.com", &["me@example.com"], &[]);
        assert_eq!(
            reply_to_addresses(&env, "me@example.com", false, &[]),
            vec!["alice@example.com".to_string()]
        );
    }

    #[test]
    fn reply_to_inbound_reply_to_same_emails_as_from() {
        let env = envelope_with_reply_to(
            "noreply@example.com",
            &["noreply@example.com"],
            &["me@example.com"],
            &[],
        );
        assert_eq!(
            reply_to_addresses(&env, "me@example.com", false, &[]),
            vec!["noreply@example.com".to_string()]
        );
    }

    #[test]
    fn reply_to_multiple_reply_to_addresses_preserve_order() {
        let env = envelope_with_reply_to(
            "noreply@example.com",
            &["first@example.com", "second@example.com"],
            &["me@example.com"],
            &[],
        );
        assert_eq!(
            reply_to_addresses(&env, "me@example.com", false, &[]),
            vec![
                "first@example.com".to_string(),
                "second@example.com".to_string()
            ]
        );
    }

    #[test]
    fn reply_to_from_is_self_ignores_reply_to_header() {
        let env = envelope_with_reply_to(
            "me@example.com",
            &["other@example.com"],
            &["bob@example.com"],
            &[],
        );
        assert_eq!(
            reply_to_addresses(&env, "me@example.com", false, &[]),
            vec!["bob@example.com".to_string()]
        );
    }

    #[test]
    fn reply_to_sent_uses_original_recipients() {
        let env = envelope("me@example.com", &["bob@example.com"], &["cc@example.com"]);
        assert_eq!(
            reply_to_addresses(&env, "me@example.com", true, &[]),
            vec!["bob@example.com".to_string()]
        );
    }

    #[test]
    fn reply_to_self_sent_falls_back_to_to_without_sent_folder() {
        let env = envelope("me@example.com", &["bob@example.com"], &[]);
        assert_eq!(
            reply_to_addresses(&env, "me@example.com", false, &[]),
            vec!["bob@example.com".to_string()]
        );
    }

    #[test]
    fn reply_all_inbound_puts_other_recipients_in_cc() {
        let env = envelope(
            "alice@example.com",
            &["me@example.com", "bob@example.com"],
            &["cc@example.com"],
        );
        let to = reply_to_addresses(&env, "me@example.com", false, &[]);
        assert_eq!(to, vec!["alice@example.com".to_string()]);
        assert_eq!(
            reply_all_cc_addresses(&env, "me@example.com", false, &to, &[]),
            vec!["bob@example.com".to_string(), "cc@example.com".to_string()]
        );
    }

    #[test]
    fn reply_all_cc_skips_own_identity() {
        let identities = [identity(10, "alias@example.com")];
        let env = envelope(
            "alice@example.com",
            &["alias@example.com", "bob@example.com"],
            &["cc@example.com"],
        );
        let to = reply_to_addresses(&env, "me@example.com", false, &identities);
        assert_eq!(to, vec!["alice@example.com".to_string()]);
        assert_eq!(
            reply_all_cc_addresses(&env, "me@example.com", false, &to, &identities),
            vec!["bob@example.com".to_string(), "cc@example.com".to_string()]
        );
    }

    #[test]
    fn reply_all_sent_keeps_original_cc_minus_self() {
        let env = envelope("me@example.com", &["bob@example.com"], &["cc@example.com"]);
        let to = reply_to_addresses(&env, "me@example.com", true, &[]);
        assert_eq!(to, vec!["bob@example.com".to_string()]);
        assert_eq!(
            reply_all_cc_addresses(&env, "me@example.com", true, &to, &[]),
            vec!["cc@example.com".to_string()]
        );
    }

    #[test]
    fn reply_all_sent_drops_cc_already_in_to() {
        let env = envelope(
            "me@example.com",
            &["bob@example.com"],
            &["bob@example.com", "cc@example.com"],
        );
        let to = reply_to_addresses(&env, "me@example.com", true, &[]);
        assert_eq!(to, vec!["bob@example.com".to_string()]);
        assert_eq!(
            reply_all_cc_addresses(&env, "me@example.com", true, &to, &[]),
            vec!["cc@example.com".to_string()]
        );
    }

    #[test]
    fn reply_all_sent_empty_to_does_not_duplicate_cc() {
        let env = envelope("me@example.com", &[], &["cc@example.com"]);
        let to = reply_to_addresses(&env, "me@example.com", true, &[]);
        assert_eq!(to, vec!["cc@example.com".to_string()]);
        assert_eq!(
            reply_all_cc_addresses(&env, "me@example.com", true, &to, &[]),
            Vec::<String>::new()
        );
    }

    #[test]
    fn fwd_subject_prefixes_when_missing() {
        assert_eq!(fwd_subject("Hello"), "Fwd: Hello");
    }

    #[test]
    fn fwd_subject_keeps_fwd_prefix() {
        assert_eq!(fwd_subject("Fwd: Hello"), "Fwd: Hello");
        assert_eq!(fwd_subject("  FWD: Hello"), "  FWD: Hello");
    }

    #[test]
    fn fwd_subject_keeps_fw_prefix() {
        assert_eq!(fwd_subject("fw: Hello"), "fw: Hello");
        assert_eq!(fwd_subject("  FW: Hello"), "  FW: Hello");
    }

    #[test]
    fn fwd_subject_keeps_re_prefix() {
        assert_eq!(fwd_subject("Re: Hello"), "Fwd: Re: Hello");
    }

    #[test]
    fn fwd_subject_prefixes_multibyte_without_panic() {
        assert_eq!(fwd_subject("你好"), "Fwd: 你好");
        assert_eq!(fwd_subject("🎉 Party"), "Fwd: 🎉 Party");
    }

    #[test]
    fn build_forward_draft_empty_recipients() {
        let env = envelope(
            "alice@example.com",
            &["bob@example.com"],
            &["cc@example.com"],
        );
        let draft = build_forward_draft(&env, "Body", env.account_id, ComposeFormat::Plain, None);
        assert!(draft.to.is_empty());
        assert!(draft.cc.is_empty());
        assert!(draft.bcc.is_empty());
        assert!(draft.attachments.is_empty());
        assert!(draft.inlines.is_empty());
    }

    #[test]
    fn build_forward_draft_subject_prefixes_hello() {
        let env = envelope("alice@example.com", &["bob@example.com"], &[]);
        let draft = build_forward_draft(&env, "", env.account_id, ComposeFormat::Plain, None);
        assert_eq!(draft.subject, "Fwd: Hello");
    }

    #[test]
    fn build_forward_draft_plain_body_contains_forward_block() {
        let env = envelope("alice@example.com", &["bob@example.com"], &[]);
        let draft = build_forward_draft(
            &env,
            "Original text",
            env.account_id,
            ComposeFormat::Plain,
            None,
        );
        assert!(
            draft
                .body_text
                .contains("---------- Forwarded message ---------")
        );
        assert!(draft.body_text.contains("From: alice@example.com"));
        assert!(draft.body_text.contains("Date:"));
        assert!(draft.body_text.contains("Subject: Hello"));
        assert!(draft.body_text.contains("To: bob@example.com"));
        assert!(draft.body_text.contains("Original text"));
    }

    #[test]
    fn build_forward_draft_plain_body_no_quote_prefixes() {
        let env = envelope("alice@example.com", &["bob@example.com"], &[]);
        let draft = build_forward_draft(
            &env,
            "Line one\nLine two",
            env.account_id,
            ComposeFormat::Plain,
            None,
        );
        assert!(!draft.body_text.contains("> Line one"));
        assert!(!draft.body_text.contains("> Line two"));
        assert!(draft.body_text.contains("Line one\nLine two"));
    }

    #[test]
    fn build_forward_draft_html_body_prefix_and_escapes() {
        let env = envelope("alice@example.com", &["bob@example.com"], &[]);
        let draft = build_forward_draft(
            &env,
            "see <not-a-tag> & amp",
            env.account_id,
            ComposeFormat::Html,
            None,
        );
        assert!(
            draft
                .body_text
                .starts_with(&format!("<p style=\"{COMPOSE_BLOCK_STYLE}\"><br></p>"))
        );
        assert!(
            draft
                .body_text
                .contains("<p>---------- Forwarded message ---------</p>")
        );
        assert!(!draft.body_text.contains(&format!(
            "<p style=\"{COMPOSE_BLOCK_STYLE}\">---------- Forwarded message ---------</p>"
        )));
        assert!(draft.body_text.contains("&lt;not-a-tag&gt;"));
        assert!(draft.body_text.contains("&amp; amp"));
    }

    #[test]
    fn build_forward_draft_html_preserves_sanitized_markup() {
        let env = envelope("alice@example.com", &["bob@example.com"], &[]);
        let draft = build_forward_draft(
            &env,
            "<p>Hello <b>world</b></p><script>alert(1)</script>",
            env.account_id,
            ComposeFormat::Html,
            None,
        );
        assert!(draft.body_text.contains("Hello"));
        assert!(draft.body_text.contains("world"));
        assert!(!draft.body_text.contains("<script>"));
        assert_eq!(draft.format, ComposeFormat::Html);
    }

    #[test]
    fn build_forward_draft_cc_line_only_when_present() {
        let env = envelope(
            "alice@example.com",
            &["bob@example.com"],
            &["cc@example.com"],
        );
        let draft = build_forward_draft(&env, "Body", env.account_id, ComposeFormat::Plain, None);
        assert!(draft.body_text.contains("Cc: cc@example.com"));
    }

    #[test]
    fn build_forward_draft_cc_line_omitted_when_empty() {
        let env = envelope("alice@example.com", &["bob@example.com"], &[]);
        let draft = build_forward_draft(&env, "Body", env.account_id, ComposeFormat::Plain, None);
        assert!(!draft.body_text.contains("Cc:"));
    }

    #[test]
    fn compose_quote_source_prefers_real_html() {
        let (format, body) = compose_quote_source(Some("<p>Hi</p>"), "Hi");
        assert_eq!(format, ComposeFormat::Html);
        assert!(body.contains("Hi"));
        assert!(!body.contains("<script>"));
    }

    #[test]
    fn compose_quote_source_keeps_html_with_escaped_addresses() {
        let html = "<html><body><p>Thanks</p><p>On Jan 1, Alice &lt;alice@example.com&gt; wrote:</p><blockquote>Hi</blockquote></body></html>";
        let text = "Thanks\n\nOn Jan 1, Alice <alice@example.com> wrote:\n\nHi";
        let (format, body) = compose_quote_source(Some(html), text);
        assert_eq!(format, ComposeFormat::Html);
        assert!(body.contains("blockquote") || body.contains("Thanks"));
        assert!(!body.starts_with("Thanks\n"));
    }

    #[test]
    fn compose_quote_source_plain_when_no_html() {
        let (format, body) = compose_quote_source(None, "Hello\nWorld");
        assert_eq!(format, ComposeFormat::Plain);
        assert_eq!(body, "Hello\nWorld");
    }

    #[test]
    fn build_reply_draft_matches_html_source_format() {
        let env = envelope("alice@example.com", &["me@example.com"], &[]);
        let draft = build_reply_draft(
            &env,
            "<p>Original</p>",
            env.account_id,
            vec!["alice@example.com".to_string()],
            Vec::new(),
            None,
            None,
            None,
            ComposeFormat::Html,
            None,
        );
        assert_eq!(draft.format, ComposeFormat::Html);
        assert!(draft.body_text.contains("<blockquote>"));
        assert!(draft.body_text.contains("Original"));
        assert!(draft.body_text.contains(&format!(
            "<p style=\"{COMPOSE_BLOCK_STYLE}\"><br></p><p style=\"{COMPOSE_BLOCK_STYLE}\"><br></p><p style=\"{COMPOSE_BLOCK_STYLE}\"><br></p><p>────────────────────────</p>"
        )));
        assert!(draft.body_text.contains("<p>────────────────────────</p>"));
        assert!(!draft.body_text.contains("&lt;p&gt;"));
    }

    #[test]
    fn build_reply_draft_plain_quotes_with_gt() {
        let env = envelope("alice@example.com", &["me@example.com"], &[]);
        let draft = build_reply_draft(
            &env,
            "Line one\nLine two",
            env.account_id,
            vec!["alice@example.com".to_string()],
            Vec::new(),
            None,
            None,
            None,
            ComposeFormat::Plain,
            None,
        );
        assert_eq!(draft.format, ComposeFormat::Plain);
        assert!(draft.body_text.contains("> Line one"));
        assert!(draft.body_text.contains("> Line two"));
    }
}
