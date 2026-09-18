//! Task 10 request-body hosts: catalog, generation-safe nginx_op adapter,
//! owned copies, and pinned NGINX lifecycle.

use ngx_rustscript_nginx_test_stubs as nginx_stubs;

use std::fs;
use std::future::Future;
use std::io::{Read, Write};
use std::mem::MaybeUninit;
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::pin::Pin;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

use nginx_sys::{
    ngx_buf_t, ngx_chain_t, ngx_http_request_body_t, ngx_http_request_t, ngx_str_t, ngx_temp_file_t,
};
use ngx_rustscript::config::{
    ConfigError, compile_rss_source, compile_rss_source_for_phase,
    validate_program_imports_for_phase,
};
use ngx_rustscript::host::{
    AllowedOps, HostCapability, HostFamily, HostSyncKind, NgxPhase, bind_ngx_hosts,
    host_allowed_in_phase, manifest_entry, ngx_host_catalog, production_runtime_host_names,
    with_request_scope,
};
use ngx_rustscript::nginx_op::{NginxOpCell, NginxOpFuture, NginxOpOutcome};
use ngx_rustscript::notify::RequestIdentity;
use vm::{Program, Value, Vm, VmMap, VmStatus};

mod support;
use support::artifact::module_cdylib;
use support::nginx::{
    combined, nginx_binary, nginx_process_group_command, nginx_quote_path, nginx_test,
    terminate_process_group,
};
use support::temp::isolated_prefix;

const READ_BODY: &str = "ngx::req::read_body";
const DISCARD_BODY: &str = "ngx::req::discard_body";
const GET_BODY_DATA: &str = "ngx::req::get_body_data";
const GET_BODY_FILE: &str = "ngx::req::get_body_file";
const GET_POST_ARGS: &str = "ngx::req::get_post_args";

const BODY_HOSTS: &[&str] = &[READ_BODY, DISCARD_BODY, GET_BODY_DATA, GET_BODY_FILE];

fn compile(source: &str) -> Arc<Program> {
    compile_rss_source(source).unwrap_or_else(|err| panic!("compile failed: {err}"))
}

fn compile_err_for_phase(source: &str, phase: NgxPhase) -> ConfigError {
    compile_rss_source_for_phase(source, phase).expect_err("source should fail configuration")
}

fn eval_in_request(request: *mut ngx_http_request_t, source: &str) -> Result<Value, String> {
    eval_in_request_phase(request, NgxPhase::Access, source)
}

fn eval_in_request_phase(
    request: *mut ngx_http_request_t,
    phase: NgxPhase,
    source: &str,
) -> Result<Value, String> {
    let program = compile(source);
    let mut vm = Vm::try_new((*program).clone()).expect("construct vm");
    bind_ngx_hosts(&mut vm);
    with_request_scope(
        request,
        RequestIdentity::new(1, 1),
        phase,
        AllowedOps::for_phase(phase),
        || match vm.run() {
            Ok(VmStatus::Halted) => Ok(vm.stack().last().cloned().unwrap_or(Value::Null)),
            Ok(status) => Err(format!("unexpected status {status:?}")),
            Err(err) => Err(err.to_string()),
        },
    )
}

fn dummy_request() -> Box<ngx_http_request_t> {
    let mut req = Box::new(unsafe { MaybeUninit::<ngx_http_request_t>::zeroed().assume_init() });
    let ptr = (&mut *req) as *mut ngx_http_request_t;
    req.main = ptr;
    req
}

struct LiveBody {
    body: Box<ngx_http_request_body_t>,
    #[allow(clippy::vec_box)]
    _bufs: Vec<Box<ngx_buf_t>>,
    #[allow(clippy::vec_box)]
    _chains: Vec<Box<ngx_chain_t>>,
}

fn install_in_memory_body(req: &mut ngx_http_request_t, chunks: &mut [Vec<u8>]) -> LiveBody {
    let mut body =
        Box::new(unsafe { MaybeUninit::<ngx_http_request_body_t>::zeroed().assume_init() });
    let mut bufs = Vec::new();
    let mut chains = Vec::new();
    for chunk in chunks.iter_mut() {
        let mut buf = Box::new(unsafe { MaybeUninit::<ngx_buf_t>::zeroed().assume_init() });
        buf.pos = chunk.as_mut_ptr();
        buf.last = unsafe { chunk.as_mut_ptr().add(chunk.len()) };
        buf.start = chunk.as_mut_ptr();
        buf.end = buf.last;
        bufs.push(buf);
        chains.push(Box::new(unsafe {
            MaybeUninit::<ngx_chain_t>::zeroed().assume_init()
        }));
    }
    for index in 0..chains.len() {
        chains[index].buf = bufs[index].as_mut() as *mut ngx_buf_t;
        chains[index].next = if index + 1 < chains.len() {
            chains[index + 1].as_mut() as *mut ngx_chain_t
        } else {
            std::ptr::null_mut()
        };
    }
    body.bufs = if chains.is_empty() {
        std::ptr::null_mut()
    } else {
        chains[0].as_mut() as *mut ngx_chain_t
    };
    body.temp_file = std::ptr::null_mut();
    req.request_body = body.as_mut() as *mut ngx_http_request_body_t;
    LiveBody {
        body,
        _bufs: bufs,
        _chains: chains,
    }
}

struct LiveFileBody {
    _body: LiveBody,
    _temp: Box<ngx_temp_file_t>,
    _path: Vec<u8>,
}

fn install_file_body(req: &mut ngx_http_request_t, path: &mut Vec<u8>) -> LiveFileBody {
    let mut live = install_in_memory_body(req, &mut []);
    let mut temp = Box::new(unsafe { MaybeUninit::<ngx_temp_file_t>::zeroed().assume_init() });
    temp.file.name = ngx_str_t {
        len: path.len(),
        data: path.as_mut_ptr(),
    };
    live.body.temp_file = temp.as_mut() as *mut ngx_temp_file_t;
    LiveFileBody {
        _body: live,
        _temp: temp,
        _path: std::mem::take(path),
    }
}

fn expect_map(value: &Value) -> &VmMap {
    match value {
        Value::Map(map) => map,
        other => panic!("expected map, got {other:?}"),
    }
}

fn map_get<'a>(map: &'a VmMap, key: &str) -> &'a Value {
    map.get(&Value::string(key))
        .unwrap_or_else(|| panic!("missing map key {key}"))
}

fn poll_once<F: Future>(fut: Pin<&mut F>) -> Poll<F::Output> {
    let waker = Waker::noop();
    let mut cx = Context::from_waker(waker);
    fut.poll(&mut cx)
}

#[test]
fn body_hosts_are_in_catalog_manifest_and_runtime() {
    nginx_stubs::force_link();
    let catalog = ngx_host_catalog();
    let catalog_names: std::collections::HashSet<&str> = catalog
        .functions()
        .iter()
        .map(|schema| schema.name.as_str())
        .collect();
    let runtime: std::collections::HashSet<&str> =
        production_runtime_host_names().iter().copied().collect();
    for name in BODY_HOSTS {
        assert!(catalog_names.contains(name), "catalog must include {name}");
        let entry = manifest_entry(name).unwrap_or_else(|| panic!("manifest must include {name}"));
        assert_eq!(entry.name, *name);
        assert_eq!(entry.family, HostFamily::Request);
        assert!(
            entry.capabilities.contains(&HostCapability::RequestRead),
            "{name} must declare RequestRead"
        );
        assert!(
            !entry.capabilities.contains(&HostCapability::RequestWrite),
            "{name} must not declare RequestWrite"
        );
        assert!(
            runtime.contains(name),
            "runtime bind list must include {name}"
        );
        assert!(
            host_allowed_in_phase(name, NgxPhase::Access),
            "{name} must be allowed in access"
        );
        assert!(
            host_allowed_in_phase(name, NgxPhase::Content),
            "{name} must be allowed in content"
        );
        assert!(
            host_allowed_in_phase(name, NgxPhase::Rewrite),
            "{name} must be allowed in rewrite"
        );
    }

    let read = manifest_entry(READ_BODY).unwrap();
    assert_eq!(read.sync, HostSyncKind::Async);
    assert!(!read.phases.contains(&NgxPhase::Log));
    assert!(!host_allowed_in_phase(READ_BODY, NgxPhase::Log));

    let discard = manifest_entry(DISCARD_BODY).unwrap();
    assert_eq!(discard.sync, HostSyncKind::Sync);
    assert!(!discard.phases.contains(&NgxPhase::Log));
    assert!(!host_allowed_in_phase(DISCARD_BODY, NgxPhase::Log));

    let data = manifest_entry(GET_BODY_DATA).unwrap();
    assert_eq!(data.sync, HostSyncKind::Sync);
    assert!(data.phases.contains(&NgxPhase::Log));
    assert!(host_allowed_in_phase(GET_BODY_DATA, NgxPhase::Log));
    let data_schema = catalog
        .functions()
        .iter()
        .find(|schema| schema.name == GET_BODY_DATA)
        .expect("get_body_data schema");
    assert!(
        data_schema.params.is_empty(),
        "get_body_data must be nullary like OpenResty"
    );
    let read_schema = catalog
        .functions()
        .iter()
        .find(|schema| schema.name == READ_BODY)
        .expect("read_body schema");
    assert!(read_schema.params.is_empty());
    let discard_schema = catalog
        .functions()
        .iter()
        .find(|schema| schema.name == DISCARD_BODY)
        .expect("discard_body schema");
    assert!(discard_schema.params.is_empty());
    let file_schema = catalog
        .functions()
        .iter()
        .find(|schema| schema.name == GET_BODY_FILE)
        .expect("get_body_file schema");
    assert!(file_schema.params.is_empty());

    let file = manifest_entry(GET_BODY_FILE).unwrap();
    assert_eq!(file.sync, HostSyncKind::Sync);
    assert!(!file.phases.contains(&NgxPhase::Log));
    assert!(!host_allowed_in_phase(GET_BODY_FILE, NgxPhase::Log));

    let post = manifest_entry(GET_POST_ARGS).unwrap();
    assert_eq!(post.sync, HostSyncKind::Sync);
    assert!(post.phases.contains(&NgxPhase::Log));
}

#[test]
fn async_read_and_discard_are_forbidden_in_log() {
    for source in [
        "use ngx;\nngx::req::read_body();\n",
        "use ngx;\nngx::req::discard_body();\n",
        "use ngx;\nngx::req::get_body_file();\n",
    ] {
        let err = compile_err_for_phase(source, NgxPhase::Log);
        let msg = err.to_string().to_ascii_lowercase();
        assert!(
            msg.contains("log") || msg.contains("phase") || msg.contains("forbidden"),
            "log must reject {source:?}: {err}"
        );
    }
    validate_program_imports_for_phase(
        &compile("use ngx;\nngx::req::get_body_data();\nngx::req::get_post_args(null);\n"),
        NgxPhase::Log,
    )
    .expect("completed-body getters must be allowed in log");
}

#[test]
fn nginx_op_future_is_send_and_holds_no_request_pointer() {
    fn assert_send<T: Send>() {}
    fn assert_sync<T: Sync>() {}
    assert_send::<NginxOpFuture>();
    assert_sync::<NginxOpFuture>();
    assert_send::<Arc<NginxOpCell>>();
    assert_sync::<Arc<NginxOpCell>>();
    let cell_size = std::mem::size_of::<Arc<NginxOpCell>>();
    let ident_size = std::mem::size_of::<RequestIdentity>();
    let gen_size = std::mem::size_of::<u64>();
    let fut_size = std::mem::size_of::<NginxOpFuture>();
    assert!(
        fut_size <= cell_size + ident_size + gen_size + 16,
        "NginxOpFuture must not grow a raw request/VM pointer; size={fut_size}"
    );
}

#[test]
fn nginx_op_completes_exactly_once_and_ignores_stale_and_duplicate() {
    let identity = RequestIdentity::new(7, 11);
    let cell = Arc::new(NginxOpCell::new(3, identity));
    let mut fut = NginxOpFuture::observe(Arc::clone(&cell), 3, identity);
    assert!(matches!(poll_once(Pin::new(&mut fut)), Poll::Pending));

    assert!(cell.complete(3, identity, NginxOpOutcome::Done));
    assert!(!cell.complete(3, identity, NginxOpOutcome::Done));
    assert!(!cell.complete(3, identity, NginxOpOutcome::Special(413)));
    match poll_once(Pin::new(&mut fut)) {
        Poll::Ready(Ok(NginxOpOutcome::Done)) => {}
        other => panic!("expected Done, got {other:?}"),
    }

    let stale = Arc::new(NginxOpCell::new(4, identity));
    assert!(!stale.complete(3, identity, NginxOpOutcome::Done));
    assert!(!stale.complete(4, RequestIdentity::new(8, 11), NginxOpOutcome::Done));
    let mut stale_fut = NginxOpFuture::observe(Arc::clone(&stale), 4, identity);
    assert!(matches!(poll_once(Pin::new(&mut stale_fut)), Poll::Pending));
}

#[test]
fn nginx_op_cancel_exactly_once_and_late_callback_is_noop() {
    let identity = RequestIdentity::new(1, 2);
    let cell = Arc::new(NginxOpCell::new(1, identity));
    assert!(cell.cancel());
    assert!(!cell.cancel());
    assert!(!cell.complete(1, identity, NginxOpOutcome::Done));
    let mut fut = NginxOpFuture::observe(Arc::clone(&cell), 1, identity);
    match poll_once(Pin::new(&mut fut)) {
        Poll::Ready(Ok(NginxOpOutcome::Cancelled)) => {}
        other => panic!("expected Cancelled outcome, got {other:?}"),
    }
}

#[test]
fn get_body_data_copies_memory_and_nulls_file_backed() {
    let mut req = dummy_request();
    let mut chunks = [b"hello body".to_vec()];
    let _live = install_in_memory_body(&mut req, &mut chunks);
    let ptr = (&mut *req) as *mut ngx_http_request_t;
    let value = eval_in_request(ptr, "use ngx;\nngx::req::get_body_data();\n").expect("data");
    assert_eq!(value, Value::string("hello body"));

    let mut path = b"/tmp/ngx-rss-bodyXXXXXX".to_vec();
    let _file = install_file_body(&mut req, &mut path);
    let file_data = eval_in_request(ptr, "use ngx;\nngx::req::get_body_data();\n").expect("file");
    assert_eq!(file_data, Value::Null);
}

#[test]
fn get_body_file_returns_owned_path_and_null_for_memory() {
    let mut req = dummy_request();
    let mut chunks = [b"mem".to_vec()];
    let _live = install_in_memory_body(&mut req, &mut chunks);
    let ptr = (&mut *req) as *mut ngx_http_request_t;
    let none = eval_in_request(ptr, "use ngx;\nngx::req::get_body_file();\n").expect("mem");
    assert_eq!(none, Value::Null);

    let mut path = b"/tmp/ngx-rss-owned-body".to_vec();
    let _file = install_file_body(&mut req, &mut path);
    let got = eval_in_request(ptr, "use ngx;\nngx::req::get_body_file();\n").expect("path");
    assert_eq!(got, Value::string("/tmp/ngx-rss-owned-body"));
}

#[test]
fn get_post_args_parses_file_backed_body_without_deleting_file() {
    let dir = isolated_prefix("ngx-rss-post-file");
    let path = dir.join("body.txt");
    fs::write(&path, b"a=1&b=2&a=3&flag").unwrap();
    let mut req = dummy_request();
    let mut path_bytes = path.to_string_lossy().as_bytes().to_vec();
    let _file = install_file_body(&mut req, &mut path_bytes);
    let ptr = (&mut *req) as *mut ngx_http_request_t;
    let decoded = eval_in_request(ptr, "use ngx;\nngx::req::get_post_args(null);\n").expect("post");
    let root = expect_map(&decoded);
    assert_eq!(map_get(root, "truncated"), &Value::Bool(false));
    let parsed = expect_map(map_get(root, "args"));
    assert_eq!(map_get(parsed, "b"), &Value::string("2"));
    assert_eq!(map_get(parsed, "flag"), &Value::Bool(true));
    match map_get(parsed, "a") {
        Value::Array(values) => {
            assert_eq!(values.as_slice(), &[Value::string("1"), Value::string("3")]);
        }
        other => panic!("duplicate keys must become an array, got {other:?}"),
    }
    assert!(
        path.exists(),
        "get_post_args must not delete NGINX-owned temp files"
    );
}

#[test]
fn get_post_args_malformed_percent_is_left_literal() {
    let mut req = dummy_request();
    let mut body = b"a=%ZZ&b=%&c=%2".to_vec();
    let _live = install_in_memory_body(&mut req, std::slice::from_mut(&mut body));
    let ptr = (&mut *req) as *mut ngx_http_request_t;
    let decoded = eval_in_request(ptr, "use ngx;\nngx::req::get_post_args(null);\n").expect("post");
    let parsed = expect_map(map_get(expect_map(&decoded), "args"));
    assert_eq!(map_get(parsed, "a"), &Value::string("%ZZ"));
    assert_eq!(map_get(parsed, "b"), &Value::string("%"));
    assert_eq!(map_get(parsed, "c"), &Value::string("%2"));
}

#[test]
fn get_body_data_skips_empty_buffers_and_rejects_inverted_or_cyclic() {
    let mut req = dummy_request();
    let mut empty = Vec::new();
    let _live = install_in_memory_body(&mut req, std::slice::from_mut(&mut empty));
    let ptr = (&mut *req) as *mut ngx_http_request_t;
    let value = eval_in_request(ptr, "use ngx;\nngx::req::get_body_data();\n").expect("empty");
    assert_eq!(value, Value::string(""));

    let mut inverted = b"xy".to_vec();
    let mut live = install_in_memory_body(&mut req, std::slice::from_mut(&mut inverted));
    unsafe {
        let buf = live._bufs[0].as_mut();
        let last = buf.pos;
        buf.pos = last.add(1);
        buf.last = last;
    }
    let err = eval_in_request(ptr, "use ngx;\nngx::req::get_body_data();\n").expect_err("inverted");
    assert!(
        err.to_ascii_lowercase().contains("inverted"),
        "inverted range must error, got {err}"
    );

    let mut cyclic = [b"ab".to_vec(), b"cd".to_vec()];
    let mut live = install_in_memory_body(&mut req, &mut cyclic);
    let first = live._chains[0].as_mut() as *mut ngx_chain_t;
    live._chains[1].next = first;
    let err = eval_in_request(ptr, "use ngx;\nngx::req::get_body_data();\n").expect_err("cyclic");
    assert!(
        err.to_ascii_lowercase().contains("cyclic"),
        "cyclic chain must error, got {err}"
    );
}

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
    support::nginx::wait_for_worker_http(port, timeout);
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
    while let Some((size_line, after)) = rest.split_once("\r\n") {
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

fn write_script(path: &Path, body: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, body).unwrap();
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
fn nginx_request_body_memory_file_chunked_413_repeat_discard_and_disconnect() {
    let nginx = nginx_binary();
    let module = module_cdylib();
    let port = free_port();
    let prefix = isolated_prefix("ngx-rustscript-req-body");
    fs::create_dir_all(prefix.join("html")).unwrap();
    fs::write(prefix.join("html/index.html"), "fallback\n").unwrap();

    let memory = prefix.join("conf/body_memory.rss");
    write_script(
        &memory,
        r#"
use ngx;
ngx::req::read_body();
let data = ngx::req::get_body_data();
let file = ngx::req::get_body_file();
if data == null {
    ngx::say("empty");
} else {
    ngx::say(data);
}
if file == null {
    ngx::say("no-file");
} else {
    ngx::say(file);
}
"#,
    );
    let file_only = prefix.join("conf/body_file.rss");
    write_script(
        &file_only,
        r#"
use ngx;
ngx::req::read_body();
let data = ngx::req::get_body_data();
let file = ngx::req::get_body_file();
if data == null {
    ngx::say("file-null-data");
} else {
    ngx::say(data);
}
if file == null {
    ngx::say("no-file");
} else {
    ngx::say(file);
}
"#,
    );
    let post = prefix.join("conf/body_post.rss");
    write_script(
        &post,
        r#"
use ngx;
ngx::req::read_body();
let parsed: RequestArgsResult = ngx::req::get_post_args(null);
ngx::say(ngx::encode_args(parsed.args));
if parsed.truncated {
    ngx::say("trunc");
} else {
    ngx::say("full");
}
"#,
    );
    let repeat = prefix.join("conf/body_repeat.rss");
    write_script(
        &repeat,
        r#"
use ngx;
ngx::req::read_body();
ngx::req::read_body();
ngx::req::discard_body();
let data = ngx::req::get_body_data();
if data == null {
    ngx::say("empty");
} else {
    ngx::say(data);
}
"#,
    );
    let discard = prefix.join("conf/body_discard.rss");
    write_script(
        &discard,
        r#"
use ngx;
ngx::req::discard_body();
ngx::req::discard_body();
ngx::req::read_body();
ngx::req::get_body_data();
ngx::say("discarded");
"#,
    );
    let log_vis = prefix.join("conf/body_log.rss");
    write_script(
        &log_vis,
        r#"
use ngx;
let data = ngx::req::get_body_data();
ngx::log(ngx::log_level::notice(), "LOG_BODY=" + data.unwrap_or(""));
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
    let content_see = prefix.join("conf/body_content_see.rss");
    write_script(
        &content_see,
        r#"
use ngx;
ngx::say(ngx::encode_args({ body: ngx::ctx::get("body") }));
"#,
    );
    let slow = prefix.join("conf/body_slow.rss");
    write_script(
        &slow,
        r#"
use ngx;
ngx::req::read_body();
let data = ngx::req::get_body_data();
if data == null {
    ngx::say("empty");
} else {
    ngx::say(data);
}
"#,
    );
    let echo = prefix.join("conf/body_echo.rss");
    write_script(
        &echo,
        r#"
use ngx;
ngx::req::read_body();
ngx::say(ngx::req::get_body_data());
"#,
    );

    let locations = format!(
        r#"
        location /memory/ {{
            client_body_buffer_size 16k;
            content_by_rss_file {memory};
        }}
        location /file/ {{
            client_body_in_file_only clean;
            content_by_rss_file {file};
        }}
        location /post/ {{
            content_by_rss_file {post};
        }}
        location /repeat/ {{
            content_by_rss_file {repeat};
        }}
        location /discard/ {{
            content_by_rss_file {discard};
        }}
        location /vis/ {{
            access_by_rss_file {access};
            content_by_rss_file {content};
            log_by_rss_file {log};
        }}
        location /slow/ {{
            content_by_rss_file {slow};
        }}
        location /timeout/ {{
            client_body_timeout 1s;
            content_by_rss_file {slow};
        }}
        location /oversize/ {{
            client_max_body_size 4;
            content_by_rss_file {memory};
        }}
        location /echo/ {{
            content_by_rss_file {echo};
        }}
"#,
        memory = nginx_quote_path(&memory).unwrap(),
        file = nginx_quote_path(&file_only).unwrap(),
        post = nginx_quote_path(&post).unwrap(),
        repeat = nginx_quote_path(&repeat).unwrap(),
        discard = nginx_quote_path(&discard).unwrap(),
        access = nginx_quote_path(&access_read).unwrap(),
        content = nginx_quote_path(&content_see).unwrap(),
        log = nginx_quote_path(&log_vis).unwrap(),
        slow = nginx_quote_path(&slow).unwrap(),
        echo = nginx_quote_path(&echo).unwrap(),
    );
    let conf = body_conf(&module, &prefix, port, &locations);
    fs::write(prefix.join("conf/nginx.conf"), &conf).unwrap();
    let out = nginx_test(&nginx, &prefix);
    let text = combined(&out);
    assert!(
        out.status.success(),
        "body scripts must pass nginx -t:\n{text}"
    );

    let mut running = spawn_nginx(&nginx, &prefix);
    wait_for_port(port, Duration::from_secs(5));

    let (status, response) = http_exchange(
        port,
        "POST /memory/ HTTP/1.0\r\nHost: 127.0.0.1\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello",
    );
    assert_eq!(status, 200, "{response}");
    assert!(response.contains("hello"), "{response}");
    assert!(response.contains("no-file"), "{response}");

    let (status, response) = http_exchange(
        port,
        "POST /memory/ HTTP/1.0\r\nHost: 127.0.0.1\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
    );
    assert_eq!(status, 200, "empty body must be 200, got {response}");

    let (status, response) = http_exchange(
        port,
        "POST /file/ HTTP/1.0\r\nHost: 127.0.0.1\r\nContent-Length: 4\r\nConnection: close\r\n\r\nabcd",
    );
    assert_eq!(status, 200, "{response}");
    assert!(response.contains("file-null-data"), "{response}");
    assert!(
        response.contains("/client_body_temp") || response.contains("client_body"),
        "get_body_file must return owned temp path, got {response}"
    );

    let (status, response) = http_exchange(
        port,
        "POST /post/ HTTP/1.0\r\nHost: 127.0.0.1\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: 15\r\nConnection: close\r\n\r\na=1&b=2&a=3&flag",
    );
    assert_eq!(status, 200, "{response}");
    assert!(response.contains("a=1"), "{response}");
    assert!(response.contains("full"), "{response}");

    let (status, response) = http_exchange(
        port,
        "POST /echo/ HTTP/1.1\r\nHost: 127.0.0.1\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n5\r\nhello\r\n0\r\n\r\n",
    );
    let errlog = fs::read_to_string(prefix.join("logs/error.log")).unwrap_or_default();
    let (parsed_status, payload) = parse_http_response(&response);
    assert_eq!(
        parsed_status, 200,
        "chunked status must be 200, got {status}: {response}\nerror.log={errlog}"
    );
    assert_eq!(
        payload, "hello\n",
        "chunked payload must be hello plus ngx::say newline, got {payload:?}\nerror.log={errlog}"
    );

    let (status, response) = http_exchange(
        port,
        "POST /repeat/ HTTP/1.0\r\nHost: 127.0.0.1\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
    );
    assert_eq!(status, 200, "{response}");
    assert!(response.contains("ok"), "{response}");

    let (status, response) = http_exchange(
        port,
        "POST /discard/ HTTP/1.0\r\nHost: 127.0.0.1\r\nContent-Length: 4\r\nConnection: close\r\n\r\ndrop",
    );
    assert_eq!(status, 200, "{response}");
    assert!(response.contains("discarded"), "{response}");

    let (status, response) = http_exchange(
        port,
        "POST /vis/ HTTP/1.0\r\nHost: 127.0.0.1\r\nContent-Length: 3\r\nConnection: close\r\n\r\nxyz",
    );
    assert_eq!(status, 200, "{response}");
    assert!(
        response.contains("xyz") || response.contains("body=xyz"),
        "{response}"
    );
    let log_path = prefix.join("logs/error.log");
    let start = Instant::now();
    let mut saw_log = String::new();
    while start.elapsed() < Duration::from_secs(3) {
        let log = fs::read_to_string(&log_path).unwrap_or_default();
        if log.contains("LOG_BODY=xyz") {
            saw_log = log;
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        saw_log.contains("LOG_BODY=xyz"),
        "log phase must log getter result including xyz: {}\nconf={}",
        fs::read_to_string(&log_path).unwrap_or_default(),
        fs::read_to_string(prefix.join("conf/nginx.conf")).unwrap_or_default()
    );
    assert!(
        !saw_log.contains("log_by_rss_file:"),
        "log getter must not record a handler error: {saw_log}"
    );

    let (status, response) = http_exchange(
        port,
        "POST /oversize/ HTTP/1.0\r\nHost: 127.0.0.1\r\nContent-Length: 8\r\nConnection: close\r\n\r\ntoolarge",
    );
    assert_eq!(
        status, 413,
        "configured oversize must be 413, got {response}"
    );

    let mut disc = TcpStream::connect(("127.0.0.1", port)).unwrap();
    disc.write_all(
        b"POST /slow/ HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 16\r\nConnection: close\r\n\r\npartial",
    )
    .unwrap();
    let _ = disc.flush();
    std::thread::sleep(Duration::from_millis(300));
    drop(disc);
    let access_log = prefix.join("logs/access.log");
    assert!(
        wait_access_status(&access_log, "POST", "/slow/", 499, Duration::from_secs(8)),
        "disconnect during body wait must log 499:\n{}",
        fs::read_to_string(&access_log).unwrap_or_default()
    );

    let mut timed = TcpStream::connect(("127.0.0.1", port)).unwrap();
    timed
        .write_all(b"POST /timeout/ HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 16\r\nConnection: close\r\n\r\n")
        .unwrap();
    let _ = timed.flush();
    assert!(
        wait_access_status(
            &access_log,
            "POST",
            "/timeout/",
            408,
            Duration::from_secs(8)
        ),
        "body wait timeout must log 408:\n{}",
        fs::read_to_string(&access_log).unwrap_or_default()
    );
    drop(timed);

    let (status, response) = http_exchange(
        port,
        "POST /memory/ HTTP/1.0\r\nHost: 127.0.0.1\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
    );
    assert_eq!(status, 200, "follow-up after 499 must succeed: {response}");

    let mut handles = Vec::new();
    for i in 0..4 {
        let handle = std::thread::spawn(move || {
            let token = format!("c{i:02}");
            let (status, response) = http_exchange(
                port,
                &format!(
                    "POST /memory/ HTTP/1.0\r\nHost: 127.0.0.1\r\nContent-Length: 3\r\nConnection: close\r\n\r\n{token}"
                ),
            );
            (token, status, response)
        });
        handles.push(handle);
    }
    for handle in handles {
        let (token, status, response) = handle.join().unwrap();
        assert_eq!(
            status,
            200,
            "concurrent mapping failed for {token}: {response}\nerror.log={}",
            fs::read_to_string(prefix.join("logs/error.log")).unwrap_or_default()
        );
        let (_, payload) = parse_http_response(&response);
        assert!(
            payload.starts_with(&format!("{token}\n")),
            "concurrent request must receive unique body {token}, got {payload:?}"
        );
        for other in 0..4 {
            let other_token = format!("c{other:02}");
            if other_token != token {
                assert!(
                    !payload.contains(&other_token),
                    "concurrent body {token} must not contain {other_token}: {payload:?}"
                );
            }
        }
    }

    let reload = Command::new(&nginx)
        .arg("-p")
        .arg(prefix.path())
        .arg("-c")
        .arg("conf/nginx.conf")
        .arg("-s")
        .arg("reload")
        .output()
        .expect("reload");
    assert!(
        reload.status.success(),
        "reload failed: {}",
        String::from_utf8_lossy(&reload.stderr)
    );
    std::thread::sleep(Duration::from_millis(400));
    let (status, response) = http_exchange(
        port,
        "POST /memory/ HTTP/1.0\r\nHost: 127.0.0.1\r\nContent-Length: 3\r\nConnection: close\r\n\r\nrel",
    );
    assert_eq!(status, 200, "healthy 200 after reload: {response}");

    quit_and_wait(&mut running.child, Duration::from_secs(8));
}
