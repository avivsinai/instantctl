use super::*;
use crate::{
    mutation::{Mutation, Outcome, apply_once},
    ports::*,
};
const SITE: &str = "123e4567-e89b-12d3-a456-426614174000";
const MAC: &str = "aa:bb:cc:dd:ee:ff";
const CLIENT_MAC: &str = "11:22:33:44:55:66";
fn port(face: u64, api: u64) -> Value {
    json!({"kind":"ethernetPort","faceplatePortNumber":face,"portNumber":api,"name":"desk","userDeactivated":false,"isUplink":false,"isDedicatedUplink":false,"trunkNumber":null,"portProfileId":null,"isPoeSupported":true,"isProvidingPower":true,"isPowerCycling":false,"usePoeSchedule":false,"poePowerMode":"normal","poePriority":"low","poePowerManagementMode":"usage-based","speedDuplexMode":"automatic","speedDuplex":"1GbpsFullDuplex","capabilities":{"supportedSpeedDuplexes":["1GbpsFullDuplex"]},"vendor":{"retain":true}})
}
fn device() -> Value {
    json!({"kind":"inventory","id":MAC,"macAddress":MAC,"name":"Switch","deviceType":"switch","status":"up","ethernetPorts":[port(1,0),port(2,1),port(3,2)],"trunkPorts":[{"kind":"trunkPort","trunkNumber":1,"trunkType":"static","userDeactivated":true,"portProfileId":null,"vendor":"retain"}],"portMirroringConfig":{"isEnabled":false,"destinationPortNumber":null,"sourceType":"ports","sourceNetworkId":null,"sourcePortNumbers":[],"directionType":"both","vendor":{"retain":true}},"capabilities":{"has":{"portMirroring":true}},"vendor":{"retain":true}})
}
fn list(rows: Vec<Value>) -> Vec<u8> {
    serde_json::to_vec(&json!({"kind":"resourceList","totalCount":rows.len(),"matchingFilterCount":rows.len(),"pendingAvailability":null,"elements":rows})).unwrap()
}
fn inv(d: Value) -> Reply {
    Reply::json(200, list(vec![d]))
}
fn summary(cycling: bool) -> Value {
    json!({"id":"client-1","macAddress":CLIENT_MAC,"name":"Garage Pi","clientType":"wired","status":"up","ipAddress":"192.0.2.5","connectedToPorts":[{"deviceId":MAC,"portNumber":0,"isPoweredByPort":true,"isPowerCyclable":true,"isPowerCycling":cycling}]})
}
fn clients(rows: Vec<Value>) -> Reply {
    let mut v: Value = serde_json::from_slice(&list(rows)).unwrap();
    v["kind"] = json!("clientSummaries");
    Reply::json(200, serde_json::to_vec(&v).unwrap())
}
fn patch() -> PortPatch {
    PortPatch {
        enabled: Some(false),
        ..Default::default()
    }
}

#[tokio::test]
async fn port_set_preserves_full_object_and_ignores_telemetry() {
    let original = device();
    let mut desired = original.clone();
    desired["ethernetPorts"][0]["userDeactivated"] = json!(true);
    let mut observed = desired.clone();
    observed["uptimeInSeconds"] = json!(42);
    observed["ethernetPorts"][0]["isLinkUp"] = json!(false);
    let server = MockServer::start(vec![inv(original), Reply::empty(204), inv(observed)]);
    let api = make_client(&server, Duration::from_secs(2));
    let p = plan_port_set(&api, SITE, "Switch", 1, patch(), false)
        .await
        .unwrap();
    assert_eq!(p.target["api_port_number"], 0);
    let result = apply_once(&p.backend, &p.plan, Duration::from_secs(1))
        .await
        .unwrap();
    assert_eq!(result.outcome, Outcome::Verified);
    let requests = server.finish();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[1].method, "PUT");
    assert_eq!(
        requests[1].target,
        format!("/api/sites/{SITE}/inventory/{MAC}")
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&requests[1].body).unwrap(),
        desired
    );
}
#[tokio::test]
async fn protected_ports_refuse_before_write_and_force_never_reaches_cycle() {
    for (field, value) in [
        ("isUplink", json!(true)),
        ("isDedicatedUplink", json!(true)),
        ("trunkNumber", json!(1)),
    ] {
        let mut d = device();
        d["ethernetPorts"][0][field] = value;
        let face = d["ethernetPorts"][0]["faceplatePortNumber"]
            .as_u64()
            .unwrap();
        let server = MockServer::start(vec![inv(d.clone())]);
        let api = make_client(&server, Duration::from_secs(1));
        let error = match plan_port_set(&api, SITE, MAC, face, patch(), false).await {
            Ok(_) => panic!("protected set accepted"),
            Err(e) => e,
        };
        assert_eq!(error.kind, ErrorKind::Usage);
        assert_eq!(server.finish().len(), 1);
        let server = MockServer::start(vec![inv(d)]);
        let api = make_client(&server, Duration::from_secs(1));
        let error = match plan_power_cycle(&api, SITE, MAC, face).await {
            Ok(_) => panic!("protected cycle accepted"),
            Err(e) => e,
        };
        assert_eq!(error.kind, ErrorKind::Usage);
        assert_eq!(server.finish().len(), 1);
    }
}

#[tokio::test]
async fn ordinary_faceplates_are_not_site_specific_protected_ports() {
    for faceplate in [14, 16] {
        let mut d = device();
        d["ethernetPorts"][0]["faceplatePortNumber"] = json!(faceplate);
        let mut after = d.clone();
        after["ethernetPorts"][0]["userDeactivated"] = json!(true);
        let server = MockServer::start(vec![inv(d.clone()), Reply::empty(204), inv(after)]);
        let api = make_client(&server, Duration::from_secs(1));
        let p = plan_port_set(&api, SITE, MAC, faceplate, patch(), false)
            .await
            .unwrap();
        assert_eq!(
            apply_once(&p.backend, &p.plan, Duration::from_secs(1))
                .await
                .unwrap()
                .outcome,
            Outcome::Verified
        );
        assert_eq!(server.finish()[1].method, "PUT");

        let server = MockServer::start(vec![inv(d), clients(vec![summary(false)])]);
        let api = make_client(&server, Duration::from_secs(1));
        assert!(plan_power_cycle(&api, SITE, MAC, faceplate).await.is_ok());
        assert_eq!(server.finish().len(), 2);
    }
}

#[tokio::test]
async fn configured_protected_ports_match_switch_identity_and_physical_faceplate() {
    for entry in [format!("{}:1", MAC.to_uppercase()), "Switch:1".into()] {
        let server = MockServer::start(vec![inv(device())]);
        let api = make_client(&server, Duration::from_secs(1))
            .with_protected_ports(std::slice::from_ref(&entry))
            .unwrap();
        let failure = match plan_port_set(&api, SITE, MAC, 1, patch(), false).await {
            Ok(_) => panic!("configured protected port accepted without force"),
            Err(error) => error,
        };
        assert_eq!(failure.kind, ErrorKind::Usage);
        assert!(failure.message.contains("uplink, LAG, or protected port"));
        assert_eq!(server.finish().len(), 1);

        let mut after = device();
        after["ethernetPorts"][0]["userDeactivated"] = json!(true);
        let server = MockServer::start(vec![inv(device()), Reply::empty(204), inv(after)]);
        let api = make_client(&server, Duration::from_secs(1))
            .with_protected_ports(std::slice::from_ref(&entry))
            .unwrap();
        let p = plan_port_set(&api, SITE, MAC, 1, patch(), true)
            .await
            .unwrap();
        assert_eq!(
            apply_once(&p.backend, &p.plan, Duration::from_secs(1))
                .await
                .unwrap()
                .outcome,
            Outcome::Verified
        );
        assert_eq!(server.finish()[1].method, "PUT");

        let server = MockServer::start(vec![inv(device())]);
        let api = make_client(&server, Duration::from_secs(1))
            .with_protected_ports(&[entry])
            .unwrap();
        let failure = match plan_power_cycle(&api, SITE, MAC, 1).await {
            Ok(_) => panic!("configured protected port accepted for power-cycle"),
            Err(error) => error,
        };
        assert_eq!(failure.kind, ErrorKind::Usage);
        assert_eq!(server.finish().len(), 1);
    }
    for entry in [
        "Other switch:1",
        "switch:1",
        "00:11:22:33:44:55:1",
        "Switch:2",
    ] {
        let server = MockServer::start(vec![inv(device())]);
        let api = make_client(&server, Duration::from_secs(1))
            .with_protected_ports(&[entry.into()])
            .unwrap();
        assert!(
            plan_port_set(&api, SITE, MAC, 1, patch(), false)
                .await
                .is_ok()
        );
        assert_eq!(server.finish().len(), 1);
    }
}

#[tokio::test]
async fn configured_protection_covers_lag_members_and_mirror_sources() {
    let server = MockServer::start(vec![inv(device())]);
    let api = make_client(&server, Duration::from_secs(1))
        .with_protected_ports(&["Switch:1".into()])
        .unwrap();
    let failure = match plan_lag_create(&api, SITE, MAC, 1, vec![1, 3], "lacp", false).await {
        Ok(_) => panic!("configured protection bypassed by LAG creation"),
        Err(error) => error,
    };
    assert_eq!(failure.kind, ErrorKind::Usage);
    assert_eq!(server.finish().len(), 1);

    let server = MockServer::start(vec![inv(device())]);
    let api = make_client(&server, Duration::from_secs(1))
        .with_protected_ports(&["Switch:1".into()])
        .unwrap();
    let failure = match plan_mirror(
        &api,
        SITE,
        MAC,
        MirrorPatch {
            enabled: true,
            destination: Some(3),
            sources: vec![1],
            network: None,
            direction: "both".into(),
        },
        false,
    )
    .await
    {
        Ok(_) => panic!("configured protection bypassed by mirror source"),
        Err(error) => error,
    };
    assert_eq!(failure.kind, ErrorKind::Usage);
    assert_eq!(server.finish().len(), 1);
}

#[tokio::test]
async fn malformed_protected_port_config_refuses_before_http() {
    for entry in ["Switch", ":1", "Switch:0", "Switch:invalid", "Switch\n:1"] {
        let server = MockServer::start(vec![]);
        let failure = make_client(&server, Duration::from_secs(1))
            .with_protected_ports(&[entry.into()])
            .unwrap_err();
        assert_eq!(failure.kind, ErrorKind::Config);
        assert!(server.finish().is_empty());
    }
}
#[tokio::test]
async fn force_allows_protected_configuration_but_unknown_safety_refuses() {
    let mut d = device();
    d["ethernetPorts"][0]["isUplink"] = json!(true);
    let server = MockServer::start(vec![inv(d)]);
    let api = make_client(&server, Duration::from_secs(1));
    assert!(
        plan_port_set(&api, SITE, MAC, 1, patch(), true)
            .await
            .is_ok()
    );
    assert_eq!(server.finish().len(), 1);
    let mut d = device();
    d["ethernetPorts"][0]
        .as_object_mut()
        .unwrap()
        .remove("isUplink");
    let server = MockServer::start(vec![inv(d)]);
    let api = make_client(&server, Duration::from_secs(1));
    let e = match plan_port_set(&api, SITE, MAC, 1, patch(), true).await {
        Ok(_) => panic!("unknown safety accepted"),
        Err(e) => e,
    };
    assert_eq!(e.kind, ErrorKind::Unverified);
    assert_eq!(server.finish().len(), 1);

    let mut d = device();
    d["ethernetPorts"][0]["isUplink"] = json!(true);
    d["ethernetPorts"][0]
        .as_object_mut()
        .unwrap()
        .remove("trunkNumber");
    let server = MockServer::start(vec![inv(d)]);
    let api = make_client(&server, Duration::from_secs(1));
    let e = match plan_port_set(&api, SITE, MAC, 1, patch(), true).await {
        Ok(_) => panic!("force bypassed unknown LAG membership on uplink"),
        Err(e) => e,
    };
    assert_eq!(e.kind, ErrorKind::Unverified);
    assert_eq!(server.finish().len(), 1);
}
#[tokio::test]
async fn unknown_dedicated_uplink_refuses_cycle_before_client_lookup() {
    for value in [None, Some(Value::Null)] {
        let mut d = device();
        let p = d["ethernetPorts"][0].as_object_mut().unwrap();
        p.remove("isDedicatedUplink");
        if let Some(value) = value {
            p.insert("isDedicatedUplink".into(), value);
        }
        let server = MockServer::start(vec![inv(d)]);
        let api = make_client(&server, Duration::from_secs(1));
        let error = match plan_power_cycle(&api, SITE, MAC, 1).await {
            Ok(_) => panic!("unknown dedicated-uplink state accepted"),
            Err(error) => error,
        };
        assert_eq!(error.kind, ErrorKind::Unverified);
        let requests = server.finish();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "GET");
    }
}
#[tokio::test]
async fn absent_duplicate_port_and_unknown_enum_do_not_write() {
    for d in [
        {
            let mut d = device();
            d["ethernetPorts"][0]["faceplatePortNumber"] = json!(2);
            d
        },
        {
            let mut d = device();
            d["ethernetPorts"][0]["poePowerMode"] = json!("future-mode");
            d
        },
    ] {
        let server = MockServer::start(vec![inv(d)]);
        let api = make_client(&server, Duration::from_secs(1));
        let patch = PortPatch {
            poe_mode: Some("normal".into()),
            ..Default::default()
        };
        assert!(
            plan_port_set(&api, SITE, MAC, 1, patch, false)
                .await
                .is_err()
        );
        assert_eq!(server.finish().len(), 1);
    }
    let server = MockServer::start(vec![inv(device())]);
    let api = make_client(&server, Duration::from_secs(1));
    let e = match plan_port_set(&api, SITE, MAC, 9, patch(), false).await {
        Ok(_) => panic!("absent port accepted"),
        Err(e) => e,
    };
    assert_eq!(e.kind, ErrorKind::NotFound);
    assert_eq!(server.finish().len(), 1);
}
#[tokio::test]
async fn cycle_binds_api_port_rechecks_attachment_and_observes_false_to_true() {
    let server = MockServer::start(vec![
        inv(device()),
        clients(vec![summary(false)]),
        inv(device()),
        clients(vec![summary(false)]),
        Reply::empty(204),
        clients(vec![summary(true)]),
    ]);
    let api = make_client(&server, Duration::from_secs(2));
    let p = plan_power_cycle(&api, SITE, MAC, 1).await.unwrap();
    assert!(!p.plan.current);
    assert!(p.plan.desired);
    let report = apply_once(&p.backend, &p.plan, Duration::from_secs(1))
        .await
        .unwrap();
    assert_eq!(report.outcome, Outcome::Verified);
    let requests = server.finish();
    assert_eq!(requests.len(), 6);
    let writes: Vec<_> = requests.iter().filter(|r| r.method == "POST").collect();
    assert_eq!(writes.len(), 1);
    assert_eq!(
        writes[0].target,
        format!("/api/sites/{SITE}/clientDetails/client-1?action=powerCycle")
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&writes[0].body).unwrap(),
        json!({})
    );
}
#[tokio::test]
async fn cycle_missing_ambiguous_and_already_cycling_refuse() {
    for (rows, expected) in [
        (vec![], ErrorKind::NotFound),
        (vec![summary(true)], ErrorKind::Usage),
        (
            {
                let mut other = summary(false);
                other["id"] = json!("client-2");
                other["macAddress"] = json!("22:33:44:55:66:77");
                vec![summary(false), other]
            },
            ErrorKind::Usage,
        ),
    ] {
        let server = MockServer::start(vec![inv(device()), clients(rows)]);
        let api = make_client(&server, Duration::from_secs(1));
        let e = match plan_power_cycle(&api, SITE, MAC, 1).await {
            Ok(_) => panic!("unsafe cycle accepted"),
            Err(e) => e,
        };
        assert_eq!(e.kind, expected);
        assert_eq!(server.finish().len(), 2);
    }
}
#[tokio::test(start_paused = true)]
async fn cycle_move_before_send_refuses_and_no_edge_is_unverified() {
    let _clock = keep_clock_paused().await;
    let mut moved = summary(false);
    moved["connectedToPorts"][0]["portNumber"] = json!(1);
    let server = MockServer::start(vec![
        inv(device()),
        clients(vec![summary(false)]),
        inv(device()),
        clients(vec![moved]),
    ]);
    let api = make_client(&server, Duration::from_secs(1));
    let p = plan_power_cycle(&api, SITE, MAC, 1).await.unwrap();
    assert!(p.backend.write(&true).await.is_err());
    let requests = server.finish();
    assert!(requests.iter().all(|r| r.method == "GET"));
    let server = MockServer::start(vec![
        inv(device()),
        clients(vec![summary(false)]),
        inv(device()),
        clients(vec![summary(false)]),
        Reply::empty(204),
        clients(vec![summary(false)]),
    ]);
    let api = make_client(&server, Duration::from_secs(1));
    let p = plan_power_cycle(&api, SITE, MAC, 1).await.unwrap();
    let report = apply_readback_once(&p.backend, &p.plan, Duration::from_millis(160))
        .await
        .unwrap();
    assert_eq!(report.outcome, Outcome::Unverified);
    assert_eq!(report.observed, Some(false));
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
async fn lag_create_full_put_and_reset_action_use_api_members() {
    let mut desired = device();
    desired["ethernetPorts"][0]["trunkNumber"] = json!(1);
    desired["ethernetPorts"][2]["trunkNumber"] = json!(1);
    desired["trunkPorts"][0]["trunkType"] = json!("lacp");
    desired["trunkPorts"][0]["userDeactivated"] = json!(false);
    let server = MockServer::start(vec![inv(device()), Reply::empty(204), inv(desired.clone())]);
    let api = make_client(&server, Duration::from_secs(1));
    let p = plan_lag_create(&api, SITE, MAC, 1, vec![1, 3], "lacp", false)
        .await
        .unwrap();
    assert_eq!(p.plan.desired["members"], json!([0, 2]));
    assert_eq!(
        apply_once(&p.backend, &p.plan, Duration::from_secs(1))
            .await
            .unwrap()
            .outcome,
        Outcome::Verified
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&server.finish()[1].body).unwrap(),
        desired
    );
    let server = MockServer::start(vec![inv(desired.clone())]);
    let api = make_client(&server, Duration::from_secs(1));
    let e = match plan_lag_remove(&api, SITE, MAC, 1, false).await {
        Ok(_) => panic!("LAG removal without force accepted"),
        Err(e) => e,
    };
    assert_eq!(e.kind, ErrorKind::Usage);
    assert_eq!(server.finish().len(), 1);
    let server = MockServer::start(vec![inv(desired), Reply::empty(204), inv(device())]);
    let api = make_client(&server, Duration::from_secs(1));
    let p = plan_lag_remove(&api, SITE, MAC, 1, true).await.unwrap();
    assert_eq!(
        apply_once(&p.backend, &p.plan, Duration::from_secs(1))
            .await
            .unwrap()
            .outcome,
        Outcome::Verified
    );
    let requests = server.finish();
    assert_eq!(
        requests[1].target,
        format!("/api/sites/{SITE}/inventory/{MAC}?action=resetTrunkPort")
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&requests[1].body).unwrap(),
        json!({"trunkNumber":1})
    );
}
#[tokio::test]
async fn lag_cannot_reassign_existing_member_even_with_force() {
    let mut d = device();
    d["ethernetPorts"][0]["trunkNumber"] = json!(2);
    let server = MockServer::start(vec![inv(d)]);
    let api = make_client(&server, Duration::from_secs(1));
    assert!(
        plan_lag_create(&api, SITE, MAC, 1, vec![1, 3], "lacp", true)
            .await
            .is_err()
    );
    assert_eq!(server.finish().len(), 1);
}
#[tokio::test(start_paused = true)]
async fn lag_removal_requires_original_member_ports_and_configuration_readback() {
    let _clock = keep_clock_paused().await;
    let mut original = device();
    original["ethernetPorts"][0]["trunkNumber"] = json!(1);
    original["ethernetPorts"][1]["trunkNumber"] = json!(1);
    for incomplete in [
        {
            let mut d = device();
            d["ethernetPorts"] = json!([]);
            d
        },
        {
            let mut d = device();
            d.as_object_mut().unwrap().remove("trunkPorts");
            d
        },
    ] {
        let server = MockServer::start(vec![
            inv(original.clone()),
            Reply::empty(204),
            inv(incomplete),
        ]);
        let api = make_client(&server, Duration::from_secs(1));
        let p = plan_lag_remove(&api, SITE, MAC, 1, true).await.unwrap();
        let report = apply_readback_once(&p.backend, &p.plan, Duration::from_secs(2))
            .await
            .unwrap();
        assert_eq!(report.outcome, Outcome::Unverified);
        assert_eq!(report.readback_error.unwrap().kind, ErrorKind::Unverified);
        let requests = server.finish();
        assert_eq!(requests.iter().filter(|r| r.method == "POST").count(), 1);
    }
}
#[tokio::test]
async fn mirror_preserves_unknown_fields_and_translates_faceplate_numbers() {
    let mut desired = device();
    desired["portMirroringConfig"]["isEnabled"] = json!(true);
    desired["portMirroringConfig"]["destinationPortNumber"] = json!(2);
    desired["portMirroringConfig"]["sourcePortNumbers"] = json!([0]);
    let mut readback = desired.clone();
    readback["portMirroringConfig"]["vendor"] = json!({"changed":true});
    let server = MockServer::start(vec![inv(device()), Reply::empty(204), inv(readback)]);
    let api = make_client(&server, Duration::from_secs(1));
    let p = plan_mirror(
        &api,
        SITE,
        MAC,
        MirrorPatch {
            enabled: true,
            destination: Some(3),
            sources: vec![1],
            network: None,
            direction: "both".into(),
        },
        false,
    )
    .await
    .unwrap();
    assert_eq!(
        apply_once(&p.backend, &p.plan, Duration::from_secs(1))
            .await
            .unwrap()
            .outcome,
        Outcome::Verified
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&server.finish()[1].body).unwrap(),
        desired
    );
}
#[tokio::test]
async fn disabling_active_mirror_refuses_unknown_previous_port_identities() {
    for (field, value) in [
        ("sourcePortNumbers", json!("unknown")),
        ("sourcePortNumbers", json!([99])),
        ("destinationPortNumber", json!("unknown")),
        ("sourceType", json!("future-type")),
    ] {
        for force in [false, true] {
            let mut d = device();
            d["portMirroringConfig"]["isEnabled"] = json!(true);
            d["portMirroringConfig"]["destinationPortNumber"] = json!(2);
            d["portMirroringConfig"]["sourcePortNumbers"] = json!([0]);
            d["portMirroringConfig"][field] = value.clone();
            let server = MockServer::start(vec![inv(d)]);
            let api = make_client(&server, Duration::from_secs(1));
            let error = match plan_mirror(
                &api,
                SITE,
                MAC,
                MirrorPatch {
                    enabled: false,
                    destination: None,
                    sources: vec![],
                    network: None,
                    direction: "both".into(),
                },
                force,
            )
            .await
            {
                Ok(_) => panic!("unknown previous mirror identities accepted"),
                Err(error) => error,
            };
            assert_eq!(error.kind, ErrorKind::Unverified);
            let requests = server.finish();
            assert_eq!(requests.len(), 1);
            assert_eq!(requests[0].method, "GET");
        }
    }
}
#[tokio::test]
async fn find_ignores_wifi_names_retains_unknown_connection_and_maps_faceplate() {
    let mut wifi = summary(false);
    wifi["id"] = json!("wifi-1");
    wifi["macAddress"] = json!("22:33:44:55:66:77");
    wifi["clientType"] = json!("wireless");
    wifi["connectedToPorts"] = Value::Null;
    let mut wired = summary(false);
    wired["status"] = Value::Null;
    let server = MockServer::start(vec![clients(vec![wired, wifi]), inv(device())]);
    let api = make_client(&server, Duration::from_secs(1));
    let rows = find_port(&api, SITE, "garage").await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].port_idx, 1);
    assert_eq!(rows[0].api_port_number, 0);
    assert_eq!(rows[0].connected, None);
    assert_eq!(server.finish().len(), 2);
    let server = MockServer::start(vec![clients(vec![summary(false)])]);
    let api = make_client(&server, Duration::from_secs(1));
    assert_eq!(
        find_port(&api, SITE, "missing").await.unwrap_err().kind,
        ErrorKind::NotFound
    );
    assert_eq!(server.finish().len(), 1);
}
#[tokio::test]
async fn find_ambiguous_wired_names_and_partial_inventory_fail_closed() {
    let mut other = summary(false);
    other["id"] = json!("client-2");
    other["macAddress"] = json!("22:33:44:55:66:77");
    let server = MockServer::start(vec![clients(vec![summary(false), other])]);
    let api = make_client(&server, Duration::from_secs(1));
    assert_eq!(
        find_port(&api, SITE, "garage").await.unwrap_err().kind,
        ErrorKind::Usage
    );
    assert_eq!(server.finish().len(), 1);
    let mut partial: Value = serde_json::from_slice(&list(vec![device()])).unwrap();
    partial["totalCount"] = json!(2);
    let server = MockServer::start(vec![
        clients(vec![summary(false)]),
        Reply::json(200, serde_json::to_vec(&partial).unwrap()),
    ]);
    let api = make_client(&server, Duration::from_secs(1));
    assert!(find_port(&api, SITE, CLIENT_MAC).await.is_err());
    assert_eq!(server.finish().len(), 2);
}
