//! Worker-owned counting semaphores for `ngx::semaphore::{new,wait,post,count}`.
//!
//! OpenResty semaphores are worker-local objects, not request objects. Here the
//! permit counter and the FIFO waiter queue live in a worker-owned registry
//! (`thread_local`, one registry per nginx worker), while the current executing
//! VM holds a typed [`SemaphoreHandle`] resource entry in **its own** resource
//! table. A sync-only program may use a pooled VM; a program using async
//! `wait` stays on the request-owned VM path. The script sees that entry as an
//! `i64` handle, exactly like `ngx.socket.udp` / `ngx.socket.tcp`.
//!
//! # Ownership and escape rules
//!
//! - [`SemaphoreState`] holds the counter, the FIFO waiter queue, and a closed
//!   flag. It is `Send + Sync` and referenced through `Arc`.
//! - A wait future owns an `Arc<SemaphoreState>`, an owned timeout, and a drop
//!   guard. It never holds `*mut ngx_http_request_t`, a request pool, a `Vm`,
//!   `RequestLocalCtx`, or a foreign `ResourceHandle`.
//! - Waking a waiter only calls the [`Waker`] the generic
//!   [`crate::async_bridge::NgxAsyncBridge`] handed in (eventfd identity
//!   queue). No new wake protocol, no scheduler callback engine.
//!
//! # Semantics
//!
//! - Permits are consumed **only** at grant time, inside the same critical
//!   section that observes the waiter at the front of the queue. `post`,
//!   timeout, cancellation, and drop therefore can never leak or double
//!   release a permit.
//! - FIFO: a waiter takes a permit only when it is the front entry. `post`
//!   wakes at most `min(count, queued)` front waiters that have registered a
//!   waker; each granted waiter cascades the wake to the next entry.
//! - Cleanup is exactly-once and idempotent: the drop guard removes its queue
//!   entry if it is still queued and otherwise does nothing.
//! - Worker shutdown marks every live semaphore closed, wakes every waiter, and
//!   empties the registry. Ids are never reused.
//!
//! # Budgets
//!
//! - In-flight waits share `DEFAULT_MAX_IN_FLIGHT` (1024) through the existing
//!   `AccessExec` reservation path. The semaphore hosts add no second cap.
//! - Live semaphores per worker are bounded by [`MAX_LIVE_SEMAPHORES`].
//! - Permits per semaphore are bounded by [`MAX_SEMAPHORE_COUNT`].

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use vm::resource::{CloseProgress, HostResource, ResourceResult};
use vm::{
    CaptureAsyncHostContext, HostResourceType, ResourceCloseReason, ResourceTypeKey, Value, Vm,
    VmError, VmResult,
};

/// Resource type key of the VM-local semaphore handle entry.
pub const NGX_SEMAPHORE_RESOURCE_KEY: &str = "ngx.semaphore";

pub fn semaphore_resource_key() -> ResourceTypeKey {
    ResourceTypeKey::new(NGX_SEMAPHORE_RESOURCE_KEY)
        .expect("ngx.semaphore is a valid resource type key")
}

/// Maximum permits a single semaphore can hold (and post in one call).
pub const MAX_SEMAPHORE_COUNT: u64 = 1_000_000;

/// Maximum live semaphores per nginx worker.
pub const MAX_LIVE_SEMAPHORES: usize = 1024;

/// One queued wait in FIFO order.
struct Waiter {
    id: u64,
    waker: Option<Waker>,
}

struct SemaphoreInner {
    count: u64,
    waiters: VecDeque<Waiter>,
    closed: bool,
}

/// Worker-owned permit counter plus FIFO waiter queue.
pub(crate) struct SemaphoreState {
    id: u64,
    inner: Mutex<SemaphoreInner>,
}

/// Result of one poll against a semaphore state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Acquire {
    /// The waiter was at the front and consumed one permit.
    Granted,
    /// The waiter is queued; the waker will be called when a permit is free.
    Pending,
    /// The semaphore is closed (worker shutdown). Fail closed.
    Closed,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|err| err.into_inner())
}

impl SemaphoreState {
    fn new(id: u64, count: u64) -> Self {
        Self {
            id,
            inner: Mutex::new(SemaphoreInner {
                count,
                waiters: VecDeque::new(),
                closed: false,
            }),
        }
    }

    /// Worker registry id. Never reused within a worker process.
    pub(crate) fn id(&self) -> u64 {
        self.id
    }

    /// Readily available permits minus queued FIFO waiters, matching
    /// `ngx.semaphore:count()`. Closed semaphores fail closed.
    pub(crate) fn count(&self) -> VmResult<i64> {
        let inner = lock(&self.inner);
        if inner.closed {
            return Err(VmError::HostError(
                "semaphore is closed: worker is shutting down".to_string(),
            ));
        }
        Ok(inner.count as i64 - inner.waiters.len() as i64)
    }

    /// Number of queued waiters (test and shutdown observability).
    #[cfg(test)]
    pub(crate) fn waiting_count(&self) -> usize {
        lock(&self.inner).waiters.len()
    }

    /// Adds `amount` permits and wakes queued waiters in FIFO order.
    ///
    /// Fails closed on a closed semaphore and on a ceiling overflow. `0` is a
    /// no-op that never wakes anybody.
    pub(crate) fn post(&self, amount: u64) -> VmResult<u64> {
        let mut wakes = Vec::new();
        let next = {
            let mut inner = lock(&self.inner);
            if inner.closed {
                return Err(VmError::HostError(
                    "semaphore is closed: worker is shutting down".to_string(),
                ));
            }
            let next = inner
                .count
                .checked_add(amount)
                .filter(|next| *next <= MAX_SEMAPHORE_COUNT)
                .ok_or_else(|| {
                    VmError::HostError(format!(
                        "post would exceed the {MAX_SEMAPHORE_COUNT}-permit semaphore ceiling"
                    ))
                })?;
            inner.count = next;
            if amount > 0 {
                wake_front_locked(&mut inner, &mut wakes);
            }
            next
        };
        wake_all(wakes);
        Ok(next)
    }

    /// Poll-time acquire: grant, queue, or fail closed.
    ///
    /// Runs in a single critical section so a concurrent `post` can never be
    /// lost between "check count" and "register waker".
    fn acquire_or_register(&self, waiter_id: u64, waker: &Waker) -> Acquire {
        let mut wakes = Vec::new();
        let outcome = {
            let mut inner = lock(&self.inner);
            if inner.closed {
                Acquire::Closed
            } else if inner.count > 0 {
                let front = inner
                    .waiters
                    .front()
                    .map(|waiter| waiter.id)
                    .filter(|front| *front == waiter_id);
                if front.is_some() {
                    let _ = inner.waiters.pop_front();
                    inner.count -= 1;
                    wake_front_locked(&mut inner, &mut wakes);
                    Acquire::Granted
                } else if inner.waiters.is_empty() {
                    // No queue exists, so this waiter is the front by arrival.
                    inner.count -= 1;
                    Acquire::Granted
                } else {
                    register_waiter(&mut inner, waiter_id, waker);
                    Acquire::Pending
                }
            } else {
                register_waiter(&mut inner, waiter_id, waker);
                Acquire::Pending
            }
        };
        wake_all(wakes);
        outcome
    }

    /// Exactly-once deregistration. A second call is a no-op.
    fn release_waiter(&self, waiter_id: u64) {
        if waiter_id == 0 {
            return;
        }
        let mut wakes = Vec::new();
        {
            let mut inner = lock(&self.inner);
            let before = inner.waiters.len();
            inner.waiters.retain(|waiter| waiter.id != waiter_id);
            if inner.waiters.len() != before {
                wake_front_locked(&mut inner, &mut wakes);
            }
        }
        wake_all(wakes);
    }

    /// Marks the semaphore closed and wakes every queued waiter.
    fn close(&self) {
        let mut wakes = Vec::new();
        {
            let mut inner = lock(&self.inner);
            inner.closed = true;
            for waiter in inner.waiters.drain(..) {
                if let Some(waker) = waiter.waker {
                    wakes.push(waker);
                }
            }
        }
        wake_all(wakes);
    }
}

impl fmt::Debug for SemaphoreState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let inner = lock(&self.inner);
        f.debug_struct("SemaphoreState")
            .field("id", &self.id)
            .field("count", &inner.count)
            .field("waiters", &inner.waiters.len())
            .field("closed", &inner.closed)
            .finish()
    }
}

fn register_waiter(inner: &mut SemaphoreInner, waiter_id: u64, waker: &Waker) {
    if let Some(waiter) = inner
        .waiters
        .iter_mut()
        .find(|waiter| waiter.id == waiter_id)
    {
        waiter.waker = Some(waker.clone());
        return;
    }
    inner.waiters.push_back(Waiter {
        id: waiter_id,
        waker: Some(waker.clone()),
    });
}

/// Wakes at most `min(count, queued)` front waiters that registered a waker.
fn wake_front_locked(inner: &mut SemaphoreInner, wakes: &mut Vec<Waker>) {
    let limit = inner.waiters.len().min(inner.count as usize);
    for waiter in inner.waiters.iter_mut().take(limit) {
        if let Some(waker) = waiter.waker.take() {
            wakes.push(waker);
        }
    }
}

fn wake_all(wakes: Vec<Waker>) {
    for waker in wakes {
        waker.wake();
    }
}

/// VM-local semaphore handle resource entry.
pub(crate) struct SemaphoreHandle {
    id: u64,
    state: Arc<SemaphoreState>,
}

impl SemaphoreHandle {
    pub(crate) fn new(state: Arc<SemaphoreState>) -> Self {
        Self {
            id: state.id(),
            state,
        }
    }

    #[cfg(test)]
    pub(crate) fn id(&self) -> u64 {
        self.id
    }

    pub(crate) fn state(&self) -> &Arc<SemaphoreState> {
        &self.state
    }

    fn release(&self) {
        release_worker_handle(self.id);
    }
}

impl fmt::Debug for SemaphoreHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SemaphoreHandle")
            .field("id", &self.id)
            .finish()
    }
}

impl Drop for SemaphoreHandle {
    fn drop(&mut self) {
        self.release();
    }
}

impl HostResource for SemaphoreHandle {
    fn resource_type_key() -> Option<ResourceTypeKey>
    where
        Self: Sized,
    {
        Some(semaphore_resource_key())
    }

    fn begin_close(&mut self, _reason: ResourceCloseReason) -> ResourceResult<CloseProgress> {
        self.release();
        Ok(CloseProgress::Ready)
    }
}

impl HostResourceType for SemaphoreHandle {
    const KEY: &'static str = NGX_SEMAPHORE_RESOURCE_KEY;
    const DESCRIPTION: &'static str = "VM-local handle to a worker-owned counting semaphore";
}

struct WorkerSemaphores {
    next_id: u64,
    live: HashMap<u64, Arc<SemaphoreState>>,
}

thread_local! {
    static REGISTRY: std::cell::RefCell<WorkerSemaphores> =
        std::cell::RefCell::new(WorkerSemaphores {
            next_id: 1,
            live: HashMap::new(),
        });
}

/// Creates a worker-owned semaphore and returns its shared state.
///
/// The initial count must not exceed [`MAX_SEMAPHORE_COUNT`] and the worker
/// must not hold [`MAX_LIVE_SEMAPHORES`] live semaphores already.
pub(crate) fn create_worker_semaphore(count: u64) -> VmResult<Arc<SemaphoreState>> {
    validate_count(count, "ngx::semaphore::new")?;
    REGISTRY.with(|slot| {
        let mut registry = slot.borrow_mut();
        if registry.live.len() >= MAX_LIVE_SEMAPHORES {
            return Err(VmError::HostError(format!(
                "ngx::semaphore::new worker semaphore limit reached ({MAX_LIVE_SEMAPHORES})"
            )));
        }
        let id = registry.next_id;
        registry.next_id = registry.next_id.checked_add(1).ok_or_else(|| {
            VmError::HostError("ngx::semaphore::new exhausted worker semaphore ids".into())
        })?;
        let state = Arc::new(SemaphoreState::new(id, count));
        registry.live.insert(id, Arc::clone(&state));
        Ok(state)
    })
}

/// Looks up a live worker semaphore by id.
#[cfg(test)]
pub(crate) fn lookup_worker_semaphore(id: u64) -> Option<Arc<SemaphoreState>> {
    REGISTRY.with(|slot| slot.borrow().live.get(&id).map(Arc::clone))
}

/// Removes the worker entry for one VM-local handle. Repeated calls are no-ops.
pub(crate) fn release_worker_handle(id: u64) {
    REGISTRY.with(|slot| {
        let mut registry = slot.borrow_mut();
        registry.live.remove(&id);
    })
}

/// Live semaphores in this worker.
pub fn live_semaphore_count() -> usize {
    REGISTRY.with(|slot| slot.borrow().live.len())
}

/// Test-only view of live worker semaphore ids.
#[cfg(test)]
pub(crate) fn live_semaphore_ids() -> Vec<u64> {
    REGISTRY.with(|slot| slot.borrow().live.keys().copied().collect())
}

/// Worker shutdown: close every live semaphore, wake all waiters, empty the
/// registry. Ids are never reused. Idempotent.
pub fn shutdown_current() {
    let states = REGISTRY.with(|slot| {
        let mut registry = slot.borrow_mut();
        registry
            .live
            .drain()
            .map(|(_, state)| state)
            .collect::<Vec<_>>()
    });
    for state in states {
        state.close();
    }
}

fn validate_count(count: u64, host: &str) -> VmResult<()> {
    if count > MAX_SEMAPHORE_COUNT {
        return Err(VmError::HostError(format!(
            "{host} count must be between 0 and {MAX_SEMAPHORE_COUNT}, got {count}"
        )));
    }
    Ok(())
}

/// Validates the `new` initial count.
pub(crate) fn parse_new_count(value: i64, host: &str) -> VmResult<u64> {
    let count = u64::try_from(value).map_err(|_| {
        VmError::HostError(format!(
            "{host} expects a non-negative integer count, got {value}"
        ))
    })?;
    Ok(count)
}

/// Validates a `post` amount. `null` means one permit.
pub(crate) fn parse_post_amount(value: &Value, host: &str) -> VmResult<u64> {
    let amount = match value {
        Value::Null => 1,
        Value::Int(amount) => u64::try_from(*amount).map_err(|_| {
            VmError::HostError(format!(
                "{host} expects a non-negative integer count, got {amount}"
            ))
        })?,
        other => {
            return Err(VmError::HostError(format!(
                "{host} expects an integer count or null, got {other:?}"
            )));
        }
    };
    validate_count(amount, host)?;
    Ok(amount)
}

/// Validates a `wait` timeout. `null` waits forever; `0` never waits.
pub(crate) fn parse_wait_timeout(value: &Value, host: &str) -> VmResult<Option<Duration>> {
    match value {
        Value::Null => Ok(None),
        Value::Int(seconds) => {
            if *seconds < 0 {
                return Err(VmError::HostError(format!(
                    "{host} timeout must be non-negative seconds, got {seconds}"
                )));
            }
            Ok(Some(Duration::from_secs(*seconds as u64)))
        }
        Value::Float(seconds) => {
            if seconds.is_nan() || seconds.is_infinite() || *seconds < 0.0 {
                return Err(VmError::HostError(format!(
                    "{host} timeout must be finite non-negative seconds, got {seconds}"
                )));
            }
            Duration::try_from_secs_f64(*seconds)
                .map(Some)
                .map_err(|_| VmError::HostError(format!("{host} timeout is not a finite duration")))
        }
        other => Err(VmError::HostError(format!(
            "{host} timeout expects a number of seconds or null, got {other:?}"
        ))),
    }
}

/// Owned async capture for `ngx::semaphore::wait`.
///
/// Capture runs on the worker main thread and resolves the VM-local handle in
/// the **current** VM table. The future then holds only the worker-owned state
/// and an owned timeout.
pub(crate) struct SemaphoreWaitContext {
    state: Arc<SemaphoreState>,
    timeout: Option<Duration>,
}

impl SemaphoreWaitContext {
    #[cfg(test)]
    pub(crate) fn from_state(state: Arc<SemaphoreState>, timeout: Option<Duration>) -> Self {
        Self { state, timeout }
    }

    pub(crate) fn state(&self) -> &Arc<SemaphoreState> {
        &self.state
    }

    pub(crate) fn timeout(&self) -> Option<Duration> {
        self.timeout
    }
}

impl fmt::Debug for SemaphoreWaitContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SemaphoreWaitContext")
            .field("semaphore", &self.state.id())
            .field("timeout", &self.timeout)
            .finish()
    }
}

/// Handle lookup used by capture and by the sync hosts.
pub(crate) fn resolve_handle(
    vm: &mut Vm,
    handle_id: i64,
    host: &str,
) -> VmResult<Arc<SemaphoreState>> {
    let raw = u64::try_from(handle_id)
        .map_err(|_| VmError::HostError(format!("{host} invalid handle {handle_id}")))?;
    let handle = vm::ResourceHandle::from_raw(raw).map_err(|error| {
        VmError::HostError(format!("{host} invalid handle {handle_id}: {error}"))
    })?;
    let context = vm.host_context();
    let resource = context
        .borrow_resource::<SemaphoreHandle>(handle)
        .map_err(|error| VmError::HostError(format!("{host}: {error}")))?;
    Ok(Arc::clone(resource.state()))
}

impl CaptureAsyncHostContext for SemaphoreWaitContext {
    fn capture(_vm: &mut Vm) -> VmResult<Self> {
        Err(VmError::HostError(
            "ngx::semaphore::wait requires a semaphore handle".to_string(),
        ))
    }

    fn capture_with_args(vm: &mut Vm, args: &[Value]) -> VmResult<Self> {
        let host = "ngx::semaphore::wait";
        let handle_id = match args.first() {
            Some(Value::Int(value)) => *value,
            Some(_) => return Err(VmError::TypeMismatch("resource handle")),
            None => {
                return Err(VmError::HostError(format!(
                    "{host} missing semaphore handle"
                )));
            }
        };
        let state = resolve_handle(vm, handle_id, host)?;
        let timeout = parse_wait_timeout(args.get(1).unwrap_or(&Value::Null), host)?;
        Ok(Self { state, timeout })
    }
}

/// Awaits one permit. `true` on acquire, `false` on timeout.
pub(crate) async fn wait(context: SemaphoreWaitContext) -> VmResult<bool> {
    SemaphoreWaitFuture::new(context.state().clone(), context.timeout()).await
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WaitOutcome {
    Acquired,
    TimedOut,
}

/// Owned FIFO wait future. Holds no request, VM, pool, or raw pointer.
pub(crate) struct SemaphoreWaitFuture {
    state: Arc<SemaphoreState>,
    guard: WaiterGuard,
    timeout: Option<Duration>,
    sleep: Option<Pin<Box<tokio::time::Sleep>>>,
    done: Option<WaitOutcome>,
}

struct WaiterGuard {
    state: Arc<SemaphoreState>,
    id: u64,
}

impl Drop for WaiterGuard {
    fn drop(&mut self) {
        self.state.release_waiter(self.id);
    }
}

static NEXT_WAITER_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

impl SemaphoreWaitFuture {
    pub(crate) fn new(state: Arc<SemaphoreState>, timeout: Option<Duration>) -> Self {
        let id = NEXT_WAITER_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Self {
            guard: WaiterGuard {
                state: Arc::clone(&state),
                id,
            },
            state,
            timeout,
            sleep: None,
            done: None,
        }
    }

    fn finish(&mut self, outcome: WaitOutcome) -> VmResult<bool> {
        self.done = Some(outcome);
        match outcome {
            WaitOutcome::Acquired => {
                // The permit was consumed at grant time and the waiter already
                // left the queue, so cleanup must not restore anything.
            }
            WaitOutcome::TimedOut => self.state.release_waiter(self.guard.id),
        }
        // Disarm exactly-once cleanup: a later drop is a pure no-op.
        self.guard.id = 0;
        match outcome {
            WaitOutcome::Acquired => Ok(true),
            WaitOutcome::TimedOut => Ok(false),
        }
    }
}

impl std::future::Future for SemaphoreWaitFuture {
    type Output = VmResult<bool>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        if this.done.is_some() {
            return Poll::Ready(Err(VmError::HostError(
                "ngx::semaphore::wait future polled after completion".to_string(),
            )));
        }
        match this.state.acquire_or_register(this.guard.id, cx.waker()) {
            Acquire::Granted => return Poll::Ready(this.finish(WaitOutcome::Acquired)),
            Acquire::Closed => {
                return Poll::Ready(Err(VmError::HostError(
                    "ngx::semaphore::wait semaphore is closed: worker is shutting down".to_string(),
                )));
            }
            Acquire::Pending => {}
        }
        if let Some(timeout) = this.timeout {
            let sleep = this
                .sleep
                .get_or_insert_with(|| Box::pin(tokio::time::sleep(timeout)));
            if sleep.as_mut().poll(cx).is_ready() {
                return Poll::Ready(this.finish(WaitOutcome::TimedOut));
            }
        }
        Poll::Pending
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::Future;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::task::Wake;
    use std::time::Instant;

    struct CountingWaker {
        wakes: Arc<AtomicUsize>,
    }

    impl Wake for CountingWaker {
        fn wake(self: Arc<Self>) {
            self.wakes.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn counting_waker() -> (Waker, Arc<AtomicUsize>) {
        let wakes = Arc::new(AtomicUsize::new(0));
        let waker = Waker::from(Arc::new(CountingWaker {
            wakes: Arc::clone(&wakes),
        }));
        (waker, wakes)
    }

    fn fresh_state(count: u64) -> Arc<SemaphoreState> {
        create_worker_semaphore(count).expect("create worker semaphore")
    }

    #[test]
    fn creates_worker_semaphore_with_validated_initial_count() {
        let state = fresh_state(3);
        assert_eq!(state.count().expect("count"), 3);
        assert_eq!(state.waiting_count(), 0);
        assert_eq!(live_semaphore_count(), 1);
        assert_eq!(live_semaphore_ids(), vec![state.id()]);
        shutdown_current();
    }

    #[test]
    fn new_rejects_count_above_the_permit_ceiling() {
        let err = create_worker_semaphore(MAX_SEMAPHORE_COUNT + 1).expect_err("ceiling");
        let text = err.to_string();
        assert!(text.contains("1000000"), "{text}");
        assert_eq!(live_semaphore_count(), 0, "rejected create must not leak");
        let err = parse_new_count(-1, "ngx::semaphore::new").expect_err("negative");
        assert!(err.to_string().contains("non-negative"), "{err}");
    }

    #[test]
    fn post_increments_and_zero_post_is_a_noop() {
        let state = fresh_state(0);
        let waiter = SemaphoreWaitFuture::new(Arc::clone(&state), None);
        let (waker, wakes) = counting_waker();
        let mut waiter = Box::pin(waiter);
        let mut cx = Context::from_waker(&waker);
        assert!(matches!(waiter.as_mut().poll(&mut cx), Poll::Pending));
        assert_eq!(state.waiting_count(), 1);

        assert_eq!(state.post(0).expect("zero post"), 0);
        assert_eq!(wakes.load(Ordering::SeqCst), 0, "zero post wakes nobody");

        assert_eq!(state.post(2).expect("post"), 2);
        assert_eq!(state.count().expect("count"), 1);
        assert_eq!(
            wakes.load(Ordering::SeqCst),
            1,
            "post wakes the front waiter"
        );
        assert!(matches!(
            waiter.as_mut().poll(&mut cx),
            Poll::Ready(Ok(true))
        ));
        assert_eq!(state.count().expect("count"), 1);
        assert_eq!(state.waiting_count(), 0);
        shutdown_current();
    }

    #[test]
    fn post_rejects_overflow_past_the_permit_ceiling() {
        let state = fresh_state(MAX_SEMAPHORE_COUNT - 1);
        assert_eq!(state.post(1).expect("exact ceiling"), MAX_SEMAPHORE_COUNT);
        let err = state.post(1).expect_err("overflow");
        assert!(err.to_string().contains("ceiling"), "{err}");
        assert_eq!(state.count().expect("count"), MAX_SEMAPHORE_COUNT as i64);
        shutdown_current();
    }

    #[test]
    fn wait_acquires_an_available_permit_without_queueing() {
        let state = fresh_state(1);
        let mut waiter = Box::pin(SemaphoreWaitFuture::new(Arc::clone(&state), None));
        let (waker, wakes) = counting_waker();
        let mut cx = Context::from_waker(&waker);
        assert!(matches!(
            waiter.as_mut().poll(&mut cx),
            Poll::Ready(Ok(true))
        ));
        assert_eq!(state.count().expect("count"), 0);
        assert_eq!(
            state.waiting_count(),
            0,
            "a granted waiter leaves no queue entry"
        );
        assert_eq!(wakes.load(Ordering::SeqCst), 0);
        let err = match waiter.as_mut().poll(&mut cx) {
            Poll::Ready(Err(err)) => err,
            other => panic!("expected a fused completion error, got {other:?}"),
        };
        assert!(err.to_string().contains("after completion"), "{err}");
        shutdown_current();
    }

    #[test]
    fn wait_pends_then_post_wakes_and_grants() {
        let state = fresh_state(0);
        let mut waiter = Box::pin(SemaphoreWaitFuture::new(Arc::clone(&state), None));
        let (waker, wakes) = counting_waker();
        let mut cx = Context::from_waker(&waker);
        assert!(matches!(waiter.as_mut().poll(&mut cx), Poll::Pending));
        assert_eq!(state.post(1).expect("post"), 1);
        assert_eq!(wakes.load(Ordering::SeqCst), 1);
        assert!(matches!(
            waiter.as_mut().poll(&mut cx),
            Poll::Ready(Ok(true))
        ));
        assert_eq!(state.count().expect("count"), 0);
        shutdown_current();
    }

    #[test]
    fn count_reports_negative_fifo_waiter_depth() {
        let state = fresh_state(0);
        let (waker, _) = counting_waker();
        let mut cx = Context::from_waker(&waker);
        let mut first = Box::pin(SemaphoreWaitFuture::new(Arc::clone(&state), None));
        let mut second = Box::pin(SemaphoreWaitFuture::new(Arc::clone(&state), None));
        assert!(matches!(first.as_mut().poll(&mut cx), Poll::Pending));
        assert!(matches!(second.as_mut().poll(&mut cx), Poll::Pending));
        assert_eq!(state.count().expect("count"), -2);
        assert_eq!(state.post(1).expect("post"), 1);
        assert!(matches!(
            first.as_mut().poll(&mut cx),
            Poll::Ready(Ok(true))
        ));
        assert_eq!(state.count().expect("count"), -1);
        shutdown_current();
    }

    #[test]
    fn fifo_order_grants_the_oldest_waiter_first() {
        let state = fresh_state(0);
        let (waker_one, wakes_one) = counting_waker();
        let (waker_two, wakes_two) = counting_waker();
        let (waker_three, wakes_three) = counting_waker();
        let mut first = Box::pin(SemaphoreWaitFuture::new(Arc::clone(&state), None));
        let mut second = Box::pin(SemaphoreWaitFuture::new(Arc::clone(&state), None));
        let mut third = Box::pin(SemaphoreWaitFuture::new(Arc::clone(&state), None));
        let mut cx_one = Context::from_waker(&waker_one);
        let mut cx_two = Context::from_waker(&waker_two);
        let mut cx_three = Context::from_waker(&waker_three);
        assert!(matches!(first.as_mut().poll(&mut cx_one), Poll::Pending));
        assert!(matches!(second.as_mut().poll(&mut cx_two), Poll::Pending));
        assert!(matches!(third.as_mut().poll(&mut cx_three), Poll::Pending));
        assert_eq!(state.waiting_count(), 3);

        assert_eq!(state.post(1).expect("post"), 1);
        assert_eq!(wakes_one.load(Ordering::SeqCst), 1);
        assert_eq!(
            wakes_two.load(Ordering::SeqCst),
            0,
            "one permit wakes one waiter"
        );
        assert!(matches!(second.as_mut().poll(&mut cx_two), Poll::Pending));
        assert!(
            matches!(first.as_mut().poll(&mut cx_one), Poll::Ready(Ok(true))),
            "the oldest waiter must win the only permit"
        );
        assert!(matches!(third.as_mut().poll(&mut cx_three), Poll::Pending));
        assert_eq!(state.count().expect("count"), -2);

        assert_eq!(state.post(1).expect("post"), 1);
        assert_eq!(wakes_two.load(Ordering::SeqCst), 1);
        assert_eq!(wakes_three.load(Ordering::SeqCst), 0);
        assert!(matches!(
            second.as_mut().poll(&mut cx_two),
            Poll::Ready(Ok(true))
        ));
        assert!(matches!(third.as_mut().poll(&mut cx_three), Poll::Pending));
        assert_eq!(state.post(1).expect("post"), 1);
        assert!(matches!(
            third.as_mut().poll(&mut cx_three),
            Poll::Ready(Ok(true))
        ));
        assert_eq!(state.waiting_count(), 0);
        shutdown_current();
    }

    #[test]
    fn dropping_the_front_waiter_hands_the_queued_permit_to_the_next() {
        let state = fresh_state(0);
        let (waker_one, _) = counting_waker();
        let (waker_two, wakes_two) = counting_waker();
        let mut first = Box::pin(SemaphoreWaitFuture::new(Arc::clone(&state), None));
        let mut second = Box::pin(SemaphoreWaitFuture::new(Arc::clone(&state), None));
        let mut cx_one = Context::from_waker(&waker_one);
        let mut cx_two = Context::from_waker(&waker_two);
        assert!(matches!(first.as_mut().poll(&mut cx_one), Poll::Pending));
        assert!(matches!(second.as_mut().poll(&mut cx_two), Poll::Pending));
        assert_eq!(state.post(1).expect("post"), 1);
        // The front waiter was woken but never polls again: cancellation.
        drop(first);
        assert_eq!(state.waiting_count(), 1);
        assert_eq!(
            state.count().expect("count"),
            0,
            "one permit is ready for the remaining waiter"
        );
        assert_eq!(
            wakes_two.load(Ordering::SeqCst),
            1,
            "cancelling the front waiter must wake the next one"
        );
        assert!(matches!(
            second.as_mut().poll(&mut cx_two),
            Poll::Ready(Ok(true))
        ));
        assert_eq!(state.count().expect("count"), 0);
        assert_eq!(state.waiting_count(), 0);
        shutdown_current();
    }

    #[test]
    fn granted_waiter_drop_does_not_restore_a_permit() {
        let state = fresh_state(1);
        let mut waiter = Box::pin(SemaphoreWaitFuture::new(Arc::clone(&state), None));
        let (waker, _) = counting_waker();
        let mut cx = Context::from_waker(&waker);
        assert!(matches!(
            waiter.as_mut().poll(&mut cx),
            Poll::Ready(Ok(true))
        ));
        assert_eq!(state.count().expect("count"), 0);
        drop(waiter);
        assert_eq!(
            state.count().expect("count"),
            0,
            "cleanup must not release a granted permit"
        );
        assert_eq!(state.waiting_count(), 0);
        shutdown_current();
    }

    /// Polls until the future is ready or `limit` elapses. The timer needs a
    /// live Tokio time driver, exactly like the worker bridge enters it.
    fn poll_until_ready(
        future: &mut Pin<Box<SemaphoreWaitFuture>>,
        cx: &mut Context<'_>,
        limit: Duration,
    ) -> Poll<VmResult<bool>> {
        let start = Instant::now();
        loop {
            let polled = future.as_mut().poll(cx);
            if !matches!(polled, Poll::Pending) || start.elapsed() >= limit {
                return polled;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn zero_timeout_parses_as_immediate_no_wait() {
        // OpenResty `sem:wait(0)` is an immediate acquisition attempt.
        assert_eq!(
            parse_wait_timeout(&Value::Int(0), "ngx::semaphore::wait").unwrap(),
            Some(Duration::ZERO)
        );
        assert_eq!(
            parse_wait_timeout(&Value::Float(0.0), "ngx::semaphore::wait").unwrap(),
            Some(Duration::ZERO)
        );
        assert_eq!(
            parse_wait_timeout(&Value::Float(0.25), "ngx::semaphore::wait").unwrap(),
            Some(Duration::from_millis(250))
        );
    }

    #[test]
    fn zero_timeout_fails_closed_without_consuming_a_permit() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_time()
            .build()
            .expect("runtime");
        let _enter = runtime.enter();
        let state = fresh_state(0);
        let mut waiter = Box::pin(SemaphoreWaitFuture::new(
            Arc::clone(&state),
            Some(Duration::ZERO),
        ));
        let (waker, _) = counting_waker();
        let mut cx = Context::from_waker(&waker);
        let polled = poll_until_ready(&mut waiter, &mut cx, Duration::from_secs(5));
        assert!(
            matches!(polled, Poll::Ready(Ok(false))),
            "a zero timeout must time out without a permit, got {polled:?}"
        );
        assert_eq!(state.count().expect("count"), 0);
        assert_eq!(state.waiting_count(), 0);
        drop(waiter);
        shutdown_current();
    }

    #[test]
    fn wait_returns_false_on_timeout() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_time()
            .build()
            .expect("runtime");
        let state = fresh_state(0);
        let outcome = runtime
            .block_on(wait(SemaphoreWaitContext::from_state(
                Arc::clone(&state),
                Some(Duration::ZERO),
            )))
            .expect("wait result");
        assert!(!outcome, "timeout must remain false at the host boundary");
        assert_eq!(state.count().expect("count"), 0);
        shutdown_current();
    }

    #[test]
    fn elapsed_timeout_returns_false_and_removes_the_waiter() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_time()
            .build()
            .expect("runtime");
        let _enter = runtime.enter();
        let state = fresh_state(0);
        let mut waiter = Box::pin(SemaphoreWaitFuture::new(
            Arc::clone(&state),
            Some(Duration::from_millis(20)),
        ));
        let (waker, wakes) = counting_waker();
        let mut cx = Context::from_waker(&waker);
        let start = Instant::now();
        assert!(matches!(waiter.as_mut().poll(&mut cx), Poll::Pending));
        assert_eq!(state.waiting_count(), 1);
        let polled = poll_until_ready(&mut waiter, &mut cx, Duration::from_secs(5));
        assert!(
            matches!(polled, Poll::Ready(Ok(false))),
            "an elapsed timeout must report false, got {polled:?}"
        );
        assert!(
            wakes.load(Ordering::SeqCst) >= 1,
            "the timer must wake the parked waiter"
        );
        assert!(
            start.elapsed() >= Duration::from_millis(20),
            "the timeout must not fire early"
        );
        assert_eq!(
            state.waiting_count(),
            0,
            "timeout must deregister the waiter"
        );
        assert_eq!(
            state.count().expect("count"),
            0,
            "timeout must not consume a permit"
        );
        assert_eq!(state.post(1).expect("post"), 1);
        shutdown_current();
    }

    #[test]
    fn worker_shutdown_fails_parked_waiters_closed() {
        let state = fresh_state(0);
        let mut waiter = Box::pin(SemaphoreWaitFuture::new(Arc::clone(&state), None));
        let (waker, wakes) = counting_waker();
        let mut cx = Context::from_waker(&waker);
        assert!(matches!(waiter.as_mut().poll(&mut cx), Poll::Pending));
        shutdown_current();
        assert_eq!(live_semaphore_count(), 0);
        assert_eq!(
            wakes.load(Ordering::SeqCst),
            1,
            "shutdown wakes every waiter"
        );
        let polled = waiter.as_mut().poll(&mut cx);
        let err = match polled {
            Poll::Ready(Err(err)) => err,
            other => panic!("expected a closed error, got {other:?}"),
        };
        assert!(err.to_string().contains("closed"), "{err}");
        assert_eq!(
            state.waiting_count(),
            0,
            "closed poll deregisters the waiter"
        );
        shutdown_current();
    }

    #[test]
    fn close_drains_waiter_queue_immediately() {
        let state = fresh_state(0);
        let mut waiter = Box::pin(SemaphoreWaitFuture::new(Arc::clone(&state), None));
        let (waker, wakes) = counting_waker();
        let mut cx = Context::from_waker(&waker);
        assert!(matches!(waiter.as_mut().poll(&mut cx), Poll::Pending));

        state.close();
        assert_eq!(state.waiting_count(), 0);
        assert_eq!(wakes.load(Ordering::SeqCst), 1);
        shutdown_current();
    }

    #[test]
    fn closed_semaphore_rejects_post_and_count() {
        let state = fresh_state(2);
        state.close();
        let err = state.post(1).expect_err("closed post");
        assert!(err.to_string().contains("closed"), "{err}");
        let err = state.count().expect_err("closed count");
        assert!(err.to_string().contains("closed"), "{err}");
        shutdown_current();
    }

    #[test]
    fn handle_release_removes_the_worker_entry() {
        let state = fresh_state(1);
        let id = state.id();
        assert!(lookup_worker_semaphore(id).is_some());
        release_worker_handle(id);
        assert_eq!(live_semaphore_count(), 0, "handle release drops the entry");
        assert!(lookup_worker_semaphore(id).is_none());
        release_worker_handle(id);
        assert_eq!(live_semaphore_count(), 0, "double release is a no-op");
        shutdown_current();
    }

    #[test]
    fn registry_cap_is_enforced_per_worker() {
        for _ in 0..MAX_LIVE_SEMAPHORES {
            fresh_state(0);
        }
        assert_eq!(live_semaphore_count(), MAX_LIVE_SEMAPHORES);
        let err = create_worker_semaphore(0).expect_err("worker cap");
        assert!(err.to_string().contains("limit reached"), "{err}");
        assert_eq!(live_semaphore_count(), MAX_LIVE_SEMAPHORES);
        shutdown_current();
        assert_eq!(live_semaphore_count(), 0);
    }

    #[test]
    fn shutdown_empties_the_registry_without_reusing_ids() {
        let first = fresh_state(0);
        let first_id = first.id();
        shutdown_current();
        assert_eq!(live_semaphore_count(), 0);
        let second = fresh_state(1);
        assert_ne!(second.id(), first_id, "worker ids are never reused");
        assert_eq!(second.count().expect("count"), 1);
        shutdown_current();
    }

    #[test]
    fn dropping_an_unadmitted_handle_releases_the_worker_entry() {
        let state = fresh_state(2);
        let handle = SemaphoreHandle::new(Arc::clone(&state));
        drop(handle);
        assert_eq!(
            live_semaphore_count(),
            0,
            "a failed VM-table insertion must not leak worker-owned state"
        );
        assert!(lookup_worker_semaphore(state.id()).is_none());
        shutdown_current();
    }

    #[test]
    fn handle_resource_type_key_and_close_are_idempotent() {
        let state = fresh_state(2);
        let mut handle = SemaphoreHandle::new(Arc::clone(&state));
        assert_eq!(handle.id(), state.id());
        let key = <SemaphoreHandle as HostResource>::resource_type_key().expect("typed key");
        assert_eq!(key.as_str(), NGX_SEMAPHORE_RESOURCE_KEY);
        assert_eq!(
            handle
                .begin_close(ResourceCloseReason::Requested)
                .expect("close"),
            CloseProgress::Ready
        );
        assert_eq!(live_semaphore_count(), 0);
        assert_eq!(
            handle
                .begin_close(ResourceCloseReason::Requested)
                .expect("second close"),
            CloseProgress::Ready,
            "a second close must stay idempotent"
        );
        assert_eq!(live_semaphore_count(), 0);
    }

    #[test]
    fn semaphore_boundary_types_are_send() {
        fn assert_send<T: Send + 'static>() {}
        assert_send::<SemaphoreWaitFuture>();
        assert_send::<SemaphoreWaitContext>();
        assert_send::<SemaphoreHandle>();
    }

    #[test]
    fn pooled_vm_reuse_closes_semaphore_handles() {
        crate::vm_pool::reset_test_state();
        shutdown_current();
        // `vm_pool::run` exposes terminal status only; a mismatch raises a
        // runtime error before the expected sentinel reaches `Halted`.
        let program = crate::config::compile_rss_source(
            r#"use ngx;
let s = ngx::semaphore::new(1);
ngx::semaphore::post(s, 2);
let observed = ngx::semaphore::count(s);
let sentinel = 3;
if observed != sentinel {
    1 / 0;
}
sentinel;
"#,
        )
        .expect("compile pooled semaphore program");
        assert!(!crate::config::program_requires_request_state(&program));

        for _ in 0..2 {
            assert_eq!(
                crate::vm_pool::run(&program).expect("pooled run"),
                vm::VmStatus::Halted
            );
            assert_eq!(
                live_semaphore_count(),
                0,
                "pool reset must close the VM-local semaphore handle"
            );
        }
        assert_eq!(crate::vm_pool::created_count(), 1);
        crate::vm_pool::reset_test_state();
        assert_eq!(
            live_semaphore_count(),
            0,
            "resetting the reused pool must leave the worker registry empty"
        );
        shutdown_current();
        assert_eq!(live_semaphore_count(), 0);
    }

    #[test]
    fn raw_handle_from_another_vm_is_rejected() {
        let state = fresh_state(1);
        let owner_program = vm::compile_source("let x = 1;\n").expect("compile owner");
        let mut owner = vm::Vm::try_new(owner_program.program).expect("owner VM");
        let token = owner
            .host_context()
            .push_resource(SemaphoreHandle::new(Arc::clone(&state)))
            .expect("push owner handle");
        let raw = token.handle().raw() as i64;

        let foreign_program = vm::compile_source("let x = 1;\n").expect("compile foreign");
        let mut foreign = vm::Vm::try_new(foreign_program.program).expect("foreign VM");
        let err = resolve_handle(&mut foreign, raw, "ngx::semaphore::count")
            .expect_err("foreign VM must reject an arena-bound handle");
        assert!(
            err.to_string().contains("resource_handle_wrong_table"),
            "{err}"
        );

        drop(owner);
        assert_eq!(live_semaphore_count(), 0);
        shutdown_current();
    }

    #[test]
    fn capture_validates_handle_and_timeout_on_the_current_vm() {
        let compiled = vm::compile_source("let x = 1;\n").expect("compile");
        let mut vm = vm::Vm::try_new(compiled.program).expect("vm");
        let state = fresh_state(1);
        let token = vm
            .host_context()
            .push_resource(SemaphoreHandle::new(Arc::clone(&state)))
            .expect("push handle");
        let raw = token.handle().raw() as i64;

        let local =
            SemaphoreWaitContext::capture_with_args(&mut vm, &[Value::Int(raw), Value::Float(0.5)])
                .expect("capture with a live handle");
        assert_eq!(local.timeout(), Some(Duration::from_millis(500)));

        let defaulted = SemaphoreWaitContext::capture_with_args(&mut vm, &[Value::Int(raw)])
            .expect("missing timeout means wait forever");
        assert_eq!(defaulted.timeout(), None);

        let err = SemaphoreWaitContext::capture_with_args(&mut vm, &[Value::Int(0)])
            .expect_err("handle 0 is invalid");
        assert!(err.to_string().contains("invalid handle"), "{err}");

        let err = SemaphoreWaitContext::capture_with_args(
            &mut vm,
            &[Value::Int(raw), Value::String(Arc::new("-1".to_string()))],
        )
        .expect_err("string timeout");
        assert!(err.to_string().contains("timeout"), "{err}");

        SemaphoreWaitContext::capture(&mut vm).expect_err("context requires args");

        assert!(parse_wait_timeout(&Value::Int(-1), "h").is_err());
        assert_eq!(parse_wait_timeout(&Value::Null, "h").unwrap(), None);
        assert_eq!(
            parse_wait_timeout(&Value::Int(0), "h").unwrap(),
            Some(Duration::ZERO)
        );
        assert_eq!(
            parse_wait_timeout(&Value::Int(2), "h").unwrap(),
            Some(Duration::from_secs(2))
        );
        assert!(parse_post_amount(&Value::Null, "h").is_ok());
        assert_eq!(parse_post_amount(&Value::Null, "h").unwrap(), 1);
        assert!(parse_post_amount(&Value::Float(1.0), "h").is_err());
        shutdown_current();
    }

    /// Minimal VM with production ngx hosts, a worker bridge, and the
    /// semaphore program already parked on its wait.
    fn parked_wait_vm(
        runtime: &tokio::runtime::Runtime,
    ) -> (vm::Vm, Arc<SemaphoreState>, vm::HostOpId) {
        let program = crate::config::compile_rss_source(
            "use ngx;\nlet s = ngx::semaphore::new(0);\nngx::semaphore::wait(s, null);\n",
        )
        .expect("compile semaphore wait");
        let mut vm = vm::Vm::try_new((*program).clone()).expect("vm");
        crate::host::bind_ngx_hosts(&mut vm);
        vm.set_async_bridge(Box::new(crate::async_bridge::NgxAsyncBridge::new(
            runtime.handle().clone(),
        )))
        .expect("async bridge");
        let status = vm.run().expect("run");
        let vm::VmStatus::Waiting(op_id) = status else {
            panic!("expected Waiting, got {status:?}");
        };
        let ids = live_semaphore_ids();
        assert_eq!(ids.len(), 1, "one worker semaphore is live");
        let state = lookup_worker_semaphore(ids[0]).expect("worker state");
        (vm, state, op_id)
    }

    fn wait_for_eventfd(fd: std::os::unix::io::RawFd) {
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(2) {
            let mut pfd = libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            };
            if unsafe { libc::poll(&mut pfd, 1, 20) } > 0 {
                return;
            }
        }
        panic!("eventfd was never written");
    }

    #[test]
    fn worker_post_resumes_a_parked_vm_wait_through_the_bridge() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_time()
            .build()
            .expect("runtime");
        let (mut vm, state, _op_id) = parked_wait_vm(&runtime);

        let notify = crate::notify::NotifyBridge::new().expect("notify");
        let identity = crate::notify::RequestIdentity::new(1, 1);
        let waker = crate::notify::eventfd_waker(notify.sender(), identity);
        let mut cx = Context::from_waker(&waker);
        assert!(matches!(vm.poll_waiting_host_op(&mut cx), Poll::Pending));
        assert_eq!(
            state.waiting_count(),
            1,
            "the parked VM wait must own one FIFO slot"
        );

        // A worker-side post (another request or the same worker thread) grants
        // the permit; the waker only queues the identity.
        assert_eq!(state.post(1).expect("post"), 1);
        wait_for_eventfd(notify.raw_fd().expect("eventfd"));
        assert_eq!(
            notify.harvest_and_take_ready().expect("harvest"),
            vec![identity]
        );

        let noop = Waker::noop();
        let mut cx = Context::from_waker(noop);
        assert!(matches!(
            vm.poll_waiting_host_op(&mut cx),
            Poll::Ready(Ok(()))
        ));
        assert_eq!(vm.resume().expect("resume"), vm::VmStatus::Halted);
        assert_eq!(
            state.count().expect("count"),
            0,
            "the granted permit is consumed"
        );
        assert_eq!(state.waiting_count(), 0, "the grant removed the FIFO entry");
        assert_eq!(
            live_semaphore_count(),
            1,
            "the VM resource table still owns the handle"
        );
        shutdown_current();
    }

    #[test]
    fn typed_bridge_cancellation_drops_the_parked_waiter_exactly_once() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_time()
            .build()
            .expect("runtime");
        let state = fresh_state(0);
        let compiled = vm::compile_source("let x = 1;\n").expect("compile");
        let mut vm = vm::Vm::try_new(compiled.program).expect("vm");
        let token = vm
            .host_context()
            .push_resource(SemaphoreHandle::new(Arc::clone(&state)))
            .expect("push handle");
        let raw = token.handle().raw() as i64;
        let context =
            SemaphoreWaitContext::capture_with_args(&mut vm, &[Value::Int(raw), Value::Null])
                .expect("capture");
        let future: vm::HostFuture = Box::pin(async move {
            crate::semaphore::wait(context)
                .await
                .map(|_| vm::HostFutureOutput::returning(vm::CallReturn::none()))
        });
        use vm::HostAsyncBridge as _;

        let mut bridge = crate::async_bridge::NgxAsyncBridge::new(runtime.handle().clone());
        bridge.submit_op(7, future).expect("submit");
        let noop = Waker::noop();
        let mut cx = Context::from_waker(noop);
        assert!(matches!(
            bridge.poll_submitted_op(7, &mut cx),
            Poll::Pending
        ));
        assert_eq!(state.waiting_count(), 1);

        bridge
            .request_cancel_op(7, vm::operation::OperationCancelReason::Deadline)
            .expect("typed cancellation");
        assert!(
            matches!(bridge.poll_cancel_op(7, &mut cx), Poll::Ready(Ok(()))),
            "the typed cancel protocol acknowledges the dropped future"
        );
        assert_eq!(
            state.waiting_count(),
            0,
            "cancellation must deregister the FIFO entry exactly once"
        );
        assert_eq!(
            state.count().expect("count"),
            0,
            "cancellation must not consume or release a permit"
        );
        assert_eq!(state.post(1).expect("post"), 1);
        shutdown_current();
    }

    #[test]
    fn worker_shutdown_fails_a_parked_vm_wait_closed() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_time()
            .build()
            .expect("runtime");
        let (mut vm, state, _op_id) = parked_wait_vm(&runtime);
        let notify = crate::notify::NotifyBridge::new().expect("notify");
        let waker = crate::notify::eventfd_waker(
            notify.sender(),
            crate::notify::RequestIdentity::new(2, 1),
        );
        let mut cx = Context::from_waker(&waker);
        assert!(matches!(vm.poll_waiting_host_op(&mut cx), Poll::Pending));

        shutdown_current();
        assert_eq!(live_semaphore_count(), 0, "shutdown empties the registry");

        let noop = Waker::noop();
        let mut cx = Context::from_waker(noop);
        match vm.poll_waiting_host_op(&mut cx) {
            Poll::Ready(Err(err)) => {
                assert!(err.to_string().contains("closed"), "{err}");
            }
            other => panic!("a closed semaphore must not stay pending: {other:?}"),
        }
        assert_eq!(state.waiting_count(), 0);
        shutdown_current();
    }
}
