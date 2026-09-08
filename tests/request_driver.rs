//! Request-driver state machine for access suspend/resume.
//!
//! Real pd-vm drives `VmStatus::Waiting`. A recording NGINX sink captures
//! retain, release, and phase continuation.

use ngx_rustscript_nginx_test_stubs as _;

use std::os::unix::io::RawFd;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ngx::core::Status;
use ngx_rustscript::config::compile_rss_source;
use ngx_rustscript::notify::{DEFAULT_MAX_IN_FLIGHT, RequestIdentity};
use ngx_rustscript::request_state::{
    AccessExec, AccessStatus, IdentityRegistry, ReadyEffect, RecordingSink, apply_ready_effect,
};
use ngx_rustscript::runtime::WorkerRuntime;
use vm::{Program, VmStatus};

fn compile(source: &str) -> Arc<Program> {
    compile_rss_source(source).expect("compile with ngx::sleep catalog")
}

fn sleep_once(ms: i64) -> Arc<Program> {
    compile(&format!("use ngx;\nngx::sleep_ms({ms});\n"))
}

fn two_sleeps(a: i64, b: i64) -> Arc<Program> {
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

struct Driver {
    worker: WorkerRuntime,
    exec: AccessExec,
    sink: RecordingSink,
    registry: IdentityRegistry<u64>,
}

impl Driver {
    fn new(program: Arc<Program>) -> Self {
        let mut worker = WorkerRuntime::start().unwrap();
        let exec = worker.bind_access_exec(program, 1).expect("bind");
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
}

#[test]
fn halted_without_wait_returns_ok_and_does_not_advance_phase() {
    let mut driver = Driver::new(compile("let x = 1 + 2;\n"));
    assert_eq!(driver.handle(), AccessStatus::Ok);
    assert_eq!(driver.sink.retains, 0);
    assert_eq!(driver.sink.releases, 0);
    assert_eq!(driver.sink.phase_continues, 0);
    assert_eq!(driver.exec.vm_status(), Some(VmStatus::Halted));
}

#[test]
fn first_wait_returns_again_without_phase_advance() {
    let mut driver = Driver::new(sleep_once(30));
    assert_eq!(driver.handle(), AccessStatus::Again);
    assert_eq!(driver.registry.len(), 1);
    assert_eq!(driver.sink.retains, 0);
    assert_eq!(driver.sink.releases, 0);
    assert_eq!(driver.sink.phase_continues, 0);
    assert!(matches!(
        driver.exec.vm_status(),
        Some(VmStatus::Waiting(_))
    ));
}

#[test]
fn normal_completion_resumes_same_vm_and_advances_phase_once() {
    let mut driver = Driver::new(sleep_once(5));
    assert_eq!(driver.handle(), AccessStatus::Again);
    let effect = driver.wait_ready();
    assert_eq!(effect, ReadyEffect::Halted);
    assert_eq!(driver.sink.phase_continues, 1);
    assert_eq!(driver.registry.len(), 0);
    assert_eq!(driver.handle(), AccessStatus::Ok);
    assert_eq!(driver.sink.phase_continues, 1);
}

#[test]
fn premature_duplicate_callback_is_ignored() {
    let mut driver = Driver::new(sleep_once(1));
    assert_eq!(driver.handle(), AccessStatus::Again);
    let first = driver.wait_ready();
    assert_eq!(first, ReadyEffect::Halted);
    let second =
        driver
            .exec
            .apply_ready(driver.exec.identity(), &mut driver.registry, &driver.worker);
    apply_ready_effect(second, &mut driver.sink);
    assert_eq!(second, ReadyEffect::Ignored);
    assert_eq!(driver.sink.phase_continues, 1);
    assert_eq!(driver.handle(), AccessStatus::Ok);
}

#[test]
fn two_sequential_sleeps_resume_twice_and_advance_phase_once() {
    let mut driver = Driver::new(two_sleeps(1, 2));
    assert_eq!(driver.handle(), AccessStatus::Again);
    let first = driver.wait_ready();
    assert_eq!(first, ReadyEffect::WaitingAgain);
    assert_eq!(driver.sink.phase_continues, 0);
    assert_eq!(driver.handle(), AccessStatus::Again);
    let second = driver.wait_ready();
    assert_eq!(second, ReadyEffect::Halted);
    assert_eq!(driver.sink.phase_continues, 1);
    assert_eq!(driver.handle(), AccessStatus::Ok);
}

#[test]
fn vm_error_on_first_run_is_http_500_without_phase_continue() {
    let mut driver = Driver::new(compile("1 / 0;\n"));
    assert_eq!(driver.handle(), AccessStatus::Error500);
    assert_eq!(driver.sink.phase_continues, 0);
}

#[test]
fn vm_error_on_resume_is_http_500_with_single_phase_continue() {
    let program = compile("use ngx;\nngx::sleep_ms(1);\n1 / 0;\n");
    let mut driver = Driver::new(program);
    assert_eq!(driver.handle(), AccessStatus::Again);
    let effect = driver.wait_ready();
    assert_eq!(effect, ReadyEffect::Failed);
    assert_eq!(driver.sink.phase_continues, 1);
    assert_eq!(driver.handle(), AccessStatus::Error500);
}

#[test]
fn unknown_identity_is_ignored() {
    let mut driver = Driver::new(sleep_once(1));
    assert_eq!(driver.handle(), AccessStatus::Again);
    let effect = driver.exec.apply_ready(
        RequestIdentity::new(99, 99),
        &mut driver.registry,
        &driver.worker,
    );
    apply_ready_effect(effect, &mut driver.sink);
    assert_eq!(effect, ReadyEffect::Ignored);
    assert_eq!(driver.sink.phase_continues, 0);
    assert_eq!(driver.handle(), AccessStatus::Again);
}

#[test]
fn wrong_generation_identity_is_ignored() {
    let mut driver = Driver::new(sleep_once(1));
    assert_eq!(driver.handle(), AccessStatus::Again);
    let identity = driver.exec.identity();
    let forged = RequestIdentity::new(identity.generation().wrapping_add(1), identity.request_id());
    let effect = driver
        .exec
        .apply_ready(forged, &mut driver.registry, &driver.worker);
    apply_ready_effect(effect, &mut driver.sink);
    assert_eq!(effect, ReadyEffect::Ignored);
    assert_eq!(driver.sink.phase_continues, 0);
    assert_eq!(driver.registry.len(), 1);
}

#[test]
fn registry_bound_rejects_when_full() {
    let mut registry = IdentityRegistry::<u64>::with_capacity(1);
    registry
        .try_insert(RequestIdentity::new(1, 1), 1)
        .expect("first");
    let err = registry
        .try_insert(RequestIdentity::new(1, 2), 2)
        .expect_err("full");
    assert_eq!(err, ngx_rustscript::request_state::RegistryError::Full);
    assert_eq!(registry.len(), 1);
}

#[test]
fn duplicate_registry_insert_fails_and_keeps_original() {
    let mut registry = IdentityRegistry::<u64>::with_capacity(8);
    let identity = RequestIdentity::new(1, 1);
    registry.try_insert(identity, 1).expect("first");
    let err = registry
        .try_insert(identity, 2)
        .expect_err("duplicate must not replace");
    assert_eq!(err, ngx_rustscript::request_state::RegistryError::Duplicate);
    assert_eq!(registry.get(identity), Some(&1));
}

#[test]
fn same_key_same_value_insert_is_idempotent() {
    let mut registry = IdentityRegistry::<u64>::with_capacity(8);
    let identity = RequestIdentity::new(1, 1);
    registry.try_insert(identity, 1).expect("first");
    registry
        .try_insert(identity, 1)
        .expect("same key and value must be idempotent");
    assert_eq!(registry.len(), 1);
    assert_eq!(registry.get(identity), Some(&1));
}

#[test]
fn two_requests_receive_distinct_identities() {
    let mut worker = WorkerRuntime::start().unwrap();
    let program = sleep_once(5);
    let a = worker.bind_access_exec(Arc::clone(&program), 1).expect("a");
    let b = worker.bind_access_exec(program, 2).expect("b");
    assert_ne!(a.identity(), b.identity());
    worker.shutdown();
}

#[test]
fn waiting_maps_to_nginx_again() {
    assert_eq!(AccessStatus::Again.as_nginx(), Status::NGX_AGAIN);
    assert_eq!(AccessStatus::Ok.as_nginx(), Status::NGX_OK);
}

#[test]
fn access_again_is_the_phase_engine_wait_code() {
    assert_eq!(Status::NGX_AGAIN.0, -2);
}
