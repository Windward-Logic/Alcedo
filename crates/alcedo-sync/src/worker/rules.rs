//! Mail rules: client-side execution, and the server-rules backend hand-off.
#![allow(clippy::too_many_arguments)]

use super::flags::set_flags_batch;
use super::moves::{copy_message_to_folder, move_messages_batch, move_messages_to_folder_batch};
use super::*;
use crate::server_rules::ServerRulesBackend;
use alcedo_i18n::ntr;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum RuleApplyOp {
    Copy(FolderId),
    SetFlag {
        seen: Option<bool>,
        flagged: Option<bool>,
    },
    Move(FolderId),
    Trash,
}

/// Pure plan for applying a [`RulePlan`] (copies, then flags, then move/trash).
pub(super) fn execute_rule_plan(plan: &RulePlan) -> Vec<RuleApplyOp> {
    let mut ops = Vec::new();
    for &dest in &plan.copies {
        ops.push(RuleApplyOp::Copy(dest));
    }
    if let Some(seen) = plan.seen {
        ops.push(RuleApplyOp::SetFlag {
            seen: Some(seen),
            flagged: None,
        });
    }
    if let Some(flagged) = plan.flagged {
        ops.push(RuleApplyOp::SetFlag {
            seen: None,
            flagged: Some(flagged),
        });
    }
    if plan.trash {
        ops.push(RuleApplyOp::Trash);
    } else if let Some(dest) = plan.moves {
        ops.push(RuleApplyOp::Move(dest));
    }
    ops
}

/// Ask the backend (if any) whether the server can run Alcedo's rules.
async fn probe_server_rules(account: &Account, server_rules: &mut ServerRulesSession) {
    server_rules.status = match &server_rules.backend {
        Some(backend) => backend.probe(account).await,
        None => ServerRulesStatus::Unavailable,
    };
    server_rules.probed = true;
}

/// Mark each rule Server when `published_to` (the backend that just published
/// the rules) says it put the rule on the server; with `None`, every rule runs
/// in Alcedo.
pub(super) async fn refresh_rule_executions(
    store: &StoreHandle,
    account_id: AccountId,
    published_to: Option<&dyn ServerRulesBackend>,
) -> Result<()> {
    let rules = store.list_rules(account_id).await?;
    let folders = match published_to {
        Some(_) => store.list_folders(account_id).await?,
        None => Vec::new(),
    };
    for rule in rules {
        let execution = match published_to {
            Some(backend) if backend.can_publish(&rule, &folders) => RuleExecution::Server,
            _ => RuleExecution::Client,
        };
        if rule.execution != execution {
            store.set_rule_execution(rule.id, execution).await?;
        }
    }
    Ok(())
}

async fn publish_server_rules(
    account: &Account,
    store: &StoreHandle,
    backend: &dyn ServerRulesBackend,
) -> Result<()> {
    let rules = store.list_rules(account.id).await?;
    let folders = store.list_folders(account.id).await?;
    backend.publish(account, &rules, &folders).await
}

async fn fetch_foreign_preview(
    account: &Account,
    script_name: &str,
    store: &StoreHandle,
    backend: &dyn ServerRulesBackend,
) -> Option<ServerRulesPreview> {
    let folders = store.list_folders(account.id).await.ok()?;
    backend.preview(account, script_name, &folders).await
}

pub(super) async fn emit_rules(
    account: &Account,
    store: &StoreHandle,
    server_rules: &ServerRulesSession,
    events: &flume::Sender<SyncEvent>,
) {
    let rules = match store.list_rules(account.id).await {
        Ok(rules) => rules,
        Err(err) => {
            warn!(error = %err, "failed to list rules for RulesUpdated");
            return;
        }
    };

    let preview = match (&server_rules.status, &server_rules.backend) {
        (ServerRulesStatus::ForeignActive { name }, Some(backend)) => {
            fetch_foreign_preview(account, name, store, backend.as_ref()).await
        }
        _ => None,
    };

    let _ = events.send(SyncEvent::RulesUpdated {
        account: account.id,
        rules,
        server_rules: server_rules.status.clone(),
        preview,
    });
}

pub(super) fn server_rules_need_probe(server_rules: &ServerRulesSession) -> bool {
    !server_rules.probed
        || matches!(
            server_rules.status,
            ServerRulesStatus::Unavailable | ServerRulesStatus::Unknown
        )
}

pub(super) async fn probe_and_maybe_upload(
    account: &Account,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
    server_rules: &mut ServerRulesSession,
) -> Result<()> {
    probe_server_rules(account, server_rules).await;
    sync_server_rules(account, store, server_rules).await?;
    emit_rules(account, store, server_rules, events).await;
    Ok(())
}

pub(super) async fn sync_server_rules(
    account: &Account,
    store: &StoreHandle,
    server_rules: &mut ServerRulesSession,
) -> Result<()> {
    let backend = match (&server_rules.status, &server_rules.backend) {
        (ServerRulesStatus::Ready, Some(backend)) => backend.clone(),
        _ => {
            refresh_rule_executions(store, account.id, None).await?;
            return Ok(());
        }
    };
    match publish_server_rules(account, store, backend.as_ref()).await {
        Ok(()) => {
            refresh_rule_executions(store, account.id, Some(backend.as_ref())).await?;
        }
        Err(err) => {
            warn!(account = %account.email, error = %err, "failed to publish server rules");
            server_rules.status = ServerRulesStatus::Unavailable;
            refresh_rule_executions(store, account.id, None).await?;
        }
    }
    Ok(())
}

pub(super) async fn after_rule_mutation(
    account: &Account,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
    server_rules: &mut ServerRulesSession,
) -> Result<()> {
    if server_rules_need_probe(server_rules) {
        probe_server_rules(account, server_rules).await;
    }
    sync_server_rules(account, store, server_rules).await?;
    emit_rules(account, store, server_rules, events).await;
    Ok(())
}

/// Replace a server script Alcedo did not write with Alcedo's own rules.
pub(super) async fn adopt_server_rules(
    account: &Account,
    store: &StoreHandle,
    server_rules: &mut ServerRulesSession,
) -> Result<()> {
    let Some(backend) = server_rules.backend.clone() else {
        return Err(Error::Other(
            "this account cannot run rules on the server".to_string(),
        ));
    };
    publish_server_rules(account, store, backend.as_ref()).await?;
    refresh_rule_executions(store, account.id, Some(backend.as_ref())).await?;
    server_rules.status = ServerRulesStatus::Ready;
    Ok(())
}

pub(super) async fn apply_rule_plans(
    state: &mut WorkerImapState,
    account: &Account,
    plans: &[RulePlan],
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
) -> Result<u32> {
    let mut updated = 0u32;
    let mut flag_batches: HashMap<(Option<bool>, Option<bool>), Vec<MessageId>> = HashMap::new();
    let mut trash_ids: Vec<MessageId> = Vec::new();
    let mut move_batches: HashMap<FolderId, Vec<MessageId>> = HashMap::new();
    let mut copy_err: Option<Error> = None;

    for plan in plans {
        let ops = execute_rule_plan(plan);
        if ops.is_empty() {
            continue;
        }
        let mut plan_flags: Vec<(Option<bool>, Option<bool>)> = Vec::new();
        let mut plan_trash = false;
        let mut plan_move: Option<FolderId> = None;
        let mut plan_failed = false;
        for op in ops {
            match op {
                RuleApplyOp::Copy(dest) => {
                    if let Err(err) = copy_message_to_folder(
                        state,
                        account,
                        plan.message_id,
                        dest,
                        store,
                        events,
                        false,
                    )
                    .await
                    {
                        copy_err = Some(err);
                        plan_failed = true;
                        break;
                    }
                }
                RuleApplyOp::SetFlag { seen, flagged } => plan_flags.push((seen, flagged)),
                RuleApplyOp::Trash => plan_trash = true,
                RuleApplyOp::Move(dest) => plan_move = Some(dest),
            }
        }
        if plan_failed {
            break;
        }
        updated += 1;
        for (seen, flagged) in plan_flags {
            flag_batches
                .entry((seen, flagged))
                .or_default()
                .push(plan.message_id);
        }
        if plan_trash {
            trash_ids.push(plan.message_id);
        }
        if let Some(dest) = plan_move {
            move_batches.entry(dest).or_default().push(plan.message_id);
        }
    }

    let mut batch_err: Option<Error> = None;
    for ((seen, flagged), ids) in flag_batches {
        if let Err(err) = set_flags_batch(state, account, ids, seen, flagged, store, events).await
            && batch_err.is_none()
        {
            batch_err = Some(err);
        }
    }
    if !trash_ids.is_empty()
        && let Err(err) =
            move_messages_batch(state, account, trash_ids, SpecialUse::Trash, store, events).await
        && batch_err.is_none()
    {
        batch_err = Some(err);
    }
    for (dest, ids) in move_batches {
        if let Err(err) =
            move_messages_to_folder_batch(state, account, ids, dest, store, events, false).await
            && batch_err.is_none()
        {
            batch_err = Some(err);
        }
    }
    if let Some(err) = copy_err {
        return Err(err);
    }
    if let Some(err) = batch_err {
        return Err(err);
    }
    Ok(updated)
}

pub(super) async fn auto_apply_inbox_rules(
    state: &mut WorkerImapState,
    account: &Account,
    folder: &Folder,
    envelopes: &[alcedo_core::Envelope],
    existing_uids: &HashSet<u32>,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
    server_rules: &ServerRulesSession,
) -> Result<()> {
    let new_uids: Vec<u32> = envelopes
        .iter()
        .map(|e| e.uid)
        .filter(|uid| !existing_uids.contains(uid))
        .collect();
    if new_uids.is_empty() {
        return Ok(());
    }

    let id_rows = store.messages_by_uids(folder.id, &new_uids).await?;
    let id_by_uid: HashMap<u32, MessageId> =
        id_rows.into_iter().map(|(id, uid, _)| (uid, id)).collect();

    let mut all_new: Vec<alcedo_core::Envelope> = envelopes
        .iter()
        .filter(|e| new_uids.contains(&e.uid))
        .cloned()
        .collect();
    for env in &mut all_new {
        if let Some(id) = id_by_uid.get(&env.uid) {
            env.id = *id;
        }
    }

    let mut to_plan = Vec::new();
    let all_ids: Vec<MessageId> = all_new.iter().map(|env| env.id).collect();
    let already_run = store.rule_runs_for(&all_ids).await?;
    for env in &all_new {
        if already_run.contains(&env.id) {
            continue;
        }
        to_plan.push(env.clone());
    }

    let rules = store.list_rules(account.id).await?;
    let execution_filter = if matches!(server_rules.status, ServerRulesStatus::Ready) {
        Some(RuleExecution::Client)
    } else {
        None
    };
    let plans = plan_rules(&rules, &to_plan, execution_filter);
    let _ = apply_rule_plans(state, account, &plans, store, events).await?;

    let mark_ids: Vec<MessageId> = all_new
        .iter()
        .filter(|env| !already_run.contains(&env.id))
        .map(|env| env.id)
        .collect();
    store.mark_rule_runs(&mark_ids).await?;

    Ok(())
}

pub(super) async fn run_message_rules(
    state: &mut WorkerImapState,
    account: &Account,
    folder_id: FolderId,
    rule_ids: Option<Vec<RuleId>>,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
) -> Result<()> {
    let envelopes = store.folder_messages_all(folder_id).await?;
    let mut rules = store.list_rules(account.id).await?;
    if let Some(ids) = rule_ids {
        rules.retain(|rule| ids.contains(&rule.id));
    }
    let plans = plan_rules(&rules, &envelopes, None);
    let updated = apply_rule_plans(state, account, &plans, store, events).await?;
    emit_status(
        events,
        account,
        ntr!(
            "Ran rules: {n} message updated.",
            "Ran rules: {n} messages updated.",
            updated
        ),
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use alcedo_core::{FolderId, MessageId, RuleAction, RulePlan};

    struct FixedBackend(ServerRulesStatus);

    impl ServerRulesBackend for FixedBackend {
        fn probe<'a>(
            &'a self,
            _account: &'a Account,
        ) -> crate::server_rules::BoxFuture<'a, ServerRulesStatus> {
            Box::pin(std::future::ready(self.0.clone()))
        }

        fn publish<'a>(
            &'a self,
            _account: &'a Account,
            _rules: &'a [MessageRule],
            _folders: &'a [Folder],
        ) -> crate::server_rules::BoxFuture<'a, Result<()>> {
            Box::pin(std::future::ready(Ok(())))
        }

        fn preview<'a>(
            &'a self,
            _account: &'a Account,
            _name: &'a str,
            _folders: &'a [Folder],
        ) -> crate::server_rules::BoxFuture<'a, Option<ServerRulesPreview>> {
            Box::pin(std::future::ready(None))
        }
    }

    /// Accepts what the default accepts, except rules with this name.
    struct RefusesByName(&'static str);

    impl ServerRulesBackend for RefusesByName {
        fn probe<'a>(
            &'a self,
            account: &'a Account,
        ) -> crate::server_rules::BoxFuture<'a, ServerRulesStatus> {
            FixedBackend(ServerRulesStatus::Ready).probe(account)
        }

        fn publish<'a>(
            &'a self,
            _account: &'a Account,
            _rules: &'a [MessageRule],
            _folders: &'a [Folder],
        ) -> crate::server_rules::BoxFuture<'a, Result<()>> {
            Box::pin(std::future::ready(Ok(())))
        }

        fn preview<'a>(
            &'a self,
            _account: &'a Account,
            _name: &'a str,
            _folders: &'a [Folder],
        ) -> crate::server_rules::BoxFuture<'a, Option<ServerRulesPreview>> {
            Box::pin(std::future::ready(None))
        }

        fn can_publish(&self, rule: &MessageRule, folders: &[Folder]) -> bool {
            rule.name != self.0 && alcedo_core::rule_can_run_on_server(rule, folders)
        }
    }

    /// A store with Inbox and Newsletters (no Trash) and one rule per action.
    struct RulesFixture {
        _dir: tempfile::TempDir,
        store: StoreHandle,
        account: Account,
    }

    impl RulesFixture {
        async fn new(rules: &[(&str, RuleAction)]) -> Self {
            use super::super::test_support::{account_with_imap_host, folder_named};
            let dir = tempfile::tempdir().expect("temp dir");
            let store = StoreHandle::open_path(&dir.path().join("mail.db")).expect("store");
            let account = account_with_imap_host("imap.example.com");
            store.upsert_account(&account).await.expect("account");
            let folders = store
                .replace_folders(
                    account.id,
                    &[
                        folder_named(0, SpecialUse::Inbox, "INBOX", true),
                        folder_named(0, SpecialUse::Other, "Newsletters", true),
                    ],
                )
                .await
                .expect("folders");
            let newsletters = folders[1].id;
            for (position, (name, action)) in rules.iter().enumerate() {
                let action = match action {
                    RuleAction::MoveToFolder(_) => RuleAction::MoveToFolder(newsletters),
                    other => other.clone(),
                };
                let now = chrono::Utc::now();
                store
                    .upsert_rule(&MessageRule {
                        id: RuleId(uuid::Uuid::new_v4()),
                        account_id: account.id,
                        name: (*name).to_string(),
                        enabled: true,
                        position: position as i32,
                        match_mode: alcedo_core::RuleMatchMode::All,
                        stop_processing: true,
                        conditions: vec![alcedo_core::RuleCondition {
                            field: alcedo_core::RuleField::From,
                            op: alcedo_core::RuleOp::Contains,
                            value: "news".to_string(),
                        }],
                        actions: vec![action],
                        execution: RuleExecution::Client,
                        created_at: now,
                        updated_at: now,
                    })
                    .await
                    .expect("rule");
            }
            Self {
                _dir: dir,
                store,
                account,
            }
        }

        async fn executions(&self) -> Vec<(String, RuleExecution)> {
            let mut rules = self.store.list_rules(self.account.id).await.expect("rules");
            rules.sort_by_key(|rule| rule.position);
            rules
                .into_iter()
                .map(|rule| (rule.name, rule.execution))
                .collect()
        }
    }

    #[tokio::test]
    async fn published_rules_run_on_the_server_unless_a_folder_is_missing() {
        let fixture = RulesFixture::new(&[
            ("file", RuleAction::MoveToFolder(FolderId(0))),
            ("trash", RuleAction::MoveToTrash),
        ])
        .await;
        let backend = FixedBackend(ServerRulesStatus::Ready);
        refresh_rule_executions(&fixture.store, fixture.account.id, Some(&backend))
            .await
            .expect("refresh");
        assert_eq!(
            fixture.executions().await,
            vec![
                ("file".to_string(), RuleExecution::Server),
                ("trash".to_string(), RuleExecution::Client),
            ]
        );
    }

    #[tokio::test]
    async fn a_rule_the_backend_does_not_publish_keeps_running_in_alcedo() {
        let fixture = RulesFixture::new(&[
            ("file", RuleAction::MoveToFolder(FolderId(0))),
            ("local only", RuleAction::Star),
        ])
        .await;
        let mut session =
            ServerRulesSession::new(Some(std::sync::Arc::new(RefusesByName("local only"))));
        session.status = ServerRulesStatus::Ready;
        sync_server_rules(&fixture.account, &fixture.store, &mut session)
            .await
            .expect("sync");
        assert_eq!(
            fixture.executions().await,
            vec![
                ("file".to_string(), RuleExecution::Server),
                ("local only".to_string(), RuleExecution::Client),
            ]
        );
    }

    #[tokio::test]
    async fn without_a_ready_backend_every_rule_runs_in_alcedo() {
        let fixture = RulesFixture::new(&[("file", RuleAction::MoveToFolder(FolderId(0)))]).await;
        let backend = FixedBackend(ServerRulesStatus::Ready);
        refresh_rule_executions(&fixture.store, fixture.account.id, Some(&backend))
            .await
            .expect("refresh");
        let mut session = ServerRulesSession::new(Some(std::sync::Arc::new(backend)));
        session.status = ServerRulesStatus::Unavailable;
        sync_server_rules(&fixture.account, &fixture.store, &mut session)
            .await
            .expect("sync");
        assert_eq!(
            fixture.executions().await,
            vec![("file".to_string(), RuleExecution::Client)]
        );
    }

    #[tokio::test]
    async fn probe_without_backend_reports_unavailable() {
        let account = super::super::test_support::account_with_imap_host("imap.example.com");
        let mut session = ServerRulesSession::new(None);
        probe_server_rules(&account, &mut session).await;
        assert!(session.probed);
        assert_eq!(session.status, ServerRulesStatus::Unavailable);
    }

    #[tokio::test]
    async fn probe_reports_the_backend_status() {
        let account = super::super::test_support::account_with_imap_host("imap.example.com");
        let foreign = ServerRulesStatus::ForeignActive {
            name: "roundcube".into(),
        };
        let mut session =
            ServerRulesSession::new(Some(std::sync::Arc::new(FixedBackend(foreign.clone()))));
        probe_server_rules(&account, &mut session).await;
        assert_eq!(session.status, foreign);
    }

    #[tokio::test]
    async fn adopting_server_rules_without_backend_fails() {
        let account = super::super::test_support::account_with_imap_host("imap.example.com");
        let dir = tempfile::tempdir().expect("temp dir");
        let store =
            alcedo_store::StoreHandle::open_path(&dir.path().join("mail.db")).expect("store");
        let mut session = ServerRulesSession::new(None);
        assert!(
            adopt_server_rules(&account, &store, &mut session)
                .await
                .is_err()
        );
        assert_eq!(session.status, ServerRulesStatus::Unknown);
    }

    #[test]
    fn server_rules_need_probe_retries_unavailable_not_ready_or_foreign() {
        let mut session = ServerRulesSession::default();
        assert!(server_rules_need_probe(&session));

        session.probed = true;
        session.status = ServerRulesStatus::Unavailable;
        assert!(server_rules_need_probe(&session));

        session.status = ServerRulesStatus::Ready;
        assert!(!server_rules_need_probe(&session));

        session.status = ServerRulesStatus::ForeignActive {
            name: "roundcube".into(),
        };
        assert!(!server_rules_need_probe(&session));
    }

    #[test]
    fn execute_rule_plan_orders_copy_flags_move() {
        let plan = RulePlan {
            message_id: MessageId(1),
            moves: Some(FolderId(3)),
            copies: vec![FolderId(2)],
            seen: Some(true),
            flagged: Some(true),
            trash: false,
        };
        assert_eq!(
            execute_rule_plan(&plan),
            vec![
                RuleApplyOp::Copy(FolderId(2)),
                RuleApplyOp::SetFlag {
                    seen: Some(true),
                    flagged: None,
                },
                RuleApplyOp::SetFlag {
                    seen: None,
                    flagged: Some(true),
                },
                RuleApplyOp::Move(FolderId(3)),
            ]
        );
    }

    #[test]
    fn execute_rule_plan_trash_overrides_move() {
        let plan = RulePlan {
            message_id: MessageId(1),
            moves: Some(FolderId(3)),
            copies: Vec::new(),
            seen: None,
            flagged: None,
            trash: true,
        };
        assert_eq!(execute_rule_plan(&plan), vec![RuleApplyOp::Trash]);
    }

    #[test]
    fn execute_rule_plan_flags_only() {
        let plan = RulePlan {
            message_id: MessageId(1),
            moves: None,
            copies: Vec::new(),
            seen: Some(false),
            flagged: Some(false),
            trash: false,
        };
        assert_eq!(
            execute_rule_plan(&plan),
            vec![
                RuleApplyOp::SetFlag {
                    seen: Some(false),
                    flagged: None,
                },
                RuleApplyOp::SetFlag {
                    seen: None,
                    flagged: Some(false),
                },
            ]
        );
    }
}
