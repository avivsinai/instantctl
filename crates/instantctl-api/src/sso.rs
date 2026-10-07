//! Portal credential login and rotating OAuth tokens. All redirects are inspected locally.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use reqwest::{StatusCode, header};
use serde::{Deserialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use tokio::sync::OnceCell;
use url::Url;

use crate::{Error, ErrorKind, secret::SecretString};

const SETTINGS_URL: &str = "https://portal.instant-on.hpe.com/settings.json";
const SSO_ORIGIN: &str = "https://sso.arubainstanton.com";
const PORTAL_ORIGIN: &str = "https://portal.instant-on.hpe.com";
const MAX_RESPONSE_BYTES: usize = 64 * 1024;
const MAX_TOKEN_BYTES: usize = 16_384;

#[derive(Clone, Debug)]
pub struct Tokens {
    pub access: SecretString,
    pub refresh: SecretString,
    pub access_expiry: SystemTime,
}

/// Provider text and transport errors are never retained in this error.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum SsoError {
    #[error("an authenticator OTP is required; enter a current code")]
    OtpRequired,
    #[error("the username or password is invalid")]
    InvalidCredentials,
    #[error("the account is locked or disabled")]
    AccountLocked,
    #[error("the refresh token is invalid or revoked; run instantctl auth login")]
    RefreshRejected,
    #[error("SSO authorization was refused; run instantctl auth login")]
    AuthorizationRejected,
    #[error("{0}")]
    Config(&'static str),
    #[error("{0}")]
    Transport(&'static str),
}

impl SsoError {
    pub fn kind(self) -> ErrorKind {
        match self {
            Self::Config(_) => ErrorKind::Config,
            Self::Transport(_) => ErrorKind::General,
            _ => ErrorKind::Auth,
        }
    }
}

impl From<SsoError> for Error {
    fn from(error: SsoError) -> Self {
        Self::new(error.kind(), error.to_string())
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Settings {
    sso_fqdn: String,
    #[serde(rename = "ssoClientIdAuthZ")]
    client_id: String,
    sso_redirect_url: String,
}

struct RuntimeConfig {
    sso: Url,
    redirect: Url,
    redirect_uri: String,
    client_id: String,
}

/// Production entry points always fetch settings from the fixed portal origin.
pub struct SsoClient {
    http: reqwest::Client,
    settings_url: Url,
    sso_origin: Url,
    portal_origin: Url,
    config: OnceCell<RuntimeConfig>,
}

impl std::fmt::Debug for SsoClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SsoClient").finish_non_exhaustive()
    }
}

impl SsoClient {
    pub fn new(timeout: Duration) -> Result<Self, SsoError> {
        Self::build(
            timeout,
            parse_url(SETTINGS_URL)?,
            parse_url(SSO_ORIGIN)?,
            parse_url(PORTAL_ORIGIN)?,
        )
    }

    // Private so loopback tests cannot loosen the public origin policy.
    fn build(
        timeout: Duration,
        settings_url: Url,
        sso_origin: Url,
        portal_origin: Url,
    ) -> Result<Self, SsoError> {
        if timeout.is_zero() {
            return Err(SsoError::Config("SSO timeout must be greater than zero"));
        }
        let http = reqwest::Client::builder()
            .timeout(timeout)
            .connect_timeout(timeout)
            .read_timeout(timeout)
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .http1_only()
            .user_agent(concat!("instantctl/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|_| SsoError::Transport("could not initialize SSO transport"))?;
        Ok(Self {
            http,
            settings_url,
            sso_origin,
            portal_origin,
            config: OnceCell::new(),
        })
    }

    async fn config(&self) -> Result<&RuntimeConfig, SsoError> {
        self.config
            .get_or_try_init(|| async {
                let response = self.send(self.http.get(self.settings_url.clone())).await?;
                if !response.status().is_success() {
                    return Err(SsoError::Config("could not read portal SSO settings"));
                }
                let settings: Settings = read_json(response).await?;
                let sso = parse_url(&settings.sso_fqdn)?;
                let redirect = parse_url(&settings.sso_redirect_url)?;
                validate_root(&sso, &self.sso_origin)?;
                validate_root(&redirect, &self.portal_origin)?;
                if settings.client_id.is_empty()
                    || settings.client_id.len() > 256
                    || !settings.client_id.bytes().all(|b| (33..=126).contains(&b))
                {
                    return Err(SsoError::Config("invalid SSO client ID in portal settings"));
                }
                Ok(RuntimeConfig {
                    sso,
                    redirect,
                    redirect_uri: settings.sso_redirect_url,
                    client_id: settings.client_id,
                })
            })
            .await
    }

    pub async fn login(
        &self,
        username: &str,
        password: &SecretString,
        otp: Option<&SecretString>,
    ) -> Result<Tokens, SsoError> {
        if username.is_empty() || password.expose_secret().is_empty() {
            return Err(SsoError::Config("username and password must not be empty"));
        }
        let config = self.config().await?;
        let mut form = vec![
            ("username", username),
            ("password", password.expose_secret()),
        ];
        if let Some(otp) = otp {
            form.push(("otp", otp.expose_secret()));
        }
        let response = self
            .send(
                self.http
                    .post(endpoint(config, "/aio/api/v1/mfa/validate/full"))
                    .form(&form),
            )
            .await?;
        let session: SessionResponse = parse_oauth(response, Operation::Login).await?;
        validate_token(&session.access_token)?;
        let verifier = random_secret(32)?;
        let state = random_secret(24)?;
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.expose_secret().as_bytes()));
        let mut authorize = endpoint(config, "/as/authorization.oauth2");
        authorize.query_pairs_mut().extend_pairs([
            ("client_id", config.client_id.as_str()),
            ("redirect_uri", config.redirect_uri.as_str()),
            ("response_type", "code"),
            ("scope", "profile openid"),
            ("state", state.expose_secret()),
            ("code_challenge_method", "S256"),
            ("code_challenge", challenge.as_str()),
            ("sessionToken", session.access_token.as_str()),
        ]);
        let response = self.send(self.http.get(authorize)).await?;
        if response.status() != StatusCode::FOUND {
            return Err(SsoError::AuthorizationRejected);
        }
        let location = response
            .headers()
            .get(header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .ok_or(SsoError::Config("SSO authorization omitted the redirect"))?;
        let redirect = parse_url(location)?;
        if redirect.origin() != config.redirect.origin()
            || redirect.path() != config.redirect.path()
            || !redirect.username().is_empty()
            || redirect.password().is_some()
            || redirect.fragment().is_some()
        {
            return Err(SsoError::Config(
                "SSO authorization returned an unsafe redirect",
            ));
        }
        let returned_state = unique_parameter(&redirect, "state")?;
        if returned_state != state.expose_secret() {
            return Err(SsoError::Config("SSO authorization state did not match"));
        }
        let code = unique_parameter(&redirect, "code")?;
        self.exchange(
            config,
            &[
                ("grant_type", "authorization_code"),
                ("client_id", &config.client_id),
                ("redirect_uri", config.redirect_uri.as_str()),
                ("code", &code),
                ("code_verifier", verifier.expose_secret()),
            ],
            Operation::Authorize,
        )
        .await
    }

    pub async fn refresh(&self, refresh: &SecretString) -> Result<Tokens, SsoError> {
        validate_token(refresh.expose_secret())?;
        let config = self.config().await?;
        let tokens = self
            .exchange(
                config,
                &[
                    ("grant_type", "refresh_token"),
                    ("client_id", &config.client_id),
                    ("refresh_token", refresh.expose_secret()),
                ],
                Operation::Refresh,
            )
            .await?;
        if tokens.refresh.expose_secret() == refresh.expose_secret() {
            return Err(SsoError::Config("SSO did not rotate the refresh token"));
        }
        Ok(tokens)
    }

    pub async fn revoke(&self, refresh: &SecretString) -> Result<(), SsoError> {
        validate_token(refresh.expose_secret())?;
        let config = self.config().await?;
        let response = self
            .send(
                self.http
                    .post(endpoint(config, "/as/revoke_token.oauth2"))
                    .form(&[
                        ("client_id", config.client_id.as_str()),
                        ("token", refresh.expose_secret()),
                    ]),
            )
            .await?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(SsoError::RefreshRejected)
        }
    }

    async fn exchange(
        &self,
        config: &RuntimeConfig,
        form: &[(&str, &str)],
        operation: Operation,
    ) -> Result<Tokens, SsoError> {
        let response = self
            .send(
                self.http
                    .post(endpoint(config, "/as/token.oauth2"))
                    .form(form),
            )
            .await?;
        let tokens: TokenResponse = parse_oauth(response, operation).await?;
        validate_token(&tokens.access_token)?;
        validate_token(&tokens.refresh_token)?;
        let access_expiry = jwt_expiry(&tokens.access_token)?;
        Ok(Tokens {
            access: SecretString::new(tokens.access_token),
            refresh: SecretString::new(tokens.refresh_token),
            access_expiry,
        })
    }

    async fn send(&self, request: reqwest::RequestBuilder) -> Result<reqwest::Response, SsoError> {
        request
            .header(header::ACCEPT, "application/json")
            .send()
            .await
            .map_err(|error| {
                if error.is_timeout() {
                    SsoError::Transport("SSO request timed out")
                } else {
                    SsoError::Transport("SSO request failed during transport")
                }
            })
    }

    #[cfg(test)]
    pub(crate) fn for_test(base: Url) -> Self {
        Self::build(
            Duration::from_secs(2),
            base.join("settings.json").unwrap(),
            base.clone(),
            base,
        )
        .unwrap()
    }
}

#[derive(Deserialize)]
struct SessionResponse {
    access_token: String,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: String,
}

#[derive(Deserialize)]
struct ProviderError {
    error: Option<String>,
    error_description: Option<String>,
}

#[derive(Clone, Copy)]
enum Operation {
    Login,
    Authorize,
    Refresh,
}

async fn parse_oauth<T: DeserializeOwned>(
    response: reqwest::Response,
    operation: Operation,
) -> Result<T, SsoError> {
    if response.status().is_success() {
        return read_json(response).await;
    }
    let status = response.status();
    if status.is_redirection() {
        return Err(SsoError::Config("SSO returned an unexpected redirect"));
    }
    if status.is_server_error() || matches!(status.as_u16(), 408 | 429) {
        return Err(SsoError::Transport("SSO is unavailable; try again later"));
    }
    if matches!(operation, Operation::Refresh) {
        return Err(SsoError::RefreshRejected);
    }
    let provider: ProviderError = read_json(response).await?;
    let description = provider
        .error_description
        .unwrap_or_default()
        .to_ascii_lowercase();
    match operation {
        Operation::Refresh => Err(SsoError::RefreshRejected),
        Operation::Authorize => Err(SsoError::AuthorizationRejected),
        Operation::Login if description.contains("locked") || description.contains("disabled") => {
            Err(SsoError::AccountLocked)
        }
        Operation::Login
            if provider.error.as_deref() == Some("invalid_grant")
                && description.contains("otp") =>
        {
            Err(SsoError::OtpRequired)
        }
        Operation::Login => Err(SsoError::InvalidCredentials),
    }
}

async fn read_json<T: DeserializeOwned>(mut response: reqwest::Response) -> Result<T, SsoError> {
    if !response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| {
            v.split(';')
                .next()
                .unwrap_or_default()
                .trim()
                .eq_ignore_ascii_case("application/json")
        })
    {
        return Err(SsoError::Config("SSO returned a non-JSON response"));
    }
    if response
        .content_length()
        .is_some_and(|len| len > MAX_RESPONSE_BYTES as u64)
    {
        return Err(SsoError::Config("SSO response exceeded the size limit"));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| SsoError::Transport("SSO response failed during transport"))?
    {
        if chunk.len() > MAX_RESPONSE_BYTES - bytes.len() {
            return Err(SsoError::Config("SSO response exceeded the size limit"));
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| SsoError::Config("SSO returned invalid JSON"))
}

fn parse_url(value: &str) -> Result<Url, SsoError> {
    Url::parse(value).map_err(|_| SsoError::Config("invalid URL in SSO configuration"))
}

fn validate_root(url: &Url, expected: &Url) -> Result<(), SsoError> {
    if url.origin() != expected.origin()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(SsoError::Config(
            "portal SSO settings contain an unexpected origin or path",
        ));
    }
    Ok(())
}

fn endpoint(config: &RuntimeConfig, path: &str) -> Url {
    let mut url = config.sso.clone();
    url.set_path(path);
    url
}

fn unique_parameter(url: &Url, key: &str) -> Result<String, SsoError> {
    let mut values = url
        .query_pairs()
        .filter(|(name, _)| name == key)
        .map(|(_, value)| value.into_owned());
    let value = values
        .next()
        .filter(|value| !value.is_empty())
        .ok_or(SsoError::Config("SSO authorization omitted code or state"))?;
    if values.next().is_some() || value.len() > MAX_TOKEN_BYTES {
        return Err(SsoError::Config(
            "SSO authorization returned invalid code or state",
        ));
    }
    Ok(value)
}

fn random_secret(bytes: usize) -> Result<SecretString, SsoError> {
    let mut random = vec![0; bytes];
    getrandom::fill(&mut random)
        .map_err(|_| SsoError::Config("could not generate secure PKCE randomness"))?;
    Ok(SecretString::new(URL_SAFE_NO_PAD.encode(random)))
}

fn validate_token(token: &str) -> Result<(), SsoError> {
    if token.is_empty()
        || token.len() > MAX_TOKEN_BYTES
        || !token.bytes().all(|b| (33..=126).contains(&b))
    {
        return Err(SsoError::Config("SSO returned an invalid token"));
    }
    Ok(())
}

fn jwt_expiry(token: &str) -> Result<SystemTime, SsoError> {
    #[derive(Deserialize)]
    struct Claims {
        exp: u64,
    }
    let invalid = || SsoError::Config("SSO returned an invalid access-token expiry");
    let mut parts = token.split('.');
    parts
        .next()
        .filter(|part| !part.is_empty())
        .ok_or_else(invalid)?;
    let payload = parts
        .next()
        .filter(|part| !part.is_empty())
        .ok_or_else(invalid)?;
    parts
        .next()
        .filter(|part| !part.is_empty())
        .ok_or_else(invalid)?;
    if parts.next().is_some() {
        return Err(invalid());
    }
    let decoded = URL_SAFE_NO_PAD.decode(payload).map_err(|_| invalid())?;
    let claims: Claims = serde_json::from_slice(&decoded).map_err(|_| invalid())?;
    let expiry = UNIX_EPOCH
        .checked_add(Duration::from_secs(claims.exp))
        .ok_or_else(invalid)?;
    if expiry <= SystemTime::now() {
        return Err(invalid());
    }
    Ok(expiry)
}

#[cfg(test)]
mod tests;
