use serde_json::Value;
use std::{
    env, fs,
    io::Write,
    path::PathBuf,
    process::{Command, Output, Stdio},
    sync::atomic::{AtomicU64, Ordering},
};

const TOKEN_ENV: &str = "HPE_INSTANT_ON_TOKEN";

struct TestHome(PathBuf);

impl TestHome {
    fn new() -> Self {
        static NEXT_HOME: AtomicU64 = AtomicU64::new(0);
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/wlan-test-homes");
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
                        // Copy the auth CLI tests: preserve Keychain lookup, but
                        // put all CLI cache writes inside this test's worktree home.
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
        .unwrap_or("wlan-test")
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
    let profile = format!("wlan-test-{}-{test_name}", std::process::id());
    assert!(profile.len() <= 64);
    profile
}

fn cli(args: &[&str]) -> Output {
    test_command()
        .env_remove(TOKEN_ENV)
        .arg("--profile")
        .arg(test_profile())
        .args(args)
        .output()
        .expect("run instantctl")
}

fn cli_with_stdin(args: &[&str], input: &[u8]) -> Output {
    let mut command = test_command();
    command
        .env_remove(TOKEN_ENV)
        .arg("--profile")
        .arg(test_profile())
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().expect("spawn instantctl");
    child
        .stdin
        .take()
        .expect("stdin is piped")
        .write_all(input)
        .unwrap_or_else(|error| assert_eq!(error.kind(), std::io::ErrorKind::BrokenPipe));
    child.wait_with_output().expect("wait for instantctl")
}

fn error(output: &Output) -> Value {
    assert!(output.stdout.is_empty());
    serde_json::from_slice(&output.stderr).expect("stderr contains JSON error")
}

fn assert_usage_redacted(output: &Output, secrets: &[&[u8]]) {
    assert_eq!(
        output.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(error(output)["kind"], "usage");
    TEST_HOME.with(|home| {
        let cache = if cfg!(target_os = "macos") {
            home.0.join("Library/Caches")
        } else {
            home.0.join("xdg-cache")
        };
        assert!(
            !cache
                .join("instantctl")
                .join(format!("{}.lock", test_profile()))
                .exists(),
            "usage refusal must precede profile credential lookup"
        );
    });
    for secret in secrets {
        assert!(
            !output
                .stderr
                .windows(secret.len())
                .any(|window| window == *secret),
            "secret leaked to stderr"
        );
        assert!(
            !output
                .stdout
                .windows(secret.len())
                .any(|window| window == *secret),
            "secret leaked to stdout"
        );
    }
}

fn assert_missing_profile(output: &Output, secret: &[u8]) {
    assert_eq!(
        output.status.code(),
        Some(3),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value = error(output);
    assert_eq!(value["kind"], "auth");
    assert!(
        value["message"]
            .as_str()
            .unwrap()
            .contains("no saved login")
    );
    assert!(
        !output
            .stderr
            .windows(secret.len())
            .any(|window| window == secret)
    );
    assert!(
        !output
            .stdout
            .windows(secret.len())
            .any(|window| window == secret)
    );
}

#[test]
fn passphrase_stdin_accepts_lf_crlf_eof_and_hyphen_prefix_before_missing_credentials() {
    let secret = b"-StrongPass4!";
    for input in [b"-StrongPass4!\n".as_slice(), b"-StrongPass4!\r\n", secret] {
        let output = cli_with_stdin(
            &[
                "--format=json",
                "wlan",
                "passphrase",
                "Guest",
                "--passphrase-stdin",
            ],
            input,
        );
        assert_missing_profile(&output, secret);
    }
}

#[test]
fn create_and_update_read_personal_passphrases_from_stdin() {
    let secret = b"StrongPass4!";
    for args in [
        [
            "--format=json",
            "wlan",
            "create",
            "Guest",
            "--security",
            "wpa2-personal",
            "--passphrase-stdin",
            "--bands",
            "2.4,5",
        ]
        .as_slice(),
        [
            "--format=json",
            "wlan",
            "update",
            "Guest",
            "--security",
            "wpa3-personal",
            "--passphrase-stdin",
        ]
        .as_slice(),
    ] {
        let output = cli_with_stdin(args, b"StrongPass4!\n");
        assert_missing_profile(&output, secret);
    }
}

#[test]
fn invalid_passphrase_stdin_is_usage_and_never_echoes_input() {
    let cases: &[(&[u8], &[&[u8]])] = &[
        (b"\n", &[]),
        (b"", &[]),
        (b"seven!!\n", &[b"seven!!"]),
        (&[b'A'; 64], &[&[b'A'; 64]]),
        (b"Strong\xffPass4!\n", &[b"Strong", b"Pass4!"]),
    ];
    for (input, secrets) in cases {
        let output = cli_with_stdin(
            &[
                "--format=json",
                "wlan",
                "passphrase",
                "Guest",
                "--passphrase-stdin",
            ],
            input,
        );
        assert_usage_redacted(&output, secrets);
    }
}

#[test]
fn missing_passphrase_stdin_flag_refuses_non_tty_before_credentials_or_network() {
    for args in [
        &["--format=json", "wlan", "passphrase", "Guest"][..],
        &[
            "--format=json",
            "wlan",
            "create",
            "Guest",
            "--security",
            "wpa2-personal",
        ][..],
        &["--format=json", "wlan", "create", "Guest"][..],
    ] {
        let output = cli(args);
        assert_usage_redacted(&output, &[]);
    }
}

#[test]
fn both_stdin_secret_sources_and_mistyped_flags_are_usage_errors() {
    let both = cli_with_stdin(
        &[
            "--format=json",
            "--token-stdin",
            "wlan",
            "passphrase",
            "Guest",
            "--passphrase-stdin",
        ],
        b"token-value\npassphrase-value\n",
    );
    assert_usage_redacted(&both, &[b"token-value", b"passphrase-value"]);

    let mistyped = cli(&[
        "--format=json",
        "wlan",
        "passphrase",
        "Guest",
        "--psk-stdin",
    ]);
    assert_usage_redacted(&mistyped, &[]);
}

#[test]
fn argv_passphrase_styles_are_rejected_without_echoing_the_secret() {
    let sentinel = b"OldArgvPassphrase4!";
    let positional = cli(&[
        "--format=json",
        "wlan",
        "passphrase",
        "Guest",
        "OldArgvPassphrase4!",
    ]);
    assert_usage_redacted(&positional, &[sentinel]);

    let option = cli(&[
        "--format=json",
        "wlan",
        "update",
        "Guest",
        "--passphrase",
        "OldArgvPassphrase4!",
    ]);
    assert_usage_redacted(&option, &[sentinel]);
}

#[test]
fn read_verbs_and_nonsecret_mutations_still_reach_credential_loading() {
    let commands: &[&[&str]] = &[
        &["wlan", "list"],
        &["wlan", "show", "Guest"],
        &["wlan", "create", "Guest", "--security", "open"],
        &["wlan", "update", "Guest", "--enabled", "false"],
        &["wlan", "delete", "Guest", "--yes"],
        &["wlan", "enable", "Guest", "--apply", "--yes"],
        &["wlan", "disable", "Guest"],
        &["wlan", "bands", "Guest", "--bands", "2.4,5,6"],
        &[
            "wlan",
            "schedule",
            "Guest",
            "--schedule",
            "timed",
            "--days",
            "Monday,Wed",
            "--start",
            "08:00",
            "--end",
            "18:00",
        ],
        &["wlan", "bandwidth", "Guest", "--mode", "off"],
        &[
            "wlan",
            "bandwidth",
            "Guest",
            "--mode",
            "per-client",
            "--download",
            "50",
            "--upload",
            "10",
        ],
        &["wlan", "guest", "Guest", "--allow"],
        &[
            "wlan",
            "access",
            "Guest",
            "--restrict-access",
            "true",
            "--internet",
            "deny",
            "--intra-subnet-traffic",
            "allow",
            "--allowed-destination",
            "192.0.2.4",
        ],
    ];
    for command in commands {
        let mut args = vec![
            "--format=json",
            "--site",
            "11111111-2222-3333-4444-555555555555",
        ];
        args.extend(command.iter().copied());
        let output = cli(&args);
        let value = error(&output);
        assert_eq!(output.status.code(), Some(3), "{command:?}");
        assert_eq!(value["kind"], "auth", "{command:?}: {value}");
        assert!(
            value["message"]
                .as_str()
                .unwrap()
                .contains("no saved login")
        );
    }
}

#[test]
fn malformed_local_options_remain_usage_errors_before_secret_or_credential_input() {
    let commands: &[&[&str]] = &[
        &["wlan", "guest", "Guest"],
        &[
            "wlan",
            "schedule",
            "Guest",
            "--schedule",
            "timed",
            "--days",
            "mon",
            "--start",
            "08:00",
        ],
        &[
            "wlan",
            "bandwidth",
            "Guest",
            "--mode",
            "per-network",
            "--download",
            "20",
        ],
        &[
            "wlan",
            "access",
            "Guest",
            "--allowed-destination",
            "host.example",
        ],
        &["wlan", "create", "Positional", "--name", "Override"],
        &[
            "wlan",
            "create",
            "Guest",
            "--security",
            "wpa2-personal",
            "--passphrase-stdin",
            "--bands",
            "6",
        ],
    ];
    for command in commands {
        let mut args = vec!["--format=json"];
        args.extend(command.iter().copied());
        // EOF would fail secret validation if malformed options did not fail first.
        let output = cli_with_stdin(&args, b"");
        assert_usage_redacted(&output, &[]);
        assert!(
            !error(&output)["message"]
                .as_str()
                .unwrap()
                .contains("passphrase"),
            "{command:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
