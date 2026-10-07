//! List and show switch stacks from the validated site collection.

use clap::{Args as ClapArgs, Subcommand};
use instantctl_api::client::stacks;

use crate::context::{CommandContext, CommandResult};

#[derive(Debug, ClapArgs)]
pub struct Args {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// List validated switch stacks at the selected site.
    #[command(alias = "ls")]
    List,
    /// Show one stack by exact ID or exact unique name.
    Show { selector: String },
}

pub async fn run(args: Args, context: &CommandContext) -> anyhow::Result<CommandResult> {
    let api = super::site::read::client(context)?;
    let site = super::site::read::site_id(&api, context).await?;
    let stacks = stacks::list(&api, &site).await?;
    let data = match args.command {
        Command::List => serde_json::to_value(stacks)?,
        Command::Show { selector } => serde_json::to_value(stacks::select(&stacks, &selector)?)?,
    };
    Ok(CommandResult::success(data))
}
