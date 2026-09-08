// Worker, build, phase, and constant-family metadata hosts.
//
// Values returned to the VM are owned. Live worker fields are read from
// public NGINX globals on this worker thread; they are never sourced from
// `/proc`, the environment, or hardcoded process identity.

use nginx_sys::{ngx_cycle, ngx_exiting, ngx_pid, ngx_processes, ngx_str_t, ngx_worker};
use ngx::core::{CoreModuleMainConf, NgxCoreModule};
use pd_host_function::pd_host_function;
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

macro_rules! ngx_int_const {
    ($fn_name:ident, $host:literal, $value:expr) => {
        /// Zero-argument OpenResty-compatible NGINX constant.
        #[pd_host_function(name = $host)]
        pub fn $fn_name() -> VmResult<CallOutcome> {
            ret($value as i64)
        }
    };
}

ngx_int_const!(core_ok, "ngx::core::ok", nginx_sys::NGX_OK);
ngx_int_const!(core_error, "ngx::core::error", nginx_sys::NGX_ERROR);
ngx_int_const!(core_again, "ngx::core::again", nginx_sys::NGX_AGAIN);
ngx_int_const!(core_done, "ngx::core::done", nginx_sys::NGX_DONE);
ngx_int_const!(core_declined, "ngx::core::declined", nginx_sys::NGX_DECLINED);

/// OpenResty `ngx.null`. Named `null_value` because `null` is a reserved token.
#[pd_host_function(name = "ngx::core::null_value")]
pub fn core_null_value() -> VmResult<CallOutcome> {
    ret(())
}

ngx_int_const!(http_get, "ngx::http::get", nginx_sys::NGX_HTTP_GET);
ngx_int_const!(http_head, "ngx::http::head", nginx_sys::NGX_HTTP_HEAD);
ngx_int_const!(http_put, "ngx::http::put", nginx_sys::NGX_HTTP_PUT);
ngx_int_const!(http_post, "ngx::http::post", nginx_sys::NGX_HTTP_POST);
ngx_int_const!(http_delete, "ngx::http::delete", nginx_sys::NGX_HTTP_DELETE);
ngx_int_const!(http_options, "ngx::http::options", nginx_sys::NGX_HTTP_OPTIONS);
ngx_int_const!(http_mkcol, "ngx::http::mkcol", nginx_sys::NGX_HTTP_MKCOL);
ngx_int_const!(http_copy, "ngx::http::copy", nginx_sys::NGX_HTTP_COPY);
ngx_int_const!(http_move, "ngx::http::move", nginx_sys::NGX_HTTP_MOVE);
ngx_int_const!(http_propfind, "ngx::http::propfind", nginx_sys::NGX_HTTP_PROPFIND);
ngx_int_const!(http_proppatch, "ngx::http::proppatch", nginx_sys::NGX_HTTP_PROPPATCH);
ngx_int_const!(http_lock, "ngx::http::lock", nginx_sys::NGX_HTTP_LOCK);
ngx_int_const!(http_unlock, "ngx::http::unlock", nginx_sys::NGX_HTTP_UNLOCK);
ngx_int_const!(http_patch, "ngx::http::patch", nginx_sys::NGX_HTTP_PATCH);
ngx_int_const!(http_trace, "ngx::http::trace", nginx_sys::NGX_HTTP_TRACE);

ngx_int_const!(
    http_status_continue,
    "ngx::http::status_continue",
    nginx_sys::NGX_HTTP_CONTINUE
);
ngx_int_const!(
    http_status_switching_protocols,
    "ngx::http::status_switching_protocols",
    nginx_sys::NGX_HTTP_SWITCHING_PROTOCOLS
);
ngx_int_const!(http_status_ok, "ngx::http::status_ok", nginx_sys::NGX_HTTP_OK);
ngx_int_const!(
    http_status_created,
    "ngx::http::status_created",
    nginx_sys::NGX_HTTP_CREATED
);
ngx_int_const!(
    http_status_accepted,
    "ngx::http::status_accepted",
    nginx_sys::NGX_HTTP_ACCEPTED
);
ngx_int_const!(
    http_status_no_content,
    "ngx::http::status_no_content",
    nginx_sys::NGX_HTTP_NO_CONTENT
);
ngx_int_const!(
    http_status_partial_content,
    "ngx::http::status_partial_content",
    nginx_sys::NGX_HTTP_PARTIAL_CONTENT
);
ngx_int_const!(
    http_status_special_response,
    "ngx::http::status_special_response",
    nginx_sys::NGX_HTTP_SPECIAL_RESPONSE
);
ngx_int_const!(
    http_status_moved_permanently,
    "ngx::http::status_moved_permanently",
    nginx_sys::NGX_HTTP_MOVED_PERMANENTLY
);
ngx_int_const!(
    http_status_moved_temporarily,
    "ngx::http::status_moved_temporarily",
    nginx_sys::NGX_HTTP_MOVED_TEMPORARILY
);
ngx_int_const!(
    http_status_see_other,
    "ngx::http::status_see_other",
    nginx_sys::NGX_HTTP_SEE_OTHER
);
ngx_int_const!(
    http_status_not_modified,
    "ngx::http::status_not_modified",
    nginx_sys::NGX_HTTP_NOT_MODIFIED
);
ngx_int_const!(
    http_status_temporary_redirect,
    "ngx::http::status_temporary_redirect",
    nginx_sys::NGX_HTTP_TEMPORARY_REDIRECT
);
ngx_int_const!(
    http_status_permanent_redirect,
    "ngx::http::status_permanent_redirect",
    nginx_sys::NGX_HTTP_PERMANENT_REDIRECT
);
ngx_int_const!(
    http_status_bad_request,
    "ngx::http::status_bad_request",
    nginx_sys::NGX_HTTP_BAD_REQUEST
);
ngx_int_const!(
    http_status_unauthorized,
    "ngx::http::status_unauthorized",
    nginx_sys::NGX_HTTP_UNAUTHORIZED
);
ngx_int_const!(
    http_status_payment_required,
    "ngx::http::status_payment_required",
    402
);
ngx_int_const!(
    http_status_forbidden,
    "ngx::http::status_forbidden",
    nginx_sys::NGX_HTTP_FORBIDDEN
);
ngx_int_const!(
    http_status_not_found,
    "ngx::http::status_not_found",
    nginx_sys::NGX_HTTP_NOT_FOUND
);
ngx_int_const!(
    http_status_not_allowed,
    "ngx::http::status_not_allowed",
    nginx_sys::NGX_HTTP_NOT_ALLOWED
);
ngx_int_const!(
    http_status_not_acceptable,
    "ngx::http::status_not_acceptable",
    406
);
ngx_int_const!(
    http_status_request_timeout,
    "ngx::http::status_request_timeout",
    nginx_sys::NGX_HTTP_REQUEST_TIME_OUT
);
ngx_int_const!(
    http_status_conflict,
    "ngx::http::status_conflict",
    nginx_sys::NGX_HTTP_CONFLICT
);
ngx_int_const!(http_status_gone, "ngx::http::status_gone", 410);
ngx_int_const!(
    http_status_upgrade_required,
    "ngx::http::status_upgrade_required",
    426
);
ngx_int_const!(
    http_status_too_many_requests,
    "ngx::http::status_too_many_requests",
    nginx_sys::NGX_HTTP_TOO_MANY_REQUESTS
);
ngx_int_const!(http_status_illegal, "ngx::http::status_illegal", 451);
ngx_int_const!(
    http_status_close,
    "ngx::http::status_close",
    nginx_sys::NGX_HTTP_CLOSE
);
ngx_int_const!(
    http_status_internal_server_error,
    "ngx::http::status_internal_server_error",
    nginx_sys::NGX_HTTP_INTERNAL_SERVER_ERROR
);
ngx_int_const!(
    http_status_not_implemented,
    "ngx::http::status_not_implemented",
    nginx_sys::NGX_HTTP_NOT_IMPLEMENTED
);
ngx_int_const!(
    http_status_bad_gateway,
    "ngx::http::status_bad_gateway",
    nginx_sys::NGX_HTTP_BAD_GATEWAY
);
ngx_int_const!(
    http_status_service_unavailable,
    "ngx::http::status_service_unavailable",
    nginx_sys::NGX_HTTP_SERVICE_UNAVAILABLE
);
ngx_int_const!(
    http_status_gateway_timeout,
    "ngx::http::status_gateway_timeout",
    nginx_sys::NGX_HTTP_GATEWAY_TIME_OUT
);
ngx_int_const!(
    http_status_version_not_supported,
    "ngx::http::status_version_not_supported",
    nginx_sys::NGX_HTTP_VERSION_NOT_SUPPORTED
);
ngx_int_const!(
    http_status_insufficient_storage,
    "ngx::http::status_insufficient_storage",
    nginx_sys::NGX_HTTP_INSUFFICIENT_STORAGE
);

ngx_int_const!(log_level_stderr, "ngx::log_level::stderr", nginx_sys::NGX_LOG_STDERR);
ngx_int_const!(log_level_emerg, "ngx::log_level::emerg", nginx_sys::NGX_LOG_EMERG);
ngx_int_const!(log_level_alert, "ngx::log_level::alert", nginx_sys::NGX_LOG_ALERT);
ngx_int_const!(log_level_crit, "ngx::log_level::crit", nginx_sys::NGX_LOG_CRIT);
ngx_int_const!(log_level_error, "ngx::log_level::error", nginx_sys::NGX_LOG_ERR);
ngx_int_const!(log_level_warn, "ngx::log_level::warn", nginx_sys::NGX_LOG_WARN);
ngx_int_const!(log_level_notice, "ngx::log_level::notice", nginx_sys::NGX_LOG_NOTICE);
ngx_int_const!(log_level_info, "ngx::log_level::info", nginx_sys::NGX_LOG_INFO);
ngx_int_const!(log_level_debug, "ngx::log_level::debug", nginx_sys::NGX_LOG_DEBUG);

/// Current NGINX execution phase as an OpenResty `ngx.get_phase` name.
#[pd_host_function(name = "ngx::get_phase")]
pub fn get_phase() -> VmResult<CallOutcome> {
    match crate::host::current_phase() {
        Ok(phase) => ret(phase_openresty_name(phase)),
        Err(_) => Err(VmError::HostError(
            "ngx::get_phase requires an active NGINX execution scope".into(),
        )),
    }
}

/// OpenResty `ngx.config.subsystem` (`http`).
#[pd_host_function(name = "ngx::config::subsystem")]
pub fn config_subsystem() -> VmResult<CallOutcome> {
    ret("http")
}

/// OpenResty `ngx.config.debug`.
#[pd_host_function(name = "ngx::config::debug")]
pub fn config_debug() -> VmResult<CallOutcome> {
    ret(ngx::log::DEBUG)
}

/// OpenResty `ngx.config.prefix`.
#[pd_host_function(name = "ngx::config::prefix")]
pub fn config_prefix() -> VmResult<CallOutcome> {
    let cycle = live_cycle().map_err(|_| {
        VmError::HostError("ngx::config::prefix requires a live NGINX cycle".into())
    })?;
    ret(owned_ngx_str(cycle.prefix))
}

/// OpenResty `ngx.config.nginx_version`.
#[pd_host_function(name = "ngx::config::nginx_version")]
pub fn config_nginx_version() -> VmResult<CallOutcome> {
    ret(nginx_sys::nginx_version as i64)
}

/// OpenResty `ngx.config.nginx_configure`.
#[pd_host_function(name = "ngx::config::nginx_configure")]
pub fn config_nginx_configure() -> VmResult<CallOutcome> {
    ret(nginx_sys::NGX_CONFIGURE.to_string_lossy().into_owned())
}

/// Module version analog of `ngx.config.ngx_lua_version`.
#[pd_host_function(name = "ngx::config::ngx_rustscript_version")]
pub fn config_ngx_rustscript_version() -> VmResult<CallOutcome> {
    ret(NGX_RUSTSCRIPT_VERSION)
}

/// OpenResty `ngx.worker.exiting`.
#[pd_host_function(name = "ngx::worker::exiting")]
pub fn worker_exiting() -> VmResult<CallOutcome> {
    // SAFETY: `ngx_exiting` is a worker-local NGINX global.
    let exiting = unsafe { core::ptr::addr_of!(ngx_exiting).read() };
    ret(exiting != 0)
}

/// OpenResty `ngx.worker.pid`.
#[pd_host_function(name = "ngx::worker::pid")]
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
#[pd_host_function(name = "ngx::worker::pids")]
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
#[pd_host_function(name = "ngx::worker::count")]
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
#[pd_host_function(name = "ngx::worker::id")]
pub fn worker_id() -> VmResult<CallOutcome> {
    // SAFETY: `ngx_worker` is the worker ordinal assigned at process init.
    let id = unsafe { core::ptr::addr_of!(ngx_worker).read() };
    ret(id as i64)
}
