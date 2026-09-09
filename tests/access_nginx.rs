//! Real `nginx -t` and HTTP acceptance for `access_by_rss_file`.

mod support;

use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use support::artifact::module_cdylib;
use support::nginx::{combined, nginx_binary, nginx_escape_quoted, nginx_quote_path, nginx_test};
use support::temp::{isolated_prefix, unique_temp_dir};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn write_conf(prefix: &Path, body: &str) {
    fs::write(prefix.join("conf/nginx.conf"), body).unwrap();
}

fn conf_skeleton(module: &Path, prefix: &Path, locations: &str) -> String {
    let module = nginx_quote_path(module).unwrap();
    let pid = nginx_quote_path(&prefix.join("logs/nginx.pid")).unwrap();
    let port = free_port();
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
        listen 127.0.0.1:{port};
        server_name localhost;
{locations}
    }}
}}
"#
    )
}

fn location_with_script(path: &str, script: &Path) -> String {
    let script = nginx_quote_path(script).unwrap();
    format!(
        r#"
        location {path} {{
            access_by_rss_file {script};
            return 200;
        }}
"#
    )
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
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(3)))
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

#[test]
fn nginx_t_rejects_missing_file() {
    let nginx = nginx_binary();
    let module = module_cdylib();
    let prefix = isolated_prefix("t-missing");
    let missing = prefix.join("conf/no-such.rss");
    let conf = conf_skeleton(&module, &prefix, &location_with_script("/", &missing));
    write_conf(&prefix, &conf);
    let out = nginx_test(&nginx, &prefix);
    let text = combined(&out);
    assert!(!out.status.success(), "expected nginx -t fail:\n{text}");
    assert!(
        text.contains("script file not found"),
        "missing file must report that cause, got:\n{text}"
    );
}

#[test]
fn nginx_t_rejects_syntax_error() {
    let nginx = nginx_binary();
    let module = module_cdylib();
    let prefix = isolated_prefix("t-syntax");
    let conf = conf_skeleton(
        &module,
        &prefix,
        &location_with_script("/", &fixture("syntax_error.rss")),
    );
    write_conf(&prefix, &conf);
    let out = nginx_test(&nginx, &prefix);
    let text = combined(&out);
    assert!(!out.status.success(), "expected nginx -t fail:\n{text}");
    assert!(
        text.contains("failed to compile script"),
        "syntax error must report compile cause, got:\n{text}"
    );
}

#[test]
fn nginx_t_rejects_unknown_import() {
    let nginx = nginx_binary();
    let module = module_cdylib();
    let prefix = isolated_prefix("t-import");
    let conf = conf_skeleton(
        &module,
        &prefix,
        &location_with_script("/", &fixture("unknown_import.rss")),
    );
    write_conf(&prefix, &conf);
    let out = nginx_test(&nginx, &prefix);
    let text = combined(&out);
    assert!(!out.status.success(), "expected nginx -t fail:\n{text}");
    let lower = text.to_ascii_lowercase();
    assert!(
        lower.contains("failed to compile script")
            || lower.contains("script imports are not supported"),
        "unknown import must report compile or import cause, got:\n{text}"
    );
    assert!(
        lower.contains("unknown") || lower.contains("unknownmod"),
        "unknown import must name the module, got:\n{text}"
    );
}

fn nginx_t_rejects_import_fixture(label: &str, fixture_name: &str, needles: &[&str]) {
    let nginx = nginx_binary();
    let module = module_cdylib();
    let prefix = isolated_prefix(label);
    let conf = conf_skeleton(
        &module,
        &prefix,
        &location_with_script("/", &fixture(fixture_name)),
    );
    write_conf(&prefix, &conf);
    let out = nginx_test(&nginx, &prefix);
    let text = combined(&out);
    assert!(!out.status.success(), "expected nginx -t fail:\n{text}");
    let lower = text.to_ascii_lowercase();
    assert!(
        lower.contains("script imports are not supported")
            || lower.contains("failed to compile script"),
        "{fixture_name} must report import or compile cause, got:\n{text}"
    );
    for needle in needles {
        assert!(
            lower.contains(&needle.to_ascii_lowercase()),
            "{fixture_name} must mention {needle}, got:\n{text}"
        );
    }
}

#[test]
fn nginx_t_rejects_print_import() {
    nginx_t_rejects_import_fixture("t-print", "print.rss", &["print"]);
}

#[test]
fn nginx_t_rejects_runtime_sleep_import() {
    nginx_t_rejects_import_fixture("t-runtime-sleep", "runtime_sleep.rss", &["runtime::sleep"]);
}

#[test]
fn nginx_t_accepts_ngx_sleep_import() {
    let nginx = nginx_binary();
    let module = module_cdylib();
    let prefix = isolated_prefix("t-ngx-sleep");
    let conf = conf_skeleton(
        &module,
        &prefix,
        &location_with_script("/", &fixture("ngx_sleep.rss")),
    );
    write_conf(&prefix, &conf);
    let out = nginx_test(&nginx, &prefix);
    let text = combined(&out);
    assert!(
        out.status.success(),
        "ngx::sleep must pass nginx -t:\n{text}"
    );
}

#[test]
fn nginx_t_rejects_duplicate_directive() {
    let nginx = nginx_binary();
    let module = module_cdylib();
    let prefix = isolated_prefix("t-dup");
    let script = nginx_quote_path(&fixture("ok.rss")).unwrap();
    let locations = format!(
        r#"
        location / {{
            access_by_rss_file {script};
            access_by_rss_file {script};
            return 200;
        }}
"#
    );
    write_conf(&prefix, &conf_skeleton(&module, &prefix, &locations));
    let out = nginx_test(&nginx, &prefix);
    let text = combined(&out);
    assert!(!out.status.success(), "expected nginx -t fail:\n{text}");
    let lower = text.to_ascii_lowercase();
    assert!(
        lower.contains("duplicate access_by_rss_file"),
        "duplicate must report that cause, got:\n{text}"
    );
}

#[test]
fn nginx_t_rejects_wrong_arity() {
    let nginx = nginx_binary();
    let module = module_cdylib();
    let prefix = isolated_prefix("t-arity");
    let locations = r#"
        location /none {
            access_by_rss_file;
            return 200;
        }
"#;
    write_conf(&prefix, &conf_skeleton(&module, &prefix, locations));
    let out = nginx_test(&nginx, &prefix);
    let text = combined(&out);
    assert!(!out.status.success(), "expected nginx -t fail:\n{text}");
    assert!(text.contains("invalid number of arguments"), "got:\n{text}");

    let script = nginx_quote_path(&fixture("ok.rss")).unwrap();
    let locations = format!(
        r#"
        location /two {{
            access_by_rss_file {script} extra;
            return 200;
        }}
"#
    );
    write_conf(&prefix, &conf_skeleton(&module, &prefix, &locations));
    let out = nginx_test(&nginx, &prefix);
    let text = combined(&out);
    assert!(!out.status.success(), "expected nginx -t fail:\n{text}");
    assert!(text.contains("invalid number of arguments"), "got:\n{text}");
}

#[test]
fn nginx_t_accepts_relative_and_absolute_paths() {
    let nginx = nginx_binary();
    let module = module_cdylib();
    let prefix = isolated_prefix("t-paths");
    fs::create_dir_all(prefix.join("conf/scripts")).unwrap();
    fs::copy(fixture("ok.rss"), prefix.join("conf/scripts/ok.rss")).unwrap();
    let abs = nginx_quote_path(&fixture("ok.rss")).unwrap();
    let locations = format!(
        r#"
        location /rel {{
            access_by_rss_file scripts/ok.rss;
            return 200;
        }}
        location /abs {{
            access_by_rss_file {abs};
            return 200;
        }}
"#
    );
    write_conf(&prefix, &conf_skeleton(&module, &prefix, &locations));
    let out = nginx_test(&nginx, &prefix);
    let text = combined(&out);
    assert!(
        out.status.success(),
        "relative/absolute paths should pass nginx -t:\n{text}"
    );
}

#[test]
fn nginx_t_accepts_inherited_and_override_config() {
    let nginx = nginx_binary();
    let module = module_cdylib();
    let prefix = isolated_prefix("t-inherit");
    let parent = nginx_quote_path(&fixture("runtime_error.rss")).unwrap();
    let child = nginx_quote_path(&fixture("ok.rss")).unwrap();
    let locations = format!(
        r#"
        location /parent {{
            access_by_rss_file {parent};
            return 200;
            location /parent/child {{
                return 200;
            }}
            location /parent/override {{
                access_by_rss_file {child};
                return 200;
            }}
        }}
"#
    );
    write_conf(&prefix, &conf_skeleton(&module, &prefix, &locations));
    let out = nginx_test(&nginx, &prefix);
    let text = combined(&out);
    assert!(
        out.status.success(),
        "inherited/override config should pass nginx -t:\n{text}"
    );
}

#[test]
fn nginx_t_quotes_script_path_with_spaces() {
    let nginx = nginx_binary();
    let module = module_cdylib();
    let prefix = isolated_prefix("t-spaces");
    let dir = unique_temp_dir("rss space");
    let script = dir.join("ok file.rss");
    fs::copy(fixture("ok.rss"), &script).unwrap();
    let conf = conf_skeleton(&module, &prefix, &location_with_script("/", &script));
    write_conf(&prefix, &conf);
    let out = nginx_test(&nginx, &prefix);
    let text = combined(&out);
    assert!(
        out.status.success(),
        "quoted path with spaces should pass nginx -t:\n{text}"
    );
}

#[test]
fn http_sync_script_halts_and_content_responds() {
    let nginx = nginx_binary();
    let module = module_cdylib();
    let prefix = isolated_prefix("http-sync");
    let port = free_port();
    write_index(&prefix, "scripted/index.html", "scripted\n");
    write_index(&prefix, "plain/index.html", "plain\n");
    write_index(&prefix, "parent/inherited/index.html", "inherited\n");
    write_index(&prefix, "parent/override/index.html", "override\n");

    let template = include_str!("fixtures/nginx.access.conf.in");
    let rendered = template
        .replace("@@MODULE@@", &nginx_quote_path(&module).unwrap())
        .replace(
            "@@PREFIX@@",
            &nginx_escape_quoted(prefix.to_str().unwrap()).unwrap(),
        )
        .replace("@@PORT@@", &port.to_string())
        .replace(
            "@@SCRIPT_OK@@",
            &nginx_quote_path(&fixture("ok.rss")).unwrap(),
        )
        .replace(
            "@@SCRIPT_RUNTIME@@",
            &nginx_quote_path(&fixture("runtime_error.rss")).unwrap(),
        );
    write_conf(&prefix, &rendered);

    let _child = spawn_nginx(&nginx, &prefix);
    wait_for_port(port, Duration::from_secs(5));

    let (status, response) = http_get(port, "/scripted/");
    assert_eq!(status, 200, "{response}");
    assert!(
        http_body(&response).contains("scripted"),
        "content stage must run after Halted access; got:\n{response}"
    );

    let (status, response) = http_get(port, "/plain/");
    assert_eq!(status, 200, "{response}");
    assert!(
        http_body(&response).contains("plain"),
        "unconfigured location must stay unaffected; got:\n{response}"
    );

    let (status, response) = http_get(port, "/parent/inherited/");
    assert_eq!(
        status, 500,
        "inherited runtime-error script must fail the request; got:\n{response}"
    );

    let (status, response) = http_get(port, "/parent/override/");
    assert_eq!(status, 200, "{response}");
    assert!(
        http_body(&response).contains("override"),
        "explicit child must override parent script; got:\n{response}"
    );

    let log = fs::read_to_string(prefix.join("logs/error.log")).unwrap_or_default();
    assert!(
        log.to_ascii_lowercase().contains("access_by_rss_file")
            || log.to_ascii_lowercase().contains("division"),
        "runtime VM error must be logged; got:\n{log}"
    );
}

#[test]
fn http_satisfy_any_scripted_ok_permits_unconfigured_declined_does_not() {
    let nginx = nginx_binary();
    let module = module_cdylib();
    let prefix = isolated_prefix("http-satisfy");
    let port = free_port();
    let script = nginx_quote_path(&fixture("ok.rss")).unwrap();
    let module_q = nginx_quote_path(&module).unwrap();
    let pid = nginx_quote_path(&prefix.join("logs/nginx.pid")).unwrap();
    let html = nginx_quote_path(&prefix.join("html")).unwrap();
    write_index(&prefix, "satisfy-any-ok/index.html", "any-ok\n");
    write_index(&prefix, "satisfy-any-declined/index.html", "any-declined\n");
    write_index(&prefix, "satisfy-all-ok/index.html", "all-ok\n");
    write_index(&prefix, "satisfy-default-ok/index.html", "default-ok\n");
    let conf = format!(
        r#"
worker_processes 1;
error_log stderr emerg;
pid {pid};
daemon off;
master_process off;
load_module {module_q};

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

        location /satisfy-any-ok/ {{
            satisfy any;
            deny all;
            access_by_rss_file {script};
        }}

        location /satisfy-any-declined/ {{
            satisfy any;
            deny all;
        }}

        location /satisfy-all-ok/ {{
            satisfy all;
            deny all;
            access_by_rss_file {script};
        }}

        location /satisfy-default-ok/ {{
            deny all;
            access_by_rss_file {script};
        }}
    }}
}}
"#
    );
    write_conf(&prefix, &conf);
    let out = nginx_test(&nginx, &prefix);
    let text = combined(&out);
    assert!(
        out.status.success(),
        "satisfy fixture must pass nginx -t:\n{text}"
    );

    let _child = spawn_nginx(&nginx, &prefix);
    wait_for_port(port, Duration::from_secs(5));

    let (status, response) = http_get(port, "/satisfy-any-ok/");
    assert_eq!(status, 200, "{response}");
    assert!(
        http_body(&response).contains("any-ok"),
        "scripted NGX_OK under satisfy any must permit content; got:\n{response}"
    );

    let (status, response) = http_get(port, "/satisfy-any-declined/");
    assert_eq!(
        status, 403,
        "unconfigured DECLINED under satisfy any must not permit content; got:\n{response}"
    );

    let (status, response) = http_get(port, "/satisfy-all-ok/");
    assert_eq!(
        status, 403,
        "satisfy all must still apply deny all after scripted NGX_OK; got:\n{response}"
    );

    let (status, response) = http_get(port, "/satisfy-default-ok/");
    assert_eq!(
        status, 403,
        "default satisfy all must still apply deny all after scripted NGX_OK; got:\n{response}"
    );
}

fn meta_conf(module: &str, prefix: &Path, port: u16, locations: &str) -> String {
    format!(
        r#"
worker_processes 1;
error_log "{error_log}" notice;
pid {pid};
daemon off;
master_process off;
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
"#,
        error_log = prefix.join("logs/error.log").display(),
        pid = nginx_quote_path(&prefix.join("logs/nginx.pid")).unwrap(),
        html = nginx_quote_path(&prefix.join("html")).unwrap(),
    )
}

#[test]
fn access_phase_logs_worker_metadata_constants_and_request_log() {
    let nginx = nginx_binary();
    let module = module_cdylib();
    let port = free_port();
    let prefix = isolated_prefix("ngx-rustscript-meta");
    write_index(&prefix, "meta/index.html", "meta\n");
    let script = prefix.join("conf/meta.rss");
    write_script(
        &script,
        r#"
use ngx;
ngx::log(ngx::log_level::notice(), "TASK4_REQUEST_LOG");
ngx::log(ngx::log_level::notice(), "TASK4_PREFIX=" + ngx::config::prefix());
ngx::log(ngx::log_level::notice(), "TASK4_META=" + ngx::encode_args({ pid: ngx::worker::pid(), id: ngx::worker::id(), count: ngx::worker::count(), version: ngx::config::nginx_version(), phase: ngx::get_phase(), ok: ngx::core::ok(), status_ok: ngx::http::status_ok(), get: ngx::http::get(), err: ngx::log_level::error(), subsystem: ngx::config::subsystem() }));
ngx::log(ngx::log_level::notice(), "TASK4_PIDS=" + ngx::encode_args({ pid: ngx::worker::pids() }));
if ngx::config::debug() {
    ngx::log(-1, "unexpected-debug");
}
if ngx::worker::exiting() {
    ngx::log(-1, "unexpected-exiting");
}
"#,
    );
    let conf = meta_conf(
        &nginx_quote_path(&module).unwrap(),
        &prefix,
        port,
        &format!(
            r#"
        location /meta/ {{
            access_by_rss_file {script};
        }}
"#,
            script = nginx_quote_path(&script).unwrap(),
        ),
    );
    write_conf(&prefix, &conf);
    let out = nginx_test(&nginx, &prefix);
    let text = combined(&out);
    assert!(
        out.status.success(),
        "metadata script must pass nginx -t:\n{text}"
    );

    let child = spawn_nginx(&nginx, &prefix);
    wait_for_port(port, Duration::from_secs(5));
    let (status, response) = http_get(port, "/meta/");
    assert_eq!(status, 200, "{response}");
    assert!(
        http_body(&response).contains("meta"),
        "content must run after metadata access script; got:\n{response}"
    );

    let log = fs::read_to_string(prefix.join("logs/error.log")).unwrap_or_default();
    assert!(
        log.contains("ngx-rustscript: TASK4_REQUEST_LOG"),
        "request-scoped ngx::log must write the module prefix; got:\n{log}"
    );
    let prefix_line = log
        .lines()
        .find(|line| line.contains("TASK4_PREFIX="))
        .unwrap_or_else(|| panic!("missing TASK4_PREFIX in log:\n{log}"));
    let logged_prefix = prefix_line
        .rsplit_once("TASK4_PREFIX=")
        .map(|(_, rest)| rest.split(", client:").next().unwrap_or(rest).trim())
        .unwrap_or("");
    let prefix_str = prefix.to_string_lossy();
    assert!(
        logged_prefix.contains(prefix_str.trim_end_matches('/'))
            || prefix_str.contains(logged_prefix.trim_end_matches('/')),
        "prefix must be the live NGINX prefix {prefix_str}, got {logged_prefix}"
    );

    let meta_line = log
        .lines()
        .find(|line| line.contains("TASK4_META="))
        .unwrap_or_else(|| panic!("missing TASK4_META in log:\n{log}"));
    let query = meta_line
        .rsplit_once("TASK4_META=")
        .map(|(_, rest)| rest.split(", client:").next().unwrap_or(rest).trim())
        .unwrap_or("");
    let mut fields = std::collections::HashMap::new();
    for pair in query.split('&') {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        fields.insert(key.to_string(), value.to_string());
    }
    let pid: i64 = fields
        .get("pid")
        .expect("pid field")
        .parse()
        .expect("pid int");
    assert_eq!(
        pid,
        i64::from(child.child.id()),
        "worker pid must match the live nginx worker, fields={fields:?}"
    );
    assert_eq!(fields.get("id").map(String::as_str), Some("0"));
    assert_eq!(fields.get("count").map(String::as_str), Some("1"));
    assert_eq!(fields.get("version").map(String::as_str), Some("1030004"));
    assert_eq!(fields.get("phase").map(String::as_str), Some("access"));
    assert_eq!(fields.get("ok").map(String::as_str), Some("0"));
    assert_eq!(fields.get("status_ok").map(String::as_str), Some("200"));
    assert_eq!(fields.get("get").map(String::as_str), Some("2"));
    assert_eq!(fields.get("err").map(String::as_str), Some("4"));
    assert_eq!(fields.get("subsystem").map(String::as_str), Some("http"));

    let pids_line = log
        .lines()
        .find(|line| line.contains("TASK4_PIDS="))
        .unwrap_or_else(|| panic!("missing TASK4_PIDS in log:\n{log}"));
    let pids_query = pids_line
        .rsplit_once("TASK4_PIDS=")
        .map(|(_, rest)| rest.split(", client:").next().unwrap_or(rest).trim())
        .unwrap_or("");
    let mut pids = Vec::new();
    for pair in pids_query.split('&') {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        if key == "pid" {
            pids.push(value.parse::<i64>().expect("pid int"));
        }
    }
    assert!(
        pids.contains(&pid),
        "worker::pids must include the current ngx_pid {pid}, got {pids:?}"
    );
    let unique = pids
        .iter()
        .copied()
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        unique.len(),
        pids.len(),
        "worker::pids must not contain duplicates, got {pids:?}"
    );
    assert_eq!(
        pids.len(),
        1,
        "single-worker nginx must report only the live worker pid, got {pids:?}"
    );
}

#[test]
fn access_phase_rejects_invalid_log_level_and_embedded_nul() {
    let nginx = nginx_binary();
    let module = module_cdylib();
    let port = free_port();
    let prefix = isolated_prefix("ngx-rustscript-log-reject");
    write_index(&prefix, "bad-level/index.html", "level-ok\n");
    write_index(&prefix, "bad-nul/index.html", "nul-ok\n");
    let bad_level = prefix.join("conf/bad_level.rss");
    let bad_nul = prefix.join("conf/bad_nul.rss");
    write_script(&bad_level, "use ngx;\nngx::log(99, \"bad-level\");\n");
    write_script(
        &bad_nul,
        "use ngx;\nngx::log(ngx::log_level::notice(), ngx::unescape_uri(\"a%00b\"));\n",
    );
    let conf = meta_conf(
        &nginx_quote_path(&module).unwrap(),
        &prefix,
        port,
        &format!(
            r#"
        location /bad-level/ {{
            access_by_rss_file {bad_level};
        }}
        location /bad-nul/ {{
            access_by_rss_file {bad_nul};
        }}
"#,
            bad_level = nginx_quote_path(&bad_level).unwrap(),
            bad_nul = nginx_quote_path(&bad_nul).unwrap(),
        ),
    );
    write_conf(&prefix, &conf);
    let out = nginx_test(&nginx, &prefix);
    let text = combined(&out);
    assert!(
        out.status.success(),
        "invalid-log scripts must compile and fail at runtime:\n{text}"
    );

    let _child = spawn_nginx(&nginx, &prefix);
    wait_for_port(port, Duration::from_secs(5));

    let (status, response) = http_get(port, "/bad-level/");
    assert_eq!(
        status, 500,
        "invalid log level must fail the request; got:\n{response}"
    );
    let (status, response) = http_get(port, "/bad-nul/");
    assert_eq!(
        status, 500,
        "embedded NUL must fail the request; got:\n{response}"
    );
}

#[test]
fn access_phase_ctx_persists_across_two_sleeps() {
    let nginx = nginx_binary();
    let module = module_cdylib();
    let port = free_port();
    let prefix = isolated_prefix("ngx-rustscript-ctx");
    write_index(&prefix, "ctx/index.html", "ctx-ok\n");
    let script = prefix.join("conf/ctx_persist.rss");
    std::fs::copy(fixture("ctx_persist.rss"), &script).unwrap();
    let conf = meta_conf(
        &nginx_quote_path(&module).unwrap(),
        &prefix,
        port,
        &format!(
            r#"
        location /ctx/ {{
            access_by_rss_file {script};
        }}
"#,
            script = nginx_quote_path(&script).unwrap(),
        ),
    );
    write_conf(&prefix, &conf);
    let out = nginx_test(&nginx, &prefix);
    let text = combined(&out);
    assert!(
        out.status.success(),
        "ctx persist script must compile:\n{text}"
    );

    let _child = spawn_nginx(&nginx, &prefix);
    wait_for_port(port, Duration::from_secs(5));
    let (status, response) = http_get(port, "/ctx/");
    assert_eq!(status, 200, "ctx persist request failed:\n{response}");
    let log = fs::read_to_string(prefix.join("logs/error.log")).unwrap();
    assert!(
        log.contains("CTX_PERSIST=") && log.contains("n=2") && log.contains("k=v"),
        "missing persisted ctx in log:\n{log}"
    );
    assert!(
        log.contains("CTX_USER=") && log.contains("id=7") && log.contains("name=ada"),
        "missing nested ctx in log:\n{log}"
    );
}

#[test]
fn access_phase_ctx_sync_no_sleep_set_get_delete_clear() {
    let nginx = nginx_binary();
    let module = module_cdylib();
    let port = free_port();
    let prefix = isolated_prefix("ngx-rustscript-ctx-sync");
    write_index(&prefix, "ctx-sync/index.html", "ctx-sync-ok\n");
    let script = prefix.join("conf/ctx_sync.rss");
    std::fs::copy(fixture("ctx_sync.rss"), &script).unwrap();
    let conf = meta_conf(
        &nginx_quote_path(&module).unwrap(),
        &prefix,
        port,
        &format!(
            r#"
        location /ctx-sync/ {{
            access_by_rss_file {script};
        }}
"#,
            script = nginx_quote_path(&script).unwrap(),
        ),
    );
    write_conf(&prefix, &conf);
    let out = nginx_test(&nginx, &prefix);
    let text = combined(&out);
    assert!(
        out.status.success(),
        "ctx-only no-sleep script must compile:\n{text}"
    );

    let _child = spawn_nginx(&nginx, &prefix);
    wait_for_port(port, Duration::from_secs(5));
    let (status, response) = http_get(port, "/ctx-sync/");
    assert_eq!(status, 200, "ctx-only no-sleep request failed:\n{response}");
    let log = fs::read_to_string(prefix.join("logs/error.log")).unwrap();
    assert!(
        log.contains("CTX_SYNC=ok"),
        "missing CTX_SYNC=ok marker in log:\n{log}"
    );
}
