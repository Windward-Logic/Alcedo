//! Incoming OpenPGP processing and PGP/MIME sending.

use super::*;

pub(super) async fn send_pgp_compose(
    account: &Account,
    draft: &ComposeDraft,
    bodies: &RenderedBodies,
    store: &StoreHandle,
    opts: SmtpOptions,
) -> Result<(String, Vec<u8>)> {
    let inner = serialize_inner_body(draft, bodies)?;
    let (secret, passphrase) = crate::pgp::load_secret_and_passphrase(account.id).await?;
    let mut recipients = draft.to.clone();
    recipients.extend(draft.cc.clone());
    recipients.extend(draft.bcc.clone());
    let peer_keys = store.pgp_keys_for_emails(&recipients).await?;
    let own_keys = store.list_pgp_keys(account.id).await?;
    let mut encrypt_to: Vec<String> = Vec::new();
    if draft.encrypt {
        for token in &recipients {
            let email = email_from_address_token(token)
                .ok_or_else(|| Error::Pgp(format!("invalid recipient address: {token}")))?;
            let armored = peer_keys
                .iter()
                .find(|peer| peer.email.eq_ignore_ascii_case(&email))
                .and_then(|peer| peer.armored.clone())
                .ok_or_else(|| Error::Pgp(format!("no public key for {email}")))?;
            if !encrypt_to.iter().any(|key| key == &armored) {
                encrypt_to.push(armored);
            }
        }
        let own_armored = own_keys
            .iter()
            .find(|key| key.source == PgpKeySource::Own)
            .and_then(|key| key.armored.clone())
            .ok_or_else(|| Error::Pgp("no own public key for encrypt-to-self".into()))?;
        if !encrypt_to.iter().any(|key| key == &own_armored) {
            encrypt_to.push(own_armored);
        }
    }
    let sign = if draft.sign {
        Some(SignKey {
            secret_armored: secret,
            passphrase,
        })
    } else {
        None
    };
    let protected = tokio::task::spawn_blocking(move || {
        Pgp::protect_outbound(ProtectRequest {
            inner_mime: inner,
            sign,
            encrypt_to,
        })
    })
    .await
    .map_err(|e| Error::Pgp(e.to_string()))??;
    let parts = protected.body_parts;
    let pgp_body = PgpMimeBody {
        encrypted: protected.mime_kind == PgpMimeKind::Encrypted,
        signed_part: Some(parts.inner_mime.clone()),
        signature: parts.signature,
        payload: parts.encrypted_payload.unwrap_or(parts.inner_mime),
    };
    send_pgp_message(opts, draft, &pgp_body).await
}

/// What incoming OpenPGP processing produced for one message.
pub(super) struct IncomingPgp {
    pub(super) text: String,
    pub(super) html: String,
    pub(super) status: PgpStatus,
    pub(super) snippet_override: Option<String>,
    /// False when the verdict reflects a missing or broken OpenPGP library
    /// rather than the message or the account, so it must not be stored.
    pub(super) persist: bool,
}

/// A verdict is only worth storing when the backend was able to produce a
/// real one; otherwise the next open must verify again.
fn verdict_is_persistable(backend_available: Result<()>) -> bool {
    backend_available.is_ok()
}

pub(super) async fn process_incoming_pgp(
    account_id: AccountId,
    raw: &[u8],
    store: &StoreHandle,
) -> Result<Option<IncomingPgp>> {
    let kind = inspect_incoming(raw);
    if kind == IncomingKind::None {
        return Ok(None);
    }
    // The first check may load the library, so keep it off the async threads.
    let backend_available = tokio::task::spawn_blocking(Pgp::ensure_available)
        .await
        .unwrap_or_else(|e| Err(Error::Pgp(e.to_string())));
    let backend_persistable = verdict_is_persistable(backend_available);
    let public_armoreds: Vec<String> = store
        .list_pgp_keys(account_id)
        .await?
        .into_iter()
        .filter_map(|peer| peer.armored)
        .collect();
    let raw_vec = raw.to_vec();
    match kind {
        IncomingKind::EncryptedMime | IncomingKind::InlineEncrypted => {
            let failed = PgpStatus {
                encrypted: true,
                decrypted: false,
                signature: alcedo_core::PgpSignatureState::None,
            };
            let (secret, passphrase) =
                match crate::pgp::load_secret_and_passphrase(account_id).await {
                    Ok(pair) => pair,
                    Err(_) => {
                        return Ok(Some(IncomingPgp {
                            text: String::new(),
                            html: String::new(),
                            status: failed,
                            snippet_override: Some(alcedo_core::ENCRYPTED_SNIPPET.into()),
                            persist: true,
                        }));
                    }
                };
            let extra = public_armoreds.clone();
            match tokio::task::spawn_blocking(move || {
                Pgp::decrypt(&raw_vec, &secret, &passphrase, &extra)
            })
            .await
            {
                Ok(Ok(decrypted)) => {
                    let parsed = MessageParser::default().parse(&decrypted.inner_rfc822);
                    let text = parsed
                        .as_ref()
                        .and_then(|m| m.body_text(0))
                        .unwrap_or_default()
                        .to_string();
                    let html = parsed
                        .as_ref()
                        .and_then(|m| m.body_html(0))
                        .unwrap_or_default()
                        .to_string();
                    Ok(Some(IncomingPgp {
                        text,
                        html,
                        status: decrypted.status,
                        snippet_override: None,
                        persist: backend_persistable,
                    }))
                }
                _ => Ok(Some(IncomingPgp {
                    text: String::new(),
                    html: String::new(),
                    status: failed,
                    snippet_override: Some(alcedo_core::ENCRYPTED_SNIPPET.into()),
                    persist: backend_persistable,
                })),
            }
        }
        IncomingKind::SignedMime | IncomingKind::SignedCleartext => {
            let pubs = public_armoreds;
            let raw_for_verify = raw_vec.clone();
            let status = tokio::task::spawn_blocking(move || Pgp::verify(&raw_for_verify, &pubs))
                .await
                .map_err(|e| Error::Pgp(e.to_string()))?;
            let parsed = MessageParser::default().parse(&raw_vec);
            let text = parsed
                .as_ref()
                .and_then(|m| m.body_text(0))
                .unwrap_or_default()
                .to_string();
            let html = parsed
                .as_ref()
                .and_then(|m| m.body_html(0))
                .unwrap_or_default()
                .to_string();
            Ok(Some(IncomingPgp {
                text,
                html,
                status,
                snippet_override: None,
                persist: backend_persistable,
            }))
        }
        IncomingKind::None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn available_backend_verdict_is_persisted() {
        assert!(verdict_is_persistable(Ok(())));
    }

    #[test]
    fn unavailable_backend_verdict_is_not_persisted() {
        let unavailable = Error::Pgp("OpenPGP library unavailable: x".into());
        assert!(!verdict_is_persistable(Err(unavailable)));
    }
}
