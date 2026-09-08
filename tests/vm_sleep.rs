//! Discriminating tests for the generic pd-host-function async bridge.
//!
//! Real pd-vm compiles `ngx::sleep`. There is no handwritten Yield host.

use ngx_rustscript_nginx_test_stubs as _;

use std::os::unix::io::RawFd;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ngx_rustscript::config::compile_rss_source;
use ngx_rustscript::host::{NGX_SLEEP_NAME, ngx_host_catalog};
use ngx_rustscript::notify::DEFAULT_MAX_IN_FLIGHT;
use ngx_rustscript::request_state::{AccessStatus, IdentityRegistry, ReadyEffect};
use ngx_rustscript::runtime::WorkerRuntime;
use vm::{HostTypeSchema, Program, VmStatus};

fn compile(source: &str) -> Arc<Program> {
    compile_rss_source(source).expect("compile with ngx::sleep catalog")
}

fn sleep_once(ms: i64) -> Arc<Program> {
    compile(&format!("use ngx;\nngx::sleep({ms});\n"))
}

fn two_sleeps(a: i64, b: i64) -> Arc<Program> {
    compile(&format!("use ngx;\nngx::sleep({a});\nngx::sleep({b});\n"))
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

#[test]
fn catalog_and_wrapper_are_macro_sleep() {
    let catalog = ngx_host_catalog();
    let schema = catalog.function(NGX_SLEEP_NAME).expect("catalogued");
    assert_eq!(schema.name, NGX_SLEEP_NAME);
    assert_eq!(schema.params.len(), 1);
    assert_eq!(schema.params[0].name, "ms");
    assert_eq!(schema.params[0].ty, HostTypeSchema::Int);
    assert_eq!(schema.return_type, HostTypeSchema::Null);
    let mut worker = WorkerRuntime::start().unwrap();
    let mut exec = worker.bind_access_exec(sleep_once(1), 1).unwrap();
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
fn no_legacy_sleep_coordinator_in_tracked_sources() {
    for rel in [
        "src/lib.rs",
        "src/host.rs",
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
    let mut exec = worker.bind_access_exec(sleep_once(8), 1).unwrap();
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
    let mut exec = worker.bind_access_exec(two_sleeps(1, 1), 1).unwrap();
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
fn negative_duration_is_host_error() {
    let mut worker = WorkerRuntime::start().unwrap();
    let mut exec = worker.bind_access_exec(sleep_once(-1), 1).unwrap();
    let mut registry = IdentityRegistry::with_capacity(8);
    assert_eq!(
        exec.handle_access(&mut registry, &worker),
        AccessStatus::Error500
    );
    worker.shutdown();
}

#[test]
fn wrong_arity_is_rejected_at_compile() {
    assert!(compile_rss_source("use ngx;\nngx::sleep();\n").is_err());
    assert!(compile_rss_source("use ngx;\nngx::sleep(1, 2);\n").is_err());
}

#[test]
fn print_and_runtime_sleep_are_rejected() {
    assert!(compile_rss_source("print(\"x\");\n").is_err());
    assert!(compile_rss_source("runtime::sleep(1);\n").is_err());
}
