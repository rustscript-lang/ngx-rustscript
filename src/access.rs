//! Access-phase VM execution: sync Halted via the worker pool, or
//! generic async host suspend/resume through the worker eventfd bridge.
//!
//! Wait returns `NGX_AGAIN`. `ngx_http_core_access_phase` stops the checker
//! without advancing `phase_handler` and without incrementing `r->main->count`.
//! Resume runs on the worker main event turn after the original handler
//! returns: poll the waiting host op, resume the same VM, then
//! `ngx_http_core_run_phases` exactly once so this handler can return
//! `NGX_OK` or 500. Host-bound VMs are never returned to the Task 2 pool.

use std::cell::RefCell;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;

use nginx_sys::{
    ngx_handle_read_event, ngx_http_block_reading, ngx_http_request_t, ngx_int_t, ngx_post_event,
    ngx_posted_events,
};
use ngx::core::Status;
use ngx::ffi::NGX_LOG_ERR;
use ngx::http::{HTTPStatus, HttpModule, HttpModuleLocationConf, HttpRequestHandler, Request};
use ngx::ngx_log_error;
use vm::{Program, VmError, VmStatus};

use crate::Module;
use crate::config::{self, LocationConf};
use crate::host::{
    AllowedOps, NgxPhase, RequestLocalCtx, with_request_scope, with_vm_request_scope,
};
use crate::notify::{DEFAULT_MAX_IN_FLIGHT, RequestIdentity};
use crate::phase::PhaseSlot;
use crate::request_state::{AccessExec, AccessStatus, IdentityRegistry, ReadyEffect};
use crate::vm_pool;

unsafe extern "C" {
    fn ngx_rustscript_c_send_last_response(
        r: *mut ngx_http_request_t,
        status: ngx_int_t,
    ) -> ngx_int_t;
    fn ngx_rustscript_c_request_main_count_inc(r: *mut ngx_http_request_t);
    fn ngx_rustscript_c_test_reading_and_note_client_closed(r: *mut ngx_http_request_t);
}

struct RequestPhaseCtx {
    raw: *mut ngx_http_request_t,
    ngx_ctx: RefCell<RequestLocalCtx>,
    access: RefCell<Option<AccessExec>>,
    content: RefCell<Option<AccessExec>>,
    output: RefCell<Option<crate::host::OutputState>>,
}

impl RequestPhaseCtx {
    fn ngx_ctx_ptr(&self) -> *mut RequestLocalCtx {
        self.ngx_ctx.as_ptr()
    }

    fn slot(&self, slot: PhaseSlot) -> &RefCell<Option<AccessExec>> {
        match slot {
            PhaseSlot::Access => &self.access,
            PhaseSlot::Content => &self.content,
        }
    }

    fn matching_slot(&self, identity: RequestIdentity) -> Option<PhaseSlot> {
        if self
            .access
            .borrow()
            .as_ref()
            .is_some_and(|exec| exec.identity() == identity)
        {
            return Some(PhaseSlot::Access);
        }
        if self
            .content
            .borrow()
            .as_ref()
            .is_some_and(|exec| exec.identity() == identity)
        {
            return Some(PhaseSlot::Content);
        }
        None
    }

    fn with_attached_exec_mut<R>(
        &self,
        slot: PhaseSlot,
        f: impl FnOnce(&mut AccessExec) -> R,
    ) -> Option<R> {
        let ptr = self.ngx_ctx_ptr();
        let mut exec = self.slot(slot).borrow_mut();
        let exec = exec.as_mut()?;
        exec.attach_request_ctx_ptr(ptr);
        Some(f(exec))
    }
}

pub(crate) fn has_module_phase_ctx(raw: *mut ngx_http_request_t) -> bool {
    if raw.is_null() {
        return false;
    }
    unsafe {
        if (*raw).ctx.is_null() {
            return false;
        }
    }
    let module = Module::module();
    if module.ctx_index == nginx_sys::ngx_uint_t::MAX {
        return false;
    }
    let request = unsafe { Request::from_ngx_http_request(raw) };
    request.get_module_ctx::<RequestPhaseCtx>(module).is_some()
}

pub(crate) fn try_with_module_output<R>(
    raw: *mut ngx_http_request_t,
    f: impl FnOnce(&mut Option<crate::host::OutputState>) -> R,
) -> Option<R> {
    if raw.is_null() {
        return None;
    }
    unsafe {
        if (*raw).ctx.is_null() {
            return None;
        }
    }
    let module = Module::module();
    if module.ctx_index == nginx_sys::ngx_uint_t::MAX {
        return None;
    }
    // SAFETY: `raw` is a live request with a non-null ctx array and a valid
    // module ctx_index. Output hosts run only while the request is live.
    let request = unsafe { Request::from_ngx_http_request(raw) };
    let ctx = request.get_module_ctx::<RequestPhaseCtx>(module)?;
    Some(f(&mut ctx.output.borrow_mut()))
}

struct WorkerAccess {
    ids: IdentityRegistry<u64>,
    live: IdentityRegistry<*mut ngx_http_request_t>,
    in_handler: bool,
    deferred: Vec<RequestIdentity>,
    next_id: u64,
    next_generation: u64,
}

impl WorkerAccess {
    fn new() -> Self {
        Self {
            ids: IdentityRegistry::default(),
            live: IdentityRegistry::with_capacity(DEFAULT_MAX_IN_FLIGHT),
            in_handler: false,
            deferred: Vec::new(),
            next_id: 1,
            next_generation: 1,
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

/// Get or create the request-owned `ngx.ctx` store shared by access/content/log.
pub(crate) fn ensure_shared_ngx_ctx(
    request: &mut Request,
) -> Result<*mut RequestLocalCtx, &'static str> {
    if let Some(ctx) = request.get_module_ctx::<RequestPhaseCtx>(Module::module()) {
        return Ok(ctx.ngx_ctx_ptr());
    }
    let raw = request.as_mut() as *mut ngx_http_request_t;
    let phase_ctx = RequestPhaseCtx {
        raw,
        ngx_ctx: RefCell::new(RequestLocalCtx::new()),
        access: RefCell::new(None),
        content: RefCell::new(None),
        output: RefCell::new(None),
    };
    let ctx_ptr = request.pool().allocate(phase_ctx);
    if ctx_ptr.is_null() {
        return Err("no memory for request state");
    }
    request.set_module_ctx(ctx_ptr.cast(), Module::module());
    let Some(ctx) = request.get_module_ctx::<RequestPhaseCtx>(Module::module()) else {
        return Err("missing request module ctx after allocate");
    };
    Ok(ctx.ngx_ctx_ptr())
}

impl Drop for RequestPhaseCtx {
    fn drop(&mut self) {
        // `Pool::allocate` registers this Drop as `ngx_pool_cleanup_add`.
        // NGINX 1.30.4 runs it from `ngx_destroy_pool` inside
        // `ngx_http_free_request`, after `r->cleanup` handlers and after
        // `r->pool = NULL`. Cancel flush wait and restore the write handler
        // while `r` is still the request being freed, then drop the VM.
        crate::host::cleanup_output_slot(self.raw, self.output.get_mut());
        if let Some(exec) = self.access.get_mut() {
            cleanup_access_ctx(exec);
        }
        if let Some(exec) = self.content.get_mut() {
            cleanup_access_ctx(exec);
        }
    }
}

fn cleanup_access_ctx(exec: &mut AccessExec) {
    let request_id = exec.request_id();
    let identity = exec.identity();
    unregister_exact(identity, request_id, None);
    invalidate_deferred(identity);
    if crate::runtime::with_worker(|worker| exec.cleanup_wait(worker)).is_none() {
        let _ = exec.cleanup();
    }
}

fn invalidate_deferred(identity: RequestIdentity) {
    WORKER_ACCESS.with(|state| {
        state
            .borrow_mut()
            .deferred
            .retain(|queued| *queued != identity);
    });
}

/// Worker exit: cancel output waits, then drop identity maps.
pub(crate) fn clear_worker_access() {
    WORKER_ACCESS.with(|state| {
        let mut state = state.borrow_mut();
        for raw in state.live.iter_values().copied() {
            crate::host::cleanup_output(raw);
        }
        state.ids.clear();
        state.live.clear();
        state.deferred.clear();
        state.in_handler = false;
    });
}

fn mark_cached_clock_event_boundary() {
    crate::host::notify_cached_clock_event_boundary();
}

pub(crate) fn enter_phase_scope<R>(
    raw: *mut ngx_http_request_t,
    identity: RequestIdentity,
    phase: NgxPhase,
    f: impl FnOnce() -> R,
) -> R {
    mark_cached_clock_event_boundary();
    with_request_scope(raw, identity, phase, AllowedOps::for_phase(phase), f)
}

pub(crate) fn enter_phase_vm<R>(
    identity: RequestIdentity,
    phase: NgxPhase,
    f: impl FnOnce() -> R,
) -> R {
    with_vm_request_scope(identity, phase, AllowedOps::for_phase(phase), f)
}

pub(crate) fn allocate_scope_identity() -> RequestIdentity {
    let request_id = next_request_id();
    WORKER_ACCESS.with(|state| {
        let mut state = state.borrow_mut();
        let generation = state.next_generation;
        state.next_generation = state.next_generation.wrapping_add(1);
        if state.next_generation == 0 {
            state.next_generation = 1;
        }
        RequestIdentity::new(generation, request_id)
    })
}

/// Run a compiled program on a worker-local pooled VM until the first status.
pub fn run_access_program(program: &Arc<Program>) -> Result<VmStatus, VmError> {
    mark_cached_clock_event_boundary();
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
        run_phase_handler(request, PhaseSlot::Access)
    }
}

pub(crate) fn run_phase_handler(request: &mut Request, slot: PhaseSlot) -> Status {
    let _guard = HandlerGuard::enter();
    let Some(conf) = Module::location_conf(request) else {
        return Status::NGX_DECLINED;
    };
    run_configured_phase(request, conf, slot)
}

fn run_configured_phase(request: &mut Request, conf: &LocationConf, slot: PhaseSlot) -> Status {
    let Some(program) = (match slot {
        PhaseSlot::Access => conf.access_program(),
        PhaseSlot::Content => conf.content_program(),
    }) else {
        return Status::NGX_DECLINED;
    };
    let phase = slot.phase();
    let directive = slot.directive();
    if !config::program_requires_request_state(program) {
        // Pool fast path: no Async and no RequestCtx. Keep ctx-null VM scope;
        // do not wrap a pooled VM with a stack ctx.
        let raw = request.as_mut() as *mut ngx_http_request_t;
        let identity = allocate_scope_identity();
        return enter_phase_scope(raw, identity, phase, || {
            enter_phase_vm(identity, phase, || match run_access_program(program) {
                Ok(VmStatus::Halted) => complete_sync_ok(raw, slot),
                Ok(status) => {
                    ngx_log_error!(
                        NGX_LOG_ERR,
                        request.log(),
                        "{directive}: unexpected VM status {status:?}"
                    );
                    HTTPStatus::INTERNAL_SERVER_ERROR.into()
                }
                Err(err) => {
                    ngx_log_error!(NGX_LOG_ERR, request.log(), "{directive}: {err}");
                    HTTPStatus::INTERNAL_SERVER_ERROR.into()
                }
            })
        });
    }
    run_request_owned_phase(request, program, slot)
}

fn complete_sync_ok(raw: *mut ngx_http_request_t, slot: PhaseSlot) -> Status {
    if slot == PhaseSlot::Content {
        return send_empty_last(raw);
    }
    Status::NGX_OK
}

fn send_empty_last(raw: *mut ngx_http_request_t) -> Status {
    Status(crate::host::finish_response(raw))
}

fn run_request_owned_phase(
    request: &mut Request,
    program: &Arc<Program>,
    slot: PhaseSlot,
) -> Status {
    let raw = request.as_mut() as *mut ngx_http_request_t;
    let directive = slot.directive();
    if request
        .get_module_ctx::<RequestPhaseCtx>(Module::module())
        .is_none_or(|ctx| ctx.slot(slot).borrow().is_none())
    {
        let request_id = next_request_id();
        let mut exec = match crate::runtime::with_worker(|worker| {
            worker.bind_phase_exec(Arc::clone(program), request_id, slot.phase())
        }) {
            Some(Ok(exec)) => exec,
            Some(Err(err)) => {
                ngx_log_error!(NGX_LOG_ERR, request.log(), "{directive}: {err}");
                return HTTPStatus::INTERNAL_SERVER_ERROR.into();
            }
            None => {
                ngx_log_error!(
                    NGX_LOG_ERR,
                    request.log(),
                    "{directive}: worker runtime is not started"
                );
                return HTTPStatus::INTERNAL_SERVER_ERROR.into();
            }
        };
        if let Some(ctx) = request.get_module_ctx::<RequestPhaseCtx>(Module::module()) {
            exec.attach_request_ctx_ptr(ctx.ngx_ctx_ptr());
            *ctx.slot(slot).borrow_mut() = Some(exec);
        } else {
            let phase_ctx = RequestPhaseCtx {
                raw,
                ngx_ctx: RefCell::new(RequestLocalCtx::new()),
                access: RefCell::new(None),
                content: RefCell::new(None),
                output: RefCell::new(None),
            };
            let ctx_ptr = request.pool().allocate(phase_ctx);
            if ctx_ptr.is_null() {
                ngx_log_error!(
                    NGX_LOG_ERR,
                    request.log(),
                    "{directive}: no memory for request state"
                );
                return HTTPStatus::INTERNAL_SERVER_ERROR.into();
            }
            request.set_module_ctx(ctx_ptr.cast(), Module::module());
            let Some(ctx) = request.get_module_ctx::<RequestPhaseCtx>(Module::module()) else {
                ngx_log_error!(
                    NGX_LOG_ERR,
                    request.log(),
                    "{directive}: missing request module ctx after allocate"
                );
                return HTTPStatus::INTERNAL_SERVER_ERROR.into();
            };
            exec.attach_request_ctx_ptr(ctx.ngx_ctx_ptr());
            *ctx.slot(slot).borrow_mut() = Some(exec);
        }
    }
    finish_from_raw(raw, slot)
}

fn finish_from_raw(raw: *mut ngx_http_request_t, slot: PhaseSlot) -> Status {
    // SAFETY: `raw` is the current phase request on the worker main thread.
    let request = unsafe { Request::from_ngx_http_request(raw) };
    let directive = slot.directive();
    let Some(ctx) = request.get_module_ctx::<RequestPhaseCtx>(Module::module()) else {
        ngx_log_error!(
            NGX_LOG_ERR,
            request.log(),
            "{directive}: missing request state"
        );
        return HTTPStatus::INTERNAL_SERVER_ERROR.into();
    };
    let exec = ctx.slot(slot);
    if exec.borrow().is_none() {
        ngx_log_error!(
            NGX_LOG_ERR,
            request.log(),
            "{directive}: missing request state"
        );
        return HTTPStatus::INTERNAL_SERVER_ERROR.into();
    }
    let status = {
        let identity = exec.borrow().as_ref().expect("checked above").identity();
        enter_phase_scope(raw, identity, slot.phase(), || {
            crate::runtime::with_worker(|worker| {
                WORKER_ACCESS.with(|state| {
                    let mut state = state.borrow_mut();
                    ctx.with_attached_exec_mut(slot, |exec| {
                        exec.handle_access(&mut state.ids, worker)
                    })
                    .unwrap_or(AccessStatus::Error500)
                })
            })
            .unwrap_or(AccessStatus::Error500)
        })
    };
    if status == AccessStatus::Again {
        let identity = exec
            .borrow()
            .as_ref()
            .and_then(AccessExec::current_identity);
        if let Some(identity) = identity {
            let request_id = exec.borrow().as_ref().expect("checked above").request_id();
            let inserted =
                WORKER_ACCESS.with(|state| state.borrow_mut().live.try_insert(identity, raw));
            if inserted.is_err() {
                unregister_exact(identity, request_id, Some(raw));
                ngx_log_error!(
                    NGX_LOG_ERR,
                    request.log(),
                    "{directive}: duplicate async wait mapping"
                );
                return HTTPStatus::INTERNAL_SERVER_ERROR.into();
            }
            if !arm_read_close(raw) {
                unregister_exact(identity, request_id, Some(raw));
                ngx_log_error!(
                    NGX_LOG_ERR,
                    request.log(),
                    "{directive}: failed to arm client-close read handler"
                );
                return HTTPStatus::INTERNAL_SERVER_ERROR.into();
            }
            exec.borrow_mut()
                .as_mut()
                .expect("checked above")
                .commit_suspend();
        }
    } else if status == AccessStatus::Error500 {
        ngx_log_error!(
            NGX_LOG_ERR,
            request.log(),
            "{directive}: async execution failed"
        );
    }
    complete_phase_status(raw, slot, status)
}

fn complete_phase_status(
    raw: *mut ngx_http_request_t,
    slot: PhaseSlot,
    status: AccessStatus,
) -> Status {
    match status {
        AccessStatus::Again if slot == PhaseSlot::Content => {
            #[cfg(ngx_rustscript_integration_hooks)]
            crate::request_state::record_wait_count_inc();
            unsafe { ngx_rustscript_c_request_main_count_inc(raw) };
            Status::NGX_DONE
        }
        AccessStatus::Ok if slot == PhaseSlot::Content => {
            let rc = send_empty_last(raw);
            crate::host::cleanup_output(raw);
            rc
        }
        AccessStatus::Error500 if slot == PhaseSlot::Content => {
            let _ = crate::host::finish_response_on_error(raw);
            crate::host::cleanup_output(raw);
            HTTPStatus::INTERNAL_SERVER_ERROR.into()
        }
        AccessStatus::Finalize(code) if slot == PhaseSlot::Content => {
            let _ = crate::host::finish_response_on_finalize(raw);
            crate::host::cleanup_output(raw);
            apply_finalize_send(raw, code)
        }
        AccessStatus::Finalize(code) => apply_finalize_send(raw, code),
        other => other.as_nginx(),
    }
}

fn apply_finalize_send(raw: *mut ngx_http_request_t, code: i32) -> Status {
    let rc = unsafe { ngx_rustscript_c_send_last_response(raw, code as ngx_int_t) };
    if rc == Status::NGX_ERROR.0 || rc > Status::NGX_OK.0 {
        return Status(rc);
    }
    AccessStatus::Finalize(code).as_nginx()
}

pub(crate) fn next_request_id() -> u64 {
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

fn unregister_exact(
    identity: RequestIdentity,
    request_id: u64,
    raw: Option<*mut ngx_http_request_t>,
) {
    WORKER_ACCESS.with(|state| {
        let mut state = state.borrow_mut();
        let ids_removed = state.ids.remove_exact(identity, &request_id).is_some();
        if let Some(raw) = raw {
            let _ = state.live.remove_exact(identity, &raw);
        } else if ids_removed {
            state.live.remove(identity);
        }
    });
}

/// Consume ready identities on the worker main event turn.
pub fn on_async_ready(identities: Vec<RequestIdentity>) {
    mark_cached_clock_event_boundary();
    let deferred = WORKER_ACCESS.with(|state| {
        let mut state = state.borrow_mut();
        core::mem::take(&mut state.deferred)
    });
    for identity in deferred.into_iter().chain(identities) {
        dispatch_ready_identity(identity);
    }
}

fn dispatch_ready_identity(identity: RequestIdentity) {
    let lookup = WORKER_ACCESS.with(|state| {
        let state = state.borrow();
        if state.in_handler {
            return ReadyLookup::InHandler;
        }
        match (
            state.ids.get(identity).copied(),
            state.live.get(identity).copied(),
        ) {
            (Some(request_id), Some(raw)) => ReadyLookup::Live { raw, request_id },
            _ => ReadyLookup::Unknown,
        }
    });
    let (raw, request_id) = match lookup {
        ReadyLookup::InHandler => {
            WORKER_ACCESS.with(|state| state.borrow_mut().deferred.push(identity));
            return;
        }
        ReadyLookup::Unknown => return,
        ReadyLookup::Live { raw, request_id } => (raw, request_id),
    };
    let effect = apply_on_request(raw, identity, request_id);
    match effect {
        ReadyEffect::Halted | ReadyEffect::Failed => {
            unregister_exact(identity, request_id, Some(raw));
            restore_block_reading(raw);
            // SAFETY: the request stayed alive because the access handler
            // returned NGX_AGAIN without finalizing. Resume is on the worker
            // main thread after that handler returned.
            unsafe { nginx_sys::ngx_http_core_run_phases(raw) };
        }
        ReadyEffect::WaitingAgain => {
            WORKER_ACCESS.with(|state| {
                let _ = state.borrow_mut().live.remove_exact(identity, &raw);
            });
            if let Some(next) = current_identity_on_request(raw) {
                let _ = WORKER_ACCESS.with(|state| state.borrow_mut().live.try_insert(next, raw));
            }
        }
        ReadyEffect::Deferred => {
            WORKER_ACCESS.with(|state| state.borrow_mut().deferred.push(identity));
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
    identity: RequestIdentity,
    request_id: u64,
) -> ReadyEffect {
    // SAFETY: both worker-local maps contained this exact identity, so `raw`
    // is the live request that armed it. Stale/unknown identities never reach here.
    let request = unsafe { Request::from_ngx_http_request(raw) };
    let Some(ctx) = request.get_module_ctx::<RequestPhaseCtx>(Module::module()) else {
        return ReadyEffect::Ignored;
    };
    let Some(slot) = ctx.matching_slot(identity) else {
        return ReadyEffect::Ignored;
    };
    let exec = ctx.slot(slot);
    if exec
        .borrow()
        .as_ref()
        .is_none_or(|exec| exec.request_id() != request_id || exec.is_cancelled())
    {
        return ReadyEffect::Ignored;
    }
    crate::runtime::with_worker(|worker| {
        WORKER_ACCESS.with(|state| {
            let mut state = state.borrow_mut();
            enter_phase_scope(raw, identity, slot.phase(), || {
                ctx.with_attached_exec_mut(slot, |exec| {
                    exec.apply_ready(identity, &mut state.ids, worker)
                })
                .unwrap_or(ReadyEffect::Ignored)
            })
        })
    })
    .unwrap_or(ReadyEffect::Ignored)
}

fn current_identity_on_request(raw: *mut ngx_http_request_t) -> Option<RequestIdentity> {
    // SAFETY: same liveness as [`apply_on_request`].
    let request = unsafe { Request::from_ngx_http_request(raw) };
    let ctx = request.get_module_ctx::<RequestPhaseCtx>(Module::module())?;
    if let Some(identity) = ctx
        .access
        .borrow()
        .as_ref()
        .and_then(AccessExec::current_identity)
    {
        return Some(identity);
    }
    ctx.content
        .borrow()
        .as_ref()
        .and_then(AccessExec::current_identity)
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
        unsafe { ngx_rustscript_c_test_reading_and_note_client_closed(r) };
    }));
}
