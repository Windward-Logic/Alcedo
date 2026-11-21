//! Mail canvas: folder pane, message list, reader, and composer.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::{Duration, Instant};

use alcedo_core::byte_lru::ByteLru;
use alcedo_core::{
    Account, AccountDraft, AccountId, AccountIdentity, AccountSettingsUpdate, AccountSignature,
    Address, AuthMethod, BodyAttachment, ComposeDraft, ComposeFormat, ComposeFromChoice, ContactId,
    ContactStatus, Conversation, Envelope, EnvelopeListKind, ExtensionListId, Folder, FolderId,
    FolderToken, GroupedReaction, IdentityId, ImapDraftRef, ListUnsubscribe, MAX_COMPOSE_INLINES,
    MessageId, MessageListMode, MessageRule, PgpAccountState, PgpStatus, QueuedSend, QueuedSendId,
    ReactionPerson, ReplyFrom, ReplyMode, ReplyPlanner, RuleAction, RuleCondition, RuleField,
    RuleId, RuleMatchMode, RuleOp, ScopeBake, SearchQuery, SelfAddresses, SendState,
    ServerRulesPreview, ServerRulesStatus, Snippet, SnippetId, SpecialUse, SyncEvent, ThreadId,
    UiCommand, apply_is_token, collect_outgoing_into, descendant_imap_names,
    effective_mailbox_delimiter, encrypted_reaction_block, ensure_scope_in_query,
    expand_snippet_placeholders, folder_is_mutable, group_envelopes, group_reactions,
    hide_special_in_unscoped_search, is_keyring_auth_message, is_single_reaction_emoji,
    join_mailbox_path, looks_like_network_failure, mime_from_for_draft, nested_imap_names,
    normalize_sender_email, parse_address_token, parse_search_query, query_has_folder_operator,
    reaction_block_reason, reaction_bodies, reaction_from_display_name, reaction_reader_body,
    rename_imap_map, should_load_remote_images, should_toast_sync_failure,
};
use alcedo_html::{
    apply_signature, cid_map_key, looks_like_generated_plaintext_html, looks_like_html,
    remove_inline_imgs, rewrite_cids, sanitize_compose_html, signature_inlines_for_body,
};
use alcedo_widgets::CidCache;
use chrono::{Local, NaiveDate, Utc};
use relm4::Sender;
use relm4::adw;
use relm4::adw::prelude::*;
use relm4::gtk;
use relm4::gtk::gio;
use relm4::gtk::glib;
use relm4::prelude::*;
use tokio::runtime::Handle;
use tokio::sync::oneshot;
#[cfg(test)]
use uuid::Uuid;

use crate::account_settings::{
    apply_settings_update_to_account, focus_open_account_password, mail_oauth_pkce,
    mark_account_saved, open_account_id, present_account_editor, refresh_account_rows,
    refresh_catch_all_subpage, refresh_envelope_policy_subpage, refresh_identities_subpage,
    refresh_pgp_subpage, refresh_signature_subpage,
};
use crate::activity_log::{self, ActivityLog, ActivityOutcome};
use crate::add_account::build_add_account_dialog;
use crate::address_popover::show_address_popover;
use crate::app::present_main_window;
use crate::auth_banner::{
    SignInStep, offline_banner_title, sign_in_button_label, unnamed_offline_banner_title,
};
use crate::command_palette::{CommandPaletteSpec, PaletteActivation, open_command_palette};
use crate::compose_draft_store::{
    DraftSlot, RecoveredDraft, clear_compose_draft as clear_compose_draft_slot,
    forget_recovered_drafts, load_recovered_drafts,
};
use crate::compose_snippets::{open_snippet_editor, refresh_snippets_prefs};
use crate::composer::{
    ComposeSendChrome, ComposerHooks, ComposerWidgets, ComposerWindow, apply_compose_draft,
    apply_compose_plain_spacing, build_composer_page, clear_composer, compose_draft_from_widgets,
    composer_has_draft, confirm_discard_composer, flush_compose_draft_then, focus_composer_to,
    imap_draft_has_server_uid, open_composer_window, ready_to_send, refresh_pgp_disclosure,
    sync_composer_from_accounts,
};
use crate::folder_bar::{
    FolderRestoreMatch, MailView, account_added_toast_title, bind_folder_bar,
    bind_folder_pane_empty_context, build_account_switcher, build_folder_tree, everything_title,
    find_default_folder, folder_display_name, folder_is_under, imap_ancestor_names,
    match_pending_folder_restore, populate_folder_bar, refresh_account_switcher,
    resolve_startup_selection, set_switcher_unread, should_abandon_pending_restore,
};
use crate::folder_editor::{
    FolderEditorMode, FolderEditorSpec, FolderEditorSubmit, open_folder_editor,
};
use crate::folder_pane_lists::FolderPaneLists;
use crate::folder_picker::{FolderPickerMode, FolderPickerSpec, open_folder_picker};
use crate::menu::{FolderMenuKind, FolderMenuSpec, folder_menu_entries, mail_selection_menu};
use crate::message_list::{
    AccountChipSnap, FolderLabel, ListEmptyView, MessageListView, MessageRow, NewMailChip,
    SelectionBar, apply_message_list_columns_for_scope, apply_newest_first_sort,
    apply_saved_message_list_sort, apply_trash_deleted_sort, build_messages_pane,
    find_message_position, find_visible_position, flatten_conversation_rows,
    focus_ignore_is_commit, focus_selection_ignored, ids_to_restore_on_refresh,
    ignore_focus_selection_briefly, list_selection_suppressed, message_at, mutate_row_at,
    new_list_view, note_list_selection_commit, open_id_following_bitset, populate_list,
    populate_list_preserved, reapply_message_list_column_visibility, refresh_follows_newer_bitset,
    refresh_should_restore_reader, remove_ids_from_list, restore_reader_row,
    schedule_sync_list_selection_to_reader, select_ids, selected_ids, selected_ids_from_selection,
    selected_member_ids, selection_count, set_account_chips, set_account_labels,
    set_context_message_handler, set_deleted_column_visible, set_flag_toggle_handler,
    set_folder_labels, set_thread_expand_handler, suppress_list_selection,
    sync_empty_state_from_count, sync_list_selection_to_reader, update_envelope_in_list,
};
use crate::message_window::{MessageWindow, open_message_window};
use crate::notifications;
use crate::preferences::PreferencesUi;
use crate::reader::{
    ReaderPlaceholder, ReaderWidgets, apply_theme as apply_reader_theme, build_reader_page,
    clear_reader, popup_reaction_popover, prepare_reaction_popover, reload_reader_document,
    set_react_enabled, set_reader_actions_sensitive, show_reaction_chips, show_reader_body_loading,
    show_reader_placeholder, sync_reader_star, update_reader, update_reader_autocrypt_banner,
    update_reader_headers, update_reader_pgp_banner, update_unsubscribe_button,
    wire_reader_pgp_banner_accept,
};
use crate::recipient_row::ContactBook;
use crate::rule_editor::{RuleEditorSpec, open_rule_editor};
use crate::rules::{RulesDialogHandle, RulesDialogSpec, open_rules_dialog, refresh_rules_dialog};
use crate::search_autocomplete::attach_search_autocomplete;
use crate::search_bar::{SearchFilterBar, SearchScopeKind, set_last_query_for_toggles};
use crate::settings::{
    FolderPaneAccounts, LastFolder, ListDensity, MessageListColumnScope, ReadingPane,
    account_order, apply_list_density, clamp_folder_pane_width, cleanup_verification_codes,
    collapsed_accounts, collapsed_folders, collapsed_personal_folders, compose_format_for_account,
    editor_font_size, effective_tracking_pixel_url, ensure_message_list_mode_default,
    folder_pane_accounts, folder_pane_width, folders_docked, hidden_accounts, last_account,
    last_folder, list_density, load_remote_images, message_list_mode, on_editor_font_size_changed,
    on_folder_pane_accounts_changed, on_list_density_changed, on_load_remote_images_changed,
    on_message_list_mode_changed, on_reader_font_size_changed, on_reading_pane_changed,
    on_show_verification_codes_changed, push_recent_move_folder, reading_pane, recent_move_folders,
    request_mdn, set_account_order, set_collapsed_accounts, set_collapsed_folders,
    set_collapsed_personal_folders, set_folder_pane_width, set_hidden_accounts, set_last_account,
    set_last_folder, show_verification_codes,
};
use crate::source_viewer::{open_source_viewer, save_message_to_file, suggested_eml_filename};
use crate::status_bar::{FolderStats, SelectionInfo, StatusBar, StatusFolder};

mod account_sections;
mod accounts;
mod calendar;
mod chrome;
mod compose;
mod delete;
mod drafts;
mod folder_ops;
mod folder_peek;
mod folders;
mod layout;
mod list;
mod list_actions;
mod message_lookup;
mod notify;
mod reactions;
use reactions::ReactionOriginal;
mod invite_response;
pub use invite_response::InviteResponsePayload;
mod extension_views;
mod reader;
use self::extension_views::ViewLeave;
mod reply;
mod rules;
mod search;
mod send;
mod send_guard;
mod status;
mod sync_events;
#[cfg(test)]
mod test_support;
mod undo_move;
mod views;
mod welcome;
mod window_actions;

pub use self::calendar::mock_seed_meeting_invite;
use self::chrome::{apply_reading_pane, wire_folder_pane_persist};
pub use self::reactions::ReactionMsg;
use self::reader::{BodyCache, body_cache, mock_html_bodies};
use self::window_actions::setup_mail_window_actions;

/// Mail-space messages handled by [`MailModel`].
#[derive(Clone)]
pub enum MailMsg {
    Compose,
    ComposeMailto(String),
    ShowReader,
    ShowComposer,
    RefreshPreferences,
    RefreshStatusBar,
    SetPreferences(Rc<PreferencesUi>),
    ApplyTheme,
    ListDensityChanged(ListDensity),
    ListModeChanged(MessageListMode),
    ReaderFontSizeChanged,
    EditorFontSizeChanged,
    Sync(SyncEvent),
    Escape,
    ListSelectionChanged,
    ListActivated(u32),
    OpenMessage(MessageId),
    OpenNotificationMessage(MessageId),
    CopyNotifiedVerificationCode(MessageId),
    MarkNotifiedMessageRead(MessageId),
    DeleteNotifiedMessage(MessageId),
    OpenMessageWindow(MessageId),
    MessageWindowClosed(MessageId),
    LoadRemoteImages,
    LoadRemoteImagesFor(MessageId),
    Unsubscribe,
    UnsubscribeFor(MessageId),
    ExecuteUnsubscribe {
        id: MessageId,
        list: ListUnsubscribe,
        mock: bool,
        cmd: Sender<UiCommand>,
    },
    AlwaysLoadRemoteFromSender,
    AlwaysLoadRemoteFromSenderFor(MessageId),
    SetSenderRemoteImages {
        email: String,
        allow: bool,
    },
    RemoteImagesPreferenceChanged,
    VerificationCodesPreferenceChanged,
    Reply,
    ReplyAll,
    ReplyTo(MessageId),
    ReplyAllTo(MessageId),
    /// Reply to the List-Post address of the selected message.
    ReplyList,
    ReplyListTo(MessageId),
    InviteResponse(invite_response::InviteResponsePayload),
    Reaction(ReactionMsg),
    ReplyInWindow,
    ReplyAllInWindow,
    ReplyToInWindow(MessageId),
    ReplyAllToInWindow(MessageId),
    Forward,
    ForwardTo(MessageId),
    ForwardInWindow,
    ForwardToInWindow(MessageId),
    ViewMessageSource,
    SaveMessageToFile,
    ViewMessageSourceFor(MessageId),
    SaveMessageToFileFor(MessageId),
    SelectUnifiedInbox,
    SelectFolder(FolderId),
    ToggleFolderExpanded {
        account: AccountId,
        imap_name: String,
    },
    TogglePersonalFolders(AccountId),
    /// Collapse or expand the extension sidebar section with this title.
    ToggleExtensionSection(String),
    ReorderExtensionViews {
        dragged: ExtensionListId,
        target: ExtensionListId,
    },
    /// Run item `index` of the open extension view context menu.
    ExtensionViewMenuItem(i32),
    SelectAccount(AccountId),
    ToggleAccountExpanded(AccountId),
    ToggleHiddenAccountsExpanded,
    ReorderAccounts(Vec<AccountId>),
    FolderPaneAccountsChanged(FolderPaneAccounts),
    AccountHeaderContextMenu {
        account: AccountId,
        x: f64,
        y: f64,
    },
    AddAccount,
    CloseAddAccount,
    SubmitAddAccount(AccountDraft),
    UpdateAccount {
        id: AccountId,
        update: AccountSettingsUpdate,
    },
    RemoveAccount(AccountId),
    PgpGenerate {
        account: AccountId,
        passphrase: String,
    },
    PgpImport {
        account: AccountId,
        armored: String,
        passphrase: Option<String>,
    },
    PgpExportPublic(AccountId),
    PgpDeleteKey(AccountId),
    PgpSetSignOutgoing {
        account: AccountId,
        enabled: bool,
    },
    PgpAcceptPeer {
        account: AccountId,
        email: String,
        armored: String,
    },
    RefreshAccountPgp(AccountId),
    PaintAccountIdentities(AccountId),
    SetSmtpEnvelopePolicy {
        account: AccountId,
        same_domain_identity: bool,
    },
    RefreshSmtpEnvelopePolicy(AccountId),
    SetCatchAllDomains {
        account: AccountId,
        text: String,
    },
    SendCompose,
    SendComposerWindow(u64),
    ComposerWindowClosed(u64),
    PopOutComposer,
    PopOutComposerWithDraft(ComposeDraft),
    QueuedComposeSend {
        draft: ComposeDraft,
        window_id: Option<u64>,
    },
    ConfirmedComposeSend {
        draft: ComposeDraft,
        window_id: Option<u64>,
    },
    /// User confirmed an empty-subject or missing-attachment warning.
    PreSendConfirmed {
        draft: ComposeDraft,
        window_id: Option<u64>,
    },
    /// Take back a send still inside the undo-send delay.
    UndoSend,
    /// The undo-send timer elapsed; deliver the held draft.
    DelayedSendElapsed,
    Search(String),
    SearchScope(SearchScopeKind),
    SearchQueryRewrite(String),
    /// Run item `index` of the search bar's save menu.
    SearchSaveAction(usize),
    FocusSearchEntry,
    CancelCompose,
    /// Leave the composer after discard: do not IMAP-save.
    ComposerDiscarded,
    ToggleFlag(MessageId, bool),
    DeleteSelected,
    DeletePermanentlySelected,
    ConfirmPermanentDelete(Vec<MessageId>),
    ArchiveSelected,
    CopiedVerificationCode(MessageId),
    UndoDelete,
    /// Take back the last archive / move / spam.
    UndoMove,
    MarkSelectionSpam,
    MarkSelectionNotSpam,
    ChooseMoveFolder,
    ChooseCopyFolder,
    StartAccountMove {
        mode: FolderPickerMode,
        groups: Vec<(AccountId, Vec<MessageId>)>,
    },
    ApplySelectionToFolder {
        dest: FolderId,
        copy: bool,
    },
    DropMessagesOnFolder {
        dest: FolderId,
        ids: Vec<MessageId>,
        copy: bool,
    },
    /// Offer to restore composers autosaved by an earlier (crashed) session.
    RestoreSavedComposeDrafts(Vec<RecoveredDraft>),
    ApplyRestoredComposeDrafts(Vec<RecoveredDraft>),
    RestoreQueuedSends(Vec<QueuedSend>),
    ApplyQueuedSends(Vec<QueuedSend>),
    MarkSelectionRead,
    MarkSelectionUnread,
    /// Repaint folder and extension-view unread badges from in-memory counts.
    RefreshFolderBadges,
    StarSelection,
    UnstarSelection,
    SelectAllMessages,
    RowContextMenu {
        id: MessageId,
        x: f64,
        y: f64,
    },
    /// Run extension message action `index` on the context-menu message.
    MessageAction(usize),
    /// Run extension address action `index` for an address seen in `account`.
    AddressAction {
        index: usize,
        account_id: Option<AccountId>,
        email: String,
    },
    /// Run extension address action `index` from a composer's recipient chip.
    ComposerAddressAction {
        index: usize,
        window_id: Option<u64>,
        email: String,
    },
    /// Open an extension view (sidebar click or the extension itself).
    ShowExtensionView(ExtensionListId),
    ExtensionViewsChanged,
    ExtensionViewToggled(bool),
    ExtensionToast(String),
    FolderContextMenu {
        view: MailView,
        x: f64,
        y: f64,
    },
    FolderPaneContextMenu {
        x: f64,
        y: f64,
    },
    FolderContextSync,
    FolderContextMarkRead,
    FolderContextSearch,
    FolderContextToggleExpand,
    FolderContextEmpty,
    FolderContextDeleteOnOrBefore,
    FolderContextNew,
    FolderContextAccountSettings,
    FolderContextToggleAccountHidden,
    FolderContextRename,
    FolderContextDelete,
    NewFolder,
    EditFolder,
    ConfirmDeleteFolder(FolderId),
    FolderEditorSubmitCreate(FolderEditorSubmit),
    FolderEditorSubmitEdit {
        folder_id: FolderId,
        submit: FolderEditorSubmit,
    },
    FolderMenuClosed,
    ConfirmEmptyFolder(FolderId),
    ConfirmDeleteOnOrBefore {
        folder_id: FolderId,
        through: NaiveDate,
    },
    RequestSync,
    SyncAnimHoldElapsed,
    FocusSearch,
    OpenCommandPalette,
    /// The folder toggle: dock or undock the column, or open or close the
    /// overlay sidebar when the window is narrow.
    SetFoldersVisible(bool),
    /// The overlay sidebar opened or closed on its own (edge hover, scrim
    /// click, swipe); bring the toggle and header buttons in line.
    FolderOverlayChanged,
    /// A folder row was picked: slide an overlay sidebar away.
    DismissFolderOverlay,
    /// The pointer is on, or left, the part of the app rail that reveals a
    /// hidden folder sidebar.
    RailRevealHover(bool),
    /// The pointer entered or left the app rail, which keeps a revealed
    /// folder sidebar open.
    RailPointerInside(bool),
    /// Width breakpoints applied to the main window.
    SetAdaptiveLayout(alcedo_core::layout_policy::WidthBreakpoints),
    BackToMessages,
    ApplyTitleButtons,
    RefreshShortcutTooltips,
    InsertSearchOperator(&'static str),
    OpenRules,
    OpenRulesForAccount(AccountId),
    CreateRuleFromMessage,
    UpsertMessageRule {
        rule: MessageRule,
        run_folder: Option<FolderId>,
    },
    DeleteMessageRule(RuleId),
    ReorderMessageRules {
        account: AccountId,
        ids: Vec<RuleId>,
    },
    RunMessageRules {
        folder: FolderId,
        rule_ids: Option<Vec<RuleId>>,
    },
    AdoptServerRules(AccountId),
    RulesDialogClosed,
    ToggleThreadExpand(ThreadId),
    /// Hand a composer's draft to the extension's Send menu `choice`.
    SendWithExtension {
        choice: String,
        window_id: Option<u64>,
    },
    SendWithExtensionDraft {
        draft: ComposeDraft,
        choice: String,
        window_id: Option<u64>,
    },
    ExtensionToastWidget(adw::Toast),
    ExtensionOpenDraft(Box<ComposeDraft>),
    ExtensionNotify {
        id: &'static str,
        title: String,
        body: String,
        open_message: Option<MessageId>,
    },
    UpsertSignature(AccountSignature),
    UpsertIdentity(AccountIdentity),
    DeleteIdentity(IdentityId),
    UpsertSnippet(Snippet),
    DeleteSnippet(SnippetId),
    OpenSnippetEditor(Option<Snippet>),
    IncludeSignatureToggled(Option<u64>, bool),
    InsertSnippet(Option<u64>, SnippetId),
    ComposeDraftJsonPersist,
    ComposeDraftImapPersist,
    ComposeFromChanged(Option<u64>),
    /// Sign in again on the account the offline banner button is acting on.
    /// Password accounts open the editor; OAuth accounts start browser sign-in.
    ReconnectBrokenAccount,
    OAuthReauthFinished,
    OAuthReauthFailed(String),
}

impl std::fmt::Debug for MailMsg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MailMsg")
    }
}

/// Shell routing output from the mail child.
#[derive(Debug)]
pub enum MailOutput {
    ShowContacts {
        view: Option<ContactId>,
        new_from: Option<Address>,
    },
    UnifiedUnread(u32),
}

impl MailOutput {
    /// Map to app-shell routing messages.
    pub fn into_app_msg(self) -> crate::app::AppMsg {
        use crate::app::AppMsg;
        match self {
            Self::ShowContacts { view, new_from } => {
                if let Some(id) = view {
                    AppMsg::ViewContact(id)
                } else if let Some(addr) = new_from {
                    AppMsg::AddContactFromAddress(addr)
                } else {
                    AppMsg::ShowContacts
                }
            }
            Self::UnifiedUnread(count) => AppMsg::UnifiedUnread(count),
        }
    }
}

pub struct MailInit {
    pub mock: bool,
    pub cmd: Sender<UiCommand>,
    pub runtime: Handle,
    pub toasts: adw::ToastOverlay,
    pub status_bar: StatusBar,
    pub window: adw::ApplicationWindow,
    pub contact_book: Rc<RefCell<ContactBook>>,
    pub sent_recipients: Rc<RefCell<Vec<Address>>>,
    pub remote_image_senders: Rc<RefCell<HashSet<String>>>,
    pub cid_cache: CidCache,
    pub accounts: Vec<Account>,
    pub folders: HashMap<AccountId, Vec<Folder>>,
    pub envelopes: Vec<Envelope>,
    pub source_envelopes: Vec<Envelope>,
    pub bodies: HashMap<MessageId, String>,
    pub app_sender: Sender<crate::app::AppMsg>,
    pub activity: Rc<ActivityLog>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum MailContentPane {
    #[default]
    Reader,
    Composer,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum DeleteToast {
    #[default]
    Default,
    AfterVerificationCopy,
    Permanent,
}

struct PendingDeleteConfirm {
    ids: Vec<MessageId>,
    moved_ids: Vec<MessageId>,
    permanent_ids: Vec<MessageId>,
    undoable: Vec<(MessageId, FolderId)>,
    undo_envelopes: Vec<Envelope>,
    toast_kind: DeleteToast,
}

impl MailContentPane {
    fn stack_name(self) -> &'static str {
        match self {
            MailContentPane::Reader => "reader",
            MailContentPane::Composer => "composer",
        }
    }
}

/// Initial focus for a detached composer. Body and To are mutually exclusive.
#[derive(Clone, Copy, Debug)]
enum DetachedComposerFocus {
    Body,
    To,
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RawMessageAction {
    View,
    Save,
}

struct MailActions {
    select_all: gio::SimpleAction,
    mark_read: gio::SimpleAction,
    mark_unread: gio::SimpleAction,
    star: gio::SimpleAction,
    unstar: gio::SimpleAction,
    mark_spam: gio::SimpleAction,
    mark_not_spam: gio::SimpleAction,
    choose_move_folder: gio::SimpleAction,
    choose_copy_folder: gio::SimpleAction,
    archive: gio::SimpleAction,
    delete: gio::SimpleAction,
    delete_permanently: gio::SimpleAction,
    reply: gio::SimpleAction,
    reply_all: gio::SimpleAction,
    forward: gio::SimpleAction,
    forward_window: gio::SimpleAction,
    reply_window: gio::SimpleAction,
    reply_all_window: gio::SimpleAction,
    sync_folder: gio::SimpleAction,
    folder_ctx_sync: gio::SimpleAction,
    folder_ctx_mark_read: gio::SimpleAction,
    folder_ctx_search: gio::SimpleAction,
    folder_ctx_toggle_expand: gio::SimpleAction,
    folder_ctx_empty: gio::SimpleAction,
    folder_ctx_delete_on_or_before: gio::SimpleAction,
    new_folder: gio::SimpleAction,
    edit_folder: gio::SimpleAction,
    folder_ctx_new: gio::SimpleAction,
    folder_ctx_account_settings: gio::SimpleAction,
    folder_ctx_toggle_account_hidden: gio::SimpleAction,
    folder_ctx_rename: gio::SimpleAction,
    folder_ctx_delete: gio::SimpleAction,
    ext_view_item: gio::SimpleAction,
    create_rule: gio::SimpleAction,
    /// One per extension message action, in `extension::message_actions()` order.
    extension_actions: Vec<gio::SimpleAction>,
}

struct PendingAccountMoves {
    mode: FolderPickerMode,
    groups: Vec<(AccountId, Vec<MessageId>)>,
    next: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FolderMenuTarget {
    View(MailView),
    AccountRoot,
    Account(AccountId),
}

pub struct MailModel {
    mock: bool,
    cmd: Sender<UiCommand>,
    runtime: tokio::runtime::Handle,
    accounts: Vec<Account>,
    folders: HashMap<AccountId, Vec<Folder>>,
    selected_account: Option<AccountId>,
    pending_account_folder_select: Option<AccountId>,
    /// Email from a user Add Account submit; gates toast/activity (not startup hydrate).
    pending_account_add_email: Option<String>,
    pending_select_imap: Option<(AccountId, String)>,
    pending_restore: Option<(AccountId, String)>,
    /// Extension view to reopen once its extension lists it (last folder).
    pending_extension_restore: Option<ExtensionListId>,
    /// Items of the extension view context menu that is open.
    view_menu_items: Vec<crate::extension::ViewMenuItem>,
    /// Items of the search bar's save menu.
    search_save_items: Vec<crate::extension::SearchSaveAction>,
    pending_folder_sync: Option<FolderId>,
    /// Everything list load. Drives the row spinner and the
    /// empty-list pull icon until that view's list event arrives.
    pending_virtual_sync: Option<MailView>,
    context_sync_target: Option<MailView>,
    mail_view: MailView,
    pre_search_mail_view: Option<MailView>,
    envelopes: Vec<Envelope>,
    conversations: Vec<Conversation>,
    expanded_threads: HashSet<ThreadId>,
    list_mode: MessageListMode,
    source_envelopes: Vec<Envelope>,
    /// Plain-text and HTML bodies of recently opened messages, LRU-bounded
    /// (M9); `OpenMessage` reloads an evicted body from the store.
    bodies: BodyCache,
    html_bodies: BodyCache,
    attachments_by_id: HashMap<MessageId, Vec<BodyAttachment>>,
    attachments_pending_ids: HashSet<MessageId>,
    attachments_failed_ids: HashSet<MessageId>,
    unsubscribe_by_id: HashMap<MessageId, ListUnsubscribe>,
    unsubscribed_ids: HashSet<MessageId>,
    unsubscribing_ids: HashSet<MessageId>,
    pending_deleted_messages: HashMap<MessageId, FolderId>,
    pending_delete_confirm: Option<PendingDeleteConfirm>,
    cid_cache: CidCache,
    /// Per-message Content-ID → attachment sha256, for reader inline images.
    inline_cid_sha: HashMap<MessageId, HashMap<String, String>>,
    threading_hdrs: HashMap<MessageId, (Option<String>, Option<String>)>,
    reactions_by_target: HashMap<MessageId, Vec<ReactionPerson>>,
    optimistic_reactions: HashMap<MessageId, Vec<GroupedReaction>>,
    reaction_message_ids: HashSet<MessageId>,
    /// Original message for an open reaction, keyed by the reaction row.
    reaction_originals: HashMap<MessageId, ReactionOriginal>,
    pending_account_moves: Option<PendingAccountMoves>,
    selected: Option<MessageId>,
    mail_content_pane: MailContentPane,
    pending_send_draft: Option<ComposeDraft>,
    pending_send_queue_id: Option<QueuedSendId>,
    /// Reader still shows the pre-delete message. Selection already moved.
    reopen_reader_after_send: bool,
    /// Composer was closed when the send was queued, so later progress must not
    /// wipe a compose the user started while this one was going out.
    send_surface_released: bool,
    pending_reaction_target: Option<MessageId>,
    /// Organizer display name for a successful invite RSVP toast (cleared on send fail).
    pending_invite_response_organizer: Option<String>,
    /// Set while a send is queued. An autosave flush is async and can land after
    /// Send; its callback bails out while this is set.
    send_in_flight: Rc<Cell<bool>>,
    list_view: MessageListView,
    folder_lists: FolderPaneLists,
    add_account_dialog: adw::Dialog,
    preferences: Option<Rc<PreferencesUi>>,
    account_switcher: crate::folder_bar::AccountSwitcher,
    toasts: adw::ToastOverlay,
    activity: Rc<ActivityLog>,
    search_entry: gtk::SearchEntry,
    search_filter_bar: SearchFilterBar,
    search_scope: SearchScopeKind,
    search_complete_folders: Rc<RefCell<Vec<(String, Vec<Folder>)>>>,
    sync_btn: gtk::Button,
    status_bar: StatusBar,
    sync_text: Option<String>,
    last_logged_status: HashMap<AccountId, String>,
    last_sync_at: Option<chrono::DateTime<chrono::Utc>>,
    content_stack: gtk::Stack,
    folder_split: gtk::Paned,
    reader: ReaderWidgets,
    composer: ComposerWidgets,
    composer_send: ComposeSendChrome,
    unread_by_account: HashMap<AccountId, (u32, u32)>,
    last_unread_indicator: Option<u32>,
    collapsed_folders: HashMap<AccountId, HashSet<String>>,
    folder_pane_accounts: FolderPaneAccounts,
    collapsed_accounts: HashSet<AccountId>,
    collapsed_personal_folders: HashSet<AccountId>,
    hidden_accounts: HashSet<AccountId>,
    hidden_accounts_expanded: bool,
    account_order: Vec<AccountId>,
    messages_scrolled: gtk::ScrolledWindow,
    messages_body_stack: gtk::Stack,
    empty_status: adw::StatusPage,
    mail_paned: gtk::Paned,
    message_windows: HashMap<MessageId, MessageWindow>,
    raw_messages: HashMap<MessageId, Vec<u8>>,
    pending_raw_action: Option<(MessageId, RawMessageAction)>,
    composer_windows: HashMap<u64, ComposerWindow>,
    next_composer_window_id: u64,
    pending_send_window: Option<u64>,
    contact_book: Rc<RefCell<ContactBook>>,
    sent_recipients: Rc<RefCell<Vec<Address>>>,
    on_address_click: Rc<dyn Fn(&gtk::Widget, Address)>,
    mail_sender: Sender<MailMsg>,
    msg_sender: Sender<crate::app::AppMsg>,
    selection_bar: SelectionBar,
    new_mail_chip: NewMailChip,
    mail_actions: MailActions,
    mail_context_menu: gtk::PopoverMenu,
    folder_context_menu: gtk::Popover,
    folder_menu_target: Option<FolderMenuTarget>,
    sync_anim_hold: bool,
    sync_anim_timeout: Option<glib::SourceId>,
    remote_allowed_messages: Rc<RefCell<HashSet<MessageId>>>,
    remote_image_senders: Rc<RefCell<HashSet<String>>>,
    known_inbox_ids: HashSet<MessageId>,
    inbox_notify_ready: HashSet<FolderId>,
    pending_notification_message: Option<(MessageId, FolderId)>,
    pending_notification_tried_folder: bool,
    last_notified: HashMap<MessageId, FolderId>,
    notified_otp: HashMap<MessageId, (String, Envelope)>,
    pending_delete_toast: Option<adw::Toast>,
    pending_delete_folder: Option<FolderId>,
    pending_delete_through: Option<NaiveDate>,
    undo_delete_toast: Option<adw::Toast>,
    undo_delete_items: Vec<(MessageId, FolderId)>,
    undo_delete_envelopes: Vec<Envelope>,
    undo_move: Option<undo_move::PendingUndoMove>,
    deleted_column_on: bool,
    newest_first_sort_on: bool,
    context_message_id: Option<MessageId>,
    view_toggle: gtk::ToggleButton,
    ext_ctx: crate::extension::MailContext,
    rules: HashMap<AccountId, Vec<MessageRule>>,
    server_rules: HashMap<AccountId, ServerRulesStatus>,
    preview: HashMap<AccountId, Option<ServerRulesPreview>>,
    rules_dialog: Option<RulesDialogHandle>,
    pgp_by_account: HashMap<AccountId, PgpAccountState>,
    pgp_by_message: HashMap<MessageId, PgpStatus>,
    autocrypt_by_message: HashMap<MessageId, (AccountId, String, String)>,
    signatures: HashMap<AccountId, AccountSignature>,
    identities: HashMap<AccountId, Vec<AccountIdentity>>,
    /// Per-account catch-all domains; any address on them is the user's.
    catch_all_domains: HashMap<AccountId, Vec<String>>,
    snippets: Vec<Snippet>,
    compose_hooks: ComposerHooks,
    pending_imap_draft_open: Option<MessageId>,
    pending_popout_draft: Option<MessageId>,
    window: adw::ApplicationWindow,
    auth_required: HashMap<AccountId, String>,
    auth_banner: adw::Banner,
    oauth_reauth_in_flight: bool,
    started_at: Instant,
    network_error_toasted: HashSet<Option<AccountId>>,
    /// Mail layout vs the first-run welcome page.
    root_stack: gtk::Stack,
    /// Set on the first sync event that is not `AccountAdded`. Startup emits
    /// every saved account before anything else, so this means the list is complete.
    accounts_hydrated: bool,
    /// Send held for the undo-send delay, if one is running.
    delayed_send: Option<send_guard::DelayedSend>,
    layout: layout::MailLayout,
}

impl MailModel {
    fn collect_own_emails(&self) -> Vec<String> {
        let mut emails: Vec<String> = self.accounts.iter().map(|a| a.email.clone()).collect();
        for idents in self.identities.values() {
            for id in idents {
                emails.push(id.email.clone());
            }
        }
        emails.sort();
        emails.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
        emails
    }

    /// Tell extensions the user's addresses (accounts and identities).
    fn notify_own_addresses(&self) {
        let emails = self.collect_own_emails();
        for extension in crate::extension::registered() {
            extension.own_addresses_changed(&emails);
        }
    }
}

fn address_chip_popover_handler(
    contact_book: Rc<RefCell<ContactBook>>,
    remote_image_senders: Rc<RefCell<HashSet<String>>>,
    app_sender: Sender<crate::app::AppMsg>,
    on_address_action: Rc<dyn Fn(usize, String)>,
) -> Rc<dyn Fn(&gtk::Widget, Address)> {
    Rc::new(move |anchor, addr| {
        let sender_allowed = normalize_sender_email(&addr.email)
            .is_some_and(|email| remote_image_senders.borrow().contains(&email));
        show_address_popover(
            anchor,
            addr,
            &contact_book.borrow(),
            sender_allowed,
            app_sender.clone(),
            on_address_action.clone(),
        );
    })
}

#[relm4::component(pub)]
impl Component for MailModel {
    type Init = MailInit;
    type Input = MailMsg;
    type Output = MailOutput;
    type CommandOutput = ();

    view! {
        #[root]
        gtk::Stack {
            set_hexpand: true,
            set_vexpand: true,

            #[name = "mail_layout"]
            add_named[Some("mail")] = &gtk::Stack {
                set_hexpand: true,
                set_vexpand: true,
                set_transition_type: gtk::StackTransitionType::None,

            #[name = "folder_split"]
            add_named[Some("paned")] = &gtk::Paned {
            set_orientation: gtk::Orientation::Horizontal,
            set_wide_handle: true,
            set_position: 240,
            set_resize_start_child: false,
            set_shrink_start_child: false,
            set_resize_end_child: true,
            set_shrink_end_child: false,
            add_css_class: "alcedo-folder-paned",

            #[wrap(Some)]
            #[name = "folder_pane"]
            set_start_child = &gtk::Box {
                set_orientation: gtk::Orientation::Vertical,
                set_width_request: 180,
                add_css_class: "alcedo-folder-pane",

                #[name = "folder_top_list"]
                gtk::ListBox {
                    add_css_class: "navigation-sidebar",
                    add_css_class: "alcedo-folder-list-top",
                    set_selection_mode: gtk::SelectionMode::Single,
                    set_activate_on_single_click: true,
                },

                #[name = "folder_scroll"]
                gtk::ScrolledWindow {
                    set_vexpand: true,
                    set_policy: (gtk::PolicyType::Never, gtk::PolicyType::Automatic),

                    #[name = "folder_list"]
                    gtk::ListBox {
                        add_css_class: "navigation-sidebar",
                        set_selection_mode: gtk::SelectionMode::Single,
                        set_activate_on_single_click: true,
                    },
                },

                #[name = "folder_pinned_list"]
                gtk::ListBox {
                    add_css_class: "navigation-sidebar",
                    set_selection_mode: gtk::SelectionMode::Single,
                    set_activate_on_single_click: true,
                },

                #[name = "folder_bottom_list"]
                gtk::ListBox {
                    add_css_class: "navigation-sidebar",
                    add_css_class: "alcedo-folder-list-bottom",
                    set_selection_mode: gtk::SelectionMode::Single,
                    set_activate_on_single_click: true,
                },
            },

            #[wrap(Some)]
            set_end_child = &gtk::Box {
                set_orientation: gtk::Orientation::Vertical,
                set_vexpand: true,
                set_hexpand: true,

                #[name = "auth_banner"]
                adw::Banner {
                    set_use_markup: false,
                    set_revealed: false,
                },

                #[name = "mail_paned"]
                gtk::Paned {
                    set_orientation: gtk::Orientation::Horizontal,
                    set_vexpand: true,
                    set_hexpand: true,
                    set_wide_handle: true,
                    set_position: 400,
                    set_resize_start_child: false,
                    set_shrink_start_child: false,
                    set_resize_end_child: true,
                    set_shrink_end_child: false,

                    #[wrap(Some)]
                    #[name = "messages_placeholder"]
                    set_start_child = &gtk::Box {
                        set_orientation: gtk::Orientation::Vertical,
                        set_vexpand: true,
                    },

                    #[wrap(Some)]
                    #[name = "content_placeholder"]
                    set_end_child = &gtk::Box {
                        set_orientation: gtk::Orientation::Vertical,
                        set_hexpand: true,
                        set_vexpand: true,
                    },
                },
            },
        },
            },
        },
    }

    fn init(
        init: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        root.add_named(
            &welcome::build_welcome_page({
                let sender = sender.input_sender().clone();
                move || {
                    sender.emit(MailMsg::AddAccount);
                }
            }),
            Some("welcome"),
        );
        // The "mail" child is created by `view_output!()` below. Setting the
        // visible child here would no-op and leave the welcome page up, which
        // hides mock (and still-loading) mail behind Add Account.

        let accounts = init.accounts;
        let folders = init.folders;
        let envelopes = init.envelopes;
        let source_envelopes = init.source_envelopes;
        let bodies = init.bodies;

        let startup = resolve_startup_selection(
            &last_folder(),
            last_account(),
            &accounts
                .iter()
                .map(|account| account.id)
                .collect::<Vec<_>>(),
            &folders,
        );
        let selected_account = startup.selected_account;
        let mail_view = startup.mail_view;
        let pending_restore = startup.pending_restore;
        let pending_extension_restore = match last_folder() {
            LastFolder::Extension { id } => Some(id),
            _ => None,
        };

        let cid_cache = init.cid_cache.clone();
        let mut attachments_by_id = HashMap::new();
        let html_bodies = mock_html_bodies(init.mock, &bodies);
        let mut unsubscribe_by_id = HashMap::new();
        if init.mock {
            mock_seed_meeting_invite(&envelopes, &mut attachments_by_id, &cid_cache);
            if let Some(env) = envelopes.first() {
                unsubscribe_by_id.insert(
                    env.id,
                    ListUnsubscribe {
                        https: Some("https://example.com/unsubscribe".to_string()),
                        mailto: None,
                        one_click: false,
                    },
                );
            }
        }
        let contact_book = init.contact_book;
        let sent_recipients = init.sent_recipients;
        let remote_allowed_messages = Rc::new(RefCell::new(HashSet::new()));
        let remote_image_senders = init.remote_image_senders;

        let on_address_click: Rc<dyn Fn(&gtk::Widget, Address)> = address_chip_popover_handler(
            contact_book.clone(),
            remote_image_senders.clone(),
            init.app_sender.clone(),
            Rc::new({
                let sender = sender.clone();
                move |index, email| {
                    sender.input(MailMsg::AddressAction {
                        index,
                        account_id: None,
                        email,
                    })
                }
            }),
        );

        let mut list_view = new_list_view();
        ensure_message_list_mode_default(!accounts.is_empty());
        let list_mode = message_list_mode();
        let list_rows: Vec<MessageRow> = envelopes
            .iter()
            .cloned()
            .map(MessageRow::from_envelope)
            .collect();
        populate_list(&mut list_view, &list_rows);

        let widgets = view_output!();

        let content_stack = gtk::Stack::new();
        content_stack.set_hexpand(true);
        content_stack.set_vexpand(true);
        widgets.content_placeholder.append(&content_stack);

        let ext_ctx = crate::extension::MailContext::new(
            sender.input_sender().clone(),
            init.cmd.clone(),
            init.window.clone().upcast(),
        );
        let reader_page = build_reader_page(
            {
                let sender = sender.clone();
                move |address| sender.input(MailMsg::ComposeMailto(address))
            },
            {
                let sender = sender.clone();
                move || sender.input(MailMsg::LoadRemoteImages)
            },
            {
                let sender = sender.clone();
                move || sender.input(MailMsg::AlwaysLoadRemoteFromSender)
            },
            {
                let sender = sender.clone();
                move || sender.input(MailMsg::Reply)
            },
            {
                let sender = sender.clone();
                move || sender.input(MailMsg::ReplyAll)
            },
            {
                let sender = sender.clone();
                move || sender.input(MailMsg::ReplyInWindow)
            },
            {
                let sender = sender.clone();
                move || sender.input(MailMsg::ReplyAllInWindow)
            },
            {
                let sender = sender.clone();
                move || sender.input(MailMsg::ReplyList)
            },
            {
                let sender = sender.clone();
                move || sender.input(MailMsg::Forward)
            },
            {
                let sender = sender.clone();
                move || sender.input(MailMsg::ForwardInWindow)
            },
            {
                let sender = sender.clone();
                move || sender.input(MailMsg::ArchiveSelected)
            },
            {
                let sender = sender.clone();
                move || sender.input(MailMsg::StarSelection)
            },
            {
                let sender = sender.clone();
                move || sender.input(MailMsg::UnstarSelection)
            },
            {
                let sender = sender.clone();
                move || sender.input(MailMsg::DeleteSelected)
            },
            {
                let sender = sender.clone();
                move || sender.input(MailMsg::ViewMessageSource)
            },
            {
                let sender = sender.clone();
                move || sender.input(MailMsg::SaveMessageToFile)
            },
            {
                let sender = sender.clone();
                move || sender.input(MailMsg::Unsubscribe)
            },
            {
                let sender = sender.clone();
                move |id| sender.input(MailMsg::CopiedVerificationCode(id))
            },
            crate::extension::registered()
                .iter()
                .filter_map(|extension| extension.reader_bar(&ext_ctx))
                .collect(),
            {
                let sender = sender.clone();
                move || sender.input(MailMsg::Reaction(ReactionMsg::Open(MessageId(0))))
            },
            {
                let sender = sender.clone();
                move |emoji| {
                    sender.input(MailMsg::Reaction(ReactionMsg::Send {
                        id: MessageId(0),
                        emoji,
                    }))
                }
            },
            {
                let on_address_click = on_address_click.clone();
                move |anchor, addr| on_address_click(anchor, addr)
            },
            cid_cache.clone(),
        );
        clear_reader(&reader_page.widgets);

        let json_debounce = Rc::new(Cell::new(None::<glib::SourceId>));
        let imap_debounce = Rc::new(Cell::new(None::<glib::SourceId>));
        let on_compose_changed: Rc<dyn Fn()> = Rc::new({
            let mail_sender = sender.input_sender().clone();
            let json_debounce = json_debounce.clone();
            let imap_debounce = imap_debounce.clone();
            move || {
                crate::glib_source::cancel_source(&json_debounce);
                let mail_sender_json = mail_sender.clone();
                let json_debounce_cb = json_debounce.clone();
                json_debounce.set(Some(glib::timeout_add_local(
                    Duration::from_millis(200),
                    move || {
                        let _ = json_debounce_cb.take();
                        mail_sender_json.send(MailMsg::ComposeDraftJsonPersist).ok();
                        glib::ControlFlow::Break
                    },
                )));

                crate::glib_source::cancel_source(&imap_debounce);
                let mail_sender_imap = mail_sender.clone();
                let imap_debounce_cb = imap_debounce.clone();
                imap_debounce.set(Some(glib::timeout_add_local(
                    Duration::from_millis(3000),
                    move || {
                        let _ = imap_debounce_cb.take();
                        mail_sender_imap.send(MailMsg::ComposeDraftImapPersist).ok();
                        glib::ControlFlow::Break
                    },
                )));
            }
        });

        let snippets_rc = Rc::new(RefCell::new(Vec::new()));
        let compose_hooks = ComposerHooks {
            on_include_signature_toggled: Rc::new({
                let sender = sender.clone();
                let on_compose_changed = on_compose_changed.clone();
                move |include| {
                    sender.input(MailMsg::IncludeSignatureToggled(None, include));
                    on_compose_changed();
                }
            }),
            on_snippet_chosen: Rc::new({
                let sender = sender.clone();
                move |id| sender.input(MailMsg::InsertSnippet(None, id))
            }),
            on_manage_snippets: Rc::new({
                let app_sender = init.app_sender.clone();
                move || {
                    app_sender
                        .send(crate::app::AppMsg::OpenComposingPreferences)
                        .ok();
                }
            }),
            on_from_changed: Rc::new({
                let sender = sender.clone();
                move || sender.input(MailMsg::ComposeFromChanged(None))
            }),
            on_compose_changed,
            on_address_action: Rc::new({
                let sender = sender.clone();
                move |index, email| {
                    sender.input(MailMsg::ComposerAddressAction {
                        index,
                        window_id: None,
                        email,
                    })
                }
            }),
            snippets: snippets_rc,
        };

        let composer_page = build_composer_page(
            contact_book.clone(),
            sent_recipients.clone(),
            init.runtime.clone(),
            compose_hooks.clone(),
            {
                let sender = sender.clone();
                move || sender.input(MailMsg::SendCompose)
            },
            {
                let sender = sender.clone();
                move |choice| {
                    sender.input(MailMsg::SendWithExtension {
                        choice,
                        window_id: None,
                    })
                }
            },
            {
                let sender = sender.clone();
                move || sender.input(MailMsg::CancelCompose)
            },
            {
                let sender = sender.clone();
                move || sender.input(MailMsg::PopOutComposer)
            },
        );

        let reader_toolbar = adw::ToolbarView::new();
        reader_toolbar.add_top_bar(&reader_page.widgets.header_bar);
        reader_toolbar.set_content(Some(&reader_page.root));
        content_stack.add_named(&reader_toolbar, Some("reader"));
        content_stack.add_named(&composer_page.root, Some("composer"));
        reader_page.widgets.back_btn.connect_clicked({
            let sender = sender.clone();
            move |_| sender.input(MailMsg::BackToMessages)
        });

        let messages_pane = build_messages_pane(
            &list_view,
            {
                let sender = sender.clone();
                move |query| sender.input(MailMsg::Search(query))
            },
            {
                let sender = sender.clone();
                move |visible| sender.input(MailMsg::SetFoldersVisible(visible))
            },
            Rc::new({
                let sender = sender.clone();
                move |scope| sender.input(MailMsg::SearchScope(scope))
            }),
            Rc::new({
                let sender = sender.clone();
                move |query| sender.input(MailMsg::SearchQueryRewrite(query))
            }),
        );
        widgets.messages_placeholder.append(&messages_pane.root);

        let search_complete_folders = Rc::new(RefCell::new(Vec::new()));
        attach_search_autocomplete(
            &messages_pane.search_entry,
            contact_book.clone(),
            sent_recipients.clone(),
            search_complete_folders.clone(),
        );

        let mail_actions = setup_mail_window_actions(&init.window, sender.input_sender());
        let mail_context_menu = gtk::PopoverMenu::from_model(Some(&mail_selection_menu()));
        mail_context_menu.set_parent(&list_view.view);
        mail_context_menu.set_has_arrow(false);

        let folder_context_menu = gtk::Popover::new();
        folder_context_menu.set_parent(&init.toasts);
        folder_context_menu.set_has_arrow(false);
        folder_context_menu.add_css_class("menu");
        folder_context_menu.add_css_class("alcedo-folder-menu");
        let folder_menu_closed = sender.clone();
        folder_context_menu.connect_closed(move |_| {
            folder_menu_closed.input(MailMsg::FolderMenuClosed);
        });

        set_flag_toggle_handler(Rc::new({
            let sender = sender.clone();
            move |id, flagged| sender.input(MailMsg::ToggleFlag(id, flagged))
        }));

        set_thread_expand_handler(Rc::new({
            let sender = sender.clone();
            move |thread_id| sender.input(MailMsg::ToggleThreadExpand(thread_id))
        }));

        let mut account_switcher = build_account_switcher();
        widgets.folder_pane.prepend(&account_switcher.button);
        let layout::FolderChrome {
            header: folder_header,
            start: folder_start,
            compose: folder_compose,
            menu: menu_button,
            close: folder_close,
        } = layout::build_folder_chrome({
            let sender = sender.clone();
            move || sender.input(MailMsg::DismissFolderOverlay)
        });
        let folder_body = gtk::Box::new(gtk::Orientation::Vertical, 0);
        folder_body.set_vexpand(true);
        while let Some(child) = widgets.folder_pane.first_child() {
            widgets.folder_pane.remove(&child);
            folder_body.append(&child);
        }
        let folder_toolbar = adw::ToolbarView::new();
        folder_toolbar.add_top_bar(&folder_header);
        folder_toolbar.set_content(Some(&folder_body));
        folder_toolbar.set_vexpand(true);
        widgets.folder_pane.append(&folder_toolbar);

        let folder_overlay = adw::OverlaySplitView::new();
        folder_overlay.set_collapsed(true);
        folder_overlay.set_sidebar_position(gtk::PackType::Start);
        folder_overlay.set_show_sidebar(false);
        folder_overlay.set_vexpand(true);
        folder_overlay.set_hexpand(true);
        let overlay_host = gtk::Overlay::new();
        overlay_host.set_child(Some(&folder_overlay));
        widgets
            .mail_layout
            .add_named(&overlay_host, Some("overlay"));
        let folder_peek =
            folder_peek::FolderPeek::install(&overlay_host, &folder_overlay, &widgets.folder_pane);
        let sidebar_notify_suppress = Rc::new(Cell::new(false));
        folder_overlay.connect_show_sidebar_notify({
            let sender = sender.clone();
            let sidebar_notify_suppress = sidebar_notify_suppress.clone();
            move |_| {
                if sidebar_notify_suppress.get() {
                    return;
                }
                sender.input(MailMsg::FolderOverlayChanged);
            }
        });
        refresh_account_switcher(
            &mut account_switcher,
            &accounts,
            selected_account,
            &HashMap::new(),
            &HashMap::new(),
            {
                let sender = sender.clone();
                move |account_id| sender.input(MailMsg::SelectAccount(account_id))
            },
            {
                let sender = sender.clone();
                move || sender.input(MailMsg::AddAccount)
            },
        );

        let add_account_dialog =
            build_add_account_dialog(sender.input_sender().clone(), init.runtime.clone());

        let reader_header = reader_page.widgets.header_bar.clone();
        let reader_back = reader_page.widgets.back_btn.clone();
        let composer_header = composer_page.header.clone();
        let composer_chrome = composer_page.header_chrome.clone();
        let list_header = messages_pane.header.clone();
        let list_start = messages_pane.list_start.clone();
        let folder_toggle = messages_pane.folder_toggle.clone();
        let list_compose = messages_pane.compose_btn.clone();

        let mut model = MailModel {
            mock: init.mock,
            cmd: init.cmd,
            runtime: init.runtime.clone(),
            accounts,
            folders,
            selected_account,
            pending_account_folder_select: None,
            pending_account_add_email: None,
            pending_select_imap: None,
            pending_restore,
            pending_extension_restore,
            view_menu_items: Vec::new(),
            search_save_items: Vec::new(),
            pending_folder_sync: None,
            pending_virtual_sync: None,
            context_sync_target: None,
            mail_view,
            pre_search_mail_view: None,
            envelopes,
            conversations: Vec::new(),
            expanded_threads: HashSet::new(),
            list_mode,
            source_envelopes,
            bodies: body_cache(bodies),
            html_bodies: body_cache(html_bodies),
            attachments_by_id,
            attachments_pending_ids: HashSet::new(),
            attachments_failed_ids: HashSet::new(),
            unsubscribe_by_id,
            unsubscribed_ids: HashSet::new(),
            unsubscribing_ids: HashSet::new(),
            pending_deleted_messages: HashMap::new(),
            pending_delete_confirm: None,
            cid_cache: init.cid_cache,
            inline_cid_sha: HashMap::new(),
            threading_hdrs: HashMap::new(),
            reactions_by_target: HashMap::new(),
            optimistic_reactions: HashMap::new(),
            reaction_message_ids: HashSet::new(),
            reaction_originals: HashMap::new(),
            pending_account_moves: None,
            selected: None,
            mail_content_pane: MailContentPane::Reader,
            pending_send_draft: None,
            pending_send_queue_id: None,
            reopen_reader_after_send: false,
            send_surface_released: false,
            pending_reaction_target: None,
            pending_invite_response_organizer: None,
            send_in_flight: Rc::new(Cell::new(false)),
            list_view,
            folder_lists: FolderPaneLists {
                top: widgets.folder_top_list.clone(),
                middle: widgets.folder_list.clone(),
                pinned: widgets.folder_pinned_list.clone(),
                bottom: widgets.folder_bottom_list.clone(),
            },
            add_account_dialog,
            preferences: None,
            account_switcher,
            toasts: init.toasts,
            activity: init.activity,
            search_entry: messages_pane.search_entry,
            search_filter_bar: messages_pane.search_filter_bar,
            search_scope: SearchScopeKind::Current,
            search_complete_folders,
            sync_btn: messages_pane.sync_btn,
            status_bar: init.status_bar,
            sync_text: if init.mock {
                Some("Mock data".to_string())
            } else {
                None
            },
            last_logged_status: HashMap::new(),
            last_sync_at: if init.mock {
                Some(chrono::Utc::now())
            } else {
                None
            },
            content_stack: content_stack.clone(),
            folder_split: widgets.folder_split.clone(),
            reader: reader_page.widgets,
            composer: composer_page.widgets,
            composer_send: composer_page.send_chrome,
            unread_by_account: HashMap::new(),
            last_unread_indicator: None,
            collapsed_folders: collapsed_folders(),
            folder_pane_accounts: folder_pane_accounts(),
            collapsed_accounts: collapsed_accounts(),
            collapsed_personal_folders: collapsed_personal_folders(),
            hidden_accounts: hidden_accounts(),
            hidden_accounts_expanded: false,
            account_order: account_order(),
            messages_scrolled: messages_pane.scrolled_window,
            messages_body_stack: messages_pane.body_stack,
            empty_status: messages_pane.empty_status,
            mail_paned: widgets.mail_paned.clone(),
            auth_banner: widgets.auth_banner.clone(),
            auth_required: HashMap::new(),
            oauth_reauth_in_flight: false,
            message_windows: HashMap::new(),
            raw_messages: HashMap::new(),
            pending_raw_action: None,
            composer_windows: HashMap::new(),
            next_composer_window_id: 1,
            pending_send_window: None,
            contact_book,
            sent_recipients,
            on_address_click,
            mail_sender: sender.input_sender().clone(),
            msg_sender: init.app_sender.clone(),
            window: init.window,
            selection_bar: messages_pane.selection_bar,
            new_mail_chip: messages_pane.new_mail_chip,
            mail_actions,
            mail_context_menu,
            folder_context_menu,
            folder_menu_target: None,
            sync_anim_hold: false,
            sync_anim_timeout: None,
            remote_allowed_messages,
            remote_image_senders,
            known_inbox_ids: HashSet::new(),
            inbox_notify_ready: HashSet::new(),
            pending_notification_message: None,
            pending_notification_tried_folder: false,
            last_notified: HashMap::new(),
            notified_otp: HashMap::new(),
            pending_delete_toast: None,
            pending_delete_folder: None,
            pending_delete_through: None,
            undo_delete_toast: None,
            undo_delete_items: Vec::new(),
            undo_delete_envelopes: Vec::new(),
            undo_move: None,
            deleted_column_on: false,
            newest_first_sort_on: false,
            context_message_id: None,
            view_toggle: messages_pane.view_toggle.clone(),
            ext_ctx,
            rules: HashMap::new(),
            server_rules: HashMap::new(),
            preview: HashMap::new(),
            rules_dialog: None,
            pgp_by_account: HashMap::new(),
            pgp_by_message: HashMap::new(),
            autocrypt_by_message: HashMap::new(),
            signatures: HashMap::new(),
            identities: HashMap::new(),
            catch_all_domains: HashMap::new(),
            snippets: Vec::new(),
            compose_hooks,
            pending_imap_draft_open: None,
            pending_popout_draft: None,
            started_at: Instant::now(),
            network_error_toasted: HashSet::new(),
            root_stack: root.clone(),
            accounts_hydrated: false,
            delayed_send: None,
            layout: layout::MailLayout {
                mail_layout: widgets.mail_layout.clone(),
                folder_overlay,
                folder_header,
                folder_start,
                folder_compose,
                folder_close,
                list_header,
                list_start,
                reader_header,
                reader_back,
                composer_header,
                composer_chrome,
                folder_toggle,
                list_compose,
                menu_button,
                messages_placeholder: widgets.messages_placeholder.clone(),
                content_placeholder: widgets.content_placeholder.clone(),
                overlay_mode: false,
                overlay_forced: false,
                folder_peek,
                sidebar_notify_suppress,
                single_pane: false,
                folders_docked: folders_docked(),
                single_show_detail: false,
            },
        };

        model.disable_folder_context_actions();

        model.notify_own_addresses();

        model.auth_banner.connect_button_clicked({
            let sender = sender.clone();
            move |_| sender.input(MailMsg::ReconnectBrokenAccount)
        });

        if let Some(id) = model
            .selected_account
            .or_else(|| model.accounts.first().map(|account| account.id))
        {
            model.bind_composer_from(&model.composer, id);
        }

        set_context_message_handler(Rc::new({
            let sender = sender.clone();
            move |id, x, y| sender.input(MailMsg::RowContextMenu { id, x, y })
        }));

        if let MailView::Folder(folder_id) = model.mail_view {
            if model.mock {
                model.apply_mock_list();
            } else {
                model.pending_folder_sync = Some(folder_id);
                model.start_sync_anim_hold(&sender);
                model.cmd.emit(UiCommand::SelectFolder {
                    folder: folder_id,
                    mode: model.list_mode,
                    sync: true,
                });
            }
        }

        model.search_filter_bar.wire_save_menu(&[], {
            let sender = sender.clone();
            move |index| sender.input(MailMsg::SearchSaveAction(index))
        });

        model.rebuild_folder_bar(&sender);
        model.sync_deleted_column();
        model.refresh_unread_badges(&sender);
        model.refresh_status_bar();
        bind_folder_bar(
            &model.folder_lists,
            {
                let sender = sender.clone();
                move || {
                    sender.input(MailMsg::DismissFolderOverlay);
                    sender.input(MailMsg::SelectUnifiedInbox);
                }
            },
            {
                let sender = sender.clone();
                move |id| {
                    sender.input(MailMsg::DismissFolderOverlay);
                    sender.input(MailMsg::ShowExtensionView(id));
                }
            },
            {
                let sender = sender.clone();
                move |folder_id| {
                    sender.input(MailMsg::DismissFolderOverlay);
                    sender.input(MailMsg::SelectFolder(folder_id));
                }
            },
            {
                let sender = sender.clone();
                move |view, x, y| sender.input(MailMsg::FolderContextMenu { view, x, y })
            },
        );
        messages_pane.view_toggle.connect_toggled({
            let sender = sender.clone();
            move |btn| sender.input(MailMsg::ExtensionViewToggled(btn.is_active()))
        });
        bind_folder_pane_empty_context(&widgets.folder_scroll, {
            let sender = sender.clone();
            move |x, y| sender.input(MailMsg::FolderPaneContextMenu { x, y })
        });

        content_stack.set_visible_child_name(model.mail_content_pane.stack_name());

        model.list_view.selection_model.connect_selection_changed({
            let sender = sender.clone();
            move |_, _position, _n_items| {
                if list_selection_suppressed() {
                    return;
                }
                sender.input(MailMsg::ListSelectionChanged);
            }
        });

        model.list_view.view.connect_activate({
            let sender = sender.clone();
            move |_, position| {
                sender.input(MailMsg::ListActivated(position));
            }
        });

        apply_reading_pane(&model.mail_paned, reading_pane());
        model.sync_folder_placement();
        model.sync_folder_chrome();
        on_reading_pane_changed({
            let mail_paned = model.mail_paned.clone();
            let sender = sender.clone();
            move |pane| {
                apply_reading_pane(&mail_paned, pane);
                sender.input(MailMsg::ApplyTitleButtons);
            }
        });

        apply_list_density(&root, list_density());
        on_list_density_changed({
            let sender = sender.clone();
            move |density| sender.input(MailMsg::ListDensityChanged(density))
        });
        on_message_list_mode_changed({
            let sender = sender.clone();
            move |mode| sender.input(MailMsg::ListModeChanged(mode))
        });
        on_reader_font_size_changed({
            let sender = sender.clone();
            move |_| sender.input(MailMsg::ReaderFontSizeChanged)
        });
        on_editor_font_size_changed({
            let sender = sender.clone();
            move |_| sender.input(MailMsg::EditorFontSizeChanged)
        });
        on_load_remote_images_changed({
            let sender = sender.clone();
            move |_| sender.input(MailMsg::RemoteImagesPreferenceChanged)
        });
        on_show_verification_codes_changed({
            let sender = sender.clone();
            move |_| sender.input(MailMsg::VerificationCodesPreferenceChanged)
        });
        on_folder_pane_accounts_changed({
            let sender = sender.clone();
            move |mode| sender.input(MailMsg::FolderPaneAccountsChanged(mode))
        });

        model.sync_column_layout();
        model.sync_account_switcher(&sender);
        model.folder_split.set_position(folder_pane_width());
        wire_folder_pane_persist(&model.folder_split);
        // MultiSelection does not autoselect index 0; open the first row when needed.
        model.open_autoselected_message(&sender);
        model.sync_selection_chrome();
        // `view_output!()` has created the mail child by now. Mock has no sync
        // hydrate, so this is what actually reveals the 3-pane layout.
        model.refresh_welcome_page();
        let recovered = load_recovered_drafts();
        if !recovered.is_empty() {
            sender.input(MailMsg::RestoreSavedComposeDrafts(recovered));
        }

        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>, _root: &Self::Root) {
        match msg {
            msg @ (MailMsg::ListSelectionChanged
            | MailMsg::ListActivated(..)
            | MailMsg::ListDensityChanged(..)
            | MailMsg::ListModeChanged(..)
            | MailMsg::ToggleThreadExpand(..)
            | MailMsg::SelectAllMessages
            | MailMsg::RowContextMenu { .. }) => self.update_list(msg, sender),
            msg @ (MailMsg::OpenMessage(..)
            | MailMsg::OpenMessageWindow(..)
            | MailMsg::MessageWindowClosed(..)
            | MailMsg::ShowReader
            | MailMsg::LoadRemoteImages
            | MailMsg::LoadRemoteImagesFor(..)
            | MailMsg::AlwaysLoadRemoteFromSender
            | MailMsg::AlwaysLoadRemoteFromSenderFor(..)
            | MailMsg::Unsubscribe
            | MailMsg::UnsubscribeFor(..)
            | MailMsg::ExecuteUnsubscribe { .. }
            | MailMsg::SetSenderRemoteImages { .. }
            | MailMsg::RemoteImagesPreferenceChanged
            | MailMsg::ViewMessageSource
            | MailMsg::SaveMessageToFile
            | MailMsg::ViewMessageSourceFor(..)
            | MailMsg::SaveMessageToFileFor(..)) => self.update_reader(msg, sender),
            msg @ (MailMsg::OpenNotificationMessage(..)
            | MailMsg::CopyNotifiedVerificationCode(..)
            | MailMsg::MarkNotifiedMessageRead(..)
            | MailMsg::DeleteNotifiedMessage(..)) => self.update_notify(msg, sender),
            msg @ (MailMsg::Compose
            | MailMsg::CancelCompose
            | MailMsg::ComposerDiscarded
            | MailMsg::ShowComposer
            | MailMsg::ComposeMailto(..)
            | MailMsg::ComposerWindowClosed(..)
            | MailMsg::PopOutComposer
            | MailMsg::PopOutComposerWithDraft(..)
            | MailMsg::OpenSnippetEditor(..)
            | MailMsg::IncludeSignatureToggled(..)
            | MailMsg::InsertSnippet(..)
            | MailMsg::ComposeFromChanged(..)) => self.update_compose(msg, sender),
            msg @ (MailMsg::RestoreSavedComposeDrafts(..)
            | MailMsg::ApplyRestoredComposeDrafts(..)
            | MailMsg::RestoreQueuedSends(..)
            | MailMsg::ApplyQueuedSends(..)
            | MailMsg::ComposeDraftJsonPersist
            | MailMsg::ComposeDraftImapPersist) => self.update_drafts(msg, sender),
            msg @ (MailMsg::RefreshPreferences
            | MailMsg::SetPreferences(..)
            | MailMsg::ApplyTheme
            | MailMsg::ReaderFontSizeChanged
            | MailMsg::EditorFontSizeChanged
            | MailMsg::VerificationCodesPreferenceChanged
            | MailMsg::Escape
            | MailMsg::OpenCommandPalette
            | MailMsg::SetFoldersVisible(_)
            | MailMsg::FolderOverlayChanged
            | MailMsg::DismissFolderOverlay
            | MailMsg::RailRevealHover(_)
            | MailMsg::RailPointerInside(_)
            | MailMsg::SetAdaptiveLayout(_)
            | MailMsg::BackToMessages
            | MailMsg::ApplyTitleButtons
            | MailMsg::RefreshShortcutTooltips) => self.update_chrome(msg, sender),
            msg @ (MailMsg::RefreshStatusBar
            | MailMsg::RequestSync
            | MailMsg::SyncAnimHoldElapsed) => self.update_status(msg, sender),
            msg @ (MailMsg::CopiedVerificationCode(..)
            | MailMsg::DeleteSelected
            | MailMsg::DeletePermanentlySelected
            | MailMsg::ConfirmPermanentDelete(..)
            | MailMsg::UndoDelete) => self.update_delete(msg, sender),
            msg @ MailMsg::UndoMove => self.update_undo_move(msg, sender),
            msg @ (MailMsg::Reply
            | MailMsg::ReplyAll
            | MailMsg::ReplyTo(..)
            | MailMsg::ReplyList
            | MailMsg::ReplyListTo(..)
            | MailMsg::ReplyAllTo(..)
            | MailMsg::ReplyInWindow
            | MailMsg::ReplyAllInWindow
            | MailMsg::ReplyToInWindow(..)
            | MailMsg::ReplyAllToInWindow(..)
            | MailMsg::Forward
            | MailMsg::ForwardTo(..)
            | MailMsg::ForwardInWindow
            | MailMsg::ForwardToInWindow(..)) => self.update_reply(msg, sender),
            MailMsg::InviteResponse(payload) => self.send_invite_response(payload),
            MailMsg::Reaction(msg) => self.update_reactions(msg),
            msg @ MailMsg::SelectUnifiedInbox => self.update_views(msg, sender),
            msg @ (MailMsg::SelectFolder(..)
            | MailMsg::ToggleFolderExpanded { .. }
            | MailMsg::TogglePersonalFolders(..)
            | MailMsg::ToggleExtensionSection(..)
            | MailMsg::ReorderExtensionViews { .. }
            | MailMsg::ExtensionViewMenuItem(..)
            | MailMsg::RefreshFolderBadges
            | MailMsg::FolderContextMenu { .. }
            | MailMsg::FolderPaneContextMenu { .. }
            | MailMsg::FolderMenuClosed
            | MailMsg::FolderContextSync
            | MailMsg::FolderContextMarkRead
            | MailMsg::FolderContextSearch
            | MailMsg::FolderContextToggleExpand) => self.update_folders(msg, sender),
            msg @ (MailMsg::ToggleAccountExpanded(..)
            | MailMsg::ToggleHiddenAccountsExpanded
            | MailMsg::ReorderAccounts(..)
            | MailMsg::FolderPaneAccountsChanged(..)
            | MailMsg::AccountHeaderContextMenu { .. }
            | MailMsg::FolderContextAccountSettings
            | MailMsg::FolderContextToggleAccountHidden) => {
                self.update_account_sections(msg, sender)
            }
            msg @ (MailMsg::SelectAccount(..)
            | MailMsg::AddAccount
            | MailMsg::CloseAddAccount
            | MailMsg::SubmitAddAccount(..)
            | MailMsg::UpdateAccount { .. }
            | MailMsg::ReconnectBrokenAccount
            | MailMsg::OAuthReauthFinished
            | MailMsg::OAuthReauthFailed(..)
            | MailMsg::RemoveAccount(..)
            | MailMsg::PgpGenerate { .. }
            | MailMsg::PgpImport { .. }
            | MailMsg::PgpExportPublic(..)
            | MailMsg::PgpDeleteKey(..)
            | MailMsg::PgpSetSignOutgoing { .. }
            | MailMsg::PgpAcceptPeer { .. }
            | MailMsg::RefreshAccountPgp(..)
            | MailMsg::PaintAccountIdentities(..)
            | MailMsg::SetSmtpEnvelopePolicy { .. }
            | MailMsg::SetCatchAllDomains { .. }
            | MailMsg::RefreshSmtpEnvelopePolicy(..)
            | MailMsg::UpsertSignature(..)
            | MailMsg::UpsertIdentity(..)
            | MailMsg::DeleteIdentity(..)
            | MailMsg::UpsertSnippet(..)
            | MailMsg::DeleteSnippet(..)) => self.update_accounts(msg, sender),
            msg @ (MailMsg::SendCompose
            | MailMsg::SendComposerWindow(..)
            | MailMsg::QueuedComposeSend { .. }
            | MailMsg::ConfirmedComposeSend { .. }
            | MailMsg::PreSendConfirmed { .. }
            | MailMsg::UndoSend
            | MailMsg::DelayedSendElapsed) => self.update_send(msg, sender),
            msg @ (MailMsg::Search(..)
            | MailMsg::SearchScope(..)
            | MailMsg::SearchQueryRewrite(..)
            | MailMsg::FocusSearch
            | MailMsg::InsertSearchOperator(..)) => self.update_search(msg, sender),
            msg @ (MailMsg::SearchSaveAction(..) | MailMsg::FocusSearchEntry) => {
                self.update_extension_views(msg, sender)
            }
            msg @ (MailMsg::ToggleFlag(..)
            | MailMsg::ArchiveSelected
            | MailMsg::MarkSelectionSpam
            | MailMsg::MarkSelectionNotSpam
            | MailMsg::ChooseMoveFolder
            | MailMsg::ChooseCopyFolder
            | MailMsg::StartAccountMove { .. }
            | MailMsg::ApplySelectionToFolder { .. }
            | MailMsg::DropMessagesOnFolder { .. }
            | MailMsg::MarkSelectionRead
            | MailMsg::MarkSelectionUnread
            | MailMsg::StarSelection
            | MailMsg::UnstarSelection) => self.update_list_actions(msg, sender),
            msg @ (MailMsg::SendWithExtension { .. } | MailMsg::SendWithExtensionDraft { .. }) => {
                self.update_send(msg, sender)
            }
            msg @ (MailMsg::ExtensionToastWidget(..)
            | MailMsg::ExtensionOpenDraft(..)
            | MailMsg::ExtensionNotify { .. }) => self.update_extension_views(msg, sender),
            msg @ (MailMsg::MessageAction(..)
            | MailMsg::AddressAction { .. }
            | MailMsg::ComposerAddressAction { .. }
            | MailMsg::ShowExtensionView(..)
            | MailMsg::ExtensionViewsChanged
            | MailMsg::ExtensionViewToggled(..)
            | MailMsg::ExtensionToast(..)) => self.update_extension_views(msg, sender),
            msg @ (MailMsg::FolderContextEmpty
            | MailMsg::FolderContextDeleteOnOrBefore
            | MailMsg::FolderContextNew
            | MailMsg::FolderContextRename
            | MailMsg::FolderContextDelete
            | MailMsg::NewFolder
            | MailMsg::EditFolder
            | MailMsg::ConfirmDeleteFolder(..)
            | MailMsg::FolderEditorSubmitCreate(..)
            | MailMsg::FolderEditorSubmitEdit { .. }
            | MailMsg::ConfirmEmptyFolder(..)
            | MailMsg::ConfirmDeleteOnOrBefore { .. }) => self.update_folder_ops(msg, sender),
            msg @ (MailMsg::OpenRules
            | MailMsg::OpenRulesForAccount(..)
            | MailMsg::CreateRuleFromMessage
            | MailMsg::UpsertMessageRule { .. }
            | MailMsg::DeleteMessageRule(..)
            | MailMsg::ReorderMessageRules { .. }
            | MailMsg::RunMessageRules { .. }
            | MailMsg::AdoptServerRules(..)
            | MailMsg::RulesDialogClosed) => self.update_rules(msg, sender),
            MailMsg::Sync(event) => {
                let window = self.window.clone();
                self.handle_sync_event(event, &sender, &window);
            }
        }
    }
}
