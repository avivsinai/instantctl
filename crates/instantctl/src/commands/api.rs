use std::{
    fs::File,
    io::{self, Read},
};

use clap::Args as ClapArgs;
use instantctl_api::{
    Error, ErrorKind,
    api::{MAX_REQUEST_BYTES, Method, Request},
};
use serde_json::{Map, Value, json};

use crate::{
    context::{CommandContext, CommandResult},
    credentials::CredentialSource,
    exit::ExitStatus,
    mutation::Options,
    output::{redact_secrets, sensitive_key},
};

#[derive(Debug, ClapArgs)]
pub struct Args {
    /// API route relative to the fixed portal API origin.
    pub path: String,
    /// HTTP method (default: POST with fields, GET otherwise).
    #[arg(short = 'X', long)]
    pub method: Option<Method>,
    /// String field in key=value form; may be repeated.
    #[arg(short = 'f', long = "raw-field")]
    pub raw_fields: Vec<String>,
    /// Scalar field in key=value form; @file or @- reads a string.
    #[arg(short = 'F', long = "field")]
    pub fields: Vec<String>,
    /// Request body file, or - for stdin. Fields become query parameters.
    #[arg(long)]
    pub input: Option<String>,
    /// Send a non-GET request once, without claiming state readback.
    #[arg(long)]
    pub apply: bool,
    /// Confirm the request without an interactive prompt.
    #[arg(long)]
    pub yes: bool,
    /// Allow secret or opaque request bodies and response fields in output.
    #[arg(long)]
    pub show_secrets: bool,
}

impl Args {
    fn prepare(&self, context: &CommandContext) -> Result<Request, Error> {
        let method =
            self.method
                .unwrap_or(if self.raw_fields.is_empty() && self.fields.is_empty() {
                    Method::Get
                } else {
                    Method::Post
                });
        Request::new(method, &self.path, &[], None)?;
        let stdin_inputs = usize::from(self.input.as_deref() == Some("-"))
            + self
                .fields
                .iter()
                .filter(|field| {
                    field
                        .split_once('=')
                        .is_some_and(|(_, value)| matches!(value, "@-" | "-"))
                })
                .count();
        if stdin_inputs > 1
            || (stdin_inputs > 0 && matches!(context.token_source, CredentialSource::Stdin))
        {
            return Err(config(
                "stdin can supply one request input and cannot also supply --token-stdin",
            ));
        }
        if method == Method::Get && (self.apply || self.yes) {
            return Err(usage(
                "--apply and --yes are only valid for non-GET requests",
            ));
        }
        if method != Method::Get {
            Options {
                apply: self.apply,
                yes: self.yes,
            }
            .preflight(context.token_source)?;
            if stdin_inputs > 0 && self.apply && !self.yes {
                return Err(Error::new(
                    ErrorKind::ConfirmationRequired,
                    "request input from stdin requires --yes when applying a request",
                ));
            }
        }
        let mut fields = Map::new();
        let mut field_bytes = 0;
        for field in &self.raw_fields {
            let (key, value) = split_field(field)?;
            account_field(&mut field_bytes, key, value.len())?;
            fields.insert(key.to_owned(), Value::String(value.to_owned()));
        }
        for field in &self.fields {
            let (key, value) = split_field(field)?;
            let value = if let Some(file) = value.strip_prefix('@') {
                Value::String(read_string(file)?)
            } else if value == "-" {
                Value::String(read_string("-")?)
            } else {
                scalar(value)
            };
            let length = value
                .as_str()
                .map_or_else(|| value.to_string().len(), str::len);
            account_field(&mut field_bytes, key, length)?;
            fields.insert(key.to_owned(), value);
        }
        let (query, body) = if self.input.is_some() || method == Method::Get {
            let query = fields
                .iter()
                .map(|(key, value)| {
                    (
                        key.clone(),
                        match value {
                            Value::String(value) => value.clone(),
                            other => other.to_string(),
                        },
                    )
                })
                .collect::<Vec<_>>();
            let body = self.input.as_deref().map(read_input).transpose()?;
            (query, body)
        } else {
            let body = if fields.is_empty() {
                None
            } else {
                Some(
                    serde_json::to_vec(&fields)
                        .map_err(|_| config("request fields could not be encoded"))?,
                )
            };
            (Vec::new(), body)
        };
        Request::new(method, &self.path, &query, body)
    }
}

pub async fn run(args: Args, context: &CommandContext) -> anyhow::Result<CommandResult> {
    let request = args.prepare(context)?;
    if request.method() != Method::Get {
        let mut plan = json!({
            "method": request.method().as_str(),
            "path": display_path(request.path(), args.show_secrets),
            "body": display_body(request.body(), args.show_secrets),
            "request_attempted": false,
        });
        crate::output::write_data(&mut io::stderr().lock(), context.format, &plan)?;
        if !args.apply {
            return Ok(CommandResult::success(plan));
        }
        crate::mutation::confirm(args.yes)?;
        let response = context.client()?.raw_api(&request).await?;
        plan["request_attempted"] = json!(true);
        plan["outcome"] = json!("sent");
        plan["response"] = display_response(response, args.show_secrets, &request);
        return Ok(CommandResult::success(plan));
    }
    let data = context.client()?.raw_api(&request).await?;
    let result = read_result(data, args.show_secrets, &request);
    if result.status == ExitStatus::Incomplete {
        crate::output::write_data(
            &mut io::stderr().lock(),
            context.format,
            &json!({"kind":"incomplete", "message":"API collection is incomplete; no confirmed pagination contract is available"}),
        )?;
    }
    Ok(result)
}

fn read_result(data: Value, show_secrets: bool, request: &Request) -> CommandResult {
    let incomplete = collection_is_incomplete(&data);
    let mut data = display_response(data, show_secrets, request);
    if incomplete {
        data.as_object_mut()
            .expect("collection metadata is an object")
            .insert("complete".into(), Value::Bool(false));
    }
    CommandResult {
        data,
        status: if incomplete {
            ExitStatus::Incomplete
        } else {
            ExitStatus::Success
        },
    }
}

fn account_field(bytes: &mut usize, key: &str, value_bytes: usize) -> Result<(), Error> {
    *bytes = bytes
        .checked_add(key.len())
        .and_then(|bytes| bytes.checked_add(value_bytes))
        .filter(|bytes| *bytes <= MAX_REQUEST_BYTES)
        .ok_or_else(|| config("API request fields exceeded the size limit"))?;
    Ok(())
}

fn split_field(field: &str) -> Result<(&str, &str), Error> {
    field
        .split_once('=')
        .filter(|(key, _)| !key.is_empty())
        .ok_or_else(|| usage("API fields must use a nonempty key=value form"))
}

fn scalar(value: &str) -> Value {
    if let Ok(number) = value.parse::<i64>() {
        return json!(number);
    }
    if let Ok(number) = value.parse::<u64>() {
        return json!(number);
    }
    if let Ok(number) = value.parse::<f64>()
        && let Some(number) = serde_json::Number::from_f64(number)
    {
        return Value::Number(number);
    }
    match value {
        "true" => Value::Bool(true),
        "false" => Value::Bool(false),
        "null" => Value::Null,
        _ => Value::String(value.to_owned()),
    }
}

fn read_input(path: &str) -> Result<Vec<u8>, Error> {
    let mut input: Box<dyn Read> = if path == "-" {
        Box::new(io::stdin())
    } else {
        Box::new(File::open(path).map_err(|_| config("request input could not be opened"))?)
    };
    let mut bytes = Vec::new();
    input
        .by_ref()
        .take(MAX_REQUEST_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| config("request input could not be read"))?;
    if bytes.len() > MAX_REQUEST_BYTES {
        return Err(config("API request input exceeded the size limit"));
    }
    Ok(bytes)
}

fn read_string(path: &str) -> Result<String, Error> {
    String::from_utf8(read_input(path)?).map_err(|_| config("API field input must be UTF-8"))
}

fn contains_secret(value: &Value) -> bool {
    match value {
        Value::Object(object) => object
            .iter()
            .any(|(key, child)| sensitive_key(key) || contains_secret(child)),
        Value::Array(values) => values.iter().any(contains_secret),
        _ => false,
    }
}

fn display_body(body: Option<&[u8]>, show_secrets: bool) -> Value {
    let Some(body) = body else {
        return Value::Null;
    };
    match serde_json::from_slice::<Value>(body) {
        Ok(value) if show_secrets || !contains_secret(&value) => value,
        _ if show_secrets => Value::String(String::from_utf8_lossy(body).into_owned()),
        _ => json!("<redacted>"),
    }
}

fn sensitive_request(request: &Request) -> bool {
    request.body().is_some_and(|body| {
        serde_json::from_slice::<Value>(body).map_or(true, |body| contains_secret(&body))
    }) || request.path().split_once('?').is_some_and(|(_, query)| {
        url::form_urlencoded::parse(query.as_bytes()).any(|(key, _)| sensitive_key(&key))
    })
}

fn display_response(mut value: Value, show_secrets: bool, request: &Request) -> Value {
    if !show_secrets {
        if value.is_string() && sensitive_request(request) {
            return json!("<redacted>");
        }
        redact_secrets(&mut value);
    }
    value
}

fn display_path(path: &str, show_secrets: bool) -> String {
    let Some((path, query)) = path.split_once('?') else {
        return path.to_owned();
    };
    let fields = url::form_urlencoded::parse(query.as_bytes()).map(|(key, value)| {
        let value = if !show_secrets && sensitive_key(&key) {
            "<redacted>".into()
        } else {
            value
        };
        (key, value)
    });
    format!(
        "{path}?{}",
        url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(fields)
            .finish()
    )
}

fn collection_is_incomplete(value: &Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    if let Some(elements) = object.get("elements").and_then(Value::as_array)
        && object.iter().any(|(key, count)| {
            (key.eq_ignore_ascii_case("total") || key.to_ascii_lowercase().ends_with("count"))
                && count
                    .as_u64()
                    .is_some_and(|count| count > elements.len() as u64)
        })
    {
        return true;
    }
    object
        .iter()
        .filter(|(key, _)| key.as_str() != "elements")
        .any(|(key, value)| pagination_marker(key, value))
}

fn pagination_marker(key: &str, value: &Value) -> bool {
    let key = key.to_ascii_lowercase().replace(['_', '-', '.'], "");
    let present = match value {
        Value::Null | Value::Bool(false) => false,
        Value::String(value) => !value.is_empty(),
        Value::Number(value) => value.as_u64() != Some(0) && value.as_i64() != Some(0),
        Value::Array(values) => !values.is_empty(),
        Value::Object(values) => !values.is_empty(),
        Value::Bool(true) => true,
    };
    if present
        && matches!(
            key.as_str(),
            "next"
                | "nextpage"
                | "nextcursor"
                | "cursor"
                | "pagecursor"
                | "page"
                | "token"
                | "pagetoken"
                | "nextpagetoken"
                | "hasmore"
                | "istruncated"
                | "partial"
        )
    {
        return true;
    }
    match value {
        Value::Object(object) => object
            .iter()
            .any(|(key, value)| pagination_marker(key, value)),
        Value::Array(values) => values.iter().any(|value| pagination_marker("", value)),
        _ => false,
    }
}

fn config(message: &'static str) -> Error {
    Error::new(ErrorKind::Config, message)
}
fn usage(message: &'static str) -> Error {
    Error::new(ErrorKind::Usage, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_get_preserves_partial_data_but_exits_incomplete() {
        let request = Request::new(Method::Get, "/sites", &[], None).unwrap();
        for (response, incomplete) in [
            (
                json!({"elements":[{"id":"ap","future":null}],"totalCount":2}),
                true,
            ),
            (json!({"elements":[1],"count":2}), true),
            (json!({"elements":[1],"total":2}), true),
            (
                json!({"elements":[1],"metaData":{"pageCursor":"continue"}}),
                true,
            ),
            (json!({"elements":[1],"nextPageToken":"continue"}), true),
            (json!({"elements":[1],"metaData":{"hasMore":true}}), true),
            (
                json!({"elements":[1],"metadata":{"cursor":"continue"}}),
                true,
            ),
            (
                json!({"elements":[1],"metaData":{"links":[{"nextCursor":"continue"}]}}),
                true,
            ),
            (
                json!({"elements":[{"token":"row-value"}],"totalCount":1,"metaData":{"nextPage":null}}),
                false,
            ),
            (
                json!({"elements":[1],"count":1,"maxElements":100,"nextPage":""}),
                false,
            ),
            (
                json!({"elements":[{"id":"a"}],"metadata":{"pageSize":1}}),
                false,
            ),
            (json!({"elements":[1],"accessToken":"auth-value"}), false),
            (
                json!({"elements":[1],"metadata":{"currentPage":1,"offset":0,"limit":20}}),
                false,
            ),
            (json!([1, 2]), false),
        ] {
            let result = read_result(response.clone(), true, &request);
            assert_eq!(result.status.code(), if incomplete { 4 } else { 0 });
            if incomplete {
                assert_eq!(result.data["complete"], false);
                let mut data = result.data;
                data.as_object_mut().unwrap().remove("complete");
                assert_eq!(
                    data, response,
                    "partial data must remain available to the caller"
                );
            } else {
                assert_eq!(result.data, response);
            }
        }
    }

    #[test]
    fn responses_hide_secret_fields_and_sensitive_or_opaque_input_echoes() {
        for (method, body) in [
            (Method::Get, None),
            (Method::Post, Some(br#"{"safe":true}"#.to_vec())),
        ] {
            let request =
                Request::new(method, "/sites?passphrase=query-echo-secret", &[], body).unwrap();
            let response = json!("query-echo-secret");
            assert_eq!(
                read_result(response.clone(), false, &request).data,
                "<redacted>"
            );
            assert_eq!(read_result(response.clone(), true, &request).data, response);
        }
        let request = Request::new(Method::Get, "/sites", &[], None).unwrap();
        let response = json!({"nested":[{"preSharedKey":"response-key-secret","passphrase":"response-phrase-secret","label":"safe"}]});
        let hidden = read_result(response.clone(), false, &request);
        assert_eq!(
            hidden.data,
            json!({"nested":[{"preSharedKey":"<redacted>","passphrase":"<redacted>","label":"safe"}]})
        );
        assert_eq!(read_result(response.clone(), true, &request).data, response);
        for body in [
            br#"{"nested":{"PSK":"sentinel-response-secret"}}"#.as_slice(),
            b"sentinel-response-secret".as_slice(),
        ] {
            let request = Request::new(Method::Post, "/sites", &[], Some(body.to_vec())).unwrap();
            let response = json!("sentinel-response-secret");
            let hidden = read_result(response.clone(), false, &request);
            assert!(!hidden.data.to_string().contains("sentinel-response-secret"));
            assert_eq!(hidden.status, ExitStatus::Success);
            let shown = read_result(response.clone(), true, &request);
            assert_eq!(shown.data, response);
        }
    }
}
