//! Task 5 request-read hosts: catalog, scope/capability, owned copies, and
//! real NGINX semantics. Runtime dispatch is exercised through bound hosts,
//! not by scanning source.

use ngx_rustscript_nginx_test_stubs as nginx_stubs;

use std::fs;
use std::io::{Read, Write};
use std::mem::MaybeUninit;
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use nginx_sys::{
    NGX_HTTP_VERSION_10, NGX_HTTP_VERSION_11, ngx_http_request_t, ngx_list_part_t, ngx_str_t,
    ngx_table_elt_t,
};
use ngx_rustscript::config::{
    ConfigError, compile_rss_source, reject_program_imports, validate_program_imports_for_phase,
};
use ngx_rustscript::host::{
    AllowedOps, HostCapability, HostFamily, HostSyncKind, NgxPhase, bind_ngx_hosts,
    host_allowed_in_phase, manifest_entry, ngx_host_catalog, production_runtime_host_names,
    with_request_scope,
};
use ngx_rustscript::notify::RequestIdentity;
use vm::{HostTypeSchema, Program, Value, Vm, VmMap, VmStatus};

mod support;
use support::artifact::module_cdylib;
use support::nginx::{combined, nginx_binary, nginx_quote_path, nginx_test};
use support::temp::isolated_prefix;

const GET_METHOD: &str = "ngx::req::get_method";
const HTTP_VERSION: &str = "ngx::req::http_version";
const RAW_HEADER: &str = "ngx::req::raw_header";
const GET_HEADERS: &str = "ngx::req::get_headers";
const GET_URI_ARGS: &str = "ngx::req::get_uri_args";
const GET_POST_ARGS: &str = "ngx::req::get_post_args";
const IS_INTERNAL: &str = "ngx::req::is_internal";
const START_TIME: &str = "ngx::req::start_time";
const IS_SUBREQUEST: &str = "ngx::is_subrequest";
const HEADERS_SENT: &str = "ngx::headers_sent";
const VAR_GET: &str = "ngx::var::get";

const REQUEST_READ_HOSTS: &[&str] = &[
    GET_METHOD,
    HTTP_VERSION,
    RAW_HEADER,
    GET_HEADERS,
    GET_URI_ARGS,
    GET_POST_ARGS,
    IS_INTERNAL,
    START_TIME,
    IS_SUBREQUEST,
    HEADERS_SENT,
    VAR_GET,
];

fn compile(source: &str) -> Arc<Program> {
    compile_rss_source(source).unwrap_or_else(|err| panic!("compile failed: {err}"))
}

fn compile_err(source: &str) -> ConfigError {
    compile_rss_source(source).expect_err("source should fail configuration")
}

fn eval(source: &str) -> Result<Value, String> {
    let program = compile(source);
    let mut vm = Vm::try_new((*program).clone()).expect("construct vm");
    bind_ngx_hosts(&mut vm);
    match vm.run() {
        Ok(VmStatus::Halted) => Ok(vm
            .stack()
            .last()
            .cloned()
            .unwrap_or_else(|| panic!("expected a stack value for {source}"))),
        Ok(status) => Err(format!("unexpected status {status:?}")),
        Err(err) => Err(err.to_string()),
    }
}

fn eval_in_request(request: *mut ngx_http_request_t, source: &str) -> Result<Value, String> {
    eval_in_request_ops(
        request,
        AllowedOps::for_phase(NgxPhase::Access),
        RequestIdentity::new(1, 1),
        source,
    )
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

fn expect_string(value: Value) -> String {
    match value {
        Value::String(text) => (*text).clone(),
        other => panic!("expected string, got {other:?}"),
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

fn schema_named(name: &str) -> vm::HostFunctionSchema {
    ngx_host_catalog()
        .function(name)
        .cloned()
        .unwrap_or_else(|| panic!("missing catalog schema for {name}"))
}

#[test]
fn request_read_hosts_are_in_catalog_manifest_and_runtime() {
    nginx_stubs::force_link();
    let catalog = ngx_host_catalog();
    let runtime: std::collections::HashSet<&str> =
        production_runtime_host_names().iter().copied().collect();
    for name in REQUEST_READ_HOSTS {
        assert!(
            catalog.function(name).is_some(),
            "catalog must include {name}"
        );
        let entry = manifest_entry(name).unwrap_or_else(|| panic!("manifest must include {name}"));
        assert_eq!(entry.name, *name);
        assert_eq!(entry.family, HostFamily::Request);
        assert_eq!(entry.sync, HostSyncKind::Sync);
        assert!(
            entry.capabilities.contains(&HostCapability::RequestRead),
            "{name} must declare RequestRead"
        );
        assert!(
            entry.phases.contains(&NgxPhase::Access),
            "{name} must be allowed in access"
        );
        assert!(
            runtime.contains(name),
            "runtime bind list must include {name}"
        );
        assert!(
            production_runtime_host_names()
                .iter()
                .filter(|bound| **bound == *name)
                .count()
                == 1,
            "{name} must be bound exactly once"
        );
    }
}

#[test]
fn request_read_schemas_match_callable_namespaces() {
    let method = schema_named(GET_METHOD);
    assert!(method.params.is_empty());
    assert_eq!(method.return_type, HostTypeSchema::String);

    let version = schema_named(HTTP_VERSION);
    assert!(version.params.is_empty());
    assert_eq!(
        version.return_type,
        HostTypeSchema::Optional(Box::new(HostTypeSchema::Float))
    );

    let raw = schema_named(RAW_HEADER);
    assert_eq!(raw.params.len(), 1);
    assert_eq!(raw.params[0].ty, HostTypeSchema::Unknown);
    assert_eq!(raw.return_type, HostTypeSchema::String);

    let headers = schema_named(GET_HEADERS);
    assert_eq!(headers.params.len(), 2);
    assert_eq!(headers.params[0].ty, HostTypeSchema::Unknown);
    assert_eq!(headers.params[1].ty, HostTypeSchema::Unknown);
    assert_eq!(
        headers.return_type,
        HostTypeSchema::Map(Box::new(HostTypeSchema::Unknown))
    );

    let args = schema_named(GET_URI_ARGS);
    assert_eq!(args.params.len(), 1);
    assert_eq!(args.params[0].ty, HostTypeSchema::Unknown);
    assert_eq!(
        args.return_type,
        HostTypeSchema::Map(Box::new(HostTypeSchema::Unknown))
    );

    let post = schema_named(GET_POST_ARGS);
    assert_eq!(post.params.len(), 1);
    assert_eq!(
        post.return_type,
        HostTypeSchema::Map(Box::new(HostTypeSchema::Unknown))
    );

    assert_eq!(schema_named(IS_INTERNAL).return_type, HostTypeSchema::Bool);
    assert_eq!(schema_named(START_TIME).return_type, HostTypeSchema::Float);
    assert_eq!(
        schema_named(IS_SUBREQUEST).return_type,
        HostTypeSchema::Bool
    );
    assert_eq!(schema_named(HEADERS_SENT).return_type, HostTypeSchema::Bool);

    let var = schema_named(VAR_GET);
    assert_eq!(var.params.len(), 1);
    assert_eq!(var.params[0].ty, HostTypeSchema::String);
    assert_eq!(
        var.return_type,
        HostTypeSchema::Optional(Box::new(HostTypeSchema::String))
    );
}

#[test]
fn request_read_phases_match_openresty_v0_10_29() {
    assert!(host_allowed_in_phase(GET_METHOD, NgxPhase::Log));
    assert!(host_allowed_in_phase(GET_METHOD, NgxPhase::Balancer));
    assert!(!host_allowed_in_phase(GET_METHOD, NgxPhase::InitWorker));
    assert!(!host_allowed_in_phase(GET_METHOD, NgxPhase::Timer));

    assert!(host_allowed_in_phase(HTTP_VERSION, NgxPhase::HeaderFilter));
    assert!(!host_allowed_in_phase(HTTP_VERSION, NgxPhase::Log));
    assert!(!host_allowed_in_phase(HTTP_VERSION, NgxPhase::BodyFilter));
    assert!(!host_allowed_in_phase(RAW_HEADER, NgxPhase::Log));

    assert!(host_allowed_in_phase(GET_URI_ARGS, NgxPhase::Balancer));
    assert!(!host_allowed_in_phase(GET_POST_ARGS, NgxPhase::Set));
    assert!(!host_allowed_in_phase(GET_POST_ARGS, NgxPhase::Balancer));
    assert!(host_allowed_in_phase(GET_POST_ARGS, NgxPhase::Access));

    assert!(!host_allowed_in_phase(GET_HEADERS, NgxPhase::Balancer));
    assert!(host_allowed_in_phase(IS_INTERNAL, NgxPhase::Log));
    assert!(host_allowed_in_phase(START_TIME, NgxPhase::BodyFilter));
    assert!(host_allowed_in_phase(IS_SUBREQUEST, NgxPhase::Log));
    assert!(host_allowed_in_phase(HEADERS_SENT, NgxPhase::Set));
    assert!(!host_allowed_in_phase(HEADERS_SENT, NgxPhase::Log));
    assert!(host_allowed_in_phase(VAR_GET, NgxPhase::Balancer));
    assert!(!host_allowed_in_phase(VAR_GET, NgxPhase::Init));
}

#[test]
fn request_read_config_time_rejects_disallowed_phases() {
    let program = compile("use ngx;\nngx::req::get_method();\n");
    reject_program_imports(&program).expect("access phase allows get_method");
    let err = validate_program_imports_for_phase(&program, NgxPhase::InitWorker)
        .expect_err("get_method is not allowed in init_worker");
    let text = err.to_string();
    assert!(
        text.contains("ngx::req::get_method") && text.contains("init_worker"),
        "phase denial must name the host and phase, got {err}"
    );

    let program = compile("use ngx;\nngx::req::http_version();\n");
    let err = validate_program_imports_for_phase(&program, NgxPhase::Log)
        .expect_err("http_version is not allowed in log");
    assert!(err.to_string().contains("ngx::req::http_version"));
}

#[test]
fn request_read_hosts_fail_without_request_scope() {
    for source in [
        "use ngx;\nngx::req::get_method();\n",
        "use ngx;\nngx::req::http_version();\n",
        "use ngx;\nngx::req::raw_header(null);\n",
        "use ngx;\nngx::req::get_headers(null, null);\n",
        "use ngx;\nngx::req::get_uri_args(null);\n",
        "use ngx;\nngx::req::get_post_args(null);\n",
        "use ngx;\nngx::req::is_internal();\n",
        "use ngx;\nngx::req::start_time();\n",
        "use ngx;\nngx::is_subrequest();\n",
        "use ngx;\nngx::headers_sent();\n",
        "use ngx;\nngx::var::get(\"uri\");\n",
    ] {
        let err = eval(source).expect_err(source);
        assert!(
            err.contains("outside request execution scope") || err.contains("request"),
            "no-scope must fail the host, {source} -> {err}"
        );
    }
}

#[test]
fn request_read_capability_bypass_is_rejected_then_restored() {
    let mut req = dummy_request();
    let ptr = (&mut *req) as *mut ngx_http_request_t;
    let err = eval_in_request_ops(
        ptr,
        AllowedOps::LOG,
        RequestIdentity::new(1, 1),
        "use ngx;\nngx::req::get_method();\n",
    )
    .expect_err("LOG-only scope must deny request read");
    assert!(
        err.contains("not allowed"),
        "capability denial must mention allow-list, got {err}"
    );

    let mut method = b"GET".to_vec();
    req.method_name = ngx_str(&mut method);
    let value = eval_in_request_ops(
        ptr,
        AllowedOps::for_phase(NgxPhase::Access),
        RequestIdentity::new(1, 1),
        "use ngx;\nngx::req::get_method();\n",
    )
    .expect("restored REQUEST_READ must succeed");
    assert_eq!(expect_string(value), "GET");
}

#[test]
fn get_method_copies_custom_and_standard_names() {
    let mut req = dummy_request();
    let mut method = b"FOOBAR".to_vec();
    req.method_name = ngx_str(&mut method);
    req.method = 0;
    let ptr = (&mut *req) as *mut ngx_http_request_t;
    let value = eval_in_request(ptr, "use ngx;\nngx::req::get_method();\n").expect("method");
    let owned = expect_string(value);
    req.method_name = ngx_str_t {
        len: 0,
        data: std::ptr::null_mut(),
    };
    method.fill(0);
    assert_eq!(
        owned, "FOOBAR",
        "owned method must survive request mutation"
    );
}

#[test]
fn http_version_maps_enums_and_unknown_is_null() {
    let mut req = dummy_request();
    req.http_version = NGX_HTTP_VERSION_11 as _;
    let ptr = (&mut *req) as *mut ngx_http_request_t;
    match eval_in_request(ptr, "use ngx;\nngx::req::http_version();\n").expect("1.1") {
        Value::Float(value) => assert!((value - 1.1).abs() < 1e-9, "got {value}"),
        other => panic!("expected 1.1 float, got {other:?}"),
    }

    req.http_version = NGX_HTTP_VERSION_10 as _;
    match eval_in_request(ptr, "use ngx;\nngx::req::http_version();\n").expect("1.0") {
        Value::Float(value) => assert!((value - 1.0).abs() < 1e-9, "got {value}"),
        other => panic!("expected 1.0 float, got {other:?}"),
    }

    req.http_version = 1234;
    match eval_in_request(ptr, "use ngx;\nngx::req::http_version();\n").expect("unknown") {
        Value::Null => {}
        other => panic!("unrecognized HTTP version must be null, got {other:?}"),
    }
}

#[test]
fn flags_start_time_and_headers_sent_are_truthful() {
    let mut req = dummy_request();
    let ptr = (&mut *req) as *mut ngx_http_request_t;
    req.start_sec = 1_700_000_000;
    req.start_msec = 250;
    assert_eq!(
        eval_in_request(ptr, "use ngx;\nngx::req::is_internal();\n").expect("internal"),
        Value::Bool(false)
    );
    assert_eq!(
        eval_in_request(ptr, "use ngx;\nngx::is_subrequest();\n").expect("sub"),
        Value::Bool(false)
    );
    assert_eq!(
        eval_in_request(ptr, "use ngx;\nngx::headers_sent();\n").expect("sent"),
        Value::Bool(false)
    );
    match eval_in_request(ptr, "use ngx;\nngx::req::start_time();\n").expect("start") {
        Value::Float(value) => {
            assert!((value - 1_700_000_000.25).abs() < 1e-9, "got {value}")
        }
        other => panic!("expected start_time float, got {other:?}"),
    }

    req.set_internal(1);
    req.set_header_sent(1);
    assert_eq!(
        eval_in_request(ptr, "use ngx;\nngx::req::is_internal();\n").expect("internal"),
        Value::Bool(true)
    );
    assert_eq!(
        eval_in_request(ptr, "use ngx;\nngx::headers_sent();\n").expect("sent"),
        Value::Bool(true)
    );

    let mut main = dummy_request();
    req.main = (&mut *main) as *mut ngx_http_request_t;
    assert_eq!(
        eval_in_request(ptr, "use ngx;\nngx::is_subrequest();\n").expect("sub"),
        Value::Bool(true)
    );
}

#[test]
fn get_uri_args_uses_request_args_with_decode_args_shape() {
    let mut req = dummy_request();
    let mut args = b"a=1&b=2&a=3&flag&empty=".to_vec();
    req.args = ngx_str(&mut args);
    let ptr = (&mut *req) as *mut ngx_http_request_t;
    let decoded = eval_in_request(ptr, "use ngx;\nngx::req::get_uri_args(null);\n").expect("args");
    let root = expect_map(&decoded);
    assert_eq!(map_get(root, "truncated"), &Value::Bool(false));
    let parsed = expect_map(map_get(root, "args"));
    assert_eq!(map_get(parsed, "b"), &Value::string("2"));
    assert_eq!(map_get(parsed, "flag"), &Value::Bool(true));
    assert_eq!(map_get(parsed, "empty"), &Value::string(""));
    match map_get(parsed, "a") {
        Value::Array(values) => {
            assert_eq!(values.as_slice(), &[Value::string("1"), Value::string("3")]);
        }
        other => panic!("repeated keys must become an array, got {other:?}"),
    }

    let truncated = eval_in_request(ptr, "use ngx;\nngx::req::get_uri_args(2);\n").expect("cap");
    let truncated_root = expect_map(&truncated);
    assert_eq!(map_get(truncated_root, "truncated"), &Value::Bool(true));
    let truncated_args = expect_map(map_get(truncated_root, "args"));
    assert!(truncated_args.get(&Value::string("a")).is_some());
    assert!(truncated_args.get(&Value::string("b")).is_some());
    assert!(truncated_args.get(&Value::string("flag")).is_none());
}

#[test]
fn get_headers_canonical_raw_duplicates_empty_and_truncation() {
    let mut req = dummy_request();
    let mut host_key = b"Host".to_vec();
    let mut host_low = b"host".to_vec();
    let mut host_val = b"example.test".to_vec();
    let mut foo_key = b"X-Foo".to_vec();
    let mut foo_low = b"x-foo".to_vec();
    let mut foo_a = b"one".to_vec();
    let mut foo_b = b"two".to_vec();
    let mut empty_key = b"X-Empty".to_vec();
    let mut empty_low = b"x-empty".to_vec();
    let mut empty_val = Vec::new();
    let mut tables = [
        header_elt(&mut host_key, &mut host_low, &mut host_val, 1),
        header_elt(&mut foo_key, &mut foo_low, &mut foo_a, 2),
        header_elt(&mut foo_key, &mut foo_low, &mut foo_b, 2),
        header_elt(&mut empty_key, &mut empty_low, &mut empty_val, 3),
    ];
    install_headers(&mut req, &mut tables);
    let ptr = (&mut *req) as *mut ngx_http_request_t;

    let decoded =
        eval_in_request(ptr, "use ngx;\nngx::req::get_headers(null, null);\n").expect("headers");
    let root = expect_map(&decoded);
    assert_eq!(map_get(root, "truncated"), &Value::Bool(false));
    let headers = expect_map(map_get(root, "headers"));
    assert_eq!(map_get(headers, "host"), &Value::string("example.test"));
    assert_eq!(map_get(headers, "x-empty"), &Value::string(""));
    match map_get(headers, "x-foo") {
        Value::Array(values) => {
            assert_eq!(
                values.as_slice(),
                &[Value::string("one"), Value::string("two")]
            );
        }
        other => panic!("duplicate headers must become an array, got {other:?}"),
    }
    assert!(
        headers.get(&Value::string("Host")).is_none(),
        "canonical mode must not keep mixed-case keys"
    );

    let raw = eval_in_request(ptr, "use ngx;\nngx::req::get_headers(null, true);\n")
        .expect("raw headers");
    let raw_headers = expect_map(map_get(expect_map(&raw), "headers"));
    assert_eq!(map_get(raw_headers, "Host"), &Value::string("example.test"));
    assert!(raw_headers.get(&Value::string("host")).is_none());

    let truncated =
        eval_in_request(ptr, "use ngx;\nngx::req::get_headers(1, null);\n").expect("cap");
    let truncated_root = expect_map(&truncated);
    assert_eq!(map_get(truncated_root, "truncated"), &Value::Bool(true));
    let truncated_headers = expect_map(map_get(truncated_root, "headers"));
    assert_eq!(truncated_headers.len(), 1);

    let restored =
        eval_in_request(ptr, "use ngx;\nngx::req::get_headers(null, null);\n").expect("restore");
    assert_eq!(
        map_get(expect_map(&restored), "truncated"),
        &Value::Bool(false)
    );
}

#[test]
fn get_headers_skips_hash_zero_and_stops_on_corrupt_lists() {
    let mut req = dummy_request();
    let mut live_key = b"X-Live".to_vec();
    let mut live_low = b"x-live".to_vec();
    let mut live_val = b"ok".to_vec();
    let mut dead_key = b"X-Dead".to_vec();
    let mut dead_low = b"x-dead".to_vec();
    let mut dead_val = b"no".to_vec();
    let mut tables = [
        header_elt(&mut dead_key, &mut dead_low, &mut dead_val, 0),
        header_elt(&mut live_key, &mut live_low, &mut live_val, 1),
    ];
    install_headers(&mut req, &mut tables);
    let ptr = (&mut *req) as *mut ngx_http_request_t;
    let decoded =
        eval_in_request(ptr, "use ngx;\nngx::req::get_headers(null, null);\n").expect("headers");
    let headers = expect_map(map_get(expect_map(&decoded), "headers"));
    assert!(headers.get(&Value::string("x-dead")).is_none());
    assert_eq!(map_get(headers, "x-live"), &Value::string("ok"));

    req.headers_in.headers.part.elts = std::ptr::null_mut();
    req.headers_in.headers.part.nelts = 8;
    let empty = eval_in_request(ptr, "use ngx;\nngx::req::get_headers(null, null);\n")
        .expect("null elts must not crash");
    assert_eq!(
        map_get(expect_map(&empty), "truncated"),
        &Value::Bool(false)
    );

    let mut cycle = ngx_list_part_t {
        elts: std::ptr::null_mut(),
        nelts: 0,
        next: std::ptr::null_mut(),
    };
    cycle.next = &mut cycle;
    req.headers_in.headers.part = cycle;
    eval_in_request(ptr, "use ngx;\nngx::req::get_headers(null, null);\n")
        .expect("cyclic list must not hang");
}

#[test]
fn raw_header_copies_request_line_and_optional_skip() {
    let mut req = dummy_request();
    let mut buf = b"GET /t HTTP/1.1\r\nHost: localhost\r\n\r\n".to_vec();
    let line_len = b"GET /t HTTP/1.1".len();
    req.request_line = ngx_str_t {
        len: line_len,
        data: buf.as_mut_ptr(),
    };
    req.request_start = buf.as_mut_ptr();
    req.header_end = unsafe { buf.as_mut_ptr().add(buf.len()) };
    let ptr = (&mut *req) as *mut ngx_http_request_t;
    let raw = expect_string(
        eval_in_request(ptr, "use ngx;\nngx::req::raw_header(null);\n").expect("raw"),
    );
    assert!(raw.contains("GET /t HTTP/1.1"), "{raw:?}");
    assert!(raw.contains("Host: localhost"), "{raw:?}");

    let skipped = expect_string(
        eval_in_request(ptr, "use ngx;\nngx::req::raw_header(true);\n").expect("skip"),
    );
    assert!(!skipped.contains("GET /t HTTP/1.1"), "{skipped:?}");
    assert!(skipped.contains("Host: localhost"), "{skipped:?}");
}

#[test]
fn get_post_args_unread_body_is_task10_host_error() {
    let mut req = dummy_request();
    req.request_body = std::ptr::null_mut();
    let ptr = (&mut *req) as *mut ngx_http_request_t;
    let err = eval_in_request(ptr, "use ngx;\nngx::req::get_post_args(null);\n")
        .expect_err("unread body must fail");
    assert!(
        err.contains("body") && (err.contains("Task 10") || err.contains("task 10")),
        "unread body must defer to Task 10, got {err}"
    );
}

#[test]
fn var_get_validates_names_and_distinguishes_missing_empty_found() {
    let mut req = dummy_request();
    let ptr = (&mut *req) as *mut ngx_http_request_t;
    assert_eq!(
        eval_in_request(ptr, "use ngx;\nngx::var::get(\"missing\");\n").expect("missing"),
        Value::Null
    );
    assert_eq!(
        eval_in_request(ptr, "use ngx;\nngx::var::get(\"empty\");\n").expect("empty"),
        Value::string("")
    );
    assert_eq!(
        eval_in_request(ptr, "use ngx;\nngx::var::get(\"found\");\n").expect("found"),
        Value::string("stub-value")
    );

    let empty_name =
        eval_in_request(ptr, "use ngx;\nngx::var::get(\"\");\n").expect_err("empty variable name");
    assert!(
        empty_name.contains("variable") || empty_name.contains("name"),
        "{empty_name}"
    );
    let nul = eval_in_request(
        ptr,
        "use ngx;\nngx::var::get(ngx::unescape_uri(\"a%00b\"));\n",
    )
    .expect_err("NUL in variable name");
    assert!(
        nul.contains("NUL") || nul.contains("null") || nul.contains("invalid"),
        "{nul}"
    );
}

#[test]
fn owned_values_survive_request_scope_exit() {
    let mut method = *b"PATCH";
    let mut req = dummy_request();
    req.method_name = ngx_str(&mut method);
    let ptr = (&mut *req) as *mut ngx_http_request_t;
    let value = eval_in_request(ptr, "use ngx;\nngx::req::get_method();\n").expect("method");
    drop(req);
    assert_eq!(value, Value::string("PATCH"));
}

fn header_elt(
    key: &mut [u8],
    lowcase: &mut [u8],
    value: &mut [u8],
    hash: usize,
) -> ngx_table_elt_t {
    ngx_table_elt_t {
        hash,
        key: ngx_str(key),
        value: ngx_str(value),
        lowcase_key: lowcase.as_mut_ptr(),
        next: std::ptr::null_mut(),
    }
}

fn install_headers(req: &mut ngx_http_request_t, tables: &mut [ngx_table_elt_t]) {
    req.headers_in.headers.part.elts = tables.as_mut_ptr().cast();
    req.headers_in.headers.part.nelts = tables.len();
    req.headers_in.headers.part.next = std::ptr::null_mut();
    req.headers_in.headers.last = &mut req.headers_in.headers.part;
    req.headers_in.headers.size = std::mem::size_of::<ngx_table_elt_t>();
    req.headers_in.headers.nalloc = tables.len();
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

fn request_conf(module: &str, prefix: &Path, port: u16, locations: &str) -> String {
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

fn log_field<'a>(log: &'a str, needle: &str) -> &'a str {
    let line = log
        .lines()
        .find(|line| line.contains(needle))
        .unwrap_or_else(|| panic!("missing {needle} in log:\n{log}"));
    line.rsplit_once(needle)
        .map(|(_, rest)| rest.split(", client:").next().unwrap_or(rest).trim())
        .unwrap_or("")
}

#[test]
fn nginx_request_read_semantics_methods_headers_args_vars_and_internal() {
    let nginx = nginx_binary();
    let module = module_cdylib();
    let port = free_port();
    let prefix = isolated_prefix("ngx-rustscript-req-read");
    write_index(&prefix, "read/index.html", "read-ok\n");
    write_index(&prefix, "internal-read/index.html", "internal-ok\n");
    write_index(&prefix, "post-read/index.html", "post-ok\n");
    let script = prefix.join("conf/request_read.rss");
    write_script(
        &script,
        r#"
use ngx;
ngx::log(ngx::log_level::notice(), "REQ_METHOD=" + ngx::req::get_method());
ngx::log(ngx::log_level::notice(), "REQ_VERSION=" + ngx::encode_args({ v: ngx::req::http_version() }));
if ngx::req::is_internal() {
    ngx::log(ngx::log_level::notice(), "REQ_INTERNAL=true");
} else {
    ngx::log(ngx::log_level::notice(), "REQ_INTERNAL=false");
}
if ngx::is_subrequest() {
    ngx::log(ngx::log_level::notice(), "REQ_SUB=true");
} else {
    ngx::log(ngx::log_level::notice(), "REQ_SUB=false");
}
if ngx::headers_sent() {
    ngx::log(ngx::log_level::notice(), "REQ_SENT=true");
} else {
    ngx::log(ngx::log_level::notice(), "REQ_SENT=false");
}
ngx::log(ngx::log_level::notice(), "REQ_START=" + ngx::encode_args({ v: ngx::req::start_time() }));
ngx::log(ngx::log_level::notice(), "REQ_RAW=" + ngx::req::raw_header(null));
let headers = ngx::req::get_headers(null, null);
ngx::log(ngx::log_level::notice(), "REQ_HEADERS=" + ngx::encode_args(headers["headers"]));
ngx::log(ngx::log_level::notice(), "REQ_HEADERS_TRUNC=" + ngx::encode_args({ v: headers["truncated"] }));
let raw_headers = ngx::req::get_headers(null, true);
ngx::log(ngx::log_level::notice(), "REQ_RAW_HEADERS=" + ngx::encode_args(raw_headers["headers"]));
let capped = ngx::req::get_headers(1, null);
ngx::log(ngx::log_level::notice(), "REQ_HEADERS_CAP=" + ngx::encode_args({ v: capped["truncated"] }));
let args = ngx::req::get_uri_args(null);
ngx::log(ngx::log_level::notice(), "REQ_ARGS=" + ngx::encode_args(args["args"]));
ngx::log(ngx::log_level::notice(), "REQ_ARGS_TRUNC=" + ngx::encode_args({ v: args["truncated"] }));
ngx::log(ngx::log_level::notice(), "REQ_VARS=" + ngx::encode_args({ uri: ngx::var::get("uri"), request_uri: ngx::var::get("request_uri"), args: ngx::var::get("args"), host: ngx::var::get("host"), missing: ngx::var::get("no_such_rss_var"), empty: ngx::var::get("rss_empty") }));
"#,
    );
    let post_script = prefix.join("conf/post_args.rss");
    write_script(
        &post_script,
        r#"
use ngx;
ngx::req::get_post_args(null);
"#,
    );
    let internal_script = prefix.join("conf/internal_read.rss");
    write_script(
        &internal_script,
        r#"
use ngx;
ngx::log(ngx::log_level::notice(), "REQ_INTERNAL_LOC=" + ngx::encode_args({ internal: ngx::req::is_internal(), uri: ngx::var::get("uri") }));
"#,
    );
    let conf = request_conf(
        &nginx_quote_path(&module).unwrap(),
        &prefix,
        port,
        &format!(
            r#"
        set $rss_empty "";
        location /read/ {{
            access_by_rss_file {script};
        }}
        location /post-read/ {{
            access_by_rss_file {post_script};
        }}
        location /trigger-internal {{
            error_page 418 = /internal-read/;
            return 418;
        }}
        location /internal-read/ {{
            internal;
            access_by_rss_file {internal};
        }}
"#,
            script = nginx_quote_path(&script).unwrap(),
            post_script = nginx_quote_path(&post_script).unwrap(),
            internal = nginx_quote_path(&internal_script).unwrap(),
        ),
    );
    fs::write(prefix.join("conf/nginx.conf"), &conf).unwrap();
    let out = nginx_test(&nginx, &prefix);
    let text = combined(&out);
    assert!(
        out.status.success(),
        "request-read script must pass nginx -t:\n{text}"
    );

    let _child = spawn_nginx(&nginx, &prefix);
    wait_for_port(port, Duration::from_secs(5));

    let (status, response) = http_exchange(
        port,
        "GET /read/?a=1&a=2&flag&empty= HTTP/1.1\r\nHost: 127.0.0.1\r\nX-Foo: one\r\nX-Foo: two\r\nX-Empty: \r\nConnection: close\r\n\r\n",
    );
    let log = fs::read_to_string(prefix.join("logs/error.log")).unwrap_or_default();
    assert_eq!(status, 200, "response={response}\nerror.log={log}");
    assert!(
        response.contains("read-ok"),
        "content must run after request-read script; got:\n{response}"
    );

    let (status, response) = http_exchange(
        port,
        "FOOBAR /read/ HTTP/1.0\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
    );
    assert!(
        status == 200 || status == 405,
        "custom method should reach access; got:\n{response}"
    );

    let (status, response) = http_exchange(
        port,
        "POST /post-read/ HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 3\r\nContent-Type: application/x-www-form-urlencoded\r\nConnection: close\r\n\r\na=1",
    );
    assert_eq!(
        status, 500,
        "unread POST body must fail get_post_args; got:\n{response}"
    );

    let (status, response) = http_exchange(
        port,
        "GET /trigger-internal HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
    );
    assert_eq!(status, 200, "{response}");

    let log = fs::read_to_string(prefix.join("logs/error.log")).unwrap_or_default();
    assert!(
        log.contains("REQ_METHOD=GET"),
        "GET method must be logged; got:\n{log}"
    );
    assert!(
        log.contains("REQ_METHOD=FOOBAR"),
        "custom method must retain the textual name; got:\n{log}"
    );
    assert!(
        log.contains("REQ_VERSION=v=1.1") || log.contains("REQ_VERSION=v=1%2E1"),
        "HTTP/1.1 must map to 1.1; got:\n{log}"
    );
    assert!(
        log.lines().any(|line| {
            line.contains("FOOBAR /read/ HTTP/1.0")
                && (line.contains("REQ_VERSION=v=1.0")
                    || line.contains("REQ_VERSION=v=1") && !line.contains("v=1.1"))
        }),
        "HTTP/1.0 must map to 1.0; got:\n{log}"
    );
    assert!(
        log.contains("REQ_INTERNAL=false"),
        "normal requests are not internal; got:\n{log}"
    );
    assert!(
        log.contains("internal-read"),
        "error_page must internally redirect to /internal-read/; got:\n{log}"
    );
    assert!(
        log.contains("REQ_SUB=false"),
        "normal requests are not subrequests; got:\n{log}"
    );
    assert!(
        log.contains("REQ_SENT=false"),
        "access phase must not have sent headers; got:\n{log}"
    );
    let start = log_field(&log, "REQ_START=");
    assert!(
        start.contains("v="),
        "start_time must be logged, got {start}"
    );
    assert!(
        log.contains("GET /read/?a=1&a=2&flag&empty= HTTP/1.1"),
        "raw_header must include the request line; got:\n{log}"
    );
    let headers = log_field(&log, "REQ_HEADERS=");
    assert!(
        headers.contains("x-foo=one") && headers.contains("x-foo=two"),
        "canonical duplicate headers must be preserved, got {headers}"
    );
    assert!(
        headers.contains("x-empty="),
        "empty header values must be present, got {headers}"
    );
    let raw_headers = log_field(&log, "REQ_RAW_HEADERS=");
    assert!(
        raw_headers.contains("X-Foo=one") || raw_headers.contains("X-Foo=two"),
        "raw mode must keep original header names, got {raw_headers}"
    );
    assert!(
        log.contains("REQ_HEADERS_CAP=v"),
        "header cap must set truncated; got:\n{log}"
    );
    let args = log_field(&log, "REQ_ARGS=");
    assert!(
        args.contains("a=1")
            && args.contains("a=2")
            && args.contains("flag")
            && args.contains("empty="),
        "uri args must keep repeated/flag/empty keys, got {args}"
    );
    let vars = log_field(&log, "REQ_VARS=");
    assert!(
        vars.contains("uri=%2Fread%2F") || vars.contains("uri=/read/"),
        "uri variable must be the location URI, got {vars}"
    );
    assert!(
        vars.contains("request_uri="),
        "request_uri must be present, got {vars}"
    );
    assert!(
        !vars.contains("missing="),
        "missing variables must be null/omitted, got {vars}"
    );
    assert!(
        vars.contains("empty="),
        "configured empty variable must be empty, not missing, got {vars}"
    );
    assert!(
        log.contains("get_post_args") || log.contains("Task 10") || log.contains("body"),
        "unread body must surface a Task 10 host error; got:\n{log}"
    );
}

#[test]
fn request_read_unknown_import_still_rejected() {
    let err = compile_err("use ngx;\nngx::req::not_a_host();\n");
    let text = err.to_string();
    assert!(
        text.contains("ngx::req::not_a_host"),
        "unknown request host must be rejected, got {err}"
    );
}
