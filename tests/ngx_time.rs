//! Real pd-vm tests for L1 ngx time hosts.
//!
//! `ngx::time` is integer cached seconds. `ngx::now` is cached seconds plus
//! milliseconds as a float. `ngx::update_time` refreshes the process-local
//! cached clock used by these hosts.

use ngx_rustscript_nginx_test_stubs as _;

use std::sync::Arc;

use ngx_rustscript::access::{on_async_ready, run_access_program};
use ngx_rustscript::config::compile_rss_source;
use ngx_rustscript::host::{
    bind_ngx_hosts, clear_os_clock_override, notify_cached_clock_event_boundary, override_os_clock,
};
use vm::{Program, Value, Vm, VmStatus};

/// Serializes tests that share process-global stub or zone state.
fn clock_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|error| error.into_inner())
}

fn compile(source: &str) -> Arc<Program> {
    compile_rss_source(source).unwrap_or_else(|err| panic!("compile failed: {err}"))
}

fn eval(source: &str) -> Value {
    let program = compile(source);
    let mut vm = Vm::try_new((*program).clone()).expect("construct vm");
    bind_ngx_hosts(&mut vm);
    let status = vm.run().unwrap_or_else(|err| panic!("run failed: {err}"));
    assert_eq!(status, VmStatus::Halted, "time hosts are synchronous");
    vm.stack()
        .last()
        .cloned()
        .unwrap_or_else(|| panic!("expected a stack value for {source}"))
}

fn eval_err(source: &str) -> String {
    let program = compile(source);
    let mut vm = Vm::try_new((*program).clone()).expect("construct vm");
    bind_ngx_hosts(&mut vm);
    match vm.run() {
        Err(err) => err.to_string(),
        Ok(status) => panic!("expected host failure, got {status:?}"),
    }
}

fn expect_string(value: Value) -> String {
    match value {
        Value::String(text) => (*text).clone(),
        other => panic!("expected string, got {other:?}"),
    }
}

/// Unwraps a canonical guest `Option` carrier into its payload.
fn option_payload(value: &Value) -> Option<Value> {
    let (some, payload) = vm::option::payload(value).expect("Option carrier");
    if some {
        Some(payload.expect("Some payload").clone())
    } else {
        None
    }
}

fn expect_option_int(value: Value) -> Option<i64> {
    option_payload(&value).map(|payload| match payload {
        Value::Int(number) => number,
        other => panic!("expected int payload, got {other:?}"),
    })
}

fn expect_int(value: Value) -> i64 {
    match value {
        Value::Int(number) => number,
        other => panic!("expected int, got {other:?}"),
    }
}

fn expect_float(value: Value) -> f64 {
    match value {
        Value::Float(number) => number,
        other => panic!("expected float, got {other:?}"),
    }
}

fn reset_clock(sec: i64, msec: u32) {
    override_os_clock(sec, msec);
    notify_cached_clock_event_boundary();
}

#[test]
fn http_cookie_and_parse_time_are_fixed() {
    let _guard = clock_lock();
    assert_eq!(
        expect_string(eval("use ngx;\nngx::http_time(1290079655);\n")),
        "Thu, 18 Nov 2010 11:27:35 GMT"
    );
    assert_eq!(
        expect_string(eval("use ngx;\nngx::cookie_time(1290079655);\n")),
        "Thu, 18-Nov-10 11:27:35 GMT"
    );
    assert_eq!(
        expect_int(eval(
            "use ngx;\nngx::parse_http_time(\"Thu, 18 Nov 2010 11:27:35 GMT\");\n"
        )),
        1290079655
    );
    assert_eq!(
        eval("use ngx;\nngx::parse_http_time(\"not a date\");\n"),
        Value::Null
    );
}

#[test]
fn parse_http_time_accepts_rfc850_and_asctime() {
    let _guard = clock_lock();
    assert_eq!(
        expect_option_int(eval(
            "use ngx;\nngx::parse_http_time(\"Thursday, 18-Nov-10 11:27:35 GMT\");\n"
        )),
        Some(1290079655)
    );
    assert_eq!(
        expect_option_int(eval(
            "use ngx;\nngx::parse_http_time(\"Thu Nov 18 11:27:35 2010\");\n"
        )),
        Some(1290079655)
    );
}

#[test]
fn parse_http_time_rejects_pre_1970_and_over_128_bytes() {
    let _guard = clock_lock();
    assert_eq!(
        expect_option_int(eval(
            "use ngx;\nngx::parse_http_time(\"Wed, 31 Dec 1969 23:59:59 GMT\");\n"
        )),
        None,
        "pre-1970 dates are Option::None"
    );
    let over = "x".repeat(129);
    let err = eval_err(&format!("use ngx;\nngx::parse_http_time(\"{over}\");\n"));
    let lower = err.to_ascii_lowercase();
    assert!(
        lower.contains("128") || lower.contains("too long") || lower.contains("limit"),
        "parse_http_time must reject inputs over 128 bytes, got {err}"
    );
}

#[test]
fn time_is_int_and_now_is_float_from_the_same_cache() {
    let _guard = clock_lock();
    let seconds = expect_int(eval("use ngx;\nngx::time();\n"));
    let now = expect_float(eval("use ngx;\nngx::now();\n"));
    assert!(
        seconds > 1_600_000_000,
        "unix seconds should be current, got {seconds}"
    );
    assert!(
        (now.trunc() - seconds as f64).abs() < 2.0,
        "ngx::now ({now}) and ngx::time ({seconds}) must share the cached second"
    );
    assert!(now >= seconds as f64, "ngx::now includes milliseconds");
}

#[test]
fn today_localtime_and_utctime_use_fixed_formats() {
    let _guard = clock_lock();
    let today = expect_string(eval("use ngx;\nngx::today();\n"));
    assert!(
        today.chars().count() == 10 && today.as_bytes()[4] == b'-' && today.as_bytes()[7] == b'-',
        "ngx::today must be yyyy-mm-dd, got {today}"
    );
    let local = expect_string(eval("use ngx;\nngx::localtime();\n"));
    assert!(
        local.len() == 19 && local.as_bytes()[10] == b' ',
        "ngx::localtime must be yyyy-mm-dd hh:mm:ss, got {local}"
    );
    let utc = expect_string(eval("use ngx;\nngx::utctime();\n"));
    assert!(
        utc.len() == 19 && utc.as_bytes()[10] == b' ',
        "ngx::utctime must be yyyy-mm-dd hh:mm:ss, got {utc}"
    );
}

#[test]
fn intra_script_now_reads_share_the_cached_snapshot() {
    let _guard = clock_lock();
    reset_clock(1_700_000_000, 250);
    let value = eval("use ngx;\nlet a = ngx::now();\nlet b = ngx::now();\na == b;\n");
    assert_eq!(value, Value::Bool(true));
    clear_os_clock_override();
}

#[test]
fn event_boundary_and_update_time_refresh_cached_clock_across_vms() {
    let _guard = clock_lock();
    reset_clock(1_700_000_000, 0);
    let first = expect_float(eval("use ngx;\nngx::now();\n"));
    assert!((first - 1_700_000_000.0).abs() < f64::EPSILON);

    override_os_clock(1_700_000_100, 500);
    let cached = expect_float(eval("use ngx;\nngx::now();\n"));
    assert_eq!(
        cached, first,
        "without an event/request boundary, ngx::now keeps the cached snapshot"
    );

    notify_cached_clock_event_boundary();
    let after_boundary = expect_float(eval("use ngx;\nngx::now();\n"));
    assert!(
        (after_boundary - 1_700_000_100.5).abs() < 1e-9,
        "event/request boundary must refresh the cached clock, got {after_boundary}"
    );

    override_os_clock(1_700_000_200, 0);
    let still_cached = expect_float(eval("use ngx;\nngx::now();\n"));
    assert_eq!(still_cached, after_boundary);

    let updated = expect_float(eval("use ngx;\nngx::update_time();\nngx::now();\n"));
    assert!(
        (updated - 1_700_000_200.0).abs() < f64::EPSILON,
        "ngx::update_time must sample the OS clock, got {updated}"
    );
    clear_os_clock_override();
}

#[test]
fn access_program_turn_refreshes_cached_clock_without_direct_hook() {
    let _guard = clock_lock();
    override_os_clock(1_710_000_000, 0);
    let primed = expect_float(eval("use ngx;\nngx::update_time();\nngx::now();\n"));
    assert!((primed - 1_710_000_000.0).abs() < f64::EPSILON);

    override_os_clock(1_710_000_050, 250);
    let cached = expect_float(eval("use ngx;\nngx::now();\n"));
    assert_eq!(
        cached, primed,
        "cached clock must hold until a production request/event turn"
    );

    let program = compile("let x = 1;\n");
    let status = run_access_program(&program).expect("sync access program");
    assert_eq!(status, VmStatus::Halted);

    let after_request = expect_float(eval("use ngx;\nngx::now();\n"));
    assert!(
        (after_request - 1_710_000_050.25).abs() < 1e-9,
        "run_access_program must mark a cached-clock event boundary, got {after_request}"
    );
    clear_os_clock_override();
}

#[test]
fn async_ready_event_turn_refreshes_cached_clock_without_direct_hook() {
    let _guard = clock_lock();
    override_os_clock(1_720_000_000, 0);
    let primed = expect_float(eval("use ngx;\nngx::update_time();\nngx::now();\n"));
    assert!((primed - 1_720_000_000.0).abs() < f64::EPSILON);

    override_os_clock(1_720_000_080, 500);
    let cached = expect_float(eval("use ngx;\nngx::now();\n"));
    assert_eq!(cached, primed);

    on_async_ready(Vec::new());

    let after_event = expect_float(eval("use ngx;\nngx::now();\n"));
    assert!(
        (after_event - 1_720_000_080.5).abs() < 1e-9,
        "on_async_ready must mark a cached-clock event boundary, got {after_event}"
    );
    clear_os_clock_override();
}

#[test]
fn update_time_samples_os_immediately_without_event_turn() {
    let _guard = clock_lock();
    override_os_clock(1_730_000_000, 0);
    let primed = expect_float(eval("use ngx;\nngx::update_time();\nngx::now();\n"));
    assert!((primed - 1_730_000_000.0).abs() < f64::EPSILON);

    override_os_clock(1_730_000_009, 0);
    let updated = expect_float(eval("use ngx;\nngx::update_time();\nngx::now();\n"));
    assert!(
        (updated - 1_730_000_009.0).abs() < f64::EPSILON,
        "ngx::update_time must sample immediately, got {updated}"
    );
    clear_os_clock_override();
}
