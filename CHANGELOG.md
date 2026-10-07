# Changelog

Changes to this project are recorded here.

## 0.1.0 — unreleased

This is the first public source release candidate. No crate, package-manager,
or binary release is published yet.

- Add the `instantctl` Rust CLI for HPE Instant On portal operations.
- Support saved login profiles on macOS and token input through stdin or the
  environment on macOS and Linux.
- Provide read commands and guarded configuration commands with preview,
  confirmation, single-write behavior, and fresh readback where supported.
- Generate shell completion and man-page output from the CLI definitions.
- Document supported commands, known gaps, output formats, and security
  reporting.
