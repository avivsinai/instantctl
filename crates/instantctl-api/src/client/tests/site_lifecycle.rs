use super::*;
use crate::{
    ErrorKind,
    client::site_lifecycle::{NewSite, SiteMutation, State},
    mutation::{Mutation, Outcome, apply_once},
};
use serde_json::{Value, json};

const SITE_ID: &str = "123e4567-e89b-12d3-a456-426614174000";
const SOURCE_ID: &str = "123e4567-e89b-12d3-a456-426614174001";
const CREATED_ID: &str = "123e4567-e89b-12d3-a456-426614174002";
const NEW_NAME: &str = "Workshop West";
const COUNTRY: &str = "US";
const TIMEZONE: &str = "Europe/Berlin";

fn reply_json(value: Value) -> Reply {
    Reply::json(
        200,
        serde_json::to_vec(&value).expect("serialize lifecycle fixture"),
    )
}

fn sites(elements: Vec<Value>) -> Value {
    let count = elements.len() as u64;
    json!({"elements":elements,"totalCount":count,"matchingFilterCount":count})
}

fn account_site(id: &str, name: &str) -> Value {
    json!({"id":id,"name":name,"status":"active","futureAccountField":{"preserve":true}})
}

fn administration(id: &str, name: &str, country: &str) -> Value {
    json!({
        "id":id,
        "siteName":name,
        "regulatoryDomain":country,
        "futureAdministrationField":{"retain":[1,"opaque"]}
    })
}

fn country() -> Value {
    json!({"countryCode":"US","supportedCountryCodes":["US","CA"]})
}

fn body(request: &Request) -> Value {
    serde_json::from_slice(&request.body).expect("request body should be JSON")
}

fn assert_request(request: &Request, method: &str, target: &str) {
    assert_route(request, method, target);
    assert_eq!(
        request.headers.get("authorization").map(String::as_str),
        Some("Bearer netcli-test-token-sentinel")
    );
}

fn assert_route(request: &Request, method: &str, target: &str) {
    assert_eq!(request.method, method);
    assert_eq!(request.target, target);
}

fn requested_site() -> NewSite {
    NewSite {
        name: NEW_NAME.into(),
        country: COUNTRY.into(),
        timezone: TIMEZONE.into(),
    }
}

struct UnavailableToken {
    kind: ErrorKind,
    message: &'static str,
}

impl TokenSource for UnavailableToken {
    async fn token(&self) -> Result<crate::secret::SecretString, crate::Error> {
        Err(crate::Error::new(self.kind, self.message))
    }
}

#[tokio::test]
async fn create_resolves_credentials_before_the_public_country_request() {
    for (kind, message) in [
        (ErrorKind::Auth, "no saved login for this profile"),
        (
            ErrorKind::Config,
            "could not read credentials from macOS Keychain",
        ),
    ] {
        // Any accidental HTTP request gets a 500, so a public-route failure
        // cannot hide the original credential error.
        let server = MockServer::start(vec![Reply::empty(500)]);
        let source = UnavailableToken { kind, message };
        let client = Client::build(source, Duration::from_secs(1), server.base.clone())
            .expect("build isolated lifecycle client");
        let error = SiteMutation::create(&client, requested_site())
            .await
            .err()
            .expect("unavailable credentials must block site creation");
        let requests = server.finish();
        assert_eq!(
            error.kind,
            kind,
            "credential failure was replaced: {}",
            serde_json::to_string(&error).expect("serialize error")
        );
        assert_eq!(error.message, message);
        assert!(
            requests.is_empty(),
            "credentials must precede all HTTP requests"
        );
    }
}

#[tokio::test]
async fn create_and_clone_send_only_confirmed_fields_and_read_back_the_acknowledged_identity() {
    for cloning in [false, true] {
        let mut replies = Vec::new();
        if cloning {
            replies.push(reply_json(sites(vec![
                json!({"id":SOURCE_ID,"name":"Source"}),
            ])));
            replies.push(reply_json(account_site(SOURCE_ID, "Source")));
        }
        replies.push(reply_json(country()));
        replies.push(reply_json(if cloning {
            sites(vec![json!({"id":SOURCE_ID,"name":"Source"})])
        } else {
            sites(vec![])
        }));
        replies.push(reply_json(country()));
        if cloning {
            replies.push(reply_json(account_site(SOURCE_ID, "Source")));
        }
        replies.push(reply_json(json!({"id":CREATED_ID})));
        replies.push(reply_json(account_site(CREATED_ID, NEW_NAME)));
        replies.push(reply_json(administration(CREATED_ID, NEW_NAME, COUNTRY)));
        replies.push(reply_json(json!({"timezoneIana":TIMEZONE})));

        let server = MockServer::start(replies);
        let client = make_client(&server, Duration::from_secs(2));
        let (mutation, plan) = if cloning {
            SiteMutation::clone(&client, SOURCE_ID, requested_site())
                .await
                .expect("clone should prepare from an exact source site")
        } else {
            SiteMutation::create(&client, requested_site())
                .await
                .expect("create should prepare for an advertised country")
        };
        assert_eq!(plan.current, State::Absent);
        assert_eq!(
            plan.desired,
            State::Present {
                site_name: NEW_NAME.into(),
                country: Some(COUNTRY.into()),
                timezone: Some(TIMEZONE.into()),
            }
        );
        let report = apply_once(&mutation, &plan, Duration::from_secs(2))
            .await
            .expect("create or clone application should report");
        assert_eq!(report.outcome, Outcome::Verified);
        assert_eq!(report.observed, Some(plan.desired));

        let requests = server.finish();
        let mut index = 0;
        if cloning {
            assert_request(&requests[index], "GET", "/api/sites");
            index += 1;
            assert_request(&requests[index], "GET", &format!("/api/sites/{SOURCE_ID}"));
            index += 1;
        }
        assert_route(&requests[index], "GET", "/public/country");
        index += 1;
        assert_request(&requests[index], "GET", "/api/sites");
        index += 1;
        if cloning {
            assert_route(&requests[index], "GET", "/public/country");
            index += 1;
            assert_request(&requests[index], "GET", &format!("/api/sites/{SOURCE_ID}"));
            index += 1;
        } else {
            assert_route(&requests[index], "GET", "/public/country");
            index += 1;
        }
        let create_target = if cloning {
            format!("/api/sites/{SOURCE_ID}/siteCloning")
        } else {
            "/api/initialSetup".to_owned()
        };
        assert_request(&requests[index], "POST", &create_target);
        assert_eq!(
            body(&requests[index]),
            json!({"siteName":NEW_NAME,"regulatoryDomain":COUNTRY,"timezoneIana":TIMEZONE})
        );
        index += 1;
        assert_request(&requests[index], "GET", &format!("/api/sites/{CREATED_ID}"));
        index += 1;
        assert_request(
            &requests[index],
            "GET",
            &format!("/api/sites/{CREATED_ID}/administration"),
        );
        index += 1;
        assert_request(
            &requests[index],
            "GET",
            &format!("/api/sites/{CREATED_ID}/timezone"),
        );
        index += 1;
        assert_eq!(index, requests.len());
    }
}

#[tokio::test]
async fn rename_puts_the_fresh_full_object_and_preserves_unowned_fields() {
    let before = administration(SITE_ID, "Old Name", COUNTRY);
    let mut fresh = before.clone();
    fresh["futureAdministrationField"]["revision"] = json!(8);
    fresh["vendorExtension"] = json!({"newSincePlan":{"keep":true}});
    let mut expected_put = fresh.clone();
    expected_put["siteName"] = json!(NEW_NAME);

    let server = MockServer::start(vec![
        reply_json(sites(vec![json!({"id":SITE_ID,"name":"Old Name"})])),
        reply_json(account_site(SITE_ID, "Old Name")),
        reply_json(before),
        reply_json(account_site(SITE_ID, "Old Name")),
        reply_json(fresh),
        reply_json(json!({"id":SITE_ID})),
        reply_json(account_site(SITE_ID, NEW_NAME)),
        reply_json(expected_put.clone()),
    ]);
    let client = make_client(&server, Duration::from_secs(2));
    let (mutation, plan) = SiteMutation::rename(&client, SITE_ID, NEW_NAME)
        .await
        .expect("rename should resolve an exact account site");
    assert_eq!(
        plan.current,
        State::Present {
            site_name: "Old Name".into(),
            country: None,
            timezone: None,
        }
    );
    let report = apply_once(&mutation, &plan, Duration::from_secs(2))
        .await
        .expect("rename application should report");
    assert_eq!(report.outcome, Outcome::Verified);

    let requests = server.finish();
    assert_eq!(requests.len(), 8);
    assert_request(&requests[0], "GET", "/api/sites");
    assert_request(&requests[1], "GET", &format!("/api/sites/{SITE_ID}"));
    assert_request(
        &requests[2],
        "GET",
        &format!("/api/sites/{SITE_ID}/administration"),
    );
    assert_request(&requests[3], "GET", &format!("/api/sites/{SITE_ID}"));
    assert_request(
        &requests[4],
        "GET",
        &format!("/api/sites/{SITE_ID}/administration"),
    );
    assert_request(
        &requests[5],
        "PUT",
        &format!("/api/sites/{SITE_ID}/administration"),
    );
    assert_eq!(body(&requests[5]), expected_put);
    assert_request(&requests[6], "GET", &format!("/api/sites/{SITE_ID}"));
    assert_request(
        &requests[7],
        "GET",
        &format!("/api/sites/{SITE_ID}/administration"),
    );
}

#[tokio::test(start_paused = true)]
async fn delete_requires_the_fresh_exact_name_and_only_item_404_verifies_absence() {
    let _clock = keep_clock_paused().await;
    let initial = sites(vec![json!({"id":SITE_ID,"name":"Lab"})]);
    let absent_server = MockServer::start(vec![
        reply_json(initial.clone()),
        reply_json(account_site(SITE_ID, "Lab")),
        reply_json(account_site(SITE_ID, "Lab")),
        Reply::empty(204),
        Reply::json(404, br#"{"error":"missing"}"#.to_vec()),
    ]);
    let absent_client = make_client(&absent_server, Duration::from_secs(2));
    let (mutation, plan) = SiteMutation::delete(&absent_client, SITE_ID, Some("Lab"))
        .await
        .expect("exact fresh name should prepare deletion");
    let report = apply_readback_once(&mutation, &plan, Duration::from_millis(150))
        .await
        .expect("delete report");
    assert_eq!(report.outcome, Outcome::Verified);
    assert_eq!(report.observed, Some(State::Absent));
    let requests = absent_server.finish();
    assert_eq!(requests.len(), 5);
    assert_request(&requests[0], "GET", "/api/sites");
    assert_request(&requests[1], "GET", &format!("/api/sites/{SITE_ID}"));
    assert_request(&requests[2], "GET", &format!("/api/sites/{SITE_ID}"));
    assert_request(&requests[3], "DELETE", &format!("/api/sites/{SITE_ID}"));
    assert_request(&requests[4], "GET", &format!("/api/sites/{SITE_ID}"));

    let present_server = MockServer::start(vec![
        reply_json(initial.clone()),
        reply_json(account_site(SITE_ID, "Lab")),
        reply_json(account_site(SITE_ID, "Lab")),
        Reply::empty(204),
        reply_json(account_site(SITE_ID, "Lab")),
    ]);
    let present_client = make_client(&present_server, Duration::from_secs(2));
    let (mutation, plan) = SiteMutation::delete(&present_client, SITE_ID, Some("Lab"))
        .await
        .expect("exact fresh name should prepare deletion");
    let report = apply_readback_once(&mutation, &plan, Duration::from_millis(150))
        .await
        .expect("present-site readback should remain unverified");
    assert_eq!(report.outcome, Outcome::Unverified);
    assert_eq!(
        report.observed,
        Some(State::Present {
            site_name: "Lab".into(),
            country: None,
            timezone: None,
        })
    );
    let requests = present_server.finish();
    assert_eq!(requests.len(), 5);
    assert_request(&requests[3], "DELETE", &format!("/api/sites/{SITE_ID}"));
    assert_request(&requests[4], "GET", &format!("/api/sites/{SITE_ID}"));

    for (status, readback_requests) in [(401, 1), (403, 1), (503, 3)] {
        let mut replies = vec![
            reply_json(initial.clone()),
            reply_json(account_site(SITE_ID, "Lab")),
            reply_json(account_site(SITE_ID, "Lab")),
            Reply::empty(204),
        ];
        replies.extend((0..readback_requests).map(|_| {
            Reply::json(status, br#"{"error":"readback unavailable"}"#.to_vec())
                .header("Retry-After", "0")
        }));
        let server = MockServer::start(replies);
        let client = make_client(&server, Duration::from_secs(2));
        let (mutation, plan) = SiteMutation::delete(&client, SITE_ID, Some("Lab"))
            .await
            .expect("exact name should prepare deletion");
        let report = apply_readback_once(&mutation, &plan, Duration::from_millis(150))
            .await
            .expect("non-404 item reads remain unverified");
        assert_eq!(report.outcome, Outcome::Unverified, "status={status}");
        assert_eq!(report.error_kind(), Some(ErrorKind::Unverified));
        assert_eq!(report.readback_attempts, 1);
        let requests = server.finish();
        assert_eq!(requests.len(), 4 + readback_requests, "status={status}");
        assert_request(&requests[3], "DELETE", &format!("/api/sites/{SITE_ID}"));
        for request in &requests[4..] {
            assert_request(request, "GET", &format!("/api/sites/{SITE_ID}"));
        }
    }

    let denied_server = MockServer::start(vec![
        reply_json(initial.clone()),
        reply_json(account_site(SITE_ID, "Lab")),
        reply_json(account_site(SITE_ID, "Lab")),
        Reply::json(403, br#"{"error":"delete denied"}"#.to_vec()),
        reply_json(account_site(SITE_ID, "Lab")),
    ]);
    let denied_client = make_client(&denied_server, Duration::from_secs(2));
    let (mutation, plan) = SiteMutation::delete(&denied_client, SITE_ID, Some("Lab"))
        .await
        .expect("exact name should prepare deletion");
    let report = apply_readback_once(&mutation, &plan, Duration::from_millis(150))
        .await
        .expect("denied delete should preserve request failure");
    assert_eq!(report.outcome, Outcome::Failed);
    assert_eq!(
        report.request_error.as_ref().map(|error| error.kind),
        Some(ErrorKind::Auth)
    );
    let requests = denied_server.finish();
    assert_eq!(requests.len(), 5);
    assert_request(&requests[3], "DELETE", &format!("/api/sites/{SITE_ID}"));
    assert_request(&requests[4], "GET", &format!("/api/sites/{SITE_ID}"));

    let gate = Arc::new(ResponseGate::default());
    let timeout_server = MockServer::start(vec![
        reply_json(initial.clone()),
        reply_json(account_site(SITE_ID, "Lab")),
        reply_json(account_site(SITE_ID, "Lab")),
        Reply::empty(204),
        reply_json(account_site(SITE_ID, "Lab")).gated(&gate),
    ]);
    let application = tokio::spawn(async move {
        let timeout_client = make_client(&timeout_server, Duration::from_millis(40));
        let (mutation, plan) = SiteMutation::delete(&timeout_client, SITE_ID, Some("Lab"))
            .await
            .expect("exact name should prepare deletion");
        let application = apply_readback_once(&mutation, &plan, Duration::from_secs(1));
        tokio::pin!(application);
        tokio::select! {
            _ = gate.reached.notified() => {
                tokio::time::advance(Duration::from_millis(100)).await;
            }
            _ = &mut application => panic!("gated readback should wait for the client timeout"),
        }
        let report = application
            .await
            .expect("timed-out readback should return a report");
        gate.release();
        (report, timeout_server.finish())
    });
    let (report, requests) = application.await.expect("timeout task should finish");
    assert_eq!(report.outcome, Outcome::Unverified);
    assert_eq!(report.error_kind(), Some(ErrorKind::Unverified));
    assert_eq!(report.readback_attempts, 1);
    assert_eq!(requests.len(), 5);
    assert_request(&requests[3], "DELETE", &format!("/api/sites/{SITE_ID}"));
    assert_request(&requests[4], "GET", &format!("/api/sites/{SITE_ID}"));

    let changed_server = MockServer::start(vec![
        reply_json(initial),
        reply_json(account_site(SITE_ID, "Lab")),
        reply_json(account_site(SITE_ID, "Renamed")),
    ]);
    let changed_client = make_client(&changed_server, Duration::from_secs(2));
    let (mutation, plan) = SiteMutation::delete(&changed_client, SITE_ID, Some("Lab"))
        .await
        .expect("initial exact name should prepare delete guard");
    let error = mutation
        .write(&plan.desired)
        .await
        .expect_err("fresh changed name must stop deletion");
    assert_eq!(error.kind, ErrorKind::ConfirmationRequired);
    let requests = changed_server.finish();
    assert_eq!(requests.len(), 3);
    assert_request(&requests[2], "GET", &format!("/api/sites/{SITE_ID}"));
}

#[tokio::test(start_paused = true)]
async fn lifecycle_guards_reject_invalid_or_ambiguous_inputs_before_writes() {
    let _clock = keep_clock_paused().await;
    let server = MockServer::start(vec![reply_json(sites(vec![
        json!({"id":SITE_ID,"name":"Duplicate"}),
        json!({"id":SOURCE_ID,"name":"Duplicate"}),
    ]))]);
    let client = make_client(&server, Duration::from_secs(1));
    let error = SiteMutation::rename(&client, "Duplicate", NEW_NAME)
        .await
        .err()
        .expect("duplicate exact site names must be ambiguous");
    assert_eq!(error.kind, ErrorKind::Usage);
    assert_eq!(server.finish().len(), 1);

    let server = MockServer::start(vec![reply_json(sites(vec![json!({
        "id":"not-a-uuid","name":"Broken"
    })]))]);
    let client = make_client(&server, Duration::from_secs(1));
    let error = SiteMutation::rename(&client, "Broken", NEW_NAME)
        .await
        .err()
        .expect("invalid account identity must block mutation");
    assert_eq!(error.kind, ErrorKind::Unverified);
    assert_eq!(server.finish().len(), 1);

    let server = MockServer::start(vec![
        reply_json(sites(vec![json!({"id":SITE_ID,"name":"Old Name"})])),
        reply_json(account_site(SITE_ID, "Old Name")),
        reply_json(administration(SITE_ID, "Different name", COUNTRY)),
    ]);
    let client = make_client(&server, Duration::from_secs(1));
    let error = SiteMutation::rename(&client, SITE_ID, NEW_NAME)
        .await
        .err()
        .expect("account and administration names must agree before rename");
    assert_eq!(error.kind, ErrorKind::Unverified);
    let requests = server.finish();
    assert_eq!(requests.len(), 3);
    assert_request(&requests[1], "GET", &format!("/api/sites/{SITE_ID}"));
    assert_request(
        &requests[2],
        "GET",
        &format!("/api/sites/{SITE_ID}/administration"),
    );

    let server = MockServer::start(vec![reply_json(json!({
        "countryCode":"US","supportedCountryCodes":["CA"]
    }))]);
    let client = make_client(&server, Duration::from_secs(1));
    let error = SiteMutation::create(&client, requested_site())
        .await
        .err()
        .expect("unadvertised country must block site creation");
    assert_eq!(error.kind, ErrorKind::Unsupported);
    let requests = server.finish();
    assert_eq!(requests.len(), 1);
    assert_route(&requests[0], "GET", "/public/country");

    let server = MockServer::start(vec![]);
    let client = make_client(&server, Duration::from_secs(1));
    let invalid = NewSite {
        name: NEW_NAME.into(),
        country: COUNTRY.into(),
        timezone: "".into(),
    };
    let error = SiteMutation::create(&client, invalid)
        .await
        .err()
        .expect("invalid timezone must fail before portal access");
    assert_eq!(error.kind, ErrorKind::Usage);
    assert!(server.finish().is_empty());

    let server = MockServer::start(vec![
        reply_json(country()),
        reply_json(sites(vec![])),
        reply_json(country()),
        reply_json(json!({"message":"created without an ID"})),
    ]);
    let client = make_client(&server, Duration::from_secs(1));
    let (mutation, plan) = SiteMutation::create(&client, requested_site())
        .await
        .expect("creation can be prepared before the acknowledgment arrives");
    let report = apply_readback_once(&mutation, &plan, Duration::from_millis(100))
        .await
        .expect("missing ID acknowledgment must become unverified");
    assert_eq!(report.outcome, Outcome::Unverified);
    assert_eq!(report.readback_attempts, 1);
    let requests = server.finish();
    assert_eq!(requests.len(), 4);
    assert_request(&requests[3], "POST", "/api/initialSetup");

    for account in [
        json!({"name":NEW_NAME}),
        json!({"id":SITE_ID,"name":NEW_NAME}),
    ] {
        let server = MockServer::start(vec![
            reply_json(country()),
            reply_json(sites(vec![])),
            reply_json(country()),
            reply_json(json!({"id":CREATED_ID})),
            reply_json(account),
        ]);
        let client = make_client(&server, Duration::from_secs(1));
        let (mutation, plan) = SiteMutation::create(&client, requested_site())
            .await
            .expect("create should prepare before direct identity readback");
        let report = apply_readback_once(&mutation, &plan, Duration::from_millis(100))
            .await
            .expect("missing or mismatched account ID must stay unverified");
        assert_eq!(report.outcome, Outcome::Unverified);
        assert_eq!(report.readback_attempts, 1);
        let requests = server.finish();
        assert_eq!(requests.len(), 5);
        assert_request(&requests[4], "GET", &format!("/api/sites/{CREATED_ID}"));
    }

    let server = MockServer::start(vec![
        reply_json(country()),
        reply_json(sites(vec![])),
        reply_json(country()),
        reply_json(json!({"id":CREATED_ID})),
        reply_json(account_site(CREATED_ID, "Different name")),
        reply_json(administration(CREATED_ID, NEW_NAME, COUNTRY)),
        reply_json(json!({"timezoneIana":TIMEZONE})),
    ]);
    let client = make_client(&server, Duration::from_secs(1));
    let (mutation, plan) = SiteMutation::create(&client, requested_site())
        .await
        .expect("create should prepare before cross-resource readback");
    let report = apply_readback_once(&mutation, &plan, Duration::from_millis(100))
        .await
        .expect("account/admin name disagreement must stay unverified");
    assert_eq!(report.outcome, Outcome::Unverified);
    let requests = server.finish();
    assert_eq!(requests.len(), 6);
    assert_request(&requests[4], "GET", &format!("/api/sites/{CREATED_ID}"));
    assert_request(
        &requests[5],
        "GET",
        &format!("/api/sites/{CREATED_ID}/administration"),
    );
}
