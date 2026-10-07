use std::{collections::BTreeSet, fs};

use clap::{Args as ClapArgs, Subcommand, ValueEnum};
use instantctl_api::{
    Error, ErrorKind,
    client::port_settings::{
        ActiveSchedule, ProfilePatch, SchedulePatch, SimpleSchedule, WeekSchedule, Weekday,
        WeekdaySchedule,
    },
};

use crate::{
    context::{CommandContext, CommandResult},
    mutation::{self, Options},
};

#[derive(Debug, ClapArgs)]
pub struct ProfileArgs {
    #[command(subcommand)]
    pub command: ProfileCommand,
}

#[derive(Debug, Subcommand)]
pub enum ProfileCommand {
    /// List port profiles.
    #[command(alias = "ls")]
    List,
    /// Show a port profile by exact name or identifier.
    Show { selector: String },
    /// Create a port profile by cloning a source profile with no port assignments.
    Create {
        name: String,
        #[arg(long = "from", required = true)]
        from_selector: String,
        #[command(flatten)]
        options: Options,
    },
    /// Update selected fields on a port profile.
    Set {
        selector: String,
        #[arg(long)]
        name: Option<String>,
        /// Wired network ID to use as the untagged network.
        #[arg(long)]
        untagged_network: Option<String>,
        /// Comma-separated wired network IDs. An empty value clears the tagged list.
        #[arg(long, value_name = "ID[,ID...]")]
        tagged_networks: Option<String>,
        /// Set port protection explicitly.
        #[arg(long, value_parser = clap::value_parser!(bool))]
        protected: Option<bool>,
        /// Set whether the port should trust traffic.
        #[arg(long, value_parser = clap::value_parser!(bool))]
        trust: Option<bool>,
        /// Set storm control explicitly.
        #[arg(long, value_parser = clap::value_parser!(bool))]
        storm_control: Option<bool>,
        /// Set PoE schedule use explicitly.
        #[arg(long, value_parser = clap::value_parser!(bool))]
        poe_schedule: Option<bool>,
        /// Allow changes that require overriding a safety restriction.
        #[arg(long)]
        force: bool,
        #[command(flatten)]
        options: Options,
    },
    /// Remove a port profile.
    Remove {
        selector: String,
        /// Allow removal when profiles are still referenced.
        #[arg(long)]
        force: bool,
        #[command(flatten)]
        options: Options,
    },
}

impl ProfileArgs {
    pub fn preflight(&self, context: &CommandContext) -> Result<(), Error> {
        if let Some(options) = profile_options(&self.command) {
            options.preflight(context.token_source)?;
        }
        Ok(())
    }
}

fn profile_options(command: &ProfileCommand) -> Option<Options> {
    match command {
        ProfileCommand::Create { options, .. }
        | ProfileCommand::Set { options, .. }
        | ProfileCommand::Remove { options, .. } => Some(*options),
        ProfileCommand::List | ProfileCommand::Show { .. } => None,
    }
}

pub async fn run_profile(
    args: ProfileArgs,
    context: &CommandContext,
) -> anyhow::Result<CommandResult> {
    args.preflight(context)?;
    let command = args.command;
    let patch = match &command {
        ProfileCommand::Set {
            name,
            untagged_network,
            tagged_networks,
            protected,
            trust,
            storm_control,
            poe_schedule,
            ..
        } => {
            let patch = ProfilePatch {
                name: name.clone(),
                untagged_network: untagged_network.clone().map(Some),
                tagged_networks: tagged_networks.as_deref().map(parse_id_list).transpose()?,
                protected: *protected,
                trust: *trust,
                storm_control: *storm_control,
                poe_schedule: *poe_schedule,
            };
            if name.is_none()
                && untagged_network.is_none()
                && tagged_networks.is_none()
                && protected.is_none()
                && trust.is_none()
                && storm_control.is_none()
                && poe_schedule.is_none()
            {
                return Err(usage("specify at least one port profile change").into());
            }
            Some(patch)
        }
        _ => None,
    };

    let (api, site, resolved) = selected_site_client(context).await?;
    match command {
        ProfileCommand::List => {
            let profiles =
                instantctl_api::client::port_settings::list_port_profiles(&api, &site).await?;
            Ok(CommandResult::success(serde_json::Value::Array(
                profiles
                    .iter()
                    .map(|profile| profile_row(&profile.details()))
                    .collect(),
            )))
        }
        ProfileCommand::Show { selector } => {
            let profile =
                instantctl_api::client::port_settings::read_port_profile(&api, &site, &selector)
                    .await?;
            Ok(CommandResult::success(profile.details()))
        }
        ProfileCommand::Create {
            name,
            from_selector,
            options,
        } => {
            let prepared = instantctl_api::client::port_settings::plan_clone_port_profile(
                &api,
                &site,
                &from_selector,
                &name,
            )
            .await?;
            mutation::execute(
                &prepared.backend,
                prepared.plan,
                "port.profile.clone",
                prepared.target,
                options,
                &resolved,
            )
            .await
        }
        ProfileCommand::Set {
            selector,
            force,
            options,
            ..
        } => {
            let prepared = instantctl_api::client::port_settings::plan_update_port_profile(
                &api,
                &site,
                &selector,
                patch.expect("profile set builds a patch"),
                force,
            )
            .await?;
            mutation::execute(
                &prepared.backend,
                prepared.plan,
                "port.profile.update",
                prepared.target,
                options,
                &resolved,
            )
            .await
        }
        ProfileCommand::Remove {
            selector,
            force,
            options,
        } => {
            let prepared = instantctl_api::client::port_settings::plan_delete_port_profile(
                &api, &site, &selector, force,
            )
            .await?;
            mutation::execute(
                &prepared.backend,
                prepared.plan,
                "port.profile.delete",
                prepared.target,
                options,
                &resolved,
            )
            .await
        }
    }
}

#[derive(Debug, ClapArgs)]
pub struct ScheduleArgs {
    #[command(subcommand)]
    pub command: ScheduleCommand,
}

#[derive(Debug, Subcommand)]
pub enum ScheduleCommand {
    /// Show the site's PoE schedule configuration.
    Show,
    /// Set the active PoE schedule.
    Set {
        #[arg(long, value_enum)]
        active: ActiveScheduleArg,
        /// Comma-separated weekdays, for a simple schedule.
        #[arg(long, value_name = "DAY[,DAY...]")]
        days: Option<String>,
        /// Start time in local HH:MM format, for a timed simple schedule.
        #[arg(long)]
        start: Option<String>,
        /// End time in local HH:MM format, for a timed simple schedule.
        #[arg(long)]
        end: Option<String>,
        /// Make the simple schedule active all day on the selected days.
        #[arg(long, conflicts_with_all = ["start", "end"])]
        all_day: bool,
        /// Weekly schedule JSON, or @PATH to read it from a file.
        #[arg(long, value_name = "JSON|@FILE")]
        week: Option<String>,
        /// Allow changes that require overriding a safety restriction.
        #[arg(long)]
        force: bool,
        #[command(flatten)]
        options: Options,
    },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum ActiveScheduleArg {
    None,
    Simple,
    Week,
}

impl ScheduleArgs {
    pub fn preflight(&self, context: &CommandContext) -> Result<(), Error> {
        if let ScheduleCommand::Set { options, .. } = &self.command {
            options.preflight(context.token_source)?;
        }
        Ok(())
    }
}

pub async fn run_schedule(
    args: ScheduleArgs,
    context: &CommandContext,
) -> anyhow::Result<CommandResult> {
    args.preflight(context)?;
    let command = args.command;
    let patch = match &command {
        ScheduleCommand::Set {
            active,
            days,
            start,
            end,
            all_day,
            week,
            ..
        } => Some(schedule_patch(
            *active,
            days.as_deref(),
            start.as_deref(),
            end.as_deref(),
            *all_day,
            week.as_deref(),
        )?),
        ScheduleCommand::Show => None,
    };
    let (api, site, resolved) = selected_site_client(context).await?;
    match command {
        ScheduleCommand::Show => Ok(CommandResult::success(
            instantctl_api::client::port_settings::poe_schedule(&api, &site).await?,
        )),
        ScheduleCommand::Set { force, options, .. } => {
            let prepared = instantctl_api::client::port_settings::plan_poe_schedule(
                &api,
                &site,
                patch.expect("schedule set builds a patch"),
                force,
            )
            .await?;
            mutation::execute(
                &prepared.backend,
                prepared.plan,
                "port.poe_schedule.set",
                prepared.target,
                options,
                &resolved,
            )
            .await
        }
    }
}

fn schedule_patch(
    active: ActiveScheduleArg,
    days: Option<&str>,
    start: Option<&str>,
    end: Option<&str>,
    all_day: bool,
    week: Option<&str>,
) -> Result<SchedulePatch, Error> {
    let simple_fields_supplied = days.is_some() || start.is_some() || end.is_some() || all_day;
    let active_schedule = Some(match active {
        ActiveScheduleArg::None => ActiveSchedule::None,
        ActiveScheduleArg::Simple => ActiveSchedule::Simple,
        ActiveScheduleArg::Week => ActiveSchedule::Week,
    });
    let (simple_schedule, week_schedule) = match active {
        ActiveScheduleArg::None => {
            if simple_fields_supplied || week.is_some() {
                return Err(usage("active none does not accept schedule fields"));
            }
            (None, None)
        }
        ActiveScheduleArg::Simple => {
            if week.is_some() {
                return Err(usage("active simple does not accept --week"));
            }
            let active_days = days
                .ok_or_else(|| usage("active simple requires --days"))
                .and_then(parse_weekdays)?;
            let (start_time, end_time) = if all_day {
                (None, None)
            } else {
                let start_time = start.ok_or_else(|| usage("timed schedule requires --start"))?;
                let end_time = end.ok_or_else(|| usage("timed schedule requires --end"))?;
                validate_time(start_time)?;
                validate_time(end_time)?;
                (Some(start_time.to_owned()), Some(end_time.to_owned()))
            };
            (
                Some(SimpleSchedule {
                    active_days,
                    start_time,
                    end_time,
                }),
                None,
            )
        }
        ActiveScheduleArg::Week => {
            if simple_fields_supplied {
                return Err(usage(
                    "active week does not accept --days or simple schedule times",
                ));
            }
            let json = week.ok_or_else(|| usage("active week requires --week"))?;
            (None, Some(parse_week_schedule(json)?))
        }
    };
    Ok(SchedulePatch {
        active_schedule,
        simple_schedule,
        week_schedule,
    })
}

fn parse_week_schedule(input: &str) -> Result<WeekSchedule, Error> {
    let contents = if let Some(path) = input.strip_prefix('@') {
        fs::read_to_string(path).map_err(|_| usage("could not read weekly schedule file"))?
    } else {
        input.to_owned()
    };
    let schedule: WeekSchedule = serde_json::from_str(&contents)
        .map_err(|_| usage("weekly schedule must be valid WeekSchedule JSON"))?;
    let actual: BTreeSet<_> = schedule.days.keys().copied().collect();
    let expected = [
        Weekday::Monday,
        Weekday::Tuesday,
        Weekday::Wednesday,
        Weekday::Thursday,
        Weekday::Friday,
        Weekday::Saturday,
        Weekday::Sunday,
    ]
    .into_iter()
    .collect();
    if actual != expected {
        return Err(usage("weekly schedule must contain all seven weekdays"));
    }
    for day in schedule.days.values() {
        validate_weekday_schedule(day)?;
    }
    Ok(schedule)
}

fn validate_weekday_schedule(day: &WeekdaySchedule) -> Result<(), Error> {
    if day.active_all_day && !day.enabled {
        return Err(usage("an all-day weekly range must be enabled"));
    }
    match (
        day.enabled,
        day.active_all_day,
        day.start_time.as_deref(),
        day.end_time.as_deref(),
    ) {
        (false, false, None, None) | (true, true, None, None) => Ok(()),
        (true, false, Some(start), Some(end)) => {
            validate_time(start)?;
            validate_time(end)
        }
        _ => Err(usage(
            "each enabled timed weekday needs start_time and end_time; all-day days omit both",
        )),
    }
}

fn parse_weekdays(input: &str) -> Result<Vec<Weekday>, Error> {
    let mut result = Vec::new();
    let mut seen = BTreeSet::new();
    for part in input.split(',') {
        let day = match part.trim().to_ascii_lowercase().as_str() {
            "monday" | "mon" => Weekday::Monday,
            "tuesday" | "tue" => Weekday::Tuesday,
            "wednesday" | "wed" => Weekday::Wednesday,
            "thursday" | "thu" => Weekday::Thursday,
            "friday" | "fri" => Weekday::Friday,
            "saturday" | "sat" => Weekday::Saturday,
            "sunday" | "sun" => Weekday::Sunday,
            _ => return Err(usage("invalid weekday; use mon,tue,wed,thu,fri,sat,sun")),
        };
        if !seen.insert(day) {
            return Err(usage("weekday list contains a duplicate day"));
        }
        result.push(day);
    }
    if result.is_empty() {
        return Err(usage("weekday list cannot be empty"));
    }
    Ok(result)
}

fn parse_id_list(input: &str) -> Result<Vec<String>, Error> {
    if input.trim().is_empty() {
        return Ok(Vec::new());
    }
    let mut result = Vec::new();
    let mut seen = BTreeSet::new();
    for item in input.split(',') {
        let id = item.trim();
        if id.is_empty() {
            return Err(usage("tagged network list contains an empty ID"));
        }
        if !seen.insert(id.to_owned()) {
            return Err(usage("tagged network list contains a duplicate ID"));
        }
        result.push(id.to_owned());
    }
    Ok(result)
}

fn validate_time(input: &str) -> Result<(), Error> {
    let Some((hour, minute)) = input.split_once(':') else {
        return Err(usage("time must use HH:MM"));
    };
    let parsed = (hour.parse::<u8>(), minute.parse::<u8>());
    if hour.len() != 2
        || minute.len() != 2
        || !hour.bytes().all(|byte| byte.is_ascii_digit())
        || !minute.bytes().all(|byte| byte.is_ascii_digit())
        || !matches!(parsed, (Ok(hour), Ok(minute)) if hour <= 23 && minute <= 59)
    {
        return Err(usage("time must be a valid 24-hour HH:MM value"));
    }
    Ok(())
}

#[derive(Debug, ClapArgs)]
pub struct EeeArgs {
    #[command(subcommand)]
    pub command: EeeCommand,
}

#[derive(Debug, Subcommand)]
pub enum EeeCommand {
    /// Show energy-efficient Ethernet and PoE settings.
    Show,
    /// Enable or disable energy-efficient Ethernet.
    Set {
        #[arg(action = clap::ArgAction::Set)]
        enabled: bool,
        /// Allow changes that require overriding a safety restriction.
        #[arg(long)]
        force: bool,
        #[command(flatten)]
        options: Options,
    },
}

impl EeeArgs {
    pub fn preflight(&self, context: &CommandContext) -> Result<(), Error> {
        if let EeeCommand::Set { options, .. } = &self.command {
            options.preflight(context.token_source)?;
        }
        Ok(())
    }
}

pub async fn run_eee(args: EeeArgs, context: &CommandContext) -> anyhow::Result<CommandResult> {
    args.preflight(context)?;
    let (api, site, resolved) = selected_site_client(context).await?;
    match args.command {
        EeeCommand::Show => Ok(CommandResult::success(
            instantctl_api::client::port_settings::power_management(&api, &site).await?,
        )),
        EeeCommand::Set {
            enabled,
            force,
            options,
        } => {
            let prepared =
                instantctl_api::client::port_settings::plan_eee(&api, &site, enabled, force)
                    .await?;
            mutation::execute(
                &prepared.backend,
                prepared.plan,
                "port.eee.set",
                prepared.target,
                options,
                &resolved,
            )
            .await
        }
    }
}

async fn selected_site_client(
    context: &CommandContext,
) -> Result<(crate::context::PortalClient, String, CommandContext), Error> {
    super::site::read::check_site(context)?;
    let resolved = if context.site.is_none() && context.token_source.uses_profile() {
        context.resolve_profile_default().await?
    } else {
        context.clone()
    };
    let api = super::site::read::client(&resolved)?;
    let site = super::site::read::site_id(&api, &resolved).await?;
    Ok((api, site, resolved))
}

fn usage(message: &str) -> Error {
    Error::new(ErrorKind::Usage, message)
}

fn profile_row(value: &serde_json::Value) -> serde_json::Value {
    super::site::read::project(
        value,
        &[
            ("id", "id"),
            ("name", "name"),
            ("untagged_network", "customMappingUntaggedWiredNetworkId"),
            (
                "tagged_networks",
                "customMappingTaggedWiredNetworksSelection",
            ),
            ("protected", "protectedPortEnabled"),
            ("trust", "shouldTrustTraffic"),
            ("storm_control", "stormControlEnabled"),
            ("poe_schedule", "usePoeSchedule"),
        ],
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn profile_projection_retains_missing_state_as_null() {
        let row = profile_row(&json!({"id":"p1","name":"Lab","shouldTrustTraffic":false}));
        assert_eq!(row["trust"], false);
        assert!(row["protected"].is_null());
        assert!(row["poe_schedule"].is_null());
        assert_eq!(row["name"], "Lab");
    }
    #[test]
    fn lists_and_times_reject_invalid_or_ambiguous_data() {
        assert!(parse_id_list("x,x").is_err());
        assert!(parse_weekdays("mon,monday").is_err());
        for time in ["24:00", "09:60", "9:00", "０９:００"] {
            assert!(validate_time(time).is_err(), "{time}");
        }
        assert_eq!(parse_id_list("").unwrap(), Vec::<String>::new());
        assert_eq!(
            parse_weekdays("mon,sun").unwrap(),
            vec![Weekday::Monday, Weekday::Sunday]
        );
        assert!(validate_time("09:00").is_ok());
    }
}
