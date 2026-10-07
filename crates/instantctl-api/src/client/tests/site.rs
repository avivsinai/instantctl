use super::clock::{apply_readback_once, keep_clock_paused};
use super::*;
use crate::mutation::Mutation;
use crate::{
    mutation::{Outcome, apply_once},
    site::{Change, DnsMode, Resource, plan_update},
};
use std::net::Ipv4Addr;

const SITE: &str = "12345678-1234-5678-1234-567812345678";

#[tokio::test]
async fn site_resources_use_exact_get_routes_and_preserve_object_responses() {
    let resources = [
        (
            Resource::Health,
            "health",
            json!({"health":"healthy","futureHealth":{"status":null}}),
        ),
        (
            Resource::Dashboard,
            "dashboard",
            json!({"cards":[{"id":"clients","count":3}],"futureDashboardField":[1,2]}),
        ),
        (
            Resource::Topology,
            "graphTopology",
            json!({"nodes":[{"id":"ap"}],"edges":[],"futureTopologyField":true}),
        ),
        (
            Resource::Timezone,
            "timezone",
            json!({"timezoneIana":"Europe/Berlin","futureTimezoneField":null}),
        ),
        (
            Resource::ManagementNetwork,
            "managementNetwork",
            json!({"kind":"managementNetwork","managementVlan":20,
                "managementSubnet":{"dns":{"dnsServerAssignationMode":"infrastructure",
                    "automaticPrimaryDns":null,"customSecondaryDns":null,
                    "futureDnsField":{"keep":true},"nullDnsField":null},
                    "futureSubnetField":[1,null]},
                "futureNetworkField":{"enabled":true}}),
        ),
        (
            Resource::SpanningTree,
            "spanningTree",
            json!({"useRstp":true,"stpBaseBridgePriority":32768,"futureStpField":"preserve"}),
        ),
        (
            Resource::ExtendNetwork,
            "extendNetwork",
            json!({"kind":"extendNetwork","extendNetworkEnabled":true,
                "isExtendNetworkOutdoorMesh":false,"availableDevices":[{"id":"future"}],
                "futureExtendField":{"keep":true}}),
        ),
    ];
    let mut replies = resources
        .iter()
        .map(|(_, _, response)| Reply::json(200, serde_json::to_vec(response).unwrap()))
        .collect::<Vec<_>>();
    let management_network = resources
        .iter()
        .find(|(resource, _, _)| *resource == Resource::ManagementNetwork)
        .expect("management-network fixture");
    replies.push(Reply::json(
        200,
        serde_json::to_vec(&management_network.2).unwrap(),
    ));
    let mut future_dns = management_network.2.clone();
    future_dns["managementSubnet"]["dns"]["dnsServerAssignationMode"] = json!("vendor-mode");
    replies.push(Reply::json(200, serde_json::to_vec(&future_dns).unwrap()));
    let server = MockServer::start(replies);
    let client = make_client(&server, Duration::from_secs(2));

    for (resource, _, expected) in &resources {
        let actual = client
            .site_resource(SITE, *resource)
            .await
            .expect("site resource GET should return its JSON object unchanged");
        assert_eq!(&actual, expected);
    }

    for expected_mode in ["infrastructure", "vendor-mode"] {
        let dns = client
            .site_dns(SITE)
            .await
            .expect("nested management DNS should deserialize without defaults");
        assert_eq!(dns.mode.as_deref(), Some(expected_mode));
        assert_eq!(dns.automatic_primary, None);
        assert_eq!(dns.custom_primary, None);
        assert_eq!(dns.custom_secondary, None);
        assert_eq!(dns.extra["futureDnsField"], json!({"keep":true}));
        assert!(dns.extra["nullDnsField"].is_null());
    }

    let requests = server.finish();
    assert_eq!(requests.len(), resources.len() + 2);
    for (request, (_, segment, _)) in requests.iter().take(resources.len()).zip(resources.iter()) {
        assert_eq!(request.method, "GET");
        assert_eq!(request.target, format!("/api/sites/{SITE}/{segment}"));
        assert!(request.body.is_empty());
    }
    for dns_request in requests.iter().skip(resources.len()) {
        assert_eq!(dns_request.method, "GET");
        assert_eq!(
            dns_request.target,
            format!("/api/sites/{SITE}/managementNetwork")
        );
        assert!(dns_request.body.is_empty());
    }

    let server = MockServer::start(vec![Reply::json(200, br#"[1,2,3]"#.to_vec())]);
    let client = make_client(&server, Duration::from_secs(2));
    let error = client
        .site_resource(SITE, Resource::Health)
        .await
        .expect_err("non-object site resource must be unverified");
    assert_eq!(error.kind, ErrorKind::Unverified);
    assert_eq!(error.message, "site resource response is not an object");
    let requests = server.finish();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "GET");
    assert_eq!(requests[0].target, format!("/api/sites/{SITE}/health"));

    let server = MockServer::start(vec![]);
    let client = make_client(&server, Duration::from_secs(2));
    let error = client
        .site_resource("../escape", Resource::Dashboard)
        .await
        .expect_err("invalid site UUID must be rejected before HTTP");
    assert_eq!(error.kind, ErrorKind::Config);
    assert_eq!(error.message, "site identifier must be a UUID");
    assert!(server.finish().is_empty());
}

#[tokio::test(start_paused = true)]
async fn site_setting_updates_prepare_then_put_once_and_verify_only_requested_fields() {
    let _clock = keep_clock_paused().await;
    let timezone_before = json!({
        "timezoneIana":"UTC",
        "portalMetadata":{"revision":9,"vendorField":[1,null,"future"]}
    });
    let mut timezone_put = timezone_before.clone();
    timezone_put["timezoneIana"] = json!("Europe/Berlin");
    let mut timezone_readback = timezone_put.clone();
    timezone_readback["portalMetadata"]["revision"] = json!(10);

    let stp_before = json!({
        "id":SITE,
        "kind":"spanningTree",
        "useRstp":false,
        "stpBaseBridgePriority":32768,
        "vendorConfig":{"futureField":{"opaque":true}},
        "metrics":{"sample":1}
    });
    let mut stp_rstp_put = stp_before.clone();
    stp_rstp_put["useRstp"] = json!(true);
    let mut stp_rstp_readback = stp_rstp_put.clone();
    stp_rstp_readback["stpBaseBridgePriority"] = json!(4096);
    stp_rstp_readback["metrics"]["sample"] = json!(2);

    let mut stp_priority_put = stp_before.clone();
    stp_priority_put["stpBaseBridgePriority"] = json!(4096);
    let mut stp_priority_readback = stp_priority_put.clone();
    stp_priority_readback["useRstp"] = json!(true);
    stp_priority_readback["vendorConfig"]["futureField"]["opaque"] = json!(false);
    let mut stp_both_put = stp_before.clone();
    stp_both_put["useRstp"] = json!(true);
    stp_both_put["stpBaseBridgePriority"] = json!(4096);
    let mut stp_both_readback = stp_both_put.clone();
    stp_both_readback["metrics"]["sample"] = json!(2);

    let management_network_before = json!({
        "kind":"managementNetwork",
        "managementVlan":20,
        "managementSubnet":{
            "dns":{
                "dnsServerAssignationMode":"infrastructure",
                "futureDnsField":{"keep":true}
            },
            "futureSubnetField":[1,null]
        },
        "futureManagementField":{"opaque":"preserve"},
        "portalMetadata":{"revision":4}
    });
    let mut management_network_put = management_network_before.clone();
    management_network_put["managementVlan"] = json!(42);
    let mut management_network_readback = management_network_put.clone();
    management_network_readback["portalMetadata"]["revision"] = json!(5);

    let custom_dns_before = json!({
        "kind":"managementNetwork",
        "managementVlan":20,
        "managementSubnet":{
            "dns":{
                "dnsServerAssignationMode":"custom",
                "customPrimaryDns":"192.0.2.1",
                "customSecondaryDns":"192.0.2.2",
                "automaticPrimaryDns":"198.51.100.1",
                "automaticSecondaryDns":null,
                "futureDnsField":{"keep":[1,null,"opaque"]},
                "dnsServerAssignation":"legacy-shape-preserve"
            },
            "futureSubnetField":{"preserve":true}
        },
        "futureManagementField":{"opaque":"preserve"},
        "portalMetadata":{"revision":6}
    });
    let mut custom_dns_put = custom_dns_before.clone();
    custom_dns_put["managementSubnet"]["dns"]["customPrimaryDns"] = json!("192.0.2.53");
    custom_dns_put["managementSubnet"]["dns"]
        .as_object_mut()
        .unwrap()
        .remove("customSecondaryDns");
    let mut custom_dns_readback = custom_dns_put.clone();
    custom_dns_readback["managementSubnet"]["dns"]["customSecondaryDns"] = json!("");
    custom_dns_readback["managementVlan"] = json!(21);
    custom_dns_readback["portalMetadata"]["revision"] = json!(7);

    let automatic_dns_before = json!({
        "kind":"managementNetwork",
        "managementVlan":20,
        "managementSubnet":{
            "dns":{
                "dnsServerAssignationMode":"custom",
                "customPrimaryDns":null,
                "customSecondaryDns":"192.0.2.8",
                "automaticPrimaryDns":"198.51.100.1",
                "automaticSecondaryDns":"198.51.100.2",
                "futureDnsField":{"keep":true}
            },
            "futureSubnetField":["keep",null]
        },
        "futureManagementField":{"opaque":"preserve"}
    });
    let mut automatic_dns_put = automatic_dns_before.clone();
    automatic_dns_put["managementSubnet"]["dns"]["dnsServerAssignationMode"] = json!("automatic");
    let mut automatic_dns_readback = automatic_dns_put.clone();
    automatic_dns_readback["managementVlan"] = json!(21);

    let mut secondary_dns_put = automatic_dns_put.clone();
    secondary_dns_put["managementSubnet"]["dns"]["dnsServerAssignationMode"] = json!("custom");
    secondary_dns_put["managementSubnet"]["dns"]["customPrimaryDns"] = json!("192.0.2.53");
    secondary_dns_put["managementSubnet"]["dns"]["customSecondaryDns"] = json!("192.0.2.54");
    let mut infrastructure_dns_put = automatic_dns_before.clone();
    infrastructure_dns_put["managementSubnet"]["dns"]["dnsServerAssignationMode"] =
        json!("infrastructure");

    let extend_network_before = json!({
        "id":SITE,
        "kind":"extendNetwork",
        "extendNetworkEnabled":false,
        "isExtendNetworkOutdoorMesh":false,
        "availableDevices":[{"id":"future-device","compatible":true}],
        "portalMetadata":{"revision":8,"opaque":[null,"preserve"]}
    });
    let mut extend_enabled_put = extend_network_before.clone();
    extend_enabled_put["extendNetworkEnabled"] = json!(true);
    let mut extend_enabled_readback = extend_enabled_put.clone();
    extend_enabled_readback["isExtendNetworkOutdoorMesh"] = json!(true);
    extend_enabled_readback["portalMetadata"]["revision"] = json!(9);

    let mut extend_outdoor_put = extend_network_before.clone();
    extend_outdoor_put["isExtendNetworkOutdoorMesh"] = json!(true);
    let mut extend_outdoor_readback = extend_outdoor_put.clone();
    extend_outdoor_readback["extendNetworkEnabled"] = json!(true);
    extend_outdoor_readback["portalMetadata"]["revision"] = json!(9);

    let mut extend_both_put = extend_network_before.clone();
    extend_both_put["extendNetworkEnabled"] = json!(true);
    extend_both_put["isExtendNetworkOutdoorMesh"] = json!(true);
    let mut extend_both_readback = extend_both_put.clone();
    extend_both_readback["portalMetadata"]["revision"] = json!(9);

    let cases = [
        (
            "timezone",
            timezone_before,
            Change::Timezone("Europe/Berlin".into()),
            json!({"timezoneIana":"UTC"}),
            json!({"timezoneIana":"Europe/Berlin"}),
            timezone_put,
            timezone_readback,
        ),
        (
            "spanningTree",
            stp_before.clone(),
            Change::SpanningTree {
                use_rstp: Some(true),
                priority: None,
            },
            json!({"useRstp":false}),
            json!({"useRstp":true}),
            stp_rstp_put,
            stp_rstp_readback,
        ),
        (
            "spanningTree",
            stp_before.clone(),
            Change::SpanningTree {
                use_rstp: None,
                priority: Some(4096),
            },
            json!({"stpBaseBridgePriority":32768}),
            json!({"stpBaseBridgePriority":4096}),
            stp_priority_put,
            stp_priority_readback,
        ),
        (
            "spanningTree",
            stp_before,
            Change::SpanningTree {
                use_rstp: Some(true),
                priority: Some(4096),
            },
            json!({"useRstp":false,"stpBaseBridgePriority":32768}),
            json!({"useRstp":true,"stpBaseBridgePriority":4096}),
            stp_both_put,
            stp_both_readback,
        ),
        (
            "extendNetwork",
            extend_network_before.clone(),
            Change::ExtendNetwork {
                enabled: Some(true),
                outdoor_mesh: None,
            },
            json!({"extendNetworkEnabled":false}),
            json!({"extendNetworkEnabled":true}),
            extend_enabled_put,
            extend_enabled_readback,
        ),
        (
            "extendNetwork",
            extend_network_before.clone(),
            Change::ExtendNetwork {
                enabled: None,
                outdoor_mesh: Some(true),
            },
            json!({"isExtendNetworkOutdoorMesh":false}),
            json!({"isExtendNetworkOutdoorMesh":true}),
            extend_outdoor_put.clone(),
            extend_outdoor_readback,
        ),
        (
            "extendNetwork",
            extend_network_before.clone(),
            Change::ExtendNetwork {
                enabled: Some(true),
                outdoor_mesh: Some(true),
            },
            json!({"extendNetworkEnabled":false,"isExtendNetworkOutdoorMesh":false}),
            json!({"extendNetworkEnabled":true,"isExtendNetworkOutdoorMesh":true}),
            extend_both_put,
            extend_both_readback,
        ),
        (
            "managementNetwork",
            management_network_before,
            Change::ManagementVlan(42),
            json!({"managementVlan":20}),
            json!({"managementVlan":42}),
            management_network_put,
            management_network_readback,
        ),
        (
            "managementNetwork",
            custom_dns_before,
            Change::Dns {
                mode: DnsMode::Custom,
                primary: Some(Ipv4Addr::new(192, 0, 2, 53)),
                secondary: None,
            },
            json!({
                "dnsServerAssignationMode":"custom",
                "customPrimaryDns":"192.0.2.1",
                "customSecondaryDns":"192.0.2.2"
            }),
            json!({
                "dnsServerAssignationMode":"custom",
                "customPrimaryDns":"192.0.2.53",
                "customSecondaryDns":null
            }),
            custom_dns_put,
            custom_dns_readback,
        ),
        (
            "managementNetwork",
            automatic_dns_put.clone(),
            Change::Dns {
                mode: DnsMode::Custom,
                primary: Some(Ipv4Addr::new(192, 0, 2, 53)),
                secondary: Some(Ipv4Addr::new(192, 0, 2, 54)),
            },
            json!({"dnsServerAssignationMode":"automatic"}),
            json!({"dnsServerAssignationMode":"custom","customPrimaryDns":"192.0.2.53","customSecondaryDns":"192.0.2.54"}),
            secondary_dns_put.clone(),
            secondary_dns_put,
        ),
        (
            "managementNetwork",
            automatic_dns_before.clone(),
            Change::Dns {
                mode: DnsMode::Infrastructure,
                primary: None,
                secondary: None,
            },
            json!({"dnsServerAssignationMode":"custom"}),
            json!({"dnsServerAssignationMode":"infrastructure"}),
            infrastructure_dns_put.clone(),
            infrastructure_dns_put,
        ),
        (
            "managementNetwork",
            automatic_dns_before,
            Change::Dns {
                mode: DnsMode::Automatic,
                primary: None,
                secondary: None,
            },
            json!({"dnsServerAssignationMode":"custom"}),
            json!({"dnsServerAssignationMode":"automatic"}),
            automatic_dns_put,
            automatic_dns_readback,
        ),
    ];

    for (
        segment,
        current_object,
        change,
        expected_current,
        expected_desired,
        expected_put,
        readback,
    ) in cases
    {
        let server = MockServer::start(vec![
            Reply::json(200, serde_json::to_vec(&current_object).unwrap()),
            Reply::json(200, br#"{}"#.to_vec()),
            Reply::json(200, serde_json::to_vec(&readback).unwrap()),
        ]);
        let client = make_client(&server, Duration::from_secs(2));
        let prepared = plan_update(&client, SITE, change)
            .await
            .expect("valid site setting should prepare");
        assert_eq!(prepared.plan.current, expected_current);
        assert_eq!(prepared.plan.desired, expected_desired);

        let planning_requests = server.requests.try_iter().collect::<Vec<_>>();
        assert_eq!(planning_requests.len(), 1, "planning must only read");
        assert_eq!(planning_requests[0].method, "GET");
        assert_eq!(
            planning_requests[0].target,
            format!("/api/sites/{SITE}/{segment}")
        );
        assert!(planning_requests[0].body.is_empty());

        let report = apply_once(&prepared.backend, &prepared.plan, Duration::from_secs(2))
            .await
            .expect("site setting update should produce a report");
        assert_eq!(report.outcome, Outcome::Verified);
        assert_eq!(report.observed, Some(expected_desired));

        let mut requests = planning_requests;
        requests.extend(server.finish());
        assert_eq!(requests.len(), 3, "one read, one write, one readback");
        assert_eq!(requests[0].method, "GET");
        assert_eq!(requests[0].target, format!("/api/sites/{SITE}/{segment}"));
        assert_eq!(requests[1].method, "PUT");
        assert_eq!(requests[1].target, format!("/api/sites/{SITE}/{segment}"));
        assert_eq!(
            serde_json::from_slice::<Value>(&requests[1].body).unwrap(),
            expected_put,
            "PUT must preserve the complete resource and alter only requested fields"
        );
        assert_eq!(requests[2].method, "GET");
        assert_eq!(requests[2].target, format!("/api/sites/{SITE}/{segment}"));
    }

    let mut wrong_extend_readback = extend_network_before.clone();
    wrong_extend_readback["portalMetadata"]["revision"] = json!(9);
    let server = MockServer::start(vec![
        Reply::json(200, serde_json::to_vec(&extend_network_before).unwrap()),
        Reply::json(200, br#"{}"#.to_vec()),
        Reply::json(200, serde_json::to_vec(&wrong_extend_readback).unwrap()),
    ]);
    let client = make_client(&server, Duration::from_secs(2));
    let prepared = plan_update(
        &client,
        SITE,
        Change::ExtendNetwork {
            enabled: None,
            outdoor_mesh: Some(true),
        },
    )
    .await
    .expect("valid extend-network setting should prepare");
    let report = apply_readback_once(
        &prepared.backend,
        &prepared.plan,
        Duration::from_millis(100),
    )
    .await
    .expect("mismatching readback should produce an unverified report");
    assert_eq!(report.outcome, Outcome::Unverified);
    assert_eq!(report.error_kind(), Some(ErrorKind::Unverified));
    assert_eq!(
        report.observed,
        Some(json!({"isExtendNetworkOutdoorMesh":false}))
    );
    let requests = server.finish();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[0].method, "GET");
    assert_eq!(
        requests[0].target,
        format!("/api/sites/{SITE}/extendNetwork")
    );
    assert_eq!(requests[1].method, "PUT");
    assert_eq!(
        requests[1].target,
        format!("/api/sites/{SITE}/extendNetwork")
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&requests[1].body).unwrap(),
        extend_outdoor_put
    );
    assert_eq!(requests[2].method, "GET");
    assert_eq!(
        requests[2].target,
        format!("/api/sites/{SITE}/extendNetwork")
    );

    for (segment, response, change) in [
        (
            "timezone",
            json!({"id":SITE,"kind":"timezone","vendorConfig":{"keep":true}}),
            Change::Timezone("Europe/Berlin".into()),
        ),
        (
            "spanningTree",
            json!({"id":SITE,"kind":"spanningTree","useRstp":"true"}),
            Change::SpanningTree {
                use_rstp: Some(false),
                priority: None,
            },
        ),
        (
            "spanningTree",
            json!({"stpBaseBridgePriority":10000}),
            Change::SpanningTree {
                use_rstp: None,
                priority: Some(4096),
            },
        ),
        (
            "managementNetwork",
            json!({"kind":"managementNetwork","managementVlan":"20"}),
            Change::ManagementVlan(42),
        ),
        (
            "managementNetwork",
            json!({"managementSubnet":{"dns":{"dnsServerAssignationMode":"vendor-mode"}}}),
            Change::Dns {
                mode: DnsMode::Automatic,
                primary: None,
                secondary: None,
            },
        ),
        (
            "managementNetwork",
            json!({"managementSubnet":{"futureField":true}}),
            Change::Dns {
                mode: DnsMode::Automatic,
                primary: None,
                secondary: None,
            },
        ),
        (
            "extendNetwork",
            json!({"id":SITE,"kind":"extendNetwork",
                "isExtendNetworkOutdoorMesh":false,"availableDevices":[],
                "futureExtendField":{"keep":true}}),
            Change::ExtendNetwork {
                enabled: Some(true),
                outdoor_mesh: None,
            },
        ),
        (
            "extendNetwork",
            json!({"id":SITE,"kind":"extendNetwork",
                "extendNetworkEnabled":false,"isExtendNetworkOutdoorMesh":"false",
                "availableDevices":[],"futureExtendField":{"keep":true}}),
            Change::ExtendNetwork {
                enabled: Some(true),
                outdoor_mesh: Some(true),
            },
        ),
    ] {
        let server = MockServer::start(vec![Reply::json(
            200,
            serde_json::to_vec(&response).unwrap(),
        )]);
        let client = make_client(&server, Duration::from_secs(2));
        let error = match plan_update(&client, SITE, change).await {
            Ok(_) => panic!("missing or wrong-type current state must be unverified"),
            Err(error) => error,
        };
        assert_eq!(error.kind, ErrorKind::Unverified);
        let requests = server.finish();
        assert_eq!(
            requests.len(),
            1,
            "invalid current state must not be written"
        );
        assert_eq!(requests[0].method, "GET");
        assert_eq!(requests[0].target, format!("/api/sites/{SITE}/{segment}"));
    }

    let server = MockServer::start(vec![]);
    let client = make_client(&server, Duration::from_secs(2));
    let result = plan_update(
        &client,
        SITE,
        Change::SpanningTree {
            use_rstp: None,
            priority: Some(10000),
        },
    )
    .await;
    let error = match result {
        Ok(_) => panic!("invalid bridge priority must be refused before the request"),
        Err(error) => error,
    };
    assert_eq!(error.kind, ErrorKind::Usage);
    assert!(error.message.contains("multiple of 4096"));
    assert!(server.finish().is_empty());

    for (change, expected_kind) in [
        (Change::Timezone("Not/AZone".into()), ErrorKind::Usage),
        (Change::Timezone("Etc/Unknown".into()), ErrorKind::Usage),
        (
            Change::Dns {
                mode: DnsMode::Custom,
                primary: None,
                secondary: None,
            },
            ErrorKind::Config,
        ),
        (
            Change::ExtendNetwork {
                enabled: None,
                outdoor_mesh: None,
            },
            ErrorKind::Usage,
        ),
    ] {
        let server = MockServer::start(vec![]);
        let client = make_client(&server, Duration::from_secs(2));
        let error = match plan_update(&client, SITE, change).await {
            Ok(_) => panic!("invalid desired site setting must be refused before the request"),
            Err(error) => error,
        };
        assert_eq!(error.kind, expected_kind);
        assert!(server.finish().is_empty());
    }

    let server = MockServer::start(vec![]);
    let client = make_client(&server, Duration::from_secs(2));
    let result = plan_update(&client, SITE, Change::ManagementVlan(3333)).await;
    let error = match result {
        Ok(_) => panic!("reserved management VLAN must be refused before the request"),
        Err(error) => error,
    };
    assert_eq!(error.kind, ErrorKind::Usage);
    assert!(error.message.contains("reserved VLANs"));
    assert!(server.finish().is_empty());

    let current = json!({"id":SITE,"useRstp":false});
    let different_resource = json!({"id":"87654321-1234-5678-1234-567812345678","useRstp":true});
    let server = MockServer::start(vec![
        Reply::json(200, serde_json::to_vec(&current).unwrap()),
        Reply::json(200, serde_json::to_vec(&different_resource).unwrap()),
    ]);
    let client = make_client(&server, Duration::from_secs(2));
    let prepared = plan_update(
        &client,
        SITE,
        Change::SpanningTree {
            use_rstp: Some(true),
            priority: None,
        },
    )
    .await
    .unwrap();
    let error = prepared
        .backend
        .read()
        .await
        .expect_err("a different resource cannot supply readback evidence");
    assert_eq!(error.kind, ErrorKind::Unverified);
    assert!(error.message.contains("identity changed"));
    let requests = server.finish();
    assert_eq!(requests.len(), 2);
    assert!(requests.iter().all(|request| request.method == "GET"));
}
