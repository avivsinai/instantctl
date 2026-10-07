mod access_common;
pub mod admin;
pub mod allowlist;
pub mod api;
pub mod auth;
mod auth_input;
pub mod client;
pub mod completion;
pub mod device;
pub mod device_actions;
pub mod device_config;
pub mod device_locate;
pub mod device_reads;
pub mod device_replace;
pub mod device_reserve;
pub mod event;
pub mod firmware;
pub mod guest_portal;
pub mod lag;
pub mod monitor;
pub mod network;
pub mod network_routing;
pub mod policy;
pub mod port;
pub mod port_access_control;
pub mod port_actions;
pub mod port_settings;
pub mod profile;
pub mod radio;
pub mod radius;
pub mod schedule;
pub mod site;
pub mod site_lifecycle;
pub mod site_stp_auto;
pub mod stack;
pub mod wlan;

use std::io::Write;

use crate::{
    cli::Command,
    context::{CommandContext, CommandResult},
    exit::ExitStatus,
    output,
};

pub async fn run(
    command: Command,
    context: &CommandContext,
    out: &mut impl Write,
) -> anyhow::Result<ExitStatus> {
    match &command {
        Command::Device(args) => args.preflight(context)?,
        Command::Site(args) => args.preflight(context)?,
        Command::Port(args) => args.preflight(context)?,
        Command::Lag(args) => args.preflight(context)?,
        Command::Profile(args) => args.preflight(context)?,
        Command::Policy(args) => args.preflight(context.token_source)?,
        Command::Firmware(args) => args.preflight(context)?,
        Command::Admin(args) => args.preflight(context.token_source)?,
        Command::Network(args) => {
            if let network::Command::Routing(args) = &args.command {
                args.preflight(context)?;
            }
        }
        Command::Wlan(args) => {
            if let wlan::Command::Allowlist(args) = &args.command {
                allowlist::preflight_wireless(args, context.token_source)?;
            }
        }
        _ => {}
    }
    let resolved;
    let context = if uses_selected_site(&command) && context.token_source.uses_profile() {
        site::read::check_site(context)?;
        resolved = context.resolve_profile_default().await?;
        &resolved
    } else {
        context
    };
    let result = match command {
        Command::Auth(args) => auth::run(args, context).await?,
        Command::Profile(args) => profile::run(args, context).await?,
        Command::Api(args) => api::run(args, context).await?,
        Command::Site(args) => site::run(args, context).await?,
        Command::Stack(args) => stack::run(args, context).await?,
        Command::Device(args) => device::run(args, context).await?,
        Command::Client(args) => client::run(args, context).await?,
        Command::Network(args) => network::run(args, context).await?,
        Command::Wlan(args) => wlan::run(args, context).await?,
        Command::GuestPortal(args) => guest_portal::run(args, context).await?,
        Command::Schedule(args) => schedule::run(args, context).await?,
        Command::Radius(args) => radius::run(args, context).await?,
        Command::PortAccessControl(args) => port_access_control::run(args, context).await?,
        Command::Policy(args) => policy::run(args, context).await?,
        Command::Firmware(args) => firmware::run(args, context).await?,
        Command::Admin(args) => admin::run(args, context).await?,
        Command::Port(args) => port::run(args, context).await?,
        Command::Lag(args) => lag::run(args, context).await?,
        Command::Radio(args) => radio::run(args, context).await?,
        Command::Event(args) => return event::run(args, context, out).await,
        Command::Monitor(args) => monitor::run(args, context).await?,
        Command::Completion(args) => {
            completion::run(args, out)?;
            return Ok(ExitStatus::Success);
        }
        Command::Version => CommandResult::success(serde_json::json!({
            "name": "instantctl",
            "version": env!("CARGO_PKG_VERSION"),
            "git_sha": crate::version::GIT_SHA,
        })),
    };
    output::write_data(out, context.format, &result.data)?;
    Ok(result.status)
}

fn uses_selected_site(command: &Command) -> bool {
    match command {
        Command::Device(args) => !matches!(&args.command,
            device::Command::Health { ap_or_host, host }
                if *host || ap_or_host.parse::<std::net::IpAddr>().is_ok()),
        Command::Client(_)
        | Command::Port(_)
        | Command::Lag(_)
        | Command::Radio(_)
        | Command::Policy(_)
        | Command::Firmware(_)
        | Command::Admin(_)
        | Command::Event(_)
        | Command::Monitor(_) => true,
        Command::Stack(_) => true,
        Command::Site(args) => !matches!(
            args.command,
            site::Command::List
                | site::Command::Show { id: Some(_) }
                | site::Command::Create(_)
                | site::Command::Rename(_)
                | site::Command::Delete(_)
                | site::Command::Clone(_)
                | site::Command::Country
        ),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn profile_site_defaults_cover_cloud_consumers_and_skip_local_commands() {
        for (expected, commands) in [
            (
                true,
                vec![
                    vec!["device", "list"],
                    vec!["device", "health", "AP"],
                    vec!["device", "locate", "AP", "on"],
                    vec!["device", "rename", "AP", "New name"],
                    vec!["device", "led", "AP", "quiet"],
                    vec!["device", "reboot", "AP"],
                    vec!["device", "forget", "AP"],
                    vec!["device", "details", "AP"],
                    vec!["device", "power-usage", "AP"],
                    vec![
                        "device",
                        "management-ip",
                        "AP",
                        "192.0.2.10",
                        "--prefix-length",
                        "24",
                        "--gateway",
                        "192.0.2.1",
                        "--dns",
                        "192.0.2.53",
                    ],
                    vec!["client", "list"],
                    vec!["policy", "list"],
                    vec!["policy", "show", "policy-1"],
                    vec!["policy", "app-visibility", "get"],
                    vec!["policy", "app-visibility", "set", "false"],
                    vec!["firmware", "window", "get"],
                    vec!["firmware", "window", "set", "--day", "monday"],
                    vec!["firmware", "update-now"],
                    vec!["firmware", "schedule", "2026-11-02T03:00:00"],
                    vec!["admin", "list"],
                    vec!["admin", "permissions"],
                    vec!["admin", "check-account", "person@example.test"],
                    vec!["admin", "maintenance-mode", "get"],
                    vec!["admin", "maintenance-mode", "set", "true"],
                    vec!["port", "list"],
                    vec!["lag", "list"],
                    vec!["stack", "list"],
                    vec!["stack", "show", "Core"],
                    vec!["radio", "list"],
                    vec!["event", "list"],
                    vec!["alert", "list"],
                    vec!["monitor", "health"],
                    vec!["monitor", "dashboard"],
                    vec!["monitor", "topology"],
                    vec!["monitor", "app-usage"],
                    vec!["monitor", "client-usage"],
                    vec!["monitor", "threats"],
                    vec!["site", "show"],
                    vec!["site", "capabilities"],
                    vec!["site", "health"],
                    vec!["site", "dashboard"],
                    vec!["site", "topology"],
                    vec!["site", "timezone"],
                    vec!["site", "timezone", "Europe/Berlin"],
                    vec!["site", "management-network"],
                    vec!["site", "management-network", "--vlan", "42"],
                    vec!["site", "dns"],
                    vec!["site", "dns", "--mode", "custom", "--primary", "192.0.2.53"],
                    vec!["site", "spanning-tree"],
                    vec!["site", "spanning-tree", "--rstp", "true"],
                ],
            ),
            (
                false,
                vec![
                    vec!["device", "health", "192.0.2.1"],
                    vec!["device", "health", "ap.example.test", "--host"],
                    vec!["site", "list"],
                    vec!["site", "show", "Home"],
                    vec!["site", "country"],
                    vec![
                        "site",
                        "create",
                        "New",
                        "--country",
                        "IL",
                        "--timezone",
                        "Europe/Berlin",
                    ],
                    vec![
                        "site",
                        "clone",
                        "Home",
                        "New",
                        "--country",
                        "IL",
                        "--timezone",
                        "Europe/Berlin",
                    ],
                    vec!["site", "rename", "Home", "New"],
                    vec!["site", "delete", "Home"],
                    vec!["auth", "status"],
                    vec!["api", "/sites"],
                    vec!["version"],
                    vec!["completion", "zsh"],
                    vec!["network", "list"],
                    vec!["wlan", "list"],
                ],
            ),
        ] {
            for arguments in commands {
                let mut argv = vec!["instantctl"];
                argv.extend(arguments);
                let cli = crate::cli::Cli::try_parse_from(&argv).unwrap();
                assert_eq!(uses_selected_site(&cli.command), expected, "{argv:?}");
            }
        }
    }
}
