use super::*;
use crate::{
    ErrorKind,
    device::{
        ConfigChange, LedMode, StaticManagementIp, details, plan_forget, plan_reboot, plan_update,
        power_usage,
    },
    mutation::{Outcome, apply_once},
};
use serde_json::{Value, json};
use std::{net::Ipv4Addr, time::Duration};

const SITE: &str = "123e4567-e89b-12d3-a456-426614174000";
const DEVICE_ID: &str = "aa:bb:cc:dd:ee:ff";
const OTHER_ID: &str = "bb:cc:dd:ee:ff:00";
const DEVICE_NAME: &str = "Hallway AP";

fn device() -> Value {
    json!({
        "kind":"inventory",
        "id":DEVICE_ID,
        "macAddress":DEVICE_ID,
        "name":DEVICE_NAME,
        "deviceType":"accessPoint",
        "deviceRole":"accessPoint",
        "status":"up",
        "operationalState":"active",
        "uptimeInSeconds":3600,
        "ledMode":"led_on",
        "managementIpAddress":{
            "ipAssignmentScheme":"dhcp",
            "staticIpAddress":"192.168.10.25",
            "staticIpAddressPrefixLength":24,
            "gatewayIpAddress":"192.168.10.1",
            "dnsIpAddress":"1.1.1.1",
            "secondaryDnsIpAddress":"8.8.8.8",
            "vendorConfig":{"preserve":true}
        },
        "capabilities":{"ledMode":true,"powerUsage":true},
        "vendorMetadata":{"owner":"network-team"}
    })
}

fn inventory(devices: Vec<Value>) -> Vec<u8> {
    let count = devices.len() as u64;
    serde_json::to_vec(&json!({
        "kind":"resourceList",
        "totalCount":count,
        "matchingFilterCount":count,
        "pendingAvailability":null,
        "elements":devices
    }))
    .expect("serialize complete inventory")
}

fn ack() -> Vec<u8> {
    serde_json::to_vec(&json!({"kind":"inventory","id":DEVICE_ID}))
        .expect("serialize device acknowledgment")
}

fn assert_inventory_get(request: &Request) {
    assert_eq!(request.method, "GET");
    assert_eq!(request.target, format!("/api/sites/{SITE}/inventory"));
}

fn assert_write_path(request: &Request) {
    assert_eq!(request.method, "PUT");
    assert_eq!(
        request.target,
        format!("/api/sites/{SITE}/inventory/{DEVICE_ID}")
    );
}

#[tokio::test]
async fn rename_puts_the_complete_selected_object_and_verifies_only_the_owned_name() {
    let mut expected_put = device();
    expected_put["name"] = json!("Hallway AP  ");

    let mut readback = expected_put.clone();
    readback["uptimeInSeconds"] = json!(3650);
    readback["lastSeenAt"] = json!("newer-metadata");
    let server = MockServer::start(vec![
        Reply::json(200, inventory(vec![device()])),
        Reply::json(
            200,
            br#"{"kind":"inventory","id":"aa:bb:cc:dd:ee:ff"}"#.to_vec(),
        ),
        Reply::json(200, inventory(vec![readback])),
    ]);
    let client = make_client(&server, Duration::from_secs(2));
    let prepared = plan_update(
        &client,
        SITE,
        DEVICE_NAME,
        ConfigChange::Name("Hallway AP  ".to_owned()),
    )
    .await
    .expect("valid rename should prepare");

    assert_eq!(prepared.target["device_id"], DEVICE_ID);
    let report = apply_once(&prepared.backend, &prepared.plan, Duration::from_secs(2))
        .await
        .expect("rename mutation should report");
    assert_eq!(report.outcome, Outcome::Verified);
    assert_eq!(report.observed, Some(json!("Hallway AP  ")));

    let requests = server.finish();
    assert_eq!(requests.len(), 3);
    assert_inventory_get(&requests[0]);
    assert_write_path(&requests[1]);
    assert_eq!(
        serde_json::from_slice::<Value>(&requests[1].body).expect("full device JSON"),
        expected_put,
        "PUT must preserve fields outside the rename"
    );
    assert_inventory_get(&requests[2]);
}

#[tokio::test]
async fn static_management_update_preserves_unknown_config_and_omits_cleared_secondary_dns() {
    let mut expected_put = device();
    expected_put["managementIpAddress"] = json!({
        "ipAssignmentScheme":"static",
        "staticIpAddress":"192.168.10.50",
        "staticIpAddressPrefixLength":24,
        "gatewayIpAddress":"192.168.10.1",
        "dnsIpAddress":"1.1.1.1",
        "vendorConfig":{"preserve":true}
    });
    let mut readback = expected_put.clone();
    readback["uptimeInSeconds"] = json!(3610);
    let server = MockServer::start(vec![
        Reply::json(200, inventory(vec![device()])),
        Reply::json(
            200,
            br#"{"kind":"inventory","id":"aa:bb:cc:dd:ee:ff"}"#.to_vec(),
        ),
        Reply::json(200, inventory(vec![readback])),
    ]);
    let client = make_client(&server, Duration::from_secs(2));
    let prepared = plan_update(
        &client,
        SITE,
        DEVICE_ID,
        ConfigChange::ManagementIp(StaticManagementIp {
            address: Ipv4Addr::new(192, 168, 10, 50),
            prefix_length: 24,
            gateway: Ipv4Addr::new(192, 168, 10, 1),
            dns: Ipv4Addr::new(1, 1, 1, 1),
            secondary_dns: None,
        }),
    )
    .await
    .expect("valid static management IP should prepare");

    let report = apply_once(&prepared.backend, &prepared.plan, Duration::from_secs(2))
        .await
        .expect("management IP mutation should report");
    assert_eq!(report.outcome, Outcome::Verified);
    let requests = server.finish();
    assert_eq!(requests.len(), 3);
    assert_inventory_get(&requests[0]);
    assert_write_path(&requests[1]);
    assert_eq!(
        serde_json::from_slice::<Value>(&requests[1].body).expect("full device JSON"),
        expected_put,
        "PUT must preserve unknown nested fields and remove the cleared owned field"
    );
    assert_inventory_get(&requests[2]);
}

#[tokio::test]
async fn led_update_writes_only_the_supported_led_mode_and_verifies_it() {
    let mut expected_put = device();
    expected_put["ledMode"] = json!("led_quiet");
    let server = MockServer::start(vec![
        Reply::json(200, inventory(vec![device()])),
        Reply::json(
            200,
            br#"{"kind":"inventory","id":"aa:bb:cc:dd:ee:ff"}"#.to_vec(),
        ),
        Reply::json(200, inventory(vec![expected_put.clone()])),
    ]);
    let client = make_client(&server, Duration::from_secs(2));
    let prepared = plan_update(
        &client,
        SITE,
        DEVICE_ID,
        ConfigChange::LedMode(LedMode::Quiet),
    )
    .await
    .expect("supported LED mode should prepare");

    let report = apply_once(&prepared.backend, &prepared.plan, Duration::from_secs(2))
        .await
        .expect("LED mutation should report");
    assert_eq!(report.outcome, Outcome::Verified);
    assert_eq!(report.observed, Some(json!("led_quiet")));
    let requests = server.finish();
    assert_eq!(requests.len(), 3);
    assert_inventory_get(&requests[0]);
    assert_write_path(&requests[1]);
    assert_eq!(
        serde_json::from_slice::<Value>(&requests[1].body).expect("full device JSON"),
        expected_put
    );
    assert_inventory_get(&requests[2]);
}

#[tokio::test]
async fn update_guards_refuse_duplicate_names_unknown_led_and_cross_subnet_gateway() {
    let mut duplicate = device();
    duplicate["id"] = json!(OTHER_ID);
    duplicate["macAddress"] = json!(OTHER_ID);
    duplicate["name"] = json!("Other AP");
    let mut no_led_capability = device();
    no_led_capability["capabilities"]["ledMode"] = json!(false);
    let mut missing_led_capability = device();
    missing_led_capability["capabilities"]
        .as_object_mut()
        .expect("capabilities object")
        .remove("ledMode");
    let mut unknown_led_mode = device();
    unknown_led_mode["ledMode"] = json!("led_blink");
    let cases = [
        (
            inventory(vec![device(), duplicate]),
            DEVICE_ID,
            ConfigChange::Name("Other AP".into()),
            1,
            ErrorKind::Usage,
            "already belongs",
        ),
        (
            inventory(vec![no_led_capability]),
            DEVICE_ID,
            ConfigChange::LedMode(LedMode::Quiet),
            1,
            ErrorKind::Unsupported,
            "does not support",
        ),
        (
            inventory(vec![missing_led_capability]),
            DEVICE_ID,
            ConfigChange::LedMode(LedMode::Quiet),
            1,
            ErrorKind::General,
            "capability is missing or unknown",
        ),
        (
            inventory(vec![unknown_led_mode]),
            DEVICE_ID,
            ConfigChange::LedMode(LedMode::Quiet),
            1,
            ErrorKind::General,
            "LED mode is missing or unknown",
        ),
        (
            inventory(vec![device()]),
            DEVICE_ID,
            ConfigChange::ManagementIp(StaticManagementIp {
                address: Ipv4Addr::new(192, 168, 10, 50),
                prefix_length: 24,
                gateway: Ipv4Addr::new(192, 168, 11, 1),
                dns: Ipv4Addr::new(1, 1, 1, 1),
                secondary_dns: None,
            }),
            0,
            ErrorKind::Usage,
            "share a subnet",
        ),
    ];

    for (body, selector, change, expected_reads, expected_kind, expected_message) in cases {
        let server = MockServer::start(vec![Reply::json(200, body)]);
        let client = make_client(&server, Duration::from_secs(1));
        let result = plan_update(&client, SITE, selector, change).await;
        let error = match result {
            Ok(_) => panic!("invalid update should be refused"),
            Err(error) => error,
        };
        assert_eq!(error.kind, expected_kind);
        assert!(error.message.contains(expected_message), "{error:?}");
        let requests = server.finish();
        assert_eq!(
            requests.len(),
            expected_reads,
            "guard must run before any write"
        );
        if expected_reads == 1 {
            assert_inventory_get(&requests[0]);
        }
    }
}

#[tokio::test]
async fn failed_put_with_matching_readback_is_reported_as_request_failure_without_retry() {
    let mut changed = device();
    changed["name"] = json!("New AP");
    let server = MockServer::start(vec![
        Reply::json(200, inventory(vec![device()])),
        Reply::json(503, b"unavailable".to_vec()),
        Reply::json(200, inventory(vec![changed])),
    ]);
    let client = make_client(&server, Duration::from_secs(2));
    let prepared = plan_update(
        &client,
        SITE,
        DEVICE_ID,
        ConfigChange::Name("New AP".into()),
    )
    .await
    .expect("rename should prepare");

    let report = apply_once(&prepared.backend, &prepared.plan, Duration::from_secs(1))
        .await
        .expect("failed request still has a mutation report");
    assert_eq!(report.outcome, Outcome::RequestFailedStateMatches);
    assert_eq!(report.error_kind(), Some(ErrorKind::General));
    assert_eq!(report.observed, Some(json!("New AP")));
    let requests = server.finish();
    assert_eq!(requests.len(), 3);
    assert_inventory_get(&requests[0]);
    assert_write_path(&requests[1]);
    assert_inventory_get(&requests[2]);
}

#[tokio::test(start_paused = true)]
async fn missing_owned_name_in_readback_does_not_verify_the_update() {
    let _clock = keep_clock_paused().await;
    let mut missing_name = device();
    missing_name
        .as_object_mut()
        .expect("device object")
        .remove("name");
    let server = MockServer::start(vec![
        Reply::json(200, inventory(vec![device()])),
        Reply::json(200, br#"{"ok":true}"#.to_vec()),
        Reply::json(200, inventory(vec![missing_name])),
    ]);
    let client = make_client(&server, Duration::from_secs(2));
    let prepared = plan_update(
        &client,
        SITE,
        DEVICE_ID,
        ConfigChange::Name("New AP".into()),
    )
    .await
    .expect("rename should prepare");

    let report = apply_readback_once(
        &prepared.backend,
        &prepared.plan,
        Duration::from_millis(100),
    )
    .await
    .expect("missing owned state should produce an unverified report");
    assert_eq!(report.outcome, Outcome::Unverified);
    assert_eq!(report.error_kind(), Some(ErrorKind::Unverified));
    assert!(
        report
            .readback_error
            .as_ref()
            .is_some_and(|error| error.message.contains("name is missing"))
    );
    let requests = server.finish();
    assert_eq!(requests.len(), 3);
    assert_inventory_get(&requests[0]);
    assert_write_path(&requests[1]);
    assert_inventory_get(&requests[2]);
}

#[tokio::test]
async fn reboot_is_one_action_and_verifies_offline_to_online_transition() {
    let mut without_baseline_uptime = device();
    without_baseline_uptime["uptimeInSeconds"] = json!(null);
    let mut offline = device();
    offline["status"] = json!("down");
    offline["uptimeInSeconds"] = json!(null);
    let server = MockServer::start(vec![
        Reply::json(200, inventory(vec![without_baseline_uptime])),
        Reply::json(200, ack()),
        Reply::json(200, inventory(vec![offline])),
        Reply::json(
            200,
            inventory(vec![{
                let mut online = device();
                online["uptimeInSeconds"] = json!(null);
                online
                    .as_object_mut()
                    .expect("device object")
                    .remove("operationalState");
                online
            }]),
        ),
    ]);
    let client = make_client(&server, Duration::from_secs(2));
    let prepared = plan_reboot(&client, SITE, DEVICE_ID)
        .await
        .expect("reboot target should prepare");

    let report = apply_once(&prepared.backend, &prepared.plan, Duration::from_secs(2))
        .await
        .expect("reboot should report");
    assert_eq!(report.outcome, Outcome::Verified);
    assert!(
        report
            .observed
            .as_ref()
            .is_some_and(|state| { state.restart_observed && state.back_in_service })
    );
    let requests = server.finish();
    assert_eq!(requests.len(), 4);
    assert_inventory_get(&requests[0]);
    assert_eq!(requests[1].method, "POST");
    assert_eq!(
        requests[1].target,
        format!("/api/sites/{SITE}/inventory/{DEVICE_ID}?action=reboot")
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&requests[1].body).expect("reboot request body"),
        json!({"id":DEVICE_ID})
    );
    assert_inventory_get(&requests[2]);
    assert_inventory_get(&requests[3]);
}

#[tokio::test(start_paused = true)]
async fn reboot_requires_a_new_strict_uptime_reset_not_zero_or_unknown_values() {
    let _clock = keep_clock_paused().await;
    let cases = [
        (json!(0), json!(0)),
        (json!(3600), json!(3600)),
        (json!(3600), json!(3500)),
        (Value::Null, json!(0)),
        (json!(3600), Value::Null),
    ];
    for (baseline, fresh) in cases {
        let mut before = device();
        before["uptimeInSeconds"] = baseline;
        let mut after = device();
        after["uptimeInSeconds"] = fresh;
        let server = MockServer::start(vec![
            Reply::json(200, inventory(vec![before])),
            Reply::json(200, ack()),
            Reply::json(200, inventory(vec![after])),
        ]);
        let client = make_client(&server, Duration::from_secs(2));
        let prepared = plan_reboot(&client, SITE, DEVICE_ID)
            .await
            .expect("online reboot target should prepare");

        let report = apply_readback_once(
            &prepared.backend,
            &prepared.plan,
            Duration::from_millis(100),
        )
        .await
        .expect("unchanged status should remain unverified");
        assert_eq!(report.outcome, Outcome::Unverified);
        let requests = server.finish();
        assert_eq!(requests.len(), 3);
        assert_inventory_get(&requests[0]);
        assert_eq!(requests[1].method, "POST");
        assert_inventory_get(&requests[2]);
    }
}

#[tokio::test(start_paused = true)]
async fn reboot_accepts_a_strict_uptime_reset_only_after_elapsed_time() {
    let _clock = keep_clock_paused().await;
    let mut switch = device();
    switch["deviceType"] = json!("switch");
    switch
        .as_object_mut()
        .expect("switch object")
        .remove("deviceRole");
    let mut reset = device();
    reset["deviceType"] = json!("switch");
    reset
        .as_object_mut()
        .expect("switch object")
        .remove("deviceRole");
    reset["uptimeInSeconds"] = json!(0);
    let server = MockServer::start(vec![
        Reply::json(200, inventory(vec![switch])),
        Reply::json(200, ack()),
        Reply::json(200, inventory(vec![reset])),
    ]);
    let client = make_client(&server, Duration::from_secs(2));
    let prepared = plan_reboot(&client, SITE, DEVICE_ID)
        .await
        .expect("online reboot target should prepare");

    let report = apply_readback_after(
        &prepared.backend,
        &prepared.plan,
        Duration::from_secs(1),
        Duration::from_millis(250),
    )
    .await
    .expect("uptime reset should report");
    assert_eq!(report.outcome, Outcome::Verified);
    assert!(
        report
            .observed
            .as_ref()
            .is_some_and(|state| { state.restart_observed && state.back_in_service })
    );
    let requests = server.finish();
    assert_eq!(requests.len(), 3);
    assert_inventory_get(&requests[0]);
    assert_eq!(requests[1].method, "POST");
    assert_inventory_get(&requests[2]);
}

#[tokio::test]
async fn forget_verifies_only_complete_inventory_absence() {
    for entry in [device(), {
        let mut switch = device();
        switch["deviceType"] = json!("switch");
        switch
            .as_object_mut()
            .expect("switch object")
            .remove("deviceRole");
        switch
    }] {
        let server = MockServer::start(vec![
            Reply::json(200, inventory(vec![entry.clone()])),
            Reply::empty(204),
            Reply::json(200, inventory(vec![])),
        ]);
        let client = make_client(&server, Duration::from_secs(2));
        let prepared = plan_forget(&client, SITE, DEVICE_ID)
            .await
            .expect("supported AP or switch can be prepared for forget");
        let report = apply_once(&prepared.backend, &prepared.plan, Duration::from_secs(2))
            .await
            .expect("forget should report");

        assert_eq!(report.outcome, Outcome::Verified);
        let requests = server.finish();
        assert_eq!(requests.len(), 3);
        assert_inventory_get(&requests[0]);
        assert_eq!(requests[1].method, "DELETE");
        assert_eq!(
            requests[1].target,
            format!("/api/sites/{SITE}/inventory/{DEVICE_ID}")
        );
        assert!(requests[1].body.is_empty());
        assert_inventory_get(&requests[2]);
    }
}

#[tokio::test(start_paused = true)]
async fn forget_does_not_treat_a_partial_inventory_as_proof_of_absence() {
    let _clock = keep_clock_paused().await;
    let partial = serde_json::to_vec(&json!({
        "kind":"resourceList",
        "totalCount":1,
        "matchingFilterCount":1,
        "pendingAvailability":null,
        "elements":[]
    }))
    .expect("serialize incomplete inventory");
    let server = MockServer::start(vec![
        Reply::json(200, inventory(vec![device()])),
        Reply::empty(204),
        Reply::json(200, partial),
    ]);
    let client = make_client(&server, Duration::from_secs(2));
    let prepared = plan_forget(&client, SITE, DEVICE_ID)
        .await
        .expect("access point can be prepared for forget");
    let report = apply_readback_once(
        &prepared.backend,
        &prepared.plan,
        Duration::from_millis(100),
    )
    .await
    .expect("partial readback should not verify deletion");

    assert_eq!(report.outcome, Outcome::Unverified);
    let requests = server.finish();
    assert_eq!(requests.len(), 3);
    assert_inventory_get(&requests[0]);
    assert_eq!(requests[1].method, "DELETE");
    assert_inventory_get(&requests[2]);
}

#[tokio::test]
async fn device_mutation_plans_refuse_gateway_or_unknown_identity() {
    let mut gateway_type = device();
    gateway_type["deviceType"] = json!("gateway");
    // Keep the AP role so the deviceType check is the only invalid property.
    let mut gateway_role = device();
    gateway_role["deviceRole"] = json!("gateway");
    let mut unknown_type = device();
    unknown_type["deviceType"] = json!("futureType");
    let mut missing_type = device();
    missing_type
        .as_object_mut()
        .expect("device object")
        .remove("deviceType");
    let mut unknown_ap_role = device();
    unknown_ap_role["deviceRole"] = json!("futureRole");
    let mut missing_ap_role = device();
    missing_ap_role
        .as_object_mut()
        .expect("device object")
        .remove("deviceRole");

    for (entry, expected_kind) in [
        (gateway_type, ErrorKind::Unsupported),
        (gateway_role, ErrorKind::Unsupported),
        (unknown_type, ErrorKind::General),
        (missing_type, ErrorKind::General),
        (unknown_ap_role, ErrorKind::General),
        (missing_ap_role, ErrorKind::General),
    ] {
        let response = Reply::json(200, inventory(vec![entry]));
        let server = MockServer::start(vec![response.clone(), response.clone(), response]);
        let client = make_client(&server, Duration::from_secs(1));
        let results = [
            plan_forget(&client, SITE, DEVICE_ID).await.map(|_| ()),
            plan_reboot(&client, SITE, DEVICE_ID).await.map(|_| ()),
            plan_update(
                &client,
                SITE,
                DEVICE_ID,
                ConfigChange::ManagementIp(StaticManagementIp {
                    address: Ipv4Addr::new(192, 168, 10, 50),
                    prefix_length: 24,
                    gateway: Ipv4Addr::new(192, 168, 10, 1),
                    dns: Ipv4Addr::new(1, 1, 1, 1),
                    secondary_dns: None,
                }),
            )
            .await
            .map(|_| ()),
        ];

        for result in results {
            let error = match result {
                Ok(()) => panic!("unsafe or unknown device identity must be refused"),
                Err(error) => error,
            };
            assert_eq!(error.kind, expected_kind);
        }
        let requests = server.finish();
        assert_eq!(requests.len(), 3, "each plan reads inventory once only");
        for request in &requests {
            assert_inventory_get(request);
        }
    }
}

#[tokio::test(start_paused = true)]
async fn reboot_restart_observation_does_not_verify_transient_operational_states() {
    let _clock = keep_clock_paused().await;
    for operational_state in ["rebooting", "updating", "synchronizing"] {
        let mut before = device();
        before["uptimeInSeconds"] = json!(3600);
        let mut transient = device();
        transient["uptimeInSeconds"] = json!(0);
        transient["operationalState"] = json!(operational_state);
        let server = MockServer::start(vec![
            Reply::json(200, inventory(vec![before])),
            Reply::json(200, ack()),
            Reply::json(200, inventory(vec![transient])),
        ]);
        let client = make_client(&server, Duration::from_secs(2));
        let prepared = plan_reboot(&client, SITE, DEVICE_ID)
            .await
            .expect("online AP can be rebooted");

        let report = apply_readback_after(
            &prepared.backend,
            &prepared.plan,
            Duration::from_millis(500),
            Duration::from_millis(250),
        )
        .await
        .expect("transient state should produce a report");
        assert_eq!(report.outcome, Outcome::Unverified);
        assert!(
            report
                .observed
                .as_ref()
                .is_some_and(|state| { state.restart_observed && !state.back_in_service })
        );
        let requests = server.finish();
        assert_eq!(requests.len(), 3);
        assert_inventory_get(&requests[0]);
        assert_eq!(requests[1].method, "POST");
        assert_inventory_get(&requests[2]);
    }
}

#[tokio::test]
async fn device_detail_and_power_reads_use_selected_inventory_identity_and_never_write() {
    let expected_detail = json!({"id":DEVICE_ID,"config":{"ledMode":"led_on"}});
    let expected_power = json!({"watts":14.5});
    let details_server = MockServer::start(vec![
        Reply::json(200, inventory(vec![device()])),
        Reply::json(200, serde_json::to_vec(&expected_detail).unwrap()),
    ]);
    let details_client = make_client(&details_server, Duration::from_secs(1));
    assert_eq!(
        details(&details_client, SITE, DEVICE_NAME)
            .await
            .expect("device details should be returned"),
        expected_detail
    );
    let detail_requests = details_server.finish();
    assert_eq!(detail_requests.len(), 2);
    assert_inventory_get(&detail_requests[0]);
    assert_eq!(detail_requests[1].method, "GET");
    assert_eq!(
        detail_requests[1].target,
        format!("/api/sites/{SITE}/deviceDetails/{DEVICE_ID}")
    );

    let power_server = MockServer::start(vec![
        Reply::json(200, inventory(vec![device()])),
        Reply::json(200, serde_json::to_vec(&expected_power).unwrap()),
    ]);
    let power_client = make_client(&power_server, Duration::from_secs(1));
    assert_eq!(
        power_usage(&power_client, SITE, DEVICE_ID)
            .await
            .expect("power usage should be returned"),
        expected_power
    );
    let power_requests = power_server.finish();
    assert_eq!(power_requests.len(), 2);
    assert_inventory_get(&power_requests[0]);
    assert_eq!(power_requests[1].method, "GET");
    assert_eq!(
        power_requests[1].target,
        format!("/api/sites/{SITE}/inventory/{DEVICE_ID}/powerUsage")
    );
}
