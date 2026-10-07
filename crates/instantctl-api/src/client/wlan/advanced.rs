//! Additional SSID fields from the portal's ey1, Zy and jy serializers.
use std::collections::HashSet;

use serde_json::{Value, json};

use super::{Client, Security, incomplete, route, usage};
use crate::{Error, TokenSource, client::reads};

#[derive(Debug)]
pub enum Binding {
    Network(String),
    Vlan(u16),
}

#[derive(Debug)]
pub enum AccessPoints {
    All,
    Selected(Vec<String>),
}

#[derive(Clone, Copy, Debug)]
pub enum TrafficPriority {
    Off,
    Low,
    Medium,
    High,
    VeryHigh,
}

#[derive(Default, Debug)]
pub struct AdvancedPatch {
    pub binding: Option<Binding>,
    pub radius_profile: Option<String>,
    pub access_points: Option<AccessPoints>,
    pub captive_portal: Option<bool>,
    pub legacy_rates: Option<bool>,
    pub wifi6: Option<bool>,
    pub ofdma: Option<bool>,
    pub wifi7: Option<bool>,
    pub mlo: Option<bool>,
    pub multicast_optimization: Option<bool>,
    pub broadcast_all_bands: Option<bool>,
    pub traffic_priority: Option<TrafficPriority>,
}

impl AdvancedPatch {
    pub(super) fn validate(
        &self,
        security: Option<Security>,
        bands: Option<super::Bands>,
        guest: Option<bool>,
        has_psk: bool,
    ) -> Result<(), Error> {
        let enterprise = security.is_some_and(Security::is_enterprise);
        if enterprise && self.radius_profile.is_none() {
            return Err(usage("enterprise security requires --radius-profile"));
        }
        if enterprise && (guest == Some(true) || has_psk) {
            return Err(usage(
                "enterprise security requires an employee network and does not use a passphrase",
            ));
        }
        if self.radius_profile.is_some()
            && (guest == Some(true) || security.is_some_and(|s| !s.is_enterprise()))
        {
            return Err(usage("a RADIUS profile requires enterprise security"));
        }
        if self.radius_profile.as_ref().is_some_and(|s| s.is_empty()) {
            return Err(usage("RADIUS profile selector must not be empty"));
        }
        if let Some(binding) = &self.binding {
            match binding {
                Binding::Network(s) if s.is_empty() => {
                    return Err(usage("wired network selector must not be empty"));
                }
                Binding::Vlan(vlan)
                    if !(1..=4092).contains(vlan) || (3333..=3349).contains(vlan) =>
                {
                    return Err(usage(
                        "select an existing VLAN from 1 to 4092 outside 3333 to 3349",
                    ));
                }
                _ => {}
            }
        }
        if let Some(AccessPoints::Selected(selectors)) = &self.access_points
            && (selectors.is_empty()
                || selectors.iter().any(String::is_empty)
                || selectors.iter().collect::<HashSet<_>>().len() != selectors.len())
        {
            return Err(usage("select at least one unique AP"));
        }
        if self.captive_portal == Some(true) && guest == Some(false) {
            return Err(usage("captive portal requires a guest network"));
        }
        if self.ofdma == Some(true) && self.wifi6 == Some(false) {
            return Err(usage("OFDMA requires Wi-Fi 6"));
        }
        if self.mlo == Some(true)
            && (bands.is_some_and(|b| !b.six)
                || security.is_some_and(|s| {
                    !matches!(s, Security::Wpa3Personal | Security::Wpa3Enterprise)
                })
                || self.wifi7 == Some(false)
                || self.wifi6 == Some(false))
        {
            return Err(usage("MLO requires 6 GHz, WPA3, Wi-Fi 6 and Wi-Fi 7"));
        }
        Ok(())
    }

    pub(super) fn fields(&self) -> Vec<&'static str> {
        let mut fields = Vec::new();
        for (present, keys) in [
            (
                self.binding.is_some(),
                &["wiredNetworkId", "ipAddressingMode"][..],
            ),
            (self.radius_profile.is_some(), &["radiusProfileId"]),
            (self.access_points.is_some(), &["accessPoints"]),
            (
                self.captive_portal.is_some(),
                &["isCaptivePortalEnabled", "isGuestPortalEnabled"],
            ),
            (self.legacy_rates.is_some(), &["isLegacy80211bRatesEnabled"]),
            (self.wifi6.is_some(), &["isHighEfficiency11axEnabled"]),
            (self.ofdma.is_some(), &["isHighEfficiency11axOfdmaEnabled"]),
            (
                self.wifi7.is_some(),
                &["isExtremelyHighThroughput11beEnabled"],
            ),
            (
                self.mlo.is_some(),
                &["isExtremelyHighThroughput11beMloEnabled"],
            ),
            (
                self.multicast_optimization.is_some(),
                &["isDynamicMulticastOptimizationEnabled"],
            ),
            (
                self.broadcast_all_bands.is_some(),
                &["isBroadcastOnAllBoundApsOnAllBands"],
            ),
            (self.traffic_priority.is_some(), &["qos"]),
        ] {
            if present {
                fields.extend(keys);
            }
        }
        fields
    }

    /// Resolve selectors from authoritative, complete collections before planning.
    pub(super) async fn resolve<T: TokenSource>(
        &mut self,
        client: &Client<T>,
        site: &str,
        body: &Value,
    ) -> Result<(), Error> {
        if let Some(binding) = &self.binding {
            let networks = crate::client::network::list(client, site).await?;
            let selected = match binding {
                Binding::Network(selector) => crate::client::network::select(&networks, selector)?,
                Binding::Vlan(vlan) => {
                    let mut matches = networks
                        .iter()
                        .filter(|n| n.summary()["vlan_id"].as_u64() == Some(u64::from(*vlan)));
                    let selected = matches
                        .next()
                        .ok_or_else(|| usage("the selected VLAN does not exist at this site"))?;
                    if matches.next().is_some() {
                        return Err(usage(
                            "VLAN is ambiguous; select a wired network by identifier",
                        ));
                    }
                    selected
                }
            };
            match selected.summary()["enabled"].as_bool() {
                Some(true) => {}
                Some(false) => return Err(usage("the selected wired network is disabled")),
                None => return Err(incomplete("wired network enabled state is unavailable")),
            }
            self.binding = Some(Binding::Network(selected.id().to_owned()));
        }
        if let Some(selector) = &self.radius_profile {
            let profiles = crate::client::access::radius::list(client, site).await?;
            let selected = crate::client::access::radius::show(&profiles, selector)?;
            self.radius_profile = Some(
                selected["id"]
                    .as_str()
                    .ok_or_else(|| incomplete("RADIUS profile identity is missing"))?
                    .to_owned(),
            );
        }
        let required: Vec<_> = [
            (self.radius_profile.is_some(), "radius-profiles"),
            (self.wifi6 == Some(true), "high-efficiency-11ax"),
            (self.ofdma == Some(true), "high-efficiency-11ax-ofdma"),
            (self.wifi7 == Some(true) || self.mlo == Some(true), "wifi-7"),
            (self.mlo == Some(true), "wifi-6e"),
            (
                self.multicast_optimization == Some(true),
                "multicast-optimizations",
            ),
        ]
        .into_iter()
        .filter_map(|(enabled, name)| enabled.then_some(name))
        .collect();
        if !required.is_empty() {
            let capabilities = client.capabilities(site).await?;
            if required
                .iter()
                .any(|required| !capabilities.iter().any(|c| c == required))
            {
                return Err(usage(
                    "the site does not advertise support for the requested WLAN feature",
                ));
            }
        }
        // Zy stores the device entity ID, which Qs may take from raw API id
        // rather than macAddress. Resolve a MAC through inventory when needed.
        if let Some(AccessPoints::Selected(selectors)) = &mut self.access_points {
            let unresolved_mac = |selector: &str| {
                reads::is_mac(selector)
                    && !body["accessPoints"].as_array().is_some_and(|aps| {
                        aps.iter().any(|ap| {
                            ap["deviceId"]
                                .as_str()
                                .is_some_and(|id| id.eq_ignore_ascii_case(selector))
                        })
                    })
            };
            if selectors.iter().any(|s| unresolved_mac(s)) {
                let payload = client.get(&route(site, "inventory")?).await?;
                let devices: Vec<Value> = reads::parse_elements(&payload)?;
                let count = Some(devices.len() as u64);
                let pending = payload.get("pendingAvailability");
                if payload["kind"] != "resourceList"
                    || payload["totalCount"].as_u64() != count
                    || payload["matchingFilterCount"].as_u64() != count
                    || !pending.is_some_and(|v| {
                        v.is_null()
                            || v == false
                            || v == 0
                            || v.as_array().is_some_and(Vec::is_empty)
                            || v.as_object().is_some_and(serde_json::Map::is_empty)
                    })
                {
                    return Err(incomplete(
                        "AP inventory is incomplete or has pending availability",
                    ));
                }
                reads::unique(
                    devices.iter().map(|d| d["id"].as_str().unwrap_or_default()),
                    false,
                )?;
                reads::unique(
                    devices
                        .iter()
                        .map(|d| d["macAddress"].as_str().unwrap_or_default()),
                    true,
                )?;
                for selector in selectors.iter_mut().filter(|s| unresolved_mac(s)) {
                    let device = devices
                        .iter()
                        .find(|d| {
                            d["macAddress"]
                                .as_str()
                                .is_some_and(|mac| mac.eq_ignore_ascii_case(selector))
                        })
                        .ok_or_else(|| {
                            Error::new(crate::ErrorKind::NotFound, "no AP matches the supplied MAC")
                        })?;
                    *selector = device["id"]
                        .as_str()
                        .ok_or_else(|| incomplete("AP inventory identity is missing"))?
                        .to_owned();
                }
            }
        }
        Ok(())
    }

    pub(super) fn apply(self, body: &mut Value) -> Result<(), Error> {
        if let Some(Binding::Network(id)) = self.binding {
            body["wiredNetworkId"] = json!(id);
            body["ipAddressingMode"] = json!("network");
        }
        if let Some(id) = self.radius_profile {
            if body["authentication"] != "802.1x" || body["type"] != "employee" {
                return Err(usage(
                    "a RADIUS profile requires an employee network with enterprise security",
                ));
            }
            body["radiusProfileId"] = json!(id);
        }
        if let Some(selection) = self.access_points {
            bind_aps(body, selection)?;
        }
        if let Some(enabled) = self.captive_portal {
            if enabled && body["type"] != "guest" {
                return Err(usage("captive portal requires a guest network"));
            }
            body["isCaptivePortalEnabled"] = json!(enabled);
            body["isGuestPortalEnabled"] = json!(enabled);
        }
        for (value, key) in [
            (self.legacy_rates, "isLegacy80211bRatesEnabled"),
            (self.wifi6, "isHighEfficiency11axEnabled"),
            (self.ofdma, "isHighEfficiency11axOfdmaEnabled"),
            (self.wifi7, "isExtremelyHighThroughput11beEnabled"),
            (self.mlo, "isExtremelyHighThroughput11beMloEnabled"),
            (
                self.multicast_optimization,
                "isDynamicMulticastOptimizationEnabled",
            ),
            (
                self.broadcast_all_bands,
                "isBroadcastOnAllBoundApsOnAllBands",
            ),
        ] {
            if let Some(value) = value {
                body[key] = json!(value);
            }
        }
        if (self.ofdma.is_some() || self.wifi6.is_some())
            && body["isHighEfficiency11axOfdmaEnabled"] == true
            && body["isHighEfficiency11axEnabled"] != true
        {
            return Err(usage("OFDMA requires Wi-Fi 6"));
        }
        if (self.mlo.is_some() || self.wifi7.is_some() || self.wifi6.is_some())
            && body["isExtremelyHighThroughput11beMloEnabled"] == true
            && (body["isAvailableOn6GHzRadioBand"] != true
                || body["security"] != "wpa3"
                || body["isExtremelyHighThroughput11beEnabled"] != true
                || body["isHighEfficiency11axEnabled"] != true)
        {
            return Err(usage("MLO requires 6 GHz, WPA3, Wi-Fi 6 and Wi-Fi 7"));
        }
        if let Some(priority) = self.traffic_priority {
            if !matches!(priority, TrafficPriority::Off)
                && body.pointer("/capabilities/qosTrafficPriority") == Some(&json!(false))
            {
                return Err(usage(
                    "traffic priority is unavailable on this wireless network",
                ));
            }
            let qos = body
                .get_mut("qos")
                .and_then(Value::as_object_mut)
                .ok_or_else(|| incomplete("wireless QoS configuration is unavailable"))?;
            let wire = match priority {
                TrafficPriority::Off => None,
                TrafficPriority::Low => Some("low"),
                TrafficPriority::Medium => Some("medium"),
                TrafficPriority::High => Some("high"),
                TrafficPriority::VeryHigh => Some("veryHigh"),
            };
            qos.insert("isTrafficPriorityEnabled".into(), json!(wire.is_some()));
            if let Some(wire) = wire {
                qos.insert("trafficPriority".into(), json!(wire));
            }
        }
        Ok(())
    }
}

fn bind_aps(body: &mut Value, selection: AccessPoints) -> Result<(), Error> {
    let aps = body
        .get_mut("accessPoints")
        .and_then(Value::as_array_mut)
        .filter(|aps| !aps.is_empty())
        .ok_or_else(|| incomplete("wireless AP bindings are unavailable"))?;
    let mut ids = HashSet::new();
    for ap in aps.iter() {
        let id = ap["deviceId"]
            .as_str()
            .filter(|id| !id.is_empty())
            .ok_or_else(|| incomplete("AP binding identity is missing"))?;
        if !ids.insert(id.to_ascii_lowercase()) || !ap["isBoundToNetwork"].is_boolean() {
            return Err(incomplete(
                "AP bindings have duplicate identities or missing state",
            ));
        }
    }
    let mut selected = HashSet::new();
    match selection {
        AccessPoints::All => selected.extend(0..aps.len()),
        AccessPoints::Selected(selectors) => {
            for selector in selectors {
                let by_id = aps.iter().position(|ap| {
                    ap["deviceId"].as_str().is_some_and(|id| {
                        id == selector
                            || (reads::is_mac(&selector) && id.eq_ignore_ascii_case(&selector))
                    })
                });
                let index = if let Some(index) = by_id {
                    index
                } else {
                    let mut matches = aps
                        .iter()
                        .enumerate()
                        .filter(|(_, ap)| ap["deviceName"].as_str() == Some(selector.as_str()));
                    let (index, _) = matches.next().ok_or_else(|| {
                        Error::new(
                            crate::ErrorKind::NotFound,
                            "no AP matches the supplied selector",
                        )
                    })?;
                    if matches.next().is_some() {
                        return Err(usage("AP name is ambiguous; select by identifier"));
                    }
                    index
                };
                if !selected.insert(index) {
                    return Err(usage("AP selectors refer to the same AP more than once"));
                }
            }
        }
    }
    for (index, ap) in aps.iter_mut().enumerate() {
        ap["isBoundToNetwork"] = json!(selected.contains(&index));
    }
    Ok(())
}
