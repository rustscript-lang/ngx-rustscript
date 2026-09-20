//! Worker-local Tokio runtime for polling generic host futures.
//!
//! Created in NGINX `init_process` after fork, only in `NGX_PROCESS_WORKER`
//! or `NGX_PROCESS_SINGLE`. Helper, master, config, and `nginx -t` never
//! create Tokio threads or the eventfd. Wakers own only a
//! [`crate::notify::CompletionSender`] and a
//! [`crate::notify::RequestIdentity`]. They never hold NGINX request/pool/VM
//! pointers and never call `ngx_post_event`.

use std::cell::RefCell;
use std::fmt;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::task::Waker;
use std::time::Duration;

use nginx_sys::{ngx_cycle_t, ngx_event_t, ngx_int_t, ngx_uint_t};
use ngx::core::Status;
use ngx::ffi::{NGX_LOG_ERR, NGX_LOG_NOTICE, NGX_PROCESS_SINGLE, NGX_PROCESS_WORKER, ngx_process};
use ngx::ngx_log_error;
use tokio::runtime::{Handle, Runtime};
use vm::{Program, Vm};

use crate::notify::{
    CompletionPermit, CompletionSender, DEFAULT_MAX_IN_FLIGHT, NotifyBridge, NotifyError,
    RequestIdentity, eventfd_waker,
};
use crate::request_state::{AccessExec, AsyncWait, ReusableVmKey, SubmitError};

thread_local! {
    static WORKER: RefCell<Option<WorkerRuntime>> = const { RefCell::new(None) };
}

/// Fail-closed worker runtime errors.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RuntimeError {
    Notify(NotifyError),
    RuntimeBuild(String),
    HostRegistry(String),
    AlreadyStarted,
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Notify(err) => write!(f, "{err}"),
            Self::RuntimeBuild(msg) => write!(f, "tokio runtime build failed: {msg}"),
            Self::HostRegistry(msg) => write!(f, "host registry: {msg}"),
            Self::AlreadyStarted => write!(f, "worker runtime is already started"),
        }
    }
}

impl std::error::Error for RuntimeError {}

impl From<NotifyError> for RuntimeError {
    fn from(err: NotifyError) -> Self {
        Self::Notify(err)
    }
}

fn is_worker_or_single() -> bool {
    // SAFETY: `ngx_process` is NGINX process-lifetime state. Read on the
    // worker/main thread from `init_process`.
    let process = unsafe { ngx_process };
    process == NGX_PROCESS_WORKER as ngx_uint_t || process == NGX_PROCESS_SINGLE as ngx_uint_t
}

const MAX_IDLE_REUSABLE_REQUEST_VMS: usize = 4;

fn request_vm_ngx_catalog_abi() -> vm::HostApiFingerprint {
    static ABI: std::sync::OnceLock<vm::HostApiFingerprint> = std::sync::OnceLock::new();
    *ABI.get_or_init(|| crate::host::compile_host_catalog().fingerprint())
}

struct ReusableRequestVm {
    key: ReusableVmKey,
    vm: Vm,
}

#[derive(Default)]
struct ReusableRequestVmPool {
    idle: Vec<ReusableRequestVm>,
    created: usize,
}

impl ReusableRequestVmPool {
    fn take(&mut self, key: &ReusableVmKey) -> Result<Vm, String> {
        if let Some(index) = self.idle.iter().position(|entry| entry.key.matches(key)) {
            return Ok(self.idle.swap_remove(index).vm);
        }
        let mut vm = Vm::new_shared(Arc::clone(&key.program));
        crate::request_state::bind_program_hosts(&mut vm, &key.policy)?;
        self.created = self.created.saturating_add(1);
        Ok(vm)
    }

    fn recycle(&mut self, key: ReusableVmKey, mut vm: Vm, runtime: &Handle) {
        if self.idle.len() >= MAX_IDLE_REUSABLE_REQUEST_VMS {
            return;
        }
        if crate::vm_pool::reset_request_vm_for_pool(&mut vm, runtime) {
            self.idle.push(ReusableRequestVm { key, vm });
        }
    }
}

/// Per-worker Tokio runtime, eventfd bridge, and generation counter.
pub struct WorkerRuntime {
    runtime: Option<Runtime>,
    notify: NotifyBridge,
    next_generation: u64,
    reusable_request_vms: RefCell<ReusableRequestVmPool>,
    stopped: bool,
}

impl WorkerRuntime {
    pub fn start() -> Result<Self, RuntimeError> {
        Self::start_with_capacity(DEFAULT_MAX_IN_FLIGHT)
    }

    pub fn start_with_capacity(cap: usize) -> Result<Self, RuntimeError> {
        crate::host::ngx_host_registry().map_err(RuntimeError::HostRegistry)?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_time()
            .enable_io()
            .thread_name("ngx-rustscript-async")
            .build()
            .map_err(|err| RuntimeError::RuntimeBuild(err.to_string()))?;
        let notify = NotifyBridge::with_capacity(cap)?;
        crate::timer::install_worker_timers(runtime.handle().clone(), notify.sender());
        Ok(Self {
            runtime: Some(runtime),
            notify,
            next_generation: 1,
            reusable_request_vms: RefCell::new(ReusableRequestVmPool::default()),
            stopped: false,
        })
    }

    pub fn sender(&self) -> CompletionSender {
        self.notify.sender()
    }

    pub fn raw_fd(&self) -> Option<std::os::unix::io::RawFd> {
        self.notify.raw_fd()
    }

    pub fn is_accepting(&self) -> bool {
        self.notify.is_accepting()
    }

    pub fn in_flight(&self) -> Result<usize, NotifyError> {
        self.notify.in_flight()
    }

    pub fn post_continuation(&self, identity: RequestIdentity) -> Result<(), RuntimeError> {
        self.notify.post_continuation(identity).map_err(Into::into)
    }

    pub fn handle(&self) -> Result<Handle, RuntimeError> {
        self.runtime
            .as_ref()
            .map(|runtime| runtime.handle().clone())
            .ok_or(NotifyError::ShutDown.into())
    }

    /// Invalidate `identity` so stale wakes are ignored until forget.
    pub fn invalidate(&self, identity: RequestIdentity) {
        let _ = self.notify.invalidate(identity);
    }

    pub fn forget_invalidated(&self, identity: RequestIdentity) {
        let _ = self.notify.forget_invalidated(identity);
    }

    /// Worker-main-thread access setup: bind one request-local VM and bridge.
    pub fn bind_access_exec(
        &mut self,
        program: Arc<Program>,
        request_id: u64,
    ) -> Result<AccessExec, String> {
        self.bind_phase_exec(program, request_id, crate::host::NgxPhase::Access)
    }

    pub fn bind_phase_exec(
        &mut self,
        program: Arc<Program>,
        request_id: u64,
        phase: crate::host::NgxPhase,
    ) -> Result<AccessExec, String> {
        self.bind_phase_exec_with_policy(
            program,
            request_id,
            phase,
            crate::host::StandardHostPolicy::fail_closed(),
        )
    }

    pub fn bind_phase_exec_with_policy(
        &mut self,
        program: Arc<Program>,
        request_id: u64,
        phase: crate::host::NgxPhase,
        policy: crate::host::StandardHostPolicy,
    ) -> Result<AccessExec, String> {
        if self.stopped || !self.notify.is_accepting() {
            return Err(NotifyError::ShutDown.to_string());
        }
        let generation = self.next_generation;
        self.next_generation = self.next_generation.wrapping_add(1);
        if self.next_generation == 0 {
            self.next_generation = 1;
        }
        let handle = self.handle().map_err(|err| err.to_string())?;
        AccessExec::bind_program_in_phase_with_policy(
            program, request_id, generation, handle, phase, policy,
        )
    }

    pub(crate) fn bind_reusable_phase_exec_with_policy(
        &mut self,
        program: Arc<Program>,
        request_id: u64,
        phase: crate::host::NgxPhase,
        policy: crate::host::StandardHostPolicy,
    ) -> Result<AccessExec, String> {
        if self.stopped || !self.notify.is_accepting() {
            return Err(NotifyError::ShutDown.to_string());
        }
        let generation = self.next_generation;
        self.next_generation = self.next_generation.wrapping_add(1);
        if self.next_generation == 0 {
            self.next_generation = 1;
        }
        let key = ReusableVmKey {
            program,
            phase,
            policy,
            ngx_catalog_abi: request_vm_ngx_catalog_abi(),
            standard_catalog_abi: vm::standard_host_catalog_fingerprint(),
        };
        let handle = self.handle().map_err(|error| error.to_string())?;
        let mut vm = self.reusable_request_vms.borrow_mut().take(&key)?;
        vm.set_async_bridge(Box::new(crate::async_bridge::NgxAsyncBridge::new(handle)))
            .map_err(|error| error.to_string())?;
        vm.set_runtime_print_sink(|_| {});
        Ok(AccessExec::bind_reusable_vm(
            vm, key, request_id, generation,
        ))
    }

    #[cfg(test)]
    fn reusable_request_vm_created_count(&self) -> usize {
        self.reusable_request_vms.borrow().created
    }

    #[cfg(test)]
    fn reusable_request_vm_idle_count(&self) -> usize {
        self.reusable_request_vms.borrow().idle.len()
    }

    pub(crate) fn bind_configured_phase_exec_with_policy(
        &mut self,
        program: Arc<Program>,
        request_id: u64,
        phase: crate::host::NgxPhase,
        policy: crate::host::StandardHostPolicy,
        metadata: crate::phase::ScriptExecutionMetadata,
    ) -> Result<AccessExec, String> {
        if metadata.reuses_request_vm {
            self.bind_reusable_phase_exec_with_policy(program, request_id, phase, policy)
        } else {
            self.bind_phase_exec_with_policy(program, request_id, phase, policy)
        }
    }

    pub fn drain_ready(&mut self) -> Result<Vec<RequestIdentity>, RuntimeError> {
        let (ready, err) = self.drain_ready_outcome();
        match err {
            Some(err) if ready.is_empty() => Err(err),
            _ => Ok(ready),
        }
    }

    pub(crate) fn drain_ready_outcome(&mut self) -> (Vec<RequestIdentity>, Option<RuntimeError>) {
        let (ready, err) = self.notify.harvest_take();
        (ready, err.map(RuntimeError::from))
    }

    /// Reject new work, invalidate senders, drop the runtime (cancels timers
    /// and in-flight futures), then close eventfd with the existing fd gate.
    ///
    /// Live semaphores are closed and their waiters woken **before** the
    /// runtime is dropped so a parked wait never resumes silently. Pending
    /// timer callbacks run once with `premature=true`, live callbacks are
    /// cancelled, and every callback VM is released in the same step.
    pub fn shutdown(&mut self) {
        if self.stopped {
            return;
        }
        self.stopped = true;
        self.reusable_request_vms.get_mut().idle.clear();
        crate::socket::pool::shutdown_current();
        crate::semaphore::shutdown_current();
        crate::timer::shutdown_current();
        self.notify.reject_new();
        let _ = self.notify.reclaim_all();
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_timeout(Duration::from_secs(5));
        }
        self.notify.shutdown_fd();
    }

    unsafe fn register_nginx(&mut self, cycle: *mut ngx_cycle_t) -> Result<(), NotifyError> {
        unsafe {
            self.notify
                .register_nginx(cycle, Some(eventfd_read_handler))
        }
    }
}

impl AsyncWait for WorkerRuntime {
    fn reserve(&self) -> Result<CompletionPermit, SubmitError> {
        if self.stopped || !self.notify.is_accepting() {
            return Err(SubmitError::Closed);
        }
        match self.notify.try_reserve() {
            Ok(permit) => Ok(permit),
            Err(NotifyError::QueueFull) => Err(SubmitError::Full),
            Err(_) => Err(SubmitError::Closed),
        }
    }

    fn waker(&self, identity: RequestIdentity) -> Waker {
        eventfd_waker(self.sender(), identity)
    }

    fn post_continuation(&self, identity: RequestIdentity) -> Result<(), SubmitError> {
        WorkerRuntime::post_continuation(self, identity).map_err(|_| SubmitError::Closed)
    }

    fn invalidate(&self, identity: RequestIdentity) {
        WorkerRuntime::invalidate(self, identity);
    }

    fn forget_invalidated(&self, identity: RequestIdentity) {
        WorkerRuntime::forget_invalidated(self, identity);
    }

    fn recycle_finished_exec(&self, exec: &mut AccessExec) -> bool {
        let Some((key, vm)) = exec.take_reusable_vm() else {
            return false;
        };
        let Ok(handle) = self.handle() else {
            return true;
        };
        self.reusable_request_vms
            .borrow_mut()
            .recycle(key, vm, &handle);
        true
    }
}

impl Drop for WorkerRuntime {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// NGINX worker `init_process` entry: create runtime + eventfd + register read.
///
/// # Safety
///
/// `cycle` must be the live worker cycle NGINX passes to `init_process` after
/// fork.
pub unsafe fn init_worker(cycle: *mut ngx_cycle_t) -> Result<(), RuntimeError> {
    if !is_worker_or_single() {
        return Ok(());
    }
    if worker_is_active() {
        return Err(RuntimeError::AlreadyStarted);
    }
    // Build the restricted catalog registry before the worker accepts
    // connections. First `bind_vm_cached` is cheap once the process-wide
    // table exists; constructing it on the first request would block the
    // event loop for the entire install.
    crate::host::ngx_host_registry().map_err(RuntimeError::HostRegistry)?;
    let mut worker = WorkerRuntime::start()?;
    unsafe { worker.register_nginx(cycle)? };
    if !cycle.is_null() {
        let log = unsafe { (*cycle).log };
        if !log.is_null() {
            ngx_log_error!(NGX_LOG_NOTICE, log, "ngx-rustscript: worker runtime ready");
        }
    }
    WORKER.with(|slot| {
        *slot.borrow_mut() = Some(worker);
    });
    Ok(())
}

/// NGINX worker `exit_process` entry. Idempotent.
pub fn exit_worker() {
    WORKER.with(|slot| {
        if let Some(mut worker) = slot.borrow_mut().take() {
            worker.shutdown();
        }
    });
    crate::access::clear_worker_access();
}

pub fn worker_is_active() -> bool {
    WORKER.with(|slot| slot.borrow().is_some())
}

pub fn with_worker<R>(f: impl FnOnce(&mut WorkerRuntime) -> R) -> Option<R> {
    WORKER.with(|slot| slot.borrow_mut().as_mut().map(f))
}

unsafe extern "C" fn eventfd_read_handler(ev: *mut ngx_event_t) {
    let log = if ev.is_null() {
        std::ptr::null_mut()
    } else {
        unsafe { (*ev).log }
    };
    let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
        let (ready, err) = WORKER.with(|slot| match slot.borrow_mut().as_mut() {
            Some(worker) => worker.drain_ready_outcome(),
            None => (Vec::new(), None),
        });
        // Timer identities dispatch on the timer registry first; request
        // identities are unknown to it and route to the request path unchanged.
        crate::timer::on_worker_event(&ready);
        crate::access::on_async_ready(ready);
        match err {
            Some(err) => Err(err),
            None => Ok(()),
        }
    }));
    match result {
        Ok(Ok(())) => {}
        Ok(Err(err)) => {
            if !log.is_null() {
                ngx_log_error!(NGX_LOG_ERR, log, "ngx-rustscript eventfd: {err}");
            }
        }
        Err(_) => {
            if !log.is_null() {
                ngx_log_error!(NGX_LOG_ERR, log, "ngx-rustscript eventfd callback panicked");
            }
        }
    }
}

/// NGINX `ngx_module_t.init_process` hook.
///
/// # Safety
///
/// Called by NGINX on the worker main thread with a valid `cycle`.
pub unsafe extern "C" fn init_process(cycle: *mut ngx_cycle_t) -> ngx_int_t {
    let result = std::panic::catch_unwind(AssertUnwindSafe(|| unsafe { init_worker(cycle) }));
    match result {
        Ok(Ok(())) => Status::NGX_OK.into(),
        Ok(Err(err)) => {
            if !cycle.is_null() {
                let log = unsafe { (*cycle).log };
                if !log.is_null() {
                    ngx_log_error!(
                        ngx::ffi::NGX_LOG_EMERG,
                        log,
                        "ngx-rustscript worker runtime: {err}"
                    );
                }
            }
            Status::NGX_ERROR.into()
        }
        Err(_) => Status::NGX_ERROR.into(),
    }
}

/// NGINX `ngx_module_t.exit_process` hook. Idempotent.
///
/// # Safety
///
/// Called by NGINX on the worker main thread. `cycle` may be unused.
pub unsafe extern "C" fn exit_process(_cycle: *mut ngx_cycle_t) {
    let _ = std::panic::catch_unwind(AssertUnwindSafe(exit_worker));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::compile_rss_source;
    use crate::notify::RequestIdentity;
    use crate::request_state::{AccessExec, AccessStatus, IdentityRegistry, ReadyEffect};
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::os::unix::io::RawFd;
    use std::time::{Duration, Instant};
    use vm::VmStatus;

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

    fn sleep_once(ms: i64) -> Arc<Program> {
        compile_rss_source(&format!("use ngx;\nngx::sleep_ms({ms});\n")).expect("compile")
    }

    fn run_async_exec_to_halt(worker: &mut WorkerRuntime, exec: &mut AccessExec) -> AccessStatus {
        let mut registry = IdentityRegistry::with_capacity(8);
        let initial = exec.handle_access(&mut registry, worker);
        if initial != AccessStatus::Again {
            return initial;
        }
        exec.commit_suspend();
        loop {
            let mut ready = worker.drain_ready().expect("drain ready");
            if ready.is_empty() {
                let fd = worker.raw_fd().expect("eventfd");
                assert!(wait_readable(fd, Duration::from_secs(3)));
                ready = worker.drain_ready().expect("drain ready after wake");
            }
            assert_eq!(ready, vec![exec.identity()]);
            match exec.apply_ready(exec.identity(), &mut registry, worker) {
                ReadyEffect::Halted => return exec.handle_access(&mut registry, worker),
                ReadyEffect::WaitingAgain | ReadyEffect::Ignored => {}
                effect => panic!("async request failed with {effect:?}"),
            }
        }
    }

    fn start_http_server(requests: usize) -> (u16, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind HTTP fixture");
        let port = listener.local_addr().expect("fixture address").port();
        let join = std::thread::spawn(move || {
            for _ in 0..requests {
                let (mut stream, _) = listener.accept().expect("accept HTTP request");
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .expect("read timeout");
                let mut request = [0u8; 4096];
                let read = stream.read(&mut request).expect("read HTTP request");
                assert!(
                    request[..read]
                        .windows(b"GET / HTTP/1.1".len())
                        .any(|window| window == b"GET / HTTP/1.1")
                );
                std::thread::sleep(Duration::from_millis(20));
                stream
                    .write_all(
                        b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    )
                    .expect("write HTTP response");
            }
        });
        (port, join)
    }

    #[test]
    fn waker_payload_contains_no_nginx_data() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<CompletionSender>();
        assert_send_sync::<RequestIdentity>();
        for name in [
            std::any::type_name::<CompletionSender>(),
            std::any::type_name::<RequestIdentity>(),
            std::any::type_name::<crate::notify::NgxWake>(),
        ] {
            assert!(
                !name.contains("ngx_connection")
                    && !name.contains("ngx_http")
                    && !name.contains("ngx_pool")
                    && !name.contains("ngx_cycle")
                    && !name.contains("*mut"),
                "waker payload must not mention NGINX/VM data: {name}"
            );
        }
    }

    #[test]
    fn waiting_sleep_wakes_eventfd_and_resumes() {
        let mut worker = WorkerRuntime::start().unwrap();
        let mut exec = worker.bind_access_exec(sleep_once(5), 1).unwrap();
        let mut registry = IdentityRegistry::with_capacity(8);
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
        let effect = exec.apply_ready(exec.identity(), &mut registry, &worker);
        assert_eq!(effect, crate::request_state::ReadyEffect::Halted);
        assert_eq!(exec.handle_access(&mut registry, &worker), AccessStatus::Ok);
        worker.shutdown();
    }

    #[test]
    fn posted_continuation_queues_without_consuming_wait_capacity() {
        let mut worker = WorkerRuntime::start_with_capacity(1).unwrap();
        let identity = RequestIdentity::new(7, 11);
        worker
            .post_continuation(identity)
            .expect("post continuation");
        assert_eq!(worker.in_flight().unwrap(), 0);
        assert_eq!(worker.drain_ready().unwrap(), vec![identity]);
        assert_eq!(worker.in_flight().unwrap(), 0);
        worker.shutdown();
    }

    #[test]
    fn submit_after_shutdown_is_rejected() {
        let mut worker = WorkerRuntime::start().unwrap();
        worker.shutdown();
        assert_eq!(worker.reserve().map(|_| ()), Err(SubmitError::Closed));
        worker.shutdown();
    }

    #[test]
    fn shutdown_does_not_hang_on_long_sleep() {
        let mut worker = WorkerRuntime::start().unwrap();
        let mut exec = worker.bind_access_exec(sleep_once(60_000), 1).unwrap();
        let mut registry = IdentityRegistry::with_capacity(8);
        assert_eq!(
            exec.handle_access(&mut registry, &worker),
            AccessStatus::Again
        );
        let start = Instant::now();
        exec.cleanup_wait(&worker);
        worker.shutdown();
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "runtime shutdown must not wait out the sleep"
        );
        assert!(worker.raw_fd().is_none());
        assert!(!worker.is_accepting());
        assert_eq!(worker.in_flight().unwrap(), 0);
    }

    #[test]
    fn shutdown_releases_unfinished_reservation() {
        let mut worker = WorkerRuntime::start_with_capacity(1).unwrap();
        let mut exec = worker.bind_access_exec(sleep_once(60_000), 1).unwrap();
        let mut registry = IdentityRegistry::with_capacity(1);
        assert_eq!(
            exec.handle_access(&mut registry, &worker),
            AccessStatus::Again
        );
        assert_eq!(worker.in_flight().unwrap(), 1);
        exec.cleanup_wait(&worker);
        worker.shutdown();
        assert_eq!(worker.in_flight().unwrap(), 0);
    }

    #[test]
    fn shutdown_is_idempotent() {
        let mut worker = WorkerRuntime::start().unwrap();
        worker.shutdown();
        worker.shutdown();
        worker.shutdown();
        assert!(!worker_is_active());
    }

    #[test]
    fn compile_time_runtime_enables_time_and_io() {
        let _rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_time()
            .enable_io()
            .build()
            .unwrap();
    }

    #[test]
    fn exit_worker_without_init_is_idempotent() {
        exit_worker();
        exit_worker();
        exit_worker();
        assert!(!worker_is_active());
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
    fn sync_request_owned_exec_reuses_bound_vm_and_clears_ctx() {
        let mut worker = WorkerRuntime::start().unwrap();
        let program = compile_rss_source(
            r#"
use ngx;
if ngx::ctx::get("seen") != null {
    1 / 0;
}
ngx::ctx::set("seen", 1);
ngx::exit(ngx::core::ok());
"#,
        )
        .expect("compile");
        let policy = crate::host::StandardHostPolicy::fail_closed();

        for request_id in 1..=32 {
            let mut exec = worker
                .bind_reusable_phase_exec_with_policy(
                    Arc::clone(&program),
                    request_id,
                    crate::host::NgxPhase::Access,
                    policy.clone(),
                )
                .expect("bind reusable request VM");
            let mut registry = IdentityRegistry::with_capacity(8);
            assert_eq!(
                exec.handle_access(&mut registry, &worker),
                AccessStatus::Ok,
                "request-local ctx must be empty after every pool checkout"
            );
            assert!(exec.cleanup_wait(&worker).is_none());
        }

        assert_eq!(worker.reusable_request_vm_created_count(), 1);
        assert_eq!(worker.reusable_request_vm_idle_count(), 1);
        worker.shutdown();
    }

    #[test]
    fn buffered_http_vm_reuses_after_quiescence_with_fresh_context_and_exact_policy() {
        let (port, server) = start_http_server(3);
        let mut worker = WorkerRuntime::start().unwrap();
        let program = compile_rss_source(&format!(
            r#"
use http;
use ngx;
if ngx::ctx::get("seen") != null {{ 1 / 0; }}
ngx::ctx::set("seen", 1);
let response: map = http::client::request({{
    method: "GET",
    url: "http://127.0.0.1:{port}/",
    body: null
}});
if response["status"] != 204 {{ 1 / 0; }}
"#
        ))
        .expect("compile buffered HTTP fixture");
        assert!(crate::config::program_can_reuse_request_vm(&program));
        let mut policy = crate::host::StandardHostPolicy::fail_closed();
        policy.http_hosts.push("127.0.0.1".into());
        policy.http_schemes.push("http".into());
        policy.http_ports.push(port);
        policy.http_allow_private = true;

        let mut identities = Vec::new();
        for request_id in 1..=2 {
            let mut exec = worker
                .bind_reusable_phase_exec_with_policy(
                    Arc::clone(&program),
                    request_id,
                    crate::host::NgxPhase::Access,
                    policy.clone(),
                )
                .expect("bind buffered HTTP VM");
            identities.push(exec.identity());
            assert_eq!(
                run_async_exec_to_halt(&mut worker, &mut exec),
                AccessStatus::Ok
            );
            assert!(exec.cleanup_wait(&worker).is_none());
        }
        assert_ne!(identities[0], identities[1]);
        assert_eq!(worker.reusable_request_vm_created_count(), 1);

        let mut isolated_policy = policy;
        isolated_policy.http_hosts.push("localhost".into());
        let mut isolated = worker
            .bind_reusable_phase_exec_with_policy(
                Arc::clone(&program),
                3,
                crate::host::NgxPhase::Access,
                isolated_policy,
            )
            .expect("bind policy-isolated buffered HTTP VM");
        assert_eq!(
            run_async_exec_to_halt(&mut worker, &mut isolated),
            AccessStatus::Ok
        );
        assert!(isolated.cleanup_wait(&worker).is_none());
        assert_eq!(worker.reusable_request_vm_created_count(), 2);
        assert_eq!(worker.reusable_request_vm_idle_count(), 2);

        server.join().expect("HTTP fixture server");
        worker.shutdown();
    }

    #[test]
    fn identical_source_in_distinct_programs_does_not_share_request_vms() {
        let mut worker = WorkerRuntime::start().unwrap();
        let source = "use ngx;\nngx::ctx::set(\"k\", 1);\n";
        let first = compile_rss_source(source).expect("first program");
        let second = compile_rss_source(source).expect("second program");
        for (request_id, program) in [(1, first), (2, second)] {
            let mut exec = worker
                .bind_reusable_phase_exec_with_policy(
                    program,
                    request_id,
                    crate::host::NgxPhase::Access,
                    crate::host::StandardHostPolicy::fail_closed(),
                )
                .expect("bind distinct program");
            let mut registry = IdentityRegistry::with_capacity(8);
            assert_eq!(exec.handle_access(&mut registry, &worker), AccessStatus::Ok);
            assert!(exec.cleanup_wait(&worker).is_none());
        }
        assert_eq!(worker.reusable_request_vm_created_count(), 2);
        worker.shutdown();
    }

    #[test]
    fn configured_exec_selects_reusable_pool_from_cached_metadata() {
        let mut worker = WorkerRuntime::start().unwrap();
        let program = compile_rss_source("use ngx;\nngx::ctx::set(\"k\", 1);\n").expect("compile");
        let metadata = crate::phase::ScriptExecutionMetadata {
            requires_request_state: true,
            reuses_request_vm: true,
            uses_sqlite: false,
        };
        for request_id in 1..=2 {
            let mut exec = worker
                .bind_configured_phase_exec_with_policy(
                    Arc::clone(&program),
                    request_id,
                    crate::host::NgxPhase::Access,
                    crate::host::StandardHostPolicy::fail_closed(),
                    metadata,
                )
                .expect("bind configured exec");
            let mut registry = IdentityRegistry::with_capacity(8);
            assert_eq!(exec.handle_access(&mut registry, &worker), AccessStatus::Ok);
            assert!(exec.cleanup_wait(&worker).is_none());
        }
        assert_eq!(worker.reusable_request_vm_created_count(), 1);
        worker.shutdown();
    }

    #[test]
    fn reusable_request_vm_pool_separates_policy_and_phase() {
        let mut worker = WorkerRuntime::start().unwrap();
        let program = compile_rss_source("use ngx;\nngx::ctx::set(\"k\", 1);\n").expect("compile");
        let first_policy = crate::host::StandardHostPolicy::fail_closed();
        let mut second_policy = first_policy.clone();
        second_policy.io_roots.push("/tmp/isolated".into());

        for (request_id, phase, policy) in [
            (1, crate::host::NgxPhase::Access, first_policy.clone()),
            (2, crate::host::NgxPhase::Access, second_policy),
            (3, crate::host::NgxPhase::Content, first_policy),
        ] {
            let mut exec = worker
                .bind_reusable_phase_exec_with_policy(
                    Arc::clone(&program),
                    request_id,
                    phase,
                    policy,
                )
                .expect("bind reusable exec");
            let mut registry = IdentityRegistry::with_capacity(8);
            assert_eq!(exec.handle_access(&mut registry, &worker), AccessStatus::Ok);
            assert!(exec.cleanup_wait(&worker).is_none());
        }

        assert_eq!(worker.reusable_request_vm_created_count(), 3);
        assert_eq!(worker.reusable_request_vm_idle_count(), 3);
        worker.shutdown();
    }

    #[test]
    fn configured_exec_keeps_non_reusable_metadata_out_of_request_vm_pool() {
        let mut worker = WorkerRuntime::start().unwrap();
        let program = compile_rss_source("use ngx;\nngx::ctx::set(\"k\", 1);\n").expect("compile");
        let metadata = crate::phase::ScriptExecutionMetadata {
            requires_request_state: true,
            reuses_request_vm: false,
            uses_sqlite: false,
        };
        for request_id in 1..=2 {
            let mut exec = worker
                .bind_configured_phase_exec_with_policy(
                    Arc::clone(&program),
                    request_id,
                    crate::host::NgxPhase::Access,
                    crate::host::StandardHostPolicy::fail_closed(),
                    metadata,
                )
                .expect("bind configured exec");
            let mut registry = IdentityRegistry::with_capacity(8);
            assert_eq!(exec.handle_access(&mut registry, &worker), AccessStatus::Ok);
            assert!(exec.cleanup_wait(&worker).is_none());
        }

        assert_eq!(worker.reusable_request_vm_created_count(), 0);
        assert_eq!(worker.reusable_request_vm_idle_count(), 0);
        worker.shutdown();
    }

    #[test]
    fn reusable_request_vm_replaces_and_clears_async_bridge_per_checkout() {
        crate::async_bridge::reset_bridge_lifecycle_counts();
        let mut worker = WorkerRuntime::start().unwrap();
        let program = compile_rss_source("use ngx;\nngx::ctx::set(\"k\", 1);\n").expect("compile");

        for request_id in 1..=2 {
            let mut exec = worker
                .bind_reusable_phase_exec_with_policy(
                    Arc::clone(&program),
                    request_id,
                    crate::host::NgxPhase::Access,
                    crate::host::StandardHostPolicy::fail_closed(),
                )
                .expect("bind reusable exec");
            let mut registry = IdentityRegistry::with_capacity(8);
            assert_eq!(exec.handle_access(&mut registry, &worker), AccessStatus::Ok);
            assert!(exec.cleanup_wait(&worker).is_none());
        }

        assert_eq!(crate::async_bridge::bridge_lifecycle_counts(), (2, 2));
        worker.shutdown();
    }

    #[test]
    fn failed_request_vm_is_dropped_instead_of_reused() {
        let mut worker = WorkerRuntime::start().unwrap();
        let program = compile_rss_source("use ngx;\nngx::ctx::set(\"k\", 1);\nlet x = 1 / 0;\n")
            .expect("compile");

        for request_id in 1..=2 {
            let mut exec = worker
                .bind_reusable_phase_exec_with_policy(
                    Arc::clone(&program),
                    request_id,
                    crate::host::NgxPhase::Access,
                    crate::host::StandardHostPolicy::fail_closed(),
                )
                .expect("bind reusable exec");
            let mut registry = IdentityRegistry::with_capacity(8);
            assert_eq!(
                exec.handle_access(&mut registry, &worker),
                AccessStatus::Error500
            );
            assert!(exec.cleanup_wait(&worker).is_none());
        }

        assert_eq!(worker.reusable_request_vm_created_count(), 2);
        assert_eq!(worker.reusable_request_vm_idle_count(), 0);
        worker.shutdown();
    }

    #[test]
    fn bounded_capacity_rejects_then_reclaims() {
        let mut worker = WorkerRuntime::start_with_capacity(1).unwrap();
        let program = sleep_once(60_000);
        let mut a = worker.bind_access_exec(Arc::clone(&program), 1).expect("a");
        let mut b = worker.bind_access_exec(program, 2).expect("b");
        let mut registry = IdentityRegistry::with_capacity(8);
        assert_eq!(a.handle_access(&mut registry, &worker), AccessStatus::Again);
        assert_eq!(
            b.handle_access(&mut registry, &worker),
            AccessStatus::Error500
        );
        a.cleanup_wait(&worker);
        assert_eq!(worker.in_flight().unwrap(), 0);
        worker.shutdown();
    }

    #[test]
    fn many_concurrent_sleeps_complete_without_loss() {
        let mut worker = WorkerRuntime::start_with_capacity(16).unwrap();
        let program = sleep_once(5);
        let mut registry = IdentityRegistry::with_capacity(16);
        let mut execs = Vec::new();
        for id in 1..=8 {
            let mut exec = worker
                .bind_access_exec(Arc::clone(&program), id)
                .expect("bind");
            assert_eq!(
                exec.handle_access(&mut registry, &worker),
                AccessStatus::Again
            );
            exec.commit_suspend();
            execs.push(exec);
        }
        let fd = worker.raw_fd().expect("eventfd");
        let start = Instant::now();
        let mut ready = Vec::new();
        while ready.len() < 8 && start.elapsed() < Duration::from_secs(2) {
            let _ = wait_readable(fd, Duration::from_millis(50));
            ready.extend(worker.drain_ready().unwrap());
        }
        ready.sort_by_key(|identity| identity.request_id());
        let expected: Vec<_> = execs.iter().map(|exec| exec.identity()).collect();
        let mut expected = expected;
        expected.sort_by_key(|identity| identity.request_id());
        assert_eq!(ready, expected, "every concurrent sleep must drain");
        worker.shutdown();
    }
}
