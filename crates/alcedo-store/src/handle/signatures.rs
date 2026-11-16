//! `StoreHandle` methods for account signatures.

use super::*;

impl StoreHandle {
    pub async fn get_signature(&self, account: AccountId) -> Result<Option<AccountSignature>> {
        send_read(&self.read_tx, |resp| ReadRequest::GetSignature {
            account,
            resp,
        })
        .await
    }

    pub async fn upsert_signature(&self, signature: &AccountSignature) -> Result<AccountSignature> {
        send_write(&self.write_tx, |resp| WriteRequest::UpsertSignature {
            signature: signature.clone(),
            resp,
        })
        .await
    }

    pub async fn delete_signature(&self, account: AccountId) -> Result<()> {
        send_write(&self.write_tx, |resp| WriteRequest::DeleteSignature {
            account,
            resp,
        })
        .await
    }
}
