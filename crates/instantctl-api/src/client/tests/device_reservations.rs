use super::clock::{apply_readback_once, keep_clock_paused};
use super::*;
use crate::{
    ErrorKind,
    device::reservations::{Reservation, plan},
    mutation::{Outcome, apply_once},
};
use serde_json::{Value, json};
use std::{net::Ipv4Addr, time::Duration};

const SITE: &str = "123e4567-e89b-12d3-a456-426614174000";
const DEVICE_ID: &str = "aa:bb:cc:dd:ee:ff";
const OTHER_MAC: &str = "bb:cc:dd:ee:ff:00";
const NETWORK_ID: &str = "wired-opaque-17";

fn device(reserved_ip: Option<&str>) -> Value {
    json!({
        "kind":"inventory",
        "id":DEVICE_ID,
        "macAddress":DEVICE_ID,
        "name":"Hallway AP",
        "canReserveIpAddress":true,
        "reservedIpAddress":reserved_ip
    })
}

fn reservation(mac: &str, ip: &str) -> Value {
    json!({"macAddress":mac,"ipAddress":ip})
}

fn network_scope(rows: Vec<Value>) -> Value {
    json!({
        "networkId":NETWORK_ID,
        "isDhcpServer":true,
        "dhcpScope":{
            "network":"192.0.2.0",
            "ipAddress":"192.0.2.1",
            "netmask":"255.255.255.0",
            "ipAddressRange":{"start":"192.0.2.20","end":"192.0.2.200"},
            "ipReservations":rows
        },
        "ipReservationInfo":{"clients":[],"siteDevices":[]}
    })
}

fn inventory_reply(device: Value, scopes: Vec<Value>) -> Reply {
    let elements = vec![device];
    Reply::json(
        200,
        serde_json::to_vec(&json!({
            "kind":"resourceList",
            "totalCount":1,
            "matchingFilterCount":1,
            "pendingAvailability":null,
            "metaData":{"networkScopes":scopes},
            "elements":elements
        }))
        .expect("serialize complete inventory"),
    )
}

fn ack() -> Reply {
    Reply::json(
        200,
        br#"{"kind":"inventory","id":"aa:bb:cc:dd:ee:ff"}"#.to_vec(),
    )
}

fn assert_inventory_get(request: &Request) {
    assert_eq!(request.method, "GET");
    assert_eq!(request.target, format!("/api/sites/{SITE}/inventory"));
    assert!(request.body.is_empty());
}

fn request_json(request: &Request) -> Value {
    serde_json::from_slice(&request.body).expect("write body should be JSON")
}

#[tokio::test]
async fn reservation_add_and_remove_send_one_action_and_verify_authoritative_scopes() {
    let unrelated = reservation(OTHER_MAC, "192.0.2.40");
    let desired = Reservation {
        network_id: NETWORK_ID.to_owned(),
        ip_address: Ipv4Addr::new(192, 0, 2, 45),
    };
    let server = MockServer::start(vec![
        inventory_reply(device(None), vec![network_scope(vec![unrelated.clone()])]),
        ack(),
        inventory_reply(
            device(Some("192.0.2.45")),
            vec![network_scope(vec![
                unrelated.clone(),
                reservation(DEVICE_ID, "192.0.2.45"),
            ])],
        ),
    ]);
    let client = make_client(&server, Duration::from_secs(2));
    let prepared = plan(&client, SITE, DEVICE_ID, Some(desired.clone()))
        .await
        .expect("valid reservation should prepare");
    assert_eq!(prepared.plan.current, None);
    assert_eq!(prepared.plan.desired, Some(desired));
    let report = apply_once(&prepared.backend, &prepared.plan, Duration::from_secs(2))
        .await
        .expect("reservation mutation should report");
    assert_eq!(report.outcome, Outcome::Verified);
    assert_eq!(
        report.observed,
        Some(Some(Reservation {
            network_id: NETWORK_ID.to_owned(),
            ip_address: Ipv4Addr::new(192, 0, 2, 45),
        }))
    );

    let requests = server.finish();
    assert_eq!(requests.len(), 3);
    assert_inventory_get(&requests[0]);
    assert_eq!(requests[1].method, "POST");
    assert_eq!(
        requests[1].target,
        format!("/api/sites/{SITE}/inventory/{DEVICE_ID}?action=reserveIp")
    );
    assert_eq!(
        request_json(&requests[1]),
        json!({"ipReservations":[{"networkId":NETWORK_ID,"ipAddress":"192.0.2.45"}]})
    );
    assert_inventory_get(&requests[2]);

    let server = MockServer::start(vec![
        inventory_reply(
            device(Some("192.0.2.45")),
            vec![network_scope(vec![
                unrelated.clone(),
                reservation(DEVICE_ID, "192.0.2.45"),
            ])],
        ),
        ack(),
        inventory_reply(device(None), vec![network_scope(vec![unrelated.clone()])]),
    ]);
    let client = make_client(&server, Duration::from_secs(2));
    let prepared = plan(&client, SITE, DEVICE_ID, None)
        .await
        .expect("existing reservation should be removable");
    assert_eq!(
        prepared.plan.current,
        Some(Reservation {
            network_id: NETWORK_ID.to_owned(),
            ip_address: Ipv4Addr::new(192, 0, 2, 45),
        })
    );
    assert_eq!(prepared.plan.desired, None);
    let report = apply_once(&prepared.backend, &prepared.plan, Duration::from_secs(2))
        .await
        .expect("reservation removal should report");
    assert_eq!(report.outcome, Outcome::Verified);
    assert_eq!(report.observed, Some(None));
    let requests = server.finish();
    assert_eq!(requests.len(), 3);
    assert_inventory_get(&requests[0]);
    assert_eq!(requests[1].method, "POST");
    assert_eq!(
        requests[1].target,
        format!("/api/sites/{SITE}/inventory/{DEVICE_ID}?action=reserveIp")
    );
    assert_eq!(request_json(&requests[1]), json!({"ipReservations":[]}));
    assert_inventory_get(&requests[2]);
}

#[tokio::test]
async fn reservation_plan_refuses_unknown_unsupported_and_conflicting_state_before_write() {
    let mut unsupported = device(None);
    unsupported["canReserveIpAddress"] = json!(false);
    let mut unknown_capability = device(None);
    unknown_capability
        .as_object_mut()
        .unwrap()
        .remove("canReserveIpAddress");
    let duplicate_ip = network_scope(vec![
        reservation(OTHER_MAC, "192.0.2.45"),
        reservation("cc:dd:ee:ff:00:11", "192.0.2.45"),
    ]);
    let contradictory_flat = device(Some("192.0.2.41"));
    let mut non_dhcp = network_scope(vec![]);
    non_dhcp["isDhcpServer"] = json!(false);
    let mut malformed_network = network_scope(vec![]);
    malformed_network["dhcpScope"]["network"] = json!("192.0.2.1");
    let mut malformed_gateway = network_scope(vec![]);
    malformed_gateway["dhcpScope"]["ipAddress"] = json!("192.0.2.0");
    let mut malformed_range = network_scope(vec![]);
    malformed_range["dhcpScope"]["ipAddressRange"] = json!({
        "start":"192.0.2.200","end":"192.0.2.20"
    });
    let mut client_conflict = network_scope(vec![]);
    client_conflict["ipReservationInfo"]["clients"] = json!([{
        "macAddress":OTHER_MAC,
        "ipAddress":"192.0.2.45",
        "isOnline":true,
        "hasActiveLease":false
    }]);
    let mut lease_conflict = client_conflict.clone();
    lease_conflict["ipReservationInfo"]["clients"][0]["isOnline"] = json!(false);
    lease_conflict["ipReservationInfo"]["clients"][0]["hasActiveLease"] = json!(true);
    let mut unknown_dhcp = network_scope(vec![]);
    unknown_dhcp["isDhcpServer"] = Value::Null;
    let mut static_device_conflict = network_scope(vec![]);
    static_device_conflict["ipReservationInfo"]["siteDevices"] = json!([{
        "macAddress":OTHER_MAC,
        "ipAddress":"192.0.2.45",
        "isOnline":false,
        "ipAssignmentScheme":"static"
    }]);
    let already_reserved_device = device(Some("192.0.2.42"));
    let already_reserved_scope = network_scope(vec![reservation(DEVICE_ID, "192.0.2.42")]);
    let cases = [
        (
            "unsupported capability",
            unsupported,
            vec![network_scope(vec![])],
            ErrorKind::Unsupported,
            "cannot reserve",
        ),
        (
            "unknown capability",
            unknown_capability,
            vec![network_scope(vec![])],
            ErrorKind::Unverified,
            "capability is unknown",
        ),
        (
            "non-DHCP network",
            device(None),
            vec![non_dhcp],
            ErrorKind::Unsupported,
            "not a DHCP server",
        ),
        (
            "missing network",
            device(None),
            vec![],
            ErrorKind::NotFound,
            "not found",
        ),
        (
            "unknown DHCP server",
            device(None),
            vec![unknown_dhcp],
            ErrorKind::Unverified,
            "DHCP server state is unknown",
        ),
        (
            "duplicate reservation IP",
            device(None),
            vec![duplicate_ip.clone()],
            ErrorKind::Unverified,
            "duplicated",
        ),
        (
            "flat and scope disagree",
            contradictory_flat,
            vec![network_scope(vec![])],
            ErrorKind::Unverified,
            "disagree",
        ),
        (
            "non-base subnet",
            device(None),
            vec![malformed_network],
            ErrorKind::Usage,
            "subnet base",
        ),
        (
            "invalid gateway",
            device(None),
            vec![malformed_gateway],
            ErrorKind::Usage,
            "gateway",
        ),
        (
            "invalid range",
            device(None),
            vec![malformed_range],
            ErrorKind::Usage,
            "range",
        ),
        (
            "network address",
            device(None),
            vec![network_scope(vec![])],
            ErrorKind::Usage,
            "reservation",
        ),
        (
            "gateway address",
            device(None),
            vec![network_scope(vec![])],
            ErrorKind::Usage,
            "reservation",
        ),
        (
            "broadcast address",
            device(None),
            vec![network_scope(vec![])],
            ErrorKind::Usage,
            "reservation",
        ),
        (
            "outside DHCP range",
            device(None),
            vec![network_scope(vec![])],
            ErrorKind::Usage,
            "range",
        ),
        (
            "active client conflict",
            device(None),
            vec![client_conflict],
            ErrorKind::Usage,
            "conflicts",
        ),
        (
            "offline active lease conflict",
            device(None),
            vec![lease_conflict],
            ErrorKind::Usage,
            "conflicts",
        ),
        (
            "static device conflict",
            device(None),
            vec![static_device_conflict],
            ErrorKind::Usage,
            "conflicts",
        ),
        (
            "remove before changing address",
            already_reserved_device,
            vec![already_reserved_scope],
            ErrorKind::Usage,
            "remove the device's existing",
        ),
    ];

    for (label, target_device, scopes, kind, message) in cases {
        let ip = match label {
            "network address" => Ipv4Addr::new(192, 0, 2, 0),
            "gateway address" => Ipv4Addr::new(192, 0, 2, 1),
            "broadcast address" => Ipv4Addr::new(192, 0, 2, 255),
            "outside DHCP range" => Ipv4Addr::new(192, 0, 2, 10),
            _ => Ipv4Addr::new(192, 0, 2, 45),
        };
        let server = MockServer::start(vec![inventory_reply(target_device, scopes)]);
        let client = make_client(&server, Duration::from_secs(1));
        let result = plan(
            &client,
            SITE,
            DEVICE_ID,
            Some(Reservation {
                network_id: NETWORK_ID.into(),
                ip_address: ip,
            }),
        )
        .await;
        let error = match result {
            Err(error) => error,
            Ok(_) => panic!("{label} should be refused"),
        };
        assert_eq!(error.kind, kind, "wrong refusal for {label}: {error}");
        assert!(
            error.to_string().contains(message),
            "wrong error for {label}: {error}"
        );
        let requests = server.finish();
        assert_eq!(requests.len(), 1, "{label} must fail before any write");
        assert_inventory_get(&requests[0]);
    }
}

#[tokio::test(start_paused = true)]
async fn reservation_readback_rejects_changed_device_identity() {
    let _clock = keep_clock_paused().await;
    let mut changed_identity = device(None);
    changed_identity["macAddress"] = json!(OTHER_MAC);
    let server = MockServer::start(vec![
        inventory_reply(device(None), vec![network_scope(vec![])]),
        ack(),
        inventory_reply(changed_identity, vec![network_scope(vec![])]),
    ]);
    let client = make_client(&server, Duration::from_secs(1));
    let prepared = plan(
        &client,
        SITE,
        DEVICE_ID,
        Some(Reservation {
            network_id: NETWORK_ID.into(),
            ip_address: Ipv4Addr::new(192, 0, 2, 45),
        }),
    )
    .await
    .expect("reservation should prepare from initial identity");
    let report = apply_readback_once(
        &prepared.backend,
        &prepared.plan,
        Duration::from_millis(200),
    )
    .await
    .expect("identity mismatch should produce an unverified report");
    assert_eq!(report.outcome, Outcome::Unverified);
    assert_eq!(report.error_kind(), Some(ErrorKind::Unverified));
    assert!(
        report
            .readback_error
            .expect("identity error should be retained")
            .to_string()
            .contains("identity changed")
    );
    let requests = server.finish();
    assert_eq!(requests.len(), 3);
    assert_inventory_get(&requests[0]);
    assert_eq!(requests[1].method, "POST");
    assert_eq!(
        requests[1].target,
        format!("/api/sites/{SITE}/inventory/{DEVICE_ID}?action=reserveIp")
    );
    assert_inventory_get(&requests[2]);
}
