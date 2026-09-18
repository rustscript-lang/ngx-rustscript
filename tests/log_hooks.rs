//! Source and artifact isolation for log-phase integration proof hooks.

mod support;

use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

use ngx_rustscript_nginx_test_stubs as _;
use support::hooks::{
    HOOK_SOURCE_IDENTS, HOOK_STRINGS, assert_has_hook_strings, assert_no_hook_strings,
    hook_enabled_debug_cdylib, isolated_ordinary_debug_cdylib, isolated_ordinary_release_cdylib,
    same_target_isolation_sequence, strip_integration_hook_items,
};
use support::nginx::{
    combined, nginx_binary, nginx_escape_quoted, nginx_process_group_command, nginx_quote_path,
    nginx_test, terminate_process_group,
};
use support::temp::isolated_prefix;

const PRODUCTION_SOURCE_FILES: &[&str] = &[
    "build.rs",
    "src/lib.rs",
    "src/log.rs",
    "src/access.rs",
    "src/request_state.rs",
    "src/host/body.rs",
];

fn crate_file(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)
}

fn read_crate_file(rel: &str) -> String {
    fs::read_to_string(crate_file(rel))
        .unwrap_or_else(|err| panic!("read {}: {err}", crate_file(rel).display()))
}

#[test]
fn log_proof_hooks_are_cfg_gated_in_source() {
    for rel in PRODUCTION_SOURCE_FILES {
        let src = read_crate_file(rel);
        assert!(
            !src.contains("debug_assertions, ngx_rustscript_integration_hooks")
                && !src.contains("any(debug_assertions, ngx_rustscript_integration_hooks)"),
            "{rel} must not gate proof hooks on debug_assertions"
        );
        let stripped = strip_integration_hook_items(&src);
        for ident in HOOK_SOURCE_IDENTS {
            assert!(
                !stripped.contains(ident),
                "{rel} still mentions {ident} outside cfg(ngx_rustscript_integration_hooks):\n{stripped}"
            );
        }
    }
}

#[test]
fn ordinary_debug_cdylib_contains_no_log_proof_hooks() {
    let module = isolated_ordinary_debug_cdylib();
    assert_no_hook_strings(module);
}

#[test]
fn ordinary_release_cdylib_contains_no_log_proof_hooks() {
    let module = isolated_ordinary_release_cdylib();
    assert_no_hook_strings(module);
}

#[test]
fn same_target_hooks_rebuild_clears_fingerprint_cfg_and_artifacts() {
    let seq = same_target_isolation_sequence();
    assert_eq!(seq.first_ordinary.fingerprint, 0);
    assert!(!seq.first_ordinary.hooks_cfg);
    assert_eq!(seq.hook_enabled.fingerprint, 1);
    assert!(seq.hook_enabled.hooks_cfg);
    assert_eq!(seq.second_ordinary.fingerprint, 0);
    assert!(!seq.second_ordinary.hooks_cfg);
    assert_eq!(seq.post_hook_release.fingerprint, 0);
    assert!(!seq.post_hook_release.hooks_cfg);
    assert!(seq.hook_enabled.reran_build_script);
    assert!(seq.second_ordinary.reran_build_script);
    assert!(seq.post_hook_release.reran_build_script);
    assert_no_hook_strings(&seq.ordinary_debug);
    assert_has_hook_strings(&seq.retained_hook_debug);
    assert_no_hook_strings(&seq.ordinary_release);
}

#[test]
fn hook_enabled_debug_cdylib_contains_log_proof_hooks() {
    assert_has_hook_strings(hook_enabled_debug_cdylib());
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

fn inject_main_env(conf: &str, vars: &[(&str, &str)]) -> String {
    let mut env_lines = String::new();
    for (key, value) in vars {
        env_lines.push_str(&format!("env {key}={value};\n"));
    }
    format!("{env_lines}{conf}")
}

fn wait_for_port(port: u16, timeout: Duration) {
    support::nginx::wait_for_worker_http(port, timeout);
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

fn log_script() -> &'static str {
    r#"
use ngx;
ngx::log(ngx::log_level::notice(), "LOG_PHASE=" + ngx::get_phase());
ngx::log(ngx::log_level::notice(), "LOG_MARK=" + ngx::encode_args({ uri: ngx::var::get("uri") }));
"#
}

fn hook_log_location(prefix: &Path, extra_locations: &str) -> (u16, PathBuf) {
    let nginx = nginx_binary();
    let module = hook_enabled_debug_cdylib();
    let port = free_port();
    write_index(prefix, "ok/index.html", "ok-body\n");
    write_index(prefix, "follow/index.html", "follow-ok\n");
    let script = prefix.join("conf/log.rss");
    write_script(&script, log_script());
    let script_q = nginx_quote_path(&script).unwrap();
    write_conf(
        prefix,
        &inject_main_env(
            &http_conf(
                module,
                prefix,
                port,
                &format!(
                    r#"
        location /ok/ {{
            log_by_rss_file {script_q};
        }}
        location /follow/ {{
        }}
{extra_locations}
"#
                ),
            ),
            &[
                ("NGX_RUSTSCRIPT_TEST_LOG_PANIC", "1"),
                ("NGX_RUSTSCRIPT_LOG_WAIT_PROOF", "1"),
            ],
        ),
    );
    let out = nginx_test(&nginx, prefix);
    let text = combined(&out);
    assert!(
        out.status.success(),
        "hook-enabled conf must pass nginx -t:\n{text}"
    );
    (port, prefix.join("logs/error.log"))
}

#[test]
fn hook_enabled_log_panic_is_isolated_and_follow_up_succeeds() {
    let nginx = nginx_binary();
    let prefix = isolated_prefix("log-hook-panic");
    let (port, error) = hook_log_location(&prefix, "");
    let _child = spawn_nginx(&nginx, &prefix);
    wait_for_port(port, Duration::from_secs(5));
    let (status, response) = http_get(port, "/ok/");
    assert_eq!(
        status, 200,
        "panic must not change the already-sent 200:\n{response}"
    );
    assert!(
        http_body(&response).contains("ok-body"),
        "body must stay static:\n{response}"
    );
    assert!(
        wait_log_contains(&error, "log_by_rss_file: panic", Duration::from_secs(3)),
        "panic must be logged:\n{}",
        fs::read_to_string(&error).unwrap_or_default()
    );
    let (status, response) = http_get(port, "/follow/");
    assert_eq!(status, 200, "follow-up request must succeed:\n{response}");
    assert!(
        http_body(&response).contains("follow-ok"),
        "follow-up body:\n{response}"
    );
}

#[test]
fn hook_enabled_log_wait_count_proof_stays_zero() {
    let nginx = nginx_binary();
    let prefix = isolated_prefix("log-hook-wait");
    write_conf_wait_only(&prefix);
    let out = nginx_test(&nginx, &prefix);
    let text = combined(&out);
    assert!(
        out.status.success(),
        "wait-proof conf must pass nginx -t:\n{text}"
    );
    let port = wait_only_port(&prefix);
    let _child = spawn_nginx(&nginx, &prefix);
    wait_for_port(port, Duration::from_secs(5));
    let (status, response) = http_get(port, "/ok/");
    assert_eq!(status, 200, "{response}");
    let error = prefix.join("logs/error.log");
    assert!(
        wait_log_contains(
            &error,
            "LOG_WAIT_PROOF eventfd=0 count=0 timer=0",
            Duration::from_secs(3)
        ),
        "wait-arm counters must stay zero:\n{}",
        fs::read_to_string(&error).unwrap_or_default()
    );
}

fn write_conf_wait_only(prefix: &Path) {
    let module = hook_enabled_debug_cdylib();
    let port = free_port();
    write_index(prefix, "ok/index.html", "ok\n");
    let script = prefix.join("conf/log.rss");
    write_script(&script, log_script());
    let script_q = nginx_quote_path(&script).unwrap();
    let conf = inject_main_env(
        &http_conf(
            module,
            prefix,
            port,
            &format!(
                r#"
        location /ok/ {{
            log_by_rss_file {script_q};
        }}
"#
            ),
        ),
        &[("NGX_RUSTSCRIPT_LOG_WAIT_PROOF", "1")],
    );
    fs::write(prefix.join("conf/nginx.conf"), &conf).unwrap();
    fs::write(prefix.join("conf/listen.port"), port.to_string()).unwrap();
}

fn wait_only_port(prefix: &Path) -> u16 {
    fs::read_to_string(prefix.join("conf/listen.port"))
        .unwrap()
        .trim()
        .parse()
        .expect("listen.port")
}

#[test]
fn hook_strings_are_the_documented_runtime_needles() {
    assert_eq!(
        HOOK_STRINGS,
        [
            "NGX_RUSTSCRIPT_TEST_LOG_PANIC",
            "NGX_RUSTSCRIPT_LOG_WAIT_PROOF",
            "LOG_WAIT_PROOF",
            "NGX_RUSTSCRIPT_BODY_COUNT_PROOF",
            "BODY_COUNT_PROOF",
            "BODY_WAIT_ARMED",
        ]
    );
}
