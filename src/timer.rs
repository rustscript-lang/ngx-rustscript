//! Worker-owned OpenResty-style timer runtime for `ngx::timer::{at,every}`.
//!
//! OpenResty timers are worker-local objects that outlive the request that
//! created them. Here the accepted registrations ("fake requests") live in a
//! worker-owned `thread_local` registry; every registration owns one ordinary
//! [`RequestIdentity`] and one [`OwnedTimerCallback`] with its private callback
//! VM. There is **no** second identity type, no second eventfd, and no second
//! completion queue: the deadline task holds only a [`CompletionSender`] and
//! that identity, and it wakes through the same worker eventfd queue as
//! `ngx::sleep` and the semaphore family.
//!
//! # Threading
//!
//! - Registration, callback execution, polling, and shutdown run on the NGINX
//!   worker main thread only. Script bytecode and NGINX FFI never leave it.
//! - The Tokio task spawned per deadline only sleeps and enqueues the identity;
//!   it holds no VM, no request, no pool, and no raw pointer.
//! - A callback that needs to wait (`ngx::sleep`, outbound async I/O) suspends
//!   on its **own** `NgxAsyncBridge`, installed before the first round; the
//!   wake carries the same timer identity.
//!
//! # Lifecycle
//!
//! - An accepted registration survives its creating request. Request cleanup
//!   owns no timer state.
//! - `every` serializes its rounds: the next deadline is scheduled only after
//!   the previous round reaches a terminal state, so a slow callback never
//!   overlaps itself, and its captured cells persist between rounds.
//! - Worker `exit_process` runs pending callbacks once with `premature=true`,
//!   cancels active callbacks, prevents further `every` rounds, and releases
//!   every callback VM exactly once.
//! - A configuration reload does not migrate timers: the old workers keep
//!   their registrations and new workers start with zero timers (`thread_local`
//!   worker state, ids never reused).

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use tokio::runtime::Handle;
use vm::{
    HttpConfig, HttpHostExt, IoHostExt, IoPolicy, OwnedTimerCallback, SqliteHostExt, SqlitePolicy,
    TimerBackend, TimerCallbackError, TimerCallbackState, TimerConfig, TimerHostExt,
    TimerRegistration, Vm, VmError, VmResult,
};

use crate::async_bridge::NgxAsyncBridge;
use crate::host::{AllowedOps, NgxPhase, StandardHostPolicy, ngx_host_registry};
use crate::notify::{CompletionSender, RequestIdentity, eventfd_waker};

/// Maximum live timer registrations ("fake requests") per nginx worker.
///
/// The installed [`TimerConfig`] additionally caps registrations per runtime
/// ([`vm::DEFAULT_MAX_PENDING_TIMERS`] = 1024, [`vm::DEFAULT_MAX_RUNNING_TIMERS`]
/// = 256 by default); this constant is the hard worker pool bound the backend
/// enforces under the same critical section as its insertion.
pub const MAX_LIVE_TIMER_REQUESTS: usize = 1024;

/// Identity generation reserved for timer registrations.
///
/// Request scopes always allocate generation `>= 1` (`allocate_scope_identity`
/// wraps back to 1), so a timer identity can never collide with a request
/// identity and the two dispatch paths stay disjoint. Timer ids are never
/// reused within one worker process.
const TIMER_GENERATION: u64 = 0;

/// Upper bound of immediate `Yielded` resumptions inside one round.
const MAX_YIELD_RESUMES_PER_ROUND: usize = 64;

/// Deadline scheduler handed to the worker registry.
#[derive(Clone)]
struct TimerScheduler {
    handle: Handle,
    sender: CompletionSender,
}

/// Round state of one accepted registration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RoundPhase {
    /// Waiting for its first (or next) deadline, or for a running slot.
    Pending,
    /// One callback round is live (running or suspended on an async step).
    Live,
}

/// One accepted registration: the backend-owned "fake request".
struct TimerEntry {
    /// Worker-unique registration id, never reused.
    id: u64,
    identity: RequestIdentity,
    callback: OwnedTimerCallback,
    /// `Some` for `ngx::timer::every`.
    interval: Option<Duration>,
    max_running: usize,
    phase: RoundPhase,
    /// True when the deadline fired while no running slot was free.
    due: bool,
}

struct WorkerTimers {
    scheduler: TimerScheduler,
    /// Worker-lifetime backend handle.
    ///
    /// Callback VMs hold only a weak reference to their backend (pd-vm's
    /// `TimerHostState::callback`), so the strong reference must outlive every
    /// creating request: it lives here, next to the worker registry.
    backend: Arc<NgxTimerBackend>,
    next_id: u64,
    entries: HashMap<RequestIdentity, Rc<RefCell<TimerEntry>>>,
    closed: bool,
    /// Live callback rounds, including callbacks suspended on async host work.
    running: usize,
    /// Callback failures reported through the backend sink.
    callback_errors: usize,
}

thread_local! {
    static TIMERS: RefCell<Option<WorkerTimers>> = const { RefCell::new(None) };
}

/// Links the worker timer registry to the worker runtime.
///
/// Called by [`crate::runtime::WorkerRuntime`] once per worker process after
/// the Tokio runtime and the eventfd bridge exist. Re-installing replaces the
/// previous registry (worker restart in tests).
pub(crate) fn install_worker_timers(handle: Handle, sender: CompletionSender) {
    TIMERS.with(|slot| {
        *slot.borrow_mut() = Some(WorkerTimers {
            scheduler: TimerScheduler { handle, sender },
            backend: Arc::new(NgxTimerBackend),
            next_id: 1,
            entries: HashMap::new(),
            closed: false,
            running: 0,
            callback_errors: 0,
        });
    });
}

/// True when `program` imports any `ngx::timer` host.
///
/// Such programs are bound from [`ngx_host_registry`] (owned dispatch) and must
/// run on the request-owned VM path, so configuration marks them as requiring
/// request state.
pub fn program_uses_ngx_timer_hosts(program: &vm::Program) -> bool {
    let names = crate::host::ngx_timer_host_names();
    program
        .imports
        .iter()
        .any(|import| names.contains(&import.name.as_str()))
}

/// The worker-lifetime timer backend, if the timer runtime is installed.
///
/// Every timer-capable VM installs this shared backend so callback VMs keep
/// working after the creating request (and its VM) is gone.
pub fn worker_backend() -> Option<Arc<dyn TimerBackend>> {
    TIMERS.with(|slot| {
        slot.borrow()
            .as_ref()
            .map(|state| Arc::clone(&state.backend) as Arc<dyn TimerBackend>)
    })
}

/// Live registrations in this worker (pending plus running).
pub fn live_timer_count() -> usize {
    TIMERS.with(|slot| {
        slot.borrow()
            .as_ref()
            .map_or(0, |state| state.entries.len())
    })
}

/// Callback failures reported so far in this worker.
pub fn callback_error_count() -> usize {
    TIMERS.with(|slot| {
        slot.borrow()
            .as_ref()
            .map_or(0, |state| state.callback_errors)
    })
}

/// Identities of the live timer registrations in this worker, in id order.
///
/// Every registration owns one ordinary [`RequestIdentity`] (timer generation);
/// timers never allocate a second identity type.
pub fn live_timer_identities() -> Vec<RequestIdentity> {
    TIMERS.with(|slot| {
        slot.borrow()
            .as_ref()
            .map(|state| {
                let mut entries: Vec<(u64, RequestIdentity)> = state
                    .entries
                    .values()
                    .filter_map(|entry| {
                        let entry = entry.try_borrow().ok()?;
                        Some((entry.id, entry.identity))
                    })
                    .collect();
                entries.sort_by_key(|(id, _)| *id);
                entries.into_iter().map(|(_, identity)| identity).collect()
            })
            .unwrap_or_default()
    })
}

fn with_timers<R>(f: impl FnOnce(&mut WorkerTimers) -> R) -> Option<R> {
    TIMERS.with(|slot| slot.borrow_mut().as_mut().map(f))
}

/// Worker shutdown: close registration, run pending callbacks once with
/// `premature=true`, cancel live callbacks, and release every callback VM.
///
/// Idempotent. Runs on the worker main thread from `exit_process` before the
/// Tokio runtime is dropped.
pub fn shutdown_current() {
    let drained = TIMERS.with(|slot| {
        let mut guard = slot.borrow_mut();
        let Some(state) = guard.as_mut() else {
            return Vec::new();
        };
        state.closed = true;
        state.running = 0;
        state.entries.drain().map(|(_, entry)| entry).collect()
    });
    let backend = NgxTimerBackend;
    for entry in drained {
        let mut entry = entry.borrow_mut();
        match entry.phase {
            RoundPhase::Pending => {
                let identity = entry.identity;
                let state =
                    with_timer_scope(identity, || entry.callback.start_reporting(true, &backend));
                if matches!(
                    state,
                    TimerCallbackState::Running
                        | TimerCallbackState::Waiting(_)
                        | TimerCallbackState::Yielded
                ) {
                    // A callback that suspended during shutdown must not resume;
                    // cancellation releases its pending async work exactly once.
                    let _ = entry.callback.cancel();
                }
            }
            RoundPhase::Live => {
                let _ = entry.callback.cancel();
            }
        }
    }
}

fn with_timer_scope<R>(identity: RequestIdentity, f: impl FnOnce() -> R) -> R {
    crate::host::with_vm_request_scope(
        identity,
        NgxPhase::Timer,
        AllowedOps::for_phase(NgxPhase::Timer),
        f,
    )
}

/// Installs the worker timer backend on a VM that imports the timer family.
///
/// The timer family contains owned-dispatch hosts, which can only be bound
/// through the registry path; the standard I/O, HTTP, and SQLite policies are
/// installed without per-name binds because the registry plan already resolved
/// every standard import.
pub fn install_timer_vm(vm: &mut Vm, policy: &StandardHostPolicy) -> Result<(), String> {
    let registry = ngx_host_registry()?;
    registry
        .bind_vm_cached(vm)
        .map_err(|error| format!("bind ngx timer host registry: {error}"))?;
    install_standard_policy(vm, policy)?;
    let backend = worker_backend().unwrap_or_else(|| Arc::new(NgxTimerBackend));
    vm.install_timer_runtime(backend, TimerConfig::default());
    Ok(())
}

/// Applies the location's standard-host policy without the per-name binds that
/// [`crate::host::install_standard_runtime`] performs.
///
/// A plan-bound VM resolves imports by registry slot; a later name bind would
/// flip it back to name resolution and fail on the owned timer entries.
fn install_standard_policy(vm: &mut Vm, policy: &StandardHostPolicy) -> Result<(), String> {
    vm.configure_io(IoPolicy {
        allowed_roots: policy.io_roots.clone(),
        allow_write: policy.io_write,
        allow_process: false,
        max_read_bytes: policy.io_max_read,
        max_write_bytes: policy.io_max_write,
    });
    vm.configure_http(HttpConfig {
        allowed_schemes: policy.http_schemes.clone(),
        allowed_hosts: policy.http_hosts.clone(),
        allowed_ports: policy.http_ports.clone(),
        allow_private_ips: policy.http_allow_private,
        ..HttpConfig::default()
    })
    .map_err(|err| err.to_string())?;
    vm.configure_sqlite(SqlitePolicy {
        database_root: policy.sqlite_root.clone(),
        allow_unsafe_sql: false,
        ..SqlitePolicy::default()
    });
    Ok(())
}

/// Dispatches every ready identity that belongs to the timer registry.
///
/// Called by the worker eventfd handler before request identities are routed;
/// request identities are not in the timer registry and are left untouched.
pub fn on_worker_event(ready: &[RequestIdentity]) {
    for identity in ready {
        if is_timer_identity(*identity) {
            dispatch_timer(*identity);
        }
    }
}

fn is_timer_identity(identity: RequestIdentity) -> bool {
    TIMERS.with(|slot| {
        slot.borrow()
            .as_ref()
            .is_some_and(|state| state.entries.contains_key(&identity))
    })
}

fn lookup_entry(identity: RequestIdentity) -> Option<Rc<RefCell<TimerEntry>>> {
    TIMERS.with(|slot| {
        slot.borrow()
            .as_ref()
            .and_then(|state| state.entries.get(&identity).cloned())
    })
}

fn dispatch_timer(identity: RequestIdentity) {
    let Some(entry) = lookup_entry(identity) else {
        return;
    };
    let backend = NgxTimerBackend;
    let mut entry = entry.borrow_mut();
    let step = match entry.phase {
        RoundPhase::Pending => {
            let has_slot = with_timers(|state| state.running < entry.max_running).unwrap_or(false);
            if !has_slot {
                // The running cap is full: the due callback stays pending and
                // starts when a running slot is released.
                entry.due = true;
                return;
            }
            with_timers(|state| state.running += 1);
            entry.phase = RoundPhase::Live;
            entry.due = false;
            let identity = entry.identity;
            let state =
                with_timer_scope(identity, || entry.callback.start_reporting(false, &backend));
            Step::Settled(state)
        }
        RoundPhase::Live => {
            if !matches!(entry.callback.state(), TimerCallbackState::Waiting(_)) {
                return;
            }
            poll_step(&mut entry, &backend)
        }
    };
    drive_round(&mut entry, &backend, step);
}

/// Outcome of one callback-driving step.
enum Step {
    /// The callback advanced to this state.
    Settled(TimerCallbackState),
    /// An asynchronous step is still outstanding; its identity wake resumes it.
    Suspended,
}

/// Polls the callback's outstanding async step once.
fn poll_step(entry: &mut TimerEntry, backend: &dyn TimerBackend) -> Step {
    let identity = entry.identity;
    let waker = TIMERS.with(|slot| {
        slot.borrow()
            .as_ref()
            .map(|state| eventfd_waker(state.scheduler.sender.clone(), identity))
    });
    let Some(waker) = waker else {
        return Step::Suspended;
    };
    let mut cx = Context::from_waker(&waker);
    match with_timer_scope(identity, || entry.callback.poll_reporting(&mut cx, backend)) {
        Poll::Pending => Step::Suspended,
        Poll::Ready(state) => Step::Settled(state),
    }
}

/// Drives one round to a terminal state or a parked `Waiting` state.
///
/// A callback that suspends (`ngx::sleep`, outbound async I/O) is polled once
/// so the submitted host future registers with this registration's identity
/// waker; the wake then arrives through the worker eventfd queue and the next
/// event turn polls the same round again. Rounds never overlap: the next
/// `every` deadline is scheduled only after the round is terminal.
fn drive_round(entry: &mut TimerEntry, backend: &dyn TimerBackend, step: Step) {
    let mut step = step;
    let mut steps = 0usize;
    loop {
        match step {
            Step::Suspended => return,
            Step::Settled(TimerCallbackState::Ready) => {
                // A spurious wake before the first round: stay live.
                return;
            }
            Step::Settled(TimerCallbackState::Complete)
            | Step::Settled(TimerCallbackState::Cancelled) => {
                retire_round(entry, false);
                return;
            }
            Step::Settled(TimerCallbackState::Running)
            | Step::Settled(TimerCallbackState::Yielded)
            | Step::Settled(TimerCallbackState::Waiting(_)) => {
                if steps >= MAX_YIELD_RESUMES_PER_ROUND {
                    backend.report_callback_error(TimerCallbackError {
                        error: VmError::HostError(
                            "timer callback did not make progress after resumption".to_string(),
                        ),
                    });
                    let _ = entry.callback.cancel();
                    retire_round(entry, true);
                    return;
                }
                steps += 1;
                step = poll_step(entry, backend);
            }
        }
    }
}

/// Releases one finished round and schedules the next `every` deadline.
fn retire_round(entry: &mut TimerEntry, cancelled: bool) {
    let identity = entry.identity;
    with_timers(|state| state.running = state.running.saturating_sub(1));
    match (cancelled, entry.interval) {
        (false, Some(interval)) => {
            // `every`: re-arm only after terminal completion, so rounds serialize.
            entry.phase = RoundPhase::Pending;
            entry.due = false;
            spawn_deadline(identity, interval);
        }
        _ => remove_entry(identity),
    }
    start_due_entries();
}

/// Starts due `Pending` entries while running slots are free.
fn start_due_entries() {
    let due: Vec<RequestIdentity> = TIMERS.with(|slot| {
        slot.borrow()
            .as_ref()
            .map(|state| {
                state
                    .entries
                    .values()
                    .filter_map(|entry| {
                        // The entry whose round is finishing is still mutably
                        // borrowed by the dispatcher and is never "due".
                        let entry = entry.try_borrow().ok()?;
                        (entry.phase == RoundPhase::Pending && entry.due).then_some(entry.identity)
                    })
                    .collect()
            })
            .unwrap_or_default()
    });
    for identity in due {
        dispatch_timer(identity);
    }
}

fn remove_entry(identity: RequestIdentity) {
    TIMERS.with(|slot| {
        if let Some(state) = slot.borrow_mut().as_mut() {
            state.entries.remove(&identity);
        }
    });
}

/// The worker scheduler, if the timer runtime is installed.
fn scheduler() -> Option<TimerScheduler> {
    TIMERS.with(|slot| slot.borrow().as_ref().map(|state| state.scheduler.clone()))
}

fn spawn_deadline(identity: RequestIdentity, delay: Duration) {
    let Some(scheduler) = scheduler() else {
        return;
    };
    spawn_deadline_with(&scheduler, identity, delay);
}

/// Spawns the deadline task: it holds only the sender and the identity.
fn spawn_deadline_with(scheduler: &TimerScheduler, identity: RequestIdentity, delay: Duration) {
    let handle = scheduler.handle.clone();
    let sender = scheduler.sender.clone();
    handle.spawn(async move {
        tokio::time::sleep(delay).await;
        let _ = sender.wake_identity(identity);
    });
}

/// The `ngx::timer` backend installed in every timer-capable ngx VM.
///
/// The backend owns admission and accounting: the worker registry keeps the
/// counters, enforces the worker pool bound and the installed [`TimerConfig`]
/// limits under one critical section, and is the sole source of
/// `pending_count` / `running_count`.
pub struct NgxTimerBackend;

impl TimerBackend for NgxTimerBackend {
    fn register(&self, registration: TimerRegistration) -> VmResult<()> {
        let TimerRegistration {
            delay,
            interval,
            max_pending,
            max_running,
            mut callback,
        } = registration;
        let Some(scheduler) = scheduler() else {
            return Err(VmError::HostError(
                "ngx.timer: worker timer runtime is not started".to_string(),
            ));
        };
        // The callback VM can suspend on async host work: install its own
        // bridge before the first round, never the creating request's.
        callback.set_async_bridge(Box::new(NgxAsyncBridge::new(scheduler.handle.clone())))?;
        let identity = TIMERS.with(|slot| {
            let mut guard = slot.borrow_mut();
            let Some(state) = guard.as_mut() else {
                return Err(VmError::HostError(
                    "ngx.timer: worker timer runtime is not started".to_string(),
                ));
            };
            if state.closed {
                return Err(VmError::HostError(
                    "ngx.timer: worker is shutting down".to_string(),
                ));
            }
            if state.entries.len() >= MAX_LIVE_TIMER_REQUESTS {
                return Err(VmError::HostError(format!(
                    "ngx.timer: worker timer limit reached ({MAX_LIVE_TIMER_REQUESTS})"
                )));
            }
            let pending = state
                .entries
                .values()
                .filter(|entry| {
                    // An executing callback's own entry is mutably borrowed by
                    // the dispatcher and is Live, so it never counts as pending.
                    entry
                        .try_borrow()
                        .is_ok_and(|entry| entry.phase == RoundPhase::Pending)
                })
                .count();
            if pending >= max_pending {
                return Err(VmError::HostError(format!(
                    "ngx.timer: pending timer limit reached ({max_pending})"
                )));
            }
            let id = state.next_id;
            state.next_id = state.next_id.checked_add(1).ok_or_else(|| {
                VmError::HostError("ngx.timer: worker timer ids exhausted".to_string())
            })?;
            let identity = RequestIdentity::new(TIMER_GENERATION, id);
            let entry = Rc::new(RefCell::new(TimerEntry {
                id,
                identity,
                callback,
                interval,
                max_running,
                phase: RoundPhase::Pending,
                due: false,
            }));
            state.entries.insert(identity, Rc::clone(&entry));
            Ok(identity)
        })?;
        spawn_deadline_with(&scheduler, identity, delay);
        Ok(())
    }

    fn pending_count(&self) -> usize {
        TIMERS.with(|slot| {
            slot.borrow()
                .as_ref()
                .map(|state| {
                    state
                        .entries
                        .values()
                        .filter(|entry| {
                            entry
                                .try_borrow()
                                .is_ok_and(|entry| entry.phase == RoundPhase::Pending)
                        })
                        .count()
                })
                .unwrap_or(0)
        })
    }

    fn running_count(&self) -> usize {
        TIMERS.with(|slot| slot.borrow().as_ref().map_or(0, |state| state.running))
    }

    fn report_callback_error(&self, error: TimerCallbackError) {
        with_timers(|state| state.callback_errors = state.callback_errors.saturating_add(1));
        log_callback_error(&error);
    }

    fn shutdown(&self) -> VmResult<()> {
        shutdown_current();
        Ok(())
    }
}

/// Reports one callback failure through the nginx cycle log.
///
/// Timer callbacks have no request; the cycle log is the worker-owned sink.
fn log_callback_error(error: &TimerCallbackError) {
    // SAFETY: `ngx_cycle` is process-lifetime NGINX state. The pointer is read
    // on the worker main thread and only used for logging.
    let cycle = unsafe { core::ptr::addr_of!(nginx_sys::ngx_cycle).read() };
    if cycle.is_null() {
        return;
    }
    // SAFETY: non-null cycle on the worker main thread.
    let log = unsafe { (*cycle).log };
    if log.is_null() {
        return;
    }
    ngx::ngx_log_error!(
        nginx_sys::NGX_LOG_ERR,
        log,
        "ngx-rustscript ngx.timer callback: {}",
        error.error
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timer_generation_never_collides_with_request_generations() {
        let timer = RequestIdentity::new(TIMER_GENERATION, 7);
        for request_generation in 1..=4u64 {
            assert_ne!(timer, RequestIdentity::new(request_generation, 7));
        }
        assert_eq!(timer.generation(), 0);
    }

    #[test]
    fn shutdown_without_runtime_is_idempotent() {
        shutdown_current();
        shutdown_current();
        assert_eq!(live_timer_count(), 0);
        assert_eq!(callback_error_count(), 0);
    }
}
