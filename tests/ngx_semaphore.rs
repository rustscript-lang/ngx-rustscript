//! `ngx::semaphore` family: worker-owned state, VM-local handles, FIFO waits.
//!
//! Covers catalog/manifest/runtime registration at the exact `195` baseline,
//! resource-key typing, phase and Log fail-closed policy, handle validation,
//! OpenResty-style `count` semantics, timeout, request cleanup, worker
//! shutdown, and the shared `DEFAULT_MAX_IN_FLIGHT` reservation path.
//!
//! The worker registry is thread-local, so tests that assert exact live counts
//! reset it first: `cargo test` may run every test on one thread
//! (`--test-threads=1`, as CI does).

use ngx_rustscript_nginx_test_stubs as _;

use std::os::unix::io::RawFd;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ngx_rustscript::config::{ConfigError, compile_rss_source, compile_rss_source_for_phase};
use ngx_rustscript::host::{
    HostCapability, HostFamily, HostSyncKind, NGX_SEMAPHORE_COUNT_NAME, NGX_SEMAPHORE_NEW_NAME,
    NGX_SEMAPHORE_POST_NAME, NGX_SEMAPHORE_RESOURCE_KEY, NGX_SEMAPHORE_WAIT_NAME, NgxPhase,
    bind_ngx_hosts, host_allowed_in_phase, manifest_entry, ngx_host_catalog, ngx_host_manifest,
    production_runtime_host_names,
};
use ngx_rustscript::notify::DEFAULT_MAX_IN_FLIGHT;
use ngx_rustscript::request_state::{AccessStatus, IdentityRegistry, ReadyEffect};
use ngx_rustscript::runtime::WorkerRuntime;
use ngx_rustscript::semaphore::{live_semaphore_count, shutdown_current};
use vm::{HostParamPassing, HostTypeSchema, Program, Value, Vm, VmStatus};

const NEW: &str = NGX_SEMAPHORE_NEW_NAME;
const WAIT: &str = NGX_SEMAPHORE_WAIT_NAME;
const POST: &str = NGX_SEMAPHORE_POST_NAME;
const COUNT: &str = NGX_SEMAPHORE_COUNT_NAME;
const HOSTS: [&str; 4] = [NEW, WAIT, POST, COUNT];

/// Scheduler candidates that must stay absent from the production surface.
const GATED_CANDIDATES: &[&str] = &[
    "ngx::semaphore",
    "ngx::thread::spawn",
    "ngx::thread::wait",
    "ngx::thread::kill",
    "ngx::timer::at",
    "ngx::timer::every",
    "ngx::timer::pending_count",
    "ngx::timer::running_count",
    "ngx::on_abort",
    "ngx::run_worker_thread",
];

fn compile(source: &str) -> Arc<Program> {
    compile_rss_source(source).unwrap_or_else(|err| panic!("compile failed: {err}"))
}

fn run_sync(source: &str) -> Result<VmStatus, String> {
    let program = compile(source);
    let mut vm = Vm::try_new((*program).clone()).expect("construct vm");
    bind_ngx_hosts(&mut vm);
    vm.run().map_err(|err| err.to_string())
}

fn run_value(source: &str) -> Result<Value, String> {
    let program = compile(source);
    let mut vm = Vm::try_new((*program).clone()).expect("construct vm");
    bind_ngx_hosts(&mut vm);
    match vm.run().map_err(|err| err.to_string())? {
        VmStatus::Halted => Ok(vm.stack().last().cloned().unwrap_or(Value::Null)),
        other => Err(format!("expected Halted, got {other:?}")),
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

/// Request-owned driver: one `AccessExec` plus the shared worker bridge.
struct Driver {
    worker: WorkerRuntime,
    exec: ngx_rustscript::request_state::AccessExec,
    registry: IdentityRegistry<u64>,
}

impl Driver {
    fn start(program: Arc<Program>) -> Self {
        let worker = WorkerRuntime::start().expect("worker runtime");
        Self::from_worker(worker, program)
    }

    fn from_worker(mut worker: WorkerRuntime, program: Arc<Program>) -> Self {
        let exec = worker.bind_access_exec(program, 1).expect("bind");
        Self {
            worker,
            exec,
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

    fn cleanup_pending(&mut self) {
        self.exec.cleanup_wait(&self.worker);
        self.worker.shutdown();
    }

    fn pump_until(mut self, timeout: Duration) -> AccessStatus {
        let deadline = Instant::now() + timeout;
        let mut status = self.handle();
        while status == AccessStatus::Again && Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let fd = self.worker.raw_fd().expect("eventfd");
            if !wait_readable(fd, remaining) {
                self.worker.shutdown();
                panic!("timed out waiting for semaphore host completion");
            }
            let _ = self.worker.drain_ready().unwrap();
            match self
                .exec
                .apply_ready(self.exec.identity(), &mut self.registry, &self.worker)
            {
                ReadyEffect::Halted => {
                    status = AccessStatus::Ok;
                    break;
                }
                ReadyEffect::Failed => {
                    status = AccessStatus::Error500;
                    break;
                }
                ReadyEffect::WaitingAgain => {
                    self.exec.commit_suspend();
                    status = AccessStatus::Again;
                }
                ReadyEffect::Deferred | ReadyEffect::Ignored => {
                    status = self.handle();
                }
            }
        }
        self.worker.shutdown();
        status
    }
}

#[test]
fn semaphore_catalog_is_typed_and_phase_closed() {
    let catalog = ngx_host_catalog();
    assert_eq!(ngx_host_manifest().len(), 195);
    assert_eq!(catalog.functions().len(), 195);
    assert_eq!(production_runtime_host_names().len(), 195);
    assert!(catalog.resource(NGX_SEMAPHORE_RESOURCE_KEY).is_some());

    for name in HOSTS {
        let entry = manifest_entry(name).unwrap_or_else(|| panic!("missing manifest entry {name}"));
        assert_eq!(entry.family, HostFamily::Semaphore);
        assert_eq!(entry.capabilities, &[HostCapability::Semaphore]);
        assert_eq!(
            entry.sync,
            if name == WAIT {
                HostSyncKind::Async
            } else {
                HostSyncKind::Sync
            }
        );
        assert_eq!(
            entry.openresty_name,
            name.replace("::", "."),
            "{name} must name its OpenResty counterpart"
        );
        assert!(host_allowed_in_phase(name, NgxPhase::Access));
        assert!(host_allowed_in_phase(name, NgxPhase::Content));
        for phase in [
            NgxPhase::Log,
            NgxPhase::Rewrite,
            NgxPhase::Timer,
            NgxPhase::Init,
        ] {
            assert!(
                !host_allowed_in_phase(name, phase),
                "{name} must stay fail-closed in {phase}"
            );
        }
    }

    let new = catalog.function(NEW).expect("new schema");
    assert_eq!(new.params.len(), 1);
    assert_eq!(new.params[0].name, "count");
    assert_eq!(new.params[0].ty, HostTypeSchema::Int);
    assert!(matches!(
        new.return_type,
        HostTypeSchema::Resource(ref key) if key.as_str() == NGX_SEMAPHORE_RESOURCE_KEY
    ));

    let wait = catalog.function(WAIT).expect("wait schema");
    assert_eq!(wait.params.len(), 2);
    assert_eq!(wait.params[0].name, "sem");
    assert_eq!(wait.params[0].passing, HostParamPassing::Borrow);
    assert!(matches!(
        wait.params[0].ty,
        HostTypeSchema::Resource(ref key) if key.as_str() == NGX_SEMAPHORE_RESOURCE_KEY
    ));
    assert_eq!(wait.params[1].name, "timeout");
    assert_eq!(wait.params[1].ty, HostTypeSchema::Unknown);
    assert_eq!(
        wait.return_type,
        HostTypeSchema::Bool,
        "wait must report acquire versus timeout"
    );

    let post = catalog.function(POST).expect("post schema");
    assert_eq!(post.params.len(), 2);
    assert_eq!(post.params[0].passing, HostParamPassing::Borrow);
    assert_eq!(post.params[1].name, "count");
    assert_eq!(post.params[1].ty, HostTypeSchema::Unknown);
    assert_eq!(post.return_type, HostTypeSchema::Null);

    let count = catalog.function(COUNT).expect("count schema");
    assert_eq!(count.params.len(), 1);
    assert_eq!(count.params[0].passing, HostParamPassing::Borrow);
    assert_eq!(count.return_type, HostTypeSchema::Int);

    for phase in [NgxPhase::Access, NgxPhase::Content] {
        compile_rss_source_for_phase(
            "use ngx;\nlet sem = ngx::semaphore::new(1);\nngx::semaphore::wait(sem, 0);\nngx::semaphore::post(sem, 1);\nngx::semaphore::count(sem);\n",
            phase,
        )
        .unwrap_or_else(|err| panic!("{phase:?} must accept semaphore hosts: {err}"));
    }
    match compile_rss_source_for_phase(
        "use ngx;\nlet sem = ngx::semaphore::new(0);\nngx::semaphore::count(sem);\n",
        NgxPhase::Log,
    ) {
        Err(ConfigError::PhaseForbidden { name, phase }) => {
            assert_eq!(name, NEW);
            assert_eq!(phase, NgxPhase::Log);
        }
        other => panic!("Log must fail closed at configuration compile, got {other:?}"),
    }
}

#[test]
fn binding_installs_the_family_and_keeps_gated_candidates_absent() {
    let program = compile("let x = 1;\n");
    let mut vm = Vm::try_new((*program).clone()).expect("construct vm");
    bind_ngx_hosts(&mut vm);
    for name in HOSTS {
        assert!(
            vm.has_bound_function(name),
            "production bind must install {name}"
        );
    }
    for name in GATED_CANDIDATES {
        assert!(
            !vm.has_bound_function(name),
            "production bind must not install {name}"
        );
        assert!(
            ngx_host_catalog().function(name).is_none(),
            "{name} must not be catalogued"
        );
        assert!(
            manifest_entry(name).is_none(),
            "{name} must not be in the manifest"
        );
    }
}

#[test]
fn script_round_trip_uses_openresty_count_semantics() {
    shutdown_current();
    assert_eq!(
        run_value(
            "use ngx;\nlet s = ngx::semaphore::new(2);\nngx::semaphore::post(s, 3);\nngx::semaphore::count(s);\n"
        )
        .expect("count"),
        Value::Int(5)
    );
    assert_eq!(
        run_value(
            "use ngx;\nlet s = ngx::semaphore::new(0);\nngx::semaphore::post(s, null);\nngx::semaphore::count(s);\n"
        )
        .expect("null counts as one permit"),
        Value::Int(1)
    );
    assert_eq!(
        run_value(
            "use ngx;\nlet s = ngx::semaphore::new(4);\nngx::semaphore::post(s, 0);\nngx::semaphore::count(s);\n"
        )
        .expect("zero post"),
        Value::Int(4)
    );
    assert!(matches!(
        run_sync("use ngx;\nngx::semaphore::new(1);\n"),
        Ok(VmStatus::Halted)
    ));
    assert_eq!(
        live_semaphore_count(),
        0,
        "a released VM table removes the semaphore handle"
    );
}

#[test]
fn validation_errors_fail_closed_with_the_host_name() {
    shutdown_current();
    for (source, needle) in [
        ("use ngx;\nngx::semaphore::new(-1);\n", "non-negative"),
        ("use ngx;\nngx::semaphore::new(1000001);\n", "1000000"),
        (
            "use ngx;\nlet s = ngx::semaphore::new(1000000);\nngx::semaphore::post(s, 1);\n",
            "ceiling",
        ),
        (
            "use ngx;\nlet s = ngx::semaphore::new(1);\nngx::semaphore::post(s, 1.5);\n",
            "integer count",
        ),
    ] {
        let err = run_sync(source).expect_err(&format!("must fail: {source}"));
        assert!(
            err.contains(needle),
            "{source} must fail with {needle}, got {err}"
        );
        assert!(
            err.contains("ngx::semaphore::"),
            "{source} must name the semaphore host, got {err}"
        );
    }
    assert_eq!(live_semaphore_count(), 0);
}

#[test]
fn forged_integer_handles_never_compile() {
    // The catalog types the handle as `resource<ngx.semaphore>`, so an integer
    // (forged, foreign, or stale) cannot reach `count`/`post`/`wait` at all.
    for source in [
        "use ngx;\nngx::semaphore::count(0);\n",
        "use ngx;\nngx::semaphore::wait(7, null);\n",
        "use ngx;\nlet s = ngx::semaphore::new(1);\nngx::semaphore::post(0, 1);\n",
    ] {
        let err = compile_rss_source(source)
            .expect_err("a non-resource argument must fail configuration compile");
        let text = err.to_string();
        assert!(
            text.contains("resource") || text.contains("ngx::semaphore"),
            "{source} must name the resource typing failure, got {text}"
        );
    }
}

#[test]
fn sync_only_semaphore_program_stays_on_pooled_path() {
    let program = compile(
        "use ngx;\nlet s = ngx::semaphore::new(1);\nngx::semaphore::post(s, 1);\nngx::semaphore::count(s);\n",
    );
    assert!(
        !ngx_rustscript::config::program_requires_request_state(&program),
        "sync-only semaphore imports must stay on the pooled VM path"
    );
}

#[test]
fn async_semaphore_wait_program_uses_request_owned_path() {
    let program =
        compile("use ngx;\nlet s = ngx::semaphore::new(0);\nngx::semaphore::wait(s, null);\n");
    assert!(
        ngx_rustscript::config::program_requires_request_state(&program),
        "async semaphore wait must select the request-owned VM path"
    );
}

#[test]
fn wait_acquires_an_available_permit_without_suspending() {
    shutdown_current();
    let status = Driver::start(compile(
        "use ngx;\nlet s = ngx::semaphore::new(1);\nngx::semaphore::wait(s, null);\nngx::semaphore::count(s);\n",
    ))
    .handle();
    assert_eq!(
        status,
        AccessStatus::Ok,
        "a ready permit must not suspend the request"
    );
    assert_eq!(live_semaphore_count(), 0);
}

#[test]
fn wait_times_out_through_the_bridge_without_leaking_state() {
    shutdown_current();
    let status = Driver::start(compile(
        "use ngx;\nlet s = ngx::semaphore::new(0);\nngx::semaphore::wait(s, 0.02);\nngx::semaphore::count(s);\n",
    ))
    .pump_until(Duration::from_secs(5));
    assert_eq!(
        status,
        AccessStatus::Ok,
        "a timed-out wait resumes the VM without failing the request"
    );
    assert_eq!(live_semaphore_count(), 0);
}

/// Drives a VM with a real bridge until it halts and returns the script value.
fn run_async_value(source: &str) -> Value {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_time()
        .build()
        .expect("runtime");
    let program = compile(source);
    let mut vm = Vm::try_new((*program).clone()).expect("construct vm");
    bind_ngx_hosts(&mut vm);
    vm.set_async_bridge(Box::new(ngx_rustscript::async_bridge::NgxAsyncBridge::new(
        runtime.handle().clone(),
    )))
    .expect("async bridge");
    let mut status = vm.run().expect("run");
    let noop = std::task::Waker::noop();
    let start = Instant::now();
    while matches!(status, VmStatus::Waiting(_)) && start.elapsed() < Duration::from_secs(5) {
        let mut cx = std::task::Context::from_waker(noop);
        match vm.poll_waiting_host_op(&mut cx) {
            std::task::Poll::Pending => std::thread::sleep(Duration::from_millis(2)),
            std::task::Poll::Ready(Ok(())) => status = vm.resume().expect("resume"),
            std::task::Poll::Ready(Err(err)) => panic!("semaphore host error: {err}"),
        }
    }
    assert_eq!(status, VmStatus::Halted, "the wait never completed");
    vm.stack().last().cloned().unwrap_or(Value::Null)
}

#[test]
fn wait_reports_acquire_versus_timeout_to_the_script() {
    shutdown_current();
    assert_eq!(
        run_async_value(
            "use ngx;\nlet s = ngx::semaphore::new(1);\nngx::semaphore::wait(s, null);\n"
        ),
        Value::Bool(true),
        "a free permit acquires without a timer"
    );
    assert_eq!(
        run_async_value("use ngx;\nlet s = ngx::semaphore::new(1);\nngx::semaphore::wait(s, 0);\n"),
        Value::Bool(true),
        "a zero timeout still takes a free permit"
    );
    assert_eq!(
        run_async_value("use ngx;\nlet s = ngx::semaphore::new(0);\nngx::semaphore::wait(s, 0);\n"),
        Value::Bool(false),
        "a zero timeout is an immediate check on an empty semaphore"
    );
    assert_eq!(
        run_async_value(
            "use ngx;\nlet s = ngx::semaphore::new(0);\nngx::semaphore::wait(s, 0.02);\n"
        ),
        Value::Bool(false),
        "an elapsed timeout reports false, never an error"
    );
    assert_eq!(
        run_async_value(
            "use ngx;\nlet s = ngx::semaphore::new(0);\nngx::semaphore::post(s, 2);\nngx::semaphore::wait(s, 0.5);\n"
        ),
        Value::Bool(true),
        "a posted permit satisfies the queued wait"
    );
    assert_eq!(
        live_semaphore_count(),
        0,
        "the value checks must not leak worker semaphore entries"
    );
}

#[test]
fn request_cleanup_releases_parked_waits_and_the_worker_handle() {
    shutdown_current();
    let mut driver = Driver::start(compile(
        "use ngx;\nlet s = ngx::semaphore::new(0);\nngx::semaphore::wait(s, 60);\n",
    ));
    assert_eq!(driver.handle(), AccessStatus::Again);
    assert_eq!(driver.worker.in_flight().unwrap(), 1);
    assert_eq!(live_semaphore_count(), 1);

    // Client close / request end: the parked wait is cancelled and the worker
    // reservation is released exactly once.
    driver.cleanup_pending();
    assert_eq!(driver.worker.in_flight().unwrap(), 0);
    assert_eq!(
        live_semaphore_count(),
        0,
        "dropping the request-owned VM removes its VM-local semaphore handle"
    );
}

#[test]
fn semaphore_waits_share_the_in_flight_capacity_cap() {
    shutdown_current();
    let mut worker = WorkerRuntime::start_with_capacity(1).expect("worker");
    let program =
        compile("use ngx;\nlet s = ngx::semaphore::new(0);\nngx::semaphore::wait(s, 60);\n");
    let mut registry = IdentityRegistry::with_capacity(4);

    let mut first = worker
        .bind_access_exec(Arc::clone(&program), 1)
        .expect("first");
    assert_eq!(
        first.handle_access(&mut registry, &worker),
        AccessStatus::Again
    );
    first.commit_suspend();
    assert_eq!(worker.in_flight().unwrap(), 1);

    let mut second = worker
        .bind_access_exec(Arc::clone(&program), 2)
        .expect("second");
    assert_eq!(
        second.handle_access(&mut registry, &worker),
        AccessStatus::Error500,
        "a semaphore wait must not bypass DEFAULT_MAX_IN_FLIGHT"
    );

    first.cleanup_wait(&worker);
    assert_eq!(worker.in_flight().unwrap(), 0);
    second.cleanup_wait(&worker);
    worker.shutdown();
    assert_eq!(live_semaphore_count(), 0);
}

#[test]
fn worker_shutdown_closes_live_semaphores_and_releases_reservations() {
    shutdown_current();
    let mut driver = Driver::start(compile(
        "use ngx;\nlet s = ngx::semaphore::new(0);\nngx::semaphore::wait(s, 60);\n",
    ));
    assert_eq!(driver.handle(), AccessStatus::Again);
    assert_eq!(live_semaphore_count(), 1);

    driver.worker.shutdown();
    assert_eq!(
        live_semaphore_count(),
        0,
        "worker exit must empty the semaphore registry"
    );
    assert_eq!(driver.worker.in_flight().unwrap(), 0);
    driver.worker.shutdown();
}
