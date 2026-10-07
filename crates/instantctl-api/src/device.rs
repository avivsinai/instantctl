//! Device configuration and actions selected from complete site inventory.

pub mod reservations;

use std::{
    net::Ipv4Addr,
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use serde::Serialize;
use serde_json::{Map, Value, json};
use tokio::time::Instant;

use crate::{
    Client, Error, ErrorKind, TokenSource,
    inventory::{
        inventory_elements, inventory_path, select_device, select_device_by_id, validate_action_ack,
    },
    mutation::{FullObjectPut, Mutation, ObjectResource, Plan},
};

pub use crate::mutation::Prepared;

#[derive(Clone, Copy, Debug)]
pub enum LedMode {
    On,
    Quiet,
}

impl LedMode {
    fn api_id(self) -> &'static str {
        match self {
            Self::On => "led_on",
            Self::Quiet => "led_quiet",
        }
    }
}

#[derive(Debug)]
pub struct StaticManagementIp {
    pub address: Ipv4Addr,
    pub prefix_length: u8,
    pub gateway: Ipv4Addr,
    pub dns: Ipv4Addr,
    pub secondary_dns: Option<Ipv4Addr>,
}

pub enum ConfigChange {
    Name(String),
    LedMode(LedMode),
    ManagementIp(StaticManagementIp),
}

#[derive(Clone, Copy)]
enum ConfigField {
    Name,
    LedMode,
    ManagementIp,
}

pub(crate) struct DeviceResource<'a, T> {
    client: &'a Client<T>,
    inventory_path: String,
    device_id: String,
}

impl<'a, T: TokenSource> DeviceResource<'a, T> {
    pub(crate) async fn resolve(
        client: &'a Client<T>,
        site: &str,
        selector: &str,
    ) -> Result<(Self, Value, Value), Error> {
        let inventory_path = inventory_path(site)?;
        let inventory = client.get(&inventory_path).await?;
        let selected = select_device(&inventory, selector)?.clone();
        let device_id = selected["id"]
            .as_str()
            .ok_or_else(|| general("device identifier is missing"))?
            .to_owned();
        Ok((
            Self {
                client,
                inventory_path,
                device_id,
            },
            selected,
            inventory,
        ))
    }

    pub(crate) fn target(&self, device: &Value) -> Value {
        json!({"device_id": self.device_id, "device_name": device.get("name")})
    }

    fn object_path(&self) -> String {
        format!("{}/{}", self.inventory_path, self.device_id)
    }
}

impl<T: TokenSource> ObjectResource for DeviceResource<'_, T> {
    async fn read_object(&self) -> Result<Value, Error> {
        let inventory = self.client.get(&self.inventory_path).await?;
        Ok(select_device_by_id(&inventory, &self.device_id)?.clone())
    }

    async fn put_object(&self, body: &Value) -> Result<(), Error> {
        if body.get("id").and_then(Value::as_str) != Some(self.device_id.as_str()) {
            return Err(Error::new(
                ErrorKind::Config,
                "full-object update changed the device identity",
            ));
        }
        let response = self.client.put_full(&self.object_path(), body).await?;
        // The portal may return an empty successful response. If it returns an
        // identity, it must describe the device we actually updated.
        if let Some(id) = response.get("id")
            && id.as_str() != Some(self.device_id.as_str())
        {
            return Err(general(
                "update acknowledgment identified a different device",
            ));
        }
        Ok(())
    }
}

/// Fetch the complete inventory, change only the requested configuration fields,
/// and retain every other field in the one PUT body.
pub async fn plan_update<'a, T: TokenSource>(
    client: &'a Client<T>,
    site: &str,
    selector: &str,
    change: ConfigChange,
) -> Result<Prepared<impl Mutation<State = Value> + 'a>, Error> {
    let (field, desired) = match change {
        ConfigChange::Name(name) => {
            if name.is_empty() || name.encode_utf16().count() > 32 {
                return Err(Error::new(
                    ErrorKind::Usage,
                    "device name must contain 1 to 32 characters",
                ));
            }
            (ConfigField::Name, Value::String(name))
        }
        ConfigChange::LedMode(mode) => (ConfigField::LedMode, json!(mode.api_id())),
        ConfigChange::ManagementIp(config) => {
            validate_static_ip(&config)?;
            let mut desired = json!({
                "ipAssignmentScheme": "static",
                "staticIpAddress": config.address.to_string(),
                "staticIpAddressPrefixLength": config.prefix_length,
                "gatewayIpAddress": config.gateway.to_string(),
                "dnsIpAddress": config.dns.to_string(),
            });
            if let Some(secondary) = config.secondary_dns {
                desired["secondaryDnsIpAddress"] = json!(secondary.to_string());
            }
            (ConfigField::ManagementIp, desired)
        }
    };
    let (resource, device, inventory) = DeviceResource::resolve(client, site, selector).await?;
    if matches!(field, ConfigField::Name)
        && inventory_elements(&inventory)?
            .iter()
            .any(|other| other.get("id") != device.get("id") && other.get("name") == Some(&desired))
    {
        return Err(Error::new(
            ErrorKind::Usage,
            "device name already belongs to another device",
        ));
    }
    let target = resource.target(&device);
    let (backend, plan) = FullObjectPut::prepare(
        resource,
        device,
        move |device| observe_config(device, field),
        move |device| {
            match field {
                ConfigField::Name => device["name"] = desired,
                ConfigField::LedMode => device["ledMode"] = desired,
                ConfigField::ManagementIp => {
                    let management =
                        device["managementIpAddress"]
                            .as_object_mut()
                            .ok_or_else(|| {
                                general("management IP configuration is missing or unknown")
                            })?;
                    for key in MANAGEMENT_FIELDS {
                        management.remove(key);
                    }
                    management.extend(
                        desired
                            .as_object()
                            .ok_or_else(|| general("invalid static IP configuration"))?
                            .clone(),
                    );
                }
            }
            Ok(())
        },
    )?;
    Ok(Prepared {
        backend,
        plan,
        target,
    })
}

const MANAGEMENT_FIELDS: [&str; 6] = [
    "ipAssignmentScheme",
    "staticIpAddress",
    "staticIpAddressPrefixLength",
    "gatewayIpAddress",
    "dnsIpAddress",
    "secondaryDnsIpAddress",
];

fn observe_config(device: &Value, field: ConfigField) -> Result<Value, Error> {
    match field {
        ConfigField::Name => device
            .get("name")
            .filter(|value| value.is_string())
            .cloned()
            .ok_or_else(|| general("device name is missing or unknown")),
        ConfigField::LedMode => {
            require_capability(device, "ledMode")?;
            match device.get("ledMode").and_then(Value::as_str) {
                Some(mode @ ("led_on" | "led_quiet")) => Ok(json!(mode)),
                _ => Err(general("device LED mode is missing or unknown")),
            }
        }
        ConfigField::ManagementIp => {
            require_supported_device(device)?;
            let management = device
                .get("managementIpAddress")
                .and_then(Value::as_object)
                .ok_or_else(|| general("management IP configuration is missing or unknown"))?;
            if !matches!(
                management.get("ipAssignmentScheme").and_then(Value::as_str),
                Some("dhcp" | "pppoe" | "static" | "wwan")
            ) {
                return Err(general(
                    "management IP assignment scheme is missing or unknown",
                ));
            }
            let mut state = Map::new();
            for key in MANAGEMENT_FIELDS {
                if let Some(value) = management.get(key) {
                    // Optional DNS can be absent, empty, or null in readback.
                    if key == "secondaryDnsIpAddress"
                        && (value.is_null() || value.as_str() == Some(""))
                    {
                        continue;
                    }
                    state.insert(key.to_owned(), value.clone());
                }
            }
            Ok(Value::Object(state))
        }
    }
}

fn validate_static_ip(config: &StaticManagementIp) -> Result<(), Error> {
    if config.prefix_length > 32 {
        return Err(Error::new(
            ErrorKind::Usage,
            "IPv4 prefix length must be 0 to 32",
        ));
    }
    let mask = u32::MAX
        .checked_shl(u32::from(32 - config.prefix_length))
        .unwrap_or(0);
    if config.address == config.gateway
        || (u32::from(config.address) & mask) != (u32::from(config.gateway) & mask)
    {
        return Err(Error::new(
            ErrorKind::Usage,
            "static IPv4 address and gateway must differ and share a subnet",
        ));
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct RebootState {
    pub restart_observed: bool,
    pub back_in_service: bool,
}

pub struct Reboot<'a, T> {
    resource: DeviceResource<'a, T>,
    baseline_uptime: Option<u64>,
    requested_at: Mutex<Option<Instant>>,
    observed_offline: AtomicBool,
    restart_observed: AtomicBool,
}

pub async fn plan_reboot<'a, T: TokenSource>(
    client: &'a Client<T>,
    site: &str,
    selector: &str,
) -> Result<Prepared<Reboot<'a, T>>, Error> {
    let (resource, device, _) = DeviceResource::resolve(client, site, selector).await?;
    require_supported_device(&device)?;
    if device.get("status").and_then(Value::as_str) != Some("up") {
        return Err(Error::new(
            ErrorKind::Unsupported,
            "reboot requires a device known to be online",
        ));
    }
    let mut target = resource.target(&device);
    target["baseline_uptime_seconds"] = device
        .get("uptimeInSeconds")
        .cloned()
        .unwrap_or(Value::Null);
    Ok(Prepared {
        backend: Reboot {
            resource,
            baseline_uptime: device.get("uptimeInSeconds").and_then(Value::as_u64),
            requested_at: Mutex::new(None),
            observed_offline: AtomicBool::new(false),
            restart_observed: AtomicBool::new(false),
        },
        plan: Plan {
            current: RebootState {
                restart_observed: false,
                back_in_service: back_in_service(&device),
            },
            desired: RebootState {
                restart_observed: true,
                back_in_service: true,
            },
        },
        target,
    })
}

impl<T> Reboot<'_, T> {
    /// Retain restart evidence even if a later inventory request fails.
    pub fn restart_observed(&self) -> bool {
        self.restart_observed.load(Ordering::Relaxed)
    }
}

impl<T: TokenSource> Mutation for Reboot<'_, T> {
    type State = RebootState;

    async fn read(&self) -> Result<RebootState, Error> {
        let requested_at = *self
            .requested_at
            .lock()
            .map_err(|_| general("reboot state lock failed"))?;
        let Some(requested_at) = requested_at else {
            return Ok(RebootState {
                restart_observed: false,
                back_in_service: false,
            });
        };
        let device = self.resource.read_object().await?;
        match device.get("status").and_then(Value::as_str) {
            Some("down") => self.observed_offline.store(true, Ordering::Relaxed),
            Some("up") => {
                let reset = self
                    .baseline_uptime
                    .zip(device.get("uptimeInSeconds").and_then(Value::as_u64))
                    .is_some_and(|(baseline, fresh)| {
                        fresh < baseline && (fresh as f64) < requested_at.elapsed().as_secs_f64()
                    });
                if reset || self.observed_offline.load(Ordering::Relaxed) {
                    self.restart_observed.store(true, Ordering::Relaxed);
                }
            }
            _ => return Err(general("reboot readback status is missing or unknown")),
        }
        Ok(RebootState {
            restart_observed: self.restart_observed(),
            back_in_service: back_in_service(&device),
        })
    }

    async fn write(&self, desired: &RebootState) -> Result<(), Error> {
        if !desired.restart_observed || !desired.back_in_service {
            return Err(Error::new(
                ErrorKind::Config,
                "reboot requires a restart and return to service",
            ));
        }
        *self
            .requested_at
            .lock()
            .map_err(|_| general("reboot state lock failed"))? = Some(Instant::now());
        let response = self
            .resource
            .client
            .action(
                &self.resource.inventory_path,
                &self.resource.device_id,
                "reboot",
                &json!({"id": self.resource.device_id}),
            )
            .await?;
        validate_action_ack(&response, &self.resource.device_id)
    }
}

fn back_in_service(device: &Value) -> bool {
    device.get("status").and_then(Value::as_str) == Some("up")
        && match device.get("operationalState") {
            None | Some(Value::Null) => true,
            Some(value) => value.as_str() == Some("active"),
        }
}

struct Forget<'a, T> {
    resource: DeviceResource<'a, T>,
}

pub async fn plan_forget<'a, T: TokenSource>(
    client: &'a Client<T>,
    site: &str,
    selector: &str,
) -> Result<Prepared<impl Mutation<State = bool> + 'a>, Error> {
    let (resource, device, _) = DeviceResource::resolve(client, site, selector).await?;
    require_supported_device(&device)?;
    let target = resource.target(&device);
    Ok(Prepared {
        backend: Forget { resource },
        plan: Plan {
            current: true,
            desired: false,
        },
        target,
    })
}

impl<T: TokenSource> Mutation for Forget<'_, T> {
    type State = bool;

    async fn read(&self) -> Result<bool, Error> {
        let inventory = self
            .resource
            .client
            .get(&self.resource.inventory_path)
            .await?;
        Ok(inventory_elements(&inventory)?.iter().any(|device| {
            device.get("id").and_then(Value::as_str) == Some(self.resource.device_id.as_str())
        }))
    }

    async fn write(&self, desired: &bool) -> Result<(), Error> {
        if *desired {
            return Err(Error::new(
                ErrorKind::Config,
                "forget requires device removal",
            ));
        }
        self.resource
            .client
            .delete(&self.resource.object_path())
            .await?;
        Ok(())
    }
}

pub async fn power_usage<T: TokenSource>(
    client: &Client<T>,
    site: &str,
    selector: &str,
) -> Result<Value, Error> {
    let (resource, device, _) = DeviceResource::resolve(client, site, selector).await?;
    require_capability(&device, "powerUsage")?;
    client
        .get(&format!("{}/powerUsage", resource.object_path()))
        .await
}

pub async fn details<T: TokenSource>(
    client: &Client<T>,
    site: &str,
    selector: &str,
) -> Result<Value, Error> {
    let (resource, _, _) = DeviceResource::resolve(client, site, selector).await?;
    client
        .get(&format!(
            "/sites/{}/deviceDetails/{}",
            site.to_ascii_lowercase(),
            resource.device_id
        ))
        .await
}

fn require_capability(device: &Value, capability: &str) -> Result<(), Error> {
    match device
        .get("capabilities")
        .and_then(|value| value.get(capability))
        .and_then(Value::as_bool)
    {
        Some(true) => Ok(()),
        Some(false) => Err(Error::new(
            ErrorKind::Unsupported,
            format!("device does not support {capability}"),
        )),
        None => Err(general("device capability is missing or unknown")),
    }
}

fn require_supported_device(device: &Value) -> Result<(), Error> {
    let kind = device.get("deviceType").and_then(Value::as_str);
    let role = device.get("deviceRole").and_then(Value::as_str);
    if kind == Some("gateway") || role == Some("gateway") {
        return Err(Error::new(
            ErrorKind::Unsupported,
            "gateway devices require a different operation",
        ));
    }
    match kind {
        Some("accessPoint" | "switch") => {}
        _ => return Err(general("device type is missing or unknown")),
    }
    // Switch inventory can omit the AP/gateway role field. Do not interpret an
    // unknown AP role or an explicit unknown switch role as permission to write.
    match role {
        Some("accessPoint") => Ok(()),
        None if kind == Some("switch") && device.get("deviceRole").is_none_or(Value::is_null) => {
            Ok(())
        }
        _ => Err(general("device role is missing or unknown")),
    }
}

fn general(message: &'static str) -> Error {
    Error::new(ErrorKind::General, message)
}
