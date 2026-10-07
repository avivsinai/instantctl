# Contributing

Contributions are welcome. Before a large change, open a GitHub discussion or
issue to agree on the scope. Keep changes focused, document user-visible
behavior, and include a test for each behavior change.

## Checks

Run these checks from the repository root before opening a pull request:

```sh
cargo fmt --all -- --check
cargo build --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --all-targets --locked
cargo xtask man
```

Credential-free checks run in CI. Tests must not require a real HPE account,
portal credentials, or live writes. Do not add real credentials or private
tenant data to fixtures, logs, screenshots, or documentation.

If your change affects live reads, you may run the read-acceptance command with
an account you control and a saved profile:

```sh
cargo build -p instantctl --locked
cargo xtask live-read --binary target/debug/instantctl \
  --profile default --output "$HOME/instantctl-live-read.json"
```

This check contacts the portal and requires credentials. The output can contain
account and device data. Keep it outside the repository and do not attach it to
a public issue or pull request. No credentialed live write is required for CI
or contribution review. Report precisely which checks you ran; do not treat
fixtures or mock responses as live evidence.

## Pull requests

- Explain the user-visible problem and behavior.
- Include relevant tests and documentation.
- State the checks you ran and any live evidence separately.
- Avoid unrelated formatting or generated-file changes.
