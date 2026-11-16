use serde::{Deserialize, Serialize};

use crate::account::{AuthMethod, OAuthTokens, SpecialUse};
use crate::contact::{ContactDraft, GroupDraft};
use crate::envelope::MessageListMode;
use crate::extension::ExtensionMessage;
use crate::identity::AccountIdentity;
use crate::ids::{
    AccountId, ContactId, FolderId, GroupId, IdentityId, MessageId, QueuedSendId, RuleId,
    SnippetId, ThreadId,
};
use crate::rules::MessageRule;
use crate::snippet::{AccountSignature, Snippet};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum UiCommand {
    AddAccount(AccountDraft),
    UpdateAccount {
        id: AccountId,
        update: AccountSettingsUpdate,
    },
    RemoveAccount(AccountId),
    SelectFolder {
        folder: FolderId,
        #[serde(default)]
        mode: MessageListMode,
        /// When false, reload the local list only (no IMAP SELECT).
        #[serde(default = "default_true")]
        sync: bool,
    },
    SelectUnifiedInbox {
        #[serde(default)]
        mode: MessageListMode,
    },
    OpenMessage(MessageId),
    /// Load the next row's text into the reader cache without attachment bytes.
    PrefetchMessage(MessageId),
    OpenThread {
        thread_id: ThreadId,
        folder_ids: Option<Vec<FolderId>>,
    },
    FetchRawMessage(MessageId),
    SetFlag {
        id: MessageId,
        seen: Option<bool>,
        flagged: Option<bool>,
    },
    SetFlags {
        ids: Vec<MessageId>,
        seen: Option<bool>,
        flagged: Option<bool>,
    },
    DeleteMessage(MessageId),
    DeleteMessages {
        ids: Vec<MessageId>,
    },
    ExpungeMessages {
        ids: Vec<MessageId>,
    },
    UndeleteMessage {
        id: MessageId,
        dest: FolderId,
    },
    MoveMessage {
        id: MessageId,
        dest: SpecialUse,
    },
    MoveMessageToFolder {
        id: MessageId,
        dest: FolderId,
    },
    MoveMessages {
        ids: Vec<MessageId>,
        dest: SpecialUse,
    },
    MoveMessagesToFolder {
        ids: Vec<MessageId>,
        dest: FolderId,
    },
    CopyMessageToFolder {
        id: MessageId,
        dest: FolderId,
    },
    Compose(ComposeDraft),
    Send(ComposeDraft),
    UpsertContact(ContactDraft),
    DeleteContacts(Vec<ContactId>),
    UpsertGroup(GroupDraft),
    DeleteGroup(GroupId),
    PromoteContact(ContactId),
    BlockContact {
        id: ContactId,
    },
    AllowSender {
        email: String,
        display_name: String,
    },
    Search {
        query: String,
        /// None = all folders. Some(ids) = restrict unless query has folder:/in:.
        folder_ids: Option<Vec<FolderId>>,
        #[serde(default)]
        mode: MessageListMode,
    },
    LoadRemoteImages(MessageId),
    LoadReactions {
        target_id: MessageId,
    },
    /// Resolve `reaction_target_hdr` on this message and load the original.
    LoadReactionOriginal {
        id: MessageId,
    },
    OneClickUnsubscribe(MessageId),
    SetSenderRemoteImages {
        emails: Vec<String>,
        allow: bool,
    },
    RefreshAccount(AccountId),
    SyncFolder(FolderId),
    MarkFolderRead(FolderId),
    EmptyFolder(FolderId),
    DeleteFolderOnOrBefore {
        folder: FolderId,
        through: chrono::NaiveDate,
    },
    CreateFolder {
        account: AccountId,
        parent: Option<FolderId>,
        name: String,
    },
    RenameFolder {
        id: FolderId,
        parent: Option<FolderId>,
        name: String,
    },
    DeleteFolder(FolderId),
    UpsertMessageRule(MessageRule),
    DeleteMessageRule {
        id: RuleId,
    },
    ReorderMessageRules {
        account: AccountId,
        ids: Vec<RuleId>,
    },
    RunMessageRules {
        account: AccountId,
        folder: FolderId,
        rule_ids: Option<Vec<RuleId>>,
    },
    AdoptServerRules(AccountId),
    PersistQueuedSend {
        id: QueuedSendId,
        draft: ComposeDraft,
    },
    ForgetQueuedSend {
        id: QueuedSendId,
    },
    PgpGenerate {
        account: AccountId,
        passphrase: String,
    },
    PgpImport {
        account: AccountId,
        armored: String,
        passphrase: Option<String>,
    },
    PgpExportPublic {
        account: AccountId,
    },
    PgpSetSignOutgoing {
        account: AccountId,
        enabled: bool,
    },
    SetSmtpEnvelopePolicy {
        account: AccountId,
        same_domain_identity: bool,
    },
    RefreshSmtpEnvelopePolicy {
        account: AccountId,
    },
    /// Domains where every address belongs to the account. Replies to mail
    /// sent to any of them come from that address.
    SetCatchAllDomains {
        account: AccountId,
        domains: Vec<String>,
    },
    PgpDeleteKey {
        account: AccountId,
    },
    PgpAcceptPeer {
        account: AccountId,
        email: String,
        armored: String,
    },
    PgpRefreshState {
        account: AccountId,
    },
    UpsertSignature(AccountSignature),
    UpsertIdentity(AccountIdentity),
    DeleteIdentity {
        id: IdentityId,
    },
    UpsertSnippet(Snippet),
    DeleteSnippet(SnippetId),
    SaveImapDraft(ComposeDraft),
    DeleteImapDraft {
        account: AccountId,
        folder: FolderId,
        uid: u32,
        message_id: String,
    },
    /// A command for the extension named in the message.
    Extension(ExtensionMessage),
}

#[derive(Clone, Serialize, Deserialize)]
pub struct AccountDraft {
    pub email: String,
    pub username: String,
    pub display_name: String,
    pub imap_host: String,
    pub imap_port: u16,
    pub smtp_host: String,
    pub smtp_port: u16,
    pub smtp_starttls: bool,
    pub auth: AuthMethod,
    pub password: Option<String>,
    pub oauth_tokens: Option<OAuthTokens>,
    #[serde(default)]
    pub split_credentials: bool,
    #[serde(default)]
    pub smtp_username: String,
    #[serde(default)]
    pub smtp_password: Option<String>,
}

impl std::fmt::Debug for AccountDraft {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AccountDraft")
            .field("email", &self.email)
            .field("username", &self.username)
            .field("display_name", &self.display_name)
            .field("imap_host", &self.imap_host)
            .field("imap_port", &self.imap_port)
            .field("smtp_host", &self.smtp_host)
            .field("smtp_port", &self.smtp_port)
            .field("smtp_starttls", &self.smtp_starttls)
            .field("auth", &self.auth)
            .field("password", &self.password.as_ref().map(|_| "***"))
            .field("oauth_tokens", &self.oauth_tokens.as_ref().map(|_| "***"))
            .field("split_credentials", &self.split_credentials)
            .field("smtp_username", &self.smtp_username)
            .field("smtp_password", &self.smtp_password.as_ref().map(|_| "***"))
            .finish()
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct AccountSettingsUpdate {
    /// Mailbox address. Default From, and the SMTP envelope unless a saved From address is used.
    pub email: String,
    pub display_name: String,
    /// Personal name shown on outbound reactions. Empty falls back to the account label.
    #[serde(default)]
    pub sender_name: String,
    /// Custom chip initials. Empty means generate from the account name.
    #[serde(default)]
    pub avatar_text: String,
    /// Chip background as `#rrggbb`.
    #[serde(default)]
    pub accent_color: String,
    /// Square PNG cropped to a circle. Empty means initials on the background color.
    #[serde(default)]
    pub avatar_image: Vec<u8>,
    pub username: String,
    pub imap_host: String,
    pub imap_port: u16,
    pub smtp_host: String,
    pub smtp_port: u16,
    pub smtp_starttls: bool,
    pub password: Option<String>,
    pub oauth_tokens: Option<OAuthTokens>,
    #[serde(default)]
    pub split_credentials: bool,
    #[serde(default)]
    pub smtp_username: String,
    #[serde(default)]
    pub smtp_password: Option<String>,
}

impl std::fmt::Debug for AccountSettingsUpdate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AccountSettingsUpdate")
            .field("email", &self.email)
            .field("display_name", &self.display_name)
            .field("sender_name", &self.sender_name)
            .field("avatar_text", &self.avatar_text)
            .field("accent_color", &self.accent_color)
            .field("avatar_image_bytes", &self.avatar_image.len())
            .field("username", &self.username)
            .field("imap_host", &self.imap_host)
            .field("imap_port", &self.imap_port)
            .field("smtp_host", &self.smtp_host)
            .field("smtp_port", &self.smtp_port)
            .field("smtp_starttls", &self.smtp_starttls)
            .field("password", &self.password.as_ref().map(|_| "***"))
            .field("oauth_tokens", &self.oauth_tokens.as_ref().map(|_| "***"))
            .field("split_credentials", &self.split_credentials)
            .field("smtp_username", &self.smtp_username)
            .field("smtp_password", &self.smtp_password.as_ref().map(|_| "***"))
            .finish()
    }
}

impl AccountSettingsUpdate {
    pub fn validate(&self) -> Result<(), &'static str> {
        let email = self.email.trim();
        if email.is_empty() {
            return Err("Email is required");
        }
        let Some((local, domain)) = email.split_once('@') else {
            return Err("Email must contain @");
        };
        if local.is_empty() || domain.is_empty() || domain.contains('@') {
            return Err("Email is invalid");
        }
        if self.imap_host.is_empty() {
            return Err("IMAP host is required");
        }
        if self.smtp_host.is_empty() {
            return Err("SMTP host is required");
        }
        if !(1..=65535).contains(&self.imap_port) {
            return Err("IMAP port must be between 1 and 65535");
        }
        if !(1..=65535).contains(&self.smtp_port) {
            return Err("SMTP port must be between 1 and 65535");
        }
        Ok(())
    }

    pub fn validate_for_auth(&self, auth: &AuthMethod) -> Result<(), &'static str> {
        self.validate()?;
        if let AuthMethod::OAuth2 { provider } = auth {
            provider.validate_mail_hosts(&self.imap_host, &self.smtp_host)?;
        }
        Ok(())
    }
}

impl AccountDraft {
    pub fn validate_password_auth(&self) -> Result<(), &'static str> {
        if self.email.is_empty() {
            return Err("Email is required");
        }
        let Some((local, domain)) = self.email.split_once('@') else {
            return Err("Email must contain @");
        };
        if local.is_empty() || domain.is_empty() {
            return Err("Email is invalid");
        }
        match &self.password {
            None => return Err("Password is required"),
            Some(p) if p.is_empty() => return Err("Password is required"),
            Some(_) => {}
        }
        if self.split_credentials {
            match &self.smtp_password {
                None => return Err("SMTP password is required"),
                Some(p) if p.is_empty() => return Err("SMTP password is required"),
                Some(_) => {}
            }
        }
        if self.imap_host.is_empty() {
            return Err("IMAP host is required");
        }
        if self.smtp_host.is_empty() {
            return Err("SMTP host is required");
        }
        if !(1..=65535).contains(&self.imap_port) {
            return Err("IMAP port must be between 1 and 65535");
        }
        if !(1..=65535).contains(&self.smtp_port) {
            return Err("SMTP port must be between 1 and 65535");
        }
        Ok(())
    }

    pub fn validate_oauth_email(&self) -> Result<(), &'static str> {
        if self.email.is_empty() {
            return Err("Email is required");
        }
        let Some((local, domain)) = self.email.split_once('@') else {
            return Err("Email must contain @");
        };
        if local.is_empty() || domain.is_empty() {
            return Err("Email is invalid");
        }
        if !matches!(self.auth, AuthMethod::OAuth2 { .. }) {
            return Err("OAuth provider is required");
        }
        Ok(())
    }

    pub fn validate_oauth_auth(&self) -> Result<(), &'static str> {
        self.validate_oauth_email()?;
        let AuthMethod::OAuth2 { provider } = &self.auth else {
            return Err("OAuth provider is required");
        };
        provider.validate_mail_hosts(&self.imap_host, &self.smtp_host)?;
        let Some(tokens) = &self.oauth_tokens else {
            return Err("OAuth sign-in is required");
        };
        if tokens.refresh_token.as_ref().is_none_or(|t| t.is_empty()) {
            return Err("OAuth refresh token is required");
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ComposeFormat {
    #[default]
    Plain,
    Html,
    Markdown,
}

impl ComposeFormat {
    pub fn from_gsettings(value: &str) -> Self {
        match value {
            "html" => Self::Html,
            "markdown" => Self::Markdown,
            _ => Self::Plain,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Plain => "plain",
            Self::Html => "html",
            Self::Markdown => "markdown",
        }
    }

    pub fn dropdown_index(self) -> u32 {
        match self {
            Self::Plain => 0,
            Self::Html => 1,
            Self::Markdown => 2,
        }
    }

    pub fn from_dropdown_index(index: u32) -> Self {
        match index {
            1 => Self::Html,
            2 => Self::Markdown,
            _ => Self::Plain,
        }
    }

    pub fn dropdown_label(self) -> &'static str {
        match self {
            Self::Plain => "Plain text",
            Self::Html => "HTML",
            Self::Markdown => "Markdown",
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct ComposeAttachment {
    pub filename: String,
    pub mime: String,
    pub bytes: Vec<u8>,
}

impl std::fmt::Debug for ComposeAttachment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ComposeAttachment")
            .field("filename", &self.filename)
            .field("mime", &self.mime)
            .field("bytes", &format_args!("len={}", self.bytes.len()))
            .finish()
    }
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ComposeInline {
    pub content_id: String,
    pub filename: String,
    pub mime: String,
    pub bytes: Vec<u8>,
}

impl std::fmt::Debug for ComposeInline {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ComposeInline")
            .field("content_id", &self.content_id)
            .field("filename", &self.filename)
            .field("mime", &self.mime)
            .field("bytes", &format_args!("len={}", self.bytes.len()))
            .finish()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ImapDraftRef {
    pub folder_id: FolderId,
    pub uid: u32,
    pub uidvalidity: u32,
    pub message_id: String,
    #[serde(default)]
    pub last_internaldate: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct ComposeDraft {
    pub account_id: AccountId,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub bcc: Vec<String>,
    pub subject: String,
    pub body_text: String,
    #[serde(default)]
    pub format: ComposeFormat,
    pub request_mdn: bool,
    pub tracking_pixel_url: Option<String>,
    #[serde(default)]
    pub attachments: Vec<ComposeAttachment>,
    #[serde(default)]
    pub inlines: Vec<ComposeInline>,
    #[serde(default = "default_collect_outgoing")]
    pub collect_outgoing_contacts: bool,
    #[serde(default)]
    pub in_reply_to: Option<String>,
    #[serde(default)]
    pub references: Option<String>,
    #[serde(default)]
    pub thread_id: Option<ThreadId>,
    /// After sending, add this address as an approved contact. Set by an
    /// extension when replying; `screen_promote_email` is the older name.
    #[serde(default, alias = "screen_promote_email")]
    pub approve_contact_email: Option<String>,
    #[serde(default)]
    pub sign: bool,
    #[serde(default)]
    pub encrypt: bool,
    #[serde(default)]
    pub autocrypt_header: Option<String>,
    #[serde(default)]
    pub identity_id: Option<IdentityId>,
    #[serde(default)]
    pub custom_from: Option<String>,
    #[serde(default)]
    pub imap_draft: Option<ImapDraftRef>,
    #[serde(default)]
    pub reaction_emoji: Option<String>,
}

fn default_true() -> bool {
    true
}

fn default_collect_outgoing() -> bool {
    true
}

impl std::fmt::Debug for ComposeDraft {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ComposeDraft")
            .field("account_id", &self.account_id)
            .field("to", &self.to)
            .field("cc", &self.cc)
            .field("bcc", &self.bcc)
            .field("subject", &self.subject)
            .field("body_text", &self.body_text)
            .field("format", &self.format)
            .field("request_mdn", &self.request_mdn)
            .field("tracking_pixel_url", &self.tracking_pixel_url)
            .field("attachments", &self.attachments)
            .field("inlines", &self.inlines)
            .field("collect_outgoing_contacts", &self.collect_outgoing_contacts)
            .field("in_reply_to", &self.in_reply_to)
            .field("references", &self.references)
            .field("thread_id", &self.thread_id)
            .field("approve_contact_email", &self.approve_contact_email)
            .field("sign", &self.sign)
            .field("encrypt", &self.encrypt)
            .field(
                "autocrypt_header",
                &self.autocrypt_header.as_ref().map(|_| "***"),
            )
            .field("identity_id", &self.identity_id)
            .field("custom_from", &self.custom_from)
            .field("imap_draft", &self.imap_draft)
            .field("reaction_emoji", &self.reaction_emoji)
            .finish()
    }
}
