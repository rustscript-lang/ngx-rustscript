//! Pure VM tests for Task 4 metadata, constants, phase, and worker-only logging.
//!
//! Real NGINX request-log, worker pid/id/count, prefix, and access-phase
//! checks live in `access_nginx.rs`. These tests only use safe stubs.

use ngx_rustscript_nginx_test_stubs as nginx_stubs;

use std::collections::BTreeSet;
use std::sync::Arc;

use nginx_sys::{NGX_LOG_DEBUG, ngx_cycle_t, ngx_log_t};
use ngx_rustscript::config::compile_rss_source;
use ngx_rustscript::host::{
    AllowedOps, HostSyncKind, NgxPhase, bind_ngx_hosts, host_allowed_in_phase, ngx_host_catalog,
    ngx_host_manifest, production_runtime_host_names, with_vm_request_scope,
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

fn configure_has_exact_arg(configure: &str, expected: &str) -> bool {
    configure
        .split_ascii_whitespace()
        .any(|arg| arg == expected)
}

fn expect_pids(source: &str) -> Vec<i64> {
    match eval(source) {
        Value::Array(values) => values.iter().cloned().map(expect_int).collect(),
        other => panic!("expected pid array, got {other:?}"),
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

fn reset_process_table() {
    unsafe {
        core::ptr::addr_of_mut!(nginx_sys::ngx_pid).write(0);
        core::ptr::addr_of_mut!(nginx_sys::ngx_process_slot).write(0);
        let processes = &mut *core::ptr::addr_of_mut!(nginx_sys::ngx_processes);
        core::ptr::write_bytes(processes.as_mut_ptr(), 0, processes.len());
    }
}

fn process_table_len() -> isize {
    unsafe { (*core::ptr::addr_of!(nginx_sys::ngx_processes)).len() as isize }
}

struct ProcessTableGuard;

impl ProcessTableGuard {
    fn install(current_pid: i32, slot: isize, slots: &[(usize, i32, i32)]) -> Self {
        reset_process_table();
        unsafe {
            core::ptr::addr_of_mut!(nginx_sys::ngx_pid).write(current_pid);
            core::ptr::addr_of_mut!(nginx_sys::ngx_process_slot).write(slot);
            let processes = &mut *core::ptr::addr_of_mut!(nginx_sys::ngx_processes);
            for &(index, pid, channel0) in slots {
                processes[index].pid = pid;
                processes[index].channel[0] = channel0;
            }
        }
        Self
    }
}

impl Drop for ProcessTableGuard {
    fn drop(&mut self) {
        reset_process_table();
    }
}

struct CycleLogGuard {
    _cycle: Box<ngx_cycle_t>,
    _log: Box<ngx_log_t>,
    prev: *mut ngx_cycle_t,
}

impl CycleLogGuard {
    fn install() -> Self {
        nginx_stubs::stub_log_reset();
        let mut log = Box::new(unsafe { std::mem::zeroed::<ngx_log_t>() });
        log.log_level = NGX_LOG_DEBUG as _;
        let mut cycle = Box::new(unsafe { std::mem::zeroed::<ngx_cycle_t>() });
        cycle.log = &mut *log;
        let prev = unsafe { core::ptr::addr_of!(nginx_sys::ngx_cycle).read() };
        unsafe {
            core::ptr::addr_of_mut!(nginx_sys::ngx_cycle).write(&mut *cycle);
        }
        Self {
            _cycle: cycle,
            _log: log,
            prev,
        }
    }
}

impl Drop for CycleLogGuard {
    fn drop(&mut self) {
        unsafe {
            core::ptr::addr_of_mut!(nginx_sys::ngx_cycle).write(self.prev);
        }
        nginx_stubs::stub_log_reset();
    }
}

#[test]
fn catalog_runtime_and_manifest_agree_on_task4_hosts() {
    let catalog_api = ngx_host_catalog();
    let catalog: BTreeSet<&str> = catalog_api
        .functions()
        .iter()
        .map(|schema| schema.name.as_str())
        .collect();
    let runtime: BTreeSet<&str> = production_runtime_host_names().iter().copied().collect();
    let manifest: BTreeSet<&str> = ngx_host_manifest().iter().map(|entry| entry.name).collect();
    assert_eq!(
        catalog, manifest,
        "compiler catalog and manifest must agree"
    );
    assert_eq!(
        runtime, manifest,
        "descriptor registry names must match the catalog/manifest"
    );
    let timer_family: BTreeSet<&str> = manifest
        .iter()
        .copied()
        .filter(|name| name.starts_with("ngx::timer::"))
        .collect();
    assert_eq!(timer_family.len(), 4, "the timer family has four hosts");
    for name in [
        "ngx::get_phase",
        "ngx::config::prefix",
        "ngx::config::debug",
        "ngx::config::nginx_version",
        "ngx::config::nginx_configure",
        "ngx::config::ngx_rustscript_version",
        "ngx::config::subsystem",
        "ngx::worker::exiting",
        "ngx::worker::pid",
        "ngx::worker::pids",
        "ngx::worker::count",
        "ngx::worker::id",
        "ngx::core::ok",
        "ngx::core::error",
        "ngx::core::again",
        "ngx::core::done",
        "ngx::core::declined",
        "ngx::core::null_value",
        "ngx::http::get",
        "ngx::http::status_ok",
        "ngx::http::status_payment_required",
        "ngx::http::status_not_acceptable",
        "ngx::http::status_gone",
        "ngx::http::status_upgrade_required",
        "ngx::http::status_illegal",
        "ngx::log",
        "ngx::log_level::error",
        "ngx::log_level::debug",
    ] {
        assert!(catalog.contains(name), "{name} missing from catalog");
        assert!(runtime.contains(name), "{name} missing from runtime");
    }
    assert!(
        !catalog.contains("ngx::core::null") && !runtime.contains("ngx::core::null"),
        "ngx::core::null is a reserved token and must not be catalogued"
    );
}

#[test]
fn task4_hosts_are_synchronous_in_the_manifest() {
    for entry in ngx_host_manifest() {
        if entry.name.starts_with("ngx::config::")
            || entry.name.starts_with("ngx::worker::")
            || entry.name.starts_with("ngx::core::")
            || entry.name.starts_with("ngx::http::")
            || entry.name.starts_with("ngx::log_level::")
            || entry.name == "ngx::get_phase"
            || entry.name == "ngx::log"
        {
            assert_eq!(
                entry.sync,
                HostSyncKind::Sync,
                "{} must be synchronous",
                entry.name
            );
        }
    }
}

#[test]
fn constant_hosts_are_zero_arg_ints_matching_nginx_sys_values() {
    assert!(
        catalog_params("ngx::core::ok").is_empty(),
        "constants are zero-argument hosts"
    );
    assert_eq!(catalog_schema("ngx::core::ok"), HostTypeSchema::Int);
    assert!(catalog_params("ngx::core::null_value").is_empty());
    assert_eq!(
        catalog_schema("ngx::core::null_value"),
        HostTypeSchema::Null
    );
    assert!(
        compile_rss_source("use ngx;\nngx::core::null();\n").is_err(),
        "ngx::core::null is a reserved-word path in rustscript"
    );

    assert_eq!(expect_int(eval("use ngx;\nngx::core::ok();\n")), 0);
    assert_eq!(expect_int(eval("use ngx;\nngx::core::error();\n")), -1);
    assert_eq!(expect_int(eval("use ngx;\nngx::core::again();\n")), -2);
    assert_eq!(expect_int(eval("use ngx;\nngx::core::done();\n")), -4);
    assert_eq!(expect_int(eval("use ngx;\nngx::core::declined();\n")), -5);
    assert_eq!(eval("use ngx;\nngx::core::null_value();\n"), Value::Null);

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
    assert_eq!(
        expect_int(eval("use ngx;\nngx::http::status_payment_required();\n")),
        402
    );
    assert_eq!(
        expect_int(eval("use ngx;\nngx::http::status_not_acceptable();\n")),
        406
    );
    assert_eq!(
        expect_int(eval("use ngx;\nngx::http::status_gone();\n")),
        410
    );
    assert_eq!(
        expect_int(eval("use ngx;\nngx::http::status_upgrade_required();\n")),
        426
    );
    assert_eq!(
        expect_int(eval("use ngx;\nngx::http::status_illegal();\n")),
        451
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

    let null_value = manifest_named("ngx::core::null_value");
    assert_eq!(null_value.openresty_name, "ngx.null");
    assert_eq!(format!("{:?}", null_value.family), "Constant");

    let get = manifest_named("ngx::http::get");
    assert_eq!(get.openresty_name, "ngx.HTTP_GET");
    assert_eq!(format!("{:?}", get.family), "Constant");

    let status = manifest_named("ngx::http::status_ok");
    assert_eq!(status.openresty_name, "ngx.HTTP_OK");
    assert_eq!(
        manifest_named("ngx::http::status_payment_required").openresty_name,
        "ngx.HTTP_PAYMENT_REQUIRED"
    );
    assert_eq!(
        manifest_named("ngx::http::status_not_acceptable").openresty_name,
        "ngx.HTTP_NOT_ACCEPTABLE"
    );
    assert_eq!(
        manifest_named("ngx::http::status_gone").openresty_name,
        "ngx.HTTP_GONE"
    );
    assert_eq!(
        manifest_named("ngx::http::status_upgrade_required").openresty_name,
        "ngx.HTTP_UPGRADE_REQUIRED"
    );
    assert_eq!(
        manifest_named("ngx::http::status_illegal").openresty_name,
        "ngx.HTTP_ILLEGAL"
    );

    let err = manifest_named("ngx::log_level::error");
    assert_eq!(err.openresty_name, "ngx.ERR");
    assert!(err.phases.contains(&NgxPhase::InitWorker));
    assert!(err.phases.contains(&NgxPhase::ExitWorker));
}

#[test]
fn configure_argument_matching_requires_an_exact_token() {
    assert!(configure_has_exact_arg(
        "--with-compat --with-debug --with-http_ssl_module",
        "--with-debug"
    ));
    assert!(!configure_has_exact_arg(
        "--with-compat --with-debug-log --with-http_ssl_module",
        "--with-debug"
    ));
}

#[test]
fn compile_time_config_metadata_is_owned_and_truthful() {
    assert_eq!(
        expect_string(eval("use ngx;\nngx::config::subsystem();\n")),
        "http"
    );
    let debug = expect_bool(eval("use ngx;\nngx::config::debug();\n"));
    assert_eq!(
        expect_int(eval("use ngx;\nngx::config::nginx_version();\n")),
        1_031_004
    );
    assert_eq!(
        expect_int(eval("use ngx;\nngx::config::ngx_rustscript_version();\n")),
        1_000
    );
    let configure = expect_string(eval("use ngx;\nngx::config::nginx_configure();\n"));
    assert_eq!(
        debug,
        configure_has_exact_arg(&configure, "--with-debug"),
        "debug metadata must match the owned NGX_CONFIGURE value: {configure}"
    );
    assert!(
        configure_has_exact_arg(&configure, "--with-http_ssl_module"),
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
fn nginx_version_phases_include_init_worker_and_exit_worker() {
    assert!(host_allowed_in_phase(
        "ngx::config::nginx_version",
        NgxPhase::InitWorker
    ));
    assert!(host_allowed_in_phase(
        "ngx::config::nginx_version",
        NgxPhase::ExitWorker
    ));
    assert!(host_allowed_in_phase(
        "ngx::config::nginx_version",
        NgxPhase::Init
    ));
    assert!(!host_allowed_in_phase(
        "ngx::config::nginx_configure",
        NgxPhase::InitWorker
    ));
    assert!(!host_allowed_in_phase(
        "ngx::config::nginx_configure",
        NgxPhase::ExitWorker
    ));
    assert!(host_allowed_in_phase(
        "ngx::config::nginx_configure",
        NgxPhase::Init
    ));
    assert!(!host_allowed_in_phase(
        "ngx::config::ngx_rustscript_version",
        NgxPhase::InitWorker
    ));
    assert!(!host_allowed_in_phase(
        "ngx::config::ngx_rustscript_version",
        NgxPhase::ExitWorker
    ));
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
fn worker_pids_mixed_slots_keep_current_and_active_workers_only() {
    let _table = ProcessTableGuard::install(
        100,
        1,
        &[
            (0, 1, 0),    // master: pid>0, no channel
            (1, 100, 7),  // current worker
            (2, 200, 8),  // active worker
            (3, 300, 0),  // cache manager
            (4, 0, 9),    // stale empty pid
            (5, 400, -1), // helper / closed channel
            (6, 200, 10), // duplicate pid of the other worker
        ],
    );
    let mut pids = expect_pids("use ngx;\nngx::worker::pids();\n");
    pids.sort_unstable();
    pids.dedup();
    assert_eq!(
        pids,
        vec![100, 200],
        "pids must include current ngx_pid and active workers only"
    );
}

#[test]
fn worker_pids_includes_current_pid_when_channels_are_absent() {
    let _table = ProcessTableGuard::install(50, 0, &[(0, 50, 0)]);
    let pids = expect_pids("use ngx;\nngx::worker::pids();\n");
    assert_eq!(pids, vec![50]);
}

#[test]
fn worker_pids_invalid_slot_still_includes_live_current_pid() {
    let _table = ProcessTableGuard::install(77, -1, &[(2, 88, 3)]);
    let mut pids = expect_pids("use ngx;\nngx::worker::pids();\n");
    pids.sort_unstable();
    assert_eq!(pids, vec![77, 88]);

    let _table = ProcessTableGuard::install(91, process_table_len(), &[(0, 91, -1)]);
    assert_eq!(
        expect_pids("use ngx;\nngx::worker::pids();\n"),
        vec![91],
        "out-of-range process slot must still yield the live ngx_pid"
    );
}

#[test]
fn worker_pids_does_not_duplicate_the_current_slot() {
    let _table = ProcessTableGuard::install(100, 1, &[(1, 100, 5)]);
    let pids = expect_pids("use ngx;\nngx::worker::pids();\n");
    assert_eq!(pids, vec![100]);
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
fn log_and_constant_schemas_are_exact() {
    let catalog = ngx_host_catalog();
    let log = catalog
        .function("ngx::log")
        .expect("ngx::log is catalogued");
    assert_eq!(log.params.len(), 2);
    assert_eq!(log.params[0].name, "level");
    assert_eq!(log.params[0].ty, HostTypeSchema::Int);
    assert_eq!(log.params[1].name, "message");
    assert_eq!(log.params[1].ty, HostTypeSchema::String);
    assert_eq!(log.return_type, HostTypeSchema::Null);

    assert!(catalog_params("ngx::get_phase").is_empty());
    assert_eq!(catalog_schema("ngx::get_phase"), HostTypeSchema::String);
    assert!(catalog_params("ngx::worker::pids").is_empty());
    assert_eq!(
        catalog_schema("ngx::worker::pids"),
        HostTypeSchema::Array(Box::new(HostTypeSchema::Int))
    );
    assert_eq!(
        catalog_schema("ngx::core::null_value"),
        HostTypeSchema::Null
    );
    assert_eq!(
        catalog_schema("ngx::http::status_payment_required"),
        HostTypeSchema::Int
    );
    assert_eq!(catalog_schema("ngx::log_level::debug"), HostTypeSchema::Int);
}

#[test]
fn log_without_sink_returns_host_error() {
    nginx_stubs::stub_log_reset();
    let err = eval_err("use ngx;\nngx::log(ngx::log_level::notice(), \"no-sink\");\n");
    let lower = err.to_ascii_lowercase();
    assert!(
        lower.contains("ngx::log")
            && (lower.contains("cycle") || lower.contains("request") || lower.contains("sink")),
        "expected structured missing-sink host error, got {err}"
    );
    assert_eq!(
        nginx_stubs::stub_log_count(),
        0,
        "no sink must not call ngx_log_error_core"
    );
}

#[test]
fn log_with_cycle_sink_writes_prefixed_message() {
    let _cycle = CycleLogGuard::install();
    assert_eq!(
        eval("use ngx;\nngx::log(ngx::log_level::notice(), \"worker-only\");\n"),
        Value::Null
    );
    assert!(
        nginx_stubs::stub_log_count() >= 1,
        "cycle log path must invoke ngx_log_error_core"
    );
    let msg = nginx_stubs::stub_last_log();
    assert!(
        msg.contains("ngx-rustscript: worker-only"),
        "cycle log must write the prefixed message without request scope, got {msg:?}"
    );
}

#[test]
fn worker_only_log_rejects_invalid_level_and_embedded_nul() {
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
