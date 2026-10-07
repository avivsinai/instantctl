use std::time::Duration;

use instantctl_api::{Client, Error, ProfileMetadata};
use serde_json::Value;

use crate::{
    credentials::{CommandTokenSource, CredentialSource},
    exit::ExitStatus,
    output::Format,
};

pub type PortalClient = Client<CommandTokenSource>;

#[derive(Clone)]
pub struct CommandContext {
    pub profile: String,
    pub site: Option<String>,
    pub timeout: Duration,
    pub format: Format,
    pub token_source: CredentialSource,
    pub prepared_token: Option<CommandTokenSource>,
    pub protected_ports: Vec<String>,
}

impl CommandContext {
    pub fn client(&self) -> Result<PortalClient, Error> {
        let source = match &self.prepared_token {
            Some(source) => source.clone(),
            None => self.token_source.load(&self.profile, self.timeout)?,
        };
        Client::new(source, self.timeout)?.with_protected_ports(&self.protected_ports)
    }

    pub(crate) fn with_default_site(&self, default_site: Option<String>) -> Self {
        Self {
            site: self.site.clone().or(default_site),
            ..self.clone()
        }
    }

    pub(crate) async fn resolve_profile_default(&self) -> Result<Self, Error> {
        let source = self.token_source.load(&self.profile, self.timeout)?;
        let mut resolved = self.with_profile_metadata(source.profile_metadata().await?);
        resolved.prepared_token = Some(source);
        Ok(resolved)
    }

    fn with_profile_metadata(&self, metadata: ProfileMetadata) -> Self {
        let mut resolved = self.with_default_site(metadata.default_site);
        resolved.protected_ports = metadata.protected_ports;
        resolved
    }
}

pub struct CommandResult {
    pub data: Value,
    pub status: ExitStatus,
}

impl CommandResult {
    pub fn success(data: Value) -> Self {
        Self {
            data,
            status: ExitStatus::Success,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use instantctl_api::StaticToken;

    #[test]
    fn explicit_site_keeps_saved_profile_protection() {
        let context = CommandContext {
            profile: "work".into(),
            site: Some("explicit-site".into()),
            timeout: Duration::from_secs(1),
            format: Format::Json,
            token_source: CredentialSource::Environment,
            prepared_token: Some(CommandTokenSource::Static(
                StaticToken::new("token-sentinel").unwrap(),
            )),
            protected_ports: Vec::new(),
        };
        let resolved = context.with_profile_metadata(ProfileMetadata {
            default_site: Some("saved-site".into()),
            protected_ports: vec!["Switch:7".into()],
        });
        assert_eq!(resolved.site.as_deref(), Some("explicit-site"));
        assert_eq!(resolved.protected_ports, vec!["Switch:7"]);
        assert!(resolved.client().is_ok());
        let mut invalid = resolved;
        invalid.protected_ports = vec!["Switch:0".into()];
        assert_eq!(
            invalid.client().unwrap_err().kind,
            instantctl_api::ErrorKind::Config
        );
    }
}
