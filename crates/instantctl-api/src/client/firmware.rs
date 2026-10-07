//! Site maintenance settings and firmware actions observed in the portal bundle.
//! The local-date schedule endpoint carries no timezone offset. Readback must
//! match the requested local timestamp exactly; no offset conversion is safe.

use serde_json::{Value, json};

use super::{Client, reads};
use crate::{
    Error, ErrorKind, TokenSource,
    mutation::{FullObjectPut, Mutation, ObjectResource, Plan},
};

const RESOURCE: &str = "maintenance";
const STATES: [&str; 5] = [
    "started-maintenance",
    "downloading-installing",
    "rebooting",
    "completing",
    "not-in-maintenance",
];
const DELAYS: [u8; 5] = [0, 7, 14, 21, 28];
const DAYS: [&str; 7] = [
    "monday",
    "tuesday",
    "wednesday",
    "thursday",
    "friday",
    "saturday",
    "sunday",
];

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WindowPatch {
    pub day: Option<String>,
    pub start_time: Option<String>,
    pub update_delay_in_days: Option<u8>,
}

impl WindowPatch {
    pub fn validate(&self) -> Result<(), Error> {
        if self.day.is_none() && self.start_time.is_none() && self.update_delay_in_days.is_none() {
            return Err(usage("specify at least one maintenance window field"));
        }
        if let Some(day) = &self.day
            && !DAYS.contains(&day.as_str())
        {
            return Err(usage("maintenance day must be a lowercase weekday"));
        }
        if let Some(time) = &self.start_time {
            validate_time(time)?;
        }
        if let Some(delay) = self.update_delay_in_days
            && !DELAYS.contains(&delay)
        {
            return Err(usage("maintenance delay must be 0, 7, 14, 21, or 28 days"));
        }
        Ok(())
    }
}

fn site_path(site: &str) -> Result<String, Error> {
    if !reads::valid_site_id(site) {
        return Err(config("--site must be a UUID"));
    }
    Ok(format!("/sites/{site}/{RESOURCE}"))
}

/// Read the complete maintenance object and retain fields this client does not own.
pub async fn get<T: TokenSource>(client: &Client<T>, site: &str) -> Result<Value, Error> {
    let body = client.get(&site_path(site)?).await?;
    validate_maintenance(&body)?;
    Ok(body)
}

fn validate_maintenance(body: &Value) -> Result<(), Error> {
    if !body.is_object() || body.get("kind").and_then(Value::as_str) != Some(RESOURCE) {
        return Err(incomplete("maintenance response is missing its kind"));
    }
    Ok(())
}

fn validate_day(value: &str) -> Result<(), Error> {
    if DAYS.contains(&value) {
        Ok(())
    } else {
        Err(incomplete("maintenance day is missing or unknown"))
    }
}

fn validate_time(value: &str) -> Result<(), Error> {
    let bytes = value.as_bytes();
    if bytes.len() != 5
        || bytes[2] != b':'
        || !bytes[..2]
            .iter()
            .chain(bytes[3..].iter())
            .all(u8::is_ascii_digit)
    {
        return Err(usage("time must use HH:mm format"));
    }
    let hour = value[..2]
        .parse::<u8>()
        .map_err(|_| usage("time is invalid"))?;
    let minute = value[3..]
        .parse::<u8>()
        .map_err(|_| usage("time is invalid"))?;
    if hour > 23 || minute > 59 {
        return Err(usage("time is outside the 24-hour day"));
    }
    Ok(())
}

/// Accept only the portal's offset-free local schedule form.
pub fn validate_schedule(value: &str) -> Result<String, Error> {
    let bytes = value.as_bytes();
    if bytes.len() != 19
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes[10] != b'T'
        || bytes[13] != b':'
        || bytes[16] != b':'
        || !bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| [4, 7, 10, 13, 16].contains(&index) || byte.is_ascii_digit())
    {
        return Err(usage(
            "schedule must use local YYYY-MM-DDTHH:mm:ss without an offset",
        ));
    }
    value
        .parse::<jiff::civil::DateTime>()
        .map_err(|_| usage("schedule date or time is outside its valid range"))?;
    // Jiff accepts leap seconds, but the portal's local Date formatter emits 00–59.
    if bytes[17] > b'5' {
        return Err(usage("schedule seconds must be between 00 and 59"));
    }
    Ok(value.to_owned())
}

type Observe = Box<dyn Fn(&Value) -> Result<Value, Error> + Send + Sync>;

struct MaintenanceResource<'a, T> {
    client: &'a Client<T>,
    path: String,
}

impl<T: TokenSource> ObjectResource for MaintenanceResource<'_, T> {
    async fn read_object(&self) -> Result<Value, Error> {
        let body = self.client.get(&self.path).await?;
        validate_maintenance(&body)?;
        Ok(body)
    }

    async fn put_object(&self, body: &Value) -> Result<(), Error> {
        validate_maintenance(body)?;
        let reply = self.client.put_full(&self.path, body).await?;
        if reply
            .get("kind")
            .and_then(Value::as_str)
            .is_some_and(|kind| kind != RESOURCE)
        {
            return Err(incomplete(
                "maintenance acknowledgment has an unexpected kind",
            ));
        }
        Ok(())
    }
}

fn window_state(body: &Value, fields: &[&'static str]) -> Result<Value, Error> {
    validate_maintenance(body)?;
    let mut state = serde_json::Map::new();
    for field in fields {
        let value = body
            .get(*field)
            .ok_or_else(|| incomplete("maintenance window field is missing"))?;
        match *field {
            "day" => validate_day(
                value
                    .as_str()
                    .ok_or_else(|| incomplete("maintenance day is missing or unknown"))?,
            )?,
            "startTime" => validate_time(
                value
                    .as_str()
                    .ok_or_else(|| incomplete("maintenance start time is missing or malformed"))?,
            )
            .map_err(|_| incomplete("maintenance start time is missing or malformed"))?,
            "updateDelayInDays"
                if value.as_u64().is_none_or(|delay| {
                    delay > u64::from(u8::MAX) || !DELAYS.contains(&(delay as u8))
                }) =>
            {
                return Err(incomplete("maintenance delay is missing or unknown"));
            }
            _ => {}
        }
        state.insert((*field).to_owned(), value.clone());
    }
    Ok(Value::Object(state))
}

/// Prepare a full-object update. Unknown server fields stay in the PUT body.
pub async fn set_window<'a, T: TokenSource>(
    client: &'a Client<T>,
    site: &'a str,
    patch: WindowPatch,
) -> Result<(impl Mutation<State = Value> + 'a, Plan<Value>), Error> {
    patch.validate()?;
    let path = site_path(site)?;
    let current = get(client, site).await?;
    let mut fields = Vec::new();
    if patch.day.is_some() {
        fields.push("day");
    }
    if patch.start_time.is_some() {
        fields.push("startTime");
    }
    if patch.update_delay_in_days.is_some() {
        fields.push("updateDelayInDays");
    }
    let observed_fields = fields.clone();
    let observe: Observe = Box::new(move |body| window_state(body, &observed_fields));
    FullObjectPut::prepare(
        MaintenanceResource { client, path },
        current,
        observe,
        move |body| {
            if let Some(day) = patch.day {
                body["day"] = Value::String(day);
            }
            if let Some(time) = patch.start_time {
                body["startTime"] = Value::String(time);
            }
            if let Some(delay) = patch.update_delay_in_days {
                body["updateDelayInDays"] = json!(delay);
            }
            Ok(())
        },
    )
}

#[derive(Clone)]
enum ActionKind {
    UpdateNow,
    Schedule { local_date_time: String },
}

struct FirmwareAction<'a, T> {
    client: &'a Client<T>,
    site: &'a str,
    path: String,
    kind: ActionKind,
}

impl<T: TokenSource> FirmwareAction<'_, T> {
    async fn current(&self) -> Result<Value, Error> {
        get(self.client, self.site).await
    }

    async fn verify_eligibility(&self) -> Result<(), Error> {
        let current = self.current().await?;
        ensure_update_eligible(&current)
    }
}

impl<T: TokenSource> Mutation for FirmwareAction<'_, T> {
    type State = Value;

    async fn read(&self) -> Result<Self::State, Error> {
        let current = self.current().await?;
        match &self.kind {
            ActionKind::UpdateNow => {
                let active = maintenance_active(&current)?;
                Ok(json!({"update_started":active}))
            }
            ActionKind::Schedule { .. } => Ok(json!({
                "update_schedule_date_time":current.get("updateScheduleDateTime").cloned().unwrap_or(Value::Null)
            })),
        }
    }

    async fn write(&self, desired: &Self::State) -> Result<(), Error> {
        self.verify_eligibility().await?;
        match &self.kind {
            ActionKind::UpdateNow if desired == &json!({"update_started":true}) => {
                self.client
                    .create(
                        &format!("{}?action=updateSoftwareNow", self.path),
                        &json!({}),
                    )
                    .await?;
            }
            ActionKind::Schedule { local_date_time }
                if desired == &json!({"update_schedule_date_time":local_date_time}) =>
            {
                self.client
                    .create(
                        &format!("{}/schedule", self.path),
                        &json!({"localDateTime":local_date_time}),
                    )
                    .await?;
            }
            _ => {
                return Err(config(
                    "firmware action plan does not match its prepared target",
                ));
            }
        }
        Ok(())
    }
}

fn maintenance_active(body: &Value) -> Result<bool, Error> {
    validate_maintenance(body)?;
    match body.get("state").and_then(Value::as_str) {
        Some("not-in-maintenance") => Ok(false),
        Some(state) if STATES.contains(&state) => Ok(true),
        _ => Err(incomplete("maintenance state is missing or unknown")),
    }
}

fn ensure_update_eligible(body: &Value) -> Result<(), Error> {
    if maintenance_active(body)? {
        return Err(Error::new(
            ErrorKind::Unsupported,
            "firmware update is already in progress",
        ));
    }
    match body.get("newUpdateVersion") {
        Some(Value::String(version)) if !version.trim().is_empty() => {}
        Some(Value::String(_)) => {
            return Err(Error::new(
                ErrorKind::Unsupported,
                "no firmware update is available",
            ));
        }
        _ => return Err(incomplete("available firmware version is unknown")),
    }
    Ok(())
}

pub async fn update_now<'a, T: TokenSource>(
    client: &'a Client<T>,
    site: &'a str,
) -> Result<(impl Mutation<State = Value> + 'a, Plan<Value>), Error> {
    let path = site_path(site)?;
    let current_body = get(client, site).await?;
    ensure_update_eligible(&current_body)?;
    let backend = FirmwareAction {
        client,
        site,
        path,
        kind: ActionKind::UpdateNow,
    };
    Ok((
        backend,
        Plan {
            current: json!({"update_started":false}),
            desired: json!({"update_started":true}),
        },
    ))
}

pub async fn schedule<'a, T: TokenSource>(
    client: &'a Client<T>,
    site: &'a str,
    local_date_time: String,
) -> Result<(impl Mutation<State = Value> + 'a, Plan<Value>), Error> {
    let local_date_time = validate_schedule(&local_date_time)?;
    let path = site_path(site)?;
    let current_body = get(client, site).await?;
    ensure_update_eligible(&current_body)?;
    let current = json!({
        "update_schedule_date_time":current_body.get("updateScheduleDateTime").cloned().unwrap_or(Value::Null)
    });
    let desired = json!({"update_schedule_date_time":local_date_time});
    let backend = FirmwareAction {
        client,
        site,
        path,
        kind: ActionKind::Schedule { local_date_time },
    };
    Ok((backend, Plan { current, desired }))
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
