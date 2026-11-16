//! Applies SyncEvent updates from the sync runtime to the mail model.

use super::*;

mod accounts;
mod list;
mod reader;
mod send;

impl MailModel {
    pub(super) fn handle_sync_event(
        &mut self,
        event: SyncEvent,
        sender: &ComponentSender<MailModel>,
        root: &adw::ApplicationWindow,
    ) {
        self.observe_sync_for_welcome(&event);
        match event {
            SyncEvent::Extension(message) => {
                for extension in crate::extension::registered() {
                    extension.handle_event(&message, &self.ext_ctx);
                }
            }
            event @ (SyncEvent::EnvelopesReplaced { .. }
            | SyncEvent::ConversationsReplaced { .. }
            | SyncEvent::EnvelopesAppended { .. }
            | SyncEvent::EnvelopeRemoved { .. }
            | SyncEvent::FolderDeleteFinished { .. }
            | SyncEvent::EnvelopeUpdated(..)
            | SyncEvent::UnreadCounts { .. }
            | SyncEvent::MessageDeleteFailed { .. }) => {
                self.on_list_sync_event(event, sender, root)
            }
            event @ (SyncEvent::ReactionsLoaded { .. }
            | SyncEvent::ReactionReceived { .. }
            | SyncEvent::ReactionOriginalReady { .. }
            | SyncEvent::BodyReady { .. }
            | SyncEvent::UnsubscribeFinished { .. }
            | SyncEvent::RawMessageReady { .. }
            | SyncEvent::ThreadReady { .. }
            | SyncEvent::RemoteImageSendersChanged { .. }
            | SyncEvent::PgpAutocryptOffer { .. }) => self.on_reader_sync_event(event, root),
            event @ (SyncEvent::AccountAdded(..)
            | SyncEvent::FoldersUpdated { .. }
            | SyncEvent::AccountUpdated(..)
            | SyncEvent::AccountRemoved(..)
            | SyncEvent::AuthRequired { .. }
            | SyncEvent::Error { .. }
            | SyncEvent::Status { .. }
            | SyncEvent::ContactsChanged { .. }
            | SyncEvent::SentRecipientsChanged { .. }
            | SyncEvent::RulesUpdated { .. }
            | SyncEvent::PgpState(..)
            | SyncEvent::PgpPublicKey { .. }
            | SyncEvent::SignaturesUpdated { .. }
            | SyncEvent::IdentitiesUpdated { .. }
            | SyncEvent::SmtpEnvelopePolicy { .. }
            | SyncEvent::CatchAllDomainsUpdated { .. }
            | SyncEvent::SnippetsUpdated { .. }) => self.on_accounts_sync_event(event, sender),
            event @ (SyncEvent::SendProgress { .. }
            | SyncEvent::ImapDraftSaved { .. }
            | SyncEvent::ImapDraftSaveFailed { .. }
            | SyncEvent::ImapDraftReady { .. }
            | SyncEvent::QueuedSendsRecovered(..)) => self.on_send_sync_event(event, sender),
        }
        self.refresh_welcome_page();
    }
}
