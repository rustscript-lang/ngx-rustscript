// NGINX error-log host. Sync only; never retains a request or log pointer.

use nginx_sys::{ngx_cycle, NGX_LOG_DEBUG, NGX_LOG_STDERR};
use ngx::http::Request;
use pd_host_function::pd_host_function;
use vm::VmError;

use super::super::VmResult;
use super::{CallOutcome, IntoHostCallOutcome};
use crate::host::{current_allowed_ops, current_request_live, AllowedOps};

fn ret(value: impl IntoHostCallOutcome) -> VmResult<CallOutcome> {
    Ok(value.into_host_call_outcome())
}

fn request_log() -> Option<*mut nginx_sys::ngx_log_t> {
    let Ok(allowed) = current_allowed_ops() else {
        return None;
    };
    if !allowed.contains(AllowedOps::LOG) {
        return None;
    }
    if current_request_live() != Ok(true) {
        return None;
    }
    let ptr = crate::host::context::current_request_ptr()?;
    // SAFETY: the pointer comes from the live worker-thread request scope.
    let request = unsafe { Request::from_ngx_http_request(ptr) };
    if request.connection().is_null() {
        return None;
    }
    let log = request.log();
    if log.is_null() {
        None
    } else {
        Some(log)
    }
}

fn cycle_log() -> Option<*mut nginx_sys::ngx_log_t> {
    // SAFETY: `ngx_cycle` is a process-lifetime NGINX global. The pointer is
    // copied and discarded before return; it is never stored.
    let cycle = unsafe { core::ptr::addr_of!(ngx_cycle).read() };
    if cycle.is_null() {
        return None;
    }
    // SAFETY: non-null cycle is the current cycle on this worker thread.
    let log = unsafe { (*cycle).log };
    if log.is_null() {
        None
    } else {
        Some(log)
    }
}

/// OpenResty `ngx.log(level, message)`.
#[pd_host_function(name = "ngx::log")]
pub fn log(level: i64, message: &str) -> VmResult<CallOutcome> {
    if level < NGX_LOG_STDERR as i64 || level > NGX_LOG_DEBUG as i64 {
        return Err(VmError::HostError(format!(
            "ngx::log level {level} is outside NGX_LOG_STDERR..=NGX_LOG_DEBUG"
        )));
    }
    if message.as_bytes().contains(&0) {
        return Err(VmError::HostError(
            "ngx::log message must not contain embedded NUL".into(),
        ));
    }
    let Some(log) = request_log().or_else(cycle_log) else {
        return ret(());
    };
    ngx::ngx_log_error!(
        level as nginx_sys::ngx_uint_t,
        log,
        "ngx-rustscript: {message}"
    );
    ret(())
}
