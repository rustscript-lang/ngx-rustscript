//! Content-phase handler for `content_by_rss_file`.

use std::panic::AssertUnwindSafe;

use nginx_sys::{ngx_conf_t, ngx_http_request_t, ngx_int_t};
use ngx::core::Status;
use ngx::http::{HttpModuleLocationConf, NgxHttpCoreModule, Request};

use crate::access;
use crate::phase::PhaseSlot;

/// Install the RustScript content handler on the core location conf.
///
/// Called from `merge_loc_conf` when a `content_by_rss_file` program is present.
/// Returns `Err` when the handler cannot be installed so configured content
/// cannot fail open onto NGINX's static handler.
pub(crate) fn install_content_handler(cf: *mut ngx_conf_t) -> Result<(), ()> {
    if cf.is_null() {
        return Err(());
    }
    // SAFETY: `cf` is the live NGINX conf pointer passed to `merge_loc_conf`.
    let cf = unsafe { &*cf };
    let Some(clcf) = NgxHttpCoreModule::location_conf_mut(cf) else {
        return Err(());
    };
    clcf.handler = Some(content_handler);
    Ok(())
}

/// Install the content handler only when this location compiled a content program.
pub(crate) fn install_configured_content(
    cf: *mut ngx_conf_t,
    loc: &crate::config::LocationConf,
) -> Result<(), ()> {
    if loc.content_program().is_none() {
        return Ok(());
    }
    install_content_handler(cf)
}

unsafe extern "C" fn content_handler(r: *mut ngx_http_request_t) -> ngx_int_t {
    let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
        if r.is_null() {
            return Status::NGX_ERROR;
        }
        // SAFETY: NGINX calls the location content handler with a live request.
        let request = unsafe { Request::from_ngx_http_request(r) };
        access::run_phase_handler(request, PhaseSlot::Content)
    }));
    match result {
        Ok(status) => status.into(),
        Err(_) => Status::NGX_ERROR.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{LocationConf, compile_rss_source_for_phase};
    use crate::host::NgxPhase;

    #[test]
    fn configured_content_install_fails_closed_on_null_cf() {
        let program =
            compile_rss_source_for_phase("use ngx;\nngx::get_phase();\n", NgxPhase::Content)
                .expect("compile content program");
        let mut loc = LocationConf::default();
        loc.set_content_program(program).unwrap();
        assert!(
            install_configured_content(core::ptr::null_mut(), &loc).is_err(),
            "configured content must not fail open when the core loc conf is missing"
        );
    }

    #[test]
    fn unconfigured_content_does_not_install_on_null_cf() {
        let loc = LocationConf::default();
        assert!(install_configured_content(core::ptr::null_mut(), &loc).is_ok());
    }
}
