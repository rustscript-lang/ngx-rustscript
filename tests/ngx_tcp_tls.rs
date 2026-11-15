//! Task 18: Tokio/rustls TLS cosocket — catalog, verification, handshake, reuse.

use ngx_rustscript_nginx_test_stubs as _;

use std::io::{Read, Write};
use std::net::TcpListener as StdTcpListener;
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
use vm::{HostParamPassing, HostTypeSchema, Program, ResourceTypeKey, VmStatus};

const SSLHANDSHAKE: &str = "ngx::socket::tcp::sslhandshake";
const SETTRUSTEDSTORE: &str = "ngx::socket::tcp::settrustedstore";
const SETCLIENTCERT: &str = "ngx::socket::tcp::setclientcert";

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
                panic!("timed out waiting for TLS host completion");
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

    fn shutdown(mut self) {
        self.worker.shutdown();
    }
}

fn rss_string(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

struct TlsFixtures {
    ca_pem: String,
    other_ca_pem: String,
    cert_pem: String,
    key_pem: String,
    client_cert_pem: String,
    client_key_pem: String,
    other_client_key_pem: String,
}

fn generate_fixtures() -> TlsFixtures {
    let mut ca_params = rcgen::CertificateParams::default();
    ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    ca_params.key_usages = vec![
        rcgen::KeyUsagePurpose::KeyCertSign,
        rcgen::KeyUsagePurpose::CrlSign,
    ];
    let mut dn = rcgen::DistinguishedName::new();
    dn.push(rcgen::DnType::CommonName, "task18-test-ca");
    ca_params.distinguished_name = dn;
    let ca_key = rcgen::KeyPair::generate().expect("ca key");
    let ca = ca_params.self_signed(&ca_key).expect("ca cert");

    let mut other_params = rcgen::CertificateParams::default();
    other_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    let mut other_dn = rcgen::DistinguishedName::new();
    other_dn.push(rcgen::DnType::CommonName, "task18-other-ca");
    other_params.distinguished_name = other_dn;
    let other_key = rcgen::KeyPair::generate().expect("other ca key");
    let other_ca = other_params.self_signed(&other_key).expect("other ca");

    let mut server_params = rcgen::CertificateParams::new(vec!["localhost".into()]).expect("sans");
    let mut server_dn = rcgen::DistinguishedName::new();
    server_dn.push(rcgen::DnType::CommonName, "localhost");
    server_params.distinguished_name = server_dn;
    let server_key = rcgen::KeyPair::generate().expect("server key");
    let server = server_params
        .signed_by(&server_key, &ca, &ca_key)
        .expect("server cert");

    let mut client_params = rcgen::CertificateParams::new(Vec::<String>::new()).expect("client");
    client_params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ClientAuth];
    let mut client_dn = rcgen::DistinguishedName::new();
    client_dn.push(rcgen::DnType::CommonName, "task18-client");
    client_params.distinguished_name = client_dn;
    let client_key = rcgen::KeyPair::generate().expect("client key");
    let client = client_params
        .signed_by(&client_key, &ca, &ca_key)
        .expect("client cert");
    let other_client_key = rcgen::KeyPair::generate().expect("other client key");

    TlsFixtures {
        ca_pem: ca.pem(),
        other_ca_pem: other_ca.pem(),
        cert_pem: server.pem(),
        key_pem: server_key.serialize_pem(),
        client_cert_pem: client.pem(),
        client_key_pem: client_key.serialize_pem(),
        other_client_key_pem: other_client_key.serialize_pem(),
    }
}

fn bind_listener() -> (StdTcpListener, u16) {
    let listener = StdTcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    (listener, port)
}

fn spawn_tls_echo(
    listener: StdTcpListener,
    cert_pem: String,
    key_pem: String,
    n_msgs: usize,
    accepts: Arc<AtomicU64>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let certs: Vec<_> = rustls_pemfile::certs(&mut cert_pem.as_bytes())
            .collect::<Result<Vec<_>, _>>()
            .expect("server certs");
        let key = rustls_pemfile::private_key(&mut key_pem.as_bytes())
            .expect("server key parse")
            .expect("server key");
        let config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .expect("server config");
        let config = Arc::new(config);
        let (mut tcp, _) = listener.accept().expect("tls accept");
        accepts.fetch_add(1, Ordering::SeqCst);
        tcp.set_read_timeout(Some(Duration::from_secs(2))).ok();
        tcp.set_write_timeout(Some(Duration::from_secs(2))).ok();
        let mut conn = rustls::ServerConnection::new(Arc::clone(&config)).expect("server conn");
        let mut tls = rustls::Stream::new(&mut conn, &mut tcp);
        for _ in 0..n_msgs {
            let mut buf = [0u8; 16];
            let n = tls.read(&mut buf).expect("tls read");
            tls.write_all(&buf[..n]).expect("tls write");
            tls.flush().ok();
        }
        let _ = tls.read(&mut [0u8; 1]);
    })
}

fn handshake_script(port: u16, ca: &str, server_name: &str, verify: &str, payload: &str) -> String {
    format!(
        "use ngx;
let sock = ngx::socket::tcp();
ngx::socket::tcp::settimeout(sock, 2000);
ngx::socket::tcp::connect(sock, \"127.0.0.1\", {port});
let store = ngx::socket::tcp::trustedstore(\"{ca}\");
ngx::socket::tcp::settrustedstore(sock, store);
ngx::socket::tcp::sslhandshake(sock, null, \"{server_name}\", {verify}, null);
ngx::socket::tcp::send(sock, \"{payload}\");
ngx::socket::tcp::receive(sock, {len});
ngx::socket::tcp::close(sock);
",
        ca = rss_string(ca),
        len = payload.len()
    )
}

#[test]
fn catalog_declares_tls_hosts() {
    let catalog = ngx_host_catalog();
    let key = ResourceTypeKey::new(NGX_SOCKET_TCP_RESOURCE_KEY).expect("tcp key");

    let handshake = catalog.function(SSLHANDSHAKE).expect("sslhandshake");
    assert_eq!(handshake.params.len(), 5);
    assert_eq!(
        handshake.params[0].ty,
        HostTypeSchema::Resource(key.clone())
    );
    assert_ne!(handshake.params[0].passing, HostParamPassing::Value);
    assert_eq!(handshake.params[1].name, "reused_session");
    assert_eq!(handshake.params[2].name, "server_name");
    assert_eq!(handshake.params[3].name, "ssl_verify");
    assert_eq!(handshake.params[4].name, "send_status_req");
    assert_eq!(handshake.return_type, HostTypeSchema::Unit);

    let store = catalog.function(SETTRUSTEDSTORE).expect("settrustedstore");
    assert_eq!(store.params.len(), 2);
    assert_eq!(store.return_type, HostTypeSchema::Unit);

    let cert = catalog.function(SETCLIENTCERT).expect("setclientcert");
    assert_eq!(cert.params.len(), 2);
    assert_eq!(cert.params[1].name, "cert");
    assert_eq!(cert.return_type, HostTypeSchema::Unit);

    let trustedstore = catalog
        .function("ngx::socket::tcp::trustedstore")
        .expect("trustedstore constructor");
    assert_eq!(trustedstore.params.len(), 1);
    assert_eq!(
        trustedstore.return_type,
        HostTypeSchema::Resource(ResourceTypeKey::new("ngx.socket.tls.trustedstore").unwrap())
    );
    let clientcert = catalog
        .function("ngx::socket::tcp::clientcert")
        .expect("clientcert constructor");
    assert_eq!(clientcert.params.len(), 2);
    assert!(catalog.resource("ngx.socket.tls.trustedstore").is_some());
    assert!(catalog.resource("ngx.socket.tls.clientcert").is_some());

    for name in [
        SSLHANDSHAKE,
        SETTRUSTEDSTORE,
        SETCLIENTCERT,
        "ngx::socket::tcp::trustedstore",
        "ngx::socket::tcp::clientcert",
    ] {
        assert!(production_runtime_host_names().contains(&name));
        let entry = manifest_entry(name).unwrap();
        assert_eq!(entry.family, HostFamily::Cosocket);
        assert_eq!(entry.capabilities, &[HostCapability::Cosocket]);
        assert_eq!(entry.openresty_name, "ngx.socket.tcp");
        assert!(host_allowed_in_phase(name, NgxPhase::Access));
        assert!(!host_allowed_in_phase(name, NgxPhase::Init));
    }
    assert_eq!(
        manifest_entry(SSLHANDSHAKE).unwrap().sync,
        HostSyncKind::Async
    );
    assert_eq!(
        manifest_entry(SETTRUSTEDSTORE).unwrap().sync,
        HostSyncKind::Sync
    );
    assert_eq!(
        manifest_entry(SETCLIENTCERT).unwrap().sync,
        HostSyncKind::Sync
    );
    assert_eq!(
        manifest_entry("ngx::socket::tcp::trustedstore")
            .unwrap()
            .sync,
        HostSyncKind::Sync
    );
    assert_eq!(
        manifest_entry("ngx::socket::tcp::clientcert").unwrap().sync,
        HostSyncKind::Sync
    );
}

#[test]
fn tls_valid_ca_and_hostname_send_receive() {
    let fixtures = generate_fixtures();
    let (listener, port) = bind_listener();
    let accepts = Arc::new(AtomicU64::new(0));
    let peer = spawn_tls_echo(
        listener,
        fixtures.cert_pem.clone(),
        fixtures.key_pem.clone(),
        1,
        Arc::clone(&accepts),
    );
    let mut session = Session::new();
    assert_eq!(
        session.pump(
            &handshake_script(port, &fixtures.ca_pem, "localhost", "true", "ping"),
            Duration::from_secs(4)
        ),
        AccessStatus::Ok
    );
    session.shutdown();
    peer.join().expect("peer");
    assert_eq!(accepts.load(Ordering::SeqCst), 1);
}

#[test]
fn tls_invalid_ca_is_rejected() {
    let fixtures = generate_fixtures();
    let (listener, port) = bind_listener();
    let accepts = Arc::new(AtomicU64::new(0));
    let peer = spawn_tls_echo(
        listener,
        fixtures.cert_pem.clone(),
        fixtures.key_pem.clone(),
        1,
        Arc::clone(&accepts),
    );
    let mut session = Session::new();
    assert_eq!(
        session.pump(
            &handshake_script(port, &fixtures.other_ca_pem, "localhost", "true", "ping"),
            Duration::from_secs(4)
        ),
        AccessStatus::Error500
    );
    session.shutdown();
    let _ = peer.join();
}

#[test]
fn tls_invalid_hostname_is_rejected() {
    let fixtures = generate_fixtures();
    let (listener, port) = bind_listener();
    let accepts = Arc::new(AtomicU64::new(0));
    let peer = spawn_tls_echo(
        listener,
        fixtures.cert_pem.clone(),
        fixtures.key_pem.clone(),
        1,
        Arc::clone(&accepts),
    );
    let mut session = Session::new();
    assert_eq!(
        session.pump(
            &handshake_script(port, &fixtures.ca_pem, "wrong.example", "true", "ping"),
            Duration::from_secs(4)
        ),
        AccessStatus::Error500
    );
    session.shutdown();
    let _ = peer.join();
}

#[test]
fn tls_verify_false_skips_certificate_checks() {
    let fixtures = generate_fixtures();
    let (listener, port) = bind_listener();
    let accepts = Arc::new(AtomicU64::new(0));
    let peer = spawn_tls_echo(
        listener,
        fixtures.cert_pem.clone(),
        fixtures.key_pem.clone(),
        1,
        Arc::clone(&accepts),
    );
    let mut session = Session::new();
    let source = format!(
        "use ngx;
let sock = ngx::socket::tcp();
ngx::socket::tcp::settimeout(sock, 2000);
ngx::socket::tcp::connect(sock, \"127.0.0.1\", {port});
ngx::socket::tcp::sslhandshake(sock, null, \"wrong.example\", false, null);
ngx::socket::tcp::send(sock, \"ping\");
ngx::socket::tcp::receive(sock, 4);
ngx::socket::tcp::close(sock);
"
    );
    assert_eq!(
        session.pump(&source, Duration::from_secs(4)),
        AccessStatus::Ok
    );
    session.shutdown();
    peer.join().expect("peer");
    assert_eq!(accepts.load(Ordering::SeqCst), 1);
}

#[test]
fn tls_handshake_timeout() {
    let (listener, port) = bind_listener();
    let peer = thread::spawn(move || {
        let (stream, _) = listener.accept().expect("accept stall");
        thread::sleep(Duration::from_millis(400));
        drop(stream);
    });
    let mut session = Session::new();
    let source = format!(
        "use ngx;
let sock = ngx::socket::tcp();
ngx::socket::tcp::settimeout(sock, 80);
ngx::socket::tcp::connect(sock, \"127.0.0.1\", {port});
ngx::socket::tcp::sslhandshake(sock, null, \"localhost\", false, null);
"
    );
    assert_eq!(
        session.pump(&source, Duration::from_secs(4)),
        AccessStatus::Error500
    );
    session.shutdown();
    let _ = peer.join();
}

#[test]
fn tls_handshake_cancel_does_not_leak_inflight() {
    let (listener, port) = bind_listener();
    let _peer = thread::spawn(move || {
        let (stream, _) = listener.accept().expect("accept stall");
        thread::sleep(Duration::from_secs(2));
        drop(stream);
    });
    let mut worker = WorkerRuntime::start().unwrap();
    let mut exec = worker
        .bind_access_exec(
            compile(&format!(
                "use ngx;
let sock = ngx::socket::tcp();
ngx::socket::tcp::settimeout(sock, 0);
ngx::socket::tcp::connect(sock, \"127.0.0.1\", {port});
ngx::socket::tcp::sslhandshake(sock, null, \"localhost\", false, null);
"
            )),
            1,
        )
        .expect("bind");
    let mut registry = IdentityRegistry::with_capacity(DEFAULT_MAX_IN_FLIGHT);
    let started = Instant::now();
    loop {
        match exec.handle_access(&mut registry, &worker) {
            AccessStatus::Again => {
                exec.commit_suspend();
                if matches!(exec.vm_status(), Some(VmStatus::Waiting(_)))
                    && started.elapsed() > Duration::from_millis(40)
                {
                    break;
                }
            }
            other => panic!("expected wait for handshake, got {other:?}"),
        }
        if started.elapsed() > Duration::from_secs(2) {
            panic!("never reached a pending handshake");
        }
    }
    assert_eq!(worker.in_flight().unwrap(), 1);
    let request_id = exec.request_id();
    let identity = exec.identity();
    let _ = registry.remove_exact(identity, &request_id);
    let _ = exec.cleanup_wait(&worker);
    assert_eq!(
        worker.in_flight().unwrap(),
        0,
        "handshake cancel must drop inflight 1→0"
    );
    worker.shutdown();
}

#[test]
fn pooled_tls_reuses_the_sessioned_connection() {
    let fixtures = generate_fixtures();
    let (listener, port) = bind_listener();
    let accepts = Arc::new(AtomicU64::new(0));
    let peer = spawn_tls_echo(
        listener,
        fixtures.cert_pem.clone(),
        fixtures.key_pem.clone(),
        2,
        Arc::clone(&accepts),
    );
    let ca = rss_string(&fixtures.ca_pem);
    let first = format!(
        "use ngx;
let sock = ngx::socket::tcp();
ngx::socket::tcp::settimeout(sock, 2000);
ngx::socket::tcp::connect(sock, \"127.0.0.1\", {port});
let store = ngx::socket::tcp::trustedstore(\"{ca}\");
ngx::socket::tcp::settrustedstore(sock, store);
ngx::socket::tcp::sslhandshake(sock, null, \"localhost\", true, null);
ngx::socket::tcp::send(sock, \"one\");
ngx::socket::tcp::receive(sock, 3);
ngx::socket::tcp::setkeepalive(sock, 60000, 8);
"
    );
    let second = format!(
        "use ngx;
let sock = ngx::socket::tcp();
ngx::socket::tcp::settimeout(sock, 2000);
ngx::socket::tcp::connect(sock, \"127.0.0.1\", {port});
let n = ngx::socket::tcp::getreusedtimes(sock);
if n != 0 {{
ngx::exit(500);
}}
let store = ngx::socket::tcp::trustedstore(\"{ca}\");
ngx::socket::tcp::settrustedstore(sock, store);
ngx::socket::tcp::sslhandshake(sock, null, \"localhost\", true, null);
let n = ngx::socket::tcp::getreusedtimes(sock);
if n != 1 {{
ngx::exit(500);
}}
ngx::socket::tcp::send(sock, \"two\");
ngx::socket::tcp::receive(sock, 3);
ngx::socket::tcp::close(sock);
"
    );
    let mut session = Session::new();
    assert_eq!(
        session.pump(&first, Duration::from_secs(4)),
        AccessStatus::Ok
    );
    assert_eq!(
        session.pump(&second, Duration::from_secs(4)),
        AccessStatus::Ok
    );
    session.shutdown();
    peer.join().expect("peer");
    assert_eq!(accepts.load(Ordering::SeqCst), 1);
}

#[test]
fn tls_identity_mismatch_does_not_reuse_pooled_socket() {
    let fixtures = generate_fixtures();
    let (listener, port) = bind_listener();
    let accepts = Arc::new(AtomicU64::new(0));
    let seen = Arc::clone(&accepts);
    let cert = fixtures.cert_pem.clone();
    let key = fixtures.key_pem.clone();
    let peer = thread::spawn(move || {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let certs: Vec<_> = rustls_pemfile::certs(&mut cert.as_bytes())
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let key = rustls_pemfile::private_key(&mut key.as_bytes())
            .unwrap()
            .unwrap();
        let config = Arc::new(
            rustls::ServerConfig::builder()
                .with_no_client_auth()
                .with_single_cert(certs, key)
                .unwrap(),
        );
        let (mut tcp, _) = listener.accept().expect("first");
        seen.fetch_add(1, Ordering::SeqCst);
        {
            let mut conn = rustls::ServerConnection::new(Arc::clone(&config)).unwrap();
            let mut tls = rustls::Stream::new(&mut conn, &mut tcp);
            let mut buf = [0u8; 16];
            let n = match tls.read(&mut buf) {
                Ok(n) => n,
                Err(_) => return,
            };
            tls.write_all(&buf[..n]).ok();
        }
        let (tcp2, _) = match listener.accept() {
            Ok(pair) => pair,
            Err(_) => return,
        };
        seen.fetch_add(1, Ordering::SeqCst);
        drop(tcp2);
    });
    let ca = rss_string(&fixtures.ca_pem);
    let first = format!(
        "use ngx;
let sock = ngx::socket::tcp();
ngx::socket::tcp::settimeout(sock, 2000);
ngx::socket::tcp::connect(sock, \"127.0.0.1\", {port});
let store = ngx::socket::tcp::trustedstore(\"{ca}\");
ngx::socket::tcp::settrustedstore(sock, store);
ngx::socket::tcp::sslhandshake(sock, null, \"localhost\", true, null);
ngx::socket::tcp::send(sock, \"one\");
ngx::socket::tcp::receive(sock, 3);
ngx::socket::tcp::setkeepalive(sock, 60000, 8);
"
    );
    let second = format!(
        "use ngx;
let sock = ngx::socket::tcp();
ngx::socket::tcp::settimeout(sock, 200);
ngx::socket::tcp::connect(sock, \"127.0.0.1\", {port});
ngx::socket::tcp::sslhandshake(sock, null, \"other.example\", false, null);
"
    );
    let mut session = Session::new();
    assert_eq!(
        session.pump(&first, Duration::from_secs(4)),
        AccessStatus::Ok
    );
    let _ = session.pump(&second, Duration::from_secs(4));
    session.shutdown();
    let _ = peer.join();
    assert!(accepts.load(Ordering::SeqCst) >= 2);
}

#[test]
fn connect_never_checkouts_a_tls_pooled_session() {
    let fixtures = generate_fixtures();
    let (listener, port) = bind_listener();
    let accepts = Arc::new(AtomicU64::new(0));
    let peer = spawn_tls_echo(
        listener,
        fixtures.cert_pem.clone(),
        fixtures.key_pem.clone(),
        1,
        Arc::clone(&accepts),
    );
    let ca = rss_string(&fixtures.ca_pem);
    let first = format!(
        "use ngx;
let sock = ngx::socket::tcp();
ngx::socket::tcp::settimeout(sock, 2000);
ngx::socket::tcp::connect(sock, \"127.0.0.1\", {port});
let store = ngx::socket::tcp::trustedstore(\"{ca}\");
ngx::socket::tcp::settrustedstore(sock, store);
ngx::socket::tcp::sslhandshake(sock, null, \"localhost\", true, null);
ngx::socket::tcp::send(sock, \"one\");
ngx::socket::tcp::receive(sock, 3);
ngx::socket::tcp::setkeepalive(sock, 60000, 8);
"
    );
    let second = format!(
        "use ngx;
let sock = ngx::socket::tcp();
ngx::socket::tcp::settimeout(sock, 2000);
ngx::socket::tcp::connect(sock, \"127.0.0.1\", {port});
let n = ngx::socket::tcp::getreusedtimes(sock);
if n != 0 {{
ngx::exit(500);
}}
ngx::socket::tcp::close(sock);
"
    );
    let mut session = Session::new();
    assert_eq!(
        session.pump(&first, Duration::from_secs(4)),
        AccessStatus::Ok
    );
    assert_eq!(
        session.pump(&second, Duration::from_secs(4)),
        AccessStatus::Ok,
        "connect() must not steal a TLS-pooled connection"
    );
    session.shutdown();
    let _ = peer.join();
}

fn spawn_mtls_echo(
    listener: StdTcpListener,
    cert_pem: String,
    key_pem: String,
    ca_pem: String,
    require_client: bool,
    accepts: Arc<AtomicU64>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let certs: Vec<_> = rustls_pemfile::certs(&mut cert_pem.as_bytes())
            .collect::<Result<Vec<_>, _>>()
            .expect("server certs");
        let key = rustls_pemfile::private_key(&mut key_pem.as_bytes())
            .expect("server key parse")
            .expect("server key");
        let mut roots = rustls::RootCertStore::empty();
        for cert in rustls_pemfile::certs(&mut ca_pem.as_bytes()).flatten() {
            let _ = roots.add(cert);
        }
        let verifier = if require_client {
            rustls::server::WebPkiClientVerifier::builder(Arc::new(roots))
                .build()
                .expect("client verifier")
        } else {
            rustls::server::WebPkiClientVerifier::no_client_auth()
        };
        let config = rustls::ServerConfig::builder()
            .with_client_cert_verifier(verifier)
            .with_single_cert(certs, key)
            .expect("mtls config");
        let config = Arc::new(config);
        let (mut tcp, _) = match listener.accept() {
            Ok(pair) => pair,
            Err(_) => return,
        };
        accepts.fetch_add(1, Ordering::SeqCst);
        tcp.set_read_timeout(Some(Duration::from_secs(2))).ok();
        tcp.set_write_timeout(Some(Duration::from_secs(2))).ok();
        let mut conn = match rustls::ServerConnection::new(Arc::clone(&config)) {
            Ok(conn) => conn,
            Err(_) => return,
        };
        let mut tls = rustls::Stream::new(&mut conn, &mut tcp);
        let mut buf = [0u8; 16];
        if let Ok(n) = tls.read(&mut buf) {
            let _ = tls.write_all(&buf[..n]);
            let _ = tls.flush();
        }
    })
}

fn client_hello_has_status_request(buf: &[u8]) -> bool {
    if buf.len() < 9 || buf[0] != 0x16 {
        return false;
    }
    let rec_len = u16::from_be_bytes([buf[3], buf[4]]) as usize;
    if buf.len() < 5 + rec_len {
        return false;
    }
    let hs = &buf[5..5 + rec_len];
    if hs.is_empty() || hs[0] != 0x01 || hs.len() < 4 {
        return false;
    }
    let hs_len = u32::from_be_bytes([0, hs[1], hs[2], hs[3]]) as usize;
    let end = 4 + hs_len.min(hs.len() - 4);
    let body = &hs[4..end];
    if body.len() < 34 {
        return false;
    }
    let mut i = 34;
    let sid_len = body[i] as usize;
    i += 1 + sid_len;
    if i + 2 > body.len() {
        return false;
    }
    let cs_len = u16::from_be_bytes([body[i], body[i + 1]]) as usize;
    i += 2 + cs_len;
    if i + 1 > body.len() {
        return false;
    }
    let comp_len = body[i] as usize;
    i += 1 + comp_len;
    if i + 2 > body.len() {
        return false;
    }
    let ext_len = u16::from_be_bytes([body[i], body[i + 1]]) as usize;
    i += 2;
    let ext_end = (i + ext_len).min(body.len());
    while i + 4 <= ext_end {
        let ty = u16::from_be_bytes([body[i], body[i + 1]]);
        let len = u16::from_be_bytes([body[i + 2], body[i + 3]]) as usize;
        i += 4;
        if ty == 5 {
            return true;
        }
        i += len;
    }
    false
}

#[test]
fn mtls_succeeds_with_matching_client_cert() {
    let fixtures = generate_fixtures();
    let (listener, port) = bind_listener();
    let accepts = Arc::new(AtomicU64::new(0));
    let peer = spawn_mtls_echo(
        listener,
        fixtures.cert_pem.clone(),
        fixtures.key_pem.clone(),
        fixtures.ca_pem.clone(),
        true,
        Arc::clone(&accepts),
    );
    let ca = rss_string(&fixtures.ca_pem);
    let cert = rss_string(&fixtures.client_cert_pem);
    let key = rss_string(&fixtures.client_key_pem);
    let source = format!(
        "use ngx;
let sock = ngx::socket::tcp();
ngx::socket::tcp::settimeout(sock, 2000);
ngx::socket::tcp::connect(sock, \"127.0.0.1\", {port});
let store = ngx::socket::tcp::trustedstore(\"{ca}\");
ngx::socket::tcp::settrustedstore(sock, store);
let client = ngx::socket::tcp::clientcert(\"{cert}\", \"{key}\");
ngx::socket::tcp::setclientcert(sock, client);
ngx::socket::tcp::sslhandshake(sock, null, \"localhost\", true, null);
ngx::socket::tcp::send(sock, \"ping\");
ngx::socket::tcp::receive(sock, 4);
ngx::socket::tcp::close(sock);
"
    );
    let mut session = Session::new();
    assert_eq!(
        session.pump(&source, Duration::from_secs(4)),
        AccessStatus::Ok
    );
    session.shutdown();
    peer.join().expect("peer");
    assert_eq!(accepts.load(Ordering::SeqCst), 1);
}

#[test]
fn mtls_fails_without_client_cert() {
    let fixtures = generate_fixtures();
    let (listener, port) = bind_listener();
    let accepts = Arc::new(AtomicU64::new(0));
    let peer = spawn_mtls_echo(
        listener,
        fixtures.cert_pem.clone(),
        fixtures.key_pem.clone(),
        fixtures.ca_pem.clone(),
        true,
        Arc::clone(&accepts),
    );
    let ca = rss_string(&fixtures.ca_pem);
    let source = format!(
        "use ngx;
let sock = ngx::socket::tcp();
ngx::socket::tcp::settimeout(sock, 2000);
ngx::socket::tcp::connect(sock, \"127.0.0.1\", {port});
let store = ngx::socket::tcp::trustedstore(\"{ca}\");
ngx::socket::tcp::settrustedstore(sock, store);
ngx::socket::tcp::sslhandshake(sock, null, \"localhost\", true, null);
ngx::socket::tcp::send(sock, \"ping\");
ngx::socket::tcp::receive(sock, 4);
"
    );
    let mut session = Session::new();
    assert_eq!(
        session.pump(&source, Duration::from_secs(4)),
        AccessStatus::Error500
    );
    session.shutdown();
    let _ = peer.join();
}

#[test]
fn clientcert_rejects_cert_key_mismatch() {
    let fixtures = generate_fixtures();
    let cert = rss_string(&fixtures.client_cert_pem);
    let key = rss_string(&fixtures.other_client_key_pem);
    let source = format!(
        "use ngx;
let client = ngx::socket::tcp::clientcert(\"{cert}\", \"{key}\");
"
    );
    let mut session = Session::new();
    assert_eq!(
        session.pump(&source, Duration::from_secs(2)),
        AccessStatus::Error500
    );
    session.shutdown();
}

#[test]
fn trust_and_client_resource_type_mismatch_is_rejected() {
    let source = "use ngx;
let sock = ngx::socket::tcp();
ngx::socket::tcp::settrustedstore(sock, sock);
";
    assert!(
        compile_rss_source(source).is_err(),
        "TCP socket must not forge a trusted-store resource"
    );
    let source = "use ngx;
let sock = ngx::socket::tcp();
ngx::socket::tcp::setclientcert(sock, sock);
";
    assert!(
        compile_rss_source(source).is_err(),
        "TCP socket must not forge a client-cert resource"
    );
}

#[test]
fn reused_session_handle_is_unsupported() {
    let (listener, port) = bind_listener();
    let peer = thread::spawn(move || {
        let _ = listener.accept();
    });
    let source = format!(
        "use ngx;
let sock = ngx::socket::tcp();
ngx::socket::tcp::settimeout(sock, 500);
ngx::socket::tcp::connect(sock, \"127.0.0.1\", {port});
ngx::socket::tcp::sslhandshake(sock, 1, \"localhost\", false, null);
"
    );
    let mut session = Session::new();
    assert_eq!(
        session.pump(&source, Duration::from_secs(3)),
        AccessStatus::Error500
    );
    session.shutdown();
    let _ = peer.join();
}

#[test]
fn send_status_req_false_is_unsupported() {
    let (listener, port) = bind_listener();
    let peer = thread::spawn(move || {
        let _ = listener.accept();
    });
    let source = format!(
        "use ngx;
let sock = ngx::socket::tcp();
ngx::socket::tcp::settimeout(sock, 500);
ngx::socket::tcp::connect(sock, \"127.0.0.1\", {port});
ngx::socket::tcp::sslhandshake(sock, null, \"localhost\", false, false);
"
    );
    let mut session = Session::new();
    assert_eq!(
        session.pump(&source, Duration::from_secs(3)),
        AccessStatus::Error500
    );
    session.shutdown();
    let _ = peer.join();
}

#[test]
fn send_status_req_true_puts_ocsp_status_request_in_client_hello() {
    let (listener, port) = bind_listener();
    let seen = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = Arc::clone(&seen);
    let peer = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept hello");
        stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
        let mut buf = [0u8; 4096];
        if let Ok(n) = stream.read(&mut buf) {
            flag.store(client_hello_has_status_request(&buf[..n]), Ordering::SeqCst);
        }
    });
    let source = format!(
        "use ngx;
let sock = ngx::socket::tcp();
ngx::socket::tcp::settimeout(sock, 800);
ngx::socket::tcp::connect(sock, \"127.0.0.1\", {port});
ngx::socket::tcp::sslhandshake(sock, null, \"localhost\", false, true);
"
    );
    let mut session = Session::new();
    let _ = session.pump(&source, Duration::from_secs(3));
    session.shutdown();
    let _ = peer.join();
    assert!(
        seen.load(Ordering::SeqCst),
        "rustls ClientHello must include OCSP status_request when send_status_req is true"
    );
}

#[test]
fn reused_session_false_still_handshakes() {
    let fixtures = generate_fixtures();
    let (listener, port) = bind_listener();
    let accepts = Arc::new(AtomicU64::new(0));
    let peer = spawn_tls_echo(
        listener,
        fixtures.cert_pem.clone(),
        fixtures.key_pem.clone(),
        1,
        Arc::clone(&accepts),
    );
    let ca = rss_string(&fixtures.ca_pem);
    let source = format!(
        "use ngx;
let sock = ngx::socket::tcp();
ngx::socket::tcp::settimeout(sock, 2000);
ngx::socket::tcp::connect(sock, \"127.0.0.1\", {port});
let store = ngx::socket::tcp::trustedstore(\"{ca}\");
ngx::socket::tcp::settrustedstore(sock, store);
ngx::socket::tcp::sslhandshake(sock, false, \"localhost\", true, null);
ngx::socket::tcp::send(sock, \"ping\");
ngx::socket::tcp::receive(sock, 4);
ngx::socket::tcp::close(sock);
"
    );
    let mut session = Session::new();
    assert_eq!(
        session.pump(&source, Duration::from_secs(4)),
        AccessStatus::Ok
    );
    session.shutdown();
    peer.join().expect("peer");
}
