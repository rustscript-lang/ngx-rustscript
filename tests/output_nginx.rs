//! Real NGINX 1.30.4 lifecycle tests for content-phase output hosts.

mod support;

use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use ngx_rustscript_nginx_test_stubs as _;
use support::artifact::module_cdylib;
use support::nginx::{combined, nginx_binary, nginx_escape_quoted, nginx_quote_path, nginx_test};
use support::temp::{OwnedTempDir, isolated_prefix};

fn write_conf(prefix: &Path, body: &str) {
    fs::write(prefix.join("conf/nginx.conf"), body).unwrap();
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

fn wait_for_port(port: u16, timeout: Duration) {
    let start = Instant::now();
    loop {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return;
        }
        if start.elapsed() > timeout {
            panic!("nginx did not listen on {port} within {timeout:?}");
        }
        thread::sleep(Duration::from_millis(20));
    }
}

fn http_exchange(port: u16, request: &str) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(8)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(8)))
        .unwrap();
    stream.write_all(request.as_bytes()).unwrap();
    let _ = stream.flush();
    let mut body = Vec::new();
    let _ = stream.read_to_end(&mut body);
    String::from_utf8_lossy(&body).into_owned()
}

fn http_get10(port: u16, path: &str) -> String {
    http_exchange(
        port,
        &format!("GET {path} HTTP/1.0\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"),
    )
}

fn http_get11(port: u16, path: &str) -> String {
    http_exchange(
        port,
        &format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"),
    )
}

fn http_head(port: u16, path: &str) -> String {
    http_exchange(
        port,
        &format!("HEAD {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"),
    )
}

fn status_of(response: &str) -> u16 {
    response
        .lines()
        .find(|line| line.starts_with("HTTP/"))
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

fn http_body(response: &str) -> String {
    let Some((headers, body)) = response.split_once("\r\n\r\n") else {
        return String::new();
    };
    let headers_l = headers.to_ascii_lowercase();
    if headers_l.contains("transfer-encoding: chunked") {
        decode_chunked(body)
    } else {
        body.trim_end_matches('\0').to_string()
    }
}

fn decode_chunked(mut rest: &str) -> String {
    let mut out = String::new();
    while let Some((size_line, after)) = rest.split_once("\r\n") {
        let size = usize::from_str_radix(size_line.trim(), 16).unwrap_or(0);
        if size == 0 {
            break;
        }
        if after.len() < size {
            out.push_str(after);
            break;
        }
        out.push_str(&after[..size]);
        rest = after.get(size..).unwrap_or("");
        rest = rest.strip_prefix("\r\n").unwrap_or(rest);
    }
    out
}

fn http_conf(module: &Path, prefix: &Path, port: u16, locations: &str) -> String {
    let module = nginx_quote_path(module).unwrap();
    let pid = nginx_quote_path(&prefix.join("logs/nginx.pid")).unwrap();
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
    worker_connections 64;
}}

http {{
    access_log off;
    default_type text/plain;
    sendfile off;
    server {{
        listen 127.0.0.1:{port};
        server_name localhost;
{locations}
    }}
}}
"#
    )
}

fn loc(path: &str, script: &Path) -> String {
    let script = nginx_quote_path(script).unwrap();
    format!("        location {path} {{ content_by_rss_file {script}; }}\n")
}

fn start_with(locations: &str) -> (u16, OwnedTempDir, RunningNginx) {
    let nginx = nginx_binary();
    let module = module_cdylib();
    let prefix = isolated_prefix("output-nginx");
    let port = free_port();
    write_conf(&prefix, &http_conf(&module, &prefix, port, locations));
    let out = nginx_test(&nginx, &prefix);
    assert!(out.status.success(), "nginx -t failed:\n{}", combined(&out));
    let running = spawn_nginx(&nginx, &prefix);
    wait_for_port(port, Duration::from_secs(3));
    (port, prefix, running)
}

fn error_log(prefix: &Path) -> String {
    fs::read_to_string(prefix.join("logs/error.log")).unwrap_or_default()
}

#[test]
fn print_say_order_and_status_before_send() {
    let prefix = isolated_prefix("output-print");
    let script = prefix.join("html/print.rss");
    write_script(
        &script,
        r#"
use ngx;
ngx::status::set(201);
ngx::header::set("X-Out", "yes");
ngx::print("ab");
ngx::say("cd");
ngx::print(12);
"#,
    );
    let (port, keep, _nginx) = start_with(&loc("/p", &script));
    let response = http_get11(port, "/p");
    assert_eq!(
        status_of(&response),
        201,
        "{response}\nerror.log:\n{}",
        error_log(&keep)
    );
    assert!(response.contains("X-Out: yes"), "{response}");
    assert_eq!(http_body(&response), "abcd\n12");
}

#[test]
fn idempotent_headers_eof_and_mutation_after_eof() {
    let prefix = isolated_prefix("output-eof");
    let ok = prefix.join("html/ok.rss");
    write_script(
        &ok,
        r#"
use ngx;
ngx::send_headers();
ngx::send_headers();
ngx::print("z");
ngx::eof();
"#,
    );
    let bad = prefix.join("html/bad.rss");
    write_script(
        &bad,
        r#"
use ngx;
ngx::print("z");
ngx::eof();
ngx::print("nope");
"#,
    );
    let (port, _keep, _nginx) = start_with(&format!("{}{}", loc("/ok", &ok), loc("/bad", &bad)));
    let ok_resp = http_get11(port, "/ok");
    assert_eq!(status_of(&ok_resp), 200, "{ok_resp}");
    assert_eq!(http_body(&ok_resp), "z");
    let bad_resp = http_get11(port, "/bad");
    assert_eq!(status_of(&bad_resp), 200, "{bad_resp}");
    assert_eq!(
        http_body(&bad_resp),
        "z",
        "post-eof print must not append after last_buf: {bad_resp}"
    );
}

#[test]
fn http10_sets_content_length_and_head_skips_body() {
    let prefix = isolated_prefix("output-http10");
    let script = prefix.join("html/body.rss");
    write_script(
        &script,
        r#"
use ngx;
ngx::print("hello");
ngx::print("world");
"#,
    );
    let (port, keep, _nginx) = start_with(&loc("/b", &script));
    let get = http_get10(port, "/b");
    assert_eq!(status_of(&get), 200, "get={get:?} log={}", error_log(&keep));
    assert!(
        get.to_ascii_lowercase().contains("content-length: 10"),
        "HTTP/1.0 must set Content-Length, got {get}"
    );
    assert_eq!(http_body(&get), "helloworld");

    let head = http_head(port, "/b");
    assert_eq!(status_of(&head), 200, "{head}");
    assert!(
        http_body(&head).is_empty(),
        "HEAD must not include a body, got {head:?}"
    );
}

#[test]
fn concurrent_locations_map_independent_bodies() {
    let prefix = isolated_prefix("output-map");
    let a = prefix.join("html/a.rss");
    let b = prefix.join("html/b.rss");
    write_script(&a, "use ngx;\nngx::print(\"AAA\");\n");
    write_script(&b, "use ngx;\nngx::print(\"BBB\");\n");
    let (port, _keep, _nginx) = start_with(&format!("{}{}", loc("/a", &a), loc("/b", &b)));
    let ra = http_get11(port, "/a");
    let rb = http_get11(port, "/b");
    assert_eq!(http_body(&ra), "AAA", "{ra}");
    assert_eq!(http_body(&rb), "BBB", "{rb}");
}

#[test]
fn flush_false_and_true_complete() {
    let prefix = isolated_prefix("output-flush");
    let script = prefix.join("html/flush.rss");
    write_script(
        &script,
        r#"
use ngx;
ngx::print("one");
ngx::flush(false);
ngx::print("two");
ngx::flush(true);
ngx::print("three");
"#,
    );
    let (port, _keep, _nginx) = start_with(&loc("/f", &script));
    let response = http_get11(port, "/f");
    assert_eq!(status_of(&response), 200, "{response}");
    assert_eq!(http_body(&response), "onetwothree");
}

#[test]
fn slow_client_and_disconnect_and_shutdown() {
    let prefix = isolated_prefix("output-slow");
    let script = prefix.join("html/slow.rss");
    write_script(
        &script,
        r#"
use ngx;
ngx::print("0123456789");
ngx::flush(true);
ngx::print("abcdefghij");
"#,
    );
    let (port, keep, nginx) = start_with(&loc("/s", &script));
    let response = http_get11(port, "/s");
    assert_eq!(status_of(&response), 200, "{response}");
    assert_eq!(http_body(&response), "0123456789abcdefghij");

    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .write_all(b"GET /s HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")
        .unwrap();
    drop(stream);

    drop(nginx);
    drop(keep);
}

#[test]
fn access_phase_rejects_print_at_nginx_t() {
    let nginx = nginx_binary();
    let module = module_cdylib();
    let prefix = isolated_prefix("output-access-print");
    let script = prefix.join("html/print.rss");
    write_script(&script, "use ngx;\nngx::print(\"x\");\n");
    let script_q = nginx_quote_path(&script).unwrap();
    let module_q = nginx_quote_path(&module).unwrap();
    let pid = nginx_quote_path(&prefix.join("logs/nginx.pid")).unwrap();
    write_conf(
        &prefix,
        &format!(
            r#"
worker_processes 1;
error_log stderr emerg;
pid {pid};
daemon off;
master_process off;
load_module {module_q};

events {{
    worker_connections 8;
}}

http {{
    access_log off;
    server {{
        listen 127.0.0.1:18080;
        location / {{
            access_by_rss_file {script_q};
        }}
    }}
}}
"#
        ),
    );
    let out = nginx_test(&nginx, &prefix);
    let text = combined(&out);
    assert!(
        !out.status.success(),
        "print in access must fail nginx -t:\n{text}"
    );
    assert!(
        text.to_ascii_lowercase().contains("print") || text.to_ascii_lowercase().contains("phase"),
        "expected phase-forbidden print, got:\\n{text}"
    );
}

#[test]
fn http11_chunked_without_content_length() {
    let prefix = isolated_prefix("output-chunked");
    let script = prefix.join("html/chunked.rss");
    write_script(
        &script,
        r#"
use ngx;
ngx::print("hello");
ngx::say("world");
"#,
    );
    let (port, keep, _nginx) = start_with(&loc("/c", &script));
    let response = http_get11(port, "/c");
    assert_eq!(status_of(&response), 200, "{response}");
    assert!(
        response
            .to_ascii_lowercase()
            .contains("transfer-encoding: chunked"),
        "HTTP/1.1 without Content-Length must be chunked:\n{response}\n{}",
        error_log(&keep)
    );
    assert_eq!(http_body(&response), "helloworld\n");
}

#[test]
fn http10_explicit_content_length_is_preserved() {
    let prefix = isolated_prefix("output-http10-cl");
    let script = prefix.join("html/cl.rss");
    write_script(
        &script,
        r#"
use ngx;
ngx::header::set("Content-Length", "4");
ngx::print("ab");
ngx::print("cd");
"#,
    );
    let (port, keep, _nginx) = start_with(&loc("/cl", &script));
    let response = http_get10(port, "/cl");
    assert_eq!(status_of(&response), 200, "{response}");
    let headers = response
        .split("\r\n\r\n")
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    assert!(
        headers.contains("content-length: 4"),
        "must keep user Content-Length:\n{response}\n{}",
        error_log(&keep)
    );
    assert!(
        !headers.contains("transfer-encoding: chunked"),
        "HTTP/1.0 must not chunk:\n{response}"
    );
    assert_eq!(http_body(&response), "abcd");
}

#[test]
fn concurrent_requests_on_threads() {
    let prefix = isolated_prefix("output-conc");
    let a = prefix.join("html/a.rss");
    let b = prefix.join("html/b.rss");
    write_script(&a, "use ngx;\nngx::print(\"AAA\");\n");
    write_script(&b, "use ngx;\nngx::print(\"BBB\");\n");
    let locations = format!("{}{}", loc("/a", &a), loc("/b", &b));
    let (port, keep, _nginx) = start_with(&locations);
    let (ra, rb) = thread::scope(|scope| {
        let ta = scope.spawn(|| http_get11(port, "/a"));
        let tb = scope.spawn(|| http_get11(port, "/b"));
        (ta.join().unwrap(), tb.join().unwrap())
    });
    assert_eq!(status_of(&ra), 200, "{ra}\n{}", error_log(&keep));
    assert_eq!(status_of(&rb), 200, "{rb}\n{}", error_log(&keep));
    assert_eq!(http_body(&ra), "AAA");
    assert_eq!(http_body(&rb), "BBB");
}

#[test]
fn throttled_slow_client_flush_wait_completes() {
    let prefix = isolated_prefix("output-throttle");
    let script = prefix.join("html/slow.rss");
    write_script(
        &script,
        r#"
use ngx;
ngx::print("0123456789abcdefghij0123456789abcdefghij");
ngx::flush(true);
ngx::print("TAIL");
"#,
    );
    let loc = format!(
        "        location /t {{ limit_rate 32; content_by_rss_file {}; }}\n",
        nginx_quote_path(&script).unwrap()
    );
    let (port, keep, _nginx) = start_with(&loc);
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(8)))
        .unwrap();
    stream
        .write_all(b"GET /t HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
        .unwrap();
    let mut body = Vec::new();
    let mut buf = [0u8; 8];
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(8) {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => body.extend_from_slice(&buf[..n]),
            Err(_) => thread::sleep(Duration::from_millis(20)),
        }
    }
    let response = String::from_utf8_lossy(&body).into_owned();
    assert_eq!(
        status_of(&response),
        200,
        "{response}\n{}",
        error_log(&keep)
    );
    assert!(
        http_body(&response).contains("TAIL"),
        "slow client must still receive the full body:\n{response}"
    );
}

#[test]
fn sigquit_during_flush_wait_shuts_down() {
    let prefix = isolated_prefix("output-quit");
    let script = prefix.join("html/wait.rss");
    write_script(
        &script,
        r#"
use ngx;
ngx::print("WAIT");
ngx::flush(true);
ngx::print("AFTER");
"#,
    );
    let loc = format!(
        "        location /q {{ limit_rate 1; content_by_rss_file {}; }}\n",
        nginx_quote_path(&script).unwrap()
    );
    let (port, keep, mut nginx) = start_with(&loc);
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_millis(200)))
        .unwrap();
    stream
        .write_all(b"GET /q HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
        .unwrap();
    let mut buf = [0u8; 16];
    let _ = stream.read(&mut buf);
    let pid_path = keep.path().join("logs/nginx.pid");
    let pid = fs::read_to_string(&pid_path)
        .unwrap_or_default()
        .trim()
        .to_string();
    assert!(!pid.is_empty(), "missing nginx pid");
    let status = Command::new("kill")
        .args(["-QUIT", &pid])
        .status()
        .expect("kill -QUIT");
    assert!(status.success(), "kill -QUIT {pid} failed: {status}");
    drop(stream);
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(8) {
        if nginx.child.try_wait().ok().flatten().is_some() {
            return;
        }
        thread::sleep(Duration::from_millis(50));
    }
    panic!(
        "nginx did not exit after SIGQUIT; log:\\n{}",
        error_log(&keep)
    );
}

fn access_log(prefix: &Path) -> String {
    fs::read_to_string(prefix.join("logs/access.log")).unwrap_or_default()
}

fn wait_file_contains(path: &Path, needle: &str, timeout: Duration) -> String {
    let start = Instant::now();
    loop {
        let text = fs::read_to_string(path).unwrap_or_default();
        if text.contains(needle) {
            return text;
        }
        if start.elapsed() > timeout {
            panic!(
                "timed out waiting for {needle:?} in {} after {timeout:?}:\n{text}",
                path.display()
            );
        }
        thread::sleep(Duration::from_millis(20));
    }
}

fn wait_access_status(log: &Path, uri: &str, status: u16, timeout: Duration) -> String {
    let needle = format!("\"GET {uri} HTTP/");
    let status_token = format!("\" {status} ");
    let start = Instant::now();
    loop {
        let text = fs::read_to_string(log).unwrap_or_default();
        if text
            .lines()
            .any(|line| line.contains(&needle) && line.contains(&status_token))
        {
            return text;
        }
        if start.elapsed() > timeout {
            panic!(
                "timed out waiting for {uri} access-log {status} in {} after {timeout:?}:\n{text}",
                log.display()
            );
        }
        thread::sleep(Duration::from_millis(20));
    }
}

fn read_until_headers(stream: &mut TcpStream, timeout: Duration) -> Vec<u8> {
    stream
        .set_read_timeout(Some(Duration::from_millis(50)))
        .unwrap();
    let start = Instant::now();
    let mut buf = Vec::new();
    let mut tmp = [0u8; 512];
    while start.elapsed() < timeout {
        match stream.read(&mut tmp) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&tmp[..n]);
                if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    return buf;
                }
            }
            Err(err)
                if err.kind() == std::io::ErrorKind::WouldBlock
                    || err.kind() == std::io::ErrorKind::TimedOut =>
            {
                thread::sleep(Duration::from_millis(10));
            }
            Err(_) => break,
        }
    }
    buf
}

/// Client disconnect while `flush(wait=true)` is armed on a large buffered
/// response must log NGINX `$status` 499, restore handlers, and leave the
/// worker able to serve a later request. This is not the full-speed unread
/// drop in `slow_client_and_disconnect_and_shutdown`.
#[test]
fn flush_wait_client_disconnect_logs_499_and_serves_next() {
    let nginx = nginx_binary();
    let module = module_cdylib();
    let prefix = isolated_prefix("output-flush-wait-499");
    let wait_script = prefix.join("html/flush_wait.rss");
    let ok_script = prefix.join("html/ok.rss");
    let chunk = "X".repeat(32 * 1024);
    let mut source = String::from("use ngx;\n");
    for _ in 0..16 {
        source.push_str("ngx::print(\"");
        source.push_str(&chunk);
        source.push_str("\");\n");
    }
    source.push_str(
        "ngx::flush(true);\nngx::print(\"TAIL_SHOULD_NOT_COMPLETE\");\nngx::log(ngx::log_level::error(), \"FLUSH_WAIT_DONE\");\n",
    );
    write_script(&wait_script, &source);
    write_script(&ok_script, "use ngx;\nngx::print(\"ok\");\n");

    let port = free_port();
    let module_q = nginx_quote_path(&module).unwrap();
    let pid = nginx_quote_path(&prefix.join("logs/nginx.pid")).unwrap();
    let prefix_q = nginx_escape_quoted(prefix.to_str().unwrap()).unwrap();
    let wait_q = nginx_quote_path(&wait_script).unwrap();
    let ok_q = nginx_quote_path(&ok_script).unwrap();
    write_conf(
        &prefix,
        &format!(
            r#"
worker_processes 1;
error_log {prefix_q}/logs/error.log info;
pid {pid};
daemon off;
master_process on;
load_module {module_q};

events {{
    worker_connections 64;
}}

http {{
    access_log {prefix_q}/logs/access.log combined;
    default_type text/plain;
    sendfile off;
    tcp_nopush off;
    postpone_output 0;
    output_buffers 1 4k;
    server {{
        listen 127.0.0.1:{port} sndbuf=4k;
        server_name localhost;
        location /flush-wait/ {{
            content_by_rss_file {wait_q};
        }}
        location /ok/ {{
            content_by_rss_file {ok_q};
        }}
    }}
}}
"#
        ),
    );
    let out = nginx_test(&nginx, &prefix);
    assert!(out.status.success(), "nginx -t failed:\n{}", combined(&out));
    let _running = spawn_nginx(&nginx, &prefix);
    wait_for_port(port, Duration::from_secs(3));

    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    stream
        .write_all(b"GET /flush-wait/ HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
        .unwrap();
    let headers = read_until_headers(&mut stream, Duration::from_secs(2));
    let header_text = String::from_utf8_lossy(&headers);
    assert!(
        header_text.starts_with("HTTP/1.1 200") || header_text.starts_with("HTTP/1.0 200"),
        "headers must leave the server before flush wait; got {header_text:?}\n{}",
        error_log(&prefix)
    );
    let error_path = prefix.join("logs/error.log");
    let error_text = wait_file_contains(&error_path, "FLUSH_WAIT_ARMING", Duration::from_secs(2));
    assert!(
        !error_text.contains("FLUSH_WAIT_DONE"),
        "flush wait must still be armed:\n{error_text}"
    );
    let access_path = prefix.join("logs/access.log");
    let access_before = access_log(&prefix);
    assert!(
        !access_before.contains("/flush-wait/"),
        "request must still be in flight while flush wait is armed:\n{access_before}"
    );

    drop(stream);

    let access_after =
        wait_access_status(&access_path, "/flush-wait/", 499, Duration::from_secs(3));
    assert!(
        access_after.contains("GET /flush-wait/") && access_after.contains(" 499 "),
        "access log $status after headers+flush-wait disconnect must be 499:\n{access_after}\nerror:\n{}",
        error_log(&prefix)
    );
    assert!(
        !fs::read_to_string(&error_path)
            .unwrap_or_default()
            .contains("FLUSH_WAIT_DONE"),
        "disconnect during flush wait must not complete the tail print:\n{}",
        error_log(&prefix)
    );

    let ok = http_get11(port, "/ok/");
    assert_eq!(
        status_of(&ok),
        200,
        "handler/timer restoration must serve a later request:\n{ok}\nerror:\n{}",
        error_log(&prefix)
    );
    assert_eq!(http_body(&ok), "ok");
    let access_ok = wait_access_status(&access_path, "/ok/", 200, Duration::from_secs(3));
    assert!(
        access_ok.contains("GET /ok/") && access_ok.contains(" 200 "),
        "subsequent /ok/ must access-log 200:\n{access_ok}\nerror:\n{}",
        error_log(&prefix)
    );
}

#[test]
fn ordinary_print_say_flush_access_log_is_200() {
    let nginx = nginx_binary();
    let module = module_cdylib();
    let prefix = isolated_prefix("output-access-200");
    let script = prefix.join("html/out.rss");
    write_script(
        &script,
        r#"
use ngx;
ngx::print("ab");
ngx::say("cd");
ngx::flush(false);
ngx::print("ef");
ngx::flush(true);
"#,
    );
    let port = free_port();
    let module_q = nginx_quote_path(&module).unwrap();
    let pid = nginx_quote_path(&prefix.join("logs/nginx.pid")).unwrap();
    let prefix_q = nginx_escape_quoted(prefix.to_str().unwrap()).unwrap();
    let script_q = nginx_quote_path(&script).unwrap();
    write_conf(
        &prefix,
        &format!(
            r#"
worker_processes 1;
error_log {prefix_q}/logs/error.log info;
pid {pid};
daemon off;
master_process on;
load_module {module_q};

events {{
    worker_connections 64;
}}

http {{
    access_log {prefix_q}/logs/access.log combined;
    default_type text/plain;
    sendfile off;
    server {{
        listen 127.0.0.1:{port};
        server_name localhost;
        location /p/ {{
            content_by_rss_file {script_q};
        }}
    }}
}}
"#
        ),
    );
    let out = nginx_test(&nginx, &prefix);
    assert!(out.status.success(), "nginx -t failed:\n{}", combined(&out));
    let _running = spawn_nginx(&nginx, &prefix);
    wait_for_port(port, Duration::from_secs(3));

    let response = http_get11(port, "/p/");
    assert_eq!(
        status_of(&response),
        200,
        "{response}\nerror.log:\n{}",
        error_log(&prefix)
    );
    assert_eq!(http_body(&response), "abcd\nef");
    let access = wait_access_status(
        &prefix.join("logs/access.log"),
        "/p/",
        200,
        Duration::from_secs(3),
    );
    assert!(
        access.contains("GET /p/") && access.contains(" 200 ") && !access.contains(" 499 "),
        "ordinary print/say/flush must access-log 200:\n{access}\nerror:\n{}",
        error_log(&prefix)
    );
}
