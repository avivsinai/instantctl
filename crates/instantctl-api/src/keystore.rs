use std::{collections::HashMap, sync::Mutex, time::SystemTime};

#[cfg(any(target_os = "macos", test))]
use std::time::{Duration, UNIX_EPOCH};

use crate::{Error, ErrorKind, secret::SecretString};

#[cfg(target_os = "macos")]
// Keep the credential namespace stable across executable renames.
const KEYCHAIN_SERVICE: &str = "hpe-network";

#[derive(Clone, Debug)]
pub struct StoredCredential {
    pub username: String,
    pub refresh: SecretString,
    pub access: SecretString,
    pub access_expiry: SystemTime,
    pub default_site: Option<String>,
    pub protected_ports: Vec<String>,
    /// True means there is no safe refresh token to use until new login or recovery.
    pub refresh_pending: bool,
}

impl StoredCredential {
    pub fn new(username: impl Into<String>, tokens: crate::Tokens) -> Self {
        Self {
            username: username.into(),
            refresh: tokens.refresh,
            access: tokens.access,
            access_expiry: tokens.access_expiry,
            default_site: None,
            protected_ports: Vec::new(),
            refresh_pending: false,
        }
    }

    pub fn tokens(&self) -> crate::Tokens {
        crate::Tokens {
            access: self.access.clone(),
            refresh: self.refresh.clone(),
            access_expiry: self.access_expiry,
        }
    }
}

pub trait CredentialStore: Send + Sync {
    fn load(&self, profile: &str) -> Result<Option<StoredCredential>, Error>;
    /// Atomically replace the whole profile entry. Production stores must commit
    /// durably before returning success, including the refresh-pending flag.
    fn save(&self, profile: &str, credential: &StoredCredential) -> Result<(), Error>;
    fn delete(&self, profile: &str) -> Result<(), Error>;
}

impl<T: CredentialStore + ?Sized> CredentialStore for std::sync::Arc<T> {
    fn load(&self, profile: &str) -> Result<Option<StoredCredential>, Error> {
        (**self).load(profile)
    }

    fn save(&self, profile: &str, credential: &StoredCredential) -> Result<(), Error> {
        (**self).save(profile, credential)
    }

    fn delete(&self, profile: &str) -> Result<(), Error> {
        (**self).delete(profile)
    }
}

#[derive(Default)]
pub struct MemoryStore {
    credentials: Mutex<HashMap<String, StoredCredential>>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl CredentialStore for MemoryStore {
    fn load(&self, profile: &str) -> Result<Option<StoredCredential>, Error> {
        validate_profile(profile)?;
        self.credentials
            .lock()
            .map_err(|_| store_unavailable())
            .map(|credentials| credentials.get(profile).cloned())
    }

    fn save(&self, profile: &str, credential: &StoredCredential) -> Result<(), Error> {
        validate_profile(profile)?;
        self.credentials
            .lock()
            .map_err(|_| store_unavailable())?
            .insert(profile.to_owned(), credential.clone());
        Ok(())
    }

    fn delete(&self, profile: &str) -> Result<(), Error> {
        validate_profile(profile)?;
        self.credentials
            .lock()
            .map_err(|_| store_unavailable())?
            .remove(profile);
        Ok(())
    }
}

#[cfg(target_os = "macos")]
pub struct KeychainStore {
    store: std::sync::Arc<apple_native_keyring_store::keychain::Store>,
}

#[cfg(not(target_os = "macos"))]
pub struct KeychainStore;

impl KeychainStore {
    #[cfg(target_os = "macos")]
    pub fn new() -> Result<Self, Error> {
        use apple_native_keyring_store::keychain::Store;

        let store = Store::new().map_err(|_| keychain_unavailable())?;
        Ok(Self { store })
    }

    #[cfg(not(target_os = "macos"))]
    pub fn new() -> Result<Self, Error> {
        Err(keychain_unsupported())
    }
}

#[cfg(target_os = "macos")]
impl CredentialStore for KeychainStore {
    fn load(&self, profile: &str) -> Result<Option<StoredCredential>, Error> {
        use keyring_core::api::CredentialStoreApi;

        validate_profile(profile)?;
        let entry = self
            .store
            .build(KEYCHAIN_SERVICE, profile, None)
            .map_err(|_| keychain_read_failed())?;
        let value = match entry.get_password() {
            Ok(value) => value,
            Err(keyring_core::Error::NoEntry) => return Ok(None),
            Err(_) => return Err(keychain_read_failed()),
        };
        decode_credential(&value).map(Some)
    }

    fn save(&self, profile: &str, credential: &StoredCredential) -> Result<(), Error> {
        use keyring_core::api::CredentialStoreApi;

        validate_profile(profile)?;
        let entry = self
            .store
            .build(KEYCHAIN_SERVICE, profile, None)
            .map_err(|_| keychain_write_failed())?;
        let value = encode_credential(credential)?;
        entry
            .set_password(&value)
            .map_err(|_| keychain_write_failed())
    }

    fn delete(&self, profile: &str) -> Result<(), Error> {
        use keyring_core::api::CredentialStoreApi;

        validate_profile(profile)?;
        let entry = self
            .store
            .build(KEYCHAIN_SERVICE, profile, None)
            .map_err(|_| keychain_delete_failed())?;
        match entry.delete_credential() {
            Ok(()) | Err(keyring_core::Error::NoEntry) => Ok(()),
            Err(_) => Err(keychain_delete_failed()),
        }
    }
}

#[cfg(not(target_os = "macos"))]
impl CredentialStore for KeychainStore {
    fn load(&self, _profile: &str) -> Result<Option<StoredCredential>, Error> {
        Err(keychain_unsupported())
    }

    fn save(&self, _profile: &str, _credential: &StoredCredential) -> Result<(), Error> {
        Err(keychain_unsupported())
    }

    fn delete(&self, _profile: &str) -> Result<(), Error> {
        Err(keychain_unsupported())
    }
}

fn validate_profile(profile: &str) -> Result<(), Error> {
    if profile.is_empty() || profile.bytes().any(|byte| byte.is_ascii_control()) {
        return Err(Error::new(ErrorKind::Config, "invalid credential profile"));
    }
    Ok(())
}

fn store_unavailable() -> Error {
    Error::new(ErrorKind::General, "credential store is unavailable")
}

#[cfg(target_os = "macos")]
fn keychain_unavailable() -> Error {
    Error::new(ErrorKind::Config, "could not initialize macOS Keychain")
}

#[cfg(not(target_os = "macos"))]
fn keychain_unsupported() -> Error {
    Error::new(
        ErrorKind::Config,
        "macOS Keychain is not available on this platform",
    )
}

#[cfg(target_os = "macos")]
fn keychain_read_failed() -> Error {
    Error::new(
        ErrorKind::Config,
        "could not read credentials from macOS Keychain",
    )
}

#[cfg(target_os = "macos")]
fn keychain_write_failed() -> Error {
    Error::new(
        ErrorKind::Config,
        "could not save credentials to macOS Keychain",
    )
}

#[cfg(target_os = "macos")]
fn keychain_delete_failed() -> Error {
    Error::new(
        ErrorKind::Config,
        "could not delete credentials from macOS Keychain",
    )
}

#[cfg(any(target_os = "macos", test))]
fn stored_value_invalid() -> Error {
    Error::new(ErrorKind::Config, "saved credentials are invalid")
}

#[cfg(any(target_os = "macos", test))]
fn encode_credential(credential: &StoredCredential) -> Result<String, Error> {
    if let Some(site) = &credential.default_site {
        crate::validate_site_id(site).map_err(|_| stored_value_invalid())?;
    }
    crate::ports::validate_protected_ports(&credential.protected_ports)
        .map_err(|_| stored_value_invalid())?;
    let access_expiry = credential
        .access_expiry
        .duration_since(UNIX_EPOCH)
        .map_err(|_| stored_value_invalid())?
        .as_secs();
    serde_json::to_string(&serde_json::json!({
        "username": credential.username,
        "refresh": credential.refresh.expose_secret(),
        "access": credential.access.expose_secret(),
        "access_expiry": access_expiry,
        "default_site": credential.default_site,
        "protected_ports": credential.protected_ports,
        "refresh_pending": credential.refresh_pending,
    }))
    .map_err(|_| stored_value_invalid())
}

#[cfg(any(target_os = "macos", test))]
fn decode_credential(encoded: &str) -> Result<StoredCredential, Error> {
    let value: serde_json::Value =
        serde_json::from_str(encoded).map_err(|_| stored_value_invalid())?;
    let username = value
        .get("username")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(stored_value_invalid)?;
    let refresh = value
        .get("refresh")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(stored_value_invalid)?;
    let access = value
        .get("access")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(stored_value_invalid)?;
    let access_expiry = value
        .get("access_expiry")
        .and_then(serde_json::Value::as_u64)
        .and_then(|seconds| UNIX_EPOCH.checked_add(Duration::from_secs(seconds)))
        .ok_or_else(stored_value_invalid)?;
    let refresh_pending = value
        .get("refresh_pending")
        .and_then(serde_json::Value::as_bool)
        .ok_or_else(stored_value_invalid)?;
    let default_site = match value.get("default_site") {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::String(site)) => {
            crate::validate_site_id(site).map_err(|_| stored_value_invalid())?;
            Some(site.clone())
        }
        Some(_) => return Err(stored_value_invalid()),
    };
    let protected_ports = match value.get("protected_ports") {
        None => Vec::new(),
        Some(serde_json::Value::Array(values)) => values
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .map(str::to_owned)
                    .ok_or_else(stored_value_invalid)
            })
            .collect::<Result<Vec<_>, _>>()?,
        Some(_) => return Err(stored_value_invalid()),
    };
    crate::ports::validate_protected_ports(&protected_ports).map_err(|_| stored_value_invalid())?;
    Ok(StoredCredential {
        username: username.to_owned(),
        refresh: SecretString::new(refresh),
        access: SecretString::new(access),
        access_expiry,
        default_site,
        protected_ports,
        refresh_pending,
    })
}

#[cfg(test)]
mod tests {
    use super::{
        CredentialStore, MemoryStore, StoredCredential, decode_credential, encode_credential,
    };
    use crate::{ErrorKind, secret::SecretString};
    use std::time::{Duration, UNIX_EPOCH};

    fn credential(username: &str, refresh: &str) -> StoredCredential {
        StoredCredential {
            username: username.to_owned(),
            refresh: SecretString::new(refresh),
            access: SecretString::new(format!("{username}-access-token-sentinel")),
            access_expiry: UNIX_EPOCH + Duration::from_secs(2_000_000_000),
            default_site: None,
            protected_ports: Vec::new(),
            refresh_pending: false,
        }
    }

    #[test]
    fn memory_store_keeps_profiles_isolated() {
        let store = MemoryStore::new();
        let home = credential("home-user", "home-refresh");
        let lab = credential("lab-user", "lab-refresh");

        store.save("home", &home).expect("save home profile");
        store.save("lab", &lab).expect("save lab profile");

        assert_eq!(
            store
                .load("home")
                .expect("load home profile")
                .expect("home credentials")
                .username,
            "home-user"
        );
        assert_eq!(
            store
                .load("lab")
                .expect("load lab profile")
                .expect("lab credentials")
                .username,
            "lab-user"
        );
        assert_eq!(
            store
                .load("home")
                .expect("reload home profile")
                .expect("home credentials")
                .access
                .expose_secret(),
            "home-user-access-token-sentinel"
        );
        assert_eq!(
            store
                .load("lab")
                .expect("reload lab profile")
                .expect("lab credentials")
                .access
                .expose_secret(),
            "lab-user-access-token-sentinel"
        );
    }

    #[test]
    fn saving_rotated_credentials_replaces_the_old_value() {
        let store = MemoryStore::new();
        store
            .save("home", &credential("user", "old-refresh"))
            .expect("save initial credentials");
        store
            .save("home", &credential("user", "new-refresh"))
            .expect("save rotated credentials");

        let loaded = store
            .load("home")
            .expect("load credentials")
            .expect("credentials exist");
        assert_eq!(loaded.refresh.expose_secret(), "new-refresh");
        assert_eq!(loaded.access.expose_secret(), "user-access-token-sentinel");
        assert_eq!(
            loaded.access_expiry,
            UNIX_EPOCH + Duration::from_secs(2_000_000_000)
        );
        assert_ne!(loaded.refresh.expose_secret(), "old-refresh");
    }

    #[test]
    fn deleting_an_absent_profile_is_idempotent() {
        let store = MemoryStore::new();

        store.delete("missing").expect("first delete");
        store.delete("missing").expect("repeated delete");
        assert!(
            store
                .load("missing")
                .expect("load absent profile")
                .is_none()
        );
    }

    #[test]
    fn deleting_a_profile_removes_only_its_credentials() {
        let store = MemoryStore::new();
        store
            .save("home", &credential("home-user", "home-refresh"))
            .expect("save home profile");
        store
            .save("lab", &credential("lab-user", "lab-refresh"))
            .expect("save lab profile");

        store.delete("home").expect("delete home profile");

        assert!(store.load("home").expect("load deleted profile").is_none());
        assert_eq!(
            store
                .load("lab")
                .expect("load retained profile")
                .expect("lab credentials remain")
                .username,
            "lab-user"
        );
    }

    #[test]
    fn stored_credential_debug_redacts_both_tokens() {
        let value = credential("user", "refresh-secret-sentinel");

        let debug = format!("{value:?}");
        assert!(!debug.contains("refresh-secret-sentinel"));
        assert!(!debug.contains("user-access-token-sentinel"));
    }

    #[test]
    fn keychain_json_round_trip_preserves_tokens_site_and_integer_expiry() {
        let mut original = StoredCredential::new(
            "user",
            credential("user", "refresh-secret-sentinel").tokens(),
        );
        original.default_site = Some("123e4567-e89b-12d3-a456-426614174000".to_owned());
        original.protected_ports = vec![
            "aa:bb:cc:dd:ee:ff:14".to_owned(),
            "aa:bb:cc:dd:ee:ff:16".to_owned(),
        ];
        let encoded = encode_credential(&original).expect("encode stored credential");
        let value: serde_json::Value = serde_json::from_str(&encoded).expect("valid stored JSON");
        assert!(value["access_expiry"].is_u64());
        assert_eq!(
            value["default_site"],
            "123e4567-e89b-12d3-a456-426614174000"
        );

        let decoded = decode_credential(&encoded).expect("decode stored credential");
        assert_eq!(decoded.username, original.username);
        assert_eq!(
            decoded.refresh.expose_secret(),
            original.refresh.expose_secret()
        );
        assert_eq!(
            decoded.access.expose_secret(),
            original.access.expose_secret()
        );
        assert_eq!(decoded.access_expiry, original.access_expiry);
        assert_eq!(decoded.default_site, original.default_site);
        assert_eq!(decoded.protected_ports, original.protected_ports);

        let tokens = decoded.tokens();
        assert_eq!(
            tokens.refresh.expose_secret(),
            original.refresh.expose_secret()
        );
        assert_eq!(
            tokens.access.expose_secret(),
            original.access.expose_secret()
        );
        assert_eq!(tokens.access_expiry, original.access_expiry);
        assert!(!decoded.refresh_pending);

        let mut pending = original.clone();
        pending.refresh_pending = true;
        let pending_debug = format!("{pending:?}");
        assert!(!pending_debug.contains("refresh-secret-sentinel"));
        assert!(!pending_debug.contains("user-access-token-sentinel"));
        let encoded_pending = encode_credential(&pending).expect("encode pending credential");
        let decoded_pending =
            decode_credential(&encoded_pending).expect("decode pending credential");
        assert!(decoded_pending.refresh_pending);
        assert_eq!(
            decoded_pending.tokens().refresh.expose_secret(),
            original.refresh.expose_secret()
        );
        assert_eq!(decoded_pending.default_site, original.default_site);
        assert_eq!(decoded_pending.protected_ports, original.protected_ports);
    }

    #[test]
    fn keychain_json_without_a_default_site_remains_readable() {
        for encoded in [
            r#"{"username":"user","refresh":"refresh-sentinel","access":"access-sentinel","access_expiry":2000000000,"refresh_pending":false}"#,
            r#"{"username":"user","refresh":"refresh-sentinel","access":"access-sentinel","access_expiry":2000000000,"default_site":null,"refresh_pending":false}"#,
        ] {
            let decoded = decode_credential(encoded).expect("legacy profile remains readable");
            assert_eq!(decoded.default_site, None);
            assert!(decoded.protected_ports.is_empty());
        }
    }

    #[test]
    fn protected_port_metadata_requires_a_string_list_and_legacy_value_defaults_empty() {
        for encoded in [
            r#"{"username":"user","refresh":"refresh-sentinel","access":"access-sentinel","access_expiry":2000000000,"refresh_pending":false}"#,
            r#"{"username":"user","refresh":"refresh-sentinel","access":"access-sentinel","access_expiry":2000000000,"protected_ports":[],"refresh_pending":false}"#,
        ] {
            let decoded = decode_credential(encoded).expect("missing or empty metadata is valid");
            assert!(decoded.protected_ports.is_empty());
        }

        for encoded in [
            r#"{"username":"user","refresh":"refresh-sentinel","access":"access-sentinel","access_expiry":2000000000,"protected_ports":"14","refresh_pending":false}"#,
            r#"{"username":"user","refresh":"refresh-sentinel","access":"access-sentinel","access_expiry":2000000000,"protected_ports":[14],"refresh_pending":false}"#,
            r#"{"username":"user","refresh":"refresh-sentinel","access":"access-sentinel","access_expiry":2000000000,"protected_ports":["no-faceplate-selector"],"refresh_pending":false}"#,
        ] {
            let error = decode_credential(encoded).expect_err("malformed metadata must fail");
            assert_eq!(error.kind, ErrorKind::Config);
            assert_eq!(error.message, "saved credentials are invalid");
        }

        let mut invalid = credential("user", "refresh-sentinel");
        invalid.protected_ports = vec!["no-faceplate-selector".into()];
        let error = encode_credential(&invalid).expect_err("invalid selector must not be saved");
        assert_eq!(error.kind, ErrorKind::Config);
        assert_eq!(error.message, "saved credentials are invalid");
    }

    #[test]
    fn invalid_default_site_metadata_is_rejected_on_read_and_write() {
        let encoded = r#"{"username":"user","refresh":"refresh-sentinel","access":"access-sentinel","access_expiry":2000000000,"default_site":"../escape","refresh_pending":false}"#;
        let error = decode_credential(encoded).expect_err("invalid stored site must fail closed");
        assert_eq!(error.message, "saved credentials are invalid");

        let mut credential = credential("user", "refresh-sentinel");
        credential.default_site = Some("../escape".to_owned());
        let error = encode_credential(&credential).expect_err("invalid site must not be saved");
        assert_eq!(error.message, "saved credentials are invalid");
    }

    #[test]
    fn invalid_keychain_json_and_expiry_use_fixed_redacted_errors() {
        for encoded in [
            "not-json-refresh-secret-sentinel",
            r#"{"username":"u","refresh":"refresh-secret-sentinel","access":"access-token-sentinel","access_expiry":-1,"refresh_pending":false}"#,
            r#"{"username":"u","refresh":"refresh-secret-sentinel","access":"access-token-sentinel","access_expiry":1.5,"refresh_pending":false}"#,
            r#"{"username":"u","refresh":"refresh-secret-sentinel","access":"access-token-sentinel","access_expiry":18446744073709551615,"refresh_pending":false}"#,
            r#"{"username":"u","refresh":"refresh-secret-sentinel","access":"access-token-sentinel","access_expiry":1}"#,
        ] {
            let error = decode_credential(encoded).expect_err("invalid credential must fail");
            assert_eq!(error.message, "saved credentials are invalid");
            assert!(!error.to_string().contains("refresh-secret-sentinel"));
            assert!(!error.to_string().contains("access-token-sentinel"));
            assert!(!format!("{error:?}").contains("refresh-secret-sentinel"));
            assert!(!format!("{error:?}").contains("access-token-sentinel"));
        }
        let before_epoch = StoredCredential {
            access_expiry: UNIX_EPOCH - Duration::from_secs(1),
            ..credential("user", "refresh-secret-sentinel")
        };
        let error = encode_credential(&before_epoch).expect_err("pre-epoch expiry must fail");
        assert_eq!(error.message, "saved credentials are invalid");
        assert!(!error.to_string().contains("refresh-secret-sentinel"));
        assert!(!format!("{error:?}").contains("refresh-secret-sentinel"));
    }

    #[test]
    fn stores_reject_invalid_profiles() {
        let store = MemoryStore::new();

        for profile in ["", "has\nline-break", "has\0null"] {
            assert!(store.load(profile).is_err(), "load accepted {profile:?}");
            assert!(
                store.save(profile, &credential("user", "refresh")).is_err(),
                "save accepted {profile:?}"
            );
            assert!(
                store.delete(profile).is_err(),
                "delete accepted {profile:?}"
            );
        }

        #[cfg(not(target_os = "macos"))]
        assert!(super::KeychainStore::new().is_err());
    }
}
