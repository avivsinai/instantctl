mod self_guard;

use std::net::Ipv4Addr;

use clap::{Args as ClapArgs, Subcommand};
use instantctl_api::{
    Error, ErrorKind,
    client::{
        operations::{Change, ClientMutation, TagMode},
        reads::ClientSummary,
    },
};
use serde_json::{Value, json};

use crate::{
    context::{CommandContext, CommandResult},
    mutation::Options,
};

#[derive(Debug, ClapArgs)]
pub struct Args {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// List clients at the selected site.
    #[command(alias = "ls")]
    List,
    /// Show a client by exact name or MAC address.
    Show { id: String },
    /// Rename a client.
    Rename {
        id: String,
        name: String,
        #[command(flatten)]
        options: Options,
    },
    /// Block a client. Refuses to block this local machine without --force.
    Block {
        id: String,
        #[arg(long)]
        force: bool,
        #[command(flatten)]
        options: Options,
    },
    /// Unblock a client.
    Unblock {
        id: String,
        #[command(flatten)]
        options: Options,
    },
    /// Reserve an IPv4 address for a client.
    ReserveIp {
        id: String,
        ip: Ipv4Addr,
        #[arg(long)]
        network: String,
        #[command(flatten)]
        options: Options,
    },
    /// Add or remove a client from the watchlist.
    Watchlist {
        #[command(subcommand)]
        command: WatchlistCommand,
    },
    /// Start a client power cycle.
    PowerCycle {
        id: String,
        #[command(flatten)]
        options: Options,
    },
    /// List or change client tags.
    Tags {
        #[command(subcommand)]
        command: TagsCommand,
    },
    /// Show reported wired ports or access point for a client.
    WhereIs { id: String },
}

#[derive(Debug, Subcommand)]
pub enum WatchlistCommand {
    Add {
        id: String,
        #[command(flatten)]
        options: Options,
    },
    Remove {
        id: String,
        #[command(flatten)]
        options: Options,
    },
}

#[derive(Debug, Subcommand)]
pub enum TagsCommand {
    List {
        id: String,
    },
    Set {
        id: String,
        tags: Vec<String>,
        #[command(flatten)]
        options: Options,
    },
    Add {
        id: String,
        tags: Vec<String>,
        #[command(flatten)]
        options: Options,
    },
    Remove {
        id: String,
        tags: Vec<String>,
        #[command(flatten)]
        options: Options,
    },
}

pub async fn run(args: Args, context: &CommandContext) -> anyhow::Result<CommandResult> {
    match args.command {
        Command::List
        | Command::Show { .. }
        | Command::Tags {
            command: TagsCommand::List { .. },
        }
        | Command::WhereIs { .. } => {
            let api = super::site::read::client(context)?;
            let clients = super::site::read::clients(&api, context).await?;
            render(args, &clients)
        }
        command => {
            let (id, change, options, check_local) = mutation_args(command)?;
            let operation = format!("client.{}", change_name(&change));
            let api = super::site::read::client(context)?;
            let site = super::site::read::site_id(&api, context).await?;
            let (backend, plan) = ClientMutation::plan(&api, &site, &id, change).await?;
            if check_local {
                let interfaces = self_guard::local_interfaces().ok();
                self_guard::ensure_not_local(
                    Some(&backend.target().mac),
                    backend.target().ip.as_deref(),
                    interfaces.as_deref(),
                    false,
                )?;
            }
            let target = serde_json::to_value(backend.target())?;
            crate::mutation::execute(&backend, plan, &operation, target, options, context).await
        }
    }
}

fn mutation_args(command: Command) -> Result<(String, Change, Options, bool), Error> {
    Ok(match command {
        Command::Rename { id, name, options } => (id, Change::Rename(name), options, false),
        Command::Block { id, force, options } => (id, Change::Block, options, !force),
        Command::Unblock { id, options } => (id, Change::Unblock, options, false),
        Command::ReserveIp {
            id,
            ip,
            network,
            options,
        } => (id, Change::ReserveIp { network, ip }, options, false),
        Command::Watchlist {
            command: WatchlistCommand::Add { id, options },
        } => (id, Change::Watchlist(true), options, false),
        Command::Watchlist {
            command: WatchlistCommand::Remove { id, options },
        } => (id, Change::Watchlist(false), options, false),
        Command::PowerCycle { id, options } => (id, Change::PowerCycle, options, false),
        Command::Tags {
            command: TagsCommand::Set { id, tags, options },
        } => (
            id,
            Change::Tags {
                mode: TagMode::Set,
                tags,
            },
            options,
            false,
        ),
        Command::Tags {
            command: TagsCommand::Add { id, tags, options },
        } => (
            id,
            Change::Tags {
                mode: TagMode::Add,
                tags,
            },
            options,
            false,
        ),
        Command::Tags {
            command: TagsCommand::Remove { id, tags, options },
        } => (
            id,
            Change::Tags {
                mode: TagMode::Remove,
                tags,
            },
            options,
            false,
        ),
        _ => return Err(Error::new(ErrorKind::Usage, "expected a client mutation")),
    })
}

fn change_name(change: &Change) -> &'static str {
    match change {
        Change::Rename(_) => "rename",
        Change::Block => "block",
        Change::Unblock => "unblock",
        Change::ReserveIp { .. } => "reserve-ip",
        Change::Watchlist(true) => "watchlist.add",
        Change::Watchlist(false) => "watchlist.remove",
        Change::PowerCycle => "power-cycle",
        Change::Tags {
            mode: TagMode::Set, ..
        } => "tags.set",
        Change::Tags {
            mode: TagMode::Add, ..
        } => "tags.add",
        Change::Tags {
            mode: TagMode::Remove,
            ..
        } => "tags.remove",
    }
}

fn render(args: Args, models: &[ClientSummary]) -> anyhow::Result<CommandResult> {
    let clients = super::site::read::values(models)?;
    let data = match args.command {
        Command::List => Value::Array(clients.iter().map(summary).collect()),
        Command::Show { id } => details(super::site::read::select(&clients, &id, "client")?),
        Command::Tags {
            command: TagsCommand::List { id },
        } => {
            let item = super::site::read::select(&clients, &id, "client")?;
            if item.get("classification").is_some() {
                Value::Array(super::site::read::array(item, "classification")?.to_vec())
            } else {
                Value::Null
            }
        }
        Command::WhereIs { id } => where_is(super::site::read::select(&clients, &id, "client")?)?,
        _ => return Err(Error::new(ErrorKind::Usage, "expected a client read command").into()),
    };
    Ok(CommandResult::success(data))
}

fn summary(item: &Value) -> Value {
    super::site::read::project(
        item,
        &[
            ("name", "name"),
            ("mac", "macAddress"),
            ("ip", "ipAddress"),
            ("type", "clientType"),
        ],
    )
}

fn details(item: &Value) -> Value {
    super::site::read::project(
        item,
        &[
            ("name", "name"),
            ("mac", "macAddress"),
            ("ip", "ipAddress"),
            ("type", "clientType"),
            ("status", "status"),
            ("health", "health"),
            ("device", "deviceName"),
            ("device_id", "deviceId"),
            ("ssid", "wirelessNetworkName"),
            ("band", "wirelessBand"),
            ("snr", "snrInDb"),
            ("traffic", "dataTraffic"),
        ],
    )
}

fn where_is(item: &Value) -> Result<Value, Error> {
    if item.get("connectedToPorts").is_some() {
        let ports = super::site::read::array(item, "connectedToPorts")?;
        let rows: Vec<_> = ports.iter().map(port_location).collect();
        if !rows.is_empty() {
            return Ok(Value::Array(rows));
        }
    }
    Ok(json!([{
        "device_mac": item.get("deviceId").and_then(Value::as_str).filter(|v| super::site::read::mac(v)).map(Value::from).unwrap_or(Value::Null),
        "device_name": item.get("deviceName").cloned().unwrap_or(Value::Null),
        "port_idx": Value::Null,
        "name": Value::Null,
        "media": Value::Null,
        "up": Value::Null,
        "speed": Value::Null,
        "full_duplex": Value::Null,
        "poe_enable": Value::Null,
        "poe_power": Value::Null,
        "port_poe": Value::Null,
        "tx_bytes": Value::Null,
        "rx_bytes": Value::Null,
        "connected": Value::Null,
        "duplex": Value::Null,
        "trunk_idx": Value::Null,
        "trunk_name": Value::Null,
        "wireless_radio_id": item.get("wirelessRadioId").cloned().unwrap_or(Value::Null),
        "network_name": item.get("wirelessNetworkName").cloned().unwrap_or(Value::Null),
        "device_id": item.get("deviceId").cloned().unwrap_or(Value::Null),
    }]))
}

fn port_location(port: &Value) -> Value {
    let device_id = port.get("deviceId").and_then(Value::as_str);
    let valid_mac = device_id.filter(|value| super::site::read::mac(value));
    json!({
        "device_mac": valid_mac.map(Value::from).unwrap_or(Value::Null),
        "device_name": port.get("deviceName").cloned().unwrap_or(Value::Null),
        "port_idx": port.get("portNumber").cloned().unwrap_or(Value::Null),
        "name": port.get("portName").cloned().unwrap_or(Value::Null),
        "media": Value::Null,
        "up": Value::Null,
        "speed": port.get("portSpeed").cloned().unwrap_or(Value::Null),
        "full_duplex": Value::Null,
        "poe_enable": Value::Null,
        "poe_power": Value::Null,
        "port_poe": port.get("isPoweredByPort").cloned().unwrap_or(Value::Null),
        "tx_bytes": port.get("upstreamDataTransferredInBytes").cloned().unwrap_or(Value::Null),
        "rx_bytes": port.get("downstreamDataTransferredInBytes").cloned().unwrap_or(Value::Null),
        "connected": Value::Null,
        "duplex": port.get("duplex").cloned().unwrap_or(Value::Null),
        "trunk_idx": port.get("trunkNumber").cloned().unwrap_or(Value::Null),
        "trunk_name": port.get("trunkName").cloned().unwrap_or(Value::Null),
        "device_id": port.get("deviceId").cloned().unwrap_or(Value::Null),
    })
}

#[cfg(test)]
mod tests {
    use super::super::site::read::tests::{error_kind, model};
    use super::*;
    use clap::Parser;
    use instantctl_api::ErrorKind;
    use serde_json::json;

    const CLIENT_ID: &str = "11111111-2222-3333-4444-555555555555";
    const CLIENT_MAC: &str = "AA:BB:CC:DD:EE:01";

    fn client_with_id(id: &str, name: &str, mac: &str) -> ClientSummary {
        model(
            json!({"id":id,"name":name,"macAddress":mac,"ipAddress":"192.0.2.11","clientType":"wireless"}),
        )
    }
    fn client(name: &str, mac: &str) -> ClientSummary {
        client_with_id(CLIENT_ID, name, mac)
    }

    #[test]
    fn list_projects_requested_columns_in_order() {
        let result = render(
            Args {
                command: Command::List,
            },
            &[client("Desk", CLIENT_MAC)],
        )
        .unwrap();
        assert_eq!(
            result.data,
            json!([{"name":"Desk","mac":CLIENT_MAC,"ip":"192.0.2.11","type":"wireless"}])
        );
        assert_eq!(
            result.data[0]
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["name", "mac", "ip", "type"]
        );
    }

    #[test]
    fn show_matches_exact_name_or_case_insensitive_mac_and_projects_reported_fields() {
        let desk = model(
            json!({"id":CLIENT_ID,"name":"Desk","macAddress":CLIENT_MAC,"ipAddress":"192.0.2.11","clientType":"wireless","status":"online","health":"good","deviceName":"Hall AP","deviceId":"ap-id","wirelessNetworkName":"Home WiFi","wirelessBand":"5GHz","snrInDb":42,"dataTraffic":{"downstreamDataTransferredInBytesInLast24Hours":1024,"upstreamDataTransferredInBytesInLast24Hours":512}}),
        );
        let clients = [
            desk,
            client_with_id(
                "22222222-3333-4444-5555-666666666666",
                "Printer",
                "AA:BB:CC:DD:EE:02",
            ),
        ];
        for selector in ["Desk", "aa:bb:cc:dd:ee:01"] {
            let result = render(
                Args {
                    command: Command::Show {
                        id: selector.into(),
                    },
                },
                &clients,
            )
            .unwrap();
            assert_eq!(result.data["name"], "Desk");
            assert_eq!(result.data["mac"], CLIENT_MAC);
            assert_eq!(result.data["status"], "online");
            assert_eq!(result.data["health"], "good");
            assert_eq!(result.data["device"], "Hall AP");
            assert_eq!(result.data["device_id"], "ap-id");
            assert_eq!(result.data["ssid"], "Home WiFi");
            assert_eq!(result.data["band"], "5GHz");
            assert_eq!(result.data["snr"], 42.0);
            assert_eq!(
                result.data["traffic"]["downstreamDataTransferredInBytesInLast24Hours"],
                1024
            );
        }
    }

    #[test]
    fn selectors_are_exact_and_ambiguous_names_are_usage_errors() {
        let clients = [
            client("Desk", CLIENT_MAC),
            client_with_id(
                "22222222-3333-4444-5555-666666666666",
                "Desk",
                "AA:BB:CC:DD:EE:02",
            ),
        ];
        for (id, kind) in [("Desk", ErrorKind::Usage), ("Des", ErrorKind::NotFound)] {
            let err = render(
                Args {
                    command: Command::Show { id: id.into() },
                },
                &clients,
            )
            .err()
            .expect("selector should fail");
            assert_eq!(error_kind(&err), kind);
            assert_eq!(
                crate::exit::ExitStatus::Error(kind).code(),
                if kind == ErrorKind::Usage { 2 } else { 4 }
            );
        }
    }

    #[test]
    fn sparse_show_and_where_is_keep_unreported_fields_null() {
        let result = render(
            Args {
                command: Command::Show { id: "Desk".into() },
            },
            &[client("Desk", CLIENT_MAC)],
        )
        .unwrap();
        for field in [
            "status",
            "health",
            "device",
            "device_id",
            "ssid",
            "band",
            "snr",
            "traffic",
        ] {
            assert!(result.data[field].is_null(), "missing {field} must be null");
        }
        let location = render(
            Args {
                command: Command::WhereIs { id: "Desk".into() },
            },
            &[client("Desk", CLIENT_MAC)],
        )
        .unwrap();
        assert!(location.data[0]["connected"].is_null());
        assert!(location.data[0]["device_mac"].is_null() || location.data[0]["device_mac"] == "");
    }

    #[test]
    fn where_is_maps_only_reported_wired_fields_and_keeps_unknown_state_null() {
        let model = model(
            json!({"id":CLIENT_ID,"name":"Desk","macAddress":CLIENT_MAC,"connectedToPorts":[{"deviceId":"AA:BB:CC:DD:EE:FF","deviceName":"Switch","portNumber":7,"portName":"Office","portSpeed":"1Gbps","duplex":"FULL","isPoweredByPort":true,"upstreamDataTransferredInBytes":12,"downstreamDataTransferredInBytes":34,"trunkNumber":2,"trunkName":"uplink"}]}),
        );
        let result = render(
            Args {
                command: Command::WhereIs { id: "Desk".into() },
            },
            &[model],
        )
        .unwrap();
        let row = &result.data[0];
        assert_eq!(row["device_mac"], "AA:BB:CC:DD:EE:FF");
        assert_eq!(row["port_idx"], 7);
        assert_eq!(row["name"], "Office");
        assert_eq!(row["speed"], "1Gbps");
        assert_eq!(row["port_poe"], true);
        assert_eq!(row["tx_bytes"], 12);
        assert_eq!(row["rx_bytes"], 34);
        assert_eq!(row["trunk_idx"], 2);
        assert_eq!(row["trunk_name"], "uplink");
        assert_eq!(row["duplex"], "FULL");
        for key in [
            "media",
            "up",
            "full_duplex",
            "poe_enable",
            "poe_power",
            "connected",
        ] {
            assert!(row[key].is_null(), "unknown {key} remains null");
        }
    }

    #[test]
    fn where_is_reports_ap_identity_and_refuses_malformed_collections() {
        let ap = model(json!({"id":CLIENT_ID,"name":"Desk","macAddress":CLIENT_MAC,
            "deviceId":"ap-id","deviceName":"Hall AP","wirelessRadioId":"radio-id",
            "wirelessNetworkName":"Home"}));
        let result = render(
            Args {
                command: Command::WhereIs { id: "Desk".into() },
            },
            &[ap],
        )
        .unwrap();
        assert_eq!(result.data[0]["device_id"], "ap-id");
        assert_eq!(result.data[0]["device_name"], "Hall AP");
        assert_eq!(result.data[0]["wireless_radio_id"], "radio-id");
        assert_eq!(result.data[0]["network_name"], "Home");
        assert!(result.data[0]["port_idx"].is_null());
        for value in [json!({}), json!(["bad"])] {
            let malformed = model(
                json!({"id":CLIENT_ID,"name":"Desk","macAddress":CLIENT_MAC,"connectedToPorts":value}),
            );
            let error = render(
                Args {
                    command: Command::WhereIs { id: "Desk".into() },
                },
                &[malformed],
            )
            .err()
            .unwrap();
            assert_eq!(error_kind(&error), ErrorKind::Unverified);
        }
        let malformed = model(
            json!({"id":CLIENT_ID,"name":"Desk","macAddress":CLIENT_MAC,"classification":{}}),
        );
        let error = render(
            Args {
                command: Command::Tags {
                    command: TagsCommand::List { id: "Desk".into() },
                },
            },
            &[malformed],
        )
        .err()
        .unwrap();
        assert_eq!(error_kind(&error), ErrorKind::Unverified);
    }

    #[test]
    fn tags_list_returns_reported_classification() {
        let model = model(
            json!({"id":CLIENT_ID,"name":"Desk","macAddress":CLIENT_MAC,"classification":[{"id":"tag-1","str":"staff"}]}),
        );
        let result = render(
            Args {
                command: Command::Tags {
                    command: TagsCommand::List { id: "Desk".into() },
                },
            },
            &[model],
        )
        .unwrap();
        assert_eq!(result.data, json!([{"id":"tag-1","str":"staff"}]));
    }

    #[test]
    fn client_mutations_parse_with_safe_defaults_and_explicit_apply_flags() {
        fn parsed(args: &[&str]) -> Command {
            match crate::cli::Cli::try_parse_from(args)
                .expect("valid client command")
                .command
            {
                crate::cli::Command::Client(args) => args.command,
                _ => panic!("expected client command"),
            }
        }

        match parsed(&["instantctl", "client", "rename", "Desk", "New Name"]) {
            Command::Rename { options, .. } => assert!(!options.apply && !options.yes),
            other => panic!("unexpected command {other:?}"),
        }
        match parsed(&[
            "instantctl",
            "client",
            "block",
            "Desk",
            "--force",
            "--apply",
            "--yes",
        ]) {
            Command::Block { force, options, .. } => assert!(force && options.apply && options.yes),
            other => panic!("unexpected command {other:?}"),
        }
        match parsed(&[
            "instantctl",
            "client",
            "reserve-ip",
            "Desk",
            "192.0.2.9",
            "--network",
            "network-id",
            "--apply",
        ]) {
            Command::ReserveIp {
                ip,
                network,
                options,
                ..
            } => {
                assert_eq!(ip, Ipv4Addr::new(192, 0, 2, 9));
                assert_eq!(network, "network-id");
                assert!(options.apply && !options.yes);
            }
            other => panic!("unexpected command {other:?}"),
        }
        match parsed(&[
            "instantctl",
            "client",
            "tags",
            "add",
            "Desk",
            "staff",
            "--yes",
        ]) {
            Command::Tags {
                command: TagsCommand::Add { options, tags, .. },
            } => {
                assert_eq!(tags, ["staff"]);
                assert!(!options.apply && options.yes);
            }
            other => panic!("unexpected command {other:?}"),
        }
    }
}
