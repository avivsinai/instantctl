//! Read validated switch-stack membership without exposing full device objects.

use std::collections::HashSet;

use serde::Serialize;
use serde_json::Value;

use super::{Client, reads};
use crate::{Error, ErrorKind, TokenSource};

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Member {
    pub device_id: String,
    pub mac_address: Option<String>,
    pub name: Option<String>,
    /// The portal value is preserved, including future role strings.
    pub role: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Stack {
    pub id: String,
    pub name: Option<String>,
    pub active_conductor_id: Option<String>,
    pub members: Vec<Member>,
}

/// Read the stack collection from the selected site's collection route.
pub async fn list<T: TokenSource>(client: &Client<T>, site: &str) -> Result<Vec<Stack>, Error> {
    if !reads::valid_site_id(site) {
        return Err(Error::new(ErrorKind::Config, "--site must be a UUID"));
    }
    let payload = client
        .get(&format!(
            "/sites/{}/deviceStacks",
            site.to_ascii_lowercase()
        ))
        .await?;
    let elements: Vec<Value> = reads::parse_elements(&payload)?;
    let mut ids = HashSet::new();
    let mut stacks = Vec::with_capacity(elements.len());
    for element in &elements {
        let stack = parse_stack(element)?;
        if !ids.insert(stack.id.clone()) {
            return Err(incomplete("stack collection contains duplicate identities"));
        }
        stacks.push(stack);
    }
    Ok(stacks)
}

/// Resolve by stable stack ID first, then by exact unique name.
pub fn select<'a>(stacks: &'a [Stack], selector: &str) -> Result<&'a Stack, Error> {
    if let Some(stack) = stacks.iter().find(|stack| stack.id == selector) {
        return Ok(stack);
    }
    let mut matches = stacks
        .iter()
        .filter(|stack| stack.name.as_deref() == Some(selector));
    let stack = matches
        .next()
        .ok_or_else(|| Error::new(ErrorKind::NotFound, "no stack matches the selector"))?;
    if matches.next().is_some() {
        return Err(Error::new(
            ErrorKind::Usage,
            "stack name is ambiguous; select by stack ID",
        ));
    }
    Ok(stack)
}

fn parse_stack(value: &Value) -> Result<Stack, Error> {
    let id = required_string(value, "id", "stack identity is missing")?.to_owned();
    let raw_members = value
        .get("deviceStackMembers")
        .and_then(Value::as_array)
        .ok_or_else(|| incomplete("stack member collection is missing or invalid"))?;
    let mut member_ids = HashSet::new();
    let mut members = Vec::with_capacity(raw_members.len());
    for row in raw_members {
        let device_id = required_string(row, "deviceId", "stack member identity is missing")?;
        if !reads::is_mac(device_id) || !member_ids.insert(device_id.to_ascii_lowercase()) {
            return Err(incomplete(
                "stack member identities are invalid or duplicated",
            ));
        }
        let device = row
            .get("device")
            .filter(|device| device.is_object())
            .ok_or_else(|| incomplete("stack member device identity is missing"))?;
        let nested_id = required_string(device, "id", "stack member device identity is missing")?;
        if !reads::is_mac(nested_id) || !nested_id.eq_ignore_ascii_case(device_id) {
            return Err(incomplete(
                "stack member and nested device identities disagree",
            ));
        }
        let mac_address = match device.get("macAddress") {
            Some(Value::String(mac)) if reads::is_mac(mac) => Some(mac.clone()),
            Some(Value::Null) | None => None,
            _ => return Err(incomplete("stack member MAC address is invalid")),
        };
        let name = optional_string(device, "name", "stack member name is invalid")?;
        let role = optional_string(row, "deviceStackRole", "stack member role is invalid")?;
        members.push(Member {
            device_id: device_id.to_owned(),
            mac_address,
            name,
            role,
        });
    }

    let active_conductor_id = optional_string(
        value,
        "activeConductorId",
        "stack conductor identity is invalid",
    )?;
    if let Some(id) = &active_conductor_id
        && (!reads::is_mac(id)
            || !members
                .iter()
                .any(|member| member.device_id.eq_ignore_ascii_case(id)))
    {
        return Err(incomplete(
            "stack conductor identity is missing from the member collection",
        ));
    }
    let name = optional_string(value, "name", "stack name is invalid")?;

    Ok(Stack {
        id,
        name,
        active_conductor_id,
        members,
    })
}

fn required_string<'a>(value: &'a Value, key: &str, message: &str) -> Result<&'a str, Error> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| incomplete(message))
}

fn optional_string(value: &Value, key: &str, message: &str) -> Result<Option<String>, Error> {
    match value.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        _ => Err(incomplete(message)),
    }
}

fn incomplete(message: &str) -> Error {
    Error::new(ErrorKind::Unverified, message)
}
