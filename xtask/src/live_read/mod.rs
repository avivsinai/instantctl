//! Read acceptance runs use a fixed catalog, never an arbitrary shell command.
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    io::{self, Read, Write},
    path::PathBuf,
    process::{Command, Stdio},
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, bail};
use clap::CommandFactory;
use instantctl::output::{self, Format};
use serde::Serialize;
use serde_json::Value;

mod catalog;
use catalog::{Action, Arg, ENTRIES, Filter};

const MAX_OUTPUT_BYTES: u64 = 16 * 1024 * 1024;

#[derive(clap::Args)]
pub struct Args {
    /// Installed instantctl executable; the runner does not build or install it.
    #[arg(long, default_value = "instantctl")]
    binary: PathBuf,
    /// New evidence file outside build caches. Existing files are never overwritten.
    #[arg(long)]
    output: PathBuf,
    /// Saved credential profile whose default site is selected once for this run.
    #[arg(long, default_value = "default")]
    profile: String,
    /// Per-request timeout passed to the installed CLI.
    #[arg(long, default_value_t = 15, value_parser = clap::value_parser!(u64).range(1..))]
    timeout: u64,
}

#[derive(Clone)]
struct Invocation {
    path: String,
    arguments: Vec<Arg>,
}

#[derive(Clone, Serialize)]
struct Selection {
    source: String,
    row_index: usize,
    field: String,
    value: Value,
}

#[derive(Serialize)]
struct ReadResult {
    command: String,
    argv: Vec<String>,
    executed: bool,
    classification: &'static str,
    exit_code: Option<i32>,
    duration_ms: u128,
    shape: Option<&'static str>,
    count: Option<usize>,
    complete: Option<bool>,
    eligibility: Option<&'static str>,
    selection: Vec<Selection>,
    sample: Value,
    failure: Option<&'static str>,
}

#[derive(Serialize)]
struct Report {
    started_unix_ms: u128,
    command_tree_git_sha: &'static str,
    profile: String,
    site_id: Option<String>,
    excluded: BTreeMap<String, String>,
    results: Vec<ReadResult>,
}

impl Report {
    fn failed(&self) -> bool {
        self.results.iter().any(|result| {
            result.failure.is_some()
                || !matches!(result.classification, "success" | "no_live_subject")
        })
    }
}

pub fn run(args: Args) -> anyhow::Result<()> {
    let plan = plan(&instantctl::cli::Cli::command())?;
    validate_plan(&plan)?;
    if let Some(parent) = args
        .output
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)?;
    }
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&args.output)
        .context("cannot create new evidence file")?;
    let report = execute_plan(&args, plan)?;
    serde_json::to_writer_pretty(&mut file, &report)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    let summary: Vec<_> = report
        .results
        .iter()
        .map(|result| Summary {
            command: &result.command,
            classification: result.classification,
            exit: result.exit_code,
            duration_ms: result.duration_ms,
            shape: result.shape,
            count: result.count,
            complete: result.complete,
            eligibility: result.eligibility,
            failure: result.failure,
        })
        .collect();
    output::write_data(&mut io::stdout().lock(), Format::Table, &summary)?;
    if report.failed() {
        bail!("read acceptance failed; inspect the evidence file");
    }
    Ok(())
}

#[derive(Serialize)]
struct Summary<'a> {
    command: &'a str,
    classification: &'static str,
    exit: Option<i32>,
    duration_ms: u128,
    shape: Option<&'static str>,
    count: Option<usize>,
    complete: Option<bool>,
    eligibility: Option<&'static str>,
    failure: Option<&'static str>,
}

fn leaves(command: &clap::Command, prefix: &str, paths: &mut Vec<String>) {
    let children: Vec<_> = command.get_subcommands().collect();
    if children.is_empty() {
        paths.push(prefix.to_owned());
    } else {
        for child in children {
            let path = if prefix.is_empty() {
                child.get_name().to_owned()
            } else {
                format!("{prefix} {}", child.get_name())
            };
            leaves(child, &path, paths);
        }
    }
}

fn plan(command: &clap::Command) -> anyhow::Result<Vec<Invocation>> {
    let mut paths = Vec::new();
    leaves(command, "", &mut paths);
    let mut seen = BTreeSet::new();
    for entry in ENTRIES {
        if !seen.insert(entry.path) {
            bail!("duplicate command classification: {}", entry.path);
        }
    }
    let mut reads = Vec::new();
    for path in &paths {
        let entry = ENTRIES
            .iter()
            .find(|entry| entry.path == path)
            .with_context(|| format!("unclassified command: {path}"))?;
        if let Action::Read(arguments) = entry.action {
            reads.push(Invocation {
                path: path.clone(),
                arguments: arguments.to_vec(),
            });
        }
    }
    // These named read gates are already assigned, but can precede installation.
    // Keep their parser failures in evidence instead of silently omitting them.
    for entry in ENTRIES {
        if ["firmware ", "admin ", "radio "]
            .iter()
            .any(|prefix| entry.path.starts_with(prefix))
            && !paths.iter().any(|path| path == entry.path)
            && let Action::Read(arguments) = entry.action
        {
            reads.push(Invocation {
                path: entry.path.into(),
                arguments: arguments.to_vec(),
            });
        }
    }
    Ok(reads)
}

fn validate_plan(plan: &[Invocation]) -> anyhow::Result<()> {
    for invocation in plan {
        let Some(entry) = ENTRIES.iter().find(|entry| entry.path == invocation.path) else {
            bail!("command is not read-allowlisted");
        };
        let Action::Read(arguments) = entry.action else {
            bail!("mutating or excluded command refused before execution");
        };
        if invocation.arguments != arguments {
            bail!("arguments differ from the read-only recipe");
        }
    }
    Ok(())
}

fn execute_plan(args: &Args, mut plan: Vec<Invocation>) -> anyhow::Result<Report> {
    validate_plan(&plan)?;
    if !safe_selector(&args.profile) {
        bail!("invalid saved profile name");
    }
    let mut report = Report {
        started_unix_ms: SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis(),
        command_tree_git_sha: instantctl::version::GIT_SHA,
        profile: args.profile.clone(),
        site_id: None,
        excluded: ENTRIES
            .iter()
            .filter_map(|entry| match entry.action {
                Action::Exclude(reason) => Some((entry.path.into(), reason.into())),
                Action::Read(_) => None,
            })
            .collect(),
        results: Vec::new(),
    };
    let version_index = plan
        .iter()
        .position(|invocation| invocation.path == "version")
        .context("read plan has no binary revision check")?;
    let version = plan.remove(version_index);
    let (mut result, version_data) = invoke(args, None, &version, &[], Vec::new());
    if result.failure.is_none()
        && version_data
            .as_ref()
            .and_then(|data| data.get("git_sha"))
            .and_then(Value::as_str)
            != Some(instantctl::version::GIT_SHA)
    {
        result.failure = Some("command_tree_revision_mismatch");
        result.classification = "failure";
    }
    let revision_verified = result.failure.is_none();
    report.results.push(result);
    if !revision_verified {
        for invocation in plan {
            report.results.push(blocked(
                args,
                None,
                &invocation,
                "binary_revision_unavailable",
            ));
        }
        return Ok(report);
    }
    let profile_index = plan
        .iter()
        .position(|invocation| invocation.path == "profile show")
        .context("read plan has no saved-profile discovery")?;
    let profile = plan.remove(profile_index);
    let (mut result, data) = invoke(args, None, &profile, &[], Vec::new());
    if result.failure.is_none() {
        report.site_id = data
            .as_ref()
            .and_then(|value| value.get("default_site"))
            .and_then(Value::as_str)
            .filter(|site| instantctl_api::client::reads::valid_site_id(site))
            .map(str::to_owned);
        if report.site_id.is_none() {
            result.failure = Some("missing_saved_default_site");
            result.classification = "failure";
        }
    }
    report.results.push(result);
    let mut data_by_command = BTreeMap::new();
    if let Some(data) = version_data {
        data_by_command.insert(version.path, data);
    }
    if let Some(data) = data {
        data_by_command.insert(profile.path, data);
    }
    let Some(site) = report.site_id.clone() else {
        for invocation in plan {
            report
                .results
                .push(blocked(args, None, &invocation, "default_site_unavailable"));
        }
        return Ok(report);
    };
    while !plan.is_empty() {
        let ready = plan.iter().position(|invocation| {
            invocation.arguments.iter().all(|argument| match argument {
                Arg::Literal(_) => true,
                Arg::First { source, .. } => report
                    .results
                    .iter()
                    .any(|result| result.command == *source),
            })
        });
        let Some(index) = ready else {
            for invocation in plan {
                report.results.push(blocked(
                    args,
                    Some(&site),
                    &invocation,
                    "unresolved_read_source",
                ));
            }
            break;
        };
        let invocation = plan.remove(index);
        if invocation.path == "device allowlist" {
            match discover_wired_allowlist_subject(args, &site, &mut report, &data_by_command) {
                Ok(AllowlistDiscovery::Selected { suffix, selection }) => {
                    let (result, _) = invoke(args, Some(&site), &invocation, &suffix, selection);
                    report.results.push(result);
                }
                Ok(AllowlistDiscovery::NoLive {
                    reason,
                    checked_ports,
                }) => {
                    let mut result = no_live_subject_result(args, Some(&site), &invocation, reason);
                    result.sample = serde_json::json!({
                        "reason": reason,
                        "positive_port_candidates_checked": checked_ports,
                    });
                    report.results.push(result);
                }
                Err(failure) => {
                    report
                        .results
                        .push(blocked(args, Some(&site), &invocation, failure))
                }
            }
            continue;
        }
        let mut arguments = Vec::new();
        let mut selections = Vec::new();
        let mut failure = None;
        let mut empty_source = None;
        for argument in &invocation.arguments {
            match argument {
                Arg::Literal(value) => arguments.push((*value).to_owned()),
                Arg::First {
                    source,
                    field,
                    filter,
                } => {
                    let Some(data) = data_by_command.get(*source) else {
                        failure = Some("source_read_failed");
                        break;
                    };
                    let rows = collection_rows(data);
                    if !field.starts_with('/') && rows.is_some_and(|rows| rows.is_empty()) {
                        empty_source = Some(*source);
                        break;
                    }
                    if !field.starts_with('/') && rows.is_none() {
                        failure = Some("malformed_collection");
                        break;
                    }
                    let selected = match (rows, filter) {
                        (Some(rows), Filter::PositivePort) => {
                            rows.iter().enumerate().find(|(_, row)| {
                                row.get("Port")
                                    .and_then(Value::as_u64)
                                    .is_some_and(|port| port > 0)
                            })
                        }
                        (Some(_), Filter::WiredAllowlistEligiblePort) => None,
                        (Some(rows), Filter::Any) => rows.first().map(|row| (0, row)),
                        (None, Filter::Any) => Some((0, data)),
                        (None, Filter::PositivePort | Filter::WiredAllowlistEligiblePort) => None,
                    };
                    let row = selected.map(|(_, row)| row);
                    let value = row.and_then(|row| {
                        if field.starts_with('/') {
                            row.pointer(field)
                        } else {
                            row.get(*field)
                        }
                    });
                    let text = value
                        .and_then(|value| match value {
                            Value::String(value) => Some(value.clone()),
                            Value::Number(value) => Some(value.to_string()),
                            _ => None,
                        })
                        .filter(|value| safe_read_argument(argument, value));
                    let Some(text) = text else {
                        failure = Some("no_runtime_subject");
                        break;
                    };
                    arguments.push(text);
                    selections.push(Selection {
                        source: (*source).into(),
                        row_index: selected.expect("selected row").0,
                        field: (*field).into(),
                        value: value.expect("selected value").clone(),
                    });
                }
            }
        }
        if let Some(source) = empty_source {
            report.results.push(no_live_subject_result(
                args,
                Some(&site),
                &invocation,
                source,
            ));
            continue;
        }
        if let Some(failure) = failure {
            report
                .results
                .push(blocked(args, Some(&site), &invocation, failure));
            continue;
        }
        let (mut result, data) = invoke(args, Some(&site), &invocation, &arguments, selections);
        if result.failure.is_none() && has_collection_dependents(&invocation.path) {
            match data.as_ref().and_then(collection_rows) {
                None => result.failure = Some("malformed_collection"),
                Some(rows)
                    if rows.is_empty()
                        && result.complete != Some(true)
                        && !data.as_ref().is_some_and(Value::is_array) =>
                {
                    result.failure = Some("collection_completeness_unknown");
                }
                Some(_) => {}
            }
            if result.failure.is_some() {
                result.classification = "failure";
            }
        }
        if result.failure.is_none()
            && let Some(data) = data
        {
            data_by_command.insert(invocation.path, data);
        }
        report.results.push(result);
    }
    Ok(report)
}

fn safe_selector(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 512
        && !value.starts_with('-')
        && !value.chars().any(char::is_control)
}

fn collection_rows(value: &Value) -> Option<&[Value]> {
    value
        .as_array()
        .or_else(|| value.get("elements").and_then(Value::as_array))
        .map(Vec::as_slice)
}

enum AllowlistDiscovery {
    Selected {
        suffix: Vec<String>,
        selection: Vec<Selection>,
    },
    NoLive {
        reason: &'static str,
        checked_ports: usize,
    },
}

struct WiredPortCandidate {
    row_index: usize,
    device_name: String,
    faceplate_port: u64,
    device_row_index: usize,
    mac: String,
}

struct WiredDeviceRead {
    show: Value,
    details: Value,
    show_result: usize,
    details_result: usize,
}

fn discover_wired_allowlist_subject(
    args: &Args,
    site: &str,
    report: &mut Report,
    data_by_command: &BTreeMap<String, Value>,
) -> Result<AllowlistDiscovery, &'static str> {
    let port_list = data_by_command
        .get("port list")
        .ok_or("source_read_failed")?;
    let port_rows = collection_rows(port_list).ok_or("malformed_collection")?;
    let device_list = data_by_command
        .get("device list")
        .ok_or("source_read_failed")?;
    let device_rows = collection_rows(device_list).ok_or("malformed_collection")?;
    let mut seen_ports = BTreeSet::new();
    let mut seen_macs = BTreeMap::<String, String>::new();
    let mut saw_positive_port = false;
    let device_show_invocation = read_invocation("device show")?;
    let details_invocation = read_invocation("device details")?;
    let mut device_reads = BTreeMap::<String, WiredDeviceRead>::new();
    let mut checked_ports = 0;
    for (row_index, row) in port_rows.iter().enumerate() {
        let device_name = row
            .get("Device")
            .and_then(Value::as_str)
            .filter(|name| safe_selector(name))
            .ok_or("malformed_port_device")?;
        let faceplate_port = row
            .get("Port")
            .and_then(Value::as_u64)
            .ok_or("malformed_port_number")?;
        if faceplate_port == 0 {
            continue;
        }
        saw_positive_port = true;
        let matching_devices: Vec<_> = device_rows
            .iter()
            .enumerate()
            .filter(|(_, device)| device.get("name").and_then(Value::as_str) == Some(device_name))
            .collect();
        let [(device_row_index, device)] = matching_devices.as_slice() else {
            return Err("device_identity_ambiguous_or_missing");
        };
        let mac = device
            .get("mac")
            .and_then(Value::as_str)
            .filter(|mac| instantctl_api::client::reads::is_mac(mac))
            .ok_or("device_identity_missing")?;
        if seen_macs
            .insert(mac.to_ascii_lowercase(), device_name.to_owned())
            .is_some_and(|known_name| known_name != device_name)
        {
            return Err("device_identity_ambiguous_or_missing");
        }
        if !seen_ports.insert((mac.to_ascii_lowercase(), faceplate_port)) {
            return Err("duplicate_port_identity");
        }

        let candidate = WiredPortCandidate {
            row_index,
            device_name: device_name.to_owned(),
            faceplate_port,
            device_row_index: *device_row_index,
            mac: mac.to_owned(),
        };
        let mac_key = candidate.mac.to_ascii_lowercase();
        if !device_reads.contains_key(&mac_key) {
            let show_selection = vec![Selection {
                source: "device list".into(),
                row_index: candidate.device_row_index,
                field: "mac".into(),
                value: Value::String(candidate.mac.clone()),
            }];
            let (mut show_result, show) = invoke(
                args,
                Some(site),
                &device_show_invocation,
                std::slice::from_ref(&candidate.mac),
                show_selection,
            );
            let show = match show {
                Some(show) if show_result.failure.is_none() => show,
                _ => {
                    report.results.push(show_result);
                    return Err("source_read_failed");
                }
            };
            let show_id = show
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty());
            let show_mac = show
                .get("mac")
                .and_then(Value::as_str)
                .filter(|mac| instantctl_api::client::reads::is_mac(mac));
            if show_id.is_none() || show_mac.is_none() {
                show_result.failure = Some("device_identity_missing");
                show_result.classification = "failure";
                report.results.push(show_result);
                return Err("device_identity_missing");
            }
            let show_id = show_id.expect("identity checked");
            let show_mac = show_mac.expect("identity checked");
            if !show_mac.eq_ignore_ascii_case(&candidate.mac) {
                show_result.failure = Some("device_identity_mismatch");
                show_result.classification = "failure";
                report.results.push(show_result);
                return Err("device_identity_mismatch");
            }
            let show_result_index = report.results.len();
            report.results.push(show_result);

            let details_selection = vec![Selection {
                source: "device list".into(),
                row_index: candidate.device_row_index,
                field: "mac".into(),
                value: Value::String(candidate.mac.clone()),
            }];
            let (mut details_result, details) = invoke(
                args,
                Some(site),
                &details_invocation,
                std::slice::from_ref(&candidate.mac),
                details_selection,
            );
            let details = match details {
                Some(details) if details_result.failure.is_none() => details,
                _ => {
                    report.results.push(details_result);
                    return Err("source_read_failed");
                }
            };
            let details_id = details
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty());
            if details_id != Some(show_id) {
                details_result.failure = Some("device_identity_mismatch");
                details_result.classification = "failure";
                report.results.push(details_result);
                return Err("device_identity_mismatch");
            }
            let port_allowlists = details
                .get("allowListByPortNumber")
                .and_then(Value::as_object);
            details_result.sample = serde_json::json!({
                "allow_list_port_entries": port_allowlists.map_or(0, |entries| entries.len()),
                "allow_list_trunk_entries": details
                    .get("allowListByTrunkNumber")
                    .and_then(Value::as_object)
                    .map_or(0, |entries| entries.len()),
            });
            let Some(port_allowlists) = port_allowlists else {
                details_result.failure = Some("allowlist_eligibility_unknown");
                details_result.classification = "failure";
                details_result.eligibility = Some("unknown");
                report.results.push(details_result);
                return Err("allowlist_eligibility_unknown");
            };
            if port_allowlists.values().any(|entry| !entry.is_object()) {
                details_result.failure = Some("allowlist_eligibility_malformed");
                details_result.classification = "failure";
                details_result.eligibility = Some("unknown");
                report.results.push(details_result);
                return Err("allowlist_eligibility_malformed");
            }
            let details_result_index = report.results.len();
            report.results.push(details_result);
            device_reads.insert(
                mac_key.clone(),
                WiredDeviceRead {
                    show,
                    details,
                    show_result: show_result_index,
                    details_result: details_result_index,
                },
            );
        }
        let fresh = device_reads
            .get(&mac_key)
            .expect("device show and details were read for each candidate");
        let (ethernet_port_index, api_port) =
            match map_faceplate_port(&fresh.show, candidate.faceplate_port) {
                Ok(mapping) => mapping,
                Err(failure) => {
                    report.results[fresh.show_result].failure = Some(failure);
                    report.results[fresh.show_result].classification = "failure";
                    return Err(failure);
                }
            };
        let map_key = api_port.to_string();
        let status = fresh
            .details
            .get("allowListByPortNumber")
            .and_then(Value::as_object)
            .and_then(|entries| entries.get(&map_key))
            .and_then(|entry| entry.get("allowListState"))
            .and_then(Value::as_str);
        let eligibility = match status {
            Some("forbidden") => "forbidden",
            Some("allowed") => "allowed",
            Some("maxEntityAllowedAllowListReached") => "max_entity_allowed",
            Some("maxAllowedClientsReached") => "max_clients_reached",
            _ => {
                let result = &mut report.results[fresh.details_result];
                result.failure = Some("allowlist_eligibility_unknown");
                result.classification = "failure";
                result.eligibility = Some("unknown");
                return Err("allowlist_eligibility_unknown");
            }
        };
        let faceplate = Value::from(candidate.faceplate_port);
        let api_port_value = Value::from(api_port);
        report.results[fresh.show_result].selection.extend([
            Selection {
                source: "port list".into(),
                row_index: candidate.row_index,
                field: "Device".into(),
                value: Value::String(candidate.device_name.clone()),
            },
            Selection {
                source: "port list".into(),
                row_index: candidate.row_index,
                field: "Port".into(),
                value: faceplate.clone(),
            },
            Selection {
                source: "device show".into(),
                row_index: ethernet_port_index,
                field: "ethernet_ports.faceplatePortNumber".into(),
                value: faceplate.clone(),
            },
            Selection {
                source: "device show".into(),
                row_index: ethernet_port_index,
                field: "ethernet_ports.portNumber".into(),
                value: api_port_value.clone(),
            },
        ]);
        report.results[fresh.details_result].eligibility = Some(eligibility);
        report.results[fresh.details_result]
            .selection
            .push(Selection {
                source: "device show".into(),
                row_index: ethernet_port_index,
                field: "ethernet_ports.portNumber".into(),
                value: api_port_value,
            });
        report.results[fresh.details_result]
            .selection
            .push(Selection {
                source: "device details".into(),
                row_index: 0,
                field: "allowListByPortNumber.allowListState".into(),
                value: serde_json::json!({(map_key): status.unwrap()}),
            });
        checked_ports += 1;
        if eligibility != "forbidden" {
            return Ok(AllowlistDiscovery::Selected {
                suffix: vec![
                    candidate.mac,
                    "--port".into(),
                    candidate.faceplate_port.to_string(),
                ],
                selection: report.results[fresh.show_result]
                    .selection
                    .iter()
                    .chain(&report.results[fresh.details_result].selection)
                    .cloned()
                    .collect(),
            });
        }
    }
    if !port_list.is_array() && port_list.get("complete").and_then(Value::as_bool) != Some(true) {
        return Err("collection_completeness_unknown");
    }

    Ok(AllowlistDiscovery::NoLive {
        reason: if saw_positive_port {
            "all_current_positive_ports_forbidden"
        } else {
            "no_current_positive_port"
        },
        checked_ports,
    })
}

fn map_faceplate_port(device: &Value, faceplate: u64) -> Result<(usize, u64), &'static str> {
    let ports = device
        .get("ethernet_ports")
        .and_then(Value::as_array)
        .ok_or("api_port_mapping_missing")?;
    let mut faceplates = BTreeSet::new();
    let mut api_ports = BTreeSet::new();
    let mut selected = None;
    for (index, port) in ports.iter().enumerate() {
        let face = port
            .get("faceplatePortNumber")
            .and_then(Value::as_u64)
            .filter(|number| *number > 0)
            .ok_or("api_port_mapping_malformed")?;
        let api = port
            .get("portNumber")
            .and_then(Value::as_u64)
            .ok_or("api_port_mapping_malformed")?;
        if !faceplates.insert(face) || !api_ports.insert(api) {
            return Err("api_port_mapping_ambiguous");
        }
        if face == faceplate {
            selected = Some((index, api));
        }
    }
    selected.ok_or("api_port_mapping_missing")
}

fn read_invocation(path: &str) -> Result<Invocation, &'static str> {
    ENTRIES
        .iter()
        .find(|entry| entry.path == path)
        .and_then(|entry| match entry.action {
            Action::Read(arguments) => Some(Invocation {
                path: path.to_owned(),
                arguments: arguments.to_vec(),
            }),
            Action::Exclude(_) => None,
        })
        .ok_or("allowlist_discovery_recipe_missing")
}

fn has_collection_dependents(source: &str) -> bool {
    ENTRIES.iter().any(|entry| match entry.action {
        Action::Read(arguments) => arguments.iter().any(|argument| {
            matches!(argument, Arg::First { source: candidate, field, .. } if *candidate == source && !field.starts_with('/'))
        }),
        Action::Exclude(_) => false,
    })
}

fn safe_read_argument(argument: &Arg, value: &str) -> bool {
    match argument {
        Arg::Literal(expected) => value == *expected,
        // Radio rows report names. Address-shaped names would instead select a
        // direct health host or an unrelated inventory device by MAC.
        Arg::First {
            source: "radio list",
            field: "device",
            ..
        } => {
            safe_selector(value)
                && value.parse::<std::net::IpAddr>().is_err()
                && !instantctl_api::client::reads::is_mac(value)
        }
        Arg::First { .. } => safe_selector(value),
    }
}

fn argv(
    args: &Args,
    site: Option<&str>,
    invocation: &Invocation,
    suffix: &[String],
) -> Vec<String> {
    let mut argv = vec![
        args.binary.to_string_lossy().into_owned(),
        "--format".into(),
        "json".into(),
        "--profile".into(),
        args.profile.clone(),
        "--timeout".into(),
        args.timeout.to_string(),
    ];
    if let Some(site) = site {
        argv.extend(["--site".into(), site.into()]);
    }
    argv.extend(invocation.path.split_whitespace().map(str::to_owned));
    argv.extend_from_slice(suffix);
    argv
}

fn blocked(
    args: &Args,
    site: Option<&str>,
    invocation: &Invocation,
    failure: &'static str,
) -> ReadResult {
    ReadResult {
        command: invocation.path.clone(),
        argv: argv(args, site, invocation, &[]),
        executed: false,
        classification: "blocked",
        exit_code: None,
        duration_ms: 0,
        shape: None,
        count: None,
        complete: None,
        eligibility: None,
        selection: Vec::new(),
        sample: Value::Null,
        failure: Some(failure),
    }
}

fn no_live_subject_result(
    args: &Args,
    site: Option<&str>,
    invocation: &Invocation,
    source: &str,
) -> ReadResult {
    ReadResult {
        command: invocation.path.clone(),
        argv: argv(args, site, invocation, &[]),
        executed: false,
        classification: "no_live_subject",
        exit_code: None,
        duration_ms: 0,
        shape: None,
        count: None,
        complete: None,
        eligibility: None,
        selection: Vec::new(),
        sample: serde_json::json!({"empty_source": source}),
        failure: None,
    }
}

fn invoke(
    args: &Args,
    site: Option<&str>,
    invocation: &Invocation,
    suffix: &[String],
    selection: Vec<Selection>,
) -> (ReadResult, Option<Value>) {
    let mut result = blocked(args, site, invocation, "spawn_failed");
    result.argv = argv(args, site, invocation, suffix);
    result.selection = selection;
    result.classification = "failure";
    if validate_plan(std::slice::from_ref(invocation)).is_err()
        || invocation.arguments.len() != suffix.len()
        || !invocation
            .arguments
            .iter()
            .zip(suffix)
            .all(|(expected, actual)| safe_read_argument(expected, actual))
    {
        result.failure = Some("unsafe_read_arguments");
        return (result, None);
    }
    let started = Instant::now();
    let spawned = Command::new(&args.binary)
        .args(&result.argv[1..])
        .env_remove("HPE_INSTANT_ON_TOKEN")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();
    let Ok(mut child) = spawned else {
        result.duration_ms = started.elapsed().as_millis();
        return (result, None);
    };
    result.executed = true;
    let mut bytes = Vec::new();
    let read = child
        .stdout
        .take()
        .expect("piped stdout")
        .take(MAX_OUTPUT_BYTES + 1)
        .read_to_end(&mut bytes);
    if read.is_err() || bytes.len() as u64 > MAX_OUTPUT_BYTES {
        let _ = child.kill();
        let _ = child.wait();
        result.duration_ms = started.elapsed().as_millis();
        result.failure = Some(if read.is_err() {
            "output_read_failed"
        } else {
            "output_too_large"
        });
        return (result, None);
    }
    result.exit_code = child.wait().ok().and_then(|status| status.code());
    result.duration_ms = started.elapsed().as_millis();
    let data = serde_json::from_slice::<Value>(&bytes).ok();
    result.failure = if result.exit_code != Some(0) {
        Some("command_failed")
    } else if data.is_none() {
        Some("invalid_json")
    } else {
        None
    };
    result.classification = if result.failure.is_some() {
        "failure"
    } else {
        "success"
    };
    if let Some(data) = &data {
        (result.shape, result.count) = shape(data);
        result.complete = data.get("complete").and_then(Value::as_bool);
        if result.complete == Some(false) {
            result.failure = Some("incomplete_read");
            result.classification = "failure";
        }
        let mut sample = data.clone();
        output::redact_secrets(&mut sample);
        result.sample = sample_value(&sample, 0);
    }
    (result, data)
}

fn shape(value: &Value) -> (Option<&'static str>, Option<usize>) {
    match value {
        Value::Array(items) => (Some("array"), Some(items.len())),
        Value::Object(object) => (
            Some("object"),
            Some(
                object
                    .get("elements")
                    .and_then(Value::as_array)
                    .map_or(object.len(), Vec::len),
            ),
        ),
        Value::String(_) => (Some("string"), None),
        Value::Number(_) => (Some("number"), None),
        Value::Bool(_) => (Some("boolean"), None),
        Value::Null => (Some("null"), None),
    }
}

fn sample_value(value: &Value, depth: usize) -> Value {
    if depth >= 4 {
        return Value::String("<truncated>".into());
    }
    match value {
        Value::Array(items) => items
            .iter()
            .take(2)
            .map(|value| sample_value(value, depth + 1))
            .collect(),
        Value::Object(object) => Value::Object(
            object
                .iter()
                .take(12)
                .map(|(key, value)| (key.clone(), sample_value(value, depth + 1)))
                .collect(),
        ),
        Value::String(_) if depth == 0 => Value::String("<redacted>".into()),
        Value::String(text) => Value::String(text.chars().take(256).collect()),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests;
