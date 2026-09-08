//! Access-phase VM execution: sync Halted via the worker pool, or
//! `ngx::sleep` suspend/resume through the worker eventfd bridge.
//!
//! Yield wait returns `NGX_AGAIN`. `ngx_http_core_access_phase` stops the
//! checker without advancing `phase_handler` and without incrementing
//! `r->main->count`. Resume runs on the worker main event turn after the
//! original handler returns: complete the coordinator, resume the same VM,
//! then `ngx_http_core_run_phases` exactly once so this handler can return
//! `NGX_OK` or 500. Host-bound VMs are never returned to the Task 2 pool.

use std::cell::RefCell;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;

use nginx_sys::{
    ngx_handle_read_event, ngx_http_block_reading, ngx_http_request_t, ngx_http_test_reading,
    ngx_post_event, ngx_posted_events,
};
use ngx::core::Status;
use ngx::ffi::NGX_LOG_ERR;
use ngx::http::{HTTPStatus, HttpModule, HttpModuleLocationConf, HttpRequestHandler, Request};
use ngx::ngx_log_error;
use vm::{Program, VmError, VmStatus};

use crate::Module;
use crate::config::{self, LocationConf};
use crate::notify::{DEFAULT_MAX_IN_FLIGHT, NotifyError};
use crate::request_state::{
    AccessExec, AccessStatus, ReadyEffect, SleepSubmit, SubmitError, TokenRegistry,
};
use crate::runtime::RuntimeError;
use crate::sleep_binding::SleepToken;
use crate::vm_pool;

struct AccessCtx {
    exec: RefCell<AccessExec>,
}

struct WorkerAccess {
    ids: TokenRegistry<u64>,
    live: TokenRegistry<*mut ngx_http_request_t>,
    in_handler: bool,
    deferred: Vec<SleepToken>,
    next_id: u64,
}

impl WorkerAccess {
    fn new() -> Self {
        Self {
            ids: TokenRegistry::default(),
            live: TokenRegistry::with_capacity(DEFAULT_MAX_IN_FLIGHT),
            in_handler: false,
            deferred: Vec::new(),
            next_id: 1,
        }
    }
}

thread_local! {
    static WORKER_ACCESS: RefCell<WorkerAccess> = RefCell::new(WorkerAccess::new());
}

struct HandlerGuard;

impl HandlerGuard {
    fn enter() -> Self {
        WORKER_ACCESS.with(|state| {
            state.borrow_mut().in_handler = true;
        });
        Self
    }
}

impl Drop for HandlerGuard {
    fn drop(&mut self) {
        WORKER_ACCESS.with(|state| {
            state.borrow_mut().in_handler = false;
        });
    }
}

impl Drop for AccessCtx {
    fn drop(&mut self) {
        // `Pool::allocate` registers this Drop as `ngx_pool_cleanup_add`.
        // NGINX 1.30.4 runs it from `ngx_destroy_pool` inside
        // `ngx_http_free_request`, after `r->cleanup` handlers and after
        // `r->pool = NULL`. Commit Cancelled before unregister/abort/drop.
        cleanup_access_ctx(self.exec.get_mut());
    }
}

fn cleanup_access_ctx(exec: &mut AccessExec) {
    let request_id = exec.request_id();
    let token = exec.cleanup();
    if let Some(token) = token {
        unregister_exact(token, request_id, None);
        invalidate_deferred(token);
        let _ = crate::runtime::with_worker(|worker| worker.abort_sleep(token));
    }
}

fn invalidate_deferred(token: SleepToken) {
    WORKER_ACCESS.with(|state| {
        state
            .borrow_mut()
            .deferred
            .retain(|queued| *queued != token);
    });
}

/// Worker exit: drop token maps without dereferencing request pointers.
pub(crate) fn clear_worker_access() {
    WORKER_ACCESS.with(|state| {
        let mut state = state.borrow_mut();
        state.ids.clear();
        state.live.clear();
        state.deferred.clear();
        state.in_handler = false;
    });
}

struct WorkerSleepSubmit;

impl SleepSubmit for WorkerSleepSubmit {
    fn submit(&mut self, token: SleepToken, ms: i64) -> Result<(), SubmitError> {
        crate::runtime::with_worker(|worker| worker.submit_sleep(token, ms))
            .ok_or(SubmitError::Closed)?
            .map_err(|err| match err {
                RuntimeError::Notify(NotifyError::QueueFull) => SubmitError::Full,
                RuntimeError::Notify(NotifyError::ShutDown) => SubmitError::Closed,
                _ => SubmitError::Closed,
            })
    }
}

/// Run a compiled program on a worker-local pooled VM until the first status.
pub fn run_access_program(program: &Arc<Program>) -> Result<VmStatus, VmError> {
    vm_pool::run(program)
}

/// Map VM termination onto the access-phase continuation / error codes.
pub fn status_from_vm_run(result: Result<VmStatus, VmError>) -> Status {
    match result {
        Ok(VmStatus::Halted) => Status::NGX_OK,
        Ok(_) | Err(_) => HTTPStatus::INTERNAL_SERVER_ERROR.into(),
    }
}

pub(crate) struct AccessHandler;

impl HttpRequestHandler for AccessHandler {
    const PHASE: ngx::http::HttpPhase = ngx::http::HttpPhase::Access;
    type Output = Status;

    fn handler(request: &mut Request) -> Self::Output {
        let _guard = HandlerGuard::enter();
        let Some(conf) = Module::location_conf(request) else {
            return Status::NGX_DECLINED;
        };
        run_configured_access(request, conf)
    }
}

fn run_configured_access(request: &mut Request, conf: &LocationConf) -> Status {
    let Some(program) = conf.program() else {
        return Status::NGX_DECLINED;
    };
    if !config::program_needs_sleep(program) {
        return match run_access_program(program) {
            Ok(VmStatus::Halted) => Status::NGX_OK,
            Ok(status) => {
                ngx_log_error!(
                    NGX_LOG_ERR,
                    request.log(),
                    "access_by_rss_file: unexpected VM status {status:?}"
                );
                HTTPStatus::INTERNAL_SERVER_ERROR.into()
            }
            Err(err) => {
                ngx_log_error!(NGX_LOG_ERR, request.log(), "access_by_rss_file: {err}");
                HTTPStatus::INTERNAL_SERVER_ERROR.into()
            }
        };
    }
    run_sleep_access(request, program)
}

fn run_sleep_access(request: &mut Request, program: &Arc<Program>) -> Status {
    let raw = request.as_mut() as *mut ngx_http_request_t;
    if request
        .get_module_ctx::<AccessCtx>(Module::module())
        .is_none()
    {
        let request_id = next_request_id();
        let exec = match crate::runtime::with_worker(|worker| {
            worker.bind_access_exec(Arc::clone(program), request_id)
        }) {
            Some(Ok(exec)) => exec,
            Some(Err(err)) => {
                ngx_log_error!(NGX_LOG_ERR, request.log(), "access_by_rss_file: {err}");
                return HTTPStatus::INTERNAL_SERVER_ERROR.into();
            }
            None => {
                ngx_log_error!(
                    NGX_LOG_ERR,
                    request.log(),
                    "access_by_rss_file: worker runtime is not started"
                );
                return HTTPStatus::INTERNAL_SERVER_ERROR.into();
            }
        };
        let ctx_ptr = request.pool().allocate(AccessCtx {
            exec: RefCell::new(exec),
        });
        if ctx_ptr.is_null() {
            ngx_log_error!(
                NGX_LOG_ERR,
                request.log(),
                "access_by_rss_file: no memory for request state"
            );
            return HTTPStatus::INTERNAL_SERVER_ERROR.into();
        }
        request.set_module_ctx(ctx_ptr.cast(), Module::module());
    }
    finish_from_raw(raw)
}

fn finish_from_raw(raw: *mut ngx_http_request_t) -> Status {
    // SAFETY: `raw` is the current access request on the worker main thread.
    let request = unsafe { Request::from_ngx_http_request(raw) };
    let Some(ctx) = request.get_module_ctx::<AccessCtx>(Module::module()) else {
        ngx_log_error!(
            NGX_LOG_ERR,
            request.log(),
            "access_by_rss_file: missing request state"
        );
        return HTTPStatus::INTERNAL_SERVER_ERROR.into();
    };
    let status = WORKER_ACCESS.with(|state| {
        let mut state = state.borrow_mut();
        ctx.exec
            .borrow_mut()
            .handle_access(&mut state.ids, &mut WorkerSleepSubmit)
    });
    if status == AccessStatus::Again {
        if let Some(token) = ctx.exec.borrow().current_token() {
            let request_id = ctx.exec.borrow().request_id();
            let inserted =
                WORKER_ACCESS.with(|state| state.borrow_mut().live.try_insert(token, raw));
            if inserted.is_err() {
                unregister_exact(token, request_id, Some(raw));
                ngx_log_error!(
                    NGX_LOG_ERR,
                    request.log(),
                    "access_by_rss_file: duplicate sleep token mapping"
                );
                return HTTPStatus::INTERNAL_SERVER_ERROR.into();
            }
            if !arm_read_close(raw) {
                unregister_exact(token, request_id, Some(raw));
                ngx_log_error!(
                    NGX_LOG_ERR,
                    request.log(),
                    "access_by_rss_file: failed to arm client-close read handler"
                );
                return HTTPStatus::INTERNAL_SERVER_ERROR.into();
            }
        }
    } else if status == AccessStatus::Error500 {
        ngx_log_error!(
            NGX_LOG_ERR,
            request.log(),
            "access_by_rss_file: sleep access failed"
        );
    }
    status.as_nginx()
}

fn next_request_id() -> u64 {
    WORKER_ACCESS.with(|state| {
        let mut state = state.borrow_mut();
        let id = state.next_id;
        state.next_id = state.next_id.saturating_add(1);
        if state.next_id == 0 {
            state.next_id = 1;
        }
        id
    })
}

fn unregister_exact(token: SleepToken, request_id: u64, raw: Option<*mut ngx_http_request_t>) {
    WORKER_ACCESS.with(|state| {
        let mut state = state.borrow_mut();
        let ids_removed = state.ids.remove_exact(token, &request_id).is_some();
        if let Some(raw) = raw {
            let _ = state.live.remove_exact(token, &raw);
        } else if ids_removed {
            state.live.remove(token);
        }
    });
}

/// Consume Task 4 ready tokens on the worker main event turn.
pub fn on_sleep_ready(tokens: Vec<SleepToken>) {
    let deferred = WORKER_ACCESS.with(|state| {
        let mut state = state.borrow_mut();
        core::mem::take(&mut state.deferred)
    });
    for token in deferred.into_iter().chain(tokens) {
        dispatch_ready_token(token);
    }
}

fn dispatch_ready_token(token: SleepToken) {
    let lookup = WORKER_ACCESS.with(|state| {
        let state = state.borrow();
        if state.in_handler {
            return ReadyLookup::InHandler;
        }
        match (
            state.ids.get(token).copied(),
            state.live.get(token).copied(),
        ) {
            (Some(request_id), Some(raw)) => ReadyLookup::Live { raw, request_id },
            _ => ReadyLookup::Unknown,
        }
    });
    let (raw, request_id) = match lookup {
        ReadyLookup::InHandler => {
            WORKER_ACCESS.with(|state| state.borrow_mut().deferred.push(token));
            return;
        }
        ReadyLookup::Unknown => return,
        ReadyLookup::Live { raw, request_id } => (raw, request_id),
    };
    let effect = apply_on_request(raw, token, request_id);
    match effect {
        ReadyEffect::Halted | ReadyEffect::Failed => {
            unregister_exact(token, request_id, Some(raw));
            restore_block_reading(raw);
            // SAFETY: the request stayed alive because the access handler
            // returned NGX_AGAIN without finalizing. Resume is on the worker
            // main thread after that handler returned.
            unsafe { nginx_sys::ngx_http_core_run_phases(raw) };
        }
        ReadyEffect::YieldedAgain => {
            WORKER_ACCESS.with(|state| {
                let _ = state.borrow_mut().live.remove_exact(token, &raw);
            });
            if let Some(next) = current_token_on_request(raw) {
                let _ = WORKER_ACCESS.with(|state| state.borrow_mut().live.try_insert(next, raw));
            }
        }
        ReadyEffect::Deferred => {
            WORKER_ACCESS.with(|state| state.borrow_mut().deferred.push(token));
        }
        ReadyEffect::Ignored => {}
    }
}

enum ReadyLookup {
    InHandler,
    Unknown,
    Live {
        raw: *mut ngx_http_request_t,
        request_id: u64,
    },
}

fn apply_on_request(
    raw: *mut ngx_http_request_t,
    token: SleepToken,
    request_id: u64,
) -> ReadyEffect {
    // SAFETY: both worker-local maps contained this exact token, so `raw`
    // is the live request that armed it. Stale/unknown tokens never reach here.
    let request = unsafe { Request::from_ngx_http_request(raw) };
    let Some(ctx) = request.get_module_ctx::<AccessCtx>(Module::module()) else {
        return ReadyEffect::Ignored;
    };
    if ctx.exec.borrow().request_id() != request_id || ctx.exec.borrow().is_cancelled() {
        return ReadyEffect::Ignored;
    }
    WORKER_ACCESS.with(|state| {
        let mut state = state.borrow_mut();
        ctx.exec
            .borrow_mut()
            .apply_ready(token, &mut state.ids, &mut WorkerSleepSubmit)
    })
}

fn current_token_on_request(raw: *mut ngx_http_request_t) -> Option<SleepToken> {
    // SAFETY: same liveness as [`apply_on_request`].
    let request = unsafe { Request::from_ngx_http_request(raw) };
    let ctx = request.get_module_ctx::<AccessCtx>(Module::module())?;
    ctx.exec.borrow().current_token()
}

/// Install the module read-close handler and re-arm like auth delay / limit_req.
///
/// `ngx_http_test_reading` is link-visible (`ngx_http.h`, nginx-sys `cda9d837`).
/// The wrapper only adds `catch_unwind` so a panic cannot cross the NGINX FFI.
fn arm_read_close(raw: *mut ngx_http_request_t) -> bool {
    if raw.is_null() {
        return false;
    }
    // SAFETY: `raw` is the live request that just returned NGX_AGAIN on this
    // worker thread. Connection and read event are NGINX-owned for that request.
    unsafe {
        let connection = (*raw).connection;
        if connection.is_null() {
            return false;
        }
        let rev = (*connection).read;
        if rev.is_null() {
            return false;
        }
        (*raw).read_event_handler = Some(access_read_close_handler);
        if (*rev).ready() != 0 {
            ngx_post_event(rev, core::ptr::addr_of_mut!(ngx_posted_events));
        } else if ngx_handle_read_event(rev, 0) != 0 {
            return false;
        }
        true
    }
}

fn restore_block_reading(raw: *mut ngx_http_request_t) {
    if raw.is_null() {
        return;
    }
    // SAFETY: called on the worker thread before phase continuation while `raw`
    // is still the suspended access request.
    unsafe {
        (*raw).read_event_handler = Some(ngx_http_block_reading);
    }
}

unsafe extern "C" fn access_read_close_handler(r: *mut ngx_http_request_t) {
    let _ = std::panic::catch_unwind(AssertUnwindSafe(|| {
        if r.is_null() {
            return;
        }
        unsafe { ngx_http_test_reading(r) };
    }));
}
