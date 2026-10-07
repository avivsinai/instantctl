//! Observed Instant On read routes and response models.
use std::collections::HashSet;

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Map, Value};

use crate::{Client, Error, ErrorKind, TokenSource};

pub mod local_health;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Site {
    pub id: String,
    pub name: Option<String>,
    pub status: Option<String>,
    pub health: Option<Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Device {
    pub id: String,
    pub mac_address: String,
    pub name: Option<String>,
    pub device_type: Option<String>,
    pub model: Option<String>,
    pub device_model: Option<String>,
    pub sku: Option<String>,
    pub status: Option<String>,
    pub health: Option<Value>,
    pub ip_address: Option<String>,
    pub device_software_version: Option<String>,
    pub capabilities: Option<Value>,
    pub ethernet_ports: Option<Vec<EthernetPort>>,
    pub trunk_ports: Option<Vec<TrunkPort>>,
    pub radios: Option<Vec<Radio>>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EthernetPort {
    pub faceplate_port_number: Option<u64>,
    pub port_number: Option<u64>,
    pub name: Option<String>,
    pub is_link_up: Option<bool>,
    pub speed: Option<Value>,
    pub is_uplink: Option<bool>,
    pub user_deactivated: Option<bool>,
    pub trunk_number: Option<u64>,
    pub power_provided_in_milliwatts: Option<f64>,
    pub capabilities: Option<Value>,
    pub port_data_traffic: Option<PortTraffic>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PortTraffic {
    pub downstream_throughput_in_bits_per_second: Option<f64>,
    pub upstream_throughput_in_bits_per_second: Option<f64>,
    pub downstream_data_transferred_in_bytes_in_last24_hours: Option<u64>,
    pub upstream_data_transferred_in_bytes_in_last24_hours: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrunkPort {
    pub trunk_number: Option<u64>,
    pub name: Option<String>,
    pub trunk_type: Option<Value>,
    pub user_deactivated: Option<bool>,
    pub port_profile_id: Option<String>,
    #[serde(flatten)]
    pub additional_fields: Map<String, Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Radio {
    pub band: Option<Value>,
    pub channel: Option<u64>,
    pub channel_width: Option<Value>,
    pub tx_power_eirp_in_dbm: Option<f64>,
    pub utilization_percent: Option<f64>,
    pub wireless_clients_count: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientSummary {
    pub id: String,
    pub mac_address: String,
    pub name: Option<String>,
    pub client_type: Option<String>,
    pub status: Option<String>,
    pub health: Option<Value>,
    pub ip_address: Option<String>,
    pub device_name: Option<String>,
    pub device_id: Option<String>,
    pub wireless_network_name: Option<String>,
    pub wireless_network_id: Option<String>,
    pub wireless_radio_id: Option<String>,
    pub wireless_band: Option<Value>,
    pub wireless_bands: Option<Value>,
    pub snr_in_db: Option<f64>,
    pub signal_quality: Option<Value>,
    pub connected_to_ports: Option<Value>,
    pub state_duration_in_seconds: Option<u64>,
    pub last_state_change: Option<Value>,
    pub downstream_throughput_in_bits_per_second: Option<f64>,
    pub upstream_throughput_in_bits_per_second: Option<f64>,
    pub data_traffic: Option<ClientTraffic>,
    pub is_blockable: Option<bool>,
    pub is_watchable: Option<bool>,
    pub can_reserve_ip_address: Option<bool>,
    pub is_watchlisted: Option<bool>,
    pub reserved_ip_address: Option<String>,
    pub classification: Option<Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientTraffic {
    pub downstream_data_transferred_in_bytes_in_last24_hours: Option<u64>,
    pub upstream_data_transferred_in_bytes_in_last24_hours: Option<u64>,
    pub data_transferred_period_enum: Option<Value>,
}

pub fn valid_site_id(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| {
            if [8, 13, 18, 23].contains(&index) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

fn route(site: &str, resource: &str) -> Result<String, Error> {
    if !valid_site_id(site) {
        return Err(Error::new(ErrorKind::Config, "--site must be a UUID"));
    }
    Ok(format!("/sites/{site}/{resource}"))
}

impl<T: TokenSource> Client<T> {
    pub async fn sites(&self) -> Result<Vec<Site>, Error> {
        let payload = self.get("/sites").await?;
        let sites: Vec<Site> = parse_elements(&payload)?;
        let mut seen = HashSet::new();
        for site in &sites {
            if !valid_site_id(&site.id) || !seen.insert(site.id.to_ascii_lowercase()) {
                return Err(incomplete(
                    "site collection contains invalid or duplicate identities",
                ));
            }
        }
        Ok(sites)
    }

    pub async fn inventory(&self, site: &str) -> Result<Vec<Device>, Error> {
        let payload = self.get(&route(site, "inventory")?).await?;
        if payload.get("kind").and_then(Value::as_str) != Some("resourceList") {
            return Err(incomplete("inventory response is not a resourceList"));
        }
        let devices: Vec<Device> = parse_elements(&payload)?;
        let total = payload.get("totalCount").and_then(Value::as_u64);
        let matching = payload.get("matchingFilterCount").and_then(Value::as_u64);
        if total != Some(devices.len() as u64) || matching != total {
            return Err(incomplete(
                "inventory is partial; counts do not match returned devices",
            ));
        }
        let pending = payload
            .get("pendingAvailability")
            .ok_or_else(|| incomplete("inventory is missing pendingAvailability"))?;
        if !(pending.is_null()
            || pending == false
            || pending == 0
            || pending.as_array().is_some_and(Vec::is_empty)
            || pending.as_object().is_some_and(Map::is_empty))
        {
            return Err(incomplete("inventory has pending availability"));
        }
        unique(devices.iter().map(|device| device.id.as_str()), true)?;
        unique(
            devices.iter().map(|device| device.mac_address.as_str()),
            true,
        )?;
        Ok(devices)
    }

    /// The API supplies no authoritative total for this collection.
    pub async fn clients(&self, site: &str) -> Result<Vec<ClientSummary>, Error> {
        let payload = self.get(&route(site, "clientSummary")?).await?;
        parse_clients(&payload)
    }

    pub async fn capabilities(&self, site: &str) -> Result<Vec<String>, Error> {
        let payload = self.get(&route(site, "capabilities")?).await?;
        let items = payload
            .get("capabilities")
            .and_then(Value::as_array)
            .ok_or_else(|| incomplete("capabilities response has an invalid shape"))?;
        serde_json::from_value(Value::Array(items.clone()))
            .map_err(|_| incomplete("capabilities response has an invalid shape"))
    }
}

pub fn is_mac(value: &str) -> bool {
    value.len() == 17
        && value.bytes().enumerate().all(|(index, byte)| {
            if index % 3 == 2 {
                byte == b':'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

pub(super) fn parse_clients(payload: &Value) -> Result<Vec<ClientSummary>, Error> {
    if payload.get("kind").and_then(Value::as_str) != Some("clientSummaries") {
        return Err(incomplete("client summary response has an unexpected kind"));
    }
    let clients: Vec<ClientSummary> = parse_elements(payload)?;
    unique(clients.iter().map(|client| client.id.as_str()), false)?;
    unique(
        clients.iter().map(|client| client.mac_address.as_str()),
        true,
    )?;
    Ok(clients)
}

pub(super) fn unique<'a>(values: impl Iterator<Item = &'a str>, mac: bool) -> Result<(), Error> {
    let mut seen = HashSet::new();
    for value in values {
        if value.is_empty() || (mac && !is_mac(value)) || !seen.insert(value.to_ascii_lowercase()) {
            return Err(incomplete(
                "collection contains invalid or duplicate identities",
            ));
        }
    }
    Ok(())
}

pub(super) fn parse_elements<T: DeserializeOwned>(payload: &Value) -> Result<Vec<T>, Error> {
    let items = payload
        .get("elements")
        .and_then(Value::as_array)
        .filter(|items| items.iter().all(Value::is_object))
        .ok_or_else(|| incomplete("collection has no complete elements list"))?;
    for field in ["totalCount", "matchingFilterCount"] {
        if let Some(count) = payload.get(field)
            && count.as_u64() != Some(items.len() as u64)
        {
            return Err(incomplete("collection is partial or has invalid counts"));
        }
    }
    if has_pagination_marker(payload.get("metaData").unwrap_or(&Value::Null)) {
        return Err(incomplete("collection indicates more pages are available"));
    }
    serde_json::from_value(Value::Array(items.clone()))
        .map_err(|_| incomplete("collection entry has an invalid shape"))
}

fn has_pagination_marker(value: &Value) -> bool {
    match value {
        Value::Object(object) => object.iter().any(|(key, child)| {
            let key = key.to_ascii_lowercase().replace('_', "");
            let next = key.contains("next")
                && ["page", "cursor", "token"]
                    .iter()
                    .any(|part| key.contains(part));
            (next && !matches!(child, Value::Null | Value::Bool(false)) && child != "")
                || (["hasmore", "istruncated", "partial"].contains(&key.as_str()) && child == true)
                || has_pagination_marker(child)
        }),
        Value::Array(items) => items.iter().any(has_pagination_marker),
        _ => false,
    }
}

fn incomplete(message: &str) -> Error {
    Error::new(ErrorKind::Unverified, message)
}

#[cfg(test)]
pub(crate) mod tests;
