//! Site and AP radio configuration from the portal's radio serializers.
//! Offered channels are fetched from DRT; no local regulatory table is used.
mod config;
pub use config::{Band, BandMapping, Patch, Power, Width};

use serde_json::{Value, json};

use super::{Client, reads};
use crate::{
    Error, ErrorKind, TokenSource,
    mutation::{FullObjectPut, Mutation, ObjectResource, Prepared},
};
use config::{BANDS, CONFIG_FIELDS, channels, current, unknown, usage, visible_config};

fn route(site: &str, name: &str) -> Result<String, Error> {
    crate::inventory::validate_site_id(site)?;
    Ok(format!("/sites/{site}/{name}"))
}
fn site_object(body: &Value) -> Result<(), Error> {
    if !body.is_object()
        || body
            .get("kind")
            .is_some_and(|kind| kind != "radioManagement")
    {
        return Err(unknown("radio management resource identity is unknown"));
    }
    Ok(())
}
fn boolean(body: &Value, path: &str) -> Result<bool, Error> {
    body.pointer(path)
        .and_then(Value::as_bool)
        .ok_or_else(|| unknown("radio inheritance state is unknown"))
}
fn radio_path(device: bool, band: Band) -> String {
    if device {
        format!(
            "/radioManagementBands/{}/radioManagementBand",
            band.api_id()
        )
    } else {
        format!("/radios/{}", band.api_id())
    }
}
fn flag_path(band: Band) -> String {
    format!(
        "/radioManagementBands/{}/useDeviceRadioManagementConfig",
        band.api_id()
    )
}

async fn inventory<T: TokenSource>(client: &Client<T>, path: &str) -> Result<Vec<Value>, Error> {
    let payload = client.get(path).await?;
    let devices: Vec<Value> = reads::parse_elements(&payload)?;
    let count = Some(devices.len() as u64);
    if payload["kind"] != "resourceList"
        || payload["totalCount"].as_u64() != count
        || payload["matchingFilterCount"].as_u64() != count
        || !payload.get("pendingAvailability").is_some_and(|v| {
            v.is_null()
                || v == false
                || v == 0
                || v.as_array().is_some_and(Vec::is_empty)
                || v.as_object().is_some_and(serde_json::Map::is_empty)
        })
    {
        return Err(unknown(
            "AP inventory is incomplete or has pending availability",
        ));
    }
    reads::unique(
        devices.iter().map(|d| d["id"].as_str().unwrap_or_default()),
        false,
    )?;
    reads::unique(
        devices
            .iter()
            .map(|d| d["macAddress"].as_str().unwrap_or_default()),
        true,
    )?;
    if devices
        .iter()
        .any(|d| !d["macAddress"].as_str().is_some_and(reads::is_mac))
    {
        return Err(unknown("AP inventory MAC identity is unknown"));
    }
    Ok(devices)
}
fn select<'a>(devices: &'a [Value], selector: &str, by_id: bool) -> Result<&'a Value, Error> {
    let mut matches = devices.iter().filter(|d| {
        if by_id {
            d["id"].as_str() == Some(selector)
        } else if reads::is_mac(selector) {
            d["macAddress"]
                .as_str()
                .is_some_and(|mac| mac.eq_ignore_ascii_case(selector))
        } else {
            d["name"].as_str() == Some(selector)
        }
    });
    let device = matches
        .next()
        .ok_or_else(|| Error::new(ErrorKind::NotFound, "no AP matches the supplied selector"))?;
    if matches.next().is_some() {
        return Err(usage("AP name is ambiguous; select by MAC address"));
    }
    match device["deviceType"].as_str() {
        Some("accessPoint") => Ok(device),
        Some(_) => Err(usage("selected device is not an access point")),
        None => Err(unknown("selected device type is unknown")),
    }
}

struct Resource<'a, T> {
    client: &'a Client<T>,
    read_path: String,
    write_path: String,
    id: Option<String>,
    identity: Value,
}
impl<T: TokenSource> ObjectResource for Resource<'_, T> {
    async fn read_object(&self) -> Result<Value, Error> {
        let body = if let Some(id) = &self.id {
            select(&inventory(self.client, &self.read_path).await?, id, true)?.clone()
        } else {
            let body = self.client.get(&self.read_path).await?;
            site_object(&body)?;
            body
        };
        if identity(&body) != self.identity {
            return Err(unknown("radio resource identity changed"));
        }
        Ok(body)
    }
    async fn put_object(&self, body: &Value) -> Result<(), Error> {
        if identity(body) != self.identity {
            return Err(unknown("radio update changed resource identity"));
        }
        let ack = self.client.put_full(&self.write_path, body).await?;
        for key in ["id", "kind", "macAddress"] {
            if ack
                .get(key)
                .is_some_and(|value| body.get(key) != Some(value))
            {
                return Err(unknown(
                    "radio acknowledgment identified a different resource",
                ));
            }
        }
        Ok(())
    }
}
fn identity(body: &Value) -> Value {
    json!({"id":body.get("id"),"kind":body.get("kind"),"macAddress":body.get("macAddress")})
}
async fn resource<'a, T: TokenSource>(
    client: &'a Client<T>,
    site: &str,
    selector: Option<&str>,
) -> Result<(Resource<'a, T>, Value), Error> {
    let (read_path, body, id) = if let Some(selector) = selector {
        if selector.is_empty() {
            return Err(usage("AP selector must not be empty"));
        }
        let path = route(site, "inventory")?;
        let body = select(&inventory(client, &path).await?, selector, false)?.clone();
        let id = body["id"]
            .as_str()
            .ok_or_else(|| unknown("AP identity is missing"))?
            .to_owned();
        (path, body, Some(id))
    } else {
        let path = route(site, "radioManagement")?;
        let body = client.get(&path).await?;
        site_object(&body)?;
        (path, body, None)
    };
    let write_path = if let Some(id) = &id {
        let url = client.resource_url(&read_path, &[id], None)?;
        url.path()
            .strip_prefix(client.base.path().trim_end_matches('/'))
            .ok_or_else(|| usage("invalid AP radio route"))?
            .to_owned()
    } else {
        read_path.clone()
    };
    Ok((
        Resource {
            client,
            read_path,
            write_path,
            id,
            identity: identity(&body),
        },
        body,
    ))
}

fn visible(body: &Value, device: bool) -> Value {
    let mut radios = serde_json::Map::new();
    for band in BANDS {
        let path = radio_path(device, band);
        if let Some(radio) = body.pointer(&path) {
            let offered = radio["drtAvailableChannels"]
                .as_array()
                .map(|entries| entries.iter().map(visible_config).collect::<Vec<_>>());
            let mut row = json!({"configuration":visible_config(&radio["configuration"]),
                "drtAvailableChannels":offered});
            if device {
                row["useDeviceRadioManagementConfig"] =
                    json!(body.pointer(&flag_path(band)).and_then(Value::as_bool));
                row["isBroadcastingOfNetworksEnabled"] = json!(
                    body.pointer(&format!(
                        "/radioManagementBands/{}/isBroadcastingOfNetworksEnabled",
                        band.api_id()
                    ))
                    .and_then(Value::as_bool)
                );
            }
            radios.insert(band.api_id().to_owned(), row);
        }
    }
    let mut result = json!({"radioBandMapping":body["radioBandMapping"].as_str()
        .and_then(|s| s.parse::<BandMapping>().ok()).map(BandMapping::api_id),
        "radios":if radios.is_empty() { Value::Null } else { Value::Object(radios) }});
    if device {
        result["id"] = body["id"].clone();
        result["name"] = body.get("name").cloned().unwrap_or(Value::Null);
        result["useDeviceRadioBandMapping"] = json!(body["useDeviceRadioBandMapping"].as_bool());
        result["globalRadioBandMapping"] = json!(
            body["globalRadioBandMapping"]
                .as_str()
                .and_then(|s| s.parse::<BandMapping>().ok())
                .map(BandMapping::api_id)
        );
    }
    result
}
pub async fn site_plan<T: TokenSource>(client: &Client<T>, site: &str) -> Result<Value, Error> {
    let (_, body) = resource(client, site, None).await?;
    Ok(visible(&body, false))
}
pub async fn ap_override<T: TokenSource>(
    client: &Client<T>,
    site: &str,
    selector: &str,
) -> Result<Value, Error> {
    let (_, body) = resource(client, site, Some(selector)).await?;
    Ok(visible(&body, true))
}

pub async fn set_site<'a, T: TokenSource>(
    client: &'a Client<T>,
    site: &str,
    patch: Patch,
) -> Result<Prepared<impl Mutation<State = Value> + 'a>, Error> {
    prepare(client, site, None, patch).await
}
pub async fn set_ap<'a, T: TokenSource>(
    client: &'a Client<T>,
    site: &str,
    selector: &str,
    patch: Patch,
) -> Result<Prepared<impl Mutation<State = Value> + 'a>, Error> {
    prepare(client, site, Some(selector), patch).await
}

async fn prepare<'a, T: TokenSource>(
    client: &'a Client<T>,
    site: &str,
    selector: Option<&str>,
    patch: Patch,
) -> Result<Prepared<impl Mutation<State = Value> + 'a>, Error> {
    let device = selector.is_some();
    patch.validate(device)?;
    let (resource, mut body) = resource(client, site, selector).await?;
    let target = if device {
        json!({"id":body["id"],"name":body.get("name")})
    } else {
        json!({"resource":"radioManagement"})
    };
    let original = body.clone();
    let mut paths = Vec::new();
    if let Some(band) = patch.band {
        if device {
            if patch.has_configuration() {
                specific_configuration_allowed(client, site, &body, &patch, band).await?;
            }
            let bands = body["radioManagementBands"]
                .as_object()
                .ok_or_else(|| unknown("AP radio configuration is unavailable"))?;
            if !bands.contains_key(band.api_id()) {
                return Err(usage("selected radio band is unavailable"));
            }
            let flag = flag_path(band);
            let specific = boolean(&body, &flag)?;
            paths.push(flag.clone());
            *body
                .pointer_mut(&flag)
                .ok_or_else(|| unknown("radio inheritance state is missing"))? =
                json!(!patch.inherit_config);
            if !specific && patch.has_configuration() {
                paths.extend(
                    CONFIG_FIELDS
                        .iter()
                        .map(|field| format!("{}/configuration/{field}", radio_path(true, band))),
                );
            }
        }
        if patch.has_configuration() {
            let radio_path = radio_path(device, band);
            let radio = body
                .pointer(&radio_path)
                .ok_or_else(|| usage("selected radio band is unavailable"))?;
            let mut config = radio["configuration"].clone();
            patch.apply(&mut config, band)?;
            let width = current::<Width>(&config, "channelWidth")?;
            offered(radio, width, &channels(&config)?)?;
            capabilities(client, site, &body, device, band, width).await?;
            paths.extend(
                patch
                    .fields()
                    .iter()
                    .map(|field| format!("{radio_path}/configuration/{field}")),
            );
            *body
                .pointer_mut(&format!("{radio_path}/configuration"))
                .ok_or_else(|| unknown("radio configuration is missing"))? = config;
        }
    }
    if patch.mapping.is_some() || patch.inherit_mapping {
        // A mapping has its own independent inheritance flag in device config.
        if device {
            boolean(&body, "/useDeviceRadioBandMapping")?;
            paths.push("/useDeviceRadioBandMapping".to_owned());
            body["useDeviceRadioBandMapping"] = json!(!patch.inherit_mapping);
        }
        if let Some(mapping) = patch.mapping {
            let _: BandMapping = current(&body, "radioBandMapping")?;
            if device {
                require_device_capability(&body, "radioBandMapping")?;
                if !body
                    .pointer(&radio_path(true, Band::Ghz6))
                    .is_some_and(Value::is_object)
                {
                    return Err(usage("AP does not report a 6 GHz radio for band mapping"));
                }
            } else {
                if body["radioBandMappingDeviceCurrentlyPresent"].as_bool() != Some(true) {
                    return Err(usage("site does not report a band-mapping-capable AP"));
                }
                require_site_capability(client, site, "radio-band-mapping").await?;
            }
            body["radioBandMapping"] = json!(mapping.api_id());
            paths.push("/radioBandMapping".to_owned());
        }
    }
    paths.sort_unstable();
    paths.dedup();
    let expected_identity = identity(&original);
    let observe = move |body: &Value| -> Result<Value, Error> {
        if identity(body) != expected_identity {
            return Err(unknown("radio resource identity changed during readback"));
        }
        Ok(Value::Object(
            paths
                .iter()
                .map(|path| (path.clone(), owned_value(body, path)))
                .collect(),
        ))
    };
    let (backend, plan) = FullObjectPut::prepare(resource, original, observe, |value| {
        *value = body;
        Ok(())
    })?;
    Ok(Prepared {
        backend,
        plan,
        target,
    })
}

fn owned_value(body: &Value, path: &str) -> Value {
    let value = body.pointer(path).cloned().unwrap_or(Value::Null);
    match path.rsplit('/').next() {
        Some("channelWidth") => json!(
            value
                .as_str()
                .and_then(|s| s.parse::<Width>().ok())
                .map(Width::api_id)
        ),
        Some("minTxPower" | "maxTxPower") => json!(
            value
                .as_str()
                .and_then(|s| s.parse::<Power>().ok())
                .map(Power::api_id)
        ),
        Some("radioBandMapping") => json!(
            value
                .as_str()
                .and_then(|s| s.parse::<BandMapping>().ok())
                .map(BandMapping::api_id)
        ),
        Some("channels") => {
            if channels(&json!({"channels":value})).is_ok() {
                value
            } else {
                Value::Null
            }
        }
        _ => json!(value.as_bool()),
    }
}

fn offered(radio: &Value, width: Width, selected: &[u16]) -> Result<(), Error> {
    let entries = radio["drtAvailableChannels"]
        .as_array()
        .ok_or_else(|| unknown("radio offered channels are unavailable"))?;
    let mut matching = entries
        .iter()
        .filter(|e| e["channelWidth"].as_str() == Some(width.api_id()));
    let entry = matching
        .next()
        .ok_or_else(|| usage("channel width is not offered by this radio"))?;
    if matching.next().is_some() {
        return Err(unknown("radio offered channel widths are duplicated"));
    }
    let offers = channels(entry)?;
    if selected.iter().any(|channel| !offers.contains(channel)) {
        return Err(usage(
            "selected channels are not in this radio's offered list",
        ));
    }
    Ok(())
}
fn require_device_capability(body: &Value, name: &str) -> Result<(), Error> {
    if body
        .pointer(&format!("/capabilities/has/{name}"))
        .and_then(Value::as_bool)
        != Some(true)
    {
        return Err(usage(
            "AP does not advertise support for the requested radio feature",
        ));
    }
    Ok(())
}

async fn specific_configuration_allowed<T: TokenSource>(
    client: &Client<T>,
    site: &str,
    body: &Value,
    patch: &Patch,
    band: Band,
) -> Result<(), Error> {
    match body["isUnderpowered"].as_bool() {
        Some(false) => {}
        Some(true) => {
            return Err(usage(
                "AP power is insufficient for specific radio settings",
            ));
        }
        None => return Err(unknown("AP power status is unknown")),
    }
    if band != Band::Ghz24 && body.get("parentId").is_some_and(|id| !id.is_null()) {
        return Err(usage(
            "mesh uplinks cannot use specific 5 or 6 GHz radio settings",
        ));
    }
    match body
        .pointer("/capabilities/has/radioBandMapping")
        .and_then(Value::as_bool)
    {
        Some(false) => return Ok(()),
        Some(true) => {}
        None => return Err(unknown("AP band-mapping capability is unknown")),
    }
    let mapping = if let Some(mapping) = patch.mapping {
        mapping
    } else if !patch.inherit_mapping && boolean(body, "/useDeviceRadioBandMapping")? {
        current(body, "radioBandMapping")?
    } else {
        let site_plan = client.get(&route(site, "radioManagement")?).await?;
        site_object(&site_plan)?;
        current(&site_plan, "radioBandMapping")?
    };
    if !mapping.includes(band) {
        return Err(usage(
            "selected radio band is disabled by the effective band mapping",
        ));
    }
    Ok(())
}
async fn require_site_capability<T: TokenSource>(
    client: &Client<T>,
    site: &str,
    name: &str,
) -> Result<(), Error> {
    if !client.capabilities(site).await?.iter().any(|s| s == name) {
        return Err(usage(
            "site does not advertise support for the requested radio feature",
        ));
    }
    Ok(())
}
async fn capabilities<T: TokenSource>(
    client: &Client<T>,
    site: &str,
    body: &Value,
    device: bool,
    band: Band,
    width: Width,
) -> Result<(), Error> {
    if device && band == Band::Ghz6 {
        require_device_capability(body, "wifi6E")?;
    }
    let feature = match (band, width) {
        (Band::Ghz5, Width::Mhz160) => Some(("channelBandwidth160Mhz", "channel-bandwidth-160mhz")),
        (Band::Ghz6, Width::Mhz160) => Some((
            "channelBandwidth160MhzOn6Ghz",
            "channel-bandwidth-160mhz-on-6ghz",
        )),
        (Band::Ghz6, Width::Mhz320) => Some(("channelBandwidth320Mhz", "channel-bandwidth-320mhz")),
        (Band::Ghz24, Width::Mhz160 | Width::Mhz320) | (Band::Ghz5, Width::Mhz320) => {
            return Err(usage("channel width is not offered for this band"));
        }
        _ => None,
    };
    if let Some((device_flag, site_flag)) = feature {
        if device {
            require_device_capability(body, device_flag)?;
        }
        if !device || band == Band::Ghz5 {
            require_site_capability(client, site, site_flag).await?;
        }
    }
    Ok(())
}
