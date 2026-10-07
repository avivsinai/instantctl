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

struct TestHome(PathBuf);

impl TestHome {
    fn new() -> Self {
        static NEXT_HOME: AtomicUsize = AtomicUsize::new(0);
        let root =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/wlan-advanced-test-homes");
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
        .args([
            "--format=json",
            "--profile",
            "wlan-advanced-test",
            "--site",
            SITE,
        ])
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

#[test]
fn enterprise_requires_radius_profile_before_credentials_or_http() {
    let (output, requests) = run(&[
        "wlan",
        "create",
        "enterprise",
        "--security",
        "wpa2-enterprise",
    ]);
    assert_local_usage(&output, requests);
}

#[test]
fn mlo_rejects_explicit_five_ghz_wpa2_before_credentials_or_http() {
    let (output, requests) = run(&[
        "wlan",
        "create",
        "legacy",
        "--security",
        "wpa2-personal",
        "--bands",
        "5",
        "--mlo",
        "true",
    ]);
    assert_local_usage(&output, requests);
}

#[test]
fn binding_and_access_point_selectors_conflict_in_clap() {
    for args in [
        vec![
            "wlan",
            "create",
            "ssid",
            "--wired-network",
            "staff",
            "--vlan",
            "20",
        ],
        vec!["wlan", "create", "ssid", "--ap", "ap-1", "--all-aps"],
    ] {
        let (output, requests) = run(&args);
        assert_local_usage(&output, requests);
    }
}

#[test]
fn valid_enterprise_flags_pass_local_validation_and_reach_credentials() {
    let (output, requests) = run(&[
        "wlan",
        "create",
        "enterprise",
        "--security",
        "wpa3-enterprise",
        "--radius-profile",
        "corp-radius",
        "--traffic-priority",
        "very-high",
        "--all-aps",
        "--wifi7",
        "true",
    ]);
    assert_eq!(output.status.code(), Some(2));
    let error: serde_json::Value =
        serde_json::from_slice(&output.stderr).expect("credential resolution returns a JSON error");
    assert_eq!(
        error["kind"], "config",
        "valid advanced flags must pass local usage validation: {error}"
    );
    assert_eq!(requests, 0, "missing credentials must stop before HTTP");
}
