use super::*;
use crate::{
    Error, ErrorKind,
    client::wlan::{
        AccessPoints, AdvancedPatch, Bands, Binding, Patch, Security, TrafficPriority, WlanMutation,
    },
    mutation::{Outcome, apply_once},
};
use serde_json::{Value, json};
use std::time::Duration;

const SITE: &str = "123e4567-e89b-12d3-a456-426614174000";
const SSID_ID: &str = "ssid-opaque-advanced";
const SSID_NAME: &str = "Engineering Wi-Fi";
const OLD_PSK: &str = "old-wlan-secret-sentinel";
const RADIUS_SECRET: &str = "radius-server-secret-sentinel";
const RADIUS_ID: &str = "radius-profile-opaque";
const WIRED_ID: &str = "wired-network-opaque";

fn network() -> Value {
    json!({
        "id":SSID_ID,"isWireless":true,"networkName":SSID_NAME,
        "authentication":"psk","security":"wpa2","preSharedKey":OLD_PSK,
        "isEnabled":true,"isSsidHidden":false,"type":"employee",
        "isAvailableOn24GHzRadioBand":true,"isAvailableOn5GHzRadioBand":true,
        "isAvailableOn6GHzRadioBand":false,
        "isLegacy80211bRatesEnabled":false,"isHighEfficiency11axEnabled":true,
        "isHighEfficiency11axOfdmaEnabled":true,
        "isExtremelyHighThroughput11beEnabled":false,
        "isExtremelyHighThroughput11beMloEnabled":false,
        "isDynamicMulticastOptimizationEnabled":false,
        "isBroadcastOnAllBoundApsOnAllBands":false,
        "isCaptivePortalEnabled":false,"isGuestPortalEnabled":false,
        "useVlan":true,"vlanId":222,"wiredNetworkId":"old-wired-id",
        "ipAddressingMode":"network","radiusProfileId":"old-radius-id",
        "accessPoints":[
            {"deviceId":"ap-one","deviceName":"AP One","isBoundToNetwork":true,"extension":{"keep":1}},
            {"deviceId":"AA:BB:CC:DD:EE:FF","deviceName":"AP Two","isBoundToNetwork":false,"extension":{"keep":2}}
        ],
        "qos":{"isTrafficPriorityEnabled":false,"trafficPriority":"medium","limits":{"download":80},"extension":{"keep":true}},
        "vendorExtension":{"owner":"netcli-tests","opaque":[1,"keep"]}
    })
}

fn networks(elements: Vec<Value>) -> Reply {
    let count = elements.len() as u64;
    Reply::json(
        200,
        serde_json::to_vec(&json!({
            "kind":"networksSummary","totalCount":count,
            "matchingFilterCount":count,"elements":elements
        }))
        .expect("serialize networks summary"),
    )
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

fn update_replies(before: Value, after: Value, extra: Vec<Reply>) -> Vec<Reply> {
    let mut replies = vec![networks(vec![before])];
    replies.extend(extra);
    replies.push(reply_json(json!({"id":SSID_ID,"kind":"wirelessNetwork"})));
    replies.push(networks(vec![after]));
    replies
}

fn wired(id: &str, vlan: u16) -> Value {
    json!({
        "id":id,"isWireless":false,"wiredNetworkName":"Office VLAN",
        "isEnabled":true,"type":"employee","vlanId":vlan,
        "isManagement":false,"isDeletable":true,"vlanIdCanBeChanged":true,
        "canDisableDhcpScope":true,"useDhcpScope":false,
        "shouldApplyNetworkSecurityProtections":false,"isAccessRestricted":false,
        "isInternetAllowed":true,"isIntraSubnetTrafficAllowed":true,
        "isSpecificDestinationsAllowed":false,"allowedDestinations":[],
        "isIpRoutingEnabled":false,"ipRoutingConfig":{"isStatic":false},
        "devicePortMappings":[],"isGuestPortalEnabled":false,
        "isIgmpSnoopingEnabled":false,"qos":{"trafficPriority":"medium"},
        "vendorExtension":{"retain":"wired-extension"}
    })
}

fn wired_collection(elements: Vec<Value>) -> Reply {
    let count = elements.len() as u64;
    reply_json(json!({
        "kind":"wiredNetworks","totalCount":count,
        "matchingFilterCount":count,"elements":elements
    }))
}

fn radius_profile() -> Value {
    json!({
        "id":RADIUS_ID,"name":"Corporate RADIUS",
        "primaryServer":{"serverHost":"192.0.2.80","sharedSecret":RADIUS_SECRET,
            "timeout":5,"retryCount":3,"authPort":1812,"accountingPort":1813},
        "secondaryServer":null,"enableSecondaryServer":false,
        "enableRadiusOverTls":false,"requireRadiusAuthentication":false,
        "enableRadiusAccounting":false,"serverTimeoutSeconds":5,"serverRetryCount":3,
        "radiusNasIpSettings":{"useNasIpAddress":false},
        "radiusNasIdentifierSettings":{"useNasIdentifier":false},
        "usedByNetworks":[],"usedByDevices":[]
    })
}

fn radius_collection(elements: Vec<Value>) -> Reply {
    let count = elements.len() as u64;
    reply_json(json!({
        "kind":"resourceList","totalCount":count,
        "matchingFilterCount":count,"elements":elements
    }))
}

fn advanced_template() -> Value {
    json!({
        "id":"template-row","authentication":"802.1x","security":"wpa3",
        "networkName":"Template Wi-Fi","isEnabled":true,"useVlan":true,"vlanId":411,
        "wiredNetworkId":"template-wired","ipAddressingMode":"network",
        "isSsidHidden":false,"isWireless":false,"type":"employee",
        "isCaptivePortalEnabled":false,"isGuestPortalEnabled":false,
        "isAvailableOn24GHzRadioBand":true,"isAvailableOn5GHzRadioBand":true,
        "isAvailableOn6GHzRadioBand":true,"isLegacy80211bRatesEnabled":false,
        "isHighEfficiency11axEnabled":true,"isHighEfficiency11axOfdmaEnabled":false,
        "isExtremelyHighThroughput11beEnabled":false,
        "isExtremelyHighThroughput11beMloEnabled":false,
        "isDynamicMulticastOptimizationEnabled":false,
        "isBroadcastOnAllBoundApsOnAllBands":false,"isBandwidthLimitEnabled":false,
        "accessPoints":[
            {"deviceId":"create-ap-a","deviceName":"Create AP A","isBoundToNetwork":false,"opaque":{"retain":"a"}},
            {"deviceId":"create-ap-b","deviceName":"Create AP B","isBoundToNetwork":true,"opaque":{"retain":"b"}}
        ],
        "qos":{"isBandwidthLimitEnabled":false,"isDownloadBandwidthLimitEnabled":false,
            "isUploadBandwidthLimitEnabled":false,"isTrafficPriorityEnabled":false,
            "trafficPriority":"low","limits":{"upstream":20},"opaque":{"retain":true}},
        "wirelessClientsCount":9,"telemetry":{"shouldNot":"be posted"},
        "vendorExtension":{"retain":"top level is outside create serializer"}
    })
}

fn capabilities(names: &[&str]) -> Reply {
    reply_json(json!({"capabilities":names}))
}

fn inventory(elements: Vec<Value>) -> Reply {
    let count = elements.len() as u64;
    reply_json(json!({
        "kind":"resourceList","totalCount":count,"matchingFilterCount":count,
        "pendingAvailability":null,"elements":elements
    }))
}

fn test_client(server: &MockServer) -> crate::client::Client<StaticToken> {
    make_client(server, Duration::from_secs(1))
}

#[tokio::test]
async fn advanced_flags_ap_binding_and_qos_preserve_unknown_fields_and_verify_once() {
    let before = network();
    let mut after = before.clone();
    after["accessPoints"][0]["isBoundToNetwork"] = json!(false);
    after["accessPoints"][1]["isBoundToNetwork"] = json!(true);
    after["isLegacy80211bRatesEnabled"] = json!(true);
    after["isHighEfficiency11axEnabled"] = json!(false);
    after["isHighEfficiency11axOfdmaEnabled"] = json!(false);
    after["isExtremelyHighThroughput11beEnabled"] = json!(false);
    after["isExtremelyHighThroughput11beMloEnabled"] = json!(false);
    after["isDynamicMulticastOptimizationEnabled"] = json!(false);
    after["isBroadcastOnAllBoundApsOnAllBands"] = json!(true);

    let server = MockServer::start(update_replies(before, after.clone(), vec![]));
    let api = test_client(&server);
    let patch = Patch {
        advanced: AdvancedPatch {
            access_points: Some(AccessPoints::Selected(vec!["AA:BB:CC:DD:EE:FF".into()])),
            legacy_rates: Some(true),
            wifi6: Some(false),
            ofdma: Some(false),
            wifi7: Some(false),
            mlo: Some(false),
            multicast_optimization: Some(false),
            broadcast_all_bands: Some(true),
            ..AdvancedPatch::default()
        },
        ..Patch::default()
    };
    let (backend, plan) = WlanMutation::update(&api, SITE, SSID_ID, patch)
        .await
        .unwrap();
    let report = apply_once(&backend, &plan, Duration::from_secs(1))
        .await
        .unwrap();
    assert_eq!(report.outcome, Outcome::Verified);

    let requests = server.finish();
    assert_eq!(
        requests.len(),
        3,
        "QoS and disabled radio options need no capability GET"
    );
    assert_request(
        &requests[0],
        "GET",
        &format!("/api/sites/{SITE}/networksSummary"),
    );
    assert_request(
        &requests[1],
        "PUT",
        &format!("/api/sites/{SITE}/networksSummary/{SSID_ID}"),
    );
    assert_request(
        &requests[2],
        "GET",
        &format!("/api/sites/{SITE}/networksSummary"),
    );
    let sent = request_json(&requests[1]);
    assert_eq!(sent, after);
    assert_eq!(sent["accessPoints"][0]["extension"], json!({"keep":1}));
    assert_eq!(sent["accessPoints"][1]["extension"], json!({"keep":2}));
    assert_eq!(sent["qos"]["limits"]["download"], 80);
    assert_eq!(sent["qos"]["extension"]["keep"], true);
}

#[tokio::test]
async fn every_traffic_priority_patches_only_priority_fields_and_verifies_once() {
    let cases = [
        (TrafficPriority::Off, false, None),
        (TrafficPriority::Low, true, Some("low")),
        (TrafficPriority::Medium, true, Some("medium")),
        (TrafficPriority::High, true, Some("high")),
        (TrafficPriority::VeryHigh, true, Some("veryHigh")),
    ];
    for (priority, enabled, wire_priority) in cases {
        let before = network();
        let mut after = before.clone();
        after["qos"]["isTrafficPriorityEnabled"] = json!(enabled);
        if let Some(wire_priority) = wire_priority {
            after["qos"]["trafficPriority"] = json!(wire_priority);
        }
        let server = MockServer::start(update_replies(before, after.clone(), vec![]));
        let api = test_client(&server);
        let (backend, plan) = WlanMutation::update(
            &api,
            SITE,
            SSID_ID,
            Patch {
                advanced: AdvancedPatch {
                    traffic_priority: Some(priority),
                    ..AdvancedPatch::default()
                },
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
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.method == "PUT")
                .count(),
            1
        );
        let sent = request_json(&requests[1]);
        assert_eq!(sent["qos"]["isTrafficPriorityEnabled"], enabled);
        assert_eq!(
            sent["qos"]["trafficPriority"],
            wire_priority.unwrap_or("medium")
        );
        assert_eq!(sent["qos"]["limits"]["download"], 80);
        assert_eq!(sent["qos"]["extension"]["keep"], true);
    }
}

#[tokio::test]
async fn enabling_radio_features_requires_the_advertised_capability_set() {
    let mut before = network();
    before["isAvailableOn6GHzRadioBand"] = json!(true);
    let mut after = before.clone();
    after["security"] = json!("wpa3");
    after["isAvailableOn6GHzRadioBand"] = json!(true);
    after["isHighEfficiency11axEnabled"] = json!(true);
    after["isHighEfficiency11axOfdmaEnabled"] = json!(true);
    after["isExtremelyHighThroughput11beEnabled"] = json!(true);
    after["isExtremelyHighThroughput11beMloEnabled"] = json!(true);
    after["isDynamicMulticastOptimizationEnabled"] = json!(true);
    let required = [
        "high-efficiency-11ax",
        "high-efficiency-11ax-ofdma",
        "wifi-7",
        "wifi-6e",
        "multicast-optimizations",
        "radius-profiles",
    ];
    let server = MockServer::start(update_replies(
        before,
        after.clone(),
        vec![capabilities(&required[..5])],
    ));
    let api = test_client(&server);
    let (backend, plan) = WlanMutation::update(
        &api,
        SITE,
        SSID_ID,
        Patch {
            security: Some(Security::Wpa3Personal),
            bands: Some(Bands {
                two_four: true,
                five: true,
                six: true,
            }),
            advanced: AdvancedPatch {
                wifi6: Some(true),
                ofdma: Some(true),
                wifi7: Some(true),
                mlo: Some(true),
                multicast_optimization: Some(true),
                ..AdvancedPatch::default()
            },
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
    assert_eq!(requests.len(), 4);
    assert_request(
        &requests[0],
        "GET",
        &format!("/api/sites/{SITE}/networksSummary"),
    );
    assert_request(
        &requests[1],
        "GET",
        &format!("/api/sites/{SITE}/capabilities"),
    );
    assert_request(
        &requests[2],
        "PUT",
        &format!("/api/sites/{SITE}/networksSummary/{SSID_ID}"),
    );
    assert_request(
        &requests[3],
        "GET",
        &format!("/api/sites/{SITE}/networksSummary"),
    );
    assert_eq!(request_json(&requests[2]), after);
}

#[tokio::test]
async fn all_ap_binding_selects_each_existing_row_and_preserves_row_extensions() {
    let before = network();
    let mut after = before.clone();
    after["accessPoints"][0]["isBoundToNetwork"] = json!(true);
    after["accessPoints"][1]["isBoundToNetwork"] = json!(true);
    let server = MockServer::start(update_replies(before, after.clone(), vec![]));
    let api = test_client(&server);
    let (backend, plan) = WlanMutation::update(
        &api,
        SITE,
        SSID_ID,
        Patch {
            advanced: AdvancedPatch {
                access_points: Some(AccessPoints::All),
                ..AdvancedPatch::default()
            },
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
    assert_eq!(request_json(&requests[1]), after);
}

#[tokio::test]
async fn mac_selector_resolves_to_opaque_ap_id_through_complete_inventory() {
    const AP_ID: &str = "ap-opaque-device-id";
    const AP_MAC: &str = "02:AA:BB:CC:DD:EE";
    let mut before = network();
    before["accessPoints"][0]["deviceId"] = json!(AP_ID);
    before["accessPoints"][0]["isBoundToNetwork"] = json!(false);
    let mut after = before.clone();
    after["accessPoints"][0]["isBoundToNetwork"] = json!(true);
    after["accessPoints"][1]["isBoundToNetwork"] = json!(false);
    let server = MockServer::start(vec![
        networks(vec![before]),
        inventory(vec![json!({"id":AP_ID,"macAddress":AP_MAC})]),
        reply_json(json!({"id":SSID_ID,"kind":"wirelessNetwork"})),
        networks(vec![after.clone()]),
    ]);
    let api = test_client(&server);
    let (backend, plan) = WlanMutation::update(
        &api,
        SITE,
        SSID_ID,
        Patch {
            advanced: AdvancedPatch {
                access_points: Some(AccessPoints::Selected(vec![AP_MAC.to_ascii_lowercase()])),
                ..AdvancedPatch::default()
            },
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
    assert_eq!(requests.len(), 4);
    assert_request(
        &requests[0],
        "GET",
        &format!("/api/sites/{SITE}/networksSummary"),
    );
    assert_request(&requests[1], "GET", &format!("/api/sites/{SITE}/inventory"));
    assert_request(
        &requests[2],
        "PUT",
        &format!("/api/sites/{SITE}/networksSummary/{SSID_ID}"),
    );
    assert_request(
        &requests[3],
        "GET",
        &format!("/api/sites/{SITE}/networksSummary"),
    );
    assert_eq!(request_json(&requests[2]), after);
}

#[tokio::test]
async fn create_uses_template_and_resolves_enterprise_vlan_ap_radio_and_qos_fields() {
    const CREATED_ID: &str = "created-advanced-ssid";
    let template = advanced_template();
    let mut posted = template.clone();
    for field in [
        "id",
        "wirelessClientsCount",
        "telemetry",
        "vendorExtension",
        "isGuestPortalEnabled",
    ] {
        posted.as_object_mut().unwrap().remove(field);
    }
    posted["networkName"] = json!("New Enterprise Wi-Fi");
    posted["isWireless"] = json!(true);
    posted["wiredNetworkId"] = json!(WIRED_ID);
    posted["radiusProfileId"] = json!(RADIUS_ID);
    posted["isExtremelyHighThroughput11beEnabled"] = json!(true);
    posted["isExtremelyHighThroughput11beMloEnabled"] = json!(true);
    posted["isDynamicMulticastOptimizationEnabled"] = json!(true);
    posted["accessPoints"][0]["isBoundToNetwork"] = json!(true);
    posted["accessPoints"][1]["isBoundToNetwork"] = json!(true);
    posted["qos"]["isTrafficPriorityEnabled"] = json!(true);
    posted["qos"]["trafficPriority"] = json!("veryHigh");
    posted["isSpecificDestinationsAllowed"] = json!(false);
    posted["activeSchedule"] = json!("none");
    posted["schedule"] = json!({
        "activeDays": ["monday", "tuesday", "wednesday", "thursday", "friday"],
        "activeTimeRange": {"enabled": false},
    });
    let weekdays = [
        "monday",
        "tuesday",
        "wednesday",
        "thursday",
        "friday",
        "saturday",
        "sunday",
    ]
    .into_iter()
    .map(|day| {
        (
            day.to_owned(),
            json!({
                "enabled": false, "activeAllDay": true,
                "startTime": "09:00", "endTime": "17:00",
            }),
        )
    })
    .collect::<serde_json::Map<_, _>>();
    posted["weekSchedule"] = json!({"schedulePerWeekdayMap": weekdays});
    let mut created = posted.clone();
    created["id"] = json!(CREATED_ID);
    created["wirelessClientsCount"] = json!(0);

    let server = MockServer::start(vec![
        networks(vec![]),
        reply_json(json!({"metaData":{"defaultWirelessNetwork":template}})),
        wired_collection(vec![wired(WIRED_ID, 212)]),
        radius_collection(vec![radius_profile()]),
        capabilities(&[
            "radius-profiles",
            "high-efficiency-11ax",
            "wifi-7",
            "wifi-6e",
            "multicast-optimizations",
        ]),
        reply_json(
            json!({"id":CREATED_ID,"kind":"wirelessNetwork","networkName":"New Enterprise Wi-Fi","isWireless":true}),
        ),
        networks(vec![created]),
    ]);
    let api = test_client(&server);
    let (backend, plan) = WlanMutation::create(
        &api,
        SITE,
        Patch {
            name: Some("New Enterprise Wi-Fi".into()),
            advanced: AdvancedPatch {
                binding: Some(Binding::Vlan(212)),
                radius_profile: Some("Corporate RADIUS".into()),
                access_points: Some(AccessPoints::All),
                wifi6: Some(true),
                wifi7: Some(true),
                mlo: Some(true),
                multicast_optimization: Some(true),
                traffic_priority: Some(TrafficPriority::VeryHigh),
                ..AdvancedPatch::default()
            },
            ..Patch::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(backend.target()["name"], "New Enterprise Wi-Fi");
    assert_eq!(
        apply_once(&backend, &plan, Duration::from_secs(1))
            .await
            .unwrap()
            .outcome,
        Outcome::Verified
    );

    let requests = server.finish();
    assert_eq!(requests.len(), 7);
    assert_request(
        &requests[0],
        "GET",
        &format!("/api/sites/{SITE}/networksSummary"),
    );
    assert_request(
        &requests[1],
        "GET",
        &format!("/api/sites/{SITE}/wiredNetworks"),
    );
    assert_request(
        &requests[2],
        "GET",
        &format!("/api/sites/{SITE}/wiredNetworks"),
    );
    assert_request(
        &requests[3],
        "GET",
        &format!("/api/sites/{SITE}/radiusProfiles"),
    );
    assert_request(
        &requests[4],
        "GET",
        &format!("/api/sites/{SITE}/capabilities"),
    );
    assert_request(
        &requests[5],
        "POST",
        &format!("/api/sites/{SITE}/networksSummary"),
    );
    assert_request(
        &requests[6],
        "GET",
        &format!("/api/sites/{SITE}/networksSummary"),
    );
    let sent = request_json(&requests[5]);
    assert_eq!(sent, posted);
    assert_eq!(sent["useVlan"], true);
    assert_eq!(sent["vlanId"], 411);
    assert_eq!(sent["wiredNetworkId"], WIRED_ID);
    assert_eq!(sent["radiusProfileId"], RADIUS_ID);
    assert_eq!(sent["accessPoints"][0]["opaque"], json!({"retain":"a"}));
    assert_eq!(sent["accessPoints"][1]["opaque"], json!({"retain":"b"}));
    assert_eq!(sent["qos"]["limits"]["upstream"], 20);
    assert_eq!(sent["qos"]["opaque"]["retain"], true);
    assert!(sent.get("id").is_none());
    assert!(sent.get("wirelessClientsCount").is_none());
    assert!(sent.get("telemetry").is_none());
    assert!(
        !serde_json::to_string(&sent)
            .unwrap()
            .contains(RADIUS_SECRET)
    );
}

#[tokio::test]
async fn create_fails_unverified_before_post_when_requested_qos_template_is_missing() {
    let mut template = advanced_template();
    template.as_object_mut().unwrap().remove("qos");
    let server = MockServer::start(vec![
        networks(vec![]),
        reply_json(json!({"metaData":{"defaultWirelessNetwork":template}})),
    ]);
    let api = test_client(&server);
    let error = expect_error(
        WlanMutation::create(
            &api,
            SITE,
            Patch {
                name: Some("New Wi-Fi".into()),
                advanced: AdvancedPatch {
                    traffic_priority: Some(TrafficPriority::High),
                    ..AdvancedPatch::default()
                },
                ..Patch::default()
            },
        )
        .await,
    );
    assert_eq!(error.kind, ErrorKind::Unverified);
    assert!(
        server
            .finish()
            .iter()
            .all(|request| request.method != "POST")
    );
}

#[tokio::test]
async fn vlan_binding_resolves_existing_enabled_row_and_preserves_vlan_fields() {
    let before = network();
    let mut after = before.clone();
    after["wiredNetworkId"] = json!(WIRED_ID);
    after["ipAddressingMode"] = json!("network");
    let server = MockServer::start(update_replies(
        before,
        after.clone(),
        vec![wired_collection(vec![wired(WIRED_ID, 212)])],
    ));
    let api = test_client(&server);
    let (backend, plan) = WlanMutation::update(
        &api,
        SITE,
        SSID_ID,
        Patch {
            advanced: AdvancedPatch {
                binding: Some(Binding::Vlan(212)),
                ..AdvancedPatch::default()
            },
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
    assert_eq!(requests.len(), 4);
    assert_request(
        &requests[0],
        "GET",
        &format!("/api/sites/{SITE}/networksSummary"),
    );
    assert_request(
        &requests[1],
        "GET",
        &format!("/api/sites/{SITE}/wiredNetworks"),
    );
    assert_request(
        &requests[2],
        "PUT",
        &format!("/api/sites/{SITE}/networksSummary/{SSID_ID}"),
    );
    assert_request(
        &requests[3],
        "GET",
        &format!("/api/sites/{SITE}/networksSummary"),
    );
    let sent = request_json(&requests[2]);
    assert_eq!(sent, after);
    assert_eq!(sent["useVlan"], true);
    assert_eq!(sent["vlanId"], 222);
    assert_eq!(sent["vendorExtension"]["opaque"], json!([1, "keep"]));
}

#[tokio::test]
async fn enterprise_security_clears_psk_and_radius_assignment_never_copies_profile_secrets() {
    let before = network();
    let mut after = before.clone();
    after["authentication"] = json!("802.1x");
    after["security"] = json!("wpa2");
    after["preSharedKey"] = json!("");
    after["radiusProfileId"] = json!(RADIUS_ID);
    let server = MockServer::start(update_replies(
        before,
        after.clone(),
        vec![
            radius_collection(vec![radius_profile()]),
            capabilities(&["radius-profiles"]),
        ],
    ));
    let api = test_client(&server);
    let (backend, plan) = WlanMutation::update(
        &api,
        SITE,
        SSID_ID,
        Patch {
            security: Some(Security::Wpa2Enterprise),
            advanced: AdvancedPatch {
                radius_profile: Some("Corporate RADIUS".into()),
                ..AdvancedPatch::default()
            },
            ..Patch::default()
        },
    )
    .await
    .unwrap();
    assert!(!format!("{plan:?}").contains(RADIUS_SECRET));
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
        &format!("/api/sites/{SITE}/networksSummary"),
    );
    assert_request(
        &requests[1],
        "GET",
        &format!("/api/sites/{SITE}/radiusProfiles"),
    );
    assert_request(
        &requests[2],
        "GET",
        &format!("/api/sites/{SITE}/capabilities"),
    );
    assert_request(
        &requests[3],
        "PUT",
        &format!("/api/sites/{SITE}/networksSummary/{SSID_ID}"),
    );
    assert_request(
        &requests[4],
        "GET",
        &format!("/api/sites/{SITE}/networksSummary"),
    );
    let sent = request_json(&requests[3]);
    assert_eq!(sent, after);
    assert_eq!(sent["radiusProfileId"], RADIUS_ID);
    assert_eq!(sent["authentication"], "802.1x");
    assert_eq!(sent["preSharedKey"], "");
    assert!(!serde_json::to_string(&sent).unwrap().contains(OLD_PSK));
    assert!(
        !serde_json::to_string(&sent)
            .unwrap()
            .contains(RADIUS_SECRET)
    );
    assert_eq!(
        sent["vendorExtension"],
        json!({"owner":"netcli-tests","opaque":[1,"keep"]})
    );
}

#[tokio::test]
async fn guest_portal_sets_both_portal_flags_and_employee_refusal_precedes_put() {
    let mut before = network();
    before["type"] = json!("guest");
    let mut after = before.clone();
    after["isCaptivePortalEnabled"] = json!(true);
    after["isGuestPortalEnabled"] = json!(true);
    let server = MockServer::start(update_replies(before, after.clone(), vec![]));
    let api = test_client(&server);
    let (backend, plan) = WlanMutation::update(
        &api,
        SITE,
        SSID_ID,
        Patch {
            advanced: AdvancedPatch {
                captive_portal: Some(true),
                ..AdvancedPatch::default()
            },
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
    assert_eq!(request_json(&requests[1]), after);

    let mut employee = network();
    employee["isGuestPortalEnabled"] = json!(false);
    let server = MockServer::start(vec![networks(vec![employee])]);
    let api = test_client(&server);
    let error = expect_error(
        WlanMutation::update(
            &api,
            SITE,
            SSID_ID,
            Patch {
                advanced: AdvancedPatch {
                    captive_portal: Some(true),
                    ..AdvancedPatch::default()
                },
                ..Patch::default()
            },
        )
        .await,
    );
    assert_eq!(error.kind, ErrorKind::Usage);
    assert_eq!(
        server.finish().len(),
        1,
        "invalid merged state must stop before PUT"
    );
}

#[tokio::test]
async fn invalid_ap_maps_fail_closed_before_put() {
    let cases = [
        (
            "ambiguous AP name",
            ErrorKind::Usage,
            {
                let mut row = network();
                row["accessPoints"][1]["deviceName"] = json!("AP One");
                row
            },
            "AP One",
        ),
        (
            "missing AP selector",
            ErrorKind::NotFound,
            network(),
            "unknown-ap",
        ),
        (
            "empty AP map",
            ErrorKind::Unverified,
            {
                let mut row = network();
                row["accessPoints"] = json!([]);
                row
            },
            "ap-one",
        ),
        (
            "duplicate AP identity",
            ErrorKind::Unverified,
            {
                let mut row = network();
                row["accessPoints"][1]["deviceId"] = json!("ap-one");
                row
            },
            "ap-one",
        ),
        (
            "missing AP identity",
            ErrorKind::Unverified,
            {
                let mut row = network();
                row["accessPoints"][1]
                    .as_object_mut()
                    .unwrap()
                    .remove("deviceId");
                row
            },
            "ap-one",
        ),
    ];
    for (case, expected, before, selector) in cases {
        let server = MockServer::start(vec![networks(vec![before])]);
        let api = test_client(&server);
        let error = expect_error(
            WlanMutation::update(
                &api,
                SITE,
                SSID_ID,
                Patch {
                    advanced: AdvancedPatch {
                        access_points: Some(AccessPoints::Selected(vec![selector.into()])),
                        ..AdvancedPatch::default()
                    },
                    ..Patch::default()
                },
            )
            .await,
        );
        assert_eq!(error.kind, expected, "{case}");
        assert_eq!(server.finish().len(), 1, "{case} must fail before PUT");
    }
}

#[tokio::test]
async fn unmatched_mac_and_partial_ap_inventory_fail_before_put() {
    let cases = [
        (
            inventory(vec![
                json!({"id":"unrelated-ap","macAddress":"02:00:00:00:00:01"}),
            ]),
            ErrorKind::NotFound,
        ),
        (
            reply_json(json!({
                "kind":"resourceList","totalCount":2,"matchingFilterCount":2,
                "pendingAvailability":null,
                "elements":[{"id":"unrelated-ap","macAddress":"02:00:00:00:00:01"}]
            })),
            ErrorKind::Unverified,
        ),
    ];
    for (inventory_reply, expected) in cases {
        let server = MockServer::start(vec![networks(vec![network()]), inventory_reply]);
        let api = test_client(&server);
        let error = expect_error(
            WlanMutation::update(
                &api,
                SITE,
                SSID_ID,
                Patch {
                    advanced: AdvancedPatch {
                        access_points: Some(AccessPoints::Selected(vec![
                            "02:12:34:56:78:9A".into(),
                        ])),
                        ..AdvancedPatch::default()
                    },
                    ..Patch::default()
                },
            )
            .await,
        );
        assert_eq!(error.kind, expected);
        assert_eq!(server.finish().len(), 2);
    }
}

#[tokio::test]
async fn vlan_selector_requires_one_existing_enabled_network_with_known_state() {
    let cases = [
        ("missing VLAN", ErrorKind::Usage, vec![]),
        (
            "ambiguous VLAN",
            ErrorKind::Usage,
            vec![wired(WIRED_ID, 212), wired("wired-duplicate", 212)],
        ),
        (
            "disabled VLAN",
            ErrorKind::Usage,
            vec![{
                let mut row = wired(WIRED_ID, 212);
                row["isEnabled"] = json!(false);
                row
            }],
        ),
        (
            "unknown enabled state",
            ErrorKind::Unverified,
            vec![{
                let mut row = wired(WIRED_ID, 212);
                row.as_object_mut().unwrap().remove("isEnabled");
                row
            }],
        ),
        (
            "partial wired collection",
            ErrorKind::Unverified,
            vec![wired(WIRED_ID, 212)],
        ),
    ];
    for (case, expected, rows) in cases {
        let wired_reply = if case == "partial wired collection" {
            reply_json(json!({
                "kind":"wiredNetworks","totalCount":2,
                "matchingFilterCount":2,"elements":rows
            }))
        } else {
            wired_collection(rows)
        };
        let server = MockServer::start(vec![networks(vec![network()]), wired_reply]);
        let api = test_client(&server);
        let error = expect_error(
            WlanMutation::update(
                &api,
                SITE,
                SSID_ID,
                Patch {
                    advanced: AdvancedPatch {
                        binding: Some(Binding::Vlan(212)),
                        ..AdvancedPatch::default()
                    },
                    ..Patch::default()
                },
            )
            .await,
        );
        assert_eq!(error.kind, expected, "{case}");
        assert_eq!(server.finish().len(), 2, "{case} must stop before PUT");
    }
}

#[tokio::test]
async fn invalid_advanced_combinations_fail_before_any_http_request() {
    let invalid = [
        AdvancedPatch {
            ofdma: Some(true),
            wifi6: Some(false),
            ..AdvancedPatch::default()
        },
        AdvancedPatch {
            mlo: Some(true),
            wifi7: Some(false),
            ..AdvancedPatch::default()
        },
    ];
    for advanced in invalid {
        let server = MockServer::start(vec![]);
        let api = test_client(&server);
        let error = expect_error(
            WlanMutation::update(
                &api,
                SITE,
                SSID_ID,
                Patch {
                    advanced,
                    ..Patch::default()
                },
            )
            .await,
        );
        assert_eq!(error.kind, ErrorKind::Usage);
        assert_eq!(server.finish().len(), 0);
    }

    let server = MockServer::start(vec![]);
    let api = test_client(&server);
    let error = expect_error(
        WlanMutation::update(
            &api,
            SITE,
            SSID_ID,
            Patch {
                security: Some(Security::Wpa2Enterprise),
                ..Patch::default()
            },
        )
        .await,
    );
    assert_eq!(error.kind, ErrorKind::Usage);
    assert_eq!(
        server.finish().len(),
        0,
        "enterprise security needs a profile before HTTP"
    );
}

#[tokio::test]
async fn merged_radio_invariant_failure_happens_after_read_and_before_put() {
    let mut before = network();
    before["isHighEfficiency11axEnabled"] = json!(true);
    before["isHighEfficiency11axOfdmaEnabled"] = json!(true);
    let server = MockServer::start(vec![networks(vec![before])]);
    let api = test_client(&server);
    let error = expect_error(
        WlanMutation::update(
            &api,
            SITE,
            SSID_ID,
            Patch {
                advanced: AdvancedPatch {
                    wifi6: Some(false),
                    ..AdvancedPatch::default()
                },
                ..Patch::default()
            },
        )
        .await,
    );
    assert_eq!(error.kind, ErrorKind::Usage);
    assert_eq!(
        server.finish().len(),
        1,
        "the fetched state exposes the dependent OFDMA setting"
    );
}

#[tokio::test]
async fn qos_priority_is_refused_when_the_network_marks_it_unsupported() {
    let mut before = network();
    before["capabilities"] = json!({"qosTrafficPriority":false});
    let server = MockServer::start(vec![networks(vec![before])]);
    let api = test_client(&server);
    let error = expect_error(
        WlanMutation::update(
            &api,
            SITE,
            SSID_ID,
            Patch {
                advanced: AdvancedPatch {
                    traffic_priority: Some(TrafficPriority::High),
                    ..AdvancedPatch::default()
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
async fn missing_capability_and_partial_inventories_stop_before_put() {
    let before = network();
    let server = MockServer::start(vec![networks(vec![before]), capabilities(&[])]);
    let api = test_client(&server);
    let error = expect_error(
        WlanMutation::update(
            &api,
            SITE,
            SSID_ID,
            Patch {
                advanced: AdvancedPatch {
                    wifi6: Some(true),
                    ..AdvancedPatch::default()
                },
                ..Patch::default()
            },
        )
        .await,
    );
    assert_eq!(error.kind, ErrorKind::Usage);
    assert_eq!(server.finish().len(), 2);

    let before = network();
    let server = MockServer::start(vec![Reply::json(
        200,
        serde_json::to_vec(&json!({
            "kind":"networksSummary","totalCount":2,
            "matchingFilterCount":2,"elements":[before]
        }))
        .unwrap(),
    )]);
    let api = test_client(&server);
    let error = expect_error(
        WlanMutation::update(
            &api,
            SITE,
            SSID_ID,
            Patch {
                advanced: AdvancedPatch {
                    legacy_rates: Some(true),
                    ..AdvancedPatch::default()
                },
                ..Patch::default()
            },
        )
        .await,
    );
    assert_eq!(error.kind, ErrorKind::Unverified);
    assert_eq!(server.finish().len(), 1);
}

#[tokio::test]
async fn readback_mismatch_is_unverified_and_does_not_retry_the_put() {
    let before = network();
    let unchanged = before.clone();
    let server = MockServer::start(update_replies(before, unchanged, vec![]));
    let api = test_client(&server);
    let (backend, plan) = WlanMutation::update(
        &api,
        SITE,
        SSID_ID,
        Patch {
            advanced: AdvancedPatch {
                broadcast_all_bands: Some(true),
                ..AdvancedPatch::default()
            },
            ..Patch::default()
        },
    )
    .await
    .unwrap();
    let report = apply_once(&backend, &plan, Duration::from_secs(1))
        .await
        .unwrap();
    assert_eq!(report.outcome, Outcome::Unverified);
    let requests = server.finish();
    assert_eq!(requests[0].method, "GET");
    assert_eq!(requests[1].method, "PUT");
    assert!(requests[2..].iter().all(|request| request.method == "GET"
        && request.target == format!("/api/sites/{SITE}/networksSummary")));
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.method == "PUT")
            .count(),
        1
    );
}

#[tokio::test]
async fn oversized_network_inventory_is_rejected_before_put() {
    let oversized = vec![b'x'; crate::client::MAX_RESPONSE_BYTES + 1];
    let server = MockServer::start(vec![Reply::json(200, oversized)]);
    let api = test_client(&server);
    let error = expect_error(
        WlanMutation::update(
            &api,
            SITE,
            SSID_ID,
            Patch {
                advanced: AdvancedPatch {
                    legacy_rates: Some(true),
                    ..AdvancedPatch::default()
                },
                ..Patch::default()
            },
        )
        .await,
    );
    assert_eq!(error.kind, ErrorKind::General);
    assert!(error.message.contains("size limit"));
    assert!(!error.message.contains("xxxxxxxxxx"));
    assert_eq!(server.finish().len(), 1);
}

#[tokio::test]
async fn guest_switch_cannot_leave_existing_enterprise_authentication_on_a_guest_network() {
    let mut before = network();
    before["authentication"] = json!("802.1x");
    before["radiusProfileId"] = json!("radius-profile-existing");
    let server = MockServer::start(vec![networks(vec![before])]);
    let client = test_client(&server);
    let error = expect_error(
        WlanMutation::update(
            &client,
            SITE,
            SSID_ID,
            Patch {
                guest: Some(true),
                ..Patch::default()
            },
        )
        .await,
    );
    assert_eq!(error.kind, ErrorKind::Usage);
    let requests = server.finish();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "GET");
}

#[tokio::test]
async fn guest_to_employee_refuses_inherited_active_portal_before_put() {
    for flag in ["isGuestPortalEnabled", "isCaptivePortalEnabled"] {
        let mut before = network();
        before["type"] = json!("guest");
        before[flag] = json!(true);
        let server = MockServer::start(vec![networks(vec![before])]);
        let client = test_client(&server);
        let error = expect_error(
            WlanMutation::update(
                &client,
                SITE,
                SSID_ID,
                Patch {
                    guest: Some(false),
                    ..Patch::default()
                },
            )
            .await,
        );
        assert_eq!(error.kind, ErrorKind::Usage);
        assert!(error.message.contains("--captive-portal false"));
        let requests = server.finish();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "GET");
    }
}

#[tokio::test]
async fn guest_to_employee_explicit_portal_disable_is_sent_and_verified_with_type() {
    for verified in [true, false] {
        let mut before = network();
        before["type"] = json!("guest");
        before["isGuestPortalEnabled"] = json!(true);
        before["isCaptivePortalEnabled"] = json!(true);
        let mut desired = before.clone();
        desired["type"] = json!("employee");
        desired["isGuestPortalEnabled"] = json!(false);
        desired["isCaptivePortalEnabled"] = json!(false);
        let mut readback = desired.clone();
        if !verified {
            readback["isCaptivePortalEnabled"] = json!(true);
        }
        let server = MockServer::start(update_replies(before, readback, vec![]));
        let client = test_client(&server);
        let (backend, plan) = WlanMutation::update(
            &client,
            SITE,
            SSID_ID,
            Patch {
                guest: Some(false),
                advanced: AdvancedPatch {
                    captive_portal: Some(false),
                    ..AdvancedPatch::default()
                },
                ..Patch::default()
            },
        )
        .await
        .unwrap();
        let report = apply_once(&backend, &plan, Duration::from_millis(100))
            .await
            .unwrap();
        assert_eq!(
            report.outcome,
            if verified {
                Outcome::Verified
            } else {
                Outcome::Unverified
            }
        );
        let requests = server.finish();
        let writes: Vec<_> = requests.iter().filter(|r| r.method == "PUT").collect();
        assert_eq!(writes.len(), 1);
        assert_eq!(request_json(writes[0]), desired);
        assert!(
            requests
                .iter()
                .all(|r| r.method == "GET" || r.method == "PUT")
        );
    }
}
