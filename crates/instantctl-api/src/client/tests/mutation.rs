use super::*;
use crate::{
    ErrorKind,
    locator::Locator,
    mutation::{Outcome, apply_once},
};
use serde_json::{Value, json};
use std::time::Duration;

const SITE: &str = "123e4567-e89b-12d3-a456-426614174000";
const DEVICE_ID: &str = "aa:bb:cc:dd:ee:ff";
const DEVICE_NAME: &str = "Hallway AP";

fn inventory(active: bool) -> Vec<u8> {
    inventory_entries(vec![json!({
        "kind":"inventory",
        "id":DEVICE_ID,
        "macAddress":DEVICE_ID,
        "name":DEVICE_NAME,
        "capabilities":{"locatorLed":true},
        "isLocatorLedActive":active
    })])
}

fn inventory_entries(elements: Vec<Value>) -> Vec<u8> {
    let count = elements.len() as u64;
    serde_json::to_vec(&json!({
        "kind":"resourceList",
        "totalCount":count,
        "matchingFilterCount":count,
        "pendingAvailability":null,
        "elements":elements
    }))
    .expect("serialize inventory")
}

fn locator_ack() -> Vec<u8> {
    serde_json::to_vec(&json!({"kind":"inventory","id":DEVICE_ID}))
        .expect("serialize locator acknowledgment")
}

fn assert_inventory_get(request: &Request) {
    assert_eq!(request.method, "GET");
    assert_eq!(request.target, format!("/api/sites/{SITE}/inventory"));
}

fn assert_locator_post(request: &Request, active: bool) {
    let action = if active {
        "activateLocatorLED"
    } else {
        "deactivateLocatorLED"
    };
    assert_eq!(request.method, "POST");
    assert_eq!(
        request.target,
        format!("/api/sites/{SITE}/inventory/{DEVICE_ID}?action={action}")
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&request.body).expect("locator action JSON"),
        json!({"id":DEVICE_ID})
    );
}

#[tokio::test]
async fn locator_plan_reads_inventory_without_writing() {
    let server = MockServer::start(vec![Reply::json(200, inventory(false))]);
    let client = make_client(&server, Duration::from_secs(1));
    let planned = Locator::plan(&client, SITE, DEVICE_NAME, true).await;
    let (locator, plan) = match planned {
        Ok(value) => value,
        Err(error) => panic!("locator planning should succeed: {error}"),
    };

    assert!(!plan.current);
    assert!(plan.desired);
    assert_eq!(locator.device_id(), DEVICE_ID);
    assert_eq!(locator.device_name(), Some(DEVICE_NAME));
    let requests = server.finish();
    assert_eq!(requests.len(), 1, "planning must not send a write");
    assert_inventory_get(&requests[0]);
}

#[tokio::test]
async fn invalid_site_is_rejected_before_any_network_request() {
    let server = MockServer::start(vec![]);
    let client = make_client(&server, Duration::from_secs(1));
    let planned = Locator::plan(&client, "not-a-site-id", DEVICE_NAME, true).await;
    let error = match planned {
        Ok(_) => panic!("invalid site identifier must fail"),
        Err(error) => error,
    };

    assert_eq!(error.kind, ErrorKind::Config);
    assert_eq!(server.finish().len(), 0);
}

#[tokio::test]
async fn locator_plan_rejects_unsupported_or_unknown_state_without_writing() {
    let cases = [
        (
            json!({
                "kind":"inventory",
                "id":DEVICE_ID,
                "macAddress":DEVICE_ID,
                "name":DEVICE_NAME,
                "capabilities":{"locatorLed":false},
                "isLocatorLedActive":false
            }),
            ErrorKind::Unsupported,
        ),
        (
            json!({
                "kind":"inventory",
                "id":DEVICE_ID,
                "macAddress":DEVICE_ID,
                "name":DEVICE_NAME,
                "isLocatorLedActive":false
            }),
            ErrorKind::General,
        ),
        (
            json!({
                "kind":"inventory",
                "id":DEVICE_ID,
                "macAddress":DEVICE_ID,
                "name":DEVICE_NAME,
                "capabilities":{"locatorLed":true}
            }),
            ErrorKind::General,
        ),
    ];
    for (entry, expected_kind) in cases {
        let body = inventory_entries(vec![entry]);
        let server = MockServer::start(vec![Reply::json(200, body)]);
        let client = make_client(&server, Duration::from_secs(1));
        let planned = Locator::plan(&client, SITE, DEVICE_ID, true).await;
        let error = match planned {
            Ok(_) => panic!("unsupported or unknown locator state must fail"),
            Err(error) => error,
        };

        assert_eq!(error.kind, expected_kind);
        let requests = server.finish();
        assert_eq!(requests.len(), 1, "planning must reject before POST");
        assert_inventory_get(&requests[0]);
    }
}

#[tokio::test]
async fn locator_plan_rejects_ambiguous_name_before_writing() {
    let second_id = "bb:cc:dd:ee:ff:00";
    let body = inventory_entries(vec![
        json!({
            "kind":"inventory",
            "id":DEVICE_ID,
            "macAddress":DEVICE_ID,
            "name":DEVICE_NAME,
            "capabilities":{"locatorLed":true},
            "isLocatorLedActive":false
        }),
        json!({
            "kind":"inventory",
            "id":second_id,
            "macAddress":second_id,
            "name":DEVICE_NAME,
            "capabilities":{"locatorLed":true},
            "isLocatorLedActive":false
        }),
    ]);
    let server = MockServer::start(vec![Reply::json(200, body)]);
    let client = make_client(&server, Duration::from_secs(1));
    let planned = Locator::plan(&client, SITE, DEVICE_NAME, true).await;
    let error = match planned {
        Ok(_) => panic!("ambiguous device name must fail"),
        Err(error) => error,
    };

    assert_eq!(error.kind, ErrorKind::Usage);
    assert!(error.message.contains("ambiguous"));
    let requests = server.finish();
    assert_eq!(requests.len(), 1, "ambiguous plan must not send a write");
    assert_inventory_get(&requests[0]);
}

#[tokio::test]
async fn locator_apply_sends_one_on_or_off_action_and_polls_until_a_later_match() {
    for active in [true, false] {
        let initial = !active;
        let server = MockServer::start(vec![
            Reply::json(200, inventory(initial)),
            Reply::json(200, locator_ack()),
            Reply::json(200, inventory(initial)),
            Reply::json(200, inventory(initial)),
            Reply::json(200, inventory(active)),
        ]);
        let client = make_client(&server, Duration::from_secs(2));
        let planned = Locator::plan(&client, SITE, DEVICE_ID, active).await;
        let (locator, plan) = match planned {
            Ok(value) => value,
            Err(error) => panic!("locator planning should succeed: {error}"),
        };
        let report = apply_once(&locator, &plan, Duration::from_secs(2))
            .await
            .expect("apply state machine should complete");

        assert_eq!(report.outcome, Outcome::Verified);
        assert_eq!(report.observed, Some(active));
        assert_eq!(report.readback_attempts, 3);
        let requests = server.finish();
        assert_eq!(requests.len(), 5);
        assert_inventory_get(&requests[0]);
        assert_locator_post(&requests[1], active);
        assert_inventory_get(&requests[2]);
        assert_inventory_get(&requests[3]);
        assert_inventory_get(&requests[4]);
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.method == "POST")
                .count(),
            1,
            "readback mismatch must never resubmit the action"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn successful_action_with_persistently_mismatching_state_is_unverified() {
    let _clock = keep_clock_paused().await;
    let server = MockServer::start(vec![
        Reply::json(200, inventory(false)),
        Reply::json(200, locator_ack()),
        Reply::json(200, inventory(false)),
    ]);
    let client = make_client(&server, Duration::from_secs(1));
    let planned = Locator::plan(&client, SITE, DEVICE_ID, true).await;
    let (locator, plan) = match planned {
        Ok(value) => value,
        Err(error) => panic!("locator planning should succeed: {error}"),
    };
    let report = apply_readback_once(&locator, &plan, Duration::from_millis(100))
        .await
        .expect("an unverified report is a valid outcome");

    assert_eq!(report.outcome, Outcome::Unverified);
    assert_eq!(report.error_kind(), Some(ErrorKind::Unverified));
    assert_eq!(report.observed, Some(false));
    let requests = server.finish();
    assert_eq!(requests.len(), 3);
    assert_inventory_get(&requests[0]);
    assert_locator_post(&requests[1], true);
    assert_inventory_get(&requests[2]);
}

#[tokio::test]
async fn failed_action_with_matching_readback_remains_a_request_failure() {
    let server = MockServer::start(vec![
        Reply::json(200, inventory(false)),
        Reply::json(503, b"unavailable".to_vec()),
        Reply::json(200, inventory(true)),
    ]);
    let client = make_client(&server, Duration::from_secs(1));
    let planned = Locator::plan(&client, SITE, DEVICE_ID, true).await;
    let (locator, plan) = match planned {
        Ok(value) => value,
        Err(error) => panic!("locator planning should succeed: {error}"),
    };
    let report = apply_once(&locator, &plan, Duration::from_secs(1))
        .await
        .expect("request failure still has a report");

    assert_eq!(report.outcome, Outcome::RequestFailedStateMatches);
    assert_eq!(report.error_kind(), Some(ErrorKind::General));
    assert_eq!(report.observed, Some(true));
    assert!(report.request_error.is_some());
    let requests = server.finish();
    assert_eq!(requests.len(), 3);
    assert_inventory_get(&requests[0]);
    assert_locator_post(&requests[1], true);
    assert_inventory_get(&requests[2]);
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.method == "POST")
            .count(),
        1
    );
}

#[tokio::test]
async fn invalid_action_acknowledgments_remain_request_failures_after_matching_readback() {
    for acknowledgment in [
        json!({"kind":"inventory","id":"bb:cc:dd:ee:ff:00"}),
        json!({"kind":"","id":DEVICE_ID}),
    ] {
        let server = MockServer::start(vec![
            Reply::json(200, inventory(false)),
            Reply::json(200, serde_json::to_vec(&acknowledgment).unwrap()),
            Reply::json(200, inventory(true)),
        ]);
        let client = make_client(&server, Duration::from_secs(1));
        let planned = Locator::plan(&client, SITE, DEVICE_ID, true).await;
        let (locator, plan) = match planned {
            Ok(value) => value,
            Err(error) => panic!("locator planning should succeed: {error}"),
        };
        let report = apply_once(&locator, &plan, Duration::from_secs(1))
            .await
            .expect("invalid acknowledgment still produces a report");

        assert_eq!(report.outcome, Outcome::RequestFailedStateMatches);
        assert_eq!(report.error_kind(), Some(ErrorKind::General));
        assert!(report.request_error.is_some());
        assert_eq!(report.observed, Some(true));
        let requests = server.finish();
        assert_eq!(requests.len(), 3);
        assert_inventory_get(&requests[0]);
        assert_locator_post(&requests[1], true);
        assert_inventory_get(&requests[2]);
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.method == "POST")
                .count(),
            1
        );
    }
}

#[tokio::test(start_paused = true)]
async fn failed_action_with_mismatching_readback_remains_failed() {
    let _clock = keep_clock_paused().await;
    let server = MockServer::start(vec![
        Reply::json(200, inventory(false)),
        Reply::json(503, b"unavailable".to_vec()),
        Reply::json(200, inventory(false)),
    ]);
    let client = make_client(&server, Duration::from_secs(1));
    let planned = Locator::plan(&client, SITE, DEVICE_ID, true).await;
    let (locator, plan) = match planned {
        Ok(value) => value,
        Err(error) => panic!("locator planning should succeed: {error}"),
    };
    let report = apply_readback_once(&locator, &plan, Duration::from_millis(100))
        .await
        .expect("failed request still has a report");

    assert_eq!(report.outcome, Outcome::Failed);
    assert_eq!(report.error_kind(), Some(ErrorKind::General));
    assert_eq!(report.observed, Some(false));
    assert!(report.request_error.is_some());
    let requests = server.finish();
    assert_eq!(requests.len(), 3);
    assert_inventory_get(&requests[0]);
    assert_locator_post(&requests[1], true);
    assert_inventory_get(&requests[2]);
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.method == "POST")
            .count(),
        1
    );
}
