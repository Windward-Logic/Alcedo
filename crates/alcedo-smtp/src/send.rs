//! SMTP message send via lettre + rustls.

use std::time::Duration;

use alcedo_core::{
    ComposeAttachment, ComposeDraft, ComposeFormat, ComposeInline, Error, Result,
    crlf_line_endings, format_mailbox, is_single_reaction_emoji, msg_id_list, parse_address_token,
    reaction_bodies,
};
use lettre::{
    AsyncSmtpTransport, AsyncTransport, Tokio1Executor,
    address::Envelope,
    message::{
        Attachment, Body, Mailbox, Message, MultiPart, MultiPartBuilder, SinglePart, header,
    },
    transport::smtp::{
        authentication::Credentials,
        client::{Certificate, CertificateStore, Tls, TlsParameters},
    },
};

use crate::xoauth2;
use tracing::debug;

/// SMTP authentication credentials.
#[derive(Clone)]
pub enum SmtpAuth {
    Password(String),
    XOAuth2(String),
}

impl std::fmt::Debug for SmtpAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SmtpAuth::Password(_) => f.debug_tuple("Password").field(&"***").finish(),
            SmtpAuth::XOAuth2(_) => f.debug_tuple("XOAuth2").field(&"***").finish(),
        }
    }
}

/// Connection and identity options for sending mail.
#[derive(Clone)]
pub struct SmtpOptions {
    pub host: String,
    pub port: u16,
    pub starttls: bool,
    /// SMTP AUTH username. Empty means use [`Self::email`].
    pub username: String,
    /// SMTP AUTH/mailbox address and fallback MIME From, not always SMTP `MAIL FROM`.
    pub email: String,
    pub display_name: String,
    /// MIME `From` address; defaults to [`Self::email`] when unset.
    pub from_email: Option<String>,
    /// MIME `From` display name; defaults to [`Self::display_name`] when unset.
    pub from_display_name: Option<String>,
    /// Envelope identity for `Sender` when it differs from MIME `From`.
    pub envelope_email: Option<String>,
    pub auth: SmtpAuth,
}

impl SmtpOptions {
    pub fn auth_username(&self) -> &str {
        if self.username.is_empty() {
            &self.email
        } else {
            &self.username
        }
    }

    pub fn mime_from_email(&self) -> &str {
        self.from_email.as_deref().unwrap_or(&self.email)
    }

    pub fn mime_from_display_name(&self) -> &str {
        self.from_display_name
            .as_deref()
            .unwrap_or(&self.display_name)
    }

    pub fn envelope_from_email(&self) -> &str {
        self.envelope_email.as_deref().unwrap_or(&self.email)
    }
}

impl std::fmt::Debug for SmtpOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SmtpOptions")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("starttls", &self.starttls)
            .field("email", &self.email)
            .field("username", &self.username)
            .field("display_name", &self.display_name)
            .field("from_email", &self.from_email)
            .field("from_display_name", &self.from_display_name)
            .field("envelope_email", &self.envelope_email)
            .field("auth", &self.auth)
            .finish()
    }
}

/// Already-rendered plain and optional HTML bodies for MIME assembly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenderedBodies {
    pub plain: String,
    pub html: Option<String>,
}

/// Send a composed message over SMTP (port 465 implicit TLS or 587 STARTTLS).
///
/// Returns the `Message-ID` header and the exact RFC822 bytes that were sent
/// (for IMAP Sent APPEND). Bytes are captured before SMTP consumes the message.
pub async fn send_rendered(
    opts: SmtpOptions,
    draft: &ComposeDraft,
    bodies: &RenderedBodies,
) -> Result<(String, Vec<u8>)> {
    let from_mailbox = mailbox_from_parts(opts.mime_from_display_name(), opts.mime_from_email())?;
    let envelope_email = opts.envelope_from_email();
    let (message, message_id_hdr) =
        build_message(&from_mailbox, envelope_email, draft, bodies, None)?;
    let rfc822 = message_to_rfc822(&message);
    send_message(
        opts,
        message.envelope(),
        &rfc822,
        smtp_transfer_timeout(rfc822.len()),
    )
    .await?;
    Ok((message_id_hdr, rfc822))
}

/// Build an RFC822 draft MIME message for IMAP APPEND (no MDN / tracking pixel headers).
pub fn build_draft_message(
    opts: &SmtpOptions,
    draft: &ComposeDraft,
    bodies: &RenderedBodies,
    stable_message_id: Option<&str>,
) -> Result<(Message, String)> {
    let from_mailbox = mailbox_from_parts(opts.mime_from_display_name(), opts.mime_from_email())?;
    let envelope_email = opts.envelope_from_email();
    let extra = DraftHeaders {
        compose_format: draft.format,
        identity_id: draft.identity_id.map(|id| id.0.to_string()),
        custom_from: if draft.identity_id.is_none() {
            draft.custom_from.clone()
        } else {
            None
        },
    };
    build_message(
        &from_mailbox,
        envelope_email,
        draft,
        bodies,
        Some((stable_message_id, &extra)),
    )
}

/// Serialize a lettre message to RFC822 bytes.
pub fn message_to_rfc822(message: &Message) -> Vec<u8> {
    message.formatted()
}

/// Total deadline for one send: connect, TLS, AUTH and DATA.
///
/// Lettre's async client only applies its timeout to the TCP connect. A server
/// that accepts the connection and then stops reading a multi-megabyte DATA
/// would otherwise leave Send spinning with no error. This is a deadline for
/// the whole send, not an inactivity timeout, so it grows with the message
/// assuming at least 32 KiB/s of upstream (a 20 MiB message gets ~11 minutes)
/// and is capped at 15 minutes.
pub fn smtp_transfer_timeout(bytes: usize) -> Duration {
    const BASE_SECS: u64 = 60;
    const CAP_SECS: u64 = 900;
    const MIN_BYTES_PER_SEC: usize = 32 * 1024;
    let extra = (bytes / MIN_BYTES_PER_SEC) as u64;
    Duration::from_secs((BASE_SECS + extra).min(CAP_SECS))
}

/// Send already-serialized RFC822 bytes to the envelope recipients.
///
/// Sending the same bytes the caller keeps for the Sent copy avoids formatting
/// a large message twice and guarantees the copy matches what went out.
pub async fn send_message(
    opts: SmtpOptions,
    envelope: &Envelope,
    rfc822: &[u8],
    timeout: Duration,
) -> Result<()> {
    debug!(host = %opts.host, port = opts.port, "smtp send");
    let host = opts.host.clone();
    let port = opts.port;
    match tokio::time::timeout(timeout, send_message_unlimited(opts, envelope, rfc822)).await {
        Ok(result) => result,
        // The deadline can fire after the server accepted DATA but before we
        // read its final reply, so say so instead of implying nothing was sent.
        Err(_) => Err(Error::Smtp(format!(
            "timed out sending via {host}:{port}; the message may still have been delivered"
        ))),
    }
}

async fn send_message_unlimited(
    opts: SmtpOptions,
    envelope: &Envelope,
    rfc822: &[u8],
) -> Result<()> {
    if matches!(opts.auth, SmtpAuth::XOAuth2(_)) {
        return xoauth2::send_message_xoauth2(&opts, envelope, rfc822).await;
    }
    let mailer = build_transport(&opts)?;
    mailer
        .send_raw(envelope, rfc822)
        .await
        .map_err(|e| Error::Smtp(format!("send failed via {}:{}: {e}", opts.host, opts.port)))?;
    Ok(())
}

/// CRLF MIME body bytes of the inner message tree (no RFC822 headers).
pub fn serialize_inner_body(draft: &ComposeDraft, bodies: &RenderedBodies) -> Result<Vec<u8>> {
    match build_body_content(draft, bodies)? {
        BodyContent::Single(part) => Ok(part.formatted()),
        BodyContent::Multi(part) => Ok(part.formatted()),
    }
}

/// PGP-wrapped body parts for [`build_pgp_message`].
#[derive(Clone, Debug)]
pub struct PgpMimeBody {
    pub encrypted: bool,
    pub signed_part: Option<Vec<u8>>,
    pub signature: Option<Vec<u8>>,
    pub payload: Vec<u8>,
}

/// Build an RFC822 message with PGP/MIME wrapping.
pub fn build_pgp_message(
    from: &Mailbox,
    envelope_email: &str,
    draft: &ComposeDraft,
    pgp: &PgpMimeBody,
) -> Result<(Message, String)> {
    let domain = envelope_email
        .split('@')
        .nth(1)
        .filter(|d| !d.is_empty())
        .unwrap_or("localhost");
    let message_id_hdr = format!("<{}@{}>", uuid::Uuid::new_v4(), domain);

    let mut builder = Message::builder()
        .from(from.clone())
        .subject(&draft.subject)
        .message_id(Some(message_id_hdr.clone()));

    let mime_from: &str = from.email.as_ref();
    if !mime_from.eq_ignore_ascii_case(envelope_email) {
        let sender = mailbox_from_parts("", envelope_email)?;
        builder = builder.sender(sender);
    }

    for addr in &draft.to {
        builder = builder.to(parse_mailbox(addr)?);
    }
    for addr in &draft.cc {
        builder = builder.cc(parse_mailbox(addr)?);
    }
    for addr in &draft.bcc {
        builder = builder.bcc(parse_mailbox(addr)?);
    }

    if draft.request_mdn {
        builder = builder.header(DispositionNotificationTo(envelope_email.to_string()));
    }
    if let Some(header) = draft.autocrypt_header.as_deref().filter(|s| !s.is_empty()) {
        builder = builder.header(AutocryptHeader(header.to_string()));
    }
    builder = with_threading_headers(builder, draft);

    let body = if pgp.encrypted {
        builder.multipart(
            MultiPart::encrypted("application/pgp-encrypted".into())
                .singlepart(
                    SinglePart::builder()
                        .header(
                            header::ContentType::parse("application/pgp-encrypted")
                                .map_err(|e| Error::Smtp(e.to_string()))?,
                        )
                        .body("Version: 1".to_string()),
                )
                .singlepart(
                    SinglePart::builder()
                        .header(
                            header::ContentType::parse("application/octet-stream")
                                .map_err(|e| Error::Smtp(e.to_string()))?,
                        )
                        .body(pgp.payload.clone()),
                ),
        )
    } else {
        let signed_inner = pgp
            .signed_part
            .clone()
            .unwrap_or_else(|| pgp.payload.clone());
        let signature = pgp.signature.clone().unwrap_or_else(|| pgp.payload.clone());
        builder.singlepart(build_signed_singlepart(&signed_inner, &signature)?)
    };

    body.map_err(|e| Error::Smtp(format!("build pgp message: {e}")))
        .map(|message| (message, message_id_hdr))
}

/// Send a PGP-wrapped composed message over SMTP.
///
/// Returns the `Message-ID` header and the exact RFC822 bytes that were sent.
pub async fn send_pgp_message(
    opts: SmtpOptions,
    draft: &ComposeDraft,
    pgp: &PgpMimeBody,
) -> Result<(String, Vec<u8>)> {
    let from_mailbox = mailbox_from_parts(opts.mime_from_display_name(), opts.mime_from_email())?;
    let envelope_email = opts.envelope_from_email();
    let (message, message_id_hdr) = build_pgp_message(&from_mailbox, envelope_email, draft, pgp)?;
    let rfc822 = message_to_rfc822(&message);
    send_message(
        opts,
        message.envelope(),
        &rfc822,
        smtp_transfer_timeout(rfc822.len()),
    )
    .await?;
    Ok((message_id_hdr, rfc822))
}

/// Validate that a tracking pixel URL uses HTTPS.
pub fn validate_https_pixel_url(pixel_url: &str) -> Result<()> {
    let parsed = url::Url::parse(pixel_url)
        .map_err(|e| Error::Smtp(format!("invalid tracking pixel URL: {e}")))?;
    if parsed.scheme() != "https" {
        return Err(Error::Smtp("tracking pixel URL must use https".to_string()));
    }
    if parsed.host_str().is_none() {
        return Err(Error::Smtp(
            "tracking pixel URL must include a host".to_string(),
        ));
    }
    Ok(())
}

fn build_transport(opts: &SmtpOptions) -> Result<AsyncSmtpTransport<Tokio1Executor>> {
    let SmtpAuth::Password(password) = &opts.auth else {
        return Err(Error::Smtp(
            "password transport requires password credentials".to_string(),
        ));
    };
    let creds = Credentials::new(opts.auth_username().to_string(), password.clone());

    let tls = smtp_tls_parameters(&opts.host)?;
    let mut builder = if opts.starttls {
        AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&opts.host)
            .map_err(|e| Error::Smtp(format!("starttls builder: {e}")))?
            .tls(Tls::Required(tls))
    } else {
        AsyncSmtpTransport::<Tokio1Executor>::relay(&opts.host)
            .map_err(|e| Error::Smtp(format!("tls relay builder: {e}")))?
            .tls(Tls::Wrapper(tls))
    };

    builder = builder.port(opts.port).credentials(creds);

    Ok(builder.build())
}

/// TLS parameters trusting the bundled Mozilla roots plus the system store.
///
/// lettre's default under rustls is the Mozilla roots alone, which rejects
/// servers behind a corporate or self-managed CA that IMAP already accepts
/// (see `alcedo_tls`). A system certificate lettre cannot parse is skipped.
pub(crate) fn smtp_tls_parameters(host: &str) -> Result<TlsParameters> {
    let mut builder =
        TlsParameters::builder(host.to_string()).certificate_store(CertificateStore::WebpkiRoots);
    for der in alcedo_tls::native_root_certs_der() {
        if let Ok(cert) = Certificate::from_der(der.clone()) {
            builder = builder.add_root_certificate(cert);
        }
    }
    builder
        .build()
        .map_err(|e| Error::Smtp(format!("tls parameters for {host}: {e}")))
}

pub(crate) struct DraftHeaders {
    compose_format: ComposeFormat,
    identity_id: Option<String>,
    custom_from: Option<String>,
}

pub(crate) fn build_message(
    from: &Mailbox,
    envelope_email: &str,
    draft: &ComposeDraft,
    bodies: &RenderedBodies,
    draft_headers: Option<(Option<&str>, &DraftHeaders)>,
) -> Result<(Message, String)> {
    let domain = envelope_email
        .split('@')
        .nth(1)
        .filter(|d| !d.is_empty())
        .unwrap_or("localhost");
    let message_id_hdr = draft_headers
        .and_then(|(stable, _)| stable)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("<{}@{}>", uuid::Uuid::new_v4(), domain));

    let mut builder = Message::builder()
        .from(from.clone())
        .subject(&draft.subject)
        .message_id(Some(message_id_hdr.clone()));

    let mime_from: &str = from.email.as_ref();
    if !mime_from.eq_ignore_ascii_case(envelope_email) {
        let sender = mailbox_from_parts("", envelope_email)?;
        builder = builder.sender(sender);
    }

    for addr in &draft.to {
        builder = builder.to(parse_mailbox(addr)?);
    }
    for addr in &draft.cc {
        builder = builder.cc(parse_mailbox(addr)?);
    }
    // Lettre derives the SMTP envelope from To+Cc+Bcc, then strips Bcc from MIME.
    // Do not call keep_bcc() — that would leak Bcc to other recipients.
    for addr in &draft.bcc {
        builder = builder.bcc(parse_mailbox(addr)?);
    }

    // IMAP drafts are valid without To/Cc/Bcc (RFC 3501). Lettre still requires an
    // envelope, so use From as a dummy destination and omit those headers.
    if draft_headers.is_some() && draft.to.is_empty() && draft.cc.is_empty() && draft.bcc.is_empty()
    {
        builder = builder.envelope(
            Envelope::new(Some(from.email.clone()), vec![from.email.clone()])
                .map_err(|e| Error::Smtp(format!("build message: {e}")))?,
        );
    }

    if draft.request_mdn {
        builder = builder.header(DispositionNotificationTo(envelope_email.to_string()));
    }
    if let Some(header) = draft.autocrypt_header.as_deref().filter(|s| !s.is_empty()) {
        builder = builder.header(AutocryptHeader(header.to_string()));
    }
    builder = with_threading_headers(builder, draft);

    if let Some((_, extra)) = draft_headers {
        // Betterbird/Thunderbird keep the Message-ID, and replace the previous
        // Drafts copy, only when they see this header. Without it they allocate
        // a new Message-ID and the old draft is left behind as a second message.
        builder = builder.header(MozillaDraftInfo(mozilla_draft_info(draft)));
        builder = builder.header(AlcedoComposeFormat(extra.compose_format.as_str()));
        if let Some(id) = extra.identity_id.as_deref().filter(|s| !s.is_empty()) {
            builder = builder.header(AlcedoIdentity(id.to_string()));
        }
        if let Some(from) = extra
            .custom_from
            .as_deref()
            .filter(|value| !value.is_empty() && !value.contains('\r') && !value.contains('\n'))
        {
            builder = builder.header(AlcedoFrom(from.to_string()));
        }
    }

    let body = if let Some(emoji) = draft
        .reaction_emoji
        .as_deref()
        .filter(|emoji| is_single_reaction_emoji(emoji))
    {
        let from_name = from.name.as_deref().unwrap_or("");
        let from_email = from.email.as_ref();
        let (plain, html, json) = reaction_bodies(from_name, from_email, emoji);
        builder.multipart(
            MultiPart::alternative()
                .singlepart(
                    SinglePart::builder()
                        .header(header::ContentType::TEXT_PLAIN)
                        .body(plain),
                )
                .singlepart(
                    SinglePart::builder()
                        .header(
                            header::ContentType::parse(
                                "text/vnd.google.email-reaction+json; charset=utf-8",
                            )
                            .map_err(|e| Error::Smtp(e.to_string()))?,
                        )
                        .body(json),
                )
                .singlepart(
                    SinglePart::builder()
                        .header(header::ContentType::TEXT_HTML)
                        .body(html),
                ),
        )
    } else {
        match build_body_content(draft, bodies)? {
            BodyContent::Single(part) => builder.singlepart(part),
            BodyContent::Multi(part) => builder.multipart(part),
        }
    };

    body.map_err(|e| Error::Smtp(format!("build message: {e}")))
        .map(|message| (message, message_id_hdr))
}

enum BodyContent {
    Single(SinglePart),
    Multi(MultiPart),
}

/// In-Reply-To and References with every id in `<>` (RFC 5322 §3.6.4).
/// Stored ids are bare because mail-parser strips the brackets.
fn with_threading_headers(
    mut builder: lettre::message::MessageBuilder,
    draft: &ComposeDraft,
) -> lettre::message::MessageBuilder {
    let in_reply_to = draft.in_reply_to.as_deref().map(msg_id_list);
    if let Some(irt) = in_reply_to.filter(|ids| !ids.is_empty()) {
        builder = builder.header(header::InReplyTo::from(irt));
    }
    let references = draft.references.as_deref().map(msg_id_list);
    if let Some(refs) = references.filter(|ids| !ids.is_empty()) {
        builder = builder.header(header::References::from(refs));
    }
    builder
}

/// Header values must stay on one line; CR/LF would start a new header.
fn single_line(value: &str) -> String {
    value.chars().filter(|c| *c != '\r' && *c != '\n').collect()
}

fn build_body_content(draft: &ComposeDraft, bodies: &RenderedBodies) -> Result<BodyContent> {
    let has_attachments = !draft.attachments.is_empty();
    let has_inlines = !draft.inlines.is_empty();

    if !has_attachments && !has_inlines {
        return Ok(if let Some(html) = &bodies.html {
            BodyContent::Multi(MultiPart::alternative_plain_html(
                bodies.plain.clone(),
                html.clone(),
            ))
        } else {
            BodyContent::Single(plain_singlepart(bodies))
        });
    }

    if has_inlines {
        let mut related = with_primary_body(MultiPart::related(), bodies);
        for inline in &draft.inlines {
            related = related.singlepart(inline_part(inline)?);
        }
        if has_attachments {
            return Ok(BodyContent::Multi(append_attachments(
                MultiPart::mixed().multipart(related),
                &draft.attachments,
            )?));
        }
        return Ok(BodyContent::Multi(related));
    }

    Ok(BodyContent::Multi(append_attachments(
        with_primary_body(MultiPart::mixed(), bodies),
        &draft.attachments,
    )?))
}

fn parse_content_type(mime: &str) -> Result<header::ContentType> {
    header::ContentType::parse(mime).map_err(|e| Error::Smtp(format!("invalid mime {mime}: {e}")))
}

fn plain_singlepart(bodies: &RenderedBodies) -> SinglePart {
    SinglePart::builder()
        .header(header::ContentType::TEXT_PLAIN)
        .body(bodies.plain.clone())
}

fn with_primary_body(builder: MultiPartBuilder, bodies: &RenderedBodies) -> MultiPart {
    if let Some(html) = &bodies.html {
        builder.multipart(MultiPart::alternative_plain_html(
            bodies.plain.clone(),
            html.clone(),
        ))
    } else {
        builder.singlepart(plain_singlepart(bodies))
    }
}

fn inline_part(inline: &ComposeInline) -> Result<SinglePart> {
    let content_type = parse_content_type(&inline.mime)?;
    Ok(
        Attachment::new_inline_with_name(inline.content_id.clone(), inline.filename.clone())
            .body(inline.bytes.clone(), content_type),
    )
}

fn file_attachment_part(attachment: &ComposeAttachment) -> Result<SinglePart> {
    let content_type = parse_content_type(&attachment.mime)?;
    Ok(Attachment::new(attachment.filename.clone()).body(attachment.bytes.clone(), content_type))
}

fn append_attachments(
    mut mixed: MultiPart,
    attachments: &[ComposeAttachment],
) -> Result<MultiPart> {
    for attachment in attachments {
        mixed = mixed.singlepart(file_attachment_part(attachment)?);
    }
    Ok(mixed)
}

/// Thunderbird `X-Mozilla-Draft-Info`. `deliveryformat` is 1 for plaintext and 2 for HTML.
fn mozilla_draft_info(draft: &ComposeDraft) -> String {
    let receipt = if draft.request_mdn { "1" } else { "0" };
    let deliveryformat = match draft.format {
        ComposeFormat::Html => "2",
        ComposeFormat::Plain | ComposeFormat::Markdown => "1",
    };
    format!(
        "internal/draft; vcard=0; receipt={receipt}; DSN=0; uuencode=0; attachmentreminder=0; deliveryformat={deliveryformat}"
    )
}

fn mailbox_from_parts(display_name: &str, email: &str) -> Result<Mailbox> {
    let formatted = if display_name.is_empty() {
        email.to_string()
    } else {
        format_mailbox(Some(display_name), email)
    };
    formatted
        .parse()
        .map_err(|e| Error::Smtp(format!("invalid from address: {e}")))
}

fn parse_mailbox(addr: &str) -> Result<Mailbox> {
    if let Ok(mailbox) = addr.parse::<Mailbox>() {
        return Ok(mailbox);
    }
    if let Some(parsed) = parse_address_token(addr) {
        let formatted = parsed.formatted();
        if formatted != addr {
            return formatted
                .parse()
                .map_err(|e| Error::Smtp(format!("invalid address {addr:?}: {e}")));
        }
    }
    addr.parse()
        .map_err(|e| Error::Smtp(format!("invalid address {addr:?}: {e}")))
}

/// `multipart/signed` (RFC 3156) whose first part is `signed_part` verbatim.
///
/// The signature covers those exact bytes, so they are written into the body
/// as they are. Rebuilding the part through lettre re-encodes it: it drops a
/// folded `Content-Type` (losing the inner boundary), adds a transfer-encoding
/// header, and changes line endings, any of which breaks verification.
/// `signed_part` must not end with the CRLF that precedes the boundary.
fn build_signed_singlepart(signed_part: &[u8], signature: &[u8]) -> Result<SinglePart> {
    let boundary = format!("=_alcedo_{}", uuid::Uuid::new_v4().simple());
    let mut body = Vec::with_capacity(signed_part.len() + signature.len() + 256);
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(signed_part);
    body.extend_from_slice(format!("\r\n--{boundary}\r\n").as_bytes());
    body.extend_from_slice(
        b"Content-Type: application/pgp-signature; name=\"signature.asc\"\r\n\
          Content-Description: OpenPGP digital signature\r\n\r\n",
    );
    let signature = crlf_line_endings(signature);
    body.extend_from_slice(&signature);
    if !signature.ends_with(b"\r\n") {
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());

    let content_type = SignedContentType(format!(
        "multipart/signed; protocol=\"application/pgp-signature\"; micalg=\"pgp-sha256\"; \
         boundary=\"{boundary}\""
    ));
    Ok(SinglePart::builder()
        .header(content_type)
        .body(Body::dangerous_pre_encoded(
            body,
            header::ContentTransferEncoding::SevenBit,
        )))
}

/// Exact `Content-Type` value for the signed container, kept as written so the
/// boundary and micalg parameters are not normalised.
#[derive(Clone, Debug)]
struct SignedContentType(String);

impl header::Header for SignedContentType {
    fn name() -> header::HeaderName {
        header::HeaderName::new_from_ascii_str("Content-Type")
    }

    fn parse(s: &str) -> std::result::Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        Ok(Self(s.trim().to_string()))
    }

    fn display(&self) -> header::HeaderValue {
        header::HeaderValue::new(Self::name(), self.0.clone())
    }
}

/// RFC 8098 MDN request header.
#[derive(Clone, Debug)]
struct DispositionNotificationTo(String);

/// Autocrypt header (phase 3).
#[derive(Clone, Debug)]
struct AutocryptHeader(String);

#[derive(Clone, Debug)]
struct MozillaDraftInfo(String);

#[derive(Clone, Debug)]
struct AlcedoComposeFormat(&'static str);

#[derive(Clone, Debug)]
struct AlcedoIdentity(String);

#[derive(Clone, Debug)]
struct AlcedoFrom(String);

impl header::Header for MozillaDraftInfo {
    fn name() -> header::HeaderName {
        header::HeaderName::new_from_ascii_str("X-Mozilla-Draft-Info")
    }

    fn parse(s: &str) -> std::result::Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        Ok(Self(s.trim().to_string()))
    }

    fn display(&self) -> header::HeaderValue {
        header::HeaderValue::new(Self::name(), self.0.clone())
    }
}

impl header::Header for AlcedoComposeFormat {
    fn name() -> header::HeaderName {
        header::HeaderName::new_from_ascii_str("X-Alcedo-Compose-Format")
    }

    fn parse(s: &str) -> std::result::Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        Ok(Self(match s.trim() {
            "html" => "html",
            "markdown" => "markdown",
            _ => "plain",
        }))
    }

    fn display(&self) -> header::HeaderValue {
        header::HeaderValue::new(Self::name(), self.0.to_string())
    }
}

impl header::Header for AlcedoIdentity {
    fn name() -> header::HeaderName {
        header::HeaderName::new_from_ascii_str("X-Alcedo-Identity")
    }

    fn parse(s: &str) -> std::result::Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        Ok(Self(s.trim().to_string()))
    }

    fn display(&self) -> header::HeaderValue {
        header::HeaderValue::new(Self::name(), self.0.clone())
    }
}

impl header::Header for AlcedoFrom {
    fn name() -> header::HeaderName {
        header::HeaderName::new_from_ascii_str("X-Alcedo-From")
    }

    fn parse(s: &str) -> std::result::Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        Ok(Self(s.trim().to_string()))
    }

    fn display(&self) -> header::HeaderValue {
        header::HeaderValue::new(Self::name(), self.0.clone())
    }
}

impl header::Header for AutocryptHeader {
    fn name() -> header::HeaderName {
        header::HeaderName::new_from_ascii_str("Autocrypt")
    }

    fn parse(s: &str) -> std::result::Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        Ok(Self(s.trim().to_string()))
    }

    fn display(&self) -> header::HeaderValue {
        header::HeaderValue::new(Self::name(), single_line(&self.0))
    }
}

impl header::Header for DispositionNotificationTo {
    fn name() -> header::HeaderName {
        header::HeaderName::new_from_ascii_str("Disposition-Notification-To")
    }

    fn parse(s: &str) -> std::result::Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        Ok(Self(
            s.trim().trim_matches(|c| c == '<' || c == '>').to_string(),
        ))
    }

    fn display(&self) -> header::HeaderValue {
        header::HeaderValue::new(Self::name(), format!("<{}>", single_line(&self.0)))
    }
}

#[cfg(test)]
mod tests {
    use alcedo_core::{AccountId, ComposeFormat};

    use super::*;
    use uuid::Uuid;

    #[test]
    fn smtp_transfer_timeout_grows_with_the_message_and_caps() {
        assert_eq!(smtp_transfer_timeout(0).as_secs(), 60);
        assert_eq!(smtp_transfer_timeout(32 * 1024).as_secs(), 61);
        assert_eq!(smtp_transfer_timeout(20 * 1024 * 1024).as_secs(), 700);
        assert_eq!(smtp_transfer_timeout(100 * 1024 * 1024).as_secs(), 900);
    }

    #[test]
    fn smtp_tls_parameters_build_with_system_roots() {
        smtp_tls_parameters("smtp.example.com").expect("tls parameters");
    }

    fn sample_draft() -> ComposeDraft {
        ComposeDraft {
            account_id: AccountId(Uuid::nil()),
            to: vec!["to@example.com".to_string()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "Test".to_string(),
            body_text: "Hello".to_string(),
            format: ComposeFormat::Plain,
            request_mdn: false,
            tracking_pixel_url: None,
            attachments: Vec::new(),
            inlines: Vec::new(),
            collect_outgoing_contacts: true,
            in_reply_to: None,
            references: None,
            thread_id: None,
            approve_contact_email: None,
            sign: false,
            encrypt: false,
            autocrypt_header: None,
            identity_id: None,
            custom_from: None,
            imap_draft: None,
            reaction_emoji: None,
        }
    }

    fn sample_opts() -> SmtpOptions {
        SmtpOptions {
            host: "smtp.example.com".to_string(),
            port: 587,
            starttls: true,
            email: "user@example.com".to_string(),
            username: String::new(),
            display_name: "User".to_string(),
            from_email: None,
            from_display_name: None,
            envelope_email: None,
            auth: SmtpAuth::Password("secret".to_string()),
        }
    }

    const PNG_BYTES: &[u8] = &[
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90,
        0x77, 0x53, 0xde, 0x00, 0x00, 0x00, 0x0c, 0x49, 0x44, 0x41, 0x54, 0x08, 0xd7, 0x63, 0xf8,
        0xcf, 0xc0, 0x00, 0x00, 0x03, 0x01, 0x01, 0x00, 0x18, 0xdd, 0x8d, 0xb4, 0x00, 0x00, 0x00,
        0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ];

    #[test]
    fn plain_without_html_is_singlepart() {
        let draft = sample_draft();
        let bodies = RenderedBodies {
            plain: "Hello".to_string(),
            html: None,
        };
        let from: Mailbox = "User <user@example.com>".parse().unwrap();
        let (msg, message_id_hdr) =
            build_message(&from, "user@example.com", &draft, &bodies, None).unwrap();
        let formatted = msg.formatted();
        let raw = String::from_utf8_lossy(&formatted);
        assert!(raw.contains("text/plain"));
        assert!(!raw.contains("multipart/alternative"));
        assert!(raw.contains("Message-ID"));
        assert!(raw.contains(&message_id_hdr));
    }

    #[test]
    fn html_body_is_multipart_alternative() {
        let draft = sample_draft();
        let bodies = RenderedBodies {
            plain: "Hello".to_string(),
            html: Some("<html><body>Hello</body></html>".to_string()),
        };
        let from: Mailbox = "User <user@example.com>".parse().unwrap();
        let (msg, _message_id_hdr) =
            build_message(&from, "user@example.com", &draft, &bodies, None).unwrap();
        let formatted = msg.formatted();
        let raw = String::from_utf8_lossy(&formatted);
        assert!(raw.contains("multipart/alternative"));
        assert!(raw.contains("text/plain"));
        assert!(raw.contains("text/html"));
    }

    #[test]
    fn mdn_header_independent_of_format() {
        let mut draft = sample_draft();
        draft.request_mdn = true;
        let bodies = RenderedBodies {
            plain: "Hi".to_string(),
            html: None,
        };
        let from: Mailbox = "User <user@example.com>".parse().unwrap();
        let (msg, _message_id_hdr) =
            build_message(&from, "user@example.com", &draft, &bodies, None).unwrap();
        let formatted = msg.formatted();
        let raw = String::from_utf8_lossy(&formatted);
        assert!(raw.contains("Disposition-Notification-To"));
    }

    #[test]
    fn pdf_attachment_is_multipart_mixed() {
        let mut draft = sample_draft();
        draft.attachments.push(alcedo_core::ComposeAttachment {
            filename: "invoice.pdf".to_string(),
            mime: "application/pdf".to_string(),
            bytes: vec![0x25, 0x50, 0x44, 0x46, 0x2d],
        });
        let bodies = RenderedBodies {
            plain: "Hello".to_string(),
            html: None,
        };
        let from: Mailbox = "User <user@example.com>".parse().unwrap();
        let (msg, _message_id_hdr) =
            build_message(&from, "user@example.com", &draft, &bodies, None).unwrap();
        let formatted = msg.formatted();
        let raw = String::from_utf8_lossy(&formatted);
        assert!(raw.contains("multipart/mixed"));
        assert!(raw.contains("Content-Disposition: attachment"));
        assert!(raw.contains("invoice.pdf"));
        assert!(!raw.contains("multipart/related"));
        assert_eq!(raw.matches("multipart/mixed").count(), 1);
    }

    #[test]
    fn png_inline_with_html_is_multipart_related() {
        let mut draft = sample_draft();
        draft.format = ComposeFormat::Html;
        draft.body_text = "<img src=\"cid:abc@alcedo.mail\">".to_string();
        draft.inlines.push(alcedo_core::ComposeInline {
            content_id: "abc@alcedo.mail".to_string(),
            filename: "photo.png".to_string(),
            mime: "image/png".to_string(),
            bytes: PNG_BYTES.to_vec(),
        });
        let bodies = RenderedBodies {
            plain: String::new(),
            html: Some("<html><body><img src=\"cid:abc@alcedo.mail\"></body></html>".to_string()),
        };
        let from: Mailbox = "User <user@example.com>".parse().unwrap();
        let (msg, _message_id_hdr) =
            build_message(&from, "user@example.com", &draft, &bodies, None).unwrap();
        let formatted = msg.formatted();
        let raw = String::from_utf8_lossy(&formatted);
        assert!(raw.contains("multipart/related"));
        assert!(raw.contains("Content-ID"));
        assert!(raw.contains("inline"));
        assert!(raw.contains("abc@alcedo.mail"));
    }

    #[test]
    fn attachment_and_inline_is_mixed_wrapping_related() {
        let mut draft = sample_draft();
        draft.inlines.push(alcedo_core::ComposeInline {
            content_id: "abc@alcedo.mail".to_string(),
            filename: "photo.png".to_string(),
            mime: "image/png".to_string(),
            bytes: PNG_BYTES.to_vec(),
        });
        draft.attachments.push(alcedo_core::ComposeAttachment {
            filename: "invoice.pdf".to_string(),
            mime: "application/pdf".to_string(),
            bytes: vec![0x25, 0x50, 0x44, 0x46, 0x2d],
        });
        let bodies = RenderedBodies {
            plain: "Hello".to_string(),
            html: Some("<html><body><img src=\"cid:abc@alcedo.mail\"></body></html>".to_string()),
        };
        let from: Mailbox = "User <user@example.com>".parse().unwrap();
        let (msg, _message_id_hdr) =
            build_message(&from, "user@example.com", &draft, &bodies, None).unwrap();
        let formatted = msg.formatted();
        let raw = String::from_utf8_lossy(&formatted);
        assert!(raw.contains("multipart/mixed"));
        assert!(raw.contains("multipart/related"));
        assert!(raw.contains("Content-Disposition: attachment"));
        assert!(raw.contains("invoice.pdf"));
    }

    #[test]
    fn bcc_is_in_envelope_not_mime_headers() {
        let mut draft = sample_draft();
        draft.bcc = vec!["secret-bcc@example.com".to_string()];
        let bodies = RenderedBodies {
            plain: "Hello".to_string(),
            html: None,
        };
        let from: Mailbox = "User <user@example.com>".parse().unwrap();
        let (msg, _message_id_hdr) =
            build_message(&from, "user@example.com", &draft, &bodies, None).unwrap();
        let envelope_addrs: Vec<String> = msg
            .envelope()
            .to()
            .iter()
            .map(|addr| addr.to_string())
            .collect();
        assert!(
            envelope_addrs
                .iter()
                .any(|addr| addr.contains("to@example.com"))
        );
        assert!(
            envelope_addrs
                .iter()
                .any(|addr| addr.contains("secret-bcc@example.com"))
        );
        let formatted = msg.formatted();
        let raw = String::from_utf8_lossy(&formatted);
        assert!(!raw.to_ascii_lowercase().contains("bcc:"));
        assert!(!raw.contains("secret-bcc@example.com"));
    }

    #[test]
    fn bcc_only_message_builds() {
        let mut draft = sample_draft();
        draft.to.clear();
        draft.cc.clear();
        draft.bcc = vec!["secret-bcc@example.com".to_string()];
        let bodies = RenderedBodies {
            plain: "Hello".to_string(),
            html: None,
        };
        let from: Mailbox = "User <user@example.com>".parse().unwrap();
        let (msg, _message_id_hdr) =
            build_message(&from, "user@example.com", &draft, &bodies, None).unwrap();
        let envelope_addrs: Vec<String> = msg
            .envelope()
            .to()
            .iter()
            .map(|addr| addr.to_string())
            .collect();
        assert!(
            envelope_addrs
                .iter()
                .any(|addr| addr.contains("secret-bcc@example.com"))
        );
        let formatted = msg.formatted();
        let raw = String::from_utf8_lossy(&formatted);
        assert!(!raw.to_ascii_lowercase().contains("bcc:"));
    }

    #[test]
    fn rejects_non_https_pixel_url() {
        let err = validate_https_pixel_url("javascript:alert(1)").expect_err("js url");
        assert!(err.to_string().contains("https"));
        validate_https_pixel_url("https://example.com/p.gif").expect("https ok");
    }

    #[test]
    fn smtp_auth_debug_redacts_secrets() {
        let auth = SmtpAuth::Password("smtp-secret".to_string());
        let debug = format!("{auth:?}");
        assert!(debug.contains("***"));
        assert!(!debug.contains("smtp-secret"));

        let opts = SmtpOptions {
            host: "smtp.example.com".to_string(),
            port: 587,
            starttls: true,
            email: "user@example.com".to_string(),
            username: String::new(),
            display_name: "User".to_string(),
            from_email: None,
            from_display_name: None,
            envelope_email: None,
            auth: SmtpAuth::XOAuth2("oauth-token".to_string()),
        };
        let debug = format!("{opts:?}");
        assert!(debug.contains("***"));
        assert!(!debug.contains("oauth-token"));
    }

    #[test]
    fn reaction_draft_builds_gmail_multipart_order() {
        let mut draft = sample_draft();
        draft.reaction_emoji = Some("👍".to_string());
        draft.in_reply_to = Some("<parent@example.com>".to_string());
        let bodies = RenderedBodies {
            plain: "ignored".to_string(),
            html: None,
        };
        let from: Mailbox = "User <user@example.com>".parse().unwrap();
        let (msg, _message_id_hdr) =
            build_message(&from, "user@example.com", &draft, &bodies, None).unwrap();
        let formatted = msg.formatted();
        let raw = String::from_utf8_lossy(&formatted);
        let plain_pos = raw.find("text/plain").expect("plain");
        let json_pos = raw
            .find("text/vnd.google.email-reaction+json")
            .expect("json");
        let html_pos = raw.find("text/html").expect("html");
        assert!(plain_pos < json_pos);
        assert!(json_pos < html_pos);
        assert!(raw.contains("\"version\":1"));
        assert!(!raw.contains("Content-Disposition: attachment"));
        assert!(raw.contains("In-Reply-To: <parent@example.com>"));
        assert!(raw.contains("User reacted"));
        assert!(raw.contains("=F0=9F=91=8D") || raw.contains("👍"));
        assert!(!raw.contains("via Alcedo"));
    }

    #[test]
    fn reply_draft_includes_threading_headers() {
        let mut draft = sample_draft();
        draft.in_reply_to = Some("<parent@example.com>".to_string());
        draft.references = Some("<root@example.com> <parent@example.com>".to_string());
        let bodies = RenderedBodies {
            plain: "Reply".to_string(),
            html: None,
        };
        let from: Mailbox = "User <user@example.com>".parse().unwrap();
        let (msg, _message_id_hdr) =
            build_message(&from, "user@example.com", &draft, &bodies, None).unwrap();
        let formatted = msg.formatted();
        let raw = String::from_utf8_lossy(&formatted);
        assert!(raw.contains("In-Reply-To: <parent@example.com>"));
        assert!(raw.contains("References: <root@example.com> <parent@example.com>"));
    }

    #[test]
    fn reply_draft_brackets_bare_threading_ids() {
        let mut draft = sample_draft();
        draft.in_reply_to = Some("parent@example.com".to_string());
        draft.references = Some("root@example.com parent@example.com".to_string());
        let bodies = RenderedBodies {
            plain: "Reply".to_string(),
            html: None,
        };
        let from: Mailbox = "User <user@example.com>".parse().unwrap();
        let (msg, _message_id_hdr) =
            build_message(&from, "user@example.com", &draft, &bodies, None).unwrap();
        let formatted = msg.formatted();
        let raw = String::from_utf8_lossy(&formatted);
        assert!(raw.contains("In-Reply-To: <parent@example.com>"));
        assert!(raw.contains("References: <root@example.com> <parent@example.com>"));
    }

    #[test]
    fn pgp_message_brackets_bare_threading_ids() {
        let mut draft = sample_draft();
        draft.in_reply_to = Some("parent@example.com".to_string());
        draft.references = Some("parent@example.com".to_string());
        let pgp = PgpMimeBody {
            encrypted: false,
            signed_part: Some(b"Content-Type: text/plain\r\n\r\nHi\r\n".to_vec()),
            signature: Some(b"sig".to_vec()),
            payload: b"sig".to_vec(),
        };
        let from: Mailbox = "User <user@example.com>".parse().unwrap();
        let (msg, _) = build_pgp_message(&from, "user@example.com", &draft, &pgp).unwrap();
        let raw = String::from_utf8_lossy(&msg.formatted()).into_owned();
        assert!(raw.contains("In-Reply-To: <parent@example.com>"));
        assert!(raw.contains("References: <parent@example.com>"));
    }

    #[test]
    fn custom_header_values_cannot_inject_lines() {
        let mut draft = sample_draft();
        draft.request_mdn = true;
        draft.autocrypt_header = Some("addr=a@x; keydata=AAA\r\nBcc: evil@x".to_string());
        let bodies = RenderedBodies {
            plain: "Hello".to_string(),
            html: None,
        };
        let from: Mailbox = "User <user@example.com>".parse().unwrap();
        let (msg, _) = build_message(&from, "user@example.com", &draft, &bodies, None).unwrap();
        let raw = String::from_utf8_lossy(&msg.formatted()).into_owned();
        assert!(!raw.lines().any(|line| line.starts_with("Bcc:")));
    }

    #[test]
    fn new_compose_omits_threading_headers() {
        let draft = sample_draft();
        let bodies = RenderedBodies {
            plain: "Hello".to_string(),
            html: None,
        };
        let from: Mailbox = "User <user@example.com>".parse().unwrap();
        let (msg, _message_id_hdr) =
            build_message(&from, "user@example.com", &draft, &bodies, None).unwrap();
        let formatted = msg.formatted();
        let raw = String::from_utf8_lossy(&formatted);
        assert!(!raw.contains("In-Reply-To:"));
        assert!(!raw.contains("References:"));
    }

    #[test]
    fn autocrypt_header_only_when_draft_field_set() {
        let bodies = RenderedBodies {
            plain: "Hello".to_string(),
            html: None,
        };
        let from: Mailbox = "User <user@example.com>".parse().unwrap();
        let draft = sample_draft();
        let (msg, _) = build_message(&from, "user@example.com", &draft, &bodies, None).unwrap();
        let formatted = msg.formatted();
        let raw = String::from_utf8_lossy(&formatted);
        assert!(!raw.contains("Autocrypt:"));

        let mut with_header = sample_draft();
        with_header.autocrypt_header = Some("addr=user@example.com; keydata=abc".to_string());
        let (msg, _) =
            build_message(&from, "user@example.com", &with_header, &bodies, None).unwrap();
        let formatted = msg.formatted();
        let raw = String::from_utf8_lossy(&formatted);
        assert!(raw.contains("Autocrypt:"));
        assert!(raw.contains("addr=user@example.com"));
    }

    #[test]
    fn serialize_inner_body_succeeds_without_from_header() {
        let draft = sample_draft();
        let bodies = RenderedBodies {
            plain: "Hello".to_string(),
            html: None,
        };
        let inner = serialize_inner_body(&draft, &bodies).unwrap();
        let raw = String::from_utf8_lossy(&inner);
        assert!(raw.contains("text/plain"));
        assert!(!raw.contains("From:"));
    }

    #[test]
    fn signed_message_carries_the_signed_part_verbatim() {
        let bodies = RenderedBodies {
            plain: "Hello wörld".to_string(),
            html: Some("<p>Hello</p>".to_string()),
        };
        let mut with_file = sample_draft();
        with_file.attachments.push(alcedo_core::ComposeAttachment {
            filename: "invoice.pdf".to_string(),
            mime: "application/pdf".to_string(),
            bytes: (0..=255u8).collect(),
        });
        for draft in [sample_draft(), with_file] {
            let inner = serialize_inner_body(&draft, &bodies).unwrap();
            let signed_part = inner.strip_suffix(b"\r\n").unwrap().to_vec();
            let pgp = PgpMimeBody {
                encrypted: false,
                signed_part: Some(signed_part.clone()),
                signature: Some(b"-----BEGIN PGP SIGNATURE-----\r\nabc\r\n".to_vec()),
                payload: Vec::new(),
            };
            let from: Mailbox = "User <user@example.com>".parse().unwrap();
            let (msg, _) = build_pgp_message(&from, "user@example.com", &draft, &pgp).unwrap();
            let raw = msg.formatted();
            let text = String::from_utf8_lossy(&raw);

            let boundary = text
                .split("boundary=\"")
                .nth(1)
                .and_then(|rest| rest.split('"').next())
                .expect("signed container boundary");
            let mut expected = format!("--{boundary}\r\n").into_bytes();
            expected.extend_from_slice(&signed_part);
            expected.extend_from_slice(
                format!("\r\n--{boundary}\r\nContent-Type: application/pgp-signature").as_bytes(),
            );
            assert!(
                raw.windows(expected.len()).any(|window| window == expected),
                "the signed first part must appear unchanged between the delimiters"
            );
            assert!(text.contains("micalg=\"pgp-sha256\""));
        }
    }

    #[test]
    fn signed_message_has_no_bare_line_feeds() {
        let bodies = RenderedBodies {
            plain: "Hello wörld".to_string(),
            html: Some("<p>Hello</p>".to_string()),
        };
        let mut with_file = sample_draft();
        with_file.attachments.push(alcedo_core::ComposeAttachment {
            filename: "invoice.pdf".to_string(),
            mime: "application/pdf".to_string(),
            bytes: (0..=255u8).collect(),
        });
        let crlf_only = |bytes: &[u8]| {
            bytes.first() != Some(&b'\n')
                && bytes.windows(2).all(|w| w[1] != b'\n' || w[0] == b'\r')
        };
        for draft in [sample_draft(), with_file] {
            let inner = serialize_inner_body(&draft, &bodies).unwrap();
            let pgp = PgpMimeBody {
                encrypted: false,
                signed_part: Some(inner.strip_suffix(b"\r\n").unwrap().to_vec()),
                signature: Some(
                    b"-----BEGIN PGP SIGNATURE-----\n\nabc\n-----END PGP SIGNATURE-----\n".to_vec(),
                ),
                payload: Vec::new(),
            };
            let from: Mailbox = "User <user@example.com>".parse().unwrap();
            let (msg, _) = build_pgp_message(&from, "user@example.com", &draft, &pgp).unwrap();
            assert!(
                crlf_only(&msg.formatted()),
                "every LF on the wire must be preceded by CR"
            );
        }
    }

    #[test]
    fn pgp_signed_message_structure() {
        let draft = sample_draft();
        let inner = b"Content-Type: text/plain; charset=utf-8\r\n\r\nHello\r\n".to_vec();
        let pgp = PgpMimeBody {
            encrypted: false,
            signed_part: Some(inner),
            signature: Some(b"signature-bytes".to_vec()),
            payload: b"signature-bytes".to_vec(),
        };
        let from: Mailbox = "User <user@example.com>".parse().unwrap();
        let (msg, _) = build_pgp_message(&from, "user@example.com", &draft, &pgp).unwrap();
        let formatted = msg.formatted();
        let raw = String::from_utf8_lossy(&formatted);
        assert!(raw.contains("multipart/signed"));
        assert!(raw.contains("application/pgp-signature"));
        assert!(raw.contains("micalg=\"pgp-sha256\""));
        assert!(raw.contains("Content-Type: text/plain"));
        assert!(!raw.contains("application/octet-stream"));
    }

    #[test]
    fn pgp_encrypted_message_structure() {
        let draft = sample_draft();
        let pgp = PgpMimeBody {
            encrypted: true,
            signed_part: None,
            signature: None,
            payload: b"encrypted payload".to_vec(),
        };
        let from: Mailbox = "User <user@example.com>".parse().unwrap();
        let (msg, _) = build_pgp_message(&from, "user@example.com", &draft, &pgp).unwrap();
        let formatted = msg.formatted();
        let raw = String::from_utf8_lossy(&formatted);
        assert!(raw.contains("multipart/encrypted"));
        assert!(raw.contains("application/pgp-encrypted"));
        assert!(raw.contains("Version: 1"));
        assert!(raw.contains("application/octet-stream"));
    }

    #[test]
    fn bcc_not_leaked_in_mime_headers() {
        let mut draft = sample_draft();
        draft.bcc.push("secret@example.com".to_string());
        let bodies = RenderedBodies {
            plain: "Hello".to_string(),
            html: None,
        };
        let from: Mailbox = "User <user@example.com>".parse().unwrap();
        let (msg, _) = build_message(&from, "user@example.com", &draft, &bodies, None).unwrap();
        let formatted = msg.formatted();
        let raw = String::from_utf8_lossy(&formatted);
        assert!(!raw.contains("secret@example.com"));
    }

    #[test]
    fn sender_header_when_from_differs_from_envelope() {
        let draft = sample_draft();
        let bodies = RenderedBodies {
            plain: "Hello".to_string(),
            html: None,
        };
        let from: Mailbox = "Alias <alias@example.com>".parse().unwrap();
        let (msg, _) = build_message(&from, "user@example.com", &draft, &bodies, None).unwrap();
        let formatted = msg.formatted();
        let raw = String::from_utf8_lossy(&formatted);
        assert!(raw.contains("From: Alias <alias@example.com>"));
        assert!(raw.contains("Sender:") && raw.contains("user@example.com"));
        assert_eq!(
            msg.envelope().from().expect("reverse-path").to_string(),
            "user@example.com"
        );
    }

    #[test]
    fn aligned_from_and_envelope_omits_sender() {
        let draft = sample_draft();
        let bodies = RenderedBodies {
            plain: "Hello".to_string(),
            html: None,
        };
        let from: Mailbox = "Alias <alias@example.com>".parse().unwrap();
        let (msg, _) = build_message(&from, "alias@example.com", &draft, &bodies, None).unwrap();
        let formatted = msg.formatted();
        let raw = String::from_utf8_lossy(&formatted);
        assert!(raw.contains("From: Alias <alias@example.com>"));
        assert!(!raw.contains("Sender:"));
        assert_eq!(
            msg.envelope().from().expect("reverse-path").to_string(),
            "alias@example.com"
        );
    }

    #[test]
    fn no_sender_when_from_matches_envelope() {
        let draft = sample_draft();
        let bodies = RenderedBodies {
            plain: "Hello".to_string(),
            html: None,
        };
        let from: Mailbox = "User <user@example.com>".parse().unwrap();
        let (msg, _) = build_message(&from, "user@example.com", &draft, &bodies, None).unwrap();
        let formatted = msg.formatted();
        let raw = String::from_utf8_lossy(&formatted);
        assert!(!raw.contains("Sender:"));
    }

    #[test]
    fn pgp_sender_when_from_differs_from_envelope() {
        let draft = sample_draft();
        let inner = b"Content-Type: text/plain; charset=utf-8\r\n\r\nHello\r\n".to_vec();
        let pgp = PgpMimeBody {
            encrypted: false,
            signed_part: Some(inner),
            signature: Some(b"signature-bytes".to_vec()),
            payload: b"signature-bytes".to_vec(),
        };
        let from: Mailbox = "Alias <alias@example.com>".parse().unwrap();
        let (msg, _) = build_pgp_message(&from, "user@example.com", &draft, &pgp).unwrap();
        let formatted = msg.formatted();
        let raw = String::from_utf8_lossy(&formatted);
        assert!(raw.contains("Sender:") && raw.contains("user@example.com"));
        assert_eq!(
            msg.envelope().from().expect("reverse-path").to_string(),
            "user@example.com"
        );
    }

    #[test]
    fn pgp_aligned_from_and_envelope_omits_sender() {
        let draft = sample_draft();
        let inner = b"Content-Type: text/plain; charset=utf-8\r\n\r\nHello\r\n".to_vec();
        let pgp = PgpMimeBody {
            encrypted: false,
            signed_part: Some(inner),
            signature: Some(b"signature-bytes".to_vec()),
            payload: b"signature-bytes".to_vec(),
        };
        let from: Mailbox = "Alias <alias@example.com>".parse().unwrap();
        let (msg, _) = build_pgp_message(&from, "alias@example.com", &draft, &pgp).unwrap();
        let formatted = msg.formatted();
        let raw = String::from_utf8_lossy(&formatted);
        assert!(!raw.contains("Sender:"));
        assert_eq!(
            msg.envelope().from().expect("reverse-path").to_string(),
            "alias@example.com"
        );
    }

    #[test]
    fn draft_message_includes_alcedo_headers_and_stable_message_id() {
        let draft = sample_draft();
        let bodies = RenderedBodies {
            plain: "Hello".to_string(),
            html: None,
        };
        let opts = SmtpOptions {
            host: "smtp.example.com".to_string(),
            port: 587,
            starttls: true,
            email: "user@example.com".to_string(),
            username: String::new(),
            display_name: "User".to_string(),
            from_email: None,
            from_display_name: None,
            envelope_email: None,
            auth: SmtpAuth::Password("secret".to_string()),
        };
        let stable = "<stable@example.com>";
        let (msg, id) = build_draft_message(&opts, &draft, &bodies, Some(stable)).unwrap();
        assert_eq!(id, stable);
        let formatted = msg.formatted();
        let raw = String::from_utf8_lossy(&formatted);
        assert!(raw.contains("X-Alcedo-Compose-Format: plain"));
        assert!(
            raw.contains("X-Mozilla-Draft-Info:") && raw.contains("internal/draft"),
            "missing X-Mozilla-Draft-Info in {raw}"
        );
        assert!(raw.contains("deliveryformat=1"));
        assert!(raw.contains("Message-ID: <stable@example.com>"));
        assert!(!raw.contains("Disposition-Notification-To"));
    }

    #[test]
    fn draft_message_allows_empty_recipients() {
        let mut draft = sample_draft();
        draft.to.clear();
        let bodies = RenderedBodies {
            plain: "Still writing".to_string(),
            html: None,
        };
        let (msg, _) = build_draft_message(&sample_opts(), &draft, &bodies, None).unwrap();
        let formatted = msg.formatted();
        let raw = String::from_utf8_lossy(&formatted);
        assert!(!raw.lines().any(|line| line.starts_with("To:")));
        assert!(raw.contains("From:"));
        assert!(raw.contains("X-Alcedo-Compose-Format: plain"));
        assert!(raw.contains("Still writing"));
    }

    #[test]
    fn parse_mailbox_quotes_display_name_with_hash() {
        let mailbox = parse_mailbox("Payal k (#TSI2930) <payal@tanishasystems.com>")
            .expect("parse recruiter address");
        assert_eq!(mailbox.email.to_string(), "payal@tanishasystems.com");
        assert_eq!(mailbox.name, Some("Payal k (#TSI2930)".into()));
    }

    #[test]
    fn send_message_requires_a_recipient() {
        let mut draft = sample_draft();
        draft.to.clear();
        let bodies = RenderedBodies {
            plain: "Hello".to_string(),
            html: None,
        };
        let from: Mailbox = "User <user@example.com>".parse().unwrap();
        let err = build_message(&from, "user@example.com", &draft, &bodies, None).unwrap_err();
        assert!(
            err.to_string().contains("missing destination"),
            "unexpected error: {err}"
        );
    }
}
