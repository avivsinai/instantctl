//! Read and update selected wireless-network or wired-port MAC allow lists.

use std::collections::BTreeSet;

use reqwest::Method;
use serde::Serialize;
use serde_json::{Value, json};

use super::{Client, reads, wlan};
use crate::{
    Error, ErrorKind, TokenSource,
    device::DeviceResource,
    mutation::{Mutation, Plan, Prepared},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PortScope {
    Port(u64),
    Trunk(u64),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
enum AllowListStatus {
    Allowed,
    MaxEntityAllowedAllowListReached,
    MaxAllowedClientsReached,
    Forbidden,
}

impl AllowListStatus {
    fn parse(value: &Value) -> Result<Self, Error> {
        match value.as_str() {
            Some("allowed") => Ok(Self::Allowed),
            Some("maxEntityAllowedAllowListReached") => Ok(Self::MaxEntityAllowedAllowListReached),
            Some("maxAllowedClientsReached") => Ok(Self::MaxAllowedClientsReached),
            Some("forbidden") => Ok(Self::Forbidden),
            _ => Err(unknown("allow-list eligibility is missing or unknown")),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct State {
    id: String,
    allowed_clients: Vec<String>,
}

#[derive(Clone, Debug)]
struct Snapshot {
    state: State,
    status: AllowListStatus,
    max_allowed_clients: Option<usize>,
}

#[derive(Clone, Debug)]
enum ResourcePath {
    Wireless {
        site: String,
        network_id: String,
        network_name: Option<String>,
        allow_list_id: String,
    },
    Wired {
        site: String,
        device_id: String,
        device_name: Option<String>,
        allow_list_id: String,
        scope: PortScope,
    },
}

impl ResourcePath {
    async fn state<T: TokenSource>(&self, client: &Client<T>) -> Result<Value, Error> {
        let value = match self {
            Self::Wireless {
                site,
                allow_list_id,
                ..
            } => {
                let path = client.resource_url(
                    &format!("/sites/{site}/extendWirelessNetworkAllowList"),
                    &[allow_list_id],
                    None,
                )?;
                client.request(Method::GET, path, None).await
            }
            Self::Wired {
                site,
                device_id,
                scope,
                ..
            } => {
                let path = device_details_url(client, site, device_id)?;
                let details = client.request(Method::GET, path, None).await?;
                allow_list_from_device(&details, device_id, *scope)
            }
        }?;
        let expected_id = match self {
            Self::Wireless { allow_list_id, .. } | Self::Wired { allow_list_id, .. } => {
                allow_list_id
            }
        };
        if parse_snapshot(&value)?.state.id.as_str() != expected_id.as_str() {
            return Err(unknown("allow-list identity changed during read"));
        }
        Ok(value)
    }

    fn action_url<T: TokenSource>(&self, client: &Client<T>, add: bool) -> Result<url::Url, Error> {
        let action = if add {
            "addToAllowList"
        } else {
            "removeFromAllowList"
        };
        match self {
            Self::Wireless {
                site,
                allow_list_id,
                ..
            } => client.resource_url(
                &format!("/sites/{site}/extendWirelessNetworkAllowList"),
                &[allow_list_id],
                Some(action),
            ),
            Self::Wired {
                site,
                device_id,
                allow_list_id,
                scope,
                ..
            } => {
                let route_id = if add { allow_list_id } else { device_id };
                let mut url = client.resource_url(
                    &format!("/sites/{site}/extendWiredPortAllowList"),
                    &[route_id],
                    Some(action),
                )?;
                let (name, number) = match scope {
                    PortScope::Port(number) => ("portNumber", number),
                    PortScope::Trunk(number) => ("trunkNumber", number),
                };
                url.query_pairs_mut().append_pair(name, &number.to_string());
                Ok(url)
            }
        }
    }

    fn target(&self) -> Value {
        match self {
            Self::Wireless {
                network_id,
                network_name,
                allow_list_id,
                ..
            } => json!({
                "kind":"wireless_network",
                "network_id":network_id,
                "network_name":network_name,
                "allow_list_id":allow_list_id,
            }),
            Self::Wired {
                device_id,
                device_name,
                allow_list_id,
                scope,
                ..
            } => match scope {
                PortScope::Port(number) => json!({
                    "kind":"wired_port",
                    "device_id":device_id,
                    "device_name":device_name,
                    "allow_list_id":allow_list_id,
                    "port_number":number,
                }),
                PortScope::Trunk(number) => json!({
                    "kind":"wired_trunk",
                    "device_id":device_id,
                    "device_name":device_name,
                    "allow_list_id":allow_list_id,
                    "trunk_number":number,
                }),
            },
        }
    }
}

pub async fn read_wireless<T: TokenSource>(
    client: &Client<T>,
    site: &str,
    selector: &str,
) -> Result<Value, Error> {
    let path = wireless_path(client, site, selector).await?;
    let snapshot = parse_snapshot(&path.state(client).await?)?;
    reject_forbidden(snapshot.status)?;
    snapshot_value(snapshot)
}

pub async fn plan_wireless<'a, T: TokenSource>(
    client: &'a Client<T>,
    site: &str,
    selector: &str,
    add: bool,
    mac_addresses: &[String],
) -> Result<Prepared<AllowListMutation<'a, T>>, Error> {
    let requested = validate_mac_addresses(mac_addresses)?;
    let path = wireless_path(client, site, selector).await?;
    let target = path.target();
    let snapshot = parse_snapshot(&path.state(client).await?)?;
    prepare(client, path, target, add, requested, snapshot)
}

pub async fn read_wired<T: TokenSource>(
    client: &Client<T>,
    site: &str,
    selector: &str,
    scope: PortScope,
) -> Result<Value, Error> {
    validate_port_scope(scope)?;
    let (_path, snapshot) = wired_path(client, site, selector, scope).await?;
    reject_forbidden(snapshot.status)?;
    snapshot_value(snapshot)
}

pub async fn plan_wired<'a, T: TokenSource>(
    client: &'a Client<T>,
    site: &str,
    selector: &str,
    scope: PortScope,
    add: bool,
    mac_addresses: &[String],
) -> Result<Prepared<AllowListMutation<'a, T>>, Error> {
    validate_port_scope(scope)?;
    let requested = validate_mac_addresses(mac_addresses)?;
    let (path, snapshot) = wired_path(client, site, selector, scope).await?;
    let target = path.target();
    prepare(client, path, target, add, requested, snapshot)
}

pub fn validate_mac_addresses(values: &[String]) -> Result<Vec<String>, Error> {
    if values.is_empty() {
        return Err(usage("provide at least one MAC address"));
    }
    let mut normalized = BTreeSet::new();
    for value in values {
        if !reads::is_mac(value) {
            return Err(usage("allow-list MAC address is invalid"));
        }
        if !normalized.insert(value.to_ascii_uppercase()) {
            return Err(usage("allow-list MAC addresses must be unique"));
        }
    }
    Ok(normalized.into_iter().collect())
}

async fn wireless_path<T: TokenSource>(
    client: &Client<T>,
    site: &str,
    selector: &str,
) -> Result<ResourcePath, Error> {
    if !reads::valid_site_id(site) {
        return Err(Error::new(ErrorKind::Config, "--site must be a UUID"));
    }
    let networks = wlan::list(client, site).await?;
    let network = wlan::select(&networks, selector)?;
    let allow_list_id = network.allow_list_id()?.to_owned();
    let site = site.to_ascii_lowercase();
    Ok(ResourcePath::Wireless {
        site,
        network_id: network.id().to_owned(),
        network_name: network.name().map(str::to_owned),
        allow_list_id,
    })
}

async fn wired_path<T: TokenSource>(
    client: &Client<T>,
    site: &str,
    selector: &str,
    scope: PortScope,
) -> Result<(ResourcePath, Snapshot), Error> {
    let (_resource, device, _) = DeviceResource::resolve(client, site, selector).await?;
    let device_id = device
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| unknown("device identity is missing"))?
        .to_owned();
    let site = site.to_ascii_lowercase();
    let details = client
        .request(
            Method::GET,
            device_details_url(client, &site, &device_id)?,
            None,
        )
        .await?;
    let allow_list = allow_list_from_device(&details, &device_id, scope)?;
    let snapshot = parse_snapshot(&allow_list)?;
    let allow_list_id = allow_list
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| unknown("wired allow-list identity is missing"))?
        .to_owned();
    Ok((
        ResourcePath::Wired {
            site,
            device_id,
            device_name: device
                .get("name")
                .and_then(Value::as_str)
                .map(str::to_owned),
            allow_list_id,
            scope,
        },
        snapshot,
    ))
}

fn device_details_url<T: TokenSource>(
    client: &Client<T>,
    site: &str,
    device_id: &str,
) -> Result<url::Url, Error> {
    client.resource_url(&format!("/sites/{site}/deviceDetails"), &[device_id], None)
}

fn allow_list_from_device(
    details: &Value,
    device_id: &str,
    scope: PortScope,
) -> Result<Value, Error> {
    if details.get("id").and_then(Value::as_str) != Some(device_id) {
        return Err(unknown("device identity changed during allow-list read"));
    }
    let (field, number) = match scope {
        PortScope::Port(number) => ("allowListByPortNumber", number),
        PortScope::Trunk(number) => ("allowListByTrunkNumber", number),
    };
    details
        .get(field)
        .and_then(Value::as_object)
        .and_then(|items| items.get(&number.to_string()))
        .filter(|value| value.is_object())
        .cloned()
        .ok_or_else(|| unknown("selected wired allow-list is unavailable"))
}

pub struct AllowListMutation<'a, T> {
    client: &'a Client<T>,
    path: ResourcePath,
    add: bool,
    requested: Vec<String>,
    desired: State,
}

fn prepare<'a, T: TokenSource>(
    client: &'a Client<T>,
    path: ResourcePath,
    target: Value,
    add: bool,
    requested: Vec<String>,
    snapshot: Snapshot,
) -> Result<Prepared<AllowListMutation<'a, T>>, Error> {
    let current = snapshot.state;
    reject_forbidden(snapshot.status)?;
    let mut clients: BTreeSet<_> = current.allowed_clients.iter().cloned().collect();
    if add {
        if snapshot.status == AllowListStatus::MaxAllowedClientsReached {
            return Err(Error::new(
                ErrorKind::Unsupported,
                "selected allow list cannot accept additional clients",
            ));
        }
        let limit = snapshot
            .max_allowed_clients
            .filter(|limit| *limit > 0)
            .ok_or_else(|| unknown("allow-list client capacity is missing or invalid"))?;
        if clients.len() > limit {
            return Err(unknown("current allow-list exceeds its client capacity"));
        }
        clients.extend(requested.iter().cloned());
        if clients.len() > limit {
            return Err(usage("requested clients exceed allow-list capacity"));
        }
    } else {
        for address in &requested {
            clients.remove(address);
        }
    }
    let desired = State {
        id: current.id.clone(),
        allowed_clients: clients.into_iter().collect(),
    };
    Ok(Prepared {
        backend: AllowListMutation {
            client,
            path,
            add,
            requested,
            desired: desired.clone(),
        },
        plan: Plan { current, desired },
        target,
    })
}

impl<T: TokenSource> Mutation for AllowListMutation<'_, T> {
    type State = State;

    async fn read(&self) -> Result<Self::State, Error> {
        let snapshot = parse_snapshot(&self.path.state(self.client).await?)?;
        reject_forbidden(snapshot.status)?;
        Ok(snapshot.state)
    }

    async fn write(&self, desired: &Self::State) -> Result<(), Error> {
        if desired != &self.desired {
            return Err(usage(
                "requested allow-list state differs from the prepared plan",
            ));
        }
        let url = self.path.action_url(self.client, self.add)?;
        let body = json!({"macAddresses":self.requested});
        let response = self.client.request(Method::POST, url, Some(&body)).await?;
        let acknowledged_id = response.get("id").or_else(|| {
            response
                .get("allowList")
                .and_then(|allow_list| allow_list.get("id"))
        });
        if let Some(id) = acknowledged_id
            && id.as_str() != Some(self.desired.id.as_str())
        {
            return Err(unknown("allow-list acknowledgment identity changed"));
        }
        Ok(())
    }
}

fn parse_snapshot(value: &Value) -> Result<Snapshot, Error> {
    let id = value
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| unknown("allow-list identity is missing"))?
        .to_owned();
    let status = AllowListStatus::parse(
        value
            .get("allowListState")
            .ok_or_else(|| unknown("allow-list eligibility is missing or unknown"))?,
    )?;
    let allowed = value
        .get("allowedClients")
        .and_then(Value::as_array)
        .ok_or_else(|| unknown("allow-list client collection is unknown"))?;
    let mut clients = BTreeSet::new();
    for entry in allowed {
        let mac = entry
            .get("macAddress")
            .and_then(Value::as_str)
            .filter(|mac| reads::is_mac(mac))
            .ok_or_else(|| unknown("allow-list client identity is invalid"))?;
        if !clients.insert(mac.to_ascii_uppercase()) {
            return Err(unknown("allow-list client identities are duplicated"));
        }
    }
    let max_allowed_clients = match value.get("maxAllowedClients") {
        None | Some(Value::Null) => None,
        Some(value) => Some(
            value
                .as_u64()
                .and_then(|value| usize::try_from(value).ok())
                .ok_or_else(|| unknown("allow-list client capacity is invalid"))?,
        ),
    };
    Ok(Snapshot {
        state: State {
            id,
            allowed_clients: clients.into_iter().collect(),
        },
        status,
        max_allowed_clients,
    })
}

fn snapshot_value(snapshot: Snapshot) -> Result<Value, Error> {
    let mut value = serde_json::to_value(snapshot.state)
        .map_err(|_| Error::new(ErrorKind::General, "could not represent allow-list state"))?;
    if let Value::Object(object) = &mut value {
        object.insert(
            "allowListState".into(),
            serde_json::to_value(snapshot.status).map_err(|_| {
                Error::new(ErrorKind::General, "could not represent allow-list state")
            })?,
        );
        if let Some(maximum) = snapshot.max_allowed_clients {
            object.insert("maxAllowedClients".into(), json!(maximum));
        }
    }
    Ok(value)
}

fn reject_forbidden(status: AllowListStatus) -> Result<(), Error> {
    if status == AllowListStatus::Forbidden {
        Err(Error::new(
            ErrorKind::Unsupported,
            "selected allow list is forbidden",
        ))
    } else {
        Ok(())
    }
}

fn validate_port_scope(scope: PortScope) -> Result<(), Error> {
    match scope {
        PortScope::Port(0) | PortScope::Trunk(0) => {
            Err(usage("port and trunk numbers must be greater than zero"))
        }
        _ => Ok(()),
    }
}

fn usage(message: &'static str) -> Error {
    Error::new(ErrorKind::Usage, message)
}
fn unknown(message: &'static str) -> Error {
    Error::new(ErrorKind::Unverified, message)
}
