//! Keeps the local rows of one server message in step across folders.
//!
//! Gmail exposes a message under each of its labels (Inbox, All Mail, ...) and
//! keeps `\Seen` and `\Flagged` per message, so a change made through one folder
//! applies to all of them. The store holds one row per folder, and the other
//! rows would otherwise show stale flags until their own folder syncs.

use alcedo_imap::ImapServerCaps;
use alcedo_store::{FlagMirrorSource, MirroredCopy, MirroredFlags};

use super::mailbox_sync::persist_folder_unread;
use super::*;

/// Whether a server shares flags between the folders that show a message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FlagScope {
    PerFolder,
    SharedAcrossLabels,
}

impl FlagScope {
    pub(super) fn of(capabilities: &ImapServerCaps) -> Self {
        if capabilities.gmail_ext {
            Self::SharedAcrossLabels
        } else {
            Self::PerFolder
        }
    }
}

/// Copy flags onto the source rows' copies in other folders and publish the
/// result. Returns the changed copies so a rejected server write can undo them.
///
/// Best effort: a stale copy is corrected by its folder's next sync, so a local
/// failure is logged rather than failing the flag change itself.
pub(super) async fn mirror_flags(
    scope: FlagScope,
    account: &Account,
    source: FlagMirrorSource,
    flags: MirroredFlags,
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
) -> Vec<MirroredCopy> {
    if scope == FlagScope::PerFolder {
        return Vec::new();
    }
    let mirrored = match store.mirror_flags_to_copies(source, flags).await {
        Ok(mirrored) => mirrored,
        Err(err) => {
            warn!(account = %account.email, error = %err, "could not mirror flags onto message copies");
            return Vec::new();
        }
    };
    publish_copy_changes(account, &mirrored, store, events).await;
    mirrored
}

/// Undo a mirror after the server write it anticipated failed.
///
/// Restoring is the safe direction when the outcome is unknown: a write that did
/// land raises the message's modseq, so the next sync brings the new value back.
pub(super) async fn restore_mirrored_flags(
    account: &Account,
    mirrored: &[MirroredCopy],
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
) {
    if mirrored.is_empty() {
        return;
    }
    if let Err(err) = store.restore_mirrored_copies(mirrored).await {
        warn!(account = %account.email, error = %err, "could not restore mirrored message flags");
        return;
    }
    publish_copy_changes(account, mirrored, store, events).await;
    if let Err(err) = emit_unread_counts(store, events).await {
        warn!(account = %account.email, error = %err, "could not emit unread counts after restoring flags");
    }
}

/// Send the changed rows to the UI and refresh each affected folder's count. A
/// failure in one folder does not stop the others.
async fn publish_copy_changes(
    account: &Account,
    copies: &[MirroredCopy],
    store: &StoreHandle,
    events: &flume::Sender<SyncEvent>,
) {
    if copies.is_empty() {
        return;
    }
    let ids: Vec<MessageId> = copies.iter().map(|copy| copy.id).collect();
    match store.envelopes_by_ids(&ids).await {
        Ok(envelopes) => {
            for envelope in envelopes {
                let _ = events.send(SyncEvent::EnvelopeUpdated(envelope));
            }
        }
        Err(err) => {
            warn!(account = %account.email, error = %err, "could not load mirrored message rows");
        }
    }
    let folders: HashSet<FolderId> = copies.iter().map(|copy| copy.folder_id).collect();
    for folder_id in folders {
        if let Err(err) = persist_folder_unread(account, folder_id, store, events).await {
            warn!(account = %account.email, ?folder_id, error = %err, "could not refresh folder unread count");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worker::test_support::{CopyFixture, copy_envelope};

    fn updated_ids(rx: &flume::Receiver<SyncEvent>) -> Vec<MessageId> {
        rx.try_iter()
            .filter_map(|event| match event {
                SyncEvent::EnvelopeUpdated(envelope) => Some(envelope.id),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn only_gmail_capability_shares_flags_across_folders() {
        let gmail = ImapServerCaps {
            gmail_ext: true,
            ..ImapServerCaps::default()
        };
        assert_eq!(FlagScope::of(&gmail), FlagScope::SharedAcrossLabels);
        assert_eq!(
            FlagScope::of(&ImapServerCaps::default()),
            FlagScope::PerFolder
        );
    }

    #[tokio::test]
    async fn copy_outside_the_newest_page_still_reaches_the_ui() {
        let fx = CopyFixture::new().await;
        let newer: Vec<_> = (100..250)
            .map(|uid| copy_envelope(&fx.account, fx.all_mail, uid, -i64::from(uid)))
            .collect();
        fx.store
            .upsert_envelopes(&newer, None)
            .await
            .expect("newer");
        let (tx, rx) = flume::unbounded::<SyncEvent>();
        fx.store
            .set_flags(fx.original, Some(true), None)
            .await
            .expect("read");

        mirror_flags(
            FlagScope::SharedAcrossLabels,
            &fx.account,
            FlagMirrorSource::Messages(vec![fx.original]),
            MirroredFlags::SEEN,
            &fx.store,
            &tx,
        )
        .await;

        assert_eq!(updated_ids(&rx), vec![fx.copy]);
    }

    #[tokio::test]
    async fn restore_returns_the_copy_to_unread_and_recounts() {
        let fx = CopyFixture::new().await;
        let (tx, rx) = flume::unbounded::<SyncEvent>();
        fx.store
            .set_flags(fx.original, Some(true), None)
            .await
            .expect("read");
        let mirrored = mirror_flags(
            FlagScope::SharedAcrossLabels,
            &fx.account,
            FlagMirrorSource::Messages(vec![fx.original]),
            MirroredFlags::SEEN,
            &fx.store,
            &tx,
        )
        .await;
        assert!(fx.is_seen(fx.copy).await);

        restore_mirrored_flags(&fx.account, &mirrored, &fx.store, &tx).await;

        assert!(!fx.is_seen(fx.copy).await);
        assert_eq!(fx.stored_unread(fx.all_mail).await, 2);
        assert!(
            rx.try_iter()
                .any(|event| matches!(event, SyncEvent::UnreadCounts { .. }))
        );
    }
}
