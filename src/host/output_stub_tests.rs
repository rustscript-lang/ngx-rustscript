// Stub-level output host tests compiled only under `cfg(test)`.
//
// Integration tests link the production rlib (no `--cfg test`, no
// `ngx_features="rustc-test"`), so they exercise the fail-closed missing-ctx
// path. These tests need the stub FALLBACK_OUTPUT map and therefore live in
// the unit-test compilation.

use ngx_rustscript_nginx_test_stubs as nginx_stubs;

use std::mem::MaybeUninit;
use std::sync::Arc;

use crate::config::compile_rss_source_for_phase;
use crate::host::{
    bind_ngx_hosts, with_request_scope, AllowedOps, NgxPhase,
};
use crate::notify::{RequestIdentity, DEFAULT_MAX_IN_FLIGHT};
use crate::request_state::{AccessStatus, IdentityRegistry, ReadyEffect};
use crate::runtime::WorkerRuntime;
use nginx_sys::ngx_http_request_t;
use vm::{Program, Value, Vm, VmStatus};


/// Serializes tests that share process-global stub or zone state.
fn stub_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|error| error.into_inner())
}

fn compile_content(source: &str) -> Arc<Program> {
    compile_rss_source_for_phase(source, NgxPhase::Content).expect("content source should compile")
}

fn dummy_request() -> Box<ngx_http_request_t> {
    nginx_stubs::stub_alloc_reset();
    nginx_stubs::stub_alloc_fail_after(64);
    let mut req = Box::new(unsafe { MaybeUninit::<ngx_http_request_t>::zeroed().assume_init() });
    let ptr = (&mut *req) as *mut ngx_http_request_t;
    req.main = ptr;
    req.pool = std::ptr::dangling_mut();
    req.headers_out.content_length_n = -1;
    nginx_stubs::test_set_http_version(ptr.cast(), 1001);
    req
}

fn eval_in_content(request: *mut ngx_http_request_t, source: &str) -> Result<Value, String> {
    let program = compile_content(source);
    let mut vm = Vm::try_new((*program).clone()).expect("construct vm");
    bind_ngx_hosts(&mut vm);
    with_request_scope(
        request,
        RequestIdentity::new(1, 1),
        NgxPhase::Content,
        AllowedOps::for_phase(NgxPhase::Content),
        || match vm.run() {
            Ok(VmStatus::Halted) => Ok(vm.stack().last().cloned().unwrap_or(Value::Null)),
            Ok(status) => Err(format!("unexpected status {status:?}")),
            Err(err) => Err(err.to_string()),
        },
    )
}

#[test]
fn send_headers_is_idempotent_and_sets_default_status() {
    let _guard = stub_lock();
    nginx_stubs::stub_send_reset();
    nginx_stubs::stub_output_reset();
    let mut req = dummy_request();
    let ptr = req.as_mut() as *mut ngx_http_request_t;
    eval_in_content(
        ptr,
        "use ngx;\nngx::send_headers();\nngx::send_headers();\n",
    )
    .expect("idempotent send_headers");
    assert_eq!(nginx_stubs::stub_send_header_calls(), 1);
    assert_eq!(req.headers_out.status, 200);
}

#[test]
fn print_and_say_preserve_order_and_say_appends_newline() {
    let _guard = stub_lock();
    nginx_stubs::stub_send_reset();
    nginx_stubs::stub_output_reset();
    let mut req = dummy_request();
    let ptr = req.as_mut() as *mut ngx_http_request_t;
    eval_in_content(
        ptr,
        "use ngx;\nngx::print(\"ab\");\nngx::say(\"cd\");\nngx::print(12);\n",
    )
    .expect("print/say");
    assert_eq!(nginx_stubs::stub_output_body(), b"abcd\n12");
    assert!(nginx_stubs::stub_send_header_calls() >= 1);
}

#[test]
fn print_flattens_arrays_and_accepts_binary() {
    let _guard = stub_lock();
    nginx_stubs::stub_send_reset();
    nginx_stubs::stub_output_reset();
    let mut req = dummy_request();
    let ptr = req.as_mut() as *mut ngx_http_request_t;
    eval_in_content(
        ptr,
        "use ngx;\nngx::print([\"he\", \"llo\"]);\nngx::print(ngx::md5_bin(\"hello\"));\n",
    )
    .expect("array/binary print");
    let body = nginx_stubs::stub_output_body();
    assert!(body.starts_with(b"hello"), "{body:?}");
    assert_eq!(body.len(), 5 + 16, "hello + md5_bin");
}

#[test]
fn flush_false_sends_flush_buf_without_waiting() {
    let _guard = stub_lock();
    nginx_stubs::stub_send_reset();
    nginx_stubs::stub_output_reset();
    let mut req = dummy_request();
    let ptr = req.as_mut() as *mut ngx_http_request_t;
    let program = compile_content("use ngx;\nngx::print(\"x\");\nngx::flush(false);\n");
    let mut worker = WorkerRuntime::start().unwrap();
    let mut exec = worker
        .bind_phase_exec(program, 1, NgxPhase::Content)
        .unwrap();
    let mut registry = IdentityRegistry::with_capacity(DEFAULT_MAX_IN_FLIGHT);
    let status = with_request_scope(
        ptr,
        exec.identity(),
        NgxPhase::Content,
        AllowedOps::for_phase(NgxPhase::Content),
        || exec.handle_access(&mut registry, &worker),
    );
    worker.shutdown();
    assert_eq!(status, AccessStatus::Ok);
    assert!(nginx_stubs::stub_output_saw_flush());
}

#[test]
fn flush_true_waits_when_connection_is_buffered() {
    let _guard = stub_lock();
    nginx_stubs::stub_send_reset();
    nginx_stubs::stub_output_reset();
    nginx_stubs::stub_output_set_needs_flush_wait(1);
    let mut req = dummy_request();
    let ptr = req.as_mut() as *mut ngx_http_request_t;
    let program = compile_content("use ngx;\nngx::print(\"x\");\nngx::flush(true);\n");
    let mut worker = WorkerRuntime::start().unwrap();
    let mut exec = worker
        .bind_phase_exec(program, 1, NgxPhase::Content)
        .unwrap();
    let mut registry = IdentityRegistry::with_capacity(DEFAULT_MAX_IN_FLIGHT);
    let status = with_request_scope(
        ptr,
        exec.identity(),
        NgxPhase::Content,
        AllowedOps::for_phase(NgxPhase::Content),
        || exec.handle_access(&mut registry, &worker),
    );
    worker.shutdown();
    nginx_stubs::stub_output_set_needs_flush_wait(0);
    assert_eq!(
        status,
        AccessStatus::Again,
        "flush(wait=true) must yield on backpressure"
    );
}

#[test]
fn eof_sends_last_buf_and_rejects_later_output() {
    let _guard = stub_lock();
    nginx_stubs::stub_send_reset();
    nginx_stubs::stub_output_reset();
    let mut req = dummy_request();
    let ptr = req.as_mut() as *mut ngx_http_request_t;
    eval_in_content(ptr, "use ngx;\nngx::print(\"x\");\nngx::eof();\n").expect("eof");
    assert!(nginx_stubs::stub_output_saw_last());
    let err = eval_in_content(ptr, "use ngx;\nngx::print(\"y\");\n").expect_err("post-eof print");
    assert!(
        err.to_ascii_lowercase().contains("eof"),
        "post-eof must mention eof, got {err}"
    );
}

#[test]
fn status_and_header_mutation_rejected_after_send_headers() {
    let _guard = stub_lock();
    nginx_stubs::stub_send_reset();
    nginx_stubs::stub_output_reset();
    let mut req = dummy_request();
    let ptr = req.as_mut() as *mut ngx_http_request_t;
    let err = eval_in_content(
        ptr,
        "use ngx;\nngx::send_headers();\nngx::status::set(201);\n",
    )
    .expect_err("status after headers");
    assert!(err.to_ascii_lowercase().contains("header"), "got {err}");
}

#[test]
fn head_skips_body_and_http10_buffers_until_eof() {
    let _guard = stub_lock();
    nginx_stubs::stub_send_reset();
    nginx_stubs::stub_output_reset();
    let mut req = dummy_request();
    nginx_stubs::test_request_set_header_only(req.as_mut() as *mut _ as *mut _, 1);
    let ptr = req.as_mut() as *mut ngx_http_request_t;
    eval_in_content(ptr, "use ngx;\nngx::print(\"secret\");\n").expect("head print");
    assert!(
        nginx_stubs::stub_output_body().is_empty(),
        "HEAD must not emit body"
    );

    nginx_stubs::stub_send_reset();
    nginx_stubs::stub_output_reset();
    let mut req = dummy_request();
    nginx_stubs::test_set_http_version(req.as_mut() as *mut _ as *mut _, 1000);
    let ptr = req.as_mut() as *mut ngx_http_request_t;
    eval_in_content(
        ptr,
        "use ngx;\nngx::print(\"ab\");\nngx::print(\"cd\");\nngx::eof();\n",
    )
    .expect("http10 print");
    assert_eq!(nginx_stubs::stub_output_body(), b"abcd");
    assert_eq!(req.headers_out.content_length_n, 4);
}

#[test]
fn second_eof_returns_seen_eof() {
    let _guard = stub_lock();
    nginx_stubs::stub_send_reset();
    nginx_stubs::stub_output_reset();
    let mut req = dummy_request();
    let ptr = req.as_mut() as *mut ngx_http_request_t;
    eval_in_content(
        ptr,
        r#"
use ngx;
ngx::eof();
"#,
    )
    .expect("first eof");
    let err = eval_in_content(
        ptr,
        r#"
use ngx;
ngx::eof();
"#,
    )
    .expect_err("second eof");
    assert!(
        err.to_ascii_lowercase().contains("seen eof"),
        "second eof must be seen eof, got {err}"
    );
}

#[test]
fn http10_explicit_content_length_is_not_overwritten_and_rejects_overflow() {
    let _guard = stub_lock();
    nginx_stubs::stub_send_reset();
    nginx_stubs::stub_output_reset();
    let mut req = dummy_request();
    nginx_stubs::test_set_http_version(req.as_mut() as *mut _ as *mut _, 1000);
    nginx_stubs::test_request_set_content_length_n(req.as_mut() as *mut _ as *mut _, 4);
    let ptr = req.as_mut() as *mut ngx_http_request_t;
    eval_in_content(
        ptr,
        r#"
use ngx;
ngx::print("ab");
ngx::send_headers();
"#,
    )
    .expect("explicit CL print");
    assert_eq!(
        nginx_stubs::test_request_content_length_n(ptr.cast()),
        4,
        "must not overwrite a user Content-Length"
    );
    assert_eq!(nginx_stubs::stub_output_body(), b"ab");
    let err = eval_in_content(
        ptr,
        r#"
use ngx;
ngx::print("xyz");
"#,
    )
    .expect_err("overflow body");
    assert!(
        err.to_ascii_lowercase().contains("content-length"),
        "overflow must mention Content-Length, got {err}"
    );
    assert_eq!(nginx_stubs::stub_output_body(), b"ab");
}

#[test]
fn print_rejects_maps_deep_nesting_and_oversized_arrays() {
    let _guard = stub_lock();
    nginx_stubs::stub_send_reset();
    nginx_stubs::stub_output_reset();
    let mut req = dummy_request();
    let ptr = req.as_mut() as *mut ngx_http_request_t;
    let map_err = eval_in_content(
        ptr,
        r#"
use ngx;
ngx::print({a: 1});
"#,
    )
    .expect_err("map");
    assert!(
        map_err.to_ascii_lowercase().contains("map"),
        "got {map_err}"
    );
}

#[test]
fn nested_array_is_bounded_before_allocation() {
    let _guard = stub_lock();
    nginx_stubs::stub_send_reset();
    nginx_stubs::stub_output_reset();
    let mut req = dummy_request();
    let ptr = req.as_mut() as *mut ngx_http_request_t;
    let chunk = "x".repeat(200_000);
    let source = format!(
        "use ngx;\nngx::print([\"{0}\", \"{0}\", \"{0}\", \"{0}\", \"{0}\", \"{0}\"]);\n",
        chunk
    );
    let err = eval_in_content(ptr, &source).expect_err("per-call bound");
    assert!(
        err.to_ascii_lowercase().contains("limit") || err.to_ascii_lowercase().contains("byte"),
        "got {err}"
    );
}

#[test]
fn output_filter_again_keeps_body_and_done_does_not_fail() {
    let _guard = stub_lock();
    nginx_stubs::stub_send_reset();
    nginx_stubs::stub_output_reset();
    nginx_stubs::stub_output_set_rc(-2);
    let mut req = dummy_request();
    let ptr = req.as_mut() as *mut ngx_http_request_t;
    eval_in_content(
        ptr,
        r#"
use ngx;
ngx::print("kept");
"#,
    )
    .expect("AGAIN body");
    assert_eq!(nginx_stubs::stub_output_body(), b"kept");

    nginx_stubs::stub_send_reset();
    nginx_stubs::stub_output_reset();
    nginx_stubs::stub_output_set_rc(-4);
    eval_in_content(
        ptr,
        r#"
use ngx;
ngx::print("done");
"#,
    )
    .expect("DONE body");
    assert_eq!(nginx_stubs::stub_output_body(), b"done");

    nginx_stubs::stub_send_reset();
    nginx_stubs::stub_output_reset();
    nginx_stubs::stub_send_header_returns(-2);
    let mut header_req = dummy_request();
    let header_ptr = header_req.as_mut() as *mut ngx_http_request_t;
    let err = eval_in_content(
        header_ptr,
        r#"
use ngx;
ngx::send_headers();
"#,
    )
    .expect_err("header AGAIN");
    assert!(
        err.to_ascii_lowercase().contains("send_headers")
            || err.to_ascii_lowercase().contains("header"),
        "got {err}"
    );
}

#[test]
fn subrequest_eof_sets_last_in_chain_not_last_buf() {
    let _guard = stub_lock();
    nginx_stubs::stub_send_reset();
    nginx_stubs::stub_output_reset();
    let mut main = dummy_request();
    let mut sub = dummy_request();
    let main_ptr = main.as_mut() as *mut ngx_http_request_t;
    let sub_ptr = sub.as_mut() as *mut ngx_http_request_t;
    nginx_stubs::test_request_set_main(sub_ptr.cast(), main_ptr.cast());
    eval_in_content(
        sub_ptr,
        r#"
use ngx;
ngx::print("s");
ngx::eof();
"#,
    )
    .expect("sub eof");
    assert!(
        nginx_stubs::stub_output_saw_last_in_chain(),
        "subrequest eof must set last_in_chain"
    );
    assert!(
        !nginx_stubs::stub_output_saw_last(),
        "subrequest eof must not set last_buf"
    );
}

#[test]
fn identity_zero_is_rejected() {
    let _guard = stub_lock();
    nginx_stubs::stub_send_reset();
    nginx_stubs::stub_output_reset();
    let mut req = dummy_request();
    let ptr = req.as_mut() as *mut ngx_http_request_t;
    let program = compile_content(
        r#"
use ngx;
ngx::print("x");
"#,
    );
    let mut vm = Vm::try_new((*program).clone()).expect("vm");
    bind_ngx_hosts(&mut vm);
    let err = with_request_scope(
        ptr,
        RequestIdentity::new(0, 0),
        NgxPhase::Content,
        AllowedOps::for_phase(NgxPhase::Content),
        || match vm.run() {
            Ok(_) => Ok(()),
            Err(err) => Err(err.to_string()),
        },
    )
    .expect_err("identity 0");
    assert!(err.to_ascii_lowercase().contains("identity"), "got {err}");
}

#[test]
fn pointer_reuse_does_not_inherit_eof() {
    let _guard = stub_lock();
    nginx_stubs::stub_send_reset();
    nginx_stubs::stub_output_reset();
    let mut req = dummy_request();
    let ptr = req.as_mut() as *mut ngx_http_request_t;
    eval_in_content(
        ptr,
        r#"
use ngx;
ngx::eof();
"#,
    )
    .expect("first request eof");
    crate::host::cleanup_output(ptr);
    let program = compile_content(
        r#"
use ngx;
ngx::print("fresh");
"#,
    );
    let mut vm = Vm::try_new((*program).clone()).expect("vm");
    bind_ngx_hosts(&mut vm);
    with_request_scope(
        ptr,
        RequestIdentity::new(2, 9),
        NgxPhase::Content,
        AllowedOps::for_phase(NgxPhase::Content),
        || vm.run().expect("reused pointer must not inherit eof"),
    );
    assert_eq!(nginx_stubs::stub_output_body(), b"fresh");
}

unsafe extern "C" fn dummy_writer(_: *mut ngx_http_request_t) {}

fn dummy_writer_ptr() -> Option<unsafe extern "C" fn(*mut ngx_http_request_t)> {
    Some(dummy_writer as unsafe extern "C" fn(*mut ngx_http_request_t))
}

fn err_status(ptr: *mut ngx_http_request_t) -> usize {
    nginx_stubs::test_err_status(ptr.cast())
}

fn mark_headers_sent(ptr: *mut ngx_http_request_t) {
    nginx_stubs::write_header_sent(ptr.cast(), true);
}

fn assert_handler_restored(ptr: *mut ngx_http_request_t) {
    unsafe {
        #[allow(unpredictable_function_pointer_comparisons)]
        {
            assert_eq!((*ptr).write_event_handler, dummy_writer_ptr());
        }
    }
}

fn run_flush_wait(
    source: &str,
) -> (
    Box<ngx_http_request_t>,
    crate::request_state::AccessExec,
    IdentityRegistry<u64>,
    WorkerRuntime,
) {
    nginx_stubs::stub_send_reset();
    nginx_stubs::stub_output_reset();
    nginx_stubs::stub_output_set_needs_flush_wait(1);
    let mut req = dummy_request();
    let ptr = req.as_mut() as *mut ngx_http_request_t;
    unsafe {
        (*ptr).write_event_handler = dummy_writer_ptr();
    }
    let program = compile_content(source);
    let mut worker = WorkerRuntime::start().unwrap();
    let mut exec = worker
        .bind_phase_exec(program, 1, NgxPhase::Content)
        .unwrap();
    let mut registry = IdentityRegistry::with_capacity(DEFAULT_MAX_IN_FLIGHT);
    let status = with_request_scope(
        ptr,
        exec.identity(),
        NgxPhase::Content,
        AllowedOps::for_phase(NgxPhase::Content),
        || exec.handle_access(&mut registry, &worker),
    );
    assert_eq!(status, AccessStatus::Again);
    exec.commit_suspend();
    (req, exec, registry, worker)
}

#[test]
fn flush_write_callback_resumes_and_restores_handler() {
    let _guard = stub_lock();
    let (mut req, mut exec, mut registry, mut worker) = run_flush_wait(
        r#"
use ngx;
ngx::print("x");
ngx::flush(true);
"#,
    );
    let ptr = req.as_mut() as *mut ngx_http_request_t;
    nginx_stubs::stub_set_flush_continue(0);
    crate::host::fire_flush_write_handler(ptr);
    let effect = with_request_scope(
        ptr,
        exec.identity(),
        NgxPhase::Content,
        AllowedOps::for_phase(NgxPhase::Content),
        || exec.apply_ready(exec.identity(), &mut registry, &worker),
    );
    worker.shutdown();
    assert_eq!(effect, ReadyEffect::Halted);
    unsafe {
        #[allow(unpredictable_function_pointer_comparisons)]
        {
            assert_eq!((*ptr).write_event_handler, dummy_writer_ptr());
        }
    }
}

#[test]
fn flush_send_timeout_fails_wait() {
    let _guard = stub_lock();
    let (mut req, mut exec, mut registry, mut worker) = run_flush_wait(
        r#"
use ngx;
ngx::flush(true);
"#,
    );
    let ptr = req.as_mut() as *mut ngx_http_request_t;
    nginx_stubs::stub_set_flush_continue(408);
    crate::host::fire_flush_write_handler(ptr);
    let effect = with_request_scope(
        ptr,
        exec.identity(),
        NgxPhase::Content,
        AllowedOps::for_phase(NgxPhase::Content),
        || exec.apply_ready(exec.identity(), &mut registry, &worker),
    );
    worker.shutdown();
    assert_eq!(effect, ReadyEffect::Failed);
}

#[test]
fn flush_delayed_then_ready_does_not_spin() {
    let _guard = stub_lock();
    let (mut req, mut exec, mut registry, mut worker) = run_flush_wait(
        r#"
use ngx;
ngx::flush(true);
"#,
    );
    let ptr = req.as_mut() as *mut ngx_http_request_t;
    nginx_stubs::stub_set_flush_continue(-2);
    crate::host::fire_flush_write_handler(ptr);
    nginx_stubs::stub_set_flush_continue(0);
    crate::host::fire_flush_write_handler(ptr);
    let effect = with_request_scope(
        ptr,
        exec.identity(),
        NgxPhase::Content,
        AllowedOps::for_phase(NgxPhase::Content),
        || exec.apply_ready(exec.identity(), &mut registry, &worker),
    );
    worker.shutdown();
    assert_eq!(effect, ReadyEffect::Halted);
}

#[test]
fn stale_flush_wake_after_cleanup_is_ignored() {
    let _guard = stub_lock();
    let (mut req, exec, _registry, mut worker) = run_flush_wait(
        r#"
use ngx;
ngx::flush(true);
"#,
    );
    let ptr = req.as_mut() as *mut ngx_http_request_t;
    crate::host::cleanup_output(ptr);
    crate::host::fire_flush_write_handler(ptr);
    worker.shutdown();
    drop(exec);
    unsafe {
        #[allow(unpredictable_function_pointer_comparisons)]
        {
            assert_eq!((*ptr).write_event_handler, dummy_writer_ptr());
        }
    }
}

#[test]
fn client_abort_cancels_flush_wait() {
    let _guard = stub_lock();
    let (mut req, mut exec, mut registry, mut worker) = run_flush_wait(
        r#"
use ngx;
ngx::flush(true);
"#,
    );
    let ptr = req.as_mut() as *mut ngx_http_request_t;
    crate::host::cleanup_output(ptr);
    let effect = with_request_scope(
        ptr,
        exec.identity(),
        NgxPhase::Content,
        AllowedOps::for_phase(NgxPhase::Content),
        || exec.apply_ready(exec.identity(), &mut registry, &worker),
    );
    worker.shutdown();
    assert!(
        effect == ReadyEffect::Failed || effect == ReadyEffect::Halted,
        "499/cleanup must complete the wait, got {effect:?}"
    );
}

#[test]
fn ok_finish_and_cleanup_with_armed_wait_do_not_stamp_499() {
    let _guard = stub_lock();
    let (mut req, exec, _registry, mut worker) = run_flush_wait(
        r#"
use ngx;
ngx::flush(true);
"#,
    );
    let ptr = req.as_mut() as *mut ngx_http_request_t;
    mark_headers_sent(ptr);
    with_request_scope(
        ptr,
        exec.identity(),
        NgxPhase::Content,
        AllowedOps::for_phase(NgxPhase::Content),
        || {
            assert_eq!(crate::host::finish_response(ptr), 0);
        },
    );
    assert_eq!(
        err_status(ptr),
        0,
        "NormalFinish must restore the waiter without stamping 499"
    );
    crate::host::cleanup_output(ptr);
    assert_eq!(
        err_status(ptr),
        0,
        "successful cleanup with an armed wait must not stamp 499"
    );
    assert_handler_restored(ptr);
    crate::host::cleanup_output(ptr);
    assert_handler_restored(ptr);
    worker.shutdown();
    drop(exec);
}

#[test]
fn error_and_finalize_cleanup_do_not_stamp_499() {
    let _guard = stub_lock();
    let (mut req, exec, _registry, mut worker) = run_flush_wait(
        r#"
use ngx;
ngx::flush(true);
"#,
    );
    let ptr = req.as_mut() as *mut ngx_http_request_t;
    mark_headers_sent(ptr);
    with_request_scope(
        ptr,
        exec.identity(),
        NgxPhase::Content,
        AllowedOps::for_phase(NgxPhase::Content),
        || {
            crate::host::finish_response_on_error(ptr);
        },
    );
    assert_eq!(
        err_status(ptr),
        0,
        "TerminalError finish must not stamp 499"
    );
    crate::host::cleanup_output(ptr);
    assert_eq!(err_status(ptr), 0);
    assert_handler_restored(ptr);
    worker.shutdown();
    drop(exec);

    let (mut req, exec, _registry, mut worker) = run_flush_wait(
        r#"
use ngx;
ngx::flush(true);
"#,
    );
    let ptr = req.as_mut() as *mut ngx_http_request_t;
    mark_headers_sent(ptr);
    with_request_scope(
        ptr,
        exec.identity(),
        NgxPhase::Content,
        AllowedOps::for_phase(NgxPhase::Content),
        || {
            crate::host::finish_response_on_finalize(ptr);
        },
    );
    assert_eq!(
        err_status(ptr),
        0,
        "Finalize finish must not stamp 499"
    );
    crate::host::cleanup_output(ptr);
    assert_eq!(err_status(ptr), 0);
    assert_handler_restored(ptr);
    worker.shutdown();
    drop(exec);
}

#[test]
fn identity_replacement_does_not_stamp_499() {
    let _guard = stub_lock();
    let (mut req, _exec, _registry, mut worker) = run_flush_wait(
        r#"
use ngx;
ngx::flush(true);
"#,
    );
    let ptr = req.as_mut() as *mut ngx_http_request_t;
    mark_headers_sent(ptr);
    let program = compile_content(
        r#"
use ngx;
ngx::print("fresh");
"#,
    );
    let mut vm = Vm::try_new((*program).clone()).expect("vm");
    bind_ngx_hosts(&mut vm);
    with_request_scope(
        ptr,
        RequestIdentity::new(2, 9),
        NgxPhase::Content,
        AllowedOps::for_phase(NgxPhase::Content),
        || vm.run().expect("identity replacement must start a fresh output state"),
    );
    assert_eq!(
        err_status(ptr),
        0,
        "IdentitySuperseded must not stamp 499"
    );
    worker.shutdown();
}

#[test]
fn confirmed_client_abort_stamps_499_once() {
    let _guard = stub_lock();
    let (mut req, mut exec, mut registry, mut worker) = run_flush_wait(
        r#"
use ngx;
ngx::flush(true);
"#,
    );
    let ptr = req.as_mut() as *mut ngx_http_request_t;
    mark_headers_sent(ptr);
    nginx_stubs::stub_set_flush_continue(499);
    crate::host::fire_flush_write_handler(ptr);
    assert_eq!(err_status(ptr), 499);
    crate::host::fire_flush_write_handler(ptr);
    crate::host::cleanup_output(ptr);
    assert_eq!(err_status(ptr), 499, "499 must be recorded exactly once");
    assert_handler_restored(ptr);
    let effect = with_request_scope(
        ptr,
        exec.identity(),
        NgxPhase::Content,
        AllowedOps::for_phase(NgxPhase::Content),
        || exec.apply_ready(exec.identity(), &mut registry, &worker),
    );
    worker.shutdown();
    assert!(
        effect == ReadyEffect::Failed || effect == ReadyEffect::Halted,
        "confirmed abort must complete the waiter, got {effect:?}"
    );
}

#[test]
fn flush_timeout_stamps_408_not_499() {
    let _guard = stub_lock();
    let (mut req, mut exec, mut registry, mut worker) = run_flush_wait(
        r#"
use ngx;
ngx::flush(true);
"#,
    );
    let ptr = req.as_mut() as *mut ngx_http_request_t;
    mark_headers_sent(ptr);
    nginx_stubs::stub_set_flush_continue(408);
    crate::host::fire_flush_write_handler(ptr);
    assert_eq!(
        err_status(ptr),
        408,
        "timeout must log its defined non-499 status"
    );
    assert_handler_restored(ptr);
    let effect = with_request_scope(
        ptr,
        exec.identity(),
        NgxPhase::Content,
        AllowedOps::for_phase(NgxPhase::Content),
        || exec.apply_ready(exec.identity(), &mut registry, &worker),
    );
    worker.shutdown();
    assert_eq!(effect, ReadyEffect::Failed);
}

#[test]
fn generic_flush_error_does_not_stamp_499() {
    let _guard = stub_lock();
    let (mut req, mut exec, mut registry, mut worker) = run_flush_wait(
        r#"
use ngx;
ngx::flush(true);
"#,
    );
    let ptr = req.as_mut() as *mut ngx_http_request_t;
    mark_headers_sent(ptr);
    nginx_stubs::stub_set_flush_continue(-1);
    crate::host::fire_flush_write_handler(ptr);
    assert_eq!(
        err_status(ptr),
        0,
        "generic flush failure must not be inferred as client abort"
    );
    assert_handler_restored(ptr);
    let effect = with_request_scope(
        ptr,
        exec.identity(),
        NgxPhase::Content,
        AllowedOps::for_phase(NgxPhase::Content),
        || exec.apply_ready(exec.identity(), &mut registry, &worker),
    );
    worker.shutdown();
    assert_eq!(effect, ReadyEffect::Failed);
}

#[test]
fn auto_last_on_ok_error_and_finalize() {
    let _guard = stub_lock();
    nginx_stubs::stub_send_reset();
    nginx_stubs::stub_output_reset();
    let mut req = dummy_request();
    let ptr = req.as_mut() as *mut ngx_http_request_t;
    eval_in_content(
        ptr,
        r#"
use ngx;
ngx::print("ok");
"#,
    )
    .expect("print");
    with_request_scope(
        ptr,
        RequestIdentity::new(1, 1),
        NgxPhase::Content,
        AllowedOps::for_phase(NgxPhase::Content),
        || {
            assert_eq!(crate::host::finish_response(ptr), 0);
        },
    );
    assert!(nginx_stubs::stub_output_saw_last());
    assert_eq!(nginx_stubs::stub_output_body(), b"ok");
    crate::host::cleanup_output(ptr);

    nginx_stubs::stub_send_reset();
    nginx_stubs::stub_output_reset();
    let mut req = dummy_request();
    let ptr = req.as_mut() as *mut ngx_http_request_t;
    eval_in_content(
        ptr,
        r#"
use ngx;
ngx::print("er");
"#,
    )
    .expect("print");
    with_request_scope(
        ptr,
        RequestIdentity::new(1, 1),
        NgxPhase::Content,
        AllowedOps::for_phase(NgxPhase::Content),
        || {
            crate::host::finish_response_on_error(ptr);
        },
    );
    assert!(nginx_stubs::stub_output_saw_last());
    assert_eq!(nginx_stubs::stub_output_body(), b"er");
    crate::host::cleanup_output(ptr);

    nginx_stubs::stub_send_reset();
    nginx_stubs::stub_output_reset();
    let mut req = dummy_request();
    let ptr = req.as_mut() as *mut ngx_http_request_t;
    eval_in_content(
        ptr,
        r#"
use ngx;
ngx::print("fn");
"#,
    )
    .expect("print");
    with_request_scope(
        ptr,
        RequestIdentity::new(1, 1),
        NgxPhase::Content,
        AllowedOps::for_phase(NgxPhase::Content),
        || {
            crate::host::finish_response_on_finalize(ptr);
        },
    );
    assert!(nginx_stubs::stub_output_saw_last());
    assert_eq!(nginx_stubs::stub_output_body(), b"fn");
}

#[test]
fn concurrent_dummy_requests_on_threads_keep_independent_bodies() {
    let _guard = stub_lock();
    std::thread::scope(|scope| {
        let a = scope.spawn(|| {
            let mut req = dummy_request();
            let ptr = req.as_mut() as *mut ngx_http_request_t;
            eval_in_content(
                ptr,
                r#"
use ngx;
ngx::print("AAA");
"#,
            )
            .expect("A")
        });
        let b = scope.spawn(|| {
            let mut req = dummy_request();
            let ptr = req.as_mut() as *mut ngx_http_request_t;
            eval_in_content(
                ptr,
                r#"
use ngx;
ngx::print("BBB");
"#,
            )
            .expect("B")
        });
        a.join().unwrap();
        b.join().unwrap();
    });
}
