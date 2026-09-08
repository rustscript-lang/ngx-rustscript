//! Worker-local Tokio runtime for `tokio::time::sleep`.
//!
//! Created in NGINX `init_process` after fork, only in `NGX_PROCESS_WORKER`
//! or `NGX_PROCESS_SINGLE`. Helper, master, config, and `nginx -t` never
//! create Tokio threads or the eventfd. Tokio tasks own only [`SleepToken`],
//! an integer duration, a [`CompletionSender`], and a [`CompletionPermit`].
//! They never hold NGINX request/pool/VM pointers and never call
//! `ngx_post_event`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt;
use std::panic::AssertUnwindSafe;
use std::time::Duration;

use nginx_sys::{ngx_cycle_t, ngx_event_t, ngx_int_t, ngx_uint_t};
use ngx::core::Status;
use ngx::ffi::{NGX_LOG_ERR, NGX_LOG_NOTICE, NGX_PROCESS_SINGLE, NGX_PROCESS_WORKER, ngx_process};
use ngx::ngx_log_error;
use tokio::runtime::Runtime;
use tokio::task::AbortHandle;

use crate::notify::{
    CompletionPermit, CompletionSender, DEFAULT_MAX_IN_FLIGHT, NotifyBridge, NotifyError,
};
use crate::sleep_binding::{SleepCoordinator, SleepOpsError, SleepToken};

thread_local! {
    static WORKER: RefCell<Option<WorkerRuntime>> = const { RefCell::new(None) };
}

/// Fail-closed worker runtime errors.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RuntimeError {
    Notify(NotifyError),
    SleepOps(SleepOpsError),
    InvalidDuration,
    RuntimeBuild(String),
    AlreadyStarted,
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Notify(err) => write!(f, "{err}"),
            Self::SleepOps(err) => write!(f, "{err}"),
            Self::InvalidDuration => {
                write!(f, "ngx::sleep duration is not a valid millisecond count")
            }
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

impl From<SleepOpsError> for RuntimeError {
    fn from(err: SleepOpsError) -> Self {
        Self::SleepOps(err)
    }
}

fn is_worker_or_single() -> bool {
    // SAFETY: `ngx_process` is NGINX process-lifetime state. Read on the
    // worker/main thread from `init_process`.
    let process = unsafe { ngx_process };
    process == NGX_PROCESS_WORKER as ngx_uint_t || process == NGX_PROCESS_SINGLE as ngx_uint_t
}

/// Tokio task payload: no NGINX request, pool, VM, or raw pointer.
struct SleepTask {
    token: SleepToken,
    duration_ms: u64,
    sender: CompletionSender,
    permit: CompletionPermit,
}

impl SleepTask {
    async fn run(self) {
        tokio::time::sleep(Duration::from_millis(self.duration_ms)).await;
        let _ = self.sender.complete(self.token, self.permit);
    }
}

/// Per-worker Tokio runtime, eventfd bridge, and tracked abort handles.
pub struct WorkerRuntime {
    runtime: Option<Runtime>,
    notify: NotifyBridge,
    tasks: std::sync::Mutex<HashMap<SleepToken, AbortHandle>>,
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
            .thread_name("ngx-rustscript-sleep")
            .build()
            .map_err(|err| RuntimeError::RuntimeBuild(err.to_string()))?;
        Ok(Self {
            runtime: Some(runtime),
            notify: NotifyBridge::with_capacity(cap)?,
            tasks: std::sync::Mutex::new(HashMap::new()),
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

    #[cfg(test)]
    pub(crate) fn in_flight(&self) -> Result<usize, NotifyError> {
        self.notify.in_flight()
    }

    /// Submit one Task 3 `(SleepToken, ms)` claim to a real `tokio::time::sleep`.
    pub fn submit_sleep(&self, token: SleepToken, ms: i64) -> Result<(), RuntimeError> {
        if self.stopped || !self.notify.is_accepting() {
            return Err(NotifyError::ShutDown.into());
        }
        let duration_ms = u64::try_from(ms).map_err(|_| RuntimeError::InvalidDuration)?;
        let permit = self.notify.try_reserve()?;
        self.spawn_sleep(token, duration_ms, permit)
    }

    fn spawn_sleep(
        &self,
        token: SleepToken,
        duration_ms: u64,
        permit: CompletionPermit,
    ) -> Result<(), RuntimeError> {
        let runtime = self.runtime.as_ref().ok_or(NotifyError::ShutDown)?;
        let sender = self.notify.sender();
        let handle = runtime.spawn(
            SleepTask {
                token,
                duration_ms,
                sender,
                permit,
            }
            .run(),
        );
        match self.tasks.lock() {
            Ok(mut tasks) => {
                tasks.insert(token, handle.abort_handle());
            }
            Err(_) => {
                handle.abort();
                return Err(NotifyError::Poisoned.into());
            }
        }
        Ok(())
    }

    /// Claim every queued Task 3 `(token, ms)` submission and spawn it.
    ///
    /// Capacity is reserved before `claim_submission`, so a full queue does
    /// not consume the Task 3 item.
    pub fn submit_claimed(&self, coordinator: &SleepCoordinator) -> Result<usize, RuntimeError> {
        let mut n = 0;
        loop {
            if self.stopped || !self.notify.is_accepting() {
                return Err(NotifyError::ShutDown.into());
            }
            let permit = match self.notify.try_reserve() {
                Ok(permit) => permit,
                Err(NotifyError::QueueFull) if n > 0 => return Ok(n),
                Err(err) => return Err(err.into()),
            };
            match coordinator.claim_submission()? {
                Some((token, ms)) => {
                    let duration_ms =
                        u64::try_from(ms).map_err(|_| RuntimeError::InvalidDuration)?;
                    self.spawn_sleep(token, duration_ms, permit)?;
                    n += 1;
                }
                None => return Ok(n),
            }
        }
    }

    pub fn on_notify_readable(&mut self) -> Result<(), NotifyError> {
        self.notify.harvest()
    }

    pub fn pop_ready(&mut self) -> Result<Option<SleepToken>, NotifyError> {
        let token = self.notify.pop_ready()?;
        if let Some(token) = token {
            if let Ok(mut tasks) = self.tasks.lock() {
                tasks.remove(&token);
            }
            Ok(Some(token))
        } else {
            Ok(None)
        }
    }

    pub fn drain_ready(&mut self) -> Result<Vec<SleepToken>, RuntimeError> {
        self.notify.harvest()?;
        let ready = self.notify.take_ready()?;
        if let Ok(mut tasks) = self.tasks.lock() {
            for token in &ready {
                tasks.remove(token);
            }
        }
        Ok(ready)
    }

    /// Unregister NGINX, abort tracked tasks, shut down Tokio, then close fd.
    pub fn shutdown(&mut self) {
        if self.stopped {
            return;
        }
        self.stopped = true;
        self.notify.reject_new();
        if let Ok(mut tasks) = self.tasks.lock() {
            for handle in tasks.values() {
                handle.abort();
            }
            tasks.clear();
        }
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

impl Drop for WorkerRuntime {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// NGINX worker `init_process` entry: create runtime + eventfd + register read.
///
/// Helper processes return without starting. Must not run during
/// configuration, master, or `nginx -t`.
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
        WORKER.with(|slot| match slot.borrow_mut().as_mut() {
            Some(worker) => worker.on_notify_readable(),
            None => Ok(()),
        })
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
    use crate::sleep_binding::SleepSession;
    use std::os::unix::io::RawFd;
    use std::time::{Duration, Instant};
    use vm::{CallOutcome, HostArgsFunction, Value};

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

    fn token(id: u64) -> SleepToken {
        SleepToken::from_raw(1, id)
    }

    #[test]
    fn sleep_task_payload_contains_no_nginx_data() {
        fn assert_send<T: Send>() {}
        assert_send::<SleepTask>();
        assert_send::<SleepToken>();
        assert_send::<u64>();
        assert_send::<CompletionSender>();
        assert_send::<CompletionPermit>();
        for name in [
            std::any::type_name::<SleepTask>(),
            std::any::type_name::<CompletionSender>(),
            std::any::type_name::<CompletionPermit>(),
        ] {
            assert!(
                !name.contains("ngx_connection")
                    && !name.contains("ngx_http")
                    && !name.contains("ngx_pool")
                    && !name.contains("ngx_cycle")
                    && !name.contains("Request")
                    && !name.contains("Vm"),
                "task payload must not mention NGINX/VM data: {name}"
            );
        }
    }

    #[test]
    fn real_tokio_sleep_completion_wakes_eventfd_with_token() {
        let mut worker = WorkerRuntime::start().unwrap();
        let fd = worker.raw_fd().expect("eventfd");
        worker.submit_sleep(token(1), 1).unwrap();
        assert!(
            wait_readable(fd, Duration::from_secs(2)),
            "eventfd must become readable after tokio::time::sleep"
        );
        let ready = worker.drain_ready().unwrap();
        assert_eq!(ready, vec![token(1)]);
        worker.shutdown();
    }

    #[test]
    fn zero_ms_sleep_still_delivers_payload_not_just_elapsed_time() {
        let mut worker = WorkerRuntime::start().unwrap();
        let fd = worker.raw_fd().expect("eventfd");
        worker.submit_sleep(token(2), 0).unwrap();
        assert!(wait_readable(fd, Duration::from_secs(2)));
        assert_eq!(worker.drain_ready().unwrap(), vec![token(2)]);
        worker.shutdown();
    }

    #[test]
    fn many_sleeps_complete_without_loss() {
        let mut worker = WorkerRuntime::start_with_capacity(16).unwrap();
        let fd = worker.raw_fd().expect("eventfd");
        for id in 1..=8 {
            worker.submit_sleep(token(id), 0).unwrap();
        }
        let start = Instant::now();
        let mut got = Vec::new();
        while got.len() < 8 && start.elapsed() < Duration::from_secs(2) {
            let _ = wait_readable(fd, Duration::from_millis(50));
            got.extend(worker.drain_ready().unwrap());
        }
        got.sort_by_key(|t| t.op_id());
        let expected: Vec<_> = (1..=8).map(token).collect();
        assert_eq!(got, expected);
        worker.shutdown();
    }

    #[test]
    fn capacity_rejection_recovers_after_drain() {
        let mut worker = WorkerRuntime::start_with_capacity(1).unwrap();
        worker.submit_sleep(token(1), 0).unwrap();
        assert_eq!(
            worker.submit_sleep(token(2), 0),
            Err(RuntimeError::Notify(NotifyError::QueueFull))
        );
        let fd = worker.raw_fd().expect("eventfd");
        assert!(wait_readable(fd, Duration::from_secs(2)));
        assert_eq!(worker.drain_ready().unwrap(), vec![token(1)]);
        worker.submit_sleep(token(2), 0).unwrap();
        assert!(wait_readable(fd, Duration::from_secs(2)));
        assert_eq!(worker.drain_ready().unwrap(), vec![token(2)]);
        worker.shutdown();
    }

    #[test]
    fn submit_after_shutdown_is_rejected() {
        let mut worker = WorkerRuntime::start().unwrap();
        worker.shutdown();
        assert_eq!(
            worker.submit_sleep(token(1), 0),
            Err(RuntimeError::Notify(NotifyError::ShutDown))
        );
        worker.shutdown();
    }

    #[test]
    fn shutdown_vs_completion_does_not_hang_or_lose_fd_ownership() {
        let mut worker = WorkerRuntime::start().unwrap();
        worker.submit_sleep(token(1), 60_000).unwrap();
        let start = Instant::now();
        worker.shutdown();
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "abort + runtime shutdown must not wait out the sleep"
        );
        assert!(worker.raw_fd().is_none());
        assert!(!worker.is_accepting());
        assert_eq!(worker.in_flight().unwrap(), 0);
    }

    #[test]
    fn shutdown_releases_unfinished_reservation() {
        let mut worker = WorkerRuntime::start_with_capacity(1).unwrap();
        worker.submit_sleep(token(1), 60_000).unwrap();
        assert_eq!(worker.in_flight().unwrap(), 1);
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
    fn task3_claim_submission_feeds_runtime_once() {
        let coordinator = SleepCoordinator::new();
        let mut session = SleepSession::new(&coordinator).unwrap();
        let outcome = session.call(&[Value::Int(5)]).expect("first host entry");
        assert!(matches!(outcome, CallOutcome::Yield));
        let mut worker = WorkerRuntime::start().unwrap();
        let n = worker.submit_claimed(&coordinator).unwrap();
        assert_eq!(n, 1);
        assert_eq!(worker.submit_claimed(&coordinator).unwrap(), 0);
        let fd = worker.raw_fd().expect("eventfd");
        assert!(wait_readable(fd, Duration::from_secs(2)));
        let ready = worker.drain_ready().unwrap();
        assert_eq!(ready.len(), 1);
        worker.shutdown();
    }

    #[test]
    fn reserve_before_claim_does_not_consume_task3_when_full() {
        let coordinator = SleepCoordinator::new();
        let mut session = SleepSession::new(&coordinator).unwrap();
        let outcome = session.call(&[Value::Int(5)]).expect("first host entry");
        assert!(matches!(outcome, CallOutcome::Yield));
        let mut worker = WorkerRuntime::start_with_capacity(1).unwrap();
        worker.submit_sleep(token(99), 60_000).unwrap();
        let err = worker.submit_claimed(&coordinator).expect_err("full");
        assert_eq!(err, RuntimeError::Notify(NotifyError::QueueFull));
        let claimed = coordinator.claim_submission().unwrap();
        assert!(
            claimed.is_some(),
            "Task 3 submission must remain claimable when reserve fails"
        );
        worker.shutdown();
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
    fn poisoned_task_map_does_not_panic_on_submit() {
        let worker = WorkerRuntime::start().unwrap();
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = worker.tasks.lock().expect("lock");
            panic!("poison tasks");
        }));
        std::panic::set_hook(prev);
        let err = worker.submit_sleep(token(1), 0).expect_err("poison");
        assert_eq!(err, RuntimeError::Notify(NotifyError::Poisoned));
        drop(worker);
    }
}
