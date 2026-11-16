//! `StoreHandle` methods for contacts and contact groups.

use super::*;

impl StoreHandle {
    pub async fn contacts_snapshot(&self) -> Result<(Vec<Contact>, Vec<ContactGroup>)> {
        send_read(&self.read_tx, |resp| ReadRequest::ContactsSnapshot { resp }).await
    }

    pub async fn upsert_contact(&self, draft: ContactDraft) -> Result<Contact> {
        send_write(&self.write_tx, |resp| WriteRequest::UpsertContact {
            draft,
            resp,
        })
        .await
    }

    pub async fn delete_contacts(&self, ids: Vec<ContactId>) -> Result<()> {
        send_write(&self.write_tx, |resp| WriteRequest::DeleteContacts {
            ids,
            resp,
        })
        .await
    }

    pub async fn delete_contact(&self, id: ContactId) -> Result<()> {
        self.delete_contacts(vec![id]).await
    }

    pub async fn upsert_group(&self, draft: GroupDraft) -> Result<ContactGroup> {
        send_write(&self.write_tx, |resp| WriteRequest::UpsertGroup {
            draft,
            resp,
        })
        .await
    }

    pub async fn delete_group(&self, id: GroupId) -> Result<()> {
        send_write(&self.write_tx, |resp| WriteRequest::DeleteGroup {
            id,
            resp,
        })
        .await
    }

    pub async fn promote_contact(&self, id: ContactId) -> Result<Contact> {
        send_write(&self.write_tx, |resp| WriteRequest::PromoteContact {
            id,
            resp,
        })
        .await
    }

    pub async fn block_contact(&self, id: ContactId) -> Result<Contact> {
        send_write(&self.write_tx, |resp| WriteRequest::BlockContact {
            id,
            resp,
        })
        .await
    }

    pub async fn allow_sender(&self, email: &str, display_name: &str) -> Result<Contact> {
        send_write(&self.write_tx, |resp| WriteRequest::AllowSender {
            email: email.to_string(),
            display_name: display_name.to_string(),
            resp,
        })
        .await
    }

    pub async fn collect_outgoing(&self, account_email: &str, addresses: &[Address]) -> Result<()> {
        send_write(&self.write_tx, |resp| WriteRequest::CollectOutgoing {
            account_email: account_email.to_string(),
            addresses: addresses.to_vec(),
            resp,
        })
        .await
    }
}
