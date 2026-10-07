use super::*;
use crate::{
    ErrorKind,
    client::{replace::candidates, site_actions::BridgePriorityAction},
    mutation::Outcome,
};
use serde_json::{Value, json};
use std::time::Duration;

const SITE: &str = "123e4567-e89b-12d3-a456-426614174000";
const SWITCH_ID: &str = "aa:bb:cc:dd:ee:ff";
const SECOND_SWITCH_ID: &str = "bb:cc:dd:ee:ff:00";
const AP_ID: &str = "cc:dd:ee:ff:00:11";

fn device(id: &str, device_type: &str) -> Value {
    json!({
        "id":id,
        "macAddress":id,
        "name":format!("{device_type} {id}"),
        "deviceType":device_type
    })
}

fn inventory_reply(devices: Vec<Value>) -> Reply {
    let count = devices.len() as u64;
    Reply::json(
        200,
        serde_json::to_vec(&json!({
            "kind":"resourceList",
            "totalCount":count,
            "matchingFilterCount":count,
            "pendingAvailability":null,
            "elements":devices
        }))
        .expect("serialize complete inventory"),
    )
}

fn spanning_tree_reply(priorities: Vec<Value>) -> Reply {
    Reply::json(
        200,
        serde_json::to_vec(&json!({"useRstp":true,"devicePriorities":priorities}))
            .expect("serialize spanning-tree response"),
    )
}

fn assert_inventory_get(request: &Request) {
    assert_eq!(request.method, "GET");
    assert_eq!(request.target, format!("/api/sites/{SITE}/inventory"));
    assert!(request.body.is_empty());
}

fn assert_spanning_tree_get(request: &Request) {
    assert_eq!(request.method, "GET");
    assert_eq!(request.target, format!("/api/sites/{SITE}/spanningTree"));
    assert!(request.body.is_empty());
}

#[tokio::test]
async fn bridge_priority_action_posts_once_and_keeps_missing_priorities_null_unverified() {
    let devices = vec![
        device(SWITCH_ID, "switch"),
        device(SECOND_SWITCH_ID, "switch"),
        device(AP_ID, "accessPoint"),
        device("00:11:22:33:44:99", "gateway"),
    ];
    let server = MockServer::start(vec![
        inventory_reply(devices.clone()),
        spanning_tree_reply(vec![
            json!({"id":SWITCH_ID,"priority":32768}),
            json!({"id":SECOND_SWITCH_ID}),
        ]),
        Reply::json(200, b"{}".to_vec()),
        inventory_reply(devices),
        spanning_tree_reply(vec![json!({"id":SWITCH_ID,"priority":4096})]),
    ]);
    let client = make_client(&server, Duration::from_secs(2));
    let action = BridgePriorityAction::prepare(&client, SITE)
        .await
        .expect("complete switch snapshot should prepare");
    assert_eq!(
        action.current,
        json!({
            "devices":[
                {"id":SWITCH_ID,"name":format!("switch {SWITCH_ID}"),"bridge_priority":32768},
                {"id":SECOND_SWITCH_ID,"name":format!("switch {SECOND_SWITCH_ID}"),"bridge_priority":null}
            ],
            "completion_verifiable":false
        })
    );

    let report = action
        .apply(Duration::from_secs(2))
        .await
        .expect("action request and snapshot should complete");
    assert_eq!(report.outcome, Outcome::Unverified);
    assert_eq!(report.error_kind(), Some(ErrorKind::Unverified));
    assert_eq!(report.readback_attempts, 1);
    assert_eq!(
        report.observed.expect("fresh snapshot should be retained"),
        json!({
            "devices":[
                {"id":SWITCH_ID,"name":format!("switch {SWITCH_ID}"),"bridge_priority":4096},
                {"id":SECOND_SWITCH_ID,"name":format!("switch {SECOND_SWITCH_ID}"),"bridge_priority":null}
            ],
            "completion_verifiable":false
        })
    );

    let requests = server.finish();
    assert_eq!(requests.len(), 5);
    assert_inventory_get(&requests[0]);
    assert_spanning_tree_get(&requests[1]);
    assert_eq!(requests[2].method, "POST");
    assert_eq!(
        requests[2].target,
        format!("/api/sites/{SITE}/spanningTree?action=computeDevicesBridgePriority")
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&requests[2].body).expect("empty JSON body"),
        json!({})
    );
    assert_inventory_get(&requests[3]);
    assert_spanning_tree_get(&requests[4]);
}

#[tokio::test]
async fn bridge_priority_refuses_bad_snapshots_and_never_replays_failed_action() {
    let duplicate_priorities = vec![
        (
            "duplicate priority identity",
            vec![device(SWITCH_ID, "switch")],
            vec![
                json!({"id":SWITCH_ID,"priority":4096}),
                json!({"id":SWITCH_ID.to_ascii_uppercase(),"priority":8192}),
            ],
        ),
        (
            "unknown inventory device type",
            vec![
                device(SWITCH_ID, "switch"),
                device(SECOND_SWITCH_ID, "router"),
            ],
            vec![json!({"id":SWITCH_ID,"priority":4096})],
        ),
    ];
    for (label, devices, priorities) in duplicate_priorities {
        let server = MockServer::start(vec![
            inventory_reply(devices),
            spanning_tree_reply(priorities),
        ]);
        let client = make_client(&server, Duration::from_secs(1));
        let error = match BridgePriorityAction::prepare(&client, SITE).await {
            Err(error) => error,
            Ok(_) => panic!("{label} should block the action"),
        };
        assert_eq!(error.kind, ErrorKind::Unverified, "{label}: {error}");
        let requests = server.finish();
        assert_eq!(requests.len(), 2, "{label} must not send a write");
        assert_inventory_get(&requests[0]);
        assert_spanning_tree_get(&requests[1]);
    }

    let devices = vec![device(SWITCH_ID, "switch")];
    let server = MockServer::start(vec![
        inventory_reply(devices.clone()),
        spanning_tree_reply(vec![json!({"id":SWITCH_ID,"priority":32768})]),
        Reply::json(503, br#"{"error":"unavailable"}"#.to_vec()),
        inventory_reply(devices),
        spanning_tree_reply(vec![json!({"id":SWITCH_ID,"priority":32768})]),
    ]);
    let client = make_client(&server, Duration::from_secs(2));
    let action = BridgePriorityAction::prepare(&client, SITE)
        .await
        .expect("initial snapshot should prepare");
    let report = action
        .apply(Duration::from_secs(2))
        .await
        .expect("failed action should still take one observation");
    assert_eq!(report.outcome, Outcome::Failed);
    assert_eq!(report.error_kind(), Some(ErrorKind::General));
    assert!(report.request_error.is_some());
    assert_eq!(report.readback_attempts, 1);
    assert!(
        report.observed.is_some(),
        "the snapshot may look good but cannot prove completion"
    );
    let requests = server.finish();
    assert_eq!(
        requests.len(),
        5,
        "POST is single-send; readback uses exactly two GETs"
    );
    assert_inventory_get(&requests[0]);
    assert_spanning_tree_get(&requests[1]);
    assert_eq!(requests[2].method, "POST");
    assert_eq!(
        requests[2].target,
        format!("/api/sites/{SITE}/spanningTree?action=computeDevicesBridgePriority")
    );
    assert_inventory_get(&requests[3]);
    assert_spanning_tree_get(&requests[4]);
}

#[tokio::test]
async fn replacement_candidates_use_complete_inventory_and_redact_secrets() {
    let raw_candidates = json!({
        "extendNetworkEnabled":false,
        "isExtendNetworkOutdoorMesh":false,
        "availableDevices":[{
            "device":{"id":SECOND_SWITCH_ID,"macAddress":SECOND_SWITCH_ID,"name":"Spare switch"},
            "deviceCompatibilitySeverity":"Compatible",
            "deviceCompatibilities":[],
            "isOnboarded":false,
            "sharedSecret":"candidate-shared-secret"
        }],
        "apiToken":"candidate-api-token",
        "futureField":{"nested":{"password":"candidate-password"}}
    });
    let mut target = device(SWITCH_ID, "switch");
    target["macAddress"] = json!(AP_ID);
    target["name"] = json!("Original switch");
    let server = MockServer::start(vec![
        inventory_reply(vec![target]),
        Reply::json(
            200,
            serde_json::to_vec(&raw_candidates).expect("serialize candidates"),
        ),
    ]);
    let client = make_client(&server, Duration::from_secs(2));
    let result = candidates(&client, SITE, "Original switch")
        .await
        .expect("replacement response should be returned after redaction");
    let mut expected = raw_candidates;
    expected["availableDevices"][0]["sharedSecret"] = json!("(redacted)");
    expected["apiToken"] = json!("(redacted)");
    expected["futureField"]["nested"]["password"] = json!("(redacted)");
    assert_eq!(
        result, expected,
        "response keeps its direct shape and unknown fields"
    );

    let requests = server.finish();
    assert_eq!(requests.len(), 2);
    assert_inventory_get(&requests[0]);
    assert_eq!(requests[1].method, "GET");
    assert_eq!(
        requests[1].target,
        format!("/api/sites/{SITE}/replaceDevice/{SWITCH_ID}")
    );
    assert!(requests[1].body.is_empty());

    let server = MockServer::start(vec![inventory_reply(vec![device(SWITCH_ID, "switch")])]);
    let client = make_client(&server, Duration::from_secs(1));
    let error = candidates(&client, SITE, "unknown selector")
        .await
        .expect_err("unknown selector must not trigger a candidate fetch");
    assert_eq!(error.kind, ErrorKind::Usage);
    let requests = server.finish();
    assert_eq!(requests.len(), 1);
    assert_inventory_get(&requests[0]);
}
