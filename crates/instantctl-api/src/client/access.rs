//! Portal access settings and named schedules. Full-object updates and one-write
//! creation/deletion use the same mutation engine as network and WLAN commands.
pub mod guest_portal;
pub mod port_access_control;
pub mod radius;
pub mod schedule;

use serde::{Serialize, Serializer};
use serde_json::{Value, json};
use std::{collections::HashSet, fmt, sync::Mutex};

use super::{Client, reads};
use crate::{
    Error, ErrorKind, TokenSource,
    mutation::{FullObjectPut, Mutation, ObjectResource, Plan},
};

#[derive(Clone, PartialEq)]
pub struct State {
    exists: bool,
    configuration: Value,
}
impl State {
    fn printable(&self) -> Value {
        let mut configuration = self.configuration.clone();
        redact(&mut configuration);
        json!({"exists":self.exists,"configuration":configuration})
    }
}
impl fmt::Debug for State {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.printable().fmt(f)
    }
}
impl Serialize for State {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.printable().serialize(serializer)
    }
}

pub(super) fn redact(value: &mut Value) {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                let key = key.to_ascii_lowercase();
                if ["secret", "password", "presharedkey", "privatekey", "token"]
                    .iter()
                    .any(|part| key.contains(part))
                {
                    if !value.is_null() {
                        *value = json!("(redacted)");
                    }
                } else {
                    redact(value);
                }
            }
        }
        Value::Array(array) => array.iter_mut().for_each(redact),
        _ => {}
    }
}
pub(super) fn safe(mut value: Value) -> Value {
    redact(&mut value);
    value
}
fn masked_secret(value: &Value) -> bool {
    value.as_str().is_some_and(|secret| {
        !secret.is_empty()
            && (secret.chars().all(|ch| matches!(ch, '*' | '•'))
                || matches!(secret, "(redacted)" | "[redacted]" | "<redacted>"))
    })
}

// A display placeholder is not evidence of the underlying credential. Never
// forward it in a full-object write, or use it to verify a requested secret.
fn require_unmasked_secrets(body: &Value) -> Result<(), Error> {
    match body {
        Value::Object(fields) => {
            for (key, value) in fields {
                if key.eq_ignore_ascii_case("sharedSecret") && masked_secret(value) {
                    return Err(incomplete(
                        "RADIUS shared secret is masked or ambiguous; provide an unmasked secret",
                    ));
                }
                require_unmasked_secrets(value)?;
            }
        }
        Value::Array(values) => {
            for value in values {
                require_unmasked_secrets(value)?;
            }
        }
        _ => {}
    }
    Ok(())
}
fn normalize_secret_readback(body: &mut Value) {
    match body {
        Value::Object(fields) => {
            for (key, value) in fields {
                if key.eq_ignore_ascii_case("sharedSecret") && masked_secret(value) {
                    *value = Value::Null;
                } else {
                    normalize_secret_readback(value);
                }
            }
        }
        Value::Array(values) => values.iter_mut().for_each(normalize_secret_readback),
        _ => {}
    }
}
pub(super) fn usage(message: &str) -> Error {
    Error::new(ErrorKind::Usage, message)
}
pub(super) fn incomplete(message: &str) -> Error {
    Error::new(ErrorKind::Unverified, message)
}
pub(super) fn path(site: &str, resource: &str) -> Result<String, Error> {
    if !reads::valid_site_id(site) {
        return Err(Error::new(ErrorKind::Config, "--site must be a UUID"));
    }
    Ok(format!("/sites/{site}/{resource}"))
}
pub(super) fn name_valid(name: &str, max: usize) -> bool {
    !name.trim().is_empty()
        && name.encode_utf16().count() <= max
        && !name.chars().any(char::is_control)
}
pub(super) fn parse(payload: &Value) -> Result<Vec<Value>, Error> {
    let rows: Vec<Value> = reads::parse_elements(payload)?;
    let mut ids = HashSet::new();
    for row in &rows {
        let id = row
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .ok_or_else(|| incomplete("resource identity is missing"))?;
        if !ids.insert(id) {
            return Err(incomplete("resource identities are duplicated"));
        }
    }
    Ok(rows)
}
pub(super) fn select<'a>(rows: &'a [Value], selector: &str) -> Result<&'a Value, Error> {
    if let Some(row) = rows.iter().find(|row| row["id"].as_str() == Some(selector)) {
        return Ok(row);
    }
    let mut matches = rows
        .iter()
        .filter(|row| row["name"].as_str() == Some(selector));
    let row = matches.next().ok_or_else(|| {
        Error::new(
            ErrorKind::NotFound,
            "no resource matches the supplied selector",
        )
    })?;
    if matches.next().is_some() {
        return Err(usage("resource name is ambiguous; select by ID"));
    }
    Ok(row)
}
pub(super) fn unique_name(
    rows: &[Value],
    id: Option<&str>,
    name: Option<&str>,
) -> Result<(), Error> {
    if let Some(name) = name
        && rows
            .iter()
            .any(|row| row["id"].as_str() != id && row["name"].as_str() == Some(name))
    {
        return Err(usage("resource name already exists"));
    }
    Ok(())
}
pub(super) fn limit(payload: &Value, pointer: &str, count: usize) -> Result<(), Error> {
    if let Some(maximum) = payload.pointer(pointer) {
        let maximum = maximum
            .as_u64()
            .ok_or_else(|| incomplete("resource limit is malformed"))?;
        if count as u64 >= maximum {
            return Err(usage("the site resource limit has been reached"));
        }
    }
    Ok(())
}
pub(super) fn fields(paths: &[&str]) -> Vec<String> {
    paths.iter().map(|path| (*path).to_owned()).collect()
}
fn observe(body: &Value, paths: &[String]) -> State {
    State {
        exists: true,
        configuration: Value::Object(
            paths
                .iter()
                .map(|path| {
                    (
                        path.clone(),
                        body.pointer(&format!("/{path}"))
                            .cloned()
                            .unwrap_or(Value::Null),
                    )
                })
                .collect(),
        ),
    }
}
fn absent() -> State {
    State {
        exists: false,
        configuration: json!({}),
    }
}
fn check_ack(reply: &Value, id: &str) -> Result<(), Error> {
    if reply
        .get("id")
        .is_some_and(|value| value.as_str() != Some(id))
    {
        return Err(incomplete("acknowledgment identified a different resource"));
    }
    Ok(())
}
fn item_path<T: TokenSource>(
    client: &Client<T>,
    collection: &str,
    id: &str,
) -> Result<String, Error> {
    let url = client.resource_url(collection, &[id], None)?;
    url.path()
        .strip_prefix(client.base.path().trim_end_matches('/'))
        .map(str::to_owned)
        .ok_or_else(|| usage("invalid resource route"))
}

type Observe = Box<dyn Fn(&Value) -> Result<State, Error> + Send + Sync>;
struct Resource<'a, T> {
    client: &'a Client<T>,
    path: String,
    id: Option<String>,
}
impl<T: TokenSource> ObjectResource for Resource<'_, T> {
    async fn read_object(&self) -> Result<Value, Error> {
        let payload = self.client.get(&self.path).await?;
        let mut body = if let Some(id) = &self.id {
            select(&parse(&payload)?, id)?.clone()
        } else {
            payload
        };
        normalize_secret_readback(&mut body);
        Ok(body)
    }
    async fn put_object(&self, body: &Value) -> Result<(), Error> {
        require_unmasked_secrets(body)?;
        let path = if let Some(id) = &self.id {
            item_path(self.client, &self.path, id)?
        } else {
            self.path.clone()
        };
        let reply = self.client.put_full(&path, body).await?;
        if let Some(id) = &self.id {
            check_ack(&reply, id)?;
        }
        for field in ["id", "kind"] {
            if let Some(returned) = reply.get(field)
                && body.get(field) != Some(returned)
            {
                return Err(incomplete("acknowledgment identified a different resource"));
            }
        }
        Ok(())
    }
}
type Update<'a, T> = FullObjectPut<Resource<'a, T>, State, Observe>;

pub struct SettingsMutation<'a, T> {
    backend: Update<'a, T>,
    target: Value,
}
impl<'a, T: TokenSource> SettingsMutation<'a, T> {
    pub fn target(&self) -> Value {
        self.target.clone()
    }
    pub(super) async fn prepare(
        client: &'a Client<T>,
        site: &str,
        kind: &'static str,
        paths: impl FnOnce(&Value) -> Result<Vec<String>, Error>,
        patch: impl FnOnce(&mut Value) -> Result<(), Error>,
    ) -> Result<(Self, Plan<State>), Error> {
        let path = path(site, kind)?;
        let body = client.get(&path).await?;
        if !body.is_object() || body.get("kind").is_some_and(|value| value != kind) {
            return Err(incomplete("settings resource identity is unavailable"));
        }
        let paths = paths(&body)?;
        let id = body.get("id").cloned();
        let observed_kind = body.get("kind").cloned();
        let observe: Observe = Box::new(move |body| {
            if !body.is_object()
                || body.get("id").cloned() != id
                || observed_kind
                    .as_ref()
                    .is_some_and(|kind| body.get("kind") != Some(kind))
                || body.get("kind").is_some_and(|value| value != kind)
            {
                return Err(incomplete("settings identity changed during readback"));
            }
            Ok(observe(body, &paths))
        });
        let (backend, plan) = FullObjectPut::prepare(
            Resource {
                client,
                path,
                id: None,
            },
            body,
            observe,
            |body| {
                patch(body)?;
                require_unmasked_secrets(body)
            },
        )?;
        Ok((
            Self {
                backend,
                target: json!({"resource":kind}),
            },
            plan,
        ))
    }
}
impl<T: TokenSource> Mutation for SettingsMutation<'_, T> {
    type State = State;
    async fn read(&self) -> Result<State, Error> {
        self.backend.read().await
    }
    async fn write(&self, desired: &State) -> Result<(), Error> {
        self.backend.write(desired).await
    }
}

enum Write<'a, T> {
    Update(Update<'a, T>),
    Create {
        body: Value,
        id: Mutex<Option<String>>,
        existing_ids: Vec<String>,
    },
    Delete(String),
}
pub struct NamedMutation<'a, T> {
    client: &'a Client<T>,
    collection: String,
    target: Value,
    paths: Vec<String>,
    desired: State,
    write: Write<'a, T>,
}
impl<'a, T: TokenSource> NamedMutation<'a, T> {
    pub fn target(&self) -> Value {
        self.target.clone()
    }
    pub(super) fn update(
        client: &'a Client<T>,
        collection: String,
        body: Value,
        paths: Vec<String>,
        patch: impl FnOnce(&mut Value) -> Result<(), Error>,
    ) -> Result<(Self, Plan<State>), Error> {
        let id = body["id"]
            .as_str()
            .ok_or_else(|| incomplete("resource identity is missing"))?
            .to_owned();
        let target = json!({"id":id,"name":body.get("name").and_then(Value::as_str)});
        let observed_id = id.clone();
        let observed_paths = paths.clone();
        let observe: Observe = Box::new(move |body| {
            if body["id"].as_str() != Some(&observed_id) {
                return Err(incomplete("resource identity changed during readback"));
            }
            Ok(observe(body, &observed_paths))
        });
        let (backend, plan) = FullObjectPut::prepare(
            Resource {
                client,
                path: collection.clone(),
                id: Some(id),
            },
            body,
            observe,
            |body| {
                patch(body)?;
                require_unmasked_secrets(body)
            },
        )?;
        Ok((
            Self {
                client,
                collection,
                target,
                paths,
                desired: plan.desired.clone(),
                write: Write::Update(backend),
            },
            plan,
        ))
    }
    pub(super) fn create(
        client: &'a Client<T>,
        collection: String,
        rows: &[Value],
        body: Value,
        paths: Vec<String>,
    ) -> Result<(Self, Plan<State>), Error> {
        require_unmasked_secrets(&body)?;
        let target = json!({"id":Value::Null,"name":body["name"]});
        let desired = observe(&body, &paths);
        let plan = Plan {
            current: absent(),
            desired: desired.clone(),
        };
        Ok((
            Self {
                client,
                collection,
                target,
                paths,
                desired,
                write: Write::Create {
                    body,
                    id: Mutex::new(None),
                    existing_ids: rows
                        .iter()
                        .map(|row| row["id"].as_str().unwrap_or_default().to_owned())
                        .collect(),
                },
            },
            plan,
        ))
    }
    pub(super) fn delete(
        client: &'a Client<T>,
        collection: String,
        row: &Value,
        references: Value,
    ) -> Result<(Self, Plan<State>), Error> {
        let id = row["id"]
            .as_str()
            .ok_or_else(|| incomplete("resource identity is missing"))?
            .to_owned();
        let desired = absent();
        let plan = Plan {
            current: State {
                exists: true,
                configuration: json!({}),
            },
            desired: desired.clone(),
        };
        Ok((
            Self {
                client,
                collection,
                target: json!({"id":id,"name":row.get("name").and_then(Value::as_str),"references":references}),
                paths: vec![],
                desired,
                write: Write::Delete(id),
            },
            plan,
        ))
    }
}
impl<T: TokenSource> Mutation for NamedMutation<'_, T> {
    type State = State;
    async fn read(&self) -> Result<State, Error> {
        if let Write::Update(backend) = &self.write {
            return backend.read().await;
        }
        let rows = parse(&self.client.get(&self.collection).await?)?;
        let id = match &self.write {
            Write::Create { id, .. } => id
                .lock()
                .map_err(|_| incomplete("creation identity lock failed"))?
                .clone(),
            Write::Delete(id) => {
                return Ok(if rows.iter().any(|row| row["id"].as_str() == Some(id)) {
                    State {
                        exists: true,
                        configuration: json!({}),
                    }
                } else {
                    absent()
                });
            }
            Write::Update(_) => unreachable!(),
        };
        let row = if let Some(id) = id {
            rows.iter().find(|row| row["id"].as_str() == Some(&id))
        } else {
            match select(
                &rows,
                self.target["name"]
                    .as_str()
                    .ok_or_else(|| incomplete("creation name is missing"))?,
            ) {
                Ok(row) => Some(row),
                Err(error) if error.kind == ErrorKind::NotFound => None,
                Err(error) => return Err(error),
            }
        };
        if let (Some(row), Write::Create { existing_ids, .. }) = (row, &self.write)
            && existing_ids.iter().any(|id| row["id"].as_str() == Some(id))
        {
            return Err(incomplete(
                "creation readback identified a pre-existing resource",
            ));
        }
        Ok(row.map_or_else(absent, |row| {
            let mut row = row.clone();
            normalize_secret_readback(&mut row);
            observe(&row, &self.paths)
        }))
    }
    async fn write(&self, desired: &State) -> Result<(), Error> {
        if desired != &self.desired {
            return Err(usage("desired state does not match the prepared change"));
        }
        match &self.write {
            Write::Update(backend) => backend.write(desired).await,
            Write::Create {
                body,
                id,
                existing_ids,
            } => {
                require_unmasked_secrets(body)?;
                let reply = self.client.create(&self.collection, body).await?;
                let created = reply
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty())
                    .ok_or_else(|| {
                        incomplete("creation acknowledgment has no resource identity")
                    })?;
                if existing_ids.iter().any(|id| id == created)
                    || reply.get("name").is_some_and(|name| name != &body["name"])
                {
                    return Err(incomplete(
                        "creation acknowledgment does not identify the new resource",
                    ));
                }
                *id.lock()
                    .map_err(|_| incomplete("creation identity lock failed"))? =
                    Some(created.to_owned());
                Ok(())
            }
            Write::Delete(id) => {
                let reply = self
                    .client
                    .delete(&item_path(self.client, &self.collection, id)?)
                    .await?;
                check_ack(&reply, id)
            }
        }
    }
}
