//! Policy reads and application visibility. Portal bundle byte offsets:
//! policies @5097253, visibility body @4975106, permissions @4929094.
//! Visibility UI permission/policy guard: chunk-GGK5P27H.js @2077/@3557.
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{access, reads};
use crate::{
    Client, Error, ErrorKind, TokenSource,
    mutation::{FullObjectPut, Mutation, ObjectResource, Plan},
};

const VISIBILITY: &str = "applicationCategoryUsageConfiguration";
pub const VISIBILITY_PERMISSION: &str = "applicationCategoryUsageConfiguration_update_all";
pub const POLICIES_CAPABILITY: &str = "policies";

/// Preserve unknown policy fields and wire enum values for inspection.
pub async fn list<T: TokenSource>(client: &Client<T>, site: &str) -> Result<Vec<Value>, Error> {
    if !client
        .capabilities(site)
        .await?
        .iter()
        .any(|cap| cap == POLICIES_CAPABILITY)
    {
        return Err(unsupported(POLICIES_CAPABILITY));
    }
    read_policies(client, site).await
}

async fn read_policies<T: TokenSource>(
    client: &Client<T>,
    site: &str,
) -> Result<Vec<Value>, Error> {
    let payload = client.get(&access::path(site, "policies")?).await?;
    if payload
        .get("isApiVersionIncompatible")
        .is_some_and(|value| !matches!(value, Value::Null | Value::Bool(false)))
    {
        return Err(access::incomplete(
            "policy API version is incompatible or unknown",
        ));
    }
    let rows = payload
        .get("policies")
        .and_then(Value::as_array)
        .ok_or_else(|| access::incomplete("policies response has no complete policies list"))?;
    let mut collection = payload.clone();
    collection["elements"] = Value::Array(rows.clone());
    access::parse(&collection).map(|rows| rows.into_iter().map(access::safe).collect())
}

pub fn show(rows: &[Value], selector: &str) -> Result<Value, Error> {
    validate_selector(selector)?;
    access::select(rows, selector).cloned()
}

pub fn validate_selector(selector: &str) -> Result<(), Error> {
    if selector.trim().is_empty() || selector.chars().any(char::is_control) {
        return Err(access::usage("policy selector must be an ID or exact name"));
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Visibility {
    pub is_application_categorization_enabled: bool,
}

async fn require_visibility<T: TokenSource>(client: &Client<T>, site: &str) -> Result<(), Error> {
    let capabilities = client.capabilities(site).await?;
    #[derive(Deserialize)]
    struct Permission {
        permission: String,
    }
    #[derive(Deserialize)]
    struct Permissions {
        permissions: Vec<Permission>,
    }
    let permissions: Permissions =
        serde_json::from_value(client.get(&access::path(site, "permissions")?).await?)
            .map_err(|_| access::incomplete("site permissions response is missing or malformed"))?;
    if !permissions
        .permissions
        .iter()
        .any(|permission| permission.permission == VISIBILITY_PERMISSION)
    {
        return Err(unsupported(VISIBILITY_PERMISSION));
    }
    if capabilities.iter().any(|cap| cap == POLICIES_CAPABILITY) {
        for policy in read_policies(client, site).await? {
            let policy_type = policy
                .get("policyType")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    access::incomplete(
                        "policy type is missing or malformed; visibility ownership is unknown",
                    )
                })?;
            if ![
                "network",
                "traffic",
                "poe",
                "portForwarding",
                "clientTraffic",
                "networkRouting",
                "networkFirewall",
            ]
            .contains(&policy_type)
            {
                return Err(access::incomplete(
                    "policy type is unknown; visibility ownership is unknown",
                ));
            }
            if policy_type == "traffic" {
                let name = policy
                    .get("name")
                    .and_then(Value::as_str)
                    .filter(|name| !name.is_empty())
                    .unwrap_or_else(|| policy["id"].as_str().unwrap_or_default());
                return Err(Error::new(
                    ErrorKind::Unsupported,
                    format!("unsupported: application visibility is controlled by policy {name}"),
                ));
            }
        }
    }
    Ok(())
}

fn unsupported(required: &str) -> Error {
    Error::new(
        ErrorKind::Unsupported,
        format!("unsupported: missing capability or permission {required}"),
    )
}

fn observe(body: &Value) -> Result<Visibility, Error> {
    if body.get("kind").and_then(Value::as_str) != Some(VISIBILITY) {
        return Err(access::incomplete(
            "application visibility kind is missing or unsupported",
        ));
    }
    serde_json::from_value(body.clone())
        .map_err(|_| access::incomplete("application visibility state is missing or malformed"))
}

struct VisibilityResource<'a, T> {
    client: &'a Client<T>,
    site: &'a str,
    path: String,
}
impl<T: TokenSource> ObjectResource for VisibilityResource<'_, T> {
    async fn read_object(&self) -> Result<Value, Error> {
        self.client.get(&self.path).await
    }
    async fn put_object(&self, body: &Value) -> Result<(), Error> {
        // Recheck immediately before the write: support may have changed since planning.
        require_visibility(self.client, self.site).await?;
        self.client.put_full(&self.path, body).await.map(|_| ())
    }
}

pub async fn set_visibility<'a, T: TokenSource>(
    client: &'a Client<T>,
    site: &'a str,
    enabled: bool,
) -> Result<(impl Mutation<State = Visibility> + 'a, Plan<Visibility>), Error> {
    if !reads::valid_site_id(site) {
        return Err(Error::new(ErrorKind::Config, "--site must be a UUID"));
    }
    require_visibility(client, site).await?;
    let resource = VisibilityResource {
        client,
        site,
        path: access::path(site, VISIBILITY)?,
    };
    let body = resource.read_object().await?;
    FullObjectPut::prepare(resource, body, observe, |body| {
        body["isApplicationCategorizationEnabled"] = Value::Bool(enabled);
        Ok(())
    })
}
