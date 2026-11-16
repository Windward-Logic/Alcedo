//! `StoreHandle` methods for message rules and their execution log.

use super::*;

impl StoreHandle {
    pub async fn list_rules(&self, account: AccountId) -> Result<Vec<MessageRule>> {
        send_read(&self.read_tx, |resp| ReadRequest::ListRules {
            account,
            resp,
        })
        .await
    }

    pub async fn upsert_rule(&self, rule: &MessageRule) -> Result<MessageRule> {
        send_write(&self.write_tx, |resp| WriteRequest::UpsertRule {
            rule: rule.clone(),
            resp,
        })
        .await
    }

    pub async fn delete_rule(&self, id: RuleId) -> Result<()> {
        send_write(&self.write_tx, |resp| WriteRequest::DeleteRule { id, resp }).await
    }

    pub async fn reorder_rules(&self, account: AccountId, ids: &[RuleId]) -> Result<()> {
        send_write(&self.write_tx, |resp| WriteRequest::ReorderRules {
            account,
            ids: ids.to_vec(),
            resp,
        })
        .await
    }

    pub async fn set_rule_execution(&self, id: RuleId, execution: RuleExecution) -> Result<()> {
        send_write(&self.write_tx, |resp| WriteRequest::SetRuleExecution {
            id,
            execution,
            resp,
        })
        .await
    }

    pub async fn has_rule_run(&self, message_id: MessageId) -> Result<bool> {
        send_read(&self.read_tx, |resp| ReadRequest::HasRuleRun {
            message_id,
            resp,
        })
        .await
    }

    pub async fn rule_runs_for(&self, ids: &[MessageId]) -> Result<HashSet<MessageId>> {
        send_read(&self.read_tx, |resp| ReadRequest::RuleRunsFor {
            ids: ids.to_vec(),
            resp,
        })
        .await
    }

    pub async fn mark_rule_run(&self, message_id: MessageId) -> Result<()> {
        send_write(&self.write_tx, |resp| WriteRequest::MarkRuleRun {
            message_id,
            resp,
        })
        .await
    }

    pub async fn mark_rule_runs(&self, ids: &[MessageId]) -> Result<()> {
        send_write(&self.write_tx, |resp| WriteRequest::MarkRuleRuns {
            ids: ids.to_vec(),
            resp,
        })
        .await
    }
}
