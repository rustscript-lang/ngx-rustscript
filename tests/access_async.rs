//! Focused real NGINX async HTTP tests for access `ngx::sleep` suspend/resume.
//!
//! `master_process on`, one worker, absolute `load_module` path, isolated prefix.
//! Observable post-sleep behavior is the content phase running (HTTP 200 + body).

mod support;

use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

use support::artifact::module_cdylib;
use support::nginx::{
    combined, nginx_binary, nginx_escape_quoted, nginx_process_group_command, nginx_quote_path,
    nginx_test, terminate_process_group,
};
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

fn async_conf(module: &Path, prefix: &Path, port: u16) -> String {
    let module = nginx_quote_path(module).unwrap();
    let pid = nginx_quote_path(&prefix.join("logs/nginx.pid")).unwrap();
    let html = nginx_quote_path(&prefix.join("html")).unwrap();
    let prefix_q = nginx_escape_quoted(prefix.to_str().unwrap()).unwrap();
    let sleep = nginx_quote_path(&fixture("sleep_30ms.rss")).unwrap();
    let sleep_long = nginx_quote_path(&fixture("sleep_80ms.rss")).unwrap();
    let two = nginx_quote_path(&fixture("two_sleeps_access.rss")).unwrap();
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

        location /sleep/ {{
            access_by_rss_file {sleep};
        }}

        location /alpha/ {{
            access_by_rss_file {sleep_long};
        }}

        location /beta/ {{
            access_by_rss_file {sleep};
        }}

        location /two/ {{
            access_by_rss_file {two};
        }}

        location /plain/ {{
        }}
    }}
}}
"#
    )
}

fn nginx_t_conf(module: &Path, prefix: &Path, script: &Path) -> String {
    let module = nginx_quote_path(module).unwrap();
    let pid = nginx_quote_path(&prefix.join("logs/nginx.pid")).unwrap();
    let script = nginx_quote_path(script).unwrap();
    format!(
        r#"
worker_processes 1;
error_log stderr emerg;
pid {pid};
daemon off;
master_process on;
load_module {module};

events {{
    worker_connections 32;
}}

http {{
    access_log off;
    server {{
        listen 127.0.0.1:18080;
        server_name localhost;
        location / {{
            access_by_rss_file {script};
            return 200;
        }}
    }}
}}
"#
    )
}

#[test]
fn config_accepts_ngx_sleep_and_rejects_other_imports() {
    let nginx = nginx_binary();
    let module = module_cdylib();

    let prefix = isolated_prefix("async-t-sleep");
    write_conf(
        &prefix,
        &nginx_t_conf(&module, &prefix, &fixture("sleep_30ms.rss")),
    );
    let out = nginx_test(&nginx, &prefix);
    let text = combined(&out);
    assert!(
        out.status.success(),
        "ngx::sleep must pass nginx -t:\n{text}"
    );

    for (label, name, needle) in [
        ("async-t-print", "print.rss", "print"),
        ("async-t-runtime", "runtime_sleep.rss", "runtime::sleep"),
        ("async-t-unknown", "unknown_import.rss", "unknown"),
    ] {
        let prefix = isolated_prefix(label);
        write_conf(&prefix, &nginx_t_conf(&module, &prefix, &fixture(name)));
        let out = nginx_test(&nginx, &prefix);
        let text = combined(&out);
        assert!(!out.status.success(), "expected nginx -t fail:\n{text}");
        let lower = text.to_ascii_lowercase();
        assert!(
            lower.contains("script imports are not supported")
                || lower.contains("failed to compile script"),
            "{name} must report import or compile cause, got:\n{text}"
        );
        assert!(
            lower.contains(needle) || (needle == "unknown" && lower.contains("unknownmod")),
            "{name} must mention {needle}, got:\n{text}"
        );
    }
}

#[test]
fn sleep_30ms_reaches_content_with_http_200() {
    let nginx = nginx_binary();
    let module = module_cdylib();
    let prefix = isolated_prefix("async-http-sleep");
    let port = free_port();
    write_index(&prefix, "sleep/index.html", "slept\n");
    write_index(&prefix, "plain/index.html", "plain\n");
    write_conf(&prefix, &async_conf(&module, &prefix, port));

    let out = nginx_test(&nginx, &prefix);
    let text = combined(&out);
    assert!(
        out.status.success(),
        "async fixture must pass nginx -t:\n{text}"
    );

    let _child = spawn_nginx(&nginx, &prefix);
    wait_for_port(port, Duration::from_secs(5));

    let started = Instant::now();
    let (status, response) = http_get(port, "/sleep/");
    let elapsed = started.elapsed();
    assert_eq!(status, 200, "{response}");
    assert!(
        http_body(&response).contains("slept"),
        "content must run after sleep resume; got:\n{response}"
    );
    assert!(
        elapsed >= Duration::from_millis(20),
        "30ms sleep should not complete immediately, elapsed {elapsed:?}"
    );
}

#[test]
fn two_sleeps_resume_twice_and_reach_content() {
    let nginx = nginx_binary();
    let module = module_cdylib();
    let prefix = isolated_prefix("async-http-two");
    let port = free_port();
    write_index(&prefix, "two/index.html", "twice\n");
    write_conf(&prefix, &async_conf(&module, &prefix, port));

    let out = nginx_test(&nginx, &prefix);
    let text = combined(&out);
    assert!(
        out.status.success(),
        "async fixture must pass nginx -t:\n{text}"
    );

    let _child = spawn_nginx(&nginx, &prefix);
    wait_for_port(port, Duration::from_secs(5));

    let started = Instant::now();
    let (status, response) = http_get(port, "/two/");
    let elapsed = started.elapsed();
    assert_eq!(status, 200, "{response}");
    assert!(
        http_body(&response).contains("twice"),
        "content must run after two sleep resumes; got:\n{response}"
    );
    assert!(
        elapsed >= Duration::from_millis(15),
        "two 10ms sleeps should not complete immediately, elapsed {elapsed:?}"
    );
}

#[test]
fn synchronous_route_remains_unchanged() {
    let nginx = nginx_binary();
    let module = module_cdylib();
    let prefix = isolated_prefix("async-http-plain");
    let port = free_port();
    write_index(&prefix, "plain/index.html", "plain\n");
    write_index(&prefix, "sleep/index.html", "slept\n");
    write_conf(&prefix, &async_conf(&module, &prefix, port));

    let _child = spawn_nginx(&nginx, &prefix);
    wait_for_port(port, Duration::from_secs(5));

    let started = Instant::now();
    let (status, response) = http_get(port, "/plain/");
    let elapsed = started.elapsed();
    assert_eq!(status, 200, "{response}");
    assert!(
        http_body(&response).contains("plain"),
        "unconfigured location must stay unaffected; got:\n{response}"
    );
    assert!(
        elapsed < Duration::from_millis(200),
        "synchronous route should not wait on sleep, elapsed {elapsed:?}"
    );
}

#[test]
fn overlapping_sleep_requests_keep_distinct_responses() {
    let nginx = nginx_binary();
    let module = module_cdylib();
    let prefix = isolated_prefix("async-http-overlap");
    let port = free_port();
    write_index(&prefix, "alpha/index.html", "alpha-body\n");
    write_index(&prefix, "beta/index.html", "beta-body\n");
    write_index(&prefix, "plain/index.html", "plain\n");
    write_conf(&prefix, &async_conf(&module, &prefix, port));

    let out = nginx_test(&nginx, &prefix);
    let text = combined(&out);
    assert!(
        out.status.success(),
        "overlap fixture must pass nginx -t:\n{text}"
    );

    let _child = spawn_nginx(&nginx, &prefix);
    wait_for_port(port, Duration::from_secs(5));

    let alpha = std::thread::spawn(move || http_get(port, "/alpha/"));
    std::thread::sleep(Duration::from_millis(15));
    let (beta_status, beta_response) = http_get(port, "/beta/");
    let (alpha_status, alpha_response) = alpha.join().expect("alpha request thread");

    assert_eq!(alpha_status, 200, "{alpha_response}");
    assert_eq!(beta_status, 200, "{beta_response}");
    let alpha_body = http_body(&alpha_response);
    let beta_body = http_body(&beta_response);
    assert!(
        alpha_body.contains("alpha-body"),
        "alpha must keep its own body; got:\n{alpha_response}"
    );
    assert!(
        beta_body.contains("beta-body"),
        "beta must keep its own body; got:\n{beta_response}"
    );
    assert!(
        !alpha_body.contains("beta-body"),
        "alpha must not receive beta's body:\n{alpha_response}"
    );
    assert!(
        !beta_body.contains("alpha-body"),
        "beta must not receive alpha's body:\n{beta_response}"
    );

    let (plain_status, plain_response) = http_get(port, "/plain/");
    assert_eq!(plain_status, 200, "{plain_response}");
    assert!(
        http_body(&plain_response).contains("plain"),
        "later plain request must succeed; got:\n{plain_response}"
    );
}
