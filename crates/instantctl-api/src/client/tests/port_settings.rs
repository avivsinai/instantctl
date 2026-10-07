use super::*;
use crate::{
    client::port_settings::{
        ActiveSchedule, ProfilePatch, SchedulePatch, SimpleSchedule, WeekSchedule, Weekday,
        WeekdaySchedule, plan_clone_port_profile, plan_delete_port_profile, plan_eee,
        plan_poe_schedule, plan_update_port_profile, poe_schedule, power_management,
        read_port_profile,
    },
    mutation::{Mutation, Outcome, apply_once},
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, time::Duration};

const SITE: &str = "123e4567-e89b-12d3-a456-426614174000";
const PROFILE_ID: &str = "profile-1";
const PROFILE_KIND: &str = "ethernetPort";

fn inventory() -> Vec<u8> {
    inventory_with(Vec::new())
}

fn inventory_with(devices: Vec<Value>) -> Vec<u8> {
    let count = devices.len() as u64;
    serde_json::to_vec(&json!({
        "kind":"resourceList","totalCount":count,"matchingFilterCount":count,
        "pendingAvailability":null,"elements":devices
    }))
    .expect("inventory JSON")
}

fn switch(id: &str, ethernet_ports: Vec<Value>, trunk_ports: Vec<Value>) -> Value {
    json!({
        "id":id,"macAddress":id,"deviceType":"switch","ethernetPorts":ethernet_ports,
        "trunkPorts":trunk_ports
    })
}

fn profile(id: &str, name: &str) -> Value {
    json!({
        "id":id,"kind":PROFILE_KIND,"name":name,
        "customMappingUntaggedWiredNetworkId":"wired-1",
        "customMappingTaggedWiredNetworksSelection":{"selection":"specific","specificEntityIds":["wired-2"],"vendorConfig":{"keep":true}},
        "networkMapping":"custom","protectedPortEnabled":false,"shouldTrustTraffic":true,
        "stormControlEnabled":false,"usePoeSchedule":false,"spanningTreeProtection":"none",
        "portAssignmentByDeviceId":{},
        "vendorExtension":{"retain":true},"referenceCount":1,"references":1,
        "capabilities":{"readOnly":true},"serverMetadata":{"revision":"source"}
    })
}

fn profiles(elements: Vec<Value>) -> Vec<u8> {
    let count = elements.len() as u64;
    serde_json::to_vec(&json!({
        "kind":"resourceList","totalCount":count,"matchingFilterCount":count,"elements":elements
    }))
    .expect("port profile collection JSON")
}

fn reply(value: Value) -> Reply {
    Reply::json(200, serde_json::to_vec(&value).expect("reply JSON"))
}

fn body(request: &Request) -> Value {
    serde_json::from_slice(&request.body).expect("request body JSON")
}

fn error<T>(result: Result<T, crate::Error>) -> crate::Error {
    match result {
        Err(error) => error,
        Ok(_) => panic!("operation should fail"),
    }
}

#[tokio::test]
async fn configured_protection_covers_sitewide_eee_and_poe_schedule() {
    let inventory_bytes = inventory_with(vec![switch(
        "aa:bb:cc:dd:ee:ff",
        vec![
            json!({"portNumber":8,"faceplatePortNumber":2,"isUplink":false,
            "isDedicatedUplink":false,"trunkNumber":null}),
        ],
        vec![],
    )]);
    let server = MockServer::start(vec![Reply::json(200, inventory_bytes.clone())]);
    let client = make_client(&server, Duration::from_secs(1))
        .with_protected_ports(&["aa:bb:cc:dd:ee:ff:2".into()])
        .unwrap();
    assert_eq!(
        error(plan_eee(&client, SITE, true, false).await).kind,
        ErrorKind::Usage
    );
    assert_eq!(server.finish().len(), 1);

    let server = MockServer::start(vec![Reply::json(200, inventory_bytes)]);
    let client = make_client(&server, Duration::from_secs(1))
        .with_protected_ports(&["aa:bb:cc:dd:ee:ff:2".into()])
        .unwrap();
    let failure = error(
        plan_poe_schedule(
            &client,
            SITE,
            SchedulePatch {
                active_schedule: Some(ActiveSchedule::None),
                simple_schedule: None,
                week_schedule: None,
            },
            false,
        )
        .await,
    );
    assert_eq!(failure.kind, ErrorKind::Usage);
    assert_eq!(server.finish().len(), 1);
}

#[tokio::test]
async fn profile_update_preserves_nested_unknown_fields_and_ignores_their_readback_changes() {
    let before = profile(PROFILE_ID, "Desk");
    let mut expected = before.clone();
    expected["customMappingTaggedWiredNetworksSelection"]["specificEntityIds"] = json!(["wired-3"]);
    let mut after = expected.clone();
    after["vendorRuntime"] = json!("newer");
    after["customMappingTaggedWiredNetworksSelection"]["vendorConfig"] =
        json!({"keep":false,"revision":2});
    let server = MockServer::start(vec![
        Reply::json(200, profiles(vec![before])),
        Reply::json(200, inventory()),
        reply(json!({"id":PROFILE_ID,"kind":PROFILE_KIND})),
        reply(after),
    ]);
    let client = make_client(&server, Duration::from_secs(2));
    let prepared = plan_update_port_profile(
        &client,
        SITE,
        PROFILE_ID,
        ProfilePatch {
            tagged_networks: Some(vec!["wired-3".into()]),
            ..ProfilePatch::default()
        },
        true,
    )
    .await
    .expect("valid profile update");
    let report = apply_once(&prepared.backend, &prepared.plan, Duration::from_secs(2))
        .await
        .expect("mutation report");
    assert_eq!(report.outcome, Outcome::Verified, "{report:?}");
    let requests = server.finish();
    assert_eq!(requests.len(), 4);
    assert_eq!(requests[2].method, "PUT");
    assert_eq!(body(&requests[2]), expected);
    assert_eq!(
        requests[3].target,
        format!("/api/sites/{SITE}/portProfiles/{PROFILE_ID}")
    );
}

#[tokio::test]
async fn configured_authoritative_faceplate_assignment_blocks_without_force_and_force_verifies_update()
 {
    const DEVICE: &str = "aa:bb:cc:dd:ee:ff";
    let mut assigned = profile(PROFILE_ID, "Desk");
    assigned["portAssignmentByDeviceId"] =
        json!({DEVICE:[{"portNumber":13,"usePortProfile":true}]});
    let inventory = inventory_with(vec![switch(
        DEVICE,
        vec![json!({
            "portNumber":13,"faceplatePortNumber":14,"isUplink":false,
            "isDedicatedUplink":false,"trunkNumber":null,"portProfileId":null
        })],
        vec![],
    )]);

    let blocked = MockServer::start(vec![
        Reply::json(200, profiles(vec![assigned.clone()])),
        Reply::json(200, inventory.clone()),
    ]);
    let client = make_client(&blocked, Duration::from_secs(2));
    let client = client
        .with_protected_ports(&[format!("{DEVICE}:14")])
        .unwrap();
    let error = error(
        plan_update_port_profile(
            &client,
            SITE,
            PROFILE_ID,
            ProfilePatch {
                name: Some("Desk Renamed".into()),
                ..ProfilePatch::default()
            },
            false,
        )
        .await,
    );
    assert_eq!(error.kind, ErrorKind::Usage);
    let requests = blocked.finish();
    assert_eq!(requests.len(), 2, "unsafe update must stop before PUT");
    assert!(requests.iter().all(|request| request.method == "GET"));

    let mut after = assigned.clone();
    after["name"] = json!("Desk Renamed");
    let forced = MockServer::start(vec![
        Reply::json(200, profiles(vec![assigned])),
        Reply::json(200, inventory),
        reply(json!({"id":PROFILE_ID,"kind":PROFILE_KIND})),
        reply(after),
    ]);
    let client = make_client(&forced, Duration::from_secs(2));
    let client = client
        .with_protected_ports(&[format!("{DEVICE}:14")])
        .unwrap();
    let prepared = plan_update_port_profile(
        &client,
        SITE,
        PROFILE_ID,
        ProfilePatch {
            name: Some("Desk Renamed".into()),
            ..ProfilePatch::default()
        },
        true,
    )
    .await
    .expect("--force accepts the known protected assignment");
    let report = apply_once(&prepared.backend, &prepared.plan, Duration::from_secs(2))
        .await
        .expect("forced update report");
    assert_eq!(report.outcome, Outcome::Verified, "{report:?}");
    let requests = forced.finish();
    assert_eq!(requests.len(), 4);
    assert_eq!(requests[2].method, "PUT");
}

#[tokio::test]
async fn authoritative_lag_assignment_blocks_without_force_but_unknown_target_fails_closed() {
    const DEVICE: &str = "aa:bb:cc:dd:ee:ff";
    let mut assigned = profile(PROFILE_ID, "Desk");
    assigned["portAssignmentByDeviceId"] =
        json!({DEVICE:[{"trunkNumber":7,"usePortProfile":true}]});
    let inventory_bytes = inventory_with(vec![switch(
        DEVICE,
        vec![json!({
            "portNumber":13,"faceplatePortNumber":1,"isUplink":false,
            "isDedicatedUplink":false,"trunkNumber":7,"portProfileId":null
        })],
        vec![
            json!({"trunkNumber":7,"portProfileId":null,"trunkType":"lacp","userDeactivated":false}),
        ],
    )]);
    let blocked = MockServer::start(vec![
        Reply::json(200, profiles(vec![assigned.clone()])),
        Reply::json(200, inventory_bytes.clone()),
    ]);
    let client = make_client(&blocked, Duration::from_secs(2));
    let blocked_error = error(
        plan_update_port_profile(
            &client,
            SITE,
            PROFILE_ID,
            ProfilePatch {
                protected: Some(true),
                ..ProfilePatch::default()
            },
            false,
        )
        .await,
    );
    assert_eq!(blocked_error.kind, ErrorKind::Usage);
    let requests = blocked.finish();
    assert_eq!(requests.len(), 2, "unsafe LAG update must stop before PUT");
    assert!(requests.iter().all(|request| request.method == "GET"));

    let mut after = assigned.clone();
    after["protectedPortEnabled"] = json!(true);
    let forced = MockServer::start(vec![
        Reply::json(200, profiles(vec![assigned.clone()])),
        Reply::json(200, inventory_bytes.clone()),
        reply(json!({"id":PROFILE_ID,"kind":PROFILE_KIND})),
        reply(after),
    ]);
    let client = make_client(&forced, Duration::from_secs(2));
    let prepared = plan_update_port_profile(
        &client,
        SITE,
        PROFILE_ID,
        ProfilePatch {
            protected: Some(true),
            ..ProfilePatch::default()
        },
        true,
    )
    .await
    .expect("--force accepts a known protected LAG assignment");
    let report = apply_once(&prepared.backend, &prepared.plan, Duration::from_secs(2))
        .await
        .expect("forced LAG update report");
    assert_eq!(report.outcome, Outcome::Verified, "{report:?}");
    let requests = forced.finish();
    assert_eq!(requests.len(), 4);
    assert_eq!(requests[2].method, "PUT");

    let mut missing_target = profile(PROFILE_ID, "Desk");
    missing_target["portAssignmentByDeviceId"] =
        json!({"bb:cc:dd:ee:ff:00":[{"portNumber":13,"usePortProfile":true}]});
    let missing = MockServer::start(vec![
        Reply::json(200, profiles(vec![missing_target])),
        Reply::json(200, inventory()),
    ]);
    let client = make_client(&missing, Duration::from_secs(2));
    let missing_error = error(
        plan_update_port_profile(
            &client,
            SITE,
            PROFILE_ID,
            ProfilePatch {
                protected: Some(true),
                ..ProfilePatch::default()
            },
            true,
        )
        .await,
    );
    assert_eq!(missing_error.kind, ErrorKind::Unverified);
    assert_eq!(
        missing.finish().len(),
        2,
        "missing assignment target must fail even with force"
    );
}

#[tokio::test]
async fn inventory_only_lag_profile_links_cannot_force_unknown_member_safety() {
    const DEVICE: &str = "aa:bb:cc:dd:ee:ff";
    let inventory_bytes = inventory_with(vec![switch(
        DEVICE,
        vec![json!({
            "portNumber":13,"faceplatePortNumber":1,"isUplink":false,
            "isDedicatedUplink":null,"trunkNumber":1,"portProfileId":null
        })],
        vec![json!({"trunkNumber":1,"portProfileId":PROFILE_ID})],
    )]);
    for delete in [false, true] {
        let server = MockServer::start(vec![
            Reply::json(200, profiles(vec![profile(PROFILE_ID, "Desk")])),
            Reply::json(200, inventory_bytes.clone()),
        ]);
        let client = make_client(&server, Duration::from_secs(1));
        let failure = if delete {
            error(plan_delete_port_profile(&client, SITE, PROFILE_ID, true).await)
        } else {
            error(
                plan_update_port_profile(
                    &client,
                    SITE,
                    PROFILE_ID,
                    ProfilePatch {
                        protected: Some(true),
                        ..ProfilePatch::default()
                    },
                    true,
                )
                .await,
            )
        };
        assert_eq!(failure.kind, ErrorKind::Unverified);
        let requests = server.finish();
        assert_eq!(requests.len(), 2);
        assert!(requests.iter().all(|request| request.method == "GET"));
    }
}

#[tokio::test]
async fn authoritative_empty_lag_profile_assignment_still_requires_force() {
    const DEVICE: &str = "aa:bb:cc:dd:ee:ff";
    let mut assigned = profile(PROFILE_ID, "Desk");
    assigned["portAssignmentByDeviceId"] =
        json!({DEVICE:[{"trunkNumber":1,"usePortProfile":true}]});
    let inventory_bytes = inventory_with(vec![switch(
        DEVICE,
        vec![],
        vec![json!({"trunkNumber":1,"portProfileId":null})],
    )]);
    let server = MockServer::start(vec![
        Reply::json(200, profiles(vec![assigned.clone()])),
        Reply::json(200, inventory_bytes.clone()),
    ]);
    let client = make_client(&server, Duration::from_secs(1));
    let failure = error(
        plan_update_port_profile(
            &client,
            SITE,
            PROFILE_ID,
            ProfilePatch {
                protected: Some(true),
                ..ProfilePatch::default()
            },
            false,
        )
        .await,
    );
    assert_eq!(failure.kind, ErrorKind::Usage);
    assert_eq!(server.finish().len(), 2);

    let mut after = assigned.clone();
    after["protectedPortEnabled"] = json!(true);
    let server = MockServer::start(vec![
        Reply::json(200, profiles(vec![assigned])),
        Reply::json(200, inventory_bytes),
        Reply::empty(204),
        reply(after),
    ]);
    let client = make_client(&server, Duration::from_secs(1));
    let prepared = plan_update_port_profile(
        &client,
        SITE,
        PROFILE_ID,
        ProfilePatch {
            protected: Some(true),
            ..ProfilePatch::default()
        },
        true,
    )
    .await
    .unwrap();
    assert_eq!(
        apply_once(&prepared.backend, &prepared.plan, Duration::from_secs(1))
            .await
            .unwrap()
            .outcome,
        Outcome::Verified
    );
    assert_eq!(server.finish().len(), 4);
}

#[tokio::test]
async fn profile_clone_omits_server_id_clears_assignments_and_binds_ack_identity() {
    let mut source = profile(PROFILE_ID, "Desk");
    source["portAssignmentByDeviceId"] = json!({
        "aa:bb:cc:dd:ee:ff":[{"portNumber":1,"usePortProfile":true}]
    });
    let mut created = source.clone();
    created["id"] = json!("profile-new");
    created["name"] = json!("Desk Copy");
    created["portAssignmentByDeviceId"] = json!({});
    created["referenceCount"] = json!(2);
    created["references"] = json!(2);
    created["serverMetadata"] = json!({"revision":"created"});
    let server = MockServer::start(vec![
        Reply::json(200, profiles(vec![source])),
        reply(
            json!({"id":"profile-new","kind":PROFILE_KIND,"name":"Desk Copy","referenceCount":0,"serverMetadata":{"ack":true}}),
        ),
        Reply::json(200, profiles(vec![profile(PROFILE_ID, "Desk"), created])),
    ]);
    let client = make_client(&server, Duration::from_secs(2));
    let prepared = plan_clone_port_profile(&client, SITE, PROFILE_ID, "Desk Copy")
        .await
        .expect("valid clone");
    let report = apply_once(&prepared.backend, &prepared.plan, Duration::from_secs(2))
        .await
        .expect("clone report");
    assert_eq!(report.outcome, Outcome::Verified, "{report:?}");
    let requests = server.finish();
    let posted = body(&requests[1]);
    assert_eq!(
        posted["id"],
        json!(""),
        "portal serializer clears the source id"
    );
    assert_eq!(posted["portAssignmentByDeviceId"], json!({}));
    assert!(posted.get("referenceCount").is_none());
    assert!(posted.get("references").is_none());
    assert!(posted.get("vendorExtension").is_none());
    assert!(posted.get("capabilities").is_none());
    assert!(posted.get("serverMetadata").is_none());
    assert_eq!(requests.len(), 3);
}

#[tokio::test]
async fn clone_rejects_foreign_or_incomplete_acknowledgments() {
    for ack in [
        json!({"id":"profile-new","kind":"other"}),
        json!({"kind":PROFILE_KIND}),
    ] {
        let server = MockServer::start(vec![
            Reply::json(200, profiles(vec![profile(PROFILE_ID, "Desk")])),
            reply(ack),
        ]);
        let client = make_client(&server, Duration::from_secs(2));
        let prepared = plan_clone_port_profile(&client, SITE, PROFILE_ID, "Copy")
            .await
            .expect("prepare clone");
        let error = error(prepared.backend.write(&prepared.plan.desired).await);
        assert_eq!(error.kind, ErrorKind::Unverified);
        server.finish();
    }
}

#[tokio::test]
async fn clone_write_is_one_shot_even_after_a_valid_acknowledgment() {
    let server = MockServer::start(vec![
        Reply::json(200, profiles(vec![profile(PROFILE_ID, "Desk")])),
        reply(json!({"id":"profile-new","kind":PROFILE_KIND})),
    ]);
    let client = make_client(&server, Duration::from_secs(2));
    let prepared = plan_clone_port_profile(&client, SITE, PROFILE_ID, "Copy")
        .await
        .expect("prepare clone");
    prepared
        .backend
        .write(&prepared.plan.desired)
        .await
        .expect("first clone write should succeed");
    let error = error(prepared.backend.write(&prepared.plan.desired).await);
    assert_eq!(error.kind, ErrorKind::Unverified);
    assert_eq!(server.finish().len(), 2, "only one POST is sent");
}

#[tokio::test]
async fn partial_profile_inventory_is_rejected_before_a_write() {
    let server = MockServer::start(vec![Reply::json(200, br#"{"kind":"resourceList","totalCount":2,"matchingFilterCount":1,"elements":[{"id":"p1","kind":"ethernetPort","name":"One"}]}"#.to_vec())]);
    let client = make_client(&server, Duration::from_secs(2));
    let error = error(plan_clone_port_profile(&client, SITE, "p1", "Copy").await);
    assert_eq!(error.kind, ErrorKind::Unverified);
    assert_eq!(server.finish().len(), 1);
}

#[tokio::test]
async fn profile_collection_rejects_missing_kind_pending_and_pagination() {
    let cases = [
        json!({"totalCount":0,"matchingFilterCount":0,"elements":[]}),
        json!({"kind":"resourceList","totalCount":0,"matchingFilterCount":0,"elements":[],"pendingAvailability":["page"]}),
        json!({"kind":"resourceList","totalCount":0,"matchingFilterCount":0,"elements":[],"metaData":{"hasMore":true}}),
    ];
    for payload in cases {
        let server = MockServer::start(vec![reply(payload)]);
        let client = make_client(&server, Duration::from_secs(2));
        let error = error(plan_clone_port_profile(&client, SITE, "p1", "Copy").await);
        assert_eq!(error.kind, ErrorKind::Unverified);
        assert_eq!(server.finish().len(), 1);
    }
}

#[tokio::test]
async fn profile_read_fetches_full_item_when_collection_row_is_a_summary() {
    let full = profile(PROFILE_ID, "Desk");
    let summary = json!({"id":PROFILE_ID,"kind":PROFILE_KIND,"name":"Desk"});
    let server = MockServer::start(vec![
        Reply::json(200, profiles(vec![summary])),
        reply(full.clone()),
    ]);
    let client = make_client(&server, Duration::from_secs(2));
    let result = read_port_profile(&client, SITE, PROFILE_ID)
        .await
        .expect("full profile detail");
    assert_eq!(result.details(), full);
    let requests = server.finish();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[1].target,
        format!("/api/sites/{SITE}/portProfiles/{PROFILE_ID}")
    );
}

#[tokio::test]
async fn profile_delete_retains_plan_configuration_and_verifies_complete_collection_absence() {
    let full = profile(PROFILE_ID, "Desk");
    let server = MockServer::start(vec![
        Reply::json(200, profiles(vec![full.clone()])),
        Reply::json(200, inventory()),
        reply(json!({"id":PROFILE_ID})),
        Reply::json(200, profiles(vec![])),
    ]);
    let client = make_client(&server, Duration::from_secs(2));
    let prepared = plan_delete_port_profile(&client, SITE, PROFILE_ID, true)
        .await
        .expect("safe profile delete plan");
    assert_eq!(
        prepared.plan.current["configuration"]["vendorExtension"],
        full["vendorExtension"]
    );
    let report = apply_once(&prepared.backend, &prepared.plan, Duration::from_secs(2))
        .await
        .expect("delete report");
    assert_eq!(report.outcome, Outcome::Verified, "{report:?}");
    let requests = server.finish();
    assert_eq!(requests.len(), 4);
    assert_eq!(requests[2].method, "DELETE");
    assert_eq!(
        requests[3].target,
        format!("/api/sites/{SITE}/portProfiles")
    );
}

#[tokio::test]
async fn simple_poe_schedule_put_preserves_mappings_and_verifies_owned_fields() {
    let before = json!({
        "kind":"poeSchedule","activeSchedule":"none",
        "schedule":{"activeDays":[],"activeTimeRange":{"enabled":false,"vendorConfig":{"keep":true}},"vendorConfig":{"owner":"schedule"}},
        "weekSchedule":{"schedulePerWeekdayMap":{}},
        "poeScheduleDeviceMappings":[{"deviceId":"aa:bb:cc:dd:ee:ff","poeSchedulePortMappings":[{"portNumber":1,"usePoeSchedule":true}]}],
        "vendorExtension":{"keep":true}
    });
    let mut after = before.clone();
    after["activeSchedule"] = json!("simple");
    after["schedule"]["activeDays"] = json!(["monday", "wednesday"]);
    after["schedule"]["activeTimeRange"]["enabled"] = json!(true);
    after["schedule"]["activeTimeRange"]["startTime"] = json!("08:30");
    after["schedule"]["activeTimeRange"]["endTime"] = json!("17:00");
    after["schedule"]["activeTimeRange"]["vendorConfig"] = json!({"keep":false,"revision":2});
    after["schedule"]["vendorConfig"] = json!({"owner":"updated"});
    let server = MockServer::start(vec![
        Reply::json(200, inventory()),
        reply(before.clone()),
        reply(json!({"kind":"poeSchedule"})),
        reply(after.clone()),
    ]);
    let client = make_client(&server, Duration::from_secs(2));
    let prepared = plan_poe_schedule(
        &client,
        SITE,
        SchedulePatch {
            active_schedule: Some(ActiveSchedule::Simple),
            simple_schedule: Some(SimpleSchedule {
                active_days: vec![Weekday::Monday, Weekday::Wednesday],
                start_time: Some("08:30".into()),
                end_time: Some("17:00".into()),
            }),
            week_schedule: None,
        },
        true,
    )
    .await
    .expect("valid schedule plan");
    let report = apply_once(&prepared.backend, &prepared.plan, Duration::from_secs(2))
        .await
        .expect("schedule report");
    assert_eq!(report.outcome, Outcome::Verified);
    let requests = server.finish();
    assert_eq!(
        body(&requests[2])["poeScheduleDeviceMappings"],
        before["poeScheduleDeviceMappings"]
    );
    assert_eq!(
        body(&requests[2])["vendorExtension"],
        before["vendorExtension"]
    );
    assert_eq!(
        body(&requests[2])["schedule"]["vendorConfig"],
        before["schedule"]["vendorConfig"]
    );
    assert_eq!(
        body(&requests[2])["schedule"]["activeTimeRange"]["vendorConfig"],
        before["schedule"]["activeTimeRange"]["vendorConfig"]
    );
    assert_eq!(
        body(&requests[2])["schedule"]["activeDays"],
        after["schedule"]["activeDays"]
    );
}

#[tokio::test]
async fn all_day_schedule_removes_only_legacy_time_fields() {
    let before = json!({
        "kind":"poeSchedule","activeSchedule":"simple",
        "schedule":{"activeDays":["monday"],"activeTimeRange":{"enabled":true,"startTime":"08:00","endTime":"17:00","vendorConfig":{"keep":true},"otherFlag":"retain"}},
        "weekSchedule":{"schedulePerWeekdayMap":{}},"poeScheduleDeviceMappings":[]
    });
    let mut after = before.clone();
    after["schedule"]["activeTimeRange"]["enabled"] = json!(false);
    after["schedule"]["activeTimeRange"]
        .as_object_mut()
        .unwrap()
        .remove("startTime");
    after["schedule"]["activeTimeRange"]
        .as_object_mut()
        .unwrap()
        .remove("endTime");
    let server = MockServer::start(vec![
        Reply::json(200, inventory()),
        reply(before.clone()),
        reply(json!({"kind":"poeSchedule"})),
        reply(after),
    ]);
    let client = make_client(&server, Duration::from_secs(2));
    let prepared = plan_poe_schedule(
        &client,
        SITE,
        SchedulePatch {
            active_schedule: Some(ActiveSchedule::Simple),
            simple_schedule: Some(SimpleSchedule {
                active_days: vec![Weekday::Monday],
                start_time: None,
                end_time: None,
            }),
            week_schedule: None,
        },
        true,
    )
    .await
    .expect("all-day schedule plan");
    let report = apply_once(&prepared.backend, &prepared.plan, Duration::from_secs(2))
        .await
        .expect("all-day schedule report");
    assert_eq!(report.outcome, Outcome::Verified, "{report:?}");
    let requests = server.finish();
    let range = &body(&requests[2])["schedule"]["activeTimeRange"];
    assert_eq!(range["enabled"], json!(false));
    assert!(range.get("startTime").is_none());
    assert!(range.get("endTime").is_none());
    assert_eq!(
        range["vendorConfig"],
        before["schedule"]["activeTimeRange"]["vendorConfig"]
    );
    assert_eq!(range["otherFlag"], json!("retain"));
}

#[tokio::test]
async fn weekly_schedule_put_preserves_nested_extensions_and_ignores_readback_metadata_changes() {
    let days = [
        Weekday::Monday,
        Weekday::Tuesday,
        Weekday::Wednesday,
        Weekday::Thursday,
        Weekday::Friday,
        Weekday::Saturday,
        Weekday::Sunday,
    ];
    let mut input = BTreeMap::new();
    for day in days {
        input.insert(
            day,
            if day == Weekday::Monday {
                WeekdaySchedule {
                    enabled: true,
                    active_all_day: false,
                    start_time: Some("08:00".into()),
                    end_time: Some("17:00".into()),
                }
            } else {
                WeekdaySchedule {
                    enabled: false,
                    active_all_day: false,
                    start_time: None,
                    end_time: None,
                }
            },
        );
    }
    let before = json!({
        "kind":"poeSchedule","activeSchedule":"week",
        "schedule":{"activeDays":[],"activeTimeRange":{"enabled":false}},
        "weekSchedule":{"vendorConfig":{"keep":true},"schedulePerWeekdayMap":{
            "monday":{"enabled":false,"activeAllDay":false,"startTime":"01:00","endTime":"02:00","vendorConfig":{"keep":true}},
            "tuesday":{"enabled":false,"activeAllDay":false,"startTime":"01:00","endTime":"02:00","vendorConfig":{"keep":true}},
            "wednesday":{"enabled":false,"activeAllDay":false,"vendorConfig":{"keep":true}},
            "thursday":{"enabled":false,"activeAllDay":false,"vendorConfig":{"keep":true}},
            "friday":{"enabled":false,"activeAllDay":false,"vendorConfig":{"keep":true}},
            "saturday":{"enabled":false,"activeAllDay":false,"vendorConfig":{"keep":true}},
            "sunday":{"enabled":false,"activeAllDay":false,"vendorConfig":{"keep":true}}
        }},
        "poeScheduleDeviceMappings":[]
    });
    let mut after = before.clone();
    after["weekSchedule"]["vendorConfig"] = json!({"keep":false,"revision":2});
    after["weekSchedule"]["schedulePerWeekdayMap"]["monday"]["enabled"] = json!(true);
    after["weekSchedule"]["schedulePerWeekdayMap"]["monday"]["startTime"] = json!("08:00");
    after["weekSchedule"]["schedulePerWeekdayMap"]["monday"]["endTime"] = json!("17:00");
    after["weekSchedule"]["schedulePerWeekdayMap"]["monday"]["vendorConfig"] =
        json!({"keep":false});
    for day in [
        "tuesday",
        "wednesday",
        "thursday",
        "friday",
        "saturday",
        "sunday",
    ] {
        after["weekSchedule"]["schedulePerWeekdayMap"][day]["vendorConfig"] =
            json!({"changed":true});
    }
    let server = MockServer::start(vec![
        Reply::json(200, inventory()),
        reply(before.clone()),
        reply(json!({"kind":"poeSchedule"})),
        reply(after),
    ]);
    let client = make_client(&server, Duration::from_secs(2));
    let prepared = plan_poe_schedule(
        &client,
        SITE,
        SchedulePatch {
            active_schedule: Some(ActiveSchedule::Week),
            simple_schedule: None,
            week_schedule: Some(WeekSchedule { days: input }),
        },
        true,
    )
    .await
    .expect("valid week schedule plan");
    let report = apply_once(&prepared.backend, &prepared.plan, Duration::from_secs(2))
        .await
        .expect("weekly schedule report");
    assert_eq!(report.outcome, Outcome::Verified, "{report:?}");
    let requests = server.finish();
    let sent = body(&requests[2]);
    assert_eq!(
        sent["weekSchedule"]["vendorConfig"],
        before["weekSchedule"]["vendorConfig"]
    );
    assert_eq!(
        sent["weekSchedule"]["schedulePerWeekdayMap"]["monday"]["vendorConfig"],
        before["weekSchedule"]["schedulePerWeekdayMap"]["monday"]["vendorConfig"]
    );
    assert_eq!(
        sent["weekSchedule"]["schedulePerWeekdayMap"]["monday"]["startTime"],
        json!("08:00")
    );
    assert!(
        sent["weekSchedule"]["schedulePerWeekdayMap"]["monday"]
            .get("activeAllDay")
            .is_some()
    );
    for day in [
        "tuesday",
        "wednesday",
        "thursday",
        "friday",
        "saturday",
        "sunday",
    ] {
        assert_eq!(
            sent["weekSchedule"]["schedulePerWeekdayMap"][day]["vendorConfig"],
            json!({"keep":true})
        );
    }
    assert!(
        sent["weekSchedule"]["schedulePerWeekdayMap"]["tuesday"]
            .get("startTime")
            .is_none()
    );
}

#[tokio::test]
async fn invalid_schedule_is_rejected_before_inventory_or_resource_requests() {
    let server = MockServer::start(vec![]);
    let client = make_client(&server, Duration::from_secs(2));
    let error = error(
        plan_poe_schedule(
            &client,
            SITE,
            SchedulePatch {
                active_schedule: None,
                simple_schedule: Some(SimpleSchedule {
                    active_days: vec![],
                    start_time: Some("8:30".into()),
                    end_time: Some("17:00".into()),
                }),
                week_schedule: None,
            },
            true,
        )
        .await,
    );
    assert_eq!(error.kind, ErrorKind::Usage);
    assert!(server.finish().is_empty());
}

#[tokio::test]
async fn eee_put_preserves_poe_schedule_and_reads_back_boolean() {
    let before = json!({"kind":"powerManagement","isEnergyEfficientEthernetEnabled":false,"poeSchedule":{"kind":"poeSchedule","activeSchedule":"week","schedule":{"activeDays":[]},"weekSchedule":{"schedulePerWeekdayMap":{}},"poeScheduleDeviceMappings":[]},"vendorExtension":{"keep":1}});
    let mut after = before.clone();
    after["isEnergyEfficientEthernetEnabled"] = json!(true);
    let server = MockServer::start(vec![
        Reply::json(200, inventory()),
        reply(before.clone()),
        reply(json!({"kind":"powerManagement"})),
        reply(after.clone()),
    ]);
    let client = make_client(&server, Duration::from_secs(2));
    let prepared = plan_eee(&client, SITE, true, true)
        .await
        .expect("valid EEE plan");
    let report = apply_once(&prepared.backend, &prepared.plan, Duration::from_secs(2))
        .await
        .expect("EEE report");
    assert_eq!(report.outcome, Outcome::Verified);
    let requests = server.finish();
    assert_eq!(body(&requests[2])["poeSchedule"], before["poeSchedule"]);
    assert_eq!(
        body(&requests[2])["vendorExtension"],
        before["vendorExtension"]
    );
    assert_eq!(
        body(&requests[2])["isEnergyEfficientEthernetEnabled"],
        after["isEnergyEfficientEthernetEnabled"]
    );
}

#[tokio::test]
async fn inactive_null_poe_configuration_is_preserved_by_both_read_owners() {
    let schedule = json!({
        "kind":"poeSchedule",
        "activeSchedule":"none",
        "schedule":null,
        "weekSchedule":null,
        "poeScheduleDeviceMappings":[{"deviceId":"aa:bb:cc:dd:ee:ff","vendor":"retain"}],
        "vendorExtension":{"retain":true}
    });
    let server = MockServer::start(vec![reply(schedule.clone())]);
    let client = make_client(&server, Duration::from_secs(2));
    assert_eq!(
        poe_schedule(&client, SITE).await.expect("PoE schedule"),
        schedule
    );
    let requests = server.finish();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].target, format!("/api/sites/{SITE}/poeSchedule"));

    let power = json!({
        "kind":"powerManagement",
        "isEnergyEfficientEthernetEnabled":true,
        "poeSchedule":schedule,
        "vendorExtension":{"retain":true}
    });
    let server = MockServer::start(vec![reply(power.clone())]);
    let client = make_client(&server, Duration::from_secs(2));
    assert_eq!(
        power_management(&client, SITE)
            .await
            .expect("power management"),
        power
    );
    let requests = server.finish();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].target,
        format!("/api/sites/{SITE}/powerManagement")
    );
}

#[tokio::test]
async fn malformed_or_active_null_poe_schedules_remain_unverified() {
    let valid = || {
        json!({
            "kind":"poeSchedule",
            "activeSchedule":"none",
            "schedule":null,
            "weekSchedule":null,
            "poeScheduleDeviceMappings":[]
        })
    };
    let mut cases = Vec::new();

    let mut missing_schedule = valid();
    missing_schedule.as_object_mut().unwrap().remove("schedule");
    cases.push(("missing schedule", missing_schedule));

    let mut missing_week = valid();
    missing_week.as_object_mut().unwrap().remove("weekSchedule");
    cases.push(("missing week schedule", missing_week));

    let mut scalar_schedule = valid();
    scalar_schedule["schedule"] = json!("garbage");
    cases.push(("scalar schedule", scalar_schedule));

    let mut scalar_week = valid();
    scalar_week["weekSchedule"] = json!([]);
    cases.push(("scalar week schedule", scalar_week));

    let mut unknown_kind = valid();
    unknown_kind["kind"] = json!("unexpected");
    cases.push(("unknown kind", unknown_kind));

    let mut unknown_active = valid();
    unknown_active["activeSchedule"] = json!("monthly");
    cases.push(("unknown active schedule", unknown_active));

    let mut active_simple_without_config = valid();
    active_simple_without_config["activeSchedule"] = json!("simple");
    cases.push((
        "active simple without configuration",
        active_simple_without_config,
    ));

    let mut active_week_without_config = valid();
    active_week_without_config["activeSchedule"] = json!("week");
    cases.push((
        "active week without configuration",
        active_week_without_config,
    ));

    for (label, response) in cases {
        let server = MockServer::start(vec![reply(response)]);
        let client = make_client(&server, Duration::from_secs(2));
        let failure = error(poe_schedule(&client, SITE).await);
        assert_eq!(failure.kind, ErrorKind::Unverified, "{label}: {failure}");
        assert_eq!(server.finish().len(), 1, "{label}");
    }

    for active in [ActiveSchedule::Simple, ActiveSchedule::Week] {
        let server = MockServer::start(vec![Reply::json(200, inventory()), reply(valid())]);
        let client = make_client(&server, Duration::from_secs(2));
        let failure = error(
            plan_poe_schedule(
                &client,
                SITE,
                SchedulePatch {
                    active_schedule: Some(active),
                    ..SchedulePatch::default()
                },
                true,
            )
            .await,
        );
        assert_eq!(failure.kind, ErrorKind::Unverified, "{active:?}");
        let requests = server.finish();
        assert_eq!(requests.len(), 2, "{active:?}");
        assert!(
            requests.iter().all(|request| request.method == "GET"),
            "{active:?}"
        );
    }
}
