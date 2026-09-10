//! Cfg-isolated body-read wait and main-count lease proof against pinned NGINX.

mod support;

use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use ngx_rustscript_nginx_test_stubs as _;
use support::hooks::hook_enabled_debug_cdylib;
use support::nginx::{
    combined, nginx_binary, nginx_process_group_command, nginx_quote_path, nginx_test,
    terminate_process_group,
};
use support::temp::isolated_prefix;

const DELAYED_BODY: &str = "hello-async-body";
const DELAYED_SPLIT: usize = 7;
const CONTENT_DELAYED_URI: &str = "/echo/?tok=content-delayed";
const ACCESS_DELAYED_URI: &str = "/access-echo/?tok=access-delayed";
const CLOSED_499_URI: &str = "/slow/?tok=closed-499";
const TIMEOUT_408_URI: &str = "/timeout/?tok=timeout-408";
const CONTENT_DELAYED_TOKEN: &str = "tok=content-delayed";
const ACCESS_DELAYED_TOKEN: &str = "tok=access-delayed";
const CLOSED_499_TOKEN: &str = "tok=closed-499";
const TIMEOUT_408_TOKEN: &str = "tok=timeout-408";

struct RunningNginx {
    child: Child,
}

impl Drop for RunningNginx {
    fn drop(&mut self) {
        terminate_process_group(&mut self.child, Duration::from_secs(2));
    }
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
        if start.elapsed() > timeout {
            panic!("nginx did not listen on {port} within {timeout:?}");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn inject_main_env(conf: &str, vars: &[(&str, &str)]) -> String {
    let mut env_lines = String::new();
    for (key, value) in vars {
        env_lines.push_str(&format!("env {key}={value};\n"));
    }
    format!("{env_lines}{conf}")
}

fn write_script(path: &Path, body: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, body).unwrap();
}

fn parse_http_response(raw: &str) -> (u16, String) {
    let status = raw
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let (headers, body) = raw.split_once("\r\n\r\n").unwrap_or((raw, ""));
    let body = body.trim_end_matches('\0');
    let payload = if headers
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
        || looks_chunked_body(body)
    {
        decode_chunked_body(body)
    } else {
        body.to_string()
    };
    (status, payload)
}

fn looks_chunked_body(body: &str) -> bool {
    body.split_once("\r\n")
        .and_then(|(line, _)| usize::from_str_radix(line.trim(), 16).ok())
        .is_some()
}

fn decode_chunked_body(body: &str) -> String {
    let mut out = String::new();
    let mut rest = body;
    loop {
        let Some((size_line, after)) = rest.split_once("\r\n") else {
            break;
        };
        let Ok(size) = usize::from_str_radix(size_line.trim(), 16) else {
            break;
        };
        if size == 0 {
            break;
        }
        if after.len() < size {
            out.push_str(after);
            break;
        }
        out.push_str(&after[..size]);
        rest = &after[size..];
        if let Some(stripped) = rest.strip_prefix("\r\n") {
            rest = stripped;
        }
    }
    out
}

fn http_exchange(port: u16, request: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(8)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(8)))
        .unwrap();
    stream.write_all(request.as_bytes()).unwrap();
    let _ = stream.shutdown(std::net::Shutdown::Write);
    let mut buf = Vec::new();
    let _ = stream.read_to_end(&mut buf);
    let raw = String::from_utf8_lossy(&buf);
    let (status, _) = parse_http_response(&raw);
    (status, raw.into_owned())
}

fn log_len(path: &Path) -> usize {
    fs::read_to_string(path).map(|s| s.len()).unwrap_or(0)
}

fn wait_log_tail_contains(log: &Path, start_len: usize, needle: &str, timeout: Duration) -> String {
    let start = Instant::now();
    loop {
        let text = fs::read_to_string(log).unwrap_or_default();
        let tail = if text.len() >= start_len {
            &text[start_len..]
        } else {
            text.as_str()
        };
        if tail.contains(needle) {
            return tail.to_string();
        }
        if start.elapsed() >= timeout {
            return tail.to_string();
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn wait_access_status(log: &Path, method: &str, uri: &str, status: u16, timeout: Duration) -> bool {
    let needle = format!("\"{method} {uri} ");
    let status_tok = format!(" {status} ");
    let start = Instant::now();
    while start.elapsed() < timeout {
        if let Ok(text) = fs::read_to_string(log) {
            if text
                .lines()
                .any(|line| line.contains(&needle) && line.contains(&status_tok))
            {
                return true;
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

#[derive(Debug)]
struct CountProof {
    stage: String,
    count: u32,
    extra: String,
    uri: String,
}

#[derive(Clone, Copy)]
struct ExpectedCount {
    stage: &'static str,
    count: u32,
    extra: Option<&'static str>,
}

fn request_uri_from_line(line: &str) -> String {
    if let Some(rest) = line.split("request: \"").nth(1) {
        let req = rest.split('"').next().unwrap_or("");
        let mut parts = req.split_whitespace();
        let _method = parts.next();
        if let Some(uri) = parts.next() {
            return uri.to_string();
        }
    }
    String::new()
}

fn parse_count_proofs(text: &str) -> Vec<CountProof> {
    let mut out = Vec::new();
    for line in text.lines() {
        let Some(rest) = line.split("BODY_COUNT_PROOF ").nth(1) else {
            continue;
        };
        let mut stage = String::new();
        let mut count = 0u32;
        let mut extra = String::new();
        let mut uri = request_uri_from_line(line);
        for tok in rest.split_whitespace() {
            let tok = tok.trim_end_matches(',');
            if let Some(value) = tok.strip_prefix("stage=") {
                stage = value.to_string();
            } else if let Some(value) = tok.strip_prefix("count=") {
                count = value.parse().unwrap_or(0);
            } else if let Some(value) = tok.strip_prefix("uri=") {
                if uri.is_empty() {
                    uri = value.to_string();
                }
            } else if tok.starts_with("rc=") || !tok.contains(':') {
                if !extra.is_empty() {
                    extra.push(' ');
                }
                extra.push_str(tok);
            }
        }
        out.push(CountProof {
            stage,
            count,
            extra,
            uri,
        });
    }
    out
}

fn format_proofs(proofs: &[&CountProof]) -> Vec<String> {
    proofs
        .iter()
        .map(|p| format!("{} count={} uri={} {}", p.stage, p.count, p.uri, p.extra))
        .collect()
}

fn proofs_for_token<'a>(proofs: &'a [CountProof], token: &str) -> Vec<&'a CountProof> {
    let scoped: Vec<&'a CountProof> = proofs
        .iter()
        .filter(|proof| proof.uri.contains(token))
        .collect();
    assert!(
        !scoped.is_empty(),
        "missing BODY_COUNT_PROOF lines for token {token} in {:?}",
        proofs
            .iter()
            .map(|p| format!("{} count={} uri={} {}", p.stage, p.count, p.uri, p.extra))
            .collect::<Vec<_>>()
    );
    let uri = &scoped[0].uri;
    assert!(
        scoped.iter().all(|proof| proof.uri == *uri),
        "token {token} mixed request identities: {:?}",
        format_proofs(&scoped)
    );
    for other in [
        CONTENT_DELAYED_TOKEN,
        ACCESS_DELAYED_TOKEN,
        CLOSED_499_TOKEN,
        TIMEOUT_408_TOKEN,
    ] {
        if other == token {
            continue;
        }
        assert!(
            scoped.iter().all(|proof| !proof.uri.contains(other)),
            "token {token} uri={} must not also contain {other}: {:?}",
            uri,
            format_proofs(&scoped)
        );
        assert!(
            proofs
                .iter()
                .filter(|proof| proof.uri.contains(other))
                .all(|proof| !proof.uri.contains(token)),
            "token {token} must not share a URI with {other}"
        );
    }
    scoped
}

fn assert_no_underflow_or_leak(proofs: &[&CountProof]) {
    for proof in proofs {
        assert!(
            proof.count >= 1,
            "main count must not underflow at {} count={} uri={} {}",
            proof.stage,
            proof.count,
            proof.uri,
            proof.extra
        );
        assert!(
            proof.count <= 2,
            "isolated main request count must stay in {{1,2}} at {} count={} uri={} {}",
            proof.stage,
            proof.count,
            proof.uri,
            proof.extra
        );
    }
}

fn assert_ordered_counts(proofs: &[&CountProof], expected: &[ExpectedCount]) {
    let mut start = 0usize;
    for exp in expected {
        let rel = proofs[start..]
            .iter()
            .position(|proof| {
                proof.stage == exp.stage
                    && exp
                        .extra
                        .map(|extra| proof.extra.contains(extra))
                        .unwrap_or(true)
            })
            .unwrap_or_else(|| {
                panic!(
                    "missing ordered BODY_COUNT_PROOF stage={} extra={:?} from index {start} in {:?}",
                    exp.stage,
                    exp.extra,
                    format_proofs(proofs)
                )
            });
        let proof = proofs[start + rel];
        assert_eq!(
            proof.count, exp.count,
            "stage {} uri={} extra={} count={} expected {}",
            proof.stage, proof.uri, proof.extra, proof.count, exp.count
        );
        start += rel + 1;
    }
}

fn assert_content_delayed_counts(proofs: &[CountProof], token: &str) {
    let scoped = proofs_for_token(proofs, token);
    assert_no_underflow_or_leak(&scoped);
    // Isolated main request starts at count=1. ngx_http_read_client_request_body
    // always does r->main->count++ (nginx 1.30.4 ngx_http_request_body.c), so
    // NGX_AGAIN is observed at 2. Manual undo returns 1. Content NGX_DONE takes
    // the extra lease (2); ngx_http_core_content_phase then
    // ngx_http_finalize_request(NGX_DONE) -> ngx_http_close_request count-- to 1
    // before callback/resume/terminal.
    assert_ordered_counts(
        &scoped,
        &[
            ExpectedCount {
                stage: "core_read",
                count: 2,
                extra: Some("AGAIN"),
            },
            ExpectedCount {
                stage: "undo",
                count: 1,
                extra: None,
            },
            ExpectedCount {
                stage: "wait_armed",
                count: 1,
                extra: None,
            },
            ExpectedCount {
                stage: "content_suspend",
                count: 2,
                extra: None,
            },
            ExpectedCount {
                stage: "callback",
                count: 1,
                extra: None,
            },
            ExpectedCount {
                stage: "content_resume",
                count: 1,
                extra: None,
            },
        ],
    );
}

fn assert_access_delayed_counts(proofs: &[CountProof], token: &str) {
    let scoped = proofs_for_token(proofs, token);
    assert_no_underflow_or_leak(&scoped);
    // Access returns NGX_AGAIN without the extra count lease, so suspend and
    // later callback/resume/terminal stay at the undone base count 1.
    assert_ordered_counts(
        &scoped,
        &[
            ExpectedCount {
                stage: "core_read",
                count: 2,
                extra: Some("AGAIN"),
            },
            ExpectedCount {
                stage: "undo",
                count: 1,
                extra: None,
            },
            ExpectedCount {
                stage: "wait_armed",
                count: 1,
                extra: None,
            },
            ExpectedCount {
                stage: "access_suspend",
                count: 1,
                extra: None,
            },
            ExpectedCount {
                stage: "callback",
                count: 1,
                extra: None,
            },
            ExpectedCount {
                stage: "access_resume",
                count: 1,
                extra: None,
            },
        ],
    );
}

fn assert_content_closed_499_counts(proofs: &[CountProof], token: &str) {
    let scoped = proofs_for_token(proofs, token);
    assert_no_underflow_or_leak(&scoped);
    assert_ordered_counts(
        &scoped,
        &[
            ExpectedCount {
                stage: "core_read",
                count: 2,
                extra: Some("AGAIN"),
            },
            ExpectedCount {
                stage: "undo",
                count: 1,
                extra: None,
            },
            ExpectedCount {
                stage: "wait_armed",
                count: 1,
                extra: None,
            },
            ExpectedCount {
                stage: "content_suspend",
                count: 2,
                extra: None,
            },
            ExpectedCount {
                stage: "closed_499",
                count: 1,
                extra: None,
            },
        ],
    );
}

fn assert_content_timeout_408_counts(proofs: &[CountProof], token: &str) {
    let scoped = proofs_for_token(proofs, token);
    assert_no_underflow_or_leak(&scoped);
    assert_ordered_counts(
        &scoped,
        &[
            ExpectedCount {
                stage: "core_read",
                count: 2,
                extra: Some("AGAIN"),
            },
            ExpectedCount {
                stage: "undo",
                count: 1,
                extra: None,
            },
            ExpectedCount {
                stage: "wait_armed",
                count: 1,
                extra: None,
            },
            ExpectedCount {
                stage: "content_suspend",
                count: 2,
                extra: None,
            },
            ExpectedCount {
                stage: "continue_read",
                count: 1,
                extra: None,
            },
        ],
    );
}

fn delayed_body_post(
    port: u16,
    path: &str,
    body: &str,
    split: usize,
    wait_armed: impl FnOnce(),
) -> (u16, String) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(8)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(8)))
        .unwrap();
    let headers = format!(
        "POST {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(headers.as_bytes()).unwrap();
    stream.write_all(&body.as_bytes()[..split]).unwrap();
    let _ = stream.flush();
    wait_armed();
    stream.write_all(&body.as_bytes()[split..]).unwrap();
    let _ = stream.flush();
    let mut buf = Vec::new();
    let _ = stream.read_to_end(&mut buf);
    let raw = String::from_utf8_lossy(&buf).into_owned();
    let (status, _) = parse_http_response(&raw);
    (status, raw)
}

fn follow_up_echo(port: u16, error_log: &Path) {
    let (status, response) = http_exchange(
        port,
        "POST /echo/ HTTP/1.0\r\nHost: 127.0.0.1\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
    );
    let (_, payload) = parse_http_response(&response);
    assert_eq!(
        status,
        200,
        "follow-up must stay healthy: {response}\nerror.log={}",
        fs::read_to_string(error_log).unwrap_or_default()
    );
    assert_eq!(payload, "ok\n", "follow-up payload: {payload:?}");
}

fn body_conf(module: &Path, prefix: &Path, port: u16, extra: &str) -> String {
    format!(
        r#"
worker_processes 1;
error_log "{error_log}" info;
pid {pid};
daemon off;
master_process on;
load_module {module};

events {{
    worker_connections 64;
}}

http {{
    access_log {access};
    default_type text/plain;
    client_body_timeout 2s;
    client_max_body_size 32;
    server {{
        listen 127.0.0.1:{port};
        server_name localhost;
        root {html};
        {extra}
    }}
}}
"#,
        error_log = prefix.join("logs/error.log").display(),
        pid = nginx_quote_path(&prefix.join("logs/nginx.pid")).unwrap(),
        module = nginx_quote_path(module).unwrap(),
        access = nginx_quote_path(&prefix.join("logs/access.log")).unwrap(),
        html = nginx_quote_path(&prefix.join("html")).unwrap(),
    )
}

#[test]
fn nginx_delayed_body_success_and_main_count_lease() {
    let nginx = nginx_binary();
    let module = hook_enabled_debug_cdylib();
    let port = free_port();
    let prefix = isolated_prefix("ngx-rustscript-body-hooks");
    fs::create_dir_all(prefix.join("html")).unwrap();
    fs::write(prefix.join("html/index.html"), "fallback\n").unwrap();

    let echo = prefix.join("conf/body_echo.rss");
    write_script(
        &echo,
        r#"
use ngx;
ngx::req::read_body();
ngx::say(ngx::req::get_body_data());
"#,
    );
    let access_read = prefix.join("conf/body_access.rss");
    write_script(
        &access_read,
        r#"
use ngx;
ngx::req::read_body();
ngx::ctx::set("body", ngx::req::get_body_data());
"#,
    );
    let content_see = prefix.join("conf/body_content.rss");
    write_script(
        &content_see,
        r#"
use ngx;
ngx::say(ngx::ctx::get("body"));
"#,
    );

    let echo_q = nginx_quote_path(&echo).unwrap();
    let access_q = nginx_quote_path(&access_read).unwrap();
    let content_q = nginx_quote_path(&content_see).unwrap();
    let locations = format!(
        r#"
        location /echo/ {{
            client_body_timeout 10s;
            content_by_rss_file {echo_q};
        }}
        location /access-echo/ {{
            client_body_timeout 10s;
            access_by_rss_file {access_q};
            content_by_rss_file {content_q};
        }}
        location /slow/ {{
            content_by_rss_file {echo_q};
        }}
        location /timeout/ {{
            client_body_timeout 1s;
            content_by_rss_file {echo_q};
        }}
"#
    );
    let conf = inject_main_env(
        &body_conf(module, &prefix, port, &locations),
        &[("NGX_RUSTSCRIPT_BODY_COUNT_PROOF", "1")],
    );
    fs::write(prefix.join("conf/nginx.conf"), &conf).unwrap();

    let out = nginx_test(&nginx, prefix.path());
    let text = combined(&out);
    assert!(out.status.success(), "nginx -t failed:\n{text}");

    let mut running = spawn_nginx(&nginx, prefix.path());
    wait_for_port(port, Duration::from_secs(5));

    let error_log = prefix.join("logs/error.log");
    let access_log = prefix.join("logs/access.log");
    let expected = format!("{DELAYED_BODY}\n");

    let start = log_len(&error_log);
    let (status, response) = delayed_body_post(
        port,
        CONTENT_DELAYED_URI,
        DELAYED_BODY,
        DELAYED_SPLIT,
        || {
            let tail = wait_log_tail_contains(
                &error_log,
                start,
                CONTENT_DELAYED_TOKEN,
                Duration::from_secs(4),
            );
            assert!(
                tail.contains("BODY_WAIT_ARMED")
                    && tail.contains("BODY_COUNT_PROOF stage=content_suspend")
                    && tail.contains(CONTENT_DELAYED_TOKEN),
                "delayed content body must arm wait after NGX_AGAIN, not wall-clock:\n{tail}"
            );
        },
    );
    let (_, payload) = parse_http_response(&response);
    let content_tail = wait_log_tail_contains(
        &error_log,
        start,
        "BODY_COUNT_PROOF stage=content_resume",
        Duration::from_secs(4),
    );
    assert_eq!(
        status, 200,
        "delayed content body must complete 200: {response}\n{content_tail}"
    );
    assert_eq!(payload, expected, "delayed content payload: {payload:?}");
    let proofs = parse_count_proofs(&content_tail);
    assert_content_delayed_counts(&proofs, CONTENT_DELAYED_TOKEN);
    follow_up_echo(port, &error_log);

    let start = log_len(&error_log);
    let (status, response) = delayed_body_post(
        port,
        ACCESS_DELAYED_URI,
        DELAYED_BODY,
        DELAYED_SPLIT,
        || {
            let tail = wait_log_tail_contains(
                &error_log,
                start,
                ACCESS_DELAYED_TOKEN,
                Duration::from_secs(4),
            );
            assert!(
                tail.contains("BODY_WAIT_ARMED")
                    && tail.contains("BODY_COUNT_PROOF stage=access_suspend")
                    && tail.contains(ACCESS_DELAYED_TOKEN),
                "delayed access body must arm wait after NGX_AGAIN:\n{tail}"
            );
        },
    );
    let (_, payload) = parse_http_response(&response);
    let access_tail = wait_log_tail_contains(
        &error_log,
        start,
        "BODY_COUNT_PROOF stage=access_resume",
        Duration::from_secs(4),
    );
    assert_eq!(
        status, 200,
        "delayed access body must complete 200: {response}\n{access_tail}"
    );
    assert_eq!(payload, expected, "delayed access payload: {payload:?}");
    let proofs = parse_count_proofs(&access_tail);
    assert_access_delayed_counts(&proofs, ACCESS_DELAYED_TOKEN);
    follow_up_echo(port, &error_log);

    let start = log_len(&error_log);
    let mut disc = TcpStream::connect(("127.0.0.1", port)).unwrap();
    disc.write_all(
        format!(
            "POST {CLOSED_499_URI} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 16\r\nConnection: close\r\n\r\npartial"
        )
        .as_bytes(),
    )
    .unwrap();
    let _ = disc.flush();
    let armed = wait_log_tail_contains(&error_log, start, CLOSED_499_TOKEN, Duration::from_secs(4));
    assert!(
        armed.contains("BODY_WAIT_ARMED")
            && armed.contains("BODY_COUNT_PROOF stage=content_suspend")
            && armed.contains(CLOSED_499_TOKEN),
        "499 path must arm wait before disconnect:\n{armed}"
    );
    drop(disc);
    assert!(
        wait_access_status(
            &access_log,
            "POST",
            CLOSED_499_URI,
            499,
            Duration::from_secs(8)
        ),
        "disconnect during body wait must log 499:\n{}",
        fs::read_to_string(&access_log).unwrap_or_default()
    );
    let closed = wait_log_tail_contains(
        &error_log,
        start,
        "BODY_COUNT_PROOF stage=closed_499",
        Duration::from_secs(4),
    );
    let proofs = parse_count_proofs(&closed);
    assert_content_closed_499_counts(&proofs, CLOSED_499_TOKEN);
    follow_up_echo(port, &error_log);

    let start = log_len(&error_log);
    let mut timed = TcpStream::connect(("127.0.0.1", port)).unwrap();
    timed
        .write_all(
            format!(
                "POST {TIMEOUT_408_URI} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 16\r\nConnection: close\r\n\r\n"
            )
            .as_bytes(),
        )
        .unwrap();
    let _ = timed.flush();
    let armed =
        wait_log_tail_contains(&error_log, start, TIMEOUT_408_TOKEN, Duration::from_secs(4));
    assert!(
        armed.contains("BODY_WAIT_ARMED")
            && armed.contains("BODY_COUNT_PROOF stage=content_suspend")
            && armed.contains(TIMEOUT_408_TOKEN),
        "408 path must arm wait before timeout:\n{armed}"
    );
    assert!(
        wait_access_status(
            &access_log,
            "POST",
            TIMEOUT_408_URI,
            408,
            Duration::from_secs(8)
        ),
        "body wait timeout must log 408:\n{}",
        fs::read_to_string(&access_log).unwrap_or_default()
    );
    let timeout_tail = wait_log_tail_contains(
        &error_log,
        start,
        "BODY_COUNT_PROOF stage=continue_read",
        Duration::from_secs(4),
    );
    let proofs = parse_count_proofs(&timeout_tail);
    assert_content_timeout_408_counts(&proofs, TIMEOUT_408_TOKEN);
    drop(timed);
    follow_up_echo(port, &error_log);

    quit_and_wait(&mut running.child, Duration::from_secs(5));
}

#[test]
fn hook_enabled_module_contains_body_count_proof_needles() {
    let bytes = fs::read(hook_enabled_debug_cdylib()).expect("read hook module");
    for needle in [
        "NGX_RUSTSCRIPT_BODY_COUNT_PROOF",
        "BODY_COUNT_PROOF",
        "BODY_WAIT_ARMED",
    ] {
        assert!(
            bytes.windows(needle.len()).any(|w| w == needle.as_bytes()),
            "hook-enabled module must contain {needle}"
        );
    }
}
