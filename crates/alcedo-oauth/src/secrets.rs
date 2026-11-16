//! oo7 keyring integration (P5).

use std::collections::HashMap;
use std::time::{Duration, Instant};

use oo7::{Item, Keyring};
use tokio::sync::Mutex;
use tracing::debug;

tokio::task_local! {
    /// Background sign-in retries must not raise another keyring dialog.
    static KEYRING_QUIET: bool;
}

use alcedo_core::{Error, Result};

/// Secret Service / portal application identifier.
pub const APPLICATION: &str = "dev.alcedo.Mail";

/// Serialize unlock prompts so parallel account workers don't stack dialogs.
static UNLOCK_GATE: Mutex<()> = Mutex::const_new(());
/// After a dismissed unlock prompt, skip prompting again for a short time.
static DISMISSED_UNTIL: Mutex<Option<Instant>> = Mutex::const_new(None);
const DISMISS_COOLDOWN: Duration = Duration::from_secs(30);

/// Kind of credential stored in the keyring.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SecretKind {
    RefreshToken,
    AccessToken,
    Password,
    SmtpPassword,
    PgpSecretKey,
    PgpPassphrase,
}

impl SecretKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::RefreshToken => "refresh_token",
            Self::AccessToken => "access_token",
            Self::Password => "password",
            Self::SmtpPassword => "smtp_password",
            Self::PgpSecretKey => "pgp_secret_key",
            Self::PgpPassphrase => "pgp_passphrase",
        }
    }
}

fn label_for(email: &str) -> String {
    format!("Alcedo: {email}")
}

fn attributes(owner_id: uuid::Uuid, kind: SecretKind) -> HashMap<String, String> {
    HashMap::from([
        ("application".to_string(), APPLICATION.to_string()),
        ("account_id".to_string(), owner_id.to_string()),
        ("kind".to_string(), kind.as_str().to_string()),
    ])
}

async fn keyring() -> Result<Keyring> {
    let ring = Keyring::new()
        .await
        .map_err(|e| map_keyring_error("unavailable", e))?;
    ensure_unlocked(&ring).await?;
    Ok(ring)
}

/// Run a secret read without prompting to unlock the keyring.
///
/// Used when a previous sign-in already failed because the keyring was locked.
/// The desktop may unlock it later; those retries only succeed if it is already open.
pub async fn with_quiet_keyring<T>(fut: impl std::future::Future<Output = T>) -> T {
    KEYRING_QUIET.scope(true, fut).await
}

fn keyring_is_quiet() -> bool {
    KEYRING_QUIET.try_with(|quiet| *quiet).unwrap_or(false)
}

/// Unlock the default collection after login/reboot before reading secrets.
async fn ensure_unlocked(ring: &Keyring) -> Result<()> {
    let _gate = UNLOCK_GATE.lock().await;
    if !is_locked(ring.is_locked().await) {
        return Ok(());
    }

    if keyring_is_quiet() {
        return Err(Error::Auth(locked_message().into()));
    }

    if dismiss_cooldown_active().await {
        return Err(Error::Auth(locked_message().into()));
    }

    debug!("unlocking system keyring");
    match ring.unlock().await {
        Ok(()) => Ok(()),
        Err(err) if is_dismissed_err(&err) => {
            *DISMISSED_UNTIL.lock().await = Some(Instant::now() + DISMISS_COOLDOWN);
            Err(Error::Auth("keyring unlock was cancelled".into()))
        }
        Err(err) => Err(map_keyring_error("unlock", err)),
    }
}

async fn dismiss_cooldown_active() -> bool {
    DISMISSED_UNTIL
        .lock()
        .await
        .is_some_and(|until| Instant::now() < until)
}

fn is_locked(result: oo7::Result<bool>) -> bool {
    result.unwrap_or(true)
}

/// Store a secret for an account (replaces any existing item with the same attributes).
pub async fn store_secret(
    owner_id: uuid::Uuid,
    email: &str,
    kind: SecretKind,
    value: &str,
) -> Result<()> {
    let attrs = attributes(owner_id, kind);
    let ring = keyring().await?;
    ring.delete(&attrs)
        .await
        .map_err(|e| map_keyring_error("delete", e))?;
    ring.create_item(&label_for(email), &attrs, value.as_bytes(), true)
        .await
        .map_err(|e| map_keyring_error("store", e))?;
    Ok(())
}

/// Retrieve a stored secret, if present.
pub async fn get_secret(owner_id: uuid::Uuid, kind: SecretKind) -> Result<Option<String>> {
    let attrs = attributes(owner_id, kind);
    let ring = keyring().await?;
    let items = ring
        .search_items(&attrs)
        .await
        .map_err(|e| map_keyring_error("search", e))?;
    let Some(item) = items.first() else {
        return Ok(None);
    };
    let secret = read_item_secret(item).await?;
    Ok(Some(
        String::from_utf8(secret.as_bytes().to_vec())
            .map_err(|e| Error::Auth(format!("keyring secret is not UTF-8: {e}")))?,
    ))
}

async fn read_item_secret(item: &Item) -> Result<oo7::Secret> {
    if is_locked(item.is_locked().await) {
        if keyring_is_quiet() {
            return Err(Error::Auth(locked_message().into()));
        }
        debug!("unlocking keyring item");
        item.unlock()
            .await
            .map_err(|e| map_keyring_error("unlock", e))?;
    }
    match item.secret().await {
        Ok(secret) => Ok(secret),
        Err(err) if is_locked_err(&err) => {
            if keyring_is_quiet() {
                return Err(Error::Auth(locked_message().into()));
            }
            debug!("keyring item still locked; unlocking and retrying");
            item.unlock()
                .await
                .map_err(|e| map_keyring_error("unlock", e))?;
            item.secret()
                .await
                .map_err(|e| map_keyring_error("read", e))
        }
        Err(err) => Err(map_keyring_error("read", err)),
    }
}

fn app_attributes(kind: &str) -> HashMap<String, String> {
    HashMap::from([
        ("application".into(), APPLICATION.into()),
        ("scope".into(), "app".into()),
        ("kind".into(), kind.into()),
    ])
}

/// Store an application-scoped secret (replaces any existing item with the same attributes).
pub async fn store_app_secret(kind: &str, label: &str, value: &str) -> Result<()> {
    let attrs = app_attributes(kind);
    let ring = keyring().await?;
    ring.delete(&attrs)
        .await
        .map_err(|e| map_keyring_error("delete", e))?;
    ring.create_item(label, &attrs, value.as_bytes(), true)
        .await
        .map_err(|e| map_keyring_error("store", e))?;
    Ok(())
}

/// Retrieve an application-scoped secret, if present.
pub async fn get_app_secret(kind: &str) -> Result<Option<String>> {
    let attrs = app_attributes(kind);
    let ring = keyring().await?;
    let items = ring
        .search_items(&attrs)
        .await
        .map_err(|e| map_keyring_error("search", e))?;
    let Some(item) = items.first() else {
        return Ok(None);
    };
    let secret = read_item_secret(item).await?;
    let value = String::from_utf8(secret.as_bytes().to_vec())
        .map_err(|e| Error::Auth(format!("keyring secret is not UTF-8: {e}")))?;
    if value.is_empty() {
        return Ok(None);
    }
    Ok(Some(value))
}

/// Delete an application-scoped secret.
pub async fn delete_app_secret(kind: &str) -> Result<()> {
    let attrs = app_attributes(kind);
    let ring = keyring().await?;
    ring.delete(&attrs)
        .await
        .map_err(|e| map_keyring_error("delete", e))?;
    Ok(())
}

/// Point existing keyring items for this account at the current mailbox address.
///
/// Lookup is by account id. The label is only what the keyring shows.
pub async fn relabel_account_secrets(owner_id: uuid::Uuid, email: &str) -> Result<()> {
    let label = label_for(email);
    let ring = keyring().await?;
    for kind in [
        SecretKind::RefreshToken,
        SecretKind::AccessToken,
        SecretKind::Password,
        SecretKind::SmtpPassword,
        SecretKind::PgpSecretKey,
        SecretKind::PgpPassphrase,
    ] {
        let items = ring
            .search_items(&attributes(owner_id, kind))
            .await
            .map_err(|e| map_keyring_error("search", e))?;
        for item in items {
            item.set_label(&label)
                .await
                .map_err(|e| map_keyring_error("relabel", e))?;
        }
    }
    Ok(())
}

/// Delete a stored secret for an account.
pub async fn delete_secret(owner_id: uuid::Uuid, kind: SecretKind) -> Result<()> {
    let attrs = attributes(owner_id, kind);
    let ring = keyring().await?;
    ring.delete(&attrs)
        .await
        .map_err(|e| map_keyring_error("delete", e))?;
    Ok(())
}

fn map_keyring_error(op: &str, err: oo7::Error) -> Error {
    Error::Auth(keyring_error_message(op, &err))
}

fn keyring_error_message(op: &str, err: &oo7::Error) -> String {
    if is_locked_err(err) {
        locked_message().into()
    } else if is_dismissed_err(err) {
        "keyring unlock was cancelled".into()
    } else {
        format!("keyring {op} failed: {err}")
    }
}

fn locked_message() -> &'static str {
    "system keyring is locked; unlock it and try again"
}

fn is_locked_err(err: &oo7::Error) -> bool {
    matches!(
        err,
        oo7::Error::DBus(oo7::dbus::Error::Service(
            oo7::dbus::ServiceError::IsLocked(_)
        )) | oo7::Error::File(oo7::file::Error::Locked)
    )
}

fn is_dismissed_err(err: &oo7::Error) -> bool {
    matches!(err, oo7::Error::DBus(oo7::dbus::Error::Dismissed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn quiet_scope_suppresses_unlock_prompts_only_inside_the_future() {
        assert!(!keyring_is_quiet());
        with_quiet_keyring(async {
            assert!(keyring_is_quiet());
        })
        .await;
        assert!(!keyring_is_quiet());
    }

    #[test]
    fn pgp_secret_kinds_as_str() {
        assert_eq!(SecretKind::PgpSecretKey.as_str(), "pgp_secret_key");
        assert_eq!(SecretKind::PgpPassphrase.as_str(), "pgp_passphrase");
        assert_eq!(SecretKind::SmtpPassword.as_str(), "smtp_password");
    }

    #[test]
    fn locked_file_backend_maps_to_friendly_auth_error() {
        let err = oo7::Error::File(oo7::file::Error::Locked);
        assert_eq!(
            keyring_error_message("read", &err),
            "system keyring is locked; unlock it and try again"
        );
    }

    #[test]
    fn dismissed_prompt_maps_to_friendly_auth_error() {
        let err = oo7::Error::DBus(oo7::dbus::Error::Dismissed);
        assert_eq!(
            keyring_error_message("unlock", &err),
            "keyring unlock was cancelled"
        );
    }

    #[test]
    fn app_attributes_include_scope_and_kind() {
        let attrs = app_attributes("example_key");
        assert_eq!(
            attrs.get("application").map(String::as_str),
            Some(APPLICATION)
        );
        assert_eq!(attrs.get("scope").map(String::as_str), Some("app"));
        assert_eq!(attrs.get("kind").map(String::as_str), Some("example_key"));
    }

    #[test]
    fn other_keyring_errors_keep_operation_context() {
        let err = oo7::Error::File(oo7::file::Error::NoData);
        let message = keyring_error_message("read", &err);
        assert!(message.starts_with("keyring read failed:"), "{message}");
        assert!(
            message.contains("NoData") || message.contains("data"),
            "{message}"
        );
    }
}
