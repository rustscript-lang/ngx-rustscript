//! Native NGINX C ABI stubs linked only into `cargo test` binaries.
//!
//! Production `libngx_rustscript.so` leaves these symbols undefined so the
//! nginx process provides them. Allocators and fallible helpers stay
//! fail-closed. Cycle-log tests observe `ngx_log_error_core` through the
//! capture helpers below.

use std::ffi::{c_uint, c_void, CStr};

unsafe extern "C" {
    fn ngx_rustscript_stub_log_reset();
    fn ngx_rustscript_stub_log_count() -> i32;
    fn ngx_rustscript_stub_last_log() -> *const core::ffi::c_char;
    fn ngx_rustscript_test_request_new() -> *mut c_void;
    fn ngx_rustscript_test_request_free(r: *mut c_void);
    fn ngx_rustscript_test_request_sizeof() -> c_uint;
    fn ngx_rustscript_test_request_set_internal(r: *mut c_void, val: c_uint);
    fn ngx_rustscript_test_request_set_filter_finalize(r: *mut c_void, val: c_uint);
    fn ngx_rustscript_test_request_set_error_page(r: *mut c_void, val: c_uint);
    fn ngx_rustscript_test_request_set_header_sent(r: *mut c_void, val: c_uint);
    fn ngx_rustscript_test_request_set_reading_body(r: *mut c_void, val: c_uint);
    fn ngx_rustscript_test_request_set_discard_body(r: *mut c_void, val: c_uint);
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

/// Heap `ngx_http_request_t` allocated and zeroed by C against official headers.
pub struct AbiRequest {
    ptr: *mut c_void,
}

impl AbiRequest {
    pub fn new() -> Self {
        let ptr = unsafe { ngx_rustscript_test_request_new() };
        assert!(
            !ptr.is_null(),
            "C allocator failed to create ngx_http_request_t"
        );
        Self { ptr }
    }

    pub fn as_ptr(&self) -> *mut c_void {
        self.ptr
    }

    pub fn set_internal(&mut self, val: bool) {
        unsafe { ngx_rustscript_test_request_set_internal(self.ptr, u32::from(val)) }
    }

    pub fn set_filter_finalize(&mut self, val: bool) {
        unsafe { ngx_rustscript_test_request_set_filter_finalize(self.ptr, u32::from(val)) }
    }

    pub fn set_error_page(&mut self, val: bool) {
        unsafe { ngx_rustscript_test_request_set_error_page(self.ptr, u32::from(val)) }
    }

    pub fn set_header_sent(&mut self, val: bool) {
        unsafe { ngx_rustscript_test_request_set_header_sent(self.ptr, u32::from(val)) }
    }

    pub fn set_reading_body(&mut self, val: bool) {
        unsafe { ngx_rustscript_test_request_set_reading_body(self.ptr, u32::from(val)) }
    }

    pub fn set_discard_body(&mut self, val: bool) {
        unsafe { ngx_rustscript_test_request_set_discard_body(self.ptr, u32::from(val)) }
    }
}

/// Overlay C `r->internal` onto an existing request allocation.
pub fn write_internal(ptr: *mut c_void, val: bool) {
    unsafe { ngx_rustscript_test_request_set_internal(ptr, u32::from(val)) }
}

/// Overlay C `r->header_sent` onto an existing request allocation.
pub fn write_header_sent(ptr: *mut c_void, val: bool) {
    unsafe { ngx_rustscript_test_request_set_header_sent(ptr, u32::from(val)) }
}

/// Overlay C `r->reading_body` onto an existing request allocation.
pub fn write_reading_body(ptr: *mut c_void, val: bool) {
    unsafe { ngx_rustscript_test_request_set_reading_body(ptr, u32::from(val)) }
}

impl Drop for AbiRequest {
    fn drop(&mut self) {
        unsafe { ngx_rustscript_test_request_free(self.ptr) }
    }
}

/// `sizeof(ngx_http_request_t)` from the configured NGINX C headers.
pub fn request_sizeof() -> u32 {
    unsafe { ngx_rustscript_test_request_sizeof() }
}
