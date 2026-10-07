//! Per-device cable and connectivity diagnostics.

use std::{collections::HashSet, net::IpAddr, sync::Mutex};

use serde::Serialize;
use serde_json::{Value, json};

use super::{Client, reads};
use crate::{
    Error, ErrorKind, TokenSource,
    device::{DeviceResource, Prepared},
    mutation::{Mutation, Plan},
};

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DiagnosticState {
    pub started: bool,
    pub complete: bool,
}

#[derive(Clone, Copy)]
enum DiagnosticKind {
    Cable { port_number: u64 },
    Connectivity,
}

/// One diagnostic start request and its authoritative result readback.
pub struct Diagnostic<'a, T> {
    client: &'a Client<T>,
    collection: String,
    device_id: String,
    kind: DiagnosticKind,
    baseline_ids: HashSet<String>,
    acknowledged_id: Mutex<Option<String>>,
    details: Mutex<Value>,
}

impl<T: TokenSource> Diagnostic<'_, T> {
    /// Return the POST acknowledgment and the latest matching GET result.
    pub fn details(&self) -> Value {
        self.details
            .lock()
            .map(|details| details.clone())
            .unwrap_or_else(|_| json!({"acknowledgment":null,"observed":null}))
    }
}

impl<T: TokenSource> Mutation for Diagnostic<'_, T> {
    type State = DiagnosticState;

    async fn read(&self) -> Result<Self::State, Error> {
        let acknowledged_id = self
            .acknowledged_id
            .lock()
            .map_err(|_| incomplete("diagnostic identity lock failed"))?
            .clone();
        let Some(acknowledged_id) = acknowledged_id else {
            return Ok(DiagnosticState {
                started: false,
                complete: false,
            });
        };

        let payload = self.client.get(&self.device_path()?).await?;
        let rows = diagnostic_rows(&payload)?;
        let mut matching = rows
            .iter()
            .filter(|row| row.get("id").and_then(Value::as_str) == Some(&acknowledged_id));
        let Some(row) = matching.next() else {
            self.set_observed(Value::Null)?;
            return Ok(DiagnosticState {
                started: false,
                complete: false,
            });
        };
        if matching.next().is_some() {
            return Err(incomplete(
                "diagnostic result has duplicate test identities",
            ));
        }
        self.validate_identity(row, &acknowledged_id)?;
        self.set_observed(row.clone())?;
        Ok(DiagnosticState {
            started: true,
            complete: row.get("state").and_then(Value::as_str) == Some("complete"),
        })
    }

    async fn write(&self, desired: &Self::State) -> Result<(), Error> {
        if desired
            != &(DiagnosticState {
                started: true,
                complete: true,
            })
        {
            return Err(usage("diagnostic write requires a complete test state"));
        }
        let response = match self.kind {
            DiagnosticKind::Cable { port_number } => {
                self.client
                    .create(&self.device_path()?, &json!({"portNumber":port_number}))
                    .await?
            }
            DiagnosticKind::Connectivity => {
                let address = self
                    .details
                    .lock()
                    .map_err(|_| incomplete("diagnostic details lock failed"))?
                    .get("requested_address")
                    .and_then(Value::as_str)
                    .ok_or_else(|| incomplete("connectivity test address is missing"))?
                    .to_owned();
                self.client
                    .action(
                        &self.collection,
                        &self.device_id,
                        "start",
                        &json!({"address":address}),
                    )
                    .await?
            }
        };
        self.set_acknowledgment(response.clone())?;
        let id = response
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .ok_or_else(|| incomplete("diagnostic acknowledgment has no test identifier"))?;
        if self.baseline_ids.contains(id) {
            return Err(incomplete(
                "diagnostic acknowledgment identified a test that predates this request",
            ));
        }
        self.validate_identity(&response, id)?;
        *self
            .acknowledged_id
            .lock()
            .map_err(|_| incomplete("diagnostic identity lock failed"))? = Some(id.to_owned());
        Ok(())
    }
}

impl<T: TokenSource> Diagnostic<'_, T> {
    fn device_path(&self) -> Result<String, Error> {
        let url = self
            .client
            .resource_url(&self.collection, &[&self.device_id], None)?;
        let prefix = self.client.base.path().trim_end_matches('/');
        let path = url
            .path()
            .strip_prefix(prefix)
            .ok_or_else(|| Error::new(ErrorKind::Config, "invalid diagnostic resource route"))?;
        Ok(path.to_owned())
    }

    fn validate_identity(&self, row: &Value, id: &str) -> Result<(), Error> {
        if row.get("id").and_then(Value::as_str) != Some(id)
            || row.get("deviceId").and_then(Value::as_str) != Some(self.device_id.as_str())
        {
            return Err(incomplete(
                "diagnostic acknowledgment or readback identified a different test or device",
            ));
        }
        match self.kind {
            DiagnosticKind::Cable { port_number } => {
                if row.get("portNumber").and_then(Value::as_u64) != Some(port_number) {
                    return Err(incomplete(
                        "cable test acknowledgment or readback identified a different API port",
                    ));
                }
            }
            DiagnosticKind::Connectivity => {
                let requested = self
                    .details
                    .lock()
                    .map_err(|_| incomplete("diagnostic details lock failed"))?
                    .get("requested_address")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .ok_or_else(|| incomplete("connectivity test address is missing"))?;
                if row.get("address").and_then(Value::as_str) != Some(requested.as_str()) {
                    return Err(incomplete(
                        "connectivity test acknowledgment or readback identified a different address",
                    ));
                }
            }
        }
        Ok(())
    }

    fn set_acknowledgment(&self, value: Value) -> Result<(), Error> {
        self.details
            .lock()
            .map_err(|_| incomplete("diagnostic details lock failed"))?
            .as_object_mut()
            .ok_or_else(|| incomplete("diagnostic details are malformed"))?
            .insert("acknowledgment".into(), value);
        Ok(())
    }

    fn set_observed(&self, value: Value) -> Result<(), Error> {
        self.details
            .lock()
            .map_err(|_| incomplete("diagnostic details lock failed"))?
            .as_object_mut()
            .ok_or_else(|| incomplete("diagnostic details are malformed"))?
            .insert("observed".into(), value);
        Ok(())
    }
}

pub async fn plan_cable_test<'a, T: TokenSource>(
    client: &'a Client<T>,
    site: &str,
    device_selector: &str,
    faceplate: u64,
    force: bool,
) -> Result<Prepared<Diagnostic<'a, T>>, Error> {
    let (resource, device, _) = DeviceResource::resolve(client, site, device_selector).await?;
    let port_number =
        crate::ports::guard_faceplate(&device, faceplate, client.protected_ports(), force)?;
    let port = device
        .get("ethernetPorts")
        .and_then(Value::as_array)
        .and_then(|ports| {
            ports.iter().find(|port| {
                port.get("faceplatePortNumber").and_then(Value::as_u64) == Some(faceplate)
            })
        })
        .ok_or_else(|| incomplete("selected faceplate port is missing from inventory"))?;
    if port
        .pointer("/capabilities/cableTest")
        .and_then(Value::as_bool)
        != Some(true)
    {
        return Err(Error::new(
            ErrorKind::Unsupported,
            "selected port does not report cable test support",
        ));
    }
    let mut target = resource.target(&device);
    target["port"] = json!(faceplate);
    target["api_port_number"] = json!(port_number);
    prepare(
        client,
        site,
        &device,
        target,
        DiagnosticKind::Cable { port_number },
        None,
    )
    .await
}

pub async fn plan_connectivity_test<'a, T: TokenSource>(
    client: &'a Client<T>,
    site: &str,
    device_selector: &str,
    address: String,
) -> Result<Prepared<Diagnostic<'a, T>>, Error> {
    if !valid_address(&address) {
        return Err(usage(
            "connectivity test address must be an IP address or DNS name without a URL or port",
        ));
    }
    let (resource, device, _) = DeviceResource::resolve(client, site, device_selector).await?;
    let mut target = resource.target(&device);
    target["address"] = json!(address);
    prepare(
        client,
        site,
        &device,
        target,
        DiagnosticKind::Connectivity,
        Some(address),
    )
    .await
}

async fn prepare<'a, T: TokenSource>(
    client: &'a Client<T>,
    site: &str,
    device: &Value,
    target: Value,
    kind: DiagnosticKind,
    address: Option<String>,
) -> Result<Prepared<Diagnostic<'a, T>>, Error> {
    if !reads::valid_site_id(site) {
        return Err(Error::new(ErrorKind::Config, "--site must be a UUID"));
    }
    let device_id = device
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| incomplete("device identifier is missing"))?
        .to_owned();
    let resource = match kind {
        DiagnosticKind::Cable { .. } => "cableTest",
        DiagnosticKind::Connectivity => "connectivityTest",
    };
    let collection = format!("/sites/{site}/{resource}");
    let url = client.resource_url(&collection, &[&device_id], None)?;
    let prefix = client.base.path().trim_end_matches('/');
    let path = url
        .path()
        .strip_prefix(prefix)
        .ok_or_else(|| Error::new(ErrorKind::Config, "invalid diagnostic resource route"))?
        .to_owned();
    let payload = client.get(&path).await?;
    let baseline_ids = diagnostic_rows(&payload)?
        .iter()
        .map(|row| {
            row.get("id")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
                .map(str::to_owned)
                .ok_or_else(|| incomplete("baseline diagnostic has no test identifier"))
        })
        .collect::<Result<HashSet<_>, _>>()?;
    let mut details = json!({"acknowledgment":null,"observed":null});
    if let Some(address) = address {
        details["requested_address"] = json!(address);
    }
    let current = DiagnosticState {
        started: false,
        complete: false,
    };
    Ok(Prepared {
        backend: Diagnostic {
            client,
            collection,
            device_id,
            kind,
            baseline_ids,
            acknowledged_id: Mutex::new(None),
            details: Mutex::new(details),
        },
        plan: Plan {
            current,
            desired: DiagnosticState {
                started: true,
                complete: true,
            },
        },
        target,
    })
}

fn diagnostic_rows(payload: &Value) -> Result<Vec<Value>, Error> {
    reads::parse_elements(payload)
}

fn valid_address(address: &str) -> bool {
    if address.is_empty() || address.len() > 253 || !address.is_ascii() {
        return false;
    }
    if address.parse::<IpAddr>().is_ok() {
        return true;
    }
    let hostname = address.strip_suffix('.').unwrap_or(address);
    !hostname.is_empty()
        && hostname.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && label.as_bytes()[0].is_ascii_alphanumeric()
                && label.as_bytes()[label.len() - 1].is_ascii_alphanumeric()
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
}

fn usage(message: &str) -> Error {
    Error::new(ErrorKind::Usage, message)
}

fn incomplete(message: &str) -> Error {
    Error::new(ErrorKind::Unverified, message)
}
