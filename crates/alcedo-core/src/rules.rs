//! Message rules: matching, planning, and validation.

use alcedo_i18n::{tr, trs};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::account::{Folder, SpecialUse};
use crate::envelope::{Address, Envelope};
use crate::ids::{AccountId, FolderId, MessageId, RuleId};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RuleMatchMode {
    All,
    Any,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleField {
    From,
    To,
    Cc,
    Recipient,
    Subject,
    HasAttachment,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleOp {
    Contains,
    NotContains,
    Is,
    IsNot,
    StartsWith,
    EndsWith,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RuleExecution {
    Client,
    Server,
}

/// Whether the account's server runs Alcedo's rules.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServerRulesStatus {
    #[default]
    Unknown,
    Unavailable,
    Ready,
    ForeignActive {
        name: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerRuleView {
    pub name: String,
    pub match_mode: RuleMatchMode,
    pub stop_processing: bool,
    pub conditions: Vec<RuleCondition>,
    pub actions: Vec<ServerRuleAction>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServerRuleAction {
    MoveToFolder {
        imap_name: String,
        folder_id: Option<FolderId>,
    },
    CopyToFolder {
        imap_name: String,
        folder_id: Option<FolderId>,
    },
    MarkRead,
    MarkUnread,
    Star,
    Unstar,
    MoveToTrash,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerRulesPreview {
    pub script_name: String,
    pub fingerprint: String,
    pub rules: Vec<ServerRuleView>,
    pub unsupported_count: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleCondition {
    pub field: RuleField,
    pub op: RuleOp,
    pub value: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RuleAction {
    MoveToFolder(FolderId),
    CopyToFolder(FolderId),
    MarkRead,
    MarkUnread,
    Star,
    Unstar,
    MoveToTrash,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageRule {
    pub id: RuleId,
    pub account_id: AccountId,
    pub name: String,
    pub enabled: bool,
    pub position: i32,
    pub match_mode: RuleMatchMode,
    pub stop_processing: bool,
    pub conditions: Vec<RuleCondition>,
    pub actions: Vec<RuleAction>,
    pub execution: RuleExecution,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RulePlan {
    pub message_id: MessageId,
    pub moves: Option<FolderId>,
    pub copies: Vec<FolderId>,
    pub seen: Option<bool>,
    pub flagged: Option<bool>,
    pub trash: bool,
}

pub fn default_stop_processing(actions: &[RuleAction]) -> bool {
    actions.iter().any(|action| {
        matches!(
            action,
            RuleAction::MoveToFolder(_) | RuleAction::MoveToTrash
        )
    })
}

pub fn validate_rule(rule: &MessageRule) -> Result<(), &'static str> {
    if rule.name.trim().is_empty() {
        return Err(trs!("Enter a name."));
    }
    if rule.conditions.is_empty() {
        return Err(trs!("Add a condition."));
    }
    if rule.actions.is_empty() {
        return Err(trs!("Add an action."));
    }
    for condition in &rule.conditions {
        if condition.field == RuleField::HasAttachment {
            continue;
        }
        if condition.value.trim().is_empty() {
            return Err(trs!("Add a condition."));
        }
    }
    for action in &rule.actions {
        match action {
            RuleAction::MoveToFolder(id) | RuleAction::CopyToFolder(id) => {
                if id.0 == 0 {
                    return Err(trs!("Choose a folder."));
                }
            }
            _ => {}
        }
    }
    Ok(())
}

pub fn rule_matches(rule: &MessageRule, env: &Envelope) -> bool {
    if !rule.enabled {
        return false;
    }
    let results: Vec<bool> = rule
        .conditions
        .iter()
        .map(|c| condition_matches(c, env))
        .collect();
    match rule.match_mode {
        RuleMatchMode::All => results.iter().all(|&v| v),
        RuleMatchMode::Any => results.iter().any(|&v| v),
    }
}

fn condition_matches(condition: &RuleCondition, env: &Envelope) -> bool {
    if condition.field == RuleField::HasAttachment {
        return env.has_attachments;
    }
    let value = condition.value.trim();
    if value.is_empty() {
        return false;
    }
    let needle = value.to_lowercase();
    match condition.field {
        RuleField::From => address_list_matches(&env.from, condition.op, &needle),
        RuleField::To => address_list_matches(&env.to, condition.op, &needle),
        RuleField::Cc => address_list_matches(&env.cc, condition.op, &needle),
        RuleField::Recipient => {
            let mut addrs = env.to.clone();
            addrs.extend(env.cc.clone());
            address_list_matches(&addrs, condition.op, &needle)
        }
        RuleField::Subject => text_matches(&env.display_subject(), condition.op, &needle),
        RuleField::HasAttachment => env.has_attachments,
    }
}

fn address_list_matches(addrs: &[Address], op: RuleOp, needle: &str) -> bool {
    let matches_one = |addr: &Address| {
        let email = addr.email.to_lowercase();
        let name = addr.display_name().to_lowercase();
        text_matches(&email, op, needle) || text_matches(&name, op, needle)
    };
    match op {
        RuleOp::NotContains => !addrs.iter().any(|addr| {
            let email = addr.email.to_lowercase();
            let name = addr.display_name().to_lowercase();
            email.contains(needle) || name.contains(needle)
        }),
        RuleOp::IsNot => !addrs.iter().any(|addr| {
            let email = addr.email.to_lowercase();
            let name = addr.display_name().to_lowercase();
            email == needle || name == needle
        }),
        _ => addrs.iter().any(matches_one),
    }
}

fn text_matches(haystack: &str, op: RuleOp, needle: &str) -> bool {
    let hay = haystack.to_lowercase();
    match op {
        RuleOp::Contains => hay.contains(needle),
        RuleOp::NotContains => !hay.contains(needle),
        RuleOp::Is => hay == needle,
        RuleOp::IsNot => hay != needle,
        RuleOp::StartsWith => hay.starts_with(needle),
        RuleOp::EndsWith => hay.ends_with(needle),
    }
}

pub fn plan_rules(
    rules: &[MessageRule],
    envelopes: &[Envelope],
    execution_filter: Option<RuleExecution>,
) -> Vec<RulePlan> {
    let mut sorted: Vec<&MessageRule> = rules.iter().filter(|r| r.enabled).collect();
    sorted.sort_by_key(|r| r.position);

    let mut plans = Vec::new();
    for env in envelopes {
        let mut plan = RulePlan {
            message_id: env.id,
            moves: None,
            copies: Vec::new(),
            seen: None,
            flagged: None,
            trash: false,
        };
        let mut stopped = false;
        for rule in &sorted {
            if stopped {
                break;
            }
            if let Some(filter) = execution_filter
                && rule.execution != filter
            {
                continue;
            }
            if !rule_matches(rule, env) {
                continue;
            }
            apply_actions(&mut plan, &rule.actions);
            if rule.stop_processing {
                stopped = true;
            }
        }
        if plan.moves.is_some()
            || !plan.copies.is_empty()
            || plan.seen.is_some()
            || plan.flagged.is_some()
            || plan.trash
        {
            plans.push(plan);
        }
    }
    plans
}

fn apply_actions(plan: &mut RulePlan, actions: &[RuleAction]) {
    for action in actions {
        match action {
            RuleAction::MoveToFolder(folder) => {
                plan.moves = Some(*folder);
                plan.trash = false;
            }
            RuleAction::CopyToFolder(folder) => {
                if !plan.copies.contains(folder) {
                    plan.copies.push(*folder);
                }
            }
            RuleAction::MarkRead => plan.seen = Some(true),
            RuleAction::MarkUnread => plan.seen = Some(false),
            RuleAction::Star => plan.flagged = Some(true),
            RuleAction::Unstar => plan.flagged = Some(false),
            RuleAction::MoveToTrash => {
                plan.trash = true;
                plan.moves = None;
            }
        }
    }
}

fn field_label(field: RuleField) -> &'static str {
    match field {
        RuleField::From => trs!("From"),
        RuleField::To => trs!("To"),
        RuleField::Cc => trs!("Cc"),
        RuleField::Recipient => trs!("Recipient"),
        RuleField::Subject => trs!("Subject"),
        RuleField::HasAttachment => trs!("Has attachment"),
    }
}

fn op_label(op: RuleOp) -> &'static str {
    match op {
        RuleOp::Contains => trs!("contains"),
        RuleOp::NotContains => trs!("does not contain"),
        RuleOp::Is => trs!("is"),
        RuleOp::IsNot => trs!("is not"),
        RuleOp::StartsWith => trs!("starts with"),
        RuleOp::EndsWith => trs!("ends with"),
    }
}

fn action_label(action: &RuleAction, folders: &[Folder]) -> String {
    match action {
        RuleAction::MoveToFolder(id) | RuleAction::CopyToFolder(id) => {
            folder_display_name(*id, folders)
        }
        RuleAction::MarkRead => tr!("Mark as read"),
        RuleAction::MarkUnread => tr!("Mark as unread"),
        RuleAction::Star => tr!("Star"),
        RuleAction::Unstar => tr!("Unstar"),
        RuleAction::MoveToTrash => tr!("Trash"),
    }
}

fn folder_display_name(id: FolderId, folders: &[Folder]) -> String {
    folders
        .iter()
        .find(|f| f.id == id)
        .map(|f| f.imap_name.clone())
        .unwrap_or_else(|| "Missing folder".to_string())
}

fn condition_summary(condition: &RuleCondition) -> String {
    if condition.field == RuleField::HasAttachment {
        return field_label(condition.field).to_string();
    }
    format!(
        "{} {} {}",
        field_label(condition.field),
        op_label(condition.op),
        condition.value.trim()
    )
}

fn server_action_label(action: &ServerRuleAction, folders: &[Folder]) -> String {
    match action {
        ServerRuleAction::MoveToFolder { folder_id, .. }
        | ServerRuleAction::CopyToFolder { folder_id, .. } => match folder_id {
            Some(id) => folder_display_name(*id, folders),
            None => tr!("Missing folder"),
        },
        ServerRuleAction::MarkRead => tr!("Mark as read"),
        ServerRuleAction::MarkUnread => tr!("Mark as unread"),
        ServerRuleAction::Star => tr!("Star"),
        ServerRuleAction::Unstar => tr!("Unstar"),
        ServerRuleAction::MoveToTrash => tr!("Trash"),
    }
}

pub fn rule_summary(rule: &MessageRule, folders: &[Folder]) -> String {
    let cond = rule
        .conditions
        .first()
        .map(condition_summary)
        .unwrap_or_default();
    let action = rule
        .actions
        .first()
        .map(|a| action_label(a, folders))
        .unwrap_or_default();
    format!("{cond} → {action}")
}

pub fn server_rule_summary(rule: &ServerRuleView, folders: &[Folder]) -> String {
    let cond = rule
        .conditions
        .first()
        .map(condition_summary)
        .unwrap_or_default();
    let action = rule
        .actions
        .first()
        .map(|a| server_action_label(a, folders))
        .unwrap_or_default();
    format!("{cond} → {action}")
}

pub fn rule_has_server_capable_conditions(rule: &MessageRule) -> bool {
    !rule
        .conditions
        .iter()
        .any(|c| c.field == RuleField::HasAttachment)
}

/// Whether `rule` can run on the mail server with the account's `folders`: no
/// condition the server cannot test, and every folder an action names exists
/// (Trash for "Move to Trash"). The default for
/// `alcedo_sync::server_rules::ServerRulesBackend::can_publish`.
pub fn rule_can_run_on_server(rule: &MessageRule, folders: &[Folder]) -> bool {
    rule_has_server_capable_conditions(rule)
        && rule.actions.iter().all(|action| match action {
            RuleAction::MoveToFolder(id) | RuleAction::CopyToFolder(id) => {
                folders.iter().any(|folder| folder.id == *id)
            }
            RuleAction::MoveToTrash => folders
                .iter()
                .any(|folder| folder.special == SpecialUse::Trash),
            RuleAction::MarkRead
            | RuleAction::MarkUnread
            | RuleAction::Star
            | RuleAction::Unstar => true,
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use uuid::Uuid;

    fn sample_account() -> AccountId {
        AccountId(Uuid::new_v4())
    }

    fn sample_envelope(from_email: &str, subject: &str) -> Envelope {
        Envelope {
            id: MessageId(1),
            account_id: sample_account(),
            folder_id: FolderId(1),
            uid: 1,
            thread_id: crate::ids::ThreadId(Uuid::new_v4()),
            date: Utc::now(),
            subject: subject.to_string(),
            from: vec![Address {
                name: Some("Ada".to_string()),
                email: from_email.to_string(),
            }],
            to: vec![],
            cc: vec![],
            reply_to: Vec::new(),
            sender: Vec::new(),
            routing: None,
            snippet: String::new(),
            seen: false,
            flagged: false,
            answered: false,
            draft: false,
            has_attachments: false,
            deleted_at: None,
            account_accent: String::new(),
            account_avatar: String::new(),
            size_bytes: 0,
        }
    }

    fn sample_rule(
        conditions: Vec<RuleCondition>,
        actions: Vec<RuleAction>,
        match_mode: RuleMatchMode,
    ) -> MessageRule {
        let now = Utc::now();
        MessageRule {
            id: RuleId(Uuid::new_v4()),
            account_id: sample_account(),
            name: "Test".to_string(),
            enabled: true,
            position: 0,
            match_mode,
            stop_processing: default_stop_processing(&actions),
            conditions,
            actions,
            execution: RuleExecution::Client,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn from_is_matches_email() {
        let rule = sample_rule(
            vec![RuleCondition {
                field: RuleField::From,
                op: RuleOp::Is,
                value: "ada@example.com".to_string(),
            }],
            vec![RuleAction::Star],
            RuleMatchMode::All,
        );
        let env = sample_envelope("ada@example.com", "hi");
        assert!(rule_matches(&rule, &env));
    }

    #[test]
    fn from_contains_case_insensitive() {
        let rule = sample_rule(
            vec![RuleCondition {
                field: RuleField::From,
                op: RuleOp::Contains,
                value: "EXAMPLE".to_string(),
            }],
            vec![RuleAction::Star],
            RuleMatchMode::All,
        );
        let env = sample_envelope("ada@example.com", "hi");
        assert!(rule_matches(&rule, &env));
    }

    #[test]
    fn from_not_contains() {
        let rule = sample_rule(
            vec![RuleCondition {
                field: RuleField::From,
                op: RuleOp::NotContains,
                value: "bob".to_string(),
            }],
            vec![RuleAction::Star],
            RuleMatchMode::All,
        );
        let env = sample_envelope("ada@example.com", "hi");
        assert!(rule_matches(&rule, &env));
    }

    #[test]
    fn recipient_hits_cc() {
        let mut env = sample_envelope("ada@example.com", "hi");
        env.cc = vec![Address {
            name: None,
            email: "bob@example.com".to_string(),
        }];
        let rule = sample_rule(
            vec![RuleCondition {
                field: RuleField::Recipient,
                op: RuleOp::Is,
                value: "bob@example.com".to_string(),
            }],
            vec![RuleAction::Star],
            RuleMatchMode::All,
        );
        assert!(rule_matches(&rule, &env));
    }

    #[test]
    fn has_attachment_true_false() {
        let rule = sample_rule(
            vec![RuleCondition {
                field: RuleField::HasAttachment,
                op: RuleOp::Is,
                value: String::new(),
            }],
            vec![RuleAction::Star],
            RuleMatchMode::All,
        );
        let mut env = sample_envelope("a@b.com", "hi");
        env.has_attachments = false;
        assert!(!rule_matches(&rule, &env));
        env.has_attachments = true;
        assert!(rule_matches(&rule, &env));
    }

    #[test]
    fn all_vs_any() {
        let rule = sample_rule(
            vec![
                RuleCondition {
                    field: RuleField::From,
                    op: RuleOp::Is,
                    value: "ada@example.com".to_string(),
                },
                RuleCondition {
                    field: RuleField::Subject,
                    op: RuleOp::Contains,
                    value: "invoice".to_string(),
                },
            ],
            vec![RuleAction::Star],
            RuleMatchMode::All,
        );
        let env = sample_envelope("ada@example.com", "hello");
        assert!(!rule_matches(&rule, &env));

        let rule_any = MessageRule {
            match_mode: RuleMatchMode::Any,
            ..rule
        };
        assert!(rule_matches(&rule_any, &env));
    }

    #[test]
    fn empty_value_no_match() {
        let rule = sample_rule(
            vec![RuleCondition {
                field: RuleField::Subject,
                op: RuleOp::Contains,
                value: "  ".to_string(),
            }],
            vec![RuleAction::Star],
            RuleMatchMode::All,
        );
        let env = sample_envelope("a@b.com", "invoice");
        assert!(!rule_matches(&rule, &env));
    }

    #[test]
    fn stop_after_move() {
        let rules = vec![
            MessageRule {
                position: 0,
                actions: vec![RuleAction::MoveToFolder(FolderId(2))],
                stop_processing: true,
                ..sample_rule(vec![], vec![], RuleMatchMode::All)
            },
            MessageRule {
                position: 1,
                actions: vec![RuleAction::Star],
                stop_processing: false,
                ..sample_rule(vec![], vec![], RuleMatchMode::All)
            },
        ];
        let env = sample_envelope("a@b.com", "hi");
        let plans = plan_rules(&rules, &[env], None);
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].moves, Some(FolderId(2)));
        assert!(plans[0].flagged.is_none());
    }

    #[test]
    fn plan_merge_move_and_star() {
        let rules = vec![MessageRule {
            position: 0,
            actions: vec![RuleAction::Star, RuleAction::MoveToFolder(FolderId(3))],
            stop_processing: false,
            ..sample_rule(vec![], vec![], RuleMatchMode::All)
        }];
        let env = sample_envelope("a@b.com", "hi");
        let plans = plan_rules(&rules, &[env], None);
        assert_eq!(plans[0].flagged, Some(true));
        assert_eq!(plans[0].moves, Some(FolderId(3)));
    }

    #[test]
    fn skip_disabled() {
        let rules = vec![MessageRule {
            enabled: false,
            actions: vec![RuleAction::Star],
            ..sample_rule(vec![], vec![], RuleMatchMode::All)
        }];
        let env = sample_envelope("a@b.com", "hi");
        assert!(plan_rules(&rules, &[env], None).is_empty());
    }

    #[test]
    fn skip_server_when_client_filter() {
        let rules = vec![
            MessageRule {
                position: 0,
                execution: RuleExecution::Server,
                actions: vec![RuleAction::Star],
                ..sample_rule(vec![], vec![], RuleMatchMode::All)
            },
            MessageRule {
                position: 1,
                execution: RuleExecution::Client,
                actions: vec![RuleAction::MarkRead],
                ..sample_rule(vec![], vec![], RuleMatchMode::All)
            },
        ];
        let env = sample_envelope("a@b.com", "hi");
        let plans = plan_rules(&rules, &[env], Some(RuleExecution::Client));
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].seen, Some(true));
        assert!(plans[0].flagged.is_none());
    }

    #[test]
    fn default_stop_processing_move() {
        assert!(default_stop_processing(&[RuleAction::MoveToFolder(
            FolderId(1)
        )]));
        assert!(default_stop_processing(&[RuleAction::MoveToTrash]));
        assert!(!default_stop_processing(&[RuleAction::Star]));
    }

    #[test]
    fn validate_rule_errors() {
        let now = Utc::now();
        let base = MessageRule {
            id: RuleId(Uuid::new_v4()),
            account_id: sample_account(),
            name: "x".to_string(),
            enabled: true,
            position: 0,
            match_mode: RuleMatchMode::All,
            stop_processing: false,
            conditions: vec![RuleCondition {
                field: RuleField::From,
                op: RuleOp::Is,
                value: "a@b.com".to_string(),
            }],
            actions: vec![RuleAction::Star],
            execution: RuleExecution::Client,
            created_at: now,
            updated_at: now,
        };
        assert!(validate_rule(&base).is_ok());
        let mut empty_name = base.clone();
        empty_name.name = "  ".to_string();
        assert_eq!(validate_rule(&empty_name), Err("Enter a name."));
    }

    #[test]
    fn server_rule_summary_missing_folder() {
        let rule = ServerRuleView {
            name: "Server".to_string(),
            match_mode: RuleMatchMode::All,
            stop_processing: true,
            conditions: vec![RuleCondition {
                field: RuleField::From,
                op: RuleOp::Is,
                value: "a@b.com".to_string(),
            }],
            actions: vec![ServerRuleAction::MoveToFolder {
                imap_name: "Newsletters".to_string(),
                folder_id: None,
            }],
        };
        let summary = server_rule_summary(&rule, &[]);
        assert!(summary.contains("Missing folder"));
    }

    #[test]
    fn rule_id_uuid_serde_roundtrip() {
        let id = RuleId(Uuid::new_v4());
        let json = serde_json::to_string(&id).expect("serialize");
        let back: RuleId = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(id, back);
    }
    fn folder(id: i64, special: SpecialUse) -> Folder {
        Folder {
            id: FolderId(id),
            account_id: sample_account(),
            imap_name: format!("Folder {id}"),
            delimiter: "/".to_string(),
            special,
            selectable: true,
            uidvalidity: None,
            uidnext: None,
            highest_modseq: None,
            unread: 0,
            total: 0,
        }
    }

    fn from_contains(value: &str) -> Vec<RuleCondition> {
        vec![RuleCondition {
            field: RuleField::From,
            op: RuleOp::Contains,
            value: value.to_string(),
        }]
    }

    #[test]
    fn server_rule_needs_every_folder_its_actions_name() {
        let folders = [folder(1, SpecialUse::Inbox), folder(2, SpecialUse::Other)];
        let files_into_existing = sample_rule(
            from_contains("news"),
            vec![RuleAction::MoveToFolder(FolderId(2)), RuleAction::MarkRead],
            RuleMatchMode::All,
        );
        assert!(rule_can_run_on_server(&files_into_existing, &folders));

        let copies_into_missing = sample_rule(
            from_contains("news"),
            vec![RuleAction::CopyToFolder(FolderId(9))],
            RuleMatchMode::All,
        );
        assert!(!rule_can_run_on_server(&copies_into_missing, &folders));
    }

    #[test]
    fn server_rule_moving_to_trash_needs_a_trash_folder() {
        let rule = sample_rule(
            from_contains("spam"),
            vec![RuleAction::MoveToTrash],
            RuleMatchMode::All,
        );
        assert!(!rule_can_run_on_server(
            &rule,
            &[folder(1, SpecialUse::Inbox)]
        ));
        assert!(rule_can_run_on_server(
            &rule,
            &[folder(1, SpecialUse::Inbox), folder(3, SpecialUse::Trash)]
        ));
    }

    #[test]
    fn server_rule_cannot_test_for_attachments() {
        let mut conditions = from_contains("news");
        conditions.push(RuleCondition {
            field: RuleField::HasAttachment,
            op: RuleOp::Is,
            value: String::new(),
        });
        let rule = sample_rule(conditions, vec![RuleAction::Star], RuleMatchMode::Any);
        assert!(!rule_can_run_on_server(
            &rule,
            &[folder(1, SpecialUse::Inbox)]
        ));
    }
}
