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
            "instantctl-policy-cli-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(root.join("home")).expect("create isolated HOME");
        fs::create_dir_all(root.join("cache")).expect("create isolated XDG cache");
        fs::create_dir_all(root.join("tmp")).expect("create isolated TMPDIR");
        Self { root }
    }

    fn command(&self, arguments: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_instantctl"))
            .args(arguments)
            .env("HOME", self.root.join("home"))
            .env("XDG_CACHE_HOME", self.root.join("cache"))
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
fn invalid_policy_selector_fails_before_credentials_are_loaded() {
    let sandbox = Sandbox::new();
    let output = sandbox.command(&["--format", "json", "--site", SITE, "policy", "show", ""]);
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let error = error(&output);
    assert_eq!(error["kind"], "usage");
    assert!(error["message"].as_str().unwrap().contains("selector"));
}

#[test]
fn stdin_apply_requires_yes_before_credentials_or_network_access() {
    let sandbox = Sandbox::new();
    let output = sandbox.command(&[
        "--format",
        "json",
        "--token-stdin",
        "--site",
        SITE,
        "policy",
        "app-visibility",
        "set",
        "true",
        "--apply",
    ]);
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert_eq!(error(&output)["kind"], "confirmation_required");
}

#[test]
fn policy_help_exposes_reads_and_visibility_without_creation() {
    let sandbox = Sandbox::new();
    let output = sandbox.command(&["policy", "--help"]);
    assert_eq!(output.status.code(), Some(0));
    let help = String::from_utf8(output.stdout).expect("help is UTF-8");
    assert!(help.contains("list"));
    assert!(help.contains("show"));
    assert!(help.contains("app-visibility"));
    assert!(!help.contains("create"));
}
