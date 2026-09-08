//! Native NGINX C ABI stubs linked only into `cargo test` binaries.
//!
//! Production `libngx_rustscript.so` leaves these symbols undefined so the
//! nginx process provides them. Allocators and fallible helpers stay
//! fail-closed. Cycle-log tests observe `ngx_log_error_core` through the
//! capture helpers below.

use std::ffi::CStr;

unsafe extern "C" {
    fn ngx_rustscript_stub_log_reset();
    fn ngx_rustscript_stub_log_count() -> i32;
    fn ngx_rustscript_stub_last_log() -> *const core::ffi::c_char;
}

/// Ensure the static stub archive is pulled into the test link line.
pub fn force_link() {}

/// Clear the captured cycle/request log payload and counter.
pub fn stub_log_reset() {
    unsafe { ngx_rustscript_stub_log_reset() }
}

/// Number of `ngx_log_error_core` calls since the last reset.
pub fn stub_log_count() -> i32 {
    unsafe { ngx_rustscript_stub_log_count() }
}

/// Last `%*s` payload written through `ngx_log_error_core`.
pub fn stub_last_log() -> String {
    unsafe {
        let ptr = ngx_rustscript_stub_last_log();
        if ptr.is_null() {
            String::new()
        } else {
            CStr::from_ptr(ptr).to_string_lossy().into_owned()
        }
    }
}
