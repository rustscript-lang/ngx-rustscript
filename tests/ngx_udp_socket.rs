//! Task 16: typed UDP cosocket resource ABI and Tokio lifecycle.
//!
//! Covers catalog/resource typing, double close, timeout, cancel, shutdown,
//! handle reuse after close, UDP echo, and oversized datagrams.

use ngx_rustscript_nginx_test_stubs as _;

use std::net::UdpSocket as StdUdpSocket;
use std::os::unix::io::RawFd;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use ngx_rustscript::config::{ConfigError, compile_rss_source};
use ngx_rustscript::host::{
    HostCapability, HostFamily, HostSyncKind, NGX_SOCKET_UDP_BIND_NAME, NGX_SOCKET_UDP_CLOSE_NAME,
    NGX_SOCKET_UDP_NAME, NGX_SOCKET_UDP_RECEIVE_NAME, NGX_SOCKET_UDP_RESOURCE_KEY,
    NGX_SOCKET_UDP_SEND_NAME, NGX_SOCKET_UDP_SETPEERNAME_NAME, NGX_SOCKET_UDP_SETTIMEOUT_NAME,
    NgxPhase, host_allowed_in_phase, manifest_entry, ngx_host_catalog,
    production_runtime_host_names,
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
                panic!("timed out waiting for UDP host completion");
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

fn udp_key() -> ResourceTypeKey {
    ResourceTypeKey::new(NGX_SOCKET_UDP_RESOURCE_KEY).expect("valid udp resource key")
}

fn bind_echo_peer() -> StdUdpSocket {
    let socket = StdUdpSocket::bind("127.0.0.1:0").expect("bind echo peer");
    socket
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("read timeout");
    socket
        .set_write_timeout(Some(Duration::from_secs(2)))
        .expect("write timeout");
    socket
}

fn spawn_echo(peer: StdUdpSocket, reply: &'static [u8]) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut buf = [0u8; 2048];
        let (n, addr) = peer.recv_from(&mut buf).expect("echo recv");
        assert_eq!(&buf[..n], b"ping");
        peer.send_to(reply, addr).expect("echo send");
    })
}

#[test]
fn catalog_declares_typed_udp_resource_and_hosts() {
    let catalog = ngx_host_catalog();
    let key = udp_key();
    let resource = catalog
        .resource(NGX_SOCKET_UDP_RESOURCE_KEY)
        .expect("udp resource type must be catalogued");
    assert_eq!(resource.key, key);

    let ctor = catalog
        .function(NGX_SOCKET_UDP_NAME)
        .expect("udp constructor");
    assert!(ctor.params.is_empty());
    assert_eq!(ctor.return_type, HostTypeSchema::Resource(key.clone()));

    for (name, extra) in [
        (NGX_SOCKET_UDP_BIND_NAME, 2usize),
        (NGX_SOCKET_UDP_SETPEERNAME_NAME, 2),
        (NGX_SOCKET_UDP_SEND_NAME, 1),
        (NGX_SOCKET_UDP_RECEIVE_NAME, 1),
        (NGX_SOCKET_UDP_CLOSE_NAME, 0),
        (NGX_SOCKET_UDP_SETTIMEOUT_NAME, 1),
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
}

#[test]
fn udp_hosts_are_cosocket_family_in_openresty_phases() {
    for name in [
        NGX_SOCKET_UDP_NAME,
        NGX_SOCKET_UDP_BIND_NAME,
        NGX_SOCKET_UDP_SETPEERNAME_NAME,
        NGX_SOCKET_UDP_SEND_NAME,
        NGX_SOCKET_UDP_RECEIVE_NAME,
        NGX_SOCKET_UDP_CLOSE_NAME,
        NGX_SOCKET_UDP_SETTIMEOUT_NAME,
    ] {
        let entry = manifest_entry(name).unwrap_or_else(|| panic!("manifest {name}"));
        assert_eq!(entry.family, HostFamily::Cosocket);
        assert_eq!(entry.capabilities, &[HostCapability::Cosocket]);
        assert_eq!(entry.openresty_name, "ngx.socket.udp");
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
        manifest_entry(NGX_SOCKET_UDP_NAME).unwrap().sync,
        HostSyncKind::Sync
    );
    assert_eq!(
        manifest_entry(NGX_SOCKET_UDP_CLOSE_NAME).unwrap().sync,
        HostSyncKind::Sync
    );
    assert_eq!(
        manifest_entry(NGX_SOCKET_UDP_SETTIMEOUT_NAME).unwrap().sync,
        HostSyncKind::Sync
    );
    assert_eq!(
        manifest_entry(NGX_SOCKET_UDP_BIND_NAME).unwrap().sync,
        HostSyncKind::Async
    );
    assert_eq!(
        manifest_entry(NGX_SOCKET_UDP_SETPEERNAME_NAME)
            .unwrap()
            .sync,
        HostSyncKind::Async
    );
    assert_eq!(
        manifest_entry(NGX_SOCKET_UDP_SEND_NAME).unwrap().sync,
        HostSyncKind::Async
    );
    assert_eq!(
        manifest_entry(NGX_SOCKET_UDP_RECEIVE_NAME).unwrap().sync,
        HostSyncKind::Async
    );
}

#[test]
fn resource_type_mismatch_is_rejected_at_compile() {
    let err = compile_err("use ngx;\nngx::socket::udp::close(1);\n");
    let text = err.to_string().to_ascii_lowercase();
    assert!(
        matches!(err, ConfigError::Compile(_)),
        "int is not a udp resource, got {err:?}"
    );
    assert!(
        text.contains("ngx::socket::udp::close")
            && (text.contains("type") || text.contains("match") || text.contains("resource")),
        "type mismatch should name the host, got {err}"
    );
}

#[test]
fn constructor_and_close_are_synchronous() {
    let status =
        run_sync("use ngx;\nlet sock = ngx::socket::udp();\nngx::socket::udp::close(sock);\n")
            .expect("create and close");
    assert_eq!(status, VmStatus::Halted);
}

#[test]
fn double_close_is_a_host_error() {
    let err = run_sync(
        "use ngx;\nlet sock = ngx::socket::udp();\nngx::socket::udp::close(sock);\nngx::socket::udp::close(sock);\n",
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
        "use ngx;\nlet sock = ngx::socket::udp();\nngx::socket::udp::close(sock);\nlet other = ngx::socket::udp();\nngx::socket::udp::close(sock);\nngx::socket::udp::close(other);\n",
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
fn receive_size_over_the_datagram_cap_is_rejected() {
    let peer = bind_echo_peer();
    let port = peer.local_addr().unwrap().port();
    let status = Driver::new(compile(&format!(
        "use ngx;\nlet sock = ngx::socket::udp();\nngx::socket::udp::settimeout(sock, 1000);\nngx::socket::udp::setpeername(sock, \"127.0.0.1\", {port});\nngx::socket::udp::receive(sock, 65536);\n"
    )))
    .pump_until(Duration::from_secs(3));
    assert_eq!(status, AccessStatus::Error500);
}

#[test]
fn oversized_datagram_is_rejected() {
    let peer = bind_echo_peer();
    let port = peer.local_addr().unwrap().port();
    let oversized = vec![b'x'; 64];
    let handle = thread::spawn(move || {
        let mut buf = [0u8; 16];
        let (n, addr) = peer.recv_from(&mut buf).expect("recv ping");
        assert_eq!(&buf[..n], b"ping");
        peer.send_to(&oversized, addr).expect("send oversized");
    });
    let status = Driver::new(compile(&format!(
        "use ngx;\nlet sock = ngx::socket::udp();\nngx::socket::udp::settimeout(sock, 1000);\nngx::socket::udp::setpeername(sock, \"127.0.0.1\", {port});\nngx::socket::udp::send(sock, \"ping\");\nngx::socket::udp::receive(sock, 8);\n"
    )))
    .pump_until(Duration::from_secs(3));
    handle.join().expect("peer thread");
    assert_eq!(status, AccessStatus::Error500);
}

#[test]
fn udp_echo_round_trip() {
    let peer = bind_echo_peer();
    let port = peer.local_addr().unwrap().port();
    let echo = spawn_echo(peer, b"pong");
    let status = Driver::new(compile(&format!(
        "use ngx;\nlet sock = ngx::socket::udp();\nngx::socket::udp::settimeout(sock, 1000);\nngx::socket::udp::setpeername(sock, \"127.0.0.1\", {port});\nngx::socket::udp::send(sock, \"ping\");\nngx::socket::udp::receive(sock, null);\nngx::socket::udp::close(sock);\n"
    )))
    .pump_until(Duration::from_secs(3));
    echo.join().expect("echo thread");
    assert_eq!(status, AccessStatus::Ok);
}

#[test]
fn receive_timeout_fails_the_request() {
    let peer = bind_echo_peer();
    let port = peer.local_addr().unwrap().port();
    let started = Instant::now();
    let status = Driver::new(compile(&format!(
        "use ngx;\nlet sock = ngx::socket::udp();\nngx::socket::udp::settimeout(sock, 50);\nngx::socket::udp::setpeername(sock, \"127.0.0.1\", {port});\nngx::socket::udp::receive(sock, null);\n"
    )))
    .pump_until(Duration::from_secs(3));
    let elapsed = started.elapsed();
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
    let peer = bind_echo_peer();
    let port = peer.local_addr().unwrap().port();
    let mut driver = Driver::new(compile(&format!(
        "use ngx;\nlet sock = ngx::socket::udp();\nngx::socket::udp::settimeout(sock, 60000);\nngx::socket::udp::setpeername(sock, \"127.0.0.1\", {port});\nngx::socket::udp::receive(sock, null);\n"
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
    let peer = bind_echo_peer();
    let port = peer.local_addr().unwrap().port();
    let mut worker = WorkerRuntime::start().unwrap();
    let mut exec = worker
        .bind_access_exec(
            compile(&format!(
                "use ngx;\nlet sock = ngx::socket::udp();\nngx::socket::udp::settimeout(sock, 60000);\nngx::socket::udp::setpeername(sock, \"127.0.0.1\", {port});\nngx::socket::udp::receive(sock, null);\n"
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
fn bind_then_echo_round_trip() {
    let peer = bind_echo_peer();
    let port = peer.local_addr().unwrap().port();
    let echo = spawn_echo(peer, b"pong");
    let status = Driver::new(compile(&format!(
        "use ngx;\nlet sock = ngx::socket::udp();\nngx::socket::udp::settimeout(sock, 1000);\nngx::socket::udp::bind(sock, \"127.0.0.1\", 0);\nngx::socket::udp::setpeername(sock, \"127.0.0.1\", {port});\nngx::socket::udp::send(sock, \"ping\");\nngx::socket::udp::receive(sock, null);\nngx::socket::udp::close(sock);\n"
    )))
    .pump_until(Duration::from_secs(3));
    echo.join().expect("echo thread");
    assert_eq!(status, AccessStatus::Ok);
}
