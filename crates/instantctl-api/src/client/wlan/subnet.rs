//! DS.generateAvailableWirelessIpSubnet + Us.prepareData from the portal.
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
    let (network, start, end) = (16..255)
        .find_map(|octet| {
            let network = Ipv4Addr::new(172, octet, 0, 0);
            let start = u32::from(network);
            let end = start + 255;
            (!reserved
                .iter()
                .any(|(low, high)| start <= *high && *low <= end))
            .then_some((network, start, end))
        })
        .ok_or_else(|| {
            incomplete("no available wireless DHCP subnet in the portal allocation range")
        })?;
    // The SPA stops at .254 even if it overlaps. Fail closed on exhaustion;
    // every successful candidate follows its same 172.16..254 /24 algorithm.
    let object = scope
        .as_object_mut()
        .ok_or_else(|| incomplete("wireless DHCP scope is malformed"))?;
    let mut kept = Vec::new();
    if let Some(entries) = object.get("ipReservations") {
        let entries = entries
            .as_array()
            .ok_or_else(|| incomplete("wireless DHCP reservations are malformed"))?;
        for entry in entries {
            let ip = entry
                .get("ipAddress")
                .and_then(Value::as_str)
                .and_then(|ip| ip.parse::<Ipv4Addr>().ok())
                .ok_or_else(|| incomplete("wireless DHCP reservation address is malformed"))?;
            if (start..=end).contains(&u32::from(ip)) {
                kept.push(entry.clone());
            }
        }
    }
    object.insert("network".into(), json!(network.to_string()));
    object.insert("netmask".into(), json!("255.255.255.0"));
    object.insert("ipReservations".into(), json!(kept));
    // The allocated address is a network base. Us.prepareData omits both
    // host address and pool range in this case.
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
    fn allocator_copies_portal_sequence_and_scope_serialization() {
        let mut scope = json!({"network":"172.30.0.0","netmask":"255.255.255.0","ipAddress":"172.30.0.1",
            "ipAddressRange":{"start":"172.30.0.10","end":"172.30.0.20"},"domainName":"lab",
            "ipReservations":[{"ipAddress":"172.17.0.20","macAddress":"aa:bb:cc:dd:ee:ff"},{"ipAddress":"172.30.0.20"}]});
        allocate(
            &mut scope,
            &json!({"managementIpSubnets":[{"network":"172.16.23.1","netmask":"255.255.0.0"}]}),
        )
        .unwrap();
        assert_eq!(scope["network"], "172.17.0.0");
        assert_eq!(scope["netmask"], "255.255.255.0");
        assert!(scope.get("ipAddress").is_none());
        assert!(scope.get("ipAddressRange").is_none());
        assert_eq!(scope["ipReservations"].as_array().unwrap().len(), 1);
        assert_eq!(scope["domainName"], "lab");
        let mut empty = json!({});
        allocate(&mut empty, &json!({})).unwrap();
        assert_eq!(empty["network"], "172.16.0.0");
    }

    #[test]
    fn every_reservation_class_blocks_overlapping_subnets() {
        for field in RESERVATIONS {
            let mut scope = json!({});
            let mut reserved = json!({});
            reserved[field] = json!([{"network":"172.16.0.0","netmask":"255.255.255.0"}]);
            allocate(&mut scope, &reserved).unwrap();
            assert_eq!(scope["network"], "172.17.0.0");
        }
    }

    #[test]
    fn exhaustion_and_malformed_reservations_refuse_allocation() {
        for reserved in [
            json!({"vpnIpSubnets":[{"network":"172.0.0.0","netmask":"255.0.0.0"}]}),
            json!({"vpnIpSubnets":[{"network":"bad","netmask":"255.255.255.0"}]}),
            json!({"vpnIpSubnets":[{"network":"172.16.0.0","netmask":"255.0.255.0"}]}),
            json!({"vpnIpSubnets":null}),
        ] {
            let original = json!({"network":"keep"});
            let mut scope = original.clone();
            assert!(allocate(&mut scope, &reserved).is_err());
            assert_eq!(scope, original);
        }
    }
}
