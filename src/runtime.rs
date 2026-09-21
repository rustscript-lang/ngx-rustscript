//! Worker-local Tokio runtime for polling generic host futures.
//!
//! Created in NGINX `init_process` after fork, only in `NGX_PROCESS_WORKER`
//! or `NGX_PROCESS_SINGLE`. Helper, master, config, and `nginx -t` never
//! create Tokio threads or the eventfd. Wakers own only a
//! [`crate::notify::CompletionSender`] and a
//! [`crate::notify::RequestIdentity`]. They never hold NGINX request/pool/VM
//! pointers and never call `ngx_post_event`.

#[cfg(any(test, ngx_rustscript_integration_hooks))]
use std::cell::Cell;
use std::cell::RefCell;
#[cfg(any(test, ngx_rustscript_integration_hooks))]
use std::collections::HashSet;
use std::fmt;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::pin::pin;
use std::sync::Arc;
use std::task::{Context, Waker};
use std::time::Duration;

use nginx_sys::{ngx_cycle_t, ngx_event_t, ngx_int_t, ngx_uint_t};
use ngx::core::Status;
use ngx::ffi::{NGX_LOG_ERR, NGX_LOG_NOTICE, NGX_PROCESS_SINGLE, NGX_PROCESS_WORKER, ngx_process};
use ngx::ngx_log_error;
use tokio::runtime::{Handle, Runtime};
#[cfg(any(test, ngx_rustscript_integration_hooks))]
use vm::HttpConfig;
use vm::{HttpClientLease, HttpWorkerResources, HttpWorkerShutdown, Program, Vm};

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

/// Result of polling the upstream worker-resource shutdown future.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HttpShutdownPoll {
    /// No worker-owned HTTP resource remained at the time of shutdown.
    NotOwned,
    /// The current upstream contract completed resource cleanup immediately.
    Ready,
    /// Admission was closed, but the upstream future did not complete in the
    /// explicit shutdown poll. The result is returned and logged; it is never
    /// silently discarded.
    Pending,
}

/// Explicit worker shutdown result. Drop intentionally has no result because
/// it is only a non-blocking fallback.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorkerShutdownOutcome {
    pub http: HttpShutdownPoll,
    pub already_stopped: bool,
}

impl WorkerShutdownOutcome {
    fn already_stopped() -> Self {
        Self {
            http: HttpShutdownPoll::NotOwned,
            already_stopped: true,
        }
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
    #[cfg(ngx_rustscript_integration_hooks)]
    checkouts: usize,
    #[cfg(ngx_rustscript_integration_hooks)]
    hits: usize,
    #[cfg(ngx_rustscript_integration_hooks)]
    returns: usize,
}

#[cfg(ngx_rustscript_integration_hooks)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ReusableRequestVmPoolStats {
    pub(crate) created: usize,
    pub(crate) checkouts: usize,
    pub(crate) hits: usize,
    pub(crate) returns: usize,
    pub(crate) idle: usize,
}

impl ReusableRequestVmPool {
    fn take(&mut self, key: &ReusableVmKey, http_client: HttpClientLease) -> Result<Vm, String> {
        #[cfg(ngx_rustscript_integration_hooks)]
        {
            self.checkouts = self.checkouts.saturating_add(1);
        }
        if let Some(index) = self.idle.iter().position(|entry| entry.key.matches(key)) {
            #[cfg(ngx_rustscript_integration_hooks)]
            {
                self.hits = self.hits.saturating_add(1);
            }
            return Ok(self.idle.swap_remove(index).vm);
        }
        let mut vm = Vm::new_shared(Arc::clone(&key.program));
        crate::request_state::bind_program_hosts(&mut vm, &key.policy, http_client)?;
        self.created = self.created.saturating_add(1);
        Ok(vm)
    }

    fn recycle(&mut self, key: ReusableVmKey, mut vm: Vm, runtime: &Handle) {
        if self.idle.len() >= MAX_IDLE_REUSABLE_REQUEST_VMS {
            return;
        }
        if crate::vm_pool::reset_request_vm_for_pool(&mut vm, runtime) {
            self.idle.push(ReusableRequestVm { key, vm });
            #[cfg(ngx_rustscript_integration_hooks)]
            {
                self.returns = self.returns.saturating_add(1);
            }
        }
    }

    #[cfg(ngx_rustscript_integration_hooks)]
    fn stats(&self) -> ReusableRequestVmPoolStats {
        ReusableRequestVmPoolStats {
            created: self.created,
            checkouts: self.checkouts,
            hits: self.hits,
            returns: self.returns,
            idle: self.idle.len(),
        }
    }
}

/// Per-worker Tokio runtime, eventfd bridge, generation counter, and HTTP
/// client resource owner.
pub struct WorkerRuntime {
    runtime: Option<Runtime>,
    notify: NotifyBridge,
    next_generation: u64,
    reusable_request_vms: RefCell<ReusableRequestVmPool>,
    http: RefCell<Option<HttpWorkerResources>>,
    #[cfg(any(test, ngx_rustscript_integration_hooks))]
    http_configs: RefCell<HashSet<HttpConfig>>,
    #[cfg(any(test, ngx_rustscript_integration_hooks))]
    http_client_builds: Cell<usize>,
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
            // pd-vm's default 64 is a per-VM limit. The worker resource uses an
            // explicit effectively-unbounded cap so sharing leases does not
            // introduce a second aggregate ceiling; each VM reinstates the
            // historical local limit through HttpHostExt.
            http: RefCell::new(Some(HttpWorkerResources::with_max_in_flight(usize::MAX))),
            #[cfg(any(test, ngx_rustscript_integration_hooks))]
            http_configs: RefCell::new(HashSet::new()),
            #[cfg(any(test, ngx_rustscript_integration_hooks))]
            http_client_builds: Cell::new(0),
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

    #[cfg(ngx_rustscript_integration_hooks)]
    pub(crate) fn reusable_request_vm_pool_stats(&self) -> ReusableRequestVmPoolStats {
        self.reusable_request_vms.borrow().stats()
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
        let http_client = self.http_lease_for(&policy)?;
        AccessExec::bind_program_in_phase_with_policy(
            program,
            request_id,
            generation,
            handle,
            phase,
            policy,
            http_client,
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
        let http_client = self.http_lease_for(&key.policy)?;
        let mut vm = self
            .reusable_request_vms
            .borrow_mut()
            .take(&key, http_client)?;
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

    /// Obtain a worker-owned HTTP client lease for `policy`'s full config.
    ///
    /// One [`HttpWorkerResources`] owner lives for the worker runtime. Same
    /// full config reuses the same Hyper client; VM create/reset/eviction only
    /// clones this lease.
    pub fn http_lease_for(
        &self,
        policy: &crate::host::StandardHostPolicy,
    ) -> Result<HttpClientLease, String> {
        if self.stopped || !self.notify.is_accepting() {
            return Err(NotifyError::ShutDown.to_string());
        }
        let config = crate::host::http_config_from_policy(policy);
        let lease = {
            let mut http = self.http.borrow_mut();
            let resources = http
                .as_mut()
                .ok_or_else(|| NotifyError::ShutDown.to_string())?;
            resources
                .client_for(&config)
                .map_err(|err| err.to_string())?
        };
        #[cfg(any(test, ngx_rustscript_integration_hooks))]
        if self.http_configs.borrow_mut().insert(config) {
            self.http_client_builds
                .set(self.http_client_builds.get().saturating_add(1));
        }
        Ok(lease)
    }

    #[cfg(test)]
    fn http_client_build_count(&self) -> usize {
        self.http_client_builds.get()
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

    /// Explicit worker shutdown. This is the NGINX exit-process path and may
    /// use the existing bounded Tokio shutdown deadline.
    pub fn shutdown(&mut self) -> WorkerShutdownOutcome {
        if self.stopped {
            return WorkerShutdownOutcome::already_stopped();
        }
        self.stopped = true;
        self.reusable_request_vms.get_mut().idle.clear();
        crate::socket::pool::shutdown_current();
        crate::semaphore::shutdown_current();
        crate::timer::shutdown_current();
        self.notify.reject_new();
        let _ = self.notify.reclaim_all();
        let http = if let Some(mut http) = self.http.get_mut().take() {
            http.close_admission();
            poll_http_worker_shutdown(http.into_shutdown())
        } else {
            HttpShutdownPoll::NotOwned
        };
        #[cfg(any(test, ngx_rustscript_integration_hooks))]
        self.http_configs.get_mut().clear();
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_timeout(Duration::from_secs(5));
        }
        self.notify.shutdown_fd();
        WorkerShutdownOutcome {
            http,
            already_stopped: false,
        }
    }

    /// Non-blocking Drop fallback. It closes admission and releases owned
    /// state, never polls a future, enters Tokio, waits, or starts a thread.
    fn shutdown_nonblocking(&mut self) {
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
        if let Some(mut http) = self.http.get_mut().take() {
            http.close_admission();
            drop(http);
        }
        #[cfg(any(test, ngx_rustscript_integration_hooks))]
        self.http_configs.get_mut().clear();
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
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

fn poll_http_worker_shutdown(shutdown: HttpWorkerShutdown) -> HttpShutdownPoll {
    let mut shutdown = pin!(shutdown);
    let waker = Waker::noop();
    let mut cx = Context::from_waker(waker);
    match shutdown.as_mut().poll(&mut cx) {
        std::task::Poll::Ready(()) => HttpShutdownPoll::Ready,
        std::task::Poll::Pending => HttpShutdownPoll::Pending,
    }
}

impl Drop for WorkerRuntime {
    fn drop(&mut self) {
        self.shutdown_nonblocking();
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
    if let Err(error) = unsafe { worker.register_nginx(cycle) } {
        let _ = worker.shutdown();
        return Err(error.into());
    }
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
pub fn exit_worker() -> WorkerShutdownOutcome {
    let outcome = WORKER.with(|slot| {
        if let Some(mut worker) = slot.borrow_mut().take() {
            worker.shutdown()
        } else {
            WorkerShutdownOutcome::already_stopped()
        }
    });
    crate::access::clear_worker_access();
    outcome
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
/// Called by NGINX on the worker main thread.
pub unsafe extern "C" fn exit_process(cycle: *mut ngx_cycle_t) {
    let result = std::panic::catch_unwind(AssertUnwindSafe(exit_worker));
    if let Ok(outcome) = result {
        if !cycle.is_null() {
            let log = unsafe { (*cycle).log };
            if !log.is_null() {
                match outcome.http {
                    HttpShutdownPoll::Pending => {
                        ngx_log_error!(
                            NGX_LOG_ERR,
                            log,
                            "ngx-rustscript: worker HTTP shutdown pending after admission close"
                        );
                    }
                    HttpShutdownPoll::Ready => {
                        ngx_log_error!(
                            NGX_LOG_NOTICE,
                            log,
                            "ngx-rustscript: worker HTTP shutdown ready"
                        );
                    }
                    HttpShutdownPoll::NotOwned => {
                        ngx_log_error!(
                            NGX_LOG_NOTICE,
                            log,
                            "ngx-rustscript: worker HTTP shutdown had no owned resource"
                        );
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::compile_rss_source;
    use crate::notify::RequestIdentity;
    use crate::request_state::{AccessExec, AccessStatus, IdentityRegistry, ReadyEffect};
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::os::unix::io::RawFd;
    use std::time::{Duration, Instant};
    use vm::{CompileSourceFileOptions, SourceFlavor, VmStatus};

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
                ReadyEffect::Halted | ReadyEffect::Failed => {
                    return exec.handle_access(&mut registry, worker);
                }
                ReadyEffect::WaitingAgain | ReadyEffect::Ignored => {}
                effect => panic!("async request failed with {effect:?}"),
            }
        }
    }

    fn try_accept_with_deadline(listener: &TcpListener, label: &str) -> Option<TcpStream> {
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            match listener.accept() {
                Ok((stream, _)) => {
                    stream
                        .set_nonblocking(false)
                        .expect("blocking accepted fixture stream");
                    return Some(stream);
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        return None;
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(error) => panic!("{label} accept: {error}"),
            }
        }
    }

    fn accept_with_deadline(listener: &TcpListener, label: &str) -> TcpStream {
        try_accept_with_deadline(listener, label)
            .unwrap_or_else(|| panic!("{label} accept timed out"))
    }

    fn read_http_headers(stream: &mut TcpStream, label: &str) -> Vec<u8> {
        let mut request = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            match stream.read(&mut byte) {
                Ok(0) => panic!("{label} request ended before headers"),
                Ok(_) => {
                    request.push(byte[0]);
                    if request.ends_with(b"\r\n\r\n") {
                        return request;
                    }
                    assert!(
                        request.len() <= 16 * 1024,
                        "{label} request headers too large"
                    );
                }
                Err(error) => panic!("{label} read: {error}"),
            }
        }
    }

    fn try_read_http_headers(stream: &mut TcpStream) -> Option<Vec<u8>> {
        let mut request = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            match stream.read(&mut byte) {
                Ok(0) => return None,
                Ok(_) => {
                    request.push(byte[0]);
                    if request.ends_with(b"\r\n\r\n") {
                        return Some(request);
                    }
                    assert!(
                        request.len() <= 16 * 1024,
                        "HTTP fixture request headers too large"
                    );
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                    ) =>
                {
                    return None;
                }
                Err(error) => panic!("HTTP fixture read: {error}"),
            }
        }
    }

    fn start_http_server(requests: usize) -> (u16, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind HTTP fixture");
        let port = listener.local_addr().expect("fixture address").port();
        listener
            .set_nonblocking(true)
            .expect("nonblocking HTTP fixture listener");
        let join = std::thread::spawn(move || {
            for _ in 0..requests {
                let mut stream = accept_with_deadline(&listener, "HTTP fixture");
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .expect("read timeout");
                stream
                    .set_write_timeout(Some(Duration::from_secs(2)))
                    .expect("write timeout");
                let request = read_http_headers(&mut stream, "HTTP fixture");
                assert!(
                    request
                        .windows(b"GET /".len())
                        .any(|window| window == b"GET /"),
                    "HTTP fixture expected a GET request, got {:?}",
                    String::from_utf8_lossy(&request)
                );
                if request
                    .windows(b"GET /events".len())
                    .any(|window| window == b"GET /events")
                {
                    stream
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
                        )
                        .expect("write SSE headers");
                    stream
                        .write_all(b"id: first\ndata: first\n\n")
                        .expect("write first SSE event");
                    std::thread::sleep(Duration::from_millis(20));
                    stream
                        .write_all(b"id: second\ndata: second\n\n")
                        .expect("write second SSE event");
                } else {
                    std::thread::sleep(Duration::from_millis(20));
                    stream
                        .write_all(
                            b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        )
                        .expect("write HTTP response");
                }
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
    fn partial_worker_init_failure_rolls_back_runtime_and_http_resources() {
        let mut worker = WorkerRuntime::start().unwrap();
        worker.notify.shutdown_fd();
        let error = unsafe { worker.register_nginx(std::ptr::null_mut()) }
            .expect_err("closed eventfd state must reject partial registration");
        assert_eq!(error, NotifyError::ShutDown);

        let outcome = worker.shutdown();
        assert_eq!(outcome.http, HttpShutdownPoll::Ready);
        assert!(
            worker
                .http_lease_for(&crate::host::StandardHostPolicy::fail_closed())
                .is_err(),
            "rollback must close later HTTP admission"
        );
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

    fn http_policy(port: u16) -> crate::host::StandardHostPolicy {
        let mut policy = crate::host::StandardHostPolicy::fail_closed();
        policy.http_hosts.push("127.0.0.1".into());
        policy.http_schemes.push("http".into());
        policy.http_ports.push(port);
        policy.http_allow_private = true;
        policy
    }

    fn buffered_http_program(port: u16, marker: &str) -> Arc<Program> {
        compile_rss_source(&format!(
            r#"
use http;
use ngx;
if ngx::ctx::get("seen") != null {{ 1 / 0; }}
ngx::ctx::set("seen", 1);
let response: map = http::client::request({{
    method: "GET",
    url: "http://127.0.0.1:{port}/{marker}",
    body: null
}});
if response["status"] != 204 {{ 1 / 0; }}
"#
        ))
        .expect("compile buffered HTTP program")
    }

    fn compile_http_program(source: &str) -> Arc<Program> {
        let mut builder = vm::HostApiBuilder::new();
        let ngx_catalog = crate::host::compile_host_catalog();
        let http_catalog = vm::http_host_catalog();
        for resource in ngx_catalog.resources() {
            builder.resource(resource.clone());
        }
        for schema in ngx_catalog.structs() {
            builder.named_struct(schema.clone());
        }
        for schema in http_catalog.structs() {
            builder.named_struct(schema.clone());
        }
        for function in ngx_catalog.functions() {
            builder.function(function.clone());
        }
        let catalog = Arc::new(builder.build().expect("merged HTTP host catalog"));
        let compiled = vm::compile_source_with_flavor_and_options(
            source,
            SourceFlavor::RustScript,
            CompileSourceFileOptions::default().with_host_api_catalog(catalog),
        )
        .expect("compile HTTP program");
        Arc::new(compiled.program)
    }

    fn sse_http_program(port: u16) -> Arc<Program> {
        compile_http_program(&format!(
            r#"
use http;
fn on_sse_event(item: SseEvent) -> SseCallbackAction {{
    if item["kind"] == "event"
        && item["data"] != "first"
        && item["data"] != "second"
    {{
        1 / 0;
    }}
    {{ action: "continue" }}
}}
let summary = http::client::sse({{
    method: "GET",
    url: "http://127.0.0.1:{port}/events",
    body: null
}}, on_sse_event);
if summary["outcome"] != "eof" {{ 1 / 0; }}
if summary["items"] != 4 {{ 1 / 0; }}
"#
        ))
    }

    fn start_keep_alive_server(
        requests: usize,
    ) -> (
        u16,
        std::sync::mpsc::Receiver<usize>,
        std::thread::JoinHandle<()>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind keep-alive fixture");
        let port = listener.local_addr().expect("keep-alive address").port();
        listener
            .set_nonblocking(true)
            .expect("nonblocking keep-alive listener");
        let (sender, receiver) = std::sync::mpsc::channel();
        let join = std::thread::spawn(move || {
            let mut accepted = 0usize;
            let mut stream: Option<std::net::TcpStream> = None;
            for _ in 0..requests {
                let mut request = Vec::new();
                loop {
                    if stream.is_none() {
                        let deadline = Instant::now() + Duration::from_secs(8);
                        let next = loop {
                            match listener.accept() {
                                Ok((next, _)) => break next,
                                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                                    assert!(
                                        Instant::now() < deadline,
                                        "keep-alive accept timed out"
                                    );
                                    std::thread::sleep(Duration::from_millis(1));
                                }
                                Err(error) => panic!("keep-alive accept: {error}"),
                            }
                        };
                        next.set_nonblocking(false).expect("blocking stream");
                        next.set_read_timeout(Some(Duration::from_secs(3)))
                            .expect("read timeout");
                        next.set_write_timeout(Some(Duration::from_secs(3)))
                            .expect("write timeout");
                        stream = Some(next);
                        accepted += 1;
                    }
                    let socket = stream.as_mut().expect("connection");
                    let mut byte = [0u8; 1];
                    match socket.read(&mut byte) {
                        Ok(0) if request.is_empty() => {
                            stream = None;
                        }
                        Ok(0) => panic!("keep-alive request ended before headers"),
                        Ok(_) => {
                            request.push(byte[0]);
                            if request.ends_with(b"\r\n\r\n") {
                                break;
                            }
                        }
                        Err(error) => panic!("keep-alive read: {error}"),
                    }
                }
                let socket = stream.as_mut().expect("connection");
                if request
                    .windows(b"GET /events".len())
                    .any(|window| window == b"GET /events")
                {
                    socket
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
                        )
                        .expect("write keep-alive SSE headers");
                    socket
                        .write_all(b"id: first\ndata: first\n\n")
                        .expect("write keep-alive first SSE event");
                    std::thread::sleep(Duration::from_millis(20));
                    socket
                        .write_all(b"id: second\ndata: second\n\n")
                        .expect("write keep-alive second SSE event");
                } else {
                    socket
                        .write_all(
                            b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: keep-alive\r\n\r\n",
                        )
                        .expect("keep-alive response");
                }
            }
            sender.send(accepted).expect("send accept count");
        });
        (port, receiver, join)
    }

    fn start_held_http_server() -> (
        u16,
        std::sync::mpsc::Receiver<usize>,
        std::sync::mpsc::Sender<()>,
        std::thread::JoinHandle<()>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind held HTTP fixture");
        let port = listener.local_addr().expect("held fixture address").port();
        listener
            .set_nonblocking(true)
            .expect("nonblocking held fixture listener");
        let (ready_sender, ready_receiver) = std::sync::mpsc::channel();
        let (release_sender, release_receiver) = std::sync::mpsc::channel();
        let join = std::thread::spawn(move || {
            let mut streams = Vec::with_capacity(2);
            let deadline = Instant::now() + Duration::from_secs(8);
            let mut accepted_sockets = 0usize;
            while streams.len() < 2 {
                assert!(
                    accepted_sockets < 8 && Instant::now() < deadline,
                    "held HTTP fixture did not receive two valid requests"
                );
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(
                                Instant::now() < deadline,
                                "held HTTP fixture accept timed out"
                            );
                            std::thread::sleep(Duration::from_millis(1));
                        }
                        Err(error) => panic!("held HTTP fixture accept: {error}"),
                    }
                };
                accepted_sockets += 1;
                stream
                    .set_nonblocking(false)
                    .expect("blocking held fixture stream");
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .expect("held read timeout");
                stream
                    .set_write_timeout(Some(Duration::from_secs(2)))
                    .expect("held write timeout");
                let Some(request) = try_read_http_headers(&mut stream) else {
                    continue;
                };
                assert!(
                    request
                        .windows(b"GET /".len())
                        .any(|window| window == b"GET /"),
                    "held fixture expected a GET request, got {:?}",
                    String::from_utf8_lossy(&request)
                );
                let is_sse = request
                    .windows(b"GET /events".len())
                    .any(|window| window == b"GET /events");
                if is_sse {
                    stream
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
                        )
                        .expect("write held SSE headers");
                    stream
                        .write_all(b"id: first\ndata: first\n\n")
                        .expect("write held SSE event");
                }
                streams.push((stream, is_sse));
            }
            ready_sender
                .send(streams.len())
                .expect("send held HTTP readiness");
            release_receiver
                .recv_timeout(Duration::from_secs(30))
                .expect("held HTTP release");
            for (mut stream, is_sse) in streams {
                if is_sse {
                    let _ = stream.write_all(b"id: second\ndata: second\n\n");
                } else {
                    let _ = stream.write_all(
                        b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    );
                }
                let _ = stream.shutdown(std::net::Shutdown::Both);
            }
        });
        (port, ready_receiver, release_sender, join)
    }

    fn drive_held_http_until_ready(
        worker: &mut WorkerRuntime,
        buffered: &mut AccessExec,
        sse: &mut AccessExec,
        registry: &mut IdentityRegistry<u64>,
        ready: &std::sync::mpsc::Receiver<usize>,
    ) -> usize {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match ready.try_recv() {
                Ok(count) => return count,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    panic!("held HTTP fixture stopped before readiness")
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
            assert!(Instant::now() < deadline, "held HTTP readiness timed out");
            if let Some(fd) = worker.raw_fd()
                && wait_readable(fd, Duration::from_millis(25))
            {
                for identity in worker.drain_ready().expect("drain held HTTP readiness") {
                    if identity == buffered.identity() {
                        let _ = buffered.apply_ready(identity, registry, worker);
                    } else if identity == sse.identity() {
                        let _ = sse.apply_ready(identity, registry, worker);
                    }
                }
            }
        }
    }

    fn run_reusable_http(
        worker: &mut WorkerRuntime,
        program: Arc<Program>,
        request_id: u64,
        policy: crate::host::StandardHostPolicy,
    ) {
        let mut exec = worker
            .bind_reusable_phase_exec_with_policy(
                program,
                request_id,
                crate::host::NgxPhase::Access,
                policy,
            )
            .expect("bind reusable HTTP VM");
        let status = run_async_exec_to_halt(worker, &mut exec);
        assert_eq!(
            status,
            AccessStatus::Ok,
            "buffered HTTP execution failed: {:?}",
            exec.last_error()
        );
        assert!(exec.cleanup_wait(worker).is_none());
    }

    #[test]
    fn one_worker_http_config_shares_client_across_vm_reuse_and_eviction() {
        let (port, accepted, server) = start_keep_alive_server(10);
        let mut worker = WorkerRuntime::start().unwrap();
        let policy = http_policy(port);
        let program = buffered_http_program(port, "reuse");
        assert!(crate::config::program_can_reuse_request_vm(&program));

        for request_id in 1..=5 {
            run_reusable_http(
                &mut worker,
                Arc::clone(&program),
                request_id,
                policy.clone(),
            );
        }
        assert_eq!(worker.reusable_request_vm_created_count(), 1);
        assert_eq!(worker.http_client_build_count(), 1);

        for (offset, marker) in ["a", "b", "c", "d", "e"].into_iter().enumerate() {
            run_reusable_http(
                &mut worker,
                buffered_http_program(port, marker),
                10 + offset as u64,
                policy.clone(),
            );
        }
        assert!(worker.reusable_request_vm_created_count() >= 5);
        assert_eq!(
            worker.reusable_request_vm_idle_count(),
            super::MAX_IDLE_REUSABLE_REQUEST_VMS
        );
        assert_eq!(
            worker.http_client_build_count(),
            1,
            "VM pool cap/eviction must not build another HTTP client"
        );

        worker.shutdown();
        assert_eq!(accepted.recv_timeout(Duration::from_secs(3)).unwrap(), 1);
        server.join().expect("keep-alive server");
    }

    #[test]
    fn different_http_configs_stay_isolated() {
        let (port, server) = start_http_server(2);
        let mut worker = WorkerRuntime::start().unwrap();
        let first = http_policy(port);
        let mut second = first.clone();
        second.http_hosts.push("localhost".into());
        let program = buffered_http_program(port, "isolated");

        run_reusable_http(&mut worker, Arc::clone(&program), 1, first);
        run_reusable_http(&mut worker, program, 2, second);
        assert_eq!(worker.http_client_build_count(), 2);
        assert_eq!(worker.reusable_request_vm_created_count(), 2);

        worker.shutdown();
        server.join().expect("isolated HTTP fixture");
    }

    #[test]
    fn vm_reset_and_drop_retain_worker_http_client_ownership() {
        let (port, server) = start_http_server(8);
        let mut worker = WorkerRuntime::start().unwrap();
        let policy = http_policy(port);
        let ok = buffered_http_program(port, "ok");
        let failed = compile_rss_source(&format!(
            r#"
use http;
let response: map = http::client::request({{
    method: "GET",
    url: "http://127.0.0.1:{port}/fail",
    body: null
}});
let boom = 1 / 0;
"#
        ))
        .expect("compile failing HTTP program");

        for request_id in 1..=2 {
            run_reusable_http(&mut worker, Arc::clone(&ok), request_id, policy.clone());
        }
        assert_eq!(worker.reusable_request_vm_created_count(), 1);
        assert_eq!(worker.http_client_build_count(), 1);

        for request_id in 3..=8 {
            let mut exec = worker
                .bind_reusable_phase_exec_with_policy(
                    Arc::clone(&failed),
                    request_id,
                    crate::host::NgxPhase::Access,
                    policy.clone(),
                )
                .expect("bind dropped HTTP VM");
            assert_eq!(
                run_async_exec_to_halt(&mut worker, &mut exec),
                AccessStatus::Error500
            );
            assert!(exec.cleanup_wait(&worker).is_none());
        }
        assert_eq!(worker.http_client_build_count(), 1);
        assert_eq!(worker.reusable_request_vm_idle_count(), 1);

        worker.shutdown();
        server.join().expect("HTTP fixture server");
    }

    #[test]
    fn worker_http_admission_allows_more_than_64_vm_leases() {
        const VMS: usize = 65;
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind lease boundary address");
        let port = listener
            .local_addr()
            .expect("lease boundary address")
            .port();
        drop(listener);
        let mut worker = WorkerRuntime::start().unwrap();
        let policy = http_policy(port);
        let program = buffered_http_program(port, "admission");
        let mut execs = Vec::with_capacity(VMS);
        for request_id in 1..=VMS as u64 {
            execs.push(
                worker
                    .bind_phase_exec_with_policy(
                        Arc::clone(&program),
                        request_id,
                        crate::host::NgxPhase::Access,
                        policy.clone(),
                    )
                    .expect("bind HTTP VM lease"),
            );
        }
        assert_eq!(execs.len(), VMS);
        assert_eq!(worker.http_client_build_count(), 1);
        drop(execs);
        worker.shutdown();
    }

    #[test]
    fn http_and_sse_share_one_worker_lease_and_no_per_vm_fallback() {
        let (http_port, accepted, http_server) = start_keep_alive_server(2);
        let mut worker = WorkerRuntime::start().unwrap();
        let policy = http_policy(http_port);

        run_reusable_http(
            &mut worker,
            buffered_http_program(http_port, "shared"),
            1,
            policy.clone(),
        );
        assert_eq!(worker.http_client_build_count(), 1);

        let sse = sse_http_program(http_port);
        assert!(!crate::config::program_can_reuse_request_vm(&sse));
        let mut exec = worker
            .bind_phase_exec_with_policy(sse, 2, crate::host::NgxPhase::Access, policy)
            .expect("bind SSE VM");
        assert_eq!(
            run_async_exec_to_halt(&mut worker, &mut exec),
            AccessStatus::Ok,
            "SSE through the worker lease must complete successfully: {:?}",
            exec.last_error()
        );
        assert!(exec.cleanup_wait(&worker).is_none());
        assert_eq!(worker.http_client_build_count(), 1);
        assert_eq!(worker.reusable_request_vm_created_count(), 1);

        worker.shutdown();
        assert_eq!(accepted.recv_timeout(Duration::from_secs(3)).unwrap(), 1);
        http_server.join().expect("HTTP fixture");
    }

    #[test]
    fn worker_http_shutdown_rejects_new_work_and_active_ops_retire() {
        let (port, ready, release, server) = start_held_http_server();
        let mut worker = WorkerRuntime::start().unwrap();
        let policy = http_policy(port);
        let mut buffered = worker
            .bind_phase_exec_with_policy(
                buffered_http_program(port, "pending"),
                1,
                crate::host::NgxPhase::Access,
                policy.clone(),
            )
            .expect("bind buffered pending HTTP");
        let mut sse = worker
            .bind_phase_exec_with_policy(
                sse_http_program(port),
                2,
                crate::host::NgxPhase::Access,
                policy.clone(),
            )
            .expect("bind SSE pending HTTP");
        let mut registry = IdentityRegistry::with_capacity(16);
        assert_eq!(
            buffered.handle_access(&mut registry, &worker),
            AccessStatus::Again
        );
        buffered.commit_suspend();
        assert_eq!(
            sse.handle_access(&mut registry, &worker),
            AccessStatus::Again
        );
        sse.commit_suspend();
        let accepted = drive_held_http_until_ready(
            &mut worker,
            &mut buffered,
            &mut sse,
            &mut registry,
            &ready,
        );
        assert_eq!(
            accepted, 2,
            "buffered and SSE operations must reach the long-held upstream"
        );
        assert!(
            matches!(sse.vm_status(), Some(VmStatus::Waiting(_))),
            "an open SSE stream must retain its host operation and permit"
        );

        let start = Instant::now();
        let outcome = worker.shutdown();
        assert!(
            start.elapsed() < Duration::from_secs(6),
            "explicit HTTP shutdown must remain bounded"
        );
        assert_eq!(outcome.http, HttpShutdownPoll::Ready);
        assert!(!outcome.already_stopped);
        assert!(
            worker
                .bind_phase_exec_with_policy(
                    buffered_http_program(port, "after"),
                    3,
                    crate::host::NgxPhase::Access,
                    policy.clone(),
                )
                .is_err(),
            "shutdown must reject new HTTP VM admission"
        );
        assert!(
            worker.http_lease_for(&http_policy(port)).is_err(),
            "shutdown must reject new HTTP leases"
        );
        assert!(buffered.cleanup_wait(&worker).is_some());
        assert!(sse.cleanup_wait(&worker).is_some());
        assert!(buffered.cleanup_wait(&worker).is_none());
        assert!(sse.cleanup_wait(&worker).is_none());
        release.send(()).expect("release held HTTP fixture");
        server.join().expect("pending HTTP fixture");

        let (replacement_port, replacement_server) = start_http_server(1);
        let mut replacement = WorkerRuntime::start().unwrap();
        run_reusable_http(
            &mut replacement,
            buffered_http_program(replacement_port, "replacement"),
            4,
            http_policy(replacement_port),
        );
        let replacement_outcome = replacement.shutdown();
        assert_eq!(replacement_outcome.http, HttpShutdownPoll::Ready);
        replacement_server.join().expect("replacement HTTP fixture");
    }

    #[test]
    fn worker_drop_is_nonblocking_with_active_http_and_sse() {
        let (port, ready, release, server) = start_held_http_server();
        let mut worker = WorkerRuntime::start().unwrap();
        let policy = http_policy(port);
        let mut buffered = worker
            .bind_phase_exec_with_policy(
                buffered_http_program(port, "drop-pending"),
                1,
                crate::host::NgxPhase::Access,
                policy.clone(),
            )
            .expect("bind buffered Drop HTTP");
        let mut sse = worker
            .bind_phase_exec_with_policy(
                sse_http_program(port),
                2,
                crate::host::NgxPhase::Access,
                policy,
            )
            .expect("bind SSE Drop HTTP");
        let mut registry = IdentityRegistry::with_capacity(16);
        assert_eq!(
            buffered.handle_access(&mut registry, &worker),
            AccessStatus::Again
        );
        buffered.commit_suspend();
        assert_eq!(
            sse.handle_access(&mut registry, &worker),
            AccessStatus::Again
        );
        sse.commit_suspend();
        assert_eq!(
            drive_held_http_until_ready(
                &mut worker,
                &mut buffered,
                &mut sse,
                &mut registry,
                &ready,
            ),
            2
        );

        let start = Instant::now();
        drop(worker);
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "WorkerRuntime Drop must not wait for active HTTP work"
        );
        drop(buffered);
        drop(sse);
        release.send(()).expect("release Drop HTTP fixture");
        server.join().expect("Drop HTTP fixture");
    }

    #[test]
    fn worker_replacement_owns_independent_http_clients() {
        let (port, server) = start_http_server(4);
        let policy = http_policy(port);
        let program = buffered_http_program(port, "reload");

        let mut first = WorkerRuntime::start().unwrap();
        run_reusable_http(&mut first, Arc::clone(&program), 1, policy.clone());
        assert_eq!(first.http_client_build_count(), 1);

        let mut second = WorkerRuntime::start().unwrap();
        run_reusable_http(&mut second, Arc::clone(&program), 2, policy.clone());
        assert_eq!(second.http_client_build_count(), 1);
        first.shutdown();
        run_reusable_http(&mut second, Arc::clone(&program), 3, policy.clone());
        assert_eq!(second.http_client_build_count(), 1);

        let mut third = WorkerRuntime::start().unwrap();
        run_reusable_http(&mut third, program, 4, policy);
        assert_eq!(third.http_client_build_count(), 1);
        second.shutdown();
        third.shutdown();
        server.join().expect("reload HTTP fixture");
    }
}
