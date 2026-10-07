use std::net::Ipv4Addr;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{incomplete, usage};
use crate::Error;

// wp.prepareData, chunk-EFL5B25Z.js @4957695. Creation copies only
// configuration from metaData.defaultWiredNetwork; updates retain the full GET.
pub(super) const FIELDS: &[&str] = &[
    "wiredNetworkName",
    "isEnabled",
    "type",
    "shouldApplyNetworkSecurityProtections",
    "vlanId",
    "useDhcpScope",
    "dhcpScope",
    "isAccessRestricted",
    "isInternetAllowed",
    "isIntraSubnetTrafficAllowed",
    "isSpecificDestinationsAllowed",
    "allowedDestinations",
    "isIpRoutingEnabled",
    "ipRoutingConfig",
    "devicePortMappings",
    "isGuestPortalEnabled",
    "isIgmpSnoopingEnabled",
    "qos",
];

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum NetworkType {
    Employee,
    Guest,
    Voice,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DnsMode {
    Automatic,
    Infrastructure,
    Custom,
}

#[derive(Clone, Debug, Default)]
pub struct DhcpPatch {
    pub enabled: Option<bool>,
    pub gateway: Option<Ipv4Addr>,
    pub prefix_length: Option<u8>,
    pub start: Option<Ipv4Addr>,
    pub end: Option<Ipv4Addr>,
    pub domain_name: Option<String>,
    pub dns_mode: Option<DnsMode>,
    pub primary_dns: Option<Ipv4Addr>,
    pub secondary_dns: Option<Ipv4Addr>,
}

impl DhcpPatch {
    pub fn is_empty(&self) -> bool {
        self.enabled.is_none()
            && self.gateway.is_none()
            && self.prefix_length.is_none()
            && self.start.is_none()
            && self.end.is_none()
            && self.domain_name.is_none()
            && self.dns_mode.is_none()
            && self.primary_dns.is_none()
            && self.secondary_dns.is_none()
    }

    fn changes_scope(&self) -> bool {
        let mut scope = self.clone();
        scope.enabled = None;
        !scope.is_empty()
    }

    pub fn validate(&self) -> Result<(), Error> {
        if self
            .prefix_length
            .is_some_and(|prefix| !(1..=30).contains(&prefix))
        {
            return Err(usage("DHCP prefix length must be between 1 and 30"));
        }
        if let (Some(start), Some(end)) = (self.start, self.end)
            && u32::from(start) > u32::from(end)
        {
            return Err(usage("DHCP range start must not exceed its end"));
        }
        if self
            .domain_name
            .as_ref()
            .is_some_and(|name| name.len() > 253 || name.chars().any(char::is_control))
        {
            return Err(usage("DHCP domain name is invalid"));
        }
        if self.dns_mode.is_some_and(|mode| mode != DnsMode::Custom)
            && (self.primary_dns.is_some() || self.secondary_dns.is_some())
        {
            return Err(usage("custom DNS addresses require custom DNS mode"));
        }
        Ok(())
    }

    pub(super) fn apply(&self, body: &mut Value, creating: bool) -> Result<(), Error> {
        self.validate()?;
        if self.is_empty() {
            return Ok(());
        }
        if !body.get("dhcpScope").is_some_and(Value::is_object) {
            return Err(incomplete("wired DHCP configuration is unavailable"));
        }
        if self.enabled == Some(false)
            && !creating
            && body.get("canDisableDhcpScope").and_then(Value::as_bool) != Some(true)
        {
            return Err(usage("this wired DHCP scope cannot be disabled"));
        }
        if let Some(enabled) = self.enabled {
            body["useDhcpScope"] = json!(enabled);
        }
        if !self.changes_scope() {
            return Ok(());
        }
        let scope = &mut body["dhcpScope"];
        if let Some(gateway) = self.gateway {
            scope["ipAddress"] = json!(gateway.to_string());
        }
        if let Some(prefix) = self.prefix_length {
            scope["netmask"] = json!(Ipv4Addr::from(u32::MAX << (32 - prefix)).to_string());
        }
        if self.gateway.is_some() || self.prefix_length.is_some() {
            let address = ipv4(scope.get("ipAddress"))
                .or_else(|| ipv4(scope.get("network")))
                .ok_or_else(|| incomplete("DHCP address is unavailable"))?;
            let mask = mask(scope.get("netmask"))?;
            scope["network"] = json!(Ipv4Addr::from(u32::from(address) & mask).to_string());
        }
        if self.start.is_some() || self.end.is_some() {
            if !scope.get("ipAddressRange").is_some_and(Value::is_object) {
                scope["ipAddressRange"] = json!({});
            }
            if let Some(start) = self.start {
                scope["ipAddressRange"]["start"] = json!(start.to_string());
            }
            if let Some(end) = self.end {
                scope["ipAddressRange"]["end"] = json!(end.to_string());
            }
        }
        if let Some(domain) = &self.domain_name {
            scope["domainName"] = if domain.is_empty() {
                Value::Null
            } else {
                json!(domain)
            };
        }
        if self.dns_mode.is_some() || self.primary_dns.is_some() || self.secondary_dns.is_some() {
            if !scope.get("dns").is_some_and(Value::is_object) {
                scope["dns"] = json!({"kind":"dns"});
            }
            if let Some(mode) = self.dns_mode {
                scope["dns"]["dnsServerAssignationMode"] = json!(mode);
            }
            if self.primary_dns.is_some() || self.secondary_dns.is_some() {
                scope["dns"]["dnsServerAssignationMode"] = json!("custom");
            }
            if let Some(dns) = self.primary_dns {
                scope["dns"]["customPrimaryDns"] = json!(dns.to_string());
            }
            if let Some(dns) = self.secondary_dns {
                scope["dns"]["customSecondaryDns"] = json!(dns.to_string());
            }
            if scope["dns"]["dnsServerAssignationMode"] == "custom"
                && ipv4(scope["dns"].get("customPrimaryDns")).is_none()
            {
                return Err(usage("custom DNS requires a primary IPv4 address"));
            }
        }
        validate_scope(scope)
    }
}

#[derive(Clone, Debug, Default)]
pub struct Patch {
    pub name: Option<String>,
    pub enabled: Option<bool>,
    pub network_type: Option<NetworkType>,
    pub vlan_id: Option<u16>,
    pub dhcp: DhcpPatch,
}

impl Patch {
    pub fn is_empty(&self) -> bool {
        self.name.is_none()
            && self.enabled.is_none()
            && self.network_type.is_none()
            && self.vlan_id.is_none()
            && self.dhcp.is_empty()
    }

    pub fn validate(&self) -> Result<(), Error> {
        if self.name.as_ref().is_some_and(|name| {
            name.trim().is_empty()
                || name.chars().count() > 32
                || name.chars().any(|c| c == '"' || c.is_control())
        }) {
            return Err(usage(
                "wired network name must contain 1 to 32 characters without double quotes or control characters",
            ));
        }
        if self
            .vlan_id
            .is_some_and(|vlan| !(2..=4092).contains(&vlan) || (3333..=3349).contains(&vlan))
        {
            return Err(usage(
                "VLAN ID must be 2 to 4092, excluding reserved IDs 3333 to 3349",
            ));
        }
        self.dhcp.validate()
    }

    pub(super) fn fields(&self) -> Vec<&'static str> {
        let mut fields = Vec::new();
        if self.name.is_some() {
            fields.push("wiredNetworkName");
        }
        if self.enabled.is_some() {
            fields.push("isEnabled");
        }
        if self.network_type.is_some() {
            fields.push("type");
        }
        if self.vlan_id.is_some() {
            fields.push("vlanId");
        }
        if self.dhcp.enabled.is_some() {
            fields.push("useDhcpScope");
        }
        if self.dhcp.gateway.is_some() || self.dhcp.prefix_length.is_some() {
            fields.extend([
                "dhcpScope/network",
                "dhcpScope/netmask",
                "dhcpScope/ipAddress",
            ]);
        }
        if self.dhcp.start.is_some() || self.dhcp.end.is_some() {
            fields.extend([
                "dhcpScope/ipAddressRange/start",
                "dhcpScope/ipAddressRange/end",
            ]);
        }
        if self.dhcp.domain_name.is_some() {
            fields.push("dhcpScope/domainName");
        }
        if self.dhcp.dns_mode.is_some()
            || self.dhcp.primary_dns.is_some()
            || self.dhcp.secondary_dns.is_some()
        {
            fields.push("dhcpScope/dns/dnsServerAssignationMode");
        }
        if self.dhcp.primary_dns.is_some() {
            fields.push("dhcpScope/dns/customPrimaryDns");
        }
        if self.dhcp.secondary_dns.is_some() {
            fields.push("dhcpScope/dns/customSecondaryDns");
        }
        fields
    }

    pub(super) fn apply(&self, body: &mut Value, creating: bool) -> Result<(), Error> {
        self.validate()?;
        if !body.is_object() {
            return Err(incomplete("wired network is not an object"));
        }
        if !creating
            && self.vlan_id.is_some()
            && body.get("vlanIdCanBeChanged").and_then(Value::as_bool) != Some(true)
        {
            return Err(usage("this network's VLAN ID cannot be changed"));
        }
        if let Some(name) = &self.name {
            body["wiredNetworkName"] = json!(name);
        }
        if let Some(enabled) = self.enabled {
            body["isEnabled"] = json!(enabled);
        }
        if let Some(kind) = self.network_type {
            body["type"] = json!(kind);
        }
        if let Some(vlan) = self.vlan_id {
            body["vlanId"] = json!(vlan);
        }
        self.dhcp.apply(body, creating)
    }
}

// Wired creation wizard, chunk-UGS2VARG.js @8245: qb access defaults
// @4960305 and mD type-specific QoS defaults @5143950. Identification
// updates do not apply these defaults; they retain the fetched configuration.
pub(super) fn apply_create_defaults(body: &mut Value, metadata: &Value) -> Result<(), Error> {
    let kind = body
        .get("type")
        .and_then(Value::as_str)
        .filter(|kind| matches!(*kind, "employee" | "guest" | "voice"))
        .ok_or_else(|| incomplete("wired creation type is unavailable"))?;
    let restricted = kind == "guest";
    let mut priority = "medium";
    if body.get("qos").is_some_and(|qos| !qos.is_null()) {
        if !body["qos"].is_object() {
            return Err(incomplete("wired creation QoS template is malformed"));
        }
        if let Some(defaults) = metadata
            .get("wiredNetworkTypeDefaults")
            .filter(|value| !value.is_null())
        {
            let defaults = defaults
                .as_array()
                .ok_or_else(|| incomplete("wired type defaults are malformed"))?;
            let mut matches = defaults
                .iter()
                .filter(|entry| entry.get("networkType").and_then(Value::as_str) == Some(kind));
            if let Some(entry) = matches.next() {
                if matches.next().is_some() {
                    return Err(incomplete("wired type defaults are ambiguous"));
                }
                priority = entry
                    .get("trafficPriority")
                    .and_then(Value::as_str)
                    .filter(|priority| matches!(*priority, "low" | "medium" | "high" | "veryHigh"))
                    .ok_or_else(|| incomplete("wired type traffic priority is unknown"))?;
            }
        }
        body["qos"]["trafficPriority"] = json!(priority);
    }
    body["isAccessRestricted"] = json!(restricted);
    body["isInternetAllowed"] = json!(true);
    Ok(())
}

pub(super) fn validate_create(body: &Value) -> Result<(), Error> {
    if body
        .get("wiredNetworkName")
        .and_then(Value::as_str)
        .is_none()
        || body.get("vlanId").and_then(Value::as_u64).is_none()
        || body.get("isEnabled").and_then(Value::as_bool).is_none()
        || !matches!(
            body.get("type").and_then(Value::as_str),
            Some("employee" | "guest" | "voice")
        )
        || body.get("useDhcpScope").and_then(Value::as_bool).is_none()
    {
        return Err(incomplete(
            "wired creation template has incomplete configuration",
        ));
    }
    if body["useDhcpScope"] == true {
        validate_scope(&body["dhcpScope"])?;
    }
    Ok(())
}

fn ipv4(value: Option<&Value>) -> Option<Ipv4Addr> {
    value?.as_str()?.parse().ok()
}

fn mask(value: Option<&Value>) -> Result<u32, Error> {
    let mask = ipv4(value)
        .map(u32::from)
        .ok_or_else(|| incomplete("DHCP netmask is unavailable"))?;
    let inverse = !mask;
    if inverse & inverse.wrapping_add(1) != 0 || mask.count_ones() > 30 || mask == 0 {
        return Err(usage(
            "DHCP netmask must be contiguous and leave usable host addresses",
        ));
    }
    Ok(mask)
}

pub(super) fn validate_scope(scope: &Value) -> Result<(), Error> {
    let network =
        ipv4(scope.get("network")).ok_or_else(|| incomplete("DHCP network is unavailable"))?;
    let mask = mask(scope.get("netmask"))?;
    let start = u32::from(network) & mask;
    let end = start | !mask;
    if u32::from(network) != start {
        return Err(usage("DHCP network must be a subnet base address"));
    }
    let gateway = ipv4(scope.get("ipAddress"));
    if scope.get("ipAddress").is_some_and(|value| !value.is_null()) && gateway.is_none() {
        return Err(incomplete("DHCP gateway address is malformed"));
    }
    if let Some(gateway) = gateway
        && !(start + 1..end).contains(&u32::from(gateway))
    {
        return Err(usage(
            "DHCP gateway must be a usable address within its subnet",
        ));
    }
    if let Some(range) = scope.get("ipAddressRange").filter(|value| !value.is_null()) {
        let low = ipv4(range.get("start"))
            .ok_or_else(|| incomplete("DHCP range start is unavailable"))?;
        let high =
            ipv4(range.get("end")).ok_or_else(|| incomplete("DHCP range end is unavailable"))?;
        if u32::from(low) > u32::from(high)
            || !(start + 1..end).contains(&u32::from(low))
            || !(start + 1..end).contains(&u32::from(high))
        {
            return Err(usage(
                "DHCP range must be ordered and within usable subnet addresses",
            ));
        }
        if gateway
            .is_some_and(|gateway| (u32::from(low)..=u32::from(high)).contains(&u32::from(gateway)))
        {
            return Err(usage("DHCP range must not include its gateway"));
        }
    }
    if let Some(reservations) = scope.get("ipReservations") {
        let reservations = reservations
            .as_array()
            .ok_or_else(|| incomplete("DHCP reservations are malformed"))?;
        for reservation in reservations {
            let address = ipv4(reservation.get("ipAddress"))
                .ok_or_else(|| incomplete("DHCP reservation address is unavailable"))?;
            if !(start + 1..end).contains(&u32::from(address)) || Some(address) == gateway {
                return Err(usage(
                    "existing DHCP reservations must remain usable within the subnet",
                ));
            }
            if gateway.is_some()
                && let Some(range) = scope.get("ipAddressRange")
            {
                let low = ipv4(range.get("start"))
                    .ok_or_else(|| incomplete("DHCP range start is unavailable"))?;
                let high = ipv4(range.get("end"))
                    .ok_or_else(|| incomplete("DHCP range end is unavailable"))?;
                if !(u32::from(low)..=u32::from(high)).contains(&u32::from(address)) {
                    return Err(usage(
                        "DHCP range changes must retain existing reservations",
                    ));
                }
            }
        }
    }
    Ok(())
}
