//! Wired-network configuration observed in the Instant On portal bundle.
//! Updates preserve the full fetched object. Creation uses the server template.
mod config;
mod membership;
mod services;
mod subnet;

use std::{collections::HashSet, fmt, sync::Mutex};

use serde::{Serialize, Serializer};
use serde_json::{Value, json};

use super::{Client, reads};
use crate::{
    Error, ErrorKind, TokenSource,
    mutation::{FullObjectPut, Mutation, ObjectResource, Plan},
};

pub use config::{DhcpPatch, DnsMode, NetworkType, Patch};
pub use membership::CreatePortMembership;
pub use services::status as shared_services_status;
pub use services::{SharedServiceMutation, SharedServicesMutation};

#[derive(Clone)]
pub struct Network(Value);

impl fmt::Debug for Network {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.details().fmt(f)
    }
}

impl Network {
    pub(crate) fn raw(&self) -> &Value {
        &self.0
    }

    pub fn id(&self) -> &str {
        self.0["id"].as_str().unwrap_or_default()
    }
    pub fn name(&self) -> Option<&str> {
        self.0.get("wiredNetworkName").and_then(Value::as_str)
    }

    pub fn summary(&self) -> Value {
        json!({
            "id":self.id(), "name":self.name(),
            "vlan_id":self.0.get("vlanId").and_then(Value::as_u64),
            "enabled":self.0.get("isEnabled").and_then(Value::as_bool),
            "type":self.0.get("type").and_then(Value::as_str),
            "dhcp":self.0.get("useDhcpScope").and_then(Value::as_bool),
            "network":self.0.pointer("/dhcpScope/network").and_then(Value::as_str),
            "netmask":self.0.pointer("/dhcpScope/netmask").and_then(Value::as_str),
            "clients":self.0.get("wiredClientsCount").and_then(Value::as_u64)
        })
    }

    pub fn details(&self) -> Value {
        let mut result = self.summary();
        for field in config::FIELDS.iter().chain(
            [
                "isManagement",
                "isDeletable",
                "vlanIdCanBeChanged",
                "canDisableDhcpScope",
                "isSharedServicesEnabled",
            ]
            .iter(),
        ) {
            result[*field] = self.0.get(*field).cloned().unwrap_or(Value::Null);
        }
        redact(&mut result);
        result
    }

    pub fn shared_services(&self) -> Result<Value, Error> {
        services::summaries(&self.0)
    }
}

#[derive(Clone, PartialEq)]
pub struct State {
    exists: bool,
    fields: Value,
}

impl State {
    fn printable(&self) -> Value {
        let mut fields = self.fields.clone();
        redact(&mut fields);
        json!({"exists":self.exists,"configuration":fields})
    }
}

impl fmt::Debug for State {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.printable().fmt(f)
    }
}
impl Serialize for State {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.printable().serialize(serializer)
    }
}

fn redact(value: &mut Value) {
    match value {
        Value::Object(object) => {
            for (key, child) in object {
                let key = key.to_ascii_lowercase();
                if [
                    "password",
                    "presharedkey",
                    "sharedsecret",
                    "secret",
                    "token",
                    "privatekey",
                ]
                .iter()
                .any(|part| key.contains(part))
                {
                    if !child.is_null() {
                        *child = json!("(redacted)");
                    }
                } else {
                    redact(child);
                }
            }
        }
        Value::Array(array) => array.iter_mut().for_each(redact),
        _ => {}
    }
}

fn route(site: &str, resource: &str) -> Result<String, Error> {
    if !reads::valid_site_id(site) {
        return Err(Error::new(ErrorKind::Config, "--site must be a UUID"));
    }
    Ok(format!("/sites/{site}/{resource}"))
}

fn parse(payload: &Value) -> Result<Vec<Network>, Error> {
    let rows: Vec<Value> = reads::parse_elements(payload)?;
    let mut ids = HashSet::new();
    rows.into_iter()
        .map(|row| {
            let id = row
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
                .ok_or_else(|| incomplete("wired network identity is missing"))?;
            if !ids.insert(id.to_owned())
                || row
                    .get("isWireless")
                    .is_some_and(|wireless| wireless != false)
            {
                return Err(incomplete(
                    "wired collection has duplicate identities or a different network type",
                ));
            }
            Ok(Network(row))
        })
        .collect()
}

pub async fn list<T: TokenSource>(client: &Client<T>, site: &str) -> Result<Vec<Network>, Error> {
    parse(&client.get(&route(site, "wiredNetworks")?).await?)
}

pub(crate) fn validate_dhcp_scope(scope: &Value) -> Result<(), Error> {
    config::validate_scope(scope)
}

pub fn select<'a>(networks: &'a [Network], selector: &str) -> Result<&'a Network, Error> {
    if let Some(network) = networks.iter().find(|network| network.id() == selector) {
        return Ok(network);
    }
    let mut matches = networks
        .iter()
        .filter(|network| network.name() == Some(selector));
    let network = matches.next().ok_or_else(|| {
        Error::new(
            ErrorKind::NotFound,
            "no wired network matches the supplied selector",
        )
    })?;
    if matches.next().is_some() {
        return Err(usage("wired network name is ambiguous; select by ID"));
    }
    Ok(network)
}

type Observe = Box<dyn Fn(&Value) -> Result<State, Error> + Send + Sync>;
type Update<'a, T> = FullObjectPut<NetworkResource<'a, T>, State, Observe>;

struct NetworkResource<'a, T> {
    client: &'a Client<T>,
    collection: String,
    id: String,
}
impl<T: TokenSource> ObjectResource for NetworkResource<'_, T> {
    async fn read_object(&self) -> Result<Value, Error> {
        let networks = parse(&self.client.get(&self.collection).await?)?;
        Ok(select(&networks, &self.id)?.0.clone())
    }
    async fn put_object(&self, body: &Value) -> Result<(), Error> {
        check_identity(body, &self.id)?;
        let reply = self
            .client
            .put_full(&item_path(self.client, &self.collection, &self.id)?, body)
            .await?;
        check_ack(&reply, &self.id)
    }
}

enum Write<'a, T> {
    Update(Update<'a, T>),
    Create {
        body: Value,
        id: Mutex<Option<String>>,
        existing_ids: Vec<String>,
    },
    Delete {
        id: String,
    },
}

pub struct NetworkMutation<'a, T> {
    client: &'a Client<T>,
    collection: String,
    target: Value,
    fields: Vec<&'static str>,
    desired: State,
    write: Write<'a, T>,
}

impl<'a, T: TokenSource> NetworkMutation<'a, T> {
    pub fn target(&self) -> Value {
        self.target.clone()
    }

    pub async fn update(
        client: &'a Client<T>,
        site: &str,
        selector: &str,
        patch: Patch,
    ) -> Result<(Self, Plan<State>), Error> {
        patch.validate()?;
        if patch.is_empty() {
            return Err(usage("specify at least one wired network change"));
        }
        let collection = route(site, "wiredNetworks")?;
        let networks = list(client, site).await?;
        let network = select(&networks, selector)?;
        reject_duplicates(&networks, network.id(), &patch)?;
        let id = network.id().to_owned();
        let fields = patch.fields();
        let observed_fields = fields.clone();
        let observed_id = id.clone();
        let observe: Observe = Box::new(move |body| {
            check_identity(body, &observed_id)?;
            Ok(observe(body, &observed_fields))
        });
        let resource = NetworkResource {
            client,
            collection: collection.clone(),
            id,
        };
        let (backend, plan) =
            FullObjectPut::prepare(resource, network.0.clone(), observe, |body| {
                patch.apply(body, false)
            })?;
        Ok((
            Self {
                client,
                collection,
                target: target(network),
                fields,
                desired: plan.desired.clone(),
                write: Write::Update(backend),
            },
            plan,
        ))
    }

    pub async fn create(
        client: &'a Client<T>,
        site: &str,
        patch: Patch,
        port_membership: CreatePortMembership,
    ) -> Result<(Self, Plan<State>), Error> {
        patch.validate()?;
        if patch.name.is_none() || patch.vlan_id.is_none() {
            return Err(usage("a new wired network requires a name and VLAN ID"));
        }
        let collection = route(site, "wiredNetworks")?;
        let payload = client.get(&collection).await?;
        let networks = parse(&payload)?;
        reject_duplicates(&networks, "", &patch)?;
        let existing_ids = networks
            .iter()
            .map(|network| network.id().to_owned())
            .collect();
        let template = payload
            .pointer("/metaData/defaultWiredNetwork")
            .filter(|template| template.is_object())
            .ok_or_else(|| incomplete("wired creation template is missing"))?;
        let mut body = Value::Object(
            config::FIELDS
                .iter()
                .filter_map(|field| {
                    template
                        .get(*field)
                        .map(|value| ((*field).to_owned(), value.clone()))
                })
                .collect(),
        );
        let port_membership =
            membership::Membership::prepare(&mut body["devicePortMappings"], port_membership)?;
        // getDefaultNetworkInfo uses gateway-configuration to decide whether
        // the wired template has a DHCP scope. Missing capability data is not false.
        if body.get("dhcpScope").is_some_and(Value::is_object) || !patch.dhcp.is_empty() {
            let capabilities = client.get(&route(site, "capabilities")?).await?;
            let capabilities = capabilities
                .get("capabilities")
                .and_then(Value::as_array)
                .filter(|items| items.iter().all(Value::is_string))
                .ok_or_else(|| incomplete("wired DHCP capability data is unavailable"))?;
            let gateway = capabilities
                .iter()
                .any(|capability| capability == "gateway-configuration");
            if !gateway {
                if !patch.dhcp.is_empty() {
                    return Err(Error::new(
                        ErrorKind::Unsupported,
                        "this site does not support wired DHCP configuration",
                    ));
                }
                body.as_object_mut().unwrap().remove("dhcpScope");
                body["useDhcpScope"] = json!(false);
            } else if body.get("dhcpScope").is_some_and(Value::is_object)
                && patch.dhcp.gateway.is_none()
            {
                let reserved = client.get(&route(site, "reservedIpSubnets")?).await?;
                subnet::allocate(&mut body["dhcpScope"], &reserved)?;
            }
        }
        patch.apply(&mut body, true)?;
        config::apply_create_defaults(&mut body, &payload["metaData"])?;
        config::validate_create(&body)?;
        let mut fields = patch.fields();
        fields.extend([
            "wiredNetworkName",
            "vlanId",
            "type",
            "isEnabled",
            "useDhcpScope",
            "isAccessRestricted",
            "isInternetAllowed",
        ]);
        if body.get("qos").is_some_and(Value::is_object) {
            fields.push("qos/trafficPriority");
        }
        if body.get("dhcpScope").is_some_and(Value::is_object) {
            fields.extend(["dhcpScope/network", "dhcpScope/netmask"]);
        }
        fields.sort_unstable();
        fields.dedup();
        let mut desired = observe(&body, &fields);
        desired.fields["devicePortMappings"] = port_membership.to_value();
        let plan = Plan {
            current: absent(),
            desired: desired.clone(),
        };
        let target = json!({"id":Value::Null,"name":body["wiredNetworkName"],"vlan_id":body["vlanId"],"port_mappings":port_membership.has_membership()});
        Ok((
            Self {
                client,
                collection,
                target,
                fields,
                desired,
                write: Write::Create {
                    body,
                    id: Mutex::new(None),
                    existing_ids,
                },
            },
            plan,
        ))
    }

    pub async fn delete(
        client: &'a Client<T>,
        site: &str,
        selector: &str,
        yes: bool,
    ) -> Result<(Self, Plan<State>), Error> {
        let collection = route(site, "wiredNetworks")?;
        let networks = list(client, site).await?;
        let network = select(&networks, selector)?;
        match network.0.get("isDeletable").and_then(Value::as_bool) {
            Some(true) => {}
            Some(false) => return Err(usage("this wired network cannot be deleted")),
            None => {
                return Err(incomplete(
                    "wired network deletion eligibility is unavailable",
                ));
            }
        }
        if port_mappings(&network.0) != Some(false) && !yes {
            return Err(usage(
                "deleting a network with assigned or unknown port mappings requires --yes",
            ));
        }
        let desired = absent();
        let plan = Plan {
            current: State {
                exists: true,
                fields: json!({}),
            },
            desired: desired.clone(),
        };
        Ok((
            Self {
                client,
                collection,
                target: target(network),
                fields: vec![],
                desired,
                write: Write::Delete {
                    id: network.id().to_owned(),
                },
            },
            plan,
        ))
    }
}

impl<T: TokenSource> Mutation for NetworkMutation<'_, T> {
    type State = State;
    async fn read(&self) -> Result<State, Error> {
        if let Write::Update(backend) = &self.write {
            return backend.read().await;
        }
        let networks = parse(&self.client.get(&self.collection).await?)?;
        let id = match &self.write {
            Write::Create { id, .. } => id
                .lock()
                .map_err(|_| incomplete("creation identity lock failed"))?
                .clone(),
            Write::Delete { id } => {
                return Ok(if networks.iter().any(|row| row.id() == id) {
                    State {
                        exists: true,
                        fields: json!({}),
                    }
                } else {
                    absent()
                });
            }
            Write::Update(_) => unreachable!(),
        };
        let row = if let Some(id) = id {
            networks.iter().find(|row| row.id() == id)
        } else {
            match select(
                &networks,
                self.target["name"]
                    .as_str()
                    .ok_or_else(|| incomplete("creation name is missing"))?,
            ) {
                Ok(row) => Some(row),
                Err(error) if error.kind == ErrorKind::NotFound => None,
                Err(error) => return Err(error),
            }
        };
        if let (Some(row), Write::Create { existing_ids, .. }) = (row, &self.write)
            && existing_ids.iter().any(|id| id == row.id())
        {
            return Err(incomplete(
                "creation readback identified a pre-existing wired network",
            ));
        }
        let Some(row) = row else {
            return Ok(absent());
        };
        let mut state = observe(&row.0, &self.fields);
        if matches!(&self.write, Write::Create { .. }) {
            state.fields["devicePortMappings"] =
                membership::Membership::parse(&row.0["devicePortMappings"])?.to_value();
        }
        Ok(state)
    }
    async fn write(&self, desired: &State) -> Result<(), Error> {
        if desired != &self.desired {
            return Err(usage(
                "desired state does not match the prepared wired change",
            ));
        }
        match &self.write {
            Write::Update(backend) => backend.write(desired).await,
            Write::Create {
                body,
                id,
                existing_ids,
            } => {
                let reply = self.client.create(&self.collection, body).await?;
                let created = reply
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty())
                    .ok_or_else(|| {
                        incomplete("creation acknowledgment has no wired network identity")
                    })?;
                if existing_ids.iter().any(|id| id == created)
                    || reply
                        .get("wiredNetworkName")
                        .is_some_and(|name| name != &body["wiredNetworkName"])
                    || reply
                        .get("vlanId")
                        .is_some_and(|vlan| vlan != &body["vlanId"])
                    || reply
                        .get("isWireless")
                        .is_some_and(|wireless| wireless != false)
                {
                    return Err(incomplete(
                        "creation acknowledgment does not identify the new wired network",
                    ));
                }
                *id.lock()
                    .map_err(|_| incomplete("creation identity lock failed"))? =
                    Some(created.to_owned());
                Ok(())
            }
            Write::Delete { id } => {
                let reply = self
                    .client
                    .delete(&item_path(self.client, &self.collection, id)?)
                    .await?;
                check_ack(&reply, id)
            }
        }
    }
}

fn observe(body: &Value, fields: &[&str]) -> State {
    let mut configuration = json!({});
    for field in fields {
        let mut value = body
            .pointer(&format!("/{field}"))
            .cloned()
            .unwrap_or(Value::Null);
        // Us.prepareData sends an empty domain suffix as null.
        if *field == "dhcpScope/domainName" && value == "" {
            value = Value::Null;
        }
        project(&mut configuration, field, value);
    }
    State {
        exists: true,
        fields: configuration,
    }
}

fn project(result: &mut Value, field: &str, value: Value) {
    if let Some((parent, child)) = field.split_once('/') {
        if !result.get(parent).is_some_and(Value::is_object) {
            result[parent] = json!({});
        }
        project(&mut result[parent], child, value);
    } else {
        result[field] = value;
    }
}

fn absent() -> State {
    State {
        exists: false,
        fields: json!({}),
    }
}
fn target(network: &Network) -> Value {
    json!({"id":network.id(),"name":network.name(),"vlan_id":network.0.get("vlanId").and_then(Value::as_u64),"port_mappings":port_mappings(&network.0)})
}

fn port_mappings(body: &Value) -> Option<bool> {
    let devices = body.get("devicePortMappings")?.as_array()?;
    let mut assigned = false;
    for device in devices {
        if !device.is_object() {
            return None;
        }
        for kind in ["portMappings", "trunkMappings"] {
            let Some(entries) = device.get(kind) else {
                continue;
            };
            for entry in entries.as_array()? {
                match entry.get("mapping")?.as_str()? {
                    "tagged" | "untagged" => assigned = true,
                    "absent" | "forbidden" => {}
                    _ => return None,
                }
            }
        }
    }
    Some(assigned)
}

fn reject_duplicates(networks: &[Network], id: &str, patch: &Patch) -> Result<(), Error> {
    for network in networks.iter().filter(|network| network.id() != id) {
        if patch.name.is_some() && network.name() == patch.name.as_deref() {
            return Err(usage("wired network name already exists"));
        }
        if let Some(vlan) = patch.vlan_id {
            let existing = network
                .0
                .get("vlanId")
                .and_then(Value::as_u64)
                .ok_or_else(|| incomplete("existing VLAN IDs are unavailable"))?;
            if existing == u64::from(vlan) {
                return Err(usage("VLAN ID already exists"));
            }
        }
    }
    Ok(())
}

fn check_identity(body: &Value, id: &str) -> Result<(), Error> {
    if body.get("id").and_then(Value::as_str) != Some(id)
        || body
            .get("isWireless")
            .is_some_and(|wireless| wireless != false)
    {
        return Err(incomplete("wired network identity or type changed"));
    }
    Ok(())
}
fn check_ack(reply: &Value, id: &str) -> Result<(), Error> {
    if reply.get("id").is_some_and(|ack| ack.as_str() != Some(id)) {
        return Err(incomplete(
            "acknowledgment identified a different wired network",
        ));
    }
    Ok(())
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
        .ok_or_else(|| usage("invalid wired network route"))
}
fn usage(message: &str) -> Error {
    Error::new(ErrorKind::Usage, message)
}
fn incomplete(message: &str) -> Error {
    Error::new(ErrorKind::Unverified, message)
}
