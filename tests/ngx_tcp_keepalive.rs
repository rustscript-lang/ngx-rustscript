//! Task 18: TCP keepalive pool — catalog, reuse, isolation, and lifecycle.

use ngx_rustscript_nginx_test_stubs as _;

use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::net::{TcpListener as StdTcpListener, TcpStream as StdTcpStream};
use std::os::unix::io::RawFd;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use ngx_rustscript::config::compile_rss_source;
use ngx_rustscript::host::{
    HostCapability, HostFamily, HostSyncKind, NGX_SOCKET_TCP_RESOURCE_KEY, NgxPhase,
    host_allowed_in_phase, manifest_entry, ngx_host_catalog, production_runtime_host_names,
};
use ngx_rustscript::notify::DEFAULT_MAX_IN_FLIGHT;
use ngx_rustscript::request_state::{AccessStatus, IdentityRegistry, ReadyEffect};
use ngx_rustscript::runtime::WorkerRuntime;
use vm::{HostParamPassing, HostTypeSchema, Program, ResourceTypeKey};

const SETKEEPALIVE: &str = "ngx::socket::tcp::setkeepalive";
const GETREUSEDTIMES: &str = "ngx::socket::tcp::getreusedtimes";

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

struct Session {
    worker: WorkerRuntime,
    next_id: u64,
}

impl Session {
    fn new() -> Self {
        Self {
            worker: WorkerRuntime::start().unwrap(),
            next_id: 0,
        }
    }

    fn pump(&mut self, source: &str, timeout: Duration) -> AccessStatus {
        self.next_id += 1;
        let mut exec = self
            .worker
            .bind_access_exec(compile(source), self.next_id)
            .expect("bind");
        let mut registry = IdentityRegistry::with_capacity(DEFAULT_MAX_IN_FLIGHT);
        let deadline = Instant::now() + timeout;
        let mut status = exec.handle_access(&mut registry, &self.worker);
        if status == AccessStatus::Again {
            exec.commit_suspend();
        }
        while status == AccessStatus::Again && Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let fd = self.worker.raw_fd().expect("eventfd");
            if !wait_readable(fd, remaining) {
                panic!("timed out waiting for TCP host completion");
            }
            let _ = self.worker.drain_ready().unwrap();
            match exec.apply_ready(exec.identity(), &mut registry, &self.worker) {
                ReadyEffect::Halted => {
                    status = AccessStatus::Ok;
                    break;
                }
                ReadyEffect::Failed => {
                    status = AccessStatus::Error500;
                    break;
                }
                ReadyEffect::WaitingAgain => {
                    exec.commit_suspend();
                    status = AccessStatus::Again;
                }
                ReadyEffect::Deferred | ReadyEffect::Ignored => {
                    status = exec.handle_access(&mut registry, &self.worker);
                    if status == AccessStatus::Again {
                        exec.commit_suspend();
                    }
                }
            }
        }
        status
    }

    fn cleanup_after_keepalive(&mut self, source: &str, timeout: Duration) -> AccessStatus {
        let status = self.pump(source, timeout);
        assert_eq!(self.worker.in_flight().unwrap(), 0);
        status
    }

    fn shutdown(mut self) {
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

fn echo_once(stream: &mut StdTcpStream, expect: &[u8], reply: &[u8]) {
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let mut buf = vec![0u8; expect.len().max(16)];
    let n = stream.read(&mut buf).expect("read");
    assert_eq!(&buf[..n], expect);
    stream.write_all(reply).unwrap();
}

fn script_connect_send_keepalive(
    port: u16,
    payload: &str,
    reused: i64,
    timeout_ms: i64,
    pool: i64,
) -> String {
    format!(
        "use ngx;
let sock = ngx::socket::tcp();
ngx::socket::tcp::settimeout(sock, 1000);
ngx::socket::tcp::connect(sock, \"127.0.0.1\", {port});
let n = ngx::socket::tcp::getreusedtimes(sock);
if n != {reused} {{
ngx::exit(500);
}}
ngx::socket::tcp::send(sock, \"{payload}\");
ngx::socket::tcp::receive(sock, {len});
ngx::socket::tcp::setkeepalive(sock, {timeout_ms}, {pool});
",
        len = payload.len()
    )
}

fn script_connect_send_close(port: u16, payload: &str, reused: i64) -> String {
    format!(
        "use ngx;
let sock = ngx::socket::tcp();
ngx::socket::tcp::settimeout(sock, 1000);
ngx::socket::tcp::connect(sock, \"127.0.0.1\", {port});
let n = ngx::socket::tcp::getreusedtimes(sock);
if n != {reused} {{
ngx::exit(500);
}}
ngx::socket::tcp::send(sock, \"{payload}\");
ngx::socket::tcp::receive(sock, {len});
ngx::socket::tcp::close(sock);
",
        len = payload.len()
    )
}

#[test]
fn catalog_declares_keepalive_hosts() {
    let catalog = ngx_host_catalog();
    let key = tcp_key();

    let keepalive = catalog
        .function(SETKEEPALIVE)
        .expect("ngx::socket::tcp::setkeepalive");
    assert_eq!(keepalive.params.len(), 3);
    assert_eq!(
        keepalive.params[0].ty,
        HostTypeSchema::Resource(key.clone())
    );
    assert_ne!(keepalive.params[0].passing, HostParamPassing::Value);
    assert_eq!(keepalive.params[1].name, "timeout");
    assert_eq!(keepalive.params[1].ty, HostTypeSchema::Unknown);
    assert_eq!(keepalive.params[2].name, "pool_size");
    assert_eq!(keepalive.params[2].ty, HostTypeSchema::Unknown);
    assert_eq!(keepalive.return_type, HostTypeSchema::Null);

    let reused = catalog
        .function(GETREUSEDTIMES)
        .expect("ngx::socket::tcp::getreusedtimes");
    assert_eq!(reused.params.len(), 1);
    assert_eq!(reused.params[0].ty, HostTypeSchema::Resource(key));
    assert_eq!(reused.return_type, HostTypeSchema::Int);

    assert!(production_runtime_host_names().contains(&SETKEEPALIVE));
    assert!(production_runtime_host_names().contains(&GETREUSEDTIMES));
}

#[test]
fn keepalive_hosts_are_cosocket_family_in_openresty_phases() {
    for name in [SETKEEPALIVE, GETREUSEDTIMES] {
        let entry = manifest_entry(name).unwrap_or_else(|| panic!("manifest {name}"));
        assert_eq!(entry.family, HostFamily::Cosocket);
        assert_eq!(entry.capabilities, &[HostCapability::Cosocket]);
        assert_eq!(entry.sync, HostSyncKind::Sync);
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
        assert!(!host_allowed_in_phase(name, NgxPhase::Log));
    }
    assert_eq!(
        manifest_entry(SETKEEPALIVE).unwrap().openresty_name,
        "ngx.socket.tcp"
    );
    assert_eq!(
        manifest_entry(GETREUSEDTIMES).unwrap().openresty_name,
        "ngx.socket.tcp"
    );
}

#[test]
fn production_host_count_adds_keepalive_and_tls_hosts() {
    let runtime: BTreeSet<&str> = production_runtime_host_names().iter().copied().collect();
    assert_eq!(
        production_runtime_host_names().len(),
        195,
        "production host inventory must stay in sync"
    );
    assert_eq!(ngx_host_catalog().functions().len(), 195);
    assert_eq!(ngx_rustscript::host::ngx_host_manifest().len(), 195);
    for name in [
        SETKEEPALIVE,
        GETREUSEDTIMES,
        "ngx::socket::tcp::sslhandshake",
        "ngx::socket::tcp::settrustedstore",
        "ngx::socket::tcp::setclientcert",
        "ngx::socket::tcp::trustedstore",
        "ngx::socket::tcp::clientcert",
    ] {
        assert!(runtime.contains(name), "missing {name}");
    }
}

#[test]
fn setkeepalive_then_connect_reuses_the_connection() {
    let (listener, port) = bind_listener("127.0.0.1:0");
    let accepts = Arc::new(AtomicU64::new(0));
    let seen = Arc::clone(&accepts);
    let peer = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept once");
        seen.fetch_add(1, Ordering::SeqCst);
        echo_once(&mut stream, b"one", b"ack");
        echo_once(&mut stream, b"two", b"ack");
        let _ = stream.read(&mut [0u8; 1]);
    });

    let mut session = Session::new();
    assert_eq!(
        session.pump(
            &script_connect_send_keepalive(port, "one", 0, 60_000, 8),
            Duration::from_secs(3)
        ),
        AccessStatus::Ok
    );
    assert_eq!(
        session.pump(
            &script_connect_send_close(port, "two", 1),
            Duration::from_secs(3)
        ),
        AccessStatus::Ok
    );
    session.shutdown();
    peer.join().expect("peer");
    assert_eq!(
        accepts.load(Ordering::SeqCst),
        1,
        "pool hit must reuse the fd"
    );
}

#[test]
fn pool_miss_when_empty_opens_a_new_connection() {
    let (listener, port) = bind_listener("127.0.0.1:0");
    let accepts = Arc::new(AtomicU64::new(0));
    let seen = Arc::clone(&accepts);
    let peer = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        seen.fetch_add(1, Ordering::SeqCst);
        echo_once(&mut stream, b"miss", b"ack!");
        let _ = stream.read(&mut [0u8; 1]);
    });
    let mut session = Session::new();
    assert_eq!(
        session.pump(
            &script_connect_send_close(port, "miss", 0),
            Duration::from_secs(3)
        ),
        AccessStatus::Ok
    );
    session.shutdown();
    peer.join().expect("peer");
    assert_eq!(accepts.load(Ordering::SeqCst), 1);
}

#[test]
fn capacity_evicts_lru_connection() {
    let (listener, port) = bind_listener("127.0.0.1:0");
    let accepts = Arc::new(AtomicU64::new(0));
    let seen = Arc::clone(&accepts);
    let peer = thread::spawn(move || {
        let (mut first, _) = listener.accept().expect("first");
        seen.fetch_add(1, Ordering::SeqCst);
        echo_once(&mut first, b"aaa", b"ack");
        let (mut second, _) = listener.accept().expect("second");
        seen.fetch_add(1, Ordering::SeqCst);
        echo_once(&mut second, b"bbb", b"ack");
        echo_once(&mut first, b"ccc", b"ack");
        second
            .set_read_timeout(Some(Duration::from_millis(400)))
            .unwrap();
        let mut buf = [0u8; 1];
        let _ = second.read(&mut buf);
        let _ = first.read(&mut buf);
    });

    let mut session = Session::new();
    let source = format!(
        "use ngx;
let a = ngx::socket::tcp();
ngx::socket::tcp::settimeout(a, 1000);
ngx::socket::tcp::connect(a, \"127.0.0.1\", {port});
ngx::socket::tcp::send(a, \"aaa\");
ngx::socket::tcp::receive(a, 3);
let b = ngx::socket::tcp();
ngx::socket::tcp::settimeout(b, 1000);
ngx::socket::tcp::connect(b, \"127.0.0.1\", {port});
ngx::socket::tcp::send(b, \"bbb\");
ngx::socket::tcp::receive(b, 3);
ngx::socket::tcp::setkeepalive(b, 60000, 1);
ngx::socket::tcp::setkeepalive(a, 60000, 1);
let c = ngx::socket::tcp();
ngx::socket::tcp::settimeout(c, 1000);
ngx::socket::tcp::connect(c, \"127.0.0.1\", {port});
let n = ngx::socket::tcp::getreusedtimes(c);
if n != 1 {{
ngx::exit(500);
}}
ngx::socket::tcp::send(c, \"ccc\");
ngx::socket::tcp::receive(c, 3);
ngx::socket::tcp::close(c);
"
    );
    assert_eq!(
        session.pump(&source, Duration::from_secs(3)),
        AccessStatus::Ok
    );
    session.shutdown();
    peer.join().expect("peer");
    assert_eq!(accepts.load(Ordering::SeqCst), 2);
}

#[test]
fn idle_timeout_expires_pooled_connection() {
    let (listener, port) = bind_listener("127.0.0.1:0");
    let accepts = Arc::new(AtomicU64::new(0));
    let seen = Arc::clone(&accepts);
    let peer = thread::spawn(move || {
        let (mut first, _) = listener.accept().expect("first");
        seen.fetch_add(1, Ordering::SeqCst);
        echo_once(&mut first, b"old", b"ack");
        first
            .set_read_timeout(Some(Duration::from_millis(800)))
            .unwrap();
        let _ = first.read(&mut [0u8; 1]);
        let (mut second, _) = listener.accept().expect("expired miss");
        seen.fetch_add(1, Ordering::SeqCst);
        echo_once(&mut second, b"new", b"ack");
        let _ = second.read(&mut [0u8; 1]);
    });

    let mut session = Session::new();
    assert_eq!(
        session.pump(
            &script_connect_send_keepalive(port, "old", 0, 50, 8),
            Duration::from_secs(3)
        ),
        AccessStatus::Ok
    );
    thread::sleep(Duration::from_millis(200));
    assert_eq!(
        session.pump(
            &script_connect_send_close(port, "new", 0),
            Duration::from_secs(3)
        ),
        AccessStatus::Ok
    );
    session.shutdown();
    peer.join().expect("peer");
    assert_eq!(accepts.load(Ordering::SeqCst), 2);
}

#[test]
fn endpoint_keys_do_not_share_pooled_sockets() {
    let (listener_a, port_a) = bind_listener("127.0.0.1:0");
    let (listener_b, port_b) = bind_listener("127.0.0.1:0");
    let peer_a = thread::spawn(move || {
        let (mut stream, _) = listener_a.accept().expect("a");
        echo_once(&mut stream, b"aaa", b"ack");
        let _ = stream.read(&mut [0u8; 1]);
    });
    let peer_b = thread::spawn(move || {
        let (mut stream, _) = listener_b.accept().expect("b");
        echo_once(&mut stream, b"bbb", b"ack");
        let _ = stream.read(&mut [0u8; 1]);
    });

    let mut session = Session::new();
    assert_eq!(
        session.pump(
            &script_connect_send_keepalive(port_a, "aaa", 0, 60_000, 8),
            Duration::from_secs(3)
        ),
        AccessStatus::Ok
    );
    assert_eq!(
        session.pump(
            &script_connect_send_close(port_b, "bbb", 0),
            Duration::from_secs(3)
        ),
        AccessStatus::Ok
    );
    session.shutdown();
    peer_a.join().expect("a");
    peer_b.join().expect("b");
}

#[test]
fn dirty_unread_and_eof_sockets_are_not_pooled() {
    let (listener, port) = bind_listener("127.0.0.1:0");
    let peer = thread::spawn(move || {
        let (mut dirty, _) = listener.accept().expect("dirty");
        dirty
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        dirty
            .set_write_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut buf = [0u8; 8];
        let n = dirty.read(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"dty");
        dirty.write_all(b"XY").unwrap();
        let (mut eof, _) = listener.accept().expect("eof");
        eof.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let n = eof.read(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"eof");
        eof.write_all(b"ack").unwrap();
        drop(eof);
    });

    let mut session = Session::new();
    let dirty = session.pump(
        &format!(
            "use ngx;
let sock = ngx::socket::tcp();
ngx::socket::tcp::settimeout(sock, 1000);
ngx::socket::tcp::connect(sock, \"127.0.0.1\", {port});
ngx::socket::tcp::send(sock, \"dty\");
ngx::socket::tcp::receive(sock, 1);
ngx::socket::tcp::setkeepalive(sock, 60000, 8);
"
        ),
        Duration::from_secs(3),
    );
    assert_eq!(dirty, AccessStatus::Error500);

    let eof = session.pump(
        &format!(
            "use ngx;
let sock = ngx::socket::tcp();
ngx::socket::tcp::settimeout(sock, 1000);
ngx::socket::tcp::connect(sock, \"127.0.0.1\", {port});
ngx::socket::tcp::send(sock, \"eof\");
ngx::socket::tcp::receive(sock, 3);
ngx::sleep_ms(80);
ngx::socket::tcp::setkeepalive(sock, 60000, 8);
"
        ),
        Duration::from_secs(3),
    );
    assert_eq!(eof, AccessStatus::Error500);
    session.shutdown();
    peer.join().expect("peer");
}

#[test]
fn old_request_cleanup_cannot_close_pooled_connection() {
    let (listener, port) = bind_listener("127.0.0.1:0");
    let accepts = Arc::new(AtomicU64::new(0));
    let seen = Arc::clone(&accepts);
    let peer = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept once");
        seen.fetch_add(1, Ordering::SeqCst);
        echo_once(&mut stream, b"keep", b"ack!");
        echo_once(&mut stream, b"next", b"ack!");
        let _ = stream.read(&mut [0u8; 1]);
    });

    let mut session = Session::new();
    assert_eq!(
        session.cleanup_after_keepalive(
            &script_connect_send_keepalive(port, "keep", 0, 60_000, 8),
            Duration::from_secs(3)
        ),
        AccessStatus::Ok
    );
    assert_eq!(
        session.pump(
            &script_connect_send_close(port, "next", 1),
            Duration::from_secs(3)
        ),
        AccessStatus::Ok
    );
    session.shutdown();
    peer.join().expect("peer");
    assert_eq!(accepts.load(Ordering::SeqCst), 1);
}

#[test]
fn worker_isolation_and_shutdown_drop_pooled_sockets() {
    let (listener, port) = bind_listener("127.0.0.1:0");
    let accepts = Arc::new(AtomicU64::new(0));
    let seen = Arc::clone(&accepts);
    let peer = thread::spawn(move || {
        let (mut first, _) = listener.accept().expect("first worker");
        seen.fetch_add(1, Ordering::SeqCst);
        echo_once(&mut first, b"w1a", b"ack");
        first
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let _ = first.read(&mut [0u8; 1]);
        let (mut second, _) = listener.accept().expect("after shutdown");
        seen.fetch_add(1, Ordering::SeqCst);
        echo_once(&mut second, b"w2a", b"ack");
        let _ = second.read(&mut [0u8; 1]);
    });

    let mut session = Session::new();
    assert_eq!(
        session.pump(
            &script_connect_send_keepalive(port, "w1a", 0, 60_000, 8),
            Duration::from_secs(3)
        ),
        AccessStatus::Ok
    );
    session.shutdown();

    let mut session = Session::new();
    assert_eq!(
        session.pump(
            &script_connect_send_close(port, "w2a", 0),
            Duration::from_secs(3)
        ),
        AccessStatus::Ok
    );
    session.shutdown();
    peer.join().expect("peer");
    assert_eq!(accepts.load(Ordering::SeqCst), 2);
}

#[test]
fn concurrent_checkout_from_one_request_reuses_once() {
    let (listener, port) = bind_listener("127.0.0.1:0");
    let accepts = Arc::new(AtomicU64::new(0));
    let seen = Arc::clone(&accepts);
    let peer = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("pooled");
        seen.fetch_add(1, Ordering::SeqCst);
        echo_once(&mut stream, b"put", b"ack");
        echo_once(&mut stream, b"get", b"ack");
        let _ = stream.read(&mut [0u8; 1]);
    });

    let mut session = Session::new();
    let source = format!(
        "use ngx;
let a = ngx::socket::tcp();
ngx::socket::tcp::settimeout(a, 1000);
ngx::socket::tcp::connect(a, \"127.0.0.1\", {port});
ngx::socket::tcp::send(a, \"put\");
ngx::socket::tcp::receive(a, 3);
ngx::socket::tcp::setkeepalive(a, 60000, 8);
let b = ngx::socket::tcp();
ngx::socket::tcp::settimeout(b, 1000);
ngx::socket::tcp::connect(b, \"127.0.0.1\", {port});
let n = ngx::socket::tcp::getreusedtimes(b);
if n != 1 {{
ngx::exit(500);
}}
ngx::socket::tcp::send(b, \"get\");
ngx::socket::tcp::receive(b, 3);
ngx::socket::tcp::close(b);
"
    );
    assert_eq!(
        session.pump(&source, Duration::from_secs(3)),
        AccessStatus::Ok
    );
    session.shutdown();
    peer.join().expect("peer");
    assert_eq!(accepts.load(Ordering::SeqCst), 1);
}

#[test]
fn worker_threads_do_not_share_the_pool() {
    let (listener, port) = bind_listener("127.0.0.1:0");
    let accepts = Arc::new(AtomicU64::new(0));
    let seen = Arc::clone(&accepts);
    let peer = thread::spawn(move || {
        let (mut first, _) = listener.accept().expect("t1");
        seen.fetch_add(1, Ordering::SeqCst);
        echo_once(&mut first, b"t1x", b"ack");
        let (mut second, _) = listener.accept().expect("t2");
        seen.fetch_add(1, Ordering::SeqCst);
        echo_once(&mut second, b"t2x", b"ack");
        first
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let _ = first.read(&mut [0u8; 1]);
        let _ = second.read(&mut [0u8; 1]);
    });

    let port_thread = port;
    let other = thread::spawn(move || {
        let mut session = Session::new();
        let status = session.pump(
            &script_connect_send_keepalive(port_thread, "t1x", 0, 60_000, 8),
            Duration::from_secs(3),
        );
        session.shutdown();
        status
    });
    assert_eq!(other.join().expect("thread"), AccessStatus::Ok);

    let mut session = Session::new();
    assert_eq!(
        session.pump(
            &script_connect_send_close(port, "t2x", 0),
            Duration::from_secs(3)
        ),
        AccessStatus::Ok
    );
    session.shutdown();
    peer.join().expect("peer");
    assert_eq!(accepts.load(Ordering::SeqCst), 2);
}
