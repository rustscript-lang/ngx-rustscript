//! Discriminating tests for the generic pd-host-function async bridge.
//!
//! Real pd-vm compiles `ngx::sleep` (seconds) and `ngx::sleep_ms` (milliseconds).
//! There is no handwritten Yield host.

use ngx_rustscript_nginx_test_stubs as _;

use std::os::unix::io::RawFd;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ngx_rustscript::config::compile_rss_source;
use ngx_rustscript::host::{NGX_SLEEP_MS_NAME, NGX_SLEEP_NAME, ngx_host_catalog};
use ngx_rustscript::notify::DEFAULT_MAX_IN_FLIGHT;
use ngx_rustscript::request_state::{AccessStatus, IdentityRegistry, ReadyEffect};
use ngx_rustscript::runtime::WorkerRuntime;
use vm::{HostTypeSchema, Program, VmStatus};

fn compile(source: &str) -> Arc<Program> {
    compile_rss_source(source).expect("compile with ngx host catalog")
}

fn sleep_ms_once(ms: i64) -> Arc<Program> {
    compile(&format!("use ngx;\nngx::sleep_ms({ms});\n"))
}

fn two_sleep_ms(a: i64, b: i64) -> Arc<Program> {
    compile(&format!(
        "use ngx;\nngx::sleep_ms({a});\nngx::sleep_ms({b});\n"
    ))
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

fn run_waiting_program_to_halt(program: Arc<Program>, timeout: Duration) {
    let mut worker = WorkerRuntime::start().unwrap();
    let mut exec = worker.bind_access_exec_for_test(program, 1).unwrap();
    let mut registry = IdentityRegistry::with_capacity(DEFAULT_MAX_IN_FLIGHT);
    assert_eq!(
        exec.handle_access(&mut registry, &worker),
        AccessStatus::Again,
        "macro wrapper plus catalog must compile and wait"
    );
    exec.commit_suspend();
    assert!(matches!(exec.vm_status(), Some(VmStatus::Waiting(_))));
    let fd = worker.raw_fd().expect("eventfd");
    assert!(wait_readable(fd, timeout));
    let ready = worker.drain_ready().unwrap();
    assert_eq!(ready, vec![exec.identity()]);
    assert_eq!(
        exec.apply_ready(exec.identity(), &mut registry, &worker),
        ReadyEffect::Halted
    );
    assert_eq!(exec.handle_access(&mut registry, &worker), AccessStatus::Ok);
    worker.shutdown();
}

#[test]
fn catalog_and_wrapper_are_macro_sleep_ms() {
    let catalog = ngx_host_catalog();
    let schema = catalog.function(NGX_SLEEP_MS_NAME).expect("catalogued");
    assert_eq!(schema.name, NGX_SLEEP_MS_NAME);
    assert_eq!(schema.params.len(), 1);
    assert_eq!(schema.params[0].name, "ms");
    assert_eq!(schema.params[0].ty, HostTypeSchema::Int);
    assert_eq!(schema.return_type, HostTypeSchema::Null);
    let mut worker = WorkerRuntime::start().unwrap();
    let mut exec = worker
        .bind_access_exec_for_test(sleep_ms_once(1), 1)
        .unwrap();
    let mut registry = IdentityRegistry::with_capacity(DEFAULT_MAX_IN_FLIGHT);
    assert_eq!(
        exec.handle_access(&mut registry, &worker),
        AccessStatus::Again,
        "macro wrapper plus catalog must compile and wait"
    );
    assert!(matches!(exec.vm_status(), Some(VmStatus::Waiting(_))));
    worker.shutdown();
}

#[test]
fn catalog_sleep_is_seconds_number() {
    let catalog = ngx_host_catalog();
    let schema = catalog.function(NGX_SLEEP_NAME).expect("catalogued");
    assert_eq!(schema.name, NGX_SLEEP_NAME);
    assert_eq!(schema.params.len(), 1);
    assert_eq!(schema.params[0].name, "seconds");
    assert_eq!(schema.params[0].ty, HostTypeSchema::Number);
    assert_eq!(schema.return_type, HostTypeSchema::Null);
}

#[test]
fn no_legacy_sleep_coordinator_in_tracked_sources() {
    for rel in [
        "src/lib.rs",
        "src/host.rs",
        "src/host/mod.rs",
        "src/host/catalog.rs",
        "src/async_bridge.rs",
        "src/runtime.rs",
        "src/request_state.rs",
        "src/access.rs",
        "src/notify.rs",
        "src/config.rs",
        "README.md",
    ] {
        let path = format!("{}/{rel}", env!("CARGO_MANIFEST_DIR"));
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        for banned in [
            "SleepCoordinator",
            "SleepSession",
            "SleepToken",
            "CallOutcome::Yield",
        ] {
            assert!(
                !text.contains(banned),
                "{rel} must not contain legacy {banned}"
            );
        }
    }
}

#[test]
fn waiting_initial_pending_ready_poll_resume() {
    let mut worker = WorkerRuntime::start().unwrap();
    let mut exec = worker
        .bind_access_exec_for_test(sleep_ms_once(8), 1)
        .unwrap();
    let mut registry = IdentityRegistry::with_capacity(DEFAULT_MAX_IN_FLIGHT);
    assert_eq!(
        exec.handle_access(&mut registry, &worker),
        AccessStatus::Again
    );
    exec.commit_suspend();
    assert!(matches!(exec.vm_status(), Some(VmStatus::Waiting(_))));
    let fd = worker.raw_fd().expect("eventfd");
    assert!(wait_readable(fd, Duration::from_secs(2)));
    let ready = worker.drain_ready().unwrap();
    assert_eq!(ready, vec![exec.identity()]);
    assert_eq!(
        exec.apply_ready(exec.identity(), &mut registry, &worker),
        ReadyEffect::Halted
    );
    assert_eq!(exec.handle_access(&mut registry, &worker), AccessStatus::Ok);
    worker.shutdown();
}

#[test]
fn two_consecutive_async_calls() {
    let mut worker = WorkerRuntime::start().unwrap();
    let mut exec = worker
        .bind_access_exec_for_test(two_sleep_ms(1, 1), 1)
        .unwrap();
    let mut registry = IdentityRegistry::with_capacity(8);
    assert_eq!(
        exec.handle_access(&mut registry, &worker),
        AccessStatus::Again
    );
    exec.commit_suspend();
    let fd = worker.raw_fd().expect("eventfd");
    assert!(wait_readable(fd, Duration::from_secs(2)));
    let _ = worker.drain_ready().unwrap();
    assert_eq!(
        exec.apply_ready(exec.identity(), &mut registry, &worker),
        ReadyEffect::WaitingAgain
    );
    assert!(wait_readable(fd, Duration::from_secs(2)));
    let _ = worker.drain_ready().unwrap();
    assert_eq!(
        exec.apply_ready(exec.identity(), &mut registry, &worker),
        ReadyEffect::Halted
    );
    worker.shutdown();
}

#[test]
fn sleep_seconds_float_executes_through_real_vm() {
    let started = Instant::now();
    run_waiting_program_to_halt(
        compile("use ngx;\nngx::sleep(0.03);\n"),
        Duration::from_secs(2),
    );
    let elapsed = started.elapsed();
    assert!(
        elapsed >= Duration::from_millis(20),
        "ngx::sleep(0.03) should wait about 30ms, elapsed {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_millis(500),
        "ngx::sleep(0.03) must not use millisecond semantics, elapsed {elapsed:?}"
    );
}

#[test]
fn sleep_seconds_integer_executes_through_real_vm() {
    run_waiting_program_to_halt(
        compile("use ngx;\nngx::sleep(0);\n"),
        Duration::from_secs(2),
    );
}

#[test]
fn negative_duration_is_host_error() {
    let mut worker = WorkerRuntime::start().unwrap();
    let mut exec = worker
        .bind_access_exec_for_test(sleep_ms_once(-1), 1)
        .unwrap();
    let mut registry = IdentityRegistry::with_capacity(8);
    assert_eq!(
        exec.handle_access(&mut registry, &worker),
        AccessStatus::Error500
    );
    worker.shutdown();
}

#[test]
fn negative_sleep_seconds_is_host_error() {
    let mut worker = WorkerRuntime::start().unwrap();
    let mut exec = worker
        .bind_access_exec_for_test(compile("use ngx;\nngx::sleep(-1);\n"), 1)
        .unwrap();
    let mut registry = IdentityRegistry::with_capacity(8);
    assert_eq!(
        exec.handle_access(&mut registry, &worker),
        AccessStatus::Error500
    );
    worker.shutdown();
}

#[test]
fn nan_infinity_and_overflow_sleep_seconds_are_host_errors() {
    for source in [
        "use ngx;\nlet nan = 0.0 / 0.0;\nngx::sleep(nan);\n",
        "use ngx;\nlet inf = 1.0 / 0.0;\nngx::sleep(inf);\n",
        "use ngx;\nlet ninf = 0.0 - 1.0 / 0.0;\nngx::sleep(ninf);\n",
        "use ngx;\nlet huge = 10000000000.0;\nngx::sleep(huge * huge * huge);\n",
    ] {
        let mut worker = WorkerRuntime::start().unwrap();
        let mut exec = worker
            .bind_access_exec_for_test(compile(source), 1)
            .unwrap();
        let mut registry = IdentityRegistry::with_capacity(8);
        assert_eq!(
            exec.handle_access(&mut registry, &worker),
            AccessStatus::Error500,
            "{source} must be rejected by ngx::sleep"
        );
        worker.shutdown();
    }
}

#[test]
fn wrong_arity_is_rejected_at_compile() {
    assert!(compile_rss_source("use ngx;\nngx::sleep();\n").is_err());
    assert!(compile_rss_source("use ngx;\nngx::sleep(1, 2);\n").is_err());
    assert!(compile_rss_source("use ngx;\nngx::sleep_ms();\n").is_err());
    assert!(compile_rss_source("use ngx;\nngx::sleep_ms(1, 2);\n").is_err());
}

#[test]
fn print_and_runtime_sleep_are_rejected() {
    assert!(compile_rss_source("print(\"x\");\n").is_err());
    assert!(compile_rss_source("runtime::sleep(1);\n").is_err());
}
