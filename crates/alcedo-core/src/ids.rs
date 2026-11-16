use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub struct AccountId(pub Uuid);

#[derive(Clone, Copy, Debug, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub struct RuleId(pub Uuid);

#[derive(Clone, Copy, Debug, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub struct IdentityId(pub Uuid);

#[derive(Clone, Copy, Debug, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub struct SnippetId(pub Uuid);

#[derive(Clone, Copy, Debug, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub struct QueuedSendId(pub Uuid);

#[derive(Clone, Copy, Debug, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub struct FolderId(pub i64);

#[derive(Clone, Copy, Debug, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub struct MessageId(pub i64);

#[derive(Clone, Copy, Debug, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub struct ThreadId(pub Uuid);

/// First threading token from RFC headers (References, In-Reply-To, then Message-ID).
pub fn thread_root_token(
    message_id: Option<&str>,
    in_reply_to: Option<&str>,
    references: Option<&str>,
) -> Option<String> {
    references
        .and_then(|r| r.split_whitespace().next())
        .filter(|s| !s.trim().is_empty())
        .map(str::to_string)
        .or_else(|| {
            in_reply_to
                .filter(|s| !s.trim().is_empty())
                .map(str::to_string)
        })
        .or_else(|| {
            message_id
                .filter(|s| !s.trim().is_empty())
                .map(str::to_string)
        })
}

/// Stable thread root from a Message-ID token, References first entry, or orphan key.
pub fn thread_id_from_root(token: &str) -> ThreadId {
    let hash = Sha256::digest(token.as_bytes());
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&hash[..16]);
    ThreadId(Uuid::from_bytes(bytes))
}

/// Per-message thread id when no RFC 5322 threading headers are available.
pub fn orphan_thread_id(folder_id: FolderId, uid: u32) -> ThreadId {
    thread_id_from_root(&format!("orphan:{}:{}", folder_id.0, uid))
}

/// Legacy sentinel hashed before orphan fallback (migration 012 backfill).
pub fn sentinel_thread_id() -> ThreadId {
    thread_id_from_root("<no-message-id>")
}

#[derive(Clone, Copy, Debug, Hash, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
pub struct ContactId(pub i64);

#[derive(Clone, Copy, Debug, Hash, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
pub struct GroupId(pub i64);
