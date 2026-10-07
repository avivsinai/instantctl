use super::*;
use crate::api::{Method as ApiMethod, Request as ApiRequest};

#[tokio::test]
async fn raw_api_sends_exact_methods_paths_queries_bodies_and_headers() {
    let body = br#"{ "raw" : [1, true] }
"#
    .to_vec();
    let cases = [
        (
            ApiMethod::Get,
            "/sites/site-1/items?filter=active&limit=2",
            vec![
                ("sort".into(), "name & id".into()),
                ("tag".into(), "a+b".into()),
            ],
            None,
            "GET",
            "/api/sites/site-1/items?filter=active&limit=2&sort=name+%26+id&tag=a%2Bb",
        ),
        (
            ApiMethod::Post,
            "sites/site-1/items",
            vec![],
            Some(body.clone()),
            "POST",
            "/api/sites/site-1/items",
        ),
        (
            ApiMethod::Put,
            "/sites/site-1/items/a?revision=4",
            vec![("note key".into(), "one value".into())],
            Some(br#"{"name":"edge"}"#.to_vec()),
            "PUT",
            "/api/sites/site-1/items/a?revision=4&note+key=one+value",
        ),
        (
            ApiMethod::Delete,
            "/sites/site-1/items/a",
            vec![("hard".into(), "true".into())],
            Some(br#"{"confirm":"delete"}"#.to_vec()),
            "DELETE",
            "/api/sites/site-1/items/a?hard=true",
        ),
    ];
    let replies = (0..cases.len())
        .map(|index| Reply::json(200, format!(r#"{{"index":{index}}}"#).into_bytes()))
        .collect();
    let server = MockServer::start(replies);
    let client = make_client(&server, Duration::from_secs(2));

    for (index, (method, path, query, body, _, _)) in cases.iter().enumerate() {
        let request =
            ApiRequest::new(*method, path, query, body.clone()).expect("valid raw API request");
        let response = client
            .raw_api(&request)
            .await
            .expect("raw API request should return JSON");
        assert_eq!(response, json!({"index":index}));
    }

    let requests = server.finish();
    assert_eq!(requests.len(), cases.len());
    for (request, (_, _, _, expected_body, method, target)) in requests.iter().zip(cases.iter()) {
        assert_eq!(request.method, *method);
        assert_eq!(request.target, *target);
        assert_eq!(
            request.headers.get("authorization").map(String::as_str),
            Some("Bearer netcli-test-token-sentinel")
        );
        assert_eq!(
            request.headers.get("accept").map(String::as_str),
            Some("application/json")
        );
        assert_eq!(
            request.headers.get("x-ion-api-version").map(String::as_str),
            Some("28")
        );
        assert_eq!(
            request
                .headers
                .get("x-ion-client-platform")
                .map(String::as_str),
            Some("web")
        );
        assert_eq!(
            request.headers.get("x-ion-client-type").map(String::as_str),
            Some("InstantOn")
        );
        if let Some(expected_body) = expected_body {
            assert_eq!(request.body, *expected_body);
            assert_eq!(
                request.headers.get("content-type").map(String::as_str),
                Some("application/json")
            );
        } else {
            assert!(request.body.is_empty());
            assert!(!request.headers.contains_key("content-type"));
        }
    }
}

#[tokio::test]
async fn raw_api_returns_json_text_and_empty_success_responses() {
    let json_reply = br#"{"ready":true}"#.to_vec();
    let mut text_reply = Reply::empty(200);
    text_reply.headers = vec![("Content-Type".into(), "text/plain; charset=utf-8".into())];
    text_reply.body = b"accepted".to_vec();
    let server = MockServer::start(vec![
        Reply::json(200, json_reply.clone()),
        Reply::json(200, json_reply),
        text_reply,
        Reply::empty(204),
    ]);
    let client = make_client(&server, Duration::from_secs(2));

    let raw_get = ApiRequest::new(ApiMethod::Get, "/sites/site-1/health", &[], None).unwrap();
    let raw_value = client.raw_api(&raw_get).await.unwrap();
    let typed_value = client.get("/sites/site-1/health").await.unwrap();
    assert_eq!(raw_value, json!({"ready":true}));
    assert_eq!(raw_value, typed_value);

    let text_request = ApiRequest::new(ApiMethod::Post, "/sites/site-1/items", &[], None).unwrap();
    assert_eq!(
        client.raw_api(&text_request).await.unwrap(),
        Value::String("accepted".into())
    );

    let no_content =
        ApiRequest::new(ApiMethod::Delete, "/sites/site-1/items/a", &[], None).unwrap();
    assert_eq!(client.raw_api(&no_content).await.unwrap(), Value::Null);
    let requests = server.finish();
    assert_eq!(requests.len(), 4);
    assert_eq!(
        requests
            .iter()
            .map(|request| request.method.as_str())
            .collect::<Vec<_>>(),
        ["GET", "GET", "POST", "DELETE"]
    );
}

#[test]
fn raw_api_request_validation_rejects_external_paths_and_oversized_bodies() {
    for path in [
        "https://attacker.invalid/capture",
        "//attacker.invalid/capture",
        "/sites/%2e%2e/secrets",
    ] {
        let error = ApiRequest::new(ApiMethod::Get, path, &[], None)
            .expect_err("raw API must stay within the fixed origin");
        assert_eq!(error.kind, ErrorKind::Config);
    }

    let oversized = vec![b'x'; 4 * 1024 * 1024 + 1];
    let error = ApiRequest::new(ApiMethod::Post, "/sites/site-1/items", &[], Some(oversized))
        .expect_err("oversized request body must be rejected before HTTP");
    assert_eq!(error.kind, ErrorKind::Config);
    assert!(error.message.contains("size limit"));
}
