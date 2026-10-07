//! Saved profile settings use the same token source as their account reads.

use std::sync::Arc;

use serde_json::Value;

use crate::{
    Client, CredentialStore, Error, ErrorKind, ProfileMetadata, RefreshingTokenSource,
    inventory::{inventory_elements, inventory_path, is_mac_address},
    ports::{parse_protected_port_entry, select_port},
    refreshing::MetadataChange,
};

pub use crate::ports::validate_protected_port_entry;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProtectedPortSummary {
    pub entry: String,
    pub switch_mac: Option<String>,
    pub switch_name: Option<String>,
    pub faceplate: u64,
}

pub fn validate_default_site(site: &str) -> Result<(), Error> {
    crate::validate_site_id(site)
        .map_err(|_| Error::new(ErrorKind::Usage, "default site must be a UUID"))
}

impl<S: CredentialStore + 'static> Client<Arc<RefreshingTokenSource<S>>> {
    pub async fn set_profile_default_site(&self, site: &str) -> Result<ProfileMetadata, Error> {
        validate_default_site(site)?;
        let site = self
            .sites()
            .await?
            .into_iter()
            .find(|candidate| candidate.id.eq_ignore_ascii_case(site))
            .ok_or_else(|| Error::new(ErrorKind::NotFound, "site is absent from this account"))?;
        self.source
            .update_profile_metadata(MetadataChange::DefaultSite(site.id.to_ascii_lowercase()))
            .await
    }

    pub async fn add_profile_protected_port(&self, entry: &str) -> Result<ProfileMetadata, Error> {
        let entry = parse_protected_port_entry(entry)?;
        let metadata = self.source.profile_metadata().await?;
        let site = profile_site(&metadata)?;
        let inventory = self.get(&inventory_path(site)?).await?;
        let device = resolve_switch(&inventory, &entry.device)?;
        select_port(device, entry.faceplate)?;
        let mac = switch_mac(device)?;
        self.source
            .update_profile_metadata(MetadataChange::AddProtectedPort {
                entry: format!("{mac}:{}", entry.faceplate),
                site: site.into(),
            })
            .await
    }

    pub async fn remove_profile_protected_port(
        &self,
        entry: &str,
    ) -> Result<ProfileMetadata, Error> {
        let entry = parse_protected_port_entry(entry)?;
        // A MAC can remove a stale policy even after the switch or port disappears.
        let (mac, site, legacy_entry) = if is_mac_address(&entry.device) {
            (entry.device, None, None)
        } else {
            let metadata = self.source.profile_metadata().await?;
            // Exact saved name entries predate the MAC-writing command. Permit
            // their removal after rename/deletion, without inventing an identity.
            let exact = format!("{}:{}", entry.device, entry.faceplate);
            if metadata.protected_ports.iter().any(|saved| {
                parse_protected_port_entry(saved).is_ok_and(|parsed| {
                    parsed.device == entry.device && parsed.faceplate == entry.faceplate
                })
            }) {
                return self
                    .source
                    .update_profile_metadata(MetadataChange::RemoveProtectedPort {
                        entry: exact,
                        site: None,
                        legacy_entry: None,
                    })
                    .await;
            }
            let site = profile_site(&metadata)?.to_owned();
            let inventory = self.get(&inventory_path(&site)?).await?;
            let device = resolve_switch(&inventory, &entry.device)?;
            (
                switch_mac(device)?,
                Some(site),
                Some(format!("{}:{}", entry.device, entry.faceplate)),
            )
        };
        self.source
            .update_profile_metadata(MetadataChange::RemoveProtectedPort {
                entry: format!("{mac}:{}", entry.faceplate),
                site,
                legacy_entry,
            })
            .await
    }

    pub async fn profile_protected_ports(&self) -> Result<Vec<ProtectedPortSummary>, Error> {
        let metadata = self.source.profile_metadata().await?;
        if metadata.protected_ports.is_empty() {
            return Ok(Vec::new());
        }
        let inventory = self.get(&inventory_path(profile_site(&metadata)?)?).await?;
        let devices = inventory_elements(&inventory)?;
        metadata
            .protected_ports
            .iter()
            .map(|entry| {
                let parsed = parse_protected_port_entry(entry)?;
                let mac = is_mac_address(&parsed.device).then(|| parsed.device.clone());
                let matches: Vec<_> = devices
                    .iter()
                    .filter(|device| {
                        device["deviceType"] == "switch" && selector_matches(device, &parsed.device)
                    })
                    .collect();
                let device = match matches.as_slice() {
                    [device] => Some(*device),
                    [] => None,
                    _ => {
                        return Err(Error::new(
                            ErrorKind::Usage,
                            "switch name is ambiguous; select by MAC",
                        ));
                    }
                };
                Ok(ProtectedPortSummary {
                    entry: entry.clone(),
                    switch_mac: mac.or_else(|| {
                        device
                            .and_then(|d| d["macAddress"].as_str())
                            .map(str::to_ascii_lowercase)
                    }),
                    switch_name: device.and_then(|d| d["name"].as_str()).map(str::to_owned),
                    faceplate: parsed.faceplate,
                })
            })
            .collect()
    }
}

fn profile_site(metadata: &ProfileMetadata) -> Result<&str, Error> {
    metadata.default_site.as_deref().ok_or_else(|| {
        Error::new(
            ErrorKind::Usage,
            "set profile default-site before managing protected ports",
        )
    })
}

fn selector_matches(device: &Value, selector: &str) -> bool {
    if is_mac_address(selector) {
        ["id", "macAddress"].iter().any(|field| {
            device[*field]
                .as_str()
                .is_some_and(|value| value.eq_ignore_ascii_case(selector))
        })
    } else {
        device["name"].as_str() == Some(selector)
    }
}

fn resolve_switch<'a>(inventory: &'a Value, selector: &str) -> Result<&'a Value, Error> {
    let matches: Vec<_> = inventory_elements(inventory)?
        .iter()
        .filter(|device| device["deviceType"] == "switch" && selector_matches(device, selector))
        .collect();
    match matches.as_slice() {
        [device] => Ok(*device),
        [] => Err(Error::new(
            ErrorKind::NotFound,
            "no switch matches the selector",
        )),
        _ => Err(Error::new(
            ErrorKind::Usage,
            "switch name is ambiguous; select by MAC",
        )),
    }
}

fn switch_mac(device: &Value) -> Result<String, Error> {
    device["macAddress"]
        .as_str()
        .filter(|mac| is_mac_address(mac))
        .map(str::to_ascii_lowercase)
        .ok_or_else(|| Error::new(ErrorKind::Unverified, "switch MAC is unknown"))
}
