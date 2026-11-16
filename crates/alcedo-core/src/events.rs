use serde::{Deserialize, Serialize};

use crate::account::{Account, Folder};
use crate::commands::ComposeDraft;
use crate::contact::{Contact, ContactGroup};
use crate::envelope::{Address, Conversation, Envelope};
use crate::extension::{ExtensionListId, ExtensionMessage};
use crate::identity::AccountIdentity;
use crate::ids::{AccountId, FolderId, MessageId, ThreadId};
use crate::list_unsubscribe::ListUnsubscribe;
use crate::pgp::PgpAccountState;
use crate::pgp::PgpStatus;
use crate::queued_send::QueuedSend;
use crate::reaction::ReactionPerson;
use crate::rules::{MessageRule, ServerRulesPreview, ServerRulesStatus};
use crate::snippet::{AccountSignature, Snippet};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum SyncEvent {
    AccountAdded(Account),
    AccountUpdated(Account),
    AccountRemoved(AccountId),
    FoldersUpdated {
        account: AccountId,
        folders: Vec<Folder>,
    },
    EnvelopesAppended {
        folder: FolderId,
        envelopes: Vec<Envelope>,
    },
    EnvelopesReplaced {
        envelopes: Vec<Envelope>,
        kind: EnvelopeListKind,
    },
    ConversationsReplaced {
        conversations: Vec<Conversation>,
        kind: EnvelopeListKind,
    },
    ThreadReady {
        thread_id: ThreadId,
        envelopes: Vec<Envelope>,
    },
    EnvelopeUpdated(Envelope),
    EnvelopeRemoved {
        id: MessageId,
    },
    MessageDeleteFailed {
        ids: Vec<MessageId>,
        #[serde(default)]
        reason: String,
    },
    UnreadCounts {
        account: AccountId,
        inbox: u32,
        total: u32,
    },
    BodyReady {
        id: MessageId,
        html: String,
        text: String,
        cids: Vec<CidInline>,
        attachments: Vec<BodyAttachment>,
        #[serde(default)]
        allow_remote_images: bool,
        #[serde(default)]
        list_unsubscribe: Option<ListUnsubscribe>,
        #[serde(default)]
        message_id_hdr: Option<String>,
        #[serde(default)]
        in_reply_to: Option<String>,
        #[serde(default)]
        references_hdr: Option<String>,
        #[serde(default)]
        pgp: PgpStatus,
        #[serde(default)]
        attachments_pending: bool,
        #[serde(default)]
        attachments_failed: bool,
    },
    UnsubscribeFinished {
        id: MessageId,
        ok: bool,
        message: String,
    },
    RawMessageReady {
        id: MessageId,
        raw: Vec<u8>,
    },
    RemoteImageSendersChanged {
        emails: Vec<String>,
    },
    SendProgress {
        id: MessageId,
        state: SendState,
    },
    FolderDeleteFinished {
        folder: FolderId,
        removed: usize,
        moved_to_trash: bool,
    },
    Error {
        account: Option<AccountId>,
        message: String,
    },
    AuthRequired {
        account: AccountId,
        message: String,
    },
    Status {
        account: AccountId,
        text: String,
    },
    ContactsChanged {
        contacts: Vec<Contact>,
        groups: Vec<ContactGroup>,
    },
    SentRecipientsChanged {
        addresses: Vec<Address>,
    },
    RulesUpdated {
        account: AccountId,
        rules: Vec<MessageRule>,
        server_rules: ServerRulesStatus,
        preview: Option<ServerRulesPreview>,
    },
    QueuedSendsRecovered(Vec<QueuedSend>),
    PgpState(PgpAccountState),
    PgpPublicKey {
        account: AccountId,
        armored: String,
        fingerprint: String,
    },
    PgpAutocryptOffer {
        message_id: MessageId,
        email: String,
        fingerprint: String,
        armored: String,
    },
    SignaturesUpdated {
        account: AccountId,
        signature: Option<AccountSignature>,
    },
    IdentitiesUpdated {
        account: AccountId,
        identities: Vec<AccountIdentity>,
    },
    SmtpEnvelopePolicy {
        account: AccountId,
        policy: crate::identity::SmtpEnvelopePolicy,
    },
    CatchAllDomainsUpdated {
        account: AccountId,
        domains: Vec<String>,
    },
    SnippetsUpdated {
        snippets: Vec<Snippet>,
    },
    ImapDraftSaved {
        account: AccountId,
        draft: ComposeDraft,
        replaced_newer: bool,
        removed_missing: bool,
        /// The server already had a newer copy, so this save did not write another one.
        #[serde(default)]
        kept_newer: bool,
        /// Why the prior copy counted as removed; shown in the activity log.
        #[serde(default)]
        save_detail: String,
    },
    ImapDraftSaveFailed {
        message: String,
    },
    ImapDraftReady {
        message_id: MessageId,
        draft: ComposeDraft,
    },
    ReactionsLoaded {
        target_id: MessageId,
        people: Vec<ReactionPerson>,
    },
    ReactionReceived {
        reaction_id: MessageId,
        target_id: Option<MessageId>,
        emoji: String,
        from_name: String,
        subject: String,
        notify: bool,
    },
    /// The message a reaction points at, for the reader to show under the reaction line.
    ReactionOriginalReady {
        reaction_id: MessageId,
        target_id: MessageId,
        subject: String,
        text: String,
        html: String,
    },
    /// An event for the extension named in the message.
    Extension(ExtensionMessage),
}

/// File attachment metadata for the reader (non-inline parts only).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BodyAttachment {
    pub filename: String,
    pub mime: String,
    pub sha256: String,
    pub size: u64,
}

/// Inline CID attachment bytes for the mail WebView cache.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CidInline {
    pub sha256: String,
    pub mime: String,
    pub bytes: Vec<u8>,
    /// Content-ID without angle brackets, when the HTML references this part.
    #[serde(default)]
    pub cid: Option<String>,
}

/// Which mail query produced a [`SyncEvent::EnvelopesReplaced`] payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EnvelopeListKind {
    UnifiedInbox,
    Folder(FolderId),
    Search,
    /// A list an extension fills.
    Extension(ExtensionListId),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum SendState {
    Queued,
    Sending,
    Sent {
        #[serde(default)]
        message_id_hdr: String,
    },
    Failed(String),
}
