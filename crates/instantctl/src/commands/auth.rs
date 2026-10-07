use clap::{Args as ClapArgs, Subcommand};
use instantctl_api::{
    Client, CredentialStore, Error, ErrorKind, KeychainStore, ProfileLock, RefreshingTokenSource,
    SsoClient, SsoError, StaticToken, StoredCredential,
};
use std::{sync::Arc, time::UNIX_EPOCH};

use super::auth_input::{self, LoginArgs};
use crate::{
    context::{CommandContext, CommandResult},
    credentials,
    exit::ExitStatus,
};

#[derive(Debug, ClapArgs)]
pub struct Args {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Sign in to the portal.
    Login(LoginArgs),
    /// Show authentication status.
    Status,
    /// Remove stored credentials.
    Logout,
}

pub async fn run(args: Args, context: &CommandContext) -> anyhow::Result<CommandResult> {
    match args.command {
        Command::Login(args) => login(args, context).await,
        Command::Status => status(context),
        Command::Logout => logout(context).await,
    }
}

async fn login(args: LoginArgs, context: &CommandContext) -> anyhow::Result<CommandResult> {
    if !cfg!(target_os = "macos") {
        return Err(Error::new(ErrorKind::Config,
            "saved login requires macOS Keychain; use HPE_INSTANT_ON_TOKEN or --token-stdin on this platform").into());
    }
    auth_input::validate_input(
        &args,
        matches!(context.token_source, credentials::CredentialSource::Stdin),
    )?;
    let store = Arc::new(KeychainStore::new()?);
    let previous = store.load(&context.profile)?;
    let username = auth_input::collect_username(
        &args,
        previous.as_ref().map(|entry| entry.username.as_str()),
    )?;
    let password = auth_input::password(&args)?;
    let sso = SsoClient::new(context.timeout).map_err(Error::from)?;
    let tokens = match sso.login(&username, &password, None).await {
        Ok(tokens) => tokens,
        Err(SsoError::OtpRequired) => {
            let otp = auth_input::otp(&args)?;
            sso.login(&username, &password, Some(&otp))
                .await
                .map_err(Error::from)?
        }
        Err(error) => return Err(Error::from(error).into()),
    };
    drop(password);
    let access = StaticToken::new(tokens.access.expose_secret())?;
    // Persist the issued refresh token before discovery or an interactive site choice.
    RefreshingTokenSource::with_login(sso, store.clone(), &context.profile, &username, tokens)
        .await?;
    let sites = Client::new(access, context.timeout)?.sites().await?;
    let site = auth_input::choose_site(&sites, context.site.as_deref())?;
    let _guard = ProfileLock::for_profile(&context.profile)?
        .acquire()
        .await?;
    // A concurrent refresh may have rotated tokens while the user chose a site.
    // Reload under the lock and change only metadata; never replay the login tokens.
    let mut credential = store
        .load(&context.profile)?
        .ok_or_else(credentials::no_login)?;
    if credential.username != username {
        return Err(Error::new(
            ErrorKind::Auth,
            "the profile login changed during site selection; sign in again",
        )
        .into());
    }
    credential.default_site = Some(site);
    store.save(&context.profile, &credential)?;
    Ok(CommandResult::success(status_data(
        &context.profile,
        &credential,
    )))
}

fn status(context: &CommandContext) -> anyhow::Result<CommandResult> {
    if !cfg!(target_os = "macos") {
        return Err(credentials::no_login().into());
    }
    let credential = KeychainStore::new()?
        .load(&context.profile)?
        .ok_or_else(credentials::no_login)?;
    Ok(CommandResult {
        data: status_data(&context.profile, &credential),
        status: if credential.refresh_pending {
            ExitStatus::Error(ErrorKind::Auth)
        } else {
            ExitStatus::Success
        },
    })
}

fn status_data(profile: &str, credential: &StoredCredential) -> serde_json::Value {
    serde_json::json!({
        "profile": profile,
        "username": credential.username,
        "default_site": credential.default_site,
        "access_expiry": credential.access_expiry.duration_since(UNIX_EPOCH).ok().map(|duration| duration.as_secs()),
        "refresh_pending": credential.refresh_pending,
    })
}

async fn logout(context: &CommandContext) -> anyhow::Result<CommandResult> {
    if !cfg!(target_os = "macos") {
        return Ok(logout_result(&context.profile, None));
    }
    let _guard = ProfileLock::for_profile(&context.profile)?
        .acquire()
        .await?;
    let store = KeychainStore::new()?;
    // Revocation is best effort. A failed read or request must not prevent deletion.
    let revoked = match store.load(&context.profile) {
        Ok(Some(credential)) => Some(match SsoClient::new(context.timeout) {
            Ok(sso) => sso.revoke(&credential.refresh).await.is_ok(),
            Err(_) => false,
        }),
        Ok(None) => None,
        Err(_) => Some(false),
    };
    store.delete(&context.profile)?;
    // Drop releases the OS lock. Keep its inode stable for waiters and future logins.
    Ok(logout_result(&context.profile, revoked))
}

fn logout_result(profile: &str, revoked: Option<bool>) -> CommandResult {
    CommandResult::success(serde_json::json!({
        "profile": profile,
        "logged_out": true,
        "refresh_revoked": revoked,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use instantctl_api::{Tokens, secret::SecretString};
    use std::time::Duration;

    #[test]
    fn status_reports_metadata_without_token_material() {
        let mut credential = StoredCredential::new(
            "user@example.test",
            Tokens {
                access: SecretString::new("access-secret-sentinel"),
                refresh: SecretString::new("refresh-secret-sentinel"),
                access_expiry: UNIX_EPOCH + Duration::from_secs(1234),
            },
        );
        credential.default_site = Some("11111111-2222-3333-4444-555555555555".into());
        credential.refresh_pending = true;
        let data = status_data("work", &credential);
        assert_eq!(
            data,
            serde_json::json!({
                "profile": "work", "username": "user@example.test",
                "default_site": "11111111-2222-3333-4444-555555555555",
                "access_expiry": 1234, "refresh_pending": true,
            })
        );
        let encoded = data.to_string();
        for secret in ["access-secret-sentinel", "refresh-secret-sentinel"] {
            assert!(!encoded.contains(secret));
        }
    }
}
