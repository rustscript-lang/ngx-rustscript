//! Real `nginx -t` and HTTP acceptance for `content_by_rss_file`.

mod support;

use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use ngx_rustscript_nginx_test_stubs as _;
use support::artifact::module_cdylib;
use support::nginx::{
    combined, nginx_binary, nginx_escape_quoted, nginx_process_group_command, nginx_quote_path,
    nginx_test, terminate_process_group,
};
use support::temp::isolated_prefix;

use ngx_rustscript::config::{ConfigError, compile_rss_source_for_phase};
use ngx_rustscript::host::{NgxPhase, ngx_host_manifest};

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
        terminate_process_group(&mut self.child, Duration::from_secs(2));
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
    let child = nginx_process_group_command(nginx)
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

fn wait_for_port(port: u16, timeout: Duration) {
    support::nginx::wait_for_worker_http(port, timeout);
}

fn http_get(port: u16, path: &str) -> (u16, String) {
    try_http_get(port, path, Duration::from_secs(5)).unwrap_or_else(|err| panic!("{err}"))
}

fn try_http_get(port: u16, path: &str, timeout: Duration) -> Result<(u16, String), String> {
    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .map_err(|err| format!("connect 127.0.0.1:{port}: {err}"))?;
    stream
        .set_read_timeout(Some(timeout))
        .map_err(|err| format!("read timeout: {err}"))?;
    stream
        .set_write_timeout(Some(timeout))
        .map_err(|err| format!("write timeout: {err}"))?;
    write!(
        stream,
        "GET {path} HTTP/1.0\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
    )
    .map_err(|err| format!("write: {err}"))?;
    let _ = stream.flush();
    let mut body = String::new();
    stream
        .read_to_string(&mut body)
        .map_err(|err| format!("read: {err}"))?;
    let status = body
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    Ok((status, body))
}

fn wait_for_http_ok(port: u16, path: &str, timeout: Duration) {
    let start = Instant::now();
    loop {
        match try_http_get(port, path, Duration::from_secs(2)) {
            Ok((200, _)) => return,
            other => {
                let last = match other {
                    Ok((status, body)) => format!("HTTP {status}: {body}"),
                    Err(err) => err,
                };
                if start.elapsed() >= timeout {
                    panic!("nginx worker did not serve {path} within {timeout:?}: {last}");
                }
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
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

fn wait_access_status(log: &Path, uri: &str, status: u16, timeout: Duration) -> bool {
    let needle = format!("\"GET {uri} HTTP/1.0\" {status} ");
    let start = Instant::now();
    while start.elapsed() < timeout {
        if let Ok(text) = fs::read_to_string(log) {
            if text.contains(&needle) {
                return true;
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
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

fn first_content_phase_forbidden_script() -> Option<(String, String)> {
    for entry in ngx_host_manifest() {
        if entry.phases.contains(&NgxPhase::Content) {
            continue;
        }
        let sources = [
            format!("use ngx;\n{}();\n", entry.name),
            format!("use ngx;\n{}(0);\n", entry.name),
            format!("use ngx;\n{}(1);\n", entry.name),
            format!("use ngx;\n{}(0.001);\n", entry.name),
            format!("use ngx;\n{}(\"k\");\n", entry.name),
            format!("use ngx;\n{}(\"k\", 1);\n", entry.name),
            format!("use ngx;\n{}(\"k\", \"v\");\n", entry.name),
        ];
        for source in sources {
            match compile_rss_source_for_phase(&source, NgxPhase::Content) {
                Err(ConfigError::PhaseForbidden { name, phase })
                    if name == entry.name && phase == NgxPhase::Content =>
                {
                    return Some((entry.name.to_string(), source));
                }
                _ => {}
            }
        }
    }
    None
}

#[test]
fn nginx_t_accepts_content_by_rss_file() {
    let nginx = nginx_binary();
    let module = module_cdylib();
    let prefix = isolated_prefix("content-t-ok");
    let script = nginx_quote_path(&fixture("ok.rss")).unwrap();
    write_conf(
        &prefix,
        &t_conf(
            &module,
            &prefix,
            &format!(
                r#"
        location / {{
            content_by_rss_file {script};
        }}
"#
            ),
        ),
    );
    let out = nginx_test(&nginx, &prefix);
    let text = combined(&out);
    assert!(
        out.status.success(),
        "content_by_rss_file must pass nginx -t:\n{text}"
    );
}

#[test]
fn nginx_t_rejects_missing_syntax_print_and_duplicate_content() {
    let nginx = nginx_binary();
    let module = module_cdylib();

    let prefix = isolated_prefix("content-t-missing");
    write_conf(
        &prefix,
        &t_conf(
            &module,
            &prefix,
            r#"
        location / {
            content_by_rss_file /no/such/content.rss;
        }
"#,
        ),
    );
    let out = nginx_test(&nginx, &prefix);
    let text = combined(&out).to_ascii_lowercase();
    assert!(
        !out.status.success(),
        "missing content script must fail nginx -t:\n{text}"
    );
    assert!(
        text.contains("script file not found") || text.contains("no such"),
        "missing content script error, got:\n{text}"
    );

    let prefix = isolated_prefix("content-t-syntax");
    let script = nginx_quote_path(&fixture("syntax_error.rss")).unwrap();
    write_conf(
        &prefix,
        &t_conf(
            &module,
            &prefix,
            &format!(
                r#"
        location / {{
            content_by_rss_file {script};
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

    let prefix = isolated_prefix("content-t-print");
    let script = nginx_quote_path(&fixture("print.rss")).unwrap();
    write_conf(
        &prefix,
        &t_conf(
            &module,
            &prefix,
            &format!(
                r#"
        location / {{
            content_by_rss_file {script};
        }}
"#
            ),
        ),
    );
    let out = nginx_test(&nginx, &prefix);
    let text = combined(&out).to_ascii_lowercase();
    assert!(!out.status.success(), "print must fail nginx -t:\n{text}");
    assert!(
        text.contains("print") || text.contains("not supported"),
        "print import error, got:\n{text}"
    );

    let prefix = isolated_prefix("content-t-dup");
    let script = nginx_quote_path(&fixture("ok.rss")).unwrap();
    write_conf(
        &prefix,
        &t_conf(
            &module,
            &prefix,
            &format!(
                r#"
        location / {{
            content_by_rss_file {script};
            content_by_rss_file {script};
        }}
"#
            ),
        ),
    );
    let out = nginx_test(&nginx, &prefix);
    let text = combined(&out).to_ascii_lowercase();
    assert!(
        !out.status.success(),
        "duplicate content must fail nginx -t:\n{text}"
    );
    assert!(
        text.contains("duplicate content_by_rss_file"),
        "duplicate content error, got:\n{text}"
    );
}

#[test]
fn nginx_t_rejects_phase_forbidden_import_via_content_by_rss_file() {
    let Some((name, source)) = first_content_phase_forbidden_script() else {
        // Catalog currently allows every host in Content. Lock that, and prove
        // content_by_rss_file still uses the Content compile path (get_method
        // is forbidden in Init, allowed in Content).
        let src = "use ngx;\nngx::req::get_method();\n";
        compile_rss_source_for_phase(src, NgxPhase::Content)
            .expect("ngx::req::get_method is allowed in content");
        match compile_rss_source_for_phase(src, NgxPhase::Init) {
            Err(ngx_rustscript::config::ConfigError::PhaseForbidden { name, phase }) => {
                assert_eq!(name, "ngx::req::get_method");
                assert_eq!(phase, NgxPhase::Init);
            }
            other => panic!("expected PhaseForbidden, got {other:?}"),
        }
        let nginx = nginx_binary();
        let module = module_cdylib();
        let prefix = isolated_prefix("content-t-phase-allowed");
        let script = prefix.join("conf/get_method.rss");
        write_script(&script, src);
        let script_q = nginx_quote_path(&script).unwrap();
        write_conf(
            &prefix,
            &t_conf(
                &module,
                &prefix,
                &format!(
                    r#"
        location / {{
            content_by_rss_file {script_q};
        }}
"#
                ),
            ),
        );
        let out = nginx_test(&nginx, &prefix);
        let text = combined(&out);
        assert!(
            out.status.success(),
            "content_by_rss_file must compile get_method as Content, not Init:\n{text}"
        );
        return;
    };
    let nginx = nginx_binary();
    let module = module_cdylib();
    let prefix = isolated_prefix("content-t-phase-forbidden");
    let script = prefix.join("conf/forbidden.rss");
    write_script(&script, &source);
    let script_q = nginx_quote_path(&script).unwrap();
    write_conf(
        &prefix,
        &t_conf(
            &module,
            &prefix,
            &format!(
                r#"
        location / {{
            content_by_rss_file {script_q};
        }}
"#
            ),
        ),
    );
    let out = nginx_test(&nginx, &prefix);
    let text = combined(&out);
    assert!(
        !out.status.success(),
        "phase-forbidden content import {name} must fail nginx -t:\n{text}"
    );
    let lower = text.to_ascii_lowercase();
    assert!(
        lower.contains("not allowed") && lower.contains("content"),
        "nginx -t must report PhaseForbidden for {name} in content, got:\n{text}"
    );
}

#[test]
fn content_script_runs_and_get_phase_is_content() {
    let nginx = nginx_binary();
    let module = module_cdylib();
    let port = free_port();
    let prefix = isolated_prefix("content-phase");
    write_index(&prefix, "phase/index.html", "static-should-not-win\n");
    let script = prefix.join("conf/phase.rss");
    write_script(
        &script,
        r#"
use ngx;
ngx::log(ngx::log_level::notice(), "CONTENT_PHASE=" + ngx::get_phase());
"#,
    );
    let script_q = nginx_quote_path(&script).unwrap();
    write_conf(
        &prefix,
        &http_conf(
            &module,
            &prefix,
            port,
            &format!(
                r#"
        location /phase/ {{
            content_by_rss_file {script_q};
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
        "content phase conf must pass nginx -t:\n{text}"
    );
    let _child = spawn_nginx(&nginx, &prefix);
    wait_for_port(port, Duration::from_secs(5));
    let (status, response) = http_get(port, "/phase/");
    assert_eq!(
        status, 200,
        "content script must produce HTTP 200:\n{response}"
    );
    assert!(
        !http_body(&response).contains("static-should-not-win"),
        "content handler must replace static output:\n{response}"
    );
    let log = fs::read_to_string(prefix.join("logs/error.log")).unwrap_or_default();
    assert!(
        log.contains("CONTENT_PHASE=content"),
        "ngx::get_phase must be content; got:\n{log}"
    );

    let (status, response) = http_get(port, "/plain/");
    assert_eq!(
        status, 200,
        "unconfigured location must still serve static:\n{response}"
    );
    assert!(
        http_body(&response).contains("plain-ok"),
        "static body missing:\n{response}"
    );
}

#[test]
fn access_then_content_run_on_the_same_request() {
    let nginx = nginx_binary();
    let module = module_cdylib();
    let port = free_port();
    let prefix = isolated_prefix("content-both");
    let access = prefix.join("conf/access.rss");
    let content = prefix.join("conf/content.rss");
    write_script(
        &access,
        r#"
use ngx;
ngx::ctx::set("shared", "from-access");
ngx::log(ngx::log_level::notice(), "ACCESS_CTX=" + ngx::ctx::get("shared"));
ngx::sleep_ms(20);
ngx::log(ngx::log_level::notice(), "BOTH_ACCESS=" + ngx::get_phase());
"#,
    );
    write_script(
        &content,
        r#"
use ngx;
ngx::log(ngx::log_level::notice(), "CONTENT_START=" + ngx::get_phase());
ngx::sleep_ms(20);
if ngx::ctx::get("shared") == "from-access" {
    ngx::log(ngx::log_level::notice(), "CONTENT_CTX=from-access");
} else {
    ngx::log(ngx::log_level::notice(), "CONTENT_CTX=missing");
}
ngx::log(ngx::log_level::notice(), "BOTH_CONTENT=" + ngx::get_phase());
"#,
    );
    let access_q = nginx_quote_path(&access).unwrap();
    let content_q = nginx_quote_path(&content).unwrap();
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
        }}
"#
            ),
        ),
    );
    let out = nginx_test(&nginx, &prefix);
    let text = combined(&out);
    assert!(
        out.status.success(),
        "access+content conf must pass nginx -t:\n{text}"
    );
    let _child = spawn_nginx(&nginx, &prefix);
    wait_for_port(port, Duration::from_secs(5));
    let started = Instant::now();
    let (status, response) = http_get(port, "/both/");
    let log = fs::read_to_string(prefix.join("logs/error.log")).unwrap_or_default();
    assert_eq!(
        status, 200,
        "access then content must succeed:\n{response}\nerror.log:\n{log}"
    );
    assert!(
        started.elapsed() >= Duration::from_millis(30),
        "both phases must wait independently: {:?}",
        started.elapsed()
    );
    let log = fs::read_to_string(prefix.join("logs/error.log")).unwrap_or_default();
    assert!(
        log.contains("BOTH_ACCESS=access"),
        "access script must run first; got:\n{log}"
    );
    assert!(
        log.contains("CONTENT_CTX=from-access"),
        "content must read the same ngx.ctx; got:\n{log}"
    );
    assert!(
        log.contains("BOTH_CONTENT=content"),
        "content script must run after access; got:\n{log}"
    );
}

#[test]
fn content_sleep_does_not_block_a_concurrent_static_request() {
    let nginx = nginx_binary();
    let module = module_cdylib();
    let port = free_port();
    let prefix = isolated_prefix("content-sleep");
    write_index(&prefix, "plain/index.html", "plain-ok\n");
    let script = nginx_quote_path(&fixture("sleep_80ms.rss")).unwrap();
    write_conf(
        &prefix,
        &http_conf(
            &module,
            &prefix,
            port,
            &format!(
                r#"
        location /sleep/ {{
            content_by_rss_file {script};
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
        "content sleep conf must pass nginx -t:\n{text}"
    );
    let _child = spawn_nginx(&nginx, &prefix);
    wait_for_port(port, Duration::from_secs(5));
    // Master can listen before `init_process` finishes installing the host
    // registry. Probe static content so the concurrency measurement starts
    // after the worker is in its event loop, without warming the sleep script.
    wait_for_http_ok(port, "/plain/", Duration::from_secs(15));

    let slow = std::thread::spawn(move || {
        let start = Instant::now();
        let result = http_get(port, "/sleep/");
        (start.elapsed(), result)
    });
    std::thread::sleep(Duration::from_millis(20));
    let plain_start = Instant::now();
    let (plain_status, plain_response) = http_get(port, "/plain/");
    let plain_elapsed = plain_start.elapsed();
    assert_eq!(
        plain_status, 200,
        "plain must succeed during content sleep:\n{plain_response}"
    );
    assert!(
        http_body(&plain_response).contains("plain-ok"),
        "plain body missing:\n{plain_response}"
    );
    assert!(
        plain_elapsed < Duration::from_millis(60),
        "plain was blocked by content sleep: {plain_elapsed:?}"
    );
    let (slow_elapsed, (slow_status, slow_response)) = slow.join().unwrap();
    assert_eq!(
        slow_status, 200,
        "content sleep must finish 200:\n{slow_response}"
    );
    assert!(
        slow_elapsed >= Duration::from_millis(50),
        "content sleep returned too quickly: {slow_elapsed:?}"
    );
}

#[test]
fn inherited_content_script_runs_in_child_location() {
    let nginx = nginx_binary();
    let module = module_cdylib();
    let port = free_port();
    let prefix = isolated_prefix("content-inherit");
    let script = prefix.join("conf/inherit.rss");
    write_script(
        &script,
        r#"
use ngx;
ngx::log(ngx::log_level::notice(), "CONTENT_INHERIT=" + ngx::get_phase());
"#,
    );
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
            content_by_rss_file {script_q};
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
        "inherited content conf must pass nginx -t:\n{text}"
    );
    let _child = spawn_nginx(&nginx, &prefix);
    wait_for_port(port, Duration::from_secs(5));
    let (status, response) = http_get(port, "/parent/child/");
    assert_eq!(status, 200, "inherited content must run:\n{response}");
    let log = fs::read_to_string(prefix.join("logs/error.log")).unwrap_or_default();
    assert!(
        log.contains("CONTENT_INHERIT=content"),
        "child location must inherit content script; got:\n{log}"
    );
}

#[test]
fn content_runtime_error_returns_500() {
    let nginx = nginx_binary();
    let module = module_cdylib();
    let port = free_port();
    let prefix = isolated_prefix("content-500");
    let script = nginx_quote_path(&fixture("runtime_error.rss")).unwrap();
    write_conf(
        &prefix,
        &http_conf(
            &module,
            &prefix,
            port,
            &format!(
                r#"
        location /boom/ {{
            content_by_rss_file {script};
        }}
"#
            ),
        ),
    );
    let out = nginx_test(&nginx, &prefix);
    let text = combined(&out);
    assert!(
        out.status.success(),
        "runtime-error content conf must pass nginx -t:\n{text}"
    );
    let _child = spawn_nginx(&nginx, &prefix);
    wait_for_port(port, Duration::from_secs(5));
    let (status, response) = http_get(port, "/boom/");
    assert_eq!(
        status, 500,
        "content runtime error must be 500:\\n{response}"
    );
}

#[test]
fn content_disconnect_during_long_sleep_logs_499_before_timer() {
    let nginx = nginx_binary();
    let module = module_cdylib();
    let prefix = isolated_prefix("content-disconnect");
    let port = free_port();
    write_index(&prefix, "long/index.html", "long\n");
    write_index(&prefix, "plain/index.html", "plain\n");
    let script = nginx_quote_path(&fixture("sleep_30s.rss")).unwrap();
    write_conf(
        &prefix,
        &http_conf_logged(
            &module,
            &prefix,
            port,
            &format!(
                r#"
        location /long/ {{
            content_by_rss_file {script};
        }}
        location /plain/ {{
        }}
"#
            ),
        ),
    );
    let out = nginx_test(&nginx, &prefix);
    let text = combined(&out);
    assert!(out.status.success(), "nginx -t failed:\\n{text}");

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
    let started = Instant::now();
    assert!(
        wait_access_status(&access_log, "/long/", 499, Duration::from_secs(5)),
        "client close during content sleep must produce access-log 499 well under the 30s sleep; access:\\n{}\\nerror:\\n{}",
        fs::read_to_string(&access_log).unwrap_or_default(),
        fs::read_to_string(prefix.join("logs/error.log")).unwrap_or_default()
    );
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "499 must arrive before the 30s timer, elapsed {:?}",
        started.elapsed()
    );

    let (status, response) = http_get(port, "/plain/");
    assert_eq!(status, 200, "{response}");
    assert!(
        http_body(&response).contains("plain"),
        "worker must stay alive after content client disconnect; got:\\n{response}"
    );
}

#[test]
fn content_graceful_quit_with_long_sleep_exits_within_bound() {
    let nginx = nginx_binary();
    let module = module_cdylib();
    let prefix = isolated_prefix("content-sigquit");
    let port = free_port();
    write_index(&prefix, "long/index.html", "long\n");
    let script = nginx_quote_path(&fixture("sleep_30s.rss")).unwrap();
    write_conf(
        &prefix,
        &http_conf(
            &module,
            &prefix,
            port,
            &format!(
                r#"
        location /long/ {{
            content_by_rss_file {script};
        }}
"#
            ),
        ),
    );
    let out = nginx_test(&nginx, &prefix);
    let text = combined(&out);
    assert!(out.status.success(), "nginx -t failed:\\n{text}");

    let mut running = spawn_nginx(&nginx, &prefix);
    wait_for_port(port, Duration::from_secs(5));

    let handle = std::thread::spawn(move || {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).ok()?;
        stream
            .set_write_timeout(Some(Duration::from_secs(2)))
            .ok()?;
        write!(
            stream,
            "GET /long/ HTTP/1.0\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
        )
        .ok()?;
        let _ = stream.flush();
        std::thread::sleep(Duration::from_millis(80));
        Some(stream)
    });
    let _held = handle.join().expect("start long content sleep");

    let start = Instant::now();
    quit_and_wait(&mut running.child, Duration::from_secs(5));
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "graceful SIGQUIT with a 30s content sleep must not wait out the sleep, elapsed {:?}",
        start.elapsed()
    );
}
