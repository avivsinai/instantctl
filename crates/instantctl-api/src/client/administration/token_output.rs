//! Save an explicitly requested support token without printing it.
use crate::{Error, ErrorKind, secret::SecretString};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

pub(super) fn validate_path(path: &Path) -> Result<(), Error> {
    if path.as_os_str().is_empty() || path.file_name().is_none() {
        return Err(usage("support token output must name a new file"));
    }
    match fs::symlink_metadata(path) {
        Ok(_) => {
            return Err(usage(
                "support token output already exists; choose a new file",
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err(io_error()),
    }
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    if !fs::metadata(parent).is_ok_and(|metadata| metadata.is_dir()) {
        return Err(usage(
            "support token output parent must be an existing directory",
        ));
    }
    Ok(())
}

pub(super) struct TokenFile {
    file: File,
    path: PathBuf,
    committed: bool,
    #[cfg(unix)]
    identity: (u64, u64),
}

impl TokenFile {
    pub(super) fn create(path: &Path) -> Result<Self, Error> {
        validate_path(path)?;
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        #[cfg(not(unix))]
        return Err(Error::new(
            ErrorKind::Unsupported,
            "secure support token output requires Unix file permissions",
        ));
        let file = options.open(path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                usage("support token output already exists; choose a new file")
            } else {
                io_error()
            }
        })?;
        #[cfg(unix)]
        let identity = {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            file.set_permissions(fs::Permissions::from_mode(0o600))
                .map_err(|_| io_error())?;
            let metadata = file.metadata().map_err(|_| io_error())?;
            (metadata.dev(), metadata.ino())
        };
        Ok(Self {
            file,
            path: path.to_owned(),
            committed: false,
            #[cfg(unix)]
            identity,
        })
    }

    pub(super) fn write(mut self, token: &SecretString) -> Result<(), Error> {
        if !self.owns_path() {
            return Err(io_error());
        }
        self.file
            .write_all(token.expose_secret().as_bytes())
            .and_then(|()| self.file.sync_all())
            .map_err(|_| io_error())?;
        if !self.owns_path() {
            return Err(io_error());
        }
        self.committed = true;
        Ok(())
    }

    fn owns_path(&self) -> bool {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            fs::symlink_metadata(&self.path).is_ok_and(|metadata| {
                metadata.is_file() && (metadata.dev(), metadata.ino()) == self.identity
            })
        }
        #[cfg(not(unix))]
        {
            false
        }
    }
}

impl Drop for TokenFile {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        // Remove only the exact file this command created, never a replacement.
        #[cfg(unix)]
        {
            if self.owns_path() {
                let _ = fs::remove_file(&self.path);
            }
        }
    }
}

fn usage(message: &str) -> Error {
    Error::new(ErrorKind::Usage, message)
}
fn io_error() -> Error {
    Error::new(ErrorKind::General, "could not save support token output")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    struct Sandbox(PathBuf);
    impl Sandbox {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "hpe-admin-token-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).expect("create token sandbox");
            Self(path)
        }
    }
    impl Drop for Sandbox {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn token_is_saved_in_a_new_private_file_and_existing_content_is_preserved() {
        let sandbox = Sandbox::new();
        let path = sandbox.0.join("support-token");
        TokenFile::create(&path)
            .unwrap()
            .write(&SecretString::new("support-secret-sentinel"))
            .unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "support-secret-sentinel"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        assert_eq!(
            TokenFile::create(&path).err().unwrap().kind,
            ErrorKind::Usage
        );
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "support-secret-sentinel"
        );
    }
    #[test]
    fn abandoned_file_is_removed_but_a_replacement_is_retained() {
        let sandbox = Sandbox::new();
        let path = sandbox.0.join("support-token");
        drop(TokenFile::create(&path).unwrap());
        assert!(!path.exists());
        let pending = TokenFile::create(&path).unwrap();
        fs::rename(&path, sandbox.0.join("original")).unwrap();
        fs::write(&path, "replacement").unwrap();
        assert_eq!(
            pending
                .write(&SecretString::new("must-not-be-written"))
                .unwrap_err()
                .kind,
            ErrorKind::General
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "replacement");
        assert!(fs::read(sandbox.0.join("original")).unwrap().is_empty());
    }
    #[test]
    fn invalid_parent_and_existing_symlink_are_rejected_without_overwrite() {
        let sandbox = Sandbox::new();
        assert_eq!(
            validate_path(&sandbox.0.join("missing/token"))
                .unwrap_err()
                .kind,
            ErrorKind::Usage
        );
        #[cfg(unix)]
        {
            let target = sandbox.0.join("target");
            fs::write(&target, "keep").unwrap();
            let path = sandbox.0.join("token");
            std::os::unix::fs::symlink(&target, &path).unwrap();
            assert_eq!(
                TokenFile::create(&path).err().unwrap().kind,
                ErrorKind::Usage
            );
            assert_eq!(fs::read_to_string(&target).unwrap(), "keep");
        }
    }
}
