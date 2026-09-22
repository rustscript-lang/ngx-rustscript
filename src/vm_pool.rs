//! Worker-thread-local synchronous VM pool.
//!
//! pd-vm at `5bd2788b620980e1d2ae6a146e4f7548c68d1882` exposes the bound
//! constructor through [`PreparedScript::new_vm`], which delegates to
//! `Vm::new_bound`. This pool therefore receives a configuration-time
//! [`PreparedScript`] and never prepares or fully binds on checkout.
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

#[cfg(test)]
use vm::Program;
use vm::{Vm, VmError, VmStatus};

use crate::host::NgxPhase;
use crate::phase::PreparedScript;
#[cfg(test)]
use crate::phase::ScriptExecutionMetadata;

/// Maximum idle VMs retained on one worker thread.
pub const MAX_IDLE_VMS: usize = 4;

thread_local! {
    static POOL: RefCell<WorkerVmPool> = const { RefCell::new(WorkerVmPool::new()) };
}

struct WorkerVmPool {
    idle: Vec<PooledVm>,
}

struct PooledVm {
    prepared: Arc<PreparedScript>,
    phase: NgxPhase,
    vm: Vm,
}

impl WorkerVmPool {
    const fn new() -> Self {
        Self { idle: Vec::new() }
    }

    fn take(&mut self, prepared: &Arc<PreparedScript>, phase: NgxPhase) -> Result<Vm, String> {
        if let Some(idx) = self.idle.iter().position(|entry| {
            Arc::ptr_eq(&entry.prepared, prepared)
                && entry.phase == phase
                && entry.prepared.ngx_catalog_abi() == prepared.ngx_catalog_abi()
                && entry.prepared.standard_catalog_abi() == prepared.standard_catalog_abi()
        }) {
            return Ok(self.idle.swap_remove(idx).vm);
        }
        construct(prepared)
    }

    fn recycle(&mut self, prepared: Arc<PreparedScript>, phase: NgxPhase, mut vm: Vm) {
        if self.idle.len() >= MAX_IDLE_VMS {
            return;
        }
        if reset_for_pool(&mut vm) {
            self.idle.push(PooledVm {
                prepared,
                phase,
                vm,
            });
        }
    }
}

fn construct(prepared: &Arc<PreparedScript>) -> Result<Vm, String> {
    // The prepared artifact was created through crate::host::ngx_host_registry;
    // pool misses only instantiate it.
    #[cfg(test)]
    CREATED.with(|created| created.set(created.get().saturating_add(1)));
    prepared.new_vm()
}

/// Reset through the supported pd-vm APIs. Pending host-IO close cannot be
/// driven without an async runtime; those VMs are dropped rather than parked.
pub(crate) fn reset_for_pool(vm: &mut Vm) -> bool {
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
    if vm.clear_async_bridge().is_err() {
        return false;
    }
    vm.clear_runtime_print_sink();
    vm.is_reusable()
}

/// Reset an async request VM while its worker Tokio runtime is current.
///
/// The mandatory poll and bridge removal are the core quiescence gate: they
/// reject pending/failed scope cleanup and any remaining future, operation,
/// resource, reset, or stream state before the VM can enter the idle pool.
pub(crate) fn reset_request_vm_for_pool(vm: &mut Vm, runtime: &tokio::runtime::Handle) -> bool {
    let _enter = runtime.enter();
    if vm.reset_for_reuse().is_err() {
        return false;
    }
    let waker = Waker::noop();
    let mut cx = Context::from_waker(waker);
    if !matches!(vm.poll_reset_for_reuse(&mut cx), Poll::Ready(Ok(()))) {
        return false;
    }
    if vm.scope_reset_pending() || !vm.is_reusable() {
        return false;
    }
    if vm.clear_async_bridge().is_err() {
        return false;
    }
    vm.clear_runtime_print_sink();
    vm.is_reusable()
}

/// Run one prebound program on a worker-local pooled VM.
pub(crate) fn run_prepared(
    prepared: &Arc<PreparedScript>,
    phase: NgxPhase,
) -> Result<VmStatus, VmError> {
    let mut vm = POOL
        .with(|pool| pool.borrow_mut().take(prepared, phase))
        .map_err(VmError::HostError)?;
    let result = vm.run();
    if matches!(&result, Ok(VmStatus::Halted) | Err(_)) {
        POOL.with(|pool| pool.borrow_mut().recycle(Arc::clone(prepared), phase, vm));
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
    use std::collections::HashMap;
    use vm::operation::OperationCancelReason;
    use vm::{
        CallReturn, HostAsyncBridge, HostAsyncOpTerminal, HostFuture, HostFutureOutput, HostOpId,
        VmResult, compile_source,
    };

    struct CleanupFailBridge {
        futures: HashMap<HostOpId, HostFuture>,
    }

    impl HostAsyncBridge for CleanupFailBridge {
        fn submit_op(&mut self, op_id: HostOpId, future: HostFuture) -> VmResult<()> {
            self.futures.insert(op_id, future);
            Ok(())
        }

        fn poll_op(
            &mut self,
            _op_id: HostOpId,
            _cx: &mut Context<'_>,
        ) -> Poll<VmResult<CallReturn>> {
            Poll::Pending
        }

        fn poll_submitted_op(
            &mut self,
            _op_id: HostOpId,
            _cx: &mut Context<'_>,
        ) -> Poll<VmResult<HostFutureOutput>> {
            Poll::Pending
        }

        fn request_cancel_op(
            &mut self,
            _op_id: HostOpId,
            _reason: OperationCancelReason,
        ) -> VmResult<()> {
            Err(VmError::HostError("injected cleanup failure".into()))
        }

        fn cleanup_op(&mut self, _op_id: HostOpId, _terminal: HostAsyncOpTerminal) -> VmResult<()> {
            Ok(())
        }
    }

    fn compile(source: &str) -> Arc<Program> {
        Arc::new(compile_source(source).expect("compile").program)
    }

    #[test]
    fn sequential_runs_of_one_program_create_one_vm() {
        reset_test_state();
        let program = compile("let x = 1 + 2;\n");
        let prepared =
            PreparedScript::prepare(program, ScriptExecutionMetadata::default()).expect("prepare");
        for _ in 0..32 {
            assert_eq!(
                run_prepared(&prepared, NgxPhase::Access).expect("run"),
                VmStatus::Halted
            );
        }
        assert_eq!(created_count(), 1, "same program must reuse one VM");
        assert_eq!(idle_count(), 1);
        assert!(idle_count() <= MAX_IDLE_VMS);
    }

    #[cfg(feature = "bind-mode-test-hooks")]
    #[test]
    fn prepared_pool_checkout_reuse_and_capacity_use_bound_construction() {
        reset_test_state();
        let scope = vm::BindModeTestScope::enter();
        let program = compile("let x = 1 + 2;\n");
        let prepared = (0..=MAX_IDLE_VMS)
            .map(|_| {
                PreparedScript::prepare(Arc::clone(&program), ScriptExecutionMetadata::default())
                    .expect("prepare pooled program")
            })
            .collect::<Vec<_>>();

        // Each checkout goes through the production `take` miss path. The
        // fifth entry is over capacity and is dropped on recycle.
        for prepared in &prepared {
            assert_eq!(
                run_prepared(prepared, NgxPhase::Access).expect("bound pool VM"),
                VmStatus::Halted
            );
        }
        assert_eq!(created_count(), MAX_IDLE_VMS + 1);
        assert_eq!(idle_count(), MAX_IDLE_VMS);

        // The first entry was retained and hits the pool. The over-capacity
        // entry was evicted and must be constructed again on its next miss.
        assert_eq!(
            run_prepared(&prepared[0], NgxPhase::Access).expect("retained VM reuse"),
            VmStatus::Halted
        );
        assert_eq!(created_count(), MAX_IDLE_VMS + 1);
        assert_eq!(
            run_prepared(&prepared[MAX_IDLE_VMS], NgxPhase::Access)
                .expect("evicted VM replacement"),
            VmStatus::Halted
        );
        assert_eq!(created_count(), MAX_IDLE_VMS + 2);

        let metrics = scope.snapshot();
        assert_eq!(
            metrics.bound_program_preparations,
            (MAX_IDLE_VMS + 1) as u64
        );
        assert_eq!(metrics.bound_vm_instantiations, (MAX_IDLE_VMS + 2) as u64);
        assert_eq!(metrics.full_bind_installs, 0);
        assert_eq!(idle_count(), MAX_IDLE_VMS);
    }

    #[cfg(feature = "bind-mode-test-hooks")]
    #[test]
    fn distinct_preparations_sharing_one_program_never_share_an_idle_vm() {
        reset_test_state();
        let scope = vm::BindModeTestScope::enter();
        let program = compile("let x = 1 + 2;\n");
        let first =
            PreparedScript::prepare(Arc::clone(&program), ScriptExecutionMetadata::default())
                .expect("first preparation");
        let second =
            PreparedScript::prepare(Arc::clone(&program), ScriptExecutionMetadata::default())
                .expect("second preparation");

        for prepared in [&first, &second, &first, &second] {
            assert_eq!(
                run_prepared(prepared, NgxPhase::Access).expect("prepared pool checkout"),
                VmStatus::Halted
            );
        }

        assert_eq!(
            created_count(),
            2,
            "prepared identity must isolate idle VMs"
        );
        assert_eq!(idle_count(), 2);
        let metrics = scope.snapshot();
        assert_eq!(metrics.bound_program_preparations, 2);
        assert_eq!(metrics.bound_vm_instantiations, 2);
        assert_eq!(metrics.full_bind_installs, 0);
    }

    #[test]
    fn prepared_pool_phase_identity_is_isolated() {
        reset_test_state();
        let program = compile("let x = 1 + 2;\n");
        let prepared =
            PreparedScript::prepare(program, ScriptExecutionMetadata::default()).expect("prepare");

        assert_eq!(
            run_prepared(&prepared, NgxPhase::Access).expect("access run"),
            VmStatus::Halted
        );
        assert_eq!(
            run_prepared(&prepared, NgxPhase::Content).expect("content run"),
            VmStatus::Halted
        );
        assert_eq!(created_count(), 2);
        assert_eq!(
            run_prepared(&prepared, NgxPhase::Content).expect("content reuse"),
            VmStatus::Halted
        );
        assert_eq!(created_count(), 2);
    }

    #[test]
    fn different_programs_do_not_share_a_vm() {
        reset_test_state();
        let first_program = compile("let x = 1;\n");
        let second_program = compile("let y = 2;\n");
        let first = PreparedScript::prepare(first_program, ScriptExecutionMetadata::default())
            .expect("first preparation");
        let second = PreparedScript::prepare(second_program, ScriptExecutionMetadata::default())
            .expect("second preparation");
        assert_eq!(
            run_prepared(&first, NgxPhase::Access).expect("first"),
            VmStatus::Halted
        );
        assert_eq!(
            run_prepared(&second, NgxPhase::Access).expect("second"),
            VmStatus::Halted
        );
        assert_eq!(
            run_prepared(&first, NgxPhase::Access).expect("first reuse"),
            VmStatus::Halted
        );
        assert_eq!(
            run_prepared(&second, NgxPhase::Access).expect("second reuse"),
            VmStatus::Halted
        );
        assert_eq!(created_count(), 2);
        assert!(idle_count() <= MAX_IDLE_VMS);
    }

    #[test]
    fn runtime_error_vm_resets_and_cannot_contaminate_next_run() {
        reset_test_state();
        let bad_program = compile("1 / 0;\n");
        let good_program = compile("let x = 1 + 2;\n");
        let bad = PreparedScript::prepare(bad_program, ScriptExecutionMetadata::default())
            .expect("bad preparation");
        let good = PreparedScript::prepare(good_program, ScriptExecutionMetadata::default())
            .expect("good preparation");
        let first_err = run_prepared(&bad, NgxPhase::Access).expect_err("division by zero");
        assert!(
            first_err.to_string().to_ascii_lowercase().contains("zero")
                || first_err
                    .to_string()
                    .to_ascii_lowercase()
                    .contains("division"),
            "{first_err}"
        );
        assert_eq!(
            run_prepared(&good, NgxPhase::Access).expect("good after error"),
            VmStatus::Halted
        );
        let second_err = run_prepared(&bad, NgxPhase::Access)
            .expect_err("error program must still fail after reset");
        assert!(
            second_err.to_string().to_ascii_lowercase().contains("zero")
                || second_err
                    .to_string()
                    .to_ascii_lowercase()
                    .contains("division"),
            "{second_err}"
        );
        assert_eq!(
            run_prepared(&good, NgxPhase::Access).expect("good after second error"),
            VmStatus::Halted
        );
        assert_eq!(created_count(), 2);
        assert!(idle_count() <= MAX_IDLE_VMS);
    }

    #[test]
    fn cleanup_failure_drops_async_request_vm() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("runtime");
        let program = vm::compile_source_with_flavor_and_options(
            "use ngx;\nngx::ready();\n",
            vm::SourceFlavor::RustScript,
            vm::CompileSourceFileOptions::default()
                .with_host_api_catalog(Arc::new(crate::host::test_bind_catalog())),
        )
        .expect("compile immediate async host")
        .program;
        let mut vm = Vm::new(program);
        crate::host::bind_test_hosts(&mut vm);
        vm.set_async_bridge(Box::new(CleanupFailBridge {
            futures: HashMap::new(),
        }))
        .expect("set failing cleanup bridge");
        assert!(matches!(vm.run(), Ok(VmStatus::Waiting(_))));
        assert!(!reset_request_vm_for_pool(&mut vm, runtime.handle()));
        assert!(vm.scope_reset_pending() || !vm.is_reusable());
    }

    #[test]
    fn idle_pool_is_bounded() {
        reset_test_state();
        let mut programs = Vec::new();
        for i in 0..(MAX_IDLE_VMS + 3) {
            programs.push(compile(&format!("let x = {i};\n")));
        }
        let prepared = programs
            .into_iter()
            .map(|program| {
                PreparedScript::prepare(program, ScriptExecutionMetadata::default())
                    .expect("prepare bounded-pool program")
            })
            .collect::<Vec<_>>();
        for prepared in &prepared {
            assert_eq!(
                run_prepared(prepared, NgxPhase::Access).expect("run"),
                VmStatus::Halted
            );
        }
        assert_eq!(created_count(), MAX_IDLE_VMS + 3);
        assert!(idle_count() <= MAX_IDLE_VMS);
    }
}
