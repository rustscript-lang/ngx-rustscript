//! Task 7 review hardening: special headers, transactionality, ABI, send, control.
//!
//! Tests go through bound hosts and C ABI helpers. Source greps are secondary.

use ngx_rustscript_nginx_test_stubs as nginx_stubs;

use std::mem::MaybeUninit;
use std::sync::Arc;

use nginx_sys::{
    ngx_http_request_t, ngx_int_t, ngx_list_part_t, ngx_str_t, ngx_table_elt_t, ngx_uint_t,
};
use ngx_rustscript::config::compile_rss_source;
use ngx_rustscript::host::{
    AllowedOps, NgxPhase, RequestDisposition, ScopeError, bind_ngx_hosts,
    with_current_disposition_mut, with_current_request_mut, with_request_scope,
    with_request_scope_full,
};
use ngx_rustscript::notify::RequestIdentity;
use vm::{Program, Value, Vm, VmStatus};

const NGX_HTTP_CONNECTION_CLOSE: u32 = 1;
const NGX_HTTP_CONNECTION_KEEP_ALIVE: u32 = 2;

unsafe extern "C" {
    fn ngx_rustscript_c_send_last_response(
        r: *mut ngx_http_request_t,
        status: ngx_int_t,
    ) -> ngx_int_t;
    fn ngx_rustscript_c_variable_set(
        r: *mut ngx_http_request_t,
        name: *mut u8,
        name_len: usize,
        value: *mut u8,
        value_len: usize,
        is_null: i32,
    ) -> i32;
}

fn compile(source: &str) -> Arc<Program> {
    compile_rss_source(source).unwrap_or_else(|err| panic!("compile failed: {err}\n{source}"))
}

fn dummy_request() -> Box<ngx_http_request_t> {
    let mut req = Box::new(unsafe { MaybeUninit::<ngx_http_request_t>::zeroed().assume_init() });
    let ptr = (&mut *req) as *mut ngx_http_request_t;
    req.main = ptr;
    req
}

fn ngx_str(bytes: &mut [u8]) -> ngx_str_t {
    ngx_str_t {
        len: bytes.len(),
        data: bytes.as_mut_ptr(),
    }
}

fn header_elt(
    key: &mut [u8],
    lowcase: &mut [u8],
    value: &mut [u8],
    hash: usize,
) -> ngx_table_elt_t {
    ngx_table_elt_t {
        hash,
        key: ngx_str(key),
        value: ngx_str(value),
        lowcase_key: lowcase.as_mut_ptr(),
        next: std::ptr::null_mut(),
    }
}

fn enable_alloc(budget: i32) {
    nginx_stubs::stub_alloc_reset();
    nginx_stubs::stub_alloc_fail_after(budget);
}

fn writable_list(req: &mut ngx_http_request_t, storage: &mut [ngx_table_elt_t], nelts: usize) {
    req.headers_in.headers.size = std::mem::size_of::<ngx_table_elt_t>();
    req.headers_in.headers.nalloc = storage.len();
    req.headers_in.headers.part.elts = storage.as_mut_ptr().cast();
    req.headers_in.headers.part.nelts = nelts;
    req.headers_in.headers.part.next = std::ptr::null_mut();
    req.headers_in.headers.last = &mut req.headers_in.headers.part;
    req.pool = std::ptr::dangling_mut();
}

fn writable_out_list(req: &mut ngx_http_request_t, storage: &mut [ngx_table_elt_t], nelts: usize) {
    req.headers_out.headers.size = std::mem::size_of::<ngx_table_elt_t>();
    req.headers_out.headers.nalloc = storage.len();
    req.headers_out.headers.part.elts = storage.as_mut_ptr().cast();
    req.headers_out.headers.part.nelts = nelts;
    req.headers_out.headers.part.next = std::ptr::null_mut();
    req.headers_out.headers.last = &mut req.headers_out.headers.part;
    req.pool = std::ptr::dangling_mut();
}

fn eval_in_request_ops(
    request: *mut ngx_http_request_t,
    ops: AllowedOps,
    source: &str,
) -> Result<Value, String> {
    let program = compile(source);
    let mut vm = Vm::try_new((*program).clone()).expect("vm");
    bind_ngx_hosts(&mut vm);
    with_request_scope(
        request,
        RequestIdentity::new(1, 1),
        NgxPhase::Access,
        ops,
        || match vm.run() {
            Ok(VmStatus::Halted) => Ok(vm.stack().last().cloned().unwrap_or(Value::Null)),
            Ok(other) => panic!("unexpected vm status {other:?}"),
            Err(err) => Err(err.to_string()),
        },
    )
}

fn write_ops() -> AllowedOps {
    AllowedOps::REQUEST_READ
        .union(AllowedOps::REQUEST_WRITE)
        .union(AllowedOps::RESPONSE_WRITE)
        .union(AllowedOps::CONTROL)
}

fn eval_write(request: *mut ngx_http_request_t, source: &str) -> Result<Value, String> {
    eval_in_request_ops(request, write_ops(), source)
}

fn server_text(ptr: *const ngx_http_request_t) -> String {
    let (len, data) = nginx_stubs::headers_in_server(ptr.cast());
    if data.is_null() || len == 0 {
        String::new()
    } else {
        unsafe { String::from_utf8_lossy(std::slice::from_raw_parts(data, len)).into_owned() }
    }
}

fn is_host_err(err: &str) -> bool {
    let lower = err.to_ascii_lowercase();
    lower.contains("host")
        || lower.contains("header")
        || lower.contains("malformed")
        || lower.contains("invalid")
        || lower.contains("cycle")
}

#[test]
fn invalid_host_is_rejected_before_list_mutation() {
    enable_alloc(64);
    let mut req = dummy_request();
    let mut storage = [unsafe { MaybeUninit::<ngx_table_elt_t>::zeroed().assume_init() }; 8];
    let mut old_key = b"X-Keep".to_vec();
    let mut old_low = b"x-keep".to_vec();
    let mut old_val = b"yes".to_vec();
    storage[0] = header_elt(&mut old_key, &mut old_low, &mut old_val, 1);
    writable_list(&mut req, &mut storage, 1);
    let ptr = req.as_mut() as *mut ngx_http_request_t;
    let err = eval_write(
        ptr,
        "use ngx;\nngx::req::set_header(\"Host\", \"bad..example.com\");\n",
    )
    .expect_err("invalid host must fail");
    assert!(
        is_host_err(&err),
        "expected host validation error, got {err}"
    );
    assert_eq!(req.headers_in.headers.part.nelts, 1);
    assert!(nginx_stubs::headers_in_host(ptr.cast()).is_null());
    assert!(server_text(ptr).is_empty());
}

#[test]
fn host_with_port_normalizes_server_and_invalidates_cached_host() {
    enable_alloc(64);
    nginx_stubs::stub_var_reset();
    let mut req = dummy_request();
    let mut storage = [unsafe { MaybeUninit::<ngx_table_elt_t>::zeroed().assume_init() }; 8];
    writable_list(&mut req, &mut storage, 0);
    let ptr = req.as_mut() as *mut ngx_http_request_t;
    assert_eq!(
        nginx_stubs::test_install_variable(ptr.cast(), "host", true, true, false, 0),
        0
    );
    let mut cached = b"old.example".to_vec();
    nginx_stubs::test_seed_indexed_var(0, &mut cached, true);
    eval_write(
        ptr,
        "use ngx;\nngx::req::set_header(\"Host\", \"Example.COM:8080\");\n",
    )
    .expect("valid host");
    assert!(!nginx_stubs::headers_in_host(ptr.cast()).is_null());
    assert_eq!(server_text(ptr), "example.com");
    assert_eq!(nginx_stubs::test_indexed_var_valid(0), 0);
    assert_eq!(nginx_stubs::test_indexed_var_not_found(0), 0);
}

#[test]
fn user_agent_recomputes_browser_flags_and_clears_them() {
    enable_alloc(64);
    let mut req = dummy_request();
    let mut storage = [unsafe { MaybeUninit::<ngx_table_elt_t>::zeroed().assume_init() }; 8];
    writable_list(&mut req, &mut storage, 0);
    let ptr = req.as_mut() as *mut ngx_http_request_t;
    nginx_stubs::write_internal(ptr.cast(), true);
    eval_write(
        ptr,
        "use ngx;\nngx::req::set_header(\"User-Agent\", \"Mozilla/4.0 (compatible; MSIE 6.0; Windows NT 5.1)\");\n",
    )
    .expect("ua");
    let flags = nginx_stubs::headers_in_ua_flags(ptr.cast());
    assert_eq!(flags.msie, 1);
    assert_eq!(flags.msie6, 1);
    assert_eq!(flags.opera, 0);
    assert_eq!(flags.gecko, 0);
    assert_eq!(flags.chrome, 0);
    assert_eq!(nginx_stubs::test_get_internal(ptr.cast()), 1);

    eval_write(
        ptr,
        "use ngx;\nngx::req::set_header(\"User-Agent\", \"Mozilla/5.0 Chrome/120.0 Safari/537.36\");\n",
    )
    .expect("chrome ua");
    let flags = nginx_stubs::headers_in_ua_flags(ptr.cast());
    assert_eq!(flags.msie, 0);
    assert_eq!(flags.msie6, 0);
    assert_eq!(flags.chrome, 1);
    assert_eq!(flags.safari, 1);
    assert_eq!(flags.gecko, 0);

    eval_write(ptr, "use ngx;\nngx::req::clear_header(\"User-Agent\");\n").expect("clear ua");
    let flags = nginx_stubs::headers_in_ua_flags(ptr.cast());
    assert_eq!(flags, nginx_stubs::UaFlags::default());
    assert!(nginx_stubs::headers_in_user_agent(ptr.cast()).is_null());
}

#[test]
fn connection_keep_alive_close_is_close_substring_semantics() {
    enable_alloc(64);
    let mut req = dummy_request();
    let mut storage = [unsafe { MaybeUninit::<ngx_table_elt_t>::zeroed().assume_init() }; 8];
    writable_list(&mut req, &mut storage, 0);
    let ptr = req.as_mut() as *mut ngx_http_request_t;
    eval_write(
        ptr,
        "use ngx;\nngx::req::set_header(\"Connection\", \"keep-alive, close\");\n",
    )
    .expect("connection");
    assert_eq!(
        nginx_stubs::headers_in_connection_type(ptr.cast()),
        NGX_HTTP_CONNECTION_CLOSE
    );
    eval_write(
        ptr,
        "use ngx;\nngx::req::set_header(\"Connection\", \"Keep-Alive\");\n",
    )
    .expect("keep-alive");
    assert_eq!(
        nginx_stubs::headers_in_connection_type(ptr.cast()),
        NGX_HTTP_CONNECTION_KEEP_ALIVE
    );
    eval_write(ptr, "use ngx;\nngx::req::clear_header(\"Connection\");\n").expect("clear");
    assert_eq!(nginx_stubs::headers_in_connection_type(ptr.cast()), 0);
}

#[test]
fn invalid_content_length_rejects_before_any_mutation() {
    enable_alloc(64);
    let mut req = dummy_request();
    let mut storage = [unsafe { MaybeUninit::<ngx_table_elt_t>::zeroed().assume_init() }; 8];
    let mut old_key = b"Content-Length".to_vec();
    let mut old_low = b"content-length".to_vec();
    let mut old_val = b"42".to_vec();
    storage[0] = header_elt(&mut old_key, &mut old_low, &mut old_val, 7);
    writable_list(&mut req, &mut storage, 1);
    req.headers_in.content_length = &mut storage[0];
    req.headers_in.content_length_n = 42;
    let ptr = req.as_mut() as *mut ngx_http_request_t;
    for bad in [
        "use ngx;\nngx::req::set_header(\"Content-Length\", \"abc\");\n",
        "use ngx;\nngx::req::set_header(\"Content-Length\", \"-1\");\n",
        "use ngx;\nngx::req::set_header(\"Content-Length\", \"999999999999999999999999\");\n",
    ] {
        let err = eval_write(ptr, bad).expect_err("invalid content-length");
        assert!(
            err.to_ascii_lowercase().contains("content-length")
                || err.to_ascii_lowercase().contains("invalid"),
            "{err}"
        );
        assert_eq!(req.headers_in.headers.part.nelts, 1);
        assert_eq!(nginx_stubs::headers_in_content_length_n(ptr.cast()), 42);
        assert!(!nginx_stubs::headers_in_content_length(ptr.cast()).is_null());
    }
}

#[test]
fn malformed_request_header_list_does_not_mutate() {
    enable_alloc(64);
    let mut req = dummy_request();
    req.pool = std::ptr::dangling_mut();
    req.headers_in.headers.part.elts = std::ptr::null_mut();
    req.headers_in.headers.part.nelts = 3;
    req.headers_in.headers.last = &mut req.headers_in.headers.part;
    req.headers_in.headers.size = std::mem::size_of::<ngx_table_elt_t>();
    req.headers_in.headers.nalloc = 3;
    let ptr = req.as_mut() as *mut ngx_http_request_t;
    let err = eval_write(ptr, "use ngx;\nngx::req::set_header(\"X-New\", \"v\");\n")
        .expect_err("null elts");
    assert!(is_host_err(&err), "{err}");

    let mut cycle = ngx_list_part_t {
        elts: std::ptr::null_mut(),
        nelts: 0,
        next: std::ptr::null_mut(),
    };
    cycle.next = &mut cycle;
    req.headers_in.headers.part = cycle;
    req.headers_in.headers.last = &mut req.headers_in.headers.part;
    let err =
        eval_write(ptr, "use ngx;\nngx::req::set_header(\"X-New\", \"v\");\n").expect_err("cycle");
    assert!(is_host_err(&err), "{err}");
}

#[test]
fn request_header_list_push_failure_rolls_back_old_entries() {
    enable_alloc(3);
    let mut req = dummy_request();
    let mut storage = [unsafe { MaybeUninit::<ngx_table_elt_t>::zeroed().assume_init() }; 1];
    let mut old_key = b"X-Keep".to_vec();
    let mut old_low = b"x-keep".to_vec();
    let mut old_val = b"old".to_vec();
    storage[0] = header_elt(&mut old_key, &mut old_low, &mut old_val, 11);
    writable_list(&mut req, &mut storage, 1);
    let ptr = req.as_mut() as *mut ngx_http_request_t;
    let err = eval_write(
        ptr,
        "use ngx;\nngx::req::set_header(\"X-New\", \"fresh\");\n",
    )
    .expect_err("alloc/list failure");
    assert!(
        err.to_ascii_lowercase().contains("alloc")
            || err.to_ascii_lowercase().contains("pool")
            || err.to_ascii_lowercase().contains("memory")
            || err.to_ascii_lowercase().contains("header"),
        "{err}"
    );
    assert_eq!(req.headers_in.headers.part.nelts, 1);
    assert_eq!(storage[0].hash, 11);
}

#[test]
fn set_method_allocation_failure_leaves_method_untouched() {
    nginx_stubs::stub_alloc_reset();
    let mut req = dummy_request();
    req.method = nginx_sys::NGX_HTTP_GET as ngx_uint_t;
    let mut get = b"GET".to_vec();
    req.method_name = ngx_str(&mut get);
    let old_data = req.method_name.data;
    let ptr = req.as_mut() as *mut ngx_http_request_t;
    let err = eval_write(ptr, "use ngx;\nngx::req::set_method(ngx::http::put());\n")
        .expect_err("null pool must fail before method mutation");
    assert!(
        err.to_ascii_lowercase().contains("pool") || err.to_ascii_lowercase().contains("alloc"),
        "{err}"
    );
    assert_eq!(req.method, nginx_sys::NGX_HTTP_GET as ngx_uint_t);
    assert_eq!(req.method_name.data, old_data);
}

#[test]
fn status_get_prefers_err_status_then_headers_out_then_http09() {
    let mut req = dummy_request();
    let ptr = req.as_mut() as *mut ngx_http_request_t;
    nginx_stubs::test_set_err_status(ptr.cast(), 500);
    req.headers_out.status = 200;
    let got = eval_write(ptr, "use ngx;\nngx::status::get();\n").expect("status");
    assert_eq!(got, Value::Int(500));

    nginx_stubs::test_set_err_status(ptr.cast(), 0);
    req.headers_out.status = 201;
    let got = eval_write(ptr, "use ngx;\nngx::status::get();\n").expect("status");
    assert_eq!(got, Value::Int(201));

    req.headers_out.status = 0;
    nginx_stubs::test_set_http_version(ptr.cast(), 9);
    let got = eval_write(ptr, "use ngx;\nngx::status::get();\n").expect("http09");
    assert_eq!(got, Value::Int(9));
}

#[test]
fn redirect_without_disposition_does_not_mutate_location_or_status() {
    enable_alloc(64);
    let mut req = dummy_request();
    let mut storage = [unsafe { MaybeUninit::<ngx_table_elt_t>::zeroed().assume_init() }; 8];
    writable_out_list(&mut req, &mut storage, 0);
    req.headers_out.status = 200;
    let ptr = req.as_mut() as *mut ngx_http_request_t;
    let program = compile("use ngx;\nngx::redirect(\"/x\", null);\n");
    let mut vm = Vm::try_new((*program).clone()).expect("vm");
    bind_ngx_hosts(&mut vm);
    let err = with_request_scope(
        ptr,
        RequestIdentity::new(3, 3),
        NgxPhase::Access,
        write_ops(),
        || match vm.run() {
            Err(err) => err.to_string(),
            Ok(status) => panic!("expected disposition failure, got {status:?}"),
        },
    );
    assert!(
        err.to_ascii_lowercase().contains("disposition")
            || err.to_ascii_lowercase().contains("null"),
        "{err}"
    );
    assert!(nginx_stubs::headers_out_location(ptr.cast()).is_null());
    assert_eq!(req.headers_out.status, 200);
}

#[test]
fn redirect_first_wins_leaves_previous_disposition_and_location() {
    enable_alloc(64);
    let mut req = dummy_request();
    let mut storage = [unsafe { MaybeUninit::<ngx_table_elt_t>::zeroed().assume_init() }; 8];
    writable_out_list(&mut req, &mut storage, 0);
    let ptr = req.as_mut() as *mut ngx_http_request_t;
    let mut disposition = RequestDisposition::Finalize(403);
    let program = compile("use ngx;\nngx::redirect(\"/later\", 302);\n");
    let mut vm = Vm::try_new((*program).clone()).expect("vm");
    bind_ngx_hosts(&mut vm);
    let _ = with_request_scope_full(
        ptr,
        std::ptr::null_mut(),
        &mut disposition,
        RequestIdentity::new(4, 4),
        NgxPhase::Access,
        write_ops(),
        || vm.run(),
    );
    assert_eq!(disposition, RequestDisposition::Finalize(403));
    assert!(nginx_stubs::headers_out_location(ptr.cast()).is_null());
    assert_eq!(req.headers_out.status, 0);
}

#[test]
fn response_set_all_allocation_failure_restores_specials() {
    enable_alloc(2);
    let mut req = dummy_request();
    let mut storage = [unsafe { MaybeUninit::<ngx_table_elt_t>::zeroed().assume_init() }; 1];
    let mut old_key = b"Content-Type".to_vec();
    let mut old_low = b"content-type".to_vec();
    let mut old_val = b"text/plain".to_vec();
    storage[0] = header_elt(&mut old_key, &mut old_low, &mut old_val, 21);
    writable_out_list(&mut req, &mut storage, 1);
    req.headers_out.content_type = ngx_str(&mut old_val);
    req.headers_out.content_type_len = old_val.len();
    let ptr = req.as_mut() as *mut ngx_http_request_t;
    let err = eval_write(
        ptr,
        "use ngx;\nlet h = {\"X-A\": \"1\", \"X-B\": \"2\"};\nngx::header::set_all(h);\n",
    )
    .expect_err("set_all alloc");
    assert!(
        err.to_ascii_lowercase().contains("alloc")
            || err.to_ascii_lowercase().contains("pool")
            || err.to_ascii_lowercase().contains("memory")
            || err.to_ascii_lowercase().contains("header"),
        "{err}"
    );
    assert_eq!(req.headers_in.headers.part.nelts, 0);
    assert_eq!(req.headers_out.headers.part.nelts, 1);
    assert_eq!(storage[0].hash, 21);
    assert_eq!(req.headers_out.content_type_len, old_val.len());
}

#[test]
fn send_last_response_matrix_and_helper_failure() {
    nginx_stubs::stub_send_reset();
    let mut req = dummy_request();
    let ptr = req.as_mut() as *mut ngx_http_request_t;
    unsafe {
        nginx_stubs::write_header_sent(ptr.cast(), false);
        for status in [100, 101, 201, 204, 301, 302, 403, 500] {
            nginx_stubs::stub_send_reset();
            let rc = ngx_rustscript_c_send_last_response(ptr, status);
            assert_eq!(rc, 0, "status {status}");
            assert_eq!(nginx_stubs::stub_send_header_calls(), 0, "status {status}");
            assert_eq!(nginx_stubs::stub_send_special_calls(), 0, "status {status}");
        }
        nginx_stubs::stub_send_reset();
        let rc = ngx_rustscript_c_send_last_response(ptr, 200);
        assert_eq!(rc, 0);
        assert_eq!(nginx_stubs::stub_send_header_calls(), 1);
        assert_eq!(nginx_stubs::stub_send_special_calls(), 1);

        nginx_stubs::stub_send_reset();
        let rc = ngx_rustscript_c_send_last_response(ptr, 202);
        assert_eq!(rc, 0);
        assert_eq!(nginx_stubs::stub_send_header_calls(), 1);

        nginx_stubs::stub_send_reset();
        nginx_stubs::write_header_sent(ptr.cast(), true);
        let rc = ngx_rustscript_c_send_last_response(ptr, 200);
        assert_eq!(rc, 0);
        assert_eq!(nginx_stubs::stub_send_header_calls(), 0);

        nginx_stubs::write_header_sent(ptr.cast(), false);
        nginx_stubs::stub_send_reset();
        nginx_stubs::stub_send_header_returns(-1);
        let rc = ngx_rustscript_c_send_last_response(ptr, 200);
        assert_eq!(rc, -1);
        assert_eq!(nginx_stubs::stub_send_header_calls(), 1);
        assert_eq!(
            nginx_stubs::stub_send_special_calls(),
            0,
            "must not send_special after send_header failure"
        );

        nginx_stubs::stub_send_reset();
        nginx_stubs::stub_send_header_returns(0);
        nginx_stubs::stub_send_special_returns(-1);
        let rc = ngx_rustscript_c_send_last_response(ptr, 200);
        assert_eq!(rc, -1);
    }
}

#[test]
fn variable_set_c_abi_branches() {
    nginx_stubs::stub_var_reset();
    enable_alloc(32);
    let mut req = dummy_request();
    req.pool = std::ptr::dangling_mut();
    let ptr = req.as_mut() as *mut ngx_http_request_t;
    assert_eq!(
        nginx_stubs::test_install_variable(ptr.cast(), "foo", true, true, true, 0),
        0
    );
    let mut name = b"foo".to_vec();
    let mut value = b"bar".to_vec();
    let rc = unsafe {
        ngx_rustscript_c_variable_set(
            ptr,
            name.as_mut_ptr(),
            name.len(),
            value.as_mut_ptr(),
            value.len(),
            0,
        )
    };
    assert_eq!(rc, 0);
    assert_eq!(nginx_stubs::test_set_handler_calls(), 1);
    assert!(!nginx_stubs::test_last_set_vv().is_null());
    assert_eq!(nginx_stubs::test_last_set_data(), 0x1000);

    nginx_stubs::stub_var_reset();
    assert_eq!(
        nginx_stubs::test_install_variable(ptr.cast(), "idx", true, true, false, 0),
        0
    );
    let mut name = b"idx".to_vec();
    let mut value = b"xyz".to_vec();
    let rc = unsafe {
        ngx_rustscript_c_variable_set(
            ptr,
            name.as_mut_ptr(),
            name.len(),
            value.as_mut_ptr(),
            value.len(),
            0,
        )
    };
    assert_eq!(rc, 0);
    let vv = nginx_stubs::test_indexed_var(0);
    assert!(!vv.is_null());
    assert_eq!(nginx_stubs::test_var_valid(vv), 1);
    assert_eq!(nginx_stubs::test_var_not_found(vv), 0);
    assert_eq!(nginx_stubs::test_var_len(vv), 3);

    let rc = unsafe {
        ngx_rustscript_c_variable_set(
            ptr,
            name.as_mut_ptr(),
            name.len(),
            std::ptr::null_mut(),
            0,
            1,
        )
    };
    assert_eq!(rc, 0);
    assert_eq!(nginx_stubs::test_var_not_found(vv), 1);
    assert_eq!(nginx_stubs::test_var_valid(vv), 0);

    nginx_stubs::stub_var_reset();
    assert_eq!(
        nginx_stubs::test_install_variable(ptr.cast(), "ro", true, false, false, 0),
        0
    );
    let mut name = b"ro".to_vec();
    let mut value = b"x".to_vec();
    let rc = unsafe {
        ngx_rustscript_c_variable_set(
            ptr,
            name.as_mut_ptr(),
            name.len(),
            value.as_mut_ptr(),
            value.len(),
            0,
        )
    };
    assert_eq!(rc, 2, "read-only");

    nginx_stubs::stub_var_reset();
    assert_eq!(
        nginx_stubs::test_install_variable(ptr.cast(), "noidx", false, true, false, 0),
        0
    );
    let mut name = b"noidx".to_vec();
    let rc = unsafe {
        ngx_rustscript_c_variable_set(
            ptr,
            name.as_mut_ptr(),
            name.len(),
            value.as_mut_ptr(),
            value.len(),
            0,
        )
    };
    assert_eq!(rc, 3, "non-indexed without handler");

    nginx_stubs::stub_var_reset();
    let mut name = b"unknown".to_vec();
    let rc = unsafe {
        ngx_rustscript_c_variable_set(
            ptr,
            name.as_mut_ptr(),
            name.len(),
            value.as_mut_ptr(),
            value.len(),
            0,
        )
    };
    assert_eq!(rc, 1, "unknown");
}

#[test]
fn disposition_mut_participates_in_exclusive_request_borrow() {
    let mut req = dummy_request();
    let mut disposition = RequestDisposition::Continue;
    let identity = RequestIdentity::new(9, 9);
    let ptr = req.as_mut() as *mut ngx_http_request_t;
    with_request_scope_full(
        ptr,
        std::ptr::null_mut(),
        &mut disposition,
        identity,
        NgxPhase::Access,
        write_ops(),
        || {
            let nested = with_current_request_mut(identity, |_| {
                with_current_disposition_mut(identity, |_| ())
            });
            assert_eq!(nested, Ok(Err(ScopeError::AlreadyBorrowed)));
            let nested = with_current_disposition_mut(identity, |_| {
                with_current_request_mut(identity, |_| ())
            });
            assert_eq!(nested, Ok(Err(ScopeError::AlreadyBorrowed)));
            assert!(with_current_disposition_mut(identity, |_| ()).is_ok());
            let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _ = with_current_disposition_mut(identity, |_| panic!("borrow-cleanup"));
            }));
            assert!(panicked.is_err());
            assert!(with_current_disposition_mut(identity, |_| ()).is_ok());
        },
    );
}

#[test]
fn var_get_name_limit_is_1024() {
    let mut req = dummy_request();
    let ptr = req.as_mut() as *mut ngx_http_request_t;
    let name_ok = "a".repeat(1024);
    let name_bad = "a".repeat(1025);
    eval_write(ptr, &format!("use ngx;\nngx::var::get(\"{name_ok}\");\n"))
        .expect("1024-byte name is accepted");
    let err = eval_write(ptr, &format!("use ngx;\nngx::var::get(\"{name_bad}\");\n"))
        .expect_err("1025-byte name");
    assert!(err.contains("1024"), "{err}");
}

#[test]
fn cookie_and_content_type_dedicated_pointers() {
    enable_alloc(64);
    let mut req = dummy_request();
    let mut storage = [unsafe { MaybeUninit::<ngx_table_elt_t>::zeroed().assume_init() }; 8];
    writable_list(&mut req, &mut storage, 0);
    let ptr = req.as_mut() as *mut ngx_http_request_t;
    eval_write(
        ptr,
        "use ngx;\nngx::req::set_header(\"Cookie\", [\"a=1\", \"b=2\"]);\nngx::req::set_header(\"Content-Type\", \"text/plain\");\n",
    )
    .expect("cookie/ct");
    assert!(!req.headers_in.cookie.is_null());
    assert!(!req.headers_in.content_type.is_null());
    unsafe {
        assert!(!(*req.headers_in.cookie).next.is_null());
    }
    eval_write(
        ptr,
        "use ngx;\nngx::req::clear_header(\"Cookie\");\nngx::req::clear_header(\"Content-Type\");\n",
    )
    .expect("clear");
    assert!(req.headers_in.cookie.is_null());
    assert!(req.headers_in.content_type.is_null());
    nginx_stubs::stub_alloc_reset();
}
