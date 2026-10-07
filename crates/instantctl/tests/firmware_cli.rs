use serde_json::Value;
use std::{
    env, fs,
    path::PathBuf,
    process::{Command, Output, Stdio},
    sync::atomic::{AtomicU64, Ordering},
};

const SITE: &str = "123e4567-e89b-12d3-a456-426614174000";

struct Sandbox {
    root: PathBuf,
}

impl Sandbox {
    fn new() -> Self {
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        let parent = env::var_os("TMPDIR")
            .map(PathBuf::from)
            .unwrap_or_else(env::temp_dir);
        let root = parent.join(format!(
            "instantctl-firmware-cli-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        for name in ["home", "cache", "config", "tmp"] {
            fs::create_dir_all(root.join(name)).expect("create isolated CLI directory");
        }
        Self { root }
    }

    fn command(&self, arguments: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_instantctl"))
            .args(arguments)
            .env("HOME", self.root.join("home"))
            .env("XDG_CACHE_HOME", self.root.join("cache"))
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("TMPDIR", self.root.join("tmp"))
            .env_remove("HPE_INSTANT_ON_TOKEN")
            .stdin(Stdio::null())
            .output()
            .expect("run isolated instantctl binary")
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn error(output: &Output) -> Value {
    serde_json::from_slice(&output.stderr).expect("stderr should contain JSON error")
}

#[test]
fn invalid_window_time_fails_before_credentials_are_loaded() {
    let sandbox = Sandbox::new();
    let output = sandbox.command(&[
        "--format",
        "json",
        "--site",
        SITE,
        "firmware",
        "window",
        "set",
        "--start-time",
        "25:00",
    ]);
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert_eq!(error(&output)["kind"], "usage");
}

#[test]
fn invalid_schedule_fails_before_credentials_are_loaded() {
    let sandbox = Sandbox::new();
    for at in ["2026-02-30T03:00:00", "2026-11-02T03:00:60"] {
        let output = sandbox.command(&[
            "--format", "json", "--site", SITE, "firmware", "schedule", at,
        ]);
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        assert_eq!(error(&output)["kind"], "usage");
    }
}

#[test]
fn stdin_update_now_requires_yes_before_credentials_or_network_access() {
    let sandbox = Sandbox::new();
    let output = sandbox.command(&[
        "--format",
        "json",
        "--token-stdin",
        "--site",
        SITE,
        "firmware",
        "update-now",
        "--apply",
    ]);
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert_eq!(error(&output)["kind"], "confirmation_required");
}

#[test]
fn firmware_help_lists_only_the_requested_operations() {
    let sandbox = Sandbox::new();
    let output = sandbox.command(&["firmware", "--help"]);
    assert_eq!(output.status.code(), Some(0));
    let help = String::from_utf8(output.stdout).expect("help is UTF-8");
    assert!(help.contains("window"));
    assert!(help.contains("update-now"));
    assert!(help.contains("schedule"));
    assert!(!help.contains("install"));
}
