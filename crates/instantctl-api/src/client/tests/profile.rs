use super::*;
use crate::{
    CredentialStore, Error, ErrorKind, SsoClient, StoredCredential, Tokens,
    client::profile::ProtectedPortSummary, refreshing::RefreshingTokenSource, secret::SecretString,
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime},
};

const SITE: &str = "123e4567-e89b-12d3-a456-426614174000";
const OTHER_SITE: &str = "123e4567-e89b-12d3-a456-426614174001";
const SWITCH_MAC: &str = "aa:bb:cc:dd:ee:ff";
const SWITCH_NAME: &str = "Closet switch";
const FACEPLATE: u64 = 14;
const ENTRY: &str = "aa:bb:cc:dd:ee:ff:14";
const ACCESS: &str = "profile-test-access";
const REFRESH: &str = "profile-test-refresh";

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        let mut random = [0; 16];
        getrandom::fill(&mut random).expect("generate unique lock directory name");
        let suffix = random
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let path =
            std::env::temp_dir().join(format!("hpe-profile-test-{}-{suffix}", std::process::id()));
        std::fs::create_dir(&path).expect("create private lock directory");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
                .expect("restrict lock directory permissions");
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

#[derive(Clone, Default)]
struct CountingStore {
    inner: Arc<Mutex<StoreState>>,
}

#[derive(Default)]
struct StoreState {
    credentials: HashMap<String, StoredCredential>,
    writes: usize,
}

impl CountingStore {
    fn seed(&self, profile: &str, credential: StoredCredential) {
        self.inner
            .lock()
            .expect("store lock")
            .credentials
            .insert(profile.to_owned(), credential);
    }

    fn writes(&self) -> usize {
        self.inner.lock().expect("store lock").writes
    }

    fn saved(&self, profile: &str) -> StoredCredential {
        self.inner
            .lock()
            .expect("store lock")
            .credentials
            .get(profile)
            .cloned()
            .expect("saved credential")
    }
}

impl CredentialStore for CountingStore {
    fn load(&self, profile: &str) -> Result<Option<StoredCredential>, Error> {
        Ok(self
            .inner
            .lock()
            .expect("store lock")
            .credentials
            .get(profile)
            .cloned())
    }

    fn save(&self, profile: &str, credential: &StoredCredential) -> Result<(), Error> {
        let mut state = self.inner.lock().expect("store lock");
        state.writes += 1;
        state
            .credentials
            .insert(profile.to_owned(), credential.clone());
        Ok(())
    }

    fn delete(&self, profile: &str) -> Result<(), Error> {
        let mut state = self.inner.lock().expect("store lock");
        state.writes += 1;
        state.credentials.remove(profile);
        Ok(())
    }
}

fn credential(expiry: SystemTime, default_site: Option<&str>) -> StoredCredential {
    let mut value = StoredCredential::new(
        "profile-test-user",
        Tokens {
            access: SecretString::new(ACCESS),
            refresh: SecretString::new(REFRESH),
            access_expiry: expiry,
        },
    );
    value.default_site = default_site.map(str::to_owned);
    value
}

async fn client(
    server: &MockServer,
    store: CountingStore,
    profile: &str,
) -> (Client<Arc<RefreshingTokenSource<CountingStore>>>, TestDir) {
    let dir = TestDir::new();
    let source = RefreshingTokenSource::for_test(
        SsoClient::for_test(server.base.clone()),
        store,
        profile,
        dir.lock_path(),
        None,
    )
    .await
    .expect("build refreshing source");
    let client = Client::build(
        Arc::new(source),
        Duration::from_secs(2),
        server.base.clone(),
    )
    .expect("build profile client");
    (client, dir)
}

fn inventory(devices: Vec<Value>) -> Value {
    let count = devices.len() as u64;
    json!({
        "kind":"resourceList", "totalCount":count, "matchingFilterCount":count,
        "pendingAvailability":null, "elements":devices
    })
}

fn switch(name: &str, mac: &str, faceplates: &[u64]) -> Value {
    json!({
        "kind":"inventory", "id":mac, "macAddress":mac, "name":name,
        "deviceType":"switch", "deviceRole":"switch", "status":"up",
        "ethernetPorts":faceplates.iter().map(|faceplate| json!({
            "portNumber":faceplate, "faceplatePortNumber":faceplate,
            "isUplink":false, "isDedicatedUplink":false, "trunkNumber":null
        })).collect::<Vec<_>>(),
        "trunkPorts":[], "capabilities":{}
    })
}

fn sites(elements: Vec<Value>) -> Value {
    let count = elements.len() as u64;
    json!({
        "kind":"resourceList", "totalCount":count, "matchingFilterCount":count,
        "pendingAvailability":null, "elements":elements
    })
}

fn reply(value: Value) -> Reply {
    Reply::json(200, serde_json::to_vec(&value).expect("reply JSON"))
}

fn failure<T>(result: Result<T, Error>) -> Error {
    match result {
        Err(error) => error,
        Ok(_) => panic!("profile operation should fail"),
    }
}

fn summary_parts(summary: &ProtectedPortSummary) -> (&str, Option<&str>, Option<&str>, u64) {
    (
        &summary.entry,
        summary.switch_mac.as_deref(),
        summary.switch_name.as_deref(),
        summary.faceplate,
    )
}

#[tokio::test]
async fn default_site_validates_membership_before_saving_and_rejects_absent_site() {
    let absent_server = MockServer::start(vec![reply(sites(vec![]))]);
    let absent_store = CountingStore::default();
    absent_store.seed(
        "default",
        credential(SystemTime::now() + Duration::from_secs(600), None),
    );
    let (absent_client, _dir) = client(&absent_server, absent_store.clone(), "default").await;
    let absent = failure(absent_client.set_profile_default_site(SITE).await);
    assert_eq!(absent.kind, ErrorKind::NotFound);
    assert_eq!(absent_store.writes(), 0);
    let requests = absent_server.finish();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "GET");
    assert_eq!(requests[0].target, "/api/sites");

    let invalid_server = MockServer::start(vec![]);
    let invalid_store = CountingStore::default();
    invalid_store.seed(
        "default",
        credential(SystemTime::now() - Duration::from_secs(10), None),
    );
    let (invalid_client, _dir) = client(&invalid_server, invalid_store.clone(), "default").await;
    let invalid = failure(invalid_client.set_profile_default_site("../bad").await);
    assert_eq!(invalid.kind, ErrorKind::Usage);
    assert_eq!(invalid_store.writes(), 0);
    assert!(
        invalid_server.finish().is_empty(),
        "invalid UUID must fail before token I/O"
    );

    let valid_server = MockServer::start(vec![reply(sites(vec![json!({
        "kind":"site", "id":SITE, "name":"Home"
    })]))]);
    let valid_store = CountingStore::default();
    valid_store.seed(
        "default",
        credential(SystemTime::now() + Duration::from_secs(600), None),
    );
    let (valid_client, _dir) = client(&valid_server, valid_store.clone(), "default").await;
    let metadata = valid_client
        .set_profile_default_site(SITE)
        .await
        .expect("site member should be saved");
    assert_eq!(metadata.default_site.as_deref(), Some(SITE));
    assert_eq!(
        valid_store.saved("default").default_site.as_deref(),
        Some(SITE)
    );
    assert_eq!(valid_store.writes(), 1);
    let requests = valid_server.finish();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].target, "/api/sites");
}

#[tokio::test]
async fn protected_port_add_list_and_remove_follow_mac_across_switch_rename() {
    let renamed = "Closet switch renamed";
    let server = MockServer::start(vec![
        reply(inventory(vec![switch(
            SWITCH_NAME,
            SWITCH_MAC,
            &[FACEPLATE],
        )])),
        reply(inventory(vec![switch(renamed, SWITCH_MAC, &[FACEPLATE])])),
    ]);
    let store = CountingStore::default();
    store.seed(
        "default",
        credential(SystemTime::now() + Duration::from_secs(600), Some(SITE)),
    );
    let (client, _dir) = client(&server, store.clone(), "default").await;

    let added = client
        .add_profile_protected_port(&format!("{SWITCH_NAME}:{FACEPLATE}"))
        .await
        .expect("add protected switch faceplate");
    assert_eq!(added.protected_ports, [ENTRY]);
    assert_eq!(store.saved("default").protected_ports, [ENTRY]);
    assert_eq!(store.writes(), 1);

    let listed = client
        .profile_protected_ports()
        .await
        .expect("list protected ports using current switch name");
    assert_eq!(listed.len(), 1);
    assert_eq!(
        summary_parts(&listed[0]),
        (ENTRY, Some(SWITCH_MAC), Some(renamed), FACEPLATE)
    );

    let removed = client
        .remove_profile_protected_port(ENTRY)
        .await
        .expect("remove protected port by canonical MAC after rename");
    assert!(removed.protected_ports.is_empty());
    assert!(store.saved("default").protected_ports.is_empty());
    assert_eq!(store.writes(), 2);

    let requests = server.finish();
    assert_eq!(requests.len(), 2);
    assert!(requests.iter().all(|request| request.method == "GET"));
    assert!(
        requests
            .iter()
            .all(|request| request.target == format!("/api/sites/{SITE}/inventory"))
    );
}

#[tokio::test]
async fn exact_saved_name_policy_can_be_removed_after_switch_rename_or_deletion() {
    let entry = format!("{SWITCH_NAME}:{FACEPLATE}");
    for devices in [
        vec![switch("Renamed switch", SWITCH_MAC, &[FACEPLATE])],
        vec![],
    ] {
        let server = MockServer::start(vec![reply(inventory(devices))]);
        let store = CountingStore::default();
        let mut saved = credential(SystemTime::now() + Duration::from_secs(600), Some(SITE));
        saved.protected_ports = vec![entry.clone()];
        store.seed("default", saved);
        let (client, _dir) = client(&server, store.clone(), "default").await;

        let listed = client
            .profile_protected_ports()
            .await
            .expect("list saved policy");
        assert_eq!(listed.len(), 1);
        assert_eq!(
            summary_parts(&listed[0]),
            (entry.as_str(), None, None, FACEPLATE)
        );
        let removed = client
            .remove_profile_protected_port(&listed[0].entry)
            .await
            .expect("remove the exact saved name without current inventory identity");
        assert!(removed.protected_ports.is_empty());
        assert!(store.saved("default").protected_ports.is_empty());
        assert_eq!(store.writes(), 1);
        let requests = server.finish();
        assert_eq!(
            requests.len(),
            1,
            "removal must not resolve a missing old name"
        );
        assert_eq!(requests[0].method, "GET");
    }
}

#[tokio::test]
async fn remove_current_switch_name_resolves_a_saved_mac_policy() {
    let server = MockServer::start(vec![reply(inventory(vec![switch(
        SWITCH_NAME,
        SWITCH_MAC,
        &[FACEPLATE],
    )]))]);
    let store = CountingStore::default();
    let mut saved = credential(SystemTime::now() + Duration::from_secs(600), Some(SITE));
    saved.protected_ports = vec![ENTRY.into()];
    store.seed("default", saved);
    let (client, _dir) = client(&server, store.clone(), "default").await;
    let removed = client
        .remove_profile_protected_port(&format!("{SWITCH_NAME}:{FACEPLATE}"))
        .await
        .expect("resolve the current name to the saved MAC");
    assert!(removed.protected_ports.is_empty());
    assert!(store.saved("default").protected_ports.is_empty());
    assert_eq!(store.writes(), 1);
    let requests = server.finish();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].target, format!("/api/sites/{SITE}/inventory"));
}

#[tokio::test]
async fn protected_port_resolution_fails_without_store_writes() {
    let ambiguous = inventory(vec![
        switch(SWITCH_NAME, SWITCH_MAC, &[FACEPLATE]),
        switch(SWITCH_NAME, "bb:cc:dd:ee:ff:00", &[FACEPLATE]),
    ]);
    let mut unknown_port = switch(SWITCH_NAME, SWITCH_MAC, &[FACEPLATE]);
    unknown_port["ethernetPorts"][0]
        .as_object_mut()
        .unwrap()
        .remove("portNumber");
    let unknown_identity = inventory(vec![unknown_port]);
    let cases = [
        (ambiguous, SWITCH_NAME, ErrorKind::Usage),
        (
            inventory(vec![json!({
                "kind":"inventory", "id":SWITCH_MAC, "macAddress":SWITCH_MAC,
                "name":SWITCH_NAME, "deviceType":"accessPoint", "deviceRole":"accessPoint",
                "ethernetPorts":[{"faceplatePortNumber":FACEPLATE}]
            })]),
            SWITCH_MAC,
            ErrorKind::NotFound,
        ),
        (
            inventory(vec![switch(SWITCH_NAME, SWITCH_MAC, &[2])]),
            SWITCH_NAME,
            ErrorKind::NotFound,
        ),
        (
            inventory(vec![switch(SWITCH_NAME, SWITCH_MAC, &[FACEPLATE])]),
            "bb:cc:dd:ee:ff:00",
            ErrorKind::NotFound,
        ),
        (unknown_identity, SWITCH_NAME, ErrorKind::Unverified),
    ];

    for (body, selector, expected_kind) in cases {
        let server = MockServer::start(vec![reply(body)]);
        let store = CountingStore::default();
        store.seed(
            "default",
            credential(SystemTime::now() + Duration::from_secs(600), Some(SITE)),
        );
        let (client, _dir) = client(&server, store.clone(), "default").await;
        let failure = failure(
            client
                .add_profile_protected_port(&format!("{selector}:{FACEPLATE}"))
                .await,
        );
        assert_eq!(
            failure.kind, expected_kind,
            "selector {selector:?} returned {failure:?}"
        );
        assert_eq!(store.writes(), 0, "selector {selector:?}");
        let requests = server.finish();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "GET");
    }
}

#[tokio::test]
async fn invalid_protected_port_entries_fail_before_token_or_store_access() {
    let invalid_entries = [
        ":14",
        "aa:bb:cc:dd:ee:zz:14",
        "Closet switch\n:14",
        "Closet switch:0",
    ];
    for entry in invalid_entries {
        let server = MockServer::start(vec![]);
        let store = CountingStore::default();
        store.seed(
            "default",
            credential(SystemTime::now() - Duration::from_secs(10), Some(SITE)),
        );
        let (client, _dir) = client(&server, store.clone(), "default").await;
        let failure = failure(client.add_profile_protected_port(entry).await);
        assert_eq!(failure.kind, ErrorKind::Usage, "entry {entry:?}");
        assert_eq!(store.writes(), 0, "entry {entry:?}");
        assert!(
            server.finish().is_empty(),
            "entry {entry:?} made network I/O"
        );
    }
}

#[tokio::test]
async fn missing_default_site_and_inventory_failures_never_store_protected_ports() {
    let missing_site_server = MockServer::start(vec![]);
    let missing_site_store = CountingStore::default();
    missing_site_store.seed(
        "default",
        credential(SystemTime::now() + Duration::from_secs(600), None),
    );
    let (missing_site_client, _dir) =
        client(&missing_site_server, missing_site_store.clone(), "default").await;
    let failure = failure(
        missing_site_client
            .add_profile_protected_port(&format!("{SWITCH_NAME}:{FACEPLATE}"))
            .await,
    );
    assert_eq!(failure.kind, ErrorKind::Usage);
    assert_eq!(missing_site_store.writes(), 0);
    assert!(missing_site_server.finish().is_empty());

    for status in [401, 503] {
        let mut response = Reply::json(status, b"{}".to_vec());
        if status == 503 {
            response = response.header("Retry-After", "0");
        }
        let server = MockServer::start(vec![response]);
        let store = CountingStore::default();
        store.seed(
            "default",
            credential(SystemTime::now() + Duration::from_secs(600), Some(SITE)),
        );
        let (client, _dir) = client(&server, store.clone(), "default").await;
        assert!(
            client
                .add_profile_protected_port(&format!("{SWITCH_NAME}:{FACEPLATE}"))
                .await
                .is_err()
        );
        assert_eq!(store.writes(), 0, "HTTP {status}");
        let requests = server.finish();
        let expected_requests = if status == 503 { 3 } else { 1 };
        assert_eq!(requests.len(), expected_requests, "HTTP {status}");
        assert!(requests.iter().all(|request| request.method == "GET"));
    }
}

#[tokio::test]
async fn listing_deleted_switch_keeps_saved_identity_with_null_current_name() {
    let server = MockServer::start(vec![reply(inventory(vec![]))]);
    let store = CountingStore::default();
    let mut saved = credential(SystemTime::now() + Duration::from_secs(600), Some(SITE));
    saved.protected_ports = vec![ENTRY.to_owned()];
    store.seed("default", saved);
    let (client, _dir) = client(&server, store.clone(), "default").await;
    let listed = client
        .profile_protected_ports()
        .await
        .expect("list saved entry");
    assert_eq!(listed.len(), 1);
    assert_eq!(
        summary_parts(&listed[0]),
        (ENTRY, Some(SWITCH_MAC), None, FACEPLATE)
    );
    assert_eq!(store.writes(), 0);
    assert_eq!(server.finish().len(), 1);
}

#[tokio::test]
async fn listing_empty_policy_needs_no_site_or_inventory_request() {
    let server = MockServer::start(vec![]);
    let store = CountingStore::default();
    store.seed(
        "default",
        credential(SystemTime::now() + Duration::from_secs(600), None),
    );
    let (client, _dir) = client(&server, store.clone(), "default").await;
    assert!(
        client
            .profile_protected_ports()
            .await
            .expect("empty protected-port policy")
            .is_empty()
    );
    assert_eq!(store.writes(), 0);
    assert!(server.finish().is_empty());
}

#[tokio::test(start_paused = true)]
async fn protected_port_add_rejects_a_default_site_replaced_during_inventory_read() {
    let _clock = keep_clock_paused().await;
    let gate = Arc::new(ResponseGate::default());
    let server = MockServer::start(vec![
        reply(inventory(vec![switch(
            SWITCH_NAME,
            SWITCH_MAC,
            &[FACEPLATE],
        )]))
        .gated(&gate),
    ]);
    let store = CountingStore::default();
    store.seed(
        "default",
        credential(SystemTime::now() + Duration::from_secs(600), Some(SITE)),
    );
    let (client, _dir) = client(&server, store.clone(), "default").await;
    let add = tokio::spawn(async move {
        client
            .add_profile_protected_port(&format!("{SWITCH_NAME}:{FACEPLATE}"))
            .await
    });

    gate.reached.notified().await;
    let request = server
        .requests
        .try_recv()
        .expect("inventory GET reached response gate");
    assert_eq!(request.target, format!("/api/sites/{SITE}/inventory"));
    let mut replacement = store.saved("default");
    replacement.default_site = Some(OTHER_SITE.to_owned());
    store.seed("default", replacement);
    gate.release();
    let failure = failure(add.await.expect("add task"));
    assert_eq!(store.writes(), 0, "stale site must not write policy");
    let saved = store.saved("default");
    assert_eq!(saved.default_site.as_deref(), Some(OTHER_SITE));
    assert!(saved.protected_ports.is_empty());
    assert!(
        server.finish().is_empty(),
        "the in-flight request was already observed"
    );
    assert!(
        matches!(failure.kind, ErrorKind::Usage | ErrorKind::Unverified),
        "the stale profile must be rejected as a conflict: {failure:?}"
    );
}
