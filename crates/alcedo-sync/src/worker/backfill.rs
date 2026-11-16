//! Header backfills for already-synced rows: List-Unsubscribe, Message-ID, sizes, reply and routing headers.

use super::session::is_recoverable_disconnect;
use super::*;

pub(super) async fn persist_fetched_list_unsubscribe(
    store: &StoreHandle,
    folder_id: FolderId,
    fetched: &[FetchedEnvelope],
) -> Result<()> {
    let uids: Vec<u32> = fetched
        .iter()
        .filter(|f| f.list_unsubscribe.is_some())
        .map(|f| f.uid)
        .collect();
    if uids.is_empty() {
        return Ok(());
    }
    let rows = store.messages_by_uids(folder_id, &uids).await?;
    let id_by_uid: HashMap<u32, MessageId> =
        rows.into_iter().map(|(id, uid, _)| (uid, id)).collect();
    for fetched in fetched {
        let Some(list) = fetched.list_unsubscribe.as_ref() else {
            continue;
        };
        let Some(id) = id_by_uid.get(&fetched.uid) else {
            continue;
        };
        store.save_list_unsubscribe(*id, Some(list)).await?;
    }
    Ok(())
}

pub(super) async fn persist_fetched_message_ids(
    store: &StoreHandle,
    folder_id: FolderId,
    fetched: &[FetchedEnvelope],
) -> Result<()> {
    let rows: Vec<(u32, String, Option<String>, Option<String>)> = fetched
        .iter()
        .filter_map(|fetched| {
            let message_id = fetched.message_id.as_deref()?.trim();
            if message_id.is_empty() {
                None
            } else {
                Some((
                    fetched.uid,
                    message_id.to_string(),
                    fetched.in_reply_to.clone(),
                    fetched.references.clone(),
                ))
            }
        })
        .collect();
    if rows.is_empty() {
        return Ok(());
    }
    store.set_threading_hdrs(folder_id, &rows).await
}

/// How many unsized messages to measure on one folder sync.
pub(super) const RFC822_SIZE_BACKFILL_LIMIT: u32 = 1000;

pub(super) const REPLY_HEADER_BACKFILL_LIMIT: u32 = 1000;

pub(super) async fn persist_fetched_sizes(
    store: &StoreHandle,
    folder_id: FolderId,
    fetched: &[FetchedEnvelope],
) -> Result<()> {
    let rows: Vec<(u32, i64)> = fetched
        .iter()
        .filter(|fetched| fetched.size > 0)
        .map(|fetched| (fetched.uid, i64::from(fetched.size)))
        .collect();
    if rows.is_empty() {
        return Ok(());
    }
    store.set_rfc822_sizes(folder_id, &rows).await
}

pub(super) fn positive_rfc822_size_rows(fetched: &[(u32, u32)]) -> Vec<(u32, i64)> {
    fetched
        .iter()
        .filter(|(_, size)| *size > 0)
        .map(|(uid, size)| (*uid, i64::from(*size)))
        .collect()
}

pub(super) async fn backfill_missing_rfc822_sizes(
    client: &mut ImapClient,
    folder_id: FolderId,
    store: &StoreHandle,
) -> Result<()> {
    let uids = store
        .uids_missing_rfc822_size(folder_id, RFC822_SIZE_BACKFILL_LIMIT)
        .await?;
    if uids.is_empty() {
        return Ok(());
    }
    for chunk in uids.chunks(200) {
        let Some(uid_set) = format_uid_set_chunks(chunk, chunk.len()).into_iter().next() else {
            continue;
        };
        let fetched = match with_imap_timeout(
            IMAP_COMMAND_TIMEOUT,
            uid_fetch_rfc822_sizes(client, &uid_set),
        )
        .await
        {
            Ok(sizes) => sizes,
            Err(err) if is_recoverable_disconnect(&err) => return Err(err),
            Err(err) => {
                warn!(error = %err, "RFC822.SIZE backfill failed");
                return Ok(());
            }
        };
        let rows = positive_rfc822_size_rows(&fetched);
        if !rows.is_empty() {
            store.set_rfc822_sizes(folder_id, &rows).await?;
        }
    }
    Ok(())
}

pub(super) async fn backfill_missing_reply_headers(
    client: &mut ImapClient,
    folder_id: FolderId,
    store: &StoreHandle,
) -> Result<()> {
    let uids = store
        .uids_missing_reply_headers(folder_id, REPLY_HEADER_BACKFILL_LIMIT)
        .await?;
    if uids.is_empty() {
        return Ok(());
    }
    for chunk in uids.chunks(200) {
        let Some(uid_set) = format_uid_set_chunks(chunk, chunk.len()).into_iter().next() else {
            continue;
        };
        let fetched = match with_imap_timeout(
            IMAP_COMMAND_TIMEOUT,
            uid_fetch_reply_headers(client, &uid_set),
        )
        .await
        {
            Ok(headers) => headers,
            Err(err) if is_recoverable_disconnect(&err) => return Err(err),
            Err(err) => {
                warn!(error = %err, "Reply-To/Sender header backfill failed");
                return Ok(());
            }
        };
        let mut rows = Vec::new();
        for (uid, reply_to, sender) in fetched {
            let reply_json = serde_json::to_string(&reply_to)
                .map_err(|e| alcedo_core::Error::Store(format!("reply_to json: {e}")))?;
            let sender_json = serde_json::to_string(&sender)
                .map_err(|e| alcedo_core::Error::Store(format!("sender json: {e}")))?;
            rows.push((uid, reply_json, sender_json));
        }
        if !rows.is_empty() {
            store.set_reply_headers(folder_id, &rows).await?;
        }
    }
    Ok(())
}

/// Mail-Reply-To, Mail-Followup-To, List-*, Delivered-To and X-Original-To
/// for rows cached before those headers were stored.
pub(super) async fn backfill_missing_routing_headers(
    client: &mut ImapClient,
    folder_id: FolderId,
    store: &StoreHandle,
) -> Result<()> {
    let uids = store
        .uids_missing_routing_headers(folder_id, REPLY_HEADER_BACKFILL_LIMIT)
        .await?;
    for chunk in uids.chunks(200) {
        let Some(uid_set) = format_uid_set_chunks(chunk, chunk.len()).into_iter().next() else {
            continue;
        };
        let rows = match with_imap_timeout(
            IMAP_COMMAND_TIMEOUT,
            uid_fetch_routing_headers(client, &uid_set),
        )
        .await
        {
            Ok(rows) => rows,
            Err(err) if is_recoverable_disconnect(&err) => return Err(err),
            Err(err) => {
                warn!(error = %err, "routing header backfill failed");
                return Ok(());
            }
        };
        if !rows.is_empty() {
            store.set_routing_headers(folder_id, rows).await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc822_backfill_skips_missing_and_zero_sizes() {
        assert_eq!(
            positive_rfc822_size_rows(&[(1, 2000), (2, 0)]),
            vec![(1, 2000)]
        );
    }
}
