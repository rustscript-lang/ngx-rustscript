//! Task 7 control-flow hosts: exit/redirect disposition, VM stop protocol,
//! AccessExec finalization, and real NGINX observations.

use ngx_rustscript_nginx_test_stubs as _;

use std::fs;
use std::io::{Read, Write};
use std::mem::MaybeUninit;
use std::net::{TcpListener, TcpStream};
use std::os::unix::io::RawFd;
use std::path::Path;
use std::process::{Child, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use nginx_sys::ngx_http_request_t;
use ngx_rustscript::config::{compile_rss_source, program_requires_request_state};
use ngx_rustscript::host::{
    AllowedOps, HostCapability, HostFamily, HostSyncKind, NgxPhase, bind_registry_for_test,
    host_allowed_in_phase, manifest_entry, ngx_host_catalog, with_request_scope,
};
use ngx_rustscript::notify::{DEFAULT_MAX_IN_FLIGHT, RequestIdentity};
use ngx_rustscript::request_state::{
    AccessStatus, IdentityRegistry, ReadyEffect, RecordingSink, apply_ready_effect,
};
use ngx_rustscript::runtime::WorkerRuntime;
use vm::{HostTypeSchema, Program, Vm};

mod support;
use support::artifact::module_cdylib;
use support::nginx::{
    combined, nginx_binary, nginx_process_group_command, nginx_quote_path, nginx_test,
    terminate_process_group,
};
use support::temp::isolated_prefix;

const EXIT: &str = "ngx::exit";
const REDIRECT: &str = "ngx::redirect";

fn compile(source: &str) -> Arc<Program> {
    compile_rss_source(source).expect("source should compile against the ngx catalog")
}

fn dummy_request() -> Box<ngx_http_request_t> {
    let mut req = Box::new(unsafe { MaybeUninit::<ngx_http_request_t>::zeroed().assume_init() });
    let ptr = (&mut *req) as *mut ngx_http_request_t;
    req.main = ptr;
    req
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
    request: Box<ngx_http_request_t>,
}

impl Driver {
    fn new(program: Arc<Program>, request_id: u64) -> Self {
        let mut worker = WorkerRuntime::start().unwrap();
        let exec = worker
            .bind_access_exec_for_test(program, request_id)
            .expect("bind");
        Self {
            worker,
            exec,
            sink: RecordingSink::default(),
            registry: IdentityRegistry::with_capacity(DEFAULT_MAX_IN_FLIGHT),
            request: dummy_request(),
        }
    }

    fn ptr(&mut self) -> *mut ngx_http_request_t {
        self.request.as_mut() as *mut ngx_http_request_t
    }

    fn handle(&mut self) -> AccessStatus {
        let identity = self.exec.identity();
        let raw = self.ptr();
        let status = with_request_scope(
            raw,
            identity,
            NgxPhase::Access,
            AllowedOps::for_phase(NgxPhase::Access),
            || self.exec.handle_access(&mut self.registry, &self.worker),
        );
        if status == AccessStatus::Again {
            self.exec.commit_suspend();
        }
        status
    }

    fn wait_ready(&mut self) -> ReadyEffect {
        let fd = self.worker.raw_fd().expect("eventfd");
        assert!(wait_readable(fd, Duration::from_secs(2)));
        let _ = self.worker.drain_ready().unwrap();
        let identity = self.exec.identity();
        let raw = self.ptr();
        let effect = with_request_scope(
            raw,
            identity,
            NgxPhase::Access,
            AllowedOps::for_phase(NgxPhase::Access),
            || {
                self.exec
                    .apply_ready(identity, &mut self.registry, &self.worker)
            },
        );
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

#[test]
fn exit_and_redirect_are_catalogued_control_hosts() {
    for name in [EXIT, REDIRECT] {
        let entry = manifest_entry(name).unwrap_or_else(|| panic!("missing {name}"));
        assert_eq!(entry.family, HostFamily::Control);
        assert_eq!(entry.sync, HostSyncKind::Sync);
        assert_eq!(entry.capabilities, &[HostCapability::Control]);
        assert!(host_allowed_in_phase(name, NgxPhase::Access));
        assert!(host_allowed_in_phase(name, NgxPhase::Rewrite));
        assert!(host_allowed_in_phase(name, NgxPhase::Content));
        assert!(!host_allowed_in_phase(name, NgxPhase::Log));
        assert!(!host_allowed_in_phase(name, NgxPhase::Init));
    }
    assert!(host_allowed_in_phase(EXIT, NgxPhase::HeaderFilter));
    assert!(!host_allowed_in_phase(REDIRECT, NgxPhase::HeaderFilter));
    assert!(ngx_host_catalog().function("ngx::exec").is_none());

    let catalog = ngx_host_catalog();
    let exit = catalog.function(EXIT).unwrap();
    assert_eq!(exit.params.len(), 1);
    assert_eq!(exit.params[0].ty, HostTypeSchema::Int);
    assert_eq!(exit.return_type, HostTypeSchema::Null);

    let redirect = catalog.function(REDIRECT).unwrap();
    assert_eq!(redirect.params.len(), 2);
    assert_eq!(redirect.params[0].ty, HostTypeSchema::String);
    assert_eq!(redirect.params[1].ty, HostTypeSchema::Unknown);
    assert_eq!(redirect.return_type, HostTypeSchema::Null);
}

#[test]
fn control_programs_require_request_state() {
    let program = compile("use ngx;\nngx::exit(403);\n");
    assert!(program_requires_request_state(&program));
    let redirect = compile("use ngx;\nngx::redirect(\"/x\", null);\n");
    assert!(program_requires_request_state(&redirect));
    let access = AllowedOps::for_phase(NgxPhase::Access);
    assert!(access.contains(AllowedOps::CONTROL));
}

#[test]
fn exit_stops_vm_and_first_disposition_wins() {
    let program = compile(
        r#"
use ngx;
ngx::exit(403);
ngx::exit(200);
ngx::log(ngx::log_level::notice(), "CONTROL_MARKER");
"#,
    );
    let mut driver = Driver::new(program, 1);
    let status = driver.handle();
    assert_eq!(status, AccessStatus::Finalize(403));
    assert!(driver.cleanup().is_none());
}

#[test]
fn ordinary_host_error_cannot_forge_control_flow() {
    let program = compile("use ngx;\nngx::req::set_method(99999);\n");
    let mut driver = Driver::new(program, 2);
    let status = driver.handle();
    assert_eq!(status, AccessStatus::Error500);
}

#[test]
fn exit_ngx_ok_continues_access_phase() {
    let program = compile("use ngx;\nngx::exit(ngx::core::ok());\n");
    let mut driver = Driver::new(program, 3);
    assert_eq!(driver.handle(), AccessStatus::Ok);
}

#[test]
fn exit_rejects_again_and_done() {
    let again = compile("use ngx;\nngx::exit(ngx::core::again());\n");
    let mut driver = Driver::new(again, 4);
    assert_eq!(driver.handle(), AccessStatus::Error500);
    let done = compile("use ngx;\nngx::exit(ngx::core::done());\n");
    let mut driver = Driver::new(done, 5);
    assert_eq!(driver.handle(), AccessStatus::Error500);
}

#[test]
fn redirect_validates_status_and_rejects_unsafe_uri() {
    let bad_status = compile("use ngx;\nngx::redirect(\"/x\", 200);\n");
    let mut driver = Driver::new(bad_status, 6);
    assert_eq!(driver.handle(), AccessStatus::Error500);
    let unsafe_uri = compile("use ngx;\nngx::redirect(\"/x\\r\\nY: 1\", null);\n");
    let mut driver = Driver::new(unsafe_uri, 7);
    assert_eq!(driver.handle(), AccessStatus::Error500);
}

#[test]
fn exit_after_sleep_consumes_disposition_once() {
    let program = compile(
        r#"
use ngx;
ngx::sleep_ms(20);
ngx::exit(403);
ngx::log(ngx::log_level::notice(), "SLEEP_MARKER");
"#,
    );
    let mut driver = Driver::new(program, 8);
    assert_eq!(driver.handle(), AccessStatus::Again);
    let effect = driver.wait_ready();
    assert_eq!(effect, ReadyEffect::Halted);
    assert_eq!(driver.handle(), AccessStatus::Finalize(403));
    assert!(driver.cleanup().is_none());
}

#[test]
fn cancel_and_stale_wake_after_disposition_are_inert() {
    let program = compile("use ngx;\nngx::sleep_ms(200);\nngx::exit(403);\n");
    let mut driver = Driver::new(program, 9);
    assert_eq!(driver.handle(), AccessStatus::Again);
    let identity = driver.cleanup();
    assert!(identity.is_some());
    let stale = driver
        .exec
        .apply_ready(identity.unwrap(), &mut driver.registry, &driver.worker);
    assert_eq!(stale, ReadyEffect::Ignored);
}

#[test]
fn control_requires_control_capability() {
    let program = compile("use ngx;\nngx::exit(403);\n");
    let mut vm = Vm::try_new((*program).clone()).expect("vm");
    bind_registry_for_test(&mut vm).expect("bind test registry");
    let mut req = dummy_request();
    let ptr = req.as_mut() as *mut ngx_http_request_t;
    let err = with_request_scope(
        ptr,
        RequestIdentity::new(1, 1),
        NgxPhase::Access,
        AllowedOps::REQUEST_READ.union(AllowedOps::REQUEST_WRITE),
        || match vm.run() {
            Err(err) => err.to_string(),
            Ok(status) => panic!("expected capability deny, got {status:?}"),
        },
    );
    assert!(err.contains("not allowed"), "{err}");
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

struct RunningNginx {
    child: Child,
}

impl Drop for RunningNginx {
    fn drop(&mut self) {
        terminate_process_group(&mut self.child, Duration::from_secs(2));
    }
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

fn http_exchange(port: u16, request: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    stream.write_all(request.as_bytes()).unwrap();
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

fn write_script(path: &Path, body: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, body).unwrap();
}

#[test]
fn nginx_exit_redirect_sync_and_after_sleep() {
    let nginx = nginx_binary();
    let module = module_cdylib();
    let port = free_port();
    let prefix = isolated_prefix("ngx-rustscript-control");
    fs::create_dir_all(prefix.join("html")).unwrap();
    fs::write(prefix.join("html/index.html"), "ok\n").unwrap();
    write_script(
        &prefix.join("conf/exit403.rss"),
        r#"
use ngx;
ngx::exit(403);
ngx::log(ngx::log_level::notice(), "EXIT_MARKER");
"#,
    );
    write_script(
        &prefix.join("conf/exitok.rss"),
        r#"
use ngx;
ngx::exit(ngx::core::ok());
ngx::log(ngx::log_level::notice(), "OK_CONTINUED");
"#,
    );
    write_script(
        &prefix.join("conf/redir.rss"),
        r#"
use ngx;
ngx::redirect("/dest", null);
ngx::log(ngx::log_level::notice(), "REDIR_MARKER");
"#,
    );
    write_script(
        &prefix.join("conf/redir301.rss"),
        r#"
use ngx;
ngx::redirect("http://example.test/x", ngx::http::status_moved_permanently());
"#,
    );
    write_script(
        &prefix.join("conf/exitsleep.rss"),
        r#"
use ngx;
ngx::sleep_ms(20);
ngx::exit(401);
ngx::log(ngx::log_level::notice(), "SLEEP_EXIT_MARKER");
"#,
    );
    let module_q = nginx_quote_path(&module).unwrap();
    let e403 = nginx_quote_path(&prefix.join("conf/exit403.rss")).unwrap();
    let eok = nginx_quote_path(&prefix.join("conf/exitok.rss")).unwrap();
    let redir = nginx_quote_path(&prefix.join("conf/redir.rss")).unwrap();
    let redir301 = nginx_quote_path(&prefix.join("conf/redir301.rss")).unwrap();
    let esleep = nginx_quote_path(&prefix.join("conf/exitsleep.rss")).unwrap();
    let conf = format!(
        r#"
worker_processes 1;
error_log "{error_log}" notice;
pid {pid};
daemon off;
master_process off;
load_module {module_q};

events {{
    worker_connections 32;
}}

http {{
    access_log off;
    default_type text/plain;
    server {{
        listen 127.0.0.1:{port};
        server_name localhost;
        root {html};
        location /e403 {{
            access_by_rss_file {e403};
        }}
        location /eok {{
            access_by_rss_file {eok};
            try_files /index.html =404;
        }}
        location /redir {{
            access_by_rss_file {redir};
        }}
        location /redir301 {{
            access_by_rss_file {redir301};
        }}
        location /esleep {{
            access_by_rss_file {esleep};
        }}
        location /dest {{
            try_files /index.html =404;
        }}
    }}
}}
"#,
        error_log = prefix.join("logs/error.log").display(),
        pid = nginx_quote_path(&prefix.join("logs/nginx.pid")).unwrap(),
        html = nginx_quote_path(&prefix.join("html")).unwrap(),
    );
    fs::write(prefix.join("conf/nginx.conf"), &conf).unwrap();
    let out = nginx_test(&nginx, &prefix);
    let text = combined(&out);
    assert!(out.status.success(), "nginx -t failed:\n{text}");
    let _child = spawn_nginx(&nginx, &prefix);
    wait_for_port(port, Duration::from_secs(5));

    let (status, body) = http_exchange(
        port,
        "GET /e403 HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
    );
    let log = fs::read_to_string(prefix.join("logs/error.log")).unwrap_or_default();
    assert_eq!(status, 403, "{body}\n{log}");
    assert!(!log.contains("EXIT_MARKER"), "post-exit marker: {log}");

    let (status, body) = http_exchange(
        port,
        "GET /eok HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
    );
    let log = fs::read_to_string(prefix.join("logs/error.log")).unwrap_or_default();
    assert_eq!(status, 200, "{body}");
    assert!(
        !log.contains("OK_CONTINUED"),
        "ngx.exit(ok) must stop the VM: {log}"
    );

    let (status, body) = http_exchange(
        port,
        "GET /redir HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
    );
    let log = fs::read_to_string(prefix.join("logs/error.log")).unwrap_or_default();
    assert_eq!(status, 302, "{body}\n{log}");
    assert!(
        body.contains("Location: /dest") || body.contains("location: /dest"),
        "Location: {body}"
    );
    assert!(!log.contains("REDIR_MARKER"), "{log}");

    let (status, body) = http_exchange(
        port,
        "GET /redir301 HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
    );
    assert_eq!(status, 301, "{body}");
    assert!(
        body.contains("http://example.test/x"),
        "absolute Location: {body}"
    );

    let (status, body) = http_exchange(
        port,
        "GET /esleep HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
    );
    let log = fs::read_to_string(prefix.join("logs/error.log")).unwrap_or_default();
    assert_eq!(status, 401, "{body}\n{log}");
    assert!(!log.contains("SLEEP_EXIT_MARKER"), "{log}");
}

#[test]
fn concurrent_control_dispositions_are_isolated() {
    let alpha = compile(
        r#"
use ngx;
ngx::status::set(201);
ngx::sleep_ms(20);
ngx::exit(201);
ngx::log(ngx::log_level::error(), "ALPHA_MARKER");
"#,
    );
    let beta = compile(
        r#"
use ngx;
ngx::status::set(202);
ngx::sleep_ms(20);
ngx::exit(202);
ngx::log(ngx::log_level::error(), "BETA_MARKER");
"#,
    );
    let mut a = Driver::new(alpha, 21);
    let mut b = Driver::new(beta, 22);
    assert_eq!(a.handle(), AccessStatus::Again);
    assert_eq!(b.handle(), AccessStatus::Again);
    assert_eq!(a.wait_ready(), ReadyEffect::Halted);
    assert_eq!(b.wait_ready(), ReadyEffect::Halted);
    assert_eq!(a.handle(), AccessStatus::Finalize(201));
    assert_eq!(b.handle(), AccessStatus::Finalize(202));
}

#[test]
fn source_never_calls_finalize_or_bitfield_setters() {
    let request = include_str!("../src/host/request.rs");
    let response = include_str!("../src/host/response_meta.rs");
    let variables = include_str!("../src/host/variables.rs");
    let access = include_str!("../src/access.rs");
    let state = include_str!("../src/request_state.rs");
    for src in [request, response, variables, access, state] {
        assert!(
            !src.contains("ngx_http_finalize_request"),
            "hosts/access must not call ngx_http_finalize_request"
        );
        assert!(
            !src.contains(".internal(")
                && !src.contains(".header_sent(")
                && !src.contains(".not_found(")
                && !src.contains(".set_not_found(")
                && !src.contains(".set_valid("),
            "must not use bindgen bitfield getters/setters"
        );
    }
}
