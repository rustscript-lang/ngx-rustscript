//! Real `nginx -t` and HTTP acceptance for `log_by_rss_file`.

mod support;

use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use ngx_rustscript_nginx_test_stubs as _;
use support::artifact::module_cdylib;
use support::nginx::{combined, nginx_binary, nginx_escape_quoted, nginx_quote_path, nginx_test};
use support::temp::isolated_prefix;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn write_conf(prefix: &Path, body: &str) {
    fs::write(prefix.join("conf/nginx.conf"), body).unwrap();
}

fn write_index(prefix: &Path, rel: &str, body: &str) {
    let path = prefix.join("html").join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, body).unwrap();
}

fn write_script(path: &Path, body: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, body).unwrap();
}

struct RunningNginx {
    child: Child,
}

impl Drop for RunningNginx {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
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
    RunningNginx { child }
}

/// NGINX clears inherited env except TZ; workers only see `env NAME=value;`.
fn inject_main_env(conf: &str, vars: &[(&str, &str)]) -> String {
    let mut env_lines = String::new();
    for (key, value) in vars {
        env_lines.push_str(&format!("env {key}={value};\n"));
    }
    format!("{env_lines}{conf}")
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
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn http_get(port: u16, path: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
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
    let status = body
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    (status, body)
}

fn http_body(response: &str) -> &str {
    response
        .split("\r\n\r\n")
        .nth(1)
        .unwrap_or("")
        .trim_end_matches('\0')
}

fn t_conf(module: &Path, prefix: &Path, locations: &str) -> String {
    let module = nginx_quote_path(module).unwrap();
    let pid = nginx_quote_path(&prefix.join("logs/nginx.pid")).unwrap();
    format!(
        r#"
worker_processes 1;
error_log stderr emerg;
pid {pid};
daemon off;
master_process off;
load_module {module};

events {{
    worker_connections 32;
}}

http {{
    access_log off;
    server {{
        listen 127.0.0.1:18080;
        server_name localhost;
{locations}
    }}
}}
"#
    )
}

fn http_conf(module: &Path, prefix: &Path, port: u16, locations: &str) -> String {
    let module = nginx_quote_path(module).unwrap();
    let pid = nginx_quote_path(&prefix.join("logs/nginx.pid")).unwrap();
    let html = nginx_quote_path(&prefix.join("html")).unwrap();
    let prefix_q = nginx_escape_quoted(prefix.to_str().unwrap()).unwrap();
    format!(
        r#"
worker_processes 1;
error_log {prefix_q}/logs/error.log info;
pid {pid};
daemon off;
master_process on;
load_module {module};

events {{
    worker_connections 32;
}}

http {{
    access_log off;
    default_type text/plain;
    server {{
        listen 127.0.0.1:{port};
        server_name localhost;
        root {html};
{locations}
    }}
}}
"#
    )
}

fn http_conf_logged(module: &Path, prefix: &Path, port: u16, locations: &str) -> String {
    let module = nginx_quote_path(module).unwrap();
    let pid = nginx_quote_path(&prefix.join("logs/nginx.pid")).unwrap();
    let html = nginx_quote_path(&prefix.join("html")).unwrap();
    let prefix_q = nginx_escape_quoted(prefix.to_str().unwrap()).unwrap();
    let access_log = nginx_quote_path(&prefix.join("logs/access.log")).unwrap();
    format!(
        r#"
worker_processes 1;
error_log {prefix_q}/logs/error.log info;
pid {pid};
daemon off;
master_process on;
load_module {module};

events {{
    worker_connections 32;
}}

http {{
    access_log {access_log};
    default_type text/plain;
    server {{
        listen 127.0.0.1:{port};
        server_name localhost;
        root {html};
{locations}
    }}
}}
"#
    )
}

fn wait_log_contains(log: &Path, needle: &str, timeout: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if let Ok(text) = fs::read_to_string(log) {
            if text.contains(needle) {
                return true;
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

fn count_needle(text: &str, needle: &str) -> usize {
    text.matches(needle).count()
}

fn wait_exact_count(log: &Path, needle: &str, expected: usize, timeout: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if let Ok(text) = fs::read_to_string(log) {
            if count_needle(&text, needle) == expected {
                return true;
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

fn marker_script(marker: &str) -> String {
    format!("use ngx;\nngx::log(ngx::log_level::notice(), \"{marker}\");\n")
}

fn nginx_has_ssi(nginx: &Path) -> bool {
    let out = Command::new(nginx).arg("-V").output().expect("nginx -V");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    !text.contains("without-http_ssi_module")
}

fn http_conf_workers(
    module: &Path,
    prefix: &Path,
    port: u16,
    workers: u32,
    locations: &str,
) -> String {
    let module = nginx_quote_path(module).unwrap();
    let pid = nginx_quote_path(&prefix.join("logs/nginx.pid")).unwrap();
    let html = nginx_quote_path(&prefix.join("html")).unwrap();
    let prefix_q = nginx_escape_quoted(prefix.to_str().unwrap()).unwrap();
    format!(
        r#"
worker_processes {workers};
error_log {prefix_q}/logs/error.log info;
pid {pid};
daemon off;
master_process on;
load_module {module};

events {{
    worker_connections 32;
}}

http {{
    access_log off;
    default_type text/plain;
    server {{
        listen 127.0.0.1:{port};
        server_name localhost;
        root {html};
{locations}
    }}
}}
"#
    )
}

fn wait_access_status(log: &Path, uri: &str, status: u16, timeout: Duration) -> bool {
    let needle = format!("\"GET {uri} HTTP/1.0\" {status} ");
    wait_log_contains(log, &needle, timeout)
}

fn quit_and_wait(child: &mut Child, timeout: Duration) {
    let pid = child.id();
    let status = Command::new("kill")
        .args(["-QUIT", &pid.to_string()])
        .status()
        .expect("send SIGQUIT");
    assert!(status.success(), "kill -QUIT {pid} failed: {status:?}");
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                assert!(
                    status.success(),
                    "nginx SIGQUIT must exit successfully: {status:?}"
                );
                return;
            }
            Ok(None) => {
                if start.elapsed() >= timeout {
                    panic!("nginx master pid {pid} did not exit after SIGQUIT within {timeout:?}");
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(err) => panic!("wait nginx: {err}"),
        }
    }
}

fn log_script() -> &'static str {
    r#"
use ngx;
ngx::log(ngx::log_level::notice(), "LOG_PHASE=" + ngx::get_phase());
ngx::log(ngx::log_level::notice(), "LOG_MARK=" + ngx::encode_args({ uri: ngx::var::get("uri") }));
"#
}

#[test]
fn nginx_t_accepts_log_by_rss_file() {
    let nginx = nginx_binary();
    let module = module_cdylib();
    let prefix = isolated_prefix("log-t-ok");
    let script = nginx_quote_path(&fixture("ok.rss")).unwrap();
    write_conf(
        &prefix,
        &t_conf(
            &module,
            &prefix,
            &format!(
                r#"
        location / {{
            log_by_rss_file {script};
            return 200;
        }}
"#
            ),
        ),
    );
    let out = nginx_test(&nginx, &prefix);
    let text = combined(&out);
    assert!(
        out.status.success(),
        "log_by_rss_file must pass nginx -t:\n{text}"
    );
}

#[test]
fn nginx_t_rejects_missing_syntax_and_duplicate_log() {
    let nginx = nginx_binary();
    let module = module_cdylib();

    let prefix = isolated_prefix("log-t-missing");
    write_conf(
        &prefix,
        &t_conf(
            &module,
            &prefix,
            r#"
        location / {
            log_by_rss_file /no/such/log.rss;
            return 200;
        }
"#,
        ),
    );
    let out = nginx_test(&nginx, &prefix);
    let text = combined(&out).to_ascii_lowercase();
    assert!(
        !out.status.success(),
        "missing log script must fail nginx -t:\n{text}"
    );
    assert!(
        text.contains("script file not found") || text.contains("no such"),
        "missing log script error, got:\n{text}"
    );

    let prefix = isolated_prefix("log-t-syntax");
    let script = nginx_quote_path(&fixture("syntax_error.rss")).unwrap();
    write_conf(
        &prefix,
        &t_conf(
            &module,
            &prefix,
            &format!(
                r#"
        location / {{
            log_by_rss_file {script};
            return 200;
        }}
"#
            ),
        ),
    );
    let out = nginx_test(&nginx, &prefix);
    let text = combined(&out).to_ascii_lowercase();
    assert!(
        !out.status.success(),
        "syntax error must fail nginx -t:\n{text}"
    );
    assert!(
        text.contains("failed to compile") || text.contains("syntax"),
        "syntax error, got:\n{text}"
    );

    let prefix = isolated_prefix("log-t-dup");
    let script = nginx_quote_path(&fixture("ok.rss")).unwrap();
    write_conf(
        &prefix,
        &t_conf(
            &module,
            &prefix,
            &format!(
                r#"
        location / {{
            log_by_rss_file {script};
            log_by_rss_file {script};
            return 200;
        }}
"#
            ),
        ),
    );
    let out = nginx_test(&nginx, &prefix);
    let text = combined(&out).to_ascii_lowercase();
    assert!(
        !out.status.success(),
        "duplicate log must fail nginx -t:\n{text}"
    );
    assert!(
        text.contains("duplicate log_by_rss_file"),
        "duplicate log error, got:\n{text}"
    );
}

#[test]
fn nginx_t_rejects_sleep_udp_tcp_output_control_and_mutation() {
    let nginx = nginx_binary();
    let module = module_cdylib();
    let cases: &[(&str, &str, &str)] = &[
        ("sleep", "use ngx;\nngx::sleep_ms(1);\n", "sleep"),
        ("udp", "use ngx;\nngx::socket::udp();\n", "udp"),
        ("tcp", "use ngx;\nngx::socket::tcp();\n", "tcp"),
        ("say", "use ngx;\nngx::say(\"x\");\n", "say"),
        ("exit", "use ngx;\nngx::exit(200);\n", "exit"),
        (
            "set_header",
            "use ngx;\nngx::req::set_header(\"X-A\", \"b\");\n",
            "set_header",
        ),
        ("status_set", "use ngx;\nngx::status::set(201);\n", "status"),
    ];
    for (label, source, needle) in cases {
        let prefix = isolated_prefix(&format!("log-t-{label}"));
        let script = prefix.join("conf/forbidden.rss");
        write_script(&script, source);
        let script_q = nginx_quote_path(&script).unwrap();
        write_conf(
            &prefix,
            &t_conf(
                &module,
                &prefix,
                &format!(
                    r#"
        location / {{
            log_by_rss_file {script_q};
            return 200;
        }}
"#
                ),
            ),
        );
        let out = nginx_test(&nginx, &prefix);
        let text = combined(&out).to_ascii_lowercase();
        assert!(!out.status.success(), "{label} must fail nginx -t:\n{text}");
        assert!(
            text.contains(needle) || text.contains("not allowed") || text.contains("not supported"),
            "{label} must name the rejected host, got:\n{text}"
        );
    }
}

#[test]
fn log_script_writes_marker_after_2xx_and_4xx() {
    let nginx = nginx_binary();
    let module = module_cdylib();
    let port = free_port();
    let prefix = isolated_prefix("log-2xx-4xx");
    write_index(&prefix, "ok/index.html", "ok-body\n");
    let script = prefix.join("conf/log.rss");
    write_script(&script, log_script());
    let script_q = nginx_quote_path(&script).unwrap();
    write_conf(
        &prefix,
        &http_conf_logged(
            &module,
            &prefix,
            port,
            &format!(
                r#"
        location /ok/ {{
            log_by_rss_file {script_q};
        }}
        location /missing/ {{
            log_by_rss_file {script_q};
        }}
"#
            ),
        ),
    );
    let out = nginx_test(&nginx, &prefix);
    let text = combined(&out);
    assert!(
        out.status.success(),
        "log 2xx/4xx conf must pass nginx -t:\n{text}"
    );
    let _child = spawn_nginx(&nginx, &prefix);
    wait_for_port(port, Duration::from_secs(5));

    let (status, response) = http_get(port, "/ok/");
    assert_eq!(status, 200, "2xx path must stay 200:\n{response}");
    assert!(
        http_body(&response).contains("ok-body"),
        "log must not replace content:\n{response}"
    );

    let (status, response) = http_get(port, "/missing/no-such");
    assert_eq!(status, 404, "4xx path must stay 404:\n{response}");

    let error = prefix.join("logs/error.log");
    let access = prefix.join("logs/access.log");
    assert!(
        wait_log_contains(&error, "LOG_PHASE=log", Duration::from_secs(3)),
        "log phase marker missing:\n{}",
        fs::read_to_string(&error).unwrap_or_default()
    );
    assert!(
        wait_exact_count(&error, "uri=%2Fok%2F", 1, Duration::from_secs(3)),
        "2xx log marker must appear exactly once:\n{}",
        fs::read_to_string(&error).unwrap_or_default()
    );
    assert!(
        wait_exact_count(
            &error,
            "uri=%2Fmissing%2Fno-such",
            1,
            Duration::from_secs(3)
        ),
        "4xx log marker must appear exactly once:\n{}",
        fs::read_to_string(&error).unwrap_or_default()
    );
    assert!(
        wait_access_status(&access, "/ok/", 200, Duration::from_secs(3)),
        "access log 200 missing:\n{}",
        fs::read_to_string(&access).unwrap_or_default()
    );
    assert!(
        wait_access_status(&access, "/missing/no-such", 404, Duration::from_secs(3)),
        "access log 404 missing:\n{}",
        fs::read_to_string(&access).unwrap_or_default()
    );
}

#[test]
fn log_script_writes_marker_after_499() {
    let nginx = nginx_binary();
    let module = module_cdylib();
    let prefix = isolated_prefix("log-499");
    let port = free_port();
    let access_script = nginx_quote_path(&fixture("sleep_30s.rss")).unwrap();
    let log = prefix.join("conf/log.rss");
    write_script(&log, log_script());
    let log_q = nginx_quote_path(&log).unwrap();
    write_conf(
        &prefix,
        &http_conf_logged(
            &module,
            &prefix,
            port,
            &format!(
                r#"
        location /long/ {{
            access_by_rss_file {access_script};
            log_by_rss_file {log_q};
        }}
"#
            ),
        ),
    );
    let out = nginx_test(&nginx, &prefix);
    let text = combined(&out);
    assert!(
        out.status.success(),
        "log 499 conf must pass nginx -t:\n{text}"
    );
    let _child = spawn_nginx(&nginx, &prefix);
    wait_for_port(port, Duration::from_secs(5));

    {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        write!(
            stream,
            "GET /long/ HTTP/1.0\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
        let _ = stream.flush();
        std::thread::sleep(Duration::from_millis(200));
        drop(stream);
    }

    let access_log = prefix.join("logs/access.log");
    let error = prefix.join("logs/error.log");
    assert!(
        wait_access_status(&access_log, "/long/", 499, Duration::from_secs(5)),
        "499 must appear in access log:\n{}\nerror:\n{}",
        fs::read_to_string(&access_log).unwrap_or_default(),
        fs::read_to_string(&error).unwrap_or_default()
    );
    assert!(
        wait_exact_count(&error, "uri=%2Flong%2F", 1, Duration::from_secs(3)),
        "log handler must run exactly once after 499:\n{}",
        fs::read_to_string(&error).unwrap_or_default()
    );
}

#[test]
fn log_runtime_error_does_not_mutate_response() {
    let nginx = nginx_binary();
    let module = module_cdylib();
    let port = free_port();
    let prefix = isolated_prefix("log-500-iso");
    write_index(&prefix, "ok/index.html", "still-ok\n");
    let boom = nginx_quote_path(&fixture("runtime_error.rss")).unwrap();
    write_conf(
        &prefix,
        &http_conf(
            &module,
            &prefix,
            port,
            &format!(
                r#"
        location /ok/ {{
            log_by_rss_file {boom};
        }}
        location /plain/ {{
        }}
"#
            ),
        ),
    );
    write_index(&prefix, "plain/index.html", "plain-ok\n");
    let out = nginx_test(&nginx, &prefix);
    let text = combined(&out);
    assert!(
        out.status.success(),
        "log error conf must pass nginx -t:\n{text}"
    );
    let _child = spawn_nginx(&nginx, &prefix);
    wait_for_port(port, Duration::from_secs(5));
    let (status, response) = http_get(port, "/ok/");
    assert_eq!(
        status,
        200,
        "log runtime error must not change 200:\n{response}\n{}",
        fs::read_to_string(prefix.join("logs/error.log")).unwrap_or_default()
    );
    assert!(
        http_body(&response).contains("still-ok"),
        "body must stay static:\n{response}"
    );
    let error = fs::read_to_string(prefix.join("logs/error.log")).unwrap_or_default();
    assert!(
        error.to_ascii_lowercase().contains("log_by_rss_file")
            || error.contains("division")
            || error.contains("failed"),
        "log error must be logged, got:\n{error}"
    );
    let (status, response) = http_get(port, "/plain/");
    assert_eq!(status, 200, "later request must succeed:\n{response}");
}

#[test]
fn access_content_then_log_share_ngx_ctx() {
    let nginx = nginx_binary();
    let module = module_cdylib();
    let port = free_port();
    let prefix = isolated_prefix("log-shared-ctx");
    let access = prefix.join("conf/access.rss");
    let content = prefix.join("conf/content.rss");
    let log = prefix.join("conf/log.rss");
    write_script(
        &access,
        r#"
use ngx;
ngx::ctx::set("shared", "from-access");
ngx::log(ngx::log_level::notice(), "ACCESS_CTX=" + ngx::ctx::get("shared"));
"#,
    );
    write_script(
        &content,
        r#"
use ngx;
ngx::ctx::set("content", ngx::ctx::get("shared"));
ngx::log(ngx::log_level::notice(), "CONTENT_CTX=" + ngx::ctx::get("content"));
"#,
    );
    write_script(
        &log,
        r#"
use ngx;
ngx::log(ngx::log_level::notice(), "LOG_CTX=" + ngx::ctx::get("shared"));
ngx::log(ngx::log_level::notice(), "LOG_CONTENT_CTX=" + ngx::ctx::get("content"));
ngx::log(ngx::log_level::notice(), "LOG_PHASE=" + ngx::get_phase());
"#,
    );
    let access_q = nginx_quote_path(&access).unwrap();
    let content_q = nginx_quote_path(&content).unwrap();
    let log_q = nginx_quote_path(&log).unwrap();
    write_conf(
        &prefix,
        &http_conf(
            &module,
            &prefix,
            port,
            &format!(
                r#"
        location /both/ {{
            access_by_rss_file {access_q};
            content_by_rss_file {content_q};
            log_by_rss_file {log_q};
        }}
"#
            ),
        ),
    );
    let out = nginx_test(&nginx, &prefix);
    let text = combined(&out);
    assert!(
        out.status.success(),
        "shared ctx conf must pass nginx -t:\n{text}"
    );
    let _child = spawn_nginx(&nginx, &prefix);
    wait_for_port(port, Duration::from_secs(5));
    let (status, response) = http_get(port, "/both/");
    let log_text = fs::read_to_string(prefix.join("logs/error.log")).unwrap_or_default();
    assert_eq!(
        status, 200,
        "access+content+log must succeed:\n{response}\n{log_text}"
    );
    assert!(
        log_text.contains("ACCESS_CTX=from-access"),
        "access ctx missing:\n{log_text}"
    );
    assert!(
        log_text.contains("CONTENT_CTX=from-access"),
        "content ctx missing:\n{log_text}"
    );
    assert!(
        log_text.contains("LOG_CTX=from-access"),
        "log must read access ctx:\n{log_text}"
    );
    assert!(
        log_text.contains("LOG_CONTENT_CTX=from-access"),
        "log must read content ctx:\n{log_text}"
    );
    assert!(
        log_text.contains("LOG_PHASE=log"),
        "get_phase must be log:\n{log_text}"
    );
}

#[test]
fn concurrent_requests_log_the_matching_uri() {
    let nginx = nginx_binary();
    let module = module_cdylib();
    let port = free_port();
    let prefix = isolated_prefix("log-concurrent");
    write_index(&prefix, "a/index.html", "a\n");
    write_index(&prefix, "b/index.html", "b\n");
    let script = prefix.join("conf/log.rss");
    write_script(&script, log_script());
    let script_q = nginx_quote_path(&script).unwrap();
    write_conf(
        &prefix,
        &http_conf_workers(
            &module,
            &prefix,
            port,
            2,
            &format!(
                r#"
        location /a/ {{
            log_by_rss_file {script_q};
        }}
        location /b/ {{
            log_by_rss_file {script_q};
        }}
"#
            ),
        ),
    );
    let out = nginx_test(&nginx, &prefix);
    let text = combined(&out);
    assert!(
        out.status.success(),
        "concurrent log conf must pass nginx -t:\n{text}"
    );
    let _child = spawn_nginx(&nginx, &prefix);
    wait_for_port(port, Duration::from_secs(5));
    let a = std::thread::spawn(move || http_get(port, "/a/"));
    let b = std::thread::spawn(move || http_get(port, "/b/"));
    let (status_a, resp_a) = a.join().unwrap();
    let (status_b, resp_b) = b.join().unwrap();
    assert_eq!(status_a, 200, "{resp_a}");
    assert_eq!(status_b, 200, "{resp_b}");
    let error = prefix.join("logs/error.log");
    assert!(
        wait_log_contains(&error, "uri=%2Fa%2F", Duration::from_secs(3)),
        "missing /a/ marker:\n{}",
        fs::read_to_string(&error).unwrap_or_default()
    );
    assert!(
        wait_log_contains(&error, "uri=%2Fb%2F", Duration::from_secs(3)),
        "missing /b/ marker:\n{}",
        fs::read_to_string(&error).unwrap_or_default()
    );
}

#[test]
fn inherited_log_script_runs_in_child_location() {
    let nginx = nginx_binary();
    let module = module_cdylib();
    let port = free_port();
    let prefix = isolated_prefix("log-inherit");
    write_index(&prefix, "parent/child/index.html", "child\n");
    let script = prefix.join("conf/log.rss");
    write_script(&script, log_script());
    let script_q = nginx_quote_path(&script).unwrap();
    write_conf(
        &prefix,
        &http_conf(
            &module,
            &prefix,
            port,
            &format!(
                r#"
        location /parent/ {{
            log_by_rss_file {script_q};
            location /parent/child/ {{
            }}
        }}
"#
            ),
        ),
    );
    let out = nginx_test(&nginx, &prefix);
    let text = combined(&out);
    assert!(
        out.status.success(),
        "inherited log conf must pass nginx -t:\n{text}"
    );
    let _child = spawn_nginx(&nginx, &prefix);
    wait_for_port(port, Duration::from_secs(5));
    let (status, response) = http_get(port, "/parent/child/");
    assert_eq!(status, 200, "{response}");
    let error = prefix.join("logs/error.log");
    assert!(
        wait_log_contains(&error, "uri=%2Fparent%2Fchild%2F", Duration::from_secs(3)),
        "child must inherit log script:\n{}",
        fs::read_to_string(&error).unwrap_or_default()
    );
}

#[test]
fn log_reload_and_sigquit_do_not_wait_or_leak() {
    let nginx = nginx_binary();
    let module = module_cdylib();
    let port = free_port();
    let prefix = isolated_prefix("log-reload-quit");
    write_index(&prefix, "ok/index.html", "ok\n");
    let script = prefix.join("conf/log.rss");
    write_script(&script, log_script());
    let script_q = nginx_quote_path(&script).unwrap();
    write_conf(
        &prefix,
        &http_conf(
            &module,
            &prefix,
            port,
            &format!(
                r#"
        location /ok/ {{
            log_by_rss_file {script_q};
        }}
"#
            ),
        ),
    );
    let out = nginx_test(&nginx, &prefix);
    let text = combined(&out);
    assert!(
        out.status.success(),
        "reload conf must pass nginx -t:\n{text}"
    );
    let mut running = spawn_nginx(&nginx, &prefix);
    wait_for_port(port, Duration::from_secs(5));
    let (status, response) = http_get(port, "/ok/");
    assert_eq!(status, 200, "{response}");
    let error = prefix.join("logs/error.log");
    assert!(
        wait_log_contains(&error, "LOG_PHASE=log", Duration::from_secs(3)),
        "log handler must run before reload:\n{}",
        fs::read_to_string(&error).unwrap_or_default()
    );
    let reload = Command::new(&nginx)
        .arg("-s")
        .arg("reload")
        .arg("-p")
        .arg(prefix.as_ref())
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
    let (status, response) = http_get(port, "/ok/");
    assert_eq!(status, 200, "after reload:\n{response}");
    assert!(
        wait_log_contains(&error, "LOG_PHASE=log", Duration::from_secs(3)),
        "log marker after reload missing:\n{}",
        fs::read_to_string(&error).unwrap_or_default()
    );
    let start = Instant::now();
    quit_and_wait(&mut running.child, Duration::from_secs(5));
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "SIGQUIT with log handler must not wait, elapsed {:?}",
        start.elapsed()
    );
    let error_text = fs::read_to_string(&error).unwrap_or_default();
    assert!(
        !error_text.contains("LOG_WAIT_PROOF"),
        "ordinary module must not emit wait-proof markers:\n{error_text}"
    );
    let follow = spawn_nginx(&nginx, &prefix);
    wait_for_port(port, Duration::from_secs(5));
    let (status, response) = http_get(port, "/ok/");
    assert_eq!(status, 200, "healthy follow-up after SIGQUIT:\n{response}");
    drop(follow);
}

#[test]
fn existing_access_and_content_stay_untouched() {
    let nginx = nginx_binary();
    let module = module_cdylib();
    let port = free_port();
    let prefix = isolated_prefix("log-untouched");
    write_index(&prefix, "plain/index.html", "plain-ok\n");
    let access = prefix.join("conf/access.rss");
    let content = prefix.join("conf/content.rss");
    let log = prefix.join("conf/log.rss");
    write_script(
        &access,
        r#"
use ngx;
ngx::log(ngx::log_level::notice(), "STILL_ACCESS=" + ngx::get_phase());
"#,
    );
    write_script(
        &content,
        r#"
use ngx;
ngx::log(ngx::log_level::notice(), "STILL_CONTENT=" + ngx::get_phase());
"#,
    );
    write_script(&log, log_script());
    let access_q = nginx_quote_path(&access).unwrap();
    let content_q = nginx_quote_path(&content).unwrap();
    let log_q = nginx_quote_path(&log).unwrap();
    write_conf(
        &prefix,
        &http_conf(
            &module,
            &prefix,
            port,
            &format!(
                r#"
        location /scripted/ {{
            access_by_rss_file {access_q};
            content_by_rss_file {content_q};
            log_by_rss_file {log_q};
        }}
        location /plain/ {{
        }}
"#
            ),
        ),
    );
    let out = nginx_test(&nginx, &prefix);
    let text = combined(&out);
    assert!(
        out.status.success(),
        "untouched conf must pass nginx -t:\n{text}"
    );
    let _child = spawn_nginx(&nginx, &prefix);
    wait_for_port(port, Duration::from_secs(5));
    let (status, response) = http_get(port, "/scripted/");
    assert_eq!(status, 200, "{response}");
    let (status, response) = http_get(port, "/plain/");
    assert_eq!(status, 200, "{response}");
    assert!(
        http_body(&response).contains("plain-ok"),
        "plain location must stay static:\n{response}"
    );
    let log_text = fs::read_to_string(prefix.join("logs/error.log")).unwrap_or_default();
    assert!(
        log_text.contains("STILL_ACCESS=access"),
        "access handler must still run:\n{log_text}"
    );
    assert!(
        log_text.contains("STILL_CONTENT=content"),
        "content handler must still run:\n{log_text}"
    );
    assert!(
        log_text.contains("LOG_PHASE=log"),
        "log handler must run:\n{log_text}"
    );
}

#[test]
fn log_uses_current_loc_conf_after_internal_redirect_and_error_page() {
    let nginx = nginx_binary();
    let module = module_cdylib();
    let port = free_port();
    let prefix = isolated_prefix("log-loc-conf");
    let from = prefix.join("conf/from.rss");
    let dest = prefix.join("conf/dest.rss");
    let err = prefix.join("conf/err.rss");
    let handler = prefix.join("conf/handler.rss");
    write_script(&from, &marker_script("LOG_FROM"));
    write_script(&dest, &marker_script("LOG_DEST"));
    write_script(&err, &marker_script("LOG_ERR"));
    write_script(&handler, &marker_script("LOG_HANDLER"));
    let from_q = nginx_quote_path(&from).unwrap();
    let dest_q = nginx_quote_path(&dest).unwrap();
    let err_q = nginx_quote_path(&err).unwrap();
    let handler_q = nginx_quote_path(&handler).unwrap();
    write_conf(
        &prefix,
        &http_conf(
            &module,
            &prefix,
            port,
            &format!(
                r#"
        location /from {{
            log_by_rss_file {from_q};
            rewrite ^ /to last;
        }}
        location /to {{
            log_by_rss_file {dest_q};
            return 200 dest;
        }}
        location /err {{
            log_by_rss_file {err_q};
            error_page 500 = /handler;
            return 500;
        }}
        location /handler {{
            internal;
            log_by_rss_file {handler_q};
            return 200 handled;
        }}
"#
            ),
        ),
    );
    let out = nginx_test(&nginx, &prefix);
    let text = combined(&out);
    assert!(
        out.status.success(),
        "loc_conf conf must pass nginx -t:\n{text}"
    );
    let _child = spawn_nginx(&nginx, &prefix);
    wait_for_port(port, Duration::from_secs(5));
    let (status, response) = http_get(port, "/from");
    assert_eq!(status, 200, "internal redirect:\n{response}");
    let (status, response) = http_get(port, "/err");
    assert_eq!(status, 200, "error_page:\n{response}");
    let error = prefix.join("logs/error.log");
    assert!(
        wait_log_contains(&error, "LOG_DEST", Duration::from_secs(3)),
        "rewrite must use dest loc_conf:\n{}",
        fs::read_to_string(&error).unwrap_or_default()
    );
    assert!(
        wait_log_contains(&error, "LOG_HANDLER", Duration::from_secs(3)),
        "error_page must use handler loc_conf:\n{}",
        fs::read_to_string(&error).unwrap_or_default()
    );
    let error_text = fs::read_to_string(&error).unwrap_or_default();
    assert!(
        !error_text.contains("LOG_FROM"),
        "source loc_conf must not run after rewrite:\n{error_text}"
    );
    assert!(
        !error_text.contains("LOG_ERR"),
        "source loc_conf must not run after error_page:\n{error_text}"
    );
}

#[test]
fn log_proof_env_is_inert_on_ordinary_module() {
    let nginx = nginx_binary();
    let module = module_cdylib();
    let port = free_port();
    let prefix = isolated_prefix("log-proof-inert");
    write_index(&prefix, "ok/index.html", "ok-body\n");
    let script = prefix.join("conf/log.rss");
    write_script(&script, log_script());
    let script_q = nginx_quote_path(&script).unwrap();
    write_conf(
        &prefix,
        &inject_main_env(
            &http_conf(
                &module,
                &prefix,
                port,
                &format!(
                    r#"
        location /ok/ {{
            log_by_rss_file {script_q};
        }}
        location /follow/ {{
        }}
"#
                ),
            ),
            &[
                ("NGX_RUSTSCRIPT_TEST_LOG_PANIC", "1"),
                ("NGX_RUSTSCRIPT_LOG_WAIT_PROOF", "1"),
            ],
        ),
    );
    write_index(&prefix, "follow/index.html", "follow-ok\n");
    let out = nginx_test(&nginx, &prefix);
    let text = combined(&out);
    assert!(
        out.status.success(),
        "inert-env conf must pass nginx -t:\n{text}"
    );
    let _child = spawn_nginx(&nginx, &prefix);
    wait_for_port(port, Duration::from_secs(5));
    let (status, response) = http_get(port, "/ok/");
    assert_eq!(
        status, 200,
        "ordinary module must ignore proof env and keep 200:\n{response}"
    );
    assert!(
        http_body(&response).contains("ok-body"),
        "body must stay static:\n{response}"
    );
    let error = prefix.join("logs/error.log");
    assert!(
        wait_log_contains(&error, "LOG_PHASE=log", Duration::from_secs(3)),
        "log handler must still run:\n{}",
        fs::read_to_string(&error).unwrap_or_default()
    );
    let error_text = fs::read_to_string(&error).unwrap_or_default();
    assert!(
        !error_text.contains("log_by_rss_file: panic"),
        "runtime panic env must do nothing without compile-time hooks:\n{error_text}"
    );
    assert!(
        !error_text.contains("LOG_WAIT_PROOF"),
        "runtime wait-proof env must do nothing without compile-time hooks:\n{error_text}"
    );
    let (status, response) = http_get(port, "/follow/");
    assert_eq!(status, 200, "follow-up request must succeed:\n{response}");
    assert!(
        http_body(&response).contains("follow-ok"),
        "follow-up body:\n{response}"
    );
}

#[test]
fn log_subrequest_requires_core_log_subrequest_on() {
    let nginx = nginx_binary();
    if !nginx_has_ssi(&nginx) {
        eprintln!(
            "pinned nginx was built without SSI; log_subrequest semantics are documented in README. Proving current loc_conf via rewrite/error_page instead."
        );
        return;
    }
    let module = module_cdylib();
    let port = free_port();
    let prefix = isolated_prefix("log-subrequest");
    write_index(
        &prefix,
        "ssi.html",
        "<!--# include virtual=\"/ssi-sub\" -->\n",
    );
    write_index(
        &prefix,
        "ssi-on.html",
        "<!--# include virtual=\"/ssi-sub-on\" -->\n",
    );
    write_index(&prefix, "ssi-sub", "sub\n");
    write_index(&prefix, "ssi-sub-on", "sub-on\n");
    let sub = prefix.join("conf/sub.rss");
    write_script(&sub, &marker_script("LOG_SUB"));
    let sub_q = nginx_quote_path(&sub).unwrap();
    write_conf(
        &prefix,
        &http_conf(
            &module,
            &prefix,
            port,
            &format!(
                r#"
        location = /ssi.html {{
            ssi on;
            default_type text/html;
        }}
        location = /ssi-on.html {{
            ssi on;
            default_type text/html;
        }}
        location = /ssi-sub {{
            internal;
            log_subrequest off;
            log_by_rss_file {sub_q};
        }}
        location = /ssi-sub-on {{
            internal;
            log_subrequest on;
            log_by_rss_file {sub_q};
        }}
"#
            ),
        ),
    );
    let out = nginx_test(&nginx, &prefix);
    let text = combined(&out);
    assert!(
        out.status.success(),
        "ssi log_subrequest conf must pass nginx -t:\n{text}"
    );
    let _child = spawn_nginx(&nginx, &prefix);
    wait_for_port(port, Duration::from_secs(5));
    let (status, response) = http_get(port, "/ssi.html");
    assert_eq!(status, 200, "ssi off:\n{response}");
    assert!(
        http_body(&response).contains("sub"),
        "SSI include must run; body:\n{response}"
    );
    std::thread::sleep(Duration::from_millis(200));
    let error = prefix.join("logs/error.log");
    let off_text = fs::read_to_string(&error).unwrap_or_default();
    assert!(
        !off_text.contains("LOG_SUB"),
        "subrequest log must not run without log_subrequest on:\n{off_text}"
    );
    let (status, response) = http_get(port, "/ssi-on.html");
    assert_eq!(status, 200, "ssi on:\n{response}");
    assert!(
        http_body(&response).contains("sub-on"),
        "SSI include with log_subrequest on must run; body:\n{response}"
    );
    assert!(
        wait_log_contains(&error, "LOG_SUB", Duration::from_secs(3)),
        "subrequest log must run with log_subrequest on:\n{}",
        fs::read_to_string(&error).unwrap_or_default()
    );
}

#[test]
fn concurrent_two_worker_log_maps_requests() {
    // Log is synchronous on the worker main thread, so one worker cannot
    // overlap two VMs. Two workers can; distinct markers prove mapping.
    let nginx = nginx_binary();
    let module = module_cdylib();
    let port = free_port();
    let prefix = isolated_prefix("log-two-workers-map");
    write_index(&prefix, "a/index.html", "a\n");
    write_index(&prefix, "b/index.html", "b\n");
    let a_script = prefix.join("conf/a.rss");
    let b_script = prefix.join("conf/b.rss");
    write_script(&a_script, &marker_script("LOG_WORKER_A"));
    write_script(&b_script, &marker_script("LOG_WORKER_B"));
    let a_q = nginx_quote_path(&a_script).unwrap();
    let b_q = nginx_quote_path(&b_script).unwrap();
    write_conf(
        &prefix,
        &http_conf_workers(
            &module,
            &prefix,
            port,
            2,
            &format!(
                r#"
        location /a/ {{
            log_by_rss_file {a_q};
        }}
        location /b/ {{
            log_by_rss_file {b_q};
        }}
"#
            ),
        ),
    );
    let out = nginx_test(&nginx, &prefix);
    let text = combined(&out);
    assert!(
        out.status.success(),
        "two-worker conf must pass nginx -t:\n{text}"
    );
    let _child = spawn_nginx(&nginx, &prefix);
    wait_for_port(port, Duration::from_secs(5));
    let a = std::thread::spawn(move || http_get(port, "/a/"));
    let b = std::thread::spawn(move || http_get(port, "/b/"));
    let (status_a, resp_a) = a.join().unwrap();
    let (status_b, resp_b) = b.join().unwrap();
    assert_eq!(status_a, 200, "{resp_a}");
    assert_eq!(status_b, 200, "{resp_b}");
    let error = prefix.join("logs/error.log");
    assert!(
        wait_log_contains(&error, "LOG_WORKER_A", Duration::from_secs(3)),
        "missing A marker:\n{}",
        fs::read_to_string(&error).unwrap_or_default()
    );
    assert!(
        wait_log_contains(&error, "LOG_WORKER_B", Duration::from_secs(3)),
        "missing B marker:\n{}",
        fs::read_to_string(&error).unwrap_or_default()
    );
    let error_text = fs::read_to_string(&error).unwrap_or_default();
    assert_eq!(count_needle(&error_text, "LOG_WORKER_A"), 1);
    assert_eq!(count_needle(&error_text, "LOG_WORKER_B"), 1);
}
