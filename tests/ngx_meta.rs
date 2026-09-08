//! Pure VM tests for Task 4 metadata, constants, phase, and worker-only logging.
//!
//! Real NGINX request-log, worker pid/id/count, prefix, and access-phase
//! checks live in `access_nginx.rs`. These tests only use safe stubs.

use ngx_rustscript_nginx_test_stubs as _;

use std::sync::Arc;

use ngx_rustscript::config::compile_rss_source;
use ngx_rustscript::host::{
    AllowedOps, HostSyncKind, NgxPhase, bind_ngx_hosts, ngx_host_catalog, ngx_host_manifest,
    production_runtime_host_names, with_vm_request_scope,
};
use ngx_rustscript::notify::RequestIdentity;
use vm::{HostTypeSchema, Program, Value, Vm, VmStatus};

fn compile(source: &str) -> Arc<Program> {
    compile_rss_source(source).unwrap_or_else(|err| panic!("compile failed: {err}"))
}

fn eval(source: &str) -> Value {
    let program = compile(source);
    let mut vm = Vm::try_new((*program).clone()).expect("construct vm");
    bind_ngx_hosts(&mut vm);
    let status = vm.run().unwrap_or_else(|err| panic!("run failed: {err}"));
    assert_eq!(status, VmStatus::Halted, "task 4 hosts are synchronous");
    vm.stack()
        .last()
        .cloned()
        .unwrap_or_else(|| panic!("expected a stack value for {source}"))
}

fn eval_err(source: &str) -> String {
    let program = compile(source);
    let mut vm = Vm::try_new((*program).clone()).expect("construct vm");
    bind_ngx_hosts(&mut vm);
    match vm.run() {
        Err(err) => err.to_string(),
        Ok(status) => panic!("expected host failure, got {status:?}"),
    }
}

fn expect_int(value: Value) -> i64 {
    match value {
        Value::Int(number) => number,
        other => panic!("expected int, got {other:?}"),
    }
}

fn expect_bool(value: Value) -> bool {
    match value {
        Value::Bool(flag) => flag,
        other => panic!("expected bool, got {other:?}"),
    }
}

fn expect_string(value: Value) -> String {
    match value {
        Value::String(text) => (*text).clone(),
        other => panic!("expected string, got {other:?}"),
    }
}

fn catalog_schema(name: &str) -> HostTypeSchema {
    ngx_host_catalog()
        .function(name)
        .unwrap_or_else(|| panic!("{name} must be catalogued"))
        .return_type
        .clone()
}

fn catalog_params(name: &str) -> Vec<(String, HostTypeSchema)> {
    ngx_host_catalog()
        .function(name)
        .unwrap_or_else(|| panic!("{name} must be catalogued"))
        .params
        .iter()
        .map(|param| (param.name.clone(), param.ty.clone()))
        .collect()
}

fn manifest_named(name: &str) -> &'static ngx_rustscript::host::HostManifestEntry {
    ngx_host_manifest()
        .iter()
        .find(|entry| entry.name == name)
        .unwrap_or_else(|| panic!("{name} must be in the manifest"))
}

#[test]
fn catalog_and_runtime_declare_task4_hosts() {
    let catalog = ngx_host_catalog();
    let runtime: std::collections::HashSet<&str> =
        production_runtime_host_names().iter().copied().collect();
    for name in [
        "ngx::core::ok",
        "ngx::core::error",
        "ngx::core::again",
        "ngx::core::done",
        "ngx::core::declined",
        "ngx::core::null",
        "ngx::http::get",
        "ngx::http::post",
        "ngx::http::status_ok",
        "ngx::http::status_not_found",
        "ngx::http::status_internal_server_error",
        "ngx::log_level::error",
        "ngx::log_level::notice",
        "ngx::get_phase",
        "ngx::config::subsystem",
        "ngx::config::debug",
        "ngx::config::prefix",
        "ngx::config::nginx_version",
        "ngx::config::nginx_configure",
        "ngx::config::ngx_rustscript_version",
        "ngx::worker::exiting",
        "ngx::worker::pid",
        "ngx::worker::pids",
        "ngx::worker::count",
        "ngx::worker::id",
        "ngx::log",
    ] {
        assert!(
            catalog.function(name).is_some(),
            "catalog must declare {name}"
        );
        assert!(runtime.contains(name), "runtime must bind {name}");
    }
    assert!(
        !runtime.contains("print"),
        "production must not bind a global print host"
    );
}

#[test]
fn constant_hosts_are_zero_arg_ints_matching_nginx_sys_values() {
    assert!(
        catalog_params("ngx::core::ok").is_empty(),
        "constants are zero-argument hosts"
    );
    assert_eq!(catalog_schema("ngx::core::ok"), HostTypeSchema::Int);
    assert!(catalog_params("ngx::core::null").is_empty());
    assert_eq!(catalog_schema("ngx::core::null"), HostTypeSchema::Null);
    // rustscript reserves `null`, so `ngx::core::null()` is unparsable.
    // The language literal `null` is the OpenResty ngx.null equivalent.
    assert!(
        compile_rss_source("use ngx;\nngx::core::null();\n").is_err(),
        "ngx::core::null is a reserved-word path in rustscript"
    );

    assert_eq!(expect_int(eval("use ngx;\nngx::core::ok();\n")), 0);
    assert_eq!(expect_int(eval("use ngx;\nngx::core::error();\n")), -1);
    assert_eq!(expect_int(eval("use ngx;\nngx::core::again();\n")), -2);
    assert_eq!(expect_int(eval("use ngx;\nngx::core::done();\n")), -4);
    assert_eq!(expect_int(eval("use ngx;\nngx::core::declined();\n")), -5);

    assert_eq!(expect_int(eval("use ngx;\nngx::http::get();\n")), 0x0002);
    assert_eq!(expect_int(eval("use ngx;\nngx::http::head();\n")), 0x0004);
    assert_eq!(expect_int(eval("use ngx;\nngx::http::post();\n")), 0x0008);
    assert_eq!(expect_int(eval("use ngx;\nngx::http::put();\n")), 0x0010);
    assert_eq!(expect_int(eval("use ngx;\nngx::http::status_ok();\n")), 200);
    assert_eq!(
        expect_int(eval("use ngx;\nngx::http::status_not_found();\n")),
        404
    );
    assert_eq!(
        expect_int(eval(
            "use ngx;\nngx::http::status_internal_server_error();\n"
        )),
        500
    );
    assert_eq!(
        expect_int(eval("use ngx;\nngx::http::status_request_timeout();\n")),
        408
    );
    assert_eq!(
        expect_int(eval("use ngx;\nngx::http::status_gateway_timeout();\n")),
        504
    );

    assert_eq!(expect_int(eval("use ngx;\nngx::log_level::stderr();\n")), 0);
    assert_eq!(expect_int(eval("use ngx;\nngx::log_level::emerg();\n")), 1);
    assert_eq!(expect_int(eval("use ngx;\nngx::log_level::error();\n")), 4);
    assert_eq!(expect_int(eval("use ngx;\nngx::log_level::notice();\n")), 6);
    assert_eq!(expect_int(eval("use ngx;\nngx::log_level::debug();\n")), 8);
}

#[test]
fn constant_manifest_uses_openresty_names_and_sync_metadata() {
    let ok = manifest_named("ngx::core::ok");
    assert_eq!(ok.sync, HostSyncKind::Sync);
    assert_eq!(ok.openresty_name, "ngx.OK");
    assert!(ok.phases.contains(&NgxPhase::Access));
    assert_eq!(format!("{:?}", ok.family), "Constant");

    let get = manifest_named("ngx::http::get");
    assert_eq!(get.openresty_name, "ngx.HTTP_GET");
    assert_eq!(format!("{:?}", get.family), "Constant");

    let status = manifest_named("ngx::http::status_ok");
    assert_eq!(status.openresty_name, "ngx.HTTP_OK");

    let err = manifest_named("ngx::log_level::error");
    assert_eq!(err.openresty_name, "ngx.ERR");
    assert!(err.phases.contains(&NgxPhase::InitWorker));
    assert!(err.phases.contains(&NgxPhase::ExitWorker));
}

#[test]
fn compile_time_config_metadata_is_owned_and_truthful() {
    assert_eq!(
        expect_string(eval("use ngx;\nngx::config::subsystem();\n")),
        "http"
    );
    assert!(!expect_bool(eval("use ngx;\nngx::config::debug();\n")));
    assert_eq!(
        expect_int(eval("use ngx;\nngx::config::nginx_version();\n")),
        1_030_004
    );
    assert_eq!(
        expect_int(eval("use ngx;\nngx::config::ngx_rustscript_version();\n")),
        1_000
    );
    let configure = expect_string(eval("use ngx;\nngx::config::nginx_configure();\n"));
    assert!(
        configure.contains("--with-compat") && configure.contains("--with-http_ssl_module"),
        "nginx_configure must come from NGX_CONFIGURE, got {configure}"
    );

    assert!(catalog_params("ngx::config::prefix").is_empty());
    assert_eq!(
        catalog_schema("ngx::config::prefix"),
        HostTypeSchema::String
    );
    assert_eq!(catalog_schema("ngx::config::debug"), HostTypeSchema::Bool);
}

#[test]
fn worker_stub_pid_id_exiting_and_cycle_dependent_hosts() {
    assert_eq!(expect_int(eval("use ngx;\nngx::worker::pid();\n")), 0);
    assert_eq!(expect_int(eval("use ngx;\nngx::worker::id();\n")), 0);
    assert!(!expect_bool(eval("use ngx;\nngx::worker::exiting();\n")));
    match eval("use ngx;\nngx::worker::pids();\n") {
        Value::Array(values) => assert!(
            values.is_empty(),
            "stub ngx_processes should yield no live pids, got {values:?}"
        ),
        other => panic!("ngx::worker::pids must return an array, got {other:?}"),
    }

    let count_err = eval_err("use ngx;\nngx::worker::count();\n");
    let lower = count_err.to_ascii_lowercase();
    assert!(
        lower.contains("cycle") || lower.contains("worker"),
        "count without a live cycle must fail truthfully, got {count_err}"
    );

    let prefix_err = eval_err("use ngx;\nngx::config::prefix();\n");
    let lower = prefix_err.to_ascii_lowercase();
    assert!(
        lower.contains("cycle") || lower.contains("prefix"),
        "prefix without a live cycle must fail truthfully, got {prefix_err}"
    );
}

#[test]
fn get_phase_reads_execution_scope_and_fails_outside_a_request() {
    let err = eval_err("use ngx;\nngx::get_phase();\n");
    let lower = err.to_ascii_lowercase();
    assert!(
        lower.contains("phase") || lower.contains("scope"),
        "get_phase outside a request must fail, got {err}"
    );

    let identity = RequestIdentity::new(1, 7);
    let phase = with_vm_request_scope(identity, NgxPhase::Access, AllowedOps::LOG, || {
        expect_string(eval("use ngx;\nngx::get_phase();\n"))
    });
    assert_eq!(phase, "access");
}

#[test]
fn worker_only_log_accepts_notice_and_rejects_invalid_level_and_nul() {
    let params = catalog_params("ngx::log");
    assert_eq!(params.len(), 2);
    assert_eq!(params[0].0, "level");
    assert_eq!(params[0].1, HostTypeSchema::Int);
    assert_eq!(params[1].0, "message");
    assert_eq!(params[1].1, HostTypeSchema::String);
    assert_eq!(catalog_schema("ngx::log"), HostTypeSchema::Null);

    let log_entry = manifest_named("ngx::log");
    assert_eq!(format!("{:?}", log_entry.family), "Log");
    assert_eq!(log_entry.sync, HostSyncKind::Sync);
    assert_eq!(log_entry.openresty_name, "ngx.log");

    assert_eq!(
        eval("use ngx;\nngx::log(ngx::log_level::notice(), \"worker-only\");\n"),
        Value::Null
    );

    let level_err = eval_err("use ngx;\nngx::log(99, \"bad\");\n");
    let lower = level_err.to_ascii_lowercase();
    assert!(
        lower.contains("level") || lower.contains("log"),
        "invalid log level must be rejected, got {level_err}"
    );
    let neg_err = eval_err("use ngx;\nngx::log(-1, \"bad\");\n");
    let lower = neg_err.to_ascii_lowercase();
    assert!(
        lower.contains("level") || lower.contains("log"),
        "negative log level must be rejected, got {neg_err}"
    );

    let nul_err =
        eval_err("use ngx;\nngx::log(ngx::log_level::notice(), ngx::unescape_uri(\"a%00b\"));\n");
    let lower = nul_err.to_ascii_lowercase();
    assert!(
        lower.contains("nul") || lower.contains("null"),
        "embedded NUL must be rejected before FFI, got {nul_err}"
    );
}
