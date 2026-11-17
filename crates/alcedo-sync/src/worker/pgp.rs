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

pub(super) async fn process_incoming_pgp(
    account_id: AccountId,
    raw: &[u8],
    store: &StoreHandle,
) -> Result<Option<(String, String, PgpStatus, Option<String>)>> {
    let kind = inspect_incoming(raw);
    if kind == IncomingKind::None {
        return Ok(None);
    }
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
                        return Ok(Some((
                            String::new(),
                            String::new(),
                            failed,
                            Some("Encrypted message".into()),
                        )));
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
                    Ok(Some((text, html, decrypted.status, None)))
                }
                _ => Ok(Some((
                    String::new(),
                    String::new(),
                    failed,
                    Some("Encrypted message".into()),
                ))),
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
            Ok(Some((text, html, status, None)))
        }
        IncomingKind::None => Ok(None),
    }
}
