mod passphrase_input;

use clap::{Args as ClapArgs, Subcommand, ValueEnum};
use instantctl_api::{
    Error, ErrorKind,
    client::wlan::{
        AccessPoints, AdvancedPatch, Bands, Bandwidth, Binding, Day, Network, Patch, Schedule,
        Security, TrafficPriority, WlanMutation,
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
    /// List wireless networks at the selected site.
    #[command(alias = "ls")]
    List,
    /// Show a wireless network by exact name or identifier.
    Show { id: String },
    /// Read or edit the wireless network's client allowlist.
    Allowlist(super::allowlist::WirelessArgs),
    /// Create a wireless network.
    Create {
        new_name: String,
        #[command(flatten)]
        patch: PatchArgs,
        #[command(flatten)]
        options: Options,
    },
    /// Update fields on a wireless network.
    Update {
        id: String,
        #[command(flatten)]
        patch: PatchArgs,
        #[command(flatten)]
        options: Options,
    },
    /// Delete a wireless network. Use --yes to acknowledge deletion risk.
    Delete {
        id: String,
        #[command(flatten)]
        options: Options,
    },
    /// Enable a wireless network.
    Enable {
        id: String,
        #[command(flatten)]
        options: Options,
    },
    /// Disable a wireless network.
    Disable {
        id: String,
        #[command(flatten)]
        options: Options,
    },
    /// Change the network passphrase.
    Passphrase {
        id: String,
        /// Read the passphrase from one bounded line on stdin, instead of a hidden prompt.
        #[arg(long)]
        passphrase_stdin: bool,
        #[command(flatten)]
        options: Options,
    },
    /// Set enabled wireless bands.
    Bands {
        id: String,
        /// Comma-separated list: 2.4,5,6.
        #[arg(long, value_parser = parse_bands)]
        bands: Bands,
        #[command(flatten)]
        options: Options,
    },
    /// Set the schedule to off, always, or timed.
    Schedule {
        id: String,
        #[arg(long, value_enum)]
        schedule: ScheduleKind,
        /// Comma-separated weekdays for a timed schedule.
        #[arg(long)]
        days: Option<String>,
        /// Local schedule start time, HH:MM.
        #[arg(long)]
        start: Option<String>,
        /// Local schedule end time, HH:MM.
        #[arg(long)]
        end: Option<String>,
        #[command(flatten)]
        options: Options,
    },
    /// Set a per-client or per-network bandwidth limit.
    Bandwidth {
        id: String,
        #[arg(long, value_enum)]
        mode: BandwidthKind,
        #[arg(long, requires = "mode")]
        download: Option<u64>,
        #[arg(long, requires = "mode")]
        upload: Option<u64>,
        #[command(flatten)]
        options: Options,
    },
    /// Mark the network as guest or employee. This does not change access rules.
    Guest {
        id: String,
        #[arg(long, conflicts_with = "deny")]
        allow: bool,
        #[arg(long, conflicts_with = "allow")]
        deny: bool,
        #[command(flatten)]
        options: Options,
    },
    /// Set client access restrictions.
    Access {
        id: String,
        #[arg(long)]
        restrict_access: Option<bool>,
        #[arg(long, value_enum)]
        internet: Option<AccessRule>,
        #[arg(long, value_enum)]
        intra_subnet_traffic: Option<AccessRule>,
        #[arg(long = "allowed-destination", value_name = "IPV4")]
        allowed_destinations: Vec<String>,
        #[arg(long, conflicts_with = "allowed_destinations")]
        clear_allowed_destinations: bool,
        #[command(flatten)]
        options: Options,
    },
}

#[derive(Clone, Debug, ClapArgs, Default)]
pub struct PatchArgs {
    #[arg(long = "name", id = "ssid_name")]
    ssid_name: Option<String>,
    #[arg(long)]
    enabled: Option<bool>,
    #[arg(long)]
    hidden: Option<bool>,
    #[arg(long, value_enum)]
    security: Option<SecurityKind>,
    /// Read the passphrase from one bounded line on stdin, instead of a hidden prompt.
    #[arg(long)]
    passphrase_stdin: bool,
    #[arg(long, value_parser = parse_bands)]
    bands: Option<Bands>,
    /// Bind to an existing wired network by its exact name or identifier.
    #[arg(long, conflicts_with = "vlan")]
    wired_network: Option<String>,
    /// Bind to the existing wired network with this VLAN; it must resolve uniquely.
    #[arg(long, conflicts_with = "wired_network")]
    vlan: Option<u16>,
    /// Select an existing RADIUS profile by name or identifier; this does not accept secrets.
    #[arg(long)]
    radius_profile: Option<String>,
    /// Replace the current AP binding set with this AP; repeat to select multiple APs.
    #[arg(long = "ap", value_name = "AP", action = clap::ArgAction::Append, conflicts_with = "all_aps")]
    aps: Vec<String>,
    /// Bind this WLAN to all available APs.
    #[arg(long, conflicts_with = "aps")]
    all_aps: bool,
    /// Enable or disable the captive portal; enabling it requires guest mode.
    #[arg(long)]
    captive_portal: Option<bool>,
    /// Enable or disable legacy 802.11b rates.
    #[arg(long)]
    legacy_rates: Option<bool>,
    /// Enable or disable Wi-Fi 6; the site must advertise Wi-Fi 6 support.
    #[arg(long)]
    wifi6: Option<bool>,
    /// Enable or disable OFDMA; enabling it requires Wi-Fi 6 and site support.
    #[arg(long)]
    ofdma: Option<bool>,
    /// Enable or disable Wi-Fi 7; the site must advertise Wi-Fi 7 support.
    #[arg(long)]
    wifi7: Option<bool>,
    /// Enable or disable MLO; enabling it requires 6 GHz, WPA3, Wi-Fi 6, Wi-Fi 7, and site support.
    #[arg(long)]
    mlo: Option<bool>,
    /// Enable or disable dynamic multicast optimization; the site must advertise support.
    #[arg(long)]
    multicast_optimization: Option<bool>,
    /// Enable or disable broadcast on all bound APs and bands.
    #[arg(long)]
    broadcast_all_bands: Option<bool>,
    #[arg(long, value_enum)]
    traffic_priority: Option<TrafficPriorityKind>,
    #[arg(long, value_enum)]
    schedule: Option<ScheduleKind>,
    #[arg(long)]
    days: Option<String>,
    #[arg(long)]
    start: Option<String>,
    #[arg(long)]
    end: Option<String>,
    #[arg(long, value_enum)]
    bandwidth: Option<BandwidthKind>,
    #[arg(long)]
    download: Option<u64>,
    #[arg(long)]
    upload: Option<u64>,
    #[arg(long)]
    guest: Option<bool>,
    #[arg(long)]
    restrict_access: Option<bool>,
    #[arg(long, value_enum)]
    internet: Option<AccessRule>,
    #[arg(long, value_enum)]
    intra_subnet_traffic: Option<AccessRule>,
    #[arg(long = "allowed-destination", value_name = "IPV4")]
    allowed_destinations: Vec<String>,
    #[arg(long, conflicts_with = "allowed_destinations")]
    clear_allowed_destinations: bool,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum SecurityKind {
    Open,
    EnhancedOpen,
    Wpa2Personal,
    Wpa3Personal,
    Wpa2Enterprise,
    Wpa3Enterprise,
}
#[derive(Clone, Copy, Debug, ValueEnum)]
enum TrafficPriorityKind {
    Off,
    Low,
    Medium,
    High,
    VeryHigh,
}
#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum ScheduleKind {
    Off,
    Always,
    Timed,
}
#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum BandwidthKind {
    Off,
    PerClient,
    PerNetwork,
}
#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum AccessRule {
    Allow,
    Deny,
}

pub async fn run(args: Args, context: &CommandContext) -> anyhow::Result<CommandResult> {
    match args.command {
        Command::Allowlist(args) => super::allowlist::run_wireless(args, context).await,
        Command::List | Command::Show { .. } => {
            let context = with_profile_site(context).await?;
            let api = super::site::read::client(&context)?;
            let site = super::site::read::site_id(&api, &context).await?;
            let networks = instantctl_api::client::wlan::list(&api, &site).await?;
            render(args, &networks)
        }
        command => run_mutation(command, context).await,
    }
}

async fn run_mutation(command: Command, context: &CommandContext) -> anyhow::Result<CommandResult> {
    let mut prepared = prepare(command)?;
    if let Some(patch) = prepared.patch.as_ref() {
        patch.validate()?;
    }
    if let Some(from_stdin) = prepared.passphrase_input {
        passphrase_input::validate_input(from_stdin, context.token_source)?;
    }
    prepared.options.preflight(context.token_source)?;
    if let Some(from_stdin) = prepared.passphrase_input {
        let secret = passphrase_input::collect(from_stdin)?;
        let patch = prepared
            .patch
            .as_mut()
            .ok_or_else(|| usage("passphrase input requires a wireless configuration change"))?;
        patch.passphrase = Some(secret);
        patch.validate()?;
    }
    let context = with_profile_site(context).await?;
    let api = super::site::read::client(&context)?;
    let site = super::site::read::site_id(&api, &context).await?;
    let (backend, plan) = match prepared.action {
        PreparedAction::Create => {
            WlanMutation::create(&api, &site, prepared.patch.expect("create patch")).await?
        }
        PreparedAction::Update { id } => {
            WlanMutation::update(&api, &site, &id, prepared.patch.expect("update patch")).await?
        }
        PreparedAction::Delete { id, yes } => WlanMutation::delete(&api, &site, &id, yes).await?,
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

async fn with_profile_site(context: &CommandContext) -> Result<CommandContext, Error> {
    super::site::read::check_site(context)?;
    // Keep profile lookup after local PSK/input validation. The dispatcher
    // resolves defaults before running other cloud nouns.
    if context.site.is_none() && context.token_source.uses_profile() {
        context.resolve_profile_default().await
    } else {
        Ok(context.clone())
    }
}

struct Prepared {
    action: PreparedAction,
    operation: &'static str,
    patch: Option<Patch>,
    passphrase_input: Option<bool>,
    options: Options,
}

enum PreparedAction {
    Create,
    Update { id: String },
    Delete { id: String, yes: bool },
}

fn prepare(command: Command) -> Result<Prepared, Error> {
    let prepared = match command {
        Command::Create {
            new_name,
            patch,
            options,
        } => {
            let input = requested_passphrase(&patch, true)?;
            let mut patch = patch.into_patch()?;
            if patch.name.is_some() {
                return Err(usage("create uses its positional name; omit --name"));
            }
            patch.name = Some(new_name);
            Prepared {
                action: PreparedAction::Create,
                operation: "wlan.create",
                patch: Some(patch),
                passphrase_input: input,
                options,
            }
        }
        Command::Update { id, patch, options } => {
            let input = requested_passphrase(&patch, false)?;
            Prepared {
                action: PreparedAction::Update { id },
                operation: "wlan.update",
                patch: Some(patch.into_patch()?),
                passphrase_input: input,
                options,
            }
        }
        Command::Delete { id, options } => Prepared {
            action: PreparedAction::Delete {
                id,
                yes: options.yes,
            },
            operation: "wlan.delete",
            patch: None,
            passphrase_input: None,
            options,
        },
        Command::Enable { id, options } => one_field(id, options, "wlan.enable", |patch| {
            patch.enabled = Some(true)
        }),
        Command::Disable { id, options } => one_field(id, options, "wlan.disable", |patch| {
            patch.enabled = Some(false)
        }),
        Command::Passphrase {
            id,
            passphrase_stdin,
            options,
        } => {
            let mut prepared = one_field(id, options, "wlan.passphrase", |_| {});
            prepared.passphrase_input = Some(passphrase_stdin);
            prepared
        }
        Command::Bands { id, bands, options } => {
            one_field(id, options, "wlan.bands", |patch| patch.bands = Some(bands))
        }
        Command::Schedule {
            id,
            schedule,
            days,
            start,
            end,
            options,
        } => {
            let change = PatchArgs {
                schedule: Some(schedule),
                days,
                start,
                end,
                ..PatchArgs::default()
            }
            .into_patch()?;
            one_field(id, options, "wlan.schedule", move |patch| {
                patch.schedule = change.schedule
            })
        }
        Command::Bandwidth {
            id,
            mode,
            download,
            upload,
            options,
        } => {
            let change = PatchArgs {
                bandwidth: Some(mode),
                download,
                upload,
                ..PatchArgs::default()
            }
            .into_patch()?;
            one_field(id, options, "wlan.bandwidth", move |patch| {
                patch.bandwidth = change.bandwidth
            })
        }
        Command::Guest {
            id,
            allow,
            deny,
            options,
        } => {
            let guest = match (allow, deny) {
                (true, false) => true,
                (false, true) => false,
                _ => return Err(usage("select exactly one of --allow or --deny")),
            };
            one_field(id, options, "wlan.guest", move |patch| {
                patch.guest = Some(guest)
            })
        }
        Command::Access {
            id,
            restrict_access,
            internet,
            intra_subnet_traffic,
            allowed_destinations,
            clear_allowed_destinations,
            options,
        } => {
            if restrict_access.is_none()
                && internet.is_none()
                && intra_subnet_traffic.is_none()
                && allowed_destinations.is_empty()
                && !clear_allowed_destinations
            {
                return Err(usage("access requires at least one access field"));
            }
            one_field(id, options, "wlan.access", move |patch| {
                patch.restrict_access = restrict_access;
                patch.internet = internet.map(Into::into);
                patch.intra_subnet_traffic = intra_subnet_traffic.map(Into::into);
                if clear_allowed_destinations || !allowed_destinations.is_empty() {
                    patch.allowed_destinations = Some(allowed_destinations);
                }
            })
        }
        Command::List | Command::Show { .. } | Command::Allowlist(_) => {
            return Err(usage("expected a wireless network mutation"));
        }
    };
    Ok(prepared)
}

fn one_field(
    id: String,
    options: Options,
    operation: &'static str,
    set: impl FnOnce(&mut Patch),
) -> Prepared {
    let mut patch = Patch::default();
    set(&mut patch);
    Prepared {
        action: PreparedAction::Update { id },
        operation,
        patch: Some(patch),
        passphrase_input: None,
        options,
    }
}

impl PatchArgs {
    fn into_patch(self) -> Result<Patch, Error> {
        let schedule = match self.schedule {
            None => {
                if self.days.is_some() || self.start.is_some() || self.end.is_some() {
                    return Err(usage("schedule days and times require --schedule timed"));
                }
                None
            }
            Some(ScheduleKind::Off) => {
                if self.days.is_some() || self.start.is_some() || self.end.is_some() {
                    return Err(usage("schedule off does not accept days or times"));
                }
                Some(Schedule::Off)
            }
            Some(ScheduleKind::Always) => {
                if self.days.is_some() || self.start.is_some() || self.end.is_some() {
                    return Err(usage("schedule always does not accept days or times"));
                }
                Some(Schedule::Always)
            }
            Some(ScheduleKind::Timed) => Some(Schedule::Timed {
                days: parse_days(
                    &self
                        .days
                        .ok_or_else(|| usage("timed schedule requires --days"))?,
                )
                .map_err(|_| usage("timed schedule has an invalid weekday list"))?,
                start: self
                    .start
                    .ok_or_else(|| usage("timed schedule requires --start"))?,
                end: self
                    .end
                    .ok_or_else(|| usage("timed schedule requires --end"))?,
            }),
        };
        let bandwidth = match self.bandwidth {
            None => {
                if self.download.is_some() || self.upload.is_some() {
                    return Err(usage("bandwidth limits require --bandwidth"));
                }
                None
            }
            Some(BandwidthKind::Off) => {
                if self.download.is_some() || self.upload.is_some() {
                    return Err(usage("bandwidth off does not accept limits"));
                }
                Some(Bandwidth::Off)
            }
            Some(BandwidthKind::PerClient) => Some(Bandwidth::PerClient {
                download: self
                    .download
                    .ok_or_else(|| usage("per-client bandwidth requires --download"))?,
                upload: self
                    .upload
                    .ok_or_else(|| usage("per-client bandwidth requires --upload"))?,
            }),
            Some(BandwidthKind::PerNetwork) => Some(Bandwidth::PerNetwork {
                download: self
                    .download
                    .ok_or_else(|| usage("per-network bandwidth requires --download"))?,
                upload: self
                    .upload
                    .ok_or_else(|| usage("per-network bandwidth requires --upload"))?,
            }),
        };
        let allowed_destinations =
            if self.clear_allowed_destinations || !self.allowed_destinations.is_empty() {
                Some(self.allowed_destinations)
            } else {
                None
            };
        let binding = match (self.wired_network, self.vlan) {
            (Some(_), Some(_)) => {
                return Err(usage("--wired-network conflicts with --vlan"));
            }
            (Some(network), None) => Some(Binding::Network(network)),
            (None, Some(vlan)) => Some(Binding::Vlan(vlan)),
            (None, None) => None,
        };
        let access_points = if self.all_aps {
            Some(AccessPoints::All)
        } else if self.aps.is_empty() {
            None
        } else {
            Some(AccessPoints::Selected(self.aps))
        };
        Ok(Patch {
            name: self.ssid_name,
            enabled: self.enabled,
            hidden: self.hidden,
            security: self.security.map(Into::into),
            passphrase: None,
            bands: self.bands,
            schedule,
            bandwidth,
            guest: self.guest,
            restrict_access: self.restrict_access,
            internet: self.internet.map(Into::into),
            intra_subnet_traffic: self.intra_subnet_traffic.map(Into::into),
            allowed_destinations,
            advanced: AdvancedPatch {
                binding,
                radius_profile: self.radius_profile,
                access_points,
                captive_portal: self.captive_portal,
                legacy_rates: self.legacy_rates,
                wifi6: self.wifi6,
                ofdma: self.ofdma,
                wifi7: self.wifi7,
                mlo: self.mlo,
                multicast_optimization: self.multicast_optimization,
                broadcast_all_bands: self.broadcast_all_bands,
                traffic_priority: self.traffic_priority.map(Into::into),
            },
        })
    }
}

impl From<SecurityKind> for Security {
    fn from(value: SecurityKind) -> Self {
        match value {
            SecurityKind::Open => Self::Open,
            SecurityKind::EnhancedOpen => Self::EnhancedOpen,
            SecurityKind::Wpa2Personal => Self::Wpa2Personal,
            SecurityKind::Wpa3Personal => Self::Wpa3Personal,
            SecurityKind::Wpa2Enterprise => Self::Wpa2Enterprise,
            SecurityKind::Wpa3Enterprise => Self::Wpa3Enterprise,
        }
    }
}

impl From<TrafficPriorityKind> for TrafficPriority {
    fn from(value: TrafficPriorityKind) -> Self {
        match value {
            TrafficPriorityKind::Off => Self::Off,
            TrafficPriorityKind::Low => Self::Low,
            TrafficPriorityKind::Medium => Self::Medium,
            TrafficPriorityKind::High => Self::High,
            TrafficPriorityKind::VeryHigh => Self::VeryHigh,
        }
    }
}

impl From<AccessRule> for bool {
    fn from(value: AccessRule) -> Self {
        matches!(value, AccessRule::Allow)
    }
}

fn requested_passphrase(patch: &PatchArgs, create: bool) -> Result<Option<bool>, Error> {
    if patch.passphrase_stdin
        && matches!(
            patch.security,
            Some(SecurityKind::Open | SecurityKind::EnhancedOpen)
        )
    {
        return Err(usage("open security does not use a passphrase"));
    }
    let personal = matches!(
        patch.security,
        Some(SecurityKind::Wpa2Personal | SecurityKind::Wpa3Personal)
    );
    Ok(
        (patch.passphrase_stdin || personal || (create && patch.security.is_none()))
            .then_some(patch.passphrase_stdin),
    )
}

fn parse_bands(value: &str) -> Result<Bands, String> {
    let mut bands = Bands {
        two_four: false,
        five: false,
        six: false,
    };
    for band in value.split(',') {
        match band.trim() {
            "2.4" => bands.two_four = true,
            "5" => bands.five = true,
            "6" => bands.six = true,
            _ => return Err("expected a comma-separated list from 2.4,5,6".into()),
        }
    }
    if !(bands.two_four || bands.five || bands.six) {
        return Err("select at least one band".into());
    }
    Ok(bands)
}

fn parse_days(value: &str) -> Result<Vec<Day>, String> {
    value
        .split(',')
        .map(|day| match day.trim().to_ascii_lowercase().as_str() {
            "monday" | "mon" => Ok(Day::Monday),
            "tuesday" | "tue" => Ok(Day::Tuesday),
            "wednesday" | "wed" => Ok(Day::Wednesday),
            "thursday" | "thu" => Ok(Day::Thursday),
            "friday" | "fri" => Ok(Day::Friday),
            "saturday" | "sat" => Ok(Day::Saturday),
            "sunday" | "sun" => Ok(Day::Sunday),
            _ => Err("expected comma-separated weekday names".into()),
        })
        .collect()
}

fn usage(message: &'static str) -> Error {
    Error::new(ErrorKind::Usage, message)
}

fn render(args: Args, networks: &[Network]) -> anyhow::Result<CommandResult> {
    let data = match args.command {
        Command::List => Value::Array(networks.iter().map(Network::summary).collect()),
        Command::Show { id } => instantctl_api::client::wlan::select(networks, &id)?.details(),
        _ => return Err(usage("expected wireless network list or show").into()),
    };
    Ok(CommandResult::success(data))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::{CommandFactory, Parser};
    use instantctl_api::secret::SecretString;

    #[test]
    fn clap_has_only_a_boolean_passphrase_input_flag_and_no_secret_argument() {
        let mut root = crate::cli::Cli::command();
        root.build();
        let wlan = root.find_subcommand("wlan").unwrap();
        for noun in ["passphrase", "create", "update"] {
            let command = wlan.find_subcommand(noun).unwrap();
            let flag = command
                .get_arguments()
                .find(|arg| arg.get_id() == "passphrase_stdin")
                .unwrap();
            assert!(matches!(flag.get_action(), clap::ArgAction::SetTrue));
            assert_eq!(flag.get_num_args().unwrap().max_values(), 0);
            for arg in command.get_arguments() {
                assert!(!arg.is_allow_hyphen_values_set(), "{noun}/{}", arg.get_id());
                assert!(!["secret", "passphrase", "password"].contains(&arg.get_id().as_str()));
            }
        }
    }

    #[test]
    fn secret_debug_is_redacted() {
        let secret = SecretString::new("ssid-passphrase-sentinel");
        assert_eq!(format!("{secret:?}"), "(redacted)");
        let patch = Patch {
            passphrase: Some(secret),
            ..Patch::default()
        };
        assert!(!format!("{patch:?}").contains("ssid-passphrase-sentinel"));
    }

    #[test]
    fn local_patch_validation_rejects_short_passphrase_and_six_ghz_without_credentials() {
        let patch = Patch {
            passphrase: Some(SecretString::new("seven!!")),
            ..Patch::default()
        };
        let error = patch.validate().unwrap_err();
        assert_eq!(error.kind, ErrorKind::Usage);
        assert!(!error.to_string().contains("seven!!"));

        let security = PatchArgs {
            security: Some(SecurityKind::Wpa2Personal),
            bands: Some(Bands {
                two_four: false,
                five: false,
                six: true,
            }),
            ..PatchArgs::default()
        };
        let error = security.into_patch().unwrap().validate().unwrap_err();
        assert_eq!(error.kind, ErrorKind::Usage);
    }

    #[test]
    fn patch_args_build_complete_timed_schedule_and_bandwidth() {
        let patch = PatchArgs {
            schedule: Some(ScheduleKind::Timed),
            days: Some("mon,Wed".into()),
            start: Some("08:00".into()),
            end: Some("18:00".into()),
            bandwidth: Some(BandwidthKind::PerClient),
            download: Some(20),
            upload: Some(5),
            ..PatchArgs::default()
        }
        .into_patch()
        .unwrap();
        patch.validate().unwrap();
        assert!(matches!(patch.schedule, Some(Schedule::Timed { .. })));
        assert!(matches!(patch.bandwidth, Some(Bandwidth::PerClient { .. })));
    }

    #[test]
    fn advanced_cli_flags_parse_into_the_advanced_patch() {
        let cli = crate::cli::Cli::try_parse_from([
            "instantctl",
            "--site",
            "123e4567-e89b-12d3-a456-426614174000",
            "wlan",
            "create",
            "staff",
            "--security",
            "wpa3-enterprise",
            "--radius-profile",
            "corp-radius",
            "--wired-network",
            "staff-network",
            "--ap",
            "ap-1",
            "--ap",
            "ap-2",
            "--captive-portal",
            "false",
            "--legacy-rates",
            "true",
            "--wifi6",
            "true",
            "--ofdma",
            "true",
            "--wifi7",
            "true",
            "--mlo",
            "true",
            "--multicast-optimization",
            "true",
            "--broadcast-all-bands",
            "false",
            "--traffic-priority",
            "very-high",
        ])
        .unwrap();
        let crate::cli::Command::Wlan(Args {
            command: Command::Create { patch, .. },
        }) = cli.command
        else {
            panic!("expected WLAN create command");
        };
        let patch = patch.into_patch().unwrap();
        assert!(matches!(patch.security, Some(Security::Wpa3Enterprise)));
        assert!(matches!(
            patch.advanced.binding,
            Some(Binding::Network(ref selector)) if selector == "staff-network"
        ));
        assert_eq!(
            patch.advanced.radius_profile.as_deref(),
            Some("corp-radius")
        );
        assert!(matches!(
            patch.advanced.access_points,
            Some(AccessPoints::Selected(ref aps))
                if aps.iter().map(String::as_str).collect::<Vec<_>>() == vec!["ap-1", "ap-2"]
        ));
        assert_eq!(patch.advanced.captive_portal, Some(false));
        assert_eq!(patch.advanced.legacy_rates, Some(true));
        assert_eq!(patch.advanced.wifi6, Some(true));
        assert_eq!(patch.advanced.ofdma, Some(true));
        assert_eq!(patch.advanced.wifi7, Some(true));
        assert_eq!(patch.advanced.mlo, Some(true));
        assert_eq!(patch.advanced.multicast_optimization, Some(true));
        assert_eq!(patch.advanced.broadcast_all_bands, Some(false));
        assert!(matches!(
            patch.advanced.traffic_priority,
            Some(TrafficPriority::VeryHigh)
        ));
    }

    #[test]
    fn advanced_vlan_and_all_ap_forms_parse() {
        let cli = crate::cli::Cli::try_parse_from([
            "instantctl",
            "wlan",
            "update",
            "ssid-id",
            "--vlan",
            "40",
            "--all-aps",
        ])
        .unwrap();
        let crate::cli::Command::Wlan(Args {
            command: Command::Update { patch, .. },
        }) = cli.command
        else {
            panic!("expected WLAN update command");
        };
        let patch = patch.into_patch().unwrap();
        assert!(matches!(patch.advanced.binding, Some(Binding::Vlan(40))));
        assert!(matches!(
            patch.advanced.access_points,
            Some(AccessPoints::All)
        ));
    }
}
