use super::*;
use crate::{
    Error,
    client::firmware::{self, WindowPatch},
    mutation::Outcome,
};
use serde_json::{Value, json};

const SITE: &str = "123e4567-e89b-12d3-a456-426614174000";

fn reply(value: Value) -> Reply {
    Reply::json(200, serde_json::to_vec(&value).expect("serialize reply"))
}

fn maintenance() -> Value {
    json!({
        "kind":"maintenance",
        "day":"sunday",
        "startTime":"03:00",
        "updateDelayInDays":7,
        "state":"not-in-maintenance",
        "newUpdateVersion":"3.2.1",
        "vendorConfig":{"mode":"opaque","list":[1,null,true]}
    })
}

fn active_maintenance() -> Value {
    let mut value = maintenance();
    value["state"] = json!("started-maintenance");
    value
}

fn request_json(request: &Request) -> Value {
    serde_json::from_slice(&request.body).expect("request body should be JSON")
}

fn assert_request(request: &Request, method: &str, target: &str) {
    assert_eq!(request.method, method);
    assert_eq!(request.target, target);
    assert_eq!(
        request.headers.get("authorization").map(String::as_str),
        Some("Bearer netcli-test-token-sentinel")
    );
}

fn expect_error<T>(result: Result<T, Error>) -> Error {
    match result {
        Err(error) => error,
        Ok(_) => panic!("operation should fail"),
    }
}

#[tokio::test]
async fn maintenance_get_requires_kind_and_valid_site_before_http() {
    let server = MockServer::start(vec![reply(json!({"state":"not-in-maintenance"}))]);
    let client = make_client(&server, Duration::from_secs(1));
    let error = expect_error(firmware::get(&client, SITE).await);
    assert_eq!(error.kind, ErrorKind::Unverified);
    let requests = server.finish();
    assert_eq!(requests.len(), 1);
    assert_request(
        &requests[0],
        "GET",
        &format!("/api/sites/{SITE}/maintenance"),
    );

    let server = MockServer::start(vec![]);
    let client = make_client(&server, Duration::from_secs(1));
    let error = expect_error(firmware::get(&client, "not-a-site").await);
    assert_eq!(error.kind, ErrorKind::Config);
    assert!(server.finish().is_empty());
}

#[tokio::test(start_paused = true)]
async fn window_update_puts_full_object_and_verifies_only_patched_fields() {
    let _clock = keep_clock_paused().await;
    let before = maintenance();
    let mut sent = before.clone();
    sent["day"] = json!("friday");
    sent["startTime"] = json!("04:15");
    sent["updateDelayInDays"] = json!(14);
    let mut after = sent.clone();
    after["vendorConfig"]["mode"] = json!("changed-by-server");
    let server = MockServer::start(vec![
        reply(before),
        reply(json!({"kind":"maintenance"})),
        reply(after),
    ]);
    let client = make_client(&server, Duration::from_secs(2));
    let (backend, plan) = firmware::set_window(
        &client,
        SITE,
        WindowPatch {
            day: Some("friday".into()),
            start_time: Some("04:15".into()),
            update_delay_in_days: Some(14),
        },
    )
    .await
    .expect("valid window patch should prepare");
    assert_eq!(
        plan.current,
        json!({"day":"sunday","startTime":"03:00","updateDelayInDays":7})
    );
    assert_eq!(
        plan.desired,
        json!({"day":"friday","startTime":"04:15","updateDelayInDays":14})
    );

    let report = apply_readback_once(&backend, &plan, Duration::from_secs(2))
        .await
        .expect("window update and readback should finish");
    assert_eq!(report.outcome, Outcome::Verified);

    let requests = server.finish();
    assert_eq!(requests.len(), 3);
    assert_request(
        &requests[0],
        "GET",
        &format!("/api/sites/{SITE}/maintenance"),
    );
    assert_request(
        &requests[1],
        "PUT",
        &format!("/api/sites/{SITE}/maintenance"),
    );
    assert_request(
        &requests[2],
        "GET",
        &format!("/api/sites/{SITE}/maintenance"),
    );
    assert_eq!(request_json(&requests[1]), sent);
}

#[tokio::test]
async fn invalid_window_fields_are_rejected_before_http() {
    for patch in [
        WindowPatch::default(),
        WindowPatch {
            day: Some("Sunday".into()),
            ..WindowPatch::default()
        },
        WindowPatch {
            start_time: Some("25:00".into()),
            ..WindowPatch::default()
        },
        WindowPatch {
            update_delay_in_days: Some(8),
            ..WindowPatch::default()
        },
    ] {
        let server = MockServer::start(vec![]);
        let client = make_client(&server, Duration::from_secs(1));
        let error = expect_error(firmware::set_window(&client, SITE, patch).await);
        assert_eq!(error.kind, ErrorKind::Usage);
        assert!(server.finish().is_empty());
    }
}

#[tokio::test(start_paused = true)]
async fn update_now_uses_action_route_and_verifies_only_initiation() {
    let _clock = keep_clock_paused().await;
    let server = MockServer::start(vec![
        reply(maintenance()),
        reply(maintenance()),
        reply(json!({})),
        reply(active_maintenance()),
    ]);
    let client = make_client(&server, Duration::from_secs(2));
    let (backend, plan) = firmware::update_now(&client, SITE)
        .await
        .expect("available firmware should permit action planning");
    assert_eq!(plan.current, json!({"update_started":false}));
    assert_eq!(plan.desired, json!({"update_started":true}));

    let report = apply_readback_once(&backend, &plan, Duration::from_secs(2))
        .await
        .expect("update action should be attempted and observed");
    assert_eq!(report.outcome, Outcome::Verified);
    assert_eq!(report.observed, Some(json!({"update_started":true})));

    let requests = server.finish();
    assert_eq!(requests.len(), 4);
    assert_request(
        &requests[0],
        "GET",
        &format!("/api/sites/{SITE}/maintenance"),
    );
    assert_request(
        &requests[1],
        "GET",
        &format!("/api/sites/{SITE}/maintenance"),
    );
    assert_request(
        &requests[2],
        "POST",
        &format!("/api/sites/{SITE}/maintenance?action=updateSoftwareNow"),
    );
    assert_request(
        &requests[3],
        "GET",
        &format!("/api/sites/{SITE}/maintenance"),
    );
    assert_eq!(request_json(&requests[2]), json!({}));
}

#[tokio::test(start_paused = true)]
async fn schedule_uses_exact_local_timestamp_and_readback() {
    let _clock = keep_clock_paused().await;
    let at = "2026-10-21T04:15:30";
    let mut scheduled = maintenance();
    scheduled["updateScheduleDateTime"] = json!(at);
    let server = MockServer::start(vec![
        reply(maintenance()),
        reply(maintenance()),
        reply(json!({})),
        reply(scheduled),
    ]);
    let client = make_client(&server, Duration::from_secs(2));
    let (backend, plan) = firmware::schedule(&client, SITE, at.into())
        .await
        .expect("valid local schedule should prepare");
    assert_eq!(plan.desired, json!({"update_schedule_date_time":at}));

    let report = apply_readback_once(&backend, &plan, Duration::from_secs(2))
        .await
        .expect("schedule action should be attempted and observed");
    assert_eq!(report.outcome, Outcome::Verified);
    assert_eq!(report.observed, Some(plan.desired));

    let requests = server.finish();
    assert_eq!(requests.len(), 4);
    assert_request(
        &requests[0],
        "GET",
        &format!("/api/sites/{SITE}/maintenance"),
    );
    assert_request(
        &requests[1],
        "GET",
        &format!("/api/sites/{SITE}/maintenance"),
    );
    assert_request(
        &requests[2],
        "POST",
        &format!("/api/sites/{SITE}/maintenance/schedule"),
    );
    assert_request(
        &requests[3],
        "GET",
        &format!("/api/sites/{SITE}/maintenance"),
    );
    assert_eq!(request_json(&requests[2]), json!({"localDateTime":at}));
}

#[tokio::test]
async fn schedule_rejects_offset_or_invalid_dates_before_http() {
    for at in [
        "2026-10-21T25:00:00",
        "2026-02-29T04:15:30",
        "2026-11-02T03:00:60",
        "2026-10-21T04:15:30Z",
        "2026-10-21T04:15:30+03:00",
    ] {
        let server = MockServer::start(vec![]);
        let client = make_client(&server, Duration::from_secs(1));
        let error = expect_error(firmware::schedule(&client, SITE, at.into()).await);
        assert_eq!(error.kind, ErrorKind::Usage);
        assert!(server.finish().is_empty());
    }
}

#[tokio::test]
async fn update_now_refuses_active_unknown_or_unavailable_state() {
    let mut unknown_state = maintenance();
    unknown_state["state"] = json!("future-state");
    let mut unavailable = maintenance();
    unavailable["newUpdateVersion"] = json!("");
    let mut missing_version = maintenance();
    missing_version
        .as_object_mut()
        .unwrap()
        .remove("newUpdateVersion");
    let mut null_version = maintenance();
    null_version["newUpdateVersion"] = Value::Null;
    let mut malformed_version = maintenance();
    malformed_version["newUpdateVersion"] = json!(42);
    for (body, expected) in [
        (active_maintenance(), ErrorKind::Unsupported),
        (unknown_state, ErrorKind::Unverified),
        (unavailable, ErrorKind::Unsupported),
        (missing_version, ErrorKind::Unverified),
        (null_version, ErrorKind::Unverified),
        (malformed_version, ErrorKind::Unverified),
    ] {
        let server = MockServer::start(vec![reply(body)]);
        let client = make_client(&server, Duration::from_secs(1));
        let error = expect_error(firmware::update_now(&client, SITE).await);
        assert_eq!(error.kind, expected);
        let requests = server.finish();
        assert_eq!(requests.len(), 1);
        assert_request(
            &requests[0],
            "GET",
            &format!("/api/sites/{SITE}/maintenance"),
        );
    }
}

#[tokio::test(start_paused = true)]
async fn update_now_rechecks_eligibility_before_posting() {
    let _clock = keep_clock_paused().await;
    let server = MockServer::start(vec![
        reply(maintenance()),
        reply(active_maintenance()),
        reply(active_maintenance()),
    ]);
    let client = make_client(&server, Duration::from_secs(2));
    let (backend, plan) = firmware::update_now(&client, SITE)
        .await
        .expect("initial inactive state should permit planning");
    let report = apply_readback_once(&backend, &plan, Duration::from_secs(2))
        .await
        .expect("concurrent update should become a failed action report");
    assert_ne!(report.outcome, Outcome::Verified);
    assert_eq!(report.error_kind(), Some(ErrorKind::Unsupported));
    let requests = server.finish();
    assert_eq!(requests.len(), 3);
    assert!(requests.iter().all(|request| request.method != "POST"));
}

#[tokio::test(start_paused = true)]
async fn failed_update_post_remains_an_error_even_when_readback_is_active() {
    let _clock = keep_clock_paused().await;
    let server = MockServer::start(vec![
        reply(maintenance()),
        reply(maintenance()),
        Reply::json(400, br#"{"error":"rejected"}"#.to_vec()),
        reply(active_maintenance()),
    ]);
    let client = make_client(&server, Duration::from_secs(2));
    let (backend, plan) = firmware::update_now(&client, SITE)
        .await
        .expect("available update should prepare");
    let report = apply_readback_once(&backend, &plan, Duration::from_secs(2))
        .await
        .expect("failed request should still produce readback report");
    assert_eq!(report.outcome, Outcome::RequestFailedStateMatches);
    assert!(report.request_error.is_some());
    assert_eq!(report.error_kind(), Some(ErrorKind::ClientError));
    let requests = server.finish();
    assert_eq!(requests.len(), 4);
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.method == "POST")
            .count(),
        1
    );
}

#[tokio::test(start_paused = true)]
async fn schedule_readback_does_not_guess_across_timezone_offsets() {
    let _clock = keep_clock_paused().await;
    let at = "2026-10-21T04:15:30";
    let mut different = maintenance();
    different["updateScheduleDateTime"] = json!("2026-10-21T01:15:30Z");
    let server = MockServer::start(vec![
        reply(maintenance()),
        reply(maintenance()),
        reply(json!({})),
        reply(different),
    ]);
    let client = make_client(&server, Duration::from_secs(2));
    let (backend, plan) = firmware::schedule(&client, SITE, at.into())
        .await
        .expect("valid local date should prepare");
    let report = apply_readback_once(&backend, &plan, Duration::from_secs(2))
        .await
        .expect("different timestamp should stay unverified");
    assert_eq!(report.outcome, Outcome::Unverified);
    assert_eq!(
        report.observed,
        Some(json!({"update_schedule_date_time":"2026-10-21T01:15:30Z"}))
    );
    let requests = server.finish();
    assert_eq!(requests.len(), 4);
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.method == "POST")
            .count(),
        1
    );
}
