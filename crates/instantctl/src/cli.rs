use clap::{ArgAction, Parser, Subcommand};

use crate::{commands, output::Format};

#[derive(Debug, Parser)]
#[command(name = "instantctl", version = crate::version::DISPLAY, propagate_version = true)]
#[command(about = "Manage HPE Instant On networks")]
pub struct Cli {
    /// Output format (default: table on a terminal, JSON when piped).
    #[arg(long, global = true, value_enum)]
    pub format: Option<Format>,
    /// Read the bearer token from stdin instead of the environment or saved profile.
    #[arg(long, global = true)]
    pub token_stdin: bool,
    /// Site identifier.
    #[arg(long, global = true)]
    pub site: Option<String>,
    /// Credential profile (default: default).
    #[arg(long, global = true)]
    pub profile: Option<String>,
    /// Request timeout in seconds.
    #[arg(long, global = true, default_value_t = 15, value_parser = clap::value_parser!(u64).range(1..))]
    pub timeout: u64,
    /// Increase diagnostic verbosity.
    #[arg(short = 'v', long, global = true, action = ArgAction::Count)]
    pub verbose: u8,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Sign in and inspect credentials.
    Auth(commands::auth::Args),
    /// Inspect and change saved profile settings.
    Profile(commands::profile::Args),
    /// Call a portal API route.
    Api(commands::api::Args),
    /// List and inspect sites.
    Site(commands::site::Args),
    /// List and inspect switch stacks.
    Stack(commands::stack::Args),
    /// List and inspect network devices.
    Device(commands::device::Args),
    /// List and inspect connected clients.
    Client(commands::client::Args),
    /// List and inspect networks.
    Network(commands::network::Args),
    /// List and inspect wireless networks.
    Wlan(commands::wlan::Args),
    /// Inspect and configure the site guest portal.
    GuestPortal(commands::guest_portal::Args),
    /// Manage named schedules used by policies.
    Schedule(commands::schedule::Args),
    /// Manage RADIUS profiles with protected shared-secret input.
    Radius(commands::radius::Args),
    /// Configure site-wide RADIUS settings for port access control.
    PortAccessControl(commands::port_access_control::Args),
    /// Manage site administrators and support access.
    Admin(commands::admin::Args),
    /// List and inspect application and firewall policies.
    Policy(commands::policy::Args),
    /// Read and schedule firmware updates.
    Firmware(commands::firmware::Args),
    /// List and inspect switch ports.
    Port(commands::port::Args),
    /// List link aggregation groups.
    Lag(commands::lag::Args),
    /// List access point radios.
    Radio(commands::radio::Args),
    #[command(flatten)]
    Event(commands::event::Args),
    /// Inspect site health, topology, and historical traffic.
    Monitor(commands::monitor::Args),
    /// Generate shell completion definitions.
    #[command(visible_alias = "completions")]
    Completion(commands::completion::Args),
    /// Show the installed CLI version.
    Version,
}
