//! Native NGINX C ABI stubs linked only into `cargo test` binaries.
//!
//! Production `libngx_rustscript.so` leaves these symbols undefined so the
//! nginx process provides them. Allocators and fallible helpers stay
//! fail-closed. Cycle-log tests observe `ngx_log_error_core` through the
//! capture helpers below.

use std::ffi::{CStr, c_uint, c_void};

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
    fn ngx_rustscript_test_request_get_internal(r: *mut c_void) -> u32;
    fn ngx_rustscript_test_var_not_found(vv: *const c_void) -> u32;
    fn ngx_rustscript_test_var_valid(vv: *const c_void) -> u32;
    fn ngx_rustscript_test_var_len(vv: *const c_void) -> u32;
    fn ngx_rustscript_test_headers_in_msie(r: *const c_void) -> u32;
    fn ngx_rustscript_test_headers_in_msie6(r: *const c_void) -> u32;
    fn ngx_rustscript_test_headers_in_opera(r: *const c_void) -> u32;
    fn ngx_rustscript_test_headers_in_gecko(r: *const c_void) -> u32;
    fn ngx_rustscript_test_headers_in_chrome(r: *const c_void) -> u32;
    fn ngx_rustscript_test_headers_in_safari(r: *const c_void) -> u32;
    fn ngx_rustscript_test_headers_in_konqueror(r: *const c_void) -> u32;
    fn ngx_rustscript_test_headers_in_connection_type(r: *const c_void) -> u32;
    fn ngx_rustscript_test_headers_in_host(r: *const c_void) -> *mut c_void;
    fn ngx_rustscript_test_headers_in_server_len(r: *const c_void) -> usize;
    fn ngx_rustscript_test_headers_in_server_data(r: *const c_void) -> *mut u8;
    fn ngx_rustscript_test_headers_in_content_length_n(r: *const c_void) -> i64;
    fn ngx_rustscript_test_headers_in_content_length(r: *const c_void) -> *mut c_void;
    fn ngx_rustscript_test_headers_in_user_agent(r: *const c_void) -> *mut c_void;
    fn ngx_rustscript_test_headers_out_location(r: *const c_void) -> *mut c_void;
    fn ngx_rustscript_test_headers_out_status(r: *const c_void) -> usize;
    fn ngx_rustscript_stub_alloc_reset();
    fn ngx_rustscript_stub_alloc_fail_after(n: i32);
    fn ngx_rustscript_stub_alloc_count() -> i32;
    fn ngx_rustscript_stub_send_reset();
    fn ngx_rustscript_stub_send_header_returns(rc: isize);
    fn ngx_rustscript_stub_send_special_returns(rc: isize);
    fn ngx_rustscript_stub_send_header_calls() -> i32;
    fn ngx_rustscript_stub_send_special_calls() -> i32;
    fn ngx_rustscript_stub_send_special_last_flags() -> usize;
    fn ngx_rustscript_stub_output_reset();
    fn ngx_rustscript_stub_output_set_needs_flush_wait(v: u32);
    fn ngx_rustscript_stub_output_set_rc(rc: isize);
    fn ngx_rustscript_stub_set_flush_continue(rc: isize);
    fn ngx_rustscript_stub_output_len() -> usize;
    fn ngx_rustscript_stub_output_body() -> *const u8;
    fn ngx_rustscript_stub_output_saw_last() -> u32;
    fn ngx_rustscript_stub_output_saw_flush() -> u32;
    fn ngx_rustscript_stub_output_saw_last_in_chain() -> u32;
    fn ngx_rustscript_test_request_set_main(r: *mut c_void, main: *mut c_void);
    fn ngx_rustscript_test_request_set_content_length_n(r: *mut c_void, n: i64);
    fn ngx_rustscript_test_request_content_length_n(r: *mut c_void) -> i64;
    fn ngx_rustscript_stub_var_reset();
    fn ngx_rustscript_test_install_variable(
        r: *mut c_void,
        name: *const i8,
        indexed: i32,
        changeable: i32,
        has_handler: i32,
        index: usize,
    ) -> i32;
    fn ngx_rustscript_test_seed_indexed_var(index: usize, data: *mut u8, len: u32, valid: u32);
    fn ngx_rustscript_test_set_handler_calls() -> i32;
    fn ngx_rustscript_test_last_set_vv() -> *mut c_void;
    fn ngx_rustscript_test_last_set_data() -> usize;
    fn ngx_rustscript_test_indexed_var(index: usize) -> *mut c_void;
    fn ngx_rustscript_test_indexed_var_valid(index: usize) -> u32;
    fn ngx_rustscript_test_indexed_var_not_found(index: usize) -> u32;
    fn ngx_rustscript_test_request_set_err_status(r: *mut c_void, status: usize);
    fn ngx_rustscript_test_request_err_status(r: *const c_void) -> usize;
    fn ngx_rustscript_test_request_set_http_version(r: *mut c_void, version: usize);
    fn ngx_rustscript_test_request_set_header_only(r: *mut c_void, val: c_uint);
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UaFlags {
    pub msie: u32,
    pub msie6: u32,
    pub opera: u32,
    pub gecko: u32,
    pub chrome: u32,
    pub safari: u32,
    pub konqueror: u32,
}

pub fn headers_in_ua_flags(ptr: *const c_void) -> UaFlags {
    unsafe {
        UaFlags {
            msie: ngx_rustscript_test_headers_in_msie(ptr),
            msie6: ngx_rustscript_test_headers_in_msie6(ptr),
            opera: ngx_rustscript_test_headers_in_opera(ptr),
            gecko: ngx_rustscript_test_headers_in_gecko(ptr),
            chrome: ngx_rustscript_test_headers_in_chrome(ptr),
            safari: ngx_rustscript_test_headers_in_safari(ptr),
            konqueror: ngx_rustscript_test_headers_in_konqueror(ptr),
        }
    }
}

pub fn headers_in_connection_type(ptr: *const c_void) -> u32 {
    unsafe { ngx_rustscript_test_headers_in_connection_type(ptr) }
}

pub fn headers_in_host(ptr: *const c_void) -> *mut c_void {
    unsafe { ngx_rustscript_test_headers_in_host(ptr) }
}

pub fn headers_in_server(ptr: *const c_void) -> (usize, *mut u8) {
    unsafe {
        (
            ngx_rustscript_test_headers_in_server_len(ptr),
            ngx_rustscript_test_headers_in_server_data(ptr),
        )
    }
}

pub fn headers_in_content_length_n(ptr: *const c_void) -> i64 {
    unsafe { ngx_rustscript_test_headers_in_content_length_n(ptr) }
}

pub fn headers_in_content_length(ptr: *const c_void) -> *mut c_void {
    unsafe { ngx_rustscript_test_headers_in_content_length(ptr) }
}

pub fn headers_in_user_agent(ptr: *const c_void) -> *mut c_void {
    unsafe { ngx_rustscript_test_headers_in_user_agent(ptr) }
}

pub fn headers_out_location(ptr: *const c_void) -> *mut c_void {
    unsafe { ngx_rustscript_test_headers_out_location(ptr) }
}

pub fn headers_out_status(ptr: *const c_void) -> usize {
    unsafe { ngx_rustscript_test_headers_out_status(ptr) }
}

pub fn stub_alloc_reset() {
    unsafe { ngx_rustscript_stub_alloc_reset() }
}

pub fn stub_alloc_fail_after(n: i32) {
    unsafe { ngx_rustscript_stub_alloc_fail_after(n) }
}

pub fn stub_alloc_count() -> i32 {
    unsafe { ngx_rustscript_stub_alloc_count() }
}

pub fn stub_send_reset() {
    unsafe { ngx_rustscript_stub_send_reset() }
}

pub fn stub_send_header_returns(rc: isize) {
    unsafe { ngx_rustscript_stub_send_header_returns(rc) }
}

pub fn stub_send_special_returns(rc: isize) {
    unsafe { ngx_rustscript_stub_send_special_returns(rc) }
}

pub fn stub_send_header_calls() -> i32 {
    unsafe { ngx_rustscript_stub_send_header_calls() }
}

pub fn stub_send_special_calls() -> i32 {
    unsafe { ngx_rustscript_stub_send_special_calls() }
}

pub fn stub_send_special_last_flags() -> usize {
    unsafe { ngx_rustscript_stub_send_special_last_flags() }
}

pub fn stub_output_reset() {
    unsafe { ngx_rustscript_stub_output_reset() }
}

pub fn stub_output_set_needs_flush_wait(v: u32) {
    unsafe { ngx_rustscript_stub_output_set_needs_flush_wait(v) }
}

pub fn stub_output_set_rc(rc: isize) {
    unsafe { ngx_rustscript_stub_output_set_rc(rc) }
}

pub fn stub_set_flush_continue(rc: isize) {
    unsafe { ngx_rustscript_stub_set_flush_continue(rc) }
}

pub fn stub_output_body() -> Vec<u8> {
    unsafe {
        let len = ngx_rustscript_stub_output_len();
        let ptr = ngx_rustscript_stub_output_body();
        if ptr.is_null() || len == 0 {
            Vec::new()
        } else {
            std::slice::from_raw_parts(ptr, len).to_vec()
        }
    }
}

pub fn stub_output_saw_last() -> bool {
    unsafe { ngx_rustscript_stub_output_saw_last() != 0 }
}

pub fn stub_output_saw_flush() -> bool {
    unsafe { ngx_rustscript_stub_output_saw_flush() != 0 }
}

pub fn stub_output_saw_last_in_chain() -> bool {
    unsafe { ngx_rustscript_stub_output_saw_last_in_chain() != 0 }
}

pub fn test_request_set_main(r: *mut c_void, main: *mut c_void) {
    unsafe { ngx_rustscript_test_request_set_main(r, main) }
}

pub fn test_request_set_content_length_n(r: *mut c_void, n: i64) {
    unsafe { ngx_rustscript_test_request_set_content_length_n(r, n) }
}

pub fn test_request_content_length_n(r: *mut c_void) -> i64 {
    unsafe { ngx_rustscript_test_request_content_length_n(r) }
}

pub fn test_request_set_header_only(r: *mut c_void, val: u32) {
    unsafe { ngx_rustscript_test_request_set_header_only(r, val) }
}

pub fn stub_var_reset() {
    unsafe { ngx_rustscript_stub_var_reset() }
}

pub fn test_install_variable(
    r: *mut c_void,
    name: &str,
    indexed: bool,
    changeable: bool,
    has_handler: bool,
    index: usize,
) -> i32 {
    let c_name = std::ffi::CString::new(name).expect("variable name");
    unsafe {
        ngx_rustscript_test_install_variable(
            r,
            c_name.as_ptr(),
            i32::from(indexed),
            i32::from(changeable),
            i32::from(has_handler),
            index,
        )
    }
}

pub fn test_seed_indexed_var(index: usize, data: &mut [u8], valid: bool) {
    unsafe {
        ngx_rustscript_test_seed_indexed_var(
            index,
            data.as_mut_ptr(),
            data.len() as u32,
            u32::from(valid),
        )
    }
}

pub fn test_set_handler_calls() -> i32 {
    unsafe { ngx_rustscript_test_set_handler_calls() }
}

pub fn test_last_set_vv() -> *mut c_void {
    unsafe { ngx_rustscript_test_last_set_vv() }
}

pub fn test_last_set_data() -> usize {
    unsafe { ngx_rustscript_test_last_set_data() }
}

pub fn test_indexed_var(index: usize) -> *mut c_void {
    unsafe { ngx_rustscript_test_indexed_var(index) }
}

pub fn test_indexed_var_valid(index: usize) -> u32 {
    unsafe { ngx_rustscript_test_indexed_var_valid(index) }
}

pub fn test_indexed_var_not_found(index: usize) -> u32 {
    unsafe { ngx_rustscript_test_indexed_var_not_found(index) }
}

pub fn test_set_err_status(r: *mut c_void, status: usize) {
    unsafe { ngx_rustscript_test_request_set_err_status(r, status) }
}

pub fn test_err_status(r: *const c_void) -> usize {
    unsafe { ngx_rustscript_test_request_err_status(r) }
}

pub fn test_set_http_version(r: *mut c_void, version: usize) {
    unsafe { ngx_rustscript_test_request_set_http_version(r, version) }
}

pub fn test_get_internal(r: *mut c_void) -> u32 {
    unsafe { ngx_rustscript_test_request_get_internal(r) }
}

pub fn test_var_valid(vv: *const c_void) -> u32 {
    unsafe { ngx_rustscript_test_var_valid(vv) }
}

pub fn test_var_not_found(vv: *const c_void) -> u32 {
    unsafe { ngx_rustscript_test_var_not_found(vv) }
}

pub fn test_var_len(vv: *const c_void) -> u32 {
    unsafe { ngx_rustscript_test_var_len(vv) }
}
