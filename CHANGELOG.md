# Changelog

Changes to this project are recorded here.

## [Unreleased]

## [0.1.3] - 2026-10-25
### Fixed
- The release workflow now fetches the freshly created tag before building the source tarball (v0.1.2's run failed there).


## [0.1.2] - 2026-10-25
### Fixed
- Update `rustls`, `webpki` and `quinn-proto` past current RustSec advisories.
- Update `tokio` to 1.53.3, `libc` to 0.2.191, `sha2` to 0.11.1 and
  `keyring-core` to 1.0.1.
- Fix flat capability flags in `instantctl radio`.


## [0.1.1] - 2026-10-20

Maintenance source release. The [GitHub release](https://github.com/avivsinai/instantctl/releases/tag/v0.1.1)
contains a source archive and SHA-256 checksum. Install the CLI from source;
no crate, package-manager, or prebuilt binary release is published yet.

- Update `sha2` to 0.11.0. SSO sign-in uses it to make the PKCE code challenge.
- Update `jiff` to 0.2.38 and `serde_json` to 1.0.154.
- Add a getting-started guide to the `instantctl-api` README. The crate
  documentation now uses this guide, and CI compiles its Rust example.
- Change the library Git dependency example to tag `v0.1.1`.

## [0.1.0] - 2026-10-07

First public source release. The [GitHub release](https://github.com/avivsinai/instantctl/releases/tag/v0.1.0)
contains a source archive and SHA-256 checksum. Install the CLI from source;
no crate, package-manager, or prebuilt binary release is published yet.

- Add the `instantctl` Rust CLI for HPE Instant On portal operations.
- Support saved login profiles on macOS and token input through stdin or the
  environment on macOS and Linux.
- Provide read commands and guarded configuration commands with preview,
  confirmation, single-write behavior, and fresh readback where supported.
- Generate shell completion and man-page output from the CLI definitions.
- Document supported commands, known gaps, output formats, and security
  reporting.
