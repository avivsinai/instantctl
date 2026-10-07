//! Site-wide RADIUS settings for switch-port authentication. Port modes are
//! device configuration; this singleton's serializer is pT @5097860.
use super::radius::{self, ServerPatch};
use super::{
    Client, Error, Plan, SettingsMutation, State, TokenSource, incomplete, path, safe, usage,
};
use serde_json::{Value, json};

const RESOURCE: &str = "portAccessControlSettings";
#[derive(Clone, Debug, Default)]
pub struct Patch {
    pub accounting: Option<bool>,
    pub secondary_enabled: Option<bool>,
    pub primary: ServerPatch,
    pub secondary: ServerPatch,
}
impl Patch {
    pub fn validate(&self) -> Result<(), Error> {
        self.primary.validate()?;
        self.secondary.validate()
    }
    pub fn is_empty(&self) -> bool {
        self.accounting.is_none()
            && self.secondary_enabled.is_none()
            && self.primary.is_empty()
            && self.secondary.is_empty()
    }
    fn paths(&self) -> Vec<String> {
        let mut paths = Vec::new();
        if self.accounting.is_some() {
            paths.push("isRadiusAccountingEnabled".to_owned());
        }
        if self.secondary_enabled.is_some() {
            paths.push("isSecondaryRadiusServerEnabled".to_owned());
        }
        for (name, patch) in [
            ("radiusServerPrimary", &self.primary),
            ("radiusServerSecondary", &self.secondary),
        ] {
            if !patch.is_empty() {
                paths.extend(
                    [
                        "serverHost",
                        "sharedSecret",
                        "timeout",
                        "retryCount",
                        "authPort",
                        "accountingPort",
                    ]
                    .iter()
                    .map(|field| format!("{name}/{field}")),
                );
            }
        }
        paths
    }
    fn apply(&self, body: &mut Value) -> Result<(), Error> {
        self.primary
            .apply(&mut body["radiusServerPrimary"], false)?;
        self.secondary
            .apply(&mut body["radiusServerSecondary"], false)?;
        if let Some(value) = self.accounting {
            body["isRadiusAccountingEnabled"] = json!(value);
        }
        if let Some(value) = self.secondary_enabled {
            body["isSecondaryRadiusServerEnabled"] = json!(value);
        }
        if body["isRadiusAccountingEnabled"] == true
            || body["isSecondaryRadiusServerEnabled"] == true
        {
            radius::validate_server(&body["radiusServerPrimary"])?;
        }
        if body["isSecondaryRadiusServerEnabled"] == true {
            radius::validate_server(&body["radiusServerSecondary"])?;
        }
        Ok(())
    }
}
pub async fn show<T: TokenSource>(client: &Client<T>, site: &str) -> Result<Value, Error> {
    let mut body = client.get(&path(site, RESOURCE)?).await?;
    if !body.is_object() {
        return Err(incomplete("port access-control settings are unavailable"));
    }
    for field in [
        "isRadiusAccountingEnabled",
        "isSecondaryRadiusServerEnabled",
    ] {
        body[field] = json!(body.get(field).and_then(Value::as_bool));
    }
    Ok(safe(body))
}
pub async fn update<'a, T: TokenSource>(
    client: &'a Client<T>,
    site: &str,
    patch: Patch,
) -> Result<(SettingsMutation<'a, T>, Plan<State>), Error> {
    patch.validate()?;
    if patch.is_empty() {
        return Err(usage("specify at least one port access-control setting"));
    }
    radius::capabilities(
        client,
        site,
        &radius::Patch {
            primary: patch.primary.clone(),
            secondary: patch.secondary.clone(),
            ..radius::Patch::default()
        },
    )
    .await?;
    SettingsMutation::prepare(
        client,
        site,
        RESOURCE,
        |_| Ok(patch.paths()),
        |body| patch.apply(body),
    )
    .await
}
