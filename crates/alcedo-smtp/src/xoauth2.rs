//! SMTP AUTH XOAUTH2 without SASL initial-response.
//!
//! Lettre always puts the XOAUTH2 payload on the `AUTH` command line. RFC 5321
//! limits that line to 512 octets, and Google access tokens routinely overflow
//! it. Gmail then replies `334` with an error JSON; lettre treats XOAUTH2 as
//! client-first and fails with "This mechanism does not expect a challenge".
//!
//! IMAP already avoids this by answering the server `+` continuation. SMTP
//! does the same here: `AUTH XOAUTH2`, then the payload, then an empty line if
//! Google sends the documented error challenge.

use std::fmt::{Display, Formatter};
use std::time::Duration;

use alcedo_core::{Error, Result};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use lettre::{
    address::Envelope,
    transport::smtp::{client::AsyncSmtpConnection, extension::ClientId},
};

use crate::send::{SmtpAuth, SmtpOptions, smtp_tls_parameters};

const SMTP_TIMEOUT: Duration = Duration::from_secs(60);

/// Build the raw XOAUTH2 SASL string (before base64 encoding).
pub fn xoauth2_sasl_string(email: &str, access_token: &str) -> String {
    format!("user={email}\x01auth=Bearer {access_token}\x01\x01")
}

/// Base64-encoded XOAUTH2 client response.
pub fn encode_xoauth2(email: &str, access_token: &str) -> String {
    STANDARD.encode(xoauth2_sasl_string(email, access_token))
}

/// Connect, authenticate with XOAUTH2, and send `message`.
pub async fn send_message_xoauth2(
    opts: &SmtpOptions,
    envelope: &Envelope,
    rfc822: &[u8],
) -> Result<()> {
    let SmtpAuth::XOAuth2(token) = &opts.auth else {
        return Err(Error::Smtp(
            "XOAUTH2 send requires XOAUTH2 credentials".to_string(),
        ));
    };

    let mut conn = connect_smtp(opts).await?;
    match authenticate_xoauth2(&mut conn, opts.auth_username(), token).await {
        Ok(()) => {}
        Err(err) => {
            conn.abort().await;
            return Err(err);
        }
    }

    let send = conn.send(envelope, rfc822).await;
    match send {
        Ok(_) => {
            let _ = conn.quit().await;
            Ok(())
        }
        Err(err) => Err(Error::Smtp(format!(
            "send failed via {}:{}: {err}",
            opts.host, opts.port
        ))),
    }
}

async fn connect_smtp(opts: &SmtpOptions) -> Result<AsyncSmtpConnection> {
    let hello = ClientId::default();
    let tls = smtp_tls_parameters(&opts.host)?;

    if opts.starttls {
        let mut conn = AsyncSmtpConnection::connect_tokio1(
            (opts.host.as_str(), opts.port),
            Some(SMTP_TIMEOUT),
            &hello,
            None,
            None,
        )
        .await
        .map_err(|e| Error::Smtp(format!("connect {}:{}: {e}", opts.host, opts.port)))?;
        conn.starttls(tls, &hello)
            .await
            .map_err(|e| Error::Smtp(format!("starttls {}:{}: {e}", opts.host, opts.port)))?;
        Ok(conn)
    } else {
        AsyncSmtpConnection::connect_tokio1(
            (opts.host.as_str(), opts.port),
            Some(SMTP_TIMEOUT),
            &hello,
            Some(tls),
            None,
        )
        .await
        .map_err(|e| Error::Smtp(format!("connect {}:{}: {e}", opts.host, opts.port)))
    }
}

async fn authenticate_xoauth2(
    conn: &mut AsyncSmtpConnection,
    username: &str,
    token: &str,
) -> Result<()> {
    let encoded = encode_xoauth2(username, token);
    let mut response = conn
        .command(SmtpLine("AUTH XOAUTH2"))
        .await
        .map_err(|e| Error::Smtp(format!("XOAUTH2 authenticate failed: {e}")))?;

    if response.has_code(235) {
        return Ok(());
    }
    if !response.has_code(334) {
        return Err(auth_failed(&response));
    }

    response = conn
        .command(SmtpLine(&encoded))
        .await
        .map_err(|e| Error::Smtp(format!("XOAUTH2 authenticate failed: {e}")))?;

    if response.has_code(334) {
        response = conn
            .command(SmtpLine(""))
            .await
            .map_err(|e| Error::Smtp(format!("XOAUTH2 authenticate failed: {e}")))?;
    }

    if response.has_code(235) {
        Ok(())
    } else {
        Err(auth_failed(&response))
    }
}

fn auth_failed(response: &lettre::transport::smtp::response::Response) -> Error {
    let detail = response.first_line().unwrap_or("authentication rejected");
    Error::Smtp(format!(
        "XOAUTH2 authenticate failed: {} {detail}",
        response.code()
    ))
}

struct SmtpLine<'a>(&'a str);

impl Display for SmtpLine<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}\r\n", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xoauth2_encoding_matches_spec() {
        let raw = xoauth2_sasl_string("user@example.com", "ya29.test-token");
        assert_eq!(
            raw,
            "user=user@example.com\x01auth=Bearer ya29.test-token\x01\x01"
        );
        assert_eq!(
            encode_xoauth2("user@example.com", "ya29.test-token"),
            STANDARD.encode(raw.as_bytes())
        );
    }

    #[test]
    fn auth_command_without_initial_response_fits_smtp_line() {
        const SMTP_COMMAND_LIMIT: usize = 512;
        assert!("AUTH XOAUTH2\r\n".len() < SMTP_COMMAND_LIMIT);

        let long_token = "a".repeat(800);
        let encoded = encode_xoauth2("codeinfusion@gmail.com", &long_token);
        let with_ir = format!("AUTH XOAUTH2 {encoded}\r\n");
        assert!(
            with_ir.len() > SMTP_COMMAND_LIMIT,
            "IR form should overflow so the continuation path stays justified"
        );
    }
}
