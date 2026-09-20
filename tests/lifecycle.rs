//! Task 6: request cancellation, stale wake filtering, exact abort,
//! permit release, and worker exit.
//!
//! Real pd-vm drives `ngx::sleep` Waiting/Halt. NGINX action sinks and syscall
//! timing are faked only where FFI is unsuitable. Focused real NGINX tests
//! cover client disconnect and graceful SIGQUIT.

mod support;

use ngx_rustscript_nginx_test_stubs as _;

use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::io::RawFd;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

use ngx::core::Status;
use ngx_rustscript::config::compile_rss_source;
use ngx_rustscript::host::{
    AllowedOps, NgxPhase, ScopeError, capture_vm_entries, current_identity, is_active,
};
use ngx_rustscript::notify::{DEFAULT_MAX_IN_FLIGHT, RequestIdentity};
use ngx_rustscript::read_close::{
    EventFlags, HandlerAction, HandlerState, PeekResult, run_read_close_handler,
};
use ngx_rustscript::request_state::{
    AccessStatus, AsyncWait, IdentityRegistry, ReadyEffect, RecordingSink, apply_ready_effect,
};
use ngx_rustscript::runtime::WorkerRuntime;
use support::artifact::module_cdylib;
use support::nginx::{
    combined, nginx_binary, nginx_process_group_command, nginx_quote_path, nginx_test,
    terminate_process_group,
};
use support::temp::isolated_prefix;
use vm::{Program, VmStatus};

fn compile(source: &str) -> Arc<Program> {
    compile_rss_source(source).expect("compile with ngx::sleep catalog")
}

fn sleep_once(ms: i64) -> Arc<Program> {
    compile(&format!("use ngx;\nngx::sleep_ms({ms});\n"))
}

fn two_sleeps(a: i64, b: i64) -> Arc<Program> {
    compile(&format!(
        "use ngx;\nngx::sleep_ms({a});\nngx::sleep_ms({b});\n"
    ))
}

fn wait_readable(fd: RawFd, timeout: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let n = unsafe { libc::poll(&mut pfd, 1, 20) };
        if n > 0 && pfd.revents & libc::POLLIN != 0 {
            return true;
        }
    }
    false
}

struct Driver {
    worker: WorkerRuntime,
    exec: ngx_rustscript::request_state::AccessExec,
    sink: RecordingSink,
    registry: IdentityRegistry<u64>,
}

impl Driver {
    fn new(program: Arc<Program>) -> Self {
        let mut worker = WorkerRuntime::start().unwrap();
        let exec = worker.bind_access_exec(program, 1).expect("bind");
        Self {
            worker,
            exec,
            sink: RecordingSink::default(),
            registry: IdentityRegistry::with_capacity(DEFAULT_MAX_IN_FLIGHT),
        }
    }

    fn handle(&mut self) -> AccessStatus {
        let status = self.exec.handle_access(&mut self.registry, &self.worker);
        if status == AccessStatus::Again {
            self.exec.commit_suspend();
        }
        status
    }

    fn wait_ready(&mut self) -> ReadyEffect {
        let fd = self.worker.raw_fd().expect("eventfd");
        assert!(wait_readable(fd, Duration::from_secs(2)));
        let _ = self.worker.drain_ready().unwrap();
        let effect = self
            .exec
            .apply_ready(self.exec.identity(), &mut self.registry, &self.worker);
        apply_ready_effect(effect, &mut self.sink);
        effect
    }

    fn cleanup(&mut self) -> Option<RequestIdentity> {
        let request_id = self.exec.request_id();
        let identity = self.exec.identity();
        let _ = self.registry.remove_exact(identity, &request_id);
        self.exec.cleanup_wait(&self.worker)
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
fn cleanup_before_completion_ignores_later_ready() {
    let mut driver = Driver::new(sleep_once(30));
    assert_eq!(driver.handle(), AccessStatus::Again);
    let identity = driver.exec.identity();
    assert_eq!(driver.registry.len(), 1);
    assert!(driver.cleanup().is_some());
    assert!(driver.exec.is_cancelled());
    assert_eq!(driver.registry.len(), 0);
    assert_eq!(driver.exec.current_identity(), None);
    let effect = driver
        .exec
        .apply_ready(identity, &mut driver.registry, &driver.worker);
    apply_ready_effect(effect, &mut driver.sink);
    assert_eq!(effect, ReadyEffect::Ignored);
    assert_eq!(driver.sink.phase_continues, 0);
    assert_ne!(driver.exec.vm_status(), Some(VmStatus::Halted));
    assert_eq!(driver.handle(), AccessStatus::Again);
    assert_eq!(driver.sink.phase_continues, 0);
}

#[test]
fn handle_poll_resume_leave_request_scope_inactive() {
    let mut driver = Driver::new(sleep_once(1));
    let identity = driver.exec.identity();
    assert!(!is_active());
    let (status, entries) = capture_vm_entries(|| driver.handle());
    assert_eq!(status, AccessStatus::Again);
    assert!(!entries.is_empty());
    for entry in &entries {
        assert_eq!(entry.identity, identity);
        assert_eq!(entry.phase, NgxPhase::Access);
        assert_eq!(entry.allowed, AllowedOps::for_phase(NgxPhase::Access));
        assert!(!entry.request_live);
    }
    assert!(!is_active());
    assert_eq!(current_identity(), Err(ScopeError::NotInRequestScope));
    let (effect, ready_entries) = capture_vm_entries(|| driver.wait_ready());
    assert_eq!(effect, ReadyEffect::Halted);
    assert!(!ready_entries.is_empty());
    for entry in &ready_entries {
        assert_eq!(entry.identity, identity);
        assert!(!entry.request_live);
    }
    assert!(!is_active());
    assert_eq!(current_identity(), Err(ScopeError::NotInRequestScope));
}

#[test]
fn cancel_and_error_leave_request_scope_inactive() {
    let mut driver = Driver::new(sleep_once(30));
    assert_eq!(driver.handle(), AccessStatus::Again);
    assert!(driver.cleanup().is_some());
    assert!(!is_active());

    let mut driver = Driver::new(compile("1 / 0;\n"));
    assert_eq!(driver.handle(), AccessStatus::Error500);
    assert!(!is_active());
}

#[test]
fn callback_after_cleanup_is_ignored() {
    let mut driver = Driver::new(sleep_once(5));
    assert_eq!(driver.handle(), AccessStatus::Again);
    let identity = driver.exec.identity();
    assert!(driver.cleanup().is_some());
    for _ in 0..3 {
        let effect = driver
            .exec
            .apply_ready(identity, &mut driver.registry, &driver.worker);
        apply_ready_effect(effect, &mut driver.sink);
        assert_eq!(effect, ReadyEffect::Ignored);
    }
    assert_eq!(driver.sink.phase_continues, 0);
}

#[test]
fn duplicate_completion_after_halt_does_not_advance_phase_twice() {
    let mut driver = Driver::new(sleep_once(1));
    assert_eq!(driver.handle(), AccessStatus::Again);
    let first = driver.wait_ready();
    assert_eq!(first, ReadyEffect::Halted);
    let second =
        driver
            .exec
            .apply_ready(driver.exec.identity(), &mut driver.registry, &driver.worker);
    apply_ready_effect(second, &mut driver.sink);
    assert_eq!(second, ReadyEffect::Ignored);
    assert_eq!(driver.sink.phase_continues, 1);
}

#[test]
fn stale_generation_identity_is_ignored() {
    let mut driver = Driver::new(sleep_once(1));
    assert_eq!(driver.handle(), AccessStatus::Again);
    let identity = driver.exec.identity();
    let forged = RequestIdentity::new(identity.generation().wrapping_add(1), identity.request_id());
    let effect = driver
        .exec
        .apply_ready(forged, &mut driver.registry, &driver.worker);
    apply_ready_effect(effect, &mut driver.sink);
    assert_eq!(effect, ReadyEffect::Ignored);
    assert_eq!(driver.sink.phase_continues, 0);
    assert_eq!(driver.registry.len(), 1);
}

#[test]
fn request_id_and_pointer_key_reuse_is_generation_safe() {
    let mut ids = IdentityRegistry::<u64>::with_capacity(8);
    let mut live = IdentityRegistry::<*mut u8>::with_capacity(8);
    let ptr = 0x1000 as *mut u8;
    let old = RequestIdentity::new(1, 1);
    let new = RequestIdentity::new(2, 2);

    ids.try_insert(old, 1).expect("old id");
    live.try_insert(old, ptr).expect("old ptr");
    assert_eq!(ids.remove_exact(old, &1), Some(1));
    assert_eq!(live.remove_exact(old, &ptr), Some(ptr));
    ids.try_insert(new, 2).expect("reused request id space");
    live.try_insert(new, ptr).expect("reused pointer key");
    assert_eq!(ids.remove_exact(old, &1), None);
    assert_eq!(live.remove_exact(old, &ptr), None);
    assert_eq!(ids.get(new), Some(&2));
}

#[test]
fn cleanup_after_normal_halted_is_idempotent() {
    let mut driver = Driver::new(sleep_once(1));
    assert_eq!(driver.handle(), AccessStatus::Again);
    let effect = driver.wait_ready();
    assert_eq!(effect, ReadyEffect::Halted);
    assert_eq!(driver.handle(), AccessStatus::Ok);
    assert!(driver.cleanup().is_none());
    assert!(driver.cleanup().is_none());
    assert_eq!(driver.sink.phase_continues, 1);
}

#[test]
fn cleanup_after_error_is_idempotent() {
    let mut driver = Driver::new(compile("1 / 0;\n"));
    assert_eq!(driver.handle(), AccessStatus::Error500);
    assert!(driver.cleanup().is_none());
    assert!(driver.cleanup().is_none());
    assert_eq!(driver.handle(), AccessStatus::Error500);
}

#[test]
fn cancel_while_second_sequential_sleep_pending() {
    let mut driver = Driver::new(two_sleeps(1, 2));
    assert_eq!(driver.handle(), AccessStatus::Again);
    let first = driver.wait_ready();
    assert_eq!(first, ReadyEffect::WaitingAgain);
    assert_eq!(driver.sink.phase_continues, 0);
    let identity = driver.exec.identity();
    assert!(driver.cleanup().is_some());
    let effect = driver
        .exec
        .apply_ready(identity, &mut driver.registry, &driver.worker);
    apply_ready_effect(effect, &mut driver.sink);
    assert_eq!(effect, ReadyEffect::Ignored);
    assert_eq!(driver.sink.phase_continues, 0);
    assert_eq!(driver.handle(), AccessStatus::Again);
}

#[test]
fn waiting_maps_to_nginx_again_unchanged() {
    assert_eq!(AccessStatus::Again.as_nginx(), Status::NGX_AGAIN);
    assert_eq!(Status::NGX_AGAIN.0, -2);
}

#[test]
fn real_vm_cancel_drops_future_and_permit() {
    let mut worker = WorkerRuntime::start().unwrap();
    let mut exec = worker
        .bind_access_exec(sleep_once(60_000), 11)
        .expect("bind");
    let mut registry = IdentityRegistry::with_capacity(8);
    assert_eq!(
        exec.handle_access(&mut registry, &worker),
        AccessStatus::Again
    );
    exec.commit_suspend();
    let identity = exec.identity();
    assert_eq!(registry.len(), 1);
    assert_eq!(worker.in_flight().unwrap(), 1);
    let _ = registry.remove_exact(identity, &11);
    let taken = exec.cleanup_wait(&worker);
    assert_eq!(taken, Some(identity));
    assert!(exec.is_cancelled());
    assert!(wait_in_flight_zero(&worker, Duration::from_secs(2)));
    assert_eq!(
        exec.apply_ready(identity, &mut registry, &worker),
        ReadyEffect::Ignored
    );
    worker.shutdown();
}

#[test]
fn close_triggered_cleanup_aborts_and_zeros_permit() {
    let mut worker = WorkerRuntime::start().unwrap();
    let mut exec = worker
        .bind_access_exec(sleep_once(60_000), 21)
        .expect("bind");
    let mut registry = IdentityRegistry::with_capacity(8);
    assert_eq!(
        exec.handle_access(&mut registry, &worker),
        AccessStatus::Again
    );
    let identity = exec.identity();
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

    let _ = registry.remove_exact(identity, &21);
    let taken = exec.cleanup_wait(&worker);
    assert_eq!(taken, Some(identity));
    assert!(exec.is_cancelled());
    assert!(wait_in_flight_zero(&worker, Duration::from_secs(2)));
    assert_eq!(
        exec.apply_ready(identity, &mut registry, &worker),
        ReadyEffect::Ignored
    );
    worker.shutdown();
}

#[test]
fn worker_shutdown_with_in_flight_waits_is_fast_and_zeros_permits() {
    let mut worker = WorkerRuntime::start().unwrap();
    let mut a = worker.bind_access_exec(sleep_once(60_000), 1).unwrap();
    let mut b = worker.bind_access_exec(sleep_once(60_000), 2).unwrap();
    let mut registry = IdentityRegistry::with_capacity(8);
    assert_eq!(a.handle_access(&mut registry, &worker), AccessStatus::Again);
    assert_eq!(b.handle_access(&mut registry, &worker), AccessStatus::Again);
    assert_eq!(worker.in_flight().unwrap(), 2);
    a.cleanup_wait(&worker);
    b.cleanup_wait(&worker);
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
    assert!(worker.reserve().is_err());
    worker.shutdown();
    worker.shutdown();
    assert!(worker.reserve().is_err());
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
        terminate_process_group(&mut self.child, Duration::from_secs(2));
    }
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn buffered_http_upstream() -> (u16, Arc<AtomicBool>, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind lifecycle upstream");
    let port = listener.local_addr().expect("upstream address").port();
    listener
        .set_nonblocking(true)
        .expect("nonblocking upstream");
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = Arc::clone(&stop);
    let join = std::thread::spawn(move || {
        while !thread_stop.load(Ordering::Acquire) {
            let (mut stream, _) = match listener.accept() {
                Ok(accepted) => accepted,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                }
                Err(error) => panic!("accept lifecycle request: {error}"),
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .expect("upstream read timeout");
            let mut request = [0u8; 4096];
            let _read = stream.read(&mut request).expect("read lifecycle request");
            stream
                .write_all(
                    b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .expect("write lifecycle response");
        }
    });
    (port, stop, join)
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
    let child = nginx_process_group_command(nginx)
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
    support::nginx::wait_for_worker_http(port, timeout);
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
fn buffered_http_requests_reuse_quiescent_vms_with_fresh_request_context() {
    const REQUESTS: usize = 8;
    let nginx = nginx_binary();
    let module = module_cdylib();
    let prefix = isolated_prefix("life-http-reuse");
    let port = free_port();
    let (upstream_port, stop_upstream, upstream) = buffered_http_upstream();
    let script_path = prefix.join("scripts/http_reuse.rss");
    fs::create_dir_all(script_path.parent().expect("script parent")).unwrap();
    fs::write(
        &script_path,
        format!(
            r#"
use http;
use ngx;
if ngx::ctx::get("seen") != null {{ ngx::exit(500); }}
ngx::ctx::set("seen", 1);
let response: map = http::client::request({{
    method: "GET",
    url: "http://127.0.0.1:{upstream_port}/",
    body: null
}});
if response["status"] != 204 {{ ngx::exit(502); }}
"#
        ),
    )
    .unwrap();
    write_index(&prefix, "reuse/index.html", "reused\n");

    let module = nginx_quote_path(&module).unwrap();
    let pid = nginx_quote_path(&prefix.join("logs/nginx.pid")).unwrap();
    let html = nginx_quote_path(&prefix.join("html")).unwrap();
    let error_log = nginx_quote_path(&prefix.join("logs/error.log")).unwrap();
    let script = nginx_quote_path(&script_path).unwrap();
    write_conf(
        &prefix,
        &format!(
            r#"
worker_processes 1;
error_log {error_log} info;
pid {pid};
daemon off;
master_process on;
load_module {module};
events {{ worker_connections 32; }}
http {{
    access_log off;
    default_type text/plain;
    server {{
        listen 127.0.0.1:{port};
        root {html};
        location /reuse/ {{
            rss_http_scheme http;
            rss_http_allow 127.0.0.1 {upstream_port};
            rss_http_allow_private on;
            access_by_rss_file {script};
        }}
    }}
}}
"#
        ),
    );

    let out = nginx_test(&nginx, &prefix);
    let text = combined(&out);
    assert!(out.status.success(), "nginx -t failed:\n{text}");
    let _running = spawn_nginx(&nginx, &prefix);
    wait_for_port(port, Duration::from_secs(5));
    for _ in 0..REQUESTS {
        let (status, response) = http_get(port, "/reuse/");
        assert_eq!(
            status,
            200,
            "{response}\nerror log:\n{}",
            fs::read_to_string(prefix.join("logs/error.log")).unwrap_or_default()
        );
        assert!(http_body(&response).contains("reused"), "{response}");
    }
    stop_upstream.store(true, Ordering::Release);
    upstream.join().expect("lifecycle upstream");
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

#[test]
fn nginx_op_cancel_and_stale_callback_are_exactly_once() {
    use std::future::Future;
    use std::pin::Pin;
    use std::task::{Context, Poll, Waker};

    use ngx_rustscript::nginx_op::{NginxOpOutcome, NginxOpState};

    fn poll_once<F: Future>(fut: Pin<&mut F>) -> Poll<F::Output> {
        let waker = Waker::noop();
        let mut cx = Context::from_waker(waker);
        fut.poll(&mut cx)
    }

    let identity = RequestIdentity::new(9, 3);
    let mut state = NginxOpState::new();
    let _cell = state.begin(identity);
    let mut fut = state.observe().expect("observe");
    assert!(matches!(poll_once(Pin::new(&mut fut)), Poll::Pending));
    assert!(state.complete_from_callback(NginxOpOutcome::Done));
    assert!(!state.complete_from_callback(NginxOpOutcome::Special(499)));
    match poll_once(Pin::new(&mut fut)) {
        Poll::Ready(Ok(NginxOpOutcome::Done)) => {}
        other => panic!("expected Done, got {other:?}"),
    }

    let mut state = NginxOpState::new();
    let _cell = state.begin(identity);
    let mut fut = state.observe().expect("observe");
    state.cancel();
    assert!(!state.complete_from_callback(NginxOpOutcome::Done));
    match poll_once(Pin::new(&mut fut)) {
        Poll::Ready(Ok(NginxOpOutcome::Cancelled)) => {}
        other => panic!("expected Cancelled, got {other:?}"),
    }
}

/// Helper-only saturating arithmetic on a zeroed request.
///
/// This is not the NGINX 1.30.4 body-read main-count lease. That contract is
/// asserted by `tests/body_hooks.rs` against a pinned worker.
#[test]
fn request_main_count_c_helpers_are_saturating_and_not_the_body_lease() {
    use std::mem::MaybeUninit;

    use nginx_sys::ngx_http_request_t;

    unsafe extern "C" {
        fn ngx_rustscript_c_request_main_count(r: *const ngx_http_request_t) -> u32;
        fn ngx_rustscript_c_request_main_count_inc(r: *mut ngx_http_request_t);
        fn ngx_rustscript_c_request_main_count_dec(r: *mut ngx_http_request_t);
    }

    let mut req = Box::new(unsafe { MaybeUninit::<ngx_http_request_t>::zeroed().assume_init() });
    let ptr = (&mut *req) as *mut ngx_http_request_t;
    req.main = ptr;
    unsafe {
        assert_eq!(ngx_rustscript_c_request_main_count(ptr), 0);
        ngx_rustscript_c_request_main_count_inc(ptr);
        assert_eq!(ngx_rustscript_c_request_main_count(ptr), 1);
        ngx_rustscript_c_request_main_count_inc(ptr);
        assert_eq!(ngx_rustscript_c_request_main_count(ptr), 2);
        ngx_rustscript_c_request_main_count_dec(ptr);
        assert_eq!(ngx_rustscript_c_request_main_count(ptr), 1);
        ngx_rustscript_c_request_main_count_dec(ptr);
        assert_eq!(ngx_rustscript_c_request_main_count(ptr), 0);
        ngx_rustscript_c_request_main_count_dec(ptr);
        assert_eq!(
            ngx_rustscript_c_request_main_count(ptr),
            0,
            "count must not underflow"
        );
    }
}
