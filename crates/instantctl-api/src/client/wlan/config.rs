use std::{collections::HashSet, net::Ipv4Addr};

use serde_json::{Value, json};

use super::{incomplete, usage};
use crate::{Error, secret::SecretString};

// ey1/Y9.prepareData in the portal bundle. No id or telemetry is sent on POST.
pub(super) const FIELDS: &[&str] = &[
    "authentication",
    "security",
    "networkName",
    "isEnabled",
    "useVlan",
    "vlanId",
    "wiredNetworkId",
    "isSsidHidden",
    "isWireless",
    "type",
    "preSharedKey",
    "isCaptivePortalEnabled",
    "isGuestPortalEnabled",
    "ipAddressingMode",
    "isAvailableOn24GHzRadioBand",
    "isAvailableOn5GHzRadioBand",
    "isAvailableOn6GHzRadioBand",
    "isLegacy80211bRatesEnabled",
    "isHighEfficiency11axEnabled",
    "isHighEfficiency11axOfdmaEnabled",
    "isExtremelyHighThroughput11beEnabled",
    "isExtremelyHighThroughput11beMloEnabled",
    "isDynamicMulticastOptimizationEnabled",
    "isBroadcastOnAllBoundApsOnAllBands",
    "accessPoints",
    "schedule",
    "weekSchedule",
    "activeSchedule",
    "isBandwidthLimitEnabled",
    "bandwidthLimitMode",
    "perClientBandwidthLimitInMbps",
    "perClientUploadBandwidthLimitInMbps",
    "perNetworkDownstreamBandwidthLimitInMbps",
    "perNetworkUpstreamBandwidthLimitInMbps",
    "qos",
    "isAccessRestricted",
    "isInternetAllowed",
    "isIntraSubnetTrafficAllowed",
    "isSpecificDestinationsAllowed",
    "allowedDestinations",
    "radiusProfileId",
    "isRadiusAccountingEnabled",
    "radiusServerPrimary",
    "isSecondaryRadiusServerEnabled",
    "radiusServerSecondary",
    "radiusNasIdentifier",
    "radiusNasIpSettings",
    "dhcpScope",
    "allowList",
];

#[derive(Clone, Copy, Debug)]
pub enum Security {
    Open,
    EnhancedOpen,
    Wpa2Personal,
    Wpa3Personal,
    Wpa2Enterprise,
    Wpa3Enterprise,
}

impl Security {
    fn wire(self) -> (&'static str, &'static str) {
        match self {
            Self::Open => ("none", "open"),
            Self::EnhancedOpen => ("none", "owe"),
            Self::Wpa2Personal => ("psk", "wpa2"),
            Self::Wpa3Personal => ("psk", "wpa3"),
            Self::Wpa2Enterprise => ("802.1x", "wpa2"),
            Self::Wpa3Enterprise => ("802.1x", "wpa3"),
        }
    }

    pub(super) fn is_enterprise(self) -> bool {
        matches!(self, Self::Wpa2Enterprise | Self::Wpa3Enterprise)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Bands {
    pub two_four: bool,
    pub five: bool,
    pub six: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Day {
    Monday,
    Tuesday,
    Wednesday,
    Thursday,
    Friday,
    Saturday,
    Sunday,
}

impl Day {
    fn wire(self) -> &'static str {
        match self {
            Self::Monday => "monday",
            Self::Tuesday => "tuesday",
            Self::Wednesday => "wednesday",
            Self::Thursday => "thursday",
            Self::Friday => "friday",
            Self::Saturday => "saturday",
            Self::Sunday => "sunday",
        }
    }
}

#[derive(Debug)]
pub enum Schedule {
    Off,
    Always,
    Timed {
        days: Vec<Day>,
        start: String,
        end: String,
    },
}

#[derive(Debug)]
pub enum Bandwidth {
    Off,
    PerClient { download: u64, upload: u64 },
    PerNetwork { download: u64, upload: u64 },
}

#[derive(Default, Debug)]
pub struct Patch {
    pub name: Option<String>,
    pub enabled: Option<bool>,
    pub hidden: Option<bool>,
    pub security: Option<Security>,
    pub passphrase: Option<SecretString>,
    pub bands: Option<Bands>,
    pub schedule: Option<Schedule>,
    pub bandwidth: Option<Bandwidth>,
    pub guest: Option<bool>,
    pub restrict_access: Option<bool>,
    pub internet: Option<bool>,
    pub intra_subnet_traffic: Option<bool>,
    pub allowed_destinations: Option<Vec<String>>,
    pub advanced: super::AdvancedPatch,
}

impl Patch {
    /// Fixed error text ensures invalid PSKs never appear in diagnostics.
    pub fn validate(&self) -> Result<(), Error> {
        self.advanced.validate(
            self.security,
            self.bands,
            self.guest,
            self.passphrase.is_some(),
        )?;
        if let Some(name) = &self.name
            && (name.is_empty() || name.encode_utf16().count() > 32 || name.contains('"'))
        {
            return Err(usage(
                "SSID must contain 1 to 32 characters without double quotes",
            ));
        }
        if let Some(passphrase) = &self.passphrase {
            validate_psk(passphrase.expose_secret())?;
        }
        if let Some(bands) = self.bands {
            if !bands.two_four && !bands.five && !bands.six {
                return Err(usage("select at least one radio band"));
            }
            if bands.six
                && self.security.is_some_and(|security| {
                    !matches!(
                        security,
                        Security::Wpa3Personal | Security::Wpa3Enterprise | Security::EnhancedOpen
                    )
                })
            {
                return Err(usage("6 GHz requires WPA3 or enhanced open security"));
            }
        }
        if self.passphrase.is_some()
            && self
                .security
                .is_some_and(|security| matches!(security, Security::Open | Security::EnhancedOpen))
        {
            return Err(usage("open security does not use a passphrase"));
        }
        if let Some(Schedule::Timed { days, start, end }) = &self.schedule {
            if days.is_empty() || days.iter().collect::<HashSet<_>>().len() != days.len() {
                return Err(usage("a timed schedule requires unique weekdays"));
            }
            if !valid_time(start) || !valid_time(end) || start == end {
                return Err(usage(
                    "schedule start and end must be different 24-hour HH:mm times",
                ));
            }
        }
        if let Some(
            Bandwidth::PerClient { download, upload } | Bandwidth::PerNetwork { download, upload },
        ) = self.bandwidth
            && (download == 0 || upload == 0)
        {
            return Err(usage("bandwidth limits must be positive Mbps values"));
        }
        if let Some(destinations) = &self.allowed_destinations {
            if destinations.len() > 5
                || destinations.iter().collect::<HashSet<_>>().len() != destinations.len()
            {
                return Err(usage(
                    "specify at most five unique allowed IPv4 destinations",
                ));
            }
            for destination in destinations {
                if destination
                    .parse::<Ipv4Addr>()
                    .ok()
                    .is_none_or(|ip| ip.octets()[0] == 0)
                {
                    return Err(usage("allowed destinations must be host IPv4 addresses"));
                }
            }
        }
        Ok(())
    }

    pub(super) fn fields(&self) -> Vec<&'static str> {
        let mut fields = Vec::new();
        for (present, keys) in [
            (self.name.is_some(), &["networkName"][..]),
            (self.enabled.is_some(), &["isEnabled"]),
            (self.hidden.is_some(), &["isSsidHidden"]),
            (
                self.security.is_some(),
                &["authentication", "security", "preSharedKey"],
            ),
            (self.passphrase.is_some(), &["preSharedKey"]),
            (
                self.bands.is_some(),
                &[
                    "isAvailableOn24GHzRadioBand",
                    "isAvailableOn5GHzRadioBand",
                    "isAvailableOn6GHzRadioBand",
                ],
            ),
            (self.schedule.is_some(), &["activeSchedule", "schedule"]),
            (self.guest.is_some(), &["type"]),
            (self.restrict_access.is_some(), &["isAccessRestricted"]),
            (self.internet.is_some(), &["isInternetAllowed"]),
            (
                self.intra_subnet_traffic.is_some(),
                &["isIntraSubnetTrafficAllowed"],
            ),
            (
                self.allowed_destinations.is_some(),
                &["isSpecificDestinationsAllowed", "allowedDestinations"],
            ),
        ] {
            if present {
                fields.extend(keys);
            }
        }
        if let Some(bandwidth) = &self.bandwidth {
            fields.push("isBandwidthLimitEnabled");
            match bandwidth {
                Bandwidth::Off => {}
                Bandwidth::PerClient { .. } => fields.extend([
                    "bandwidthLimitMode",
                    "perClientBandwidthLimitInMbps",
                    "perClientUploadBandwidthLimitInMbps",
                ]),
                Bandwidth::PerNetwork { .. } => fields.extend([
                    "bandwidthLimitMode",
                    "perNetworkDownstreamBandwidthLimitInMbps",
                    "perNetworkUpstreamBandwidthLimitInMbps",
                ]),
            }
        }
        fields.extend(self.advanced.fields());
        fields.sort_unstable();
        fields.dedup();
        fields
    }

    pub(super) fn is_empty(&self) -> bool {
        self.fields().is_empty()
    }

    pub(super) fn apply(self, body: &mut Value) -> Result<(), Error> {
        if let Some(name) = self.name {
            body["networkName"] = json!(name);
        }
        if let Some(enabled) = self.enabled {
            body["isEnabled"] = json!(enabled);
        }
        if let Some(hidden) = self.hidden {
            body["isSsidHidden"] = json!(hidden);
        }
        if let Some(security) = self.security {
            let (authentication, security) = security.wire();
            body["authentication"] = json!(authentication);
            body["security"] = json!(security);
            if authentication != "psk" {
                body["preSharedKey"] = json!("");
            }
        }
        if let Some(passphrase) = self.passphrase {
            if body["authentication"] != "psk" {
                return Err(usage(
                    "the selected wireless network does not use a personal passphrase",
                ));
            }
            body["preSharedKey"] = json!(passphrase.expose_secret());
        }
        if self.security.is_some() && body["authentication"] == "psk" {
            validate_psk(
                body["preSharedKey"]
                    .as_str()
                    .ok_or_else(|| usage("personal security requires a passphrase"))?,
            )?;
        }
        if let Some(bands) = self.bands {
            body["isAvailableOn24GHzRadioBand"] = json!(bands.two_four);
            body["isAvailableOn5GHzRadioBand"] = json!(bands.five);
            body["isAvailableOn6GHzRadioBand"] = json!(bands.six);
        }
        if (self.security.is_some() || self.bands.is_some())
            && body["isAvailableOn6GHzRadioBand"] == true
            && !matches!(body["security"].as_str(), Some("wpa3" | "owe"))
        {
            return Err(usage("6 GHz requires WPA3 or enhanced open security"));
        }
        if let Some(schedule) = self.schedule {
            let (mode, days, range) = match schedule {
                Schedule::Off => ("none", vec![], json!({"enabled": false})),
                Schedule::Always => ("simple", all_days(), json!({"enabled": false})),
                Schedule::Timed { days, start, end } => (
                    "simple",
                    days,
                    json!({"enabled": true, "startTime": start, "endTime": end}),
                ),
            };
            body["activeSchedule"] = json!(mode);
            let schedule = body
                .as_object_mut()
                .ok_or_else(|| incomplete("wireless network is not an object"))?
                .entry("schedule")
                .or_insert_with(|| json!({}));
            let schedule = schedule
                .as_object_mut()
                .ok_or_else(|| incomplete("wireless schedule is malformed"))?;
            schedule.insert(
                "activeDays".into(),
                json!(days.into_iter().map(Day::wire).collect::<Vec<_>>()),
            );
            let time_range = schedule
                .entry("activeTimeRange")
                .or_insert_with(|| json!({}));
            let time_range = time_range
                .as_object_mut()
                .ok_or_else(|| incomplete("wireless schedule time range is malformed"))?;
            for field in ["enabled", "startTime", "endTime"] {
                time_range.remove(field);
            }
            time_range.extend(
                range
                    .as_object()
                    .ok_or_else(|| incomplete("wireless schedule time range is malformed"))?
                    .clone(),
            );
        }
        if let Some(bandwidth) = self.bandwidth {
            body["isBandwidthLimitEnabled"] = json!(!matches!(bandwidth, Bandwidth::Off));
            match bandwidth {
                Bandwidth::Off => {}
                Bandwidth::PerClient { download, upload } => {
                    body["bandwidthLimitMode"] = json!("perClient");
                    body["perClientBandwidthLimitInMbps"] = json!(download);
                    body["perClientUploadBandwidthLimitInMbps"] = json!(upload);
                }
                Bandwidth::PerNetwork { download, upload } => {
                    body["bandwidthLimitMode"] = json!("perNetwork");
                    body["perNetworkDownstreamBandwidthLimitInMbps"] = json!(download);
                    body["perNetworkUpstreamBandwidthLimitInMbps"] = json!(upload);
                }
            }
        }
        if let Some(guest) = self.guest {
            if !guest
                && (body["isGuestPortalEnabled"] == true || body["isCaptivePortalEnabled"] == true)
                && self.advanced.captive_portal != Some(false)
            {
                return Err(usage(
                    "changing a portal-enabled network to employee requires --captive-portal false",
                ));
            }
            body["type"] = json!(if guest { "guest" } else { "employee" });
        }
        if (self.security.is_some() || self.guest.is_some())
            && body["authentication"] == "802.1x"
            && body["type"] != "employee"
        {
            return Err(usage("enterprise security requires an employee network"));
        }
        if let Some(restrict) = self.restrict_access {
            body["isAccessRestricted"] = json!(restrict);
        }
        if let Some(internet) = self.internet {
            body["isInternetAllowed"] = json!(internet);
        }
        if let Some(intra_subnet) = self.intra_subnet_traffic {
            body["isIntraSubnetTrafficAllowed"] = json!(intra_subnet);
        }
        if let Some(destinations) = self.allowed_destinations {
            body["isSpecificDestinationsAllowed"] = json!(!destinations.is_empty());
            body["allowedDestinations"] = json!(destinations);
        }
        // Re-check the merged object, so flags cannot bypass dependencies by
        // inheriting incompatible fields from a fetched network or template.
        self.advanced.apply(body)?;
        if (self.security.is_some() || self.bands.is_some())
            && body["isExtremelyHighThroughput11beMloEnabled"] == true
            && (body["isAvailableOn6GHzRadioBand"] != true || body["security"] != "wpa3")
        {
            return Err(usage("MLO requires 6 GHz and WPA3"));
        }
        Ok(())
    }
}

pub(super) fn validate_create(body: &Value) -> Result<(), Error> {
    if !matches!(
        (body["authentication"].as_str(), body["security"].as_str()),
        (Some("none"), Some("open" | "owe")) | (Some("psk" | "802.1x"), Some("wpa2" | "wpa3"))
    ) || !matches!(body["type"].as_str(), Some("employee" | "guest" | "voice"))
        || !matches!(
            body["ipAddressingMode"].as_str(),
            Some("network" | "internal")
        )
    {
        return Err(incomplete(
            "wireless creation template has missing or unknown configuration enums",
        ));
    }
    if body["authentication"] == "psk" {
        validate_psk(
            body["preSharedKey"]
                .as_str()
                .ok_or_else(|| usage("personal security requires a passphrase"))?,
        )?;
    }
    if body["isAvailableOn6GHzRadioBand"] == true
        && !matches!(body["security"].as_str(), Some("wpa3" | "owe"))
    {
        return Err(usage("6 GHz requires WPA3 or enhanced open security"));
    }
    if ![
        "isAvailableOn24GHzRadioBand",
        "isAvailableOn5GHzRadioBand",
        "isAvailableOn6GHzRadioBand",
    ]
    .iter()
    .any(|field| body[*field] == true)
    {
        return Err(incomplete(
            "wireless creation template has no active radio band",
        ));
    }
    Ok(())
}

/// The portal's create serializer omits fields that do not apply to the
/// selected mode. Updates deliberately retain the complete fetched object.
pub(super) fn prepare_create(body: &mut Value) -> Result<(), Error> {
    const RADIUS: &[&str] = &[
        "radiusProfileId",
        "isRadiusAccountingEnabled",
        "radiusServerPrimary",
        "isSecondaryRadiusServerEnabled",
        "radiusServerSecondary",
        "radiusNasIdentifier",
        "radiusNasIpSettings",
    ];
    const PER_CLIENT: &[&str] = &[
        "perClientBandwidthLimitInMbps",
        "perClientUploadBandwidthLimitInMbps",
    ];
    const PER_NETWORK: &[&str] = &[
        "perNetworkDownstreamBandwidthLimitInMbps",
        "perNetworkUpstreamBandwidthLimitInMbps",
    ];
    let object = body
        .as_object_mut()
        .ok_or_else(|| incomplete("wireless template is not an object"))?;
    if object.get("ipAddressingMode").and_then(Value::as_str) != Some("internal") {
        object.remove("dhcpScope");
    }
    if object.get("authentication").and_then(Value::as_str) != Some("802.1x") {
        for field in RADIUS {
            object.remove(*field);
        }
    }
    let allow_list = object.get("allowList").filter(|allow_list| {
        object.get("type").and_then(Value::as_str) == Some("employee")
            && object.get("authentication").and_then(Value::as_str) == Some("psk")
            && matches!(allow_list.get("allowListState").and_then(Value::as_str),
                Some("allowed" | "maxEntityAllowedAllowListReached" | "maxAllowedClientsReached"))
    }).map(|allow_list| json!({"id":allow_list.get("id").and_then(Value::as_str).unwrap_or(""),
        "isAllowListEnabled":allow_list.get("isAllowListEnabled").and_then(Value::as_bool).unwrap_or(false)}));
    object.remove("allowList");
    if let Some(allow_list) = allow_list {
        object.insert("allowList".into(), allow_list);
    }
    if object.get("type").and_then(Value::as_str) == Some("guest") {
        let portal = object
            .get("isGuestPortalEnabled")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        object.insert("isGuestPortalEnabled".into(), json!(portal));
        object.insert("isCaptivePortalEnabled".into(), json!(portal));
    } else {
        object.remove("isGuestPortalEnabled");
        object.insert("isCaptivePortalEnabled".into(), json!(false));
    }
    let enabled = object
        .get("isBandwidthLimitEnabled")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    object.insert("isBandwidthLimitEnabled".into(), json!(enabled));
    let unused = if !enabled {
        object.remove("bandwidthLimitMode");
        [PER_CLIENT, PER_NETWORK].concat()
    } else {
        match object.get("bandwidthLimitMode").and_then(Value::as_str) {
            Some("perClient") => PER_NETWORK.to_vec(),
            Some("perNetwork") => PER_CLIENT.to_vec(),
            _ => return Err(incomplete("wireless bandwidth mode is missing or unknown")),
        }
    };
    for field in unused {
        object.remove(field);
    }
    // ey1/P9/R6 hydrate nullable create metadata before prepareData. Sending
    // the raw null schedule produces a different (and rejected) wire object.
    for field in [
        "isLegacy80211bRatesEnabled",
        "isSpecificDestinationsAllowed",
    ] {
        if object.get(field).is_none_or(Value::is_null) {
            object.insert(field.into(), json!(false));
        }
    }
    if object.get("activeSchedule").is_none_or(Value::is_null) {
        object.insert("activeSchedule".into(), json!("none"));
    }
    if object.get("schedule").is_none_or(Value::is_null) {
        object.insert(
            "schedule".into(),
            json!({
                "activeDays": ["monday", "tuesday", "wednesday", "thursday", "friday"],
                "activeTimeRange": {"enabled": false},
            }),
        );
    }
    if object.get("weekSchedule").is_none_or(Value::is_null) {
        let days = all_days()
            .into_iter()
            .map(|day| {
                (
                    day.wire().to_owned(),
                    json!({
                        "enabled": false, "activeAllDay": true,
                        "startTime": "09:00", "endTime": "17:00",
                    }),
                )
            })
            .collect::<serde_json::Map<_, _>>();
        object.insert(
            "weekSchedule".into(),
            json!({"schedulePerWeekdayMap": days}),
        );
    }
    if object.get("qos").is_some_and(Value::is_null) {
        object.remove("qos");
    }
    if let Some(qos) = object.get_mut("qos").and_then(Value::as_object_mut) {
        for field in [
            "isBandwidthLimitEnabled",
            "isDownloadBandwidthLimitEnabled",
            "isUploadBandwidthLimitEnabled",
            "isTrafficPriorityEnabled",
        ] {
            if qos.get(field).is_none_or(Value::is_null) {
                qos.insert(field.into(), json!(false));
            }
        }
    }
    if let Some(schedule) = object.get_mut("schedule") {
        let days = schedule.get("activeDays").cloned().unwrap_or(Value::Null);
        *schedule = super::schedule_configuration(schedule);
        schedule["activeDays"] = days;
    }
    Ok(())
}

fn validate_psk(passphrase: &str) -> Result<(), Error> {
    // Portal PRESHARED_KEY_CHARSET_REGEX, excluding double quotes/backticks.
    if !(8..=63).contains(&passphrase.len())
        || !passphrase
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b" ~\\/|!@#$%?^&*()_+=-{}[];:'<>,.".contains(&b))
    {
        return Err(usage(
            "passphrase must contain 8 to 63 allowed ASCII characters",
        ));
    }
    Ok(())
}

fn valid_time(time: &str) -> bool {
    time.len() == 5
        && time.as_bytes()[2] == b':'
        && time
            .bytes()
            .enumerate()
            .all(|(i, b)| i == 2 || b.is_ascii_digit())
        && time[..2].parse::<u8>().is_ok_and(|hours| hours < 24)
        && time[3..].parse::<u8>().is_ok_and(|minutes| minutes < 60)
}

fn all_days() -> Vec<Day> {
    vec![
        Day::Monday,
        Day::Tuesday,
        Day::Wednesday,
        Day::Thursday,
        Day::Friday,
        Day::Saturday,
        Day::Sunday,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn psk_matches_portal_charset_and_never_echoes_invalid_values() {
        for secret in [
            "seven!!",
            "password\"quote",
            "password`tick",
            "password🙂",
            &"a".repeat(64),
        ] {
            let patch = Patch {
                passphrase: Some(SecretString::new(secret)),
                ..Patch::default()
            };
            let error = patch.validate().unwrap_err();
            for output in [
                error.to_string(),
                format!("{error:?}"),
                format!("{patch:?}"),
            ] {
                assert!(!output.contains(secret));
            }
        }
        validate_psk(" ~\\/|!@#$%?^&*()_+=-{}[];:'<>,.").unwrap();
        validate_psk(&"a".repeat(63)).unwrap();
    }

    #[test]
    fn invalid_schedule_bandwidth_and_destinations_are_local_errors() {
        for patch in [
            Patch {
                schedule: Some(Schedule::Timed {
                    days: vec![Day::Monday],
                    start: "24:00".into(),
                    end: "02:00".into(),
                }),
                ..Patch::default()
            },
            Patch {
                schedule: Some(Schedule::Timed {
                    days: vec![Day::Monday, Day::Monday],
                    start: "01:00".into(),
                    end: "02:00".into(),
                }),
                ..Patch::default()
            },
            Patch {
                bandwidth: Some(Bandwidth::PerClient {
                    download: 0,
                    upload: 10,
                }),
                ..Patch::default()
            },
            Patch {
                allowed_destinations: Some(vec!["192.0.2.0/24".into()]),
                ..Patch::default()
            },
            Patch {
                allowed_destinations: Some(vec!["0.1.2.3".into()]),
                ..Patch::default()
            },
            Patch {
                allowed_destinations: Some(vec!["host.local".into()]),
                ..Patch::default()
            },
        ] {
            assert!(patch.validate().is_err());
        }
    }

    #[test]
    fn schedule_patch_preserves_unknown_nested_fields_and_week_schedule() {
        let mut body = json!({"schedule":{"opaque":true,"activeTimeRange":{"opaqueRange":17,"startTime":"01:00","endTime":"02:00"}},"weekSchedule":{"opaqueWeek":true}});
        Patch {
            schedule: Some(Schedule::Always),
            ..Patch::default()
        }
        .apply(&mut body)
        .unwrap();
        assert_eq!(body["schedule"]["opaque"], true);
        assert_eq!(body["schedule"]["activeTimeRange"]["opaqueRange"], 17);
        assert_eq!(body["schedule"]["activeDays"].as_array().unwrap().len(), 7);
        assert_eq!(body["schedule"]["activeTimeRange"]["enabled"], false);
        assert!(
            body["schedule"]["activeTimeRange"]
                .get("startTime")
                .is_none()
        );
        assert_eq!(body["weekSchedule"]["opaqueWeek"], true);
    }

    #[test]
    fn creation_requires_a_supported_authentication_security_pair() {
        for (authentication, security, valid) in [
            ("none", "open", true),
            ("none", "owe", true),
            ("psk", "wpa2", true),
            ("psk", "wpa3", true),
            ("802.1x", "wpa2", true),
            ("802.1x", "wpa3", true),
            ("none", "wpa2", false),
            ("psk", "open", false),
            ("802.1x", "owe", false),
            ("future-auth", "future-security", false),
        ] {
            let body = json!({"authentication":authentication,"security":security,"preSharedKey":"password!",
                "type":"employee","ipAddressingMode":"network","isAvailableOn24GHzRadioBand":true});
            assert_eq!(
                validate_create(&body).is_ok(),
                valid,
                "{authentication}/{security}"
            );
        }
    }

    #[test]
    fn create_serializer_omits_inapplicable_template_fields() {
        let mut body = json!({"authentication":"psk","type":"employee","ipAddressingMode":"network",
            "dhcpScope":{"network":"172.16.0.0"}, "wiredNetworkId":"keep-wired-id",
            "radiusServerPrimary":{"sharedSecret":"radius-secret"}, "isGuestPortalEnabled":true,
            "isBandwidthLimitEnabled":false,"bandwidthLimitMode":"perClient","perClientBandwidthLimitInMbps":10,
            "perNetworkDownstreamBandwidthLimitInMbps":20,
            "schedule":{"activeDays":["monday"],"activeTimeRange":{"enabled":false},"state":{"active":true}}});
        prepare_create(&mut body).unwrap();
        for field in [
            "dhcpScope",
            "radiusServerPrimary",
            "isGuestPortalEnabled",
            "bandwidthLimitMode",
            "perClientBandwidthLimitInMbps",
            "perNetworkDownstreamBandwidthLimitInMbps",
        ] {
            assert!(body.get(field).is_none(), "{field}");
        }
        assert_eq!(body["wiredNetworkId"], "keep-wired-id");
        assert_eq!(body["isCaptivePortalEnabled"], false);
        assert!(body["schedule"].get("state").is_none());
    }

    #[test]
    fn create_allow_list_follows_employee_personal_security_gate_and_wire_projection() {
        for (kind, authentication, state, included) in [
            ("employee", "psk", "allowed", true),
            ("guest", "psk", "allowed", false),
            ("employee", "none", "allowed", false),
            ("employee", "psk", "forbidden", false),
            ("employee", "psk", "future", false),
        ] {
            let mut body = json!({"type":kind,"authentication":authentication,"allowList":{
                "id":"","isAllowListEnabled":true,"allowListState":state,"clients":["read-only"]}});
            prepare_create(&mut body).unwrap();
            assert_eq!(body.get("allowList").is_some(), included);
            if included {
                assert_eq!(
                    body["allowList"],
                    json!({"id":"","isAllowListEnabled":true})
                );
            }
        }
    }
}
