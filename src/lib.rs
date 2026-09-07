#![forbid(unsafe_op_in_unsafe_fn)]

use ngx::core::Status;
use ngx::ffi::{NGX_HTTP_MODULE, ngx_conf_t, ngx_http_module_t, ngx_int_t, ngx_module_t};
use ngx::http::{self, HttpModule, HttpModuleLocationConf};

pub mod access;
pub mod config;
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
        http::add_phase_handler::<access::AccessHandler>(cf)
            .map_or(Status::NGX_ERROR, |_| Status::NGX_OK)
            .into()
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

/// Module-specific identity OBJECT and source-freshness fingerprint.
///
/// ELF type `OBJECT`, bind `GLOBAL`, exact size 32, in dedicated section
/// `.ngx_rustscript_module_identity`. Tests copy the candidate `.so` to a
/// private snapshot, then require GNU `readelf -W --dyn-syms` type/bind/size
/// and `objcopy --dump-section` of that unique section (exactly 32 bytes)
/// before comparing the dump to a recomputed fingerprint. Dynsym `Ndx` must
/// name that unique section and the symbol address range must fit inside it.
/// This is an identity/freshness fingerprint, not a cryptographic MAC.
#[used]
#[allow(non_upper_case_globals)]
#[unsafe(no_mangle)]
#[unsafe(link_section = ".ngx_rustscript_module_identity")]
pub static ngx_rustscript_module_identity: [u8; 32] = *include_bytes!(concat!(
    env!("OUT_DIR"),
    "/ngx_rustscript_source_fingerprint.bin"
));

#[used]
#[allow(non_upper_case_globals)]
#[cfg_attr(not(feature = "export-modules"), unsafe(no_mangle))]
pub static mut ngx_http_rustscript_module: ngx_module_t = ngx_module_t {
    ctx: &raw const NGX_HTTP_RUSTSCRIPT_MODULE_CTX as _,
    commands: unsafe { &raw mut config::COMMANDS[0] },
    type_: NGX_HTTP_MODULE as _,
    ..ngx_module_t::default()
};
