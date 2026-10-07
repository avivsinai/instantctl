use super::*;
use crate::{
    Error, ErrorKind,
    client::access::{port_access_control, radius},
    mutation::{Outcome, apply_once},
    secret::SecretString,
};
use serde_json::{Value, json};
use std::time::Duration;

const SITE: &str = "123e4567-e89b-12d3-a456-426614174000";
const PROFILE_ID: &str = "radius-profile-a";
const PROFILE_NAME: &str = "Corp RADIUS";
const OLD_SECRET: &str = "old-secret-sentinel";
const NEW_SECRET: &str = "new-secret-sentinel";

fn reply_json(value: Value) -> Reply {
    Reply::json(
        200,
        serde_json::to_vec(&value).expect("serialize JSON reply"),
    )
}

fn rows(elements: Vec<Value>) -> Value {
    let count = elements.len() as u64;
    json!({
        "kind":"resourceList",
        "totalCount":count,
        "matchingFilterCount":count,
        "elements":elements
    })
}

fn radius_server(host: &str, secret: &str) -> Value {
    json!({
        "serverHost":host,"sharedSecret":secret,"timeout":5,"retryCount":3,
        "authPort":1812,"accountingPort":1813
    })
}

fn profile(id: &str, name: &str) -> Value {
    json!({
        "id":id,"name":name,"primaryServer":radius_server("192.0.2.10", OLD_SECRET),
        "secondaryServer":null,"enableSecondaryServer":false,"enableRadiusOverTls":false,
        "requireRadiusAuthentication":false,"enableRadiusAccounting":false,
        "serverTimeoutSeconds":5,"serverRetryCount":3,
        "radiusNasIpSettings":{"useNasIpAddress":false},
        "radiusNasIdentifierSettings":{"useNasIdentifier":false},
        "usedByNetworks":[],"usedByDevices":[],
        "vendorExtension":{"retain":["opaque",17]}
    })
}

fn radius_template() -> Value {
    json!({
        "id":"template-id","name":"Default RADIUS",
        "primaryServer":{"serverHost":"","sharedSecret":"","timeout":8,"retryCount":2,"authPort":1912,"accountingPort":1913},
        "secondaryServer":{"serverHost":"","sharedSecret":"","timeout":8,"retryCount":2,"authPort":1912,"accountingPort":1913},
        "enableSecondaryServer":false,"enableRadiusOverTls":false,
        "requireRadiusAuthentication":false,"enableRadiusAccounting":false,
        "serverTimeoutSeconds":9,"serverRetryCount":4,
        "radiusNasIpSettings":{"useNasIpAddress":false},
        "radiusNasIdentifierSettings":{"useNasIdentifier":false}
    })
}

fn radius_payload(elements: Vec<Value>) -> Value {
    json!({
        "kind":"resourceList","totalCount":elements.len(),
        "matchingFilterCount":elements.len(),"elements":elements,
        "metaData":{"maxElements":8,"defaultProfile":radius_template()}
    })
}

fn expect_error<T>(result: Result<T, Error>) -> Error {
    match result {
        Err(error) => error,
        Ok(_) => panic!("operation should fail"),
    }
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

fn secret_patch(value: &str) -> radius::ServerPatch {
    radius::ServerPatch {
        secret: Some(SecretString::new(value.to_owned())),
        ..radius::ServerPatch::default()
    }
}

#[tokio::test(start_paused = true)]
async fn create_uses_server_template_defaults_and_redacts_secret_from_public_values() {
    let _clock = keep_clock_paused().await;
    let created = json!({
        "id":"created-radius-id","name":"New RADIUS",
        "primaryServer":{"serverHost":"192.0.2.44","sharedSecret":NEW_SECRET,"timeout":8,"retryCount":2,"authPort":1912,"accountingPort":1913},
        "secondaryServer":{"serverHost":"","sharedSecret":"","timeout":8,"retryCount":2,"authPort":1912,"accountingPort":1913},
        "enableSecondaryServer":false,"enableRadiusOverTls":true,
        "requireRadiusAuthentication":false,"enableRadiusAccounting":true,
        "serverTimeoutSeconds":9,"serverRetryCount":4,
        "radiusNasIpSettings":{"useNasIpAddress":false},
        "radiusNasIdentifierSettings":{"useNasIdentifier":false}
    });
    let server = MockServer::start(vec![
        reply_json(radius_payload(vec![])),
        reply_json(json!({"id":"created-radius-id","name":"New RADIUS"})),
        reply_json(rows(vec![created])),
    ]);
    let api = make_client(&server, Duration::from_secs(1));
    let patch = radius::Patch {
        name: Some("New RADIUS".into()),
        primary: radius::ServerPatch {
            host: Some("192.0.2.44".into()),
            secret: Some(SecretString::new(NEW_SECRET)),
            ..radius::ServerPatch::default()
        },
        tls: Some(true),
        accounting: Some(true),
        ..radius::Patch::default()
    };
    assert!(!format!("{patch:?}").contains(NEW_SECRET));
    let (backend, plan) = radius::create(&api, SITE, patch).await.unwrap();

    assert!(format!("{plan:?}").contains("(redacted)"));
    assert!(!format!("{plan:?}").contains(NEW_SECRET));
    assert!(!serde_json::to_string(&plan).unwrap().contains(NEW_SECRET));
    assert!(!backend.target().to_string().contains(NEW_SECRET));
    let report = apply_readback_once(&backend, &plan, Duration::from_secs(1))
        .await
        .unwrap();
    assert_eq!(report.outcome, Outcome::Verified);
    assert!(!format!("{report:?}").contains(NEW_SECRET));
    assert!(!serde_json::to_string(&report).unwrap().contains(NEW_SECRET));

    let requests = server.finish();
    assert_eq!(requests.len(), 3);
    assert_request(
        &requests[0],
        "GET",
        &format!("/api/sites/{SITE}/radiusProfiles"),
    );
    assert_request(
        &requests[1],
        "POST",
        &format!("/api/sites/{SITE}/radiusProfiles"),
    );
    assert_request(
        &requests[2],
        "GET",
        &format!("/api/sites/{SITE}/radiusProfiles"),
    );
    let sent = request_json(&requests[1]);
    assert_eq!(
        sent["primaryServer"],
        json!({
            "serverHost":"192.0.2.44","sharedSecret":NEW_SECRET,"timeout":8,
            "retryCount":2,"authPort":1912,"accountingPort":1913
        })
    );
    assert_eq!(sent["serverTimeoutSeconds"], 9);
    assert_eq!(sent["serverRetryCount"], 4);
    assert_eq!(sent["primaryServer"]["authPort"], 1912);
    assert_eq!(sent["primaryServer"]["accountingPort"], 1913);
    assert_eq!(
        sent["radiusNasIpSettings"],
        json!({"useNasIpAddress":false})
    );
    assert_eq!(
        sent["radiusNasIdentifierSettings"],
        json!({"useNasIdentifier":false})
    );
}

#[tokio::test(start_paused = true)]
async fn update_puts_full_profile_preserves_opaque_fields_and_rotates_both_secrets() {
    let _clock = keep_clock_paused().await;
    let mut before = profile(PROFILE_ID, PROFILE_NAME);
    before["primaryServer"]["serverExtension"] = json!({"keep":true});
    before["secondaryServer"] = radius_server("192.0.2.11", OLD_SECRET);
    before["enableSecondaryServer"] = json!(false);
    let mut after = before.clone();
    after["primaryServer"]["sharedSecret"] = json!(NEW_SECRET);
    after["secondaryServer"]["sharedSecret"] = json!("secondary-secret-sentinel");
    after["enableSecondaryServer"] = json!(true);
    after["radiusNasIpSettings"] = json!({"useNasIpAddress":true,"nasIpAddress":"192.0.2.8"});
    let server = MockServer::start(vec![
        reply_json(json!({"capabilities":["radius-nas-ip-address"]})),
        reply_json(radius_payload(vec![before.clone()])),
        reply_json(json!({"id":PROFILE_ID})),
        reply_json(radius_payload(vec![after])),
    ]);
    let api = make_client(&server, Duration::from_secs(1));
    let patch = radius::Patch {
        primary: secret_patch(NEW_SECRET),
        secondary: secret_patch("secondary-secret-sentinel"),
        secondary_enabled: Some(true),
        nas_ip: Some("192.0.2.8".parse().unwrap()),
        ..radius::Patch::default()
    };
    let (backend, plan) = radius::update(&api, SITE, PROFILE_ID, patch).await.unwrap();
    let report = apply_readback_once(&backend, &plan, Duration::from_secs(1))
        .await
        .unwrap();
    assert_eq!(report.outcome, Outcome::Verified);
    assert!(!format!("{plan:?} {report:?}").contains(NEW_SECRET));

    let requests = server.finish();
    assert_eq!(requests.len(), 4);
    assert_request(
        &requests[0],
        "GET",
        &format!("/api/sites/{SITE}/capabilities"),
    );
    assert_request(
        &requests[1],
        "GET",
        &format!("/api/sites/{SITE}/radiusProfiles"),
    );
    assert_request(
        &requests[2],
        "PUT",
        &format!("/api/sites/{SITE}/radiusProfiles/{PROFILE_ID}"),
    );
    assert_request(
        &requests[3],
        "GET",
        &format!("/api/sites/{SITE}/radiusProfiles"),
    );
    let sent = request_json(&requests[2]);
    assert_eq!(sent["primaryServer"]["sharedSecret"], NEW_SECRET);
    assert_eq!(
        sent["primaryServer"]["serverExtension"],
        json!({"keep":true})
    );
    assert_eq!(
        sent["secondaryServer"]["sharedSecret"],
        "secondary-secret-sentinel"
    );
    assert_eq!(sent["radiusNasIpSettings"]["nasIpAddress"], "192.0.2.8");
    assert_eq!(sent["vendorExtension"]["retain"], json!(["opaque", 17]));
}

#[tokio::test]
async fn invalid_primary_and_server_values_fail_before_http() {
    let cases = [
        radius::Patch {
            name: Some("No primary".into()),
            ..radius::Patch::default()
        },
        radius::Patch {
            name: Some("No secret".into()),
            primary: radius::ServerPatch {
                host: Some("192.0.2.1".into()),
                ..Default::default()
            },
            ..Default::default()
        },
        radius::Patch {
            name: Some("No host".into()),
            primary: radius::ServerPatch {
                secret: Some(SecretString::new("x")),
                ..Default::default()
            },
            ..Default::default()
        },
        radius::Patch {
            name: Some("Bad port".into()),
            primary: radius::ServerPatch {
                host: Some("192.0.2.1".into()),
                secret: Some(SecretString::new("x")),
                auth_port: Some(0),
                ..Default::default()
            },
            ..Default::default()
        },
        radius::Patch {
            name: Some("Bad timeout".into()),
            primary: radius::ServerPatch {
                host: Some("192.0.2.1".into()),
                secret: Some(SecretString::new("x")),
                timeout: Some(31),
                ..Default::default()
            },
            ..Default::default()
        },
        radius::Patch {
            name: Some("Bad secret".into()),
            primary: radius::ServerPatch {
                host: Some("192.0.2.1".into()),
                secret: Some(SecretString::new("bad\nsecret")),
                ..Default::default()
            },
            ..Default::default()
        },
        radius::Patch {
            name: Some("Long secret".into()),
            primary: radius::ServerPatch {
                host: Some("192.0.2.1".into()),
                secret: Some(SecretString::new("x".repeat(65))),
                ..Default::default()
            },
            ..Default::default()
        },
    ];
    for patch in cases {
        let server = MockServer::start(vec![]);
        let api = make_client(&server, Duration::from_secs(1));
        let error = expect_error(radius::create(&api, SITE, patch).await);
        assert_eq!(error.kind, ErrorKind::Usage);
        assert!(!error.to_string().contains("bad\nsecret"));
        assert_eq!(server.finish().len(), 0);
    }
}

#[tokio::test]
async fn invalid_ids_duplicate_ids_partial_pages_and_selectors_refuse_writes() {
    let bad_site = MockServer::start(vec![]);
    let api = make_client(&bad_site, Duration::from_secs(1));
    assert_eq!(
        expect_error(radius::list(&api, "bad-site").await).kind,
        ErrorKind::Config
    );
    assert_eq!(bad_site.finish().len(), 0);

    let duplicate = profile(PROFILE_ID, PROFILE_NAME);
    let server = MockServer::start(vec![reply_json(rows(vec![duplicate.clone(), duplicate]))]);
    let api = make_client(&server, Duration::from_secs(1));
    assert_eq!(
        expect_error(radius::list(&api, SITE).await).kind,
        ErrorKind::Unverified
    );
    assert_eq!(server.finish().len(), 1);

    let mut partial = rows(vec![profile(PROFILE_ID, PROFILE_NAME)]);
    partial["totalCount"] = json!(2);
    partial["matchingFilterCount"] = json!(2);
    let server = MockServer::start(vec![reply_json(partial)]);
    let api = make_client(&server, Duration::from_secs(1));
    assert_eq!(
        expect_error(radius::delete(&api, SITE, PROFILE_ID).await).kind,
        ErrorKind::Unverified
    );
    assert_eq!(server.finish().len(), 1);

    let server = MockServer::start(vec![reply_json(radius_payload(vec![
        profile("id-1", PROFILE_NAME),
        profile("id-2", PROFILE_NAME),
    ]))]);
    let api = make_client(&server, Duration::from_secs(1));
    let error = expect_error(
        radius::update(
            &api,
            SITE,
            PROFILE_NAME,
            radius::Patch {
                accounting: Some(true),
                ..Default::default()
            },
        )
        .await,
    );
    assert_eq!(error.kind, ErrorKind::Usage);
    assert!(error.to_string().contains("ambiguous"));
    assert_eq!(server.finish().len(), 1);

    let server = MockServer::start(vec![reply_json(radius_payload(vec![]))]);
    let api = make_client(&server, Duration::from_secs(1));
    assert_eq!(
        expect_error(
            radius::update(
                &api,
                SITE,
                "missing",
                radius::Patch {
                    accounting: Some(true),
                    ..Default::default()
                }
            )
            .await
        )
        .kind,
        ErrorKind::NotFound
    );
    assert_eq!(server.finish().len(), 1);
}

#[tokio::test]
async fn create_refuses_missing_template_duplicate_name_and_limit() {
    let patch = || radius::Patch {
        name: Some("New RADIUS".into()),
        primary: radius::ServerPatch {
            host: Some("192.0.2.20".into()),
            secret: Some(SecretString::new("secret")),
            ..Default::default()
        },
        ..Default::default()
    };
    for (payload, expected) in [
        (rows(vec![]), ErrorKind::Unverified),
        (
            radius_payload(vec![profile("existing", "New RADIUS")]),
            ErrorKind::Usage,
        ),
        (
            radius_payload(
                (0..8)
                    .map(|i| profile(&format!("id-{i}"), &format!("profile-{i}")))
                    .collect(),
            ),
            ErrorKind::Usage,
        ),
    ] {
        let server = MockServer::start(vec![reply_json(payload)]);
        let api = make_client(&server, Duration::from_secs(1));
        let error = expect_error(radius::create(&api, SITE, patch()).await);
        assert_eq!(error.kind, expected);
        assert_eq!(server.finish().len(), 1);
    }
}

#[tokio::test(start_paused = true)]
async fn create_bad_acknowledgments_and_failed_writes_are_single_attempt_and_secret_free() {
    let _clock = keep_clock_paused().await;
    let patch = || radius::Patch {
        name: Some("New RADIUS".into()),
        primary: radius::ServerPatch {
            host: Some("192.0.2.20".into()),
            secret: Some(SecretString::new(NEW_SECRET)),
            ..Default::default()
        },
        ..Default::default()
    };
    for ack in [json!({}), json!({"id":"existing-id"})] {
        let existing = if ack["id"] == "existing-id" {
            vec![profile("existing-id", "Old")]
        } else {
            vec![]
        };
        let server = MockServer::start(vec![
            reply_json(radius_payload(existing)),
            reply_json(ack),
            reply_json(radius_payload(vec![])),
        ]);
        let api = make_client(&server, Duration::from_secs(1));
        let (backend, plan) = radius::create(&api, SITE, patch()).await.unwrap();
        let report = apply_readback_once(&backend, &plan, Duration::from_millis(100))
            .await
            .unwrap();
        assert_eq!(report.outcome, Outcome::Failed);
        assert_eq!(report.error_kind(), Some(ErrorKind::Unverified));
        assert!(!format!("{report:?}").contains(NEW_SECRET));
        assert_eq!(server.finish().len(), 3);
    }

    let server = MockServer::start(vec![
        reply_json(radius_payload(vec![])),
        Reply::json(
            401,
            b"rejected secret sentinel: new-secret-sentinel".to_vec(),
        ),
        reply_json(radius_payload(vec![])),
    ]);
    let api = make_client(&server, Duration::from_secs(1));
    let (backend, plan) = radius::create(&api, SITE, patch()).await.unwrap();
    let report = apply_readback_once(&backend, &plan, Duration::from_millis(80))
        .await
        .unwrap();
    assert_eq!(report.outcome, Outcome::Failed);
    let error = report
        .request_error
        .expect("failed POST should be captured");
    assert!(!error.to_string().contains(NEW_SECRET));
    assert!(!format!("{error:?}").contains(NEW_SECRET));
    let requests = server.finish();
    assert_eq!(requests.len(), 3);
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.method == "POST")
            .count(),
        1
    );
}

#[tokio::test(start_paused = true)]
async fn delete_refuses_unknown_or_nonempty_references_and_verifies_successful_absence() {
    let _clock = keep_clock_paused().await;
    for row in [
        {
            let mut value = profile(PROFILE_ID, PROFILE_NAME);
            value.as_object_mut().unwrap().remove("usedByDevices");
            value
        },
        {
            let mut value = profile(PROFILE_ID, PROFILE_NAME);
            value["usedByNetworks"] = json!([{"name":"Guest SSID"}]);
            value
        },
        {
            let mut value = profile(PROFILE_ID, PROFILE_NAME);
            value["usedByDevices"] = json!([{"name":"Switch port 4"}]);
            value
        },
    ] {
        let server = MockServer::start(vec![reply_json(rows(vec![row]))]);
        let api = make_client(&server, Duration::from_secs(1));
        let error = expect_error(radius::delete(&api, SITE, PROFILE_ID).await);
        assert!(matches!(
            error.kind,
            ErrorKind::Unverified | ErrorKind::Usage
        ));
        assert_eq!(
            server.finish().len(),
            1,
            "unsafe deletion must send no DELETE"
        );
    }

    let before = profile(PROFILE_ID, PROFILE_NAME);
    let server = MockServer::start(vec![
        reply_json(radius_payload(vec![before.clone()])),
        Reply::json(200, format!(r#"{{"id":"{PROFILE_ID}"}}"#).into_bytes()),
        reply_json(radius_payload(vec![])),
    ]);
    let api = make_client(&server, Duration::from_secs(1));
    let (backend, plan) = radius::delete(&api, SITE, PROFILE_ID).await.unwrap();
    let report = apply_readback_once(&backend, &plan, Duration::from_secs(1))
        .await
        .unwrap();
    assert_eq!(report.outcome, Outcome::Verified);
    let requests = server.finish();
    assert_eq!(requests.len(), 3);
    assert_request(
        &requests[0],
        "GET",
        &format!("/api/sites/{SITE}/radiusProfiles"),
    );
    assert_request(
        &requests[1],
        "DELETE",
        &format!("/api/sites/{SITE}/radiusProfiles/{PROFILE_ID}"),
    );
    assert_request(
        &requests[2],
        "GET",
        &format!("/api/sites/{SITE}/radiusProfiles"),
    );
}

#[tokio::test(start_paused = true)]
async fn masked_secret_readback_does_not_claim_verified_secret_rotation() {
    let _clock = keep_clock_paused().await;
    let before = profile(PROFILE_ID, PROFILE_NAME);
    let mut masked = before.clone();
    masked["primaryServer"]["sharedSecret"] = json!("********");
    let after = masked.clone();
    let server = MockServer::start(vec![
        reply_json(radius_payload(vec![before.clone()])),
        reply_json(json!({"id":PROFILE_ID})),
        reply_json(radius_payload(vec![after])),
    ]);
    let api = make_client(&server, Duration::from_secs(1));
    let (backend, plan) = radius::update(
        &api,
        SITE,
        PROFILE_ID,
        radius::Patch {
            primary: secret_patch(NEW_SECRET),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let report = apply_readback_once(&backend, &plan, Duration::from_millis(100))
        .await
        .unwrap();
    assert_eq!(report.outcome, Outcome::Unverified);
    assert_eq!(report.error_kind(), Some(ErrorKind::Unverified));
    assert!(!format!("{plan:?} {report:?}").contains(NEW_SECRET));
    let requests = server.finish();
    assert_eq!(requests.len(), 3, "masked mismatch must not retry the PUT");
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.method == "PUT")
            .count(),
        1
    );
}

#[tokio::test]
async fn radius_profile_debug_details_and_safe_summary_never_show_secret() {
    let mut row = profile(PROFILE_ID, PROFILE_NAME);
    row["primaryServer"]["sharedSecret"] = json!(OLD_SECRET);
    row["secondaryServer"] = radius_server("192.0.2.11", "secondary-secret-sentinel");
    let server = MockServer::start(vec![reply_json(radius_payload(vec![row]))]);
    let api = make_client(&server, Duration::from_secs(1));
    let list = radius::list(&api, SITE).await.unwrap();
    let detail = radius::show(&list, PROFILE_ID).unwrap();
    assert_eq!(
        detail.pointer("/primaryServer/sharedSecret"),
        Some(&json!("(redacted)"))
    );
    assert_eq!(
        detail.pointer("/secondaryServer/sharedSecret"),
        Some(&json!("(redacted)"))
    );
    let summary = list[0].summary();
    assert!(summary.get("primary_host").is_some());
    assert!(summary.get("sharedSecret").is_none());
    assert!(!format!("{:?}", list[0]).contains(OLD_SECRET));
    assert!(!detail.to_string().contains("secondary-secret-sentinel"));
    assert_eq!(server.finish().len(), 1);
}

fn pac_settings() -> Value {
    json!({
        "kind":"portAccessControlSettings",
        "isRadiusAccountingEnabled":false,"isSecondaryRadiusServerEnabled":false,
        "radiusServerPrimary":radius_server("192.0.2.31", OLD_SECRET),
        "radiusServerSecondary":radius_server("192.0.2.32", "secondary-old-secret"),
        "vendorExtension":{"retain":true}
    })
}

#[tokio::test]
async fn pac_show_preserves_known_values_redacts_secrets_and_refuses_null_or_missing_booleans() {
    let server = MockServer::start(vec![reply_json(pac_settings())]);
    let api = make_client(&server, Duration::from_secs(1));
    let shown = port_access_control::show(&api, SITE).await.unwrap();
    assert_eq!(shown["isRadiusAccountingEnabled"], false);
    assert_eq!(shown["radiusServerPrimary"]["sharedSecret"], "(redacted)");
    assert!(!shown.to_string().contains(OLD_SECRET));
    assert_eq!(server.finish().len(), 1);

    let null_server = MockServer::start(vec![reply_json(Value::Null)]);
    let api = make_client(&null_server, Duration::from_secs(1));
    assert_eq!(
        expect_error(port_access_control::show(&api, SITE).await).kind,
        ErrorKind::Unverified
    );
    assert_eq!(null_server.finish().len(), 1);

    let server = MockServer::start(vec![reply_json(json!({
        "kind":"portAccessControlSettings"
    }))]);
    let api = make_client(&server, Duration::from_secs(1));
    let shown = port_access_control::show(&api, SITE).await.unwrap();
    assert_eq!(shown["isRadiusAccountingEnabled"], Value::Null);
    assert_eq!(shown["isSecondaryRadiusServerEnabled"], Value::Null);
    assert_eq!(server.finish().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn pac_update_preserves_full_object_rotates_primary_and_secondary_secrets_once() {
    let _clock = keep_clock_paused().await;
    let before = pac_settings();
    let mut after = before.clone();
    after["isRadiusAccountingEnabled"] = json!(true);
    after["isSecondaryRadiusServerEnabled"] = json!(true);
    after["radiusServerPrimary"]["sharedSecret"] = json!(NEW_SECRET);
    after["radiusServerSecondary"]["sharedSecret"] = json!("secondary-new-secret");
    let server = MockServer::start(vec![
        reply_json(before.clone()),
        reply_json(json!({"kind":"portAccessControlSettings"})),
        reply_json(after),
    ]);
    let api = make_client(&server, Duration::from_secs(1));
    let patch = port_access_control::Patch {
        accounting: Some(true),
        secondary_enabled: Some(true),
        primary: secret_patch(NEW_SECRET),
        secondary: secret_patch("secondary-new-secret"),
    };
    let (backend, plan) = port_access_control::update(&api, SITE, patch)
        .await
        .unwrap();
    assert!(!format!("{plan:?} {:?}", backend.target()).contains(NEW_SECRET));
    let report = apply_readback_once(&backend, &plan, Duration::from_secs(1))
        .await
        .unwrap();
    assert_eq!(report.outcome, Outcome::Verified);
    assert!(!format!("{report:?}").contains(NEW_SECRET));
    let requests = server.finish();
    assert_eq!(requests.len(), 3);
    assert_request(
        &requests[0],
        "GET",
        &format!("/api/sites/{SITE}/portAccessControlSettings"),
    );
    assert_request(
        &requests[1],
        "PUT",
        &format!("/api/sites/{SITE}/portAccessControlSettings"),
    );
    assert_request(
        &requests[2],
        "GET",
        &format!("/api/sites/{SITE}/portAccessControlSettings"),
    );
    let sent = request_json(&requests[1]);
    assert_eq!(sent["kind"], "portAccessControlSettings");
    assert_eq!(sent["radiusServerPrimary"]["sharedSecret"], NEW_SECRET);
    assert_eq!(
        sent["radiusServerSecondary"]["sharedSecret"],
        "secondary-new-secret"
    );
    assert_eq!(sent["vendorExtension"]["retain"], true);
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.method == "PUT")
            .count(),
        1
    );
}

#[tokio::test]
async fn pac_unknown_enabled_server_and_invalid_values_fail_before_http() {
    let cases = [
        (
            json!({"kind":"portAccessControlSettings","isRadiusAccountingEnabled":false,"isSecondaryRadiusServerEnabled":false}),
            port_access_control::Patch {
                secondary_enabled: Some(true),
                ..Default::default()
            },
            ErrorKind::Unverified,
            1,
        ),
        (
            pac_settings(),
            port_access_control::Patch {
                primary: radius::ServerPatch {
                    auth_port: Some(0),
                    ..Default::default()
                },
                ..Default::default()
            },
            ErrorKind::Usage,
            0,
        ),
        (
            {
                let mut body = pac_settings();
                body.as_object_mut()
                    .expect("PAC settings object")
                    .remove("radiusServerSecondary");
                body
            },
            port_access_control::Patch {
                secondary_enabled: Some(true),
                secondary: secret_patch("x"),
                ..Default::default()
            },
            ErrorKind::Unverified,
            1,
        ),
    ];
    for (body, patch, kind, expected_requests) in cases {
        let server = MockServer::start(vec![reply_json(body)]);
        let api = make_client(&server, Duration::from_secs(1));
        let error = expect_error(port_access_control::update(&api, SITE, patch).await);
        assert_eq!(error.kind, kind);
        assert_eq!(
            server.finish().len(),
            expected_requests,
            "invalid or incomplete state must not PUT"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn pac_false_patch_sends_the_full_object_and_missing_readback_stays_unverified() {
    let _clock = keep_clock_paused().await;
    let mut before = pac_settings();
    before["isRadiusAccountingEnabled"] = json!(true);
    let after = pac_settings();
    let server = MockServer::start(vec![
        reply_json(before),
        reply_json(json!({"kind":"portAccessControlSettings"})),
        reply_json(after),
    ]);
    let api = make_client(&server, Duration::from_secs(1));
    let (backend, plan) = port_access_control::update(
        &api,
        SITE,
        port_access_control::Patch {
            accounting: Some(false),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let report = apply_readback_once(&backend, &plan, Duration::from_secs(1))
        .await
        .unwrap();
    assert_eq!(report.outcome, Outcome::Verified);
    let requests = server.finish();
    assert_eq!(requests.len(), 3);
    assert_eq!(
        request_json(&requests[1])["isRadiusAccountingEnabled"],
        false
    );
    assert_eq!(
        request_json(&requests[1])["vendorExtension"]["retain"],
        true
    );

    let gate = Arc::new(ResponseGate::default());
    let server = MockServer::start(vec![
        reply_json(pac_settings()),
        reply_json(json!({"kind":"portAccessControlSettings"})),
        Reply::json(503, b"readback unavailable".to_vec()).gated(&gate),
    ]);
    let api = make_client(&server, Duration::from_secs(1));
    let (backend, plan) = port_access_control::update(
        &api,
        SITE,
        port_access_control::Patch {
            accounting: Some(true),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    // Expire only after the actual readback request reaches the socket. Holding
    // its response also prevents the GET retry timer from racing the deadline.
    let apply = apply_once(&backend, &plan, Duration::from_millis(80));
    tokio::pin!(apply);
    tokio::select! {
        biased;
        result = &mut apply => panic!("apply returned before the readback gate: {result:?}"),
        () = gate.reached.notified() => {}
    }
    tokio::time::advance(Duration::from_millis(81)).await;
    let report = apply.await.unwrap();
    gate.release();
    assert_eq!(report.outcome, Outcome::Unverified);
    assert_eq!(
        report.readback_error.as_ref().map(|error| error.kind),
        Some(ErrorKind::General)
    );
    let requests = server.finish();
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.method == "PUT")
            .count(),
        1
    );
}

#[tokio::test(start_paused = true)]
async fn pac_rejects_foreign_ack_and_reports_readback_mismatch_without_retry() {
    let _clock = keep_clock_paused().await;
    let mut mismatching = pac_settings();
    mismatching["isRadiusAccountingEnabled"] = json!(false);
    let server = MockServer::start(vec![
        reply_json(pac_settings()),
        reply_json(json!({"id":"foreign-id","kind":"portAccessControlSettings"})),
        reply_json(pac_settings()),
    ]);
    let api = make_client(&server, Duration::from_secs(1));
    let (backend, plan) = port_access_control::update(
        &api,
        SITE,
        port_access_control::Patch {
            accounting: Some(true),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let report = apply_readback_once(&backend, &plan, Duration::from_millis(80))
        .await
        .unwrap();
    assert_eq!(report.outcome, Outcome::Failed);
    assert_eq!(server.finish().len(), 3);

    let server = MockServer::start(vec![
        reply_json(pac_settings()),
        reply_json(json!({"kind":"portAccessControlSettings"})),
        reply_json(mismatching),
    ]);
    let api = make_client(&server, Duration::from_secs(1));
    let (backend, plan) = port_access_control::update(
        &api,
        SITE,
        port_access_control::Patch {
            accounting: Some(true),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let report = apply_readback_once(&backend, &plan, Duration::from_millis(80))
        .await
        .unwrap();
    assert_eq!(report.outcome, Outcome::Unverified);
    let requests = server.finish();
    assert_eq!(requests.len(), 3);
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.method == "PUT")
            .count(),
        1
    );
}

#[tokio::test]
async fn mask_shaped_secret_inputs_and_masked_full_object_fields_never_write() {
    for mask in ["********", "•", "(redacted)", "[redacted]"] {
        let mut before = profile(PROFILE_ID, PROFILE_NAME);
        before["primaryServer"]["sharedSecret"] = json!(mask);
        let server = MockServer::start(vec![reply_json(radius_payload(vec![before]))]);
        let api = make_client(&server, Duration::from_secs(1));
        let error = expect_error(
            radius::update(
                &api,
                SITE,
                PROFILE_ID,
                radius::Patch {
                    primary: secret_patch(mask),
                    ..Default::default()
                },
            )
            .await,
        );
        assert_eq!(error.kind, ErrorKind::Unverified);
        assert!(!format!("{error:?} {error}").contains(mask));
        assert_eq!(
            server.finish().len(),
            1,
            "a display mask cannot be sent or verified as a credential"
        );

        let server = MockServer::start(vec![reply_json(radius_payload(vec![]))]);
        let api = make_client(&server, Duration::from_secs(1));
        let error = expect_error(
            radius::create(
                &api,
                SITE,
                radius::Patch {
                    name: Some("Masked profile".into()),
                    primary: radius::ServerPatch {
                        host: Some("192.0.2.44".into()),
                        secret: Some(SecretString::new(mask)),
                        ..Default::default()
                    },
                    ..Default::default()
                },
            )
            .await,
        );
        assert_eq!(error.kind, ErrorKind::Unverified);
        assert_eq!(
            server.finish().len(),
            1,
            "creation cannot forward a mask-shaped secret"
        );
    }
    let mut before = profile(PROFILE_ID, PROFILE_NAME);
    before["primaryServer"]["sharedSecret"] = json!("********");
    let server = MockServer::start(vec![reply_json(radius_payload(vec![before]))]);
    let api = make_client(&server, Duration::from_secs(1));
    let error = expect_error(
        radius::update(
            &api,
            SITE,
            PROFILE_ID,
            radius::Patch {
                name: Some("Renamed".into()),
                ..Default::default()
            },
        )
        .await,
    );
    assert_eq!(error.kind, ErrorKind::Unverified);
    assert_eq!(
        server.finish().len(),
        1,
        "rename must not overwrite the underlying credential with its GET mask"
    );

    let mut before = pac_settings();
    before["radiusServerPrimary"]["sharedSecret"] = json!("********");
    for patch in [
        port_access_control::Patch {
            primary: secret_patch("********"),
            ..Default::default()
        },
        port_access_control::Patch {
            accounting: Some(true),
            ..Default::default()
        },
    ] {
        let server = MockServer::start(vec![reply_json(before.clone())]);
        let api = make_client(&server, Duration::from_secs(1));
        let error = expect_error(port_access_control::update(&api, SITE, patch).await);
        assert_eq!(error.kind, ErrorKind::Unverified);
        assert_eq!(server.finish().len(), 1, "PAC must not forward a GET mask");
    }
}

#[tokio::test(start_paused = true)]
async fn supplied_unmasked_secret_replaces_a_get_mask_and_requires_unmasked_readback() {
    let _clock = keep_clock_paused().await;
    let mut before = profile(PROFILE_ID, PROFILE_NAME);
    before["primaryServer"]["sharedSecret"] = json!("********");
    let mut after = before.clone();
    after["primaryServer"]["sharedSecret"] = json!(NEW_SECRET);
    let server = MockServer::start(vec![
        reply_json(radius_payload(vec![before])),
        reply_json(json!({"id":PROFILE_ID})),
        reply_json(radius_payload(vec![after])),
    ]);
    let api = make_client(&server, Duration::from_secs(1));
    let (backend, plan) = radius::update(
        &api,
        SITE,
        PROFILE_ID,
        radius::Patch {
            primary: secret_patch(NEW_SECRET),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let report = apply_readback_once(&backend, &plan, Duration::from_secs(1))
        .await
        .unwrap();
    assert_eq!(report.outcome, Outcome::Verified);
    let requests = server.finish();
    assert_eq!(requests.len(), 3);
    assert_eq!(
        request_json(&requests[1])["primaryServer"]["sharedSecret"],
        NEW_SECRET
    );
    assert!(!format!("{plan:?} {report:?}").contains(NEW_SECRET));

    let mut masked = pac_settings();
    masked["radiusServerPrimary"]["sharedSecret"] = json!("********");
    let server = MockServer::start(vec![
        reply_json(masked.clone()),
        reply_json(json!({"kind":"portAccessControlSettings"})),
        reply_json(masked),
    ]);
    let api = make_client(&server, Duration::from_secs(1));
    let (backend, plan) = port_access_control::update(
        &api,
        SITE,
        port_access_control::Patch {
            primary: secret_patch(NEW_SECRET),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let report = apply_readback_once(&backend, &plan, Duration::from_millis(80))
        .await
        .unwrap();
    assert_eq!(report.outcome, Outcome::Unverified);
    let observed = serde_json::to_value(report.observed).unwrap();
    assert_eq!(
        observed["configuration"]["radiusServerPrimary/sharedSecret"],
        Value::Null
    );
    let requests = server.finish();
    assert_eq!(requests.len(), 3);
    assert_eq!(
        request_json(&requests[1])["radiusServerPrimary"]["sharedSecret"],
        NEW_SECRET
    );
}
