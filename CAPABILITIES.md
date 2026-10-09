# instantctl capability matrix

This matrix compares the UniFi core operations with the Instant On API surface and the Rust `instantctl` CLI shipped in this package. Audited native CLI source: `eade885a0b3e24c2cabbb53777ac09342b6c974d`; read-runner source: `1d5c986578e99e661308390f129bec386dd3e1ad`, 2026-10-07.

The command column uses the current `instantctl` name. The live receipts dated
2026-10-07 were collected with the earlier `hpe-network` executable name.

## Evidence rules

- `implemented` means a command exists in this source snapshot. It does not mean that every account or device supports the operation.
- `READ` and `WRITE` are tracked separately in each implemented row. A successful read does not prove a write. `mock-only` means the request and readback behavior has local tests but no live write evidence in the listed receipts.
- The fresh credentialed read run used the installed CLI from audited source `eade885` on 2026-10-07 and exited zero. The 71 catalog command cases produced 65 successful reads and six `no_live_subject` skips; two additional device-discovery reads ran while checking wired allow-list eligibility. That is 73 receipts total: 67 successes, six skips, and no failures. A skip is not a pass or proof of tenant-wide absence, and this run is not a complete tenant census.
- A fresh recheck on 2026-10-09 used the standalone CLI at source `a81bc6da4157778104b7c7d19644cca2deafdaf0`, but stopped at profile refresh with an authentication error before any site API calls. The saved profile was marked `refresh_pending`, so a fresh login was required before reuse. The historical CLI source `eade885a0b3e24c2cabbb53777ac09342b6c974d` and read-runner source `1d5c986578e99e661308390f129bec386dd3e1ad` are not present in this standalone repository, so the 2026-10-07 receipts remain historical evidence for those snapshots; this recheck refreshed no matrix rows.
- Live write receipts on 2026-10-07 verified device rename/restore, locator on/off, unused network create/delete, firmware maintenance-window set/restore, and disabled WLAN create/delete/absence. Other writes remain mock-tested unless a row says otherwise. No private account identifiers or target names are included here.
- `no route observed` means no matching route was present in the reviewed API map. It does not claim that the platform can never support the operation. Gateway-dependent routes are marked as such; this matrix does not expose site topology.
- Routes are relative to the fixed portal API base unless marked public. `site dns` uses the DNS object nested under `/sites/{s}/managementNetwork`; there is no separate `/dns` singleton in this implementation.

## Source legend

The bracketed `c1`–`c24` labels are local references to the 24 ranked UniFi core-operation groups in the baseline below. They are comparison labels, not vendor operation IDs. The baseline uses the [official UniFi Network API OpenAPI v10.6.106](https://developer.ui.com/network/v10.6.106/openapi.json) and, for classic-controller routes outside that specification, cross-checks sources such as the [Art-of-WiFi API client](https://github.com/Art-of-WiFi/UniFi-API-client/blob/master/src/Client.php) and [aiounifi](https://github.com/Kane610/aiounifi). These sources do not make classic routes part of the official API.

| ID | UniFi core group | ID | UniFi core group |
|---|---|---|---|
| c1 | Auth, profile, info/version, raw API | c13 | SSID operations |
| c2 | Device inventory and details | c14 | Port forwards |
| c3 | Client list, details, and location | c15 | Device adoption and removal |
| c4 | Restart device | c16 | Firewall policies and ACLs |
| c5 | PoE cycle and port discovery | c17 | Static DNS records |
| c6 | Site health | c18 | Guest vouchers |
| c7 | Events and alarms | c19 | Traffic matching lists/groups |
| c8 | Locate device | c20 | Static routes, DDNS, and VPN |
| c9 | Client block/kick and guest authorization | c21 | Device stats, usage, reports, and DPI |
| c10 | Client names and DHCP reservations | c22 | Backups, settings, and admin list |
| c11 | Firmware and upgrades | c23 | Port profiles, LAG, radio, and AP controls |
| c12 | Networks and VLANs | c24 | Multi-site/cloud and ISP metrics |

The Instant On route descriptions come from a GET-only inspection of the public [Instant On portal](https://portal.instant-on.hpe.com/) and its [runtime settings](https://portal.instant-on.hpe.com/settings.json), captured 2026-10-06. This was a reverse-engineered SPA route/model map, not published API documentation; it included no authenticated API calls or write requests. The map's section references used in this matrix mean: §2 auth/token flow; §3.1 sites/settings/singletons; §3.2 inventory/devices/actions; §3.3 networks/VLANs/SSIDs; §3.4 clients; §3.5 administrators/accounts; §3.6 MSP/multi-site; §3.7 local AP endpoint; §4 gaps and unverified behavior. The map itself is not bundled here. A `no route observed` result means only that the route was not found in that inspected map.

## Site / System

| UniFi operation | Instant On route | instantctl command | Status | Evidence |
|---|---|---|---|---|
| [c1] Auth, profile, token setup | SSO (sso.arubainstanton.com) OIDC+PKCE, not a portal route (reviewed endpoint map § 2) | `instantctl auth login` / `auth status` / `auth logout` | implemented | `crates/instantctl/src/commands/auth.rs`; `crates/instantctl-api/src/auth.rs`; live READ: `auth status` passed 2026-10-07; a deliberately invalid isolated profile produced a safely classified refresh failure and logout removed that profile; this does not prove revocation for the default account. Login is not covered by the read run. |
| [c1] App/controller info + capability probe | GET /capabilities ; GET /sites/{s}/capabilities | `instantctl site capabilities` ; `instantctl version` | implemented | `crates/instantctl/src/commands/site.rs`; `crates/instantctl-api/src/site.rs`; READ: `site capabilities` passed 2026-10-07; `version` passed 2026-10-07; WRITE not applicable. |
| List sites | GET /sites | `instantctl site list` | implemented | `crates/instantctl/src/commands/site.rs`; `crates/instantctl-api/src/site.rs`; READ: `site list` passed 2026-10-07; WRITE not applicable. |
| Show one site | GET /sites (+ capabilities) | `instantctl site show [ID]` | implemented | `crates/instantctl/src/commands/site.rs`; `crates/instantctl-api/src/site.rs`; READ: `site show` passed 2026-10-07; WRITE not applicable. |
| Site settings: timezone | GET/PUT /sites/{s}/timezone | `instantctl site timezone [ZONE]` | implemented | `crates/instantctl/src/commands/site.rs`; `crates/instantctl-api/src/site.rs`; READ: `site timezone` passed 2026-10-07; mock-tested WRITE; no live write receipt. |
| Site settings: management network / VLAN | GET/PUT /sites/{s}/managementNetwork | `instantctl site management-network` | implemented | `crates/instantctl/src/commands/site.rs`; `crates/instantctl-api/src/site.rs`; READ: `site management-network` passed 2026-10-07; mock-tested WRITE; no live write receipt. |
| Site settings: DNS (mgmt subnet DNS mode) | GET/PUT /sites/{s}/managementNetwork (nested managementSubnet.dns) | `instantctl site dns` | implemented | `crates/instantctl/src/commands/site.rs`; `crates/instantctl-api/src/site.rs`; READ: `site dns` passed 2026-10-07; mock-tested WRITE; no live write receipt. |
| Site settings: spanning tree (STP) | GET/PUT /sites/{s}/spanningTree | `instantctl site spanning-tree` | implemented | `crates/instantctl/src/commands/site.rs`; `crates/instantctl-api/src/site.rs`; READ: `site spanning-tree` passed 2026-10-07; mock-tested WRITE; no live write receipt. |
| STP auto bridge priorities (Instant On only) | POST /sites/{s}/spanningTree?action=computeDevicesBridgePriority | `instantctl site stp-auto-priority` | implemented | `crates/instantctl/src/commands/site.rs`; `crates/instantctl-api/src/site.rs`; READ not covered by the 2026-10-07 run; mock-tested WRITE; no live write receipt. |
| Site settings: NTP, SNMP, SSH/mgmt, locale, LED, connectivity | no route observed (reviewed endpoint map § 3.1 has no such singleton) | none | no route observed; no command | reviewed endpoint map § 3.1 searched; raw `api` can call a known API path |
| Create / delete / rename site | POST /initialSetup; GET/PUT /sites/{s}/administration; DELETE/GET /sites/{s} | `instantctl site create`; `site rename`; `site delete` | implemented | `crates/instantctl/src/commands/site_lifecycle.rs`; `crates/instantctl-api/src/client/site_lifecycle.rs`; READ not covered by the 2026-10-07 run; mock-tested WRITE; no live write receipt. |
| Clone site (Instant On only) | POST /sites/{s}/siteCloning | `instantctl site clone` | implemented | `crates/instantctl/src/commands/site_lifecycle.rs`; `crates/instantctl-api/src/client/site_lifecycle.rs`; READ not covered by the 2026-10-07 run; mock-tested WRITE; no live write receipt. |
| Country list | GET /public/country (fixed public route outside /api) | `instantctl site country` | implemented | `crates/instantctl/src/commands/site_lifecycle.rs`; `crates/instantctl-api/src/client/country.rs`; READ: `site country` passed 2026-10-07; WRITE not applicable. |
| [c24] Multi-site / cloud (Site Manager hosts, ISP metrics) | Multi-site = GET /sites; no Site Manager cloud route observed; MSP /amoApi/customers, domains, reports (sec 3.6) | `instantctl site list` (multi-site listing only) | implemented | `crates/instantctl/src/commands/site.rs`; `crates/instantctl-api/src/site.rs`; READ: `site list` passed 2026-10-07; WRITE not applicable. |
| [c24] ISP metrics / MSP customers, domains, reports | /amoApi/customers, domains, reports/generated, reports/scheduled (reviewed endpoint map § 3.6) | none | no command; MSP routes were observed, but this package has no MSP command | No CLI command in this package. |

## Devices

| UniFi operation | Instant On route | instantctl command | Status | Evidence |
|---|---|---|---|---|
| [c2] List adopted devices | GET /sites/{s}/inventory | `instantctl device list` | implemented | `crates/instantctl/src/commands/device.rs`; `crates/instantctl-api/src/device.rs`; READ: `device list` passed 2026-10-07; WRITE not applicable. |
| [c2] Get device | GET /sites/{s}/inventory (select matching item from complete inventory) | `instantctl device show <ID>` | implemented | `crates/instantctl/src/commands/device.rs`; `crates/instantctl-api/src/device.rs`; READ: `device show` passed 2026-10-07; WRITE not applicable. |
| [c1] [c2] Local AP health (no UniFi analog) | GET http://<ap>:8080/swarm.cgi?opcode=smb_debug_info (local, not in bundle) | `instantctl device health <AP_OR_HOST>` | implemented | `crates/instantctl/src/commands/device.rs`; `crates/instantctl-api/src/device.rs`; READ: `device health` passed 2026-10-07; WRITE not applicable. |
| Pending adoption list | no route observed | none | no route observed; no command; onboarding appears to use in-app claims and site setup, not a device adoption route | reviewed endpoint map § 3.2, 3.6 |
| [c15] Adopt device | no adoption route observed (site cloning and replacement are separate operations) | none | no route observed; no command | reviewed endpoint map § 3.2/3.6 |
| [c15] Forget / remove device | DELETE /sites/{s}/inventory/{mac} (inferred from generic datasource) | `instantctl device forget <DEVICE>` | implemented | `crates/instantctl/src/commands/device.rs`; `crates/instantctl-api/src/device.rs`; READ not covered by the 2026-10-07 run; mock-tested WRITE; no live write receipt. |
| [c4] Restart device | POST /sites/{s}/inventory/{mac}?action=reboot | `instantctl device reboot <DEVICE>` | implemented | `crates/instantctl/src/commands/device.rs`; `crates/instantctl-api/src/device.rs`; READ not covered by the 2026-10-07 run; mock-tested WRITE; no live write receipt. |
| [c8] Locate (blink LED) | POST ...?action=activateLocatorLED / deactivateLocatorLED | `instantctl device locate <DEVICE> <STATE>` | implemented | `crates/instantctl/src/commands/device.rs`; `crates/instantctl-api/src/device.rs`; READ not covered by the 2026-10-07 run; live WRITE verified: locator on and off on 2026-10-07. |
| LED mode (on/quiet) | PUT /sites/{s}/inventory/{mac} body ledMode | `instantctl device led <DEVICE> <STATE>` | implemented | `crates/instantctl/src/commands/device.rs`; `crates/instantctl-api/src/device.rs`; READ not covered by the 2026-10-07 run; mock-tested WRITE; no live write receipt. |
| Rename device | PUT /sites/{s}/inventory/{mac} body name | `instantctl device rename <DEVICE> <NAME>` | implemented | `crates/instantctl/src/commands/device.rs`; `crates/instantctl-api/src/device.rs`; READ not covered by the 2026-10-07 run; live WRITE verified: rename and restore on 2026-10-07. |
| Static management IP | PUT /sites/{s}/inventory/{mac} body managementIpAddress | `instantctl device management-ip <DEVICE> <ADDRESS> --prefix-length --gateway --dns` | implemented | `crates/instantctl/src/commands/device.rs`; `crates/instantctl-api/src/device.rs`; READ not covered by the 2026-10-07 run; mock-tested WRITE; no live write receipt. |
| Power usage / PoE draw | GET /sites/{s}/inventory/{mac}/powerUsage | `instantctl device power-usage <DEVICE>` | implemented | `crates/instantctl/src/commands/device.rs`; `crates/instantctl-api/src/device.rs`; READ: switch power-usage passed in the 2026-10-07 sweep; AP power-usage was not exercised; WRITE not applicable. |
| [c21] Latest device stats (cpu/mem/uptime/uplink) | GET /sites/{s}/deviceDetails/{mac} | `instantctl device details <DEVICE>` | implemented | `crates/instantctl/src/commands/device.rs`; `crates/instantctl-api/src/device.rs`; READ: `device details` passed 2026-10-07; WRITE not applicable. |
| Provision / force-provision | no route observed | none | no route observed; no command | reviewed endpoint map § 3.2 |
| Move / migrate device between sites | no route observed | none | no route observed; no command | reviewed endpoint map § 3.2 |
| Device tags / groups | no route observed for devices (client tags exist, see Clients) | none | no route observed; no command | reviewed endpoint map § 3.2 |
| Device DHCP reservation (Instant On only) | POST /sites/{s}/inventory/{mac}?action=reserveIp; body ipReservations[{networkId,ipAddress}], empty array removes | `instantctl device reserve-ip <DEVICE> <IP> --network <ID>`; `device remove-ip-reservation <DEVICE>` | implemented | `crates/instantctl/src/commands/device.rs`; `crates/instantctl-api/src/device.rs`; READ not covered by the 2026-10-07 run; mock-tested WRITE; no live write receipt. |
| Uplink connection type (Instant On only) | POST ...?action=setUplinkConnectionType; empty body acknowledges UPLINK_TYPE_CHANGED | none | no setter (alert confirmation only) | The reviewed route exposes only an alert confirmation; no selected uplink type or setter body is established. |
| Gateway / router role (Instant On only) | POST ...?action=setGateway / setAsRouter / unsetAsRouter (body unknown) | none | gateway-dependent route; no command | Route map §3.2 lists these actions but does not establish their request body or account/device support. |
| Replace device (Instant On only) | GET /sites/{s}/replaceDevice/{oldDeviceId}; separate gather/replace actions | `instantctl device replacement-candidates` (read only) | implemented | `crates/instantctl/src/commands/device.rs`; `crates/instantctl-api/src/device.rs`; READ: `device replacement-candidates` passed 2026-10-07; WRITE not applicable. |
| Spectrum scan | no route observed | none | no route observed; no command | reviewed endpoint map § 3.2 |

## Ports / PoE / LAG

| UniFi operation | Instant On route | instantctl command | Status | Evidence |
|---|---|---|---|---|
| Port list + state (speed, PoE state) | GET /sites/{s}/inventory (ethernetPorts, trunkPorts) | `instantctl port list [DEVICE]` ; `instantctl port show <DEVICE> <PORT>` | implemented | `crates/instantctl/src/commands/port.rs`; `crates/instantctl-api/src/ports.rs`; READ: `port show` passed 2026-10-07; `port list` passed 2026-10-07; WRITE not applicable. |
| [c5] PoE power-cycle one port | Switch port: no proven route. Used: POST /sites/{s}/clientDetails/{id}?action=powerCycle (body {}). Inventory ?action=powerCyclePort exists in enum only (body unproven) | `instantctl port power-cycle <DEVICE> <PORT>` ; `instantctl client power-cycle <ID>` | implemented | `crates/instantctl/src/commands/port.rs`; `crates/instantctl-api/src/ports.rs`; READ not covered by the 2026-10-07 run; mock-tested WRITE; no live write receipt. |
| [c5] Find which port a client/device is on | GET /sites/{s}/clientSummary + inventory | `instantctl port find <CLIENT>` ; `instantctl client where-is <ID>` | implemented | `crates/instantctl/src/commands/port.rs`; `crates/instantctl-api/src/ports.rs`; READ: `client where-is` passed 2026-10-07; `port find` passed 2026-10-07; WRITE not applicable. |
| [c23] Set port profile / PoE mode / VLAN / speed per port | PUT /sites/{s}/inventory/{mac} body ethernetPorts[] | `instantctl port set <DEVICE> <PORT> [--profile ...]` | implemented | `crates/instantctl/src/commands/port.rs`; `crates/instantctl-api/src/ports.rs`; READ not covered by the 2026-10-07 run; mock-tested WRITE; no live write receipt. |
| [c23] Port profiles CRUD | GET/POST/PUT/DELETE /sites/{s}/portProfiles[/{id}] | `instantctl port profile list\|show\|create\|set\|remove` | implemented | `crates/instantctl/src/commands/port.rs`; `crates/instantctl-api/src/ports.rs`; READ: `port profile list` passed 2026-10-07; mock-tested WRITE; no live write receipt. |
| [c23] LAG read | GET /sites/{s}/inventory (trunkPorts) | `instantctl lag list [--switch <device>]` | implemented | `crates/instantctl/src/commands/lag.rs`; `crates/instantctl-api/src/ports.rs`; READ: `lag list` passed 2026-10-07; WRITE not applicable. |
| [c23] LAG create / break | PUT inventory trunkPorts[] ; POST ...?action=resetTrunkPort {trunkNumber} | `instantctl lag create <DEVICE> <TRUNK> --ports ... [--mode static\|lacp]` ; `instantctl lag remove <DEVICE> <TRUNK>` | implemented | `crates/instantctl/src/commands/lag.rs`; `crates/instantctl-api/src/ports.rs`; READ not covered by the 2026-10-07 run; mock-tested WRITE; no live write receipt. |
| Switch stacks (UniFi: read-only stacks / MC-LAG) | GET /sites/{s}/deviceStacks; write routes/actions retained but not implemented | `instantctl stack list`; `stack show` | implemented | `crates/instantctl/src/commands/stack.rs`; `crates/instantctl-api/src/client/stacks.rs`; READ: `stack show` had no subject (empty source collection); `stack list` passed 2026-10-07; WRITE not applicable. |
| PoE schedule (Instant On only) | GET/PUT /sites/{s}/poeSchedule | `instantctl port poe-schedule show\|set` | implemented | `crates/instantctl/src/commands/port.rs`; `crates/instantctl-api/src/ports.rs`; READ: `port poe-schedule show` passed 2026-10-07; mock-tested WRITE; no live write receipt. |
| Energy Efficient Ethernet (Instant On only) | GET/PUT /sites/{s}/powerManagement | `instantctl port eee show\|set` | implemented | `crates/instantctl/src/commands/port.rs`; `crates/instantctl-api/src/ports.rs`; READ: `port eee show` passed 2026-10-07; mock-tested WRITE; no live write receipt. |
| Port mirroring (Instant On only) | PUT inventory portMirroringConfig | `instantctl port mirror <DEVICE> ...` | implemented | `crates/instantctl/src/commands/port.rs`; `crates/instantctl-api/src/ports.rs`; READ not covered by the 2026-10-07 run; mock-tested WRITE; no live write receipt. |
| Cable test (Instant On only) | POST /sites/{s}/cableTest/{mac} {portNumber}; GET polls | `instantctl port cable-test` | implemented | `crates/instantctl/src/commands/port.rs`; `crates/instantctl-api/src/ports.rs`; READ not covered by the 2026-10-07 run; mock-tested WRITE; no live write receipt. |
| Connectivity test: ping / traceroute from switch (Instant On only) | POST /sites/{s}/connectivityTest/{mac}?action=start\|abort | `instantctl port connectivity-test` | implemented | `crates/instantctl/src/commands/port.rs`; `crates/instantctl-api/src/ports.rs`; READ not covered by the 2026-10-07 run; mock-tested WRITE; no live write receipt. |
| MAC table / port anomalies | no route observed | none | no route observed; no command | reviewed endpoint map § 3.2 |
| PoE outlet / PDU control | no route observed | none | no route observed; no command; no PDU class was observed in the reviewed API map | reviewed endpoint map |

## Radios / RF

| UniFi operation | Instant On route | instantctl command | Status | Evidence |
|---|---|---|---|---|
| Radio state (channel, width, standard) | GET /sites/{s}/inventory (radios on AP objects) | `instantctl radio list` | implemented | `crates/instantctl/src/commands/radio.rs`; `crates/instantctl-api/src/client/radio.rs`; READ: `radio list` passed 2026-10-07; WRITE not applicable. |
| Radio stats (retries, utilization) | AP fields in inventory / deviceDetails | `instantctl radio list` ; `instantctl device details <DEVICE>` | implemented | `crates/instantctl/src/commands/radio.rs`; `crates/instantctl-api/src/client/radio.rs`; READ: `device details` passed 2026-10-07; `radio list` passed 2026-10-07; WRITE not applicable. |
| [c23] Set site radio plan (band mapping, channel, width, tx power) | GET/PUT /sites/{s}/radioManagement | `instantctl radio plan get\|set` | implemented | `crates/instantctl/src/commands/radio.rs`; `crates/instantctl-api/src/client/radio.rs`; READ: `radio plan get` passed 2026-10-07; mock-tested WRITE; no live write receipt. |
| [c23] Per-AP channel/power override | PUT inventory/{mac} radioManagementBands | `instantctl radio override get\|set <AP>` | implemented | `crates/instantctl/src/commands/radio.rs`; `crates/instantctl-api/src/client/radio.rs`; READ: `radio override get` passed 2026-10-07; mock-tested WRITE; no live write receipt. |
| Disable AP / per-AP SSID bindings | SSID `accessPoints[]`; reverse device inventory `wirelessNetworks[]` | `instantctl wlan list/show`; `wlan create/update --ap <AP>` or `--all-aps`; no device-disable command | partially implemented (SSID binding only) | `crates/instantctl/src/commands/wlan.rs`; `crates/instantctl-api/src/client/wlan/advanced.rs`; READ: `wlan list` and `wlan show` passed 2026-10-07; mock-tested WRITE; no live write receipt. |
|  LED override | see Devices | `instantctl device led <DEVICE> <STATE>` | implemented | `crates/instantctl/src/commands/device_config.rs`; `crates/instantctl-api/src/device.rs`; READ not covered by the 2026-10-07 run; mock-tested WRITE; no live write receipt. |
| [c23] Rogue / neighbor APs | no route observed | none | no route observed; no command | reviewed endpoint map § 3.1/3.2 |
| [c23] Spectrum scan | no route observed | none | no route observed; no command | reviewed endpoint map § 3.2 |
| Current channels per country | no separate country-channel route observed; AP radio settings are part of inventory | none | no route observed; no command | Route map §3.2 describes radio settings within inventory; it does not define a country-filtered channel-list route. |
|  Mesh / extend network (Instant On only) | GET/PUT /sites/{s}/extendNetwork; extendWirelessNetworkAllowList; extendWiredPortAllowList | `instantctl site extend-network`; `wlan allowlist --add/--remove <MAC>`; `device allowlist <DEVICE> --port/--trunk <N> --add/--remove <MAC>` | implemented | `crates/instantctl/src/commands/site.rs`, `commands/allowlist.rs`, `commands/wlan.rs`, `commands/device.rs`; `crates/instantctl-api/src/client/allowlist.rs`; READ: `site extend-network` and wireless allowlist passed 2026-10-07; wired discovery checked the complete positive-port census and skipped the dependent read as `no_live_subject` because each was explicitly forbidden; mock-tested WRITE; no live write receipt. |

## Clients

| UniFi operation | Instant On route | instantctl command | Status | Evidence |
|---|---|---|---|---|
| [c3] List connected clients (wired/wireless, uplink) | GET /sites/{s}/clientSummary | `instantctl client list` | implemented | `crates/instantctl/src/commands/client.rs`; `crates/instantctl-api/src/client/operations.rs`; READ: `client list` passed 2026-10-07; WRITE not applicable. |
| [c3] Client detail / lookup by name or MAC | GET /sites/{s}/clientDetails/{id} | `instantctl client show <ID>` | implemented | `crates/instantctl/src/commands/client.rs`; `crates/instantctl-api/src/client/operations.rs`; READ: `client show` passed 2026-10-07; WRITE not applicable. |
| [c3] Where is client (port / AP) | clientSummary + inventory | `instantctl client where-is <ID>` | implemented | `crates/instantctl/src/commands/client.rs`; `crates/instantctl-api/src/client/operations.rs`; READ: `client where-is` passed 2026-10-07; WRITE not applicable. |
| All known clients (history) | no route observed (wiredClientSummary only seen in community clients, not current bundle) | none | no route observed; no command | reviewed endpoint map § 4 |
| [c9] Block / unblock client | GET/POST/DELETE /sites/{s}/blockedClients | `instantctl client block <ID>` / `client unblock <ID>` | implemented | `crates/instantctl/src/commands/client.rs`; `crates/instantctl-api/src/client/operations.rs`; READ not covered by the 2026-10-07 run; mock-tested WRITE; no live write receipt. |
| [c9] Kick / reconnect client | no route observed | none | no route observed; no command | reviewed endpoint map § 3.4 |
| Forget client | no route observed | none | no route observed; no command | reviewed endpoint map § 3.4 |
| [c10] Rename client (alias) | PUT /sites/{s}/clientDetails/{id} {kind,name} | `instantctl client rename <ID> <NAME>` | implemented | `crates/instantctl/src/commands/client.rs`; `crates/instantctl-api/src/client/operations.rs`; READ not covered by the 2026-10-07 run; mock-tested WRITE; no live write receipt. |
| [c10] DHCP reservation (fixed IP) | POST /sites/{s}/clientSummary/{id}?action=reserveIp | `instantctl client reserve-ip <ID> <IP> --network <NETWORK>` | implemented | `crates/instantctl/src/commands/client.rs`; `crates/instantctl-api/src/client/operations.rs`; READ not covered by the 2026-10-07 run; mock-tested WRITE; no live write receipt. |
| Client tags (user groups, loosely) | GET/POST/PUT/DELETE /sites/{s}/clientClassifications ; ?action=replaceCommonTags | `instantctl client tags list\|set\|add\|remove` | implemented | `crates/instantctl/src/commands/client.rs`; `crates/instantctl-api/src/client/operations.rs`; READ: `client tags list` passed 2026-10-07; mock-tested WRITE; no live write receipt. |
| Client watchlist (Instant On only) | POST clientDetails/{id}?action=addToWatchlist\|removeFromWatchlist | `instantctl client watchlist add\|remove` | implemented | `crates/instantctl/src/commands/client.rs`; `crates/instantctl-api/src/client/operations.rs`; READ not covered by the 2026-10-07 run; mock-tested WRITE; no live write receipt. |
| QoS rate groups (UniFi usergroup) | no route observed (per-SSID limits are in Wi-Fi) | none | no route observed; no command | see `wlan bandwidth` |
| Sessions / auths / fingerprint devices | no route observed | none | no route observed; no command | reviewed endpoint map § 3.4 |

## Networks / VLANs

| UniFi operation | Instant On route | instantctl command | Status | Evidence |
|---|---|---|---|---|
| [c12] List / get networks | GET /sites/{s}/wiredNetworks[/{id}] | `instantctl network list` / `network show <ID>` | implemented | `crates/instantctl/src/commands/network.rs`; `crates/instantctl-api/src/client/network.rs`; READ: `network show` passed 2026-10-07; `network list` passed 2026-10-07; WRITE not applicable. |
| [c12] Create / update / delete network | POST/PUT/DELETE /sites/{s}/wiredNetworks[/{id}] | `instantctl network create <NEW_NAME>` / `network update <ID>` / `network delete <ID>` | implemented | `crates/instantctl/src/commands/network.rs`; `crates/instantctl-api/src/client/network.rs`; READ not covered by the 2026-10-07 run; live WRITE verified: unused-network create and delete on 2026-10-07; update not exercised. |
| [c12] Usage references (what blocks delete) | no dedicated route; wiredNetworks isDeletable + devicePortMappings | `instantctl network delete <ID>` (--yes for assigned/unknown port mappings) | implemented | `crates/instantctl/src/commands/network.rs`; `crates/instantctl-api/src/client/network.rs`; READ not covered by the 2026-10-07 run; mock-tested WRITE; no live write receipt. |
| Inter-VLAN IP routing config (isIpRoutingEnabled/ipRoutingConfig) | GET/PUT /sites/{s}/wiredNetworks/{networkId} (GET from complete collection) | `network routing <ID>`; `--enabled true/false` | implemented | `crates/instantctl/src/commands/network.rs`; `crates/instantctl-api/src/client/network.rs`; READ: `network routing` passed 2026-10-07; mock-tested WRITE; no live write receipt. |
| WAN list | GET/POST/PUT/DELETE /sites/{s}/wans | none | gateway-dependent route; no command route exists but needs an Instant On gateway; not established for this account | gateway-only: gateway-dependent; account/device availability is not established here |
| WAN failover / redundancy | GET/PUT /sites/{s}/wanRedundancyConfiguration | none | gateway-dependent route; no command route exists but needs an Instant On gateway | gateway-only: gateway-dependent; account/device availability is not established here |
| Management VLAN | see Site / System | `instantctl site management-network` | implemented | `crates/instantctl/src/commands/network.rs`; `crates/instantctl-api/src/client/network.rs`; READ: `site management-network` passed 2026-10-07; mock-tested WRITE; no live write receipt. |

## WiFi / SSIDs

| UniFi operation | Instant On route | instantctl command | Status | Evidence |
|---|---|---|---|---|
|  [c13] List / get SSIDs | GET /sites/{s}/networksSummary | `instantctl wlan list` / `wlan show <ID>` | implemented | `crates/instantctl/src/commands/wlan.rs`; `crates/instantctl-api/src/client/wlan/advanced.rs`; READ: `wlan show` passed 2026-10-07; `wlan list` passed 2026-10-07; WRITE not applicable.  |
|  [c13] Create / update / delete SSID | POST / PUT /{id} / DELETE /{id} on /sites/{s}/networksSummary | `instantctl wlan create <NEW_NAME>` / `wlan update <ID>` / `wlan delete <ID>` | implemented | `crates/instantctl/src/commands/wlan.rs`; `crates/instantctl-api/src/client/wlan/advanced.rs`; READ not covered by the 2026-10-07 run; live WRITE verified: disabled WLAN create and delete/absence on 2026-10-07; update not exercised.  |
|  [c13] Enable / disable SSID | PUT networksSummary/{id} isEnabled | `instantctl wlan enable <ID>` / `wlan disable <ID>` | implemented | `crates/instantctl/src/commands/wlan.rs`; `crates/instantctl-api/src/client/wlan/advanced.rs`; READ not covered by the 2026-10-07 run; mock-tested WRITE; no live write receipt.  |
| [c13] Change passphrase | PUT networksSummary/{id} preSharedKey | `instantctl wlan passphrase <ID>` (stdin or hidden prompt) | implemented | `crates/instantctl/src/commands/wlan.rs`; `crates/instantctl-api/src/client/wlan.rs`; READ not covered by the 2026-10-07 run; mock-tested WRITE; no live write receipt. |
|  [c13] Hide SSID | PUT networksSummary/{id} isSsidHidden | `instantctl wlan update <ID> --hidden true\|false` | implemented | `crates/instantctl/src/commands/wlan.rs`; `crates/instantctl-api/src/client/wlan/advanced.rs`; READ not covered by the 2026-10-07 run; mock-tested WRITE; no live write receipt.  |
| [c13] Client isolation | isIntraSubnetTrafficAllowed=false (no isolation flag in body) | `instantctl wlan access <ID> --intra-subnet-traffic deny` | implemented | `crates/instantctl/src/commands/wlan.rs`; `crates/instantctl-api/src/client/wlan.rs`; READ not covered by the 2026-10-07 run; mock-tested WRITE; no live write receipt. |
| Bands (2.4/5/6 GHz) | networksSummary isAvailableOn24/5/6GHzRadioBand | `instantctl wlan bands <ID> --bands 2.4,5,6` | implemented | `crates/instantctl/src/commands/wlan.rs`; `crates/instantctl-api/src/client/wlan.rs`; READ not covered by the 2026-10-07 run; mock-tested WRITE; no live write receipt. |
|  SSID schedule (inline) | networksSummary schedule/weekSchedule/activeSchedule | `instantctl wlan schedule <ID> --schedule off\|always\|timed` | implemented | `crates/instantctl/src/commands/wlan.rs`; `crates/instantctl-api/src/client/wlan/advanced.rs`; READ not covered by the 2026-10-07 run; mock-tested WRITE; no live write receipt.  |
| Per-client / per-network bandwidth limit | networksSummary bandwidth fields | `instantctl wlan bandwidth <ID> --mode off\|per-client\|per-network` | implemented | `crates/instantctl/src/commands/wlan.rs`; `crates/instantctl-api/src/client/wlan.rs`; READ not covered by the 2026-10-07 run; mock-tested WRITE; no live write receipt. |
| Guest vs employee type flag | networksSummary type | `instantctl wlan guest <ID>` | implemented | `crates/instantctl/src/commands/wlan.rs`; `crates/instantctl-api/src/client/wlan.rs`; READ not covered by the 2026-10-07 run; mock-tested WRITE; no live write receipt. |
| Access restriction / allowed destinations | networksSummary isAccessRestricted, isInternetAllowed, allowedDestinations | `instantctl wlan access <ID>` | implemented | `crates/instantctl/src/commands/wlan.rs`; `crates/instantctl-api/src/client/wlan.rs`; READ not covered by the 2026-10-07 run; mock-tested WRITE; no live write receipt. |
| Bind SSID to VLAN / wired network | networksSummary useVlan, vlanId, wiredNetworkId | `instantctl wlan list/show`; `wlan create/update --wired-network <SELECTOR>` or `--vlan <ID>` | implemented | `crates/instantctl/src/commands/wlan.rs`; `crates/instantctl-api/src/client/wlan/advanced.rs`; READ: `wlan list` and `wlan show` passed 2026-10-07; mock-tested WRITE; no live write receipt. |
| Per-AP SSID binding (UniFi AP groups) | networksSummary accessPoints[]; inventory wirelessNetworks[] | `instantctl wlan list/show`; `wlan create/update --ap <AP>` or `--all-aps` | implemented | `crates/instantctl/src/commands/wlan.rs`; `crates/instantctl-api/src/client/wlan/advanced.rs`; READ: `wlan list` and `wlan show` passed 2026-10-07; mock-tested WRITE; no live write receipt. |
| Enterprise (WPA2/3-Enterprise) SSID | networksSummary authentication/security and radiusProfileId | `instantctl wlan list/show`; `wlan create/update --security wpa2-enterprise\|wpa3-enterprise --radius-profile <PROFILE>` | implemented | `crates/instantctl/src/commands/wlan.rs`; `crates/instantctl-api/src/client/wlan/advanced.rs`; READ: `wlan list` and `wlan show` passed 2026-10-07; mock-tested WRITE; no live write receipt. |
| Advanced radio flags (Wi-Fi 6/7, OFDMA, MLO, legacy rates, multicast, broadcast, QoS priority) | networksSummary capability and QoS fields | `instantctl wlan list/show`; `wlan create/update --legacy-rates --wifi6 --ofdma --wifi7 --mlo --multicast-optimization --broadcast-all-bands --traffic-priority` | implemented | `crates/instantctl/src/commands/wlan.rs`; `crates/instantctl-api/src/client/wlan/advanced.rs`; READ: `wlan list` and `wlan show` passed 2026-10-07; mock-tested WRITE; no live write receipt. |

## Guest / Hotspot

| UniFi operation | Instant On route | instantctl command | Status | Evidence |
|---|---|---|---|---|
| Guest portal settings (type, pages, redirect) | GET/PUT /sites/{s}/guestPortalSettings | `instantctl guest-portal show` / `guest-portal update` | implemented | `crates/instantctl/src/commands/guest_portal.rs` / `schedule.rs`; `crates/instantctl-api/src/client/access.rs`; READ: `guest-portal show` passed 2026-10-07; mock-tested WRITE; no live write receipt. |
| Named schedules (policy-referenced) | GET/POST/PUT/DELETE /sites/{s}/schedules[/{id}] | `instantctl schedule list\|show\|create\|update\|delete` | implemented | `crates/instantctl/src/commands/guest_portal.rs` / `schedule.rs`; `crates/instantctl-api/src/client/access.rs`; READ: `schedule list` passed 2026-10-07; mock-tested WRITE; no live write receipt. |
| Enable captive portal per SSID/network | networksSummary isCaptivePortalEnabled; wiredNetworks isGuestPortalEnabled | `instantctl wlan list/show`; `wlan create/update --captive-portal <BOOL>` | implemented | `crates/instantctl/src/commands/wlan.rs`; `crates/instantctl-api/src/client/wlan/advanced.rs`; READ: `wlan list` and `wlan show` passed 2026-10-07; mock-tested WRITE; no live write receipt. |
| [c18] Generate vouchers | no route observed | none | no route observed; no command; Instant On guest access is click-through/external page, no voucher resource | reviewed endpoint map § 3.1 guestPortalSettings only |
| [c18] List / delete vouchers | no route observed | none | no route observed; no command | same |
| [c9] Authorize / unauthorize guest client | no route observed | none | no route observed; no command | reviewed endpoint map § 3.4 |
| Guest list, extend validity, operators, payments | no route observed | none | no route observed; no command | reviewed endpoint map |

## RADIUS / 802.1X

| UniFi operation | Instant On route | instantctl command | Status | Evidence |
|---|---|---|---|---|
| RADIUS profiles CRUD | GET/POST/PUT/DELETE /sites/{s}/radiusProfiles[/{id}] | `instantctl radius list\|show\|create\|update\|delete` | implemented | `crates/instantctl/src/commands/radius.rs` / `port_access_control.rs`; `crates/instantctl-api/src/client/access.rs`; READ: `radius list` passed 2026-10-07; mock-tested WRITE; no live write receipt. |
| Port access control (802.1X RADIUS for ports) | GET/PUT /sites/{s}/portAccessControlSettings | `instantctl port-access-control show` / `port-access-control update` | implemented | `crates/instantctl/src/commands/radius.rs` / `port_access_control.rs`; `crates/instantctl-api/src/client/access.rs`; READ: `port-access-control show` passed 2026-10-07; mock-tested WRITE; no live write receipt. |
| Assign RADIUS profile to SSID / device ports | networksSummary radiusProfileId; inventory ethernetPortRadiusProfileId | `instantctl wlan create/update --radius-profile <PROFILE>`; no port-profile assignment command | partially implemented (SSID only) | `crates/instantctl/src/commands/wlan.rs`; `crates/instantctl-api/src/client/wlan/advanced.rs`; READ: `wlan list` and `wlan show` passed 2026-10-07; mock-tested SSID WRITE; device-port assignment has no command. |
| RADIUS user accounts (UniFi rest/account) | no route observed | none | no route observed; no command | reviewed endpoint map; non-core per footprint |

## Firewall / ACL

| UniFi operation | Instant On route | instantctl command | Status | Evidence |
|---|---|---|---|---|
| [c16] Firewall zones | no zone route observed | none | no zone route observed; no command | reviewed endpoint map § 3.1 |
| [c16] Firewall / zone policies list, create, enable, disable, reorder | GET/POST/PUT/DELETE /sites/{s}/policies[/{id}] (firewall category; no ordering route found) | `instantctl policy list` / `policy show <ID_OR_NAME>` (generic read only) | partially implemented (generic read only; no category-specific writes) | `crates/instantctl/src/commands/policy.rs`; `crates/instantctl-api/src/client/policies.rs`; READ: `policy list` passed 2026-10-07; no firewall write command. |
| [c16] Switch ACL rules (L2/L3) | GET/POST/PUT/DELETE /sites/{s}/policies (ACL category; site capability wired-network-acl) | `instantctl policy list` / `policy show <ID_OR_NAME>` (generic read only) | partially implemented (generic read only; no ACL writes) | `crates/instantctl/src/commands/policy.rs`; `crates/instantctl-api/src/client/policies.rs`; READ: `policy list` passed 2026-10-07; no ACL write command. |
| [c19] Traffic matching lists / groups | no route observed | none | no route observed; no command | reviewed endpoint map |
| Traffic rules / routes (v2) | no route observed | none | no route observed; no command | reviewed endpoint map |
| DPI restriction / application policies | /sites/{s}/policies (app category) | `instantctl policy list` / `policy show <ID_OR_NAME>` (generic read only) | partially implemented (generic read only; no application-policy writes) | `crates/instantctl/src/commands/policy.rs`; `crates/instantctl-api/src/client/policies.rs`; READ: `policy list` passed 2026-10-07; no application-policy write command. |
| ALG toggles / restart firewall (Instant On only) | GET/PUT /sites/{s}/firewall ; ?action=restartFirewall | none | gateway-dependent route; no command ALG/firewall engine lives on Instant On gateway | gateway-only: gateway-dependent; account/device availability is not established here |
| IPS / threat protection config + exceptions | GET/PUT /sites/{s}/securityThreatConfiguration ; /securityThreatExceptions | none | gateway-dependent route; no command threat protection needs an Instant On gateway | gateway-only: gateway-dependent; account/device availability is not established here |

## Routing / NAT / Port-forward

| UniFi operation | Instant On route | instantctl command | Status | Evidence |
|---|---|---|---|---|
| [c14] Port forwards list / create / delete | no route observed | none | no route observed; no command; also gateway-only (NAT) | gateway-only: gateway-dependent; account/device availability is not established here |
| [c20] Static routes | no route observed | none | no route observed; no command; routing needs an Instant On gateway | gateway-only: gateway-dependent; account/device availability is not established here |
| Policy / traffic routes | no route observed | none | no route observed; no command | gateway-only: gateway-dependent; account/device availability is not established here |
| [c20] VPN servers / networks (read) | GET/POST/PUT/DELETE /sites/{s}/vpnNetworks ; ?action=generateWireGuardConfig\|regenerateServerKeys | none | gateway-dependent route; no command route exists but VPN needs an Instant On gateway | gateway-only: gateway-dependent; account/device availability is not established here |
| Site-to-site tunnels | no route observed (vpnNetworks only) | none | no route observed; no command; gateway-only | gateway-only: gateway-dependent; account/device availability is not established here |
| WAN NAT outbound / BGP | no route observed | none | no route observed; no command; gateway-only | gateway-only: gateway-dependent; account/device availability is not established here |

## DNS / DHCP

| UniFi operation | Instant On route | instantctl command | Status | Evidence |
|---|---|---|---|---|
| [c17] Static DNS records (A/AAAA/CNAME) | no route observed | none | no route observed; no command; `site dns` only sets DNS servers, not records | reviewed endpoint map § 3.1 (dns singleton = server assignment mode) |
| DNS servers for management subnet | GET/PUT /sites/{s}/managementNetwork (nested managementSubnet.dns) | `instantctl site dns` | implemented | `crates/instantctl/src/commands`; `crates/instantctl-api/src`; READ: `site dns` passed 2026-10-07; mock-tested WRITE; no live write receipt. |
| DHCP server scope / pool / domain / DNS per network | wiredNetworks dhcpScope | `instantctl network dhcp <ID>` ; `network update <ID> --dhcp-enabled --gateway --start --end --dns-mode ...` | implemented | `crates/instantctl/src/commands`; `crates/instantctl-api/src`; READ not covered by the 2026-10-07 run; mock-tested WRITE; no live write receipt. |
| [c10] DHCP reservation (fixed IP) | POST clientSummary/{id}?action=reserveIp | `instantctl client reserve-ip <ID> <IP> --network <NETWORK>` | implemented | `crates/instantctl/src/commands`; `crates/instantctl-api/src`; READ not covered by the 2026-10-07 run; mock-tested WRITE; no live write receipt. |
| DHCP leases | no route observed | none | no route observed; no command | reviewed endpoint map |
| [c20] Dynamic DNS | no route observed | none | no route observed; no command | reviewed endpoint map |
| mDNS / service sharing across networks | GET/PUT /sites/{s}/sharedServicesConfiguration ; POST wiredNetworks/{id}?action=updateSharedService | `instantctl network shared-services status\|enable\|disable\|list\|share\|unshare` | implemented | `crates/instantctl/src/commands`; `crates/instantctl-api/src`; READ: `network shared-services status` passed 2026-10-07; mock-tested WRITE; no live write receipt. |

## Monitoring (health / events / alarms / stats / DPI)

| UniFi operation | Instant On route | instantctl command | Status | Evidence |
|---|---|---|---|---|
| [c6] Site health (+ WAN status) | GET /sites/{s}/health | `instantctl site health` ; `instantctl monitor health` | implemented | `crates/instantctl/src/commands/monitor.rs` / `event.rs`; `crates/instantctl-api/src/client/monitoring.rs`; READ: `monitor health` passed 2026-10-07; `site health` passed 2026-10-07; WRITE not applicable. |
| [c7] Events (recent, follow) | GET /sites/{s}/events | `instantctl event list [--since --follow --interval --tail]` | implemented | `crates/instantctl/src/commands/monitor.rs` / `event.rs`; `crates/instantctl-api/src/client/monitoring.rs`; READ: `event list` passed 2026-10-07; WRITE not applicable. |
| [c7] Alarms list | GET /sites/{s}/alerts | `instantctl alert list` | implemented | `crates/instantctl/src/commands/monitor.rs` / `event.rs`; `crates/instantctl-api/src/client/monitoring.rs`; READ: `alert list` passed 2026-10-07; WRITE not applicable. |
| Alarm archive | no route observed | none | no route observed; no command | reviewed endpoint map § 3.1 (GET only) |
| Dashboard | GET /sites/{s}/dashboard ; /landingPage | `instantctl monitor dashboard` ; `instantctl site dashboard` | implemented | `crates/instantctl/src/commands/monitor.rs` / `event.rs`; `crates/instantctl-api/src/client/monitoring.rs`; READ: `monitor dashboard` passed 2026-10-07; `site dashboard` passed 2026-10-07; WRITE not applicable. |
| Topology | GET /sites/{s}/graphTopology | `instantctl monitor topology` ; `instantctl site topology` | implemented | `crates/instantctl/src/commands/monitor.rs` / `event.rs`; `crates/instantctl-api/src/client/monitoring.rs`; READ: `monitor topology` passed 2026-10-07; `site topology` passed 2026-10-07; WRITE not applicable. |
| [c21] Top clients / traffic reports | GET /sites/{s}/stats/{networkId\|allNetworks}/client/usage | `instantctl monitor client-usage` | implemented | `crates/instantctl/src/commands/monitor.rs` / `event.rs`; `crates/instantctl-api/src/client/monitoring.rs`; READ: `monitor client-usage` passed 2026-10-07; WRITE not applicable. |
| [c21] DPI / application usage | GET /sites/{s}/applicationCategoryUsage | `instantctl monitor app-usage` | implemented | `crates/instantctl/src/commands/monitor.rs` / `event.rs`; `crates/instantctl-api/src/client/monitoring.rs`; READ: `monitor app-usage` passed 2026-10-07; WRITE not applicable. |
| Enable / disable app visibility | PUT /sites/{s}/applicationCategoryUsageConfiguration | `instantctl policy app-visibility get\|set <BOOL>` | implemented | `crates/instantctl/src/commands/policy.rs`; `crates/instantctl-api/src/client/policies.rs`; READ: `policy app-visibility get` passed 2026-10-07; mock-tested WRITE; no live write receipt. |
| Security threat events | GET /sites/{s}/securityThreatEvents | `instantctl monitor threats` | implemented | `crates/instantctl/src/commands/monitor.rs` / `event.rs`; `crates/instantctl-api/src/client/monitoring.rs`; READ: `monitor threats` passed 2026-10-07; WRITE not applicable. |
| [c21] Historical reports 5m/hourly/daily, site/ap/user | no additional route observed beyond usage + health history | none | no route observed; no command | reviewed endpoint map |
| [c21] Speedtest results | no route observed | none | no route observed; no command | reviewed endpoint map |
| Rogue AP / SSL cert / UPS stats | no route observed | none | no route observed; no command | reviewed endpoint map |

## Firmware

| UniFi operation | Instant On route | instantctl command | Status | Evidence |
|---|---|---|---|---|
| [c11] Show firmware version + updatable flag | GET /sites/{s}/inventory (firmware fields) | `instantctl device list` / `device show <ID>` | implemented | `crates/instantctl/src/commands/device.rs`; `crates/instantctl-api/src/device.rs`; READ: `device show` passed 2026-10-07; `device list` passed 2026-10-07; WRITE not applicable. |
| [c11] Check for updates (per device) | no route observed (isSoftwareUpdateNeeded only inside events/maintenance models) | none | no route observed; no command | The CLI consumes isSoftwareUpdateNeeded from maintenance/event data |
| [c11] Upgrade now (site-wide) | POST /sites/{s}/maintenance?action=updateSoftwareNow | `instantctl firmware update-now` | implemented | `crates/instantctl/src/commands/firmware.rs`; `crates/instantctl-api/src/client/firmware.rs`; READ not covered by the 2026-10-07 run; mock-tested WRITE; no live write receipt. |
| Maintenance window get/set | GET/PUT /sites/{s}/maintenance | `instantctl firmware window get\|set` | implemented | `crates/instantctl/src/commands/firmware.rs`; `crates/instantctl-api/src/client/firmware.rs`; READ: `firmware window get` passed 2026-10-07; live WRITE verified: maintenance-window set and restore on 2026-10-07. |
| Scheduled update | POST /sites/{s}/maintenance/schedule {localDateTime} | `instantctl firmware schedule` | implemented | `crates/instantctl/src/commands/firmware.rs`; `crates/instantctl-api/src/client/firmware.rs`; READ not covered by the 2026-10-07 run; mock-tested WRITE; no live write receipt. |
| [c11] Upgrade one device / external URL / rolling upgrade | no per-device firmware route observed in the reviewed API map | none | no route observed; no command; Instant On updates are site-wide | reviewed endpoint map § 4 |
| Firmware catalogue | no route observed | none | no route observed; no command | reviewed endpoint map § 4 |

## Backups / Config

| UniFi operation | Instant On route | instantctl command | Status | Evidence |
|---|---|---|---|---|
| [c22] Create / list / download backups | no route observed | none | no route observed; no command | reviewed endpoint map § 4: "Backup/restore of config: no route observed" |
| Export site | no route observed (siteCloning is closest) | none | no route observed; no command | reviewed endpoint map § 4 |
| [c22] Read settings (snapshot of site singletons) | GET timezone, dns, managementNetwork, spanningTree, maintenance, powerManagement, ... | `instantctl site timezone\|dns\|management-network\|spanning-tree` (each reads when no change flags) | implemented | `crates/instantctl/src/commands/site.rs`; `crates/instantctl-api/src/site.rs`; READ: `site timezone` passed 2026-10-07; WRITE not applicable. |
| Config-as-code (Terraform proxy) | no route observed | none | no route observed; no provider route observed for Instant On | no provider route was observed in the reviewed ecosystem map |

## Admins

| UniFi operation | Instant On route | instantctl command | Status | Evidence |
|---|---|---|---|---|
| [c22] List admins | GET /sites/{s}/administration | `instantctl admin list` | implemented | `crates/instantctl/src/commands/admin.rs`; `crates/instantctl-api/src/client/administration.rs`; READ: `admin list` passed 2026-10-07; WRITE not applicable. |
| Check account / invite / add / remove / replace / change role | POST /sites/{s}/administration?action=checkAccount\|addAccount\|removeAccount\|replaceAccount\|changeRole | `instantctl admin check-account\|add\|remove\|change-role`; replace absent | partially implemented (check/add/remove/change-role) | `crates/instantctl/src/commands/admin.rs`; `crates/instantctl-api/src/client/administration.rs`; READ: `admin check-account` passed 2026-10-07; mock-tested WRITE; no live write receipt. |
| Lock / unlock, maintenance mode, support token | POST ...?action=lockAccount\|unlockAccount\|enableMaintenanceMode\|disableMaintenanceMode\|generateSupportToken | `instantctl admin maintenance-mode get\|set`; `admin support-token`; lock/unlock absent | partially implemented (maintenance/support-token only) | `crates/instantctl/src/commands/admin.rs`; `crates/instantctl-api/src/client/administration.rs`; READ: `admin maintenance-mode get` passed 2026-10-07; mock-tested WRITE; no live write receipt. |
| My permissions | GET /sites/{s}/permissions | `instantctl admin permissions` | implemented | `crates/instantctl/src/commands/admin.rs`; `crates/instantctl-api/src/client/administration.rs`; READ: `admin permissions` passed 2026-10-07; WRITE not applicable. |
| API keys | no separate API-key route observed (auth uses bearer-token flow) | none | no route observed; no command | reviewed endpoint map § 2 |

## Raw API

| UniFi operation | Instant On route | instantctl command | Status | Evidence |
|---|---|---|---|---|
| [c1] Raw `api <path> [-X GET\|POST\|PUT\|DELETE]` | relative fixed-origin API paths (absolute URLs rejected) | `instantctl api <PATH> [-X METHOD] [-f k=v] [-F k=v] [--input FILE\|-] [--apply] [--yes] [--show-secrets]` | implemented | `crates/instantctl/src/commands/api.rs` / `completion.rs`; `crates/instantctl-api/src/client.rs`; READ: `api` passed 2026-10-07; mock-tested WRITE; no live write receipt. |
| Shell completions / version | n/a | `instantctl completion` ; `instantctl version` | implemented | `crates/instantctl/src/cli.rs` / `commands/completion.rs`; local installed-shell check passed for this source revision; READ: `version` passed 2026-10-07; WRITE not applicable. |

## Remaining gaps

These operations have an observed route or model field but no supported command in this source snapshot:

1. Firewall, switch ACL, and application-policy writes; generic policy list/show exists, but category-specific write semantics and request bodies are not established.
2. Account lock/unlock and replacement-account actions; supported bodies are not established.
3. Per-device replacement write, mesh join/map workflows, and uplink connection-type setter; the read or alert-only surfaces do not define a safe write contract.

Rows marked `no route observed` are not included here. The route inventory reflects the reviewed endpoint map, which is not bundled with this package. It does not prove that the platform has no such capability.

## Counts per status

| Status | Rows |
|---|---:|
| implemented | 100 |
| partially implemented | 7 |
| no command or gateway-dependent / unavailable route | 50 |
| route or model field observed / no CLI command | 1 |
| no setter (alert-only contract) | 1 |
| **TOTAL** | **159** |

All 24 ranks of footprint section 3 (core feature set) appear at least once.
