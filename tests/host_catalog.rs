//! Manifest, catalog, and config-time import validation for ngx hosts.
//!
//! Catalog metadata is handwritten. Runtime bindings stay macro-generated and
//! are checked through a real VM, not by scanning source strings.

use ngx_rustscript_nginx_test_stubs as _;

use std::collections::HashSet;
use std::sync::Arc;

use ngx_rustscript::config::{
    ConfigError, compile_rss_source, reject_program_imports, validate_program_imports_for_phase,
};
use ngx_rustscript::host::{
    HostCapability, HostFamily, HostSyncKind, NGX_CRC32_LONG_NAME, NGX_CRC32_SHORT_NAME,
    NGX_DECODE_ARGS_NAME, NGX_DECODE_BASE64_NAME, NGX_DECODE_BASE64MIME_NAME, NGX_ENCODE_ARGS_NAME,
    NGX_ENCODE_BASE64_NAME, NGX_ESCAPE_URI_NAME, NGX_HEADER_GET_NAME, NGX_HMAC_SHA1_NAME,
    NGX_MD5_BIN_NAME, NGX_MD5_NAME, NGX_NOW_NAME, NGX_QUOTE_SQL_STR_NAME, NGX_SHA1_BIN_NAME,
    NGX_SLEEP_MS_NAME, NGX_SLEEP_NAME, NGX_STATUS_GET_NAME, NGX_TIME_NAME, NGX_UNESCAPE_URI_NAME,
    NGX_UPDATE_TIME_NAME, NgxPhase, bind_ngx_hosts, host_allowed_in_phase, ngx_host_catalog,
    ngx_host_manifest, production_runtime_host_names,
};
use vm::{HostTypeSchema, Program, Vm};

fn compile(source: &str) -> Arc<Program> {
    compile_rss_source(source).expect("source should compile against the ngx catalog")
}

fn compile_err(source: &str) -> ConfigError {
    compile_rss_source(source).expect_err("source should fail configuration")
}

fn error_text(err: &ConfigError) -> String {
    err.to_string().to_ascii_lowercase()
}

#[test]
fn runtime_host_names_are_unique() {
    let names = production_runtime_host_names();
    let mut seen = HashSet::new();
    for name in names {
        assert!(
            seen.insert(*name),
            "production runtime host name {name} is duplicated"
        );
    }
    assert!(
        !names.is_empty(),
        "production runtime must bind at least one ngx host"
    );
    assert_eq!(
        names.len(),
        191,
        "production runtime host count must stay 191"
    );
}

#[test]
fn manifest_catalog_and_runtime_names_correspond() {
    let catalog = ngx_host_catalog();
    let manifest = ngx_host_manifest();
    let runtime_names = production_runtime_host_names();

    let catalog_names: Vec<&str> = catalog
        .functions()
        .iter()
        .map(|schema| schema.name.as_str())
        .collect();
    let manifest_names: Vec<&str> = manifest.iter().map(|entry| entry.name).collect();

    assert_eq!(
        catalog_names.as_slice(),
        runtime_names,
        "compiler catalog names and order must match runtime bindings exactly"
    );
    assert_eq!(
        manifest_names.as_slice(),
        runtime_names,
        "manifest names and order must match runtime bindings exactly"
    );
    assert_eq!(
        catalog.functions().len(),
        runtime_names.len(),
        "catalog must not declare overloads for production ngx hosts"
    );
}

#[test]
fn each_manifest_entry_has_phase_sync_capability_and_openresty_name() {
    for entry in ngx_host_manifest() {
        assert!(!entry.name.is_empty(), "manifest entry needs a host name");
        assert!(
            !entry.phases.is_empty(),
            "{} must declare allowed phases",
            entry.name
        );
        let is_output = matches!(
            entry.name,
            "ngx::send_headers" | "ngx::print" | "ngx::say" | "ngx::flush" | "ngx::eof"
        );
        if is_output {
            assert!(
                entry.phases.contains(&NgxPhase::Content),
                "{} must be allowed in content",
                entry.name
            );
            assert!(
                !entry.phases.contains(&NgxPhase::Access),
                "{} must be rejected in access at config time",
                entry.name
            );
        } else {
            assert!(
                entry.phases.contains(&NgxPhase::Access),
                "{} must be allowed in the current access phase",
                entry.name
            );
        }
        assert!(
            !entry.capabilities.is_empty(),
            "{} must declare capabilities",
            entry.name
        );
        assert!(
            entry.openresty_name.starts_with("ngx."),
            "{} must name an OpenResty ngx.* API, got {}",
            entry.name,
            entry.openresty_name
        );
        match entry.family {
            HostFamily::Timer => {
                assert_eq!(entry.sync, HostSyncKind::Async);
                assert!(
                    entry.capabilities.contains(&HostCapability::Timer),
                    "{} is a timer host",
                    entry.name
                );
                assert_eq!(entry.openresty_name, "ngx.sleep");
            }
            HostFamily::Codec => {
                assert_eq!(entry.sync, HostSyncKind::Sync);
                assert!(
                    entry.capabilities.contains(&HostCapability::Codec),
                    "{} is a codec host",
                    entry.name
                );
            }
            HostFamily::Time => {
                assert_eq!(entry.sync, HostSyncKind::Sync);
                assert!(
                    entry.capabilities.contains(&HostCapability::Time),
                    "{} is a time host",
                    entry.name
                );
            }
            HostFamily::Meta => {
                assert_eq!(entry.sync, HostSyncKind::Sync);
                assert!(
                    entry.capabilities.contains(&HostCapability::Meta),
                    "{} is a meta host",
                    entry.name
                );
            }
            HostFamily::Log => {
                assert_eq!(entry.sync, HostSyncKind::Sync);
                assert!(
                    entry.capabilities.contains(&HostCapability::Log),
                    "{} is a log host",
                    entry.name
                );
            }
            HostFamily::Constant => {
                assert_eq!(entry.sync, HostSyncKind::Sync);
                assert!(
                    entry.capabilities.contains(&HostCapability::Constant),
                    "{} is a constant host",
                    entry.name
                );
            }
            HostFamily::Request => {
                if entry.name == "ngx::req::read_body" {
                    assert_eq!(entry.sync, HostSyncKind::Async);
                } else {
                    assert_eq!(entry.sync, HostSyncKind::Sync);
                }
                assert!(
                    entry.capabilities.contains(&HostCapability::RequestRead)
                        || entry.capabilities.contains(&HostCapability::RequestWrite),
                    "{} request hosts require RequestRead or RequestWrite",
                    entry.name
                );
            }
            HostFamily::RequestCtx => {
                assert_eq!(entry.sync, HostSyncKind::Sync);
                assert_eq!(entry.capabilities, &[HostCapability::RequestCtx]);
                assert_eq!(entry.openresty_name, "ngx.ctx");
            }
            HostFamily::Response => {
                if entry.name == "ngx::flush" {
                    assert_eq!(entry.sync, HostSyncKind::Async);
                } else {
                    assert_eq!(entry.sync, HostSyncKind::Sync);
                }
                if entry.name == NGX_STATUS_GET_NAME || entry.name == NGX_HEADER_GET_NAME {
                    assert_eq!(entry.capabilities, &[HostCapability::ResponseRead]);
                } else {
                    assert_eq!(entry.capabilities, &[HostCapability::ResponseWrite]);
                }
            }
            HostFamily::Control => {
                assert_eq!(entry.sync, HostSyncKind::Sync);
                assert_eq!(entry.capabilities, &[HostCapability::Control]);
            }
            HostFamily::SharedDict => {
                assert_eq!(entry.sync, HostSyncKind::Sync);
                assert_eq!(entry.capabilities, &[HostCapability::SharedDict]);
                assert!(
                    entry.openresty_name.starts_with("ngx.shared.DICT"),
                    "{} openresty name {}",
                    entry.name,
                    entry.openresty_name
                );
            }
            HostFamily::Cosocket => {
                assert_eq!(entry.capabilities, &[HostCapability::Cosocket]);
                assert!(
                    entry.openresty_name.starts_with("ngx.socket."),
                    "{} Cosocket hosts must name ngx.socket.*, got {}",
                    entry.name,
                    entry.openresty_name
                );
            }
        }
    }
}

#[test]
fn catalog_sleep_schema_is_openresty_seconds_number() {
    let catalog = ngx_host_catalog();
    let schema = catalog
        .function(NGX_SLEEP_NAME)
        .expect("ngx::sleep is catalogued");
    assert_eq!(schema.name, NGX_SLEEP_NAME);
    assert_eq!(schema.params.len(), 1);
    assert_eq!(schema.params[0].name, "seconds");
    assert_eq!(schema.params[0].ty, HostTypeSchema::Number);
    assert_eq!(schema.return_type, HostTypeSchema::Null);
}

#[test]
fn catalog_sleep_ms_schema_preserves_millisecond_int() {
    let catalog = ngx_host_catalog();
    let schema = catalog
        .function(NGX_SLEEP_MS_NAME)
        .expect("ngx::sleep_ms is catalogued");
    assert_eq!(schema.name, NGX_SLEEP_MS_NAME);
    assert_eq!(schema.params.len(), 1);
    assert_eq!(schema.params[0].name, "ms");
    assert_eq!(schema.params[0].ty, HostTypeSchema::Int);
    assert_eq!(schema.return_type, HostTypeSchema::Null);
}

#[test]
fn bound_vm_has_every_catalogued_runtime_host_and_no_test_hosts() {
    let program = compile("let x = 1;\n");
    let mut vm = Vm::try_new((*program).clone()).expect("construct vm");
    bind_ngx_hosts(&mut vm);
    for name in production_runtime_host_names() {
        assert!(
            vm.has_bound_function(name),
            "runtime VM must bind catalogued host {name}"
        );
    }
    for banned in [
        "ngx::probe",
        "ngx::fail",
        "ngx::ready",
        "print",
        "runtime::sleep",
    ] {
        assert!(
            !vm.has_bound_function(banned),
            "production bind must not install {banned}"
        );
    }
    assert!(
        vm.has_bound_function(NGX_NOW_NAME),
        "production bind must install ngx::now as the time host"
    );
}

#[test]
fn catalog_does_not_include_test_hosts() {
    let catalog = ngx_host_catalog();
    assert!(catalog.function("ngx::probe").is_none());
    assert!(catalog.function("ngx::fail").is_none());
    assert!(catalog.function("ngx::ready").is_none());
    assert!(
        catalog.function(NGX_NOW_NAME).is_some(),
        "production catalog must keep ngx::now"
    );
}

#[test]
fn unknown_import_is_distinct_from_arity_and_type_errors() {
    let err = compile_err("use ngx;\nngx::not_a_host(1);\n");
    let text = error_text(&err);
    assert!(
        matches!(err, ConfigError::UnsupportedImport(_)),
        "unknown ngx member is rejected by the catalog allowlist, got {err:?}"
    );
    assert!(
        text.contains("ngx::not_a_host"),
        "unknown import should name the rejected host, got {err}"
    );
    assert!(
        !text.contains("unknown host function"),
        "pinned compiler accepts an undeclared ngx member as an import; allowlist rejection is distinct: {err}"
    );
    assert!(
        !text.contains("call site"),
        "unknown import must not be reported as arity mismatch: {err}"
    );
    assert!(
        !text.contains("matches the arguments"),
        "unknown import must not be reported as type mismatch: {err}"
    );
}

#[test]
fn arity_mismatch_is_distinct_from_unknown_and_type_errors() {
    let err = compile_err("use ngx;\nngx::sleep();\n");
    let text = error_text(&err);
    assert!(
        matches!(err, ConfigError::Compile(_)),
        "wrong arity should fail in the compiler, got {err:?}"
    );
    assert!(
        text.contains("ngx::sleep") && (text.contains("argument") || text.contains("arity")),
        "arity mismatch should name the host and argument count, got {err}"
    );
    assert!(
        !text.contains("unknown host function"),
        "arity mismatch must not be reported as unknown import: {err}"
    );
}

#[test]
fn type_mismatch_is_distinct_from_unknown_and_arity_errors() {
    let err = compile_err("use ngx;\nngx::sleep(\"nope\");\n");
    let text = error_text(&err);
    assert!(
        matches!(err, ConfigError::Compile(_)),
        "type mismatch should fail in the compiler, got {err:?}"
    );
    assert!(
        text.contains("ngx::sleep")
            && (text.contains("match") || text.contains("type") || text.contains("string")),
        "type mismatch should mention the host and the bad argument, got {err}"
    );
    assert!(
        !text.contains("unknown host function"),
        "type mismatch must not be reported as unknown import: {err}"
    );
    assert!(
        !(text.contains("takes ") && text.contains("call site")),
        "type mismatch must not be reported as arity mismatch: {err}"
    );
}

#[test]
fn phase_forbidden_import_is_distinct_from_compiler_diagnostics() {
    let program = compile("use ngx;\nngx::sleep(0);\n");
    let err = validate_program_imports_for_phase(&program, NgxPhase::Log)
        .expect_err("ngx::sleep is not allowed in log");
    let text = error_text(&err);
    assert!(
        !matches!(err, ConfigError::Compile(_)),
        "phase denial is config metadata, not a compiler catalog miss: {err:?}"
    );
    assert!(
        text.contains("ngx::sleep") && text.contains("log"),
        "phase-forbidden error should name the import and phase, got {err}"
    );
    assert!(
        text.contains("not allowed") || text.contains("forbidden") || text.contains("phase"),
        "phase-forbidden error should say the import is not allowed, got {err}"
    );
    assert!(
        !text.contains("unknown host function"),
        "phase-forbidden must not look like an unknown import: {err}"
    );
}

#[test]
fn access_phase_accepts_sleep_and_sleep_ms() {
    compile("use ngx;\nngx::sleep(0);\n");
    compile("use ngx;\nngx::sleep_ms(1);\n");
    let program = compile("use ngx;\nngx::sleep(0);\nngx::sleep_ms(1);\n");
    reject_program_imports(&program).expect("access phase allows timer hosts");
    validate_program_imports_for_phase(&program, NgxPhase::Access)
        .expect("access phase allows timer hosts");
}

#[test]
fn codec_and_time_phase_metadata_matches_openresty() {
    assert!(host_allowed_in_phase(
        NGX_ESCAPE_URI_NAME,
        NgxPhase::InitWorker
    ));
    assert!(host_allowed_in_phase(NGX_ESCAPE_URI_NAME, NgxPhase::Init));
    assert!(host_allowed_in_phase(
        NGX_UNESCAPE_URI_NAME,
        NgxPhase::InitWorker
    ));
    assert!(!host_allowed_in_phase(
        NGX_UNESCAPE_URI_NAME,
        NgxPhase::SslSessionFetch
    ));
    assert!(!host_allowed_in_phase(
        NGX_ENCODE_ARGS_NAME,
        NgxPhase::InitWorker
    ));
    assert!(!host_allowed_in_phase(NGX_ENCODE_ARGS_NAME, NgxPhase::Init));
    assert!(host_allowed_in_phase(NGX_ENCODE_ARGS_NAME, NgxPhase::Set));
    assert!(!host_allowed_in_phase(
        NGX_DECODE_ARGS_NAME,
        NgxPhase::InitWorker
    ));
    assert!(host_allowed_in_phase(
        NGX_DECODE_ARGS_NAME,
        NgxPhase::SslSessionFetch
    ));
    assert!(!host_allowed_in_phase(
        NGX_DECODE_BASE64MIME_NAME,
        NgxPhase::InitWorker
    ));
    assert!(!host_allowed_in_phase(
        NGX_DECODE_BASE64MIME_NAME,
        NgxPhase::SslClientHello
    ));
    assert!(host_allowed_in_phase(NGX_TIME_NAME, NgxPhase::InitWorker));
    assert!(host_allowed_in_phase(NGX_NOW_NAME, NgxPhase::InitWorker));
    assert!(host_allowed_in_phase(
        NGX_UPDATE_TIME_NAME,
        NgxPhase::InitWorker
    ));
    assert!(host_allowed_in_phase(NGX_TIME_NAME, NgxPhase::Timer));
}

#[test]
fn codec_misc_hosts_allow_ssl_client_hello_except_decode_base64mime() {
    // OpenResty v0.10.29 allows ssl_client_hello for these codec APIs.
    // decode_base64mime is the documented exception and must not share this list.
    const SSL_CLIENT_HELLO_CODEC: &[&str] = &[
        NGX_ENCODE_BASE64_NAME,
        NGX_DECODE_BASE64_NAME,
        NGX_CRC32_SHORT_NAME,
        NGX_CRC32_LONG_NAME,
        NGX_HMAC_SHA1_NAME,
        NGX_MD5_NAME,
        NGX_MD5_BIN_NAME,
        NGX_SHA1_BIN_NAME,
        NGX_QUOTE_SQL_STR_NAME,
    ];
    for name in SSL_CLIENT_HELLO_CODEC {
        assert!(
            host_allowed_in_phase(name, NgxPhase::SslClientHello),
            "{name} must be allowed in ssl_client_hello"
        );
        assert!(
            host_allowed_in_phase(name, NgxPhase::SslSessionFetch),
            "{name} must keep ssl_session_fetch"
        );
        assert!(
            !host_allowed_in_phase(name, NgxPhase::InitWorker),
            "{name} must not gain init_worker from a shared codec list"
        );
        assert!(
            !host_allowed_in_phase(name, NgxPhase::Init),
            "{name} must not gain init from a shared codec list"
        );
        assert!(
            !host_allowed_in_phase(name, NgxPhase::ExitWorker),
            "{name} must not gain exit_worker from a shared codec list"
        );
    }
    assert!(
        !host_allowed_in_phase(NGX_DECODE_BASE64MIME_NAME, NgxPhase::SslClientHello),
        "ngx::decode_base64mime must remain disallowed in ssl_client_hello"
    );
    assert!(host_allowed_in_phase(
        NGX_DECODE_BASE64MIME_NAME,
        NgxPhase::SslSessionFetch
    ));
    assert!(!host_allowed_in_phase(
        NGX_DECODE_BASE64MIME_NAME,
        NgxPhase::InitWorker
    ));
}

#[test]
fn nginx_version_allows_init_worker_and_exit_worker_unlike_nginx_configure() {
    assert!(host_allowed_in_phase(
        "ngx::config::nginx_version",
        NgxPhase::InitWorker
    ));
    assert!(host_allowed_in_phase(
        "ngx::config::nginx_version",
        NgxPhase::ExitWorker
    ));
    assert!(host_allowed_in_phase(
        "ngx::config::nginx_version",
        NgxPhase::Init
    ));
    assert!(!host_allowed_in_phase(
        "ngx::config::nginx_configure",
        NgxPhase::InitWorker
    ));
    assert!(!host_allowed_in_phase(
        "ngx::config::nginx_configure",
        NgxPhase::ExitWorker
    ));
    assert!(host_allowed_in_phase(
        "ngx::config::nginx_configure",
        NgxPhase::Init
    ));
}
