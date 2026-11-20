//! Extension point for running message rules on the mail server.
//!
//! With no backend registered every rule runs in Alcedo as mail arrives. A
//! backend can report that the server is ready, publish the rules it can run,
//! and preview a server script Alcedo did not write. Rules the backend accepts
//! ([`ServerRulesBackend::can_publish`]) are marked
//! [`RuleExecution::Server`](alcedo_core::RuleExecution::Server) and skipped by
//! the client-side pass.

use std::future::Future;
use std::pin::Pin;

use alcedo_core::{
    Account, AuthMethod, Error, Folder, MessageRule, Result, ServerRulesPreview, ServerRulesStatus,
    rule_can_run_on_server,
};
use alcedo_oauth::{SecretKind, get_secret};

use crate::worker::oauth_access_token;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// How to sign in to the account's server for rule management.
#[derive(Clone)]
pub enum ServerCredentials {
    Password { username: String, password: String },
    OAuth2 { email: String, access_token: String },
}

impl std::fmt::Debug for ServerCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Password { username, .. } => f
                .debug_struct("Password")
                .field("username", username)
                .finish_non_exhaustive(),
            Self::OAuth2 { email, .. } => f
                .debug_struct("OAuth2")
                .field("email", email)
                .finish_non_exhaustive(),
        }
    }
}

/// Load the account's sign-in for rule management: the stored password, or a
/// fresh OAuth access token from the shared cache (refreshed near expiry).
pub async fn server_credentials(account: &Account) -> Result<ServerCredentials> {
    match &account.auth {
        AuthMethod::Password => {
            let password = get_secret(account.id.0, SecretKind::Password)
                .await?
                .ok_or_else(|| Error::Auth("no password stored for account".to_string()))?;
            Ok(ServerCredentials::Password {
                username: account.login_name().to_string(),
                password,
            })
        }
        AuthMethod::OAuth2 { provider } => Ok(ServerCredentials::OAuth2 {
            email: account.email.clone(),
            access_token: oauth_access_token(account, provider).await?,
        }),
    }
}

/// Runs rules on the account's mail server. Calls for one account never
/// overlap: the account worker holds its rules session while calling.
pub trait ServerRulesBackend: Send + Sync + 'static {
    /// Find out whether the server can take Alcedo's rules.
    fn probe<'a>(&'a self, account: &'a Account) -> BoxFuture<'a, ServerRulesStatus>;

    /// Replace the server's copy of Alcedo's rules with `rules`. Called only
    /// after `probe` returned [`ServerRulesStatus::Ready`].
    fn publish<'a>(
        &'a self,
        account: &'a Account,
        rules: &'a [MessageRule],
        folders: &'a [Folder],
    ) -> BoxFuture<'a, Result<()>>;

    /// Whether `publish` puts `rule` on the server, given the account's
    /// `folders`. The worker asks after each successful publish: a rule this
    /// accepts is marked [`RuleExecution::Server`] and Alcedo no longer runs
    /// it, so answer `true` only for rules `publish` really sends. The default
    /// is [`rule_can_run_on_server`]; a backend with narrower support should
    /// call it and add its own checks.
    ///
    /// [`RuleExecution::Server`]: alcedo_core::RuleExecution::Server
    fn can_publish(&self, rule: &MessageRule, folders: &[Folder]) -> bool {
        rule_can_run_on_server(rule, folders)
    }

    /// Read the active server script named `name` (one Alcedo did not write)
    /// so the rules dialog can show what it does.
    fn preview<'a>(
        &'a self,
        account: &'a Account,
        name: &'a str,
        folders: &'a [Folder],
    ) -> BoxFuture<'a, Option<ServerRulesPreview>>;
}
