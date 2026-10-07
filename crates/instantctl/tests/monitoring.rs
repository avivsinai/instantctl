use serde_json::Value;
use std::{
    fs,
    path::PathBuf,
    process::{Command, Output},
    sync::atomic::{AtomicUsize, Ordering},
};

static NEXT: AtomicUsize = AtomicUsize::new(0);

struct TestHome(PathBuf);

impl TestHome {
    fn new() -> Self {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/monitoring-test-homes")
            .join(format!(
                "{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for TestHome {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn cli(args: &[&str]) -> Output {
    let home = TestHome::new();
    Command::new(env!("CARGO_BIN_EXE_instantctl"))
        .env("HOME", &home.0)
        .env("XDG_CACHE_HOME", home.0.join("cache"))
        .env("XDG_CONFIG_HOME", home.0.join("config"))
        // An explicitly empty token refuses before the OS credential store is used.
        .env("HPE_INSTANT_ON_TOKEN", "")
        .args(args)
        .output()
        .unwrap()
}

fn error(output: &Output) -> Value {
    assert!(output.stdout.is_empty());
    assert_eq!(output.status.code(), Some(2));
    serde_json::from_slice(&output.stderr).unwrap()
}

#[test]
fn future_since_and_invalid_duration_fail_before_credentials_and_get() {
    for since in ["9999-01-01T00:00:00Z", "-1h", "yesterday", "1 month"] {
        for follow in [false, true] {
            let mut args = vec!["event", "list", "--since", since];
            if follow {
                args.push("--follow");
            }
            let result = error(&cli(&args));
            assert_eq!(result["kind"], "usage", "{args:?}");
            assert!(!result["message"].as_str().unwrap().contains("TOKEN"));
        }
    }
}

#[test]
fn follow_rejects_too_fast_or_unbounded_intervals_and_flags_without_follow() {
    for args in [
        vec!["event", "list", "--follow", "--interval", "9"],
        vec![
            "event",
            "list",
            "--follow",
            "--interval",
            "18446744073709551615",
        ],
        vec!["event", "list", "--interval", "15"],
        vec!["event", "list", "--tail", "0"],
        vec!["alert", "list", "--follow"],
    ] {
        assert_eq!(error(&cli(&args))["kind"], "usage", "{args:?}");
    }
}

#[test]
fn monitoring_commands_parse_and_fail_at_credentials_without_network() {
    for args in [
        vec!["event", "list"],
        vec!["event", "list", "--since", "2h"],
        vec![
            "event",
            "list",
            "--follow",
            "--interval",
            "10",
            "--tail",
            "0",
        ],
        vec!["alert", "list"],
        vec!["monitor", "health"],
        vec!["monitor", "dashboard"],
        vec!["monitor", "topology"],
        vec!["monitor", "app-usage"],
        vec!["monitor", "client-usage"],
        vec![
            "monitor",
            "client-usage",
            "--network",
            "network-1",
            "--app-category",
            "video",
        ],
        vec!["monitor", "threats"],
    ] {
        let result = error(&cli(&args));
        assert_eq!(result["kind"], "config", "{args:?}: {result}");
        assert!(result["message"].as_str().unwrap().contains("TOKEN"));
    }
}

#[test]
fn root_and_follow_help_publish_the_approved_nouns_and_options() {
    let output = cli(&["--help"]);
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).unwrap();
    for noun in ["event", "alert", "monitor"] {
        assert!(help.contains(noun));
    }
    let output = cli(&["event", "list", "--help"]);
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).unwrap();
    for option in [
        "--since",
        "--follow",
        "--interval",
        "--tail",
        "[default: 15]",
        "[default: 20]",
    ] {
        assert!(help.contains(option), "missing {option}: {help}");
    }
}
