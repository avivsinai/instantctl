use std::collections::BTreeMap;

use serde_json::{Value, json};

use super::{
    Client, Error, ErrorKind, Network, State, TokenSource, check_identity, incomplete, list, reads,
    route, select, usage,
};
use crate::mutation::{FullObjectPut, Mutation, ObjectResource, Plan};

const SERVICE_TYPES: &[&str] = &[
    "airplay",
    "airprint",
    "itunes",
    "remote-management",
    "sharing",
    "googlecast",
    "AmazonTv",
    "DIAL",
    "DLNA Media",
    "DLNA Print",
    "sonos",
    "spotify",
    "smart-speaker",
];
const SERVICE_FIELDS: &[&str] = &[
    "serviceType",
    "macAddress",
    "ipAddress",
    "vlanId",
    "name",
    "networkId",
    "networkName",
    "isShared",
    "serviceTags",
];

fn request_rows(rows: &[Value]) -> Result<Vec<Value>, Error> {
    rows.iter()
        .map(|row| {
            let mut body = Value::Object(
                SERVICE_FIELDS
                    .iter()
                    .filter_map(|field| {
                        row.get(*field)
                            .map(|value| ((*field).to_owned(), value.clone()))
                    })
                    .collect(),
            );
            let tags = match row.get("serviceTags") {
                None => vec![],
                Some(value) => value
                    .as_array()
                    .ok_or_else(|| incomplete("shared service tags are malformed"))?
                    .iter()
                    .map(|tag| {
                        if tag.get("tag").and_then(Value::as_str).is_none()
                            || tag.get("port").and_then(Value::as_u64).is_none()
                        {
                            return Err(incomplete("shared service tag identity is unavailable"));
                        }
                        Ok(json!({"tag":tag["tag"],"port":tag["port"]}))
                    })
                    .collect::<Result<Vec<_>, Error>>()?,
            };
            body["serviceTags"] = json!(tags);
            Ok(body)
        })
        .collect()
}

fn grouped(body: &Value, field: &str) -> Result<BTreeMap<String, Vec<Value>>, Error> {
    let rows = body
        .get(field)
        .and_then(Value::as_array)
        .ok_or_else(|| incomplete("shared service collection is unavailable"))?;
    let mut groups = BTreeMap::<String, Vec<Value>>::new();
    for row in rows {
        let mac = row
            .get("macAddress")
            .and_then(Value::as_str)
            .filter(|mac| reads::is_mac(mac))
            .ok_or_else(|| incomplete("shared service identity is unavailable"))?;
        groups
            .entry(mac.to_ascii_lowercase())
            .or_default()
            .push(row.clone());
    }
    Ok(groups)
}

fn group_name(rows: &[Value]) -> Option<&str> {
    rows.first()?.get("name")?.as_str()
}
fn group_shared(rows: &[Value]) -> Option<bool> {
    let shared = rows.first()?.get("isShared")?.as_bool()?;
    rows.iter()
        .all(|row| row.get("isShared").and_then(Value::as_bool) == Some(shared))
        .then_some(shared)
}

pub(super) fn summaries(body: &Value) -> Result<Value, Error> {
    let mut rows = Vec::new();
    let disabled = body.get("isSharedServicesEnabled").and_then(Value::as_bool) == Some(false);
    for (field, source) in [
        ("localAirgroupServices", "local"),
        ("sharedAirgroupServices", "other_network"),
    ] {
        if disabled && body.get(field) == Some(&Value::Null) {
            continue;
        }
        for (mac, services) in grouped(body, field)? {
            rows.push(json!({
                "mac":mac, "name":group_name(&services), "source":source,
                "shared":group_shared(&services),
                "types":services.iter().map(|row| row.get("serviceType").and_then(Value::as_str)).collect::<Vec<_>>(),
                "network_id":services.first().and_then(|row| row.get("networkId")).and_then(Value::as_str),
                "network_name":services.first().and_then(|row| row.get("networkName")).and_then(Value::as_str),
            }));
        }
    }
    Ok(Value::Array(rows))
}

fn select_group(groups: &BTreeMap<String, Vec<Value>>, selector: &str) -> Result<String, Error> {
    if reads::is_mac(selector) && groups.contains_key(&selector.to_ascii_lowercase()) {
        return Ok(selector.to_ascii_lowercase());
    }
    let mut matches = groups
        .iter()
        .filter(|(_, services)| group_name(services) == Some(selector));
    let (mac, _) = matches.next().ok_or_else(|| {
        Error::new(
            ErrorKind::NotFound,
            "no other-network shared service matches the selector",
        )
    })?;
    if matches.next().is_some() {
        return Err(usage("shared service name is ambiguous; select by MAC"));
    }
    Ok(mac.clone())
}

fn validate_services(rows: &[Value]) -> Result<(), Error> {
    let mut identities = std::collections::HashSet::new();
    for row in rows {
        let mac = row
            .get("macAddress")
            .and_then(Value::as_str)
            .filter(|mac| reads::is_mac(mac))
            .ok_or_else(|| incomplete("shared service MAC is unavailable"))?;
        let kind = row
            .get("serviceType")
            .and_then(Value::as_str)
            .filter(|kind| SERVICE_TYPES.contains(kind))
            .ok_or_else(|| incomplete("shared service type is unknown"))?;
        let network = row
            .get("networkId")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .ok_or_else(|| incomplete("shared service network identity is unavailable"))?;
        if !identities.insert((mac.to_ascii_lowercase(), kind, network)) {
            return Err(incomplete("shared service identities are duplicated"));
        }
        if row.get("isShared").and_then(Value::as_bool).is_none() {
            return Err(incomplete("shared service state is unavailable"));
        }
    }
    Ok(())
}

fn service_state(network: &Network, mac: &str) -> Result<State, Error> {
    let groups = grouped(&network.0, "sharedAirgroupServices")?;
    let rows = groups.get(mac).ok_or_else(|| {
        Error::new(
            ErrorKind::NotFound,
            "shared service disappeared during readback",
        )
    })?;
    validate_services(rows)?;
    let mut values: Vec<_> = rows.iter().map(|row| json!({
        "serviceType":row["serviceType"], "networkId":row["networkId"], "isShared":row["isShared"]
    })).collect();
    values.sort_by_key(|row| {
        (
            row["serviceType"].as_str().unwrap_or_default().to_owned(),
            row["networkId"].as_str().unwrap_or_default().to_owned(),
        )
    });
    Ok(State {
        exists: true,
        fields: json!({"network_id":network.id(),"mac":mac,"services":values}),
    })
}

pub struct SharedServiceMutation<'a, T> {
    client: &'a Client<T>,
    site: String,
    id: String,
    mac: String,
    body: Value,
    desired: State,
}
impl<'a, T: TokenSource> SharedServiceMutation<'a, T> {
    pub fn target(&self) -> Value {
        json!({"network_id":self.id,"mac":self.mac})
    }

    pub async fn update(
        client: &'a Client<T>,
        site: &str,
        network_selector: &str,
        service_selector: &str,
        shared: bool,
    ) -> Result<(Self, Plan<State>), Error> {
        let networks = list(client, site).await?;
        let network = select(&networks, network_selector)?;
        match network
            .0
            .get("isSharedServicesEnabled")
            .and_then(Value::as_bool)
        {
            Some(true) => {}
            Some(false) => {
                return Err(Error::new(
                    ErrorKind::Unsupported,
                    "shared services are disabled for this wired network",
                ));
            }
            None => return Err(incomplete("shared service availability is unknown")),
        }
        let groups = grouped(&network.0, "sharedAirgroupServices")?;
        let mac = select_group(&groups, service_selector)?;
        let mut rows = network.0["sharedAirgroupServices"]
            .as_array()
            .unwrap()
            .clone();
        validate_services(&rows)?;
        let current = service_state(network, &mac)?;
        for row in &mut rows {
            if row["macAddress"]
                .as_str()
                .is_some_and(|value| value.eq_ignore_ascii_case(&mac))
            {
                row["isShared"] = json!(shared);
            }
        }
        let mut modified = network.clone();
        modified.0["sharedAirgroupServices"] = json!(rows);
        let desired = service_state(&modified, &mac)?;
        let rows = request_rows(&rows)?;
        Ok((
            Self {
                client,
                site: site.to_owned(),
                id: network.id().to_owned(),
                mac,
                body: json!({"updateList":rows}),
                desired: desired.clone(),
            },
            Plan { current, desired },
        ))
    }
}
impl<T: TokenSource> Mutation for SharedServiceMutation<'_, T> {
    type State = State;
    async fn read(&self) -> Result<State, Error> {
        let networks = list(self.client, &self.site).await?;
        let network = select(&networks, &self.id)?;
        check_identity(&network.0, &self.id)?;
        service_state(network, &self.mac)
    }
    async fn write(&self, desired: &State) -> Result<(), Error> {
        if desired != &self.desired {
            return Err(usage(
                "desired state does not match the prepared shared service change",
            ));
        }
        self.client
            .action(
                &route(&self.site, "wiredNetworks")?,
                &self.id,
                "updateSharedService",
                &self.body,
            )
            .await?;
        Ok(())
    }
}

struct ConfigurationResource<'a, T> {
    client: &'a Client<T>,
    path: String,
}
impl<T: TokenSource> ObjectResource for ConfigurationResource<'_, T> {
    async fn read_object(&self) -> Result<Value, Error> {
        self.client.get(&self.path).await
    }
    async fn put_object(&self, body: &Value) -> Result<(), Error> {
        self.client.put_full(&self.path, body).await.map(|_| ())
    }
}
fn configuration_state(body: &Value) -> Result<State, Error> {
    let enabled = body
        .get("isSharedServicesEnabled")
        .and_then(Value::as_bool)
        .ok_or_else(|| incomplete("shared services configuration is unavailable"))?;
    Ok(State {
        exists: true,
        fields: json!({"isSharedServicesEnabled":enabled}),
    })
}
type ConfigurationUpdate<'a, T> =
    FullObjectPut<ConfigurationResource<'a, T>, State, fn(&Value) -> Result<State, Error>>;
pub struct SharedServicesMutation<'a, T> {
    backend: ConfigurationUpdate<'a, T>,
}
impl<'a, T: TokenSource> SharedServicesMutation<'a, T> {
    pub fn target(&self) -> Value {
        json!({"resource":"sharedServicesConfiguration"})
    }
    pub async fn update(
        client: &'a Client<T>,
        site: &str,
        enabled: bool,
    ) -> Result<(Self, Plan<State>), Error> {
        let path = route(site, "sharedServicesConfiguration")?;
        let body = client.get(&path).await?;
        let resource = ConfigurationResource { client, path };
        let (backend, plan) = FullObjectPut::prepare(
            resource,
            body,
            configuration_state as fn(&Value) -> Result<State, Error>,
            |body| {
                body["isSharedServicesEnabled"] = json!(enabled);
                Ok(())
            },
        )?;
        Ok((Self { backend }, plan))
    }
}
impl<T: TokenSource> Mutation for SharedServicesMutation<'_, T> {
    type State = State;
    async fn read(&self) -> Result<State, Error> {
        self.backend.read().await
    }
    async fn write(&self, desired: &State) -> Result<(), Error> {
        self.backend.write(desired).await
    }
}

pub async fn status<T: TokenSource>(client: &Client<T>, site: &str) -> Result<Value, Error> {
    let body = client
        .get(&route(site, "sharedServicesConfiguration")?)
        .await?;
    if !body.is_object() {
        return Err(incomplete("shared services response is not an object"));
    }
    Ok(json!({"enabled":body.get("isSharedServicesEnabled").and_then(Value::as_bool)}))
}
