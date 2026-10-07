use super::*;
use crate::{
    Error, ErrorKind,
    client::wlan::{Bands, Bandwidth, Day, Patch, Schedule, Security, WlanMutation},
    mutation::{Outcome, apply_once},
    secret::SecretString,
};
use serde_json::{Value, json};
use std::time::Duration;

const SITE: &str = "123e4567-e89b-12d3-a456-426614174000";
const NETWORK_ID: &str = "ssid-opaque-17";
const OTHER_ID: &str = "ssid-opaque-22";
const NETWORK_NAME: &str = "Main Wi-Fi";
const SECRET_OLD: &str = "old-WPA2-secret!";
const SECRET_NEW: &str = "new-WPA2-sentinel!";
const SECRET_CREATE: &str = "create-PSK-sentinel!";

fn network(id: &str, name: &str) -> Value {
    json!({
        "id":id,"isWireless":true,"networkName":name,
        "authentication":"psk","security":"wpa2","preSharedKey":SECRET_OLD,
        "isEnabled":true,"isSsidHidden":false,"type":"employee",
        "isAvailableOn24GHzRadioBand":true,"isAvailableOn5GHzRadioBand":true,
        "isAvailableOn6GHzRadioBand":false,"wirelessClientsCount":0,
        "isAccessRestricted":false,"isInternetAllowed":true,"isIntraSubnetTrafficAllowed":true,
        "vendorExtension":{"owner":"network-team","opaque":[1,"keep"]}
    })
}

fn collection(elements: Vec<Value>) -> Vec<u8> {
    let count = elements.len() as u64;
    serde_json::to_vec(&json!({
        "kind":"networksSummary","totalCount":count,"matchingFilterCount":count,"elements":elements
    }))
    .expect("serialize networks summary")
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

fn merged(mut base: Value, patch: Value) -> Value {
    base.as_object_mut()
        .unwrap()
        .extend(patch.as_object().unwrap().clone());
    base
}

fn update_replies(before: Value, after: Value, ack: Value) -> Vec<Reply> {
    vec![
        Reply::json(200, collection(vec![before])),
        reply_json(ack),
        Reply::json(200, collection(vec![after])),
    ]
}

#[tokio::test]
async fn list_selects_only_wireless_networks_and_redacts_psks_from_read_values() {
    let wired = json!({"id":"wired-1","isWireless":false,"networkName":"Wired uplink"});
    let server = MockServer::start(vec![Reply::json(
        200,
        collection(vec![network(NETWORK_ID, NETWORK_NAME), wired]),
    )]);
    let api = make_client(&server, Duration::from_secs(1));
    let networks = crate::client::wlan::list(&api, SITE).await.unwrap();
    assert_eq!(networks.len(), 1);
    assert_eq!(networks[0].id(), NETWORK_ID);
    assert_eq!(networks[0].name(), Some(NETWORK_NAME));
    assert_eq!(networks[0].summary()["name"], NETWORK_NAME);
    assert_eq!(networks[0].summary()["clients"], 0);
    let details = networks[0].details();
    assert_eq!(details["preSharedKey"], "(redacted)");
    let printed = format!("{:?} {}", networks[0], details);
    assert!(!printed.contains(SECRET_OLD));
    assert_request(
        &server.finish()[0],
        "GET",
        &format!("/api/sites/{SITE}/networksSummary"),
    );
}

#[tokio::test]
async fn select_uses_opaque_id_and_rejects_ambiguous_or_missing_names() {
    let server = MockServer::start(vec![Reply::json(
        200,
        collection(vec![
            network(NETWORK_ID, NETWORK_NAME),
            network(OTHER_ID, NETWORK_NAME),
        ]),
    )]);
    let api = make_client(&server, Duration::from_secs(1));
    let rows = crate::client::wlan::list(&api, SITE).await.unwrap();
    assert_eq!(
        crate::client::wlan::select(&rows, NETWORK_ID).unwrap().id(),
        NETWORK_ID
    );
    assert_eq!(
        crate::client::wlan::select(&rows, NETWORK_NAME)
            .unwrap_err()
            .kind,
        ErrorKind::Usage
    );
    assert_eq!(
        crate::client::wlan::select(&rows, "missing")
            .unwrap_err()
            .kind,
        ErrorKind::NotFound
    );
    assert_eq!(server.finish().len(), 1);
}

#[tokio::test]
async fn updates_cover_configuration_fields_and_preserve_unowned_wire_fields() {
    let cases = vec![
        (
            Patch {
                name: Some("Renamed Wi-Fi".into()),
                ..Patch::default()
            },
            json!({"networkName":"Renamed Wi-Fi"}),
        ),
        (
            Patch {
                enabled: Some(false),
                ..Patch::default()
            },
            json!({"isEnabled":false}),
        ),
        (
            Patch {
                enabled: Some(true),
                ..Patch::default()
            },
            json!({"isEnabled":true}),
        ),
        (
            Patch {
                hidden: Some(true),
                ..Patch::default()
            },
            json!({"isSsidHidden":true}),
        ),
        (
            Patch {
                security: Some(Security::Wpa3Personal),
                passphrase: Some(SecretString::new(SECRET_NEW)),
                ..Patch::default()
            },
            json!({"authentication":"psk","security":"wpa3","preSharedKey":SECRET_NEW}),
        ),
        (
            Patch {
                passphrase: Some(SecretString::new(SECRET_NEW)),
                ..Patch::default()
            },
            json!({"preSharedKey":SECRET_NEW}),
        ),
        (
            Patch {
                bands: Some(Bands {
                    two_four: false,
                    five: true,
                    six: false,
                }),
                ..Patch::default()
            },
            json!({"isAvailableOn24GHzRadioBand":false,"isAvailableOn5GHzRadioBand":true,"isAvailableOn6GHzRadioBand":false}),
        ),
        (
            Patch {
                schedule: Some(Schedule::Timed {
                    days: vec![Day::Monday, Day::Friday],
                    start: "22:00".into(),
                    end: "06:30".into(),
                }),
                ..Patch::default()
            },
            json!({"activeSchedule":"simple","schedule":{"activeDays":["monday","friday"],"activeTimeRange":{"enabled":true,"startTime":"22:00","endTime":"06:30"}}}),
        ),
        (
            Patch {
                bandwidth: Some(Bandwidth::PerClient {
                    download: 80,
                    upload: 20,
                }),
                ..Patch::default()
            },
            json!({"isBandwidthLimitEnabled":true,"bandwidthLimitMode":"perClient","perClientBandwidthLimitInMbps":80,"perClientUploadBandwidthLimitInMbps":20}),
        ),
        (
            Patch {
                bandwidth: Some(Bandwidth::PerNetwork {
                    download: 300,
                    upload: 60,
                }),
                ..Patch::default()
            },
            json!({"isBandwidthLimitEnabled":true,"bandwidthLimitMode":"perNetwork","perNetworkDownstreamBandwidthLimitInMbps":300,"perNetworkUpstreamBandwidthLimitInMbps":60}),
        ),
        (
            Patch {
                bandwidth: Some(Bandwidth::Off),
                ..Patch::default()
            },
            json!({"isBandwidthLimitEnabled":false}),
        ),
        (
            Patch {
                guest: Some(true),
                ..Patch::default()
            },
            json!({"type":"guest"}),
        ),
        (
            Patch {
                restrict_access: Some(true),
                internet: Some(false),
                intra_subnet_traffic: Some(false),
                allowed_destinations: Some(vec!["192.0.2.1".into()]),
                ..Patch::default()
            },
            json!({"isAccessRestricted":true,"isInternetAllowed":false,"isIntraSubnetTrafficAllowed":false,"isSpecificDestinationsAllowed":true,"allowedDestinations":["192.0.2.1"]}),
        ),
    ];
    for (patch, fields) in cases {
        let mut before = network(NETWORK_ID, NETWORK_NAME);
        if let Some(enabled) = fields.get("isEnabled").and_then(Value::as_bool) {
            before["isEnabled"] = json!(!enabled);
        }
        let after = merged(before.clone(), fields);
        let server = MockServer::start(update_replies(
            before,
            after.clone(),
            json!({"id":NETWORK_ID,"kind":"wirelessNetwork"}),
        ));
        let api = make_client(&server, Duration::from_secs(1));
        let (backend, plan) = WlanMutation::update(&api, SITE, NETWORK_ID, patch)
            .await
            .unwrap();
        assert_eq!(backend.target()["id"], NETWORK_ID);
        let report = apply_once(&backend, &plan, Duration::from_secs(1))
            .await
            .unwrap();
        assert_eq!(report.outcome, Outcome::Verified);
        let requests = server.finish();
        assert_eq!(requests.len(), 3);
        assert_request(
            &requests[0],
            "GET",
            &format!("/api/sites/{SITE}/networksSummary"),
        );
        assert_request(
            &requests[1],
            "PUT",
            &format!("/api/sites/{SITE}/networksSummary/{NETWORK_ID}"),
        );
        assert_eq!(
            request_json(&requests[1]),
            after,
            "PUT must preserve fetched fields outside the patch"
        );
        assert_request(
            &requests[2],
            "GET",
            &format!("/api/sites/{SITE}/networksSummary"),
        );
    }

    let mut unknown = network(NETWORK_ID, NETWORK_NAME);
    unknown["security"] = json!("vendor-security-mode");
    let after = merged(unknown.clone(), json!({"networkName":"Renamed"}));
    let server = MockServer::start(update_replies(
        unknown,
        after.clone(),
        json!({"id":NETWORK_ID,"kind":"wirelessNetwork"}),
    ));
    let api = make_client(&server, Duration::from_secs(1));
    let (backend, plan) = WlanMutation::update(
        &api,
        SITE,
        NETWORK_ID,
        Patch {
            name: Some("Renamed".into()),
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
    assert_eq!(
        request_json(&server.finish()[1])["security"],
        "vendor-security-mode"
    );
}

#[tokio::test]
async fn validation_and_wireless_selection_fail_before_mutation() {
    let invalid_patches = [
        Patch {
            passphrase: Some(SecretString::new("sh0rt!!")),
            ..Patch::default()
        },
        Patch {
            security: Some(Security::Wpa2Personal),
            bands: Some(Bands {
                two_four: false,
                five: false,
                six: true,
            }),
            ..Patch::default()
        },
        Patch {
            allowed_destinations: Some(vec!["192.0.2.0/24".into()]),
            ..Patch::default()
        },
    ];
    for patch in invalid_patches {
        let server = MockServer::start(vec![]);
        let api = make_client(&server, Duration::from_secs(1));
        let error = expect_error(WlanMutation::update(&api, SITE, NETWORK_ID, patch).await);
        assert_eq!(error.kind, ErrorKind::Usage);
        assert!(!format!("{error:?} {error}").contains("sh0rt!!"));
        assert_eq!(server.finish().len(), 0, "validation must run before HTTP");
    }

    let wired = json!({"id":"wired-1","isWireless":false,"networkName":"Wired uplink"});
    let server = MockServer::start(vec![Reply::json(200, collection(vec![wired]))]);
    let api = make_client(&server, Duration::from_secs(1));
    let error = expect_error(
        WlanMutation::update(
            &api,
            SITE,
            "Wired uplink",
            Patch {
                name: Some("renamed".into()),
                ..Patch::default()
            },
        )
        .await,
    );
    assert_eq!(error.kind, ErrorKind::NotFound);
    assert_eq!(server.finish().len(), 1);
}

#[tokio::test]
async fn malformed_and_partial_network_collections_are_unverified_before_put() {
    for body in [
        json!({"kind":"networksSummary","elements":null}),
        json!({"kind":"networksSummary","elements":[{"id":NETWORK_ID,"isWireless":true,"networkName":NETWORK_NAME}],"totalCount":2,"matchingFilterCount":2}),
        json!({"kind":"networksSummary","elements":[{"id":NETWORK_ID,"isWireless":"yes","networkName":NETWORK_NAME}]}),
    ] {
        let server = MockServer::start(vec![reply_json(body)]);
        let api = make_client(&server, Duration::from_secs(1));
        let error = expect_error(
            WlanMutation::update(
                &api,
                SITE,
                NETWORK_ID,
                Patch {
                    name: Some("renamed".into()),
                    ..Patch::default()
                },
            )
            .await,
        );
        assert_eq!(error.kind, ErrorKind::Unverified);
        assert_eq!(server.finish().len(), 1);
    }
}

#[tokio::test]
async fn foreign_update_ack_is_a_failed_write_even_when_readback_matches() {
    let before = network(NETWORK_ID, NETWORK_NAME);
    let after = merged(before.clone(), json!({"networkName":"Renamed"}));
    let server = MockServer::start(update_replies(
        before,
        after.clone(),
        json!({"id":OTHER_ID,"kind":"wirelessNetwork"}),
    ));
    let api = make_client(&server, Duration::from_secs(1));
    let (backend, plan) = WlanMutation::update(
        &api,
        SITE,
        NETWORK_ID,
        Patch {
            name: Some("Renamed".into()),
            ..Patch::default()
        },
    )
    .await
    .unwrap();
    let report = apply_once(&backend, &plan, Duration::from_secs(1))
        .await
        .unwrap();
    assert_eq!(report.outcome, Outcome::RequestFailedStateMatches);
    assert_eq!(report.error_kind(), Some(ErrorKind::Unverified));
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
async fn failed_update_is_sent_once_even_if_readback_matches() {
    let before = network(NETWORK_ID, NETWORK_NAME);
    let after = merged(before.clone(), json!({"isEnabled":false}));
    let server = MockServer::start(vec![
        Reply::json(200, collection(vec![before])),
        Reply::json(503, b"temporary failure".to_vec()),
        Reply::json(200, collection(vec![after])),
    ]);
    let api = make_client(&server, Duration::from_secs(1));
    let (backend, plan) = WlanMutation::update(
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
    let report = apply_once(&backend, &plan, Duration::from_secs(1))
        .await
        .unwrap();
    assert_eq!(report.outcome, Outcome::RequestFailedStateMatches);
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

#[tokio::test(start_paused = true)]
async fn psk_readback_compares_the_secret_but_redacts_report_and_debug() {
    let _clock = keep_clock_paused().await;
    let before = network(NETWORK_ID, NETWORK_NAME);
    let unchanged = before.clone();
    let server = MockServer::start(update_replies(
        before,
        unchanged,
        json!({"id":NETWORK_ID,"kind":"wirelessNetwork"}),
    ));
    let api = make_client(&server, Duration::from_secs(1));
    let (backend, plan) = WlanMutation::update(
        &api,
        SITE,
        NETWORK_ID,
        Patch {
            passphrase: Some(SecretString::new(SECRET_NEW)),
            ..Patch::default()
        },
    )
    .await
    .unwrap();
    let report = apply_readback_once(&backend, &plan, Duration::from_millis(100))
        .await
        .unwrap();
    assert_eq!(report.outcome, Outcome::Unverified);
    let printed = format!("{report:?} {}", serde_json::to_string(&report).unwrap());
    assert!(!printed.contains(SECRET_OLD));
    assert!(!printed.contains(SECRET_NEW));
    let requests = server.finish();
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.method == "PUT")
            .count(),
        1
    );
    assert_eq!(request_json(&requests[1])["preSharedKey"], SECRET_NEW);
}

fn template() -> Value {
    json!({
        "authentication":"psk","security":"wpa2","networkName":"Default","isEnabled":true,
        "useVlan":false,"isSsidHidden":false,"isWireless":false,"type":"employee",
        "isCaptivePortalEnabled":false,"isBandwidthLimitEnabled":false,
        "ipAddressingMode":"internal","isAvailableOn24GHzRadioBand":true,
        "isAvailableOn5GHzRadioBand":true,"isAvailableOn6GHzRadioBand":false,
        "preSharedKey":"template-secret",
        "dhcpScope":{"network":"172.31.0.0","netmask":"255.255.255.0","ipAddress":"172.31.0.1",
            "ipAddressRange":{"start":"172.31.0.10","end":"172.31.0.200"},
            "ipReservations":[{"ipAddress":"172.31.0.21"},{"ipAddress":"172.17.0.9"}],
            "domainName":"lab.example","dnsServers":["192.0.2.53"]},
        "qos":{"preserve":true}
    })
}

fn wired_template_reply(template: Value) -> Reply {
    reply_json(json!({"metaData":{"defaultWirelessNetwork":template}}))
}

#[tokio::test]
async fn create_uses_template_allocates_first_free_subnet_and_verifies_ack_identity() {
    let weekdays = json!({
        "monday":{"enabled":false,"activeAllDay":true,"startTime":"09:00","endTime":"17:00"},
        "tuesday":{"enabled":false,"activeAllDay":true,"startTime":"09:00","endTime":"17:00"},
        "wednesday":{"enabled":false,"activeAllDay":true,"startTime":"09:00","endTime":"17:00"},
        "thursday":{"enabled":false,"activeAllDay":true,"startTime":"09:00","endTime":"17:00"},
        "friday":{"enabled":false,"activeAllDay":true,"startTime":"09:00","endTime":"17:00"},
        "saturday":{"enabled":false,"activeAllDay":true,"startTime":"09:00","endTime":"17:00"},
        "sunday":{"enabled":false,"activeAllDay":true,"startTime":"09:00","endTime":"17:00"}
    });
    let schedule = json!({
        "activeDays":["monday","tuesday","wednesday","thursday","friday"],
        "activeTimeRange":{"enabled":false}
    });
    let qos = json!({
        "isBandwidthLimitEnabled":false,
        "isDownloadBandwidthLimitEnabled":false,
        "isUploadBandwidthLimitEnabled":false,
        "isTrafficPriorityEnabled":false,
        "downloadBandwidthLimitInMbps":1000,
        "uploadBandwidthLimitInMbps":1000,
        "trafficPriority":"medium",
        "bandwidthLimitMode":"perClient",
        "perClientDownloadBandwidthLimitInMbps":1000,
        "perClientUploadBandwidthLimitInMbps":1000,
        "perNetworkDownstreamBandwidthLimitInMbps":1000,
        "perNetworkUpstreamBandwidthLimitInMbps":1000
    });
    let access_points = json!([{
        "deviceId":"02:00:00:00:00:01","deviceModel":"AP-505",
        "deviceName":"Synthetic AP","isBoundToNetwork":true,
        "radioBandMapping":"2.4ghz_and_5ghz","enabledRadioBands":["2.4ghz","5ghz"]
    }]);
    let dhcp = json!({
        "network":"172.17.0.0","netmask":"255.255.255.0",
        "ipReservations":[{"ipAddress":"172.17.0.9"}],"domainName":null,
        "dns":{"kind":"dns","dnsServerAssignationMode":"automatic",
            "customPrimaryDns":"192.0.2.53","customSecondaryDns":"192.0.2.54",
            "automaticPrimaryDns":"192.0.2.1","automaticSecondaryDns":"192.0.2.2"}
    });
    let source_template =
        |mode: &str| {
            let scope = json!({
                "ipAddress":null,"network":"172.31.0.0","netmask":"255.255.255.0",
                "ipAddressRange":{"start":"172.31.0.10","end":"172.31.0.200"},
                "ipReservations":[{"ipAddress":"172.31.0.21"},{"ipAddress":"172.17.0.9"}],
                "domainName":null,
                "dns":{"kind":"dns","dnsServerAssignationMode":"automatic",
                    "customPrimaryDns":"192.0.2.53","customSecondaryDns":"192.0.2.54",
                    "automaticPrimaryDns":"192.0.2.1","automaticSecondaryDns":"192.0.2.2"}
            });
            let template_qos = json!({
                "isBandwidthLimitEnabled":null,"isDownloadBandwidthLimitEnabled":null,
                "isUploadBandwidthLimitEnabled":null,"isTrafficPriorityEnabled":null,
                "downloadBandwidthLimitInMbps":1000,"uploadBandwidthLimitInMbps":1000,
                "trafficPriority":"medium","bandwidthLimitMode":"perClient",
                "perClientDownloadBandwidthLimitInMbps":1000,
                "perClientUploadBandwidthLimitInMbps":1000,
                "perNetworkDownstreamBandwidthLimitInMbps":1000,
                "perNetworkUpstreamBandwidthLimitInMbps":1000
            });
            let mut template = json!({
                "id":null,"networkName":"","isEnabled":true,"isSsidHidden":false,
                "preSharedKey":"template-psk-sentinel","schedule":null,"weekSchedule":null,
                "activeSchedule":null,"qos":template_qos,
                "isBandwidthLimitEnabled":null,"bandwidthLimitMode":"perClient",
                "perClientBandwidthLimitInMbps":1000,"perClientUploadBandwidthLimitInMbps":1000,
                "perNetworkDownstreamBandwidthLimitInMbps":1000,
                "perNetworkUpstreamBandwidthLimitInMbps":1000,
                "isWireless":true,"type":"employee","authentication":"psk","security":"wpa3",
                "isCaptivePortalEnabled":false,"isGuestPortalEnabled":false,"useVlan":false,
                "vlanId":null,
                "ipAddressingMode":mode,"isAvailableOn24GHzRadioBand":true,
                "isAvailableOn5GHzRadioBand":true,"isAvailableOn6GHzRadioBand":false
            });
            template.as_object_mut().unwrap().extend(json!({
            "isLegacy80211bRatesEnabled":null,"isHighEfficiency11axEnabled":true,
            "isHighEfficiency11axOfdmaEnabled":false,
            "isExtremelyHighThroughput11beEnabled":false,
            "isExtremelyHighThroughput11beMloEnabled":false,
            "isDynamicMulticastOptimizationEnabled":false,
            "isBroadcastOnAllBoundApsOnAllBands":true,"accessPoints":access_points,
            "isAccessRestricted":false,"isInternetAllowed":true,
            "isIntraSubnetTrafficAllowed":false,"isSpecificDestinationsAllowed":null,
            "allowedDestinations":[],"radiusProfileId":null,"isRadiusAccountingEnabled":null,
            "radiusServerPrimary":null,"isSecondaryRadiusServerEnabled":null,
            "radiusServerSecondary":null,"radiusNasIdentifier":null,
            "radiusNasIpSettings":{"useNasIpAddress":false,"nasIpAddress":null},
            "dhcpScope":scope,
            "allowList":{"id":null,"allowListState":null,"isAllowListEnabled":null}
        }).as_object().unwrap().clone());
            if mode == "network" {
                template["wiredNetworkId"] = json!("wired-id-synthetic");
            }
            template
        };
    let mut expected_common = json!({
        "networkName":"","isEnabled":true,"isSsidHidden":false,
        "preSharedKey":SECRET_CREATE,"schedule":schedule,
        "weekSchedule":{"schedulePerWeekdayMap":weekdays},"activeSchedule":"none",
        "qos":qos,"isBandwidthLimitEnabled":false,
        "isWireless":true,"type":"employee","authentication":"psk","security":"wpa2",
        "isCaptivePortalEnabled":false,"useVlan":false,"vlanId":null,
        "wiredNetworkId":"wired-id-synthetic","ipAddressingMode":"network",
        "isAvailableOn24GHzRadioBand":true,"isAvailableOn5GHzRadioBand":true,
        "isAvailableOn6GHzRadioBand":false,"isLegacy80211bRatesEnabled":false
    });
    expected_common.as_object_mut().unwrap().extend(
        json!({
            "isHighEfficiency11axEnabled":true,"isHighEfficiency11axOfdmaEnabled":false,
            "isExtremelyHighThroughput11beEnabled":false,
            "isExtremelyHighThroughput11beMloEnabled":false,
            "isDynamicMulticastOptimizationEnabled":false,
            "isBroadcastOnAllBoundApsOnAllBands":true,"accessPoints":access_points,
            "isAccessRestricted":false,"isInternetAllowed":true,
            "isIntraSubnetTrafficAllowed":false,"isSpecificDestinationsAllowed":false,
            "allowedDestinations":[]
        })
        .as_object()
        .unwrap()
        .clone(),
    );
    let expected_internal = {
        let mut expected = expected_common.clone();
        expected["networkName"] = json!("Internal Wi-Fi");
        expected["ipAddressingMode"] = json!("internal");
        expected.as_object_mut().unwrap().remove("wiredNetworkId");
        expected["dhcpScope"] = dhcp.clone();
        expected
    };
    let expected_bridged = {
        let mut expected = expected_common;
        expected["networkName"] = json!("Bridged Wi-Fi");
        expected["isEnabled"] = json!(false);
        expected["isSsidHidden"] = json!(true);
        expected
    };
    let cases = [
        (
            "Internal Wi-Fi",
            "internal",
            Patch {
                name: Some("Internal Wi-Fi".into()),
                security: Some(Security::Wpa2Personal),
                passphrase: Some(SecretString::new(SECRET_CREATE)),
                ..Patch::default()
            },
            expected_internal,
            json!({
                "id":"created-internal","networkName":"Internal Wi-Fi","isWireless":true,
                "isEnabled":true,"isSsidHidden":false,"type":"employee",
                "authentication":"psk","security":"wpa2","preSharedKey":SECRET_CREATE,
                "ipAddressingMode":"internal","isAvailableOn24GHzRadioBand":true,
                "isAvailableOn5GHzRadioBand":true,"isAvailableOn6GHzRadioBand":false,
                "dhcpScope":{"network":"172.17.0.0","netmask":"255.255.255.0",
                    "ipReservations":[{"ipAddress":"172.17.0.9"}],"domainName":null,
                    "dns":{"kind":"dns","dnsServerAssignationMode":"automatic",
                        "customPrimaryDns":"192.0.2.53","customSecondaryDns":"192.0.2.54",
                        "automaticPrimaryDns":"192.0.2.1","automaticSecondaryDns":"192.0.2.2"}},
                "wirelessClientsCount":0
            }),
        ),
        (
            "Bridged Wi-Fi",
            "network",
            Patch {
                name: Some("Bridged Wi-Fi".into()),
                enabled: Some(false),
                hidden: Some(true),
                security: Some(Security::Wpa2Personal),
                passphrase: Some(SecretString::new(SECRET_CREATE)),
                ..Patch::default()
            },
            expected_bridged,
            json!({
                "id":"created-bridged","networkName":"Bridged Wi-Fi","isWireless":true,
                "isEnabled":false,"isSsidHidden":true,"type":"employee",
                "authentication":"psk","security":"wpa2","preSharedKey":SECRET_CREATE,
                "wiredNetworkId":"wired-id-synthetic","ipAddressingMode":"network",
                "isAvailableOn24GHzRadioBand":true,"isAvailableOn5GHzRadioBand":true,
                "isAvailableOn6GHzRadioBand":false,"wirelessClientsCount":0
            }),
        ),
    ];
    let reserved = json!({
        "managementIpSubnets":[{"network":"172.16.0.0","netmask":"255.255.255.0"}],
        "wanIpSubnets":[],"vpnIpSubnets":[],"defaultLocalDhcpSubnets":[],
        "configuredLocalDhcpSubnets":[],"domainIpSubnets":[]
    });

    for (name, mode, patch, expected_post, readback) in cases {
        let mut replies = vec![
            Reply::json(200, collection(vec![])),
            wired_template_reply(source_template(mode)),
        ];
        if mode == "internal" {
            replies.push(reply_json(reserved.clone()));
        }
        let id = readback["id"].as_str().unwrap();
        replies.push(reply_json(json!({"id":id,"kind":"wirelessNetwork"})));
        replies.push(Reply::json(200, collection(vec![readback])));
        let server = MockServer::start(replies);
        let api = make_client(&server, Duration::from_secs(1));
        let (backend, plan) = WlanMutation::create(&api, SITE, patch).await.unwrap();
        assert_eq!(backend.target()["name"], name);
        assert_eq!(
            serde_json::to_value(&plan.current).unwrap()["exists"],
            false
        );
        let report = apply_once(&backend, &plan, Duration::from_secs(1))
            .await
            .unwrap();
        assert_eq!(report.outcome, Outcome::Verified, "{name}");

        let requests = server.finish();
        let post_index = if mode == "internal" { 3 } else { 2 };
        assert_eq!(requests.len(), post_index + 2, "{name}");
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
        if mode == "internal" {
            assert_request(
                &requests[2],
                "GET",
                &format!("/api/sites/{SITE}/reservedIpSubnets"),
            );
        }
        assert_request(
            &requests[post_index],
            "POST",
            &format!("/api/sites/{SITE}/networksSummary"),
        );
        assert_eq!(
            request_json(&requests[post_index]),
            expected_post,
            "{name} wire body"
        );
        assert_request(
            &requests[post_index + 1],
            "GET",
            &format!("/api/sites/{SITE}/networksSummary"),
        );
    }
}

#[tokio::test]
async fn create_rejects_missing_template_and_redacts_secret_state() {
    let server = MockServer::start(vec![
        Reply::json(200, collection(vec![])),
        reply_json(json!({"metaData":{}})),
    ]);
    let api = make_client(&server, Duration::from_secs(1));
    let error = expect_error(
        WlanMutation::create(
            &api,
            SITE,
            Patch {
                name: Some("New Wi-Fi".into()),
                passphrase: Some(SecretString::new(SECRET_CREATE)),
                ..Patch::default()
            },
        )
        .await,
    );
    assert_eq!(error.kind, ErrorKind::Unverified);
    assert!(!format!("{error:?} {error}").contains(SECRET_CREATE));
    assert_eq!(server.finish().len(), 2);

    let server = MockServer::start(vec![
        Reply::json(200, collection(vec![])),
        wired_template_reply(template()),
        reply_json(json!({})),
        Reply::json(200, collection(vec![])),
    ]);
    let api = make_client(&server, Duration::from_secs(1));
    let (_backend, plan) = WlanMutation::create(
        &api,
        SITE,
        Patch {
            name: Some("New Wi-Fi".into()),
            passphrase: Some(SecretString::new(SECRET_CREATE)),
            ..Patch::default()
        },
    )
    .await
    .unwrap();
    let debug = format!("{plan:?}");
    let serialized = serde_json::to_string(&plan).unwrap();
    assert!(!debug.contains(SECRET_CREATE));
    assert!(!debug.contains("template-secret"));
    assert!(!serialized.contains(SECRET_CREATE));
    assert!(!serialized.contains("template-secret"));
}

#[tokio::test]
async fn create_requires_valid_ack_identity_before_claiming_success() {
    let mut body = template();
    body["networkName"] = json!("New Wi-Fi");
    body["isWireless"] = json!(true);
    body["ipAddressingMode"] = json!("network");
    body.as_object_mut().unwrap().remove("dhcpScope");
    let mut created = body.clone();
    created["id"] = json!("created-ssid");
    created["wirelessClientsCount"] = json!(0);
    let server = MockServer::start(vec![
        Reply::json(200, collection(vec![])),
        wired_template_reply(body),
        reply_json(json!({"kind":"wirelessNetwork"})),
        Reply::json(200, collection(vec![created])),
    ]);
    let api = make_client(&server, Duration::from_secs(1));
    let (backend, plan) = WlanMutation::create(
        &api,
        SITE,
        Patch {
            name: Some("New Wi-Fi".into()),
            ..Patch::default()
        },
    )
    .await
    .unwrap();
    let report = apply_once(&backend, &plan, Duration::from_secs(1))
        .await
        .unwrap();
    assert_eq!(report.outcome, Outcome::RequestFailedStateMatches);
    assert_eq!(report.error_kind(), Some(ErrorKind::Unverified));
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

#[tokio::test]
async fn delete_requires_yes_for_clients_or_unknown_count_then_verifies_absence() {
    for count in [json!(3), Value::Null] {
        let mut occupied = network(NETWORK_ID, NETWORK_NAME);
        if !count.is_null() {
            occupied["wirelessClientsCount"] = count;
        } else {
            occupied
                .as_object_mut()
                .unwrap()
                .remove("wirelessClientsCount");
        }
        let server = MockServer::start(vec![Reply::json(200, collection(vec![occupied]))]);
        let api = make_client(&server, Duration::from_secs(1));
        let error = expect_error(WlanMutation::delete(&api, SITE, NETWORK_ID, false).await);
        assert_eq!(error.kind, ErrorKind::Usage);
        assert_eq!(
            server.finish().len(),
            1,
            "delete guard must reject before DELETE"
        );
    }

    let mut empty = network(NETWORK_ID, NETWORK_NAME);
    empty["wirelessClientsCount"] = json!(0);
    let server = MockServer::start(vec![
        Reply::json(200, collection(vec![empty])),
        Reply::empty(204),
        Reply::json(200, collection(vec![])),
    ]);
    let api = make_client(&server, Duration::from_secs(1));
    let (backend, plan) = WlanMutation::delete(&api, SITE, NETWORK_ID, false)
        .await
        .unwrap();
    assert_eq!(backend.target()["id"], NETWORK_ID);
    let report = apply_once(&backend, &plan, Duration::from_secs(1))
        .await
        .unwrap();
    assert_eq!(report.outcome, Outcome::Verified);
    let requests = server.finish();
    assert_eq!(requests.len(), 3);
    assert_request(
        &requests[1],
        "DELETE",
        &format!("/api/sites/{SITE}/networksSummary/{NETWORK_ID}"),
    );
}

#[tokio::test(start_paused = true)]
async fn delete_with_yes_allows_unknown_count_but_persistent_presence_is_unverified() {
    let _clock = keep_clock_paused().await;
    let mut unknown_count = network(NETWORK_ID, NETWORK_NAME);
    unknown_count
        .as_object_mut()
        .unwrap()
        .remove("wirelessClientsCount");
    let server = MockServer::start(vec![
        Reply::json(200, collection(vec![unknown_count.clone()])),
        Reply::empty(204),
        Reply::json(200, collection(vec![unknown_count])),
    ]);
    let api = make_client(&server, Duration::from_secs(1));
    let (backend, plan) = WlanMutation::delete(&api, SITE, NETWORK_ID, true)
        .await
        .unwrap();
    let report = apply_readback_once(&backend, &plan, Duration::from_millis(100))
        .await
        .unwrap();
    assert_eq!(report.outcome, Outcome::Unverified);
    assert_eq!(report.error_kind(), Some(ErrorKind::Unverified));
    let requests = server.finish();
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.method == "DELETE")
            .count(),
        1
    );
}

#[tokio::test(start_paused = true)]
async fn a_network_type_change_cannot_be_verified_as_deletion() {
    let _clock = keep_clock_paused().await;
    let mut changed = network(NETWORK_ID, NETWORK_NAME);
    changed["isWireless"] = json!(false);
    let server = MockServer::start(vec![
        Reply::json(200, collection(vec![network(NETWORK_ID, NETWORK_NAME)])),
        Reply::empty(204),
        Reply::json(200, collection(vec![changed])),
    ]);
    let api = make_client(&server, Duration::from_secs(1));
    let (backend, plan) = WlanMutation::delete(&api, SITE, NETWORK_ID, true)
        .await
        .unwrap();
    let report = apply_readback_once(&backend, &plan, Duration::from_millis(200))
        .await
        .unwrap();
    assert_eq!(report.outcome, Outcome::Unverified);
    assert_eq!(report.readback_error.unwrap().kind, ErrorKind::Unverified);
    assert_eq!(
        server
            .finish()
            .iter()
            .filter(|request| request.method == "DELETE")
            .count(),
        1
    );
}

#[tokio::test]
async fn create_ack_cannot_reuse_an_existing_network_identity() {
    let mut template = template();
    template["ipAddressingMode"] = json!("network");
    template.as_object_mut().unwrap().remove("dhcpScope");
    let mut observed = template.clone();
    observed["id"] = json!(NETWORK_ID);
    observed["networkName"] = json!("New Wi-Fi");
    observed["isWireless"] = json!(true);
    let server = MockServer::start(vec![
        Reply::json(200, collection(vec![network(NETWORK_ID, NETWORK_NAME)])),
        wired_template_reply(template),
        reply_json(json!({"id":NETWORK_ID})),
        Reply::json(200, collection(vec![observed])),
    ]);
    let api = make_client(&server, Duration::from_secs(1));
    let (backend, plan) = WlanMutation::create(
        &api,
        SITE,
        Patch {
            name: Some("New Wi-Fi".into()),
            ..Patch::default()
        },
    )
    .await
    .unwrap();
    let report = apply_once(&backend, &plan, Duration::from_secs(1))
        .await
        .unwrap();
    assert_eq!(report.outcome, Outcome::RequestFailedStateMatches);
    assert_eq!(report.request_error.unwrap().kind, ErrorKind::Unverified);
    assert_eq!(
        server
            .finish()
            .iter()
            .filter(|request| request.method == "POST")
            .count(),
        1
    );
}

#[tokio::test]
async fn planning_update_and_create_never_send_writes() {
    let server = MockServer::start(vec![Reply::json(
        200,
        collection(vec![network(NETWORK_ID, NETWORK_NAME)]),
    )]);
    let api = make_client(&server, Duration::from_secs(1));
    let (_backend, plan) = WlanMutation::update(
        &api,
        SITE,
        NETWORK_ID,
        Patch {
            passphrase: Some(SecretString::new(SECRET_NEW)),
            ..Patch::default()
        },
    )
    .await
    .unwrap();
    assert!(!format!("{plan:?}").contains(SECRET_NEW));
    let requests = server.finish();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "GET");

    let server = MockServer::start(vec![
        Reply::json(200, collection(vec![])),
        wired_template_reply(template()),
        reply_json(json!({})),
    ]);
    let api = make_client(&server, Duration::from_secs(1));
    let (_backend, _plan) = WlanMutation::create(
        &api,
        SITE,
        Patch {
            name: Some("Plan Only".into()),
            enabled: Some(false),
            hidden: Some(true),
            ..Patch::default()
        },
    )
    .await
    .unwrap();
    let requests = server.finish();
    assert_eq!(requests.len(), 3);
    assert!(requests.iter().all(|request| request.method == "GET"));
}

#[tokio::test(start_paused = true)]
async fn failed_create_and_delete_requests_are_never_retried() {
    let _clock = keep_clock_paused().await;
    let mut create_template = template();
    create_template["ipAddressingMode"] = json!("network");
    create_template.as_object_mut().unwrap().remove("dhcpScope");
    let create_server = MockServer::start(vec![
        Reply::json(200, collection(vec![])),
        wired_template_reply(create_template),
        Reply::json(503, b"create unavailable".to_vec()),
        Reply::json(200, collection(vec![])),
    ]);
    let api = make_client(&create_server, Duration::from_secs(1));
    let (create, create_plan) = WlanMutation::create(
        &api,
        SITE,
        Patch {
            name: Some("New Wi-Fi".into()),
            ..Patch::default()
        },
    )
    .await
    .unwrap();
    let create_report = apply_readback_once(&create, &create_plan, Duration::from_millis(100))
        .await
        .unwrap();
    assert_eq!(create_report.outcome, Outcome::Failed);
    assert_eq!(create_report.error_kind(), Some(ErrorKind::General));
    let requests = create_server.finish();
    assert_eq!(requests.len(), 4);
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.method == "POST")
            .count(),
        1
    );

    let mut empty = network(NETWORK_ID, NETWORK_NAME);
    empty["wirelessClientsCount"] = json!(0);
    let delete_server = MockServer::start(vec![
        Reply::json(200, collection(vec![empty.clone()])),
        Reply::json(503, b"delete unavailable".to_vec()),
        Reply::json(200, collection(vec![empty])),
    ]);
    let api = make_client(&delete_server, Duration::from_secs(1));
    let (delete, delete_plan) = WlanMutation::delete(&api, SITE, NETWORK_ID, false)
        .await
        .unwrap();
    let delete_report = apply_readback_once(&delete, &delete_plan, Duration::from_millis(100))
        .await
        .unwrap();
    assert_eq!(delete_report.outcome, Outcome::Failed);
    assert_eq!(delete_report.error_kind(), Some(ErrorKind::General));
    let requests = delete_server.finish();
    assert_eq!(requests.len(), 3);
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.method == "DELETE")
            .count(),
        1
    );
}
