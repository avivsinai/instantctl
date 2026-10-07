//! Site actions whose desired result cannot be known before the request.

use std::{collections::HashMap, time::Duration};

use serde_json::{Value, json};
use tokio::time::{Instant, timeout_at};

use super::Client;
use crate::{
    Error, ErrorKind, TokenSource,
    inventory::{inventory_elements, inventory_path},
    mutation::{Outcome, Report},
    site::Resource,
};

pub struct BridgePriorityAction<'a, T> {
    client: &'a Client<T>,
    site: &'a str,
    pub current: Value,
}

impl<'a, T: TokenSource> BridgePriorityAction<'a, T> {
    pub async fn prepare(client: &'a Client<T>, site: &'a str) -> Result<Self, Error> {
        let current = priorities(client, site).await?;
        if current["devices"].as_array().is_none_or(Vec::is_empty) {
            return Err(Error::new(
                ErrorKind::Unsupported,
                "site has no reported switches",
            ));
        }
        Ok(Self {
            client,
            site,
            current,
        })
    }

    /// Send once and take one fresh observation. The portal exposes no completion
    /// marker or deterministic expected priorities, so this never reports verified.
    pub async fn apply(&self, timeout: Duration) -> Result<Report<Value>, Error> {
        if timeout.is_zero() {
            return Err(Error::new(
                ErrorKind::Config,
                "action timeout must be greater than zero",
            ));
        }
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or_else(|| Error::new(ErrorKind::Config, "action timeout is too large"))?;
        let path = format!(
            "/sites/{}/spanningTree?action=computeDevicesBridgePriority",
            self.site
        );
        let request_error = match timeout_at(deadline, self.client.create(&path, &json!({}))).await
        {
            Ok(result) => result.err(),
            Err(_) => Some(unknown("bridge priority request timed out")),
        };
        let mut report = Report {
            outcome: if request_error.is_some() {
                Outcome::Failed
            } else {
                Outcome::Unverified
            },
            observed: None,
            readback_attempts: 0,
            request_error,
            readback_error: None,
        };
        if Instant::now() < deadline {
            report.readback_attempts = 1;
            match timeout_at(deadline, priorities(self.client, self.site)).await {
                Ok(Ok(observed)) => report.observed = Some(observed),
                Ok(Err(error)) => report.readback_error = Some(error),
                Err(_) => {
                    report.readback_error = Some(unknown("bridge priority readback timed out"))
                }
            }
        } else {
            report.readback_error = Some(unknown("bridge priority readback deadline expired"));
        }
        Ok(report)
    }
}

async fn priorities<T: TokenSource>(client: &Client<T>, site: &str) -> Result<Value, Error> {
    let inventory = client.get(&inventory_path(site)?).await?;
    let devices = inventory_elements(&inventory)?;
    let spanning_tree = client.site_resource(site, Resource::SpanningTree).await?;
    let priorities = spanning_tree
        .get("devicePriorities")
        .and_then(Value::as_array)
        .ok_or_else(|| unknown("spanning-tree device priorities are unknown"))?;
    let mut by_id = HashMap::new();
    for row in priorities {
        let id = row
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .ok_or_else(|| unknown("spanning-tree priority device identity is unknown"))?;
        let priority = match row.get("priority") {
            Some(Value::Null) | None => Value::Null,
            Some(value)
                if value
                    .as_u64()
                    .is_some_and(|value| value <= u64::from(u16::MAX)) =>
            {
                value.clone()
            }
            _ => return Err(unknown("spanning-tree bridge priority is invalid")),
        };
        if by_id.insert(id.to_ascii_lowercase(), priority).is_some() {
            return Err(unknown(
                "spanning-tree priority device identity is duplicated",
            ));
        }
    }
    let mut switches = Vec::new();
    for device in devices {
        match device.get("deviceType").and_then(Value::as_str) {
            Some("accessPoint" | "gateway") => continue,
            Some("switch") => {}
            _ => return Err(unknown("inventory device type is unknown")),
        }
        let id = device["id"]
            .as_str()
            .ok_or_else(|| unknown("switch identity is unknown"))?;
        switches.push(json!({
            "id":id,
            "name":device.get("name"),
            "bridge_priority":by_id.remove(&id.to_ascii_lowercase()).unwrap_or(Value::Null),
        }));
    }
    Ok(json!({"devices":switches,"completion_verifiable":false}))
}

fn unknown(message: &str) -> Error {
    Error::new(ErrorKind::Unverified, message)
}
