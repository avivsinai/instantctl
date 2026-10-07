use super::access_common;
use crate::{
    context::{CommandContext, CommandResult},
    mutation::{self, Options},
    output::Format,
};
use clap::{Args as ClapArgs, Subcommand};
use instantctl_api::{Error, client::firmware};
use serde_json::{Map, Value};

#[derive(Debug, ClapArgs)]
pub struct Args {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Read or configure the firmware update window.
    Window {
        #[command(subcommand)]
        command: WindowCommand,
    },
    /// Start an available update and verify that maintenance begins.
    UpdateNow {
        #[command(flatten)]
        options: Options,
    },
    /// Send a local update timestamp to the portal without inferring a timezone.
    Schedule {
        /// Local timestamp in YYYY-MM-DDTHH:mm:ss form.
        at: String,
        #[command(flatten)]
        options: Options,
    },
}

#[derive(Debug, Subcommand)]
pub enum WindowCommand {
    /// Read the firmware update window.
    Get,
    /// Set selected firmware update window fields.
    Set {
        /// Lowercase weekday such as monday.
        #[arg(long)]
        day: Option<String>,
        /// Site-local 24-hour time in HH:mm form.
        #[arg(long)]
        start_time: Option<String>,
        /// Update delay in days: 0, 7, 14, 21, or 28.
        #[arg(long)]
        delay_days: Option<u8>,
        #[command(flatten)]
        options: Options,
    },
}

impl Args {
    /// Validate user input and mutation confirmation before profile resolution.
    pub fn preflight(&self, context: &CommandContext) -> Result<(), Error> {
        match &self.command {
            Command::Window {
                command:
                    WindowCommand::Set {
                        day,
                        start_time,
                        delay_days,
                        options,
                    },
            } => {
                firmware::WindowPatch {
                    day: day.clone(),
                    start_time: start_time.clone(),
                    update_delay_in_days: *delay_days,
                }
                .validate()?;
                options.preflight(context.token_source)
            }
            Command::UpdateNow { options } => options.preflight(context.token_source),
            Command::Schedule { at, options } => {
                firmware::validate_schedule(at)?;
                options.preflight(context.token_source)
            }
            Command::Window {
                command: WindowCommand::Get,
            } => Ok(()),
        }
    }
}

pub async fn run(args: Args, context: &CommandContext) -> anyhow::Result<CommandResult> {
    args.preflight(context)?;
    let context = access_common::context(context).await?;
    let api = super::site::read::client(&context)?;
    let site = super::site::read::site_id(&api, &context).await?;
    match args.command {
        Command::Window {
            command: WindowCommand::Get,
        } => {
            let window = firmware::get(&api, &site).await?;
            Ok(CommandResult::success(if context.format == Format::Table {
                window_table_data(&window)
            } else {
                window
            }))
        }
        Command::Window {
            command:
                WindowCommand::Set {
                    day,
                    start_time,
                    delay_days,
                    options,
                },
        } => {
            let patch = firmware::WindowPatch {
                day,
                start_time,
                update_delay_in_days: delay_days,
            };
            let (backend, plan) = firmware::set_window(&api, &site, patch).await?;
            mutation::execute(
                &backend,
                plan,
                "firmware.window.set",
                serde_json::json!({"site_id":site}),
                options,
                &context,
            )
            .await
        }
        Command::UpdateNow { options } => {
            let (backend, plan) = firmware::update_now(&api, &site).await?;
            mutation::execute(
                &backend,
                plan,
                "firmware.update-now",
                serde_json::json!({"site_id":site}),
                options,
                &context,
            )
            .await
        }
        Command::Schedule { at, options } => {
            let (backend, plan) = firmware::schedule(&api, &site, at).await?;
            mutation::execute(
                &backend,
                plan,
                "firmware.schedule",
                serde_json::json!({"site_id":site}),
                options,
                &context,
            )
            .await
        }
    }
}

fn window_table_data(window: &Value) -> Value {
    const FIELDS: &[&str] = &[
        "day",
        "startTime",
        "updateDelayInDays",
        "state",
        "currentVersion",
        "newUpdateVersion",
        "updateScheduleDateTime",
        "updateExpectedDateTime",
    ];
    let mut projected = Map::new();
    for field in FIELDS {
        projected.insert(
            (*field).to_owned(),
            window.get(*field).cloned().unwrap_or(Value::Null),
        );
    }
    Value::Object(projected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{credentials::CredentialSource, output::Format};
    use clap::Parser;

    fn context() -> CommandContext {
        CommandContext {
            profile: "firmware-cli-test-missing-profile".into(),
            site: Some("123e4567-e89b-12d3-a456-426614174000".into()),
            timeout: std::time::Duration::from_secs(2),
            format: Format::Json,
            token_source: CredentialSource::Stdin,
            prepared_token: None,
            protected_ports: Vec::new(),
        }
    }

    #[test]
    fn firmware_command_shapes_parse_including_values_that_need_semantic_validation() {
        for argv in [
            &["instantctl", "firmware", "window", "get"][..],
            &[
                "instantctl",
                "firmware",
                "window",
                "set",
                "--day",
                "monday",
                "--start-time",
                "09:00",
                "--delay-days",
                "7",
            ][..],
            &["instantctl", "firmware", "update-now"][..],
            &["instantctl", "firmware", "schedule", "2026-11-02T03:00:00"][..],
        ] {
            crate::cli::Cli::try_parse_from(argv).expect("firmware command should parse");
        }
    }

    #[test]
    fn window_schedule_and_confirmation_inputs_fail_in_preflight() {
        let invalid_window = Args {
            command: Command::Window {
                command: WindowCommand::Set {
                    day: None,
                    start_time: Some("25:00".into()),
                    delay_days: None,
                    options: Options::default(),
                },
            },
        };
        assert_eq!(
            invalid_window.preflight(&context()).unwrap_err().kind,
            instantctl_api::ErrorKind::Usage
        );

        let invalid_schedule = Args {
            command: Command::Schedule {
                at: "2026-02-30T03:00:00".into(),
                options: Options::default(),
            },
        };
        assert_eq!(
            invalid_schedule.preflight(&context()).unwrap_err().kind,
            instantctl_api::ErrorKind::Usage
        );

        let invalid_delay = Args {
            command: Command::Window {
                command: WindowCommand::Set {
                    day: None,
                    start_time: None,
                    delay_days: Some(8),
                    options: Options::default(),
                },
            },
        };
        assert_eq!(
            invalid_delay.preflight(&context()).unwrap_err().kind,
            instantctl_api::ErrorKind::Usage
        );

        let unconfirmed = Args {
            command: Command::UpdateNow {
                options: Options {
                    apply: true,
                    yes: false,
                },
            },
        };
        assert_eq!(
            unconfirmed.preflight(&context()).unwrap_err().kind,
            instantctl_api::ErrorKind::ConfirmationRequired
        );
    }

    #[test]
    fn window_table_projection_keeps_only_known_fields_and_nulls_missing_values() {
        let row = serde_json::json!({
            "day":"monday", "startTime":"03:00", "state":"inProgress",
            "currentVersion":"1.2.3", "vendor":"keep in raw API response"
        });
        let projected = window_table_data(&row);
        assert_eq!(
            projected,
            serde_json::json!({
                "day":"monday", "startTime":"03:00", "updateDelayInDays":null,
                "state":"inProgress", "currentVersion":"1.2.3",
                "newUpdateVersion":null, "updateScheduleDateTime":null,
                "updateExpectedDateTime":null
            })
        );
        assert!(projected.get("vendor").is_none());
    }
}
