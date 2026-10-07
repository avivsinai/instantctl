use super::*;
mod profile;
use crate::{
    CredentialStore, Error, ErrorKind, SsoClient, StoredCredential, TokenSource, Tokens,
    secret::SecretString,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use std::{
    collections::HashMap,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use url::Url;

const OLD_REFRESH: &str = "e30.eyJleHAiOjQxMDI0NDQ4MDB9.old-refresh-secret-sentinel";
const NEW_REFRESH: &str = "e30.eyJleHAiOjQxMDI0NDUwMDB9.new-refresh-secret-sentinel";
const ACCESS: &str = "e30.eyJleHAiOjQxMDI0NDgwMDB9.cached-access-secret-sentinel";
const REFRESHED_ACCESS: &str = "e30.eyJleHAiOjQxMDI0NDgwMDB9.refreshed-access-secret-sentinel";
const DEFAULT_SITE: &str = "123e4567-e89b-12d3-a456-426614174000";

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        let mut random = [0; 16];
        getrandom::fill(&mut random).expect("generate unique temp directory name");
        let suffix = random
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        let path = std::env::temp_dir().join(format!(
            "hpe-refreshing-test-{}-{suffix}",
            std::process::id()
        ));
        std::fs::create_dir(&path).expect("create owned temp directory");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
                .expect("secure temp directory");
        }
        Self(path)
    }

    fn lock_path(&self) -> PathBuf {
        self.0.join("profile.lock")
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Request {
    method: String,
    target: String,
    body: String,
}

struct MockServer {
    base: Url,
    access_token: String,
    requests: Arc<Mutex<Vec<Request>>>,
    stop: mpsc::Sender<()>,
    worker: Option<JoinHandle<()>>,
}

struct RefreshResponseGate {
    reached: Option<tokio::sync::oneshot::Receiver<()>>,
    release: mpsc::Sender<()>,
}

impl RefreshResponseGate {
    async fn wait_until_blocked(&mut self) {
        self.reached
            .take()
            .expect("refresh gate can only be awaited once")
            .await
            .expect("server reports refresh request before waiting for release");
    }

    fn release(self) {
        let _ = self.release.send(());
    }
}

struct ClockGuard {
    _sender: mpsc::Sender<()>,
}

async fn keep_clock_paused() -> ClockGuard {
    let (sender, receiver) = mpsc::channel();
    let (started, ready) = tokio::sync::oneshot::channel();
    tokio::task::spawn_blocking(move || {
        let _ = started.send(());
        let _ = receiver.recv();
    });
    ready.await.expect("clock guard blocking task starts");
    ClockGuard { _sender: sender }
}

impl MockServer {
    fn start(revoked: bool, refresh_delay: Duration) -> Self {
        Self::start_with_access(revoked, refresh_delay, REFRESHED_ACCESS.to_owned())
    }

    fn start_with_lifetime(
        revoked: bool,
        refresh_delay: Duration,
        access_lifetime: Duration,
    ) -> Self {
        Self::start_with_access(revoked, refresh_delay, jwt(access_lifetime.as_secs()))
    }

    fn start_with_access(revoked: bool, refresh_delay: Duration, access_token: String) -> Self {
        Self::start_inner(revoked, refresh_delay, access_token, None)
    }

    fn start_gated(revoked: bool) -> (Self, RefreshResponseGate) {
        let (reached_tx, reached_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let server = Self::start_inner(
            revoked,
            Duration::ZERO,
            REFRESHED_ACCESS.to_owned(),
            Some((reached_tx, release_rx)),
        );
        (
            server,
            RefreshResponseGate {
                reached: Some(reached_rx),
                release: release_tx,
            },
        )
    }

    fn start_inner(
        revoked: bool,
        refresh_delay: Duration,
        access_token: String,
        mut gate: Option<(tokio::sync::oneshot::Sender<()>, mpsc::Receiver<()>)>,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback server");
        listener.set_nonblocking(true).expect("set nonblocking");
        let addr = listener.local_addr().expect("read loopback address");
        let base = Url::parse(&format!("http://{addr}/")).expect("parse loopback URL");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&requests);
        let reply_access_token = access_token.clone();
        let (stop, stopped) = mpsc::channel();
        let worker = thread::spawn(move || {
            let deadline = std::time::Instant::now() + Duration::from_secs(30);
            while std::time::Instant::now() < deadline {
                match stopped.try_recv() {
                    Ok(()) | Err(mpsc::TryRecvError::Disconnected) => break,
                    Err(mpsc::TryRecvError::Empty) => {}
                }
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let _ = stream.set_nonblocking(false);
                        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                        let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
                        if let Some(request) = read_request(&mut stream) {
                            let is_settings = request.target == "/settings.json";
                            let is_refresh = request.target == "/as/token.oauth2";
                            if let Ok(mut all) = recorded.lock() {
                                all.push(request);
                            }
                            if is_refresh && let Some((reached, release)) = gate.take() {
                                let _ = reached.send(());
                                let _ = release.recv();
                            }
                            if is_refresh && !refresh_delay.is_zero() {
                                thread::sleep(refresh_delay);
                            }
                            let (status, body) = if is_settings {
                                (
                                    200,
                                    format!(
                                        r#"{{"ssoFqdn":"http://{addr}/","ssoClientIdAuthZ":"test-client","ssoRedirectUrl":"http://{addr}/"}}"#
                                    ),
                                )
                            } else if is_refresh && revoked {
                                (400, r#"{"error":"invalid_grant","error_description":"provider-secret-error-sentinel"}"#.to_owned())
                            } else if is_refresh {
                                (
                                    200,
                                    format!(
                                        r#"{{"access_token":"{reply_access_token}","refresh_token":"{NEW_REFRESH}"}}"#
                                    ),
                                )
                            } else {
                                (404, r#"{"error":"not found"}"#.to_owned())
                            };
                            let _ = write_response(&mut stream, status, body.as_bytes());
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(3))
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            base,
            access_token,
            requests,
            stop,
            worker: Some(worker),
        }
    }

    fn requests(&self) -> Vec<(String, String, String)> {
        self.requests
            .lock()
            .expect("request log lock")
            .iter()
            .map(|r| (r.method.clone(), r.target.clone(), r.body.clone()))
            .collect()
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
    let mut chunk = [0u8; 4096];
    let (head_end, content_length) = loop {
        let count = stream.read(&mut chunk).ok()?;
        if count == 0 {
            return None;
        }
        bytes.extend_from_slice(&chunk[..count]);
        let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") else {
            continue;
        };
        let head_end = end + 4;
        let headers = std::str::from_utf8(&bytes[..head_end]).ok()?;
        let content_length = headers
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().ok())
                    .flatten()
            })
            .unwrap_or(0);
        break (head_end, content_length);
    };
    while bytes.len() < head_end + content_length {
        let count = stream.read(&mut chunk).ok()?;
        if count == 0 {
            return None;
        }
        bytes.extend_from_slice(&chunk[..count]);
    }
    let headers = std::str::from_utf8(&bytes[..head_end]).ok()?;
    let mut first = headers.lines().next()?.split_whitespace();
    let method = first.next()?.to_owned();
    let target = first.next()?.to_owned();
    let body = String::from_utf8(bytes[head_end..head_end + content_length].to_vec()).ok()?;
    Some(Request {
        method,
        target,
        body,
    })
}

fn write_response(stream: &mut TcpStream, status: u16, body: &[u8]) -> std::io::Result<()> {
    let reason = if status == 200 {
        "OK"
    } else if status == 400 {
        "Bad Request"
    } else {
        "Not Found"
    };
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    stream.write_all(body)
}

fn jwt(seconds: u64) -> String {
    let exp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_secs()
        + seconds;
    format!(
        "e30.{}.sig",
        URL_SAFE_NO_PAD.encode(format!(r#"{{"exp":{exp}}}"#))
    )
}

fn assert_future_jwt(token: &str) {
    let mut parts = token.split('.');
    assert!(parts.next().is_some());
    let payload = URL_SAFE_NO_PAD
        .decode(parts.next().expect("JWT payload"))
        .expect("base64 JWT payload");
    assert!(parts.next().is_some());
    assert!(parts.next().is_none());
    let payload: serde_json::Value = serde_json::from_slice(&payload).expect("JWT payload JSON");
    let exp = payload["exp"].as_u64().expect("JWT exp claim");
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_secs();
    assert!(exp > now, "returned access JWT must be unexpired");
}

fn client(server: &MockServer) -> SsoClient {
    SsoClient::for_test(server.base.clone())
}

fn tokens(access_expiry: SystemTime, refresh: &str) -> Tokens {
    Tokens {
        access: SecretString::new(ACCESS),
        refresh: SecretString::new(refresh),
        access_expiry,
    }
}

async fn source(
    server: &MockServer,
    store: RecordingStore,
    profile: &str,
    dir: &TestDir,
    login: Option<(String, Tokens)>,
) -> Result<RefreshingTokenSource<RecordingStore>, Error> {
    RefreshingTokenSource::for_test(client(server), store, profile, dir.lock_path(), login).await
}

fn saved_tokens(expiry: SystemTime, access: &str, refresh: &str) -> StoredCredential {
    StoredCredential {
        username: "user".into(),
        access: SecretString::new(access),
        access_expiry: expiry,
        refresh: SecretString::new(refresh),
        default_site: None,
        protected_ports: Vec::new(),
        refresh_pending: false,
    }
}

fn expired_credential() -> StoredCredential {
    saved_tokens(
        SystemTime::now() + Duration::from_secs(30),
        ACCESS,
        OLD_REFRESH,
    )
}

#[derive(Clone, Default)]
struct RecordingStore {
    inner: Arc<StoreState>,
}

#[derive(Default)]
struct StoreState {
    credentials: Mutex<HashMap<String, StoredCredential>>,
    saves: Mutex<Vec<(String, StoredCredential)>>,
    fail_next_save: AtomicBool,
    fail_new_refresh_save: AtomicBool,
    rotation_saved: tokio::sync::Notify,
}

impl RecordingStore {
    fn fail_next_save(&self) {
        self.inner.fail_next_save.store(true, Ordering::SeqCst);
    }

    fn fail_new_refresh_save_once(&self) {
        self.inner
            .fail_new_refresh_save
            .store(true, Ordering::SeqCst);
    }

    fn saved(&self, profile: &str) -> Option<StoredCredential> {
        self.inner
            .credentials
            .lock()
            .expect("credential map")
            .get(profile)
            .cloned()
    }

    fn save_log(&self) -> Vec<(String, StoredCredential)> {
        self.inner.saves.lock().expect("save log").clone()
    }
}

impl CredentialStore for RecordingStore {
    fn load(&self, profile: &str) -> Result<Option<StoredCredential>, Error> {
        Ok(self.saved(profile))
    }

    fn save(&self, profile: &str, credential: &StoredCredential) -> Result<(), Error> {
        self.inner
            .saves
            .lock()
            .expect("save log")
            .push((profile.to_owned(), credential.clone()));
        let fail_any_save = self.inner.fail_next_save.swap(false, Ordering::SeqCst);
        let fail_rotated_save = credential.refresh.expose_secret() == NEW_REFRESH
            && self
                .inner
                .fail_new_refresh_save
                .swap(false, Ordering::SeqCst);
        if fail_any_save || fail_rotated_save {
            return Err(Error::new(
                ErrorKind::General,
                "credential store write failed",
            ));
        }
        self.inner
            .credentials
            .lock()
            .expect("credential map")
            .insert(profile.to_owned(), credential.clone());
        if credential.refresh.expose_secret() == NEW_REFRESH {
            self.inner.rotation_saved.notify_one();
        }
        Ok(())
    }

    fn delete(&self, profile: &str) -> Result<(), Error> {
        self.inner
            .credentials
            .lock()
            .expect("credential map")
            .remove(profile);
        Ok(())
    }
}

fn seed(store: &RecordingStore, profile: &str, credential: StoredCredential) {
    store.save(profile, &credential).expect("seed credentials");
}

fn refresh_posts(server: &MockServer) -> Vec<String> {
    server
        .requests()
        .into_iter()
        .filter(|(method, target, _)| method == "POST" && target == "/as/token.oauth2")
        .map(|(_, _, body)| body)
        .collect()
}

#[tokio::test]
async fn fresh_cached_login_returns_access_without_http() {
    let server = MockServer::start(false, Duration::ZERO);
    let store = RecordingStore::default();
    let dir = TestDir::new();
    let source = source(
        &server,
        store,
        "home",
        &dir,
        Some((
            "user".into(),
            tokens(SystemTime::now() + Duration::from_secs(600), OLD_REFRESH),
        )),
    )
    .await
    .expect("persist fresh login");

    let access = source.token().await.expect("cached access token");

    assert_eq!(access.expose_secret(), ACCESS);
    assert!(
        server.requests().is_empty(),
        "fresh cached access must avoid network calls"
    );
}

#[tokio::test]
async fn fresh_login_preserves_metadata_only_for_the_same_username() {
    let server = MockServer::start(false, Duration::ZERO);
    let store = RecordingStore::default();
    let mut previous = saved_tokens(
        SystemTime::now() + Duration::from_secs(600),
        ACCESS,
        OLD_REFRESH,
    );
    previous.default_site = Some(DEFAULT_SITE.to_owned());
    previous.protected_ports = vec!["aa:bb:cc:dd:ee:ff:14".into()];
    seed(&store, "home", previous);
    let dir = TestDir::new();
    let same_user = source(
        &server,
        store.clone(),
        "home",
        &dir,
        Some((
            "user".into(),
            tokens(SystemTime::now() + Duration::from_secs(600), OLD_REFRESH),
        )),
    )
    .await
    .expect("same-user login");
    assert_eq!(
        same_user
            .profile_metadata()
            .await
            .expect("same-user metadata"),
        crate::ProfileMetadata {
            default_site: Some(DEFAULT_SITE.to_owned()),
            protected_ports: vec!["aa:bb:cc:dd:ee:ff:14".into()],
        }
    );

    let new_user = source(
        &server,
        store.clone(),
        "home",
        &dir,
        Some((
            "different-user".into(),
            tokens(SystemTime::now() + Duration::from_secs(600), OLD_REFRESH),
        )),
    )
    .await
    .expect("different-user login");
    assert_eq!(
        new_user
            .profile_metadata()
            .await
            .expect("new-user metadata"),
        crate::ProfileMetadata::default()
    );
}

#[tokio::test]
async fn access_inside_skew_refreshes_and_persists_rotated_credential() {
    let server = MockServer::start(false, Duration::ZERO);
    let store = RecordingStore::default();
    let mut credential = expired_credential();
    credential.default_site = Some(DEFAULT_SITE.to_owned());
    credential.protected_ports = vec![
        "aa:bb:cc:dd:ee:ff:14".to_owned(),
        "aa:bb:cc:dd:ee:ff:16".to_owned(),
    ];
    seed(&store, "home", credential);
    let dir = TestDir::new();
    let source = source(&server, store.clone(), "home", &dir, None)
        .await
        .expect("load near-expiry stored login");

    let metadata = source
        .profile_metadata()
        .await
        .expect("metadata for resolved token");
    assert_eq!(metadata.default_site.as_deref(), Some(DEFAULT_SITE));
    assert_eq!(
        metadata.protected_ports,
        ["aa:bb:cc:dd:ee:ff:14", "aa:bb:cc:dd:ee:ff:16"]
    );
    assert_eq!(
        source.default_site().await.expect("legacy site accessor"),
        Some(DEFAULT_SITE.to_owned())
    );
    let access = source.token().await.expect("refreshed access token");

    assert_eq!(access.expose_secret(), REFRESHED_ACCESS);
    assert_future_jwt(access.expose_secret());
    let saved = store.saved("home").expect("saved credentials");
    assert_eq!(saved.refresh.expose_secret(), NEW_REFRESH);
    assert_eq!(saved.default_site.as_deref(), Some(DEFAULT_SITE));
    assert_eq!(
        saved.protected_ports,
        ["aa:bb:cc:dd:ee:ff:14", "aa:bb:cc:dd:ee:ff:16"]
    );

    let mut replacement = saved_tokens(
        SystemTime::now() + Duration::from_secs(600),
        "other-access-token",
        "other-refresh-token",
    );
    replacement.username = "other-user".into();
    replacement.default_site = Some("123e4567-e89b-12d3-a456-426614174001".into());
    replacement.protected_ports = vec!["aa:bb:cc:dd:ee:ff:99".into()];
    store
        .save("home", &replacement)
        .expect("replace stored account");
    assert_eq!(
        source
            .profile_metadata()
            .await
            .expect("metadata remains bound to cached token"),
        metadata
    );
    let posts = refresh_posts(&server);
    assert_eq!(posts.len(), 1);
    let posted_refresh = url::form_urlencoded::parse(posts[0].as_bytes())
        .find(|(key, _)| key == "refresh_token")
        .map(|(_, value)| value.into_owned());
    assert_eq!(posted_refresh.as_deref(), Some(OLD_REFRESH));
}

#[tokio::test]
async fn cached_profile_refuses_to_refresh_after_account_replacement() {
    let server = MockServer::start(false, Duration::ZERO);
    let store = RecordingStore::default();
    seed(&store, "home", expired_credential());
    let dir = TestDir::new();
    let source = source(&server, store.clone(), "home", &dir, None)
        .await
        .expect("load account A")
        .with_skew(Duration::MAX);

    assert_eq!(
        source
            .token()
            .await
            .expect("account A refresh")
            .expose_secret(),
        REFRESHED_ACCESS
    );
    assert_eq!(refresh_posts(&server).len(), 1);

    let replacement = saved_tokens(
        SystemTime::now() + Duration::from_secs(3600),
        ACCESS,
        "e30.eyJleHAiOjQxMDI0NDUwMDB9.account-b-refresh-sentinel",
    );
    let mut replacement = replacement;
    replacement.username = "account-b".into();
    store
        .save("home", &replacement)
        .expect("replace profile account");

    let error = source
        .token()
        .await
        .expect_err("cached account A must not refresh with account B credentials");

    assert_eq!(error.kind, ErrorKind::Auth);
    assert!(error.message.contains("instantctl auth login"));
    assert_eq!(refresh_posts(&server).len(), 1, "no refresh for account B");
    let saved = store.saved("home").expect("account B remains unchanged");
    assert_eq!(saved.username, "account-b");
    assert_eq!(
        saved.refresh.expose_secret(),
        "e30.eyJleHAiOjQxMDI0NDUwMDB9.account-b-refresh-sentinel"
    );
}

#[tokio::test]
async fn concurrent_calls_on_one_instance_share_one_refresh() {
    let (server, mut gate) = MockServer::start_gated(false);
    let store = RecordingStore::default();
    seed(&store, "home", expired_credential());
    let dir = TestDir::new();
    let source = Arc::new(
        source(&server, store.clone(), "home", &dir, None)
            .await
            .expect("load saved login"),
    );

    let mut callers = tokio::task::JoinSet::new();
    let mut ready = Vec::new();
    for _ in 0..16 {
        let source = Arc::clone(&source);
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        ready.push(ready_rx);
        callers.spawn(async move {
            let _ = ready_tx.send(());
            source.token().await.expect("token call")
        });
    }
    for caller_ready in ready {
        caller_ready.await.expect("caller starts");
    }
    gate.wait_until_blocked().await;
    gate.release();
    while let Some(caller) = callers.join_next().await {
        assert_eq!(
            caller.expect("token task").expose_secret(),
            REFRESHED_ACCESS
        );
    }

    assert_eq!(refresh_posts(&server).len(), 1);
    assert_eq!(
        store
            .saved("home")
            .expect("rotated credential")
            .refresh
            .expose_secret(),
        NEW_REFRESH
    );
}

#[tokio::test]
async fn two_instances_with_same_lock_share_one_refresh_and_access() {
    let (server, mut gate) = MockServer::start_gated(false);
    let store = RecordingStore::default();
    seed(&store, "home", expired_credential());
    let dir = TestDir::new();
    let first = Arc::new(
        source(&server, store.clone(), "home", &dir, None)
            .await
            .expect("first source"),
    );
    let second = Arc::new(
        source(&server, store.clone(), "home", &dir, None)
            .await
            .expect("second source"),
    );
    let (first_ready_tx, first_ready_rx) = tokio::sync::oneshot::channel();
    let first_call = {
        let source = Arc::clone(&first);
        tokio::spawn(async move {
            let _ = first_ready_tx.send(());
            source.token().await.expect("first token")
        })
    };
    first_ready_rx.await.expect("first caller starts");
    let (second_ready_tx, second_ready_rx) = tokio::sync::oneshot::channel();
    let second_call = {
        let source = Arc::clone(&second);
        tokio::spawn(async move {
            let _ = second_ready_tx.send(());
            source.token().await.expect("second token")
        })
    };
    second_ready_rx.await.expect("second caller starts");
    gate.wait_until_blocked().await;
    gate.release();

    let first_access = first_call.await.expect("first task");
    let second_access = second_call.await.expect("second task");

    assert_eq!(first_access.expose_secret(), REFRESHED_ACCESS);
    assert_eq!(second_access.expose_secret(), first_access.expose_secret());
    assert_eq!(refresh_posts(&server).len(), 1);
}

#[tokio::test]
async fn recreated_source_reuses_valid_stored_access_without_http() {
    let server = MockServer::start(false, Duration::ZERO);
    let store = RecordingStore::default();
    seed(&store, "home", expired_credential());
    let dir = TestDir::new();
    let first = source(&server, store.clone(), "home", &dir, None)
        .await
        .expect("first source");
    assert_eq!(
        first.token().await.expect("refresh once").expose_secret(),
        REFRESHED_ACCESS
    );
    assert_eq!(refresh_posts(&server).len(), 1);

    let recreated = source(&server, store, "home", &dir, None)
        .await
        .expect("recreated source");
    assert_eq!(
        recreated
            .token()
            .await
            .expect("reuse persisted access")
            .expose_secret(),
        REFRESHED_ACCESS
    );
    assert_eq!(
        refresh_posts(&server).len(),
        1,
        "valid persisted access must not refresh again"
    );
}

#[tokio::test]
async fn new_source_uses_still_valid_stored_access_without_http() {
    let server = MockServer::start(false, Duration::ZERO);
    let store = RecordingStore::default();
    seed(
        &store,
        "home",
        saved_tokens(
            SystemTime::now() + Duration::from_secs(600),
            ACCESS,
            OLD_REFRESH,
        ),
    );
    let dir = TestDir::new();
    let source = source(&server, store, "home", &dir, None)
        .await
        .expect("load valid saved login");

    let access = source.token().await.expect("reuse stored access");

    assert_eq!(access.expose_secret(), ACCESS);
    assert!(
        server.requests().is_empty(),
        "valid stored access must avoid all HTTP requests"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn profile_lock_file_is_private() {
    use std::os::unix::fs::PermissionsExt;

    let server = MockServer::start(false, Duration::ZERO);
    let store = RecordingStore::default();
    seed(
        &store,
        "home",
        saved_tokens(
            SystemTime::now() + Duration::from_secs(600),
            ACCESS,
            OLD_REFRESH,
        ),
    );
    let dir = TestDir::new();
    let source = source(&server, store, "home", &dir, None)
        .await
        .expect("source");
    source.token().await.expect("cached token");
    let mode = std::fs::metadata(dir.lock_path())
        .expect("profile lock file")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600, "profile lock file must be owner-only");
}

#[tokio::test]
async fn source_and_revoked_refresh_errors_redact_both_tokens() {
    let server = MockServer::start(true, Duration::ZERO);
    let store = RecordingStore::default();
    let dir = TestDir::new();
    let source = source(
        &server,
        store,
        "home",
        &dir,
        Some((
            "user".into(),
            tokens(SystemTime::now() + Duration::from_secs(30), OLD_REFRESH),
        )),
    )
    .await
    .expect("persist initial login");

    let source_debug = format!("{source:?}");
    let error = source.token().await.expect_err("revoked token should fail");
    let rendered = format!("{error:?} {error}");
    assert_eq!(error.kind, ErrorKind::Auth);
    assert!(error.message.contains("instantctl auth login"));
    for secret in [
        OLD_REFRESH,
        NEW_REFRESH,
        ACCESS,
        REFRESHED_ACCESS,
        "provider-secret-error-sentinel",
    ] {
        assert!(
            !source_debug.contains(secret),
            "source debug leaked secret text: {secret}"
        );
        assert!(
            !rendered.contains(secret),
            "error leaked secret text: {secret}"
        );
    }
}

#[tokio::test]
async fn stale_instance_reloads_rotated_store_before_using_old_refresh() {
    let server = MockServer::start_with_lifetime(false, Duration::ZERO, Duration::from_secs(30));
    let store = RecordingStore::default();
    let dir = TestDir::new();
    let stale = source(
        &server,
        store.clone(),
        "home",
        &dir,
        Some((
            "user".into(),
            tokens(SystemTime::now() + Duration::from_secs(30), OLD_REFRESH),
        )),
    )
    .await
    .expect("stale source with cached old credential");
    let other = source(&server, store.clone(), "home", &dir, None)
        .await
        .expect("other source");

    let first = other.token().await.expect("rotate stored credentials");
    let reused = stale.token().await.expect("reload stored rotation");

    assert_eq!(reused.expose_secret(), first.expose_secret());
    assert_eq!(
        refresh_posts(&server).len(),
        1,
        "stale source must not send the spent old refresh token"
    );
    let saved = store.saved("home").expect("rotated credential");
    assert_eq!(saved.refresh.expose_secret(), NEW_REFRESH);
    assert!(saved.access_expiry > SystemTime::now());
    assert!(
        saved.access_expiry < SystemTime::now() + Duration::from_secs(60),
        "rotated access must be fresh but inside the skew window"
    );
    assert_eq!(reused.expose_secret(), server.access_token);
}

#[tokio::test(start_paused = true)]
async fn persistence_failure_keeps_profile_lock_until_new_refresh_is_saved() {
    let _clock_guard = keep_clock_paused().await;
    let server = MockServer::start(false, Duration::ZERO);
    let store = RecordingStore::default();
    seed(&store, "home", expired_credential());
    let dir = TestDir::new();
    let first = Arc::new(
        source(&server, store.clone(), "home", &dir, None)
            .await
            .expect("first source"),
    );
    let second = Arc::new(
        source(&server, store.clone(), "home", &dir, None)
            .await
            .expect("second source"),
    );
    store.fail_new_refresh_save_once();

    let first_source = Arc::clone(&first);
    let first_call = tokio::spawn(async move { first_source.token().await });
    let first_error = first_call
        .await
        .expect("first task")
        .expect_err("first save fails once");
    assert_eq!(first_error.kind, ErrorKind::General);
    let pending = store.saved("home").expect("pending old store value");
    assert_eq!(pending.refresh.expose_secret(), OLD_REFRESH);
    assert!(
        pending.refresh_pending,
        "old refresh must be marked unsafe before it is spent"
    );

    let lock_probe = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(dir.lock_path())
        .expect("open independent lock probe");
    assert!(
        matches!(
            lock_probe.try_lock(),
            Err(std::fs::TryLockError::WouldBlock)
        ),
        "failed durable save must retain the real profile lock"
    );

    let second_source = Arc::clone(&second);
    let (second_ready_tx, second_ready_rx) = tokio::sync::oneshot::channel();
    let mut second_call = tokio::spawn(async move {
        let _ = second_ready_tx.send(());
        second_source.token().await.expect("second token")
    });
    second_ready_rx.await.expect("second caller starts");
    let blocked = tokio::time::timeout(Duration::from_millis(40), &mut second_call);
    tokio::pin!(blocked);
    tokio::select! {
        biased;
        result = &mut blocked => panic!("second caller did not wait for saved rotation: {result:?}"),
        _ = tokio::task::yield_now() => {}
    }
    tokio::time::advance(Duration::from_millis(40)).await;
    assert!(
        blocked.await.is_err(),
        "second instance must wait while rotated save is pending"
    );
    assert!(
        !second_call.is_finished(),
        "second instance must remain blocked after persistence failure"
    );

    let first_access = first
        .token()
        .await
        .expect("retry save with rotated credential");
    let second_access = second_call
        .await
        .expect("second source released after save");
    assert_eq!(first_access.expose_secret(), REFRESHED_ACCESS);
    assert_eq!(second_access.expose_secret(), first_access.expose_secret());
    let saved = store.saved("home").expect("new store value");
    assert_eq!(saved.refresh.expose_secret(), NEW_REFRESH);
    assert!(
        !saved.refresh_pending,
        "successful rotation clears the pending marker"
    );
    assert_eq!(
        refresh_posts(&server).len(),
        1,
        "waiting instance must reuse saved rotation"
    );
    let saves = store.save_log();
    assert_eq!(
        saves
            .iter()
            .filter(|(_, item)| item.refresh.expose_secret() == NEW_REFRESH)
            .count(),
        2
    );
}

#[tokio::test]
async fn missing_saved_login_is_auth_error_with_login_hint() {
    let server = MockServer::start(false, Duration::ZERO);
    let dir = TestDir::new();
    let source = source(&server, RecordingStore::default(), "missing", &dir, None)
        .await
        .expect("source");

    let error = source.token().await.expect_err("missing profile must fail");

    assert_eq!(error.kind, ErrorKind::Auth);
    assert!(error.message.contains("instantctl auth login"));
    assert!(
        server.requests().is_empty(),
        "missing credentials must not make HTTP calls"
    );
}

#[tokio::test]
async fn failed_rotation_save_does_not_return_access_or_reuse_spent_refresh() {
    let server = MockServer::start(false, Duration::ZERO);
    let store = RecordingStore::default();
    let dir = TestDir::new();
    let source = source(
        &server,
        store.clone(),
        "home",
        &dir,
        Some((
            "user".into(),
            tokens(SystemTime::now() + Duration::from_secs(30), OLD_REFRESH),
        )),
    )
    .await
    .expect("persist initial login");
    store.fail_new_refresh_save_once();

    let error = source
        .token()
        .await
        .expect_err("first rotated-token save fails");
    assert_eq!(error.kind, ErrorKind::General);
    assert!(!format!("{error:?}").contains(ACCESS));
    let saved = store.saved("home").expect("old pending credential remains");
    assert_eq!(saved.refresh.expose_secret(), OLD_REFRESH);
    assert!(saved.refresh_pending);

    let access = source
        .token()
        .await
        .expect("retry save without refreshing again");

    assert_eq!(access.expose_secret(), REFRESHED_ACCESS);
    let saved = store.saved("home").expect("rotated credential saved");
    assert_eq!(saved.refresh.expose_secret(), NEW_REFRESH);
    assert!(!saved.refresh_pending);
    assert_eq!(refresh_posts(&server).len(), 1);
    assert_eq!(
        store
            .save_log()
            .iter()
            .filter(|(_, item)| item.refresh.expose_secret() == NEW_REFRESH)
            .count(),
        2
    );
}

#[tokio::test]
async fn failed_pending_marker_save_makes_no_refresh_request() {
    let server = MockServer::start(false, Duration::ZERO);
    let store = RecordingStore::default();
    seed(&store, "home", expired_credential());
    let dir = TestDir::new();
    let source = source(&server, store.clone(), "home", &dir, None)
        .await
        .expect("source");
    store.fail_next_save();

    let error = source
        .token()
        .await
        .expect_err("pending marker must be durable before refresh");

    assert_eq!(error.kind, ErrorKind::General);
    assert!(
        refresh_posts(&server).is_empty(),
        "refresh must not start before pending marker save succeeds"
    );
    let saved = store.saved("home").expect("original credential remains");
    assert_eq!(saved.refresh.expose_secret(), OLD_REFRESH);
    assert!(
        !saved.refresh_pending,
        "failed marker write must leave stored state unchanged"
    );
}

#[tokio::test]
async fn dropped_source_with_pending_rotation_requires_login_without_replay() {
    let server = MockServer::start(false, Duration::ZERO);
    let store = RecordingStore::default();
    seed(&store, "home", expired_credential());
    let dir = TestDir::new();
    {
        let source = source(&server, store.clone(), "home", &dir, None)
            .await
            .expect("first source");
        store.fail_new_refresh_save_once();

        let error = source
            .token()
            .await
            .expect_err("final rotated credential save fails");
        assert_eq!(error.kind, ErrorKind::General);
        assert_eq!(refresh_posts(&server).len(), 1);
        let saved = store.saved("home").expect("durable pending credential");
        assert_eq!(saved.refresh.expose_secret(), OLD_REFRESH);
        assert!(saved.refresh_pending);
    }

    let recreated = source(&server, store.clone(), "home", &dir, None)
        .await
        .expect("new process source");
    let error = recreated
        .token()
        .await
        .expect_err("pending refresh requires a new login");

    assert_eq!(error.kind, ErrorKind::Auth);
    assert!(error.message.contains("instantctl auth login"));
    assert!(
        store
            .saved("home")
            .expect("pending credential remains")
            .refresh_pending
    );
    assert_eq!(
        refresh_posts(&server).len(),
        1,
        "new source must not replay the spent refresh token"
    );

    let relogin = source(
        &server,
        store.clone(),
        "home",
        &dir,
        Some((
            "user".into(),
            tokens(SystemTime::now() + Duration::from_secs(600), NEW_REFRESH),
        )),
    )
    .await
    .expect("a fresh login replaces the pending credential");
    assert_eq!(relogin.token().await.unwrap().expose_secret(), ACCESS);
    assert!(!store.saved("home").unwrap().refresh_pending);
    assert_eq!(refresh_posts(&server).len(), 1);
}

#[tokio::test]
async fn pending_refresh_refuses_even_valid_stored_access_without_http() {
    let server = MockServer::start(false, Duration::ZERO);
    let store = RecordingStore::default();
    let mut credential = saved_tokens(
        SystemTime::now() + Duration::from_secs(600),
        ACCESS,
        OLD_REFRESH,
    );
    credential.refresh_pending = true;
    seed(&store, "home", credential);
    let dir = TestDir::new();
    let source = source(&server, store, "home", &dir, None).await.unwrap();
    let error = source.token().await.unwrap_err();
    assert_eq!(error.kind, ErrorKind::Auth);
    assert!(error.message.contains("instantctl auth login"));
    assert!(server.requests().is_empty());
    for secret in [ACCESS, OLD_REFRESH, NEW_REFRESH] {
        assert!(!format!("{error:?} {error}").contains(secret));
    }
}

#[tokio::test]
async fn with_login_returns_save_failure() {
    let server = MockServer::start(false, Duration::ZERO);
    let store = RecordingStore::default();
    store.fail_next_save();
    let dir = TestDir::new();

    let result = source(
        &server,
        store,
        "home",
        &dir,
        Some((
            "user".into(),
            tokens(SystemTime::now() + Duration::from_secs(600), OLD_REFRESH),
        )),
    )
    .await;

    assert!(
        result.is_err(),
        "login must not be installed when credential persistence fails"
    );
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn cancelled_waiter_does_not_cancel_rotation_or_replay_spent_refresh() {
    let (server, mut gate) = MockServer::start_gated(false);
    let store = RecordingStore::default();
    seed(&store, "home", expired_credential());
    let dir = TestDir::new();
    let source = Arc::new(
        source(&server, store.clone(), "home", &dir, None)
            .await
            .expect("source"),
    );
    let caller_source = Arc::clone(&source);
    let caller = tokio::spawn(async move { caller_source.token().await });
    gate.wait_until_blocked().await;
    assert_eq!(refresh_posts(&server).len(), 1);
    caller.abort();
    assert!(caller.await.unwrap_err().is_cancelled());

    gate.release();
    store.inner.rotation_saved.notified().await;
    assert_eq!(
        store
            .saved("home")
            .expect("worker saves rotation without a waiter")
            .refresh
            .expose_secret(),
        NEW_REFRESH
    );
    let access = source.token().await.expect("join the completed refresh");
    assert_eq!(access.expose_secret(), REFRESHED_ACCESS);
    assert_eq!(refresh_posts(&server).len(), 1);
}
