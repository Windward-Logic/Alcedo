//! Microsoft OAuth (P5).

use oauth2::{CsrfToken, Scope};

use alcedo_core::{Error, OAuthTokens, Result};

use crate::loopback::LoopbackRedirect;
use crate::pkce::new_pkce_pair;
use crate::{ProviderOAuthConfig, build_oauth_client, http_client};

const MICROSOFT_AUTH_URL: &str = "https://login.microsoftonline.com/common/oauth2/v2.0/authorize";
const MICROSOFT_TOKEN_URL: &str = "https://login.microsoftonline.com/common/oauth2/v2.0/token";

pub const MICROSOFT_SCOPES: [&str; 3] = [
    "offline_access",
    "https://outlook.office.com/IMAP.AccessAsUser.All",
    "https://outlook.office.com/SMTP.Send",
];

/// Run the Microsoft OAuth PKCE flow and return tokens.
pub async fn microsoft_auth_flow<F>(
    config: &ProviderOAuthConfig,
    scopes: &[&str],
    open_auth_url: F,
) -> Result<OAuthTokens>
where
    F: FnOnce(String) + Send,
{
    let loopback = LoopbackRedirect::bind().await?;
    let redirect_url = loopback.redirect_url().clone();
    let oauth_client = build_oauth_client(
        config,
        MICROSOFT_AUTH_URL,
        MICROSOFT_TOKEN_URL,
        redirect_url,
    )?;
    let http = http_client()?;

    let (pkce_challenge, pkce_verifier) = new_pkce_pair();
    let mut authorize = oauth_client
        .authorize_url(CsrfToken::new_random)
        .set_pkce_challenge(pkce_challenge);
    for scope in scopes {
        authorize = authorize.add_scope(Scope::new((*scope).to_string()));
    }
    let (auth_url, csrf) = authorize.url();

    open_auth_url(auth_url.to_string());

    let code = loopback.wait_for_code(&csrf).await?;
    let token = oauth_client
        .exchange_code(code)
        .set_pkce_verifier(pkce_verifier)
        .request_async(&http)
        .await
        .map_err(|e| crate::map_token_error("microsoft token exchange", e))?;

    Ok(crate::tokens_from_response(token))
}

/// Refresh a Microsoft access token using a refresh token.
pub async fn refresh_microsoft(
    config: &ProviderOAuthConfig,
    refresh_token: &str,
) -> Result<OAuthTokens> {
    let redirect_url = oauth2::RedirectUrl::new("http://127.0.0.1".to_string())
        .map_err(|e| Error::Auth(format!("microsoft redirect URL invalid: {e}")))?;
    let oauth_client = build_oauth_client(
        config,
        MICROSOFT_AUTH_URL,
        MICROSOFT_TOKEN_URL,
        redirect_url,
    )?;
    let http = http_client()?;

    let token = oauth_client
        .exchange_refresh_token(&oauth2::RefreshToken::new(refresh_token.to_string()))
        .request_async(&http)
        .await
        .map_err(|e| crate::map_token_error("microsoft token refresh", e))?;

    Ok(crate::tokens_from_response(token))
}
