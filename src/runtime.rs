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
use vm::Program;

use crate::notify::{
    CompletionPermit, CompletionSender, DEFAULT_MAX_IN_FLIGHT, NotifyBridge, NotifyError,
    RequestIdentity, eventfd_waker,
};
use crate::request_state::{AccessExec, AsyncWait, SubmitError};

thread_local! {
    static WORKER: RefCell<Option<WorkerRuntime>> = const { RefCell::new(None) };
}

/// Fail-closed worker runtime errors.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RuntimeError {
    Notify(NotifyError),
    RuntimeBuild(String),
    AlreadyStarted,
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Notify(err) => write!(f, "{err}"),
            Self::RuntimeBuild(msg) => write!(f, "tokio runtime build failed: {msg}"),
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

/// Per-worker Tokio runtime, eventfd bridge, and generation counter.
pub struct WorkerRuntime {
    runtime: Option<Runtime>,
    notify: NotifyBridge,
    next_generation: u64,
    stopped: bool,
}

impl WorkerRuntime {
    pub fn start() -> Result<Self, RuntimeError> {
        Self::start_with_capacity(DEFAULT_MAX_IN_FLIGHT)
    }

    pub fn start_with_capacity(cap: usize) -> Result<Self, RuntimeError> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_time()
            .thread_name("ngx-rustscript-async")
            .build()
            .map_err(|err| RuntimeError::RuntimeBuild(err.to_string()))?;
        Ok(Self {
            runtime: Some(runtime),
            notify: NotifyBridge::with_capacity(cap)?,
            next_generation: 1,
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
        if self.stopped || !self.notify.is_accepting() {
            return Err(NotifyError::ShutDown.to_string());
        }
        let generation = self.next_generation;
        self.next_generation = self.next_generation.wrapping_add(1);
        if self.next_generation == 0 {
            self.next_generation = 1;
        }
        let handle = self.handle().map_err(|err| err.to_string())?;
        AccessExec::bind_program(program, request_id, generation, handle)
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
    pub fn shutdown(&mut self) {
        if self.stopped {
            return;
        }
        self.stopped = true;
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

    fn invalidate(&self, identity: RequestIdentity) {
        WorkerRuntime::invalidate(self, identity);
    }

    fn forget_invalidated(&self, identity: RequestIdentity) {
        WorkerRuntime::forget_invalidated(self, identity);
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
    use crate::request_state::{AccessStatus, IdentityRegistry};
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
    fn compile_time_runtime_has_only_time_not_net() {
        let _rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_time()
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
