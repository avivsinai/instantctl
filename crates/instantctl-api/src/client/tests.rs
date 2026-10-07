use super::*;
use crate::{ErrorKind, StaticToken};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    sync::{
        Arc, Condvar, Mutex,
        mpsc::{self, Receiver},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use url::Url;

mod administration;
mod clock;
mod country;
mod firmware;
mod policies;
mod port_diagnostics;
mod port_settings;
mod ports;
mod profile;
mod site_lifecycle;
mod stacks;
use clock::{apply_readback_after, apply_readback_once, keep_clock_paused};

const TOKEN: &str = "netcli-test-token-sentinel";
const BODY_SENTINEL: &str = "response-body-sentinel";

#[derive(Clone)]
struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    gate: Option<Arc<ResponseGate>>,
    chunked: bool,
    disconnect: bool,
}

impl Reply {
    fn json(status: u16, body: impl Into<Vec<u8>>) -> Self {
        Self {
            status,
            headers: vec![("Content-Type".into(), "application/json".into())],
            body: body.into(),
            gate: None,
            chunked: false,
            disconnect: false,
        }
    }

    fn empty(status: u16) -> Self {
        Self::json(status, Vec::new())
    }

    fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    fn gated(mut self, gate: &Arc<ResponseGate>) -> Self {
        self.gate = Some(Arc::clone(gate));
        self
    }

    fn chunked(mut self) -> Self {
        self.chunked = true;
        self
    }

    fn disconnect(mut self) -> Self {
        self.disconnect = true;
        self
    }
}

#[derive(Default)]
struct ResponseGate {
    reached: tokio::sync::Notify,
    released: Mutex<bool>,
    ready: Condvar,
}

impl ResponseGate {
    fn wait(&self) {
        self.reached.notify_one();
        let mut released = self.released.lock().expect("gate lock");
        while !*released {
            released = self.ready.wait(released).expect("gate release");
        }
    }

    fn release(&self) {
        *self.released.lock().expect("gate lock") = true;
        self.ready.notify_all();
    }
}

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
    gates: Vec<Arc<ResponseGate>>,
}

impl MockServer {
    fn start(replies: Vec<Reply>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback test server");
        listener
            .set_nonblocking(true)
            .expect("set loopback listener nonblocking");
        let addr = listener.local_addr().expect("read loopback address");
        let base = Url::parse(&format!("http://{addr}/api")).expect("parse loopback base URL");
        let (sender, requests) = mpsc::channel();
        let (stop, stopped) = mpsc::channel();
        let gates = replies
            .iter()
            .filter_map(|reply| reply.gate.clone())
            .collect();
        let worker = thread::spawn(move || serve(listener, replies, sender, stopped));

        Self {
            base,
            requests,
            stop,
            worker: Some(worker),
            gates,
        }
    }

    fn finish(mut self) -> Vec<Request> {
        let _ = self.stop.send(());
        for gate in &self.gates {
            gate.release();
        }
        if let Some(worker) = self.worker.take() {
            worker.join().expect("mock server worker should not panic");
        }
        self.requests.try_iter().collect()
    }
}

impl Drop for MockServer {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        for gate in &self.gates {
            gate.release();
        }
        if let Some(worker) = self.worker.take() {
            // Stop is observed after every bounded socket read.
            let _ = worker.join();
        }
    }
}

fn serve(
    listener: TcpListener,
    replies: Vec<Reply>,
    sender: mpsc::Sender<Request>,
    stopped: Receiver<()>,
) {
    let started = Instant::now();
    let hard_deadline = started + Duration::from_secs(30);
    let mut handled = 0usize;

    while Instant::now() < hard_deadline && handled < 32 {
        match stopped.try_recv() {
            Ok(()) | Err(mpsc::TryRecvError::Disconnected) => break,
            Err(mpsc::TryRecvError::Empty) => {}
        }
        match listener.accept() {
            Ok((mut stream, _)) => {
                // Darwin inherits the listener's nonblocking mode on accept.
                stream
                    .set_nonblocking(false)
                    .expect("set accepted test socket blocking");
                let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
                let _ = stream.set_write_timeout(Some(Duration::from_millis(500)));
                if let Some(request) = read_request(&mut stream) {
                    let _ = sender.send(request);
                    let reply = replies
                        .get(handled)
                        .or_else(|| replies.last())
                        .cloned()
                        .unwrap_or_else(|| Reply::empty(500));
                    if let Some(gate) = &reply.gate {
                        gate.wait();
                    }
                    write_reply(&mut stream, &reply);
                    handled += 1;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(5));
            }
            Err(_) => break,
        }
    }
}

fn read_request(stream: &mut TcpStream) -> Option<Request> {
    const MAX_HEADER_BYTES: usize = 64 * 1024;
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 4096];
    let (header_end, content_length) = loop {
        let count = stream.read(&mut chunk).ok()?;
        if count == 0 {
            return None;
        }
        bytes.extend_from_slice(&chunk[..count]);
        let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") else {
            if bytes.len() > MAX_HEADER_BYTES {
                return None;
            }
            continue;
        };
        let header_end = end + 4;
        let headers = std::str::from_utf8(&bytes[..header_end]).ok()?;
        let content_length = headers
            .lines()
            .skip(1)
            .filter_map(|line| line.split_once(':'))
            .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
            .and_then(|(_, value)| value.trim().parse::<usize>().ok())
            .unwrap_or(0);
        if content_length > 1024 * 1024 {
            return None;
        }
        break (header_end, content_length);
    };

    while bytes.len() < header_end + content_length {
        let count = stream.read(&mut chunk).ok()?;
        if count == 0 {
            return None;
        }
        bytes.extend_from_slice(&chunk[..count]);
    }

    let header_text = std::str::from_utf8(&bytes[..header_end]).ok()?;
    let mut lines = header_text.lines();
    let mut request_line = lines.next()?.split_whitespace();
    let method = request_line.next()?.to_owned();
    let target = request_line.next()?.to_owned();
    let headers = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.to_ascii_lowercase(), value.trim().to_owned()))
        .collect();

    Some(Request {
        method,
        target,
        headers,
        body: bytes[header_end..header_end + content_length].to_vec(),
    })
}

fn write_reply(stream: &mut TcpStream, reply: &Reply) {
    if reply.disconnect {
        return;
    }
    let reason = match reply.status {
        200 => "OK",
        204 => "No Content",
        302 => "Found",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        408 => "Request Timeout",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "Mock Response",
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

fn make_client(server: &MockServer, timeout: Duration) -> Client {
    let token = StaticToken::new(TOKEN).expect("valid test token");
    Client::build(token, timeout, server.base.clone()).expect("build client for loopback server")
}

#[derive(Clone)]
enum Operation {
    Get,
    Raw(crate::api::Request),
    PutFull(Value),
    Create(Value),
    Delete,
    Action {
        id: String,
        action: String,
        body: Value,
    },
    BatchCreate(Vec<Value>),
    BatchUpdate(Vec<Value>),
    BatchDelete(Vec<String>),
    BatchAction {
        action: String,
        ids: Vec<String>,
        body: Value,
    },
}

async fn invoke(client: Client, path: &str, operation: Operation) -> Result<Value, Error> {
    match operation {
        Operation::Get => client.get(path).await,
        Operation::Raw(request) => client.raw_api(&request).await,
        Operation::PutFull(body) => client.put_full(path, &body).await,
        Operation::Create(body) => client.create(path, &body).await,
        Operation::Delete => client.delete(path).await,
        Operation::Action { id, action, body } => client.action(path, &id, &action, &body).await,
        Operation::BatchCreate(values) => client.batch_create(path, &values).await,
        Operation::BatchUpdate(values) => client.batch_update(path, &values).await,
        Operation::BatchDelete(ids) => client.batch_delete(path, &ids).await,
        Operation::BatchAction { action, ids, body } => {
            client.batch_action(path, &action, &ids, &body).await
        }
    }
}

async fn assert_operation(
    method: &str,
    expected_target: &str,
    path: &str,
    operation: Operation,
    expected_body: Option<Value>,
    reply: Reply,
    expected_result: Value,
) -> Request {
    let server = MockServer::start(vec![reply]);
    let client = make_client(&server, Duration::from_secs(1));
    let result = invoke(client, path, operation).await;
    assert_eq!(
        result.expect("operation should return JSON data"),
        expected_result
    );

    let requests = server.finish();
    assert_eq!(requests.len(), 1);
    let request = requests.into_iter().next().expect("one recorded request");
    assert_eq!(request.method, method);
    assert_eq!(request.target, expected_target);
    if let Some(expected_body) = expected_body {
        assert_eq!(
            serde_json::from_slice::<Value>(&request.body).ok(),
            Some(expected_body)
        );
    } else {
        assert!(request.body.is_empty());
    }
    request
}

#[tokio::test]
async fn helpers_send_the_expected_methods_paths_headers_and_bodies() {
    let value = json!({"kind":"item","id":"a","name":"edge"});
    let rows = vec![json!({"id":"a"}), json!({"id":"b"})];
    let ids = vec!["a".to_owned(), "b".to_owned()];
    let cases = [
        (
            "GET",
            "/api/sites/site-1/inventory",
            "/sites/site-1/inventory",
            Operation::Get,
            None,
            Reply::json(200, br#"{"ok":true}"#.to_vec()),
            json!({"ok":true}),
        ),
        (
            "PUT",
            "/api/sites/site-1/inventory/a",
            "/sites/site-1/inventory/a",
            Operation::PutFull(value.clone()),
            Some(value.clone()),
            Reply::json(200, br#"{"ok":true}"#.to_vec()),
            json!({"ok":true}),
        ),
        (
            "POST",
            "/api/sites/site-1/networks",
            "/sites/site-1/networks",
            Operation::Create(value.clone()),
            Some(value.clone()),
            Reply::json(200, br#"{"ok":true}"#.to_vec()),
            json!({"ok":true}),
        ),
        (
            "DELETE",
            "/api/sites/site-1/networks/a",
            "/sites/site-1/networks/a",
            Operation::Delete,
            None,
            Reply::empty(204),
            Value::Null,
        ),
        (
            "POST",
            "/api/sites/site-1/inventory/a?action=reboot",
            "/sites/site-1/inventory",
            Operation::Action {
                id: "a".into(),
                action: "reboot".into(),
                body: json!({"id":"a"}),
            },
            Some(json!({"id":"a"})),
            Reply::json(200, br#"{"ok":true}"#.to_vec()),
            json!({"ok":true}),
        ),
        (
            "POST",
            "/api/sites/site-1/networks/batchCreate",
            "/sites/site-1/networks",
            Operation::BatchCreate(rows.clone()),
            Some(json!({"elements":rows})),
            Reply::json(200, br#"{"ok":true}"#.to_vec()),
            json!({"ok":true}),
        ),
        (
            "POST",
            "/api/sites/site-1/networks/batchUpdate",
            "/sites/site-1/networks",
            Operation::BatchUpdate(rows.clone()),
            Some(json!({"elements":rows})),
            Reply::json(200, br#"{"ok":true}"#.to_vec()),
            json!({"ok":true}),
        ),
        (
            "POST",
            "/api/sites/site-1/networks/batchDelete",
            "/sites/site-1/networks",
            Operation::BatchDelete(ids.clone()),
            Some(json!({"ids":ids})),
            Reply::json(200, br#"{"ok":true}"#.to_vec()),
            json!({"ok":true}),
        ),
        (
            "POST",
            "/api/sites/site-1/networks/batchExecute?action=enable",
            "/sites/site-1/networks",
            Operation::BatchAction {
                action: "enable".into(),
                ids: ids.clone(),
                body: json!({"mode":"safe"}),
            },
            Some(json!({"ids":ids,"mode":"safe"})),
            Reply::json(200, br#"{"ok":true}"#.to_vec()),
            json!({"ok":true}),
        ),
    ];

    for (method, target, path, operation, body, reply, result) in cases {
        let request = assert_operation(method, target, path, operation, body, reply, result).await;
        assert_eq!(
            request.headers.get("authorization").map(String::as_str),
            Some("Bearer netcli-test-token-sentinel")
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
    }
}

#[tokio::test]
async fn redirect_is_not_followed_and_cannot_send_the_token_to_another_origin() {
    let destination = TcpListener::bind("127.0.0.1:0").expect("bind redirect destination");
    destination
        .set_nonblocking(true)
        .expect("set redirect destination nonblocking");
    let destination_addr: SocketAddr = destination.local_addr().expect("read destination address");
    let location = format!("http://{destination_addr}/capture");
    let server = MockServer::start(vec![
        Reply::json(302, br#"{"redirect":true}"#.to_vec()).header("Location", &location),
    ]);
    let client = make_client(&server, Duration::from_millis(300));

    let result = client.get("/sites/site-1/inventory").await;
    assert!(result.is_err());
    let requests = server.finish();
    assert_eq!(requests.len(), 1);
    assert!(
        matches!(destination.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
    );
}

#[test]
fn production_client_uses_the_fixed_portal_origin() {
    let token = StaticToken::new(TOKEN).expect("valid test token");
    let client = Client::new(token, Duration::from_secs(1)).expect("build fixed-origin client");
    let url = client
        .url("/sites/site-1/inventory")
        .expect("build API route");

    assert_eq!(
        url.as_str(),
        "https://portal.instant-on.hpe.com/api/sites/site-1/inventory"
    );
}

#[tokio::test]
async fn response_body_is_rejected_above_four_mib() {
    let oversized = vec![b'x'; 4 * 1024 * 1024 + 1];
    let server = MockServer::start(vec![Reply::json(200, oversized)]);
    let client = make_client(&server, Duration::from_secs(2));

    let error = client
        .get("/sites/site-1/inventory")
        .await
        .expect_err("oversized response should fail");
    assert_eq!(error.kind, ErrorKind::General);
    assert!(error.message.contains("size limit"), "{error:?}");
    let requests = server.finish();
    assert_eq!(requests.len(), 1);
}

#[tokio::test]
async fn streamed_response_body_is_bounded_without_a_content_length_header() {
    let oversized = vec![b'x'; 4 * 1024 * 1024 + 1];
    let server = MockServer::start(vec![Reply::json(200, oversized).chunked()]);
    let client = make_client(&server, Duration::from_secs(2));

    let error = client
        .get("/sites/site-1/inventory")
        .await
        .expect_err("oversized streamed response should fail");
    assert_eq!(error.kind, ErrorKind::General);
    assert!(error.message.contains("size limit"), "{error:?}");
    assert_eq!(server.finish().len(), 1);
}

#[tokio::test]
async fn html_and_malformed_or_deep_json_are_safe_errors() {
    let replies = [
        (
            Reply {
                status: 200,
                headers: vec![("Content-Type".into(), "text/html".into())],
                body: b"login page".to_vec(),
                gate: None,
                chunked: false,
                disconnect: false,
            },
            "non-JSON",
        ),
        (Reply::json(200, b"{".to_vec()), "invalid JSON"),
        (
            Reply::json(200, deeply_nested_json(100_000)),
            "invalid JSON",
        ),
    ];

    for (reply, expected_message) in replies {
        let server = MockServer::start(vec![reply]);
        let client = make_client(&server, Duration::from_secs(2));
        let error = client
            .get("/sites/site-1/inventory")
            .await
            .expect_err("invalid response should fail safely");
        assert_eq!(error.kind, ErrorKind::General);
        assert!(error.message.contains(expected_message));
        let requests = server.finish();
        assert_eq!(requests.len(), 1);
    }
}

fn deeply_nested_json(depth: usize) -> Vec<u8> {
    let mut body = Vec::with_capacity(depth * 2 + 1);
    body.resize(depth, b'[');
    body.push(b'0');
    body.resize(depth * 2 + 1, b']');
    body
}

#[tokio::test]
async fn status_codes_map_to_typed_errors_without_retrying_other_statuses() {
    for (status, kind, attempts) in [
        (401, ErrorKind::Auth, 1),
        (403, ErrorKind::Auth, 1),
        (404, ErrorKind::NotFound, 1),
        (400, ErrorKind::ClientError, 1),
        (408, ErrorKind::RetryLater, 1),
        (500, ErrorKind::General, 1),
        (429, ErrorKind::RetryLater, 3),
        (503, ErrorKind::General, 3),
    ] {
        let echoed_secret = format!("{TOKEN} {BODY_SENTINEL}");
        let reply = Reply::json(status, echoed_secret.into_bytes()).header("Retry-After", "0");
        let server = MockServer::start(vec![reply; attempts]);
        let client = make_client(&server, Duration::from_secs(1));
        let error = client
            .get("/sites/site-1/inventory")
            .await
            .expect_err("HTTP status should fail");

        assert_eq!(error.kind, kind);
        assert!(!error.to_string().contains(TOKEN));
        assert!(!error.to_string().contains(BODY_SENTINEL));
        assert!(!format!("{error:?}").contains(TOKEN));
        assert!(!format!("{error:?}").contains(BODY_SENTINEL));
        let requests = server.finish();
        assert_eq!(requests.len(), attempts);
        assert!(requests.iter().all(|request| request.method == "GET"));
    }
}

#[tokio::test]
async fn post_put_and_delete_are_never_retried_on_503() {
    let cases = [
        ("POST", Operation::Create(json!({"name":"new"}))),
        ("PUT", Operation::PutFull(json!({"id":"a","name":"full"}))),
        ("DELETE", Operation::Delete),
        (
            "POST",
            Operation::Raw(
                crate::api::Request::new(
                    crate::api::Method::Post,
                    "/sites/site-1/items",
                    &[],
                    Some(br#"{"name":"new"}"#.to_vec()),
                )
                .unwrap(),
            ),
        ),
    ];

    for (method, operation) in cases {
        let server = MockServer::start(vec![Reply::json(503, b"temporary".to_vec()); 3]);
        let client = make_client(&server, Duration::from_secs(1));
        let result = invoke(client, "/sites/site-1/items", operation).await;
        assert!(result.is_err());
        let requests = server.finish();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, method);
    }
}

#[tokio::test]
async fn post_transport_disconnect_is_not_retried() {
    let server = MockServer::start(vec![Reply::empty(503).disconnect()]);
    let client = make_client(&server, Duration::from_secs(1));

    let result = client
        .create("/sites/site-1/items", &json!({"name":"new"}))
        .await;
    assert!(result.is_err());
    let requests = server.finish();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "POST");
}

#[tokio::test(start_paused = true)]
async fn timeout_returns_a_redacted_transport_error() {
    let _clock = keep_clock_paused().await;
    let gate = Arc::new(ResponseGate::default());
    let server = MockServer::start(vec![
        Reply::json(200, br#"{"ok":true}"#.to_vec()).gated(&gate),
    ]);
    let client = make_client(&server, Duration::from_millis(25));

    let request = client.get("/sites/site-1/inventory");
    tokio::pin!(request);
    tokio::select! {
        biased;
        result = &mut request => panic!("request returned before fixture gate: {result:?}"),
        () = gate.reached.notified() => {}
    }
    tokio::time::advance(Duration::from_millis(26)).await;
    let error = request.await.expect_err("gated response should time out");
    gate.release();
    assert_eq!(error.kind, ErrorKind::General);
    assert!(!error.to_string().contains(TOKEN));
    assert!(!format!("{error:?}").contains(TOKEN));
    assert_eq!(server.finish().len(), 1);
}

#[test]
fn static_token_debug_is_redacted() {
    let token = StaticToken::new(TOKEN).expect("valid test token");
    assert!(!format!("{token:?}").contains(TOKEN));
}

#[test]
fn route_validation_rejects_origin_escape_traversal_and_controls() {
    for path in [
        "https://attacker.invalid/capture",
        "//attacker.invalid/capture",
        "/sites/../secrets",
        "/sites/%2e%2e/secrets",
        "/sites/%2",
        "/%252e%252e/admin",
        "/%252fadmin",
        "/%255cadmin",
        r"/sites\..\secrets",
        "/sites/ok\nX-Evil: yes",
    ] {
        assert!(validate_path(path).is_err());
    }
    assert!(validate_path("/sites/site-1/inventory?limit=10").is_ok());
    assert!(validate_path("/sites/site-1/inventory/aa%3Abb%3Acc%3Add%3Aee%3Aff").is_ok());
}

#[tokio::test]
async fn action_id_and_action_values_are_encoded_as_single_segments_and_query_values() {
    let server = MockServer::start(vec![Reply::json(200, br#"{"ok":true}"#.to_vec())]);
    let client = make_client(&server, Duration::from_secs(1));

    let result = client
        .action(
            "/sites/site-1/items",
            "alpha?beta# gamma",
            "activate&admin=true",
            &json!({"id":"alpha?beta# gamma"}),
        )
        .await;
    assert!(result.is_ok());
    let requests = server.finish();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "POST");
    assert!(
        requests[0]
            .target
            .starts_with("/api/sites/site-1/items/alpha%3Fbeta%23%20gamma?")
    );
    assert!(
        requests[0]
            .target
            .contains("action=activate%26admin%3Dtrue")
    );
    assert!(!requests[0].target.contains("/alpha?beta"));
}

#[test]
fn retry_after_delay_is_clamped_and_invalid_values_use_the_default() {
    assert_eq!(retry_delay(Some("900")), Duration::from_secs(2));
    assert_eq!(retry_delay(Some("-10")), Duration::ZERO);
    assert_eq!(retry_delay(Some("NaN")), Duration::from_millis(250));
    assert_eq!(retry_delay(None), Duration::from_millis(250));
}

mod access_portal_schedule;
mod access_radius;
mod allowlist;
mod api;
mod client_operations;
mod device;
mod device_reservations;
mod mutation;
mod network;
mod network_routing;
mod radio;
mod site;
mod site_actions;
mod wlan;
mod wlan_advanced;
