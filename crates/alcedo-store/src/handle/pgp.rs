//! `StoreHandle` methods for OpenPGP public keys and per-message status.

use super::*;

impl StoreHandle {
    pub async fn upsert_pgp_key(
        &self,
        fingerprint: &str,
        emails: &[String],
        armored: &str,
        source: alcedo_core::PgpKeySource,
        account_id: Option<AccountId>,
    ) -> Result<()> {
        send_write(&self.write_tx, |resp| WriteRequest::UpsertPgpKey {
            fingerprint: fingerprint.to_string(),
            emails: emails.to_vec(),
            armored: armored.to_string(),
            source,
            account_id,
            resp,
        })
        .await
    }

    pub async fn delete_own_pgp_key(&self, account: AccountId) -> Result<()> {
        send_write(&self.write_tx, |resp| WriteRequest::DeleteOwnPgpKey {
            account,
            resp,
        })
        .await
    }

    pub async fn delete_all_pgp_keys(&self, account: AccountId) -> Result<()> {
        send_write(&self.write_tx, |resp| WriteRequest::DeleteAllPgpKeys {
            account,
            resp,
        })
        .await
    }

    pub async fn list_pgp_keys(&self, account: AccountId) -> Result<Vec<PgpPeerInfo>> {
        send_read(&self.read_tx, |resp| ReadRequest::ListPgpKeys {
            account,
            resp,
        })
        .await
    }

    pub async fn pgp_keys_for_emails(&self, emails: &[String]) -> Result<Vec<PgpPeerInfo>> {
        send_read(&self.read_tx, |resp| ReadRequest::KeysForEmails {
            emails: emails.to_vec(),
            resp,
        })
        .await
    }

    pub async fn get_pgp_sign_outgoing(&self, account: AccountId) -> Result<bool> {
        send_read(&self.read_tx, |resp| ReadRequest::GetPgpSignOutgoing {
            account,
            resp,
        })
        .await
    }

    pub async fn set_pgp_sign_outgoing(&self, account: AccountId, enabled: bool) -> Result<()> {
        send_write(&self.write_tx, |resp| WriteRequest::SetPgpSignOutgoing {
            account,
            enabled,
            resp,
        })
        .await
    }
}
