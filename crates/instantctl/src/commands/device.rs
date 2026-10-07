use super::site::read;
use crate::context::{CommandContext, CommandResult};
use clap::{Args as ClapArgs, Subcommand};
use instantctl_api::client::reads::{Device, local_health};
use instantctl_api::{Error, ErrorKind};
use serde_json::Value;

#[derive(Debug, ClapArgs)]
pub struct Args {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// List network devices.
    #[command(alias = "ls")]
    List,
    /// Show a device by MAC address or exact unique name.
    Show { id: String },
    /// Read local AP health (IP address, or a device MAC/exact name).
    Health {
        /// AP selector or IP address. Use --host for a DNS hostname.
        ap_or_host: String,
        /// Treat the argument as a DNS hostname or IP, without cloud credentials.
        #[arg(long)]
        host: bool,
    },
    /// Locate a device by activating or deactivating its locator LED.
    Locate(super::device_locate::Args),
    /// Rename a device while preserving its other configuration.
    Rename(super::device_config::RenameArgs),
    /// Set the device LED to on or quiet mode.
    Led(super::device_config::LedArgs),
    /// Configure a static management IPv4 address.
    ManagementIp(super::device_config::ManagementIpArgs),
    /// Request one reboot and verify that a restart occurred.
    Reboot(super::device_actions::RebootArgs),
    /// Remove a non-gateway device from the site.
    Forget(super::device_actions::ForgetArgs),
    /// Show device power usage.
    PowerUsage(super::device_reads::ReadArgs),
    /// Show detailed device telemetry.
    Details(super::device_reads::ReadArgs),
    /// Add a DHCP reservation after checking site scope and conflicts.
    ReserveIp(super::device_reserve::ReserveArgs),
    /// Remove the device's DHCP reservation.
    RemoveIpReservation(super::device_reserve::RemoveArgs),
    /// List potential replacement devices without changing hardware.
    ReplacementCandidates(super::device_replace::ReadArgs),
    /// Read or edit a wired port or trunk's client allowlist.
    Allowlist(super::allowlist::WiredArgs),
}

impl Args {
    pub(crate) fn preflight(&self, context: &CommandContext) -> Result<(), Error> {
        let options = match &self.command {
            Command::Allowlist(args) => {
                return super::allowlist::preflight_wired(args, context.token_source);
            }
            Command::Locate(args) => Some(args.options),
            Command::Rename(args) => Some(args.options),
            Command::Led(args) => Some(args.options),
            Command::ManagementIp(args) => Some(args.options),
            Command::Reboot(args) => Some(args.options),
            Command::Forget(args) => Some(args.options),
            Command::ReserveIp(args) => Some(args.options),
            Command::RemoveIpReservation(args) => Some(args.options),
            _ => None,
        };
        if let Some(options) = options {
            options.preflight(context.token_source)?;
        }
        Ok(())
    }
}

pub async fn run(args: Args, context: &CommandContext) -> anyhow::Result<CommandResult> {
    args.preflight(context)?;
    read::check_site(context)?;
    match args.command {
        Command::Locate(args) => super::device_locate::run(args, context).await,
        Command::Rename(args) => super::device_config::run_rename(args, context).await,
        Command::Led(args) => super::device_config::run_led(args, context).await,
        Command::ManagementIp(args) => super::device_config::run_management_ip(args, context).await,
        Command::Reboot(args) => super::device_actions::run_reboot(args, context).await,
        Command::Forget(args) => super::device_actions::run_forget(args, context).await,
        Command::PowerUsage(args) => super::device_reads::run_power_usage(args, context).await,
        Command::Details(args) => super::device_reads::run_details(args, context).await,
        Command::ReserveIp(args) => super::device_reserve::reserve(args, context).await,
        Command::RemoveIpReservation(args) => super::device_reserve::remove(args, context).await,
        Command::ReplacementCandidates(args) => super::device_replace::run(args, context).await,
        Command::Allowlist(args) => super::allowlist::run_wired(args, context).await,
        Command::Health { ap_or_host, host } => {
            let target = if host || ap_or_host.parse::<std::net::IpAddr>().is_ok() {
                local_health::validate_host(&ap_or_host)?
            } else {
                health_host(
                    &ap_or_host,
                    &read::inventory(&read::client(context)?, context).await?,
                )?
            };
            let health = local_health::get(&target, context.timeout).await?;
            Ok(CommandResult {
                status: if health.is_complete() {
                    crate::exit::ExitStatus::Success
                } else {
                    crate::exit::ExitStatus::Error(ErrorKind::Unverified)
                },
                data: serde_json::to_value(health)?,
            })
        }
        command => render(
            Args { command },
            &read::inventory(&read::client(context)?, context).await?,
        ),
    }
}

fn render(args: Args, models: &[Device]) -> anyhow::Result<CommandResult> {
    let devices = read::values(models)?;
    let data = match args.command {
        Command::List => Value::Array(devices.iter().map(summary).collect()),
        Command::Show { id } => {
            let device = read::select(&devices, &id, "device")?;
            let mut result = summary(device);
            for (key, source) in [
                ("id", "id"),
                ("device_type", "deviceType"),
                ("health", "health"),
                ("sku", "sku"),
                ("capabilities", "capabilities"),
                ("ethernet_ports", "ethernetPorts"),
                ("trunk_ports", "trunkPorts"),
                ("radios", "radios"),
            ] {
                result[key] = device.get(source).cloned().unwrap_or(Value::Null);
            }
            result
        }
        // Public run owns health and locate delegation.
        _ => return Err(Error::new(ErrorKind::Usage, "expected device list or show").into()),
    };
    Ok(CommandResult::success(data))
}

fn summary(device: &Value) -> Value {
    let mut row = read::project(
        device,
        &[
            ("name", "name"),
            ("model", "model"),
            ("mac", "macAddress"),
            ("ip", "ipAddress"),
            ("state", "status"),
            ("firmware", "deviceSoftwareVersion"),
        ],
    );
    if device.get("model").is_none() {
        row["model"] = device.get("deviceModel").cloned().unwrap_or(Value::Null);
    }
    row
}

fn health_host(selector: &str, models: &[Device]) -> Result<String, Error> {
    let devices = read::values(models)?;
    let device = read::select(&devices, selector, "device")?;
    // The observed radios collection identifies an AP without guessing deviceType enums.
    read::array(device, "radios")?;
    let host = device
        .get("ipAddress")
        .and_then(Value::as_str)
        .ok_or_else(|| read::incomplete("selected AP has no reported IP address"))?;
    local_health::validate_host(host)
}

#[cfg(test)]
mod tests {
    use super::*;
    use read::tests::{MAC, context, device, devices, error_kind};
    use serde_json::json;
    #[test]
    fn list_and_show_copy_rvben_columns_and_select_exact_mac_or_name() {
        let models = devices(vec![
            json!({"id":MAC,"macAddress":MAC,"name":"AP","deviceModel":"AP22",
            "ipAddress":"192.0.2.1","status":"up","deviceSoftwareVersion":"3.4"}),
        ]);
        let list = render(
            Args {
                command: Command::List,
            },
            &models,
        )
        .unwrap();
        assert_eq!(
            list.data[0],
            json!({"name":"AP","model":"AP22","mac":MAC,"ip":"192.0.2.1","state":"up","firmware":"3.4"})
        );
        assert_eq!(
            list.data[0]
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["name", "model", "mac", "ip", "state", "firmware"]
        );
        for id in ["AP", "AA:BB:CC:DD:EE:FF"] {
            let shown = render(
                Args {
                    command: Command::Show { id: id.into() },
                },
                &models,
            )
            .unwrap();
            assert_eq!(shown.data["mac"], MAC);
            assert!(shown.data["health"].is_null());
            assert!(shown.data["radios"].is_null());
        }
    }
    #[test]
    fn missing_device_telemetry_stays_null() {
        let result = render(
            Args {
                command: Command::List,
            },
            &devices(vec![device("Switch")]),
        )
        .unwrap();
        for key in ["model", "ip", "state", "firmware"] {
            assert!(result.data[0][key].is_null(), "{key}");
        }
    }
    #[test]
    fn ambiguity_is_usage_and_not_found_uses_exact_names() {
        let mut second = device("AP");
        second["id"] = json!("11:22:33:44:55:66");
        second["macAddress"] = second["id"].clone();
        let models = devices(vec![device("AP"), second]);
        for (id, kind) in [
            ("AP", ErrorKind::Usage),
            ("A", ErrorKind::NotFound),
            ("de:ad:be:ef:00:01", ErrorKind::NotFound),
        ] {
            let error = render(
                Args {
                    command: Command::Show { id: id.into() },
                },
                &models,
            )
            .err()
            .unwrap();
            assert_eq!(error_kind(&error), kind);
            assert_eq!(
                crate::exit::ExitStatus::Error(kind).code(),
                if kind == ErrorKind::Usage { 2 } else { 4 }
            );
        }
    }
    #[test]
    fn health_resolves_an_exact_ap_and_requires_a_reported_host() {
        let models = devices(vec![
            json!({"id":MAC,"macAddress":MAC,"name":"AP","radios":[],"ipAddress":"192.0.2.1"}),
        ]);
        assert_eq!(health_host("AP", &models).unwrap(), "192.0.2.1");
        assert_eq!(
            health_host("Other", &models).unwrap_err().kind,
            ErrorKind::NotFound
        );
        let models = devices(vec![
            json!({"id":MAC,"macAddress":MAC,"name":"AP","radios":[]}),
        ]);
        assert_eq!(
            health_host("AP", &models).unwrap_err().kind,
            ErrorKind::Unverified
        );
    }
    #[tokio::test]
    async fn invalid_health_host_is_refused_before_credentials_and_redacts_secrets() {
        let credential = "credential-value-7f8d923a";
        let error = run(
            Args {
                command: Command::Health {
                    ap_or_host: format!("user:{credential}@ap"),
                    host: true,
                },
            },
            &context(None),
        )
        .await
        .err()
        .unwrap();
        assert_eq!(error_kind(&error), ErrorKind::Config);
        assert!(!format!("{error:?} {error}").contains(credential));
    }
}
