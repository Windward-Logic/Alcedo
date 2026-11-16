//! UID FETCH helpers for envelope sync and full message bodies.

use alcedo_core::{
    AccountId, Address, AttachmentLists, ComposeDraft, ComposeFormat, EmailReaction, Error,
    FolderId, IdentityId, ListUnsubscribe, Result, RoutingHeaders, ServerFlags, ThreadId,
    attachment_is_downloadable, decode_mime_header, orphan_thread_id,
    parse_email_reaction_from_message, parse_list_id, parse_list_post, parse_list_unsubscribe,
    preview_scan, thread_id_from_root,
};
use alcedo_html::sanitize_compose_html;
use async_imap::types::Fetch;
use std::collections::HashSet;

use chrono::{DateTime, TimeZone, Utc};
use futures::{Stream, StreamExt};
use mail_parser::decoders::quoted_printable::quoted_printable_decode;
use mail_parser::{MessageParser, MimeHeaders};

pub use alcedo_core::attachment_bytes_are_undecoded_base64;

use crate::client::{
    ImapClient, fetch_is_flagged, fetch_is_seen, uid_fetch_query_with_changedsince,
};
use crate::decode::{PartIntegrityError, check_section_length, decode_base64_body};

/// IMAP UID FETCH query for first folder sync (envelope + header peek + text peek).
///
/// Multiple fetch-atts must be parenthesized (RFC 3501). Zoho rejects the bare
/// form with `BAD [CLIENTBUG] syntax: expecting '('`.
///
/// TEXT peek is 32KiB so HTML-only newsletters (large `<style>` blocks before
/// visible copy) still yield a plaintext snippet after tag stripping.
pub const ENVELOPE_FETCH_QUERY: &str = concat!(
    "(FLAGS UID INTERNALDATE RFC822.SIZE ENVELOPE BODYSTRUCTURE ",
    "BODY.PEEK[HEADER.FIELDS (MESSAGE-ID IN-REPLY-TO REFERENCES LIST-UNSUBSCRIBE LIST-UNSUBSCRIBE-POST ",
    "MAIL-REPLY-TO MAIL-FOLLOWUP-TO LIST-POST LIST-ID DELIVERED-TO X-ORIGINAL-TO)] ",
    "BODY.PEEK[TEXT]<0.32768>)"
);

/// Header-only FETCH used to paint new rows before BODYSTRUCTURE and text peek.
///
/// Those two items are what make a Drafts APPEND take several seconds on a
/// large mailbox. The list only needs flags, envelope, and threading headers
/// to show the row; the full query still runs afterwards for the snippet.
pub const ENVELOPE_META_QUERY: &str = concat!(
    "(FLAGS UID INTERNALDATE RFC822.SIZE ENVELOPE ",
    "BODY.PEEK[HEADER.FIELDS (MESSAGE-ID IN-REPLY-TO REFERENCES LIST-UNSUBSCRIBE LIST-UNSUBSCRIBE-POST ",
    "MAIL-REPLY-TO MAIL-FOLLOWUP-TO LIST-POST LIST-ID DELIVERED-TO X-ORIGINAL-TO)])"
);

/// Envelope-level data from a first-sync UID FETCH.
#[derive(Clone, Debug)]
pub struct FetchedEnvelope {
    pub uid: u32,
    pub size: u32,
    pub subject: String,
    pub date: DateTime<Utc>,
    pub from: Vec<Address>,
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    pub reply_to: Vec<Address>,
    pub sender: Vec<Address>,
    pub snippet: String,
    pub seen: bool,
    pub flagged: bool,
    pub answered: bool,
    pub draft: bool,
    /// IMAP `\Deleted`. The message is waiting to be expunged and is not a live draft.
    pub imap_deleted: bool,
    pub has_attachments: bool,
    pub message_id: Option<String>,
    pub in_reply_to: Option<String>,
    pub references: Option<String>,
    pub thread_id: ThreadId,
    pub modseq: Option<u64>,
    pub list_unsubscribe: Option<ListUnsubscribe>,
    pub reaction_candidate: bool,
    pub routing: RoutingHeaders,
}

/// Attachment bytes extracted during full-message parse.
#[derive(Clone, Debug)]
pub struct FetchedAttachment {
    pub filename: String,
    pub cid: Option<String>,
    pub bytes: Vec<u8>,
    pub content_type: (String, String),
    pub disposition_attachment: bool,
}

/// Full RFC822 body from `OpenMessage`.
#[derive(Clone, Debug)]
pub struct FullMessage {
    pub raw: Vec<u8>,
    pub subject: String,
    pub text: String,
    pub html: String,
    pub list_unsubscribe: Option<ListUnsubscribe>,
    pub attachments: Vec<FetchedAttachment>,
    pub reaction: Option<EmailReaction>,
    /// File parts that could not be decoded completely (cut off before
    /// their closing boundary, or invalid transfer encoding). They are left
    /// out of `attachments` so their raw transfer text is never stored or
    /// saved as the file; callers fetch them again by section.
    pub incomplete_attachments: Vec<String>,
}

/// UID FETCH envelopes for the given UID set (e.g. `"1:*"` or `"100:200"`).
pub async fn uid_fetch_envelopes(
    client: &mut ImapClient,
    folder_id: FolderId,
    uid_set: &str,
    changed_since_modseq: Option<u64>,
) -> Result<Vec<FetchedEnvelope>> {
    let caps = client.capabilities().clone();
    let query =
        uid_fetch_query_with_changedsince(ENVELOPE_FETCH_QUERY, changed_since_modseq, &caps);
    let mut stream = client
        .session_mut()
        .uid_fetch(uid_set, &query)
        .await
        .map_err(|e| Error::Imap(format!("UID FETCH {uid_set}: {e}")))?;

    collect_fetched_envelopes(&mut stream, "UID FETCH", folder_id).await
}

/// Sequence FETCH of envelopes (e.g. `"4901:*"` for the last 100 of a 5000-message mailbox).
pub async fn fetch_envelopes_by_seq(
    client: &mut ImapClient,
    folder_id: FolderId,
    seq_set: &str,
) -> Result<Vec<FetchedEnvelope>> {
    let mut stream = client
        .session_mut()
        .fetch(seq_set, ENVELOPE_FETCH_QUERY)
        .await
        .map_err(|e| Error::Imap(format!("FETCH {seq_set}: {e}")))?;

    collect_fetched_envelopes(&mut stream, "FETCH", folder_id).await
}

/// Flags only, for a UID set (`by_uid`) or a message-sequence set.
///
/// With `changed_since` (CONDSTORE) the server returns only messages whose
/// flags changed after that modseq. Far cheaper than re-fetching envelopes,
/// BODYSTRUCTURE and a text peek just to learn that a message was read.
pub async fn fetch_server_flags(
    client: &mut ImapClient,
    set: &str,
    by_uid: bool,
    changed_since_modseq: Option<u64>,
) -> Result<Vec<ServerFlags>> {
    let caps = client.capabilities().clone();
    let query = uid_fetch_query_with_changedsince("(UID FLAGS)", changed_since_modseq, &caps);
    let session = client.session_mut();
    if by_uid {
        let mut stream = session
            .uid_fetch(set, &query)
            .await
            .map_err(|e| Error::Imap(format!("UID FETCH flags {set}: {e}")))?;
        collect_server_flags(&mut stream).await
    } else {
        let mut stream = session
            .fetch(set, &query)
            .await
            .map_err(|e| Error::Imap(format!("FETCH flags {set}: {e}")))?;
        collect_server_flags(&mut stream).await
    }
}

async fn collect_server_flags<S>(stream: &mut S) -> Result<Vec<ServerFlags>>
where
    S: Stream<Item = std::result::Result<Fetch, async_imap::error::Error>> + Unpin,
{
    let mut out = Vec::new();
    while let Some(item) = stream.next().await {
        let fetch = item.map_err(|e| Error::Imap(format!("FETCH flags item: {e}")))?;
        if let Some(uid) = fetch.uid {
            out.push(server_flags_from(uid, fetch.flags()));
        }
    }
    Ok(out)
}

fn server_flags_from<'a>(
    uid: u32,
    flags: impl Iterator<Item = async_imap::types::Flag<'a>>,
) -> ServerFlags {
    use async_imap::types::Flag;
    let mut out = ServerFlags {
        uid,
        seen: false,
        flagged: false,
        answered: false,
        draft: false,
        deleted: false,
    };
    for flag in flags {
        match flag {
            Flag::Seen => out.seen = true,
            Flag::Flagged => out.flagged = true,
            Flag::Answered => out.answered = true,
            Flag::Draft => out.draft = true,
            Flag::Deleted => out.deleted = true,
            _ => {}
        }
    }
    out
}

/// UID FETCH of list metadata only (no BODYSTRUCTURE, no text peek).
pub async fn uid_fetch_envelope_meta(
    client: &mut ImapClient,
    folder_id: FolderId,
    uid_set: &str,
) -> Result<Vec<FetchedEnvelope>> {
    let mut stream = client
        .session_mut()
        .uid_fetch(uid_set, ENVELOPE_META_QUERY)
        .await
        .map_err(|e| Error::Imap(format!("UID FETCH {uid_set}: {e}")))?;

    collect_fetched_envelopes(&mut stream, "UID FETCH", folder_id).await
}

/// Sequence FETCH of list metadata only (no BODYSTRUCTURE, no text peek).
pub async fn fetch_envelope_meta_by_seq(
    client: &mut ImapClient,
    folder_id: FolderId,
    seq_set: &str,
) -> Result<Vec<FetchedEnvelope>> {
    let mut stream = client
        .session_mut()
        .fetch(seq_set, ENVELOPE_META_QUERY)
        .await
        .map_err(|e| Error::Imap(format!("FETCH {seq_set}: {e}")))?;

    collect_fetched_envelopes(&mut stream, "FETCH", folder_id).await
}

async fn collect_fetched_envelopes<S>(
    stream: &mut S,
    what: &str,
    folder_id: FolderId,
) -> Result<Vec<FetchedEnvelope>>
where
    S: Stream<Item = std::result::Result<Fetch, async_imap::error::Error>> + Unpin,
{
    let mut out = Vec::new();
    while let Some(item) = stream.next().await {
        let fetch = item.map_err(|e| Error::Imap(format!("{what} item: {e}")))?;
        if let Some(env) = parse_fetched_envelope(&fetch, folder_id) {
            out.push(env);
        }
    }
    Ok(out)
}

/// `UID FETCH {uid} (BODY.PEEK[] INTERNALDATE)` for OpenMessage.
pub async fn uid_fetch_full_message(client: &mut ImapClient, uid: u32) -> Result<FullMessage> {
    Ok(uid_fetch_full_message_with_date(client, uid).await?.message)
}

/// Full RFC822 plus `INTERNALDATE` from one UID FETCH.
///
/// Draft open used to SELECT again and FETCH INTERNALDATE separately. One
/// parenthesized query saves those round trips.
const FULL_MESSAGE_WITH_DATE_QUERY: &str = "(BODY.PEEK[] INTERNALDATE)";

/// Body bytes and the server receive time from a single UID FETCH.
pub struct FullMessageWithDate {
    pub message: FullMessage,
    pub internaldate: Option<DateTime<Utc>>,
}

/// `UID FETCH {uid} (BODY.PEEK[] INTERNALDATE)`.
pub async fn uid_fetch_full_message_with_date(
    client: &mut ImapClient,
    uid: u32,
) -> Result<FullMessageWithDate> {
    // A stale `BODY[]<0>` probe response carries BODYSTRUCTURE and no
    // INTERNALDATE; requiring this response's shape keeps a 256 KiB prefix
    // from ever being parsed as the whole message.
    let fetch = uid_fetch_matching(
        client,
        uid,
        FULL_MESSAGE_WITH_DATE_QUERY,
        is_full_message_response,
    )
    .await?;
    let raw = fetch
        .body()
        .ok_or_else(|| Error::Imap(format!("uid {uid} missing BODY.PEEK[]")))?
        .to_vec();
    let internaldate = fetch.internal_date().map(|dt| dt.with_timezone(&Utc));
    Ok(FullMessageWithDate {
        message: parse_full_message(&raw)?,
        internaldate,
    })
}

/// `UID FETCH` full bodies for a set of UIDs (reaction peek batches).
pub async fn uid_fetch_full_messages(
    client: &mut ImapClient,
    uids: &[u32],
) -> Result<Vec<(u32, FullMessage)>> {
    if uids.is_empty() {
        return Ok(Vec::new());
    }
    let uid_set = uids
        .iter()
        .map(|uid| uid.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let mut stream = client
        .session_mut()
        .uid_fetch(&uid_set, "BODY.PEEK[]")
        .await
        .map_err(|e| Error::Imap(format!("UID FETCH reaction bodies {uid_set}: {e}")))?;

    let wanted: HashSet<u32> = uids.iter().copied().collect();
    let mut out = Vec::new();
    let mut fetches = Vec::new();
    // Drain the whole response so nothing is left for the next command, and
    // skip unsolicited FETCHes (flag updates, other UIDs) instead of failing.
    while let Some(item) = stream.next().await {
        let fetch =
            item.map_err(|e| Error::Imap(format!("UID FETCH reaction bodies item: {e}")))?;
        let Some(uid) = fetch.uid.filter(|uid| wanted.contains(uid)) else {
            continue;
        };
        if fetch.bodystructure().is_some() {
            continue;
        }
        let Some(raw) = fetch.body() else {
            continue;
        };
        fetches.push((uid, raw.to_vec()));
    }
    drop(stream);
    for (uid, raw) in fetches {
        out.push((uid, parse_full_message(&raw)?));
    }
    Ok(out)
}

/// Messages at or under this size are fetched whole in one round trip.
/// Larger messages show their text parts first; attachments follow later.
pub const DISPLAY_FULL_MAX_BYTES: u32 = 262_144;

/// First-open FETCH: size, structure, headers, and a capped raw prefix.
///
/// The prefix is the entire RFC822 when `RFC822.SIZE` is within
/// [`DISPLAY_FULL_MAX_BYTES`]. Larger messages use it only as a cap so a
/// multi-megabyte attachment is not on the path that paints the reader.
const DISPLAY_PROBE_QUERY: &str = concat!(
    "(RFC822.SIZE BODYSTRUCTURE BODY.PEEK[HEADER] ",
    "BODY.PEEK[]<0.262144>)"
);

/// The same probe without the raw prefix, for a message already known to be
/// larger than [`DISPLAY_FULL_MAX_BYTES`]: the prefix would be downloaded and
/// then thrown away, because only the text parts are fetched for such mail.
const DISPLAY_PROBE_QUERY_NO_PREFIX: &str = "(RFC822.SIZE BODYSTRUCTURE BODY.PEEK[HEADER])";

fn display_probe_query(known_size: Option<u32>) -> &'static str {
    match known_size {
        Some(size) if size > DISPLAY_FULL_MAX_BYTES => DISPLAY_PROBE_QUERY_NO_PREFIX,
        _ => DISPLAY_PROBE_QUERY,
    }
}

/// Body shown on click, plus whether attachment bytes were left on the server.
#[derive(Clone, Debug)]
pub struct DisplayFetch {
    pub message: FullMessage,
    /// True when file or inline parts were not included in `message`.
    pub deferred_rest: bool,
    /// True when structure promises downloadable files but `message.attachments` is empty.
    pub files_pending: bool,
}

/// Extra MIME leaf fetched by section after the text body is on screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttachmentSection {
    pub path: Vec<u32>,
    pub filename: String,
    pub mime_type: String,
    pub mime_subtype: String,
    pub cid: Option<String>,
    pub encoding: String,
    pub disposition_attachment: bool,
    /// Encoded size from BODYSTRUCTURE (0 when unknown).
    pub octets: u64,
}

/// Fetch enough to paint the reader without downloading every attachment.
///
/// `known_size` is the `RFC822.SIZE` recorded by the list sync, when there is
/// one; a large message then skips the raw prefix it would not use.
pub async fn uid_fetch_display_message(
    client: &mut ImapClient,
    uid: u32,
    known_size: Option<u32>,
) -> Result<DisplayFetch> {
    let fetch = uid_fetch_matching(
        client,
        uid,
        display_probe_query(known_size),
        is_structure_response,
    )
    .await?;
    let size = fetch.size;
    let header = fetch.header().unwrap_or_default().to_vec();
    let structure_has_files = fetch
        .bodystructure()
        .is_some_and(structure_has_file_attachment);
    let plan = fetch.bodystructure().map(plan_display_fetch);
    let prefix_len = fetch.body().map(|raw| raw.len());
    let leaf_octets = fetch.bodystructure().map(structure_encoded_octets);
    let boundary = fetch.bodystructure().and_then(structure_boundary);
    // `BODY.PEEK[]<0.N>` stops at N even when RFC822.SIZE is missing or too
    // small. A full window whose BODYSTRUCTURE parts do not fit is a prefix,
    // not the message — parsing it stores a truncated attachment.
    let prefix_is_whole =
        prefix_len.is_some_and(|len| capped_prefix_is_whole_message(size, len, leaf_octets));
    let small_raw = if prefix_is_whole {
        fetch.body().map(|raw| raw.to_vec())
    } else {
        None
    };
    drop(fetch);

    if let Some(raw) = small_raw
        && let Ok(message) = parse_full_message(&raw)
    {
        // A prefix cut inside a part never holds the `--boundary--`
        // closing delimiter, so its base64 would be saved as the file.
        // Anything unverifiable falls through to the full fetch below:
        // an extra round trip, never a corrupt file.
        if message.incomplete_attachments.is_empty()
            && prefix_parse_is_complete(boundary.as_deref(), &raw, !message.attachments.is_empty())
        {
            return Ok(DisplayFetch {
                message,
                deferred_rest: false,
                files_pending: false,
            });
        }
    }

    if size.is_some_and(|n| n <= DISPLAY_FULL_MAX_BYTES) || plan.is_none() {
        return Ok(DisplayFetch {
            message: uid_fetch_full_message(client, uid).await?,
            deferred_rest: false,
            files_pending: false,
        });
    }

    match plan.unwrap_or(DisplayPlan::Whole) {
        DisplayPlan::Whole => Ok(DisplayFetch {
            message: uid_fetch_full_message(client, uid).await?,
            deferred_rest: false,
            files_pending: false,
        }),
        DisplayPlan::Parts {
            sections,
            has_extra,
            ..
        } => {
            let first = sections.first().map(|section| section.path.as_slice());
            let fetched =
                uid_fetch_matching(client, uid, &section_peek_query(&sections), |fetch| {
                    has_first_section(fetch, first)
                })
                .await?;
            let (text, html) = decode_display_sections(&fetched, &sections);
            if text.is_empty() && html.is_empty() {
                return Ok(DisplayFetch {
                    message: uid_fetch_full_message(client, uid).await?,
                    deferred_rest: false,
                    files_pending: false,
                });
            }
            Ok(DisplayFetch {
                message: FullMessage {
                    raw: Vec::new(),
                    subject: subject_from_header(&header),
                    text,
                    html,
                    list_unsubscribe: parse_list_headers(&header),
                    attachments: Vec::new(),
                    reaction: None,
                    incomplete_attachments: Vec::new(),
                },
                deferred_rest: has_extra,
                files_pending: structure_has_files,
            })
        }
    }
}

/// Structure-only probe so a follow-up download can skip plain messages.
pub async fn uid_fetch_display_plan(client: &mut ImapClient, uid: u32) -> Result<DisplayPlan> {
    let fetch = uid_fetch_matching(client, uid, "BODYSTRUCTURE", is_structure_response).await?;
    Ok(fetch
        .bodystructure()
        .map(plan_display_fetch)
        .unwrap_or(DisplayPlan::Whole))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DisplayPartKind {
    Plain,
    Html,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DisplaySection {
    pub path: Vec<u32>,
    pub kind: DisplayPartKind,
    pub charset: Option<String>,
    pub encoding: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DisplayPlan {
    /// Encrypted, unknown, or no text part — download the RFC822 before display.
    Whole,
    Parts {
        sections: Vec<DisplaySection>,
        has_extra: bool,
        extras: Vec<AttachmentSection>,
    },
}

pub(crate) fn plan_display_fetch(
    structure: &async_imap::imap_proto::BodyStructure<'_>,
) -> DisplayPlan {
    let mut acc = DisplayAccum::default();
    walk_structure(structure, &[], &mut acc);
    if acc.force_whole || acc.sections.is_empty() {
        DisplayPlan::Whole
    } else {
        DisplayPlan::Parts {
            sections: acc.sections,
            has_extra: !acc.extras.is_empty(),
            extras: acc.extras,
        }
    }
}

#[derive(Default)]
struct DisplayAccum {
    sections: Vec<DisplaySection>,
    extras: Vec<AttachmentSection>,
    saw_plain: bool,
    saw_html: bool,
    force_whole: bool,
}

fn walk_structure(
    part: &async_imap::imap_proto::BodyStructure<'_>,
    path: &[u32],
    acc: &mut DisplayAccum,
) {
    use async_imap::imap_proto::BodyStructure;
    if acc.force_whole {
        return;
    }
    match part {
        BodyStructure::Multipart { common, bodies, .. } => {
            if common.ty.subtype.eq_ignore_ascii_case("encrypted") {
                acc.force_whole = true;
                return;
            }
            for (index, body) in bodies.iter().enumerate() {
                let mut child = path.to_vec();
                child.push(index as u32 + 1);
                walk_structure(body, &child, acc);
            }
        }
        BodyStructure::Message { common, other, .. } => {
            if path.is_empty() {
                acc.force_whole = true;
                return;
            }
            // A forwarded or attached message is one attachment, fetched whole
            // as `BODY[path]`. Descending into it would take its text parts as
            // this message's body, or list them as extra attachments, and
            // address them with the outer part's path (an embedded body is
            // `path.1`), so that fetch would return the whole message instead.
            push_attachment_extra(acc, common, other, path);
        }
        BodyStructure::Text { common, other, .. } => {
            consider_leaf(common, other, path, true, acc);
        }
        BodyStructure::Basic { common, other, .. } => {
            consider_leaf(common, other, path, false, acc);
        }
    }
}

fn consider_leaf(
    common: &async_imap::imap_proto::BodyContentCommon<'_>,
    other: &async_imap::imap_proto::BodyContentSinglePart<'_>,
    path: &[u32],
    is_text: bool,
    acc: &mut DisplayAccum,
) {
    if leaf_forces_whole(common) {
        acc.force_whole = true;
        return;
    }
    if is_attachment_part(common) {
        push_attachment_extra(acc, common, other, path);
        return;
    }
    if leaf_is_reaction_json_part(common) {
        return;
    }
    if is_text {
        let section_path = if path.is_empty() {
            vec![1]
        } else {
            path.to_vec()
        };
        let subtype = common.ty.subtype.as_ref();
        if subtype.eq_ignore_ascii_case("html") && !acc.saw_html {
            acc.saw_html = true;
            acc.sections.push(display_section(
                section_path,
                DisplayPartKind::Html,
                common,
                other,
            ));
            return;
        }
        if subtype.eq_ignore_ascii_case("plain") && !acc.saw_plain {
            acc.saw_plain = true;
            acc.sections.push(display_section(
                section_path,
                DisplayPartKind::Plain,
                common,
                other,
            ));
            return;
        }
    }
    push_attachment_extra(acc, common, other, path);
}

fn leaf_forces_whole(common: &async_imap::imap_proto::BodyContentCommon<'_>) -> bool {
    let ty = common.ty.ty.as_ref();
    let sub = common.ty.subtype.as_ref();
    ty.eq_ignore_ascii_case("application")
        && (sub.eq_ignore_ascii_case("pgp-encrypted")
            || sub.eq_ignore_ascii_case("pkcs7-mime")
            || sub.eq_ignore_ascii_case("x-pkcs7-mime"))
}

fn is_attachment_part(common: &async_imap::imap_proto::BodyContentCommon<'_>) -> bool {
    common
        .disposition
        .as_ref()
        .is_some_and(|disposition| disposition.ty.eq_ignore_ascii_case("attachment"))
}

fn leaf_is_signature_or_crypto(common: &async_imap::imap_proto::BodyContentCommon<'_>) -> bool {
    let ty = common.ty.ty.as_ref();
    let sub = common.ty.subtype.as_ref();
    ty.eq_ignore_ascii_case("application")
        && (sub.eq_ignore_ascii_case("pgp-encrypted")
            || sub.eq_ignore_ascii_case("pkcs7-mime")
            || sub.eq_ignore_ascii_case("x-pkcs7-mime")
            || sub.eq_ignore_ascii_case("pgp-signature")
            || sub.eq_ignore_ascii_case("pkcs7-signature"))
}

fn leaf_is_inline_cid_image(
    common: &async_imap::imap_proto::BodyContentCommon<'_>,
    other: &async_imap::imap_proto::BodyContentSinglePart<'_>,
) -> bool {
    common.ty.ty.as_ref().eq_ignore_ascii_case("image")
        && other.id.is_some()
        && common.disposition.is_none()
}

fn leaf_is_reaction_json_part(common: &async_imap::imap_proto::BodyContentCommon<'_>) -> bool {
    common.ty.ty.as_ref().eq_ignore_ascii_case("text")
        && common
            .ty
            .subtype
            .as_ref()
            .eq_ignore_ascii_case("vnd.google.email-reaction+json")
        && !is_attachment_part(common)
}

fn leaf_is_reaction_disposition_part(
    common: &async_imap::imap_proto::BodyContentCommon<'_>,
) -> bool {
    common.ty.ty.as_ref().eq_ignore_ascii_case("text")
        && common.ty.subtype.as_ref().eq_ignore_ascii_case("plain")
        && common
            .disposition
            .as_ref()
            .is_some_and(|disp| disp.ty.eq_ignore_ascii_case("reaction"))
}

fn leaf_is_body_text(common: &async_imap::imap_proto::BodyContentCommon<'_>) -> bool {
    if !common.ty.ty.as_ref().eq_ignore_ascii_case("text") {
        return false;
    }
    let sub = common.ty.subtype.as_ref();
    (sub.eq_ignore_ascii_case("plain") || sub.eq_ignore_ascii_case("html"))
        && !is_attachment_part(common)
        && !leaf_is_reaction_json_part(common)
        && !leaf_is_reaction_disposition_part(common)
}

fn leaf_structure_is_file_attachment(
    common: &async_imap::imap_proto::BodyContentCommon<'_>,
    other: &async_imap::imap_proto::BodyContentSinglePart<'_>,
) -> bool {
    if leaf_is_signature_or_crypto(common) {
        return false;
    }
    if leaf_is_inline_cid_image(common, other) {
        return false;
    }
    if leaf_is_body_text(common)
        || leaf_is_reaction_json_part(common)
        || leaf_is_reaction_disposition_part(common)
    {
        return false;
    }
    if is_attachment_part(common) {
        return true;
    }
    let filename = structure_leaf_filename(common);
    let mime = structure_leaf_mime(common);
    let cid = structure_leaf_cid(other);
    attachment_is_downloadable(
        &filename,
        &mime,
        cid.as_deref(),
        &AttachmentLists::defaults(),
    )
}

fn structure_leaf_filename(common: &async_imap::imap_proto::BodyContentCommon<'_>) -> String {
    let disposition_params = common
        .disposition
        .as_ref()
        .and_then(|disposition| disposition.params.as_deref());
    disposition_params
        .and_then(|params| mime_param_value(params, "filename"))
        .or_else(|| {
            common
                .ty
                .params
                .as_deref()
                .and_then(|params| mime_param_value(params, "name"))
        })
        .unwrap_or_default()
}

/// Decoded MIME parameter value from BODYSTRUCTURE.
///
/// Servers pass parameters through as sent, so a filename may be an
/// RFC 2231 extended value (`filename*=utf-8''r%C3%A9sum%C3%A9.pdf`), split
/// into continuations (`filename*0*=…; filename*1*=…`), or (non-standard
/// but common) an RFC 2047 encoded word. Extended forms win over the plain
/// ASCII fallback many mailers send alongside them.
pub(crate) fn mime_param_value<K: AsRef<str>, V: AsRef<str>>(
    params: &[(K, V)],
    name: &str,
) -> Option<String> {
    let mut plain = None;
    let mut extended = None;
    let mut segments: Vec<(u32, bool, &str)> = Vec::new();
    for (key, value) in params {
        let key = key.as_ref();
        let value = value.as_ref();
        let Some(rest) = key
            .get(..name.len())
            .filter(|head| head.eq_ignore_ascii_case(name))
            .map(|_| &key[name.len()..])
        else {
            continue;
        };
        if rest.is_empty() {
            plain = Some(value);
        } else if rest == "*" {
            extended = Some(value);
        } else if let Some(index) = rest.strip_prefix('*') {
            let (index, encoded) = match index.strip_suffix('*') {
                Some(index) => (index, true),
                None => (index, false),
            };
            if let Ok(index) = index.parse::<u32>() {
                segments.push((index, encoded, value));
            }
        }
    }
    if let Some(value) = extended {
        return Some(decode_rfc2231_segments(&[(0, true, value)]));
    }
    if !segments.is_empty() {
        segments.sort_by_key(|(index, _, _)| *index);
        if segments[0].0 == 0 {
            return Some(decode_rfc2231_segments(&segments));
        }
    }
    plain.map(decode_mime_header)
}

/// Join RFC 2231 continuation segments and decode the charset.
fn decode_rfc2231_segments(segments: &[(u32, bool, &str)]) -> String {
    let mut charset = String::new();
    let mut bytes = Vec::new();
    for (position, (_, encoded, value)) in segments.iter().enumerate() {
        let mut value = *value;
        if position == 0 && *encoded {
            // `charset'language'data`; both prefixes may be empty.
            let mut parts = value.splitn(3, '\'');
            if let (Some(cs), Some(_lang), Some(data)) = (parts.next(), parts.next(), parts.next())
            {
                charset = cs.to_string();
                value = data;
            }
        }
        if *encoded {
            percent_decode_into(value, &mut bytes);
        } else {
            bytes.extend_from_slice(value.as_bytes());
        }
    }
    if charset.is_empty()
        || charset.eq_ignore_ascii_case("utf-8")
        || charset.eq_ignore_ascii_case("us-ascii")
    {
        return String::from_utf8_lossy(&bytes).into_owned();
    }
    mail_parser::decoders::charsets::map::charset_decoder(charset.as_bytes())
        .map(|decode| decode(&bytes))
        .unwrap_or_else(|| String::from_utf8_lossy(&bytes).into_owned())
}

fn percent_decode_into(value: &str, out: &mut Vec<u8>) {
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).ok();
            if let Some(byte) = hex.and_then(|hex| u8::from_str_radix(hex, 16).ok()) {
                out.push(byte);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
}

fn structure_leaf_cid(other: &async_imap::imap_proto::BodyContentSinglePart<'_>) -> Option<String> {
    other
        .id
        .as_ref()
        .map(|id| {
            id.trim()
                .trim_start_matches('<')
                .trim_end_matches('>')
                .to_string()
        })
        .filter(|cid| !cid.is_empty())
}

fn structure_leaf_mime(common: &async_imap::imap_proto::BodyContentCommon<'_>) -> String {
    format!("{}/{}", common.ty.ty.as_ref(), common.ty.subtype.as_ref()).to_ascii_lowercase()
}

fn attachment_section_from_leaf(
    common: &async_imap::imap_proto::BodyContentCommon<'_>,
    other: &async_imap::imap_proto::BodyContentSinglePart<'_>,
    path: &[u32],
) -> AttachmentSection {
    let section_path = if path.is_empty() {
        vec![1]
    } else {
        path.to_vec()
    };
    AttachmentSection {
        path: section_path,
        filename: structure_leaf_filename(common),
        mime_type: common.ty.ty.to_string(),
        mime_subtype: common.ty.subtype.to_string(),
        cid: structure_leaf_cid(other),
        encoding: encoding_name(&other.transfer_encoding),
        disposition_attachment: is_attachment_part(common),
        octets: u64::from(other.octets),
    }
}

fn push_attachment_extra(
    acc: &mut DisplayAccum,
    common: &async_imap::imap_proto::BodyContentCommon<'_>,
    other: &async_imap::imap_proto::BodyContentSinglePart<'_>,
    path: &[u32],
) {
    acc.extras
        .push(attachment_section_from_leaf(common, other, path));
}

/// True when a `BODY.PEEK[]<0.{cap}>` literal is the entire RFC822 message.
///
/// A shorter literal means the server stopped because the message ended.
/// A literal that fills the cap is the limit, not the end of the message.
/// Servers can report `RFC822.SIZE` and part octet counts that describe only
/// that window, and parsing it stores a truncated base64 blob as the file.
pub(crate) fn capped_prefix_is_whole_message(
    rfc822_size: Option<u32>,
    prefix_len: usize,
    leaf_octets: Option<u64>,
) -> bool {
    let Ok(len) = u32::try_from(prefix_len) else {
        return false;
    };
    if len == 0 || len >= DISPLAY_FULL_MAX_BYTES {
        return false;
    }
    if leaf_octets.is_some_and(|octets| octets > u64::from(len)) {
        return false;
    }
    rfc822_size.is_none_or(|size| size <= len)
}

fn structure_encoded_octets(part: &async_imap::imap_proto::BodyStructure<'_>) -> u64 {
    use async_imap::imap_proto::BodyStructure;
    match part {
        BodyStructure::Multipart { bodies, .. } => {
            bodies.iter().map(structure_encoded_octets).sum()
        }
        BodyStructure::Message { other, body, .. } => {
            u64::from(other.octets).max(structure_encoded_octets(body))
        }
        BodyStructure::Text { other, .. } | BodyStructure::Basic { other, .. } => {
            u64::from(other.octets)
        }
    }
}

/// Top-level multipart boundary from the probe BODYSTRUCTURE, if any.
fn structure_boundary(part: &async_imap::imap_proto::BodyStructure<'_>) -> Option<String> {
    use async_imap::imap_proto::BodyStructure;
    match part {
        BodyStructure::Multipart { common, .. } => common
            .ty
            .params
            .as_ref()?
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case("boundary"))
            .map(|(_, value)| value.to_string()),
        BodyStructure::Message { body, .. } => structure_boundary(body),
        BodyStructure::Text { .. } | BodyStructure::Basic { .. } => None,
    }
}

/// True when the prefix bytes hold the whole message.
///
/// Multipart mail ends with `--boundary--`; `-` is outside the base64
/// alphabet so the delimiter cannot hide inside an encoded part. A prefix
/// cut inside a part never contains it. Without a boundary (single-part
/// mail) only accept when nothing file-like was parsed.
pub(crate) fn prefix_parse_is_complete(
    boundary: Option<&str>,
    raw: &[u8],
    has_attachments: bool,
) -> bool {
    match boundary {
        Some(marker) if !marker.is_empty() => raw_contains_closing_delimiter(marker, raw),
        _ => !has_attachments,
    }
}

fn raw_contains_closing_delimiter(boundary: &str, raw: &[u8]) -> bool {
    let mut needle = Vec::with_capacity(boundary.len() + 4);
    needle.extend_from_slice(b"--");
    needle.extend_from_slice(boundary.as_bytes());
    needle.extend_from_slice(b"--");
    // `boundary` is guaranteed non-empty by the caller, so `needle` is
    // never empty and `windows` cannot panic; a short `raw` simply matches
    // nothing and reports incomplete.
    raw.windows(needle.len()).any(|window| window == needle)
}

pub fn structure_has_file_attachment(
    structure: &async_imap::imap_proto::BodyStructure<'_>,
) -> bool {
    structure_walk_has_file(structure)
}

fn structure_walk_has_file(part: &async_imap::imap_proto::BodyStructure<'_>) -> bool {
    use async_imap::imap_proto::BodyStructure;
    match part {
        BodyStructure::Multipart { common, bodies, .. } => {
            if common.ty.subtype.eq_ignore_ascii_case("encrypted") {
                return false;
            }
            bodies.iter().any(structure_walk_has_file)
        }
        BodyStructure::Message {
            common,
            other,
            body,
            ..
        } => {
            if leaf_structure_is_file_attachment(common, other) {
                return true;
            }
            structure_walk_has_file(body)
        }
        BodyStructure::Text { common, other, .. } | BodyStructure::Basic { common, other, .. } => {
            leaf_structure_is_file_attachment(common, other)
        }
    }
}

fn display_section(
    path: Vec<u32>,
    kind: DisplayPartKind,
    common: &async_imap::imap_proto::BodyContentCommon<'_>,
    other: &async_imap::imap_proto::BodyContentSinglePart<'_>,
) -> DisplaySection {
    DisplaySection {
        path,
        kind,
        charset: charset_param(common),
        encoding: encoding_name(&other.transfer_encoding),
    }
}

fn charset_param(common: &async_imap::imap_proto::BodyContentCommon<'_>) -> Option<String> {
    common.ty.params.as_ref().and_then(|params| {
        params.iter().find_map(|(name, value)| {
            name.eq_ignore_ascii_case("charset")
                .then(|| value.to_string())
        })
    })
}

fn encoding_name(encoding: &async_imap::imap_proto::ContentEncoding<'_>) -> String {
    use async_imap::imap_proto::ContentEncoding;
    match encoding {
        ContentEncoding::SevenBit => "7bit".to_string(),
        ContentEncoding::EightBit => "8bit".to_string(),
        ContentEncoding::Binary => "binary".to_string(),
        ContentEncoding::Base64 => "base64".to_string(),
        ContentEncoding::QuotedPrintable => "quoted-printable".to_string(),
        ContentEncoding::Other(value) => value.to_string(),
    }
}

fn section_paths_query(paths: &[&[u32]]) -> String {
    let mut query = String::from("(");
    for (index, path) in paths.iter().enumerate() {
        if index > 0 {
            query.push(' ');
        }
        let spec = path
            .iter()
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join(".");
        query.push_str("BODY.PEEK[");
        query.push_str(&spec);
        query.push(']');
    }
    query.push(')');
    query
}

fn section_peek_query(sections: &[DisplaySection]) -> String {
    let paths: Vec<&[u32]> = sections
        .iter()
        .map(|section| section.path.as_slice())
        .collect();
    section_paths_query(&paths)
}

/// Fetch attachment part bytes by IMAP section after the text body is on screen.
pub async fn uid_fetch_attachment_sections(
    client: &mut ImapClient,
    uid: u32,
    sections: &[AttachmentSection],
) -> Result<Vec<FetchedAttachment>> {
    if sections.is_empty() {
        return Ok(Vec::new());
    }
    let paths: Vec<&[u32]> = sections
        .iter()
        .map(|section| section.path.as_slice())
        .collect();
    let first = sections.first().map(|section| section.path.as_slice());
    let fetched = uid_fetch_matching(client, uid, &section_paths_query(&paths), |fetch| {
        has_first_section(fetch, first)
    })
    .await?;
    let mut attachments = Vec::new();
    let mut saw_usable = false;
    for section in sections {
        let Some(bytes) = fetched.section(&async_imap::imap_proto::SectionPath::Part(
            section.path.clone(),
            None,
        )) else {
            if section_requires_bytes(section) {
                return Err(Error::Imap(format!(
                    "uid {uid} section {:?} missing",
                    section.path
                )));
            }
            continue;
        };
        // Never store a part we could not decode completely: the caller
        // falls back to a full-message fetch, and the user sees an error
        // instead of a corrupt file.
        let decoded = decode_attachment_section_bytes(bytes, section).map_err(|err| {
            Error::Imap(format!(
                "uid {uid} section {:?} ({}): {err}",
                section.path, section.filename
            ))
        })?;
        if decoded.is_empty() {
            if section_requires_bytes(section) {
                return Err(Error::Imap(format!(
                    "uid {uid} section {:?} decoded empty",
                    section.path
                )));
            }
            continue;
        }
        saw_usable = true;
        attachments.push(FetchedAttachment {
            filename: section.filename.clone(),
            cid: section.cid.clone(),
            bytes: decoded,
            content_type: (section.mime_type.clone(), section.mime_subtype.clone()),
            disposition_attachment: section.disposition_attachment,
        });
    }
    if !saw_usable {
        return Err(Error::Imap(format!(
            "uid {uid} missing attachment sections"
        )));
    }
    Ok(attachments)
}

fn section_requires_bytes(section: &AttachmentSection) -> bool {
    section.mime_type.eq_ignore_ascii_case("video")
        || section.mime_type.eq_ignore_ascii_case("audio")
        || section.mime_type.eq_ignore_ascii_case("application")
}

fn encoding_looks_already_decoded(encoding: &str, bytes: &[u8]) -> bool {
    let encoding = encoding.to_ascii_lowercase();
    if encoding != "base64" && encoding != "quoted-printable" {
        return false;
    }
    // Quoted-printable that still carries `=XX` escapes or soft line breaks is
    // encoded even when a sloppy mailer left raw 8-bit bytes in it, and must be
    // decoded. Returning it as-is would keep the escapes in the file.
    if encoding == "quoted-printable" && has_quoted_printable_escape(bytes) {
        return false;
    }
    !bytes
        .iter()
        .take(64)
        .all(|byte| transfer_byte_is_encoded(*byte, &encoding))
}

/// True when `bytes` hold an `=XX` escape or an `=` soft line break.
///
/// An `=` right after another `=` is never an encoder's escape (a literal `=`
/// is written `=3D`), so a decoded `=====` rule line or `a ==` does not count.
fn has_quoted_printable_escape(bytes: &[u8]) -> bool {
    bytes.windows(3).enumerate().any(|(i, w)| {
        w[0] == b'='
            && (i == 0 || bytes[i - 1] != b'=')
            && ((w[1].is_ascii_hexdigit() && w[2].is_ascii_hexdigit())
                || w[1] == b'\r'
                || w[1] == b'\n')
    })
}

fn transfer_byte_is_encoded(byte: u8, encoding: &str) -> bool {
    match encoding {
        "base64" => {
            byte.is_ascii_alphanumeric()
                || byte == b'+'
                || byte == b'/'
                || byte == b'='
                || byte == b'\r'
                || byte == b'\n'
                || byte == b' '
                || byte == b'\t'
        }
        "quoted-printable" => {
            byte.is_ascii_graphic()
                || byte == b' '
                || byte == b'\t'
                || byte == b'\r'
                || byte == b'\n'
                || byte == b'='
        }
        _ => true,
    }
}

/// Decode one fetched section to the exact file bytes.
///
/// Errors instead of returning the raw transfer text: a truncated part must
/// not reach the attachment store.
fn decode_attachment_section_bytes(
    bytes: &[u8],
    section: &AttachmentSection,
) -> std::result::Result<Vec<u8>, PartIntegrityError> {
    check_section_length(section.octets, bytes.len() as u64)?;
    let encoding = sanitize_header_token(&section.encoding);
    if encoding.eq_ignore_ascii_case("7bit")
        || encoding.eq_ignore_ascii_case("8bit")
        || encoding.eq_ignore_ascii_case("binary")
        || encoding_looks_already_decoded(&encoding, bytes)
    {
        return Ok(bytes.to_vec());
    }
    if encoding.eq_ignore_ascii_case("base64") {
        return decode_base64_body(bytes);
    }
    if encoding.eq_ignore_ascii_case("quoted-printable")
        && let Some(decoded) = quoted_printable_decode(bytes)
    {
        return Ok(decoded);
    }
    // Malformed quoted-printable or an unknown encoding: let mail-parser
    // apply its lenient rules (unknown encodings pass through unchanged).
    Ok(decode_attachment_section_bytes_via_reparse(
        bytes, section, &encoding,
    ))
}

fn decode_attachment_section_bytes_via_reparse(
    bytes: &[u8],
    section: &AttachmentSection,
    encoding: &str,
) -> Vec<u8> {
    let ctype = format!(
        "{}/{}",
        section.mime_type.to_ascii_lowercase(),
        section.mime_subtype.to_ascii_lowercase()
    );
    let mut raw = format!("Content-Type: {ctype}");
    if !section.filename.is_empty() {
        raw.push_str("; name=\"");
        raw.push_str(&sanitize_header_token(&section.filename));
        raw.push('"');
    }
    raw.push_str("\r\nContent-Transfer-Encoding: ");
    raw.push_str(encoding);
    raw.push_str("\r\n\r\n");
    let mut buf = raw.into_bytes();
    buf.extend_from_slice(bytes);
    let Some(parsed) = MessageParser::default().parse(&buf) else {
        return Vec::new();
    };
    parsed
        .attachments()
        .next()
        .map(|part| part.contents().to_vec())
        .unwrap_or_default()
}

fn decode_display_sections(
    fetch: &async_imap::types::Fetch,
    sections: &[DisplaySection],
) -> (String, String) {
    use async_imap::imap_proto::SectionPath;
    let mut text = String::new();
    let mut html = String::new();
    for section in sections {
        let Some(bytes) = fetch.section(&SectionPath::Part(section.path.clone(), None)) else {
            continue;
        };
        let decoded = decode_section_bytes(bytes, section);
        match section.kind {
            DisplayPartKind::Plain if text.is_empty() => text = decoded,
            DisplayPartKind::Html if html.is_empty() => html = decoded,
            _ => {}
        }
    }
    (text, html)
}

fn decode_section_bytes(bytes: &[u8], section: &DisplaySection) -> String {
    let ctype = match section.kind {
        DisplayPartKind::Plain => "text/plain",
        DisplayPartKind::Html => "text/html",
    };
    let charset = section
        .charset
        .as_deref()
        .map(sanitize_header_token)
        .unwrap_or_default();
    let encoding = sanitize_header_token(&section.encoding);
    let mut raw = format!("Content-Type: {ctype}");
    if !charset.is_empty() {
        raw.push_str("; charset=\"");
        raw.push_str(&charset);
        raw.push('"');
    }
    raw.push_str("\r\nContent-Transfer-Encoding: ");
    raw.push_str(&encoding);
    raw.push_str("\r\n\r\n");
    let mut buf = raw.into_bytes();
    buf.extend_from_slice(bytes);
    let Some(parsed) = MessageParser::default().parse(&buf) else {
        return String::new();
    };
    match section.kind {
        DisplayPartKind::Html => parsed.body_html(0).map(|cow| cow.into_owned()),
        DisplayPartKind::Plain => parsed.body_text(0).map(|cow| cow.into_owned()),
    }
    .unwrap_or_default()
}

fn sanitize_header_token(value: &str) -> String {
    value
        .chars()
        .filter(|ch| *ch != '"' && *ch != '\\' && *ch != '\r' && *ch != '\n')
        .collect()
}

fn subject_from_header(header: &[u8]) -> String {
    MessageParser::default()
        .parse_headers(header)
        .and_then(|message| message.subject().map(str::to_string))
        .unwrap_or_default()
}

/// `UID FETCH {uid} {query}` returning the response for `uid` that has the
/// shape `wanted` expects, after draining the whole response stream.
///
/// async-imap yields every untagged FETCH until the tagged completion,
/// including unsolicited flag updates and responses left over from an
/// earlier command that timed out. Taking the first item (and dropping the
/// stream) let a stale `BODY[]<0.262144>` probe prefix be parsed as the
/// full message; the truncated base64 attachment was then stored and saved
/// as the "file".
async fn uid_fetch_matching(
    client: &mut ImapClient,
    uid: u32,
    query: &str,
    wanted: impl Fn(&Fetch) -> bool,
) -> Result<Fetch> {
    let mut stream = client
        .session_mut()
        .uid_fetch(uid.to_string(), query)
        .await
        .map_err(|e| Error::Imap(format!("UID FETCH {uid}: {e}")))?;

    let mut responses = Vec::new();
    while let Some(item) = stream.next().await {
        responses.push(item.map_err(|e| Error::Imap(format!("UID FETCH item {uid}: {e}")))?);
    }
    drop(stream);
    let (found, ignored) = select_fetch_response(responses, uid, wanted);
    if ignored > 0 {
        tracing::debug!(
            uid,
            ignored,
            query,
            "UID FETCH: skipped unrelated FETCH responses"
        );
    }
    found.ok_or_else(|| Error::Imap(format!("no matching FETCH response for uid {uid}")))
}

/// The parts of a FETCH response that decide whether it answers our
/// command. A trait so the selection rules are unit-testable without a
/// server (`async_imap::types::Fetch` cannot be built outside the crate).
pub(crate) trait FetchShape {
    fn response_uid(&self) -> Option<u32>;
    fn has_body(&self) -> bool;
    fn has_bodystructure(&self) -> bool;
    fn has_internal_date(&self) -> bool;
    fn has_section(&self, path: &[u32]) -> bool;
}

impl FetchShape for Fetch {
    fn response_uid(&self) -> Option<u32> {
        self.uid
    }
    fn has_body(&self) -> bool {
        self.body().is_some()
    }
    fn has_bodystructure(&self) -> bool {
        self.bodystructure().is_some()
    }
    fn has_internal_date(&self) -> bool {
        self.internal_date().is_some()
    }
    fn has_section(&self, path: &[u32]) -> bool {
        self.section(&async_imap::imap_proto::SectionPath::Part(
            path.to_vec(),
            None,
        ))
        .is_some()
    }
}

/// First response for `uid` that satisfies `wanted`, plus how many were
/// skipped. UID FETCH responses must carry UID (RFC 3501 §6.4.8); a missing
/// one is accepted only because `wanted` also checks the response shape.
pub(crate) fn select_fetch_response<F: FetchShape>(
    responses: Vec<F>,
    uid: u32,
    wanted: impl Fn(&F) -> bool,
) -> (Option<F>, usize) {
    let total = responses.len();
    let found = responses
        .into_iter()
        .find(|fetch| fetch_is_for_uid(fetch, uid) && wanted(fetch));
    let ignored = total - usize::from(found.is_some());
    (found, ignored)
}

fn fetch_is_for_uid<F: FetchShape>(fetch: &F, uid: u32) -> bool {
    fetch.response_uid().is_none_or(|got| got == uid)
}

/// `(BODY.PEEK[] INTERNALDATE)` answer. A display probe answer also has a
/// `BODY[]` (only the first 256 KiB) but carries BODYSTRUCTURE and no
/// INTERNALDATE, so it can never pass as the whole message.
pub(crate) fn is_full_message_response<F: FetchShape>(fetch: &F) -> bool {
    fetch.has_body() && fetch.has_internal_date() && !fetch.has_bodystructure()
}

/// Display probe or structure-only answer.
pub(crate) fn is_structure_response<F: FetchShape>(fetch: &F) -> bool {
    fetch.has_bodystructure()
}

/// Section fetch answer: carries the first requested section.
pub(crate) fn has_first_section<F: FetchShape>(fetch: &F, first: Option<&[u32]>) -> bool {
    first.is_none_or(|path| fetch.has_section(path))
}

const LIST_HEADER_FETCH: &str = "BODY.PEEK[HEADER.FIELDS (LIST-UNSUBSCRIBE LIST-UNSUBSCRIBE-POST)]";

/// Peek `List-Unsubscribe` headers for a cached message missing stored JSON.
pub async fn uid_fetch_list_headers(
    client: &mut ImapClient,
    uid: u32,
) -> Result<Option<ListUnsubscribe>> {
    let mut stream = client
        .session_mut()
        .uid_fetch(uid.to_string(), LIST_HEADER_FETCH)
        .await
        .map_err(|e| Error::Imap(format!("UID FETCH list headers {uid}: {e}")))?;

    let mut header_peek = None;
    while let Some(item) = stream.next().await {
        let fetch = item.map_err(|e| Error::Imap(format!("UID FETCH list headers item: {e}")))?;
        if header_peek.is_none() && fetch_is_for_uid(&fetch, uid) {
            header_peek = Some(fetch.header().unwrap_or_default().to_vec());
        }
    }
    let header_peek = header_peek
        .ok_or_else(|| Error::Imap(format!("no FETCH response for list headers uid {uid}")))?;
    Ok(parse_list_headers(&header_peek))
}

/// `UID RFC822.SIZE` for messages whose stored size is still unknown.
pub async fn uid_fetch_rfc822_sizes(
    client: &mut ImapClient,
    uid_set: &str,
) -> Result<Vec<(u32, u32)>> {
    let mut stream = client
        .session_mut()
        .uid_fetch(uid_set, "(UID RFC822.SIZE)")
        .await
        .map_err(|e| Error::Imap(format!("UID FETCH RFC822.SIZE {uid_set}: {e}")))?;

    let mut out = Vec::new();
    while let Some(item) = stream.next().await {
        let fetch = item.map_err(|e| Error::Imap(format!("UID FETCH RFC822.SIZE item: {e}")))?;
        if let Some(uid) = fetch.uid {
            out.push((uid, fetch.size.unwrap_or(0)));
        }
    }
    Ok(out)
}

/// Reply-To and Sender from ENVELOPE only (no body structure or text peek).
pub async fn uid_fetch_reply_headers(
    client: &mut ImapClient,
    uid_set: &str,
) -> Result<Vec<(u32, Vec<Address>, Vec<Address>)>> {
    let mut stream = client
        .session_mut()
        .uid_fetch(uid_set, "(UID ENVELOPE)")
        .await
        .map_err(|e| Error::Imap(format!("UID FETCH reply headers {uid_set}: {e}")))?;

    let mut out = Vec::new();
    while let Some(item) = stream.next().await {
        let fetch = item.map_err(|e| Error::Imap(format!("UID FETCH reply headers item: {e}")))?;
        let Some(uid) = fetch.uid else {
            continue;
        };
        let Some(imap_env) = fetch.envelope() else {
            continue;
        };
        let reply_to = imap_envelope_addresses(imap_env.reply_to.as_deref());
        let sender = imap_envelope_addresses(imap_env.sender.as_deref());
        out.push((uid, reply_to, sender));
    }
    Ok(out)
}

const ROUTING_HEADER_FETCH: &str = "(UID BODY.PEEK[HEADER.FIELDS (MAIL-REPLY-TO MAIL-FOLLOWUP-TO LIST-POST LIST-ID DELIVERED-TO X-ORIGINAL-TO)])";

/// Reply routing headers for cached rows fetched before they were stored.
pub async fn uid_fetch_routing_headers(
    client: &mut ImapClient,
    uid_set: &str,
) -> Result<Vec<(u32, RoutingHeaders)>> {
    let mut stream = client
        .session_mut()
        .uid_fetch(uid_set, ROUTING_HEADER_FETCH)
        .await
        .map_err(|e| Error::Imap(format!("UID FETCH routing headers {uid_set}: {e}")))?;

    let mut out = Vec::new();
    while let Some(item) = stream.next().await {
        let fetch =
            item.map_err(|e| Error::Imap(format!("UID FETCH routing headers item: {e}")))?;
        if let Some(uid) = fetch.uid {
            out.push((
                uid,
                parse_routing_headers(fetch.header().unwrap_or_default()),
            ));
        }
    }
    Ok(out)
}

/// Mail-Reply-To, Mail-Followup-To, List-Post, List-Id, X-Original-To and
/// Delivered-To from a header peek. Missing headers stay empty.
pub fn parse_routing_headers(header_bytes: &[u8]) -> RoutingHeaders {
    MessageParser::default()
        .parse_headers(header_bytes)
        .map(|msg| routing_from_message(&msg))
        .unwrap_or_default()
}

fn routing_from_message(msg: &mail_parser::Message<'_>) -> RoutingHeaders {
    let mut delivered_to = Vec::new();
    for name in ["X-Original-To", "Delivered-To"] {
        for email in header_addresses(msg, name)
            .into_iter()
            .map(|addr| addr.email)
        {
            if !delivered_to
                .iter()
                .any(|seen: &String| seen.eq_ignore_ascii_case(&email))
            {
                delivered_to.push(email);
            }
        }
    }
    RoutingHeaders {
        mail_reply_to: header_addresses(msg, "Mail-Reply-To"),
        mail_followup_to: header_addresses(msg, "Mail-Followup-To"),
        list_post: list_header_raw(msg, "List-Post")
            .as_deref()
            .and_then(parse_list_post),
        list_id: list_header_raw(msg, "List-Id")
            .as_deref()
            .and_then(parse_list_id),
        delivered_to,
    }
}

/// Every address in every instance of a header mail-parser does not know.
fn header_addresses(msg: &mail_parser::Message<'_>, name: &str) -> Vec<Address> {
    let mut out: Vec<Address> = Vec::new();
    for value in msg.header_as(name, mail_parser::HeaderForm::Addresses) {
        let mail_parser::HeaderValue::Address(list) = value else {
            continue;
        };
        let addrs: Vec<&mail_parser::Addr<'_>> = match &list {
            mail_parser::Address::List(list) => list.iter().collect(),
            mail_parser::Address::Group(groups) => groups
                .iter()
                .flat_map(|group| group.addresses.iter())
                .collect(),
        };
        for addr in addrs {
            let Some(email) = addr.address.as_deref().map(str::trim) else {
                continue;
            };
            if !email.contains('@') {
                continue;
            }
            let name = addr
                .name
                .as_deref()
                .map(alcedo_core::clean_display_name)
                .filter(|name| !name.is_empty());
            out.push(Address {
                name,
                email: email.to_string(),
            });
        }
    }
    out
}

fn imap_envelope_addresses(
    addrs: Option<&[async_imap::imap_proto::types::Address<'_>]>,
) -> Vec<Address> {
    addrs
        .map(|addrs| addrs.iter().filter_map(imap_addr_to_core).collect())
        .unwrap_or_default()
}

/// BODYSTRUCTURE-only UID FETCH for Sent attachment-flag backfill.
pub async fn uid_fetch_attachment_flags(
    client: &mut ImapClient,
    uid_set: &str,
) -> Result<Vec<(u32, bool)>> {
    let mut stream = client
        .session_mut()
        .uid_fetch(uid_set, "(UID BODYSTRUCTURE)")
        .await
        .map_err(|e| Error::Imap(format!("UID FETCH attachment flags {uid_set}: {e}")))?;

    let mut out = Vec::new();
    while let Some(item) = stream.next().await {
        let fetch =
            item.map_err(|e| Error::Imap(format!("UID FETCH attachment flags item: {e}")))?;
        if let Some(uid) = fetch.uid {
            let has = fetch
                .bodystructure()
                .is_some_and(structure_has_file_attachment);
            out.push((uid, has));
        }
    }
    Ok(out)
}

fn parse_fetched_envelope(fetch: &Fetch, folder_id: FolderId) -> Option<FetchedEnvelope> {
    let uid = fetch.uid?;
    let size = fetch.size.unwrap_or(0);
    let modseq = fetch.modseq;

    let imap_env = fetch.envelope()?;
    let subject = imap_env
        .subject
        .as_ref()
        .map(|s| decode_mime_header(&decode_imap_utf8(s)))
        .unwrap_or_default();

    let date = pick_envelope_date(
        imap_env
            .date
            .as_ref()
            .and_then(|d| parse_imap_envelope_date(d)),
        fetch.internal_date().map(|dt| dt.with_timezone(&Utc)),
    );

    let from = imap_envelope_addresses(imap_env.from.as_deref());
    let to = imap_envelope_addresses(imap_env.to.as_deref());
    let cc = imap_envelope_addresses(imap_env.cc.as_deref());
    let reply_to = imap_envelope_addresses(imap_env.reply_to.as_deref());
    let sender = imap_envelope_addresses(imap_env.sender.as_deref());

    let header_peek = fetch.header().unwrap_or_default();
    // One header parse feeds threading, list and routing fields.
    let header_msg = MessageParser::default().parse_headers(header_peek);
    let (message_id, in_reply_to, references) = header_msg
        .as_ref()
        .map(threading_from_message)
        .unwrap_or_default();
    let list_unsubscribe = header_msg
        .as_ref()
        .and_then(parse_list_headers_from_message);
    let routing = header_msg
        .as_ref()
        .map(routing_from_message)
        .unwrap_or_default();

    let text_peek = fetch.text().unwrap_or_default();
    let snippet = snippet_from_peek(text_peek);

    let seen = fetch_is_seen(fetch);
    let flagged = fetch_is_flagged(fetch);
    let answered = fetch
        .flags()
        .any(|f| matches!(f, async_imap::types::Flag::Answered));
    let draft = fetch
        .flags()
        .any(|f| matches!(f, async_imap::types::Flag::Draft));
    let imap_deleted = fetch
        .flags()
        .any(|f| matches!(f, async_imap::types::Flag::Deleted));

    let thread_id = thread_id_from_headers(
        folder_id,
        uid,
        message_id.as_deref(),
        in_reply_to.as_deref(),
        references.as_deref(),
    );

    let reaction_candidate = in_reply_to_is_single_id(in_reply_to.as_deref())
        && fetch
            .bodystructure()
            .is_some_and(structure_has_reaction_candidate);

    Some(FetchedEnvelope {
        uid,
        size,
        subject,
        date,
        from,
        to,
        cc,
        reply_to,
        sender,
        snippet,
        seen,
        flagged,
        answered,
        draft,
        imap_deleted,
        has_attachments: fetch
            .bodystructure()
            .is_some_and(structure_has_file_attachment),
        message_id,
        in_reply_to,
        references,
        thread_id,
        modseq,
        list_unsubscribe,
        reaction_candidate,
        routing,
    })
}

fn in_reply_to_is_single_id(in_reply_to: Option<&str>) -> bool {
    match in_reply_to {
        Some(value) => {
            let value = value.trim();
            !value.is_empty() && !value.contains(char::is_whitespace)
        }
        None => false,
    }
}

fn structure_has_reaction_candidate(structure: &async_imap::imap_proto::BodyStructure<'_>) -> bool {
    structure_walk_has_reaction_candidate(structure)
}

fn structure_walk_has_reaction_candidate(part: &async_imap::imap_proto::BodyStructure<'_>) -> bool {
    use async_imap::imap_proto::BodyStructure;
    match part {
        BodyStructure::Multipart { bodies, .. } => {
            bodies.iter().any(structure_walk_has_reaction_candidate)
        }
        BodyStructure::Message {
            common,
            other,
            body,
            ..
        } => {
            if leaf_is_reaction_json_part(common) || leaf_is_reaction_disposition_part(common) {
                return true;
            }
            if leaf_structure_is_file_attachment(common, other) {
                return false;
            }
            structure_walk_has_reaction_candidate(body)
        }
        BodyStructure::Text { common, .. } | BodyStructure::Basic { common, .. } => {
            leaf_is_reaction_json_part(common) || leaf_is_reaction_disposition_part(common)
        }
    }
}

pub fn parse_full_message(raw: &[u8]) -> Result<FullMessage> {
    let parser = MessageParser::default();
    let parsed = parser
        .parse(raw)
        .ok_or_else(|| Error::Imap("failed to parse RFC822 body".to_string()))?;

    let text = parsed
        .body_text(0)
        .map(|c| c.into_owned())
        .unwrap_or_default();
    let html = parsed
        .body_html(0)
        .map(|c| c.into_owned())
        .unwrap_or_default();

    let list_unsubscribe = parse_list_headers_from_message(&parsed);

    let reaction = parse_email_reaction_from_message(&parsed);

    let mut attachments = Vec::new();
    let mut incomplete_attachments = Vec::new();
    for attachment in parsed.attachments() {
        let (ty, sub) = part_content_type(attachment);
        if ty.eq_ignore_ascii_case("text")
            && sub.eq_ignore_ascii_case("vnd.google.email-reaction+json")
        {
            continue;
        }
        if ty.eq_ignore_ascii_case("text")
            && sub.eq_ignore_ascii_case("plain")
            && attachment
                .content_disposition()
                .is_some_and(|ct| ct.c_type.eq_ignore_ascii_case("reaction"))
        {
            continue;
        }
        let filename = attachment
            .attachment_name()
            .map(str::to_string)
            .unwrap_or_default();
        // mail-parser keeps the raw transfer text when a part is cut off
        // before its boundary or fails to decode. That text is not the file.
        if attachment_part_is_unusable(attachment, &ty, raw.len()) {
            tracing::warn!(
                filename = %filename,
                "attachment part is truncated or not decodable; not storing it"
            );
            incomplete_attachments.push(filename);
            continue;
        }
        let bytes = attachment.contents().to_vec();
        if bytes.is_empty() {
            continue;
        }
        attachments.push(FetchedAttachment {
            filename,
            cid: attachment.content_id().map(|c| c.to_string()),
            bytes,
            content_type: part_content_type(attachment),
            disposition_attachment: alcedo_core::disposition_is_attachment(attachment),
        });
    }

    Ok(FullMessage {
        raw: raw.to_vec(),
        subject: parsed.subject().unwrap_or_default().to_string(),
        text,
        html,
        list_unsubscribe,
        attachments,
        reaction,
        incomplete_attachments,
    })
}

/// True when a parsed attachment part holds raw transfer text, not the file.
///
/// mail-parser sets `is_encoding_problem` when the decoder fails, most often
/// because the part has no closing boundary (the input was cut off), and
/// then returns the undecoded text. For binary types that text is never the
/// file. A text part whose only problem is sloppy encoding (it still ends
/// before the input does) keeps the old lenient behaviour.
fn attachment_part_is_unusable(
    part: &mail_parser::MessagePart<'_>,
    content_type: &str,
    raw_len: usize,
) -> bool {
    if !part.is_encoding_problem {
        return false;
    }
    let runs_to_end = part.offset_end as usize >= raw_len.saturating_sub(2);
    runs_to_end || !content_type.eq_ignore_ascii_case("text")
}

fn part_content_type(part: &mail_parser::MessagePart) -> (String, String) {
    part.content_type()
        .map(|ct| {
            (
                ct.c_type.to_string(),
                ct.c_subtype
                    .as_ref()
                    .map(|sub| sub.to_string())
                    .unwrap_or_else(|| "octet-stream".to_string()),
            )
        })
        .unwrap_or_else(|| ("application".to_string(), "octet-stream".to_string()))
}

fn parse_list_headers(header_bytes: &[u8]) -> Option<ListUnsubscribe> {
    let parser = MessageParser::default();
    let msg = parser.parse_headers(header_bytes)?;
    parse_list_headers_from_message(&msg)
}

fn parse_list_headers_from_message(msg: &mail_parser::Message<'_>) -> Option<ListUnsubscribe> {
    let list = list_header_raw(msg, "List-Unsubscribe");
    let post = list_header_raw(msg, "List-Unsubscribe-Post");
    parse_list_unsubscribe(list.as_deref(), post.as_deref())
}

/// mail-parser treats `List-Unsubscribe` as an address header, so `header()` is
/// `Address` rather than `Text` and drops the RFC 2369 URIs. Read the raw field.
fn list_header_raw(msg: &mail_parser::Message<'_>, name: &str) -> Option<String> {
    let raw = msg.header_raw(name)?.trim();
    if raw.is_empty() {
        None
    } else {
        Some(raw.to_string())
    }
}

fn threading_from_message(
    msg: &mail_parser::Message<'_>,
) -> (Option<String>, Option<String>, Option<String>) {
    let message_id = msg.message_id().map(str::to_string);
    let in_reply_to = header_value_to_string(msg.in_reply_to());
    let references = header_value_to_string(msg.references());
    (message_id, in_reply_to, references)
}

fn header_value_to_string(value: &mail_parser::HeaderValue<'_>) -> Option<String> {
    match value {
        mail_parser::HeaderValue::Text(text) if !text.is_empty() => Some(text.to_string()),
        mail_parser::HeaderValue::TextList(list) if !list.is_empty() => Some(list.join(" ")),
        _ => None,
    }
}

fn snippet_from_peek(text_peek: &[u8]) -> String {
    let parser = MessageParser::default();
    // Prefer a parsed text/plain part when the peek includes MIME headers.
    // HTML-only TEXT peeks have no Content-Type, so use the raw bytes and let
    // preview_scan strip markup from the full peek (not a 140-char cut).
    let from_parser = parser.parse(text_peek).and_then(|msg| {
        msg.body_preview(text_peek.len())
            .map(|preview| preview.into_owned())
    });
    let source = match from_parser {
        Some(preview) if !peek_starts_with_html(&preview) => preview,
        _ => String::from_utf8_lossy(text_peek).into_owned(),
    };
    preview_scan(&source)
}

fn peek_starts_with_html(input: &str) -> bool {
    let trimmed = input.trim_start().trim_start_matches('\u{feff}');
    let lower = trimmed.to_ascii_lowercase();
    lower.starts_with("<!doctype")
        || lower.starts_with("<!--")
        || lower.starts_with("<?xml")
        || lower.starts_with("<html")
        || lower.starts_with("<head")
        || lower.starts_with("<body")
        || lower.starts_with("<div")
        || lower.starts_with("<table")
}

fn imap_addr_to_core(addr: &async_imap::imap_proto::types::Address<'_>) -> Option<Address> {
    let mailbox = addr.mailbox.as_ref()?;
    let host = addr.host.as_ref()?;
    let email = format!(
        "{}@{}",
        String::from_utf8_lossy(mailbox),
        String::from_utf8_lossy(host)
    );
    let name = addr
        .name
        .as_ref()
        .map(|n| alcedo_core::clean_display_name(&String::from_utf8_lossy(n)))
        .filter(|n| !n.is_empty());
    Some(Address { name, email })
}

fn decode_imap_utf8(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// The message's `Date`, else the server receive time. `Utc::now()` is the
/// last resort: it changes on every re-fetch and floats the row to the top.
fn pick_envelope_date(
    header: Option<DateTime<Utc>>,
    internal: Option<DateTime<Utc>>,
) -> DateTime<Utc> {
    header.or(internal).unwrap_or_else(Utc::now)
}

fn parse_imap_envelope_date(bytes: &[u8]) -> Option<DateTime<Utc>> {
    let s = String::from_utf8_lossy(bytes);
    if let Ok(dt) = DateTime::parse_from_rfc2822(s.trim()) {
        return Some(dt.with_timezone(&Utc));
    }
    mail_parser::DateTime::parse_rfc822(s.trim()).map(|dt| {
        let timestamp = dt.to_timestamp();
        Utc.timestamp_opt(timestamp, 0)
            .single()
            .unwrap_or_else(Utc::now)
    })
}

fn thread_id_from_headers(
    folder_id: FolderId,
    uid: u32,
    message_id: Option<&str>,
    in_reply_to: Option<&str>,
    references: Option<&str>,
) -> ThreadId {
    match alcedo_core::thread_root_token(message_id, in_reply_to, references) {
        Some(token) => thread_id_from_root(&token),
        None => orphan_thread_id(folder_id, uid),
    }
}

/// Parse a stored IMAP draft RFC822 message into a [`ComposeDraft`] (headers + body only).
pub fn compose_draft_from_full_message(raw: &[u8], account_id: AccountId) -> Result<ComposeDraft> {
    let parser = MessageParser::default();
    let parsed = parser
        .parse(raw)
        .ok_or_else(|| Error::Imap("failed to parse draft RFC822".to_string()))?;

    let format = compose_format_from_message(&parsed);
    let text = parsed
        .body_text(0)
        .map(|c| c.into_owned())
        .unwrap_or_default();
    let html = parsed
        .body_html(0)
        .map(|c| c.into_owned())
        .unwrap_or_default();
    let body_text = match format {
        ComposeFormat::Html => sanitize_compose_html(&html),
        ComposeFormat::Markdown | ComposeFormat::Plain => text,
    };

    let identity_id = parsed
        .header_raw("X-Alcedo-Identity")
        .and_then(|raw| uuid::Uuid::parse_str(raw.trim()).ok())
        .map(IdentityId);
    let custom_from = if identity_id.is_none() {
        parsed
            .header_raw("X-Alcedo-From")
            .and_then(|raw| alcedo_core::parse_address_token(raw.trim()))
            .map(|addr| addr.formatted())
    } else {
        None
    };

    Ok(ComposeDraft {
        account_id,
        to: recipients_to(&parsed),
        cc: recipients_cc(&parsed),
        bcc: recipients_bcc(&parsed),
        subject: parsed.subject().unwrap_or_default().to_string(),
        body_text,
        format,
        request_mdn: false,
        tracking_pixel_url: None,
        attachments: Vec::new(),
        inlines: Vec::new(),
        collect_outgoing_contacts: true,
        in_reply_to: header_value_to_string(parsed.in_reply_to()),
        references: header_value_to_string(parsed.references()),
        thread_id: None,
        approve_contact_email: None,
        sign: false,
        encrypt: false,
        autocrypt_header: None,
        identity_id,
        custom_from,
        imap_draft: None,
        reaction_emoji: None,
    })
}

fn compose_format_from_message(parsed: &mail_parser::Message<'_>) -> ComposeFormat {
    if let Some(raw) = parsed.header_raw("X-Alcedo-Compose-Format") {
        match raw.trim().to_ascii_lowercase().as_str() {
            "html" => return ComposeFormat::Html,
            "markdown" => return ComposeFormat::Markdown,
            _ => return ComposeFormat::Plain,
        }
    }
    if parsed
        .body_html(0)
        .map(|html| !html.trim().is_empty())
        .unwrap_or(false)
    {
        ComposeFormat::Html
    } else {
        ComposeFormat::Plain
    }
}

fn recipients_to(msg: &mail_parser::Message<'_>) -> Vec<String> {
    msg.all_to().flat_map(flatten_address_list).collect()
}

fn recipients_cc(msg: &mail_parser::Message<'_>) -> Vec<String> {
    msg.all_cc().flat_map(flatten_address_list).collect()
}

fn recipients_bcc(msg: &mail_parser::Message<'_>) -> Vec<String> {
    msg.all_bcc().flat_map(flatten_address_list).collect()
}

fn flatten_address_list(addr: &mail_parser::Address<'_>) -> Vec<String> {
    match addr {
        mail_parser::Address::List(list) => list.iter().filter_map(format_mail_addr).collect(),
        mail_parser::Address::Group(groups) => groups
            .iter()
            .flat_map(|group| group.addresses.iter())
            .filter_map(format_mail_addr)
            .collect(),
    }
}

fn format_mail_addr(addr: &mail_parser::Addr<'_>) -> Option<String> {
    let email = addr.address.as_ref()?.trim();
    if email.is_empty() {
        return None;
    }
    let name = addr
        .name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty());
    match name {
        Some(name) => Some(format!("{name} <{email}>")),
        None => Some(email.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alcedo_core::{preview_snippet, sentinel_thread_id, thread_id_from_root};
    use async_imap::imap_proto::{AttributeValue, Response};
    use uuid::Uuid;

    fn structure_from(bodystructure: &str) -> async_imap::imap_proto::BodyStructure<'static> {
        let line = format!("* 1 FETCH (BODYSTRUCTURE {bodystructure})\r\n");
        let (_, response) =
            async_imap::imap_proto::parser::parse_response(line.as_bytes()).expect("parse");
        match response {
            Response::Fetch(_, mut attrs) => match attrs.remove(0) {
                AttributeValue::BodyStructure(structure) => structure.into_owned(),
                other => panic!("expected BODYSTRUCTURE, got {other:?}"),
            },
            other => panic!("expected FETCH, got {other:?}"),
        }
    }

    /// Outer text plus a forwarded message that is attached as `fwd.eml`.
    const FORWARDED_PLAIN: &str = r#"(("TEXT" "PLAIN" ("CHARSET" "UTF-8") NIL NIL "7BIT" 4 1)("MESSAGE" "RFC822" ("NAME" "fwd.eml") NIL NIL "7BIT" 200 (NIL "inner" NIL NIL NIL NIL NIL NIL NIL NIL) ("TEXT" "PLAIN" ("CHARSET" "UTF-8") NIL NIL "7BIT" 9 1) 5 NIL ("ATTACHMENT" ("FILENAME" "fwd.eml")) NIL) "MIXED")"#;

    #[test]
    fn forwarded_message_is_one_attachment_not_extra_parts() {
        let structure = structure_from(FORWARDED_PLAIN);
        match plan_display_fetch(&structure) {
            DisplayPlan::Parts {
                sections, extras, ..
            } => {
                assert_eq!(
                    sections.iter().map(|s| s.path.clone()).collect::<Vec<_>>(),
                    vec![vec![1]],
                    "only the outer text is the displayed body"
                );
                assert_eq!(
                    extras
                        .iter()
                        .map(|e| (e.path.clone(), e.filename.clone()))
                        .collect::<Vec<_>>(),
                    vec![(vec![2], "fwd.eml".to_string())],
                    "the forwarded message is fetched once, whole, as fwd.eml"
                );
            }
            other => panic!("expected parts, got {other:?}"),
        }
    }

    #[test]
    fn message_with_only_a_forward_does_not_display_its_inner_text() {
        // No outer text: the forwarded message's text must not become the body.
        let structure = structure_from(
            r#"(("MESSAGE" "RFC822" ("NAME" "fwd.eml") NIL NIL "7BIT" 200 (NIL "inner" NIL NIL NIL NIL NIL NIL NIL NIL) ("TEXT" "PLAIN" ("CHARSET" "UTF-8") NIL NIL "7BIT" 9 1) 5 NIL ("ATTACHMENT" ("FILENAME" "fwd.eml")) NIL)("APPLICATION" "PDF" ("NAME" "a.pdf") NIL NIL "BASE64" 100 NIL ("ATTACHMENT" ("FILENAME" "a.pdf")) NIL) "MIXED")"#,
        );
        assert_eq!(plan_display_fetch(&structure), DisplayPlan::Whole);
    }

    #[test]
    fn known_large_messages_skip_the_raw_prefix() {
        assert!(display_probe_query(None).contains("BODY.PEEK[]<0.262144>"));
        assert!(display_probe_query(Some(1_000)).contains("BODY.PEEK[]<0.262144>"));
        assert!(
            display_probe_query(Some(DISPLAY_FULL_MAX_BYTES)).contains("BODY.PEEK[]<0.262144>"),
            "a message that fits the cap still wants the one-round-trip prefix"
        );
        let large = display_probe_query(Some(DISPLAY_FULL_MAX_BYTES + 1));
        assert!(large.contains("BODYSTRUCTURE") && large.contains("BODY.PEEK[HEADER]"));
        assert!(!large.contains("BODY.PEEK[]"));
    }

    fn text_section(octets: u64) -> AttachmentSection {
        AttachmentSection {
            path: vec![2],
            filename: "note.txt".to_string(),
            mime_type: "text".to_string(),
            mime_subtype: "plain".to_string(),
            cid: None,
            encoding: "quoted-printable".to_string(),
            disposition_attachment: true,
            octets,
        }
    }

    #[test]
    fn quoted_printable_with_raw_8bit_bytes_is_still_decoded() {
        // A sloppy mailer left the UTF-8 é raw but still escaped the `=`.
        let encoded = "caf\u{e9} =3D ok".as_bytes();
        let decoded = decode_attachment_section_bytes(encoded, &text_section(0)).unwrap();
        assert_eq!(decoded, "caf\u{e9} = ok".as_bytes());
    }

    #[test]
    fn already_decoded_text_with_a_rule_line_is_left_alone() {
        // Server-decoded text: raw UTF-8 and a "=====" underline, which a QP
        // encoder could never have written. Decoding it again would join the
        // rule line to the next line.
        let decoded_already = "caf\u{e9}\r\nTitle\r\n=====\r\nBody".as_bytes();
        let out = decode_attachment_section_bytes(decoded_already, &text_section(0)).unwrap();
        assert_eq!(out, decoded_already);
    }

    #[test]
    fn already_decoded_quoted_printable_text_is_left_alone() {
        // The server decoded it for us: raw UTF-8, no escapes left.
        let decoded_already = "caf\u{e9} ok".as_bytes();
        let out = decode_attachment_section_bytes(decoded_already, &text_section(0)).unwrap();
        assert_eq!(out, decoded_already);
    }

    #[test]
    fn display_plan_alternative_is_text_only() {
        let structure = structure_from(
            r#"(("TEXT" "PLAIN" ("CHARSET" "UTF-8") NIL NIL "7BIT" 4 1)("TEXT" "HTML" ("CHARSET" "UTF-8") NIL NIL "QUOTED-PRINTABLE" 9 1) "ALTERNATIVE")"#,
        );
        match plan_display_fetch(&structure) {
            DisplayPlan::Parts {
                sections,
                has_extra,
                extras,
            } => {
                assert!(!has_extra);
                assert!(extras.is_empty());
                assert_eq!(sections.len(), 2);
                assert_eq!(sections[0].path, vec![1]);
                assert_eq!(sections[0].kind, DisplayPartKind::Plain);
                assert_eq!(sections[1].path, vec![2]);
                assert_eq!(sections[1].kind, DisplayPartKind::Html);
                assert_eq!(sections[1].encoding, "quoted-printable");
            }
            DisplayPlan::Whole => panic!("alternative text should not force a full download"),
        }
    }

    #[test]
    fn display_plan_pdf_attachment_defers_the_file() {
        let structure = structure_from(
            r#"(("TEXT" "HTML" ("CHARSET" "UTF-8") NIL NIL "7BIT" 5 1)("APPLICATION" "PDF" ("NAME" "notes.pdf") NIL NIL "BASE64" 900000 NIL ("ATTACHMENT" ("FILENAME" "notes.pdf"))) "MIXED")"#,
        );
        match plan_display_fetch(&structure) {
            DisplayPlan::Parts {
                sections,
                has_extra,
                extras,
            } => {
                assert!(has_extra);
                assert_eq!(extras.len(), 1);
                assert_eq!(extras[0].path, vec![2]);
                assert_eq!(sections.len(), 1);
                assert_eq!(sections[0].kind, DisplayPartKind::Html);
            }
            DisplayPlan::Whole => panic!("html plus pdf should still show the html first"),
        }
    }

    #[test]
    fn capped_prefix_rejects_full_window_when_a_part_is_larger() {
        assert!(!capped_prefix_is_whole_message(
            Some(DISPLAY_FULL_MAX_BYTES),
            DISPLAY_FULL_MAX_BYTES as usize,
            Some(3_500_000),
        ));
    }

    #[test]
    fn capped_prefix_rejects_full_window_without_structure() {
        assert!(!capped_prefix_is_whole_message(
            Some(DISPLAY_FULL_MAX_BYTES),
            DISPLAY_FULL_MAX_BYTES as usize,
            None,
        ));
    }

    #[test]
    fn capped_prefix_rejects_full_window_even_when_reported_size_matches() {
        // A server can echo the partial literal length as RFC822.SIZE and
        // report part octets that fit in that window. The image is still cut.
        assert!(!capped_prefix_is_whole_message(
            Some(DISPLAY_FULL_MAX_BYTES),
            DISPLAY_FULL_MAX_BYTES as usize,
            Some(200_000),
        ));
    }

    #[test]
    fn capped_prefix_accepts_short_message() {
        assert!(capped_prefix_is_whole_message(Some(1200), 1200, Some(800)));
    }

    #[test]
    fn capped_prefix_rejects_when_declared_size_exceeds_the_literal() {
        assert!(!capped_prefix_is_whole_message(Some(5000), 1000, Some(800)));
    }

    #[test]
    fn undecoded_png_base64_is_not_the_file() {
        let encoded = b"iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFC";
        assert!(attachment_bytes_are_undecoded_base64("image/png", encoded));
        assert!(!attachment_bytes_are_undecoded_base64(
            "image/png",
            b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR",
        ));
    }

    #[test]
    fn undecoded_magics_cover_common_binary_formats() {
        let cases: &[(&str, &[u8], &[u8])] = &[
            (
                "image/webp",
                b"UklGRhoAAABXRUJQ",
                b"RIFF\x1a\x00\x00\x00WEBP\x00\x00\x00\x00",
            ),
            (
                "image/bmp",
                b"Qk0AAAAAAAAAAAAA",
                b"BM\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00",
            ),
            (
                "image/tiff",
                b"SUkqAAAAAAAAAAAA",
                b"II*\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00",
            ),
            (
                "image/svg+xml",
                b"PHN2ZyB4bWxucz0iaHR0cDovL3d3dy53My5vcmcvMjAwMC9zdmciPg==",
                b"<svg xmlns=\"http://www.w3.org/2000/svg\">",
            ),
            (
                "video/mp4",
                b"AAAAGGZ0eXBtcDQy",
                b"\x00\x00\x00\x18ftypmp42\x00\x00\x00\x00",
            ),
            (
                "video/quicktime",
                b"AAAAGGZ0eXBtcDQy",
                b"\x00\x00\x00\x18ftypmp42\x00\x00\x00\x00",
            ),
            (
                "application/msword",
                b"0M8R4KGxGuEAAAAA",
                b"\xD0\xCF\x11\xE0\xA1\xB1\x1A\xE1\x00\x00\x00\x00\x00\x00\x00\x00",
            ),
        ];
        for (mime, encoded, raw_magic) in cases {
            assert!(
                attachment_bytes_are_undecoded_base64(mime, encoded),
                "expected undecoded base64 for {mime}"
            );
            assert!(
                !attachment_bytes_are_undecoded_base64(mime, raw_magic),
                "raw magic must not look like undecoded base64 for {mime}"
            );
        }
    }

    #[test]
    fn prefix_rejects_truncated_part_without_closing_delimiter() {
        let raw = concat!(
            "From: a@b.c\r\n",
            "Subject: x\r\n",
            "MIME-Version: 1.0\r\n",
            "Content-Type: multipart/mixed; boundary=b\r\n",
            "\r\n",
            "--b\r\n",
            "Content-Type: text/html; charset=us-ascii\r\n",
            "Content-Transfer-Encoding: quoted-printable\r\n",
            "\r\n",
            "<html></html>\r\n",
            "--b\r\n",
            "Content-Disposition: attachment; filename=\"shot.png\"\r\n",
            "Content-Type: image/png; name=\"shot.png\"\r\n",
            "Content-Transfer-Encoding: base64\r\n",
            "\r\n",
            "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFC",
        );
        assert!(!prefix_parse_is_complete(Some("b"), raw.as_bytes(), true));
    }

    #[test]
    fn prefix_accepts_complete_part_with_closing_delimiter() {
        let raw = concat!(
            "From: a@b.c\r\n",
            "Subject: x\r\n",
            "MIME-Version: 1.0\r\n",
            "Content-Type: multipart/mixed; boundary=b\r\n",
            "\r\n",
            "--b\r\n",
            "Content-Type: text/html; charset=us-ascii\r\n",
            "Content-Transfer-Encoding: quoted-printable\r\n",
            "\r\n",
            "<html></html>\r\n",
            "--b\r\n",
            "Content-Disposition: attachment; filename=\"shot.png\"\r\n",
            "Content-Type: image/png; name=\"shot.png\"\r\n",
            "Content-Transfer-Encoding: base64\r\n",
            "\r\n",
            "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFC",
        );
        let with_close = format!("{raw}--b--\r\n");
        assert!(prefix_parse_is_complete(
            Some("b"),
            with_close.as_bytes(),
            true
        ));
        let with_epilogue = format!("{raw}--b--\r\nEpilogue text\r\n");
        assert!(prefix_parse_is_complete(
            Some("b"),
            with_epilogue.as_bytes(),
            true
        ));
    }

    #[test]
    fn prefix_rule_without_boundary_needs_empty_attachments() {
        assert!(!prefix_parse_is_complete(None, b"anything", true));
        assert!(prefix_parse_is_complete(None, b"anything", false));
        let raw = b"anything";
        assert!(!prefix_parse_is_complete(Some(""), raw, true));
    }

    #[test]
    fn prefix_open_delimiter_alone_is_not_complete() {
        let raw = concat!(
            "From: a@b.c\r\n",
            "Subject: x\r\n",
            "MIME-Version: 1.0\r\n",
            "Content-Type: multipart/mixed; boundary=b\r\n",
            "\r\n",
            "--b\r\n",
            "Content-Type: text/html; charset=us-ascii\r\n",
            "\r\n",
            "<html></html>\r\n",
            "--b\r\n",
            "Content-Disposition: attachment; filename=\"shot.png\"\r\n",
            "Content-Type: image/png\r\n",
            "Content-Transfer-Encoding: base64\r\n",
            "\r\n",
            "iVBORw0KGgo",
        );
        assert!(!prefix_parse_is_complete(Some("b"), raw.as_bytes(), true));
    }

    #[test]
    fn truncated_png_part_without_boundary_is_not_an_attachment() {
        let raw = concat!(
            "From: a@b.c\r\n",
            "Subject: x\r\n",
            "MIME-Version: 1.0\r\n",
            "Content-Type: multipart/mixed; boundary=b\r\n",
            "\r\n",
            "--b\r\n",
            "Content-Type: text/html; charset=us-ascii\r\n",
            "Content-Transfer-Encoding: quoted-printable\r\n",
            "\r\n",
            "<html></html>\r\n",
            "--b\r\n",
            "Content-Disposition: attachment; filename=\"shot.png\"\r\n",
            "Content-Type: image/png; name=\"shot.png\"\r\n",
            "Content-Transfer-Encoding: base64\r\n",
            "\r\n",
            "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFC",
        );
        let full = parse_full_message(raw.as_bytes()).expect("parse truncated png");
        // mail-parser hands back the raw base64 text for the cut-off part;
        // it must be reported as incomplete, not returned as the file.
        assert!(full.attachments.is_empty());
        assert_eq!(full.incomplete_attachments, vec!["shot.png"]);
    }

    #[test]
    fn display_plan_encrypted_is_whole_message() {
        let structure = structure_from(
            r#"(("APPLICATION" "PGP-ENCRYPTED" NIL NIL NIL "7BIT" 11)("APPLICATION" "OCTET-STREAM" NIL NIL NIL "7BIT" 40) "ENCRYPTED")"#,
        );
        assert_eq!(plan_display_fetch(&structure), DisplayPlan::Whole);
    }

    #[test]
    fn section_peek_query_is_parenthesized() {
        let query = section_peek_query(&[DisplaySection {
            path: vec![1, 2],
            kind: DisplayPartKind::Html,
            charset: None,
            encoding: "7bit".to_string(),
        }]);
        assert_eq!(query, "(BODY.PEEK[1.2])");
    }

    #[test]
    fn decode_section_bytes_quoted_printable_html() {
        let section = DisplaySection {
            path: vec![1],
            kind: DisplayPartKind::Html,
            charset: Some("utf-8".to_string()),
            encoding: "quoted-printable".to_string(),
        };
        let decoded = decode_section_bytes(b"<p>caf=C3=A9</p>", &section);
        assert_eq!(decoded, "<p>café</p>");
    }

    #[test]
    fn thread_id_is_stable() {
        let a = thread_id_from_root("<abc@example.com>");
        let b = thread_id_from_root("<abc@example.com>");
        assert_eq!(a, b);
    }

    #[test]
    fn references_first_token_wins() {
        let folder = FolderId(1);
        let a = thread_id_from_headers(
            folder,
            1,
            Some("<child@example.com>"),
            Some("<parent@example.com>"),
            Some("<root@example.com> <other@example.com>"),
        );
        let b = thread_id_from_root("<root@example.com>");
        assert_eq!(a, b);
    }

    #[test]
    fn in_reply_to_used_when_references_missing() {
        let folder = FolderId(1);
        let a = thread_id_from_headers(
            folder,
            1,
            Some("<child@example.com>"),
            Some("<parent@example.com>"),
            None,
        );
        let b = thread_id_from_root("<parent@example.com>");
        assert_eq!(a, b);
    }

    #[test]
    fn message_id_used_when_no_thread_headers() {
        let folder = FolderId(1);
        let a = thread_id_from_headers(folder, 1, Some("<solo@example.com>"), None, None);
        let b = thread_id_from_root("<solo@example.com>");
        assert_eq!(a, b);
    }

    #[test]
    fn two_orphans_do_not_share_thread_id() {
        let folder = FolderId(1);
        let a = thread_id_from_headers(folder, 1, None, None, None);
        let b = thread_id_from_headers(folder, 2, None, None, None);
        assert_ne!(a, b);
        assert_ne!(a, sentinel_thread_id());
    }

    #[test]
    fn snippet_truncates_long_text() {
        let long = "a".repeat(200);
        let snippet = preview_snippet(&long);
        assert!(snippet.chars().count() <= 140);
        assert!(snippet.ends_with("..."));
    }

    #[test]
    fn compose_draft_from_full_message_threading_headers() {
        let account_id = AccountId(Uuid::new_v4());
        let raw = concat!(
            "From: Alice <alice@example.com>\r\n",
            "To: Bob <bob@example.com>\r\n",
            "Subject: Re: test\r\n",
            "In-Reply-To: <parent@example.com>\r\n",
            "References: <root@example.com> <parent@example.com>\r\n",
            "Content-Type: text/plain; charset=utf-8\r\n",
            "\r\n",
            "Body text\r\n",
        );
        let draft =
            compose_draft_from_full_message(raw.as_bytes(), account_id).expect("parse draft");
        assert_eq!(draft.in_reply_to.as_deref(), Some("parent@example.com"));
        assert!(draft.references.as_deref().is_some_and(
            |refs| refs.contains("root@example.com") && refs.contains("parent@example.com")
        ));
    }

    #[test]
    fn compose_draft_from_full_message_custom_from_header() {
        let account_id = AccountId(Uuid::new_v4());
        let raw = concat!(
            "From: Alice <alice@example.com>\r\n",
            "To: Bob <bob@example.com>\r\n",
            "Subject: Draft\r\n",
            "X-Alcedo-From: Pat <pat@other.com>\r\n",
            "Content-Type: text/plain; charset=utf-8\r\n",
            "\r\n",
            "Body text\r\n",
        );
        let draft =
            compose_draft_from_full_message(raw.as_bytes(), account_id).expect("parse draft");
        assert!(draft.identity_id.is_none());
        assert_eq!(draft.custom_from.as_deref(), Some("Pat <pat@other.com>"));
    }

    #[test]
    fn compose_draft_from_full_message_identity_wins_over_custom_from() {
        let account_id = AccountId(Uuid::new_v4());
        let identity_id = IdentityId(Uuid::new_v4());
        let raw = format!(
            concat!(
                "From: Alice <alice@example.com>\r\n",
                "To: Bob <bob@example.com>\r\n",
                "Subject: Draft\r\n",
                "X-Alcedo-Identity: {}\r\n",
                "X-Alcedo-From: Pat <pat@other.com>\r\n",
                "Content-Type: text/plain; charset=utf-8\r\n",
                "\r\n",
                "Body text\r\n",
            ),
            identity_id.0
        );
        let draft =
            compose_draft_from_full_message(raw.as_bytes(), account_id).expect("parse draft");
        assert_eq!(draft.identity_id, Some(identity_id));
        assert!(draft.custom_from.is_none());
    }

    #[test]
    fn decode_mime_subject_from_envelope_bytes() {
        let raw = "=?UTF-8?Q?New_announcement:_BACK_TO?= =?UTF-8?Q?_SCHOOL_BASH?=";
        assert_eq!(
            decode_mime_header(raw),
            "New announcement: BACK TO SCHOOL BASH"
        );
    }

    #[test]
    fn parse_routing_headers_reads_reply_list_and_delivery_headers() {
        let raw = b"Mail-Reply-To: Poster <poster@example.com>\r\n\
Mail-Followup-To: dev@lists.example.org, Poster <poster@example.com>\r\n\
List-Post: <mailto:dev@lists.example.org?subject=hi>\r\n\
List-Id: Dev List <Dev.Lists.Example.Org>\r\n\
Delivered-To: carter@codeinfusion.com\r\n\
X-Original-To: vendor-x@codeinfusion.com\r\n\
Delivered-To: Carter@CodeInfusion.com\r\n\r\n";
        let routing = parse_routing_headers(raw);
        assert_eq!(routing.mail_reply_to.len(), 1);
        assert_eq!(routing.mail_reply_to[0].email, "poster@example.com");
        assert_eq!(routing.mail_reply_to[0].name.as_deref(), Some("Poster"));
        let followup: Vec<&str> = routing
            .mail_followup_to
            .iter()
            .map(|addr| addr.email.as_str())
            .collect();
        assert_eq!(
            followup,
            vec!["dev@lists.example.org", "poster@example.com"]
        );
        assert_eq!(routing.list_post.as_deref(), Some("dev@lists.example.org"));
        assert_eq!(routing.list_id.as_deref(), Some("dev.lists.example.org"));
        assert_eq!(
            routing.delivered_to,
            vec!["vendor-x@codeinfusion.com", "carter@codeinfusion.com"]
        );
    }

    #[test]
    fn parse_routing_headers_empty_when_absent() {
        let routing = parse_routing_headers(b"Subject: hi\r\n\r\n");
        assert_eq!(routing, RoutingHeaders::default());
        assert_eq!(parse_routing_headers(b""), RoutingHeaders::default());
        let no_post = parse_routing_headers(b"List-Post: NO (posting not allowed)\r\n\r\n");
        assert_eq!(no_post.list_post, None);
    }

    #[test]
    fn parse_list_headers_from_rfc8058_folded_insider() {
        let raw = concat!(
            "List-Unsubscribe: <https://mail.useinsider.com/user/v1/scu/casetifyhk/email/1054/fe827218-5df5-4f85-919f-71b9fc47b67e>,\r\n",
            " <mailto:unsubscribe@useinsider.com?subject=unsubscribe_from||aHR0cHM6Ly9tYWlsLnVzZWluc2lkZXIuY29tL3VzZXIvdjEvc2N1L2Nhc2V0aWZ5aGsvZW1haWwvMTA1NC9mZTgyNzIxOC01ZGY1LTRmODUtOTE5Zi03MWI5ZmM0N2I2N2U=>\r\n",
            "List-Unsubscribe-Post: List-Unsubscribe=One-Click\r\n",
            "\r\n",
        );
        let parsed = parse_list_headers(raw.as_bytes()).expect("parse");
        assert!(parsed.one_click);
        assert_eq!(
            parsed.https.as_deref(),
            Some(
                "https://mail.useinsider.com/user/v1/scu/casetifyhk/email/1054/fe827218-5df5-4f85-919f-71b9fc47b67e"
            )
        );
        let mailto = parsed.mailto.expect("mailto");
        assert_eq!(mailto.address, "unsubscribe@useinsider.com");
        assert_eq!(
            mailto.subject.as_deref(),
            Some(
                "unsubscribe_from||aHR0cHM6Ly9tYWlsLnVzZWluc2lkZXIuY29tL3VzZXIvdjEvc2N1L2Nhc2V0aWZ5aGsvZW1haWwvMTA1NC9mZTgyNzIxOC01ZGY1LTRmODUtOTE5Zi03MWI5ZmM0N2I2N2U="
            )
        );
    }

    #[test]
    fn envelope_fetch_query_is_parenthesized() {
        assert!(
            ENVELOPE_FETCH_QUERY.starts_with('(') && ENVELOPE_FETCH_QUERY.ends_with(')'),
            "multi-att FETCH queries must be parenthesized for strict servers"
        );
        assert!(ENVELOPE_FETCH_QUERY.contains("BODYSTRUCTURE"));
        assert!(ENVELOPE_FETCH_QUERY.contains("INTERNALDATE"));
        assert!(ENVELOPE_META_QUERY.contains("INTERNALDATE"));
        assert!(ENVELOPE_FETCH_QUERY.contains("LIST-UNSUBSCRIBE"));
        assert!(ENVELOPE_FETCH_QUERY.contains("LIST-UNSUBSCRIBE-POST"));
        assert!(
            ENVELOPE_FETCH_QUERY.contains("BODY.PEEK[TEXT]<0.32768>"),
            "HTML-only mail needs more than 4KiB to skip <style> before visible copy"
        );
    }

    #[test]
    fn server_flags_map_system_flags_and_ignore_keywords() {
        use async_imap::types::Flag;
        let flags = server_flags_from(
            7,
            [
                Flag::Seen,
                Flag::Answered,
                Flag::Deleted,
                Flag::Custom("$Junk".into()),
            ]
            .into_iter(),
        );
        assert_eq!(
            flags,
            ServerFlags {
                uid: 7,
                seen: true,
                flagged: false,
                answered: true,
                draft: false,
                deleted: true,
            }
        );
        assert!(!server_flags_from(8, std::iter::empty()).seen);
    }

    #[test]
    fn envelope_date_prefers_header_then_internaldate() {
        let header = Utc.timestamp_opt(1_700_000_000, 0).unwrap();
        let internal = Utc.timestamp_opt(1_700_000_500, 0).unwrap();
        assert_eq!(pick_envelope_date(Some(header), Some(internal)), header);
        assert_eq!(pick_envelope_date(None, Some(internal)), internal);
        let now = Utc::now();
        let fallback = pick_envelope_date(None, None);
        assert!(fallback >= now && fallback - now < chrono::Duration::seconds(5));
    }

    #[test]
    fn envelope_meta_query_skips_slow_body_items() {
        assert!(
            ENVELOPE_META_QUERY.starts_with('(') && ENVELOPE_META_QUERY.ends_with(')'),
            "multi-att FETCH queries must be parenthesized for strict servers"
        );
        assert!(ENVELOPE_META_QUERY.contains("ENVELOPE"));
        assert!(ENVELOPE_META_QUERY.contains("LIST-UNSUBSCRIBE"));
        assert!(!ENVELOPE_META_QUERY.contains("BODYSTRUCTURE"));
        assert!(!ENVELOPE_META_QUERY.contains("BODY.PEEK[TEXT]"));
        assert!(
            FULL_MESSAGE_WITH_DATE_QUERY.starts_with('(')
                && FULL_MESSAGE_WITH_DATE_QUERY.ends_with(')')
        );
        assert!(FULL_MESSAGE_WITH_DATE_QUERY.contains("BODY.PEEK[]"));
        assert!(FULL_MESSAGE_WITH_DATE_QUERY.contains("INTERNALDATE"));
    }

    #[test]
    fn structure_flags_jpeg_attachment_with_cid() {
        let structure = structure_from(
            r#"(("TEXT" "PLAIN" ("CHARSET" "UTF-8") NIL NIL "7BIT" 4 1)("IMAGE" "JPEG" ("NAME" "D01_sample.jpg") "<photo-d@codeinfusion.com>" NIL "BASE64" 6 NIL ("ATTACHMENT" ("FILENAME" "D01_sample.jpg"))) "MIXED")"#,
        );
        assert!(structure_has_file_attachment(&structure));
    }

    #[test]
    fn structure_flags_mixed_jpeg_attachment() {
        let structure = structure_from(
            r#"((("TEXT" "PLAIN" ("CHARSET" "UTF-8") NIL NIL "7BIT" 4 1)("TEXT" "HTML" ("CHARSET" "UTF-8") NIL NIL "QUOTED-PRINTABLE" 9 1) "ALTERNATIVE")("IMAGE" "JPEG" ("NAME" "A01_sample.jpg") NIL NIL "BASE64" 6 NIL ("ATTACHMENT" ("FILENAME" "A01_sample.jpg")))("IMAGE" "JPEG" ("NAME" "D01_sample.jpg") "<photo-d@codeinfusion.com>" NIL "BASE64" 6 NIL ("ATTACHMENT" ("FILENAME" "D01_sample.jpg"))) "MIXED")"#,
        );
        assert!(structure_has_file_attachment(&structure));
    }

    #[test]
    fn structure_ignores_alternative_only() {
        let structure = structure_from(
            r#"(("TEXT" "PLAIN" ("CHARSET" "UTF-8") NIL NIL "7BIT" 4 1)("TEXT" "HTML" ("CHARSET" "UTF-8") NIL NIL "QUOTED-PRINTABLE" 9 1) "ALTERNATIVE")"#,
        );
        assert!(!structure_has_file_attachment(&structure));
        match plan_display_fetch(&structure) {
            DisplayPlan::Parts {
                has_extra, extras, ..
            } => {
                assert!(!has_extra);
                assert!(extras.is_empty());
            }
            DisplayPlan::Whole => panic!("plain+html alternative should not force whole"),
        }
    }

    #[test]
    fn structure_ignores_inline_jpeg() {
        let structure = structure_from(
            r#"(("TEXT" "HTML" ("CHARSET" "UTF-8") NIL NIL "7BIT" 5 1)("IMAGE" "JPEG" ("NAME" "logo.jpg") "<logo@example.com>" NIL "BASE64" 100 NIL ("INLINE" ("FILENAME" "logo.jpg"))) "RELATED")"#,
        );
        assert!(!structure_has_file_attachment(&structure));
        match plan_display_fetch(&structure) {
            DisplayPlan::Parts {
                has_extra, extras, ..
            } => {
                assert!(has_extra);
                assert_eq!(extras.len(), 1);
            }
            DisplayPlan::Whole => panic!("html plus inline cid image should defer the image"),
        }
    }

    #[test]
    fn structure_ignores_pgp_encrypted() {
        let structure = structure_from(
            r#"(("APPLICATION" "PGP-ENCRYPTED" NIL NIL NIL "7BIT" 11)("APPLICATION" "OCTET-STREAM" NIL NIL NIL "7BIT" 40) "ENCRYPTED")"#,
        );
        assert!(!structure_has_file_attachment(&structure));
    }

    #[test]
    fn structure_ignores_pgp_signature() {
        let structure = structure_from(
            r#"(("TEXT" "PLAIN" ("CHARSET" "UTF-8") NIL NIL "7BIT" 4 1)("APPLICATION" "PGP-SIGNATURE" NIL NIL NIL "7BIT" 200 NIL ("ATTACHMENT" ("FILENAME" "signature.asc"))) "SIGNED")"#,
        );
        assert!(!structure_has_file_attachment(&structure));
    }

    #[test]
    fn structure_flags_calendar_leaf() {
        let structure = structure_from(
            r#"(("TEXT" "CALENDAR" ("CHARSET" "UTF-8") NIL NIL "7BIT" 500 1) "MIXED")"#,
        );
        assert!(structure_has_file_attachment(&structure));
    }

    #[test]
    fn structure_flags_inline_quicktime_video() {
        let structure = structure_from(
            r#"(("TEXT" "PLAIN" ("CHARSET" "UTF-8") NIL NIL "7BIT" 40 1)("VIDEO" "QUICKTIME" ("NAME" "clip.mov") NIL NIL "BASE64" 900000 NIL ("INLINE" ("FILENAME" "clip.mov"))) "MIXED")"#,
        );
        assert!(structure_has_file_attachment(&structure));
        match plan_display_fetch(&structure) {
            DisplayPlan::Parts {
                has_extra, extras, ..
            } => {
                assert!(has_extra);
                assert_eq!(extras.len(), 1);
                assert_eq!(extras[0].mime_subtype.to_ascii_uppercase(), "QUICKTIME");
            }
            DisplayPlan::Whole => panic!("text plus inline video should defer the file"),
        }
    }

    #[test]
    fn structure_flags_quicktime_without_disposition() {
        let structure = structure_from(
            r#"(("TEXT" "PLAIN" ("CHARSET" "UTF-8") NIL NIL "7BIT" 40 1)("VIDEO" "QUICKTIME" ("NAME" "clip.mov") NIL NIL "BASE64" 900000 1) "MIXED")"#,
        );
        assert!(structure_has_file_attachment(&structure));
        match plan_display_fetch(&structure) {
            DisplayPlan::Parts { has_extra, .. } => assert!(has_extra),
            DisplayPlan::Whole => panic!("text plus video without disposition should defer"),
        }
    }

    #[test]
    fn structure_flags_attachment_quicktime_video() {
        let structure = structure_from(
            r#"(("TEXT" "PLAIN" ("CHARSET" "UTF-8") NIL NIL "7BIT" 40 1)("VIDEO" "QUICKTIME" ("NAME" "clip.mov") NIL NIL "BASE64" 900000 NIL ("ATTACHMENT" ("FILENAME" "clip.mov"))) "MIXED")"#,
        );
        assert!(structure_has_file_attachment(&structure));
    }

    #[test]
    fn decode_attachment_section_base64_pdf() {
        let section = AttachmentSection {
            path: vec![2],
            filename: "notes.pdf".to_string(),
            mime_type: "application".to_string(),
            mime_subtype: "pdf".to_string(),
            cid: None,
            encoding: "base64".to_string(),
            disposition_attachment: true,
            octets: 0,
        };
        let decoded = decode_attachment_section_bytes(b"JVBERi0x", &section).unwrap();
        assert_eq!(decoded, b"%PDF-1");
    }

    #[test]
    fn decode_attachment_section_already_decoded_ftyp() {
        let section = AttachmentSection {
            path: vec![2],
            filename: "clip.mov".to_string(),
            mime_type: "video".to_string(),
            mime_subtype: "quicktime".to_string(),
            cid: None,
            encoding: "base64".to_string(),
            disposition_attachment: false,
            octets: 0,
        };
        let raw = [0x00, 0x00, 0x00, 0x18, b'f', b't', b'y', b'p'];
        let decoded = decode_attachment_section_bytes(&raw, &section).unwrap();
        assert_eq!(decoded, raw);
    }

    #[test]
    fn decode_attachment_section_matches_reparse_for_base64_jpeg() {
        let section = AttachmentSection {
            path: vec![2],
            filename: "photo.jpg".to_string(),
            mime_type: "image".to_string(),
            mime_subtype: "jpeg".to_string(),
            cid: None,
            encoding: "base64".to_string(),
            disposition_attachment: true,
            octets: 0,
        };
        let bytes = b"/9j/4AAQSkZJRg==";
        let encoding = sanitize_header_token(&section.encoding);
        let direct = decode_attachment_section_bytes(bytes, &section).unwrap();
        let via_reparse = decode_attachment_section_bytes_via_reparse(bytes, &section, &encoding);
        assert_eq!(direct, via_reparse);
    }

    #[test]
    fn decode_attachment_section_quoted_printable_direct() {
        let section = AttachmentSection {
            path: vec![2],
            filename: "note.txt".to_string(),
            mime_type: "text".to_string(),
            mime_subtype: "plain".to_string(),
            cid: None,
            encoding: "quoted-printable".to_string(),
            disposition_attachment: true,
            octets: 0,
        };
        let decoded = decode_attachment_section_bytes(b"Hello=20World=3D", &section).unwrap();
        assert_eq!(decoded, b"Hello World=");
    }

    fn docx_section(octets: u64) -> AttachmentSection {
        AttachmentSection {
            path: vec![3],
            filename: "job.docx".to_string(),
            mime_type: "application".to_string(),
            mime_subtype: "vnd.openxmlformats-officedocument.wordprocessingml.document".to_string(),
            cid: None,
            encoding: "base64".to_string(),
            disposition_attachment: true,
            octets,
        }
    }

    #[test]
    fn decode_attachment_section_rejects_dangling_base64() {
        // Old behaviour: fell back to mail-parser, which returned the raw
        // base64 text as the "file".
        let err = decode_attachment_section_bytes(b"UEsDBBQACAgIA", &docx_section(0))
            .expect_err("dangling sextet is truncation");
        assert_eq!(err, PartIntegrityError::TruncatedBase64);
    }

    #[test]
    fn decode_attachment_section_rejects_section_shorter_than_bodystructure() {
        let encoded = base64_76(&synthetic_docx(4_000));
        let cut = &encoded.as_bytes()[..encoded.len() / 2];
        let err = decode_attachment_section_bytes(cut, &docx_section(encoded.len() as u64))
            .expect_err("half a section is not the file");
        assert!(matches!(err, PartIntegrityError::ShortSection { .. }));
    }

    #[test]
    fn decode_attachment_section_returns_exact_bytes_for_complete_docx() {
        let payload = synthetic_docx(10_001);
        let encoded = base64_76(&payload);
        let decoded = decode_attachment_section_bytes(
            encoded.as_bytes(),
            &docx_section(encoded.len() as u64),
        )
        .unwrap();
        assert_eq!(decoded, payload);
    }

    #[test]
    fn decode_attachment_section_passes_binary_and_8bit_through() {
        let payload = synthetic_docx(300);
        for encoding in ["binary", "8bit", "7bit"] {
            let mut section = docx_section(payload.len() as u64);
            section.encoding = encoding.to_string();
            assert_eq!(
                decode_attachment_section_bytes(&payload, &section).unwrap(),
                payload
            );
        }
    }

    /// Deterministic bytes that start like a DOCX (ZIP local file header).
    fn synthetic_docx(len: usize) -> Vec<u8> {
        let mut bytes = b"PK\x03\x04\x14\x00\x08\x08\x08\x00".to_vec();
        let mut state: u32 = 0x1234_5678;
        while bytes.len() < len {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            bytes.push((state >> 24) as u8);
        }
        bytes.truncate(len);
        bytes
    }

    /// Base64 wrapped at 76 columns with CRLF, as mailers send it.
    fn base64_76(bytes: &[u8]) -> String {
        use base64::Engine as _;
        let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
        encoded
            .as_bytes()
            .chunks(76)
            .map(|line| std::str::from_utf8(line).unwrap())
            .collect::<Vec<_>>()
            .join("\r\n")
    }

    const REGRESSION_BOUNDARY: &str = "regression-boundary";

    /// Two DOCX attachments: a small one that fits in the 256 KiB display
    /// window, and one the size of the file Carter received (423,417 bytes)
    /// that does not.
    fn two_docx_message() -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        let small = synthetic_docx(14_000);
        let large = synthetic_docx(423_417);
        let mut raw = String::new();
        raw.push_str("From: hr@example.com\r\nTo: carter@example.com\r\n");
        raw.push_str("Subject: Job description\r\nMIME-Version: 1.0\r\n");
        raw.push_str(&format!(
            "Content-Type: multipart/mixed; boundary=\"{REGRESSION_BOUNDARY}\"\r\n\r\n"
        ));
        raw.push_str(&format!(
            "--{REGRESSION_BOUNDARY}\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nSee attached.\r\n"
        ));
        for (name, bytes) in [("small.docx", &small), ("OPTIVATIONS_job.docx", &large)] {
            raw.push_str(&format!(
                concat!(
                    "--{b}\r\n",
                    "Content-Type: application/vnd.openxmlformats-officedocument.wordprocessingml.document; name=\"{n}\"\r\n",
                    "Content-Disposition: attachment; filename=\"{n}\"\r\n",
                    "Content-Transfer-Encoding: base64\r\n\r\n",
                    "{body}\r\n"
                ),
                b = REGRESSION_BOUNDARY,
                n = name,
                body = base64_76(bytes)
            ));
        }
        raw.push_str(&format!("--{REGRESSION_BOUNDARY}--\r\n"));
        (raw.into_bytes(), small, large)
    }

    #[test]
    fn regression_full_message_yields_exact_docx_bytes() {
        let (raw, small, large) = two_docx_message();
        let message = parse_full_message(&raw).unwrap();
        assert!(message.incomplete_attachments.is_empty());
        assert_eq!(message.attachments.len(), 2);
        assert_eq!(message.attachments[0].bytes, small);
        assert_eq!(message.attachments[1].filename, "OPTIVATIONS_job.docx");
        assert_eq!(message.attachments[1].bytes.len(), 423_417);
        assert_eq!(message.attachments[1].bytes, large);
    }

    /// Regression: the 256 KiB display probe prefix (or a stale probe
    /// response taken for the full fetch) was parsed as the message, and
    /// the cut-off DOCX part was stored as its raw base64 text (a file
    /// starting `UEsDBBQACAgI`). It must never come out as an attachment.
    #[test]
    fn regression_truncated_prefix_never_yields_base64_text_as_file() {
        let (raw, small, _) = two_docx_message();
        assert!(raw.len() > DISPLAY_FULL_MAX_BYTES as usize);
        let prefix = &raw[..DISPLAY_FULL_MAX_BYTES as usize];
        let message = parse_full_message(prefix).unwrap();
        assert_eq!(message.incomplete_attachments, vec!["OPTIVATIONS_job.docx"]);
        assert_eq!(message.attachments.len(), 1, "only the complete part");
        assert_eq!(message.attachments[0].bytes, small);
        for attachment in &message.attachments {
            assert!(!attachment.bytes.starts_with(b"UEsDB"));
            assert!(!attachment_bytes_are_undecoded_base64(
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
                &attachment.bytes
            ));
        }
        // The display path also refuses the prefix as "whole message".
        assert!(!prefix_parse_is_complete(
            Some(REGRESSION_BOUNDARY),
            prefix,
            !message.attachments.is_empty()
        ));
    }

    #[test]
    fn truncated_single_text_attachment_is_flagged() {
        let raw = concat!(
            "Content-Type: multipart/mixed; boundary=\"b\"\r\n\r\n",
            "--b\r\nContent-Type: text/plain\r\n\r\nhello\r\n",
            "--b\r\nContent-Type: text/csv; name=\"data.csv\"\r\n",
            "Content-Disposition: attachment; filename=\"data.csv\"\r\n",
            "Content-Transfer-Encoding: base64\r\n\r\n",
            "YSxiLGMKMSwyLDMK\r\nYSxi"
        );
        let message = parse_full_message(raw.as_bytes()).unwrap();
        assert!(message.attachments.iter().all(|a| a.filename != "data.csv"));
        assert_eq!(message.incomplete_attachments, vec!["data.csv"]);
    }

    #[test]
    fn nested_rfc822_and_quoted_printable_parts_decode() {
        let raw = concat!(
            "Content-Type: multipart/mixed; boundary=\"outer\"\r\n\r\n",
            "--outer\r\nContent-Type: text/plain\r\n\r\nbody\r\n",
            "--outer\r\nContent-Type: multipart/mixed; boundary=\"inner\"\r\n\r\n",
            "--inner\r\nContent-Type: text/plain; name=\"note.txt\"\r\n",
            "Content-Disposition: attachment; filename=\"note.txt\"\r\n",
            "Content-Transfer-Encoding: quoted-printable\r\n\r\n",
            "caf=C3=A9 =3D ok\r\n",
            "--inner--\r\n",
            "--outer\r\nContent-Type: message/rfc822; name=\"fwd.eml\"\r\n",
            "Content-Disposition: attachment; filename=\"fwd.eml\"\r\n\r\n",
            "Subject: inner\r\nContent-Type: text/plain\r\n\r\nforwarded\r\n",
            "--outer--\r\n"
        );
        let message = parse_full_message(raw.as_bytes()).unwrap();
        assert!(message.incomplete_attachments.is_empty());
        let note = message
            .attachments
            .iter()
            .find(|a| a.filename == "note.txt")
            .expect("nested QP attachment");
        assert_eq!(note.bytes, "café = ok".as_bytes());
        assert!(message.attachments.iter().any(|a| a.filename == "fwd.eml"));
    }

    #[test]
    fn rfc2231_and_rfc2047_filenames_decode() {
        let ext = [("filename*", "utf-8''r%C3%A9sum%C3%A9.pdf")];
        assert_eq!(
            mime_param_value(&ext, "filename").as_deref(),
            Some("résumé.pdf")
        );
        let cont = [
            ("filename*1*", "%C3%A9.pdf"),
            ("filename*0*", "UTF-8'en'r%C3%A9sum"),
            ("filename", "resume.pdf"),
        ];
        assert_eq!(
            mime_param_value(&cont, "filename").as_deref(),
            Some("résumé.pdf")
        );
        let plain_cont = [("NAME*0", "long-"), ("NAME*1", "name.docx")];
        assert_eq!(
            mime_param_value(&plain_cont, "name").as_deref(),
            Some("long-name.docx")
        );
        let latin1 = [("filename*", "iso-8859-1''caf%E9.txt")];
        assert_eq!(
            mime_param_value(&latin1, "filename").as_deref(),
            Some("café.txt")
        );
        let encoded_word = [("name", "=?UTF-8?B?w6l0w6kuZG9jeA==?=")];
        assert_eq!(
            mime_param_value(&encoded_word, "name").as_deref(),
            Some("été.docx")
        );
        let plain = [("filename", "report.pdf")];
        assert_eq!(
            mime_param_value(&plain, "filename").as_deref(),
            Some("report.pdf")
        );
        assert_eq!(mime_param_value(&plain, "name"), None);
    }

    #[test]
    fn structure_filename_uses_rfc2231_disposition_param() {
        let structure = structure_from(
            r#"(("TEXT" "PLAIN" ("CHARSET" "UTF-8") NIL NIL "7BIT" 4 1)("APPLICATION" "PDF" NIL NIL NIL "BASE64" 900 NIL ("ATTACHMENT" ("FILENAME*" "utf-8''r%C3%A9sum%C3%A9.pdf"))) "MIXED")"#,
        );
        let async_imap::imap_proto::BodyStructure::Multipart { bodies, .. } = &structure else {
            panic!("multipart");
        };
        let async_imap::imap_proto::BodyStructure::Basic { common, other, .. } = &bodies[1] else {
            panic!("basic leaf");
        };
        let section = attachment_section_from_leaf(common, other, &[2]);
        assert_eq!(section.filename, "résumé.pdf");
        assert_eq!(section.octets, 900);
    }

    #[derive(Debug, PartialEq)]
    struct FakeFetch {
        uid: Option<u32>,
        body: bool,
        structure: bool,
        date: bool,
        sections: Vec<Vec<u32>>,
    }

    impl FetchShape for FakeFetch {
        fn response_uid(&self) -> Option<u32> {
            self.uid
        }
        fn has_body(&self) -> bool {
            self.body
        }
        fn has_bodystructure(&self) -> bool {
            self.structure
        }
        fn has_internal_date(&self) -> bool {
            self.date
        }
        fn has_section(&self, path: &[u32]) -> bool {
            self.sections.iter().any(|p| p == path)
        }
    }

    fn fake(uid: Option<u32>, body: bool, structure: bool, date: bool) -> FakeFetch {
        FakeFetch {
            uid,
            body,
            structure,
            date,
            sections: Vec::new(),
        }
    }

    /// Regression: a leftover probe response (`BODY[]<0>` + BODYSTRUCTURE)
    /// for the same UID arrives ahead of the real full-message answer.
    #[test]
    fn full_fetch_skips_stale_probe_response() {
        let responses = vec![
            fake(Some(911), true, true, false),
            fake(Some(911), true, false, true),
        ];
        let (found, ignored) = select_fetch_response(responses, 911, is_full_message_response);
        assert_eq!(found, Some(fake(Some(911), true, false, true)));
        assert_eq!(ignored, 1);
    }

    #[test]
    fn full_fetch_never_accepts_probe_prefix_alone() {
        let responses = vec![fake(Some(911), true, true, false)];
        let (found, _) = select_fetch_response(responses, 911, is_full_message_response);
        assert_eq!(found, None);
    }

    #[test]
    fn fetch_selection_skips_other_uids_and_flag_updates() {
        let responses = vec![
            fake(Some(7), true, false, true),
            fake(Some(911), false, false, false),
            fake(Some(911), true, false, true),
        ];
        let (found, ignored) = select_fetch_response(responses, 911, is_full_message_response);
        assert_eq!(found.and_then(|f| f.uid), Some(911));
        assert_eq!(ignored, 2);
    }

    #[test]
    fn section_fetch_requires_requested_section() {
        let mut stale = fake(Some(5), true, true, false);
        stale.sections = vec![vec![1]];
        let mut wanted = fake(Some(5), false, false, false);
        wanted.sections = vec![vec![3]];
        let path = vec![3u32];
        let (found, _) = select_fetch_response(vec![stale, wanted], 5, |f| {
            has_first_section(f, Some(path.as_slice()))
        });
        assert_eq!(found.map(|f| f.sections), Some(vec![vec![3]]));
    }

    fn parse_quicktime_fixture(disposition: &str) -> FullMessage {
        let raw = format!(
            concat!(
                "From: sender@example.com\r\n",
                "To: recipient@example.com\r\n",
                "Subject: footage\r\n",
                "MIME-Version: 1.0\r\n",
                "Content-Type: multipart/mixed; boundary=bnd\r\n",
                "\r\n",
                "--bnd\r\n",
                "Content-Type: text/plain; charset=utf-8\r\n",
                "\r\n",
                "Attached is the security camera footage.\r\n",
                "--bnd\r\n",
                "Content-Type: video/quicktime; name=\"clip.mov\"\r\n",
                "{disposition}\r\n",
                "Content-Transfer-Encoding: base64\r\n",
                "\r\n",
                "AAAAEGZ0eXA=\r\n",
                "--bnd--\r\n",
            ),
            disposition = disposition
        );
        parse_full_message(raw.as_bytes()).expect("parse quicktime fixture")
    }

    #[test]
    fn parse_full_message_quicktime_attachment_disposition() {
        let full =
            parse_quicktime_fixture("Content-Disposition: attachment; filename=\"clip.mov\"");
        assert_eq!(full.attachments.len(), 1);
        assert_eq!(full.attachments[0].filename, "clip.mov");
        assert!(!full.attachments[0].bytes.is_empty());
        assert!(full.attachments[0].disposition_attachment);
    }

    #[test]
    fn parse_full_message_quicktime_inline_disposition() {
        let full = parse_quicktime_fixture("Content-Disposition: inline; filename=\"clip.mov\"");
        assert_eq!(full.attachments.len(), 1);
        assert_eq!(full.attachments[0].filename, "clip.mov");
        assert!(!full.attachments[0].bytes.is_empty());
        assert!(!full.attachments[0].disposition_attachment);
    }

    #[test]
    fn parse_full_message_quicktime_without_disposition() {
        let full = parse_quicktime_fixture("");
        assert_eq!(full.attachments.len(), 1);
        assert_eq!(full.attachments[0].filename, "clip.mov");
        assert!(!full.attachments[0].bytes.is_empty());
        assert!(!full.attachments[0].disposition_attachment);
    }

    #[test]
    fn parse_multipart_mixed_jpegs() {
        let raw = include_str!("../tests/fixtures/multipart-mixed-jpegs.eml");
        let full = parse_full_message(raw.as_bytes()).expect("parse");
        assert_eq!(full.attachments.len(), 2);
        assert_eq!(full.attachments[0].filename, "A01_sample.jpg");
        assert_eq!(full.attachments[1].filename, "D01_sample.jpg");
        assert_eq!(
            full.attachments[0].content_type,
            ("image".to_string(), "jpeg".to_string())
        );
        assert_eq!(
            full.attachments[1].content_type,
            ("image".to_string(), "jpeg".to_string())
        );
        assert!(full.attachments[0].disposition_attachment);
        assert!(full.attachments[1].disposition_attachment);
        assert_eq!(
            full.attachments[1].cid.as_deref(),
            Some("photo-d@codeinfusion.com")
        );
    }

    #[test]
    fn snippet_collapses_crlf_from_mime_text_part() {
        let peek = concat!(
            "Content-Type: text/plain; charset=utf-8\r\n",
            "\r\n",
            "\r\nHello Guys,\r\n",
            "\r\n",
            "Nice talking to you this afternoon"
        );
        let snippet = snippet_from_peek(peek.as_bytes());
        assert!(!snippet.contains('\r'), "{snippet:?}");
        assert!(!snippet.contains('\n'), "{snippet:?}");
        assert_eq!(snippet, "Hello Guys, Nice talking to you this afternoon");
    }

    #[test]
    fn snippet_collapses_crlf_from_raw_body() {
        let snippet = snippet_from_peek(b"\r\nHello Guys,\r\n\r\nNice talking");
        assert_eq!(snippet, "Hello Guys, Nice talking");
    }

    #[test]
    fn snippet_strips_html_only_peek() {
        let peek = concat!(
            r#"<!--=E2=84=A2--><!DOCTYPE html PUBLIC "-//W3C//DTD HTML 4.01 Transitional//EN">"#,
            "<html><head><style>body{margin:0}</style></head>",
            "<body><p>August Artist Drop is here</p></body></html>",
        );
        let snippet = snippet_from_peek(peek.as_bytes());
        assert_eq!(snippet, "August Artist Drop is here");
        assert!(!snippet.contains('<'), "{snippet}");
    }

    #[test]
    fn parse_full_message_decodes_declared_multibyte_charset() {
        // mail-parser only applies encoding_rs (Big5, Shift-JIS, GBK, …) when
        // built with `full_encoding`. Without it, high bytes become U+FFFD.
        let mut raw = concat!(
            "From: sender@example.com\r\n",
            "To: recipient@example.com\r\n",
            "Subject: =?big5?B?UmVxdWVzdCChWCBiYW5r?=\r\n",
            "MIME-Version: 1.0\r\n",
            "Content-Type: text/plain; charset=big5\r\n",
            "\r\n",
            "Sep 2025",
        )
        .as_bytes()
        .to_vec();
        raw.extend_from_slice(&[0xa1, b'V']);
        raw.extend_from_slice(b"Feb 2026 ");
        raw.extend_from_slice(&[0xa1, b'X']);
        raw.extend_from_slice(b" notes\r\n");

        let full = parse_full_message(&raw).expect("parse");
        assert_eq!(full.subject, "Request — bank");
        assert!(
            full.text.contains("Sep 2025–Feb 2026 — notes"),
            "expected declared-charset dashes, got {:?}",
            full.text
        );
        assert!(
            !full.text.contains('\u{FFFD}'),
            "replacement char leaked: {:?}",
            full.text
        );
    }

    #[test]
    fn parse_full_message_extracts_named_attachment() {
        let raw = concat!(
            "From: sender@example.com\r\n",
            "To: recipient@example.com\r\n",
            "Subject: attachment test\r\n",
            "MIME-Version: 1.0\r\n",
            "Content-Type: multipart/mixed; boundary=bnd\r\n",
            "\r\n",
            "--bnd\r\n",
            "Content-Type: text/plain; charset=utf-8\r\n",
            "\r\n",
            "Hello\r\n",
            "--bnd\r\n",
            "Content-Type: application/pdf; name=report.pdf\r\n",
            "Content-Disposition: attachment; filename=report.pdf\r\n",
            "Content-Transfer-Encoding: base64\r\n",
            "\r\n",
            "JVBERi0xLjQ=\r\n",
            "--bnd--\r\n",
        );
        let full = parse_full_message(raw.as_bytes()).expect("parse");
        assert_eq!(full.attachments.len(), 1);
        assert_eq!(full.attachments[0].filename, "report.pdf");
        assert!(!full.attachments[0].bytes.is_empty());
    }

    #[test]
    fn parse_full_message_extracts_gmail_sql_attachment() {
        let raw = concat!(
            "From: sender@example.com\r\n",
            "To: recipient@example.com\r\n",
            "Subject: script to update fields to match live\r\n",
            "MIME-Version: 1.0\r\n",
            "Content-Type: multipart/mixed; boundary=\"0000000000006e42d2065b86aa1f\"\r\n",
            "\r\n",
            "--0000000000006e42d2065b86aa1f\r\n",
            "Content-Type: text/plain; charset=UTF-8\r\n",
            "\r\n",
            "see attached\r\n",
            "--0000000000006e42d2065b86aa1f\r\n",
            "Content-Type: application/sql; name=\"live fields 2026-09-15.sql\"\r\n",
            "Content-Disposition: attachment; filename=\"live fields 2026-09-15.sql\"\r\n",
            "Content-Transfer-Encoding: base64\r\n",
            "Content-ID: <f_mu2rugzc0>\r\n",
            "\r\n",
            "VVNFIFtHaXNCZW5lZml0c1NpdGVdDQpHTw0K\r\n",
            "--0000000000006e42d2065b86aa1f--\r\n",
        );
        let full = parse_full_message(raw.as_bytes()).expect("parse");
        assert_eq!(full.attachments.len(), 1);
        assert_eq!(full.attachments[0].filename, "live fields 2026-09-15.sql");
        assert_eq!(full.attachments[0].cid.as_deref(), Some("f_mu2rugzc0"));
        assert!(!full.attachments[0].bytes.is_empty());
    }

    #[test]
    fn parse_full_message_gmail_reaction_sets_reaction_and_skips_attachment() {
        let raw = "From: Sender <sender@example.com>\r\n\
To: Reader <reader@example.com>\r\n\
Subject: Hello\r\n\
In-Reply-To: <parent@example.com>\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/alternative; boundary=\"b\"\r\n\
\r\n\
--b\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
👍 Sender reacted via Gmail\r\n\
--b\r\n\
Content-Type: text/vnd.google.email-reaction+json; charset=utf-8\r\n\
\r\n\
{\"version\":1,\"emoji\":\"👍\"}\r\n\
--b\r\n\
Content-Type: text/html; charset=utf-8\r\n\
\r\n\
<p>👍 Sender reacted via Gmail</p>\r\n\
--b--\r\n"
            .to_string();
        let full = parse_full_message(raw.as_bytes()).expect("parse");
        let reaction = full.reaction.expect("reaction");
        assert_eq!(reaction.emoji, "👍");
        assert_eq!(reaction.target_message_id, "parent@example.com");
        assert!(
            full.attachments
                .iter()
                .all(|att| att.content_type.1 != "vnd.google.email-reaction+json")
        );
    }

    #[test]
    fn parse_full_message_plain_leaves_reaction_none() {
        let raw = "From: Sender <sender@example.com>\r\n\
To: Reader <reader@example.com>\r\n\
Subject: Hello\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
Hello there\r\n";
        let full = parse_full_message(raw.as_bytes()).expect("parse");
        assert!(full.reaction.is_none());
    }

    #[test]
    fn snippet_keeps_otp_past_list_preview() {
        let branding = "Microsoft account privacy and security notice. ".repeat(8);
        assert!(branding.chars().count() > 140);
        let peek = format!(
            "<html><body><p>{branding}</p><p>Your verification code is 482193</p></body></html>"
        );
        let snippet = snippet_from_peek(peek.as_bytes());
        assert!(
            snippet.contains("482193"),
            "stored scan must keep the code for notify extract, got {snippet:?}"
        );
        assert!(
            snippet.chars().count() > 140,
            "scan is longer than the list preview"
        );
    }

    #[test]
    fn imap_addr_to_core_parses_mailbox_and_host() {
        use async_imap::imap_proto::types::Address;
        let addr = Address {
            name: Some(b"Carter St.Clair".as_slice().into()),
            mailbox: Some(b"test".as_slice().into()),
            host: Some(b"codeinfusion.com".as_slice().into()),
            adl: None,
        };
        let parsed = imap_addr_to_core(&addr).expect("parse");
        assert_eq!(parsed.name.as_deref(), Some("Carter St.Clair"));
        assert_eq!(parsed.email, "test@codeinfusion.com");

        let missing_host = Address {
            name: None,
            mailbox: Some(b"test".as_slice().into()),
            host: None,
            adl: None,
        };
        assert!(imap_addr_to_core(&missing_host).is_none());
    }
}
