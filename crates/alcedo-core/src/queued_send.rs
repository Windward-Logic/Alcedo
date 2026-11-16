//! A composed message that was queued for sending and has not finished.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{ComposeDraft, QueuedSendId};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct QueuedSend {
    pub id: QueuedSendId,
    pub draft: ComposeDraft,
    pub queued_at: DateTime<Utc>,
}
