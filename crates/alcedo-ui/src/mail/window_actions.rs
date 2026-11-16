//! Window-level gio actions and drag-and-drop onto folders.

use super::*;

pub(super) fn mail_window_action(
    window: &adw::ApplicationWindow,
    name: &str,
    sender: &relm4::Sender<MailMsg>,
    msg: MailMsg,
) -> gio::SimpleAction {
    let action = gio::SimpleAction::new(name, None);
    let sender = sender.clone();
    action.connect_activate(move |_, _| sender.emit(msg.clone()));
    window.add_action(&action);
    action
}

pub(super) fn envelope_from_sources<'a>(
    envelopes: &'a [Envelope],
    source_envelopes: &'a [Envelope],
    id: MessageId,
) -> Option<&'a Envelope> {
    envelopes
        .iter()
        .find(|env| env.id == id)
        .or_else(|| source_envelopes.iter().find(|env| env.id == id))
}

pub(super) fn folder_from_map(
    folders: &HashMap<AccountId, Vec<Folder>>,
    folder_id: FolderId,
) -> Option<&Folder> {
    folders
        .values()
        .flatten()
        .find(|folder| folder.id == folder_id)
}

pub(super) fn can_drop_messages_on_folder_data(
    folders: &HashMap<AccountId, Vec<Folder>>,
    envelopes: &[Envelope],
    source_envelopes: &[Envelope],
    dest: FolderId,
    ids: &[MessageId],
) -> bool {
    if ids.is_empty() {
        return false;
    }
    let Some(dest_folder) = folder_from_map(folders, dest) else {
        return false;
    };
    if !dest_folder.selectable {
        return false;
    }
    ids.iter().any(|id| {
        let Some(env) = envelope_from_sources(envelopes, source_envelopes, *id) else {
            return false;
        };
        env.account_id == dest_folder.account_id && env.folder_id != dest
    })
}

pub(super) fn setup_mail_window_actions(
    window: &adw::ApplicationWindow,
    sender: &relm4::Sender<MailMsg>,
) -> MailActions {
    mail_window_action(window, "mail-message-rules", sender, MailMsg::OpenRules);
    let extension_actions = crate::extension::message_actions()
        .iter()
        .enumerate()
        .map(|(index, action)| {
            mail_window_action(
                window,
                action.action_name,
                sender,
                MailMsg::MessageAction(index),
            )
        })
        .collect();
    mail_window_action(
        window,
        "mail-go-unified",
        sender,
        MailMsg::SelectUnifiedInbox,
    );
    MailActions {
        select_all: mail_window_action(
            window,
            "mail-select-all",
            sender,
            MailMsg::SelectAllMessages,
        ),
        mark_read: mail_window_action(window, "mail-mark-read", sender, MailMsg::MarkSelectionRead),
        mark_unread: mail_window_action(
            window,
            "mail-mark-unread",
            sender,
            MailMsg::MarkSelectionUnread,
        ),
        star: mail_window_action(window, "mail-star", sender, MailMsg::StarSelection),
        unstar: mail_window_action(window, "mail-unstar", sender, MailMsg::UnstarSelection),
        mark_spam: mail_window_action(window, "mail-mark-spam", sender, MailMsg::MarkSelectionSpam),
        mark_not_spam: mail_window_action(
            window,
            "mail-mark-not-spam",
            sender,
            MailMsg::MarkSelectionNotSpam,
        ),
        choose_move_folder: mail_window_action(
            window,
            "mail-choose-move-folder",
            sender,
            MailMsg::ChooseMoveFolder,
        ),
        choose_copy_folder: mail_window_action(
            window,
            "mail-choose-copy-folder",
            sender,
            MailMsg::ChooseCopyFolder,
        ),
        archive: mail_window_action(window, "mail-archive", sender, MailMsg::ArchiveSelected),
        delete: mail_window_action(window, "mail-delete", sender, MailMsg::DeleteSelected),
        delete_permanently: mail_window_action(
            window,
            "mail-delete-permanently",
            sender,
            MailMsg::DeletePermanentlySelected,
        ),
        reply: mail_window_action(window, "mail-reply", sender, MailMsg::Reply),
        reply_all: mail_window_action(window, "mail-reply-all", sender, MailMsg::ReplyAll),
        forward: mail_window_action(window, "mail-forward", sender, MailMsg::Forward),
        forward_window: mail_window_action(
            window,
            "mail-forward-window",
            sender,
            MailMsg::ForwardInWindow,
        ),
        reply_window: mail_window_action(
            window,
            "mail-reply-window",
            sender,
            MailMsg::ReplyInWindow,
        ),
        reply_all_window: mail_window_action(
            window,
            "mail-reply-all-window",
            sender,
            MailMsg::ReplyAllInWindow,
        ),
        sync_folder: mail_window_action(window, "mail-sync-folder", sender, MailMsg::RequestSync),
        folder_ctx_sync: mail_window_action(
            window,
            "folder-ctx-sync",
            sender,
            MailMsg::FolderContextSync,
        ),
        folder_ctx_mark_read: mail_window_action(
            window,
            "folder-ctx-mark-read",
            sender,
            MailMsg::FolderContextMarkRead,
        ),
        folder_ctx_search: mail_window_action(
            window,
            "folder-ctx-search",
            sender,
            MailMsg::FolderContextSearch,
        ),
        folder_ctx_toggle_expand: mail_window_action(
            window,
            "folder-ctx-toggle-expand",
            sender,
            MailMsg::FolderContextToggleExpand,
        ),
        folder_ctx_empty: mail_window_action(
            window,
            "folder-ctx-empty",
            sender,
            MailMsg::FolderContextEmpty,
        ),
        folder_ctx_delete_on_or_before: mail_window_action(
            window,
            "folder-ctx-delete-on-or-before",
            sender,
            MailMsg::FolderContextDeleteOnOrBefore,
        ),
        new_folder: mail_window_action(window, "mail-new-folder", sender, MailMsg::NewFolder),
        edit_folder: mail_window_action(window, "mail-edit-folder", sender, MailMsg::EditFolder),
        folder_ctx_new: mail_window_action(
            window,
            "folder-ctx-new",
            sender,
            MailMsg::FolderContextNew,
        ),
        folder_ctx_account_settings: mail_window_action(
            window,
            "folder-ctx-account-settings",
            sender,
            MailMsg::FolderContextAccountSettings,
        ),
        folder_ctx_toggle_account_hidden: mail_window_action(
            window,
            "folder-ctx-toggle-account-hidden",
            sender,
            MailMsg::FolderContextToggleAccountHidden,
        ),
        folder_ctx_rename: mail_window_action(
            window,
            "folder-ctx-rename",
            sender,
            MailMsg::FolderContextRename,
        ),
        folder_ctx_delete: mail_window_action(
            window,
            "folder-ctx-delete",
            sender,
            MailMsg::FolderContextDelete,
        ),
        ext_view_item: {
            let action = gio::SimpleAction::new("ext-view-item", Some(glib::VariantTy::INT32));
            let sender = sender.clone();
            action.connect_activate(move |_, param| {
                if let Some(index) = param.and_then(|value| value.get::<i32>()) {
                    sender.emit(MailMsg::ExtensionViewMenuItem(index));
                }
            });
            window.add_action(&action);
            action
        },
        create_rule: mail_window_action(
            window,
            "mail-create-rule",
            sender,
            MailMsg::CreateRuleFromMessage,
        ),
        extension_actions,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mail::test_support::{envelope, sample_folder};

    fn envelope_in_folder(id: i64, account: u128, folder_id: i64) -> Envelope {
        let mut env = envelope("a@b.com", &[], &[]);
        env.id = MessageId(id);
        env.account_id = AccountId(Uuid::from_u128(account));
        env.folder_id = FolderId(folder_id);
        env
    }

    fn folders_with_dest(dest_id: i64, selectable: bool) -> HashMap<AccountId, Vec<Folder>> {
        let account_id = AccountId(Uuid::from_u128(1));
        let mut folders = HashMap::new();
        folders.insert(
            account_id,
            vec![sample_folder(1, SpecialUse::Inbox, true), {
                let mut dest = sample_folder(dest_id, SpecialUse::Other, selectable);
                dest.imap_name = "Projects".to_string();
                dest
            }],
        );
        folders
    }

    #[test]
    fn can_drop_messages_on_folder_data_empty_ids() {
        let folders = folders_with_dest(2, true);
        assert!(!can_drop_messages_on_folder_data(
            &folders,
            &[],
            &[],
            FolderId(2),
            &[],
        ));
    }

    #[test]
    fn can_drop_messages_on_folder_data_missing_dest() {
        let folders = folders_with_dest(2, true);
        let env = envelope_in_folder(10, 1, 1);
        assert!(!can_drop_messages_on_folder_data(
            &folders,
            &[env],
            &[],
            FolderId(99),
            &[MessageId(10)],
        ));
    }

    #[test]
    fn can_drop_messages_on_folder_data_non_selectable_dest() {
        let folders = folders_with_dest(2, false);
        let env = envelope_in_folder(10, 1, 1);
        assert!(!can_drop_messages_on_folder_data(
            &folders,
            &[env],
            &[],
            FolderId(2),
            &[MessageId(10)],
        ));
    }

    #[test]
    fn can_drop_messages_on_folder_data_already_in_dest() {
        let folders = folders_with_dest(2, true);
        let env = envelope_in_folder(10, 1, 2);
        assert!(!can_drop_messages_on_folder_data(
            &folders,
            &[env],
            &[],
            FolderId(2),
            &[MessageId(10)],
        ));
    }

    #[test]
    fn can_drop_messages_on_folder_data_same_account_not_in_dest() {
        let folders = folders_with_dest(2, true);
        let env = envelope_in_folder(10, 1, 1);
        assert!(can_drop_messages_on_folder_data(
            &folders,
            &[env],
            &[],
            FolderId(2),
            &[MessageId(10)],
        ));
    }

    #[test]
    fn can_drop_messages_on_folder_data_mixed_any_match() {
        let folders = folders_with_dest(2, true);
        let valid = envelope_in_folder(10, 1, 1);
        let other_account = envelope_in_folder(11, 2, 1);
        assert!(can_drop_messages_on_folder_data(
            &folders,
            &[valid, other_account],
            &[],
            FolderId(2),
            &[MessageId(10), MessageId(11)],
        ));
    }
}
