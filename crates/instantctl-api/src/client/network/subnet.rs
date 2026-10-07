//! Wired subnet allocation sequence from portal bundle @4762000.
use std::net::Ipv4Addr;

use serde_json::{Value, json};

use super::incomplete;
use crate::Error;

const RESERVATIONS: &[&str] = &[
    "managementIpSubnets",
    "wanIpSubnets",
    "vpnIpSubnets",
    "defaultLocalDhcpSubnets",
    "configuredLocalDhcpSubnets",
    "domainIpSubnets",
];

pub(super) fn allocate(scope: &mut Value, reservations: &Value) -> Result<(), Error> {
    if !reservations.is_object() {
        return Err(incomplete("reserved subnet response is not an object"));
    }
    let mut reserved = Vec::new();
    for field in RESERVATIONS {
        if let Some(entries) = reservations.get(*field) {
            let entries = entries
                .as_array()
                .ok_or_else(|| incomplete("reserved subnet list is malformed"))?;
            for entry in entries {
                reserved.push(interval(entry)?);
            }
        }
    }
    let (network, start, end) = (1..=254)
        .find_map(|octet| {
            let network = Ipv4Addr::new(172, 30, octet, 0);
            let start = u32::from(network);
            let end = start + 255;
            (!reserved
                .iter()
                .any(|(low, high)| start <= *high && *low <= end))
            .then_some((network, start, end))
        })
        .ok_or_else(|| {
            incomplete("no available wired DHCP subnet in the portal allocation range")
        })?;

    let object = scope
        .as_object_mut()
        .ok_or_else(|| incomplete("wired DHCP scope is malformed"))?;
    let mut kept = Vec::new();
    if let Some(entries) = object.get("ipReservations") {
        let entries = entries
            .as_array()
            .ok_or_else(|| incomplete("wired DHCP reservations are malformed"))?;
        for entry in entries {
            let ip = entry
                .get("ipAddress")
                .and_then(Value::as_str)
                .and_then(|ip| ip.parse::<Ipv4Addr>().ok())
                .ok_or_else(|| incomplete("wired DHCP reservation address is malformed"))?;
            if (start..=end).contains(&u32::from(ip)) {
                kept.push(entry.clone());
            }
        }
    }
    object.insert("network".into(), json!(network.to_string()));
    object.insert("netmask".into(), json!("255.255.255.0"));
    object.insert("ipReservations".into(), json!(kept));
    // Us serializes a network base without a host address or DHCP pool range.
    object.remove("ipAddress");
    object.remove("ipAddressRange");
    Ok(())
}

fn interval(entry: &Value) -> Result<(u32, u32), Error> {
    let network = entry
        .get("network")
        .and_then(Value::as_str)
        .and_then(|value| value.parse::<Ipv4Addr>().ok());
    let mask = entry
        .get("netmask")
        .and_then(Value::as_str)
        .and_then(|value| value.parse::<Ipv4Addr>().ok());
    let (Some(network), Some(mask)) = (network, mask) else {
        return Err(incomplete(
            "reserved subnet address or netmask is malformed",
        ));
    };
    let mask = u32::from(mask);
    let inverted = !mask;
    if inverted & inverted.wrapping_add(1) != 0 {
        return Err(incomplete("reserved subnet netmask is not contiguous"));
    }
    let start = u32::from(network) & mask;
    Ok((start, start | inverted))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocation_uses_first_free_wired_subnet_and_serializes_scope() {
        let mut scope = json!({
            "network":"192.0.2.0",
            "netmask":"255.255.0.0",
            "ipAddress":"172.30.2.1",
            "ipAddressRange":{"start":"172.30.2.10","end":"172.30.2.20"},
            "domainName":"lab.example",
            "dnsServers":["192.0.2.53"],
            "ipReservations":[
                {"ipAddress":"172.30.1.20","name":"old-candidate"},
                {"ipAddress":"172.30.2.20","name":"allocated-candidate"},
                {"ipAddress":"172.31.2.20","name":"outside-range"}
            ]
        });
        allocate(
            &mut scope,
            &json!({"managementIpSubnets":[{"network":"172.30.1.0","netmask":"255.255.255.0"}]}),
        )
        .unwrap();

        assert_eq!(scope["network"], "172.30.2.0");
        assert_eq!(scope["netmask"], "255.255.255.0");
        assert_eq!(scope["domainName"], "lab.example");
        assert_eq!(scope["dnsServers"], json!(["192.0.2.53"]));
        assert!(scope.get("ipAddress").is_none());
        assert!(scope.get("ipAddressRange").is_none());
        assert_eq!(
            scope["ipReservations"],
            json!([{"ipAddress":"172.30.2.20","name":"allocated-candidate"}])
        );

        let mut empty = json!({});
        allocate(&mut empty, &json!({})).unwrap();
        assert_eq!(empty["network"], "172.30.1.0");
    }

    #[test]
    fn every_reservation_category_blocks_overlapping_wired_subnets() {
        for field in RESERVATIONS {
            let mut scope = json!({});
            let mut reserved = json!({});
            reserved[field] = json!([{"network":"172.30.1.7","netmask":"255.255.255.0"}]);
            allocate(&mut scope, &reserved).unwrap();
            assert_eq!(scope["network"], "172.30.2.0", "{field}");
        }
    }

    #[test]
    fn malformed_and_exhausted_sources_fail_without_changing_scope() {
        let mut full_range = json!({});
        full_range["vpnIpSubnets"] = json!([{
            "network":"172.30.0.0",
            "netmask":"255.255.0.0"
        }]);
        for reserved in [
            full_range,
            json!({"vpnIpSubnets":[{"network":"bad","netmask":"255.255.255.0"}]}),
            json!({"vpnIpSubnets":[{"network":"172.30.1.0","netmask":"255.0.255.0"}]}),
            json!({"vpnIpSubnets":null}),
        ] {
            let original = json!({"network":"retain","metadata":{"keep":true}});
            let mut scope = original.clone();
            assert!(allocate(&mut scope, &reserved).is_err());
            assert_eq!(scope, original);
        }
    }

    #[test]
    fn malformed_scope_reservations_fail_without_partial_mutation() {
        let mut scope = json!({
            "network":"retain",
            "ipReservations":[{"ipAddress":"not-an-ip"}]
        });
        let original = scope.clone();
        assert!(allocate(&mut scope, &json!({})).is_err());
        assert_eq!(scope, original);
    }
}
