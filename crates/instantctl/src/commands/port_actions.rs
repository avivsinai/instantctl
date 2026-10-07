use crate::{
    context::{CommandContext, CommandResult},
    mutation::{self, Options},
};
use clap::{Args as ClapArgs, ValueEnum};
use instantctl_api::{
    Error, ErrorKind,
    ports::{self, MirrorPatch, PortConnection, PortPatch},
};
use serde_json::{Value, json};

#[derive(Debug, ClapArgs)]
pub struct SetArgs {
    pub device: String,
    #[arg(value_parser=clap::value_parser!(u64).range(1..))]
    pub port: u64,
    #[arg(long)]
    pub name: Option<String>,
    #[arg(long)]
    pub enabled: Option<bool>,
    /// Port profile ID or exact unique name.
    #[arg(long)]
    pub profile: Option<String>,
    #[arg(long)]
    pub poe_schedule: Option<bool>,
    #[arg(long, value_enum)]
    pub poe_mode: Option<PoeMode>,
    #[arg(long, value_enum)]
    pub poe_priority: Option<PoePriority>,
    #[arg(long, value_enum)]
    pub poe_management: Option<PoeManagement>,
    /// automatic or an API speed/duplex identifier reported by this port.
    #[arg(long)]
    pub speed_duplex: Option<String>,
    /// Allow changes to an uplink, LAG, or protected port.
    #[arg(long)]
    pub force: bool,
    #[command(flatten)]
    pub options: Options,
}
#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum PoeMode {
    AlwaysOn,
    Quick,
    Normal,
}
#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum PoePriority {
    Low,
    High,
    Critical,
}
#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum PoeManagement {
    None,
    ClassBased,
    UsageBased,
}
#[derive(Debug, ClapArgs)]
pub struct PortActionArgs {
    pub device: String,
    #[arg(value_parser=clap::value_parser!(u64).range(1..))]
    pub port: u64,
    #[command(flatten)]
    pub options: Options,
}
#[derive(Debug, ClapArgs)]
pub struct CableArgs {
    pub device: String,
    #[arg(value_parser=clap::value_parser!(u64).range(1..))]
    pub port: u64,
    #[arg(long)]
    pub force: bool,
    #[command(flatten)]
    pub options: Options,
}
#[derive(Debug, ClapArgs)]
pub struct ConnectivityArgs {
    pub device: String,
    pub address: String,
    #[command(flatten)]
    pub options: Options,
}
#[derive(Debug, ClapArgs)]
pub struct MirrorArgs {
    pub device: String,
    #[arg(long)]
    pub enabled: bool,
    #[arg(long,value_parser=clap::value_parser!(u64).range(1..))]
    pub destination: Option<u64>,
    #[arg(long,value_delimiter=',',value_parser=clap::value_parser!(u64).range(1..))]
    pub sources: Vec<u64>,
    #[arg(long, conflicts_with = "sources")]
    pub network: Option<String>,
    #[arg(long, value_enum, default_value = "both")]
    pub direction: Direction,
    #[arg(long)]
    pub force: bool,
    #[command(flatten)]
    pub options: Options,
}
#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum Direction {
    Both,
    Tx,
    Rx,
}
fn site(context: &CommandContext) -> Result<&str, Error> {
    context.site.as_deref().ok_or_else(|| {
        Error::new(
            ErrorKind::Config,
            "port operations require --site <site-id>",
        )
    })
}

pub async fn run_set(args: SetArgs, context: &CommandContext) -> anyhow::Result<CommandResult> {
    let api = context.client()?;
    let patch = PortPatch {
        name: args.name,
        enabled: args.enabled,
        profile: args.profile,
        poe_schedule: args.poe_schedule,
        poe_mode: args.poe_mode.map(|v| {
            match v {
                PoeMode::AlwaysOn => "alwaysOn",
                PoeMode::Quick => "quick",
                PoeMode::Normal => "normal",
            }
            .into()
        }),
        poe_priority: args.poe_priority.map(|v| {
            match v {
                PoePriority::Low => "low",
                PoePriority::High => "high",
                PoePriority::Critical => "critical",
            }
            .into()
        }),
        poe_management: args.poe_management.map(|v| {
            match v {
                PoeManagement::None => "none",
                PoeManagement::ClassBased => "class-based",
                PoeManagement::UsageBased => "usage-based",
            }
            .into()
        }),
        speed_duplex: args.speed_duplex,
    };
    let prepared = ports::plan_port_set(
        &api,
        site(context)?,
        &args.device,
        args.port,
        patch,
        args.force,
    )
    .await?;
    mutation::execute(
        &prepared.backend,
        prepared.plan,
        "port.set",
        prepared.target,
        args.options,
        context,
    )
    .await
}
pub async fn run_cycle(
    args: PortActionArgs,
    context: &CommandContext,
) -> anyhow::Result<CommandResult> {
    let api = context.client()?;
    let prepared = ports::plan_power_cycle(&api, site(context)?, &args.device, args.port).await?;
    mutation::execute(
        &prepared.backend,
        prepared.plan,
        "port.power-cycle",
        prepared.target,
        args.options,
        context,
    )
    .await
}
pub async fn run_mirror(
    args: MirrorArgs,
    context: &CommandContext,
) -> anyhow::Result<CommandResult> {
    let api = context.client()?;
    let patch = MirrorPatch {
        enabled: args.enabled,
        destination: args.destination,
        sources: args.sources,
        network: args.network,
        direction: match args.direction {
            Direction::Both => "both",
            Direction::Tx => "tx",
            Direction::Rx => "rx",
        }
        .into(),
    };
    let prepared =
        ports::plan_mirror(&api, site(context)?, &args.device, patch, args.force).await?;
    mutation::execute(
        &prepared.backend,
        prepared.plan,
        "port.mirror",
        prepared.target,
        args.options,
        context,
    )
    .await
}
pub async fn run_cable(args: CableArgs, context: &CommandContext) -> anyhow::Result<CommandResult> {
    let api = context.client()?;
    let prepared = instantctl_api::client::port_diagnostics::plan_cable_test(
        &api,
        site(context)?,
        &args.device,
        args.port,
        args.force,
    )
    .await?;
    let mut result = mutation::execute(
        &prepared.backend,
        prepared.plan,
        "port.cable-test",
        prepared.target,
        args.options,
        context,
    )
    .await?;
    if args.options.apply {
        result.data["diagnostic"] = prepared.backend.details();
    }
    Ok(result)
}
pub async fn run_connectivity(
    args: ConnectivityArgs,
    context: &CommandContext,
) -> anyhow::Result<CommandResult> {
    let api = context.client()?;
    let prepared = instantctl_api::client::port_diagnostics::plan_connectivity_test(
        &api,
        site(context)?,
        &args.device,
        args.address,
    )
    .await?;
    let mut result = mutation::execute(
        &prepared.backend,
        prepared.plan,
        "port.connectivity-test",
        prepared.target,
        args.options,
        context,
    )
    .await?;
    if args.options.apply {
        result.data["diagnostic"] = prepared.backend.details();
    }
    Ok(result)
}
pub async fn run_find(selector: String, context: &CommandContext) -> anyhow::Result<CommandResult> {
    let api = context.client()?;
    let rows = ports::find_port(&api, site(context)?, &selector).await?;
    Ok(CommandResult::success(connection_rows(&rows)))
}
fn connection_rows(rows: &[PortConnection]) -> Value {
    Value::Array(rows.iter().map(|r|json!({"client_mac":r.client_mac,"client_name":r.client_name,"ip":r.ip,"device_mac":r.device_mac,"device_name":r.device_name,"port_idx":r.port_idx,"api_port_number":r.api_port_number,"connected":r.connected,"powered":r.powered,"trunk_number":r.trunk_number})).collect())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unknown_connection_is_null_and_faceplate_feeds_show() {
        let data = connection_rows(&[PortConnection {
            client_mac: "11:22:33:44:55:66".into(),
            client_name: None,
            ip: None,
            device_mac: "aa:bb:cc:dd:ee:ff".into(),
            device_name: Some("Switch".into()),
            port_idx: 7,
            api_port_number: 0,
            connected: None,
            powered: None,
            trunk_number: None,
        }]);
        assert_eq!(data[0]["port_idx"], 7);
        assert_eq!(data[0]["api_port_number"], 0);
        assert!(data[0]["connected"].is_null());
        assert!(data[0]["powered"].is_null());
    }
}
