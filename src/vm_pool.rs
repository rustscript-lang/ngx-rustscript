//! Worker-thread-local synchronous VM pool.
//!
//! pd-vm at `4b749862ff1632ef1266d75709da3676de37356c` stores `Arc<Program>`
//! internally and exposes:
//!
//! - [`Vm::new_shared`] — shared-program constructor (this pin has no
//!   `try_new_shared`; [`Vm::try_new`] only wraps owned [`Vm::new`])
//! - [`Vm::reset_for_reuse`] — starts execution-scope close, rewinds IP,
//!   clears stack/locals through the drop contract
//! - [`Vm::poll_reset_for_reuse`] — completes a pending generic scope close
//! - [`Vm::is_reusable`] — supported pool-return predicate (scope Active,
//!   no pending reset, interpreter state parked)
//!
//! This pool is strictly worker-thread-local and synchronous. It does not
//! implement `Send`, use a process-global mutex, store a request pointer, or
//! drive an async lifecycle. After `Halted` and after a recoverable runtime
//! error the VM is reset through the APIs above and parked only when
//! `is_reusable` is true. Idle size is bounded; excess VMs are dropped.

#[cfg(test)]
use std::cell::Cell;
use std::cell::RefCell;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};

use vm::{Program, Vm, VmError, VmStatus};

/// Maximum idle VMs retained on one worker thread.
pub const MAX_IDLE_VMS: usize = 4;

thread_local! {
    static POOL: RefCell<WorkerVmPool> = const { RefCell::new(WorkerVmPool::new()) };
}

struct WorkerVmPool {
    idle: Vec<Vm>,
}

impl WorkerVmPool {
    const fn new() -> Self {
        Self { idle: Vec::new() }
    }

    fn take(&mut self, program: &Arc<Program>) -> Vm {
        if let Some(idx) = self
            .idle
            .iter()
            .position(|vm| std::ptr::eq(vm.program(), program.as_ref()))
        {
            return self.idle.swap_remove(idx);
        }
        construct(program)
    }

    fn recycle(&mut self, mut vm: Vm) {
        if self.idle.len() >= MAX_IDLE_VMS {
            return;
        }
        if reset_for_pool(&mut vm) {
            self.idle.push(vm);
        }
    }
}

fn construct(program: &Arc<Program>) -> Vm {
    #[cfg(test)]
    CREATED.with(|created| created.set(created.get().saturating_add(1)));
    Vm::new_shared(Arc::clone(program))
}

/// Reset through the supported pd-vm APIs. Pending host-IO close cannot be
/// driven without an async runtime; those VMs are dropped rather than parked.
fn reset_for_pool(vm: &mut Vm) -> bool {
    if vm.reset_for_reuse().is_err() {
        return false;
    }
    if vm.scope_reset_pending() {
        let waker = Waker::noop();
        let mut cx = Context::from_waker(waker);
        match vm.poll_reset_for_reuse(&mut cx) {
            Poll::Ready(Ok(())) => {}
            Poll::Ready(Err(_)) | Poll::Pending => return false,
        }
    }
    vm.is_reusable()
}

/// Run `program` on a leased worker-local VM until the first status.
pub fn run(program: &Arc<Program>) -> Result<VmStatus, VmError> {
    let mut vm = POOL.with(|pool| pool.borrow_mut().take(program));
    let result = vm.run();
    if matches!(&result, Ok(VmStatus::Halted) | Err(_)) {
        POOL.with(|pool| pool.borrow_mut().recycle(vm));
    }
    result
}

#[cfg(test)]
thread_local! {
    static CREATED: Cell<usize> = const { Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn created_count() -> usize {
    CREATED.with(Cell::get)
}

#[cfg(test)]
pub(crate) fn reset_test_state() {
    CREATED.with(|created| created.set(0));
    POOL.with(|pool| pool.borrow_mut().idle.clear());
}

#[cfg(test)]
pub(crate) fn idle_count() -> usize {
    POOL.with(|pool| pool.borrow().idle.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use vm::compile_source;

    fn compile(source: &str) -> Arc<Program> {
        Arc::new(compile_source(source).expect("compile").program)
    }

    #[test]
    fn sequential_runs_of_one_program_create_one_vm() {
        reset_test_state();
        let program = compile("let x = 1 + 2;\n");
        for _ in 0..32 {
            assert_eq!(run(&program).expect("run"), VmStatus::Halted);
        }
        assert_eq!(created_count(), 1, "same program must reuse one VM");
        assert_eq!(idle_count(), 1);
        assert!(idle_count() <= MAX_IDLE_VMS);
    }

    #[test]
    fn different_programs_do_not_share_a_vm() {
        reset_test_state();
        let first = compile("let x = 1;\n");
        let second = compile("let y = 2;\n");
        assert_eq!(run(&first).expect("first"), VmStatus::Halted);
        assert_eq!(run(&second).expect("second"), VmStatus::Halted);
        assert_eq!(run(&first).expect("first reuse"), VmStatus::Halted);
        assert_eq!(run(&second).expect("second reuse"), VmStatus::Halted);
        assert_eq!(created_count(), 2);
        assert!(idle_count() <= MAX_IDLE_VMS);
    }

    #[test]
    fn runtime_error_vm_resets_and_cannot_contaminate_next_run() {
        reset_test_state();
        let bad = compile("1 / 0;\n");
        let good = compile("let x = 1 + 2;\n");
        let first_err = run(&bad).expect_err("division by zero");
        assert!(
            first_err.to_string().to_ascii_lowercase().contains("zero")
                || first_err
                    .to_string()
                    .to_ascii_lowercase()
                    .contains("division"),
            "{first_err}"
        );
        assert_eq!(run(&good).expect("good after error"), VmStatus::Halted);
        let second_err = run(&bad).expect_err("error program must still fail after reset");
        assert!(
            second_err.to_string().to_ascii_lowercase().contains("zero")
                || second_err
                    .to_string()
                    .to_ascii_lowercase()
                    .contains("division"),
            "{second_err}"
        );
        assert_eq!(
            run(&good).expect("good after second error"),
            VmStatus::Halted
        );
        assert_eq!(created_count(), 2);
        assert!(idle_count() <= MAX_IDLE_VMS);
    }

    #[test]
    fn idle_pool_is_bounded() {
        reset_test_state();
        let mut programs = Vec::new();
        for i in 0..(MAX_IDLE_VMS + 3) {
            programs.push(compile(&format!("let x = {i};\n")));
        }
        for program in &programs {
            assert_eq!(run(program).expect("run"), VmStatus::Halted);
        }
        assert_eq!(created_count(), MAX_IDLE_VMS + 3);
        assert!(idle_count() <= MAX_IDLE_VMS);
    }
}
