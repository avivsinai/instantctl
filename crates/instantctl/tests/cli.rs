use serde_json::{Value, json};
use std::{
    env, fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::atomic::{AtomicU64, Ordering},
};

const TOKEN_ENV: &str = "HPE_INSTANT_ON_TOKEN";
const TOKEN_LIMIT: usize = 16_384;

struct TestHome {
    path: PathBuf,
}

impl TestHome {
    fn new() -> Self {
        static NEXT_HOME_ID: AtomicU64 = AtomicU64::new(0);

        for _ in 0..100 {
            let path = env::temp_dir().join(format!(
                "instantctl-cli-test-{}-{}",
                std::process::id(),
                NEXT_HOME_ID.fetch_add(1, Ordering::Relaxed)
            ));
            let mut builder = fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            match builder.create(&path) {
                Ok(()) => {
                    let home = Self { path };
                    #[cfg(target_os = "macos")]
                    {
                        // Security.framework resolves the default Keychain through HOME.
                        // Keep that lookup while isolating the CLI's cache writes.
                        let library = home.path.join("Library");
                        fs::create_dir(&library).expect("create test home Library");
                        let keychains = PathBuf::from(env::var_os("HOME").expect("parent HOME"))
                            .join("Library/Keychains");
                        std::os::unix::fs::symlink(keychains, library.join("Keychains"))
                            .expect("preserve default Keychain lookup");
                    }
                    return home;
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("create isolated test home: {error}"),
            }
        }
        panic!("could not allocate a unique isolated test home");
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn xdg_cache_path(&self) -> PathBuf {
        self.path.join("xdg-cache")
    }
}

impl Drop for TestHome {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

thread_local! {
    static TEST_HOME: TestHome = TestHome::new();
}

fn test_command(program: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut command = Command::new(program);
    TEST_HOME.with(|home| {
        command.env("HOME", home.path());
        command.env("XDG_CACHE_HOME", home.xdg_cache_path());
    });
    command
}

fn test_profile_lock_path(profile: &str) -> PathBuf {
    TEST_HOME.with(|home| {
        let cache = if cfg!(target_os = "macos") {
            home.path().join("Library/Caches")
        } else {
            home.xdg_cache_path()
        };
        cache.join("hpe-network").join(format!("{profile}.lock"))
    })
}

fn parent_profile_lock_path(profile: &str) -> Option<PathBuf> {
    let home = env::var_os("HOME").filter(|home| !home.is_empty())?;
    let cache = if cfg!(target_os = "macos") {
        PathBuf::from(home).join("Library/Caches")
    } else {
        env::var_os("XDG_CACHE_HOME")
            .filter(|cache| !cache.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(home).join(".cache"))
    };
    Some(cache.join("hpe-network").join(format!("{profile}.lock")))
}

fn cli(args: &[&str]) -> Output {
    test_command(env!("CARGO_BIN_EXE_instantctl"))
        .args(args)
        .output()
        .expect("run instantctl")
}

fn error_json(output: &Output) -> Value {
    serde_json::from_slice(&output.stderr).expect("stderr contains a JSON error")
}

fn cli_without_token(args: &[&str]) -> Output {
    test_command(env!("CARGO_BIN_EXE_instantctl"))
        .env_remove(TOKEN_ENV)
        .args(args)
        .output()
        .expect("run instantctl")
}

fn cli_with_stdin(args: &[&str], env_token: Option<&str>, token_stdin: &[u8]) -> Output {
    let mut command = test_command(env!("CARGO_BIN_EXE_instantctl"));
    command
        .env_remove(TOKEN_ENV)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(token) = env_token {
        command.env(TOKEN_ENV, token);
    }
    let mut child = command.spawn().expect("spawn instantctl");
    child
        .stdin
        .take()
        .expect("child stdin is piped")
        .write_all(token_stdin)
        .expect("write token input");
    child.wait_with_output().expect("wait for instantctl")
}

fn unique_profile(label: &str) -> String {
    static NEXT_PROFILE_ID: AtomicU64 = AtomicU64::new(0);
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!(
        "codex-test-{label}-{}-{timestamp}-{}",
        std::process::id(),
        NEXT_PROFILE_ID.fetch_add(1, Ordering::Relaxed)
    )
}

fn write_test_file(label: &str, contents: &[u8]) -> PathBuf {
    let path = TEST_HOME.with(|home| home.path().join(format!("{}.input", unique_profile(label))));
    fs::write(&path, contents).expect("write isolated CLI input file");
    path
}

fn api_preview(output: &Output) -> Value {
    assert_eq!(output.status.code(), Some(0));
    let stdout: Value = serde_json::from_slice(&output.stdout).expect("preview stdout is JSON");
    let stderr: Value = serde_json::from_slice(&output.stderr).expect("preview stderr is JSON");
    assert_eq!(stdout, stderr, "the plan is printed to both output streams");
    stdout
}

fn assert_config_error(output: &Output, message_fragment: &str, secrets: &[&str]) {
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    let error = error_json(output);
    assert_eq!(error["kind"], "config");
    let message = error["message"].as_str().expect("error message string");
    assert!(
        message.contains(message_fragment),
        "expected error message containing {message_fragment:?}, got {message:?}"
    );
    for secret in secrets {
        assert!(
            !stderr.contains(secret),
            "secret leaked to stderr: {secret}"
        );
        assert!(
            !String::from_utf8_lossy(&output.stdout).contains(secret),
            "secret leaked to stdout: {secret}"
        );
    }
}

fn assert_missing_profile_error(output: &Output) {
    assert_eq!(
        output.status.code(),
        Some(3),
        "unexpected saved-profile exit; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty());
    let error = error_json(output);
    assert_eq!(error["kind"], "auth");
    assert!(
        error["message"]
            .as_str()
            .is_some_and(|message| message.contains("no saved login")),
        "unexpected no-login error: {error}"
    );
}

fn expected_git_sha() -> String {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let tracked = Command::new("git")
        .current_dir(&manifest)
        .args(["ls-files", "--error-unmatch", "Cargo.toml"])
        .output();
    if !tracked.is_ok_and(|output| output.status.success()) {
        return "unknown".into();
    }
    let output = Command::new("git")
        .current_dir(manifest)
        .args(["rev-parse", "--verify", "HEAD"])
        .output();
    let Ok(output) = output else {
        return "unknown".into();
    };
    if !output.status.success() {
        return "unknown".into();
    }
    let Ok(sha) = String::from_utf8(output.stdout) else {
        return "unknown".into();
    };
    let sha = sha.trim();
    if matches!(sha.len(), 40 | 64) && sha.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        sha.to_owned()
    } else {
        "unknown".into()
    }
}

#[test]
fn version_defaults_to_json_when_stdout_is_piped() {
    let output = cli(&["version"]);
    let git_sha = expected_git_sha();

    assert_eq!(output.status.code(), Some(0));
    assert!(output.stderr.is_empty());
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).expect("stdout contains JSON data"),
        json!({
            "name": "instantctl",
            "version": env!("CARGO_PKG_VERSION"),
            "git_sha": git_sha,
        })
    );
}

#[test]
fn version_supports_each_explicit_format() {
    let git_sha = expected_git_sha();
    let json_output = cli(&["--format=json", "version"]);
    assert_eq!(json_output.status.code(), Some(0));
    assert_eq!(
        serde_json::from_slice::<Value>(&json_output.stdout).expect("JSON format is valid"),
        json!({
            "name": "instantctl",
            "version": env!("CARGO_PKG_VERSION"),
            "git_sha": git_sha.clone(),
        })
    );

    let yaml_output = cli(&["--format", "yaml", "version"]);
    assert_eq!(yaml_output.status.code(), Some(0));
    let yaml = String::from_utf8(yaml_output.stdout).expect("YAML output is UTF-8");
    assert!(yaml.contains("name: instantctl"), "unexpected YAML: {yaml}");
    assert!(
        yaml.contains(&format!("version: {}", env!("CARGO_PKG_VERSION"))),
        "unexpected YAML: {yaml}"
    );
    assert!(
        yaml.contains(&format!("git_sha: {git_sha}")),
        "unexpected YAML: {yaml}"
    );

    let table_output = cli(&["--format", "table", "version"]);
    assert_eq!(table_output.status.code(), Some(0));
    let table = String::from_utf8(table_output.stdout).expect("table output is UTF-8");
    assert!(table.contains("NAME"), "unexpected table: {table}");
    assert!(table.contains("VERSION"), "unexpected table: {table}");
    assert!(table.contains("GIT_SHA"), "unexpected table: {table}");
    assert!(table.contains(&git_sha), "unexpected table: {table}");
    assert!(!table.trim_start().starts_with('{'));

    let version_flag = cli(&["--version"]);
    assert_eq!(version_flag.status.code(), Some(0));
    assert!(version_flag.stderr.is_empty());
    assert_eq!(
        String::from_utf8(version_flag.stdout).expect("--version output is UTF-8"),
        format!("instantctl {} (git {git_sha})\n", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn root_help_lists_commands_and_global_flags() {
    let output = cli(&["--help"]);

    assert_eq!(output.status.code(), Some(0));
    let help = String::from_utf8(output.stdout).expect("help is UTF-8");
    for command in [
        "auth",
        "api",
        "site",
        "stack",
        "device",
        "client",
        "network",
        "wlan",
        "guest-portal",
        "schedule",
        "radius",
        "port-access-control",
        "port",
        "lag",
        "radio",
        "completion",
        "completions",
        "version",
    ] {
        assert!(
            help.contains(command),
            "help does not list {command}: {help}"
        );
    }
    for flag in ["--format", "--site", "--profile", "--timeout", "-v"] {
        assert!(help.contains(flag), "help does not list {flag}: {help}");
    }
}

#[test]
fn every_noun_and_completion_accepts_help() {
    for command in [
        "auth",
        "api",
        "site",
        "stack",
        "device",
        "client",
        "network",
        "wlan",
        "guest-portal",
        "schedule",
        "radius",
        "port-access-control",
        "port",
        "lag",
        "radio",
        "completion",
        "completions",
        "version",
    ] {
        let output = cli(&[command, "--help"]);
        assert_eq!(
            output.status.code(),
            Some(0),
            "{command} --help failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("Usage:"),
            "{command} --help did not print usage"
        );
    }

    let site_help = cli(&["site", "--help"]);
    assert_eq!(site_help.status.code(), Some(0));
    let site_help = String::from_utf8(site_help.stdout).expect("site help is UTF-8");
    assert!(
        site_help.contains("extend-network"),
        "missing command: {site_help}"
    );

    let extend_help = cli(&["site", "extend-network", "--help"]);
    assert_eq!(extend_help.status.code(), Some(0));
    let extend_help = String::from_utf8(extend_help.stdout).expect("extend-network help is UTF-8");
    for flag in ["--enabled", "--outdoor-mesh", "--apply"] {
        assert!(extend_help.contains(flag), "missing {flag}: {extend_help}");
    }

    for (args, fragments) in [
        (
            vec!["device", "--help"],
            vec![
                "reserve-ip",
                "remove-ip-reservation",
                "replacement-candidates",
            ],
        ),
        (
            vec!["device", "reserve-ip", "--help"],
            vec!["<DEVICE>", "<ADDRESS>", "--network", "--apply"],
        ),
        (
            vec!["device", "remove-ip-reservation", "--help"],
            vec!["<DEVICE>", "--apply"],
        ),
        (
            vec!["device", "replacement-candidates", "--help"],
            vec!["<DEVICE>"],
        ),
        (
            vec!["site", "stp-auto-priority", "--help"],
            vec!["--apply", "--yes"],
        ),
        (
            vec!["network", "routing", "--help"],
            vec!["<SELECTOR>", "--enabled", "--apply"],
        ),
        (
            vec!["wlan", "allowlist", "--help"],
            vec!["<NETWORK>", "--add", "--remove", "--apply"],
        ),
        (
            vec!["device", "allowlist", "--help"],
            vec!["<DEVICE>", "--port", "--trunk", "--add", "--remove"],
        ),
        (
            vec!["site", "create", "--help"],
            vec!["--country", "--timezone", "--apply"],
        ),
        (
            vec!["site", "rename", "--help"],
            vec!["<SELECTOR>", "<NAME>", "--apply"],
        ),
        (
            vec!["site", "delete", "--help"],
            vec!["--confirm-name", "--yes", "--apply"],
        ),
        (
            vec!["site", "clone", "--help"],
            vec!["<SOURCE>", "--country", "--timezone", "--apply"],
        ),
        (vec!["site", "country", "--help"], vec!["Usage:"]),
        (vec!["stack", "list", "--help"], vec!["Usage:"]),
        (vec!["stack", "show", "--help"], vec!["<SELECTOR>"]),
    ] {
        let help = cli(&args);
        assert_eq!(help.status.code(), Some(0), "{args:?}");
        let help = String::from_utf8_lossy(&help.stdout);
        for fragment in fragments {
            assert!(
                help.contains(fragment),
                "{args:?} missing {fragment}: {help}"
            );
        }
    }

    let singular = cli(&["completion", "zsh"]);
    let plural = cli(&["completions", "zsh"]);
    assert_eq!(singular.status.code(), Some(0));
    assert_eq!(plural.status.code(), Some(0));
    assert_eq!(singular.stdout, plural.stdout);
    assert!(!singular.stdout.is_empty());

    let unknown_shell = cli(&["--format=json", "completion", "no-such-shell"]);
    assert_eq!(unknown_shell.status.code(), Some(2));
    assert!(unknown_shell.stdout.is_empty());
    assert_eq!(error_json(&unknown_shell)["kind"], "usage");
}

#[test]
fn usage_errors_follow_explicit_output_format() {
    let json_output = cli(&["--format=json", "no-such-command"]);
    assert_eq!(json_output.status.code(), Some(2));
    assert!(json_output.stdout.is_empty());
    let error = error_json(&json_output);
    assert_eq!(error["kind"], "usage");
    assert!(
        error["message"]
            .as_str()
            .is_some_and(|message| !message.is_empty())
    );

    let yaml_output = cli(&["--format=yaml", "no-such-command"]);
    assert_eq!(yaml_output.status.code(), Some(2));
    assert!(yaml_output.stdout.is_empty());
    let yaml = String::from_utf8(yaml_output.stderr).expect("YAML error is UTF-8");
    assert!(
        yaml.contains("kind: usage"),
        "unexpected YAML error: {yaml}"
    );
    assert!(yaml.contains("message:"), "unexpected YAML error: {yaml}");

    let table_output = cli(&["--format=table", "no-such-command"]);
    assert_eq!(table_output.status.code(), Some(2));
    assert!(table_output.stdout.is_empty());
    let table = String::from_utf8(table_output.stderr).expect("table error is UTF-8");
    assert!(
        table.starts_with("error:"),
        "unexpected table error: {table}"
    );
    assert!(!table.trim_start().starts_with('{'));
}

#[test]
fn wired_network_list_requires_an_isolated_saved_profile_before_network() {
    let profile = unique_profile("network-no-login");
    let output = cli_without_token(&["--profile", &profile, "network", "list", "--format", "json"]);
    assert_missing_profile_error(&output);
}

#[test]
fn auth_status_for_an_isolated_missing_profile_exits_as_auth_error() {
    let profile = unique_profile("auth-status-missing");
    let output = cli_without_token(&["--format=json", "--profile", &profile, "auth", "status"]);

    assert_missing_profile_error(&output);
}

#[test]
fn auth_logout_without_a_saved_login_is_idempotent_for_an_isolated_profile() {
    let profile = unique_profile("auth-logout-empty");
    let test_lock = test_profile_lock_path(&profile);
    let parent_lock = parent_profile_lock_path(&profile);
    assert!(!test_lock.exists(), "profile lock exists before logout");
    if let Some(path) = &parent_lock {
        assert!(
            !path.try_exists().expect("inspect parent profile cache"),
            "isolated test profile already exists in the parent cache: {}",
            path.display()
        );
    }
    for _ in 0..2 {
        let output = cli_without_token(&["--format=json", "--profile", &profile, "auth", "logout"]);

        assert_eq!(output.status.code(), Some(0));
        assert!(output.stderr.is_empty());
        assert_eq!(
            serde_json::from_slice::<Value>(&output.stdout).expect("logout result JSON"),
            json!({
                "profile": profile,
                "logged_out": true,
                "refresh_revoked": null,
            })
        );
        if let Some(path) = &parent_lock {
            assert!(
                !path.try_exists().expect("inspect parent profile cache"),
                "CLI wrote the isolated profile lock into the parent cache: {}",
                path.display()
            );
        }
        #[cfg(target_os = "macos")]
        assert!(
            test_lock.is_file(),
            "logout removed the stable profile lock"
        );
        #[cfg(not(target_os = "macos"))]
        assert!(
            !test_lock.exists(),
            "unsupported-platform logout unexpectedly created a profile lock"
        );
    }
}

#[test]
fn non_tty_login_without_credentials_fails_before_authentication() {
    let profile = unique_profile("auth-login-no-input");
    let output = test_command(env!("CARGO_BIN_EXE_instantctl"))
        .env_remove(TOKEN_ENV)
        .args([
            "--format=json",
            "--profile",
            profile.as_str(),
            "auth",
            "login",
        ])
        .stdin(Stdio::null())
        .output()
        .expect("run non-interactive login without credentials");

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert_eq!(error_json(&output)["kind"], "config");
}

#[test]
fn piped_confirmation_cannot_authorize_device_forget_before_configuration() {
    let output = cli_with_stdin(
        &[
            "--format=json",
            "device",
            "forget",
            "00:11:22:33:44:55",
            "--apply",
        ],
        None,
        b"yes\n",
    );

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let error = error_json(&output);
    assert_eq!(error["kind"], "confirmation_required");
    assert!(
        error["message"]
            .as_str()
            .is_some_and(|message| message.contains("use --yes")),
        "piped confirmation was not refused: {error}"
    );
}

#[test]
fn zero_timeout_is_a_usage_error() {
    let output = cli(&["--timeout", "0", "version"]);

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert_eq!(error_json(&output)["kind"], "usage");
}

#[test]
fn core_cloud_reads_require_an_isolated_saved_profile_before_network() {
    for (index, (arguments, site)) in [
        (vec!["site", "list"], None),
        (vec!["site", "show"], None),
        (vec!["site", "capabilities"], None),
        (vec!["site", "health"], None),
        (vec!["site", "dashboard"], None),
        (vec!["site", "topology"], None),
        (vec!["site", "timezone"], None),
        (vec!["site", "management-network"], None),
        (vec!["site", "dns"], None),
        (vec!["site", "spanning-tree"], None),
        (vec!["stack", "list"], None),
        (vec!["stack", "show", "Core stack"], None),
        (
            vec![
                "site",
                "create",
                "New Site",
                "--country",
                "IL",
                "--timezone",
                "Europe/Berlin",
            ],
            None,
        ),
        (vec!["site", "rename", "Source Site", "Renamed Site"], None),
        (
            vec![
                "site",
                "clone",
                "Source Site",
                "New Copy",
                "--country",
                "IL",
                "--timezone",
                "Europe/Berlin",
            ],
            None,
        ),
        (vec!["site", "delete", "Old Site"], None),
        (vec!["device", "list"], None),
        (vec!["device", "show", "AP"], None),
        (vec!["port", "list"], None),
        (vec!["port", "show", "Switch", "1"], None),
        (vec!["lag", "list"], None),
        (vec!["radio", "list"], None),
        (vec!["client", "list"], None),
        (vec!["guest-portal", "show"], None),
        (vec!["schedule", "list"], None),
        (vec!["radius", "list"], None),
        (vec!["port-access-control", "show"], None),
        (vec!["client", "show", "PC"], None),
        (vec!["client", "tags", "list", "Desk"], None),
        (
            vec!["client", "tags", "list", "Desk"],
            Some("11111111-2222-3333-4444-555555555555"),
        ),
        (vec!["client", "where-is", "Desk"], None),
        (
            vec!["client", "where-is", "Desk"],
            Some("11111111-2222-3333-4444-555555555555"),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let profile = unique_profile(&format!("cloud-read-no-login-{index}"));
        let mut args = vec!["--format=json", "--profile", profile.as_str()];
        if let Some(site) = site {
            args.extend(["--site", site]);
        }
        args.extend(arguments);
        // A premature public request must fail locally rather than depend on
        // the real portal being reachable. Port zero has no listening service.
        let output = test_command(env!("CARGO_BIN_EXE_instantctl"))
            .env_remove(TOKEN_ENV)
            .env("HTTPS_PROXY", "http://127.0.0.1:0")
            .env("https_proxy", "http://127.0.0.1:0")
            .env("NO_PROXY", "")
            .env("no_proxy", "")
            .args(&args)
            .output()
            .expect("run instantctl without credentials or portal access");
        assert_missing_profile_error(&output);
    }
}

#[test]
fn invalid_site_precedes_credentials_and_network_for_all_core_reads() {
    for arguments in [
        vec!["site", "list"],
        vec!["site", "show"],
        vec!["site", "capabilities"],
        vec!["site", "health"],
        vec!["site", "dashboard"],
        vec!["site", "topology"],
        vec!["site", "timezone"],
        vec!["site", "management-network"],
        vec!["site", "dns"],
        vec!["site", "spanning-tree"],
        vec!["device", "list"],
        vec!["device", "show", "AP"],
        vec!["device", "health", "192.0.2.1"],
        vec!["device", "replacement-candidates", "AP"],
        vec![
            "device",
            "reserve-ip",
            "AP",
            "192.0.2.9",
            "--network",
            "net",
        ],
        vec!["device", "remove-ip-reservation", "AP"],
        vec!["device", "allowlist", "AP", "--port", "1"],
        vec!["port", "list"],
        vec!["port", "show", "Switch", "1"],
        vec!["lag", "list"],
        vec!["radio", "list"],
        vec!["client", "list"],
        vec!["client", "show", "PC"],
        vec!["guest-portal", "show"],
        vec!["schedule", "list"],
        vec!["radius", "list"],
        vec!["port-access-control", "show"],
        vec!["network", "routing", "LAN"],
        vec!["network", "routing", "LAN", "--enabled", "false"],
        vec!["wlan", "allowlist", "Guest", "--add", "AA:BB:CC:DD:EE:FF"],
        vec!["site", "stp-auto-priority"],
    ] {
        let mut args = vec!["--site", "../../escape", "--format=json"];
        args.extend(arguments);
        let output = cli_without_token(&args);
        assert_config_error(&output, "site identifier must be a UUID", &[]);
    }
}

#[test]
fn mutation_preflight_precedes_credentials_and_network() {
    for (args, fragment, kind) in [
        (
            vec![
                "site",
                "create",
                "New Site",
                "--country",
                "I1",
                "--timezone",
                "Europe/Berlin",
            ],
            "country",
            "usage",
        ),
        (
            vec![
                "site",
                "create",
                "",
                "--country",
                "IL",
                "--timezone",
                "Europe/Berlin",
            ],
            "site name",
            "usage",
        ),
        (
            vec![
                "site",
                "create",
                "New Site",
                "--country",
                "IL",
                "--timezone",
                "Etc/Unknown",
            ],
            "timezone",
            "usage",
        ),
        (
            vec!["site", "rename", "Existing Site", ""],
            "site name",
            "usage",
        ),
        (
            vec![
                "site",
                "clone",
                "Source Site",
                "",
                "--country",
                "IL",
                "--timezone",
                "Europe/Berlin",
            ],
            "site name",
            "usage",
        ),
        (
            vec![
                "site",
                "delete",
                "Existing Site",
                "--apply",
                "--confirm-name",
                "Existing Site",
            ],
            "--yes and --confirm-name",
            "confirmation_required",
        ),
        (
            vec!["site", "delete", "Existing Site", "--apply", "--yes"],
            "--yes and --confirm-name",
            "confirmation_required",
        ),
        (vec!["site", "timezone", ""], "timezone", "usage"),
        (vec!["site", "timezone", "Not/AZone"], "timezone", "usage"),
        (vec!["site", "dns", "--mode", "custom"], "primary", "config"),
        (
            vec![
                "site",
                "dns",
                "--mode",
                "automatic",
                "--primary",
                "192.0.2.53",
            ],
            "custom",
            "usage",
        ),
        (
            vec!["site", "management-network", "--vlan", "3333"],
            "VLAN",
            "usage",
        ),
        (
            vec!["site", "spanning-tree", "--priority", "500"],
            "priority",
            "usage",
        ),
        (
            vec!["site", "extend-network", "--apply"],
            "setting value",
            "config",
        ),
        (
            vec!["site", "timezone", "--apply"],
            "setting value",
            "config",
        ),
        (
            vec!["site", "extend-network", "--enabled", "false", "--apply"],
            "use --yes",
            "confirmation_required",
        ),
        (
            vec!["site", "spanning-tree", "--rstp", "true", "--apply"],
            "use --yes",
            "confirmation_required",
        ),
        (
            vec!["site", "stp-auto-priority", "--apply"],
            "use --yes",
            "confirmation_required",
        ),
        (
            vec![
                "device",
                "reserve-ip",
                "AA:BB:CC:DD:EE:FF",
                "192.0.2.9",
                "--network",
                "network-id",
                "--apply",
            ],
            "use --yes",
            "confirmation_required",
        ),
        (
            vec![
                "device",
                "remove-ip-reservation",
                "AA:BB:CC:DD:EE:FF",
                "--apply",
            ],
            "use --yes",
            "confirmation_required",
        ),
        (
            vec!["network", "routing", "LAN", "--enabled", "false", "--apply"],
            "use --yes",
            "confirmation_required",
        ),
        (
            vec!["network", "routing", "LAN", "--apply"],
            "--enabled",
            "config",
        ),
        (
            vec!["wlan", "allowlist", "Guest", "--apply"],
            "--add or --remove",
            "config",
        ),
        (
            vec![
                "wlan",
                "allowlist",
                "Guest",
                "--add",
                "AA:BB:CC:DD:EE:FF",
                "--apply",
            ],
            "use --yes",
            "confirmation_required",
        ),
        (
            vec![
                "wlan",
                "allowlist",
                "Guest",
                "--add",
                "bad-mac",
                "--apply",
                "--yes",
            ],
            "MAC",
            "usage",
        ),
        (
            vec![
                "wlan",
                "allowlist",
                "Guest",
                "--add",
                "AA:BB:CC:DD:EE:FF",
                "--remove",
                "AA:BB:CC:DD:EE:FF",
                "--apply",
            ],
            "argument cannot be used",
            "usage",
        ),
        (
            vec!["device", "allowlist", "AP", "--port", "1", "--apply"],
            "--add or --remove",
            "config",
        ),
        (
            vec![
                "device",
                "allowlist",
                "AP",
                "--add",
                "AA:BB:CC:DD:EE:FF",
                "--apply",
            ],
            "required arguments",
            "usage",
        ),
        (
            vec!["device", "allowlist", "AP", "--port", "0", "--apply"],
            "greater than zero",
            "usage",
        ),
        (
            vec!["device", "allowlist", "AP", "--trunk", "0", "--apply"],
            "greater than zero",
            "usage",
        ),
        (
            vec![
                "device",
                "allowlist",
                "AP",
                "--port",
                "1",
                "--add",
                "AA:BB:CC:DD:EE:FF",
                "--apply",
            ],
            "use --yes",
            "confirmation_required",
        ),
    ] {
        let mut argv = vec!["--format=json"];
        argv.extend(args);
        let output = test_command(env!("CARGO_BIN_EXE_instantctl"))
            .env(TOKEN_ENV, "invalid-secret token")
            .args(&argv)
            .stdin(Stdio::null())
            .output()
            .expect("run invalid or unconfirmed site change");
        assert_eq!(output.status.code(), Some(2), "args: {argv:?}");
        assert!(output.stdout.is_empty());
        let error = error_json(&output);
        assert_eq!(error["kind"], kind, "args: {argv:?}");
        assert!(
            error["message"]
                .as_str()
                .is_some_and(|message| message.contains(fragment)),
            "unexpected guard for {argv:?}: {error}"
        );
        assert!(!String::from_utf8_lossy(&output.stderr).contains("invalid-secret"));
    }
}

#[test]
fn read_verb_usage_errors_and_health_host_errors_have_exit_two() {
    for args in [
        vec!["lag", "list", "--switch"],
        vec!["radio", "list", "--ap"],
        vec!["client", "show"],
    ] {
        let output = cli_without_token(&args);
        assert_eq!(output.status.code(), Some(2));
        assert_eq!(error_json(&output)["kind"], "usage");
    }
    let output = cli_without_token(&["device", "health", "user:secret@ap", "--host"]);
    assert_config_error(&output, "AP host", &["secret"]);
}

#[test]
fn api_takes_a_path_directly_and_missing_profile_fails_before_network() {
    let help = cli_without_token(&["api", "--help"]);
    assert_eq!(help.status.code(), Some(0));
    let help = String::from_utf8(help.stdout).expect("api help is UTF-8");
    assert!(help.contains("<PATH>"), "API path argument missing: {help}");
    assert!(
        !help.contains("get <"),
        "API command still has a get subcommand: {help}"
    );
    assert!(help.contains("--apply"), "API apply flag missing: {help}");
    assert!(
        help.contains("without claiming state readback"),
        "API help must state that apply does not verify state: {help}"
    );

    let profile = unique_profile("api-no-login");
    let output = cli_without_token(&[
        "--format=json",
        "--profile",
        &profile,
        "api",
        "/sites/example",
    ]);
    assert_missing_profile_error(&output);
}

#[test]
fn api_rejects_method_options_and_extra_arguments_as_usage_errors() {
    for args in [
        vec![
            "--format=json",
            "api",
            "/sites/example",
            "--method",
            "PATCH",
        ],
        vec!["--format=json", "api", "/sites/example", "extra"],
        vec!["--format=json", "api", "get", "/sites/example"],
        vec!["--format=json", "api", "/sites/example", "--paginate"],
        vec![
            "--format=json",
            "api",
            "/sites/example",
            "--method",
            "GET",
            "--apply",
        ],
    ] {
        let output = cli_without_token(&args);
        assert_eq!(output.status.code(), Some(2), "args: {args:?}");
        assert!(output.stdout.is_empty(), "args: {args:?}");
        let error = error_json(&output);
        assert_eq!(error["kind"], "usage", "args: {args:?}");
    }
}

#[test]
fn api_dry_run_defaults_to_post_and_typed_fields_override_raw_fields() {
    let field_path = write_test_file("api-field", b"from-file\n");
    let profile = unique_profile("api-preview-no-login");
    let typed_file_field = format!("note=@{}", field_path.display());
    for yes in [false, true] {
        let mut args = vec![
            "--format=json",
            "--profile",
            &profile,
            "api",
            "/sites/example",
            "--raw-field",
            "count=raw-value",
            "--raw-field",
            "label=plain",
            "--field",
            "count=7",
            "--field",
            "enabled=true",
            "--field",
            "disabled=false",
            "--field",
            "unset=null",
            "--field",
            "ratio=1.5",
            "--field",
            "max=18446744073709551615",
            "--field",
            &typed_file_field,
        ];
        if yes {
            args.push("--yes");
        }
        let output = cli_without_token(&args);

        let plan = api_preview(&output);
        assert_eq!(plan["method"], "POST");
        assert_eq!(plan["path"], "/sites/example");
        assert_eq!(
            plan["body"],
            json!({
                "count":7,
                "label":"plain",
                "enabled":true,
                "disabled":false,
                "unset":null,
                "ratio":1.5,
                "max":18446744073709551615u64,
                "note":"from-file\n"
            })
        );
        assert_eq!(plan["request_attempted"], false);
    }
}

#[test]
fn api_input_files_and_stdin_become_body_or_encoded_query_without_credentials() {
    let input_path = write_test_file("api-body", br#"{"source":"file-body"}"#);
    let input_path = input_path.to_string_lossy().into_owned();
    let profile = unique_profile("api-file-preview");
    let output = cli_without_token(&[
        "--format=json",
        "--profile",
        &profile,
        "api",
        "/sites/example?existing=yes",
        "--method",
        "POST",
        "--input",
        &input_path,
        "--raw-field",
        "query=a & b",
        "--field",
        "limit=2",
    ]);
    let plan = api_preview(&output);
    assert_eq!(plan["method"], "POST");
    let (path, query) = plan["path"].as_str().unwrap().split_once('?').unwrap();
    assert_eq!(path, "/sites/example");
    let mut fields = url::form_urlencoded::parse(query.as_bytes())
        .into_owned()
        .collect::<Vec<_>>();
    fields.sort();
    assert_eq!(
        fields,
        vec![
            ("existing".to_owned(), "yes".to_owned()),
            ("limit".to_owned(), "2".to_owned()),
            ("query".to_owned(), "a & b".to_owned()),
        ]
    );
    assert_eq!(plan["body"], json!({"source":"file-body"}));

    let profile = unique_profile("api-stdin-body-preview");
    let output = cli_with_stdin(
        &[
            "--format=json",
            "--profile",
            &profile,
            "api",
            "/sites/example",
            "--method",
            "POST",
            "--input",
            "-",
            "--raw-field",
            "filter=AP & client",
        ],
        None,
        br#"{"source":"stdin-body"}"#,
    );
    let plan = api_preview(&output);
    assert_eq!(plan["path"], "/sites/example?filter=AP+%26+client");
    assert_eq!(plan["body"], json!({"source":"stdin-body"}));

    let profile = unique_profile("api-stdin-field-preview");
    let output = cli_with_stdin(
        &[
            "--format=json",
            "--profile",
            &profile,
            "api",
            "/sites/example",
            "--field",
            "payload=@-",
        ],
        None,
        b"stdin field text",
    );
    let plan = api_preview(&output);
    assert_eq!(plan["method"], "POST");
    assert_eq!(plan["body"], json!({"payload":"stdin field text"}));

    let profile = unique_profile("api-legacy-stdin-field-preview");
    let output = cli_with_stdin(
        &[
            "--format=json",
            "--profile",
            &profile,
            "api",
            "/sites/example",
            "--field",
            "payload=-",
        ],
        None,
        b"legacy stdin field",
    );
    let plan = api_preview(&output);
    assert_eq!(plan["body"], json!({"payload":"legacy stdin field"}));
}

#[test]
fn api_previews_redact_nested_secret_and_opaque_bodies_until_opted_in() {
    #[derive(Clone, Copy)]
    enum InputKind {
        RawField,
        TypedField,
        JsonInput,
    }

    let cases = [
        ("password", InputKind::RawField),
        ("passwd", InputKind::TypedField),
        ("passphrase", InputKind::JsonInput),
        ("preSharedKey", InputKind::RawField),
        ("psk", InputKind::TypedField),
        ("secret", InputKind::JsonInput),
        ("token", InputKind::RawField),
        ("credential", InputKind::TypedField),
        ("apiKey", InputKind::JsonInput),
        ("privateKey", InputKind::RawField),
        ("authorization", InputKind::TypedField),
        ("radiusSecret", InputKind::JsonInput),
        ("sharedSecret", InputKind::RawField),
        ("clientSecret", InputKind::TypedField),
        ("credentials", InputKind::JsonInput),
        ("pppoePassword", InputKind::RawField),
        ("oldPassword", InputKind::TypedField),
        ("newPassword", InputKind::JsonInput),
        ("privateKeyPlaceholder", InputKind::RawField),
        ("PaSs_WoRd", InputKind::TypedField),
        ("PaSs-WoRd", InputKind::JsonInput),
        ("PaSs.WoRd", InputKind::RawField),
        ("pre_shared_key", InputKind::TypedField),
        ("pre-shared-key", InputKind::JsonInput),
        ("pre.shared.key", InputKind::RawField),
        ("Client.Secret", InputKind::TypedField),
        ("Refresh-Token", InputKind::JsonInput),
        ("API-Key", InputKind::RawField),
    ];
    let run_preview = |key: &str, input_kind: InputKind, show_secrets: bool| {
        let secret = format!("secret-{key}-sentinel");
        let profile = unique_profile("api-redaction-matrix");
        let mut args = vec![
            "--format=json".to_owned(),
            "--profile".to_owned(),
            profile,
            "api".to_owned(),
            "/sites/example".to_owned(),
            "--method".to_owned(),
            "POST".to_owned(),
        ];
        match input_kind {
            InputKind::RawField => args.extend(["--raw-field".into(), format!("{key}={secret}")]),
            InputKind::TypedField => {
                args.extend(["--field".into(), format!("{key}={secret}")]);
            }
            InputKind::JsonInput => {
                let mut item = serde_json::Map::new();
                item.insert(key.to_owned(), json!(secret));
                let body = json!({"envelope":{"records":[Value::Object(item)]}});
                let path = write_test_file(
                    "api-redaction-json",
                    &serde_json::to_vec(&body).expect("serialize nested secret input"),
                );
                args.extend(["--input".into(), path.to_string_lossy().into_owned()]);
            }
        }
        if show_secrets {
            args.push("--show-secrets".into());
        }
        let args = args.iter().map(String::as_str).collect::<Vec<_>>();
        (cli_without_token(&args), secret)
    };

    for (key, input_kind) in cases {
        let (output, secret) = run_preview(key, input_kind, false);
        let plan = api_preview(&output);
        assert_eq!(plan["body"], "<redacted>", "key spelling: {key}");
        for stream in [&output.stdout, &output.stderr] {
            assert!(
                !String::from_utf8_lossy(stream).contains(&secret),
                "default preview leaked {key} on one output stream"
            );
        }

        let (output, secret) = run_preview(key, input_kind, true);
        let plan = api_preview(&output);
        let exposed = match input_kind {
            InputKind::JsonInput => plan["body"]["envelope"]["records"][0][key].clone(),
            InputKind::RawField | InputKind::TypedField => plan["body"][key].clone(),
        };
        assert_eq!(exposed, json!(secret), "show-secrets value for {key}");
        for stream in [&output.stdout, &output.stderr] {
            assert!(
                String::from_utf8_lossy(stream).contains(&secret),
                "--show-secrets hid {key} on one output stream"
            );
        }
    }

    let safe_body = write_test_file("api-query-redaction-body", br#"{"safe":"value"}"#);
    let safe_body = safe_body.to_string_lossy().into_owned();
    let profile = unique_profile("api-query-redaction-default");
    let output = cli_without_token(&[
        "--format=json",
        "--profile",
        &profile,
        "api",
        "/sites/example",
        "--method",
        "POST",
        "--input",
        safe_body.as_str(),
        "--raw-field",
        "auth.Token=query-value-sentinel",
    ]);
    let plan = api_preview(&output);
    assert_eq!(plan["body"], json!({"safe":"value"}));
    assert!(plan["path"].as_str().unwrap().contains("%3Credacted%3E"));
    for stream in [&output.stdout, &output.stderr] {
        assert!(!String::from_utf8_lossy(stream).contains("query-value-sentinel"));
    }

    let profile = unique_profile("api-query-redaction-opt-in");
    let output = cli_without_token(&[
        "--format=json",
        "--profile",
        &profile,
        "api",
        "/sites/example",
        "--method",
        "POST",
        "--input",
        safe_body.as_str(),
        "--raw-field",
        "auth.Token=query-value-sentinel",
        "--show-secrets",
    ]);
    let plan = api_preview(&output);
    assert!(
        plan["path"]
            .as_str()
            .unwrap()
            .contains("query-value-sentinel")
    );
    for stream in [&output.stdout, &output.stderr] {
        assert!(String::from_utf8_lossy(stream).contains("query-value-sentinel"));
    }

    let opaque_path = write_test_file("api-opaque-body", b"opaque-body-secret-sentinel\xff");
    let opaque_path = opaque_path.to_string_lossy().into_owned();
    let profile = unique_profile("api-opaque-redaction");
    let output = cli_without_token(&[
        "--format=json",
        "--profile",
        &profile,
        "api",
        "/sites/example",
        "--method",
        "POST",
        "--input",
        opaque_path.as_str(),
    ]);
    let plan = api_preview(&output);
    assert_eq!(plan["body"], "<redacted>");
    assert!(!String::from_utf8_lossy(&output.stderr).contains("opaque-body-secret-sentinel"));
}

#[test]
fn api_local_validation_precedes_credentials_and_stdin_cannot_be_reused() {
    let profile = unique_profile("api-absolute-route");
    let output = cli_without_token(&[
        "--format=json",
        "--profile",
        &profile,
        "api",
        "https://attacker.invalid/capture",
        "--raw-field",
        "name=edge",
    ]);
    assert_config_error(&output, "relative path", &["attacker.invalid"]);

    let mut oversized = vec![b'x'; 4 * 1024 * 1024 + 1];
    let prefix = b"oversized-body-secret-sentinel";
    oversized[..prefix.len()].copy_from_slice(prefix);
    let input_path = write_test_file("api-oversized-body", &oversized);
    let input_path = input_path.to_string_lossy().into_owned();
    let profile = "poisoned\nprofile";
    let output = cli_without_token(&[
        "--format=json",
        "--profile",
        profile,
        "api",
        "/sites/example",
        "--method",
        "POST",
        "--input",
        input_path.as_str(),
    ]);
    assert_config_error(
        &output,
        "exceeded the size limit",
        &["oversized-body-secret"],
    );

    let profile = unique_profile("api-stdin-conflict");
    let output = cli_with_stdin(
        &[
            "--format=json",
            "--profile",
            &profile,
            "--token-stdin",
            "api",
            "/sites/example",
            "--method",
            "POST",
            "--apply",
            "--yes",
            "--input",
            "-",
        ],
        None,
        b"request-or-token-secret-sentinel",
    );
    assert_config_error(
        &output,
        "stdin can supply one request input",
        &["request-or-token-secret-sentinel"],
    );
}

#[test]
fn api_apply_without_yes_refuses_piped_confirmation_before_credentials() {
    for token_stdin in [false, true] {
        let profile = "poisoned\nprofile";
        let mut args = vec!["--format=json", "--profile", profile];
        if token_stdin {
            args.push("--token-stdin");
        }
        args.extend(["api", "/sites/example", "--method", "POST", "--apply"]);
        let secret = "apply-input-token-sentinel";
        let output = cli_with_stdin(&args, None, format!("{secret}\nyes\n").as_bytes());
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        let error = error_json(&output);
        assert_eq!(error["kind"], "confirmation_required");
        assert!(error["message"].as_str().unwrap().contains("--yes"));
        assert!(!String::from_utf8_lossy(&output.stderr).contains(secret));
    }
}

#[test]
fn present_invalid_environment_token_fails_instead_of_using_a_profile() {
    for (token, message) in [("api-env-secret sentinel", "invalid"), ("", TOKEN_ENV)] {
        let profile = unique_profile("api-invalid-env");
        let output = test_command(env!("CARGO_BIN_EXE_instantctl"))
            .env(TOKEN_ENV, token)
            .args([
                "--format=json",
                "--profile",
                profile.as_str(),
                "api",
                "/sites/example",
            ])
            .output()
            .expect("run instantctl with a present invalid environment token");
        assert_config_error(&output, message, &["api-env-secret", "sentinel"]);
    }
}

#[test]
fn static_sources_skip_profile_store_even_when_default_site_is_requested() {
    // Keychain rejects this profile before lookup. A static source must bypass
    // that store entirely and reach the device command's missing-site guard.
    let invalid_profile = "codex-test-invalid-profile\n";
    for stdin_source in [false, true] {
        let mut command = test_command(env!("CARGO_BIN_EXE_instantctl"));
        command.args([
            "--format=json",
            "--profile",
            invalid_profile,
            "device",
            "details",
            "AP",
        ]);
        if stdin_source {
            command
                .env_remove(TOKEN_ENV)
                .arg("--token-stdin")
                .stdin(Stdio::null());
        } else {
            command.env(TOKEN_ENV, "static-token");
        }
        let output = command.output().expect("run static-source command");
        assert_config_error(&output, "device details requires --site", &[]);

        // Also exercise the credential factory. The invalid route stops before
        // HTTP after constructing a client with the selected static token.
        let mut args = vec![
            "--format=json",
            "--profile",
            invalid_profile,
            "api",
            "/../sites",
        ];
        let output = if stdin_source {
            args.push("--token-stdin");
            cli_with_stdin(&args, None, b"static-token\n")
        } else {
            test_command(env!("CARGO_BIN_EXE_instantctl"))
                .env(TOKEN_ENV, "static-token")
                .args(&args)
                .output()
                .expect("construct static client")
        };
        assert_config_error(&output, "invalid API route", &[]);
    }
}

#[test]
fn api_stdin_token_is_bounded_and_overrides_environment() {
    let env_token = "environment-secret has spaces";
    let stdin_prefix = b"stdin-overlimit-secret-";
    let mut too_long = stdin_prefix.to_vec();
    too_long.resize(TOKEN_LIMIT + 1, b'x');
    too_long.extend_from_slice(b"\n");
    let profile = unique_profile("api-stdin-over-env");
    let output = cli_with_stdin(
        &[
            "--format=json",
            "--profile",
            &profile,
            "api",
            "/sites/example",
            "--token-stdin",
        ],
        Some(env_token),
        &too_long,
    );
    assert_config_error(
        &output,
        "exceeded the 16384-byte limit",
        &[env_token, "stdin-overlimit-secret-"],
    );
}

#[test]
fn global_stdin_source_rejects_invalid_utf8_without_leaking_input_or_environment() {
    let env_token = "environment-secret has spaces";
    let stdin_prefix = b"stdin-secret-";
    let mut token_stdin = stdin_prefix.to_vec();
    token_stdin.push(0xff);
    token_stdin.push(b'\n');
    let profile = unique_profile("api-stdin-invalid-utf8");
    let output = cli_with_stdin(
        &[
            "--format=json",
            "--profile",
            &profile,
            "--token-stdin",
            "api",
            "/sites/example",
        ],
        Some(env_token),
        &token_stdin,
    );
    assert_config_error(&output, "valid UTF-8", &[env_token, "stdin-secret-"]);
}

#[cfg(unix)]
#[test]
fn api_rejects_non_unicode_environment_token_without_panicking_or_leaking() {
    use std::os::unix::ffi::OsStrExt;

    let token = std::ffi::OsStr::from_bytes(b"non-unicode-\xff");
    let profile = unique_profile("api-nonunicode-env");
    let output = test_command(env!("CARGO_BIN_EXE_instantctl"))
        .env(TOKEN_ENV, token)
        .args([
            "--format=json",
            "--profile",
            profile.as_str(),
            "api",
            "/sites/example",
        ])
        .output()
        .expect("run instantctl");

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let error = error_json(&output);
    assert_eq!(error["kind"], "config");
    assert_eq!(error["message"], "the session token must be valid UTF-8");
    assert!(!String::from_utf8_lossy(&output.stderr).contains("non-unicode"));
}

#[cfg(unix)]
#[test]
fn closed_stdout_exits_with_broken_pipe_code_without_panicking() {
    for args in [vec!["version"], vec!["--help"], vec!["completion", "zsh"]] {
        let (reader, writer) = nix::unistd::pipe().expect("create closed-stdout pipe");
        drop(reader);

        let output = test_command(env!("CARGO_BIN_EXE_instantctl"))
            .args(&args)
            .stdout(Stdio::from(writer))
            .output()
            .expect("run instantctl with closed stdout");

        assert_eq!(output.status.code(), Some(141), "args: {args:?}");
        assert!(
            !String::from_utf8_lossy(&output.stderr).contains("panicked at"),
            "CLI panicked for {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[cfg(unix)]
#[test]
fn tty_stdout_defaults_to_a_table() {
    use std::{
        fs::File,
        io::Read,
        thread,
        time::{Duration, Instant},
    };

    let pty = nix::pty::openpty(None, None).expect("open PTY");
    // macOS can discard unread PTY data when the final slave descriptor closes.
    // Retain a parent descriptor and drain the master while the child runs.
    let slave = pty.slave;
    let mut child = test_command(env!("CARGO_BIN_EXE_instantctl"))
        .args(["version"])
        .stdin(Stdio::null())
        .stdout(Stdio::from(slave.try_clone().expect("duplicate PTY slave")))
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn instantctl attached to PTY");
    let mut master = File::from(pty.master);
    nix::fcntl::fcntl(
        &master,
        nix::fcntl::FcntlArg::F_SETFL(nix::fcntl::OFlag::O_NONBLOCK),
    )
    .expect("make PTY master nonblocking");

    let mut stderr = child.stderr.take().expect("piped child stderr");
    let stderr_reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        stderr.read_to_end(&mut bytes).expect("read child stderr");
        bytes
    });

    let deadline = Instant::now() + Duration::from_secs(10);
    let mut stdout = Vec::new();
    let mut buffer = [0; 4096];
    let mut status = None;
    let status = loop {
        loop {
            match master.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => stdout.extend_from_slice(&buffer[..read]),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(error) => panic!("read PTY output: {error}"),
            }
        }
        // Drain once more after observing exit: all child writes have completed.
        if let Some(status) = status {
            break status;
        }
        status = child.try_wait().expect("poll instantctl status");
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            let _ = stderr_reader.join();
            panic!("instantctl did not exit within 10 seconds");
        }
        thread::sleep(Duration::from_millis(10));
    };
    drop(slave);
    let stderr = stderr_reader
        .join()
        .expect("child stderr reader thread did not panic");
    assert!(
        status.success(),
        "instantctl exited with {status}; stderr: {}",
        String::from_utf8_lossy(&stderr)
    );

    let tty_stdout = String::from_utf8(stdout).expect("PTY stdout UTF-8");
    assert!(
        tty_stdout.contains("NAME"),
        "unexpected TTY output: {tty_stdout}"
    );
    assert!(
        tty_stdout.contains("VERSION"),
        "unexpected TTY output: {tty_stdout}"
    );
    assert!(!tty_stdout.trim_start().starts_with('{'));
}

#[test]
fn invalid_site_is_rejected_before_credentials_or_network() {
    let output = cli_without_token(&["--site", "not-a-uuid", "api", "/sites"]);
    assert_config_error(&output, "site identifier must be a UUID", &[]);
}

#[test]
fn client_write_verbs_parse_with_apply_and_confirmation_flags() {
    for (index, args) in [
        vec!["client", "rename", "Desk", "New Name", "--apply", "--yes"],
        vec!["client", "block", "Desk", "--force", "--apply", "--yes"],
        vec!["client", "unblock", "Desk", "--apply", "--yes"],
        vec![
            "client",
            "reserve-ip",
            "Desk",
            "192.0.2.9",
            "--network",
            "net",
            "--apply",
            "--yes",
        ],
        vec!["client", "watchlist", "add", "Desk", "--apply", "--yes"],
        vec!["client", "watchlist", "remove", "Desk", "--apply", "--yes"],
        vec!["client", "power-cycle", "Desk", "--apply", "--yes"],
        vec!["client", "tags", "set", "Desk", "staff", "--apply", "--yes"],
        vec!["client", "tags", "add", "Desk", "staff", "--apply", "--yes"],
        vec![
            "client", "tags", "remove", "Desk", "staff", "--apply", "--yes",
        ],
    ]
    .into_iter()
    .enumerate()
    {
        let profile = unique_profile(&format!("client-write-no-login-{index}"));
        let mut full = vec!["--format=json", "--profile", profile.as_str()];
        full.extend(args.iter().copied());
        let output = cli_without_token(&full);
        assert_eq!(
            output.status.code(),
            Some(3),
            "args {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_missing_profile_error(&output);
    }
}
