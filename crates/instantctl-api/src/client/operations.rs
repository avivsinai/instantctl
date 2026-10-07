//! Client writes observed in the Instant On portal bundle.
//! Each plan binds a stable identity, sends once through the mutation engine,
//! and reads the owned state from its authoritative GET resource.
use std::{collections::HashSet, net::Ipv4Addr};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{Client, reads};
use crate::{
    Error, ErrorKind, TokenSource,
    mutation::{FullObjectPut, Mutation, ObjectResource, Plan},
};

#[derive(Clone, Debug)]
pub enum Change {
    Rename(String),
    Block,
    Unblock,
    ReserveIp { network: String, ip: Ipv4Addr },
    Watchlist(bool),
    PowerCycle,
    Tags { mode: TagMode, tags: Vec<String> },
}

#[derive(Clone, Copy, Debug)]
pub enum TagMode {
    Set,
    Add,
    Remove,
}

#[derive(Clone, Debug, Serialize)]
pub struct Target {
    pub id: String,
    pub mac: String,
    pub name: Option<String>,
    pub ip: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum State {
    Name {
        name: String,
    },
    Blocked {
        blocked: bool,
    },
    Watchlisted {
        watchlisted: bool,
    },
    Reservations {
        ip_reservations: Vec<IpReservation>,
    },
    Tags {
        tags: Vec<String>,
    },
    /// This proves initiation, not completion of the power cycle.
    PowerCycling {
        is_power_cycling: bool,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IpReservation {
    pub network_id: String,
    pub ip_address: Ipv4Addr,
}

type ObserveName = Box<dyn Fn(&Value) -> Result<State, Error> + Send + Sync>;
type Rename<'a, T> = FullObjectPut<ClientDetailsResource<'a, T>, State, ObserveName>;

struct ClientDetailsResource<'a, T> {
    client: &'a Client<T>,
    path: String,
    id: String,
    mac: String,
}

impl<T: TokenSource> ObjectResource for ClientDetailsResource<'_, T> {
    async fn read_object(&self) -> Result<Value, Error> {
        self.client.get(&self.path).await
    }

    async fn put_object(&self, body: &Value) -> Result<(), Error> {
        if body.get("id").and_then(Value::as_str) != Some(self.id.as_str())
            || !body
                .get("macAddress")
                .and_then(Value::as_str)
                .is_some_and(|mac| mac.eq_ignore_ascii_case(&self.mac))
        {
            return Err(Error::new(
                ErrorKind::Config,
                "full-object update changed the client identity",
            ));
        }
        let response = self.client.put_full(&self.path, body).await?;
        if let Some(id) = response.get("id")
            && id.as_str() != Some(self.id.as_str())
        {
            return Err(Error::new(
                ErrorKind::General,
                "update acknowledgment identified a different client",
            ));
        }
        Ok(())
    }
}

enum Write<'a, T> {
    Rename(Rename<'a, T>),
    Create {
        path: String,
        body: Value,
    },
    Delete {
        path: String,
    },
    Action {
        path: String,
        action: &'static str,
        body: Value,
        collection: bool,
    },
}

enum Read {
    Name,
    Blocked,
    Watchlist,
    Reservations,
    Tags,
    PowerCycle { device: String, port: u64 },
}

pub struct ClientMutation<'a, T> {
    client: &'a Client<T>,
    site: String,
    target: Target,
    write: Write<'a, T>,
    read: Read,
    desired: State,
}

struct Snapshot {
    clients: Vec<reads::ClientSummary>,
    scopes: Value,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BlockedClient {
    id: String,
    mac_address: String,
    name: Option<String>,
}

impl<'a, T: TokenSource> ClientMutation<'a, T> {
    pub fn target(&self) -> &Target {
        &self.target
    }

    pub async fn plan(
        client: &'a Client<T>,
        site: &str,
        selector: &str,
        change: Change,
    ) -> Result<(Self, Plan<State>), Error> {
        if !reads::valid_site_id(site) {
            return Err(Error::new(ErrorKind::Config, "--site must be a UUID"));
        }
        let site = site.to_ascii_lowercase();
        if matches!(change, Change::Unblock) {
            let blocked = blocked_clients(client, &site).await?;
            let row = select(&blocked, selector, |r| {
                (&r.id, &r.mac_address, r.name.as_deref())
            })?;
            let target = Target {
                id: row.id.clone(),
                mac: row.mac_address.clone(),
                name: row.name.clone(),
                ip: None,
            };
            let path = item_path(client, &route(&site, "blockedClients"), &row.id)?;
            return Ok(Self::prepared(
                client,
                site,
                target,
                Write::Delete { path },
                Read::Blocked,
                State::Blocked { blocked: true },
                State::Blocked { blocked: false },
            ));
        }
        let snapshot = snapshot(client, &site).await?;
        let row = select(&snapshot.clients, selector, |r| {
            (&r.id, &r.mac_address, r.name.as_deref())
        })?;
        let target = Target {
            id: row.id.clone(),
            mac: row.mac_address.clone(),
            name: row.name.clone(),
            ip: row.ip_address.clone(),
        };
        let (write, read, current, desired) = match change {
            Change::Rename(name) => {
                validate_text(&name, true)?;
                let path = item_path(client, &route(&site, "clientDetails"), &target.id)?;
                let id = target.id.clone();
                let mac = target.mac.clone();
                let observe: ObserveName = Box::new(move |body| {
                    if body.get("kind").and_then(Value::as_str) != Some("clientDetails")
                        || body.get("id").and_then(Value::as_str) != Some(id.as_str())
                        || !body
                            .get("macAddress")
                            .and_then(Value::as_str)
                            .is_some_and(|v| v.eq_ignore_ascii_case(&mac))
                    {
                        return Err(incomplete("client details identity or kind changed"));
                    }
                    let mut name = string(body, "name")?;
                    if name.is_empty() {
                        name = string(body, "defaultName")?;
                    }
                    Ok(State::Name {
                        name: name.to_owned(),
                    })
                });
                let current_body = client.get(&path).await?;
                let resource = ClientDetailsResource {
                    client,
                    path,
                    id: target.id.clone(),
                    mac: target.mac.clone(),
                };
                let (backend, plan) =
                    FullObjectPut::prepare(resource, current_body, observe, move |body| {
                        body["name"] = if body.get("defaultName").and_then(Value::as_str)
                            == Some(name.as_str())
                        {
                            json!("")
                        } else {
                            json!(name)
                        };
                        Ok(())
                    })?;
                (
                    Write::Rename(backend),
                    Read::Name,
                    plan.current,
                    plan.desired,
                )
            }
            Change::Block => {
                supported(
                    row.is_blockable,
                    "client cannot be blocked",
                    "client block capability is unknown",
                )?;
                if blocked_clients(client, &site)
                    .await?
                    .iter()
                    .any(|r| r.mac_address.eq_ignore_ascii_case(&target.mac))
                {
                    return Err(usage("client is already blocked"));
                }
                (
                    Write::Create {
                        path: route(&site, "blockedClients"),
                        body: json!({"kind":"blockedClients","macAddress":target.mac}),
                    },
                    Read::Blocked,
                    State::Blocked { blocked: false },
                    State::Blocked { blocked: true },
                )
            }
            Change::Watchlist(desired) => {
                supported(
                    row.is_watchable,
                    "client cannot be watchlisted",
                    "client watchlist capability is unknown",
                )?;
                let current = row
                    .is_watchlisted
                    .ok_or_else(|| incomplete("client watchlist state is unknown"))?;
                (
                    Write::Action {
                        path: route(&site, "clientDetails"),
                        action: if desired {
                            "addToWatchlist"
                        } else {
                            "removeFromWatchlist"
                        },
                        body: json!({}),
                        collection: false,
                    },
                    Read::Watchlist,
                    State::Watchlisted {
                        watchlisted: current,
                    },
                    State::Watchlisted {
                        watchlisted: desired,
                    },
                )
            }
            Change::ReserveIp { network, ip } => {
                supported(
                    row.can_reserve_ip_address,
                    "client cannot reserve an IP",
                    "client IP reservation capability is unknown",
                )?;
                let mut desired = reservations(&snapshot.scopes, &target)?;
                let scopes = scopes(&snapshot.scopes)?;
                let scope = scopes
                    .iter()
                    .find(|s| s.get("networkId").and_then(Value::as_str) == Some(network.as_str()))
                    .ok_or_else(|| Error::new(ErrorKind::NotFound, "DHCP network was not found"))?;
                if scope.get("isDhcpServer").and_then(Value::as_bool) != Some(true) {
                    return Err(usage("selected network is not a DHCP server"));
                }
                for entry in reservation_entries(scope)? {
                    if reservation_ip(entry)? == ip && !reservation_matches(entry, &target)? {
                        return Err(usage("IP address is reserved for another client or device"));
                    }
                }
                let current = desired.clone();
                desired.retain(|r| r.network_id != network);
                desired.push(IpReservation {
                    network_id: network,
                    ip_address: ip,
                });
                desired.sort_by(|a, b| a.network_id.cmp(&b.network_id));
                let body = json!({"ipReservations":desired});
                (
                    Write::Action {
                        path: route(&site, "clientSummary"),
                        action: "reserveIp",
                        body,
                        collection: false,
                    },
                    Read::Reservations,
                    State::Reservations {
                        ip_reservations: current,
                    },
                    State::Reservations {
                        ip_reservations: desired,
                    },
                )
            }
            Change::Tags { mode, tags } => {
                for tag in &tags {
                    validate_text(tag, false)?;
                }
                let mut requested = HashSet::new();
                if tags.iter().any(|tag| !requested.insert(tag.as_str())) {
                    return Err(usage("tag arguments must be unique"));
                }
                let existing = classification(row.classification.as_ref())?;
                let current = tag_names(&existing)?;
                let desired = match mode {
                    TagMode::Set => {
                        let mut tags = tags;
                        tags.sort();
                        tags
                    }
                    TagMode::Add => {
                        let mut names = current.clone();
                        names.extend(tags);
                        names.sort();
                        names.dedup();
                        names
                    }
                    TagMode::Remove => current
                        .iter()
                        .filter(|tag| !requested.contains(tag.as_str()))
                        .cloned()
                        .collect(),
                };
                let tags: Vec<Value> = desired
                    .iter()
                    .map(|name| {
                        existing
                            .iter()
                            .find(|tag| {
                                tag.get("str").and_then(Value::as_str) == Some(name.as_str())
                            })
                            .and_then(|tag| tag.get("id").and_then(Value::as_str))
                            .filter(|id| !id.is_empty())
                            .map_or_else(|| json!({"str":name}), |id| json!({"id":id}))
                    })
                    .collect();
                (
                    Write::Action {
                        path: route(&site, "clientClassifications"),
                        action: "replaceCommonTags",
                        body: json!({"clientIds":[target.id],"tags":tags}),
                        collection: true,
                    },
                    Read::Tags,
                    State::Tags { tags: current },
                    State::Tags { tags: desired },
                )
            }
            Change::PowerCycle => {
                let ports = ports(row)?;
                for port in ports {
                    boolean(port, "isPoweredByPort")?;
                    boolean(port, "isPowerCyclable")?;
                }
                let candidates: Vec<_> = ports
                    .iter()
                    .filter(|port| {
                        port.get("isPoweredByPort") == Some(&Value::Bool(true))
                            && port.get("isPowerCyclable") == Some(&Value::Bool(true))
                    })
                    .collect();
                let port = match candidates.as_slice() {
                    [port] => *port,
                    [] => {
                        return Err(Error::new(
                            ErrorKind::Unsupported,
                            "client has no reported power-cyclable port",
                        ));
                    }
                    _ => return Err(usage("client has multiple power-cyclable ports")),
                };
                let cycling = boolean(port, "isPowerCycling")?;
                if cycling {
                    return Err(usage("client port is already power cycling"));
                }
                let device = string(port, "deviceId")?.to_owned();
                if device.is_empty() {
                    return Err(incomplete("client port has no device identity"));
                }
                let number = port
                    .get("portNumber")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| incomplete("client port number is unknown"))?;
                (
                    Write::Action {
                        path: route(&site, "clientDetails"),
                        action: "powerCycle",
                        body: json!({}),
                        collection: false,
                    },
                    Read::PowerCycle {
                        device,
                        port: number,
                    },
                    State::PowerCycling {
                        is_power_cycling: false,
                    },
                    State::PowerCycling {
                        is_power_cycling: true,
                    },
                )
            }
            Change::Unblock => unreachable!("unblock is planned from blockedClients"),
        };
        Ok(Self::prepared(
            client, site, target, write, read, current, desired,
        ))
    }

    fn prepared(
        client: &'a Client<T>,
        site: String,
        target: Target,
        write: Write<'a, T>,
        read: Read,
        current: State,
        desired: State,
    ) -> (Self, Plan<State>) {
        let plan = Plan {
            current,
            desired: desired.clone(),
        };
        (
            Self {
                client,
                site,
                target,
                write,
                read,
                desired,
            },
            plan,
        )
    }
}

impl<T: TokenSource> Mutation for ClientMutation<'_, T> {
    type State = State;
    async fn read(&self) -> Result<State, Error> {
        if let Write::Rename(backend) = &self.write {
            return backend.read().await;
        }
        if matches!(self.read, Read::Blocked) {
            return Ok(State::Blocked {
                blocked: blocked_clients(self.client, &self.site)
                    .await?
                    .iter()
                    .any(|r| r.mac_address.eq_ignore_ascii_case(&self.target.mac)),
            });
        }
        let snapshot = snapshot(self.client, &self.site).await?;
        let row = snapshot
            .clients
            .iter()
            .find(|r| {
                r.id == self.target.id && r.mac_address.eq_ignore_ascii_case(&self.target.mac)
            })
            .ok_or_else(|| {
                Error::new(ErrorKind::NotFound, "client was not found during readback")
            })?;
        match &self.read {
            Read::Watchlist => Ok(State::Watchlisted {
                watchlisted: row
                    .is_watchlisted
                    .ok_or_else(|| incomplete("client watchlist state is unknown"))?,
            }),
            Read::Reservations => Ok(State::Reservations {
                ip_reservations: reservations(&snapshot.scopes, &self.target)?,
            }),
            Read::Tags => Ok(State::Tags {
                tags: tag_names(&classification(row.classification.as_ref())?)?,
            }),
            Read::PowerCycle { device, port } => {
                let matches: Vec<_> = ports(row)?
                    .iter()
                    .filter(|p| {
                        p.get("deviceId").and_then(Value::as_str) == Some(device.as_str())
                            && p.get("portNumber").and_then(Value::as_u64) == Some(*port)
                    })
                    .collect();
                if matches.len() != 1 {
                    return Err(incomplete("client port identity changed during readback"));
                }
                Ok(State::PowerCycling {
                    is_power_cycling: boolean(matches[0], "isPowerCycling")?,
                })
            }
            _ => Err(incomplete("client mutation readback is unavailable")),
        }
    }

    async fn write(&self, desired: &State) -> Result<(), Error> {
        if *desired != self.desired {
            return Err(Error::new(
                ErrorKind::Config,
                "desired state does not match the prepared client mutation",
            ));
        }
        match &self.write {
            Write::Rename(backend) => backend.write(desired).await?,
            Write::Create { path, body } => {
                self.client.create(path, body).await?;
            }
            Write::Delete { path } => {
                self.client.delete(path).await?;
            }
            Write::Action {
                path,
                action,
                body,
                collection,
            } => {
                if *collection {
                    self.client
                        .create(&format!("{path}?action={action}"), body)
                        .await?;
                } else {
                    self.client
                        .action(path, &self.target.id, action, body)
                        .await?;
                }
            }
        }
        Ok(())
    }
}

fn route(site: &str, resource: &str) -> String {
    format!("/sites/{site}/{resource}")
}

fn item_path<T: TokenSource>(client: &Client<T>, path: &str, id: &str) -> Result<String, Error> {
    let url = client.resource_url(path, &[id], None)?;
    let prefix = client.base.path().trim_end_matches('/');
    Ok(url
        .path()
        .strip_prefix(prefix)
        .ok_or_else(|| Error::new(ErrorKind::Config, "invalid client resource route"))?
        .to_owned())
}

async fn snapshot<T: TokenSource>(client: &Client<T>, site: &str) -> Result<Snapshot, Error> {
    let payload = client.get(&route(site, "clientSummary")).await?;
    Ok(Snapshot {
        clients: reads::parse_clients(&payload)?,
        scopes: payload
            .pointer("/metaData/networkScopes")
            .cloned()
            .unwrap_or(Value::Null),
    })
}

async fn blocked_clients<T: TokenSource>(
    client: &Client<T>,
    site: &str,
) -> Result<Vec<BlockedClient>, Error> {
    let payload = client.get(&route(site, "blockedClients")).await?;
    let rows: Vec<BlockedClient> = reads::parse_elements(&payload)?;
    reads::unique(rows.iter().map(|r| r.id.as_str()), false)?;
    reads::unique(rows.iter().map(|r| r.mac_address.as_str()), true)?;
    if let Some(pending) = payload.get("pendingAvailability")
        && !(pending.is_null()
            || pending == false
            || pending == 0
            || pending.as_array().is_some_and(Vec::is_empty))
    {
        return Err(incomplete(
            "blocked client collection has pending availability",
        ));
    }
    Ok(rows)
}

fn select<'a, R, F>(rows: &'a [R], selector: &str, fields: F) -> Result<&'a R, Error>
where
    F: for<'b> Fn(&'b R) -> (&'b str, &'b str, Option<&'b str>),
{
    let matches: Vec<_> = rows
        .iter()
        .filter(|r| {
            let (id, mac, name) = fields(r);
            id == selector || mac.eq_ignore_ascii_case(selector) || name == Some(selector)
        })
        .collect();
    match matches.as_slice() {
        [row] => Ok(row),
        [] => Err(Error::new(ErrorKind::NotFound, "client was not found")),
        _ => Err(usage("client selector is ambiguous; use its MAC address")),
    }
}

fn supported(value: Option<bool>, unsupported: &str, unknown: &str) -> Result<(), Error> {
    match value {
        Some(true) => Ok(()),
        Some(false) => Err(Error::new(ErrorKind::Unsupported, unsupported)),
        None => Err(incomplete(unknown)),
    }
}

fn string<'a>(value: &'a Value, key: &str) -> Result<&'a str, Error> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| incomplete("client response is missing a required string"))
}
fn boolean(value: &Value, key: &str) -> Result<bool, Error> {
    value
        .get(key)
        .and_then(Value::as_bool)
        .ok_or_else(|| incomplete("client response is missing a required boolean"))
}
fn validate_text(text: &str, empty: bool) -> Result<(), Error> {
    if (!empty && text.trim().is_empty()) || text.chars().any(char::is_control) {
        return Err(usage("client name or tag contains invalid text"));
    }
    Ok(())
}
fn scopes(value: &Value) -> Result<&Vec<Value>, Error> {
    let scopes = value
        .as_array()
        .filter(|v| v.iter().all(Value::is_object))
        .ok_or_else(|| incomplete("DHCP network scopes are unknown"))?;
    reads::unique(
        scopes
            .iter()
            .map(|s| s.get("networkId").and_then(Value::as_str).unwrap_or("")),
        false,
    )?;
    Ok(scopes)
}
fn reservation_entries(scope: &Value) -> Result<&Vec<Value>, Error> {
    scope
        .pointer("/dhcpScope/ipReservations")
        .and_then(Value::as_array)
        .filter(|v| v.iter().all(Value::is_object))
        .ok_or_else(|| incomplete("DHCP reservation list is unknown"))
}
fn reservation_matches(row: &Value, target: &Target) -> Result<bool, Error> {
    let mac = string(row, "macAddress")?;
    if !reads::is_mac(mac) {
        return Err(incomplete("DHCP reservation MAC address is invalid"));
    }
    let mac_matches = mac.eq_ignore_ascii_case(&target.mac);
    // DHCP uses MAC identity; when the server also supplies a client join,
    // require the two identities to agree before preserving or replacing it.
    let client_id = match row.get("clientId") {
        None | Some(Value::Null) => None,
        Some(Value::String(id)) if id.is_empty() => None,
        Some(Value::String(id)) => Some(id.as_str()),
        _ => return Err(incomplete("DHCP reservation client identity is invalid")),
    };
    if let Some(client_id) = client_id
        && (client_id == target.id) != mac_matches
    {
        return Err(incomplete(
            "DHCP reservation client identity is inconsistent",
        ));
    }
    Ok(mac_matches)
}
fn reservation_ip(row: &Value) -> Result<Ipv4Addr, Error> {
    string(row, "ipAddress")?
        .parse()
        .map_err(|_| incomplete("DHCP reservation IP address is invalid"))
}
fn reservations(value: &Value, target: &Target) -> Result<Vec<IpReservation>, Error> {
    let mut result = Vec::new();
    for scope in scopes(value)? {
        match scope.get("isDhcpServer").and_then(Value::as_bool) {
            Some(false) => continue,
            Some(true) => {}
            None => return Err(incomplete("DHCP server state is unknown")),
        }
        let mut matching = Vec::new();
        for row in reservation_entries(scope)? {
            if reservation_matches(row, target)? {
                matching.push(row);
            }
        }
        if matching.len() > 1 {
            return Err(incomplete(
                "client has duplicate DHCP reservations in a network",
            ));
        }
        if let Some(row) = matching.first() {
            let ip_address = reservation_ip(row)?;
            for entry in reservation_entries(scope)? {
                if !reservation_matches(entry, target)? && reservation_ip(entry)? == ip_address {
                    return Err(incomplete(
                        "client IP reservation conflicts with another client or device",
                    ));
                }
            }
            result.push(IpReservation {
                network_id: string(scope, "networkId")?.to_owned(),
                ip_address,
            });
        }
    }
    result.sort_by(|a, b| a.network_id.cmp(&b.network_id));
    Ok(result)
}
fn classification(value: Option<&Value>) -> Result<Vec<Value>, Error> {
    value
        .and_then(Value::as_array)
        .filter(|v| v.iter().all(Value::is_object))
        .cloned()
        .ok_or_else(|| incomplete("client classification is unknown"))
}
fn tag_names(tags: &[Value]) -> Result<Vec<String>, Error> {
    let mut names = Vec::new();
    for tag in tags {
        let name = string(tag, "str")?;
        validate_text(name, false)?;
        names.push(name.to_owned());
    }
    names.sort();
    if names.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(incomplete("client classification contains duplicate names"));
    }
    Ok(names)
}
fn ports(row: &reads::ClientSummary) -> Result<&Vec<Value>, Error> {
    row.connected_to_ports
        .as_ref()
        .and_then(Value::as_array)
        .filter(|v| v.iter().all(Value::is_object))
        .ok_or_else(|| incomplete("client port connection is unknown"))
}
fn incomplete(message: &str) -> Error {
    Error::new(ErrorKind::Unverified, message)
}
fn usage(message: &str) -> Error {
    Error::new(ErrorKind::Usage, message)
}
