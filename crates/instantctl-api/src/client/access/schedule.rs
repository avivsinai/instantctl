//! Named schedules use normal timeRangePeriod objects. WLAN schedules use the
//! separate legacy wire shape. N3/P9/R6 @4570681/@4459000/@4461401.
use super::{
    Client, Error, NamedMutation, Plan, State, TokenSource, fields, incomplete, limit, name_valid,
    parse, path, safe, select, unique_name, usage,
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashSet};
const RESOURCE: &str = "schedules";

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Day {
    Monday,
    Tuesday,
    Wednesday,
    Thursday,
    Friday,
    Saturday,
    Sunday,
}
impl Day {
    pub fn wire(self) -> &'static str {
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
#[derive(Clone, Copy, Debug)]
pub enum Mode {
    None,
    Simple,
    Week,
}
impl Mode {
    fn wire(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Simple => "simple",
            Self::Week => "week",
        }
    }
}
#[derive(Clone, Debug)]
pub enum TimeRange {
    AllDay,
    Inactive,
    Between { start: String, end: String },
}
impl TimeRange {
    pub fn validate(&self) -> Result<(), Error> {
        if let Self::Between { start, end } = self
            && (!valid_time(start)
                || !valid_time(end)
                || canonical_time(start)? == canonical_time(end)?)
        {
            return Err(usage(
                "schedule start and end must be different 24-hour HH:mm times",
            ));
        }
        Ok(())
    }
    fn apply(&self, range: &mut Value) -> Result<(), Error> {
        self.validate()?;
        if !range.is_object() {
            return Err(incomplete(
                "schedule time range configuration is unavailable",
            ));
        }
        match self {
            Self::AllDay => {
                range["timeRangePeriod"] = json!("activeAllDay");
                range.as_object_mut().unwrap().remove("startTime");
                range.as_object_mut().unwrap().remove("endTime");
            }
            Self::Inactive => {
                range["timeRangePeriod"] = json!("inactiveAllDay");
                range.as_object_mut().unwrap().remove("startTime");
                range.as_object_mut().unwrap().remove("endTime");
            }
            Self::Between { start, end } => {
                let start = canonical_time(start)?;
                let end = canonical_time(end)?;
                range["timeRangePeriod"] = json!("activeBetweenStartTimeAndEndTime");
                range["startTime"] = json!(start);
                range["endTime"] = json!(end);
            }
        }
        Ok(())
    }
}
fn valid_time(time: &str) -> bool {
    let Some((hours, minutes)) = time.split_once(':') else {
        return false;
    };
    (1..=2).contains(&hours.len())
        && minutes.len() == 2
        && hours
            .bytes()
            .chain(minutes.bytes())
            .all(|byte| byte.is_ascii_digit())
        && hours.parse::<u8>().is_ok_and(|hours| hours < 24)
        && minutes.parse::<u8>().is_ok_and(|minutes| minutes < 60)
}
fn canonical_time(time: &str) -> Result<String, Error> {
    let (hours, minutes) = time
        .split_once(':')
        .ok_or_else(|| usage("schedule time must be 24-hour HH:mm"))?;
    let hours = hours
        .parse::<u8>()
        .map_err(|_| usage("schedule time must be 24-hour HH:mm"))?;
    let minutes = minutes
        .parse::<u8>()
        .map_err(|_| usage("schedule time must be 24-hour HH:mm"))?;
    Ok(format!("{hours:02}:{minutes:02}"))
}
#[derive(Clone, Debug, Default)]
pub struct Patch {
    pub name: Option<String>,
    pub mode: Option<Mode>,
    pub days: Option<Vec<Day>>,
    pub range: Option<TimeRange>,
    pub week: BTreeMap<Day, TimeRange>,
}
impl Patch {
    pub fn is_empty(&self) -> bool {
        self.name.is_none()
            && self.mode.is_none()
            && self.days.is_none()
            && self.range.is_none()
            && self.week.is_empty()
    }
    pub fn validate(&self) -> Result<(), Error> {
        if self
            .name
            .as_deref()
            .is_some_and(|name| !name_valid(name, 32))
        {
            return Err(usage(
                "schedule name must contain 1 to 32 characters without control characters",
            ));
        }
        if self.days.as_ref().is_some_and(|days| {
            days.is_empty() || days.iter().collect::<HashSet<_>>().len() != days.len()
        }) {
            return Err(usage("simple schedule requires unique weekdays"));
        }
        if matches!(self.range, Some(TimeRange::Inactive)) {
            return Err(usage(
                "inactive-all-day is only supported for a weekly schedule day",
            ));
        }
        if let Some(range) = &self.range {
            range.validate()?;
        }
        for range in self.week.values() {
            range.validate()?;
        }
        if (self.range.is_some() || self.days.is_some())
            && (self.mode.is_some_and(|mode| !matches!(mode, Mode::Simple))
                || !self.week.is_empty())
        {
            return Err(usage(
                "simple time range and weekdays cannot be combined with another schedule mode",
            ));
        }
        if !self.week.is_empty() && self.mode.is_some_and(|mode| !matches!(mode, Mode::Week)) {
            return Err(usage("weekly day settings require week mode"));
        }
        Ok(())
    }
    fn paths(&self) -> Vec<String> {
        let mut paths = Vec::new();
        if self.name.is_some() {
            paths.push("name".to_owned());
        }
        if self.mode.is_some()
            || self.days.is_some()
            || self.range.is_some()
            || !self.week.is_empty()
        {
            paths.push("activeSchedule".to_owned());
        }
        if self.days.is_some() {
            paths.push("schedule/activeDays".to_owned());
        }
        if let Some(range) = &self.range {
            paths.extend(range_paths(
                "schedule/activeTimeRange",
                matches!(range, TimeRange::Between { .. }),
            ));
        }
        for (day, range) in &self.week {
            paths.extend(range_paths(
                &format!("weekSchedule/schedulePerWeekdayMap/{}", day.wire()),
                matches!(range, TimeRange::Between { .. }),
            ));
        }
        paths
    }
    fn apply(&self, body: &mut Value, template: Option<&Value>) -> Result<(), Error> {
        if let Some(name) = &self.name {
            body["name"] = json!(name);
        }
        if let Some(mode) = self.mode {
            body["activeSchedule"] = json!(mode.wire());
        } else if self.days.is_some() || self.range.is_some() {
            body["activeSchedule"] = json!("simple");
        } else if !self.week.is_empty() {
            body["activeSchedule"] = json!("week");
        }
        let mode = body
            .get("activeSchedule")
            .and_then(Value::as_str)
            .filter(|mode| matches!(*mode, "none" | "simple" | "week"))
            .ok_or_else(|| incomplete("schedule mode is unavailable"))?;
        let mode = mode.to_owned();
        let part = match mode.as_str() {
            "simple" => Some("schedule"),
            "week" => Some("weekSchedule"),
            _ => None,
        };
        if let Some(part) = part {
            if !body.get(part).is_some_and(Value::is_object) {
                body[part] = template
                    .and_then(|template| template.get(part))
                    .filter(|part| part.is_object())
                    .cloned()
                    .ok_or_else(|| incomplete("schedule configuration template is unavailable"))?;
            }
            if mode == "simple" {
                if let Some(days) = &self.days {
                    body[part]["activeDays"] =
                        json!(days.iter().map(|day| day.wire()).collect::<Vec<_>>());
                }
                if let Some(range) = &self.range {
                    range.apply(&mut body[part]["activeTimeRange"])?;
                }
                validate_simple(&body[part])?;
            } else {
                if !body[part]
                    .get("schedulePerWeekdayMap")
                    .is_some_and(Value::is_object)
                {
                    return Err(incomplete("weekly schedule configuration is unavailable"));
                }
                for (day, range) in &self.week {
                    range.apply(&mut body[part]["schedulePerWeekdayMap"][day.wire()])?;
                }
                validate_week(&body[part])?;
            }
        }
        Ok(())
    }
}
fn range_paths(base: &str, timed: bool) -> Vec<String> {
    let fields = if timed {
        vec!["timeRangePeriod", "startTime", "endTime"]
    } else {
        vec!["timeRangePeriod"]
    };
    fields
        .iter()
        .map(|field| format!("{base}/{field}"))
        .collect()
}
fn validate_range(range: &Value, weekly: bool) -> Result<(), Error> {
    match range.get("timeRangePeriod").and_then(Value::as_str) {
        Some("activeAllDay") => Ok(()),
        Some("inactiveAllDay") if weekly => Ok(()),
        Some("activeBetweenStartTimeAndEndTime") => {
            let start = range
                .get("startTime")
                .and_then(Value::as_str)
                .ok_or_else(|| incomplete("schedule start time is unavailable"))?;
            let end = range
                .get("endTime")
                .and_then(Value::as_str)
                .ok_or_else(|| incomplete("schedule end time is unavailable"))?;
            TimeRange::Between {
                start: start.to_owned(),
                end: end.to_owned(),
            }
            .validate()
            .map_err(|_| incomplete("schedule time range is malformed"))
        }
        _ => Err(incomplete("schedule time-range period is unavailable")),
    }
}
fn validate_simple(body: &Value) -> Result<(), Error> {
    let days = body
        .get("activeDays")
        .and_then(Value::as_array)
        .ok_or_else(|| incomplete("simple schedule weekdays are unavailable"))?;
    if days.is_empty()
        || days.iter().any(|day| {
            !matches!(
                day.as_str(),
                Some(
                    "monday"
                        | "tuesday"
                        | "wednesday"
                        | "thursday"
                        | "friday"
                        | "saturday"
                        | "sunday"
                )
            )
        })
        || days.iter().collect::<HashSet<_>>().len() != days.len()
    {
        return Err(incomplete("simple schedule weekdays are malformed"));
    }
    validate_range(&body["activeTimeRange"], false)
}
fn validate_week(body: &Value) -> Result<(), Error> {
    let days = body
        .get("schedulePerWeekdayMap")
        .and_then(Value::as_object)
        .ok_or_else(|| incomplete("weekly schedule weekdays are unavailable"))?;
    if days.len() != 7 {
        return Err(incomplete(
            "weekly schedule must contain all seven weekdays",
        ));
    }
    for day in [
        Day::Monday,
        Day::Tuesday,
        Day::Wednesday,
        Day::Thursday,
        Day::Friday,
        Day::Saturday,
        Day::Sunday,
    ] {
        validate_range(
            days.get(day.wire())
                .ok_or_else(|| incomplete("weekly schedule weekday is unavailable"))?,
            true,
        )?;
    }
    Ok(())
}
pub async fn list<T: TokenSource>(client: &Client<T>, site: &str) -> Result<Vec<Value>, Error> {
    Ok(parse(&client.get(&path(site, RESOURCE)?).await?)?
        .into_iter()
        .map(safe)
        .collect())
}
pub fn summary(row: &Value) -> Value {
    json!({"id":row.get("id").and_then(Value::as_str),"name":row.get("name").and_then(Value::as_str),"mode":row.get("activeSchedule").and_then(Value::as_str).filter(|mode|matches!(*mode,"none"|"simple"|"week")),"policy_references":row.get("referencingPolicies").and_then(Value::as_array).map(Vec::len)})
}
pub fn show(rows: &[Value], selector: &str) -> Result<Value, Error> {
    Ok(safe(select(rows, selector)?.clone()))
}
pub async fn update<'a, T: TokenSource>(
    client: &'a Client<T>,
    site: &str,
    selector: &str,
    patch: Patch,
) -> Result<(NamedMutation<'a, T>, Plan<State>), Error> {
    patch.validate()?;
    if patch.is_empty() {
        return Err(usage("specify at least one schedule change"));
    }
    let collection = path(site, RESOURCE)?;
    let payload = client.get(&collection).await?;
    let rows = parse(&payload)?;
    let row = select(&rows, selector)?;
    validate_name_limit(&payload, patch.name.as_deref())?;
    unique_name(&rows, row["id"].as_str(), patch.name.as_deref())?;
    let template = payload.pointer("/metaData/defaultSchedule");
    let mut desired = row.clone();
    patch.apply(&mut desired, template)?;
    let mut paths = patch.paths();
    if patch.mode.is_some() || desired["activeSchedule"] != row["activeSchedule"] {
        paths.extend(configuration_paths(&desired)?);
    }
    paths.sort_unstable();
    paths.dedup();
    NamedMutation::update(client, collection, row.clone(), paths, |body| {
        patch.apply(body, template)
    })
}
pub async fn create<'a, T: TokenSource>(
    client: &'a Client<T>,
    site: &str,
    patch: Patch,
) -> Result<(NamedMutation<'a, T>, Plan<State>), Error> {
    patch.validate()?;
    if patch.name.is_none() {
        return Err(usage("schedule creation requires a name"));
    }
    let collection = path(site, RESOURCE)?;
    let payload = client.get(&collection).await?;
    let rows = parse(&payload)?;
    validate_name_limit(&payload, patch.name.as_deref())?;
    unique_name(&rows, None, patch.name.as_deref())?;
    limit(&payload, "/maxElements", rows.len())?;
    let template = payload
        .pointer("/metaData/defaultSchedule")
        .filter(|value| value.is_object())
        .ok_or_else(|| incomplete("schedule creation template is unavailable"))?;
    let mut body = Value::Object(
        ["name", "schedule", "weekSchedule", "activeSchedule"]
            .iter()
            .filter_map(|field| {
                template
                    .get(*field)
                    .map(|value| ((*field).to_owned(), value.clone()))
            })
            .collect(),
    );
    patch.apply(&mut body, Some(template))?;
    let mut paths = fields(&["name", "activeSchedule"]);
    paths.extend(configuration_paths(&body)?);
    paths.sort_unstable();
    paths.dedup();
    NamedMutation::create(client, collection, &rows, body, paths)
}
fn validate_name_limit(payload: &Value, name: Option<&str>) -> Result<(), Error> {
    if let (Some(max), Some(name)) = (payload.pointer("/metaData/maxLengthOfScheduleNames"), name) {
        let max = max
            .as_u64()
            .filter(|max| *max > 0)
            .ok_or_else(|| incomplete("schedule name limit is malformed"))?;
        if name.encode_utf16().count() as u64 > max {
            return Err(usage("schedule name exceeds the site name limit"));
        }
    }
    Ok(())
}
fn configuration_paths(body: &Value) -> Result<Vec<String>, Error> {
    match body["activeSchedule"].as_str() {
        Some("none") => Ok(vec![]),
        Some("simple") => {
            let mut paths = fields(&["schedule/activeDays"]);
            paths.extend(range_paths(
                "schedule/activeTimeRange",
                body.pointer("/schedule/activeTimeRange/timeRangePeriod")
                    .is_some_and(|period| period == "activeBetweenStartTimeAndEndTime"),
            ));
            Ok(paths)
        }
        Some("week") => Ok([
            Day::Monday,
            Day::Tuesday,
            Day::Wednesday,
            Day::Thursday,
            Day::Friday,
            Day::Saturday,
            Day::Sunday,
        ]
        .iter()
        .flat_map(|day| {
            range_paths(
                &format!("weekSchedule/schedulePerWeekdayMap/{}", day.wire()),
                body["weekSchedule"]["schedulePerWeekdayMap"][day.wire()]["timeRangePeriod"]
                    == "activeBetweenStartTimeAndEndTime",
            )
        })
        .collect()),
        _ => Err(incomplete("schedule mode is unavailable")),
    }
}
pub async fn delete<'a, T: TokenSource>(
    client: &'a Client<T>,
    site: &str,
    selector: &str,
    yes: bool,
) -> Result<(NamedMutation<'a, T>, Plan<State>), Error> {
    let collection = path(site, RESOURCE)?;
    let rows = parse(&client.get(&collection).await?)?;
    let row = select(&rows, selector)?;
    let policies = row
        .get("referencingPolicies")
        .and_then(Value::as_array)
        .ok_or_else(|| incomplete("schedule policy references are unavailable"))?;
    let names = policies
        .iter()
        .map(|policy| {
            policy
                .get("name")
                .and_then(Value::as_str)
                .filter(|name| !name.is_empty() && !name.chars().any(char::is_control))
                .map(str::to_owned)
        })
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| incomplete("schedule policy reference names are unavailable"))?;
    if !names.is_empty() && !yes {
        return Err(usage(&format!(
            "deleting a schedule referenced by policies requires --yes; referencing policies: {}",
            names.join(", ")
        )));
    }
    NamedMutation::delete(client, collection, row, json!(names))
}
