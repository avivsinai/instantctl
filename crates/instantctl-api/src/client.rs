use std::{fmt, time::Duration};

use reqwest::{
    Method, StatusCode,
    header::{self, HeaderValue},
};
use serde_json::{Value, json};
use url::Url;

use crate::{
    Error, ErrorKind, StaticToken, TokenSource,
    api::{Method as ApiMethod, Request as ApiRequest},
};

pub mod access;
pub mod administration;
pub mod allowlist;
pub mod country;
pub mod firmware;
pub mod monitoring;
pub mod network;
pub mod network_routing;
pub mod operations;
pub mod policies;
pub mod port_diagnostics;
pub mod port_settings;
pub mod profile;
pub mod radio;
pub mod reads;
pub mod replace;
pub mod site_actions;
pub mod site_lifecycle;
pub mod stacks;
pub mod wlan;

pub const BASE_URL: &str = "https://portal.instant-on.hpe.com/api";
pub const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
const MAX_ATTEMPTS: usize = 3;
const MAX_RETRY_DELAY: f64 = 2.0;

/// One fixed-origin transport. Token sources can refresh independently of it.
pub struct Client<T = StaticToken> {
    http: reqwest::Client,
    source: T,
    base: Url,
    protected_ports: Vec<crate::ports::ProtectedPort>,
}

impl<T> fmt::Debug for Client<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Client")
            .field("base", &self.base)
            .finish_non_exhaustive()
    }
}

impl<T: TokenSource> Client<T> {
    pub fn new(source: T, timeout: Duration) -> Result<Self, Error> {
        let base = Url::parse(BASE_URL).map_err(|_| config("invalid portal API origin"))?;
        Self::build(source, timeout, base)
    }

    /// Protect selected switch faceplate ports in addition to reported uplinks and LAG members.
    pub fn with_protected_ports(mut self, entries: &[String]) -> Result<Self, Error> {
        self.protected_ports = crate::ports::parse_protected_ports(entries)?;
        Ok(self)
    }

    pub(crate) fn protected_ports(&self) -> &[crate::ports::ProtectedPort] {
        &self.protected_ports
    }

    // The public constructor always supplies the fixed portal origin.
    fn build(source: T, timeout: Duration, base: Url) -> Result<Self, Error> {
        let http = build_http(timeout)?;
        Ok(Self {
            http,
            source,
            base,
            protected_ports: Vec::new(),
        })
    }

    fn url(&self, path: &str) -> Result<Url, Error> {
        validate_path(path)?;
        let url = Url::parse(&format!(
            "{}{path}",
            self.base.as_str().trim_end_matches('/')
        ))
        .map_err(|_| config("invalid API route"))?;
        let prefix = self.base.path().trim_end_matches('/');
        if url.origin() != self.base.origin()
            || !(url.path() == prefix || url.path().starts_with(&format!("{prefix}/")))
        {
            return Err(config("API route must remain within the portal API"));
        }
        Ok(url)
    }

    fn resource_url(
        &self,
        path: &str,
        segments: &[&str],
        action: Option<&str>,
    ) -> Result<Url, Error> {
        let mut url = self.url(path)?;
        if url.query().is_some() {
            return Err(config("resource route cannot contain query parameters"));
        }
        {
            let mut parts = url
                .path_segments_mut()
                .map_err(|_| config("invalid resource route"))?;
            parts.pop_if_empty();
            for segment in segments {
                if segment.is_empty()
                    || matches!(*segment, "." | "..")
                    || segment
                        .bytes()
                        .any(|byte| byte.is_ascii_control() || matches!(byte, b'/' | b'\\'))
                {
                    return Err(config("invalid resource identifier"));
                }
                parts.push(segment);
            }
        }
        if let Some(action) = action {
            if action.is_empty() || action.bytes().any(|byte| byte.is_ascii_control()) {
                return Err(config("invalid action name"));
            }
            url.query_pairs_mut().append_pair("action", action);
        }
        Ok(url)
    }

    pub async fn get(&self, path: &str) -> Result<Value, Error> {
        self.request(Method::GET, self.url(path)?, None).await
    }

    pub async fn put_full(&self, path: &str, entity: &Value) -> Result<Value, Error> {
        self.request(Method::PUT, self.url(path)?, Some(entity))
            .await
    }

    pub async fn create(&self, path: &str, entity: &Value) -> Result<Value, Error> {
        self.request(Method::POST, self.url(path)?, Some(entity))
            .await
    }

    pub async fn delete(&self, path: &str) -> Result<Value, Error> {
        self.request(Method::DELETE, self.url(path)?, None).await
    }

    pub async fn action(
        &self,
        path: &str,
        id: &str,
        action: &str,
        body: &Value,
    ) -> Result<Value, Error> {
        self.request(
            Method::POST,
            self.resource_url(path, &[id], Some(action))?,
            Some(body),
        )
        .await
    }

    pub async fn batch_create(&self, path: &str, entities: &[Value]) -> Result<Value, Error> {
        self.request(
            Method::POST,
            self.resource_url(path, &["batchCreate"], None)?,
            Some(&json!({"elements": entities})),
        )
        .await
    }

    pub async fn batch_update(&self, path: &str, entities: &[Value]) -> Result<Value, Error> {
        self.request(
            Method::POST,
            self.resource_url(path, &["batchUpdate"], None)?,
            Some(&json!({"elements": entities})),
        )
        .await
    }

    pub async fn batch_delete(&self, path: &str, ids: &[String]) -> Result<Value, Error> {
        self.request(
            Method::POST,
            self.resource_url(path, &["batchDelete"], None)?,
            Some(&json!({"ids": ids})),
        )
        .await
    }

    pub async fn batch_action(
        &self,
        path: &str,
        action: &str,
        ids: &[String],
        body: &Value,
    ) -> Result<Value, Error> {
        let mut body = body
            .as_object()
            .cloned()
            .ok_or_else(|| config("batch action body must be an object"))?;
        body.insert("ids".into(), json!(ids));
        self.request(
            Method::POST,
            self.resource_url(path, &["batchExecute"], Some(action))?,
            Some(&Value::Object(body)),
        )
        .await
    }

    /// Send a validated raw API request and return JSON or a lossy text body.
    pub async fn raw_api(&self, request: &ApiRequest) -> Result<Value, Error> {
        let method = match request.method() {
            ApiMethod::Get => Method::GET,
            ApiMethod::Post => Method::POST,
            ApiMethod::Put => Method::PUT,
            ApiMethod::Delete => Method::DELETE,
        };
        let response = self
            .send(method, self.url(request.path())?, request.body())
            .await?;
        read_raw_response(response).await
    }

    async fn request(
        &self,
        method: Method,
        url: Url,
        body: Option<&Value>,
    ) -> Result<Value, Error> {
        let body = body
            .map(serde_json::to_vec)
            .transpose()
            .map_err(|_| general("could not encode portal request"))?;
        let response = self.send(method, url, body.as_deref()).await?;
        read_json(response).await
    }

    async fn send(
        &self,
        method: Method,
        url: Url,
        body: Option<&[u8]>,
    ) -> Result<reqwest::Response, Error> {
        send_request(&self.http, Some(&self.source), method, url, body).await
    }
}

fn build_http(timeout: Duration) -> Result<reqwest::Client, Error> {
    if timeout.is_zero() {
        return Err(config("request timeout must be greater than zero"));
    }
    reqwest::Client::builder()
        .timeout(timeout)
        .connect_timeout(timeout)
        .read_timeout(timeout)
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .http1_only()
        .user_agent(concat!("instantctl/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|_| general("could not initialize portal transport"))
}

async fn send_request<T: TokenSource>(
    http: &reqwest::Client,
    source: Option<&T>,
    method: Method,
    url: Url,
    body: Option<&[u8]>,
) -> Result<reqwest::Response, Error> {
    let attempts = if method == Method::GET {
        MAX_ATTEMPTS
    } else {
        1
    };
    for attempt in 0..attempts {
        let mut request = http
            .request(method.clone(), url.clone())
            .header(header::ACCEPT, "application/json")
            .header("X-ION-API-VERSION", "28")
            .header("X-ION-CLIENT-PLATFORM", "web")
            .header("X-ION-CLIENT-TYPE", "InstantOn");
        if let Some(source) = source {
            let token = source.token().await?;
            let mut authorization =
                HeaderValue::from_str(&format!("Bearer {}", token.expose_secret()))
                    .map_err(|_| config("the session token is invalid"))?;
            authorization.set_sensitive(true);
            request = request.header(header::AUTHORIZATION, authorization);
        }
        if let Some(body) = body {
            request = request
                .header(header::CONTENT_TYPE, "application/json")
                .body(body.to_vec());
        }
        let response = request.send().await.map_err(|error| {
            if error.is_timeout() {
                general("portal request timed out")
            } else {
                general("portal request failed during transport")
            }
        })?;
        let status = response.status();
        if method == Method::GET && matches!(status.as_u16(), 429 | 503) && attempt + 1 < attempts {
            let delay = retry_delay(
                response
                    .headers()
                    .get(header::RETRY_AFTER)
                    .and_then(|value| value.to_str().ok()),
            );
            drop(response);
            tokio::time::sleep(delay).await;
            continue;
        }
        if !status.is_success() {
            return Err(http_error(status));
        }
        return Ok(response);
    }
    Err(general("portal request failed"))
}

/// Require a path within the API; reject parser normalization and encoded escapes.
pub(crate) fn validate_path(path: &str) -> Result<(), Error> {
    if !path.starts_with('/')
        || path.starts_with("//")
        || path.contains('#')
        || path
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte == b'\\')
    {
        return Err(config("invalid API route"));
    }
    for segment in path.split('?').next().unwrap_or_default().split('/') {
        let bytes = segment.as_bytes();
        let mut decoded = Vec::with_capacity(bytes.len());
        let mut index = 0;
        while index < bytes.len() {
            if bytes[index] == b'%' {
                let byte = bytes
                    .get(index + 1..index + 3)
                    .and_then(|hex| {
                        Some((hex[0] as char).to_digit(16)? * 16 + (hex[1] as char).to_digit(16)?)
                    })
                    .ok_or_else(|| config("invalid API route"))?;
                decoded.push(byte as u8);
                index += 3;
            } else {
                decoded.push(bytes[index]);
                index += 1;
            }
        }
        if matches!(decoded.as_slice(), b"." | b"..")
            || decoded
                .iter()
                .any(|byte| byte.is_ascii_control() || matches!(*byte, b'/' | b'\\' | b'%'))
        {
            return Err(config("invalid API route"));
        }
    }
    Ok(())
}

fn retry_delay(value: Option<&str>) -> Duration {
    let seconds = value
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|seconds| seconds.is_finite())
        .unwrap_or(0.25)
        .clamp(0.0, MAX_RETRY_DELAY);
    Duration::from_secs_f64(seconds)
}

fn http_error(status: StatusCode) -> Error {
    let kind = match status.as_u16() {
        401 | 403 => ErrorKind::Auth,
        404 => ErrorKind::NotFound,
        408 | 429 => ErrorKind::RetryLater,
        400..=499 => ErrorKind::ClientError,
        _ => ErrorKind::General,
    };
    Error::new(
        kind,
        format!("portal request failed with HTTP {}", status.as_u16()),
    )
}

async fn read_json(response: reqwest::Response) -> Result<Value, Error> {
    if response.status() == StatusCode::NO_CONTENT {
        return Ok(Value::Null);
    }
    if !response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .split(';')
                .next()
                .unwrap_or_default()
                .trim()
                .eq_ignore_ascii_case("application/json")
        })
    {
        return Err(general("portal returned a non-JSON response"));
    }
    let bytes = read_bytes(response).await?;
    serde_json::from_slice(&bytes).map_err(|_| general("portal returned invalid JSON"))
}

async fn read_raw_response(response: reqwest::Response) -> Result<Value, Error> {
    if response.status() == StatusCode::NO_CONTENT {
        return Ok(Value::Null);
    }
    let bytes = read_bytes(response).await?;
    Ok(serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned())))
}

async fn read_bytes(mut response: reqwest::Response) -> Result<Vec<u8>, Error> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
    {
        return Err(general("portal response exceeded the size limit"));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| general("portal response failed during transport"))?
    {
        if chunk.len() > MAX_RESPONSE_BYTES - bytes.len() {
            return Err(general("portal response exceeded the size limit"));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn config(message: &str) -> Error {
    Error::new(ErrorKind::Config, message)
}
fn general(message: &str) -> Error {
    Error::new(ErrorKind::General, message)
}

#[cfg(test)]
mod tests;
