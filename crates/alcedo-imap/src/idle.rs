//! IDLE on INBOX: DONE on command, 25-minute restart, EXISTS/EXPUNGE handling.

use std::time::Duration;

use alcedo_core::{Error, Result};
use async_imap::{
    Session,
    extensions::idle::{Handle, IdleResponse},
    imap_proto,
};
use stop_token::StopSource;
use tracing::debug;

use crate::client::{parse_exists_count, parse_expunge_seq};
use crate::tls::ImapTransport;

/// RFC 2177 recommends restarting IDLE before 29 minutes; we use 25.
pub const IDLE_RESTART_TIMEOUT: Duration = Duration::from_secs(25 * 60);

/// Unsolicited mailbox change while IDLE is active.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IdleNotification {
    Exists(u32),
    Expunge(u32),
    Updated,
}

/// Result of one IDLE wait cycle.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IdleWaitResult {
    Notification(IdleNotification),
    Timeout,
    Interrupted,
}

/// IDLE controller for a selected mailbox (typically INBOX).
pub struct IdleSession {
    handle: Option<Handle<ImapTransport>>,
}

impl IdleSession {
    /// Take ownership of the session and prepare for IDLE (mailbox must already be SELECTed).
    pub fn new(session: Session<ImapTransport>) -> Self {
        Self {
            handle: Some(session.idle()),
        }
    }

    /// Send `IDLE` and wait for the server continuation.
    pub async fn init(&mut self) -> Result<()> {
        let handle = self
            .handle
            .as_mut()
            .ok_or_else(|| Error::Imap("IDLE handle not available".to_string()))?;
        handle
            .init()
            .await
            .map_err(|e| Error::Imap(format!("IDLE init failed: {e}")))
    }

    /// Block until a mailbox update, manual `DONE` (via stop token), or 25-minute timeout.
    ///
    /// Returns a [`StopSource`] that sends `DONE` when stopped.
    pub fn wait(
        &mut self,
    ) -> Result<(
        impl std::future::Future<Output = Result<IdleWaitResult>> + '_,
        StopSource,
    )> {
        let handle = self
            .handle
            .as_mut()
            .ok_or_else(|| Error::Imap("IDLE wait called before init".to_string()))?;

        let (fut, stop) = handle.wait_with_timeout(IDLE_RESTART_TIMEOUT);
        let mapped = async move {
            match fut
                .await
                .map_err(|e| Error::Imap(format!("IDLE wait failed: {e}")))?
            {
                IdleResponse::Timeout => Ok(IdleWaitResult::Timeout),
                IdleResponse::ManualInterrupt => Ok(IdleWaitResult::Interrupted),
                IdleResponse::NewData(resp) => Ok(IdleWaitResult::Notification(
                    parse_idle_notification(resp.parsed()),
                )),
            }
        };
        Ok((mapped, stop))
    }

    /// Send `DONE` and return the underlying authenticated session.
    pub async fn done(mut self) -> Result<Session<ImapTransport>> {
        let handle = self
            .handle
            .take()
            .ok_or_else(|| Error::Imap("IDLE already finished".to_string()))?;
        handle
            .done()
            .await
            .map_err(|e| Error::Imap(format!("IDLE DONE failed: {e}")))
    }
}

fn parse_idle_notification(resp: &imap_proto::Response<'_>) -> IdleNotification {
    if let Some(count) = parse_exists_count(resp) {
        debug!(count, "IDLE EXISTS");
        return IdleNotification::Exists(count);
    }
    if let Some(seq) = parse_expunge_seq(resp) {
        debug!(seq, "IDLE EXPUNGE");
        return IdleNotification::Expunge(seq);
    }

    // RECENT, flag FETCHes and anything else: re-sync to find out what changed.
    IdleNotification::Updated
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_restart_is_25_minutes() {
        assert_eq!(IDLE_RESTART_TIMEOUT, Duration::from_secs(1500));
    }
}
