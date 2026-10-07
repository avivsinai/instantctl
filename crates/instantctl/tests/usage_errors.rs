use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::atomic::{AtomicU64, Ordering},
};

const TOKEN_ENV: &str = "HPE_INSTANT_ON_TOKEN";

struct TestHome(PathBuf);

impl TestHome {
    fn new() -> Self {
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);

        for _ in 0..100 {
            let target = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target");
            let path = target.join(format!(
                "instantctl-usage-test-{}-{}",
                std::process::id(),
                NEXT_ID.fetch_add(1, Ordering::Relaxed)
            ));
            let mut builder = fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            match builder.create(&path) {
                Ok(()) => return Self(path),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("create isolated test home: {error}"),
            }
        }
        panic!("could not allocate a unique test home");
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn cleanup(self) {
        fs::remove_dir_all(&self.0).expect("remove isolated test home");
    }
}

impl Drop for TestHome {
    fn drop(&mut self) {
        // Keep failure paths tidy. Normal completion calls cleanup(), which
        // checks that the isolated home was removed successfully.
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn cli(args: &[&str]) -> Output {
    cli_with_input_file(args, None)
}

fn cli_with_stdin_fixture(args: &[&str], input: &[u8]) -> Output {
    cli_with_input_file(args, Some(input))
}

fn cli_with_input_file(args: &[&str], input: Option<&[u8]>) -> Output {
    let home = TestHome::new();
    let stdin_path = input.map(|input| {
        let path = home.path().join("stdin.input");
        fs::write(&path, input).expect("write isolated stdin fixture");
        path
    });
    let mut command = Command::new(env!("CARGO_BIN_EXE_instantctl"));
    command
        .env("HOME", home.path())
        .env(TOKEN_ENV, "")
        .env("XDG_CONFIG_HOME", home.path().join("xdg-config"))
        .env("XDG_CACHE_HOME", home.path().join("xdg-cache"))
        // If a future parser regression reaches a request, fail locally without
        // contacting the network. A valid usage diagnostic proves it did not.
        .env("HTTP_PROXY", "http://127.0.0.1:9")
        .env("HTTPS_PROXY", "http://127.0.0.1:9")
        .env("ALL_PROXY", "http://127.0.0.1:9")
        .env("http_proxy", "http://127.0.0.1:9")
        .env("https_proxy", "http://127.0.0.1:9")
        .env("all_proxy", "http://127.0.0.1:9")
        .env("NO_PROXY", "")
        .env("no_proxy", "")
        .args(args);
    if let Some(path) = &stdin_path {
        command.stdin(Stdio::from(
            fs::File::open(path).expect("open isolated stdin fixture"),
        ));
    } else {
        command.stdin(Stdio::null());
    }
    let output = command.output().expect("run instantctl");
    if let Some(path) = stdin_path {
        fs::remove_file(path).expect("remove isolated stdin fixture");
    }
    home.cleanup();
    output
}

fn json_error(output: &Output) -> Value {
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    serde_json::from_slice(&output.stderr).expect("stderr contains a JSON error")
}

fn assert_usage(output: &Output) -> Value {
    let error = json_error(output);
    assert_eq!(error["kind"], "usage", "unexpected error: {error}");
    assert!(
        error["message"].is_string(),
        "missing error message: {error}"
    );
    assert!(error["fields"].is_object(), "missing usage fields: {error}");
    error
}

fn assert_known_value_error(error: &Value, argument: &str, value: &str, allowed_values: &[&str]) {
    assert_eq!(error["fields"]["argument"], argument, "{error}");
    assert_eq!(error["fields"]["value"], value, "{error}");
    assert_eq!(
        error["fields"]["allowed_values"],
        json!(allowed_values),
        "{error}"
    );
    let message = error["message"].as_str().unwrap();
    assert!(
        message.contains(argument),
        "missing {argument:?}: {message}"
    );
    assert!(message.contains(value), "missing {value:?}: {message}");
}

fn assert_json_and_table_usage(
    args: &[&str],
    argument: &str,
    value: &str,
    allowed_values: &[&str],
    expected_fragments: &[&str],
) -> Value {
    let mut json_args = vec!["--format=json"];
    json_args.extend(args);
    let error = assert_usage(&cli(&json_args));
    assert_eq!(error["fields"]["argument"], argument, "{error}");
    assert_eq!(error["fields"]["value"], value, "{error}");
    assert_eq!(
        error["fields"]["allowed_values"],
        json!(allowed_values),
        "{error}"
    );
    let expected = error["fields"]["expected"].as_str().unwrap();
    let message = error["message"].as_str().unwrap();
    for fragment in expected_fragments {
        assert!(
            expected.contains(fragment),
            "missing {fragment:?}: {expected}"
        );
        assert!(
            message.contains(fragment),
            "missing {fragment:?}: {message}"
        );
    }
    assert!(
        message.contains(argument),
        "missing {argument:?}: {message}"
    );
    assert!(message.contains(value), "missing {value:?}: {message}");

    let mut table_args = vec!["--format=table"];
    table_args.extend(args);
    let table = cli(&table_args);
    assert_eq!(table.status.code(), Some(2));
    assert!(table.stdout.is_empty());
    let table = String::from_utf8(table.stderr).expect("table diagnostic is UTF-8");
    assert!(table.contains(message), "table message differs: {table}");
    for fragment in [argument, value]
        .into_iter()
        .chain(expected_fragments.iter().copied())
    {
        assert!(
            table.contains(fragment),
            "table omitted {fragment:?}: {table}"
        );
    }
    error
}

#[test]
fn clap_value_enum_errors_keep_allowed_values_in_json_yaml_and_table() {
    let allowed = ["json", "yaml", "table"];
    let output = cli(&["--format", "toml", "version"]);
    let error = assert_usage(&output);
    assert_known_value_error(&error, "--format <FORMAT>", "toml", &allowed);
    assert_expected_values(&error, &allowed);

    let band_allowed = ["2.4ghz", "5ghz", "6ghz"];
    let band_args = ["radio", "plan", "set", "--band", "7ghz"];
    let band_error = assert_usage(&cli(&band_args));
    assert_known_value_error(&band_error, "--band <BAND>", "7ghz", &band_allowed);

    let mut yaml_args = vec!["--format=yaml"];
    yaml_args.extend(band_args);
    let yaml = cli(&yaml_args);
    assert_eq!(yaml.status.code(), Some(2));
    assert!(yaml.stdout.is_empty());
    let yaml = String::from_utf8(yaml.stderr).expect("YAML diagnostic is UTF-8");
    for detail in [
        "kind: usage",
        "argument: --band <BAND>",
        "value: 7ghz",
        "allowed_values:",
        "2.4ghz",
        "5ghz",
        "6ghz",
    ] {
        assert!(yaml.contains(detail), "missing {detail:?} in YAML: {yaml}");
    }

    let mut table_args = vec!["--format=table"];
    table_args.extend(band_args);
    let table = cli(&table_args);
    assert_eq!(table.status.code(), Some(2));
    assert!(table.stdout.is_empty());
    let table = String::from_utf8(table.stderr).expect("table diagnostic is UTF-8");
    assert!(
        table.contains(band_error["message"].as_str().unwrap()),
        "table error differs from JSON message: {table}"
    );
}

fn assert_expected_values(error: &Value, values: &[&str]) {
    let expected = error["fields"]["expected"].as_str().unwrap();
    for value in values {
        assert!(
            expected.contains(value),
            "expected metadata omitted {value:?}: {expected}"
        );
    }
}

#[test]
fn radio_api_enum_errors_name_declared_arguments_and_values() {
    let cases: &[(&[&str], &str, &str, &[&str])] = &[
        (
            &[
                "radio", "override", "set", "AP", "--band", "5ghz", "--width", "90mhz",
            ],
            "--width <WIDTH>",
            "90mhz",
            &["20mhz", "40mhz", "80mhz", "160mhz", "320mhz"],
        ),
        (
            &["radio", "plan", "set", "--band-mapping", "invalid"],
            "--band-mapping <BAND_MAPPING>",
            "invalid",
            &["2.4ghz_and_5ghz", "2.4ghz_and_6ghz", "5ghz_and_6ghz"],
        ),
        (
            &[
                "radio",
                "plan",
                "set",
                "--band",
                "5ghz",
                "--min-power",
                "7dbm",
            ],
            "--min-power <MIN_POWER>",
            "7dbm",
            &[
                "6dbm",
                "9dbm",
                "12dbm",
                "15dbm",
                "18dbm",
                "21dbm",
                "24dbm",
                "27dbm",
                "30dbm",
                "33dbm",
                "regulatoryMax",
            ],
        ),
        (
            &["radio", "plan", "set", "--band", "7ghz"],
            "--band <BAND>",
            "7ghz",
            &["2.4ghz", "5ghz", "6ghz"],
        ),
        (
            &["wlan", "bandwidth", "example", "--mode", "invalid"],
            "--mode <MODE>",
            "invalid",
            &["off", "per-client", "per-network"],
        ),
    ];

    for (args, argument, value, allowed) in cases {
        assert_json_and_table_usage(args, argument, value, allowed, allowed);
    }
}

#[test]
fn invalid_ipv4_address_reports_the_positional_argument_and_octet_range() {
    assert_json_and_table_usage(
        &[
            "device",
            "reserve-ip",
            "AP",
            "--network",
            "LAN",
            "192.0.2.300",
        ],
        "<ADDRESS>",
        "192.0.2.300",
        &[],
        &["four decimal octets", "0..255"],
    );
}

#[test]
fn numeric_range_usage_keeps_the_parser_range() {
    assert_json_and_table_usage(
        &["--timeout", "0", "version"],
        "--timeout <TIMEOUT>",
        "0",
        &[],
        &["0 is not in 1..18446744073709551615"],
    );
}

fn assert_no_secret(output: &Output, secret: &str) {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stdout.contains(secret),
        "secret leaked to stdout: {stdout}"
    );
    assert!(
        !stderr.contains(secret),
        "secret leaked to stderr: {stderr}"
    );
}

fn assert_redacted_usage_in_all_formats(args: &[&str], secrets: &[&str]) -> Value {
    let mut json_args = vec!["--format=json"];
    json_args.extend(args);
    let json_output = cli(&json_args);
    let error = assert_usage(&json_output);
    for secret in secrets {
        assert_no_secret(&json_output, secret);
    }

    for format in ["yaml", "table"] {
        let mut formatted_args = vec![if format == "yaml" {
            "--format=yaml"
        } else {
            "--format=table"
        }];
        formatted_args.extend(args);
        let output = cli(&formatted_args);
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        for secret in secrets {
            assert_no_secret(&output, secret);
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        if format == "table" {
            assert!(
                stderr.contains(error["message"].as_str().unwrap()),
                "table message differs from JSON: {stderr}"
            );
        } else {
            assert!(stderr.contains("kind: usage"), "unexpected YAML: {stderr}");
        }
    }
    error
}

#[test]
fn unknown_secret_bearing_flags_and_stray_positionals_are_generic_and_redacted() {
    let cases: &[(&[&str], &[&str])] = &[
        (
            &[
                "wlan",
                "create",
                "example",
                "--passphrase",
                "legacy-passphrase-secret",
                "--psk-stdin",
            ],
            &["legacy-passphrase-secret"],
        ),
        (
            &[
                "wlan",
                "create",
                "example",
                "old-positional-passphrase-secret",
                "--unknown-option",
            ],
            &["old-positional-passphrase-secret"],
        ),
        (
            &[
                "--token-stdin",
                "stray-token-stdin-secret",
                "--unknown-option",
                "auth",
                "status",
            ],
            &["stray-token-stdin-secret"],
        ),
    ];
    for (args, secrets) in cases {
        let error = assert_redacted_usage_in_all_formats(args, secrets);
        assert_eq!(error["fields"]["value"], Value::Null, "{error}");
    }
}

#[test]
fn secret_attached_to_a_known_stdin_flag_is_redacted_even_with_a_later_typo() {
    let secret = "attached-passphrase-secret";
    let error = assert_redacted_usage_in_all_formats(
        &[
            "wlan",
            "update",
            "example",
            "--passphrase-stdin=attached-passphrase-secret",
            "--passphras-stdin",
        ],
        &[secret],
    );
    assert_eq!(error["fields"]["argument"], "--passphrase-stdin");
    assert_eq!(error["fields"]["value"], "[REDACTED]");
    assert!(error["message"].as_str().unwrap().contains("[REDACTED]"));
}

#[test]
fn passphrase_stdin_with_a_later_typo_never_echoes_stdin_secret() {
    let secret = "stdin-passphrase-secret";
    let formats = ["--format=json", "--format=yaml", "--format=table"];
    for format in formats {
        let output = cli_with_stdin_fixture(
            &[
                format,
                "wlan",
                "passphrase",
                "example",
                "--passphrase-stdin",
                "--psk-stdin",
            ],
            secret.as_bytes(),
        );
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        assert!(!output.stderr.is_empty());
        assert_no_secret(&output, secret);
        if format == "--format=json" {
            assert_eq!(assert_usage(&output)["kind"], "usage");
        } else if format == "--format=yaml" {
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(stderr.contains("kind: usage"), "unexpected YAML: {stderr}");
        }
    }
}

#[test]
fn known_boolean_value_error_survives_secret_api_field_redaction() {
    let secret = "api-field-token-secret";
    let error = assert_redacted_usage_in_all_formats(
        &[
            "api",
            "/sites/example",
            "-f",
            "token=api-field-token-secret",
            "--yes=invalid",
        ],
        &[secret],
    );
    assert_eq!(error["fields"]["argument"], "--yes");
    assert_eq!(error["fields"]["value"], "invalid");
    assert_eq!(
        error["fields"]["expected"], "a flag without a value",
        "{error}"
    );
    assert!(
        error["message"]
            .as_str()
            .unwrap()
            .contains("invalid value \"invalid\" for \"--yes\"; expected: a flag without a value"),
        "known argument value detail was lost: {error}"
    );
}
