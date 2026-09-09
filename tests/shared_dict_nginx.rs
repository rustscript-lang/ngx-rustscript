//! Real multi-worker NGINX verification for the shared dictionary core.

mod support;

use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use support::artifact::module_cdylib;
use support::nginx::{combined, nginx_binary, nginx_quote_path, nginx_test};
use support::temp::{OwnedTempDir, isolated_prefix};

struct RunningNginx {
    child: Child,
    prefix: PathBuf,
    nginx: PathBuf,
}

impl Drop for RunningNginx {
    fn drop(&mut self) {
        let _ = Command::new(&self.nginx)
            .arg("-s")
            .arg("quit")
            .arg("-p")
            .arg(&self.prefix)
            .arg("-c")
            .arg("conf/nginx.conf")
            .output();
        let _ = self.child.wait();
        let _ = self.child.kill();
    }
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

fn conf_body(module: &Path, prefix: &Path, port: u16, locations: &str) -> String {
    let module = nginx_quote_path(module).unwrap();
    let pid = nginx_quote_path(&prefix.join("logs/nginx.pid")).unwrap();
    let error_log = nginx_quote_path(&prefix.join("logs/error.log")).unwrap();
    format!(
        r#"
worker_processes 2;
error_log {error_log} info;
pid {pid};
daemon off;
master_process on;
load_module {module};

events {{
    worker_connections 64;
}}

http {{
    access_log off;
    rss_shared_dict dogs 1m;
    rss_shared_dict cats 1m;
    server {{
        listen 127.0.0.1:{port};
        server_name localhost;
{locations}
    }}
}}
"#
    )
}

fn location(path: &str, script: &Path) -> String {
    let script = nginx_quote_path(script).unwrap();
    format!(
        r#"
        location {path} {{
            access_by_rss_file {script};
        }}
"#
    )
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
        child,
        prefix: prefix.to_path_buf(),
        nginx: nginx.to_path_buf(),
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

fn header_value(response: &str, name: &str) -> Option<String> {
    let needle = format!("{name}:");
    for line in response.lines() {
        if line.len() >= needle.len() && line[..needle.len()].eq_ignore_ascii_case(&needle) {
            return Some(line[needle.len()..].trim().to_string());
        }
    }
    None
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

fn setup_real() -> (PathBuf, u16, OwnedTempDir, RunningNginx) {
    let nginx = nginx_binary();
    let module = module_cdylib();
    let tmp = isolated_prefix("shared-dict-nginx");
    let port = free_port();
    let scripts = tmp.path().join("html");
    fs::create_dir_all(&scripts).unwrap();
    write_script(
        &scripts.join("set.rss"),
        r#"
use ngx;
let d = ngx::shared::dict("dogs");
ngx::shared::set(d, "Jim", "8", null, null);
ngx::header::set("X-Ok", "1");
ngx::exit(200);
"#,
    );
    write_script(
        &scripts.join("get.rss"),
        r#"
use ngx;
let d = ngx::shared::dict("dogs");
let r = ngx::shared::get(d, "Jim");
ngx::header::set("X-Value", r["value"]);
ngx::exit(200);
"#,
    );
    write_script(
        &scripts.join("incr.rss"),
        r#"
use ngx;
let d = ngx::shared::dict("dogs");
let r = ngx::shared::incr(d, "hits", 1, 0, null);
ngx::header::set("X-Value", ngx::encode_args({ n: r["value"] }));
ngx::exit(200);
"#,
    );
    write_script(
        &scripts.join("read_hits.rss"),
        r#"
use ngx;
let d = ngx::shared::dict("dogs");
let r = ngx::shared::get(d, "hits");
ngx::header::set("X-Value", ngx::encode_args({ n: r["value"] }));
ngx::exit(200);
"#,
    );
    write_script(
        &scripts.join("types.rss"),
        r#"
use ngx;
let d = ngx::shared::dict("dogs");
ngx::shared::set(d, "b", true, null, 3);
ngx::shared::set(d, "i", 42, null, null);
ngx::shared::set(d, "f", 1.5, null, null);
ngx::shared::set(d, "s", "hi", null, null);
let gb = ngx::shared::get(d, "b");
let gi = ngx::shared::get(d, "i");
let gf = ngx::shared::get(d, "f");
let gs = ngx::shared::get(d, "s");
if gb["value"] {
    ngx::header::set("X-Bool", "1");
} else {
    ngx::header::set("X-Bool", "0");
}
ngx::header::set("X-Flags", ngx::encode_args({ n: gb["flags"] }));
ngx::header::set("X-Int", ngx::encode_args({ n: gi["value"] }));
ngx::header::set("X-Float", ngx::encode_args({ n: gf["value"] }));
ngx::header::set("X-Str", gs["value"]);
ngx::exit(200);
"#,
    );
    write_script(
        &scripts.join("add_replace.rss"),
        r#"
use ngx;
let d = ngx::shared::dict("dogs");
let add1 = ngx::shared::add(d, "once", "a", null, null);
let add2 = ngx::shared::add(d, "once", "b", null, null);
let rep = ngx::shared::replace(d, "once", "c", null, null);
let niladd = ngx::shared::add(d, "nilk", null, null, null);
if add1["ok"] {
    ngx::header::set("X-Add1", "1");
} else {
    ngx::header::set("X-Add1", "0");
}
ngx::header::set("X-Add2", add2["error"]);
ngx::header::set("X-Rep", ngx::shared::get(d, "once")["value"]);
ngx::header::set("X-Nil", niladd["error"]);
ngx::exit(200);
"#,
    );
    write_script(
        &scripts.join("ttl.rss"),
        r#"
use ngx;
let d = ngx::shared::dict("dogs");
ngx::shared::set(d, "ttlkey", "v", 30, null);
let t = ngx::shared::ttl(d, "ttlkey");
ngx::header::set("X-Ttl", ngx::encode_args({ n: t["value"] }));
ngx::exit(200);
"#,
    );
    write_script(
        &scripts.join("space.rss"),
        r#"
use ngx;
let d = ngx::shared::dict("dogs");
let cap = ngx::shared::capacity(d);
let free = ngx::shared::free_space(d);
ngx::header::set("X-Cap", ngx::encode_args({ n: cap }));
ngx::header::set("X-Free", ngx::encode_args({ n: free }));
ngx::exit(200);
"#,
    );
    write_script(
        &scripts.join("cats.rss"),
        r#"
use ngx;
let dogs = ngx::shared::dict("dogs");
let cats = ngx::shared::dict("cats");
ngx::shared::set(dogs, "k", "woof", null, null);
ngx::shared::set(cats, "k", "meow", null, null);
ngx::header::set("X-Dogs", ngx::shared::get(dogs, "k")["value"]);
ngx::header::set("X-Cats", ngx::shared::get(cats, "k")["value"]);
ngx::exit(200);
"#,
    );
    write_script(
        &scripts.join("empty.rss"),
        r#"
use ngx;
let d = ngx::shared::dict("dogs");
let r = ngx::shared::set(d, "", "x", null, null);
ngx::header::set("X-Err", r["error"]);
ngx::exit(200);
"#,
    );
    let locations = format!(
        "{}{}{}{}{}{}{}{}{}",
        location("/set", &scripts.join("set.rss")),
        location("/get", &scripts.join("get.rss")),
        location("/incr", &scripts.join("incr.rss")),
        location("/hits", &scripts.join("read_hits.rss")),
        location("/types", &scripts.join("types.rss")),
        location("/addrep", &scripts.join("add_replace.rss")),
        location("/ttl", &scripts.join("ttl.rss")),
        location("/space", &scripts.join("space.rss")),
        location("/cats", &scripts.join("cats.rss")),
    );
    // empty-key location
    let locations = format!(
        "{}{}",
        locations,
        location("/empty", &scripts.join("empty.rss")),
    );
    write_conf(
        tmp.path(),
        &conf_body(&module, tmp.path(), port, &locations),
    );
    let out = nginx_test(&nginx, tmp.path());
    assert!(
        out.status.success(),
        "shared dict nginx -t failed: {}",
        combined(&out)
    );
    let running = spawn_nginx(&nginx, tmp.path());
    wait_for_port(port, Duration::from_secs(5));
    (nginx, port, tmp, running)
}

#[test]
fn multi_worker_set_is_visible_on_get() {
    let (_nginx, port, _tmp, _running) = setup_real();
    let (status, body) = http_get(port, "/set");
    assert_eq!(status, 200, "set failed: {body}\n{}", error_log(&_tmp));
    for _ in 0..32 {
        let (status, body) = http_get(port, "/get");
        assert_eq!(status, 200, "get failed: {body}\n{}", error_log(&_tmp));
        let value = header_value(&body, "X-Value")
            .unwrap_or_else(|| panic!("X-Value missing: {body}\n{}", error_log(&_tmp)));
        assert_eq!(value, "8", "cross-worker get lost the value: {body}");
    }
}

#[test]
fn multi_worker_incr_is_atomic() {
    let (_nginx, port, _tmp, _running) = setup_real();
    let n = 40;
    let done = AtomicUsize::new(0);
    thread::scope(|scope| {
        for _ in 0..n {
            scope.spawn(|| {
                let (status, body) = http_get(port, "/incr");
                assert_eq!(status, 200, "incr failed: {body}");
                done.fetch_add(1, Ordering::SeqCst);
            });
        }
    });
    assert_eq!(done.load(Ordering::SeqCst), n);
    let (status, body) = http_get(port, "/hits");
    assert_eq!(status, 200, "read hits failed: {body}");
    let value = header_value(&body, "X-Value").expect("X-Value");
    let value = value.strip_prefix("n=").unwrap_or(&value);
    assert_eq!(value, n.to_string(), "lost updates under 2 workers: {body}");
}

#[test]
fn reload_preserves_zone() {
    let (nginx, port, tmp, _running) = setup_real();
    let (status, body) = http_get(port, "/set");
    assert_eq!(status, 200, "set failed: {body}");
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
    let mut ok = false;
    for _ in 0..20 {
        thread::sleep(Duration::from_millis(50));
        let (status, body) = http_get(port, "/get");
        if status == 200 {
            if let Some(value) = header_value(&body, "X-Value") {
                assert_eq!(value, "8", "reload dropped the zone: {body}");
                ok = true;
                break;
            }
        }
    }
    assert!(ok, "get after reload never returned the stored value");
}

#[test]
fn production_shm_covers_types_flags_add_replace_ttl_space_two_zones_and_empty_key() {
    let (_nginx, port, tmp, _running) = setup_real();
    let (status, body) = http_get(port, "/types");
    assert_eq!(status, 200, "types failed: {body}\n{}", error_log(&tmp));
    assert_eq!(header_value(&body, "X-Bool").unwrap(), "1");
    assert_eq!(
        header_value(&body, "X-Flags")
            .unwrap()
            .trim_start_matches("n="),
        "3"
    );
    assert_eq!(
        header_value(&body, "X-Int")
            .unwrap()
            .trim_start_matches("n="),
        "42"
    );
    assert!(
        header_value(&body, "X-Float").unwrap().contains("1.5"),
        "{body}"
    );
    assert_eq!(header_value(&body, "X-Str").unwrap(), "hi");

    let (status, body) = http_get(port, "/addrep");
    assert_eq!(status, 200, "addrep failed: {body}\n{}", error_log(&tmp));
    assert_eq!(header_value(&body, "X-Add1").unwrap(), "1");
    assert_eq!(header_value(&body, "X-Add2").unwrap(), "exists");
    assert_eq!(header_value(&body, "X-Rep").unwrap(), "c");
    assert_eq!(
        header_value(&body, "X-Nil").unwrap(),
        "attempt to add or replace nil values"
    );

    let (status, body) = http_get(port, "/ttl");
    assert_eq!(status, 200, "ttl failed: {body}\n{}", error_log(&tmp));
    let ttl = header_value(&body, "X-Ttl").unwrap();
    assert!(ttl.contains("n="), "ttl missing: {body}");

    let (status, body) = http_get(port, "/space");
    assert_eq!(status, 200, "space failed: {body}\n{}", error_log(&tmp));
    let cap: usize = header_value(&body, "X-Cap")
        .unwrap()
        .trim_start_matches("n=")
        .parse()
        .unwrap();
    let free: usize = header_value(&body, "X-Free")
        .unwrap()
        .trim_start_matches("n=")
        .parse()
        .unwrap();
    assert_eq!(cap, 1024 * 1024);
    assert!(free > 0 && free <= cap, "free={free} cap={cap}");

    let (status, body) = http_get(port, "/cats");
    assert_eq!(status, 200, "cats failed: {body}\n{}", error_log(&tmp));
    assert_eq!(header_value(&body, "X-Dogs").unwrap(), "woof");
    assert_eq!(header_value(&body, "X-Cats").unwrap(), "meow");

    let (status, body) = http_get(port, "/empty");
    assert_eq!(status, 200, "empty failed: {body}\n{}", error_log(&tmp));
    assert_eq!(header_value(&body, "X-Err").unwrap(), "empty key");
}

#[test]
fn production_shm_stale_ttl_and_tiny_zone_lru() {
    let nginx = nginx_binary();
    let module = module_cdylib();
    let tmp = isolated_prefix("sd-tiny");
    fs::create_dir_all(tmp.path().join("conf")).unwrap();
    fs::create_dir_all(tmp.path().join("logs")).unwrap();
    fs::create_dir_all(tmp.path().join("scripts")).unwrap();
    let port = free_port();
    let scripts = tmp.path().join("scripts");
    write_script(
        &scripts.join("stale_set.rss"),
        r#"
use ngx;
let d = ngx::shared::dict("dogs");
ngx::shared::set(d, "keep", "alive", null, null);
ngx::shared::set(d, "k", "v", 0.05, 9);
ngx::exit(200);
"#,
    );
    write_script(
        &scripts.join("stale_get.rss"),
        r#"
use ngx;
ngx::update_time();
let d = ngx::shared::dict("dogs");
let live = ngx::shared::get(d, "k");
let stale = ngx::shared::get_stale(d, "k");
ngx::header::set("X-Live", ngx::encode_args({ n: live["flags"] }));
if stale["stale"] {
    ngx::header::set("X-Stale", "1");
} else {
    ngx::header::set("X-Stale", "0");
}
ngx::header::set("X-Val", stale["value"]);
ngx::header::set("X-Flags", ngx::encode_args({ n: stale["flags"] }));
ngx::exit(200);
"#,
    );
    write_script(
        &scripts.join("fill.rss"),
        r#"
use ngx;
let d = ngx::shared::dict("tiny");
let blob = "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx";
ngx::shared::set(d, "a", blob, null, null);
ngx::shared::set(d, "b", blob, null, null);
ngx::shared::set(d, "c", blob, null, null);
ngx::shared::set(d, "d", blob, null, null);
ngx::shared::set(d, "e", blob, null, null);
ngx::shared::set(d, "f", blob, null, null);
let safe = ngx::shared::safe_set(d, "final", blob, null, null);
if safe["ok"] {
    ngx::header::set("X-Ok", "1");
} else {
    ngx::header::set("X-Ok", "0");
}
ngx::header::set("X-Safe", safe["error"]);
ngx::exit(200);
"#,
    );
    let locations = format!(
        "{}{}{}",
        location("/stale_set", &scripts.join("stale_set.rss")),
        location("/stale_get", &scripts.join("stale_get.rss")),
        location("/fill", &scripts.join("fill.rss")),
    );
    let module_q = nginx_quote_path(&module).unwrap();
    let pid = nginx_quote_path(&tmp.path().join("logs/nginx.pid")).unwrap();
    let error_log_p = nginx_quote_path(&tmp.path().join("logs/error.log")).unwrap();
    write_conf(
        tmp.path(),
        &format!(
            r#"
worker_processes 2;
error_log {error_log_p} info;
pid {pid};
daemon off;
master_process on;
load_module {module_q};

events {{
    worker_connections 64;
}}

http {{
    access_log off;
    rss_shared_dict dogs 1m;
    rss_shared_dict tiny 8k;
    server {{
        listen 127.0.0.1:{port};
        server_name localhost;
{locations}
    }}
}}
"#
        ),
    );
    let out = nginx_test(&nginx, tmp.path());
    assert!(
        out.status.success(),
        "tiny nginx -t failed: {}",
        combined(&out)
    );
    let _running = spawn_nginx(&nginx, tmp.path());
    wait_for_port(port, Duration::from_secs(5));

    let (status, body) = http_get(port, "/stale_set");
    assert_eq!(status, 200, "stale_set failed: {body}\n{}", error_log(&tmp));
    thread::sleep(Duration::from_millis(200));
    let (status, body) = http_get(port, "/stale_get");
    assert_eq!(status, 200, "stale_get failed: {body}\n{}", error_log(&tmp));
    assert_eq!(
        header_value(&body, "X-Live")
            .unwrap()
            .trim_start_matches("n="),
        "0",
        "live flags should be 0 after TTL: {body}"
    );
    assert_eq!(header_value(&body, "X-Stale").unwrap(), "1");
    assert_eq!(header_value(&body, "X-Val").unwrap(), "v");
    assert_eq!(
        header_value(&body, "X-Flags")
            .unwrap()
            .trim_start_matches("n="),
        "9"
    );

    let (status, body) = http_get(port, "/fill");
    assert_eq!(status, 200, "fill failed: {body}\n{}", error_log(&tmp));
    assert_eq!(
        header_value(&body, "X-Ok").unwrap(),
        "0",
        "safe_set should fail: {body}"
    );
    assert_eq!(
        header_value(&body, "X-Safe").unwrap(),
        "no memory",
        "unexpected safe_set result: {body}"
    );
}
