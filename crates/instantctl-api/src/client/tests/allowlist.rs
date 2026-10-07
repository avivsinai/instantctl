use super::clock::{apply_readback_once, keep_clock_paused};
use super::*;
use crate::{
    ErrorKind,
    client::allowlist::{PortScope, plan_wired, plan_wireless, validate_mac_addresses},
    mutation::{Outcome, apply_once},
};
use serde_json::{Value, json};
use std::time::Duration;

const SITE: &str = "123e4567-e89b-12d3-a456-426614174000";
const DEVICE: &str = "00:11:22:33:44:55";
const NETWORK: &str = "network-id";
const WIRELESS_LIST: &str = "wireless-list-id";
const WIRED_LIST: &str = "wired-list-id";
const MAC: &str = "AA:BB:CC:DD:EE:FF";
const EXISTING: &str = "11:22:33:44:55:66";

fn reply_json(value: Value) -> Reply {
    Reply::json(
        200,
        serde_json::to_vec(&value).expect("serialize allow-list fixture"),
    )
}

fn networks_summary() -> Reply {
    reply_json(json!({
        "kind":"networksSummary", "totalCount":1, "matchingFilterCount":1,
        "elements":[{
            "id":NETWORK,"isWireless":true,"networkName":"Guest",
            "allowList":{"id":WIRELESS_LIST,"isAllowListEnabled":true,"allowListState":"allowed"}
        }]
    }))
}

fn allow_list(id: &str, clients: &[&str], status: &str, maximum: Option<u64>) -> Value {
    let mut value = json!({
        "id":id,
        "allowListState":status,
        "allowedClients":clients.iter().map(|mac| json!({"macAddress":mac})).collect::<Vec<_>>(),
        "vendor":{"retain":[1,"opaque"]}
    });
    if let Some(maximum) = maximum {
        value["maxAllowedClients"] = json!(maximum);
    }
    value
}

fn inventory() -> Reply {
    reply_json(json!({
        "kind":"resourceList","totalCount":1,"matchingFilterCount":1,
        "pendingAvailability":null,
        "elements":[{"id":DEVICE,"macAddress":"00:11:22:33:44:55","name":"Switch","deviceType":"switch"}]
    }))
}

fn device_details(port: u64, trunk: u64, list: Value) -> Reply {
    reply_json(json!({
        "id":DEVICE,"kind":"deviceDetails",
        "allowListByPortNumber":{(port.to_string()):list.clone()},
        "allowListByTrunkNumber":{(trunk.to_string()):list},
        "vendor":{"retain":true}
    }))
}

fn assert_request(request: &Request, method: &str, target: &str) {
    assert_eq!(request.method, method);
    assert_eq!(request.target, target);
    assert_eq!(
        request.headers.get("authorization").map(String::as_str),
        Some("Bearer netcli-test-token-sentinel")
    );
}

#[tokio::test]
async fn wireless_add_and_remove_use_selected_allow_list_and_verify_only_requested_clients() {
    for (add, before, desired, status, maximum) in [
        (true, vec![EXISTING], vec![EXISTING, MAC], "allowed", 4),
        (false, vec![EXISTING, MAC], vec![EXISTING], "allowed", 4),
        (
            true,
            vec![EXISTING],
            vec![EXISTING, MAC],
            "maxEntityAllowedAllowListReached",
            2,
        ),
    ] {
        let replies = vec![
            networks_summary(),
            reply_json(allow_list(WIRELESS_LIST, &before, status, Some(maximum))),
            reply_json(json!({"allowList":{"id":WIRELESS_LIST}})),
            reply_json(allow_list(
                WIRELESS_LIST,
                &desired,
                if desired.len() == maximum as usize {
                    "maxAllowedClientsReached"
                } else {
                    "allowed"
                },
                Some(maximum),
            )),
        ];
        let server = MockServer::start(replies);
        let api = make_client(&server, Duration::from_secs(2));
        let prepared = plan_wireless(&api, SITE, "Guest", add, &[MAC.into()])
            .await
            .expect("prepare selected wireless allow-list change");
        let report = apply_once(&prepared.backend, &prepared.plan, Duration::from_secs(1))
            .await
            .expect("apply one wireless action and read it back");
        assert_eq!(report.outcome, Outcome::Verified);
        assert_eq!(
            serde_json::to_value(report.observed.unwrap()).unwrap()["allowed_clients"],
            json!(desired)
        );

        let requests = server.finish();
        assert_eq!(requests.len(), 4);
        assert_request(
            &requests[0],
            "GET",
            &format!("/api/sites/{SITE}/networksSummary"),
        );
        assert_request(
            &requests[1],
            "GET",
            &format!("/api/sites/{SITE}/extendWirelessNetworkAllowList/{WIRELESS_LIST}"),
        );
        assert_request(
            &requests[2],
            "POST",
            &format!(
                "/api/sites/{SITE}/extendWirelessNetworkAllowList/{WIRELESS_LIST}?action={}",
                if add {
                    "addToAllowList"
                } else {
                    "removeFromAllowList"
                }
            ),
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&requests[2].body).unwrap(),
            json!({"macAddresses":[MAC]})
        );
        assert_request(
            &requests[3],
            "GET",
            &format!("/api/sites/{SITE}/extendWirelessNetworkAllowList/{WIRELESS_LIST}"),
        );
    }
}

#[tokio::test]
async fn wired_port_and_trunk_add_remove_use_asymmetric_action_ids_and_scoped_queries() {
    for (scope, add, before, desired, route_id) in [
        (
            PortScope::Port(7),
            true,
            vec![EXISTING],
            vec![EXISTING, MAC],
            WIRED_LIST,
        ),
        (
            PortScope::Port(7),
            false,
            vec![EXISTING, MAC],
            vec![EXISTING],
            DEVICE,
        ),
        (
            PortScope::Trunk(12),
            true,
            vec![EXISTING],
            vec![EXISTING, MAC],
            WIRED_LIST,
        ),
        (
            PortScope::Trunk(12),
            false,
            vec![EXISTING, MAC],
            vec![EXISTING],
            DEVICE,
        ),
    ] {
        let list = allow_list(WIRED_LIST, &before, "allowed", Some(5));
        let observed = allow_list(WIRED_LIST, &desired, "allowed", Some(5));
        let server = MockServer::start(vec![
            inventory(),
            device_details(7, 12, list),
            reply_json(json!({"id":WIRED_LIST})),
            device_details(7, 12, observed),
        ]);
        let api = make_client(&server, Duration::from_secs(2));
        let prepared = plan_wired(&api, SITE, "Switch", scope, add, &[MAC.into()])
            .await
            .expect("prepare selected wired allow-list change");
        assert_eq!(prepared.target["allow_list_id"], WIRED_LIST);
        let report = apply_once(&prepared.backend, &prepared.plan, Duration::from_secs(1))
            .await
            .expect("apply one wired action and read it back");
        assert_eq!(report.outcome, Outcome::Verified);
        assert_eq!(
            serde_json::to_value(report.observed.unwrap()).unwrap()["allowed_clients"],
            json!(desired)
        );

        let requests = server.finish();
        assert_eq!(requests.len(), 4);
        assert_request(&requests[0], "GET", &format!("/api/sites/{SITE}/inventory"));
        assert_request(
            &requests[1],
            "GET",
            &format!("/api/sites/{SITE}/deviceDetails/{DEVICE}"),
        );
        let (query_name, query_number) = match scope {
            PortScope::Port(number) => ("portNumber", number),
            PortScope::Trunk(number) => ("trunkNumber", number),
        };
        assert_request(
            &requests[2],
            "POST",
            &format!(
                "/api/sites/{SITE}/extendWiredPortAllowList/{route_id}?action={}&{query_name}={query_number}",
                if add {
                    "addToAllowList"
                } else {
                    "removeFromAllowList"
                }
            ),
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&requests[2].body).unwrap(),
            json!({"macAddresses":[MAC]})
        );
        assert_request(
            &requests[3],
            "GET",
            &format!("/api/sites/{SITE}/deviceDetails/{DEVICE}"),
        );
    }
}

#[tokio::test]
async fn unsafe_allow_list_states_capacity_and_duplicate_macs_never_write() {
    let duplicate = vec!["aa:bb:cc:dd:ee:ff".into(), MAC.into()];
    assert_eq!(
        validate_mac_addresses(&duplicate).unwrap_err().kind,
        ErrorKind::Usage
    );

    let cases = [
        ("forbidden", Some(4), vec![EXISTING], ErrorKind::Unsupported),
        (
            "maxAllowedClientsReached",
            Some(4),
            vec![EXISTING],
            ErrorKind::Unsupported,
        ),
        (
            "unrecognized",
            Some(4),
            vec![EXISTING],
            ErrorKind::Unverified,
        ),
        ("allowed", None, vec![EXISTING], ErrorKind::Unverified),
        ("allowed", Some(0), vec![EXISTING], ErrorKind::Unverified),
        ("allowed", Some(1), vec![EXISTING], ErrorKind::Usage),
    ];
    for (status, maximum, clients, expected_kind) in cases {
        let server = MockServer::start(vec![
            networks_summary(),
            reply_json(allow_list(WIRELESS_LIST, &clients, status, maximum)),
        ]);
        let api = make_client(&server, Duration::from_secs(1));
        let error = match plan_wireless(&api, SITE, "Guest", true, &[MAC.into()]).await {
            Ok(_) => panic!("unsafe allow-list state was accepted: {status}"),
            Err(error) => error,
        };
        assert_eq!(error.kind, expected_kind);
        let requests = server.finish();
        assert_eq!(
            requests.len(),
            2,
            "planning failures must not send an action"
        );
        assert_eq!(requests[1].method, "GET");
    }

    let server = MockServer::start(vec![
        networks_summary(),
        reply_json(allow_list(WIRELESS_LIST, &[EXISTING], "allowed", Some(4))),
    ]);
    let api = make_client(&server, Duration::from_secs(1));
    let error = match plan_wireless(&api, SITE, "Guest", true, &duplicate).await {
        Ok(_) => panic!("duplicate normalized input was accepted"),
        Err(error) => error,
    };
    assert_eq!(error.kind, ErrorKind::Usage);
    assert_eq!(
        server.finish().len(),
        0,
        "duplicate normalized input fails before HTTP"
    );
}

#[tokio::test(start_paused = true)]
async fn readback_and_action_ack_identity_mismatches_cannot_report_verified() {
    let _clock = keep_clock_paused().await;
    let server = MockServer::start(vec![
        networks_summary(),
        reply_json(allow_list(WIRELESS_LIST, &[EXISTING], "allowed", Some(4))),
        reply_json(json!({"allowList":{"id":"other-list"}})),
        reply_json(allow_list(
            WIRELESS_LIST,
            &[EXISTING, MAC],
            "allowed",
            Some(4),
        )),
    ]);
    let api = make_client(&server, Duration::from_secs(2));
    let prepared = plan_wireless(&api, SITE, "Guest", true, &[MAC.into()])
        .await
        .unwrap();
    let report = apply_once(&prepared.backend, &prepared.plan, Duration::from_secs(1))
        .await
        .unwrap();
    assert_eq!(report.outcome, Outcome::RequestFailedStateMatches);
    assert_eq!(report.request_error.unwrap().kind, ErrorKind::Unverified);
    assert_eq!(server.finish().len(), 4);

    let server = MockServer::start(vec![
        networks_summary(),
        reply_json(allow_list(WIRELESS_LIST, &[EXISTING], "allowed", Some(4))),
        reply_json(json!({"allowList":{"id":WIRELESS_LIST}})),
        reply_json(allow_list(WIRELESS_LIST, &[EXISTING], "allowed", Some(4))),
    ]);
    let api = make_client(&server, Duration::from_secs(1));
    let prepared = plan_wireless(&api, SITE, "Guest", true, &[MAC.into()])
        .await
        .unwrap();
    let report = apply_readback_once(&prepared.backend, &prepared.plan, Duration::from_millis(80))
        .await
        .unwrap();
    assert_eq!(report.outcome, Outcome::Unverified);
    assert_eq!(server.finish().len(), 4);

    let server = MockServer::start(vec![
        inventory(),
        reply_json(
            json!({"id":"changed-device","allowListByPortNumber":{"7":allow_list(WIRED_LIST, &[EXISTING], "allowed", Some(4))}}),
        ),
    ]);
    let api = make_client(&server, Duration::from_secs(1));
    let error = match plan_wired(
        &api,
        SITE,
        "Switch",
        PortScope::Port(7),
        true,
        &[MAC.into()],
    )
    .await
    {
        Ok(_) => panic!("device identity mismatch was accepted"),
        Err(error) => error,
    };
    assert_eq!(error.kind, ErrorKind::Unverified);
    assert_eq!(server.finish().len(), 2);
}
