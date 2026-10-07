use clap::{Args as ClapArgs, Subcommand};
use instantctl_api::{
    Client, Error, ErrorKind, ProfileMetadata,
    client::profile::{validate_default_site, validate_protected_port_entry},
};
use serde_json::{Value, json};

use crate::{
    context::{CommandContext, CommandResult},
    credentials::CommandTokenSource,
};

#[derive(Debug, ClapArgs)]
pub struct Args {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Show the selected saved profile's metadata.
    Show,
    /// Change the selected saved profile's default site.
    DefaultSite {
        #[command(subcommand)]
        command: DefaultSiteCommand,
    },
    /// Manage protected switch ports for the selected saved profile.
    ProtectedPorts {
        #[command(subcommand)]
        command: ProtectedPortsCommand,
    },
}

#[derive(Debug, Subcommand)]
pub enum DefaultSiteCommand {
    /// Set the default site by UUID.
    Set { site_id: String },
}

#[derive(Debug, Subcommand)]
pub enum ProtectedPortsCommand {
    /// List protected switch ports.
    List,
    /// Add a protected switch port.
    Add { entry: String },
    /// Remove a protected switch port.
    Remove { entry: String },
}

impl Args {
    /// Validate local input before checking whether saved-profile credentials are available.
    pub fn preflight(&self, context: &CommandContext) -> Result<(), Error> {
        match &self.command {
            Command::Show => {}
            Command::DefaultSite {
                command: DefaultSiteCommand::Set { site_id },
            } => validate_default_site(site_id)?,
            Command::ProtectedPorts {
                command:
                    ProtectedPortsCommand::Add { entry } | ProtectedPortsCommand::Remove { entry },
            } => validate_protected_port_entry(entry)?,
            Command::ProtectedPorts {
                command: ProtectedPortsCommand::List,
            } => {}
        }
        if !context.token_source.uses_profile() {
            return Err(usage(
                "profile metadata commands require saved-profile credentials; unset HPE_INSTANT_ON_TOKEN and do not use --token-stdin",
            ));
        }
        Ok(())
    }
}

pub async fn run(args: Args, context: &CommandContext) -> anyhow::Result<CommandResult> {
    args.preflight(context)?;
    let source = context
        .token_source
        .load(&context.profile, context.timeout)?;
    let CommandTokenSource::Profile(source) = source else {
        return Err(usage("profile metadata commands require saved-profile credentials").into());
    };

    match args.command {
        Command::Show => Ok(CommandResult::success(show_data(
            &context.profile,
            source.profile_metadata().await?,
        ))),
        Command::DefaultSite {
            command: DefaultSiteCommand::Set { site_id },
        } => {
            let client = Client::new(source.clone(), context.timeout)?;
            let metadata = client.set_profile_default_site(&site_id).await?;
            Ok(CommandResult::success(show_data(
                &context.profile,
                metadata,
            )))
        }
        Command::ProtectedPorts {
            command: ProtectedPortsCommand::List,
        } => {
            let client = Client::new(source.clone(), context.timeout)?;
            let rows = client.profile_protected_ports().await?;
            Ok(CommandResult::success(protected_port_list_data(rows)))
        }
        Command::ProtectedPorts {
            command: ProtectedPortsCommand::Add { entry },
        } => {
            let client = Client::new(source.clone(), context.timeout)?;
            let metadata = client.add_profile_protected_port(&entry).await?;
            Ok(CommandResult::success(show_data(
                &context.profile,
                metadata,
            )))
        }
        Command::ProtectedPorts {
            command: ProtectedPortsCommand::Remove { entry },
        } => {
            let client = Client::new(source.clone(), context.timeout)?;
            let metadata = client.remove_profile_protected_port(&entry).await?;
            Ok(CommandResult::success(show_data(
                &context.profile,
                metadata,
            )))
        }
    }
}

fn show_data(profile: &str, metadata: ProfileMetadata) -> Value {
    json!({
        "profile": profile,
        "default_site": metadata.default_site,
        "protected_ports": metadata.protected_ports,
    })
}

fn protected_port_row(row: instantctl_api::client::profile::ProtectedPortSummary) -> Value {
    json!({
        "entry": row.entry,
        "switch_mac": row.switch_mac,
        "switch_name": row.switch_name,
        "faceplate": row.faceplate,
    })
}

fn protected_port_list_data(
    rows: Vec<instantctl_api::client::profile::ProtectedPortSummary>,
) -> Value {
    Value::Array(rows.into_iter().map(protected_port_row).collect())
}

fn usage(message: &str) -> Error {
    Error::new(ErrorKind::Usage, message)
}

#[cfg(test)]
#[path = "tests/profile.rs"]
mod tests;
