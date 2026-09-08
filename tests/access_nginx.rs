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
