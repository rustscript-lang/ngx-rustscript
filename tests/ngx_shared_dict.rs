//! Shared dictionary core: process-local engine, hosts, catalog, and limits.

use ngx_rustscript_nginx_test_stubs as _;

use std::sync::Arc;

use ngx_rustscript::config::compile_rss_source;
use ngx_rustscript::host::{
    HostCapability, HostFamily, HostSyncKind, NGX_SHARED_ADD_NAME, NGX_SHARED_CAPACITY_NAME,
    NGX_SHARED_DELETE_NAME, NGX_SHARED_DICT_NAME, NGX_SHARED_EXPIRE_NAME,
    NGX_SHARED_FREE_SPACE_NAME, NGX_SHARED_GET_NAME, NGX_SHARED_GET_STALE_NAME,
    NGX_SHARED_INCR_NAME, NGX_SHARED_REPLACE_NAME, NGX_SHARED_SAFE_ADD_NAME,
    NGX_SHARED_SAFE_SET_NAME, NGX_SHARED_SET_NAME, NGX_SHARED_TTL_NAME, NgxPhase,
    bind_registry_for_test, host_allowed_in_phase, manifest_entry, ngx_host_catalog,
};
use ngx_rustscript::shared_dict::{
    self, DictValue, MAX_KEY_BYTES, MAX_VALUE_BYTES, MAX_ZONE_NAME_BYTES, StoreMode,
    install_process_zone, reset_process_zones, set_clock_ms,
};
use vm::{HostParamPassing, HostTypeSchema, Program, ResourceTypeKey, Value, Vm, VmStatus};

fn compile(source: &str) -> Arc<Program> {
    compile_rss_source(source).unwrap_or_else(|err| panic!("compile failed: {err}\n{source}"))
}

fn assert_named_return(schema: &vm::HostFunctionSchema, expected: &str) {
    match &schema.return_type {
        HostTypeSchema::Named { name, .. } => assert_eq!(name, expected),
        other => panic!("expected named {expected}, got {other:?}"),
    }
}

fn eval(source: &str) -> Value {
    reset_process_zones();
    install_process_zone("dogs", 64 * 1024);
    eval_on_zone(source)
}

fn eval_on_zone(source: &str) -> Value {
    let program = compile(source);
    let mut vm = Vm::try_new((*program).clone()).expect("construct vm");
    bind_registry_for_test(&mut vm).expect("bind test registry");
    match vm.run() {
        Ok(VmStatus::Halted) => vm
            .stack()
            .last()
            .cloned()
            .unwrap_or_else(|| panic!("expected a stack value for {source}")),
        Ok(status) => panic!("unexpected status {status:?}\n{source}"),
        Err(err) => panic!("eval failed: {err}\n{source}"),
    }
}

fn eval_err(source: &str) -> String {
    reset_process_zones();
    install_process_zone("dogs", 64 * 1024);
    let program = compile(source);
    let mut vm = Vm::try_new((*program).clone()).expect("construct vm");
    bind_registry_for_test(&mut vm).expect("bind test registry");
    match vm.run() {
        Err(err) => err.to_string(),
        Ok(status) => panic!("expected host error, got {status:?}\n{source}"),
    }
}

fn map_get<'a>(value: &'a Value, key: &str) -> &'a Value {
    match value {
        Value::Map(map) => map
            .get(&Value::string(key))
            .unwrap_or_else(|| panic!("missing map key {key} in {value:?}")),
        other => panic!("expected map, got {other:?}"),
    }
}

fn expect_int(value: &Value) -> i64 {
    match value {
        Value::Int(number) => *number,
        other => panic!("expected int, got {other:?}"),
    }
}

fn expect_bool(value: &Value) -> bool {
    match value {
        Value::Bool(flag) => *flag,
        other => panic!("expected bool, got {other:?}"),
    }
}

fn expect_string(value: &Value) -> String {
    match value {
        Value::String(text) => (**text).clone(),
        other => panic!("expected string, got {other:?}"),
    }
}

const SHARED_HOSTS: &[&str] = &[
    NGX_SHARED_DICT_NAME,
    NGX_SHARED_GET_NAME,
    NGX_SHARED_GET_STALE_NAME,
    NGX_SHARED_SET_NAME,
    NGX_SHARED_SAFE_SET_NAME,
    NGX_SHARED_ADD_NAME,
    NGX_SHARED_SAFE_ADD_NAME,
    NGX_SHARED_REPLACE_NAME,
    NGX_SHARED_DELETE_NAME,
    NGX_SHARED_INCR_NAME,
    NGX_SHARED_TTL_NAME,
    NGX_SHARED_EXPIRE_NAME,
    NGX_SHARED_CAPACITY_NAME,
    NGX_SHARED_FREE_SPACE_NAME,
];

#[test]
fn shared_dict_hosts_are_catalogued() {
    let key = ResourceTypeKey::new("ngx.shared.dict").unwrap();
    let catalog = ngx_host_catalog();
    assert!(
        catalog
            .resources()
            .iter()
            .any(|resource| resource.key == key),
        "catalog must declare the shared dict resource type"
    );
    for name in SHARED_HOSTS {
        let entry = manifest_entry(name).unwrap_or_else(|| panic!("missing {name}"));
        assert_eq!(entry.family, HostFamily::SharedDict);
        assert_eq!(entry.sync, HostSyncKind::Sync);
        assert_eq!(entry.capabilities, &[HostCapability::SharedDict]);
        assert!(entry.openresty_name.starts_with("ngx.shared.DICT"));
        assert!(host_allowed_in_phase(name, NgxPhase::Access));
        if *name != NGX_SHARED_DICT_NAME {
            assert!(!host_allowed_in_phase(name, NgxPhase::InitWorker));
        }
        let schema = catalog
            .function(name)
            .unwrap_or_else(|| panic!("missing schema {name}"));
        match *name {
            NGX_SHARED_DICT_NAME => {
                assert!(host_allowed_in_phase(name, NgxPhase::Init));
                assert!(host_allowed_in_phase(name, NgxPhase::InitWorker));
                assert!(host_allowed_in_phase(name, NgxPhase::ExitWorker));
                assert_eq!(schema.return_type, HostTypeSchema::Resource(key.clone()));
                assert_eq!(schema.params[0].ty, HostTypeSchema::String);
            }
            NGX_SHARED_GET_NAME | NGX_SHARED_GET_STALE_NAME => {
                assert!(!host_allowed_in_phase(name, NgxPhase::Init));
                assert!(!host_allowed_in_phase(name, NgxPhase::InitWorker));
                assert!(!host_allowed_in_phase(name, NgxPhase::ExitWorker));
                assert_eq!(schema.params.len(), 2);
                assert_eq!(schema.params[0].passing, HostParamPassing::Borrow);
                assert_eq!(schema.params[0].ty, HostTypeSchema::Resource(key.clone()));
                assert_eq!(schema.params[1].ty, HostTypeSchema::String);
                assert_named_return(schema, "SharedGetResult");
            }
            NGX_SHARED_SET_NAME
            | NGX_SHARED_SAFE_SET_NAME
            | NGX_SHARED_ADD_NAME
            | NGX_SHARED_SAFE_ADD_NAME
            | NGX_SHARED_REPLACE_NAME => {
                assert!(host_allowed_in_phase(name, NgxPhase::Init));
                assert!(!host_allowed_in_phase(name, NgxPhase::InitWorker));
                assert!(!host_allowed_in_phase(name, NgxPhase::ExitWorker));
                assert_eq!(schema.params.len(), 5);
                assert_eq!(schema.params[0].passing, HostParamPassing::Borrow);
                assert_eq!(schema.params[0].ty, HostTypeSchema::Resource(key.clone()));
                assert_eq!(schema.params[1].ty, HostTypeSchema::String);
                assert_eq!(schema.params[2].ty, HostTypeSchema::Unknown);
                assert_eq!(schema.params[3].ty, HostTypeSchema::Unknown);
                assert_eq!(schema.params[4].ty, HostTypeSchema::Unknown);
                match *name {
                    NGX_SHARED_SAFE_SET_NAME | NGX_SHARED_SAFE_ADD_NAME => {
                        assert_named_return(schema, "SharedSafeStoreResult");
                    }
                    _ => assert_named_return(schema, "SharedStoreResult"),
                }
            }
            NGX_SHARED_DELETE_NAME => {
                assert!(host_allowed_in_phase(name, NgxPhase::Init));
                assert!(!host_allowed_in_phase(name, NgxPhase::ExitWorker));
                assert_eq!(schema.params.len(), 2);
                assert_eq!(schema.params[0].passing, HostParamPassing::Borrow);
                assert_eq!(schema.params[1].ty, HostTypeSchema::String);
                assert_eq!(schema.return_type, HostTypeSchema::Null);
            }
            NGX_SHARED_INCR_NAME => {
                assert!(host_allowed_in_phase(name, NgxPhase::Init));
                assert!(!host_allowed_in_phase(name, NgxPhase::ExitWorker));
                assert_eq!(schema.params.len(), 5);
                assert_eq!(schema.params[0].passing, HostParamPassing::Borrow);
                assert_eq!(schema.params[1].ty, HostTypeSchema::String);
                assert_named_return(schema, "SharedIncrResult");
            }
            NGX_SHARED_TTL_NAME => {
                assert!(host_allowed_in_phase(name, NgxPhase::Init));
                assert!(!host_allowed_in_phase(name, NgxPhase::ExitWorker));
                assert_eq!(schema.params.len(), 2);
                assert_eq!(schema.params[0].passing, HostParamPassing::Borrow);
                assert_eq!(schema.params[1].ty, HostTypeSchema::String);
                assert_named_return(schema, "SharedNumberResult");
            }
            NGX_SHARED_EXPIRE_NAME => {
                assert!(host_allowed_in_phase(name, NgxPhase::Init));
                assert!(!host_allowed_in_phase(name, NgxPhase::ExitWorker));
                assert_eq!(schema.params.len(), 3);
                assert_eq!(schema.params[0].passing, HostParamPassing::Borrow);
                assert_eq!(schema.params[1].ty, HostTypeSchema::String);
                assert_named_return(schema, "SharedSafeStoreResult");
            }
            NGX_SHARED_CAPACITY_NAME | NGX_SHARED_FREE_SPACE_NAME => {
                assert!(host_allowed_in_phase(name, NgxPhase::Init));
                assert!(!host_allowed_in_phase(name, NgxPhase::ExitWorker));
                assert_eq!(schema.params.len(), 1);
                assert_eq!(schema.params[0].passing, HostParamPassing::Borrow);
                assert_eq!(schema.return_type, HostTypeSchema::Int);
            }
            _ => unreachable!(),
        }
        compile(&match *name {
            NGX_SHARED_DICT_NAME => "use ngx;\nngx::shared::dict(\"dogs\");\n".to_string(),
            NGX_SHARED_GET_NAME => {
                "use ngx;\nlet d = ngx::shared::dict(\"dogs\");\nngx::shared::get(d, \"k\");\n"
                    .to_string()
            }
            NGX_SHARED_GET_STALE_NAME => {
                "use ngx;\nlet d = ngx::shared::dict(\"dogs\");\nngx::shared::get_stale(d, \"k\");\n"
                    .to_string()
            }
            NGX_SHARED_SET_NAME => {
                "use ngx;\nlet d = ngx::shared::dict(\"dogs\");\nngx::shared::set(d, \"k\", 1, null, null);\n"
                    .to_string()
            }
            NGX_SHARED_SAFE_SET_NAME => {
                "use ngx;\nlet d = ngx::shared::dict(\"dogs\");\nngx::shared::safe_set(d, \"k\", 1, null, null);\n"
                    .to_string()
            }
            NGX_SHARED_ADD_NAME => {
                "use ngx;\nlet d = ngx::shared::dict(\"dogs\");\nngx::shared::add(d, \"k\", 1, null, null);\n"
                    .to_string()
            }
            NGX_SHARED_SAFE_ADD_NAME => {
                "use ngx;\nlet d = ngx::shared::dict(\"dogs\");\nngx::shared::safe_add(d, \"k\", 1, null, null);\n"
                    .to_string()
            }
            NGX_SHARED_REPLACE_NAME => {
                "use ngx;\nlet d = ngx::shared::dict(\"dogs\");\nngx::shared::replace(d, \"k\", 1, null, null);\n"
                    .to_string()
            }
            NGX_SHARED_DELETE_NAME => {
                "use ngx;\nlet d = ngx::shared::dict(\"dogs\");\nngx::shared::delete(d, \"k\");\n"
                    .to_string()
            }
            NGX_SHARED_INCR_NAME => {
                "use ngx;\nlet d = ngx::shared::dict(\"dogs\");\nngx::shared::incr(d, \"k\", 1, null, null);\n"
                    .to_string()
            }
            NGX_SHARED_TTL_NAME => {
                "use ngx;\nlet d = ngx::shared::dict(\"dogs\");\nngx::shared::ttl(d, \"k\");\n"
                    .to_string()
            }
            NGX_SHARED_EXPIRE_NAME => {
                "use ngx;\nlet d = ngx::shared::dict(\"dogs\");\nngx::shared::expire(d, \"k\", 1);\n"
                    .to_string()
            }
            NGX_SHARED_CAPACITY_NAME => {
                "use ngx;\nlet d = ngx::shared::dict(\"dogs\");\nngx::shared::capacity(d);\n"
                    .to_string()
            }
            NGX_SHARED_FREE_SPACE_NAME => {
                "use ngx;\nlet d = ngx::shared::dict(\"dogs\");\nngx::shared::free_space(d);\n"
                    .to_string()
            }
            _ => unreachable!(),
        });
    }
}

#[test]
fn process_local_get_set_scalars_flags_and_missing() {
    reset_process_zones();
    install_process_zone("dogs", 64 * 1024);
    assert_eq!(
        shared_dict::dict_get("dogs", b"missing", false)
            .unwrap()
            .value,
        None
    );
    let set = shared_dict::dict_store(
        "dogs",
        b"Jim",
        Some(DictValue::Int(8)),
        0.0,
        3,
        StoreMode::Set,
    )
    .unwrap();
    assert!(set.ok);
    assert!(!set.forcible);
    let got = shared_dict::dict_get("dogs", b"Jim", false).unwrap();
    assert_eq!(got.value, Some(DictValue::Int(8)));
    assert_eq!(got.flags, 3);
    assert!(!got.stale);
}

#[test]
fn host_get_set_add_replace_delete_incr_ttl() {
    let set = eval(
        r#"
use ngx;
let d = ngx::shared::dict("dogs");
ngx::shared::set(d, "Jim", 8, null, 7);
"#,
    );
    assert!(expect_bool(map_get(&set, "ok")));
    assert_eq!(*map_get(&set, "error"), Value::Null);
    assert!(!expect_bool(map_get(&set, "forcible")));

    let got = eval(
        r#"
use ngx;
let d = ngx::shared::dict("dogs");
ngx::shared::set(d, "Jim", 8, null, 7);
ngx::shared::get(d, "Jim");
"#,
    );
    assert_eq!(expect_int(map_get(&got, "value")), 8);
    assert_eq!(expect_int(map_get(&got, "flags")), 7);

    let missing = eval(
        r#"
use ngx;
let d = ngx::shared::dict("dogs");
ngx::shared::get(d, "nope");
"#,
    );
    assert_eq!(*map_get(&missing, "value"), Value::Null);

    let exists = eval(
        r#"
use ngx;
let d = ngx::shared::dict("dogs");
ngx::shared::set(d, "k", "v", null, null);
ngx::shared::add(d, "k", "other", null, null);
"#,
    );
    assert!(!expect_bool(map_get(&exists, "ok")));
    assert_eq!(expect_string(map_get(&exists, "error")), "exists");

    let not_found = eval(
        r#"
use ngx;
let d = ngx::shared::dict("dogs");
ngx::shared::replace(d, "missing", 1, null, null);
"#,
    );
    assert!(!expect_bool(map_get(&not_found, "ok")));
    assert_eq!(expect_string(map_get(&not_found, "error")), "not found");

    let deleted = eval(
        r#"
use ngx;
let d = ngx::shared::dict("dogs");
ngx::shared::set(d, "k", true, null, null);
ngx::shared::delete(d, "k");
ngx::shared::get(d, "k");
"#,
    );
    assert_eq!(*map_get(&deleted, "value"), Value::Null);

    let incr = eval(
        r#"
use ngx;
let d = ngx::shared::dict("dogs");
ngx::shared::incr(d, "n", 2, 10, null);
"#,
    );
    assert_eq!(expect_int(map_get(&incr, "value")), 12);
    assert_eq!(*map_get(&incr, "error"), Value::Null);

    let not_number = eval(
        r#"
use ngx;
let d = ngx::shared::dict("dogs");
ngx::shared::set(d, "s", "x", null, null);
ngx::shared::incr(d, "s", 1, null, null);
"#,
    );
    assert_eq!(expect_string(map_get(&not_number, "error")), "not a number");
    assert_eq!(*map_get(&not_number, "value"), Value::Null);
}

#[test]
fn ttl_expire_get_stale_and_clock() {
    reset_process_zones();
    install_process_zone("dogs", 64 * 1024);
    set_clock_ms(Some(1_000_000));
    shared_dict::dict_store(
        "dogs",
        b"k",
        Some(DictValue::String("v".into())),
        0.5,
        0,
        StoreMode::Set,
    )
    .unwrap();
    let ttl = shared_dict::dict_ttl("dogs", b"k").unwrap().unwrap();
    assert!((ttl - 0.5).abs() < 0.001);
    set_clock_ms(Some(1_000_400));
    let live = shared_dict::dict_get("dogs", b"k", false).unwrap();
    assert_eq!(live.value, Some(DictValue::String("v".into())));
    set_clock_ms(Some(1_000_600));
    let expired = shared_dict::dict_get("dogs", b"k", false).unwrap();
    assert_eq!(expired.value, None);
    let stale = shared_dict::dict_get("dogs", b"k", true).unwrap();
    assert_eq!(stale.value, Some(DictValue::String("v".into())));
    assert!(stale.stale);
    shared_dict::dict_store(
        "dogs",
        b"keep",
        Some(DictValue::Int(1)),
        0.0,
        0,
        StoreMode::Set,
    )
    .unwrap();
    assert_eq!(
        shared_dict::dict_ttl("dogs", b"keep").unwrap().unwrap(),
        0.0
    );
    shared_dict::dict_expire("dogs", b"keep", 1.0).unwrap();
    let ttl = shared_dict::dict_ttl("dogs", b"keep").unwrap().unwrap();
    assert!((ttl - 1.0).abs() < 0.001);
}

#[test]
fn lru_eviction_forcible_and_safe_set() {
    reset_process_zones();
    install_process_zone("tiny", 200);
    let payload = DictValue::String("abcdefghij".into());
    for i in 0..8 {
        let key = format!("k{i}");
        let result = shared_dict::dict_store(
            "tiny",
            key.as_bytes(),
            Some(payload.clone()),
            0.0,
            0,
            StoreMode::Set,
        )
        .unwrap();
        assert!(result.ok, "set {key} failed: {:?}", result.error);
    }
    let overflow = shared_dict::dict_store(
        "tiny",
        b"new",
        Some(payload.clone()),
        0.0,
        0,
        StoreMode::Set,
    )
    .unwrap();
    assert!(overflow.ok);
    assert!(overflow.forcible);
    assert_eq!(
        shared_dict::dict_get("tiny", b"k0", false).unwrap().value,
        None
    );
    reset_process_zones();
    install_process_zone("tiny", 120);
    shared_dict::dict_store("tiny", b"a", Some(payload.clone()), 0.0, 0, StoreMode::Set).unwrap();
    let safe =
        shared_dict::dict_store("tiny", b"b", Some(payload), 0.0, 0, StoreMode::SafeSet).unwrap();
    assert!(!safe.ok);
    assert_eq!(safe.error, Some("no memory"));
    assert!(
        shared_dict::dict_get("tiny", b"a", false)
            .unwrap()
            .value
            .is_some()
    );
}

#[test]
fn capacity_free_space_and_unknown_zone() {
    let cap = eval(
        r#"
use ngx;
let d = ngx::shared::dict("dogs");
ngx::shared::capacity(d);
"#,
    );
    assert_eq!(expect_int(&cap), 64 * 1024);
    let free = eval(
        r#"
use ngx;
let d = ngx::shared::dict("dogs");
ngx::shared::free_space(d);
"#,
    );
    assert!(expect_int(&free) > 0);
    let err = eval_err(
        r#"
use ngx;
ngx::shared::dict("missing");
"#,
    )
    .to_ascii_lowercase();
    assert!(err.contains("not found"), "{err}");
}

#[test]
fn empty_key_and_bad_value_type() {
    let empty = eval(
        r#"
use ngx;
let d = ngx::shared::dict("dogs");
ngx::shared::get(d, "");
"#,
    );
    assert_eq!(expect_string(map_get(&empty, "error")), "empty key");
    let err = eval_err(
        r#"
use ngx;
let d = ngx::shared::dict("dogs");
ngx::shared::set(d, "k", { a: 1 }, null, null);
"#,
    )
    .to_ascii_lowercase();
    assert!(err.contains("value"), "{err}");
    const { assert!(MAX_KEY_BYTES >= 65_535) };
    assert_eq!(MAX_VALUE_BYTES, 65_536);
}

#[test]
fn shared_dict_does_not_require_request_state() {
    let program = compile(
        r#"
use ngx;
let d = ngx::shared::dict("dogs");
ngx::shared::get(d, "k");
"#,
    );
    assert!(
        !ngx_rustscript::config::program_requires_request_state(&program),
        "shared-dict-only programs may stay on the pooled sync path"
    );
}

fn map_has_key(value: &Value, key: &str) -> bool {
    match value {
        Value::Map(map) => map.get(&Value::string(key)).is_some(),
        _ => false,
    }
}

#[test]
fn add_and_replace_reject_null() {
    let add = eval(
        r#"
use ngx;
let d = ngx::shared::dict("dogs");
ngx::shared::add(d, "k", null, null, null);
"#,
    );
    assert!(!expect_bool(map_get(&add, "ok")));
    assert_eq!(
        expect_string(map_get(&add, "error")),
        "attempt to add or replace nil values"
    );

    let replace = eval(
        r#"
use ngx;
let d = ngx::shared::dict("dogs");
ngx::shared::set(d, "k", 1, null, null);
ngx::shared::replace(d, "k", null, null, null);
"#,
    );
    assert!(!expect_bool(map_get(&replace, "ok")));
    assert_eq!(
        expect_string(map_get(&replace, "error")),
        "attempt to add or replace nil values"
    );
    let still = eval(
        r#"
use ngx;
let d = ngx::shared::dict("dogs");
ngx::shared::set(d, "k", 1, null, null);
ngx::shared::replace(d, "k", null, null, null);
ngx::shared::get(d, "k");
"#,
    );
    assert_eq!(expect_int(map_get(&still, "value")), 1);
}

#[test]
fn incr_result_schema_is_value_error_forcible() {
    let incr = eval(
        r#"
use ngx;
let d = ngx::shared::dict("dogs");
ngx::shared::incr(d, "n", 2, 10, null);
"#,
    );
    assert_eq!(expect_int(map_get(&incr, "value")), 12);
    assert_eq!(*map_get(&incr, "error"), Value::Null);
    assert!(!expect_bool(map_get(&incr, "forcible")));
    assert!(
        !map_has_key(&incr, "ok"),
        "incr map must not include ok: {incr:?}"
    );
}

#[test]
fn zone_name_cap_is_256_bytes() {
    assert_eq!(MAX_ZONE_NAME_BYTES, 256);
    assert!(shared_dict::validate_zone_name(&"a".repeat(256)).is_ok());
    assert!(shared_dict::validate_zone_name(&"a".repeat(257)).is_err());
    assert!(shared_dict::validate_zone_name("").is_err());
}

#[test]
fn tiny_positive_exptime_is_rejected_and_huge_exptime_is_rejected() {
    reset_process_zones();
    install_process_zone("dogs", 64 * 1024);
    set_clock_ms(Some(1_000_000));
    let tiny = shared_dict::dict_store(
        "dogs",
        b"k",
        Some(DictValue::Int(1)),
        0.0001,
        0,
        StoreMode::Set,
    )
    .unwrap();
    assert!(!tiny.ok);
    assert_eq!(tiny.error, Some("invalid exptime"));
    let huge = shared_dict::dict_store(
        "dogs",
        b"k",
        Some(DictValue::Int(1)),
        1e300,
        0,
        StoreMode::Set,
    )
    .unwrap();
    assert!(!huge.ok);
    assert_eq!(huge.error, Some("invalid exptime"));
}

#[test]
fn replace_is_allocation_transactional() {
    reset_process_zones();
    install_process_zone("tiny", 120);
    let small = DictValue::String("ab".into());
    let huge = DictValue::String("x".repeat(200));
    shared_dict::dict_store("tiny", b"k", Some(small.clone()), 0.0, 7, StoreMode::Set).unwrap();
    let failed =
        shared_dict::dict_store("tiny", b"k", Some(huge), 0.0, 0, StoreMode::Replace).unwrap();
    assert!(!failed.ok);
    assert_eq!(failed.error, Some("no memory"));
    let got = shared_dict::dict_get("tiny", b"k", false).unwrap();
    assert_eq!(got.value, Some(small));
    assert_eq!(got.flags, 7);
}

#[test]
fn existing_numeric_incr_is_in_place_under_pressure() {
    reset_process_zones();
    install_process_zone("tiny", 160);
    let filler = DictValue::String("abcdefghij".into());
    for i in 0..6 {
        let key = format!("k{i}");
        let _ = shared_dict::dict_store(
            "tiny",
            key.as_bytes(),
            Some(filler.clone()),
            0.0,
            0,
            StoreMode::Set,
        );
    }
    shared_dict::dict_store(
        "tiny",
        b"n",
        Some(DictValue::Int(1)),
        0.0,
        9,
        StoreMode::Set,
    )
    .unwrap();
    let incr = shared_dict::dict_incr("tiny", b"n", DictValue::Int(1), None, 0.0).unwrap();
    assert!(incr.ok, "in-place incr must not realloc: {:?}", incr.error);
    assert!(!incr.forcible);
    assert_eq!(incr.value, Some(DictValue::Int(2)));
    let got = shared_dict::dict_get("tiny", b"n", false).unwrap();
    assert_eq!(got.value, Some(DictValue::Int(2)));
    assert_eq!(got.flags, 9);
}

#[test]
fn handle_rejects_stale_and_cross_zone_confusion() {
    reset_process_zones();
    install_process_zone("dogs", 64 * 1024);
    install_process_zone("cats", 64 * 1024);
    let dogs = shared_dict::open_handle("dogs").unwrap();
    let cats = shared_dict::open_handle("cats").unwrap();
    shared_dict::dict_store(
        dogs.zone_name(),
        b"k",
        Some(DictValue::String("woof".into())),
        0.0,
        0,
        StoreMode::Set,
    )
    .unwrap();
    shared_dict::dict_store(
        cats.zone_name(),
        b"k",
        Some(DictValue::String("meow".into())),
        0.0,
        0,
        StoreMode::Set,
    )
    .unwrap();
    assert_eq!(
        shared_dict::dict_get_for(&dogs, b"k", false).unwrap().value,
        Some(DictValue::String("woof".into()))
    );
    assert_eq!(
        shared_dict::dict_get_for(&cats, b"k", false).unwrap().value,
        Some(DictValue::String("meow".into()))
    );
    reset_process_zones();
    install_process_zone("dogs", 64 * 1024);
    let err = shared_dict::dict_get_for(&dogs, b"k", false).unwrap_err();
    assert_eq!(err, "not found");
}

#[test]
fn rbtree_orders_equal_hash_keys_with_nginx_memn2cmp() {
    let left = shared_dict::nginx_memn2cmp(b"aa", b"ab");
    let right = shared_dict::nginx_memn2cmp(b"ab", b"aa");
    let prefix = shared_dict::nginx_memn2cmp(b"aa", b"aaa");
    assert!(left < 0);
    assert!(right > 0);
    assert!(prefix < 0);
    assert_eq!(shared_dict::nginx_memn2cmp(b"same", b"same"), 0);

    let mut tree = shared_dict::DictIndex::new();
    tree.insert(0x1111_1111, b"ab", 1);
    tree.insert(0x1111_1111, b"aa", 2);
    tree.insert(0x1111_1111, b"aaa", 3);
    tree.insert(0x0000_0001, b"z", 4);
    assert_eq!(tree.lookup(0x1111_1111, b"aa"), Some(2));
    assert_eq!(tree.lookup(0x1111_1111, b"ab"), Some(1));
    assert_eq!(tree.lookup(0x1111_1111, b"aaa"), Some(3));
    assert_eq!(tree.lookup(0x1111_1111, b"missing"), None);
    assert_eq!(
        tree.inorder_keys(),
        vec![
            b"z".to_vec(),
            b"aa".to_vec(),
            b"aaa".to_vec(),
            b"ab".to_vec()
        ]
    );
}
