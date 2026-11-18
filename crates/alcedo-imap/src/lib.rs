//! IMAP client (P6).

pub mod client;
pub mod decode;
pub mod fetch;
pub mod idle;
pub mod tls;
pub mod xoauth2;

pub use client::{
    AppendedMail, ImapAuth, ImapClient, ImapConnectOptions, ImapServerCaps, ListedFolder,
    MailboxStatus, SelectedMailbox, fetch_is_flagged, fetch_is_seen, format_uid_set_chunks,
    imap_date_text, imap_header_message_id_search, imap_quoted_astring,
    imap_search_before_inclusive, parse_exists_count, parse_expunge_seq,
    uid_fetch_query_with_changedsince,
};
pub use fetch::{
    AttachmentSection, DISPLAY_FULL_MAX_BYTES, DisplayFetch, DisplayPlan, ENVELOPE_FETCH_QUERY,
    FetchedAttachment, FetchedEnvelope, FullMessage, attachment_bytes_are_undecoded_base64,
    compose_draft_from_full_message, fetch_envelope_meta_by_seq, fetch_envelopes_by_seq,
    fetch_server_flags, parse_full_message, parse_routing_headers, structure_has_file_attachment,
    uid_fetch_attachment_flags, uid_fetch_attachment_sections, uid_fetch_display_message,
    uid_fetch_display_plan, uid_fetch_envelope_meta, uid_fetch_envelopes, uid_fetch_full_message,
    uid_fetch_full_message_with_date, uid_fetch_full_messages, uid_fetch_list_headers,
    uid_fetch_reply_headers, uid_fetch_rfc822_sizes, uid_fetch_routing_headers,
};
pub use idle::{IDLE_RESTART_TIMEOUT, IdleNotification, IdleSession, IdleWaitResult};
pub use tls::{ImapTransport, connect_tcp, connect_tls, upgrade_to_tls};
pub use xoauth2::{Xoauth2Authenticator, encode_xoauth2, xoauth2_sasl_string};
