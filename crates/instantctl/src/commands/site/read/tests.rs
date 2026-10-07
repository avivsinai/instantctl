use crate::{
    context::CommandContext,
    credentials::{CommandTokenSource, CredentialSource},
    output::Format,
};
use instantctl_api::client::reads::Device;
use instantctl_api::{Error, ErrorKind, StaticToken};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::time::Duration;
pub(crate) const SITE: &str = "12345678-1234-5678-1234-567812345678";
pub(crate) const MAC: &str = "aa:bb:cc:dd:ee:ff";
pub(crate) fn context(site: Option<&str>) -> CommandContext {
    CommandContext {
        profile: "test-site-read-nonexistent-profile-7b32".to_owned(),
        site: site.map(str::to_owned),
        timeout: Duration::from_secs(3),
        format: Format::Json,
        token_source: CredentialSource::Environment,
        prepared_token: Some(CommandTokenSource::Static(
            StaticToken::new("netcli-site-read-test-token").unwrap(),
        )),
        protected_ports: Vec::new(),
    }
}
pub(crate) fn device(name: &str) -> Value {
    json!({"id":MAC,"macAddress":MAC,"name":name})
}
pub(crate) fn model<T: DeserializeOwned>(value: Value) -> T {
    serde_json::from_value(value).unwrap()
}
pub(crate) fn devices(items: Vec<Value>) -> Vec<Device> {
    items.into_iter().map(model).collect()
}
pub(crate) fn error_kind(error: &anyhow::Error) -> ErrorKind {
    error.downcast_ref::<Error>().unwrap().kind
}

#[test]
fn site_selection_uses_an_explicit_uuid_or_the_unique_site() {
    use super::selected_site;
    let site = model(json!({"id":SITE,"name":"Home"}));
    assert_eq!(selected_site(&context(None), &[site]).unwrap(), SITE);
    assert_eq!(selected_site(&context(Some(SITE)), &[]).unwrap(), SITE);
    let default = "11111111-2222-3333-4444-555555555555";
    let profile_default = context(None).with_default_site(Some(default.to_owned()));
    assert_eq!(selected_site(&profile_default, &[]).unwrap(), default);
    let explicit_over_default = context(Some(SITE)).with_default_site(Some(default.to_owned()));
    assert_eq!(selected_site(&explicit_over_default, &[]).unwrap(), SITE);
    let sites = vec![
        model(json!({"id":SITE})),
        model(json!({"id":"11111111-2222-3333-4444-555555555555"})),
    ];
    assert_eq!(
        selected_site(&context(None), &sites).unwrap_err().kind,
        ErrorKind::Config
    );
    assert_eq!(
        selected_site(&context(None), &[]).unwrap_err().kind,
        ErrorKind::NotFound
    );
    assert_eq!(
        selected_site(&context(Some("../escape")), &sites)
            .unwrap_err()
            .kind,
        ErrorKind::Config
    );
}

#[tokio::test]
async fn every_read_noun_rejects_invalid_site_at_the_dispatch_boundary() {
    use clap::Parser;
    for arguments in [
        vec!["site", "list"],
        vec!["site", "show"],
        vec!["site", "capabilities"],
        vec!["device", "list"],
        vec!["device", "show", "AP"],
        vec!["device", "health", "192.0.2.1"],
        vec!["port", "list"],
        vec!["port", "show", "Switch", "1"],
        vec!["lag", "list"],
        vec!["radio", "list"],
        vec!["client", "list"],
        vec!["client", "show", "PC"],
    ] {
        let mut argv = vec!["instantctl", "--site", "../escape"];
        argv.extend(arguments);
        let cli = crate::cli::Cli::try_parse_from(argv).unwrap();
        let context = context(cli.site.as_deref());
        let mut output = Vec::new();
        let error = crate::commands::run(cli.command, &context, &mut output)
            .await
            .err()
            .unwrap();
        assert_eq!(error_kind(&error), ErrorKind::Config);
        assert_eq!(crate::exit::ExitStatus::Error(error_kind(&error)).code(), 2);
        assert!(error.to_string().contains("--site must be a UUID"));
        assert!(output.is_empty());
    }
}
