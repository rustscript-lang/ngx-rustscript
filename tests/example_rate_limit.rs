//! Compile/config schema and real multi-worker NGINX checks for the
//! copy-paste shared-dict fixed-window rate-limit example.

use ngx_rustscript_nginx_test_stubs as _;

mod support;

use std::fs;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddrV4, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use ngx_rustscript::config::{compile_rss_source, program_requires_request_state};
use support::artifact::module_cdylib;
use support::nginx::{combined, nginx_binary, nginx_quote_path, nginx_test};
use support::temp::{OwnedTempDir, isolated_prefix};

const EXAMPLE_RSS: &str = include_str!("../examples/rate-limit.rss");
const EXAMPLE_CONF: &str = include_str!("../examples/rate-limit.nginx.conf");

/// Must match `let LIMIT = …` in `examples/rate-limit.rss`.
const LIMIT: usize = 5;
/// Must match `let WINDOW_SECONDS = …` / `let RETRY_AFTER = "…"` in the example.
const WINDOW_SECONDS: u64 = 2;
const RETRY_AFTER: &str = "2";
const ZONE: &str = "ratelimit";
const LONG_WINDOW_SECONDS: u64 = 3600;
const WORKER_PID_HEADER: &str = "X-Worker-Pid";

struct RunningNginx {
    child: Option<Child>,
    prefix: PathBuf,
    nginx: PathBuf,
}

impl RunningNginx {
    fn quit_wait(&mut self) -> std::process::ExitStatus {
        let _ = Command::new(&self.nginx)
            .arg("-s")
            .arg("quit")
            .arg("-p")
            .arg(&self.prefix)
            .arg("-c")
            .arg("conf/nginx.conf")
            .output();
        let mut child = self.child.take().expect("nginx child already reaped");
        child.wait().expect("wait nginx after SIGQUIT")
    }
}

impl Drop for RunningNginx {
    fn drop(&mut self) {
        if self.child.is_some() {
            let _ = self.quit_wait();
        }
    }
}

struct HarnessSpec<'a> {
    label: &'a str,
    http_extra: &'a str,
    set_key: &'a str,
    rss: &'a str,
    extra_server: &'a str,
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn write_conf(prefix: &Path, body: &str) {
    fs::write(prefix.join("conf/nginx.conf"), body).unwrap();
}

fn write_script(path: &Path, body: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, body).unwrap();
}

fn write_index(prefix: &Path) {
    fs::write(prefix.join("html/index.html"), "ok\n").unwrap();
}

fn example_rss_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/rate-limit.rss")
}

fn header_value(response: &str, name: &str) -> Option<String> {
    let needle = format!("{name}:");
    for line in response.lines() {
        if line.len() >= needle.len() && line[..needle.len()].eq_ignore_ascii_case(&needle) {
            return Some(line[needle.len()..].trim().to_string());
        }
    }
    None
}

fn status_of(response: &str) -> u16 {
    response
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

fn content_length(response: &str) -> Option<usize> {
    header_value(response, "Content-Length")?.parse().ok()
}

fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

fn read_http(stream: &mut TcpStream) -> (u16, String) {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 1024];
    loop {
        let n = stream.read(&mut tmp).unwrap_or(0);
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(header_end) = find_header_end(&buf) {
            let headers = String::from_utf8_lossy(&buf[..header_end]).into_owned();
            let body_start = header_end + 4;
            if let Some(len) = content_length(&headers) {
                while buf.len() < body_start + len {
                    let n = stream.read(&mut tmp).unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&tmp[..n]);
                }
            }
            break;
        }
        if buf.len() > 64 * 1024 {
            panic!("HTTP headers exceeded 64KiB");
        }
    }
    let body = String::from_utf8_lossy(&buf).into_owned();
    (status_of(&body), body)
}

fn http_get_to(dest: SocketAddrV4, path: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(dest).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    write!(
        stream,
        "GET {path} HTTP/1.0\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let _ = stream.flush();
    let mut body = String::new();
    stream.read_to_string(&mut body).unwrap();
    (status_of(&body), body)
}

fn http_get(port: u16, path: &str) -> (u16, String) {
    http_get_to(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port), path)
}

fn http_get_from(src: Ipv4Addr, port: u16, path: &str) -> (u16, String) {
    let url = format!("http://127.0.0.1:{port}{path}");
    let out = Command::new("curl")
        .args([
            "--interface",
            &src.to_string(),
            "--http1.0",
            "-sS",
            "-i",
            "--max-time",
            "5",
            &url,
        ])
        .output()
        .unwrap_or_else(|err| panic!("curl --interface {src}: {err}"));
    assert!(
        out.status.success(),
        "curl --interface {src} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let body = String::from_utf8_lossy(&out.stdout).into_owned();
    (status_of(&body), body)
}

struct Keepalive {
    stream: TcpStream,
}

impl Keepalive {
    fn connect(port: u16) -> Self {
        let stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        Self { stream }
    }

    fn get(&mut self, path: &str) -> (u16, String) {
        write!(
            self.stream,
            "GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: keep-alive\r\n\r\n"
        )
        .unwrap();
        let _ = self.stream.flush();
        read_http(&mut self.stream)
    }
}

fn wait_for_port(port: u16, timeout: Duration) {
    let start = Instant::now();
    loop {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return;
        }
        if start.elapsed() >= timeout {
            panic!("nginx did not listen on 127.0.0.1:{port} within {timeout:?}");
        }
        thread::sleep(Duration::from_millis(20));
    }
}

fn error_log(tmp: &OwnedTempDir) -> String {
    fs::read_to_string(tmp.path().join("logs/error.log")).unwrap_or_default()
}

fn spawn_nginx(nginx: &Path, prefix: &Path) -> RunningNginx {
    let child = Command::new(nginx)
        .arg("-p")
        .arg(prefix)
        .arg("-c")
        .arg("conf/nginx.conf")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn nginx");
    RunningNginx {
        child: Some(child),
        prefix: prefix.to_path_buf(),
        nginx: nginx.to_path_buf(),
    }
}

fn assert_retry_after_value(body: &str, expected: &str) {
    let got =
        header_value(body, "Retry-After").unwrap_or_else(|| panic!("Retry-After missing: {body}"));
    assert_eq!(got, expected, "Retry-After mismatch: {body}");
}

fn assert_retry_after(body: &str) {
    assert_retry_after_value(body, RETRY_AFTER);
}

fn rss_with_window(window: u64) -> String {
    EXAMPLE_RSS
        .replace(
            &format!("let WINDOW_SECONDS = {WINDOW_SECONDS};"),
            &format!("let WINDOW_SECONDS = {window};"),
        )
        .replace(
            &format!("let RETRY_AFTER = \"{RETRY_AFTER}\";"),
            &format!("let RETRY_AFTER = \"{window}\";"),
        )
}

fn test_conf(
    module: &Path,
    prefix: &Path,
    port: u16,
    script: &Path,
    http_extra: &str,
    set_key: &str,
    extra_server: &str,
) -> String {
    let module = nginx_quote_path(module).unwrap();
    let pid = nginx_quote_path(&prefix.join("logs/nginx.pid")).unwrap();
    let error_log = nginx_quote_path(&prefix.join("logs/error.log")).unwrap();
    let script = nginx_quote_path(script).unwrap();
    let html = nginx_quote_path(&prefix.join("html")).unwrap();
    format!(
        r#"
worker_processes 2;
error_log {error_log} info;
pid {pid};
daemon off;
master_process on;
load_module {module};

events {{
    worker_connections 128;
}}

http {{
    access_log off;
    keepalive_timeout 75s;
    {http_extra}
    server {{
        listen 127.0.0.1:{port};
        server_name localhost;
        root {html};
        {set_key}
        {extra_server}
        location / {{
            access_by_rss_file {script};
            try_files /index.html =404;
        }}
        location /whoami {{
            add_header {WORKER_PID_HEADER} $pid always;
            return 204;
        }}
    }}
}}
"#
    )
}

fn setup(spec: HarnessSpec<'_>) -> (PathBuf, u16, OwnedTempDir, RunningNginx, PathBuf) {
    let nginx = nginx_binary();
    let module = module_cdylib();
    let tmp = isolated_prefix(spec.label);
    let port = free_port();
    write_index(tmp.path());
    let script = tmp.path().join("conf/rate-limit.rss");
    write_script(&script, spec.rss);
    write_conf(
        tmp.path(),
        &test_conf(
            &module,
            tmp.path(),
            port,
            &script,
            spec.http_extra,
            spec.set_key,
            spec.extra_server,
        ),
    );
    let out = nginx_test(&nginx, tmp.path());
    assert!(out.status.success(), "nginx -t failed: {}", combined(&out));
    let running = spawn_nginx(&nginx, tmp.path());
    wait_for_port(port, Duration::from_secs(5));
    (nginx, port, tmp, running, script)
}

fn setup_keyed(label: &str) -> (PathBuf, u16, OwnedTempDir, RunningNginx) {
    let (nginx, port, tmp, running, _) = setup(HarnessSpec {
        label,
        http_extra: &format!("rss_shared_dict {ZONE} 1m;"),
        set_key: "set $rate_limit_key $arg_id;",
        rss: EXAMPLE_RSS,
        extra_server: "",
    });
    (nginx, port, tmp, running)
}

fn get_id(port: u16, id: &str) -> (u16, String) {
    http_get(port, &format!("/?id={id}"))
}

fn assert_documented_constants() {
    assert!(
        EXAMPLE_RSS.contains(&format!("let LIMIT = {LIMIT};")),
        "LIMIT constant missing from example: {EXAMPLE_RSS}"
    );
    assert!(
        EXAMPLE_RSS.contains(&format!("let WINDOW_SECONDS = {WINDOW_SECONDS};")),
        "WINDOW_SECONDS constant missing from example: {EXAMPLE_RSS}"
    );
    assert!(
        EXAMPLE_RSS.contains(&format!("let RETRY_AFTER = \"{RETRY_AFTER}\";")),
        "RETRY_AFTER constant missing from example: {EXAMPLE_RSS}"
    );
    assert!(
        EXAMPLE_RSS.contains(&format!("ngx::shared::dict(\"{ZONE}\")")),
        "zone name missing from example: {EXAMPLE_RSS}"
    );
    assert!(
        EXAMPLE_RSS.contains("ngx::shared::safe_add(dict, key, 1, WINDOW_SECONDS, null)"),
        "new windows must use safe_add: {EXAMPLE_RSS}"
    );
    assert!(
        EXAMPLE_RSS.contains("ngx::shared::incr(dict, key, 1, null, null)"),
        "existing windows must incr without init: {EXAMPLE_RSS}"
    );
    assert!(
        !EXAMPLE_RSS.contains("incr(dict, key, 1, 0,"),
        "allocating incr init must not create windows: {EXAMPLE_RSS}"
    );
}

#[test]
fn example_rss_compiles_and_requires_request_state() {
    assert_documented_constants();
    let disk = fs::read_to_string(example_rss_path()).expect("read examples/rate-limit.rss");
    assert_eq!(
        disk, EXAMPLE_RSS,
        "include_str drifted from examples/rate-limit.rss"
    );
    let program = compile_rss_source(EXAMPLE_RSS).expect("example rss compiles");
    assert!(
        program_requires_request_state(&program),
        "rate limiter reads $rate_limit_key and must be request-owned"
    );
}

#[test]
fn example_rss_does_not_log_keys_or_trust_client_headers() {
    let rss = EXAMPLE_RSS.to_ascii_lowercase();
    assert!(
        !rss.contains("ngx::log"),
        "example must not log keys or identities"
    );
    let conf = EXAMPLE_CONF.to_ascii_lowercase();
    assert!(
        conf.contains("set $rate_limit_key $binary_remote_addr;"),
        "default identity must be $binary_remote_addr:\n{EXAMPLE_CONF}"
    );
    assert!(
        !conf.contains("set $rate_limit_key $http_")
            && !conf.contains("set $rate_limit_key $arg_")
            && !conf.contains("set $rate_limit_key $cookie_"),
        "example conf must not assign $rate_limit_key from a client-controlled source:\n{EXAMPLE_CONF}"
    );
    assert!(
        EXAMPLE_CONF.contains("rss_shared_dict ratelimit 10m;"),
        "example conf must declare the shared dict zone"
    );
    assert!(
        EXAMPLE_CONF.contains("worker_processes 2;"),
        "example conf must run at least two workers"
    );
    assert!(
        EXAMPLE_CONF.contains("root /absolute/path/to/html;"),
        "example conf must template the document root"
    );
    assert!(
        EXAMPLE_CONF.contains("try_files /index.html =404;"),
        "example conf must use a content-phase handler so access runs"
    );
    assert!(
        !EXAMPLE_CONF.contains("return "),
        "rewrite-phase return would skip access_by_rss_file:\n{EXAMPLE_CONF}"
    );
    assert!(
        EXAMPLE_CONF.contains("ngx_http_realip") || EXAMPLE_CONF.contains("set_real_ip_from"),
        "example conf must point operators at ngx_http_realip, not raw XFF:\n{EXAMPLE_CONF}"
    );
}

#[test]
fn example_conf_nginx_t_accepts_templated_copy() {
    let nginx = nginx_binary();
    let module = module_cdylib();
    let tmp = isolated_prefix("rate-limit-example-conf");
    write_index(tmp.path());
    let script = tmp.path().join("conf/rate-limit.rss");
    write_script(&script, EXAMPLE_RSS);
    let rendered = EXAMPLE_CONF
        .replace(
            "/absolute/path/to/libngx_rustscript.so",
            &nginx_quote_path(&module).unwrap(),
        )
        .replace(
            "/absolute/path/to/rate-limit.rss",
            &nginx_quote_path(&script).unwrap(),
        )
        .replace(
            "/absolute/path/to/error.log",
            &nginx_quote_path(&tmp.path().join("logs/error.log")).unwrap(),
        )
        .replace(
            "/absolute/path/to/nginx.pid",
            &nginx_quote_path(&tmp.path().join("logs/nginx.pid")).unwrap(),
        )
        .replace(
            "/absolute/path/to/html",
            &nginx_quote_path(&tmp.path().join("html")).unwrap(),
        );
    write_conf(tmp.path(), &rendered);
    let out = nginx_test(&nginx, tmp.path());
    assert!(
        out.status.success(),
        "templated example conf failed nginx -t: {}",
        combined(&out)
    );
}

#[test]
fn first_n_are_200_then_429_with_retry_after() {
    let (_nginx, port, tmp, _running) = setup_keyed("rate-limit-n");
    for i in 0..LIMIT {
        let (status, body) = get_id(port, "n");
        assert_eq!(
            status,
            200,
            "request {} of {LIMIT} should be 200: {body}\n{}",
            i + 1,
            error_log(&tmp)
        );
        assert!(
            header_value(&body, "Retry-After").is_none(),
            "200 must not send Retry-After: {body}"
        );
    }
    let (status, body) = get_id(port, "n");
    assert_eq!(
        status,
        429,
        "request {} should be 429: {body}\n{}",
        LIMIT + 1,
        error_log(&tmp)
    );
    assert_retry_after(&body);
}

#[test]
fn window_resets_after_ttl() {
    let (_nginx, port, tmp, _running) = setup_keyed("rate-limit-ttl");
    for i in 0..LIMIT {
        let (status, body) = get_id(port, "ttl");
        assert_eq!(status, 200, "pre-wait {i}: {body}\n{}", error_log(&tmp));
    }
    let (status, body) = get_id(port, "ttl");
    assert_eq!(
        status,
        429,
        "pre-wait overflow: {body}\n{}",
        error_log(&tmp)
    );
    assert_retry_after(&body);
    thread::sleep(Duration::from_millis(WINDOW_SECONDS * 1000 + 1500));
    let (status, body) = get_id(port, "ttl");
    assert_eq!(
        status,
        200,
        "first request after TTL should start a new window: {body}\n{}",
        error_log(&tmp)
    );
}

#[test]
fn concurrent_burst_allows_exactly_n() {
    let (_nginx, port, tmp, _running) = setup_keyed("rate-limit-burst");
    let allowed = AtomicUsize::new(0);
    let denied = AtomicUsize::new(0);
    let total = LIMIT * 4;
    thread::scope(|scope| {
        for _ in 0..total {
            scope.spawn(|| {
                let (status, body) = get_id(port, "burst");
                match status {
                    200 => {
                        assert!(
                            header_value(&body, "Retry-After").is_none(),
                            "200 with Retry-After: {body}"
                        );
                        allowed.fetch_add(1, Ordering::SeqCst);
                    }
                    429 => {
                        assert_retry_after(&body);
                        denied.fetch_add(1, Ordering::SeqCst);
                    }
                    other => panic!("unexpected status {other}: {body}\n{}", error_log(&tmp)),
                }
            });
        }
    });
    assert_eq!(
        allowed.load(Ordering::SeqCst),
        LIMIT,
        "atomic burst must allow exactly {LIMIT} under 2 workers\n{}",
        error_log(&tmp)
    );
    assert_eq!(
        denied.load(Ordering::SeqCst),
        total - LIMIT,
        "remaining burst requests must be 429"
    );
}

#[test]
fn shared_state_across_two_distinct_worker_pids() {
    let extra = format!("add_header {WORKER_PID_HEADER} $pid always;");
    let (_nginx, port, tmp, _running, _) = setup(HarnessSpec {
        label: "rate-limit-shared-pid",
        http_extra: &format!("rss_shared_dict {ZONE} 1m;"),
        set_key: "set $rate_limit_key $arg_id;",
        rss: EXAMPLE_RSS,
        extra_server: &extra,
    });

    let mut conn_a = Keepalive::connect(port);
    let (status, body) = conn_a.get("/whoami");
    assert_eq!(status, 204, "whoami A: {body}\n{}", error_log(&tmp));
    let pid_a = header_value(&body, WORKER_PID_HEADER)
        .unwrap_or_else(|| panic!("missing {WORKER_PID_HEADER} on A: {body}"));
    assert!(!pid_a.is_empty(), "empty worker pid on A: {body}");

    let mut conn_b = None;
    let mut pid_b = String::new();
    for _ in 0..32 {
        let mut candidate = Keepalive::connect(port);
        let (status, body) = candidate.get("/whoami");
        assert_eq!(status, 204, "whoami B: {body}\n{}", error_log(&tmp));
        let pid = header_value(&body, WORKER_PID_HEADER)
            .unwrap_or_else(|| panic!("missing {WORKER_PID_HEADER} on B: {body}"));
        if pid != pid_a {
            pid_b = pid;
            conn_b = Some(candidate);
            break;
        }
    }
    let mut conn_b = conn_b.unwrap_or_else(|| {
        panic!(
            "could not obtain a second worker PID (A={pid_a})\n{}",
            error_log(&tmp)
        )
    });
    assert_ne!(pid_a, pid_b, "connections must be distinct worker PIDs");

    let mut allowed = 0usize;
    let mut denied = 0usize;
    let total = LIMIT + 4;
    for i in 0..total {
        let conn = if i % 2 == 0 { &mut conn_a } else { &mut conn_b };
        let (status, body) = conn.get("/?id=shared-pid");
        let pid = header_value(&body, WORKER_PID_HEADER)
            .unwrap_or_else(|| panic!("rate-limited response missing pid: {body}"));
        if i % 2 == 0 {
            assert_eq!(pid, pid_a, "connection A left its worker: {body}");
        } else {
            assert_eq!(pid, pid_b, "connection B left its worker: {body}");
        }
        match status {
            200 => {
                assert!(
                    header_value(&body, "Retry-After").is_none(),
                    "200 with Retry-After: {body}"
                );
                allowed += 1;
            }
            429 => {
                assert_retry_after(&body);
                denied += 1;
            }
            other => panic!("unexpected status {other}: {body}\n{}", error_log(&tmp)),
        }
    }
    assert_eq!(
        allowed,
        LIMIT,
        "two worker PIDs ({pid_a}, {pid_b}) must share one LIMIT, not {LIMIT}*2\n{}",
        error_log(&tmp)
    );
    assert_eq!(denied, total - LIMIT, "overflow must be 429");
}

#[test]
fn distinct_trusted_keys_are_isolated() {
    let (_nginx, port, tmp, _running) = setup_keyed("rate-limit-iso");
    for _ in 0..LIMIT {
        let (status, body) = get_id(port, "alice");
        assert_eq!(
            status,
            200,
            "alice under limit: {body}\n{}",
            error_log(&tmp)
        );
    }
    let (status, body) = get_id(port, "alice");
    assert_eq!(status, 429, "alice exceeded: {body}\n{}", error_log(&tmp));
    assert_retry_after(&body);
    let (status, body) = get_id(port, "bob");
    assert_eq!(
        status,
        200,
        "bob must not share alice's window: {body}\n{}",
        error_log(&tmp)
    );
}

#[test]
fn default_binary_remote_addr_isolates_loopback_peers() {
    let probe = TcpListener::bind("127.0.0.2:0");
    assert!(
        probe.is_ok(),
        "test requires 127.0.0.2 loopback: {:?}",
        probe.err()
    );
    drop(probe);

    let (_nginx, port, tmp, _running, _) = setup(HarnessSpec {
        label: "rate-limit-binary-addr",
        http_extra: &format!("rss_shared_dict {ZONE} 1m;"),
        set_key: "set $rate_limit_key $binary_remote_addr;",
        rss: EXAMPLE_RSS,
        extra_server: "",
    });

    let a = Ipv4Addr::new(127, 0, 0, 1);
    let b = Ipv4Addr::new(127, 0, 0, 2);
    for i in 0..LIMIT {
        let (status, body) = http_get_from(a, port, "/");
        assert_eq!(
            status,
            200,
            "127.0.0.1 request {} of {LIMIT}: {body}\n{}",
            i + 1,
            error_log(&tmp)
        );
    }
    let (status, body) = http_get_from(a, port, "/");
    assert_eq!(
        status,
        429,
        "127.0.0.1 must exhaust its own quota: {body}\n{}",
        error_log(&tmp)
    );
    assert_retry_after(&body);

    for i in 0..LIMIT {
        let (status, body) = http_get_from(b, port, "/");
        assert_eq!(
            status,
            200,
            "127.0.0.2 request {} of {LIMIT} must be an independent quota (embedded-NUL key): {body}\n{}",
            i + 1,
            error_log(&tmp)
        );
    }
    let (status, body) = http_get_from(b, port, "/");
    assert_eq!(
        status,
        429,
        "127.0.0.2 must exhaust its own quota: {body}\n{}",
        error_log(&tmp)
    );
    assert_retry_after(&body);
}

#[test]
fn missing_key_fails_closed() {
    let (_nginx, port, tmp, _running, _) = setup(HarnessSpec {
        label: "rate-limit-missing-key",
        http_extra: &format!("rss_shared_dict {ZONE} 1m;"),
        set_key: "",
        rss: EXAMPLE_RSS,
        extra_server: "",
    });
    let (status, body) = http_get(port, "/");
    assert_eq!(
        status,
        503,
        "missing $rate_limit_key must fail closed: {body}\n{}",
        error_log(&tmp)
    );
}

#[test]
fn present_empty_key_fails_closed() {
    let (_nginx, port, tmp, _running, _) = setup(HarnessSpec {
        label: "rate-limit-empty-key",
        http_extra: &format!("rss_shared_dict {ZONE} 1m;"),
        set_key: r#"set $rate_limit_key "";"#,
        rss: EXAMPLE_RSS,
        extra_server: "",
    });
    let (status, body) = http_get(port, "/");
    assert_eq!(
        status,
        503,
        "present-empty $rate_limit_key must fail closed: {body}\n{}",
        error_log(&tmp)
    );
}

#[test]
fn unknown_zone_fails_closed() {
    let (_nginx, port, tmp, _running, _) = setup(HarnessSpec {
        label: "rate-limit-unknown-zone",
        http_extra: "rss_shared_dict other 1m;",
        set_key: "set $rate_limit_key $arg_id;",
        rss: EXAMPLE_RSS,
        extra_server: "",
    });
    let (status, body) = get_id(port, "zone");
    assert_eq!(
        status,
        500,
        "unknown zone must fail closed: {body}\n{}",
        error_log(&tmp)
    );
}

#[test]
fn reload_preserves_window_count() {
    let long = rss_with_window(LONG_WINDOW_SECONDS);
    let (nginx, port, tmp, _running, _) = setup(HarnessSpec {
        label: "rate-limit-reload",
        http_extra: &format!("rss_shared_dict {ZONE} 1m;"),
        set_key: "set $rate_limit_key $arg_id;",
        rss: &long,
        extra_server: "",
    });
    for i in 0..(LIMIT - 1) {
        let (status, body) = get_id(port, "reload");
        assert_eq!(status, 200, "pre-reload {i}: {body}\n{}", error_log(&tmp));
    }
    let reload = Command::new(&nginx)
        .arg("-s")
        .arg("reload")
        .arg("-p")
        .arg(tmp.path())
        .arg("-c")
        .arg("conf/nginx.conf")
        .output()
        .expect("nginx -s reload");
    assert!(
        reload.status.success(),
        "reload failed: {}",
        combined(&reload)
    );
    wait_for_port(port, Duration::from_secs(5));

    let start = Instant::now();
    let mut remaining = None;
    while start.elapsed() < Duration::from_secs(5) {
        match TcpStream::connect(("127.0.0.1", port)) {
            Ok(_) => {
                let (status, body) = get_id(port, "reload");
                match status {
                    200 => {
                        remaining = Some(body);
                        break;
                    }
                    429 => panic!(
                        "reload dropped in-window count: {body}\n{}",
                        error_log(&tmp)
                    ),
                    502 | 0 => {
                        thread::sleep(Duration::from_millis(20));
                    }
                    other => panic!(
                        "unexpected post-reload status {other}: {body}\n{}",
                        error_log(&tmp)
                    ),
                }
            }
            Err(_) => thread::sleep(Duration::from_millis(20)),
        }
    }
    let body = remaining.unwrap_or_else(|| {
        panic!(
            "post-reload remaining slot never served 200 within bound\n{}",
            error_log(&tmp)
        )
    });
    assert!(
        header_value(&body, "Retry-After").is_none(),
        "remaining slot must not send Retry-After: {body}"
    );
    let (status, body) = get_id(port, "reload");
    assert_eq!(
        status,
        429,
        "reload must keep the shared count so the next request is 429: {body}\n{}",
        error_log(&tmp)
    );
    assert_retry_after_value(&body, &LONG_WINDOW_SECONDS.to_string());
}

#[test]
fn capacity_pressure_fails_closed_without_resetting_live_quotas() {
    let long = rss_with_window(LONG_WINDOW_SECONDS);
    let (_nginx, port, tmp, _running, _) = setup(HarnessSpec {
        label: "rate-limit-capacity",
        http_extra: &format!("rss_shared_dict {ZONE} 8k;"),
        set_key: "set $rate_limit_key $arg_id;",
        rss: &long,
        extra_server: "",
    });

    let mut live = Vec::new();
    let mut overflow_503 = false;
    for i in 0..512 {
        let id = format!("cap{i:04}");
        let (status, body) = get_id(port, &id);
        match status {
            200 => live.push(id),
            503 => {
                overflow_503 = true;
                break;
            }
            other => panic!(
                "unexpected status while filling zone ({other}): {body}\n{}",
                error_log(&tmp)
            ),
        }
    }
    assert!(
        overflow_503,
        "tiny zone never returned 503 for a new identity after {} live keys\n{}",
        live.len(),
        error_log(&tmp)
    );
    assert!(
        !live.is_empty(),
        "tiny zone must admit at least one live identity before 503\n{}",
        error_log(&tmp)
    );

    for id in &live {
        for n in 1..LIMIT {
            let (status, body) = get_id(port, id);
            assert_eq!(
                status,
                200,
                "live {id} request {} of {LIMIT}: {body}\n{}",
                n + 1,
                error_log(&tmp)
            );
        }
        let (status, body) = get_id(port, id);
        assert_eq!(
            status,
            429,
            "live {id} must hit LIMIT without being reset: {body}\n{}",
            error_log(&tmp)
        );
        assert_retry_after_value(&body, &LONG_WINDOW_SECONDS.to_string());
    }

    let (status, body) = get_id(port, "victim-new");
    assert_eq!(
        status,
        503,
        "next identity must fail closed under capacity pressure: {body}\n{}",
        error_log(&tmp)
    );

    let denied = AtomicUsize::new(0);
    let unexpected = AtomicUsize::new(0);
    thread::scope(|scope| {
        let denied = &denied;
        let unexpected = &unexpected;
        for i in 0..16 {
            let id = format!("race{i:02}");
            scope.spawn(move || {
                let (status, body) = get_id(port, &id);
                match status {
                    503 => {
                        denied.fetch_add(1, Ordering::SeqCst);
                    }
                    _ => {
                        unexpected.fetch_add(1, Ordering::SeqCst);
                        panic!(
                            "concurrent first hit under capacity must be 503, got {status}: {body}"
                        );
                    }
                }
            });
        }
    });
    assert_eq!(
        denied.load(Ordering::SeqCst),
        16,
        "concurrent first hits under capacity must all fail closed\n{}",
        error_log(&tmp)
    );
    assert_eq!(unexpected.load(Ordering::SeqCst), 0);

    for id in &live {
        let (status, body) = get_id(port, id);
        assert_eq!(
            status,
            429,
            "live {id} quota must remain exhausted (no LRU victim reset): {body}\n{}",
            error_log(&tmp)
        );
        assert_retry_after_value(&body, &LONG_WINDOW_SECONDS.to_string());
    }
}

#[test]
fn sigquit_exits_cleanly() {
    let (nginx, port, tmp, mut running) = setup_keyed("rate-limit-quit");
    let (status, body) = get_id(port, "quit");
    assert_eq!(status, 200, "warmup: {body}\n{}", error_log(&tmp));
    let start = Instant::now();
    let status = running.quit_wait();
    assert!(
        status.success(),
        "nginx -s quit (SIGQUIT) must exit cleanly: {status:?}"
    );
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "SIGQUIT took too long: {:?}",
        start.elapsed()
    );
    let refused = TcpStream::connect(("127.0.0.1", port));
    assert!(
        refused.is_err(),
        "listen port must be gone after SIGQUIT: {refused:?}"
    );
    let _ = nginx;
}
