//! Manifest, catalog, and config-time import validation for ngx hosts.
//!
//! Catalog metadata is handwritten. Runtime bindings stay macro-generated and
//! are checked through a real VM, not by scanning source strings.

use ngx_rustscript_nginx_test_stubs as _;

use std::collections::{BTreeSet, HashSet};
use std::sync::Arc;

use ngx_rustscript::config::{
    ConfigError, compile_rss_source, compile_rss_source_for_phase, program_requires_request_state,
    reject_program_imports, validate_program_imports_for_phase,
};
use ngx_rustscript::host::{
    HostCapability, HostFamily, HostSyncKind, NGX_CRC32_LONG_NAME, NGX_CRC32_SHORT_NAME,
    NGX_DECODE_ARGS_NAME, NGX_DECODE_BASE64_NAME, NGX_DECODE_BASE64MIME_NAME, NGX_ENCODE_ARGS_NAME,
    NGX_ENCODE_BASE64_NAME, NGX_ESCAPE_URI_NAME, NGX_HEADER_GET_NAME, NGX_HMAC_SHA1_NAME,
    NGX_HOST_CATALOG_FINGERPRINT, NGX_HOST_CATALOG_FINGERPRINT_V1, NGX_HOST_CATALOG_FINGERPRINT_V2,
    NGX_HOST_CATALOG_SCHEMA_VERSION, NGX_MD5_BIN_NAME, NGX_MD5_NAME, NGX_NOW_NAME,
    NGX_QUOTE_SQL_STR_NAME, NGX_SHA1_BIN_NAME, NGX_SLEEP_MS_NAME, NGX_SLEEP_NAME,
    NGX_STATUS_GET_NAME, NGX_TIME_NAME, NGX_UNESCAPE_URI_NAME, NGX_UPDATE_TIME_NAME, NgxPhase,
    StandardHostPolicy, bind_ngx_hosts, compile_host_catalog, host_allowed_in_phase,
    install_standard_runtime, is_standard_host_import, ngx_host_catalog, ngx_host_manifest,
    ngx_host_registry, production_runtime_host_names,
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
fn compiler_catalog_adds_no_standard_family_schema() {
    let ngx = ngx_host_catalog();
    let merged = compile_host_catalog();
    // The compile catalog is the ngx surface exactly: no standard family
    // schema is merged, so no standard position can be relaxed. HTTP stays a
    // dynamic host import that configuration validates by name.
    assert_eq!(
        merged.functions(),
        ngx.functions(),
        "the merged catalog must carry exactly the ngx functions"
    );
    assert_eq!(merged.functions().len(), ngx.functions().len());
    assert_eq!(
        merged.structs(),
        ngx.structs(),
        "the merged catalog must not inherit the pd-vm HTTP named records"
    );
    for name in ["http::client::request", "http::client::sse"] {
        assert!(
            merged.function(name).is_none(),
            "{name} must stay a dynamic standard import, not a catalog function"
        );
        assert!(
            is_standard_host_import(name),
            "standard import set misses {name}"
        );
        assert!(host_allowed_in_phase(name, NgxPhase::Access));
        assert!(host_allowed_in_phase(name, NgxPhase::Content));
        assert!(!host_allowed_in_phase(name, NgxPhase::Log));
    }
    for name in [
        "io::open",
        "io::read_all",
        "io::close",
        "sqlite::open",
        "sqlite::query",
        "sqlite::close",
    ] {
        assert!(
            merged.function(name).is_none(),
            "{name} must stay on pd-vm's builtin dispatch path"
        );
        assert!(!is_standard_host_import(name));
    }
}

#[test]
fn standard_http_imports_compile_dynamically_and_keep_the_runtime_contract() {
    // The documented loose object-literal scripts compile against the dynamic
    // import, and the runtime payload variants of the pd-vm contract stay
    // intact in the catalog the adapter validates against.
    let program = compile(
        r#"
        use http;
        let response: map = http::client::request({
            method: "GET",
            url: "http://127.0.0.1:8081/data",
            headers: { "accept": "application/json" },
            body: null
        });
        let status: int = response["status"];
        "#,
    );
    assert!(
        program
            .imports
            .iter()
            .any(|import| import.name == "http::client::request"),
        "the HTTP call must be recorded as an import for configuration validation"
    );
    assert!(program_requires_request_state(&program));

    // The runtime contract still declares the duplicate-header array and the
    // text/bytes header-value payload variants: the merged catalog must not
    // degrade them.
    let contract = vm::http_host_catalog();
    let request = contract
        .function("http::client::request")
        .expect("contract request schema");
    let request_fields = match &request.params[0].ty {
        HostTypeSchema::Named { fields, .. } => fields.clone(),
        other => panic!("request param must stay a named record, got {other:?}"),
    };
    let headers = request_fields
        .iter()
        .find(|field| field.name == "headers")
        .expect("request record carries headers");
    match &headers.ty {
        HostTypeSchema::Optional(inner) => match inner.as_ref() {
            HostTypeSchema::Array(inner) => match inner.as_ref() {
                HostTypeSchema::Named { name, .. } => assert_eq!(
                    name, "HttpRequestHeader",
                    "headers stay an array of typed header entries (duplicates allowed)"
                ),
                other => panic!("header entry must stay named, got {other:?}"),
            },
            other => panic!("headers must stay an array, got {other:?}"),
        },
        other => panic!("headers must stay optional, got {other:?}"),
    }
    let response = contract
        .function("http::client::request")
        .expect("contract request schema")
        .return_type
        .clone();
    let response_headers = match &response {
        HostTypeSchema::Named { fields, .. } => fields
            .iter()
            .find(|field| field.name == "headers")
            .expect("response record carries headers")
            .ty
            .clone(),
        other => panic!("response must stay a named record, got {other:?}"),
    };
    match response_headers {
        HostTypeSchema::Array(inner) => match inner.as_ref() {
            HostTypeSchema::Named { fields, .. } => {
                let value = fields
                    .iter()
                    .find(|field| field.name == "value")
                    .expect("response header carries the value variant");
                match &value.ty {
                    HostTypeSchema::Named { name, fields } => {
                        assert_eq!(name, "HttpHeaderValue");
                        let kinds: HashSet<&str> =
                            fields.iter().map(|field| field.name.as_str()).collect();
                        assert!(
                            kinds.contains("text") && kinds.contains("bytes"),
                            "the header-value payload stays a text/bytes variant: {kinds:?}"
                        );
                    }
                    other => panic!("header value must stay named, got {other:?}"),
                }
            }
            other => panic!("response header entry must stay named, got {other:?}"),
        },
        other => panic!("response headers must stay an array, got {other:?}"),
    }
}

#[test]
fn timer_callback_capability_metadata_matches_the_manifest_capabilities() {
    // Consume the callback capability metadata: the deny list is exactly the
    // request/response/ctx/control set, and every manifest entry is admitted
    // to a timer callback if and only if it requires none of them.
    use ngx_rustscript::host::{
        TIMER_CALLBACK_DENIED_CAPABILITIES, TIMER_CALLBACK_PHASE, host_allowed_in_timer_callback,
    };
    assert_eq!(TIMER_CALLBACK_PHASE, NgxPhase::Timer);
    assert_eq!(
        TIMER_CALLBACK_DENIED_CAPABILITIES,
        [
            HostCapability::RequestRead,
            HostCapability::RequestWrite,
            HostCapability::RequestCtx,
            HostCapability::ResponseRead,
            HostCapability::ResponseWrite,
            HostCapability::Control,
        ],
        "the callback deny list is the reviewed request/response/ctx/control set"
    );
    for entry in ngx_host_manifest() {
        let denied = entry
            .capabilities
            .iter()
            .any(|capability| TIMER_CALLBACK_DENIED_CAPABILITIES.contains(capability));
        assert_eq!(
            host_allowed_in_timer_callback(entry.name),
            !denied,
            "{} callback admission must agree with its capabilities",
            entry.name
        );
    }
    // The documented callback surface works without a request; the request-,
    // response-, ctx-, and control-coupled hosts stay out.
    for name in [
        NGX_SLEEP_MS_NAME,
        "ngx::timer::at",
        "ngx::timer::every",
        "ngx::timer::pending_count",
        "ngx::timer::running_count",
        "ngx::shared::set",
        "ngx::semaphore::new",
        "ngx::log",
    ] {
        assert!(
            host_allowed_in_timer_callback(name),
            "{name} must be admitted to a timer callback"
        );
    }
    for name in [
        "ngx::req::get_method",
        "ngx::header::get",
        "ngx::var::get",
        "ngx::ctx::get",
        "ngx::exit",
        "ngx::print",
    ] {
        assert!(
            !host_allowed_in_timer_callback(name),
            "{name} must stay denied in a timer callback"
        );
    }
    assert!(
        !host_allowed_in_timer_callback("ngx::unknown_host"),
        "an unlisted host is never admitted to a callback"
    );
}

#[test]
fn timer_catalog_callback_schema_is_the_declared_callable() {
    use ngx_rustscript::host::{NGX_TIMER_AT_NAME, NGX_TIMER_EVERY_NAME, timer_callback_schema};
    let catalog = ngx_host_catalog();
    let expected = timer_callback_schema();
    for name in [NGX_TIMER_AT_NAME, NGX_TIMER_EVERY_NAME] {
        let schema = catalog.function(name).expect("timer registration schema");
        let callback = schema
            .params
            .iter()
            .find(|param| param.name == "callback")
            .unwrap_or_else(|| panic!("{name} must declare a callback parameter"));
        assert_eq!(
            callback.ty, expected,
            "{name} callback must use the declared `fn(bool) -> unknown` schema"
        );
    }
}

#[test]
fn standard_async_imports_compile_and_require_request_owned_execution() {
    let program = compile(
        r#"
        use io;
        use http;
        use sqlite;
        let file = io::open("input.txt", "r");
        let text = io::read_all(file);
        io::close(file);
        http::client::request({"method": "GET", "url": text});
        let db = sqlite::open({"path": "state.db", "mode": "read_write_create"});
        sqlite::query(db, "SELECT value FROM events", [], {});
        sqlite::close(db);
        "#,
    );
    assert!(ngx_rustscript::config::program_requires_request_state(
        &program
    ));
    validate_program_imports_for_phase(&program, NgxPhase::Access).expect("access allows async");
    validate_program_imports_for_phase(&program, NgxPhase::Content).expect("content allows async");
    let err = validate_program_imports_for_phase(&program, NgxPhase::Log)
        .expect_err("log cannot suspend");
    assert!(matches!(err, ConfigError::PhaseForbidden { .. }));
}

#[test]
fn documented_standard_async_interfaces_compile() {
    compile(
        r#"
        use io;
        use http;
        use sqlite;
        use bytes as builtin_bytes;

        let handle: int = io::open("input.txt", "r");
        let line: string = io::read_line(handle);
        let remaining: string = io::read_all(handle);
        let written: int = io::write(handle, line);
        let flushed: bool = io::flush(handle);
        let closed: bool = io::close(handle);
        let exists: bool = io::exists("input.txt");

        let response: map = http::client::request({
            method: "GET",
            url: "https://example.test/",
            headers: { "accept": "application/json" },
            body: null
        });
        let status: int = response["status"];
        let body_bytes: bytes = response["body"];
        let body: string = builtin_bytes::to_utf8(body_bytes);

        fn on_sse_event(item: map) -> map {
            { action: "continue" }
        }
        let sse_summary: map = http::client::sse({
            method: "GET",
            url: "https://example.test/events",
            timeout_ms: 1000
        }, on_sse_event);

        let db: int = sqlite::open({
            path: "state.db",
            mode: "read_write_create",
            limits: {}
        });
        let executed: map = sqlite::execute(db, "CREATE TABLE t (v TEXT)", []);
        let affected: int = executed["rows_affected"];
        let queried: map = sqlite::query(db, "SELECT v FROM t", [], { max_rows: 10 });
        let truncated: bool = queried["truncated"];
        let transaction_results = sqlite::transaction(db, {
            { sql: "INSERT INTO t(v) VALUES (?1)", params: {"value"} }
        });
        sqlite::close(db);
        "#,
    );
}

#[test]
fn standard_runtime_policy_defaults_fail_closed_and_installs_on_a_vm() {
    let policy = StandardHostPolicy::fail_closed();
    assert!(policy.io_roots.is_empty());
    assert!(!policy.io_write);
    assert!(policy.http_hosts.is_empty());
    assert!(policy.http_ports.is_empty());
    assert!(!policy.http_allow_private);
    assert!(policy.sqlite_root.is_none());

    let program = compile("let value = 1;\n");
    let mut vm = Vm::try_new((*program).clone()).expect("construct vm");
    install_standard_runtime(&mut vm, &policy).expect("valid fail-closed policy");
}

#[test]
fn log_phase_rejects_each_standard_async_domain_independently() {
    for (domain, source) in [
        (
            "io",
            "use io; let h = io::open(\"/tmp/x\", \"r\"); io::close(h);",
        ),
        (
            "http",
            "use http; let r = http::client::request({\"method\": \"GET\", \"url\": \"https://example.test\"});",
        ),
        (
            "sqlite",
            "use sqlite; let db = sqlite::open({\"path\": \":memory:\", \"mode\": \"memory\"}); sqlite::close(db);",
        ),
    ] {
        let err = compile_rss_source_for_phase(source, NgxPhase::Log)
            .expect_err("standard async domain must be rejected in log phase");
        let text = err.to_string();
        assert!(text.contains(domain), "{domain} rejection was {text:?}");
    }
}

#[test]
fn async_pipeline_example_compiles_with_request_owned_execution() {
    let source = include_str!("../examples/async-pipeline.rss");
    let program = compile_rss_source_for_phase(source, NgxPhase::Content)
        .expect("async pipeline example must compile");
    assert!(program_requires_request_state(&program));
}

#[test]
fn shared_fixed_results_compile_as_explicit_struct_types() {
    compile(
        r#"
        use ngx;

        let dict = ngx::shared::dict("ratelimit");
        let got: SharedGetResult = ngx::shared::get(dict, "key");
        if got.error == null && !got.stale {
            got.value;
            got.flags;
        }

        let stored: SharedStoreResult = ngx::shared::set(dict, "key", 1, 2, null);
        if stored.ok && !stored.forcible {
            stored.error;
        }

        let added: SharedSafeStoreResult =
            ngx::shared::safe_add(dict, "key", 1, 2, null);
        if added.ok {
            added.error;
        }

        let counted: SharedIncrResult = ngx::shared::incr(dict, "key", 1, null, null);
        if counted.error == null && !counted.forcible {
            counted.value;
        }

        let ttl: SharedNumberResult = ngx::shared::ttl(dict, "key");
        if ttl.error == null {
            ttl.value;
        }
        "#,
    );
}

#[test]
fn dynamic_map_envelopes_compile_as_explicit_struct_types() {
    compile(
        r#"
        use ngx;

        let decoded: DecodedArgsResult = ngx::decode_args("a=1", null);
        if !decoded.truncated {
            ngx::encode_args(decoded.args);
        }

        let headers: RequestHeadersResult = ngx::req::get_headers(null, null);
        if !headers.truncated {
            ngx::encode_args(headers.headers);
        }

        let uri: RequestArgsResult = ngx::req::get_uri_args(null);
        let post: RequestArgsResult = ngx::req::get_post_args(null);
        if !uri.truncated && !post.truncated {
            ngx::encode_args(uri.args);
            ngx::encode_args(post.args);
        }
        "#,
    );
}

#[test]
fn dynamic_map_envelopes_are_not_declared_as_maps() {
    let catalog = ngx_host_catalog();
    for (name, expected) in [
        ("ngx::decode_args", "DecodedArgsResult"),
        ("ngx::req::get_headers", "RequestHeadersResult"),
        ("ngx::req::get_uri_args", "RequestArgsResult"),
        ("ngx::req::get_post_args", "RequestArgsResult"),
    ] {
        match &catalog.function(name).expect("host schema").return_type {
            HostTypeSchema::Named {
                name: struct_name,
                fields,
            } => {
                assert_eq!(struct_name, expected, "{name} named envelope");
                assert_eq!(fields.len(), 2, "{name} envelope field count");
                assert_eq!(fields[1].name, "truncated");
                assert_eq!(fields[1].ty, HostTypeSchema::Bool);
            }
            other => panic!("{name} must return a named envelope, got {other:?}"),
        }
    }
}

#[test]
fn shared_fixed_results_are_not_declared_as_dynamic_maps() {
    let catalog = ngx_host_catalog();
    for (name, expected) in [
        ("ngx::shared::get", "SharedGetResult"),
        ("ngx::shared::get_stale", "SharedGetResult"),
        ("ngx::shared::set", "SharedStoreResult"),
        ("ngx::shared::safe_set", "SharedSafeStoreResult"),
        ("ngx::shared::add", "SharedStoreResult"),
        ("ngx::shared::safe_add", "SharedSafeStoreResult"),
        ("ngx::shared::replace", "SharedStoreResult"),
        ("ngx::shared::incr", "SharedIncrResult"),
        ("ngx::shared::ttl", "SharedNumberResult"),
        ("ngx::shared::expire", "SharedSafeStoreResult"),
        ("ngx::shared::lpush", "SharedNumberResult"),
        ("ngx::shared::rpush", "SharedNumberResult"),
        ("ngx::shared::lpop", "SharedGetResult"),
        ("ngx::shared::rpop", "SharedGetResult"),
        ("ngx::shared::llen", "SharedNumberResult"),
    ] {
        match &catalog
            .function(name)
            .expect("shared host schema")
            .return_type
        {
            HostTypeSchema::Named {
                name: struct_name, ..
            } => {
                assert_eq!(struct_name, expected, "{name} named envelope");
            }
            other => panic!(
                "{name} has a fixed result shape and must use an explicit RustScript struct type, got {other:?}"
            ),
        }
    }
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
        199,
        "production runtime host count must stay 199"
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
        manifest_names.as_slice(),
        "compiler catalog names and order must match the manifest exactly"
    );
    assert_eq!(
        catalog.functions().len(),
        manifest_names.len(),
        "catalog must not declare overloads for production ngx hosts"
    );
    assert_eq!(
        runtime_names,
        manifest_names.as_slice(),
        "production runtime names must match the catalog/manifest order exactly"
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
                assert!(
                    entry.capabilities.contains(&HostCapability::Timer),
                    "{} is a timer host",
                    entry.name
                );
                if entry.sync == HostSyncKind::Async {
                    // `ngx::sleep` / `ngx::sleep_ms`.
                    assert_eq!(entry.openresty_name, "ngx.sleep");
                } else {
                    // `ngx::timer::{at,every,pending_count,running_count}`:
                    // registration and counts are synchronous host calls; the
                    // callback executes later on a timer-owned VM.
                    assert!(
                        entry.openresty_name.starts_with("ngx.timer."),
                        "{} must name an ngx.timer API, got {}",
                        entry.name,
                        entry.openresty_name
                    );
                }
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
            HostFamily::Semaphore => {
                assert_eq!(entry.capabilities, &[HostCapability::Semaphore]);
                assert!(
                    entry.openresty_name.starts_with("ngx.semaphore."),
                    "{} Semaphore hosts must name ngx.semaphore.*, got {}",
                    entry.name,
                    entry.openresty_name
                );
                assert_eq!(
                    entry.phases,
                    &[NgxPhase::Access, NgxPhase::Content],
                    "{} stays fail-closed outside Access/Content",
                    entry.name
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
    assert_eq!(schema.return_type, HostTypeSchema::Unit);
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
    assert_eq!(schema.return_type, HostTypeSchema::Unit);
}

#[test]
fn bound_vm_has_every_catalogued_runtime_host_and_no_test_hosts() {
    let registry = ngx_host_registry().expect("production registry");
    for name in production_runtime_host_names() {
        assert!(
            registry.contains_name(name),
            "production registry must contain catalogued host {name}"
        );
    }
    for banned in ["ngx::probe", "ngx::fail", "ngx::ready"] {
        assert!(
            !registry.contains_name(banned),
            "production registry must not install {banned}"
        );
    }

    let program = compile("let x = 1;\n");
    let mut vm = Vm::try_new((*program).clone()).expect("construct vm");
    bind_ngx_hosts(&mut vm);
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

    let now_program = compile_rss_source("use ngx;\nngx::now();\n").expect("ngx::now program");
    let mut now_vm = Vm::new_shared(now_program);
    bind_ngx_hosts(&mut now_vm);
    now_vm
        .run()
        .expect("production bind must execute imported ngx::now");
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

fn collect_dynamic_slots(ty: &HostTypeSchema, path: &str, out: &mut BTreeSet<String>) {
    match ty {
        HostTypeSchema::Unknown => {
            out.insert(format!("{path}:Unknown"));
        }
        HostTypeSchema::Map(inner) => {
            out.insert(format!("{path}:Map"));
            collect_dynamic_slots(inner, &format!("{path}.<map>"), out);
        }
        HostTypeSchema::Array(inner) => {
            collect_dynamic_slots(inner, &format!("{path}[]"), out);
        }
        HostTypeSchema::Optional(inner) => {
            collect_dynamic_slots(inner, &format!("{path}?"), out);
        }
        HostTypeSchema::Named { name, fields } => {
            for field in fields {
                collect_dynamic_slots(&field.ty, &format!("{path}.{name}.{}", field.name), out);
            }
        }
        _ => {}
    }
}

fn catalog_dynamic_slots() -> BTreeSet<String> {
    let catalog = ngx_host_catalog();
    let mut slots = BTreeSet::new();
    for schema in catalog.functions() {
        for param in &schema.params {
            collect_dynamic_slots(
                &param.ty,
                &format!("{}#{}", schema.name, param.name),
                &mut slots,
            );
        }
        collect_dynamic_slots(
            &schema.return_type,
            &format!("{}#return", schema.name),
            &mut slots,
        );
    }
    slots
}

/// Exact per-function/slot allowlist for remaining Unknown/Map catalog types.
///
/// Rationales:
/// - `header::get` return: OpenResty can yield a string or a multi-value array.
/// - request/codec maps: query/header bags are open-keyed.
/// - shared-dict `value`/store payloads: heterogeneous zone values.
/// - optional OpenResty trailing args (`exptime`, `flags`, `max_*`, timeouts).
/// - print/say/socket payloads: mixed printable or buffer values.
/// - request ctx: opaque per-request table, not a VM resource.
const ALLOWED_DYNAMIC_SLOTS: &[&str] = &[
    "ngx::ctx::delete#return:Unknown",
    "ngx::ctx::get#return:Unknown",
    "ngx::ctx::set#value:Unknown",
    "ngx::decode_args#max_args:Unknown",
    "ngx::decode_args#return.DecodedArgsResult.args.<map>:Unknown",
    "ngx::decode_args#return.DecodedArgsResult.args:Map",
    "ngx::encode_args#args:Unknown",
    "ngx::encode_base64#input:Unknown",
    "ngx::encode_base64#no_padding:Unknown",
    "ngx::escape_uri#escape_type:Unknown",
    "ngx::header::get#return:Unknown",
    "ngx::header::set#value:Unknown",
    "ngx::header::set_all#headers:Unknown",
    "ngx::print#data:Unknown",
    "ngx::redirect#status:Unknown",
    "ngx::req::get_headers#max_headers:Unknown",
    "ngx::req::get_headers#raw:Unknown",
    "ngx::req::get_headers#return.RequestHeadersResult.headers.<map>:Unknown",
    "ngx::req::get_headers#return.RequestHeadersResult.headers:Map",
    "ngx::req::get_post_args#max_args:Unknown",
    "ngx::req::get_post_args#return.RequestArgsResult.args.<map>:Unknown",
    "ngx::req::get_post_args#return.RequestArgsResult.args:Map",
    "ngx::req::get_uri_args#max_args:Unknown",
    "ngx::req::get_uri_args#return.RequestArgsResult.args.<map>:Unknown",
    "ngx::req::get_uri_args#return.RequestArgsResult.args:Map",
    "ngx::req::init_body#buffer_size:Unknown",
    "ngx::req::raw_header#no_request_line:Unknown",
    "ngx::req::set_body_file#auto_clean:Unknown",
    "ngx::req::set_header#value:Unknown",
    "ngx::req::set_uri#jump:Unknown",
    "ngx::req::set_uri_args#args:Unknown",
    "ngx::say#data:Unknown",
    "ngx::semaphore::post#count:Unknown",
    "ngx::semaphore::wait#timeout:Unknown",
    "ngx::shared::add#exptime:Unknown",
    "ngx::shared::add#flags:Unknown",
    "ngx::shared::add#value:Unknown",
    "ngx::shared::expire#exptime:Unknown",
    "ngx::shared::flush_expired#max:Unknown",
    "ngx::shared::get#return.SharedGetResult.value?:Unknown",
    "ngx::shared::get_keys#max:Unknown",
    "ngx::shared::get_stale#return.SharedGetResult.value?:Unknown",
    "ngx::shared::incr#init:Unknown",
    "ngx::shared::incr#init_ttl:Unknown",
    "ngx::shared::incr#value:Unknown",
    "ngx::shared::lpop#return.SharedGetResult.value?:Unknown",
    "ngx::shared::lpush#value:Unknown",
    "ngx::shared::replace#exptime:Unknown",
    "ngx::shared::replace#flags:Unknown",
    "ngx::shared::replace#value:Unknown",
    "ngx::shared::rpop#return.SharedGetResult.value?:Unknown",
    "ngx::shared::rpush#value:Unknown",
    "ngx::shared::safe_add#exptime:Unknown",
    "ngx::shared::safe_add#flags:Unknown",
    "ngx::shared::safe_add#value:Unknown",
    "ngx::shared::safe_set#exptime:Unknown",
    "ngx::shared::safe_set#flags:Unknown",
    "ngx::shared::safe_set#value:Unknown",
    "ngx::shared::set#exptime:Unknown",
    "ngx::shared::set#flags:Unknown",
    "ngx::shared::set#value:Unknown",
    "ngx::socket::tcp::clientcert#cert:Unknown",
    "ngx::socket::tcp::clientcert#pkey:Unknown",
    "ngx::socket::tcp::receive#mode:Unknown",
    "ngx::socket::tcp::receiveuntil#delimiter:Unknown",
    "ngx::socket::tcp::send#data:Unknown",
    "ngx::socket::tcp::setkeepalive#pool_size:Unknown",
    "ngx::socket::tcp::setkeepalive#timeout:Unknown",
    "ngx::socket::tcp::sslhandshake#reused_session:Unknown",
    "ngx::socket::tcp::sslhandshake#send_status_req:Unknown",
    "ngx::socket::tcp::sslhandshake#server_name:Unknown",
    "ngx::socket::tcp::sslhandshake#ssl_verify:Unknown",
    "ngx::socket::tcp::trustedstore#pem:Unknown",
    "ngx::socket::udp::bind#host:Unknown",
    "ngx::socket::udp::receive#size:Unknown",
    "ngx::socket::udp::send#data:Unknown",
    "ngx::var::set#value:Unknown",
];

#[test]
fn catalog_schema_version_and_fingerprint_goldens() {
    let catalog = ngx_host_catalog();
    assert_eq!(NGX_HOST_CATALOG_SCHEMA_VERSION, 4);
    assert_eq!(NGX_HOST_CATALOG_FINGERPRINT_V1, 0x5e37_8285_8fa0_b584);
    assert_eq!(NGX_HOST_CATALOG_FINGERPRINT_V2, 0x10c4_07b2_b827_eaff);
    assert_eq!(
        catalog.fingerprint().as_u64(),
        NGX_HOST_CATALOG_FINGERPRINT,
        "catalog fingerprint drifted; bump schema version and goldens"
    );
    assert_ne!(
        NGX_HOST_CATALOG_FINGERPRINT, NGX_HOST_CATALOG_FINGERPRINT_V1,
        "current fingerprint must differ from the v1 golden"
    );
    assert_ne!(
        NGX_HOST_CATALOG_FINGERPRINT, NGX_HOST_CATALOG_FINGERPRINT_V2,
        "current fingerprint must differ from the v2 golden"
    );
}

#[test]
fn void_hosts_return_unit_instead_of_legacy_null() {
    let catalog = ngx_host_catalog();
    for function in catalog.functions() {
        if function.name == "ngx::core::null_value" {
            // The one deliberate null-returning host: it hands guest code a
            // raw null value to prove literal rejection and null semantics.
            assert_eq!(
                function.return_type,
                HostTypeSchema::Null,
                "ngx::core::null_value must stay the legacy Null probe"
            );
            continue;
        }
        assert_ne!(
            function.return_type,
            HostTypeSchema::Null,
            "{} must return Unit; the RustScript compiler rejects legacy Null host returns",
            function.name
        );
    }
}

#[test]
fn exact_dynamic_unknown_and_map_slots_are_allowlisted() {
    let actual = catalog_dynamic_slots();

    let expected: BTreeSet<String> = ALLOWED_DYNAMIC_SLOTS
        .iter()
        .map(|slot| (*slot).to_string())
        .collect();
    assert_eq!(
        actual, expected,
        "Unknown/Map catalog slots drifted; update ALLOWED_DYNAMIC_SLOTS with rationale"
    );
}
