use std::{
    ffi::OsString,
    io::{self, Read},
    sync::Arc,
    time::Duration,
};

use instantctl_api::{
    Error, ErrorKind, KeychainStore, ProfileMetadata, RefreshingTokenSource, StaticToken,
    TokenSource, secret::SecretString,
};

#[cfg(target_os = "macos")]
use instantctl_api::SsoClient;

const TOKEN_ENV: &str = "HPE_INSTANT_ON_TOKEN";
const MAX_TOKEN_BYTES: usize = 16_384;
const MAX_STDIN_BYTES: usize = MAX_TOKEN_BYTES + 3;

#[derive(Clone, Copy, Debug)]
pub enum CredentialSource {
    Environment,
    Stdin,
}

#[derive(Clone)]
pub enum CommandTokenSource {
    Static(StaticToken),
    Profile(Arc<RefreshingTokenSource<KeychainStore>>),
}

impl CommandTokenSource {
    pub(crate) async fn profile_metadata(&self) -> Result<ProfileMetadata, Error> {
        match self {
            Self::Static(_) => Ok(ProfileMetadata::default()),
            Self::Profile(source) => source.profile_metadata().await,
        }
    }
}

impl TokenSource for CommandTokenSource {
    async fn token(&self) -> Result<SecretString, Error> {
        match self {
            Self::Static(source) => source.token().await,
            Self::Profile(source) => source.token().await,
        }
    }
}

impl CredentialSource {
    pub(crate) fn uses_profile(&self) -> bool {
        matches!(self, Self::Environment) && std::env::var_os(TOKEN_ENV).is_none()
    }

    pub fn load(&self, profile: &str, timeout: Duration) -> Result<CommandTokenSource, Error> {
        match self {
            Self::Stdin => static_token(token_from_stdin()?).map(CommandTokenSource::Static),
            Self::Environment => match token_from_environment(std::env::var_os(TOKEN_ENV))? {
                Some(token) => Ok(CommandTokenSource::Static(token)),
                None => profile_source(profile, timeout),
            },
        }
    }
}

fn static_token(token: String) -> Result<StaticToken, Error> {
    StaticToken::new(token)
        .map_err(|_| Error::new(ErrorKind::Config, "the session token is invalid"))
}

fn token_from_environment(value: Option<OsString>) -> Result<Option<StaticToken>, Error> {
    let Some(value) = value else { return Ok(None) };
    let token = value
        .into_string()
        .map_err(|_| Error::new(ErrorKind::Config, "the session token must be valid UTF-8"))?;
    if token.is_empty() {
        return Err(missing_token_error());
    }
    if token.len() > MAX_TOKEN_BYTES {
        return Err(token_size_error());
    }
    static_token(token).map(Some)
}

#[cfg(target_os = "macos")]
fn profile_source(profile: &str, timeout: Duration) -> Result<CommandTokenSource, Error> {
    let store = KeychainStore::new()?;
    let sso = SsoClient::new(timeout).map_err(Error::from)?;
    RefreshingTokenSource::new(sso, store, profile)
        .map(|source| CommandTokenSource::Profile(Arc::new(source)))
}

#[cfg(not(target_os = "macos"))]
fn profile_source(_profile: &str, _timeout: Duration) -> Result<CommandTokenSource, Error> {
    Err(no_login())
}

pub(crate) fn no_login() -> Error {
    Error::new(
        ErrorKind::Auth,
        "no saved login; use auth login on macOS, HPE_INSTANT_ON_TOKEN, or --token-stdin",
    )
}

fn token_from_stdin() -> Result<String, Error> {
    let mut bytes = Vec::new();
    io::stdin()
        .take(MAX_STDIN_BYTES as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| {
            Error::new(
                ErrorKind::Config,
                "failed to read the session token from stdin",
            )
        })?;

    if bytes.len() == MAX_STDIN_BYTES {
        return Err(token_size_error());
    }
    let bytes = bytes
        .strip_suffix(b"\r\n")
        .or_else(|| bytes.strip_suffix(b"\n"))
        .unwrap_or(&bytes);
    if bytes.len() > MAX_TOKEN_BYTES {
        return Err(token_size_error());
    }
    String::from_utf8(bytes.to_vec())
        .map_err(|_| Error::new(ErrorKind::Config, "the session token must be valid UTF-8"))
}

fn missing_token_error() -> Error {
    Error::new(
        ErrorKind::Config,
        format!("provide a session token with {TOKEN_ENV} or --token-stdin"),
    )
}

fn token_size_error() -> Error {
    Error::new(
        ErrorKind::Config,
        "session token input exceeded the 16384-byte limit",
    )
}
