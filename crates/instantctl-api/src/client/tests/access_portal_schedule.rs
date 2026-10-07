use super::*;
use crate::{
    Error, ErrorKind,
    client::access::{guest_portal, schedule},
    mutation::Outcome,
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, time::Duration};

const SITE: &str = "123e4567-e89b-12d3-a456-426614174000";
const SCHEDULE_ID: &str = "schedule-opaque-17";
const SECOND_SCHEDULE_ID: &str = "schedule-opaque-22";
const SCHEDULE_NAME: &str = "Quiet hours";

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

fn schedule_collection(rows: Vec<Value>, default: Value, max_elements: u64) -> Value {
    json!({
        "kind":"schedules",
        "elements":rows,
        "maxElements":max_elements,
        "metaData":{"defaultSchedule":default,"maxLengthOfScheduleNames":32}
    })
}

fn week_template() -> Value {
    let mut by_day = serde_json::Map::new();
    for day in [
        "monday",
        "tuesday",
        "wednesday",
        "thursday",
        "friday",
        "saturday",
        "sunday",
    ] {
        by_day.insert(
            day.to_owned(),
            json!({"timeRangePeriod":"activeAllDay","dayExtension":{"keep":day}}),
        );
    }
    json!({
        "id":"template-id",
        "name":"Default",
        "activeSchedule":"simple",
        "schedule":{
            "activeDays":["monday","tuesday","wednesday","thursday","friday","saturday","sunday"],
            "activeTimeRange":{"timeRangePeriod":"activeAllDay","simpleExtension":{"keep":true}},
            "simpleExtension":{"retain":[1,2]}
        },
        "weekSchedule":{"schedulePerWeekdayMap":by_day,"weekExtension":{"retain":true}},
        "rootExtension":{"preserve":"template"}
    })
}

fn schedule_row(id: &str, name: &str) -> Value {
    let mut row = week_template();
    row["id"] = json!(id);
    row["name"] = json!(name);
    row["referencingPolicies"] = json!([]);
    row["schedule"]["activeTimeRange"]["startTime"] = json!("09:00");
    row["schedule"]["activeTimeRange"]["endTime"] = json!("17:00");
    row
}

fn internal_portal() -> Value {
    json!({
        "kind":"guestPortalSettings",
        "guestPortalType":"internalAck",
        "internalAckPageSettings":{
            "kind":"captivePortalSettings",
            "backgroundColor":"#ffffff",
            "welcomeMsgText":"Welcome",
            "welcomeMsgFontSizeInPx":24,
            "welcomeMsgFontColor":"#111111",
            "welcomeMsgFontFamily":"Arial",
            "logoImageFile":"image-data-sentinel",
            "termsTitle":"Terms",
            "termsTitleFontSizeInPx":16,
            "termsTitleFontColor":"#111111",
            "termsTitleFontFamily":"Arial",
            "termsContent":"Read this",
            "termsAgreeText":"I agree",
            "termsAgreeTextFontColor":"#111111",
            "termsAgreeTextFontFamily":"Arial",
            "acceptBtnText":"Continue",
            "acceptBtnBorderRadiusInPx":4,
            "acceptBtnBackgroundColor":"#0066cc",
            "acceptBtnFontColor":"#ffffff",
            "acceptBtnFontFamily":"Arial",
            "redirectUrl":"",
            "opaqueInternal":{"keep":true}
        },
        "externalPageSettings":{
            "kind":"externalCaptivePortalSettings",
            "serverHost":"portal.example.test",
            "serverUrlPath":"/login",
            "serverPort":443,
            "useHttps":true,
            "redirectUrl":"",
            "socialLogins":["facebook"],
            "whitelistedDomains":["example.test"],
            "isAuthenticationRequired":true,
            "canDisableAuthentication":true,
            "radiusProfileId":"radius-existing",
            "radiusServerPrimary":{
                "serverHost":"radius.example.test",
                "sharedSecret":"portal-secret-sentinel",
                "timeout":5,
                "retryCount":3,
                "authPort":1812,
                "accountingPort":1813
            },
            "opaqueExternal":{"keep":true}
        },
        "opaqueRoot":{"retain":["future","field"]}
    })
}

#[tokio::test(start_paused = true)]
async fn guest_internal_update_puts_full_object_reads_back_and_redacts_secrets() {
    let _clock = keep_clock_paused().await;
    let current = internal_portal();
    let mut observed = current.clone();
    observed["internalAckPageSettings"]["welcomeMsgText"] = json!("Welcome home");
    let server = MockServer::start(vec![
        reply_json(current.clone()),
        Reply::empty(204),
        reply_json(observed),
    ]);
    let api = make_client(&server, Duration::from_secs(1));
    let patch = guest_portal::Patch {
        welcome: Some("Welcome home".into()),
        ..Default::default()
    };
    let (mutation, plan) = guest_portal::update(&api, SITE, patch)
        .await
        .expect("prepare guest portal update");
    assert_eq!(mutation.target(), json!({"resource":"guestPortalSettings"}));
    assert!(!format!("{plan:?}").contains("portal-secret-sentinel"));
    assert!(!format!("{plan:?}").contains("sharedSecret"));

    let report = apply_readback_once(&mutation, &plan, Duration::from_secs(1))
        .await
        .expect("apply should return a report");
    assert_eq!(report.outcome, Outcome::Verified);
    assert_eq!(report.readback_attempts, 1);
    let requests = server.finish();
    assert_eq!(requests.len(), 3, "GET, one PUT, and a fresh GET");
    assert_request(
        &requests[0],
        "GET",
        &format!("/api/sites/{SITE}/guestPortalSettings"),
    );
    assert_request(
        &requests[1],
        "PUT",
        &format!("/api/sites/{SITE}/guestPortalSettings"),
    );
    let body = request_json(&requests[1]);
    assert_eq!(
        body["internalAckPageSettings"]["welcomeMsgText"],
        "Welcome home"
    );
    assert_eq!(
        body["internalAckPageSettings"]["opaqueInternal"]["keep"],
        true
    );
    assert_eq!(body["externalPageSettings"]["opaqueExternal"]["keep"], true);
    assert_eq!(body["opaqueRoot"]["retain"][0], "future");
    assert_eq!(
        body["externalPageSettings"]["radiusServerPrimary"]["sharedSecret"],
        "portal-secret-sentinel"
    );
    assert_request(
        &requests[2],
        "GET",
        &format!("/api/sites/{SITE}/guestPortalSettings"),
    );
}

#[tokio::test]
async fn guest_portal_show_redacts_nested_secrets_while_preserving_full_shape() {
    let current = internal_portal();
    let server = MockServer::start(vec![reply_json(current)]);
    let api = make_client(&server, Duration::from_secs(1));
    let visible = guest_portal::show(&api, SITE)
        .await
        .expect("read guest portal settings");
    assert_eq!(visible["guestPortalType"], "internalAck");
    assert_eq!(
        visible["internalAckPageSettings"]["opaqueInternal"]["keep"],
        true
    );
    assert_eq!(
        visible["externalPageSettings"]["radiusServerPrimary"]["sharedSecret"],
        "(redacted)"
    );
    assert_eq!(visible["opaqueRoot"]["retain"][1], "field");
    assert!(!format!("{visible:?}").contains("portal-secret-sentinel"));
    assert_eq!(server.finish().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn external_url_and_radius_selector_resolve_to_wire_fields() {
    let _clock = keep_clock_paused().await;
    let profile_payload = json!({
        "kind":"radiusProfiles",
        "elements":[{"id":"radius-new","name":"Remote RADIUS","primaryServer":{"serverHost":"radius.example.test","sharedSecret":"profile-secret"}}]
    });
    let current = internal_portal();
    let mut observed = current.clone();
    observed["guestPortalType"] = json!("external");
    observed["externalPageSettings"]["serverHost"] = json!("login.example.test");
    observed["externalPageSettings"]["serverUrlPath"] = json!("/start?plan=guest");
    observed["externalPageSettings"]["serverPort"] = json!(443);
    observed["externalPageSettings"]["useHttps"] = json!(true);
    observed["externalPageSettings"]["radiusProfileId"] = json!("radius-new");
    let server = MockServer::start(vec![
        reply_json(profile_payload),
        reply_json(current),
        Reply::empty(204),
        reply_json(observed),
    ]);
    let api = make_client(&server, Duration::from_secs(1));
    let patch = guest_portal::Patch {
        portal_type: Some(guest_portal::PortalType::External),
        external_url: Some("https://login.example.test/start?plan=guest".into()),
        radius_profile: Some("Remote RADIUS".into()),
        ..Default::default()
    };
    let (mutation, plan) = guest_portal::update(&api, SITE, patch)
        .await
        .expect("prepare external portal update");
    let report = apply_readback_once(&mutation, &plan, Duration::from_secs(1))
        .await
        .expect("apply should return a report");
    assert_eq!(report.outcome, Outcome::Verified);
    let requests = server.finish();
    assert_eq!(requests.len(), 4, "profile lookup, GET, one PUT, readback");
    assert_request(
        &requests[0],
        "GET",
        &format!("/api/sites/{SITE}/radiusProfiles"),
    );
    assert_request(
        &requests[1],
        "GET",
        &format!("/api/sites/{SITE}/guestPortalSettings"),
    );
    assert_request(
        &requests[2],
        "PUT",
        &format!("/api/sites/{SITE}/guestPortalSettings"),
    );
    let body = request_json(&requests[2]);
    assert_eq!(body["guestPortalType"], "external");
    assert_eq!(
        body["externalPageSettings"]["serverHost"],
        "login.example.test"
    );
    assert_eq!(
        body["externalPageSettings"]["serverUrlPath"],
        "/start?plan=guest"
    );
    assert_eq!(body["externalPageSettings"]["serverPort"], 443);
    assert_eq!(body["externalPageSettings"]["useHttps"], true);
    assert_eq!(
        body["externalPageSettings"]["radiusProfileId"],
        "radius-new"
    );
    assert_eq!(
        body["internalAckPageSettings"]["opaqueInternal"]["keep"],
        true
    );
}

#[tokio::test(start_paused = true)]
async fn unauthenticated_external_portal_accepts_http_and_rejects_https_without_put() {
    let _clock = keep_clock_paused().await;
    let mut current = internal_portal();
    current["guestPortalType"] = json!("external");
    let mut observed = current.clone();
    observed["externalPageSettings"]["serverHost"] = json!("legacy.example.test");
    observed["externalPageSettings"]["serverUrlPath"] = json!("/guest");
    observed["externalPageSettings"]["serverPort"] = json!(80);
    observed["externalPageSettings"]["useHttps"] = json!(false);
    observed["externalPageSettings"]["isAuthenticationRequired"] = json!(false);
    let server = MockServer::start(vec![
        reply_json(current.clone()),
        Reply::empty(204),
        reply_json(observed),
        reply_json(current),
    ]);
    let api = make_client(&server, Duration::from_secs(1));
    let (mutation, plan) = guest_portal::update(
        &api,
        SITE,
        guest_portal::Patch {
            external_url: Some("http://legacy.example.test/guest".into()),
            authentication: Some(false),
            ..Default::default()
        },
    )
    .await
    .expect("prepare unauthenticated HTTP portal");
    let report = apply_readback_once(&mutation, &plan, Duration::from_secs(1))
        .await
        .expect("apply should return a report");
    assert_eq!(report.outcome, Outcome::Verified);
    let error = expect_error(
        guest_portal::update(
            &api,
            SITE,
            guest_portal::Patch {
                external_url: Some("https://secure.example.test/guest".into()),
                authentication: Some(false),
                ..Default::default()
            },
        )
        .await,
    );
    assert_eq!(error.kind, ErrorKind::Usage);
    let requests = server.finish();
    assert_eq!(
        requests.len(),
        4,
        "valid write is followed by one GET for invalid dependent state"
    );
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.method == "PUT")
            .count(),
        1
    );
    let body = request_json(&requests[1]);
    assert_eq!(
        body["externalPageSettings"]["serverHost"],
        "legacy.example.test"
    );
    assert_eq!(body["externalPageSettings"]["serverPort"], 80);
    assert_eq!(body["externalPageSettings"]["useHttps"], false);
}

#[tokio::test]
async fn guest_portal_invalid_controls_fail_before_any_request() {
    for patch in [
        guest_portal::Patch {
            welcome: Some("  ".into()),
            ..Default::default()
        },
        guest_portal::Patch {
            welcome_size: Some(17),
            ..Default::default()
        },
        guest_portal::Patch {
            font_family: Some("Comic Sans".into()),
            ..Default::default()
        },
        guest_portal::Patch {
            external_url: Some("javascript:alert(1)".into()),
            ..Default::default()
        },
    ] {
        let server = MockServer::start(vec![]);
        let api = make_client(&server, Duration::from_secs(1));
        let error = expect_error(guest_portal::update(&api, SITE, patch).await);
        assert_eq!(error.kind, ErrorKind::Usage);
        assert!(server.finish().is_empty(), "validation must precede GET");
    }
}

#[tokio::test]
async fn guest_portal_unknown_or_missing_configuration_is_readable_as_null_but_not_writable() {
    let mut unknown = internal_portal();
    unknown["guestPortalType"] = json!("futurePortal");
    unknown
        .as_object_mut()
        .unwrap()
        .remove("externalPageSettings");
    let server = MockServer::start(vec![reply_json(unknown.clone()), reply_json(unknown)]);
    let api = make_client(&server, Duration::from_secs(1));
    let visible = guest_portal::show(&api, SITE)
        .await
        .expect("show should retain partial status");
    assert_eq!(visible["guestPortalType"], Value::Null);
    assert_eq!(visible["externalPageSettings"], Value::Null);
    let error = expect_error(
        guest_portal::update(
            &api,
            SITE,
            guest_portal::Patch {
                welcome: Some("new text".into()),
                ..Default::default()
            },
        )
        .await,
    );
    assert_eq!(error.kind, ErrorKind::Unverified);
    let requests = server.finish();
    assert_eq!(requests.len(), 2);
    assert!(requests.iter().all(|request| request.method == "GET"));
}

#[tokio::test]
async fn schedule_list_summary_and_selector_report_unknown_fields_safely() {
    let mut row = schedule_row(SCHEDULE_ID, SCHEDULE_NAME);
    row["activeSchedule"] = json!("futureMode");
    row.as_object_mut().unwrap().remove("referencingPolicies");
    row["extension"]["sharedSecret"] = json!("schedule-secret-sentinel");
    let payload = schedule_collection(vec![row.clone()], week_template(), 16);
    let server = MockServer::start(vec![reply_json(payload)]);
    let api = make_client(&server, Duration::from_secs(1));
    let rows = schedule::list(&api, SITE).await.expect("list schedules");
    assert_eq!(rows.len(), 1);
    assert_eq!(
        schedule::summary(&rows[0]),
        json!({"id":SCHEDULE_ID,"name":SCHEDULE_NAME,"mode":null,"policy_references":null})
    );
    assert_eq!(
        schedule::show(&rows, SCHEDULE_NAME).expect("select by name")["id"],
        SCHEDULE_ID
    );
    assert!(!format!("{:?}", rows[0]).contains("schedule-secret-sentinel"));
    let requests = server.finish();
    assert_eq!(requests.len(), 1);
    assert_request(&requests[0], "GET", &format!("/api/sites/{SITE}/schedules"));

    let ambiguous = vec![
        schedule_row(SCHEDULE_ID, SCHEDULE_NAME),
        schedule_row(SECOND_SCHEDULE_ID, SCHEDULE_NAME),
    ];
    assert_eq!(
        schedule::show(&ambiguous, SCHEDULE_NAME).unwrap_err().kind,
        ErrorKind::Usage
    );
    assert_eq!(
        schedule::show(&ambiguous, "missing").unwrap_err().kind,
        ErrorKind::NotFound
    );
}

#[tokio::test(start_paused = true)]
async fn create_schedule_uses_template_switches_to_week_and_verifies_created_id() {
    let _clock = keep_clock_paused().await;
    let template = week_template();
    let payload = schedule_collection(vec![], template.clone(), 16);
    let mut expected = json!({
        "id":"created-schedule",
        "name":"Night shift",
        "activeSchedule":"week",
        "schedule":template["schedule"],
        "weekSchedule":template["weekSchedule"],
        "rootExtension":template["rootExtension"]
    });
    expected["weekSchedule"]["schedulePerWeekdayMap"]["monday"]["timeRangePeriod"] =
        json!("activeBetweenStartTimeAndEndTime");
    expected["weekSchedule"]["schedulePerWeekdayMap"]["monday"]["startTime"] = json!("22:30");
    expected["weekSchedule"]["schedulePerWeekdayMap"]["monday"]["endTime"] = json!("06:15");
    let mut readback = expected.clone();
    readback["referencingPolicies"] = json!([]);
    let server = MockServer::start(vec![
        reply_json(payload.clone()),
        reply_json(json!({"id":"created-schedule","name":"Night shift"})),
        reply_json(schedule_collection(vec![readback], template, 16)),
    ]);
    let api = make_client(&server, Duration::from_secs(1));
    let mut week = BTreeMap::new();
    week.insert(
        schedule::Day::Monday,
        schedule::TimeRange::Between {
            start: "22:30".into(),
            end: "06:15".into(),
        },
    );
    let patch = schedule::Patch {
        name: Some("Night shift".into()),
        mode: Some(schedule::Mode::Week),
        week,
        ..Default::default()
    };
    let (mutation, plan) = schedule::create(&api, SITE, patch)
        .await
        .expect("prepare schedule creation from default template");
    assert_eq!(
        serde_json::to_value(&plan.current).expect("serialize plan state")["exists"],
        false
    );
    let report = apply_readback_once(&mutation, &plan, Duration::from_secs(1))
        .await
        .expect("apply should return a report");
    assert_eq!(report.outcome, Outcome::Verified);
    let requests = server.finish();
    assert_eq!(requests.len(), 3, "GET, one POST, readback GET");
    assert_request(&requests[0], "GET", &format!("/api/sites/{SITE}/schedules"));
    assert_request(
        &requests[1],
        "POST",
        &format!("/api/sites/{SITE}/schedules"),
    );
    let body = request_json(&requests[1]);
    assert_eq!(body["name"], "Night shift");
    assert_eq!(body["activeSchedule"], "week");
    assert_eq!(
        body["weekSchedule"]["schedulePerWeekdayMap"]["monday"]["timeRangePeriod"],
        "activeBetweenStartTimeAndEndTime"
    );
    assert_eq!(
        body["weekSchedule"]["schedulePerWeekdayMap"]["monday"]["startTime"],
        "22:30"
    );
    assert_eq!(
        body["weekSchedule"]["schedulePerWeekdayMap"]["monday"]["endTime"],
        "06:15"
    );
    assert_eq!(
        body["weekSchedule"]["schedulePerWeekdayMap"]["tuesday"]["dayExtension"]["keep"],
        "tuesday"
    );
    assert_eq!(body["weekSchedule"]["weekExtension"]["retain"], true);
    assert_eq!(body["schedule"]["simpleExtension"]["retain"][1], 2);
    assert!(body.get("id").is_none(), "template ID must not be reused");
    assert_request(&requests[2], "GET", &format!("/api/sites/{SITE}/schedules"));
}

#[tokio::test(start_paused = true)]
async fn simple_all_day_update_preserves_full_object_and_ignores_unused_times_on_readback() {
    let _clock = keep_clock_paused().await;
    let mut current = schedule_row(SCHEDULE_ID, SCHEDULE_NAME);
    current["activeSchedule"] = json!("simple");
    current["schedule"]["activeTimeRange"]["timeRangePeriod"] =
        json!("activeBetweenStartTimeAndEndTime");
    current["schedule"]["activeTimeRange"]["startTime"] = json!("09:00");
    current["schedule"]["activeTimeRange"]["endTime"] = json!("17:00");
    let mut returned = current.clone();
    returned["schedule"]["activeTimeRange"]["timeRangePeriod"] = json!("activeAllDay");
    // A server may retain stale times even when the active period does not use them.
    let server = MockServer::start(vec![
        reply_json(schedule_collection(
            vec![current.clone()],
            week_template(),
            16,
        )),
        Reply::empty(204),
        reply_json(schedule_collection(vec![returned], week_template(), 16)),
    ]);
    let api = make_client(&server, Duration::from_secs(1));
    let patch = schedule::Patch {
        range: Some(schedule::TimeRange::AllDay),
        ..Default::default()
    };
    let (mutation, plan) = schedule::update(&api, SITE, SCHEDULE_ID, patch)
        .await
        .expect("prepare schedule update");
    let report = apply_readback_once(&mutation, &plan, Duration::from_secs(1))
        .await
        .expect("apply should return a report");
    assert_eq!(report.outcome, Outcome::Verified);
    let requests = server.finish();
    assert_eq!(requests.len(), 3, "GET, one PUT, readback GET");
    assert_request(
        &requests[1],
        "PUT",
        &format!("/api/sites/{SITE}/schedules/{SCHEDULE_ID}"),
    );
    let body = request_json(&requests[1]);
    assert_eq!(
        body["schedule"]["activeTimeRange"]["timeRangePeriod"],
        "activeAllDay"
    );
    assert!(
        body["schedule"]["activeTimeRange"]
            .get("startTime")
            .is_none()
    );
    assert!(body["schedule"]["activeTimeRange"].get("endTime").is_none());
    assert_eq!(
        body["schedule"]["activeTimeRange"]["simpleExtension"]["keep"],
        true
    );
    assert_eq!(body["rootExtension"]["preserve"], "template");
}

#[tokio::test(start_paused = true)]
async fn disabling_schedule_preserves_nested_template_data_and_rejects_empty_patch() {
    let _clock = keep_clock_paused().await;
    let current = schedule_row(SCHEDULE_ID, SCHEDULE_NAME);
    let server = MockServer::start(vec![
        reply_json(schedule_collection(
            vec![current.clone()],
            week_template(),
            16,
        )),
        Reply::empty(204),
        reply_json(schedule_collection(
            {
                let mut row = current.clone();
                row["activeSchedule"] = json!("none");
                vec![row]
            },
            week_template(),
            16,
        )),
        reply_json(schedule_collection(vec![current], week_template(), 16)),
    ]);
    let api = make_client(&server, Duration::from_secs(1));
    let (mutation, plan) = schedule::update(
        &api,
        SITE,
        SCHEDULE_ID,
        schedule::Patch {
            mode: Some(schedule::Mode::None),
            ..Default::default()
        },
    )
    .await
    .expect("prepare schedule disable");
    assert_eq!(
        serde_json::to_value(&plan.desired).expect("serialize desired state")["configuration"]["activeSchedule"],
        "none"
    );
    let report = apply_readback_once(&mutation, &plan, Duration::from_secs(1))
        .await
        .expect("apply should return a report");
    assert_eq!(report.outcome, Outcome::Verified);
    let error =
        expect_error(schedule::update(&api, SITE, SCHEDULE_ID, schedule::Patch::default()).await);
    assert_eq!(error.kind, ErrorKind::Usage);
    let requests = server.finish();
    assert_eq!(requests.len(), 3, "empty patch must not issue a GET or PUT");
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.method == "PUT")
            .count(),
        1
    );
    let body = request_json(&requests[1]);
    assert_eq!(body["activeSchedule"], "none");
    assert_eq!(body["schedule"]["simpleExtension"]["retain"][0], 1);
    assert_eq!(body["weekSchedule"]["weekExtension"]["retain"], true);
    // The second call has no patch and makes no request.
}

#[tokio::test]
async fn schedule_update_preview_reads_the_full_row_without_putting() {
    let current = schedule_row(SCHEDULE_ID, SCHEDULE_NAME);
    let server = MockServer::start(vec![reply_json(schedule_collection(
        vec![current],
        week_template(),
        16,
    ))]);
    let api = make_client(&server, Duration::from_secs(1));
    let (_mutation, plan) = schedule::update(
        &api,
        SITE,
        SCHEDULE_ID,
        schedule::Patch {
            name: Some("Preview only".into()),
            ..Default::default()
        },
    )
    .await
    .expect("prepare preview");
    assert_eq!(
        serde_json::to_value(&plan.desired).expect("serialize desired state")["configuration"]["name"],
        "Preview only"
    );
    let requests = server.finish();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "GET");
}

#[tokio::test]
async fn invalid_schedule_controls_fail_before_get_and_capacity_or_malformed_rows_never_write() {
    for patch in [
        schedule::Patch {
            range: Some(schedule::TimeRange::Between {
                start: "24:00".into(),
                end: "06:00".into(),
            }),
            ..Default::default()
        },
        schedule::Patch {
            range: Some(schedule::TimeRange::Between {
                start: "09:00".into(),
                end: "09:00".into(),
            }),
            ..Default::default()
        },
        schedule::Patch {
            range: Some(schedule::TimeRange::Between {
                start: "8:00".into(),
                end: "08:00".into(),
            }),
            ..Default::default()
        },
        schedule::Patch {
            days: Some(vec![schedule::Day::Monday, schedule::Day::Monday]),
            ..Default::default()
        },
    ] {
        let server = MockServer::start(vec![]);
        let api = make_client(&server, Duration::from_secs(1));
        let error = expect_error(schedule::update(&api, SITE, SCHEDULE_ID, patch).await);
        assert_eq!(error.kind, ErrorKind::Usage);
        assert!(server.finish().is_empty());
    }

    let full = schedule_row(SCHEDULE_ID, SCHEDULE_NAME);
    let full_server = MockServer::start(vec![reply_json(schedule_collection(
        vec![full],
        week_template(),
        1,
    ))]);
    let api = make_client(&full_server, Duration::from_secs(1));
    let error = expect_error(
        schedule::create(
            &api,
            SITE,
            schedule::Patch {
                name: Some("Another".into()),
                ..Default::default()
            },
        )
        .await,
    );
    assert_eq!(error.kind, ErrorKind::Usage);
    let requests = full_server.finish();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "GET");

    let malformed = json!({"elements":[{"name":"missing id"}],"maxElements":16,"metaData":{"defaultSchedule":week_template()}});
    let malformed_server = MockServer::start(vec![reply_json(malformed)]);
    let api = make_client(&malformed_server, Duration::from_secs(1));
    let error = expect_error(
        schedule::create(
            &api,
            SITE,
            schedule::Patch {
                name: Some("New".into()),
                ..Default::default()
            },
        )
        .await,
    );
    assert_eq!(error.kind, ErrorKind::Unverified);
    let requests = malformed_server.finish();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "GET");

    let partial_template = json!({
        "elements":[],
        "maxElements":16,
        "metaData":{"defaultSchedule":{"id":"template","name":"Default","activeSchedule":"simple","schedule":week_template()["schedule"]}}
    });
    let partial_server = MockServer::start(vec![reply_json(partial_template)]);
    let api = make_client(&partial_server, Duration::from_secs(1));
    let error = expect_error(
        schedule::create(
            &api,
            SITE,
            schedule::Patch {
                name: Some("Weekly".into()),
                mode: Some(schedule::Mode::Week),
                week: BTreeMap::from([(schedule::Day::Monday, schedule::TimeRange::AllDay)]),
                ..Default::default()
            },
        )
        .await,
    );
    assert_eq!(error.kind, ErrorKind::Unverified);
    let requests = partial_server.finish();
    assert_eq!(
        requests.len(),
        1,
        "partial creation template must stop before POST"
    );

    let long_name_server = MockServer::start(vec![reply_json({
        let mut payload = schedule_collection(vec![], week_template(), 16);
        payload["metaData"]["maxLengthOfScheduleNames"] = json!(8);
        payload
    })]);
    let api = make_client(&long_name_server, Duration::from_secs(1));
    let error = expect_error(
        schedule::create(
            &api,
            SITE,
            schedule::Patch {
                name: Some("Longer than eight".into()),
                ..Default::default()
            },
        )
        .await,
    );
    assert_eq!(error.kind, ErrorKind::Usage);
    let requests = long_name_server.finish();
    assert_eq!(
        requests.len(),
        1,
        "metadata name limit must stop before POST"
    );

    let duplicate_server = MockServer::start(vec![reply_json(schedule_collection(
        vec![
            schedule_row(SCHEDULE_ID, SCHEDULE_NAME),
            schedule_row(SECOND_SCHEDULE_ID, "Occupied"),
        ],
        week_template(),
        16,
    ))]);
    let api = make_client(&duplicate_server, Duration::from_secs(1));
    let error = expect_error(
        schedule::update(
            &api,
            SITE,
            SCHEDULE_ID,
            schedule::Patch {
                name: Some("Occupied".into()),
                ..Default::default()
            },
        )
        .await,
    );
    assert_eq!(error.kind, ErrorKind::Usage);
    let requests = duplicate_server.finish();
    assert_eq!(requests.len(), 1, "duplicate name must block PUT");
}

#[tokio::test(start_paused = true)]
async fn schedule_delete_requires_complete_policy_references_and_verifies_absence() {
    let _clock = keep_clock_paused().await;
    let mut row = schedule_row(SCHEDULE_ID, SCHEDULE_NAME);
    row["referencingPolicies"] = json!([
        {"id":"policy-1","name":"Guest access window"},
        {"id":"policy-2","name":"IoT quiet hours"}
    ]);
    let server = MockServer::start(vec![
        reply_json(schedule_collection(vec![row.clone()], week_template(), 16)),
        reply_json(schedule_collection(vec![row.clone()], week_template(), 16)),
        Reply::empty(204),
        reply_json(schedule_collection(vec![], week_template(), 16)),
    ]);
    let api = make_client(&server, Duration::from_secs(1));
    let error = expect_error(schedule::delete(&api, SITE, SCHEDULE_ID, false).await);
    assert_eq!(error.kind, ErrorKind::Usage);
    assert!(error.message.contains("Guest access window"));
    assert!(error.message.contains("IoT quiet hours"));
    let (mutation, plan) = schedule::delete(&api, SITE, SCHEDULE_ID, true)
        .await
        .expect("confirmed deletion can be prepared");
    assert_eq!(
        mutation.target()["references"],
        json!(["Guest access window", "IoT quiet hours"])
    );
    let report = apply_readback_once(&mutation, &plan, Duration::from_secs(1))
        .await
        .expect("apply should return a report");
    assert_eq!(report.outcome, Outcome::Verified);
    let requests = server.finish();
    assert_eq!(
        requests.len(),
        4,
        "two previews, one DELETE, absence readback"
    );
    assert_request(
        &requests[2],
        "DELETE",
        &format!("/api/sites/{SITE}/schedules/{SCHEDULE_ID}"),
    );
    assert_request(&requests[3], "GET", &format!("/api/sites/{SITE}/schedules"));
}

#[tokio::test]
async fn schedule_delete_with_missing_reference_data_is_unverified_without_delete() {
    let mut row = schedule_row(SCHEDULE_ID, SCHEDULE_NAME);
    row.as_object_mut().unwrap().remove("referencingPolicies");
    let server = MockServer::start(vec![reply_json(schedule_collection(
        vec![row],
        week_template(),
        16,
    ))]);
    let api = make_client(&server, Duration::from_secs(1));
    let error = expect_error(schedule::delete(&api, SITE, SCHEDULE_ID, true).await);
    assert_eq!(error.kind, ErrorKind::Unverified);
    let requests = server.finish();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "GET");
}

#[tokio::test(start_paused = true)]
async fn foreign_ack_and_mismatched_readback_do_not_verify_or_retry_schedule_put() {
    let _clock = keep_clock_paused().await;
    let current = schedule_row(SCHEDULE_ID, SCHEDULE_NAME);
    let mut desired = current.clone();
    desired["name"] = json!("Night hours");
    let foreign_ack = MockServer::start(vec![
        reply_json(schedule_collection(
            vec![current.clone()],
            week_template(),
            16,
        )),
        reply_json(json!({"id":"different-resource"})),
        reply_json(schedule_collection(
            vec![desired.clone()],
            week_template(),
            16,
        )),
    ]);
    let api = make_client(&foreign_ack, Duration::from_secs(1));
    let (mutation, plan) = schedule::update(
        &api,
        SITE,
        SCHEDULE_ID,
        schedule::Patch {
            name: Some("Night hours".into()),
            ..Default::default()
        },
    )
    .await
    .expect("prepare rename");
    let report = apply_readback_once(&mutation, &plan, Duration::from_secs(1))
        .await
        .expect("apply should report the failed acknowledgment");
    assert_eq!(report.outcome, Outcome::RequestFailedStateMatches);
    assert_eq!(
        report.request_error.as_ref().unwrap().kind,
        ErrorKind::Unverified
    );
    let requests = foreign_ack.finish();
    assert!(requests.len() >= 3, "readback may be polled until deadline");
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.method == "PUT")
            .count(),
        1
    );

    let mismatch = MockServer::start(vec![
        reply_json(schedule_collection(
            vec![current.clone()],
            week_template(),
            16,
        )),
        Reply::empty(204),
        reply_json(schedule_collection(vec![current], week_template(), 16)),
    ]);
    let api = make_client(&mismatch, Duration::from_secs(1));
    let (mutation, plan) = schedule::update(
        &api,
        SITE,
        SCHEDULE_ID,
        schedule::Patch {
            name: Some("Night hours".into()),
            ..Default::default()
        },
    )
    .await
    .expect("prepare rename");
    let report = apply_readback_once(&mutation, &plan, Duration::from_secs(1))
        .await
        .expect("apply should report mismatched readback");
    assert_eq!(report.outcome, Outcome::Unverified);
    let requests = mismatch.finish();
    assert!(requests.len() >= 3, "readback may be polled until deadline");
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.method == "PUT")
            .count(),
        1
    );
}
