use clap::Args as ClapArgs;
use instantctl_api::{Error, ErrorKind, client::replace};

use crate::context::{CommandContext, CommandResult};

#[derive(Debug, ClapArgs)]
pub struct ReadArgs {
    /// Device MAC address or exact unique name.
    pub device: String,
}

pub async fn run(args: ReadArgs, context: &CommandContext) -> anyhow::Result<CommandResult> {
    let site = context.site.as_deref().ok_or_else(|| {
        Error::new(
            ErrorKind::Config,
            "device replacement candidates require --site <site-id>",
        )
    })?;
    let client = context.client()?;
    let data = replace::candidates(&client, site, &args.device).await?;
    Ok(CommandResult::success(data))
}
