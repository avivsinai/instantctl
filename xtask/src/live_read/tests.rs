use super::*;

use clap::{CommandFactory, Parser};
use instantctl::cli::Cli;
use std::{
    fs,
    path::{Path, PathBuf},
    process,
    sync::atomic::{AtomicU64, Ordering},
};

const SITE_ID: &str = "123e4567-e89b-12d3-a456-426614174000";
const FIRST_MAC: &str = "aa:bb:cc:dd:ee:01";
const FIRST_SWITCH_MAC: &str = "aa:bb:cc:dd:ee:02";
const SECOND_SWITCH_MAC: &str = "aa:bb:cc:dd:ee:03";
const TOKEN_SENTINEL: &str = "nested-token-sentinel";
const SHARED_SECRET_SENTINEL: &str = "nested-shared-secret-sentinel";
const STDERR_SENTINEL: &str = "stderr-only-sentinel";

static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "xtask-live-read-{}-{}",
            process::id(),
            NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed),
        ));
        fs::create_dir(&path).expect("create isolated live-read test directory");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700))
                .expect("restrict isolated test directory permissions");
        }
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[derive(Clone, Copy)]
enum StubBehavior {
    Success,
    EmptyCollections,
    MissingDefaultSite,
    Allowlist(AllowlistFixture),
    Fault {
        command: &'static str,
        response: &'static str,
    },
}

#[derive(Clone, Copy, Debug)]
enum AllowlistFixture {
    FirstAllowed,
    FirstAllowedThenMalformedLater,
    FirstForbiddenThenAllowed,
    AllForbidden,
    AllForbiddenCompleteCensus,
    AllForbiddenUnknownCensus,
    AllForbiddenMalformedCensus,
    UnknownStatus,
    MissingStatus,
    MalformedStatus,
    DetailsReadFailure,
    ShowIdentityMismatch,
    DetailsIdentityMismatch,
    DuplicatePortMapping,
}

fn stub_binary(directory: &TestDirectory, behavior: StubBehavior) -> (PathBuf, PathBuf) {
    let binary = directory.path().join("instantctl-stub");
    let calls = directory.path().join("calls.txt");
    let calls_literal = shell_literal(&calls);
    let profile_response = match behavior {
        StubBehavior::MissingDefaultSite => r#"{"default_site":null}"#,
        _ => r#"{"default_site":"123e4567-e89b-12d3-a456-426614174000"}"#,
    };
    let version_sha = instantctl::version::GIT_SHA;
    let device_list = serde_json::json!({"elements":[
        {"id":"device-ap","mac":FIRST_MAC,"name":"First AP","token":TOKEN_SENTINEL,"sharedSecret":SHARED_SECRET_SENTINEL},
        {"id":"device-switch-a","mac":FIRST_SWITCH_MAC,"name":"Switch A"},
        {"id":"device-switch-b","mac":SECOND_SWITCH_MAC,"name":"Switch B"},
    ]})
    .to_string();
    let mut script = format!(
        "#!/bin/sh\nset -eu\nprintf '%s\\n' \"$*\" >> {calls_literal}\nprintf '%s\\n' '{STDERR_SENTINEL}' >&2\ncase \"$*\" in\n  *' profile show'*) printf '%s\\n' '{profile_response}'; exit 0 ;;\n  *' device list'*) printf '%s\\n' '{device_list}'; exit 0 ;;\n"
    );
    if let StubBehavior::Fault { command, response } = behavior {
        let response = match response {
            "exit" => "printf '%s\\n' '{\"complete\":true}'; exit 7",
            "invalid_json" => "printf '%s\\n' 'not-json'; exit 0",
            "incomplete" => "printf '%s\\n' '{\"complete\":false,\"elements\":[]}'; exit 0",
            "malformed" => "printf '%s\\n' '{\"complete\":true,\"items\":[]}'; exit 0",
            "empty_unknown" => "printf '%s\\n' '{\"elements\":[]}'; exit 0",
            "unusable" => {
                "printf '%s\\n' '{\"complete\":true,\"elements\":[{\"id\":null}]}'; exit 0"
            }
            "version_mismatch" => "printf '%s\\n' '{\"git_sha\":\"intentionally-stale\"}'; exit 0",
            "address_name" => "printf '%s\\n' '[{\"device\":\"192.0.2.10\"}]'; exit 0",
            "mac_name" => "printf '%s\\n' '[{\"device\":\"aa:bb:cc:dd:ee:03\"}]'; exit 0",
            _ => unreachable!("test uses a known stub response"),
        };
        script.push_str(&format!("  *' {command}'*) {response} ;;\n"));
    }
    if matches!(behavior, StubBehavior::EmptyCollections) {
        for command in [
            "stack list",
            "schedule list",
            "radius list",
            "policy list",
            "port profile list",
            "port list",
        ] {
            script.push_str(&format!(
                "  *' {command}'*) printf '%s\\n' '[]'; exit 0 ;;\n"
            ));
        }
    }
    let fixture = match behavior {
        StubBehavior::Allowlist(fixture) => fixture,
        _ => AllowlistFixture::FirstAllowed,
    };
    let (first_status, second_status, detail_failure) = match fixture {
        AllowlistFixture::FirstAllowed => (Some("allowed"), Some("allowed"), false),
        AllowlistFixture::FirstAllowedThenMalformedLater => {
            (Some("allowed"), Some("allowed"), false)
        }
        AllowlistFixture::FirstForbiddenThenAllowed => (Some("forbidden"), Some("allowed"), false),
        AllowlistFixture::AllForbidden
        | AllowlistFixture::AllForbiddenCompleteCensus
        | AllowlistFixture::AllForbiddenUnknownCensus
        | AllowlistFixture::AllForbiddenMalformedCensus => {
            (Some("forbidden"), Some("forbidden"), false)
        }
        AllowlistFixture::UnknownStatus => (Some("unrecognized"), Some("allowed"), false),
        AllowlistFixture::MissingStatus => (None, Some("allowed"), false),
        AllowlistFixture::MalformedStatus => (None, Some("allowed"), false),
        AllowlistFixture::DetailsReadFailure => (Some("allowed"), Some("allowed"), true),
        AllowlistFixture::ShowIdentityMismatch => (Some("allowed"), Some("allowed"), false),
        AllowlistFixture::DetailsIdentityMismatch => (Some("allowed"), Some("allowed"), false),
        AllowlistFixture::DuplicatePortMapping => (Some("allowed"), Some("allowed"), false),
    };
    let first_details = match fixture {
        AllowlistFixture::MalformedStatus => {
            serde_json::json!({"id":"device-switch-a","kind":"switch","allowListByPortNumber":[]})
        }
        AllowlistFixture::MissingStatus => serde_json::json!({
            "id":"device-switch-a","kind":"switch","allowListByPortNumber":{}
        }),
        AllowlistFixture::DetailsIdentityMismatch => {
            device_details("wrong-device-id", 101, first_status)
        }
        _ => device_details("device-switch-a", 101, first_status),
    }
    .to_string();
    let first_show = if matches!(fixture, AllowlistFixture::DuplicatePortMapping) {
        serde_json::json!({
            "id":"device-switch-a",
            "mac":FIRST_SWITCH_MAC,
            "ethernet_ports":[
                {"faceplatePortNumber":1,"portNumber":101},
                {"faceplatePortNumber":1,"portNumber":102},
            ],
            "trunk_ports":[],
        })
    } else {
        device_show(
            "device-switch-a",
            if matches!(fixture, AllowlistFixture::ShowIdentityMismatch) {
                "aa:bb:cc:dd:ee:99"
            } else {
                FIRST_SWITCH_MAC
            },
            1,
            101,
        )
    }
    .to_string();
    let second_details = device_details("device-switch-b", 202, second_status).to_string();
    let second_show = device_show("device-switch-b", SECOND_SWITCH_MAC, 2, 202).to_string();
    let ap_show = device_show("device-ap", FIRST_MAC, 0, 0).to_string();
    let ap_details = device_details("device-ap", 0, Some("forbidden")).to_string();
    let port_list = if matches!(fixture, AllowlistFixture::FirstAllowedThenMalformedLater) {
        r#"[{"Device":"First AP","Port":0},{"Device":"Switch A","Port":1},{"Device":null,"Port":"malformed"}]"#
    } else {
        r#"[{"Device":"First AP","Port":0},{"Device":"Switch A","Port":1},{"Device":"Switch B","Port":2}]"#
    };
    let port_list = match fixture {
        AllowlistFixture::AllForbiddenCompleteCensus => {
            format!(r#"{{"complete":true,"elements":{port_list}}}"#)
        }
        AllowlistFixture::AllForbiddenUnknownCensus => {
            format!(r#"{{"elements":{port_list}}}"#)
        }
        AllowlistFixture::AllForbiddenMalformedCensus => {
            format!(r#"{{"complete":"true","elements":{port_list}}}"#)
        }
        _ => port_list.to_owned(),
    };
    script.push_str(&format!(
        "  *' device details {FIRST_MAC}'*) printf '%s\\n' '{ap_details}'; exit 0 ;;\n  *' device show {FIRST_MAC}'*) printf '%s\\n' '{ap_show}'; exit 0 ;;\n  *' device show {FIRST_SWITCH_MAC}'*) printf '%s\\n' '{first_show}'; exit 0 ;;\n  *' device show {SECOND_SWITCH_MAC}'*) printf '%s\\n' '{second_show}'; exit 0 ;;\n  *' device details {FIRST_SWITCH_MAC}'*) {} ;;\n  *' device details {SECOND_SWITCH_MAC}'*) printf '%s\\n' '{second_details}'; exit 0 ;;\n  *' port show {FIRST_SWITCH_MAC} 1'*) printf '%s\\n' '{{\"device\":{{\"id\":\"device-switch-a\"}},\"port\":{{\"port_number\":1,\"api_port_number\":101}}}}'; exit 0 ;;\n  *' port show {SECOND_SWITCH_MAC} 2'*) printf '%s\\n' '{{\"device\":{{\"id\":\"device-switch-b\"}},\"port\":{{\"port_number\":2,\"api_port_number\":202}}}}'; exit 0 ;;\n  *' port list'*) printf '%s\\n' '{port_list}'; exit 0 ;;\n  *' radio list'*) printf '%s\\n' '[{{\"device\":\"First AP\"}}]'; exit 0 ;;\n  *' admin list'*) printf '%s\\n' '{{\"accounts\":[{{\"email\":\"runner@example.test\"}}]}}'; exit 0 ;;\n",
        if detail_failure {
            "printf '%s\\n' 'detail read failed' >&2; exit 7".to_owned()
        } else {
            format!("printf '%s\\n' '{first_details}'; exit 0")
        }
    ));
    script.push_str(&format!(
        "  *' version'*) printf '%s\\n' '{{\"git_sha\":\"{version_sha}\"}}'; exit 0 ;;\n  *) printf '%s\\n' '{{\"complete\":true,\"elements\":[{{\"id\":\"opaque-subject\",\"mac\":\"aa:bb:cc:dd:ee:03\",\"device\":\"aa:bb:cc:dd:ee:03\",\"Device\":\"aa:bb:cc:dd:ee:03\",\"Port\":\"3\",\"username\":\"offline-user\"}}]}}' ;;\nesac\n",
    ));
    fs::write(&binary, script).expect("write private executable test stub");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o700))
            .expect("make test stub executable");
    }
    (binary, calls)
}

fn device_details(id: &str, api_port: u64, status: Option<&str>) -> serde_json::Value {
    let entries = status.map_or_else(serde_json::Map::new, |status| {
        serde_json::Map::from_iter([(
            api_port.to_string(),
            serde_json::json!({"id":format!("allow-list-{id}"),"allowListState":status}),
        )])
    });
    serde_json::json!({
        "id":id,
        "kind":"switch",
        "allowListByPortNumber":entries,
        "allowListByTrunkNumber":{},
    })
}

fn device_show(id: &str, mac: &str, faceplate: u64, api_port: u64) -> serde_json::Value {
    serde_json::json!({
        "id":id,
        "mac":mac,
        "ethernet_ports":[{
            "faceplatePortNumber":faceplate,
            "portNumber":api_port,
            "isLinkUp":true,
        }],
        "trunk_ports":[],
    })
}

fn shell_literal(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
}

fn args(binary: PathBuf, output: PathBuf) -> Args {
    Args {
        binary,
        output,
        profile: "default".into(),
        timeout: 15,
    }
}

fn calls(path: &Path) -> Vec<String> {
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

#[test]
fn current_cli_tree_is_fully_classified_and_rejects_new_leaves() {
    let command = Cli::command();
    let mut paths = Vec::new();
    leaves(&command, "", &mut paths);
    assert!(!paths.is_empty());
    for path in &paths {
        assert!(
            ENTRIES.iter().any(|entry| entry.path == path),
            "current CLI leaf lacks a read/exclude classification: {path}"
        );
    }
    let read_plan = plan(&command).expect("all current CLI leaves are classified");
    validate_plan(&read_plan).expect("the generated read plan matches the catalog");
    // A fake binary would accept a mistyped recipe. Check the linked real parser
    // as well; pending feature leaves deliberately remain unsupported on main.
    for invocation in read_plan
        .iter()
        .filter(|invocation| paths.contains(&invocation.path))
    {
        let mut argv = vec!["instantctl".to_owned(), "--format".into(), "json".into()];
        argv.extend(invocation.path.split_whitespace().map(str::to_owned));
        argv.extend(invocation.arguments.iter().map(|argument| match argument {
            Arg::Literal(value) => (*value).to_owned(),
            Arg::First { field: "Port", .. } => "1".into(),
            Arg::First {
                field: "/accounts/0/email",
                ..
            } => "runner@example.test".into(),
            Arg::First { .. } => SITE_ID.into(),
        }));
        Cli::try_parse_from(argv).unwrap_or_else(|error| {
            panic!("read recipe does not match {}: {error}", invocation.path)
        });
    }

    let command_with_new_leaf = command.subcommand(clap::Command::new("unreviewed-leaf"));
    let error = plan(&command_with_new_leaf)
        .err()
        .expect("new command leaves must fail closed until classified");
    assert!(
        error
            .to_string()
            .contains("unclassified command: unreviewed-leaf")
    );
}

#[test]
fn validate_rejects_mutating_recipes_before_any_process_starts() {
    let valid = plan(&Cli::command()).expect("current catalog plan");
    let mut excluded_path = valid.clone();
    excluded_path.push(Invocation {
        path: "site delete".into(),
        arguments: Vec::new(),
    });

    let mut raw_post = valid.clone();
    let api = raw_post
        .iter_mut()
        .find(|invocation| invocation.path == "api")
        .expect("allowlisted raw API read exists");
    api.arguments.extend([
        Arg::Literal("-X"),
        Arg::Literal("POST"),
        Arg::Literal("--apply"),
    ]);

    let mut mixed_mutation = valid.clone();
    let routing = mixed_mutation
        .iter_mut()
        .find(|invocation| invocation.path == "network routing")
        .expect("mixed read/update command exists");
    routing.arguments.extend([
        Arg::Literal("--enabled"),
        Arg::Literal("true"),
        Arg::Literal("--apply"),
    ]);

    for (case, invalid) in [
        ("excluded mutating leaf", excluded_path),
        ("raw API POST", raw_post),
        ("mixed routing update", mixed_mutation),
    ] {
        let directory = TestDirectory::new();
        let (binary, marker) = stub_binary(&directory, StubBehavior::Success);
        let error = execute_plan(&args(binary, directory.path().join("unused.json")), invalid)
            .err()
            .unwrap_or_else(|| panic!("{case} must fail plan validation"));
        assert!(
            error.to_string().contains("refused before execution")
                || error.to_string().contains("arguments differ"),
            "{case}: {error:#}"
        );
        assert!(!marker.exists(), "{case} spawned the stub before refusal");
    }
}

#[test]
fn executes_current_first_device_read_safely_and_records_selection() {
    let directory = TestDirectory::new();
    let (binary, marker) = stub_binary(&directory, StubBehavior::Success);
    let read_plan = plan(&Cli::command()).expect("current read catalog");
    let report = execute_plan(
        &args(binary.clone(), directory.path().join("unused.json")),
        read_plan,
    )
    .expect("stubbed current read plan");
    assert!(!report.failed());
    assert_eq!(report.command_tree_git_sha, instantctl::version::GIT_SHA);
    assert_eq!(report.site_id.as_deref(), Some(SITE_ID));

    let device_list = report
        .results
        .iter()
        .find(|result| result.command == "device list")
        .expect("device list result");
    assert_eq!(device_list.count, Some(3));
    assert_eq!(
        device_list.complete, None,
        "unknown completeness stays unknown"
    );
    assert_eq!(device_list.sample["elements"][0]["token"], "<redacted>");
    assert_eq!(
        device_list.sample["elements"][0]["sharedSecret"],
        "<redacted>"
    );

    let device_show = report
        .results
        .iter()
        .find(|result| result.command == "device show")
        .expect("device show result");
    assert!(device_show.executed);
    assert_eq!(device_show.selection.len(), 1);
    assert_eq!(device_show.selection[0].source, "device list");
    assert_eq!(device_show.selection[0].row_index, 0);
    assert_eq!(device_show.selection[0].field, "mac");
    assert_eq!(device_show.selection[0].value, FIRST_MAC);
    let port_show = report
        .results
        .iter()
        .find(|result| result.command == "port show")
        .unwrap();
    assert!(port_show.executed);
    assert_eq!(port_show.selection.len(), 2);
    assert_eq!(port_show.selection[0].source, "port list");
    assert_eq!(port_show.selection[0].row_index, 1);
    assert_eq!(port_show.selection[0].value, "Switch A");
    assert_eq!(port_show.selection[1].value, 1);

    let power_usage = report
        .results
        .iter()
        .find(|result| result.command == "device power-usage")
        .unwrap();
    assert!(power_usage.executed);
    assert_eq!(power_usage.selection.len(), 1);
    assert_eq!(power_usage.selection[0].source, "port list");
    assert_eq!(power_usage.selection[0].field, "Device");
    assert_eq!(power_usage.selection[0].row_index, 1);
    assert_eq!(power_usage.selection[0].value, "Switch A");
    assert!(power_usage.argv.ends_with(&[
        "device".into(),
        "power-usage".into(),
        "Switch A".into()
    ]));

    let allowlist = report
        .results
        .iter()
        .find(|result| result.command == "device allowlist")
        .unwrap();
    assert!(allowlist.executed);
    assert_eq!(allowlist.selection.len(), 8);
    assert_eq!(allowlist.eligibility, None);
    assert!(allowlist.selection.iter().any(|selection| {
        selection.source == "port list"
            && selection.field == "Device"
            && selection.row_index == 1
            && selection.value == "Switch A"
    }));
    assert!(allowlist.selection.iter().any(|selection| {
        selection.source == "device show"
            && selection.field == "ethernet_ports.faceplatePortNumber"
            && selection.value == 1
    }));
    assert!(allowlist.selection.iter().any(|selection| {
        selection.source == "device show"
            && selection.field == "ethernet_ports.portNumber"
            && selection.value == 101
    }));
    assert!(allowlist.selection.iter().any(|selection| {
        selection.source == "device details"
            && selection.value == serde_json::json!({"101":"allowed"})
    }));
    assert!(allowlist.argv.ends_with(&[
        "device".into(),
        "allowlist".into(),
        FIRST_SWITCH_MAC.into(),
        "--port".into(),
        "1".into(),
    ]));
    assert_eq!(
        device_show.argv,
        vec![
            binary.to_string_lossy().into_owned(),
            "--format".into(),
            "json".into(),
            "--profile".into(),
            "default".into(),
            "--timeout".into(),
            "15".into(),
            "--site".into(),
            SITE_ID.into(),
            "device".into(),
            "show".into(),
            FIRST_MAC.into(),
        ]
    );

    let serialized = serde_json::to_string(&report.results).expect("serialize result evidence");
    assert!(!serialized.contains(TOKEN_SENTINEL));
    assert!(!serialized.contains(SHARED_SECRET_SENTINEL));
    assert!(!serialized.contains(STDERR_SENTINEL));
    let recorded = calls(&marker);
    assert_eq!(
        recorded.len(),
        report
            .results
            .iter()
            .filter(|result| result.executed)
            .count()
    );
    assert!(recorded.iter().any(|line| {
        line.contains("--format json --profile default --timeout 15")
            && line.ends_with(&format!("device show {FIRST_MAC}"))
    }));

    // An AP name that looks like an address switches device health into direct
    // host mode. It must not probe a host unrelated to the selected AP.
    for response in ["address_name", "mac_name"] {
        let directory = TestDirectory::new();
        let (binary, marker) = stub_binary(
            &directory,
            StubBehavior::Fault {
                command: "radio list",
                response,
            },
        );
        let report = execute_plan(
            &args(binary, directory.path().join("unused.json")),
            plan(&Cli::command()).unwrap(),
        )
        .unwrap();
        let health = report
            .results
            .iter()
            .find(|result| result.command == "device health")
            .unwrap();
        assert!(
            !health.executed,
            "address-shaped AP names must not select a different host"
        );
        assert_eq!(health.failure, Some("no_runtime_subject"));
        assert!(report.failed());
        assert!(
            !calls(&marker)
                .iter()
                .any(|line| line.contains(" device health "))
        );
    }
}

#[test]
fn wired_allowlist_searches_each_switch_and_requires_known_fresh_eligibility() {
    let directory = TestDirectory::new();
    let (binary, marker) = stub_binary(
        &directory,
        StubBehavior::Allowlist(AllowlistFixture::FirstForbiddenThenAllowed),
    );
    let report = execute_plan(
        &args(binary, directory.path().join("unused.json")),
        plan(&Cli::command()).expect("current read catalog"),
    )
    .expect("wired allowlist eligibility is discovered from fresh device details");
    assert!(!report.failed());

    for (mac, faceplate, api_port, expected_status) in [
        (FIRST_SWITCH_MAC, 1, 101, "forbidden"),
        (SECOND_SWITCH_MAC, 2, 202, "allowed"),
    ] {
        let detail = report
            .results
            .iter()
            .find(|result| {
                result.command == "device details"
                    && result.argv.last().is_some_and(|arg| arg == mac)
            })
            .expect("fresh details read is recorded for each distinct switch");
        assert!(detail.executed);
        assert_eq!(detail.exit_code, Some(0));
        assert_eq!(detail.shape, Some("object"));
        assert_eq!(detail.eligibility, Some(expected_status));
        assert!(detail.sample.get("allow_list_port_entries").is_some());

        let device_show = report
            .results
            .iter()
            .find(|result| {
                result.command == "device show" && result.argv.last().is_some_and(|arg| arg == mac)
            })
            .expect("candidate device show proves faceplate-to-API mapping");
        assert!(device_show.executed);
        assert_eq!(device_show.exit_code, Some(0));
        assert_eq!(device_show.shape, Some("object"));
        assert!(device_show.selection.iter().any(|selection| {
            selection.source == "device show"
                && selection.field == "ethernet_ports.faceplatePortNumber"
                && selection.value == faceplate
        }));
        assert!(device_show.selection.iter().any(|selection| {
            selection.source == "device show"
                && selection.field == "ethernet_ports.portNumber"
                && selection.value == api_port
        }));
        assert!(detail.selection.iter().any(|selection| {
            selection.source == "device details"
                && selection.field == "allowListByPortNumber.allowListState"
                && selection.value == serde_json::json!({(api_port.to_string()):expected_status})
        }));
        let calls = calls(&marker);
        for command in ["device show", "device details"] {
            assert_eq!(
                calls
                    .iter()
                    .filter(|line| line.contains(&format!(" {command} {mac}")))
                    .count(),
                1,
                "one fresh {command} per distinct candidate device"
            );
        }
    }

    let allowlist = report
        .results
        .iter()
        .find(|result| result.command == "device allowlist")
        .expect("allowlist read uses the first eligible switch port");
    assert!(allowlist.executed);
    assert!(allowlist.argv.ends_with(&[
        "device".into(),
        "allowlist".into(),
        SECOND_SWITCH_MAC.into(),
        "--port".into(),
        "2".into(),
    ]));
    assert!(allowlist.selection.iter().any(|selection| {
        selection.source == "port list"
            && selection.field == "Device"
            && selection.row_index == 2
            && selection.value == "Switch B"
    }));
    assert!(allowlist.selection.iter().any(|selection| {
        selection.source == "device show"
            && selection.field == "ethernet_ports.portNumber"
            && selection.value == 202
    }));
    assert_eq!(
        calls(&marker)
            .iter()
            .filter(|line| line.contains(&format!(" device details {FIRST_MAC}")))
            .count(),
        1,
        "the AP detail read is not used as a switch allowlist candidate"
    );

    let directory = TestDirectory::new();
    let (binary, marker) = stub_binary(
        &directory,
        StubBehavior::Allowlist(AllowlistFixture::FirstAllowedThenMalformedLater),
    );
    let report = execute_plan(
        &args(binary, directory.path().join("unused.json")),
        plan(&Cli::command()).unwrap(),
    )
    .unwrap();
    assert!(
        !report.failed(),
        "a proven early eligible port ends discovery"
    );
    let allowlist = report
        .results
        .iter()
        .find(|result| result.command == "device allowlist")
        .unwrap();
    assert!(allowlist.executed);
    assert!(allowlist.argv.ends_with(&[
        "device".into(),
        "allowlist".into(),
        FIRST_SWITCH_MAC.into(),
        "--port".into(),
        "1".into(),
    ]));
    assert!(
        !calls(&marker)
            .iter()
            .any(|line| line.contains(&format!(" device details {SECOND_SWITCH_MAC}")))
    );
    assert!(
        !calls(&marker)
            .iter()
            .any(|line| line.contains(&format!(" device show {SECOND_SWITCH_MAC}")))
    );

    for fixture in [
        AllowlistFixture::AllForbidden,
        AllowlistFixture::AllForbiddenCompleteCensus,
    ] {
        let directory = TestDirectory::new();
        let (binary, marker) = stub_binary(&directory, StubBehavior::Allowlist(fixture));
        let report = execute_plan(
            &args(binary, directory.path().join("unused.json")),
            plan(&Cli::command()).unwrap(),
        )
        .unwrap();
        assert!(
            !report.failed(),
            "only explicit all-forbidden is no subject"
        );
        let result = report
            .results
            .iter()
            .find(|result| result.command == "device allowlist")
            .unwrap();
        assert_eq!(result.classification, "no_live_subject");
        assert_eq!(result.failure, None);
        assert_eq!(
            result.sample,
            serde_json::json!({
                "reason":"all_current_positive_ports_forbidden",
                "positive_port_candidates_checked":2,
            })
        );
        assert!(
            !calls(&marker)
                .iter()
                .any(|line| line.contains(" device allowlist "))
        );
    }

    for fixture in [
        AllowlistFixture::AllForbiddenUnknownCensus,
        AllowlistFixture::AllForbiddenMalformedCensus,
    ] {
        let directory = TestDirectory::new();
        let (binary, marker) = stub_binary(&directory, StubBehavior::Allowlist(fixture));
        let report = execute_plan(
            &args(binary, directory.path().join("unused.json")),
            plan(&Cli::command()).unwrap(),
        )
        .unwrap();
        assert!(
            report.failed(),
            "{fixture:?} cannot prove there is no eligible port"
        );
        let result = report
            .results
            .iter()
            .find(|result| result.command == "device allowlist")
            .unwrap();
        assert_eq!(result.classification, "blocked", "{fixture:?}");
        assert_eq!(
            result.failure,
            Some("collection_completeness_unknown"),
            "{fixture:?}"
        );
        assert!(!result.executed);
        assert!(
            !calls(&marker)
                .iter()
                .any(|line| line.contains(" device allowlist "))
        );
    }

    for fixture in [
        AllowlistFixture::UnknownStatus,
        AllowlistFixture::MissingStatus,
        AllowlistFixture::MalformedStatus,
        AllowlistFixture::DetailsReadFailure,
        AllowlistFixture::ShowIdentityMismatch,
        AllowlistFixture::DetailsIdentityMismatch,
        AllowlistFixture::DuplicatePortMapping,
    ] {
        let directory = TestDirectory::new();
        let (binary, marker) = stub_binary(&directory, StubBehavior::Allowlist(fixture));
        let report = execute_plan(
            &args(binary, directory.path().join("unused.json")),
            plan(&Cli::command()).unwrap(),
        )
        .unwrap();
        assert!(report.failed(), "{fixture:?} cannot establish eligibility");
        let allowlist = report
            .results
            .iter()
            .find(|result| result.command == "device allowlist")
            .unwrap();
        assert_eq!(allowlist.classification, "blocked", "{fixture:?}");
        assert!(!allowlist.executed, "{fixture:?}");
        assert!(
            !calls(&marker)
                .iter()
                .any(|line| line.contains(" device allowlist "))
        );
        assert!(
            report.results.iter().any(|result| {
                (result.command == "device details" || result.command == "device show")
                    && result
                        .argv
                        .last()
                        .is_some_and(|arg| arg == FIRST_SWITCH_MAC)
                    && result.failure.is_some()
            }),
            "{fixture:?} records the actual bad discovery read"
        );
    }
}

#[test]
fn empty_cli_collections_are_recorded_as_no_live_subject() {
    let directory = TestDirectory::new();
    let (binary, marker) = stub_binary(&directory, StubBehavior::EmptyCollections);
    let report = execute_plan(
        &args(binary, directory.path().join("unused.json")),
        plan(&Cli::command()).expect("current read catalog"),
    )
    .expect("empty CLI collections are valid read outcomes");
    assert!(!report.failed());

    for (source, target) in [
        ("stack list", "stack show"),
        ("schedule list", "schedule show"),
        ("radius list", "radius show"),
        ("policy list", "policy show"),
        ("port profile list", "port profile show"),
        ("port list", "port show"),
        ("port list", "device allowlist"),
    ] {
        let source_result = report
            .results
            .iter()
            .find(|result| result.command == source)
            .expect("selector source is recorded");
        assert_eq!(source_result.classification, "success", "{source}");
        assert_eq!(source_result.complete, None, "{source}");
        assert_eq!(source_result.count, Some(0), "{source}");
        assert_eq!(source_result.sample, serde_json::json!([]));

        let target_result = report
            .results
            .iter()
            .find(|result| result.command == target)
            .expect("selector target is recorded");
        assert_eq!(target_result.classification, "no_live_subject", "{target}");
        assert_eq!(target_result.failure, None, "{target}");
        assert!(
            !target_result.executed,
            "{target} must not run without a subject"
        );
        assert!(target_result.selection.is_empty(), "{target}");
        if target == "device allowlist" {
            assert_eq!(
                target_result.sample,
                serde_json::json!({
                    "reason":"no_current_positive_port",
                    "positive_port_candidates_checked":0,
                })
            );
        } else {
            assert_eq!(
                target_result.sample,
                serde_json::json!({"empty_source":source})
            );
        }
    }
    assert!(calls(&marker).iter().all(|line| {
        ![
            "stack show",
            "schedule show",
            "radius show",
            "policy show",
            "port profile show",
            "port show",
            "device allowlist",
        ]
        .iter()
        .any(|command| line.contains(&format!("--site {SITE_ID} {command}")))
    }));
}

#[test]
fn selector_collections_keep_command_errors_and_reject_unknown_or_malformed_empty_data() {
    for (response, source_failure, target_failure) in [
        ("exit", "command_failed", "source_read_failed"),
        ("incomplete", "incomplete_read", "source_read_failed"),
        ("malformed", "malformed_collection", "source_read_failed"),
        (
            "empty_unknown",
            "collection_completeness_unknown",
            "source_read_failed",
        ),
    ] {
        let directory = TestDirectory::new();
        let (binary, marker) = stub_binary(
            &directory,
            StubBehavior::Fault {
                command: "schedule list",
                response,
            },
        );
        let report = execute_plan(
            &args(binary, directory.path().join("unused.json")),
            plan(&Cli::command()).unwrap(),
        )
        .expect("source failures remain evidence");
        assert!(report.failed(), "{response} must fail the run");
        let source = report
            .results
            .iter()
            .find(|result| result.command == "schedule list")
            .unwrap();
        assert_eq!(source.failure, Some(source_failure), "{response}");
        assert_eq!(source.classification, "failure", "{response}");
        let target = report
            .results
            .iter()
            .find(|result| result.command == "schedule show")
            .unwrap();
        assert_eq!(target.failure, Some(target_failure), "{response}");
        assert_eq!(target.classification, "blocked", "{response}");
        assert!(
            !calls(&marker)
                .iter()
                .any(|line| line.contains(&format!("--site {SITE_ID} schedule show")))
        );
    }

    let directory = TestDirectory::new();
    let (binary, marker) = stub_binary(
        &directory,
        StubBehavior::Fault {
            command: "policy list",
            response: "unusable",
        },
    );
    let report = execute_plan(
        &args(binary, directory.path().join("unused.json")),
        plan(&Cli::command()).unwrap(),
    )
    .unwrap();
    assert!(
        report.failed(),
        "a nonempty collection without a usable selector fails"
    );
    let source = report
        .results
        .iter()
        .find(|result| result.command == "policy list")
        .unwrap();
    assert_eq!(source.classification, "success");
    assert_eq!(source.count, Some(1));
    let target = report
        .results
        .iter()
        .find(|result| result.command == "policy show")
        .unwrap();
    assert_eq!(target.failure, Some("no_runtime_subject"));
    assert_eq!(target.classification, "blocked");
    assert!(
        !calls(&marker)
            .iter()
            .any(|line| line.ends_with("policy show"))
    );
}

#[test]
fn read_failures_are_recorded_while_later_reads_continue() {
    for (target, response, expected_failure) in [
        ("monitor dashboard", "exit", "command_failed"),
        ("site dashboard", "invalid_json", "invalid_json"),
        ("alert list", "incomplete", "incomplete_read"),
    ] {
        let directory = TestDirectory::new();
        let (binary, marker) = stub_binary(
            &directory,
            StubBehavior::Fault {
                command: target,
                response,
            },
        );
        let read_plan = plan(&Cli::command()).expect("current read catalog");
        let report = execute_plan(
            &args(binary, directory.path().join("unused.json")),
            read_plan,
        )
        .expect("individual read failures are evidence, not runner aborts");
        assert!(report.failed(), "{target} should fail the acceptance run");
        let index = report
            .results
            .iter()
            .position(|result| result.command == target)
            .expect("faulted command is collected");
        assert_eq!(report.results[index].failure, Some(expected_failure));
        assert!(report.results[index].executed);
        assert!(
            report.results[index + 1..]
                .iter()
                .any(|result| result.executed),
            "later independent reads must still execute after {target} fails"
        );
        assert_eq!(
            calls(&marker).len(),
            report
                .results
                .iter()
                .filter(|result| result.executed)
                .count(),
            "each executed read is recorded exactly once"
        );
    }
}

#[test]
fn metadata_gates_block_all_portal_reads_before_spawn() {
    for (behavior, index, failure, blocked_reason) in [
        (
            StubBehavior::MissingDefaultSite,
            1,
            "missing_saved_default_site",
            "default_site_unavailable",
        ),
        (
            StubBehavior::Fault {
                command: "version",
                response: "version_mismatch",
            },
            0,
            "command_tree_revision_mismatch",
            "binary_revision_unavailable",
        ),
    ] {
        let directory = TestDirectory::new();
        let (binary, marker) = stub_binary(&directory, behavior);
        let report = execute_plan(
            &args(binary, directory.path().join("unused.json")),
            plan(&Cli::command()).expect("current read catalog"),
        )
        .expect("missing site is recorded as a gate failure");
        assert_eq!(report.site_id, None);
        assert!(report.failed());
        assert_eq!(report.results[index].failure, Some(failure));
        assert!(report.results[index].executed);
        assert!(
            report.results[index + 1..]
                .iter()
                .all(|result| { !result.executed && result.failure == Some(blocked_reason) })
        );
        assert_eq!(
            calls(&marker).len(),
            index + 1,
            "only metadata gates may spawn"
        );
    }
}

#[cfg(unix)]
#[test]
fn evidence_file_is_private_and_never_overwritten() {
    use std::os::unix::fs::PermissionsExt;

    let directory = TestDirectory::new();
    let (binary, marker) = stub_binary(&directory, StubBehavior::Success);
    let output_path = directory.path().join("evidence.json");
    run(args(binary.clone(), output_path.clone())).expect("successful offline evidence run");

    let metadata = fs::metadata(&output_path).expect("evidence file exists");
    assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
    let before = fs::read(&output_path).expect("read evidence file");
    let report: Value = serde_json::from_slice(&before).expect("evidence JSON is readable");
    assert_eq!(report["site_id"], SITE_ID);
    assert!(!String::from_utf8_lossy(&before).contains(TOKEN_SENTINEL));
    assert!(!String::from_utf8_lossy(&before).contains(SHARED_SECRET_SENTINEL));
    let call_count = calls(&marker).len();
    assert!(call_count > 1, "evidence run executes portal reads");

    assert!(
        run(args(binary, output_path.clone())).is_err(),
        "existing evidence must not be overwritten"
    );
    assert_eq!(fs::read(&output_path).unwrap(), before);
    assert_eq!(
        calls(&marker).len(),
        call_count,
        "collision fails before spawning"
    );
}
