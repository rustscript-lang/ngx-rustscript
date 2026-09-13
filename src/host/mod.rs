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
    host_allowed_in_phase, host_validates_for_log, log_allowlist_accepts, manifest_entry,
    ngx_host_catalog, ngx_host_manifest,
};
pub use catalog::{compile_host_catalog, is_standard_host_import};
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

/// Bind every production host using the macro-generated wrapper.
pub fn bind_ngx_hosts(vm: &mut Vm) {
    vm.bind_static_function(NGX_SLEEP_NAME, layout::helpers::functions::sleep);
    vm.bind_static_function(NGX_SLEEP_MS_NAME, layout::helpers::functions::sleep_ms);
    vm.bind_static_function(NGX_SEMAPHORE_NEW_NAME, layout::helpers::semaphore::new);
    vm.bind_static_function(NGX_SEMAPHORE_WAIT_NAME, layout::helpers::semaphore::wait);
    vm.bind_static_function(NGX_SEMAPHORE_POST_NAME, layout::helpers::semaphore::post);
    vm.bind_static_function(NGX_SEMAPHORE_COUNT_NAME, layout::helpers::semaphore::count);
    vm.bind_static_args_function(NGX_ESCAPE_URI_NAME, layout::helpers::codec::escape_uri);
    vm.bind_static_args_function(NGX_UNESCAPE_URI_NAME, layout::helpers::codec::unescape_uri);
    vm.bind_static_args_function(NGX_ENCODE_ARGS_NAME, layout::helpers::codec::encode_args);
    vm.bind_static_args_function(NGX_DECODE_ARGS_NAME, layout::helpers::codec::decode_args);
    vm.bind_static_args_function(
        NGX_ENCODE_BASE64_NAME,
        layout::helpers::codec::encode_base64,
    );
    vm.bind_static_args_function(
        NGX_DECODE_BASE64_NAME,
        layout::helpers::codec::decode_base64,
    );
    vm.bind_static_args_function(
        NGX_DECODE_BASE64MIME_NAME,
        layout::helpers::codec::decode_base64mime,
    );
    vm.bind_static_args_function(NGX_CRC32_SHORT_NAME, layout::helpers::codec::crc32_short);
    vm.bind_static_args_function(NGX_CRC32_LONG_NAME, layout::helpers::codec::crc32_long);
    vm.bind_static_args_function(NGX_HMAC_SHA1_NAME, layout::helpers::codec::hmac_sha1);
    vm.bind_static_args_function(NGX_MD5_NAME, layout::helpers::codec::md5);
    vm.bind_static_args_function(NGX_MD5_BIN_NAME, layout::helpers::codec::md5_bin);
    vm.bind_static_args_function(NGX_SHA1_BIN_NAME, layout::helpers::codec::sha1_bin);
    vm.bind_static_args_function(
        NGX_QUOTE_SQL_STR_NAME,
        layout::helpers::codec::quote_sql_str,
    );
    vm.bind_static_args_function(NGX_TODAY_NAME, layout::helpers::time::today);
    vm.bind_static_args_function(NGX_TIME_NAME, layout::helpers::time::time);
    vm.bind_static_args_function(NGX_NOW_NAME, layout::helpers::time::now);
    vm.bind_static_args_function(NGX_UPDATE_TIME_NAME, layout::helpers::time::update_time);
    vm.bind_static_args_function(NGX_LOCALTIME_NAME, layout::helpers::time::localtime);
    vm.bind_static_args_function(NGX_UTCTIME_NAME, layout::helpers::time::utctime);
    vm.bind_static_args_function(NGX_COOKIE_TIME_NAME, layout::helpers::time::cookie_time);
    vm.bind_static_args_function(NGX_HTTP_TIME_NAME, layout::helpers::time::http_time);
    vm.bind_static_args_function(
        NGX_PARSE_HTTP_TIME_NAME,
        layout::helpers::time::parse_http_time,
    );
    vm.bind_static_args_function("ngx::get_phase", layout::helpers::meta::get_phase);
    vm.bind_static_args_function(
        "ngx::config::subsystem",
        layout::helpers::meta::config_subsystem,
    );
    vm.bind_static_args_function("ngx::config::debug", layout::helpers::meta::config_debug);
    vm.bind_static_args_function("ngx::config::prefix", layout::helpers::meta::config_prefix);
    vm.bind_static_args_function(
        "ngx::config::nginx_version",
        layout::helpers::meta::config_nginx_version,
    );
    vm.bind_static_args_function(
        "ngx::config::nginx_configure",
        layout::helpers::meta::config_nginx_configure,
    );
    vm.bind_static_args_function(
        "ngx::config::ngx_rustscript_version",
        layout::helpers::meta::config_ngx_rustscript_version,
    );
    vm.bind_static_args_function(
        "ngx::worker::exiting",
        layout::helpers::meta::worker_exiting,
    );
    vm.bind_static_args_function("ngx::worker::pid", layout::helpers::meta::worker_pid);
    vm.bind_static_args_function("ngx::worker::pids", layout::helpers::meta::worker_pids);
    vm.bind_static_args_function("ngx::worker::count", layout::helpers::meta::worker_count);
    vm.bind_static_args_function("ngx::worker::id", layout::helpers::meta::worker_id);
    vm.bind_static_args_function("ngx::log", layout::helpers::log::log);
    vm.bind_static_args_function("ngx::core::ok", layout::helpers::meta::core_ok);
    vm.bind_static_args_function("ngx::core::error", layout::helpers::meta::core_error);
    vm.bind_static_args_function("ngx::core::again", layout::helpers::meta::core_again);
    vm.bind_static_args_function("ngx::core::done", layout::helpers::meta::core_done);
    vm.bind_static_args_function("ngx::core::declined", layout::helpers::meta::core_declined);
    vm.bind_static_args_function(
        "ngx::core::null_value",
        layout::helpers::meta::core_null_value,
    );
    vm.bind_static_args_function("ngx::http::get", layout::helpers::meta::http_get);
    vm.bind_static_args_function("ngx::http::head", layout::helpers::meta::http_head);
    vm.bind_static_args_function("ngx::http::put", layout::helpers::meta::http_put);
    vm.bind_static_args_function("ngx::http::post", layout::helpers::meta::http_post);
    vm.bind_static_args_function("ngx::http::delete", layout::helpers::meta::http_delete);
    vm.bind_static_args_function("ngx::http::options", layout::helpers::meta::http_options);
    vm.bind_static_args_function("ngx::http::mkcol", layout::helpers::meta::http_mkcol);
    vm.bind_static_args_function("ngx::http::copy", layout::helpers::meta::http_copy);
    vm.bind_static_args_function("ngx::http::move", layout::helpers::meta::http_move);
    vm.bind_static_args_function("ngx::http::propfind", layout::helpers::meta::http_propfind);
    vm.bind_static_args_function(
        "ngx::http::proppatch",
        layout::helpers::meta::http_proppatch,
    );
    vm.bind_static_args_function("ngx::http::lock", layout::helpers::meta::http_lock);
    vm.bind_static_args_function("ngx::http::unlock", layout::helpers::meta::http_unlock);
    vm.bind_static_args_function("ngx::http::patch", layout::helpers::meta::http_patch);
    vm.bind_static_args_function("ngx::http::trace", layout::helpers::meta::http_trace);
    vm.bind_static_args_function(
        "ngx::http::status_continue",
        layout::helpers::meta::http_status_continue,
    );
    vm.bind_static_args_function(
        "ngx::http::status_switching_protocols",
        layout::helpers::meta::http_status_switching_protocols,
    );
    vm.bind_static_args_function(
        "ngx::http::status_ok",
        layout::helpers::meta::http_status_ok,
    );
    vm.bind_static_args_function(
        "ngx::http::status_created",
        layout::helpers::meta::http_status_created,
    );
    vm.bind_static_args_function(
        "ngx::http::status_accepted",
        layout::helpers::meta::http_status_accepted,
    );
    vm.bind_static_args_function(
        "ngx::http::status_no_content",
        layout::helpers::meta::http_status_no_content,
    );
    vm.bind_static_args_function(
        "ngx::http::status_partial_content",
        layout::helpers::meta::http_status_partial_content,
    );
    vm.bind_static_args_function(
        "ngx::http::status_special_response",
        layout::helpers::meta::http_status_special_response,
    );
    vm.bind_static_args_function(
        "ngx::http::status_moved_permanently",
        layout::helpers::meta::http_status_moved_permanently,
    );
    vm.bind_static_args_function(
        "ngx::http::status_moved_temporarily",
        layout::helpers::meta::http_status_moved_temporarily,
    );
    vm.bind_static_args_function(
        "ngx::http::status_see_other",
        layout::helpers::meta::http_status_see_other,
    );
    vm.bind_static_args_function(
        "ngx::http::status_not_modified",
        layout::helpers::meta::http_status_not_modified,
    );
    vm.bind_static_args_function(
        "ngx::http::status_temporary_redirect",
        layout::helpers::meta::http_status_temporary_redirect,
    );
    vm.bind_static_args_function(
        "ngx::http::status_permanent_redirect",
        layout::helpers::meta::http_status_permanent_redirect,
    );
    vm.bind_static_args_function(
        "ngx::http::status_bad_request",
        layout::helpers::meta::http_status_bad_request,
    );
    vm.bind_static_args_function(
        "ngx::http::status_unauthorized",
        layout::helpers::meta::http_status_unauthorized,
    );
    vm.bind_static_args_function(
        "ngx::http::status_payment_required",
        layout::helpers::meta::http_status_payment_required,
    );
    vm.bind_static_args_function(
        "ngx::http::status_forbidden",
        layout::helpers::meta::http_status_forbidden,
    );
    vm.bind_static_args_function(
        "ngx::http::status_not_found",
        layout::helpers::meta::http_status_not_found,
    );
    vm.bind_static_args_function(
        "ngx::http::status_not_allowed",
        layout::helpers::meta::http_status_not_allowed,
    );
    vm.bind_static_args_function(
        "ngx::http::status_not_acceptable",
        layout::helpers::meta::http_status_not_acceptable,
    );
    vm.bind_static_args_function(
        "ngx::http::status_request_timeout",
        layout::helpers::meta::http_status_request_timeout,
    );
    vm.bind_static_args_function(
        "ngx::http::status_conflict",
        layout::helpers::meta::http_status_conflict,
    );
    vm.bind_static_args_function(
        "ngx::http::status_gone",
        layout::helpers::meta::http_status_gone,
    );
    vm.bind_static_args_function(
        "ngx::http::status_upgrade_required",
        layout::helpers::meta::http_status_upgrade_required,
    );
    vm.bind_static_args_function(
        "ngx::http::status_too_many_requests",
        layout::helpers::meta::http_status_too_many_requests,
    );
    vm.bind_static_args_function(
        "ngx::http::status_illegal",
        layout::helpers::meta::http_status_illegal,
    );
    vm.bind_static_args_function(
        "ngx::http::status_close",
        layout::helpers::meta::http_status_close,
    );
    vm.bind_static_args_function(
        "ngx::http::status_internal_server_error",
        layout::helpers::meta::http_status_internal_server_error,
    );
    vm.bind_static_args_function(
        "ngx::http::status_not_implemented",
        layout::helpers::meta::http_status_not_implemented,
    );
    vm.bind_static_args_function(
        "ngx::http::status_bad_gateway",
        layout::helpers::meta::http_status_bad_gateway,
    );
    vm.bind_static_args_function(
        "ngx::http::status_service_unavailable",
        layout::helpers::meta::http_status_service_unavailable,
    );
    vm.bind_static_args_function(
        "ngx::http::status_gateway_timeout",
        layout::helpers::meta::http_status_gateway_timeout,
    );
    vm.bind_static_args_function(
        "ngx::http::status_version_not_supported",
        layout::helpers::meta::http_status_version_not_supported,
    );
    vm.bind_static_args_function(
        "ngx::http::status_insufficient_storage",
        layout::helpers::meta::http_status_insufficient_storage,
    );
    vm.bind_static_args_function(
        "ngx::log_level::stderr",
        layout::helpers::meta::log_level_stderr,
    );
    vm.bind_static_args_function(
        "ngx::log_level::emerg",
        layout::helpers::meta::log_level_emerg,
    );
    vm.bind_static_args_function(
        "ngx::log_level::alert",
        layout::helpers::meta::log_level_alert,
    );
    vm.bind_static_args_function(
        "ngx::log_level::crit",
        layout::helpers::meta::log_level_crit,
    );
    vm.bind_static_args_function(
        "ngx::log_level::error",
        layout::helpers::meta::log_level_error,
    );
    vm.bind_static_args_function(
        "ngx::log_level::warn",
        layout::helpers::meta::log_level_warn,
    );
    vm.bind_static_args_function(
        "ngx::log_level::notice",
        layout::helpers::meta::log_level_notice,
    );
    vm.bind_static_args_function(
        "ngx::log_level::info",
        layout::helpers::meta::log_level_info,
    );
    vm.bind_static_args_function(
        "ngx::log_level::debug",
        layout::helpers::meta::log_level_debug,
    );
    vm.bind_static_args_function(
        NGX_REQ_GET_METHOD_NAME,
        layout::helpers::request::get_method,
    );
    vm.bind_static_args_function(
        NGX_REQ_HTTP_VERSION_NAME,
        layout::helpers::request::http_version,
    );
    vm.bind_static_args_function(
        NGX_REQ_RAW_HEADER_NAME,
        layout::helpers::request::raw_header,
    );
    vm.bind_static_args_function(
        NGX_REQ_GET_HEADERS_NAME,
        layout::helpers::request::get_headers,
    );
    vm.bind_static_args_function(
        NGX_REQ_GET_URI_ARGS_NAME,
        layout::helpers::request::get_uri_args,
    );
    vm.bind_static_args_function(
        NGX_REQ_GET_POST_ARGS_NAME,
        layout::helpers::request::get_post_args,
    );
    vm.bind_static_function(NGX_REQ_READ_BODY_NAME, layout::helpers::body::read_body);
    vm.bind_static_args_function(
        NGX_REQ_DISCARD_BODY_NAME,
        layout::helpers::body::discard_body,
    );
    vm.bind_static_args_function(
        NGX_REQ_GET_BODY_DATA_NAME,
        layout::helpers::body::get_body_data,
    );
    vm.bind_static_args_function(
        NGX_REQ_GET_BODY_FILE_NAME,
        layout::helpers::body::get_body_file,
    );
    vm.bind_static_args_function(
        NGX_REQ_SET_BODY_DATA_NAME,
        layout::helpers::body::set_body_data,
    );
    vm.bind_static_args_function(
        NGX_REQ_SET_BODY_FILE_NAME,
        layout::helpers::body::set_body_file,
    );
    vm.bind_static_args_function(NGX_REQ_INIT_BODY_NAME, layout::helpers::body::init_body);
    vm.bind_static_args_function(NGX_REQ_APPEND_BODY_NAME, layout::helpers::body::append_body);
    vm.bind_static_args_function(NGX_REQ_FINISH_BODY_NAME, layout::helpers::body::finish_body);
    vm.bind_static_args_function(
        NGX_REQ_IS_INTERNAL_NAME,
        layout::helpers::request::is_internal,
    );
    vm.bind_static_args_function(
        NGX_REQ_START_TIME_NAME,
        layout::helpers::request::start_time,
    );
    vm.bind_static_args_function(
        NGX_IS_SUBREQUEST_NAME,
        layout::helpers::request::is_subrequest,
    );
    vm.bind_static_args_function(
        NGX_HEADERS_SENT_NAME,
        layout::helpers::request::headers_sent,
    );
    vm.bind_static_args_function(NGX_VAR_GET_NAME, layout::helpers::variables::get);
    vm.bind_static_args_function(
        NGX_REQ_SET_METHOD_NAME,
        layout::helpers::request::set_method,
    );
    vm.bind_static_args_function(NGX_REQ_SET_URI_NAME, layout::helpers::request::set_uri);
    vm.bind_static_args_function(
        NGX_REQ_SET_URI_ARGS_NAME,
        layout::helpers::request::set_uri_args,
    );
    vm.bind_static_args_function(
        NGX_REQ_SET_HEADER_NAME,
        layout::helpers::request::set_header,
    );
    vm.bind_static_args_function(
        NGX_REQ_CLEAR_HEADER_NAME,
        layout::helpers::request::clear_header,
    );
    vm.bind_static_args_function(NGX_VAR_SET_NAME, layout::helpers::variables::set);
    vm.bind_static_args_function(
        NGX_STATUS_GET_NAME,
        layout::helpers::response_meta::status_get,
    );
    vm.bind_static_args_function(
        NGX_STATUS_SET_NAME,
        layout::helpers::response_meta::status_set,
    );
    vm.bind_static_args_function(
        NGX_HEADER_GET_NAME,
        layout::helpers::response_meta::header_get,
    );
    vm.bind_static_args_function(
        NGX_HEADER_SET_NAME,
        layout::helpers::response_meta::header_set,
    );
    vm.bind_static_args_function(
        NGX_HEADER_SET_ALL_NAME,
        layout::helpers::response_meta::header_set_all,
    );
    vm.bind_static_args_function(
        NGX_HEADER_CLEAR_NAME,
        layout::helpers::response_meta::header_clear,
    );
    vm.bind_static_args_function(NGX_EXIT_NAME, layout::helpers::response_meta::exit);
    vm.bind_static_args_function(NGX_REDIRECT_NAME, layout::helpers::response_meta::redirect);
    vm.bind_static_args_function(NGX_CTX_GET_NAME, layout::helpers::request_ctx::get);
    vm.bind_static_args_function(NGX_CTX_SET_NAME, layout::helpers::request_ctx::set);
    vm.bind_static_args_function(NGX_CTX_DELETE_NAME, layout::helpers::request_ctx::delete);
    vm.bind_static_args_function(NGX_CTX_CLEAR_NAME, layout::helpers::request_ctx::clear);
    vm.bind_static_function(NGX_SHARED_DICT_NAME, layout::helpers::shared_dict::dict);
    vm.bind_static_function(NGX_SHARED_GET_NAME, layout::helpers::shared_dict::get);
    vm.bind_static_function(
        NGX_SHARED_GET_STALE_NAME,
        layout::helpers::shared_dict::get_stale,
    );
    vm.bind_static_function(NGX_SHARED_SET_NAME, layout::helpers::shared_dict::set);
    vm.bind_static_function(
        NGX_SHARED_SAFE_SET_NAME,
        layout::helpers::shared_dict::safe_set,
    );
    vm.bind_static_function(NGX_SHARED_ADD_NAME, layout::helpers::shared_dict::add);
    vm.bind_static_function(
        NGX_SHARED_SAFE_ADD_NAME,
        layout::helpers::shared_dict::safe_add,
    );
    vm.bind_static_function(
        NGX_SHARED_REPLACE_NAME,
        layout::helpers::shared_dict::replace,
    );
    vm.bind_static_function(NGX_SHARED_DELETE_NAME, layout::helpers::shared_dict::delete);
    vm.bind_static_function(NGX_SHARED_INCR_NAME, layout::helpers::shared_dict::incr);
    vm.bind_static_function(NGX_SHARED_TTL_NAME, layout::helpers::shared_dict::ttl);
    vm.bind_static_function(NGX_SHARED_EXPIRE_NAME, layout::helpers::shared_dict::expire);
    vm.bind_static_function(
        NGX_SHARED_CAPACITY_NAME,
        layout::helpers::shared_dict::capacity,
    );
    vm.bind_static_function(
        NGX_SHARED_FREE_SPACE_NAME,
        layout::helpers::shared_dict::free_space,
    );
    vm.bind_static_function(NGX_SHARED_LPUSH_NAME, layout::helpers::shared_dict::lpush);
    vm.bind_static_function(NGX_SHARED_RPUSH_NAME, layout::helpers::shared_dict::rpush);
    vm.bind_static_function(NGX_SHARED_LPOP_NAME, layout::helpers::shared_dict::lpop);
    vm.bind_static_function(NGX_SHARED_RPOP_NAME, layout::helpers::shared_dict::rpop);
    vm.bind_static_function(NGX_SHARED_LLEN_NAME, layout::helpers::shared_dict::llen);
    vm.bind_static_function(
        NGX_SHARED_FLUSH_ALL_NAME,
        layout::helpers::shared_dict::flush_all,
    );
    vm.bind_static_function(
        NGX_SHARED_FLUSH_EXPIRED_NAME,
        layout::helpers::shared_dict::flush_expired,
    );
    vm.bind_static_function(
        NGX_SHARED_GET_KEYS_NAME,
        layout::helpers::shared_dict::get_keys,
    );
    vm.bind_static_function(NGX_SOCKET_UDP_NAME, layout::helpers::socket::udp);
    vm.bind_static_function(NGX_SOCKET_UDP_BIND_NAME, layout::helpers::socket::bind);
    vm.bind_static_function(
        NGX_SOCKET_UDP_SETPEERNAME_NAME,
        layout::helpers::socket::setpeername,
    );
    vm.bind_static_function(NGX_SOCKET_UDP_SEND_NAME, layout::helpers::socket::send);
    vm.bind_static_function(
        NGX_SOCKET_UDP_RECEIVE_NAME,
        layout::helpers::socket::receive,
    );
    vm.bind_static_function(NGX_SOCKET_UDP_CLOSE_NAME, layout::helpers::socket::close);
    vm.bind_static_function(
        NGX_SOCKET_UDP_SETTIMEOUT_NAME,
        layout::helpers::socket::settimeout,
    );
    vm.bind_static_function(NGX_SOCKET_TCP_NAME, layout::helpers::socket::tcp);
    vm.bind_static_function(
        NGX_SOCKET_TCP_CONNECT_NAME,
        layout::helpers::socket::tcp_connect,
    );
    vm.bind_static_function(NGX_SOCKET_CONNECT_NAME, layout::helpers::socket::connect);
    vm.bind_static_function(NGX_SOCKET_TCP_SEND_NAME, layout::helpers::socket::tcp_send);
    vm.bind_static_function(
        NGX_SOCKET_TCP_RECEIVE_NAME,
        layout::helpers::socket::tcp_receive,
    );
    vm.bind_static_function(
        NGX_SOCKET_TCP_RECEIVEANY_NAME,
        layout::helpers::socket::tcp_receiveany,
    );
    vm.bind_static_function(
        NGX_SOCKET_TCP_RECEIVEUNTIL_NAME,
        layout::helpers::socket::tcp_receiveuntil,
    );
    vm.bind_static_function(
        NGX_SOCKET_TCP_CLOSE_NAME,
        layout::helpers::socket::tcp_close,
    );
    vm.bind_static_function(
        NGX_SOCKET_TCP_SETTIMEOUT_NAME,
        layout::helpers::socket::tcp_settimeout,
    );
    vm.bind_static_function(
        NGX_SOCKET_TCP_SETKEEPALIVE_NAME,
        layout::helpers::socket::tcp_setkeepalive,
    );
    vm.bind_static_function(
        NGX_SOCKET_TCP_GETREUSEDTIMES_NAME,
        layout::helpers::socket::tcp_getreusedtimes,
    );
    vm.bind_static_function(
        NGX_SOCKET_TCP_SSLHANDSHAKE_NAME,
        layout::helpers::socket::tcp_sslhandshake,
    );
    vm.bind_static_function(
        NGX_SOCKET_TCP_SETTRUSTEDSTORE_NAME,
        layout::helpers::socket::tcp_settrustedstore,
    );
    vm.bind_static_function(
        NGX_SOCKET_TCP_SETCLIENTCERT_NAME,
        layout::helpers::socket::tcp_setclientcert,
    );
    vm.bind_static_function(
        NGX_SOCKET_TCP_TRUSTEDSTORE_NAME,
        layout::helpers::socket::tcp_trustedstore,
    );
    vm.bind_static_function(
        NGX_SOCKET_TCP_CLIENTCERT_NAME,
        layout::helpers::socket::tcp_clientcert,
    );
    vm.bind_static_args_function(NGX_SEND_HEADERS_NAME, layout::helpers::output::send_headers);
    vm.bind_static_args_function(NGX_PRINT_NAME, layout::helpers::output::print);
    vm.bind_static_args_function(NGX_SAY_NAME, layout::helpers::output::say);
    vm.bind_static_function(NGX_FLUSH_NAME, layout::helpers::output::flush);
    vm.bind_static_args_function(NGX_EOF_NAME, layout::helpers::output::eof);
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
