//! Task 7 request/response mutation hosts: catalog, capabilities, scope,
//! injection/limits, and real NGINX observations.

use ngx_rustscript_nginx_test_stubs as nginx_stubs;

use std::fs;
use std::io::{Read, Write};
use std::mem::MaybeUninit;
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use nginx_sys::{NGX_HTTP_GET, NGX_HTTP_POST, ngx_http_request_t, ngx_str_t, ngx_uint_t};
use ngx_rustscript::config::{ConfigError, compile_rss_source, program_requires_request_state};
use ngx_rustscript::host::{
    AllowedOps, HostCapability, HostFamily, HostSyncKind, NgxPhase, bind_ngx_hosts,
    host_allowed_in_phase, manifest_entry, ngx_host_catalog, production_runtime_host_names,
    with_request_scope,
};
use ngx_rustscript::notify::RequestIdentity;
use vm::{HostTypeSchema, Program, Value, Vm, VmStatus};

mod support;
use support::artifact::module_cdylib;
use support::nginx::{combined, nginx_binary, nginx_quote_path, nginx_test};
use support::temp::isolated_prefix;

const SET_METHOD: &str = "ngx::req::set_method";
const SET_URI: &str = "ngx::req::set_uri";
const SET_URI_ARGS: &str = "ngx::req::set_uri_args";
const SET_HEADER: &str = "ngx::req::set_header";
const CLEAR_HEADER: &str = "ngx::req::clear_header";
const VAR_SET: &str = "ngx::var::set";
const STATUS_GET: &str = "ngx::status::get";
const STATUS_SET: &str = "ngx::status::set";
const HEADER_GET: &str = "ngx::header::get";
const HEADER_SET: &str = "ngx::header::set";
const HEADER_SET_ALL: &str = "ngx::header::set_all";
const HEADER_CLEAR: &str = "ngx::header::clear";

const MUTATION_HOSTS: &[&str] = &[
    SET_METHOD,
    SET_URI,
    SET_URI_ARGS,
    SET_HEADER,
    CLEAR_HEADER,
    VAR_SET,
    STATUS_GET,
    STATUS_SET,
    HEADER_GET,
    HEADER_SET,
    HEADER_SET_ALL,
    HEADER_CLEAR,
];

fn compile(source: &str) -> Arc<Program> {
    compile_rss_source(source).unwrap_or_else(|err| panic!("compile failed: {err}\n{source}"))
}

fn compile_err(source: &str) -> ConfigError {
    compile_rss_source(source).expect_err("source should fail configuration")
}

fn dummy_request() -> Box<ngx_http_request_t> {
    let mut req = Box::new(unsafe { MaybeUninit::<ngx_http_request_t>::zeroed().assume_init() });
    let ptr = (&mut *req) as *mut ngx_http_request_t;
    req.main = ptr;
    req
}

fn ngx_str(bytes: &mut [u8]) -> ngx_str_t {
    ngx_str_t {
        len: bytes.len(),
        data: bytes.as_mut_ptr(),
    }
}

fn eval_in_request_ops(
    request: *mut ngx_http_request_t,
    ops: AllowedOps,
    identity: RequestIdentity,
    source: &str,
) -> Result<Value, String> {
    let program = compile(source);
    let mut vm = Vm::try_new((*program).clone()).expect("construct vm");
    bind_ngx_hosts(&mut vm);
    with_request_scope(request, identity, NgxPhase::Access, ops, || {
        match vm.run() {
            Ok(VmStatus::Halted) => Ok(vm
                .stack()
                .last()
                .cloned()
                .unwrap_or_else(|| panic!("expected a stack value for {source}"))),
            Ok(status) => Err(format!("unexpected status {status:?}")),
            Err(err) => Err(err.to_string()),
        }
    })
}

fn eval_in_request(request: *mut ngx_http_request_t, source: &str) -> Result<Value, String> {
    eval_in_request_ops(
        request,
        AllowedOps::for_phase(NgxPhase::Access),
        RequestIdentity::new(1, 1),
        source,
    )
}

fn expect_string(value: Value) -> String {
    match value {
        Value::String(text) => (*text).clone(),
        other => panic!("expected string, got {other:?}"),
    }
}

fn schema(name: &str) -> vm::HostFunctionSchema {
    ngx_host_catalog()
        .function(name)
        .unwrap_or_else(|| panic!("missing catalog schema {name}"))
        .clone()
}

#[test]
fn mutation_hosts_are_catalogued_unique_and_bound() {
    let runtime: Vec<&str> = production_runtime_host_names().to_vec();
    for name in MUTATION_HOSTS {
        assert!(
            runtime.contains(name),
            "{name} must be in production_runtime_host_names"
        );
        let entry = manifest_entry(name).unwrap_or_else(|| panic!("missing manifest {name}"));
        assert_eq!(entry.sync, HostSyncKind::Sync);
        assert!(host_allowed_in_phase(name, NgxPhase::Access));
        assert!(ngx_host_catalog().function(name).is_some());
        compile(&format!(
            "use ngx;\n{}{};\n",
            name,
            match *name {
                SET_METHOD | STATUS_SET => "(ngx::http::post())",
                SET_URI => "(\"/x\", false)",
                SET_URI_ARGS => "(\"a=1\")",
                SET_HEADER | HEADER_SET => "(\"X-Foo\", \"bar\")",
                CLEAR_HEADER => "(\"X-Foo\")",
                VAR_SET => "(\"foo\", \"bar\")",
                STATUS_GET => "()",
                HEADER_GET => "(\"X-Foo\")",
                HEADER_SET_ALL => "({ \"X-Foo\": \"bar\" })",
                HEADER_CLEAR => "()",
                _ => unreachable!(),
            }
        ));
    }
    assert!(ngx_host_catalog().function("ngx::exec").is_none());
    assert!(!runtime.contains(&"ngx::exec"));
    assert!(manifest_entry("ngx::exec").is_none());
}

#[test]
fn mutation_schemas_match_vm_types() {
    let set_method = schema(SET_METHOD);
    assert_eq!(set_method.params.len(), 1);
    assert_eq!(set_method.params[0].ty, HostTypeSchema::Int);
    assert_eq!(set_method.return_type, HostTypeSchema::Null);

    let set_uri = schema(SET_URI);
    assert_eq!(set_uri.params.len(), 2);
    assert_eq!(set_uri.params[0].ty, HostTypeSchema::String);
    assert_eq!(set_uri.params[1].ty, HostTypeSchema::Unknown);
    assert_eq!(set_uri.return_type, HostTypeSchema::Null);

    let set_args = schema(SET_URI_ARGS);
    assert_eq!(set_args.params.len(), 1);
    assert_eq!(set_args.params[0].ty, HostTypeSchema::Unknown);
    assert_eq!(set_args.return_type, HostTypeSchema::Null);

    let set_header = schema(SET_HEADER);
    assert_eq!(set_header.params.len(), 2);
    assert_eq!(set_header.params[0].ty, HostTypeSchema::String);
    assert_eq!(set_header.params[1].ty, HostTypeSchema::Unknown);

    let var_set = schema(VAR_SET);
    assert_eq!(var_set.params[0].ty, HostTypeSchema::String);
    assert_eq!(var_set.params[1].ty, HostTypeSchema::Unknown);

    let status_get = schema(STATUS_GET);
    assert_eq!(status_get.params.len(), 0);
    assert_eq!(status_get.return_type, HostTypeSchema::Int);

    let header_get = schema(HEADER_GET);
    assert_eq!(header_get.return_type, HostTypeSchema::Unknown);

    let set_all = schema(HEADER_SET_ALL);
    assert_eq!(set_all.params[0].ty, HostTypeSchema::Unknown);
}

#[test]
fn mutation_capabilities_do_not_broaden_request_read() {
    for name in [
        SET_METHOD,
        SET_URI,
        SET_URI_ARGS,
        SET_HEADER,
        CLEAR_HEADER,
        VAR_SET,
    ] {
        let entry = manifest_entry(name).unwrap();
        assert_eq!(entry.family, HostFamily::Request);
        assert!(entry.capabilities.contains(&HostCapability::RequestWrite));
        assert!(!entry.capabilities.contains(&HostCapability::RequestRead));
        assert!(!entry.capabilities.contains(&HostCapability::RequestCtx));
    }
    for name in [
        STATUS_GET,
        STATUS_SET,
        HEADER_GET,
        HEADER_SET,
        HEADER_SET_ALL,
        HEADER_CLEAR,
    ] {
        let entry = manifest_entry(name).unwrap();
        assert_eq!(entry.family, HostFamily::Response);
        assert!(entry.capabilities.contains(&HostCapability::ResponseWrite));
        assert!(!entry.capabilities.contains(&HostCapability::RequestRead));
        assert!(!entry.capabilities.contains(&HostCapability::RequestCtx));
    }
    let access = AllowedOps::for_phase(NgxPhase::Access);
    assert!(access.contains(AllowedOps::REQUEST_WRITE));
    assert!(access.contains(AllowedOps::RESPONSE_WRITE));
    assert!(access.contains(AllowedOps::REQUEST_READ));
}

#[test]
fn mutation_programs_require_request_owned_exec_reads_stay_pooled() {
    let mutation = compile("use ngx;\nngx::req::set_method(ngx::http::post());\n");
    assert!(program_requires_request_state(&mutation));
    let response = compile("use ngx;\nngx::status::set(201);\n");
    assert!(program_requires_request_state(&response));
    let read = compile("use ngx;\nngx::req::get_method();\n");
    assert!(
        !program_requires_request_state(&read),
        "pure request-read must remain pooled"
    );
    let config = include_str!("../src/config.rs");
    assert!(
        config.contains("HostCapability::RequestWrite")
            && config.contains("HostCapability::ResponseWrite")
            && config.contains("HostCapability::Control"),
        "request-state routing must use mutation/control capabilities, not host-name matching"
    );
    assert!(!config.contains("ngx::req::set_method"));
}

#[test]
fn exec_import_is_rejected_at_config_time() {
    let err = compile_err("use ngx;\nngx::exec(\"/x\");\n");
    let text = err.to_string();
    assert!(
        text.contains("ngx::exec"),
        "exec must be rejected as unknown/unavailable, got {err}"
    );
}

#[test]
fn set_method_updates_numeric_and_canonical_name() {
    nginx_stubs::stub_alloc_reset();
    nginx_stubs::stub_alloc_fail_after(8);
    let mut req = dummy_request();
    req.pool = std::ptr::dangling_mut();
    let mut get = *b"GET";
    req.method = NGX_HTTP_GET as ngx_uint_t;
    req.method_name = ngx_str(&mut get);
    let ptr = req.as_mut() as *mut ngx_http_request_t;
    assert_eq!(
        expect_string(
            eval_in_request(
                ptr,
                "use ngx;\nngx::req::set_method(ngx::http::post());\nngx::req::get_method();\n"
            )
            .expect("set_method")
        ),
        "POST"
    );
    assert_eq!(req.method, NGX_HTTP_POST as ngx_uint_t);
    nginx_stubs::stub_alloc_reset();
}

#[test]
fn set_method_rejects_unsupported_ids_before_mutation() {
    let mut req = dummy_request();
    let mut get = *b"GET";
    req.method = NGX_HTTP_GET as ngx_uint_t;
    req.method_name = ngx_str(&mut get);
    let ptr = req.as_mut() as *mut ngx_http_request_t;
    let err = eval_in_request(ptr, "use ngx;\nngx::req::set_method(12345);\n").expect_err("bad id");
    assert!(
        err.contains("method") || err.contains("unsupported"),
        "{err}"
    );
    assert_eq!(req.method, NGX_HTTP_GET as ngx_uint_t);
}

#[test]
fn set_uri_jump_true_fails_before_mutation() {
    let mut req = dummy_request();
    let mut uri = *b"/old";
    req.uri = ngx_str(&mut uri);
    let ptr = req.as_mut() as *mut ngx_http_request_t;
    let err = eval_in_request(ptr, "use ngx;\nngx::req::set_uri(\"/new\", true);\n")
        .expect_err("jump=true");
    assert!(
        err.contains("jump") || err.contains("not supported"),
        "{err}"
    );
    let got = unsafe { std::slice::from_raw_parts(req.uri.data, req.uri.len) };
    assert_eq!(got, b"/old");
}

#[test]
fn set_uri_rejects_empty_and_unsafe_before_allocation() {
    let mut req = dummy_request();
    let ptr = req.as_mut() as *mut ngx_http_request_t;
    let empty =
        eval_in_request(ptr, "use ngx;\nngx::req::set_uri(\"\", false);\n").expect_err("empty");
    assert!(
        empty.contains("zero-length") || empty.contains("empty"),
        "{empty}"
    );
    let unsafe_uri = eval_in_request(
        ptr,
        "use ngx;\nngx::req::set_uri(\"/foo\\r\\nbar\", false);\n",
    )
    .expect_err("unsafe");
    assert!(
        unsafe_uri.contains("unsafe") || unsafe_uri.contains("0x0d") || unsafe_uri.contains("CR"),
        "{unsafe_uri}"
    );
}

#[test]
fn set_uri_and_headers_need_pool_and_reject_injection() {
    let mut req = dummy_request();
    let ptr = req.as_mut() as *mut ngx_http_request_t;
    let no_pool = eval_in_request(ptr, "use ngx;\nngx::req::set_uri(\"/ok\", false);\n")
        .expect_err("null pool");
    assert!(
        no_pool.contains("pool") || no_pool.contains("memory"),
        "{no_pool}"
    );
    let inject = eval_in_request(
        ptr,
        "use ngx;\nngx::req::set_header(\"X-Foo\\r\\nX-Evil\", \"x\");\n",
    )
    .expect_err("inject name");
    assert!(
        inject.contains("token") || inject.contains("invalid") || inject.contains("header"),
        "{inject}"
    );
    let inject_val = eval_in_request(
        ptr,
        "use ngx;\nngx::req::set_header(\"X-Foo\", \"a\\r\\nb\");\n",
    )
    .expect_err("inject value");
    assert!(
        inject_val.contains("CR")
            || inject_val.contains("invalid")
            || inject_val.contains("header"),
        "{inject_val}"
    );
}

#[test]
fn mutation_requires_write_capability() {
    let mut req = dummy_request();
    let ptr = req.as_mut() as *mut ngx_http_request_t;
    let denied = eval_in_request_ops(
        ptr,
        AllowedOps::REQUEST_READ.union(AllowedOps::LOG),
        RequestIdentity::new(1, 1),
        "use ngx;\nngx::req::set_method(ngx::http::post());\n",
    )
    .expect_err("read-only");
    assert!(denied.contains("not allowed"), "{denied}");
    let resp_denied = eval_in_request_ops(
        ptr,
        AllowedOps::REQUEST_READ.union(AllowedOps::REQUEST_WRITE),
        RequestIdentity::new(1, 1),
        "use ngx;\nngx::status::set(201);\n",
    )
    .expect_err("no response write");
    assert!(resp_denied.contains("not allowed"), "{resp_denied}");
}

#[test]
fn status_get_set_and_headers_sent_reject() {
    let mut req = dummy_request();
    req.headers_out.status = 200;
    let ptr = req.as_mut() as *mut ngx_http_request_t;
    match eval_in_request(ptr, "use ngx;\nngx::status::get();\n").expect("get") {
        Value::Int(200) => {}
        other => panic!("expected 200, got {other:?}"),
    }
    eval_in_request(ptr, "use ngx;\nngx::status::set(201);\n").expect("set");
    assert_eq!(req.headers_out.status, 201);

    let mut abi = nginx_stubs::AbiRequest::new();
    abi.set_header_sent(true);
    let sent = abi.as_ptr() as *mut ngx_http_request_t;
    let status_err =
        eval_in_request(sent, "use ngx;\nngx::status::set(201);\n").expect_err("headers sent");
    assert!(status_err.contains("after they were sent"), "{status_err}");
    let header_err = eval_in_request(sent, "use ngx;\nngx::header::set(\"X-A\", \"1\");\n")
        .expect_err("headers sent");
    assert!(header_err.contains("after they were sent"), "{header_err}");
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
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

fn http_exchange(port: u16, request: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    stream.write_all(request.as_bytes()).unwrap();
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

fn write_script(path: &Path, body: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, body).unwrap();
}

fn mutation_conf(module: &str, prefix: &Path, port: u16, locations: &str) -> String {
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
fn nginx_request_mutation_method_uri_args_headers_vars_and_specials() {
    let nginx = nginx_binary();
    let module = module_cdylib();
    let port = free_port();
    let prefix = isolated_prefix("ngx-rustscript-req-mut");
    fs::create_dir_all(prefix.join("html")).unwrap();
    fs::write(prefix.join("html/index.html"), "ok\n").unwrap();
    let mut_script = prefix.join("conf/mutate.rss");
    write_script(
        &mut_script,
        r#"
use ngx;
ngx::req::set_method(ngx::http::post());
ngx::req::set_uri("/mut-new", false);
ngx::req::set_uri_args({ a: "1", a2: ["2", "3"], flag: true, empty: "" });
ngx::req::set_header("X-Foo", "bar");
ngx::req::set_header("X-Multi", ["one", "two"]);
ngx::req::set_header("Content-Type", "text/plain");
ngx::var::set("writable", "xyz");
ngx::log(ngx::log_level::notice(), "MUT_METHOD=" + ngx::req::get_method());
ngx::log(ngx::log_level::notice(), "MUT_INTERNAL=" + ngx::encode_args({ v: ngx::req::is_internal() }));
ngx::exit(204);
"#,
    );
    let jump_script = prefix.join("conf/jump.rss");
    write_script(
        &jump_script,
        r#"
use ngx;
ngx::req::set_uri("/jumped", true);
ngx::log(ngx::log_level::notice(), "JUMP_MARKER");
"#,
    );
    let ro_script = prefix.join("conf/rovar.rss");
    write_script(
        &ro_script,
        r#"
use ngx;
ngx::var::set("uri", "/nope");
"#,
    );
    let header_script = prefix.join("conf/headers.rss");
    write_script(
        &header_script,
        r#"
use ngx;
ngx::req::set_header("X-Dup", "first");
ngx::req::set_header("X-Dup", ["a", "b"]);
ngx::req::clear_header("X-Remove");
ngx::log(ngx::log_level::notice(), "HDR=" + ngx::encode_args(ngx::req::get_headers(null, null)["headers"]));
"#,
    );
    let resp_script = prefix.join("conf/resp.rss");
    write_script(
        &resp_script,
        r#"
use ngx;
ngx::status::set(201);
ngx::header::set("X-Out", "yes");
ngx::header::set("Content-Type", "text/plain");
ngx::header::set("X-Multi", ["m1", "m2"]);
ngx::exit(201);
ngx::log(ngx::log_level::notice(), "RESP_MARKER");
"#,
    );
    let set_all_script = prefix.join("conf/setall.rss");
    write_script(
        &set_all_script,
        r#"
use ngx;
ngx::header::set("X-Old", "stale");
ngx::header::set_all({ xa: "1", xb: "2" });
ngx::exit(200);
"#,
    );
    let clear_script = prefix.join("conf/clear.rss");
    write_script(
        &clear_script,
        r#"
use ngx;
ngx::header::set("X-Old", "stale");
ngx::header::clear();
ngx::header::set("X-New", "fresh");
ngx::exit(200);
"#,
    );
    let module_q = nginx_quote_path(&module).unwrap();
    let mut_q = nginx_quote_path(&mut_script).unwrap();
    let jump_q = nginx_quote_path(&jump_script).unwrap();
    let ro_q = nginx_quote_path(&ro_script).unwrap();
    let hdr_q = nginx_quote_path(&header_script).unwrap();
    let resp_q = nginx_quote_path(&resp_script).unwrap();
    let setall_q = nginx_quote_path(&set_all_script).unwrap();
    let clear_q = nginx_quote_path(&clear_script).unwrap();
    let conf = mutation_conf(
        &module_q,
        &prefix,
        port,
        &format!(
            r#"
        location /mut {{
            set $writable "";
            access_by_rss_file {mut_q};
            add_header X-Obs-Method $request_method always;
            add_header X-Obs-Uri $uri always;
            add_header X-Obs-Args $args always;
            add_header X-Obs-Foo $http_x_foo always;
            add_header X-Obs-Var $writable always;
            add_header X-Obs-Ct $content_type always;
        }}
        location /jump {{
            access_by_rss_file {jump_q};
        }}
        location /rovar {{
            access_by_rss_file {ro_q};
        }}
        location /hdr {{
            access_by_rss_file {hdr_q};
            try_files /index.html =204;
        }}
        location /resp {{
            access_by_rss_file {resp_q};
        }}
        location /setall {{
            access_by_rss_file {setall_q};
        }}
        location /clearh {{
            access_by_rss_file {clear_q};
        }}
"#
        ),
    );
    fs::write(prefix.join("conf/nginx.conf"), &conf).unwrap();
    let out = nginx_test(&nginx, &prefix);
    let text = combined(&out);
    assert!(out.status.success(), "nginx -t failed:\n{text}");
    let _child = spawn_nginx(&nginx, &prefix);
    wait_for_port(port, Duration::from_secs(5));

    let (status, body) = http_exchange(
        port,
        "GET /mut HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
    );
    let log = fs::read_to_string(prefix.join("logs/error.log")).unwrap_or_default();
    assert_eq!(status, 204, "mut body={body}\nlog={log}");
    assert!(
        body.contains("X-Obs-Method: POST") || body.contains("x-obs-method: POST"),
        "method: {body}"
    );
    assert!(body.contains("/mut-new"), "uri: {body}");
    assert!(
        body.contains("a=1") || body.contains("flag"),
        "args: {body}"
    );
    assert!(body.contains("bar"), "x-foo: {body}");
    assert!(body.contains("xyz"), "var: {body}");
    assert!(log.contains("MUT_METHOD=POST"), "{log}");
    assert!(
        log.contains("MUT_INTERNAL=v"),
        "set_uri jump=false must mark internal: {log}"
    );

    let (status, body) = http_exchange(
        port,
        "GET /jump HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
    );
    let log = fs::read_to_string(prefix.join("logs/error.log")).unwrap_or_default();
    assert_eq!(status, 500, "jump=true must fail: {body}");
    assert!(
        !log.contains("JUMP_MARKER"),
        "jump must stop before later log: {log}"
    );

    let (status, body) = http_exchange(
        port,
        "GET /rovar HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
    );
    assert_eq!(status, 500, "read-only var set must fail: {body}");

    let (status, body) = http_exchange(
        port,
        "GET /hdr HTTP/1.1\r\nHost: 127.0.0.1\r\nX-Remove: gone\r\nConnection: close\r\n\r\n",
    );
    let log = fs::read_to_string(prefix.join("logs/error.log")).unwrap_or_default();
    assert_eq!(status, 200, "{body}\n{log}");
    assert!(
        log.contains("x-dup=a") && log.contains("x-dup=b"),
        "duplicate set must replace with array values: {log}"
    );

    let (status, body) = http_exchange(
        port,
        "GET /resp HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
    );
    let log = fs::read_to_string(prefix.join("logs/error.log")).unwrap_or_default();
    assert_eq!(status, 201, "{body}");
    assert!(
        body.contains("X-Out: yes") || body.contains("x-out: yes"),
        "{body}"
    );
    assert!(
        body.contains("m1") && body.contains("m2"),
        "multi response headers: {body}"
    );
    assert!(!log.contains("RESP_MARKER"), "exit must stop VM: {log}");

    let (status, body) = http_exchange(
        port,
        "GET /setall HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
    );
    let log = fs::read_to_string(prefix.join("logs/error.log")).unwrap_or_default();
    assert_eq!(status, 200, "{body}\n{log}");
    assert!(
        body.contains("xa: 1") || body.contains("XA: 1") || body.contains("Xa: 1"),
        "{body}"
    );
    assert!(
        body.contains("xb: 2") || body.contains("XB: 2") || body.contains("Xb: 2"),
        "{body}"
    );
    assert!(
        !body.to_ascii_lowercase().contains("x-old"),
        "set_all must replace: {body}"
    );

    let (status, body) = http_exchange(
        port,
        "GET /clearh HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
    );
    assert_eq!(status, 200, "{body}");
    assert!(
        body.contains("X-New: fresh") || body.contains("x-new: fresh"),
        "{body}"
    );
    assert!(
        !body.to_ascii_lowercase().contains("x-old"),
        "clear then set: {body}"
    );
}
