//! Wireless network CRUD observed in the Instant On portal bundle.
//! Updates retain the entire fetched network. Secrets participate in readback
//! equality but have no printable representation in plans or read results.

mod advanced;
mod config;
mod subnet;

use std::{collections::HashSet, fmt, sync::Mutex};

use serde::{Serialize, Serializer};
use serde_json::{Map, Value, json};

use super::{Client, reads};
use crate::{
    Error, ErrorKind, TokenSource,
    mutation::{FullObjectPut, Mutation, ObjectResource, Plan},
};
pub use advanced::{AccessPoints, AdvancedPatch, Binding, TrafficPriority};
pub use config::{Bands, Bandwidth, Day, Patch, Schedule, Security};

/// Full wire object kept private so read commands cannot expose credentials.
#[derive(Clone)]
pub struct Network(Value);

impl fmt::Debug for Network {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.details().fmt(f)
    }
}

impl Network {
    pub fn id(&self) -> &str {
        // Construction checks every identity.
        self.0["id"].as_str().unwrap_or_default()
    }

    pub fn name(&self) -> Option<&str> {
        self.0["networkName"].as_str()
    }

    pub(crate) fn allow_list_id(&self) -> Result<&str, Error> {
        self.0
            .pointer("/allowList/id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .ok_or_else(|| {
                Error::new(
                    ErrorKind::Unverified,
                    "wireless allowlist identity is unknown",
                )
            })
    }

    pub fn summary(&self) -> Value {
        json!({
            "id": self.id(), "name": self.name(),
            "enabled": self.0.get("isEnabled").and_then(Value::as_bool),
            "security": self.0.get("security").and_then(Value::as_str),
            "hidden": self.0.get("isSsidHidden").and_then(Value::as_bool),
            "clients": self.0.get("wirelessClientsCount").and_then(Value::as_u64),
            "2.4_ghz": self.0.get("isAvailableOn24GHzRadioBand").and_then(Value::as_bool),
            "5_ghz": self.0.get("isAvailableOn5GHzRadioBand").and_then(Value::as_bool),
            "6_ghz": self.0.get("isAvailableOn6GHzRadioBand").and_then(Value::as_bool)
        })
    }

    pub fn details(&self) -> Value {
        let mut result = self.summary();
        for field in config::FIELDS {
            result[field] = self.0.get(*field).cloned().unwrap_or(Value::Null);
        }
        redact(&mut result);
        result
    }
}

/// Actual values are used for verification; serialization and Debug redact PSKs.
#[derive(Clone, PartialEq)]
pub struct State {
    exists: bool,
    fields: Value,
}

impl State {
    fn printable(&self) -> Value {
        let mut fields = self.fields.clone();
        redact(&mut fields);
        json!({"exists": self.exists, "configuration": fields})
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

fn parse_rows(payload: &Value) -> Result<Vec<Value>, Error> {
    let rows: Vec<Value> = reads::parse_elements(payload)?;
    let mut ids = HashSet::new();
    for row in &rows {
        let id = row
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .ok_or_else(|| incomplete("network identity is missing"))?;
        if !ids.insert(id) || !row.get("isWireless").is_some_and(Value::is_boolean) {
            return Err(incomplete(
                "network collection has duplicate identities or unknown network types",
            ));
        }
    }
    Ok(rows)
}

fn parse(payload: &Value) -> Result<Vec<Network>, Error> {
    Ok(parse_rows(payload)?
        .into_iter()
        .filter(|row| row["isWireless"] == true)
        .map(Network)
        .collect())
}

pub async fn list<T: TokenSource>(client: &Client<T>, site: &str) -> Result<Vec<Network>, Error> {
    parse(&client.get(&route(site, "networksSummary")?).await?)
}

pub fn select<'a>(networks: &'a [Network], selector: &str) -> Result<&'a Network, Error> {
    // Stable identifiers take precedence over names.
    if let Some(network) = networks.iter().find(|network| network.id() == selector) {
        return Ok(network);
    }
    let mut matches = networks
        .iter()
        .filter(|network| network.name() == Some(selector));
    let network = matches.next().ok_or_else(|| {
        Error::new(
            ErrorKind::NotFound,
            "no wireless network matches the supplied selector",
        )
    })?;
    if matches.next().is_some() {
        return Err(usage(
            "wireless network name is ambiguous; select by identifier",
        ));
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
        let path = item_path(self.client, &self.collection, &self.id)?;
        let reply = self.client.put_full(&path, body).await?;
        if let Some(id) = reply.get("id")
            && id.as_str() != Some(self.id.as_str())
        {
            return Err(incomplete(
                "update acknowledgment identified a different wireless network",
            ));
        }
        Ok(())
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

pub struct WlanMutation<'a, T> {
    client: &'a Client<T>,
    collection: String,
    target: Value,
    fields: Vec<&'static str>,
    desired: State,
    write: Write<'a, T>,
}

impl<'a, T: TokenSource> WlanMutation<'a, T> {
    pub fn target(&self) -> Value {
        self.target.clone()
    }

    pub async fn update(
        client: &'a Client<T>,
        site: &str,
        selector: &str,
        mut patch: Patch,
    ) -> Result<(Self, Plan<State>), Error> {
        patch.validate()?;
        if patch.is_empty() {
            return Err(usage("specify at least one wireless network change"));
        }
        let collection = route(site, "networksSummary")?;
        let networks = list(client, site).await?;
        let network = select(&networks, selector)?;
        reject_duplicate_name(&networks, network.id(), patch.name.as_deref())?;
        let id = network.id().to_owned();
        patch.advanced.resolve(client, site, &network.0).await?;
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
                patch.apply(body)
            })?;
        let mutation = Self {
            client,
            collection,
            target: target(network),
            fields,
            desired: plan.desired.clone(),
            write: Write::Update(backend),
        };
        Ok((mutation, plan))
    }

    pub async fn create(
        client: &'a Client<T>,
        site: &str,
        mut patch: Patch,
    ) -> Result<(Self, Plan<State>), Error> {
        patch.validate()?;
        if patch.name.is_none() {
            return Err(usage("a new wireless network requires a name"));
        }
        let collection = route(site, "networksSummary")?;
        let rows = parse_rows(&client.get(&collection).await?)?;
        let existing_ids = rows
            .iter()
            .filter_map(|row| row["id"].as_str().map(str::to_owned))
            .collect();
        let networks: Vec<_> = rows
            .into_iter()
            .filter(|row| row["isWireless"] == true)
            .map(Network)
            .collect();
        reject_duplicate_name(&networks, "", patch.name.as_deref())?;
        // The portal copies this server-supplied template, rather than making up
        // defaults. Its serializer omits id and all telemetry fields.
        let defaults = client.get(&route(site, "wiredNetworks")?).await?;
        let template = defaults
            .pointer("/metaData/defaultWirelessNetwork")
            .filter(|v| v.is_object())
            .ok_or_else(|| incomplete("wireless creation template is missing"))?;
        let mut body = Value::Object(
            config::FIELDS
                .iter()
                .filter_map(|key| {
                    template
                        .get(*key)
                        .map(|value| ((*key).to_owned(), value.clone()))
                })
                .collect(),
        );
        body["isWireless"] = json!(true);
        patch.advanced.resolve(client, site, &body).await?;
        let mut fields = patch.fields();
        patch.apply(&mut body)?;
        config::validate_create(&body)?;
        config::prepare_create(&mut body)?;
        if body["ipAddressingMode"] == "internal"
            && body.get("dhcpScope").is_some_and(Value::is_object)
        {
            let reserved = client.get(&route(site, "reservedIpSubnets")?).await?;
            subnet::allocate(&mut body["dhcpScope"], &reserved)?;
        }
        // As with an update, verify the owned configuration rather than
        // returned runtime metadata or IDs generated for nested resources.
        fields.extend([
            "networkName",
            "isWireless",
            "authentication",
            "security",
            "isEnabled",
            "isSsidHidden",
            "type",
            "ipAddressingMode",
            "isAvailableOn24GHzRadioBand",
            "isAvailableOn5GHzRadioBand",
            "isAvailableOn6GHzRadioBand",
        ]);
        if body["authentication"] == "psk" {
            fields.push("preSharedKey");
        }
        if body["ipAddressingMode"] == "internal" && body.get("dhcpScope").is_some() {
            fields.push("dhcpScope");
        }
        fields.sort_unstable();
        fields.dedup();
        let desired = observe(&body, &fields);
        let plan = Plan {
            current: absent(),
            desired: desired.clone(),
        };
        let target =
            json!({"id": Value::Null, "name": body["networkName"], "clients": Value::Null});
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
        let collection = route(site, "networksSummary")?;
        let networks = list(client, site).await?;
        let network = select(&networks, selector)?;
        if network
            .0
            .get("wirelessClientsCount")
            .and_then(Value::as_u64)
            != Some(0)
            && !yes
        {
            return Err(usage(
                "deleting a wireless network with clients or an unknown client count requires --yes",
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

impl<T: TokenSource> Mutation for WlanMutation<'_, T> {
    type State = State;

    async fn read(&self) -> Result<State, Error> {
        if let Write::Update(backend) = &self.write {
            return backend.read().await;
        }
        let payload = self.client.get(&self.collection).await?;
        if let Write::Delete { id } = &self.write {
            let rows = parse_rows(&payload)?;
            if let Some(row) = rows.iter().find(|row| row["id"].as_str() == Some(id)) {
                check_identity(row, id)?;
                return Ok(State {
                    exists: true,
                    fields: json!({}),
                });
            }
            return Ok(absent());
        }
        let networks = parse(&payload)?;
        let id = match &self.write {
            Write::Delete { id } => Some(id.clone()),
            Write::Create { id, .. } => id
                .lock()
                .map_err(|_| incomplete("creation identity lock failed"))?
                .clone(),
            Write::Update(_) => unreachable!("update read delegated above"),
        };
        let row = if let Some(id) = id {
            networks.iter().find(|row| row.id() == id)
        } else {
            let name = self.target["name"]
                .as_str()
                .ok_or_else(|| incomplete("creation name is missing"))?;
            match select(&networks, name) {
                Ok(network) => Some(network),
                Err(error) if error.kind == ErrorKind::NotFound => None,
                Err(error) => return Err(error),
            }
        };
        Ok(row.map_or_else(absent, |row| observe(&row.0, &self.fields)))
    }

    async fn write(&self, desired: &State) -> Result<(), Error> {
        if desired != &self.desired {
            return Err(usage(
                "desired state does not match the prepared wireless change",
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
                        incomplete("creation acknowledgment has no wireless network identity")
                    })?;
                if existing_ids.iter().any(|id| id == created)
                    || reply.get("isWireless").is_some_and(|value| value != true)
                    || reply
                        .get("networkName")
                        .is_some_and(|value| value != &body["networkName"])
                {
                    return Err(incomplete(
                        "creation acknowledgment does not identify a new wireless network",
                    ));
                }
                *id.lock()
                    .map_err(|_| incomplete("creation identity lock failed"))? =
                    Some(created.to_owned());
                Ok(())
            }
            Write::Delete { id } => {
                let path = item_path(self.client, &self.collection, id)?;
                let reply = self.client.delete(&path).await?;
                if let Some(ack_id) = reply.get("id")
                    && ack_id.as_str() != Some(id)
                {
                    return Err(incomplete(
                        "delete acknowledgment identified a different wireless network",
                    ));
                }
                Ok(())
            }
        }
    }
}

fn target(network: &Network) -> Value {
    json!({"id": network.id(), "name": network.name(), "clients": network.0.get("wirelessClientsCount").and_then(Value::as_u64)})
}

fn check_identity(body: &Value, id: &str) -> Result<(), Error> {
    if body.get("id").and_then(Value::as_str) != Some(id)
        || body.get("isWireless") != Some(&Value::Bool(true))
    {
        return Err(incomplete("wireless network identity or type changed"));
    }
    Ok(())
}

fn reject_duplicate_name(networks: &[Network], id: &str, name: Option<&str>) -> Result<(), Error> {
    if name.is_some()
        && networks
            .iter()
            .any(|network| network.id() != id && network.name() == name)
    {
        return Err(usage("wireless network name already exists"));
    }
    Ok(())
}

fn observe(body: &Value, fields: &[&str]) -> State {
    State {
        exists: true,
        fields: Value::Object(
            fields
                .iter()
                .map(|field| {
                    (
                        (*field).to_owned(),
                        if *field == "schedule" {
                            schedule_configuration(body.get(*field).unwrap_or(&Value::Null))
                        } else if *field == "dhcpScope" {
                            body.get(*field).filter(|scope| scope.is_object()).map_or(Value::Null, |scope| json!({"network":scope.get("network"),"netmask":scope.get("netmask")}))
                        } else {
                            body.get(*field).cloned().unwrap_or(Value::Null)
                        },
                    )
                })
                .collect::<Map<_, _>>(),
        ),
    }
}

fn schedule_configuration(schedule: &Value) -> Value {
    if !schedule.is_object() {
        return Value::Null;
    }
    let range = schedule.get("activeTimeRange").unwrap_or(&Value::Null);
    let days = schedule
        .get("activeDays")
        .and_then(Value::as_array)
        .filter(|days| days.iter().all(Value::is_string))
        .map(|days| {
            let mut days = days.clone();
            days.sort_by(|left, right| left.as_str().cmp(&right.as_str()));
            days
        });
    let mut configuration =
        json!({"activeDays": days, "activeTimeRange": {"enabled": range.get("enabled")}});
    if range.get("enabled") == Some(&Value::Bool(true)) {
        configuration["activeTimeRange"]["startTime"] =
            range.get("startTime").cloned().unwrap_or(Value::Null);
        configuration["activeTimeRange"]["endTime"] =
            range.get("endTime").cloned().unwrap_or(Value::Null);
    }
    configuration
}

fn absent() -> State {
    State {
        exists: false,
        fields: json!({}),
    }
}

fn usage(message: &str) -> Error {
    Error::new(ErrorKind::Usage, message)
}

fn incomplete(message: &str) -> Error {
    Error::new(ErrorKind::Unverified, message)
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
        .ok_or_else(|| usage("invalid wireless network route"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_summary_scalars_are_null_and_never_expose_nested_credentials() {
        let secret = "malformed-response-PSK-sentinel";
        let rows = parse(&json!({"elements":[{
            "id":"wlan1", "isWireless":true,
            "security":{"preSharedKey":secret},
            "isEnabled":{"password":secret},
            "wirelessClientsCount":{"token":secret}
        }]}))
        .unwrap();
        let network = &rows[0];
        for field in [
            "name", "enabled", "security", "hidden", "clients", "2.4_ghz", "5_ghz", "6_ghz",
        ] {
            assert!(network.summary()[field].is_null(), "{field}");
        }
        for printable in [network.summary(), network.details(), target(network)] {
            assert!(!printable.to_string().contains(secret));
        }
        assert!(!format!("{network:?}").contains(secret));
    }

    #[test]
    fn schedule_readback_verifies_configuration_while_runtime_state_changes() {
        let desired = json!({"activeSchedule":"simple", "schedule":{
            "activeDays":["monday","friday"], "activeTimeRange":{"enabled":true,"startTime":"08:00","endTime":"18:00"},
            "state":{"active":true,"nextStateChangeDay":"monday"}}});
        let mut observed = desired.clone();
        observed["schedule"]["state"] = json!({"active":false,"nextStateChangeDay":"tuesday"});
        observed["schedule"]["activeDays"] = json!(["friday", "monday"]);
        assert_eq!(
            observe(&desired, &["activeSchedule", "schedule"]),
            observe(&observed, &["activeSchedule", "schedule"])
        );
        observed["schedule"]["activeTimeRange"]["endTime"] = json!("17:00");
        assert_ne!(
            observe(&desired, &["activeSchedule", "schedule"]),
            observe(&observed, &["activeSchedule", "schedule"])
        );
    }
}
