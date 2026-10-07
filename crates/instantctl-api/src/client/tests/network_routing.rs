use super::clock::{apply_readback_once, keep_clock_paused};
use super::*;
use crate::{
    Error, ErrorKind,
    client::network_routing,
    mutation::{Outcome, apply_once},
};
use serde_json::{Value, json};
use std::time::Duration;

const SITE: &str = "123e4567-e89b-12d3-a456-426614174000";
const NETWORK_ID: &str = "wired-opaque-17";
const NETWORK_NAME: &str = "Office LAN";

fn collection_reply(elements: Vec<Value>) -> Reply {
    let count = elements.len() as u64;
    Reply::json(
        200,
        serde_json::to_vec(&json!({
            "kind":"wiredNetworks",
            "totalCount":count,
            "matchingFilterCount":count,
            "elements":elements
        }))
        .unwrap(),
    )
}

fn reply_json(value: Value) -> Reply {
    Reply::json(200, serde_json::to_vec(&value).unwrap())
}

fn assert_request(request: &Request, method: &str, target: &str) {
    assert_eq!(request.method, method);
    assert_eq!(request.target, target);
}

fn request_json(request: &Request) -> Value {
    serde_json::from_slice(&request.body).expect("write body should be JSON")
}

fn wired(id: &str, name: &str, vlan: u16) -> Value {
    json!({
        "id":id,"isWireless":false,"wiredNetworkName":name,"isEnabled":true,
        "type":"employee","vlanId":vlan,"isManagement":false,"isDeletable":true,
        "vlanIdCanBeChanged":true,"canDisableDhcpScope":true,"useDhcpScope":false,
        "shouldApplyNetworkSecurityProtections":false,"isAccessRestricted":false,
        "isInternetAllowed":true,"isIntraSubnetTrafficAllowed":true,
        "isSpecificDestinationsAllowed":false,"allowedDestinations":[],
        "isIpRoutingEnabled":false,"ipRoutingConfig":{"isStatic":false},
        "devicePortMappings":[],"isGuestPortalEnabled":false,"isIgmpSnoopingEnabled":false,
        "qos":{"trafficPriority":"medium"},"vendorExtension":{"keep":[1,"opaque"]}
    })
}

fn expect_error<T>(result: Result<T, Error>) -> Error {
    match result {
        Err(error) => error,
        Ok(_) => panic!("operation should fail"),
    }
}

fn permission_reply(value: Value) -> Reply {
    reply_json(value)
}

fn granted_permissions() -> Value {
    json!({"permissions":[{"permission":"inventory_update_all"}]})
}

fn changed_network(enabled: bool) -> Value {
    let mut row = wired(NETWORK_ID, NETWORK_NAME, 20);
    row["isIpRoutingEnabled"] = json!(enabled);
    row
}

#[tokio::test]
async fn routing_update_preserves_the_full_network_and_verifies_one_put() {
    let mut fresh = changed_network(false);
    fresh["vendorExtension"] = json!({"keep":[2,"changed during confirmation"]});
    let server = MockServer::start(vec![
        collection_reply(vec![changed_network(false)]),
        permission_reply(granted_permissions()),
        collection_reply(vec![fresh.clone()]),
        permission_reply(granted_permissions()),
        reply_json(json!({"id":NETWORK_ID})),
        collection_reply(vec![changed_network(true)]),
    ]);
    let api = make_client(&server, Duration::from_secs(2));
    let prepared = network_routing::plan_update(&api, SITE, NETWORK_ID, true)
        .await
        .unwrap();
    assert!(!prepared.plan.current);
    assert!(prepared.plan.desired);

    let report = apply_once(&prepared.backend, &prepared.plan, Duration::from_secs(2))
        .await
        .unwrap();
    assert_eq!(report.outcome, Outcome::Verified);
    assert_eq!(report.observed, Some(true));

    let requests = server.finish();
    assert_eq!(requests.len(), 6);
    assert_request(
        &requests[0],
        "GET",
        &format!("/api/sites/{SITE}/wiredNetworks"),
    );
    assert_request(
        &requests[1],
        "GET",
        &format!("/api/sites/{SITE}/permissions"),
    );
    assert_request(
        &requests[2],
        "GET",
        &format!("/api/sites/{SITE}/wiredNetworks"),
    );
    assert_request(
        &requests[3],
        "GET",
        &format!("/api/sites/{SITE}/permissions"),
    );
    assert_request(
        &requests[4],
        "PUT",
        &format!("/api/sites/{SITE}/wiredNetworks/{NETWORK_ID}"),
    );
    let mut expected = fresh;
    expected["isIpRoutingEnabled"] = json!(true);
    assert_eq!(request_json(&requests[4]), expected);
    assert_request(
        &requests[5],
        "GET",
        &format!("/api/sites/{SITE}/wiredNetworks"),
    );
}

#[tokio::test]
async fn routing_update_refuses_unknown_or_management_networks_before_put() {
    for management_value in [Some(Value::Bool(true)), Some(Value::Null), None] {
        let mut row = changed_network(false);
        match management_value {
            Some(value) => row["isManagement"] = value,
            None => {
                row.as_object_mut().unwrap().remove("isManagement");
            }
        }
        let server = MockServer::start(vec![collection_reply(vec![row])]);
        let api = make_client(&server, Duration::from_secs(1));
        let error = expect_error(network_routing::plan_update(&api, SITE, NETWORK_ID, true).await);
        assert_eq!(error.kind, ErrorKind::Unsupported);
        let requests = server.finish();
        assert_eq!(requests.len(), 1);
        assert!(requests.iter().all(|request| request.method != "PUT"));
    }
}

#[tokio::test]
async fn routing_update_refuses_missing_malformed_or_insufficient_permissions() {
    for permission_response in [
        Reply::json(404, br#"{}"#.to_vec()),
        Reply::json(200, b"not-json".to_vec()),
        permission_reply(json!({})),
        permission_reply(json!({"permissions":{}})),
        permission_reply(json!({"permissions":[{"permission":17}]})),
        permission_reply(json!({"permissions":[{"permission":"inventory_read"}]})),
    ] {
        let server = MockServer::start(vec![
            collection_reply(vec![changed_network(false)]),
            permission_response,
        ]);
        let api = make_client(&server, Duration::from_secs(1));
        let error = expect_error(network_routing::plan_update(&api, SITE, NETWORK_ID, true).await);
        assert_eq!(error.kind, ErrorKind::Unsupported);
        let requests = server.finish();
        assert_eq!(requests.len(), 2);
        assert!(requests.iter().all(|request| request.method != "PUT"));
    }
}

#[tokio::test(start_paused = true)]
async fn routing_apply_rechecks_live_permission_before_put() {
    let _clock = keep_clock_paused().await;
    let server = MockServer::start(vec![
        collection_reply(vec![changed_network(false)]),
        permission_reply(granted_permissions()),
        collection_reply(vec![changed_network(false)]),
        permission_reply(json!({"permissions":[]})),
        collection_reply(vec![changed_network(false)]),
    ]);
    let api = make_client(&server, Duration::from_secs(1));
    let prepared = network_routing::plan_update(&api, SITE, NETWORK_ID, true)
        .await
        .unwrap();
    let report = apply_readback_once(
        &prepared.backend,
        &prepared.plan,
        Duration::from_millis(150),
    )
    .await
    .unwrap();
    assert_eq!(report.outcome, Outcome::Failed);
    assert_eq!(report.error_kind(), Some(ErrorKind::Unsupported));
    let requests = server.finish();
    assert_eq!(requests.len(), 5);
    assert!(requests.iter().all(|request| request.method != "PUT"));
}

#[tokio::test(start_paused = true)]
async fn routing_mismatch_after_put_is_unverified() {
    let _clock = keep_clock_paused().await;
    let server = MockServer::start(vec![
        collection_reply(vec![changed_network(false)]),
        permission_reply(granted_permissions()),
        collection_reply(vec![changed_network(false)]),
        permission_reply(granted_permissions()),
        reply_json(json!({"id":NETWORK_ID})),
        collection_reply(vec![changed_network(false)]),
    ]);
    let api = make_client(&server, Duration::from_secs(1));
    let prepared = network_routing::plan_update(&api, SITE, NETWORK_ID, true)
        .await
        .unwrap();
    let report = apply_readback_once(
        &prepared.backend,
        &prepared.plan,
        Duration::from_millis(150),
    )
    .await
    .unwrap();
    assert_eq!(report.outcome, Outcome::Unverified);
    assert_eq!(report.error_kind(), Some(ErrorKind::Unverified));
    let requests = server.finish();
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.method == "PUT")
            .count(),
        1
    );
}
