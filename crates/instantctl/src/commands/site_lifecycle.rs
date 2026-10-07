use clap::Args as ClapArgs;
use instantctl_api::{
    Error, ErrorKind,
    client::site_lifecycle::{NewSite, SiteMutation},
};

use crate::{
    context::{CommandContext, CommandResult},
    mutation::{self, Options},
};

#[derive(Debug, ClapArgs)]
pub struct CreateArgs {
    /// Name of the new site.
    pub name: String,
    /// Supported two-letter regulatory country code, such as IL.
    #[arg(long)]
    pub country: String,
    /// IANA timezone, such as Europe/Berlin.
    #[arg(long)]
    pub timezone: String,
    #[command(flatten)]
    pub options: Options,
}

impl CreateArgs {
    fn new_site(&self) -> NewSite {
        NewSite {
            name: self.name.clone(),
            country: self.country.clone(),
            timezone: self.timezone.clone(),
        }
    }
    pub fn preflight(&self, context: &CommandContext) -> Result<(), Error> {
        self.new_site().validate()?;
        self.options.preflight(context.token_source)
    }
}

#[derive(Debug, ClapArgs)]
pub struct RenameArgs {
    /// Site UUID or exact unique name.
    pub selector: String,
    pub name: String,
    #[command(flatten)]
    pub options: Options,
}

impl RenameArgs {
    pub fn preflight(&self, context: &CommandContext) -> Result<(), Error> {
        selector(&self.selector)?;
        instantctl_api::client::site_lifecycle::validate_name(&self.name)?;
        self.options.preflight(context.token_source)
    }
}

#[derive(Debug, ClapArgs)]
pub struct CloneArgs {
    /// Source site UUID or exact unique name.
    pub source: String,
    #[command(flatten)]
    pub new_site: CreateArgs,
}

impl CloneArgs {
    pub fn preflight(&self, context: &CommandContext) -> Result<(), Error> {
        selector(&self.source)?;
        self.new_site.preflight(context)
    }
}

#[derive(Debug, ClapArgs)]
pub struct DeleteArgs {
    /// Site UUID or exact unique name.
    pub selector: String,
    /// Type the current site name exactly; required with --apply and --yes.
    #[arg(long)]
    pub confirm_name: Option<String>,
    #[command(flatten)]
    pub options: Options,
}

impl DeleteArgs {
    pub fn preflight(&self, context: &CommandContext) -> Result<(), Error> {
        selector(&self.selector)?;
        if self.options.apply
            && (!self.options.yes || self.confirm_name.as_deref().is_none_or(str::is_empty))
        {
            return Err(Error::new(
                ErrorKind::ConfirmationRequired,
                "site deletion requires --yes and --confirm-name <current site name>",
            ));
        }
        self.options.preflight(context.token_source)
    }
}

fn selector(value: &str) -> Result<(), Error> {
    if value.is_empty() || value.chars().any(char::is_control) {
        return Err(Error::new(
            ErrorKind::Config,
            "site selector must be a UUID or exact name",
        ));
    }
    Ok(())
}

pub async fn create(args: CreateArgs, context: &CommandContext) -> anyhow::Result<CommandResult> {
    args.preflight(context)?;
    let api = context.client()?;
    let (backend, plan) = SiteMutation::create(&api, args.new_site()).await?;
    mutation::execute(
        &backend,
        plan,
        "site.create",
        backend.target(),
        args.options,
        context,
    )
    .await
}

pub async fn rename(args: RenameArgs, context: &CommandContext) -> anyhow::Result<CommandResult> {
    args.preflight(context)?;
    let api = context.client()?;
    let (backend, plan) = SiteMutation::rename(&api, &args.selector, &args.name).await?;
    mutation::execute(
        &backend,
        plan,
        "site.rename",
        backend.target(),
        args.options,
        context,
    )
    .await
}

pub async fn clone(args: CloneArgs, context: &CommandContext) -> anyhow::Result<CommandResult> {
    args.preflight(context)?;
    let api = context.client()?;
    let (backend, plan) = SiteMutation::clone(&api, &args.source, args.new_site.new_site()).await?;
    mutation::execute(
        &backend,
        plan,
        "site.clone",
        backend.target(),
        args.new_site.options,
        context,
    )
    .await
}

pub async fn delete(args: DeleteArgs, context: &CommandContext) -> anyhow::Result<CommandResult> {
    args.preflight(context)?;
    let api = context.client()?;
    let (backend, plan) =
        SiteMutation::delete(&api, &args.selector, args.confirm_name.as_deref()).await?;
    mutation::execute(
        &backend,
        plan,
        "site.delete",
        backend.target(),
        args.options,
        context,
    )
    .await
}
