//! Catalog, config-time phase, and stub-level tests for response output hosts.

use ngx_rustscript_nginx_test_stubs as nginx_stubs;

use std::mem::MaybeUninit;
use std::sync::Arc;

use nginx_sys::ngx_http_request_t;
use ngx_rustscript::config::{
    ConfigError, compile_rss_source, compile_rss_source_for_phase, program_requires_request_state,
    validate_program_imports_for_phase,
};
use ngx_rustscript::host::{
    AllowedOps, HostCapability, HostFamily, HostSyncKind, NgxPhase, bind_registry_for_test,
    host_allowed_in_phase, manifest_entry, ngx_host_catalog, with_request_scope,
};
use ngx_rustscript::notify::RequestIdentity;
use vm::{HostTypeSchema, Program, Value, Vm, VmStatus};

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
    bind_registry_for_test(&mut vm).expect("bind test registry");
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

const NGX_SEND_HEADERS_NAME: &str = "ngx::send_headers";
const NGX_PRINT_NAME: &str = "ngx::print";
const NGX_SAY_NAME: &str = "ngx::say";
const NGX_FLUSH_NAME: &str = "ngx::flush";
const NGX_EOF_NAME: &str = "ngx::eof";

const OUTPUT_HOSTS: &[&str] = &[
    NGX_SEND_HEADERS_NAME,
    NGX_PRINT_NAME,
    NGX_SAY_NAME,
    NGX_FLUSH_NAME,
    NGX_EOF_NAME,
];

#[test]
fn output_hosts_are_content_only_response_write() {
    for name in OUTPUT_HOSTS {
        let entry =
            manifest_entry(name).unwrap_or_else(|| panic!("{name} must be in the manifest"));
        assert_eq!(entry.family, HostFamily::Response);
        assert_eq!(entry.capabilities, &[HostCapability::ResponseWrite]);
        assert!(
            entry.phases.contains(&NgxPhase::Content),
            "{name} must be allowed in content"
        );
        assert!(
            !entry.phases.contains(&NgxPhase::Access),
            "{name} must be rejected in access"
        );
        assert!(
            !entry.phases.contains(&NgxPhase::Init),
            "{name} must be rejected in init"
        );
        assert!(host_allowed_in_phase(name, NgxPhase::Content));
        assert!(!host_allowed_in_phase(name, NgxPhase::Access));
        assert!(!host_allowed_in_phase(name, NgxPhase::Rewrite));
    }
    let flush = manifest_entry(NGX_FLUSH_NAME).expect("flush");
    assert_eq!(flush.sync, HostSyncKind::Async);
    assert_eq!(flush.openresty_name, "ngx.flush");
    assert_eq!(
        manifest_entry(NGX_SEND_HEADERS_NAME)
            .expect("send_headers")
            .sync,
        HostSyncKind::Sync
    );
    assert_eq!(
        manifest_entry(NGX_SEND_HEADERS_NAME)
            .expect("send_headers")
            .openresty_name,
        "ngx.send_headers"
    );
    assert_eq!(
        manifest_entry(NGX_PRINT_NAME)
            .expect("print")
            .openresty_name,
        "ngx.print"
    );
    assert_eq!(
        manifest_entry(NGX_SAY_NAME).expect("say").openresty_name,
        "ngx.say"
    );
    assert_eq!(
        manifest_entry(NGX_EOF_NAME).expect("eof").openresty_name,
        "ngx.eof"
    );
}

#[test]
fn output_catalog_signatures_match_openresty_names() {
    let catalog = ngx_host_catalog();
    let send = catalog
        .function(NGX_SEND_HEADERS_NAME)
        .expect("send_headers schema");
    assert!(send.params.is_empty());
    assert_eq!(send.return_type, HostTypeSchema::Null);

    let print = catalog.function(NGX_PRINT_NAME).expect("print schema");
    assert_eq!(print.params.len(), 1);
    assert_eq!(print.params[0].name, "data");
    assert_eq!(print.params[0].ty, HostTypeSchema::Unknown);
    assert_eq!(print.return_type, HostTypeSchema::Null);

    let say = catalog.function(NGX_SAY_NAME).expect("say schema");
    assert_eq!(say.params.len(), 1);
    assert_eq!(say.params[0].name, "data");
    assert_eq!(say.params[0].ty, HostTypeSchema::Unknown);
    assert_eq!(say.return_type, HostTypeSchema::Null);

    let flush = catalog.function(NGX_FLUSH_NAME).expect("flush schema");
    assert_eq!(flush.params.len(), 1);
    assert_eq!(flush.params[0].name, "wait");
    assert_eq!(flush.params[0].ty, HostTypeSchema::Bool);
    assert_eq!(flush.return_type, HostTypeSchema::Null);

    let eof = catalog.function(NGX_EOF_NAME).expect("eof schema");
    assert!(eof.params.is_empty());
    assert_eq!(eof.return_type, HostTypeSchema::Null);
}

#[test]
fn content_phase_accepts_output_hosts_and_requires_request_state() {
    for source in [
        "use ngx;\nngx::send_headers();\n",
        "use ngx;\nngx::print(\"x\");\n",
        "use ngx;\nngx::say(\"x\");\n",
        "use ngx;\nngx::flush(false);\n",
        "use ngx;\nngx::eof();\n",
    ] {
        let program = compile_content(source);
        assert!(
            program_requires_request_state(&program),
            "output hosts must take the request-owned path: {source}"
        );
        validate_program_imports_for_phase(&program, NgxPhase::Content)
            .expect("content allows output hosts");
    }
}

#[test]
fn access_and_init_reject_output_hosts_as_phase_forbidden() {
    for name in OUTPUT_HOSTS {
        let source = match *name {
            NGX_PRINT_NAME | NGX_SAY_NAME => format!("use ngx;\n{name}(\"x\");\n"),
            NGX_FLUSH_NAME => format!("use ngx;\n{name}(false);\n"),
            _ => format!("use ngx;\n{name}();\n"),
        };
        let err = compile_rss_source(&source)
            .expect_err("access must reject output hosts at config time");
        match err {
            ConfigError::PhaseForbidden {
                name: got,
                phase: NgxPhase::Access,
            } => assert_eq!(got, *name),
            other => panic!("expected PhaseForbidden in access for {name}, got {other:?}"),
        }
        let compiled = compile_content(&source);
        let err = validate_program_imports_for_phase(&compiled, NgxPhase::Init)
            .expect_err("init must reject output hosts");
        match err {
            ConfigError::PhaseForbidden {
                name: got,
                phase: NgxPhase::Init,
            } => assert_eq!(got, *name),
            other => panic!("expected PhaseForbidden in init for {name}, got {other:?}"),
        }
    }
}

mod support;

#[test]
fn output_hosts_reject_missing_module_ctx() {
    nginx_stubs::stub_send_reset();
    nginx_stubs::stub_output_reset();
    let mut req = dummy_request();
    let ptr = req.as_mut() as *mut ngx_http_request_t;
    let err = eval_in_content(
        ptr,
        r#"
use ngx;
ngx::print("x");
"#,
    )
    .expect_err("production path must fail closed without module ctx");
    let text = err.to_ascii_lowercase();
    assert!(
        text.contains("module ctx"),
        "missing ctx must fail closed, got {err}"
    );
    assert_eq!(nginx_stubs::stub_output_body(), b"");
}

#[test]
fn production_output_source_gates_fallback_behind_rustc_test() {
    let src = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/host/output.rs"));
    assert!(
        src.contains(r#"#[cfg(any(test, ngx_features = "rustc-test"))]"#),
        "FALLBACK_OUTPUT must be compiled only under test/rustc-test cfg"
    );
    assert!(
        src.contains("static FALLBACK_OUTPUT"),
        "stub fallback map must remain named FALLBACK_OUTPUT under the test cfg"
    );
    assert!(
        src.contains("ngx output requires request module ctx"),
        "production missing-ctx path must fail closed"
    );
    assert!(
        src.contains("never store pointer-keyed output state"),
        "production path must not store pointer-keyed fallback state"
    );
    let cfg_idx = src
        .find(r#"#[cfg(any(test, ngx_features = "rustc-test"))]"#)
        .expect("cfg gate");
    let map_idx = src.find("static FALLBACK_OUTPUT").expect("fallback map");
    assert!(cfg_idx < map_idx, "cfg gate must precede FALLBACK_OUTPUT");
}

#[test]
fn production_output_cfg_fail_closed_is_not_test_only() {
    let src = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/host/output.rs"));
    assert!(
        src.contains(r#"#[cfg(not(any(test, ngx_features = "rustc-test")))]"#),
        "production fail-closed branch must compile when rustc-test/test cfg is off"
    );
}

#[test]
fn production_cdylib_cannot_contain_or_reach_fallback_output() {
    let module = support::artifact::module_cdylib();
    let bytes = std::fs::read(&module)
        .unwrap_or_else(|err| panic!("read production cdylib {}: {err}", module.display()));
    let needle = b"FALLBACK_OUTPUT";
    assert!(
        !bytes.windows(needle.len()).any(|window| window == needle),
        "production cdylib {} must not contain FALLBACK_OUTPUT",
        module.display()
    );
}
