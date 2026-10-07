use std::{collections::HashMap, net::IpAddr, time::Duration};

use crate::{Error, ErrorKind};
use url::Url;

use super::incomplete;
use serde::Serialize;
fn config(message: &str) -> Error {
    Error::new(ErrorKind::Config, message)
}

const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
#[derive(Clone, Debug, Serialize)]
pub struct LocalHealth {
    pub host: String,
    pub model: Option<String>,
    pub serial: Option<String>,
    pub cloud_status: Option<String>,
    pub ntp_status: Option<String>,
    pub swarm_state: Option<String>,
    pub cpu_usage_percent: Option<u64>,
    pub uplink_flap_count: Option<u64>,
}
impl LocalHealth {
    pub fn is_complete(&self) -> bool {
        self.cloud_status.is_some() && self.ntp_status.is_some() && self.swarm_state.is_some()
    }
}

pub fn validate_host(host: &str) -> Result<String, Error> {
    if let Ok(address) = host.parse::<IpAddr>() {
        return Ok(address.to_string());
    }
    let host = host.strip_suffix('.').unwrap_or(host);
    if host.is_empty()
        || host.len() > 253
        || !host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && label.as_bytes()[0].is_ascii_alphanumeric()
                && label.as_bytes()[label.len() - 1].is_ascii_alphanumeric()
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
    {
        return Err(config(
            "AP host must be an IP address or DNS hostname without a port or URL",
        ));
    }
    Ok(host.to_owned())
}

fn endpoint(host: &str) -> Result<Url, Error> {
    let host = validate_host(host)?;
    let authority = if host.parse::<std::net::Ipv6Addr>().is_ok() {
        format!("[{host}]")
    } else {
        host
    };
    Url::parse(&format!(
        "http://{authority}:8080/swarm.cgi?opcode=smb_debug_info"
    ))
    .map_err(|_| config("invalid AP host"))
}

pub async fn get(host: &str, timeout: Duration) -> Result<LocalHealth, Error> {
    let url = endpoint(host)?;
    request(host, url, timeout).await
}

async fn request(host: &str, url: Url, timeout: Duration) -> Result<LocalHealth, Error> {
    if timeout.is_zero() {
        return Err(config("request timeout must be greater than zero"));
    }
    let http = reqwest::Client::builder()
        .timeout(timeout)
        .connect_timeout(timeout)
        .read_timeout(timeout)
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .build()
        .map_err(|_| {
            Error::new(
                ErrorKind::General,
                "could not initialize local AP transport",
            )
        })?;
    // The portal bearer token is never sent to the local AP endpoint.
    let mut response = http.get(url.clone()).send().await.map_err(|_| {
        Error::new(
            ErrorKind::General,
            "local AP request failed during transport",
        )
    })?;
    if response.url().origin() != url.origin() || response.url().path() != url.path() {
        return Err(incomplete(
            "local AP response escaped the expected endpoint",
        ));
    }
    if response.status().as_u16() != 200 {
        return Err(Error::new(
            ErrorKind::General,
            format!(
                "local AP request failed with HTTP {}",
                response.status().as_u16()
            ),
        ));
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
    {
        return Err(incomplete("local AP response exceeded the size limit"));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| {
        Error::new(
            ErrorKind::General,
            "local AP response failed during transport",
        )
    })? {
        if chunk.len() > MAX_RESPONSE_BYTES - bytes.len() {
            return Err(incomplete("local AP response exceeded the size limit"));
        }
        bytes.extend_from_slice(&chunk);
    }
    let xml = std::str::from_utf8(&bytes)
        .map_err(|_| incomplete("local AP response is not valid XML"))?;
    parse(host, xml)
}

fn parse(host: &str, xml: &str) -> Result<LocalHealth, Error> {
    if xml.len() > MAX_RESPONSE_BYTES {
        return Err(incomplete("local AP response exceeded the size limit"));
    }
    let document = roxmltree::Document::parse_with_options(
        xml,
        roxmltree::ParsingOptions {
            allow_dtd: false,
            nodes_limit: 100_000,
            ..Default::default()
        },
    )
    .map_err(|_| incomplete("local AP response is not valid XML"))?;
    let mut fields = HashMap::new();
    for node in document
        .root_element()
        .children()
        .filter(|node| node.has_tag_name("data"))
    {
        let name = node.attribute("name").unwrap_or_default().trim();
        if name.is_empty() {
            continue;
        }
        if node.children().any(|child| child.is_element()) {
            return Err(incomplete("local AP XML field has an invalid shape"));
        }
        let text: String = node.children().filter_map(|child| child.text()).collect();
        if fields
            .insert(name.to_owned(), text.trim().to_owned())
            .is_some()
        {
            return Err(incomplete("local AP XML contains duplicate fields"));
        }
    }
    let field = |name| fields.get(name).filter(|text| !text.is_empty()).cloned();
    let stats = field("Election Stats").unwrap_or_default();
    Ok(LocalHealth {
        host: host.to_owned(),
        model: field("AP Model"),
        serial: field("Serial Number"),
        cloud_status: field("Cloudconnect Status"),
        ntp_status: field("NTP Status"),
        swarm_state: field("CLI Swarm State"),
        cpu_usage_percent: counter(&stats, "ap cpu usage"),
        uplink_flap_count: counter(&stats, "uplink flap count"),
    })
}

fn counter(text: &str, label: &str) -> Option<u64> {
    let text = text.to_ascii_lowercase();
    let rest = text
        .get(text.find(label)? + label.len()..)?
        .trim_start()
        .strip_prefix(':')?
        .trim_start();
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::super::tests::{Mock, Reply};
    use super::*;
    const PATH: &str = "/swarm.cgi?opcode=smb_debug_info";

    fn url(server: &Mock) -> Url {
        Url::parse(&format!("http://{}{PATH}", server.address)).unwrap()
    }
    fn complete() -> &'static str {
        "<response><data name='Cloudconnect Status'>Connected</data><data name='NTP Status'>Synchronized</data><data name='CLI Swarm State'>master</data><data name='AP Model'>AP22</data><data name='Serial Number'>SN1</data><data name='Election Stats'>AP CPU usage : 17\nuplink flap count : 3</data></response>"
    }

    #[tokio::test]
    async fn local_health_reads_xml_and_preserves_reported_values() {
        let server = Mock::new([(PATH, Reply::xml(complete()))]);
        let result = request("ap.example", url(&server), Duration::from_secs(2))
            .await
            .unwrap();
        assert!(result.is_complete());
        let data = serde_json::to_value(result).unwrap();
        assert_eq!(data["model"], "AP22");
        assert_eq!(data["cloud_status"], "Connected");
        assert_eq!(data["cpu_usage_percent"], 17);
        assert_eq!(data["uplink_flap_count"], 3);
        assert_eq!(server.paths(), [PATH]);
    }

    #[tokio::test]
    async fn missing_health_and_counters_stay_null_and_exit_unverified() {
        let server = Mock::new([(
            PATH,
            Reply::xml("<response><data name='NTP Status'>  </data></response>"),
        )]);
        let result = request("ap", url(&server), Duration::from_secs(2))
            .await
            .unwrap();
        assert!(!result.is_complete());
        let data = serde_json::to_value(result).unwrap();
        for key in [
            "model",
            "serial",
            "cloud_status",
            "ntp_status",
            "swarm_state",
            "cpu_usage_percent",
            "uplink_flap_count",
        ] {
            assert!(data[key].is_null(), "{key}");
        }
    }

    #[tokio::test]
    async fn redirects_are_not_followed_even_to_a_second_host() {
        let foreign = Mock::new([(PATH, Reply::xml(complete()))]);
        let server = Mock::new([(
            PATH,
            Reply {
                status: 302,
                headers: vec![("Location".into(), url(&foreign).to_string())],
                ..Reply::xml(complete())
            },
        )]);
        assert!(
            request("ap", url(&server), Duration::from_secs(2))
                .await
                .is_err()
        );
        assert_eq!(server.paths(), [PATH]);
        assert!(foreign.paths().is_empty());
    }

    #[tokio::test]
    async fn malformed_xml_dtd_entities_and_oversized_responses_are_refused() {
        for xml in [
            "<response>",
            "<!DOCTYPE response [<!ENTITY x 'healthy'>]><response><data name='NTP Status'>&x;</data></response>",
            "<!DOCTYPE response><response/>",
            "<response><data>&unknown;</data></response>",
            "<response><data name='NTP Status'>yes</data><data name='NTP Status'>no</data></response>",
        ] {
            let server = Mock::new([(PATH, Reply::xml(xml))]);
            assert_eq!(
                request("ap", url(&server), Duration::from_secs(2))
                    .await
                    .err()
                    .unwrap()
                    .kind,
                ErrorKind::Unverified
            );
        }
        let server = Mock::new([(PATH, Reply::xml(&"x".repeat(MAX_RESPONSE_BYTES + 1)))]);
        assert_eq!(
            request("ap", url(&server), Duration::from_secs(2))
                .await
                .err()
                .unwrap()
                .kind,
            ErrorKind::Unverified
        );
    }

    #[test]
    fn hosts_cannot_inject_urls_ports_paths_or_credentials() {
        for host in [
            "http://ap",
            "ap:8080",
            "user:secret@ap",
            "ap/path",
            "ap?query",
            "-ap",
            "ap..local",
            "ap\n",
            "",
        ] {
            let error = endpoint(host).unwrap_err();
            assert_eq!(error.kind, ErrorKind::Config);
            assert!(!format!("{error:?} {error}").contains("secret"));
        }
        assert_eq!(
            endpoint("::1").unwrap().as_str(),
            "http://[::1]:8080/swarm.cgi?opcode=smb_debug_info"
        );
        assert_eq!(endpoint("ap.local.").unwrap().host_str(), Some("ap.local"));
        assert!(counter("ap cpu usage : -1", "ap cpu usage").is_none());
        assert_eq!(counter("ap cpu usage : 0", "ap cpu usage"), Some(0));
    }
}
