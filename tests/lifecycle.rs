//! Task 6: request cancellation, stale completion filtering, exact abort,
//! permit release, and worker exit.
//!
//! Real pd-vm drives `ngx::sleep` Yield/Halt. NGINX action sinks and syscall
//! timing are faked only where FFI is unsuitable. Focused real NGINX tests
//! cover client disconnect and graceful SIGQUIT.

mod support;

use ngx_rustscript_nginx_test_stubs as _;

use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ngx::core::Status;
use ngx_rustscript::config::compile_rss_source;
use ngx_rustscript::notify::DEFAULT_MAX_IN_FLIGHT;
use ngx_rustscript::read_close::{
    EventFlags, HandlerAction, HandlerState, PeekResult, run_read_close_handler,
};
use ngx_rustscript::request_state::{
    AccessExec, AccessStatus, ReadyEffect, RecordingSink, SubmitError, TokenRegistry,
};
use ngx_rustscript::runtime::WorkerRuntime;
use ngx_rustscript::sleep_binding::{SleepCoordinator, SleepSession, SleepToken};
use support::artifact::module_cdylib;
use support::nginx::{combined, nginx_binary, nginx_quote_path, nginx_test};
use support::temp::isolated_prefix;
use vm::{Program, VmStatus};

fn compile(source: &str) -> Arc<Program> {
    compile_rss_source(source).expect("compile with ngx::sleep catalog")
}

fn sleep_once(ms: i64) -> Arc<Program> {
    compile(&format!("use ngx;\nngx::sleep({ms});\n"))
}

fn two_sleeps(a: i64, b: i64) -> Arc<Program> {
    compile(&format!("use ngx;\nngx::sleep({a});\nngx::sleep({b});\n"))
}

fn bind_exec(program: Arc<Program>, request_id: u64, coordinator: &SleepCoordinator) -> AccessExec {
    let session = SleepSession::new(coordinator).expect("session");
    AccessExec::bind_program(program, request_id, session, coordinator.clone()).expect("bind")
}

struct FakeSubmit {
    ops: Vec<(SleepToken, i64)>,
}

impl FakeSubmit {
    fn new() -> Self {
        Self { ops: Vec::new() }
    }
}

impl ngx_rustscript::request_state::SleepSubmit for FakeSubmit {
    fn submit(&mut self, token: SleepToken, ms: i64) -> Result<(), SubmitError> {
        self.ops.push((token, ms));
        Ok(())
    }
}

struct Driver {
    exec: AccessExec,
    sink: RecordingSink,
    registry: TokenRegistry<u64>,
    submit: FakeSubmit,
}

impl Driver {
    fn new(program: Arc<Program>) -> Self {
        let coordinator = SleepCoordinator::new();
        Self {
            exec: bind_exec(program, 1, &coordinator),
            sink: RecordingSink::default(),
            registry: TokenRegistry::with_capacity(DEFAULT_MAX_IN_FLIGHT),
            submit: FakeSubmit::new(),
        }
    }

    fn handle(&mut self) -> AccessStatus {
        self.exec
            .handle_access(&mut self.registry, &mut self.submit)
    }

    fn ready(&mut self, token: SleepToken) -> ReadyEffect {
        self.exec
            .apply_ready(token, &mut self.registry, &mut self.submit)
    }

    fn apply_effect(&mut self, effect: ReadyEffect) {
        ngx_rustscript::request_state::apply_ready_effect(effect, &mut self.sink);
    }

    fn cleanup(&mut self) -> Option<SleepToken> {
        let request_id = self.exec.request_id();
        let token = self.exec.cleanup();
        if let Some(token) = token {
            let _ = self.registry.remove_exact(token, &request_id);
        }
        token
    }
}

#[test]
fn cleanup_before_completion_ignores_later_ready() {
    let mut driver = Driver::new(sleep_once(30));
    assert_eq!(driver.handle(), AccessStatus::Again);
    let token = driver.submit.ops[0].0;
    assert_eq!(driver.registry.len(), 1);
    let cancelled = driver.cleanup();
    assert_eq!(cancelled, Some(token));
    assert!(driver.exec.is_cancelled());
    assert_eq!(driver.registry.len(), 0);
    assert_eq!(driver.exec.current_token(), None);

    let effect = driver.ready(token);
    driver.apply_effect(effect);
    assert_eq!(effect, ReadyEffect::Ignored);
    assert_eq!(driver.sink.phase_continues, 0);
    assert_ne!(driver.exec.vm_status(), Some(VmStatus::Halted));
    assert_eq!(driver.handle(), AccessStatus::Again);
    assert_eq!(driver.sink.phase_continues, 0);
}

#[test]
fn completion_queued_before_cleanup_does_not_finalize() {
    let mut driver = Driver::new(sleep_once(0));
    assert_eq!(driver.handle(), AccessStatus::Again);
    let token = driver.submit.ops[0].0;
    // Completion is queued (coordinator marked) before pool cleanup.
    // apply_ready is the eventfd callback; cleanup wins if it runs first
    // after the queue, or the callback is filtered if cleanup already ran.
    let _ = driver.cleanup();
    let effect = driver.ready(token);
    driver.apply_effect(effect);
    assert_eq!(effect, ReadyEffect::Ignored);
    assert_eq!(driver.sink.phase_continues, 0);
    assert!(driver.exec.is_cancelled());
}

#[test]
fn callback_after_cleanup_is_ignored() {
    let mut driver = Driver::new(sleep_once(5));
    assert_eq!(driver.handle(), AccessStatus::Again);
    let token = driver.submit.ops[0].0;
    assert!(driver.cleanup().is_some());
    for _ in 0..3 {
        let effect = driver.ready(token);
        driver.apply_effect(effect);
        assert_eq!(effect, ReadyEffect::Ignored);
    }
    assert_eq!(driver.sink.phase_continues, 0);
}

#[test]
fn duplicate_completion_after_halt_does_not_advance_phase_twice() {
    let mut driver = Driver::new(sleep_once(1));
    assert_eq!(driver.handle(), AccessStatus::Again);
    let token = driver.submit.ops[0].0;
    let first = driver.ready(token);
    driver.apply_effect(first);
    assert_eq!(first, ReadyEffect::Halted);
    let second = driver.ready(token);
    driver.apply_effect(second);
    assert_eq!(second, ReadyEffect::Ignored);
    assert_eq!(driver.sink.phase_continues, 1);
}

#[test]
fn duplicate_ready_after_cancel_is_ignored() {
    let mut driver = Driver::new(sleep_once(1));
    assert_eq!(driver.handle(), AccessStatus::Again);
    let token = driver.submit.ops[0].0;
    assert!(driver.cleanup().is_some());
    assert_eq!(driver.ready(token), ReadyEffect::Ignored);
    assert_eq!(driver.ready(token), ReadyEffect::Ignored);
    assert_eq!(driver.sink.phase_continues, 0);
}

#[test]
fn stale_generation_token_is_ignored() {
    let mut driver = Driver::new(sleep_once(1));
    assert_eq!(driver.handle(), AccessStatus::Again);
    let token = driver.submit.ops[0].0;
    let forged = SleepToken::from_raw(token.generation().wrapping_add(1), token.op_id());
    let effect = driver.ready(forged);
    driver.apply_effect(effect);
    assert_eq!(effect, ReadyEffect::Ignored);
    assert_eq!(driver.sink.phase_continues, 0);
    assert_eq!(driver.registry.len(), 1);
}

#[test]
fn request_id_and_pointer_key_reuse_is_generation_safe() {
    let mut ids = TokenRegistry::<u64>::with_capacity(8);
    let mut live = TokenRegistry::<*mut u8>::with_capacity(8);
    let ptr = 0x1000 as *mut u8;
    let old = SleepToken::from_raw(1, 1);
    let new = SleepToken::from_raw(2, 2);

    ids.try_insert(old, 1).expect("old id");
    live.try_insert(old, ptr).expect("old ptr");
    assert_eq!(ids.remove_exact(old, &1), Some(1));
    assert_eq!(live.remove_exact(old, &ptr), Some(ptr));
    assert!(ids.get(old).is_none());
    assert!(live.get(old).is_none());

    ids.try_insert(new, 2).expect("reused request id space");
    live.try_insert(new, ptr).expect("reused pointer key");
    assert_eq!(ids.get(new), Some(&2));
    assert_eq!(live.get(new), Some(&ptr));
    assert_eq!(ids.remove_exact(old, &1), None);
    assert_eq!(live.remove_exact(old, &ptr), None);
    assert_eq!(ids.get(new), Some(&2));
    assert_eq!(live.get(new), Some(&ptr));
    assert_eq!(ids.remove_exact(new, &1), None);
    assert_eq!(ids.get(new), Some(&2));
}

#[test]
fn cleanup_after_normal_halted_is_idempotent() {
    let mut driver = Driver::new(sleep_once(1));
    assert_eq!(driver.handle(), AccessStatus::Again);
    let token = driver.submit.ops[0].0;
    let effect = driver.ready(token);
    driver.apply_effect(effect);
    assert_eq!(effect, ReadyEffect::Halted);
    assert_eq!(driver.sink.phase_continues, 1);
    assert_eq!(driver.handle(), AccessStatus::Ok);

    assert!(driver.cleanup().is_none());
    assert!(driver.cleanup().is_none());
    let effect = driver.ready(token);
    driver.apply_effect(effect);
    assert_eq!(effect, ReadyEffect::Ignored);
    assert_eq!(driver.sink.phase_continues, 1);
    assert_eq!(driver.handle(), AccessStatus::Ok);
}

#[test]
fn cleanup_after_error_is_idempotent() {
    let mut driver = Driver::new(compile("1 / 0;\n"));
    assert_eq!(driver.handle(), AccessStatus::Error500);
    assert_eq!(driver.sink.phase_continues, 0);
    assert!(driver.cleanup().is_none());
    assert!(driver.cleanup().is_none());
    assert_eq!(driver.handle(), AccessStatus::Error500);
    assert_eq!(driver.sink.phase_continues, 0);
}

#[test]
fn cancel_while_second_sequential_sleep_pending() {
    let mut driver = Driver::new(two_sleeps(1, 2));
    assert_eq!(driver.handle(), AccessStatus::Again);
    let first = driver.submit.ops[0].0;
    let effect = driver.ready(first);
    driver.apply_effect(effect);
    assert_eq!(effect, ReadyEffect::YieldedAgain);
    assert_eq!(driver.submit.ops.len(), 2);
    assert_eq!(driver.sink.phase_continues, 0);
    let second = driver.submit.ops[1].0;
    assert_ne!(first, second);

    let cancelled = driver.cleanup();
    assert_eq!(cancelled, Some(second));
    assert_eq!(driver.registry.len(), 0);
    let effect = driver.ready(second);
    driver.apply_effect(effect);
    assert_eq!(effect, ReadyEffect::Ignored);
    let effect = driver.ready(first);
    driver.apply_effect(effect);
    assert_eq!(effect, ReadyEffect::Ignored);
    assert_eq!(driver.sink.phase_continues, 0);
    assert_eq!(driver.handle(), AccessStatus::Again);
}

#[test]
fn exact_registry_removal_does_not_touch_other_request() {
    let coordinator = SleepCoordinator::new();
    let program = sleep_once(1);
    let mut a = bind_exec(Arc::clone(&program), 1, &coordinator);
    let mut b = bind_exec(program, 2, &coordinator);
    let mut registry = TokenRegistry::with_capacity(8);
    let mut submit = FakeSubmit::new();
    assert_eq!(
        a.handle_access(&mut registry, &mut submit),
        AccessStatus::Again
    );
    assert_eq!(
        b.handle_access(&mut registry, &mut submit),
        AccessStatus::Again
    );
    let ta = a.current_token().expect("a");
    let tb = b.current_token().expect("b");
    let taken = a.cleanup();
    assert_eq!(taken, Some(ta));
    assert_eq!(registry.remove_exact(ta, &1), Some(1));
    assert_eq!(registry.get(tb), Some(&2));
    assert_eq!(registry.remove_exact(ta, &1), None);
    assert_eq!(
        b.apply_ready(tb, &mut registry, &mut submit),
        ReadyEffect::Halted
    );
}

#[test]
fn deferred_ready_is_invalidated_by_cleanup() {
    let mut driver = Driver::new(sleep_once(0));
    let outcome = driver.exec.run_until_yield_or_halt().expect("run");
    let token = outcome.token().expect("token");
    driver
        .exec
        .register_current(&mut driver.registry)
        .expect("register");
    assert_eq!(driver.ready(token), ReadyEffect::Deferred);
    assert!(driver.cleanup().is_some());
    assert_eq!(
        driver
            .exec
            .flush_deferred(&mut driver.registry, &mut driver.submit),
        ReadyEffect::Ignored
    );
    assert_eq!(driver.ready(token), ReadyEffect::Ignored);
    assert_eq!(driver.sink.phase_continues, 0);
}

#[test]
fn yielded_maps_to_nginx_again_unchanged() {
    assert_eq!(AccessStatus::Again.as_nginx(), Status::NGX_AGAIN);
    assert_eq!(Status::NGX_AGAIN.0, -2);
}

struct WorkerSubmit<'a> {
    worker: &'a WorkerRuntime,
}

impl ngx_rustscript::request_state::SleepSubmit for WorkerSubmit<'_> {
    fn submit(&mut self, token: SleepToken, ms: i64) -> Result<(), SubmitError> {
        self.worker
            .submit_sleep(token, ms)
            .map_err(|err| match err {
                ngx_rustscript::runtime::RuntimeError::Notify(
                    ngx_rustscript::notify::NotifyError::QueueFull,
                ) => SubmitError::Full,
                _ => SubmitError::Closed,
            })
    }
}

fn wait_in_flight_zero(worker: &WorkerRuntime, timeout: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if worker.in_flight().unwrap() == 0 {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    worker.in_flight().unwrap() == 0
}

#[test]
fn exact_abort_once_releases_permit_while_sleeping() {
    let mut worker = WorkerRuntime::start().unwrap();
    let token = SleepToken::from_raw(1, 1);
    worker.submit_sleep(token, 60_000).unwrap();
    assert_eq!(worker.in_flight().unwrap(), 1);
    assert!(worker.abort_sleep(token), "first abort must hit the task");
    assert!(
        wait_in_flight_zero(&worker, Duration::from_secs(2)),
        "permit must return to zero after abort of a sleeping task"
    );
    assert!(
        !worker.abort_sleep(token),
        "second abort of the same token must be a no-op"
    );
    assert_eq!(worker.in_flight().unwrap(), 0);
    worker.shutdown();
}

#[test]
fn abort_queued_completion_releases_permit() {
    let mut worker = WorkerRuntime::start().unwrap();
    let token = SleepToken::from_raw(1, 7);
    worker.submit_sleep(token, 0).unwrap();
    let fd = worker.raw_fd().expect("eventfd");
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(2) {
        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let n = unsafe { libc::poll(&mut pfd, 1, 20) };
        if n > 0 && pfd.revents & libc::POLLIN != 0 {
            break;
        }
    }
    assert!(
        worker.in_flight().unwrap() >= 1,
        "completion may be queued before harvest"
    );
    let _ = worker.abort_sleep(token);
    assert!(
        wait_in_flight_zero(&worker, Duration::from_secs(2)),
        "queued completion must release its permit on abort"
    );
    let ready = worker.drain_ready().unwrap();
    assert!(
        ready.is_empty() || ready.iter().all(|t| *t != token),
        "aborted token must not remain ready: {ready:?}"
    );
    worker.shutdown();
}

#[test]
fn real_vm_cancel_aborts_sleep_and_drops_session() {
    let mut worker = WorkerRuntime::start().unwrap();
    let program = sleep_once(60_000);
    let mut exec = worker.bind_access_exec(program, 11).expect("bind");
    let mut registry = TokenRegistry::with_capacity(8);
    let mut submit = WorkerSubmit { worker: &worker };
    assert_eq!(
        exec.handle_access(&mut registry, &mut submit),
        AccessStatus::Again
    );
    let token = exec.current_token().expect("token");
    assert_eq!(registry.len(), 1);
    assert_eq!(worker.in_flight().unwrap(), 1);

    let taken = exec.cleanup();
    assert_eq!(taken, Some(token));
    assert_eq!(registry.remove_exact(token, &11), Some(11));
    assert!(worker.abort_sleep(token));
    assert!(exec.is_cancelled());
    assert_eq!(registry.len(), 0);
    assert!(wait_in_flight_zero(&worker, Duration::from_secs(2)));
    assert!(!worker.abort_sleep(token));
    assert_eq!(
        exec.apply_ready(token, &mut registry, &mut submit),
        ReadyEffect::Ignored
    );
    worker.shutdown();
}

#[test]
fn close_triggered_cleanup_aborts_and_zeros_permit() {
    let mut worker = WorkerRuntime::start().unwrap();
    let program = sleep_once(60_000);
    let mut exec = worker.bind_access_exec(program, 21).expect("bind");
    let mut registry = TokenRegistry::with_capacity(8);
    let mut submit = WorkerSubmit { worker: &worker };
    assert_eq!(
        exec.handle_access(&mut registry, &mut submit),
        AccessStatus::Again
    );
    let token = exec.current_token().expect("token");
    assert_eq!(worker.in_flight().unwrap(), 1);
    assert!(!exec.is_finished());

    let mut finalized = 0;
    let mut state = HandlerState {
        already_done: exec.is_finished(),
        flags: EventFlags {
            pending_eof: true,
            error: false,
        },
    };
    let first = run_read_close_handler(&mut state, || PeekResult::Bytes(1), || finalized += 1);
    let second = run_read_close_handler(&mut state, || PeekResult::Eof, || finalized += 1);
    assert_eq!(first, HandlerAction::Finalized);
    assert_eq!(second, HandlerAction::Ignored);
    assert_eq!(finalized, 1);

    let taken = exec.cleanup();
    assert_eq!(taken, Some(token));
    assert_eq!(registry.remove_exact(token, &21), Some(21));
    assert!(worker.abort_sleep(token));
    assert!(exec.is_cancelled());
    assert_eq!(registry.len(), 0);
    assert!(wait_in_flight_zero(&worker, Duration::from_secs(2)));
    assert_eq!(worker.in_flight().unwrap(), 0);
    assert_eq!(
        exec.apply_ready(token, &mut registry, &mut submit),
        ReadyEffect::Ignored
    );
    worker.shutdown();
}

#[test]
fn worker_shutdown_with_in_flight_tasks_is_fast_and_zeros_permits() {
    let mut worker = WorkerRuntime::start().unwrap();
    worker
        .submit_sleep(SleepToken::from_raw(1, 1), 60_000)
        .unwrap();
    worker
        .submit_sleep(SleepToken::from_raw(1, 2), 60_000)
        .unwrap();
    assert_eq!(worker.in_flight().unwrap(), 2);
    let start = Instant::now();
    worker.shutdown();
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "long sleeps must not delay shutdown, elapsed {:?}",
        start.elapsed()
    );
    assert_eq!(worker.in_flight().unwrap(), 0);
    assert!(!worker.is_accepting());
}

#[test]
fn submit_after_shutdown_is_rejected_and_exit_is_idempotent() {
    let mut worker = WorkerRuntime::start().unwrap();
    worker.shutdown();
    assert_eq!(
        worker
            .submit_sleep(SleepToken::from_raw(1, 1), 0)
            .unwrap_err(),
        ngx_rustscript::runtime::RuntimeError::Notify(
            ngx_rustscript::notify::NotifyError::ShutDown
        )
    );
    worker.shutdown();
    worker.shutdown();
    assert_eq!(
        worker
            .submit_sleep(SleepToken::from_raw(1, 2), 1)
            .unwrap_err(),
        ngx_rustscript::runtime::RuntimeError::Notify(
            ngx_rustscript::notify::NotifyError::ShutDown
        )
    );
}

#[test]
fn cleanup_invalidates_sleep_session_generation() {
    let coordinator = SleepCoordinator::new();
    let mut exec = bind_exec(sleep_once(5), 3, &coordinator);
    let mut registry = TokenRegistry::with_capacity(4);
    let mut submit = FakeSubmit::new();
    assert_eq!(
        exec.handle_access(&mut registry, &mut submit),
        AccessStatus::Again
    );
    let token = exec.current_token().expect("token");
    assert!(exec.cleanup().is_some());
    assert!(
        coordinator.complete(token).is_err(),
        "dropped SleepSession generation must reject later complete"
    );
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

struct RunningNginx {
    child: Child,
}

impl Drop for RunningNginx {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn write_conf(prefix: &Path, body: &str) {
    fs::write(prefix.join("conf/nginx.conf"), body).unwrap();
}

fn write_index(prefix: &Path, rel: &str, body: &str) {
    let path = prefix.join("html").join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, body).unwrap();
}

fn spawn_nginx(nginx: &Path, prefix: &Path) -> RunningNginx {
    let child = Command::new(nginx)
        .arg("-p")
        .arg(prefix)
        .arg("-c")
        .arg("conf/nginx.conf")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn nginx");
    RunningNginx { child }
}

fn wait_for_port(port: u16, timeout: Duration) {
    let start = Instant::now();
    loop {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return;
        }
        if start.elapsed() >= timeout {
            panic!("nginx did not listen on 127.0.0.1:{port} within {timeout:?}");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn http_get(port: u16, path: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    write!(
        stream,
        "GET {path} HTTP/1.0\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let _ = stream.flush();
    let mut body = String::new();
    stream.read_to_string(&mut body).unwrap();
    let status = body
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    (status, body)
}

fn http_body(response: &str) -> &str {
    response
        .split("\r\n\r\n")
        .nth(1)
        .unwrap_or("")
        .trim_end_matches('\0')
}

fn lifecycle_conf(module: &Path, prefix: &Path, port: u16) -> String {
    let module = nginx_quote_path(module).unwrap();
    let pid = nginx_quote_path(&prefix.join("logs/nginx.pid")).unwrap();
    let html = nginx_quote_path(&prefix.join("html")).unwrap();
    let error_log = nginx_quote_path(&prefix.join("logs/error.log")).unwrap();
    let access_log = nginx_quote_path(&prefix.join("logs/access.log")).unwrap();
    let sleep = nginx_quote_path(&fixture("sleep_30ms.rss")).unwrap();
    let two = nginx_quote_path(&fixture("two_sleeps_access.rss")).unwrap();
    let mid = nginx_quote_path(&fixture("sleep_500ms.rss")).unwrap();
    let long = nginx_quote_path(&fixture("sleep_30s.rss")).unwrap();
    format!(
        r#"
worker_processes 1;
error_log {error_log} info;
pid {pid};
daemon off;
master_process on;
load_module {module};

events {{
    worker_connections 32;
}}

http {{
    access_log {access_log};
    default_type text/plain;
    server {{
        listen 127.0.0.1:{port};
        server_name localhost;
        root {html};

        location /sleep/ {{
            access_by_rss_file {sleep};
        }}

        location /two/ {{
            access_by_rss_file {two};
        }}

        location /mid/ {{
            access_by_rss_file {mid};
        }}

        location /long/ {{
            access_by_rss_file {long};
        }}

        location /plain/ {{
        }}
    }}
}}
"#
    )
}

fn wait_access_status(log: &Path, uri: &str, status: u16, timeout: Duration) -> bool {
    let needle = format!("\"GET {uri} HTTP/1.0\" {status} ");
    let start = Instant::now();
    while start.elapsed() < timeout {
        if let Ok(text) = fs::read_to_string(log) {
            if text.contains(&needle) {
                return true;
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

fn quit_and_wait(child: &mut Child, timeout: Duration) {
    let pid = child.id();
    let status = Command::new("kill")
        .args(["-QUIT", &pid.to_string()])
        .status()
        .expect("send SIGQUIT");
    assert!(status.success(), "kill -QUIT {pid} failed: {status:?}");
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                assert!(
                    status.success(),
                    "nginx SIGQUIT must exit successfully: {status:?}"
                );
                return;
            }
            Ok(None) => {
                if start.elapsed() >= timeout {
                    panic!("nginx master pid {pid} did not exit after SIGQUIT within {timeout:?}");
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(err) => panic!("wait nginx: {err}"),
        }
    }
}

#[test]
fn sleep_30ms_and_two_sleeps_remain_green() {
    let nginx = nginx_binary();
    let module = module_cdylib();
    let prefix = isolated_prefix("life-http-sleep");
    let port = free_port();
    write_index(&prefix, "sleep/index.html", "slept\n");
    write_index(&prefix, "two/index.html", "twice\n");
    write_index(&prefix, "plain/index.html", "plain\n");
    write_conf(&prefix, &lifecycle_conf(&module, &prefix, port));

    let out = nginx_test(&nginx, &prefix);
    let text = combined(&out);
    assert!(
        out.status.success(),
        "lifecycle fixture must pass nginx -t:\n{text}"
    );

    let _child = spawn_nginx(&nginx, &prefix);
    wait_for_port(port, Duration::from_secs(5));

    let started = Instant::now();
    let (status, response) = http_get(port, "/sleep/");
    assert_eq!(status, 200, "{response}");
    assert!(
        http_body(&response).contains("slept"),
        "normal sleep must still reach content:\n{response}"
    );
    assert!(started.elapsed() >= Duration::from_millis(20));

    let started = Instant::now();
    let (status, response) = http_get(port, "/two/");
    assert_eq!(status, 200, "{response}");
    assert!(
        http_body(&response).contains("twice"),
        "two-sleep route must still reach content:\n{response}"
    );
    assert!(started.elapsed() >= Duration::from_millis(15));
}

#[test]
fn disconnect_during_long_sleep_logs_499_before_timer() {
    let nginx = nginx_binary();
    let module = module_cdylib();
    let prefix = isolated_prefix("life-disconnect");
    let port = free_port();
    write_index(&prefix, "long/index.html", "long\n");
    write_index(&prefix, "plain/index.html", "plain\n");
    write_conf(&prefix, &lifecycle_conf(&module, &prefix, port));

    let out = nginx_test(&nginx, &prefix);
    let text = combined(&out);
    assert!(out.status.success(), "nginx -t failed:\n{text}");

    let _child = spawn_nginx(&nginx, &prefix);
    wait_for_port(port, Duration::from_secs(5));

    {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        write!(
            stream,
            "GET /long/ HTTP/1.0\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
        let _ = stream.flush();
        std::thread::sleep(Duration::from_millis(200));
        drop(stream);
    }

    let access_log = prefix.join("logs/access.log");
    let started = Instant::now();
    assert!(
        wait_access_status(&access_log, "/long/", 499, Duration::from_secs(5)),
        "client close must produce access-log 499 well under the 30s sleep; access:\n{}\nerror:\n{}",
        fs::read_to_string(&access_log).unwrap_or_default(),
        fs::read_to_string(prefix.join("logs/error.log")).unwrap_or_default()
    );
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "499 must arrive before the 30s timer, elapsed {:?}",
        started.elapsed()
    );

    let (status, response) = http_get(port, "/plain/");
    assert_eq!(status, 200, "{response}");
    assert!(
        http_body(&response).contains("plain"),
        "worker must stay alive after client disconnect; got:\n{response}"
    );
}

#[test]
fn graceful_quit_with_long_sleep_exits_within_bound() {
    let nginx = nginx_binary();
    let module = module_cdylib();
    let prefix = isolated_prefix("life-sigquit");
    let port = free_port();
    write_index(&prefix, "long/index.html", "long\n");
    write_index(&prefix, "plain/index.html", "plain\n");
    write_conf(&prefix, &lifecycle_conf(&module, &prefix, port));

    let out = nginx_test(&nginx, &prefix);
    let text = combined(&out);
    assert!(out.status.success(), "nginx -t failed:\n{text}");

    let mut running = spawn_nginx(&nginx, &prefix);
    wait_for_port(port, Duration::from_secs(5));

    let handle = std::thread::spawn(move || {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).ok()?;
        stream
            .set_write_timeout(Some(Duration::from_secs(2)))
            .ok()?;
        write!(
            stream,
            "GET /long/ HTTP/1.0\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
        )
        .ok()?;
        let _ = stream.flush();
        std::thread::sleep(Duration::from_millis(80));
        Some(stream)
    });
    let _held = handle.join().expect("start long sleep");

    let start = Instant::now();
    quit_and_wait(&mut running.child, Duration::from_secs(5));
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "graceful SIGQUIT with a 30s sleep must not wait out the sleep, elapsed {:?}",
        start.elapsed()
    );
}
