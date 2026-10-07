use super::*;
use crate::{
    Error, ErrorKind,
    client::network::{
        CreatePortMembership, DhcpPatch, DnsMode, NetworkMutation, NetworkType, Patch,
    },
    mutation::{Outcome, apply_once},
};
use serde_json::{Value, json};
use std::time::Duration;

const SITE: &str = "123e4567-e89b-12d3-a456-426614174000";
const NETWORK_ID: &str = "wired-opaque-17";
const OTHER_ID: &str = "wired-opaque-22";
const NETWORK_NAME: &str = "Office LAN";

fn wired(id: &str, name: &str, vlan: u16) -> Value {
    json!({
        "id":id,"isWireless":false,"wiredNetworkName":name,"isEnabled":true,
        "type":"employee","vlanId":vlan,"isManagement":false,"isDeletable":true,
        "vlanIdCanBeChanged":true,"canDisableDhcpScope":true,"useDhcpScope":false,
        "shouldApplyNetworkSecurityProtections":false,"isAccessRestricted":false,
        "isInternetAllowed":true,"isIntraSubnetTrafficAllowed":true,
        "isSpecificDestinationsAllowed":false,"allowedDestinations":[],
        "isIpRoutingEnabled":false,"ipRoutingConfig":{"isStatic":false},
        "devicePortMappings":[{"deviceId":"switch-a","portMappings":[],"trunkMappings":[]}],
        "isGuestPortalEnabled":false,"isIgmpSnoopingEnabled":false,
        "qos":{"trafficPriority":"medium"},"vendorExtension":{"keep":[1,"opaque"]}
    })
}

fn dhcp_scope() -> Value {
    json!({
        "ipAddress":"192.0.2.1","network":"192.0.2.0","netmask":"255.255.255.0",
        "domainName":"lan.example","ipAddressRange":{"start":"192.0.2.20","end":"192.0.2.200"},
        "ipReservations":[{"id":"reservation-1","ipAddress":"192.0.2.30","macAddress":"02:00:00:00:00:01","name":"printer"}],
        "dns":{"kind":"dns","dnsServerAssignationMode":"automatic","automaticPrimaryDns":"192.0.2.1","automaticSecondaryDns":null,"telemetry":"preserve"},
        "scopeExtension":{"retain":true}
    })
}

fn collection(elements: Vec<Value>) -> Value {
    let count = elements.len() as u64;
    json!({"kind":"wiredNetworks","totalCount":count,"matchingFilterCount":count,"elements":elements})
}

fn collection_reply(elements: Vec<Value>) -> Reply {
    reply_json(collection(elements))
}

fn template_collection_reply(elements: Vec<Value>, template: Value) -> Reply {
    let mut payload = collection(elements);
    payload["metaData"] = json!({"defaultWiredNetwork":template});
    reply_json(payload)
}

fn template_reply(template: Value) -> Reply {
    template_collection_reply(vec![], template)
}

fn reply_json(value: Value) -> Reply {
    Reply::json(
        200,
        serde_json::to_vec(&value).expect("serialize mock response"),
    )
}

fn request_json(request: &Request) -> Value {
    serde_json::from_slice(&request.body).expect("request body should be JSON")
}

fn expect_error<T>(result: Result<T, Error>) -> Error {
    match result {
        Err(error) => error,
        Ok(_) => panic!("operation should fail"),
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

fn merged(mut base: Value, patch: Value) -> Value {
    base.as_object_mut()
        .unwrap()
        .extend(patch.as_object().unwrap().clone());
    base
}

fn assigned_network() -> Value {
    let mut network = wired(NETWORK_ID, NETWORK_NAME, 20);
    network["devicePortMappings"][0]["portMappings"] = json!([
        {"portNumber":1,"mapping":"tagged"},
        {"portNumber":2,"mapping":"untagged"}
    ]);
    network
}

fn default_port_memberships() -> Value {
    json!([
        {
            "deviceId":"test-switch",
            "portMappings":[
                {"portNumber":1,"mapping":"tagged","vendorTelemetry":{"link":"up"}},
                {"portNumber":2,"mapping":"forbidden","vendorPortField":"keep"},
                {"portNumber":3,"mapping":"untagged","isUplink":false}
            ],
            "trunkMappings":[
                {"trunkNumber":1,"mapping":"tagged","vendorTrunkField":7}
            ],
            "vendorDeviceField":{"retain":true}
        },
        {
            "deviceId":"test-ap",
            "portMappings":[{"portNumber":0,"mapping":"absent"}],
            "trunkMappings":[]
        }
    ])
}

fn service(mac: &str, kind: &str, network_id: &str, shared: bool, name: &str) -> Value {
    json!({
        "serviceType":kind,"macAddress":mac,"ipAddress":"192.0.2.50","vlanId":20,
        "name":name,"networkId":network_id,"networkName":"Guest Wi-Fi","isShared":shared,
        "serviceTags":[{"tag":"media","port":5353}],"portalMetadata":{"retain":true}
    })
}

fn with_services(mut network: Value, local: Vec<Value>, other: Vec<Value>) -> Value {
    network["isSharedServicesEnabled"] = json!(true);
    network["localAirgroupServices"] = json!(local);
    network["sharedAirgroupServices"] = json!(other);
    network
}

#[tokio::test]
async fn list_select_and_details_parse_wired_rows_and_redact_nested_secrets() {
    let mut row = wired(NETWORK_ID, NETWORK_NAME, 20);
    row["ipRoutingConfig"]["privateKey"] = json!("network-secret-sentinel");
    let server = MockServer::start(vec![collection_reply(vec![row])]);
    let api = make_client(&server, Duration::from_secs(1));

    let rows = crate::client::network::list(&api, SITE).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id(), NETWORK_ID);
    assert_eq!(rows[0].name(), Some(NETWORK_NAME));
    assert_eq!(rows[0].summary()["vlan_id"], 20);
    assert_eq!(
        rows[0].details()["ipRoutingConfig"]["privateKey"],
        "(redacted)"
    );
    assert!(!format!("{:?}", rows[0]).contains("network-secret-sentinel"));
    assert_request(
        &server.finish()[0],
        "GET",
        &format!("/api/sites/{SITE}/wiredNetworks"),
    );
}

#[tokio::test]
async fn shared_service_read_treats_null_as_empty_only_when_network_is_disabled() {
    let mut disabled_null = wired(NETWORK_ID, NETWORK_NAME, 20);
    disabled_null["isSharedServicesEnabled"] = json!(false);
    disabled_null["localAirgroupServices"] = Value::Null;
    disabled_null["sharedAirgroupServices"] = Value::Null;

    let mut disabled_with_service = disabled_null.clone();
    disabled_with_service["localAirgroupServices"] = json!([service(
        "aa:bb:cc:dd:ee:01",
        "airplay",
        NETWORK_ID,
        false,
        "Reported speaker"
    )]);

    let mut enabled_null = disabled_null.clone();
    enabled_null["isSharedServicesEnabled"] = json!(true);

    let mut missing_collection = disabled_null.clone();
    missing_collection
        .as_object_mut()
        .unwrap()
        .remove("sharedAirgroupServices");

    let mut malformed_collection = disabled_null.clone();
    malformed_collection["sharedAirgroupServices"] = json!({});

    let mut unknown_state = disabled_null.clone();
    unknown_state["isSharedServicesEnabled"] = Value::Null;

    for (name, network, expected) in [
        ("disabled null collections", disabled_null, Ok(json!([]))),
        (
            "disabled retains reported service",
            disabled_with_service,
            Ok(json!([{
                "mac":"aa:bb:cc:dd:ee:01", "name":"Reported speaker", "source":"local",
                "shared":false, "types":["airplay"], "network_id":NETWORK_ID, "network_name":"Guest Wi-Fi"
            }])),
        ),
        (
            "enabled null collections",
            enabled_null,
            Err(ErrorKind::Unverified),
        ),
        (
            "missing collection",
            missing_collection,
            Err(ErrorKind::Unverified),
        ),
        (
            "malformed collection",
            malformed_collection,
            Err(ErrorKind::Unverified),
        ),
        (
            "unknown enablement with null collections",
            unknown_state,
            Err(ErrorKind::Unverified),
        ),
    ] {
        let server = MockServer::start(vec![collection_reply(vec![network])]);
        let api = make_client(&server, Duration::from_secs(1));
        let networks = crate::client::network::list(&api, SITE)
            .await
            .unwrap_or_else(|error| panic!("{name}: network collection should parse: {error}"));
        let result = networks[0].shared_services();
        match expected {
            Err(kind) => assert_eq!(expect_error(result).kind, kind, "{name}"),
            Ok(rows) => assert_eq!(
                result.expect("disabled service collections are readable"),
                rows,
                "{name}"
            ),
        }
        let requests = server.finish();
        assert_eq!(requests.len(), 1, "{name}");
        assert_request(
            &requests[0],
            "GET",
            &format!("/api/sites/{SITE}/wiredNetworks"),
        );
    }
}

#[tokio::test]
async fn summary_keeps_missing_or_malformed_scalars_null_and_debug_redacts_secrets() {
    let mut row = wired(NETWORK_ID, NETWORK_NAME, 20);
    row["wiredNetworkName"] = json!(17);
    row["vlanId"] = json!("20");
    row["isEnabled"] = json!("true");
    row["type"] = Value::Null;
    row["useDhcpScope"] = json!(1);
    row["dhcpScope"] = json!({"network":42,"netmask":false});
    row["wiredClientsCount"] = json!(-1);
    row["ipRoutingConfig"]["privateKey"] = json!("summary-secret-sentinel");
    let server = MockServer::start(vec![collection_reply(vec![row])]);
    let api = make_client(&server, Duration::from_secs(1));

    let networks = crate::client::network::list(&api, SITE).await.unwrap();
    let summary = networks[0].summary();
    for field in [
        "name", "vlan_id", "enabled", "type", "dhcp", "network", "netmask", "clients",
    ] {
        assert!(summary[field].is_null(), "{field} must stay unknown");
    }
    let debug = format!("{:?}", networks[0]);
    assert!(!debug.contains("summary-secret-sentinel"));
    assert!(debug.contains("(redacted)"));
    assert_eq!(server.finish().len(), 1);
}

#[tokio::test]
async fn selectors_prefer_id_and_reject_ambiguous_or_missing_names() {
    let server = MockServer::start(vec![collection_reply(vec![
        wired(NETWORK_ID, NETWORK_NAME, 20),
        wired(OTHER_ID, NETWORK_NAME, 30),
    ])]);
    let api = make_client(&server, Duration::from_secs(1));
    let rows = crate::client::network::list(&api, SITE).await.unwrap();
    assert_eq!(
        crate::client::network::select(&rows, NETWORK_ID)
            .unwrap()
            .id(),
        NETWORK_ID
    );
    assert_eq!(
        crate::client::network::select(&rows, NETWORK_NAME)
            .unwrap_err()
            .kind,
        ErrorKind::Usage
    );
    assert_eq!(
        crate::client::network::select(&rows, "missing")
            .unwrap_err()
            .kind,
        ErrorKind::NotFound
    );
    assert_eq!(server.finish().len(), 1);
}

#[tokio::test]
async fn partial_collection_and_unknown_template_type_refuse_mutation() {
    let mut partial = collection(vec![wired(NETWORK_ID, NETWORK_NAME, 20)]);
    partial["totalCount"] = json!(2);
    partial["matchingFilterCount"] = json!(2);
    let server = MockServer::start(vec![reply_json(partial)]);
    let api = make_client(&server, Duration::from_secs(1));
    let error = expect_error(
        NetworkMutation::update(
            &api,
            SITE,
            NETWORK_ID,
            Patch {
                enabled: Some(false),
                ..Patch::default()
            },
        )
        .await,
    );
    assert_eq!(error.kind, ErrorKind::Unverified);
    assert_eq!(server.finish().len(), 1);

    let mut template = wired("template", "Default", 2);
    template["type"] = json!("vendor-wired-kind");
    template["useDhcpScope"] = json!(false);
    template.as_object_mut().unwrap().remove("dhcpScope");
    let server = MockServer::start(vec![template_reply(template)]);
    let api = make_client(&server, Duration::from_secs(1));
    let error = expect_error(
        NetworkMutation::create(
            &api,
            SITE,
            Patch {
                name: Some("New".into()),
                vlan_id: Some(40),
                ..Patch::default()
            },
            CreatePortMembership::Template,
        )
        .await,
    );
    assert_eq!(error.kind, ErrorKind::Unverified);
    assert_eq!(server.finish().len(), 1);
}

#[tokio::test]
async fn update_uses_full_fetched_object_and_reads_the_change_back() {
    let before = wired(NETWORK_ID, NETWORK_NAME, 20);
    let after = merged(
        before.clone(),
        json!({"wiredNetworkName":"Updated LAN","type":"voice","vlanId":21}),
    );
    let server = MockServer::start(vec![
        collection_reply(vec![before]),
        reply_json(json!({"id":NETWORK_ID,"kind":"wiredNetwork"})),
        collection_reply(vec![after.clone()]),
    ]);
    let api = make_client(&server, Duration::from_secs(1));
    let (backend, plan) = NetworkMutation::update(
        &api,
        SITE,
        NETWORK_ID,
        Patch {
            name: Some("Updated LAN".into()),
            network_type: Some(NetworkType::Voice),
            vlan_id: Some(21),
            ..Patch::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(backend.target()["id"], NETWORK_ID);
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
        &format!("/api/sites/{SITE}/wiredNetworks"),
    );
    assert_request(
        &requests[1],
        "PUT",
        &format!("/api/sites/{SITE}/wiredNetworks/{NETWORK_ID}"),
    );
    assert_eq!(
        request_json(&requests[1]),
        after,
        "full PUT must retain fields outside the patch"
    );
    assert_eq!(
        request_json(&requests[1])["vendorExtension"],
        json!({"keep":[1,"opaque"]})
    );
    assert_request(
        &requests[2],
        "GET",
        &format!("/api/sites/{SITE}/wiredNetworks"),
    );
}

#[tokio::test]
async fn changing_network_type_preserves_existing_access_internet_and_qos_settings() {
    let mut before = wired(NETWORK_ID, NETWORK_NAME, 20);
    before["isAccessRestricted"] = json!(true);
    before["isInternetAllowed"] = json!(false);
    before["qos"] = json!({
        "trafficPriority":"veryHigh",
        "vendorQos":{"keep":"opaque"}
    });
    let mut after = before.clone();
    after["type"] = json!("voice");
    let server = MockServer::start(vec![
        collection_reply(vec![before]),
        reply_json(json!({"id":NETWORK_ID})),
        collection_reply(vec![after.clone()]),
    ]);
    let api = make_client(&server, Duration::from_secs(1));
    let (backend, plan) = NetworkMutation::update(
        &api,
        SITE,
        NETWORK_ID,
        Patch {
            network_type: Some(NetworkType::Voice),
            ..Patch::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        apply_once(&backend, &plan, Duration::from_secs(1))
            .await
            .unwrap()
            .outcome,
        Outcome::Verified
    );
    let requests = server.finish();
    assert_eq!(requests.len(), 3);
    let body = request_json(&requests[1]);
    assert_eq!(body["type"], "voice");
    assert_eq!(body["isAccessRestricted"], true);
    assert_eq!(body["isInternetAllowed"], false);
    assert_eq!(body["qos"]["trafficPriority"], "veryHigh");
    assert_eq!(body["qos"]["vendorQos"], json!({"keep":"opaque"}));
}

#[tokio::test]
async fn update_refuses_duplicate_vlan_and_vlan_that_cannot_change_before_put() {
    let server = MockServer::start(vec![collection_reply(vec![
        wired(NETWORK_ID, NETWORK_NAME, 20),
        wired(OTHER_ID, "Other", 21),
    ])]);
    let api = make_client(&server, Duration::from_secs(1));
    let error = expect_error(
        NetworkMutation::update(
            &api,
            SITE,
            NETWORK_ID,
            Patch {
                vlan_id: Some(21),
                ..Patch::default()
            },
        )
        .await,
    );
    assert_eq!(error.kind, ErrorKind::Usage);
    assert_eq!(server.finish().len(), 1);

    let mut cannot_change = wired(NETWORK_ID, NETWORK_NAME, 20);
    cannot_change["vlanIdCanBeChanged"] = json!(false);
    let server = MockServer::start(vec![collection_reply(vec![cannot_change])]);
    let api = make_client(&server, Duration::from_secs(1));
    let error = expect_error(
        NetworkMutation::update(
            &api,
            SITE,
            NETWORK_ID,
            Patch {
                vlan_id: Some(22),
                ..Patch::default()
            },
        )
        .await,
    );
    assert_eq!(error.kind, ErrorKind::Usage);
    assert_eq!(server.finish().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn update_readback_mismatch_is_reported_after_exactly_one_put() {
    let _clock = keep_clock_paused().await;
    let before = wired(NETWORK_ID, NETWORK_NAME, 20);
    let server = MockServer::start(vec![
        collection_reply(vec![before.clone()]),
        reply_json(json!({"id":NETWORK_ID})),
        collection_reply(vec![before]),
    ]);
    let api = make_client(&server, Duration::from_secs(1));
    let (backend, plan) = NetworkMutation::update(
        &api,
        SITE,
        NETWORK_ID,
        Patch {
            enabled: Some(false),
            ..Patch::default()
        },
    )
    .await
    .unwrap();
    let report = apply_readback_once(&backend, &plan, Duration::from_millis(150))
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
async fn create_uses_gateway_capability_reserved_subnets_template_and_fresh_readback() {
    let mut template = wired("template-id", "Default LAN", 2);
    template["id"] = Value::Null;
    template["vlanId"] = json!(2);
    template["dhcpScope"] = dhcp_scope();
    template["useDhcpScope"] = json!(true);
    template["templateOnly"] = json!("must not be sent");
    template["devicePortMappings"] = default_port_memberships();
    let mut created = template.clone();
    created["id"] = json!(NETWORK_ID);
    created["wiredNetworkName"] = json!("New LAN");
    created["vlanId"] = json!(40);
    created["type"] = json!("guest");
    created["isAccessRestricted"] = json!(true);
    created["dhcpScope"]["network"] = json!("172.30.2.0");
    // The server allocates gateway and pool values after accepting the
    // submitted subnet. These generated fields are outside our ownership.
    created["dhcpScope"]["ipAddress"] = json!("172.30.2.1");
    created["dhcpScope"]["ipAddressRange"] = json!({"start":"172.30.2.20","end":"172.30.2.200"});
    created["dhcpScope"]["runtimeMetadata"] = json!({"leaseCount":7});
    created["dhcpScope"]["ipReservations"] = json!([]);
    let reserved = json!({
        "managementIpSubnets":[{"network":"172.30.1.17","netmask":"255.255.255.0"}],"wanIpSubnets":[],"vpnIpSubnets":[],
        "defaultLocalDhcpSubnets":[],"configuredLocalDhcpSubnets":[],"domainIpSubnets":[]
    });
    let server = MockServer::start(vec![
        template_reply(template),
        reply_json(json!({"capabilities":["gateway-configuration"]})),
        reply_json(reserved),
        reply_json(
            json!({"id":NETWORK_ID,"wiredNetworkName":"New LAN","vlanId":40,"isWireless":false}),
        ),
        collection_reply(vec![created]),
    ]);
    let api = make_client(&server, Duration::from_secs(1));
    let (backend, plan) = NetworkMutation::create(
        &api,
        SITE,
        Patch {
            name: Some("New LAN".into()),
            vlan_id: Some(40),
            network_type: Some(NetworkType::Guest),
            ..Patch::default()
        },
        CreatePortMembership::Template,
    )
    .await
    .unwrap();
    let planned = serde_json::to_value(&plan.desired).unwrap();
    assert_eq!(
        planned["configuration"]["devicePortMappings"],
        json!([
            {
                "deviceId":"test-ap",
                "portMappings":[{"portNumber":0,"mapping":"absent"}],
                "trunkMappings":[]
            },
            {
                "deviceId":"test-switch",
                "portMappings":[
                    {"portNumber":1,"mapping":"tagged"},
                    {"portNumber":2,"mapping":"forbidden"},
                    {"portNumber":3,"mapping":"untagged"}
                ],
                "trunkMappings":[{"trunkNumber":1,"mapping":"tagged"}]
            }
        ])
    );
    assert_eq!(
        apply_once(&backend, &plan, Duration::from_secs(1))
            .await
            .unwrap()
            .outcome,
        Outcome::Verified
    );
    let requests = server.finish();
    assert_eq!(requests.len(), 5);
    assert_request(
        &requests[0],
        "GET",
        &format!("/api/sites/{SITE}/wiredNetworks"),
    );
    assert_request(
        &requests[1],
        "GET",
        &format!("/api/sites/{SITE}/capabilities"),
    );
    assert_request(
        &requests[2],
        "GET",
        &format!("/api/sites/{SITE}/reservedIpSubnets"),
    );
    assert_request(
        &requests[3],
        "POST",
        &format!("/api/sites/{SITE}/wiredNetworks"),
    );
    assert_request(
        &requests[4],
        "GET",
        &format!("/api/sites/{SITE}/wiredNetworks"),
    );
    let body = request_json(&requests[3]);
    assert_eq!(body["wiredNetworkName"], "New LAN");
    assert_eq!(body["vlanId"], 40);
    assert_eq!(body["type"], "guest");
    assert_eq!(body["isAccessRestricted"], true);
    assert_eq!(body["useDhcpScope"], true);
    assert_eq!(body["dhcpScope"]["network"], "172.30.2.0");
    assert!(body["dhcpScope"].get("ipAddress").is_none());
    assert!(body["dhcpScope"].get("ipAddressRange").is_none());
    assert_eq!(body["dhcpScope"]["ipReservations"], json!([]));
    assert!(body.get("templateOnly").is_none());
    assert_eq!(body["devicePortMappings"], default_port_memberships());
}

#[tokio::test]
async fn create_without_port_membership_changes_only_assignments_and_reads_exact_intent_back() {
    let mut template = wired("template-id", "Default LAN", 2);
    template["useDhcpScope"] = json!(false);
    template.as_object_mut().unwrap().remove("dhcpScope");
    template["devicePortMappings"] = default_port_memberships();

    let mut expected_mappings = default_port_memberships();
    expected_mappings[0]["portMappings"][0]["mapping"] = json!("absent");
    expected_mappings[0]["portMappings"][2]["mapping"] = json!("absent");
    expected_mappings[0]["trunkMappings"][0]["mapping"] = json!("absent");

    let mut created = template.clone();
    created["id"] = json!(NETWORK_ID);
    created["wiredNetworkName"] = json!("No Membership");
    created["vlanId"] = json!(40);
    created["devicePortMappings"] = expected_mappings.clone();
    // Readback may reorder rows and refresh telemetry. The semantic mapping
    // identity and state stay exact while those non-owned fields can change.
    created["devicePortMappings"]
        .as_array_mut()
        .unwrap()
        .reverse();
    created["devicePortMappings"][1]["portMappings"]
        .as_array_mut()
        .unwrap()
        .reverse();
    created["devicePortMappings"][1]["portMappings"][0]["vendorTelemetry"] =
        json!({"link":"down","sample":2});

    let server = MockServer::start(vec![
        template_reply(template),
        reply_json(
            json!({"id":NETWORK_ID,"wiredNetworkName":"No Membership","vlanId":40,"isWireless":false}),
        ),
        collection_reply(vec![created]),
    ]);
    let api = make_client(&server, Duration::from_secs(1));
    let (backend, plan) = NetworkMutation::create(
        &api,
        SITE,
        Patch {
            name: Some("No Membership".into()),
            vlan_id: Some(40),
            ..Patch::default()
        },
        CreatePortMembership::None,
    )
    .await
    .unwrap();
    assert_eq!(backend.target()["port_mappings"], false);
    let plan_json = serde_json::to_value(&plan.desired).unwrap();
    assert_eq!(
        plan_json["configuration"]["devicePortMappings"],
        json!([
            {
                "deviceId":"test-ap",
                "portMappings":[{"portNumber":0,"mapping":"absent"}],
                "trunkMappings":[]
            },
            {
                "deviceId":"test-switch",
                "portMappings":[
                    {"portNumber":1,"mapping":"absent"},
                    {"portNumber":2,"mapping":"forbidden"},
                    {"portNumber":3,"mapping":"absent"}
                ],
                "trunkMappings":[{"trunkNumber":1,"mapping":"absent"}]
            }
        ])
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
        &requests[0],
        "GET",
        &format!("/api/sites/{SITE}/wiredNetworks"),
    );
    assert_request(
        &requests[1],
        "POST",
        &format!("/api/sites/{SITE}/wiredNetworks"),
    );
    let sent_mappings = &request_json(&requests[1])["devicePortMappings"];
    assert_eq!(sent_mappings, &expected_mappings);
    assert_request(
        &requests[2],
        "GET",
        &format!("/api/sites/{SITE}/wiredNetworks"),
    );
}

#[tokio::test(start_paused = true)]
async fn create_mapping_readback_rejects_different_membership_with_same_summary_boolean() {
    let _clock = keep_clock_paused().await;
    for policy in [CreatePortMembership::Template, CreatePortMembership::None] {
        let mut template = wired("template-id", "Default LAN", 2);
        template["useDhcpScope"] = json!(false);
        template.as_object_mut().unwrap().remove("dhcpScope");
        template["devicePortMappings"] = default_port_memberships();
        let mut created = template.clone();
        if policy == CreatePortMembership::None {
            created["devicePortMappings"][0]["portMappings"][0]["mapping"] = json!("absent");
            created["devicePortMappings"][0]["portMappings"][2]["mapping"] = json!("absent");
            created["devicePortMappings"][0]["trunkMappings"][0]["mapping"] = json!("absent");
            // A server that leaves any requested assignment in place must not
            // pass because the target summary only reports a boolean.
            created["devicePortMappings"][0]["portMappings"][0]["mapping"] = json!("tagged");
        } else {
            // Keep the same tagged/untagged counts while assigning each state
            // to a different port. Count-only or boolean comparison is weak.
            created["devicePortMappings"][0]["portMappings"][0]["mapping"] = json!("untagged");
            created["devicePortMappings"][0]["portMappings"][2]["mapping"] = json!("tagged");
        }
        created["id"] = json!(NETWORK_ID);
        created["wiredNetworkName"] = json!("Template Membership");
        created["vlanId"] = json!(40);
        let server = MockServer::start(vec![
            template_reply(template),
            reply_json(
                json!({"id":NETWORK_ID,"wiredNetworkName":"Template Membership","vlanId":40,"isWireless":false}),
            ),
            collection_reply(vec![created]),
        ]);
        let api = make_client(&server, Duration::from_secs(1));
        let (backend, plan) = NetworkMutation::create(
            &api,
            SITE,
            Patch {
                name: Some("Template Membership".into()),
                vlan_id: Some(40),
                ..Patch::default()
            },
            policy,
        )
        .await
        .unwrap();
        assert_eq!(
            backend.target()["port_mappings"],
            policy == CreatePortMembership::Template
        );
        let report = apply_readback_once(&backend, &plan, Duration::from_millis(150))
            .await
            .unwrap();
        assert_eq!(report.outcome, Outcome::Unverified);
        let requests = server.finish();
        assert_eq!(requests.len(), 3);
        assert_eq!(requests[1].method, "POST");
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.method == "POST")
                .count(),
            1
        );
    }
}

#[tokio::test]
async fn create_refuses_unknown_or_ambiguous_template_mapping_identity_before_post() {
    let base = default_port_memberships();
    let mut missing_device = base.clone();
    missing_device[0]
        .as_object_mut()
        .unwrap()
        .remove("deviceId");
    let mut duplicate_device = base.clone();
    duplicate_device[1]["deviceId"] = json!("test-switch");
    let mut missing_port_collection = base.clone();
    missing_port_collection[0]
        .as_object_mut()
        .unwrap()
        .remove("portMappings");
    let mut malformed_trunk_collection = base.clone();
    malformed_trunk_collection[0]["trunkMappings"] = json!({});
    let mut missing_port = base.clone();
    missing_port[0]["portMappings"][0]
        .as_object_mut()
        .unwrap()
        .remove("portNumber");
    let mut duplicate_port = base.clone();
    duplicate_port[0]["portMappings"][1]["portNumber"] = json!(1);
    let mut missing_trunk = base.clone();
    missing_trunk[0]["trunkMappings"][0]
        .as_object_mut()
        .unwrap()
        .remove("trunkNumber");
    let mut duplicate_trunk = base.clone();
    duplicate_trunk[0]["trunkMappings"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "trunkNumber":1,"mapping":"absent"
        }));
    let mut unknown_mapping = base;
    unknown_mapping[0]["portMappings"][0]["mapping"] = json!("unknown-state");

    for mappings in [
        missing_device,
        duplicate_device,
        missing_port_collection,
        malformed_trunk_collection,
        missing_port,
        duplicate_port,
        missing_trunk,
        duplicate_trunk,
        unknown_mapping,
    ] {
        let mut template = wired("template-id", "Default LAN", 2);
        template["useDhcpScope"] = json!(false);
        template.as_object_mut().unwrap().remove("dhcpScope");
        template["devicePortMappings"] = mappings;
        let server = MockServer::start(vec![template_reply(template)]);
        let api = make_client(&server, Duration::from_secs(1));
        let error = expect_error(
            NetworkMutation::create(
                &api,
                SITE,
                Patch {
                    name: Some("No Membership".into()),
                    vlan_id: Some(40),
                    ..Patch::default()
                },
                CreatePortMembership::None,
            )
            .await,
        );
        assert_eq!(error.kind, ErrorKind::Unverified);
        let requests = server.finish();
        assert_eq!(requests.len(), 1, "invalid mappings must stop before POST");
        assert_eq!(requests[0].method, "GET");
    }
}

#[tokio::test]
async fn create_applies_type_access_internet_and_qos_defaults() {
    for (kind, restricted, priority) in [
        (NetworkType::Employee, false, "low"),
        (NetworkType::Guest, true, "veryHigh"),
        (NetworkType::Voice, false, "high"),
    ] {
        let kind_name = match kind {
            NetworkType::Employee => "employee",
            NetworkType::Guest => "guest",
            NetworkType::Voice => "voice",
        };
        let mut template = wired("template", "Default", 2);
        template["id"] = Value::Null;
        template["isAccessRestricted"] = json!(!restricted);
        template["isInternetAllowed"] = json!(false);
        template["qos"] = json!({
            "trafficPriority":"wrong",
            "vendorQos":{"keep":"opaque"}
        });
        let mut created = template.clone();
        created["id"] = json!(NETWORK_ID);
        created["wiredNetworkName"] = json!("Defaulted LAN");
        created["vlanId"] = json!(40);
        created["type"] = json!(kind_name);
        created["isAccessRestricted"] = json!(restricted);
        created["isInternetAllowed"] = json!(true);
        created["qos"]["trafficPriority"] = json!(priority);
        let mut payload = collection(vec![]);
        payload["metaData"] = json!({
            "defaultWiredNetwork":template,
            "wiredNetworkTypeDefaults":[
                {"networkType":"employee","trafficPriority":"low"},
                {"networkType":"guest","trafficPriority":"veryHigh"},
                {"networkType":"voice","trafficPriority":"high"}
            ]
        });
        let server = MockServer::start(vec![
            reply_json(payload),
            reply_json(json!({"id":NETWORK_ID,"isWireless":false})),
            collection_reply(vec![created]),
        ]);
        let api = make_client(&server, Duration::from_secs(1));
        let (backend, plan) = NetworkMutation::create(
            &api,
            SITE,
            Patch {
                name: Some("Defaulted LAN".into()),
                vlan_id: Some(40),
                network_type: Some(kind),
                ..Patch::default()
            },
            CreatePortMembership::Template,
        )
        .await
        .unwrap();
        assert_eq!(
            apply_once(&backend, &plan, Duration::from_secs(1))
                .await
                .unwrap()
                .outcome,
            Outcome::Verified,
            "creation defaults for {kind_name}"
        );
        let requests = server.finish();
        assert_eq!(requests.len(), 3);
        assert_request(
            &requests[1],
            "POST",
            &format!("/api/sites/{SITE}/wiredNetworks"),
        );
        let body = request_json(&requests[1]);
        assert_eq!(body["type"], kind_name);
        assert_eq!(body["isAccessRestricted"], restricted);
        assert_eq!(body["isInternetAllowed"], true);
        assert_eq!(body["qos"]["trafficPriority"], priority);
        assert_eq!(body["qos"]["vendorQos"], json!({"keep":"opaque"}));
    }
}

#[tokio::test(start_paused = true)]
async fn create_refuses_wrong_access_internet_or_qos_readback_after_one_post() {
    let _clock = keep_clock_paused().await;
    for mismatch in [
        "isAccessRestricted",
        "isInternetAllowed",
        "qos/trafficPriority",
    ] {
        let mut template = wired("template", "Default", 2);
        template["id"] = Value::Null;
        template["isAccessRestricted"] = json!(false);
        template["isInternetAllowed"] = json!(false);
        template["qos"]["trafficPriority"] = json!("wrong");
        let mut created = template.clone();
        created["id"] = json!(NETWORK_ID);
        created["wiredNetworkName"] = json!("Defaulted LAN");
        created["vlanId"] = json!(40);
        created["type"] = json!("guest");
        created["isAccessRestricted"] = json!(true);
        created["isInternetAllowed"] = json!(true);
        created["qos"]["trafficPriority"] = json!("medium");
        match mismatch {
            "isAccessRestricted" => created[mismatch] = json!(false),
            "isInternetAllowed" => created[mismatch] = json!(false),
            "qos/trafficPriority" => created["qos"]["trafficPriority"] = json!("low"),
            _ => unreachable!(),
        }
        let mut payload = collection(vec![]);
        payload["metaData"] = json!({"defaultWiredNetwork":template});
        let server = MockServer::start(vec![
            reply_json(payload),
            reply_json(json!({"id":NETWORK_ID,"isWireless":false})),
            collection_reply(vec![created]),
        ]);
        let api = make_client(&server, Duration::from_secs(1));
        let (backend, plan) = NetworkMutation::create(
            &api,
            SITE,
            Patch {
                name: Some("Defaulted LAN".into()),
                vlan_id: Some(40),
                network_type: Some(NetworkType::Guest),
                ..Patch::default()
            },
            CreatePortMembership::Template,
        )
        .await
        .unwrap();
        let report = apply_readback_once(&backend, &plan, Duration::from_millis(150))
            .await
            .unwrap();
        assert_eq!(report.outcome, Outcome::Unverified, "mismatch: {mismatch}");
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
}

#[tokio::test]
async fn create_refuses_unknown_malformed_or_duplicate_type_defaults_before_post() {
    let scenarios = [
        json!([{"networkType":"guest","trafficPriority":"veryhigh"}]),
        json!({"guest":{"trafficPriority":"high"}}),
        json!([
            {"networkType":"guest","trafficPriority":"high"},
            {"networkType":"guest","trafficPriority":"low"}
        ]),
    ];
    for defaults in scenarios {
        let mut template = wired("template", "Default", 2);
        template["id"] = Value::Null;
        let mut payload = collection(vec![]);
        payload["metaData"] = json!({
            "defaultWiredNetwork":template,
            "wiredNetworkTypeDefaults":defaults
        });
        let server = MockServer::start(vec![reply_json(payload)]);
        let api = make_client(&server, Duration::from_secs(1));
        let error = expect_error(
            NetworkMutation::create(
                &api,
                SITE,
                Patch {
                    name: Some("Defaulted LAN".into()),
                    vlan_id: Some(40),
                    network_type: Some(NetworkType::Guest),
                    ..Patch::default()
                },
                CreatePortMembership::Template,
            )
            .await,
        );
        assert_eq!(error.kind, ErrorKind::Unverified);
        assert_eq!(
            server.finish().len(),
            1,
            "invalid defaults must stop before POST"
        );
    }
}

#[tokio::test]
async fn create_without_gateway_capability_removes_template_dhcp_and_does_not_invent_scope() {
    let mut template = wired("template-id", "Default LAN", 2);
    template["dhcpScope"] = dhcp_scope();
    template["useDhcpScope"] = json!(true);
    let mut created = template.clone();
    created["id"] = json!(NETWORK_ID);
    created["wiredNetworkName"] = json!("No DHCP LAN");
    created["vlanId"] = json!(40);
    created["useDhcpScope"] = json!(false);
    created.as_object_mut().unwrap().remove("dhcpScope");
    let server = MockServer::start(vec![
        template_reply(template),
        reply_json(json!({"capabilities":["shared-services"]})),
        reply_json(json!({"id":NETWORK_ID,"wiredNetworkName":"No DHCP LAN","vlanId":40})),
        collection_reply(vec![created]),
    ]);
    let api = make_client(&server, Duration::from_secs(1));
    let (backend, plan) = NetworkMutation::create(
        &api,
        SITE,
        Patch {
            name: Some("No DHCP LAN".into()),
            vlan_id: Some(40),
            ..Patch::default()
        },
        CreatePortMembership::Template,
    )
    .await
    .unwrap();
    assert_eq!(
        apply_once(&backend, &plan, Duration::from_secs(1))
            .await
            .unwrap()
            .outcome,
        Outcome::Verified
    );
    let requests = server.finish();
    assert_eq!(requests.len(), 4);
    assert!(request_json(&requests[2]).get("dhcpScope").is_none());
    assert_eq!(request_json(&requests[2])["useDhcpScope"], false);
}

#[tokio::test]
async fn create_refuses_duplicate_or_unknown_vlan_before_post() {
    let mut template = wired("template", "Default", 2);
    template.as_object_mut().unwrap().remove("dhcpScope");
    let existing = wired(OTHER_ID, "Existing", 40);
    let server = MockServer::start(vec![template_collection_reply(
        vec![existing],
        template.clone(),
    )]);
    let api = make_client(&server, Duration::from_secs(1));
    // Collection data is fetched once; duplicate validation must stop before any capability lookup or POST.
    // First request is actually the collection that carries both template and elements.
    let error = expect_error(
        NetworkMutation::create(
            &api,
            SITE,
            Patch {
                name: Some("New".into()),
                vlan_id: Some(40),
                ..Patch::default()
            },
            CreatePortMembership::Template,
        )
        .await,
    );
    assert_eq!(error.kind, ErrorKind::Usage);
    let requests = server.finish();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "GET");

    let existing = wired(OTHER_ID, "Taken name", 30);
    let server = MockServer::start(vec![template_collection_reply(
        vec![existing],
        template.clone(),
    )]);
    let api = make_client(&server, Duration::from_secs(1));
    let error = expect_error(
        NetworkMutation::create(
            &api,
            SITE,
            Patch {
                name: Some("Taken name".into()),
                vlan_id: Some(40),
                ..Patch::default()
            },
            CreatePortMembership::Template,
        )
        .await,
    );
    assert_eq!(error.kind, ErrorKind::Usage);
    assert_eq!(server.finish().len(), 1);

    let mut unknown_vlan = wired(OTHER_ID, "Unknown VLAN", 40);
    unknown_vlan.as_object_mut().unwrap().remove("vlanId");
    let server = MockServer::start(vec![template_collection_reply(
        vec![unknown_vlan],
        template.clone(),
    )]);
    let api = make_client(&server, Duration::from_secs(1));
    let error = expect_error(
        NetworkMutation::create(
            &api,
            SITE,
            Patch {
                name: Some("New".into()),
                vlan_id: Some(41),
                ..Patch::default()
            },
            CreatePortMembership::Template,
        )
        .await,
    );
    assert_eq!(error.kind, ErrorKind::Unverified);
    assert_eq!(server.finish().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn create_ack_without_matching_fresh_readback_is_unverified() {
    let _clock = keep_clock_paused().await;
    let mut template = wired("template", "Default", 2);
    template["useDhcpScope"] = json!(false);
    template.as_object_mut().unwrap().remove("dhcpScope");
    let server = MockServer::start(vec![
        template_reply(template),
        reply_json(
            json!({"id":NETWORK_ID,"wiredNetworkName":"Created LAN","vlanId":40,"isWireless":false}),
        ),
        collection_reply(vec![]),
    ]);
    let api = make_client(&server, Duration::from_secs(1));
    let (backend, plan) = NetworkMutation::create(
        &api,
        SITE,
        Patch {
            name: Some("Created LAN".into()),
            vlan_id: Some(40),
            ..Patch::default()
        },
        CreatePortMembership::Template,
    )
    .await
    .unwrap();
    let report = apply_readback_once(&backend, &plan, Duration::from_millis(150))
        .await
        .unwrap();
    assert_eq!(report.outcome, Outcome::Unverified);
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
async fn create_rejects_missing_reused_and_foreign_acknowledgments_without_retry() {
    let _clock = keep_clock_paused().await;
    let mut template = wired("template", "Default", 2);
    template["useDhcpScope"] = json!(false);
    template.as_object_mut().unwrap().remove("dhcpScope");
    let intended = |id: &str| {
        let mut row = wired(id, "Created LAN", 40);
        row["type"] = json!("employee");
        row
    };
    let reused = wired(OTHER_ID, "Other LAN", 30);
    let scenarios = [
        (vec![], json!({}), vec![intended(NETWORK_ID)]),
        (
            vec![reused.clone()],
            json!({"id":OTHER_ID}),
            vec![reused.clone()],
        ),
        (
            vec![],
            json!({"id":NETWORK_ID,"wiredNetworkName":"Wrong LAN","vlanId":40,"isWireless":false}),
            vec![intended(NETWORK_ID)],
        ),
    ];
    for (existing, ack, readback) in scenarios {
        let server = MockServer::start(vec![
            template_collection_reply(existing, template.clone()),
            reply_json(ack),
            collection_reply(readback),
        ]);
        let api = make_client(&server, Duration::from_secs(1));
        let (backend, plan) = NetworkMutation::create(
            &api,
            SITE,
            Patch {
                name: Some("Created LAN".into()),
                vlan_id: Some(40),
                ..Patch::default()
            },
            CreatePortMembership::Template,
        )
        .await
        .unwrap();
        let report = apply_readback_once(&backend, &plan, Duration::from_millis(150))
            .await
            .unwrap();
        assert_ne!(
            report.outcome,
            Outcome::Verified,
            "bad ACK cannot verify a create"
        );
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
}

#[tokio::test]
async fn oversized_wired_collection_fails_once_without_echoing_payload() {
    let oversized = vec![b'x'; crate::client::MAX_RESPONSE_BYTES + 1];
    let server = MockServer::start(vec![Reply::json(200, oversized)]);
    let api = make_client(&server, Duration::from_secs(2));
    let error = expect_error(crate::client::network::list(&api, SITE).await);
    assert_eq!(error.kind, ErrorKind::General);
    assert!(error.message.contains("size limit"));
    assert!(!error.message.contains("xxxxxxxx"));
    assert_eq!(server.finish().len(), 1);
}

#[tokio::test]
async fn update_refuses_pool_that_excludes_an_existing_reservation() {
    let mut before = wired(NETWORK_ID, NETWORK_NAME, 20);
    before["useDhcpScope"] = json!(true);
    before["dhcpScope"] = dhcp_scope();
    before["dhcpScope"]["ipReservations"][0]["ipAddress"] = json!("192.0.2.99");
    let server = MockServer::start(vec![collection_reply(vec![before])]);
    let api = make_client(&server, Duration::from_secs(1));
    let error = expect_error(
        NetworkMutation::update(
            &api,
            SITE,
            NETWORK_ID,
            Patch {
                dhcp: DhcpPatch {
                    start: Some("192.0.2.20".parse().unwrap()),
                    end: Some("192.0.2.50".parse().unwrap()),
                    ..DhcpPatch::default()
                },
                ..Patch::default()
            },
        )
        .await,
    );
    assert_eq!(error.kind, ErrorKind::Usage);
    let requests = server.finish();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.method == "PUT")
            .count(),
        0
    );
}

#[tokio::test]
async fn update_refuses_domain_change_when_existing_gateway_is_malformed() {
    let mut before = wired(NETWORK_ID, NETWORK_NAME, 20);
    before["useDhcpScope"] = json!(true);
    before["dhcpScope"] = dhcp_scope();
    before["dhcpScope"]["ipAddress"] = json!("not-an-ip");
    let server = MockServer::start(vec![collection_reply(vec![before])]);
    let api = make_client(&server, Duration::from_secs(1));
    let error = expect_error(
        NetworkMutation::update(
            &api,
            SITE,
            NETWORK_ID,
            Patch {
                dhcp: DhcpPatch {
                    domain_name: Some("office.example".into()),
                    ..DhcpPatch::default()
                },
                ..Patch::default()
            },
        )
        .await,
    );
    assert_eq!(error.kind, ErrorKind::Unverified);
    let requests = server.finish();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.method == "PUT")
            .count(),
        0
    );
}

#[tokio::test]
async fn update_dhcp_preserves_nested_fields_and_refuses_invalid_scope_or_unsupported_disable() {
    let mut before = wired(NETWORK_ID, NETWORK_NAME, 20);
    before["useDhcpScope"] = json!(true);
    before["dhcpScope"] = dhcp_scope();
    let mut after = before.clone();
    after["dhcpScope"]["ipAddress"] = json!("192.0.2.1");
    after["dhcpScope"]["network"] = json!("192.0.2.0");
    after["dhcpScope"]["netmask"] = json!("255.255.255.0");
    after["dhcpScope"]["ipAddressRange"]["start"] = json!("192.0.2.25");
    after["dhcpScope"]["ipAddressRange"]["end"] = json!("192.0.2.180");
    after["dhcpScope"]["domainName"] = json!("office.example");
    after["dhcpScope"]["dns"]["dnsServerAssignationMode"] = json!("custom");
    after["dhcpScope"]["dns"]["customPrimaryDns"] = json!("192.0.2.53");
    after["dhcpScope"]["scopeExtension"] = json!({"retain":true});
    let server = MockServer::start(vec![
        collection_reply(vec![before]),
        reply_json(json!({"id":NETWORK_ID})),
        collection_reply(vec![after.clone()]),
    ]);
    let api = make_client(&server, Duration::from_secs(1));
    let patch = Patch {
        dhcp: DhcpPatch {
            gateway: Some("192.0.2.1".parse().unwrap()),
            prefix_length: Some(24),
            start: Some("192.0.2.25".parse().unwrap()),
            end: Some("192.0.2.180".parse().unwrap()),
            domain_name: Some("office.example".into()),
            dns_mode: Some(DnsMode::Custom),
            primary_dns: Some("192.0.2.53".parse().unwrap()),
            ..DhcpPatch::default()
        },
        ..Patch::default()
    };
    let (backend, plan) = NetworkMutation::update(&api, SITE, NETWORK_ID, patch)
        .await
        .unwrap();
    assert_eq!(
        apply_once(&backend, &plan, Duration::from_secs(1))
            .await
            .unwrap()
            .outcome,
        Outcome::Verified
    );
    let requests = server.finish();
    let body = request_json(&requests[1]);
    assert_eq!(
        body["dhcpScope"]["ipReservations"],
        dhcp_scope()["ipReservations"]
    );
    assert_eq!(body["dhcpScope"]["scopeExtension"], json!({"retain":true}));
    assert_eq!(body["dhcpScope"]["dns"]["automaticPrimaryDns"], "192.0.2.1");
    assert_eq!(body["dhcpScope"]["dns"]["customPrimaryDns"], "192.0.2.53");

    for (scope, patch) in [(
        dhcp_scope(),
        DhcpPatch {
            start: Some("192.0.3.20".parse().unwrap()),
            end: Some("192.0.3.40".parse().unwrap()),
            ..DhcpPatch::default()
        },
    )] {
        let mut row = wired(NETWORK_ID, NETWORK_NAME, 20);
        row["dhcpScope"] = scope;
        row["useDhcpScope"] = json!(true);
        let server = MockServer::start(vec![collection_reply(vec![row])]);
        let api = make_client(&server, Duration::from_secs(1));
        let error = expect_error(
            NetworkMutation::update(
                &api,
                SITE,
                NETWORK_ID,
                Patch {
                    dhcp: patch,
                    ..Patch::default()
                },
            )
            .await,
        );
        assert_eq!(error.kind, ErrorKind::Usage);
        assert_eq!(server.finish().len(), 1);
    }

    let mut malformed_mask = wired(NETWORK_ID, NETWORK_NAME, 20);
    malformed_mask["dhcpScope"] = dhcp_scope();
    malformed_mask["dhcpScope"]["netmask"] = json!("255.0.255.0");
    let server = MockServer::start(vec![collection_reply(vec![malformed_mask])]);
    let api = make_client(&server, Duration::from_secs(1));
    let error = expect_error(
        NetworkMutation::update(
            &api,
            SITE,
            NETWORK_ID,
            Patch {
                dhcp: DhcpPatch {
                    gateway: Some("192.0.2.2".parse().unwrap()),
                    ..DhcpPatch::default()
                },
                ..Patch::default()
            },
        )
        .await,
    );
    assert_eq!(error.kind, ErrorKind::Usage);
    assert_eq!(server.finish().len(), 1);

    let mut cannot_disable = wired(NETWORK_ID, NETWORK_NAME, 20);
    cannot_disable["canDisableDhcpScope"] = json!(false);
    cannot_disable["useDhcpScope"] = json!(true);
    cannot_disable["dhcpScope"] = dhcp_scope();
    let server = MockServer::start(vec![collection_reply(vec![cannot_disable])]);
    let api = make_client(&server, Duration::from_secs(1));
    let error = expect_error(
        NetworkMutation::update(
            &api,
            SITE,
            NETWORK_ID,
            Patch {
                dhcp: DhcpPatch {
                    enabled: Some(false),
                    ..DhcpPatch::default()
                },
                ..Patch::default()
            },
        )
        .await,
    );
    assert_eq!(error.kind, ErrorKind::Usage);
    assert_eq!(server.finish().len(), 1);
}

#[tokio::test]
async fn delete_requires_explicit_eligibility_and_yes_for_assigned_or_unknown_mappings() {
    let assigned = assigned_network();
    let server = MockServer::start(vec![collection_reply(vec![assigned.clone()])]);
    let api = make_client(&server, Duration::from_secs(1));
    let error = expect_error(NetworkMutation::delete(&api, SITE, NETWORK_ID, false).await);
    assert_eq!(error.kind, ErrorKind::Usage);
    assert_eq!(server.finish().len(), 1);

    let mut unknown = wired(NETWORK_ID, NETWORK_NAME, 20);
    unknown
        .as_object_mut()
        .unwrap()
        .remove("devicePortMappings");
    let server = MockServer::start(vec![collection_reply(vec![unknown])]);
    let api = make_client(&server, Duration::from_secs(1));
    let error = expect_error(NetworkMutation::delete(&api, SITE, NETWORK_ID, false).await);
    assert_eq!(error.kind, ErrorKind::Usage);
    assert_eq!(server.finish().len(), 1);

    let mut ineligible = wired(NETWORK_ID, NETWORK_NAME, 20);
    ineligible["isDeletable"] = json!(false);
    let server = MockServer::start(vec![collection_reply(vec![ineligible])]);
    let api = make_client(&server, Duration::from_secs(1));
    let error = expect_error(NetworkMutation::delete(&api, SITE, NETWORK_ID, true).await);
    assert_eq!(error.kind, ErrorKind::Usage);
    assert_eq!(server.finish().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn confirmed_delete_sends_one_request_and_reads_absence_back() {
    let _clock = keep_clock_paused().await;
    let assigned = assigned_network();
    let server = MockServer::start(vec![
        collection_reply(vec![assigned]),
        reply_json(json!({"id":NETWORK_ID})),
        collection_reply(vec![]),
    ]);
    let api = make_client(&server, Duration::from_secs(1));
    let (backend, plan) = NetworkMutation::delete(&api, SITE, NETWORK_ID, true)
        .await
        .unwrap();
    let report = apply_readback_once(&backend, &plan, Duration::from_millis(100))
        .await
        .unwrap();
    assert_eq!(report.outcome, Outcome::Verified);
    let requests = server.finish();
    assert_eq!(requests.len(), 3);
    assert_request(
        &requests[0],
        "GET",
        &format!("/api/sites/{SITE}/wiredNetworks"),
    );
    assert_request(
        &requests[1],
        "DELETE",
        &format!("/api/sites/{SITE}/wiredNetworks/{NETWORK_ID}"),
    );
    assert_request(
        &requests[2],
        "GET",
        &format!("/api/sites/{SITE}/wiredNetworks"),
    );
}

#[tokio::test(start_paused = true)]
async fn failed_conflict_write_is_not_retried_and_needs_readback() {
    let _clock = keep_clock_paused().await;
    let before = wired(NETWORK_ID, NETWORK_NAME, 20);
    let server = MockServer::start(vec![
        collection_reply(vec![before.clone()]),
        Reply::json(409, br#"{"error":"conflict"}"#.to_vec()),
        collection_reply(vec![before]),
    ]);
    let api = make_client(&server, Duration::from_secs(1));
    let (backend, plan) = NetworkMutation::update(
        &api,
        SITE,
        NETWORK_ID,
        Patch {
            enabled: Some(false),
            ..Patch::default()
        },
    )
    .await
    .unwrap();
    let report = apply_readback_once(&backend, &plan, Duration::from_millis(100))
        .await
        .unwrap();
    assert_eq!(report.outcome, Outcome::Failed);
    assert_eq!(report.error_kind(), Some(ErrorKind::ClientError));
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
async fn shared_services_global_toggle_uses_full_object_and_fresh_readback() {
    let before = json!({"isSharedServicesEnabled":false,"vendorExtension":{"keep":true}});
    let after = json!({"isSharedServicesEnabled":true,"vendorExtension":{"keep":true}});
    let server = MockServer::start(vec![
        reply_json(before),
        reply_json(json!({})),
        reply_json(after.clone()),
    ]);
    let api = make_client(&server, Duration::from_secs(1));
    let (backend, plan) = crate::client::network::SharedServicesMutation::update(&api, SITE, true)
        .await
        .unwrap();
    assert_eq!(
        backend.target(),
        json!({"resource":"sharedServicesConfiguration"})
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
        &requests[0],
        "GET",
        &format!("/api/sites/{SITE}/sharedServicesConfiguration"),
    );
    assert_request(
        &requests[1],
        "PUT",
        &format!("/api/sites/{SITE}/sharedServicesConfiguration"),
    );
    assert_eq!(request_json(&requests[1]), after);
    assert_request(
        &requests[2],
        "GET",
        &format!("/api/sites/{SITE}/sharedServicesConfiguration"),
    );
}

#[tokio::test]
async fn shared_services_global_disable_is_verified_and_missing_state_refuses_write() {
    let before = json!({"isSharedServicesEnabled":true,"vendorExtension":{"keep":true}});
    let after = json!({"isSharedServicesEnabled":false,"vendorExtension":{"keep":true}});
    let server = MockServer::start(vec![
        reply_json(before),
        reply_json(json!({})),
        reply_json(after.clone()),
    ]);
    let api = make_client(&server, Duration::from_secs(1));
    let (backend, plan) = crate::client::network::SharedServicesMutation::update(&api, SITE, false)
        .await
        .unwrap();
    assert_eq!(
        apply_once(&backend, &plan, Duration::from_secs(1))
            .await
            .unwrap()
            .outcome,
        Outcome::Verified
    );
    let requests = server.finish();
    assert_eq!(requests.len(), 3);
    assert_eq!(request_json(&requests[1]), after);

    let server = MockServer::start(vec![reply_json(json!({"vendorExtension":{"keep":true}}))]);
    let api = make_client(&server, Duration::from_secs(1));
    let error = expect_error(
        crate::client::network::SharedServicesMutation::update(&api, SITE, false).await,
    );
    assert_eq!(error.kind, ErrorKind::Unverified);
    assert_eq!(server.finish().len(), 1);
}

#[tokio::test]
async fn shared_service_change_toggles_every_type_for_one_mac_and_preserves_known_records() {
    const TARGET_MAC: &str = "02:00:00:00:00:01";
    const OTHER_MAC: &str = "02:00:00:00:00:02";
    let target_airplay = service(TARGET_MAC, "airplay", "ssid-1", false, "Conference Room");
    let target_print = service(TARGET_MAC, "airprint", "ssid-2", false, "Conference Room");
    let other = service(OTHER_MAC, "googlecast", "ssid-3", true, "Lobby Display");
    let local = service(TARGET_MAC, "sonos", "ssid-local", false, "Local Sonos");
    let before = with_services(
        wired(NETWORK_ID, NETWORK_NAME, 20),
        vec![local.clone()],
        vec![target_airplay.clone(), target_print.clone(), other.clone()],
    );
    let mut updated_airplay = target_airplay.clone();
    let mut updated_print = target_print.clone();
    updated_airplay["isShared"] = json!(true);
    updated_print["isShared"] = json!(true);
    let after = with_services(
        wired(NETWORK_ID, NETWORK_NAME, 20),
        vec![local.clone()],
        vec![
            updated_airplay.clone(),
            updated_print.clone(),
            other.clone(),
        ],
    );
    let server = MockServer::start(vec![
        collection_reply(vec![before]),
        reply_json(json!({})),
        collection_reply(vec![after]),
    ]);
    let api = make_client(&server, Duration::from_secs(1));
    let (backend, plan) = crate::client::network::SharedServiceMutation::update(
        &api,
        SITE,
        NETWORK_ID,
        "Conference Room",
        true,
    )
    .await
    .unwrap();
    assert_eq!(
        backend.target(),
        json!({"network_id":NETWORK_ID,"mac":TARGET_MAC})
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
        &requests[0],
        "GET",
        &format!("/api/sites/{SITE}/wiredNetworks"),
    );
    assert_request(
        &requests[1],
        "POST",
        &format!("/api/sites/{SITE}/wiredNetworks/{NETWORK_ID}?action=updateSharedService"),
    );
    assert_request(
        &requests[2],
        "GET",
        &format!("/api/sites/{SITE}/wiredNetworks"),
    );

    let body = request_json(&requests[1]);
    let rows = body["updateList"].as_array().unwrap();
    assert_eq!(rows.len(), 3);
    for expected in [updated_airplay, updated_print, other] {
        let row = rows
            .iter()
            .find(|row| {
                row["serviceType"] == expected["serviceType"]
                    && row["macAddress"] == expected["macAddress"]
            })
            .unwrap();
        for field in [
            "serviceType",
            "macAddress",
            "ipAddress",
            "vlanId",
            "name",
            "networkId",
            "networkName",
            "isShared",
            "serviceTags",
        ] {
            assert_eq!(row[field], expected[field], "field {field}");
        }
        assert!(
            row.get("portalMetadata").is_none(),
            "portal-only metadata must not be sent"
        );
    }
    assert!(
        !rows
            .iter()
            .any(|row| row["macAddress"] == TARGET_MAC && row["serviceType"] == "sonos")
    );
    assert_eq!(
        local["isShared"], false,
        "local services are outside this updateList"
    );
}

#[tokio::test(start_paused = true)]
async fn shared_service_unshare_is_verified_and_mismatched_readback_stays_unverified() {
    let _clock = keep_clock_paused().await;
    const TARGET_MAC: &str = "02:00:00:00:00:01";
    let shared = service(TARGET_MAC, "airplay", "ssid-1", true, "Conference Room");
    let other = service(
        "02:00:00:00:00:02",
        "googlecast",
        "ssid-2",
        true,
        "Lobby Display",
    );
    let before = with_services(
        wired(NETWORK_ID, NETWORK_NAME, 20),
        vec![],
        vec![shared.clone(), other.clone()],
    );
    let mut unshared = shared.clone();
    unshared["isShared"] = json!(false);
    let after = with_services(
        wired(NETWORK_ID, NETWORK_NAME, 20),
        vec![],
        vec![unshared, other.clone()],
    );
    let server = MockServer::start(vec![
        collection_reply(vec![before.clone()]),
        reply_json(json!({})),
        collection_reply(vec![after]),
    ]);
    let api = make_client(&server, Duration::from_secs(1));
    let (backend, plan) = crate::client::network::SharedServiceMutation::update(
        &api, SITE, NETWORK_ID, TARGET_MAC, false,
    )
    .await
    .unwrap();
    assert_eq!(
        apply_once(&backend, &plan, Duration::from_secs(1))
            .await
            .unwrap()
            .outcome,
        Outcome::Verified
    );
    let requests = server.finish();
    assert_eq!(requests.len(), 3);
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.method == "POST")
            .count(),
        1
    );
    assert!(
        request_json(&requests[1])["updateList"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| { row["macAddress"] == TARGET_MAC && row["isShared"] == false })
    );

    let server = MockServer::start(vec![
        collection_reply(vec![before.clone()]),
        reply_json(json!({})),
        collection_reply(vec![before]),
    ]);
    let api = make_client(&server, Duration::from_secs(1));
    let (backend, plan) = crate::client::network::SharedServiceMutation::update(
        &api, SITE, NETWORK_ID, TARGET_MAC, false,
    )
    .await
    .unwrap();
    let report = apply_readback_once(&backend, &plan, Duration::from_millis(100))
        .await
        .unwrap();
    assert_eq!(report.outcome, Outcome::Unverified);
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

#[tokio::test]
async fn shared_service_selection_refuses_ambiguous_missing_and_unknown_services_before_post() {
    let duplicate_names = with_services(
        wired(NETWORK_ID, NETWORK_NAME, 20),
        vec![],
        vec![
            service("02:00:00:00:00:01", "airplay", "ssid-1", false, "Same name"),
            service("02:00:00:00:00:02", "airplay", "ssid-2", false, "Same name"),
        ],
    );
    for (network, selector, expected) in [
        (duplicate_names, "Same name", ErrorKind::Usage),
        (
            with_services(
                wired(NETWORK_ID, NETWORK_NAME, 20),
                vec![],
                vec![service(
                    "02:00:00:00:00:01",
                    "airplay",
                    "ssid-1",
                    false,
                    "Available",
                )],
            ),
            "missing",
            ErrorKind::NotFound,
        ),
        (
            with_services(
                wired(NETWORK_ID, NETWORK_NAME, 20),
                vec![],
                vec![service(
                    "02:00:00:00:00:01",
                    "future-service",
                    "ssid-1",
                    false,
                    "Unknown",
                )],
            ),
            "02:00:00:00:00:01",
            ErrorKind::Unverified,
        ),
    ] {
        let server = MockServer::start(vec![collection_reply(vec![network])]);
        let api = make_client(&server, Duration::from_secs(1));
        let error = expect_error(
            crate::client::network::SharedServiceMutation::update(
                &api, SITE, NETWORK_ID, selector, true,
            )
            .await,
        );
        assert_eq!(error.kind, expected);
        assert_eq!(
            server.finish().len(),
            1,
            "validation must stop before the action"
        );
    }
}
