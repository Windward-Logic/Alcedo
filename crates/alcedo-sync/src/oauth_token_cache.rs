//! Process-wide OAuth access-token cache.
//!
//! Every worker for an account (IMAP, SMTP, server rules, extensions) asks here for
//! an access token. Tokens are kept with their expiry and refreshed 60 s
//! early; a per-account async mutex makes sure only one refresh runs at a
//! time, so concurrent workers share the result instead of each spending a
//! refresh-token round trip.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use alcedo_core::{OAuthTokens, Result};
use tokio::sync::Mutex;
use uuid::Uuid;

/// Refresh this long before the provider-reported expiry.
pub(crate) const REFRESH_MARGIN: Duration = Duration::from_secs(60);

/// What the token is for; grants with different scopes are cached apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TokenPurpose {
    Mail,
    /// A grant an extension holds for its own accounts, named by the extension.
    Extension(&'static str),
}

type Key = (Uuid, TokenPurpose);

#[derive(Clone)]
struct CachedToken {
    secret: String,
    /// `None` when the provider did not report `expires_in`; then the token is
    /// used until a server rejects it.
    expires_at: Option<Instant>,
}

impl CachedToken {
    fn from_tokens(tokens: &OAuthTokens, now: Instant) -> Self {
        Self {
            secret: tokens.access_token.clone(),
            expires_at: tokens
                .expires_in
                .map(|secs| now + Duration::from_secs(secs)),
        }
    }

    fn is_fresh(&self, now: Instant) -> bool {
        self.expires_at
            .is_none_or(|expires_at| now + REFRESH_MARGIN < expires_at)
    }
}

type Slot = Arc<Mutex<Option<CachedToken>>>;

static SLOTS: LazyLock<std::sync::Mutex<HashMap<Key, Slot>>> =
    LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));

fn slot(key: Key) -> Slot {
    let mut slots = SLOTS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    slots.entry(key).or_default().clone()
}

/// Return a fresh access token for `account`, running `refresh` only when
/// the cached one is missing, near expiry or was rejected. Concurrent callers
/// wait for the in-flight refresh and reuse its token.
pub async fn cached_access_token<F, Fut>(
    account: Uuid,
    purpose: TokenPurpose,
    refresh: F,
) -> Result<String>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<OAuthTokens>>,
{
    let slot = slot((account, purpose));
    let mut guard = slot.lock().await;
    if let Some(token) = guard.as_ref().filter(|t| t.is_fresh(Instant::now())) {
        return Ok(token.secret.clone());
    }
    let tokens = refresh().await?;
    let token = CachedToken::from_tokens(&tokens, Instant::now());
    let secret = token.secret.clone();
    *guard = Some(token);
    Ok(secret)
}

/// Drop the cached token after a server rejected it, unless another worker
/// already replaced it with a newer one.
pub async fn invalidate_access_token(account: Uuid, purpose: TokenPurpose, rejected: &str) {
    let slot = slot((account, purpose));
    let mut guard = slot.lock().await;
    if guard.as_ref().is_some_and(|t| t.secret == rejected) {
        *guard = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn tokens(secret: &str, expires_in: Option<u64>) -> OAuthTokens {
        OAuthTokens {
            access_token: secret.into(),
            refresh_token: None,
            expires_in,
        }
    }

    #[test]
    fn freshness_honours_refresh_margin() {
        let now = Instant::now();
        assert!(CachedToken::from_tokens(&tokens("a", Some(3600)), now).is_fresh(now));
        assert!(!CachedToken::from_tokens(&tokens("a", Some(59)), now).is_fresh(now));
        assert!(!CachedToken::from_tokens(&tokens("a", Some(60)), now).is_fresh(now));
        assert!(CachedToken::from_tokens(&tokens("a", None), now).is_fresh(now));
    }

    #[tokio::test]
    async fn concurrent_callers_share_one_refresh() {
        let account = Uuid::new_v4();
        let calls = Arc::new(AtomicUsize::new(0));
        let mut tasks = Vec::new();
        for _ in 0..8 {
            let calls = calls.clone();
            tasks.push(tokio::spawn(async move {
                cached_access_token(account, TokenPurpose::Mail, || async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    Ok(tokens("shared", Some(3600)))
                })
                .await
                .unwrap()
            }));
        }
        for task in tasks {
            assert_eq!(task.await.unwrap(), "shared");
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn expiring_or_rejected_tokens_are_refreshed() {
        let account = Uuid::new_v4();
        let first = cached_access_token(account, TokenPurpose::Mail, || async {
            Ok(tokens("short", Some(30)))
        })
        .await
        .unwrap();
        assert_eq!(first, "short");
        let second = cached_access_token(account, TokenPurpose::Mail, || async {
            Ok(tokens("long", Some(3600)))
        })
        .await
        .unwrap();
        assert_eq!(second, "long", "token inside the 60 s margin must refresh");

        invalidate_access_token(account, TokenPurpose::Mail, "not-current").await;
        let kept = cached_access_token(account, TokenPurpose::Mail, || async {
            panic!("must not refresh");
        })
        .await
        .unwrap();
        assert_eq!(kept, "long");

        invalidate_access_token(account, TokenPurpose::Mail, "long").await;
        let fresh = cached_access_token(account, TokenPurpose::Mail, || async {
            Ok(tokens("new", Some(3600)))
        })
        .await
        .unwrap();
        assert_eq!(fresh, "new");
        let other = cached_access_token(account, TokenPurpose::Extension("other"), || async {
            Ok(tokens("other", None))
        })
        .await
        .unwrap();
        assert_eq!(other, "other", "purposes are cached separately");
    }
}
