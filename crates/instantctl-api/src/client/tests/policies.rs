use super::clock::{apply_readback_once, keep_clock_paused};
use super::*;
use crate::{
    Error,
    client::policies::{self, POLICIES_CAPABILITY, VISIBILITY_PERMISSION, Visibility},
    mutation::Outcome,
};
use serde_json::{Value, json};

const SITE: &str = "123e4567-e89b-12d3-a456-426614174000";
const VISIBILITY_KIND: &str = "applicationCategoryUsageConfiguration";

fn reply_json(value: Value) -> Reply {
    Reply::json(
        200,
        serde_json::to_vec(&value).expect("serialize JSON reply"),
    )
}

fn policy_payload(policies: Vec<Value>) -> Value {
    let count = policies.len() as u64;
    json!({
        "policies":policies,
        "isApiVersionIncompatible":false,
        "filteredPolicyIds":[],
        "totalCount":count,
        "matchingFilterCount":count
    })
}

fn caps(values: Vec<&str>) -> Reply {
    reply_json(json!({"capabilities":values}))
}

fn permissions(values: Vec<&str>) -> Reply {
    reply_json(json!({
        "permissions":values.into_iter().map(|permission| json!({"permission":permission})).collect::<Vec<_>>()
    }))
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

fn visibility(enabled: bool) -> Value {
    json!({
        "kind":VISIBILITY_KIND,
        "isApplicationCategorizationEnabled":enabled
    })
}

#[tokio::test]
async fn list_preserves_unknown_wire_values_and_show_selects_id_or_exact_name() {
    let first = json!({
        "id":"policy-1","name":"Guest",
        "policyType":"future-policy-type",
        "vendorConfig":{"futureMode":"vendor-only","nullable":null}
    });
    let second = json!({
        "id":"policy-2","name":"Guest",
        "policyType":"other-future-type",
        "vendorConfig":{"enabled":true}
    });
    let third = json!({"id":"policy-3","name":"IoT","policyType":"unknown-enum"});
    let server = MockServer::start(vec![
        caps(vec![POLICIES_CAPABILITY]),
        reply_json(policy_payload(vec![
            first.clone(),
            second.clone(),
            third.clone(),
        ])),
    ]);
    let client = make_client(&server, Duration::from_secs(1));

    let listed = policies::list(&client, SITE)
        .await
        .expect("complete policy list should parse");
    assert_eq!(listed, vec![first.clone(), second, third.clone()]);
    assert_eq!(policies::show(&listed, "policy-1").unwrap(), first);
    assert_eq!(policies::show(&listed, "IoT").unwrap(), third);
    let error = expect_error(policies::show(&listed, "Guest"));
    assert_eq!(error.kind, ErrorKind::Usage);
    assert!(error.message.contains("ambiguous"));

    let requests = server.finish();
    assert_eq!(requests.len(), 2);
    assert_request(
        &requests[0],
        "GET",
        &format!("/api/sites/{SITE}/capabilities"),
    );
    assert_request(&requests[1], "GET", &format!("/api/sites/{SITE}/policies"));
    assert!(requests[1].body.is_empty());
}

#[tokio::test]
async fn duplicate_or_partial_policy_list_is_unverified() {
    let duplicate = json!({"id":"same-id","name":"Policy"});
    let mut partial = policy_payload(vec![json!({"id":"policy-1","name":"Policy"})]);
    partial["totalCount"] = json!(2);
    partial["matchingFilterCount"] = json!(2);
    for payload in [
        policy_payload(vec![duplicate.clone(), duplicate]),
        partial,
        {
            let mut paged = policy_payload(vec![json!({"id":"policy-1","name":"Policy"})]);
            paged["metaData"] = json!({"nextPageToken":"next"});
            paged
        },
    ] {
        let server = MockServer::start(vec![caps(vec![POLICIES_CAPABILITY]), reply_json(payload)]);
        let client = make_client(&server, Duration::from_secs(1));
        let error = expect_error(policies::list(&client, SITE).await);
        assert_eq!(error.kind, ErrorKind::Unverified);
        let requests = server.finish();
        assert_eq!(requests.len(), 2);
        assert_request(
            &requests[0],
            "GET",
            &format!("/api/sites/{SITE}/capabilities"),
        );
        assert_request(&requests[1], "GET", &format!("/api/sites/{SITE}/policies"));
    }

    let server = MockServer::start(vec![caps(vec!["other-capability"])]);
    let client = make_client(&server, Duration::from_secs(1));
    let error = expect_error(policies::list(&client, SITE).await);
    assert_eq!(error.kind, ErrorKind::Unsupported);
    assert_eq!(server.finish().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn visibility_rechecks_support_and_puts_full_object_then_verifies_owned_state() {
    let _clock = keep_clock_paused().await;
    let mut before = visibility(false);
    before["id"] = json!("server-owned-id");
    before["futureEnum"] = json!("vendor-mode-v9");
    before["vendorConfig"] = json!({"preserve":[1,null,"opaque"],"nested":{"flag":true}});
    let mut sent = before.clone();
    sent["isApplicationCategorizationEnabled"] = json!(true);
    let mut readback = sent.clone();
    readback["futureEnum"] = json!("vendor-mode-v10");
    readback["vendorConfig"]["nested"]["flag"] = json!(false);
    let server = MockServer::start(vec![
        caps(vec![POLICIES_CAPABILITY]),
        permissions(vec![VISIBILITY_PERMISSION]),
        reply_json(policy_payload(vec![])),
        reply_json(before),
        caps(vec![POLICIES_CAPABILITY]),
        permissions(vec![VISIBILITY_PERMISSION]),
        reply_json(policy_payload(vec![])),
        reply_json(json!({"kind":VISIBILITY_KIND})),
        reply_json(readback),
    ]);
    let client = make_client(&server, Duration::from_secs(2));
    let (backend, plan) = policies::set_visibility(&client, SITE, true)
        .await
        .expect("supported visibility setting should prepare");
    assert_eq!(
        plan.current,
        Visibility {
            is_application_categorization_enabled: false
        }
    );
    assert_eq!(
        plan.desired,
        Visibility {
            is_application_categorization_enabled: true
        }
    );

    let report = apply_readback_once(&backend, &plan, Duration::from_secs(2))
        .await
        .expect("put and readback should complete");
    assert_eq!(report.outcome, Outcome::Verified);
    assert_eq!(
        report.observed,
        Some(Visibility {
            is_application_categorization_enabled: true
        })
    );
    assert_eq!(report.readback_attempts, 1);

    let requests = server.finish();
    assert_eq!(requests.len(), 9);
    assert_request(
        &requests[0],
        "GET",
        &format!("/api/sites/{SITE}/capabilities"),
    );
    assert_request(
        &requests[1],
        "GET",
        &format!("/api/sites/{SITE}/permissions"),
    );
    assert_request(&requests[2], "GET", &format!("/api/sites/{SITE}/policies"));
    assert_request(
        &requests[3],
        "GET",
        &format!("/api/sites/{SITE}/applicationCategoryUsageConfiguration"),
    );
    assert_request(
        &requests[4],
        "GET",
        &format!("/api/sites/{SITE}/capabilities"),
    );
    assert_request(
        &requests[5],
        "GET",
        &format!("/api/sites/{SITE}/permissions"),
    );
    assert_request(&requests[6], "GET", &format!("/api/sites/{SITE}/policies"));
    assert_request(
        &requests[7],
        "PUT",
        &format!("/api/sites/{SITE}/applicationCategoryUsageConfiguration"),
    );
    assert_request(
        &requests[8],
        "GET",
        &format!("/api/sites/{SITE}/applicationCategoryUsageConfiguration"),
    );
    assert_eq!(request_json(&requests[7]), sent);
}

#[tokio::test]
async fn malformed_or_unauthorized_capability_response_propagates() {
    let cases = [
        (
            reply_json(json!({"capabilities":"not-an-array"})),
            ErrorKind::Unverified,
        ),
        (
            Reply::json(401, br#"{"error":"unauthorized"}"#.to_vec()),
            ErrorKind::Auth,
        ),
    ];
    for (capability_reply, expected_kind) in cases {
        let server = MockServer::start(vec![capability_reply]);
        let client = make_client(&server, Duration::from_secs(1));
        let error = expect_error(policies::set_visibility(&client, SITE, true).await);
        assert_eq!(error.kind, expected_kind);
        let requests = server.finish();
        assert_eq!(requests.len(), 1);
        assert_request(
            &requests[0],
            "GET",
            &format!("/api/sites/{SITE}/capabilities"),
        );
    }
}

#[tokio::test(start_paused = true)]
async fn disappearing_permission_refuses_put_even_after_a_valid_plan() {
    let _clock = keep_clock_paused().await;
    let current = visibility(false);
    let server = MockServer::start(vec![
        caps(vec![POLICIES_CAPABILITY]),
        permissions(vec![VISIBILITY_PERMISSION]),
        reply_json(policy_payload(vec![])),
        reply_json(current.clone()),
        caps(vec![POLICIES_CAPABILITY]),
        reply_json(json!({"permissions":[]})),
        reply_json(current),
    ]);
    let client = make_client(&server, Duration::from_secs(1));
    let (backend, plan) = policies::set_visibility(&client, SITE, true)
        .await
        .expect("initial permission should permit planning");

    let report = apply_readback_once(&backend, &plan, Duration::from_secs(1))
        .await
        .expect("permission loss should be a failed write report");
    assert_eq!(report.outcome, Outcome::Failed);
    assert_eq!(report.error_kind(), Some(ErrorKind::Unsupported));
    assert_eq!(report.readback_attempts, 1);
    assert_eq!(
        report.observed,
        Some(Visibility {
            is_application_categorization_enabled: false
        })
    );

    let requests = server.finish();
    assert_eq!(requests.len(), 7);
    assert_request(
        &requests[0],
        "GET",
        &format!("/api/sites/{SITE}/capabilities"),
    );
    assert_request(
        &requests[1],
        "GET",
        &format!("/api/sites/{SITE}/permissions"),
    );
    assert_request(&requests[2], "GET", &format!("/api/sites/{SITE}/policies"));
    assert_request(
        &requests[3],
        "GET",
        &format!("/api/sites/{SITE}/applicationCategoryUsageConfiguration"),
    );
    assert_request(
        &requests[4],
        "GET",
        &format!("/api/sites/{SITE}/capabilities"),
    );
    assert_request(
        &requests[5],
        "GET",
        &format!("/api/sites/{SITE}/permissions"),
    );
    assert_request(
        &requests[6],
        "GET",
        &format!("/api/sites/{SITE}/applicationCategoryUsageConfiguration"),
    );
    assert!(requests.iter().all(|request| request.method != "PUT"));
}

#[tokio::test]
async fn missing_visibility_kind_or_boolean_refuses_before_put() {
    for body in [
        json!({"isApplicationCategorizationEnabled":false}),
        json!({"kind":VISIBILITY_KIND}),
    ] {
        let server = MockServer::start(vec![
            caps(vec![]),
            permissions(vec![VISIBILITY_PERMISSION]),
            reply_json(body),
        ]);
        let client = make_client(&server, Duration::from_secs(1));
        let error = expect_error(policies::set_visibility(&client, SITE, true).await);
        assert_eq!(error.kind, ErrorKind::Unverified);
        let requests = server.finish();
        assert_eq!(requests.len(), 3);
        assert_request(
            &requests[0],
            "GET",
            &format!("/api/sites/{SITE}/capabilities"),
        );
        assert_request(
            &requests[1],
            "GET",
            &format!("/api/sites/{SITE}/permissions"),
        );
        assert_request(
            &requests[2],
            "GET",
            &format!("/api/sites/{SITE}/applicationCategoryUsageConfiguration"),
        );
    }
}

#[tokio::test(start_paused = true)]
async fn visibility_without_policy_capability_skips_policy_read_and_still_verifies() {
    let _clock = keep_clock_paused().await;
    let before = visibility(false);
    let mut after = before.clone();
    after["isApplicationCategorizationEnabled"] = json!(true);
    let server = MockServer::start(vec![
        caps(vec!["other-capability"]),
        permissions(vec![VISIBILITY_PERMISSION]),
        reply_json(before),
        caps(vec!["other-capability"]),
        permissions(vec![VISIBILITY_PERMISSION]),
        reply_json(json!({"kind":VISIBILITY_KIND})),
        reply_json(after.clone()),
    ]);
    let client = make_client(&server, Duration::from_secs(2));
    let (backend, plan) = policies::set_visibility(&client, SITE, true)
        .await
        .expect("policy capability is optional for visibility");
    let report = apply_readback_once(&backend, &plan, Duration::from_secs(2))
        .await
        .expect("supported update should verify");
    assert_eq!(report.outcome, Outcome::Verified);

    let requests = server.finish();
    assert_eq!(requests.len(), 7);
    assert_request(
        &requests[0],
        "GET",
        &format!("/api/sites/{SITE}/capabilities"),
    );
    assert_request(
        &requests[1],
        "GET",
        &format!("/api/sites/{SITE}/permissions"),
    );
    assert_request(
        &requests[2],
        "GET",
        &format!("/api/sites/{SITE}/applicationCategoryUsageConfiguration"),
    );
    assert_request(
        &requests[3],
        "GET",
        &format!("/api/sites/{SITE}/capabilities"),
    );
    assert_request(
        &requests[4],
        "GET",
        &format!("/api/sites/{SITE}/permissions"),
    );
    assert_request(
        &requests[5],
        "PUT",
        &format!("/api/sites/{SITE}/applicationCategoryUsageConfiguration"),
    );
    assert_request(
        &requests[6],
        "GET",
        &format!("/api/sites/{SITE}/applicationCategoryUsageConfiguration"),
    );
    assert_eq!(request_json(&requests[5]), after);
}

#[tokio::test]
async fn missing_visibility_permission_refuses_before_policy_or_configuration_get() {
    let server = MockServer::start(vec![caps(vec![POLICIES_CAPABILITY]), permissions(vec![])]);
    let client = make_client(&server, Duration::from_secs(1));
    let error = expect_error(policies::set_visibility(&client, SITE, true).await);
    assert_eq!(error.kind, ErrorKind::Unsupported);
    let requests = server.finish();
    assert_eq!(requests.len(), 2);
    assert_request(
        &requests[0],
        "GET",
        &format!("/api/sites/{SITE}/capabilities"),
    );
    assert_request(
        &requests[1],
        "GET",
        &format!("/api/sites/{SITE}/permissions"),
    );
}

#[tokio::test(start_paused = true)]
async fn traffic_policy_added_after_plan_refuses_put() {
    let _clock = keep_clock_paused().await;
    let current = visibility(false);
    let server = MockServer::start(vec![
        caps(vec![POLICIES_CAPABILITY]),
        permissions(vec![VISIBILITY_PERMISSION]),
        reply_json(policy_payload(vec![])),
        reply_json(current.clone()),
        caps(vec![POLICIES_CAPABILITY]),
        permissions(vec![VISIBILITY_PERMISSION]),
        reply_json(policy_payload(vec![json!({
            "id":"traffic-policy","name":"Application visibility policy",
            "policyType":"traffic","isEnabled":false
        })])),
        reply_json(current),
    ]);
    let client = make_client(&server, Duration::from_secs(1));
    let (backend, plan) = policies::set_visibility(&client, SITE, true)
        .await
        .expect("no controlling policy during planning");
    let report = apply_readback_once(&backend, &plan, Duration::from_secs(1))
        .await
        .expect("new policy should be a failed write report");
    assert_eq!(report.outcome, Outcome::Failed);
    assert_eq!(report.error_kind(), Some(ErrorKind::Unsupported));

    let requests = server.finish();
    assert_eq!(requests.len(), 8);
    assert_request(
        &requests[0],
        "GET",
        &format!("/api/sites/{SITE}/capabilities"),
    );
    assert_request(
        &requests[1],
        "GET",
        &format!("/api/sites/{SITE}/permissions"),
    );
    assert_request(&requests[2], "GET", &format!("/api/sites/{SITE}/policies"));
    assert_request(
        &requests[3],
        "GET",
        &format!("/api/sites/{SITE}/applicationCategoryUsageConfiguration"),
    );
    assert_request(
        &requests[4],
        "GET",
        &format!("/api/sites/{SITE}/capabilities"),
    );
    assert_request(
        &requests[5],
        "GET",
        &format!("/api/sites/{SITE}/permissions"),
    );
    assert_request(&requests[6], "GET", &format!("/api/sites/{SITE}/policies"));
    assert_request(
        &requests[7],
        "GET",
        &format!("/api/sites/{SITE}/applicationCategoryUsageConfiguration"),
    );
    assert!(requests.iter().all(|request| request.method != "PUT"));
}

#[tokio::test]
async fn unknown_policy_type_refuses_visibility_change_but_remains_listable() {
    let unknown =
        json!({"id":"future-policy","name":"Future policy","policyType":"future-policy-type"});
    let server = MockServer::start(vec![
        caps(vec![POLICIES_CAPABILITY]),
        permissions(vec![VISIBILITY_PERMISSION]),
        reply_json(policy_payload(vec![unknown.clone()])),
    ]);
    let client = make_client(&server, Duration::from_secs(1));
    let error = expect_error(policies::set_visibility(&client, SITE, true).await);
    assert_eq!(error.kind, ErrorKind::Unverified);
    let requests = server.finish();
    assert_eq!(requests.len(), 3);
    assert_request(
        &requests[0],
        "GET",
        &format!("/api/sites/{SITE}/capabilities"),
    );
    assert_request(
        &requests[1],
        "GET",
        &format!("/api/sites/{SITE}/permissions"),
    );
    assert_request(&requests[2], "GET", &format!("/api/sites/{SITE}/policies"));

    let server = MockServer::start(vec![
        caps(vec![POLICIES_CAPABILITY]),
        reply_json(policy_payload(vec![unknown.clone()])),
    ]);
    let client = make_client(&server, Duration::from_secs(1));
    let listed = policies::list(&client, SITE)
        .await
        .expect("list should preserve unknown policy enum");
    assert_eq!(listed, vec![unknown]);
    assert_eq!(server.finish().len(), 2);
}
