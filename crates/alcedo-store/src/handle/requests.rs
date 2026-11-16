//! Read and write request enums sent to the store threads, grouped by table.

use super::*;

/// A closure an extension runs on a store connection; it sends its own result.
pub(super) type ExtensionJob = Box<dyn FnOnce(&rusqlite::Connection) + Send>;
use crate::folders;

pub(super) enum ReadRequest {
    // --- accounts ---
    ListAccounts {
        resp: oneshot::Sender<StoreResult<Vec<Account>>>,
    },
    GetSmtpEnvelopePolicy {
        account: AccountId,
        resp: oneshot::Sender<StoreResult<SmtpEnvelopePolicy>>,
    },
    GetCatchAllDomains {
        account: AccountId,
        resp: oneshot::Sender<StoreResult<Vec<String>>>,
    },

    // --- folders ---
    ListFolders {
        account: AccountId,
        resp: oneshot::Sender<StoreResult<Vec<Folder>>>,
    },
    GetFolder {
        id: FolderId,
        resp: oneshot::Sender<StoreResult<Option<Folder>>>,
    },

    // --- messages ---
    UnifiedInbox {
        limit: u32,
        offset: u32,
        resp: oneshot::Sender<StoreResult<Vec<Envelope>>>,
    },
    UnifiedInboxConversations {
        limit: u32,
        offset: u32,
        resp: oneshot::Sender<StoreResult<Vec<Conversation>>>,
    },
    FolderMessages {
        folder: FolderId,
        limit: u32,
        offset: u32,
        resp: oneshot::Sender<StoreResult<Vec<Envelope>>>,
    },
    FolderConversations {
        folder: FolderId,
        resp: oneshot::Sender<StoreResult<Vec<Conversation>>>,
    },
    ThreadEnvelopes {
        thread_id: ThreadId,
        folder_ids: Option<Vec<FolderId>>,
        include_sent: bool,
        resp: oneshot::Sender<StoreResult<Vec<Envelope>>>,
    },
    ThreadMessagesAllFolders {
        account_id: AccountId,
        thread_id: ThreadId,
        resp: oneshot::Sender<StoreResult<Vec<Envelope>>>,
    },
    MessagesWithAddress {
        account_id: AccountId,
        email: String,
        resp: oneshot::Sender<StoreResult<Vec<Envelope>>>,
    },
    ListSentRecipients {
        resp: oneshot::Sender<StoreResult<Vec<Address>>>,
    },
    GetMessage {
        id: MessageId,
        resp: oneshot::Sender<StoreResult<Option<StoredMessage>>>,
    },
    MessagesByUids {
        folder: FolderId,
        uids: Vec<u32>,
        resp: oneshot::Sender<StoreResult<Vec<MessageUidRow>>>,
    },
    MessageRfc822Size {
        id: MessageId,
        resp: oneshot::Sender<StoreResult<Option<u32>>>,
    },
    MessageLocators {
        ids: Vec<MessageId>,
        resp: oneshot::Sender<StoreResult<Vec<MessageLocator>>>,
    },
    LiveMessageIdHdrs {
        folder: FolderId,
        resp: oneshot::Sender<StoreResult<Vec<(u32, String)>>>,
    },
    HasLiveCopyElsewhere {
        id: MessageId,
        resp: oneshot::Sender<StoreResult<bool>>,
    },
    LiveSiblingIds {
        id: MessageId,
        resp: oneshot::Sender<StoreResult<Vec<MessageId>>>,
    },
    UnreadCounts {
        resp: oneshot::Sender<StoreResult<Vec<(AccountId, u32, u32)>>>,
    },
    FolderUnreadCount {
        folder: FolderId,
        resp: oneshot::Sender<StoreResult<u32>>,
    },
    FolderLiveCount {
        folder: FolderId,
        resp: oneshot::Sender<StoreResult<u32>>,
    },
    MessagesMissingBody {
        folder: FolderId,
        window: u32,
        resp: oneshot::Sender<StoreResult<Vec<MessageId>>>,
    },
    ReactionsForTarget {
        account: AccountId,
        message_id_hdr: String,
        resp: oneshot::Sender<StoreResult<Vec<alcedo_core::ReactionPerson>>>,
    },
    ReactionTargetMessage {
        account: AccountId,
        message_id_hdr: String,
        resp: oneshot::Sender<StoreResult<Option<(MessageId, String)>>>,
    },
    ReactionTargetHdr {
        id: MessageId,
        resp: oneshot::Sender<StoreResult<Option<String>>>,
    },
    SentStructureScanDone {
        account: AccountId,
        resp: oneshot::Sender<StoreResult<bool>>,
    },
    SentUidsMissingAttachmentFlag {
        folder: FolderId,
        resp: oneshot::Sender<StoreResult<Vec<(MessageId, u32)>>>,
    },
    UidsMissingRfc822Size {
        folder: FolderId,
        limit: u32,
        resp: oneshot::Sender<StoreResult<Vec<u32>>>,
    },
    UidsMissingReplyHeaders {
        folder: FolderId,
        limit: u32,
        resp: oneshot::Sender<StoreResult<Vec<u32>>>,
    },
    UidsMissingRoutingHeaders {
        folder: FolderId,
        limit: u32,
        resp: oneshot::Sender<StoreResult<Vec<u32>>>,
    },
    FolderMessagesAll {
        folder: FolderId,
        resp: oneshot::Sender<StoreResult<Vec<Envelope>>>,
    },

    // --- attachments ---
    LoadAttachment {
        sha256: String,
        resp: oneshot::Sender<StoreResult<Vec<u8>>>,
    },

    // --- search ---
    Search {
        query: String,
        folder_ids: Option<Vec<FolderId>>,
        limit: u32,
        resp: oneshot::Sender<StoreResult<Vec<Envelope>>>,
    },
    SearchConversations {
        query: String,
        folder_ids: Option<Vec<FolderId>>,
        limit: u32,
        resp: oneshot::Sender<StoreResult<Vec<Conversation>>>,
    },

    // --- remote_images ---
    ListRemoteImageSenders {
        resp: oneshot::Sender<StoreResult<Vec<String>>>,
    },

    // --- contacts ---
    ContactsSnapshot {
        resp: oneshot::Sender<StoreResult<(Vec<Contact>, Vec<ContactGroup>)>>,
    },

    // --- rules ---
    ListRules {
        account: AccountId,
        resp: oneshot::Sender<StoreResult<Vec<MessageRule>>>,
    },
    HasRuleRun {
        message_id: MessageId,
        resp: oneshot::Sender<StoreResult<bool>>,
    },
    RuleRunsFor {
        ids: Vec<MessageId>,
        resp: oneshot::Sender<StoreResult<HashSet<MessageId>>>,
    },

    // --- queued_sends ---
    ListQueuedSends {
        resp: oneshot::Sender<StoreResult<Vec<QueuedSend>>>,
    },

    // --- pgp ---
    ListPgpKeys {
        account: AccountId,
        resp: oneshot::Sender<StoreResult<Vec<PgpPeerInfo>>>,
    },
    KeysForEmails {
        emails: Vec<String>,
        resp: oneshot::Sender<StoreResult<Vec<PgpPeerInfo>>>,
    },
    GetPgpSignOutgoing {
        account: AccountId,
        resp: oneshot::Sender<StoreResult<bool>>,
    },

    // --- identities ---
    ListIdentities {
        account: AccountId,
        resp: oneshot::Sender<StoreResult<Vec<AccountIdentity>>>,
    },

    // --- signatures ---
    GetSignature {
        account: AccountId,
        resp: oneshot::Sender<StoreResult<Option<AccountSignature>>>,
    },

    // --- snippets ---
    ListSnippets {
        resp: oneshot::Sender<StoreResult<Vec<Snippet>>>,
    },

    // --- extensions ---
    /// Run an extension's closure on the reader connection.
    Extension {
        label: &'static str,
        run: ExtensionJob,
    },
}

impl ReadRequest {
    pub(super) fn kind(&self) -> &'static str {
        match self {
            // --- accounts ---
            ReadRequest::ListAccounts { .. } => "list_accounts",
            ReadRequest::GetSmtpEnvelopePolicy { .. } => "get_smtp_envelope_policy",
            ReadRequest::GetCatchAllDomains { .. } => "get_catch_all_domains",

            // --- folders ---
            ReadRequest::ListFolders { .. } => "list_folders",
            ReadRequest::GetFolder { .. } => "get_folder",

            // --- messages ---
            ReadRequest::UnifiedInbox { .. } => "unified_inbox",
            ReadRequest::UnifiedInboxConversations { .. } => "unified_inbox_conversations",
            ReadRequest::FolderMessages { .. } => "folder_messages",
            ReadRequest::FolderConversations { .. } => "folder_conversations",
            ReadRequest::ThreadEnvelopes { .. } => "thread_envelopes",
            ReadRequest::ThreadMessagesAllFolders { .. } => "thread_messages_all_folders",
            ReadRequest::MessagesWithAddress { .. } => "messages_with_address",
            ReadRequest::ListSentRecipients { .. } => "list_sent_recipients",
            ReadRequest::GetMessage { .. } => "get_message",
            ReadRequest::MessagesByUids { .. } => "messages_by_uids",
            ReadRequest::MessageLocators { .. } => "message_locators",
            ReadRequest::MessageRfc822Size { .. } => "message_rfc822_size",
            ReadRequest::LiveMessageIdHdrs { .. } => "live_message_id_hdrs",
            ReadRequest::HasLiveCopyElsewhere { .. } => "has_live_copy_elsewhere",
            ReadRequest::LiveSiblingIds { .. } => "live_sibling_ids",
            ReadRequest::UnreadCounts { .. } => "unread_counts",
            ReadRequest::FolderUnreadCount { .. } => "folder_unread_count",
            ReadRequest::FolderLiveCount { .. } => "folder_live_count",
            ReadRequest::MessagesMissingBody { .. } => "messages_missing_body",
            ReadRequest::ReactionsForTarget { .. } => "reactions_for_target",
            ReadRequest::ReactionTargetMessage { .. } => "reaction_target_message",
            ReadRequest::ReactionTargetHdr { .. } => "reaction_target_hdr",
            ReadRequest::SentStructureScanDone { .. } => "sent_structure_scan_done",
            ReadRequest::SentUidsMissingAttachmentFlag { .. } => {
                "sent_uids_missing_attachment_flag"
            }
            ReadRequest::UidsMissingRfc822Size { .. } => "uids_missing_rfc822_size",
            ReadRequest::UidsMissingReplyHeaders { .. } => "uids_missing_reply_headers",
            ReadRequest::UidsMissingRoutingHeaders { .. } => "uids_missing_routing_headers",
            ReadRequest::FolderMessagesAll { .. } => "folder_messages_all",

            // --- attachments ---
            ReadRequest::LoadAttachment { .. } => "load_attachment",

            // --- search ---
            ReadRequest::Search { .. } => "search",
            ReadRequest::SearchConversations { .. } => "search_conversations",

            // --- remote_images ---
            ReadRequest::ListRemoteImageSenders { .. } => "list_remote_image_senders",

            // --- contacts ---
            ReadRequest::ContactsSnapshot { .. } => "contacts_snapshot",

            // --- rules ---
            ReadRequest::ListRules { .. } => "list_rules",
            ReadRequest::HasRuleRun { .. } => "has_rule_run",
            ReadRequest::RuleRunsFor { .. } => "rule_runs_for",

            // --- queued_sends ---
            ReadRequest::ListQueuedSends { .. } => "list_queued_sends",

            // --- pgp ---
            ReadRequest::ListPgpKeys { .. } => "list_pgp_keys",
            ReadRequest::KeysForEmails { .. } => "keys_for_emails",
            ReadRequest::GetPgpSignOutgoing { .. } => "get_pgp_sign_outgoing",

            // --- identities ---
            ReadRequest::ListIdentities { .. } => "list_identities",

            // --- signatures ---
            ReadRequest::GetSignature { .. } => "get_signature",

            // --- snippets ---
            ReadRequest::ListSnippets { .. } => "list_snippets",

            // --- extensions ---
            ReadRequest::Extension { label, .. } => label,
        }
    }
}

pub(super) enum WriteRequest {
    // --- accounts ---
    UpsertAccount {
        account: Account,
        resp: oneshot::Sender<StoreResult<()>>,
    },
    DeleteAccount {
        id: AccountId,
        resp: oneshot::Sender<StoreResult<()>>,
    },
    SetCatchAllDomains {
        account: AccountId,
        domains: Vec<String>,
        resp: oneshot::Sender<StoreResult<()>>,
    },
    SetSmtpEnvelopePolicy {
        account: AccountId,
        policy: SmtpEnvelopePolicy,
        resp: oneshot::Sender<StoreResult<()>>,
    },

    // --- folders ---
    ReplaceFolders {
        account: AccountId,
        folders: Vec<Folder>,
        mode: Option<folders::FolderReplaceMode>,
        resp: oneshot::Sender<StoreResult<Vec<Folder>>>,
    },
    SetFolderUnread {
        folder: FolderId,
        unread: u32,
        resp: oneshot::Sender<StoreResult<()>>,
    },
    RenameFolderTree {
        account: AccountId,
        pairs: Vec<(String, String)>,
        resp: oneshot::Sender<StoreResult<()>>,
    },

    // --- messages ---
    UpsertEnvelopes {
        envs: Vec<Envelope>,
        body_text: Option<String>,
        resp: oneshot::Sender<StoreResult<()>>,
    },
    SetFlags {
        id: MessageId,
        seen: Option<bool>,
        flagged: Option<bool>,
        resp: oneshot::Sender<StoreResult<()>>,
    },
    SetFlagsMany {
        ids: Vec<MessageId>,
        seen: Option<bool>,
        flagged: Option<bool>,
        resp: oneshot::Sender<StoreResult<()>>,
    },
    MarkDeleted {
        id: MessageId,
        resp: oneshot::Sender<StoreResult<()>>,
    },
    MarkDeletedByUids {
        folder: FolderId,
        uids: Vec<u32>,
        resp: oneshot::Sender<StoreResult<usize>>,
    },
    ApplyServerFlags {
        folder: FolderId,
        flags: Vec<alcedo_core::ServerFlags>,
        resp: oneshot::Sender<StoreResult<Vec<MessageId>>>,
    },
    RehomeMessage {
        id: MessageId,
        folder: FolderId,
        uid: u32,
        deleted_at: Option<chrono::DateTime<chrono::Utc>>,
        resp: oneshot::Sender<StoreResult<bool>>,
    },
    SetMessageIdHdrs {
        folder: FolderId,
        rows: Vec<(u32, String)>,
        resp: oneshot::Sender<StoreResult<()>>,
    },
    SetRfc822Sizes {
        folder: FolderId,
        rows: Vec<(u32, i64)>,
        resp: oneshot::Sender<StoreResult<()>>,
    },
    SetReplyHeaders {
        folder: FolderId,
        rows: Vec<(u32, String, String)>,
        resp: oneshot::Sender<StoreResult<()>>,
    },
    SetRoutingHeaders {
        folder: FolderId,
        rows: Vec<(u32, RoutingHeaders)>,
        resp: oneshot::Sender<StoreResult<()>>,
    },
    SetThreadingHdrs {
        folder: FolderId,
        rows: Vec<(u32, String, Option<String>, Option<String>)>,
        resp: oneshot::Sender<StoreResult<()>>,
    },
    MarkFolderSeen {
        folder: FolderId,
        resp: oneshot::Sender<StoreResult<()>>,
    },
    MarkFolderDeleted {
        folder: FolderId,
        resp: oneshot::Sender<StoreResult<()>>,
    },
    SaveMessageBody {
        id: MessageId,
        body_text: Option<String>,
        body_html: Option<String>,
        pgp_status: Option<PgpStatus>,
        resp: oneshot::Sender<StoreResult<()>>,
    },
    PurgeMissingUids {
        folder: FolderId,
        live_uids: Vec<u32>,
        resp: oneshot::Sender<StoreResult<Vec<MessageId>>>,
    },
    SetAllowRemoteImages {
        id: MessageId,
        allow: bool,
        resp: oneshot::Sender<StoreResult<()>>,
    },
    SaveListUnsubscribe {
        id: MessageId,
        value: Option<alcedo_core::ListUnsubscribe>,
        resp: oneshot::Sender<StoreResult<()>>,
    },
    SaveReaction {
        id: MessageId,
        emoji: Option<String>,
        target_hdr: Option<String>,
        resp: oneshot::Sender<StoreResult<()>>,
    },
    SetHasAttachmentsTrue {
        id: MessageId,
        resp: oneshot::Sender<StoreResult<()>>,
    },
    MarkSentStructureScanDone {
        account: AccountId,
        resp: oneshot::Sender<StoreResult<()>>,
    },
    NextLocalUid {
        folder_id: FolderId,
        resp: oneshot::Sender<StoreResult<u32>>,
    },
    SetListBumpUtc {
        id: MessageId,
        when: chrono::DateTime<chrono::Utc>,
        resp: oneshot::Sender<StoreResult<()>>,
    },

    // --- attachments ---
    SaveAttachment {
        meta: AttachmentMeta,
        bytes: Vec<u8>,
        resp: oneshot::Sender<StoreResult<PathBuf>>,
    },
    DeleteAttachment {
        message_id: MessageId,
        sha256: String,
        resp: oneshot::Sender<StoreResult<()>>,
    },

    // --- remote_images ---
    SetRemoteImageSender {
        email: String,
        allow: bool,
        resp: oneshot::Sender<StoreResult<()>>,
    },

    // --- contacts ---
    UpsertContact {
        draft: ContactDraft,
        resp: oneshot::Sender<StoreResult<Contact>>,
    },
    DeleteContacts {
        ids: Vec<ContactId>,
        resp: oneshot::Sender<StoreResult<()>>,
    },
    UpsertGroup {
        draft: GroupDraft,
        resp: oneshot::Sender<StoreResult<ContactGroup>>,
    },
    DeleteGroup {
        id: GroupId,
        resp: oneshot::Sender<StoreResult<()>>,
    },
    PromoteContact {
        id: ContactId,
        resp: oneshot::Sender<StoreResult<Contact>>,
    },
    BlockContact {
        id: ContactId,
        resp: oneshot::Sender<StoreResult<Contact>>,
    },
    AllowSender {
        email: String,
        display_name: String,
        resp: oneshot::Sender<StoreResult<Contact>>,
    },
    CollectOutgoing {
        account_email: String,
        addresses: Vec<Address>,
        resp: oneshot::Sender<StoreResult<()>>,
    },

    // --- rules ---
    UpsertRule {
        rule: MessageRule,
        resp: oneshot::Sender<StoreResult<MessageRule>>,
    },
    DeleteRule {
        id: RuleId,
        resp: oneshot::Sender<StoreResult<()>>,
    },
    ReorderRules {
        account: AccountId,
        ids: Vec<RuleId>,
        resp: oneshot::Sender<StoreResult<()>>,
    },
    SetRuleExecution {
        id: RuleId,
        execution: RuleExecution,
        resp: oneshot::Sender<StoreResult<()>>,
    },
    MarkRuleRun {
        message_id: MessageId,
        resp: oneshot::Sender<StoreResult<()>>,
    },
    MarkRuleRuns {
        ids: Vec<MessageId>,
        resp: oneshot::Sender<StoreResult<()>>,
    },

    // --- queued_sends ---
    InsertQueuedSend {
        queued: Box<QueuedSend>,
        resp: oneshot::Sender<StoreResult<()>>,
    },
    DeleteQueuedSend {
        id: QueuedSendId,
        resp: oneshot::Sender<StoreResult<()>>,
    },

    // --- pgp ---
    UpsertPgpKey {
        fingerprint: String,
        emails: Vec<String>,
        armored: String,
        source: alcedo_core::PgpKeySource,
        account_id: Option<AccountId>,
        resp: oneshot::Sender<StoreResult<()>>,
    },
    DeleteOwnPgpKey {
        account: AccountId,
        resp: oneshot::Sender<StoreResult<()>>,
    },
    DeleteAllPgpKeys {
        account: AccountId,
        resp: oneshot::Sender<StoreResult<()>>,
    },
    SetPgpSignOutgoing {
        account: AccountId,
        enabled: bool,
        resp: oneshot::Sender<StoreResult<()>>,
    },

    // --- identities ---
    UpsertIdentity {
        identity: AccountIdentity,
        resp: oneshot::Sender<StoreResult<AccountIdentity>>,
    },
    DeleteIdentity {
        id: IdentityId,
        resp: oneshot::Sender<StoreResult<()>>,
    },

    // --- signatures ---
    UpsertSignature {
        signature: AccountSignature,
        resp: oneshot::Sender<StoreResult<AccountSignature>>,
    },
    DeleteSignature {
        account: AccountId,
        resp: oneshot::Sender<StoreResult<()>>,
    },

    // --- snippets ---
    UpsertSnippet {
        snippet: Snippet,
        resp: oneshot::Sender<StoreResult<Snippet>>,
    },
    DeleteSnippet {
        id: SnippetId,
        resp: oneshot::Sender<StoreResult<()>>,
    },

    // --- extensions ---
    /// Run an extension's closure on the writer connection.
    Extension {
        label: &'static str,
        run: ExtensionJob,
    },
}

impl WriteRequest {
    pub(super) fn kind(&self) -> &'static str {
        match self {
            // --- accounts ---
            WriteRequest::UpsertAccount { .. } => "upsert_account",
            WriteRequest::DeleteAccount { .. } => "delete_account",
            WriteRequest::SetCatchAllDomains { .. } => "set_catch_all_domains",
            WriteRequest::SetSmtpEnvelopePolicy { .. } => "set_smtp_envelope_policy",

            // --- folders ---
            WriteRequest::ReplaceFolders { .. } => "replace_folders",
            WriteRequest::SetFolderUnread { .. } => "set_folder_unread",
            WriteRequest::RenameFolderTree { .. } => "rename_folder_tree",

            // --- messages ---
            WriteRequest::UpsertEnvelopes { .. } => "upsert_envelopes",
            WriteRequest::SetFlags { .. } => "set_flags",
            WriteRequest::SetFlagsMany { .. } => "set_flags_many",
            WriteRequest::MarkDeleted { .. } => "mark_deleted",
            WriteRequest::MarkDeletedByUids { .. } => "mark_deleted_by_uids",
            WriteRequest::ApplyServerFlags { .. } => "apply_server_flags",
            WriteRequest::RehomeMessage { .. } => "rehome_message",
            WriteRequest::SetMessageIdHdrs { .. } => "set_message_id_hdrs",
            WriteRequest::SetRfc822Sizes { .. } => "set_rfc822_sizes",
            WriteRequest::SetReplyHeaders { .. } => "set_reply_headers",
            WriteRequest::SetRoutingHeaders { .. } => "set_routing_headers",
            WriteRequest::SetThreadingHdrs { .. } => "set_threading_hdrs",
            WriteRequest::MarkFolderSeen { .. } => "mark_folder_seen",
            WriteRequest::MarkFolderDeleted { .. } => "mark_folder_deleted",
            WriteRequest::SaveMessageBody { .. } => "save_message_body",
            WriteRequest::PurgeMissingUids { .. } => "purge_missing_uids",
            WriteRequest::SetAllowRemoteImages { .. } => "set_allow_remote_images",
            WriteRequest::SaveListUnsubscribe { .. } => "save_list_unsubscribe",
            WriteRequest::SaveReaction { .. } => "save_reaction",
            WriteRequest::SetHasAttachmentsTrue { .. } => "set_has_attachments_true",
            WriteRequest::MarkSentStructureScanDone { .. } => "mark_sent_structure_scan_done",
            WriteRequest::NextLocalUid { .. } => "next_local_uid",
            WriteRequest::SetListBumpUtc { .. } => "set_list_bump_utc",

            // --- attachments ---
            WriteRequest::SaveAttachment { .. } => "save_attachment",
            WriteRequest::DeleteAttachment { .. } => "delete_attachment",

            // --- remote_images ---
            WriteRequest::SetRemoteImageSender { .. } => "set_remote_image_sender",

            // --- contacts ---
            WriteRequest::UpsertContact { .. } => "upsert_contact",
            WriteRequest::DeleteContacts { .. } => "delete_contacts",
            WriteRequest::UpsertGroup { .. } => "upsert_group",
            WriteRequest::DeleteGroup { .. } => "delete_group",
            WriteRequest::PromoteContact { .. } => "promote_contact",
            WriteRequest::BlockContact { .. } => "block_contact",
            WriteRequest::AllowSender { .. } => "allow_sender",
            WriteRequest::CollectOutgoing { .. } => "collect_outgoing",

            // --- rules ---
            WriteRequest::UpsertRule { .. } => "upsert_rule",
            WriteRequest::DeleteRule { .. } => "delete_rule",
            WriteRequest::ReorderRules { .. } => "reorder_rules",
            WriteRequest::SetRuleExecution { .. } => "set_rule_execution",
            WriteRequest::MarkRuleRun { .. } => "mark_rule_run",
            WriteRequest::MarkRuleRuns { .. } => "mark_rule_runs",

            // --- queued_sends ---
            WriteRequest::InsertQueuedSend { .. } => "insert_queued_send",
            WriteRequest::DeleteQueuedSend { .. } => "delete_queued_send",

            // --- pgp ---
            WriteRequest::UpsertPgpKey { .. } => "upsert_pgp_key",
            WriteRequest::DeleteOwnPgpKey { .. } => "delete_own_pgp_key",
            WriteRequest::DeleteAllPgpKeys { .. } => "delete_all_pgp_keys",
            WriteRequest::SetPgpSignOutgoing { .. } => "set_pgp_sign_outgoing",

            // --- identities ---
            WriteRequest::UpsertIdentity { .. } => "upsert_identity",
            WriteRequest::DeleteIdentity { .. } => "delete_identity",

            // --- signatures ---
            WriteRequest::UpsertSignature { .. } => "upsert_signature",
            WriteRequest::DeleteSignature { .. } => "delete_signature",

            // --- snippets ---
            WriteRequest::UpsertSnippet { .. } => "upsert_snippet",
            WriteRequest::DeleteSnippet { .. } => "delete_snippet",

            // --- extensions ---
            WriteRequest::Extension { label, .. } => label,
        }
    }
}
