//! Shared site selection and projections for the read noun commands.
use crate::context::{CommandContext, PortalClient};
use instantctl_api::client::reads::{ClientSummary, Device, Site};
pub(crate) use instantctl_api::client::reads::{is_mac as mac, valid_site_id};
use instantctl_api::{Error, ErrorKind};
use serde::Serialize;
use serde_json::Value;

pub(crate) fn config(message: &str) -> Error {
    Error::new(ErrorKind::Config, message)
}
pub(crate) fn incomplete(message: &str) -> Error {
    Error::new(ErrorKind::Unverified, message)
}
pub(crate) fn check_site(context: &CommandContext) -> Result<(), Error> {
    if context
        .site
        .as_deref()
        .is_some_and(|site| !valid_site_id(site))
    {
        return Err(config("--site must be a UUID"));
    }
    Ok(())
}
pub(crate) fn client(context: &CommandContext) -> Result<PortalClient, Error> {
    check_site(context)?;
    context.client()
}
pub(crate) async fn sites(api: &PortalClient) -> Result<Vec<Site>, Error> {
    api.sites().await
}
pub(crate) fn selected_site(context: &CommandContext, sites: &[Site]) -> Result<String, Error> {
    check_site(context)?;
    if let Some(site) = &context.site {
        return Ok(site.clone());
    }
    match sites {
        [site] => Ok(site.id.clone()),
        [] => Err(Error::new(ErrorKind::NotFound, "no sites are available")),
        _ => Err(config(
            "multiple sites are available; select one with --site <UUID>",
        )),
    }
}
pub(crate) async fn site_id(api: &PortalClient, context: &CommandContext) -> Result<String, Error> {
    check_site(context)?;
    if let Some(site) = &context.site {
        return Ok(site.clone());
    }
    selected_site(context, &api.sites().await?)
}
pub(crate) async fn inventory(
    api: &PortalClient,
    context: &CommandContext,
) -> Result<Vec<Device>, Error> {
    let site = site_id(api, context).await?;
    api.inventory(&site).await
}
pub(crate) async fn clients(
    api: &PortalClient,
    context: &CommandContext,
) -> Result<Vec<ClientSummary>, Error> {
    let site = site_id(api, context).await?;
    api.clients(&site).await
}
pub(crate) fn values<T: Serialize>(models: &[T]) -> Result<Vec<Value>, Error> {
    models
        .iter()
        .map(|model| {
            let mut value = serde_json::to_value(model)
                .map_err(|_| incomplete("could not represent read response"))?;
            // Missing optional collections cannot provide evidence of a device's role.
            if let Some(object) = value.as_object_mut() {
                object.retain(|_, value| !value.is_null());
            }
            Ok(value)
        })
        .collect()
}
pub(crate) fn select<'a>(
    items: &'a [Value],
    selector: &str,
    noun: &str,
) -> Result<&'a Value, Error> {
    let mut matches = items.iter().filter(|item| {
        if mac(selector) {
            ["id", "macAddress"].iter().any(|key| {
                item.get(key)
                    .and_then(Value::as_str)
                    .is_some_and(|value| value.eq_ignore_ascii_case(selector))
            })
        } else {
            item.get("name").and_then(Value::as_str) == Some(selector)
        }
    });
    let item = matches.next().ok_or_else(|| {
        Error::new(
            ErrorKind::NotFound,
            format!("no {noun} matches the supplied selector"),
        )
    })?;
    if matches.next().is_some() {
        return Err(Error::new(
            ErrorKind::Usage,
            format!("{noun} name is ambiguous; select by MAC address"),
        ));
    }
    Ok(item)
}

pub(crate) fn project(item: &Value, fields: &[(&str, &str)]) -> Value {
    Value::Object(
        fields
            .iter()
            .map(|(column, source)| {
                (
                    (*column).to_owned(),
                    item.get(*source).cloned().unwrap_or(Value::Null),
                )
            })
            .collect(),
    )
}

pub(crate) fn array<'a>(item: &'a Value, field: &str) -> Result<&'a [Value], Error> {
    item.get(field)
        .and_then(Value::as_array)
        .filter(|items| items.iter().all(Value::is_object))
        .map(Vec::as_slice)
        .ok_or_else(|| incomplete("device has no complete requested collection"))
}

#[cfg(test)]
pub(crate) mod tests;
