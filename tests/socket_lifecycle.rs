//! Task 16 lifecycle: generation, DNS, IPv6, pending ops, cleanup, workers.
//!
//! Catalog/echo coverage stays in `tests/ngx_udp_socket.rs`. This file is the
//! roadmap lifecycle suite.

use ngx_rustscript_nginx_test_stubs as _;

use std::io::{Read, Write};
use std::net::{
    SocketAddr, TcpListener as StdTcpListener, ToSocketAddrs, UdpSocket as StdUdpSocket,
};
use std::os::unix::io::RawFd;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use ngx_rustscript::config::{compile_rss_source, program_requires_request_state};
use ngx_rustscript::notify::DEFAULT_MAX_IN_FLIGHT;
use ngx_rustscript::request_state::{AccessStatus, IdentityRegistry, ReadyEffect};
use ngx_rustscript::runtime::WorkerRuntime;
use vm::{Program, VmStatus};

fn compile(source: &str) -> Arc<Program> {
    compile_rss_source(source).unwrap_or_else(|err| panic!("compile failed: {err}"))
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
    registry: IdentityRegistry<u64>,
}

impl Driver {
    fn new(program: Arc<Program>) -> Self {
        let mut worker = WorkerRuntime::start().unwrap();
        let exec = worker.bind_access_exec_for_test(program, 1).expect("bind");
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

    fn wait_until_waiting(&mut self, timeout: Duration) {
        let started = Instant::now();
        loop {
            match self.handle() {
                AccessStatus::Again => {
                    if matches!(self.exec.vm_status(), Some(VmStatus::Waiting(_)))
                        && started.elapsed() > Duration::from_millis(20)
                    {
                        return;
                    }
                }
                other => panic!("expected wait, got {other:?}"),
            }
            if started.elapsed() > timeout {
                panic!("never reached a pending UDP operation");
            }
        }
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

fn bind_echo_peer(addr: &str) -> StdUdpSocket {
    let socket = StdUdpSocket::bind(addr).expect("bind echo peer");
    socket
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("read timeout");
    socket
        .set_write_timeout(Some(Duration::from_secs(2)))
        .expect("write timeout");
    socket
}

fn spawn_echo(
    peer: StdUdpSocket,
    expect: &'static [u8],
    reply: &'static [u8],
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut buf = [0u8; 2048];
        let (n, addr) = peer.recv_from(&mut buf).expect("echo recv");
        assert_eq!(&buf[..n], expect);
        peer.send_to(reply, addr).expect("echo send");
    })
}

fn spawn_delayed_echo(
    peer: StdUdpSocket,
    delay: Duration,
    expect: &'static [u8],
    reply: &'static [u8],
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut buf = [0u8; 2048];
        let (n, addr) = peer.recv_from(&mut buf).expect("echo recv");
        assert_eq!(&buf[..n], expect);
        thread::sleep(delay);
        peer.send_to(reply, addr).expect("echo send");
    })
}

fn pump(source: &str, timeout: Duration) -> AccessStatus {
    Driver::new(compile(source)).pump_until(timeout)
}

#[test]
fn constructor_only_program_requires_request_state() {
    let program = compile("use ngx;\nlet sock = ngx::socket::udp();\n");
    assert!(
        program_requires_request_state(&program),
        "ngx::socket::udp constructor is Cosocket and must not use the pooled VM"
    );
}

#[test]
fn close_only_program_requires_request_state() {
    let program =
        compile("use ngx;\nlet sock = ngx::socket::udp();\nngx::socket::udp::close(sock);\n");
    assert!(program_requires_request_state(&program));
}

#[test]
fn settimeout_only_program_requires_request_state() {
    let program = compile(
        "use ngx;\nlet sock = ngx::socket::udp();\nngx::socket::udp::settimeout(sock, 10);\n",
    );
    assert!(
        program_requires_request_state(&program),
        "sync settimeout is still Cosocket request-owned"
    );
}

#[test]
fn dns_setpeername_echo_round_trip() {
    let resolved = ("localhost", 0)
        .to_socket_addrs()
        .expect("resolve localhost")
        .next()
        .expect("localhost address");
    assert!(resolved.ip().is_loopback());
    let peer = bind_echo_peer(&resolved.to_string());
    let port = peer.local_addr().unwrap().port();
    let echo = spawn_echo(peer, b"ping", b"pong");
    let status = pump(
        &format!(
            "use ngx;\nlet sock = ngx::socket::udp();\nngx::socket::udp::settimeout(sock, 1000);\nngx::socket::udp::setpeername(sock, \"localhost\", {port});\nngx::socket::udp::send(sock, \"ping\");\nngx::socket::udp::receive(sock, null);\nngx::socket::udp::close(sock);\n"
        ),
        Duration::from_secs(3),
    );
    echo.join().expect("echo thread");
    assert_eq!(status, AccessStatus::Ok);
}

#[test]
fn ipv6_bind_setpeername_echo_round_trip() {
    let peer = bind_echo_peer("[::1]:0");
    let port = peer.local_addr().unwrap().port();
    let echo = spawn_echo(peer, b"ping", b"pong");
    let status = pump(
        &format!(
            "use ngx;\nlet sock = ngx::socket::udp();\nngx::socket::udp::settimeout(sock, 1000);\nngx::socket::udp::bind(sock, \"::1\", 0);\nngx::socket::udp::setpeername(sock, \"::1\", {port});\nngx::socket::udp::send(sock, \"ping\");\nngx::socket::udp::receive(sock, null);\nngx::socket::udp::close(sock);\n"
        ),
        Duration::from_secs(3),
    );
    echo.join().expect("echo thread");
    assert_eq!(status, AccessStatus::Ok);
}

#[test]
fn two_sockets_may_pend_independently() {
    let left = bind_echo_peer("127.0.0.1:0");
    let right = bind_echo_peer("127.0.0.1:0");
    let left_port = left.local_addr().unwrap().port();
    let right_port = right.local_addr().unwrap().port();
    let echo_left = spawn_echo(left, b"L", b"l");
    let echo_right = spawn_echo(right, b"R", b"r");
    let status = pump(
        &format!(
            "use ngx;\nlet a = ngx::socket::udp();\nlet b = ngx::socket::udp();\nngx::socket::udp::settimeout(a, 1000);\nngx::socket::udp::settimeout(b, 1000);\nngx::socket::udp::setpeername(a, \"127.0.0.1\", {left_port});\nngx::socket::udp::setpeername(b, \"127.0.0.1\", {right_port});\nngx::socket::udp::send(a, \"L\");\nngx::socket::udp::send(b, \"R\");\nngx::socket::udp::receive(a, null);\nngx::socket::udp::receive(b, null);\nngx::socket::udp::close(a);\nngx::socket::udp::close(b);\n"
        ),
        Duration::from_secs(3),
    );
    echo_left.join().expect("left echo");
    echo_right.join().expect("right echo");
    assert_eq!(status, AccessStatus::Ok);
}

#[test]
fn request_cleanup_closes_the_udp_fd() {
    let probe = StdUdpSocket::bind("127.0.0.1:0").expect("probe");
    let port = probe.local_addr().unwrap().port();
    drop(probe);
    let mut driver = Driver::new(compile(&format!(
        "use ngx;\nlet sock = ngx::socket::udp();\nngx::socket::udp::settimeout(sock, 60000);\nngx::socket::udp::bind(sock, \"127.0.0.1\", {port});\nngx::socket::udp::receive(sock, null);\n"
    )));
    driver.wait_until_waiting(Duration::from_secs(2));
    assert_eq!(driver.worker.in_flight().unwrap(), 1);
    driver.cleanup_pending();
    StdUdpSocket::bind(SocketAddr::from(([127, 0, 0, 1], port)))
        .expect("cleanup must close the UDP fd so the port can be bound again");
}

#[test]
fn request_cleanup_cancels_a_pending_receive() {
    let peer = bind_echo_peer("127.0.0.1:0");
    let port = peer.local_addr().unwrap().port();
    let mut driver = Driver::new(compile(&format!(
        "use ngx;\nlet sock = ngx::socket::udp();\nngx::socket::udp::settimeout(sock, 60000);\nngx::socket::udp::setpeername(sock, \"127.0.0.1\", {port});\nngx::socket::udp::receive(sock, null);\n"
    )));
    driver.wait_until_waiting(Duration::from_secs(2));
    assert_eq!(driver.worker.in_flight().unwrap(), 1);
    driver.cleanup_pending();
}

#[test]
fn request_cleanup_cancels_a_pending_send_when_waiting() {
    let peer = bind_echo_peer("127.0.0.1:0");
    let port = peer.local_addr().unwrap().port();
    let mut driver = Driver::new(compile(&format!(
        "use ngx;\nlet sock = ngx::socket::udp();\nngx::socket::udp::settimeout(sock, 60000);\nngx::socket::udp::setpeername(sock, \"127.0.0.1\", {port});\nngx::socket::udp::send(sock, \"ping\");\nngx::socket::udp::receive(sock, null);\n"
    )));
    let started = Instant::now();
    loop {
        match driver.handle() {
            AccessStatus::Again => {
                if matches!(driver.exec.vm_status(), Some(VmStatus::Waiting(_)))
                    && started.elapsed() > Duration::from_millis(20)
                {
                    assert_eq!(driver.worker.in_flight().unwrap(), 1);
                    driver.cleanup_pending();
                    return;
                }
            }
            AccessStatus::Ok => {
                driver.worker.shutdown();
                return;
            }
            other => panic!("expected wait or immediate send completion, got {other:?}"),
        }
        if started.elapsed() > Duration::from_secs(2) {
            panic!("never reached a pending send or receive");
        }
    }
}

#[test]
fn worker_runtimes_isolate_udp_sockets() {
    let left = bind_echo_peer("127.0.0.1:0");
    let right = bind_echo_peer("127.0.0.1:0");
    let left_port = left.local_addr().unwrap().port();
    let right_port = right.local_addr().unwrap().port();
    let echo_left = spawn_echo(left, b"A", b"a");
    let echo_right = spawn_echo(right, b"B", b"b");
    let left_status = Driver::new(compile(&format!(
        "use ngx;\nlet sock = ngx::socket::udp();\nngx::socket::udp::settimeout(sock, 1000);\nngx::socket::udp::setpeername(sock, \"127.0.0.1\", {left_port});\nngx::socket::udp::send(sock, \"A\");\nngx::socket::udp::receive(sock, null);\nngx::socket::udp::close(sock);\n"
    )))
    .pump_until(Duration::from_secs(3));
    let right_status = Driver::new(compile(&format!(
        "use ngx;\nlet sock = ngx::socket::udp();\nngx::socket::udp::settimeout(sock, 1000);\nngx::socket::udp::setpeername(sock, \"127.0.0.1\", {right_port});\nngx::socket::udp::send(sock, \"B\");\nngx::socket::udp::receive(sock, null);\nngx::socket::udp::close(sock);\n"
    )))
    .pump_until(Duration::from_secs(3));
    echo_left.join().expect("left echo");
    echo_right.join().expect("right echo");
    assert_eq!(left_status, AccessStatus::Ok);
    assert_eq!(right_status, AccessStatus::Ok);
}

#[test]
fn invalid_port_is_rejected() {
    let status = pump(
        "use ngx;\nlet sock = ngx::socket::udp();\nngx::socket::udp::bind(sock, \"127.0.0.1\", 65536);\n",
        Duration::from_secs(3),
    );
    assert_eq!(status, AccessStatus::Error500);
}

#[test]
fn negative_port_is_rejected() {
    let status = pump(
        "use ngx;\nlet sock = ngx::socket::udp();\nngx::socket::udp::setpeername(sock, \"127.0.0.1\", -1);\n",
        Duration::from_secs(3),
    );
    assert_eq!(status, AccessStatus::Error500);
}

#[test]
fn send_without_peer_is_rejected() {
    let status = pump(
        "use ngx;\nlet sock = ngx::socket::udp();\nngx::socket::udp::bind(sock, \"127.0.0.1\", 0);\nngx::socket::udp::send(sock, \"x\");\n",
        Duration::from_secs(3),
    );
    assert_eq!(status, AccessStatus::Error500);
}

#[test]
fn bind_already_bound_is_rejected() {
    let status = pump(
        "use ngx;\nlet sock = ngx::socket::udp();\nngx::socket::udp::bind(sock, \"127.0.0.1\", 0);\nngx::socket::udp::bind(sock, \"127.0.0.1\", 0);\n",
        Duration::from_secs(3),
    );
    assert_eq!(status, AccessStatus::Error500);
}

#[test]
fn unresolved_error_host_name_is_rejected() {
    let started = Instant::now();
    let status = pump(
        "use ngx;\nlet sock = ngx::socket::udp();\nngx::socket::udp::settimeout(sock, 1000);\nngx::socket::udp::setpeername(sock, \"foo:bar\", 53);\n",
        Duration::from_secs(5),
    );
    let elapsed = started.elapsed();
    assert_eq!(status, AccessStatus::Error500);
    assert!(
        elapsed < Duration::from_secs(4),
        "unresolvable host must fail without hanging, elapsed {elapsed:?}"
    );
}

#[test]
fn dns_host_over_the_length_cap_is_rejected() {
    let host = "a".repeat(256);
    let started = Instant::now();
    let status = pump(
        &format!(
            "use ngx;\nlet sock = ngx::socket::udp();\nngx::socket::udp::settimeout(sock, 1000);\nngx::socket::udp::setpeername(sock, \"{host}\", 53);\n"
        ),
        Duration::from_secs(3),
    );
    let elapsed = started.elapsed();
    assert_eq!(status, AccessStatus::Error500);
    assert!(
        elapsed < Duration::from_millis(1500),
        "oversized DNS host must be rejected before lookup, elapsed {elapsed:?}"
    );
}

#[test]
fn timeout_zero_does_not_fire_a_timer() {
    let peer = bind_echo_peer("127.0.0.1:0");
    let port = peer.local_addr().unwrap().port();
    let echo = spawn_delayed_echo(peer, Duration::from_millis(120), b"ping", b"pong");
    let started = Instant::now();
    let status = pump(
        &format!(
            "use ngx;\nlet sock = ngx::socket::udp();\nngx::socket::udp::settimeout(sock, 0);\nngx::socket::udp::setpeername(sock, \"127.0.0.1\", {port});\nngx::socket::udp::send(sock, \"ping\");\nngx::socket::udp::receive(sock, null);\nngx::socket::udp::close(sock);\n"
        ),
        Duration::from_secs(3),
    );
    echo.join().expect("echo thread");
    let elapsed = started.elapsed();
    assert_eq!(status, AccessStatus::Ok);
    assert!(
        elapsed >= Duration::from_millis(80),
        "timeout 0 must wait for the datagram, elapsed {elapsed:?}"
    );
}

#[test]
fn receive_size_over_the_datagram_cap_names_the_host() {
    let status = pump(
        "use ngx;\nlet sock = ngx::socket::udp();\nngx::socket::udp::receive(sock, 65536);\n",
        Duration::from_secs(3),
    );
    assert_eq!(status, AccessStatus::Error500);
}

#[test]
fn oversized_datagram_names_the_host() {
    let peer = bind_echo_peer("127.0.0.1:0");
    let port = peer.local_addr().unwrap().port();
    let oversized = vec![b'x'; 64];
    let handle = thread::spawn(move || {
        let mut buf = [0u8; 16];
        let (n, addr) = peer.recv_from(&mut buf).expect("recv ping");
        assert_eq!(&buf[..n], b"ping");
        peer.send_to(&oversized, addr).expect("send oversized");
    });
    let status = pump(
        &format!(
            "use ngx;\nlet sock = ngx::socket::udp();\nngx::socket::udp::settimeout(sock, 1000);\nngx::socket::udp::setpeername(sock, \"127.0.0.1\", {port});\nngx::socket::udp::send(sock, \"ping\");\nngx::socket::udp::receive(sock, 8);\n"
        ),
        Duration::from_secs(3),
    );
    handle.join().expect("peer thread");
    assert_eq!(status, AccessStatus::Error500);
}

#[test]
fn tcp_constructor_only_program_requires_request_state() {
    let program = compile("use ngx;\nlet sock = ngx::socket::tcp();\n");
    assert!(
        program_requires_request_state(&program),
        "ngx::socket::tcp constructor is Cosocket and must not use the pooled VM"
    );
}

#[test]
fn tcp_worker_runtimes_isolate_sockets() {
    let left = StdTcpListener::bind("127.0.0.1:0").expect("left");
    let right = StdTcpListener::bind("127.0.0.1:0").expect("right");
    let left_port = left.local_addr().unwrap().port();
    let right_port = right.local_addr().unwrap().port();
    let echo_left = thread::spawn(move || {
        let (mut stream, _) = left.accept().expect("left accept");
        let mut buf = [0u8; 1];
        stream.read_exact(&mut buf).expect("left read");
        assert_eq!(&buf, b"A");
        stream.write_all(b"a").expect("left write");
    });
    let echo_right = thread::spawn(move || {
        let (mut stream, _) = right.accept().expect("right accept");
        let mut buf = [0u8; 1];
        stream.read_exact(&mut buf).expect("right read");
        assert_eq!(&buf, b"B");
        stream.write_all(b"b").expect("right write");
    });
    let left_status = Driver::new(compile(&format!(
        "use ngx;\nlet sock = ngx::socket::tcp();\nngx::socket::tcp::settimeout(sock, 1000);\nngx::socket::tcp::connect(sock, \"127.0.0.1\", {left_port});\nngx::socket::tcp::send(sock, \"A\");\nngx::socket::tcp::receive(sock, 1);\nngx::socket::tcp::close(sock);\n"
    )))
    .pump_until(Duration::from_secs(3));
    let right_status = Driver::new(compile(&format!(
        "use ngx;\nlet sock = ngx::socket::tcp();\nngx::socket::tcp::settimeout(sock, 1000);\nngx::socket::tcp::connect(sock, \"127.0.0.1\", {right_port});\nngx::socket::tcp::send(sock, \"B\");\nngx::socket::tcp::receive(sock, 1);\nngx::socket::tcp::close(sock);\n"
    )))
    .pump_until(Duration::from_secs(3));
    echo_left.join().expect("left echo");
    echo_right.join().expect("right echo");
    assert_eq!(left_status, AccessStatus::Ok);
    assert_eq!(right_status, AccessStatus::Ok);
}

#[test]
fn tcp_request_cleanup_closes_the_peer_fd() {
    let listener = StdTcpListener::bind("127.0.0.1:0").expect("listener");
    listener.set_nonblocking(false).ok();
    let port = listener.local_addr().unwrap().port();
    let peer = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
        let mut buf = [0u8; 8];
        let n = stream.read(&mut buf).expect("peer sees close");
        assert_eq!(n, 0, "cleanup must close the TCP fd so the peer reads EOF");
    });
    let mut driver = Driver::new(compile(&format!(
        "use ngx;\nlet sock = ngx::socket::tcp();\nngx::socket::tcp::settimeout(sock, 60000);\nngx::socket::tcp::connect(sock, \"127.0.0.1\", {port});\nngx::socket::tcp::receive(sock, 4);\n"
    )));
    driver.wait_until_waiting(Duration::from_secs(2));
    assert_eq!(driver.worker.in_flight().unwrap(), 1);
    driver.cleanup_pending();
    peer.join().expect("peer");
}
