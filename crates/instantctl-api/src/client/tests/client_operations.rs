use super::*;
use crate::{
    Error, ErrorKind,
    client::operations::{Change, ClientMutation, State, TagMode},
    mutation::{Outcome, apply_once},
};
use serde_json::{Value, json};
use std::{net::Ipv4Addr, time::Duration};

const SITE: &str = "123e4567-e89b-12d3-a456-426614174000";
const CLIENT_ID: &str = "client-1";
const MAC: &str = "aa:bb:cc:dd:ee:ff";
const OTHER_MAC: &str = "bb:cc:dd:ee:ff:00";
const NAME: &str = "Desk laptop";

fn client_summary(extra: Value) -> Value {
    let mut summary = json!({"id":CLIENT_ID,"macAddress":MAC,"name":NAME});
    summary
        .as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    summary
}

fn summaries(elements: Vec<Value>) -> Vec<u8> {
    serde_json::to_vec(&json!({"kind":"clientSummaries","elements":elements}))
        .expect("serialize client summaries")
}

fn summaries_with_scopes(elements: Vec<Value>, scopes: Vec<Value>) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "kind":"clientSummaries","elements":elements,
        "metaData":{"networkScopes":scopes}
    }))
    .expect("serialize summaries with network scopes")
}

fn blocked(elements: Vec<Value>) -> Vec<u8> {
    serde_json::to_vec(&json!({"kind":"blockedClients","elements":elements}))
        .expect("serialize blocked clients")
}

fn detail(name: &str) -> Value {
    json!({
        "kind":"clientDetails","id":CLIENT_ID,"macAddress":MAC,"name":name,
        "defaultName":"Default laptop","vendorExtension":{"opaque":[1,"keep",true]}
    })
}

fn request_json(request: &Request) -> Value {
    serde_json::from_slice(&request.body).expect("request body should be JSON")
}

fn expect_error<T>(result: Result<T, Error>) -> Error {
    match result {
        Err(error) => error,
        Ok(_) => panic!("operation planning should fail"),
    }
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
async fn planning_is_read_only_and_rename_preserves_the_full_details_object() {
    let before = detail(NAME);
    let mut after = before.clone();
    after["name"] = json!("Renamed laptop");
    let server = MockServer::start(vec![
        Reply::json(200, summaries(vec![client_summary(json!({}))])),
        Reply::json(200, serde_json::to_vec(&before).unwrap()),
        Reply::json(200, br#"{"kind":"clientDetails","id":"client-1"}"#.to_vec()),
        Reply::json(200, serde_json::to_vec(&after).unwrap()),
    ]);
    let client = make_client(&server, Duration::from_secs(1));
    let planned =
        ClientMutation::plan(&client, SITE, NAME, Change::Rename("Renamed laptop".into())).await;
    let (backend, plan) = planned.expect("rename should plan from client details");

    assert_eq!(backend.target().id, CLIENT_ID);
    assert_eq!(backend.target().mac, MAC);
    assert_eq!(backend.target().name.as_deref(), Some(NAME));
    assert_eq!(plan.current, State::Name { name: NAME.into() });
    assert_eq!(
        plan.desired,
        State::Name {
            name: "Renamed laptop".into()
        }
    );
    let report = apply_once(&backend, &plan, Duration::from_secs(1))
        .await
        .unwrap();
    assert_eq!(report.outcome, Outcome::Verified);

    let requests = server.finish();
    assert_eq!(requests.len(), 4);
    assert_request(
        &requests[0],
        "GET",
        &format!("/api/sites/{SITE}/clientSummary"),
    );
    assert_request(
        &requests[1],
        "GET",
        &format!("/api/sites/{SITE}/clientDetails/{CLIENT_ID}"),
    );
    assert_request(
        &requests[2],
        "PUT",
        &format!("/api/sites/{SITE}/clientDetails/{CLIENT_ID}"),
    );
    assert_eq!(
        request_json(&requests[2]),
        after,
        "full-object update must preserve fields outside name"
    );
    assert_eq!(requests[3].method, "GET");
    assert_eq!(requests[3].target, requests[1].target);
}

#[tokio::test]
async fn rename_rejects_changed_detail_identity_and_restores_default_with_an_empty_name() {
    for invalid in [
        json!({"kind":"clientDetails","id":"client-2","macAddress":MAC,"name":NAME,"defaultName":"Default laptop"}),
        json!({"kind":"clientDetails","id":CLIENT_ID,"macAddress":OTHER_MAC,"name":NAME,"defaultName":"Default laptop"}),
        json!({"kind":"clientDetails","id":CLIENT_ID,"macAddress":MAC,"defaultName":"Default laptop"}),
    ] {
        let server = MockServer::start(vec![
            Reply::json(200, summaries(vec![client_summary(json!({}))])),
            Reply::json(200, serde_json::to_vec(&invalid).unwrap()),
        ]);
        let client = make_client(&server, Duration::from_secs(1));
        let error = expect_error(
            ClientMutation::plan(&client, SITE, MAC, Change::Rename("new name".into())).await,
        );
        assert_eq!(error.kind, ErrorKind::Unverified);
        let requests = server.finish();
        assert_eq!(requests.len(), 2);
        assert!(requests.iter().all(|request| request.method == "GET"));
    }

    let mut default_details = detail("");
    default_details["defaultName"] = json!("Default laptop");
    let server = MockServer::start(vec![
        Reply::json(200, summaries(vec![client_summary(json!({}))])),
        Reply::json(200, serde_json::to_vec(&default_details).unwrap()),
        Reply::json(200, br#"{"kind":"clientDetails","id":"client-1"}"#.to_vec()),
        Reply::json(200, serde_json::to_vec(&default_details).unwrap()),
    ]);
    let client = make_client(&server, Duration::from_secs(1));
    let (backend, plan) =
        ClientMutation::plan(&client, SITE, MAC, Change::Rename("Default laptop".into()))
            .await
            .unwrap();
    assert_eq!(
        plan.current,
        State::Name {
            name: "Default laptop".into()
        }
    );
    assert_eq!(
        plan.desired,
        State::Name {
            name: "Default laptop".into()
        }
    );
    let report = apply_once(&backend, &plan, Duration::from_secs(1))
        .await
        .unwrap();
    assert_eq!(report.outcome, Outcome::Verified);
    let requests = server.finish();
    assert_eq!(requests.len(), 4);
    assert_eq!(request_json(&requests[2])["name"], "");

    let before = detail("Custom name");
    let after = detail("Default laptop");
    let server = MockServer::start(vec![
        Reply::json(200, summaries(vec![client_summary(json!({}))])),
        Reply::json(200, serde_json::to_vec(&before).unwrap()),
        Reply::empty(204),
        Reply::json(200, serde_json::to_vec(&after).unwrap()),
    ]);
    let client = make_client(&server, Duration::from_secs(1));
    let (backend, plan) = ClientMutation::plan(&client, SITE, MAC, Change::Rename(String::new()))
        .await
        .unwrap();
    assert_eq!(
        plan.desired,
        State::Name {
            name: "Default laptop".into()
        }
    );
    assert_eq!(
        apply_once(&backend, &plan, Duration::from_secs(1))
            .await
            .unwrap()
            .outcome,
        Outcome::Verified
    );
    let requests = server.finish();
    assert_eq!(request_json(&requests[2])["name"], "");
    assert_eq!(
        request_json(&requests[2])["vendorExtension"],
        before["vendorExtension"]
    );
}

#[tokio::test]
async fn rename_acknowledgment_for_another_client_remains_a_request_failure() {
    let before = detail(NAME);
    let after = detail("Renamed laptop");
    let server = MockServer::start(vec![
        Reply::json(200, summaries(vec![client_summary(json!({}))])),
        Reply::json(200, serde_json::to_vec(&before).unwrap()),
        Reply::json(200, br#"{"id":"another-client"}"#.to_vec()),
        Reply::json(200, serde_json::to_vec(&after).unwrap()),
    ]);
    let client = make_client(&server, Duration::from_secs(1));
    let (backend, plan) =
        ClientMutation::plan(&client, SITE, MAC, Change::Rename("Renamed laptop".into()))
            .await
            .unwrap();
    let report = apply_once(&backend, &plan, Duration::from_secs(1))
        .await
        .unwrap();
    assert_eq!(report.outcome, Outcome::RequestFailedStateMatches);
    assert_eq!(report.error_kind(), Some(ErrorKind::General));
    assert_eq!(
        server.finish().iter().filter(|r| r.method == "PUT").count(),
        1
    );
}

#[tokio::test]
async fn altered_plan_state_is_refused_without_a_put() {
    let current = detail(NAME);
    let server = MockServer::start(vec![
        Reply::json(200, summaries(vec![client_summary(json!({}))])),
        Reply::json(200, serde_json::to_vec(&current).unwrap()),
        Reply::json(200, serde_json::to_vec(&current).unwrap()),
    ]);
    let client = make_client(&server, Duration::from_secs(1));
    let (backend, mut plan) =
        ClientMutation::plan(&client, SITE, MAC, Change::Rename("Renamed laptop".into()))
            .await
            .unwrap();
    plan.desired = State::Name {
        name: "forged plan".into(),
    };
    let report = apply_once(&backend, &plan, Duration::from_secs(1))
        .await
        .unwrap();
    assert_eq!(report.outcome, Outcome::Failed);
    assert_eq!(report.error_kind(), Some(ErrorKind::Config));
    let requests = server.finish();
    assert!(requests.len() >= 3);
    assert!(requests.iter().all(|request| request.method == "GET"));
}

#[tokio::test]
async fn block_and_unblock_use_the_blocked_clients_collection_and_verify_readback() {
    let existing = json!({"id":"blocked-7","macAddress":MAC,"name":NAME});
    let block_server = MockServer::start(vec![
        Reply::json(
            200,
            summaries(vec![client_summary(json!({"isBlockable":true}))]),
        ),
        Reply::json(200, blocked(vec![])),
        Reply::empty(204),
        Reply::json(200, blocked(vec![existing.clone()])),
    ]);
    let client = make_client(&block_server, Duration::from_secs(1));
    let (backend, plan) = ClientMutation::plan(&client, SITE, MAC, Change::Block)
        .await
        .unwrap();
    assert_eq!(plan.current, State::Blocked { blocked: false });
    assert_eq!(plan.desired, State::Blocked { blocked: true });
    let report = apply_once(&backend, &plan, Duration::from_secs(1))
        .await
        .unwrap();
    assert_eq!(report.outcome, Outcome::Verified);
    let requests = block_server.finish();
    assert_eq!(requests.len(), 4);
    assert_request(
        &requests[0],
        "GET",
        &format!("/api/sites/{SITE}/clientSummary"),
    );
    assert_request(
        &requests[1],
        "GET",
        &format!("/api/sites/{SITE}/blockedClients"),
    );
    assert_request(
        &requests[2],
        "POST",
        &format!("/api/sites/{SITE}/blockedClients"),
    );
    assert_eq!(
        request_json(&requests[2]),
        json!({"kind":"blockedClients","macAddress":MAC})
    );
    assert_request(
        &requests[3],
        "GET",
        &format!("/api/sites/{SITE}/blockedClients"),
    );

    let unblock_server = MockServer::start(vec![
        Reply::json(200, blocked(vec![existing])),
        Reply::empty(204),
        Reply::json(200, blocked(vec![])),
    ]);
    let client = make_client(&unblock_server, Duration::from_secs(1));
    let (backend, plan) = ClientMutation::plan(&client, SITE, NAME, Change::Unblock)
        .await
        .unwrap();
    assert_eq!(plan.current, State::Blocked { blocked: true });
    assert_eq!(plan.desired, State::Blocked { blocked: false });
    let report = apply_once(&backend, &plan, Duration::from_secs(1))
        .await
        .unwrap();
    assert_eq!(report.outcome, Outcome::Verified);
    let requests = unblock_server.finish();
    assert_eq!(requests.len(), 3);
    assert_request(
        &requests[0],
        "GET",
        &format!("/api/sites/{SITE}/blockedClients"),
    );
    assert_request(
        &requests[1],
        "DELETE",
        &format!("/api/sites/{SITE}/blockedClients/blocked-7"),
    );
    assert_request(
        &requests[2],
        "GET",
        &format!("/api/sites/{SITE}/blockedClients"),
    );
}

#[tokio::test]
async fn unblock_refuses_a_partial_membership_list() {
    let server = MockServer::start(vec![Reply::json(
        200,
        br#"{"kind":"blockedClients","elements":[],"metaData":{"hasMore":true}}"#.to_vec(),
    )]);
    let client = make_client(&server, Duration::from_secs(1));
    let error = expect_error(ClientMutation::plan(&client, SITE, MAC, Change::Unblock).await);
    assert_eq!(error.kind, ErrorKind::Unverified);
    let requests = server.finish();
    assert_eq!(requests.len(), 1);
    assert_request(
        &requests[0],
        "GET",
        &format!("/api/sites/{SITE}/blockedClients"),
    );
}

#[tokio::test]
async fn watchlist_action_is_read_back_from_the_summary() {
    let server = MockServer::start(vec![
        Reply::json(
            200,
            summaries(vec![client_summary(
                json!({"isWatchlisted":false,"isWatchable":true}),
            )]),
        ),
        Reply::empty(204),
        Reply::json(
            200,
            summaries(vec![client_summary(
                json!({"isWatchlisted":true,"isWatchable":true}),
            )]),
        ),
    ]);
    let client = make_client(&server, Duration::from_secs(1));
    let (backend, plan) = ClientMutation::plan(&client, SITE, NAME, Change::Watchlist(true))
        .await
        .unwrap();
    assert_eq!(plan.current, State::Watchlisted { watchlisted: false });
    assert_eq!(plan.desired, State::Watchlisted { watchlisted: true });
    assert_eq!(
        apply_once(&backend, &plan, Duration::from_secs(1))
            .await
            .unwrap()
            .outcome,
        Outcome::Verified
    );

    let requests = server.finish();
    assert_eq!(requests.len(), 3);
    assert_request(
        &requests[0],
        "GET",
        &format!("/api/sites/{SITE}/clientSummary"),
    );
    assert_request(
        &requests[1],
        "POST",
        &format!("/api/sites/{SITE}/clientDetails/{CLIENT_ID}?action=addToWatchlist"),
    );
    assert_eq!(request_json(&requests[1]), json!({}));
    assert_request(
        &requests[2],
        "GET",
        &format!("/api/sites/{SITE}/clientSummary"),
    );
}

#[tokio::test(start_paused = true)]
async fn removing_watchlist_membership_requires_explicit_false_readback() {
    let _clock = keep_clock_paused().await;
    for (after, expected) in [
        (
            json!({"isWatchlisted":false,"isWatchable":true}),
            Outcome::Verified,
        ),
        (json!({"isWatchable":true}), Outcome::Unverified),
    ] {
        let server = MockServer::start(vec![
            Reply::json(
                200,
                summaries(vec![client_summary(
                    json!({"isWatchlisted":true,"isWatchable":true}),
                )]),
            ),
            Reply::empty(204),
            Reply::json(200, summaries(vec![client_summary(after)])),
        ]);
        let client = make_client(&server, Duration::from_secs(1));
        let (backend, plan) = ClientMutation::plan(&client, SITE, MAC, Change::Watchlist(false))
            .await
            .unwrap();
        let report = apply_readback_once(&backend, &plan, Duration::from_millis(100))
            .await
            .unwrap();
        assert_eq!(report.outcome, expected);
        let requests = server.finish();
        assert_eq!(requests.len(), 3);
        assert_request(
            &requests[1],
            "POST",
            &format!("/api/sites/{SITE}/clientDetails/{CLIENT_ID}?action=removeFromWatchlist"),
        );
        assert_eq!(request_json(&requests[1]), json!({}));
    }
}

#[tokio::test]
async fn watchlist_requires_known_capability_and_current_state() {
    for (value, expected) in [
        (
            json!({"isWatchable":false,"isWatchlisted":false}),
            ErrorKind::Unsupported,
        ),
        (json!({"isWatchable":true}), ErrorKind::Unverified),
    ] {
        let server = MockServer::start(vec![Reply::json(
            200,
            summaries(vec![client_summary(value)]),
        )]);
        let client = make_client(&server, Duration::from_secs(1));
        let error =
            expect_error(ClientMutation::plan(&client, SITE, MAC, Change::Watchlist(true)).await);
        assert_eq!(error.kind, expected);
        assert_eq!(server.finish().len(), 1);
    }
}

#[tokio::test(start_paused = true)]
async fn power_cycle_requires_one_eligible_port_and_never_claims_success_without_readback() {
    let _clock = keep_clock_paused().await;
    let eligible = json!({"connectedToPorts":[{"deviceId":"switch-1","portNumber":7,
        "isPoweredByPort":true,"isPowerCyclable":true,"isPowerCycling":false}]});
    let cycled = json!({"connectedToPorts":[{"deviceId":"switch-1","portNumber":7,
        "isPoweredByPort":true,"isPowerCyclable":true,"isPowerCycling":true}]});
    let server = MockServer::start(vec![
        Reply::json(200, summaries(vec![client_summary(eligible.clone())])),
        Reply::empty(204),
        Reply::json(200, summaries(vec![client_summary(cycled)])),
    ]);
    let client = make_client(&server, Duration::from_secs(1));
    let (backend, plan) = ClientMutation::plan(&client, SITE, MAC, Change::PowerCycle)
        .await
        .unwrap();
    assert_eq!(
        plan.current,
        State::PowerCycling {
            is_power_cycling: false
        }
    );
    assert_eq!(
        plan.desired,
        State::PowerCycling {
            is_power_cycling: true
        }
    );
    assert_eq!(
        apply_once(&backend, &plan, Duration::from_secs(1))
            .await
            .unwrap()
            .outcome,
        Outcome::Verified
    );
    let requests = server.finish();
    assert_eq!(requests.len(), 3);
    assert_request(
        &requests[1],
        "POST",
        &format!("/api/sites/{SITE}/clientDetails/{CLIENT_ID}?action=powerCycle"),
    );
    assert_eq!(request_json(&requests[1]), json!({}));

    let unchanged = MockServer::start(vec![
        Reply::json(200, summaries(vec![client_summary(eligible.clone())])),
        Reply::empty(204),
        Reply::json(200, summaries(vec![client_summary(eligible)])),
    ]);
    let client = make_client(&unchanged, Duration::from_secs(1));
    let (backend, plan) = ClientMutation::plan(&client, SITE, MAC, Change::PowerCycle)
        .await
        .unwrap();
    let report = apply_readback_once(&backend, &plan, Duration::from_millis(100))
        .await
        .unwrap();
    assert_eq!(report.outcome, Outcome::Unverified);
    assert_eq!(report.error_kind(), Some(ErrorKind::Unverified));
    assert_eq!(
        report.observed,
        Some(State::PowerCycling {
            is_power_cycling: false
        })
    );
    assert_eq!(
        unchanged
            .finish()
            .iter()
            .filter(|request| request.method == "POST")
            .count(),
        1
    );

    let unknown_eligibility = client_summary(json!({"connectedToPorts":[{
        "deviceId":"switch-1","portNumber":7,"isPoweredByPort":true,"isPowerCycling":false
    }]}));
    let server = MockServer::start(vec![Reply::json(200, summaries(vec![unknown_eligibility]))]);
    let client = make_client(&server, Duration::from_secs(1));
    let error = expect_error(ClientMutation::plan(&client, SITE, MAC, Change::PowerCycle).await);
    assert_eq!(error.kind, ErrorKind::Unverified);
    assert_eq!(
        server.finish().len(),
        1,
        "unknown eligibility must not send an action"
    );
}

#[tokio::test]
async fn reservation_preserves_existing_scopes_and_rejects_conflicts_or_missing_scopes() {
    let scopes = vec![
        json!({"networkId":"network-a","isDhcpServer":true,"dhcpScope":{"ipReservations":[
            {"clientId":CLIENT_ID,"macAddress":MAC,"ipAddress":"192.0.2.10"},
            {"clientId":"other","macAddress":OTHER_MAC,"ipAddress":"192.0.2.11"}
        ]}}),
        json!({"networkId":"network-b","isDhcpServer":true,"dhcpScope":{"ipReservations":[]}}),
    ];
    let readback_scopes = vec![
        scopes[0].clone(),
        json!({"networkId":"network-b","isDhcpServer":true,"dhcpScope":{"ipReservations":[
        {"clientId":CLIENT_ID,"macAddress":MAC,"ipAddress":"198.51.100.22"}
        ]}}),
    ];
    let server = MockServer::start(vec![
        Reply::json(
            200,
            summaries_with_scopes(
                vec![client_summary(json!({"canReserveIpAddress":true}))],
                scopes,
            ),
        ),
        Reply::empty(204),
        Reply::json(
            200,
            summaries_with_scopes(
                vec![client_summary(json!({"canReserveIpAddress":true}))],
                readback_scopes,
            ),
        ),
    ]);
    let client = make_client(&server, Duration::from_secs(1));
    let change = Change::ReserveIp {
        network: "network-b".into(),
        ip: Ipv4Addr::new(198, 51, 100, 22),
    };
    let (backend, plan) = ClientMutation::plan(&client, SITE, MAC, change)
        .await
        .unwrap();
    assert_eq!(backend.target().ip, None);
    let State::Reservations { ip_reservations } = &plan.desired else {
        panic!("expected reservations state")
    };
    assert_eq!(
        ip_reservations.len(),
        2,
        "target reservations on other networks must be preserved"
    );
    assert_eq!(
        apply_once(&backend, &plan, Duration::from_secs(1))
            .await
            .unwrap()
            .outcome,
        Outcome::Verified
    );
    let requests = server.finish();
    assert_eq!(requests.len(), 3);
    assert_request(
        &requests[1],
        "POST",
        &format!("/api/sites/{SITE}/clientSummary/{CLIENT_ID}?action=reserveIp"),
    );
    assert_eq!(
        request_json(&requests[1]),
        json!({"ipReservations":[
            {"networkId":"network-a","ipAddress":"192.0.2.10"},
            {"networkId":"network-b","ipAddress":"198.51.100.22"}
        ]})
    );

    for (body, expected) in [
        (
            summaries_with_scopes(
                vec![client_summary(json!({"canReserveIpAddress":true}))],
                vec![
                    json!({"networkId":"network-b","isDhcpServer":true,"dhcpScope":{"ipReservations":[
                        {"clientId":"someone-else","macAddress":OTHER_MAC,"ipAddress":"198.51.100.22"}
                    ]}}),
                ],
            ),
            ErrorKind::Usage,
        ),
        (
            summaries(vec![client_summary(json!({"canReserveIpAddress":true}))]),
            ErrorKind::Unverified,
        ),
        (
            summaries_with_scopes(
                vec![client_summary(json!({"canReserveIpAddress":true}))],
                vec![
                    json!({"networkId":"network-b","isDhcpServer":true,"dhcpScope":{"ipReservations":[
                        {"clientId":"other-client","macAddress":MAC,"ipAddress":"192.0.2.30"}
                    ]}}),
                ],
            ),
            ErrorKind::Unverified,
        ),
        (
            summaries_with_scopes(
                vec![client_summary(json!({"canReserveIpAddress":true}))],
                vec![
                    json!({"networkId":"network-b","isDhcpServer":true,"dhcpScope":{"ipReservations":[
                        {"clientId":CLIENT_ID,"macAddress":OTHER_MAC,"ipAddress":"192.0.2.30"}
                    ]}}),
                ],
            ),
            ErrorKind::Unverified,
        ),
    ] {
        let server = MockServer::start(vec![Reply::json(200, body)]);
        let client = make_client(&server, Duration::from_secs(1));
        let error = expect_error(
            ClientMutation::plan(
                &client,
                SITE,
                MAC,
                Change::ReserveIp {
                    network: "network-b".into(),
                    ip: Ipv4Addr::new(198, 51, 100, 22),
                },
            )
            .await,
        );
        assert_eq!(error.kind, expected);
        assert_eq!(server.finish().len(), 1, "refusal must happen before POST");
    }
}

#[tokio::test(start_paused = true)]
async fn conflicting_reservation_readback_cannot_be_verified() {
    let _clock = keep_clock_paused().await;
    let summary = client_summary(json!({"canReserveIpAddress":true}));
    let server = MockServer::start(vec![
        Reply::json(
            200,
            summaries_with_scopes(
                vec![summary.clone()],
                vec![json!({
                    "networkId":"network-a","isDhcpServer":true,"dhcpScope":{"ipReservations":[]}
                })],
            ),
        ),
        Reply::empty(204),
        Reply::json(
            200,
            summaries_with_scopes(
                vec![summary],
                vec![json!({
                    "networkId":"network-a","isDhcpServer":true,"dhcpScope":{"ipReservations":[
                        {"clientId":CLIENT_ID,"macAddress":MAC,"ipAddress":"192.0.2.10"},
                        {"clientId":"other","macAddress":OTHER_MAC,"ipAddress":"192.0.2.10"}
                    ]}
                })],
            ),
        ),
    ]);
    let client = make_client(&server, Duration::from_secs(1));
    let (backend, plan) = ClientMutation::plan(
        &client,
        SITE,
        MAC,
        Change::ReserveIp {
            network: "network-a".into(),
            ip: Ipv4Addr::new(192, 0, 2, 10),
        },
    )
    .await
    .unwrap();
    let report = apply_readback_once(&backend, &plan, Duration::from_millis(100))
        .await
        .unwrap();
    assert_eq!(report.outcome, Outcome::Unverified);
    assert_eq!(report.error_kind(), Some(ErrorKind::Unverified));
    assert_eq!(
        server
            .finish()
            .iter()
            .filter(|r| r.method == "POST")
            .count(),
        1
    );
}

#[tokio::test]
async fn tags_keep_existing_ids_and_verify_names_after_add() {
    let initial = client_summary(json!({"classification":[{"id":"tag-1","str":"family"}]}));
    let after = client_summary(
        json!({"classification":[{"id":"tag-1","str":"family"},{"id":"tag-2","str":"work"}]}),
    );
    let server = MockServer::start(vec![
        Reply::json(200, summaries(vec![initial])),
        Reply::empty(204),
        Reply::json(200, summaries(vec![after])),
    ]);
    let client = make_client(&server, Duration::from_secs(1));
    let (backend, plan) = ClientMutation::plan(
        &client,
        SITE,
        MAC,
        Change::Tags {
            mode: TagMode::Add,
            tags: vec!["work".into()],
        },
    )
    .await
    .unwrap();
    assert_eq!(
        plan.current,
        State::Tags {
            tags: vec!["family".into()]
        }
    );
    assert_eq!(
        plan.desired,
        State::Tags {
            tags: vec!["family".into(), "work".into()]
        }
    );
    assert_eq!(
        apply_once(&backend, &plan, Duration::from_secs(1))
            .await
            .unwrap()
            .outcome,
        Outcome::Verified
    );
    let requests = server.finish();
    assert_eq!(requests.len(), 3);
    assert_request(
        &requests[1],
        "POST",
        &format!("/api/sites/{SITE}/clientClassifications?action=replaceCommonTags"),
    );
    assert_eq!(
        request_json(&requests[1]),
        json!({"clientIds":[CLIENT_ID],"tags":[{"id":"tag-1"},{"str":"work"}]})
    );
}

#[tokio::test]
async fn tag_set_replaces_existing_values_with_new_names() {
    let before = client_summary(json!({"classification":[{"id":"tag-old","str":"family"}]}));
    let after = client_summary(json!({"classification":[{"id":"tag-new","str":"work"}]}));
    let server = MockServer::start(vec![
        Reply::json(200, summaries(vec![before])),
        Reply::empty(204),
        Reply::json(200, summaries(vec![after])),
    ]);
    let client = make_client(&server, Duration::from_secs(1));
    let (backend, plan) = ClientMutation::plan(
        &client,
        SITE,
        MAC,
        Change::Tags {
            mode: TagMode::Set,
            tags: vec!["work".into()],
        },
    )
    .await
    .unwrap();
    assert_eq!(
        plan.desired,
        State::Tags {
            tags: vec!["work".into()]
        }
    );
    assert_eq!(
        apply_once(&backend, &plan, Duration::from_secs(1))
            .await
            .unwrap()
            .outcome,
        Outcome::Verified
    );
    let requests = server.finish();
    assert_eq!(
        request_json(&requests[1]),
        json!({"clientIds":[CLIENT_ID],"tags":[{"str":"work"}]})
    );
}

#[tokio::test(start_paused = true)]
async fn tag_removal_keeps_the_surviving_id_and_mismatching_readback_is_unverified() {
    let _clock = keep_clock_paused().await;
    let before = client_summary(json!({"classification":[
        {"id":"tag-1","str":"family"},{"id":"tag-2","str":"work"}
    ]}));
    let after = client_summary(json!({"classification":[{"id":"tag-2","str":"work"}]}));
    let server = MockServer::start(vec![
        Reply::json(200, summaries(vec![before.clone()])),
        Reply::empty(204),
        Reply::json(200, summaries(vec![after])),
    ]);
    let client = make_client(&server, Duration::from_secs(1));
    let (backend, plan) = ClientMutation::plan(
        &client,
        SITE,
        MAC,
        Change::Tags {
            mode: TagMode::Remove,
            tags: vec!["family".into()],
        },
    )
    .await
    .unwrap();
    assert_eq!(
        plan.desired,
        State::Tags {
            tags: vec!["work".into()]
        }
    );
    assert_eq!(
        apply_once(&backend, &plan, Duration::from_secs(1))
            .await
            .unwrap()
            .outcome,
        Outcome::Verified
    );
    let requests = server.finish();
    assert_eq!(
        request_json(&requests[1]),
        json!({"clientIds":[CLIENT_ID],"tags":[{"id":"tag-2"}]})
    );

    let unchanged = MockServer::start(vec![
        Reply::json(
            200,
            summaries(vec![client_summary(
                json!({"classification":[{"id":"tag-1","str":"family"}]}),
            )]),
        ),
        Reply::empty(204),
        Reply::json(
            200,
            summaries(vec![client_summary(
                json!({"classification":[{"id":"tag-1","str":"family"}]}),
            )]),
        ),
    ]);
    let client = make_client(&unchanged, Duration::from_secs(1));
    let (backend, plan) = ClientMutation::plan(
        &client,
        SITE,
        MAC,
        Change::Tags {
            mode: TagMode::Add,
            tags: vec!["work".into()],
        },
    )
    .await
    .unwrap();
    let report = apply_readback_once(&backend, &plan, Duration::from_millis(100))
        .await
        .unwrap();
    assert_eq!(report.outcome, Outcome::Unverified);
    assert_eq!(report.error_kind(), Some(ErrorKind::Unverified));
    let requests = unchanged.finish();
    assert_eq!(requests.len(), 3);
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.method == "POST")
            .count(),
        1
    );
}

#[tokio::test]
async fn ambiguous_missing_unknown_and_partial_blocked_reads_fail_closed_before_writing() {
    let duplicate = summaries(vec![
        client_summary(json!({"isBlockable":true})),
        json!({"id":"client-2","macAddress":OTHER_MAC,"name":NAME,"isBlockable":true}),
    ]);
    for (body, selector, expected) in [
        (duplicate, NAME, ErrorKind::Usage),
        (summaries(vec![]), MAC, ErrorKind::NotFound),
        (
            summaries(vec![client_summary(json!({}))]),
            MAC,
            ErrorKind::Unverified,
        ),
    ] {
        let server = MockServer::start(vec![Reply::json(200, body)]);
        let client = make_client(&server, Duration::from_secs(1));
        let error =
            expect_error(ClientMutation::plan(&client, SITE, selector, Change::Block).await);
        assert_eq!(error.kind, expected);
        assert_eq!(server.finish().len(), 1);
    }

    for incomplete in [
        json!({"kind":"blockedClients","elements":null}),
        json!({"kind":"blockedClients","elements":[],"metaData":{"hasMore":true}}),
    ] {
        let server = MockServer::start(vec![
            Reply::json(
                200,
                summaries(vec![client_summary(json!({"isBlockable":true}))]),
            ),
            Reply::json(200, serde_json::to_vec(&incomplete).unwrap()),
        ]);
        let client = make_client(&server, Duration::from_secs(1));
        let error = expect_error(ClientMutation::plan(&client, SITE, MAC, Change::Block).await);
        assert_eq!(error.kind, ErrorKind::Unverified);
        assert_eq!(
            server.finish().len(),
            2,
            "partial membership cannot be treated as absence"
        );
    }
}

#[tokio::test]
async fn failed_write_is_not_retried_even_when_readback_matches() {
    let before = client_summary(json!({"isWatchlisted":false,"isWatchable":true}));
    let after = client_summary(json!({"isWatchlisted":true,"isWatchable":true}));
    let server = MockServer::start(vec![
        Reply::json(200, summaries(vec![before])),
        Reply::json(503, b"unavailable".to_vec()),
        Reply::json(200, summaries(vec![after])),
    ]);
    let client = make_client(&server, Duration::from_secs(1));
    let (backend, plan) = ClientMutation::plan(&client, SITE, MAC, Change::Watchlist(true))
        .await
        .unwrap();
    let report = apply_once(&backend, &plan, Duration::from_secs(1))
        .await
        .unwrap();
    assert_eq!(report.outcome, Outcome::RequestFailedStateMatches);
    assert_eq!(report.error_kind(), Some(ErrorKind::General));
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
