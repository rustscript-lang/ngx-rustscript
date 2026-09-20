//! Request bitfield ABI tests. Fields are set through C compiled against
//! official NGINX headers, never through nginx-sys bindgen setters.

use std::mem::MaybeUninit;
use std::process::Command;
use std::sync::Arc;

use nginx_sys::{ngx_http_request_body_t, ngx_http_request_t};
use ngx_rustscript::config::compile_rss_source;
use ngx_rustscript::host::{AllowedOps, NgxPhase, bind_ngx_hosts, with_request_scope};
use ngx_rustscript::notify::RequestIdentity;
use ngx_rustscript_nginx_test_stubs as nginx_stubs;
use vm::{Program, Value, Vm, VmStatus};

fn compile(source: &str) -> Arc<Program> {
    compile_rss_source(source).unwrap_or_else(|err| panic!("compile failed: {err}"))
}

fn eval_in_request(request: *mut ngx_http_request_t, source: &str) -> Result<Value, String> {
    let program = compile(source);
    let mut vm = Vm::try_new((*program).clone()).expect("construct vm");
    bind_ngx_hosts(&mut vm);
    with_request_scope(
        request,
        RequestIdentity::new(1, 1),
        NgxPhase::Access,
        AllowedOps::for_phase(NgxPhase::Access),
        || match vm.run() {
            Ok(VmStatus::Halted) => Ok(vm
                .stack()
                .last()
                .cloned()
                .unwrap_or_else(|| panic!("expected a stack value for {source}"))),
            Ok(status) => Err(format!("unexpected status {status:?}")),
            Err(err) => Err(err.to_string()),
        },
    )
}

fn expect_bool(value: Value) -> bool {
    match value {
        Value::Bool(flag) => flag,
        other => panic!("expected bool, got {other:?}"),
    }
}

fn install_unread_body(req: *mut ngx_http_request_t) -> Box<ngx_http_request_body_t> {
    let mut body =
        Box::new(unsafe { MaybeUninit::<ngx_http_request_body_t>::zeroed().assume_init() });
    body.temp_file = std::ptr::null_mut();
    body.bufs = std::ptr::null_mut();
    unsafe {
        (*req).request_body = body.as_mut() as *mut ngx_http_request_body_t;
    }
    body
}

#[test]
fn c_request_sizeof_matches_selected_configured_nginx_headers() {
    nginx_stubs::force_link();
    assert_eq!(
        nginx_stubs::request_sizeof(),
        std::mem::size_of::<ngx_http_request_t>(),
        "C helpers and nginx-sys bindgen must use the same selected generated headers"
    );
}

#[test]
fn c_internal_is_independent_of_filter_finalize_and_neighbors() {
    nginx_stubs::force_link();
    let mut req = nginx_stubs::AbiRequest::new();
    let ptr = req.as_ptr() as *mut ngx_http_request_t;

    assert!(!expect_bool(
        eval_in_request(ptr, "use ngx;\nngx::req::is_internal();\n").expect("zeroed")
    ));

    req.set_filter_finalize(true);
    assert!(
        !expect_bool(eval_in_request(ptr, "use ngx;\nngx::req::is_internal();\n").expect("ff")),
        "C filter_finalize must not make is_internal true"
    );
    req.set_filter_finalize(false);

    req.set_error_page(true);
    assert!(
        !expect_bool(eval_in_request(ptr, "use ngx;\nngx::req::is_internal();\n").expect("ep")),
        "C error_page must not make is_internal true"
    );
    req.set_error_page(false);

    req.set_discard_body(true);
    assert!(
        !expect_bool(
            eval_in_request(ptr, "use ngx;\nngx::req::is_internal();\n").expect("discard")
        ),
        "C discard_body (bindgen internal slot) must not make is_internal true"
    );
    req.set_discard_body(false);

    req.set_internal(true);
    assert!(expect_bool(
        eval_in_request(ptr, "use ngx;\nngx::req::is_internal();\n").expect("internal")
    ));
}

#[test]
fn c_headers_sent_reading_body_and_discard_body_follow_c_fields() {
    nginx_stubs::force_link();
    let mut req = nginx_stubs::AbiRequest::new();
    let ptr = req.as_ptr() as *mut ngx_http_request_t;

    assert!(!expect_bool(
        eval_in_request(ptr, "use ngx;\nngx::headers_sent();\n").expect("sent false")
    ));

    req.set_header_sent(true);
    assert!(expect_bool(
        eval_in_request(ptr, "use ngx;\nngx::headers_sent();\n").expect("sent true")
    ));
    req.set_header_sent(false);
    assert!(!expect_bool(
        eval_in_request(ptr, "use ngx;\nngx::headers_sent();\n").expect("sent reset")
    ));

    let _body = install_unread_body(ptr);
    req.set_reading_body(true);
    let err = eval_in_request(ptr, "use ngx;\nngx::req::get_post_args(null);\n")
        .expect_err("reading_body must error");
    assert!(
        err.to_ascii_lowercase().contains("read") || err.to_ascii_lowercase().contains("body"),
        "got {err}"
    );
    req.set_reading_body(false);

    req.set_discard_body(true);
    let err = eval_in_request(ptr, "use ngx;\nngx::req::get_post_args(null);\n")
        .expect_err("discard_body must error");
    assert!(
        err.to_ascii_lowercase().contains("discard") || err.to_ascii_lowercase().contains("body"),
        "got {err}"
    );
    assert!(!expect_bool(
        eval_in_request(ptr, "use ngx;\nngx::req::is_internal();\n").expect("discard not internal")
    ));
}

unsafe extern "C" {
    fn ngx_rustscript_request_is_internal(r: *const ngx_http_request_t) -> u32;
    fn ngx_rustscript_request_headers_sent(r: *const ngx_http_request_t) -> u32;
    fn ngx_rustscript_request_reading_body(r: *const ngx_http_request_t) -> u32;
    fn ngx_rustscript_request_discard_body(r: *const ngx_http_request_t) -> u32;
}

#[test]
fn shims_follow_c_fields_and_reject_null() {
    nginx_stubs::force_link();
    unsafe {
        assert_eq!(ngx_rustscript_request_is_internal(std::ptr::null()), 0);
        assert_eq!(ngx_rustscript_request_headers_sent(std::ptr::null()), 0);
        assert_eq!(ngx_rustscript_request_reading_body(std::ptr::null()), 0);
        assert_eq!(ngx_rustscript_request_discard_body(std::ptr::null()), 0);
    }

    let mut req = nginx_stubs::AbiRequest::new();
    let ptr = req.as_ptr() as *const ngx_http_request_t;
    unsafe {
        assert_eq!(ngx_rustscript_request_is_internal(ptr), 0);
        req.set_filter_finalize(true);
        assert_eq!(
            ngx_rustscript_request_is_internal(ptr),
            0,
            "shim internal must not follow C filter_finalize"
        );
        req.set_filter_finalize(false);
        req.set_internal(true);
        assert_eq!(ngx_rustscript_request_is_internal(ptr), 1);
        req.set_internal(false);
        req.set_header_sent(true);
        assert_eq!(ngx_rustscript_request_headers_sent(ptr), 1);
        assert_eq!(ngx_rustscript_request_is_internal(ptr), 0);
        req.set_header_sent(false);
        req.set_reading_body(true);
        assert_eq!(ngx_rustscript_request_reading_body(ptr), 1);
        req.set_reading_body(false);
        req.set_discard_body(true);
        assert_eq!(ngx_rustscript_request_discard_body(ptr), 1);
        assert_eq!(ngx_rustscript_request_is_internal(ptr), 0);
    }
}

fn compile_and_run_c_probe(cc: &str) {
    use std::path::PathBuf;

    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let shim = manifest.join("src/nginx_request_shim.c");
    let probe = manifest.join("tests/nginx_request_abi_probe.c");
    let include = env!("DEP_NGINX_INCLUDE");
    let cflags = env!("DEP_NGINX_CFLAGS");
    let tmp = std::env::var_os("NGX_RUSTSCRIPT_TASK_TMP")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let out = tmp.join(format!("ngx-request-abi-probe-{cc}-{}", std::process::id()));
    let mut cmd = Command::new(cc);
    cmd.arg("-o").arg(&out);
    for path in std::env::split_paths(include) {
        cmd.arg("-I").arg(path);
    }
    for flag in cflags.split_whitespace() {
        cmd.arg(flag);
    }
    cmd.arg("-DNGX_RUSTSCRIPT_ABI_PROBE");
    cmd.arg(&shim).arg(&probe);
    let compile = cmd
        .output()
        .unwrap_or_else(|err| panic!("{cc} missing: {err}"));
    assert!(
        compile.status.success(),
        "{cc} compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let run = Command::new(&out)
        .output()
        .unwrap_or_else(|err| panic!("run {cc} probe: {err}"));
    let _ = std::fs::remove_file(&out);
    assert!(
        run.status.success(),
        "{cc} probe failed: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "ok");
}

#[test]
fn c_abi_probe_gcc_and_clang_agree() {
    compile_and_run_c_probe("gcc");
    if Command::new("clang")
        .arg("--version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
    {
        compile_and_run_c_probe("clang");
    }
}
