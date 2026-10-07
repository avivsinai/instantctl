use clap::{Args as ClapArgs, Subcommand};
use instantctl_api::client::reads::Device;
use instantctl_api::{Error, ErrorKind};
use serde_json::{Map, Value};

use crate::context::{CommandContext, CommandResult};

#[derive(Debug, ClapArgs)]
pub struct Args {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// List Ethernet ports for all devices or one selected device.
    #[command(alias = "ls")]
    List { device: Option<String> },
    /// Show one Ethernet port by its faceplate number.
    Show {
        device: String,
        #[arg(value_parser = clap::value_parser!(u64))]
        port: u64,
    },
    /// Change selected port settings using a full device update.
    Set(super::port_actions::SetArgs),
    /// Cycle power for one attached powered client (never an uplink or LAG port).
    #[command(alias = "cycle")]
    PowerCycle(super::port_actions::PortActionArgs),
    /// Find the switch port for a client name, MAC, or IP address.
    #[command(alias = "find-port")]
    Find { client: String },
    /// Run a cable diagnostic on a physical switch port.
    CableTest(super::port_actions::CableArgs),
    /// Ping and trace an address from a switch.
    ConnectivityTest(super::port_actions::ConnectivityArgs),
    /// Configure switch port mirroring.
    Mirror(super::port_actions::MirrorArgs),
    /// Manage switch port profiles.
    Profile(super::port_settings::ProfileArgs),
    /// Inspect or change the site's PoE schedule.
    PoeSchedule(super::port_settings::ScheduleArgs),
    /// Inspect or change site Energy Efficient Ethernet.
    Eee(super::port_settings::EeeArgs),
}

impl Args {
    pub fn preflight(&self, context: &CommandContext) -> Result<(), Error> {
        match &self.command {
            Command::Set(args) => {
                if args.name.is_none()
                    && args.enabled.is_none()
                    && args.profile.is_none()
                    && args.poe_schedule.is_none()
                    && args.poe_mode.is_none()
                    && args.poe_priority.is_none()
                    && args.poe_management.is_none()
                    && args.speed_duplex.is_none()
                {
                    return Err(Error::new(
                        ErrorKind::Usage,
                        "port set requires a configuration option",
                    ));
                }
                args.options.preflight(context.token_source)
            }
            Command::PowerCycle(args) => args.options.preflight(context.token_source),
            Command::CableTest(args) => args.options.preflight(context.token_source),
            Command::ConnectivityTest(args) => args.options.preflight(context.token_source),
            Command::Mirror(args) => args.options.preflight(context.token_source),
            Command::Profile(args) => args.preflight(context),
            Command::PoeSchedule(args) => args.preflight(context),
            Command::Eee(args) => args.preflight(context),
            _ => Ok(()),
        }
    }
}

pub async fn run(args: Args, context: &CommandContext) -> anyhow::Result<CommandResult> {
    match args.command {
        Command::Set(args) => return super::port_actions::run_set(args, context).await,
        Command::PowerCycle(args) => return super::port_actions::run_cycle(args, context).await,
        Command::Find { client } => return super::port_actions::run_find(client, context).await,
        Command::CableTest(args) => return super::port_actions::run_cable(args, context).await,
        Command::ConnectivityTest(args) => {
            return super::port_actions::run_connectivity(args, context).await;
        }
        Command::Mirror(args) => return super::port_actions::run_mirror(args, context).await,
        Command::Profile(args) => return super::port_settings::run_profile(args, context).await,
        Command::PoeSchedule(args) => {
            return super::port_settings::run_schedule(args, context).await;
        }
        Command::Eee(args) => return super::port_settings::run_eee(args, context).await,
        _ => {}
    }
    let api = super::site::read::client(context)?;
    let inventory = super::site::read::inventory(&api, context).await?;
    render(args, &inventory).map_err(Into::into)
}

fn render(args: Args, models: &[Device]) -> Result<CommandResult, Error> {
    let inventory = super::site::read::values(models)?;
    let data = match args.command {
        Command::List {
            device: Some(selector),
        } => {
            let device = super::site::read::select(&inventory, &selector, "device")?;
            Value::Array(port_rows(device, false)?)
        }
        Command::List { device: None } => {
            let mut rows = Vec::new();
            for device in &inventory {
                if has_port_collection(device) {
                    rows.extend(port_rows(device, true)?);
                } else if device.get("radios").is_none() {
                    return Err(super::site::read::incomplete(
                        "inventory device has no observed Ethernet port or radio collection",
                    ));
                }
            }
            Value::Array(rows)
        }
        Command::Show { device, port } => {
            let device = super::site::read::select(&inventory, &device, "device")?;
            if port == 0 && device.get("radios").is_none() {
                return Err(Error::new(
                    ErrorKind::Usage,
                    "port zero is valid only for a device with a reported radio collection",
                ));
            }
            let ports = validated_ports(device)?;
            let selected = ports
                .iter()
                .find(|item| item["faceplatePortNumber"].as_u64() == Some(port))
                .ok_or_else(|| {
                    Error::new(
                        ErrorKind::NotFound,
                        "no port matches the supplied faceplate number",
                    )
                })?;
            port_details(device, selected)
        }
        _ => {
            return Err(Error::new(
                ErrorKind::Config,
                "port read renderer received a write command",
            ));
        }
    };
    Ok(CommandResult::success(data))
}

fn has_port_collection(device: &Value) -> bool {
    device.get("ethernetPorts").is_some() || device.get("trunkPorts").is_some()
}

pub(super) fn validated_ports(device: &Value) -> Result<Vec<Value>, Error> {
    let ports = super::site::read::array(device, "ethernetPorts")?;
    let allows_zero = if device.get("radios").is_some() {
        super::site::read::array(device, "radios")?;
        true
    } else {
        false
    };
    let mut seen = std::collections::HashSet::new();
    for port in ports {
        let number = port
            .get("faceplatePortNumber")
            .and_then(Value::as_u64)
            .filter(|number| *number > 0 || allows_zero && *number == 0)
            .ok_or_else(|| {
                super::site::read::incomplete("port is missing a valid faceplatePortNumber")
            })?;
        if !seen.insert(number) {
            return Err(super::site::read::incomplete(
                "device has duplicate faceplate port numbers",
            ));
        }
    }
    Ok(ports.to_vec())
}

fn port_rows(device: &Value, include_device: bool) -> Result<Vec<Value>, Error> {
    let ports = validated_ports(device)?;
    let mut rows = Vec::with_capacity(ports.len());
    for port in &ports {
        let traffic = port
            .get("portDataTraffic")
            .filter(|traffic| traffic.is_object());
        let mut source = port.clone();
        let source = source
            .as_object_mut()
            .expect("port collection contains objects");
        source.insert(
            "__display_poe".into(),
            poe(port.get("powerProvidedInMilliwatts")),
        );
        source.insert(
            "__display_tx".into(),
            field(traffic, "downstreamDataTransferredInBytesInLast24Hours"),
        );
        source.insert(
            "__display_rx".into(),
            field(traffic, "upstreamDataTransferredInBytesInLast24Hours"),
        );
        source.insert(
            "__display_device".into(),
            device.get("name").cloned().unwrap_or(Value::Null),
        );
        let fields = if include_device {
            vec![
                ("Device", "__display_device"),
                ("Port", "faceplatePortNumber"),
                ("Name", "name"),
                ("Link", "isLinkUp"),
                ("Speed", "speed"),
                ("PoE", "__display_poe"),
                ("TX", "__display_tx"),
                ("RX", "__display_rx"),
            ]
        } else {
            vec![
                ("Port", "faceplatePortNumber"),
                ("Name", "name"),
                ("Link", "isLinkUp"),
                ("Speed", "speed"),
                ("PoE", "__display_poe"),
                ("TX", "__display_tx"),
                ("RX", "__display_rx"),
            ]
        };
        rows.push(super::site::read::project(
            &Value::Object(source.clone()),
            &fields,
        ));
    }
    Ok(rows)
}

fn port_details(device: &Value, port: &Value) -> Value {
    let mut row = Map::new();
    row.insert(
        "device".into(),
        super::site::read::project(
            device,
            &[
                ("id", "id"),
                ("mac_address", "macAddress"),
                ("name", "name"),
            ],
        ),
    );
    row.insert(
        "port".into(),
        super::site::read::project(
            port,
            &[
                ("port_number", "faceplatePortNumber"),
                ("api_port_number", "portNumber"),
                ("name", "name"),
                ("link_up", "isLinkUp"),
                ("speed", "speed"),
                ("is_uplink", "isUplink"),
                ("user_deactivated", "userDeactivated"),
                ("trunk_number", "trunkNumber"),
                ("power_provided_milliwatts", "powerProvidedInMilliwatts"),
                ("capabilities", "capabilities"),
                ("traffic", "portDataTraffic"),
            ],
        ),
    );
    if let Some(traffic) = port
        .get("portDataTraffic")
        .filter(|traffic| traffic.is_object())
    {
        row.insert(
            "downstream_throughput_bps".into(),
            field(Some(traffic), "downstreamThroughputInBitsPerSecond"),
        );
        row.insert(
            "upstream_throughput_bps".into(),
            field(Some(traffic), "upstreamThroughputInBitsPerSecond"),
        );
        row.insert(
            "downstream_bytes_24h".into(),
            field(
                Some(traffic),
                "downstreamDataTransferredInBytesInLast24Hours",
            ),
        );
        row.insert(
            "upstream_bytes_24h".into(),
            field(Some(traffic), "upstreamDataTransferredInBytesInLast24Hours"),
        );
    } else {
        for field in [
            "downstream_throughput_bps",
            "upstream_throughput_bps",
            "downstream_bytes_24h",
            "upstream_bytes_24h",
        ] {
            row.insert(field.into(), Value::Null);
        }
    }
    Value::Object(row)
}

fn field(object: Option<&Value>, name: &str) -> Value {
    object
        .and_then(|value| value.get(name))
        .cloned()
        .unwrap_or(Value::Null)
}

fn poe(milliwatts: Option<&Value>) -> Value {
    match milliwatts.and_then(Value::as_f64) {
        Some(value) => Value::String(format!("{:.1} W", value / 1000.0)),
        None => milliwatts.cloned().unwrap_or(Value::Null),
    }
}

#[cfg(test)]
mod tests {
    use super::super::site::read::tests::{MAC, device, devices};
    use super::*;
    use clap::Parser;
    use serde_json::json;

    fn switch(ports: Value) -> Value {
        let mut item = device("Switch");
        item["ethernetPorts"] = ports;
        item
    }

    fn models(items: Vec<Value>) -> Vec<Device> {
        devices(items)
    }

    fn list(device: Option<&str>) -> Args {
        Args {
            command: Command::List {
                device: device.map(str::to_owned),
            },
        }
    }

    #[test]
    fn list_projects_columns_null_telemetry_and_poe() {
        let inventory = models(vec![switch(json!([
            {"faceplatePortNumber":19,"portNumber":9001,"name":"uplink","isLinkUp":true,
             "speed":"mbps2500","powerProvidedInMilliwatts":12500,
             "portDataTraffic":{"downstreamDataTransferredInBytesInLast24Hours":4200,
                                "upstreamDataTransferredInBytesInLast24Hours":840}},
            {"faceplatePortNumber":21,"portDataTraffic":null}
        ]))]);
        let result = render(list(None), &inventory).unwrap();

        assert_eq!(result.data[0]["Device"], "Switch");
        assert_eq!(result.data[0]["Port"], 19);
        assert_eq!(result.data[0]["Name"], "uplink");
        assert_eq!(result.data[0]["Link"], true);
        assert_eq!(result.data[0]["Speed"], "mbps2500");
        assert_eq!(result.data[0]["PoE"], "12.5 W");
        assert_eq!(result.data[0]["TX"], 4200);
        assert_eq!(result.data[0]["RX"], 840);
        assert_eq!(result.data[1]["PoE"], Value::Null);
        assert_eq!(result.data[1]["TX"], Value::Null);
        assert_eq!(result.data[1]["RX"], Value::Null);
        let keys: Vec<_> = result.data[0]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            ["Device", "Port", "Name", "Link", "Speed", "PoE", "TX", "RX"]
        );
    }

    #[test]
    fn selector_accepts_exact_mac_and_single_device_rows_omit_device() {
        let inventory = models(vec![switch(
            json!([{"faceplatePortNumber":7,"name":"lan"}]),
        )]);
        let result = render(list(Some(&MAC.to_ascii_uppercase())), &inventory).unwrap();
        assert_eq!(result.data[0]["Port"], 7);
        assert!(result.data[0].get("Device").is_none());

        let shown = render(
            Args {
                command: Command::Show {
                    device: MAC.into(),
                    port: 7,
                },
            },
            &inventory,
        )
        .unwrap();
        assert_eq!(shown.data["device"]["mac_address"], MAC);
        assert_eq!(shown.data["port"]["port_number"], 7);
    }

    #[test]
    fn selectors_distinguish_ambiguous_name_from_not_found() {
        let inventory = models(vec![
            switch(json!([])),
            json!({"id":"11:22:33:44:55:66","macAddress":"11:22:33:44:55:66","name":"Switch","ethernetPorts":[]}),
        ]);
        for (selector, expected) in [
            ("Switch", ErrorKind::Usage),
            ("missing", ErrorKind::NotFound),
        ] {
            let error = render(list(Some(selector)), &inventory).err().unwrap();
            assert_eq!(error.kind, expected);
        }
        let error = render(
            Args {
                command: Command::Show {
                    device: MAC.into(),
                    port: 99,
                },
            },
            &inventory,
        )
        .err()
        .unwrap();
        assert_eq!(error.kind, ErrorKind::NotFound);
    }

    #[test]
    fn missing_lists_duplicate_ports_and_unknown_roles_are_unverified() {
        let inventory = models(vec![device("Switch")]);
        assert_eq!(
            render(list(Some(MAC)), &inventory).err().unwrap().kind,
            ErrorKind::Unverified
        );

        let inventory = models(vec![switch(json!([
            {"faceplatePortNumber":19}, {"faceplatePortNumber":19}
        ]))]);
        assert_eq!(
            render(list(None), &inventory).err().unwrap().kind,
            ErrorKind::Unverified
        );

        let inventory = models(vec![device("Unclassified")]);
        assert_eq!(
            render(list(None), &inventory).err().unwrap().kind,
            ErrorKind::Unverified
        );
    }

    #[test]
    fn reported_ap_zero_port_is_listed_and_shown() {
        crate::cli::Cli::try_parse_from(["instantctl", "port", "show", MAC, "0"])
            .expect("AP uplink zero must reach the inventory role check");
        let ap = json!({
            "id":MAC,"macAddress":MAC,"name":"AP","deviceRole":"accessPoint",
            "radios":[{},{}],
            "ethernetPorts":[{"faceplatePortNumber":0,"portNumber":0,"trunkNumber":null}]
        });
        let inventory = models(vec![ap]);
        let result = render(list(Some(MAC)), &inventory).unwrap();
        assert_eq!(result.data[0]["Port"], 0);
        let result = render(list(None), &inventory).unwrap();
        assert_eq!(result.data[0]["Port"], 0);

        let shown = render(
            Args {
                command: Command::Show {
                    device: MAC.into(),
                    port: 0,
                },
            },
            &inventory,
        )
        .unwrap();
        assert_eq!(shown.data["port"]["port_number"], 0);
        assert_eq!(shown.data["port"]["api_port_number"], 0);
        assert_eq!(shown.data["port"]["trunk_number"], Value::Null);
    }

    #[test]
    fn switch_zero_is_rejected_for_list_and_show() {
        let error = render(
            list(Some(MAC)),
            &models(vec![switch(json!([{"faceplatePortNumber":0}]))]),
        )
        .err()
        .unwrap();
        assert_eq!(error.kind, ErrorKind::Unverified);

        let error = render(
            Args {
                command: Command::Show {
                    device: MAC.into(),
                    port: 0,
                },
            },
            &models(vec![switch(json!([{"faceplatePortNumber":1}]))]),
        )
        .err()
        .unwrap();
        assert_eq!(error.kind, ErrorKind::Usage);
    }
}
