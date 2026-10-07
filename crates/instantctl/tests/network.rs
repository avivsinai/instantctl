use serde_json::Value;
use std::{
    env, fs,
    path::PathBuf,
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};

const TOKEN_ENV: &str = "HPE_INSTANT_ON_TOKEN";
const SITE: &str = "11111111-2222-3333-4444-555555555555";

struct TestHome(PathBuf);

impl TestHome {
    fn new() -> Self {
        static NEXT_HOME: AtomicU64 = AtomicU64::new(0);
        let root =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/network-test-homes");
        fs::create_dir_all(&root).expect("create test home parent inside worktree");
        for _ in 0..100 {
            let path = root.join(format!(
                "{}-{}",
                std::process::id(),
                NEXT_HOME.fetch_add(1, Ordering::Relaxed)
            ));
            let mut builder = fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            match builder.create(&path) {
                Ok(()) => {
                    let home = Self(path);
                    #[cfg(target_os = "macos")]
                    {
                        // Preserve Keychain lookup while keeping CLI cache writes in this worktree.
                        let library = home.0.join("Library");
                        fs::create_dir(&library).expect("create test Library");
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
        panic!("could not allocate an isolated test home");
    }
}

impl Drop for TestHome {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).expect("remove test's isolated home");
    }
}

thread_local! {
    static TEST_HOME: TestHome = TestHome::new();
}

fn test_command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_instantctl"));
    TEST_HOME.with(|home| {
        command.env("HOME", &home.0);
        command.env("XDG_CACHE_HOME", home.0.join("xdg-cache"));
    });
    command
}

fn test_profile() -> String {
    let test_name: String = std::thread::current()
        .name()
        .unwrap_or("network-test")
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .take(32)
        .collect();
    let profile = format!("network-test-{}-{test_name}", std::process::id());
    assert!(profile.len() <= 64);
    profile
}

fn cli(args: &[&str]) -> Output {
    test_command()
        .env_remove(TOKEN_ENV)
        .arg("--format=json")
        .arg("--profile")
        .arg(test_profile())
        .arg("--site")
        .arg(SITE)
        .args(args)
        .output()
        .expect("run instantctl")
}

fn cli_with_site(site: &str, args: &[&str]) -> Output {
    test_command()
        .env_remove(TOKEN_ENV)
        .arg("--format=json")
        .arg("--profile")
        .arg(test_profile())
        .arg("--site")
        .arg(site)
        .args(args)
        .output()
        .expect("run instantctl")
}

fn error(output: &Output) -> Value {
    assert!(output.stdout.is_empty());
    serde_json::from_slice(&output.stderr).expect("stderr contains JSON error")
}

fn profile_lock_exists() -> bool {
    TEST_HOME.with(|home| {
        let cache = if cfg!(target_os = "macos") {
            home.0.join("Library/Caches")
        } else {
            home.0.join("xdg-cache")
        };
        cache
            .join("instantctl")
            .join(format!("{}.lock", test_profile()))
            .exists()
    })
}

fn assert_missing_profile(output: &Output, command: &[&str]) {
    assert_eq!(
        output.status.code(),
        Some(3),
        "{command:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value = error(output);
    assert_eq!(value["kind"], "auth", "{command:?}: {value}");
    assert!(
        value["message"]
            .as_str()
            .unwrap()
            .contains("no saved login"),
        "{command:?}: {value}"
    );
}

fn assert_usage_before_profile(output: &Output, args: &[&str]) {
    assert_eq!(
        output.status.code(),
        Some(2),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(error(output)["kind"], "usage", "{args:?}");
    assert!(
        !profile_lock_exists(),
        "local usage error must precede profile lookup: {args:?}"
    );
}

fn assert_confirmation_before_profile(output: &Output, args: &[&str]) {
    assert_eq!(
        output.status.code(),
        Some(2),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(error(output)["kind"], "confirmation_required", "{args:?}");
    assert!(
        !profile_lock_exists(),
        "confirmation refusal must precede profile lookup: {args:?}"
    );
}

#[test]
fn every_network_and_shared_service_verb_reaches_missing_profile_auth() {
    let commands: &[&[&str]] = &[
        &["network", "list"],
        &["network", "show", "Main"],
        &[
            "network",
            "create",
            "Guest",
            "--vlan",
            "3999",
            "--enabled",
            "false",
        ],
        &["network", "update", "Main", "--name", "Renamed"],
        &["network", "delete", "Main", "--yes"],
        &[
            "network",
            "dhcp",
            "Main",
            "--enabled",
            "true",
            "--gateway",
            "192.0.2.1",
            "--prefix-length",
            "24",
            "--start",
            "192.0.2.10",
            "--end",
            "192.0.2.20",
            "--domain-name",
            "lab",
            "--dns-mode",
            "custom",
            "--primary-dns",
            "192.0.2.53",
        ],
        &["network", "shared-services", "status"],
        &["network", "shared-services", "enable"],
        &["network", "shared-services", "disable"],
        &["network", "shared-services", "list", "Main"],
        &["network", "shared-services", "share", "Main", "Printer"],
        &["network", "shared-services", "unshare", "Main", "Printer"],
    ];
    for command in commands {
        assert_missing_profile(&cli(command), command);
    }
}

#[test]
fn applied_mutations_without_yes_refuse_before_profile_lookup() {
    let commands: &[&[&str]] = &[
        &["network", "create", "Guest", "--vlan", "3999", "--apply"],
        &["network", "update", "Main", "--enabled", "false", "--apply"],
        &["network", "delete", "Main", "--apply"],
        &["network", "dhcp", "Main", "--enabled", "false", "--apply"],
        &["network", "shared-services", "enable", "--apply"],
        &["network", "shared-services", "disable", "--apply"],
        &[
            "network",
            "shared-services",
            "share",
            "Main",
            "Printer",
            "--apply",
        ],
        &[
            "network",
            "shared-services",
            "unshare",
            "Main",
            "Printer",
            "--apply",
        ],
    ];
    for command in commands {
        assert_confirmation_before_profile(&cli(command), command);
    }
}

#[test]
fn malformed_flags_and_types_are_usage_before_profile_lookup() {
    let cases: &[&[&str]] = &[
        &["network", "list", "--unknown"],
        &["network", "create", "Guest", "--vlan", "not-a-number"],
        &[
            "network", "create", "Guest", "--vlan", "3999", "--type", "other",
        ],
        &["network", "update", "Main", "--enabled", "perhaps"],
        &["network", "dhcp", "Main", "--gateway", "not-an-ip"],
        &["network", "dhcp", "Main", "--dns-mode", "manual"],
        &["network", "create"],
        &[
            "network",
            "create",
            "Positional",
            "--vlan",
            "3999",
            "--name",
            "Override",
        ],
        &["network", "update", "Main"],
        &["network", "dhcp", "Main"],
        &[
            "network",
            "dhcp",
            "Main",
            "--gateway",
            "192.0.2.1",
            "--prefix-length",
            "33",
        ],
        &[
            "network",
            "dhcp",
            "Main",
            "--enabled",
            "true",
            "--gateway",
            "192.0.2.1",
            "--prefix-length",
            "24",
            "--start",
            "192.0.2.20",
            "--end",
            "192.0.2.10",
        ],
    ];
    for args in cases {
        assert_usage_before_profile(&cli(args), args);
    }

    let control_name = "Guest\nInjected";
    let args = ["network", "create", control_name, "--vlan", "3999"];
    assert_usage_before_profile(&cli(&args), &args);
}

#[test]
fn invalid_vlan_ids_are_usage_before_profile_lookup() {
    for vlan in [0, 1, 4093].into_iter().chain(3333..=3349) {
        let vlan = vlan.to_string();
        let args = ["network", "create", "Guest", "--vlan", vlan.as_str()];
        assert_usage_before_profile(&cli(&args), &args);
    }
}

#[test]
fn invalid_site_is_config_error_before_profile_lookup() {
    let output = cli_with_site("not-a-uuid", &["network", "list"]);
    assert_eq!(
        output.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(error(&output)["kind"], "config");
    assert!(
        !profile_lock_exists(),
        "invalid site must be rejected before profile lookup"
    );
}
