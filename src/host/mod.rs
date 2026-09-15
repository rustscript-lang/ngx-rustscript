//! Production host bindings via `pd-host-function`.
//!
//! The pinned macro only accepts `name = "..."` and emits wrappers that resolve
//! `super::{borrow_arg, CallOutcome, ...}` and `super::super::{Vm, Value, VmResult}`.
//! Nested modules below match that layout. There is no handwritten
//! `HostFunction`, argument decode, or yield-based host re-entry.

use vm::{Vm, VmError};

pub mod catalog;
pub(crate) mod context;
pub(crate) mod standard;
pub(crate) mod timer;

pub use catalog::{
    HostCapability, HostFamily, HostManifestEntry, HostSyncKind, LOG_ALLOWED_NAMES,
    NGX_COOKIE_TIME_NAME, NGX_CRC32_LONG_NAME, NGX_CRC32_SHORT_NAME, NGX_CTX_CLEAR_NAME,
    NGX_CTX_DELETE_NAME, NGX_CTX_GET_NAME, NGX_CTX_SET_NAME, NGX_DECODE_ARGS_NAME,
    NGX_DECODE_BASE64_NAME, NGX_DECODE_BASE64MIME_NAME, NGX_ENCODE_ARGS_NAME,
    NGX_ENCODE_BASE64_NAME, NGX_EOF_NAME, NGX_ESCAPE_URI_NAME, NGX_EXIT_NAME, NGX_FLUSH_NAME,
    NGX_HEADER_CLEAR_NAME, NGX_HEADER_GET_NAME, NGX_HEADER_SET_ALL_NAME, NGX_HEADER_SET_NAME,
    NGX_HEADERS_SENT_NAME, NGX_HMAC_SHA1_NAME, NGX_HTTP_TIME_NAME, NGX_IS_SUBREQUEST_NAME,
    NGX_LOCALTIME_NAME, NGX_MD5_BIN_NAME, NGX_MD5_NAME, NGX_NOW_NAME, NGX_PARSE_HTTP_TIME_NAME,
    NGX_PRINT_NAME, NGX_QUOTE_SQL_STR_NAME, NGX_REDIRECT_NAME, NGX_REQ_APPEND_BODY_NAME,
    NGX_REQ_CLEAR_HEADER_NAME, NGX_REQ_DISCARD_BODY_NAME, NGX_REQ_FINISH_BODY_NAME,
    NGX_REQ_GET_BODY_DATA_NAME, NGX_REQ_GET_BODY_FILE_NAME, NGX_REQ_GET_HEADERS_NAME,
    NGX_REQ_GET_METHOD_NAME, NGX_REQ_GET_POST_ARGS_NAME, NGX_REQ_GET_URI_ARGS_NAME,
    NGX_REQ_HTTP_VERSION_NAME, NGX_REQ_INIT_BODY_NAME, NGX_REQ_IS_INTERNAL_NAME,
    NGX_REQ_RAW_HEADER_NAME, NGX_REQ_READ_BODY_NAME, NGX_REQ_SET_BODY_DATA_NAME,
    NGX_REQ_SET_BODY_FILE_NAME, NGX_REQ_SET_HEADER_NAME, NGX_REQ_SET_METHOD_NAME,
    NGX_REQ_SET_URI_ARGS_NAME, NGX_REQ_SET_URI_NAME, NGX_REQ_START_TIME_NAME, NGX_SAY_NAME,
    NGX_SEMAPHORE_COUNT_NAME, NGX_SEMAPHORE_NEW_NAME, NGX_SEMAPHORE_POST_NAME,
    NGX_SEMAPHORE_RESOURCE_KEY, NGX_SEMAPHORE_WAIT_NAME, NGX_SEND_HEADERS_NAME, NGX_SHA1_BIN_NAME,
    NGX_SHARED_ADD_NAME, NGX_SHARED_CAPACITY_NAME, NGX_SHARED_DELETE_NAME, NGX_SHARED_DICT_NAME,
    NGX_SHARED_EXPIRE_NAME, NGX_SHARED_FLUSH_ALL_NAME, NGX_SHARED_FLUSH_EXPIRED_NAME,
    NGX_SHARED_FREE_SPACE_NAME, NGX_SHARED_GET_KEYS_NAME, NGX_SHARED_GET_NAME,
    NGX_SHARED_GET_STALE_NAME, NGX_SHARED_INCR_NAME, NGX_SHARED_LLEN_NAME, NGX_SHARED_LPOP_NAME,
    NGX_SHARED_LPUSH_NAME, NGX_SHARED_REPLACE_NAME, NGX_SHARED_RPOP_NAME, NGX_SHARED_RPUSH_NAME,
    NGX_SHARED_SAFE_ADD_NAME, NGX_SHARED_SAFE_SET_NAME, NGX_SHARED_SET_NAME, NGX_SHARED_TTL_NAME,
    NGX_SLEEP_MS_NAME, NGX_SLEEP_NAME, NGX_SOCKET_CONNECT_NAME, NGX_SOCKET_TCP_CLIENTCERT_NAME,
    NGX_SOCKET_TCP_CLOSE_NAME, NGX_SOCKET_TCP_CONNECT_NAME, NGX_SOCKET_TCP_GETREUSEDTIMES_NAME,
    NGX_SOCKET_TCP_NAME, NGX_SOCKET_TCP_RECEIVE_NAME, NGX_SOCKET_TCP_RECEIVEANY_NAME,
    NGX_SOCKET_TCP_RECEIVEUNTIL_NAME, NGX_SOCKET_TCP_RESOURCE_KEY, NGX_SOCKET_TCP_SEND_NAME,
    NGX_SOCKET_TCP_SETCLIENTCERT_NAME, NGX_SOCKET_TCP_SETKEEPALIVE_NAME,
    NGX_SOCKET_TCP_SETTIMEOUT_NAME, NGX_SOCKET_TCP_SETTRUSTEDSTORE_NAME,
    NGX_SOCKET_TCP_SSLHANDSHAKE_NAME, NGX_SOCKET_TCP_TRUSTEDSTORE_NAME,
    NGX_SOCKET_TLS_CLIENTCERT_RESOURCE_KEY, NGX_SOCKET_TLS_TRUSTEDSTORE_RESOURCE_KEY,
    NGX_SOCKET_UDP_BIND_NAME, NGX_SOCKET_UDP_CLOSE_NAME, NGX_SOCKET_UDP_NAME,
    NGX_SOCKET_UDP_RECEIVE_NAME, NGX_SOCKET_UDP_RESOURCE_KEY, NGX_SOCKET_UDP_SEND_NAME,
    NGX_SOCKET_UDP_SETPEERNAME_NAME, NGX_SOCKET_UDP_SETTIMEOUT_NAME, NGX_STATUS_GET_NAME,
    NGX_STATUS_SET_NAME, NGX_TIME_NAME, NGX_TODAY_NAME, NGX_UNESCAPE_URI_NAME,
    NGX_UPDATE_TIME_NAME, NGX_UTCTIME_NAME, NGX_VAR_GET_NAME, NGX_VAR_SET_NAME, NgxPhase,
    TIMER_CALLBACK_DENIED_CAPABILITIES, TIMER_CALLBACK_PHASE, host_allowed_in_phase,
    host_allowed_in_timer_callback, host_validates_for_log, log_allowlist_accepts, manifest_entry,
    ngx_host_catalog, ngx_host_manifest, timer_callback_schema,
};
pub use catalog::{compile_host_catalog, is_standard_host_import};

pub use catalog::{
    NGX_TIMER_AT_NAME, NGX_TIMER_EVERY_NAME, NGX_TIMER_PENDING_COUNT_NAME,
    NGX_TIMER_RUNNING_COUNT_NAME,
};

pub use context::{
    AllowedOps, RequestDisposition, ScopeError, VmEntryObservation, capture_vm_entries,
    current_allowed_ops, current_identity, current_phase, current_request_live, is_active,
    with_current_ctx, with_current_ctx_mut, with_current_disposition_mut, with_current_request,
    with_current_request_and_disposition_mut, with_current_request_mut,
    with_current_request_mut_ops, with_current_request_ops, with_request_scope,
    with_request_scope_ctx, with_request_scope_full, with_vm_request_scope,
    with_vm_request_scope_ctx, with_vm_request_scope_full,
};
#[cfg(test)]
pub(crate) use layout::helpers::request_ctx::live_owned_entry_count;
pub use layout::helpers::request_ctx::{
    MAX_CTX_CONTAINER_ELEMS, MAX_CTX_DEPTH, MAX_CTX_KEY_BYTES, MAX_CTX_KEYS, MAX_CTX_SCALAR_BYTES,
    MAX_CTX_TOTAL_BYTES, RequestLocalCtx,
};
pub use standard::{StandardHostPolicy, install_standard_runtime};

/// Override the OS clock sampled by time hosts. Used by tests and by
/// `notify_cached_clock_event_boundary` to model nginx's event-loop refresh
/// without sleeping.
pub fn override_os_clock(sec: i64, msec: u32) {
    layout::helpers::time::override_os_clock(sec, msec);
}

/// Clear a previous [`override_os_clock`] so the next snapshot reads the OS.
pub fn clear_os_clock_override() {
    layout::helpers::time::clear_os_clock_override();
}

/// Mark an event/request-like boundary so the cached clock is eligible to
/// refresh, matching nginx `ngx_time_update` on the event loop.
pub fn notify_cached_clock_event_boundary() {
    layout::helpers::time::notify_cached_clock_event_boundary();
}

pub(crate) use layout::helpers::body::BodyMutationState;
pub(crate) use layout::helpers::output::OutputState;

pub fn finish_response(raw: *mut nginx_sys::ngx_http_request_t) -> nginx_sys::ngx_int_t {
    layout::helpers::output::finish_response(raw)
}

pub fn finish_response_on_error(raw: *mut nginx_sys::ngx_http_request_t) -> nginx_sys::ngx_int_t {
    layout::helpers::output::finish_response_on_error(raw)
}

pub fn finish_response_on_finalize(
    raw: *mut nginx_sys::ngx_http_request_t,
) -> nginx_sys::ngx_int_t {
    layout::helpers::output::finish_response_on_finalize(raw)
}

pub(crate) fn cleanup_output_slot(
    raw: *mut nginx_sys::ngx_http_request_t,
    slot: &mut Option<OutputState>,
) {
    layout::helpers::output::cleanup_output_slot(raw, slot)
}
pub fn cleanup_output(raw: *mut nginx_sys::ngx_http_request_t) {
    layout::helpers::output::cleanup_output(raw)
}

/// Test hook: run the request-level flush write callback.
pub fn fire_flush_write_handler(raw: *mut nginx_sys::ngx_http_request_t) {
    layout::helpers::output::fire_flush_write_handler(raw)
}

/// VM-stop sentinel used by `ngx::exit` / `ngx::redirect`.
///
/// The pinned VM has no control-flow error variant, so AccessExec uses this
/// private HostError text plus a request-owned non-Continue disposition.
/// AccessExec consumes the sentinel only when `disposition != Continue`.
/// An arbitrary HostError with identical or user-shaped text is treated as a
/// normal host failure (500) unless that disposition is already set.
/// Ordinary HostError cannot forge a disposition.
pub(crate) const CONTROL_STOP_MESSAGE: &str = "\u{1}ngx::control-stop";

pub(crate) fn control_stop_error() -> VmError {
    VmError::HostError(CONTROL_STOP_MESSAGE.to_string())
}

pub(crate) fn is_control_stop(err: &VmError) -> bool {
    matches!(err, VmError::HostError(message) if message == CONTROL_STOP_MESSAGE)
}

/// Names bound by [`bind_ngx_hosts`]. Kept next to the explicit bind list so
/// catalog correspondence can be tested without generating dispatch from
/// catalog metadata.
pub fn production_runtime_host_names() -> &'static [&'static str] {
    &[
        NGX_SLEEP_NAME,
        NGX_SLEEP_MS_NAME,
        NGX_SEMAPHORE_NEW_NAME,
        NGX_SEMAPHORE_WAIT_NAME,
        NGX_SEMAPHORE_POST_NAME,
        NGX_SEMAPHORE_COUNT_NAME,
        NGX_ESCAPE_URI_NAME,
        NGX_UNESCAPE_URI_NAME,
        NGX_ENCODE_ARGS_NAME,
        NGX_DECODE_ARGS_NAME,
        NGX_ENCODE_BASE64_NAME,
        NGX_DECODE_BASE64_NAME,
        NGX_DECODE_BASE64MIME_NAME,
        NGX_CRC32_SHORT_NAME,
        NGX_CRC32_LONG_NAME,
        NGX_HMAC_SHA1_NAME,
        NGX_MD5_NAME,
        NGX_MD5_BIN_NAME,
        NGX_SHA1_BIN_NAME,
        NGX_QUOTE_SQL_STR_NAME,
        NGX_TODAY_NAME,
        NGX_TIME_NAME,
        NGX_NOW_NAME,
        NGX_UPDATE_TIME_NAME,
        NGX_LOCALTIME_NAME,
        NGX_UTCTIME_NAME,
        NGX_COOKIE_TIME_NAME,
        NGX_HTTP_TIME_NAME,
        NGX_PARSE_HTTP_TIME_NAME,
        "ngx::get_phase",
        "ngx::config::subsystem",
        "ngx::config::debug",
        "ngx::config::prefix",
        "ngx::config::nginx_version",
        "ngx::config::nginx_configure",
        "ngx::config::ngx_rustscript_version",
        "ngx::worker::exiting",
        "ngx::worker::pid",
        "ngx::worker::pids",
        "ngx::worker::count",
        "ngx::worker::id",
        "ngx::log",
        "ngx::core::ok",
        "ngx::core::error",
        "ngx::core::again",
        "ngx::core::done",
        "ngx::core::declined",
        "ngx::core::null_value",
        "ngx::http::get",
        "ngx::http::head",
        "ngx::http::put",
        "ngx::http::post",
        "ngx::http::delete",
        "ngx::http::options",
        "ngx::http::mkcol",
        "ngx::http::copy",
        "ngx::http::move",
        "ngx::http::propfind",
        "ngx::http::proppatch",
        "ngx::http::lock",
        "ngx::http::unlock",
        "ngx::http::patch",
        "ngx::http::trace",
        "ngx::http::status_continue",
        "ngx::http::status_switching_protocols",
        "ngx::http::status_ok",
        "ngx::http::status_created",
        "ngx::http::status_accepted",
        "ngx::http::status_no_content",
        "ngx::http::status_partial_content",
        "ngx::http::status_special_response",
        "ngx::http::status_moved_permanently",
        "ngx::http::status_moved_temporarily",
        "ngx::http::status_see_other",
        "ngx::http::status_not_modified",
        "ngx::http::status_temporary_redirect",
        "ngx::http::status_permanent_redirect",
        "ngx::http::status_bad_request",
        "ngx::http::status_unauthorized",
        "ngx::http::status_payment_required",
        "ngx::http::status_forbidden",
        "ngx::http::status_not_found",
        "ngx::http::status_not_allowed",
        "ngx::http::status_not_acceptable",
        "ngx::http::status_request_timeout",
        "ngx::http::status_conflict",
        "ngx::http::status_gone",
        "ngx::http::status_upgrade_required",
        "ngx::http::status_too_many_requests",
        "ngx::http::status_illegal",
        "ngx::http::status_close",
        "ngx::http::status_internal_server_error",
        "ngx::http::status_not_implemented",
        "ngx::http::status_bad_gateway",
        "ngx::http::status_service_unavailable",
        "ngx::http::status_gateway_timeout",
        "ngx::http::status_version_not_supported",
        "ngx::http::status_insufficient_storage",
        "ngx::log_level::stderr",
        "ngx::log_level::emerg",
        "ngx::log_level::alert",
        "ngx::log_level::crit",
        "ngx::log_level::error",
        "ngx::log_level::warn",
        "ngx::log_level::notice",
        "ngx::log_level::info",
        "ngx::log_level::debug",
        NGX_REQ_GET_METHOD_NAME,
        NGX_REQ_HTTP_VERSION_NAME,
        NGX_REQ_RAW_HEADER_NAME,
        NGX_REQ_GET_HEADERS_NAME,
        NGX_REQ_GET_URI_ARGS_NAME,
        NGX_REQ_GET_POST_ARGS_NAME,
        NGX_REQ_READ_BODY_NAME,
        NGX_REQ_DISCARD_BODY_NAME,
        NGX_REQ_GET_BODY_DATA_NAME,
        NGX_REQ_GET_BODY_FILE_NAME,
        NGX_REQ_SET_BODY_DATA_NAME,
        NGX_REQ_SET_BODY_FILE_NAME,
        NGX_REQ_INIT_BODY_NAME,
        NGX_REQ_APPEND_BODY_NAME,
        NGX_REQ_FINISH_BODY_NAME,
        NGX_REQ_IS_INTERNAL_NAME,
        NGX_REQ_START_TIME_NAME,
        NGX_IS_SUBREQUEST_NAME,
        NGX_HEADERS_SENT_NAME,
        NGX_VAR_GET_NAME,
        NGX_REQ_SET_METHOD_NAME,
        NGX_REQ_SET_URI_NAME,
        NGX_REQ_SET_URI_ARGS_NAME,
        NGX_REQ_SET_HEADER_NAME,
        NGX_REQ_CLEAR_HEADER_NAME,
        NGX_VAR_SET_NAME,
        NGX_STATUS_GET_NAME,
        NGX_STATUS_SET_NAME,
        NGX_HEADER_GET_NAME,
        NGX_HEADER_SET_NAME,
        NGX_HEADER_SET_ALL_NAME,
        NGX_HEADER_CLEAR_NAME,
        NGX_EXIT_NAME,
        NGX_REDIRECT_NAME,
        NGX_CTX_GET_NAME,
        NGX_CTX_SET_NAME,
        NGX_CTX_DELETE_NAME,
        NGX_CTX_CLEAR_NAME,
        NGX_SHARED_DICT_NAME,
        NGX_SHARED_GET_NAME,
        NGX_SHARED_GET_STALE_NAME,
        NGX_SHARED_SET_NAME,
        NGX_SHARED_SAFE_SET_NAME,
        NGX_SHARED_ADD_NAME,
        NGX_SHARED_SAFE_ADD_NAME,
        NGX_SHARED_REPLACE_NAME,
        NGX_SHARED_DELETE_NAME,
        NGX_SHARED_INCR_NAME,
        NGX_SHARED_TTL_NAME,
        NGX_SHARED_EXPIRE_NAME,
        NGX_SHARED_CAPACITY_NAME,
        NGX_SHARED_FREE_SPACE_NAME,
        NGX_SHARED_LPUSH_NAME,
        NGX_SHARED_RPUSH_NAME,
        NGX_SHARED_LPOP_NAME,
        NGX_SHARED_RPOP_NAME,
        NGX_SHARED_LLEN_NAME,
        NGX_SHARED_FLUSH_ALL_NAME,
        NGX_SHARED_FLUSH_EXPIRED_NAME,
        NGX_SHARED_GET_KEYS_NAME,
        NGX_SOCKET_UDP_NAME,
        NGX_SOCKET_UDP_BIND_NAME,
        NGX_SOCKET_UDP_SETPEERNAME_NAME,
        NGX_SOCKET_UDP_SEND_NAME,
        NGX_SOCKET_UDP_RECEIVE_NAME,
        NGX_SOCKET_UDP_CLOSE_NAME,
        NGX_SOCKET_UDP_SETTIMEOUT_NAME,
        NGX_SOCKET_TCP_NAME,
        NGX_SOCKET_TCP_CONNECT_NAME,
        NGX_SOCKET_CONNECT_NAME,
        NGX_SOCKET_TCP_SEND_NAME,
        NGX_SOCKET_TCP_RECEIVE_NAME,
        NGX_SOCKET_TCP_RECEIVEANY_NAME,
        NGX_SOCKET_TCP_RECEIVEUNTIL_NAME,
        NGX_SOCKET_TCP_CLOSE_NAME,
        NGX_SOCKET_TCP_SETTIMEOUT_NAME,
        NGX_SOCKET_TCP_SETKEEPALIVE_NAME,
        NGX_SOCKET_TCP_GETREUSEDTIMES_NAME,
        NGX_SOCKET_TCP_SSLHANDSHAKE_NAME,
        NGX_SOCKET_TCP_SETTRUSTEDSTORE_NAME,
        NGX_SOCKET_TCP_SETCLIENTCERT_NAME,
        NGX_SOCKET_TCP_TRUSTEDSTORE_NAME,
        NGX_SOCKET_TCP_CLIENTCERT_NAME,
        NGX_SEND_HEADERS_NAME,
        NGX_PRINT_NAME,
        NGX_SAY_NAME,
        NGX_FLUSH_NAME,
        NGX_EOF_NAME,
    ]
}

/// One production host wrapper shape.
#[derive(Clone, Copy)]
pub enum NgxHostWrapper {
    /// `bind_static_function` shape: receives the VM and the call arguments.
    Static(vm::StaticHostFunction),
    /// `bind_static_args_function` shape: receives only the call arguments.
    Args(vm::StaticHostArgsFunction),
}

/// Every production host binding, in bind order.
///
/// Single source of truth for [`bind_ngx_hosts`] and for the registry that
/// binds the VMs importing the `ngx::timer` family ([`ngx_host_registry`]).
pub fn ngx_host_bindings() -> &'static [(&'static str, NgxHostWrapper)] {
    &[
        (
            NGX_SLEEP_NAME,
            NgxHostWrapper::Static(layout::helpers::functions::sleep),
        ),
        (
            NGX_SLEEP_MS_NAME,
            NgxHostWrapper::Static(layout::helpers::functions::sleep_ms),
        ),
        (
            NGX_SEMAPHORE_NEW_NAME,
            NgxHostWrapper::Static(layout::helpers::semaphore::new),
        ),
        (
            NGX_SEMAPHORE_WAIT_NAME,
            NgxHostWrapper::Static(layout::helpers::semaphore::wait),
        ),
        (
            NGX_SEMAPHORE_POST_NAME,
            NgxHostWrapper::Static(layout::helpers::semaphore::post),
        ),
        (
            NGX_SEMAPHORE_COUNT_NAME,
            NgxHostWrapper::Static(layout::helpers::semaphore::count),
        ),
        (
            NGX_ESCAPE_URI_NAME,
            NgxHostWrapper::Args(layout::helpers::codec::escape_uri),
        ),
        (
            NGX_UNESCAPE_URI_NAME,
            NgxHostWrapper::Args(layout::helpers::codec::unescape_uri),
        ),
        (
            NGX_ENCODE_ARGS_NAME,
            NgxHostWrapper::Args(layout::helpers::codec::encode_args),
        ),
        (
            NGX_DECODE_ARGS_NAME,
            NgxHostWrapper::Args(layout::helpers::codec::decode_args),
        ),
        (
            NGX_ENCODE_BASE64_NAME,
            NgxHostWrapper::Args(layout::helpers::codec::encode_base64),
        ),
        (
            NGX_DECODE_BASE64_NAME,
            NgxHostWrapper::Args(layout::helpers::codec::decode_base64),
        ),
        (
            NGX_DECODE_BASE64MIME_NAME,
            NgxHostWrapper::Args(layout::helpers::codec::decode_base64mime),
        ),
        (
            NGX_CRC32_SHORT_NAME,
            NgxHostWrapper::Args(layout::helpers::codec::crc32_short),
        ),
        (
            NGX_CRC32_LONG_NAME,
            NgxHostWrapper::Args(layout::helpers::codec::crc32_long),
        ),
        (
            NGX_HMAC_SHA1_NAME,
            NgxHostWrapper::Args(layout::helpers::codec::hmac_sha1),
        ),
        (
            NGX_MD5_NAME,
            NgxHostWrapper::Args(layout::helpers::codec::md5),
        ),
        (
            NGX_MD5_BIN_NAME,
            NgxHostWrapper::Args(layout::helpers::codec::md5_bin),
        ),
        (
            NGX_SHA1_BIN_NAME,
            NgxHostWrapper::Args(layout::helpers::codec::sha1_bin),
        ),
        (
            NGX_QUOTE_SQL_STR_NAME,
            NgxHostWrapper::Args(layout::helpers::codec::quote_sql_str),
        ),
        (
            NGX_TODAY_NAME,
            NgxHostWrapper::Args(layout::helpers::time::today),
        ),
        (
            NGX_TIME_NAME,
            NgxHostWrapper::Args(layout::helpers::time::time),
        ),
        (
            NGX_NOW_NAME,
            NgxHostWrapper::Args(layout::helpers::time::now),
        ),
        (
            NGX_UPDATE_TIME_NAME,
            NgxHostWrapper::Args(layout::helpers::time::update_time),
        ),
        (
            NGX_LOCALTIME_NAME,
            NgxHostWrapper::Args(layout::helpers::time::localtime),
        ),
        (
            NGX_UTCTIME_NAME,
            NgxHostWrapper::Args(layout::helpers::time::utctime),
        ),
        (
            NGX_COOKIE_TIME_NAME,
            NgxHostWrapper::Args(layout::helpers::time::cookie_time),
        ),
        (
            NGX_HTTP_TIME_NAME,
            NgxHostWrapper::Args(layout::helpers::time::http_time),
        ),
        (
            NGX_PARSE_HTTP_TIME_NAME,
            NgxHostWrapper::Args(layout::helpers::time::parse_http_time),
        ),
        (
            "ngx::get_phase",
            NgxHostWrapper::Args(layout::helpers::meta::get_phase),
        ),
        (
            "ngx::config::subsystem",
            NgxHostWrapper::Args(layout::helpers::meta::config_subsystem),
        ),
        (
            "ngx::config::debug",
            NgxHostWrapper::Args(layout::helpers::meta::config_debug),
        ),
        (
            "ngx::config::prefix",
            NgxHostWrapper::Args(layout::helpers::meta::config_prefix),
        ),
        (
            "ngx::config::nginx_version",
            NgxHostWrapper::Args(layout::helpers::meta::config_nginx_version),
        ),
        (
            "ngx::config::nginx_configure",
            NgxHostWrapper::Args(layout::helpers::meta::config_nginx_configure),
        ),
        (
            "ngx::config::ngx_rustscript_version",
            NgxHostWrapper::Args(layout::helpers::meta::config_ngx_rustscript_version),
        ),
        (
            "ngx::worker::exiting",
            NgxHostWrapper::Args(layout::helpers::meta::worker_exiting),
        ),
        (
            "ngx::worker::pid",
            NgxHostWrapper::Args(layout::helpers::meta::worker_pid),
        ),
        (
            "ngx::worker::pids",
            NgxHostWrapper::Args(layout::helpers::meta::worker_pids),
        ),
        (
            "ngx::worker::count",
            NgxHostWrapper::Args(layout::helpers::meta::worker_count),
        ),
        (
            "ngx::worker::id",
            NgxHostWrapper::Args(layout::helpers::meta::worker_id),
        ),
        ("ngx::log", NgxHostWrapper::Args(layout::helpers::log::log)),
        (
            "ngx::core::ok",
            NgxHostWrapper::Args(layout::helpers::meta::core_ok),
        ),
        (
            "ngx::core::error",
            NgxHostWrapper::Args(layout::helpers::meta::core_error),
        ),
        (
            "ngx::core::again",
            NgxHostWrapper::Args(layout::helpers::meta::core_again),
        ),
        (
            "ngx::core::done",
            NgxHostWrapper::Args(layout::helpers::meta::core_done),
        ),
        (
            "ngx::core::declined",
            NgxHostWrapper::Args(layout::helpers::meta::core_declined),
        ),
        (
            "ngx::core::null_value",
            NgxHostWrapper::Args(layout::helpers::meta::core_null_value),
        ),
        (
            "ngx::http::get",
            NgxHostWrapper::Args(layout::helpers::meta::http_get),
        ),
        (
            "ngx::http::head",
            NgxHostWrapper::Args(layout::helpers::meta::http_head),
        ),
        (
            "ngx::http::put",
            NgxHostWrapper::Args(layout::helpers::meta::http_put),
        ),
        (
            "ngx::http::post",
            NgxHostWrapper::Args(layout::helpers::meta::http_post),
        ),
        (
            "ngx::http::delete",
            NgxHostWrapper::Args(layout::helpers::meta::http_delete),
        ),
        (
            "ngx::http::options",
            NgxHostWrapper::Args(layout::helpers::meta::http_options),
        ),
        (
            "ngx::http::mkcol",
            NgxHostWrapper::Args(layout::helpers::meta::http_mkcol),
        ),
        (
            "ngx::http::copy",
            NgxHostWrapper::Args(layout::helpers::meta::http_copy),
        ),
        (
            "ngx::http::move",
            NgxHostWrapper::Args(layout::helpers::meta::http_move),
        ),
        (
            "ngx::http::propfind",
            NgxHostWrapper::Args(layout::helpers::meta::http_propfind),
        ),
        (
            "ngx::http::proppatch",
            NgxHostWrapper::Args(layout::helpers::meta::http_proppatch),
        ),
        (
            "ngx::http::lock",
            NgxHostWrapper::Args(layout::helpers::meta::http_lock),
        ),
        (
            "ngx::http::unlock",
            NgxHostWrapper::Args(layout::helpers::meta::http_unlock),
        ),
        (
            "ngx::http::patch",
            NgxHostWrapper::Args(layout::helpers::meta::http_patch),
        ),
        (
            "ngx::http::trace",
            NgxHostWrapper::Args(layout::helpers::meta::http_trace),
        ),
        (
            "ngx::http::status_continue",
            NgxHostWrapper::Args(layout::helpers::meta::http_status_continue),
        ),
        (
            "ngx::http::status_switching_protocols",
            NgxHostWrapper::Args(layout::helpers::meta::http_status_switching_protocols),
        ),
        (
            "ngx::http::status_ok",
            NgxHostWrapper::Args(layout::helpers::meta::http_status_ok),
        ),
        (
            "ngx::http::status_created",
            NgxHostWrapper::Args(layout::helpers::meta::http_status_created),
        ),
        (
            "ngx::http::status_accepted",
            NgxHostWrapper::Args(layout::helpers::meta::http_status_accepted),
        ),
        (
            "ngx::http::status_no_content",
            NgxHostWrapper::Args(layout::helpers::meta::http_status_no_content),
        ),
        (
            "ngx::http::status_partial_content",
            NgxHostWrapper::Args(layout::helpers::meta::http_status_partial_content),
        ),
        (
            "ngx::http::status_special_response",
            NgxHostWrapper::Args(layout::helpers::meta::http_status_special_response),
        ),
        (
            "ngx::http::status_moved_permanently",
            NgxHostWrapper::Args(layout::helpers::meta::http_status_moved_permanently),
        ),
        (
            "ngx::http::status_moved_temporarily",
            NgxHostWrapper::Args(layout::helpers::meta::http_status_moved_temporarily),
        ),
        (
            "ngx::http::status_see_other",
            NgxHostWrapper::Args(layout::helpers::meta::http_status_see_other),
        ),
        (
            "ngx::http::status_not_modified",
            NgxHostWrapper::Args(layout::helpers::meta::http_status_not_modified),
        ),
        (
            "ngx::http::status_temporary_redirect",
            NgxHostWrapper::Args(layout::helpers::meta::http_status_temporary_redirect),
        ),
        (
            "ngx::http::status_permanent_redirect",
            NgxHostWrapper::Args(layout::helpers::meta::http_status_permanent_redirect),
        ),
        (
            "ngx::http::status_bad_request",
            NgxHostWrapper::Args(layout::helpers::meta::http_status_bad_request),
        ),
        (
            "ngx::http::status_unauthorized",
            NgxHostWrapper::Args(layout::helpers::meta::http_status_unauthorized),
        ),
        (
            "ngx::http::status_payment_required",
            NgxHostWrapper::Args(layout::helpers::meta::http_status_payment_required),
        ),
        (
            "ngx::http::status_forbidden",
            NgxHostWrapper::Args(layout::helpers::meta::http_status_forbidden),
        ),
        (
            "ngx::http::status_not_found",
            NgxHostWrapper::Args(layout::helpers::meta::http_status_not_found),
        ),
        (
            "ngx::http::status_not_allowed",
            NgxHostWrapper::Args(layout::helpers::meta::http_status_not_allowed),
        ),
        (
            "ngx::http::status_not_acceptable",
            NgxHostWrapper::Args(layout::helpers::meta::http_status_not_acceptable),
        ),
        (
            "ngx::http::status_request_timeout",
            NgxHostWrapper::Args(layout::helpers::meta::http_status_request_timeout),
        ),
        (
            "ngx::http::status_conflict",
            NgxHostWrapper::Args(layout::helpers::meta::http_status_conflict),
        ),
        (
            "ngx::http::status_gone",
            NgxHostWrapper::Args(layout::helpers::meta::http_status_gone),
        ),
        (
            "ngx::http::status_upgrade_required",
            NgxHostWrapper::Args(layout::helpers::meta::http_status_upgrade_required),
        ),
        (
            "ngx::http::status_too_many_requests",
            NgxHostWrapper::Args(layout::helpers::meta::http_status_too_many_requests),
        ),
        (
            "ngx::http::status_illegal",
            NgxHostWrapper::Args(layout::helpers::meta::http_status_illegal),
        ),
        (
            "ngx::http::status_close",
            NgxHostWrapper::Args(layout::helpers::meta::http_status_close),
        ),
        (
            "ngx::http::status_internal_server_error",
            NgxHostWrapper::Args(layout::helpers::meta::http_status_internal_server_error),
        ),
        (
            "ngx::http::status_not_implemented",
            NgxHostWrapper::Args(layout::helpers::meta::http_status_not_implemented),
        ),
        (
            "ngx::http::status_bad_gateway",
            NgxHostWrapper::Args(layout::helpers::meta::http_status_bad_gateway),
        ),
        (
            "ngx::http::status_service_unavailable",
            NgxHostWrapper::Args(layout::helpers::meta::http_status_service_unavailable),
        ),
        (
            "ngx::http::status_gateway_timeout",
            NgxHostWrapper::Args(layout::helpers::meta::http_status_gateway_timeout),
        ),
        (
            "ngx::http::status_version_not_supported",
            NgxHostWrapper::Args(layout::helpers::meta::http_status_version_not_supported),
        ),
        (
            "ngx::http::status_insufficient_storage",
            NgxHostWrapper::Args(layout::helpers::meta::http_status_insufficient_storage),
        ),
        (
            "ngx::log_level::stderr",
            NgxHostWrapper::Args(layout::helpers::meta::log_level_stderr),
        ),
        (
            "ngx::log_level::emerg",
            NgxHostWrapper::Args(layout::helpers::meta::log_level_emerg),
        ),
        (
            "ngx::log_level::alert",
            NgxHostWrapper::Args(layout::helpers::meta::log_level_alert),
        ),
        (
            "ngx::log_level::crit",
            NgxHostWrapper::Args(layout::helpers::meta::log_level_crit),
        ),
        (
            "ngx::log_level::error",
            NgxHostWrapper::Args(layout::helpers::meta::log_level_error),
        ),
        (
            "ngx::log_level::warn",
            NgxHostWrapper::Args(layout::helpers::meta::log_level_warn),
        ),
        (
            "ngx::log_level::notice",
            NgxHostWrapper::Args(layout::helpers::meta::log_level_notice),
        ),
        (
            "ngx::log_level::info",
            NgxHostWrapper::Args(layout::helpers::meta::log_level_info),
        ),
        (
            "ngx::log_level::debug",
            NgxHostWrapper::Args(layout::helpers::meta::log_level_debug),
        ),
        (
            NGX_REQ_GET_METHOD_NAME,
            NgxHostWrapper::Args(layout::helpers::request::get_method),
        ),
        (
            NGX_REQ_HTTP_VERSION_NAME,
            NgxHostWrapper::Args(layout::helpers::request::http_version),
        ),
        (
            NGX_REQ_RAW_HEADER_NAME,
            NgxHostWrapper::Args(layout::helpers::request::raw_header),
        ),
        (
            NGX_REQ_GET_HEADERS_NAME,
            NgxHostWrapper::Args(layout::helpers::request::get_headers),
        ),
        (
            NGX_REQ_GET_URI_ARGS_NAME,
            NgxHostWrapper::Args(layout::helpers::request::get_uri_args),
        ),
        (
            NGX_REQ_GET_POST_ARGS_NAME,
            NgxHostWrapper::Args(layout::helpers::request::get_post_args),
        ),
        (
            NGX_REQ_READ_BODY_NAME,
            NgxHostWrapper::Static(layout::helpers::body::read_body),
        ),
        (
            NGX_REQ_DISCARD_BODY_NAME,
            NgxHostWrapper::Args(layout::helpers::body::discard_body),
        ),
        (
            NGX_REQ_GET_BODY_DATA_NAME,
            NgxHostWrapper::Args(layout::helpers::body::get_body_data),
        ),
        (
            NGX_REQ_GET_BODY_FILE_NAME,
            NgxHostWrapper::Args(layout::helpers::body::get_body_file),
        ),
        (
            NGX_REQ_SET_BODY_DATA_NAME,
            NgxHostWrapper::Args(layout::helpers::body::set_body_data),
        ),
        (
            NGX_REQ_SET_BODY_FILE_NAME,
            NgxHostWrapper::Args(layout::helpers::body::set_body_file),
        ),
        (
            NGX_REQ_INIT_BODY_NAME,
            NgxHostWrapper::Args(layout::helpers::body::init_body),
        ),
        (
            NGX_REQ_APPEND_BODY_NAME,
            NgxHostWrapper::Args(layout::helpers::body::append_body),
        ),
        (
            NGX_REQ_FINISH_BODY_NAME,
            NgxHostWrapper::Args(layout::helpers::body::finish_body),
        ),
        (
            NGX_REQ_IS_INTERNAL_NAME,
            NgxHostWrapper::Args(layout::helpers::request::is_internal),
        ),
        (
            NGX_REQ_START_TIME_NAME,
            NgxHostWrapper::Args(layout::helpers::request::start_time),
        ),
        (
            NGX_IS_SUBREQUEST_NAME,
            NgxHostWrapper::Args(layout::helpers::request::is_subrequest),
        ),
        (
            NGX_HEADERS_SENT_NAME,
            NgxHostWrapper::Args(layout::helpers::request::headers_sent),
        ),
        (
            NGX_VAR_GET_NAME,
            NgxHostWrapper::Args(layout::helpers::variables::get),
        ),
        (
            NGX_REQ_SET_METHOD_NAME,
            NgxHostWrapper::Args(layout::helpers::request::set_method),
        ),
        (
            NGX_REQ_SET_URI_NAME,
            NgxHostWrapper::Args(layout::helpers::request::set_uri),
        ),
        (
            NGX_REQ_SET_URI_ARGS_NAME,
            NgxHostWrapper::Args(layout::helpers::request::set_uri_args),
        ),
        (
            NGX_REQ_SET_HEADER_NAME,
            NgxHostWrapper::Args(layout::helpers::request::set_header),
        ),
        (
            NGX_REQ_CLEAR_HEADER_NAME,
            NgxHostWrapper::Args(layout::helpers::request::clear_header),
        ),
        (
            NGX_VAR_SET_NAME,
            NgxHostWrapper::Args(layout::helpers::variables::set),
        ),
        (
            NGX_STATUS_GET_NAME,
            NgxHostWrapper::Args(layout::helpers::response_meta::status_get),
        ),
        (
            NGX_STATUS_SET_NAME,
            NgxHostWrapper::Args(layout::helpers::response_meta::status_set),
        ),
        (
            NGX_HEADER_GET_NAME,
            NgxHostWrapper::Args(layout::helpers::response_meta::header_get),
        ),
        (
            NGX_HEADER_SET_NAME,
            NgxHostWrapper::Args(layout::helpers::response_meta::header_set),
        ),
        (
            NGX_HEADER_SET_ALL_NAME,
            NgxHostWrapper::Args(layout::helpers::response_meta::header_set_all),
        ),
        (
            NGX_HEADER_CLEAR_NAME,
            NgxHostWrapper::Args(layout::helpers::response_meta::header_clear),
        ),
        (
            NGX_EXIT_NAME,
            NgxHostWrapper::Args(layout::helpers::response_meta::exit),
        ),
        (
            NGX_REDIRECT_NAME,
            NgxHostWrapper::Args(layout::helpers::response_meta::redirect),
        ),
        (
            NGX_CTX_GET_NAME,
            NgxHostWrapper::Args(layout::helpers::request_ctx::get),
        ),
        (
            NGX_CTX_SET_NAME,
            NgxHostWrapper::Args(layout::helpers::request_ctx::set),
        ),
        (
            NGX_CTX_DELETE_NAME,
            NgxHostWrapper::Args(layout::helpers::request_ctx::delete),
        ),
        (
            NGX_CTX_CLEAR_NAME,
            NgxHostWrapper::Args(layout::helpers::request_ctx::clear),
        ),
        (
            NGX_SHARED_DICT_NAME,
            NgxHostWrapper::Static(layout::helpers::shared_dict::dict),
        ),
        (
            NGX_SHARED_GET_NAME,
            NgxHostWrapper::Static(layout::helpers::shared_dict::get),
        ),
        (
            NGX_SHARED_GET_STALE_NAME,
            NgxHostWrapper::Static(layout::helpers::shared_dict::get_stale),
        ),
        (
            NGX_SHARED_SET_NAME,
            NgxHostWrapper::Static(layout::helpers::shared_dict::set),
        ),
        (
            NGX_SHARED_SAFE_SET_NAME,
            NgxHostWrapper::Static(layout::helpers::shared_dict::safe_set),
        ),
        (
            NGX_SHARED_ADD_NAME,
            NgxHostWrapper::Static(layout::helpers::shared_dict::add),
        ),
        (
            NGX_SHARED_SAFE_ADD_NAME,
            NgxHostWrapper::Static(layout::helpers::shared_dict::safe_add),
        ),
        (
            NGX_SHARED_REPLACE_NAME,
            NgxHostWrapper::Static(layout::helpers::shared_dict::replace),
        ),
        (
            NGX_SHARED_DELETE_NAME,
            NgxHostWrapper::Static(layout::helpers::shared_dict::delete),
        ),
        (
            NGX_SHARED_INCR_NAME,
            NgxHostWrapper::Static(layout::helpers::shared_dict::incr),
        ),
        (
            NGX_SHARED_TTL_NAME,
            NgxHostWrapper::Static(layout::helpers::shared_dict::ttl),
        ),
        (
            NGX_SHARED_EXPIRE_NAME,
            NgxHostWrapper::Static(layout::helpers::shared_dict::expire),
        ),
        (
            NGX_SHARED_CAPACITY_NAME,
            NgxHostWrapper::Static(layout::helpers::shared_dict::capacity),
        ),
        (
            NGX_SHARED_FREE_SPACE_NAME,
            NgxHostWrapper::Static(layout::helpers::shared_dict::free_space),
        ),
        (
            NGX_SHARED_LPUSH_NAME,
            NgxHostWrapper::Static(layout::helpers::shared_dict::lpush),
        ),
        (
            NGX_SHARED_RPUSH_NAME,
            NgxHostWrapper::Static(layout::helpers::shared_dict::rpush),
        ),
        (
            NGX_SHARED_LPOP_NAME,
            NgxHostWrapper::Static(layout::helpers::shared_dict::lpop),
        ),
        (
            NGX_SHARED_RPOP_NAME,
            NgxHostWrapper::Static(layout::helpers::shared_dict::rpop),
        ),
        (
            NGX_SHARED_LLEN_NAME,
            NgxHostWrapper::Static(layout::helpers::shared_dict::llen),
        ),
        (
            NGX_SHARED_FLUSH_ALL_NAME,
            NgxHostWrapper::Static(layout::helpers::shared_dict::flush_all),
        ),
        (
            NGX_SHARED_FLUSH_EXPIRED_NAME,
            NgxHostWrapper::Static(layout::helpers::shared_dict::flush_expired),
        ),
        (
            NGX_SHARED_GET_KEYS_NAME,
            NgxHostWrapper::Static(layout::helpers::shared_dict::get_keys),
        ),
        (
            NGX_SOCKET_UDP_NAME,
            NgxHostWrapper::Static(layout::helpers::socket::udp),
        ),
        (
            NGX_SOCKET_UDP_BIND_NAME,
            NgxHostWrapper::Static(layout::helpers::socket::bind),
        ),
        (
            NGX_SOCKET_UDP_SETPEERNAME_NAME,
            NgxHostWrapper::Static(layout::helpers::socket::setpeername),
        ),
        (
            NGX_SOCKET_UDP_SEND_NAME,
            NgxHostWrapper::Static(layout::helpers::socket::send),
        ),
        (
            NGX_SOCKET_UDP_RECEIVE_NAME,
            NgxHostWrapper::Static(layout::helpers::socket::receive),
        ),
        (
            NGX_SOCKET_UDP_CLOSE_NAME,
            NgxHostWrapper::Static(layout::helpers::socket::close),
        ),
        (
            NGX_SOCKET_UDP_SETTIMEOUT_NAME,
            NgxHostWrapper::Static(layout::helpers::socket::settimeout),
        ),
        (
            NGX_SOCKET_TCP_NAME,
            NgxHostWrapper::Static(layout::helpers::socket::tcp),
        ),
        (
            NGX_SOCKET_TCP_CONNECT_NAME,
            NgxHostWrapper::Static(layout::helpers::socket::tcp_connect),
        ),
        (
            NGX_SOCKET_CONNECT_NAME,
            NgxHostWrapper::Static(layout::helpers::socket::connect),
        ),
        (
            NGX_SOCKET_TCP_SEND_NAME,
            NgxHostWrapper::Static(layout::helpers::socket::tcp_send),
        ),
        (
            NGX_SOCKET_TCP_RECEIVE_NAME,
            NgxHostWrapper::Static(layout::helpers::socket::tcp_receive),
        ),
        (
            NGX_SOCKET_TCP_RECEIVEANY_NAME,
            NgxHostWrapper::Static(layout::helpers::socket::tcp_receiveany),
        ),
        (
            NGX_SOCKET_TCP_RECEIVEUNTIL_NAME,
            NgxHostWrapper::Static(layout::helpers::socket::tcp_receiveuntil),
        ),
        (
            NGX_SOCKET_TCP_CLOSE_NAME,
            NgxHostWrapper::Static(layout::helpers::socket::tcp_close),
        ),
        (
            NGX_SOCKET_TCP_SETTIMEOUT_NAME,
            NgxHostWrapper::Static(layout::helpers::socket::tcp_settimeout),
        ),
        (
            NGX_SOCKET_TCP_SETKEEPALIVE_NAME,
            NgxHostWrapper::Static(layout::helpers::socket::tcp_setkeepalive),
        ),
        (
            NGX_SOCKET_TCP_GETREUSEDTIMES_NAME,
            NgxHostWrapper::Static(layout::helpers::socket::tcp_getreusedtimes),
        ),
        (
            NGX_SOCKET_TCP_SSLHANDSHAKE_NAME,
            NgxHostWrapper::Static(layout::helpers::socket::tcp_sslhandshake),
        ),
        (
            NGX_SOCKET_TCP_SETTRUSTEDSTORE_NAME,
            NgxHostWrapper::Static(layout::helpers::socket::tcp_settrustedstore),
        ),
        (
            NGX_SOCKET_TCP_SETCLIENTCERT_NAME,
            NgxHostWrapper::Static(layout::helpers::socket::tcp_setclientcert),
        ),
        (
            NGX_SOCKET_TCP_TRUSTEDSTORE_NAME,
            NgxHostWrapper::Static(layout::helpers::socket::tcp_trustedstore),
        ),
        (
            NGX_SOCKET_TCP_CLIENTCERT_NAME,
            NgxHostWrapper::Static(layout::helpers::socket::tcp_clientcert),
        ),
        (
            NGX_SEND_HEADERS_NAME,
            NgxHostWrapper::Args(layout::helpers::output::send_headers),
        ),
        (
            NGX_PRINT_NAME,
            NgxHostWrapper::Args(layout::helpers::output::print),
        ),
        (
            NGX_SAY_NAME,
            NgxHostWrapper::Args(layout::helpers::output::say),
        ),
        (
            NGX_FLUSH_NAME,
            NgxHostWrapper::Static(layout::helpers::output::flush),
        ),
        (
            NGX_EOF_NAME,
            NgxHostWrapper::Args(layout::helpers::output::eof),
        ),
    ]
}

/// Bind every production host using the macro-generated wrapper.
pub fn bind_ngx_hosts(vm: &mut Vm) {
    for (name, wrapper) in ngx_host_bindings() {
        match *wrapper {
            NgxHostWrapper::Static(function) => vm.bind_static_function(*name, function),
            NgxHostWrapper::Args(function) => vm.bind_static_args_function(*name, function),
        }
    }
}

/// Names of the `ngx::timer` family, bound through the timer-host registry.
///
/// `ngx::timer::{at,every}` consume their callback through
/// `HostParamPassing::TakeOwned`, so they cannot be installed by
/// [`bind_ngx_hosts`]'s name/symbol path: the owned dispatch needs the
/// registry binding path. Every name below is catalogued in [`ngx_host_catalog`]
/// and resolves through [`ngx_host_registry`].
pub fn ngx_timer_host_names() -> &'static [&'static str] {
    &[
        NGX_TIMER_AT_NAME,
        NGX_TIMER_EVERY_NAME,
        NGX_TIMER_PENDING_COUNT_NAME,
        NGX_TIMER_RUNNING_COUNT_NAME,
    ]
}

/// Registry that resolves every production ngx host plus the `ngx::timer`
/// family.
///
/// A VM whose program imports a timer host is bound from this registry
/// (`bind_vm_cached`) instead of the per-name [`bind_ngx_hosts`] path, because
/// the owned `ngx::timer::{at,every}` entries have no name-binding API. The
/// same registry is handed to `register_owned_timer` through
/// `OwnedHostContext`, so the callback VMs spawn from it with the identical
/// host surface.
///
/// The registry is built once per process and cloned per bind; a clone shares
/// the immutable entry table, capability profile, and standard-surface
/// composition, and reuses its host-binding plan cache.
pub fn ngx_host_registry() -> Result<vm::HostFunctionRegistry, String> {
    static REGISTRY: std::sync::OnceLock<Result<vm::HostFunctionRegistry, String>> =
        std::sync::OnceLock::new();
    REGISTRY.get_or_init(build_ngx_host_registry).clone()
}

fn build_ngx_host_registry() -> Result<vm::HostFunctionRegistry, String> {
    let catalog = catalog::compile_host_catalog();
    // The catalog fingerprint is a whole-catalog hash. Compute it once and
    // reuse it for every exact import schema instead of paying it per host.
    let fingerprint = catalog.fingerprint();
    let mut registry = vm::HostFunctionRegistry::new();
    registry
        .install_named_struct_schemas(vm::catalog_named_struct_schemas(&catalog))
        .map_err(|error| format!("install named struct schemas: {error}"))?;
    registry.set_standard_composition(vm::standard_composition());
    for (name, wrapper) in ngx_host_bindings() {
        let functions = catalog.functions_named(name);
        if functions.is_empty() {
            return Err(format!("{name} is missing from the ngx compile catalog"));
        }
        for function in functions {
            let schema = import_schema_with_fingerprint(function, &fingerprint);
            let result = match wrapper {
                NgxHostWrapper::Static(host) => registry.register_catalog_static(schema, *host),
                NgxHostWrapper::Args(host) => registry.register_catalog_static_args(schema, *host),
            };
            result.map_err(|error| format!("register {name}: {error}"))?;
        }
    }
    crate::host::timer::register_timer_hosts(&mut registry, &catalog)?;
    Ok(registry)
}

/// Builds the exact import schema the compiler embeds for `function`.
///
/// Identical to [`vm::HostImportSchema::from_function`] except that the
/// caller-supplied whole-catalog fingerprint is reused.
fn import_schema_with_fingerprint(
    function: &vm::HostFunctionSchema,
    fingerprint: &vm::HostApiFingerprint,
) -> vm::HostImportSchema {
    vm::HostImportSchema {
        name: function.name.clone(),
        params: function
            .params
            .iter()
            .map(|param| vm::HostImportParam {
                name: param.name.clone(),
                schema: param.ty.clone(),
                passing: param.passing,
            })
            .collect(),
        return_type: function.return_type.clone(),
        fingerprint: *fingerprint,
    }
}

#[cfg(test)]
pub(crate) fn bind_test_hosts(vm: &mut Vm) {
    bind_ngx_hosts(vm);
    vm.bind_static_function("ngx::probe", layout::helpers::functions::probe);
    vm.bind_static_function("ngx::ready", layout::helpers::functions::ready);
    vm.bind_static_function("ngx::fail", layout::helpers::functions::fail);
}

mod layout {
    pub use vm::{Value, Vm, VmError, VmResult, resource};

    pub mod helpers {
        #[allow(unused_imports)]
        pub use super::VmResult;
        #[allow(unused_imports)]
        pub use vm::{
            CallOutcome, CaptureAsyncHostContext, FromVmValue, HostFutureOutput,
            IntoHostCallOutcome, VmError, arg, borrow_arg, resource, take_arg,
        };

        pub mod codec {
            include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/host/codec.rs"));
        }
        pub mod time {
            include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/host/time.rs"));
        }
        pub mod meta {
            include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/host/meta.rs"));
        }
        pub mod log {
            include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/host/log.rs"));
        }
        pub mod request {
            include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/host/request.rs"));
        }
        pub mod body {
            include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/host/body.rs"));
        }
        pub mod variables {
            include!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/src/host/variables.rs"
            ));
        }
        pub mod response_meta {
            include!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/src/host/response_meta.rs"
            ));
        }
        pub mod request_ctx {
            include!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/src/host/request_ctx.rs"
            ));
        }
        pub mod shared_dict {
            include!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/src/host/shared_dict.rs"
            ));
        }
        pub mod socket {
            include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/host/socket.rs"));
        }
        pub mod semaphore {
            include!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/src/host/semaphore.rs"
            ));
        }
        pub mod output {
            include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/host/output.rs"));
        }

        pub mod functions {
            use std::time::Duration;

            use super::*;
            use pd_host_function::pd_host_function;
            use vm::{FromVmValue, Value};

            /// Integer or float number accepted by `ngx::sleep`.
            ///
            /// Named `NumberValue` so the pinned macro records a `number` schema
            /// label. pd-vm does not re-export its internal `NumberValue` type.
            #[derive(Clone, Copy, Debug)]
            pub(crate) enum NumberValue {
                Int(i64),
                Float(f64),
            }

            impl FromVmValue<'_> for NumberValue {
                fn from_vm_value(value: &Value, _label: &str) -> VmResult<Self> {
                    match value {
                        Value::Int(number) => Ok(Self::Int(*number)),
                        Value::Float(number) => Ok(Self::Float(*number)),
                        _ => Err(VmError::TypeMismatch("number")),
                    }
                }
            }

            fn duration_from_seconds(seconds: NumberValue) -> VmResult<Duration> {
                match seconds {
                    NumberValue::Int(value) => {
                        if value < 0 {
                            return Err(VmError::HostError(format!(
                                "ngx::sleep expects non-negative seconds, got {value}"
                            )));
                        }
                        Ok(Duration::from_secs(value as u64))
                    }
                    NumberValue::Float(value) => {
                        if value.is_nan() {
                            return Err(VmError::HostError(
                                "ngx::sleep rejects NaN seconds".to_string(),
                            ));
                        }
                        if value.is_infinite() {
                            return Err(VmError::HostError(
                                "ngx::sleep rejects infinite seconds".to_string(),
                            ));
                        }
                        if value < 0.0 {
                            return Err(VmError::HostError(format!(
                                "ngx::sleep expects non-negative seconds, got {value}"
                            )));
                        }
                        Duration::try_from_secs_f64(value).map_err(|_| {
                            VmError::HostError("ngx::sleep duration overflowed".to_string())
                        })
                    }
                }
            }

            /// Sleeps for `seconds` and returns `null`.
            ///
            /// `seconds` is an OpenResty-style number (integer or float). The
            /// generated wrapper submits an owned host future; this body only
            /// validates and awaits Tokio.
            #[pd_host_function(name = "ngx::sleep")]
            pub async fn sleep(seconds: NumberValue) -> VmResult<()> {
                let duration = duration_from_seconds(seconds)?;
                tokio::time::sleep(duration).await;
                Ok(())
            }

            /// Sleeps for `ms` milliseconds and returns `null`.
            ///
            /// `ms` must be a nonnegative integer. The generated wrapper submits
            /// an owned host future; this body only validates and awaits Tokio.
            #[pd_host_function(name = "ngx::sleep_ms")]
            pub async fn sleep_ms(ms: i64) -> VmResult<()> {
                if ms < 0 {
                    return Err(VmError::HostError(format!(
                        "ngx::sleep_ms expects non-negative milliseconds, got {ms}"
                    )));
                }
                let ms = u64::try_from(ms).map_err(|_| {
                    VmError::HostError(
                        "ngx::sleep_ms duration is not a valid millisecond count".into(),
                    )
                })?;
                tokio::time::sleep(Duration::from_millis(ms)).await;
                Ok(())
            }

            /// Test-only async host sharing the generic bridge.
            #[cfg(test)]
            #[pd_host_function(name = "ngx::probe")]
            pub async fn probe(ms: i64) -> VmResult<i64> {
                sleep_ms_impl(ms).await?;
                Ok(ms)
            }

            /// Test-only immediately-ready async host. Named `ngx::ready` so it
            /// cannot shadow production `ngx::now`.
            #[cfg(test)]
            #[pd_host_function(name = "ngx::ready")]
            pub async fn ready() -> VmResult<i64> {
                Ok(7)
            }

            /// Test-only async host that fails after becoming ready.
            #[cfg(test)]
            #[pd_host_function(name = "ngx::fail")]
            pub async fn fail() -> VmResult<i64> {
                Err(VmError::HostError("ngx::fail".into()))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vm::HostTypeSchema;

    #[test]
    fn catalog_matches_macro_sleep_schema() {
        let catalog = ngx_host_catalog();
        let schema = catalog
            .function(NGX_SLEEP_NAME)
            .expect("ngx::sleep is catalogued");
        assert_eq!(schema.name, NGX_SLEEP_NAME);
        assert_eq!(schema.params.len(), 1);
        assert_eq!(schema.params[0].name, "seconds");
        assert_eq!(schema.params[0].ty, HostTypeSchema::Number);
        assert_eq!(schema.return_type, HostTypeSchema::Null);
    }

    #[test]
    fn catalog_matches_macro_sleep_ms_schema() {
        let catalog = ngx_host_catalog();
        let schema = catalog
            .function(NGX_SLEEP_MS_NAME)
            .expect("ngx::sleep_ms is catalogued");
        assert_eq!(schema.name, NGX_SLEEP_MS_NAME);
        assert_eq!(schema.params.len(), 1);
        assert_eq!(schema.params[0].name, "ms");
        assert_eq!(schema.params[0].ty, HostTypeSchema::Int);
        assert_eq!(schema.return_type, HostTypeSchema::Null);
    }

    #[test]
    fn catalog_does_not_include_test_hosts() {
        let catalog = ngx_host_catalog();
        assert!(catalog.function("ngx::probe").is_none());
        assert!(catalog.function("ngx::fail").is_none());
        assert!(catalog.function("ngx::ready").is_none());
        assert!(catalog.function(NGX_NOW_NAME).is_some());
    }

    #[test]
    fn control_stop_sentinel_requires_non_continue_disposition() {
        let err = control_stop_error();
        assert!(is_control_stop(&err));
        let forged = VmError::HostError(CONTROL_STOP_MESSAGE.to_string());
        assert!(is_control_stop(&forged));
        let shaped = VmError::HostError("ngx::control-stop".into());
        assert!(!is_control_stop(&shaped));
        assert!(!is_control_stop(&VmError::HostError("boom".into())));
        let continue_disp = RequestDisposition::Continue;
        assert!(!(is_control_stop(&err) && continue_disp != RequestDisposition::Continue));
        let finalize = RequestDisposition::Finalize(403);
        assert!(is_control_stop(&err) && finalize != RequestDisposition::Continue);
    }
}
