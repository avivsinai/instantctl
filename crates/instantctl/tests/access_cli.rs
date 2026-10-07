use serde_json::Value;
use std::{
    env, fs,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::PathBuf,
    process::{Command, Output, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

const TOKEN_ENV: &str = "HPE_INSTANT_ON_TOKEN";
const SITE: &str = "123e4567-e89b-12d3-a456-426614174000";

struct TestHome(PathBuf);

impl TestHome {
    fn new() -> Self {
        static NEXT_HOME: AtomicU64 = AtomicU64::new(0);
        let root =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/access-cli-test-homes");
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

struct MockProxy {
    address: String,
    requests: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl MockProxy {
    fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind local mock proxy");
        listener
            .set_nonblocking(true)
            .expect("set mock proxy nonblocking");
        let address = listener
            .local_addr()
            .expect("mock proxy address")
            .to_string();
        let requests = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let worker_requests = Arc::clone(&requests);
        let worker_stop = Arc::clone(&stop);
        let worker = thread::spawn(move || {
            while !worker_stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        worker_requests.fetch_add(1, Ordering::SeqCst);
                        reject_proxy_request(stream);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("mock proxy accept failed: {error}"),
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

    fn request_count(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }
}

impl Drop for MockProxy {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            worker.join().expect("join mock proxy");
        }
    }
}

fn reject_proxy_request(mut stream: TcpStream) {
    let _ = stream.set_read_timeout(Some(Duration::from_millis(250)));
    let mut request = [0_u8; 4096];
    let _ = stream.read(&mut request);
    let _ = stream
        .write_all(b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
}

fn run(args: &[&str], stdin: Option<&[u8]>) -> (Output, usize) {
    let home = TestHome::new();
    let proxy = MockProxy::new();
    let profile = format!("access-test-{}", std::process::id());
    let mut command = Command::new(env!("CARGO_BIN_EXE_instantctl"));
    command
        .env("HOME", &home.0)
        .env("XDG_CACHE_HOME", home.0.join("xdg-cache"))
        .env("XDG_CONFIG_HOME", home.0.join("xdg-config"))
        // An empty environment token prevents access to the real keychain.
        .env(TOKEN_ENV, "")
        .env("HTTPS_PROXY", format!("http://{}", proxy.address))
        .env("https_proxy", format!("http://{}", proxy.address))
        .env("NO_PROXY", "")
        .env("no_proxy", "")
        .arg("--format=json")
        .arg("--profile")
        .arg(profile)
        .arg("--site")
        .arg(SITE)
        .args(args);

    let output = if let Some(input) = stdin {
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().expect("spawn instantctl");
        if let Some(mut child_stdin) = child.stdin.take()
            && let Err(error) = child_stdin.write_all(input)
        {
            assert_eq!(error.kind(), std::io::ErrorKind::BrokenPipe);
        }
        child.wait_with_output().expect("wait for instantctl")
    } else {
        command.output().expect("run instantctl")
    };
    let request_count = proxy.request_count();
    drop(proxy);
    drop(home);
    (output, request_count)
}

fn error(output: &Output) -> Value {
    assert!(
        output.stdout.is_empty(),
        "unexpected stdout: {:?}",
        output.stdout
    );
    serde_json::from_slice(&output.stderr).expect("stderr contains a JSON error")
}

fn assert_usage(output: &Output, secrets: &[&[u8]]) {
    assert_eq!(
        output.status.code(),
        Some(2),
        "unexpected exit and stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(error(output)["kind"], "usage");
    for secret in secrets {
        assert!(!contains(&output.stdout, secret), "secret leaked to stdout");
        assert!(!contains(&output.stderr, secret), "secret leaked to stderr");
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

#[test]
fn radius_create_requires_primary_host_before_credentials_or_http() {
    let secret = b"radius-secret-sentinel";
    let (output, requests) = run(
        &["radius", "create", "office", "--primary-secret-stdin"],
        Some(b"radius-secret-sentinel\n"),
    );
    assert_usage(&output, &[secret]);
    assert_eq!(requests, 0, "local validation must not send HTTP");
}

#[test]
fn radius_create_requires_explicit_stdin_for_non_tty_secret_input() {
    let (output, requests) = run(
        &["radius", "create", "office", "--primary-host", "192.0.2.20"],
        None,
    );
    assert_usage(&output, &[]);
    assert_eq!(requests, 0, "input-mode validation must not send HTTP");
}

#[test]
fn radius_secret_values_are_not_accepted_as_flags_or_positionals() {
    let secret = b"never-echo-this-radius-secret";
    for args in [
        vec![
            "radius",
            "create",
            "office",
            "--primary-host",
            "192.0.2.20",
            "--primary-secret",
            "never-echo-this-radius-secret",
        ],
        vec![
            "radius",
            "create",
            "office",
            "never-echo-this-radius-secret",
            "--primary-host",
            "192.0.2.20",
            "--primary-secret-stdin",
        ],
    ] {
        let (output, requests) = run(&args, Some(b"stdin-secret-sentinel\n"));
        assert_usage(&output, &[secret, b"stdin-secret-sentinel"]);
        assert_eq!(requests, 0, "clap usage errors must not send HTTP");
    }
}

#[test]
fn radius_secret_stdin_rejects_token_stdin_and_oversized_input_without_echo() {
    let (output, requests) = run(
        &[
            "--token-stdin",
            "radius",
            "create",
            "office",
            "--primary-host",
            "192.0.2.20",
            "--primary-secret-stdin",
        ],
        Some(b"token-secret-sentinel\nradius-secret-sentinel\n"),
    );
    assert_usage(
        &output,
        &[b"token-secret-sentinel", b"radius-secret-sentinel"],
    );
    assert_eq!(requests, 0, "stdin conflict must be local");

    let oversized = vec![b'X'; 65];
    let (output, requests) = run(
        &[
            "radius",
            "create",
            "office",
            "--primary-host",
            "192.0.2.20",
            "--primary-secret-stdin",
        ],
        Some(&oversized),
    );
    assert_usage(&output, &[&oversized]);
    let message = error(&output)["message"].as_str().unwrap().to_owned();
    assert!(
        message.contains("64 characters"),
        "unexpected error: {message}"
    );
    assert_eq!(requests, 0, "bounded input validation must not send HTTP");
}

#[test]
fn schedule_rejects_invalid_weekday_times_before_credentials_or_http() {
    for interval in ["25:00-17:00", "08:00-08:60"] {
        let (output, requests) = run(
            &[
                "schedule",
                "create",
                "office-hours",
                "--mode",
                "week",
                "--week-day",
                &format!("monday={interval}"),
            ],
            None,
        );
        assert_usage(&output, &[]);
        assert_eq!(requests, 0, "invalid schedule input must stay local");
    }
}

#[test]
fn guest_portal_apply_requires_yes_before_http() {
    let (output, requests) = run(
        &["guest-portal", "update", "--welcome", "Welcome", "--apply"],
        None,
    );
    assert_eq!(
        output.status.code(),
        Some(2),
        "unexpected exit and stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(error(&output)["kind"], "confirmation_required");
    assert_eq!(requests, 0, "unconfirmed apply must not send HTTP");
}
