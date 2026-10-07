use crate::context::{CommandContext, CommandResult};
use crate::mutation::Options;
use clap::{Args as ClapArgs, Subcommand, ValueEnum};
use instantctl_api::client::reads::Site;
use instantctl_api::site::{Change, DnsMode, Resource};
use instantctl_api::{Error, ErrorKind};
use serde_json::Value;
use std::net::Ipv4Addr;
pub(crate) mod read;

#[derive(Debug, ClapArgs)]
pub struct Args {
    #[command(subcommand)]
    pub command: Command,
}

impl Args {
    pub fn preflight(&self, context: &CommandContext) -> Result<(), Error> {
        match &self.command {
            Command::Create(args) => return args.preflight(context),
            Command::Rename(args) => return args.preflight(context),
            Command::Delete(args) => return args.preflight(context),
            Command::Clone(args) => return args.preflight(context),
            _ => {}
        }
        let (change, options) = match &self.command {
            Command::Timezone(args) => (args.change(), args.options),
            Command::ManagementNetwork(args) => (args.change(), args.options),
            Command::Dns(args) => (args.change(), args.options),
            Command::SpanningTree(args) => (args.change(), args.options),
            Command::ExtendNetwork(args) => (args.change(), args.options),
            Command::StpAutoPriority(args) => return args.options.preflight(context.token_source),
            _ => return Ok(()),
        };
        if let Some(change) = change {
            change.validate()?;
            options.preflight(context.token_source)?;
        } else if options.apply {
            return Err(read::config(
                "a setting value is required when applying a change",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, ClapArgs)]
pub struct TimezoneArgs {
    /// IANA timezone to plan (omit to read the current timezone).
    pub zone: Option<String>,
    #[command(flatten)]
    pub options: Options,
}

impl TimezoneArgs {
    fn change(&self) -> Option<Change> {
        self.zone.clone().map(Change::Timezone)
    }
}

#[derive(Debug, ClapArgs)]
pub struct ManagementNetworkArgs {
    /// Management VLAN (1 through 4092; 3333 through 3349 are reserved).
    #[arg(long)]
    pub vlan: Option<u16>,
    #[command(flatten)]
    pub options: Options,
}

impl ManagementNetworkArgs {
    fn change(&self) -> Option<Change> {
        self.vlan.map(Change::ManagementVlan)
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum DnsModeArg {
    Automatic,
    Infrastructure,
    Custom,
}

impl From<DnsModeArg> for DnsMode {
    fn from(mode: DnsModeArg) -> Self {
        match mode {
            DnsModeArg::Automatic => Self::Automatic,
            DnsModeArg::Infrastructure => Self::Infrastructure,
            DnsModeArg::Custom => Self::Custom,
        }
    }
}

#[derive(Debug, ClapArgs)]
pub struct DnsArgs {
    /// DNS assignment mode (omit to read the current configuration).
    #[arg(long, value_enum)]
    pub mode: Option<DnsModeArg>,
    /// Primary IPv4 DNS server; required with custom mode.
    #[arg(long, requires = "mode")]
    pub primary: Option<Ipv4Addr>,
    /// Secondary IPv4 DNS server; omitting it in custom mode clears the old value.
    #[arg(long, requires = "mode")]
    pub secondary: Option<Ipv4Addr>,
    #[command(flatten)]
    pub options: Options,
}

impl DnsArgs {
    fn change(&self) -> Option<Change> {
        self.mode.map(|mode| Change::Dns {
            mode: mode.into(),
            primary: self.primary,
            secondary: self.secondary,
        })
    }
}

#[derive(Debug, ClapArgs)]
pub struct SpanningTreeArgs {
    /// Enable or disable rapid spanning tree.
    #[arg(long)]
    pub rstp: Option<bool>,
    /// Base bridge priority (0 through 61440, in steps of 4096).
    #[arg(long)]
    pub priority: Option<u16>,
    #[command(flatten)]
    pub options: Options,
}

impl SpanningTreeArgs {
    fn change(&self) -> Option<Change> {
        (self.rstp.is_some() || self.priority.is_some()).then_some(Change::SpanningTree {
            use_rstp: self.rstp,
            priority: self.priority,
        })
    }
}

#[derive(Debug, ClapArgs)]
pub struct ExtendNetworkArgs {
    /// Enable or disable the site's mesh extension feature.
    #[arg(long)]
    pub enabled: Option<bool>,
    /// Enable or disable outdoor mesh mode.
    #[arg(long)]
    pub outdoor_mesh: Option<bool>,
    #[command(flatten)]
    pub options: Options,
}

impl ExtendNetworkArgs {
    fn change(&self) -> Option<Change> {
        (self.enabled.is_some() || self.outdoor_mesh.is_some()).then_some(Change::ExtendNetwork {
            enabled: self.enabled,
            outdoor_mesh: self.outdoor_mesh,
        })
    }
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Create an account-level site; plan by default.
    Create(super::site_lifecycle::CreateArgs),
    /// Rename a site by UUID or exact unique name.
    Rename(super::site_lifecycle::RenameArgs),
    /// Delete a site with its exact current name confirmed.
    Delete(super::site_lifecycle::DeleteArgs),
    /// Clone a source site into a new account-level site.
    Clone(super::site_lifecycle::CloneArgs),
    /// Read public country and regulatory code metadata without credentials.
    Country,
    /// List available sites.
    #[command(alias = "ls")]
    List,
    /// Show a site by UUID or exact unique name (default: the selected site).
    Show { id: Option<String> },
    /// Show the selected site's reported capabilities.
    Capabilities,
    /// Show the selected site's reported health.
    Health,
    /// Show the selected site's dashboard.
    Dashboard,
    /// Show the selected site's topology graph.
    Topology,
    /// Read or change the selected site's timezone.
    Timezone(TimezoneArgs),
    /// Read the management network or change its VLAN.
    ManagementNetwork(ManagementNetworkArgs),
    /// Read or change the management subnet's DNS configuration.
    Dns(DnsArgs),
    /// Read or change the selected site's spanning-tree settings.
    SpanningTree(SpanningTreeArgs),
    /// Read or change the selected site's mesh extension settings.
    ExtendNetwork(ExtendNetworkArgs),
    /// Request automatic switch bridge priorities; completion remains unverified.
    StpAutoPriority(super::site_stp_auto::Args),
}

pub async fn run(args: Args, context: &CommandContext) -> anyhow::Result<CommandResult> {
    args.preflight(context)?;
    match args.command {
        Command::StpAutoPriority(args) => return super::site_stp_auto::run(args, context).await,
        Command::Create(args) => return super::site_lifecycle::create(args, context).await,
        Command::Rename(args) => return super::site_lifecycle::rename(args, context).await,
        Command::Delete(args) => return super::site_lifecycle::delete(args, context).await,
        Command::Clone(args) => return super::site_lifecycle::clone(args, context).await,
        Command::Country => {
            return Ok(CommandResult::success(serde_json::to_value(
                instantctl_api::client::country::read(context.timeout).await?,
            )?));
        }
        _ => {}
    }
    let api = read::client(context)?;
    let resource = match &args.command {
        Command::Health => Some((Resource::Health, None, Options::default(), "site.health")),
        Command::Dashboard => Some((
            Resource::Dashboard,
            None,
            Options::default(),
            "site.dashboard",
        )),
        Command::Topology => Some((
            Resource::Topology,
            None,
            Options::default(),
            "site.topology",
        )),
        Command::Timezone(args) => Some((
            Resource::Timezone,
            args.change(),
            args.options,
            "site.timezone",
        )),
        Command::ManagementNetwork(args) => Some((
            Resource::ManagementNetwork,
            args.change(),
            args.options,
            "site.management_network",
        )),
        Command::Dns(args) => Some((
            Resource::ManagementNetwork,
            args.change(),
            args.options,
            "site.dns",
        )),
        Command::SpanningTree(args) => Some((
            Resource::SpanningTree,
            args.change(),
            args.options,
            "site.spanning_tree",
        )),
        Command::ExtendNetwork(args) => Some((
            Resource::ExtendNetwork,
            args.change(),
            args.options,
            "site.extend_network",
        )),
        _ => None,
    };
    if let Some((resource, change, options, operation)) = resource {
        let site = read::site_id(&api, context).await?;
        if let Some(change) = change {
            let instantctl_api::mutation::Prepared {
                backend,
                plan,
                target,
            } = instantctl_api::site::plan_update(&api, &site, change).await?;
            return crate::mutation::execute(&backend, plan, operation, target, options, context)
                .await;
        }
        if matches!(args.command, Command::Dns(_)) {
            return Ok(CommandResult::success(serde_json::to_value(
                api.site_dns(&site).await?,
            )?));
        }
        return Ok(CommandResult::success(
            api.site_resource(&site, resource).await?,
        ));
    }
    if matches!(args.command, Command::Capabilities) {
        let site = read::site_id(&api, context).await?;
        return Ok(CommandResult::success(serde_json::to_value(
            api.capabilities(&site).await?,
        )?));
    }
    render(args, context, &read::sites(&api).await?)
}

fn render(args: Args, context: &CommandContext, models: &[Site]) -> anyhow::Result<CommandResult> {
    read::check_site(context)?;
    let sites = read::values(models)?;
    let data = match args.command {
        Command::List => Value::Array(sites.iter().map(summary).collect()),
        Command::Show { id } => {
            let selector = match id.or_else(|| context.site.clone()) {
                Some(id) => id,
                None if sites.len() == 1 => {
                    sites[0]["id"].as_str().expect("validated site").to_owned()
                }
                None if sites.is_empty() => {
                    return Err(Error::new(ErrorKind::NotFound, "no sites are available").into());
                }
                None => {
                    return Err(read::config(
                        "multiple sites are available; select one with --site <UUID>",
                    )
                    .into());
                }
            };
            let matches: Vec<_> = sites
                .iter()
                .filter(|site| {
                    if read::valid_site_id(&selector) {
                        site["id"]
                            .as_str()
                            .is_some_and(|id| id.eq_ignore_ascii_case(&selector))
                    } else {
                        site["name"].as_str() == Some(&selector)
                    }
                })
                .collect();
            match matches.as_slice() {
                [site] => summary(site),
                [] => {
                    return Err(Error::new(
                        ErrorKind::NotFound,
                        "no site matches the supplied selector",
                    )
                    .into());
                }
                _ => {
                    return Err(Error::new(
                        ErrorKind::Usage,
                        "site name is ambiguous; select by UUID",
                    )
                    .into());
                }
            }
        }
        Command::Create(_)
        | Command::Rename(_)
        | Command::Delete(_)
        | Command::Clone(_)
        | Command::Country
        | Command::Capabilities
        | Command::Health
        | Command::Dashboard
        | Command::Topology
        | Command::Timezone(_)
        | Command::ManagementNetwork(_)
        | Command::Dns(_)
        | Command::SpanningTree(_)
        | Command::ExtendNetwork(_)
        | Command::StpAutoPriority(_) => {
            return Err(Error::new(ErrorKind::Usage, "expected site list or show").into());
        }
    };
    Ok(CommandResult::success(data))
}

fn summary(site: &Value) -> Value {
    read::project(
        site,
        &[
            ("id", "id"),
            ("name", "name"),
            ("status", "status"),
            ("health", "health"),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use read::tests::{SITE, context, error_kind, model};
    use serde_json::json;
    #[test]
    fn list_and_show_keep_missing_status_null() {
        let sites = vec![model(json!({"id":SITE,"name":"Home"}))];
        for command in [
            Command::List,
            Command::Show {
                id: Some("Home".into()),
            },
            Command::Show { id: None },
        ] {
            let result = render(Args { command }, &context(None), &sites).unwrap();
            let row = if result.data.is_array() {
                &result.data[0]
            } else {
                &result.data
            };
            assert_eq!(row["name"], "Home");
            assert!(row["health"].is_null());
            assert!(row["status"].is_null());
        }
    }
    #[test]
    fn show_distinguishes_ambiguity_and_not_found_with_exact_names() {
        let sites = vec![
            model(json!({"id":SITE,"name":"Home"})),
            model(json!({"id":"11111111-2222-3333-4444-555555555555","name":"Home"})),
        ];
        for (id, kind) in [("Home", ErrorKind::Usage), ("Hom", ErrorKind::NotFound)] {
            assert_eq!(
                error_kind(
                    &render(
                        Args {
                            command: Command::Show {
                                id: Some(id.into())
                            }
                        },
                        &context(None),
                        &sites
                    )
                    .err()
                    .unwrap()
                ),
                kind
            );
        }
    }
    #[test]
    fn show_accepts_site_uuid_and_multiple_sites_require_selection() {
        let sites = vec![
            model(json!({"id":SITE,"name":"Home"})),
            model(json!({"id":"11111111-2222-3333-4444-555555555555","name":"Office"})),
        ];
        let result = render(
            Args {
                command: Command::Show {
                    id: Some(SITE.into()),
                },
            },
            &context(None),
            &sites,
        )
        .unwrap();
        assert_eq!(result.data["name"], "Home");
        assert_eq!(
            error_kind(
                &render(
                    Args {
                        command: Command::Show { id: None }
                    },
                    &context(None),
                    &sites
                )
                .err()
                .unwrap()
            ),
            ErrorKind::Config
        );
    }
}
