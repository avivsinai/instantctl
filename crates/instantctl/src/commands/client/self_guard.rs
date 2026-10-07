use std::net::IpAddr;

use instantctl_api::{Error, ErrorKind};
use network_interface::{Addr, NetworkInterface, NetworkInterfaceConfig};

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct InterfaceIdentity {
    pub(super) mac: Option<String>,
    pub(super) ips: Vec<IpAddr>,
}

pub(super) fn ensure_not_local(
    client_mac: Option<&str>,
    client_ip: Option<&str>,
    interfaces: Option<&[InterfaceIdentity]>,
    force: bool,
) -> Result<(), Error> {
    if force {
        return Ok(());
    }
    let Some(interfaces) = interfaces else {
        return Err(interfaces_unknown());
    };

    let local_mac = normalized_mac(client_mac.unwrap_or_default());
    let local_ip = client_ip.and_then(|ip| ip.parse::<IpAddr>().ok());
    let mut mac_matches = false;
    let mut ip_matches = false;
    for interface in interfaces {
        if let Some(mac) = interface.mac.as_deref().and_then(normalized_mac)
            && local_mac.as_deref() == Some(mac.as_str())
        {
            mac_matches = true;
        }
        for ip in &interface.ips {
            if local_ip == Some(*ip) {
                ip_matches = true;
            }
        }
    }
    if mac_matches || ip_matches {
        return Err(Error::new(
            ErrorKind::ConfirmationRequired,
            "client matches a local interface; use --force to block it",
        ));
    }
    let mut compared = false;
    for interface in interfaces {
        let mac = interface.mac.as_deref().and_then(normalized_mac);
        // macOS exposes addressless gif/stf interfaces. They cannot be the
        // portal client and must not force every block to use --force.
        if mac.is_none() && interface.ips.is_empty() {
            continue;
        }
        // Loopback cannot be the portal client's physical interface. Still
        // compare its addresses above so a reported local IP is refused.
        if !interface.ips.is_empty() && interface.ips.iter().all(IpAddr::is_loopback) {
            continue;
        }
        let can_compare_mac = local_mac.is_some() && mac.is_some();
        let can_compare_ip = local_ip.is_some() && !interface.ips.is_empty();
        compared |= can_compare_mac || can_compare_ip;
    }
    if !compared {
        return Err(interfaces_unknown());
    }
    Ok(())
}

pub(super) fn local_interfaces() -> Result<Vec<InterfaceIdentity>, Error> {
    let interfaces = NetworkInterface::show().map_err(|_| interfaces_unknown())?;
    Ok(interfaces
        .into_iter()
        .map(|interface| {
            let ips = interface
                .addr
                .into_iter()
                .map(|addr| match addr {
                    Addr::V4(value) => IpAddr::V4(value.ip),
                    Addr::V6(value) => IpAddr::V6(value.ip),
                })
                .collect();
            InterfaceIdentity {
                mac: interface.mac_addr,
                ips,
            }
        })
        .collect())
}

fn normalized_mac(value: &str) -> Option<String> {
    let compact: String = value
        .bytes()
        .filter(|byte| *byte != b':' && *byte != b'-')
        .map(|byte| byte.to_ascii_lowercase() as char)
        .collect();
    (compact.len() == 12 && compact.bytes().all(|byte| byte.is_ascii_hexdigit())).then_some(compact)
}

fn interfaces_unknown() -> Error {
    Error::new(
        ErrorKind::Unverified,
        "local interfaces could not be verified; refusing to block without --force",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(mac: Option<&str>, ips: &[&str]) -> InterfaceIdentity {
        InterfaceIdentity {
            mac: mac.map(str::to_owned),
            ips: ips.iter().map(|ip| ip.parse().unwrap()).collect(),
        }
    }

    #[test]
    fn blocks_a_client_with_a_local_mac() {
        let interfaces = [identity(
            Some("aa:bb:cc:dd:ee:ff"),
            &["192.0.2.4", "2001:db8::4"],
        )];
        let error = ensure_not_local(Some("AA-BB-CC-DD-EE-FF"), None, Some(&interfaces), false)
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::ConfirmationRequired);
    }

    #[test]
    fn blocks_a_client_with_any_local_ip_address() {
        let interfaces = [identity(
            Some("00:11:22:33:44:55"),
            &["192.0.2.4", "2001:db8::4"],
        )];
        for client_ip in ["192.0.2.4", "2001:db8::4"] {
            let error =
                ensure_not_local(None, Some(client_ip), Some(&interfaces), false).unwrap_err();
            assert_eq!(error.kind, ErrorKind::ConfirmationRequired);
        }
    }

    #[test]
    fn permits_nonlocal_identity_and_refuses_unknown_or_empty_enumeration() {
        let interfaces = [identity(Some("00:11:22:33:44:55"), &["192.0.2.4"])];
        assert!(
            ensure_not_local(
                Some("aa:bb:cc:dd:ee:ff"),
                Some("192.0.2.20"),
                Some(&interfaces),
                false
            )
            .is_ok()
        );
        assert_eq!(
            ensure_not_local(None, None, None, false).unwrap_err().kind,
            ErrorKind::Unverified
        );
        assert_eq!(
            ensure_not_local(None, None, Some(&[]), false)
                .unwrap_err()
                .kind,
            ErrorKind::Unverified
        );
        let ip_only = [identity(None, &["192.0.2.4"])];
        assert_eq!(
            ensure_not_local(Some("aa:bb:cc:dd:ee:ff"), None, Some(&ip_only), false)
                .unwrap_err()
                .kind,
            ErrorKind::Unverified
        );
        assert_eq!(
            ensure_not_local(None, None, Some(&interfaces), false)
                .unwrap_err()
                .kind,
            ErrorKind::Unverified
        );
    }

    #[test]
    fn checks_all_interfaces_for_matching_mac_or_ip() {
        let interfaces = [
            identity(Some("00:11:22:33:44:55"), &["192.0.2.1"]),
            identity(Some("AA:BB:CC:DD:EE:FF"), &["192.0.2.9"]),
        ];
        assert_eq!(
            ensure_not_local(Some("aa:bb:cc:dd:ee:ff"), None, Some(&interfaces), false)
                .unwrap_err()
                .kind,
            ErrorKind::ConfirmationRequired
        );
        assert_eq!(
            ensure_not_local(None, Some("192.0.2.9"), Some(&interfaces), false)
                .unwrap_err()
                .kind,
            ErrorKind::ConfirmationRequired
        );
    }

    #[test]
    fn macos_addressless_and_tunnel_interfaces_do_not_require_force() {
        let interfaces = [
            identity(None, &["127.0.0.1", "::1"]),
            identity(None, &[]),          // gif0
            identity(None, &[]),          // stf0
            identity(None, &["fe80::1"]), // utun
            identity(Some("00:11:22:33:44:55"), &["192.0.2.5"]),
        ];
        for ip in [None, Some("192.0.2.20")] {
            assert!(
                ensure_not_local(Some("aa:bb:cc:dd:ee:ff"), ip, Some(&interfaces), false).is_ok()
            );
        }
        for (mac, ip) in [(Some("00:11:22:33:44:55"), None), (None, Some("192.0.2.5"))] {
            assert_eq!(
                ensure_not_local(mac, ip, Some(&interfaces), false)
                    .unwrap_err()
                    .kind,
                ErrorKind::ConfirmationRequired
            );
        }
        assert_eq!(
            ensure_not_local(None, None, Some(&interfaces), false)
                .unwrap_err()
                .kind,
            ErrorKind::Unverified
        );
        assert_eq!(
            ensure_not_local(
                Some("aa:bb:cc:dd:ee:ff"),
                None,
                Some(&interfaces[..4]),
                false
            )
            .unwrap_err()
            .kind,
            ErrorKind::Unverified
        );
    }

    #[test]
    fn force_explicitly_allows_local_or_unknown_identity() {
        let interfaces = [identity(Some("aa:bb:cc:dd:ee:ff"), &["192.0.2.4"])];
        assert!(ensure_not_local(Some("AA:BB:CC:DD:EE:FF"), None, Some(&interfaces), true).is_ok());
        assert!(ensure_not_local(None, None, None, true).is_ok());
    }
}
