use std::{
    env, fs,
    io::{Read, Write},
    net::TcpListener,
    path::PathBuf,
    process::{Command, Output},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

const SITE: &str = "123e4567-e89b-12d3-a456-426614174000";
const AP: &str = "11:22:33:44:55:66";

struct TestHome(PathBuf);

impl TestHome {
    fn new() -> Self {
        static NEXT_HOME: AtomicUsize = AtomicUsize::new(0);
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/radio-test-homes");
        fs::create_dir_all(&root).expect("create isolated test-home parent");
        for _ in 0..100 {
            let path = root.join(format!(
                "{}-{}",
                std::process::id(),
                NEXT_HOME.fetch_add(1, Ordering::Relaxed)
            ));
            let mut builder = fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            match builder.create(&path) {
                Ok(()) => return Self(path),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("create isolated test home: {error}"),
            }
        }
        panic!("could not allocate an isolated test home");
    }
}

impl Drop for TestHome {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).expect("remove isolated test home");
    }
}

struct RejectProxy {
    address: String,
    requests: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl RejectProxy {
    fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind local reject proxy");
        listener
            .set_nonblocking(true)
            .expect("set reject proxy nonblocking");
        let address = listener.local_addr().expect("proxy address").to_string();
        let requests = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let worker_requests = Arc::clone(&requests);
        let worker_stop = Arc::clone(&stop);
        let worker = thread::spawn(move || {
            while !worker_stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        worker_requests.fetch_add(1, Ordering::SeqCst);
                        let _ = stream.set_read_timeout(Some(Duration::from_millis(100)));
                        let mut request = [0_u8; 1024];
                        let _ = stream.read(&mut request);
                        let _ = stream.write_all(
                            b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        );
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("proxy accept failed: {error}"),
                }
            }
        });
        Self {
            address,
            requests,
            stop,
            worker: Some(worker),
        }
    }

    fn count(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }
}

impl Drop for RejectProxy {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            worker.join().expect("join reject proxy");
        }
    }
}

fn run(args: &[&str]) -> (Output, usize) {
    let home = TestHome::new();
    let proxy = RejectProxy::new();
    let output = Command::new(env!("CARGO_BIN_EXE_instantctl"))
        .env("HOME", &home.0)
        .env("XDG_CACHE_HOME", home.0.join("xdg-cache"))
        .env("XDG_CONFIG_HOME", home.0.join("xdg-config"))
        .env("HPE_INSTANT_ON_TOKEN", "")
        .env("HTTPS_PROXY", format!("http://{}", proxy.address))
        .env("https_proxy", format!("http://{}", proxy.address))
        .env("NO_PROXY", "")
        .env("no_proxy", "")
        .args(["--format=json", "--profile", "radio-test", "--site", SITE])
        .args(args)
        .output()
        .expect("run instantctl");
    let requests = proxy.count();
    drop(proxy);
    drop(home);
    (output, requests)
}

fn assert_local_usage(output: &Output, requests: usize) {
    assert_eq!(
        output.status.code(),
        Some(2),
        "expected local usage error, got stdout {:?}, stderr {:?}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(requests, 0, "local CLI failures must send no HTTP");
}

fn assert_valid_config_reaches_credentials(output: &Output, requests: usize) {
    assert_eq!(output.status.code(), Some(2));
    let error: serde_json::Value =
        serde_json::from_slice(&output.stderr).expect("credential resolution returns a JSON error");
    assert_eq!(
        error["kind"], "config",
        "valid radio configuration must pass local usage validation: {error}"
    );
    assert_eq!(requests, 0, "missing credentials must stop before HTTP");
}

#[test]
fn plan_set_rejects_invalid_enums_before_credentials_or_http() {
    for args in [
        vec!["radio", "plan", "set", "--band", "invalid"],
        vec![
            "radio", "plan", "set", "--band", "2.4ghz", "--width", "invalid",
        ],
        vec![
            "radio",
            "plan",
            "set",
            "--band",
            "2.4ghz",
            "--min-power",
            "invalid",
        ],
        vec![
            "radio",
            "plan",
            "set",
            "--band",
            "2.4ghz",
            "--max-power",
            "invalid",
        ],
        vec![
            "radio",
            "plan",
            "set",
            "--band",
            "2.4ghz",
            "--band-mapping",
            "invalid",
        ],
    ] {
        let (output, requests) = run(&args);
        assert_local_usage(&output, requests);
    }
}

#[test]
fn plan_set_rejects_missing_band_and_empty_or_band_only_changes() {
    for args in [
        vec!["radio", "plan", "set", "--width", "20mhz"],
        vec!["radio", "plan", "set"],
        vec!["radio", "plan", "set", "--band", "2.4ghz"],
    ] {
        let (output, requests) = run(&args);
        assert_local_usage(&output, requests);
    }
}

#[test]
fn plan_set_rejects_duplicate_and_zero_channels_before_credentials_or_http() {
    for channels in ["1,1", "0"] {
        let (output, requests) = run(&[
            "radio",
            "plan",
            "set",
            "--band",
            "2.4ghz",
            "--channels",
            channels,
        ]);
        assert_local_usage(&output, requests);
    }
}

#[test]
fn plan_set_rejects_reversed_and_band_invalid_power_ranges() {
    for args in [
        vec![
            "radio",
            "plan",
            "set",
            "--band",
            "5ghz",
            "--min-power",
            "21dbm",
            "--max-power",
            "15dbm",
        ],
        vec![
            "radio",
            "plan",
            "set",
            "--band",
            "2.4ghz",
            "--max-power",
            "33dbm",
        ],
        vec![
            "radio",
            "plan",
            "set",
            "--band",
            "5ghz",
            "--min-power",
            "12dbm",
        ],
        vec![
            "radio",
            "plan",
            "set",
            "--band",
            "6ghz",
            "--max-power",
            "12dbm",
        ],
    ] {
        let (output, requests) = run(&args);
        assert_local_usage(&output, requests);
    }
}

#[test]
fn override_set_rejects_invalid_enums_and_missing_band() {
    for args in [
        vec![
            "radio", "override", "set", AP, "--band", "invalid", "--width", "20mhz",
        ],
        vec![
            "radio", "override", "set", AP, "--band", "2.4ghz", "--width", "invalid",
        ],
        vec!["radio", "override", "set", AP, "--band-mapping", "invalid"],
        vec!["radio", "override", "set", AP, "--width", "20mhz"],
    ] {
        let (output, requests) = run(&args);
        assert_local_usage(&output, requests);
    }
}

#[test]
fn override_set_rejects_empty_or_band_only_changes() {
    for args in [
        vec!["radio", "override", "set", AP],
        vec!["radio", "override", "set", AP, "--band", "5ghz"],
    ] {
        let (output, requests) = run(&args);
        assert_local_usage(&output, requests);
    }
}

#[test]
fn override_set_rejects_duplicate_and_zero_channels() {
    for channels in ["1,1", "0"] {
        let (output, requests) = run(&[
            "radio",
            "override",
            "set",
            AP,
            "--band",
            "2.4ghz",
            "--channels",
            channels,
        ]);
        assert_local_usage(&output, requests);
    }
}

#[test]
fn override_set_rejects_reversed_and_band_invalid_power_ranges() {
    for args in [
        vec![
            "radio",
            "override",
            "set",
            AP,
            "--band",
            "6ghz",
            "--min-power",
            "24dbm",
            "--max-power",
            "18dbm",
        ],
        vec![
            "radio",
            "override",
            "set",
            AP,
            "--band",
            "2.4ghz",
            "--min-power",
            "33dbm",
        ],
        vec![
            "radio",
            "override",
            "set",
            AP,
            "--band",
            "5ghz",
            "--max-power",
            "9dbm",
        ],
        vec![
            "radio",
            "override",
            "set",
            AP,
            "--band",
            "6ghz",
            "--min-power",
            "6dbm",
        ],
    ] {
        let (output, requests) = run(&args);
        assert_local_usage(&output, requests);
    }
}

#[test]
fn override_set_rejects_config_with_inherit_and_mapping_with_inherit_mapping() {
    for args in [
        vec![
            "radio",
            "override",
            "set",
            AP,
            "--band",
            "2.4ghz",
            "--inherit",
            "--width",
            "20mhz",
        ],
        vec![
            "radio",
            "override",
            "set",
            AP,
            "--band",
            "2.4ghz",
            "--inherit-band-mapping",
            "--band-mapping",
            "2.4ghz_and_5ghz",
        ],
    ] {
        let (output, requests) = run(&args);
        assert_local_usage(&output, requests);
    }
}

#[test]
fn valid_plan_set_passes_local_validation_and_reaches_credentials() {
    let (output, requests) = run(&[
        "radio",
        "plan",
        "set",
        "--band",
        "5ghz",
        "--width",
        "80mhz",
        "--channels",
        "36,40",
        "--min-power",
        "15dbm",
        "--max-power",
        "24dbm",
    ]);
    assert_valid_config_reaches_credentials(&output, requests);
}

#[test]
fn valid_override_set_passes_local_validation_and_reaches_credentials() {
    let (output, requests) = run(&[
        "radio",
        "override",
        "set",
        AP,
        "--band",
        "6ghz",
        "--width",
        "160mhz",
        "--channels",
        "5,21",
        "--min-power",
        "15dbm",
        "--max-power",
        "regulatoryMax",
    ]);
    assert_valid_config_reaches_credentials(&output, requests);
}

#[test]
fn radio_help_lists_plan_and_override_configuration_commands() {
    let (output, requests) = run(&["radio", "--help"]);
    assert_eq!(output.status.code(), Some(0));
    let help = String::from_utf8_lossy(&output.stdout);
    assert!(help.contains("plan"), "radio help omitted plan: {help}");
    assert!(
        help.contains("override"),
        "radio help omitted override: {help}"
    );
    assert_eq!(requests, 0, "help must not send HTTP");
}
