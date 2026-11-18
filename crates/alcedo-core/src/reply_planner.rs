//! Who a reply goes to and which address it is sent from.
//!
//! Pure rules, no UI: the mail view builds a [`ReplyPlanner`] for the message
//! being answered and turns the [`ReplyPlan`] into a compose draft.
//!
//! Recipient precedence:
//! - Reply: Mail-Reply-To, then Reply-To, then From. For mail the user sent,
//!   the original To (or Cc) minus the user's own addresses.
//! - Reply All: Mail-Followup-To when present; otherwise the Reply target plus
//!   the original To and Cc, minus the user's own addresses.
//! - Reply to List: the List-Post address.

use std::collections::HashSet;

use crate::envelope::{Address, Envelope};
use crate::identity::AccountIdentity;
use crate::ids::IdentityId;

/// Every address that belongs to the user on one account: the account email,
/// its saved identities, and any address on a catch-all domain.
#[derive(Clone, Copy, Debug)]
pub struct SelfAddresses<'a> {
    account_email: &'a str,
    identities: &'a [AccountIdentity],
    catch_all_domains: &'a [String],
}

impl<'a> SelfAddresses<'a> {
    pub fn new(
        account_email: &'a str,
        identities: &'a [AccountIdentity],
        catch_all_domains: &'a [String],
    ) -> Self {
        Self {
            account_email,
            identities,
            catch_all_domains,
        }
    }

    pub fn is_self(&self, email: &str) -> bool {
        let email = email.trim();
        !email.is_empty()
            && (self.is_account_email(email)
                || self.identity_for(email).is_some()
                || self.on_catch_all_domain(email))
    }

    pub fn identity_for(&self, email: &str) -> Option<&'a AccountIdentity> {
        let email = email.trim();
        self.identities
            .iter()
            .find(|identity| identity.email.eq_ignore_ascii_case(email))
    }

    fn is_account_email(&self, email: &str) -> bool {
        !self.account_email.is_empty() && self.account_email.eq_ignore_ascii_case(email)
    }

    fn on_catch_all_domain(&self, email: &str) -> bool {
        email.rsplit_once('@').is_some_and(|(local, domain)| {
            !local.is_empty()
                && self
                    .catch_all_domains
                    .iter()
                    .any(|catch_all| catch_all.eq_ignore_ascii_case(domain))
        })
    }
}

/// Catch-all domains from free text: split on commas and whitespace, drop a
/// leading `@`, lowercase, and keep each valid domain once.
pub fn parse_catch_all_domains(input: &str) -> Vec<String> {
    let mut domains: Vec<String> = Vec::new();
    for token in input.split(|c: char| c == ',' || c == ';' || c.is_whitespace()) {
        let domain = token.trim().trim_start_matches('@').to_ascii_lowercase();
        let valid = domain.contains('.')
            && !domain.starts_with('.')
            && !domain.ends_with('.')
            && domain
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-');
        if valid && !domains.contains(&domain) {
            domains.push(domain);
        }
    }
    domains
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplyMode {
    Sender,
    All,
    List,
}

/// The From address a reply should use.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReplyFrom {
    AccountDefault,
    Identity(IdentityId),
    /// An address that is the user's (catch-all domain) but not a saved identity.
    Alias(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplyPlan {
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub from: ReplyFrom,
}

pub struct ReplyPlanner<'a> {
    envelope: &'a Envelope,
    me: SelfAddresses<'a>,
    outbound_folder: bool,
}

impl<'a> ReplyPlanner<'a> {
    /// `outbound_folder` is true when the message sits in Sent (or another
    /// folder that only holds the user's own mail).
    pub fn new(envelope: &'a Envelope, me: SelfAddresses<'a>, outbound_folder: bool) -> Self {
        Self {
            envelope,
            me,
            outbound_folder,
        }
    }

    pub fn plan(&self, mode: ReplyMode) -> ReplyPlan {
        let (to, cc) = match mode {
            ReplyMode::Sender => (self.reply_to(), Vec::new()),
            ReplyMode::All => self.reply_all(),
            ReplyMode::List => match self.list_address() {
                Some(list) => (vec![list], Vec::new()),
                None => (self.reply_to(), Vec::new()),
            },
        };
        ReplyPlan {
            to,
            cc,
            from: self.from_choice(),
        }
    }

    /// True when `email` is the account address, a saved identity, or a catch-all alias.
    pub fn address_is_self(&self, email: &str) -> bool {
        self.me.is_self(email)
    }

    /// True when the user wrote this message.
    pub fn is_outbound(&self) -> bool {
        self.outbound_folder
            || self
                .envelope
                .from
                .iter()
                .any(|addr| self.me.is_self(&addr.email))
    }

    pub fn list_address(&self) -> Option<String> {
        self.envelope.routing_headers().list_post.clone()
    }

    /// Recipients for a plain Reply.
    pub fn reply_to(&self) -> Vec<String> {
        if self.is_outbound() {
            return self.original_recipients();
        }
        let routing = self.envelope.routing_headers();
        let target = [
            &routing.mail_reply_to,
            &self.envelope.reply_to,
            &self.envelope.from,
        ]
        .into_iter()
        .find(|addrs| !addrs.is_empty())
        .map(Vec::as_slice)
        .unwrap_or(&[]);
        unique_emails(target.iter())
    }

    /// To and Cc for Reply All.
    pub fn reply_all(&self) -> (Vec<String>, Vec<String>) {
        if !self.is_outbound() {
            let followup = self.others(self.envelope.routing_headers().mail_followup_to.iter());
            if !followup.is_empty() {
                return (followup, Vec::new());
            }
        }
        let to = self.reply_to();
        let cc = self.reply_all_cc(&to);
        (to, cc)
    }

    /// Reply All Cc for a given To: the original recipients minus the user's
    /// own addresses and anyone already in `to`.
    pub fn reply_all_cc(&self, to: &[String]) -> Vec<String> {
        let candidates: Vec<&Address> = if self.is_outbound() {
            self.envelope.cc.iter().collect()
        } else {
            self.envelope
                .to
                .iter()
                .chain(self.envelope.cc.iter())
                .collect()
        };
        let mut cc = self.others(candidates.into_iter());
        cc.retain(|email| {
            !to.iter()
                .any(|existing| existing.eq_ignore_ascii_case(email))
        });
        cc
    }

    /// From for the reply: a saved identity the message involved, else the
    /// first catch-all alias it reached (X-Original-To, Delivered-To, To, Cc;
    /// From for the user's own mail), else the account default.
    pub fn from_choice(&self) -> ReplyFrom {
        let candidates = self.sender_identity_candidates();
        if let Some(identity) = candidates
            .iter()
            .find_map(|email| self.me.identity_for(email))
        {
            return ReplyFrom::Identity(identity.id);
        }
        candidates
            .into_iter()
            .find(|email| self.me.is_self(email) && !self.me.is_account_email(email))
            .map(|alias| ReplyFrom::Alias(alias.trim().to_string()))
            .unwrap_or(ReplyFrom::AccountDefault)
    }

    fn sender_identity_candidates(&self) -> Vec<&'a str> {
        let envelope = self.envelope;
        if self.is_outbound() {
            return envelope
                .from
                .iter()
                .map(|addr| addr.email.as_str())
                .collect();
        }
        envelope
            .routing_headers()
            .delivered_to
            .iter()
            .map(String::as_str)
            .chain(envelope.to.iter().map(|addr| addr.email.as_str()))
            .chain(envelope.cc.iter().map(|addr| addr.email.as_str()))
            .collect()
    }

    /// The user's own message: answer its To, else its Cc, else themselves.
    fn original_recipients(&self) -> Vec<String> {
        for recipients in [&self.envelope.to, &self.envelope.cc] {
            let others = self.others(recipients.iter());
            if !others.is_empty() {
                return others;
            }
        }
        unique_emails(self.envelope.from.iter())
    }

    fn others<'b>(&self, addrs: impl Iterator<Item = &'b Address>) -> Vec<String> {
        unique_emails(addrs.filter(|addr| !self.me.is_self(&addr.email)))
    }
}

fn unique_emails<'b>(addrs: impl Iterator<Item = &'b Address>) -> Vec<String> {
    let mut seen = HashSet::new();
    addrs
        .map(|addr| addr.email.trim())
        .filter(|email| !email.is_empty() && seen.insert(email.to_ascii_lowercase()))
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{AccountId, FolderId, MessageId, ThreadId};
    use crate::routing::RoutingHeaders;
    use chrono::Utc;
    use uuid::Uuid;

    const ACCOUNT: &str = "carter@codeinfusion.com";

    fn addr(email: &str) -> Address {
        Address {
            name: None,
            email: email.to_string(),
        }
    }

    fn addrs(emails: &[&str]) -> Vec<Address> {
        emails.iter().map(|email| addr(email)).collect()
    }

    fn envelope(from: &str, to: &[&str], cc: &[&str]) -> Envelope {
        Envelope {
            id: MessageId(1),
            account_id: AccountId(Uuid::from_u128(1)),
            folder_id: FolderId(1),
            uid: 1,
            thread_id: ThreadId(Uuid::from_u128(2)),
            date: Utc::now(),
            subject: "Hello".to_string(),
            from: addrs(&[from]),
            to: addrs(to),
            cc: addrs(cc),
            reply_to: Vec::new(),
            sender: Vec::new(),
            routing: None,
            snippet: String::new(),
            seen: true,
            flagged: false,
            answered: false,
            draft: false,
            has_attachments: false,
            size_bytes: 0,
            deleted_at: None,
            account_accent: String::new(),
            account_avatar: String::new(),
        }
    }

    fn identity(email: &str) -> AccountIdentity {
        AccountIdentity {
            id: IdentityId(Uuid::from_u128(7)),
            account_id: AccountId(Uuid::from_u128(1)),
            display_name: "Carter".to_string(),
            email: email.to_string(),
        }
    }

    fn catch_alls() -> Vec<String> {
        vec![
            "codeinfusion.com".to_string(),
            "windwardlogic.com".to_string(),
            "alcedo.email".to_string(),
        ]
    }

    fn plan(
        env: &Envelope,
        identities: &[AccountIdentity],
        outbound: bool,
        mode: ReplyMode,
    ) -> ReplyPlan {
        let domains = catch_alls();
        let me = SelfAddresses::new(ACCOUNT, identities, &domains);
        ReplyPlanner::new(env, me, outbound).plan(mode)
    }

    #[test]
    fn self_addresses_cover_account_identity_and_catch_all() {
        let identities = [identity("me@personal.net")];
        let domains = catch_alls();
        let me = SelfAddresses::new(ACCOUNT, &identities, &domains);
        assert!(me.is_self("Carter@CodeInfusion.com"));
        assert!(me.is_self("me@personal.net"));
        assert!(me.is_self("vendor-x@windwardlogic.com"));
        assert!(!me.is_self("bob@example.com"));
        assert!(!me.is_self("@codeinfusion.com"));
        assert!(!me.is_self("evil@notcodeinfusion.com"));
        assert!(!me.is_self(""));
    }

    #[test]
    fn inbound_to_random_alias_replies_from_that_alias() {
        let env = envelope("vendor@example.com", &["vendor-x@codeinfusion.com"], &[]);
        let plan = plan(&env, &[], false, ReplyMode::Sender);
        assert_eq!(plan.to, vec!["vendor@example.com"]);
        assert_eq!(
            plan.from,
            ReplyFrom::Alias("vendor-x@codeinfusion.com".to_string())
        );
    }

    #[test]
    fn reply_all_with_alias_in_to_does_not_cc_self() {
        let env = envelope(
            "vendor@example.com",
            &["vendor-x@codeinfusion.com", "bob@example.com"],
            &["Carter@codeinfusion.com", "carol@example.com"],
        );
        let plan = plan(&env, &[], false, ReplyMode::All);
        assert_eq!(plan.to, vec!["vendor@example.com"]);
        assert_eq!(plan.cc, vec!["bob@example.com", "carol@example.com"]);
        assert_eq!(
            plan.from,
            ReplyFrom::Alias("vendor-x@codeinfusion.com".to_string())
        );
    }

    #[test]
    fn self_sent_from_alias_in_inbox_replies_to_recipients() {
        let env = envelope("shop@alcedo.email", &["bob@example.com"], &[]);
        let plan = plan(&env, &[], false, ReplyMode::Sender);
        assert_eq!(plan.to, vec!["bob@example.com"]);
        assert_eq!(plan.from, ReplyFrom::Alias("shop@alcedo.email".to_string()));
    }

    #[test]
    fn self_sent_from_alias_in_sent_keeps_alias() {
        let env = envelope(
            "shop@alcedo.email",
            &["bob@example.com"],
            &["cc@example.com"],
        );
        let plan = plan(&env, &[], true, ReplyMode::All);
        assert_eq!(plan.to, vec!["bob@example.com"]);
        assert_eq!(plan.cc, vec!["cc@example.com"]);
        assert_eq!(plan.from, ReplyFrom::Alias("shop@alcedo.email".to_string()));
    }

    #[test]
    fn outbound_drops_self_and_duplicates_from_to() {
        let env = envelope(
            ACCOUNT,
            &[
                "carter@codeinfusion.com",
                "bob@example.com",
                "BOB@example.com",
                "x@windwardlogic.com",
            ],
            &[],
        );
        let plan = plan(&env, &[], true, ReplyMode::Sender);
        assert_eq!(plan.to, vec!["bob@example.com"]);
        assert_eq!(plan.from, ReplyFrom::AccountDefault);
    }

    #[test]
    fn note_to_self_replies_to_self() {
        let env = envelope(ACCOUNT, &[ACCOUNT], &[]);
        let plan = plan(&env, &[], true, ReplyMode::Sender);
        assert_eq!(plan.to, vec![ACCOUNT]);
    }

    #[test]
    fn saved_identity_wins_over_alias_and_account() {
        let identities = [identity("sales@windwardlogic.com")];
        let mut env = envelope(
            "vendor@example.com",
            &["vendor-x@codeinfusion.com"],
            &["sales@windwardlogic.com"],
        );
        env.routing = Some(RoutingHeaders {
            delivered_to: vec![ACCOUNT.to_string()],
            ..RoutingHeaders::default()
        });
        let plan = plan(&env, &identities, false, ReplyMode::Sender);
        assert_eq!(plan.from, ReplyFrom::Identity(identities[0].id));
    }

    #[test]
    fn bcc_delivery_uses_x_original_to_alias() {
        let mut env = envelope("vendor@example.com", &["list@example.org"], &[]);
        env.routing = Some(RoutingHeaders {
            delivered_to: vec!["hidden@codeinfusion.com".to_string(), ACCOUNT.to_string()],
            ..RoutingHeaders::default()
        });
        let plan = plan(&env, &[], false, ReplyMode::Sender);
        assert_eq!(
            plan.from,
            ReplyFrom::Alias("hidden@codeinfusion.com".to_string())
        );
    }

    #[test]
    fn mail_to_account_address_uses_account_default() {
        let env = envelope("vendor@example.com", &[ACCOUNT], &[]);
        assert_eq!(
            plan(&env, &[], false, ReplyMode::Sender).from,
            ReplyFrom::AccountDefault
        );
    }

    #[test]
    fn without_catch_all_domains_alias_is_not_self() {
        let env = envelope(
            "vendor@example.com",
            &["vendor-x@codeinfusion.com"],
            &["bob@example.com"],
        );
        let me = SelfAddresses::new(ACCOUNT, &[], &[]);
        let plan = ReplyPlanner::new(&env, me, false).plan(ReplyMode::All);
        assert_eq!(
            plan.cc,
            vec!["vendor-x@codeinfusion.com", "bob@example.com"]
        );
        assert_eq!(plan.from, ReplyFrom::AccountDefault);
    }

    #[test]
    fn mail_reply_to_beats_reply_to_beats_from() {
        let mut env = envelope("poster@example.com", &[ACCOUNT], &[]);
        assert_eq!(
            plan(&env, &[], false, ReplyMode::Sender).to,
            vec!["poster@example.com"]
        );
        env.reply_to = addrs(&["replies@example.com"]);
        assert_eq!(
            plan(&env, &[], false, ReplyMode::Sender).to,
            vec!["replies@example.com"]
        );
        env.routing = Some(RoutingHeaders {
            mail_reply_to: addrs(&["private@example.com"]),
            ..RoutingHeaders::default()
        });
        assert_eq!(
            plan(&env, &[], false, ReplyMode::Sender).to,
            vec!["private@example.com"]
        );
    }

    #[test]
    fn reply_all_honours_mail_followup_to_minus_self() {
        let mut env = envelope(
            "poster@example.com",
            &["dev@lists.example.org"],
            &["bob@example.com"],
        );
        env.routing = Some(RoutingHeaders {
            mail_followup_to: addrs(&["dev@lists.example.org", ACCOUNT]),
            ..RoutingHeaders::default()
        });
        let plan = plan(&env, &[], false, ReplyMode::All);
        assert_eq!(plan.to, vec!["dev@lists.example.org"]);
        assert!(plan.cc.is_empty());
    }

    #[test]
    fn reply_list_uses_list_post_or_falls_back() {
        let mut env = envelope("poster@example.com", &["dev@lists.example.org"], &[]);
        assert_eq!(
            plan(&env, &[], false, ReplyMode::List).to,
            vec!["poster@example.com"]
        );
        env.routing = Some(RoutingHeaders {
            list_post: Some("dev@lists.example.org".to_string()),
            ..RoutingHeaders::default()
        });
        let plan = plan(&env, &[], false, ReplyMode::List);
        assert_eq!(plan.to, vec!["dev@lists.example.org"]);
        assert!(plan.cc.is_empty());
    }

    #[test]
    fn parse_catch_all_domains_normalizes_and_dedupes() {
        assert_eq!(
            parse_catch_all_domains(
                " @CodeInfusion.com, windwardlogic.com;alcedo.email codeinfusion.com bogus .bad. a b@c"
            ),
            vec!["codeinfusion.com", "windwardlogic.com", "alcedo.email"]
        );
        assert!(parse_catch_all_domains("").is_empty());
    }
}
