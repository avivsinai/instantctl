use super::access_common;
use crate::{
    context::{CommandContext, CommandResult},
    credentials::CredentialSource,
    mutation::{self, Options},
    output::Format,
};
use clap::{ArgAction, Args as ClapArgs, Subcommand};
use instantctl_api::{Error, client::policies};
use serde_json::{Map, Value};

#[derive(Debug, ClapArgs)]
pub struct Args {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// List application and firewall policies.
    List,
    /// Show a policy by ID or exact name.
    Show { selector: String },
    /// Read or change site application visibility.
    AppVisibility {
        #[command(subcommand)]
        command: AppVisibilityCommand,
    },
}

#[derive(Debug, Subcommand)]
pub enum AppVisibilityCommand {
    /// Read the application visibility setting.
    Get,
    /// Set whether application categories are visible.
    Set {
        /// New visibility value.
        #[arg(action = ArgAction::Set, value_parser = clap::value_parser!(bool))]
        enabled: bool,
        #[command(flatten)]
        options: Options,
    },
}

impl Args {
    /// Reject bad selectors and unconfirmed writes before loading credentials.
    pub fn preflight(&self, token_source: CredentialSource) -> Result<(), Error> {
        match &self.command {
            Command::Show { selector } => policies::validate_selector(selector),
            Command::AppVisibility {
                command: AppVisibilityCommand::Set { options, .. },
            } => options.preflight(token_source),
            Command::List
            | Command::AppVisibility {
                command: AppVisibilityCommand::Get,
            } => Ok(()),
        }
    }
}

pub async fn run(args: Args, context: &CommandContext) -> anyhow::Result<CommandResult> {
    args.preflight(context.token_source)?;
    let context = access_common::context(context).await?;
    let api = super::site::read::client(&context)?;
    let site = super::site::read::site_id(&api, &context).await?;
    match args.command {
        Command::List => {
            let rows = policies::list(&api, &site).await?;
            Ok(CommandResult::success(if context.format == Format::Table {
                policy_list_data(&rows)
            } else {
                serde_json::to_value(rows)?
            }))
        }
        Command::Show { selector } => {
            let rows = policies::list(&api, &site).await?;
            Ok(CommandResult::success(policies::show(&rows, &selector)?))
        }
        Command::AppVisibility { command } => match command {
            AppVisibilityCommand::Get => Ok(CommandResult::success(serde_json::to_value(
                api.application_configuration(&site).await?,
            )?)),
            AppVisibilityCommand::Set { enabled, options } => {
                let (backend, plan) = policies::set_visibility(&api, &site, enabled).await?;
                mutation::execute(
                    &backend,
                    plan,
                    "policy.app-visibility.set",
                    serde_json::json!({"site_id":site}),
                    options,
                    &context,
                )
                .await
            }
        },
    }
}

fn policy_list_data(rows: &[Value]) -> Value {
    const FIELDS: &[&str] = &[
        "id",
        "name",
        "isEnabled",
        "policyType",
        "action",
        "scheduleId",
        "sourceRuleType",
    ];
    Value::Array(
        rows.iter()
            .map(|row| {
                let mut projected = Map::new();
                for field in FIELDS {
                    projected.insert(
                        (*field).to_owned(),
                        row.get(*field).cloned().unwrap_or(Value::Null),
                    );
                }
                Value::Object(projected)
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn policy_commands_parse_boolean_as_an_explicit_positional_value() {
        for (value, expected) in [("true", true), ("false", false)] {
            let cli = crate::cli::Cli::try_parse_from([
                "instantctl",
                "policy",
                "app-visibility",
                "set",
                value,
            ])
            .expect("visibility set command should parse");
            let crate::cli::Command::Policy(args) = cli.command else {
                panic!("policy command should be selected");
            };
            let Command::AppVisibility {
                command: AppVisibilityCommand::Set { enabled, options },
            } = args.command
            else {
                panic!("app-visibility set should be selected");
            };
            assert_eq!(enabled, expected);
            assert!(!options.apply);
            assert!(!options.yes);
        }
    }

    #[test]
    fn policy_surface_has_only_read_commands_and_confirmed_visibility_set() {
        for argv in [
            &["instantctl", "policy", "list"][..],
            &["instantctl", "policy", "show", "id-1"][..],
            &["instantctl", "policy", "app-visibility", "get"][..],
        ] {
            crate::cli::Cli::try_parse_from(argv).expect("read command should parse");
        }
        assert!(
            crate::cli::Cli::try_parse_from(["instantctl", "policy", "create", "New policy"])
                .is_err()
        );
    }

    #[test]
    fn preflight_rejects_empty_selector_and_unconfirmed_stdin_apply() {
        let show = Args {
            command: Command::Show {
                selector: " \t".into(),
            },
        };
        assert_eq!(
            show.preflight(CredentialSource::Environment)
                .unwrap_err()
                .kind,
            instantctl_api::ErrorKind::Usage
        );
        let set = Args {
            command: Command::AppVisibility {
                command: AppVisibilityCommand::Set {
                    enabled: true,
                    options: Options {
                        apply: true,
                        yes: false,
                    },
                },
            },
        };
        assert_eq!(
            set.preflight(CredentialSource::Stdin).unwrap_err().kind,
            instantctl_api::ErrorKind::ConfirmationRequired
        );
    }

    #[test]
    fn policy_list_projection_has_only_known_fields_and_nulls_missing_values() {
        let data = policy_list_data(&[serde_json::json!({
            "id":"policy-1", "name":"Staff", "isEnabled":false,
            "policyType":"application", "action":"allow", "vendor":"retain-in-api"
        })]);
        assert_eq!(
            data,
            serde_json::json!([{
                "id":"policy-1", "name":"Staff", "isEnabled":false,
                "policyType":"application", "action":"allow",
                "scheduleId":null, "sourceRuleType":null
            }])
        );
        assert!(data[0].get("vendor").is_none());
    }
}
