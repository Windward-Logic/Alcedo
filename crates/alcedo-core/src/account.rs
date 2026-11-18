use serde::{Deserialize, Serialize};

use crate::commands::AccountDraft;
use crate::ids::{AccountId, FolderId};
use crate::mail_provider::{GoogleMail, MailProvider, MicrosoftMail};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum AuthMethod {
    OAuth2 { provider: OAuthProvider },
    Password,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum OAuthProvider {
    Google,
    Microsoft,
}

impl OAuthProvider {
    /// Server settings and behaviour of this provider.
    pub fn profile(self) -> &'static dyn MailProvider {
        match self {
            Self::Google => &GoogleMail,
            Self::Microsoft => &MicrosoftMail,
        }
    }

    pub fn imap_host(self) -> &'static str {
        self.profile().imap_host()
    }

    pub fn smtp_host(self) -> &'static str {
        self.profile().smtp_host()
    }

    pub fn validate_imap_host(self, host: &str) -> Result<(), &'static str> {
        if host_eq(host, self.imap_host()) {
            Ok(())
        } else {
            Err("IMAP host must match the OAuth provider")
        }
    }

    pub fn validate_smtp_host(self, host: &str) -> Result<(), &'static str> {
        let profile = self.profile();
        let is_allowed = host_eq(host, profile.smtp_host())
            || profile
                .alternate_smtp_hosts()
                .iter()
                .any(|allowed| host_eq(host, allowed));
        if is_allowed {
            Ok(())
        } else {
            Err("SMTP host must match the OAuth provider")
        }
    }

    pub fn validate_mail_hosts(self, imap_host: &str, smtp_host: &str) -> Result<(), &'static str> {
        self.validate_imap_host(imap_host)?;
        self.validate_smtp_host(smtp_host)
    }
}

fn host_eq(actual: &str, expected: &str) -> bool {
    let actual = actual.trim().trim_end_matches('.').to_ascii_lowercase();
    actual == expected
}

/// Access and refresh tokens from an OAuth flow or refresh.
#[derive(Clone, Serialize, Deserialize)]
pub struct OAuthTokens {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_in: Option<u64>,
    /// The account name the provider reported at sign-in, when it reported one.
    #[serde(default)]
    pub sign_in_name: Option<String>,
}

impl std::fmt::Debug for OAuthTokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OAuthTokens")
            .field("access_token", &"***")
            .field("refresh_token", &self.refresh_token.as_ref().map(|_| "***"))
            .field("expires_in", &self.expires_in)
            .field("sign_in_name", &self.sign_in_name.as_ref().map(|_| "***"))
            .finish()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SpecialUse {
    Inbox,
    Sent,
    Drafts,
    Trash,
    Junk,
    Archive,
    Snoozed,
    Outbox,
    All,
    Other,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Account {
    pub id: AccountId,
    pub email: String,
    /// IMAP/SMTP login (also used for server rules). Empty means use [`Self::email`].
    pub username: String,
    /// Password accounts only. When true, SMTP uses `smtp_username` and the `smtp_password` keyring item.
    #[serde(default)]
    pub split_credentials: bool,
    /// SMTP login for split password accounts, or the sign-in name for Microsoft OAuth accounts.
    /// Empty means use the normal login. Ignored for password accounts that are not split.
    #[serde(default)]
    pub smtp_username: String,
    pub display_name: String,
    /// Personal name shown on outbound reactions. Empty falls back to the account label.
    #[serde(default)]
    pub sender_name: String,
    pub imap_host: String,
    pub imap_port: u16,
    pub smtp_host: String,
    pub smtp_port: u16,
    pub smtp_starttls: bool,
    pub auth: AuthMethod,
    pub accent_color: String,
    pub avatar_text: String,
    /// Square PNG cropped to a circle for the account chip. Empty means initials.
    #[serde(default)]
    pub avatar_image: Vec<u8>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Folder {
    pub id: FolderId,
    pub account_id: AccountId,
    pub imap_name: String,
    pub delimiter: String,
    pub special: SpecialUse,
    pub selectable: bool,
    pub uidvalidity: Option<u32>,
    pub uidnext: Option<u32>,
    pub highest_modseq: Option<u64>,
    pub unread: u32,
    pub total: u32,
}

impl Account {
    /// Label shown in account pickers. Falls back to the email when unset.
    pub fn label(&self) -> &str {
        if self.display_name.is_empty() {
            &self.email
        } else {
            &self.display_name
        }
    }

    /// IMAP/SMTP login (also used for server rules). Falls back to the email when unset.
    pub fn login_name(&self) -> &str {
        if self.username.is_empty() {
            &self.email
        } else {
            &self.username
        }
    }

    pub fn uses_split_credentials(&self) -> bool {
        matches!(self.auth, AuthMethod::Password) && self.split_credentials
    }

    /// The OAuth provider behind this account, if it signs in with OAuth.
    pub fn mail_provider(&self) -> Option<&'static dyn MailProvider> {
        match self.auth {
            AuthMethod::OAuth2 { provider } => Some(provider.profile()),
            AuthMethod::Password => None,
        }
    }

    /// True when the provider needs the sign-in name for SMTP and reported one.
    fn sends_with_sign_in_name(&self) -> bool {
        !self.split_credentials
            && !self.smtp_username.is_empty()
            && self
                .mail_provider()
                .is_some_and(|provider| provider.smtp_login_is_sign_in_name())
    }

    pub fn smtp_login_name(&self) -> &str {
        if self.uses_split_credentials() {
            if self.smtp_username.is_empty() {
                &self.email
            } else {
                &self.smtp_username
            }
        } else if self.sends_with_sign_in_name() {
            &self.smtp_username
        } else {
            self.login_name()
        }
    }

    /// Name sent to the SMTP server when authenticating.
    pub fn smtp_auth_username(&self) -> &str {
        match self.auth {
            AuthMethod::OAuth2 { .. } if !self.sends_with_sign_in_name() => &self.email,
            _ => self.smtp_login_name(),
        }
    }

    /// Chip initials: stored override when set, otherwise from [`Self::label`].
    pub fn avatar_chip(&self) -> String {
        let custom = sanitize_avatar_text(&self.avatar_text);
        if custom.is_empty() {
            avatar_from_label(self.label())
        } else {
            custom
        }
    }

    pub fn preset_for_email(email: &str) -> Option<AccountDraft> {
        let (local, domain) = email.split_once('@')?;
        if local.is_empty() || domain.is_empty() {
            return None;
        }

        let display_name = display_name_from_email(email);
        let domain = domain.to_ascii_lowercase();

        match domain.as_str() {
            "gmail.com" | "googlemail.com" => Some(oauth_preset(
                OAuthProvider::Google,
                email,
                &domain,
                display_name,
            )),
            "outlook.com" | "hotmail.com" | "live.com" | "office365.com" => Some(oauth_preset(
                OAuthProvider::Microsoft,
                email,
                &domain,
                display_name,
            )),
            "yahoo.com" | "ymail.com" => Some(AccountDraft {
                email: email.to_string(),
                username: email.to_string(),
                display_name,
                imap_host: "imap.mail.yahoo.com".to_string(),
                imap_port: 993,
                smtp_host: "smtp.mail.yahoo.com".to_string(),
                smtp_port: 465,
                smtp_starttls: false,
                auth: AuthMethod::Password,
                password: None,
                oauth_tokens: None,
                split_credentials: false,
                smtp_username: String::new(),
                smtp_password: None,
            }),
            _ => None,
        }
    }
}

fn oauth_preset(
    provider: OAuthProvider,
    email: &str,
    domain: &str,
    display_name: String,
) -> AccountDraft {
    let profile = provider.profile();
    AccountDraft {
        email: email.to_string(),
        username: email.to_string(),
        display_name,
        imap_host: profile.imap_host().to_string(),
        imap_port: 993,
        smtp_host: profile.smtp_host_for_domain(domain).to_string(),
        smtp_port: profile.smtp_port(),
        smtp_starttls: profile.smtp_starttls(),
        auth: AuthMethod::OAuth2 { provider },
        password: None,
        oauth_tokens: None,
        split_credentials: false,
        smtp_username: String::new(),
        smtp_password: None,
    }
}

pub fn display_name_from_email(email: &str) -> String {
    let local = email.split_once('@').map(|(l, _)| l).unwrap_or("");
    capitalize_local_part(local)
}

/// Initials for an account chip from the visible label (display name, else email).
///
/// - Two or more words: first letter of the first two (`Code Infusion` → `CI`)
/// - CamelCase / inner capitals: first letter plus the next capital (`CodeInfusion` → `CI`, `GMail` → `GM`)
/// - Otherwise: first letter (`Alcedo` → `A`)
pub fn avatar_from_label(label: &str) -> String {
    let trimmed = label.trim();
    if trimmed.is_empty() {
        return "?".to_string();
    }
    let source = trimmed
        .split_once('@')
        .map(|(local, _)| local)
        .unwrap_or(trimmed);

    let words: Vec<&str> = source
        .split(|c: char| c.is_whitespace() || matches!(c, '-' | '_'))
        .filter(|word| word.chars().any(|c| c.is_alphabetic()))
        .collect();

    if words.len() >= 2 {
        let mut out = String::new();
        for word in words.iter().take(2) {
            if let Some(ch) = word.chars().find(|c| c.is_alphabetic()) {
                out.extend(ch.to_uppercase());
            }
        }
        return nonempty_avatar(out);
    }

    let word = words.first().copied().unwrap_or(source);
    camel_or_single(word)
}

fn camel_or_single(word: &str) -> String {
    let mut letters = word.chars().filter(|c| c.is_alphabetic());
    let Some(first) = letters.next() else {
        return "?".to_string();
    };
    let rest: Vec<char> = letters.collect();
    let has_lower = first.is_lowercase() || rest.iter().any(|c| c.is_lowercase());
    let internal_upper = rest.into_iter().find(|c| c.is_uppercase());

    let mut out = String::new();
    if has_lower && let Some(inner) = internal_upper {
        out.extend(first.to_uppercase());
        out.extend(inner.to_uppercase());
        return nonempty_avatar(out);
    }
    out.extend(first.to_uppercase());
    nonempty_avatar(out)
}

fn nonempty_avatar(out: String) -> String {
    if out.is_empty() { "?".to_string() } else { out }
}

/// Fallback chip background when an account has no stored color.
pub const DEFAULT_ACCENT_COLOR: &str = "#3584e4";

/// Normalize a chip background to `#rrggbb`. Invalid input becomes [`DEFAULT_ACCENT_COLOR`].
pub fn sanitize_accent_color(raw: &str) -> String {
    let hex = raw.trim().strip_prefix('#').unwrap_or(raw.trim());
    if hex.len() == 6 && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return format!("#{}", hex.to_ascii_lowercase());
    }
    if hex.len() == 3 && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        let mut out = String::from("#");
        for b in hex.bytes() {
            out.push(b.to_ascii_lowercase() as char);
            out.push(b.to_ascii_lowercase() as char);
        }
        return out;
    }
    DEFAULT_ACCENT_COLOR.to_string()
}

/// Keep at most two letters or digits for a custom chip. Empty means automatic.
pub fn sanitize_avatar_text(raw: &str) -> String {
    let mut out = String::new();
    for ch in raw.chars().filter(|c| c.is_alphanumeric()).take(2) {
        out.extend(ch.to_uppercase());
    }
    out
}

/// Space-separated initials so libadwaita Avatar shows every letter (`CI` → `C I`).
pub fn avatar_chip_words(chip: &str) -> String {
    chip.chars()
        .map(|ch| ch.to_string())
        .collect::<Vec<_>>()
        .join(" ")
}

/// IMAP/SMTP login. Empty username means use the email address.
/// SMTP login an OAuth account keeps after `tokens` arrive.
///
/// Microsoft's SMTP server accepts only the account's primary sign-in name, even
/// when IMAP accepts an alias. Without a reported name, the current login stays.
pub fn oauth_smtp_login(current: &str, tokens: Option<&OAuthTokens>) -> String {
    tokens
        .and_then(|tokens| tokens.sign_in_name.as_deref())
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or(current)
        .to_string()
}

pub fn login_name_from(username: &str, email: &str) -> String {
    let username = username.trim();
    if username.is_empty() {
        email.trim().to_string()
    } else {
        username.to_string()
    }
}

/// Username to persist when the mailbox address may have changed.
///
/// The settings UI prefills the username row with `login_name()` (the old
/// mailbox address when login was implicit or backfilled). An untouched row
/// that still shows the old address must stay implicit so login follows the
/// new mailbox; anything the user actually typed is kept verbatim.
pub fn username_for_email_change(row_username: &str, old_email: &str, new_email: &str) -> String {
    if !new_email.eq_ignore_ascii_case(old_email)
        && row_username.trim().eq_ignore_ascii_case(old_email.trim())
    {
        String::new()
    } else {
        row_username.to_string()
    }
}

fn capitalize_local_part(local: &str) -> String {
    let mut chars = local.chars();
    match chars.next() {
        None => String::new(),
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
    }
}
