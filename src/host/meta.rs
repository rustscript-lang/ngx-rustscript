// Worker, build, phase, and constant-family metadata hosts.
//
// Values returned to the VM are owned. Live worker fields are read from
// public NGINX globals on this worker thread; they are never sourced from
// `/proc`, the environment, or hardcoded process identity.

use nginx_sys::{ngx_cycle, ngx_exiting, ngx_pid, ngx_processes, ngx_str_t, ngx_worker};
use ngx::core::{CoreModuleMainConf, NgxCoreModule};
use pd_host_function::pd_host_function;
use vm::HostModuleDescriptor;
use vm::{Value, VmError};

use super::super::VmResult;
use super::{CallOutcome, IntoHostCallOutcome};

/// `ngx.config.ngx_lua_version` encodes 0.x.y as `x * 1000 + y`.
/// Crate version 0.1.0 therefore reports 1000.
pub const NGX_RUSTSCRIPT_VERSION: i64 = 1_000;

fn ret(value: impl IntoHostCallOutcome) -> VmResult<CallOutcome> {
    Ok(value.into_host_call_outcome())
}

fn owned_ngx_str(value: ngx_str_t) -> String {
    if value.data.is_null() || value.len == 0 {
        return String::new();
    }
    // SAFETY: `data`/`len` describe a live NGINX string buffer.
    let bytes = unsafe { core::slice::from_raw_parts(value.data, value.len) };
    String::from_utf8_lossy(bytes).into_owned()
}

fn live_cycle() -> Result<&'static nginx_sys::ngx_cycle_t, VmError> {
    // SAFETY: `ngx_cycle` is a process-lifetime NGINX global. The pointer is
    // copied; we do not retain it across await.
    let cycle = unsafe { core::ptr::addr_of!(ngx_cycle).read() };
    if cycle.is_null() {
        return Err(VmError::HostError(
            "ngx metadata requires a live NGINX cycle".into(),
        ));
    }
    // SAFETY: non-null `ngx_cycle` is the current cycle on this worker.
    Ok(unsafe { &*cycle })
}

fn phase_openresty_name(phase: crate::host::NgxPhase) -> &'static str {
    match phase {
        crate::host::NgxPhase::Init => "init",
        crate::host::NgxPhase::InitWorker => "init_worker",
        crate::host::NgxPhase::Set => "set",
        crate::host::NgxPhase::Rewrite => "rewrite",
        crate::host::NgxPhase::Access => "access",
        crate::host::NgxPhase::Content => "content",
        crate::host::NgxPhase::HeaderFilter => "header_filter",
        crate::host::NgxPhase::BodyFilter => "body_filter",
        crate::host::NgxPhase::Log => "log",
        crate::host::NgxPhase::Balancer => "balancer",
        crate::host::NgxPhase::SslCertificate => "ssl_cert",
        crate::host::NgxPhase::SslSessionFetch => "ssl_session_fetch",
        crate::host::NgxPhase::SslSessionStore => "ssl_session_store",
        crate::host::NgxPhase::SslClientHello => "ssl_client_hello",
        crate::host::NgxPhase::ExitWorker => "exit_worker",
        crate::host::NgxPhase::Timer => "timer",
    }
}

/// Zero-argument OpenResty-compatible NGINX constant.
#[pd_host_function(name = "ngx::core::ok", contract = crate::host::catalog::ngx_core_ok_contract)]
pub fn core_ok() -> VmResult<CallOutcome> {
    ret(nginx_sys::NGX_OK as i64)
}
/// Zero-argument OpenResty-compatible NGINX constant.
#[pd_host_function(name = "ngx::core::error", contract = crate::host::catalog::ngx_core_error_contract)]
pub fn core_error() -> VmResult<CallOutcome> {
    ret(nginx_sys::NGX_ERROR as i64)
}
/// Zero-argument OpenResty-compatible NGINX constant.
#[pd_host_function(name = "ngx::core::again", contract = crate::host::catalog::ngx_core_again_contract)]
pub fn core_again() -> VmResult<CallOutcome> {
    ret(nginx_sys::NGX_AGAIN as i64)
}
/// Zero-argument OpenResty-compatible NGINX constant.
#[pd_host_function(name = "ngx::core::done", contract = crate::host::catalog::ngx_core_done_contract)]
pub fn core_done() -> VmResult<CallOutcome> {
    ret(nginx_sys::NGX_DONE as i64)
}
/// Zero-argument OpenResty-compatible NGINX constant.
#[pd_host_function(name = "ngx::core::declined", contract = crate::host::catalog::ngx_core_declined_contract)]
pub fn core_declined() -> VmResult<CallOutcome> {
    ret(nginx_sys::NGX_DECLINED as i64)
}

/// OpenResty `ngx.null`. Named `null_value` because `null` is a reserved token.
#[pd_host_function(name = "ngx::core::null_value", contract = crate::host::catalog::ngx_core_null_value_contract)]
pub fn core_null_value() -> VmResult<CallOutcome> {
    ret(())
}

/// Zero-argument OpenResty-compatible NGINX constant.
#[pd_host_function(name = "ngx::http::get", contract = crate::host::catalog::ngx_http_get_contract)]
pub fn http_get() -> VmResult<CallOutcome> {
    ret(nginx_sys::NGX_HTTP_GET as i64)
}
/// Zero-argument OpenResty-compatible NGINX constant.
#[pd_host_function(name = "ngx::http::head", contract = crate::host::catalog::ngx_http_head_contract)]
pub fn http_head() -> VmResult<CallOutcome> {
    ret(nginx_sys::NGX_HTTP_HEAD as i64)
}
/// Zero-argument OpenResty-compatible NGINX constant.
#[pd_host_function(name = "ngx::http::put", contract = crate::host::catalog::ngx_http_put_contract)]
pub fn http_put() -> VmResult<CallOutcome> {
    ret(nginx_sys::NGX_HTTP_PUT as i64)
}
/// Zero-argument OpenResty-compatible NGINX constant.
#[pd_host_function(name = "ngx::http::post", contract = crate::host::catalog::ngx_http_post_contract)]
pub fn http_post() -> VmResult<CallOutcome> {
    ret(nginx_sys::NGX_HTTP_POST as i64)
}
/// Zero-argument OpenResty-compatible NGINX constant.
#[pd_host_function(name = "ngx::http::delete", contract = crate::host::catalog::ngx_http_delete_contract)]
pub fn http_delete() -> VmResult<CallOutcome> {
    ret(nginx_sys::NGX_HTTP_DELETE as i64)
}
/// Zero-argument OpenResty-compatible NGINX constant.
#[pd_host_function(name = "ngx::http::options", contract = crate::host::catalog::ngx_http_options_contract)]
pub fn http_options() -> VmResult<CallOutcome> {
    ret(nginx_sys::NGX_HTTP_OPTIONS as i64)
}
/// Zero-argument OpenResty-compatible NGINX constant.
#[pd_host_function(name = "ngx::http::mkcol", contract = crate::host::catalog::ngx_http_mkcol_contract)]
pub fn http_mkcol() -> VmResult<CallOutcome> {
    ret(nginx_sys::NGX_HTTP_MKCOL as i64)
}
/// Zero-argument OpenResty-compatible NGINX constant.
#[pd_host_function(name = "ngx::http::copy", contract = crate::host::catalog::ngx_http_copy_contract)]
pub fn http_copy() -> VmResult<CallOutcome> {
    ret(nginx_sys::NGX_HTTP_COPY as i64)
}
/// Zero-argument OpenResty-compatible NGINX constant.
#[pd_host_function(name = "ngx::http::move", contract = crate::host::catalog::ngx_http_move_contract)]
pub fn http_move() -> VmResult<CallOutcome> {
    ret(nginx_sys::NGX_HTTP_MOVE as i64)
}
/// Zero-argument OpenResty-compatible NGINX constant.
#[pd_host_function(name = "ngx::http::propfind", contract = crate::host::catalog::ngx_http_propfind_contract)]
pub fn http_propfind() -> VmResult<CallOutcome> {
    ret(nginx_sys::NGX_HTTP_PROPFIND as i64)
}
/// Zero-argument OpenResty-compatible NGINX constant.
#[pd_host_function(name = "ngx::http::proppatch", contract = crate::host::catalog::ngx_http_proppatch_contract)]
pub fn http_proppatch() -> VmResult<CallOutcome> {
    ret(nginx_sys::NGX_HTTP_PROPPATCH as i64)
}
/// Zero-argument OpenResty-compatible NGINX constant.
#[pd_host_function(name = "ngx::http::lock", contract = crate::host::catalog::ngx_http_lock_contract)]
pub fn http_lock() -> VmResult<CallOutcome> {
    ret(nginx_sys::NGX_HTTP_LOCK as i64)
}
/// Zero-argument OpenResty-compatible NGINX constant.
#[pd_host_function(name = "ngx::http::unlock", contract = crate::host::catalog::ngx_http_unlock_contract)]
pub fn http_unlock() -> VmResult<CallOutcome> {
    ret(nginx_sys::NGX_HTTP_UNLOCK as i64)
}
/// Zero-argument OpenResty-compatible NGINX constant.
#[pd_host_function(name = "ngx::http::patch", contract = crate::host::catalog::ngx_http_patch_contract)]
pub fn http_patch() -> VmResult<CallOutcome> {
    ret(nginx_sys::NGX_HTTP_PATCH as i64)
}
/// Zero-argument OpenResty-compatible NGINX constant.
#[pd_host_function(name = "ngx::http::trace", contract = crate::host::catalog::ngx_http_trace_contract)]
pub fn http_trace() -> VmResult<CallOutcome> {
    ret(nginx_sys::NGX_HTTP_TRACE as i64)
}

/// Zero-argument OpenResty-compatible NGINX constant.
#[pd_host_function(name = "ngx::http::status_continue", contract = crate::host::catalog::ngx_http_status_continue_contract)]
pub fn http_status_continue() -> VmResult<CallOutcome> {
    ret(nginx_sys::NGX_HTTP_CONTINUE as i64)
}
/// Zero-argument OpenResty-compatible NGINX constant.
#[pd_host_function(name = "ngx::http::status_switching_protocols", contract = crate::host::catalog::ngx_http_status_switching_protocols_contract)]
pub fn http_status_switching_protocols() -> VmResult<CallOutcome> {
    ret(nginx_sys::NGX_HTTP_SWITCHING_PROTOCOLS as i64)
}
/// Zero-argument OpenResty-compatible NGINX constant.
#[pd_host_function(name = "ngx::http::status_ok", contract = crate::host::catalog::ngx_http_status_ok_contract)]
pub fn http_status_ok() -> VmResult<CallOutcome> {
    ret(nginx_sys::NGX_HTTP_OK as i64)
}
/// Zero-argument OpenResty-compatible NGINX constant.
#[pd_host_function(name = "ngx::http::status_created", contract = crate::host::catalog::ngx_http_status_created_contract)]
pub fn http_status_created() -> VmResult<CallOutcome> {
    ret(nginx_sys::NGX_HTTP_CREATED as i64)
}
/// Zero-argument OpenResty-compatible NGINX constant.
#[pd_host_function(name = "ngx::http::status_accepted", contract = crate::host::catalog::ngx_http_status_accepted_contract)]
pub fn http_status_accepted() -> VmResult<CallOutcome> {
    ret(nginx_sys::NGX_HTTP_ACCEPTED as i64)
}
/// Zero-argument OpenResty-compatible NGINX constant.
#[pd_host_function(name = "ngx::http::status_no_content", contract = crate::host::catalog::ngx_http_status_no_content_contract)]
pub fn http_status_no_content() -> VmResult<CallOutcome> {
    ret(nginx_sys::NGX_HTTP_NO_CONTENT as i64)
}
/// Zero-argument OpenResty-compatible NGINX constant.
#[pd_host_function(name = "ngx::http::status_partial_content", contract = crate::host::catalog::ngx_http_status_partial_content_contract)]
pub fn http_status_partial_content() -> VmResult<CallOutcome> {
    ret(nginx_sys::NGX_HTTP_PARTIAL_CONTENT as i64)
}
/// Zero-argument OpenResty-compatible NGINX constant.
#[pd_host_function(name = "ngx::http::status_special_response", contract = crate::host::catalog::ngx_http_status_special_response_contract)]
pub fn http_status_special_response() -> VmResult<CallOutcome> {
    ret(nginx_sys::NGX_HTTP_SPECIAL_RESPONSE as i64)
}
/// Zero-argument OpenResty-compatible NGINX constant.
#[pd_host_function(name = "ngx::http::status_moved_permanently", contract = crate::host::catalog::ngx_http_status_moved_permanently_contract)]
pub fn http_status_moved_permanently() -> VmResult<CallOutcome> {
    ret(nginx_sys::NGX_HTTP_MOVED_PERMANENTLY as i64)
}
/// Zero-argument OpenResty-compatible NGINX constant.
#[pd_host_function(name = "ngx::http::status_moved_temporarily", contract = crate::host::catalog::ngx_http_status_moved_temporarily_contract)]
pub fn http_status_moved_temporarily() -> VmResult<CallOutcome> {
    ret(nginx_sys::NGX_HTTP_MOVED_TEMPORARILY as i64)
}
/// Zero-argument OpenResty-compatible NGINX constant.
#[pd_host_function(name = "ngx::http::status_see_other", contract = crate::host::catalog::ngx_http_status_see_other_contract)]
pub fn http_status_see_other() -> VmResult<CallOutcome> {
    ret(nginx_sys::NGX_HTTP_SEE_OTHER as i64)
}
/// Zero-argument OpenResty-compatible NGINX constant.
#[pd_host_function(name = "ngx::http::status_not_modified", contract = crate::host::catalog::ngx_http_status_not_modified_contract)]
pub fn http_status_not_modified() -> VmResult<CallOutcome> {
    ret(nginx_sys::NGX_HTTP_NOT_MODIFIED as i64)
}
/// Zero-argument OpenResty-compatible NGINX constant.
#[pd_host_function(name = "ngx::http::status_temporary_redirect", contract = crate::host::catalog::ngx_http_status_temporary_redirect_contract)]
pub fn http_status_temporary_redirect() -> VmResult<CallOutcome> {
    ret(nginx_sys::NGX_HTTP_TEMPORARY_REDIRECT as i64)
}
/// Zero-argument OpenResty-compatible NGINX constant.
#[pd_host_function(name = "ngx::http::status_permanent_redirect", contract = crate::host::catalog::ngx_http_status_permanent_redirect_contract)]
pub fn http_status_permanent_redirect() -> VmResult<CallOutcome> {
    ret(nginx_sys::NGX_HTTP_PERMANENT_REDIRECT as i64)
}
/// Zero-argument OpenResty-compatible NGINX constant.
#[pd_host_function(name = "ngx::http::status_bad_request", contract = crate::host::catalog::ngx_http_status_bad_request_contract)]
pub fn http_status_bad_request() -> VmResult<CallOutcome> {
    ret(nginx_sys::NGX_HTTP_BAD_REQUEST as i64)
}
/// Zero-argument OpenResty-compatible NGINX constant.
#[pd_host_function(name = "ngx::http::status_unauthorized", contract = crate::host::catalog::ngx_http_status_unauthorized_contract)]
pub fn http_status_unauthorized() -> VmResult<CallOutcome> {
    ret(nginx_sys::NGX_HTTP_UNAUTHORIZED as i64)
}
/// Zero-argument OpenResty-compatible NGINX constant.
#[pd_host_function(name = "ngx::http::status_payment_required", contract = crate::host::catalog::ngx_http_status_payment_required_contract)]
pub fn http_status_payment_required() -> VmResult<CallOutcome> {
    ret(402_i64)
}
/// Zero-argument OpenResty-compatible NGINX constant.
#[pd_host_function(name = "ngx::http::status_forbidden", contract = crate::host::catalog::ngx_http_status_forbidden_contract)]
pub fn http_status_forbidden() -> VmResult<CallOutcome> {
    ret(nginx_sys::NGX_HTTP_FORBIDDEN as i64)
}
/// Zero-argument OpenResty-compatible NGINX constant.
#[pd_host_function(name = "ngx::http::status_not_found", contract = crate::host::catalog::ngx_http_status_not_found_contract)]
pub fn http_status_not_found() -> VmResult<CallOutcome> {
    ret(nginx_sys::NGX_HTTP_NOT_FOUND as i64)
}
/// Zero-argument OpenResty-compatible NGINX constant.
#[pd_host_function(name = "ngx::http::status_not_allowed", contract = crate::host::catalog::ngx_http_status_not_allowed_contract)]
pub fn http_status_not_allowed() -> VmResult<CallOutcome> {
    ret(nginx_sys::NGX_HTTP_NOT_ALLOWED as i64)
}
/// Zero-argument OpenResty-compatible NGINX constant.
#[pd_host_function(name = "ngx::http::status_not_acceptable", contract = crate::host::catalog::ngx_http_status_not_acceptable_contract)]
pub fn http_status_not_acceptable() -> VmResult<CallOutcome> {
    ret(406_i64)
}
/// Zero-argument OpenResty-compatible NGINX constant.
#[pd_host_function(name = "ngx::http::status_request_timeout", contract = crate::host::catalog::ngx_http_status_request_timeout_contract)]
pub fn http_status_request_timeout() -> VmResult<CallOutcome> {
    ret(nginx_sys::NGX_HTTP_REQUEST_TIME_OUT as i64)
}
/// Zero-argument OpenResty-compatible NGINX constant.
#[pd_host_function(name = "ngx::http::status_conflict", contract = crate::host::catalog::ngx_http_status_conflict_contract)]
pub fn http_status_conflict() -> VmResult<CallOutcome> {
    ret(nginx_sys::NGX_HTTP_CONFLICT as i64)
}
/// Zero-argument OpenResty-compatible NGINX constant.
#[pd_host_function(name = "ngx::http::status_gone", contract = crate::host::catalog::ngx_http_status_gone_contract)]
pub fn http_status_gone() -> VmResult<CallOutcome> {
    ret(410_i64)
}
/// Zero-argument OpenResty-compatible NGINX constant.
#[pd_host_function(name = "ngx::http::status_upgrade_required", contract = crate::host::catalog::ngx_http_status_upgrade_required_contract)]
pub fn http_status_upgrade_required() -> VmResult<CallOutcome> {
    ret(426_i64)
}
/// Zero-argument OpenResty-compatible NGINX constant.
#[pd_host_function(name = "ngx::http::status_too_many_requests", contract = crate::host::catalog::ngx_http_status_too_many_requests_contract)]
pub fn http_status_too_many_requests() -> VmResult<CallOutcome> {
    ret(nginx_sys::NGX_HTTP_TOO_MANY_REQUESTS as i64)
}
/// Zero-argument OpenResty-compatible NGINX constant.
#[pd_host_function(name = "ngx::http::status_illegal", contract = crate::host::catalog::ngx_http_status_illegal_contract)]
pub fn http_status_illegal() -> VmResult<CallOutcome> {
    ret(451_i64)
}
/// Zero-argument OpenResty-compatible NGINX constant.
#[pd_host_function(name = "ngx::http::status_close", contract = crate::host::catalog::ngx_http_status_close_contract)]
pub fn http_status_close() -> VmResult<CallOutcome> {
    ret(nginx_sys::NGX_HTTP_CLOSE as i64)
}
/// Zero-argument OpenResty-compatible NGINX constant.
#[pd_host_function(name = "ngx::http::status_internal_server_error", contract = crate::host::catalog::ngx_http_status_internal_server_error_contract)]
pub fn http_status_internal_server_error() -> VmResult<CallOutcome> {
    ret(nginx_sys::NGX_HTTP_INTERNAL_SERVER_ERROR as i64)
}
/// Zero-argument OpenResty-compatible NGINX constant.
#[pd_host_function(name = "ngx::http::status_not_implemented", contract = crate::host::catalog::ngx_http_status_not_implemented_contract)]
pub fn http_status_not_implemented() -> VmResult<CallOutcome> {
    ret(nginx_sys::NGX_HTTP_NOT_IMPLEMENTED as i64)
}
/// Zero-argument OpenResty-compatible NGINX constant.
#[pd_host_function(name = "ngx::http::status_bad_gateway", contract = crate::host::catalog::ngx_http_status_bad_gateway_contract)]
pub fn http_status_bad_gateway() -> VmResult<CallOutcome> {
    ret(nginx_sys::NGX_HTTP_BAD_GATEWAY as i64)
}
/// Zero-argument OpenResty-compatible NGINX constant.
#[pd_host_function(name = "ngx::http::status_service_unavailable", contract = crate::host::catalog::ngx_http_status_service_unavailable_contract)]
pub fn http_status_service_unavailable() -> VmResult<CallOutcome> {
    ret(nginx_sys::NGX_HTTP_SERVICE_UNAVAILABLE as i64)
}
/// Zero-argument OpenResty-compatible NGINX constant.
#[pd_host_function(name = "ngx::http::status_gateway_timeout", contract = crate::host::catalog::ngx_http_status_gateway_timeout_contract)]
pub fn http_status_gateway_timeout() -> VmResult<CallOutcome> {
    ret(nginx_sys::NGX_HTTP_GATEWAY_TIME_OUT as i64)
}
/// Zero-argument OpenResty-compatible NGINX constant.
#[pd_host_function(name = "ngx::http::status_version_not_supported", contract = crate::host::catalog::ngx_http_status_version_not_supported_contract)]
pub fn http_status_version_not_supported() -> VmResult<CallOutcome> {
    ret(nginx_sys::NGX_HTTP_VERSION_NOT_SUPPORTED as i64)
}
/// Zero-argument OpenResty-compatible NGINX constant.
#[pd_host_function(name = "ngx::http::status_insufficient_storage", contract = crate::host::catalog::ngx_http_status_insufficient_storage_contract)]
pub fn http_status_insufficient_storage() -> VmResult<CallOutcome> {
    ret(nginx_sys::NGX_HTTP_INSUFFICIENT_STORAGE as i64)
}

/// Zero-argument OpenResty-compatible NGINX constant.
#[pd_host_function(name = "ngx::log_level::stderr", contract = crate::host::catalog::ngx_log_level_stderr_contract)]
pub fn log_level_stderr() -> VmResult<CallOutcome> {
    ret(nginx_sys::NGX_LOG_STDERR as i64)
}
/// Zero-argument OpenResty-compatible NGINX constant.
#[pd_host_function(name = "ngx::log_level::emerg", contract = crate::host::catalog::ngx_log_level_emerg_contract)]
pub fn log_level_emerg() -> VmResult<CallOutcome> {
    ret(nginx_sys::NGX_LOG_EMERG as i64)
}
/// Zero-argument OpenResty-compatible NGINX constant.
#[pd_host_function(name = "ngx::log_level::alert", contract = crate::host::catalog::ngx_log_level_alert_contract)]
pub fn log_level_alert() -> VmResult<CallOutcome> {
    ret(nginx_sys::NGX_LOG_ALERT as i64)
}
/// Zero-argument OpenResty-compatible NGINX constant.
#[pd_host_function(name = "ngx::log_level::crit", contract = crate::host::catalog::ngx_log_level_crit_contract)]
pub fn log_level_crit() -> VmResult<CallOutcome> {
    ret(nginx_sys::NGX_LOG_CRIT as i64)
}
/// Zero-argument OpenResty-compatible NGINX constant.
#[pd_host_function(name = "ngx::log_level::error", contract = crate::host::catalog::ngx_log_level_error_contract)]
pub fn log_level_error() -> VmResult<CallOutcome> {
    ret(nginx_sys::NGX_LOG_ERR as i64)
}
/// Zero-argument OpenResty-compatible NGINX constant.
#[pd_host_function(name = "ngx::log_level::warn", contract = crate::host::catalog::ngx_log_level_warn_contract)]
pub fn log_level_warn() -> VmResult<CallOutcome> {
    ret(nginx_sys::NGX_LOG_WARN as i64)
}
/// Zero-argument OpenResty-compatible NGINX constant.
#[pd_host_function(name = "ngx::log_level::notice", contract = crate::host::catalog::ngx_log_level_notice_contract)]
pub fn log_level_notice() -> VmResult<CallOutcome> {
    ret(nginx_sys::NGX_LOG_NOTICE as i64)
}
/// Zero-argument OpenResty-compatible NGINX constant.
#[pd_host_function(name = "ngx::log_level::info", contract = crate::host::catalog::ngx_log_level_info_contract)]
pub fn log_level_info() -> VmResult<CallOutcome> {
    ret(nginx_sys::NGX_LOG_INFO as i64)
}
/// Zero-argument OpenResty-compatible NGINX constant.
#[pd_host_function(name = "ngx::log_level::debug", contract = crate::host::catalog::ngx_log_level_debug_contract)]
pub fn log_level_debug() -> VmResult<CallOutcome> {
    ret(nginx_sys::NGX_LOG_DEBUG as i64)
}

/// Current NGINX execution phase as an OpenResty `ngx.get_phase` name.
#[pd_host_function(name = "ngx::get_phase", contract = crate::host::catalog::ngx_get_phase_contract)]
pub fn get_phase() -> VmResult<CallOutcome> {
    match crate::host::current_phase() {
        Ok(phase) => ret(phase_openresty_name(phase)),
        Err(_) => Err(VmError::HostError(
            "ngx::get_phase requires an active NGINX execution scope".into(),
        )),
    }
}

/// OpenResty `ngx.config.subsystem` (`http`).
#[pd_host_function(name = "ngx::config::subsystem", contract = crate::host::catalog::ngx_config_subsystem_contract)]
pub fn config_subsystem() -> VmResult<CallOutcome> {
    ret("http")
}

/// OpenResty `ngx.config.debug`.
#[pd_host_function(name = "ngx::config::debug", contract = crate::host::catalog::ngx_config_debug_contract)]
pub fn config_debug() -> VmResult<CallOutcome> {
    ret(ngx::log::DEBUG)
}

/// OpenResty `ngx.config.prefix`.
#[pd_host_function(name = "ngx::config::prefix", contract = crate::host::catalog::ngx_config_prefix_contract)]
pub fn config_prefix() -> VmResult<CallOutcome> {
    let cycle = live_cycle().map_err(|_| {
        VmError::HostError("ngx::config::prefix requires a live NGINX cycle".into())
    })?;
    ret(owned_ngx_str(cycle.prefix))
}

/// OpenResty `ngx.config.nginx_version`.
#[pd_host_function(name = "ngx::config::nginx_version", contract = crate::host::catalog::ngx_config_nginx_version_contract)]
pub fn config_nginx_version() -> VmResult<CallOutcome> {
    ret(nginx_sys::nginx_version as i64)
}

/// OpenResty `ngx.config.nginx_configure`.
#[pd_host_function(name = "ngx::config::nginx_configure", contract = crate::host::catalog::ngx_config_nginx_configure_contract)]
pub fn config_nginx_configure() -> VmResult<CallOutcome> {
    ret(nginx_sys::NGX_CONFIGURE.to_string_lossy().into_owned())
}

/// Module version analog of `ngx.config.ngx_lua_version`.
#[pd_host_function(name = "ngx::config::ngx_rustscript_version", contract = crate::host::catalog::ngx_config_ngx_rustscript_version_contract)]
pub fn config_ngx_rustscript_version() -> VmResult<CallOutcome> {
    ret(NGX_RUSTSCRIPT_VERSION)
}

/// OpenResty `ngx.worker.exiting`.
#[pd_host_function(name = "ngx::worker::exiting", contract = crate::host::catalog::ngx_worker_exiting_contract)]
pub fn worker_exiting() -> VmResult<CallOutcome> {
    // SAFETY: `ngx_exiting` is a worker-local NGINX global.
    let exiting = unsafe { core::ptr::addr_of!(ngx_exiting).read() };
    ret(exiting != 0)
}

/// OpenResty `ngx.worker.pid`.
#[pd_host_function(name = "ngx::worker::pid", contract = crate::host::catalog::ngx_worker_pid_contract)]
pub fn worker_pid() -> VmResult<CallOutcome> {
    // SAFETY: `ngx_pid` is the current process pid published by NGINX.
    let pid = unsafe { core::ptr::addr_of!(ngx_pid).read() };
    ret(i64::from(pid))
}

/// OpenResty `ngx.worker.pids`.
///
/// Always includes the live `ngx_pid`. Other slots are workers only when
/// `pid > 0` and `channel[0] > 0`, which excludes master, helpers, and stale
/// entries. Invalid `ngx_process_slot` values are ignored. Duplicate pids are
/// collapsed so a mixed table and a `master_process off` worker both stay
/// unique.
#[pd_host_function(name = "ngx::worker::pids", contract = crate::host::catalog::ngx_worker_pids_contract)]
pub fn worker_pids() -> VmResult<CallOutcome> {
    let mut pids = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    // SAFETY: `ngx_pid` is the current process pid published by NGINX.
    let current = unsafe { core::ptr::addr_of!(ngx_pid).read() };
    if current > 0 {
        seen.insert(current);
        pids.push(Value::Int(i64::from(current)));
    }
    // SAFETY: `ngx_processes` is the public NGINX process table.
    let processes = unsafe { &*core::ptr::addr_of!(ngx_processes) };
    for proc in processes {
        if proc.pid > 0 && proc.channel[0] > 0 && seen.insert(proc.pid) {
            pids.push(Value::Int(i64::from(proc.pid)));
        }
    }
    ret(pids)
}

/// OpenResty `ngx.worker.count`.
#[pd_host_function(name = "ngx::worker::count", contract = crate::host::catalog::ngx_worker_count_contract)]
pub fn worker_count() -> VmResult<CallOutcome> {
    let cycle = live_cycle().map_err(|_| {
        VmError::HostError("ngx::worker::count requires a live NGINX cycle".into())
    })?;
    let Some(ccf) = NgxCoreModule::main_conf(cycle) else {
        return Err(VmError::HostError(
            "ngx::worker::count requires NGINX core conf".into(),
        ));
    };
    ret(ccf.worker_processes as i64)
}

/// OpenResty `ngx.worker.id`.
#[pd_host_function(name = "ngx::worker::id", contract = crate::host::catalog::ngx_worker_id_contract)]
pub fn worker_id() -> VmResult<CallOutcome> {
    // SAFETY: `ngx_worker` is the worker ordinal assigned at process init.
    let id = unsafe { core::ptr::addr_of!(ngx_worker).read() };
    ret(id as i64)
}


const META_HOST_MODULE_FUNCTIONS: &[fn() -> vm::HostFunctionDescriptor] = &[
        core_ok_descriptor,
        core_error_descriptor,
        core_again_descriptor,
        core_done_descriptor,
        core_declined_descriptor,
        http_get_descriptor,
        http_head_descriptor,
        http_put_descriptor,
        http_post_descriptor,
        http_delete_descriptor,
        http_options_descriptor,
        http_mkcol_descriptor,
        http_copy_descriptor,
        http_move_descriptor,
        http_propfind_descriptor,
        http_proppatch_descriptor,
        http_lock_descriptor,
        http_unlock_descriptor,
        http_patch_descriptor,
        http_trace_descriptor,
        http_status_continue_descriptor,
        http_status_switching_protocols_descriptor,
        http_status_ok_descriptor,
        http_status_created_descriptor,
        http_status_accepted_descriptor,
        http_status_no_content_descriptor,
        http_status_partial_content_descriptor,
        http_status_special_response_descriptor,
        http_status_moved_permanently_descriptor,
        http_status_moved_temporarily_descriptor,
        http_status_see_other_descriptor,
        http_status_not_modified_descriptor,
        http_status_temporary_redirect_descriptor,
        http_status_permanent_redirect_descriptor,
        http_status_bad_request_descriptor,
        http_status_unauthorized_descriptor,
        http_status_payment_required_descriptor,
        http_status_forbidden_descriptor,
        http_status_not_found_descriptor,
        http_status_not_allowed_descriptor,
        http_status_not_acceptable_descriptor,
        http_status_request_timeout_descriptor,
        http_status_conflict_descriptor,
        http_status_gone_descriptor,
        http_status_upgrade_required_descriptor,
        http_status_too_many_requests_descriptor,
        http_status_illegal_descriptor,
        http_status_close_descriptor,
        http_status_internal_server_error_descriptor,
        http_status_not_implemented_descriptor,
        http_status_bad_gateway_descriptor,
        http_status_service_unavailable_descriptor,
        http_status_gateway_timeout_descriptor,
        http_status_version_not_supported_descriptor,
        http_status_insufficient_storage_descriptor,
        log_level_stderr_descriptor,
        log_level_emerg_descriptor,
        log_level_alert_descriptor,
        log_level_crit_descriptor,
        log_level_error_descriptor,
        log_level_warn_descriptor,
        log_level_notice_descriptor,
        log_level_info_descriptor,
        log_level_debug_descriptor,
        core_null_value_descriptor,
        get_phase_descriptor,
        config_subsystem_descriptor,
        config_debug_descriptor,
        config_prefix_descriptor,
        config_nginx_version_descriptor,
        config_nginx_configure_descriptor,
        config_ngx_rustscript_version_descriptor,
        worker_exiting_descriptor,
        worker_pid_descriptor,
        worker_pids_descriptor,
        worker_count_descriptor,
        worker_id_descriptor,
];

pub fn meta_host_module() -> HostModuleDescriptor {
    HostModuleDescriptor {
        name: "ngx.meta",
        functions: META_HOST_MODULE_FUNCTIONS,
        resources: &[],
    }
}
