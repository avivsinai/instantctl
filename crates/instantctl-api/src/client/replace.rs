//! Read replacement candidates for a validated device in a complete site inventory.

use serde_json::Value;

use super::Client;
use crate::{
    Error, ErrorKind, TokenSource,
    inventory::{inventory_path, is_mac_address, select_device},
};

/// Fetch the portal's replacement candidates for the selected inventory device.
///
/// The response is returned with its original shape and fields. Secret-like fields
/// are redacted before callers can display or serialize it.
pub async fn candidates<T: TokenSource>(
    client: &Client<T>,
    site: &str,
    selector: &str,
) -> Result<Value, Error> {
    let inventory_route = inventory_path(site)?;
    let inventory = client.get(&inventory_route).await?;
    let selected = select_device(&inventory, selector)?;
    let target_id = selected
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| is_mac_address(id))
        .ok_or_else(|| {
            Error::new(
                ErrorKind::General,
                "selected device has an invalid identity",
            )
        })?;

    let collection = inventory_route
        .strip_suffix("/inventory")
        .ok_or_else(|| Error::new(ErrorKind::Config, "invalid inventory route"))?;
    let path = format!("{collection}/replaceDevice/{target_id}");
    let mut response = client.get(&path).await?;
    redact(&mut response);
    Ok(response)
}

fn redact(value: &mut Value) {
    match value {
        Value::Object(object) => {
            for (key, child) in object {
                let normalized = key.to_ascii_lowercase();
                if [
                    "password",
                    "presharedkey",
                    "sharedsecret",
                    "secret",
                    "token",
                    "privatekey",
                ]
                .iter()
                .any(|part| normalized.contains(part))
                {
                    if !child.is_null() {
                        *child = Value::String("(redacted)".into());
                    }
                } else {
                    redact(child);
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(redact),
        _ => {}
    }
}
