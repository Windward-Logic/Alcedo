//! OAuth PKCE and keyring secrets (P5).

pub mod flow;
pub mod google;
pub mod loopback;
pub mod microsoft;
pub mod pkce;
pub mod secrets;

use std::path::{Path, PathBuf};

use oauth2::TokenResponse;
use oauth2::basic::{
    BasicErrorResponse, BasicRevocationErrorResponse, BasicTokenIntrospectionResponse,
};
use oauth2::{AuthType, Client, EndpointNotSet, EndpointSet, StandardRevocableToken};
use serde::Deserialize;

use alcedo_core::{Error, Result, looks_like_network_failure};

pub use alcedo_core::OAuthTokens;
pub use flow::{OAuthFlow, OpenAuthUrl, oauth_flow};
pub use google::{GOOGLE_MAIL_SCOPE, google_auth_flow, refresh_google};
pub use microsoft::{MICROSOFT_SCOPES, microsoft_auth_flow, refresh_microsoft};
pub use secrets::{
    APPLICATION as SECRET_APPLICATION, SecretKind, delete_app_secret, delete_secret,
    get_app_secret, get_secret, relabel_account_secrets, store_app_secret, store_secret,
    with_quiet_keyring,
};

/// OAuth2 client with authorization and token endpoints configured.
pub(crate) type ConfiguredOAuthClient<TokenResponseType> = Client<
    BasicErrorResponse,
    TokenResponseType,
    BasicTokenIntrospectionResponse,
    StandardRevocableToken,
    BasicRevocationErrorResponse,
    EndpointSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointSet,
>;

/// Build an OAuth2 client with auth + token URIs and redirect URL set.
pub(crate) fn build_oauth_client<TokenResponseType: TokenResponse>(
    config: &ProviderOAuthConfig,
    auth_url: &str,
    token_url: &str,
    redirect_url: oauth2::RedirectUrl,
) -> Result<ConfiguredOAuthClient<TokenResponseType>> {
    let mut client = Client::new(oauth2::ClientId::new(config.client_id.clone()))
        .set_auth_uri(
            oauth2::AuthUrl::new(auth_url.to_string())
                .map_err(|e| Error::Auth(format!("auth URL invalid: {e}")))?,
        )
        .set_token_uri(
            oauth2::TokenUrl::new(token_url.to_string())
                .map_err(|e| Error::Auth(format!("token URL invalid: {e}")))?,
        )
        .set_redirect_uri(redirect_url)
        .set_auth_type(AuthType::RequestBody);
    if let Some(secret) = config
        .client_secret
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        client = client.set_client_secret(oauth2::ClientSecret::new(secret.to_string()));
    }
    Ok(client)
}

/// Map a token-endpoint failure to Auth (revoked/invalid) or Network (DNS/timeouts).
pub(crate) fn map_token_error<RE, TE>(what: &str, err: oauth2::RequestTokenError<RE, TE>) -> Error
where
    RE: std::error::Error + 'static,
    TE: oauth2::ErrorResponse + 'static,
{
    match err {
        oauth2::RequestTokenError::ServerResponse(resp) => {
            token_failure(what, &resp.to_string(), oauth_server_error_is_transient)
        }
        oauth2::RequestTokenError::Request(req) => {
            Error::Network(format!("{what} failed: request failed: {req}"))
        }
        oauth2::RequestTokenError::Parse(parse, body) => {
            let body = summarize_token_body(&body);
            token_failure(
                what,
                &format!("failed to parse token response: {parse}; {body}"),
                |detail| !oauth_detail_is_credential_reject(detail),
            )
        }
        oauth2::RequestTokenError::Other(msg) => token_failure(what, &msg, |detail| {
            looks_like_network_failure(detail) || oauth_server_error_is_transient(detail)
        }),
    }
}

/// Describe an unparseable token-endpoint body without echoing it: the body
/// may contain access or refresh tokens, so only the OAuth `error` code (when
/// it is a short identifier) or the body kind and size are reported.
fn summarize_token_body(body: &[u8]) -> String {
    let error_code = serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|value| value.get("error")?.as_str().map(str::to_string))
        .filter(|code| {
            !code.is_empty()
                && code.len() <= 64
                && code
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        });
    if let Some(code) = error_code {
        return format!("error={code}");
    }
    let trimmed = String::from_utf8_lossy(body);
    let kind = if trimmed.trim_start().starts_with('<') {
        "HTML"
    } else if trimmed.trim_start().starts_with('{') {
        "JSON"
    } else {
        "non-JSON"
    };
    format!("{kind} body ({} bytes)", body.len())
}

fn token_failure(what: &str, detail: &str, as_network: impl Fn(&str) -> bool) -> Error {
    let message = format!("{what} failed: {detail}");
    if as_network(detail) {
        Error::Network(message)
    } else {
        Error::Auth(message)
    }
}

fn oauth_server_error_is_transient(detail: &str) -> bool {
    let lower = detail.to_ascii_lowercase();
    lower.contains("temporarily_unavailable")
        || lower.contains("server_error")
        || looks_like_network_failure(&lower)
}

fn oauth_detail_is_credential_reject(detail: &str) -> bool {
    let lower = detail.to_ascii_lowercase();
    lower.contains("invalid_grant")
        || lower.contains("invalid_client")
        || lower.contains("unauthorized_client")
        || lower.contains("invalid_token")
        || lower.contains("access_denied")
}

/// OAuth client configuration for a single provider.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderOAuthConfig {
    pub client_id: String,
    /// Google Desktop clients include a client_secret that is not confidential.
    /// Microsoft public clients omit this.
    #[serde(default)]
    pub client_secret: Option<String>,
}

impl ProviderOAuthConfig {
    /// True when `client_secret` is present and non-empty after trim.
    pub fn has_client_secret(&self) -> bool {
        self.client_secret
            .as_deref()
            .map(str::trim)
            .is_some_and(|s| !s.is_empty())
    }
}

/// Microsoft public client compiled into this build, if the build set
/// `ALCEDO_MICROSOFT_CLIENT_ID`. Public clients have no secret, so the ID is
/// safe to ship; it is a build setting so forks do not inherit the official app.
const BUILT_IN_MICROSOFT_CLIENT_ID: Option<&str> = option_env!("ALCEDO_MICROSOFT_CLIENT_ID");

/// OAuth client IDs from `oauth.toml` and this build's built-in clients.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct OAuthConfig {
    pub google: Option<ProviderOAuthConfig>,
    pub microsoft: Option<ProviderOAuthConfig>,
}

impl OAuthConfig {
    /// Use `built_in_client_id` when `oauth.toml` names no Microsoft client.
    ///
    /// A client from `oauth.toml` always wins: refresh tokens only work with
    /// the client that issued them, so replacing it would sign accounts out.
    fn with_microsoft_fallback(mut self, built_in_client_id: Option<&str>) -> Self {
        if self.microsoft.is_none() {
            self.microsoft = built_in_client_id
                .map(str::trim)
                .filter(|id| !id.is_empty())
                .map(|id| ProviderOAuthConfig {
                    client_id: id.to_string(),
                    client_secret: None,
                });
        }
        self
    }
}

/// Convert an oauth2 token response into [`OAuthTokens`].
pub(crate) fn tokens_from_response<T: TokenResponse>(response: T) -> OAuthTokens {
    OAuthTokens {
        access_token: response.access_token().secret().clone(),
        refresh_token: response.refresh_token().map(|token| token.secret().clone()),
        expires_in: response.expires_in().map(|duration| duration.as_secs()),
        sign_in_name: None,
    }
}

/// Build a reqwest client for OAuth and provider API requests (no redirects —
/// SSRF-safe).
pub fn http_client() -> Result<reqwest::Client> {
    reqwest::ClientBuilder::new()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(HTTP_CONNECT_TIMEOUT)
        .timeout(HTTP_REQUEST_TIMEOUT)
        .build()
        .map_err(|e| Error::Network(format!("http client build failed: {e}")))
}

/// Connect timeout for OAuth and provider API HTTP requests.
pub const HTTP_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);
/// Whole-request timeout for OAuth and provider API HTTP requests.
pub const HTTP_REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// Path to `oauth.toml` under `$XDG_CONFIG_HOME/alcedo/`.
pub fn oauth_config_path() -> Option<PathBuf> {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        return Some(Path::new(&xdg).join("alcedo/oauth.toml"));
    }
    directories::BaseDirs::new().map(|dirs| dirs.config_dir().join("alcedo/oauth.toml"))
}

/// User-facing path for oauth.toml, using `~/` when it lives under the home directory.
pub fn oauth_config_path_display() -> String {
    let Some(path) = oauth_config_path() else {
        return "oauth.toml".to_string();
    };
    if let Some(home) = directories::BaseDirs::new().map(|dirs| dirs.home_dir().to_path_buf())
        && let Ok(stripped) = path.strip_prefix(home)
    {
        return format!("~/{}", stripped.display());
    }
    path.display().to_string()
}

/// Short instruction to add Google `client_secret` to oauth.toml.
pub fn google_client_secret_missing_message() -> String {
    format!(
        "Add Google client_secret to {}",
        oauth_config_path_display()
    )
}

/// Load OAuth clients from `$XDG_CONFIG_HOME/alcedo/oauth.toml`, which is
/// optional, and add this build's built-in clients.
pub fn load_oauth_config() -> Result<OAuthConfig> {
    let path = oauth_config_path().ok_or_else(|| {
        Error::Auth("could not resolve XDG config directory for oauth.toml".to_string())
    })?;
    Ok(read_oauth_config_or_default(&path)?.with_microsoft_fallback(BUILT_IN_MICROSOFT_CLIENT_ID))
}

/// Like [`load_oauth_config`], but re-parses `oauth.toml` only when its
/// modification time changes (token refreshes call this every hour per account).
pub fn load_oauth_config_cached() -> Result<OAuthConfig> {
    let path = oauth_config_path().ok_or_else(|| {
        Error::Auth("could not resolve XDG config directory for oauth.toml".to_string())
    })?;
    Ok(load_oauth_config_cached_from(&path)?.with_microsoft_fallback(BUILT_IN_MICROSOFT_CLIENT_ID))
}

/// Read `oauth.toml`, treating only a file that is really absent as empty. An
/// unreadable or invalid file stays an error, so a built-in client never
/// silently replaces the client the user configured.
fn read_oauth_config_or_default(path: &Path) -> Result<OAuthConfig> {
    match path.try_exists() {
        Ok(true) => load_oauth_config_from_path(path),
        Ok(false) => Ok(OAuthConfig::default()),
        Err(e) => Err(Error::Auth(format!(
            "read oauth config {}: {e}",
            path.display()
        ))),
    }
}

type ConfigCacheEntry = (PathBuf, Option<std::time::SystemTime>, OAuthConfig);

fn load_oauth_config_cached_from(path: &Path) -> Result<OAuthConfig> {
    static CACHE: std::sync::Mutex<Option<ConfigCacheEntry>> = std::sync::Mutex::new(None);
    let modified = std::fs::metadata(path).and_then(|m| m.modified()).ok();
    let mut cache = CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some((cached_path, cached_mtime, config)) = cache.as_ref()
        && cached_path == path
        && modified.is_some()
        && *cached_mtime == modified
    {
        return Ok(config.clone());
    }
    let config = read_oauth_config_or_default(path)?;
    *cache = Some((path.to_path_buf(), modified, config.clone()));
    Ok(config)
}

/// Load OAuth client configuration from an explicit path (tests and overrides).
pub fn load_oauth_config_from_path(path: &Path) -> Result<OAuthConfig> {
    if !path.is_file() {
        return Err(Error::Auth(format!(
            "oauth config not found: {}",
            path.display()
        )));
    }
    let contents = std::fs::read_to_string(path)
        .map_err(|e| Error::Auth(format!("read oauth config {}: {e}", path.display())))?;
    toml::from_str(&contents)
        .map_err(|e| Error::Auth(format!("parse oauth config {}: {e}", path.display())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn oauth_toml_parses_google_and_microsoft_client_ids() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("oauth.toml");
        let mut file = std::fs::File::create(&path).expect("create");
        file.write_all(
            r#"
[google]
client_id = "test-google.apps.googleusercontent.com"

[microsoft]
client_id = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"
"#
            .as_bytes(),
        )
        .expect("write");

        let config = load_oauth_config_from_path(&path).expect("parse");
        assert_eq!(
            config.google.as_ref().map(|g| g.client_id.as_str()),
            Some("test-google.apps.googleusercontent.com")
        );
        assert_eq!(
            config.microsoft.as_ref().map(|m| m.client_id.as_str()),
            Some("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee")
        );
        assert_eq!(
            config
                .google
                .as_ref()
                .and_then(|g| g.client_secret.as_deref()),
            None
        );
    }

    #[test]
    fn oauth_toml_missing_file_errors() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("missing.toml");
        let err = load_oauth_config_from_path(&path).expect_err("missing file should error");
        assert!(err.to_string().contains("oauth config not found"));
    }

    #[test]
    fn oauth_toml_parses_optional_google_client_secret() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("oauth.toml");
        let mut file = std::fs::File::create(&path).expect("create");
        file.write_all(
            r#"
[google]
client_id = "test-google.apps.googleusercontent.com"
client_secret = "GOCSPX-desktop-secret"
"#
            .as_bytes(),
        )
        .expect("write");

        let config = load_oauth_config_from_path(&path).expect("parse");
        assert_eq!(
            config
                .google
                .as_ref()
                .and_then(|g| g.client_secret.as_deref()),
            Some("GOCSPX-desktop-secret")
        );
    }

    #[test]
    fn oauth_toml_rejects_unknown_fields() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("oauth.toml");
        let mut file = std::fs::File::create(&path).expect("create");
        file.write_all(
            r#"
[google]
client_id = "test-google.apps.googleusercontent.com"
redirect_uri = "http://127.0.0.1"
"#
            .as_bytes(),
        )
        .expect("write");

        let err = load_oauth_config_from_path(&path).expect_err("unknown field should error");
        let message = err.to_string();
        assert!(message.contains("parse oauth config"), "{message}");
        assert!(message.contains("redirect_uri"), "{message}");
    }

    #[test]
    fn google_client_secret_helpers() {
        let missing = ProviderOAuthConfig {
            client_id: "id".into(),
            client_secret: None,
        };
        assert!(!missing.has_client_secret());
        let present = ProviderOAuthConfig {
            client_id: "id".into(),
            client_secret: Some("GOCSPX-x".into()),
        };
        assert!(present.has_client_secret());
        let message = google_client_secret_missing_message();
        assert!(message.contains("client_secret"), "{message}");
        assert!(message.contains("oauth.toml"), "{message}");
    }

    #[test]
    fn token_refresh_dns_failure_is_network_not_auth() {
        let err = token_failure(
            "google token refresh",
            "request failed: error sending request for url (https://oauth2.googleapis.com/token): dns error: failed to lookup address information: Temporary failure in name resolution",
            looks_like_network_failure,
        );
        assert!(matches!(err, Error::Network(_)), "{err}");
        assert!(!alcedo_core::is_auth_failure(&err));
    }

    #[test]
    fn token_refresh_invalid_grant_stays_auth() {
        let err = token_failure(
            "google token refresh",
            "invalid_grant: Token has been expired or revoked.",
            oauth_server_error_is_transient,
        );
        assert!(matches!(err, Error::Auth(_)), "{err}");
        assert!(alcedo_core::is_auth_failure(&err));
    }

    #[test]
    fn token_parse_html_is_network_unless_grant_error() {
        let html = token_failure(
            "google token refresh",
            "failed to parse token response: unexpected; body: <html>502 Bad Gateway</html>",
            |detail| !oauth_detail_is_credential_reject(detail),
        );
        assert!(matches!(html, Error::Network(_)), "{html}");

        let grant = token_failure(
            "google token refresh",
            r#"failed to parse token response: unexpected; body: {"error":"invalid_grant"}"#,
            |detail| !oauth_detail_is_credential_reject(detail),
        );
        assert!(matches!(grant, Error::Auth(_)), "{grant}");
    }

    #[test]
    fn token_parse_errors_never_echo_the_body() {
        let leaked =
            br#"{"access_token":"ya29.SECRET","refresh_token":"1//SECRET","token_type":5}"#;
        let summary = summarize_token_body(leaked);
        assert!(!summary.contains("SECRET"), "{summary}");
        assert_eq!(summary, format!("JSON body ({} bytes)", leaked.len()));
        assert_eq!(
            summarize_token_body(br#"{"error":"invalid_grant","error_description":"x"}"#),
            "error=invalid_grant"
        );
        let spaced = br#"{"error":"ya29 secret with spaces"}"#;
        assert_eq!(
            summarize_token_body(spaced),
            format!("JSON body ({} bytes)", spaced.len())
        );
        assert!(summarize_token_body(b"<html>502</html>").starts_with("HTML body"));
    }

    #[test]
    fn cached_config_reloads_only_when_file_changes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("oauth.toml");
        std::fs::write(&path, "[google]\nclient_id = \"one\"\n").unwrap();
        let first = load_oauth_config_cached_from(&path).unwrap();
        assert_eq!(first.google.unwrap().client_id, "one");
        std::fs::write(&path, "[google]\nclient_id = \"two\"\n").unwrap();
        let file = std::fs::File::options().write(true).open(&path).unwrap();
        file.set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(5))
            .unwrap();
        let second = load_oauth_config_cached_from(&path).unwrap();
        assert_eq!(second.google.unwrap().client_id, "two");
    }

    #[test]
    fn cached_config_treats_a_missing_file_as_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = load_oauth_config_cached_from(&dir.path().join("oauth.toml"))
            .expect("a missing oauth.toml is not an error");
        assert!(config.google.is_none());
        assert!(config.microsoft.is_none());
    }

    #[test]
    fn missing_oauth_toml_uses_the_built_in_microsoft_client() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = read_oauth_config_or_default(&dir.path().join("oauth.toml"))
            .expect("a missing oauth.toml is not an error")
            .with_microsoft_fallback(Some("built-in-id"));
        let microsoft = config.microsoft.expect("built-in client");
        assert_eq!(microsoft.client_id, "built-in-id");
        assert!(!microsoft.has_client_secret());
        assert!(config.google.is_none());
    }

    #[test]
    fn own_microsoft_client_wins_over_the_built_in_one() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("oauth.toml");
        std::fs::write(&path, "[microsoft]\nclient_id = \"own-id\"\n").unwrap();
        let config = read_oauth_config_or_default(&path)
            .unwrap()
            .with_microsoft_fallback(Some("built-in-id"));
        assert_eq!(config.microsoft.unwrap().client_id, "own-id");
    }

    #[test]
    fn google_only_oauth_toml_still_gets_the_built_in_microsoft_client() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("oauth.toml");
        std::fs::write(&path, "[google]\nclient_id = \"google-id\"\n").unwrap();
        let config = read_oauth_config_or_default(&path)
            .unwrap()
            .with_microsoft_fallback(Some("built-in-id"));
        assert_eq!(config.google.unwrap().client_id, "google-id");
        assert_eq!(config.microsoft.unwrap().client_id, "built-in-id");
    }

    #[test]
    fn build_without_a_client_id_leaves_microsoft_unconfigured() {
        for built_in in [None, Some(""), Some("   ")] {
            let config = OAuthConfig::default().with_microsoft_fallback(built_in);
            assert!(config.microsoft.is_none(), "{built_in:?}");
        }
    }

    #[test]
    fn invalid_oauth_toml_stays_an_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("oauth.toml");
        std::fs::write(&path, "[microsoft\n").unwrap();
        let err = read_oauth_config_or_default(&path).expect_err("invalid file should error");
        assert!(err.to_string().contains("parse oauth config"), "{err}");
    }

    #[test]
    fn oauth_transient_server_error_is_network() {
        assert!(oauth_server_error_is_transient("temporarily_unavailable"));
        assert!(oauth_detail_is_credential_reject("invalid_grant"));
        assert!(!oauth_detail_is_credential_reject("502 Bad Gateway"));
    }
}
