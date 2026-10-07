use std::net::Ipv4Addr;

use clap::{Args as ClapArgs, ValueEnum};
use instantctl_api::{
    Error, ErrorKind,
    device::{ConfigChange, LedMode, StaticManagementIp, plan_update},
};

use crate::{
    context::{CommandContext, CommandResult},
    mutation::Options,
};

#[derive(Debug, ClapArgs)]
pub struct RenameArgs {
    /// Device MAC address or exact unique name.
    pub device: String,
    /// New device name.
    pub name: String,
    #[command(flatten)]
    pub options: Options,
}

#[derive(Debug, ClapArgs)]
pub struct LedArgs {
    /// Device MAC address or exact unique name.
    pub device: String,
    /// Requested device LED mode.
    #[arg(value_enum)]
    pub state: LedState,
    #[command(flatten)]
    pub options: Options,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum LedState {
    On,
    Quiet,
}

#[derive(Debug, ClapArgs)]
pub struct ManagementIpArgs {
    /// Device MAC address or exact unique name.
    pub device: String,
    /// Static IPv4 address.
    pub address: Ipv4Addr,
    /// Prefix length (0 through 32).
    #[arg(long, value_parser = clap::value_parser!(u8).range(0..=32))]
    pub prefix_length: u8,
    /// IPv4 gateway.
    #[arg(long)]
    pub gateway: Ipv4Addr,
    /// Primary IPv4 DNS server.
    #[arg(long)]
    pub dns: Ipv4Addr,
    /// Optional secondary IPv4 DNS server.
    #[arg(long)]
    pub secondary_dns: Option<Ipv4Addr>,
    #[command(flatten)]
    pub options: Options,
}

pub async fn run_rename(
    args: RenameArgs,
    context: &CommandContext,
) -> anyhow::Result<CommandResult> {
    let site = context
        .site
        .as_deref()
        .ok_or_else(|| Error::new(ErrorKind::Config, "device rename requires --site <site-id>"))?;
    let client = context.client()?;
    let prepared = plan_update(&client, site, &args.device, ConfigChange::Name(args.name)).await?;
    let operation = "device.rename";
    let instantctl_api::device::Prepared {
        backend,
        plan,
        target,
    } = prepared;
    crate::mutation::execute(&backend, plan, operation, target, args.options, context).await
}

pub async fn run_led(args: LedArgs, context: &CommandContext) -> anyhow::Result<CommandResult> {
    let site = context
        .site
        .as_deref()
        .ok_or_else(|| Error::new(ErrorKind::Config, "device LED requires --site <site-id>"))?;
    let client = context.client()?;
    let mode = match args.state {
        LedState::On => LedMode::On,
        LedState::Quiet => LedMode::Quiet,
    };
    let prepared = plan_update(&client, site, &args.device, ConfigChange::LedMode(mode)).await?;
    let instantctl_api::device::Prepared {
        backend,
        plan,
        target,
    } = prepared;
    crate::mutation::execute(&backend, plan, "device.led", target, args.options, context).await
}

pub async fn run_management_ip(
    args: ManagementIpArgs,
    context: &CommandContext,
) -> anyhow::Result<CommandResult> {
    let site = context.site.as_deref().ok_or_else(|| {
        Error::new(
            ErrorKind::Config,
            "device management IP requires --site <site-id>",
        )
    })?;
    let client = context.client()?;
    let change = ConfigChange::ManagementIp(StaticManagementIp {
        address: args.address,
        prefix_length: args.prefix_length,
        gateway: args.gateway,
        dns: args.dns,
        secondary_dns: args.secondary_dns,
    });
    let prepared = plan_update(&client, site, &args.device, change).await?;
    let instantctl_api::device::Prepared {
        backend,
        plan,
        target,
    } = prepared;
    crate::mutation::execute(
        &backend,
        plan,
        "device.management_ip",
        target,
        args.options,
        context,
    )
    .await
}
