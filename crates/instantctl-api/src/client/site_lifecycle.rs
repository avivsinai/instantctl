//! Site creation, cloning, rename, and deletion with identity-bound readback.

use std::{collections::HashSet, sync::Mutex};

use serde::Serialize;
use serde_json::{Value, json};

use super::{
    Client, access,
    reads::{self, Site},
};
use crate::{
    Error, ErrorKind, TokenSource,
    mutation::{FullObjectPut, Mutation, ObjectResource, Plan},
    site::{Change as SiteChange, Resource as SiteResource},
};

#[derive(Clone, Debug)]
pub struct NewSite {
    pub name: String,
    pub country: String,
    pub timezone: String,
}

impl NewSite {
    /// Validate the fields before credentials or portal requests are needed.
    pub fn validate(&self) -> Result<(), Error> {
        validate_name(&self.name)?;
        if self.country.len() != 2 || !self.country.bytes().all(|byte| byte.is_ascii_alphabetic()) {
            return Err(Error::new(
                ErrorKind::Usage,
                "country must be a two-letter country code",
            ));
        }
        SiteChange::Timezone(self.timezone.clone()).validate()?;
        Ok(())
    }

    fn validated(&self) -> Result<ValidatedSite, Error> {
        self.validate()?;
        Ok(ValidatedSite {
            name: self.name.clone(),
            country: self.country.to_ascii_uppercase(),
            timezone: self.timezone.clone(),
        })
    }
}

/// Validate a site name using the portal's observed 64-character limit.
pub fn validate_name(name: &str) -> Result<(), Error> {
    if access::name_valid(name, 64) {
        Ok(())
    } else {
        Err(Error::new(
            ErrorKind::Usage,
            "site name must contain 1 to 64 characters and no control characters",
        ))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum State {
    Absent,
    Present {
        site_name: String,
        country: Option<String>,
        timezone: Option<String>,
    },
}

impl State {
    fn named(site_name: String) -> Self {
        Self::Present {
            site_name,
            country: None,
            timezone: None,
        }
    }

    fn created(site: &ValidatedSite) -> Self {
        Self::Present {
            site_name: site.name.clone(),
            country: Some(site.country.clone()),
            timezone: Some(site.timezone.clone()),
        }
    }
}

struct ValidatedSite {
    name: String,
    country: String,
    timezone: String,
}

type NameObserve = Box<dyn Fn(&Value) -> Result<State, Error> + Send + Sync>;
type RenameBackend<'a, T> = FullObjectPut<Administration<'a, T>, State, NameObserve>;

enum Action<'a, T> {
    Create {
        path: String,
        source_id: Option<String>,
        site: ValidatedSite,
        known_ids: HashSet<String>,
        created_id: Mutex<Option<String>>,
    },
    Rename(RenameBackend<'a, T>),
    Delete {
        site_id: String,
        confirm_name: Option<String>,
    },
}

/// A prepared one-send site lifecycle operation.
pub struct SiteMutation<'a, T> {
    client: &'a Client<T>,
    action: Action<'a, T>,
    desired: State,
    target: Value,
}

impl<'a, T: TokenSource> SiteMutation<'a, T> {
    /// Prepare a site creation using only the confirmed create serializer fields.
    pub async fn create(
        client: &'a Client<T>,
        site: NewSite,
    ) -> Result<(Self, Plan<State>), Error> {
        let site = site.validated()?;
        // Site creation requires a login even though country metadata is public.
        // Resolve it first so a public GET cannot mask a missing saved profile.
        client.source.token().await?;
        ensure_country_supported(client, &site).await?;
        let known_ids = known_site_ids(client).await?;
        Ok(Self::create_plan(
            client,
            "/initialSetup".into(),
            None,
            site,
            known_ids,
            json!({"operation":"site.create"}),
        ))
    }

    /// Prepare a rename for a UUID or exact unique account site name.
    pub async fn rename(
        client: &'a Client<T>,
        selector: &str,
        new_name: impl Into<String>,
    ) -> Result<(Self, Plan<State>), Error> {
        let new_name = new_name.into();
        validate_name(&new_name)?;
        let site = resolve_site(client, selector).await?;
        let site_id = canonical_id(&site.id)?;
        let account = read_account_site(client, &site_id).await?;
        let current = read_administration(client, &site_id, None).await?;
        check_names(&account, &current)?;
        let current_name = current
            .get("siteName")
            .and_then(Value::as_str)
            .filter(|name| access::name_valid(name, 64))
            .ok_or_else(|| unverified("site administration name is missing or invalid"))?
            .to_owned();
        let kind = current.get("kind").cloned();
        let observe_id = site_id.clone();
        let observe_kind = kind.clone();
        let observe: NameObserve = Box::new(move |value| {
            check_administration_identity(value, &observe_id, observe_kind.as_ref())?;
            let name = value
                .get("siteName")
                .and_then(Value::as_str)
                .filter(|name| access::name_valid(name, 64))
                .ok_or_else(|| unverified("site administration name is missing or invalid"))?;
            Ok(State::named(name.to_owned()))
        });
        let (backend, plan) = FullObjectPut::prepare(
            Administration {
                client,
                site_id: site_id.clone(),
                kind,
            },
            current,
            observe,
            |body| {
                body["siteName"] = Value::String(new_name.clone());
                Ok(())
            },
        )?;
        let target = json!({"operation":"site.rename","site_id":site_id,"site_name":current_name});
        Ok((
            Self {
                client,
                action: Action::Rename(backend),
                desired: State::named(new_name),
                target,
            },
            plan,
        ))
    }

    /// Prepare deletion; apply requires an exact name and rechecks it before sending.
    pub async fn delete(
        client: &'a Client<T>,
        selector: &str,
        confirm_name: Option<&str>,
    ) -> Result<(Self, Plan<State>), Error> {
        let site = resolve_site(client, selector).await?;
        let site_id = canonical_id(&site.id)?;
        let fresh = read_account_site(client, &site_id).await?;
        let fresh_name = fresh
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| unverified("site name is unavailable for delete confirmation"))?;
        if confirm_name.is_some_and(|confirmed| confirmed != fresh_name) {
            return Err(confirmation_required());
        }
        let current_name = fresh_name.to_owned();
        let current = State::named(current_name.clone());
        let plan = Plan {
            current,
            desired: State::Absent,
        };
        let target = json!({"operation":"site.delete","site_id":site_id,"site_name":current_name});
        Ok((
            Self {
                client,
                action: Action::Delete {
                    site_id,
                    confirm_name: confirm_name.map(str::to_owned),
                },
                desired: State::Absent,
                target,
            },
            plan,
        ))
    }

    /// Prepare a site clone using the same proven serializer as site creation.
    pub async fn clone(
        client: &'a Client<T>,
        source_selector: &str,
        site: NewSite,
    ) -> Result<(Self, Plan<State>), Error> {
        let site = site.validated()?;
        let source = resolve_site(client, source_selector).await?;
        let source_id = canonical_id(&source.id)?;
        read_account_site(client, &source_id).await?;
        ensure_country_supported(client, &site).await?;
        let known_ids = known_site_ids(client).await?;
        let path = format!("/sites/{source_id}/siteCloning");
        Ok(Self::create_plan(
            client,
            path,
            Some(source_id.clone()),
            site,
            known_ids,
            json!({"operation":"site.clone","source_site_id":source_id}),
        ))
    }

    fn create_plan(
        client: &'a Client<T>,
        path: String,
        source_id: Option<String>,
        site: ValidatedSite,
        known_ids: HashSet<String>,
        mut target: Value,
    ) -> (Self, Plan<State>) {
        target["site_name"] = json!(site.name);
        target["country"] = json!(site.country);
        target["timezone"] = json!(site.timezone);
        let desired = State::created(&site);
        let plan = Plan {
            current: State::Absent,
            desired: desired.clone(),
        };
        (
            Self {
                client,
                action: Action::Create {
                    path,
                    source_id,
                    site,
                    known_ids,
                    created_id: Mutex::new(None),
                },
                desired,
                target,
            },
            plan,
        )
    }

    pub fn target(&self) -> Value {
        self.target.clone()
    }
}

impl<T: TokenSource> Mutation for SiteMutation<'_, T> {
    type State = State;

    async fn read(&self) -> Result<Self::State, Error> {
        match &self.action {
            Action::Create { created_id, .. } => {
                let id = created_id
                    .lock()
                    .map_err(|_| unverified("created site identity lock failed"))?
                    .clone()
                    .ok_or_else(|| unverified("create acknowledgment has no valid site ID"))?;
                read_created_site(self.client, &id).await
            }
            Action::Rename(backend) => backend.read().await,
            Action::Delete { site_id, .. } => match read_account_site(self.client, site_id).await {
                Ok(site) => {
                    let name = site
                        .get("name")
                        .and_then(Value::as_str)
                        .ok_or_else(|| unverified("site name is missing during delete readback"))?;
                    Ok(State::named(name.to_owned()))
                }
                Err(error) if error.kind == ErrorKind::NotFound => Ok(State::Absent),
                Err(_) => Err(unverified("site deletion readback could not be verified")),
            },
        }
    }

    async fn write(&self, desired: &Self::State) -> Result<(), Error> {
        if desired != &self.desired {
            return Err(Error::new(
                ErrorKind::Config,
                "site mutation desired state does not match its prepared plan",
            ));
        }
        match &self.action {
            Action::Create {
                path,
                source_id,
                site,
                known_ids,
                created_id,
            } => {
                ensure_country_supported(self.client, site).await?;
                if let Some(source_id) = source_id {
                    read_account_site(self.client, source_id).await?;
                }
                let response = self.client.create(path, &create_body(site)).await?;
                let returned_id = response
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|id| reads::valid_site_id(id))
                    .map(str::to_ascii_lowercase)
                    .filter(|id| !known_ids.contains(id));
                *created_id
                    .lock()
                    .map_err(|_| unverified("created site identity lock failed"))? = returned_id;
                Ok(())
            }
            Action::Rename(backend) => backend.write(desired).await,
            Action::Delete {
                site_id,
                confirm_name,
            } => {
                let confirm_name = confirm_name.as_deref().ok_or_else(confirmation_required)?;
                let fresh = match read_account_site(self.client, site_id).await {
                    Ok(fresh) => fresh,
                    Err(error) if error.kind == ErrorKind::NotFound => return Ok(()),
                    Err(error) => return Err(error),
                };
                if fresh.get("name").and_then(Value::as_str) != Some(confirm_name) {
                    return Err(confirmation_required());
                }
                match self.client.delete(&format!("/sites/{site_id}")).await {
                    Ok(_) => Ok(()),
                    Err(error) if error.kind == ErrorKind::NotFound => Ok(()),
                    Err(error) => Err(error),
                }
            }
        }
    }
}

struct Administration<'a, T> {
    client: &'a Client<T>,
    site_id: String,
    kind: Option<Value>,
}

impl<T: TokenSource> Administration<'_, T> {
    async fn read(&self) -> Result<Value, Error> {
        let account = read_account_site(self.client, &self.site_id).await?;
        let administration =
            read_administration(self.client, &self.site_id, self.kind.as_ref()).await?;
        check_names(&account, &administration)?;
        Ok(administration)
    }
}

impl<T: TokenSource> ObjectResource for Administration<'_, T> {
    async fn read_object(&self) -> Result<Value, Error> {
        self.read().await
    }

    async fn put_object(&self, body: &Value) -> Result<(), Error> {
        check_administration_identity(body, &self.site_id, self.kind.as_ref())?;
        let desired_name = body
            .get("siteName")
            .and_then(Value::as_str)
            .filter(|name| access::name_valid(name, 64))
            .ok_or_else(|| unverified("site administration name is missing or invalid"))?
            .to_owned();
        let mut current = self.read().await?;
        current["siteName"] = Value::String(desired_name);
        let response = self
            .client
            .put_full(&format!("/sites/{}/administration", self.site_id), &current)
            .await?;
        check_administration_identity(&response, &self.site_id, self.kind.as_ref())?;
        Ok(())
    }
}

async fn read_created_site<T: TokenSource>(client: &Client<T>, id: &str) -> Result<State, Error> {
    let account = read_account_site(client, id).await?;
    let administration = read_administration(client, id, None).await?;
    check_names(&account, &administration)?;
    let site_name = administration
        .get("siteName")
        .and_then(Value::as_str)
        .ok_or_else(|| unverified("created site name is missing from readback"))?;
    let country = administration
        .get("regulatoryDomain")
        .and_then(Value::as_str)
        .ok_or_else(|| unverified("created site country is missing from readback"))?;
    let timezone_resource = client.site_resource(id, SiteResource::Timezone).await?;
    let timezone = timezone_resource
        .get("timezoneIana")
        .and_then(Value::as_str)
        .ok_or_else(|| unverified("created site timezone is missing from readback"))?;
    Ok(State::Present {
        site_name: site_name.to_owned(),
        country: Some(country.to_owned()),
        timezone: Some(timezone.to_owned()),
    })
}

async fn read_account_site<T: TokenSource>(client: &Client<T>, id: &str) -> Result<Value, Error> {
    let site = client.get(&format!("/sites/{id}")).await?;
    if !site
        .get("id")
        .and_then(Value::as_str)
        .is_some_and(|reported| reads::valid_site_id(reported) && reported.eq_ignore_ascii_case(id))
    {
        return Err(unverified("account site identity is missing or changed"));
    }
    Ok(site)
}

async fn read_administration<T: TokenSource>(
    client: &Client<T>,
    id: &str,
    kind: Option<&Value>,
) -> Result<Value, Error> {
    let administration = client.get(&format!("/sites/{id}/administration")).await?;
    if !administration.is_object() {
        return Err(unverified("site administration response is not an object"));
    }
    check_administration_identity(&administration, id, kind)?;
    Ok(administration)
}

fn check_names(account: &Value, administration: &Value) -> Result<(), Error> {
    let name = account
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| access::name_valid(name, 64))
        .ok_or_else(|| unverified("account site name is missing or invalid"))?;
    if administration.get("siteName").and_then(Value::as_str) != Some(name) {
        return Err(unverified("site name differs across readback resources"));
    }
    Ok(())
}

fn check_administration_identity(
    administration: &Value,
    id: &str,
    kind: Option<&Value>,
) -> Result<(), Error> {
    if !administration.is_object()
        || !returned_id_matches(administration, id)
        || kind.is_some_and(|expected| administration.get("kind") != Some(expected))
    {
        return Err(unverified("site administration identity changed"));
    }
    Ok(())
}

fn returned_id_matches(value: &Value, expected: &str) -> bool {
    match value.get("id") {
        None => true,
        Some(Value::String(id)) => id.eq_ignore_ascii_case(expected),
        Some(_) => false,
    }
}

async fn resolve_site<T: TokenSource>(client: &Client<T>, selector: &str) -> Result<Site, Error> {
    let sites = client.sites().await?;
    if reads::valid_site_id(selector) {
        return sites
            .into_iter()
            .find(|site| site.id.eq_ignore_ascii_case(selector))
            .ok_or_else(|| Error::new(ErrorKind::NotFound, "no site matches the supplied UUID"));
    }
    let mut matching = sites
        .into_iter()
        .filter(|site| site.name.as_deref() == Some(selector));
    let site = matching
        .next()
        .ok_or_else(|| Error::new(ErrorKind::NotFound, "no site matches the supplied name"))?;
    if matching.next().is_some() {
        return Err(Error::new(
            ErrorKind::Usage,
            "site name is ambiguous; select by UUID",
        ));
    }
    Ok(site)
}

async fn known_site_ids<T: TokenSource>(client: &Client<T>) -> Result<HashSet<String>, Error> {
    Ok(client
        .sites()
        .await?
        .into_iter()
        .map(|site| site.id.to_ascii_lowercase())
        .collect())
}

fn canonical_id(id: &str) -> Result<String, Error> {
    if !reads::valid_site_id(id) {
        return Err(unverified("site identity is not a valid UUID"));
    }
    Ok(id.to_ascii_lowercase())
}

async fn ensure_country_supported<T: TokenSource>(
    client: &Client<T>,
    site: &ValidatedSite,
) -> Result<(), Error> {
    let country = client.country().await?;
    if country
        .supported_country_codes
        .as_ref()
        .is_some_and(|codes| {
            codes
                .iter()
                .any(|code| code.eq_ignore_ascii_case(&site.country))
        })
    {
        Ok(())
    } else {
        Err(Error::new(
            ErrorKind::Unsupported,
            "country is not explicitly supported by the portal",
        ))
    }
}

fn create_body(site: &ValidatedSite) -> Value {
    json!({
        "siteName":site.name,
        "regulatoryDomain":site.country,
        "timezoneIana":site.timezone,
    })
}

fn confirmation_required() -> Error {
    Error::new(
        ErrorKind::ConfirmationRequired,
        "deleting a site requires its exact current name confirmation",
    )
}

fn unverified(message: &'static str) -> Error {
    Error::new(ErrorKind::Unverified, message)
}
