//! Shared-dict list, flush, and key-enumeration hosts (Task 15).

use ngx_rustscript_nginx_test_stubs as _;

use std::sync::Arc;

use ngx_rustscript::config::compile_rss_source;
use ngx_rustscript::host::{
    HostCapability, HostFamily, HostSyncKind, NGX_SHARED_FLUSH_ALL_NAME,
    NGX_SHARED_FLUSH_EXPIRED_NAME, NGX_SHARED_GET_KEYS_NAME, NGX_SHARED_LLEN_NAME,
    NGX_SHARED_LPOP_NAME, NGX_SHARED_LPUSH_NAME, NGX_SHARED_RPOP_NAME, NGX_SHARED_RPUSH_NAME,
    NgxPhase, bind_ngx_hosts, host_allowed_in_phase, manifest_entry, ngx_host_catalog,
    production_runtime_host_names,
};
use ngx_rustscript::shared_dict::{
    self, DictValue, MAX_FLUSH_BATCH, MAX_GET_KEYS, MAX_LIST_LEN, StoreMode, install_process_zone,
    reset_process_zones, set_clock_ms,
};
use vm::{HostParamPassing, HostTypeSchema, Program, ResourceTypeKey, Value, Vm, VmStatus};

fn compile(source: &str) -> Arc<Program> {
    compile_rss_source(source).unwrap_or_else(|err| panic!("compile failed: {err}\n{source}"))
}

fn eval(source: &str) -> Value {
    reset_process_zones();
    install_process_zone("dogs", 64 * 1024);
    eval_on_zone(source)
}

fn eval_on_zone(source: &str) -> Value {
    let program = compile(source);
    let mut vm = Vm::try_new((*program).clone()).expect("construct vm");
    bind_ngx_hosts(&mut vm);
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

fn expect_string(value: &Value) -> String {
    match value {
        Value::String(text) => (**text).clone(),
        other => panic!("expected string, got {other:?}"),
    }
}

fn expect_array(value: &Value) -> &[Value] {
    match value {
        Value::Array(items) => items,
        other => panic!("expected array, got {other:?}"),
    }
}

const LIST_HOSTS: &[&str] = &[
    NGX_SHARED_LPUSH_NAME,
    NGX_SHARED_RPUSH_NAME,
    NGX_SHARED_LPOP_NAME,
    NGX_SHARED_RPOP_NAME,
    NGX_SHARED_LLEN_NAME,
    NGX_SHARED_FLUSH_ALL_NAME,
    NGX_SHARED_FLUSH_EXPIRED_NAME,
    NGX_SHARED_GET_KEYS_NAME,
];

#[test]
fn list_and_maintenance_hosts_are_catalogued() {
    assert_eq!(
        production_runtime_host_names().len(),
        191,
        "production host inventory must stay in sync"
    );
    let key = ResourceTypeKey::new("ngx.shared.dict").unwrap();
    let catalog = ngx_host_catalog();
    for name in LIST_HOSTS {
        let entry = manifest_entry(name).unwrap_or_else(|| panic!("missing {name}"));
        assert_eq!(entry.family, HostFamily::SharedDict);
        assert_eq!(entry.sync, HostSyncKind::Sync);
        assert_eq!(entry.capabilities, &[HostCapability::SharedDict]);
        assert!(entry.openresty_name.starts_with("ngx.shared.DICT"));
        assert!(host_allowed_in_phase(name, NgxPhase::Access));
        assert!(host_allowed_in_phase(name, NgxPhase::Init));
        assert!(!host_allowed_in_phase(name, NgxPhase::InitWorker));
        assert!(!host_allowed_in_phase(name, NgxPhase::ExitWorker));
        let schema = catalog
            .function(name)
            .unwrap_or_else(|| panic!("missing schema {name}"));
        assert_eq!(schema.params[0].passing, HostParamPassing::Borrow);
        assert_eq!(schema.params[0].ty, HostTypeSchema::Resource(key.clone()));
        match *name {
            NGX_SHARED_LPUSH_NAME | NGX_SHARED_RPUSH_NAME => {
                assert_eq!(schema.params.len(), 3);
                assert_eq!(schema.params[1].ty, HostTypeSchema::String);
                assert_eq!(schema.params[2].ty, HostTypeSchema::Unknown);
                assert_eq!(schema.return_type, HostTypeSchema::Unknown);
            }
            NGX_SHARED_LPOP_NAME | NGX_SHARED_RPOP_NAME | NGX_SHARED_LLEN_NAME => {
                assert_eq!(schema.params.len(), 2);
                assert_eq!(schema.params[1].ty, HostTypeSchema::String);
                assert_eq!(schema.return_type, HostTypeSchema::Unknown);
            }
            NGX_SHARED_FLUSH_ALL_NAME => {
                assert_eq!(schema.params.len(), 1);
                assert_eq!(schema.return_type, HostTypeSchema::Null);
            }
            NGX_SHARED_FLUSH_EXPIRED_NAME => {
                assert_eq!(schema.params.len(), 2);
                assert_eq!(schema.params[1].ty, HostTypeSchema::Unknown);
                assert_eq!(schema.return_type, HostTypeSchema::Int);
            }
            NGX_SHARED_GET_KEYS_NAME => {
                assert_eq!(schema.params.len(), 2);
                assert_eq!(schema.params[1].ty, HostTypeSchema::Unknown);
                assert_eq!(
                    schema.return_type,
                    HostTypeSchema::Array(Box::new(HostTypeSchema::String))
                );
            }
            _ => unreachable!(),
        }
        compile(&match *name {
            NGX_SHARED_LPUSH_NAME => {
                "use ngx;\nlet d = ngx::shared::dict(\"dogs\");\nngx::shared::lpush(d, \"q\", 1);\n"
                    .to_string()
            }
            NGX_SHARED_RPUSH_NAME => {
                "use ngx;\nlet d = ngx::shared::dict(\"dogs\");\nngx::shared::rpush(d, \"q\", 1);\n"
                    .to_string()
            }
            NGX_SHARED_LPOP_NAME => {
                "use ngx;\nlet d = ngx::shared::dict(\"dogs\");\nngx::shared::lpop(d, \"q\");\n"
                    .to_string()
            }
            NGX_SHARED_RPOP_NAME => {
                "use ngx;\nlet d = ngx::shared::dict(\"dogs\");\nngx::shared::rpop(d, \"q\");\n"
                    .to_string()
            }
            NGX_SHARED_LLEN_NAME => {
                "use ngx;\nlet d = ngx::shared::dict(\"dogs\");\nngx::shared::llen(d, \"q\");\n"
                    .to_string()
            }
            NGX_SHARED_FLUSH_ALL_NAME => {
                "use ngx;\nlet d = ngx::shared::dict(\"dogs\");\nngx::shared::flush_all(d);\n"
                    .to_string()
            }
            NGX_SHARED_FLUSH_EXPIRED_NAME => {
                "use ngx;\nlet d = ngx::shared::dict(\"dogs\");\nngx::shared::flush_expired(d, null);\n"
                    .to_string()
            }
            NGX_SHARED_GET_KEYS_NAME => {
                "use ngx;\nlet d = ngx::shared::dict(\"dogs\");\nngx::shared::get_keys(d, null);\n"
                    .to_string()
            }
            _ => unreachable!(),
        });
    }
}

#[test]
fn lpush_rpush_preserve_order_and_llen() {
    let result = eval(
        r#"
use ngx;
let d = ngx::shared::dict("dogs");
ngx::shared::rpush(d, "q", "a");
ngx::shared::rpush(d, "q", "b");
ngx::shared::lpush(d, "q", "z");
ngx::shared::llen(d, "q");
"#,
    );
    assert_eq!(expect_int(map_get(&result, "value")), 3);
    assert_eq!(*map_get(&result, "error"), Value::Null);

    let left = eval(
        r#"
use ngx;
let d = ngx::shared::dict("dogs");
ngx::shared::rpush(d, "q", "a");
ngx::shared::rpush(d, "q", "b");
ngx::shared::lpush(d, "q", "z");
ngx::shared::lpop(d, "q");
"#,
    );
    assert_eq!(expect_string(map_get(&left, "value")), "z");

    let right = eval(
        r#"
use ngx;
let d = ngx::shared::dict("dogs");
ngx::shared::rpush(d, "q", "a");
ngx::shared::rpush(d, "q", "b");
ngx::shared::lpush(d, "q", "z");
ngx::shared::rpop(d, "q");
"#,
    );
    assert_eq!(expect_string(map_get(&right, "value")), "b");
}

#[test]
fn pop_and_llen_on_missing_key() {
    let llen = eval(
        r#"
use ngx;
let d = ngx::shared::dict("dogs");
ngx::shared::llen(d, "missing");
"#,
    );
    assert_eq!(expect_int(map_get(&llen, "value")), 0);
    assert_eq!(*map_get(&llen, "error"), Value::Null);

    let pop = eval(
        r#"
use ngx;
let d = ngx::shared::dict("dogs");
ngx::shared::lpop(d, "missing");
"#,
    );
    assert_eq!(*map_get(&pop, "value"), Value::Null);
    assert_eq!(*map_get(&pop, "error"), Value::Null);
}

#[test]
fn last_pop_removes_the_list_key() {
    let result = eval(
        r#"
use ngx;
let d = ngx::shared::dict("dogs");
ngx::shared::lpush(d, "q", 1);
ngx::shared::lpop(d, "q");
ngx::shared::llen(d, "q");
"#,
    );
    assert_eq!(expect_int(map_get(&result, "value")), 0);
}

#[test]
fn scalar_list_conflict_returns_structured_error_and_does_not_corrupt() {
    let push = eval(
        r#"
use ngx;
let d = ngx::shared::dict("dogs");
ngx::shared::set(d, "k", "scalar", null, 9);
ngx::shared::lpush(d, "k", "x");
"#,
    );
    assert_eq!(*map_get(&push, "value"), Value::Null);
    assert_eq!(expect_string(map_get(&push, "error")), "value not a list");

    let got = eval(
        r#"
use ngx;
let d = ngx::shared::dict("dogs");
ngx::shared::set(d, "k", "scalar", null, 9);
ngx::shared::lpush(d, "k", "x");
ngx::shared::get(d, "k");
"#,
    );
    assert_eq!(expect_string(map_get(&got, "value")), "scalar");
    assert_eq!(expect_int(map_get(&got, "flags")), 9);

    let llen = eval(
        r#"
use ngx;
let d = ngx::shared::dict("dogs");
ngx::shared::set(d, "k", "scalar", null, null);
ngx::shared::llen(d, "k");
"#,
    );
    assert_eq!(*map_get(&llen, "value"), Value::Null);
    assert_eq!(expect_string(map_get(&llen, "error")), "value not a list");

    let pop = eval(
        r#"
use ngx;
let d = ngx::shared::dict("dogs");
ngx::shared::set(d, "k", "scalar", null, null);
ngx::shared::rpop(d, "k");
"#,
    );
    assert_eq!(*map_get(&pop, "value"), Value::Null);
    assert_eq!(expect_string(map_get(&pop, "error")), "value not a list");

    let list_get = eval(
        r#"
use ngx;
let d = ngx::shared::dict("dogs");
ngx::shared::lpush(d, "q", 1);
ngx::shared::get(d, "q");
"#,
    );
    assert_eq!(*map_get(&list_get, "value"), Value::Null);
    assert_eq!(
        expect_string(map_get(&list_get, "error")),
        "value is a list"
    );

    let incr = eval(
        r#"
use ngx;
let d = ngx::shared::dict("dogs");
ngx::shared::lpush(d, "q", 1);
ngx::shared::incr(d, "q", 1, null, null);
"#,
    );
    assert_eq!(expect_string(map_get(&incr, "error")), "not a number");
    let still = eval(
        r#"
use ngx;
let d = ngx::shared::dict("dogs");
ngx::shared::lpush(d, "q", 1);
ngx::shared::incr(d, "q", 1, null, null);
ngx::shared::llen(d, "q");
"#,
    );
    assert_eq!(expect_int(map_get(&still, "value")), 1);
}

#[test]
fn list_push_rejects_bool_and_null() {
    let bad = eval(
        r#"
use ngx;
let d = ngx::shared::dict("dogs");
ngx::shared::lpush(d, "q", true);
"#,
    );
    assert_eq!(*map_get(&bad, "value"), Value::Null);
    assert_eq!(expect_string(map_get(&bad, "error")), "bad value type");
    let nil = eval(
        r#"
use ngx;
let d = ngx::shared::dict("dogs");
ngx::shared::rpush(d, "q", null);
"#,
    );
    assert_eq!(expect_string(map_get(&nil, "error")), "bad value type");
}

#[test]
fn list_push_is_allocation_transactional() {
    reset_process_zones();
    install_process_zone("tiny", 80);
    let failed = shared_dict::dict_lpush("tiny", b"q", DictValue::String("x".repeat(200))).unwrap();
    assert_eq!(failed.error, Some("no memory"));
    assert_eq!(failed.value, None);
    let llen = shared_dict::dict_llen("tiny", b"q").unwrap();
    assert_eq!(llen.value, Some(DictValue::Int(0)));
    assert_eq!(llen.error, None);
}

#[test]
fn list_length_is_bounded() {
    assert_eq!(MAX_LIST_LEN, 65_536);
    reset_process_zones();
    install_process_zone("dogs", 16 * 1024 * 1024);
    for i in 0..MAX_LIST_LEN {
        let pushed = shared_dict::dict_lpush("dogs", b"q", DictValue::Int(i as i64)).unwrap();
        assert_eq!(pushed.error, None);
    }
    let pushed = shared_dict::dict_rpush("dogs", b"q", DictValue::Int(2)).unwrap();
    assert_eq!(pushed.error, Some("list too long"));
    let llen = shared_dict::dict_llen("dogs", b"q").unwrap();
    assert_eq!(llen.value, Some(DictValue::Int(MAX_LIST_LEN as i64)));
}

#[test]
fn flush_all_marks_expired_without_requiring_delete() {
    reset_process_zones();
    install_process_zone("dogs", 64 * 1024);
    set_clock_ms(Some(1_000_000));
    shared_dict::dict_store(
        "dogs",
        b"keep",
        Some(DictValue::String("v".into())),
        0.0,
        4,
        StoreMode::Set,
    )
    .unwrap();
    shared_dict::dict_lpush("dogs", b"q", DictValue::Int(1)).unwrap();
    shared_dict::dict_flush_all("dogs").unwrap();
    let live = shared_dict::dict_get("dogs", b"keep", false).unwrap();
    assert_eq!(live.value, None);
    let stale = shared_dict::dict_get("dogs", b"keep", true).unwrap();
    if stale.value.is_some() {
        assert!(stale.stale);
        assert_eq!(stale.flags, 4);
        assert_eq!(stale.value, Some(DictValue::String("v".into())));
    }
    let llen = shared_dict::dict_llen("dogs", b"q").unwrap();
    assert_eq!(llen.value, Some(DictValue::Int(0)));
}

#[test]
fn flush_expired_honors_max_and_hard_cap() {
    assert_eq!(MAX_FLUSH_BATCH, 1024);
    reset_process_zones();
    install_process_zone("dogs", 256 * 1024);
    set_clock_ms(Some(1_000_000));
    for i in 0..8 {
        let key = format!("k{i}");
        shared_dict::dict_store(
            "dogs",
            key.as_bytes(),
            Some(DictValue::Int(i)),
            0.05,
            0,
            StoreMode::Set,
        )
        .unwrap();
    }
    set_clock_ms(Some(1_000_100));
    let flushed = shared_dict::dict_flush_expired("dogs", Some(3)).unwrap();
    assert_eq!(flushed, 3);
    let rest = shared_dict::dict_flush_expired("dogs", Some(0)).unwrap();
    assert_eq!(rest, 5);
    assert_eq!(shared_dict::dict_flush_expired("dogs", None).unwrap(), 0);
}

#[test]
fn get_keys_skips_expired_and_obeys_hard_cap() {
    assert_eq!(MAX_GET_KEYS, 1024);
    reset_process_zones();
    install_process_zone("dogs", 512 * 1024);
    set_clock_ms(Some(2_000_000));
    shared_dict::dict_store(
        "dogs",
        b"live",
        Some(DictValue::Int(1)),
        0.0,
        0,
        StoreMode::Set,
    )
    .unwrap();
    shared_dict::dict_store(
        "dogs",
        b"soon",
        Some(DictValue::Int(2)),
        0.05,
        0,
        StoreMode::Set,
    )
    .unwrap();
    shared_dict::dict_lpush("dogs", b"queue", DictValue::Int(3)).unwrap();
    set_clock_ms(Some(2_000_100));
    let keys = shared_dict::dict_get_keys("dogs", Some(0)).unwrap();
    assert!(keys.iter().any(|k| k == b"live"));
    assert!(keys.iter().any(|k| k == b"queue"));
    assert!(!keys.iter().any(|k| k == b"soon"));

    let limited = eval(
        r#"
use ngx;
let d = ngx::shared::dict("dogs");
ngx::shared::set(d, "a", 1, null, null);
ngx::shared::set(d, "b", 2, null, null);
ngx::shared::set(d, "c", 3, null, null);
ngx::shared::get_keys(d, 1);
"#,
    );
    assert_eq!(expect_array(&limited).len(), 1);

    reset_process_zones();
    install_process_zone("dogs", 2 * 1024 * 1024);
    for i in 0..(MAX_GET_KEYS + 5) {
        let key = format!("k{i:04}");
        shared_dict::dict_store(
            "dogs",
            key.as_bytes(),
            Some(DictValue::Int(1)),
            0.0,
            0,
            StoreMode::Set,
        )
        .unwrap();
    }
    let capped = shared_dict::dict_get_keys("dogs", Some(0)).unwrap();
    assert_eq!(capped.len(), MAX_GET_KEYS);
}

#[test]
fn set_replaces_list_without_leaving_list_nodes() {
    let got = eval(
        r#"
use ngx;
let d = ngx::shared::dict("dogs");
ngx::shared::lpush(d, "k", "x");
ngx::shared::lpush(d, "k", "y");
ngx::shared::set(d, "k", "scalar", null, null);
ngx::shared::get(d, "k");
"#,
    );
    assert_eq!(expect_string(map_get(&got, "value")), "scalar");
}

#[test]
fn list_hosts_do_not_require_request_state() {
    let program = compile(
        r#"
use ngx;
let d = ngx::shared::dict("dogs");
ngx::shared::lpush(d, "q", 1);
ngx::shared::get_keys(d, null);
"#,
    );
    assert!(
        !ngx_rustscript::config::program_requires_request_state(&program),
        "list/maintenance programs may stay on the pooled sync path"
    );
}

#[test]
fn list_ops_keep_sibling_scalar_lookup() {
    reset_process_zones();
    install_process_zone("dogs", 64 * 1024);
    shared_dict::dict_store(
        "dogs",
        b"alpha",
        Some(DictValue::String("A".into())),
        0.0,
        0,
        StoreMode::Set,
    )
    .unwrap();
    shared_dict::dict_lpush("dogs", b"list", DictValue::Int(1)).unwrap();
    shared_dict::dict_rpush("dogs", b"list", DictValue::Int(2)).unwrap();
    shared_dict::dict_store(
        "dogs",
        b"omega",
        Some(DictValue::String("O".into())),
        0.0,
        0,
        StoreMode::Set,
    )
    .unwrap();
    shared_dict::dict_lpop("dogs", b"list").unwrap();
    let alpha = shared_dict::dict_get("dogs", b"alpha", false).unwrap();
    let omega = shared_dict::dict_get("dogs", b"omega", false).unwrap();
    assert_eq!(alpha.value, Some(DictValue::String("A".into())));
    assert_eq!(omega.value, Some(DictValue::String("O".into())));
    assert_eq!(
        shared_dict::dict_llen("dogs", b"list").unwrap().value,
        Some(DictValue::Int(1))
    );
}

#[test]
fn get_keys_is_read_only_and_preserves_every_live_key() {
    reset_process_zones();
    install_process_zone("dogs", 64 * 1024);
    set_clock_ms(Some(3_000_000));
    for (key, value) in [
        (b"a".as_slice(), DictValue::String("1".into())),
        (b"b".as_slice(), DictValue::String("2".into())),
        (b"c".as_slice(), DictValue::String("3".into())),
    ] {
        shared_dict::dict_store("dogs", key, Some(value), 0.0, 0, StoreMode::Set).unwrap();
    }
    shared_dict::dict_rpush("dogs", b"lst2", DictValue::String("x".into())).unwrap();
    let before = shared_dict::dict_get_keys("dogs", Some(0)).unwrap();
    assert_eq!(before.len(), 4);
    let after = shared_dict::dict_get_keys("dogs", Some(0)).unwrap();
    assert_eq!(after, before);
    for key in [b"a".as_slice(), b"b".as_slice(), b"c".as_slice()] {
        assert!(
            shared_dict::dict_get("dogs", key, false)
                .unwrap()
                .value
                .is_some(),
            "get_keys must not delete live key {}",
            String::from_utf8_lossy(key)
        );
    }
    assert_eq!(
        shared_dict::dict_llen("dogs", b"lst2").unwrap().value,
        Some(DictValue::Int(1))
    );
}

#[test]
fn get_keys_bounds_visits_independently_of_result_hits() {
    reset_process_zones();
    install_process_zone("dogs", 2 * 1024 * 1024);
    set_clock_ms(Some(4_000_000));
    for i in 0..(MAX_GET_KEYS + 1) {
        let key = format!("e{i:04}");
        shared_dict::dict_store(
            "dogs",
            key.as_bytes(),
            Some(DictValue::Int(i as i64)),
            0.05,
            0,
            StoreMode::Set,
        )
        .unwrap();
    }
    set_clock_ms(Some(4_000_100));
    shared_dict::dict_store(
        "dogs",
        b"live",
        Some(DictValue::Int(1)),
        0.0,
        0,
        StoreMode::Set,
    )
    .unwrap();
    let keys = shared_dict::dict_get_keys("dogs", Some(10)).unwrap();
    assert!(
        keys.is_empty(),
        "visit cap must stop before the live MRU key, got {keys:?}"
    );
    assert_eq!(
        shared_dict::dict_get("dogs", b"live", false).unwrap().value,
        Some(DictValue::Int(1))
    );
}

#[test]
fn flush_expired_bounds_visits_independently_of_hits() {
    reset_process_zones();
    install_process_zone("dogs", 2 * 1024 * 1024);
    set_clock_ms(Some(5_000_000));
    shared_dict::dict_store(
        "dogs",
        b"expired-lru",
        Some(DictValue::Int(0)),
        0.05,
        0,
        StoreMode::Set,
    )
    .unwrap();
    for i in 0..MAX_FLUSH_BATCH {
        let key = format!("live{i:04}");
        shared_dict::dict_store(
            "dogs",
            key.as_bytes(),
            Some(DictValue::Int(1)),
            0.0,
            0,
            StoreMode::Set,
        )
        .unwrap();
    }
    for i in 0..5 {
        let key = format!("expired-mru-{i}");
        shared_dict::dict_store(
            "dogs",
            key.as_bytes(),
            Some(DictValue::Int(2)),
            0.05,
            0,
            StoreMode::Set,
        )
        .unwrap();
    }
    set_clock_ms(Some(5_000_100));
    let freed = shared_dict::dict_flush_expired("dogs", Some(10)).unwrap();
    assert_eq!(freed, 1, "visit cap should free only the LRU expired key");
    let stale = shared_dict::dict_get("dogs", b"expired-mru-4", true).unwrap();
    assert!(
        stale.value.is_some(),
        "MRU expired keys must remain when the walk is visit-capped"
    );
    assert!(stale.stale);
}

#[test]
fn flush_all_repeated_calls_complete_beyond_batch() {
    reset_process_zones();
    install_process_zone("dogs", 4 * 1024 * 1024);
    set_clock_ms(Some(6_000_000));
    let total = MAX_FLUSH_BATCH + 76;
    for i in 0..total {
        let key = format!("k{i:04}");
        shared_dict::dict_store(
            "dogs",
            key.as_bytes(),
            Some(DictValue::Int(1)),
            0.0,
            0,
            StoreMode::Set,
        )
        .unwrap();
    }
    shared_dict::dict_flush_all("dogs").unwrap();
    let oldest = shared_dict::dict_get("dogs", b"k0000", false).unwrap();
    let newest = shared_dict::dict_get("dogs", b"k1099", false).unwrap();
    assert!(
        oldest.value.is_none(),
        "first flush_all batch must expire the LRU key"
    );
    assert_eq!(
        newest.value,
        Some(DictValue::Int(1)),
        "first flush_all batch must not wipe keys beyond the visit cap"
    );
    let mut newest_live = true;
    for _ in 2..=8 {
        shared_dict::dict_flush_all("dogs").unwrap();
        newest_live = shared_dict::dict_get("dogs", b"k1099", false)
            .unwrap()
            .value
            .is_some();
        if !newest_live {
            break;
        }
    }
    assert!(
        !newest_live,
        "repeated flush_all must finish more than {MAX_FLUSH_BATCH} keys"
    );
    let leftover = shared_dict::dict_get_keys("dogs", Some(0)).unwrap();
    assert!(
        leftover.is_empty(),
        "get_keys must see no live keys after flush_all completes, leftover {}",
        leftover.len()
    );
}
