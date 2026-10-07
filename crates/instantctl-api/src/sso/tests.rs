use super::*;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use std::{
    collections::HashMap,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        mpsc::{self, Receiver},
    },
    thread::{self, JoinHandle},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::oneshot;
use url::Url;

const USERNAME: &str = "alice@example.test";
const PASSWORD: &str = "password-sentinel";
const OTP: &str = "otp-sentinel";
const SESSION: &str = "session-sentinel";
const REFRESH: &str = "refresh-sentinel";
const CLIENT_ID: &str = "runtime-client-id";
type SettingsVariant = fn(&str) -> String;

#[derive(Clone)]
struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    delay: Duration,
    chunked: bool,
}

impl Reply {
    fn json(status: u16, body: impl Into<Vec<u8>>) -> Self {
        Self {
            status,
            headers: vec![("Content-Type".into(), "application/json".into())],
            body: body.into(),
            delay: Duration::ZERO,
            chunked: false,
        }
    }

    fn found(location: String) -> Self {
        Self {
            status: 302,
            headers: vec![("Location".into(), location)],
            body: vec![],
            delay: Duration::ZERO,
            chunked: false,
        }
    }

    fn chunked(mut self) -> Self {
        self.chunked = true;
        self
    }
}

#[derive(Clone, Debug)]
struct Request {
    method: String,
    target: String,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

struct MockServer {
    base: Url,
    requests: Receiver<Request>,
    stop: mpsc::Sender<()>,
    worker: Option<JoinHandle<()>>,
}

struct ResponseGate {
    reached: Option<oneshot::Receiver<Request>>,
    release: Option<mpsc::Sender<()>>,
}

impl ResponseGate {
    async fn wait_until_reached(&mut self) -> Request {
        self.reached
            .take()
            .expect("response gate can be awaited once")
            .await
            .expect("mock server should report the gated request")
    }

    fn release(&mut self) {
        if let Some(release) = self.release.take() {
            let _ = release.send(());
        }
    }
}

impl Drop for ResponseGate {
    fn drop(&mut self) {
        self.release();
    }
}

impl MockServer {
    fn start(handler: impl Fn(&Request) -> Reply + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback SSO server");
        listener
            .set_nonblocking(true)
            .expect("set listener nonblocking");
        let addr = listener.local_addr().expect("read loopback address");
        let base = Url::parse(&format!("http://{addr}")).expect("parse loopback origin");
        let (sender, requests) = mpsc::channel();
        let (stop, stopped) = mpsc::channel();
        let handler = Arc::new(handler);
        let worker = thread::spawn(move || {
            let deadline = std::time::Instant::now() + Duration::from_secs(30);
            let mut handled = 0;
            while handled < 32 && std::time::Instant::now() < deadline {
                match stopped.try_recv() {
                    Ok(()) | Err(mpsc::TryRecvError::Disconnected) => break,
                    Err(mpsc::TryRecvError::Empty) => {}
                }
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        // Darwin may preserve the listener's nonblocking flag.
                        stream
                            .set_nonblocking(false)
                            .expect("set accepted socket blocking");
                        let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
                        let _ = stream.set_write_timeout(Some(Duration::from_millis(500)));
                        if let Some(request) = read_request(&mut stream) {
                            let _ = sender.send(request.clone());
                            let reply = handler(&request);
                            if !reply.delay.is_zero() {
                                thread::sleep(reply.delay);
                            }
                            write_reply(&mut stream, &reply);
                            handled += 1;
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5))
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            base,
            requests,
            stop,
            worker: Some(worker),
        }
    }

    fn finish(mut self) -> Vec<Request> {
        let _ = self.stop.send(());
        if let Some(worker) = self.worker.take() {
            worker.join().expect("mock server should not panic");
        }
        self.requests.try_iter().collect()
    }
}

impl Drop for MockServer {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn read_request(stream: &mut TcpStream) -> Option<Request> {
    let mut bytes = Vec::new();
    let mut buf = [0u8; 4096];
    let (end, length) = loop {
        let count = stream.read(&mut buf).ok()?;
        if count == 0 {
            return None;
        }
        bytes.extend_from_slice(&buf[..count]);
        if bytes.len() > 1024 * 1024 {
            return None;
        }
        if let Some(index) = bytes.windows(4).position(|x| x == b"\r\n\r\n") {
            let end = index + 4;
            let text = std::str::from_utf8(&bytes[..end]).ok()?;
            let length = text
                .lines()
                .skip(1)
                .filter_map(|line| line.split_once(':'))
                .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                .and_then(|(_, value)| value.trim().parse().ok())
                .unwrap_or(0);
            break (end, length);
        }
    };
    while bytes.len() < end + length {
        let count = stream.read(&mut buf).ok()?;
        if count == 0 {
            return None;
        }
        bytes.extend_from_slice(&buf[..count]);
    }
    let text = std::str::from_utf8(&bytes[..end]).ok()?;
    let mut lines = text.lines();
    let mut request_line = lines.next()?.split_whitespace();
    Some(Request {
        method: request_line.next()?.to_owned(),
        target: request_line.next()?.to_owned(),
        headers: lines
            .filter_map(|line| line.split_once(':'))
            .map(|(k, v)| (k.to_ascii_lowercase(), v.trim().to_owned()))
            .collect(),
        body: bytes[end..end + length].to_vec(),
    })
}

fn write_reply(stream: &mut TcpStream, reply: &Reply) {
    let reason = match reply.status {
        200 => "OK",
        204 => "No Content",
        302 => "Found",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "Mock",
    };
    let mut head = format!(
        "HTTP/1.1 {} {}\r\nConnection: close\r\n",
        reply.status, reason
    );
    if reply.chunked {
        head.push_str("Transfer-Encoding: chunked\r\n");
    } else {
        head.push_str(&format!("Content-Length: {}\r\n", reply.body.len()));
    }
    for (name, value) in &reply.headers {
        head.push_str(name);
        head.push_str(": ");
        head.push_str(value);
        head.push_str("\r\n");
    }
    head.push_str("\r\n");
    if stream.write_all(head.as_bytes()).is_err() {
        return;
    }
    if reply.chunked {
        for chunk in reply.body.chunks(16 * 1024) {
            if write!(stream, "{:x}\r\n", chunk.len()).is_err()
                || stream.write_all(chunk).is_err()
                || stream.write_all(b"\r\n").is_err()
            {
                return;
            }
        }
        let _ = stream.write_all(b"0\r\n\r\n");
    } else {
        let _ = stream.write_all(&reply.body);
    }
    let _ = stream.flush();
}

fn client(server: &MockServer, timeout: Duration) -> SsoClient {
    let mut settings_url = server.base.clone();
    settings_url.set_path("/settings.json");
    SsoClient::build(
        timeout,
        settings_url,
        server.base.clone(),
        server.base.clone(),
    )
    .expect("build loopback SSO client")
}

#[test]
fn production_constructor_has_fixed_origins_and_zero_timeout_is_rejected() {
    let api = SsoClient::new(Duration::from_secs(1)).expect("fixed production client builds");
    assert_eq!(
        api.settings_url.as_str(),
        "https://portal.instant-on.hpe.com/settings.json"
    );
    assert_eq!(api.sso_origin.as_str(), "https://sso.arubainstanton.com/");
    assert_eq!(
        api.portal_origin.as_str(),
        "https://portal.instant-on.hpe.com/"
    );
    assert_eq!(
        SsoClient::new(Duration::ZERO).unwrap_err(),
        SsoError::Config("SSO timeout must be greater than zero")
    );
}

#[test]
fn token_client_and_converted_errors_redact_all_secret_sentinels() {
    let api = SsoClient::new(Duration::from_secs(1)).unwrap();
    let tokens = Tokens {
        access: SecretString::new("access-token-sentinel"),
        refresh: SecretString::new(REFRESH),
        access_expiry: UNIX_EPOCH,
    };
    let error: crate::Error = SsoError::InvalidCredentials.into();
    let rendered = [
        format!("{api:?}"),
        format!("{tokens:?}"),
        error.to_string(),
        format!("{error:?}"),
    ]
    .join("\n");
    for sentinel in [
        PASSWORD,
        OTP,
        SESSION,
        REFRESH,
        "access-token-sentinel",
        CLIENT_ID,
    ] {
        assert!(!rendered.contains(sentinel), "secret leaked: {sentinel}");
    }
}

fn oauth_success() -> Reply {
    Reply::json(200, serde_json::json!({"access_token":jwt("access-token-sentinel", future_exp()),"refresh_token":REFRESH}).to_string())
}

fn future_exp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 3600
}

fn jwt(label: &str, exp: u64) -> String {
    let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"none"}"#);
    let payload = URL_SAFE_NO_PAD.encode(serde_json::json!({"sub":label,"exp":exp}).to_string());
    format!("{header}.{payload}.signature")
}

fn form(request: &Request) -> HashMap<String, String> {
    url::form_urlencoded::parse(&request.body)
        .into_owned()
        .collect()
}

fn assert_form_content_type(request: &Request) {
    assert_eq!(
        request.headers.get("content-type").map(String::as_str),
        Some("application/x-www-form-urlencoded")
    );
}

fn server_origin(request: &Request) -> String {
    format!(
        "http://{}",
        request.headers.get("host").expect("Host header")
    )
}

fn auth_redirect(request: &Request, path: &str) -> Reply {
    let authorize = Url::parse(&format!("http://localhost{}", request.target))
        .expect("parse authorization URL");
    let state = authorize
        .query_pairs()
        .find(|(key, _)| key == "state")
        .unwrap()
        .1
        .into_owned();
    let host = request.headers.get("host").expect("Host header");
    let location = format!(
        "http://{host}{path}?state={}&code=authorization-code",
        url::form_urlencoded::byte_serialize(state.as_bytes()).collect::<String>()
    );
    Reply::found(location)
}

fn standard_server(handler: impl Fn(&Request) -> Reply + Send + Sync + 'static) -> MockServer {
    configured_server(
        |origin| {
            serde_json::json!({"ssoFqdn":origin,"ssoClientIdAuthZ":CLIENT_ID,"ssoRedirectUrl":origin}).to_string()
        },
        handler,
    )
}

fn gated_response(
    reply: Reply,
) -> (
    impl Fn(&Request) -> Reply + Send + Sync + 'static,
    ResponseGate,
) {
    let (reached_tx, reached_rx) = oneshot::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let gate = Arc::new(Mutex::new(Some((reached_tx, release_rx))));
    let handler = move |request: &Request| {
        let (reached, release) = gate
            .lock()
            .expect("response gate mutex")
            .take()
            .expect("response gate handles one request");
        let _ = reached.send(request.clone());
        let _ = release.recv();
        reply.clone()
    };
    (
        handler,
        ResponseGate {
            reached: Some(reached_rx),
            release: Some(release_tx),
        },
    )
}

fn configured_server(
    settings_json: impl Fn(&str) -> String + Send + Sync + 'static,
    handler: impl Fn(&Request) -> Reply + Send + Sync + 'static,
) -> MockServer {
    // The handler needs its own origin after bind, so use localhost URLs in the
    // response and translate them to this listener's dynamic authority here.
    let settings_body = Arc::new(Mutex::new(None::<String>));
    let holder = settings_body.clone();
    let settings_json = Arc::new(settings_json);
    let wrapped = move |request: &Request| {
        if let Some(body) = holder.lock().unwrap().clone() {
            let target = Url::parse(&format!("http://localhost{}", request.target)).unwrap();
            if target.path() == "/settings.json" {
                return Reply::json(200, body);
            }
        }
        handler(request)
    };
    let server = MockServer::start(wrapped);
    let origin = server.base.origin().ascii_serialization();
    *settings_body.lock().unwrap() = Some(settings_json(&origin));
    server
}

#[tokio::test]
async fn login_sends_credentials_runtime_client_id_and_validates_pkce_exchange() {
    let challenge_seen = Arc::new(Mutex::new(None::<String>));
    let challenge_slot = challenge_seen.clone();
    let expected_exp = future_exp();
    let server = standard_server(move |request| {
        let target = Url::parse(&format!("http://localhost{}", request.target)).unwrap();
        match target.path() {
            "/aio/api/v1/mfa/validate/full" => {
                Reply::json(200, serde_json::json!({"access_token":SESSION}).to_string())
            }
            "/as/authorization.oauth2" => {
                let pairs: HashMap<_, _> = target.query_pairs().into_owned().collect();
                assert_eq!(pairs.get("client_id").map(String::as_str), Some(CLIENT_ID));
                assert_eq!(pairs.get("sessionToken").map(String::as_str), Some(SESSION));
                assert_eq!(
                    pairs.get("redirect_uri").map(String::as_str),
                    Some(server_origin(request).as_str())
                );
                assert_eq!(
                    pairs.get("code_challenge_method").map(String::as_str),
                    Some("S256")
                );
                *challenge_slot.lock().unwrap() = pairs.get("code_challenge").cloned();
                auth_redirect(request, "/")
            }
            "/as/token.oauth2" => {
                let posted = form(request);
                let verifier = posted.get("code_verifier").expect("PKCE verifier sent");
                let expected = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
                assert_eq!(
                    challenge_slot.lock().unwrap().as_deref(),
                    Some(expected.as_str()),
                    "server validates verifier against authorize challenge"
                );
                assert_eq!(posted.get("client_id").map(String::as_str), Some(CLIENT_ID));
                assert_eq!(
                    posted.get("grant_type").map(String::as_str),
                    Some("authorization_code")
                );
                assert_eq!(
                    posted.get("code").map(String::as_str),
                    Some("authorization-code")
                );
                assert_eq!(
                    posted.get("redirect_uri").map(String::as_str),
                    Some(server_origin(request).as_str())
                );
                Reply::json(200, serde_json::json!({"access_token":jwt("access-token-sentinel", expected_exp),"refresh_token":REFRESH}).to_string())
            }
            _ => Reply::json(404, b"{}".to_vec()),
        }
    });
    let mut settings_url = server.base.clone();
    settings_url.set_path("/settings.json");
    let api = SsoClient::build(
        Duration::from_secs(2),
        settings_url,
        server.base.clone(),
        server.base.clone(),
    )
    .unwrap();
    let tokens = api
        .login(USERNAME, &SecretString::new(PASSWORD), None)
        .await
        .expect("login should succeed");
    assert_eq!(tokens.refresh.expose_secret(), REFRESH);
    assert_eq!(
        tokens.access_expiry,
        UNIX_EPOCH + Duration::from_secs(expected_exp)
    );
    let requests = server.finish();
    assert_eq!(requests.len(), 4);
    assert_eq!(
        requests
            .iter()
            .map(|request| request.method.as_str())
            .collect::<Vec<_>>(),
        ["GET", "POST", "GET", "POST"]
    );
    let credentials = form(&requests[1]);
    assert_form_content_type(&requests[1]);
    assert_eq!(
        credentials.get("username").map(String::as_str),
        Some(USERNAME)
    );
    assert_eq!(
        credentials.get("password").map(String::as_str),
        Some(PASSWORD)
    );
    assert_form_content_type(&requests[3]);
}

#[tokio::test]
async fn otp_required_can_be_retried_and_otp_is_sent_as_a_form_field() {
    let attempts = Arc::new(Mutex::new(0));
    let seen = attempts.clone();
    let server = standard_server(move |request| {
        let path = Url::parse(&format!("http://localhost{}", request.target))
            .unwrap()
            .path()
            .to_owned();
        match path.as_str() {
            "/aio/api/v1/mfa/validate/full" => {
                let n = {
                    let mut n = seen.lock().unwrap();
                    *n += 1;
                    *n
                };
                if n == 1 {
                    Reply::json(
                        400,
                        br#"{"error":"invalid_grant","error_description":"OTP required"}"#.to_vec(),
                    )
                } else {
                    assert_eq!(form(request).get("otp").map(String::as_str), Some(OTP));
                    Reply::json(200, serde_json::json!({"access_token":SESSION}).to_string())
                }
            }
            "/as/authorization.oauth2" => auth_redirect(request, "/"),
            "/as/token.oauth2" => oauth_success(),
            _ => Reply::json(404, b"{}".to_vec()),
        }
    });
    let api = client(&server, Duration::from_secs(2));
    assert_eq!(
        api.login(USERNAME, &SecretString::new(PASSWORD), None)
            .await
            .unwrap_err(),
        SsoError::OtpRequired
    );
    api.login(
        USERNAME,
        &SecretString::new(PASSWORD),
        Some(&SecretString::new(OTP)),
    )
    .await
    .expect("OTP retry succeeds");
    let requests = server.finish();
    assert_eq!(
        requests
            .iter()
            .filter(|r| r.target.starts_with("/aio/api/"))
            .count(),
        2
    );
    assert_form_content_type(&requests[1]);
    assert_form_content_type(&requests[2]);
}

#[tokio::test]
async fn wrong_otp_provider_error_is_typed_before_authorization() {
    let server = standard_server(|request| {
        if request.target.starts_with("/aio/api/") {
            Reply::json(
                400,
                br#"{"error":"invalid_grant","error_description":"OTP code is invalid"}"#.to_vec(),
            )
        } else {
            Reply::json(404, b"{}".to_vec())
        }
    });
    let error = client(&server, Duration::from_secs(1))
        .login(
            USERNAME,
            &SecretString::new(PASSWORD),
            Some(&SecretString::new(OTP)),
        )
        .await
        .unwrap_err();
    assert_eq!(error, SsoError::OtpRequired);
    let requests = server.finish();
    assert_eq!(requests.len(), 2);
    assert_form_content_type(&requests[1]);
    assert_eq!(form(&requests[1]).get("otp").map(String::as_str), Some(OTP));
    assert!(
        !requests
            .iter()
            .any(|request| request.target.starts_with("/as/authorization.oauth2"))
    );
}

#[tokio::test]
async fn provider_login_errors_are_typed_and_never_echo_provider_secrets() {
    for (body, expected) in [
        (
            br#"{"error":"invalid_grant","error_description":"wrong password password-sentinel"}"#
                .as_slice(),
            SsoError::InvalidCredentials,
        ),
        (
            br#"{"error":"access_denied","error_description":"account locked password-sentinel"}"#
                .as_slice(),
            SsoError::AccountLocked,
        ),
        (
            br#"{"error":"access_denied","error_description":"user disabled password-sentinel"}"#
                .as_slice(),
            SsoError::AccountLocked,
        ),
    ] {
        let body = body.to_vec();
        let server = standard_server(move |request| {
            if request.target.starts_with("/aio/api/") {
                Reply::json(401, body.clone())
            } else {
                Reply::json(404, b"{}".to_vec())
            }
        });
        let error = client(&server, Duration::from_secs(1))
            .login(USERNAME, &SecretString::new(PASSWORD), None)
            .await
            .unwrap_err();
        assert_eq!(error, expected);
        assert!(!error.to_string().contains(PASSWORD));
        assert!(!format!("{error:?}").contains(PASSWORD));
        assert!(!error.to_string().contains("wrong password"));
        assert_eq!(
            server.finish().len(),
            2,
            "settings and credential post only"
        );
    }
}

#[tokio::test]
async fn state_mismatch_and_duplicate_callback_parameters_stop_before_token_post() {
    for case in ["mismatch", "duplicate-state", "duplicate-code"] {
        let case = case.to_owned();
        let redirect_case = case.clone();
        let server = standard_server(move |request| {
            let url = Url::parse(&format!("http://localhost{}", request.target)).unwrap();
            let path = url.path().to_owned();
            match path.as_str() {
                "/aio/api/v1/mfa/validate/full" => {
                    Reply::json(200, serde_json::json!({"access_token":SESSION}).to_string())
                }
                "/as/authorization.oauth2" => {
                    let echoed_state = url
                        .query_pairs()
                        .find(|(name, _)| name == "state")
                        .expect("authorization state")
                        .1
                        .into_owned();
                    let query = match redirect_case.as_str() {
                        "mismatch" => "state=wrong&code=x".to_owned(),
                        "duplicate-state" => format!("state={echoed_state}&state=second&code=x"),
                        "duplicate-code" => format!("state={echoed_state}&code=x&code=y"),
                        _ => unreachable!(),
                    };
                    let location =
                        format!("http://{}?{query}", request.headers.get("host").unwrap());
                    Reply::found(location)
                }
                _ => Reply::json(404, b"{}".to_vec()),
            }
        });
        let error = client(&server, Duration::from_secs(1))
            .login(USERNAME, &SecretString::new(PASSWORD), None)
            .await
            .unwrap_err();
        match case.as_str() {
            "mismatch" => assert_eq!(
                error,
                SsoError::Config("SSO authorization state did not match")
            ),
            "duplicate-state" | "duplicate-code" => assert_eq!(
                error,
                SsoError::Config("SSO authorization returned invalid code or state")
            ),
            _ => unreachable!(),
        }
        let requests = server.finish();
        assert_eq!(requests.len(), 3);
        assert!(
            !requests
                .iter()
                .any(|request| request.target.starts_with("/as/token.oauth2"))
        );
    }
}

#[tokio::test]
async fn foreign_host_authorization_redirect_is_rejected_without_following_it() {
    let spy = TcpListener::bind("127.0.0.1:0").unwrap();
    spy.set_nonblocking(true).unwrap();
    let spy_addr = spy.local_addr().unwrap();
    let server = standard_server(move |request| {
        let path = Url::parse(&format!("http://localhost{}", request.target))
            .unwrap()
            .path()
            .to_owned();
        match path.as_str() {
            "/aio/api/v1/mfa/validate/full" => {
                Reply::json(200, serde_json::json!({"access_token":SESSION}).to_string())
            }
            "/as/authorization.oauth2" => {
                Reply::found(format!("http://{spy_addr}/callback?state=bad&code=secret"))
            }
            _ => Reply::json(404, b"{}".to_vec()),
        }
    });
    let error = client(&server, Duration::from_secs(1))
        .login(USERNAME, &SecretString::new(PASSWORD), None)
        .await
        .unwrap_err();
    assert!(matches!(error, SsoError::Config(_)));
    assert!(
        matches!(spy.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
        "foreign callback listener received no request"
    );
    let requests = server.finish();
    assert_eq!(requests.len(), 3);
    assert!(
        !requests
            .iter()
            .any(|request| request.target.starts_with("/as/token.oauth2"))
    );
}

#[tokio::test]
async fn foreign_redirects_from_settings_credential_token_and_revoke_are_never_followed() {
    for endpoint in ["settings", "credential", "token", "revoke"] {
        let spy = TcpListener::bind("127.0.0.1:0").expect("bind foreign redirect spy");
        spy.set_nonblocking(true).unwrap();
        let destination = format!("http://{}/capture", spy.local_addr().unwrap());
        let server = if endpoint == "settings" {
            MockServer::start({
                let destination = destination.clone();
                move |_| Reply::found(destination.clone())
            })
        } else {
            standard_server({
                let endpoint = endpoint.to_owned();
                let destination = destination.clone();
                move |request| {
                    let path = Url::parse(&format!("http://localhost{}", request.target))
                        .unwrap()
                        .path()
                        .to_owned();
                    let should_redirect = match endpoint.as_str() {
                        "credential" => path == "/aio/api/v1/mfa/validate/full",
                        "token" => path == "/as/token.oauth2",
                        "revoke" => path == "/as/revoke_token.oauth2",
                        _ => false,
                    };
                    if should_redirect {
                        Reply::found(destination.clone())
                    } else if path == "/aio/api/v1/mfa/validate/full" {
                        Reply::json(200, serde_json::json!({"access_token":SESSION}).to_string())
                    } else {
                        Reply::json(404, b"{}".to_vec())
                    }
                }
            })
        };
        let api = client(&server, Duration::from_secs(1));
        match endpoint {
            "settings" | "credential" => {
                assert!(
                    api.login(USERNAME, &SecretString::new(PASSWORD), None)
                        .await
                        .is_err()
                );
            }
            "token" => {
                assert!(api.refresh(&SecretString::new(REFRESH)).await.is_err());
            }
            "revoke" => {
                assert_eq!(
                    api.revoke(&SecretString::new(REFRESH)).await.unwrap_err(),
                    SsoError::RefreshRejected
                );
            }
            _ => unreachable!(),
        }
        assert!(
            matches!(spy.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
            "{endpoint} redirect reached foreign listener"
        );
        let requests = server.finish();
        assert_eq!(requests.len(), if endpoint == "settings" { 1 } else { 2 });
    }
}

#[tokio::test]
async fn refresh_rotates_tokens_and_revoke_posts_the_refresh_token_form() {
    let server = standard_server(|request| {
        let path = Url::parse(&format!("http://localhost{}", request.target))
            .unwrap()
            .path()
            .to_owned();
        match path.as_str() {
            "/as/token.oauth2" => Reply::json(200, serde_json::json!({"access_token":jwt("rotated", future_exp()),"refresh_token":"refresh-rotated"}).to_string()),
            "/as/revoke_token.oauth2" => Reply::json(200, b"{}".to_vec()),
            _ => Reply::json(404, b"{}".to_vec()),
        }
    });
    let api = client(&server, Duration::from_secs(1));
    let tokens = api
        .refresh(&SecretString::new(REFRESH))
        .await
        .expect("refresh succeeds");
    assert_eq!(tokens.refresh.expose_secret(), "refresh-rotated");
    api.revoke(&SecretString::new(REFRESH))
        .await
        .expect("revoke succeeds");
    let requests = server.finish();
    assert_eq!(requests.len(), 3);
    let refresh = form(&requests[1]);
    assert_form_content_type(&requests[1]);
    assert_eq!(
        refresh.get("grant_type").map(String::as_str),
        Some("refresh_token")
    );
    assert_eq!(
        refresh.get("client_id").map(String::as_str),
        Some(CLIENT_ID)
    );
    assert_eq!(
        refresh.get("refresh_token").map(String::as_str),
        Some(REFRESH)
    );
    assert_eq!(requests[2].target, "/as/revoke_token.oauth2");
    let revoke = form(&requests[2]);
    assert_form_content_type(&requests[2]);
    assert_eq!(revoke.get("client_id").map(String::as_str), Some(CLIENT_ID));
    assert_eq!(revoke.get("token").map(String::as_str), Some(REFRESH));
}

#[tokio::test]
async fn unchanged_refresh_token_is_rejected_and_non_json_revoke_error_is_redacted() {
    let server = standard_server(|request| {
        let path = Url::parse(&format!("http://localhost{}", request.target))
            .unwrap()
            .path()
            .to_owned();
        match path.as_str() {
            "/as/token.oauth2" => Reply::json(200, serde_json::json!({"access_token":jwt("access", future_exp()),"refresh_token":REFRESH}).to_string()),
            "/as/revoke_token.oauth2" => Reply { status: 401, headers: vec![("Content-Type".into(), "text/html".into())], body: b"refresh-sentinel provider-body-sentinel".to_vec(), delay: Duration::ZERO, chunked: false },
            _ => Reply::json(404, b"{}".to_vec()),
        }
    });
    let api = client(&server, Duration::from_secs(1));
    assert_eq!(
        api.refresh(&SecretString::new(REFRESH)).await.unwrap_err(),
        SsoError::Config("SSO did not rotate the refresh token")
    );
    let error = api.revoke(&SecretString::new(REFRESH)).await.unwrap_err();
    assert_eq!(error, SsoError::RefreshRejected);
    assert!(error.to_string().contains("instantctl auth login"));
    assert!(!error.to_string().contains(REFRESH));
    assert!(!error.to_string().contains("provider-body-sentinel"));
    assert!(!format!("{error:?}").contains("provider-body-sentinel"));
    let requests = server.finish();
    assert_eq!(requests.len(), 3);
    assert_form_content_type(&requests[1]);
    assert_form_content_type(&requests[2]);
}

#[tokio::test]
async fn unsafe_settings_origins_and_redirects_are_rejected_before_credentials_post() {
    let variants: [SettingsVariant; 10] = [
        |origin| {
            serde_json::json!({"ssoFqdn":origin.replace("http://", "https://"),"ssoClientIdAuthZ":CLIENT_ID,"ssoRedirectUrl":origin}).to_string()
        },
        |origin| {
            serde_json::json!({"ssoFqdn":"http://user:pass@127.0.0.1/","ssoClientIdAuthZ":CLIENT_ID,"ssoRedirectUrl":origin}).to_string()
        },
        |origin| {
            serde_json::json!({"ssoFqdn":format!("{origin}/nested"),"ssoClientIdAuthZ":CLIENT_ID,"ssoRedirectUrl":origin}).to_string()
        },
        |origin| {
            serde_json::json!({"ssoFqdn":origin.replace("127.0.0.1", "localhost"),"ssoClientIdAuthZ":CLIENT_ID,"ssoRedirectUrl":origin}).to_string()
        },
        |origin| {
            let mut url = Url::parse(origin).unwrap();
            url.set_port(Some(1)).unwrap();
            serde_json::json!({"ssoFqdn":url.as_str(),"ssoClientIdAuthZ":CLIENT_ID,"ssoRedirectUrl":origin}).to_string()
        },
        |origin| {
            serde_json::json!({"ssoFqdn":origin,"ssoClientIdAuthZ":CLIENT_ID,"ssoRedirectUrl":origin.replace("http://", "https://")}).to_string()
        },
        |origin| {
            serde_json::json!({"ssoFqdn":origin,"ssoClientIdAuthZ":CLIENT_ID,"ssoRedirectUrl":format!("{origin}/callback")}).to_string()
        },
        |origin| {
            serde_json::json!({"ssoFqdn":origin,"ssoClientIdAuthZ":CLIENT_ID,"ssoRedirectUrl":"http://user:pass@127.0.0.1/"}).to_string()
        },
        |origin| {
            serde_json::json!({"ssoFqdn":origin,"ssoClientIdAuthZ":CLIENT_ID,"ssoRedirectUrl":origin.replace("127.0.0.1", "localhost")}).to_string()
        },
        |origin| {
            let mut url = Url::parse(origin).unwrap();
            url.set_port(Some(1)).unwrap();
            serde_json::json!({"ssoFqdn":origin,"ssoClientIdAuthZ":CLIENT_ID,"ssoRedirectUrl":url.as_str()}).to_string()
        },
    ];
    for variant in variants {
        let server = configured_server(variant, |_| {
            Reply::json(200, serde_json::json!({"access_token":SESSION}).to_string())
        });
        let error = client(&server, Duration::from_secs(1))
            .login(USERNAME, &SecretString::new(PASSWORD), None)
            .await
            .unwrap_err();
        assert!(matches!(error, SsoError::Config(_)), "{error:?}");
        let requests = server.finish();
        assert_eq!(
            requests.len(),
            1,
            "only settings are fetched before rejecting config"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn bounded_chunked_non_json_and_timeout_fail_with_redacted_errors() {
    let (_clock_sender, clock_receiver) = mpsc::channel::<()>();
    let (clock_started, clock_ready) = oneshot::channel();
    tokio::task::spawn_blocking(move || {
        let _ = clock_started.send(());
        let _ = clock_receiver.recv();
    });
    clock_ready
        .await
        .expect("clock guard blocking task started");

    let cases = [
        Reply::json(200, vec![b'x'; 64 * 1024 + 1]),
        Reply::json(200, vec![b'x'; 64 * 1024 + 1]).chunked(),
        Reply {
            status: 200,
            headers: vec![("Content-Type".into(), "text/html".into())],
            body: PASSWORD.as_bytes().to_vec(),
            delay: Duration::ZERO,
            chunked: false,
        },
        Reply::json(200, serde_json::json!({"access_token":SESSION}).to_string()),
    ];
    let expected = ["size limit", "size limit", "non-JSON", "timed out"];
    for (index, reply) in cases.into_iter().enumerate() {
        let (server, mut gate) = if index == 3 {
            let (handler, response_gate) = gated_response(reply.clone());
            (standard_server(handler), Some(response_gate))
        } else {
            (standard_server(move |_request| reply.clone()), None)
        };
        let api = client(
            &server,
            if index == 3 {
                Duration::from_millis(40)
            } else {
                Duration::from_secs(1)
            },
        );
        let result = if let Some(gate) = gate.as_mut() {
            let login = tokio::spawn(async move {
                api.login(USERNAME, &SecretString::new(PASSWORD), None)
                    .await
            });
            let request = gate.wait_until_reached().await;
            tokio::time::advance(Duration::from_millis(41)).await;
            let result = login.await;
            gate.release();
            assert_eq!(request.target, "/aio/api/v1/mfa/validate/full");
            result.expect("login task should return a timeout")
        } else {
            api.login(USERNAME, &SecretString::new(PASSWORD), None)
                .await
        };
        let error = result.expect_err("unsafe or delayed response must fail");
        assert!(error.to_string().contains(expected[index]), "{error:?}");
        assert!(!error.to_string().contains(PASSWORD));
        assert!(!format!("{error:?}").contains(PASSWORD));
        server.finish();
    }
}

#[tokio::test]
async fn token_exchange_rejects_missing_or_invalid_jwt_expiry() {
    for access in [
        "not-a-jwt".to_owned(),
        jwt("no-exp", 0),
        format!(
            "{}.{}.sig",
            URL_SAFE_NO_PAD.encode(b"{}"),
            URL_SAFE_NO_PAD.encode(b"{}")
        ),
    ] {
        let access = access.clone();
        let server = standard_server(move |request| {
            let path = Url::parse(&format!("http://localhost{}", request.target))
                .unwrap()
                .path()
                .to_owned();
            if path == "/as/token.oauth2" {
                Reply::json(
                    200,
                    serde_json::json!({"access_token":access,"refresh_token":REFRESH}).to_string(),
                )
            } else if path == "/aio/api/v1/mfa/validate/full" {
                Reply::json(200, serde_json::json!({"access_token":SESSION}).to_string())
            } else if path == "/as/authorization.oauth2" {
                auth_redirect(request, "/")
            } else {
                Reply::json(404, b"{}".to_vec())
            }
        });
        let error = client(&server, Duration::from_secs(1))
            .login(USERNAME, &SecretString::new(PASSWORD), None)
            .await
            .unwrap_err();
        assert!(
            matches!(
                error,
                SsoError::Config("SSO returned an invalid access-token expiry")
            ),
            "{error:?}"
        );
        server.finish();
    }
}
