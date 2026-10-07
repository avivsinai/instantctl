use super::*;

fn fresh() -> StoredCredential {
    let mut credential = saved_tokens(
        SystemTime::now() + Duration::from_secs(600),
        ACCESS,
        OLD_REFRESH,
    );
    credential.default_site = Some(DEFAULT_SITE.into());
    credential
}

#[tokio::test]
async fn metadata_patch_reloads_rotated_tokens_and_unrelated_policy() {
    let server = MockServer::start(false, Duration::ZERO);
    let store = RecordingStore::default();
    let dir = TestDir::new();
    seed(&store, "home", fresh());
    let source = source(&server, store.clone(), "home", &dir, None)
        .await
        .unwrap();
    source.profile_metadata().await.unwrap();
    let mut rotated = fresh();
    rotated.access = SecretString::new(REFRESHED_ACCESS);
    rotated.refresh = SecretString::new(NEW_REFRESH);
    rotated.protected_ports = vec!["aa:bb:cc:dd:ee:ff:7".into()];
    let guard = ProfileLock::at(dir.lock_path()).acquire().await.unwrap();
    store.save("home", &rotated).unwrap();
    drop(guard);
    let site = "11111111-2222-3333-4444-555555555555";
    let metadata = source
        .update_profile_metadata(MetadataChange::DefaultSite(site.into()))
        .await
        .unwrap();
    assert_eq!(metadata.default_site.as_deref(), Some(site));
    assert_eq!(metadata.protected_ports, rotated.protected_ports);
    let saved = store.saved("home").unwrap();
    assert_eq!(saved.refresh.expose_secret(), NEW_REFRESH);
    assert_eq!(saved.access_expiry, rotated.access_expiry);
    assert_eq!(
        source.token().await.unwrap().expose_secret(),
        REFRESHED_ACCESS
    );
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn independently_cached_metadata_writers_merge_under_the_profile_lock() {
    let server = MockServer::start(false, Duration::ZERO);
    let store = RecordingStore::default();
    let dir = TestDir::new();
    seed(&store, "home", fresh());
    let first = source(&server, store.clone(), "home", &dir, None)
        .await
        .unwrap();
    let second = source(&server, store.clone(), "home", &dir, None)
        .await
        .unwrap();
    first.profile_metadata().await.unwrap();
    second.profile_metadata().await.unwrap();
    let (a, b) = tokio::join!(
        first.update_profile_metadata(MetadataChange::AddProtectedPort {
            entry: "aa:bb:cc:dd:ee:ff:7".into(),
            site: DEFAULT_SITE.into(),
        }),
        second.update_profile_metadata(MetadataChange::AddProtectedPort {
            entry: "bb:cc:dd:ee:ff:00:8".into(),
            site: DEFAULT_SITE.into(),
        }),
    );
    a.unwrap();
    b.unwrap();
    let mut entries = store.saved("home").unwrap().protected_ports;
    entries.sort();
    assert_eq!(entries, ["aa:bb:cc:dd:ee:ff:7", "bb:cc:dd:ee:ff:00:8"]);
    first
        .update_profile_metadata(MetadataChange::AddProtectedPort {
            entry: "aa:bb:cc:dd:ee:ff:7".into(),
            site: DEFAULT_SITE.into(),
        })
        .await
        .unwrap();
    assert_eq!(store.saved("home").unwrap().protected_ports.len(), 2);
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn metadata_writer_refuses_replaced_account_pending_rotation_or_site() {
    for replacement in ["account", "pending", "site"] {
        let server = MockServer::start(false, Duration::ZERO);
        let store = RecordingStore::default();
        let dir = TestDir::new();
        seed(&store, "home", fresh());
        let source = source(&server, store.clone(), "home", &dir, None)
            .await
            .unwrap();
        source.profile_metadata().await.unwrap();
        let mut changed = fresh();
        let expected = match replacement {
            "account" => {
                changed.username = "another-user".into();
                ErrorKind::Auth
            }
            "pending" => {
                changed.refresh_pending = true;
                ErrorKind::Auth
            }
            _ => {
                changed.default_site = Some("11111111-2222-3333-4444-555555555555".into());
                ErrorKind::Usage
            }
        };
        store.save("home", &changed).unwrap();
        let saves = store.save_log().len();
        let error = source
            .update_profile_metadata(MetadataChange::AddProtectedPort {
                entry: "aa:bb:cc:dd:ee:ff:7".into(),
                site: DEFAULT_SITE.into(),
            })
            .await
            .unwrap_err();
        assert_eq!(error.kind, expected, "{replacement}");
        assert_eq!(store.save_log().len(), saves);
        assert!(store.saved("home").unwrap().protected_ports.is_empty());
        assert!(server.requests().is_empty());
    }
}

#[tokio::test]
async fn metadata_write_failure_does_not_publish_an_uncommitted_snapshot() {
    let server = MockServer::start(false, Duration::ZERO);
    let store = RecordingStore::default();
    let dir = TestDir::new();
    seed(&store, "home", fresh());
    let source = source(&server, store.clone(), "home", &dir, None)
        .await
        .unwrap();
    let before = source.profile_metadata().await.unwrap();
    store.fail_next_save();
    let target = "11111111-2222-3333-4444-555555555555";
    assert_eq!(
        source
            .update_profile_metadata(MetadataChange::DefaultSite(target.into()))
            .await
            .unwrap_err()
            .kind,
        ErrorKind::General
    );
    assert_eq!(source.profile_metadata().await.unwrap(), before);
    assert_eq!(
        store.saved("home").unwrap().default_site,
        before.default_site
    );
    source
        .update_profile_metadata(MetadataChange::DefaultSite(target.into()))
        .await
        .unwrap();
    assert_eq!(
        source
            .profile_metadata()
            .await
            .unwrap()
            .default_site
            .as_deref(),
        Some(target)
    );
}

#[tokio::test]
async fn metadata_writer_finishes_retained_rotation_without_replaying_refresh() {
    let server = MockServer::start(false, Duration::ZERO);
    let store = RecordingStore::default();
    let dir = TestDir::new();
    seed(&store, "home", expired_credential());
    let source = source(&server, store.clone(), "home", &dir, None)
        .await
        .unwrap();
    store.fail_new_refresh_save_once();
    assert_eq!(source.token().await.unwrap_err().kind, ErrorKind::General);
    let result = tokio::time::timeout(
        Duration::from_secs(3),
        source.update_profile_metadata(MetadataChange::DefaultSite(DEFAULT_SITE.into())),
    )
    .await
    .expect("retained lock must not deadlock metadata writer")
    .unwrap();
    assert_eq!(result.default_site.as_deref(), Some(DEFAULT_SITE));
    let saved = store.saved("home").unwrap();
    assert_eq!(saved.refresh.expose_secret(), NEW_REFRESH);
    assert!(!saved.refresh_pending);
    assert_eq!(refresh_posts(&server).len(), 1);
}

#[tokio::test]
async fn invalid_metadata_changes_stop_before_expired_token_or_store_write() {
    let server = MockServer::start(false, Duration::ZERO);
    let store = RecordingStore::default();
    let dir = TestDir::new();
    seed(&store, "home", expired_credential());
    let source = source(&server, store.clone(), "home", &dir, None)
        .await
        .unwrap();
    let saves = store.save_log().len();
    for change in [
        MetadataChange::DefaultSite("bad-site".into()),
        MetadataChange::AddProtectedPort {
            entry: "Switch:0".into(),
            site: DEFAULT_SITE.into(),
        },
    ] {
        assert_eq!(
            source
                .update_profile_metadata(change)
                .await
                .unwrap_err()
                .kind,
            ErrorKind::Usage
        );
    }
    assert_eq!(store.save_log().len(), saves);
    assert!(server.requests().is_empty());
}
