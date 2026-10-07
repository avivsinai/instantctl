use super::access_common::{self, ServersArgs, usage};
use crate::{
    context::{CommandContext, CommandResult},
    mutation::{self, Options},
    output::Format,
};
use clap::{Args as ClapArgs, Subcommand};
use instantctl_api::client::access::radius::{self, Patch};
use std::net::Ipv4Addr;

#[derive(Debug, ClapArgs)]
pub struct Args {
    #[command(subcommand)]
    pub command: Command,
}
#[derive(Debug, Subcommand)]
pub enum Command {
    /// List RADIUS profiles with shared secrets redacted.
    #[command(alias = "ls")]
    List,
    /// Show a profile by ID or exact unique name.
    Show { id: String },
    /// Create a profile. Secrets come from stdin or hidden terminal prompts.
    Create {
        new_name: String,
        #[command(flatten)]
        patch: PatchArgs,
        #[command(flatten)]
        options: Options,
    },
    /// Update a profile, preserving its complete fetched configuration.
    Update {
        id: String,
        #[command(flatten)]
        patch: PatchArgs,
        #[command(flatten)]
        options: Options,
    },
    /// Delete a profile that is not referenced by networks or devices.
    Delete {
        id: String,
        #[command(flatten)]
        options: Options,
    },
}
#[derive(Clone, Debug, Default, ClapArgs)]
pub struct PatchArgs {
    #[arg(long)]
    name: Option<String>,
    #[command(flatten)]
    servers: ServersArgs,
    #[arg(long)]
    secondary_enabled: Option<bool>,
    #[arg(long)]
    tls: Option<bool>,
    #[arg(long)]
    require_authenticator: Option<bool>,
    #[arg(long)]
    accounting: Option<bool>,
    #[arg(long)]
    server_timeout: Option<u8>,
    #[arg(long)]
    server_retries: Option<u8>,
    #[arg(long, conflicts_with = "clear_nas_identifier")]
    nas_identifier: Option<String>,
    #[arg(long, conflicts_with = "clear_nas_ip")]
    nas_ip: Option<Ipv4Addr>,
    #[arg(long)]
    clear_nas_identifier: bool,
    #[arg(long)]
    clear_nas_ip: bool,
}
impl PatchArgs {
    fn metadata(&self) -> Patch {
        let (primary, secondary) = self.servers.metadata();
        Patch {
            name: self.name.clone(),
            primary,
            secondary,
            secondary_enabled: self.secondary_enabled,
            tls: self.tls,
            require_authenticator: self.require_authenticator,
            accounting: self.accounting,
            timeout: self.server_timeout,
            retries: self.server_retries,
            nas_identifier: self.nas_identifier.clone(),
            nas_ip: self.nas_ip,
            clear_nas_identifier: self.clear_nas_identifier,
            clear_nas_ip: self.clear_nas_ip,
        }
    }
}
enum Action {
    Create,
    Update(String),
    Delete(String),
}
pub async fn run(args: Args, context: &CommandContext) -> anyhow::Result<CommandResult> {
    match args.command {
        Command::List | Command::Show { .. } => {
            let context = access_common::context(context).await?;
            let api = super::site::read::client(&context)?;
            let site = super::site::read::site_id(&api, &context).await?;
            let rows = radius::list(&api, &site).await?;
            Ok(CommandResult::success(match args.command {
                Command::Show { id } => radius::show(&rows, &id)?,
                _ => serde_json::to_value(
                    rows.iter()
                        .map(|row| {
                            if context.format == Format::Table {
                                row.summary()
                            } else {
                                row.details()
                            }
                        })
                        .collect::<Vec<_>>(),
                )?,
            }))
        }
        command => {
            let (action, patch_args, options, operation) = match command {
                Command::Create {
                    new_name,
                    mut patch,
                    options,
                } => {
                    if patch.name.is_some() {
                        return Err(usage("create uses its positional name; omit --name").into());
                    }
                    patch.name = Some(new_name);
                    (Action::Create, patch, options, "radius.create")
                }
                Command::Update { id, patch, options } => {
                    (Action::Update(id), patch, options, "radius.update")
                }
                Command::Delete { id, options } => (
                    Action::Delete(id),
                    PatchArgs::default(),
                    options,
                    "radius.delete",
                ),
                _ => return Err(usage("expected a RADIUS mutation").into()),
            };
            let create = matches!(action, Action::Create);
            let mut patch = patch_args.metadata();
            patch.validate()?;
            if create
                && (patch.primary.host.is_none()
                    || patch.secondary_enabled == Some(true) && patch.secondary.host.is_none())
            {
                return Err(usage("RADIUS creation requires a primary host and a host for any enabled secondary server").into());
            }
            if matches!(action, Action::Update(_))
                && patch.is_empty()
                && !patch_args.servers.has_secret()
            {
                return Err(usage("specify at least one RADIUS profile change").into());
            }
            options.preflight(context.token_source)?;
            if !matches!(action, Action::Delete(_)) {
                (patch.primary, patch.secondary) =
                    patch_args.servers.collect(context.token_source, create)?;
                if create {
                    patch.validate_create()?;
                } else {
                    patch.validate()?;
                }
            }
            let context = access_common::context(context).await?;
            let api = super::site::read::client(&context)?;
            let site = super::site::read::site_id(&api, &context).await?;
            let (backend, plan) = match action {
                Action::Create => radius::create(&api, &site, patch).await?,
                Action::Update(id) => radius::update(&api, &site, &id, patch).await?,
                Action::Delete(id) => radius::delete(&api, &site, &id).await?,
            };
            mutation::execute(
                &backend,
                plan,
                operation,
                backend.target(),
                options,
                &context,
            )
            .await
        }
    }
}
