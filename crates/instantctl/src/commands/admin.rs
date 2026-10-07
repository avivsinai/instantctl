use std::path::PathBuf;

use super::access_common;
use crate::{
    context::{CommandContext, CommandResult},
    exit::ExitStatus,
    mutation::{self, Options},
    output::Format,
};
use clap::{ArgAction, Args as ClapArgs, Subcommand};
use instantctl_api::{Error, client::administration};
use serde_json::{Map, Value};

#[derive(Debug, ClapArgs)]
pub struct Args {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// List site administrators.
    List,
    /// Read the current account permissions.
    Permissions,
    /// Check whether an email address belongs to a site account.
    CheckAccount { email: String },
    /// Add an account to the site.
    Add {
        email: String,
        /// Site role to assign.
        #[arg(long, default_value = "administrator")]
        role: String,
        #[command(flatten)]
        options: Options,
    },
    /// Remove a site administrator by user ID or exact email.
    Remove {
        selector: String,
        #[command(flatten)]
        options: Options,
    },
    /// Change an administrator's site role.
    ChangeRole {
        selector: String,
        role: String,
        #[command(flatten)]
        options: Options,
    },
    /// Read or change site maintenance mode.
    MaintenanceMode {
        #[command(subcommand)]
        command: MaintenanceModeCommand,
    },
    /// Request a support token and save it to a protected file.
    SupportToken {
        /// File path for the support token. The file is created only with --apply.
        #[arg(long, required = true)]
        output: PathBuf,
        #[command(flatten)]
        options: Options,
    },
}

#[derive(Debug, Subcommand)]
pub enum MaintenanceModeCommand {
    /// Read the site's maintenance-mode state.
    Get,
    /// Set the site's maintenance-mode state.
    Set {
        /// New state: true or false.
        #[arg(action = ArgAction::Set, value_parser = clap::value_parser!(bool))]
        enabled: bool,
        #[command(flatten)]
        options: Options,
    },
}

impl Args {
    /// Validate local input, output path, and confirmation before credentials are loaded.
    pub fn preflight(
        &self,
        token_source: crate::credentials::CredentialSource,
    ) -> Result<(), Error> {
        match &self.command {
            Command::CheckAccount { email } => {
                administration::validate_email(email)?;
            }
            Command::Add { email, role, .. } => {
                administration::validate_email(email)?;
                administration::validate_role(role)?;
            }
            Command::Remove { selector, .. } => {
                administration::validate_selector(selector)?;
            }
            Command::ChangeRole { selector, role, .. } => {
                administration::validate_selector(selector)?;
                administration::validate_role(role)?;
            }
            Command::MaintenanceMode { command } => {
                if let MaintenanceModeCommand::Set { options, .. } = command {
                    options.preflight(token_source)?;
                }
            }
            Command::SupportToken { output, options } => {
                administration::validate_support_token_output(output)?;
                options.preflight(token_source)?;
            }
            Command::List | Command::Permissions => {}
        }
        match &self.command {
            Command::Add { options, .. }
            | Command::Remove { options, .. }
            | Command::ChangeRole { options, .. } => options.preflight(token_source),
            _ => Ok(()),
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
            let body = administration::get(&api, &site).await?;
            Ok(CommandResult::success(if context.format == Format::Table {
                administrator_table_data(
                    body.get("accounts")
                        .and_then(Value::as_array)
                        .map(Vec::as_slice)
                        .unwrap_or_default(),
                )
            } else {
                body
            }))
        }
        Command::Permissions => Ok(CommandResult::success(serde_json::to_value(
            administration::permissions(&api, &site).await?,
        )?)),
        Command::CheckAccount { email } => Ok(CommandResult::success(serde_json::json!({
            "email": email,
            "exists": administration::check_account(&api, &site, &email).await?,
        }))),
        Command::Add {
            email,
            role,
            options,
        } => {
            let (backend, plan) = administration::add(&api, &site, email.clone(), role).await?;
            mutation::execute(
                &backend,
                plan,
                "admin.add",
                serde_json::json!({"site_id":site,"email":email}),
                options,
                &context,
            )
            .await
        }
        Command::Remove { selector, options } => {
            let (backend, plan) = administration::remove(&api, &site, selector.clone()).await?;
            mutation::execute(
                &backend,
                plan,
                "admin.remove",
                serde_json::json!({"site_id":site,"selector":selector}),
                options,
                &context,
            )
            .await
        }
        Command::ChangeRole {
            selector,
            role,
            options,
        } => {
            let (backend, plan) =
                administration::change_role(&api, &site, selector.clone(), role).await?;
            mutation::execute(
                &backend,
                plan,
                "admin.change-role",
                serde_json::json!({"site_id":site,"selector":selector}),
                options,
                &context,
            )
            .await
        }
        Command::MaintenanceMode { command } => match command {
            MaintenanceModeCommand::Get => {
                let settings = administration::get(&api, &site).await?;
                Ok(CommandResult::success(serde_json::json!({
                    "isMaintenanceMode": settings.get("isMaintenanceMode").cloned().unwrap_or(Value::Null),
                })))
            }
            MaintenanceModeCommand::Set { enabled, options } => {
                let (backend, plan) = administration::maintenance(&api, &site, enabled).await?;
                mutation::execute(
                    &backend,
                    plan,
                    "admin.maintenance-mode.set",
                    serde_json::json!({"site_id":site}),
                    options,
                    &context,
                )
                .await
            }
        },
        Command::SupportToken { output, options } => {
            let (backend, plan) = administration::support_token(&api, &site).await?;
            let mut result = mutation::execute(
                &backend,
                plan,
                "admin.support-token",
                serde_json::json!({"site_id":site}),
                options,
                &context,
            )
            .await?;
            if result.status == ExitStatus::Success
                && options.apply
                && result.data.get("request_attempted") == Some(&Value::Bool(true))
            {
                backend.write_verified_token(&output)?;
                result
                    .data
                    .as_object_mut()
                    .ok_or_else(|| anyhow::anyhow!("support-token result must be a JSON object"))?
                    .insert(
                        "output_path".into(),
                        Value::String(output.display().to_string()),
                    );
            }
            Ok(result)
        }
    }
}

fn administrator_table_data(rows: &[Value]) -> Value {
    const FIELDS: &[&str] = &[
        "userId",
        "email",
        "roleOnSite",
        "isActivated",
        "isCurrentUser",
        "isMfaEnabled",
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
    use clap::Parser;

    use super::*;

    #[test]
    fn maintenance_boolean_requires_an_explicit_value() {
        for (value, expected) in [("true", true), ("false", false)] {
            let cli = crate::cli::Cli::try_parse_from([
                "instantctl",
                "admin",
                "maintenance-mode",
                "set",
                value,
            ])
            .unwrap();
            let crate::cli::Command::Admin(args) = cli.command else {
                panic!("admin command should be selected");
            };
            let Command::MaintenanceMode {
                command: MaintenanceModeCommand::Set { enabled, options },
            } = args.command
            else {
                panic!("maintenance-mode set should be selected");
            };
            assert_eq!(enabled, expected);
            assert!(!options.apply);
            assert!(!options.yes);
        }
    }

    #[test]
    fn admin_surface_does_not_expose_lock_or_unlock() {
        for argv in [
            &["instantctl", "admin", "list"][..],
            &["instantctl", "admin", "permissions"][..],
            &[
                "instantctl",
                "admin",
                "check-account",
                "person@example.test",
            ][..],
            &["instantctl", "admin", "maintenance-mode", "get"][..],
        ] {
            crate::cli::Cli::try_parse_from(argv).expect("supported command should parse");
        }
        for action in ["lock", "unlock"] {
            assert!(crate::cli::Cli::try_parse_from(["instantctl", "admin", action]).is_err());
        }
    }

    #[test]
    fn administrator_table_projection_keeps_known_fields_and_nulls_missing_values() {
        let data = administrator_table_data(&[serde_json::json!({
            "userId": "user-1",
            "email": "person@example.test",
            "roleOnSite": "administrator",
            "isActivated": true,
            "vendorField": "preserve-only-in-raw-api-data"
        })]);
        assert_eq!(
            data,
            serde_json::json!([{
                "userId": "user-1",
                "email": "person@example.test",
                "roleOnSite": "administrator",
                "isActivated": true,
                "isCurrentUser": null,
                "isMfaEnabled": null
            }])
        );
        assert!(data[0].get("vendorField").is_none());
    }
}
