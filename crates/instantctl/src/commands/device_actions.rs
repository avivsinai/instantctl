use std::time::Duration;

use clap::Args as ClapArgs;
use instantctl_api::{
    Error, ErrorKind,
    device::{plan_forget, plan_reboot},
};
use serde_json::json;

use crate::{
    context::{CommandContext, CommandResult},
    mutation::Options,
};

#[derive(Debug, ClapArgs)]
pub struct RebootArgs {
    /// Device MAC address or exact unique name.
    pub device: String,
    /// Maximum time to wait for device readback, in seconds.
    #[arg(long, default_value_t = 300, value_parser = clap::value_parser!(u64).range(1..))]
    pub readback_timeout: u64,
    #[command(flatten)]
    pub options: Options,
}

#[derive(Debug, ClapArgs)]
pub struct ForgetArgs {
    /// Device MAC address or exact unique name.
    pub device: String,
    #[command(flatten)]
    pub options: Options,
}

pub async fn run_reboot(
    args: RebootArgs,
    context: &CommandContext,
) -> anyhow::Result<CommandResult> {
    let site = context
        .site
        .as_deref()
        .ok_or_else(|| Error::new(ErrorKind::Config, "device reboot requires --site <site-id>"))?;
    let client = context.client()?;
    let prepared = plan_reboot(&client, site, &args.device).await?;
    let instantctl_api::device::Prepared {
        backend,
        plan,
        target,
    } = prepared;
    let readback_context = CommandContext {
        timeout: Duration::from_secs(args.readback_timeout),
        ..context.clone()
    };
    let mut result = crate::mutation::execute(
        &backend,
        plan,
        "device.reboot",
        target,
        args.options,
        &readback_context,
    )
    .await?;
    result.data["restart_observed"] = json!(backend.restart_observed());
    Ok(result)
}

pub async fn run_forget(
    args: ForgetArgs,
    context: &CommandContext,
) -> anyhow::Result<CommandResult> {
    let site = context
        .site
        .as_deref()
        .ok_or_else(|| Error::new(ErrorKind::Config, "device forget requires --site <site-id>"))?;
    let client = context.client()?;
    let prepared = plan_forget(&client, site, &args.device).await?;
    let instantctl_api::device::Prepared {
        backend,
        plan,
        target,
    } = prepared;
    crate::mutation::execute(
        &backend,
        plan,
        "device.forget",
        target,
        args.options,
        context,
    )
    .await
}
