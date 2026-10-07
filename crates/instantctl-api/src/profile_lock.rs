//! Advisory locks shared by independent commands. Never remove a lock file: its
//! inode must stay the same for every process using a profile.

use std::{
    env, fmt,
    fs::{DirBuilder, File, OpenOptions},
    path::PathBuf,
};

use crate::{Error, ErrorKind};

#[derive(Clone)]
pub struct ProfileLock {
    path: PathBuf,
}

/// Closing the owned file releases the advisory lock, including on error paths.
pub struct ProfileGuard {
    _file: File,
}

impl fmt::Debug for ProfileLock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProfileLock").finish_non_exhaustive()
    }
}

impl fmt::Debug for ProfileGuard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProfileGuard").finish_non_exhaustive()
    }
}

impl ProfileLock {
    pub fn for_profile(profile: &str) -> Result<Self, Error> {
        if profile.is_empty() || profile.bytes().any(|b| b.is_ascii_control()) {
            return Err(config("invalid credential profile"));
        }
        let home = env::var_os("HOME")
            .filter(|home| !home.is_empty())
            .map(PathBuf::from)
            .ok_or_else(|| config("could not find the user cache directory"))?;
        let cache = if cfg!(target_os = "macos") {
            home.join("Library/Caches")
        } else {
            env::var_os("XDG_CACHE_HOME")
                .filter(|dir| !dir.is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".cache"))
        };
        if !cache.is_absolute() {
            return Err(config("the user cache directory must be absolute"));
        }
        // Preserve all accepted profile names without letting '/' escape the
        // application directory. Percent encoding is injective, including '%'.
        let filename: String = url::form_urlencoded::byte_serialize(profile.as_bytes()).collect();
        // The lock namespace must stay shared with earlier executable names.
        Ok(Self {
            path: cache.join("hpe-network").join(format!("{filename}.lock")),
        })
    }

    /// Lock acquisition blocks an OS thread, never the async executor. Each call
    /// opens an independent descriptor so separate instances also exclude each other.
    pub async fn acquire(&self) -> Result<ProfileGuard, Error> {
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || {
            let parent = path.parent().ok_or_else(lock_unavailable)?;
            let mut directory = DirBuilder::new();
            directory.recursive(true);
            let mut options = OpenOptions::new();
            options.read(true).write(true).create(true).truncate(false);
            #[cfg(unix)]
            {
                use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
                directory.mode(0o700);
                options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
            }
            directory.create(parent).map_err(|_| lock_unavailable())?;
            let file = options.open(path).map_err(|_| lock_unavailable())?;
            if !file.metadata().map_err(|_| lock_unavailable())?.is_file() {
                return Err(lock_unavailable());
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                file.set_permissions(std::fs::Permissions::from_mode(0o600))
                    .map_err(|_| lock_unavailable())?;
            }
            file.lock().map_err(|_| lock_unavailable())?;
            Ok(ProfileGuard { _file: file })
        })
        .await
        .map_err(|_| lock_unavailable())?
    }

    #[cfg(test)]
    pub(crate) fn at(path: PathBuf) -> Self {
        Self { path }
    }
}

fn config(message: &'static str) -> Error {
    Error::new(ErrorKind::Config, message)
}

fn lock_unavailable() -> Error {
    config("could not acquire the profile credential lock")
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestDir(PathBuf);
    impl TestDir {
        fn new() -> Self {
            let mut bytes = [0; 16];
            getrandom::fill(&mut bytes).unwrap();
            let name: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
            let path = env::temp_dir().join(format!("hpe-lock-{name}"));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn profile_names_stay_inside_cache_and_do_not_collide() {
        let home = ProfileLock::for_profile("home").unwrap();
        assert_eq!(home.path.file_name().unwrap(), "home.lock");
        let slash = ProfileLock::for_profile("a/b").unwrap();
        let escaped = ProfileLock::for_profile("a%2Fb").unwrap();
        assert_ne!(slash.path, escaped.path);
        for profile in ["../outside", "a/b", "..", "snow-☃"] {
            let lock = ProfileLock::for_profile(profile).unwrap();
            assert_eq!(lock.path.parent(), home.path.parent());
            assert!(
                !lock
                    .path
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .contains('/')
            );
        }
        for profile in ["", "secret\nprofile"] {
            let error = ProfileLock::for_profile(profile).unwrap_err();
            assert_eq!(error.kind, ErrorKind::Config);
            assert!(!format!("{error:?} {error}").contains("secret"));
        }
    }

    #[cfg(unix)]
    #[test]
    fn lock_excludes_independent_process_and_releases_on_drop() {
        // Run this same test in a fresh process to exercise an independent open,
        // rather than a cloned descriptor which would share its parent's lock.
        if let Some(path) = env::var_os("HPE_NETAPI_TEST_LOCK_PROBE") {
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .open(path)
                .unwrap();
            if env::var("HPE_NETAPI_TEST_LOCK_EXPECT").unwrap() == "blocked" {
                assert!(matches!(
                    file.try_lock(),
                    Err(std::fs::TryLockError::WouldBlock)
                ));
            } else {
                file.try_lock().expect("parent released the lock");
            }
            return;
        }
        let directory = TestDir::new();
        let path = directory.0.join("home.lock");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let guard = runtime
            .block_on(ProfileLock::at(path.clone()).acquire())
            .unwrap();
        let run_child = |expected| {
            let output = std::process::Command::new(env::current_exe().unwrap())
                .args([
                    "--exact",
                    "profile_lock::tests::lock_excludes_independent_process_and_releases_on_drop",
                ])
                .env("HPE_NETAPI_TEST_LOCK_PROBE", &path)
                .env("HPE_NETAPI_TEST_LOCK_EXPECT", expected)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        run_child("blocked");
        drop(guard);
        run_child("free");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlink_lock_is_refused_without_changing_target_and_errors_are_redacted() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let directory = TestDir::new();
        let target = directory.0.join("access-secret-refresh-secret");
        let path = directory.0.join("home.lock");
        std::fs::write(&target, b"untouched").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o644)).unwrap();
        symlink(&target, &path).unwrap();
        let lock = ProfileLock::at(path);
        let error = lock.acquire().await.unwrap_err();
        assert_eq!(error.kind, ErrorKind::Config);
        for secret in ["access-secret", "refresh-secret"] {
            assert!(!format!("{lock:?} {error:?} {error}").contains(secret));
        }
        assert_eq!(std::fs::read(&target).unwrap(), b"untouched");
        assert_eq!(
            std::fs::metadata(target).unwrap().permissions().mode() & 0o777,
            0o644
        );
    }
}
