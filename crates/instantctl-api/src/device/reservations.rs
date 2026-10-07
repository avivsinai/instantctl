//! Device DHCP reservations use inventory metadata, not the client reservation list.

use std::{collections::HashSet, net::Ipv4Addr};

use serde::Serialize;
use serde_json::{Value, json};

use super::DeviceResource;
use crate::{
    Client, Error, ErrorKind, TokenSource,
    client::{network::validate_dhcp_scope, reads::is_mac},
    inventory::{select_device_by_id, validate_action_ack},
    mutation::{Mutation, Plan, Prepared},
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Reservation {
    pub network_id: String,
    pub ip_address: Ipv4Addr,
}

pub struct ReservationMutation<'a, T> {
    client: &'a Client<T>,
    path: String,
    device_id: String,
    mac: String,
    desired: Option<Reservation>,
}

/// `None` removes the existing reservation; changing its address requires removal first.
pub async fn plan<'a, T: TokenSource>(
    client: &'a Client<T>,
    site: &str,
    selector: &str,
    desired: Option<Reservation>,
) -> Result<Prepared<ReservationMutation<'a, T>>, Error> {
    let (resource, device, inventory) = DeviceResource::resolve(client, site, selector).await?;
    let mac = string(&device, "macAddress")?.to_owned();
    let current = observe(&inventory, &resource.device_id, &mac)?;
    if let Some(reservation) = &desired {
        if current.is_some() {
            return Err(usage(
                "remove the device's existing IP reservation before adding one",
            ));
        }
        match device.get("canReserveIpAddress").and_then(Value::as_bool) {
            Some(true) => {}
            Some(false) => {
                return Err(Error::new(
                    ErrorKind::Unsupported,
                    "device cannot reserve an IP address",
                ));
            }
            None => return Err(unknown("device IP reservation capability is unknown")),
        }
        let scope = scopes(&inventory)?
            .iter()
            .find(|scope| {
                scope.get("networkId").and_then(Value::as_str)
                    == Some(reservation.network_id.as_str())
            })
            .ok_or_else(|| Error::new(ErrorKind::NotFound, "DHCP network was not found"))?;
        if scope.get("isDhcpServer").and_then(Value::as_bool) != Some(true) {
            return Err(Error::new(
                ErrorKind::Unsupported,
                "selected network is not a DHCP server",
            ));
        }
        let mut candidate = scope
            .get("dhcpScope")
            .cloned()
            .ok_or_else(|| unknown("DHCP scope is unknown"))?;
        let entries = entries_mut(&mut candidate)?;
        if entries
            .iter()
            .any(|row| ip(row).ok() == Some(reservation.ip_address))
        {
            return Err(usage("requested IP address is already reserved"));
        }
        entries.push(json!({"macAddress":mac,"ipAddress":reservation.ip_address}));
        validate_dhcp_scope(&candidate)?;
        reject_active_conflict(scope, &mac, reservation.ip_address)?;
    } else if current.is_none() {
        return Err(usage("device has no IP reservation to remove"));
    }
    let target = resource.target(&device);
    Ok(Prepared {
        backend: ReservationMutation {
            client,
            path: resource.inventory_path,
            device_id: resource.device_id,
            mac,
            desired: desired.clone(),
        },
        plan: Plan { current, desired },
        target,
    })
}

impl<T: TokenSource> Mutation for ReservationMutation<'_, T> {
    type State = Option<Reservation>;

    async fn read(&self) -> Result<Self::State, Error> {
        observe(
            &self.client.get(&self.path).await?,
            &self.device_id,
            &self.mac,
        )
    }

    async fn write(&self, desired: &Self::State) -> Result<(), Error> {
        if desired != &self.desired {
            return Err(usage(
                "requested reservation differs from the prepared plan",
            ));
        }
        let entries: Vec<Value> = desired
            .iter()
            .map(|reservation| {
                json!({
                    "networkId": reservation.network_id,
                    "ipAddress": reservation.ip_address,
                })
            })
            .collect();
        let response = self
            .client
            .action(
                &self.path,
                &self.device_id,
                "reserveIp",
                &json!({"ipReservations":entries}),
            )
            .await?;
        validate_action_ack(&response, &self.device_id)
    }
}

fn observe(inventory: &Value, device_id: &str, mac: &str) -> Result<Option<Reservation>, Error> {
    let device = select_device_by_id(inventory, device_id)?;
    if !string(device, "macAddress")?.eq_ignore_ascii_case(mac) {
        return Err(unknown(
            "device identity changed during reservation readback",
        ));
    }
    let flat = match device.get("reservedIpAddress") {
        Some(Value::Null) => None,
        Some(Value::String(address)) => Some(
            address
                .parse::<Ipv4Addr>()
                .map_err(|_| unknown("device reserved IP address is invalid"))?,
        ),
        _ => return Err(unknown("device reserved IP address is unknown")),
    };
    let mut observed = None;
    for scope in scopes(inventory)? {
        match scope.get("isDhcpServer").and_then(Value::as_bool) {
            Some(false) => continue,
            Some(true) => {}
            None => return Err(unknown("DHCP server state is unknown")),
        }
        let rows = scope
            .pointer("/dhcpScope/ipReservations")
            .and_then(Value::as_array)
            .ok_or_else(|| unknown("DHCP reservation list is unknown"))?;
        let mut addresses = HashSet::new();
        let mut identities = HashSet::new();
        for row in rows {
            let row_mac = string(row, "macAddress")?;
            if !is_mac(row_mac) || !identities.insert(row_mac.to_ascii_lowercase()) {
                return Err(unknown(
                    "DHCP reservation identity is invalid or duplicated",
                ));
            }
            let address = ip(row)?;
            if !addresses.insert(address) {
                return Err(unknown("DHCP reservation IP address is duplicated"));
            }
            let matches = row_mac.eq_ignore_ascii_case(mac);
            if let Some(id) = row.get("deviceId").filter(|id| !id.is_null()) {
                let id = id
                    .as_str()
                    .filter(|id| !id.is_empty())
                    .ok_or_else(|| unknown("DHCP reservation device identity is invalid"))?;
                if (id == device_id) != matches {
                    return Err(unknown(
                        "DHCP reservation device and MAC identities disagree",
                    ));
                }
            }
            if matches {
                if observed.is_some() {
                    return Err(unknown(
                        "device has reservations in more than one DHCP scope",
                    ));
                }
                observed = Some(Reservation {
                    network_id: string(scope, "networkId")?.to_owned(),
                    ip_address: address,
                });
            }
        }
    }
    if flat != observed.as_ref().map(|reservation| reservation.ip_address) {
        return Err(unknown(
            "inventory and DHCP scope disagree about the device reservation",
        ));
    }
    Ok(observed)
}

fn scopes(inventory: &Value) -> Result<&Vec<Value>, Error> {
    let scopes = inventory
        .pointer("/metaData/networkScopes")
        .and_then(Value::as_array)
        .ok_or_else(|| unknown("inventory DHCP network scopes are unknown"))?;
    let mut ids = HashSet::new();
    for scope in scopes {
        let id = string(scope, "networkId")?;
        if id.is_empty() || !ids.insert(id) {
            return Err(unknown(
                "DHCP network scope identity is missing or duplicated",
            ));
        }
    }
    Ok(scopes)
}

fn entries_mut(scope: &mut Value) -> Result<&mut Vec<Value>, Error> {
    scope
        .get_mut("ipReservations")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| unknown("DHCP reservation list is unknown"))
}

fn reject_active_conflict(scope: &Value, mac: &str, address: Ipv4Addr) -> Result<(), Error> {
    let info = scope
        .get("ipReservationInfo")
        .ok_or_else(|| unknown("DHCP reservation conflict information is unknown"))?;
    for (key, device) in [("clients", false), ("siteDevices", true)] {
        let rows = info
            .get(key)
            .and_then(Value::as_array)
            .ok_or_else(|| unknown("DHCP reservation conflict list is unknown"))?;
        for row in rows {
            let identity = string(row, "macAddress")?;
            if !is_mac(identity) {
                return Err(unknown("DHCP conflict MAC address is invalid"));
            }
            if identity.eq_ignore_ascii_case(mac) {
                continue;
            }
            let candidate = match row.get("ipAddress") {
                Some(Value::Null) | None => continue,
                Some(Value::String(value)) => value
                    .parse::<Ipv4Addr>()
                    .map_err(|_| unknown("DHCP conflict IP address is invalid"))?,
                _ => return Err(unknown("DHCP conflict IP address is invalid")),
            };
            let online = row
                .get("isOnline")
                .and_then(Value::as_bool)
                .ok_or_else(|| unknown("DHCP conflict online state is unknown"))?;
            let occupied = if device {
                online || string(row, "ipAssignmentScheme")? != "dhcp"
            } else {
                online
                    || row
                        .get("hasActiveLease")
                        .and_then(Value::as_bool)
                        .ok_or_else(|| unknown("DHCP conflict lease state is unknown"))?
            };
            if occupied && candidate == address {
                return Err(usage(
                    "requested IP address conflicts with a client or device",
                ));
            }
        }
    }
    Ok(())
}

fn string<'a>(value: &'a Value, key: &str) -> Result<&'a str, Error> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| unknown("DHCP reservation response is missing a required string"))
}
fn ip(value: &Value) -> Result<Ipv4Addr, Error> {
    string(value, "ipAddress")?
        .parse()
        .map_err(|_| unknown("DHCP reservation IP address is invalid"))
}
fn unknown(message: &str) -> Error {
    Error::new(ErrorKind::Unverified, message)
}
fn usage(message: &str) -> Error {
    Error::new(ErrorKind::Usage, message)
}
