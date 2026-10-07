use std::collections::BTreeMap;

use crate::mutation::Options;
use clap::{Args as ClapArgs, Subcommand, ValueEnum};
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
    /// List configured or active link aggregation groups.
    #[command(alias = "ls")]
    List {
        /// Select one switch by exact name or MAC address.
        #[arg(long, value_name = "device")]
        switch: Option<String>,
    },
    /// Configure an unused LAG slot with at least two member ports.
    Create {
        device: String,
        #[arg(value_parser=clap::value_parser!(u64).range(1..))]
        trunk: u64,
        #[arg(long,required=true,value_delimiter=',',num_args=1..,value_parser=clap::value_parser!(u64).range(1..))]
        ports: Vec<u64>,
        #[arg(long, value_enum, default_value = "lacp")]
        mode: LagMode,
        #[arg(long)]
        force: bool,
        #[command(flatten)]
        options: Options,
    },
    /// Break a configured LAG (member changes require --force).
    Remove {
        device: String,
        #[arg(value_parser=clap::value_parser!(u64).range(1..))]
        trunk: u64,
        #[arg(long)]
        force: bool,
        #[command(flatten)]
        options: Options,
    },
}
#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum LagMode {
    Static,
    Lacp,
}
impl Args {
    pub fn preflight(&self, context: &CommandContext) -> Result<(), Error> {
        match &self.command {
            Command::Create { options, .. } | Command::Remove { options, .. } => {
                options.preflight(context.token_source)
            }
            _ => Ok(()),
        }
    }
}

pub async fn run(args: Args, context: &CommandContext) -> anyhow::Result<CommandResult> {
    let api = super::site::read::client(context)?;
    match args.command {
        Command::Create {
            device,
            trunk,
            ports,
            mode,
            force,
            options,
        } => {
            let site = context.site.as_deref().ok_or_else(|| {
                Error::new(ErrorKind::Config, "LAG operations require --site <site-id>")
            })?;
            let prepared = instantctl_api::ports::plan_lag_create(
                &api,
                site,
                &device,
                trunk,
                ports,
                match mode {
                    LagMode::Static => "static",
                    LagMode::Lacp => "lacp",
                },
                force,
            )
            .await?;
            return crate::mutation::execute(
                &prepared.backend,
                prepared.plan,
                "lag.create",
                prepared.target,
                options,
                context,
            )
            .await;
        }
        Command::Remove {
            device,
            trunk,
            force,
            options,
        } => {
            let site = context.site.as_deref().ok_or_else(|| {
                Error::new(ErrorKind::Config, "LAG operations require --site <site-id>")
            })?;
            let prepared =
                instantctl_api::ports::plan_lag_remove(&api, site, &device, trunk, force).await?;
            return crate::mutation::execute(
                &prepared.backend,
                prepared.plan,
                "lag.remove",
                prepared.target,
                options,
                context,
            )
            .await;
        }
        _ => {}
    }
    let inventory = super::site::read::inventory(&api, context).await?;
    render(args, &inventory).map_err(Into::into)
}

fn render(args: Args, models: &[Device]) -> Result<CommandResult, Error> {
    let Command::List { switch } = args.command else {
        return Err(Error::new(
            ErrorKind::Config,
            "LAG read renderer received a write command",
        ));
    };
    let inventory = super::site::read::values(models)?;
    let data = if let Some(selector) = switch {
        let device = super::site::read::select(&inventory, &selector, "switch")?;
        check_switch_role(device)?;
        Value::Array(rows(device, false)?)
    } else {
        let mut all = Vec::new();
        for device in &inventory {
            if has_switch_collection(device) {
                all.extend(rows(device, true)?);
            } else if device.get("radios").is_none() {
                return Err(super::site::read::incomplete(
                    "inventory device has no observed switch or AP collection",
                ));
            }
        }
        Value::Array(all)
    };
    Ok(CommandResult::success(data))
}

fn has_switch_collection(device: &Value) -> bool {
    device.get("trunkPorts").is_some()
        || (device.get("ethernetPorts").is_some() && device.get("radios").is_none())
}

fn check_switch_role(device: &Value) -> Result<(), Error> {
    if device.get("radios").is_some() && device.get("trunkPorts").is_none() {
        return Err(Error::new(
            ErrorKind::Usage,
            "selected device reports radios, not switch ports",
        ));
    }
    Ok(())
}

fn rows(device: &Value, include_device: bool) -> Result<Vec<Value>, Error> {
    let configurations = super::site::read::array(device, "trunkPorts")?;
    let ports = match device.get("ethernetPorts") {
        Some(_) => Some(super::port::validated_ports(device)?),
        None => None,
    };

    let mut configured = BTreeMap::<u64, Value>::new();
    for trunk in configurations {
        let number = positive_number(
            trunk.get("trunkNumber"),
            "trunkPorts entry has an invalid trunkNumber",
        )?;
        if configured.insert(number, trunk.clone()).is_some() {
            return Err(super::site::read::incomplete(
                "trunkPorts contains duplicate trunkNumber values",
            ));
        }
    }

    let mut members = BTreeMap::<u64, Vec<u64>>::new();
    if let Some(ports) = ports.as_deref() {
        for port in ports {
            let faceplate = port
                .get("faceplatePortNumber")
                .and_then(Value::as_u64)
                .ok_or_else(|| {
                    super::site::read::incomplete("port is missing a valid faceplatePortNumber")
                })?;
            if let Some(trunk) = port.get("trunkNumber").filter(|value| !value.is_null()) {
                let number = positive_number(Some(trunk), "port has an invalid trunkNumber")?;
                members.entry(number).or_default().push(faceplate);
            }
        }
    }

    let mut numbers: Vec<u64> = configured.keys().chain(members.keys()).copied().collect();
    numbers.sort_unstable();
    numbers.dedup();
    let mut result = Vec::with_capacity(numbers.len());
    for number in numbers {
        let configuration = configured.get(&number);
        let mut item = Map::new();
        if include_device {
            item.insert(
                "device".into(),
                device.get("name").cloned().unwrap_or(Value::Null),
            );
        }
        item.insert("trunk_number".into(), Value::String(number.to_string()));
        item.insert(
            "name".into(),
            configuration
                .and_then(|value| value.get("name"))
                .cloned()
                .unwrap_or(Value::Null),
        );
        item.insert(
            "members".into(),
            match ports {
                Some(_) => {
                    let mut values = members.remove(&number).unwrap_or_default();
                    values.sort_unstable();
                    Value::Array(values.into_iter().map(Value::from).collect())
                }
                None => Value::Null,
            },
        );
        item.insert(
            "configuration".into(),
            configuration.cloned().unwrap_or(Value::Null),
        );
        result.push(Value::Object(item));
    }
    Ok(result)
}

fn positive_number(value: Option<&Value>, message: &str) -> Result<u64, Error> {
    value
        .and_then(Value::as_u64)
        .filter(|number| *number > 0)
        .ok_or_else(|| super::site::read::incomplete(message))
}

#[cfg(test)]
mod tests {
    use super::super::site::read::tests::{MAC, device, devices};
    use super::*;
    use serde_json::json;

    fn switch(ethernet_ports: Value, trunk_ports: Value) -> Value {
        let mut item = device("Switch");
        item["ethernetPorts"] = ethernet_ports;
        item["trunkPorts"] = trunk_ports;
        item
    }

    fn models(items: Vec<Value>) -> Vec<Device> {
        devices(items)
    }
    fn list(switch: Option<&str>) -> Args {
        Args {
            command: Command::List {
                switch: switch.map(str::to_owned),
            },
        }
    }

    #[test]
    fn list_shows_configured_empty_trunks_and_observed_members() {
        let inventory = models(vec![switch(
            json!([
                {"faceplatePortNumber":1,"trunkNumber":4},
                {"faceplatePortNumber":2,"trunkNumber":4},
                {"faceplatePortNumber":3,"trunkNumber":null}
            ]),
            json!([{"trunkNumber":4,"name":"nas"},{"trunkNumber":5,"name":"unused"}]),
        )]);
        let result = render(list(None), &inventory).unwrap();

        assert_eq!(result.data.as_array().unwrap().len(), 2);
        assert_eq!(result.data[0]["device"], "Switch");
        assert_eq!(result.data[0]["trunk_number"], "4");
        assert_eq!(result.data[0]["name"], "nas");
        assert_eq!(result.data[0]["members"], json!([1, 2]));
        assert_eq!(result.data[1]["trunk_number"], "5");
        assert_eq!(result.data[1]["members"], json!([]));
    }

    #[test]
    fn list_accepts_anonymous_ap_zero_port_beside_switch_ports() {
        let ap = json!({
            "id":"00:11:22:33:44:55","macAddress":"00:11:22:33:44:55","name":"AP",
            "deviceRole":"accessPoint","radios":[{},{}],
            "ethernetPorts":[{"faceplatePortNumber":0,"portNumber":0,"trunkNumber":null}],
            "trunkPorts":[]
        });
        let switch = switch(
            json!([{"faceplatePortNumber":1,"trunkNumber":4}]),
            json!([{"trunkNumber":4,"name":"uplink"}]),
        );
        let result = render(list(None), &models(vec![ap, switch])).unwrap();
        assert_eq!(result.data.as_array().unwrap().len(), 1);
        assert_eq!(result.data[0]["trunk_number"], "4");
        assert_eq!(result.data[0]["members"], json!([1]));
    }

    #[test]
    fn absent_member_collection_keeps_configured_trunk_members_unknown() {
        let inventory = models(vec![json!({
            "id":MAC,"macAddress":MAC,"name":"Switch",
            "trunkPorts":[{"trunkNumber":8,"name":"nas"}]
        })]);
        let result = render(list(Some(MAC)), &inventory).unwrap();
        assert_eq!(result.data.as_array().unwrap().len(), 1);
        assert_eq!(result.data[0]["trunk_number"], "8");
        assert_eq!(result.data[0]["members"], Value::Null);
        assert!(result.data[0].get("device").is_none());
    }

    #[test]
    fn selector_supports_exact_mac_and_reports_ambiguous_or_missing_names() {
        let inventory = models(vec![
            switch(json!([]), json!([])),
            json!({"id":"11:22:33:44:55:66","macAddress":"11:22:33:44:55:66","name":"Switch","ethernetPorts":[],"trunkPorts":[]}),
        ]);
        for (selector, expected) in [
            ("Switch", ErrorKind::Usage),
            ("missing", ErrorKind::NotFound),
        ] {
            assert_eq!(
                render(list(Some(selector)), &inventory).err().unwrap().kind,
                expected
            );
        }
        let inventory = models(vec![switch(json!([]), json!([]))]);
        assert!(
            render(list(Some(&MAC.to_ascii_uppercase())), &inventory)
                .unwrap()
                .data
                .as_array()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn missing_or_duplicate_trunks_and_unknown_default_role_are_unverified() {
        assert_eq!(
            render(list(Some(MAC)), &models(vec![device("Switch")]))
                .err()
                .unwrap()
                .kind,
            ErrorKind::Unverified
        );
        assert_eq!(
            render(
                list(Some(MAC)),
                &models(vec![switch(
                    json!([]),
                    json!([
                        {"trunkNumber":2}, {"trunkNumber":2}
                    ])
                )])
            )
            .err()
            .unwrap()
            .kind,
            ErrorKind::Unverified
        );
        assert_eq!(
            render(
                list(Some(MAC)),
                &models(vec![switch(json!([{"faceplatePortNumber":0}]), json!([]))])
            )
            .err()
            .unwrap()
            .kind,
            ErrorKind::Unverified
        );
        assert_eq!(
            render(
                list(Some(MAC)),
                &models(vec![switch(
                    json!([
                        {"faceplatePortNumber":1},
                        {"faceplatePortNumber":1}
                    ]),
                    json!([])
                )])
            )
            .err()
            .unwrap()
            .kind,
            ErrorKind::Unverified
        );
        assert_eq!(
            render(list(None), &models(vec![device("Unclassified")]))
                .err()
                .unwrap()
                .kind,
            ErrorKind::Unverified
        );
    }

    #[test]
    fn radio_only_selection_is_usage_and_known_radio_devices_are_skipped_by_default() {
        let ap = json!({"id":MAC,"macAddress":MAC,"name":"AP","radios":[]});
        assert_eq!(
            render(list(Some(MAC)), &models(vec![ap.clone()]))
                .err()
                .unwrap()
                .kind,
            ErrorKind::Usage
        );
        assert!(
            render(list(None), &models(vec![ap, switch(json!([]), json!([]))]))
                .unwrap()
                .data
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
}
