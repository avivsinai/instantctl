use super::*;

struct UnavailableToken;
impl TokenSource for UnavailableToken {
    async fn token(&self) -> Result<crate::secret::SecretString, Error> {
        Err(Error::new(ErrorKind::Auth, "saved token is unavailable"))
    }
}

#[tokio::test]
async fn country_uses_the_fixed_public_route_without_a_bearer_token() {
    let server = MockServer::start(vec![Reply::json(
        200,
        br#"{"countryCode":"IL","supportedCountryCodes":["IL","US"]}"#.to_vec(),
    )]);
    let api = Client::build(
        UnavailableToken,
        Duration::from_secs(1),
        server.base.clone(),
    )
    .unwrap();
    let country = api.country().await.unwrap();
    assert_eq!(country.country_code, "IL");
    assert_eq!(country.supported_country_codes.unwrap(), vec!["IL", "US"]);
    let requests = server.finish();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "GET");
    assert_eq!(requests[0].target, "/public/country");
    assert!(!requests[0].headers.contains_key("authorization"));
    assert!(requests[0].body.is_empty());
}

#[tokio::test]
async fn country_preserves_unknown_supported_list_and_rejects_malformed_codes() {
    for (value, succeeds) in [
        (json!({"countryCode":"IL"}), true),
        (
            json!({"countryCode":"IL","supportedCountryCodes":null}),
            true,
        ),
        (json!({"countryCode":null}), false),
        (json!({"countryCode":"il"}), false),
        (
            json!({"countryCode":"IL","supportedCountryCodes":["IL", "IL"]}),
            false,
        ),
        (
            json!({"countryCode":"IL","supportedCountryCodes":["IL", null]}),
            false,
        ),
        (
            json!({"countryCode":"IL","supportedCountryCodes":false}),
            false,
        ),
    ] {
        let server = MockServer::start(vec![Reply::json(200, serde_json::to_vec(&value).unwrap())]);
        let result = make_client(&server, Duration::from_secs(1)).country().await;
        if succeeds {
            assert!(result.unwrap().supported_country_codes.is_none());
        } else {
            assert_eq!(result.unwrap_err().kind, ErrorKind::Unverified);
        }
        assert_eq!(server.finish().len(), 1);
    }
}
