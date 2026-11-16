//! Loopback redirect server (P5).

use std::time::Duration;

use oauth2::{AuthorizationCode, CsrfToken, RedirectUrl};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use alcedo_core::{Error, Result};

const LOOPBACK_TIMEOUT: Duration = Duration::from_secs(300);
/// A browser sends the callback request immediately; stalled connections are dropped.
const REQUEST_READ_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_REQUEST_BYTES: usize = 16 * 1024;

const SUCCESS_HTML: &str = concat!(
    "HTTP/1.1 200 OK\r\n",
    "Content-Type: text/html; charset=utf-8\r\n",
    "Connection: close\r\n",
    "\r\n",
    "<html><body><p>Authorization successful. You can close this window.</p></body></html>"
);

const IGNORED_HTML: &str = concat!(
    "HTTP/1.1 404 Not Found\r\n",
    "Content-Type: text/plain; charset=utf-8\r\n",
    "Connection: close\r\n",
    "\r\n",
    "not found"
);

enum CallbackRequest {
    Code {
        code: String,
        state: Option<String>,
    },
    Denied {
        description: String,
        state: Option<String>,
    },
    Ignore,
}

enum CallbackOutcome {
    Code(AuthorizationCode),
    Denied(String),
    Ignore,
}

/// Ephemeral loopback redirect listener bound to `127.0.0.1:0`.
pub struct LoopbackRedirect {
    listener: TcpListener,
    redirect_url: RedirectUrl,
}

impl LoopbackRedirect {
    /// Bind an ephemeral port on the loopback interface.
    pub async fn bind() -> Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|e| Error::Network(format!("loopback bind failed: {e}")))?;
        let port = listener
            .local_addr()
            .map_err(|e| Error::Network(format!("loopback local_addr failed: {e}")))?
            .port();
        let redirect_url = RedirectUrl::new(format!("http://127.0.0.1:{port}/"))
            .map_err(|e| Error::Auth(format!("invalid redirect URL: {e}")))?;
        Ok(Self {
            listener,
            redirect_url,
        })
    }

    pub fn redirect_url(&self) -> &RedirectUrl {
        &self.redirect_url
    }

    /// Wait for `GET /?code=…&state=…` on the loopback listener (5 minute timeout).
    ///
    /// Ignores non-callback requests (favicon, prefetch) and callbacks whose `state`
    /// does not match the CSRF token from the authorize URL.
    pub async fn wait_for_code(self, expected_state: &CsrfToken) -> Result<AuthorizationCode> {
        let deadline = tokio::time::Instant::now() + LOOPBACK_TIMEOUT;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Err(Error::Auth(
                    "authorization timed out after 5 minutes".to_string(),
                ));
            }

            let (mut stream, _) = tokio::time::timeout(remaining, self.listener.accept())
                .await
                .map_err(|_| Error::Auth("authorization timed out after 5 minutes".to_string()))?
                .map_err(|e| Error::Network(format!("loopback accept failed: {e}")))?;

            // A stalled, oversized or broken connection must not block or abort
            // the flow; drop it and keep waiting for the real callback.
            let read_timeout = REQUEST_READ_TIMEOUT.min(remaining);
            let Some(buf) = read_request_head(&mut stream, read_timeout).await else {
                continue;
            };
            let request = String::from_utf8_lossy(&buf);

            match outcome_for_request(&request, expected_state) {
                CallbackOutcome::Code(code) => {
                    let _ = stream.write_all(SUCCESS_HTML.as_bytes()).await;
                    let _ = stream.flush().await;
                    return Ok(code);
                }
                CallbackOutcome::Denied(description) => {
                    let _ = stream.write_all(IGNORED_HTML.as_bytes()).await;
                    return Err(Error::Auth(format!("oauth error: {description}")));
                }
                CallbackOutcome::Ignore => {
                    let _ = stream.write_all(IGNORED_HTML.as_bytes()).await;
                    let _ = stream.flush().await;
                }
            }
        }
    }
}

/// Read an HTTP request head (up to the blank line) within `timeout`.
async fn read_request_head<S: tokio::io::AsyncRead + Unpin>(
    stream: &mut S,
    timeout: Duration,
) -> Option<Vec<u8>> {
    let read = async {
        let mut buf = Vec::with_capacity(1024);
        loop {
            if buf.len() > MAX_REQUEST_BYTES {
                return None;
            }
            let mut chunk = [0u8; 1024];
            let n = stream.read(&mut chunk).await.ok()?;
            if n == 0 {
                return Some(buf);
            }
            buf.extend_from_slice(&chunk[..n]);
            if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                return Some(buf);
            }
        }
    };
    tokio::time::timeout(timeout, read).await.ok().flatten()
}

fn csrf_matches(expected: &CsrfToken, got: Option<&str>) -> bool {
    got.is_some_and(|got| got == expected.secret())
}

fn outcome_for_request(request: &str, expected_state: &CsrfToken) -> CallbackOutcome {
    match classify_request(request) {
        CallbackRequest::Code { code, state } if csrf_matches(expected_state, state.as_deref()) => {
            CallbackOutcome::Code(AuthorizationCode::new(code))
        }
        CallbackRequest::Denied { description, state }
            if csrf_matches(expected_state, state.as_deref()) =>
        {
            CallbackOutcome::Denied(description)
        }
        _ => CallbackOutcome::Ignore,
    }
}

fn classify_request(request: &str) -> CallbackRequest {
    let first_line = request.lines().next().unwrap_or("");
    if !first_line.starts_with("GET ") {
        return CallbackRequest::Ignore;
    }

    let path = first_line.split_whitespace().nth(1).unwrap_or("").trim();
    let query = path.split('?').nth(1).unwrap_or("");

    let mut code: Option<String> = None;
    let mut state: Option<String> = None;
    let mut error: Option<String> = None;
    let mut error_description: Option<String> = None;

    for (key, value) in url::form_urlencoded::parse(query.as_bytes()) {
        if key == "code" && !value.is_empty() && code.is_none() {
            code = Some(value.into_owned());
        } else if key == "state" && !value.is_empty() && state.is_none() {
            state = Some(value.into_owned());
        } else if key == "error" && !value.is_empty() && error.is_none() {
            error = Some(value.into_owned());
        } else if key == "error_description" && !value.is_empty() && error_description.is_none() {
            error_description = Some(value.into_owned());
        }
    }

    if let Some(code) = code {
        CallbackRequest::Code { code, state }
    } else if let Some(error) = error {
        CallbackRequest::Denied {
            description: error_description.unwrap_or(error),
            state,
        }
    } else {
        CallbackRequest::Ignore
    }
}

impl std::fmt::Debug for CallbackOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Code(_) => f.debug_tuple("Code").field(&"***").finish(),
            Self::Denied(description) => f.debug_tuple("Denied").field(description).finish(),
            Self::Ignore => write!(f, "Ignore"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token(state: &str) -> CsrfToken {
        CsrfToken::new(state.to_string())
    }

    #[tokio::test]
    async fn stalled_or_oversized_requests_are_dropped_not_fatal() {
        let (mut client, mut server) = tokio::io::duplex(64 * 1024);
        assert!(
            read_request_head(&mut server, Duration::from_millis(50))
                .await
                .is_none(),
            "a silent connection must time out"
        );
        client
            .write_all(&vec![b'a'; MAX_REQUEST_BYTES + 2048])
            .await
            .unwrap();
        assert!(
            read_request_head(&mut server, Duration::from_secs(1))
                .await
                .is_none()
        );

        let (mut client, mut server) = tokio::io::duplex(4096);
        client
            .write_all(b"GET /?code=abc&state=s HTTP/1.1\r\nHost: x\r\n\r\n")
            .await
            .unwrap();
        let head = read_request_head(&mut server, Duration::from_secs(1))
            .await
            .unwrap();
        assert!(head.starts_with(b"GET /?code=abc"));
    }

    #[test]
    fn matching_state_accepts_percent_decoded_code() {
        let request = "GET /?code=abc%2Fdef&state=csrf-token HTTP/1.1\r\n\r\n";
        match outcome_for_request(request, &token("csrf-token")) {
            CallbackOutcome::Code(code) => assert_eq!(code.secret(), "abc/def"),
            other => panic!("expected code, got mismatch: {other:?}"),
        }
    }

    #[test]
    fn missing_or_wrong_state_is_ignored() {
        let expected = token("csrf-token");
        assert!(matches!(
            outcome_for_request("GET /?code=abc HTTP/1.1\r\n\r\n", &expected),
            CallbackOutcome::Ignore
        ));
        assert!(matches!(
            outcome_for_request("GET /?code=abc&state=other HTTP/1.1\r\n\r\n", &expected),
            CallbackOutcome::Ignore
        ));
    }

    #[test]
    fn matching_state_reports_error_description() {
        let request = "GET /?error=access_denied&error_description=user%20denied&state=csrf-token HTTP/1.1\r\n\r\n";
        match outcome_for_request(request, &token("csrf-token")) {
            CallbackOutcome::Denied(description) => assert!(description.contains("user denied")),
            other => panic!("expected denied, got {other:?}"),
        }
    }

    #[test]
    fn denied_without_matching_state_is_ignored() {
        let request = "GET /?error=access_denied&state=other HTTP/1.1\r\n\r\n";
        assert!(matches!(
            outcome_for_request(request, &token("csrf-token")),
            CallbackOutcome::Ignore
        ));
    }

    #[test]
    fn favicon_is_ignored() {
        assert!(matches!(
            classify_request("GET /favicon.ico HTTP/1.1\r\n\r\n"),
            CallbackRequest::Ignore
        ));
    }
}
