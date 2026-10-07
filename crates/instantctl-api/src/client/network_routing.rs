//! Read and update wired-network IP routing state.

use serde_json::{Value, json};

use super::{
    Client,
    network::{self, Network},
};
use crate::{
    Error, ErrorKind, TokenSource,
    mutation::{FullObjectPut, Mutation, ObjectResource, Prepared},
};

/// Read routing fields for a wired network selected by ID or exact name.
pub async fn read<T: TokenSource>(
    client: &Client<T>,
    site: &str,
    selector: &str,
) -> Result<Value, Error> {
    let networks = network::list(client, site).await?;
    let selected = network::select(&networks, selector)?;
    let raw = selected.raw();
    let mut result = serde_json::Map::new();
    for field in ["id", "wiredNetworkName", "isManagement"] {
        if let Some(value) = raw.get(field) {
            result.insert(field.to_owned(), value.clone());
        }
    }
    for (field, allowed_children) in [
        ("isIpRoutingEnabled", &[][..]),
        (
            "ipRoutingConfig",
            &["isStatic", "staticIpAddress", "staticSubnetMask"][..],
        ),
        ("ipRoutingState", &["network", "netmask"][..]),
    ] {
        if let Some(value) = raw.get(field) {
            result.insert(
                field.to_owned(),
                project_known_fields(value, allowed_children)?,
            );
        }
    }
    Ok(Value::Object(result))
}

/// Prepare one full-object update, while limiting observed state to routing enabled.
pub async fn plan_update<'a, T: TokenSource>(
    client: &'a Client<T>,
    site: &str,
    selector: &str,
    enabled: bool,
) -> Result<Prepared<impl Mutation<State = bool> + 'a>, Error> {
    let networks = network::list(client, site).await?;
    let selected = network::select(&networks, selector)?;
    let id = selected.id().to_owned();
    ensure_non_management(selected.raw())?;
    ensure_permission(client, site).await?;
    let current = selected.raw().clone();
    let observed_id = id.clone();
    let resource = RoutingResource {
        client,
        site: site.to_owned(),
        id: id.clone(),
    };
    let (backend, plan) = FullObjectPut::prepare(
        resource,
        current,
        move |body| observe_routing(body, &observed_id),
        move |body| {
            body["isIpRoutingEnabled"] = json!(enabled);
            Ok(())
        },
    )?;
    let target = json!({
        "network_id": id,
        "network_name": selected.name(),
    });
    Ok(Prepared {
        backend,
        plan,
        target,
    })
}

struct RoutingResource<'a, T> {
    client: &'a Client<T>,
    site: String,
    id: String,
}

impl<T: TokenSource> RoutingResource<'_, T> {
    fn collection(&self) -> String {
        format!("/sites/{}/wiredNetworks", self.site)
    }

    async fn selected(&self) -> Result<Network, Error> {
        let networks = network::list(self.client, &self.site).await?;
        let selected = network::select(&networks, &self.id)?;
        if selected.id() != self.id {
            return Err(incomplete("wired network identity changed"));
        }
        ensure_non_management(selected.raw())?;
        Ok((*selected).clone())
    }
}

impl<T: TokenSource> ObjectResource for RoutingResource<'_, T> {
    async fn read_object(&self) -> Result<Value, Error> {
        Ok(self.selected().await?.raw().clone())
    }

    async fn put_object(&self, body: &Value) -> Result<(), Error> {
        check_identity(body, &self.id)?;
        let mut current = self.selected().await?.raw().clone();
        // The gate read is also the latest full-object snapshot. Keep its
        // unrelated fields if they changed while the user confirmed the plan.
        current["isIpRoutingEnabled"] = json!(observe_routing(body, &self.id)?);
        ensure_permission(self.client, &self.site).await?;
        let path = item_path(self.client, &self.collection(), &self.id)?;
        let response = self.client.put_full(&path, &current).await?;
        if response
            .get("id")
            .is_some_and(|ack| ack.as_str() != Some(self.id.as_str()))
        {
            return Err(incomplete(
                "acknowledgment identified a different wired network",
            ));
        }
        Ok(())
    }
}

fn observe_routing(body: &Value, id: &str) -> Result<bool, Error> {
    check_identity(body, id)?;
    body.get("isIpRoutingEnabled")
        .and_then(Value::as_bool)
        .ok_or_else(|| incomplete("wired network routing state is missing or malformed"))
}

fn ensure_non_management(body: &Value) -> Result<(), Error> {
    if body.get("isManagement").and_then(Value::as_bool) == Some(false) {
        Ok(())
    } else {
        Err(unsupported(
            "IP routing changes require a confirmed non-management wired network",
        ))
    }
}

async fn ensure_permission<T: TokenSource>(client: &Client<T>, site: &str) -> Result<(), Error> {
    let permissions = match client.get(&format!("/sites/{site}/permissions")).await {
        Ok(permissions) => permissions,
        Err(error) if error.kind == ErrorKind::NotFound => {
            return Err(unsupported("site permissions are missing or malformed"));
        }
        Err(error)
            if error.kind == ErrorKind::General
                && matches!(
                    error.message.as_str(),
                    "portal returned a non-JSON response" | "portal returned invalid JSON"
                ) =>
        {
            return Err(unsupported("site permissions are missing or malformed"));
        }
        Err(error) => return Err(error),
    };
    let entries = permissions
        .get("permissions")
        .and_then(Value::as_array)
        .ok_or_else(|| unsupported("site permissions are missing or malformed"))?;
    let mut allowed = false;
    for entry in entries {
        let permission = entry
            .get("permission")
            .and_then(Value::as_str)
            .filter(|permission| !permission.is_empty())
            .ok_or_else(|| unsupported("site permissions are missing or malformed"))?;
        allowed |= permission == "inventory_update_all";
    }
    if allowed {
        Ok(())
    } else {
        Err(unsupported(
            "site permissions are missing inventory_update_all",
        ))
    }
}

fn check_identity(body: &Value, id: &str) -> Result<(), Error> {
    if body.get("id").and_then(Value::as_str) != Some(id)
        || body
            .get("isWireless")
            .is_some_and(|wireless| wireless != false)
    {
        return Err(incomplete("wired network identity or type changed"));
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
        .ok_or_else(|| Error::new(ErrorKind::Config, "invalid wired network route"))
}

fn project_known_fields(value: &Value, fields: &[&str]) -> Result<Value, Error> {
    if value.is_null() {
        return Ok(Value::Null);
    }
    if fields.is_empty() {
        if !value.is_boolean() {
            return Err(incomplete("wired routing enabled state is malformed"));
        }
        return Ok(value.clone());
    }
    let object = value
        .as_object()
        .ok_or_else(|| incomplete("wired routing configuration is malformed"))?;
    let mut projected = serde_json::Map::new();
    for field in fields {
        if let Some(value) = object.get(*field) {
            projected.insert((*field).to_owned(), value.clone());
        }
    }
    Ok(Value::Object(projected))
}

fn unsupported(message: &'static str) -> Error {
    Error::new(ErrorKind::Unsupported, message)
}

fn incomplete(message: &'static str) -> Error {
    Error::new(ErrorKind::Unverified, message)
}
