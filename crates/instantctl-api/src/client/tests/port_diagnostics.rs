use super::*;
use crate::{
    ErrorKind,
    client::port_diagnostics::{DiagnosticState, plan_cable_test, plan_connectivity_test},
    mutation::{Mutation, Outcome, apply_once},
};
use serde_json::{Value, json};
use std::time::Duration;

const SITE: &str = "123e4567-e89b-12d3-a456-426614174000";
const DEVICE_ID: &str = "aa:bb:cc:dd:ee:ff";
const FACEPLATE: u64 = 2;
const API_PORT: u64 = 8;

fn switch() -> Value {
    json!({
        "kind":"inventory", "id":DEVICE_ID, "macAddress":DEVICE_ID,
        "name":"Closet switch", "deviceType":"switch", "deviceRole":"switch",
        "status":"up", "operationalState":"active",
        "ethernetPorts":[{
            "portNumber":API_PORT, "faceplatePortNumber":FACEPLATE,
            "trunkNumber":null, "isUplink":false, "isDedicatedUplink":false,
            "capabilities":{"cableTest":true}
        }],
        "trunkPorts":[], "capabilities":{}
    })
}

fn inventory() -> Value {
    json!({
        "kind":"resourceList", "totalCount":1, "matchingFilterCount":1,
        "pendingAvailability":null, "elements":[switch()]
    })
}

fn rows(elements: Vec<Value>) -> Value {
    json!({"elements":elements})
}

fn reply(value: Value) -> Reply {
    Reply::json(200, serde_json::to_vec(&value).expect("reply JSON"))
}

fn body(request: &Request) -> Value {
    serde_json::from_slice(&request.body).expect("request body JSON")
}

#[tokio::test]
async fn configured_cable_port_refuses_before_start_without_force() {
    let server = MockServer::start(vec![reply(inventory())]);
    let client = make_client(&server, Duration::from_secs(1))
        .with_protected_ports(&["Closet switch:2".into()])
        .unwrap();
    let failure = match plan_cable_test(&client, SITE, DEVICE_ID, FACEPLATE, false).await {
        Ok(_) => panic!("configured protected cable port accepted without force"),
        Err(error) => error,
    };
    assert_eq!(failure.kind, ErrorKind::Usage);
    let requests = server.finish();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "GET");
}

#[tokio::test]
async fn cable_test_posts_api_port_and_verifies_only_the_acknowledged_new_result() {
    let result = json!({
        "id":"cable-new", "deviceId":DEVICE_ID, "portNumber":API_PORT,
        "state":"complete", "result":"openPair", "distanceToFault":12,
        "vendorResult":{"retained":true}
    });
    let server = MockServer::start(vec![
        reply(inventory()),
        reply(rows(vec![])),
        reply(result.clone()),
        reply(rows(vec![result.clone()])),
    ]);
    let client = make_client(&server, Duration::from_secs(2));
    let prepared = plan_cable_test(&client, SITE, "Closet switch", FACEPLATE, false)
        .await
        .expect("supported cable test should prepare");

    assert_eq!(prepared.target["port"], FACEPLATE);
    assert_eq!(prepared.target["api_port_number"], API_PORT);
    let report = apply_once(&prepared.backend, &prepared.plan, Duration::from_secs(2))
        .await
        .expect("diagnostic report");
    assert_eq!(report.outcome, Outcome::Verified);
    assert_eq!(
        report.observed,
        Some(DiagnosticState {
            started: true,
            complete: true,
        })
    );
    assert_eq!(prepared.backend.details()["observed"], result);

    let requests = server.finish();
    assert_eq!(requests.len(), 4);
    assert_eq!(requests[0].target, format!("/api/sites/{SITE}/inventory"));
    assert_eq!(
        requests[1].target,
        format!("/api/sites/{SITE}/cableTest/{DEVICE_ID}")
    );
    assert_eq!(requests[2].method, "POST");
    assert_eq!(
        requests[2].target,
        format!("/api/sites/{SITE}/cableTest/{DEVICE_ID}")
    );
    assert_eq!(body(&requests[2]), json!({"portNumber":API_PORT}));
    assert_eq!(
        requests[3].target,
        format!("/api/sites/{SITE}/cableTest/{DEVICE_ID}")
    );
}

#[tokio::test(start_paused = true)]
async fn old_complete_result_cannot_verify_a_new_cable_test() {
    let _clock = keep_clock_paused().await;
    let old = json!({
        "id":"old-test", "deviceId":DEVICE_ID, "portNumber":API_PORT, "state":"complete"
    });
    let server = MockServer::start(vec![
        reply(inventory()),
        reply(rows(vec![old.clone()])),
        reply(old),
        reply(rows(vec![json!({
            "id":"old-test", "deviceId":DEVICE_ID, "portNumber":API_PORT, "state":"complete"
        })])),
    ]);
    let client = make_client(&server, Duration::from_secs(2));
    let prepared = plan_cable_test(&client, SITE, DEVICE_ID, FACEPLATE, false)
        .await
        .expect("supported cable test should prepare");
    let report = apply_readback_once(&prepared.backend, &prepared.plan, Duration::from_millis(40))
        .await
        .expect("mutation report");
    assert_ne!(report.outcome, Outcome::Verified);
    assert_eq!(
        prepared.backend.details()["acknowledgment"]["id"],
        "old-test"
    );
    let _ = server.finish();
}

#[tokio::test(start_paused = true)]
async fn connectivity_abort_and_unrelated_get_rows_remain_unverified() {
    let _clock = keep_clock_paused().await;
    let acknowledged = json!({
        "id":"connectivity-new", "deviceId":DEVICE_ID,
        "address":"example.com", "state":"inProgress"
    });
    let abort = json!({
        "id":"connectivity-new", "deviceId":DEVICE_ID,
        "address":"example.com", "state":"abort", "result":"unknown",
        "vendorDetails":{"kept":true}
    });
    let server = MockServer::start(vec![
        reply(inventory()),
        reply(rows(vec![])),
        reply(acknowledged),
        reply(rows(vec![
            json!({
                "id":"another-test", "deviceId":DEVICE_ID,
                "address":"example.com", "state":"complete"
            }),
            abort.clone(),
        ])),
    ]);
    let client = make_client(&server, Duration::from_secs(2));
    let prepared = plan_connectivity_test(&client, SITE, DEVICE_ID, "example.com".into())
        .await
        .expect("valid DNS name should prepare");
    assert_eq!(prepared.target["address"], "example.com");
    let report = apply_readback_once(&prepared.backend, &prepared.plan, Duration::from_millis(40))
        .await
        .expect("mutation report");
    assert_eq!(report.outcome, Outcome::Unverified);
    assert_eq!(prepared.backend.details()["observed"], abort);
    assert!(
        plan_connectivity_test(&client, SITE, DEVICE_ID, "https://example.com".into())
            .await
            .is_err()
    );

    let requests = server.finish();
    assert_eq!(requests[2].method, "POST");
    assert_eq!(
        requests[2].target,
        format!("/api/sites/{SITE}/connectivityTest/{DEVICE_ID}?action=start")
    );
    assert_eq!(body(&requests[2]), json!({"address":"example.com"}));
}

#[tokio::test(start_paused = true)]
async fn foreign_acknowledgment_is_refused() {
    let _clock = keep_clock_paused().await;
    let server = MockServer::start(vec![
        reply(inventory()),
        reply(rows(vec![])),
        reply(json!({
            "id":"foreign-test", "deviceId":"bb:cc:dd:ee:ff:00",
            "portNumber":API_PORT, "state":"complete"
        })),
        reply(rows(vec![])),
    ]);
    let client = make_client(&server, Duration::from_secs(2));
    let prepared = plan_cable_test(&client, SITE, DEVICE_ID, FACEPLATE, false)
        .await
        .expect("supported cable test should prepare");
    let report = apply_readback_once(&prepared.backend, &prepared.plan, Duration::from_millis(40))
        .await
        .expect("mutation report");
    assert_ne!(report.outcome, Outcome::Verified);
    assert_eq!(
        report.request_error.as_ref().map(|error| error.kind),
        Some(ErrorKind::Unverified)
    );
    let _ = server.finish();
}

#[tokio::test]
async fn write_refuses_a_state_that_does_not_request_diagnostic_completion() {
    let server = MockServer::start(vec![reply(inventory()), reply(rows(vec![]))]);
    let client = make_client(&server, Duration::from_secs(2));
    let prepared = plan_cable_test(&client, SITE, DEVICE_ID, FACEPLATE, false)
        .await
        .expect("supported cable test should prepare");
    let error = prepared
        .backend
        .write(&DiagnosticState {
            started: true,
            complete: false,
        })
        .await
        .expect_err("wrong desired state must not start a test");
    assert_eq!(error.kind, ErrorKind::Usage);
    let requests = server.finish();
    assert_eq!(requests.len(), 2, "only inventory and baseline GETs run");
}
