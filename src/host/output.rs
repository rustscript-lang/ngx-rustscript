// Content-phase response output: send_headers, print, say, flush, eof.
//
// Payload is copied on the worker thread into request-pool buffers and sent
// through `ngx_http_output_filter`. Output state is request-owned (module
// `RequestPhaseCtx`, with a stub fallback only when module ctx is absent).
// `flush(wait=true)` waits on NGINX write backpressure through a request-owned
// completion cell and eventfd; the future never holds a raw request pointer.

#[cfg(any(test, ngx_features = "rustc-test"))]
use std::cell::RefCell;
#[cfg(any(test, ngx_features = "rustc-test"))]
use std::collections::HashMap;
use std::future::Future;
use std::os::raw::c_uint;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

use nginx_sys::{
    ngx_http_event_handler_pt, ngx_http_request_t, ngx_int_t, NGX_AGAIN, NGX_DONE, NGX_ERROR,
    NGX_HTTP_CLIENT_CLOSED_REQUEST, NGX_HTTP_REQUEST_TIME_OUT, NGX_OK,
};
use ngx::http::Request;
use pd_host_function::pd_host_function;
use vm::{Value, VmError};

use super::super::VmResult;
use super::{CallOutcome, IntoHostCallOutcome};
use crate::host::{
    AllowedOps, NgxPhase, ScopeError, current_identity, current_phase, with_current_request_mut_ops,
};
use crate::notify::RequestIdentity;

const MAX_OUTPUT_CALL_BYTES: usize = 1_048_576;
const MAX_OUTPUT_TOTAL_BYTES: usize = 8_388_608;
const MAX_OUTPUT_CHUNKS: usize = 4_096;
const MAX_OUTPUT_DEPTH: usize = 8;
const MAX_OUTPUT_ARRAY_ELEMS: usize = 1_024;

fn ngx_ok() -> ngx_int_t {
    NGX_OK as ngx_int_t
}

fn ngx_error() -> ngx_int_t {
    NGX_ERROR as ngx_int_t
}

fn ngx_again() -> ngx_int_t {
    NGX_AGAIN as ngx_int_t
}

fn ngx_done() -> ngx_int_t {
    NGX_DONE as ngx_int_t
}

fn ret<T: IntoHostCallOutcome>(value: T) -> VmResult<CallOutcome> {
    Ok(value.into_host_call_outcome())
}

fn scope_err(err: ScopeError) -> VmError {
    VmError::HostError(err.to_string())
}

fn host_err(message: impl Into<String>) -> VmError {
    VmError::HostError(message.into())
}

fn require_content() -> VmResult<()> {
    match current_phase() {
        Ok(NgxPhase::Content) => Ok(()),
        Ok(phase) => Err(host_err(format!(
            "output APIs are only available in the content phase, not {phase:?}"
        ))),
        Err(err) => Err(scope_err(err)),
    }
}

fn require_identity(identity: RequestIdentity) -> VmResult<RequestIdentity> {
    if identity.generation() == 0 || identity.request_id() == 0 {
        return Err(host_err("ngx output requires a live request identity"));
    }
    Ok(identity)
}

unsafe extern "C" {
    fn ngx_rustscript_c_request_headers_sent(r: *const ngx_http_request_t) -> c_uint;
    fn ngx_rustscript_c_request_header_only(r: *const ngx_http_request_t) -> c_uint;
    fn ngx_rustscript_c_request_is_http10(r: *const ngx_http_request_t) -> c_uint;
    fn ngx_rustscript_c_content_length_n(r: *const ngx_http_request_t) -> i64;
    fn ngx_rustscript_c_send_response_headers(r: *mut ngx_http_request_t) -> ngx_int_t;
    fn ngx_rustscript_c_output_body(
        r: *mut ngx_http_request_t,
        data: *const u8,
        len: usize,
        flush: c_uint,
        last: c_uint,
    ) -> ngx_int_t;
    fn ngx_rustscript_c_connection_needs_flush_wait(r: *mut ngx_http_request_t) -> c_uint;
    fn ngx_rustscript_c_arm_flush_wait(
        r: *mut ngx_http_request_t,
        handler: unsafe extern "C" fn(*mut ngx_http_request_t),
    ) -> ngx_int_t;
    fn ngx_rustscript_c_continue_output(r: *mut ngx_http_request_t) -> ngx_int_t;
    fn ngx_rustscript_c_disarm_flush_wait(r: *mut ngx_http_request_t);
    fn ngx_rustscript_c_set_content_length(r: *mut ngx_http_request_t, n: i64);
    fn ngx_rustscript_c_note_client_closed_log_status(r: *mut ngx_http_request_t);
}

/// Request-owned output / flush-wait state. Lives in `RequestPhaseCtx` in
/// production. Stub tests without module ctx use [`FALLBACK_OUTPUT`], compiled
/// only under `cfg(test)` / `ngx_features = "rustc-test"`.
pub(crate) struct OutputState {
    identity: RequestIdentity,
    eof: bool,
    header_sent: bool,
    buffering: bool,
    buffered: Vec<u8>,
    bytes: usize,
    chunks: usize,
    emitted: i64,
    flush_generation: u64,
    flush_wait: Option<FlushWaitSlot>,
    saved_write_handler: Option<ngx_http_event_handler_pt>,
    restored: bool,
}

struct FlushWaitSlot {
    cell: Arc<FlushWaitCell>,
    generation: u64,
    identity: RequestIdentity,
}

impl OutputState {
    fn new(identity: RequestIdentity, buffering: bool) -> Self {
        Self {
            identity,
            eof: false,
            header_sent: false,
            buffering,
            buffered: Vec::new(),
            bytes: 0,
            chunks: 0,
            emitted: 0,
            flush_generation: 0,
            flush_wait: None,
            saved_write_handler: None,
            restored: false,
        }
    }
}

#[cfg(any(test, ngx_features = "rustc-test"))]
thread_local! {
    static FALLBACK_OUTPUT: RefCell<HashMap<usize, OutputState>> = RefCell::new(HashMap::new());
}

struct FlushWaitCell {
    result: Mutex<Option<Result<(), String>>>,
    waker: Mutex<Option<Waker>>,
}

impl FlushWaitCell {
    fn new() -> Self {
        Self {
            result: Mutex::new(None),
            waker: Mutex::new(None),
        }
    }

    fn complete(&self, result: Result<(), String>) {
        if let Ok(mut slot) = self.result.lock() {
            if slot.is_none() {
                *slot = Some(result);
            }
        }
        if let Ok(mut waker) = self.waker.lock() {
            if let Some(waker) = waker.take() {
                waker.wake();
            }
        }
    }
}

struct FlushWait {
    cell: Arc<FlushWaitCell>,
}

impl Future for FlushWait {
    type Output = Result<(), String>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if let Ok(slot) = self.cell.result.lock() {
            if let Some(result) = slot.clone() {
                return Poll::Ready(result);
            }
        }
        if let Ok(mut waker) = self.cell.waker.lock() {
            *waker = Some(cx.waker().clone());
        }
        Poll::Pending
    }
}

fn headers_sent(raw: *mut ngx_http_request_t) -> bool {
    unsafe { ngx_rustscript_c_request_headers_sent(raw) != 0 }
}

fn is_http10(raw: *mut ngx_http_request_t) -> bool {
    unsafe { ngx_rustscript_c_request_is_http10(raw) != 0 }
}

fn header_only(raw: *mut ngx_http_request_t) -> bool {
    unsafe { ngx_rustscript_c_request_header_only(raw) != 0 }
}

fn declared_content_length(raw: *mut ngx_http_request_t) -> i64 {
    unsafe { ngx_rustscript_c_content_length_n(raw) }
}

fn http10_should_buffer(raw: *mut ngx_http_request_t) -> bool {
    is_http10(raw) && !headers_sent(raw) && declared_content_length(raw) < 0
}

enum FilterKind {
    Header,
    Body,
}

fn filter_rc(rc: ngx_int_t, what: &str, kind: FilterKind) -> VmResult<()> {
    if rc == ngx_ok() {
        return Ok(());
    }
    match kind {
        FilterKind::Header => {
            if rc == ngx_done() {
                Ok(())
            } else {
                Err(host_err(format!("{what} failed")))
            }
        }
        FilterKind::Body => {
            // NGX_AGAIN: the chain is already in the filter; later flush(wait)
            // waits for the writer. NGX_DONE: this chain is complete; do not
            // treat it as a failure (body is not lost) and do not retry it.
            if rc == ngx_again() || rc == ngx_done() {
                Ok(())
            } else {
                Err(host_err(format!("{what} failed")))
            }
        }
    }
}

#[cfg(not(any(test, ngx_features = "rustc-test")))]
fn missing_module_ctx_err() -> VmError {
    host_err("ngx output requires request module ctx")
}

#[cfg(any(test, ngx_features = "rustc-test"))]
fn fallback_with_output_slot<R>(
    raw: *mut ngx_http_request_t,
    f: impl FnOnce(&mut Option<OutputState>) -> R,
) -> R {
    FALLBACK_OUTPUT.with(|map| {
        let mut map = map.borrow_mut();
        let key = raw as usize;
        let mut slot = map.remove(&key);
        let result = f(&mut slot);
        if let Some(state) = slot {
            map.insert(key, state);
        }
        result
    })
}

fn with_output_slot<R>(
    raw: *mut ngx_http_request_t,
    f: impl FnOnce(&mut Option<OutputState>) -> R,
) -> R {
    if crate::access::has_module_phase_ctx(raw) {
        crate::access::try_with_module_output(raw, f).expect("module output ctx vanished")
    } else {
        #[cfg(any(test, ngx_features = "rustc-test"))]
        {
            fallback_with_output_slot(raw, f)
        }
        #[cfg(not(any(test, ngx_features = "rustc-test")))]
        {
            // Production fail-closed: never store pointer-keyed output state.
            let mut slot = None;
            f(&mut slot)
        }
    }
}

fn with_output<T>(
    raw: *mut ngx_http_request_t,
    f: impl FnOnce(&mut OutputState) -> VmResult<T>,
) -> VmResult<T> {
    let identity = require_identity(current_identity().map_err(scope_err)?)?;
    #[cfg(not(any(test, ngx_features = "rustc-test")))]
    if !crate::access::has_module_phase_ctx(raw) {
        return Err(missing_module_ctx_err());
    }
    with_output_slot(raw, |slot| {
        match slot.as_mut() {
            Some(state) if state.identity == identity => f(state),
            Some(state) => {
                // Pointer reuse: never inherit eof / flush wait / buffers.
                cancel_flush_wait(raw, state, OutputCleanupReason::IdentitySuperseded);
                *slot = Some(OutputState::new(identity, http10_should_buffer(raw)));
                f(slot.as_mut().expect("just inserted"))
            }
            None => {
                #[cfg(not(any(test, ngx_features = "rustc-test")))]
                if !crate::access::has_module_phase_ctx(raw) {
                    return Err(missing_module_ctx_err());
                }
                *slot = Some(OutputState::new(identity, http10_should_buffer(raw)));
                f(slot.as_mut().expect("just inserted"))
            }
        }
    })
}

/// Single cleanup owner: cancel/complete flush wait, restore the saved
/// request write handler exactly once, drop timer/event state, clear output.
pub(crate) fn cleanup_output_slot(raw: *mut ngx_http_request_t, slot: &mut Option<OutputState>) {
    if let Some(mut state) = slot.take() {
        cancel_flush_wait(raw, &mut state, OutputCleanupReason::RequestTeardown);
    }
}

/// Clear request-owned output state on every content exit path.
pub fn cleanup_output(raw: *mut ngx_http_request_t) {
    if raw.is_null() {
        return;
    }
    with_output_slot(raw, |slot| cleanup_output_slot(raw, slot));
}

fn restore_write_handler(raw: *mut ngx_http_request_t, state: &mut OutputState) {
    if state.restored {
        return;
    }
    state.restored = true;
    if let Some(handler) = state.saved_write_handler.take() {
        // SAFETY: `raw` is the live request whose write handler we replaced.
        unsafe {
            (*raw).write_event_handler = handler;
        }
    }
}

#[derive(Clone, Copy)]
enum OutputCleanupReason {
    NormalFinish,
    TerminalError,
    IdentitySuperseded,
    RequestTeardown,
    ConfirmedClientAbort,
}

impl OutputCleanupReason {
    fn waiter_message(self) -> &'static str {
        match self {
            Self::NormalFinish => "finished",
            Self::TerminalError => "error",
            Self::IdentitySuperseded => "identity superseded",
            Self::RequestTeardown => "request teardown",
            Self::ConfirmedClientAbort => "client aborted",
        }
    }
}

fn cancel_flush_wait(
    raw: *mut ngx_http_request_t,
    state: &mut OutputState,
    reason: OutputCleanupReason,
) {
    restore_write_handler(raw, state);
    if !raw.is_null() {
        unsafe { ngx_rustscript_c_disarm_flush_wait(raw) };
        if matches!(reason, OutputCleanupReason::ConfirmedClientAbort) {
            unsafe { ngx_rustscript_c_note_client_closed_log_status(raw) };
        }
    }
    if let Some(wait) = state.flush_wait.take() {
        wait.cell.complete(Err(reason.waiter_message().to_string()));
    }
}

fn complete_flush_wait(
    raw: *mut ngx_http_request_t,
    state: &mut OutputState,
    result: Result<(), String>,
) {
    restore_write_handler(raw, state);
    unsafe { ngx_rustscript_c_disarm_flush_wait(raw) };
    if let Some(wait) = state.flush_wait.take() {
        wait.cell.complete(result);
    }
}

fn send_headers_inner(raw: *mut ngx_http_request_t, state: &mut OutputState) -> VmResult<()> {
    if headers_sent(raw) || state.header_sent {
        state.header_sent = true;
        state.buffering = false;
        return Ok(());
    }
    if state.buffering && !state.buffered.is_empty() && declared_content_length(raw) < 0 {
        unsafe {
            ngx_rustscript_c_set_content_length(raw, state.buffered.len() as i64);
        }
    }
    let rc = unsafe { ngx_rustscript_c_send_response_headers(raw) };
    filter_rc(rc, "ngx::send_headers", FilterKind::Header)?;
    if !headers_sent(raw) && rc != ngx_ok() && rc != ngx_done() {
        return Err(host_err("ngx::send_headers failed"));
    }
    state.header_sent = true;
    if state.buffering && !state.buffered.is_empty() {
        let payload = std::mem::take(&mut state.buffered);
        state.buffering = false;
        emit_body(raw, state, &payload, 0, 0)?;
    } else {
        state.buffering = false;
    }
    Ok(())
}

fn emit_body(
    raw: *mut ngx_http_request_t,
    state: &mut OutputState,
    payload: &[u8],
    flush: c_uint,
    last: c_uint,
) -> VmResult<()> {
    if payload.is_empty() && flush == 0 && last == 0 {
        return Ok(());
    }
    if payload.len() > MAX_OUTPUT_CALL_BYTES {
        return Err(host_err(format!(
            "output chunk exceeds the {MAX_OUTPUT_CALL_BYTES}-byte per-call limit"
        )));
    }
    let next_bytes = state.bytes.saturating_add(payload.len());
    if next_bytes > MAX_OUTPUT_TOTAL_BYTES {
        return Err(host_err(format!(
            "response exceeds the {MAX_OUTPUT_TOTAL_BYTES}-byte total limit"
        )));
    }
    if state.chunks >= MAX_OUTPUT_CHUNKS {
        return Err(host_err(format!(
            "response exceeds the {MAX_OUTPUT_CHUNKS}-chunk limit"
        )));
    }
    let declared = declared_content_length(raw);
    if last == 0 && declared >= 0 {
        let next_emitted = state.emitted.saturating_add(payload.len() as i64);
        if next_emitted > declared {
            return Err(host_err("body exceeds Content-Length"));
        }
    }
    state.bytes = next_bytes;
    state.chunks += 1;
    state.emitted = state.emitted.saturating_add(payload.len() as i64);
    let rc = unsafe {
        ngx_rustscript_c_output_body(raw, payload.as_ptr(), payload.len(), flush, last)
    };
    filter_rc(rc, "ngx output filter", FilterKind::Body)
}

fn push_bytes(out: &mut Vec<u8>, bytes: &[u8]) -> VmResult<()> {
    if out.len().saturating_add(bytes.len()) > MAX_OUTPUT_CALL_BYTES {
        return Err(host_err(format!(
            "output chunk exceeds the {MAX_OUTPUT_CALL_BYTES}-byte per-call limit"
        )));
    }
    out.extend_from_slice(bytes);
    Ok(())
}

fn append_output(out: &mut Vec<u8>, value: &Value, depth: usize) -> VmResult<()> {
    if depth > MAX_OUTPUT_DEPTH {
        return Err(host_err("output value is nested too deeply"));
    }
    match value {
        Value::Null => Ok(()),
        Value::Bool(true) => push_bytes(out, b"true"),
        Value::Bool(false) => push_bytes(out, b"false"),
        Value::Int(number) => push_bytes(out, number.to_string().as_bytes()),
        Value::Float(number) => {
            if number.is_nan() || number.is_infinite() {
                return Err(host_err("output rejects non-finite numbers"));
            }
            let text = if number.fract() == 0.0 && number.abs() < 1e15 {
                (*number as i64).to_string()
            } else {
                number.to_string()
            };
            push_bytes(out, text.as_bytes())
        }
        Value::String(text) => push_bytes(out, text.as_bytes()),
        Value::Bytes(bytes) => push_bytes(out, bytes.as_ref()),
        Value::Array(items) => {
            if items.len() > MAX_OUTPUT_ARRAY_ELEMS {
                return Err(host_err(format!(
                    "output array exceeds the {MAX_OUTPUT_ARRAY_ELEMS}-element limit"
                )));
            }
            for item in items.iter() {
                append_output(out, item, depth + 1)?;
            }
            Ok(())
        }
        Value::Map(_) | Value::Callable(_) => {
            Err(host_err("output cannot encode maps or functions"))
        }
    }
}

fn write_body(raw: *mut ngx_http_request_t, payload: &[u8], newline: bool) -> VmResult<()> {
    with_output(raw, |state| {
        if state.eof {
            return Err(host_err("seen eof"));
        }
        if header_only(raw) {
            send_headers_inner(raw, state)?;
            return Ok(());
        }
        let mut body = Vec::new();
        push_bytes(&mut body, payload)?;
        if newline {
            push_bytes(&mut body, b"\n")?;
        }
        if state.buffering && http10_should_buffer(raw) {
            if state
                .buffered
                .len()
                .saturating_add(body.len())
                .saturating_add(state.bytes)
                > MAX_OUTPUT_TOTAL_BYTES
            {
                return Err(host_err(format!(
                    "response exceeds the {MAX_OUTPUT_TOTAL_BYTES}-byte total limit"
                )));
            }
            state.buffered.extend_from_slice(&body);
            return Ok(());
        }
        state.buffering = false;
        send_headers_inner(raw, state)?;
        emit_body(raw, state, &body, 0, 0)
    })
}

fn send_eof(raw: *mut ngx_http_request_t) -> VmResult<()> {
    with_output(raw, |state| {
        if state.eof {
            return Err(host_err("seen eof"));
        }
        close_output_state(raw, state)
    })
}

fn close_output_state(raw: *mut ngx_http_request_t, state: &mut OutputState) -> VmResult<()> {
    if header_only(raw) {
        send_headers_inner(raw, state)?;
        emit_body(raw, state, &[], 0, 1)?;
        state.eof = true;
        return Ok(());
    }
    if state.buffering {
        if declared_content_length(raw) < 0 {
            unsafe {
                ngx_rustscript_c_set_content_length(raw, state.buffered.len() as i64);
            }
        }
        send_headers_inner(raw, state)?;
        let payload = std::mem::take(&mut state.buffered);
        emit_body(raw, state, &payload, 0, 1)?;
        state.eof = true;
        return Ok(());
    }
    send_headers_inner(raw, state)?;
    emit_body(raw, state, &[], 0, 1)?;
    state.eof = true;
    Ok(())
}

/// Send remaining last_buf from the content handler without duplicating eof.
pub(crate) fn finish_response(raw: *mut ngx_http_request_t) -> ngx_int_t {
    finish_content(raw, ContentTerminal::Ok)
}

/// Auto-last on error after output, without overwriting an already-sent status.
pub(crate) fn finish_response_on_error(raw: *mut ngx_http_request_t) -> ngx_int_t {
    finish_content(raw, ContentTerminal::Error)
}

/// Auto-last on finalize after output, without overwriting an already-sent status.
pub(crate) fn finish_response_on_finalize(raw: *mut ngx_http_request_t) -> ngx_int_t {
    finish_content(raw, ContentTerminal::Finalize)
}

#[derive(Clone, Copy)]
enum ContentTerminal {
    Ok,
    Error,
    Finalize,
}

fn finish_content(raw: *mut ngx_http_request_t, terminal: ContentTerminal) -> ngx_int_t {
    if raw.is_null() {
        return ngx_error();
    }
    with_output_slot(raw, |slot| {
        let Some(state) = slot.as_mut() else {
            return match terminal {
                ContentTerminal::Ok => send_default_empty_response(raw),
                ContentTerminal::Error | ContentTerminal::Finalize => ngx_ok(),
            };
        };
        let reason = match terminal {
            ContentTerminal::Ok => OutputCleanupReason::NormalFinish,
            ContentTerminal::Error | ContentTerminal::Finalize => OutputCleanupReason::TerminalError,
        };
        cancel_flush_wait(raw, state, reason);
        if state.eof {
            return ngx_ok();
        }
        let header_already = state.header_sent || headers_sent(raw);
        match terminal {
            ContentTerminal::Ok => match close_output_state(raw, state) {
                Ok(()) => ngx_ok(),
                Err(err) if err.to_string().contains("seen eof") => ngx_ok(),
                Err(_) => send_empty_last(raw),
            },
            ContentTerminal::Error | ContentTerminal::Finalize => {
                state.buffering = false;
                state.buffered.clear();
                if header_already {
                    state.eof = true;
                    send_empty_last(raw)
                } else {
                    ngx_ok()
                }
            }
        }
    })
}

fn send_empty_last(raw: *mut ngx_http_request_t) -> ngx_int_t {
    let rc = unsafe { ngx_rustscript_c_output_body(raw, std::ptr::null(), 0, 0, 1) };
    if rc == ngx_error() {
        ngx_error()
    } else {
        ngx_ok()
    }
}

fn send_default_empty_response(raw: *mut ngx_http_request_t) -> ngx_int_t {
    if !headers_sent(raw) {
        let rc = unsafe { ngx_rustscript_c_send_response_headers(raw) };
        if rc != ngx_ok() && rc != ngx_done() {
            return ngx_error();
        }
    }
    send_empty_last(raw)
}

fn with_request<T>(f: impl FnOnce(*mut ngx_http_request_t) -> VmResult<T>) -> VmResult<T> {
    require_content()?;
    let identity = require_identity(current_identity().map_err(scope_err)?)?;
    with_current_request_mut_ops(identity, AllowedOps::RESPONSE_WRITE, |request: &mut Request| {
        f(request.as_mut() as *mut ngx_http_request_t)
    })
    .map_err(scope_err)?
}

fn save_write_handler(raw: *mut ngx_http_request_t, state: &mut OutputState) {
    if state.saved_write_handler.is_none() {
        // SAFETY: `raw` is the live request for this output state.
        state.saved_write_handler = Some(unsafe { (*raw).write_event_handler });
        state.restored = false;
    }
}

fn prepare_flush(wait: bool) -> VmResult<Option<FlushWait>> {
    with_request(|raw| {
        let cell = with_output(raw, |state| {
            if state.eof {
                return Err(host_err("seen eof"));
            }
            if state.buffering && http10_should_buffer(raw) {
                // HTTP/1.0 buffering holds the body until headers; nothing is
                // on the socket yet, so wait is a no-op.
                return Ok(None);
            }
            if header_only(raw) {
                send_headers_inner(raw, state)?;
                return Ok(None);
            }
            send_headers_inner(raw, state)?;
            emit_body(raw, state, &[], 1, 0)?;
            let needs_wait =
                wait && unsafe { ngx_rustscript_c_connection_needs_flush_wait(raw) != 0 };
            if !needs_wait {
                return Ok(None);
            }
            save_write_handler(raw, state);
            state.flush_generation = state.flush_generation.wrapping_add(1);
            let cell = Arc::new(FlushWaitCell::new());
            state.flush_wait = Some(FlushWaitSlot {
                cell: cell.clone(),
                generation: state.flush_generation,
                identity: state.identity,
            });
            Ok(Some(cell))
        })?;
        let Some(cell) = cell else {
            return Ok(None);
        };
        let rc = unsafe { ngx_rustscript_c_arm_flush_wait(raw, flush_write_handler) };
        if rc == ngx_error() {
            with_output_slot(raw, |slot| {
                if let Some(state) = slot.as_mut() {
                    cancel_flush_wait(raw, state, OutputCleanupReason::TerminalError);
                }
            });
            return Err(host_err("flush wait"));
        }
        Ok(Some(FlushWait { cell }))
    })
}

unsafe extern "C" fn flush_write_handler(r: *mut ngx_http_request_t) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if r.is_null() {
            return;
        }
        with_output_slot(r, |slot| {
            let Some(state) = slot.as_mut() else {
                return;
            };
            let Some(wait) = state.flush_wait.as_ref() else {
                return;
            };
            if wait.generation != state.flush_generation || wait.identity != state.identity {
                return;
            }
            let rc = unsafe { ngx_rustscript_c_continue_output(r) };
            if rc == ngx_again() {
                return;
            }
            if rc == ngx_ok() || rc == ngx_done() {
                complete_flush_wait(r, state, Ok(()));
                return;
            }
            if rc == NGX_HTTP_REQUEST_TIME_OUT as ngx_int_t {
                complete_flush_wait(r, state, Err("timeout".to_string()));
                return;
            }
            if rc == NGX_HTTP_CLIENT_CLOSED_REQUEST as ngx_int_t {
                cancel_flush_wait(r, state, OutputCleanupReason::ConfirmedClientAbort);
                return;
            }
            complete_flush_wait(r, state, Err("flush failed".to_string()));
        });
    }));
}

/// Send response headers. Idempotent after the first successful send.
#[pd_host_function(name = "ngx::send_headers")]
pub fn send_headers() -> VmResult<CallOutcome> {
    with_request(|raw| {
        with_output(raw, |state| {
            if state.eof {
                return Err(host_err("seen eof"));
            }
            send_headers_inner(raw, state)
        })
    })?;
    ret(())
}

/// Write response body without a trailing newline.
#[pd_host_function(name = "ngx::print")]
pub fn print(data: &Value) -> VmResult<CallOutcome> {
    let mut payload = Vec::new();
    append_output(&mut payload, data, 0)?;
    with_request(|raw| write_body(raw, &payload, false))?;
    ret(())
}

/// Write response body and a trailing newline.
#[pd_host_function(name = "ngx::say")]
pub fn say(data: &Value) -> VmResult<CallOutcome> {
    let mut payload = Vec::new();
    append_output(&mut payload, data, 0)?;
    with_request(|raw| write_body(raw, &payload, true))?;
    ret(())
}

/// Flush buffered output. `wait=true` yields on NGINX write backpressure.
#[pd_host_function(name = "ngx::flush")]
pub async fn flush(wait: bool) -> VmResult<()> {
    if let Some(pending) = prepare_flush(wait)? {
        pending.await.map_err(host_err)?;
    }
    Ok(())
}

/// Send the last buffer and reject further output.
#[pd_host_function(name = "ngx::eof")]
pub fn eof() -> VmResult<CallOutcome> {
    with_request(send_eof)?;
    ret(())
}

/// Test hook: invoke the request write handler NGINX would run.
pub fn fire_flush_write_handler(raw: *mut ngx_http_request_t) {
    if raw.is_null() {
        return;
    }
    unsafe { flush_write_handler(raw) };
}

#[cfg(test)]
mod output_bound_tests {
    use super::*;
    use vm::Value;

    #[test]
    fn append_output_rejects_deep_nesting() {
        let mut value = Value::Int(1);
        for _ in 0..12 {
            value = Value::Array(vec![value].into());
        }
        let mut out = Vec::new();
        let err = append_output(&mut out, &value, 0).unwrap_err();
        assert!(
            err.to_string().to_ascii_lowercase().contains("nest"),
            "{err}"
        );
    }

    #[test]
    fn append_output_rejects_oversized_arrays() {
        let items = vec![Value::Int(1); MAX_OUTPUT_ARRAY_ELEMS + 1];
        let value = Value::Array(items.into());
        let mut out = Vec::new();
        let err = append_output(&mut out, &value, 0).unwrap_err();
        let text = err.to_string().to_ascii_lowercase();
        assert!(
            text.contains("1024") || text.contains("element"),
            "{err}"
        );
    }
}

#[cfg(test)]
mod output_stub_tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/host/output_stub_tests.rs"
    ));
}
