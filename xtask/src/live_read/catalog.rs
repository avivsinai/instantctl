//! Exact read recipes for the live-read command sweep.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Entry {
    pub path: &'static str,
    pub action: Action,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Action {
    /// Safe suffix arguments after `path`; mixed leaves use only their read form.
    Read(&'static [Arg]),
    /// A mutating or deliberately omitted utility leaf, with a specific reason.
    Exclude(&'static str),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Arg {
    Literal(&'static str),
    First {
        source: &'static str,
        field: &'static str,
        filter: Filter,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Filter {
    Any,
    PositivePort,
    WiredAllowlistEligiblePort,
}

const fn read(path: &'static str, args: &'static [Arg]) -> Entry {
    Entry {
        path,
        action: Action::Read(args),
    }
}

const fn exclude(path: &'static str, reason: &'static str) -> Entry {
    Entry {
        path,
        action: Action::Exclude(reason),
    }
}

const fn first(source: &'static str, field: &'static str) -> Arg {
    Arg::First {
        source,
        field,
        filter: Filter::Any,
    }
}

const fn first_positive_port(field: &'static str) -> Arg {
    Arg::First {
        source: "port list",
        field,
        filter: Filter::PositivePort,
    }
}

const fn first_allowlist_port(field: &'static str) -> Arg {
    Arg::First {
        source: "port list",
        field,
        filter: Filter::WiredAllowlistEligiblePort,
    }
}

pub const ENTRIES: &[Entry] = &[
    exclude("auth login", "SSO login and saved-credential write"),
    read("auth status", &[]),
    exclude(
        "auth logout",
        "credential deletion and refresh-token revocation",
    ),
    read("profile show", &[]),
    exclude(
        "profile default-site set",
        "writes saved-profile site selection",
    ),
    read("profile protected-ports list", &[]),
    exclude(
        "profile protected-ports add",
        "writes saved-profile protected-port list",
    ),
    exclude(
        "profile protected-ports remove",
        "writes saved-profile protected-port list",
    ),
    read(
        "api",
        &[
            Arg::Literal("/sites"),
            Arg::Literal("-X"),
            Arg::Literal("GET"),
        ],
    ),
    exclude(
        "site create",
        "creates a site when applied; default output is a mutation plan",
    ),
    exclude(
        "site rename",
        "renames a site when applied; default output is a mutation plan",
    ),
    exclude(
        "site delete",
        "deletes a site when applied; requires exact-name confirmation",
    ),
    exclude(
        "site clone",
        "creates a cloned site when applied; default output is a mutation plan",
    ),
    read("site country", &[]),
    read("site list", &[]),
    read("site show", &[]),
    read("site capabilities", &[]),
    read("site health", &[]),
    read("site dashboard", &[]),
    read("site topology", &[]),
    read("site timezone", &[]),
    read("site management-network", &[]),
    read("site dns", &[]),
    read("site spanning-tree", &[]),
    read("site extend-network", &[]),
    exclude(
        "site stp-auto-priority",
        "requests a bridge-priority action when applied; completion is unverified",
    ),
    read("stack list", &[]),
    read("stack show", &[first("stack list", "id")]),
    read("device list", &[]),
    read("device show", &[first("device list", "mac")]),
    // Select an AP name from live radio rows; address-shaped names are refused.
    // Duplicate names are left for the CLI's normal ambiguity error.
    read("device health", &[first("radio list", "device")]),
    exclude(
        "device locate",
        "activates or deactivates a device locator LED",
    ),
    exclude("device rename", "renames a device configuration"),
    exclude("device led", "changes a device LED state"),
    exclude(
        "device management-ip",
        "changes device management addressing",
    ),
    exclude("device reboot", "starts a device reboot"),
    exclude("device forget", "removes a device from the site"),
    read("device power-usage", &[first_positive_port("Device")]),
    read("device details", &[first("device list", "mac")]),
    exclude("device reserve-ip", "creates a DHCP reservation"),
    exclude("device remove-ip-reservation", "removes a DHCP reservation"),
    read(
        "device replacement-candidates",
        &[first("device list", "mac")],
    ),
    read(
        "device allowlist",
        &[
            first_allowlist_port("Device"),
            Arg::Literal("--port"),
            first_allowlist_port("Port"),
        ],
    ),
    read("client list", &[]),
    read("client show", &[first("client list", "mac")]),
    exclude("client rename", "renames a client"),
    exclude("client block", "blocks a client"),
    exclude("client unblock", "unblocks a client"),
    exclude("client reserve-ip", "creates a client DHCP reservation"),
    exclude("client watchlist add", "adds a client to the watchlist"),
    exclude(
        "client watchlist remove",
        "removes a client from the watchlist",
    ),
    exclude("client power-cycle", "starts a client power cycle"),
    read("client tags list", &[first("client list", "mac")]),
    exclude("client tags set", "replaces client tags"),
    exclude("client tags add", "adds client tags"),
    exclude("client tags remove", "removes client tags"),
    read("client where-is", &[first("client list", "mac")]),
    read("network list", &[]),
    read("network show", &[first("network list", "id")]),
    read("network routing", &[first("network list", "id")]),
    exclude("network create", "creates a wired network"),
    exclude("network update", "updates a wired network"),
    exclude("network delete", "deletes a wired network"),
    exclude("network dhcp", "updates wired-network DHCP configuration"),
    read("network shared-services status", &[]),
    exclude(
        "network shared-services enable",
        "enables site shared services",
    ),
    exclude(
        "network shared-services disable",
        "disables site shared services",
    ),
    read(
        "network shared-services list",
        &[first("network list", "id")],
    ),
    exclude(
        "network shared-services share",
        "shares a service group with a network",
    ),
    exclude(
        "network shared-services unshare",
        "stops sharing a service group",
    ),
    read("wlan list", &[]),
    read("wlan show", &[first("wlan list", "id")]),
    read("wlan allowlist", &[first("wlan list", "id")]),
    exclude("wlan create", "creates a wireless network"),
    exclude("wlan update", "updates a wireless network"),
    exclude("wlan delete", "deletes a wireless network"),
    exclude("wlan enable", "enables a wireless network"),
    exclude("wlan disable", "disables a wireless network"),
    exclude("wlan passphrase", "changes the wireless passphrase"),
    exclude("wlan bands", "changes wireless radio bands"),
    exclude("wlan schedule", "changes a wireless schedule"),
    exclude("wlan bandwidth", "changes a wireless bandwidth limit"),
    exclude("wlan guest", "changes wireless guest classification"),
    exclude("wlan access", "changes wireless client access rules"),
    read("guest-portal show", &[]),
    exclude("guest-portal update", "updates guest-portal settings"),
    read("schedule list", &[]),
    read("schedule show", &[first("schedule list", "id")]),
    exclude("schedule create", "creates a schedule"),
    exclude("schedule update", "updates a schedule"),
    exclude("schedule delete", "deletes a schedule"),
    read("radius list", &[]),
    read("radius show", &[first("radius list", "id")]),
    exclude("radius create", "creates a RADIUS profile"),
    exclude("radius update", "updates a RADIUS profile"),
    exclude("radius delete", "deletes a RADIUS profile"),
    read("port-access-control show", &[]),
    exclude(
        "port-access-control update",
        "updates site RADIUS access-control settings",
    ),
    read("policy list", &[]),
    read("policy show", &[first("policy list", "id")]),
    read("policy app-visibility get", &[]),
    exclude(
        "policy app-visibility set",
        "changes application visibility",
    ),
    read("port list", &[]),
    read(
        "port show",
        &[first_positive_port("Device"), first_positive_port("Port")],
    ),
    exclude("port set", "changes switch port configuration"),
    exclude("port power-cycle", "cycles power on an attached client"),
    read("port find", &[first("client list", "mac")]),
    exclude("port cable-test", "starts a cable diagnostic"),
    exclude(
        "port connectivity-test",
        "starts switch-originated connectivity diagnostics",
    ),
    exclude("port mirror", "changes switch port mirroring"),
    read("port profile list", &[]),
    read("port profile show", &[first("port profile list", "id")]),
    exclude("port profile create", "creates a port profile"),
    exclude("port profile set", "updates a port profile"),
    exclude("port profile remove", "removes a port profile"),
    read("port poe-schedule show", &[]),
    exclude("port poe-schedule set", "changes the site's PoE schedule"),
    read("port eee show", &[]),
    exclude("port eee set", "changes Energy Efficient Ethernet"),
    read("lag list", &[]),
    exclude(
        "lag create",
        "creates or configures a link aggregation group",
    ),
    exclude("lag remove", "removes a link aggregation group"),
    read("radio list", &[]),
    read("radio plan get", &[]),
    exclude("radio plan set", "changes the site radio plan"),
    read("radio override get", &[first("radio list", "device")]),
    exclude("radio override set", "changes a device radio override"),
    read("event list", &[Arg::Literal("--since"), Arg::Literal("1h")]),
    read("alert list", &[]),
    read("monitor health", &[]),
    read("monitor dashboard", &[]),
    read("monitor topology", &[]),
    read("monitor app-usage", &[]),
    read("monitor client-usage", &[]),
    read("monitor threats", &[]),
    exclude(
        "completion",
        "local output-generation utility, not a portal read",
    ),
    read("version", &[]),
    // Planned .17 leaves are kept in the catalog so an older installed binary records its parser failure.
    read("firmware window get", &[]),
    exclude("firmware window set", "sets the firmware update window"),
    exclude("firmware update-now", "starts a firmware update"),
    exclude("firmware schedule", "schedules a firmware update"),
    // Planned .18 administration leaves are kept for the same installed-binary compatibility check.
    read("admin list", &[]),
    read("admin permissions", &[]),
    read(
        "admin check-account",
        &[first("admin list", "/accounts/0/email")],
    ),
    exclude("admin add", "adds a site administrator"),
    exclude("admin remove", "removes a site administrator"),
    exclude("admin change-role", "changes an administrator's role"),
    read("admin maintenance-mode get", &[]),
    exclude(
        "admin maintenance-mode set",
        "changes site maintenance mode",
    ),
    exclude("admin support-token", "requests and writes a support token"),
];
