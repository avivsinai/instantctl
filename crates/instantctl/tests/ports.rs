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
        let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/ports-test-homes")
            .join(format!(
                "{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir_all(&p).unwrap();
        Self(p)
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
        .env("HPE_INSTANT_ON_TOKEN", "")
        .args(args)
        .output()
        .unwrap()
}
fn error(output: Output) -> Value {
    assert!(output.stdout.is_empty());
    assert_eq!(output.status.code(), Some(2));
    serde_json::from_slice(&output.stderr).unwrap()
}
#[test]
fn all_port_verbs_parse_and_stop_at_empty_credentials() {
    for args in [
        vec!["port", "set", "Switch", "1", "--enabled", "false"],
        vec!["port", "power-cycle", "Switch", "1", "--apply", "--yes"],
        vec!["port", "find", "Garage Pi"],
        vec!["port", "cable-test", "Switch", "1", "--apply", "--yes"],
        vec![
            "port",
            "connectivity-test",
            "Switch",
            "1.1.1.1",
            "--apply",
            "--yes",
        ],
        vec![
            "port",
            "mirror",
            "Switch",
            "--enabled",
            "--destination",
            "2",
            "--sources",
            "1",
        ],
        vec!["port", "profile", "list"],
        vec!["port", "profile", "show", "Default"],
        vec!["port", "profile", "create", "test", "--from", "Default"],
        vec!["port", "profile", "set", "test", "--storm-control", "true"],
        vec!["port", "profile", "remove", "test", "--apply", "--yes"],
        vec!["port", "poe-schedule", "show"],
        vec![
            "port",
            "poe-schedule",
            "set",
            "--active",
            "simple",
            "--days",
            "mon,tue",
            "--all-day",
        ],
        vec!["port", "eee", "show"],
        vec!["port", "eee", "set", "true"],
        vec![
            "lag", "create", "Switch", "1", "--ports", "1,2", "--mode", "lacp",
        ],
        vec![
            "lag", "remove", "Switch", "1", "--force", "--apply", "--yes",
        ],
    ] {
        let e = error(cli(&args));
        assert_eq!(e["kind"], "config", "{args:?}: {e}");
        assert!(e["message"].as_str().unwrap().contains("TOKEN"));
    }
}
#[test]
fn noninteractive_apply_refuses_before_credentials_for_every_write() {
    for mut args in [
        vec!["port", "set", "Switch", "1", "--enabled", "false"],
        vec!["port", "power-cycle", "Switch", "1"],
        vec!["port", "cable-test", "Switch", "1"],
        vec!["port", "connectivity-test", "Switch", "1.1.1.1"],
        vec!["port", "mirror", "Switch"],
        vec!["port", "profile", "create", "test", "--from", "Default"],
        vec!["port", "profile", "set", "test", "--name", "New"],
        vec!["port", "profile", "remove", "test"],
        vec!["port", "poe-schedule", "set", "--active", "none"],
        vec!["port", "eee", "set", "true"],
        vec!["lag", "create", "Switch", "1", "--ports", "1,2"],
        vec!["lag", "remove", "Switch", "1"],
    ] {
        args.push("--apply");
        let e = error(cli(&args));
        assert_eq!(e["kind"], "confirmation_required", "{args:?}: {e}");
        assert!(!e["message"].as_str().unwrap().contains("TOKEN"));
    }
}
#[test]
fn cycle_has_no_force_override_and_invalid_ports_are_usage() {
    for args in [
        vec!["port", "power-cycle", "Switch", "14", "--force"],
        vec!["port", "power-cycle", "Switch", "0"],
        vec!["port", "set", "Switch", "1"],
        vec!["port", "set", "Switch", "1", "--poe-mode", "disabled"],
        vec!["lag", "create", "Switch", "0", "--ports", "1,2"],
    ] {
        assert_eq!(error(cli(&args))["kind"], "usage", "{args:?}");
    }
}
#[test]
fn port_help_exposes_feature_commands() {
    let output = cli(&["port", "--help"]);
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).unwrap();
    for verb in [
        "power-cycle",
        "find",
        "set",
        "profile",
        "poe-schedule",
        "eee",
        "mirror",
        "cable-test",
        "connectivity-test",
    ] {
        assert!(help.contains(verb), "{verb}");
    }
}
