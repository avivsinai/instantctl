use super::access_common::{self, usage};
use crate::{
    context::{CommandContext, CommandResult},
    mutation::{self, Options},
    output::Format,
};
use clap::{Args as ClapArgs, Subcommand, ValueEnum};
use instantctl_api::{
    Error,
    client::access::schedule::{self, Day, Mode, Patch, TimeRange},
};
use std::collections::BTreeMap;

#[derive(Debug, ClapArgs)]
pub struct Args {
    #[command(subcommand)]
    pub command: Command,
}
#[derive(Debug, Subcommand)]
pub enum Command {
    /// List named schedules and policy reference counts.
    #[command(alias = "ls")]
    List,
    /// Show a schedule by ID or exact unique name.
    Show { id: String },
    /// Create a named schedule using the site's schedule defaults.
    Create {
        new_name: String,
        #[command(flatten)]
        patch: PatchArgs,
        #[command(flatten)]
        options: Options,
    },
    /// Update a named schedule, preserving its complete fetched configuration.
    Update {
        id: String,
        #[command(flatten)]
        patch: PatchArgs,
        #[command(flatten)]
        options: Options,
    },
    /// Delete a named schedule. Referencing policies require --yes.
    Delete {
        id: String,
        #[command(flatten)]
        options: Options,
    },
}
#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum ModeKind {
    None,
    Simple,
    Week,
}
#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum DayKind {
    Monday,
    Tuesday,
    Wednesday,
    Thursday,
    Friday,
    Saturday,
    Sunday,
}
impl DayKind {
    fn into_day(self) -> Day {
        match self {
            Self::Monday => Day::Monday,
            Self::Tuesday => Day::Tuesday,
            Self::Wednesday => Day::Wednesday,
            Self::Thursday => Day::Thursday,
            Self::Friday => Day::Friday,
            Self::Saturday => Day::Saturday,
            Self::Sunday => Day::Sunday,
        }
    }
}
#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum RangeKind {
    AllDay,
    Between,
}
#[derive(Clone, Debug, Default, ClapArgs)]
pub struct PatchArgs {
    #[arg(long)]
    name: Option<String>,
    #[arg(long, value_enum)]
    mode: Option<ModeKind>,
    #[arg(long, value_enum, value_delimiter = ',')]
    days: Option<Vec<DayKind>>,
    #[arg(long, value_enum)]
    range: Option<RangeKind>,
    #[arg(long, requires = "end")]
    start: Option<String>,
    #[arg(long, requires = "start")]
    end: Option<String>,
    /// Repeat DAY=all-day, DAY=inactive, or DAY=HH:mm-HH:mm for a weekly schedule.
    #[arg(long)]
    week_day: Vec<String>,
}
impl PatchArgs {
    fn into_patch(self) -> Result<Patch, Error> {
        let range = match (self.range, self.start, self.end) {
            (Some(RangeKind::AllDay), None, None) => Some(TimeRange::AllDay),
            (None, None, None) => None,
            (None | Some(RangeKind::Between), Some(start), Some(end)) => {
                Some(TimeRange::Between { start, end })
            }
            _ => {
                return Err(usage(
                    "between requires --start and --end; all-day cannot take times",
                ));
            }
        };
        let mut week = BTreeMap::new();
        for value in self.week_day {
            let (day, range) = value.split_once('=').ok_or_else(|| {
                usage("weekly day must be DAY=all-day, DAY=inactive, or DAY=HH:mm-HH:mm")
            })?;
            let day = DayKind::from_str(day, false)
                .map_err(|_| usage("weekly schedule day is invalid"))?
                .into_day();
            let range = match range {
                "all-day" => TimeRange::AllDay,
                "inactive" => TimeRange::Inactive,
                value => {
                    let (start, end) = value
                        .split_once('-')
                        .ok_or_else(|| usage("weekly time range is invalid"))?;
                    TimeRange::Between {
                        start: start.to_owned(),
                        end: end.to_owned(),
                    }
                }
            };
            if week.insert(day, range).is_some() {
                return Err(usage("weekly schedule day may only be set once"));
            }
        }
        let patch = Patch {
            name: self.name,
            mode: self.mode.map(|mode| match mode {
                ModeKind::None => Mode::None,
                ModeKind::Simple => Mode::Simple,
                ModeKind::Week => Mode::Week,
            }),
            days: self
                .days
                .map(|days| days.into_iter().map(DayKind::into_day).collect()),
            range,
            week,
        };
        patch.validate()?;
        Ok(patch)
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
            let rows = schedule::list(&api, &site).await?;
            Ok(CommandResult::success(match args.command {
                Command::Show { id } => schedule::show(&rows, &id)?,
                _ => serde_json::to_value(if context.format == Format::Table {
                    rows.iter().map(schedule::summary).collect::<Vec<_>>()
                } else {
                    rows
                })?,
            }))
        }
        command => {
            let (action, patch, options, operation) = match command {
                Command::Create {
                    new_name,
                    patch,
                    options,
                } => {
                    let mut patch = patch.into_patch()?;
                    if patch.name.is_some() {
                        return Err(usage("create uses its positional name; omit --name").into());
                    }
                    patch.name = Some(new_name);
                    (Action::Create, patch, options, "schedule.create")
                }
                Command::Update { id, patch, options } => (
                    Action::Update(id),
                    patch.into_patch()?,
                    options,
                    "schedule.update",
                ),
                Command::Delete { id, options } => (
                    Action::Delete(id),
                    Patch::default(),
                    options,
                    "schedule.delete",
                ),
                _ => return Err(usage("expected a schedule mutation").into()),
            };
            patch.validate()?;
            if matches!(action, Action::Update(_)) && patch.is_empty() {
                return Err(usage("specify at least one schedule change").into());
            }
            options.preflight(context.token_source)?;
            let context = access_common::context(context).await?;
            let api = super::site::read::client(&context)?;
            let site = super::site::read::site_id(&api, &context).await?;
            let (backend, plan) = match action {
                Action::Create => schedule::create(&api, &site, patch).await?,
                Action::Update(id) => schedule::update(&api, &site, &id, patch).await?,
                Action::Delete(id) => schedule::delete(&api, &site, &id, options.yes).await?,
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
