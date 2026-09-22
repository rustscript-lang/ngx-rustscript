use std::cell::RefCell;
use std::sync::{Mutex, MutexGuard};

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

#[cfg(test)]
mod tests {
    use super::global_ngx_test_guard;

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
}
