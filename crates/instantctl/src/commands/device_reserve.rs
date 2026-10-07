use std::net::Ipv4Addr;

use clap::Args as ClapArgs;
use instantctl_api::{
    device::reservations::{self, Reservation},
    mutation::Prepared,
};

use crate::{
    context::{CommandContext, CommandResult},
    mutation::Options,
};

#[derive(Debug, ClapArgs)]
pub struct ReserveArgs {
    /// Device MAC address or exact unique name.
    pub device: String,
    /// DHCP network ID reported in site inventory metadata.
    #[arg(long)]
    pub network: String,
    pub address: Ipv4Addr,
    #[command(flatten)]
    pub options: Options,
}

#[derive(Debug, ClapArgs)]
pub struct RemoveArgs {
    /// Device MAC address or exact unique name.
    pub device: String,
    #[command(flatten)]
    pub options: Options,
}

pub async fn reserve(args: ReserveArgs, context: &CommandContext) -> anyhow::Result<CommandResult> {
    run(
        &args.device,
        Some(Reservation {
            network_id: args.network,
            ip_address: args.address,
        }),
        args.options,
        context,
    )
    .await
}

pub async fn remove(args: RemoveArgs, context: &CommandContext) -> anyhow::Result<CommandResult> {
    run(&args.device, None, args.options, context).await
}

async fn run(
    device: &str,
    desired: Option<Reservation>,
    options: Options,
    context: &CommandContext,
) -> anyhow::Result<CommandResult> {
    let client = super::site::read::client(context)?;
    let site = super::site::read::site_id(&client, context).await?;
    let Prepared {
        backend,
        plan,
        target,
    } = reservations::plan(&client, &site, device, desired).await?;
    crate::mutation::execute(
        &backend,
        plan,
        "device.reserve_ip",
        target,
        options,
        context,
    )
    .await
}
