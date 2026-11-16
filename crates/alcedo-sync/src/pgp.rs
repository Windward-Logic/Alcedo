//! OpenPGP key lifecycle and account state (Tokio; crypto on `spawn_blocking`).

use alcedo_core::{AccountId, Error, Result, SyncEvent};
use alcedo_core::{PgpAccountState, PgpKeySource};
use alcedo_oauth::{SecretKind, delete_secret, get_secret, store_secret};
use alcedo_pgp::{generate, parse_public, parse_secret};
use alcedo_store::StoreHandle;
use tracing::warn;

pub async fn emit_pgp_state(
    store: &StoreHandle,
    account_id: AccountId,
    events: &flume::Sender<SyncEvent>,
) {
    match load_pgp_state(store, account_id).await {
        Ok(state) => {
            let _ = events.send(SyncEvent::PgpState(state));
        }
        Err(err) => {
            warn!(?account_id, error = %err, "failed to load pgp state");
        }
    }
}

pub async fn load_pgp_state(store: &StoreHandle, account_id: AccountId) -> Result<PgpAccountState> {
    let sign_outgoing = store.get_pgp_sign_outgoing(account_id).await?;
    let peers = store.list_pgp_keys(account_id).await?;
    let own = peers
        .iter()
        .find(|peer| peer.source == PgpKeySource::Own)
        .cloned();
    Ok(PgpAccountState {
        account_id,
        sign_outgoing,
        own,
        peers,
    })
}

pub async fn pgp_generate(
    account_id: AccountId,
    email: &str,
    display_name: &str,
    passphrase: String,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
) -> Result<()> {
    let email_owned = email.to_string();
    let name_owned = display_name.to_string();
    let pass_for_crypto = passphrase.clone();
    let generated =
        tokio::task::spawn_blocking(move || generate(&name_owned, &email_owned, &pass_for_crypto))
            .await
            .map_err(|e| Error::Pgp(e.to_string()))??;

    store_secret(
        account_id.0,
        email,
        SecretKind::PgpSecretKey,
        &generated.secret_armored,
    )
    .await?;
    store_secret(
        account_id.0,
        email,
        SecretKind::PgpPassphrase,
        passphrase.as_str(),
    )
    .await?;

    store.delete_own_pgp_key(account_id).await?;
    let emails = parse_public(&generated.public_armored)
        .map(|s| s.emails)
        .unwrap_or_else(|_| vec![email.to_string()]);
    store
        .upsert_pgp_key(
            &generated.fingerprint,
            &emails,
            &generated.public_armored,
            PgpKeySource::Own,
            Some(account_id),
        )
        .await?;
    emit_pgp_state(store, account_id, events).await;
    Ok(())
}

pub async fn pgp_import(
    account_id: AccountId,
    email: &str,
    armored: String,
    passphrase: Option<String>,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
) -> Result<()> {
    let pass = passphrase.clone();
    let generated = tokio::task::spawn_blocking(move || parse_secret(&armored, pass.as_deref()))
        .await
        .map_err(|e| Error::Pgp(e.to_string()))??;

    store_secret(
        account_id.0,
        email,
        SecretKind::PgpSecretKey,
        &generated.secret_armored,
    )
    .await?;
    if let Some(pass) = passphrase.filter(|p| !p.is_empty()) {
        store_secret(account_id.0, email, SecretKind::PgpPassphrase, &pass).await?;
    } else {
        let _ = delete_secret(account_id.0, SecretKind::PgpPassphrase).await;
    }

    store.delete_own_pgp_key(account_id).await?;
    let emails = parse_public(&generated.public_armored)
        .map(|s| s.emails)
        .unwrap_or_else(|_| vec![email.to_string()]);
    store
        .upsert_pgp_key(
            &generated.fingerprint,
            &emails,
            &generated.public_armored,
            PgpKeySource::Own,
            Some(account_id),
        )
        .await?;
    emit_pgp_state(store, account_id, events).await;
    Ok(())
}

pub async fn pgp_export_public(
    account_id: AccountId,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
) -> Result<()> {
    let state = load_pgp_state(store, account_id).await?;
    let own = state
        .own
        .ok_or_else(|| Error::Pgp("no key for account".into()))?;
    let armored = own
        .armored
        .ok_or_else(|| Error::Pgp("missing public key material".into()))?;
    let _ = events.send(SyncEvent::PgpPublicKey {
        account: account_id,
        armored,
        fingerprint: own.fingerprint,
    });
    Ok(())
}

pub async fn pgp_set_sign_outgoing(
    account_id: AccountId,
    enabled: bool,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
) -> Result<()> {
    store.set_pgp_sign_outgoing(account_id, enabled).await?;
    emit_pgp_state(store, account_id, events).await;
    Ok(())
}

pub async fn pgp_delete_key(
    account_id: AccountId,
    email: &str,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
) -> Result<()> {
    store.delete_own_pgp_key(account_id).await?;
    store.set_pgp_sign_outgoing(account_id, false).await?;
    let _ = delete_secret(account_id.0, SecretKind::PgpSecretKey).await;
    let _ = delete_secret(account_id.0, SecretKind::PgpPassphrase).await;
    let _ = email;
    emit_pgp_state(store, account_id, events).await;
    Ok(())
}

pub async fn pgp_accept_peer(
    account_id: AccountId,
    email: String,
    armored: String,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
) -> Result<()> {
    let armored_for_parse = armored.clone();
    let summary = tokio::task::spawn_blocking(move || parse_public(&armored_for_parse))
        .await
        .map_err(|e| Error::Pgp(e.to_string()))??;
    if !summary
        .emails
        .iter()
        .any(|e| e.eq_ignore_ascii_case(&email))
    {
        return Err(Error::Pgp("autocrypt key does not list that email".into()));
    }
    store
        .upsert_pgp_key(
            &summary.fingerprint,
            &summary.emails,
            &armored,
            PgpKeySource::Autocrypt,
            Some(account_id),
        )
        .await?;
    emit_pgp_state(store, account_id, events).await;
    Ok(())
}

pub async fn load_secret_and_passphrase(account_id: AccountId) -> Result<(String, String)> {
    let secret = get_secret(account_id.0, SecretKind::PgpSecretKey)
        .await?
        .ok_or_else(|| Error::Pgp("no secret key in keyring".into()))?;
    let passphrase = get_secret(account_id.0, SecretKind::PgpPassphrase)
        .await?
        .unwrap_or_default();
    Ok((secret, passphrase))
}

/// Attach an outbound Autocrypt header when the account has an own public key.
pub async fn attach_outbound_autocrypt(
    account_id: AccountId,
    email: &str,
    draft: &mut alcedo_core::ComposeDraft,
    store: &StoreHandle,
) -> Result<()> {
    let state = load_pgp_state(store, account_id).await?;
    let Some(own) = state.own else {
        return Ok(());
    };
    let Some(public_armored) = own.armored else {
        return Ok(());
    };
    let prefer_mutual = state.sign_outgoing;
    let addr = email.to_string();
    let armored = public_armored;
    let header = tokio::task::spawn_blocking(move || {
        alcedo_pgp::autocrypt_header_value(&addr, &armored, prefer_mutual)
    })
    .await
    .map_err(|e| Error::Pgp(e.to_string()))??;
    draft.autocrypt_header = Some(header);
    Ok(())
}

/// Emit `PgpAutocryptOffer` when an incoming message advertises an unknown key.
pub async fn maybe_emit_autocrypt_offer(
    account_id: AccountId,
    message_id: alcedo_core::MessageId,
    raw: &[u8],
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
) {
    let raw_vec = raw.to_vec();
    let peer =
        match tokio::task::spawn_blocking(move || alcedo_pgp::autocrypt_peer_from_rfc822(&raw_vec))
            .await
        {
            Ok(Some(peer)) => peer,
            Ok(None) => return,
            Err(err) => {
                warn!(?account_id, error = %err, "autocrypt parse task failed");
                return;
            }
        };

    let keys = match store.list_pgp_keys(account_id).await {
        Ok(keys) => keys,
        Err(err) => {
            warn!(?account_id, error = %err, "failed to list pgp keys for autocrypt offer");
            return;
        }
    };
    if keys
        .iter()
        .any(|key| key.fingerprint.eq_ignore_ascii_case(&peer.fingerprint))
    {
        return;
    }

    let _ = events.send(SyncEvent::PgpAutocryptOffer {
        message_id,
        email: peer.email,
        fingerprint: peer.fingerprint,
        armored: peer.armored,
    });
}
