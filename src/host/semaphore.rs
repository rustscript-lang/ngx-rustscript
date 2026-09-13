// OpenResty-oriented semaphore host wrappers.
//
// Worker-owned state, the FIFO waiter queue, the async wait future, and the
// VM-local handle resource live in `crate::semaphore`. These wrappers only
// bind the pd-host-function surface. The generated async wrapper captures an
// owned `SemaphoreWaitContext` (worker state + owned timeout) before the
// future is submitted; no request, pool, VM, or raw pointer crosses Tokio.

use pd_host_function::pd_host_function;
use vm::{Value, Vm};

use super::super::VmResult;
use super::{CallOutcome, IntoHostCallOutcome};

use crate::host::catalog::{
    NGX_SEMAPHORE_COUNT_NAME, NGX_SEMAPHORE_NEW_NAME, NGX_SEMAPHORE_POST_NAME,
};
use crate::semaphore::{self, SemaphoreHandle, SemaphoreWaitContext};

fn ret<T: IntoHostCallOutcome>(value: T) -> VmResult<CallOutcome> {
    Ok(value.into_host_call_outcome())
}

fn host_ctx(error: vm::HostContextError) -> vm::VmError {
    vm::VmError::HostError(error.to_string())
}

fn named(host: &str, error: vm::VmError) -> vm::VmError {
    match error {
        vm::VmError::HostError(text) if !text.starts_with(host) => {
            vm::VmError::HostError(format!("{host}: {text}"))
        }
        other => other,
    }
}

/// Creates a worker-owned counting semaphore and returns its VM-local handle.
///
/// `count` must be an integer in `0..=MAX_SEMAPHORE_COUNT`. The handle lives in
/// the **current** VM resource table; the permit counter and waiter queue live
/// in the worker registry.
#[pd_host_function(name = "ngx::semaphore::new")]
pub fn new(vm: &mut Vm, count: i64) -> VmResult<CallOutcome> {
    let count = semaphore::parse_new_count(count, NGX_SEMAPHORE_NEW_NAME)?;
    let state = semaphore::create_worker_semaphore(count).map_err(|error| named(NGX_SEMAPHORE_NEW_NAME, error))?;
    let token = vm
        .host_context()
        .push_resource(SemaphoreHandle::new(state))
        .map_err(host_ctx)?;
    ret(token.handle().raw() as i64)
}

/// Waits for one permit: `true` when acquired, `false` on timeout.
///
/// `timeout` is a number of seconds (fractions allowed); `null` waits without
/// a timer and `0` performs an immediate acquisition attempt. The generated
/// wrapper submits an owned host future; this body only awaits worker-owned
/// state and the owned timeout.
#[pd_host_function(name = "ngx::semaphore::wait")]
pub async fn wait(
    #[pd_host_context] context: SemaphoreWaitContext,
    _sem: i64,
    _timeout: Value,
) -> VmResult<bool> {
    semaphore::wait(context).await
}

/// Adds `count` permits (`null` means one) and wakes FIFO waiters.
#[pd_host_function(name = "ngx::semaphore::post")]
pub fn post(vm: &mut Vm, sem: i64, count: Value) -> VmResult<CallOutcome> {
    let amount = semaphore::parse_post_amount(&count, NGX_SEMAPHORE_POST_NAME)?;
    let state = semaphore::resolve_handle(vm, sem, NGX_SEMAPHORE_POST_NAME)?;
    state
        .post(amount)
        .map_err(|error| named(NGX_SEMAPHORE_POST_NAME, error))?;
    ret(())
}

/// Returns the current count: permits ready minus queued waiters.
#[pd_host_function(name = "ngx::semaphore::count")]
pub fn count(vm: &mut Vm, sem: i64) -> VmResult<CallOutcome> {
    let state = semaphore::resolve_handle(vm, sem, NGX_SEMAPHORE_COUNT_NAME)?;
    if state.is_closed() {
        return Err(vm::VmError::HostError(format!(
            "{NGX_SEMAPHORE_COUNT_NAME}: semaphore is closed: worker is shutting down"
        )));
    }
    ret(state.count())
}
