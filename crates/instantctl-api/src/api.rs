use std::{fmt, str::FromStr};

use crate::{Error, ErrorKind};

/// Maximum bytes in a raw API request body.
pub const MAX_REQUEST_BYTES: usize = 4 * 1024 * 1024;

/// An HTTP method supported by the raw API command.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Method {
    Get,
    Post,
    Put,
    Delete,
}

impl Method {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Put => "PUT",
            Self::Delete => "DELETE",
        }
    }
}

impl FromStr for Method {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.to_ascii_uppercase().as_str() {
            "GET" => Ok(Self::Get),
            "POST" => Ok(Self::Post),
            "PUT" => Ok(Self::Put),
            "DELETE" => Ok(Self::Delete),
            _ => Err(Error::new(
                ErrorKind::Usage,
                "API method must be GET, POST, PUT, or DELETE",
            )),
        }
    }
}

impl fmt::Display for Method {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A validated raw API request within the fixed portal API origin.
#[derive(Clone, Eq, PartialEq)]
pub struct Request {
    method: Method,
    path: String,
    body: Option<Vec<u8>>,
}

impl Request {
    pub fn new(
        method: Method,
        path: &str,
        query: &[(String, String)],
        body: Option<Vec<u8>>,
    ) -> Result<Self, Error> {
        if url::Url::parse(path).is_ok() {
            return Err(config("API route must be a relative path"));
        }
        let mut path = if path.starts_with('/') {
            path.to_owned()
        } else {
            format!("/{path}")
        };
        crate::client::validate_path(&path)?;
        if !query.is_empty() {
            let encoded = url::form_urlencoded::Serializer::new(String::new())
                .extend_pairs(
                    query
                        .iter()
                        .map(|(key, value)| (key.as_str(), value.as_str())),
                )
                .finish();
            if !encoded.is_empty() {
                path.push(if path.contains('?') { '&' } else { '?' });
                path.push_str(&encoded);
            }
        }
        crate::client::validate_path(&path)?;
        if body
            .as_ref()
            .is_some_and(|body| body.len() > MAX_REQUEST_BYTES)
        {
            return Err(config("API request body exceeded the size limit"));
        }
        Ok(Self { method, path, body })
    }

    pub const fn method(&self) -> Method {
        self.method
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn body(&self) -> Option<&[u8]> {
        self.body.as_deref()
    }
}

impl fmt::Debug for Request {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Request")
            .field("method", &self.method)
            .field("body_bytes", &self.body.as_ref().map(Vec::len))
            .finish_non_exhaustive()
    }
}

fn config(message: &str) -> Error {
    Error::new(ErrorKind::Config, message)
}
