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
            "instantctl-admin-cli-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        for directory in ["home", "cache", "config", "tmp"] {
            fs::create_dir_all(root.join(directory))
                .expect("create isolated environment directory");
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
fn invalid_email_role_and_selector_fail_before_credentials() {
    let sandbox = Sandbox::new();
    for (arguments, message) in [
        (
            vec![
                "--format", "json", "--site", SITE, "admin", "add", "invalid",
            ],
            "email",
        ),
        (
            vec![
                "--format",
                "json",
                "--site",
                SITE,
                "admin",
                "add",
                "person@example.test",
                "--role",
                "visitor",
            ],
            "role",
        ),
        (
            vec!["--format", "json", "--site", SITE, "admin", "remove", ""],
            "selector",
        ),
    ] {
        let output = sandbox.command(&arguments);
        assert_eq!(output.status.code(), Some(2), "{arguments:?}");
        assert!(output.stdout.is_empty(), "{arguments:?}");
        let error = error(&output);
        assert_eq!(error["kind"], "usage", "{arguments:?}");
        assert!(
            error["message"]
                .as_str()
                .unwrap_or_default()
                .to_lowercase()
                .contains(message),
            "{error}"
        );
    }
}

#[test]
fn existing_support_token_output_is_rejected_before_credentials_and_preserved() {
    let sandbox = Sandbox::new();
    let path = sandbox.root.join("reserved-token-output");
    fs::write(&path, "keep-existing-data").expect("create existing path");
    let path = path.to_str().expect("sandbox path is UTF-8");
    let output = sandbox.command(&[
        "--format",
        "json",
        "--site",
        SITE,
        "admin",
        "support-token",
        "--output",
        path,
    ]);
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let error = error(&output);
    assert_eq!(error["kind"], "usage");
    assert!(
        error["message"]
            .as_str()
            .unwrap()
            .contains("already exists")
    );
    assert_eq!(fs::read_to_string(path).unwrap(), "keep-existing-data");
}

#[test]
fn support_token_credential_failure_keeps_output_absent_and_stdin_apply_requires_yes_first() {
    let sandbox = Sandbox::new();
    let path = sandbox.root.join("new-token-output");
    let path_text = path.to_str().expect("sandbox path is UTF-8");
    let plan = sandbox.command(&[
        "--format",
        "json",
        "--token-stdin",
        "--site",
        SITE,
        "admin",
        "support-token",
        "--output",
        path_text,
    ]);
    assert_eq!(plan.status.code(), Some(2));
    assert_eq!(error(&plan)["kind"], "config");
    assert!(
        !path.exists(),
        "failed credentials must not create the token file"
    );

    let apply = sandbox.command(&[
        "--format",
        "json",
        "--token-stdin",
        "--site",
        SITE,
        "admin",
        "support-token",
        "--output",
        path_text,
        "--apply",
    ]);
    assert_eq!(apply.status.code(), Some(2));
    assert_eq!(error(&apply)["kind"], "confirmation_required");
    assert!(
        !path.exists(),
        "failed preflight must not create the token file"
    );
}

#[test]
fn admin_help_lists_supported_actions_without_lock_commands_or_token_output() {
    let sandbox = Sandbox::new();
    let output = sandbox.command(&["admin", "--help"]);
    assert_eq!(output.status.code(), Some(0));
    let help = String::from_utf8(output.stdout).expect("help is UTF-8");
    for action in [
        "list",
        "permissions",
        "check-account",
        "add",
        "remove",
        "change-role",
        "maintenance-mode",
        "support-token",
    ] {
        assert!(help.contains(action), "help omitted {action}");
    }
    assert!(!help.contains("unlock"));
    assert!(!help.contains("lock"));
    assert!(!help.contains("support-secret"));

    let token_help = sandbox.command(&["admin", "support-token", "--help"]);
    assert_eq!(token_help.status.code(), Some(0));
    let token_help = String::from_utf8(token_help.stdout).expect("help is UTF-8");
    assert!(token_help.contains("--output"));
    assert!(!token_help.contains("token value"));
}
