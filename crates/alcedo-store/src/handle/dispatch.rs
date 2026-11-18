//! Executes store requests on the writer and reader threads, grouped by table.

use super::*;
use crate::{
    accounts, attachments, contacts, folders, identities, messages, pgp, queued_sends,
    remote_images, rules, search, signatures, snippets,
};

pub(super) fn dispatch_read(reader: &mut ReaderState, req: ReadRequest) {
    match req {
        // --- accounts ---
        ReadRequest::ListAccounts { resp } => {
            let _ = resp.send(accounts::list_accounts(&reader.conn));
        }
        ReadRequest::GetSmtpEnvelopePolicy { account, resp } => {
            let _ = resp.send(accounts::get_smtp_envelope_policy(&reader.conn, account));
        }
        ReadRequest::GetCatchAllDomains { account, resp } => {
            let _ = resp.send(accounts::get_catch_all_domains(&reader.conn, account));
        }

        // --- folders ---
        ReadRequest::ListFolders { account, resp } => {
            let _ = resp.send(folders::list_folders(&reader.conn, account));
        }
        ReadRequest::GetFolder { id, resp } => {
            let _ = resp.send(folders::get_folder(&reader.conn, id));
        }

        // --- messages ---
        ReadRequest::UnifiedInbox {
            limit,
            offset,
            resp,
        } => {
            let _ = resp.send(messages::unified_inbox(&reader.conn, limit, offset));
        }
        ReadRequest::UnifiedInboxConversations {
            limit,
            offset,
            resp,
        } => {
            let _ = resp.send(messages::unified_inbox_conversations(
                &reader.conn,
                limit,
                offset,
            ));
        }
        ReadRequest::FolderMessages {
            folder,
            limit,
            offset,
            resp,
        } => {
            let _ = resp.send(messages::folder_messages(
                &reader.conn,
                folder,
                limit,
                offset,
            ));
        }
        ReadRequest::FolderConversations { folder, resp } => {
            let _ = resp.send(messages::folder_conversations(&reader.conn, folder));
        }
        ReadRequest::ThreadEnvelopes {
            thread_id,
            folder_ids,
            include_sent,
            resp,
        } => {
            let folder_refs = folder_ids.as_deref();
            let _ = resp.send(messages::thread_envelopes(
                &reader.conn,
                thread_id,
                folder_refs,
                include_sent,
            ));
        }
        ReadRequest::ThreadMessagesAllFolders {
            account_id,
            thread_id,
            resp,
        } => {
            let _ = resp.send(messages::thread_messages_all_folders(
                &reader.conn,
                account_id,
                thread_id,
            ));
        }
        ReadRequest::MessagesWithAddress {
            account_id,
            email,
            resp,
        } => {
            let _ = resp.send(messages::messages_with_address(
                &reader.conn,
                account_id,
                &email,
            ));
        }
        ReadRequest::ListSentRecipients { resp } => {
            let _ = resp.send(messages::list_sent_recipients(&reader.conn));
        }
        ReadRequest::GetMessage { id, resp } => {
            let _ = resp.send(messages::get_message(&reader.conn, id));
        }
        ReadRequest::MessagesByUids { folder, uids, resp } => {
            let _ = resp.send(messages::messages_by_uids(&reader.conn, folder, &uids));
        }
        ReadRequest::MessageRfc822Size { id, resp } => {
            let _ = resp.send(messages::message_rfc822_size(&reader.conn, id));
        }
        ReadRequest::MessageLocators { ids, resp } => {
            let _ = resp.send(messages::message_locators(&reader.conn, &ids));
        }
        ReadRequest::LiveMessageIdHdrs { folder, resp } => {
            let _ = resp.send(messages::live_message_id_hdrs(&reader.conn, folder));
        }
        ReadRequest::HasLiveCopyElsewhere { id, resp } => {
            let _ = resp.send(messages::has_live_copy_elsewhere(&reader.conn, id));
        }
        ReadRequest::LiveSiblingIds { id, resp } => {
            let _ = resp.send(messages::live_sibling_ids(&reader.conn, id));
        }
        ReadRequest::EnvelopesByIds { ids, resp } => {
            let _ = resp.send(messages::envelopes_by_ids(&reader.conn, &ids));
        }
        ReadRequest::UnreadCounts { resp } => {
            let _ = resp.send(messages::unread_counts(&reader.conn));
        }
        ReadRequest::FolderUnreadCount { folder, resp } => {
            let _ = resp.send(messages::folder_unread_count(&reader.conn, folder));
        }
        ReadRequest::FolderLiveCount { folder, resp } => {
            let _ = resp.send(messages::folder_live_count(&reader.conn, folder));
        }
        ReadRequest::MessagesMissingBody {
            folder,
            window,
            resp,
        } => {
            let _ = resp.send(messages::messages_missing_body(
                &reader.conn,
                folder,
                window,
            ));
        }
        ReadRequest::ReactionsForTarget {
            account,
            message_id_hdr,
            resp,
        } => {
            let _ = resp.send(messages::reactions_for_target(
                &reader.conn,
                account,
                &message_id_hdr,
            ));
        }
        ReadRequest::ReactionTargetMessage {
            account,
            message_id_hdr,
            resp,
        } => {
            let _ = resp.send(messages::reaction_target_message(
                &reader.conn,
                account,
                &message_id_hdr,
            ));
        }
        ReadRequest::ReactionTargetHdr { id, resp } => {
            let _ = resp.send(messages::reaction_target_hdr(&reader.conn, id));
        }
        ReadRequest::UidsMissingRoutingHeaders {
            folder,
            limit,
            resp,
        } => {
            let _ = resp.send(messages::uids_missing_routing_headers(
                &reader.conn,
                folder,
                limit,
            ));
        }
        ReadRequest::SentStructureScanDone { account, resp } => {
            let _ = resp.send(messages::sent_structure_scan_done(&reader.conn, account));
        }
        ReadRequest::SentUidsMissingAttachmentFlag { folder, resp } => {
            let _ = resp.send(messages::sent_uids_missing_attachment_flag(
                &reader.conn,
                folder,
            ));
        }
        ReadRequest::UidsMissingRfc822Size {
            folder,
            limit,
            resp,
        } => {
            let _ = resp.send(messages::uids_missing_rfc822_size(
                &reader.conn,
                folder,
                limit,
            ));
        }
        ReadRequest::UidsMissingReplyHeaders {
            folder,
            limit,
            resp,
        } => {
            let _ = resp.send(messages::uids_missing_reply_headers(
                &reader.conn,
                folder,
                limit,
            ));
        }
        ReadRequest::FolderMessagesAll { folder, resp } => {
            let _ = resp.send(messages::folder_messages_all(&reader.conn, folder));
        }

        // --- attachments ---
        ReadRequest::LoadAttachment { sha256, resp } => {
            let _ = resp.send(attachments::load_attachment_bytes(
                &reader.attachments_dir,
                &sha256,
            ));
        }

        // --- search ---
        ReadRequest::Search {
            query,
            folder_ids,
            limit,
            resp,
        } => {
            let folder_refs = folder_ids.as_deref();
            let _ = resp.send(search::search(&reader.conn, &query, folder_refs, limit));
        }
        ReadRequest::SearchConversations {
            query,
            folder_ids,
            limit,
            resp,
        } => {
            let folder_refs = folder_ids.as_deref();
            let _ = resp.send(search::search_conversations(
                &reader.conn,
                &query,
                folder_refs,
                limit,
            ));
        }

        // --- remote_images ---
        ReadRequest::ListRemoteImageSenders { resp } => {
            let _ = resp.send(remote_images::list_senders(&reader.conn));
        }

        // --- contacts ---
        ReadRequest::ContactsSnapshot { resp } => {
            let _ = resp.send(contacts::snapshot(&reader.conn));
        }

        // --- rules ---
        ReadRequest::ListRules { account, resp } => {
            let _ = resp.send(rules::list_rules(&reader.conn, account));
        }
        ReadRequest::HasRuleRun { message_id, resp } => {
            let _ = resp.send(rules::has_rule_run(&reader.conn, message_id));
        }
        ReadRequest::RuleRunsFor { ids, resp } => {
            let _ = resp.send(rules::rule_runs_for(&reader.conn, &ids));
        }

        // --- queued_sends ---
        ReadRequest::ListQueuedSends { resp } => {
            let _ = resp.send(queued_sends::list_queued_sends(&reader.conn));
        }

        // --- pgp ---
        ReadRequest::ListPgpKeys { account, resp } => {
            let _ = resp.send(pgp::list_for_account(&reader.conn, account));
        }
        ReadRequest::KeysForEmails { emails, resp } => {
            let _ = resp.send(pgp::keys_for_emails(&reader.conn, &emails));
        }
        ReadRequest::GetPgpSignOutgoing { account, resp } => {
            let _ = resp.send(pgp::get_sign_outgoing(&reader.conn, account));
        }

        // --- identities ---
        ReadRequest::ListIdentities { account, resp } => {
            let _ = resp.send(identities::list_identities(&reader.conn, account));
        }

        // --- signatures ---
        ReadRequest::GetSignature { account, resp } => {
            let _ = resp.send(signatures::get_signature(&reader.conn, account));
        }

        // --- snippets ---
        ReadRequest::ListSnippets { resp } => {
            let _ = resp.send(snippets::list_snippets(&reader.conn));
        }

        // --- extensions ---
        ReadRequest::Extension { run, .. } => run(&reader.conn),
    }
}

pub(super) fn dispatch_write(state: &mut WriterState, req: WriteRequest) {
    match req {
        // --- accounts ---
        WriteRequest::UpsertAccount { account, resp } => {
            let _ = resp.send(accounts::upsert_account(&state.conn, &account));
        }
        WriteRequest::DeleteAccount { id, resp } => {
            let result = (|| {
                let hashes = attachments::attachment_sha256_for_account(&state.conn, id)?;
                accounts::delete_account(&state.conn, id)?;
                let _ = pgp::delete_all_for_account(&state.conn, id);
                if let Err(err) = attachments::delete_orphaned_attachment_files(
                    &state.conn,
                    &state.attachments_dir,
                    &hashes,
                ) {
                    tracing::warn!(
                        ?id,
                        error = %err,
                        "failed to garbage-collect attachment files after deleting account"
                    );
                }
                Ok(())
            })();
            let _ = resp.send(result);
        }
        WriteRequest::SetCatchAllDomains {
            account,
            domains,
            resp,
        } => {
            let _ = resp.send(accounts::set_catch_all_domains(
                &state.conn,
                account,
                &domains,
            ));
        }
        WriteRequest::SetSmtpEnvelopePolicy {
            account,
            policy,
            resp,
        } => {
            let _ = resp.send(accounts::set_smtp_envelope_policy(
                &state.conn,
                account,
                policy,
            ));
        }

        // --- folders ---
        WriteRequest::ReplaceFolders {
            account,
            folders,
            mode,
            resp,
        } => {
            let mode = mode.unwrap_or_else(|| folders::infer_replace_mode(&folders));
            let _ = resp.send(folders::replace_folders(
                &state.conn,
                account,
                &folders,
                mode,
            ));
        }
        WriteRequest::SetFolderUnread {
            folder,
            unread,
            resp,
        } => {
            let _ = resp.send(folders::set_folder_unread(&state.conn, folder, unread));
        }
        WriteRequest::RenameFolderTree {
            account,
            pairs,
            resp,
        } => {
            let _ = resp.send(folders::rename_folder_tree(&state.conn, account, &pairs));
        }

        // --- messages ---
        WriteRequest::UpsertEnvelopes {
            envs,
            body_text,
            resp,
        } => {
            let body_ref = body_text.as_deref();
            let _ = resp.send(messages::upsert_envelopes(&state.conn, &envs, body_ref));
        }
        WriteRequest::SetFlags {
            id,
            seen,
            flagged,
            resp,
        } => {
            let _ = resp.send(messages::set_flags(&state.conn, id, seen, flagged));
        }
        WriteRequest::SetFlagsMany {
            ids,
            seen,
            flagged,
            resp,
        } => {
            let _ = resp.send(messages::set_flags_many(&state.conn, &ids, seen, flagged));
        }
        WriteRequest::MarkDeleted { id, resp } => {
            let _ = resp.send(messages::mark_deleted(&state.conn, id));
        }
        WriteRequest::MarkDeletedByUids { folder, uids, resp } => {
            let _ = resp.send(messages::mark_deleted_by_uids(&state.conn, folder, &uids));
        }
        WriteRequest::MirrorFlagsToCopies {
            source,
            flags,
            resp,
        } => {
            let _ = resp.send(messages::mirror_flags_to_copies(
                &state.conn,
                &source,
                flags,
            ));
        }
        WriteRequest::RestoreMirroredCopies { copies, resp } => {
            let _ = resp.send(messages::restore_mirrored_copies(&state.conn, &copies));
        }
        WriteRequest::ApplyServerFlags {
            folder,
            flags,
            resp,
        } => {
            let _ = resp.send(messages::apply_server_flags(&state.conn, folder, &flags));
        }
        WriteRequest::RehomeMessage {
            id,
            folder,
            uid,
            deleted_at,
            resp,
        } => {
            let _ = resp.send(messages::rehome_message(
                &state.conn,
                id,
                folder,
                uid,
                deleted_at,
            ));
        }
        WriteRequest::SetMessageIdHdrs { folder, rows, resp } => {
            let _ = resp.send(messages::set_message_id_hdrs(&state.conn, folder, &rows));
        }
        WriteRequest::SetRfc822Sizes { folder, rows, resp } => {
            let _ = resp.send(messages::set_rfc822_sizes(&state.conn, folder, &rows));
        }
        WriteRequest::SetReplyHeaders { folder, rows, resp } => {
            let _ = resp.send(messages::set_reply_headers(&state.conn, folder, &rows));
        }
        WriteRequest::SetRoutingHeaders { folder, rows, resp } => {
            let _ = resp.send(messages::set_routing_headers(&state.conn, folder, &rows));
        }
        WriteRequest::SetThreadingHdrs { folder, rows, resp } => {
            let _ = resp.send(messages::set_threading_hdrs(&state.conn, folder, &rows));
        }
        WriteRequest::MarkFolderSeen { folder, resp } => {
            let _ = resp.send(messages::mark_folder_seen(&state.conn, folder));
        }
        WriteRequest::MarkFolderDeleted { folder, resp } => {
            let _ = resp.send(messages::mark_folder_deleted(&state.conn, folder));
        }
        WriteRequest::SaveMessageBody {
            id,
            body_text,
            body_html,
            pgp_status,
            resp,
        } => {
            let _ = resp.send(messages::save_message_body(
                &state.conn,
                id,
                body_text.as_deref(),
                body_html.as_deref(),
                pgp_status.as_ref(),
            ));
        }
        WriteRequest::PurgeMissingUids {
            folder,
            live_uids,
            resp,
        } => {
            let _ = resp.send(messages::purge_missing_uids(
                &state.conn,
                folder,
                &live_uids,
            ));
        }
        WriteRequest::SetAllowRemoteImages { id, allow, resp } => {
            let _ = resp.send(messages::set_allow_remote_images(&state.conn, id, allow));
        }
        WriteRequest::SaveListUnsubscribe { id, value, resp } => {
            let _ = resp.send(messages::save_list_unsubscribe(
                &state.conn,
                id,
                value.as_ref(),
            ));
        }
        WriteRequest::SaveReaction {
            id,
            emoji,
            target_hdr,
            resp,
        } => {
            let _ = resp.send(messages::save_reaction(
                &state.conn,
                id,
                emoji.as_deref(),
                target_hdr.as_deref(),
            ));
        }
        WriteRequest::SetHasAttachmentsTrue { id, resp } => {
            let _ = resp.send(messages::set_has_attachments_true(&state.conn, id));
        }
        WriteRequest::MarkSentStructureScanDone { account, resp } => {
            let _ = resp.send(messages::mark_sent_structure_scan_done(
                &state.conn,
                account,
            ));
        }
        WriteRequest::NextLocalUid { folder_id, resp } => {
            let _ = resp.send(messages::next_local_uid(&state.conn, folder_id));
        }
        WriteRequest::SetListBumpUtc { id, when, resp } => {
            let _ = resp.send(messages::set_list_bump_utc(&state.conn, id, when));
        }

        // --- attachments ---
        WriteRequest::SaveAttachment { meta, bytes, resp } => {
            let _ = resp.send(attachments::save_attachment(
                &state.conn,
                &state.attachments_dir,
                meta,
                &bytes,
            ));
        }
        WriteRequest::DeleteAttachment {
            message_id,
            sha256,
            resp,
        } => {
            let _ = resp.send(attachments::delete_attachment(
                &state.conn,
                &state.attachments_dir,
                message_id,
                &sha256,
            ));
        }

        // --- remote_images ---
        WriteRequest::SetRemoteImageSender { email, allow, resp } => {
            let _ = resp.send(remote_images::set_sender(&state.conn, &email, allow));
        }

        // --- contacts ---
        WriteRequest::UpsertContact { draft, resp } => {
            let _ = resp.send(contacts::upsert_contact(&state.conn, draft));
        }
        WriteRequest::DeleteContacts { ids, resp } => {
            let _ = resp.send(contacts::delete_contacts(&state.conn, &ids));
        }
        WriteRequest::UpsertGroup { draft, resp } => {
            let _ = resp.send(contacts::upsert_group(&state.conn, draft));
        }
        WriteRequest::DeleteGroup { id, resp } => {
            let _ = resp.send(contacts::delete_group(&state.conn, id));
        }
        WriteRequest::PromoteContact { id, resp } => {
            let _ = resp.send(contacts::promote_contact(&state.conn, id));
        }
        WriteRequest::BlockContact { id, resp } => {
            let _ = resp.send(contacts::block_contact(&state.conn, id));
        }
        WriteRequest::AllowSender {
            email,
            display_name,
            resp,
        } => {
            let _ = resp.send(contacts::allow_sender(&state.conn, &email, &display_name));
        }
        WriteRequest::CollectOutgoing {
            account_email,
            addresses,
            resp,
        } => {
            let _ = resp.send(contacts::collect_outgoing(
                &state.conn,
                &account_email,
                &addresses,
            ));
        }

        // --- rules ---
        WriteRequest::UpsertRule { rule, resp } => {
            let _ = resp.send(rules::upsert_rule(&state.conn, &rule));
        }
        WriteRequest::DeleteRule { id, resp } => {
            let _ = resp.send(rules::delete_rule(&state.conn, id));
        }
        WriteRequest::ReorderRules { account, ids, resp } => {
            let _ = resp.send(rules::reorder_rules(&state.conn, account, &ids));
        }
        WriteRequest::SetRuleExecution {
            id,
            execution,
            resp,
        } => {
            let _ = resp.send(rules::set_rule_execution(&state.conn, id, execution));
        }
        WriteRequest::MarkRuleRun { message_id, resp } => {
            let _ = resp.send(rules::mark_rule_run(&state.conn, message_id));
        }
        WriteRequest::MarkRuleRuns { ids, resp } => {
            let _ = resp.send(rules::mark_rule_runs(&state.conn, &ids));
        }

        // --- queued_sends ---
        WriteRequest::InsertQueuedSend { queued, resp } => {
            let _ = resp.send(queued_sends::insert_queued_send(&state.conn, &queued));
        }
        WriteRequest::DeleteQueuedSend { id, resp } => {
            let _ = resp.send(queued_sends::delete_queued_send(&state.conn, id));
        }

        // --- pgp ---
        WriteRequest::UpsertPgpKey {
            fingerprint,
            emails,
            armored,
            source,
            account_id,
            resp,
        } => {
            let _ = resp.send(pgp::upsert_key(
                &state.conn,
                &fingerprint,
                &emails,
                &armored,
                source,
                account_id,
            ));
        }
        WriteRequest::DeleteOwnPgpKey { account, resp } => {
            let _ = resp.send(pgp::delete_own_for_account(&state.conn, account));
        }
        WriteRequest::DeleteAllPgpKeys { account, resp } => {
            let _ = resp.send(pgp::delete_all_for_account(&state.conn, account));
        }
        WriteRequest::SetPgpSignOutgoing {
            account,
            enabled,
            resp,
        } => {
            let _ = resp.send(pgp::set_sign_outgoing(&state.conn, account, enabled));
        }

        // --- identities ---
        WriteRequest::UpsertIdentity { identity, resp } => {
            let _ = resp.send(identities::upsert_identity(&state.conn, &identity));
        }
        WriteRequest::DeleteIdentity { id, resp } => {
            let _ = resp.send(identities::delete_identity(&state.conn, id));
        }

        // --- signatures ---
        WriteRequest::UpsertSignature { signature, resp } => {
            let _ = resp.send(signatures::upsert_signature(&state.conn, &signature));
        }
        WriteRequest::DeleteSignature { account, resp } => {
            let _ = resp.send(signatures::delete_signature(&state.conn, account));
        }

        // --- snippets ---
        WriteRequest::UpsertSnippet { snippet, resp } => {
            let _ = resp.send(snippets::upsert_snippet(&state.conn, &snippet));
        }
        WriteRequest::DeleteSnippet { id, resp } => {
            let _ = resp.send(snippets::delete_snippet(&state.conn, id));
        }

        // --- extensions ---
        WriteRequest::Extension { run, .. } => run(&state.conn),
    }
}
