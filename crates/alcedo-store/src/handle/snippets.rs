//! `StoreHandle` methods for reusable compose snippets.

use super::*;

impl StoreHandle {
    pub async fn list_snippets(&self) -> Result<Vec<Snippet>> {
        send_read(&self.read_tx, |resp| ReadRequest::ListSnippets { resp }).await
    }

    pub async fn upsert_snippet(&self, snippet: &Snippet) -> Result<Snippet> {
        send_write(&self.write_tx, |resp| WriteRequest::UpsertSnippet {
            snippet: snippet.clone(),
            resp,
        })
        .await
    }

    pub async fn delete_snippet(&self, id: SnippetId) -> Result<()> {
        send_write(&self.write_tx, |resp| WriteRequest::DeleteSnippet {
            id,
            resp,
        })
        .await
    }
}
