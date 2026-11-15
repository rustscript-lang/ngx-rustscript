//! Task 17: typed TCP cosocket resource ABI and Tokio lifecycle.
//!
//! Covers catalog/resource typing, double close, stale reuse, connect,
//! send/receive modes, receiveany/receiveuntil, peer close, timeouts, and
//! NGINX-oriented echo/fragment fixtures.

use ngx_rustscript_nginx_test_stubs as _;

use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::net::{TcpListener as StdTcpListener, TcpStream as StdTcpStream};
use std::os::unix::io::RawFd;
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use ngx_rustscript::config::{ConfigError, compile_rss_source};
use ngx_rustscript::host::{
    HostCapability, HostFamily, HostSyncKind, NGX_SOCKET_CONNECT_NAME, NGX_SOCKET_TCP_CLOSE_NAME,
    NGX_SOCKET_TCP_CONNECT_NAME, NGX_SOCKET_TCP_NAME, NGX_SOCKET_TCP_RECEIVE_NAME,
    NGX_SOCKET_TCP_RECEIVEANY_NAME, NGX_SOCKET_TCP_RECEIVEUNTIL_NAME, NGX_SOCKET_TCP_RESOURCE_KEY,
    NGX_SOCKET_TCP_SEND_NAME, NGX_SOCKET_TCP_SETTIMEOUT_NAME, NGX_SOCKET_UDP_NAME, NgxPhase,
    host_allowed_in_phase, manifest_entry, ngx_host_catalog, production_runtime_host_names,
};
use ngx_rustscript::notify::DEFAULT_MAX_IN_FLIGHT;
use ngx_rustscript::request_state::{AccessStatus, IdentityRegistry, ReadyEffect};
use ngx_rustscript::runtime::WorkerRuntime;
use vm::{HostParamPassing, HostTypeSchema, Program, ResourceTypeKey, Vm, VmStatus};

fn compile(source: &str) -> Arc<Program> {
    compile_rss_source(source).unwrap_or_else(|err| panic!("compile failed: {err}"))
}

fn compile_err(source: &str) -> ConfigError {
    compile_rss_source(source).expect_err("source should fail configuration")
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

fn run_sync(source: &str) -> Result<VmStatus, String> {
    let program = compile(source);
    let mut vm = Vm::try_new((*program).clone()).expect("construct vm");
    ngx_rustscript::host::bind_ngx_hosts(&mut vm);
    vm.run().map_err(|err| err.to_string())
}

struct Driver {
    worker: WorkerRuntime,
    exec: ngx_rustscript::request_state::AccessExec,
    registry: IdentityRegistry<u64>,
}

impl Driver {
    fn new(program: Arc<Program>) -> Self {
        let mut worker = WorkerRuntime::start().unwrap();
        let exec = worker.bind_access_exec(program, 1).expect("bind");
        Self {
            worker,
            exec,
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

    fn pump_until(mut self, timeout: Duration) -> AccessStatus {
        let deadline = Instant::now() + timeout;
        let mut status = self.handle();
        while status == AccessStatus::Again && Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let fd = self.worker.raw_fd().expect("eventfd");
            if !wait_readable(fd, remaining) {
                self.worker.shutdown();
                panic!("timed out waiting for TCP host completion");
            }
            let _ = self.worker.drain_ready().unwrap();
            match self
                .exec
                .apply_ready(self.exec.identity(), &mut self.registry, &self.worker)
            {
                ReadyEffect::Halted => {
                    status = AccessStatus::Ok;
                    break;
                }
                ReadyEffect::Failed => {
                    status = AccessStatus::Error500;
                    break;
                }
                ReadyEffect::WaitingAgain => {
                    self.exec.commit_suspend();
                    status = AccessStatus::Again;
                }
                ReadyEffect::Deferred | ReadyEffect::Ignored => {
                    status = self.handle();
                }
            }
        }
        self.worker.shutdown();
        status
    }

    fn cleanup_pending(mut self) {
        let request_id = self.exec.request_id();
        let identity = self.exec.identity();
        let _ = self.registry.remove_exact(identity, &request_id);
        let _ = self.exec.cleanup_wait(&self.worker);
        assert_eq!(
            self.worker.in_flight().unwrap(),
            0,
            "cleanup must release the in-flight wait"
        );
        self.worker.shutdown();
    }
}

fn tcp_key() -> ResourceTypeKey {
    ResourceTypeKey::new(NGX_SOCKET_TCP_RESOURCE_KEY).expect("valid tcp resource key")
}

fn bind_listener(addr: &str) -> (StdTcpListener, u16) {
    let listener = StdTcpListener::bind(addr).expect("bind tcp listener");
    listener.set_nonblocking(false).expect("blocking listener");
    let port = listener.local_addr().unwrap().port();
    (listener, port)
}

fn spawn_echo(listener: StdTcpListener) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept echo");
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("read timeout");
        stream
            .set_write_timeout(Some(Duration::from_secs(2)))
            .expect("write timeout");
        let mut buf = [0u8; 4096];
        let n = stream.read(&mut buf).expect("echo recv");
        stream.write_all(&buf[..n]).expect("echo send");
        stream.shutdown(std::net::Shutdown::Write).ok();
    })
}

fn spawn_scripted<F>(listener: StdTcpListener, body: F) -> thread::JoinHandle<()>
where
    F: FnOnce(StdTcpStream) + Send + 'static,
{
    thread::spawn(move || {
        let (stream, _) = listener.accept().expect("accept");
        body(stream);
    })
}

fn pump(source: &str, timeout: Duration) -> AccessStatus {
    Driver::new(compile(source)).pump_until(timeout)
}

fn tmp_unix_path() -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let root = std::env::var_os("NGX_RUSTSCRIPT_TASK_TMP")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    std::fs::create_dir_all(&root).expect("tmp root");
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    root.join(format!("tcp-unix-{}-{n}.sock", std::process::id()))
}

#[test]
fn catalog_declares_typed_tcp_resource_and_hosts() {
    let catalog = ngx_host_catalog();
    let key = tcp_key();
    let resource = catalog
        .resource(NGX_SOCKET_TCP_RESOURCE_KEY)
        .expect("tcp resource type must be catalogued");
    assert_eq!(resource.key, key);

    let ctor = catalog
        .function(NGX_SOCKET_TCP_NAME)
        .expect("tcp constructor");
    assert!(ctor.params.is_empty());
    assert_eq!(ctor.return_type, HostTypeSchema::Resource(key.clone()));

    let shortcut = catalog
        .function(NGX_SOCKET_CONNECT_NAME)
        .expect("ngx::socket::connect convenience");
    assert_eq!(shortcut.params.len(), 2);
    assert_eq!(shortcut.params[0].name, "host");
    assert_eq!(shortcut.params[1].name, "port");
    assert_eq!(shortcut.return_type, HostTypeSchema::Resource(key.clone()));

    for (name, extra) in [
        (NGX_SOCKET_TCP_CONNECT_NAME, 2usize),
        (NGX_SOCKET_TCP_SEND_NAME, 1),
        (NGX_SOCKET_TCP_RECEIVE_NAME, 1),
        (NGX_SOCKET_TCP_RECEIVEANY_NAME, 1),
        (NGX_SOCKET_TCP_RECEIVEUNTIL_NAME, 1),
        (NGX_SOCKET_TCP_CLOSE_NAME, 0),
        (NGX_SOCKET_TCP_SETTIMEOUT_NAME, 1),
    ] {
        let schema = catalog.function(name).unwrap_or_else(|| panic!("{name}"));
        assert_eq!(schema.params.len(), extra + 1, "{name} arity");
        assert_eq!(
            schema.params[0].ty,
            HostTypeSchema::Resource(key.clone()),
            "{name} sock is a typed resource"
        );
        assert_ne!(
            schema.params[0].passing,
            HostParamPassing::Value,
            "{name} must not pass the resource by value"
        );
        assert!(
            production_runtime_host_names().contains(&name),
            "{name} must be bound"
        );
    }
    assert!(production_runtime_host_names().contains(&NGX_SOCKET_TCP_NAME));
    assert!(production_runtime_host_names().contains(&NGX_SOCKET_CONNECT_NAME));
}

#[test]
fn tcp_hosts_are_cosocket_family_in_openresty_phases() {
    for name in [
        NGX_SOCKET_TCP_NAME,
        NGX_SOCKET_TCP_CONNECT_NAME,
        NGX_SOCKET_CONNECT_NAME,
        NGX_SOCKET_TCP_SEND_NAME,
        NGX_SOCKET_TCP_RECEIVE_NAME,
        NGX_SOCKET_TCP_RECEIVEANY_NAME,
        NGX_SOCKET_TCP_RECEIVEUNTIL_NAME,
        NGX_SOCKET_TCP_CLOSE_NAME,
        NGX_SOCKET_TCP_SETTIMEOUT_NAME,
    ] {
        let entry = manifest_entry(name).unwrap_or_else(|| panic!("manifest {name}"));
        assert_eq!(entry.family, HostFamily::Cosocket);
        assert_eq!(entry.capabilities, &[HostCapability::Cosocket]);
        assert!(
            entry.openresty_name.starts_with("ngx.socket."),
            "{name} openresty name {}",
            entry.openresty_name
        );
        for phase in [
            NgxPhase::Rewrite,
            NgxPhase::Access,
            NgxPhase::Content,
            NgxPhase::Timer,
            NgxPhase::SslCertificate,
            NgxPhase::SslSessionFetch,
        ] {
            assert!(
                host_allowed_in_phase(name, phase),
                "{name} must be allowed in {phase}"
            );
        }
        assert!(!host_allowed_in_phase(name, NgxPhase::Init));
        assert!(!host_allowed_in_phase(name, NgxPhase::Set));
        assert!(!host_allowed_in_phase(name, NgxPhase::Log));
    }
    assert_eq!(
        manifest_entry(NGX_SOCKET_TCP_NAME).unwrap().sync,
        HostSyncKind::Sync
    );
    assert_eq!(
        manifest_entry(NGX_SOCKET_TCP_CLOSE_NAME).unwrap().sync,
        HostSyncKind::Sync
    );
    assert_eq!(
        manifest_entry(NGX_SOCKET_TCP_SETTIMEOUT_NAME).unwrap().sync,
        HostSyncKind::Sync
    );
    assert_eq!(
        manifest_entry(NGX_SOCKET_TCP_CONNECT_NAME).unwrap().sync,
        HostSyncKind::Async
    );
    assert_eq!(
        manifest_entry(NGX_SOCKET_CONNECT_NAME).unwrap().sync,
        HostSyncKind::Async
    );
    assert_eq!(
        manifest_entry(NGX_SOCKET_TCP_SEND_NAME).unwrap().sync,
        HostSyncKind::Async
    );
    assert_eq!(
        manifest_entry(NGX_SOCKET_TCP_RECEIVE_NAME).unwrap().sync,
        HostSyncKind::Async
    );
    assert_eq!(
        manifest_entry(NGX_SOCKET_TCP_RECEIVEANY_NAME).unwrap().sync,
        HostSyncKind::Async
    );
    assert_eq!(
        manifest_entry(NGX_SOCKET_TCP_RECEIVEUNTIL_NAME)
            .unwrap()
            .sync,
        HostSyncKind::Async
    );
    assert_eq!(
        manifest_entry(NGX_SOCKET_CONNECT_NAME)
            .unwrap()
            .openresty_name,
        "ngx.socket.connect"
    );
}

#[test]
fn production_host_count_includes_tcp_pool_and_tls_hosts() {
    let runtime: BTreeSet<&str> = production_runtime_host_names().iter().copied().collect();
    assert_eq!(
        production_runtime_host_names().len(),
        199,
        "production host inventory must stay in sync"
    );
    assert_eq!(ngx_host_catalog().functions().len(), 199);
    assert_eq!(ngx_rustscript::host::ngx_host_manifest().len(), 199);
    for name in [
        NGX_SOCKET_TCP_NAME,
        NGX_SOCKET_TCP_CONNECT_NAME,
        NGX_SOCKET_CONNECT_NAME,
        NGX_SOCKET_TCP_SEND_NAME,
        NGX_SOCKET_TCP_RECEIVE_NAME,
        NGX_SOCKET_TCP_RECEIVEANY_NAME,
        NGX_SOCKET_TCP_RECEIVEUNTIL_NAME,
        NGX_SOCKET_TCP_CLOSE_NAME,
        NGX_SOCKET_TCP_SETTIMEOUT_NAME,
    ] {
        assert!(runtime.contains(name), "missing {name}");
    }
}

#[test]
fn resource_type_mismatch_is_rejected_at_compile() {
    let err = compile_err("use ngx;\nngx::socket::tcp::close(1);\n");
    let text = err.to_string().to_ascii_lowercase();
    assert!(
        matches!(err, ConfigError::Compile(_)),
        "int is not a tcp resource, got {err:?}"
    );
    assert!(
        text.contains("ngx::socket::tcp::close")
            && (text.contains("type") || text.contains("match") || text.contains("resource")),
        "type mismatch should name the host, got {err}"
    );
}

#[test]
fn udp_handle_is_not_a_tcp_socket() {
    let err =
        compile_err("use ngx;\nlet sock = ngx::socket::udp();\nngx::socket::tcp::close(sock);\n");
    let text = err.to_string().to_ascii_lowercase();
    assert!(
        matches!(err, ConfigError::Compile(_)),
        "udp resource must not satisfy tcp close, got {err:?}"
    );
    assert!(
        text.contains("type") || text.contains("match") || text.contains("resource"),
        "udp/tcp mismatch must be a type error, got {err}"
    );
    assert!(
        compile_rss_source("use ngx;\nlet sock = ngx::socket::udp();\n").is_ok(),
        "udp constructor remains valid"
    );
    let _ = NGX_SOCKET_UDP_NAME;
}

#[test]
fn constructor_and_close_are_synchronous() {
    let status =
        run_sync("use ngx;\nlet sock = ngx::socket::tcp();\nngx::socket::tcp::close(sock);\n")
            .expect("create and close");
    assert_eq!(status, VmStatus::Halted);
}

#[test]
fn double_close_is_a_host_error() {
    let err = run_sync(
        "use ngx;\nlet sock = ngx::socket::tcp();\nngx::socket::tcp::close(sock);\nngx::socket::tcp::close(sock);\n",
    )
    .expect_err("second close must fail");
    let text = err.to_ascii_lowercase();
    assert!(
        text.contains("close") || text.contains("closed") || text.contains("resource"),
        "double close should mention the closed resource, got {err}"
    );
}

#[test]
fn closed_handle_is_not_reused_by_a_new_socket() {
    let err = run_sync(
        "use ngx;\nlet sock = ngx::socket::tcp();\nngx::socket::tcp::close(sock);\nlet other = ngx::socket::tcp();\nngx::socket::tcp::close(sock);\nngx::socket::tcp::close(other);\n",
    )
    .expect_err("stale handle must not close the replacement socket");
    let text = err.to_ascii_lowercase();
    assert!(
        text.contains("close")
            || text.contains("closed")
            || text.contains("stale")
            || text.contains("resource"),
        "stale handle reuse must fail, got {err}"
    );
}

#[test]
fn ipv4_echo_round_trip() {
    let (listener, port) = bind_listener("127.0.0.1:0");
    let echo = spawn_echo(listener);
    let status = pump(
        &format!(
            "use ngx;\nlet sock = ngx::socket::tcp();\nngx::socket::tcp::settimeout(sock, 1000);\nngx::socket::tcp::connect(sock, \"127.0.0.1\", {port});\nngx::socket::tcp::send(sock, \"ping\");\nngx::socket::tcp::receive(sock, 4);\nngx::socket::tcp::close(sock);\n"
        ),
        Duration::from_secs(3),
    );
    echo.join().expect("echo thread");
    assert_eq!(status, AccessStatus::Ok);
}

#[test]
fn convenience_connect_echo_round_trip() {
    let (listener, port) = bind_listener("127.0.0.1:0");
    let echo = spawn_echo(listener);
    let status = pump(
        &format!(
            "use ngx;\nlet sock = ngx::socket::connect(\"127.0.0.1\", {port});\nngx::socket::tcp::settimeout(sock, 1000);\nngx::socket::tcp::send(sock, \"ping\");\nngx::socket::tcp::receive(sock, 4);\nngx::socket::tcp::close(sock);\n"
        ),
        Duration::from_secs(3),
    );
    echo.join().expect("echo thread");
    assert_eq!(status, AccessStatus::Ok);
}

#[test]
fn dns_connect_echo_round_trip() {
    let (listener, port) = bind_listener("127.0.0.1:0");
    let echo = spawn_echo(listener);
    let status = pump(
        &format!(
            "use ngx;\nlet sock = ngx::socket::tcp();\nngx::socket::tcp::settimeout(sock, 1000);\nngx::socket::tcp::connect(sock, \"localhost\", {port});\nngx::socket::tcp::send(sock, \"ping\");\nngx::socket::tcp::receive(sock, 4);\nngx::socket::tcp::close(sock);\n"
        ),
        Duration::from_secs(3),
    );
    echo.join().expect("echo thread");
    assert_eq!(status, AccessStatus::Ok);
}

#[test]
fn ipv6_echo_round_trip() {
    let (listener, port) = bind_listener("[::1]:0");
    let echo = spawn_echo(listener);
    let status = pump(
        &format!(
            "use ngx;\nlet sock = ngx::socket::tcp();\nngx::socket::tcp::settimeout(sock, 1000);\nngx::socket::tcp::connect(sock, \"::1\", {port});\nngx::socket::tcp::send(sock, \"ping\");\nngx::socket::tcp::receive(sock, 4);\nngx::socket::tcp::close(sock);\n"
        ),
        Duration::from_secs(3),
    );
    echo.join().expect("echo thread");
    assert_eq!(status, AccessStatus::Ok);
}

#[test]
fn unix_socket_echo_round_trip() {
    let path = tmp_unix_path();
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path).expect("unix bind");
    let path_text = path.to_string_lossy().replace('\\', "\\\\");
    let echo = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("unix accept");
        stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
        let mut buf = [0u8; 16];
        let n = stream.read(&mut buf).expect("unix recv");
        stream.write_all(&buf[..n]).expect("unix send");
    });
    let status = pump(
        &format!(
            "use ngx;\nlet sock = ngx::socket::tcp();\nngx::socket::tcp::settimeout(sock, 1000);\nngx::socket::tcp::connect(sock, \"unix:{path_text}\", 0);\nngx::socket::tcp::send(sock, \"ping\");\nngx::socket::tcp::receive(sock, 4);\nngx::socket::tcp::close(sock);\n"
        ),
        Duration::from_secs(3),
    );
    echo.join().expect("unix echo");
    let _ = std::fs::remove_file(&path);
    assert_eq!(status, AccessStatus::Ok);
}

#[test]
fn receive_exact_then_unread_remainder() {
    let (listener, port) = bind_listener("127.0.0.1:0");
    let peer = spawn_scripted(listener, |mut stream| {
        stream.write_all(b"abcdef").expect("write");
        let mut first = [0u8; 3];
        stream.read_exact(&mut first).expect("first");
        assert_eq!(&first, b"abc");
        let mut second = [0u8; 3];
        stream.read_exact(&mut second).expect("second");
        assert_eq!(&second, b"def");
    });
    let status = pump(
        &format!(
            "use ngx;\nlet sock = ngx::socket::tcp();\nngx::socket::tcp::settimeout(sock, 1000);\nngx::socket::tcp::connect(sock, \"127.0.0.1\", {port});\nlet a = ngx::socket::tcp::receive(sock, 3);\nngx::socket::tcp::send(sock, a);\nlet b = ngx::socket::tcp::receive(sock, 3);\nngx::socket::tcp::send(sock, b);\nngx::socket::tcp::close(sock);\n"
        ),
        Duration::from_secs(3),
    );
    peer.join().expect("peer");
    assert_eq!(status, AccessStatus::Ok);
}

#[test]
fn receive_line_strips_crlf() {
    let (listener, port) = bind_listener("127.0.0.1:0");
    let peer = spawn_scripted(listener, |mut stream| {
        stream.write_all(b"hello\r\nworld\n").expect("write");
        let mut first = [0u8; 5];
        stream.read_exact(&mut first).expect("first");
        assert_eq!(&first, b"hello");
        let mut second = [0u8; 5];
        stream.read_exact(&mut second).expect("second");
        assert_eq!(&second, b"world");
    });
    let status = pump(
        &format!(
            "use ngx;\nlet sock = ngx::socket::tcp();\nngx::socket::tcp::settimeout(sock, 1000);\nngx::socket::tcp::connect(sock, \"127.0.0.1\", {port});\nlet a = ngx::socket::tcp::receive(sock, ());\nngx::socket::tcp::send(sock, a);\nlet b = ngx::socket::tcp::receive(sock, \"*l\");\nngx::socket::tcp::send(sock, b);\nngx::socket::tcp::close(sock);\n"
        ),
        Duration::from_secs(3),
    );
    peer.join().expect("peer");
    assert_eq!(status, AccessStatus::Ok);
}

#[test]
fn receive_all_succeeds_on_peer_close() {
    let (listener, port) = bind_listener("127.0.0.1:0");
    let peer = spawn_scripted(listener, |mut stream| {
        stream.write_all(b"done").expect("write");
        drop(stream);
    });
    let status = pump(
        &format!(
            "use ngx;\nlet sock = ngx::socket::tcp();\nngx::socket::tcp::settimeout(sock, 1000);\nngx::socket::tcp::connect(sock, \"127.0.0.1\", {port});\nngx::socket::tcp::receive(sock, \"*a\");\nngx::socket::tcp::close(sock);\n"
        ),
        Duration::from_secs(3),
    );
    peer.join().expect("peer");
    assert_eq!(status, AccessStatus::Ok);
}

#[test]
fn receiveany_preserves_unread_bytes() {
    let (listener, port) = bind_listener("127.0.0.1:0");
    let peer = spawn_scripted(listener, |mut stream| {
        stream.write_all(b"hello").expect("write");
        let mut first = [0u8; 2];
        stream.read_exact(&mut first).expect("first");
        assert_eq!(&first, b"he");
        let mut second = [0u8; 3];
        stream.read_exact(&mut second).expect("second");
        assert_eq!(&second, b"llo");
    });
    let status = pump(
        &format!(
            "use ngx;\nlet sock = ngx::socket::tcp();\nngx::socket::tcp::settimeout(sock, 1000);\nngx::socket::tcp::connect(sock, \"127.0.0.1\", {port});\nlet a = ngx::socket::tcp::receiveany(sock, 2);\nngx::socket::tcp::send(sock, a);\nlet b = ngx::socket::tcp::receiveany(sock, 16);\nngx::socket::tcp::send(sock, b);\nngx::socket::tcp::close(sock);\n"
        ),
        Duration::from_secs(3),
    );
    peer.join().expect("peer");
    assert_eq!(status, AccessStatus::Ok);
}

#[test]
fn receiveuntil_delimiter_overlap_across_fragments() {
    let (listener, port) = bind_listener("127.0.0.1:0");
    let peer = spawn_scripted(listener, |mut stream| {
        stream.write_all(b"xxab").expect("first fragment");
        thread::sleep(Duration::from_millis(40));
        stream.write_all(b"cYYY").expect("second fragment");
        let mut first = [0u8; 2];
        stream.read_exact(&mut first).expect("until payload");
        assert_eq!(&first, b"xx");
        let mut rest = [0u8; 3];
        stream.read_exact(&mut rest).expect("unread tail");
        assert_eq!(&rest, b"YYY");
    });
    let status = pump(
        &format!(
            "use ngx;\nlet sock = ngx::socket::tcp();\nngx::socket::tcp::settimeout(sock, 1000);\nngx::socket::tcp::connect(sock, \"127.0.0.1\", {port});\nlet a = ngx::socket::tcp::receiveuntil(sock, \"abc\");\nngx::socket::tcp::send(sock, a);\nlet b = ngx::socket::tcp::receiveany(sock, 16);\nngx::socket::tcp::send(sock, b);\nngx::socket::tcp::close(sock);\n"
        ),
        Duration::from_secs(3),
    );
    peer.join().expect("peer");
    assert_eq!(status, AccessStatus::Ok);
}

#[test]
fn partial_send_loop_delivers_the_full_payload() {
    let (listener, port) = bind_listener("127.0.0.1:0");
    let payload = "x".repeat(32_768);
    let expected = payload.clone();
    let peer = spawn_scripted(listener, move |mut stream| {
        let mut got = Vec::new();
        let mut buf = [0u8; 4096];
        while got.len() < expected.len() {
            let n = stream.read(&mut buf).expect("read payload");
            assert!(n > 0, "peer closed before full payload");
            got.extend_from_slice(&buf[..n]);
        }
        assert_eq!(got, expected.as_bytes());
        stream.write_all(b"ok").expect("ack");
    });
    let status = pump(
        &format!(
            "use ngx;\nlet sock = ngx::socket::tcp();\nngx::socket::tcp::settimeout(sock, 2000);\nngx::socket::tcp::connect(sock, \"127.0.0.1\", {port});\nngx::socket::tcp::send(sock, \"{payload}\");\nngx::socket::tcp::receive(sock, 2);\nngx::socket::tcp::close(sock);\n"
        ),
        Duration::from_secs(5),
    );
    peer.join().expect("peer");
    assert_eq!(status, AccessStatus::Ok);
}

#[test]
fn peer_close_before_exact_size_fails() {
    let (listener, port) = bind_listener("127.0.0.1:0");
    let peer = spawn_scripted(listener, |mut stream| {
        stream.write_all(b"xy").expect("short write");
        stream.shutdown(std::net::Shutdown::Write).ok();
        let _ = stream.read(&mut [0u8; 1]);
    });
    let status = pump(
        &format!(
            "use ngx;\nlet sock = ngx::socket::tcp();\nngx::socket::tcp::settimeout(sock, 1000);\nngx::socket::tcp::connect(sock, \"127.0.0.1\", {port});\nngx::socket::tcp::receive(sock, 8);\n"
        ),
        Duration::from_secs(3),
    );
    peer.join().expect("peer");
    assert_eq!(status, AccessStatus::Error500);
}

#[test]
fn receive_timeout_fails_the_request() {
    let (listener, port) = bind_listener("127.0.0.1:0");
    let peer = spawn_scripted(listener, |stream| {
        thread::sleep(Duration::from_millis(400));
        drop(stream);
    });
    let started = Instant::now();
    let status = pump(
        &format!(
            "use ngx;\nlet sock = ngx::socket::tcp();\nngx::socket::tcp::settimeout(sock, 50);\nngx::socket::tcp::connect(sock, \"127.0.0.1\", {port});\nngx::socket::tcp::receive(sock, 4);\n"
        ),
        Duration::from_secs(3),
    );
    let elapsed = started.elapsed();
    peer.join().expect("peer");
    assert_eq!(status, AccessStatus::Error500);
    assert!(
        elapsed >= Duration::from_millis(20),
        "timeout should wait, elapsed {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_millis(1500),
        "timeout must not hang, elapsed {elapsed:?}"
    );
}

#[test]
fn request_cleanup_cancels_a_pending_receive() {
    let (listener, port) = bind_listener("127.0.0.1:0");
    let _peer = spawn_scripted(listener, |stream| {
        thread::sleep(Duration::from_secs(2));
        drop(stream);
    });
    let mut driver = Driver::new(compile(&format!(
        "use ngx;\nlet sock = ngx::socket::tcp();\nngx::socket::tcp::settimeout(sock, 60000);\nngx::socket::tcp::connect(sock, \"127.0.0.1\", {port});\nngx::socket::tcp::receive(sock, 4);\n"
    )));
    let started = Instant::now();
    loop {
        match driver.handle() {
            AccessStatus::Again => {
                if matches!(driver.exec.vm_status(), Some(VmStatus::Waiting(_)))
                    && started.elapsed() > Duration::from_millis(20)
                {
                    break;
                }
            }
            other => panic!("expected wait for receive, got {other:?}"),
        }
        if started.elapsed() > Duration::from_secs(2) {
            panic!("never reached a pending receive");
        }
    }
    assert_eq!(driver.worker.in_flight().unwrap(), 1);
    driver.cleanup_pending();
}

#[test]
fn request_cleanup_cancels_a_timeout_zero_pending_receive() {
    let (listener, port) = bind_listener("127.0.0.1:0");
    let _peer = spawn_scripted(listener, |stream| {
        thread::sleep(Duration::from_secs(2));
        drop(stream);
    });
    let mut driver = Driver::new(compile(&format!(
        "use ngx;\nlet sock = ngx::socket::tcp();\nngx::socket::tcp::settimeout(sock, 0);\nngx::socket::tcp::connect(sock, \"127.0.0.1\", {port});\nngx::socket::tcp::receive(sock, 4);\n"
    )));
    let started = Instant::now();
    loop {
        match driver.handle() {
            AccessStatus::Again => {
                if matches!(driver.exec.vm_status(), Some(VmStatus::Waiting(_)))
                    && started.elapsed() > Duration::from_millis(20)
                {
                    break;
                }
            }
            other => panic!("expected wait for receive, got {other:?}"),
        }
        if started.elapsed() > Duration::from_secs(2) {
            panic!("never reached a pending receive");
        }
    }
    assert_eq!(driver.worker.in_flight().unwrap(), 1);
    driver.cleanup_pending();
}

#[test]
fn worker_shutdown_drops_a_pending_receive() {
    let (listener, port) = bind_listener("127.0.0.1:0");
    let _peer = spawn_scripted(listener, |stream| {
        thread::sleep(Duration::from_secs(2));
        drop(stream);
    });
    let mut worker = WorkerRuntime::start().unwrap();
    let mut exec = worker
        .bind_access_exec(
            compile(&format!(
                "use ngx;\nlet sock = ngx::socket::tcp();\nngx::socket::tcp::settimeout(sock, 60000);\nngx::socket::tcp::connect(sock, \"127.0.0.1\", {port});\nngx::socket::tcp::receive(sock, 4);\n"
            )),
            1,
        )
        .unwrap();
    let mut registry = IdentityRegistry::with_capacity(DEFAULT_MAX_IN_FLIGHT);
    let started = Instant::now();
    loop {
        let status = exec.handle_access(&mut registry, &worker);
        if status == AccessStatus::Again {
            exec.commit_suspend();
            if matches!(exec.vm_status(), Some(VmStatus::Waiting(_)))
                && started.elapsed() > Duration::from_millis(20)
            {
                break;
            }
        } else {
            panic!("expected wait, got {status:?}");
        }
        if started.elapsed() > Duration::from_secs(2) {
            panic!("never reached a pending receive");
        }
    }
    assert_eq!(worker.in_flight().unwrap(), 1);
    worker.shutdown();
    assert_eq!(worker.in_flight().unwrap(), 0);
}

#[test]
fn send_and_receive_caps_are_rejected() {
    let status = pump(
        "use ngx;\nlet sock = ngx::socket::tcp();\nngx::socket::tcp::receive(sock, 1048577);\n",
        Duration::from_secs(3),
    );
    assert_eq!(status, AccessStatus::Error500);
    let status = pump(
        "use ngx;\nlet sock = ngx::socket::tcp();\nngx::socket::tcp::receiveany(sock, 1048577);\n",
        Duration::from_secs(3),
    );
    assert_eq!(status, AccessStatus::Error500);
    let delim = "d".repeat(257);
    let status = pump(
        &format!(
            "use ngx;\nlet sock = ngx::socket::tcp();\nngx::socket::tcp::receiveuntil(sock, \"{delim}\");\n"
        ),
        Duration::from_secs(3),
    );
    assert_eq!(status, AccessStatus::Error500);
}

#[test]
fn dns_host_over_the_length_cap_is_rejected() {
    // Pay the lazy catalog initialization before the clock starts: this test
    // measures the connect path, not the first compile in the process.
    let _ = ngx_host_catalog();
    let host = "a".repeat(256);
    let program = compile(&format!(
        "use ngx;\nlet sock = ngx::socket::tcp();\nngx::socket::tcp::settimeout(sock, 1000);\nngx::socket::tcp::connect(sock, \"{host}\", 80);\n"
    ));
    // The worker runtime start is harness cost, not connect latency: build it
    // before the clock so the bound measures the rejected lookup only.
    let driver = Driver::new(program);
    let started = Instant::now();
    let status = driver.pump_until(Duration::from_secs(3));
    let elapsed = started.elapsed();
    assert_eq!(status, AccessStatus::Error500);
    assert!(
        elapsed < Duration::from_millis(1500),
        "oversized DNS host must be rejected before lookup, elapsed {elapsed:?}"
    );
}

#[test]
fn timeout_zero_does_not_fire_a_timer() {
    let (listener, port) = bind_listener("127.0.0.1:0");
    let peer = spawn_scripted(listener, |mut stream| {
        thread::sleep(Duration::from_millis(120));
        stream.write_all(b"pong").expect("late write");
        let _ = stream.read(&mut [0u8; 1]);
    });
    let started = Instant::now();
    let status = pump(
        &format!(
            "use ngx;\nlet sock = ngx::socket::tcp();\nngx::socket::tcp::settimeout(sock, 0);\nngx::socket::tcp::connect(sock, \"127.0.0.1\", {port});\nngx::socket::tcp::receive(sock, 4);\nngx::socket::tcp::close(sock);\n"
        ),
        Duration::from_secs(3),
    );
    peer.join().expect("peer");
    let elapsed = started.elapsed();
    assert_eq!(status, AccessStatus::Ok);
    assert!(
        elapsed >= Duration::from_millis(80),
        "timeout 0 must wait for the peer, elapsed {elapsed:?}"
    );
}
