//! Access caching and serialized, durable token rotation across CLI processes.

use std::{
    fmt,
    sync::Arc,
    time::{Duration, SystemTime},
};

use tokio::{sync::Mutex, task::JoinHandle};

use crate::{
    CredentialStore, Error, ErrorKind, ProfileGuard, ProfileLock, SsoClient, StoredCredential,
    TokenSource, Tokens, secret::SecretString,
};

const DEFAULT_SKEW: Duration = Duration::from_secs(60);

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProfileMetadata {
    pub default_site: Option<String>,
    pub protected_ports: Vec<String>,
}

impl ProfileMetadata {
    fn from_credential(credential: &StoredCredential) -> Self {
        Self {
            default_site: credential.default_site.clone(),
            protected_ports: credential.protected_ports.clone(),
        }
    }
}

pub(crate) enum MetadataChange {
    DefaultSite(String),
    AddProtectedPort {
        entry: String,
        site: String,
    },
    RemoveProtectedPort {
        entry: String,
        site: Option<String>,
        legacy_entry: Option<String>,
    },
}

impl MetadataChange {
    fn validate(&self) -> Result<(), Error> {
        use crate::{client::profile::validate_default_site, ports::validate_protected_port_entry};
        match self {
            Self::DefaultSite(site) => validate_default_site(site),
            Self::AddProtectedPort { entry, site } => {
                validate_protected_port_entry(entry)?;
                validate_default_site(site)
            }
            Self::RemoveProtectedPort {
                entry,
                site,
                legacy_entry,
            } => {
                validate_protected_port_entry(entry)?;
                if let Some(site) = site {
                    validate_default_site(site)?;
                }
                if let Some(entry) = legacy_entry {
                    validate_protected_port_entry(entry)?;
                }
                Ok(())
            }
        }
    }
}

struct Cached {
    credential: StoredCredential,
    // A spent refresh token must remain inaccessible to other processes until
    // its replacement is durable. Keep the OS lock if saving rotation failed.
    pending_lock: Option<ProfileGuard>,
}

type RefreshOutcome = Result<(Cached, Option<Error>), Error>;

#[derive(Default)]
struct State {
    cached: Option<Cached>,
    in_flight: Option<JoinHandle<RefreshOutcome>>,
}

/// The profile lock covers reload, expiry check, refresh and save. Independent
/// instances and commands share the same lock and the same stored access token.
pub struct RefreshingTokenSource<S> {
    sso: Arc<SsoClient>,
    store: Arc<S>,
    profile: String,
    profile_lock: ProfileLock,
    skew: Duration,
    state: Mutex<State>,
}

impl<S> fmt::Debug for RefreshingTokenSource<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RefreshingTokenSource")
            .finish_non_exhaustive()
    }
}

impl<S: CredentialStore> RefreshingTokenSource<S> {
    pub fn new(sso: SsoClient, store: S, profile: impl Into<String>) -> Result<Self, Error> {
        let profile = profile.into();
        let lock = ProfileLock::for_profile(&profile)?;
        Ok(Self::build(sso, store, profile, lock))
    }

    fn build(sso: SsoClient, store: S, profile: String, profile_lock: ProfileLock) -> Self {
        Self {
            sso: Arc::new(sso),
            store: Arc::new(store),
            profile,
            profile_lock,
            skew: DEFAULT_SKEW,
            state: Mutex::new(State::default()),
        }
    }

    /// Save a fresh login under the same profile lock used by refresh workers.
    pub async fn with_login(
        sso: SsoClient,
        store: S,
        profile: impl Into<String>,
        username: impl Into<String>,
        tokens: Tokens,
    ) -> Result<Self, Error> {
        let source = Self::new(sso, store, profile)?;
        source.install_login(username.into(), tokens).await?;
        Ok(source)
    }

    async fn install_login(&self, username: String, tokens: Tokens) -> Result<(), Error> {
        let _guard = self.profile_lock.acquire().await?;
        let mut credential = StoredCredential::new(username.clone(), tokens);
        if let Some(previous) = self.store.load(&self.profile)?
            && previous.username == username
        {
            credential.default_site = previous.default_site;
            credential.protected_ports = previous.protected_ports;
        }
        self.store.save(&self.profile, &credential)?;
        self.state.lock().await.cached = Some(Cached {
            credential,
            pending_lock: None,
        });
        Ok(())
    }

    pub fn with_skew(mut self, skew: Duration) -> Self {
        self.skew = skew;
        self
    }

    #[cfg(test)]
    pub(crate) async fn for_test(
        sso: SsoClient,
        store: S,
        profile: &str,
        lock_path: std::path::PathBuf,
        login: Option<(String, Tokens)>,
    ) -> Result<Self, Error> {
        let source = Self::build(sso, store, profile.into(), ProfileLock::at(lock_path));
        if let Some((username, tokens)) = login {
            source.install_login(username, tokens).await?;
        }
        Ok(source)
    }
}

impl<S: CredentialStore + 'static> RefreshingTokenSource<S> {
    /// Return metadata saved with the credential that supplied this source's
    /// access token. Loading the token first binds all fields to one cached
    /// credential rather than pairing settings across different logins.
    pub async fn profile_metadata(&self) -> Result<ProfileMetadata, Error> {
        self.token().await?;
        let state = self.state.lock().await;
        state
            .cached
            .as_ref()
            .map(|cached| ProfileMetadata::from_credential(&cached.credential))
            .ok_or_else(no_login)
    }

    /// Return the default site saved with this source's token credential.
    pub async fn default_site(&self) -> Result<Option<String>, Error> {
        self.profile_metadata()
            .await
            .map(|metadata| metadata.default_site)
    }

    pub(crate) async fn update_profile_metadata(
        &self,
        change: MetadataChange,
    ) -> Result<ProfileMetadata, Error> {
        change.validate()?;
        // Serialize with retained refresh workers before acquiring their OS lock.
        // This also finishes any pending rotation save, avoiding a self-deadlock.
        let mut state = self.state.lock().await;
        self.token_with_state(&mut state).await?;
        let username = state
            .cached
            .as_ref()
            .ok_or_else(no_login)?
            .credential
            .username
            .clone();
        let _guard = self.profile_lock.acquire().await?;
        let mut credential = self.store.load(&self.profile)?.ok_or_else(no_login)?;
        if credential.username != username || credential.refresh_pending {
            return Err(Error::new(
                ErrorKind::Auth,
                "the saved login changed or has an unfinished refresh; run instantctl auth login",
            ));
        }
        let expected_site = match &change {
            MetadataChange::AddProtectedPort { site, .. } => Some(site),
            MetadataChange::RemoveProtectedPort { site, .. } => site.as_ref(),
            MetadataChange::DefaultSite(_) => None,
        };
        if let Some(site) = expected_site
            && !credential
                .default_site
                .as_deref()
                .is_some_and(|current| current.eq_ignore_ascii_case(site))
        {
            return Err(Error::new(
                ErrorKind::Usage,
                "profile default site changed; run the command again",
            ));
        }
        let existing = crate::ports::parse_protected_ports(&credential.protected_ports)?;
        match change {
            MetadataChange::DefaultSite(site) => credential.default_site = Some(site),
            MetadataChange::AddProtectedPort { entry, .. } => {
                let added = crate::ports::parse_protected_port_entry(&entry)?;
                if !existing.iter().any(|port| same_port(port, &added)) {
                    credential.protected_ports.push(entry);
                }
            }
            MetadataChange::RemoveProtectedPort {
                entry,
                legacy_entry,
                ..
            } => {
                let removed = crate::ports::parse_protected_port_entry(&entry)?;
                let legacy = legacy_entry
                    .as_deref()
                    .map(crate::ports::parse_protected_port_entry)
                    .transpose()?;
                let retained: Vec<_> = credential
                    .protected_ports
                    .iter()
                    .zip(&existing)
                    .filter(|(_, port)| {
                        !same_port(port, &removed)
                            && !legacy.as_ref().is_some_and(|old| same_port(port, old))
                    })
                    .map(|(entry, _)| entry.clone())
                    .collect();
                if retained.len() == credential.protected_ports.len() {
                    return Err(Error::new(
                        ErrorKind::NotFound,
                        "protected port is absent from this profile",
                    ));
                }
                credential.protected_ports = retained;
            }
        }
        // Reloaded tokens and unrelated metadata survive the field-level patch.
        // No await separates durable save from replacing the cached snapshot.
        self.store.save(&self.profile, &credential)?;
        let metadata = ProfileMetadata::from_credential(&credential);
        state.cached = Some(Cached {
            credential,
            pending_lock: None,
        });
        Ok(metadata)
    }
}

fn same_port(left: &crate::ports::ProtectedPort, right: &crate::ports::ProtectedPort) -> bool {
    left.faceplate == right.faceplate
        && if crate::inventory::is_mac_address(&right.device) {
            left.device.eq_ignore_ascii_case(&right.device)
        } else {
            left.device == right.device
        }
}

impl<S: CredentialStore + 'static> RefreshingTokenSource<S> {
    async fn token_with_state(&self, state: &mut State) -> Result<SecretString, Error> {
        if let Some(current) = state.cached.as_mut() {
            if current.pending_lock.is_some() {
                // The lock from the failed save is still held. Retry only that
                // save; never reload or spend the old durable refresh token.
                self.store.save(&self.profile, &current.credential)?;
                current.pending_lock = None;
            }
            if usable(&current.credential, self.skew) {
                return Ok(current.credential.access.clone());
            }
        }
        if state.in_flight.is_none() {
            let observed_refresh = state
                .cached
                .as_ref()
                .map(|cached| cached.credential.refresh.clone());
            let sso = Arc::clone(&self.sso);
            let store = Arc::clone(&self.store);
            let profile = self.profile.clone();
            let profile_lock = self.profile_lock.clone();
            let skew = self.skew;
            let bound_username = state
                .cached
                .as_ref()
                .map(|cached| cached.credential.username.clone());
            // Retain the worker: cancelling a caller cannot drop an in-progress
            // refresh or its profile lock, including a pending rotation save.
            state.in_flight = Some(tokio::spawn(async move {
                let guard = profile_lock.acquire().await?;
                // Always reload AFTER locking. A different command may already
                // have refreshed while this command waited for the lock.
                let mut credential = store.load(&profile)?.ok_or_else(no_login)?;
                if bound_username
                    .as_deref()
                    .is_some_and(|username| username != credential.username)
                {
                    return Err(Error::new(
                        ErrorKind::Auth,
                        "the saved login changed for this profile; run instantctl auth login",
                    ));
                }
                if credential.refresh_pending {
                    return Err(Error::new(
                        ErrorKind::Auth,
                        "saved login has an unfinished refresh; run instantctl auth login",
                    ));
                }
                let rotated_elsewhere = observed_refresh
                    .as_ref()
                    .is_some_and(|old| old.expose_secret() != credential.refresh.expose_secret());
                if usable(&credential, skew)
                    || (rotated_elsewhere && usable(&credential, Duration::ZERO))
                {
                    return Ok((
                        Cached {
                            credential,
                            pending_lock: None,
                        },
                        None,
                    ));
                }
                // A failed final save or process exit must not leave a reusable
                // old token. Commit this intent in the same durable store first.
                credential.refresh_pending = true;
                store.save(&profile, &credential)?;
                let tokens = sso
                    .refresh(&credential.refresh)
                    .await
                    .map_err(Error::from)?;
                let default_site = credential.default_site.clone();
                let protected_ports = credential.protected_ports.clone();
                let mut credential = StoredCredential::new(credential.username, tokens);
                credential.default_site = default_site;
                credential.protected_ports = protected_ports;
                let saved = store.save(&profile, &credential);
                let pending_lock = if saved.is_err() { Some(guard) } else { None };
                Ok((
                    Cached {
                        credential,
                        pending_lock,
                    },
                    saved.err(),
                ))
            }));
        }
        // Await a borrowed handle. A cancelled waiter leaves the same task and
        // any pending lock/rotation in shared state for the next caller to join.
        let outcome = state
            .in_flight
            .as_mut()
            .expect("refresh worker was installed")
            .await;
        state.in_flight = None;
        let (current, save_error) = outcome.map_err(|_| {
            Error::new(
                ErrorKind::Auth,
                "SSO refresh worker failed; run instantctl auth login",
            )
        })??;
        let access = current.credential.access.clone();
        state.cached = Some(current);
        if let Some(error) = save_error {
            return Err(error);
        }
        Ok(access)
    }
}

impl<S: CredentialStore + 'static> TokenSource for RefreshingTokenSource<S> {
    async fn token(&self) -> Result<SecretString, Error> {
        self.token_with_state(&mut *self.state.lock().await).await
    }
}

fn usable(credential: &StoredCredential, skew: Duration) -> bool {
    SystemTime::now()
        .checked_add(skew)
        .is_some_and(|deadline| deadline < credential.access_expiry)
}

fn no_login() -> Error {
    Error::new(
        ErrorKind::Auth,
        "no saved login for this profile; run instantctl auth login",
    )
}

#[cfg(test)]
mod tests;
