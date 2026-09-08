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
use std::sync::Arc;

use nginx_sys::ngx_http_request_t;
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
        let exec = self.exec.get_mut();
        if let Some(token) = exec.current_token() {
            unregister_exact(token, exec.request_id(), None);
        }
    }
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
        match state.live.get(token).copied() {
            Some(raw) => ReadyLookup::Live(raw),
            None => ReadyLookup::Unknown,
        }
    });
    let raw = match lookup {
        ReadyLookup::InHandler => {
            WORKER_ACCESS.with(|state| state.borrow_mut().deferred.push(token));
            return;
        }
        ReadyLookup::Unknown => return,
        ReadyLookup::Live(raw) => raw,
    };
    let effect = apply_on_request(raw, token);
    match effect {
        ReadyEffect::Halted | ReadyEffect::Failed => {
            let request_id = request_id_on_request(raw);
            unregister_exact(token, request_id, Some(raw));
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
    Live(*mut ngx_http_request_t),
}

fn apply_on_request(raw: *mut ngx_http_request_t, token: SleepToken) -> ReadyEffect {
    // SAFETY: `raw` came from the worker-local live map for a request that
    // returned NGX_AGAIN and has not been cleaned up.
    let request = unsafe { Request::from_ngx_http_request(raw) };
    let Some(ctx) = request.get_module_ctx::<AccessCtx>(Module::module()) else {
        return ReadyEffect::Ignored;
    };
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

fn request_id_on_request(raw: *mut ngx_http_request_t) -> u64 {
    // SAFETY: same liveness as [`apply_on_request`].
    let request = unsafe { Request::from_ngx_http_request(raw) };
    request
        .get_module_ctx::<AccessCtx>(Module::module())
        .map(|ctx| ctx.exec.borrow().request_id())
        .unwrap_or(0)
}
