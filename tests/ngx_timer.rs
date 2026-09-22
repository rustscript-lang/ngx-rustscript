//! `ngx::timer` family: worker-owned fake requests, owned callbacks, and
//! OpenResty-style lifecycle.
//!
//! Covers catalog/manifest/runtime registration at the exact `199` surface,
//! checked seconds conversion, owned-callback registration, the ordinary
//! `RequestIdentity` (no second identity type), deadline wake through the
//! worker eventfd, `every` round serialization, callback error isolation,
//! capacity limits, and worker shutdown with `premature=true`.
//!
//! The timer registry is worker-thread-local, so every test starts its own
//! [`WorkerRuntime`] and runs with `--test-threads=1`, as CI does.

use ngx_rustscript_nginx_test_stubs as _;

use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use ngx_rustscript::async_bridge::NgxAsyncBridge;
use ngx_rustscript::config::{
    ConfigError, compile_rss_source, compile_rss_source_for_phase, program_requires_request_state,
};
use ngx_rustscript::host::{
    NgxPhase, StandardHostPolicy, bind_registry_for_test, manifest_entry, ngx_host_catalog,
    ngx_host_manifest, ngx_host_registry, production_runtime_host_names,
};
use ngx_rustscript::notify::RequestIdentity;
use ngx_rustscript::phase::{PreparedScript, ScriptExecutionMetadata};
use ngx_rustscript::runtime::WorkerRuntime;
use ngx_rustscript::timer::{
    FAKE_REQUEST_ID_TAG, NgxTimerBackend, callback_error_count, install_prepared_timer_vm,
    live_timer_count, live_timer_identities, on_worker_event, shutdown_current,
};
use vm::{TimerBackend, TimerConfig, TimerHostExt, Vm};

const AT: &str = "ngx::timer::at";
const EVERY: &str = "ngx::timer::every";
const PENDING: &str = "ngx::timer::pending_count";
const RUNNING: &str = "ngx::timer::running_count";
const HOSTS: [&str; 4] = [AT, EVERY, PENDING, RUNNING];

fn timer_vm(source: &str, worker: &WorkerRuntime) -> Vm {
    let program =
        compile_rss_source(source).unwrap_or_else(|err| panic!("{source:?} must compile: {err}"));
    let prepared = PreparedScript::prepare(
        program,
        ScriptExecutionMetadata {
            requires_request_state: true,
            ..ScriptExecutionMetadata::default()
        },
    )
    .expect("prepare timer script");
    let mut vm = prepared.new_vm().expect("construct prepared timer vm");
    let policy = StandardHostPolicy::fail_closed();
    let http_client = worker
        .http_lease_for(&policy)
        .expect("fail-closed HTTP lease");
    install_prepared_timer_vm(&mut vm, &policy, http_client).expect("install timer vm");
    vm.set_async_bridge(Box::new(NgxAsyncBridge::new(
        worker.handle().expect("worker handle"),
    )))
    .expect("async bridge");
    vm
}

fn run_script(source: &str, worker: &WorkerRuntime) -> Result<(), String> {
    let mut vm = timer_vm(source, worker);
    vm.run().map(|_| ()).map_err(|err| err.to_string())
}

/// Waits for one worker event turn and returns the drained identities.
fn wait_for_ready_identities(
    worker: &mut WorkerRuntime,
    timeout: Duration,
) -> Vec<RequestIdentity> {
    let fd = worker.raw_fd().expect("worker eventfd");
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let ready = unsafe { libc::poll(&mut pfd, 1, 20) };
        if ready > 0 && pfd.revents & libc::POLLIN != 0 {
            return worker.drain_ready().expect("drain ready identities");
        }
    }
    panic!("no timer wake within {timeout:?}");
}

/// Runs one worker event turn for a timer registration deadline.
///
/// The delivered identity must currently belong to the worker timer registry;
/// a wake for a retired generation is not a deadline.
fn fire_deadline(worker: &mut WorkerRuntime, timeout: Duration) -> RequestIdentity {
    let ready = wait_for_ready_identities(worker, timeout);
    let live = live_timer_identities();
    let identity = ready
        .iter()
        .copied()
        .find(|identity| live.contains(identity))
        .unwrap_or_else(|| panic!("no live timer identity in {ready:?} (live {live:?})"));
    on_worker_event(&ready);
    identity
}

/// Waits until the worker delivers a wake for `identity`, dispatching every
/// other wake it observes meanwhile.
fn wait_for_identity(worker: &mut WorkerRuntime, identity: RequestIdentity, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let ready = wait_for_ready_identities(worker, timeout);
        let matched = ready.contains(&identity);
        on_worker_event(&ready);
        if matched {
            return;
        }
    }
    panic!("identity {identity:?} was not delivered within {timeout:?}");
}

#[test]
fn timer_hosts_are_catalogued_and_manifested_in_access_and_content() {
    let catalog = ngx_host_catalog();
    for name in HOSTS {
        assert!(
            catalog.function(name).is_some(),
            "{name} must be in the compiler catalog"
        );
        let entry =
            manifest_entry(name).unwrap_or_else(|| panic!("{name} must be in the manifest"));
        assert_eq!(entry.phases, [NgxPhase::Access, NgxPhase::Content]);
        assert_eq!(entry.openresty_name, name.replace("::", "."));
    }
    assert_eq!(ngx_host_manifest().len(), 199);
    assert_eq!(catalog.functions().len(), 199);
    assert_eq!(production_runtime_host_names().len(), 199);
    assert!(catalog.function("ngx::timer").is_none());
    assert!(manifest_entry("ngx::timer").is_none());
}

#[test]
fn access_and_content_accept_the_timer_family() {
    for phase in [NgxPhase::Access, NgxPhase::Content] {
        for source in [
            "use ngx;\nngx::timer::at(0.05, |premature| null);\n",
            "use ngx;\nngx::timer::every(0.05, |premature| null);\n",
            "use ngx;\nngx::timer::pending_count();\n",
            "use ngx;\nngx::timer::running_count();\n",
        ] {
            compile_rss_source_for_phase(source, phase)
                .unwrap_or_else(|err| panic!("{phase:?} must accept {source:?}: {err}"));
        }
    }
}

#[test]
fn log_phase_rejects_timer_registration_at_configuration_compile() {
    for source in [
        "use ngx;\nngx::timer::at(0.05, |premature| null);\n",
        "use ngx;\nngx::timer::pending_count();\n",
    ] {
        let err = compile_rss_source_for_phase(source, NgxPhase::Log)
            .expect_err("Log must reject the timer family");
        match err {
            ConfigError::PhaseForbidden { name, phase } => {
                assert!(name.starts_with("ngx::timer::"), "{name}");
                assert_eq!(phase, NgxPhase::Log);
            }
            other => panic!("Log must fail with PhaseForbidden, got {other:?}"),
        }
    }
}

#[test]
fn timer_imports_require_request_state() {
    let program = compile_rss_source("use ngx;\nngx::timer::at(0.05, |premature| null);\n")
        .expect("timer script must compile");
    assert!(
        program_requires_request_state(&program),
        "timer programs need the request-owned VM path (owned dispatch)"
    );
}

#[test]
fn timer_hosts_resolve_through_the_registry_not_the_name_path() {
    let compiled = vm::compile_source("let x = 1;\n").expect("import-free program");
    let mut vm = Vm::try_new(compiled.program).expect("construct vm");
    bind_registry_for_test(&mut vm).expect("bind test registry");
    for name in HOSTS {
        assert!(
            !vm.has_bound_function(name),
            "{name} must not bind through the per-name path"
        );
    }
    let registry = ngx_host_registry().expect("timer host registry");
    for name in HOSTS {
        assert!(
            registry.contains_name(name),
            "{name} must resolve through the timer host registry"
        );
    }
}

#[test]
fn at_registers_one_fake_request_with_an_ordinary_identity() {
    shutdown_current();
    let mut worker = WorkerRuntime::start().expect("worker runtime");
    assert_eq!(live_timer_count(), 0);

    // Catalog/registry install is paid during VM construction. Time only the
    // host call so a slow first bind cannot be mistaken for waiting on the
    // 10s deadline.
    let mut vm = timer_vm("use ngx;\nngx::timer::at(10, |premature| null);\n", &worker);
    let start = Instant::now();
    vm.run()
        .map(|_| ())
        .expect("registration returns immediately");
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "registration must not wait for the deadline"
    );

    assert_eq!(live_timer_count(), 1, "one fake request per registration");
    let identities = live_timer_identities();
    assert_eq!(identities.len(), 1);
    let identity: RequestIdentity = identities[0];
    assert_eq!(
        std::any::type_name_of_val(&identity),
        std::any::type_name::<RequestIdentity>(),
        "timers reuse the ordinary RequestIdentity; no second identity type"
    );
    assert!(
        identity.request_id() > 0,
        "a fake request owns a worker-unique request id, not a reserved one"
    );
    assert_ne!(
        identity.request_id() & FAKE_REQUEST_ID_TAG,
        0,
        "a fake request id is tagged out of the real request-id space: {identity:?}"
    );

    // A second registration takes the next worker-unique fake request id; two
    // live registrations never share an identity.
    run_script("use ngx;\nngx::timer::at(20, |premature| null);\n", &worker).expect("registration");
    let both = live_timer_identities();
    assert_eq!(both.len(), 2);
    assert_ne!(
        both[0], both[1],
        "live timer identities are pairwise distinct"
    );
    assert_ne!(
        both[0].request_id(),
        both[1].request_id(),
        "fake request ids are worker-unique and never reused"
    );

    // The counters observe the installed backend through the timer hosts.
    run_script(
        "use ngx;\nassert(ngx::timer::pending_count() == 2);\nassert(ngx::timer::running_count() == 0);\n",
        &worker,
    )
    .expect("count hosts read the worker registry");

    worker.shutdown();
    assert_eq!(live_timer_count(), 0, "shutdown releases the fake request");
}

#[test]
fn deadline_fires_the_callback_once_with_premature_false() {
    let mut worker = WorkerRuntime::start().expect("worker runtime");
    run_script(
        // A `premature=true` round would call the request-only host and fail;
        // a clean completion witnesses `premature=false`.
        "use ngx;\nngx::timer::at(0.05, |premature| if premature => { ngx::req::get_method(); null } else => { null });\n",
        &worker,
    )
    .expect("registration");

    fire_deadline(&mut worker, Duration::from_secs(2));
    assert_eq!(
        callback_error_count(),
        0,
        "normal expiry must run the callback with premature=false"
    );
    assert_eq!(live_timer_count(), 0, "`at` retires after its round");

    run_script(
        "use ngx;\nassert(ngx::timer::pending_count() == 0);\nassert(ngx::timer::running_count() == 0);\n",
        &worker,
    )
    .expect("counts return to zero");
    worker.shutdown();
}

#[test]
fn callback_waits_on_an_async_host_and_resumes_through_the_same_identity() {
    let mut worker = WorkerRuntime::start().expect("worker runtime");
    run_script(
        "use ngx;\nngx::timer::at(0.02, |premature| if true => { ngx::sleep_ms(50); null } else => { null });\n",
        &worker,
    )
    .expect("registration");

    fire_deadline(&mut worker, Duration::from_secs(2));
    assert_eq!(
        live_timer_count(),
        1,
        "the registration stays live while the callback waits"
    );
    assert_eq!(
        NgxTimerBackend.running_count(),
        1,
        "a waiting callback counts as running"
    );

    // The sleep completes on its own deadline and wakes the same identity.
    fire_deadline(&mut worker, Duration::from_secs(2));
    assert_eq!(callback_error_count(), 0, "the callback resumed cleanly");
    assert_eq!(live_timer_count(), 0);
    assert_eq!(NgxTimerBackend.running_count(), 0);
    worker.shutdown();
}

#[test]
fn callback_failure_is_isolated_and_reported_once() {
    let mut worker = WorkerRuntime::start().expect("worker runtime");
    run_script(
        "use ngx;\nngx::timer::at(0.02, |premature| ngx::req::get_method());\n",
        &worker,
    )
    .expect("registration");

    fire_deadline(&mut worker, Duration::from_secs(2));
    assert_eq!(
        callback_error_count(),
        1,
        "a request-only host fails closed in the timer phase and is reported once"
    );
    assert_eq!(live_timer_count(), 0, "`at` retires after the failed round");

    // The worker keeps serving: another registration still works.
    run_script("use ngx;\nngx::timer::at(10, |premature| null);\n", &worker).expect("registration");
    assert_eq!(live_timer_count(), 1);
    worker.shutdown();
    assert_eq!(live_timer_count(), 0);
}

#[test]
fn every_serializes_rounds_and_keeps_running_after_each_round() {
    let mut worker = WorkerRuntime::start().expect("worker runtime");
    // Each round calls a request-only host, so a completed round is visible as
    // exactly one reported error.
    run_script(
        "use ngx;\nngx::timer::every(0.03, |premature| ngx::req::get_method());\n",
        &worker,
    )
    .expect("registration");

    for round in 1..=3 {
        fire_deadline(&mut worker, Duration::from_secs(2));
        assert_eq!(
            callback_error_count(),
            round,
            "round {round} ran to a terminal state"
        );
        assert_eq!(
            live_timer_count(),
            1,
            "the `every` registration stays live between rounds"
        );
        assert_eq!(
            NgxTimerBackend.running_count(),
            0,
            "rounds never overlap: the next deadline waits for terminal completion"
        );
    }

    shutdown_current();
    assert_eq!(live_timer_count(), 0);
    let errors_after_shutdown = callback_error_count();
    std::thread::sleep(Duration::from_millis(120));
    assert_eq!(
        callback_error_count(),
        errors_after_shutdown,
        "no trailing `every` round after shutdown"
    );
    worker.shutdown();
}

#[test]
fn every_rounds_rotate_identity_and_retire_the_previous_generation() {
    shutdown_current();
    let mut worker = WorkerRuntime::start().expect("worker runtime");
    // A completed round is visible as exactly one reported error, because the
    // callback calls a request-only host.
    run_script(
        "use ngx;\nngx::timer::every(0.05, |premature| ngx::req::get_method());\n",
        &worker,
    )
    .expect("registration");
    let first = live_timer_identities();
    assert_eq!(first.len(), 1);
    let first = first[0];

    fire_deadline(&mut worker, Duration::from_secs(2));
    assert_eq!(callback_error_count(), 1, "round 1 ran to a terminal state");

    let second = live_timer_identities();
    assert_eq!(second.len(), 1, "the `every` registration stays live");
    let second = second[0];
    assert_eq!(
        second.request_id(),
        first.request_id(),
        "the worker-unique fake request id identifies the registration"
    );
    assert!(
        second.generation() > first.generation(),
        "a re-armed round registers a fresh, monotonically advancing generation: {first:?} -> {second:?}"
    );

    // The retired generation is no longer a deadline: delivering its wake must
    // not start the next round early.
    on_worker_event(&[first]);
    assert_eq!(
        callback_error_count(),
        1,
        "a stale previous-round wake must be ignored"
    );
    assert_eq!(live_timer_identities(), vec![second]);
    assert_eq!(NgxTimerBackend.running_count(), 0);

    // The current generation still delivers exactly one more round.
    fire_deadline(&mut worker, Duration::from_secs(2));
    assert_eq!(callback_error_count(), 2);
    let third = live_timer_identities();
    assert_eq!(third.len(), 1);
    assert_ne!(third[0], second, "each round rotates the identity again");
    shutdown_current();
    assert_eq!(live_timer_count(), 0);
}

#[test]
fn duplicate_stale_wakes_cannot_start_the_next_every_round_early() {
    shutdown_current();
    let mut worker = WorkerRuntime::start().expect("worker runtime");
    let interval = Duration::from_millis(500);
    run_script(
        &format!(
            "use ngx;\nngx::timer::every({}, |premature| ngx::req::get_method());\n",
            interval.as_secs_f64()
        ),
        &worker,
    )
    .expect("registration");

    let first = live_timer_identities()[0];
    let started = Instant::now();
    // Simulate the first deadline immediately so the duplicate-wake window is
    // deterministic.
    on_worker_event(&[first]);
    assert_eq!(callback_error_count(), 1, "round 1 ran");
    let second = live_timer_identities()[0];
    assert_ne!(second, first);

    // Duplicate wakes of the completed round must not start round 2 before the
    // interval elapses, and rounds must never overlap.
    let quiet_until = started + Duration::from_millis(300);
    while Instant::now() < quiet_until {
        on_worker_event(&[first]);
        assert_eq!(
            callback_error_count(),
            1,
            "a duplicate previous-round wake must not start a round early"
        );
        assert_eq!(NgxTimerBackend.running_count(), 0, "rounds never overlap");
        thread::sleep(Duration::from_millis(10));
    }

    // The fresh generation's own deadline arrives at the interval boundary and
    // runs exactly one round.
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut saw_fresh = false;
    while Instant::now() < deadline && !saw_fresh {
        let ready = wait_for_ready_identities(&mut worker, Duration::from_secs(2));
        let spacing = started.elapsed();
        let carries_fresh = ready.contains(&second);
        on_worker_event(&ready);
        if carries_fresh {
            assert!(
                spacing >= interval,
                "the next round must respect the `every` interval: {spacing:?}"
            );
            saw_fresh = true;
        } else {
            assert_eq!(
                callback_error_count(),
                1,
                "a stale deadline wake must be ignored"
            );
        }
    }
    assert!(saw_fresh, "the fresh generation must deliver its deadline");
    assert_eq!(callback_error_count(), 2, "round 2 ran after the interval");
    assert_eq!(NgxTimerBackend.running_count(), 0);
    shutdown_current();
}

#[test]
fn stale_async_wake_from_a_previous_round_is_ignored() {
    shutdown_current();
    let mut worker = WorkerRuntime::start().expect("worker runtime");
    run_script(
        // The round parks on `ngx::sleep_ms`; the sleep completion wakes the
        // round's own identity through the worker eventfd.
        "use ngx;\nngx::timer::every(10, |premature| if true => { ngx::sleep_ms(30); ngx::req::get_method(); null } else => { null });\n",
        &worker,
    )
    .expect("registration");

    let first = live_timer_identities()[0];
    on_worker_event(&[first]);
    assert_eq!(
        NgxTimerBackend.running_count(),
        1,
        "the callback is parked on its sleep"
    );
    wait_for_identity(&mut worker, first, Duration::from_secs(2));
    assert_eq!(
        callback_error_count(),
        1,
        "round 1 completed after the sleep"
    );

    let second = live_timer_identities()[0];
    assert!(
        second.generation() > first.generation(),
        "the re-armed round uses a fresh generation"
    );

    // A duplicate wake of the completed round's async step is stale now.
    on_worker_event(&[first]);
    assert_eq!(callback_error_count(), 1, "the stale async wake is ignored");
    assert_eq!(NgxTimerBackend.running_count(), 0);
    assert_eq!(live_timer_identities(), vec![second]);
    shutdown_current();
}

#[test]
fn http_standard_import_and_ngx_timer_bind_through_the_worker_registry() {
    shutdown_current();
    let worker = WorkerRuntime::start().expect("worker runtime");
    // A program that imports a pd-vm standard HTTP host *and* the timer family
    // must resolve both: the timer entries from `ngx_host_registry` (owned
    // dispatch) and the standard import from the same registry's standard
    // composition. The HTTP call is never executed here; binding is the
    // contract.
    let program = compile_rss_source(
        r#"use http;
use ngx;
let response: map = http::client::request({"method": "GET", "url": "https://example.test/", "headers": {"accept": "application/json"}, "body": null});
let status: int = response["status"];
ngx::timer::at(0.05, |premature| null);
"#,
    )
    .expect("merged catalog compiles HTTP + timer");
    assert!(program_requires_request_state(&program));

    let prepared = PreparedScript::prepare(
        program,
        ScriptExecutionMetadata {
            requires_request_state: true,
            ..ScriptExecutionMetadata::default()
        },
    )
    .expect("prepare HTTP + timer script");
    let mut vm = prepared.new_vm().expect("construct prepared timer vm");
    let policy = StandardHostPolicy::fail_closed();
    let http_client = worker
        .http_lease_for(&policy)
        .expect("fail-closed HTTP lease");
    install_prepared_timer_vm(&mut vm, &policy, http_client)
        .expect("install HTTP standard import + ngx::timer state");
    vm.set_async_bridge(Box::new(NgxAsyncBridge::new(
        worker.handle().expect("worker handle"),
    )))
    .expect("async bridge");
    drop(worker);
}

#[test]
fn pending_limit_rejects_the_next_registration() {
    let worker = WorkerRuntime::start().expect("worker runtime");
    let program = compile_rss_source(
        "use ngx;\nngx::timer::at(30, |premature| null);\nngx::timer::at(30, |premature| null);\n",
    )
    .expect("compile");
    let mut vm = Vm::new_shared(program);
    let registry = ngx_host_registry().expect("registry");
    registry.bind_vm_cached(&mut vm).expect("registry bind");
    vm.install_timer_runtime(
        Arc::new(NgxTimerBackend),
        TimerConfig {
            max_pending: 1,
            max_running: 1,
        },
    );
    vm.set_async_bridge(Box::new(NgxAsyncBridge::new(
        worker.handle().expect("handle"),
    )))
    .expect("bridge");

    let err = vm
        .run()
        .expect_err("the second registration exceeds max_pending");
    assert!(
        err.to_string().contains("pending timer limit"),
        "the backend limit must be reported, got {err}"
    );
    assert_eq!(
        live_timer_count(),
        1,
        "the accepted registration is untouched by the rejected one"
    );
    shutdown_current();
    assert_eq!(live_timer_count(), 0);
}

#[test]
fn invalid_seconds_are_rejected_by_the_registration_host() {
    shutdown_current();
    let mut worker = WorkerRuntime::start().expect("worker runtime");
    for (source, needle) in [
        (
            "use ngx;\nngx::timer::at(-1, |premature| null);\n",
            "non-negative",
        ),
        (
            "use ngx;\nngx::timer::every(0, |premature| null);\n",
            "positive",
        ),
        (
            "use ngx;\nlet zero = 0.0;\nngx::timer::at(1.0 / zero, |premature| null);\n",
            "infinite",
        ),
        (
            "use ngx;\nlet zero = 0.0;\nngx::timer::at(zero / zero, |premature| null);\n",
            "NaN",
        ),
    ] {
        let err = run_script(source, &worker).expect_err("invalid seconds must fail");
        assert!(
            err.contains(needle),
            "{source:?} must report {needle:?}, got {err}"
        );
    }
    assert_eq!(
        live_timer_count(),
        0,
        "rejected registrations retain nothing"
    );
    worker.shutdown();
}

#[test]
fn worker_shutdown_runs_pending_callbacks_with_premature_true_once() {
    shutdown_current();
    let mut worker = WorkerRuntime::start().expect("worker runtime");
    run_script(
        "use ngx;\nngx::timer::at(30, |premature| if premature => { ngx::req::get_method(); null } else => { null });\n",
        &worker,
    )
    .expect("registration");
    assert_eq!(live_timer_count(), 1);

    worker.shutdown();
    assert_eq!(
        callback_error_count(),
        1,
        "the pending callback ran exactly once with premature=true"
    );
    assert_eq!(
        live_timer_count(),
        0,
        "shutdown releases every fake request"
    );
    assert_eq!(NgxTimerBackend.pending_count(), 0);
    assert_eq!(NgxTimerBackend.running_count(), 0);
}

#[test]
fn cancelled_waiting_callback_releases_its_async_step() {
    shutdown_current();
    let mut worker = WorkerRuntime::start().expect("worker runtime");
    run_script(
        "use ngx;\nngx::timer::at(0.02, |premature| if true => { ngx::sleep_ms(30000); null } else => { null });\n",
        &worker,
    )
    .expect("registration");
    fire_deadline(&mut worker, Duration::from_secs(2));
    assert_eq!(
        NgxTimerBackend.running_count(),
        1,
        "the callback is parked on its sleep"
    );

    let start = Instant::now();
    worker.shutdown();
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "shutdown must not wait out the callback's sleep"
    );
    assert_eq!(live_timer_count(), 0);
    assert_eq!(NgxTimerBackend.running_count(), 0);
    assert_eq!(NgxTimerBackend.pending_count(), 0);
}
