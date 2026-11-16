//! `StoreHandle` lanes for extensions: run a closure on the reader or writer
//! connection, so tables an extension owns stay in that extension.

use super::*;

impl StoreHandle {
    /// Run `f` on the read-only connection. `label` names the request in logs.
    pub async fn extension_read<T, F>(&self, label: &'static str, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&rusqlite::Connection) -> StoreResult<T> + Send + 'static,
    {
        send_read(&self.read_tx, |resp| ReadRequest::Extension {
            label,
            run: Box::new(move |conn| {
                let _ = resp.send(f(conn));
            }),
        })
        .await
    }

    /// Run `f` on the writer connection. `label` names the request in logs.
    pub async fn extension_write<T, F>(&self, label: &'static str, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&rusqlite::Connection) -> StoreResult<T> + Send + 'static,
    {
        send_write(&self.write_tx, |resp| WriteRequest::Extension {
            label,
            run: Box::new(move |conn| {
                let _ = resp.send(f(conn));
            }),
        })
        .await
    }
}
