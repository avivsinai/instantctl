use std::collections::BTreeSet;

use serde_json::{Value, json};

use crate::{Error, ErrorKind};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CreatePortMembership {
    /// Retain the exact port and trunk mappings in the server template.
    Template,
    /// Remove network membership from every eligible port and trunk.
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Mapping {
    Tagged,
    Untagged,
    Absent,
    Forbidden,
}

impl Mapping {
    fn parse(value: Option<&Value>) -> Result<Self, Error> {
        match value.and_then(Value::as_str) {
            Some("tagged") => Ok(Self::Tagged),
            Some("untagged") => Ok(Self::Untagged),
            Some("absent") => Ok(Self::Absent),
            Some("forbidden") => Ok(Self::Forbidden),
            _ => Err(incomplete("wired port mapping state is unknown")),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Tagged => "tagged",
            Self::Untagged => "untagged",
            Self::Absent => "absent",
            Self::Forbidden => "forbidden",
        }
    }

    fn without_membership(self) -> Self {
        match self {
            Self::Tagged | Self::Untagged => Self::Absent,
            Self::Absent | Self::Forbidden => self,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Entry {
    number: u64,
    mapping: Mapping,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Device {
    id: String,
    ports: Vec<Entry>,
    trunks: Vec<Entry>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Membership(Vec<Device>);

impl Membership {
    pub(super) fn prepare(
        mappings: &mut Value,
        policy: CreatePortMembership,
    ) -> Result<Self, Error> {
        let initial = Self::parse(mappings)?;
        if policy == CreatePortMembership::None {
            for device in mappings
                .as_array_mut()
                .ok_or_else(|| incomplete("wired port mappings are unavailable"))?
            {
                for collection in ["portMappings", "trunkMappings"] {
                    for mapping in device[collection]
                        .as_array_mut()
                        .ok_or_else(|| incomplete("wired port mappings are malformed"))?
                    {
                        let value = Mapping::parse(mapping.get("mapping"))?;
                        mapping["mapping"] = json!(value.without_membership().as_str());
                    }
                }
            }
        }
        if policy == CreatePortMembership::Template {
            Ok(initial)
        } else {
            Self::parse(mappings)
        }
    }

    pub(super) fn parse(body: &Value) -> Result<Self, Error> {
        let devices = body
            .as_array()
            .ok_or_else(|| incomplete("wired port mappings are unavailable"))?;
        let mut parsed = Vec::with_capacity(devices.len());
        let mut device_ids = BTreeSet::new();
        for device in devices {
            let id = device
                .get("deviceId")
                .and_then(Value::as_str)
                .filter(|id| !id.trim().is_empty())
                .ok_or_else(|| incomplete("wired port mapping device identity is missing"))?;
            if !device_ids.insert(id.to_owned()) {
                return Err(incomplete(
                    "wired port mapping device identity is duplicated",
                ));
            }

            parsed.push(Device {
                id: id.to_owned(),
                ports: parse_entries(device, "portMappings", "portNumber")?,
                trunks: parse_entries(device, "trunkMappings", "trunkNumber")?,
            });
        }
        parsed.sort_unstable();
        Ok(Self(parsed))
    }

    pub(super) fn has_membership(&self) -> bool {
        self.0.iter().any(|device| {
            device
                .ports
                .iter()
                .any(|entry| matches!(entry.mapping, Mapping::Tagged | Mapping::Untagged))
                || device
                    .trunks
                    .iter()
                    .any(|entry| matches!(entry.mapping, Mapping::Tagged | Mapping::Untagged))
        })
    }

    pub(super) fn to_value(&self) -> Value {
        Value::Array(
            self.0
                .iter()
                .map(|device| {
                    json!({
                        "deviceId": device.id,
                        "portMappings": device.ports.iter().map(|port| json!({
                            "portNumber": port.number,
                            "mapping": port.mapping.as_str(),
                        })).collect::<Vec<_>>(),
                        "trunkMappings": device.trunks.iter().map(|trunk| json!({
                            "trunkNumber": trunk.number,
                            "mapping": trunk.mapping.as_str(),
                        })).collect::<Vec<_>>(),
                    })
                })
                .collect(),
        )
    }
}

fn parse_entries(device: &Value, collection: &str, identity: &str) -> Result<Vec<Entry>, Error> {
    let mut entries = Vec::new();
    let mut numbers = BTreeSet::new();
    for entry in mapping_rows(device, collection)? {
        let number = entry
            .get(identity)
            .and_then(Value::as_u64)
            .ok_or_else(|| incomplete("wired port or trunk mapping identity is missing"))?;
        if !numbers.insert(number) {
            return Err(incomplete(
                "wired port or trunk mapping identity is duplicated",
            ));
        }
        entries.push(Entry {
            number,
            mapping: Mapping::parse(entry.get("mapping"))?,
        });
    }
    entries.sort_unstable();
    Ok(entries)
}

fn mapping_rows<'a>(device: &'a Value, key: &str) -> Result<&'a [Value], Error> {
    device
        .get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .ok_or_else(|| incomplete("wired port mapping collection is missing or malformed"))
}

fn incomplete(message: &str) -> Error {
    Error::new(ErrorKind::Unverified, message)
}
