//! Switch port changes use the complete inventory object and a fixed port identity.
use crate::{
    Client, Error, ErrorKind, TokenSource,
    device::{DeviceResource, Prepared},
    inventory::{inventory_elements, inventory_path, select_device_by_id},
    mutation::{FullObjectPut, Mutation, Plan},
};
use serde_json::{Value, json};
use std::collections::HashSet;

fn usage(message: &str) -> Error {
    Error::new(ErrorKind::Usage, message)
}
fn unknown(message: &str) -> Error {
    Error::new(ErrorKind::Unverified, message)
}
fn missing(message: &str) -> Error {
    Error::new(ErrorKind::NotFound, message)
}

#[derive(Clone, Debug)]
pub(crate) struct ProtectedPort {
    pub(crate) device: String,
    pub(crate) faceplate: u64,
}

/// Validate a profile policy selector without loading credentials or inventory.
pub fn validate_protected_port_entry(entry: &str) -> Result<(), Error> {
    parse_protected_port_entry(entry).map(|_| ())
}

pub(crate) fn parse_protected_port_entry(entry: &str) -> Result<ProtectedPort, Error> {
    let (device, faceplate) = entry
        .rsplit_once(':')
        .ok_or_else(|| usage("protected port requires <switch>:<faceplate>"))?;
    let faceplate = faceplate
        .parse::<u64>()
        .ok()
        .filter(|n| *n > 0)
        .ok_or_else(|| usage("protected port faceplate must be positive"))?;
    let mac = crate::inventory::is_mac_address(device);
    if device.trim().is_empty()
        || device.chars().any(char::is_control)
        || (!mac && device.encode_utf16().count() > 32)
        || (!mac && device.split(':').count() == 6)
    {
        return Err(usage(
            "protected port requires a valid MAC or exact switch name",
        ));
    }
    Ok(ProtectedPort {
        device: if mac {
            device.to_ascii_lowercase()
        } else {
            device.into()
        },
        faceplate,
    })
}

pub(crate) fn parse_protected_ports(entries: &[String]) -> Result<Vec<ProtectedPort>, Error> {
    let mut ports = Vec::with_capacity(entries.len());
    for entry in entries {
        let (device, faceplate) = entry.rsplit_once(':').ok_or_else(|| {
            Error::new(
                ErrorKind::Config,
                "protected_ports entries require <switch>:<faceplate>",
            )
        })?;
        let faceplate = faceplate
            .parse::<u64>()
            .ok()
            .filter(|n| *n > 0)
            .ok_or_else(|| {
                Error::new(
                    ErrorKind::Config,
                    "protected_ports faceplate numbers must be positive",
                )
            })?;
        if device.trim().is_empty() || device.chars().any(char::is_control) {
            return Err(Error::new(
                ErrorKind::Config,
                "protected_ports switch selectors must be nonempty without controls",
            ));
        }
        ports.push(ProtectedPort {
            device: device.into(),
            faceplate,
        });
    }
    Ok(ports)
}

#[cfg(any(target_os = "macos", test))]
pub(crate) fn validate_protected_ports(entries: &[String]) -> Result<(), Error> {
    parse_protected_ports(entries).map(|_| ())
}

impl ProtectedPort {
    fn matches(&self, device: &Value, faceplate: u64) -> Result<bool, Error> {
        if self.faceplate != faceplate {
            return Ok(false);
        }
        if crate::inventory::is_mac_address(&self.device) {
            let mac = device
                .get("macAddress")
                .and_then(Value::as_str)
                .ok_or_else(|| unknown("switch MAC is unknown for protected port matching"))?;
            Ok(mac.eq_ignore_ascii_case(&self.device))
        } else {
            let name = device
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| unknown("switch name is unknown for protected port matching"))?;
            Ok(name == self.device)
        }
    }
}
fn array<'a>(value: &'a Value, key: &str) -> Result<&'a Vec<Value>, Error> {
    value
        .get(key)
        .and_then(Value::as_array)
        .filter(|rows| rows.iter().all(Value::is_object))
        .ok_or_else(|| unknown("port collection is missing or malformed"))
}
fn number(value: &Value, key: &str) -> Result<u64, Error> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| unknown("port identity is unknown"))
}
fn boolean(value: &Value, key: &str) -> Result<bool, Error> {
    value
        .get(key)
        .and_then(Value::as_bool)
        .ok_or_else(|| unknown("port safety or configuration boolean is unknown"))
}
fn switch(device: &Value) -> Result<(), Error> {
    if device["deviceType"] != "switch" {
        return Err(Error::new(
            ErrorKind::Unsupported,
            "operation requires a switch",
        ));
    }
    Ok(())
}
fn ports(device: &Value) -> Result<&Vec<Value>, Error> {
    let rows = array(device, "ethernetPorts")?;
    let mut api = HashSet::new();
    let mut face = HashSet::new();
    for row in rows {
        let n = number(row, "portNumber")?;
        let f = number(row, "faceplatePortNumber")?;
        if f == 0 || !api.insert(n) || !face.insert(f) {
            return Err(unknown("port identities are invalid or duplicated"));
        }
    }
    Ok(rows)
}
pub(crate) fn select_port(device: &Value, faceplate: u64) -> Result<&Value, Error> {
    if faceplate == 0 {
        return Err(usage("faceplate port number must be positive"));
    }
    ports(device)?
        .iter()
        .find(|p| p["faceplatePortNumber"] == faceplate)
        .ok_or_else(|| missing("no port matches the faceplate number"))
}
fn trunk(port: &Value) -> Result<Option<u64>, Error> {
    match port.get("trunkNumber") {
        Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .filter(|n| *n > 0)
            .map(Some)
            .ok_or_else(|| unknown("port LAG membership is unknown")),
        None => Err(unknown("port LAG membership is unknown")),
    }
}
fn guard_port(
    device: &Value,
    port: &Value,
    configured: &[ProtectedPort],
    force: bool,
    cycling: bool,
) -> Result<(), Error> {
    let face = number(port, "faceplatePortNumber")?;
    let dedicated = boolean(port, "isDedicatedUplink")?;
    let uplink = boolean(port, "isUplink")?;
    let lag = trunk(port)?;
    let mut protected = uplink || dedicated || lag.is_some();
    for entry in configured {
        protected |= entry.matches(device, face)?;
    }
    if protected && (cycling || !force) {
        return Err(usage(if cycling {
            "power-cycle refuses an uplink, LAG, or protected port"
        } else {
            "changes to an uplink, LAG, or protected port require --force"
        }));
    }
    Ok(())
}
pub(crate) fn guard_faceplate(
    device: &Value,
    faceplate: u64,
    configured: &[ProtectedPort],
    force: bool,
) -> Result<u64, Error> {
    switch(device)?;
    let port = select_port(device, faceplate)?;
    guard_port(device, port, configured, force, false)?;
    number_of(port)
}
pub(crate) fn guard_site(
    inventory: &Value,
    configured: &[ProtectedPort],
    force: bool,
) -> Result<(), Error> {
    for device in inventory_elements(inventory)? {
        if !matches!(
            device["deviceType"].as_str(),
            Some("switch" | "accessPoint" | "gateway")
        ) {
            return Err(unknown(
                "inventory device type is unknown; cannot establish affected ports",
            ));
        }
        if device["deviceType"] == "switch" {
            for port in ports(device)? {
                guard_port(device, port, configured, force, false)?;
            }
        }
    }
    Ok(())
}
fn guard_profile_trunk(
    device: &Value,
    n: u64,
    configured: &[ProtectedPort],
    force: bool,
) -> Result<(), Error> {
    select_trunk(device, n)?;
    if !force {
        return Err(usage("LAG profile changes require --force"));
    }
    let members = lag_members(device, n)?;
    for port in ports(device)? {
        if members.contains(&number_of(port)?) {
            guard_port(device, port, configured, force, false)?;
        }
    }
    Ok(())
}
pub(crate) fn guard_profile(
    inventory: &Value,
    id: &str,
    assignments: &Value,
    configured: &[ProtectedPort],
    force: bool,
) -> Result<(), Error> {
    let mapping = assignments
        .as_object()
        .ok_or_else(|| unknown("profile device assignments are unknown"))?;
    let inventory_devices = inventory_elements(inventory)?;
    for (device_id, assignments) in mapping {
        let rows = assignments
            .as_array()
            .filter(|rows| rows.iter().all(Value::is_object))
            .ok_or_else(|| unknown("profile port assignments are malformed"))?;
        for assignment in rows {
            if !boolean(assignment, "usePortProfile")? {
                continue;
            }
            let device = inventory_devices
                .iter()
                .find(|d| {
                    d["id"]
                        .as_str()
                        .is_some_and(|id| id.eq_ignore_ascii_case(device_id))
                })
                .ok_or_else(|| {
                    unknown("assigned profile device is absent from complete inventory")
                })?;
            switch(device)?;
            match (assignment.get("portNumber"), assignment.get("trunkNumber")) {
                (Some(api), None) => {
                    let api = api
                        .as_u64()
                        .ok_or_else(|| unknown("profile API port identity is unknown"))?;
                    let port = ports(device)?
                        .iter()
                        .find(|p| p["portNumber"] == api)
                        .ok_or_else(|| unknown("assigned profile port is absent from inventory"))?;
                    guard_port(device, port, configured, force, false)?;
                }
                (None, Some(n)) => {
                    let n = n
                        .as_u64()
                        .filter(|n| *n > 0)
                        .ok_or_else(|| unknown("profile LAG identity is unknown"))?;
                    guard_profile_trunk(device, n, configured, force)?;
                }
                _ => {
                    return Err(unknown(
                        "profile assignment needs exactly one port or LAG identity",
                    ));
                }
            }
        }
    }
    for device in inventory_elements(inventory)? {
        if !matches!(
            device["deviceType"].as_str(),
            Some("switch" | "accessPoint" | "gateway")
        ) {
            return Err(unknown(
                "inventory device type is unknown; cannot establish profile assignment safety",
            ));
        }
        if device["deviceType"] == "switch" {
            for port in ports(device)? {
                let assignment = port
                    .get("portProfileId")
                    .ok_or_else(|| unknown("port profile assignment is unknown"))?;
                if !assignment.is_null() && !assignment.is_string() {
                    return Err(unknown("port profile assignment is malformed"));
                }
                if assignment == id {
                    guard_port(device, port, configured, force, false)?;
                }
            }
            for slot in array(device, "trunkPorts")? {
                let assignment = slot
                    .get("portProfileId")
                    .ok_or_else(|| unknown("LAG profile assignment is unknown"))?;
                if !assignment.is_null() && !assignment.is_string() {
                    return Err(unknown("LAG profile assignment is malformed"));
                }
                if assignment == id {
                    guard_profile_trunk(device, number(slot, "trunkNumber")?, configured, force)?;
                }
            }
        }
    }
    Ok(())
}

#[derive(Clone, Debug, Default)]
pub struct PortPatch {
    pub name: Option<String>,
    pub enabled: Option<bool>,
    pub profile: Option<String>,
    pub poe_schedule: Option<bool>,
    pub poe_mode: Option<String>,
    pub poe_priority: Option<String>,
    pub poe_management: Option<String>,
    pub speed_duplex: Option<String>,
}
impl PortPatch {
    fn fields(&self) -> Result<Vec<(&'static str, Value)>, Error> {
        let mut fields = Vec::new();
        if let Some(name) = &self.name {
            if name.encode_utf16().count() > 32 || name.chars().any(char::is_control) {
                return Err(usage(
                    "port name must contain at most 32 characters without controls",
                ));
            }
            fields.push(("name", json!(name)));
        }
        if let Some(enabled) = self.enabled {
            fields.push(("userDeactivated", json!(!enabled)));
        }
        if let Some(profile) = &self.profile {
            if profile.is_empty() {
                return Err(usage("profile identifier must not be empty"));
            }
            fields.push(("portProfileId", json!(profile)));
        }
        if let Some(enabled) = self.poe_schedule {
            fields.push(("usePoeSchedule", json!(enabled)));
        }
        for (key, value, allowed) in [
            (
                "poePowerMode",
                self.poe_mode.as_ref(),
                &["alwaysOn", "quick", "normal"][..],
            ),
            (
                "poePriority",
                self.poe_priority.as_ref(),
                &["low", "high", "critical"][..],
            ),
            (
                "poePowerManagementMode",
                self.poe_management.as_ref(),
                &["none", "class-based", "usage-based"][..],
            ),
        ] {
            if let Some(value) = value {
                if !allowed.contains(&value.as_str()) {
                    return Err(usage("unknown PoE configuration value"));
                }
                fields.push((key, json!(value)));
            }
        }
        if let Some(speed) = &self.speed_duplex {
            if speed == "automatic" {
                fields.push(("speedDuplexMode", json!("automatic")));
            } else {
                if ![
                    "10MbpsHalfDuplex",
                    "10MbpsFullDuplex",
                    "100MbpsHalfDuplex",
                    "100MbpsFullDuplex",
                    "1GbpsFullDuplex",
                    "2_5GbpsFullDuplex",
                    "10GbpsFullDuplex",
                ]
                .contains(&speed.as_str())
                {
                    return Err(usage("unknown speed/duplex value"));
                }
                fields.push(("speedDuplexMode", json!("manual")));
                fields.push(("speedDuplex", json!(speed)));
            }
        }
        if fields.is_empty() {
            return Err(usage("port set requires at least one configuration option"));
        }
        Ok(fields)
    }
}
fn observe_port(device: &Value, api: u64, keys: &[&str]) -> Result<Value, Error> {
    let port = ports(device)?
        .iter()
        .find(|p| p["portNumber"] == api)
        .ok_or_else(|| unknown("port disappeared during readback"))?;
    let mut state = serde_json::Map::new();
    for key in keys {
        state.insert(
            (*key).into(),
            port.get(*key)
                .cloned()
                .or_else(|| {
                    (*key == "speedDuplex" && port["speedDuplexMode"] == "automatic")
                        .then_some(Value::Null)
                })
                .ok_or_else(|| unknown("owned port configuration is unknown"))?,
        );
    }
    Ok(Value::Object(state))
}
pub async fn plan_port_set<'a, T: TokenSource>(
    client: &'a Client<T>,
    site: &str,
    selector: &str,
    faceplate: u64,
    patch: PortPatch,
    force: bool,
) -> Result<Prepared<impl Mutation<State = Value> + 'a>, Error> {
    let mut fields = patch.fields()?;
    let (resource, device, _) = DeviceResource::resolve(client, site, selector).await?;
    switch(&device)?;
    let port = select_port(&device, faceplate)?;
    guard_port(&device, port, client.protected_ports(), force, false)?;
    if let Some(profile) = &patch.profile {
        let selected =
            crate::client::port_settings::read_port_profile(client, site, profile).await?;
        for (key, value) in &mut fields {
            if *key == "portProfileId" {
                *value = json!(selected.id());
            }
        }
    }
    for (key, _) in &fields {
        let current = port
            .get(*key)
            .cloned()
            .or_else(|| {
                (*key == "speedDuplex" && port["speedDuplexMode"] == "automatic")
                    .then_some(Value::Null)
            })
            .ok_or_else(|| unknown("requested port setting is not observed on this port"))?;
        let known = match *key {
            "name" => current.is_string(),
            "portProfileId" => current.is_null() || current.is_string(),
            "userDeactivated" | "usePoeSchedule" => current.is_boolean(),
            "poePowerMode" => {
                ["alwaysOn", "quick", "normal"].contains(&current.as_str().unwrap_or(""))
            }
            "poePriority" => ["low", "high", "critical"].contains(&current.as_str().unwrap_or("")),
            "poePowerManagementMode" => {
                ["none", "class-based", "usage-based"].contains(&current.as_str().unwrap_or(""))
            }
            "speedDuplexMode" => ["automatic", "manual"].contains(&current.as_str().unwrap_or("")),
            "speedDuplex" => {
                current.is_string() || (current.is_null() && port["speedDuplexMode"] == "automatic")
            }
            _ => false,
        };
        if !known {
            return Err(unknown("requested port configuration is unknown"));
        }
        if (key.starts_with("poe") || *key == "usePoeSchedule")
            && port.get("isPoeSupported").and_then(Value::as_bool) != Some(true)
        {
            return Err(Error::new(
                ErrorKind::Unsupported,
                "port does not report PoE support",
            ));
        }
    }
    if let Some(speed) = &patch.speed_duplex
        && speed != "automatic"
        && !port
            .pointer("/capabilities/supportedSpeedDuplexes")
            .and_then(Value::as_array)
            .is_some_and(|values| values.iter().any(|v| v == speed))
    {
        return Err(Error::new(
            ErrorKind::Unsupported,
            "port does not support requested speed/duplex",
        ));
    }
    let api = number(port, "portNumber")?;
    let mut target = resource.target(&device);
    target["port"] = json!(faceplate);
    target["api_port_number"] = json!(api);
    let keys: Vec<_> = fields.iter().map(|(key, _)| *key).collect();
    let (backend, plan) = FullObjectPut::prepare(
        resource,
        device,
        move |d| observe_port(d, api, &keys),
        move |d| {
            let row = d["ethernetPorts"]
                .as_array_mut()
                .and_then(|rows| rows.iter_mut().find(|p| p["portNumber"] == api))
                .ok_or_else(|| unknown("port disappeared from prepared object"))?;
            for (key, value) in fields {
                row[key] = value;
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

fn lag_members(device: &Value, number: u64) -> Result<Vec<u64>, Error> {
    let mut members = Vec::new();
    for port in ports(device)? {
        if trunk(port)? == Some(number) {
            members.push(number_of(port)?);
        }
    }
    members.sort_unstable();
    Ok(members)
}
fn number_of(port: &Value) -> Result<u64, Error> {
    number(port, "portNumber")
}
fn select_trunk(device: &Value, n: u64) -> Result<&Value, Error> {
    if n == 0 {
        return Err(usage("LAG number must be positive"));
    }
    let rows = array(device, "trunkPorts")?;
    let mut seen = HashSet::new();
    for row in rows {
        if !seen.insert(number(row, "trunkNumber")?) {
            return Err(unknown("duplicate LAG identities"));
        }
    }
    rows.iter()
        .find(|row| row["trunkNumber"] == n)
        .ok_or_else(|| missing("LAG slot is not present in switch inventory"))
}
fn observe_lag(device: &Value, n: u64) -> Result<Value, Error> {
    let slot = select_trunk(device, n)?;
    Ok(
        json!({"members":lag_members(device,n)?,"trunk_type":slot.get("trunkType"),"user_deactivated":slot.get("userDeactivated")}),
    )
}
pub async fn plan_lag_create<'a, T: TokenSource>(
    client: &'a Client<T>,
    site: &str,
    selector: &str,
    n: u64,
    faceplates: Vec<u64>,
    mode: &str,
    force: bool,
) -> Result<Prepared<impl Mutation<State = Value> + 'a>, Error> {
    if !["static", "lacp"].contains(&mode) {
        return Err(usage("LAG type must be static or lacp"));
    }
    let unique: HashSet<_> = faceplates.iter().collect();
    if faceplates.len() < 2 || unique.len() != faceplates.len() {
        return Err(usage("LAG creation requires at least two different ports"));
    }
    let (resource, device, _) = DeviceResource::resolve(client, site, selector).await?;
    switch(&device)?;
    let slot = select_trunk(&device, n)?;
    if !["static", "lacp"].contains(&slot["trunkType"].as_str().unwrap_or(""))
        || !slot["userDeactivated"].is_boolean()
    {
        return Err(unknown("LAG slot configuration is unknown"));
    }
    if !lag_members(&device, n)?.is_empty() {
        return Err(usage("LAG slot already has members"));
    }
    let mut selected = Vec::new();
    for face in faceplates {
        let port = select_port(&device, face)?;
        guard_port(&device, port, client.protected_ports(), force, false)?;
        if trunk(port)?.is_some() {
            return Err(usage(
                "a requested port already belongs to a LAG; remove that LAG first",
            ));
        }
        selected.push(number_of(port)?);
    }
    let mut target = resource.target(&device);
    target["trunk_number"] = json!(n);
    let mode = mode.to_owned();
    let (backend, plan) = FullObjectPut::prepare(
        resource,
        device,
        move |d| observe_lag(d, n),
        move |d| {
            for p in d["ethernetPorts"]
                .as_array_mut()
                .ok_or_else(|| unknown("ports missing"))?
            {
                if selected.contains(&number_of(p)?) {
                    p["trunkNumber"] = json!(n);
                }
            }
            let slot = d["trunkPorts"]
                .as_array_mut()
                .and_then(|rows| rows.iter_mut().find(|r| r["trunkNumber"] == n))
                .ok_or_else(|| unknown("LAG slot missing"))?;
            slot["trunkType"] = json!(mode);
            slot["userDeactivated"] = json!(false);
            Ok(())
        },
    )?;
    Ok(Prepared {
        backend,
        plan,
        target,
    })
}
struct RemoveLag<'a, T> {
    client: &'a Client<T>,
    site: String,
    device: String,
    n: u64,
    members: Vec<u64>,
}
pub async fn plan_lag_remove<'a, T: TokenSource>(
    client: &'a Client<T>,
    site: &str,
    selector: &str,
    n: u64,
    force: bool,
) -> Result<Prepared<impl Mutation<State = Value> + 'a>, Error> {
    let (resource, device, _) = DeviceResource::resolve(client, site, selector).await?;
    switch(&device)?;
    select_trunk(&device, n)?;
    let members = lag_members(&device, n)?;
    if members.is_empty() {
        return Err(missing("LAG has no members"));
    }
    for port in ports(&device)? {
        if trunk(port)? == Some(n) {
            guard_port(&device, port, client.protected_ports(), force, false)?;
        }
    }
    let mut target = resource.target(&device);
    target["trunk_number"] = json!(n);
    Ok(Prepared {
        backend: RemoveLag {
            client,
            site: site.to_ascii_lowercase(),
            device: device["id"]
                .as_str()
                .ok_or_else(|| unknown("device identity missing"))?
                .into(),
            n,
            members: members.clone(),
        },
        plan: Plan {
            current: json!(members),
            desired: json!([]),
        },
        target,
    })
}
impl<T: TokenSource> Mutation for RemoveLag<'_, T> {
    type State = Value;
    async fn read(&self) -> Result<Value, Error> {
        let inventory = self.client.get(&inventory_path(&self.site)?).await?;
        let device = select_device_by_id(&inventory, &self.device)?;
        let fresh_ports = ports(device)?;
        for api in &self.members {
            if !fresh_ports.iter().any(|p| p["portNumber"] == *api) {
                return Err(unknown("original LAG member port is absent from readback"));
            }
        }
        let slots = array(device, "trunkPorts")?;
        let mut numbers = HashSet::new();
        for slot in slots {
            let n = number(slot, "trunkNumber")?;
            if n == 0 || !numbers.insert(n) {
                return Err(unknown("LAG readback configuration identities are invalid"));
            }
        }
        Ok(json!(lag_members(device, self.n)?))
    }
    async fn write(&self, desired: &Value) -> Result<(), Error> {
        if desired != &json!([]) {
            return Err(Error::new(
                ErrorKind::Config,
                "LAG removal requires empty membership",
            ));
        }
        self.client
            .action(
                &inventory_path(&self.site)?,
                &self.device,
                "resetTrunkPort",
                &json!({"trunkNumber":self.n}),
            )
            .await?;
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct MirrorPatch {
    pub enabled: bool,
    pub destination: Option<u64>,
    pub sources: Vec<u64>,
    pub network: Option<String>,
    pub direction: String,
}
pub async fn plan_mirror<'a, T: TokenSource>(
    client: &'a Client<T>,
    site: &str,
    selector: &str,
    patch: MirrorPatch,
    force: bool,
) -> Result<Prepared<impl Mutation<State = Value> + 'a>, Error> {
    if !["both", "tx", "rx"].contains(&patch.direction.as_str()) {
        return Err(usage("mirroring direction must be both, tx, or rx"));
    }
    if patch.enabled
        && (patch.destination.is_none() || (patch.sources.is_empty() == patch.network.is_none()))
    {
        return Err(usage(
            "enabled mirroring requires a destination and exactly one source type",
        ));
    }
    let (resource, device, _) = DeviceResource::resolve(client, site, selector).await?;
    switch(&device)?;
    if device
        .pointer("/capabilities/has/portMirroring")
        .and_then(Value::as_bool)
        != Some(true)
    {
        return Err(Error::new(
            ErrorKind::Unsupported,
            "switch does not report mirroring capability",
        ));
    }
    let current = device
        .get("portMirroringConfig")
        .filter(|v| v.is_object())
        .ok_or_else(|| unknown("mirroring configuration is unknown"))?;
    boolean(current, "isEnabled")?;
    if patch.network.is_some() {
        for port in ports(&device)? {
            guard_port(&device, port, client.protected_ports(), force, false)?;
        }
    }
    let unique: HashSet<_> = patch.sources.iter().collect();
    if unique.len() != patch.sources.len() {
        return Err(usage("mirror source ports must be unique"));
    }
    let mut affected = patch.sources.clone();
    affected.extend(patch.destination);
    // Disabling/replacing a mirror also changes its previous ports.
    if current["isEnabled"] == true {
        match current["sourceType"].as_str() {
            Some("ports") => {
                let numbers = current["sourcePortNumbers"]
                    .as_array()
                    .ok_or_else(|| unknown("previous mirror source identities are unknown"))?;
                let mut seen = HashSet::new();
                for value in numbers {
                    let api = value
                        .as_u64()
                        .ok_or_else(|| unknown("previous mirror source port is unknown"))?;
                    if !seen.insert(api) {
                        return Err(unknown("previous mirror source identities are duplicated"));
                    }
                    let port = ports(&device)?
                        .iter()
                        .find(|p| p["portNumber"] == api)
                        .ok_or_else(|| unknown("previous mirror source port is absent"))?;
                    guard_port(&device, port, client.protected_ports(), force, false)?;
                }
            }
            Some("network") => {
                for port in ports(&device)? {
                    guard_port(&device, port, client.protected_ports(), force, false)?;
                }
            }
            _ => return Err(unknown("previous mirror source type is unknown")),
        }
        let api = current["destinationPortNumber"]
            .as_u64()
            .ok_or_else(|| unknown("previous mirror destination is unknown"))?;
        let port = ports(&device)?
            .iter()
            .find(|p| p["portNumber"] == api)
            .ok_or_else(|| unknown("previous mirror destination port is absent"))?;
        guard_port(&device, port, client.protected_ports(), force, false)?;
    }
    for face in affected {
        guard_port(
            &device,
            select_port(&device, face)?,
            client.protected_ports(),
            force,
            false,
        )?;
    }
    let mut desired = current.clone();
    desired["isEnabled"] = json!(patch.enabled);
    if patch.enabled {
        let face = patch
            .destination
            .ok_or_else(|| usage("missing destination"))?;
        desired["destinationPortNumber"] = json!(number_of(select_port(&device, face)?)?);
        if let Some(network) = patch.network {
            if network.is_empty() {
                return Err(usage("network identifier must not be empty"));
            }
            desired["sourceType"] = json!("network");
            desired["sourceNetworkId"] = json!(network);
            desired["sourcePortNumbers"] = json!([]);
        } else {
            let mut numbers = Vec::new();
            for source in patch.sources {
                if source == face {
                    return Err(usage(
                        "mirror source ports must be unique and differ from destination",
                    ));
                }
                numbers.push(number_of(select_port(&device, source)?)?);
            }
            desired["sourceType"] = json!("ports");
            desired["sourceNetworkId"] = Value::Null;
            desired["sourcePortNumbers"] = json!(numbers);
        }
        desired["directionType"] = json!(patch.direction);
    }
    let target = resource.target(&device);
    let keys = if patch.enabled {
        vec![
            "isEnabled",
            "destinationPortNumber",
            "sourceType",
            "sourceNetworkId",
            "sourcePortNumbers",
            "directionType",
        ]
    } else {
        vec!["isEnabled"]
    };
    let (backend, plan) = FullObjectPut::prepare(
        resource,
        device,
        move |d| {
            let config = d
                .get("portMirroringConfig")
                .filter(|v| v.is_object())
                .ok_or_else(|| unknown("mirroring configuration missing"))?;
            let mut state = serde_json::Map::new();
            for key in &keys {
                state.insert(
                    (*key).into(),
                    config.get(*key).cloned().unwrap_or(Value::Null),
                );
            }
            Ok(Value::Object(state))
        },
        move |d| {
            d["portMirroringConfig"] = desired;
            Ok(())
        },
    )?;
    Ok(Prepared {
        backend,
        plan,
        target,
    })
}

pub struct PowerCycle<'a, T> {
    client: &'a Client<T>,
    site: String,
    device: String,
    api_port: u64,
    faceplate: u64,
    client_id: String,
}
fn attachment<'a>(
    row: &'a crate::client::reads::ClientSummary,
    device: &str,
    port: u64,
) -> Result<Option<&'a Value>, Error> {
    let links = row
        .connected_to_ports
        .as_ref()
        .and_then(Value::as_array)
        .ok_or_else(|| unknown("client port attachment is unknown"))?;
    let matches: Vec<_> = links
        .iter()
        .filter(|p| p["deviceId"] == device && p["portNumber"] == port)
        .collect();
    match matches.as_slice() {
        [] => Ok(None),
        [link] => Ok(Some(*link)),
        _ => Err(usage("client has duplicate attachments to this port")),
    }
}
pub async fn plan_power_cycle<'a, T: TokenSource>(
    client: &'a Client<T>,
    site: &str,
    selector: &str,
    faceplate: u64,
) -> Result<Prepared<PowerCycle<'a, T>>, Error> {
    let (resource, device, _) = DeviceResource::resolve(client, site, selector).await?;
    switch(&device)?;
    let port = select_port(&device, faceplate)?;
    guard_port(&device, port, client.protected_ports(), false, true)?;
    if boolean(port, "userDeactivated")? || boolean(port, "isPowerCycling")? {
        return Err(usage(
            "power-cycle refuses a disabled or already cycling port",
        ));
    }
    if !boolean(port, "isPoeSupported")? || !boolean(port, "isProvidingPower")? {
        return Err(usage(
            "power-cycle requires a PoE port currently providing power",
        ));
    }
    let device_id = device["id"]
        .as_str()
        .ok_or_else(|| unknown("device identity missing"))?;
    let api_port = number_of(port)?;
    let clients = client.clients(site).await?;
    let mut matches = Vec::new();
    for row in &clients {
        if matches!(row.client_type.as_deref(), Some("wireless" | "vpn")) {
            continue;
        }
        if let Some(link) = attachment(row, device_id, api_port)?
            && boolean(link, "isPoweredByPort")?
        {
            if !boolean(link, "isPowerCyclable")? {
                return Err(usage("attached client is not power-cyclable"));
            }
            if row.status.as_deref() != Some("up") {
                return Err(unknown("powered client is not known to be connected"));
            }
            if boolean(link, "isPowerCycling")? {
                return Err(usage("port is already power cycling"));
            }
            for candidate in row
                .connected_to_ports
                .as_ref()
                .and_then(Value::as_array)
                .ok_or_else(|| unknown("client port attachments missing"))?
            {
                boolean(candidate, "isPoweredByPort")?;
                boolean(candidate, "isPowerCyclable")?;
            }
            let candidates = row
                .connected_to_ports
                .as_ref()
                .and_then(Value::as_array)
                .ok_or_else(|| unknown("client attachments unknown"))?
                .iter()
                .filter(|p| p["isPoweredByPort"] == true && p["isPowerCyclable"] == true)
                .count();
            if candidates != 1 {
                return Err(usage("client has multiple power-cyclable port attachments"));
            }
            matches.push(row);
        }
    }
    let row = match matches.as_slice() {
        [row] => *row,
        [] => return Err(missing("port has no attached powered client")),
        _ => return Err(usage("port has multiple attached powered clients")),
    };
    let mut target = resource.target(&device);
    target["port"] = json!(faceplate);
    target["api_port_number"] = json!(api_port);
    target["client_id"] = json!(row.id);
    Ok(Prepared {
        backend: PowerCycle {
            client,
            site: site.to_ascii_lowercase(),
            device: device_id.into(),
            api_port,
            faceplate,
            client_id: row.id.clone(),
        },
        plan: Plan {
            current: false,
            desired: true,
        },
        target,
    })
}
impl<T: TokenSource> PowerCycle<'_, T> {
    async fn observe(&self) -> Result<bool, Error> {
        let rows = self.client.clients(&self.site).await?;
        let row = rows
            .iter()
            .find(|r| r.id == self.client_id)
            .ok_or_else(|| unknown("power-cycle client disappeared"))?;
        let port = attachment(row, &self.device, self.api_port)?
            .ok_or_else(|| unknown("power-cycle client moved to another port"))?;
        boolean(port, "isPowerCycling")
    }
}
impl<T: TokenSource> Mutation for PowerCycle<'_, T> {
    type State = bool;
    async fn read(&self) -> Result<bool, Error> {
        self.observe().await
    }
    async fn write(&self, desired: &bool) -> Result<(), Error> {
        if !desired {
            return Err(Error::new(
                ErrorKind::Config,
                "power-cycle requires initiation",
            ));
        }
        // The client action chooses a powered port itself. Revalidate that its
        // unique candidate still binds the inventory port before sending once.
        let prepared =
            plan_power_cycle(self.client, &self.site, &self.device, self.faceplate).await?;
        if prepared.backend.client_id != self.client_id
            || prepared.backend.api_port != self.api_port
        {
            return Err(usage("powered client attachment changed before request"));
        }
        let reply = self
            .client
            .action(
                &format!("/sites/{}/clientDetails", self.site),
                &self.client_id,
                "powerCycle",
                &json!({}),
            )
            .await?;
        if let Some(id) = reply.get("id")
            && id.as_str() != Some(self.client_id.as_str())
        {
            return Err(unknown(
                "power-cycle acknowledgment identified a different client",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct PortConnection {
    pub client_mac: String,
    pub client_name: Option<String>,
    pub ip: Option<String>,
    pub device_mac: String,
    pub device_name: Option<String>,
    pub port_idx: u64,
    pub api_port_number: u64,
    pub connected: Option<bool>,
    pub powered: Option<bool>,
    pub trunk_number: Option<u64>,
}
pub async fn find_port<T: TokenSource>(
    client: &Client<T>,
    site: &str,
    selector: &str,
) -> Result<Vec<PortConnection>, Error> {
    if selector.trim().is_empty() {
        return Err(usage("client selector must not be empty"));
    }
    crate::validate_site_id(site)?;
    let clients = client.clients(site).await?;
    let query = selector.to_lowercase();
    let exact = clients.iter().any(|row| {
        row.id == selector
            || row.mac_address.eq_ignore_ascii_case(selector)
            || row.ip_address.as_deref() == Some(selector)
    });
    let mut selected = Vec::new();
    for row in &clients {
        let matches = if exact {
            row.id == selector
                || row.mac_address.eq_ignore_ascii_case(selector)
                || row.ip_address.as_deref() == Some(selector)
        } else {
            row.name
                .as_ref()
                .is_some_and(|n| n.to_lowercase().contains(&query))
        };
        if !matches
            || matches!(
                row.client_type.as_deref(),
                Some("wireless" | "vpn" | "cellular")
            )
        {
            continue;
        }
        let links = row
            .connected_to_ports
            .as_ref()
            .and_then(Value::as_array)
            .ok_or_else(|| unknown("matching client's port attachment is unknown"))?;
        if !links.is_empty() {
            selected.push(row);
        }
    }
    if selected.is_empty() {
        return Err(missing("no client on a switch port matches the selector"));
    }
    if selected.len() != 1 {
        return Err(usage(
            "client name matches multiple clients on switch ports; use a MAC or IP",
        ));
    }
    let row = selected[0];
    let inventory = client.get(&inventory_path(site)?).await?;
    let mut result = Vec::new();
    let mut seen = HashSet::new();
    for link in row
        .connected_to_ports
        .as_ref()
        .and_then(Value::as_array)
        .ok_or_else(|| unknown("client attachment missing"))?
    {
        let id = link
            .get("deviceId")
            .and_then(Value::as_str)
            .ok_or_else(|| unknown("client attachment device identity missing"))?;
        let api = number(link, "portNumber")?;
        if !seen.insert((id, api)) {
            return Err(unknown("client has duplicate port attachments"));
        }
        let device = select_device_by_id(&inventory, id)?;
        let port = ports(device)?
            .iter()
            .find(|p| p["portNumber"] == api)
            .ok_or_else(|| unknown("attached port is absent from inventory"))?;
        result.push(PortConnection {
            client_mac: row.mac_address.clone(),
            client_name: row.name.clone(),
            ip: row.ip_address.clone(),
            device_mac: device["macAddress"]
                .as_str()
                .ok_or_else(|| unknown("switch MAC unknown"))?
                .into(),
            device_name: device["name"].as_str().map(str::to_owned),
            port_idx: number(port, "faceplatePortNumber")?,
            api_port_number: api,
            connected: match row.status.as_deref() {
                Some("up") => Some(true),
                Some("down") => Some(false),
                _ => None,
            },
            powered: link.get("isPoweredByPort").and_then(Value::as_bool),
            trunk_number: link.get("trunkNumber").and_then(Value::as_u64),
        });
    }
    Ok(result)
}
