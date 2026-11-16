//! `StoreHandle` methods for message envelopes, bodies, lists and conversations.

use super::*;

impl StoreHandle {
    pub async fn upsert_envelopes(&self, envs: &[Envelope], body_text: Option<&str>) -> Result<()> {
        send_write(&self.write_tx, |resp| WriteRequest::UpsertEnvelopes {
            envs: envs.to_vec(),
            body_text: body_text.map(str::to_string),
            resp,
        })
        .await
    }

    pub async fn unified_inbox(&self, limit: u32, offset: u32) -> Result<Vec<Envelope>> {
        send_read(&self.read_tx, |resp| ReadRequest::UnifiedInbox {
            limit,
            offset,
            resp,
        })
        .await
    }

    pub async fn unified_inbox_conversations(
        &self,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<Conversation>> {
        send_read(&self.read_tx, |resp| {
            ReadRequest::UnifiedInboxConversations {
                limit,
                offset,
                resp,
            }
        })
        .await
    }

    pub async fn folder_conversations(&self, folder: FolderId) -> Result<Vec<Conversation>> {
        send_read(&self.read_tx, |resp| ReadRequest::FolderConversations {
            folder,
            resp,
        })
        .await
    }

    pub async fn thread_envelopes(
        &self,
        thread_id: ThreadId,
        folder_ids: Option<&[FolderId]>,
        include_sent: bool,
    ) -> Result<Vec<Envelope>> {
        send_read(&self.read_tx, |resp| ReadRequest::ThreadEnvelopes {
            thread_id,
            folder_ids: folder_ids.map(|ids| ids.to_vec()),
            include_sent,
            resp,
        })
        .await
    }

    pub async fn thread_messages_all_folders(
        &self,
        account_id: AccountId,
        thread_id: ThreadId,
    ) -> Result<Vec<Envelope>> {
        send_read(&self.read_tx, |resp| {
            ReadRequest::ThreadMessagesAllFolders {
                account_id,
                thread_id,
                resp,
            }
        })
        .await
    }

    pub async fn messages_with_address(
        &self,
        account_id: AccountId,
        email: &str,
    ) -> Result<Vec<Envelope>> {
        send_read(&self.read_tx, |resp| ReadRequest::MessagesWithAddress {
            account_id,
            email: email.to_string(),
            resp,
        })
        .await
    }

    pub async fn folder_messages(
        &self,
        folder: FolderId,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<Envelope>> {
        send_read(&self.read_tx, |resp| ReadRequest::FolderMessages {
            folder,
            limit,
            offset,
            resp,
        })
        .await
    }

    pub async fn list_sent_recipients(&self) -> Result<Vec<Address>> {
        send_read(&self.read_tx, |resp| ReadRequest::ListSentRecipients {
            resp,
        })
        .await
    }

    pub async fn get_message(&self, id: MessageId) -> Result<Option<StoredMessage>> {
        send_read(&self.read_tx, |resp| ReadRequest::GetMessage { id, resp }).await
    }

    pub async fn set_flags(
        &self,
        id: MessageId,
        seen: Option<bool>,
        flagged: Option<bool>,
    ) -> Result<()> {
        send_write(&self.write_tx, |resp| WriteRequest::SetFlags {
            id,
            seen,
            flagged,
            resp,
        })
        .await
    }

    pub async fn set_flags_many(
        &self,
        ids: &[MessageId],
        seen: Option<bool>,
        flagged: Option<bool>,
    ) -> Result<()> {
        send_write(&self.write_tx, |resp| WriteRequest::SetFlagsMany {
            ids: ids.to_vec(),
            seen,
            flagged,
            resp,
        })
        .await
    }

    pub async fn mark_deleted(&self, id: MessageId) -> Result<()> {
        send_write(&self.write_tx, |resp| WriteRequest::MarkDeleted {
            id,
            resp,
        })
        .await
    }

    pub async fn mark_deleted_by_uids(&self, folder: FolderId, uids: Vec<u32>) -> Result<usize> {
        send_write(&self.write_tx, |resp| WriteRequest::MarkDeletedByUids {
            folder,
            uids,
            resp,
        })
        .await
    }

    /// Write server-reported flags onto existing rows; returns the ids that changed.
    pub async fn apply_server_flags(
        &self,
        folder: FolderId,
        flags: Vec<alcedo_core::ServerFlags>,
    ) -> Result<Vec<MessageId>> {
        send_write(&self.write_tx, |resp| WriteRequest::ApplyServerFlags {
            folder,
            flags,
            resp,
        })
        .await
    }

    pub async fn rehome_message(
        &self,
        id: MessageId,
        folder: FolderId,
        uid: u32,
        deleted_at: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Result<bool> {
        send_write(&self.write_tx, |resp| WriteRequest::RehomeMessage {
            id,
            folder,
            uid,
            deleted_at,
            resp,
        })
        .await
    }

    pub async fn set_rfc822_sizes(&self, folder: FolderId, rows: &[(u32, i64)]) -> Result<()> {
        send_write(&self.write_tx, |resp| WriteRequest::SetRfc822Sizes {
            folder,
            rows: rows.to_vec(),
            resp,
        })
        .await
    }

    pub async fn uids_missing_rfc822_size(&self, folder: FolderId, limit: u32) -> Result<Vec<u32>> {
        send_read(&self.read_tx, |resp| ReadRequest::UidsMissingRfc822Size {
            folder,
            limit,
            resp,
        })
        .await
    }

    pub async fn uids_missing_reply_headers(
        &self,
        folder: FolderId,
        limit: u32,
    ) -> Result<Vec<u32>> {
        send_read(&self.read_tx, |resp| ReadRequest::UidsMissingReplyHeaders {
            folder,
            limit,
            resp,
        })
        .await
    }

    pub async fn set_reply_headers(
        &self,
        folder: FolderId,
        rows: &[(u32, String, String)],
    ) -> Result<()> {
        send_write(&self.write_tx, |resp| WriteRequest::SetReplyHeaders {
            folder,
            rows: rows.to_vec(),
            resp,
        })
        .await
    }

    pub async fn set_message_id_hdrs(
        &self,
        folder: FolderId,
        rows: &[(u32, String)],
    ) -> Result<()> {
        send_write(&self.write_tx, |resp| WriteRequest::SetMessageIdHdrs {
            folder,
            rows: rows.to_vec(),
            resp,
        })
        .await
    }

    pub async fn set_threading_hdrs(
        &self,
        folder: FolderId,
        rows: &[(u32, String, Option<String>, Option<String>)],
    ) -> Result<()> {
        send_write(&self.write_tx, |resp| WriteRequest::SetThreadingHdrs {
            folder,
            rows: rows.to_vec(),
            resp,
        })
        .await
    }

    pub async fn messages_by_uids(
        &self,
        folder: FolderId,
        uids: &[u32],
    ) -> Result<Vec<(MessageId, u32, Option<String>)>> {
        send_read(&self.read_tx, |resp| ReadRequest::MessagesByUids {
            folder,
            uids: uids.to_vec(),
            resp,
        })
        .await
    }

    /// Size recorded by the list sync, if any; lets a caller plan a fetch
    /// without asking the server first.
    pub async fn message_rfc822_size(&self, id: MessageId) -> Result<Option<u32>> {
        send_read(&self.read_tx, |resp| ReadRequest::MessageRfc822Size {
            id,
            resp,
        })
        .await
    }

    pub async fn message_locators(&self, ids: &[MessageId]) -> Result<Vec<MessageLocator>> {
        send_read(&self.read_tx, |resp| ReadRequest::MessageLocators {
            ids: ids.to_vec(),
            resp,
        })
        .await
    }

    pub async fn live_message_id_hdrs(&self, folder: FolderId) -> Result<Vec<(u32, String)>> {
        send_read(&self.read_tx, |resp| ReadRequest::LiveMessageIdHdrs {
            folder,
            resp,
        })
        .await
    }

    pub async fn has_live_copy_elsewhere(&self, id: MessageId) -> Result<bool> {
        send_read(&self.read_tx, |resp| ReadRequest::HasLiveCopyElsewhere {
            id,
            resp,
        })
        .await
    }

    pub async fn live_sibling_ids(&self, id: MessageId) -> Result<Vec<MessageId>> {
        send_read(&self.read_tx, |resp| ReadRequest::LiveSiblingIds {
            id,
            resp,
        })
        .await
    }

    pub async fn mark_folder_seen(&self, folder: FolderId) -> Result<()> {
        send_write(&self.write_tx, |resp| WriteRequest::MarkFolderSeen {
            folder,
            resp,
        })
        .await
    }

    pub async fn mark_folder_deleted(&self, folder: FolderId) -> Result<()> {
        send_write(&self.write_tx, |resp| WriteRequest::MarkFolderDeleted {
            folder,
            resp,
        })
        .await
    }

    pub async fn save_message_body(
        &self,
        id: MessageId,
        body_text: Option<&str>,
        body_html: Option<&str>,
        pgp_status: Option<&PgpStatus>,
    ) -> Result<()> {
        send_write(&self.write_tx, |resp| WriteRequest::SaveMessageBody {
            id,
            body_text: body_text.map(str::to_string),
            body_html: body_html.map(str::to_string),
            pgp_status: pgp_status.cloned(),
            resp,
        })
        .await
    }

    pub async fn purge_missing_uids(
        &self,
        folder: FolderId,
        live_uids: &[u32],
    ) -> Result<Vec<MessageId>> {
        send_write(&self.write_tx, |resp| WriteRequest::PurgeMissingUids {
            folder,
            live_uids: live_uids.to_vec(),
            resp,
        })
        .await
    }

    pub async fn unread_counts(&self) -> Result<Vec<(AccountId, u32, u32)>> {
        send_read(&self.read_tx, |resp| ReadRequest::UnreadCounts { resp }).await
    }

    pub async fn folder_unread_count(&self, folder: FolderId) -> Result<u32> {
        send_read(&self.read_tx, |resp| ReadRequest::FolderUnreadCount {
            folder,
            resp,
        })
        .await
    }

    pub async fn folder_live_count(&self, folder: FolderId) -> Result<u32> {
        send_read(&self.read_tx, |resp| ReadRequest::FolderLiveCount {
            folder,
            resp,
        })
        .await
    }

    pub async fn messages_missing_body(
        &self,
        folder: FolderId,
        window: u32,
    ) -> Result<Vec<MessageId>> {
        send_read(&self.read_tx, |resp| ReadRequest::MessagesMissingBody {
            folder,
            window,
            resp,
        })
        .await
    }

    pub async fn set_allow_remote_images(&self, id: MessageId, allow: bool) -> Result<()> {
        send_write(&self.write_tx, |resp| WriteRequest::SetAllowRemoteImages {
            id,
            allow,
            resp,
        })
        .await
    }

    pub async fn save_list_unsubscribe(
        &self,
        id: MessageId,
        value: Option<&alcedo_core::ListUnsubscribe>,
    ) -> Result<()> {
        send_write(&self.write_tx, |resp| WriteRequest::SaveListUnsubscribe {
            id,
            value: value.cloned(),
            resp,
        })
        .await
    }

    pub async fn save_reaction(
        &self,
        id: MessageId,
        emoji: Option<&str>,
        target_hdr: Option<&str>,
    ) -> Result<()> {
        send_write(&self.write_tx, |resp| WriteRequest::SaveReaction {
            id,
            emoji: emoji.map(str::to_string),
            target_hdr: target_hdr.map(str::to_string),
            resp,
        })
        .await
    }

    pub async fn reactions_for_target(
        &self,
        account: AccountId,
        message_id_hdr: &str,
    ) -> Result<Vec<alcedo_core::ReactionPerson>> {
        send_read(&self.read_tx, |resp| ReadRequest::ReactionsForTarget {
            account,
            message_id_hdr: message_id_hdr.to_string(),
            resp,
        })
        .await
    }

    pub async fn reaction_target_message(
        &self,
        account: AccountId,
        message_id_hdr: &str,
    ) -> Result<Option<(MessageId, String)>> {
        send_read(&self.read_tx, |resp| ReadRequest::ReactionTargetMessage {
            account,
            message_id_hdr: message_id_hdr.to_string(),
            resp,
        })
        .await
    }

    pub async fn reaction_target_hdr(&self, id: MessageId) -> Result<Option<String>> {
        send_read(&self.read_tx, |resp| ReadRequest::ReactionTargetHdr {
            id,
            resp,
        })
        .await
    }

    pub async fn uids_missing_routing_headers(
        &self,
        folder: FolderId,
        limit: u32,
    ) -> Result<Vec<u32>> {
        send_read(&self.read_tx, |resp| {
            ReadRequest::UidsMissingRoutingHeaders {
                folder,
                limit,
                resp,
            }
        })
        .await
    }

    pub async fn set_routing_headers(
        &self,
        folder: FolderId,
        rows: Vec<(u32, RoutingHeaders)>,
    ) -> Result<()> {
        send_write(&self.write_tx, |resp| WriteRequest::SetRoutingHeaders {
            folder,
            rows,
            resp,
        })
        .await
    }

    pub async fn set_has_attachments_true(&self, id: MessageId) -> Result<()> {
        send_write(&self.write_tx, |resp| WriteRequest::SetHasAttachmentsTrue {
            id,
            resp,
        })
        .await
    }

    pub async fn sent_structure_scan_done(&self, account: AccountId) -> Result<bool> {
        send_read(&self.read_tx, |resp| ReadRequest::SentStructureScanDone {
            account,
            resp,
        })
        .await
    }

    pub async fn sent_uids_missing_attachment_flag(
        &self,
        folder: FolderId,
    ) -> Result<Vec<(MessageId, u32)>> {
        send_read(&self.read_tx, |resp| {
            ReadRequest::SentUidsMissingAttachmentFlag { folder, resp }
        })
        .await
    }

    pub async fn mark_sent_structure_scan_done(&self, account: AccountId) -> Result<()> {
        send_write(&self.write_tx, |resp| {
            WriteRequest::MarkSentStructureScanDone { account, resp }
        })
        .await
    }

    pub async fn folder_messages_all(&self, folder: FolderId) -> Result<Vec<Envelope>> {
        send_read(&self.read_tx, |resp| ReadRequest::FolderMessagesAll {
            folder,
            resp,
        })
        .await
    }

    pub async fn next_local_uid(&self, folder_id: FolderId) -> Result<u32> {
        send_write(&self.write_tx, |resp| WriteRequest::NextLocalUid {
            folder_id,
            resp,
        })
        .await
    }

    pub async fn set_list_bump_utc(
        &self,
        id: MessageId,
        when: chrono::DateTime<chrono::Utc>,
    ) -> Result<()> {
        send_write(&self.write_tx, |resp| WriteRequest::SetListBumpUtc {
            id,
            when,
            resp,
        })
        .await
    }
}
