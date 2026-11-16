//! `StoreHandle` methods for attachment metadata and stored attachment files.

use super::*;

impl StoreHandle {
    pub async fn save_attachment(&self, meta: AttachmentMeta, bytes: &[u8]) -> Result<PathBuf> {
        send_write(&self.write_tx, |resp| WriteRequest::SaveAttachment {
            meta,
            bytes: bytes.to_vec(),
            resp,
        })
        .await
    }

    pub async fn delete_attachment(&self, message_id: MessageId, sha256: &str) -> Result<()> {
        send_write(&self.write_tx, |resp| WriteRequest::DeleteAttachment {
            message_id,
            sha256: sha256.to_string(),
            resp,
        })
        .await
    }

    pub async fn attachment_bytes(&self, sha256: &str) -> Result<Vec<u8>> {
        send_read(&self.read_tx, |resp| ReadRequest::LoadAttachment {
            sha256: sha256.to_string(),
            resp,
        })
        .await
    }
}
