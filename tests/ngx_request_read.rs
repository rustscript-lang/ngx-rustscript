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
    NGX_HTTP_VERSION_10, NGX_HTTP_VERSION_11, NGX_HTTP_VERSION_20, NGX_HTTP_VERSION_30, ngx_buf_t,
    ngx_chain_t, ngx_connection_t, ngx_http_connection_t, ngx_http_request_body_t,
    ngx_http_request_t, ngx_http_v2_stream_t, ngx_list_part_t, ngx_str_t, ngx_table_elt_t,
    ngx_temp_file_t,
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

fn zeroed_box<T>() -> Box<T> {
    Box::new(unsafe { MaybeUninit::<T>::zeroed().assume_init() })
}

#[allow(clippy::vec_box)]
struct LiveHeaderBuffers {
    _conn: Box<ngx_connection_t>,
    _hc: Box<ngx_http_connection_t>,
    _bufs: Vec<Box<ngx_buf_t>>,
    _chains: Vec<Box<ngx_chain_t>>,
    _dummy: Option<Vec<u8>>,
}

fn fill_header_buf(storage: &mut [u8]) -> Box<ngx_buf_t> {
    let mut buf = zeroed_box::<ngx_buf_t>();
    buf.start = storage.as_mut_ptr();
    buf.pos = unsafe { storage.as_mut_ptr().add(storage.len()) };
    buf.last = buf.pos;
    buf.end = buf.pos;
    buf
}

fn install_single_header_buffer(
    req: &mut ngx_http_request_t,
    storage: &mut [u8],
    request_line_len: usize,
) -> LiveHeaderBuffers {
    let mut buf = fill_header_buf(storage);
    let mut conn = zeroed_box::<ngx_connection_t>();
    conn.buffer = buf.as_mut() as *mut ngx_buf_t;
    let mut hc = zeroed_box::<ngx_http_connection_t>();
    req.connection = conn.as_mut() as *mut ngx_connection_t;
    req.http_connection = hc.as_mut() as *mut ngx_http_connection_t as *mut _;
    req.header_in = buf.as_mut() as *mut ngx_buf_t;
    req.request_line = ngx_str_t {
        len: request_line_len,
        data: storage.as_mut_ptr(),
    };
    req.request_start = std::ptr::null_mut();
    req.header_end = std::ptr::null_mut();
    LiveHeaderBuffers {
        _conn: conn,
        _hc: hc,
        _bufs: vec![buf],
        _chains: Vec::new(),
        _dummy: None,
    }
}

fn install_busy_header_buffers(
    req: &mut ngx_http_request_t,
    chunks: &mut [Vec<u8>],
    request_line_len: usize,
) -> LiveHeaderBuffers {
    assert!(!chunks.is_empty());
    let mut dummy_storage = b"not-the-request-line".to_vec();
    let mut dummy_buf = fill_header_buf(&mut dummy_storage);
    let mut conn = zeroed_box::<ngx_connection_t>();
    conn.buffer = dummy_buf.as_mut() as *mut ngx_buf_t;

    let mut bufs: Vec<Box<ngx_buf_t>> = chunks
        .iter_mut()
        .map(|chunk| fill_header_buf(chunk))
        .collect();
    let mut chains: Vec<Box<ngx_chain_t>> = (0..bufs.len())
        .map(|_| zeroed_box::<ngx_chain_t>())
        .collect();
    // NGINX busy chain is newest-first: last chunk -> ... -> first chunk.
    for index in (0..chains.len()).rev() {
        chains[index].buf = bufs[index].as_mut() as *mut ngx_buf_t;
        if index == 0 {
            chains[index].next = std::ptr::null_mut();
        } else {
            let prev = chains[index - 1].as_mut() as *mut ngx_chain_t;
            chains[index].next = prev;
        }
    }
    let newest = chains.len() - 1;
    let mut hc = zeroed_box::<ngx_http_connection_t>();
    hc.busy = chains[newest].as_mut() as *mut ngx_chain_t;
    hc.nbusy = chains.len() as _;
    req.connection = conn.as_mut() as *mut ngx_connection_t;
    req.http_connection = hc.as_mut() as *mut ngx_http_connection_t as *mut _;
    req.header_in = bufs[newest].as_mut() as *mut ngx_buf_t;
    req.request_line = ngx_str_t {
        len: request_line_len,
        data: chunks[0].as_mut_ptr(),
    };
    req.request_start = std::ptr::null_mut();
    req.header_end = std::ptr::null_mut();
    let mut all_bufs = vec![dummy_buf];
    all_bufs.append(&mut bufs);
    LiveHeaderBuffers {
        _conn: conn,
        _hc: hc,
        _bufs: all_bufs,
        _chains: chains,
        _dummy: Some(dummy_storage),
    }
}

#[allow(clippy::vec_box)]
struct LiveBody {
    body: Box<ngx_http_request_body_t>,
    _bufs: Vec<Box<ngx_buf_t>>,
    _chains: Vec<Box<ngx_chain_t>>,
}

fn install_in_memory_body(req: &mut ngx_http_request_t, chunks: &mut [Vec<u8>]) -> LiveBody {
    let mut body = zeroed_box::<ngx_http_request_body_t>();
    let mut bufs: Vec<Box<ngx_buf_t>> = chunks
        .iter_mut()
        .map(|chunk| {
            let mut buf = zeroed_box::<ngx_buf_t>();
            buf.start = chunk.as_mut_ptr();
            buf.pos = chunk.as_mut_ptr();
            buf.last = unsafe { chunk.as_mut_ptr().add(chunk.len()) };
            buf.end = buf.last;
            buf
        })
        .collect();
    let mut chains: Vec<Box<ngx_chain_t>> = (0..bufs.len())
        .map(|_| zeroed_box::<ngx_chain_t>())
        .collect();
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
fn get_uri_args_matches_decode_args_trailing_amp_invalid_percent_nul_empty_keys_and_max_zero() {
    let mut req = dummy_request();
    let mut args = b"a=1&b=2&".to_vec();
    req.args = ngx_str(&mut args);
    let ptr = (&mut *req) as *mut ngx_http_request_t;
    let decoded = eval_in_request(ptr, "use ngx;\nngx::req::get_uri_args(2);\n").expect("trail");
    assert_eq!(
        map_get(expect_map(&decoded), "truncated"),
        &Value::Bool(false),
        "trailing empty & at max_args must not mark truncated"
    );

    let mut args = b"a=%ZZb&b=%00c&=skip&ok=1".to_vec();
    req.args = ngx_str(&mut args);
    let decoded = eval_in_request(ptr, "use ngx;\nngx::req::get_uri_args(null);\n").expect("pct");
    let parsed = expect_map(map_get(expect_map(&decoded), "args"));
    assert_eq!(map_get(parsed, "a"), &Value::string("%ZZb"));
    assert_eq!(map_get(parsed, "b"), &Value::string("\0c"));
    assert!(parsed.get(&Value::string("")).is_none());
    assert_eq!(map_get(parsed, "ok"), &Value::string("1"));

    let codec =
        eval("use ngx;\nngx::decode_args(\"a=%ZZb&b=%00c&=skip&ok=1\", null);\n").expect("codec");
    assert_eq!(
        decoded, codec,
        "get_uri_args must share decode_args behavior"
    );

    let mut many = (0..1024)
        .map(|i| format!("k{i}=1"))
        .collect::<Vec<_>>()
        .join("&")
        .into_bytes();
    req.args = ngx_str(&mut many);
    let at_cap = eval_in_request(ptr, "use ngx;\nngx::req::get_uri_args(0);\n").expect("max0");
    assert_eq!(
        map_get(expect_map(&at_cap), "truncated"),
        &Value::Bool(false)
    );
    assert_eq!(expect_map(map_get(expect_map(&at_cap), "args")).len(), 1024);
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
fn get_headers_multipart_count_and_byte_caps_set_truncated_without_partial_false() {
    let mut req = dummy_request();
    let mut keys = [
        (b"X-A".to_vec(), b"x-a".to_vec(), b"1".to_vec()),
        (b"X-B".to_vec(), b"x-b".to_vec(), b"2".to_vec()),
        (b"X-C".to_vec(), b"x-c".to_vec(), b"3".to_vec()),
        (b"X-D".to_vec(), b"x-d".to_vec(), b"4".to_vec()),
    ];
    let mut part1 = [
        header_elt(&mut keys[0].0, &mut keys[0].1, &mut keys[0].2, 1),
        header_elt(&mut keys[1].0, &mut keys[1].1, &mut keys[1].2, 2),
    ];
    let mut part2 = [
        header_elt(&mut keys[2].0, &mut keys[2].1, &mut keys[2].2, 3),
        header_elt(&mut keys[3].0, &mut keys[3].1, &mut keys[3].2, 4),
    ];
    let mut second = ngx_list_part_t {
        elts: part2.as_mut_ptr().cast(),
        nelts: part2.len(),
        next: std::ptr::null_mut(),
    };
    req.headers_in.headers.part.elts = part1.as_mut_ptr().cast();
    req.headers_in.headers.part.nelts = part1.len();
    req.headers_in.headers.part.next = &mut second;
    req.headers_in.headers.last = &mut second;
    req.headers_in.headers.size = std::mem::size_of::<ngx_table_elt_t>();
    req.headers_in.headers.nalloc = 2;
    let ptr = (&mut *req) as *mut ngx_http_request_t;
    let capped = eval_in_request(ptr, "use ngx;\nngx::req::get_headers(3, null);\n").expect("cap");
    let root = expect_map(&capped);
    assert_eq!(map_get(root, "truncated"), &Value::Bool(true));
    let headers = expect_map(map_get(root, "headers"));
    assert_eq!(headers.len(), 3);
    assert!(headers.get(&Value::string("x-d")).is_none());
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
    let err = eval_in_request(ptr, "use ngx;\nngx::req::get_headers(null, null);\n")
        .expect_err("null elts with nelts>0 must be a host error");
    assert!(
        err.to_ascii_lowercase().contains("header")
            || err.to_ascii_lowercase().contains("malformed")
            || err.to_ascii_lowercase().contains("invalid"),
        "corrupt elts must surface a structured host error, got {err}"
    );

    let mut cycle = ngx_list_part_t {
        elts: std::ptr::null_mut(),
        nelts: 0,
        next: std::ptr::null_mut(),
    };
    cycle.next = &mut cycle;
    req.headers_in.headers.part = cycle;
    let cycle_err = eval_in_request(ptr, "use ngx;\nngx::req::get_headers(null, null);\n")
        .expect_err("cyclic list must be a host error");
    assert!(
        cycle_err.to_ascii_lowercase().contains("header")
            || cycle_err.to_ascii_lowercase().contains("cycle")
            || cycle_err.to_ascii_lowercase().contains("malformed")
            || cycle_err.to_ascii_lowercase().contains("invalid"),
        "cyclic list must surface a structured host error, got {cycle_err}"
    );
}

#[test]
fn raw_header_copies_request_line_and_optional_skip() {
    let mut req = dummy_request();
    let mut buf = b"GET /t HTTP/1.1\r\nHost: localhost\r\n\r\n".to_vec();
    let line_len = b"GET /t HTTP/1.1".len();
    let _live = install_single_header_buffer(&mut req, &mut buf, line_len);
    let ptr = (&mut *req) as *mut ngx_http_request_t;
    let raw = expect_string(
        eval_in_request(ptr, "use ngx;\nngx::req::raw_header(null);\n").expect("raw"),
    );
    assert!(raw.contains("GET /t HTTP/1.1"), "{raw:?}");
    assert!(raw.contains("Host: localhost"), "{raw:?}");
    assert!(
        raw.contains("\r\n"),
        "raw_header must keep CRLF separators, got {raw:?}"
    );

    let skipped = expect_string(
        eval_in_request(ptr, "use ngx;\nngx::req::raw_header(true);\n").expect("skip"),
    );
    assert!(!skipped.contains("GET /t HTTP/1.1"), "{skipped:?}");
    assert!(skipped.contains("Host: localhost"), "{skipped:?}");
    assert!(skipped.starts_with("Host:"), "{skipped:?}");
}

#[test]
fn raw_header_restores_parser_nuls_to_colon_and_crlf() {
    let mut req = dummy_request();
    let mut buf = b"GET /t HTTP/1.1\r\nHost\0 example.test\0\nX-Foo\0 bar\0\n\r\n".to_vec();
    let line_len = b"GET /t HTTP/1.1".len();
    let _live = install_single_header_buffer(&mut req, &mut buf, line_len);
    let ptr = (&mut *req) as *mut ngx_http_request_t;
    let raw = expect_string(
        eval_in_request(ptr, "use ngx;\nngx::req::raw_header(null);\n").expect("raw"),
    );
    assert!(
        raw.contains("Host: example.test\r\n"),
        "parser NULs must become colon/CRLF, got {raw:?}"
    );
    assert!(
        raw.contains("X-Foo: bar\r\n"),
        "second header must restore colon/CRLF, got {raw:?}"
    );
    assert!(
        !raw.contains('\0'),
        "raw_header must not leak parser NULs, got {raw:?}"
    );
}

#[test]
fn raw_header_walks_busy_buffers_and_strips_preread_body() {
    let mut req = dummy_request();
    let mut chunks = [
        b"GET /t HTTP/1.1\r\nHost\0 x\0\n".to_vec(),
        b"X-Foo\0 y\0\n\r\npreread-body".to_vec(),
    ];
    let line_len = b"GET /t HTTP/1.1".len();
    let _live = install_busy_header_buffers(&mut req, &mut chunks, line_len);
    let ptr = (&mut *req) as *mut ngx_http_request_t;
    let raw = expect_string(
        eval_in_request(ptr, "use ngx;\nngx::req::raw_header(null);\n").expect("raw"),
    );
    assert!(raw.contains("GET /t HTTP/1.1\r\n"), "{raw:?}");
    assert!(raw.contains("Host: x\r\n"), "{raw:?}");
    assert!(raw.contains("X-Foo: y\r\n"), "{raw:?}");
    assert!(
        !raw.contains("preread-body"),
        "preread body after CRLFCRLF must be stripped, got {raw:?}"
    );
}

#[test]
fn raw_header_rejects_http2_and_http3_on_main_not_current_stream() {
    let mut main = dummy_request();
    let mut child = dummy_request();
    child.main = (&mut *main) as *mut ngx_http_request_t;
    child.stream = std::ptr::null_mut();
    child.http_version = NGX_HTTP_VERSION_11 as _;
    main.http_version = NGX_HTTP_VERSION_11 as _;
    let mut line = b"GET /t HTTP/1.1".to_vec();
    child.request_line = ngx_str(&mut line);
    main.request_line = ngx_str(&mut line);
    let child_ptr = (&mut *child) as *mut ngx_http_request_t;

    main.stream = std::ptr::dangling_mut::<ngx_http_v2_stream_t>();
    let err = eval_in_request(child_ptr, "use ngx;\nngx::req::raw_header(null);\n")
        .expect_err("HTTP/2 on MAIN must reject");
    assert!(
        err.to_ascii_lowercase().contains("http/2") || err.to_ascii_lowercase().contains("http2"),
        "got {err}"
    );

    main.stream = std::ptr::null_mut();
    main.http_version = NGX_HTTP_VERSION_30 as _;
    child.http_version = NGX_HTTP_VERSION_11 as _;
    let err = eval_in_request(child_ptr, "use ngx;\nngx::req::raw_header(null);\n")
        .expect_err("HTTP/3 on MAIN must reject");
    assert!(
        err.to_ascii_lowercase().contains("http/3") || err.to_ascii_lowercase().contains("http3"),
        "got {err}"
    );

    main.http_version = NGX_HTTP_VERSION_11 as _;
    child.stream = std::ptr::dangling_mut::<ngx_http_v2_stream_t>();
    child.http_version = NGX_HTTP_VERSION_20 as _;
    let mut buf = b"GET /t HTTP/1.1\r\nHost: localhost\r\n\r\n".to_vec();
    let _live = install_single_header_buffer(&mut main, &mut buf, b"GET /t HTTP/1.1".len());
    child.connection = main.connection;
    child.http_connection = main.http_connection;
    child.header_in = main.header_in;
    child.request_line = main.request_line;
    let raw = expect_string(
        eval_in_request(child_ptr, "use ngx;\nngx::req::raw_header(null);\n")
            .expect("subrequest stream must not override main HTTP/1.x"),
    );
    assert!(raw.contains("Host: localhost"), "{raw:?}");
}

#[test]
fn raw_header_rejects_malformed_ranges_null_cycles_and_caps() {
    let mut req = dummy_request();
    let mut line = b"GET /t HTTP/1.1".to_vec();
    req.request_line = ngx_str(&mut line);
    let ptr = (&mut *req) as *mut ngx_http_request_t;
    let err = eval_in_request(ptr, "use ngx;\nngx::req::raw_header(null);\n")
        .expect_err("missing connection buffers must error");
    assert!(
        err.to_ascii_lowercase().contains("header")
            || err.to_ascii_lowercase().contains("buffer")
            || err.to_ascii_lowercase().contains("connection"),
        "got {err}"
    );

    let mut storage = b"GET /t HTTP/1.1\r\nHost: x\r\n\r\n".to_vec();
    let mut live = install_single_header_buffer(&mut req, &mut storage, b"GET /t HTTP/1.1".len());
    live._bufs[0].pos = live._bufs[0].start; // empty / inverted range
    let err = eval_in_request(ptr, "use ngx;\nngx::req::raw_header(null);\n")
        .expect_err("inverted buffer range must error");
    assert!(!err.is_empty(), "got {err}");

    let mut chunks = [
        b"GET /t HTTP/1.1\r\nHost\0 a\0\n".to_vec(),
        b"X-Foo\0 b\0\n\r\n".to_vec(),
    ];
    let mut live = install_busy_header_buffers(&mut req, &mut chunks, b"GET /t HTTP/1.1".len());
    live._chains[0].next = live._chains[1].as_mut() as *mut ngx_chain_t;
    live._chains[1].next = live._chains[0].as_mut() as *mut ngx_chain_t;
    let err = eval_in_request(ptr, "use ngx;\nngx::req::raw_header(null);\n")
        .expect_err("busy-chain cycle must error");
    assert!(
        err.to_ascii_lowercase().contains("cycle")
            || err.to_ascii_lowercase().contains("buffer")
            || err.to_ascii_lowercase().contains("header")
            || err.to_ascii_lowercase().contains("malformed"),
        "got {err}"
    );

    let mut huge = vec![b'A'; 70_000];
    huge.splice(0..0, b"GET /t HTTP/1.1\r\nX: ".iter().copied());
    huge.extend_from_slice(b"\r\n\r\n");
    let line_len = b"GET /t HTTP/1.1".len();
    let _live = install_single_header_buffer(&mut req, &mut huge, line_len);
    let err = eval_in_request(ptr, "use ngx;\nngx::req::raw_header(null);\n")
        .expect_err("over-cap raw header must error");
    assert!(
        err.to_ascii_lowercase().contains("limit")
            || err.to_ascii_lowercase().contains("cap")
            || err.contains("65536")
            || err.to_ascii_lowercase().contains("byte"),
        "got {err}"
    );
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
fn get_post_args_parses_already_read_single_and_multi_chain_bodies() {
    let mut req = dummy_request();
    let mut single = b"a=1&b=2&a=3&flag&empty=".to_vec();
    let _live = install_in_memory_body(&mut req, std::slice::from_mut(&mut single));
    let ptr = (&mut *req) as *mut ngx_http_request_t;
    let decoded = eval_in_request(ptr, "use ngx;\nngx::req::get_post_args(null);\n").expect("post");
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
        other => panic!("repeated post keys must become an array, got {other:?}"),
    }

    let mut chunks = [b"a=1&b=".to_vec(), b"2&flag".to_vec()];
    let _live = install_in_memory_body(&mut req, &mut chunks);
    let decoded =
        eval_in_request(ptr, "use ngx;\nngx::req::get_post_args(null);\n").expect("multi");
    let parsed = expect_map(map_get(expect_map(&decoded), "args"));
    assert_eq!(map_get(parsed, "a"), &Value::string("1"));
    assert_eq!(map_get(parsed, "b"), &Value::string("2"));
    assert_eq!(map_get(parsed, "flag"), &Value::Bool(true));

    let capped = eval_in_request(ptr, "use ngx;\nngx::req::get_post_args(1);\n").expect("cap");
    assert_eq!(
        map_get(expect_map(&capped), "truncated"),
        &Value::Bool(true)
    );
}

#[test]
fn get_post_args_file_backed_still_reading_and_corrupt_are_host_errors() {
    let mut req = dummy_request();
    let mut body = b"a=1".to_vec();
    let mut live = install_in_memory_body(&mut req, std::slice::from_mut(&mut body));
    live.body.temp_file = std::ptr::dangling_mut::<ngx_temp_file_t>();
    let ptr = (&mut *req) as *mut ngx_http_request_t;
    let err = eval_in_request(ptr, "use ngx;\nngx::req::get_post_args(null);\n")
        .expect_err("file-backed body must defer");
    assert!(
        err.to_ascii_lowercase().contains("file")
            || err.contains("Task 10")
            || err.contains("task 10"),
        "got {err}"
    );

    live.body.temp_file = std::ptr::null_mut();
    req.set_reading_body(1);
    let err = eval_in_request(ptr, "use ngx;\nngx::req::get_post_args(null);\n")
        .expect_err("still-reading body must error");
    assert!(
        err.to_ascii_lowercase().contains("read") || err.to_ascii_lowercase().contains("body"),
        "got {err}"
    );

    req.set_reading_body(0);
    live._chains[0].buf = std::ptr::null_mut();
    let err = eval_in_request(ptr, "use ngx;\nngx::req::get_post_args(null);\n")
        .expect_err("corrupt body chain must error");
    assert!(
        err.to_ascii_lowercase().contains("body")
            || err.to_ascii_lowercase().contains("buffer")
            || err.to_ascii_lowercase().contains("malformed")
            || err.to_ascii_lowercase().contains("invalid"),
        "got {err}"
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
    write_index(&prefix, "idx/index.html", "idx-ok\n");
    write_index(
        &prefix,
        "ssi.html",
        "<!--# include virtual=\"/ssi-sub\" -->\n",
    );
    write_index(&prefix, "ssi-sub", "sub-ok\n");
    write_index(&prefix, "mirror-parent.html", "parent-ok\n");
    write_index(&prefix, "mirror-sub", "mirror-ok\n");
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
if ngx::req::is_internal() {
    ngx::log(ngx::log_level::notice(), "REQ_INTERNAL_REDIR=true");
} else {
    ngx::log(ngx::log_level::notice(), "REQ_INTERNAL_REDIR=false");
}
ngx::log(ngx::log_level::notice(), "REQ_INTERNAL_URI=" + ngx::encode_args({ uri: ngx::var::get("uri") }));
"#,
    );
    let sub_script = prefix.join("conf/sub_read.rss");
    write_script(
        &sub_script,
        r#"
use ngx;
if ngx::is_subrequest() {
    ngx::log(ngx::log_level::notice(), "REQ_SUB_EVIDENCE=true");
} else {
    ngx::log(ngx::log_level::notice(), "REQ_SUB_EVIDENCE=false");
}
if ngx::req::is_internal() {
    ngx::log(ngx::log_level::notice(), "REQ_SUB_INTERNAL=true");
} else {
    ngx::log(ngx::log_level::notice(), "REQ_SUB_INTERNAL=false");
}
"#,
    );
    let conf = request_conf(
        &nginx_quote_path(&module).unwrap(),
        &prefix,
        port,
        &format!(
            r#"
        set $rss_empty "";
        client_header_buffer_size 512;
        large_client_header_buffers 4 4k;
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
        location /idx/ {{
            index index.html;
            access_by_rss_file {internal};
        }}
        location = /ssi.html {{
            ssi on;
            default_type text/html;
        }}
        location = /ssi-sub {{
            internal;
            access_by_rss_file {sub};
        }}
        location = /mirror-parent.html {{
            mirror /mirror-sub;
            mirror_request_body off;
        }}
        location = /mirror-sub {{
            internal;
            access_by_rss_file {sub};
        }}
"#,
            script = nginx_quote_path(&script).unwrap(),
            post_script = nginx_quote_path(&post_script).unwrap(),
            internal = nginx_quote_path(&internal_script).unwrap(),
            sub = nginx_quote_path(&sub_script).unwrap(),
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

    let (status, response) = http_exchange(
        port,
        "GET /idx/ HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
    );
    assert_eq!(status, 200, "index internal redirect: {response}");

    let (status, response) = http_exchange(
        port,
        "GET /ssi.html HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
    );
    assert_eq!(status, 200, "ssi parent: {response}");
    assert!(
        response.contains("sub-ok"),
        "SSI include must run the subrequest body; got:\n{response}"
    );

    let (status, response) = http_exchange(
        port,
        "GET /mirror-parent.html HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
    );
    assert_eq!(status, 200, "mirror parent: {response}");

    let extra_headers = (1..=40)
        .map(|i| format!("X-Large-{i}: value-{i}\r\n"))
        .collect::<String>();
    let (status, response) = http_exchange(
        port,
        &format!(
            "GET /read/ HTTP/1.1\r\nHost: 127.0.0.1\r\n{extra_headers}Connection: close\r\n\r\n"
        ),
    );
    assert_eq!(status, 200, "large headers: {response}");

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
        log.contains("REQ_INTERNAL_REDIR=true"),
        "real internal redirect/index must observe is_internal=true; got:\n{log}"
    );
    assert!(
        log.contains("internal-read") || log.contains("/idx/"),
        "internal evidence URI must appear; got:\n{log}"
    );
    assert!(
        log.contains("REQ_SUB=false"),
        "normal requests are not subrequests; got:\n{log}"
    );
    // ngx_http_core_access_phase skips every handler when r != r->main, so
    // access_by_rss_file cannot observe is_subrequest=true. SSI still returns
    // sub-ok as evidence the subrequest ran. Stub tests cover true.
    assert!(
        !log.contains("REQ_SUB_EVIDENCE=true"),
        "access must not run on SSI/mirror subrequests (NGINX skips access); got:\n{log}"
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
    assert!(
        log.contains("Host: 127.0.0.1") || log.contains("Host:127.0.0.1"),
        "raw_header must include Host with a colon, not only the request line; got:\n{log}"
    );
    assert!(
        log.contains("X-Large-40") || log.contains("x-large-40") || log.contains("X-Large-1"),
        "raw_header/get_headers must survive multi-buffer large headers; got:\n{log}"
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
