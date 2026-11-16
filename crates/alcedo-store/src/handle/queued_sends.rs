//! `StoreHandle` methods for the outgoing send queue.

use super::*;

impl StoreHandle {
    pub async fn insert_queued_send(&self, queued: &QueuedSend) -> Result<()> {
        send_write(&self.write_tx, |resp| WriteRequest::InsertQueuedSend {
            queued: Box::new(queued.clone()),
            resp,
        })
        .await
    }

    pub async fn delete_queued_send(&self, id: QueuedSendId) -> Result<()> {
        send_write(&self.write_tx, |resp| WriteRequest::DeleteQueuedSend {
            id,
            resp,
        })
        .await
    }

    pub async fn list_queued_sends(&self) -> Result<Vec<QueuedSend>> {
        send_read(&self.read_tx, |resp| ReadRequest::ListQueuedSends { resp }).await
    }
}
