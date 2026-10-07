# instantctl

`instantctl` is a command-line tool for reading and managing HPE Instant On
sites, switches, access points, wired networks, wireless networks, and clients.
It uses a reverse-engineered portal API, which can change without notice.
This is an independent project, not affiliated with or endorsed by HPE.

The CLI runs on macOS and Linux. Saved login profiles use macOS Keychain on
macOS. On Linux, provide a token through stdin or `HPE_INSTANT_ON_TOKEN`.

## Install from source

Install the CLI from a source checkout with Rust and Cargo available:

```sh
cargo install --path crates/instantctl --locked
instantctl --version
```

## Quick start

On macOS, sign in and list sites:

```sh
instantctl auth login --profile example --username operator@example.org
instantctl --profile example site list
```

On Linux, provide an access token through the environment or `--token-stdin`,
then run the same read command. For a token already set by your credential
manager or CI environment:

```sh
printf '%s\n' "$HPE_INSTANT_ON_TOKEN" | instantctl --token-stdin site list
```

Avoid putting tokens in shell history or command arguments.

Commands that change configuration show a plan first. For example, this
command previews a timezone change for the selected site and sends no write:

Replace `<site-uuid>` below with the UUID of a site in the selected account.

```sh
instantctl --profile example --site <site-uuid> site timezone Etc/UTC
```

Review the plan and the safety guidance in the
[command reference](docs/commands.md) before applying any change. An apply
requires `--apply` and confirmation, or the explicit `--yes` option. The CLI
sends each write once and uses fresh reads to verify supported changes.

## Output and safety

Interactive output defaults to a table; piped output defaults to JSON. Choose
`--format json|yaml|table` to set it explicitly. Data goes to stdout and errors
go to stderr. A successful write request does not by itself mean that the
change was verified. See [output formats and exit codes](docs/output.md) and
[write safety](docs/commands.md#write-safety-and-verification).

## Documentation

- [Capabilities and known gaps](CAPABILITIES.md)
- [Command reference](docs/commands.md)
- [Output formats and exit codes](docs/output.md)
- [Security policy](SECURITY.md)
- [Contributing](CONTRIBUTING.md)
- [Code of conduct](CODE_OF_CONDUCT.md)
- [Changelog](CHANGELOG.md)
- [MIT license](LICENSE)
