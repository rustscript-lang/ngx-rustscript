//! Request-local `ngx.ctx` get/set/delete/clear, budgets, scope, and lifecycle.

use ngx_rustscript_nginx_test_stubs as _;

use std::os::unix::io::RawFd;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ngx_rustscript::config::compile_rss_source;
use ngx_rustscript::host::{
    AllowedOps, HostCapability, HostFamily, HostSyncKind, MAX_CTX_KEY_BYTES, NGX_CTX_CLEAR_NAME,
    NGX_CTX_DELETE_NAME, NGX_CTX_GET_NAME, NGX_CTX_SET_NAME, NgxPhase, RequestLocalCtx, ScopeError,
    bind_ngx_hosts, capture_vm_entries, current_identity, host_allowed_in_phase,
    live_owned_entry_count, manifest_entry, ngx_host_catalog, with_current_ctx, with_request_scope,
    with_request_scope_ctx,
};
use ngx_rustscript::notify::{DEFAULT_MAX_IN_FLIGHT, RequestIdentity};
use ngx_rustscript::request_state::{
    AccessStatus, IdentityRegistry, ReadyEffect, RecordingSink, apply_ready_effect,
};
use ngx_rustscript::runtime::WorkerRuntime;
use vm::{HostTypeSchema, Program, Value, Vm, VmStatus};

fn compile(source: &str) -> Arc<Program> {
    compile_rss_source(source).unwrap_or_else(|err| panic!("compile failed: {err}\n{source}"))
}

fn run_vm(source: &str, ctx: &mut RequestLocalCtx, ops: AllowedOps) -> Result<Value, String> {
    let program = compile(source);
    let mut vm = Vm::try_new((*program).clone()).expect("construct vm");
    bind_ngx_hosts(&mut vm);
    let identity = RequestIdentity::new(1, 1);
    with_request_scope_ctx(
        std::ptr::null_mut(),
        ctx,
        identity,
        NgxPhase::Access,
        ops,
        || match vm.run() {
            Ok(VmStatus::Halted) => Ok(vm
                .stack()
                .last()
                .cloned()
                .unwrap_or_else(|| panic!("expected a stack value for {source}"))),
            Ok(status) => Err(format!("unexpected status {status:?}")),
            Err(err) => Err(err.to_string()),
        },
    )
}

fn eval_ctx(source: &str) -> Value {
    let mut ctx = RequestLocalCtx::new();
    run_vm(source, &mut ctx, AllowedOps::for_phase(NgxPhase::Access))
        .unwrap_or_else(|err| panic!("eval failed: {err}\n{source}"))
}

fn eval_ctx_err(source: &str) -> String {
    let mut ctx = RequestLocalCtx::new();
    run_vm(source, &mut ctx, AllowedOps::for_phase(NgxPhase::Access))
        .expect_err("expected ctx host error")
}

fn expect_string(value: Value) -> String {
    match value {
        Value::String(text) => (*text).clone(),
        other => panic!("expected string, got {other:?}"),
    }
}

fn expect_int(value: Value) -> i64 {
    match value {
        Value::Int(number) => number,
        other => panic!("expected int, got {other:?}"),
    }
}

fn wait_readable(fd: RawFd, timeout: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let n = unsafe { libc::poll(&mut pfd, 1, 20) };
        if n > 0 && pfd.revents & libc::POLLIN != 0 {
            return true;
        }
    }
    false
}

struct Driver {
    worker: WorkerRuntime,
    exec: ngx_rustscript::request_state::AccessExec,
    sink: RecordingSink,
    registry: IdentityRegistry<u64>,
}

impl Driver {
    fn new(program: Arc<Program>, request_id: u64) -> Self {
        let mut worker = WorkerRuntime::start().unwrap();
        let exec = worker.bind_access_exec(program, request_id).expect("bind");
        Self {
            worker,
            exec,
            sink: RecordingSink::default(),
            registry: IdentityRegistry::with_capacity(DEFAULT_MAX_IN_FLIGHT),
        }
    }

    fn handle(&mut self) -> AccessStatus {
        let status = self.exec.handle_access(&mut self.registry, &self.worker);
        if status == AccessStatus::Again {
            self.exec.commit_suspend();
        }
        status
    }

    fn wait_ready(&mut self) -> ReadyEffect {
        let fd = self.worker.raw_fd().expect("eventfd");
        assert!(wait_readable(fd, Duration::from_secs(2)));
        let _ = self.worker.drain_ready().unwrap();
        let effect = self
            .exec
            .apply_ready(self.exec.identity(), &mut self.registry, &self.worker);
        apply_ready_effect(effect, &mut self.sink);
        effect
    }

    fn cleanup(&mut self) -> Option<RequestIdentity> {
        let request_id = self.exec.request_id();
        let identity = self.exec.identity();
        let _ = self.registry.remove_exact(identity, &request_id);
        self.exec.cleanup_wait(&self.worker)
    }
}

#[test]
fn ctx_hosts_are_catalogued_sync_request_ctx() {
    for name in [
        NGX_CTX_GET_NAME,
        NGX_CTX_SET_NAME,
        NGX_CTX_DELETE_NAME,
        NGX_CTX_CLEAR_NAME,
    ] {
        let entry = manifest_entry(name).unwrap_or_else(|| panic!("missing {name}"));
        assert_eq!(entry.family, HostFamily::RequestCtx);
        assert_eq!(entry.sync, HostSyncKind::Sync);
        assert_eq!(entry.capabilities, &[HostCapability::RequestCtx]);
        assert_eq!(entry.openresty_name, "ngx.ctx");
        assert!(!host_allowed_in_phase(name, NgxPhase::Init));
        assert!(!host_allowed_in_phase(name, NgxPhase::InitWorker));
        assert!(!host_allowed_in_phase(name, NgxPhase::ExitWorker));
        assert!(host_allowed_in_phase(name, NgxPhase::Access));
        assert!(host_allowed_in_phase(name, NgxPhase::Rewrite));
        assert!(host_allowed_in_phase(name, NgxPhase::Content));
        assert!(host_allowed_in_phase(name, NgxPhase::Log));
        assert!(host_allowed_in_phase(name, NgxPhase::Timer));
        match name {
            NGX_CTX_GET_NAME | NGX_CTX_DELETE_NAME => {
                compile(&format!("use ngx;\n{name}(\"k\");\n"));
            }
            NGX_CTX_SET_NAME => {
                compile("use ngx;\nngx::ctx::set(\"k\", 1);\n");
            }
            NGX_CTX_CLEAR_NAME => {
                compile("use ngx;\nngx::ctx::clear();\n");
            }
            _ => unreachable!(),
        }
    }
    let catalog = ngx_host_catalog();
    let schema = |name: &str| {
        catalog
            .functions()
            .iter()
            .find(|schema| schema.name == name)
            .unwrap_or_else(|| panic!("missing schema {name}"))
    };
    assert_eq!(
        schema(NGX_CTX_GET_NAME).return_type,
        HostTypeSchema::Unknown
    );
    let set = schema(NGX_CTX_SET_NAME);
    assert_eq!(set.return_type, HostTypeSchema::Null);
    assert_eq!(set.params[1].ty, HostTypeSchema::Unknown);
    assert_eq!(
        schema(NGX_CTX_DELETE_NAME).return_type,
        HostTypeSchema::Unknown
    );
    assert_eq!(schema(NGX_CTX_CLEAR_NAME).return_type, HostTypeSchema::Null);
}

#[test]
fn get_set_replace_delete_missing_clear_scalars_bytes_nested() {
    assert_eq!(
        eval_ctx("use ngx;\nngx::ctx::get(\"missing\");\n"),
        Value::Null
    );
    assert_eq!(
        eval_ctx("use ngx;\nngx::ctx::set(\"n\", null);\nngx::ctx::get(\"n\");\n"),
        Value::Null
    );
    assert_eq!(
        eval_ctx("use ngx;\nngx::ctx::set(\"b\", true);\nngx::ctx::get(\"b\");\n"),
        Value::Bool(true)
    );
    assert_eq!(
        eval_ctx("use ngx;\nngx::ctx::set(\"b\", false);\nngx::ctx::get(\"b\");\n"),
        Value::Bool(false)
    );
    assert_eq!(
        expect_int(eval_ctx(
            "use ngx;\nngx::ctx::set(\"i\", 42);\nngx::ctx::get(\"i\");\n"
        )),
        42
    );
    match eval_ctx("use ngx;\nngx::ctx::set(\"f\", 1.5);\nngx::ctx::get(\"f\");\n") {
        Value::Float(number) => assert!((number - 1.5).abs() < 1e-9),
        other => panic!("expected float, got {other:?}"),
    }
    assert_eq!(
        expect_string(eval_ctx(
            "use ngx;\nngx::ctx::set(\"s\", \"hi\");\nngx::ctx::get(\"s\");\n"
        )),
        "hi"
    );
    match eval_ctx(
        "use ngx;\nngx::ctx::set(\"bin\", ngx::md5_bin(\"hello\"));\nngx::ctx::get(\"bin\");\n",
    ) {
        Value::Bytes(bytes) => assert_eq!(bytes.len(), 16),
        other => panic!("expected bytes, got {other:?}"),
    }
    assert_eq!(
        expect_string(eval_ctx(
            "use ngx;\nngx::ctx::set(\"k\", \"old\");\nngx::ctx::set(\"k\", \"new\");\nngx::ctx::get(\"k\");\n"
        )),
        "new"
    );
    assert_eq!(
        expect_string(eval_ctx(
            "use ngx;\nngx::ctx::set(\"k\", \"gone\");\nngx::ctx::delete(\"k\");\n"
        )),
        "gone"
    );
    assert_eq!(
        eval_ctx("use ngx;\nngx::ctx::delete(\"missing\");\n"),
        Value::Null
    );
    assert_eq!(
        eval_ctx("use ngx;\nngx::ctx::set(\"a\", 1);\nngx::ctx::clear();\nngx::ctx::get(\"a\");\n"),
        Value::Null
    );
    match eval_ctx(
        "use ngx;\nngx::ctx::set(\"user\", { id: 1, tags: [\"a\", \"b\"], ok: true });\nngx::ctx::get(\"user\");\n",
    ) {
        Value::Map(map) => {
            assert_eq!(map.get(&Value::string("id")), Some(&Value::Int(1)));
            assert_eq!(map.get(&Value::string("ok")), Some(&Value::Bool(true)));
            match map.get(&Value::string("tags")) {
                Some(Value::Array(items)) => {
                    assert_eq!(items.len(), 2);
                }
                other => panic!("expected tags array, got {other:?}"),
            }
        }
        other => panic!("expected nested map, got {other:?}"),
    }
}

#[test]
fn rejects_callable_empty_nul_and_too_long_key() {
    let callable = eval_ctx_err("use ngx;\nfn f() { 1 }\nngx::ctx::set(\"f\", f);\n");
    assert!(callable.contains("callable"), "callable reject: {callable}");
    let empty = eval_ctx_err("use ngx;\nngx::ctx::set(\"\", 1);\n");
    assert!(empty.contains("empty"), "{empty}");
    let nul = eval_ctx_err("use ngx;\nngx::ctx::set(\"a\\0b\", 1);\n");
    assert!(nul.contains("NUL"), "{nul}");
    let long_key = "k".repeat(MAX_CTX_KEY_BYTES + 1);
    let too_long = eval_ctx_err(&format!("use ngx;\nngx::ctx::set(\"{long_key}\", 1);\n"));
    assert!(too_long.contains("key exceeds"), "{too_long}");
}

#[test]
fn no_scope_wrong_capability_stale_generation_nested_and_panic() {
    let err = {
        let program = compile("use ngx;\nngx::ctx::get(\"k\");\n");
        let mut vm = Vm::try_new((*program).clone()).expect("vm");
        bind_ngx_hosts(&mut vm);
        match vm.run() {
            Err(err) => err.to_string(),
            Ok(status) => panic!("expected no-scope error, got {status:?}"),
        }
    };
    assert!(err.contains("outside request execution scope"), "{err}");

    let mut ctx = RequestLocalCtx::new();
    let log_only = AllowedOps::LOG;
    let denied = run_vm("use ngx;\nngx::ctx::set(\"k\", 1);\n", &mut ctx, log_only)
        .expect_err("LOG-only must deny ctx");
    assert!(denied.contains("not allowed"), "{denied}");

    let identity = RequestIdentity::new(7, 7);
    let stale = RequestIdentity::new(8, 7);
    let mut ctx = RequestLocalCtx::new();
    with_request_scope_ctx(
        std::ptr::null_mut(),
        &mut ctx,
        identity,
        NgxPhase::Access,
        AllowedOps::for_phase(NgxPhase::Access),
        || {
            assert_eq!(
                with_current_ctx(stale, |_| ()),
                Err(ScopeError::StaleGeneration)
            );
            let inner = RequestIdentity::new(9, 9);
            with_request_scope(
                std::ptr::null_mut(),
                inner,
                NgxPhase::Access,
                AllowedOps::for_phase(NgxPhase::Access),
                || {
                    assert_eq!(current_identity(), Ok(inner));
                },
            );
            assert_eq!(current_identity(), Ok(identity));
            let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                with_request_scope(
                    std::ptr::null_mut(),
                    RequestIdentity::new(10, 10),
                    NgxPhase::Access,
                    AllowedOps::for_phase(NgxPhase::Access),
                    || panic!("nested ctx scope unwind"),
                );
            }));
            assert!(panicked.is_err());
            assert_eq!(current_identity(), Ok(identity));
            ctx.set("kept", &Value::Int(1)).unwrap();
        },
    );
    assert_eq!(ctx.get("kept"), Value::Int(1));
}

#[test]
fn ctx_accessor_checks_generation_capability_and_avoids_request_pointer() {
    let context = include_str!("../src/host/context.rs");
    assert!(context.contains("REQUEST_CTX"));
    assert!(context.contains("StaleGeneration"));
    assert!(context.contains("fn verified_ctx_frame"));
    let host = include_str!("../src/host/request_ctx.rs");
    assert!(host.contains("with_current_ctx"));
    assert!(host.contains("with_current_ctx_mut"));
    assert!(!host.contains("with_current_request"));
}

#[test]
fn persists_across_two_sleep_resume_cycles() {
    let program = compile(
        r#"
use ngx;
ngx::ctx::set("k", "keep");
ngx::ctx::set("n", 1);
ngx::sleep_ms(10);
ngx::ctx::set("n", 2);
ngx::sleep_ms(10);
if ngx::ctx::get("k") != "keep" {
    1 / 0;
}
if ngx::ctx::get("n") != 2 {
    1 / 0;
}
"#,
    );
    let mut driver = Driver::new(program, 1);
    assert_eq!(driver.handle(), AccessStatus::Again);
    assert_eq!(driver.wait_ready(), ReadyEffect::WaitingAgain);
    assert_eq!(driver.wait_ready(), ReadyEffect::Halted);
}

#[test]
fn concurrent_requests_are_isolated_across_interleaved_wakes() {
    let alpha = compile(
        r#"
use ngx;
ngx::ctx::set("k", "alpha");
ngx::sleep_ms(20);
if ngx::ctx::get("k") != "alpha" {
    1 / 0;
}
"#,
    );
    let beta = compile(
        r#"
use ngx;
ngx::ctx::set("k", "beta");
ngx::sleep_ms(20);
if ngx::ctx::get("k") != "beta" {
    1 / 0;
}
"#,
    );
    let mut a = Driver::new(alpha, 1);
    let mut b = Driver::new(beta, 2);
    assert_eq!(a.handle(), AccessStatus::Again);
    assert_eq!(b.handle(), AccessStatus::Again);
    assert_eq!(a.wait_ready(), ReadyEffect::Halted);
    assert_eq!(b.wait_ready(), ReadyEffect::Halted);
}

#[test]
fn completion_host_error_cancel_stale_wake_and_drop_release_ctx() {
    let baseline = live_owned_entry_count();

    let halt = compile("use ngx;\nngx::ctx::set(\"k\", \"done\");\n");
    let mut driver = Driver::new(halt, 3);
    assert_eq!(driver.handle(), AccessStatus::Ok);
    assert!(live_owned_entry_count() > baseline);
    assert!(driver.cleanup().is_none());
    assert_eq!(live_owned_entry_count(), baseline);

    let boom = compile("use ngx;\nngx::ctx::set(\"k\", \"err\");\n1 / 0;\n");
    {
        let mut driver = Driver::new(boom, 4);
        assert_eq!(driver.handle(), AccessStatus::Error500);
        assert!(live_owned_entry_count() > baseline);
        drop(driver);
    }
    assert_eq!(live_owned_entry_count(), baseline);

    let sleeping = compile("use ngx;\nngx::ctx::set(\"k\", \"wait\");\nngx::sleep_ms(80);\n");
    {
        let mut driver = Driver::new(sleeping.clone(), 5);
        assert_eq!(driver.handle(), AccessStatus::Again);
        assert!(live_owned_entry_count() > baseline);
        let identity = driver.exec.identity();
        assert!(driver.cleanup().is_some());
        assert_eq!(live_owned_entry_count(), baseline);
        let effect = driver
            .exec
            .apply_ready(identity, &mut driver.registry, &driver.worker);
        assert_eq!(effect, ReadyEffect::Ignored);
        assert_eq!(live_owned_entry_count(), baseline);
        drop(driver);
    }
    assert_eq!(live_owned_entry_count(), baseline);

    {
        let mut driver = Driver::new(sleeping, 6);
        assert_eq!(driver.handle(), AccessStatus::Again);
        assert!(live_owned_entry_count() > baseline);
        // Drop while Suspended without cleanup_wait.
        drop(driver);
    }
    assert_eq!(live_owned_entry_count(), baseline);
}

#[test]
fn second_access_exec_starts_with_empty_ctx() {
    let program = compile(
        r#"
use ngx;
if ngx::ctx::get("k") != null {
    1 / 0;
}
ngx::ctx::set("k", 1);
"#,
    );
    let mut first = Driver::new(program.clone(), 7);
    assert_eq!(first.handle(), AccessStatus::Ok);
    // A second handle() on Halted is the same request, not a new one.
    assert_eq!(first.handle(), AccessStatus::Ok);
    let mut second = Driver::new(program, 8);
    assert_eq!(
        second.handle(),
        AccessStatus::Ok,
        "a new AccessExec must start with empty ctx"
    );
}

#[test]
fn ctx_only_production_path_selects_access_exec_and_set_get_works() {
    let program = compile(
        r#"
use ngx;
ngx::ctx::set("k", "sync");
if ngx::ctx::get("k") != "sync" {
    1 / 0;
}
ngx::ctx::delete("k");
if ngx::ctx::get("k") != null {
    1 / 0;
}
"#,
    );
    assert!(
        program.imports.iter().all(|import| {
            ngx_rustscript::host::ngx_host_manifest()
                .iter()
                .find(|entry| entry.name == import.name.as_str())
                .map(|entry| entry.sync != HostSyncKind::Async)
                .unwrap_or(true)
        }),
        "ctx-only fixture must not import async hosts"
    );
    assert!(
        ngx_rustscript::config::program_requires_request_state(&program),
        "config must select AccessExec/request-state for ctx-only scripts"
    );
    let baseline = live_owned_entry_count();
    let mut driver = Driver::new(program, 11);
    assert_eq!(
        driver.handle(),
        AccessStatus::Ok,
        "ctx-only AccessExec path must succeed without NullCtx"
    );
    assert!(driver.cleanup().is_none());
    assert_eq!(live_owned_entry_count(), baseline);
}

#[test]
fn vm_entry_installs_ctx_without_request_pointer() {
    let program = compile("use ngx;\nngx::ctx::set(\"k\", \"ok\");\nngx::sleep_ms(5);\n");
    let mut driver = Driver::new(program, 8);
    let (_, entries) = capture_vm_entries(|| {
        assert_eq!(driver.handle(), AccessStatus::Again);
    });
    assert!(!entries.is_empty());
    assert!(!entries[0].request_live);
    assert_eq!(entries[0].allowed, AllowedOps::for_phase(NgxPhase::Access));
    assert!(entries[0].allowed.contains(AllowedOps::REQUEST_CTX));
    assert!(!entries[0].allowed.contains(AllowedOps::REQUEST_WRITE));
}
