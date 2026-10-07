use std::{collections::BTreeMap, net::Ipv4Addr};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::{
    Client, Error, ErrorKind, TokenSource,
    mutation::{FullObjectPut, Mutation, ObjectResource, Prepared},
    validate_site_id,
};

/// A confirmed site-scoped portal resource.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Resource {
    Health,
    Dashboard,
    Topology,
    Timezone,
    ManagementNetwork,
    SpanningTree,
    ExtendNetwork,
}

/// The portal's DNS server assignment mode.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DnsMode {
    Automatic,
    Infrastructure,
    Custom,
}

impl DnsMode {
    fn api_id(self) -> &'static str {
        match self {
            Self::Automatic => "automatic",
            Self::Infrastructure => "infrastructure",
            Self::Custom => "custom",
        }
    }

    fn from_api_id(value: &str) -> Option<Self> {
        match value {
            "automatic" => Some(Self::Automatic),
            "infrastructure" => Some(Self::Infrastructure),
            "custom" => Some(Self::Custom),
            _ => None,
        }
    }
}

/// Raw DNS settings. Unknown portal fields remain available in `extra`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DnsConfig {
    #[serde(rename = "dnsServerAssignationMode")]
    pub mode: Option<String>,
    #[serde(rename = "automaticPrimaryDns")]
    pub automatic_primary: Option<String>,
    #[serde(rename = "automaticSecondaryDns")]
    pub automatic_secondary: Option<String>,
    #[serde(rename = "customPrimaryDns")]
    pub custom_primary: Option<String>,
    #[serde(rename = "customSecondaryDns")]
    pub custom_secondary: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl Resource {
    fn path_segment(self) -> &'static str {
        match self {
            Self::Health => "health",
            Self::Dashboard => "dashboard",
            Self::Topology => "graphTopology",
            Self::Timezone => "timezone",
            Self::ManagementNetwork => "managementNetwork",
            Self::SpanningTree => "spanningTree",
            Self::ExtendNetwork => "extendNetwork",
        }
    }
}

/// A supported change to one site singleton.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Change {
    Timezone(String),
    ManagementVlan(u16),
    Dns {
        mode: DnsMode,
        primary: Option<Ipv4Addr>,
        secondary: Option<Ipv4Addr>,
    },
    SpanningTree {
        use_rstp: Option<bool>,
        priority: Option<u16>,
    },
    ExtendNetwork {
        enabled: Option<bool>,
        outdoor_mesh: Option<bool>,
    },
}

impl Change {
    /// Validate user input before a planner reads the portal.
    pub fn validate(&self) -> Result<(), Error> {
        match self {
            Self::Timezone(timezone) => {
                if !valid_timezone(timezone) {
                    return Err(Error::new(
                        ErrorKind::Usage,
                        "timezone must be a known IANA timezone name",
                    ));
                }
            }
            Self::ManagementVlan(vlan) => {
                if !valid_vlan(*vlan) {
                    return Err(Error::new(
                        ErrorKind::Usage,
                        "management VLAN must be 1 to 4092 and outside reserved VLANs 3333 to 3349",
                    ));
                }
            }
            Self::Dns {
                mode,
                primary,
                secondary,
            } => match mode {
                DnsMode::Custom if primary.is_none() => {
                    return Err(Error::new(
                        ErrorKind::Config,
                        "custom DNS mode requires a primary IPv4 address",
                    ));
                }
                DnsMode::Automatic | DnsMode::Infrastructure
                    if primary.is_some() || secondary.is_some() =>
                {
                    return Err(Error::new(
                        ErrorKind::Usage,
                        "DNS server addresses are only valid in custom mode",
                    ));
                }
                _ => {}
            },
            Self::SpanningTree { use_rstp, priority } => {
                if use_rstp.is_none() && priority.is_none() {
                    return Err(Error::new(
                        ErrorKind::Usage,
                        "spanning-tree update must change at least one field",
                    ));
                }
                if priority.is_some_and(|value| !valid_priority(value)) {
                    return Err(Error::new(
                        ErrorKind::Usage,
                        "spanning-tree priority must be a multiple of 4096 from 0 to 61440",
                    ));
                }
            }
            Self::ExtendNetwork {
                enabled,
                outdoor_mesh,
            } => {
                if enabled.is_none() && outdoor_mesh.is_none() {
                    return Err(Error::new(
                        ErrorKind::Usage,
                        "extend-network update must change at least one field",
                    ));
                }
            }
        }
        Ok(())
    }

    fn resource(&self) -> Resource {
        match self {
            Self::Timezone(_) => Resource::Timezone,
            Self::ManagementVlan(_) => Resource::ManagementNetwork,
            Self::Dns { .. } => Resource::ManagementNetwork,
            Self::SpanningTree { .. } => Resource::SpanningTree,
            Self::ExtendNetwork { .. } => Resource::ExtendNetwork,
        }
    }
}

impl<T: TokenSource> Client<T> {
    /// Fetch a complete site resource without projecting or defaulting fields.
    pub async fn site_resource(&self, site: &str, resource: Resource) -> Result<Value, Error> {
        validate_site_id(site)?;
        let path = resource_path(&site.to_ascii_lowercase(), resource);
        let value = self.get(&path).await?;
        if !value.is_object() {
            return Err(Error::new(
                ErrorKind::Unverified,
                "site resource response is not an object",
            ));
        }
        Ok(value)
    }

    /// Read the nested management-network DNS settings without defaulting fields.
    pub async fn site_dns(&self, site: &str) -> Result<DnsConfig, Error> {
        let resource = self
            .site_resource(site, Resource::ManagementNetwork)
            .await?;
        let dns = Value::Object(management_dns(&resource)?.clone());
        serde_json::from_value(dns)
            .map_err(|_| unverified("management-network DNS settings are missing or invalid"))
    }
}

struct SiteResourceBackend<'a, T> {
    client: &'a Client<T>,
    site: String,
    resource: Resource,
    identity: ResourceIdentity,
}

impl<T: TokenSource> ObjectResource for SiteResourceBackend<'_, T> {
    async fn read_object(&self) -> Result<Value, Error> {
        self.client.site_resource(&self.site, self.resource).await
    }

    async fn put_object(&self, body: &Value) -> Result<(), Error> {
        let response = self
            .client
            .put_full(&resource_path(&self.site, self.resource), body)
            .await?;
        self.identity.check_returned(&response)?;
        Ok(())
    }
}

#[derive(Clone)]
struct ResourceIdentity {
    id: Option<Value>,
    kind: Option<Value>,
}

impl ResourceIdentity {
    fn capture(value: &Value) -> Result<Self, Error> {
        let object = value
            .as_object()
            .ok_or_else(|| unverified("site resource response is not an object"))?;
        Ok(Self {
            id: object.get("id").cloned(),
            kind: object.get("kind").cloned(),
        })
    }

    fn check(&self, value: &Value) -> Result<(), Error> {
        let object = value
            .as_object()
            .ok_or_else(|| unverified("site resource response is not an object"))?;
        for (field, expected) in [("id", &self.id), ("kind", &self.kind)] {
            if expected
                .as_ref()
                .is_some_and(|expected| object.get(field) != Some(expected))
            {
                return Err(unverified("site resource identity changed"));
            }
        }
        Ok(())
    }

    fn check_returned(&self, value: &Value) -> Result<(), Error> {
        let Some(object) = value.as_object() else {
            // Empty 204 responses are represented as null. Other response
            // shapes carry no resource identity to bind.
            return Ok(());
        };
        for (field, expected) in [("id", &self.id), ("kind", &self.kind)] {
            if let Some(returned) = object.get(field)
                && expected.as_ref() != Some(returned)
            {
                return Err(unverified("site resource identity changed"));
            }
        }
        Ok(())
    }
}

/// Read a complete site singleton and prepare one full-object update.
pub async fn plan_update<'a, T: TokenSource>(
    client: &'a Client<T>,
    site: &str,
    change: Change,
) -> Result<Prepared<impl Mutation<State = Value> + 'a>, Error> {
    change.validate()?;
    validate_site_id(site)?;
    let resource = change.resource();
    let site = site.to_ascii_lowercase();
    let current_body = client.site_resource(&site, resource).await?;
    let identity = ResourceIdentity::capture(&current_body)?;
    let observed_change = change.clone();
    let observed_identity = identity.clone();
    let (backend, plan) = FullObjectPut::prepare(
        SiteResourceBackend {
            client,
            site: site.clone(),
            resource,
            identity,
        },
        current_body,
        move |value| {
            observed_identity.check(value)?;
            observe(value, &observed_change)
        },
        move |value| patch(value, &change),
    )?;
    Ok(Prepared {
        backend,
        plan,
        target: json!({
            "site_id": site,
            "resource": resource.path_segment(),
        }),
    })
}

fn resource_path(site: &str, resource: Resource) -> String {
    format!("/sites/{site}/{}", resource.path_segment())
}

fn patch(value: &mut Value, change: &Change) -> Result<(), Error> {
    let object = value
        .as_object_mut()
        .ok_or_else(|| unverified("site resource response is not an object"))?;
    match change {
        Change::Timezone(timezone) => {
            object.insert("timezoneIana".into(), Value::String(timezone.clone()));
        }
        Change::ManagementVlan(vlan) => {
            object.insert("managementVlan".into(), json!(vlan));
        }
        Change::Dns {
            mode,
            primary,
            secondary,
        } => {
            let dns = management_dns_mut(value)?;
            dns.insert(
                "dnsServerAssignationMode".into(),
                Value::String(mode.api_id().into()),
            );
            if *mode == DnsMode::Custom {
                let primary = primary.ok_or_else(|| {
                    Error::new(
                        ErrorKind::Config,
                        "custom DNS update has no primary IPv4 address",
                    )
                })?;
                dns.insert(
                    "customPrimaryDns".into(),
                    Value::String(primary.to_string()),
                );
                if let Some(secondary) = secondary {
                    dns.insert(
                        "customSecondaryDns".into(),
                        Value::String(secondary.to_string()),
                    );
                } else {
                    dns.remove("customSecondaryDns");
                }
            }
        }
        Change::SpanningTree { use_rstp, priority } => {
            if let Some(value) = use_rstp {
                object.insert("useRstp".into(), Value::Bool(*value));
            }
            if let Some(value) = priority {
                object.insert("stpBaseBridgePriority".into(), json!(value));
            }
        }
        Change::ExtendNetwork {
            enabled,
            outdoor_mesh,
        } => {
            if let Some(enabled) = enabled {
                object.insert("extendNetworkEnabled".into(), Value::Bool(*enabled));
            }
            if let Some(outdoor_mesh) = outdoor_mesh {
                object.insert(
                    "isExtendNetworkOutdoorMesh".into(),
                    Value::Bool(*outdoor_mesh),
                );
            }
        }
    }
    Ok(())
}

fn observe(value: &Value, change: &Change) -> Result<Value, Error> {
    let object = value
        .as_object()
        .ok_or_else(|| unverified("site resource response is not an object"))?;
    let mut state = Map::new();
    match change {
        Change::Timezone(_) => {
            let timezone = object
                .get("timezoneIana")
                .and_then(Value::as_str)
                .filter(|timezone| valid_timezone(timezone))
                .ok_or_else(|| unverified("timezone state is missing or invalid"))?;
            state.insert("timezoneIana".into(), Value::String(timezone.to_owned()));
        }
        Change::ManagementVlan(_) => {
            let value = object
                .get("managementVlan")
                .and_then(Value::as_u64)
                .and_then(|value| u16::try_from(value).ok())
                .filter(|value| valid_vlan(*value))
                .ok_or_else(|| unverified("management VLAN state is missing or invalid"))?;
            state.insert("managementVlan".into(), json!(value));
        }
        Change::Dns {
            mode: desired_mode, ..
        } => {
            let dns = management_dns(value)?;
            let mode = dns
                .get("dnsServerAssignationMode")
                .and_then(Value::as_str)
                .and_then(DnsMode::from_api_id)
                .ok_or_else(|| unverified("DNS assignment mode is missing or unknown"))?;
            state.insert(
                "dnsServerAssignationMode".into(),
                Value::String(mode.api_id().into()),
            );
            if *desired_mode == DnsMode::Custom && mode == DnsMode::Custom {
                let primary = dns
                    .get("customPrimaryDns")
                    .and_then(Value::as_str)
                    .and_then(|value| value.parse::<Ipv4Addr>().ok())
                    .ok_or_else(|| unverified("custom primary DNS state is missing or invalid"))?;
                let secondary = optional_ipv4(dns, "customSecondaryDns")?;
                state.insert(
                    "customPrimaryDns".into(),
                    Value::String(primary.to_string()),
                );
                state.insert(
                    "customSecondaryDns".into(),
                    secondary.map_or(Value::Null, |value| Value::String(value.to_string())),
                );
            }
        }
        Change::SpanningTree { use_rstp, priority } => {
            if use_rstp.is_some() {
                let value = object
                    .get("useRstp")
                    .and_then(Value::as_bool)
                    .ok_or_else(|| unverified("spanning-tree RSTP state is missing or invalid"))?;
                state.insert("useRstp".into(), Value::Bool(value));
            }
            if priority.is_some() {
                let value = object
                    .get("stpBaseBridgePriority")
                    .and_then(Value::as_u64)
                    .and_then(|value| u16::try_from(value).ok())
                    .filter(|value| valid_priority(*value))
                    .ok_or_else(|| {
                        unverified("spanning-tree priority state is missing or invalid")
                    })?;
                state.insert("stpBaseBridgePriority".into(), json!(value));
            }
        }
        Change::ExtendNetwork {
            enabled,
            outdoor_mesh,
        } => {
            if enabled.is_some() {
                let value = object
                    .get("extendNetworkEnabled")
                    .and_then(Value::as_bool)
                    .ok_or_else(|| {
                        unverified("extend-network enabled state is missing or invalid")
                    })?;
                state.insert("extendNetworkEnabled".into(), Value::Bool(value));
            }
            if outdoor_mesh.is_some() {
                let value = object
                    .get("isExtendNetworkOutdoorMesh")
                    .and_then(Value::as_bool)
                    .ok_or_else(|| {
                        unverified("extend-network outdoor-mesh state is missing or invalid")
                    })?;
                state.insert("isExtendNetworkOutdoorMesh".into(), Value::Bool(value));
            }
        }
    }
    Ok(Value::Object(state))
}

fn management_dns(value: &Value) -> Result<&Map<String, Value>, Error> {
    value
        .get("managementSubnet")
        .and_then(Value::as_object)
        .and_then(|subnet| subnet.get("dns"))
        .and_then(Value::as_object)
        .ok_or_else(|| unverified("management-network DNS settings are missing or invalid"))
}

fn management_dns_mut(value: &mut Value) -> Result<&mut Map<String, Value>, Error> {
    value
        .as_object_mut()
        .and_then(|resource| resource.get_mut("managementSubnet"))
        .and_then(Value::as_object_mut)
        .and_then(|subnet| subnet.get_mut("dns"))
        .and_then(Value::as_object_mut)
        .ok_or_else(|| unverified("management-network DNS settings are missing or invalid"))
}

fn optional_ipv4(dns: &Map<String, Value>, field: &'static str) -> Result<Option<Ipv4Addr>, Error> {
    match dns.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if value.is_empty() => Ok(None),
        Some(Value::String(value)) => value
            .parse::<Ipv4Addr>()
            .map(Some)
            .map_err(|_| unverified("custom secondary DNS state is invalid")),
        _ => Err(unverified("custom secondary DNS state is invalid")),
    }
}

fn valid_timezone(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && !value.chars().any(char::is_control)
        && jiff::tz::TimeZone::get(value).is_ok_and(|zone| !zone.is_unknown())
}

fn valid_priority(value: u16) -> bool {
    value <= 61_440 && value.is_multiple_of(4_096)
}

fn valid_vlan(value: u16) -> bool {
    (1..=4092).contains(&value) && !(3333..=3349).contains(&value)
}

fn unverified(message: &'static str) -> Error {
    Error::new(ErrorKind::Unverified, message)
}
