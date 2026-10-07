use super::access_common::{self, usage};
use crate::{
    context::{CommandContext, CommandResult},
    mutation::{self, Options},
};
use clap::{Args as ClapArgs, Subcommand, ValueEnum};
use instantctl_api::client::access::guest_portal::{self, Patch, PortalType};

#[derive(Debug, ClapArgs)]
pub struct Args {
    #[command(subcommand)]
    pub command: Command,
}
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Show the guest portal settings with credentials redacted.
    Show,
    /// Update the selected portal page, preserving the complete fetched singleton.
    Update {
        #[command(flatten)]
        patch: Box<PatchArgs>,
        #[command(flatten)]
        options: Options,
    },
}
#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum TypeKind {
    InternalAck,
    External,
}
#[derive(Clone, Debug, Default, ClapArgs)]
pub struct PatchArgs {
    #[arg(long, value_enum)]
    portal_type: Option<TypeKind>,
    #[arg(long)]
    welcome: Option<String>,
    #[arg(long)]
    terms_title: Option<String>,
    #[arg(long)]
    terms: Option<String>,
    #[arg(long)]
    agreement: Option<String>,
    #[arg(long)]
    accept_label: Option<String>,
    /// Empty text clears the redirect URL.
    #[arg(long)]
    redirect_url: Option<String>,
    #[arg(long)]
    background_color: Option<String>,
    #[arg(long)]
    welcome_color: Option<String>,
    #[arg(long)]
    terms_color: Option<String>,
    #[arg(long)]
    agreement_color: Option<String>,
    #[arg(long)]
    button_color: Option<String>,
    #[arg(long)]
    button_text_color: Option<String>,
    #[arg(long)]
    welcome_size: Option<u8>,
    #[arg(long)]
    terms_title_size: Option<u8>,
    #[arg(long)]
    button_radius: Option<u8>,
    #[arg(long)]
    font_family: Option<String>,
    #[arg(long)]
    external_url: Option<String>,
    #[arg(long)]
    authentication: Option<bool>,
    /// Select a RADIUS profile by ID or exact unique name.
    #[arg(long)]
    radius_profile: Option<String>,
    #[arg(long)]
    accounting: Option<bool>,
    #[arg(long)]
    require_authenticator: Option<bool>,
    /// Replace the allowlist with comma-separated hostnames; empty text clears it.
    #[arg(long)]
    whitelisted_domains: Option<String>,
}
impl PatchArgs {
    fn into_patch(self) -> Patch {
        Patch {
            portal_type: self.portal_type.map(|kind| match kind {
                TypeKind::InternalAck => PortalType::InternalAck,
                TypeKind::External => PortalType::External,
            }),
            welcome: self.welcome,
            terms_title: self.terms_title,
            terms: self.terms,
            agreement: self.agreement,
            accept_label: self.accept_label,
            redirect_url: self.redirect_url,
            background_color: self.background_color,
            welcome_color: self.welcome_color,
            terms_color: self.terms_color,
            agreement_color: self.agreement_color,
            button_color: self.button_color,
            button_text_color: self.button_text_color,
            welcome_size: self.welcome_size,
            terms_title_size: self.terms_title_size,
            button_radius: self.button_radius,
            font_family: self.font_family,
            external_url: self.external_url,
            authentication: self.authentication,
            radius_profile: self.radius_profile,
            accounting: self.accounting,
            require_authenticator: self.require_authenticator,
            whitelisted_domains: self.whitelisted_domains.map(|domains| {
                if domains.is_empty() {
                    Vec::new()
                } else {
                    domains.split(',').map(str::to_owned).collect()
                }
            }),
        }
    }
}
pub async fn run(args: Args, context: &CommandContext) -> anyhow::Result<CommandResult> {
    let prepared = match args.command {
        Command::Show => None,
        Command::Update { patch, options } => {
            let patch = (*patch).into_patch();
            patch.validate()?;
            if patch.is_empty() {
                return Err(usage("specify at least one guest portal change").into());
            }
            options.preflight(context.token_source)?;
            Some((patch, options))
        }
    };
    let context = access_common::context(context).await?;
    let api = super::site::read::client(&context)?;
    let site = super::site::read::site_id(&api, &context).await?;
    if let Some((patch, options)) = prepared {
        let (backend, plan) = guest_portal::update(&api, &site, patch).await?;
        mutation::execute(
            &backend,
            plan,
            "guest-portal.update",
            backend.target(),
            options,
            &context,
        )
        .await
    } else {
        Ok(CommandResult::success(
            guest_portal::show(&api, &site).await?,
        ))
    }
}
