use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

static NGINX_GLOBAL_TEST_LOCK: Mutex<()> = Mutex::new(());

struct HeldGlobalTestLock {
    depth: usize,
    lock: Option<MutexGuard<'static, ()>>,
}

thread_local! {
    static HELD_GLOBAL_TEST_LOCK: RefCell<HeldGlobalTestLock> = const {
        RefCell::new(HeldGlobalTestLock {
            depth: 0,
            lock: None,
        })
    };
}

/// Serialize tests that inspect or mutate process-wide NGINX/eventfd state.
///
/// The lock is re-entrant on one test thread because a worker owns a notify
/// bridge and a test may create more than one worker before dropping them.
/// Poisoned locks are recovered so one failed test cannot strand later tests.
pub(crate) struct GlobalNginxTestGuard {
    active: bool,
}

pub(crate) fn global_ngx_test_guard() -> GlobalNginxTestGuard {
    HELD_GLOBAL_TEST_LOCK.with(|held| {
        let mut held = held.borrow_mut();
        if held.depth == 0 {
            held.lock = Some(
                NGINX_GLOBAL_TEST_LOCK
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner()),
            );
        }
        held.depth += 1;
    });
    GlobalNginxTestGuard { active: true }
}

impl Drop for GlobalNginxTestGuard {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        HELD_GLOBAL_TEST_LOCK.with(|held| {
            let mut held = held.borrow_mut();
            debug_assert!(held.depth > 0);
            held.depth = held.depth.saturating_sub(1);
            if held.depth == 0 {
                held.lock.take();
            }
        });
        self.active = false;
    }
}

static EVENTFD_LIFECYCLE_REGISTRATION: Mutex<Option<Arc<EventFdLifecycleState>>> = Mutex::new(None);
static NEXT_EVENTFD_LIFECYCLE_GENERATION: AtomicU64 = AtomicU64::new(1);

struct EventFdLifecycleState {
    generation: u64,
    created_fd: Mutex<Option<i32>>,
    close_called: AtomicBool,
    close_succeeded: AtomicBool,
    dropped: AtomicBool,
}

/// Test-only evidence for one eventfd-bearing notify instance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct EventFdLifecycleEvidence {
    pub(crate) generation: u64,
    pub(crate) fd: i32,
    pub(crate) close_called: bool,
    pub(crate) close_succeeded: bool,
    pub(crate) dropped: bool,
}

pub(crate) struct EventFdLifecycleToken {
    state: Arc<EventFdLifecycleState>,
    generation: u64,
    fd: i32,
}

impl EventFdLifecycleState {
    fn record_created(self: &Arc<Self>, fd: i32) -> EventFdLifecycleToken {
        let mut created_fd = recover_mutex(&self.created_fd);
        assert!(
            created_fd.is_none(),
            "one eventfd lifecycle probe observed more than one notify instance"
        );
        *created_fd = Some(fd);
        EventFdLifecycleToken {
            state: Arc::clone(self),
            generation: self.generation,
            fd,
        }
    }

    fn matches_fd(&self, fd: i32) -> bool {
        *recover_mutex(&self.created_fd) == Some(fd)
    }

    fn snapshot(&self) -> Option<EventFdLifecycleEvidence> {
        recover_mutex(&self.created_fd).map(|fd| EventFdLifecycleEvidence {
            generation: self.generation,
            fd,
            close_called: self.close_called.load(Ordering::Acquire),
            close_succeeded: self.close_succeeded.load(Ordering::Acquire),
            dropped: self.dropped.load(Ordering::Acquire),
        })
    }
}

impl EventFdLifecycleToken {
    pub(crate) fn record_close(&self, fd: i32, succeeded: bool) {
        debug_assert_eq!(self.generation, self.state.generation);
        debug_assert_eq!(self.fd, fd);
        if self.state.matches_fd(fd) {
            self.state.close_called.store(true, Ordering::Release);
            self.state
                .close_succeeded
                .store(succeeded, Ordering::Release);
        }
    }
}

impl Drop for EventFdLifecycleToken {
    fn drop(&mut self) {
        debug_assert_eq!(self.generation, self.state.generation);
        if self.state.matches_fd(self.fd) {
            self.state.dropped.store(true, Ordering::Release);
        }
    }
}

pub(crate) struct EventFdLifecycleProbeGuard<'a> {
    generation: u64,
    state: Arc<EventFdLifecycleState>,
    _global_test_guard: &'a GlobalNginxTestGuard,
    active: bool,
}

impl EventFdLifecycleProbeGuard<'_> {
    pub(crate) fn snapshot(&self) -> Option<EventFdLifecycleEvidence> {
        self.state.snapshot()
    }
}

impl Drop for EventFdLifecycleProbeGuard<'_> {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        let mut registration = recover_mutex(&EVENTFD_LIFECYCLE_REGISTRATION);
        if registration
            .as_ref()
            .is_some_and(|state| state.generation == self.generation)
        {
            registration.take();
        }
        self.active = false;
    }
}

fn recover_mutex<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poison| poison.into_inner())
}

pub(crate) fn install_eventfd_lifecycle_probe<'a>(
    global_test_guard: &'a GlobalNginxTestGuard,
) -> EventFdLifecycleProbeGuard<'a> {
    assert!(
        global_test_guard.active,
        "eventfd lifecycle probes require the global test guard"
    );
    let generation = NEXT_EVENTFD_LIFECYCLE_GENERATION.fetch_add(1, Ordering::Relaxed);
    let state = Arc::new(EventFdLifecycleState {
        generation,
        created_fd: Mutex::new(None),
        close_called: AtomicBool::new(false),
        close_succeeded: AtomicBool::new(false),
        dropped: AtomicBool::new(false),
    });
    let mut registration = recover_mutex(&EVENTFD_LIFECYCLE_REGISTRATION);
    assert!(
        registration.is_none(),
        "only one eventfd lifecycle probe may be active"
    );
    *registration = Some(Arc::clone(&state));
    EventFdLifecycleProbeGuard {
        generation,
        state,
        _global_test_guard: global_test_guard,
        active: true,
    }
}

pub(crate) fn record_eventfd_created(fd: i32) -> Option<EventFdLifecycleToken> {
    recover_mutex(&EVENTFD_LIFECYCLE_REGISTRATION)
        .as_ref()
        .map(|state| state.record_created(fd))
}

#[cfg(test)]
mod tests {
    use super::{global_ngx_test_guard, install_eventfd_lifecycle_probe, record_eventfd_created};
    use std::panic::AssertUnwindSafe;

    #[test]
    fn global_guard_reenters_and_recovers_from_poison() {
        let outer = global_ngx_test_guard();
        let inner = global_ngx_test_guard();
        drop(outer);
        drop(inner);

        let poisoned = std::thread::spawn(|| {
            let _guard = global_ngx_test_guard();
            panic!("poison the test-only global lock");
        })
        .join();
        assert!(poisoned.is_err());

        let _recovered = global_ngx_test_guard();
    }

    #[test]
    fn eventfd_probe_does_not_claim_close_when_token_is_forgotten() {
        let global_test_guard = global_ngx_test_guard();
        let probe = install_eventfd_lifecycle_probe(&global_test_guard);
        let token = record_eventfd_created(17).expect("eventfd probe registration");

        let before_forget = probe.snapshot().expect("created eventfd instance");
        assert!(!before_forget.close_called);
        assert!(!before_forget.close_succeeded);
        assert!(!before_forget.dropped);

        std::mem::forget(token);

        let after_forget = probe.snapshot().expect("created eventfd instance");
        assert!(!after_forget.close_called);
        assert!(!after_forget.close_succeeded);
        assert!(!after_forget.dropped);
    }

    #[test]
    fn eventfd_probe_clears_on_panic_and_recovers_from_poison() {
        let global_test_guard = global_ngx_test_guard();
        let panicked = std::panic::catch_unwind(AssertUnwindSafe(|| {
            let _probe = install_eventfd_lifecycle_probe(&global_test_guard);
            panic!("panic while eventfd probe is active");
        }));
        assert!(panicked.is_err());

        let poisoned = std::panic::catch_unwind(AssertUnwindSafe(|| {
            let _registration = super::EVENTFD_LIFECYCLE_REGISTRATION.lock().unwrap();
            panic!("poison the eventfd probe registry");
        }));
        assert!(poisoned.is_err());

        let recovered = install_eventfd_lifecycle_probe(&global_test_guard);
        drop(recovered);
    }
}
