//! Switch port profiles and site-wide PoE and energy settings.
//!
//! The request bodies and enum identifiers in this module follow the portal
//! serializers in `chunk-EFL5B25Z.js`. Full objects are retained for writes.

use std::{
    collections::{BTreeMap, HashSet},
    sync::Mutex,
};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::{Client, reads};
use crate::{
    Error, ErrorKind, TokenSource,
    device::Prepared,
    inventory::{inventory_elements, inventory_path},
    mutation::{FullObjectPut, Mutation, ObjectResource, Plan},
};

const PROFILE_FIELDS: &[&str] = &[
    "name",
    "kind",
    "customMappingUntaggedWiredNetworkId",
    "customMappingTaggedWiredNetworksSelection",
    "networkMapping",
    "protectedPortEnabled",
    "shouldTrustTraffic",
    "stormControlEnabled",
    "usePoeSchedule",
    "spanningTreeProtection",
    "portAssignmentByDeviceId",
];
const PROFILE_CREATE_OPTIONAL_FIELDS: &[&str] = &[
    "speedDuplexMode",
    "speedDuplex",
    "poePowerManagementMode",
    "poePriority",
    "poePowerMode",
    "accessControlEnabled",
    "portControlType",
    "macAuthenticationEnabled",
    "unauthenticatedGuestAccessAllowed",
];

#[derive(Clone, Debug)]
pub struct PortProfile(Value);

impl PortProfile {
    pub fn id(&self) -> &str {
        self.0.get("id").and_then(Value::as_str).unwrap_or_default()
    }

    pub fn name(&self) -> Option<&str> {
        self.0.get("name").and_then(Value::as_str)
    }

    pub fn kind(&self) -> Option<&str> {
        self.0.get("kind").and_then(Value::as_str)
    }

    pub fn summary(&self) -> Value {
        json!({
            "id": self.0.get("id"),
            "name": self.0.get("name"),
            "kind": self.0.get("kind"),
            "untagged_network": self.0.get("customMappingUntaggedWiredNetworkId"),
            "tagged_networks": self.0.pointer("/customMappingTaggedWiredNetworksSelection/specificEntityIds"),
            "protected": self.0.get("protectedPortEnabled"),
            "trust": self.0.get("shouldTrustTraffic"),
            "storm_control": self.0.get("stormControlEnabled"),
            "poe_schedule": self.0.get("usePoeSchedule"),
            "assigned_devices": self.0.get("portAssignmentByDeviceId").and_then(Value::as_object).map(Map::len)
        })
    }

    /// Return the fetched profile object. Port profiles contain no credentials.
    pub fn details(&self) -> Value {
        self.0.clone()
    }
}

#[derive(Clone, Debug, Default)]
pub struct ProfilePatch {
    pub name: Option<String>,
    /// `Some(None)` clears the untagged network mapping.
    pub untagged_network: Option<Option<String>>,
    pub tagged_networks: Option<Vec<String>>,
    pub protected: Option<bool>,
    pub trust: Option<bool>,
    pub storm_control: Option<bool>,
    pub poe_schedule: Option<bool>,
}

impl ProfilePatch {
    fn validate(&self) -> Result<(), Error> {
        if self
            .name
            .as_ref()
            .is_some_and(|name| name.trim().is_empty())
        {
            return Err(usage("port profile name must not be empty"));
        }
        if let Some(networks) = &self.tagged_networks {
            let mut unique = HashSet::new();
            if networks
                .iter()
                .any(|id| id.is_empty() || !unique.insert(id))
            {
                return Err(usage(
                    "tagged network identifiers must be nonempty and unique",
                ));
            }
        }
        if self
            .untagged_network
            .as_ref()
            .and_then(Option::as_ref)
            .is_some_and(String::is_empty)
        {
            return Err(usage("untagged network identifier must not be empty"));
        }
        Ok(())
    }

    fn values(&self) -> Result<Map<String, Value>, Error> {
        self.validate()?;
        let mut values = Map::new();
        if let Some(value) = &self.name {
            values.insert("name".into(), json!(value));
        }
        if let Some(value) = &self.untagged_network {
            values.insert(
                "customMappingUntaggedWiredNetworkId".into(),
                value.as_ref().map_or(Value::Null, |id| json!(id)),
            );
            values.insert("networkMapping".into(), json!("custom"));
        }
        if let Some(value) = &self.tagged_networks {
            values.insert(
                "customMappingTaggedWiredNetworksSelection".into(),
                json!({"selection":"specific","specificEntityIds":value}),
            );
            values.insert("networkMapping".into(), json!("custom"));
        }
        for (field, value) in [
            ("protectedPortEnabled", self.protected),
            ("shouldTrustTraffic", self.trust),
            ("stormControlEnabled", self.storm_control),
            ("usePoeSchedule", self.poe_schedule),
        ] {
            if let Some(value) = value {
                values.insert(field.into(), json!(value));
            }
        }
        if values.is_empty() {
            return Err(usage("specify at least one port profile change"));
        }
        Ok(values)
    }
}

fn profile_route(site: &str) -> Result<String, Error> {
    if !reads::valid_site_id(site) {
        return Err(config("--site must be a UUID"));
    }
    Ok(format!("/sites/{site}/portProfiles"))
}

fn parse_profiles(payload: &Value) -> Result<Vec<PortProfile>, Error> {
    if payload
        .get("kind")
        .and_then(Value::as_str)
        .is_none_or(str::is_empty)
    {
        return Err(incomplete("port profile collection kind is missing"));
    }
    let count = payload.get("totalCount").and_then(Value::as_u64);
    let matching = payload.get("matchingFilterCount").and_then(Value::as_u64);
    if count.is_none() || matching.is_none() || count != matching {
        return Err(incomplete(
            "port profile collection counts are missing or inconsistent",
        ));
    }
    let rows = reads::parse_elements::<Value>(payload)?;
    if count.and_then(|count| usize::try_from(count).ok()) != Some(rows.len()) {
        return Err(incomplete("port profile collection is partial"));
    }
    if let Some(pending) = payload.get("pendingAvailability")
        && !matches!(pending, Value::Null | Value::Bool(false))
        && pending.as_u64() != Some(0)
        && !matches!(pending, Value::Array(items) if items.is_empty())
        && !matches!(pending, Value::Object(items) if items.is_empty())
    {
        return Err(incomplete(
            "port profile collection has pending availability",
        ));
    }
    let mut ids = HashSet::new();
    let mut profiles = Vec::with_capacity(rows.len());
    for row in rows {
        let id = row
            .get("id")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| incomplete("port profile identity is missing"))?;
        if !ids.insert(id.to_owned()) {
            return Err(incomplete(
                "port profile collection has duplicate identities",
            ));
        }
        if row
            .get("kind")
            .and_then(Value::as_str)
            .is_none_or(str::is_empty)
        {
            return Err(incomplete("port profile kind is missing"));
        }
        profiles.push(PortProfile(row));
    }
    Ok(profiles)
}

pub async fn list_port_profiles<T: TokenSource>(
    client: &Client<T>,
    site: &str,
) -> Result<Vec<PortProfile>, Error> {
    parse_profiles(&client.get(&profile_route(site)?).await?)
}

pub fn select_port_profile<'a>(
    profiles: &'a [PortProfile],
    selector: &str,
) -> Result<&'a PortProfile, Error> {
    if let Some(profile) = profiles.iter().find(|profile| profile.id() == selector) {
        return Ok(profile);
    }
    let mut matches = profiles
        .iter()
        .filter(|profile| profile.name() == Some(selector));
    let profile = matches
        .next()
        .ok_or_else(|| Error::new(ErrorKind::NotFound, "no port profile matches the selector"))?;
    if matches.next().is_some() {
        return Err(usage(
            "port profile name is ambiguous; select by identifier",
        ));
    }
    Ok(profile)
}

pub async fn read_port_profile<T: TokenSource>(
    client: &Client<T>,
    site: &str,
    selector: &str,
) -> Result<PortProfile, Error> {
    let collection = profile_route(site)?;
    let profiles = parse_profiles(&client.get(&collection).await?)?;
    let profile = select_port_profile(&profiles, selector)?;
    if is_complete_profile(&profile.0) {
        return Ok(profile.clone());
    }
    let path = item_path(client, &collection, profile.id())?;
    let item = client.get(&path).await?;
    validate_profile(&item, Some(profile.id()))?;
    if !is_complete_profile(&item) {
        return Err(incomplete("port profile detail is incomplete"));
    }
    Ok(PortProfile(item))
}

fn is_complete_profile(profile: &Value) -> bool {
    PROFILE_FIELDS
        .iter()
        .all(|field| profile.get(*field).is_some())
}

fn validate_profile(profile: &Value, expected_id: Option<&str>) -> Result<(), Error> {
    if !profile.is_object()
        || profile
            .get("id")
            .and_then(Value::as_str)
            .is_none_or(str::is_empty)
        || profile
            .get("kind")
            .and_then(Value::as_str)
            .is_none_or(str::is_empty)
    {
        return Err(incomplete(
            "port profile response has a missing identity or kind",
        ));
    }
    if expected_id.is_some_and(|id| profile.get("id").and_then(Value::as_str) != Some(id)) {
        return Err(incomplete(
            "port profile response identified a different profile",
        ));
    }
    if !profile.get("name").is_some_and(Value::is_string)
        || !profile
            .get("portAssignmentByDeviceId")
            .is_some_and(Value::is_object)
    {
        return Err(incomplete("port profile response is incomplete"));
    }
    Ok(())
}

type ProfileObserve = Box<dyn Fn(&Value) -> Result<Value, Error> + Send + Sync>;
type ProfileUpdate<'a, T> = FullObjectPut<ProfileResource<'a, T>, Value, ProfileObserve>;

struct ProfileResource<'a, T> {
    client: &'a Client<T>,
    collection: String,
    id: String,
}

impl<T: TokenSource> ProfileResource<'_, T> {
    fn path(&self) -> Result<String, Error> {
        item_path(self.client, &self.collection, &self.id)
    }
}

impl<T: TokenSource> ObjectResource for ProfileResource<'_, T> {
    async fn read_object(&self) -> Result<Value, Error> {
        let profile = self.client.get(&self.path()?).await?;
        validate_profile(&profile, Some(&self.id))?;
        if !is_complete_profile(&profile) {
            return Err(incomplete("port profile detail is incomplete"));
        }
        Ok(profile)
    }

    async fn put_object(&self, body: &Value) -> Result<(), Error> {
        validate_profile(body, Some(&self.id))?;
        let reply = self.client.put_full(&self.path()?, body).await?;
        validate_optional_ack(&reply, &self.id, body.get("kind").and_then(Value::as_str))?;
        Ok(())
    }
}

enum ProfileWrite<'a, T> {
    Update(ProfileUpdate<'a, T>),
    Create {
        body: Value,
        id: Mutex<CreateIdentity>,
        kind: String,
        name: String,
        existing_ids: Vec<String>,
    },
    Delete {
        id: String,
    },
}

#[derive(Default)]
struct CreateIdentity {
    attempted: bool,
    id: Option<String>,
}

pub struct PortProfileMutation<'a, T> {
    client: &'a Client<T>,
    collection: String,
    desired: Value,
    write: ProfileWrite<'a, T>,
}

impl<'a, T: TokenSource> PortProfileMutation<'a, T> {
    pub async fn update(
        client: &'a Client<T>,
        site: &str,
        selector: &str,
        patch: ProfilePatch,
        force: bool,
    ) -> Result<Prepared<Self>, Error> {
        let values = patch.values()?;
        let collection = profile_route(site)?;
        let profiles = parse_profiles(&client.get(&collection).await?)?;
        let selected = select_port_profile(&profiles, selector)?;
        let profile = if is_complete_profile(&selected.0) {
            selected.0.clone()
        } else {
            let item = client
                .get(&item_path(client, &collection, selected.id())?)
                .await?;
            validate_profile(&item, Some(selected.id()))?;
            if !is_complete_profile(&item) {
                return Err(incomplete("port profile detail is incomplete"));
            }
            item
        };
        reject_duplicate_name(&profiles, selected.id(), values.get("name"))?;
        guard_profile(
            client,
            site,
            selected.id(),
            &profile["portAssignmentByDeviceId"],
            force,
        )
        .await?;
        let id = selected.id().to_owned();
        let fields: Vec<String> = values.keys().cloned().collect();
        let observed_id = id.clone();
        let observed_kind = profile["kind"].as_str().unwrap().to_owned();
        let observed_fields = fields.clone();
        let observe: ProfileObserve = Box::new(move |body| {
            validate_profile(body, Some(&observed_id))?;
            if body["kind"] != observed_kind {
                return Err(incomplete("port profile kind changed during readback"));
            }
            profile_config_state(body, &observed_fields)
        });
        let resource = ProfileResource {
            client,
            collection: collection.clone(),
            id,
        };
        let patch_values = values.clone();
        let (backend, plan) =
            FullObjectPut::prepare(resource, profile.clone(), observe, move |body| {
                apply_profile_patch(body, &patch_values)
            })?;
        let target = target_profile(&PortProfile(profile));
        Ok(Prepared {
            backend: Self {
                client,
                collection,
                desired: plan.desired.clone(),
                write: ProfileWrite::Update(backend),
            },
            plan,
            target,
        })
    }

    pub async fn clone_from(
        client: &'a Client<T>,
        site: &str,
        selector: &str,
        new_name: &str,
    ) -> Result<Prepared<Self>, Error> {
        if new_name.trim().is_empty() {
            return Err(usage("new port profile name must not be empty"));
        }
        let collection = profile_route(site)?;
        let profiles = parse_profiles(&client.get(&collection).await?)?;
        let selected = select_port_profile(&profiles, selector)?;
        let profile = if is_complete_profile(&selected.0) {
            selected.0.clone()
        } else {
            let item = client
                .get(&item_path(client, &collection, selected.id())?)
                .await?;
            validate_profile(&item, Some(selected.id()))?;
            if !is_complete_profile(&item) {
                return Err(incomplete("port profile detail is incomplete"));
            }
            item
        };
        reject_duplicate_name(&profiles, "", Some(&json!(new_name)))?;
        let kind = profile["kind"]
            .as_str()
            .ok_or_else(|| incomplete("port profile kind is missing"))?
            .to_owned();
        let mut body = Map::new();
        for field in PROFILE_FIELDS
            .iter()
            .copied()
            .chain(PROFILE_CREATE_OPTIONAL_FIELDS.iter().copied())
        {
            if let Some(value) = profile.get(field) {
                body.insert(field.to_owned(), value.clone());
            }
        }
        let selection = profile
            .get("customMappingTaggedWiredNetworksSelection")
            .and_then(Value::as_object)
            .ok_or_else(|| incomplete("port profile tagged-network mapping is incomplete"))?;
        let selection_kind = selection
            .get("selection")
            .filter(|value| value.is_string())
            .ok_or_else(|| incomplete("port profile tagged-network selection is incomplete"))?;
        let specific_ids = selection
            .get("specificEntityIds")
            .filter(|value| value.is_array())
            .ok_or_else(|| incomplete("port profile tagged-network list is incomplete"))?;
        body.insert(
            "customMappingTaggedWiredNetworksSelection".into(),
            json!({
                "selection": selection_kind,
                "specificEntityIds": specific_ids,
            }),
        );
        body.insert("id".into(), json!(""));
        body.insert("name".into(), json!(new_name));
        body.insert("portAssignmentByDeviceId".into(), json!({}));
        let body = Value::Object(body);
        let fields: Vec<String> = body
            .as_object()
            .unwrap()
            .keys()
            .filter(|field| field.as_str() != "id")
            .cloned()
            .collect();
        let desired = profile_config_state(&body, &fields)?;
        let plan = Plan {
            current: absent_state(),
            desired: desired.clone(),
        };
        let target = json!({"id":Value::Null,"name":new_name,"kind":kind});
        let existing_ids = profiles
            .iter()
            .map(|profile| profile.id().to_owned())
            .collect();
        Ok(Prepared {
            backend: Self {
                client,
                collection,
                desired,
                write: ProfileWrite::Create {
                    body,
                    id: Mutex::new(CreateIdentity::default()),
                    kind,
                    name: new_name.to_owned(),
                    existing_ids,
                },
            },
            plan,
            target,
        })
    }

    pub async fn delete(
        client: &'a Client<T>,
        site: &str,
        selector: &str,
        force: bool,
    ) -> Result<Prepared<Self>, Error> {
        let collection = profile_route(site)?;
        let profiles = parse_profiles(&client.get(&collection).await?)?;
        let selected = select_port_profile(&profiles, selector)?;
        let profile = if is_complete_profile(&selected.0) {
            selected.0.clone()
        } else {
            let item = client
                .get(&item_path(client, &collection, selected.id())?)
                .await?;
            validate_profile(&item, Some(selected.id()))?;
            if !is_complete_profile(&item) {
                return Err(incomplete("port profile detail is incomplete"));
            }
            item
        };
        guard_profile(
            client,
            site,
            selected.id(),
            &profile["portAssignmentByDeviceId"],
            force,
        )
        .await?;
        let id = selected.id().to_owned();
        let target = target_profile(selected);
        let desired = absent_state();
        let mut configuration = profile;
        configuration.as_object_mut().unwrap().remove("id");
        let plan = Plan {
            current: json!({"exists":true,"configuration":configuration}),
            desired: desired.clone(),
        };
        Ok(Prepared {
            backend: Self {
                client,
                collection,
                desired,
                write: ProfileWrite::Delete { id },
            },
            plan,
            target,
        })
    }
}

pub async fn plan_update_port_profile<'a, T: TokenSource>(
    client: &'a Client<T>,
    site: &str,
    selector: &str,
    patch: ProfilePatch,
    force: bool,
) -> Result<Prepared<impl Mutation<State = Value> + 'a>, Error> {
    PortProfileMutation::update(client, site, selector, patch, force).await
}

pub async fn plan_clone_port_profile<'a, T: TokenSource>(
    client: &'a Client<T>,
    site: &str,
    selector: &str,
    new_name: &str,
) -> Result<Prepared<impl Mutation<State = Value> + 'a>, Error> {
    PortProfileMutation::clone_from(client, site, selector, new_name).await
}

pub async fn plan_delete_port_profile<'a, T: TokenSource>(
    client: &'a Client<T>,
    site: &str,
    selector: &str,
    force: bool,
) -> Result<Prepared<impl Mutation<State = Value> + 'a>, Error> {
    PortProfileMutation::delete(client, site, selector, force).await
}

impl<T: TokenSource> Mutation for PortProfileMutation<'_, T> {
    type State = Value;

    async fn read(&self) -> Result<Value, Error> {
        match &self.write {
            ProfileWrite::Update(update) => update.read().await,
            ProfileWrite::Create { id, name, .. } => {
                let profiles = parse_profiles(&self.client.get(&self.collection).await?)?;
                let created_id = {
                    id.lock()
                        .map_err(|_| incomplete("port profile identity lock failed"))?
                        .id
                        .clone()
                };
                let found = if let Some(id) = created_id.as_ref() {
                    profiles.iter().find(|profile| profile.id() == id)
                } else {
                    let mut found = profiles
                        .iter()
                        .filter(|profile| profile.name() == Some(name));
                    let row = found.next();
                    if found.next().is_some() {
                        return Err(incomplete("new port profile name became ambiguous"));
                    }
                    row
                };
                match found {
                    Some(profile) => {
                        let body = if is_complete_profile(&profile.0) {
                            profile.0.clone()
                        } else {
                            self.client
                                .get(&item_path(self.client, &self.collection, profile.id())?)
                                .await?
                        };
                        validate_profile(&body, created_id.as_deref())?;
                        if !is_complete_profile(&body) {
                            return Err(incomplete("port profile detail is incomplete"));
                        }
                        profile_config_state(&body, &create_fields(&self.desired))
                    }
                    None => Ok(absent_state()),
                }
            }
            ProfileWrite::Delete { id } => {
                let profiles = parse_profiles(&self.client.get(&self.collection).await?)?;
                Ok(if profiles.iter().any(|profile| profile.id() == id) {
                    json!({"exists":true,"configuration":{}})
                } else {
                    absent_state()
                })
            }
        }
    }

    async fn write(&self, desired: &Value) -> Result<(), Error> {
        if desired != &self.desired {
            return Err(usage(
                "desired state does not match the prepared port profile change",
            ));
        }
        match &self.write {
            ProfileWrite::Update(update) => update.write(desired).await,
            ProfileWrite::Create {
                body,
                id,
                kind,
                name,
                existing_ids,
            } => {
                {
                    let mut identity = id
                        .lock()
                        .map_err(|_| incomplete("port profile identity lock failed"))?;
                    if identity.attempted {
                        return Err(incomplete(
                            "port profile creation may be attempted only once",
                        ));
                    }
                    identity.attempted = true;
                }
                let reply = self.client.create(&self.collection, body).await?;
                let created = reply
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty())
                    .ok_or_else(|| {
                        incomplete("port profile creation acknowledgment has no identity")
                    })?;
                if reply.get("kind").and_then(Value::as_str) != Some(kind.as_str())
                    || existing_ids.iter().any(|existing| existing == created)
                    || reply
                        .get("name")
                        .and_then(Value::as_str)
                        .is_some_and(|ack_name| ack_name != name)
                {
                    return Err(incomplete(
                        "port profile creation acknowledgment identified a different profile",
                    ));
                }
                let mut bound = id
                    .lock()
                    .map_err(|_| incomplete("port profile identity lock failed"))?;
                bound.id = Some(created.to_owned());
                Ok(())
            }
            ProfileWrite::Delete { id } => {
                let reply = self
                    .client
                    .delete(&item_path(self.client, &self.collection, id)?)
                    .await?;
                validate_optional_ack(&reply, id, None)
            }
        }
    }
}

fn create_fields(desired: &Value) -> Vec<String> {
    desired
        .get("configuration")
        .and_then(Value::as_object)
        .map(|fields| fields.keys().cloned().collect())
        .unwrap_or_default()
}

fn config_state(body: &Value, fields: &[String]) -> Value {
    let configuration: Map<String, Value> = fields
        .iter()
        .map(|field| {
            (
                field.clone(),
                body.get(field).cloned().unwrap_or(Value::Null),
            )
        })
        .collect();
    json!({"exists":true,"configuration":configuration})
}

fn profile_config_state(body: &Value, fields: &[String]) -> Result<Value, Error> {
    let mut configuration = Map::new();
    for field in fields {
        let value = if field == "customMappingTaggedWiredNetworksSelection" {
            let selection = body
                .get(field)
                .and_then(Value::as_object)
                .ok_or_else(|| incomplete("port profile tagged-network mapping is malformed"))?;
            let api_selection = selection
                .get("selection")
                .filter(|value| value.is_string())
                .ok_or_else(|| incomplete("port profile tagged-network selection is malformed"))?;
            let ids = selection
                .get("specificEntityIds")
                .filter(|value| value.is_array())
                .ok_or_else(|| incomplete("port profile tagged-network list is malformed"))?;
            json!({"selection":api_selection,"specificEntityIds":ids})
        } else {
            body.get(field).cloned().unwrap_or(Value::Null)
        };
        configuration.insert(field.clone(), value);
    }
    Ok(json!({"exists":true,"configuration":configuration}))
}

fn apply_profile_patch(body: &mut Value, patch: &Map<String, Value>) -> Result<(), Error> {
    let object = body
        .as_object_mut()
        .ok_or_else(|| incomplete("port profile is not an object"))?;
    for (field, value) in patch {
        if field == "customMappingTaggedWiredNetworksSelection" {
            let changes = value
                .as_object()
                .ok_or_else(|| incomplete("tagged-network update is malformed"))?;
            let existing = object
                .get_mut(field)
                .and_then(Value::as_object_mut)
                .ok_or_else(|| incomplete("port profile tagged-network mapping is malformed"))?;
            for key in ["selection", "specificEntityIds"] {
                let change = changes
                    .get(key)
                    .ok_or_else(|| incomplete("tagged-network update is incomplete"))?;
                existing.insert(key.to_owned(), change.clone());
            }
        } else {
            object.insert(field.clone(), value.clone());
        }
    }
    Ok(())
}

fn reject_duplicate_name(
    profiles: &[PortProfile],
    id: &str,
    name: Option<&Value>,
) -> Result<(), Error> {
    if let Some(name) = name.and_then(Value::as_str)
        && profiles
            .iter()
            .any(|profile| profile.id() != id && profile.name() == Some(name))
    {
        return Err(usage("port profile name already exists"));
    }
    Ok(())
}

fn target_profile(profile: &PortProfile) -> Value {
    json!({"id":profile.id(),"name":profile.name(),"kind":profile.kind()})
}

fn absent_state() -> Value {
    json!({"exists":false,"configuration":{}})
}

fn item_path<T: TokenSource>(
    client: &Client<T>,
    collection: &str,
    id: &str,
) -> Result<String, Error> {
    let url = client.resource_url(collection, &[id], None)?;
    url.path()
        .strip_prefix(client.base.path().trim_end_matches('/'))
        .map(str::to_owned)
        .ok_or_else(|| config("invalid port profile route"))
}

fn validate_optional_ack(reply: &Value, id: &str, kind: Option<&str>) -> Result<(), Error> {
    if let Some(ack_id) = reply.get("id")
        && ack_id.as_str() != Some(id)
    {
        return Err(incomplete(
            "port profile acknowledgment identified a different profile",
        ));
    }
    if let Some(kind) = kind
        && reply
            .get("kind")
            .and_then(Value::as_str)
            .is_some_and(|ack_kind| ack_kind != kind)
    {
        return Err(incomplete(
            "port profile acknowledgment has a different kind",
        ));
    }
    Ok(())
}

async fn guard_profile<T: TokenSource>(
    client: &Client<T>,
    site: &str,
    id: &str,
    assignments: &Value,
    force: bool,
) -> Result<(), Error> {
    let path = inventory_path(site)?;
    let inventory = client.get(&path).await?;
    inventory_elements(&inventory)?;
    crate::ports::guard_profile(&inventory, id, assignments, client.protected_ports(), force)
}

async fn guard_site<T: TokenSource>(
    client: &Client<T>,
    site: &str,
    force: bool,
) -> Result<(), Error> {
    let path = inventory_path(site)?;
    let inventory = client.get(&path).await?;
    inventory_elements(&inventory)?;
    crate::ports::guard_site(&inventory, client.protected_ports(), force)
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Weekday {
    Monday,
    Tuesday,
    Wednesday,
    Thursday,
    Friday,
    Saturday,
    Sunday,
}

impl Weekday {
    fn api_id(self) -> &'static str {
        match self {
            Self::Monday => "monday",
            Self::Tuesday => "tuesday",
            Self::Wednesday => "wednesday",
            Self::Thursday => "thursday",
            Self::Friday => "friday",
            Self::Saturday => "saturday",
            Self::Sunday => "sunday",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActiveSchedule {
    None,
    Simple,
    Week,
}

impl ActiveSchedule {
    fn api_id(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Simple => "simple",
            Self::Week => "week",
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct SimpleSchedule {
    pub active_days: Vec<Weekday>,
    pub start_time: Option<String>,
    pub end_time: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct WeekdaySchedule {
    pub enabled: bool,
    pub active_all_day: bool,
    pub start_time: Option<String>,
    pub end_time: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct WeekSchedule {
    pub days: BTreeMap<Weekday, WeekdaySchedule>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct SchedulePatch {
    pub active_schedule: Option<ActiveSchedule>,
    pub simple_schedule: Option<SimpleSchedule>,
    pub week_schedule: Option<WeekSchedule>,
}

impl SchedulePatch {
    fn validate(&self) -> Result<(), Error> {
        if self.simple_schedule.is_some() && self.week_schedule.is_some() {
            return Err(usage("specify either a simple or weekly PoE schedule"));
        }
        if let Some(simple) = &self.simple_schedule {
            let unique: HashSet<_> = simple.active_days.iter().copied().collect();
            if simple.active_days.is_empty() || unique.len() != simple.active_days.len() {
                return Err(usage("simple schedule needs unique active days"));
            }
            validate_time_pair(&simple.start_time, &simple.end_time)?;
        }
        if let Some(week) = &self.week_schedule {
            if week.days.len() != 7
                || [
                    Weekday::Monday,
                    Weekday::Tuesday,
                    Weekday::Wednesday,
                    Weekday::Thursday,
                    Weekday::Friday,
                    Weekday::Saturday,
                    Weekday::Sunday,
                ]
                .iter()
                .any(|day| !week.days.contains_key(day))
            {
                return Err(usage("weekly schedule must specify all seven weekdays"));
            }
            for day in week.days.values() {
                validate_time_pair(&day.start_time, &day.end_time)?;
                if day.active_all_day && !day.enabled {
                    return Err(usage("an all-day weekly range must be enabled"));
                }
                if day.enabled && !day.active_all_day && day.start_time.is_none() {
                    return Err(usage("an enabled weekly range needs start and end times"));
                }
            }
        }
        if self.active_schedule.is_none()
            && self.simple_schedule.is_none()
            && self.week_schedule.is_none()
        {
            return Err(usage("specify at least one PoE schedule change"));
        }
        if self.active_schedule == Some(ActiveSchedule::Simple) && self.week_schedule.is_some()
            || self.active_schedule == Some(ActiveSchedule::Week) && self.simple_schedule.is_some()
        {
            return Err(usage(
                "active schedule kind conflicts with supplied schedule data",
            ));
        }
        Ok(())
    }
}

fn validate_time_pair(start: &Option<String>, end: &Option<String>) -> Result<(), Error> {
    if start.is_some() != end.is_some() {
        return Err(usage(
            "schedule start and end times must be supplied together",
        ));
    }
    for time in start.iter().chain(end.iter()) {
        let bytes = time.as_bytes();
        if bytes.len() != 5
            || bytes[2] != b':'
            || !bytes[..2]
                .iter()
                .chain(bytes[3..].iter())
                .all(u8::is_ascii_digit)
        {
            return Err(usage("schedule times must use HH:mm format"));
        }
        let hour: u8 = time[..2]
            .parse()
            .map_err(|_| usage("schedule time is invalid"))?;
        let minute: u8 = time[3..]
            .parse()
            .map_err(|_| usage("schedule time is invalid"))?;
        if hour > 23 || minute > 59 {
            return Err(usage("schedule time is outside the 24-hour day"));
        }
    }
    Ok(())
}

fn schedule_patch_body(
    current: &Value,
    patch: &SchedulePatch,
) -> Result<(Value, Vec<String>), Error> {
    let active = patch
        .active_schedule
        .or_else(|| {
            patch
                .simple_schedule
                .as_ref()
                .map(|_| ActiveSchedule::Simple)
        })
        .or_else(|| patch.week_schedule.as_ref().map(|_| ActiveSchedule::Week));
    let mut fields = Vec::new();
    let mut body = current.clone();
    if let Some(schedule) = &patch.simple_schedule {
        let mut days = schedule.active_days.clone();
        days.sort_unstable();
        let days = days
            .into_iter()
            .map(|day| json!(day.api_id()))
            .collect::<Vec<_>>();
        let object = body["schedule"]
            .as_object_mut()
            .ok_or_else(|| incomplete("PoE simple schedule is malformed"))?;
        object.insert("activeDays".into(), json!(days));
        let range = object
            .entry("activeTimeRange")
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .ok_or_else(|| incomplete("PoE simple time range is malformed"))?;
        if let (Some(start), Some(end)) = (&schedule.start_time, &schedule.end_time) {
            range.insert("enabled".into(), json!(true));
            range.insert("startTime".into(), json!(start));
            range.insert("endTime".into(), json!(end));
        } else {
            range.insert("enabled".into(), json!(false));
            range.remove("startTime");
            range.remove("endTime");
        }
        fields.push("schedule".to_owned());
    }
    if let Some(schedule) = &patch.week_schedule {
        let week = body["weekSchedule"]
            .as_object_mut()
            .ok_or_else(|| incomplete("PoE weekly schedule is malformed"))?;
        let map = week
            .entry("schedulePerWeekdayMap")
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .ok_or_else(|| incomplete("PoE weekly day map is malformed"))?;
        for (day, range) in &schedule.days {
            let entry = map
                .entry(day.api_id().to_owned())
                .or_insert_with(|| json!({}))
                .as_object_mut()
                .ok_or_else(|| incomplete("PoE weekday entry is malformed"))?;
            entry.insert("enabled".into(), json!(range.enabled));
            entry.insert("activeAllDay".into(), json!(range.active_all_day));
            if range.enabled && !range.active_all_day {
                entry.insert("startTime".into(), json!(range.start_time));
                entry.insert("endTime".into(), json!(range.end_time));
            } else {
                entry.remove("startTime");
                entry.remove("endTime");
            }
        }
        fields.push("weekSchedule".to_owned());
    }
    if let Some(active) = active {
        body["activeSchedule"] = json!(active.api_id());
        fields.push("activeSchedule".to_owned());
    }
    Ok((body, fields))
}

struct SettingsResource<'a, T> {
    client: &'a Client<T>,
    path: String,
    kind: &'static str,
}

impl<T: TokenSource> ObjectResource for SettingsResource<'_, T> {
    async fn read_object(&self) -> Result<Value, Error> {
        let body = self.client.get(&self.path).await?;
        if body.get("kind").and_then(Value::as_str) != Some(self.kind) {
            return Err(incomplete("settings response has an unexpected kind"));
        }
        Ok(body)
    }
    async fn put_object(&self, body: &Value) -> Result<(), Error> {
        if body.get("kind").and_then(Value::as_str) != Some(self.kind) {
            return Err(incomplete("settings PUT changed resource kind"));
        }
        let reply = self.client.put_full(&self.path, body).await?;
        if reply
            .get("kind")
            .and_then(Value::as_str)
            .is_some_and(|kind| kind != self.kind)
        {
            return Err(incomplete("settings acknowledgment has an unexpected kind"));
        }
        Ok(())
    }
}

fn site_route(site: &str, resource: &'static str) -> Result<String, Error> {
    if !reads::valid_site_id(site) {
        return Err(config("--site must be a UUID"));
    }
    Ok(format!("/sites/{site}/{resource}"))
}

type SettingsObserve = Box<dyn Fn(&Value) -> Result<Value, Error> + Send + Sync>;

pub async fn poe_schedule<T: TokenSource>(client: &Client<T>, site: &str) -> Result<Value, Error> {
    let path = site_route(site, "poeSchedule")?;
    let body = client.get(&path).await?;
    validate_poe_schedule(&body)?;
    Ok(body)
}

pub async fn plan_poe_schedule<'a, T: TokenSource>(
    client: &'a Client<T>,
    site: &str,
    patch: SchedulePatch,
    force: bool,
) -> Result<Prepared<impl Mutation<State = Value> + 'a>, Error> {
    patch.validate()?;
    guard_site(client, site, force).await?;
    let path = site_route(site, "poeSchedule")?;
    let current = client.get(&path).await?;
    validate_poe_schedule(&current)?;
    let (desired_body, fields) = schedule_patch_body(&current, &patch)?;
    validate_poe_schedule(&desired_body)?;
    let observed = fields.clone();
    let observe: SettingsObserve =
        Box::new(move |body| settings_state(body, "poeSchedule", &observed));
    let resource = SettingsResource {
        client,
        path,
        kind: "poeSchedule",
    };
    let (backend, plan) = FullObjectPut::prepare(resource, current, observe, move |body| {
        *body = desired_body;
        Ok(())
    })?;
    let target = json!({"site_id":site,"resource":"poeSchedule"});
    Ok(Prepared {
        backend,
        plan,
        target,
    })
}

pub async fn power_management<T: TokenSource>(
    client: &Client<T>,
    site: &str,
) -> Result<Value, Error> {
    let path = site_route(site, "powerManagement")?;
    let body = client.get(&path).await?;
    if body.get("kind").and_then(Value::as_str) != Some("powerManagement")
        || !body
            .get("isEnergyEfficientEthernetEnabled")
            .is_some_and(Value::is_boolean)
        || !body.get("poeSchedule").is_some_and(Value::is_object)
    {
        return Err(incomplete("power management response is incomplete"));
    }
    validate_poe_schedule(&body["poeSchedule"])?;
    Ok(body)
}

pub async fn plan_eee<'a, T: TokenSource>(
    client: &'a Client<T>,
    site: &str,
    enabled: bool,
    force: bool,
) -> Result<Prepared<impl Mutation<State = Value> + 'a>, Error> {
    guard_site(client, site, force).await?;
    let path = site_route(site, "powerManagement")?;
    let current = client.get(&path).await?;
    if current.get("kind").and_then(Value::as_str) != Some("powerManagement")
        || !current
            .get("isEnergyEfficientEthernetEnabled")
            .is_some_and(Value::is_boolean)
        || !current.get("poeSchedule").is_some_and(Value::is_object)
    {
        return Err(incomplete("power management response is incomplete"));
    }
    validate_poe_schedule(&current["poeSchedule"])?;
    let observe: SettingsObserve = Box::new(|body| {
        settings_state(
            body,
            "powerManagement",
            &["isEnergyEfficientEthernetEnabled".to_owned()],
        )
    });
    let resource = SettingsResource {
        client,
        path,
        kind: "powerManagement",
    };
    let (backend, plan) = FullObjectPut::prepare(resource, current, observe, move |body| {
        body["isEnergyEfficientEthernetEnabled"] = json!(enabled);
        Ok(())
    })?;
    let target = json!({"site_id":site,"resource":"powerManagement"});
    Ok(Prepared {
        backend,
        plan,
        target,
    })
}

fn validate_poe_schedule(body: &Value) -> Result<(), Error> {
    let active = body.get("activeSchedule").and_then(Value::as_str);
    let inactive_configuration = active == Some("none");
    let schedule_valid = body
        .get("schedule")
        .is_some_and(|value| value.is_object() || (inactive_configuration && value.is_null()));
    let week_schedule_valid = body
        .get("weekSchedule")
        .is_some_and(|value| value.is_object() || (inactive_configuration && value.is_null()));
    if body.get("kind").and_then(Value::as_str) != Some("poeSchedule")
        || !schedule_valid
        || !week_schedule_valid
        || !body
            .get("poeScheduleDeviceMappings")
            .is_some_and(Value::is_array)
        || !matches!(active, Some("none" | "simple" | "week"))
    {
        return Err(incomplete(
            "PoE schedule response is incomplete or has an unexpected kind",
        ));
    }
    Ok(())
}

fn settings_state(body: &Value, kind: &str, fields: &[String]) -> Result<Value, Error> {
    if body.get("kind").and_then(Value::as_str) != Some(kind) {
        return Err(incomplete("settings readback has an unexpected kind"));
    }
    if kind != "poeSchedule" {
        return Ok(config_state(body, fields));
    }
    let mut configuration = Map::new();
    for field in fields {
        let value = match field.as_str() {
            "schedule" => simple_schedule_state(
                body.get(field)
                    .ok_or_else(|| incomplete("simple schedule is missing"))?,
            )?,
            "weekSchedule" => week_schedule_state(
                body.get(field)
                    .ok_or_else(|| incomplete("weekly schedule is missing"))?,
            )?,
            _ => body.get(field).cloned().unwrap_or(Value::Null),
        };
        configuration.insert(field.clone(), value);
    }
    Ok(json!({"exists":true,"configuration":configuration}))
}

fn simple_schedule_state(schedule: &Value) -> Result<Value, Error> {
    let days = schedule
        .get("activeDays")
        .and_then(Value::as_array)
        .filter(|days| days.iter().all(Value::is_string))
        .ok_or_else(|| incomplete("simple schedule days are malformed"))?;
    let range = schedule
        .get("activeTimeRange")
        .and_then(Value::as_object)
        .ok_or_else(|| incomplete("simple schedule time range is malformed"))?;
    let enabled = range
        .get("enabled")
        .and_then(Value::as_bool)
        .ok_or_else(|| incomplete("simple schedule enabled state is missing"))?;
    let mut owned_range = json!({"enabled":enabled});
    if enabled {
        for field in ["startTime", "endTime"] {
            let time = range
                .get(field)
                .filter(|value| value.is_string())
                .ok_or_else(|| incomplete("simple schedule time is missing"))?;
            owned_range[field] = time.clone();
        }
    }
    Ok(json!({"activeDays":days,"activeTimeRange":owned_range}))
}

fn week_schedule_state(schedule: &Value) -> Result<Value, Error> {
    const DAYS: &[&str] = &[
        "monday",
        "tuesday",
        "wednesday",
        "thursday",
        "friday",
        "saturday",
        "sunday",
    ];
    let map = schedule
        .get("schedulePerWeekdayMap")
        .and_then(Value::as_object)
        .ok_or_else(|| incomplete("weekly schedule day map is malformed"))?;
    let mut owned_map = Map::new();
    for day in DAYS {
        let Some(entry) = map.get(*day) else { continue };
        let entry = entry
            .as_object()
            .ok_or_else(|| incomplete("weekly schedule weekday is malformed"))?;
        let enabled = entry
            .get("enabled")
            .and_then(Value::as_bool)
            .ok_or_else(|| incomplete("weekly schedule enabled state is missing"))?;
        let active_all_day = entry
            .get("activeAllDay")
            .and_then(Value::as_bool)
            .ok_or_else(|| incomplete("weekly schedule all-day state is missing"))?;
        let mut owned = json!({"enabled":enabled,"activeAllDay":active_all_day});
        if enabled && !active_all_day {
            for field in ["startTime", "endTime"] {
                let time = entry
                    .get(field)
                    .filter(|value| value.is_string())
                    .ok_or_else(|| incomplete("weekly schedule time is missing"))?;
                owned[field] = time.clone();
            }
        }
        owned_map.insert((*day).to_owned(), owned);
    }
    Ok(json!({"schedulePerWeekdayMap":owned_map}))
}

fn usage(message: &str) -> Error {
    Error::new(ErrorKind::Usage, message)
}
fn config(message: &str) -> Error {
    Error::new(ErrorKind::Config, message)
}
fn incomplete(message: &str) -> Error {
    Error::new(ErrorKind::Unverified, message)
}
