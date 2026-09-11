//! Log-phase handler for `log_by_rss_file`.
//!
//! Runs synchronously on the worker main thread after the response is
//! complete. Errors and panics are logged and never mutate the already-sent
//! response, return `NGX_DONE`, increment `r->main->count`, or arm waits.
//! Pooled and request-owned execution share one fail-closed `Waiting` policy.

use std::panic::AssertUnwindSafe;
use std::sync::Arc;

use nginx_sys::ngx_http_request_t;
use ngx::core::Status;
use ngx::ffi::NGX_LOG_ERR;
use ngx::http::{HttpModuleLocationConf, HttpRequestHandler, Request};
use ngx::ngx_log_error;
use vm::Program;

use crate::Module;
use crate::access::{
    allocate_scope_identity, ensure_shared_ngx_ctx, enter_phase_scope, enter_phase_vm,
    next_request_id, run_access_program,
};
use crate::host::NgxPhase;
use crate::request_state::fail_closed_log_status;

const DIRECTIVE: &str = "log_by_rss_file";

pub(crate) struct LogHandler;

impl HttpRequestHandler for LogHandler {
    const PHASE: ngx::http::HttpPhase = ngx::http::HttpPhase::Log;
    type Output = Status;

    fn handler(request: &mut Request) -> Self::Output {
        let raw = request.as_mut() as *mut ngx_http_request_t;
        let result = std::panic::catch_unwind(AssertUnwindSafe(|| run_log_handler(request)));
        match result {
            Ok(status) => status,
            Err(_) => {
                // SAFETY: NGINX keeps the request live for the entire log phase.
                let request = unsafe { Request::from_ngx_http_request(raw) };
                ngx_log_error!(NGX_LOG_ERR, request.log(), "{DIRECTIVE}: panic");
                Status::NGX_OK
            }
        }
    }
}

fn run_log_handler(request: &mut Request) -> Status {
    #[cfg(ngx_rustscript_integration_hooks)]
    maybe_integration_log_panic();
    let Some(conf) = Module::location_conf(request) else {
        return Status::NGX_DECLINED;
    };
    let Some(program) = conf.log_program() else {
        return Status::NGX_DECLINED;
    };
    let status = if !conf.log_metadata().requires_request_state {
        run_pooled_log(request, program)
    } else {
        run_request_owned_log(request, program)
    };
    #[cfg(ngx_rustscript_integration_hooks)]
    log_wait_proof(request);
    status
}

fn run_pooled_log(request: &mut Request, program: &Arc<Program>) -> Status {
    let raw = request.as_mut() as *mut ngx_http_request_t;
    let identity = allocate_scope_identity();
    enter_phase_scope(raw, identity, NgxPhase::Log, || {
        enter_phase_vm(identity, NgxPhase::Log, || {
            if let Err(err) = fail_closed_log_status(run_access_program(program)) {
                ngx_log_error!(NGX_LOG_ERR, request.log(), "{DIRECTIVE}: {err}");
            }
            Status::NGX_OK
        })
    })
}

fn run_request_owned_log(request: &mut Request, program: &Arc<Program>) -> Status {
    let raw = request.as_mut() as *mut ngx_http_request_t;
    let ctx_ptr = match ensure_shared_ngx_ctx(request) {
        Ok(ptr) => ptr,
        Err(err) => {
            ngx_log_error!(NGX_LOG_ERR, request.log(), "{DIRECTIVE}: {err}");
            return Status::NGX_OK;
        }
    };
    let request_id = next_request_id();
    let mut exec = match crate::runtime::with_worker(|worker| {
        worker.bind_phase_exec(Arc::clone(program), request_id, NgxPhase::Log)
    }) {
        Some(Ok(exec)) => exec,
        Some(Err(err)) => {
            ngx_log_error!(NGX_LOG_ERR, request.log(), "{DIRECTIVE}: {err}");
            return Status::NGX_OK;
        }
        None => {
            ngx_log_error!(
                NGX_LOG_ERR,
                request.log(),
                "{DIRECTIVE}: worker runtime is not started"
            );
            return Status::NGX_OK;
        }
    };
    exec.attach_request_ctx_ptr(ctx_ptr);
    let identity = exec.identity();
    let result = enter_phase_scope(raw, identity, NgxPhase::Log, || exec.run_sync_fail_closed());
    if let Err(err) = result {
        ngx_log_error!(NGX_LOG_ERR, request.log(), "{DIRECTIVE}: {err}");
    }
    let _ = exec.cleanup();
    Status::NGX_OK
}

#[cfg(ngx_rustscript_integration_hooks)]
fn log_wait_proof(request: &Request) {
    if std::env::var_os("NGX_RUSTSCRIPT_LOG_WAIT_PROOF").is_none() {
        return;
    }
    let (eventfd, count, timer) = crate::request_state::wait_arm_snapshot();
    ngx_log_error!(
        NGX_LOG_ERR,
        request.log(),
        "LOG_WAIT_PROOF eventfd={eventfd} count={count} timer={timer}"
    );
}

#[cfg(ngx_rustscript_integration_hooks)]
fn maybe_integration_log_panic() {
    if std::env::var_os("NGX_RUSTSCRIPT_TEST_LOG_PANIC").as_deref()
        == Some(std::ffi::OsStr::new("1"))
    {
        panic!("ngx-rustscript integration log panic");
    }
}

#[cfg(test)]
mod tests {
    use vm::{VmError, VmStatus};

    use crate::config::compile_rss_source;
    use crate::host::NgxPhase;
    use crate::request_state::{LOG_CANNOT_WAIT, fail_closed_log_status};
    use crate::runtime::WorkerRuntime;

    #[test]
    fn fail_closed_log_status_maps_waiting_to_exact_error() {
        let waiting = fail_closed_log_status(Ok(VmStatus::Waiting(0))).expect_err("waiting");
        assert_eq!(waiting, LOG_CANNOT_WAIT);
        assert!(fail_closed_log_status(Ok(VmStatus::Halted)).is_ok());
        let yielded = fail_closed_log_status(Ok(VmStatus::Yielded)).expect_err("yielded");
        assert_eq!(yielded, "log handler cannot yield");
        let host =
            fail_closed_log_status(Err(VmError::HostError("boom".into()))).expect_err("host");
        assert_eq!(host, "host error: boom");
    }

    #[test]
    fn log_waiting_host_fails_closed_without_arming() {
        let mut worker = WorkerRuntime::start().unwrap();
        let program = compile_rss_source("use ngx;\nngx::sleep_ms(50);\n").expect("compile sleep");
        let mut exec = worker
            .bind_phase_exec(program, 42, NgxPhase::Log)
            .expect("bind log");
        let err = exec.run_sync_fail_closed().expect_err("must fail closed");
        assert_eq!(err, LOG_CANNOT_WAIT);
        assert!(!exec.wait_permit_armed());
        assert!(!exec.vm_is_live());
        assert!(exec.is_finished());
        worker.shutdown();
    }

    #[test]
    fn log_halted_ignores_disposition_and_does_not_arm() {
        let mut worker = WorkerRuntime::start().unwrap();
        let program = compile_rss_source("let x = 1 + 2;\n").expect("compile log");
        let mut exec = worker
            .bind_phase_exec(program, 7, NgxPhase::Log)
            .expect("bind log");
        exec.run_sync_fail_closed().expect("halted log");
        assert!(!exec.wait_permit_armed());
        assert!(!exec.vm_is_live());
        assert!(exec.is_finished());
        worker.shutdown();
    }
}
