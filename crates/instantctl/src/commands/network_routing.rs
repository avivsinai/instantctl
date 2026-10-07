//! Read or update wired-network IP routing state.

use clap::Args as ClapArgs;
use instantctl_api::{Error, ErrorKind, client::network_routing};

use crate::{
    context::{CommandContext, CommandResult},
    mutation::Options,
};

#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Wired network ID or exact name.
    pub selector: String,
    /// Set IP routing to this state. Omit to read current routing details.
    #[arg(long)]
    pub enabled: Option<bool>,
    #[command(flatten)]
    pub options: Options,
}

impl Args {
    pub fn preflight(&self, context: &CommandContext) -> Result<(), Error> {
        if self.options.apply && self.enabled.is_none() {
            return Err(Error::new(
                ErrorKind::Config,
                "network routing --apply requires --enabled true or false",
            ));
        }
        self.options.preflight(context.token_source)
    }
}

pub async fn run(args: Args, context: &CommandContext) -> anyhow::Result<CommandResult> {
    args.preflight(context)?;
    let api = super::site::read::client(context)?;
    let site = super::site::read::site_id(&api, context).await?;
    let Some(enabled) = args.enabled else {
        let routing = network_routing::read(&api, &site, &args.selector).await?;
        return Ok(CommandResult::success(routing));
    };

    let prepared = network_routing::plan_update(&api, &site, &args.selector, enabled).await?;
    crate::mutation::execute(
        &prepared.backend,
        prepared.plan,
        "network.routing",
        prepared.target,
        args.options,
        context,
    )
    .await
}
