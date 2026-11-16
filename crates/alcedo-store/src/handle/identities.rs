//! `StoreHandle` methods for account sending identities.

use super::*;

impl StoreHandle {
    pub async fn list_identities(&self, account: AccountId) -> Result<Vec<AccountIdentity>> {
        send_read(&self.read_tx, |resp| ReadRequest::ListIdentities {
            account,
            resp,
        })
        .await
    }

    pub async fn upsert_identity(&self, identity: &AccountIdentity) -> Result<AccountIdentity> {
        send_write(&self.write_tx, |resp| WriteRequest::UpsertIdentity {
            identity: identity.clone(),
            resp,
        })
        .await
    }

    pub async fn delete_identity(&self, id: IdentityId) -> Result<()> {
        send_write(&self.write_tx, |resp| WriteRequest::DeleteIdentity {
            id,
            resp,
        })
        .await
    }
}
