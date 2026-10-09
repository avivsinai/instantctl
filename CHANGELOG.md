# Changelog

Changes to this project are recorded here.

## 0.1.0 — 2026-10-07

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
