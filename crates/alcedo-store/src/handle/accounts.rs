//! `StoreHandle` methods for mail accounts.

use super::*;

impl StoreHandle {
    pub async fn upsert_account(&self, a: &Account) -> Result<()> {
        send_write(&self.write_tx, |resp| WriteRequest::UpsertAccount {
            account: a.clone(),
            resp,
        })
        .await
    }

    pub async fn delete_account(&self, id: AccountId) -> Result<()> {
        send_write(&self.write_tx, |resp| WriteRequest::DeleteAccount {
            id,
            resp,
        })
        .await
    }

    pub async fn list_accounts(&self) -> Result<Vec<Account>> {
        send_read(&self.read_tx, |resp| ReadRequest::ListAccounts { resp }).await
    }

    pub async fn get_catch_all_domains(&self, account: AccountId) -> Result<Vec<String>> {
        send_read(&self.read_tx, |resp| ReadRequest::GetCatchAllDomains {
            account,
            resp,
        })
        .await
    }

    pub async fn set_catch_all_domains(
        &self,
        account: AccountId,
        domains: Vec<String>,
    ) -> Result<()> {
        send_write(&self.write_tx, |resp| WriteRequest::SetCatchAllDomains {
            account,
            domains,
            resp,
        })
        .await
    }

    pub async fn get_smtp_envelope_policy(&self, account: AccountId) -> Result<SmtpEnvelopePolicy> {
        send_read(&self.read_tx, |resp| ReadRequest::GetSmtpEnvelopePolicy {
            account,
            resp,
        })
        .await
    }

    pub async fn set_smtp_envelope_policy(
        &self,
        account: AccountId,
        policy: SmtpEnvelopePolicy,
    ) -> Result<()> {
        send_write(&self.write_tx, |resp| WriteRequest::SetSmtpEnvelopePolicy {
            account,
            policy,
            resp,
        })
        .await
    }
}
