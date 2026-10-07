use std::{fmt, str::FromStr};

use serde_json::{Value, json};

use crate::{Error, ErrorKind};

// API values from the portal's E0, Wg, Bg and B6 enums. Keep CLI parsing and
// wire serialization on the same definitions.
macro_rules! api_enum {
    ($name:ident { $($variant:ident => $wire:literal),+ $(,)? }) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum $name { $($variant),+ }
        impl $name {
            pub fn api_id(self) -> &'static str {
                match self { $(Self::$variant => $wire),+ }
            }
        }
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.api_id())
            }
        }
        impl FromStr for $name {
            type Err = Error;
            fn from_str(value: &str) -> Result<Self, Error> {
                match value {
                    $($wire => Ok(Self::$variant),)+
                    _ => Err(usage(concat!("unknown ", stringify!($name), " value"))),
                }
            }
        }
    };
}

api_enum!(Band { Ghz24 => "2.4ghz", Ghz5 => "5ghz", Ghz6 => "6ghz" });
api_enum!(Width {
    Mhz20 => "20mhz", Mhz40 => "40mhz", Mhz80 => "80mhz",
    Mhz160 => "160mhz", Mhz320 => "320mhz"
});
api_enum!(Power {
    Dbm6 => "6dbm", Dbm9 => "9dbm", Dbm12 => "12dbm", Dbm15 => "15dbm",
    Dbm18 => "18dbm", Dbm21 => "21dbm", Dbm24 => "24dbm", Dbm27 => "27dbm",
    Dbm30 => "30dbm", Dbm33 => "33dbm", RegulatoryMax => "regulatoryMax"
});
api_enum!(BandMapping {
    Ghz24And5 => "2.4ghz_and_5ghz", Ghz24And6 => "2.4ghz_and_6ghz",
    Ghz5And6 => "5ghz_and_6ghz"
});

impl BandMapping {
    pub(super) fn includes(self, band: Band) -> bool {
        !matches!(
            (self, band),
            (Self::Ghz24And5, Band::Ghz6)
                | (Self::Ghz24And6, Band::Ghz5)
                | (Self::Ghz5And6, Band::Ghz24)
        )
    }
}

pub(super) const BANDS: [Band; 3] = [Band::Ghz24, Band::Ghz5, Band::Ghz6];
pub(super) const CONFIG_FIELDS: [&str; 4] =
    ["channelWidth", "channels", "minTxPower", "maxTxPower"];

impl Power {
    fn rank(self) -> u8 {
        match self {
            Self::Dbm6 => 6,
            Self::Dbm9 => 9,
            Self::Dbm12 => 12,
            Self::Dbm15 => 15,
            Self::Dbm18 => 18,
            Self::Dbm21 => 21,
            Self::Dbm24 => 24,
            Self::Dbm27 => 27,
            Self::Dbm30 => 30,
            Self::Dbm33 => 33,
            Self::RegulatoryMax => u8::MAX,
        }
    }
    fn allowed(self, band: Band) -> bool {
        match band {
            Band::Ghz24 => self != Self::Dbm33,
            Band::Ghz5 => self.rank() >= 15 && self != Self::Dbm33,
            Band::Ghz6 => self.rank() >= 15,
        }
    }
}

#[derive(Debug, Default)]
pub struct Patch {
    pub band: Option<Band>,
    pub width: Option<Width>,
    pub channels: Option<Vec<u16>>,
    pub min_power: Option<Power>,
    pub max_power: Option<Power>,
    pub mapping: Option<BandMapping>,
    pub inherit_config: bool,
    pub inherit_mapping: bool,
}

impl Patch {
    pub fn has_configuration(&self) -> bool {
        self.width.is_some()
            || self.channels.is_some()
            || self.min_power.is_some()
            || self.max_power.is_some()
    }
    pub fn validate(&self, device: bool) -> Result<(), Error> {
        let config = self.has_configuration();
        if !(config || self.mapping.is_some() || self.inherit_config || self.inherit_mapping) {
            return Err(usage("specify at least one radio change"));
        }
        if (config || self.inherit_config) != self.band.is_some() {
            return Err(usage("radio configuration changes require --band"));
        }
        if self.inherit_config && config {
            return Err(usage(
                "--inherit conflicts with specific radio configuration",
            ));
        }
        if self.inherit_mapping && self.mapping.is_some() {
            return Err(usage("choose a specific band mapping or inherit it"));
        }
        if !device && (self.inherit_config || self.inherit_mapping) {
            return Err(usage("only AP overrides can inherit site settings"));
        }
        if let Some(channels) = &self.channels
            && (channels.is_empty()
                || channels.contains(&0)
                || channels
                    .iter()
                    .collect::<std::collections::HashSet<_>>()
                    .len()
                    != channels.len())
        {
            return Err(usage("select at least one unique positive channel"));
        }
        if let Some(band) = self.band {
            if matches!(
                (band, self.width),
                (Band::Ghz24, Some(Width::Mhz160 | Width::Mhz320))
                    | (Band::Ghz5, Some(Width::Mhz320))
            ) {
                return Err(usage("channel width is not offered for this band"));
            }
            for power in [self.min_power, self.max_power].into_iter().flatten() {
                if !power.allowed(band) {
                    return Err(usage("transmit power is not offered for the selected band"));
                }
            }
        }
        if let (Some(min), Some(max)) = (self.min_power, self.max_power)
            && min.rank() > max.rank()
        {
            return Err(usage("minimum transmit power exceeds maximum"));
        }
        Ok(())
    }
    pub(super) fn fields(&self) -> Vec<&'static str> {
        [
            (self.width.is_some(), "channelWidth"),
            (self.channels.is_some(), "channels"),
            (self.min_power.is_some(), "minTxPower"),
            (self.max_power.is_some(), "maxTxPower"),
        ]
        .into_iter()
        .filter_map(|(set, field)| set.then_some(field))
        .collect()
    }
    pub(super) fn apply(&self, config: &mut Value, band: Band) -> Result<(), Error> {
        validate_config(config, band)?;
        if let Some(width) = self.width {
            config["channelWidth"] = json!(width.api_id());
        }
        if let Some(channels) = &self.channels {
            // The portal editor splits checkbox strings and sends string channels.
            config["channels"] = json!(channels.iter().map(u16::to_string).collect::<Vec<_>>());
        }
        if let Some(power) = self.min_power {
            config["minTxPower"] = json!(power.api_id());
        }
        if let Some(power) = self.max_power {
            config["maxTxPower"] = json!(power.api_id());
        }
        validate_config(config, band)
    }
}

pub(super) fn usage(message: &str) -> Error {
    Error::new(ErrorKind::Usage, message)
}
pub(super) fn unknown(message: &str) -> Error {
    Error::new(ErrorKind::Unverified, message)
}

pub(super) fn current<T: FromStr>(config: &Value, field: &str) -> Result<T, Error> {
    config
        .get(field)
        .and_then(Value::as_str)
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| unknown("radio configuration is missing or has an unknown enum"))
}
pub(super) fn channel(value: &Value) -> Option<u16> {
    let number = value
        .as_u64()
        .and_then(|n| u16::try_from(n).ok())
        .or_else(|| value.as_str()?.parse::<u16>().ok())?;
    (number != 0).then_some(number)
}
pub(super) fn channels(config: &Value) -> Result<Vec<u16>, Error> {
    let values = config["channels"]
        .as_array()
        .filter(|a| !a.is_empty())
        .ok_or_else(|| unknown("radio channel selection is missing"))?;
    let channels: Vec<_> = values
        .iter()
        .map(channel)
        .collect::<Option<_>>()
        .ok_or_else(|| unknown("radio channel selection is malformed"))?;
    if channels
        .iter()
        .collect::<std::collections::HashSet<_>>()
        .len()
        != channels.len()
    {
        return Err(unknown("radio channel selection is duplicated"));
    }
    Ok(channels)
}
pub(super) fn validate_config(config: &Value, band: Band) -> Result<(), Error> {
    let _: Width = current(config, "channelWidth")?;
    channels(config)?;
    let min: Power = current(config, "minTxPower")?;
    let max: Power = current(config, "maxTxPower")?;
    if !min.allowed(band) || !max.allowed(band) {
        return Err(unknown(
            "stored transmit power is not recognized for this band",
        ));
    }
    if min.rank() > max.rank() {
        return Err(usage("minimum transmit power exceeds maximum"));
    }
    Ok(())
}

pub(super) fn visible_config(config: &Value) -> Value {
    if !config.is_object() {
        return Value::Null;
    }
    json!({
        "channelWidth": current::<Width>(config, "channelWidth").ok().map(Width::api_id),
        "channels": channels(config).ok().map(|_| config["channels"].clone()),
        "minTxPower": current::<Power>(config, "minTxPower").ok().map(Power::api_id),
        "maxTxPower": current::<Power>(config, "maxTxPower").ok().map(Power::api_id)
    })
}
