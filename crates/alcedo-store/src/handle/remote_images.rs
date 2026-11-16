//! `StoreHandle` methods for per-sender remote image permissions.

use super::*;

impl StoreHandle {
    pub async fn list_remote_image_senders(&self) -> Result<Vec<String>> {
        send_read(&self.read_tx, |resp| ReadRequest::ListRemoteImageSenders {
            resp,
        })
        .await
    }

    pub async fn set_remote_image_sender(&self, email: &str, allow: bool) -> Result<()> {
        send_write(&self.write_tx, |resp| WriteRequest::SetRemoteImageSender {
            email: email.to_string(),
            allow,
            resp,
        })
        .await
    }
}
