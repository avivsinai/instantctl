use clap::{Args as ClapArgs, Subcommand};
use serde::Serialize;
use serde_json::{Value, json};

use super::site::read;
use crate::context::{CommandContext, CommandResult};

#[derive(Debug, ClapArgs)]
pub struct Args {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Show current and historical site health as reported.
    Health,
    /// Show the site dashboard and landing-page counters.
    Dashboard,
    /// Show the portal's topology graph, including unknown node/link state.
    Topology,
    /// Show application visibility configuration and traffic over the last 24 hours.
    AppUsage,
    /// Show client traffic over the last 24 hours (historical, not current activity).
    ClientUsage {
        #[arg(long, default_value = "allNetworks")]
        network: String,
        #[arg(long, default_value = "allAppCategories")]
        app_category: String,
    },
    /// List reported security threat events.
    Threats,
}

pub async fn run(args: Args, context: &CommandContext) -> anyhow::Result<CommandResult> {
    let api = read::client(context)?;
    let site = read::site_id(&api, context).await?;
    let data = match args.command {
        Command::Health => project(
            &api.monitoring_health(&site).await?,
            &[
                ("current_health", "currentHealth"),
                ("historical_healths", "historicalHealths"),
                ("sample_period_seconds", "samplePeriodSeconds"),
                ("history_duration_seconds", "historyDurationSeconds"),
            ],
        )?,
        Command::Dashboard => json!({
            "dashboard": api.monitoring_dashboard(&site).await?,
            "landing_page": api.landing_page(&site).await?,
        }),
        Command::Topology => serde_json::to_value(api.graph_topology(&site).await?)?,
        Command::AppUsage => json!({
            "configuration": api.application_configuration(&site).await?,
            "usage": api.application_usage(&site).await?,
        }),
        Command::ClientUsage {
            network,
            app_category,
        } => client_rows(&api.client_usage(&site, &network, &app_category).await?)?,
        Command::Threats => threat_rows(&api.security_threats(&site).await?)?,
    };
    Ok(CommandResult::success(data))
}

fn project(model: &impl Serialize, fields: &[(&str, &str)]) -> anyhow::Result<Value> {
    Ok(read::project(&serde_json::to_value(model)?, fields))
}

fn client_rows(
    models: &[instantctl_api::client::monitoring::ClientUsage],
) -> anyhow::Result<Value> {
    Ok(Value::Array(
        models
            .iter()
            .map(|model| {
                project(
                    model,
                    &[
                        ("client_id", "clientId"),
                        ("client_name", "clientName"),
                        ("client_currently_active", "clientCurrentlyActive"),
                        (
                            "bytes_last_24_hours",
                            "dataTransferredDuringLast24HoursInBytes",
                        ),
                        ("application_category", "applicationCategory"),
                    ],
                )
            })
            .collect::<anyhow::Result<Vec<_>>>()?,
    ))
}

fn threat_rows(models: &[instantctl_api::client::monitoring::Threat]) -> anyhow::Result<Value> {
    Ok(Value::Array(
        models
            .iter()
            .map(|model| {
                project(
                    model,
                    &[
                        ("id", "id"),
                        ("description", "description"),
                        ("severity", "severity"),
                        ("classification", "classification"),
                        ("occur_date", "occurDate"),
                        ("state", "state"),
                        ("protocol", "protocol"),
                        ("source", "source"),
                        ("destination", "destination"),
                        ("signature_id", "signatureId"),
                        ("exception_creation_date", "exceptionCreationDate"),
                        ("causes", "causes"),
                        ("malware_family", "malwareFamily"),
                        ("cve", "cve"),
                        ("recommendations", "recommendations"),
                    ],
                )
            })
            .collect::<anyhow::Result<Vec<_>>>()?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use instantctl_api::client::monitoring::{ClientUsage, Health, Threat};

    #[test]
    fn missing_health_and_client_activity_are_null_not_healthy_or_inactive() {
        let health: Health =
            serde_json::from_value(json!({"historyDurationSeconds":3600})).unwrap();
        let row = project(
            &health,
            &[
                ("current", "currentHealth"),
                ("period", "samplePeriodSeconds"),
            ],
        )
        .unwrap();
        assert_eq!(row, json!({"current":null,"period":null}));
        let clients: Vec<ClientUsage> = serde_json::from_value(json!([
            {"clientId":"c1","dataTransferredDuringLast24HoursInBytes":42},
            {"clientId":"c2","clientCurrentlyActive":false},
            {"clientId":"c3","clientCurrentlyActive":true}
        ]))
        .unwrap();
        let rows = client_rows(&clients).unwrap();
        assert!(rows[0]["client_currently_active"].is_null());
        assert_eq!(rows[0]["bytes_last_24_hours"], 42);
        assert_eq!(rows[1]["client_currently_active"], false);
        assert_eq!(rows[2]["client_currently_active"], true);
        assert!(rows[1]["bytes_last_24_hours"].is_null());
    }

    #[test]
    fn threat_projection_keeps_unknown_fields_null_and_reported_severity() {
        let threats: Vec<Threat> = serde_json::from_value(json!([
            {"id":"t1","severity":"new-severity","occurDate":123,"source":{"ipAddress":"192.0.2.1"}}
        ]))
        .unwrap();
        let rows = threat_rows(&threats).unwrap();
        assert_eq!(rows[0]["severity"], "new-severity");
        assert_eq!(rows[0]["source"]["ipAddress"], "192.0.2.1");
        assert!(rows[0]["destination"].is_null());
        assert!(rows[0]["state"].is_null());
    }
}
