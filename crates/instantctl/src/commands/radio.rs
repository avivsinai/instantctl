use clap::{Args as ClapArgs, Subcommand};
use instantctl_api::client::radio::{self, Band, BandMapping, Patch, Power, Width};
use instantctl_api::client::reads::Device;
use instantctl_api::{Error, ErrorKind};
use serde_json::{Map, Value};

use crate::context::{CommandContext, CommandResult};
use crate::mutation::Options;

#[derive(Debug, ClapArgs)]
pub struct Args {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// List the reported operational state of AP radios.
    #[command(alias = "ls")]
    List {
        /// Select one AP by exact name or MAC address.
        #[arg(long, value_name = "device")]
        ap: Option<String>,
    },
    /// Read or set the site radio plan.
    Plan(PlanArgs),
    /// Read or set an AP's radio overrides.
    Override(OverrideArgs),
}

#[derive(Debug, ClapArgs)]
pub struct PlanArgs {
    #[command(subcommand)]
    command: PlanCommand,
}
#[derive(Debug, Subcommand)]
enum PlanCommand {
    /// Read configured ranges and offered channels from the portal.
    Get,
    /// Plan a site radio change; --apply sends it once and verifies readback.
    Set {
        #[command(flatten)]
        settings: Settings,
        #[command(flatten)]
        options: Options,
    },
}
#[derive(Debug, ClapArgs)]
pub struct OverrideArgs {
    #[command(subcommand)]
    command: OverrideCommand,
}
#[derive(Debug, Subcommand)]
enum OverrideCommand {
    /// Read an AP's settings and inheritance flags, selected by MAC or unique name.
    Get { ap: String },
    /// Plan specific AP settings or restore inheritance from the site plan.
    Set {
        ap: String,
        #[command(flatten)]
        settings: Settings,
        /// Use the site configuration for --band, preserving saved AP settings.
        #[arg(long, requires = "band", conflicts_with_all = ["width", "channels", "min_power", "max_power"])]
        inherit: bool,
        /// Use the site radio-band mapping, preserving the saved AP mapping.
        #[arg(long, conflicts_with = "band_mapping")]
        inherit_band_mapping: bool,
        #[command(flatten)]
        options: Options,
    },
}
#[derive(Debug, ClapArgs)]
struct Settings {
    /// Radio band: 2.4ghz, 5ghz or 6ghz.
    #[arg(long)]
    band: Option<Band>,
    /// Channel width: 20mhz, 40mhz, 80mhz, 160mhz or 320mhz; must be offered.
    #[arg(long, requires = "band")]
    width: Option<Width>,
    /// Comma-separated selected channels from this radio's offered list.
    #[arg(long, requires = "band", value_delimiter = ',')]
    channels: Option<Vec<u16>>,
    /// Lower power bound: 6dbm through 33dbm in steps of 3, or regulatoryMax.
    #[arg(long, requires = "band")]
    min_power: Option<Power>,
    /// Upper power bound from the same band-specific power choices.
    #[arg(long, requires = "band")]
    max_power: Option<Power>,
    /// Dual-radio mapping: 2.4ghz_and_5ghz, 2.4ghz_and_6ghz or 5ghz_and_6ghz.
    #[arg(long)]
    band_mapping: Option<BandMapping>,
}
impl Settings {
    fn patch(self) -> Patch {
        Patch {
            band: self.band,
            width: self.width,
            channels: self.channels,
            min_power: self.min_power,
            max_power: self.max_power,
            mapping: self.band_mapping,
            ..Patch::default()
        }
    }
}

pub async fn run(args: Args, context: &CommandContext) -> anyhow::Result<CommandResult> {
    let (selector, change) = match args.command {
        Command::List { ap } => {
            let context = context.resolve_profile_default().await?;
            let api = super::site::read::client(&context)?;
            let inventory = super::site::read::inventory(&api, &context).await?;
            return render(
                Args {
                    command: Command::List { ap },
                },
                &inventory,
            )
            .map_err(Into::into);
        }
        Command::Plan(PlanArgs {
            command: PlanCommand::Get,
        }) => (None, None),
        Command::Plan(PlanArgs {
            command: PlanCommand::Set { settings, options },
        }) => (None, Some((settings.patch(), options))),
        Command::Override(OverrideArgs {
            command: OverrideCommand::Get { ap },
        }) => (Some(ap), None),
        Command::Override(OverrideArgs {
            command:
                OverrideCommand::Set {
                    ap,
                    settings,
                    inherit,
                    inherit_band_mapping,
                    options,
                },
        }) => {
            let mut patch = settings.patch();
            patch.inherit_config = inherit;
            patch.inherit_mapping = inherit_band_mapping;
            (Some(ap), Some((patch, options)))
        }
    };
    if let Some((patch, options)) = &change {
        patch.validate(selector.is_some())?;
        options.preflight(context.token_source)?;
    }
    let context = context.resolve_profile_default().await?;
    let api = super::site::read::client(&context)?;
    let site = super::site::read::site_id(&api, &context).await?;
    match (selector, change) {
        (None, None) => Ok(CommandResult::success(radio::site_plan(&api, &site).await?)),
        (Some(ap), None) => Ok(CommandResult::success(
            radio::ap_override(&api, &site, &ap).await?,
        )),
        (None, Some((patch, options))) => {
            let prepared = radio::set_site(&api, &site, patch).await?;
            crate::mutation::execute(
                &prepared.backend,
                prepared.plan,
                "radio.plan.set",
                prepared.target,
                options,
                &context,
            )
            .await
        }
        (Some(ap), Some((patch, options))) => {
            let prepared = radio::set_ap(&api, &site, &ap, patch).await?;
            crate::mutation::execute(
                &prepared.backend,
                prepared.plan,
                "radio.override.set",
                prepared.target,
                options,
                &context,
            )
            .await
        }
    }
}

fn render(args: Args, models: &[Device]) -> Result<CommandResult, Error> {
    let Command::List { ap } = args.command else {
        return Err(Error::new(ErrorKind::Usage, "expected radio list"));
    };
    let inventory = super::site::read::values(models)?;
    let data = if let Some(selector) = ap {
        let device = super::site::read::select(&inventory, &selector, "AP")?;
        check_ap_role(device)?;
        Value::Array(rows(device, false)?)
    } else {
        let mut all = Vec::new();
        for device in &inventory {
            if device.get("radios").is_some() {
                all.extend(rows(device, true)?);
            } else if device.get("ethernetPorts").is_none() && device.get("trunkPorts").is_none() {
                return Err(super::site::read::incomplete(
                    "inventory device has no observed AP or switch collection",
                ));
            }
        }
        Value::Array(all)
    };
    Ok(CommandResult::success(data))
}

fn check_ap_role(device: &Value) -> Result<(), Error> {
    if device.get("radios").is_none()
        && (device.get("ethernetPorts").is_some() || device.get("trunkPorts").is_some())
    {
        return Err(Error::new(
            ErrorKind::Usage,
            "selected device reports switch ports, not AP radios",
        ));
    }
    Ok(())
}

fn rows(device: &Value, include_device: bool) -> Result<Vec<Value>, Error> {
    let radios = super::site::read::array(device, "radios")?;
    Ok(radios
        .iter()
        .map(|radio| {
            let fields = [
                ("band", "band"),
                ("channel", "channel"),
                ("channel_width", "channelWidth"),
                ("tx_power_eirp_dbm", "txPowerEirpInDbm"),
                ("utilization_percent", "utilizationPercent"),
                ("wireless_clients_count", "wirelessClientsCount"),
            ];
            let mut row = Map::new();
            if include_device {
                row.insert(
                    "device".into(),
                    device.get("name").cloned().unwrap_or(Value::Null),
                );
            }
            if let Value::Object(projected) = super::site::read::project(radio, &fields) {
                row.extend(projected);
            }
            Value::Object(row)
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::super::site::read::tests::{MAC, device, devices};
    use super::*;
    use serde_json::json;

    fn ap(name: &str, radios: Value) -> Value {
        json!({"id":MAC,"macAddress":MAC,"name":name,"radios":radios})
    }
    fn models(items: Vec<Value>) -> Vec<Device> {
        devices(items)
    }
    fn list(ap: Option<&str>) -> Args {
        Args {
            command: Command::List {
                ap: ap.map(str::to_owned),
            },
        }
    }

    #[test]
    fn list_maps_radio_fields_and_preserves_unknown_as_null() {
        let inventory = models(vec![ap(
            "AP kitchen",
            json!([
                {"band":"5GHz","channel":36,"channelWidth":"mhz80","txPowerEirpInDbm":18.5,
                 "utilizationPercent":24,"wirelessClientsCount":7},
                {"band":"2.4GHz"}
            ]),
        )]);
        let result = render(list(None), &inventory).unwrap();

        assert_eq!(result.data[0]["device"], "AP kitchen");
        assert_eq!(result.data[0]["band"], "5GHz");
        assert_eq!(result.data[0]["channel_width"], "mhz80");
        assert_eq!(result.data[0]["tx_power_eirp_dbm"], 18.5);
        assert_eq!(result.data[0]["utilization_percent"], 24.0);
        assert_eq!(result.data[0]["wireless_clients_count"], 7);
        assert!(result.data[1]["channel"].is_null());
        assert!(result.data[1]["channel_width"].is_null());
        assert!(result.data[1]["tx_power_eirp_dbm"].is_null());
    }

    #[test]
    fn ap_selector_accepts_exact_mac_and_omits_device_column() {
        let inventory = models(vec![ap(
            "AP kitchen",
            json!([{"band":"5GHz","channel":36}]),
        )]);
        let result = render(list(Some(&MAC.to_ascii_uppercase())), &inventory).unwrap();
        assert_eq!(result.data[0]["band"], "5GHz");
        assert!(result.data[0].get("device").is_none());
    }

    #[test]
    fn selector_distinguishes_ambiguity_from_not_found() {
        let inventory = models(vec![
            ap("AP", json!([])),
            json!({"id":"11:22:33:44:55:66","macAddress":"11:22:33:44:55:66","name":"AP","radios":[]}),
        ]);
        for (selector, expected) in [("AP", ErrorKind::Usage), ("missing", ErrorKind::NotFound)] {
            assert_eq!(
                render(list(Some(selector)), &inventory).err().unwrap().kind,
                expected
            );
        }
    }

    #[test]
    fn missing_radios_unknown_role_and_switch_only_selection_are_distinguished() {
        assert_eq!(
            render(list(Some(MAC)), &models(vec![device("AP")]))
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

        let mut switch = device("Switch");
        switch["ethernetPorts"] = json!([]);
        assert_eq!(
            render(list(Some(MAC)), &models(vec![switch.clone()]))
                .err()
                .unwrap()
                .kind,
            ErrorKind::Usage
        );
        assert!(
            render(list(None), &models(vec![switch]))
                .unwrap()
                .data
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
}
