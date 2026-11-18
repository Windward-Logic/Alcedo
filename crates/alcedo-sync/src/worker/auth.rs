//! IMAP/SMTP authentication: passwords, OAuth access tokens and token refresh.

use super::*;

pub(super) async fn connect_imap(
    account: &Account,
    token_cache: &mut Option<String>,
) -> Result<ImapClient> {
    let auth = imap_auth(account, token_cache).await?;
    let opts = imap_connect_options(account, auth);

    match ImapClient::connect(opts).await {
        Ok(client) => Ok(client),
        Err(err) if is_auth_failure(&err) => {
            forget_rejected_token(account, token_cache).await;
            let auth = imap_auth(account, token_cache).await?;
            let opts = imap_connect_options(account, auth);
            ImapClient::connect(opts).await
        }
        Err(err) => Err(err),
    }
}

pub(super) fn imap_connect_options(account: &Account, auth: ImapAuth) -> ImapConnectOptions {
    ImapConnectOptions {
        host: account.imap_host.clone(),
        port: account.imap_port,
        email: account.email.clone(),
        username: match &account.auth {
            AuthMethod::Password => account.login_name().to_string(),
            AuthMethod::OAuth2 { .. } => account.email.clone(),
        },
        auth,
    }
}

pub(super) async fn imap_auth(
    account: &Account,
    token_cache: &mut Option<String>,
) -> Result<ImapAuth> {
    match &account.auth {
        AuthMethod::Password => {
            let password = get_secret(account.id.0, SecretKind::Password)
                .await?
                .ok_or_else(|| Error::Auth("no password stored for account".to_string()))?;
            Ok(ImapAuth::Password(password))
        }
        AuthMethod::OAuth2 { provider } => {
            provider
                .validate_imap_host(&account.imap_host)
                .map_err(|message| Error::Auth(message.to_string()))?;
            // Always ask the shared cache: it refreshes near expiry.
            *token_cache = Some(oauth_access_token(account, provider).await?);
            Ok(ImapAuth::XOAuth2(token_cache.clone().unwrap_or_default()))
        }
    }
}

pub(crate) async fn oauth_access_token(
    account: &Account,
    provider: &OAuthProvider,
) -> Result<String> {
    cached_access_token(account.id.0, TokenPurpose::Mail, || {
        refresh_mail_oauth_tokens(account, provider)
    })
    .await
}

/// After a server rejected `token_cache`, drop it locally and in the shared cache.
pub(super) async fn forget_rejected_token(account: &Account, token_cache: &mut Option<String>) {
    if let Some(rejected) = token_cache.take() {
        invalidate_access_token(account.id.0, TokenPurpose::Mail, &rejected).await;
    }
}

pub(super) async fn refresh_mail_oauth_tokens(
    account: &Account,
    provider: &OAuthProvider,
) -> Result<alcedo_oauth::OAuthTokens> {
    let config = load_oauth_config_cached()?;
    let refresh_token = get_secret(account.id.0, SecretKind::RefreshToken)
        .await?
        .ok_or_else(|| Error::Auth("no refresh token stored for account".to_string()))?;

    let flow = oauth_flow(*provider);
    let client = flow.client_config(&config).map_err(Error::Auth)?;
    let tokens = flow.refresh(&client, &refresh_token).await?;

    if let Some(new_refresh) = tokens.refresh_token.as_deref() {
        store_secret(
            account.id.0,
            &account.email,
            SecretKind::RefreshToken,
            new_refresh,
        )
        .await?;
    }

    Ok(tokens)
}

pub(super) async fn smtp_auth(
    account: &Account,
    token_cache: &mut Option<String>,
) -> Result<SmtpAuth> {
    match &account.auth {
        AuthMethod::Password => {
            let kind = if account.uses_split_credentials() {
                SecretKind::SmtpPassword
            } else {
                SecretKind::Password
            };
            let password = get_secret(account.id.0, kind)
                .await?
                .ok_or_else(|| Error::Auth("no password stored for account".to_string()))?;
            Ok(SmtpAuth::Password(password))
        }
        AuthMethod::OAuth2 { provider } => {
            provider
                .validate_smtp_host(&account.smtp_host)
                .map_err(|message| Error::Auth(message.to_string()))?;
            // Always ask the shared cache: it refreshes near expiry.
            *token_cache = Some(oauth_access_token(account, provider).await?);
            Ok(SmtpAuth::XOAuth2(token_cache.clone().unwrap_or_default()))
        }
    }
}
