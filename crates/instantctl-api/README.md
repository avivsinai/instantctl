# instantctl-api

Rust client library for HPE Networking Instant On. This independent project
uses the portal API and is not affiliated with HPE. The portal API can change
without notice.

## Add the library

The crate is not published on crates.io. Add the tagged Git source to your
application's `Cargo.toml`:

```toml
[dependencies]
instantctl-api = { git = "https://github.com/avivsinai/instantctl", tag = "v0.1.0" }
tokio = { version = "1", features = ["macros", "rt"] }
```

## Read sites

Supply a portal access token through your credential manager or environment.
`StaticToken` uses the supplied token without refreshing it; `Client` sets a
request timeout and uses the fixed HPE portal origin. This example reads sites
and sends no configuration writes:

```rust,no_run
use std::time::Duration;

use instantctl_api::{Client, StaticToken};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let token = StaticToken::new(std::env::var("HPE_INSTANT_ON_TOKEN")?)?;
    let client = Client::new(token, Duration::from_secs(20))?;

    for site in client.sites().await? {
        println!("{}: {}", site.id, site.name.as_deref().unwrap_or("<unnamed>"));
    }
    Ok(())
}
```

The example is compiled by `cargo test --doc --locked -p instantctl-api`;
it is not run against a live account. Site responses contain private account
data. Do not attach the output or tokens to public issues.

## API and safety

`Client::inventory` and `Client::clients` read a selected site.
`TokenSource` supports caller-owned token providers; `RefreshingTokenSource`
supports the library's stored-profile refresh flow. Generate the full Rust
API reference from a checkout with:

```sh
cargo doc --locked --no-deps -p instantctl-api --open
```

Library writes do not pass through the CLI's `--apply` or confirmation prompt.
The application owns authorization, target selection, and confirmation. Read
the [write safety and verification guide](https://github.com/avivsinai/instantctl/blob/main/docs/commands.md#write-safety-and-verification)
before using mutation methods, and distinguish a successful request from
verified readback. The [capability matrix](https://github.com/avivsinai/instantctl/blob/main/CAPABILITIES.md)
states implementation and live-evidence limits.

See the [project documentation](https://github.com/avivsinai/instantctl) for
CLI installation, authentication, output formats, and security policy.

Licensed under MIT.
