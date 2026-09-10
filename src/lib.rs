#![forbid(unsafe_op_in_unsafe_fn)]

use ngx::core::{NGX_CONF_ERROR, Status};
use ngx::ffi::{NGX_HTTP_MODULE, ngx_conf_t, ngx_http_module_t, ngx_int_t, ngx_module_t};
use ngx::http::{self, HttpModule, HttpModuleLocationConf, Merge};

pub mod access;
pub mod async_bridge;
pub mod config;
pub mod content;
pub mod host;
pub mod log;
pub mod nginx_op;
pub mod notify;
pub mod phase;
pub mod read_close;
pub mod request_state;
pub mod runtime;
pub mod shared_dict;
pub(crate) mod socket;
pub(crate) mod vm_pool;

#[cfg(test)]
use ngx_rustscript_nginx_test_stubs as _;

pub(crate) struct Module;

impl HttpModule for Module {
    fn module() -> &'static ngx_module_t {
        // SAFETY: `ngx_http_rustscript_module` is the process-lifetime module object
        // exported to NGINX. Taking a shared reference is the SDK `HttpModule::module` contract.
        unsafe { &*::core::ptr::addr_of!(ngx_http_rustscript_module) }
    }

    unsafe extern "C" fn postconfiguration(cf: *mut ngx_conf_t) -> ngx_int_t {
        // SAFETY: NGINX always calls postconfiguration with a live `ngx_conf_t`.
        let cf = unsafe { &mut *cf };
        if http::add_phase_handler::<access::AccessHandler>(cf).is_err() {
            return Status::NGX_ERROR.into();
        }
        if http::add_phase_handler::<log::LogHandler>(cf).is_err() {
            return Status::NGX_ERROR.into();
        }
        Status::NGX_OK.into()
    }

    unsafe extern "C" fn merge_loc_conf(
        cf: *mut ngx_conf_t,
        prev: *mut core::ffi::c_void,
        conf: *mut core::ffi::c_void,
    ) -> *mut core::ffi::c_char {
        // SAFETY: NGINX merge_loc_conf always passes live parent/child location confs.
        let prev = unsafe { &*prev.cast::<config::LocationConf>() };
        let loc = unsafe { &mut *conf.cast::<config::LocationConf>() };
        if loc.merge(prev).is_err() {
            return NGX_CONF_ERROR;
        }
        if content::install_configured_content(cf, loc).is_err() {
            return NGX_CONF_ERROR;
        }
        core::ptr::null_mut()
    }
}

unsafe impl HttpModuleLocationConf for Module {
    type LocationConf = config::LocationConf;
}

static NGX_HTTP_RUSTSCRIPT_MODULE_CTX: ngx_http_module_t = ngx_http_module_t {
    preconfiguration: Some(Module::preconfiguration),
    postconfiguration: Some(Module::postconfiguration),
    create_main_conf: None,
    init_main_conf: None,
    create_srv_conf: None,
    merge_srv_conf: None,
    create_loc_conf: Some(Module::create_loc_conf),
    merge_loc_conf: Some(Module::merge_loc_conf),
};

#[cfg(feature = "export-modules")]
ngx::ngx_modules!(ngx_http_rustscript_module);

#[used]
#[allow(non_upper_case_globals)]
#[cfg_attr(not(feature = "export-modules"), unsafe(no_mangle))]
pub static mut ngx_http_rustscript_module: ngx_module_t = ngx_module_t {
    ctx: &raw const NGX_HTTP_RUSTSCRIPT_MODULE_CTX as _,
    commands: unsafe { &raw mut config::COMMANDS[0] },
    type_: NGX_HTTP_MODULE as _,
    init_process: Some(runtime::init_process),
    exit_process: Some(runtime::exit_process),
    ..ngx_module_t::default()
};
