//! `StoreHandle` methods for full-text and structured message search.

use super::*;

impl StoreHandle {
    pub async fn search(
        &self,
        query: &str,
        folder_ids: Option<&[FolderId]>,
        limit: u32,
    ) -> Result<Vec<Envelope>> {
        send_read(&self.read_tx, |resp| ReadRequest::Search {
            query: query.to_string(),
            folder_ids: folder_ids.map(|ids| ids.to_vec()),
            limit,
            resp,
        })
        .await
    }

    pub async fn search_conversations(
        &self,
        query: &str,
        folder_ids: Option<&[FolderId]>,
        limit: u32,
    ) -> Result<Vec<Conversation>> {
        send_read(&self.read_tx, |resp| ReadRequest::SearchConversations {
            query: query.to_string(),
            folder_ids: folder_ids.map(|ids| ids.to_vec()),
            limit,
            resp,
        })
        .await
    }
}
