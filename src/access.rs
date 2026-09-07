//! Synchronous access-phase VM execution. No polling, Tokio, eventfd, or sleep.

use std::sync::Arc;

use ngx::core::Status;
use ngx::ffi::NGX_LOG_ERR;
use ngx::http::{HTTPStatus, HttpModuleLocationConf, HttpRequestHandler, Request};
use ngx::ngx_log_error;
use vm::{Program, VmError, VmStatus};

use crate::Module;
use crate::config::LocationConf;
use crate::vm_pool;

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
        let Some(conf) = Module::location_conf(request) else {
            return Status::NGX_DECLINED;
        };
        run_configured_access(request, conf)
    }
}

fn run_configured_access(request: &Request, conf: &LocationConf) -> Status {
    let Some(program) = conf.program() else {
        return Status::NGX_DECLINED;
    };
    match run_access_program(program) {
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
    }
}
