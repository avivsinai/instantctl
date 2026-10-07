use clap::Args as ClapArgs;
use instantctl_api::{Error, ErrorKind, device};

use crate::context::{CommandContext, CommandResult};

#[derive(Debug, ClapArgs)]
pub struct ReadArgs {
    /// Device MAC address or exact unique name.
    pub device: String,
}

pub async fn run_power_usage(
    args: ReadArgs,
    context: &CommandContext,
) -> anyhow::Result<CommandResult> {
    let site = context.site.as_deref().ok_or_else(|| {
        Error::new(
            ErrorKind::Config,
            "device power usage requires --site <site-id>",
        )
    })?;
    let client = context.client()?;
    let data = device::power_usage(&client, site, &args.device).await?;
    Ok(CommandResult::success(data))
}

pub async fn run_details(
    args: ReadArgs,
    context: &CommandContext,
) -> anyhow::Result<CommandResult> {
    let site = context.site.as_deref().ok_or_else(|| {
        Error::new(
            ErrorKind::Config,
            "device details requires --site <site-id>",
        )
    })?;
    let client = context.client()?;
    let data = device::details(&client, site, &args.device).await?;
    Ok(CommandResult::success(data))
}
