//! Real pd-vm tests for L1 ngx time hosts.
//!
//! `ngx::time` is integer cached seconds. `ngx::now` is cached seconds plus
//! milliseconds as a float. `ngx::update_time` refreshes the process-local
//! cached clock used by these hosts.

use ngx_rustscript_nginx_test_stubs as _;

use std::sync::Arc;
use std::thread;
use std::time::Duration;

use ngx_rustscript::config::compile_rss_source;
use ngx_rustscript::host::bind_ngx_hosts;
use vm::{Program, Value, Vm, VmStatus};

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

fn expect_string(value: Value) -> String {
    match value {
        Value::String(text) => (*text).clone(),
        other => panic!("expected string, got {other:?}"),
    }
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

#[test]
fn http_cookie_and_parse_time_are_fixed() {
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
fn time_is_int_and_now_is_float_from_the_same_cache() {
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
fn update_time_refreshes_the_cached_clock() {
    let first = expect_float(eval("use ngx;\nngx::now();\n"));
    thread::sleep(Duration::from_millis(30));
    let cached = expect_float(eval("use ngx;\nngx::now();\n"));
    assert_eq!(
        first, cached,
        "ngx::now must keep returning the cached snapshot until update_time"
    );
    thread::sleep(Duration::from_millis(30));
    let updated = expect_float(eval("use ngx;\nngx::update_time();\nngx::now();\n"));
    assert!(
        updated > cached,
        "ngx::update_time must refresh the cached clock, cached={cached} updated={updated}"
    );
}
