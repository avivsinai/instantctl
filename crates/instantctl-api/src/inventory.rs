use serde_json::Value;

use crate::{Error, ErrorKind};

/// Validate the site UUID shape used for inventory route construction.
pub fn validate_site_id(site: &str) -> Result<(), Error> {
    let bytes = site.as_bytes();
    let valid = bytes.len() == 36
        && bytes.iter().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                *byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        });
    if !valid {
        return Err(Error::new(
            ErrorKind::Config,
            "site identifier must be a UUID",
        ));
    }
    Ok(())
}

pub(crate) fn inventory_path(site: &str) -> Result<String, Error> {
    validate_site_id(site)?;
    Ok(format!("/sites/{}/inventory", site.to_ascii_lowercase()))
}

pub(crate) fn select_device<'a>(inventory: &'a Value, selector: &str) -> Result<&'a Value, Error> {
    let elements = inventory_elements(inventory)?;
    let matches: Vec<&Value> = if is_mac_address(selector) {
        elements
            .iter()
            .filter(|device| {
                ["id", "macAddress"].iter().any(|field| {
                    device
                        .get(field)
                        .and_then(Value::as_str)
                        .is_some_and(|candidate| candidate.eq_ignore_ascii_case(selector))
                })
            })
            .collect()
    } else {
        elements
            .iter()
            .filter(|device| device.get("name").and_then(Value::as_str) == Some(selector))
            .collect()
    };
    match matches.as_slice() {
        [device] => Ok(*device),
        [] => Err(Error::new(
            ErrorKind::Usage,
            "no device matches the selector",
        )),
        _ => Err(Error::new(
            ErrorKind::Usage,
            "device name is ambiguous; select by MAC address",
        )),
    }
}

pub(crate) fn select_device_by_id<'a>(
    inventory: &'a Value,
    device_id: &str,
) -> Result<&'a Value, Error> {
    let elements = inventory_elements(inventory)?;
    let matches: Vec<&Value> = elements
        .iter()
        .filter(|device| device.get("id").and_then(Value::as_str) == Some(device_id))
        .collect();
    match matches.as_slice() {
        [device] => Ok(*device),
        [] => Err(general("device was absent from inventory readback")),
        _ => Err(general(
            "inventory readback has duplicate device identifiers",
        )),
    }
}

pub(crate) fn inventory_elements(inventory: &Value) -> Result<&[Value], Error> {
    if inventory.get("kind").and_then(Value::as_str) != Some("resourceList") {
        return Err(general("inventory response is not a resourceList"));
    }
    let elements = inventory
        .get("elements")
        .and_then(Value::as_array)
        .ok_or_else(|| general("inventory response has invalid elements"))?;
    let total_count = inventory
        .get("totalCount")
        .and_then(Value::as_u64)
        .ok_or_else(|| general("inventory response has invalid totalCount"))?;
    let matching_count = inventory
        .get("matchingFilterCount")
        .and_then(Value::as_u64)
        .ok_or_else(|| general("inventory response has invalid matchingFilterCount"))?;
    let pending = inventory
        .get("pendingAvailability")
        .ok_or_else(|| general("inventory response is missing pendingAvailability"))?;
    if !matches!(pending, Value::Null | Value::Bool(false))
        && pending.as_u64() != Some(0)
        && !matches!(pending, Value::Array(items) if items.is_empty())
        && !matches!(pending, Value::Object(items) if items.is_empty())
    {
        return Err(general("inventory has pending availability"));
    }
    if total_count != matching_count || usize::try_from(matching_count).ok() != Some(elements.len())
    {
        return Err(general(
            "inventory is partial; counts do not match returned devices",
        ));
    }
    if has_pagination_marker(inventory.get("metaData").unwrap_or(&Value::Null)) {
        return Err(general("inventory indicates more pages are available"));
    }

    let mut ids = Vec::with_capacity(elements.len());
    let mut mac_addresses = Vec::with_capacity(elements.len());
    for device in elements {
        let Some(device) = device.as_object() else {
            return Err(general("inventory contains an invalid device"));
        };
        let (Some(id), Some(mac_address)) = (
            device.get("id").and_then(Value::as_str),
            device.get("macAddress").and_then(Value::as_str),
        ) else {
            return Err(general("inventory device identity is missing or invalid"));
        };
        if !is_mac_address(id) || !is_mac_address(mac_address) {
            return Err(general("inventory device identity is missing or invalid"));
        }
        ids.push(id.to_ascii_lowercase());
        mac_addresses.push(mac_address.to_ascii_lowercase());
    }
    ids.sort_unstable();
    ids.dedup();
    mac_addresses.sort_unstable();
    mac_addresses.dedup();
    if ids.len() != elements.len() || mac_addresses.len() != elements.len() {
        return Err(general("inventory contains duplicate device identities"));
    }
    Ok(elements)
}

pub(crate) fn is_mac_address(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 17
        && bytes.iter().enumerate().all(|(index, byte)| {
            if matches!(index, 2 | 5 | 8 | 11 | 14) {
                *byte == b':'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

pub(crate) fn validate_action_ack(response: &Value, device_id: &str) -> Result<(), Error> {
    let acknowledged_id = response.get("id").and_then(Value::as_str);
    let acknowledged_kind = response.get("kind").and_then(Value::as_str);
    if acknowledged_id != Some(device_id) || !acknowledged_kind.is_some_and(|kind| !kind.is_empty())
    {
        return Err(general(
            "action acknowledgment did not identify the requested device",
        ));
    }
    Ok(())
}

fn has_pagination_marker(value: &Value) -> bool {
    match value {
        Value::Object(object) => object.iter().any(|(key, child)| {
            let normalized = key.to_ascii_lowercase().replace('_', "");
            let marker_is_present = match child {
                Value::Null | Value::Bool(false) => false,
                Value::String(value) => !value.is_empty(),
                Value::Number(value) => value.as_u64() != Some(0) && value.as_i64() != Some(0),
                _ => true,
            };
            (normalized.contains("next")
                && ["page", "cursor", "token"]
                    .iter()
                    .any(|word| normalized.contains(word))
                && marker_is_present)
                || (matches!(normalized.as_str(), "hasmore" | "istruncated" | "partial")
                    && child == &Value::Bool(true))
                || has_pagination_marker(child)
        }),
        Value::Array(items) => items.iter().any(has_pagination_marker),
        _ => false,
    }
}

fn general(message: &'static str) -> Error {
    Error::new(ErrorKind::General, message)
}
