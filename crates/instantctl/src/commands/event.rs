use std::{
    collections::HashSet,
    future::Future,
    io::{self, Write},
    time::Duration,
};

use clap::{Args as ClapArgs, Subcommand};
use instantctl_api::{
    Error, ErrorKind,
    client::monitoring::{Alert, Event},
};
use jiff::{Span, SpanRelativeTo, Timestamp};
use serde_json::Value;

use super::site::read;
use crate::{
    context::CommandContext,
    exit::ExitStatus,
    output::{self, Format},
};

/// Flattened into the root parser so event and alert share one dispatch entry.
#[derive(Debug, Subcommand)]
pub enum Args {
    /// List recent site events or follow new events.
    Event(EventArgs),
    /// List the site's reported alerts.
    Alert(AlertArgs),
}

#[derive(Debug, ClapArgs)]
pub struct EventArgs {
    #[command(subcommand)]
    pub command: EventCommand,
}

#[derive(Debug, Subcommand)]
pub enum EventCommand {
    /// List events, oldest first. Follow emits JSON lines when piped.
    List(ListArgs),
}

#[derive(Debug, ClapArgs)]
pub struct ListArgs {
    /// Include events at or after this RFC3339 time or duration ago (e.g. 2h, 1d).
    #[arg(long, allow_hyphen_values = true, value_parser = since_argument)]
    pub since: Option<Timestamp>,
    /// Poll until Ctrl+C. JSON output is one object per line.
    #[arg(long)]
    pub follow: bool,
    /// Poll interval in seconds (10–86400).
    #[arg(long, default_value_t = 15, requires = "follow", value_parser = clap::value_parser!(u64).range(10..=86400))]
    pub interval: u64,
    /// Number of recent events to print on the first follow poll; 0 skips history.
    #[arg(long, default_value_t = 20, requires = "follow")]
    pub tail: usize,
}

#[derive(Debug, ClapArgs)]
pub struct AlertArgs {
    #[command(subcommand)]
    pub command: AlertCommand,
}

#[derive(Debug, Subcommand)]
pub enum AlertCommand {
    /// List active and cleared alerts as reported by the portal.
    List,
}

pub async fn run(
    args: Args,
    context: &CommandContext,
    out: &mut impl Write,
) -> anyhow::Result<ExitStatus> {
    let args = match args {
        Args::Event(EventArgs {
            command: EventCommand::List(args),
        }) => args,
        Args::Alert(_) => {
            let api = read::client(context)?;
            let site = read::site_id(&api, context).await?;
            output::write_data(out, context.format, &alert_rows(&api.alerts(&site).await?)?)?;
            return Ok(ExitStatus::Success);
        }
    };
    // Clap resolves and validates the cutoff before saved-profile resolution.
    let since = args.since;
    let api = read::client(context)?;
    if !args.follow {
        let site = read::site_id(&api, context).await?;
        let events = ordered(&api.events(&site).await?, since)?;
        output::write_data(out, context.format, &event_rows(&events)?)?;
        return Ok(ExitStatus::Success);
    }

    let stop = tokio::signal::ctrl_c();
    tokio::pin!(stop);
    let site = tokio::select! {
        biased;
        result = &mut stop => { result?; out.flush()?; return Ok(ExitStatus::Success); }
        result = read::site_id(&api, context) => result?,
    };
    follow(
        || api.events(&site),
        &mut stop,
        FollowOptions {
            since,
            tail: args.tail,
            interval: Duration::from_secs(args.interval),
        },
        context.format,
        out,
        &mut io::stderr(),
    )
    .await?;
    Ok(ExitStatus::Success)
}

fn usage(message: &str) -> Error {
    Error::new(ErrorKind::Usage, message)
}

fn since_argument(value: &str) -> Result<Timestamp, Error> {
    Ok(parse_since(Some(value), Timestamp::now())?.expect("a supplied cutoff"))
}

fn parse_since(value: Option<&str>, now: Timestamp) -> Result<Option<Timestamp>, Error> {
    let Some(value) = value else { return Ok(None) };
    let timestamp = match value.parse::<Timestamp>() {
        Ok(timestamp) => timestamp,
        Err(_) => {
            let span: Span = value.parse().map_err(|_| {
                usage("--since must be an RFC3339 time or a duration, such as 2h or 1d")
            })?;
            let duration = span
                .to_duration(SpanRelativeTo::days_are_24_hours())
                .map_err(|_| {
                    usage("--since duration must use weeks, days, hours, minutes, or seconds")
                })?;
            if duration.is_negative() {
                return Err(usage("--since cannot be in the future"));
            }
            now.checked_sub(duration)
                .map_err(|_| usage("--since duration is out of range"))?
        }
    };
    if timestamp > now {
        return Err(usage("--since cannot be in the future"));
    }
    Ok(Some(timestamp))
}

fn event_time(event: &Event) -> Result<Option<Timestamp>, Error> {
    event
        .occurrence_time
        .map(|seconds| {
            // The portal stores epoch seconds, not RFC3339 strings or milliseconds.
            Timestamp::from_nanosecond((seconds * 1_000_000_000.0) as i128).map_err(|_| {
                Error::new(
                    ErrorKind::Unverified,
                    "event occurrenceTime is out of range",
                )
            })
        })
        .transpose()
}

fn ordered(events: &[Event], since: Option<Timestamp>) -> Result<Vec<Event>, Error> {
    let mut timed = events
        .iter()
        .map(|event| Ok((event_time(event)?, event)))
        .collect::<Result<Vec<_>, Error>>()?;
    timed.retain(|(time, _)| since.is_none_or(|since| time.is_some_and(|time| time >= since)));
    timed.sort_by(|(a_time, a), (b_time, b)| a_time.cmp(b_time).then_with(|| a.id.cmp(&b.id)));
    Ok(timed.into_iter().map(|(_, event)| event.clone()).collect())
}

fn event_rows(events: &[Event]) -> anyhow::Result<Value> {
    Ok(Value::Array(
        events
            .iter()
            .map(|event| {
                Ok(read::project(
                    &serde_json::to_value(event)?,
                    &[
                        ("id", "id"),
                        ("occurrence_time", "occurrenceTime"),
                        ("event", "event"),
                        ("category", "category"),
                        ("type", "type"),
                        ("state", "state"),
                        ("account", "account"),
                        ("source", "source"),
                        ("attributes", "attributes"),
                    ],
                ))
            })
            .collect::<anyhow::Result<Vec<_>>>()?,
    ))
}

fn alert_rows(alerts: &[Alert]) -> anyhow::Result<Value> {
    Ok(Value::Array(
        alerts
            .iter()
            .map(|alert| {
                Ok(read::project(
                    &serde_json::to_value(alert)?,
                    &[
                        ("id", "id"),
                        ("type", "type"),
                        ("severity", "severity"),
                        ("raised_time", "raisedTime"),
                        ("cleared_time", "clearedTime"),
                        ("properties", "alertTypeProperties"),
                    ],
                ))
            })
            .collect::<anyhow::Result<Vec<_>>>()?,
    ))
}

struct FollowOptions {
    since: Option<Timestamp>,
    tail: usize,
    interval: Duration,
}

#[derive(Default)]
struct Cursor {
    seen: HashSet<String>,
    newest: Option<Timestamp>,
    initialized: bool,
}

impl Cursor {
    fn take(&mut self, events: &[Event], options: &FollowOptions) -> Result<Vec<Event>, Error> {
        let events = ordered(events, options.since)?;
        let mut fresh = Vec::new();
        let mut newest = self.newest;
        for event in events {
            let time = event_time(&event)?;
            // Unseen IDs at the watermark can arrive in a later poll with the same second.
            let newer = !self.initialized
                || time.is_some_and(|time| self.newest.is_none_or(|last| time >= last));
            newest = newest.max(time);
            if self.seen.insert(event.id.clone()) && newer {
                fresh.push(event);
            }
        }
        self.newest = newest;
        if !self.initialized {
            fresh.drain(..fresh.len().saturating_sub(options.tail));
        }
        self.initialized = true;
        Ok(fresh)
    }
}

async fn follow<F, Fetch, Stop>(
    mut fetch: F,
    stop: Stop,
    options: FollowOptions,
    format: Format,
    out: &mut impl Write,
    warnings: &mut impl Write,
) -> anyhow::Result<()>
where
    F: FnMut() -> Fetch,
    Fetch: Future<Output = Result<Vec<Event>, Error>>,
    Stop: Future<Output = io::Result<()>>,
{
    tokio::pin!(stop);
    let mut cursor = Cursor::default();
    loop {
        let result = tokio::select! {
            biased;
            result = &mut stop => { result?; out.flush()?; return Ok(()); }
            result = fetch() => result,
        };
        match result {
            Ok(events) => write_follow(out, format, &cursor.take(&events, &options)?)?,
            Err(error) if matches!(error.kind, ErrorKind::General | ErrorKind::RetryLater) => {
                writeln!(warnings, "warning: {}; following will retry", error.message)?;
                warnings.flush()?;
            }
            Err(error) => return Err(error.into()),
        }
        // Delay after each attempt: slow GETs never cause catch-up bursts.
        tokio::select! {
            biased;
            result = &mut stop => { result?; out.flush()?; return Ok(()); }
            _ = tokio::time::sleep(options.interval) => {},
        }
    }
}

fn write_follow(out: &mut impl Write, format: Format, events: &[Event]) -> anyhow::Result<()> {
    if events.is_empty() {
        return Ok(());
    }
    let rows = event_rows(events)?;
    match format {
        Format::Json => {
            for row in rows.as_array().expect("event rows are an array") {
                output::write_bytes(out, format!("{}\n", serde_json::to_string(row)?).as_bytes())?;
            }
        }
        Format::Yaml => {
            for row in rows.as_array().expect("event rows are an array") {
                output::write_bytes(out, b"---\n")?;
                output::write_data(out, format, row)?;
            }
        }
        Format::Table => output::write_data(out, format, &rows)?,
    }
    Ok(())
}

#[cfg(test)]
mod tests;
