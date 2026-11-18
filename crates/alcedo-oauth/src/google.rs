//! Google OAuth (P5).

use oauth2::basic::BasicTokenResponse;
use oauth2::{CsrfToken, Scope};

use alcedo_core::{Error, OAuthTokens, Result};

use crate::flow::{BoxFuture, OAuthFlow, OpenAuthUrl, missing_client_message};
use crate::loopback::LoopbackRedirect;
use crate::pkce::new_pkce_pair;
use crate::{
    OAuthConfig, ProviderOAuthConfig, build_oauth_client, http_client, tokens_from_response,
};

const GOOGLE_AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const GOOGLE_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";

pub const GOOGLE_MAIL_SCOPE: &str = "https://mail.google.com/";

/// Run the Google OAuth PKCE flow and return tokens.
pub async fn google_auth_flow<F>(
    config: &ProviderOAuthConfig,
    scopes: &[&str],
    open_auth_url: F,
) -> Result<OAuthTokens>
where
    F: FnOnce(String) + Send,
{
    require_google_client_secret(config)?;
    let loopback = LoopbackRedirect::bind().await?;
    let redirect_url = loopback.redirect_url().clone();
    let oauth_client = build_oauth_client::<BasicTokenResponse>(
        config,
        GOOGLE_AUTH_URL,
        GOOGLE_TOKEN_URL,
        redirect_url,
    )?;
    let http = http_client()?;

    let (pkce_challenge, pkce_verifier) = new_pkce_pair();
    let mut authorize = oauth_client
        .authorize_url(CsrfToken::new_random)
        .add_extra_param("access_type", "offline")
        .add_extra_param("prompt", "consent")
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
        .map_err(|e| crate::map_token_error("google token exchange", e))?;

    Ok(tokens_from_response(token))
}

/// Refresh a Google access token using a refresh token.
pub async fn refresh_google(
    config: &ProviderOAuthConfig,
    refresh_token: &str,
) -> Result<OAuthTokens> {
    require_google_client_secret(config)?;
    let redirect_url = oauth2::RedirectUrl::new("http://127.0.0.1".to_string())
        .map_err(|e| Error::Auth(format!("google redirect URL invalid: {e}")))?;
    let oauth_client = build_oauth_client::<BasicTokenResponse>(
        config,
        GOOGLE_AUTH_URL,
        GOOGLE_TOKEN_URL,
        redirect_url,
    )?;
    let http = http_client()?;

    let token = oauth_client
        .exchange_refresh_token(&oauth2::RefreshToken::new(refresh_token.to_string()))
        .request_async(&http)
        .await
        .map_err(|e| crate::map_token_error("google token refresh", e))?;

    Ok(tokens_from_response(token))
}

/// Google sign-in for mail accounts.
pub(crate) struct GoogleFlow;

impl OAuthFlow for GoogleFlow {
    fn client_config(
        &self,
        config: &OAuthConfig,
    ) -> std::result::Result<ProviderOAuthConfig, String> {
        let client = config
            .google
            .clone()
            .ok_or_else(|| missing_client_message("Google"))?;
        if client.has_client_secret() {
            Ok(client)
        } else {
            Err(crate::google_client_secret_missing_message())
        }
    }

    fn sign_in<'a>(
        &'a self,
        config: &'a ProviderOAuthConfig,
        open_auth_url: OpenAuthUrl,
    ) -> BoxFuture<'a, Result<OAuthTokens>> {
        Box::pin(google_auth_flow(
            config,
            &[GOOGLE_MAIL_SCOPE],
            open_auth_url,
        ))
    }

    fn refresh<'a>(
        &'a self,
        config: &'a ProviderOAuthConfig,
        refresh_token: &'a str,
    ) -> BoxFuture<'a, Result<OAuthTokens>> {
        Box::pin(refresh_google(config, refresh_token))
    }
}

fn require_google_client_secret(config: &ProviderOAuthConfig) -> Result<()> {
    if config.has_client_secret() {
        Ok(())
    } else {
        Err(Error::Auth(crate::google_client_secret_missing_message()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ProviderOAuthConfig;

    #[test]
    fn missing_client_secret_is_rejected_before_browser() {
        let config = ProviderOAuthConfig {
            client_id: "test.apps.googleusercontent.com".into(),
            client_secret: None,
        };
        let err = require_google_client_secret(&config).expect_err("secret required");
        assert!(err.to_string().contains("client_secret"), "{err}");
    }

    #[test]
    fn blank_client_secret_is_rejected() {
        let config = ProviderOAuthConfig {
            client_id: "test.apps.googleusercontent.com".into(),
            client_secret: Some("  ".into()),
        };
        require_google_client_secret(&config).expect_err("blank secret");
    }
}
