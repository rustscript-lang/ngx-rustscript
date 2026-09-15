//! Real pinned NGINX 1.30.4 lifecycle tests for `ngx::timer`.
//!
//! Covers detached lifetime, owned callback execution, shared-dict/log markers
//! after the creating request finished, `premature=false`/`true`, `ngx::sleep`
//! resumption, outbound async work, Timer-phase capability denial, `every`
//! round serialization with persistent captures, pending/running counts,
//! reload isolation, and graceful worker shutdown.
//!
//! All cases use the shared typed harness (`tests/support/nginx_harness.rs`)
//! and run serially with `--test-threads=1`. Each case owns an isolated prefix
//! and process group; the harness Drop removes both.

mod support;

use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;
use std::time::{Duration, Instant};

use support::nginx_harness::{HttpRequest, NginxConfig, NginxHarness, RunningNginx};

const ERROR_LOG: &str = "logs/error.log";
const READY: Duration = Duration::from_secs(8);
const QUIET: Duration = Duration::from_secs(8);

fn timers_config(locations: &str) -> NginxConfig {
    NginxConfig::default()
        .master_process(true)
        .workers(1)
        .error_log_level("error")
        .http_directives("rss_shared_dict timers 1m;")
        .server_directives(locations)
}

/// Access-phase location.
///
/// No `return` directive: `return` is a rewrite-phase handler that finalizes
/// the request before the access phase, so the script would never run. The
/// scripts finalize their own 200 with `ngx::exit`.
fn access_location(path: &str, script: &str) -> String {
    format!("location {path} {{ access_by_rss_file \"@@PREFIX@@/{script}\"; }}\n")
}

fn content_location(path: &str, script: &str) -> String {
    format!("location {path} {{ content_by_rss_file \"@@PREFIX@@/{script}\"; }}\n")
}

fn get(path: &str) -> HttpRequest {
    HttpRequest::get(path)
}

/// Shared-dict result structs. The host catalog exposes the fixed
/// multi-return shared-dict calls as `unknown`, so a script that consumes one
/// declares the matching struct and annotates the binding.
const RSS_SHARED_RESULT_STRUCTS: &str = r#"
struct SharedGetResult<T> {
    value: T?,
    flags: int,
    stale: bool,
    error: string?
}

struct SharedStoreResult {
    ok: bool,
    error: string?,
    forcible: bool
}

struct SharedIncrResult {
    value: number?,
    error: string?,
    forcible: bool
}
"#;

fn write_script(harness: &NginxHarness, path: &str, contents: &str) {
    let source = format!("{RSS_SHARED_RESULT_STRUCTS}{contents}");
    harness
        .write_file(path, source)
        .unwrap_or_else(|err| panic!("write {path}: {err}"));
}

fn error_log_text(harness: &NginxHarness) -> String {
    std::fs::read_to_string(harness.prefix().join(ERROR_LOG)).unwrap_or_default()
}

/// Combined worker log: the configured error log plus the captured stderr that
/// carries cycle-log messages emitted outside a request.
fn marker_text(harness: &NginxHarness, running: &RunningNginx) -> String {
    let stderr = std::fs::read_to_string(running.stderr_log_path()).unwrap_or_default();
    format!("{}\n{stderr}", error_log_text(harness))
}

fn response_or_panic(
    harness: &NginxHarness,
    running: &mut RunningNginx,
    request: &HttpRequest,
    what: &str,
) -> support::nginx_harness::HttpResponse {
    running.request(request).unwrap_or_else(|err| {
        panic!(
            "{what} failed: {err}\nerror log:\n{}",
            error_log_text(harness)
        )
    })
}

fn wait_marker(harness: &NginxHarness, running: &mut RunningNginx, needle: &str) {
    let deadline = Instant::now() + READY;
    while Instant::now() < deadline {
        if marker_text(harness, running).contains(needle) {
            return;
        }
        thread::sleep(Duration::from_millis(20));
    }
    panic!(
        "missing marker {needle}; worker log:\n{}",
        marker_text(harness, running)
    );
}

fn wait_absent(harness: &NginxHarness, running: &RunningNginx, needle: &str, quiet: Duration) {
    let deadline = Instant::now() + quiet;
    while Instant::now() < deadline {
        let text = marker_text(harness, running);
        assert!(
            !text.contains(needle),
            "{needle} must not appear in the worker log:\n{text}"
        );
        thread::sleep(Duration::from_millis(25));
    }
}

fn wait_min_occurrences(
    harness: &NginxHarness,
    running: &RunningNginx,
    needle: &str,
    min: usize,
    timeout: Duration,
) -> String {
    let deadline = Instant::now() + timeout;
    loop {
        let text = marker_text(harness, running);
        if text.matches(needle).count() >= min {
            return text;
        }
        if Instant::now() >= deadline {
            panic!("expected at least {min} occurrences of {needle}, log:\n{text}");
        }
        thread::sleep(Duration::from_millis(20));
    }
}

/// Number of times `needle` occurs in the combined worker log.
fn marker_count(harness: &NginxHarness, running: &RunningNginx, needle: &str) -> usize {
    marker_text(harness, running).matches(needle).count()
}

/// Polls `/observe` until its body reports `token`, returning the body.
fn wait_for_observe_token(
    harness: &NginxHarness,
    running: &mut RunningNginx,
    token: &str,
    timeout: Duration,
) -> String {
    let deadline = Instant::now() + timeout;
    loop {
        let response = response_or_panic(harness, running, &get("/observe"), "observe");
        let body = response.body_text();
        if body.lines().any(|line| line.trim() == token) {
            return body;
        }
        if Instant::now() >= deadline {
            panic!(
                "observe never reported {token}; last body {body:?}\n{}",
                error_log_text(harness)
            );
        }
        thread::sleep(Duration::from_millis(5));
    }
}

/// One `/observe` sample that must report a live registration count pair.
fn observe_sample(
    harness: &NginxHarness,
    running: &mut RunningNginx,
    expected_token: &str,
) -> String {
    let response = response_or_panic(harness, running, &get("/observe"), "observe");
    let body = response.body_text();
    let token = body
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with('P') && line.contains('R'))
        .unwrap_or_else(|| panic!("observe body has no counter token: {body:?}"));
    assert_eq!(
        token,
        expected_token,
        "unexpected timer counts (body {body:?}):\n{}",
        error_log_text(harness)
    );
    body
}

fn location(path: &str, script: &str) -> String {
    access_location(path, script)
}

#[test]
fn timer_at_fires_after_the_creating_request_completed() {
    let harness = NginxHarness::new("timer-nginx-at-fire");
    write_script(
        &harness,
        "scripts/at_fire.rss",
        r#"
use ngx;
let d = ngx::shared::dict("timers");
ngx::timer::at(5, |premature| if true => {
    let d = ngx::shared::dict("timers");
    ngx::shared::set(d, "at_fired", "yes", null, null);
    ngx::log(ngx::log_level::error(), "TIMER_AT_FIRED");
    null
} else => { null });
ngx::shared::set(d, "creating_done", "yes", null, null);
assert(ngx::timer::pending_count() == 1);
ngx::log(ngx::log_level::error(), "DIAG pending_count is 1 after registration");
ngx::exit(200);
"#,
    );
    write_script(
        &harness,
        "scripts/read_fired.rss",
        r#"
use ngx;
let d = ngx::shared::dict("timers");
let r: SharedGetResult<string> = ngx::shared::get(d, "at_fired");
ngx::say(r.value);
"#,
    );
    let locations = format!(
        "{}{}",
        location("/at", "scripts/at_fire.rss"),
        content_location("/fired", "scripts/read_fired.rss")
    );
    harness.write_config(&timers_config(&locations));
    let mut running = harness.start();

    let start = Instant::now();
    let response = response_or_panic(&harness, &mut running, &get("/at"), "register request");
    assert_eq!(response.status, 200, "{}", error_log_text(&harness));
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "registration must return immediately, not wait for the 5s deadline; took {:?}",
        start.elapsed()
    );
    let immediate = response_or_panic(&harness, &mut running, &get("/fired"), "early reader");
    assert_ne!(
        immediate.body_text().trim(),
        "yes",
        "the callback must not have run before the creating request completed"
    );

    wait_marker(&harness, &mut running, "TIMER_AT_FIRED");
    let fired = response_or_panic(&harness, &mut running, &get("/fired"), "reader");
    assert_eq!(
        fired.body_text().trim(),
        "yes",
        "the callback's shared-dict marker must be observable after the request ended:\n{}",
        error_log_text(&harness)
    );
    running.graceful_quit(QUIET).expect("graceful quit");
}

#[test]
fn timer_at_passes_premature_false_on_normal_expiry() {
    let harness = NginxHarness::new("timer-nginx-premature-false");
    write_script(
        &harness,
        "scripts/premature.rss",
        r#"
use ngx;
ngx::timer::at(0.05, |premature| if premature => {
    ngx::log(ngx::log_level::error(), "TIMER_PREMATURE_TRUE");
    null
} else => {
    ngx::log(ngx::log_level::error(), "TIMER_PREMATURE_FALSE");
    null
});
ngx::exit(200);
"#,
    );
    harness.write_config(&timers_config(&location("/start", "scripts/premature.rss")));
    let mut running = harness.start();
    let response = response_or_panic(&harness, &mut running, &get("/start"), "register");
    assert_eq!(response.status, 200, "{}", error_log_text(&harness));
    wait_marker(&harness, &mut running, "TIMER_PREMATURE_FALSE");
    wait_absent(
        &harness,
        &running,
        "TIMER_PREMATURE_TRUE",
        Duration::from_millis(200),
    );
    running.graceful_quit(QUIET).expect("graceful quit");
}

#[test]
fn timer_callback_resumes_after_ngx_sleep() {
    let harness = NginxHarness::new("timer-nginx-sleep");
    write_script(
        &harness,
        "scripts/sleep.rss",
        r#"
use ngx;
ngx::timer::at(0.05, |premature| if true => {
    ngx::sleep_ms(40);
    ngx::log(ngx::log_level::error(), "TIMER_SLEPT");
    null
} else => { null });
ngx::exit(200);
"#,
    );
    harness.write_config(&timers_config(&location("/start", "scripts/sleep.rss")));
    let mut running = harness.start();
    let response = response_or_panic(&harness, &mut running, &get("/start"), "register");
    assert_eq!(response.status, 200, "{}", error_log_text(&harness));
    wait_marker(&harness, &mut running, "TIMER_SLEPT");
    running.graceful_quit(QUIET).expect("graceful quit");
}

#[test]
fn timer_callback_performs_outbound_async_work() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("echo listener");
    let port = listener.local_addr().expect("listener addr").port();
    let echo = thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buffer = [0u8; 4];
            let _ = stream.read_exact(&mut buffer);
            let _ = stream.write_all(b"pong");
            let _ = stream.flush();
        }
    });

    let harness = NginxHarness::new("timer-nginx-outbound");
    write_script(
        &harness,
        "scripts/outbound.rss",
        &format!(
            r#"
use ngx;
ngx::timer::at(0.05, |premature| if true => {{
    let s = ngx::socket::tcp();
    ngx::socket::tcp::connect(s, "127.0.0.1", {port});
    ngx::socket::tcp::send(s, "ping");
    ngx::socket::tcp::close(s);
    ngx::log(ngx::log_level::error(), "TIMER_TCP_OK");
    null
}} else => {{ null }});
ngx::exit(200);
"#
        ),
    );
    harness.write_config(&timers_config(&location("/start", "scripts/outbound.rss")));
    let mut running = harness.start();
    let response = response_or_panic(&harness, &mut running, &get("/start"), "register");
    assert_eq!(response.status, 200, "{}", error_log_text(&harness));
    wait_marker(&harness, &mut running, "TIMER_TCP_OK");
    let _ = echo.join();
    running.graceful_quit(QUIET).expect("graceful quit");
}

#[test]
fn timer_callback_cannot_use_request_hosts_and_keeps_serving() {
    let harness = NginxHarness::new("timer-nginx-denied");
    write_script(
        &harness,
        "scripts/denied.rss",
        r#"
use ngx;
ngx::timer::at(0.05, |premature| if true => {
    ngx::req::get_method();
    null
} else => { null });
ngx::exit(200);
"#,
    );
    harness.write_config(&timers_config(&location("/start", "scripts/denied.rss")));
    let mut running = harness.start();
    let response = response_or_panic(&harness, &mut running, &get("/start"), "register");
    assert_eq!(response.status, 200, "{}", error_log_text(&harness));

    // The request-only host fails closed inside the timer phase and the failure
    // is reported through the worker log instead of unwinding the worker.
    wait_marker(&harness, &mut running, "ngx.timer callback");
    let health = response_or_panic(&harness, &mut running, &get("/start"), "follow-up health");
    assert_eq!(health.status, 200, "{}", error_log_text(&harness));
    running.graceful_quit(QUIET).expect("graceful quit");
}

#[test]
fn timer_every_serializes_rounds_with_a_persistent_capture() {
    let harness = NginxHarness::new("timer-nginx-every");
    write_script(
        &harness,
        "scripts/every.rss",
        r#"
use ngx;
let mut rounds = 0;
ngx::timer::every(0.05, |premature| if true => {
    rounds = rounds + 1;
    let d = ngx::shared::dict("timers");
    ngx::shared::set(d, "rounds", rounds, null, null);
    null
} else => { null });
ngx::exit(200);
"#,
    );
    write_script(
        &harness,
        "scripts/read_rounds.rss",
        r#"
use ngx;
let d = ngx::shared::dict("timers");
let r: SharedGetResult<int> = ngx::shared::get(d, "rounds");
if r.value == 3 { ngx::say("ROUNDS_3"); } else { ngx::say("ROUNDS_OTHER"); }
"#,
    );
    let locations = format!(
        "{}{}",
        location("/start", "scripts/every.rss"),
        content_location("/rounds", "scripts/read_rounds.rss")
    );
    harness.write_config(&timers_config(&locations));
    let mut running = harness.start();
    let response = response_or_panic(&harness, &mut running, &get("/start"), "register");
    assert_eq!(response.status, 200, "{}", error_log_text(&harness));

    let deadline = Instant::now() + READY;
    loop {
        let reader = response_or_panic(&harness, &mut running, &get("/rounds"), "rounds reader");
        if reader.body_text().trim() == "ROUNDS_3" {
            break;
        }
        if Instant::now() >= deadline {
            panic!(
                "the moved capture must reach three rounds; last body {:?}\n{}",
                reader.body_text(),
                error_log_text(&harness)
            );
        }
        thread::sleep(Duration::from_millis(25));
    }
    running.graceful_quit(QUIET).expect("graceful quit");
}

#[test]
fn timer_every_never_overlaps_a_slow_callback() {
    let harness = NginxHarness::new("timer-nginx-no-overlap");
    write_script(
        &harness,
        "scripts/slow.rss",
        r#"
use ngx;
ngx::timer::every(0.05, |premature| if true => {
    ngx::log(ngx::log_level::error(), "SLOW_START");
    ngx::sleep_ms(120);
    ngx::log(ngx::log_level::error(), "SLOW_END");
    null
} else => { null });
ngx::exit(200);
"#,
    );
    harness.write_config(&timers_config(&location("/start", "scripts/slow.rss")));
    let mut running = harness.start();
    let response = response_or_panic(&harness, &mut running, &get("/start"), "register");
    assert_eq!(response.status, 200, "{}", error_log_text(&harness));
    let text = wait_min_occurrences(&harness, &running, "SLOW_END", 2, READY);

    let mut sequence = Vec::new();
    for line in text.lines() {
        if line.contains("SLOW_START") {
            sequence.push("start");
        } else if line.contains("SLOW_END") {
            sequence.push("end");
        }
    }
    for pair in sequence.windows(2) {
        assert_ne!(
            pair,
            ["start", "start"],
            "a slow callback must not overlap its next round:\n{text}"
        );
    }
    running.graceful_quit(QUIET).expect("graceful quit");
}

#[test]
fn timer_counts_change_at_registration_start_and_completion() {
    let harness = NginxHarness::new("timer-nginx-counts");
    write_script(
        &harness,
        "scripts/counts.rss",
        r#"
use ngx;
ngx::timer::at(0.2, |premature| if true => {
    assert(ngx::timer::running_count() == 1);
    assert(ngx::timer::pending_count() == 0);
    ngx::log(ngx::log_level::error(), "TIMER_COUNTS_OK");
    null
} else => { null });
assert(ngx::timer::pending_count() == 1);
assert(ngx::timer::running_count() == 0);
ngx::exit(200);
"#,
    );
    harness.write_config(&timers_config(&location("/start", "scripts/counts.rss")));
    let mut running = harness.start();
    let response = response_or_panic(&harness, &mut running, &get("/start"), "register");
    assert_eq!(response.status, 200, "{}", error_log_text(&harness));
    wait_marker(&harness, &mut running, "TIMER_COUNTS_OK");
    running.graceful_quit(QUIET).expect("graceful quit");
}

#[test]
fn reload_leaves_old_workers_with_their_timers_and_new_workers_empty() {
    let harness = NginxHarness::new("timer-nginx-reload");
    write_script(
        &harness,
        "scripts/hold.rss",
        r#"
use ngx;
ngx::timer::at(30, |premature| if premature => {
    ngx::log(ngx::log_level::error(), "TIMER_RELOAD_PREMATURE_TRUE");
    null
} else => {
    ngx::log(ngx::log_level::error(), "TIMER_RELOAD_PREMATURE_FALSE");
    null
});
ngx::exit(200);
"#,
    );
    write_script(
        &harness,
        "scripts/empty.rss",
        r#"
use ngx;
assert(ngx::timer::pending_count() == 0);
assert(ngx::timer::running_count() == 0);
ngx::exit(200);
"#,
    );
    let locations = format!(
        "{}{}",
        location("/hold", "scripts/hold.rss"),
        location("/empty", "scripts/empty.rss")
    );
    let config = NginxConfig::default()
        .master_process(true)
        .workers(2)
        .error_log_level("error")
        .server_directives(&locations);
    harness.write_config(&config);
    let mut running = harness.start();
    let response = response_or_panic(&harness, &mut running, &get("/hold"), "hold request");
    assert_eq!(response.status, 200, "{}", error_log_text(&harness));

    let outcome = running
        .reload(READY)
        .unwrap_or_else(|err| panic!("reload failed: {err}\n{}", error_log_text(&harness)));
    assert!(
        !outcome.new_workers.is_empty(),
        "reload must start a new worker set: {outcome:?}"
    );
    for previous in &outcome.previous_workers {
        assert!(
            !outcome.new_workers.contains(previous),
            "every previous worker must be replaced: {outcome:?}"
        );
    }

    let empty = response_or_panic(&harness, &mut running, &get("/empty"), "empty check");
    assert_eq!(
        empty.status,
        200,
        "new workers must start with zero timers:\n{}",
        error_log_text(&harness)
    );
    running.graceful_quit(QUIET).expect("graceful quit");
}

#[test]
fn timer_callback_uses_the_worker_semaphore_family() {
    let harness = NginxHarness::new("timer-nginx-semaphore");
    write_script(
        &harness,
        "scripts/semaphore.rss",
        r#"
use ngx;
ngx::timer::at(0.05, |premature| if true => {
    let sem = ngx::semaphore::new(0);
    assert(ngx::semaphore::count(sem) == 0);
    ngx::semaphore::post(sem, 1);
    assert(ngx::semaphore::count(sem) == 1);
    let acquired = ngx::semaphore::wait(sem, 1);
    assert(acquired == true);
    assert(ngx::semaphore::count(sem) == 0);
    // A wait with a timeout parks the callback round on its own async step and
    // returns false when the timeout wins.
    let timed_out = ngx::semaphore::wait(sem, 0.05);
    assert(timed_out == false);
    let d = ngx::shared::dict("timers");
    ngx::shared::set(d, "semaphore_ok", "yes", null, null);
    ngx::log(ngx::log_level::error(), "TIMER_SEMAPHORE_OK");
    null
} else => { null });
ngx::exit(200);
"#,
    );
    harness.write_config(&timers_config(&location("/start", "scripts/semaphore.rss")));
    let mut running = harness.start();
    let response = response_or_panic(&harness, &mut running, &get("/start"), "register");
    assert_eq!(response.status, 200, "{}", error_log_text(&harness));
    wait_marker(&harness, &mut running, "TIMER_SEMAPHORE_OK");
    // The whole chain (new/count/post/wait immediate/wait parked) completed
    // without a callback failure.
    wait_absent(
        &harness,
        &running,
        "ngx.timer callback",
        Duration::from_millis(200),
    );
    running.graceful_quit(QUIET).expect("graceful quit");
}

#[test]
fn graceful_worker_exit_runs_a_pending_every_round_once_with_premature_true() {
    let harness = NginxHarness::new("timer-nginx-every-exit");
    write_script(
        &harness,
        "scripts/every_exit.rss",
        r#"
use ngx;
ngx::timer::every(30, |premature| if premature => {
    ngx::log(ngx::log_level::error(), "TIMER_EVERY_EXIT_PREMATURE_TRUE");
    null
} else => {
    ngx::log(ngx::log_level::error(), "TIMER_EVERY_EXIT_PREMATURE_FALSE");
    null
});
ngx::exit(200);
"#,
    );
    harness.write_config(&timers_config(&location(
        "/start",
        "scripts/every_exit.rss",
    )));
    let mut running = harness.start();
    let response = response_or_panic(&harness, &mut running, &get("/start"), "register");
    assert_eq!(response.status, 200, "{}", error_log_text(&harness));

    running
        .graceful_quit(QUIET)
        .unwrap_or_else(|err| panic!("graceful quit failed: {err}\n{}", error_log_text(&harness)));
    let text = error_log_text(&harness);
    assert_eq!(
        marker_count(&harness, &running, "TIMER_EVERY_EXIT_PREMATURE_TRUE"),
        1,
        "worker exit must invoke the pending `every` round exactly once with premature=true:\n{text}"
    );
    assert_eq!(
        marker_count(&harness, &running, "TIMER_EVERY_EXIT_PREMATURE_FALSE"),
        0,
        "a pending `every` round must not observe premature=false:\n{text}"
    );
    // No reschedule: the shutdown round is the only invocation, and it must not
    // re-arm the series or leave a stale wake that runs another round.
    assert_eq!(
        marker_count(&harness, &running, "ngx.timer callback"),
        0,
        "the shutdown round must not fail or re-enter:\n{text}"
    );
}

#[test]
fn timer_counts_track_registration_waiting_completion_and_every_rearm() {
    let harness = NginxHarness::new("timer-nginx-counts-boundaries");
    write_script(
        &harness,
        "scripts/counts_boundaries.rss",
        r#"
use ngx;
let d = ngx::shared::dict("timers");
ngx::timer::every(0.6, |premature| if true => {
    let d = ngx::shared::dict("timers");
    if ngx::timer::running_count() == 1 && ngx::timer::pending_count() == 0 {
        ngx::shared::set(d, "round_start_counts", "ok", null, null);
    } else {
        ngx::shared::set(d, "round_start_counts", "bad", null, null);
    }
    ngx::log(ngx::log_level::error(), "TIMER_COUNTS_ROUND_START");
    ngx::sleep_ms(300);
    if ngx::timer::running_count() == 1 && ngx::timer::pending_count() == 0 {
        ngx::shared::set(d, "waiting_counts", "ok", null, null);
    } else {
        ngx::shared::set(d, "waiting_counts", "bad", null, null);
    }
    ngx::log(ngx::log_level::error(), "TIMER_COUNTS_ROUND_DONE");
    null
} else => { null });
if ngx::timer::pending_count() == 1 && ngx::timer::running_count() == 0 {
    ngx::shared::set(d, "registered_counts", "ok", null, null);
} else {
    ngx::shared::set(d, "registered_counts", "bad", null, null);
}
ngx::exit(200);
"#,
    );
    write_script(
        &harness,
        "scripts/observe_counts.rss",
        r#"
use ngx;
let d = ngx::shared::dict("timers");
let registered: SharedGetResult<string> = ngx::shared::get(d, "registered_counts");
let round_start: SharedGetResult<string> = ngx::shared::get(d, "round_start_counts");
let waiting: SharedGetResult<string> = ngx::shared::get(d, "waiting_counts");
ngx::say(registered.value);
ngx::say(round_start.value);
ngx::say(waiting.value);
if ngx::timer::pending_count() == 1 {
    if ngx::timer::running_count() == 0 {
        ngx::say("P1R0");
    } else {
        ngx::say("P1R1");
    }
} else {
    if ngx::timer::running_count() == 1 {
        ngx::say("P0R1");
    } else {
        ngx::say("P0R0");
    }
}
"#,
    );
    let locations = format!(
        "{}{}",
        location("/start", "scripts/counts_boundaries.rss"),
        content_location("/observe", "scripts/observe_counts.rss")
    );
    harness.write_config(&timers_config(&locations));
    let mut running = harness.start();

    // Registration boundary: one pending registration, nothing running.
    let response = response_or_panic(&harness, &mut running, &get("/start"), "register");
    assert_eq!(response.status, 200, "{}", error_log_text(&harness));
    let registered = wait_for_observe_token(&harness, &mut running, "P1R0", READY);
    assert!(
        registered.lines().any(|line| line.trim() == "ok"),
        "the registering script must observe pending=1/running=0: {registered:?}"
    );

    // Waiting boundary: the callback round is parked on `ngx::sleep_ms`, so the
    // registration is running and nothing is pending.
    wait_marker(&harness, &mut running, "TIMER_COUNTS_ROUND_START");
    let waiting = observe_sample(&harness, &mut running, "P0R1");
    assert!(
        !waiting.lines().any(|line| line.trim() == "bad"),
        "in-round counts must stay 1 running / 0 pending: {waiting:?}"
    );

    // Completion and re-arm boundary: the round finished, the `every` series is
    // pending again with one live registration.
    wait_marker(&harness, &mut running, "TIMER_COUNTS_ROUND_DONE");
    let rearmed = wait_for_observe_token(&harness, &mut running, "P1R0", READY);
    let ok_lines = rearmed.lines().filter(|line| line.trim() == "ok").count();
    assert!(
        ok_lines >= 3,
        "the callback and registration must observe the documented counts: {rearmed:?}"
    );
    assert!(
        !rearmed.lines().any(|line| line.trim() == "bad"),
        "no boundary may observe a wrong count pair: {rearmed:?}"
    );

    // Counts never exceed one running round or one live registration.
    for _ in 0..4 {
        let body = observe_sample(&harness, &mut running, "P1R0");
        assert!(
            !body.contains("P1R1") && !body.contains("P0R0"),
            "counts drifted while the series is live: {body:?}"
        );
    }
    running.graceful_quit(QUIET).expect("graceful quit");
}

#[test]
fn http_standard_import_and_ngx_timer_run_together_through_the_registry() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("upstream listener");
    let port = listener.local_addr().expect("listener addr").port();
    let upstream = thread::spawn(move || {
        let mut seen = Vec::new();
        if let Ok((mut stream, _)) = listener.accept() {
            let mut request = [0u8; 4096];
            let read = stream.read(&mut request).unwrap_or(0);
            seen.extend_from_slice(&request[..read]);
            let _ = stream.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
            );
            let _ = stream.flush();
        }
        String::from_utf8_lossy(&seen).to_string()
    });

    let harness = NginxHarness::new("timer-nginx-http-import");
    write_script(
        &harness,
        "scripts/http_timer.rss",
        &format!(
            r#"
use ngx;
use http;
ngx::timer::at(0.05, |premature| if true => {{
    let d = ngx::shared::dict("timers");
    ngx::shared::set(d, "timer_fired", "yes", null, null);
    ngx::log(ngx::log_level::error(), "TIMER_HTTP_IMPORT_FIRED");
    null
}} else => {{ null }});
let response: map = http::client::request({{
    method: "GET",
    url: "http://127.0.0.1:{port}/source",
    headers: [
        {{ name: "accept", value: "text/plain" }},
        {{ name: "accept", value: "application/json" }}
    ],
    body: null
}});
let status: int = response["status"];
assert(status == 200);
ngx::exit(200);
"#
        ),
    );
    write_script(
        &harness,
        "scripts/read_http_timer.rss",
        r#"
use ngx;
let d = ngx::shared::dict("timers");
let r: SharedGetResult<string> = ngx::shared::get(d, "timer_fired");
ngx::say(r.value);
"#,
    );
    let locations = format!(
        "location /start {{ access_by_rss_file \"@@PREFIX@@/scripts/http_timer.rss\"; rss_http_scheme http; rss_http_allow 127.0.0.1 {port}; rss_http_allow_private on; }}\n{}",
        content_location("/fired", "scripts/read_http_timer.rss")
    );
    let config = NginxConfig::default()
        .master_process(true)
        .workers(1)
        .error_log_level("error")
        .http_directives("rss_shared_dict timers 1m;")
        .server_directives(&locations);
    harness.write_config(&config);
    let mut running = harness.start();

    let response = response_or_panic(&harness, &mut running, &get("/start"), "http+timer request");
    assert_eq!(response.status, 200, "{}", error_log_text(&harness));
    wait_marker(&harness, &mut running, "TIMER_HTTP_IMPORT_FIRED");
    let fired = response_or_panic(&harness, &mut running, &get("/fired"), "reader");
    assert_eq!(
        fired.body_text().trim(),
        "yes",
        "the timer callback must have run after the HTTP-calling request finished:\n{}",
        error_log_text(&harness)
    );
    let request_text = upstream.join().expect("upstream thread");
    assert_eq!(
        request_text.to_ascii_lowercase().matches("accept:").count(),
        2,
        "both duplicate request headers must reach the upstream:\n{request_text}"
    );
    running.graceful_quit(QUIET).expect("graceful quit");
}

#[test]
fn timer_callback_does_not_inherit_the_registering_location_http_policy() {
    let harness = NginxHarness::new("timer-nginx-callback-http-policy");
    write_script(
        &harness,
        "scripts/callback_http.rss",
        r#"
use ngx;
use http;
ngx::timer::at(0.05, |premature| if true => {
    let response: map = http::client::request({"method": "GET", "url": "http://127.0.0.1:9/source", "body": null});
    ngx::log(ngx::log_level::error(), "TIMER_CALLBACK_HTTP_ALLOWED");
    null
} else => { null });
ngx::exit(200);
"#,
    );
    let locations = "location /start { access_by_rss_file \"@@PREFIX@@/scripts/callback_http.rss\"; rss_http_scheme http; rss_http_allow 127.0.0.1 9; rss_http_allow_private on; }\n";
    let config = NginxConfig::default()
        .master_process(true)
        .workers(1)
        .error_log_level("error")
        .http_directives("rss_shared_dict timers 1m;")
        .server_directives(locations);
    harness.write_config(&config);
    let mut running = harness.start();
    let response = response_or_panic(&harness, &mut running, &get("/start"), "register");
    assert_eq!(response.status, 200, "{}", error_log_text(&harness));

    // The callback VM shares the host surface, not the registering VM's policy:
    // the standard HTTP host fails closed with its own not-configured error and
    // the worker keeps serving.
    wait_marker(&harness, &mut running, "ngx.timer callback");
    let text = error_log_text(&harness);
    assert!(
        text.contains("HTTP host is not configured"),
        "the callback's HTTP host must fail closed per-VM:\n{text}"
    );
    assert!(
        !text.contains("TIMER_CALLBACK_HTTP_ALLOWED"),
        "the callback must not reach the allowed-HTTP marker:\n{text}"
    );
    let health = response_or_panic(&harness, &mut running, &get("/start"), "follow-up health");
    assert_eq!(health.status, 200, "{}", error_log_text(&harness));
    running.graceful_quit(QUIET).expect("graceful quit");
}

#[test]
fn graceful_worker_exit_runs_pending_callbacks_with_premature_true() {
    let harness = NginxHarness::new("timer-nginx-exit");
    write_script(
        &harness,
        "scripts/long_at.rss",
        r#"
use ngx;
ngx::timer::at(30, |premature| if premature => {
    ngx::log(ngx::log_level::error(), "TIMER_EXIT_PREMATURE_TRUE");
    null
} else => {
    ngx::log(ngx::log_level::error(), "TIMER_EXIT_PREMATURE_FALSE");
    null
});
ngx::exit(200);
"#,
    );
    harness.write_config(&timers_config(&location("/start", "scripts/long_at.rss")));
    let mut running = harness.start();
    let response = response_or_panic(&harness, &mut running, &get("/start"), "register");
    assert_eq!(response.status, 200, "{}", error_log_text(&harness));

    running
        .graceful_quit(QUIET)
        .unwrap_or_else(|err| panic!("graceful quit failed: {err}\n{}", error_log_text(&harness)));
    let text = error_log_text(&harness);
    assert!(
        text.contains("TIMER_EXIT_PREMATURE_TRUE"),
        "worker exit must run pending callbacks with premature=true:\n{text}"
    );
    assert!(
        !text.contains("TIMER_EXIT_PREMATURE_FALSE"),
        "a pending callback must not observe premature=false:\n{text}"
    );
}
