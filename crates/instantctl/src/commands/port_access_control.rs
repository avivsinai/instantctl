use super::access_common::{self, ServersArgs, usage};
use crate::{
    context::{CommandContext, CommandResult},
    mutation::{self, Options},
};
use clap::{Args as ClapArgs, Subcommand};
use instantctl_api::client::access::port_access_control::{self, Patch};

#[derive(Debug, ClapArgs)]
pub struct Args {
    #[command(subcommand)]
    pub command: Command,
}
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Show site-wide RADIUS settings used by port access control.
    Show,
    /// Update RADIUS settings, preserving the complete fetched singleton.
    Update {
        #[command(flatten)]
        servers: ServersArgs,
        #[arg(long)]
        accounting: Option<bool>,
        #[arg(long)]
        secondary_enabled: Option<bool>,
        #[command(flatten)]
        options: Options,
    },
}
pub async fn run(args: Args, context: &CommandContext) -> anyhow::Result<CommandResult> {
    let prepared = match args.command {
        Command::Show => None,
        Command::Update {
            servers,
            accounting,
            secondary_enabled,
            options,
        } => {
            let (primary, secondary) = servers.metadata();
            let mut patch = Patch {
                accounting,
                secondary_enabled,
                primary,
                secondary,
            };
            patch.validate()?;
            if patch.is_empty() && !servers.has_secret() {
                return Err(usage("specify at least one port access-control change").into());
            }
            options.preflight(context.token_source)?;
            (patch.primary, patch.secondary) = servers.collect(context.token_source, false)?;
            patch.validate()?;
            Some((patch, options))
        }
    };
    let context = access_common::context(context).await?;
    let api = super::site::read::client(&context)?;
    let site = super::site::read::site_id(&api, &context).await?;
    if let Some((patch, options)) = prepared {
        let (backend, plan) = port_access_control::update(&api, &site, patch).await?;
        mutation::execute(
            &backend,
            plan,
            "port-access-control.update",
            backend.target(),
            options,
            &context,
        )
        .await
    } else {
        Ok(CommandResult::success(
            port_access_control::show(&api, &site).await?,
        ))
    }
}
