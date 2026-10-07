# Command reference

This page describes authentication, raw API access, common command families, write safety, and output behavior. Start with `instantctl --help` or a command-specific `--help` for the exact options in your installed build. For support status and known gaps, see [Capabilities](../CAPABILITIES.md).

The CLI provides version output, shell completions, help, raw API requests,
device telemetry, configuration commands with readback where supported, and
saved portal logins on macOS. Tokens are selected in this order: `--token-stdin`,
`HPE_INSTANT_ON_TOKEN`, then the saved `--profile` (default: `default`).
Static tokens bypass Keychain and saved profile metadata. An empty or invalid
environment value fails instead of falling back to a profile;
tokens are limited to 16,384 visible ASCII bytes and never appear in errors.
Saved profiles use macOS Keychain and refresh access tokens when needed. Other
platforms accept environment or stdin tokens.
The saved site and access token come from the same credential snapshot. If a
concurrent login changes the profile account, a later refresh refuses the switch
and reports an authentication error with a re-login hint.

## Authentication

```sh
instantctl auth login --profile example --username operator@example.org
instantctl auth status --profile example
instantctl --profile example device list
instantctl auth logout --profile example
```

Login prompts for a username, with the stored username as its default, and hides
password input. It requests an authenticator code only after the server requires
one. A single available site becomes the profile default. With several sites,
choose one at the terminal or supply `--site <site-uuid>`. Later commands prefer
`--site` over the saved default for profile credentials, including device write
commands. Static-token commands retain explicit site selection or single-site
discovery where supported.

For login without a terminal, supply `--username` and `--password-stdin`.
The first stdin line is the password. Add `--otp-stdin` for an account that
requires an authenticator code; the next line is read only after that challenge.
Each input line is limited to 16,384 bytes. Secrets have no command-line argument.
At a terminal, omit the stdin secret flags so input uses the hidden prompts.
Use `--site` when the account has several sites, and do not combine login with
`--token-stdin`. If site discovery or selection fails after successful sign-in,
the issued credentials remain saved without a default site. Sign in again with
`--site`, or supply a site on later commands.

Status reads local metadata without refreshing or contacting the portal. It
reports profile, username, default site, access expiry as Unix seconds, and
`refresh_pending`, with no token values. A missing login or pending refresh exits
3. Logout attempts refresh-token revocation and deletes the local credential,
even if revocation fails. It is idempotent. The empty profile-lock file stays in
place so concurrent processes continue to use one lock inode.

## Raw API requests

The `api` command calls raw routes on the fixed portal origin. GET requests
send fields as query parameters. Use `-f key=value` for a string field or
`-F key=value` for a scalar (`true`, `false`, `null`, numbers, or a string).
`-F key=@file` reads a UTF-8 string from a file; `-F key=@-` reads one from
stdin. `--input file` sends file bytes as the request body, and `--input -`
reads body bytes from stdin. With `--input`, fields become query parameters;
without it, fields on a non-GET request become a JSON body. The default method
is GET with no fields and POST when fields are supplied. Use `-X` with
`GET`, `POST`, `PUT`, or `DELETE` to choose a method explicitly.

Non-GET requests only show a preview until you add `--apply`. Applying sends
the request once; a successful HTTP response means the request was sent, not
that the resulting resource state was verified. The CLI prompts before an
apply unless you use `--yes`. A request body, input, or set of fields is limited
to 4 MiB. One request input may come from stdin; stdin cannot also provide
`--token-stdin`, and applying stdin input requires `--yes`.

Sensitive request bodies and response fields are redacted by default. Add
`--show-secrets` to display them. Use that option only when you need to inspect
the full values.

For example, use a disposable test tenant and substitute its resource UUIDs:

```sh
instantctl api /sites/11111111-2222-3333-4444-555555555555/capabilities
instantctl api -X GET /sites -F limit=20
instantctl api /sites/11111111-2222-3333-4444-555555555555/timezone > timezone.json
# Edit timezoneIana in the complete fetched object, then preview and apply:
instantctl api -X PUT /sites/11111111-2222-3333-4444-555555555555/timezone \
  --input timezone.json
instantctl api -X PUT /sites/11111111-2222-3333-4444-555555555555/timezone \
  --input timezone.json --apply --yes
```

If a GET response signals more results through a count mismatch or pagination
marker, the command returns the available data with `complete: false`, writes
an incomplete-result note to stderr, and exits 4. It does not fetch another
page because the portal's cursor contract is not confirmed.

Commands refuse capabilities or resource shapes that the backend does not
provide. An unsupported result is not proof that a requested change occurred.

## Sites and stacks

Site lifecycle commands work at account scope and accept a UUID or exact unique
site name. They do not use the selected site's saved profile default:

```sh
instantctl site country
instantctl site create example-site --country US --timezone Etc/UTC
instantctl site rename example-site example-site-renamed
instantctl site clone 123e4567-e89b-12d3-a456-426614174000 example-site --country US --timezone Etc/UTC
instantctl site delete example-site-renamed
instantctl site delete example-site-renamed --apply --yes \
  --confirm-name example-site-renamed
instantctl --site <site-uuid> stack list
instantctl --site <site-uuid> stack show <stack-id-or-name>
```

Lifecycle commands print a plan by default. Creation and cloning require a
country explicitly listed by the public country endpoint and a known IANA
timezone. The request includes no devices or gateways. Apply sends one write;
creation readback uses only the UUID returned by the server. Rename preserves
the fresh administration object and changes only its name. Every deletion,
including a default site, requires both `--yes` and its exact current name in
`--confirm-name`. A direct site GET returning 404 verifies deletion; uncertain
readback exits 6. No write is retried.

`site country` needs no credentials. Stack list and show preserve reported
member identities and unknown roles. Stack changes need hardware verification
and have no commands in this release.

## Site settings

Site reads return the portal's complete resource object:

```sh
instantctl --site <site-uuid> site health
instantctl --site <site-uuid> site dashboard
instantctl --site <site-uuid> site topology
instantctl --site <site-uuid> site timezone
instantctl --site <site-uuid> site management-network
instantctl --site <site-uuid> site dns
instantctl --site <site-uuid> site spanning-tree
instantctl --site <site-uuid> site extend-network
```

Supply setting values to plan a change. Add `--apply` to send it once and verify
fresh readback, with interactive confirmation or `--yes`:

```sh
instantctl --site <site-uuid> site timezone Etc/UTC
instantctl --site <site-uuid> site management-network --vlan 42
instantctl --site <site-uuid> site dns --mode custom --primary 192.0.2.53
instantctl --site <site-uuid> site spanning-tree --rstp true --priority 4096
instantctl --site <site-uuid> site extend-network --enabled true --outdoor-mesh false
```

Settings writes preserve the complete fetched object and change only the
requested fields. Readback verifies those fields; unrelated metrics can change.
Missing or invalid current setting values refuse a write. Management VLANs are
1 through 4092, excluding reserved VLANs 3333 through 3349. STP priorities are
0 through 61440 in steps of 4096. The management VLAN change preserves the raw
subnet and DNS configuration. Timezones must exist in the IANA database.

## Radio and network configuration

Radio configuration reads and plans use the portal's site plan and AP overrides:

```sh
instantctl --site <site-uuid> radio plan get
instantctl --site <site-uuid> radio override get <ap-mac-or-name>
instantctl --site <site-uuid> radio plan set --band 5ghz --width 40mhz \
  --channels 36,44 --min-power 15dbm --max-power 21dbm
instantctl --site <site-uuid> radio override set <ap-mac-or-name> \
  --band 5ghz --channels 149
instantctl --site <site-uuid> radio override set <ap-mac-or-name> --band 5ghz --inherit
instantctl --site <site-uuid> radio override set <ap-mac-or-name> \
  --band-mapping 2.4ghz_and_6ghz
instantctl --site <site-uuid> radio override set <ap-mac-or-name> --inherit-band-mapping
```

`radio list` continues to report operational telemetry. The new reads show
configured ranges, offered channels and inheritance flags. Missing or unknown
values stay null. Configuration changes require `--band`; a band mapping has
its own inheritance flag. Restoring inheritance keeps the saved AP configuration
and mapping. Channel selections must belong to the freshly fetched offered list
for the chosen width. Wider channels and band mapping require the portal's
reported capabilities. Power bounds use the portal's band-specific choices,
not measured EIRP. Specific AP settings require a known sufficient-power state;
mesh uplinks cannot override 5 or 6 GHz settings. A mapping-capable AP can
configure only bands enabled by its effective mapping. Updates preserve the
full fetched object, send one PUT with
`--apply`, and verify the requested configuration fields through fresh readback.
This verification does not prove RF coverage or configuration synchronization.

Plan radio changes for a maintenance window and confirm configuration sync and
uplink health afterward. The CLI enforces the returned channel offers.

`site stp-auto-priority` previews the site-wide bridge-priority action. Applying
it sends one request and reads every reported switch's priority again. The portal
has no completion marker or predictable result for this action, so the report
stays `unverified` (exit 6) even when fresh priorities are available. Missing
priorities remain null. This action can change spanning-tree topology.

`network routing <network-id-or-name>` reads the routing configuration and
observed subnet. Add `--enabled true|false` to plan a toggle. A toggle requires
the selected network's live `isManagement` field to be false and the site's live
permissions to include `inventory_update_all`; otherwise it exits 4. Applying
preserves the full network object and verifies the requested routing flag.

`site dns` returns the management subnet's typed DNS configuration, including
unknown fields and modes. DNS edits use the same complete management-network
object. Modes are `automatic`, `infrastructure`, and `custom`; custom mode
requires an IPv4 `--primary` server. In custom mode, omitting `--secondary`
clears the previous secondary server. Other modes preserve saved custom servers.

## Devices

`instantctl --site <site-uuid> device locate <mac-or-name> on|off` reads
inventory and prints the current and desired state to stderr. This command
sends no write by default; `--yes` alone also sends no write. Add `--apply`
to send one action and poll fresh inventory until the state matches or the
timeout expires. Applying requires an interactive confirmation or `--yes`.
Piped stdin and `--token-stdin` require `--yes` when applying.
Unknown capability or state, incomplete inventory, and ambiguous names refuse
the operation.

Device write commands use the same plan, confirmation, and single-write policy:

```sh
instantctl --site <site-uuid> device rename <mac-or-name> 'AP name '
instantctl --site <site-uuid> device led <mac-or-name> quiet
instantctl --site <site-uuid> device reboot <mac-or-name> --readback-timeout 300
instantctl --site <site-uuid> device forget <mac-or-name>
instantctl --site <site-uuid> device reserve-ip <mac-or-name> 192.0.2.45 --network <network-id>
instantctl --site <site-uuid> device remove-ip-reservation <mac-or-name>
instantctl --site <site-uuid> device management-ip <mac-or-name> 192.0.2.10 \
  --prefix-length 24 --gateway 192.0.2.1 --dns 192.0.2.53
instantctl --site <site-uuid> device power-usage <mac-or-name>
instantctl --site <site-uuid> device details <mac-or-name>
```

Rename preserves spaces in the supplied name. Configuration writes preserve the
full selected inventory object and verify only the changed fields. Static IP
changes, reboot, and device removal support APs and switches, and refuse gateway
devices or unknown device types/roles. Switch inventory may omit the AP role.
Reboot readback waits up to 300 seconds by default, with progress on stderr.
Verification requires a strict uptime decrease below elapsed request time, or
an observed offline-to-online transition, followed by an active operational
state (or online status when that field is absent). The report retains
`restart_observed` even if return to service is still unverified. A device that is merely online,
with unchanged or unknown uptime, does not prove a reboot.

The API library fixes the origin to `https://portal.instant-on.hpe.com/api`,
rejects redirects and escaping paths, and bounds decoded responses to 4 MiB.
Each request uses `--timeout` (default 15 seconds). Only GET responses with
status 429 or 503 are retried, for at most three attempts. Retry-After delays
are clamped to 0–2 seconds; missing or invalid values use 0.25 seconds.
Transport errors and all writes have one attempt. Error output includes no
response bodies or request URLs. The mutation library also supports full-object
PUT preparation: select the object from complete inventory, preserve other fields,
send it once, and verify fresh readback. A single monotonic deadline bounds
the write and its readback polling.

Device reservations require consistent inventory and DHCP-scope state, a usable
address within the subnet (or its DHCP pool when a DHCP gateway is reported),
and no conflicting reservation,
active client lease, or device address. Remove an existing reservation before
adding a different one. `device replacement-candidates <mac-or-name>` only reads
potential replacements. Hardware replacement writes require separate hardware
verification. The portal's uplink-type action acknowledges an alert; it does
not provide a setter for a chosen connection type.

## Clients and policies

Read or edit client allowlists with `wlan allowlist <network-id-or-name>` or
`device allowlist <mac-or-name> --port <number>` (use `--trunk` for a trunk).
With no change flags these commands read the selected list. Add `--add <MAC>`
or `--remove <MAC>` to plan a change, and `--apply` to send it once. Adds require
known eligibility and capacity. Fresh readback verifies the client set and list
identity. Both commands preserve clients outside the requested change.

Mutation data includes `outcome`, `observed`, and `request_attempted`. A
successful request with matching readback is `verified`. A successful request
without matching readback is `unverified` (exit 6). A failed request with
matching readback is `request_failed_state_matches` and still exits nonzero;
the observation does not erase the request failure. Failed requests retain
their safe error details.

`policy list` and `policy show <id-or-exact-name>` inspect the complete policy
collection. They require the site's `policies` capability. Unknown policy fields
and enum values remain available in JSON. ACL and app-policy creation need a
live request capture before implementation.

`policy app-visibility get` reads application categorization. Use
`policy app-visibility set true|false` to plan a toggle, then `--apply --yes` to
send it once and verify the boolean by fresh readback. The full fetched object
is preserved. Planning and writing both read live capabilities and permissions:
`applicationCategoryUsageConfiguration_update_all` is required, and when the
site supports policies, a traffic policy that owns visibility blocks the toggle
with unsupported exit 4. Without the policies capability the toggle skips the
policy check, as the portal does. The capability matrix describes the current
evidence for this operation.

## Firmware and administration

`firmware window get` reads maintenance settings. `firmware window set`
accepts `--day monday|...|sunday`, `--start-time HH:mm`, and `--delay-days`
of 0, 7, 14, 21, or 28. It preserves the complete fetched maintenance object
and verifies only the requested fields.

`firmware update-now` starts an available update. Its readback verifies that
maintenance began, not that the software upgrade completed. `firmware schedule
<YYYY-MM-DDTHH:mm:ss>` sends the portal's local timestamp without an offset;
readback requires the exact returned `updateScheduleDateTime`, and does not
infer timezone conversion. Both actions refuse an already active or unknown
maintenance state and recheck eligibility before the single POST.

Firmware mutations plan by default; `--apply` requires confirmation or `--yes`.
The capability matrix describes the current evidence for each operation.

`admin list`, `admin permissions`, and `admin check-account <email>` read site
administration data. Add, remove, change-role, and maintenance-mode changes use
the same plan and apply flow. Removing or demoting the final active administrator
is refused, including when the account list changes after planning. Pending
invitations can be removed by exact email. Unknown roles and safety flags refuse
account changes.

`admin support-token --output <new-file>` saves a token only after an applied
request has matching fresh readback. The file has mode 0600 and existing files
are never overwritten. Tokens are redacted from normal output. Lock and unlock
commands are limited to the behavior described by the capability matrix.

## Write safety and verification

Mutating commands show a plan and send no write until `--apply` is supplied.
An apply requires interactive confirmation or `--yes`. Each command sends one
write attempt. Commands that support verification make fresh reads and report
whether the requested state was observed. Some actions have no reliable
readback; see [Capabilities](../CAPABILITIES.md) before using them.

## Output and exit codes

Output defaults to a table on a terminal and JSON when piped. Use
`--format json|yaml|table` to override it. Data goes to stdout and errors go
to stderr. JSON errors contain `kind` and `message`. See the full
[output and exit-code guide](output.md).

| Exit code | Meaning |
| --- | --- |
| 0 | Success |
| 1 | General error |
| 2 | Configuration, usage, or confirmation required |
| 3 | Authentication error |
| 4 | Not found, unsupported, or incomplete API result |
| 5 | Client error or retry later |
| 6 | Write not verified by readback |
| 141 | Broken stdout pipe |

Dependency versions are pinned in the workspace and Cargo.lock. CI checks
formatting, workspace build, Clippy, and all targets on Linux and macOS.
