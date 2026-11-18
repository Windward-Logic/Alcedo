//! Core domain types for Alcedo.

mod account;
mod attachment;
mod attachment_integrity;
pub mod byte_lru;
mod calendar;
mod calendar_details;
mod calendar_ics_details;
mod calendar_invite_link;
mod calendar_rsvp;
mod calendar_vtimezone;
mod calendar_windows_zones;
mod calendar_zone;
mod commands;
pub mod compose_checks;
pub mod compose_status;
mod contact;
mod envelope;
mod error;
mod events;
mod extension;
mod folder_ops;
mod identity;
mod ids;
pub mod layout_policy;
mod line_endings;
mod list_unsubscribe;
pub mod mail_feedback;
mod mail_provider;
mod mailbox_utf7;
mod message_id;
mod mime;
mod pgp;
pub mod pgp_copy;
pub mod private_fs;
pub mod provider_label;
pub mod queued_send;
mod reaction;
mod remote_images;
mod reply_planner;
mod routing;
pub mod row_summary;
mod rules;
mod search_query;
mod sender;
mod snippet;
mod subject;
mod url_redaction;
pub mod welcome;

pub use account::{
    Account, AuthMethod, DEFAULT_ACCENT_COLOR, Folder, OAuthProvider, OAuthTokens, SpecialUse,
    avatar_chip_words, avatar_from_label, display_name_from_email, login_name_from,
    oauth_smtp_login, sanitize_accent_color, sanitize_avatar_text, username_for_email_change,
};
pub use attachment::{
    AttachmentLists, DEFAULT_COMPOSE_SIZE_LIMIT, DEFAULT_RISKY_ATTACHMENT_EXTENSIONS,
    DEFAULT_RISKY_ATTACHMENT_MIME_TYPES, DEFAULT_TYPICAL_INLINE_IMAGE_EXTENSIONS,
    MAX_COMPOSE_INLINES, attachment_is_downloadable, attachment_size_warning, compose_size_limit,
    content_id_is_safe, extension_of, filename_is_safe, format_type_list, parse_type_list,
    part_is_file_attachment, tokens_from_strv,
};
pub use attachment_integrity::{
    attachment_bytes_are_undecoded_base64, binary_file_magic, raster_image_mime,
};
pub use calendar::{
    CalendarEventDraft, InviteMethod, ParsedInvite, build_ics, build_vevent_from_draft,
    is_calendar_part, parse_ics, parse_ics_with_method, recurrence_id_equals,
};
pub use calendar_details::{
    Attendee, EventDetails, EventPerson, MAX_ATTENDEES, ParticipationStatus, find_join_url,
};
pub use calendar_invite_link::{draft_from_google_template_url, is_calendar_invite_uri};
pub use calendar_rsvp::{
    apply_participation, build_reply_ics, reply_subject as calendar_invite_reply_subject,
    rsvp_responder_email,
};
pub use calendar_windows_zones::windows_zone_to_iana;
pub use calendar_zone::{
    ResolvedZone, ZoneResolver, local_day_start_utc, system_zone_name, wall_to_utc,
    zone_abbreviation, zone_name_to_iana,
};
pub use commands::{
    AccountDraft, AccountSettingsUpdate, ComposeAttachment, ComposeDraft, ComposeFormat,
    ComposeInline, ImapDraftRef, UiCommand,
};
pub use contact::{
    Contact, ContactDraft, ContactGroup, ContactStatus, GroupDraft, UNAPPROVED_GROUP_ID,
    UNAPPROVED_GROUP_NAME, block_contact_in_memory, collect_outgoing_into,
    promote_contact_in_memory, remove_contacts_in_memory,
};
pub use envelope::{
    Address, Conversation, Envelope, MessageListMode, PREVIEW_SCAN_MAX, PREVIEW_SNIPPET_MAX,
    ServerFlags, address_emails_equal, clean_display_name, clean_preview_text, contains_html_tag,
    format_mailbox, group_envelopes, html_tag_prefix, parse_address_token, participant_summary,
    preview_scan, preview_snippet, preview_without_embedded_mime, show_distinct_address_row,
};
pub use error::{
    Error, Result, SYNC_NETWORK_TOAST_GRACE, auth_retry_delay, is_auth_failure,
    is_keyring_auth_message, looks_like_network_failure, should_toast_sync_failure,
};
pub use events::{BodyAttachment, CidInline, EnvelopeListKind, SendState, SyncEvent};
pub use extension::{ExtensionListId, ExtensionMessage};
pub use folder_ops::{
    descendant_imap_names, effective_mailbox_delimiter, folder_delimiter_for_parent,
    folder_display_name, folder_is_mutable, imap_name_exists, join_mailbox_path, nested_imap_names,
    rename_imap_map, validate_folder_leaf,
};
pub use identity::{
    AccountIdentity, ComposeFromChoice, SmtpEnvelopePolicy, mime_from_for_draft,
    resolve_compose_from_text, same_email_domain, smtp_mail_from,
};
pub use ids::{
    AccountId, ContactId, FolderId, GroupId, IdentityId, MessageId, QueuedSendId, RuleId,
    SnippetId, ThreadId, orphan_thread_id, sentinel_thread_id, thread_id_from_root,
    thread_root_token,
};
pub use line_endings::crlf_line_endings;
pub use list_unsubscribe::{
    ListUnsubscribe, MailtoUnsubscribe, https_host_is_blocked, https_uri_host, ip_addr_is_blocked,
    parse_list_unsubscribe,
};
pub use mail_provider::MailProvider;
pub use mailbox_utf7::{decode_mailbox_name, encode_mailbox_name};
pub use message_id::{msg_id_list, reply_in_reply_to, reply_references};
pub use mime::decode_mime_header;
pub use pgp::{
    EncryptReadiness, PgpAccountState, PgpFingerprint, PgpKeySource, PgpPeerInfo,
    PgpSignatureState, PgpStatus, email_from_address_token, format_fingerprint_display,
    recipient_encrypt_ready,
};
pub use provider_label::{mail_provider_labels, smtp_security_labels};
pub use queued_send::QueuedSend;
pub use reaction::{
    EmailReaction, GroupedReaction, ReactionPerson, bare_reaction_target_hdr,
    disposition_is_attachment, encrypted_reaction_block, escape_reaction_html, group_reactions,
    is_single_reaction_emoji, parse_email_reaction, parse_email_reaction_from_message,
    reaction_block_reason, reaction_bodies, reaction_from_display_name, reaction_reader_body,
    sanitize_sender_name,
};
pub use remote_images::{normalize_sender_email, should_load_remote_images};
pub use reply_planner::{
    ReplyFrom, ReplyMode, ReplyPlan, ReplyPlanner, SelfAddresses, parse_catch_all_domains,
};
pub use routing::{RoutingHeaders, parse_list_id, parse_list_post};
pub use row_summary::{RowSummary, row_accessible_summary};
pub use rules::{
    MessageRule, RuleAction, RuleCondition, RuleExecution, RuleField, RuleMatchMode, RuleOp,
    RulePlan, ServerRuleAction, ServerRuleView, ServerRulesPreview, ServerRulesStatus,
    default_stop_processing, plan_rules, rule_has_server_capable_conditions, rule_matches,
    rule_summary, server_rule_summary, validate_rule,
};
pub use search_query::{
    FolderToken, ScopeBake, SearchQuery, apply_is_token, ensure_scope_in_query, fts_match_clause,
    hide_special_in_unscoped_search, parse_search_query, query_has_folder_operator,
};
pub use sender::{canonical_email, is_noreply_address, sender_is_blocked};
pub use snippet::{AccountSignature, Snippet, expand_snippet_placeholders};
pub use subject::{NO_SUBJECT, reply_subject};
pub use url_redaction::redact_url_userinfo;

#[cfg(test)]
mod tests {
    use super::*;

    use uuid::Uuid;

    #[test]
    fn account_id_uuid_serde_roundtrip() {
        let id = AccountId(Uuid::new_v4());
        let json = serde_json::to_string(&id).expect("serialize");
        let back: AccountId = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(id, back);
    }

    #[test]
    fn thread_id_uuid_serde_roundtrip() {
        let id = ThreadId(Uuid::new_v4());
        let json = serde_json::to_string(&id).expect("serialize");
        let back: ThreadId = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(id, back);
    }

    #[test]
    fn thread_root_token_prefers_references_first_token() {
        assert_eq!(
            thread_root_token(
                Some("<solo@example.com>"),
                Some("<reply@example.com>"),
                Some("<root@example.com> <other@example.com>")
            ),
            Some("<root@example.com>".to_string())
        );
    }

    #[test]
    fn thread_root_token_falls_back_to_in_reply_to() {
        assert_eq!(
            thread_root_token(Some("<solo@example.com>"), Some("<irt@example.com>"), None),
            Some("<irt@example.com>".to_string())
        );
    }

    #[test]
    fn thread_root_token_falls_back_to_message_id() {
        assert_eq!(
            thread_root_token(Some("<solo@example.com>"), None, None),
            Some("<solo@example.com>".to_string())
        );
    }

    #[test]
    fn thread_root_token_skips_empty_strings() {
        assert_eq!(thread_root_token(Some(""), Some("  "), Some("  \t")), None);
        assert_eq!(
            thread_root_token(Some("<a@b>"), Some(""), Some("   ")),
            Some("<a@b>".to_string())
        );
    }

    #[test]
    fn preset_for_user_gmail_com() {
        let draft = Account::preset_for_email("user@gmail.com").expect("preset");
        assert_eq!(draft.email, "user@gmail.com");
        assert_eq!(draft.imap_host, "imap.gmail.com");
        assert_eq!(draft.imap_port, 993);
        assert_eq!(draft.smtp_host, "smtp.gmail.com");
        assert_eq!(draft.smtp_port, 465);
        assert!(!draft.smtp_starttls);
        assert!(matches!(
            draft.auth,
            AuthMethod::OAuth2 {
                provider: OAuthProvider::Google
            }
        ));
    }

    #[test]
    fn preset_for_yahoo_com() {
        let draft = Account::preset_for_email("user@yahoo.com").expect("preset");
        assert_eq!(draft.imap_host, "imap.mail.yahoo.com");
        assert_eq!(draft.smtp_host, "smtp.mail.yahoo.com");
        assert!(matches!(draft.auth, AuthMethod::Password));
    }

    #[test]
    fn personal_microsoft_presets_use_the_outlook_com_smtp_host() {
        for email in ["user@outlook.com", "user@hotmail.com", "user@Live.com"] {
            let draft = Account::preset_for_email(email).expect("preset");
            assert_eq!(draft.smtp_host, "smtp-mail.outlook.com", "{email}");
            assert_eq!(draft.smtp_port, 587, "{email}");
            assert!(draft.smtp_starttls, "{email}");
            assert!(
                OAuthProvider::Microsoft
                    .validate_mail_hosts(&draft.imap_host, &draft.smtp_host)
                    .is_ok(),
                "{email}"
            );
        }
    }

    #[test]
    fn microsoft_365_preset_keeps_the_office365_smtp_host() {
        let draft = Account::preset_for_email("user@office365.com").expect("preset");
        assert_eq!(draft.smtp_host, "smtp.office365.com");
    }

    #[test]
    fn microsoft_oauth_tokens_only_go_to_microsoft_smtp_hosts() {
        let microsoft = OAuthProvider::Microsoft;
        assert!(microsoft.validate_smtp_host("smtp.office365.com").is_ok());
        assert!(
            microsoft
                .validate_smtp_host("SMTP-MAIL.outlook.com.")
                .is_ok()
        );
        assert!(microsoft.validate_smtp_host("smtp.example.com").is_err());
        assert!(
            OAuthProvider::Google
                .validate_smtp_host("smtp-mail.outlook.com")
                .is_err()
        );
    }

    #[test]
    fn preset_for_unknown_domain_returns_none() {
        assert!(Account::preset_for_email("user@unknown.example").is_none());
    }

    #[test]
    fn preset_for_email_domain_is_case_insensitive() {
        let draft = Account::preset_for_email("user@Gmail.COM").expect("preset");
        assert_eq!(draft.imap_host, "imap.gmail.com");
        assert!(matches!(
            draft.auth,
            AuthMethod::OAuth2 {
                provider: OAuthProvider::Google
            }
        ));
    }

    #[test]
    fn display_name_from_email_capitalizes_local_part() {
        assert_eq!(
            crate::account::display_name_from_email("alice@example.com"),
            "Alice"
        );
    }

    #[test]
    fn avatar_from_label_prefers_words_then_camel_then_first() {
        assert_eq!(crate::avatar_from_label("Code Infusion"), "CI");
        assert_eq!(crate::avatar_from_label("CodeInfusion"), "CI");
        assert_eq!(crate::avatar_from_label("GMail"), "GM");
        assert_eq!(crate::avatar_from_label("Alcedo"), "A");
        assert_eq!(crate::avatar_from_label("GMAIL"), "G");
        assert_eq!(crate::avatar_from_label("Alice Gmail"), "AG");
        assert_eq!(crate::avatar_from_label("Code-Infusion"), "CI");
        assert_eq!(crate::avatar_from_label("carter@codeinfusion.com"), "C");
        assert_eq!(crate::avatar_from_label(""), "?");
    }

    #[test]
    fn sanitize_avatar_text_keeps_two_alphanumerics() {
        assert_eq!(crate::sanitize_avatar_text("ca"), "CA");
        assert_eq!(crate::sanitize_avatar_text("  ci! "), "CI");
        assert_eq!(crate::sanitize_avatar_text("Code"), "CO");
        assert_eq!(crate::sanitize_avatar_text(""), "");
        assert_eq!(crate::avatar_chip_words("CI"), "C I");
        assert_eq!(crate::avatar_chip_words("A"), "A");
    }

    #[test]
    fn sanitize_accent_color_normalizes_hex() {
        assert_eq!(crate::sanitize_accent_color("#3584E4"), "#3584e4");
        assert_eq!(crate::sanitize_accent_color("abc"), "#aabbcc");
        assert_eq!(crate::sanitize_accent_color("  #FFF  "), "#ffffff");
        assert_eq!(
            crate::sanitize_accent_color("not-a-color"),
            crate::DEFAULT_ACCENT_COLOR
        );
    }

    #[test]
    fn login_name_from_falls_back_to_email() {
        assert_eq!(
            crate::account::login_name_from("", "alice@example.com"),
            "alice@example.com"
        );
        assert_eq!(
            crate::account::login_name_from("  imapuser  ", "alice@example.com"),
            "imapuser"
        );
    }

    #[test]
    fn username_for_email_change_clears_implicit_old_login() {
        let old = "alice@example.com";
        let new = "bob@example.com";
        assert_eq!(crate::username_for_email_change(old, old, new), "");
        assert_eq!(
            crate::username_for_email_change("  ALICE@example.com  ", old, new),
            ""
        );
        assert_eq!(crate::username_for_email_change(old, old, old), old);
        assert_eq!(
            crate::username_for_email_change("imapuser", old, new),
            "imapuser"
        );
    }

    #[test]
    fn account_label_falls_back_to_email() {
        let named = crate::Account {
            id: crate::AccountId(uuid::Uuid::nil()),
            email: "alice@example.com".to_string(),
            username: "imapuser".to_string(),
            split_credentials: false,
            smtp_username: String::new(),
            display_name: "Work".to_string(),
            sender_name: String::new(),
            imap_host: String::new(),
            imap_port: 993,
            smtp_host: String::new(),
            smtp_port: 465,
            smtp_starttls: false,
            auth: crate::AuthMethod::Password,
            accent_color: String::new(),
            avatar_text: String::new(),
            avatar_image: Vec::new(),
        };
        assert_eq!(named.label(), "Work");
        assert_eq!(named.avatar_chip(), "W");
        let custom = crate::Account {
            avatar_text: "ca!".to_string(),
            ..named.clone()
        };
        assert_eq!(custom.avatar_chip(), "CA");
        assert_eq!(named.login_name(), "imapuser");
        let unnamed = crate::Account {
            display_name: String::new(),
            username: String::new(),
            ..named
        };
        assert_eq!(unnamed.label(), "alice@example.com");
        assert_eq!(unnamed.login_name(), "alice@example.com");
    }

    fn valid_password_draft() -> AccountDraft {
        AccountDraft {
            email: "user@example.com".to_string(),
            username: "user@example.com".to_string(),
            display_name: "User".to_string(),
            imap_host: "imap.example.com".to_string(),
            imap_port: 993,
            smtp_host: "smtp.example.com".to_string(),
            smtp_port: 587,
            smtp_starttls: true,
            auth: AuthMethod::Password,
            password: Some("secret".to_string()),
            oauth_tokens: None,
            split_credentials: false,
            smtp_username: String::new(),
            smtp_password: None,
        }
    }

    #[test]
    fn validate_password_auth_ok() {
        assert!(valid_password_draft().validate_password_auth().is_ok());
    }

    #[test]
    fn validate_password_auth_empty_host() {
        let mut draft = valid_password_draft();
        draft.imap_host.clear();
        assert_eq!(draft.validate_password_auth(), Err("IMAP host is required"));
    }

    #[test]
    fn validate_password_auth_port_zero() {
        let mut draft = valid_password_draft();
        draft.imap_port = 0;
        assert_eq!(
            draft.validate_password_auth(),
            Err("IMAP port must be between 1 and 65535")
        );
    }

    #[test]
    fn validate_password_auth_missing_at() {
        let mut draft = valid_password_draft();
        draft.email = "notanemail".to_string();
        assert_eq!(draft.validate_password_auth(), Err("Email must contain @"));
    }

    #[test]
    fn validate_password_auth_missing_password() {
        let mut draft = valid_password_draft();
        draft.password = None;
        assert_eq!(draft.validate_password_auth(), Err("Password is required"));
    }

    #[test]
    fn account_draft_debug_redacts_password() {
        let draft = valid_password_draft();
        let debug = format!("{draft:?}");
        assert!(!debug.contains("secret"));
        assert!(debug.contains("***"));
    }

    fn valid_settings_update() -> AccountSettingsUpdate {
        AccountSettingsUpdate {
            email: "user@example.com".to_string(),
            display_name: "User".to_string(),
            sender_name: String::new(),
            avatar_text: String::new(),
            accent_color: String::new(),
            avatar_image: Vec::new(),
            username: "user@example.com".to_string(),
            imap_host: "imap.example.com".to_string(),
            imap_port: 993,
            smtp_host: "smtp.example.com".to_string(),
            smtp_port: 587,
            smtp_starttls: true,
            password: None,
            oauth_tokens: None,
            split_credentials: false,
            smtp_username: String::new(),
            smtp_password: None,
        }
    }

    #[test]
    fn validate_settings_update_ok() {
        assert!(valid_settings_update().validate().is_ok());
    }

    #[test]
    fn validate_settings_update_rejects_bad_email() {
        let mut update = valid_settings_update();
        update.email = "not-an-email".to_string();
        assert_eq!(update.validate(), Err("Email must contain @"));
        update.email = "  ".to_string();
        assert_eq!(update.validate(), Err("Email is required"));
        update.email = "@example.com".to_string();
        assert_eq!(update.validate(), Err("Email is invalid"));
    }

    #[test]
    fn validate_settings_update_empty_imap_host() {
        let mut update = valid_settings_update();
        update.imap_host.clear();
        assert_eq!(update.validate(), Err("IMAP host is required"));
    }

    #[test]
    fn validate_settings_update_port_zero() {
        let mut update = valid_settings_update();
        update.imap_port = 0;
        assert_eq!(
            update.validate(),
            Err("IMAP port must be between 1 and 65535")
        );
    }

    #[test]
    fn account_settings_update_debug_redacts_password() {
        let mut update = valid_settings_update();
        update.password = Some("secret".to_string());
        let debug = format!("{update:?}");
        assert!(!debug.contains("secret"));
        assert!(debug.contains("***"));
    }

    fn valid_oauth_draft() -> AccountDraft {
        AccountDraft {
            email: "user@gmail.com".to_string(),
            username: "user@gmail.com".to_string(),
            display_name: "User".to_string(),
            imap_host: "imap.gmail.com".to_string(),
            imap_port: 993,
            smtp_host: "smtp.gmail.com".to_string(),
            smtp_port: 465,
            smtp_starttls: false,
            auth: AuthMethod::OAuth2 {
                provider: OAuthProvider::Google,
            },
            password: None,
            oauth_tokens: Some(OAuthTokens {
                access_token: "ya29.access".to_string(),
                refresh_token: Some("1//refresh".to_string()),
                expires_in: Some(3600),
                sign_in_name: None,
            }),
            split_credentials: false,
            smtp_username: String::new(),
            smtp_password: None,
        }
    }

    #[test]
    fn smtp_login_name_shared_mode_ignores_smtp_username() {
        let account = crate::Account {
            id: crate::AccountId(uuid::Uuid::nil()),
            email: "user@example.com".to_string(),
            username: "imapuser".to_string(),
            split_credentials: false,
            smtp_username: "smtp-user".to_string(),
            display_name: String::new(),
            sender_name: String::new(),
            imap_host: String::new(),
            imap_port: 993,
            smtp_host: String::new(),
            smtp_port: 465,
            smtp_starttls: false,
            auth: crate::AuthMethod::Password,
            accent_color: String::new(),
            avatar_text: String::new(),
            avatar_image: Vec::new(),
        };
        assert_eq!(account.smtp_login_name(), account.login_name());
    }

    #[test]
    fn oauth_account_sends_with_its_sign_in_name_and_reads_with_its_address() {
        let account = crate::Account {
            id: crate::AccountId(uuid::Uuid::nil()),
            email: "alias@outlook.com".to_string(),
            username: "alias@outlook.com".to_string(),
            split_credentials: false,
            smtp_username: "primary@example.com".to_string(),
            display_name: String::new(),
            sender_name: String::new(),
            imap_host: String::new(),
            imap_port: 993,
            smtp_host: String::new(),
            smtp_port: 587,
            smtp_starttls: true,
            auth: crate::AuthMethod::OAuth2 {
                provider: OAuthProvider::Microsoft,
            },
            accent_color: String::new(),
            avatar_text: String::new(),
            avatar_image: Vec::new(),
        };
        assert_eq!(account.smtp_login_name(), "primary@example.com");
        assert_eq!(account.login_name(), "alias@outlook.com");
    }

    fn account_for_smtp_auth(auth: crate::AuthMethod, smtp_username: &str) -> crate::Account {
        crate::Account {
            id: crate::AccountId(uuid::Uuid::nil()),
            email: "alias@outlook.com".to_string(),
            username: "custom-login".to_string(),
            split_credentials: false,
            smtp_username: smtp_username.to_string(),
            display_name: String::new(),
            sender_name: String::new(),
            imap_host: String::new(),
            imap_port: 993,
            smtp_host: String::new(),
            smtp_port: 587,
            smtp_starttls: true,
            auth,
            accent_color: String::new(),
            avatar_text: String::new(),
            avatar_image: Vec::new(),
        }
    }

    #[test]
    fn microsoft_oauth_authenticates_smtp_with_the_sign_in_name() {
        let account = account_for_smtp_auth(
            crate::AuthMethod::OAuth2 {
                provider: OAuthProvider::Microsoft,
            },
            "primary@example.com",
        );
        assert_eq!(account.smtp_auth_username(), "primary@example.com");
    }

    #[test]
    fn microsoft_oauth_without_a_sign_in_name_authenticates_smtp_with_the_address() {
        let account = account_for_smtp_auth(
            crate::AuthMethod::OAuth2 {
                provider: OAuthProvider::Microsoft,
            },
            "",
        );
        assert_eq!(account.smtp_auth_username(), "alias@outlook.com");
    }

    #[test]
    fn google_oauth_authenticates_smtp_with_the_address() {
        let account = account_for_smtp_auth(
            crate::AuthMethod::OAuth2 {
                provider: OAuthProvider::Google,
            },
            "stale-smtp-user",
        );
        assert_eq!(account.smtp_auth_username(), "alias@outlook.com");
    }

    #[test]
    fn password_account_authenticates_smtp_with_its_login() {
        let account = account_for_smtp_auth(crate::AuthMethod::Password, "");
        assert_eq!(account.smtp_auth_username(), "custom-login");
    }

    #[test]
    fn smtp_login_name_split_with_explicit_username() {
        let account = crate::Account {
            id: crate::AccountId(uuid::Uuid::nil()),
            email: "user@example.com".to_string(),
            username: "imapuser".to_string(),
            split_credentials: true,
            smtp_username: "smtp-user".to_string(),
            display_name: String::new(),
            sender_name: String::new(),
            imap_host: String::new(),
            imap_port: 993,
            smtp_host: String::new(),
            smtp_port: 465,
            smtp_starttls: false,
            auth: crate::AuthMethod::Password,
            accent_color: String::new(),
            avatar_text: String::new(),
            avatar_image: Vec::new(),
        };
        assert_eq!(account.smtp_login_name(), "smtp-user");
    }

    #[test]
    fn smtp_login_name_split_empty_username_falls_back_to_email() {
        let account = crate::Account {
            id: crate::AccountId(uuid::Uuid::nil()),
            email: "user@example.com".to_string(),
            username: "imapuser".to_string(),
            split_credentials: true,
            smtp_username: String::new(),
            display_name: String::new(),
            sender_name: String::new(),
            imap_host: String::new(),
            imap_port: 993,
            smtp_host: String::new(),
            smtp_port: 465,
            smtp_starttls: false,
            auth: crate::AuthMethod::Password,
            accent_color: String::new(),
            avatar_text: String::new(),
            avatar_image: Vec::new(),
        };
        assert_eq!(account.smtp_login_name(), "user@example.com");
    }

    #[test]
    fn oauth_never_uses_split_credentials() {
        let account = crate::Account {
            id: crate::AccountId(uuid::Uuid::nil()),
            email: "user@gmail.com".to_string(),
            username: String::new(),
            split_credentials: true,
            smtp_username: "smtp-user".to_string(),
            display_name: String::new(),
            sender_name: String::new(),
            imap_host: String::new(),
            imap_port: 993,
            smtp_host: String::new(),
            smtp_port: 465,
            smtp_starttls: false,
            auth: crate::AuthMethod::OAuth2 {
                provider: crate::OAuthProvider::Google,
            },
            accent_color: String::new(),
            avatar_text: String::new(),
            avatar_image: Vec::new(),
        };
        assert!(!account.uses_split_credentials());
        assert_eq!(account.smtp_login_name(), account.login_name());
    }

    #[test]
    fn validate_password_auth_requires_smtp_password_when_split() {
        let mut draft = valid_password_draft();
        draft.split_credentials = true;
        draft.smtp_password = None;
        assert_eq!(
            draft.validate_password_auth(),
            Err("SMTP password is required")
        );
    }

    #[test]
    fn validate_password_auth_split_off_ignores_smtp_password() {
        let mut draft = valid_password_draft();
        draft.split_credentials = false;
        draft.smtp_password = None;
        assert!(draft.validate_password_auth().is_ok());
    }

    #[test]
    fn account_draft_debug_redacts_smtp_password() {
        let mut draft = valid_password_draft();
        draft.smtp_password = Some("secret".to_string());
        let debug = format!("{draft:?}");
        assert!(!debug.contains("secret"));
        assert!(debug.contains("***"));
    }

    #[test]
    fn validate_oauth_auth_ok() {
        assert!(valid_oauth_draft().validate_oauth_auth().is_ok());
    }

    #[test]
    fn validate_oauth_auth_rejects_foreign_imap_host() {
        let mut draft = valid_oauth_draft();
        draft.imap_host = "imap.evil.example".to_string();
        assert_eq!(
            draft.validate_oauth_auth(),
            Err("IMAP host must match the OAuth provider")
        );
    }

    #[test]
    fn validate_oauth_auth_rejects_foreign_smtp_host() {
        let mut draft = valid_oauth_draft();
        draft.smtp_host = "smtp.evil.example".to_string();
        assert_eq!(
            draft.validate_oauth_auth(),
            Err("SMTP host must match the OAuth provider")
        );
    }

    #[test]
    fn oauth_host_compare_is_case_insensitive() {
        assert!(
            OAuthProvider::Google
                .validate_mail_hosts("IMAP.Gmail.COM.", "SMTP.GMAIL.COM")
                .is_ok()
        );
    }

    #[test]
    fn validate_settings_update_pins_oauth_hosts() {
        let mut update = valid_settings_update();
        update.imap_host = "imap.gmail.com".to_string();
        update.smtp_host = "smtp.gmail.com".to_string();
        let auth = AuthMethod::OAuth2 {
            provider: OAuthProvider::Google,
        };
        assert!(update.validate_for_auth(&auth).is_ok());

        update.imap_host = "imap.example.com".to_string();
        assert_eq!(
            update.validate_for_auth(&auth),
            Err("IMAP host must match the OAuth provider")
        );
    }

    #[test]
    fn validate_settings_update_allows_custom_hosts_for_password() {
        assert!(
            valid_settings_update()
                .validate_for_auth(&AuthMethod::Password)
                .is_ok()
        );
    }

    #[test]
    fn compose_draft_without_format_deserializes_as_plain() {
        let json = r#"{
            "account_id": "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
            "to": [],
            "cc": [],
            "bcc": [],
            "subject": "",
            "body_text": "hi",
            "request_mdn": false,
            "tracking_pixel_url": null
        }"#;
        let draft: ComposeDraft = serde_json::from_str(json).expect("deserialize");
        assert_eq!(draft.format, ComposeFormat::Plain);
        assert!(draft.attachments.is_empty());
        assert!(draft.inlines.is_empty());
        assert!(draft.thread_id.is_none());
    }

    #[test]
    fn compose_draft_without_thread_id_deserializes() {
        let json = r#"{
            "account_id": "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
            "to": [],
            "cc": [],
            "bcc": [],
            "subject": "",
            "body_text": "hi",
            "request_mdn": false,
            "tracking_pixel_url": null
        }"#;
        let draft: ComposeDraft = serde_json::from_str(json).expect("deserialize");
        assert!(draft.thread_id.is_none());
    }

    #[test]
    fn compose_attachment_debug_redacts_bytes() {
        let part = ComposeAttachment {
            filename: "note.txt".to_string(),
            mime: "text/plain".to_string(),
            bytes: b"secret".to_vec(),
        };
        let debug = format!("{part:?}");
        assert!(!debug.contains("secret"));
        assert!(debug.contains("len=6"));
    }

    #[test]
    fn compose_inline_debug_redacts_bytes() {
        let part = ComposeInline {
            content_id: "abc@alcedo.mail".to_string(),
            filename: "photo.png".to_string(),
            mime: "image/png".to_string(),
            bytes: b"secret".to_vec(),
        };
        let debug = format!("{part:?}");
        assert!(!debug.contains("secret"));
        assert!(debug.contains("len=6"));
    }

    #[test]
    fn filename_and_content_id_safety() {
        assert!(filename_is_safe("invoice.pdf"));
        assert!(!filename_is_safe("evil\r\n.txt"));
        assert!(!filename_is_safe("a/b.txt"));
        assert!(content_id_is_safe("abc-def@alcedo.mail"));
        assert!(!content_id_is_safe("abc"));
        assert!(!content_id_is_safe("abc@alcedo.mail\r\n"));
    }

    #[test]
    fn compose_draft_without_identity_or_imap_deserializes() {
        let json = r#"{
            "account_id": "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
            "to": [],
            "cc": [],
            "bcc": [],
            "subject": "",
            "body_text": "hi",
            "request_mdn": false,
            "tracking_pixel_url": null
        }"#;
        let draft: ComposeDraft = serde_json::from_str(json).expect("deserialize");
        assert!(draft.identity_id.is_none());
        assert!(draft.imap_draft.is_none());
    }

    #[test]
    fn expand_snippet_placeholders_with_contact() {
        use chrono::Utc;
        let contact = Contact {
            id: ContactId(1),
            display_name: "Jane Doe".to_string(),
            email: "jane@example.com".to_string(),
            nickname: String::new(),
            organization: String::new(),
            notes: String::new(),
            status: ContactStatus::Approved,
            group_ids: Vec::new(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            last_used_at: None,
        };
        let out = expand_snippet_placeholders(
            "Hi {{first_name}}, reach me at {{email}} ({{full_name}})",
            Some("jane@example.com"),
            Some(&contact),
        );
        assert_eq!(out, "Hi Jane, reach me at jane@example.com (Jane Doe)");
    }

    #[test]
    fn expand_snippet_placeholders_email_only() {
        let out = expand_snippet_placeholders(
            "{{first_name}} <{{email}}>",
            Some("alice.smith+tag@example.com"),
            None,
        );
        assert_eq!(out, "alice <alice.smith+tag@example.com>");
    }

    #[test]
    fn expand_snippet_placeholders_empty_to_leaves_tokens() {
        let out = expand_snippet_placeholders("Hi {{first_name}}", None, None);
        assert_eq!(out, "Hi {{first_name}}");
    }

    #[test]
    fn identity_validate_rejects_account_email() {
        let id = IdentityId(Uuid::new_v4());
        let account = AccountId(Uuid::new_v4());
        let identity = AccountIdentity {
            id,
            account_id: account,
            display_name: "Alias".to_string(),
            email: "user@example.com".to_string(),
        };
        assert_eq!(
            identity.validate("user@example.com"),
            Err("This address is already the account address")
        );
    }

    #[test]
    fn oauth_tokens_debug_redacts_secrets() {
        let tokens = OAuthTokens {
            access_token: "ya29.secret-access".to_string(),
            refresh_token: Some("1//secret-refresh".to_string()),
            expires_in: Some(3600),
            sign_in_name: Some("private@example.com".to_string()),
        };
        let debug = format!("{tokens:?}");
        assert!(debug.contains("***"));
        assert!(!debug.contains("secret-access"));
        assert!(!debug.contains("secret-refresh"));
        assert!(!debug.contains("private@example.com"));
        assert!(debug.contains("3600"));
    }

    fn tokens_signed_in_as(name: Option<&str>) -> OAuthTokens {
        OAuthTokens {
            access_token: "access".to_string(),
            refresh_token: None,
            expires_in: None,
            sign_in_name: name.map(str::to_string),
        }
    }

    #[test]
    fn oauth_smtp_login_uses_the_reported_sign_in_name() {
        let tokens = tokens_signed_in_as(Some(" primary@example.com "));
        assert_eq!(
            crate::oauth_smtp_login("old@example.com", Some(&tokens)),
            "primary@example.com"
        );
    }

    #[test]
    fn oauth_smtp_login_keeps_the_current_login_without_a_reported_name() {
        for tokens in [tokens_signed_in_as(None), tokens_signed_in_as(Some("  "))] {
            assert_eq!(
                crate::oauth_smtp_login("old@example.com", Some(&tokens)),
                "old@example.com"
            );
        }
        assert_eq!(
            crate::oauth_smtp_login("old@example.com", None),
            "old@example.com"
        );
    }

    #[test]
    fn oauth_tokens_saved_before_sign_in_names_still_load() {
        let tokens: OAuthTokens =
            serde_json::from_str(r#"{"access_token":"a","refresh_token":null,"expires_in":null}"#)
                .expect("older token JSON");
        assert_eq!(tokens.sign_in_name, None);
    }
}
