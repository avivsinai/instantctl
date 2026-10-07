//! Site monitoring routes observed in the Instant On portal bundle.
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;

use super::reads::{parse_elements, unique, valid_site_id};
use crate::{Client, Error, ErrorKind, TokenSource};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Event {
    pub id: String,
    pub event: Option<String>,
    pub category: Option<String>,
    pub r#type: Option<String>,
    pub state: Option<String>,
    pub account: Option<String>,
    /// Unix seconds, as supplied to the portal's epoch-time model.
    pub occurrence_time: Option<f64>,
    pub source: Option<EntityReference>,
    pub attributes: Option<Vec<Value>>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct EntityReference {
    pub id: Option<String>,
    pub name: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Alert {
    pub id: String,
    pub r#type: Option<String>,
    pub severity: Option<String>,
    pub raised_time: Option<f64>,
    pub cleared_time: Option<f64>,
    pub alert_type_properties: Option<Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Health {
    pub history_duration_seconds: Option<f64>,
    pub sample_period_seconds: Option<f64>,
    pub current_health: Option<Value>,
    pub historical_healths: Option<Vec<Value>>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Dashboard {
    pub history_duration_seconds: Option<f64>,
    pub health_overview: Option<Value>,
    pub alerts_overview: Option<Value>,
    pub devices_overview: Option<Value>,
    pub networks_overview: Option<Value>,
    pub wans_overview: Option<Value>,
    pub clients_overview: Option<Value>,
    pub policies_overview: Option<Value>,
    pub applications_overview: Option<Value>,
    pub security_threats_overview: Option<Value>,
    pub profiles_overview: Option<Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LandingPage {
    pub site_name: Option<String>,
    pub update_in_progress: Option<bool>,
    pub active_alerts_count: Option<u64>,
    pub wireless_clients_count: Option<u64>,
    pub wired_clients_count: Option<u64>,
    pub current_network_throughput_in_bits_per_second: Option<f64>,
    pub total_data_transferred_during_last24_hours_in_bytes: Option<u64>,
    pub is_application_data_available: Option<bool>,
    pub no_wireless_network: Option<bool>,
    pub should_show_wired_networks: Option<bool>,
    pub should_show_wireless_networks: Option<bool>,
    pub device_entities_count: Option<u64>,
    pub device_entities_up_count: Option<u64>,
    pub device_count: Option<u64>,
    pub device_up_count: Option<u64>,
    pub configured_wired_networks_count: Option<u64>,
    pub configured_wireless_networks_count: Option<u64>,
    pub currently_active_wired_networks_count: Option<u64>,
    pub currently_active_wireless_networks_count: Option<u64>,
    pub configured_policies_count: Option<u64>,
    pub currently_active_policies_count: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Topology {
    pub response_state: Option<String>,
    pub nodes: Option<Vec<TopologyNode>>,
    pub edges: Option<Vec<TopologyEdge>>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TopologyNode {
    pub id: String,
    pub icon_type: Option<String>,
    pub wired_client: Option<Value>,
    pub device: Option<Value>,
    pub device_stack: Option<Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TopologyEdge {
    pub source_node_id: String,
    pub target_node_id: String,
    pub loop_detected: Option<bool>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplicationConfiguration {
    pub is_application_categorization_enabled: Option<bool>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplicationUsage {
    pub application_category: Option<String>,
    pub downstream_data_transferred_during_last24_hours_in_bytes: Option<u64>,
    pub upstream_data_transferred_during_last24_hours_in_bytes: Option<u64>,
    pub wired_network_description: Option<EntityReference>,
    pub wireless_network_description: Option<EntityReference>,
    pub network_id: Option<String>,
    pub network_ssid: Option<String>,
    pub is_blocked: Option<bool>,
    pub is_blockable: Option<bool>,
    pub effective_policy: Option<Value>,
    pub application_usage: Option<Vec<Value>>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplicationUsageReport {
    pub pending_availability: Option<Value>,
    pub elements: Vec<ApplicationUsage>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientUsage {
    pub client_id: Option<String>,
    pub client_name: Option<String>,
    pub client_currently_active: Option<bool>,
    pub data_transferred_during_last24_hours_in_bytes: Option<u64>,
    pub application_category: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Threat {
    pub id: String,
    pub description: Option<String>,
    pub severity: Option<String>,
    pub classification: Option<Value>,
    pub occur_date: Option<f64>,
    pub causes: Option<Value>,
    pub exception_creation_date: Option<f64>,
    pub signature_id: Option<Value>,
    pub state: Option<String>,
    pub protocol: Option<String>,
    pub source: Option<Value>,
    pub destination: Option<Value>,
    pub malware_family: Option<String>,
    pub cve: Option<Value>,
    pub recommendations: Option<Value>,
}

impl<T: TokenSource> Client<T> {
    pub async fn events(&self, site: &str) -> Result<Vec<Event>, Error> {
        let events: Vec<Event> = parse_elements(&self.get(&route(site, "events")?).await?)?;
        unique(events.iter().map(|event| event.id.as_str()), false)?;
        Ok(events)
    }

    pub async fn alerts(&self, site: &str) -> Result<Vec<Alert>, Error> {
        let alerts: Vec<Alert> = parse_elements(&self.get(&route(site, "alerts")?).await?)?;
        unique(alerts.iter().map(|alert| alert.id.as_str()), false)?;
        Ok(alerts)
    }

    pub async fn monitoring_health(&self, site: &str) -> Result<Health, Error> {
        singleton(self.get(&route(site, "health")?).await?)
    }

    pub async fn monitoring_dashboard(&self, site: &str) -> Result<Dashboard, Error> {
        singleton(self.get(&route(site, "dashboard")?).await?)
    }

    pub async fn landing_page(&self, site: &str) -> Result<LandingPage, Error> {
        singleton(self.get(&route(site, "landingPage")?).await?)
    }

    pub async fn graph_topology(&self, site: &str) -> Result<Option<Topology>, Error> {
        let payload = self.get(&route(site, "graphTopology")?).await?;
        if payload.is_null() {
            return Ok(None);
        }
        let graph: Topology = singleton(payload)?;
        if let Some(nodes) = &graph.nodes {
            unique(nodes.iter().map(|node| node.id.as_str()), false)?;
        }
        Ok(Some(graph))
    }

    pub async fn application_configuration(
        &self,
        site: &str,
    ) -> Result<ApplicationConfiguration, Error> {
        singleton(
            self.get(&route(site, "applicationCategoryUsageConfiguration")?)
                .await?,
        )
    }

    pub async fn application_usage(&self, site: &str) -> Result<ApplicationUsageReport, Error> {
        let payload = self.get(&route(site, "applicationCategoryUsage")?).await?;
        Ok(ApplicationUsageReport {
            pending_availability: payload.get("pendingAvailability").cloned(),
            elements: parse_elements(&payload)?,
        })
    }

    pub async fn client_usage(
        &self,
        site: &str,
        network: &str,
        category: &str,
    ) -> Result<Vec<ClientUsage>, Error> {
        if category.is_empty() || category.bytes().any(|byte| byte.is_ascii_control()) {
            return Err(Error::new(ErrorKind::Usage, "invalid application category"));
        }
        let mut url =
            self.resource_url(&route(site, "stats")?, &[network, "client", "usage"], None)?;
        url.query_pairs_mut().append_pair("appCategory", category);
        parse_elements(&self.request(reqwest::Method::GET, url, None).await?)
    }

    pub async fn security_threats(&self, site: &str) -> Result<Vec<Threat>, Error> {
        let threats: Vec<Threat> =
            parse_elements(&self.get(&route(site, "securityThreatEvents")?).await?)?;
        unique(threats.iter().map(|threat| threat.id.as_str()), false)?;
        Ok(threats)
    }
}

fn route(site: &str, resource: &str) -> Result<String, Error> {
    if !valid_site_id(site) {
        return Err(Error::new(ErrorKind::Config, "--site must be a UUID"));
    }
    Ok(format!("/sites/{site}/{resource}"))
}

fn singleton<T: DeserializeOwned>(payload: Value) -> Result<T, Error> {
    if !payload.is_object() {
        return Err(Error::new(
            ErrorKind::Unverified,
            "monitoring response is not an object",
        ));
    }
    serde_json::from_value(payload).map_err(|_| {
        Error::new(
            ErrorKind::Unverified,
            "monitoring response has an invalid shape",
        )
    })
}

#[cfg(test)]
mod tests;
