//! Microsoft OAuth (P5).

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use oauth2::basic::{BasicTokenResponse, BasicTokenType};
use oauth2::{CsrfToken, ExtraTokenFields, Scope, StandardTokenResponse};
use serde::{Deserialize, Serialize};

use alcedo_core::{Error, OAuthTokens, Result};

use crate::flow::{BoxFuture, OAuthFlow, OpenAuthUrl, missing_client_message};
use crate::loopback::LoopbackRedirect;
use crate::pkce::new_pkce_pair;
use crate::{OAuthConfig, ProviderOAuthConfig, build_oauth_client, http_client};

const MICROSOFT_AUTH_URL: &str = "https://login.microsoftonline.com/common/oauth2/v2.0/authorize";
const MICROSOFT_TOKEN_URL: &str = "https://login.microsoftonline.com/common/oauth2/v2.0/token";

pub const MICROSOFT_SCOPES: [&str; 5] = [
    "openid",
    "profile",
    "offline_access",
    "https://outlook.office.com/IMAP.AccessAsUser.All",
    "https://outlook.office.com/SMTP.Send",
];

/// The part of Microsoft's token response that the standard fields leave out.
#[derive(Clone, Debug, Deserialize, Serialize)]
struct MicrosoftTokenFields {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    id_token: Option<String>,
}

impl ExtraTokenFields for MicrosoftTokenFields {}

type MicrosoftTokenResponse = StandardTokenResponse<MicrosoftTokenFields, BasicTokenType>;

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
    let oauth_client = build_oauth_client::<MicrosoftTokenResponse>(
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

    let sign_in_name = token
        .extra_fields()
        .id_token
        .as_deref()
        .and_then(sign_in_name_from_id_token);
    Ok(OAuthTokens {
        sign_in_name,
        ..crate::tokens_from_response(token)
    })
}

/// Read the account's sign-in name from an ID token.
///
/// The signature is not checked because the token came straight from Microsoft's
/// token endpoint over TLS (OpenID Connect Core 3.1.3.7), and the name only
/// selects the SMTP login that the access token must still satisfy.
fn sign_in_name_from_id_token(id_token: &str) -> Option<String> {
    let payload = id_token.split('.').nth(1)?;
    let claims: serde_json::Value =
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload).ok()?).ok()?;
    claims
        .get("preferred_username")?
        .as_str()
        .map(str::trim)
        .filter(|name| name.contains('@'))
        .map(str::to_string)
}

/// Refresh a Microsoft access token using a refresh token.
pub async fn refresh_microsoft(
    config: &ProviderOAuthConfig,
    refresh_token: &str,
) -> Result<OAuthTokens> {
    let redirect_url = oauth2::RedirectUrl::new("http://127.0.0.1".to_string())
        .map_err(|e| Error::Auth(format!("microsoft redirect URL invalid: {e}")))?;
    let oauth_client = build_oauth_client::<BasicTokenResponse>(
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

/// Microsoft sign-in for mail accounts.
pub(crate) struct MicrosoftFlow;

impl OAuthFlow for MicrosoftFlow {
    fn client_config(
        &self,
        config: &OAuthConfig,
    ) -> std::result::Result<ProviderOAuthConfig, String> {
        config
            .microsoft
            .clone()
            .ok_or_else(|| missing_client_message("Microsoft"))
    }

    fn sign_in<'a>(
        &'a self,
        config: &'a ProviderOAuthConfig,
        open_auth_url: OpenAuthUrl,
    ) -> BoxFuture<'a, Result<OAuthTokens>> {
        Box::pin(microsoft_auth_flow(
            config,
            &MICROSOFT_SCOPES,
            open_auth_url,
        ))
    }

    fn refresh<'a>(
        &'a self,
        config: &'a ProviderOAuthConfig,
        refresh_token: &'a str,
    ) -> BoxFuture<'a, Result<OAuthTokens>> {
        Box::pin(refresh_microsoft(config, refresh_token))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id_token_with_claims(claims: &str) -> String {
        format!("header.{}.signature", URL_SAFE_NO_PAD.encode(claims))
    }

    #[test]
    fn reads_the_sign_in_name_from_an_id_token() {
        let token = id_token_with_claims(r#"{"preferred_username":"primary@example.com"}"#);
        assert_eq!(
            sign_in_name_from_id_token(&token).as_deref(),
            Some("primary@example.com")
        );
    }

    #[test]
    fn ignores_an_id_token_without_a_usable_sign_in_name() {
        for claims in [
            r#"{"name":"No Username"}"#,
            r#"{"preferred_username":"not-an-address"}"#,
            r#"{"preferred_username":7}"#,
            "not json",
        ] {
            assert_eq!(
                sign_in_name_from_id_token(&id_token_with_claims(claims)),
                None,
                "{claims}"
            );
        }
    }

    #[test]
    fn ignores_a_malformed_id_token() {
        assert_eq!(sign_in_name_from_id_token("no-dots"), None);
        assert_eq!(sign_in_name_from_id_token("a.%%%.c"), None);
    }

    #[test]
    fn token_response_keeps_the_id_token() {
        let response: MicrosoftTokenResponse = serde_json::from_str(
            r#"{"access_token":"a","token_type":"Bearer","id_token":"h.p.s"}"#,
        )
        .expect("token response");
        assert_eq!(response.extra_fields().id_token.as_deref(), Some("h.p.s"));
    }

    #[test]
    fn token_response_without_an_id_token_still_parses() {
        let response: MicrosoftTokenResponse =
            serde_json::from_str(r#"{"access_token":"a","token_type":"Bearer"}"#)
                .expect("token response");
        assert_eq!(response.extra_fields().id_token, None);
    }
}
