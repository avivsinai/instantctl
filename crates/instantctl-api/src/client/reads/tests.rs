use std::{
    collections::HashMap,
    io::{Read, Write},
    net::{SocketAddr, TcpListener},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use crate::{Client, ErrorKind, StaticToken};
use serde_json::{Value, json};

pub(crate) const SITE: &str = "12345678-1234-5678-1234-567812345678";
pub(crate) const MAC: &str = "aa:bb:cc:dd:ee:ff";

#[derive(Clone)]
pub(crate) struct Reply {
    pub(crate) status: u16,
    pub(crate) content_type: String,
    pub(crate) body: Vec<u8>,
    pub(crate) headers: Vec<(String, String)>,
}

impl Reply {
    pub(crate) fn json(value: Value) -> Self {
        Self {
            status: 200,
            content_type: "application/json".into(),
            body: serde_json::to_vec(&value).unwrap(),
            headers: vec![],
        }
    }
    pub(crate) fn xml(body: &str) -> Self {
        Self {
            status: 200,
            content_type: "application/xml".into(),
            body: body.as_bytes().to_vec(),
            headers: vec![],
        }
    }
}

pub(crate) struct Mock {
    pub(crate) address: SocketAddr,
    paths: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl Mock {
    pub(crate) fn new<P: AsRef<str>>(routes: impl IntoIterator<Item = (P, Reply)>) -> Self {
        let routes: HashMap<String, Reply> = routes
            .into_iter()
            .map(|(path, reply)| (path.as_ref().to_owned(), reply))
            .collect();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let paths = Arc::new(Mutex::new(vec![]));
        let stop = Arc::new(AtomicBool::new(false));
        let seen = paths.clone();
        let stopping = stop.clone();
        let worker = thread::spawn(move || {
            while !stopping.load(Ordering::SeqCst) {
                let (mut stream, _) = match listener.accept() {
                    Ok(connection) => connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(error) => panic!("mock accept: {error}"),
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut bytes = Vec::new();
                while !bytes.windows(4).any(|end| end == b"\r\n\r\n") {
                    let mut chunk = [0; 1024];
                    let size = stream.read(&mut chunk).unwrap();
                    if size == 0 {
                        break;
                    }
                    bytes.extend_from_slice(&chunk[..size]);
                    assert!(bytes.len() < 16 * 1024);
                }
                let request = String::from_utf8(bytes).unwrap();
                let parts: Vec<_> = request.lines().next().unwrap().split_whitespace().collect();
                assert_eq!(parts[0], "GET");
                let path = parts[1];
                let headers = request.to_ascii_lowercase();
                if path.starts_with("/api/") {
                    assert!(headers.contains("authorization: bearer mock-credential\r\n"));
                    assert!(headers.contains("x-ion-api-version: 28\r\n"));
                } else {
                    assert!(!headers.contains("authorization:"));
                }
                seen.lock().unwrap().push(path.into());
                let reply = routes.get(path).cloned().unwrap_or_else(|| Reply {
                    status: 404,
                    ..Reply::json(Value::Null)
                });
                let extra: String = reply
                    .headers
                    .iter()
                    .map(|(key, value)| format!("{key}: {value}\r\n"))
                    .collect();
                let header = format!(
                    "HTTP/1.1 {} Mock\r\nContent-Type: {}\r\nContent-Length: {}\r\n{extra}Connection: close\r\n\r\n",
                    reply.status,
                    reply.content_type,
                    reply.body.len()
                );
                if stream.write_all(header.as_bytes()).is_ok() {
                    let _ = stream.write_all(&reply.body);
                }
            }
        });
        Self {
            address,
            paths,
            stop,
            worker: Some(worker),
        }
    }
    pub(crate) fn client(&self) -> Client {
        Client::build(
            StaticToken::new("mock-credential").unwrap(),
            Duration::from_secs(3),
            url::Url::parse(&format!("http://{}/api", self.address)).unwrap(),
        )
        .unwrap()
    }
    pub(crate) fn paths(&self) -> Vec<String> {
        self.paths.lock().unwrap().clone()
    }
}

impl Drop for Mock {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let result = self.worker.take().unwrap().join();
        if !thread::panicking() {
            result.expect("mock server failed");
        }
    }
}

fn inventory(devices: Vec<Value>) -> Value {
    json!({"kind":"resourceList","totalCount":devices.len(),"matchingFilterCount":devices.len(),
        "pendingAvailability":null,"elements":devices})
}
fn device(name: &str) -> Value {
    json!({"id":MAC,"macAddress":MAC,"name":name})
}

#[tokio::test]
async fn sites_inventory_clients_and_capabilities_use_the_real_get_transport() {
    let base = format!("/api/sites/{SITE}");
    let inv = format!("{base}/inventory");
    let clients = format!("{base}/clientSummary");
    let caps = format!("{base}/capabilities");
    let server = Mock::new([
        (
            "/api/sites",
            Reply::json(json!({"elements":[{"id":SITE,"name":"Home"}]})),
        ),
        (
            inv.as_str(),
            Reply::json(inventory(vec![
                json!({"id":MAC,"macAddress":MAC,"name":"AP",
            "ethernetPorts":[{"faceplatePortNumber":1,"isLinkUp":true,"powerProvidedInMilliwatts":4000,
                "portDataTraffic":{"downstreamDataTransferredInBytesInLast24Hours":123}}],
            "trunkPorts":[{"trunkNumber":2,"name":"NAS"}],
            "radios":[{"band":"5GHz","channel":36,"txPowerEirpInDbm":18.5}]}),
            ])),
        ),
        (
            clients.as_str(),
            Reply::json(
                json!({"kind":"clientSummaries","elements":[{"id":"c1","macAddress":MAC,"name":"PC"}]}),
            ),
        ),
        (
            caps.as_str(),
            Reply::json(json!({"capabilities":["inventory","clientSummary"]})),
        ),
    ]);
    let api = server.client();
    let sites = api.sites().await.unwrap();
    assert_eq!(sites[0].name.as_deref(), Some("Home"));
    assert!(sites[0].health.is_none());
    let inventory = api.inventory(SITE).await.unwrap();
    assert!(inventory[0].status.is_none());
    let port = &inventory[0].ethernet_ports.as_ref().unwrap()[0];
    assert_eq!(port.faceplate_port_number, Some(1));
    assert_eq!(port.is_link_up, Some(true));
    assert_eq!(port.power_provided_in_milliwatts, Some(4000.0));
    assert_eq!(
        port.port_data_traffic
            .as_ref()
            .unwrap()
            .downstream_data_transferred_in_bytes_in_last24_hours,
        Some(123)
    );
    assert_eq!(
        inventory[0].trunk_ports.as_ref().unwrap()[0].trunk_number,
        Some(2)
    );
    assert_eq!(
        inventory[0].radios.as_ref().unwrap()[0].tx_power_eirp_in_dbm,
        Some(18.5)
    );
    let clients_data = api.clients(SITE).await.unwrap();
    assert_eq!(clients_data[0].name.as_deref(), Some("PC"));
    assert!(clients_data[0].snr_in_db.is_none());
    assert!(serde_json::to_value(&clients_data[0]).unwrap()["snrInDb"].is_null());
    assert_eq!(
        api.capabilities(SITE).await.unwrap(),
        ["inventory", "clientSummary"]
    );
    assert_eq!(
        server.paths(),
        vec!["/api/sites".into(), inv, clients, caps]
    );
    assert!(!format!("{api:?}").contains("mock-credential"));
}

#[tokio::test]
async fn inventory_rejects_partial_pending_paginated_duplicate_and_malformed_responses() {
    let good = inventory(vec![device("Switch")]);
    let mut cases = Vec::new();
    for (field, value) in [
        ("kind", json!("other")),
        ("totalCount", json!(2)),
        ("matchingFilterCount", json!(false)),
        ("pendingAvailability", json!(true)),
        ("metaData", json!({"nextPage":"cursor"})),
        ("elements", json!([{"id":"not-a-mac","macAddress":MAC}])),
        (
            "elements",
            json!([{"id":MAC,"macAddress":MAC,"ethernetPorts":["malformed"]}]),
        ),
        ("elements", json!([{"id":MAC,"macAddress":MAC,"radios":{}}])),
        (
            "elements",
            json!([{"id":MAC,"macAddress":MAC,"trunkPorts":[1]}]),
        ),
        (
            "elements",
            json!([{"id":MAC,"macAddress":MAC,"status":true}]),
        ),
    ] {
        let mut payload = good.clone();
        payload[field] = value;
        cases.push(payload);
    }
    for field in [
        "elements",
        "totalCount",
        "matchingFilterCount",
        "pendingAvailability",
    ] {
        let mut payload = good.clone();
        payload.as_object_mut().unwrap().remove(field);
        cases.push(payload);
    }
    cases.push(inventory(vec![device("A"), device("B")]));
    for payload in cases {
        let server = Mock::new([(
            &format!("/api/sites/{SITE}/inventory"),
            Reply::json(payload),
        )]);
        assert_eq!(
            server.client().inventory(SITE).await.unwrap_err().kind,
            ErrorKind::Unverified
        );
    }
}

#[tokio::test]
async fn oversized_inventory_is_refused_before_parsing() {
    let path = format!("/api/sites/{SITE}/inventory");
    let reply = Reply {
        body: vec![b' '; super::super::MAX_RESPONSE_BYTES + 1],
        ..Reply::json(Value::Null)
    };
    let server = Mock::new([(&path, reply)]);
    let error = server.client().inventory(SITE).await.unwrap_err();
    assert_eq!(error.kind, ErrorKind::General);
    assert!(error.to_string().contains("size limit"));
    assert_eq!(server.paths(), [path]);
}

#[tokio::test]
async fn client_summaries_refuse_wrong_shape_pagination_and_duplicate_or_missing_identity() {
    let good = json!({"kind":"clientSummaries","elements":[{"id":"c1","macAddress":MAC}]});
    let mut cases = vec![
        json!({"kind":"wrong","elements":[]}),
        json!({"kind":"clientSummaries","elements":null}),
        json!({"kind":"clientSummaries","elements":[{"id":"c1"}]}),
        json!({"kind":"clientSummaries","elements":[{"id":"c1","macAddress":"bad"}]}),
        json!({"kind":"clientSummaries","elements":[{"id":"c1","macAddress":MAC,"dataTraffic":"secret-invalid-field"}]}),
    ];
    let mut payload = good.clone();
    payload["metaData"] = json!({"hasMore":true});
    cases.push(payload);
    let mut payload = good.clone();
    payload["elements"]
        .as_array_mut()
        .unwrap()
        .push(good["elements"][0].clone());
    cases.push(payload);
    for payload in cases {
        let server = Mock::new([(
            &format!("/api/sites/{SITE}/clientSummary"),
            Reply::json(payload),
        )]);
        let error = server.client().clients(SITE).await.unwrap_err();
        assert_eq!(error.kind, ErrorKind::Unverified);
        assert!(!format!("{error:?} {error}").contains("secret-invalid-field"));
    }
}

#[tokio::test]
async fn sites_and_capabilities_refuse_invalid_response_shapes() {
    for payload in [
        json!({}),
        json!({"elements":[{"id":"invalid"}]}),
        json!({"elements":[{"id":SITE},{"id":SITE}]}),
        json!({"elements":[{"id":SITE}],"metaData":{"partial":true}}),
    ] {
        let server = Mock::new([("/api/sites", Reply::json(payload))]);
        assert_eq!(
            server.client().sites().await.unwrap_err().kind,
            ErrorKind::Unverified
        );
    }
    for payload in [
        json!({"capabilities":null}),
        json!({"capabilities":[1]}),
        json!({}),
    ] {
        let server = Mock::new([(
            &format!("/api/sites/{SITE}/capabilities"),
            Reply::json(payload),
        )]);
        assert_eq!(
            server.client().capabilities(SITE).await.unwrap_err().kind,
            ErrorKind::Unverified
        );
    }
}

#[tokio::test]
async fn site_counts_must_be_consistent_when_reported() {
    for field in ["totalCount", "matchingFilterCount"] {
        for count in [json!(2), json!(false), json!(-1), json!("1"), Value::Null] {
            let mut payload = json!({"elements":[{"id":SITE,"name":"Home"}]});
            payload[field] = count;
            let server = Mock::new([("/api/sites", Reply::json(payload))]);
            assert_eq!(
                server.client().sites().await.unwrap_err().kind,
                ErrorKind::Unverified
            );
        }
    }
    let server = Mock::new([(
        "/api/sites",
        Reply::json(json!({
            "elements":[{"id":SITE}],"totalCount":1,"matchingFilterCount":1
        })),
    )]);
    assert_eq!(server.client().sites().await.unwrap().len(), 1);
}

#[tokio::test]
async fn invalid_site_is_config_before_any_http_and_auth_failures_stay_typed() {
    let server = Mock::new(std::iter::empty::<(&str, Reply)>());
    let api = server.client();
    for error in [
        api.inventory("../escape").await.unwrap_err(),
        api.clients("bad").await.unwrap_err(),
        api.capabilities("bad").await.unwrap_err(),
    ] {
        assert_eq!(error.kind, ErrorKind::Config);
    }
    assert!(server.paths().is_empty());
    let path = format!("/api/sites/{SITE}/inventory");
    let server = Mock::new([(
        &path,
        Reply {
            status: 401,
            ..Reply::json(json!({"error":"secret"}))
        },
    )]);
    let error = server.client().inventory(SITE).await.unwrap_err();
    assert_eq!(error.kind, ErrorKind::Auth);
    assert!(!format!("{error:?} {error}").contains("secret"));
}

#[tokio::test]
async fn inventory_redirects_are_never_followed() {
    let path = format!("/api/sites/{SITE}/inventory");
    let foreign = Mock::new([(&path, Reply::json(inventory(vec![device("Switch")])))]);
    let server = Mock::new([(
        &path,
        Reply {
            status: 302,
            headers: vec![(
                "Location".into(),
                format!("http://{}{path}", foreign.address),
            )],
            ..Reply::json(Value::Null)
        },
    )]);
    assert_eq!(
        server.client().inventory(SITE).await.unwrap_err().kind,
        ErrorKind::General
    );
    assert!(foreign.paths().is_empty());
}
