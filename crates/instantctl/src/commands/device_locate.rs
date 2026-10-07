use clap::{Args as ClapArgs, ValueEnum};
use instantctl_api::{Error, ErrorKind, locator::Locator};
use serde_json::json;

use crate::context::{CommandContext, CommandResult};

#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Device MAC address or exact unique name.
    pub device: String,
    #[arg(value_enum)]
    pub state: State,
    #[command(flatten)]
    pub options: crate::mutation::Options,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum State {
    On,
    Off,
}

pub async fn run(args: Args, context: &CommandContext) -> anyhow::Result<CommandResult> {
    let site = context
        .site
        .as_deref()
        .ok_or_else(|| Error::new(ErrorKind::Config, "device locate requires --site <site-id>"))?;
    let client = context.client()?;
    let desired = matches!(args.state, State::On);
    let (backend, plan) = Locator::plan(&client, site, &args.device, desired).await?;
    let target = json!({
        "device_id": backend.device_id(),
        "device_name": backend.device_name(),
    });
    crate::mutation::execute(
        &backend,
        plan,
        "device.locate",
        target,
        args.options,
        context,
    )
    .await
}
