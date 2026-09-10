// Request-body hosts. Copy body bytes/paths into owned VM values; never
// expose request, pool, chain, or temp-file pointers. `ngx::req::read_body`
// yields through the generic nginx_op adapter. Read-event wrapping for
// client-abort 499 lives here, not in the generic primitive.

use core::ffi::c_uint;
use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use nginx_sys::{
    ngx_chain_t, ngx_http_event_handler_pt, ngx_http_request_body_t, ngx_http_request_t,
    ngx_int_t, ngx_str_t, NGX_AGAIN, NGX_ERROR, NGX_HTTP_BAD_REQUEST,
    NGX_HTTP_CLIENT_CLOSED_REQUEST, NGX_HTTP_SPECIAL_RESPONSE, NGX_OK,
};
use ngx::http::Request;
use pd_host_function::pd_host_function;
use vm::{Value, VmError};

use super::super::VmResult;
use super::{CallOutcome, IntoHostCallOutcome};
use crate::host::{
    current_identity, with_current_disposition_mut, with_current_request, with_current_request_mut,
    RequestDisposition, ScopeError, control_stop_error,
};
use crate::nginx_op::{NginxOpFuture, NginxOpOutcome};

const MAX_COPY_BYTES: usize = 65_536;
const MAX_PATH_BYTES: usize = 4_096;
const MAX_CHAIN_NODES: usize = 256;

fn ret<T: IntoHostCallOutcome>(value: T) -> VmResult<CallOutcome> {
    Ok(value.into_host_call_outcome())
}

fn scope_err(err: ScopeError) -> VmError {
    VmError::HostError(err.to_string())
}

fn with_request<T>(f: impl FnOnce(&Request) -> VmResult<T>) -> VmResult<T> {
    let identity = current_identity().map_err(scope_err)?;
    with_current_request(identity, f).map_err(scope_err)?
}

fn with_request_write<T>(f: impl FnOnce(&mut Request) -> VmResult<T>) -> VmResult<T> {
    let identity = current_identity().map_err(scope_err)?;
    with_current_request_mut(identity, f).map_err(scope_err)?
}

unsafe extern "C" {
    fn ngx_rustscript_c_request_reading_body(r: *const ngx_http_request_t) -> c_uint;
    fn ngx_rustscript_c_request_discard_body(r: *const ngx_http_request_t) -> c_uint;
    fn ngx_rustscript_c_request_main_count_dec(r: *mut ngx_http_request_t);
    fn ngx_rustscript_c_read_client_request_body(
        r: *mut ngx_http_request_t,
        handler: Option<unsafe extern "C" fn(*mut ngx_http_request_t)>,
    ) -> ngx_int_t;
    fn ngx_rustscript_c_discard_request_body(r: *mut ngx_http_request_t) -> ngx_int_t;
    fn ngx_rustscript_c_get_read_event_handler(r: *mut ngx_http_request_t) -> ngx_http_event_handler_pt;
    fn ngx_rustscript_c_arm_body_wait(r: *mut ngx_http_request_t) -> ngx_int_t;
    fn ngx_rustscript_c_finalize_client_closed(r: *mut ngx_http_request_t);
    fn ngx_rustscript_c_request_terminated(r: *const ngx_http_request_t) -> c_uint;
    fn ngx_rustscript_c_connection_error(r: *const ngx_http_request_t) -> c_uint;
    fn ngx_rustscript_c_request_headers_sent(r: *const ngx_http_request_t) -> c_uint;
    fn ngx_rustscript_c_install_memory_body(
        r: *mut ngx_http_request_t,
        data: *const u8,
        len: usize,
    ) -> ngx_int_t;
    fn ngx_rustscript_c_install_file_body(
        r: *mut ngx_http_request_t,
        fd: i32,
        size: i64,
        path: *const u8,
        path_len: usize,
        clean: i32,
    ) -> ngx_int_t;
}

fn request_flag(
    raw: &ngx_http_request_t,
    read: unsafe extern "C" fn(*const ngx_http_request_t) -> c_uint,
) -> bool {
    unsafe { read(raw) != 0 }
}

fn bytes_as_string(bytes: Vec<u8>) -> String {
    match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(err) => err.into_bytes().into_iter().map(char::from).collect(),
    }
}

fn copy_ngx_str(s: ngx_str_t, cap: usize, name: &str) -> VmResult<Vec<u8>> {
    if s.data.is_null() || s.len == 0 {
        return Ok(Vec::new());
    }
    if s.len > cap {
        return Err(VmError::HostError(format!(
            "{name} exceeds the {cap}-byte copy limit"
        )));
    }
    // SAFETY: caller validated data/len against a live request or test fixture.
    Ok(unsafe { std::slice::from_raw_parts(s.data, s.len) }.to_vec())
}

fn body_err(what: &str, message: &str) -> VmError {
    VmError::HostError(format!("{what} {message}"))
}

pub(crate) fn body_was_discarded(raw: &ngx_http_request_t) -> bool {
    request_flag(raw, ngx_rustscript_c_request_discard_body)
}

/// Copy a completed request body (memory chain or bounded temp file).
///
/// Used by `get_post_args` and the body getters. Rejects unread, discarded,
/// still-reading, incomplete, cyclic, inverted, and over-cap bodies.
pub(crate) fn copy_completed_body(raw: &ngx_http_request_t, what: &str) -> VmResult<Vec<u8>> {
    if request_flag(raw, ngx_rustscript_c_request_reading_body) {
        return Err(body_err(what, "request body is still being read"));
    }
    if raw.request_body.is_null() {
        if request_flag(raw, ngx_rustscript_c_request_discard_body) {
            return Err(body_err(what, "request body was discarded"));
        }
        return Err(body_err(what, "request body not read"));
    }
    // SAFETY: request_body is non-null after the unread check.
    let body = unsafe { &*raw.request_body };
    if body.rest > 0 {
        return Err(body_err(what, "request body is incomplete"));
    }
    if let Some(path) = body_temp_path(body, what)? {
        return copy_temp_file_bytes(&path, what);
    }
    copy_body_bufs(body, what)
}

fn copy_body_bufs(body: &ngx_http_request_body_t, what: &str) -> VmResult<Vec<u8>> {
    if body.bufs.is_null() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    let mut seen: Vec<*mut ngx_chain_t> = Vec::new();
    let mut cl = body.bufs;
    while !cl.is_null() {
        if seen.iter().any(|&ptr| std::ptr::eq(ptr, cl)) {
            return Err(body_err(what, "malformed cyclic body buffer chain"));
        }
        if seen.len() >= MAX_CHAIN_NODES {
            return Err(body_err(what, "body buffer chain exceeds the safety cap"));
        }
        seen.push(cl);
        // SAFETY: cl is a non-null, acyclic request-body chain node.
        let chain = unsafe { &*cl };
        if chain.buf.is_null() {
            return Err(body_err(what, "malformed null body buffer"));
        }
        let buf = unsafe { &*chain.buf };
        if buf.pos.is_null() && buf.last.is_null() {
            cl = chain.next;
            continue;
        }
        if buf.pos.is_null() || buf.last.is_null() {
            return Err(body_err(what, "malformed body buffer"));
        }
        let pos = buf.pos as usize;
        let last = buf.last as usize;
        if last < pos {
            return Err(body_err(what, "inverted body buffer range"));
        }
        let len = last - pos;
        if len == 0 {
            cl = chain.next;
            continue;
        }
        if out.len().saturating_add(len) > MAX_COPY_BYTES {
            return Err(body_err(what, "exceeds the 65536-byte copy limit"));
        }
        out.extend_from_slice(unsafe { std::slice::from_raw_parts(buf.pos, len) });
        cl = chain.next;
    }
    Ok(out)
}

fn body_temp_path(body: &ngx_http_request_body_t, what: &str) -> VmResult<Option<String>> {
    if body.temp_file.is_null() {
        return Ok(None);
    }
    // SAFETY: temp_file is a NGINX-owned ngx_temp_file_t for this request.
    let temp = unsafe { &*body.temp_file };
    let bytes = copy_ngx_str(temp.file.name, MAX_PATH_BYTES, what)?;
    if bytes.is_empty() {
        return Ok(None);
    }
    Ok(Some(bytes_as_string(bytes)))
}

fn copy_temp_file_bytes(path: &str, what: &str) -> VmResult<Vec<u8>> {
    if path.contains('\0') {
        return Err(body_err(what, "malformed request body temp file path"));
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|_| body_err(what, "cannot read request body temp file"))?;
    let meta = file
        .metadata()
        .map_err(|_| body_err(what, "cannot stat request body temp file"))?;
    if !meta.file_type().is_file() {
        return Err(body_err(what, "request body temp path is not a regular file"));
    }
    if meta.len() > MAX_COPY_BYTES as u64 {
        return Err(body_err(what, "exceeds the 65536-byte copy limit"));
    }
    let mut file = file;
    let mut out = Vec::new();
    file.read_to_end(&mut out)
        .map_err(|_| body_err(what, "cannot read request body temp file"))?;
    if out.len() > MAX_COPY_BYTES {
        return Err(body_err(what, "exceeds the 65536-byte copy limit"));
    }
    Ok(out)
}

enum ReadBodyStart {
    Done,
    Special(i32),
    Waiting(NginxOpFuture),
}

fn finalize_special(status: i32) -> VmResult<()> {
    let identity = current_identity().map_err(scope_err)?;
    with_current_disposition_mut(identity, |d| {
        *d = RequestDisposition::Finalize(status);
    })
    .map_err(scope_err)?;
    Err(control_stop_error())
}

fn map_wait_outcome(outcome: NginxOpOutcome) -> VmResult<()> {
    match outcome {
        NginxOpOutcome::Done => Ok(()),
        NginxOpOutcome::Cancelled => Err(VmError::HostError(
            "ngx::req::read_body wait cancelled".into(),
        )),
        NginxOpOutcome::Special(status) => finalize_special(status),
    }
}

/// Read the client request body. Yields on NGX_AGAIN; idempotent after completion.
#[pd_host_function(name = "ngx::req::read_body")]
pub async fn read_body() -> VmResult<()> {
    match start_read_body()? {
        ReadBodyStart::Done => Ok(()),
        ReadBodyStart::Special(status) => finalize_special(status),
        ReadBodyStart::Waiting(fut) => map_wait_outcome(fut.await?),
    }
}

fn start_read_body() -> VmResult<ReadBodyStart> {
    let identity = current_identity().map_err(scope_err)?;
    with_request_write(|request| {
        let raw = request.as_mut() as *mut ngx_http_request_t;
        if crate::access::try_with_module_nginx_op(raw, |op| op.waiting()).unwrap_or(false) {
            return Err(VmError::HostError(
                "ngx::req::read_body is already waiting".to_string(),
            ));
        }
        if !crate::access::has_module_phase_ctx(raw) {
            return Err(VmError::HostError(
                "ngx::req::read_body requires request module ctx".to_string(),
            ));
        }
        let rc = unsafe {
            ngx_rustscript_c_read_client_request_body(raw, Some(read_body_post_handler))
        };
        #[cfg(ngx_rustscript_integration_hooks)]
        crate::request_state::proof_body_count(raw, "core_read", Some(rc));
        if rc == NGX_OK as ngx_int_t {
            // ngx_http_read_client_request_body always count++. Undo so the
            // phase driver (content NGX_DONE / access NGX_AGAIN) owns count.
            unsafe { ngx_rustscript_c_request_main_count_dec(raw) };
            #[cfg(ngx_rustscript_integration_hooks)]
            crate::request_state::proof_body_count(raw, "undo", Some(rc));
            return Ok(ReadBodyStart::Done);
        }
        if rc == NGX_AGAIN as ngx_int_t {
            unsafe { ngx_rustscript_c_request_main_count_dec(raw) };
            #[cfg(ngx_rustscript_integration_hooks)]
            crate::request_state::proof_body_count(raw, "undo", Some(rc));
            let orig = unsafe { ngx_rustscript_c_get_read_event_handler(raw) };
            crate::access::try_with_module_body_wait(raw, |wait| {
                wait.orig_read_handler = orig;
                wait.armed = true;
                wait.finalized = false;
            })
            .ok_or_else(|| {
                VmError::HostError("ngx::req::read_body requires request module ctx".to_string())
            })?;
            let fut = crate::access::try_with_module_nginx_op(raw, |op| {
                let _cell = op.begin(identity);
                op.observe()
            })
            .flatten()
            .ok_or_else(|| {
                VmError::HostError("ngx::req::read_body requires request module ctx".to_string())
            })?;
            if unsafe { ngx_rustscript_c_arm_body_wait(raw) } != NGX_OK as ngx_int_t {
                crate::access::try_with_module_nginx_op(raw, |op| op.cancel());
                crate::access::disarm_body_wait(raw);
                return Err(VmError::HostError(
                    "ngx::req::read_body failed to arm client-close wait".to_string(),
                ));
            }
            #[cfg(ngx_rustscript_integration_hooks)]
            crate::request_state::proof_body_count(raw, "wait_armed", Some(rc));
            return Ok(ReadBodyStart::Waiting(fut));
        }
        if rc == NGX_ERROR as ngx_int_t {
            return Err(VmError::HostError("ngx::req::read_body failed".to_string()));
        }
        if rc >= NGX_HTTP_SPECIAL_RESPONSE as ngx_int_t {
            // Core already decremented count on special responses.
            let status = if rc == NGX_HTTP_BAD_REQUEST as ngx_int_t
                && unsafe { ngx_rustscript_c_connection_error(raw) } != 0
            {
                NGX_HTTP_CLIENT_CLOSED_REQUEST as i32
            } else {
                rc as i32
            };
            return Ok(ReadBodyStart::Special(status));
        }
        Err(VmError::HostError(format!(
            "ngx::req::read_body failed with status {rc}"
        )))
    })
}

unsafe extern "C" fn read_body_post_handler(r: *mut ngx_http_request_t) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        crate::access::disarm_body_wait(r);
        #[cfg(ngx_rustscript_integration_hooks)]
        crate::request_state::proof_body_count(r, "callback", None);
        let _ = crate::access::try_with_module_nginx_op(r, |op| {
            op.complete_from_callback(NginxOpOutcome::Done)
        });
    }));
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ngx_rustscript_continue_body_read(r: *mut ngx_http_request_t) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if r.is_null() {
            return;
        }
        let orig = crate::access::try_with_module_body_wait(r, |wait| wait.orig_read_handler)
            .flatten();
        let Some(orig) = orig else {
            return;
        };
        #[cfg(ngx_rustscript_integration_hooks)]
        crate::request_state::proof_body_count(r, "continue_read", None);
        // SAFETY: orig is the NGINX body-read handler saved via the C getter
        // after ngx_http_read_client_request_body returned NGX_AGAIN.
        unsafe { orig(r) };
    }));
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ngx_rustscript_on_body_wait_closed(r: *mut ngx_http_request_t) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if r.is_null() {
            return;
        }
        let already = crate::access::try_with_module_body_wait(r, |wait| {
            if wait.finalized {
                return true;
            }
            wait.finalized = true;
            wait.armed = false;
            false
        })
        .unwrap_or(true);
        if already {
            return;
        }
        #[cfg(ngx_rustscript_integration_hooks)]
        crate::request_state::proof_body_count(r, "closed_499", None);
        let _ = crate::access::try_with_module_nginx_op(r, |op| op.cancel());
        if unsafe { ngx_rustscript_c_request_terminated(r) } != 0 {
            return;
        }
        unsafe { ngx_rustscript_c_finalize_client_closed(r) };
    }));
}

/// Discard the client request body. Sync and idempotent per NGINX core.
#[pd_host_function(name = "ngx::req::discard_body")]
pub fn discard_body() -> VmResult<CallOutcome> {
    with_request_write(|request| {
        let raw = request.as_mut() as *mut ngx_http_request_t;
        let rc = unsafe { ngx_rustscript_c_discard_request_body(raw) };
        if rc == NGX_OK as ngx_int_t || rc == NGX_AGAIN as ngx_int_t {
            return ret(());
        }
        if rc == NGX_ERROR as ngx_int_t {
            return Err(VmError::HostError(
                "ngx::req::discard_body failed".to_string(),
            ));
        }
        Err(VmError::HostError(format!(
            "ngx::req::discard_body failed with status {rc}"
        )))
    })
}

/// In-memory request body as an owned string, or `null` when file-backed/unread.
#[pd_host_function(name = "ngx::req::get_body_data")]
pub fn get_body_data() -> VmResult<CallOutcome> {
    with_request(|request| {
        let raw = request.as_ref();
        if request_flag(raw, ngx_rustscript_c_request_reading_body) {
            return Err(body_err(
                "ngx::req::get_body_data",
                "request body is still being read",
            ));
        }
        if raw.request_body.is_null() {
            return ret(Value::Null);
        }
        let body = unsafe { &*raw.request_body };
        if !body.temp_file.is_null() {
            return ret(Value::Null);
        }
        if body.bufs.is_null() {
            return ret(Value::Null);
        }
        let bytes = copy_body_bufs(body, "ngx::req::get_body_data")?;
        ret(bytes_as_string(bytes))
    })
}

/// NGINX temp-file path as an owned string, or `null` when memory-backed.
#[pd_host_function(name = "ngx::req::get_body_file")]
pub fn get_body_file() -> VmResult<CallOutcome> {
    with_request(|request| {
        let raw = request.as_ref();
        if raw.request_body.is_null() {
            return ret(Value::Null);
        }
        let body = unsafe { &*raw.request_body };
        match body_temp_path(body, "ngx::req::get_body_file")? {
            Some(path) => ret(path),
            None => ret(Value::Null),
        }
    })
}

fn host_err(message: impl Into<String>) -> VmError {
    VmError::HostError(message.into())
}

fn ngx_ok() -> ngx_int_t {
    NGX_OK as ngx_int_t
}

struct BodyBuilder {
    cap: usize,
    data: Vec<u8>,
}

/// Request-owned body builder. File-body fd/path lifetime is owned by the
/// NGINX pool cleanup registered in `ngx_rustscript_c_install_file_body`.
pub(crate) struct BodyMutationState {
    builder: Option<BodyBuilder>,
}

impl BodyMutationState {
    pub(crate) fn new() -> Self {
        Self { builder: None }
    }

    pub(crate) fn cleanup(&mut self) {
        self.builder = None;
    }
}

impl Default for BodyMutationState {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for BodyMutationState {
    fn drop(&mut self) {
        self.cleanup();
    }
}

#[cfg(any(test, debug_assertions, ngx_features = "rustc-test"))]
thread_local! {
    static FALLBACK_BODY_MUT: std::cell::RefCell<std::collections::HashMap<usize, BodyMutationState>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

#[cfg(any(test, debug_assertions, ngx_features = "rustc-test"))]
fn fallback_with_body_slot<R>(
    raw: *mut ngx_http_request_t,
    f: impl FnOnce(&mut BodyMutationState) -> R,
) -> R {
    FALLBACK_BODY_MUT.with(|map| {
        let mut map = map.borrow_mut();
        let key = raw as usize;
        let mut state = map.remove(&key).unwrap_or_default();
        let result = f(&mut state);
        map.insert(key, state);
        result
    })
}

fn with_body_slot_opt<R>(
    raw: *mut ngx_http_request_t,
    f: impl FnOnce(&mut BodyMutationState) -> R,
) -> Option<R> {
    if crate::access::has_module_phase_ctx(raw) {
        return crate::access::try_with_module_body_mut(raw, f);
    }
    #[cfg(any(test, debug_assertions, ngx_features = "rustc-test"))]
    {
        Some(fallback_with_body_slot(raw, f))
    }
    #[cfg(not(any(test, debug_assertions, ngx_features = "rustc-test")))]
    {
        let _ = f;
        None
    }
}

fn with_body_slot<R>(
    raw: *mut ngx_http_request_t,
    f: impl FnOnce(&mut BodyMutationState) -> R,
) -> VmResult<R> {
    with_body_slot_opt(raw, f)
        .ok_or_else(|| host_err("ngx body mutation requires request module ctx"))
}

fn mutation_blocked(raw: *mut ngx_http_request_t) -> VmResult<()> {
    if raw.is_null() {
        return Err(host_err("request is null"));
    }
    let r = unsafe { &*raw };
    if r.pool.is_null() {
        return Err(host_err("request pool is null"));
    }
    if request_flag(r, ngx_rustscript_c_request_headers_sent) {
        return Err(host_err("cannot mutate request body after headers are sent"));
    }
    if request_flag(r, ngx_rustscript_c_request_reading_body) {
        return Err(host_err("cannot mutate request body while a read is pending"));
    }
    if request_flag(r, ngx_rustscript_c_request_discard_body) {
        return Err(host_err("request body already discarded"));
    }
    if !r.main.is_null() && r.main != raw {
        return Err(host_err("cannot mutate request body on a subrequest"));
    }
    let waiting = crate::access::try_with_module_nginx_op(raw, |op| op.waiting()).unwrap_or(false);
    let armed = crate::access::try_with_module_body_wait(raw, |wait| wait.armed).unwrap_or(false);
    if waiting || armed {
        return Err(host_err("cannot mutate request body while a read is pending"));
    }
    Ok(())
}

fn install_memory_body(raw: *mut ngx_http_request_t, data: &[u8]) -> VmResult<()> {
    if data.len() > MAX_COPY_BYTES {
        return Err(host_err(format!(
            "request body exceeds the {MAX_COPY_BYTES}-byte copy limit"
        )));
    }
    let rc = unsafe { ngx_rustscript_c_install_memory_body(raw, data.as_ptr(), data.len()) };
    if rc != ngx_ok() {
        return Err(host_err("out of memory"));
    }
    let _ = with_body_slot_opt(raw, |state| {
        state.builder = None;
    });
    Ok(())
}

fn replace_memory_body(raw: *mut ngx_http_request_t, data: &[u8]) -> VmResult<()> {
    mutation_blocked(raw)?;
    install_memory_body(raw, data)
}

fn parse_optional_bool(value: &Value, default: bool) -> VmResult<bool> {
    match value {
        Value::Null => Ok(default),
        Value::Bool(flag) => Ok(*flag),
        _ => Err(host_err("auto_clean must be a boolean or null")),
    }
}

fn parse_optional_capacity(value: &Value) -> VmResult<usize> {
    match value {
        Value::Null => Ok(MAX_COPY_BYTES),
        Value::Int(n) if *n >= 0 && (*n as u64) <= MAX_COPY_BYTES as u64 => Ok(*n as usize),
        Value::Int(_) => Err(host_err(format!(
            "buffer_size exceeds the {MAX_COPY_BYTES}-byte body builder cap"
        ))),
        _ => Err(host_err("buffer_size must be an integer or null")),
    }
}

static TEMP_SEQ: AtomicU64 = AtomicU64::new(1);

fn copy_to_private_temp(src: &std::fs::File) -> VmResult<(std::fs::File, PathBuf)> {
    let mut path = std::env::temp_dir();
    path.push(format!(
        "ngx-rss-body-{}-{}",
        std::process::id(),
        TEMP_SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let mut dst = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .map_err(|err| host_err(format!("failed to create private body temp file: {err}")))?;
    let mut cloned = src
        .try_clone()
        .map_err(|err| host_err(format!("failed to clone source body fd: {err}")))?;
    if cloned.seek(SeekFrom::Start(0)).is_err() {
        let _ = std::fs::remove_file(&path);
        return Err(host_err("failed to rewind source body file"));
    }
    if std::io::copy(&mut cloned, &mut dst).is_err() {
        let _ = std::fs::remove_file(&path);
        return Err(host_err("failed to copy request body into private temp file"));
    }
    if dst.seek(SeekFrom::Start(0)).is_err() {
        let _ = std::fs::remove_file(&path);
        return Err(host_err("failed to rewind private body temp file"));
    }
    Ok((dst, path))
}

/// Replace the in-memory request body transactionally.
#[pd_host_function(name = "ngx::req::set_body_data")]
pub fn set_body_data(data: String) -> VmResult<CallOutcome> {
    with_request_write(|request| {
        let raw = request.as_mut() as *mut ngx_http_request_t;
        replace_memory_body(raw, data.as_bytes())?;
        ret(())
    })
}

/// Install a file-backed request body. `auto_clean=true` copies into a
/// module-owned private temp file and never unlinks the caller source.
#[pd_host_function(name = "ngx::req::set_body_file")]
pub fn set_body_file(path: String, auto_clean: &Value) -> VmResult<CallOutcome> {
    let auto_clean = parse_optional_bool(auto_clean, false)?;
    if path.is_empty() || path.len() > MAX_PATH_BYTES || path.contains('\0') {
        return Err(host_err("invalid request body file path"));
    }
    with_request_write(|request| {
        let raw = request.as_mut() as *mut ngx_http_request_t;
        mutation_blocked(raw)?;
        let lstat = std::fs::symlink_metadata(&path)
            .map_err(|err| host_err(format!("failed to stat request body file: {err}")))?;
        if lstat.file_type().is_symlink() {
            return Err(host_err("request body path is a symlink"));
        }
        if !lstat.file_type().is_file() {
            return Err(host_err("request body path is not a regular file"));
        }
        let src = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
            .open(&path)
            .map_err(|err| host_err(format!("failed to open request body file: {err}")))?;
        let fstat = src
            .metadata()
            .map_err(|err| host_err(format!("failed to stat request body file: {err}")))?;
        if !fstat.file_type().is_file() {
            return Err(host_err("request body path is not a regular file"));
        }
        if lstat.dev() != fstat.dev() || lstat.ino() != fstat.ino() {
            return Err(host_err("request body path changed between stat and open"));
        }
        let size = fstat.len();
        if size > MAX_COPY_BYTES as u64 {
            return Err(host_err(format!(
                "request body file exceeds the {MAX_COPY_BYTES}-byte size cap"
            )));
        }
        let (owned_file, owned_path, unlink) = if auto_clean {
            let (copy, copy_path) = copy_to_private_temp(&src)?;
            drop(src);
            (copy, copy_path, true)
        } else {
            (src, PathBuf::from(&path), false)
        };
        let fd = owned_file.as_raw_fd();
        let path_bytes = owned_path.as_os_str().as_encoded_bytes();
        let rc = unsafe {
            ngx_rustscript_c_install_file_body(
                raw,
                fd,
                size as i64,
                path_bytes.as_ptr(),
                path_bytes.len(),
                i32::from(unlink),
            )
        };
        if rc != ngx_ok() {
            if unlink {
                let _ = std::fs::remove_file(&owned_path);
            }
            return Err(host_err("out of memory"));
        }
        // Pool cleanup owns fd close and, when auto_clean, unlink of the copy.
        std::mem::forget(owned_file);
        let _ = with_body_slot_opt(raw, |state| {
            state.builder = None;
        });
        ret(())
    })
}

/// Create or reset the request-owned memory body builder.
#[pd_host_function(name = "ngx::req::init_body")]
pub fn init_body(buffer_size: &Value) -> VmResult<CallOutcome> {
    let cap = parse_optional_capacity(buffer_size)?;
    with_request_write(|request| {
        let raw = request.as_mut() as *mut ngx_http_request_t;
        mutation_blocked(raw)?;
        with_body_slot(raw, |state| {
            state.builder = Some(BodyBuilder {
                cap,
                data: Vec::new(),
            });
        })?;
        ret(())
    })
}

/// Append bytes to the request-owned builder.
#[pd_host_function(name = "ngx::req::append_body")]
pub fn append_body(data: String) -> VmResult<CallOutcome> {
    with_request_write(|request| {
        let raw = request.as_mut() as *mut ngx_http_request_t;
        mutation_blocked(raw)?;
        with_body_slot(raw, |state| {
            let builder = state
                .builder
                .as_mut()
                .ok_or_else(|| host_err("ngx::req::append_body requires ngx::req::init_body"))?;
            let add = data.len();
            if builder.data.len().saturating_add(add) > builder.cap {
                return Err(host_err("append_body exceeds the body builder cap"));
            }
            builder.data.extend_from_slice(data.as_bytes());
            Ok(())
        })??;
        ret(())
    })
}

/// Atomically install the builder contents as one memory body.
#[pd_host_function(name = "ngx::req::finish_body")]
pub fn finish_body() -> VmResult<CallOutcome> {
    with_request_write(|request| {
        let raw = request.as_mut() as *mut ngx_http_request_t;
        mutation_blocked(raw)?;
        let data = with_body_slot(raw, |state| {
            state
                .builder
                .take()
                .ok_or_else(|| host_err("ngx::req::finish_body requires ngx::req::init_body"))
        })??;
        let cap = data.cap;
        match install_memory_body(raw, &data.data) {
            Ok(()) => ret(()),
            Err(err) => {
                with_body_slot(raw, |state| {
                    state.builder = Some(BodyBuilder {
                        cap,
                        data: data.data,
                    });
                })?;
                Err(err)
            }
        }
    })
}
