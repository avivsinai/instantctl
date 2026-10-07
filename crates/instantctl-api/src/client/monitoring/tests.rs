use super::*;
use crate::client::reads::tests::{Mock, Reply, SITE};
use serde_json::json;

#[tokio::test]
async fn every_monitoring_route_uses_authenticated_get_and_portal_wire_fields() {
    let base = format!("/api/sites/{SITE}");
    let responses = [
        (
            "events",
            json!({"elements":[{"id":"e1","event":"deviceChanged","occurrenceTime":1730000000,"source":{"id":"d1","name":"AP"}}]}),
        ),
        (
            "alerts",
            json!({"elements":[{"id":"a1","severity":"major","raisedTime":1730000001,"alertTypeProperties":{"network":{"id":"n1"}}}]}),
        ),
        (
            "health",
            json!({"currentHealth":{"healthScore":{"score":90}},"historicalHealths":[{"sampleTime":1729999900,"health":{}}],"samplePeriodSeconds":10}),
        ),
        (
            "dashboard",
            json!({"healthOverview":{"currentScore":90},"devicesOverview":{"up":2},"historyDurationSeconds":86400}),
        ),
        (
            "landingPage",
            json!({"siteName":"Home","activeAlertsCount":1,"deviceCount":2,"wiredClientsCount":0}),
        ),
        (
            "graphTopology",
            json!({"responseState":"ready","nodes":[{"id":"n1","device":{"name":"AP"}},{"id":"n2"}],"edges":[{"sourceNodeId":"n1","targetNodeId":"n2"}]}),
        ),
        (
            "applicationCategoryUsageConfiguration",
            json!({"isApplicationCategorizationEnabled":false}),
        ),
        (
            "applicationCategoryUsage",
            json!({"pendingAvailability":true,"elements":[{"applicationCategory":"video","networkId":"n1","downstreamDataTransferredDuringLast24HoursInBytes":42,"isBlocked":false}]}),
        ),
        (
            "stats/allNetworks/client/usage?appCategory=allAppCategories",
            json!({"elements":[{"clientId":"c1","dataTransferredDuringLast24HoursInBytes":7}]}),
        ),
        (
            "stats/network-1/client/usage?appCategory=video+%26+chat",
            json!({"elements":[{"clientId":"c2","clientCurrentlyActive":false,"applicationCategory":"video & chat"}]}),
        ),
        (
            "securityThreatEvents",
            json!({"exceptionConfiguredForRuleIds":[],"elements":[{"id":"t1","occurDate":1730000002,"classification":{"category":"malware"},"source":{"ipAddress":"192.0.2.1"},"signatureId":123}]}),
        ),
    ];
    let expected: Vec<_> = responses
        .iter()
        .map(|(route, _)| format!("{base}/{route}"))
        .collect();
    let server = Mock::new(
        responses
            .into_iter()
            .map(|(route, data)| (format!("{base}/{route}"), Reply::json(data))),
    );
    let api = server.client();
    let events = api.events(SITE).await.unwrap();
    assert_eq!(events[0].occurrence_time, Some(1730000000.0));
    assert_eq!(
        events[0].source.as_ref().unwrap().name.as_deref(),
        Some("AP")
    );
    assert!(events[0].state.is_none());
    let alerts = api.alerts(SITE).await.unwrap();
    assert_eq!(alerts[0].severity.as_deref(), Some("major"));
    assert!(alerts[0].cleared_time.is_none());
    assert_eq!(
        alerts[0].alert_type_properties.as_ref().unwrap()["network"]["id"],
        "n1"
    );
    let health = api.monitoring_health(SITE).await.unwrap();
    assert_eq!(
        health.current_health.as_ref().unwrap()["healthScore"]["score"],
        90
    );
    assert_eq!(health.sample_period_seconds, Some(10.0));
    assert!(health.history_duration_seconds.is_none());
    let dashboard = api.monitoring_dashboard(SITE).await.unwrap();
    assert_eq!(
        dashboard.health_overview.as_ref().unwrap()["currentScore"],
        90
    );
    assert!(dashboard.security_threats_overview.is_none());
    let landing = api.landing_page(SITE).await.unwrap();
    assert_eq!(landing.site_name.as_deref(), Some("Home"));
    assert_eq!(landing.wired_clients_count, Some(0));
    assert!(landing.wireless_clients_count.is_none());
    let graph = api.graph_topology(SITE).await.unwrap().unwrap();
    assert!(graph.nodes.as_ref().unwrap()[1].device.is_none());
    assert!(graph.edges.as_ref().unwrap()[0].loop_detected.is_none());
    assert_eq!(
        api.application_configuration(SITE)
            .await
            .unwrap()
            .is_application_categorization_enabled,
        Some(false)
    );
    let usage = api.application_usage(SITE).await.unwrap();
    assert_eq!(usage.pending_availability, Some(json!(true)));
    assert_eq!(
        usage.elements[0].downstream_data_transferred_during_last24_hours_in_bytes,
        Some(42)
    );
    assert!(
        usage.elements[0]
            .upstream_data_transferred_during_last24_hours_in_bytes
            .is_none()
    );
    let clients = api
        .client_usage(SITE, "allNetworks", "allAppCategories")
        .await
        .unwrap();
    assert!(clients[0].client_currently_active.is_none());
    assert_eq!(
        api.client_usage(SITE, "network-1", "video & chat")
            .await
            .unwrap()[0]
            .client_currently_active,
        Some(false)
    );
    let threats = api.security_threats(SITE).await.unwrap();
    assert_eq!(threats[0].signature_id, Some(json!(123)));
    assert!(threats[0].state.is_none());
    assert_eq!(server.paths(), expected);
}

#[tokio::test]
async fn collections_reject_missing_malformed_partial_and_duplicate_event_ids() {
    for data in [
        json!({}),
        json!({"elements":[null]}),
        json!({"elements":[{"occurrenceTime":123}]}),
        json!({"elements":[{"id":""}]}),
        json!({"elements":[{"id":"e1","occurrenceTime":"yesterday"}]}),
        json!({"elements":[{"id":"e1"},{"id":"e1"}]}),
        json!({"elements":[{"id":"e1"}],"totalCount":2}),
        json!({"elements":[],"metaData":{"nextPageToken":"more"}}),
    ] {
        let server = Mock::new([(format!("/api/sites/{SITE}/events"), Reply::json(data))]);
        assert_eq!(
            server.client().events(SITE).await.unwrap_err().kind,
            ErrorKind::Unverified
        );
    }
}

#[tokio::test]
async fn absent_or_null_telemetry_stays_null_and_empty_collections_are_valid() {
    let base = format!("/api/sites/{SITE}");
    let server = Mock::new([
        (
            format!("{base}/events"),
            Reply::json(json!({"elements":[]})),
        ),
        (
            format!("{base}/alerts"),
            Reply::json(json!({"elements":[]})),
        ),
        (
            format!("{base}/health"),
            Reply::json(json!({"currentHealth":null})),
        ),
        (format!("{base}/graphTopology"), Reply::json(Value::Null)),
        (
            format!("{base}/applicationCategoryUsage"),
            Reply::json(json!({"elements":[]})),
        ),
        (
            format!("{base}/applicationCategoryUsageConfiguration"),
            Reply::json(json!({})),
        ),
        (
            format!("{base}/securityThreatEvents"),
            Reply::json(json!({"elements":[]})),
        ),
    ]);
    let api = server.client();
    assert!(api.events(SITE).await.unwrap().is_empty());
    assert!(api.alerts(SITE).await.unwrap().is_empty());
    let health = serde_json::to_value(api.monitoring_health(SITE).await.unwrap()).unwrap();
    assert!(health["currentHealth"].is_null());
    assert!(health["historicalHealths"].is_null());
    assert!(api.graph_topology(SITE).await.unwrap().is_none());
    let usage = api.application_usage(SITE).await.unwrap();
    assert!(usage.pending_availability.is_none());
    assert!(usage.elements.is_empty());
    assert!(
        api.application_configuration(SITE)
            .await
            .unwrap()
            .is_application_categorization_enabled
            .is_none()
    );
    assert!(api.security_threats(SITE).await.unwrap().is_empty());
}

#[tokio::test]
async fn invalid_singletons_and_non_event_collections_fail_typed_parsing() {
    for (resource, data) in [
        ("health", json!([])),
        ("dashboard", json!("bad")),
        ("landingPage", json!({"deviceCount":"many"})),
        ("graphTopology", json!({"nodes":[{"id":"x"},{"id":"x"}]})),
        (
            "applicationCategoryUsageConfiguration",
            json!({"isApplicationCategorizationEnabled":"yes"}),
        ),
        (
            "applicationCategoryUsage",
            json!({"elements":[{"isBlocked":"no"}]}),
        ),
        (
            "stats/allNetworks/client/usage?appCategory=allAppCategories",
            json!({"elements":[{"clientCurrentlyActive":"unknown"}]}),
        ),
        ("alerts", json!({"elements":[{"id":"a"},{"id":"a"}]})),
        (
            "securityThreatEvents",
            json!({"elements":[{"id":"t","occurDate":{}}]}),
        ),
    ] {
        let server = Mock::new([(format!("/api/sites/{SITE}/{resource}"), Reply::json(data))]);
        let api = server.client();
        let kind = match resource {
            "health" => api.monitoring_health(SITE).await.unwrap_err().kind,
            "dashboard" => api.monitoring_dashboard(SITE).await.unwrap_err().kind,
            "landingPage" => api.landing_page(SITE).await.unwrap_err().kind,
            "graphTopology" => api.graph_topology(SITE).await.unwrap_err().kind,
            "applicationCategoryUsageConfiguration" => {
                api.application_configuration(SITE).await.unwrap_err().kind
            }
            "applicationCategoryUsage" => api.application_usage(SITE).await.unwrap_err().kind,
            "alerts" => api.alerts(SITE).await.unwrap_err().kind,
            "securityThreatEvents" => api.security_threats(SITE).await.unwrap_err().kind,
            _ => {
                api.client_usage(SITE, "allNetworks", "allAppCategories")
                    .await
                    .unwrap_err()
                    .kind
            }
        };
        assert_eq!(kind, ErrorKind::Unverified, "resource {resource}");
    }
}

#[tokio::test]
async fn invalid_site_and_path_selectors_are_rejected_before_any_get() {
    let server = Mock::new([] as [(&str, Reply); 0]);
    let api = server.client();
    assert_eq!(
        api.events("not-a-site").await.unwrap_err().kind,
        ErrorKind::Config
    );
    for network in ["", "..", "a/b", "a\\b"] {
        assert_eq!(
            api.client_usage(SITE, network, "allAppCategories")
                .await
                .unwrap_err()
                .kind,
            ErrorKind::Config
        );
    }
    for category in ["", "line\nfeed"] {
        assert_eq!(
            api.client_usage(SITE, "allNetworks", category)
                .await
                .unwrap_err()
                .kind,
            ErrorKind::Usage
        );
    }
    assert!(server.paths().is_empty());
}

#[tokio::test]
async fn follow_inputs_keep_transport_auth_and_transient_error_kinds() {
    for (status, kind) in [
        (401, ErrorKind::Auth),
        (403, ErrorKind::Auth),
        (408, ErrorKind::RetryLater),
        (500, ErrorKind::General),
    ] {
        let reply = Reply {
            status,
            ..Reply::json(Value::Null)
        };
        let server = Mock::new([(format!("/api/sites/{SITE}/events"), reply)]);
        assert_eq!(server.client().events(SITE).await.unwrap_err().kind, kind);
        assert_eq!(server.paths().len(), 1);
    }
}
