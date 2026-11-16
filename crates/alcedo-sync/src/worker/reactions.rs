//! Reaction detection and persistence for synced messages.
#![allow(clippy::too_many_arguments)]

use super::bodies::recover_html_body;
use super::*;

pub(super) async fn peek_reaction_bodies(
    client: &mut ImapClient,
    account: &Account,
    folder: &Folder,
    fetched: &[FetchedEnvelope],
    existing_uids: &HashSet<u32>,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
) -> Result<()> {
    let candidates: Vec<&FetchedEnvelope> = fetched
        .iter()
        .filter(|item| item.reaction_candidate)
        .collect();
    if candidates.is_empty() {
        return Ok(());
    }
    let uids: Vec<u32> = candidates.iter().map(|item| item.uid).collect();
    let bodies = with_fetch_timeout(uid_fetch_full_messages(client, &uids)).await?;
    let rows = store.messages_by_uids(folder.id, &uids).await?;
    let id_by_uid: HashMap<u32, MessageId> =
        rows.into_iter().map(|(id, uid, _)| (uid, id)).collect();
    let fetched_by_uid: HashMap<u32, &FetchedEnvelope> =
        candidates.iter().map(|item| (item.uid, *item)).collect();

    for (uid, full) in bodies {
        let Some(reaction) = full.reaction.as_ref() else {
            continue;
        };
        let Some(message_id) = id_by_uid.get(&uid) else {
            continue;
        };
        let Some(envelope) = fetched_by_uid.get(&uid) else {
            continue;
        };
        let is_new = !existing_uids.contains(&uid);
        let from_email = envelope
            .from
            .first()
            .map(|addr| addr.email.as_str())
            .unwrap_or("");
        let from_name = envelope
            .from
            .first()
            .map(|addr| addr.display_name())
            .unwrap_or_default();
        persist_reaction_columns(
            account,
            folder,
            *message_id,
            reaction,
            envelope.seen,
            &from_name,
            from_email,
            is_new,
            store,
            events,
        )
        .await?;
    }
    Ok(())
}

pub(super) async fn persist_reaction_columns(
    account: &Account,
    folder: &Folder,
    reaction_id: MessageId,
    reaction: &alcedo_core::EmailReaction,
    seen: bool,
    from_name: &str,
    from_email: &str,
    notify_on_insert: bool,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
) -> Result<()> {
    store
        .save_reaction(
            reaction_id,
            Some(&reaction.emoji),
            Some(&reaction.target_message_id),
        )
        .await?;
    emit_reaction_original(store, reaction_id, events).await?;

    if let Some((target_id, subject)) = store
        .reaction_target_message(account.id, &reaction.target_message_id)
        .await?
    {
        let notify = notify_on_insert
            && folder.special == SpecialUse::Inbox
            && !seen
            && !from_email.eq_ignore_ascii_case(&account.email);
        let _ = events.send(SyncEvent::ReactionReceived {
            reaction_id,
            target_id: Some(target_id),
            emoji: reaction.emoji.clone(),
            from_name: from_name.to_string(),
            subject,
            notify,
        });
    } else {
        let _ = events.send(SyncEvent::ReactionReceived {
            reaction_id,
            target_id: None,
            emoji: reaction.emoji.clone(),
            from_name: from_name.to_string(),
            subject: String::new(),
            notify: false,
        });
    }
    Ok(())
}

pub(super) async fn maybe_persist_reaction_from_full(
    account: &Account,
    folder: &Folder,
    id: MessageId,
    seen: bool,
    from_name: &str,
    from_email: &str,
    full: &alcedo_imap::FullMessage,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
) -> Result<()> {
    if let Some(reaction) = full.reaction.as_ref() {
        persist_reaction_columns(
            account, folder, id, reaction, seen, from_name, from_email, false, store, events,
        )
        .await?;
    }
    Ok(())
}

/// Tell the reader which message this reaction targeted, when that row is stored.
pub(crate) async fn emit_reaction_original(
    store: &StoreHandle,
    reaction_id: MessageId,
    events: &flume::Sender<SyncEvent>,
) -> Result<()> {
    let Some(hdr) = store.reaction_target_hdr(reaction_id).await? else {
        return Ok(());
    };
    let Some(hdr) = alcedo_core::bare_reaction_target_hdr(&hdr) else {
        return Ok(());
    };
    let _ = events.send(SyncEvent::ReactionReceived {
        reaction_id,
        target_id: None,
        emoji: String::new(),
        from_name: String::new(),
        subject: String::new(),
        notify: false,
    });
    let Some(reaction) = store.get_message(reaction_id).await? else {
        return Ok(());
    };
    let Some((target_id, subject)) = store
        .reaction_target_message(reaction.account_id, &hdr)
        .await?
    else {
        return Ok(());
    };
    let Some(target) = store.get_message(target_id).await? else {
        return Ok(());
    };
    let text = target.body_text.clone().unwrap_or_default();
    let html = recover_html_body(target.body_html.as_deref().unwrap_or(""), &text);
    let _ = events.send(SyncEvent::ReactionOriginalReady {
        reaction_id,
        target_id,
        subject,
        text,
        html,
    });
    Ok(())
}
