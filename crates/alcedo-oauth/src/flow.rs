//! Browser sign-in and token refresh behind one interface per provider.

use std::future::Future;
use std::pin::Pin;

use alcedo_core::{OAuthProvider, OAuthTokens, Result};

use crate::google::GoogleFlow;
use crate::microsoft::MicrosoftFlow;
use crate::{OAuthConfig, ProviderOAuthConfig};

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Opens the provider's authorization page in the user's browser.
pub type OpenAuthUrl = Box<dyn FnOnce(String) + Send>;

/// How one provider signs a mail account in and renews its tokens.
pub trait OAuthFlow: Send + Sync {
    /// This provider's client from `config`, or a message saying what to configure.
    fn client_config(
        &self,
        config: &OAuthConfig,
    ) -> std::result::Result<ProviderOAuthConfig, String>;

    /// Run the browser sign-in for mail access.
    fn sign_in<'a>(
        &'a self,
        config: &'a ProviderOAuthConfig,
        open_auth_url: OpenAuthUrl,
    ) -> BoxFuture<'a, Result<OAuthTokens>>;

    /// Exchange a refresh token for new tokens.
    fn refresh<'a>(
        &'a self,
        config: &'a ProviderOAuthConfig,
        refresh_token: &'a str,
    ) -> BoxFuture<'a, Result<OAuthTokens>>;
}

/// The sign-in flow for `provider`.
pub fn oauth_flow(provider: OAuthProvider) -> &'static dyn OAuthFlow {
    match provider {
        OAuthProvider::Google => &GoogleFlow,
        OAuthProvider::Microsoft => &MicrosoftFlow,
    }
}

/// Message for a provider with no client configured.
pub(crate) fn missing_client_message(provider_name: &str) -> String {
    format!(
        "Add {provider_name} client_id to {}",
        crate::oauth_config_path_display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client(secret: Option<&str>) -> ProviderOAuthConfig {
        ProviderOAuthConfig {
            client_id: "id".into(),
            client_secret: secret.map(str::to_string),
        }
    }

    #[test]
    fn missing_clients_say_which_provider_to_configure() {
        let config = OAuthConfig::default();
        let google = oauth_flow(OAuthProvider::Google).client_config(&config);
        let microsoft = oauth_flow(OAuthProvider::Microsoft).client_config(&config);
        assert!(google.expect_err("google").contains("Add Google client_id"));
        assert!(
            microsoft
                .expect_err("microsoft")
                .contains("Add Microsoft client_id")
        );
    }

    #[test]
    fn google_client_requires_a_secret() {
        let config = OAuthConfig {
            google: Some(client(None)),
            microsoft: None,
        };
        let message = oauth_flow(OAuthProvider::Google)
            .client_config(&config)
            .expect_err("secret required");
        assert!(message.contains("client_secret"), "{message}");
    }

    #[test]
    fn microsoft_client_needs_no_secret() {
        let config = OAuthConfig {
            google: None,
            microsoft: Some(client(None)),
        };
        let found = oauth_flow(OAuthProvider::Microsoft)
            .client_config(&config)
            .expect("public client");
        assert_eq!(found.client_id, "id");
    }
}
