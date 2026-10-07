//! RADIUS profile wire contract: Js @4531510, S2 @4528800 in the portal bundle.
use super::{
    Client, Error, NamedMutation, Plan, State, TokenSource, fields, incomplete, limit, name_valid,
    parse, path, safe, select, unique_name, usage,
};
use crate::{ErrorKind, secret::SecretString};
use serde_json::{Value, json};
use std::{fmt, net::Ipv4Addr};

const RESOURCE: &str = "radiusProfiles";
const FIELDS: &[&str] = &[
    "id",
    "name",
    "primaryServer",
    "secondaryServer",
    "enableSecondaryServer",
    "enableRadiusOverTls",
    "requireRadiusAuthentication",
    "enableRadiusAccounting",
    "serverTimeoutSeconds",
    "serverRetryCount",
    "radiusNasIpSettings",
    "radiusNasIdentifierSettings",
];
const SERVER_FIELDS: &[&str] = &[
    "serverHost",
    "sharedSecret",
    "timeout",
    "retryCount",
    "authPort",
    "accountingPort",
];

#[derive(Clone, Debug, Default)]
pub struct ServerPatch {
    pub host: Option<String>,
    pub secret: Option<SecretString>,
    pub timeout: Option<u8>,
    pub retries: Option<u8>,
    pub auth_port: Option<u16>,
    pub accounting_port: Option<u16>,
}
impl ServerPatch {
    pub fn is_empty(&self) -> bool {
        self.host.is_none()
            && self.secret.is_none()
            && self.timeout.is_none()
            && self.retries.is_none()
            && self.auth_port.is_none()
            && self.accounting_port.is_none()
    }
    pub fn validate(&self) -> Result<(), Error> {
        if self.host.as_deref().is_some_and(|host| !valid_host(host)) {
            return Err(usage(
                "RADIUS host must be an IPv4 address or hostname of at most 127 characters",
            ));
        }
        if let Some(secret) = &self.secret {
            validate_secret(secret.expose_secret())?;
        }
        if self.timeout.is_some_and(|value| !(1..=30).contains(&value))
            || self.retries.is_some_and(|value| !(1..=5).contains(&value))
        {
            return Err(usage(
                "RADIUS timeout must be 1 to 30 seconds and retries 1 to 5",
            ));
        }
        if self.auth_port == Some(0) || self.accounting_port == Some(0) {
            return Err(usage("RADIUS ports must be 1 to 65535"));
        }
        Ok(())
    }
    pub(super) fn apply(&self, server: &mut Value, creating: bool) -> Result<(), Error> {
        self.validate()?;
        if self.is_empty() {
            return Ok(());
        }
        if !server.is_object() {
            if self.host.is_none() || self.secret.is_none() {
                return Err(incomplete(
                    "RADIUS server configuration is unavailable; provide host and secret",
                ));
            }
            *server = json!({});
        }
        if creating || self.host.is_some() && self.secret.is_some() {
            // S2.copy/prepareData source defaults for a newly supplied server.
            for (field, default) in [
                ("timeout", 5),
                ("retryCount", 3),
                ("authPort", 1812),
                ("accountingPort", 1813),
            ] {
                if server.get(field).is_none_or(Value::is_null) {
                    server[field] = json!(default);
                }
            }
        }
        if let Some(host) = &self.host {
            server["serverHost"] = json!(host);
        }
        if let Some(secret) = &self.secret {
            server["sharedSecret"] = json!(secret.expose_secret());
        }
        if let Some(value) = self.timeout {
            server["timeout"] = json!(value);
        }
        if let Some(value) = self.retries {
            server["retryCount"] = json!(value);
        }
        if let Some(value) = self.auth_port {
            server["authPort"] = json!(value);
        }
        if let Some(value) = self.accounting_port {
            server["accountingPort"] = json!(value);
        }
        validate_server(server)
    }
}
pub fn validate_secret(secret: &str) -> Result<(), Error> {
    if secret.is_empty()
        || secret.encode_utf16().count() > 64
        || secret.chars().any(char::is_control)
    {
        return Err(usage(
            "RADIUS shared secret must contain 1 to 64 characters without control characters",
        ));
    }
    Ok(())
}
pub(super) fn valid_host(host: &str) -> bool {
    if host.is_empty() || host.len() > 127 || !host.is_ascii() {
        return false;
    }
    if host.parse::<Ipv4Addr>().is_ok() {
        return true;
    }
    host.trim_end_matches('.').split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    })
}
pub(super) fn validate_server(server: &Value) -> Result<(), Error> {
    server
        .get("serverHost")
        .and_then(Value::as_str)
        .filter(|host| valid_host(host))
        .ok_or_else(|| incomplete("RADIUS server host is unavailable"))?;
    let secret = server
        .get("sharedSecret")
        .and_then(Value::as_str)
        .ok_or_else(|| incomplete("RADIUS shared secret is unavailable"))?;
    validate_secret(secret).map_err(|_| incomplete("RADIUS shared secret is unavailable"))?;
    for (field, low, high) in [
        ("timeout", 1, 30),
        ("retryCount", 1, 5),
        ("authPort", 1, 65535),
        ("accountingPort", 1, 65535),
    ] {
        if server
            .get(field)
            .and_then(Value::as_u64)
            .is_none_or(|value| !(low..=high).contains(&value))
        {
            return Err(incomplete(
                "RADIUS server numeric configuration is unavailable",
            ));
        }
    }
    Ok(())
}

#[derive(Clone, Debug, Default)]
pub struct Patch {
    pub name: Option<String>,
    pub primary: ServerPatch,
    pub secondary: ServerPatch,
    pub secondary_enabled: Option<bool>,
    pub tls: Option<bool>,
    pub require_authenticator: Option<bool>,
    pub accounting: Option<bool>,
    pub timeout: Option<u8>,
    pub retries: Option<u8>,
    pub nas_identifier: Option<String>,
    pub nas_ip: Option<Ipv4Addr>,
    pub clear_nas_identifier: bool,
    pub clear_nas_ip: bool,
}
impl Patch {
    pub fn is_empty(&self) -> bool {
        self.name.is_none()
            && self.primary.is_empty()
            && self.secondary.is_empty()
            && self.secondary_enabled.is_none()
            && self.tls.is_none()
            && self.require_authenticator.is_none()
            && self.accounting.is_none()
            && self.timeout.is_none()
            && self.retries.is_none()
            && self.nas_identifier.is_none()
            && self.nas_ip.is_none()
            && !self.clear_nas_identifier
            && !self.clear_nas_ip
    }
    pub fn validate(&self) -> Result<(), Error> {
        if self
            .name
            .as_deref()
            .is_some_and(|name| !name_valid(name, 64))
        {
            return Err(usage(
                "RADIUS profile name must contain 1 to 64 characters without control characters",
            ));
        }
        self.primary.validate()?;
        self.secondary.validate()?;
        if self.timeout.is_some_and(|value| !(1..=30).contains(&value))
            || self.retries.is_some_and(|value| !(1..=5).contains(&value))
        {
            return Err(usage(
                "RADIUS timeout must be 1 to 30 seconds and retries 1 to 5",
            ));
        }
        if self
            .nas_identifier
            .as_deref()
            .is_some_and(|id| !name_valid(id, 64))
        {
            return Err(usage(
                "NAS identifier must contain 1 to 64 characters without control characters",
            ));
        }
        if self.nas_identifier.is_some() && self.clear_nas_identifier
            || self.nas_ip.is_some() && self.clear_nas_ip
        {
            return Err(usage("NAS value and clear option cannot be used together"));
        }
        Ok(())
    }
    pub fn validate_create(&self) -> Result<(), Error> {
        self.validate()?;
        if self.name.is_none() || self.primary.host.is_none() || self.primary.secret.is_none() {
            return Err(usage(
                "RADIUS profile creation requires a name, primary server host, and shared secret",
            ));
        }
        if self.secondary_enabled == Some(true)
            && (self.secondary.host.is_none() || self.secondary.secret.is_none())
        {
            return Err(usage(
                "enabled secondary RADIUS server requires a host and shared secret",
            ));
        }
        Ok(())
    }
    fn paths(&self) -> Vec<String> {
        let mut paths = Vec::new();
        for (present, field) in [
            (self.name.is_some(), "name"),
            (self.secondary_enabled.is_some(), "enableSecondaryServer"),
            (self.tls.is_some(), "enableRadiusOverTls"),
            (
                self.require_authenticator.is_some(),
                "requireRadiusAuthentication",
            ),
            (self.accounting.is_some(), "enableRadiusAccounting"),
            (self.timeout.is_some(), "serverTimeoutSeconds"),
            (self.retries.is_some(), "serverRetryCount"),
            (
                self.nas_identifier.is_some() || self.clear_nas_identifier,
                "radiusNasIdentifierSettings",
            ),
            (
                self.nas_ip.is_some() || self.clear_nas_ip,
                "radiusNasIpSettings",
            ),
        ] {
            if present {
                paths.push(field.to_owned());
            }
        }
        for (server, patch) in [
            ("primaryServer", &self.primary),
            ("secondaryServer", &self.secondary),
        ] {
            if !patch.is_empty() {
                paths.extend(
                    SERVER_FIELDS
                        .iter()
                        .map(|field| format!("{server}/{field}")),
                );
            }
        }
        paths
    }
    fn apply(&self, body: &mut Value, creating: bool) -> Result<(), Error> {
        if let Some(name) = &self.name {
            body["name"] = json!(name);
        }
        for (value, field) in [
            (self.secondary_enabled, "enableSecondaryServer"),
            (self.tls, "enableRadiusOverTls"),
            (self.require_authenticator, "requireRadiusAuthentication"),
            (self.accounting, "enableRadiusAccounting"),
        ] {
            if let Some(value) = value {
                body[field] = json!(value);
            }
        }
        if let Some(value) = self.timeout {
            body["serverTimeoutSeconds"] = json!(value);
        }
        if let Some(value) = self.retries {
            body["serverRetryCount"] = json!(value);
        }
        self.primary.apply(&mut body["primaryServer"], creating)?;
        self.secondary
            .apply(&mut body["secondaryServer"], creating)?;
        if self.nas_identifier.is_some() || self.clear_nas_identifier {
            require_object(body, "radiusNasIdentifierSettings")?;
            body["radiusNasIdentifierSettings"]["useNasIdentifier"] =
                json!(!self.clear_nas_identifier);
            if let Some(id) = &self.nas_identifier {
                body["radiusNasIdentifierSettings"]["nasIdentifier"] = json!(id);
            } else {
                body["radiusNasIdentifierSettings"]
                    .as_object_mut()
                    .unwrap()
                    .remove("nasIdentifier");
            }
        }
        if self.nas_ip.is_some() || self.clear_nas_ip {
            require_object(body, "radiusNasIpSettings")?;
            body["radiusNasIpSettings"]["useNasIpAddress"] = json!(!self.clear_nas_ip);
            if let Some(ip) = self.nas_ip {
                body["radiusNasIpSettings"]["nasIpAddress"] = json!(ip.to_string());
            } else {
                body["radiusNasIpSettings"]
                    .as_object_mut()
                    .unwrap()
                    .remove("nasIpAddress");
            }
        }
        validate_server(&body["primaryServer"])?;
        if body["enableSecondaryServer"] == true {
            validate_server(&body["secondaryServer"])?;
        }
        Ok(())
    }
}
fn require_object(body: &Value, field: &str) -> Result<(), Error> {
    if !body.get(field).is_some_and(Value::is_object) {
        return Err(incomplete("RADIUS NAS configuration is unavailable"));
    }
    Ok(())
}

#[derive(Clone)]
pub struct Profile(Value);
impl fmt::Debug for Profile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.details().fmt(f)
    }
}
impl Profile {
    pub fn summary(&self) -> Value {
        summary(&self.0)
    }
    pub fn details(&self) -> Value {
        safe(self.0.clone())
    }
}
pub async fn list<T: TokenSource>(client: &Client<T>, site: &str) -> Result<Vec<Profile>, Error> {
    Ok(parse(&client.get(&path(site, RESOURCE)?).await?)?
        .into_iter()
        .map(Profile)
        .collect())
}
fn summary(profile: &Value) -> Value {
    safe(
        json!({"id":profile.get("id").and_then(Value::as_str),"name":profile.get("name").and_then(Value::as_str),"primary_host":profile.pointer("/primaryServer/serverHost").and_then(Value::as_str),"secondary_enabled":profile.get("enableSecondaryServer").and_then(Value::as_bool),"tls":profile.get("enableRadiusOverTls").and_then(Value::as_bool),"accounting":profile.get("enableRadiusAccounting").and_then(Value::as_bool),"timeout":profile.get("serverTimeoutSeconds").and_then(Value::as_u64),"retries":profile.get("serverRetryCount").and_then(Value::as_u64),"network_references":profile.get("usedByNetworks").and_then(Value::as_array).map(Vec::len),"device_references":profile.get("usedByDevices").and_then(Value::as_array).map(Vec::len)}),
    )
}
pub fn show(rows: &[Profile], selector: &str) -> Result<Value, Error> {
    let values: Vec<_> = rows.iter().map(|profile| profile.0.clone()).collect();
    Ok(safe(select(&values, selector)?.clone()))
}

pub(super) async fn capabilities<T: TokenSource>(
    client: &Client<T>,
    site: &str,
    patch: &Patch,
) -> Result<(), Error> {
    let hostname = [&patch.primary, &patch.secondary].iter().any(|server| {
        server
            .host
            .as_deref()
            .is_some_and(|host| host.parse::<Ipv4Addr>().is_err())
    });
    let identifier = patch.nas_identifier.is_some();
    let ip = patch.nas_ip.is_some();
    if !hostname && !identifier && !ip {
        return Ok(());
    }
    let payload = client.get(&path(site, "capabilities")?).await?;
    let caps = payload
        .get("capabilities")
        .and_then(Value::as_array)
        .filter(|caps| caps.iter().all(Value::is_string))
        .ok_or_else(|| incomplete("RADIUS capability data is unavailable"))?;
    for (needed, capability) in [
        (hostname, "radius-server-by-hostname"),
        (identifier, "radius-nas-identifier"),
        (ip, "radius-nas-ip-address"),
    ] {
        if needed && !caps.iter().any(|value| value == capability) {
            return Err(Error::new(
                ErrorKind::Unsupported,
                "this site does not support the requested RADIUS configuration",
            ));
        }
    }
    Ok(())
}
pub async fn update<'a, T: TokenSource>(
    client: &'a Client<T>,
    site: &str,
    selector: &str,
    patch: Patch,
) -> Result<(NamedMutation<'a, T>, Plan<State>), Error> {
    patch.validate()?;
    if patch.is_empty() {
        return Err(usage("specify at least one RADIUS profile change"));
    }
    capabilities(client, site, &patch).await?;
    let collection = path(site, RESOURCE)?;
    let rows = parse(&client.get(&collection).await?)?;
    let row = select(&rows, selector)?;
    unique_name(&rows, row["id"].as_str(), patch.name.as_deref())?;
    NamedMutation::update(client, collection, row.clone(), patch.paths(), |body| {
        patch.apply(body, false)
    })
}
pub async fn create<'a, T: TokenSource>(
    client: &'a Client<T>,
    site: &str,
    patch: Patch,
) -> Result<(NamedMutation<'a, T>, Plan<State>), Error> {
    patch.validate_create()?;
    capabilities(client, site, &patch).await?;
    let collection = path(site, RESOURCE)?;
    let payload = client.get(&collection).await?;
    let rows = parse(&payload)?;
    unique_name(&rows, None, patch.name.as_deref())?;
    limit(&payload, "/metaData/maxElements", rows.len())?;
    let template = payload
        .pointer("/metaData/defaultProfile")
        .filter(|value| value.is_object())
        .ok_or_else(|| incomplete("RADIUS creation template is unavailable"))?;
    let mut body = Value::Object(
        FIELDS
            .iter()
            .filter(|field| **field != "id")
            .filter_map(|field| {
                template
                    .get(*field)
                    .map(|value| ((*field).to_owned(), value.clone()))
            })
            .collect(),
    );
    patch.apply(&mut body, true)?;
    for field in [
        "enableSecondaryServer",
        "enableRadiusOverTls",
        "requireRadiusAuthentication",
        "enableRadiusAccounting",
    ] {
        if body.get(field).and_then(Value::as_bool).is_none() {
            return Err(incomplete("RADIUS creation template is incomplete"));
        }
    }
    for (field, max) in [("serverTimeoutSeconds", 30), ("serverRetryCount", 5)] {
        if body
            .get(field)
            .and_then(Value::as_u64)
            .is_none_or(|value| value == 0 || value > max)
        {
            return Err(incomplete(
                "RADIUS creation template numeric settings are unavailable",
            ));
        }
    }
    let mut paths = fields(&[
        "name",
        "enableSecondaryServer",
        "enableRadiusOverTls",
        "requireRadiusAuthentication",
        "enableRadiusAccounting",
        "serverTimeoutSeconds",
        "serverRetryCount",
    ]);
    paths.extend(patch.paths());
    paths.extend(
        SERVER_FIELDS
            .iter()
            .map(|field| format!("primaryServer/{field}")),
    );
    if body["enableSecondaryServer"] == true {
        paths.extend(
            SERVER_FIELDS
                .iter()
                .map(|field| format!("secondaryServer/{field}")),
        );
    }
    paths.sort_unstable();
    paths.dedup();
    NamedMutation::create(client, collection, &rows, body, paths)
}
pub async fn delete<'a, T: TokenSource>(
    client: &'a Client<T>,
    site: &str,
    selector: &str,
) -> Result<(NamedMutation<'a, T>, Plan<State>), Error> {
    let collection = path(site, RESOURCE)?;
    let rows = parse(&client.get(&collection).await?)?;
    let row = select(&rows, selector)?;
    let mut references = Vec::new();
    for field in ["usedByNetworks", "usedByDevices"] {
        let values = row
            .get(field)
            .and_then(Value::as_array)
            .ok_or_else(|| incomplete("RADIUS profile references are unavailable"))?;
        references.extend(
            values
                .iter()
                .map(|value| value.get("name").and_then(Value::as_str).map(str::to_owned))
                .collect::<Option<Vec<_>>>()
                .ok_or_else(|| incomplete("RADIUS profile reference names are unavailable"))?,
        );
    }
    if !references.is_empty() {
        return Err(usage(
            "RADIUS profile is in use; remove its network and device assignments before deleting",
        ));
    }
    NamedMutation::delete(client, collection, row, json!(references))
}
