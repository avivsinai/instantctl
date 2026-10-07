//! The portal's one public country route; it never needs account credentials.
use std::{collections::HashSet, time::Duration};

use reqwest::Method;
use serde::Serialize;
use serde_json::Value;
use url::Url;

use super::{BASE_URL, Client, build_http, read_json, send_request};
use crate::{Error, ErrorKind, StaticToken, TokenSource};

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Country {
    pub country_code: String,
    pub supported_country_codes: Option<Vec<String>>,
}

/// Read current country metadata without loading a token or saved profile.
pub async fn read(timeout: Duration) -> Result<Country, Error> {
    let base = Url::parse(BASE_URL).map_err(|_| incomplete("invalid portal origin"))?;
    fetch(&build_http(timeout)?, &base).await
}

impl<T: TokenSource> Client<T> {
    pub async fn country(&self) -> Result<Country, Error> {
        fetch(&self.http, &self.base).await
    }
}

async fn fetch(http: &reqwest::Client, base: &Url) -> Result<Country, Error> {
    let mut url = base.clone();
    url.set_path("/public/country");
    url.set_query(None);
    url.set_fragment(None);
    let response = send_request::<StaticToken>(http, None, Method::GET, url, None).await?;
    parse(&read_json(response).await?)
}

pub(crate) fn valid_code(code: &str) -> bool {
    code.len() == 2 && code.bytes().all(|byte| byte.is_ascii_uppercase())
}

fn parse(payload: &Value) -> Result<Country, Error> {
    let country_code = payload
        .get("countryCode")
        .and_then(Value::as_str)
        .filter(|code| valid_code(code))
        .ok_or_else(|| incomplete("country response has no valid country code"))?
        .to_owned();
    let supported_country_codes = match payload.get("supportedCountryCodes") {
        None | Some(Value::Null) => None,
        Some(Value::Array(values)) => {
            let mut seen = HashSet::new();
            let mut codes = Vec::with_capacity(values.len());
            for value in values {
                let code = value
                    .as_str()
                    .filter(|code| valid_code(code))
                    .ok_or_else(|| incomplete("supported country code is invalid"))?;
                if !seen.insert(code) {
                    return Err(incomplete("supported country codes contain duplicates"));
                }
                codes.push(code.to_owned());
            }
            Some(codes)
        }
        _ => return Err(incomplete("supported country codes are invalid")),
    };
    Ok(Country {
        country_code,
        supported_country_codes,
    })
}

fn incomplete(message: &str) -> Error {
    Error::new(ErrorKind::Unverified, message)
}
