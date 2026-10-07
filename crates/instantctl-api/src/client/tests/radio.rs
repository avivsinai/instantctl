use super::clock::{apply_readback_once, keep_clock_paused};
use super::*;
use crate::{
    client::radio::{self, Band, BandMapping, Patch, Power, Width},
    mutation::Outcome,
};
use serde_json::{Value, json};

const SITE: &str = "12345678-1234-5678-1234-567812345678";
const AP_ID: &str = "opaque-ap-id-27";
const AP_MAC: &str = "02:00:00:00:00:27";

fn config(channels: Value) -> Value {
    config_width(channels, "80mhz")
}

fn config_width(channels: Value, width: &str) -> Value {
    json!({"channelWidth":width,"channels":channels,
        "minTxPower":"18dbm","maxTxPower":"24dbm",
        "futureConfig":{"keep":[1,null,"opaque"]}})
}

fn offered(width: &str, channels: &[&str]) -> Value {
    json!({"channelWidth":width,"channels":channels,
        "futureOffer":{"retain":true}})
}

fn site_radio() -> Value {
    json!({
        "kind":"radioManagement",
        "radioBandMapping":"2.4ghz_and_5ghz",
        "radioBandMappingDeviceCurrentlyPresent":true,
        "radios":{
            "2.4ghz":{"configuration":config_width(json!([1,6,11]), "20mhz"),
                "drtAvailableChannels":[offered("20mhz", &["1","6","11"])]},
            "5ghz":{"configuration":config(json!(["36","40"])),
                "drtAvailableChannels":[
                    offered("80mhz", &["36","40","149"]),
                    offered("160mhz", &["36","149"])]},
            "6ghz":{"configuration":config(json!(["5","21"])),
                "drtAvailableChannels":[offered("80mhz", &["5","21"]),
                    offered("160mhz", &["5","21"]),offered("320mhz", &["5"])]}
        },
        "opaqueSiteField":{"secret":"opaque-site-secret","retain":true}
    })
}

fn ap_radio(name: &str) -> Value {
    json!({
        "id":AP_ID,"name":name,"kind":"accessPoint","deviceType":"accessPoint",
        "isUnderpowered":false,"parentId":null,
        "macAddress":AP_MAC,
        "radioBandMapping":"2.4ghz_and_5ghz",
        "globalRadioBandMapping":"2.4ghz_and_5ghz",
        "useDeviceRadioBandMapping":false,
        "radioManagementBands":{
            "2.4ghz":{"radioManagementBand":{"configuration":config_width(json!(["1","6"]), "20mhz"),
                "drtAvailableChannels":[offered("20mhz", &["1","6","11"])]},
                "useDeviceRadioManagementConfig":false,
                "isBroadcastingOfNetworksEnabled":true,
                "futureBandField":{"keep":true}},
            "5ghz":{"radioManagementBand":{"configuration":config(json!(["149"])),
                "drtAvailableChannels":[offered("80mhz", &["149"]),
                    offered("160mhz", &["149"])]},
                "useDeviceRadioManagementConfig":false,
                "isBroadcastingOfNetworksEnabled":false},
            "6ghz":{"radioManagementBand":{"configuration":config(json!(["5"])),
                "drtAvailableChannels":[offered("80mhz", &["5","21"]),
                    offered("160mhz", &["5"]),offered("320mhz", &["5"])]},
                "useDeviceRadioManagementConfig":true,
                "isBroadcastingOfNetworksEnabled":true}
        },
        "capabilities":{"has":{"wifi6E":true,"channelBandwidth160Mhz":true,
            "channelBandwidth160MhzOn6Ghz":true,"channelBandwidth320Mhz":true,
            "radioBandMapping":false}},
        "vendorExtension":{"secret":"opaque-ap-secret","array":[1,null,"retain"]}
    })
}

fn inventory(elements: Vec<Value>) -> Reply {
    let count = elements.len() as u64;
    Reply::json(
        200,
        serde_json::to_vec(&json!({
            "kind":"resourceList","totalCount":count,"matchingFilterCount":count,
            "pendingAvailability":false,"elements":elements
        }))
        .unwrap(),
    )
}

fn capabilities(names: &[&str]) -> Reply {
    Reply::json(
        200,
        serde_json::to_vec(&json!({"capabilities":names})).unwrap(),
    )
}

fn json_reply(value: &Value) -> Reply {
    Reply::json(200, serde_json::to_vec(value).unwrap())
}

fn request_body(request: &Request) -> Value {
    serde_json::from_slice(&request.body).expect("request body is JSON")
}

fn test_client(server: &MockServer) -> Client {
    make_client(server, Duration::from_secs(2))
}

#[tokio::test]
async fn site_plan_exposes_only_known_rf_fields_and_keeps_wire_channel_types() {
    let before = site_radio();
    let server = MockServer::start(vec![json_reply(&before)]);
    let actual = radio::site_plan(&test_client(&server), SITE).await.unwrap();
    assert_eq!(actual["radioBandMapping"], "2.4ghz_and_5ghz");
    assert_eq!(
        actual["radios"]["5ghz"]["configuration"]["channels"],
        json!(["36", "40"])
    );
    assert_eq!(
        actual["radios"]["5ghz"]["drtAvailableChannels"][0]["channels"],
        json!(["36", "40", "149"])
    );
    assert!(actual.get("opaqueSiteField").is_none());
    assert_eq!(
        server.finish()[0].target,
        format!("/api/sites/{SITE}/radioManagement")
    );
}

#[tokio::test]
async fn ap_override_selects_by_mac_and_uses_opaque_device_id_for_write_route() {
    let before = ap_radio("pergola");
    let server = MockServer::start(vec![inventory(vec![before])]);
    let actual = radio::ap_override(&test_client(&server), SITE, "02:00:00:00:00:27")
        .await
        .unwrap();
    assert_eq!(actual["id"], AP_ID);
    assert_eq!(
        actual["radios"]["5ghz"]["useDeviceRadioManagementConfig"],
        false
    );
    assert_eq!(
        actual["radios"]["5ghz"]["isBroadcastingOfNetworksEnabled"],
        false
    );
    assert_eq!(
        actual["radios"]["5ghz"]["configuration"]["channels"],
        json!(["149"])
    );
    let requests = server.finish();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].target, format!("/api/sites/{SITE}/inventory"));
}

#[tokio::test(start_paused = true)]
async fn site_full_put_changes_requested_fields_and_verifies_them_only() {
    let _clock = keep_clock_paused().await;
    let before = site_radio();
    let mut desired_body = before.clone();
    desired_body["radios"]["5ghz"]["configuration"]["channelWidth"] = json!("160mhz");
    desired_body["radios"]["5ghz"]["configuration"]["channels"] = json!(["36", "149"]);
    desired_body["radios"]["5ghz"]["configuration"]["minTxPower"] = json!("21dbm");
    desired_body["radios"]["5ghz"]["configuration"]["maxTxPower"] = json!("27dbm");
    let mut readback = desired_body.clone();
    readback["opaqueSiteField"]["retain"] = json!(false);
    let server = MockServer::start(vec![
        json_reply(&before),
        capabilities(&["channel-bandwidth-160mhz"]),
        Reply::empty(204),
        json_reply(&readback),
    ]);
    let client = test_client(&server);
    let prepared = radio::set_site(
        &client,
        SITE,
        Patch {
            band: Some(Band::Ghz5),
            width: Some(Width::Mhz160),
            channels: Some(vec![36, 149]),
            min_power: Some(Power::Dbm21),
            max_power: Some(Power::Dbm27),
            ..Patch::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        prepared.plan.current["/radios/5ghz/configuration/channelWidth"],
        "80mhz"
    );
    assert_eq!(
        prepared.plan.desired["/radios/5ghz/configuration/channels"],
        json!(["36", "149"])
    );
    let report = apply_readback_once(&prepared.backend, &prepared.plan, Duration::from_secs(1))
        .await
        .unwrap();
    assert_eq!(report.outcome, Outcome::Verified);
    let requests = server.finish();
    let put = requests.iter().find(|r| r.method == "PUT").unwrap();
    assert_eq!(request_body(put), desired_body);
    assert_eq!(requests.iter().filter(|r| r.method == "PUT").count(), 1);
}

#[tokio::test(start_paused = true)]
async fn ap_configuration_put_enables_specific_band_and_preserves_all_other_fields() {
    let _clock = keep_clock_paused().await;
    let before = ap_radio("AP27");
    let mut desired_body = before.clone();
    desired_body["radioManagementBands"]["5ghz"]["useDeviceRadioManagementConfig"] = json!(true);
    desired_body["radioManagementBands"]["5ghz"]["radioManagementBand"]["configuration"]["channels"] =
        json!(["149"]);
    let mut readback = desired_body.clone();
    readback["radioManagementBands"]["2.4ghz"]["isBroadcastingOfNetworksEnabled"] = json!(false);
    let server = MockServer::start(vec![
        inventory(vec![before]),
        Reply::empty(204),
        inventory(vec![readback]),
    ]);
    let client = test_client(&server);
    let prepared = radio::set_ap(
        &client,
        SITE,
        "AP27",
        Patch {
            band: Some(Band::Ghz5),
            channels: Some(vec![149]),
            ..Patch::default()
        },
    )
    .await
    .unwrap();
    let report = apply_readback_once(&prepared.backend, &prepared.plan, Duration::from_secs(1))
        .await
        .unwrap();
    assert_eq!(report.outcome, Outcome::Verified);
    let requests = server.finish();
    let put = requests.iter().find(|r| r.method == "PUT").unwrap();
    assert_eq!(put.target, format!("/api/sites/{SITE}/inventory/{AP_ID}"));
    assert_eq!(request_body(put), desired_body);
    assert_eq!(
        request_body(put)["vendorExtension"]["secret"],
        "opaque-ap-secret"
    );
    assert_eq!(requests.iter().filter(|r| r.method == "PUT").count(), 1);
}

#[tokio::test]
async fn ap_config_refuses_a_band_disabled_by_the_inherited_site_mapping() {
    let before = ap_radio("AP27");
    let mut mapping_capable = before.clone();
    mapping_capable["capabilities"]["has"]["radioBandMapping"] = json!(true);
    let server = MockServer::start(vec![
        inventory(vec![mapping_capable]),
        json_reply(&site_radio()),
    ]);
    let error = radio::set_ap(
        &test_client(&server),
        SITE,
        "AP27",
        Patch {
            band: Some(Band::Ghz6),
            channels: Some(vec![21]),
            ..Patch::default()
        },
    )
    .await
    .err()
    .unwrap();
    assert_eq!(error.kind, ErrorKind::Usage);
    let requests = server.finish();
    assert_eq!(requests.len(), 2);
    assert!(requests.iter().all(|request| request.method == "GET"));
    assert_eq!(
        requests[1].target,
        format!("/api/sites/{SITE}/radioManagement")
    );
}

#[tokio::test]
async fn ap_config_uses_current_device_mapping_without_site_get_when_mapping_is_specific() {
    let mut before = ap_radio("AP27");
    before["capabilities"]["has"]["radioBandMapping"] = json!(true);
    before["useDeviceRadioBandMapping"] = json!(true);
    let server = MockServer::start(vec![inventory(vec![before])]);
    let error = radio::set_ap(
        &test_client(&server),
        SITE,
        "AP27",
        Patch {
            band: Some(Band::Ghz6),
            channels: Some(vec![21]),
            ..Patch::default()
        },
    )
    .await
    .err()
    .unwrap();
    assert_eq!(error.kind, ErrorKind::Usage);
    let requests = server.finish();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "GET");
}

#[tokio::test(start_paused = true)]
async fn ap_inherit_mapping_uses_site_mapping_for_radio_configuration() {
    let _clock = keep_clock_paused().await;
    let mut before = ap_radio("AP27");
    before["capabilities"]["has"]["radioBandMapping"] = json!(true);
    before["useDeviceRadioBandMapping"] = json!(true);
    let mut desired = before.clone();
    desired["useDeviceRadioBandMapping"] = json!(false);
    desired["radioManagementBands"]["6ghz"]["radioManagementBand"]["configuration"]["channels"] =
        json!(["21"]);
    let mut site = site_radio();
    site["radioBandMapping"] = json!("2.4ghz_and_6ghz");
    let server = MockServer::start(vec![
        inventory(vec![before]),
        json_reply(&site),
        Reply::empty(204),
        inventory(vec![desired.clone()]),
    ]);
    let client = test_client(&server);
    let prepared = radio::set_ap(
        &client,
        SITE,
        "AP27",
        Patch {
            band: Some(Band::Ghz6),
            channels: Some(vec![21]),
            inherit_mapping: true,
            ..Patch::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(prepared.plan.desired["/useDeviceRadioBandMapping"], false);
    assert_eq!(
        prepared.plan.desired["/radioManagementBands/6ghz/radioManagementBand/configuration/channels"],
        json!(["21"])
    );
    let report = apply_readback_once(&prepared.backend, &prepared.plan, Duration::from_secs(1))
        .await
        .unwrap();
    assert_eq!(report.outcome, Outcome::Verified);
    let requests = server.finish();
    assert_eq!(
        request_body(
            requests
                .iter()
                .find(|request| request.method == "PUT")
                .unwrap()
        ),
        desired
    );
}

#[tokio::test]
async fn ap_specific_config_requires_known_and_sufficient_power_status() {
    for (status, expected) in [
        (Value::Null, ErrorKind::Unverified),
        (json!(true), ErrorKind::Usage),
    ] {
        let mut before = ap_radio("AP27");
        before["isUnderpowered"] = status;
        let server = MockServer::start(vec![inventory(vec![before])]);
        let error = radio::set_ap(
            &test_client(&server),
            SITE,
            "AP27",
            Patch {
                band: Some(Band::Ghz5),
                max_power: Some(Power::Dbm27),
                ..Patch::default()
            },
        )
        .await
        .err()
        .unwrap();
        assert_eq!(error.kind, expected);
        let requests = server.finish();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "GET");
    }
}

#[tokio::test]
async fn mesh_ap_refuses_specific_5_and_6_ghz_config_but_allows_24_ghz() {
    for band in [Band::Ghz5, Band::Ghz6] {
        let mut before = ap_radio("mesh AP");
        before["parentId"] = json!("mesh-parent");
        let server = MockServer::start(vec![inventory(vec![before])]);
        let channel = if band == Band::Ghz5 { 149 } else { 5 };
        let error = radio::set_ap(
            &test_client(&server),
            SITE,
            "mesh AP",
            Patch {
                band: Some(band),
                channels: Some(vec![channel]),
                ..Patch::default()
            },
        )
        .await
        .err()
        .unwrap();
        assert_eq!(error.kind, ErrorKind::Usage);
        let requests = server.finish();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "GET");
    }

    let mut before = ap_radio("mesh AP");
    before["parentId"] = json!("mesh-parent");
    let server = MockServer::start(vec![inventory(vec![before])]);
    let client = test_client(&server);
    let prepared = radio::set_ap(
        &client,
        SITE,
        "mesh AP",
        Patch {
            band: Some(Band::Ghz24),
            channels: Some(vec![6]),
            ..Patch::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        prepared.plan.desired["/radioManagementBands/2.4ghz/radioManagementBand/configuration/channels"],
        json!(["6"])
    );
    assert_eq!(server.finish().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn explicit_ap_mapping_can_enable_a_band_in_the_same_full_object_put() {
    let _clock = keep_clock_paused().await;
    let mut before = ap_radio("AP27");
    before["capabilities"]["has"]["radioBandMapping"] = json!(true);
    let mut desired = before.clone();
    desired["radioBandMapping"] = json!("2.4ghz_and_6ghz");
    desired["useDeviceRadioBandMapping"] = json!(true);
    desired["radioManagementBands"]["6ghz"]["radioManagementBand"]["configuration"]["channels"] =
        json!(["21"]);
    let server = MockServer::start(vec![
        inventory(vec![before]),
        Reply::empty(204),
        inventory(vec![desired.clone()]),
    ]);
    let client = test_client(&server);
    let prepared = radio::set_ap(
        &client,
        SITE,
        "AP27",
        Patch {
            band: Some(Band::Ghz6),
            channels: Some(vec![21]),
            mapping: Some(BandMapping::Ghz24And6),
            ..Patch::default()
        },
    )
    .await
    .unwrap();
    let report = apply_readback_once(&prepared.backend, &prepared.plan, Duration::from_secs(1))
        .await
        .unwrap();
    assert_eq!(report.outcome, Outcome::Verified);
    let requests = server.finish();
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.method == "PUT")
            .count(),
        1
    );
    assert_eq!(
        request_body(
            requests
                .iter()
                .find(|request| request.method == "PUT")
                .unwrap()
        ),
        desired
    );
}

#[tokio::test]
async fn ap_offered_channels_are_band_specific_and_channel_149_is_accepted() {
    let before = ap_radio("AP27");
    for (channel, error) in [(36, true), (149, false)] {
        let server = MockServer::start(vec![inventory(vec![before.clone()])]);
        let client = test_client(&server);
        let result = radio::set_ap(
            &client,
            SITE,
            "AP27",
            Patch {
                band: Some(Band::Ghz5),
                channels: Some(vec![channel]),
                ..Patch::default()
            },
        )
        .await;
        if error {
            assert_eq!(result.err().unwrap().kind, ErrorKind::Usage);
            assert!(server.finish().iter().all(|r| r.method != "PUT"));
        } else {
            let prepared = result.unwrap();
            assert_eq!(
                prepared.plan.desired["/radioManagementBands/5ghz/radioManagementBand/configuration/channels"],
                json!(["149"])
            );
            assert!(server.finish().iter().all(|r| r.method != "PUT"));
        }
    }
}

#[tokio::test(start_paused = true)]
async fn site_mapping_full_put_preserves_opaque_fields_and_uses_site_capability() {
    let _clock = keep_clock_paused().await;
    let before = site_radio();
    let mut desired = before.clone();
    desired["radioBandMapping"] = json!("2.4ghz_and_6ghz");
    let mut readback = desired.clone();
    readback["opaqueSiteField"]["retain"] = json!(false);
    let server = MockServer::start(vec![
        json_reply(&before),
        capabilities(&["radio-band-mapping"]),
        Reply::empty(204),
        json_reply(&readback),
    ]);
    let client = test_client(&server);
    let prepared = radio::set_site(
        &client,
        SITE,
        Patch {
            mapping: Some(BandMapping::Ghz24And6),
            ..Patch::default()
        },
    )
    .await
    .unwrap();
    let report = apply_readback_once(&prepared.backend, &prepared.plan, Duration::from_secs(1))
        .await
        .unwrap();
    assert_eq!(report.outcome, Outcome::Verified);
    let req = server.finish();
    assert_eq!(
        request_body(req.iter().find(|r| r.method == "PUT").unwrap()),
        desired
    );
}

#[tokio::test]
async fn site_mapping_requires_present_marker_and_advertised_capability() {
    for marker in [Value::Null, json!(false)] {
        let mut before = site_radio();
        before["radioBandMappingDeviceCurrentlyPresent"] = marker;
        let server = MockServer::start(vec![json_reply(&before)]);
        let error = radio::set_site(
            &test_client(&server),
            SITE,
            Patch {
                mapping: Some(BandMapping::Ghz24And6),
                ..Patch::default()
            },
        )
        .await
        .err()
        .unwrap();
        assert_eq!(error.kind, ErrorKind::Usage);
        assert!(
            server
                .finish()
                .iter()
                .all(|request| request.method != "PUT")
        );
    }

    let before = site_radio();
    let server = MockServer::start(vec![json_reply(&before), capabilities(&[])]);
    let error = radio::set_site(
        &test_client(&server),
        SITE,
        Patch {
            mapping: Some(BandMapping::Ghz24And6),
            ..Patch::default()
        },
    )
    .await
    .err()
    .unwrap();
    assert_eq!(error.kind, ErrorKind::Usage);
    assert!(
        server
            .finish()
            .iter()
            .all(|request| request.method != "PUT")
    );
}

#[tokio::test(start_paused = true)]
async fn ap_mapping_patch_sets_mapping_and_independent_inheritance_flag() {
    let _clock = keep_clock_paused().await;
    let mut before = ap_radio("AP27");
    before["capabilities"]["has"]["radioBandMapping"] = json!(true);
    let mut desired = before.clone();
    desired["radioBandMapping"] = json!("5ghz_and_6ghz");
    desired["useDeviceRadioBandMapping"] = json!(true);
    let server = MockServer::start(vec![
        inventory(vec![before]),
        Reply::empty(204),
        inventory(vec![desired.clone()]),
    ]);
    let client = test_client(&server);
    let prepared = radio::set_ap(
        &client,
        SITE,
        "AP27",
        Patch {
            mapping: Some(BandMapping::Ghz5And6),
            ..Patch::default()
        },
    )
    .await
    .unwrap();
    let report = apply_readback_once(&prepared.backend, &prepared.plan, Duration::from_secs(1))
        .await
        .unwrap();
    assert_eq!(report.outcome, Outcome::Verified);
    let requests = server.finish();
    assert_eq!(
        request_body(requests.iter().find(|r| r.method == "PUT").unwrap()),
        desired
    );
}

#[tokio::test]
async fn ap_mapping_is_refused_when_device_capability_is_missing_or_false() {
    for capability in [Value::Null, json!(false)] {
        let mut before = ap_radio("AP27");
        before["capabilities"]["has"]["radioBandMapping"] = capability;
        let server = MockServer::start(vec![inventory(vec![before])]);
        let error = radio::set_ap(
            &test_client(&server),
            SITE,
            "AP27",
            Patch {
                mapping: Some(BandMapping::Ghz5And6),
                ..Patch::default()
            },
        )
        .await
        .err()
        .unwrap();
        assert_eq!(error.kind, ErrorKind::Usage);
        assert!(
            server
                .finish()
                .iter()
                .all(|request| request.method != "PUT")
        );
    }
}

#[tokio::test]
async fn ap_mapping_requires_a_constructed_six_ghz_radio_object() {
    for malformed_radio in [Value::Null, json!("not-a-radio-object")] {
        let mut before = ap_radio("AP27");
        before["capabilities"]["has"]["radioBandMapping"] = json!(true);
        before["radioManagementBands"]["6ghz"]["radioManagementBand"] = malformed_radio;
        let server = MockServer::start(vec![inventory(vec![before])]);
        let error = radio::set_ap(
            &test_client(&server),
            SITE,
            "AP27",
            Patch {
                mapping: Some(BandMapping::Ghz5And6),
                ..Patch::default()
            },
        )
        .await
        .err()
        .unwrap();
        assert_eq!(error.kind, ErrorKind::Usage);
        let requests = server.finish();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "GET");
    }
}

#[tokio::test(start_paused = true)]
async fn ap_inherit_mapping_restores_inheritance_without_changing_mapping_value() {
    let _clock = keep_clock_paused().await;
    let mut before = ap_radio("AP27");
    before["useDeviceRadioBandMapping"] = json!(true);
    let mut desired = before.clone();
    desired["useDeviceRadioBandMapping"] = json!(false);
    let server = MockServer::start(vec![
        inventory(vec![before]),
        Reply::empty(204),
        inventory(vec![desired]),
    ]);
    let client = test_client(&server);
    let prepared = radio::set_ap(
        &client,
        SITE,
        "AP27",
        Patch {
            inherit_mapping: true,
            ..Patch::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(prepared.plan.desired.as_object().unwrap().len(), 1);
    let report = apply_readback_once(&prepared.backend, &prepared.plan, Duration::from_secs(1))
        .await
        .unwrap();
    assert_eq!(report.outcome, Outcome::Verified);
    let req = server.finish();
    let body = request_body(req.iter().find(|r| r.method == "PUT").unwrap());
    assert_eq!(body["radioBandMapping"], "2.4ghz_and_5ghz");
    assert_eq!(body["useDeviceRadioBandMapping"], false);
}

#[tokio::test(start_paused = true)]
async fn ap_inherit_config_changes_only_flag_and_keeps_stored_radio_configuration() {
    let _clock = keep_clock_paused().await;
    let before = ap_radio("AP27");
    let mut desired = before.clone();
    desired["radioManagementBands"]["5ghz"]["useDeviceRadioManagementConfig"] = json!(false);
    let server = MockServer::start(vec![
        inventory(vec![before.clone()]),
        Reply::empty(204),
        inventory(vec![desired.clone()]),
    ]);
    let client = test_client(&server);
    let prepared = radio::set_ap(
        &client,
        SITE,
        "AP27",
        Patch {
            band: Some(Band::Ghz5),
            inherit_config: true,
            ..Patch::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(prepared.plan.desired.as_object().unwrap().len(), 1);
    let report = apply_readback_once(&prepared.backend, &prepared.plan, Duration::from_secs(1))
        .await
        .unwrap();
    assert_eq!(report.outcome, Outcome::Verified);
    let req = server.finish();
    let put = request_body(req.iter().find(|r| r.method == "PUT").unwrap());
    assert_eq!(
        put["radioManagementBands"]["5ghz"]["radioManagementBand"]["configuration"],
        before["radioManagementBands"]["5ghz"]["radioManagementBand"]["configuration"]
    );
}

#[tokio::test]
async fn invalid_patches_are_rejected_before_any_http_request() {
    let cases = [
        Patch::default(),
        Patch {
            band: Some(Band::Ghz5),
            channels: Some(vec![]),
            ..Patch::default()
        },
        Patch {
            band: Some(Band::Ghz5),
            channels: Some(vec![36, 36]),
            ..Patch::default()
        },
        Patch {
            band: Some(Band::Ghz5),
            channels: Some(vec![0]),
            ..Patch::default()
        },
        Patch {
            band: Some(Band::Ghz24),
            min_power: Some(Power::Dbm33),
            ..Patch::default()
        },
        Patch {
            band: Some(Band::Ghz5),
            max_power: Some(Power::Dbm12),
            ..Patch::default()
        },
        Patch {
            band: Some(Band::Ghz5),
            min_power: Some(Power::Dbm27),
            max_power: Some(Power::Dbm21),
            ..Patch::default()
        },
        Patch {
            width: Some(Width::Mhz80),
            ..Patch::default()
        },
        Patch {
            inherit_config: true,
            ..Patch::default()
        },
    ];
    for patch in cases {
        let server = MockServer::start(vec![]);
        let client = test_client(&server);
        let result = radio::set_site(&client, SITE, patch).await;
        assert_eq!(result.err().unwrap().kind, ErrorKind::Usage);
        assert!(server.finish().is_empty());
    }
}

#[tokio::test]
async fn missing_band_is_rejected_and_empty_selector_never_reaches_http() {
    let server = MockServer::start(vec![]);
    let error = radio::set_site(
        &test_client(&server),
        SITE,
        Patch {
            width: Some(Width::Mhz80),
            ..Patch::default()
        },
    )
    .await
    .err()
    .unwrap();
    assert_eq!(error.kind, ErrorKind::Usage);
    let error = radio::set_ap(
        &test_client(&server),
        SITE,
        "",
        Patch {
            inherit_mapping: true,
            ..Patch::default()
        },
    )
    .await
    .err()
    .unwrap();
    assert_eq!(error.kind, ErrorKind::Usage);
    assert!(server.finish().is_empty());
}

#[tokio::test]
async fn ambiguous_ap_name_is_usage_and_missing_selector_is_not_found() {
    for (elements, selector, expected) in [
        (
            vec![ap_radio("dup"), {
                let mut other = ap_radio("dup");
                other["id"] = json!("other");
                other["macAddress"] = json!("02:00:00:00:00:28");
                other
            }],
            "dup",
            ErrorKind::Usage,
        ),
        (vec![ap_radio("one")], "missing", ErrorKind::NotFound),
    ] {
        let server = MockServer::start(vec![inventory(elements)]);
        let error = radio::ap_override(&test_client(&server), SITE, selector)
            .await
            .unwrap_err();
        assert_eq!(error.kind, expected);
        assert_eq!(server.finish().len(), 1);
    }
}

#[tokio::test]
async fn malformed_or_partial_ap_inventory_is_unverified() {
    let wrong_kind = json_reply(&json!({"kind":"wrong","elements":[]}));
    let mut item = ap_radio("AP27");
    item["deviceType"] = json!("switch");
    let cases = [
        (
            Reply::json(200, serde_json::to_vec(&json!({"kind":"resourceList","elements":[ap_radio("AP27")],"totalCount":1,"matchingFilterCount":0,"pendingAvailability":false})).unwrap()),
            ErrorKind::Unverified,
        ),
        (wrong_kind, ErrorKind::Unverified),
        (inventory(vec![item]), ErrorKind::Usage),
    ];
    for (reply, expected) in cases {
        let server = MockServer::start(vec![reply]);
        let err = radio::ap_override(&test_client(&server), SITE, "AP27")
            .await
            .unwrap_err();
        assert_eq!(err.kind, expected);
        assert_eq!(server.finish().len(), 1);
    }
}

#[tokio::test]
async fn unknown_and_missing_config_values_read_as_null_and_block_mutation() {
    for bad in [json!("vendor-width"), Value::Null] {
        let mut body = site_radio();
        body["radios"]["5ghz"]["configuration"]["channelWidth"] = bad;
        let server = MockServer::start(vec![json_reply(&body)]);
        let visible = radio::site_plan(&test_client(&server), SITE).await.unwrap();
        assert!(visible["radios"]["5ghz"]["configuration"]["channelWidth"].is_null());
        let _ = server.finish();

        let server = MockServer::start(vec![json_reply(&body)]);
        let error = radio::set_site(
            &test_client(&server),
            SITE,
            Patch {
                band: Some(Band::Ghz5),
                channels: Some(vec![36]),
                ..Patch::default()
            },
        )
        .await
        .err()
        .unwrap();
        assert_eq!(error.kind, ErrorKind::Unverified);
        assert!(
            server
                .finish()
                .iter()
                .all(|request| request.method != "PUT")
        );
    }
}

#[tokio::test]
async fn wide_channel_requires_offered_entry_and_capability_before_any_put() {
    let cases = [
        (vec![], Band::Ghz5, Width::Mhz160, vec![36]),
        (
            vec!["channel-bandwidth-160mhz"],
            Band::Ghz6,
            Width::Mhz160,
            vec![5],
        ),
        (
            vec!["channel-bandwidth-160mhz"],
            Band::Ghz6,
            Width::Mhz320,
            vec![5],
        ),
    ];
    for (caps, band, width, channels) in cases {
        let site = site_radio();
        let server = MockServer::start(vec![json_reply(&site), capabilities(&caps)]);
        let client = test_client(&server);
        let result = radio::set_site(
            &client,
            SITE,
            Patch {
                band: Some(band),
                width: Some(width),
                channels: Some(channels),
                ..Patch::default()
            },
        )
        .await;
        assert_eq!(result.err().unwrap().kind, ErrorKind::Usage);
        assert!(server.finish().iter().all(|r| r.method != "PUT"));
    }
}

#[tokio::test(start_paused = true)]
async fn site_six_ghz_160_and_320_widths_verify_when_their_capabilities_are_offered() {
    let _clock = keep_clock_paused().await;
    for (width, capability, channels) in [
        (Width::Mhz160, "channel-bandwidth-160mhz-on-6ghz", vec![5]),
        (Width::Mhz320, "channel-bandwidth-320mhz", vec![5]),
    ] {
        let before = site_radio();
        let mut desired = before.clone();
        desired["radios"]["6ghz"]["configuration"]["channelWidth"] = json!(width.api_id());
        desired["radios"]["6ghz"]["configuration"]["channels"] =
            json!(channels.iter().map(u16::to_string).collect::<Vec<_>>());
        let server = MockServer::start(vec![
            json_reply(&before),
            capabilities(&[capability]),
            Reply::empty(204),
            json_reply(&desired),
        ]);
        let client = test_client(&server);
        let prepared = radio::set_site(
            &client,
            SITE,
            Patch {
                band: Some(Band::Ghz6),
                width: Some(width),
                channels: Some(channels),
                ..Patch::default()
            },
        )
        .await
        .unwrap();
        let report = apply_readback_once(&prepared.backend, &prepared.plan, Duration::from_secs(1))
            .await
            .unwrap();
        assert_eq!(
            report.outcome,
            Outcome::Verified,
            "readback report: {report:?}"
        );
        let requests = server.finish();
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.method == "PUT")
                .count(),
            1
        );
        assert_eq!(
            request_body(
                requests
                    .iter()
                    .find(|request| request.method == "PUT")
                    .unwrap()
            ),
            desired
        );
    }
}

#[tokio::test]
async fn ap_width_capability_requires_true_device_and_site_gates_where_defined() {
    for (band, width, mutate_device, site_caps) in [
        (
            Band::Ghz5,
            Width::Mhz160,
            "channelBandwidth160Mhz",
            vec!["channel-bandwidth-160mhz"],
        ),
        (
            Band::Ghz6,
            Width::Mhz160,
            "channelBandwidth160MhzOn6Ghz",
            vec![],
        ),
        (Band::Ghz6, Width::Mhz320, "channelBandwidth320Mhz", vec![]),
    ] {
        let mut ap = ap_radio("AP27");
        ap["capabilities"]["has"][mutate_device] = json!(false);
        let server = MockServer::start(vec![inventory(vec![ap]), capabilities(&site_caps)]);
        let error = radio::set_ap(
            &test_client(&server),
            SITE,
            "AP27",
            Patch {
                band: Some(band),
                width: Some(width),
                ..Patch::default()
            },
        )
        .await
        .err()
        .unwrap();
        assert_eq!(error.kind, ErrorKind::Usage);
        assert!(server.finish().iter().all(|r| r.method != "PUT"));
    }
}

#[tokio::test]
async fn ap_five_ghz_160_requires_site_capability_even_when_device_flag_is_true() {
    let server = MockServer::start(vec![inventory(vec![ap_radio("AP27")]), capabilities(&[])]);
    let error = radio::set_ap(
        &test_client(&server),
        SITE,
        "AP27",
        Patch {
            band: Some(Band::Ghz5),
            width: Some(Width::Mhz160),
            channels: Some(vec![149]),
            ..Patch::default()
        },
    )
    .await
    .err()
    .unwrap();
    assert_eq!(error.kind, ErrorKind::Usage);
    let requests = server.finish();
    assert_eq!(requests.len(), 2);
    assert!(requests.iter().all(|request| request.method == "GET"));
}

#[tokio::test]
async fn six_ghz_configuration_requires_wifi6e_capability() {
    for wifi6e in [Value::Null, json!(false)] {
        let mut ap = ap_radio("AP27");
        ap["capabilities"]["has"]["wifi6E"] = wifi6e;
        let server = MockServer::start(vec![inventory(vec![ap])]);
        let error = radio::set_ap(
            &test_client(&server),
            SITE,
            "AP27",
            Patch {
                band: Some(Band::Ghz6),
                channels: Some(vec![5]),
                ..Patch::default()
            },
        )
        .await
        .err()
        .unwrap();
        assert_eq!(error.kind, ErrorKind::Usage);
        let requests = server.finish();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "GET");
    }
}

#[tokio::test(start_paused = true)]
async fn ap_160_and_320_widths_verify_with_device_and_site_capabilities() {
    let _clock = keep_clock_paused().await;
    for (band, width, channels, site_capability) in [
        (
            Band::Ghz5,
            Width::Mhz160,
            vec![149],
            Some("channel-bandwidth-160mhz"),
        ),
        (Band::Ghz6, Width::Mhz160, vec![5], None),
        (Band::Ghz6, Width::Mhz320, vec![5], None),
    ] {
        let before = ap_radio("AP27");
        let mut desired = before.clone();
        let id = band.api_id();
        desired["radioManagementBands"][id]["useDeviceRadioManagementConfig"] = json!(true);
        desired["radioManagementBands"][id]["radioManagementBand"]["configuration"]["channelWidth"] =
            json!(width.api_id());
        desired["radioManagementBands"][id]["radioManagementBand"]["configuration"]["channels"] =
            json!(channels.iter().map(u16::to_string).collect::<Vec<_>>());
        let mut replies = vec![inventory(vec![before])];
        if let Some(capability) = site_capability {
            replies.push(capabilities(&[capability]));
        }
        replies.extend([Reply::empty(204), inventory(vec![desired.clone()])]);
        let server = MockServer::start(replies);
        let client = test_client(&server);
        let prepared = radio::set_ap(
            &client,
            SITE,
            "AP27",
            Patch {
                band: Some(band),
                width: Some(width),
                channels: Some(channels),
                ..Patch::default()
            },
        )
        .await
        .unwrap();
        let report = apply_readback_once(&prepared.backend, &prepared.plan, Duration::from_secs(1))
            .await
            .unwrap();
        assert_eq!(
            report.outcome,
            Outcome::Verified,
            "readback report: {report:?}"
        );
        let requests = server.finish();
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.method == "PUT")
                .count(),
            1
        );
        assert_eq!(
            request_body(
                requests
                    .iter()
                    .find(|request| request.method == "PUT")
                    .unwrap()
            ),
            desired
        );
    }
}

#[tokio::test]
async fn unavailable_duplicate_or_malformed_offers_refuse_site_writes() {
    let mut unavailable = site_radio();
    unavailable["radios"]["5ghz"]["drtAvailableChannels"] = json!([offered("80mhz", &["36"])]);
    let mut duplicate = site_radio();
    duplicate["radios"]["5ghz"]["drtAvailableChannels"] =
        json!([offered("160mhz", &["36"]), offered("160mhz", &["149"])]);
    let mut malformed = site_radio();
    malformed["radios"]["5ghz"]["drtAvailableChannels"] = json!([offered("160mhz", &["0"])]);
    let cases = [
        (unavailable, ErrorKind::Usage),
        (duplicate, ErrorKind::Unverified),
        (malformed, ErrorKind::Unverified),
    ];
    for (before, expected) in cases {
        let server = MockServer::start(vec![json_reply(&before)]);
        let error = radio::set_site(
            &test_client(&server),
            SITE,
            Patch {
                band: Some(Band::Ghz5),
                width: Some(Width::Mhz160),
                channels: Some(vec![36]),
                ..Patch::default()
            },
        )
        .await
        .err()
        .unwrap();
        assert_eq!(error.kind, expected);
        let requests = server.finish();
        assert_eq!(requests.len(), 1);
        assert!(requests.iter().all(|request| request.method == "GET"));
    }
}

#[tokio::test]
async fn five_ghz_accepts_supported_power_bounds_and_regulatory_max() {
    for (min, max) in [
        (Power::Dbm15, Power::Dbm30),
        (Power::Dbm18, Power::RegulatoryMax),
    ] {
        let server = MockServer::start(vec![json_reply(&site_radio())]);
        let client = test_client(&server);
        let prepared = radio::set_site(
            &client,
            SITE,
            Patch {
                band: Some(Band::Ghz5),
                min_power: Some(min),
                max_power: Some(max),
                ..Patch::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(
            prepared.plan.desired["/radios/5ghz/configuration/minTxPower"],
            min.api_id()
        );
        assert_eq!(
            prepared.plan.desired["/radios/5ghz/configuration/maxTxPower"],
            max.api_id()
        );
        assert!(
            server
                .finish()
                .iter()
                .all(|request| request.method != "PUT")
        );
    }
}

#[tokio::test(start_paused = true)]
async fn numeric_readback_is_not_equivalent_to_requested_string_channels() {
    let _clock = keep_clock_paused().await;
    let before = site_radio();
    let mut readback = before.clone();
    readback["radios"]["5ghz"]["configuration"]["channels"] = json!([149]);
    let server = MockServer::start(vec![
        json_reply(&before),
        Reply::empty(204),
        json_reply(&readback),
    ]);
    let client = test_client(&server);
    let prepared = radio::set_site(
        &client,
        SITE,
        Patch {
            band: Some(Band::Ghz5),
            channels: Some(vec![149]),
            ..Patch::default()
        },
    )
    .await
    .unwrap();
    let report = apply_readback_once(
        &prepared.backend,
        &prepared.plan,
        Duration::from_millis(100),
    )
    .await
    .unwrap();
    assert_eq!(report.outcome, Outcome::Unverified);
    assert_eq!(
        report.observed.unwrap()["/radios/5ghz/configuration/channels"],
        json!([149])
    );
    assert_eq!(
        server.finish().iter().filter(|r| r.method == "PUT").count(),
        1
    );
}

#[tokio::test(start_paused = true)]
async fn changed_inheritance_flag_or_owned_power_mismatch_is_unverified_without_retry() {
    let _clock = keep_clock_paused().await;
    let before = ap_radio("AP27");
    let mut desired = before.clone();
    desired["radioManagementBands"]["5ghz"]["useDeviceRadioManagementConfig"] = json!(true);
    desired["radioManagementBands"]["5ghz"]["radioManagementBand"]["configuration"]["maxTxPower"] =
        json!("27dbm");
    let mut readback = desired.clone();
    readback["radioManagementBands"]["5ghz"]["useDeviceRadioManagementConfig"] = json!(false);
    readback["radioManagementBands"]["5ghz"]["radioManagementBand"]["configuration"]["maxTxPower"] =
        json!("24dbm");
    let server = MockServer::start(vec![
        inventory(vec![before]),
        Reply::empty(204),
        inventory(vec![readback]),
    ]);
    let client = test_client(&server);
    let prepared = radio::set_ap(
        &client,
        SITE,
        "AP27",
        Patch {
            band: Some(Band::Ghz5),
            max_power: Some(Power::Dbm27),
            ..Patch::default()
        },
    )
    .await
    .unwrap();
    let report = apply_readback_once(
        &prepared.backend,
        &prepared.plan,
        Duration::from_millis(100),
    )
    .await
    .unwrap();
    assert_eq!(report.outcome, Outcome::Unverified);
    assert_eq!(
        server.finish().iter().filter(|r| r.method == "PUT").count(),
        1
    );
}

#[tokio::test(start_paused = true)]
async fn mismatched_resource_identity_and_missing_owned_fields_are_never_verified() {
    let _clock = keep_clock_paused().await;
    let before = site_radio();
    let mut foreign = before.clone();
    foreign["kind"] = json!("foreignRadioManagement");
    let server = MockServer::start(vec![
        json_reply(&before),
        Reply::empty(204),
        json_reply(&foreign),
    ]);
    let client = test_client(&server);
    let prepared = radio::set_site(
        &client,
        SITE,
        Patch {
            band: Some(Band::Ghz5),
            channels: Some(vec![149]),
            ..Patch::default()
        },
    )
    .await
    .unwrap();
    let report = apply_readback_once(
        &prepared.backend,
        &prepared.plan,
        Duration::from_millis(100),
    )
    .await
    .unwrap();
    assert_eq!(report.outcome, Outcome::Unverified);
    assert_eq!(
        server.finish().iter().filter(|r| r.method == "PUT").count(),
        1
    );

    let _clock = keep_clock_paused().await;
    let mut missing = site_radio();
    missing["radios"]["5ghz"]["configuration"]
        .as_object_mut()
        .unwrap()
        .remove("channels");
    let server = MockServer::start(vec![
        json_reply(&before),
        Reply::empty(204),
        json_reply(&missing),
    ]);
    let client = test_client(&server);
    let prepared = radio::set_site(
        &client,
        SITE,
        Patch {
            band: Some(Band::Ghz5),
            channels: Some(vec![149]),
            ..Patch::default()
        },
    )
    .await
    .unwrap();
    let report = apply_readback_once(
        &prepared.backend,
        &prepared.plan,
        Duration::from_millis(100),
    )
    .await
    .unwrap();
    assert_eq!(report.outcome, Outcome::Unverified);

    let _clock = keep_clock_paused().await;
    let before = ap_radio("AP27");
    let mut foreign_ap = ap_radio("AP27");
    foreign_ap["id"] = json!("different-opaque-id");
    let server = MockServer::start(vec![
        inventory(vec![before]),
        Reply::empty(204),
        inventory(vec![foreign_ap]),
    ]);
    let client = test_client(&server);
    let prepared = radio::set_ap(
        &client,
        SITE,
        "AP27",
        Patch {
            band: Some(Band::Ghz5),
            max_power: Some(Power::Dbm27),
            ..Patch::default()
        },
    )
    .await
    .unwrap();
    let report = apply_readback_once(
        &prepared.backend,
        &prepared.plan,
        Duration::from_millis(100),
    )
    .await
    .unwrap();
    assert_eq!(report.outcome, Outcome::Unverified);
}

#[tokio::test(start_paused = true)]
async fn foreign_acknowledgment_and_http_failure_each_make_one_put_and_never_verify() {
    let _clock = keep_clock_paused().await;
    let before = site_radio();
    let ack = json!({"id":"some-other-resource","kind":"radioManagement"});
    let server = MockServer::start(vec![
        json_reply(&before),
        Reply::json(200, serde_json::to_vec(&ack).unwrap()),
        json_reply(&before),
    ]);
    let client = test_client(&server);
    let prepared = radio::set_site(
        &client,
        SITE,
        Patch {
            band: Some(Band::Ghz5),
            channels: Some(vec![149]),
            ..Patch::default()
        },
    )
    .await
    .unwrap();
    let report = apply_readback_once(
        &prepared.backend,
        &prepared.plan,
        Duration::from_millis(100),
    )
    .await
    .unwrap();
    assert_eq!(report.outcome, Outcome::Failed);
    assert_eq!(
        server.finish().iter().filter(|r| r.method == "PUT").count(),
        1
    );

    let _clock = keep_clock_paused().await;
    let server = MockServer::start(vec![
        json_reply(&before),
        Reply::json(500, b"write rejected response-secret".to_vec()),
        json_reply(&before),
    ]);
    let client = test_client(&server);
    let prepared = radio::set_site(
        &client,
        SITE,
        Patch {
            band: Some(Band::Ghz5),
            channels: Some(vec![149]),
            ..Patch::default()
        },
    )
    .await
    .unwrap();
    let report = apply_readback_once(
        &prepared.backend,
        &prepared.plan,
        Duration::from_millis(100),
    )
    .await
    .unwrap();
    assert_eq!(report.outcome, Outcome::Failed);
    let error = format!(
        "{:?} {}",
        report.request_error,
        serde_json::to_string(&report.request_error).unwrap()
    );
    assert!(!error.contains("response-secret"));
    assert_eq!(
        server.finish().iter().filter(|r| r.method == "PUT").count(),
        1
    );
}

#[tokio::test]
async fn oversized_initial_radio_get_is_rejected_before_any_put() {
    let oversized = vec![b' '; crate::client::MAX_RESPONSE_BYTES + 1];
    let server = MockServer::start(vec![Reply::json(200, oversized)]);
    let error = radio::set_site(
        &test_client(&server),
        SITE,
        Patch {
            band: Some(Band::Ghz5),
            channels: Some(vec![149]),
            ..Patch::default()
        },
    )
    .await
    .err()
    .unwrap();
    assert_eq!(error.kind, ErrorKind::General);
    assert!(server.finish().iter().all(|r| r.method != "PUT"));
}

#[tokio::test(start_paused = true)]
async fn plans_results_and_errors_redact_secrets_but_full_put_keeps_opaque_server_data() {
    let _clock = keep_clock_paused().await;
    let before = site_radio();
    let mut desired = before.clone();
    desired["radios"]["5ghz"]["configuration"]["channels"] = json!(["149"]);
    let mut readback = desired.clone();
    readback["opaqueSiteField"]["secret"] = json!("readback-secret");
    let server = MockServer::start(vec![
        json_reply(&before),
        Reply::empty(204),
        json_reply(&readback),
    ]);
    let client = test_client(&server);
    let prepared = radio::set_site(
        &client,
        SITE,
        Patch {
            band: Some(Band::Ghz5),
            channels: Some(vec![149]),
            ..Patch::default()
        },
    )
    .await
    .unwrap();
    let debug = format!(
        "{:?} {}",
        prepared.plan,
        serde_json::to_string(&prepared.plan).unwrap()
    );
    assert!(!debug.contains("opaque-site-secret"));
    let report = apply_readback_once(&prepared.backend, &prepared.plan, Duration::from_secs(1))
        .await
        .unwrap();
    let exposed = format!(
        "{:?} {}",
        report.outcome,
        serde_json::to_string(&report.observed).unwrap()
    );
    assert!(!exposed.contains("readback-secret"));
    let requests = server.finish();
    assert_eq!(
        request_body(requests.iter().find(|r| r.method == "PUT").unwrap())["opaqueSiteField"]["secret"],
        "opaque-site-secret"
    );
}
