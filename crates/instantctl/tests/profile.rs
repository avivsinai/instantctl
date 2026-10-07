use serde_json::Value;
use std::{
    fs,
    path::PathBuf,
    process::{Command, Output, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};

struct TestHome(PathBuf);

impl TestHome {
    fn new() -> Self {
        static NEXT_HOME_ID: AtomicU64 = AtomicU64::new(0);
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target/profile-test-homes")
            .join(format!(
                "{}-{}",
                std::process::id(),
                NEXT_HOME_ID.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir_all(&path).expect("create isolated profile test home");
        Self(path)
    }

    fn cache(&self) -> PathBuf {
        self.0.join("xdg-cache")
    }

    fn config(&self) -> PathBuf {
        self.0.join("xdg-config")
    }
}

impl Drop for TestHome {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn command(home: &TestHome, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_instantctl"));
    command
        .env("HOME", &home.0)
        .env("XDG_CACHE_HOME", home.cache())
        .env("XDG_CONFIG_HOME", home.config())
        .args(args);
    command
}

fn assert_usage(output: Output, message: &str) {
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let error: Value = serde_json::from_slice(&output.stderr).expect("JSON error output");
    assert_eq!(error["kind"], "usage");
    assert!(error["message"].as_str().unwrap().contains(message));
}

fn assert_no_profile_lock(home: &TestHome) {
    assert!(
        !home.cache().join("instantctl").exists()
            && !home.0.join("Library/Caches/instantctl").exists(),
        "preflight must not load or write saved profile state"
    );
}

#[test]
fn invalid_profile_input_is_usage_before_static_environment_credentials() {
    for (args, expected) in [
        (
            vec![
                "--format",
                "json",
                "profile",
                "default-site",
                "set",
                "not-a-uuid",
            ],
            "UUID",
        ),
        (
            vec![
                "--format",
                "json",
                "profile",
                "protected-ports",
                "add",
                "aa:bb:cc:dd:ee:gg:7",
            ],
            "valid MAC or exact switch name",
        ),
    ] {
        let home = TestHome::new();
        let output = command(&home, &args)
            .env("HPE_INSTANT_ON_TOKEN", "profile-test-static-token")
            .output()
            .expect("run CLI");
        assert_usage(output, expected);
        assert_no_profile_lock(&home);
    }
}

#[test]
fn static_environment_token_refuses_profile_metadata_before_store_access() {
    let home = TestHome::new();
    let output = command(&home, &["--format", "json", "profile", "show"])
        .env("HPE_INSTANT_ON_TOKEN", "profile-test-static-token")
        .output()
        .expect("run CLI");
    assert_usage(
        output,
        "profile metadata commands require saved-profile credentials",
    );
    assert_no_profile_lock(&home);
}

#[test]
fn stdin_token_refusal_returns_before_reading_stdin_or_accessing_store() {
    let home = TestHome::new();
    let mut child = command(
        &home,
        &["--format", "json", "--token-stdin", "profile", "show"],
    )
    .env_remove("HPE_INSTANT_ON_TOKEN")
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .spawn()
    .expect("spawn CLI");
    let stdin = child.stdin.take().expect("keep stdin open");
    let deadline = Instant::now() + Duration::from_secs(3);
    let status = loop {
        if let Some(status) = child.try_wait().expect("poll CLI") {
            break status;
        }
        if Instant::now() >= deadline {
            child.kill().expect("stop a CLI blocked on stdin");
            drop(stdin);
            let _ = child.wait();
            panic!("profile preflight waited for stdin instead of refusing it");
        }
        thread::sleep(Duration::from_millis(10));
    };
    drop(stdin);
    let output = child.wait_with_output().expect("collect CLI output");
    assert_eq!(status, output.status);
    assert_usage(
        output,
        "profile metadata commands require saved-profile credentials",
    );
    assert_no_profile_lock(&home);
}
