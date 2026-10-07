//! Read and change MAC allow lists on wireless networks or wired ports.

use crate::{
    context::{CommandContext, CommandResult},
    credentials::CredentialSource,
    mutation::{self, Options},
};
use clap::Args as ClapArgs;
use instantctl_api::{
    Error, ErrorKind,
    client::allowlist::{self, PortScope},
    mutation::Prepared,
};

/// Arguments for a wireless network's client allow list.
#[derive(Debug, ClapArgs)]
pub struct WirelessArgs {
    /// Wireless network ID or exact name.
    pub network: String,
    /// Add one or more MAC addresses.
    #[arg(long, value_name = "MAC", num_args = 1.., conflicts_with = "remove")]
    pub add: Vec<String>,
    /// Remove one or more MAC addresses.
    #[arg(long, value_name = "MAC", num_args = 1.., conflicts_with = "add")]
    pub remove: Vec<String>,
    #[command(flatten)]
    pub options: Options,
}

#[derive(Debug, ClapArgs)]
pub struct WiredArgs {
    /// Device ID, MAC address, or exact name.
    pub device: String,
    /// Faceplate port number.
    #[arg(long, conflicts_with = "trunk", required_unless_present = "trunk")]
    pub port: Option<u64>,
    /// Trunk number.
    #[arg(long, conflicts_with = "port", required_unless_present = "port")]
    pub trunk: Option<u64>,
    /// Add one or more MAC addresses.
    #[arg(long, value_name = "MAC", num_args = 1.., conflicts_with = "remove")]
    pub add: Vec<String>,
    /// Remove one or more MAC addresses.
    #[arg(long, value_name = "MAC", num_args = 1.., conflicts_with = "add")]
    pub remove: Vec<String>,
    #[command(flatten)]
    pub options: Options,
}

/// Run local validation and confirmation checks before resolving credentials.
pub(crate) fn preflight_wireless(
    args: &WirelessArgs,
    source: CredentialSource,
) -> Result<(), Error> {
    let _ = change(&args.add, &args.remove, args.options)?;
    args.options.preflight(source)
}

pub(crate) fn preflight_wired(args: &WiredArgs, source: CredentialSource) -> Result<(), Error> {
    let _ = wired_scope(args)?;
    let _ = change(&args.add, &args.remove, args.options)?;
    args.options.preflight(source)
}

pub async fn run_wireless(
    args: WirelessArgs,
    context: &CommandContext,
) -> anyhow::Result<CommandResult> {
    preflight_wireless(&args, context.token_source)?;
    let change = change(&args.add, &args.remove, args.options)?;
    let context = with_profile_site(context).await?;
    let api = super::site::read::client(&context)?;
    let site = super::site::read::site_id(&api, &context).await?;
    let Some((add, addresses)) = change else {
        return Ok(CommandResult::success(
            allowlist::read_wireless(&api, &site, &args.network).await?,
        ));
    };
    let Prepared {
        backend,
        plan,
        target,
    } = allowlist::plan_wireless(&api, &site, &args.network, add, &addresses).await?;
    mutation::execute(
        &backend,
        plan,
        if add {
            "allowlist.wireless.add"
        } else {
            "allowlist.wireless.remove"
        },
        target,
        args.options,
        &context,
    )
    .await
}

pub async fn run_wired(args: WiredArgs, context: &CommandContext) -> anyhow::Result<CommandResult> {
    preflight_wired(&args, context.token_source)?;
    let scope = wired_scope(&args)?;
    let change = change(&args.add, &args.remove, args.options)?;
    let context = with_profile_site(context).await?;
    let api = super::site::read::client(&context)?;
    let site = super::site::read::site_id(&api, &context).await?;
    let Some((add, addresses)) = change else {
        return Ok(CommandResult::success(
            allowlist::read_wired(&api, &site, &args.device, scope).await?,
        ));
    };
    let Prepared {
        backend,
        plan,
        target,
    } = allowlist::plan_wired(&api, &site, &args.device, scope, add, &addresses).await?;
    mutation::execute(
        &backend,
        plan,
        if add {
            "allowlist.wired.add"
        } else {
            "allowlist.wired.remove"
        },
        target,
        args.options,
        &context,
    )
    .await
}

fn change(
    add: &[String],
    remove: &[String],
    options: Options,
) -> Result<Option<(bool, Vec<String>)>, Error> {
    if !add.is_empty() && !remove.is_empty() {
        return Err(usage("choose either --add or --remove"));
    }
    if options.apply && add.is_empty() && remove.is_empty() {
        return Err(Error::new(
            ErrorKind::Config,
            "--apply requires --add or --remove",
        ));
    }
    if !add.is_empty() {
        Ok(Some((true, allowlist::validate_mac_addresses(add)?)))
    } else if !remove.is_empty() {
        Ok(Some((false, allowlist::validate_mac_addresses(remove)?)))
    } else {
        Ok(None)
    }
}

fn wired_scope(args: &WiredArgs) -> Result<PortScope, Error> {
    match (args.port, args.trunk) {
        (Some(port), None) if port > 0 => Ok(PortScope::Port(port)),
        (None, Some(trunk)) if trunk > 0 => Ok(PortScope::Trunk(trunk)),
        (Some(0), _) | (_, Some(0)) => {
            Err(usage("port and trunk numbers must be greater than zero"))
        }
        _ => Err(usage("select exactly one of --port or --trunk")),
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

fn usage(message: &'static str) -> Error {
    Error::new(ErrorKind::Usage, message)
}
