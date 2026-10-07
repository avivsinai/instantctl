use std::net::Ipv4Addr;

use clap::{Args as ClapArgs, Subcommand, ValueEnum};
use instantctl_api::{
    Error, ErrorKind,
    client::network::{
        self, CreatePortMembership, DhcpPatch, DnsMode, Network, NetworkMutation, NetworkType,
        Patch, SharedServiceMutation, SharedServicesMutation,
    },
};
use serde_json::Value;

use crate::{
    context::{CommandContext, CommandResult},
    mutation::{self, Options},
};

#[derive(Debug, ClapArgs)]
pub struct Args {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// List wired networks and VLANs at the selected site.
    #[command(alias = "ls")]
    List,
    /// Show wired configuration by ID or exact unique name.
    Show { id: String },
    /// Read or change routing on an eligible wired network.
    Routing(super::network_routing::Args),
    /// Create a wired network from the site's server-supplied defaults.
    Create {
        new_name: String,
        /// Create with no assigned ports or trunks.
        #[arg(long)]
        no_port_membership: bool,
        #[command(flatten)]
        patch: PatchArgs,
        #[command(flatten)]
        options: Options,
    },
    /// Update wired configuration, preserving the complete fetched object.
    Update {
        id: String,
        #[command(flatten)]
        patch: PatchArgs,
        #[command(flatten)]
        options: Options,
    },
    /// Delete a wired network. Assigned or unknown port mappings require --yes.
    Delete {
        id: String,
        #[command(flatten)]
        options: Options,
    },
    /// Update DHCP scope, pool, domain, and DNS configuration.
    Dhcp {
        id: String,
        #[arg(long)]
        enabled: Option<bool>,
        #[command(flatten)]
        scope: ScopeArgs,
        #[command(flatten)]
        options: Options,
    },
    /// Configure site discovery and services shared from other networks.
    SharedServices {
        #[command(subcommand)]
        command: ServicesCommand,
    },
}

#[derive(Debug, Subcommand)]
pub enum ServicesCommand {
    /// Show whether shared services are enabled at the site.
    Status,
    /// Enable shared services at the site.
    Enable {
        #[command(flatten)]
        options: Options,
    },
    /// Disable shared services at the site.
    Disable {
        #[command(flatten)]
        options: Options,
    },
    /// List local and other-network service groups for a wired network.
    List { id: String },
    /// Share an other-network service group selected by MAC or exact unique name.
    Share {
        id: String,
        service: String,
        #[command(flatten)]
        options: Options,
    },
    /// Stop sharing an other-network service group.
    Unshare {
        id: String,
        service: String,
        #[command(flatten)]
        options: Options,
    },
}

#[derive(Clone, Debug, Default, ClapArgs)]
pub struct PatchArgs {
    #[arg(long)]
    name: Option<String>,
    #[arg(long)]
    enabled: Option<bool>,
    #[arg(long = "type", value_enum)]
    network_type: Option<TypeKind>,
    #[arg(long = "vlan")]
    vlan_id: Option<u16>,
    #[arg(long)]
    dhcp_enabled: Option<bool>,
    #[command(flatten)]
    scope: ScopeArgs,
}

#[derive(Clone, Debug, Default, ClapArgs)]
pub struct ScopeArgs {
    #[arg(long)]
    gateway: Option<Ipv4Addr>,
    #[arg(long)]
    prefix_length: Option<u8>,
    #[arg(long)]
    start: Option<Ipv4Addr>,
    #[arg(long)]
    end: Option<Ipv4Addr>,
    /// Set the DHCP domain suffix; an empty value clears it.
    #[arg(long)]
    domain_name: Option<String>,
    #[arg(long, value_enum)]
    dns_mode: Option<DnsKind>,
    #[arg(long)]
    primary_dns: Option<Ipv4Addr>,
    #[arg(long)]
    secondary_dns: Option<Ipv4Addr>,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum TypeKind {
    Employee,
    Guest,
    Voice,
}
#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum DnsKind {
    Automatic,
    Infrastructure,
    Custom,
}

impl ScopeArgs {
    fn into_patch(self, enabled: Option<bool>) -> DhcpPatch {
        DhcpPatch {
            enabled,
            gateway: self.gateway,
            prefix_length: self.prefix_length,
            start: self.start,
            end: self.end,
            domain_name: self.domain_name,
            dns_mode: self.dns_mode.map(|mode| match mode {
                DnsKind::Automatic => DnsMode::Automatic,
                DnsKind::Infrastructure => DnsMode::Infrastructure,
                DnsKind::Custom => DnsMode::Custom,
            }),
            primary_dns: self.primary_dns,
            secondary_dns: self.secondary_dns,
        }
    }
}
impl PatchArgs {
    fn into_patch(self) -> Patch {
        Patch {
            name: self.name,
            enabled: self.enabled,
            vlan_id: self.vlan_id,
            network_type: self.network_type.map(|kind| match kind {
                TypeKind::Employee => NetworkType::Employee,
                TypeKind::Guest => NetworkType::Guest,
                TypeKind::Voice => NetworkType::Voice,
            }),
            dhcp: self.scope.into_patch(self.dhcp_enabled),
        }
    }
}

pub async fn run(args: Args, context: &CommandContext) -> anyhow::Result<CommandResult> {
    match args.command {
        Command::List | Command::Show { .. } => {
            let context = with_profile_site(context).await?;
            let api = super::site::read::client(&context)?;
            let site = super::site::read::site_id(&api, &context).await?;
            let networks = network::list(&api, &site).await?;
            render(args, &networks)
        }
        Command::SharedServices { command } => run_services(command, context).await,
        Command::Routing(args) => {
            args.preflight(context)?;
            let context = with_profile_site(context).await?;
            super::network_routing::run(args, &context).await
        }
        command => run_mutation(command, context).await,
    }
}

async fn with_profile_site(context: &CommandContext) -> Result<CommandContext, Error> {
    super::site::read::check_site(context)?;
    if context.site.is_none() && context.token_source.uses_profile() {
        context.resolve_profile_default().await
    } else {
        Ok(context.clone())
    }
}

enum Action {
    Create(CreatePortMembership),
    Update(String),
    Delete(String),
}
struct Prepared {
    action: Action,
    operation: &'static str,
    patch: Patch,
    options: Options,
}

fn prepare(command: Command) -> Result<Prepared, Error> {
    let prepared = match command {
        Command::Create {
            new_name,
            no_port_membership,
            patch,
            options,
        } => {
            let mut patch = patch.into_patch();
            if patch.name.is_some() {
                return Err(usage("create uses its positional name; omit --name"));
            }
            patch.name = Some(new_name);
            if patch.vlan_id.is_none() {
                return Err(usage("a new wired network requires --vlan"));
            }
            Prepared {
                action: Action::Create(if no_port_membership {
                    CreatePortMembership::None
                } else {
                    CreatePortMembership::Template
                }),
                operation: "network.create",
                patch,
                options,
            }
        }
        Command::Update { id, patch, options } => Prepared {
            action: Action::Update(id),
            operation: "network.update",
            patch: patch.into_patch(),
            options,
        },
        Command::Dhcp {
            id,
            enabled,
            scope,
            options,
        } => Prepared {
            action: Action::Update(id),
            operation: "network.dhcp",
            patch: Patch {
                dhcp: scope.into_patch(enabled),
                ..Patch::default()
            },
            options,
        },
        Command::Delete { id, options } => Prepared {
            action: Action::Delete(id),
            operation: "network.delete",
            patch: Patch::default(),
            options,
        },
        _ => return Err(usage("expected a wired network mutation")),
    };
    prepared.patch.validate()?;
    if matches!(&prepared.action, Action::Update(_)) && prepared.patch.is_empty() {
        return Err(usage("specify at least one wired network change"));
    }
    Ok(prepared)
}

async fn run_mutation(command: Command, context: &CommandContext) -> anyhow::Result<CommandResult> {
    let prepared = prepare(command)?;
    prepared.options.preflight(context.token_source)?;
    let context = with_profile_site(context).await?;
    let api = super::site::read::client(&context)?;
    let site = super::site::read::site_id(&api, &context).await?;
    let (backend, plan) = match prepared.action {
        Action::Create(membership) => {
            NetworkMutation::create(&api, &site, prepared.patch, membership).await?
        }
        Action::Update(id) => NetworkMutation::update(&api, &site, &id, prepared.patch).await?,
        Action::Delete(id) => {
            NetworkMutation::delete(&api, &site, &id, prepared.options.yes).await?
        }
    };
    mutation::execute(
        &backend,
        plan,
        prepared.operation,
        backend.target(),
        prepared.options,
        &context,
    )
    .await
}

async fn run_services(
    command: ServicesCommand,
    context: &CommandContext,
) -> anyhow::Result<CommandResult> {
    let enabled = matches!(&command, ServicesCommand::Enable { .. });
    let shared = matches!(&command, ServicesCommand::Share { .. });
    let options = match &command {
        ServicesCommand::Enable { options }
        | ServicesCommand::Disable { options }
        | ServicesCommand::Share { options, .. }
        | ServicesCommand::Unshare { options, .. } => Some(*options),
        _ => None,
    };
    if let Some(options) = options {
        options.preflight(context.token_source)?;
    }
    let context = with_profile_site(context).await?;
    let api = super::site::read::client(&context)?;
    let site = super::site::read::site_id(&api, &context).await?;
    match command {
        ServicesCommand::Status => Ok(CommandResult::success(
            network::shared_services_status(&api, &site).await?,
        )),
        ServicesCommand::List { id } => {
            let networks = network::list(&api, &site).await?;
            Ok(CommandResult::success(
                network::select(&networks, &id)?.shared_services()?,
            ))
        }
        ServicesCommand::Enable { options } | ServicesCommand::Disable { options } => {
            let (backend, plan) = SharedServicesMutation::update(&api, &site, enabled).await?;
            mutation::execute(
                &backend,
                plan,
                if enabled {
                    "network.shared-services.enable"
                } else {
                    "network.shared-services.disable"
                },
                backend.target(),
                options,
                &context,
            )
            .await
        }
        ServicesCommand::Share {
            id,
            service,
            options,
        }
        | ServicesCommand::Unshare {
            id,
            service,
            options,
        } => {
            let (backend, plan) =
                SharedServiceMutation::update(&api, &site, &id, &service, shared).await?;
            mutation::execute(
                &backend,
                plan,
                if shared {
                    "network.shared-services.share"
                } else {
                    "network.shared-services.unshare"
                },
                backend.target(),
                options,
                &context,
            )
            .await
        }
    }
}

fn render(args: Args, networks: &[Network]) -> anyhow::Result<CommandResult> {
    let value = match args.command {
        Command::List => Value::Array(networks.iter().map(Network::summary).collect()),
        Command::Show { id } => network::select(networks, &id)?.details(),
        _ => return Err(usage("expected network list or show").into()),
    };
    Ok(CommandResult::success(value))
}
fn usage(message: &str) -> Error {
    Error::new(ErrorKind::Usage, message)
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use crate::cli::{Cli, Command as RootCommand};

    use super::{Action, CreatePortMembership, prepare};

    #[test]
    fn no_port_membership_is_create_only_and_template_remains_the_default() {
        for (flag, expected) in [
            (Some("--no-port-membership"), CreatePortMembership::None),
            (None, CreatePortMembership::Template),
        ] {
            let mut argv = vec![
                "instantctl",
                "network",
                "create",
                "test-network",
                "--vlan",
                "3999",
            ];
            argv.extend(flag);
            let cli = Cli::try_parse_from(argv).expect("valid create arguments");
            let RootCommand::Network(args) = cli.command else {
                panic!("expected network command");
            };
            let prepared = prepare(args.command).expect("create should prepare");
            assert!(matches!(prepared.action, Action::Create(actual) if actual == expected));
        }
        assert!(
            Cli::try_parse_from([
                "instantctl",
                "network",
                "update",
                "network-id",
                "--enabled",
                "false",
                "--no-port-membership",
            ])
            .is_err()
        );
    }
}
