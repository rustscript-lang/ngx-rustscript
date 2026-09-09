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

#[test]
fn two_workers_list_flush_keys_reload_and_pressure() {
    let nginx = nginx_binary();
    let module = module_cdylib();
    let tmp = isolated_prefix("shared-dict-list-nginx");
    let port = free_port();
    let scripts = tmp.path().join("html");
    fs::create_dir_all(&scripts).unwrap();

    write_script(
        &scripts.join("push.rss"),
        r#"
use ngx;
let d = ngx::shared::dict("dogs");
ngx::shared::lpush(d, "q", "left");
ngx::shared::rpush(d, "q", "right");
ngx::header::set("X-Len", ngx::encode_args({ n: ngx::shared::llen(d, "q")["value"] }));
ngx::header::set("X-Worker", ngx::encode_args({ n: ngx::worker::id() }));
ngx::exit(200);
"#,
    );
    write_script(
        &scripts.join("pop.rss"),
        r#"
use ngx;
let d = ngx::shared::dict("dogs");
let left = ngx::shared::lpop(d, "q");
let right = ngx::shared::rpop(d, "q");
ngx::header::set("X-Left", left["value"]);
ngx::header::set("X-Right", right["value"]);
ngx::header::set("X-Len", ngx::encode_args({ n: ngx::shared::llen(d, "q")["value"] }));
ngx::header::set("X-Worker", ngx::encode_args({ n: ngx::worker::id() }));
ngx::exit(200);
"#,
    );
    write_script(
        &scripts.join("conflict.rss"),
        r#"
use ngx;
let d = ngx::shared::dict("dogs");
ngx::shared::set(d, "scalar", "keep", null, null);
let push = ngx::shared::lpush(d, "scalar", "x");
let got = ngx::shared::get(d, "scalar");
ngx::header::set("X-Err", push["error"]);
ngx::header::set("X-Val", got["value"]);
ngx::exit(200);
"#,
    );
    write_script(
        &scripts.join("list_get.rss"),
        r#"
use ngx;
let d = ngx::shared::dict("dogs");
ngx::shared::rpush(d, "lst", "a");
let got = ngx::shared::get(d, "lst");
ngx::header::set("X-Err", got["error"]);
ngx::header::set("X-Len", ngx::encode_args({ n: ngx::shared::llen(d, "lst")["value"] }));
ngx::exit(200);
"#,
    );
    write_script(
        &scripts.join("seed_keys.rss"),
        r#"
use ngx;
let d = ngx::shared::dict("dogs");
ngx::shared::flush_all(d);
ngx::shared::flush_expired(d, 0);
ngx::shared::set(d, "a", "1", null, null);
ngx::shared::set(d, "b", "2", null, null);
ngx::shared::set(d, "c", "3", null, null);
ngx::shared::rpush(d, "lst2", "x");
ngx::exit(200);
"#,
    );
    write_script(
        &scripts.join("get_keys.rss"),
        r#"
use ngx;
let d = ngx::shared::dict("dogs");
let keys = ngx::shared::get_keys(d, 0);
ngx::header::set("X-N", ngx::encode_args({ n: keys.length }));
ngx::header::set("X-0", keys[0]);
ngx::header::set("X-1", keys[1]);
ngx::header::set("X-2", keys[2]);
ngx::header::set("X-3", keys[3]);
ngx::header::set("X-A", ngx::shared::get(d, "a")["value"]);
ngx::header::set("X-B", ngx::shared::get(d, "b")["value"]);
ngx::header::set("X-C", ngx::shared::get(d, "c")["value"]);
ngx::header::set("X-Lst", ngx::encode_args({ n: ngx::shared::llen(d, "lst2")["value"] }));
ngx::exit(200);
"#,
    );
    write_script(
        &scripts.join("flush.rss"),
        r#"
use ngx;
let d = ngx::shared::dict("dogs");
ngx::shared::set(d, "live", "1", 1, null);
ngx::shared::flush_all(d);
let n = ngx::shared::flush_expired(d, 0);
let live = ngx::shared::get(d, "live");
ngx::header::set("X-Freed", ngx::encode_args({ n: n }));
if live["stale"] {
    ngx::header::set("X-Stale", "1");
} else {
    ngx::header::set("X-Stale", "0");
}
ngx::exit(200);
"#,
    );
    write_script(
        &scripts.join("expire_list.rss"),
        r#"
use ngx;
let d = ngx::shared::dict("dogs");
ngx::shared::rpush(d, "exp", "z");
ngx::shared::expire(d, "exp", 0.05);
ngx::exit(200);
"#,
    );
    write_script(
        &scripts.join("expire_list_get.rss"),
        r#"
use ngx;
ngx::update_time();
let d = ngx::shared::dict("dogs");
let n = ngx::shared::llen(d, "exp");
ngx::header::set("X-Len", ngx::encode_args({ n: n["value"] }));
ngx::exit(200);
"#,
    );
    write_script(
        &scripts.join("list_fill.rss"),
        r#"
use ngx;
let d = ngx::shared::dict("tiny");
let blob = "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx";
let a = ngx::shared::lpush(d, "q", blob);
let b = ngx::shared::lpush(d, "q", blob);
let c = ngx::shared::lpush(d, "q", blob);
let d2 = ngx::shared::lpush(d, "q", blob);
let e = ngx::shared::lpush(d, "q", blob);
let last = ngx::shared::lpush(d, "q", blob);
ngx::header::set("X-Err", last["error"]);
ngx::exit(200);
"#,
    );
    write_script(
        &scripts.join("reload_check.rss"),
        r#"
use ngx;
let d = ngx::shared::dict("dogs");
let n = ngx::shared::llen(d, "reload");
ngx::header::set("X-Len", ngx::encode_args({ n: n["value"] }));
ngx::header::set("X-Pop", ngx::shared::lpop(d, "reload")["value"]);
ngx::exit(200);
"#,
    );
    write_script(
        &scripts.join("reload_seed.rss"),
        r#"
use ngx;
let d = ngx::shared::dict("dogs");
ngx::shared::rpush(d, "reload", "keep");
ngx::exit(200);
"#,
    );

    let locations = format!(
        "{}{}{}{}{}{}{}{}{}{}{}",
        location("/push", &scripts.join("push.rss")),
        location("/pop", &scripts.join("pop.rss")),
        location("/conflict", &scripts.join("conflict.rss")),
        location("/list_get", &scripts.join("list_get.rss")),
        location("/seed_keys", &scripts.join("seed_keys.rss")),
        location("/get_keys", &scripts.join("get_keys.rss")),
        location("/flush", &scripts.join("flush.rss")),
        location("/expire_list", &scripts.join("expire_list.rss")),
        location("/expire_list_get", &scripts.join("expire_list_get.rss")),
        location("/list_fill", &scripts.join("list_fill.rss")),
        location("/reload_seed", &scripts.join("reload_seed.rss")),
    );
    let reload_loc = location("/reload_check", &scripts.join("reload_check.rss"));
    let module_q = nginx_quote_path(&module).unwrap();
    let pid = nginx_quote_path(&tmp.path().join("logs/nginx.pid")).unwrap();
    let error_log_p = nginx_quote_path(&tmp.path().join("logs/error.log")).unwrap();
    let conf = format!(
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
{locations}{reload_loc}
    }}
}}
"#
    );
    write_conf(tmp.path(), &conf);
    let out = nginx_test(&nginx, tmp.path());
    assert!(
        out.status.success(),
        "list nginx -t failed: {}",
        combined(&out)
    );
    let running = spawn_nginx(&nginx, tmp.path());
    wait_for_port(port, Duration::from_secs(5));

    let (status, body) = http_get(port, "/push");
    assert_eq!(status, 200, "push failed: {body}\n{}", error_log(&tmp));
    assert_eq!(
        header_value(&body, "X-Len")
            .unwrap()
            .trim_start_matches("n="),
        "2"
    );
    let push_worker = header_value(&body, "X-Worker").unwrap();

    let mut pop_body = String::new();
    let mut saw_other_worker = false;
    for _ in 0..20 {
        let _ = http_get(port, "/push");
        let (status, body) = http_get(port, "/pop");
        assert_eq!(status, 200, "pop failed: {body}\n{}", error_log(&tmp));
        if header_value(&body, "X-Worker").unwrap() != push_worker {
            saw_other_worker = true;
            pop_body = body;
            break;
        }
        pop_body = body;
    }
    assert!(
        saw_other_worker || header_value(&pop_body, "X-Left").is_some(),
        "never observed a pop: {pop_body}"
    );
    assert_eq!(header_value(&pop_body, "X-Left").unwrap(), "left");
    assert_eq!(header_value(&pop_body, "X-Right").unwrap(), "right");

    let (status, body) = http_get(port, "/conflict");
    assert_eq!(status, 200, "conflict failed: {body}\n{}", error_log(&tmp));
    assert_eq!(header_value(&body, "X-Err").unwrap(), "value not a list");
    assert_eq!(header_value(&body, "X-Val").unwrap(), "keep");

    let (status, body) = http_get(port, "/list_get");
    assert_eq!(status, 200, "list_get failed: {body}\n{}", error_log(&tmp));
    assert_eq!(header_value(&body, "X-Err").unwrap(), "value is a list");
    assert_eq!(
        header_value(&body, "X-Len")
            .unwrap()
            .trim_start_matches("n="),
        "1"
    );

    let (status, body) = http_get(port, "/seed_keys");
    assert_eq!(status, 200, "seed_keys failed: {body}\n{}", error_log(&tmp));
    let (status, body) = http_get(port, "/get_keys");
    assert_eq!(status, 200, "get_keys failed: {body}\n{}", error_log(&tmp));
    assert_eq!(
        header_value(&body, "X-N").unwrap().trim_start_matches("n="),
        "4",
        "get_keys must return every live key: {body}"
    );
    let mut names = [
        header_value(&body, "X-0").unwrap(),
        header_value(&body, "X-1").unwrap(),
        header_value(&body, "X-2").unwrap(),
        header_value(&body, "X-3").unwrap(),
    ];
    names.sort();
    assert_eq!(
        names,
        [
            "a".to_string(),
            "b".to_string(),
            "c".to_string(),
            "lst2".to_string()
        ],
        "get_keys must preserve the live key set: {body}"
    );
    assert_eq!(header_value(&body, "X-A").unwrap(), "1");
    assert_eq!(header_value(&body, "X-B").unwrap(), "2");
    assert_eq!(header_value(&body, "X-C").unwrap(), "3");
    assert_eq!(
        header_value(&body, "X-Lst")
            .unwrap()
            .trim_start_matches("n="),
        "1",
        "get_keys must not delete the list key: {body}"
    );

    let (status, body) = http_get(port, "/expire_list");
    assert_eq!(
        status,
        200,
        "expire_list failed: {body}\n{}",
        error_log(&tmp)
    );
    thread::sleep(Duration::from_millis(200));
    let (status, body) = http_get(port, "/expire_list_get");
    assert_eq!(
        status,
        200,
        "expire_list_get failed: {body}\n{}",
        error_log(&tmp)
    );
    assert_eq!(
        header_value(&body, "X-Len")
            .unwrap()
            .trim_start_matches("n="),
        "0",
        "expired list should report llen 0: {body}"
    );

    let (status, body) = http_get(port, "/flush");
    assert_eq!(status, 200, "flush failed: {body}\n{}", error_log(&tmp));
    let freed = header_value(&body, "X-Freed")
        .unwrap()
        .trim_start_matches("n=")
        .to_string();
    assert!(
        freed.parse::<i64>().unwrap() >= 0,
        "flush_expired should return a count: {body}"
    );

    let (status, body) = http_get(port, "/list_fill");
    assert_eq!(status, 200, "list_fill failed: {body}\n{}", error_log(&tmp));
    assert_eq!(header_value(&body, "X-Err").unwrap(), "no memory");

    let (status, body) = http_get(port, "/reload_seed");
    assert_eq!(
        status,
        200,
        "reload_seed failed: {body}\n{}",
        error_log(&tmp)
    );
    let reload = Command::new(&nginx)
        .arg("-s")
        .arg("reload")
        .arg("-p")
        .arg(tmp.path())
        .arg("-c")
        .arg("conf/nginx.conf")
        .output()
        .unwrap();
    assert!(
        reload.status.success(),
        "reload failed: {}",
        combined(&reload)
    );
    thread::sleep(Duration::from_millis(200));
    write_script(
        &scripts.join("reload_check.rss"),
        r#"
use ngx;
let d = ngx::shared::dict("dogs");
let n = ngx::shared::llen(d, "reload");
ngx::header::set("X-Len", ngx::encode_args({ n: n["value"] }));
ngx::header::set("X-Pop", ngx::shared::lpop(d, "reload")["value"]);
ngx::exit(200);
"#,
    );
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
{locations}{reload_loc}
    }}
}}
"#
        ),
    );
    let _ = Command::new(&nginx)
        .arg("-s")
        .arg("reload")
        .arg("-p")
        .arg(tmp.path())
        .arg("-c")
        .arg("conf/nginx.conf")
        .output();
    thread::sleep(Duration::from_millis(200));
    let (status, body) = http_get(port, "/reload_check");
    assert_eq!(
        status,
        200,
        "reload_check failed: {body}\n{}",
        error_log(&tmp)
    );
    assert_eq!(
        header_value(&body, "X-Len")
            .unwrap()
            .trim_start_matches("n="),
        "1"
    );
    assert_eq!(header_value(&body, "X-Pop").unwrap(), "keep");
    drop(running);
}

fn http_get_timeout(port: u16, path: &str, timeout: Duration) -> (u16, String) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream.set_read_timeout(Some(timeout)).unwrap();
    stream.set_write_timeout(Some(timeout)).unwrap();
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

fn rss_seed_numbered(dict: &str, prefix: &str, count: usize, exptime: &str) -> String {
    let mut body = format!("use ngx;\nlet d = ngx::shared::dict(\"{dict}\");\n");
    for i in 0..count {
        body.push_str(&format!(
            "ngx::shared::set(d, \"{prefix}{i:04}\", 1, {exptime}, null);\n"
        ));
    }
    body.push_str("ngx::exit(200);\n");
    body
}

#[test]
fn shm_get_keys_flush_all_visit_caps_and_list_pop() {
    let nginx = nginx_binary();
    let module = module_cdylib();
    let tmp = isolated_prefix("shared-dict-shm-caps");
    let port = free_port();
    let scripts = tmp.path().join("html");
    fs::create_dir_all(&scripts).unwrap();

    write_script(
        &scripts.join("seed_many.rss"),
        &rss_seed_numbered("dogs", "k", 1100, "null"),
    );
    write_script(
        &scripts.join("flush_all.rss"),
        r#"
use ngx;
let d = ngx::shared::dict("dogs");
ngx::shared::flush_all(d);
ngx::exit(200);
"#,
    );
    write_script(
        &scripts.join("keys_count.rss"),
        r#"
use ngx;
let d = ngx::shared::dict("dogs");
let keys = ngx::shared::get_keys(d, 0);
ngx::header::set("X-N", ngx::encode_args({ n: keys.length }));
ngx::exit(200);
"#,
    );
    write_script(
        &scripts.join("ends_probe.rss"),
        r#"
use ngx;
let d = ngx::shared::dict("dogs");
let k0 = ngx::shared::get(d, "k0000");
let kn = ngx::shared::get(d, "k1099");
if k0["value"] == null {
    ngx::header::set("X-K0", "missing");
} else {
    ngx::header::set("X-K0", "1");
}
if kn["value"] == null {
    ngx::header::set("X-KN", "missing");
} else {
    ngx::header::set("X-KN", "1");
}
ngx::exit(200);
"#,
    );
    write_script(
        &scripts.join("seed_visit_expired.rss"),
        &rss_seed_numbered("visit", "e", 1100, "3"),
    );
    write_script(
        &scripts.join("seed_visit_live.rss"),
        r#"
use ngx;
let d = ngx::shared::dict("visit");
ngx::shared::set(d, "live", "1", null, null);
ngx::exit(200);
"#,
    );
    write_script(
        &scripts.join("get_keys_visit.rss"),
        r#"
use ngx;
ngx::update_time();
let d = ngx::shared::dict("visit");
let keys = ngx::shared::get_keys(d, 10);
ngx::header::set("X-N", ngx::encode_args({ n: keys.length }));
ngx::header::set("X-Live", ngx::shared::get(d, "live")["value"]);
ngx::exit(200);
"#,
    );
    let mut flush_expired_seed = String::from(
        "use ngx;\nlet d = ngx::shared::dict(\"cats\");\nngx::shared::set(d, \"expired-lru\", 0, 5, null);\n",
    );
    for i in 0..1024 {
        flush_expired_seed.push_str(&format!(
            "ngx::shared::set(d, \"live{i:04}\", 1, null, null);\n"
        ));
    }
    for i in 0..5 {
        flush_expired_seed.push_str(&format!(
            "ngx::shared::set(d, \"expired-mru-{i}\", 2, 5, null);\n"
        ));
    }
    flush_expired_seed.push_str("ngx::exit(200);\n");
    write_script(
        &scripts.join("seed_flush_expired_visit.rss"),
        &flush_expired_seed,
    );
    write_script(
        &scripts.join("flush_expired_visit.rss"),
        r#"
use ngx;
ngx::update_time();
let d = ngx::shared::dict("cats");
let n = ngx::shared::flush_expired(d, 10);
let stale = ngx::shared::get_stale(d, "expired-mru-4");
ngx::header::set("X-Freed", ngx::encode_args({ n: n }));
if stale["value"] == null {
    ngx::header::set("X-Stale", "missing");
} else {
    ngx::header::set("X-Stale", "present");
}
ngx::exit(200);
"#,
    );
    write_script(
        &scripts.join("list_pop_ok.rss"),
        r#"
use ngx;
let d = ngx::shared::dict("dogs");
ngx::shared::rpush(d, "decode", "keep");
ngx::shared::rpush(d, "decode", "tail");
let popped = ngx::shared::lpop(d, "decode");
let rest = ngx::shared::llen(d, "decode");
ngx::header::set("X-Pop", popped["value"]);
if popped["error"] == null {
    ngx::header::set("X-Err", "none");
} else {
    ngx::header::set("X-Err", "err");
}
ngx::header::set("X-Len", ngx::encode_args({ n: rest["value"] }));
ngx::exit(200);
"#,
    );

    let locations = format!(
        "{}{}{}{}{}{}{}{}{}{}",
        location("/seed_many", &scripts.join("seed_many.rss")),
        location("/flush_all", &scripts.join("flush_all.rss")),
        location("/keys_count", &scripts.join("keys_count.rss")),
        location("/ends_probe", &scripts.join("ends_probe.rss")),
        location(
            "/seed_visit_expired",
            &scripts.join("seed_visit_expired.rss")
        ),
        location("/seed_visit_live", &scripts.join("seed_visit_live.rss")),
        location("/get_keys_visit", &scripts.join("get_keys_visit.rss")),
        location(
            "/seed_flush_expired_visit",
            &scripts.join("seed_flush_expired_visit.rss")
        ),
        location(
            "/flush_expired_visit",
            &scripts.join("flush_expired_visit.rss")
        ),
        location("/list_pop_ok", &scripts.join("list_pop_ok.rss")),
    );
    let module_q = nginx_quote_path(&module).unwrap();
    let pid = nginx_quote_path(&tmp.path().join("logs/nginx.pid")).unwrap();
    let error_log_p = nginx_quote_path(&tmp.path().join("logs/error.log")).unwrap();
    let conf = format!(
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
    rss_shared_dict dogs 2m;
    rss_shared_dict cats 2m;
    rss_shared_dict visit 2m;
    server {{
        listen 127.0.0.1:{port};
        server_name localhost;
{locations}
    }}
}}
"#
    );
    write_conf(tmp.path(), &conf);
    let out = nginx_test(&nginx, tmp.path());
    assert!(
        out.status.success(),
        "shm cap nginx -t failed: {}",
        combined(&out)
    );
    let running = spawn_nginx(&nginx, tmp.path());
    wait_for_port(port, Duration::from_secs(5));

    let (status, body) = http_get_timeout(port, "/seed_many", Duration::from_secs(30));
    assert_eq!(status, 200, "seed_many failed: {body}\n{}", error_log(&tmp));
    let (status, body) = http_get(port, "/keys_count");
    assert_eq!(
        status,
        200,
        "keys_count failed: {body}\n{}",
        error_log(&tmp)
    );
    assert_eq!(
        header_value(&body, "X-N").unwrap().trim_start_matches("n="),
        "1024",
        "1100 live keys must cap get_keys at 1024: {body}"
    );

    let (status, body) = http_get(port, "/flush_all");
    assert_eq!(
        status,
        200,
        "flush_all 1 failed: {body}\n{}",
        error_log(&tmp)
    );
    let (status, body) = http_get(port, "/ends_probe");
    assert_eq!(
        status,
        200,
        "ends_probe after flush_all 1 failed: {body}\n{}",
        error_log(&tmp)
    );
    assert_eq!(
        header_value(&body, "X-K0").unwrap(),
        "missing",
        "first flush_all must expire the LRU key: {body}"
    );
    assert_eq!(
        header_value(&body, "X-KN").unwrap(),
        "1",
        "first flush_all must not wipe keys beyond the visit cap: {body}"
    );

    let mut newest_live = true;
    for call in 2..=8 {
        let (status, body) = http_get(port, "/flush_all");
        assert_eq!(
            status,
            200,
            "flush_all {call} failed: {body}\n{}",
            error_log(&tmp)
        );
        let (status, body) = http_get(port, "/ends_probe");
        assert_eq!(
            status,
            200,
            "ends_probe after flush_all {call} failed: {body}\n{}",
            error_log(&tmp)
        );
        newest_live = header_value(&body, "X-KN").unwrap() != "missing";
        if !newest_live {
            break;
        }
    }
    assert!(
        !newest_live,
        "repeated flush_all must finish >1024 keys; newest still live"
    );
    let (status, body) = http_get(port, "/keys_count");
    assert_eq!(
        status,
        200,
        "final count failed: {body}\n{}",
        error_log(&tmp)
    );
    assert_eq!(
        header_value(&body, "X-N").unwrap().trim_start_matches("n="),
        "0",
        "get_keys must be empty after flush_all completes: {body}"
    );

    let (status, body) = http_get_timeout(port, "/seed_visit_expired", Duration::from_secs(30));
    assert_eq!(
        status,
        200,
        "seed_visit_expired failed: {body}\n{}",
        error_log(&tmp)
    );
    thread::sleep(Duration::from_millis(3100));
    let (status, body) = http_get(port, "/seed_visit_live");
    assert_eq!(
        status,
        200,
        "seed_visit_live failed: {body}\n{}",
        error_log(&tmp)
    );
    let (status, body) = http_get(port, "/get_keys_visit");
    assert_eq!(
        status,
        200,
        "get_keys_visit failed: {body}\n{}",
        error_log(&tmp)
    );
    assert_eq!(
        header_value(&body, "X-N").unwrap().trim_start_matches("n="),
        "0",
        "get_keys visit cap must skip the live MRU key: {body}"
    );
    assert_eq!(
        header_value(&body, "X-Live").unwrap(),
        "1",
        "live key must survive get_keys: {body}"
    );

    let (status, body) =
        http_get_timeout(port, "/seed_flush_expired_visit", Duration::from_secs(30));
    assert_eq!(
        status,
        200,
        "seed_flush_expired_visit failed: {body}\n{}",
        error_log(&tmp)
    );
    thread::sleep(Duration::from_millis(5100));
    let (status, body) = http_get(port, "/flush_expired_visit");
    assert_eq!(
        status,
        200,
        "flush_expired_visit failed: {body}\n{}",
        error_log(&tmp)
    );
    assert_eq!(
        header_value(&body, "X-Freed")
            .unwrap()
            .trim_start_matches("n="),
        "1",
        "flush_expired visit cap should free only the LRU expired key: {body}"
    );
    assert_eq!(
        header_value(&body, "X-Stale").unwrap(),
        "present",
        "MRU expired key must remain under the visit cap: {body}"
    );

    let (status, body) = http_get(port, "/list_pop_ok");
    assert_eq!(
        status,
        200,
        "list_pop_ok failed: {body}\n{}",
        error_log(&tmp)
    );
    assert_eq!(header_value(&body, "X-Pop").unwrap(), "keep");
    assert_eq!(
        header_value(&body, "X-Err").unwrap(),
        "none",
        "well-formed lpop must not error: {body}"
    );
    assert_eq!(
        header_value(&body, "X-Len")
            .unwrap()
            .trim_start_matches("n="),
        "1"
    );
    drop(running);
}
