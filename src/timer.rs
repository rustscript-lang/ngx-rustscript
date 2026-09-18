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
//! # Identity and generations
//!
//! - The registration map is the generic
//!   [`crate::request_state::IdentityRegistry`] keyed by exact identity: the
//!   same bounded exact-generation lookup the request path uses.
//! - A fake request id is worker-unique and never reused: the allocator hands
//!   out [`FAKE_REQUEST_ID_TAG`]-tagged ids, while real request ids are
//!   ascending integers from 1 ([`crate::access::next_request_id`]). A timer
//!   identity can therefore never equal a live request identity, so both
//!   dispatch paths stay disjoint without a reserved generation, and the
//!   request path may be inside its own worker-access borrow while a script
//!   registers a timer.
//! - Every `every` round terminates by **retiring its identity**: the
//!   completed round's identity leaves the registry, a fresh monotonically
//!   advancing generation is allocated for the same fake request id, and only
//!   then is the next deadline scheduled. A stale or duplicate wake of a
//!   completed round therefore matches no live entry and can never start the
//!   pending round early or break the interval. The freshly minted identity is
//!   held by exactly one deadline task, so nothing else can wake it.
//!
//! # Threading
//!
//! - Registration, callback execution, polling, and shutdown run on the NGINX
//!   worker main thread only. Script bytecode and NGINX FFI never leave it.
//! - The Tokio task spawned per deadline only sleeps and enqueues the identity;
//!   it holds no VM, no request, no pool, and no raw pointer.
//! - A callback that needs to wait (`ngx::sleep`, outbound async I/O) suspends
//!   on its **own** `NgxAsyncBridge`, installed before the first round; the
//!   wake carries the round's current identity.
//! - Dispatch is eventfd-only: the worker eventfd handler routes every ready
//!   identity through [`on_worker_event`] on the worker main thread, which
//!   ignores identities that are not a live timer registration.
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
use std::rc::Rc;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use tokio::runtime::Handle;
use vm::{
    OwnedTimerCallback, TimerBackend, TimerCallbackError, TimerCallbackState, TimerConfig,
    TimerHostExt, TimerRegistration, Vm, VmError, VmResult,
};

use crate::async_bridge::NgxAsyncBridge;
use crate::host::{AllowedOps, NgxPhase, StandardHostPolicy};
use crate::notify::{CompletionSender, RequestIdentity, eventfd_waker};
use crate::request_state::{IdentityRegistry, RegistryError};

/// Maximum live timer registrations ("fake requests") per nginx worker.
///
/// The installed [`TimerConfig`] additionally caps registrations per runtime
/// ([`vm::DEFAULT_MAX_PENDING_TIMERS`] = 1024, [`vm::DEFAULT_MAX_RUNNING_TIMERS`]
/// = 256 by default); this constant is the hard worker pool bound: it is the
/// capacity of the worker registry ([`IdentityRegistry`]), enforced under the
/// same critical section as its insertion.
pub const MAX_LIVE_TIMER_REQUESTS: usize = 1024;

/// High bit that separates fake request ids from real request ids.
///
/// Real request ids are ascending integers from 1
/// ([`crate::access::next_request_id`]); fake request ids set this bit, so a
/// timer identity can never equal a live request identity even though both are
/// ordinary `(generation, request_id)` pairs. Dispatch stays disjoint without
/// a reserved generation, and a script may register a timer while the request
/// path is inside its own worker-access borrow (the fake id allocator never
/// touches it).
pub const FAKE_REQUEST_ID_TAG: u64 = 1 << 63;

/// Fake request ids available per worker (the untagged half of the space).
///
/// The allocator fails closed instead of wrapping, so a fake id always stays
/// tagged.
const MAX_FAKE_REQUEST_IDS: u64 = FAKE_REQUEST_ID_TAG;

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
    /// Worker-unique fake request id, never reused. The identity's request id
    /// is always this value; only the generation rotates between rounds.
    id: u64,
    /// Current round identity. Retired generations are removed from the
    /// registry and never match a live entry again.
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
    /// Next per-round identity generation. Monotonic across the worker's
    /// registrations and rounds; never zero, so it always differs from the
    /// uninitialised state.
    next_generation: u64,
    /// Next untagged fake request id. Monotonic; ids are never reused and the
    /// tagged id always stays in the fake-request half of the id space.
    next_fake_request_id: u64,
    /// Exact-generation registration map with a hard capacity: the same
    /// bounded registry the request path uses for `Waiting` identities.
    entries: IdentityRegistry<Rc<RefCell<TimerEntry>>>,
    closed: bool,
    /// Live callback rounds, including callbacks suspended on async host work.
    running: usize,
    /// Callback failures reported through the backend sink.
    callback_errors: usize,
}

thread_local! {
    static TIMERS: RefCell<Option<WorkerTimers>> = const { RefCell::new(None) };
}

/// Advances a worker generation counter, wrapping past zero.
fn advance_generation(generation: &mut u64) {
    *generation = generation.wrapping_add(1);
    if *generation == 0 {
        *generation = 1;
    }
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
            next_generation: 1,
            next_fake_request_id: 0,
            entries: IdentityRegistry::with_capacity(MAX_LIVE_TIMER_REQUESTS),
            closed: false,
            running: 0,
            callback_errors: 0,
        });
    });
}

/// True when `program` imports any `ngx::timer` host.
///
/// Such programs are bound from [`crate::host::ngx_host_registry`] and must
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

/// Current identities of the live timer registrations in this worker, in fake
/// request id order.
///
/// Every registration owns one ordinary [`RequestIdentity`]; the identity of an
/// `every` registration changes (generation advances) on every re-arm, so this
/// returns the **current** round identity of each live registration.
pub fn live_timer_identities() -> Vec<RequestIdentity> {
    TIMERS.with(|slot| {
        slot.borrow()
            .as_ref()
            .map(|state| {
                let mut entries: Vec<(u64, RequestIdentity)> = state
                    .entries
                    .iter_values()
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
        let drained: Vec<Rc<RefCell<TimerEntry>>> = state.entries.iter_values().cloned().collect();
        state.entries.clear();
        drained
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
/// Hosts are bound through [`crate::host::ngx_host_registry`]; this only
/// attaches the worker timer runtime after that bind.
pub fn install_timer_vm(vm: &mut Vm, policy: &StandardHostPolicy) -> Result<(), String> {
    crate::host::bind_production_hosts(vm)?;
    crate::host::install_standard_policy(vm, policy)?;
    install_timer_runtime_on_vm(vm);
    Ok(())
}

/// Attach the worker-owned timer runtime to an already registry-bound VM.
pub fn install_timer_runtime_on_vm(vm: &mut Vm) {
    let backend = worker_backend().unwrap_or_else(|| Arc::new(NgxTimerBackend));
    vm.install_timer_runtime(backend, TimerConfig::default());
}

/// Dispatches every ready identity that is a live timer registration.
///
/// Called by the worker eventfd handler before request identities are routed.
/// Identities that no live registration currently owns — request identities,
/// and stale or duplicate wakes of retired generations — match no registry
/// entry and are ignored, so both dispatch paths stay disjoint and a retired
/// generation can never start a pending round.
pub fn on_worker_event(ready: &[RequestIdentity]) {
    for identity in ready {
        dispatch_timer(*identity);
    }
}

fn lookup_entry(identity: RequestIdentity) -> Option<Rc<RefCell<TimerEntry>>> {
    TIMERS.with(|slot| {
        slot.borrow()
            .as_ref()
            .and_then(|state| state.entries.get(identity).cloned())
    })
}

fn dispatch_timer(identity: RequestIdentity) {
    let Some(entry) = lookup_entry(identity) else {
        return;
    };
    let backend = NgxTimerBackend;
    let step = {
        let mut entry = entry.borrow_mut();
        // Exact-generation guard: only the registration's current identity is a
        // deadline. A lookup that raced an identity rotation must never start
        // the pending round early.
        if entry.identity != identity {
            return;
        }
        match entry.phase {
            RoundPhase::Pending => {
                let has_slot =
                    with_timers(|state| state.running < entry.max_running).unwrap_or(false);
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
        }
    };
    drive_round(&entry, &backend, step);
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
/// so the submitted host future registers with this round's identity waker; the
/// wake then arrives through the worker eventfd queue and the next event turn
/// polls the same round again. Rounds never overlap: the next `every` deadline
/// is scheduled only after the round is terminal, and it uses a **fresh**
/// identity so no wake of the completed round can trigger it.
fn drive_round(entry: &Rc<RefCell<TimerEntry>>, backend: &dyn TimerBackend, step: Step) {
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
                    let _ = entry.borrow_mut().callback.cancel();
                    retire_round(entry, true);
                    return;
                }
                steps += 1;
                step = {
                    let mut entry = entry.borrow_mut();
                    poll_step(&mut entry, backend)
                };
            }
        }
    }
}

/// Releases one finished round and, for `every`, re-arms it under a fresh
/// identity.
///
/// The completed round's identity is retired **before** the new one is
/// registered, and the new generation is never the identity any earlier wake
/// carried. A stale or duplicate wake of the finished round therefore matches
/// no live entry: it cannot start the pending round early and cannot break the
/// `every` interval or start an overlapping round.
fn retire_round(entry: &Rc<RefCell<TimerEntry>>, cancelled: bool) {
    let (identity, interval, request_id) = {
        let entry = entry.borrow();
        (entry.identity, entry.interval, entry.id)
    };
    with_timers(|state| state.running = state.running.saturating_sub(1));
    match (cancelled, interval) {
        (false, Some(interval)) => {
            let rotated = TIMERS.with(|slot| {
                let mut guard = slot.borrow_mut();
                let state = guard.as_mut()?;
                state.entries.remove(identity);
                let generation = state.next_generation;
                advance_generation(&mut state.next_generation);
                let next = RequestIdentity::new(generation, request_id);
                match state.entries.insert_new(next, Rc::clone(entry)) {
                    // The finished identity made room for exactly one entry, so
                    // a full or duplicate registry is an internal invariant
                    // violation; fail closed by leaving the registration out.
                    Ok(()) => Some(next),
                    Err(RegistryError::Full | RegistryError::Duplicate) => None,
                }
            });
            match rotated {
                Some(next) => {
                    {
                        let mut entry = entry.borrow_mut();
                        entry.identity = next;
                        entry.phase = RoundPhase::Pending;
                        entry.due = false;
                    }
                    // Only the freshly minted identity reaches the deadline
                    // task; nothing else can wake the pending round.
                    spawn_deadline(next, interval);
                }
                None => remove_entry(identity),
            }
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
                    .iter_values()
                    .filter_map(|entry| {
                        // The entry whose round is finishing is never borrowed
                        // here and is not "due"; a mutably borrowed entry can
                        // only be an executing callback.
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
            state.entries.remove(identity);
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
            let pending = state
                .entries
                .iter_values()
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
            // One ordinary identity per registration: the fake request id is
            // worker-unique, never reused, and tagged out of the real
            // request-id space.
            let untagged = state.next_fake_request_id;
            if untagged >= MAX_FAKE_REQUEST_IDS {
                return Err(VmError::HostError(
                    "ngx.timer: worker timer ids exhausted".to_string(),
                ));
            }
            state.next_fake_request_id += 1;
            let request_id = FAKE_REQUEST_ID_TAG | untagged;
            let generation = state.next_generation;
            advance_generation(&mut state.next_generation);
            let identity = RequestIdentity::new(generation, request_id);
            let entry = Rc::new(RefCell::new(TimerEntry {
                id: request_id,
                identity,
                callback,
                interval,
                max_running,
                phase: RoundPhase::Pending,
                due: false,
            }));
            match state.entries.insert_new(identity, Rc::clone(&entry)) {
                Ok(()) => Ok(identity),
                Err(RegistryError::Full) => Err(VmError::HostError(format!(
                    "ngx.timer: worker timer limit reached ({MAX_LIVE_TIMER_REQUESTS})"
                ))),
                Err(RegistryError::Duplicate) => Err(VmError::HostError(
                    "ngx.timer: worker timer identity was already registered".to_string(),
                )),
            }
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
                        .iter_values()
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
    use crate::notify::DEFAULT_MAX_IN_FLIGHT;

    #[test]
    fn worker_registry_is_the_shared_identity_registry_at_the_pool_bound() {
        let mut worker = crate::runtime::WorkerRuntime::start().expect("worker runtime");
        let (capacity, len) = TIMERS.with(|slot| {
            let guard = slot.borrow();
            let state = guard.as_ref().expect("installed worker timers");
            (state.entries.capacity(), state.entries.len())
        });
        assert_eq!(
            capacity, MAX_LIVE_TIMER_REQUESTS,
            "the timer registry reuses the request_state::IdentityRegistry at the worker pool bound"
        );
        assert_eq!(len, 0);
        worker.shutdown();
    }

    #[test]
    fn fake_request_ids_are_tagged_and_never_reused() {
        shutdown_current();
        let mut worker = crate::runtime::WorkerRuntime::start().expect("worker runtime");
        // Two registrations take two distinct tagged ids: the fake-request half
        // of the worker id space is separate from the ascending request ids, so
        // a timer identity can never equal a request identity.
        let program = crate::config::compile_rss_source(
            "use ngx;\nngx::timer::at(30, |premature| null);\nngx::timer::at(20, |premature| null);\n",
        )
        .expect("compile timer script");
        let mut vm = Vm::new_shared(program);
        install_timer_vm(&mut vm, &StandardHostPolicy::fail_closed()).expect("install timer vm");
        vm.set_async_bridge(Box::new(NgxAsyncBridge::new(
            worker.handle().expect("worker handle"),
        )))
        .expect("async bridge");
        vm.run().expect("registrations");

        let identities = live_timer_identities();
        assert_eq!(identities.len(), 2);
        let first = identities[0];
        let second = identities[1];
        assert_ne!(
            first.request_id(),
            second.request_id(),
            "fake request ids are never reused"
        );
        for identity in [first, second] {
            assert_ne!(
                identity.request_id() & FAKE_REQUEST_ID_TAG,
                0,
                "a fake request id sets the tag bit: {identity:?}"
            );
        }
        // A real request id never carries the tag.
        let next_request_id = crate::access::next_request_id();
        assert_eq!(
            next_request_id & FAKE_REQUEST_ID_TAG,
            0,
            "request ids stay in the untagged half"
        );
        shutdown_current();
        worker.shutdown();
    }

    #[test]
    fn registry_capacity_and_duplicate_are_exact() {
        let mut registry: IdentityRegistry<u64> = IdentityRegistry::with_capacity(1);
        let first = RequestIdentity::new(1, 7);
        let second = RequestIdentity::new(2, 7);
        assert_eq!(registry.insert_new(first, 7), Ok(()));
        assert_eq!(
            registry.insert_new(first, 7),
            Err(RegistryError::Duplicate),
            "an existing identity is rejected even with an equal value"
        );
        assert_eq!(
            registry.insert_new(second, 7),
            Err(RegistryError::Full),
            "capacity is enforced with the same insertion"
        );
        assert_eq!(registry.remove(first), Some(7));
        assert_eq!(registry.insert_new(second, 7), Ok(()));
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn worker_registry_rejects_a_duplicate_live_identity() {
        shutdown_current();
        let mut worker = crate::runtime::WorkerRuntime::start().expect("worker runtime");
        let program =
            crate::config::compile_rss_source("use ngx;\nngx::timer::at(30, |premature| null);\n")
                .expect("compile timer script");
        let mut vm = Vm::new_shared(program);
        install_timer_vm(&mut vm, &StandardHostPolicy::fail_closed()).expect("install timer vm");
        vm.set_async_bridge(Box::new(NgxAsyncBridge::new(
            worker.handle().expect("worker handle"),
        )))
        .expect("async bridge");
        vm.run().expect("registration");

        let identities = live_timer_identities();
        assert_eq!(identities.len(), 1);
        let duplicate = TIMERS.with(|slot| {
            let mut guard = slot.borrow_mut();
            let state = guard.as_mut().expect("installed worker timers");
            let entry = state
                .entries
                .get(identities[0])
                .cloned()
                .expect("live entry");
            state.entries.insert_new(identities[0], entry)
        });
        assert_eq!(duplicate, Err(RegistryError::Duplicate));
        assert_eq!(
            live_timer_count(),
            1,
            "a rejected duplicate must not add a fake request"
        );

        shutdown_current();
        assert_eq!(live_timer_count(), 0);
        worker.shutdown();
    }

    #[test]
    fn every_generation_never_collides_with_request_generations() {
        // A timer identity is an ordinary `(generation, request_id)` pair from
        // worker-unique sequences; the reserved-generation scheme is gone.
        let timer = RequestIdentity::new(3, 7);
        for request_generation in 1..=4u64 {
            assert_ne!(timer, RequestIdentity::new(request_generation, 8));
        }
        assert_eq!(timer.generation(), 3);
        assert_eq!(timer.request_id(), 7);
    }

    #[test]
    fn shutdown_without_runtime_is_idempotent() {
        shutdown_current();
        shutdown_current();
        assert_eq!(live_timer_count(), 0);
        assert_eq!(callback_error_count(), 0);
    }

    #[test]
    fn max_in_flight_default_matches_the_timer_pool() {
        // The registry is a bounded exact map exactly like the request path's
        // waiting registry; both default to a hard 1024 in-flight bound.
        assert_eq!(DEFAULT_MAX_IN_FLIGHT, MAX_LIVE_TIMER_REQUESTS);
    }
}
