//! Worker-local TCP/TLS connection pool.
//!
//! Keys include the connect endpoint and TLS identity. Connections are owned
//! by the pool, not by a request, so old-request cancel/close cannot drop a
//! pooled socket. The pool is thread-local: workers never share connections.
//!
//! `connect()` may check out a plaintext (`tls = None`) key only. TLS streams
//! transfer only during `sslhandshake` after an exact endpoint + SNI + verify
//! + trust-identity + client-identity match.

use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::io::ErrorKind;
use std::os::fd::AsRawFd;
use std::time::{Duration, Instant};

use tokio::net::{TcpStream, UnixStream};

pub(crate) const DEFAULT_IDLE_TIMEOUT_MS: u64 = 60_000;
pub(crate) const DEFAULT_POOL_SIZE: usize = 30;
pub(crate) const MAX_POOL_SIZE: usize = 256;
pub(crate) const MAX_POOL_KEYS: usize = 32;
pub(crate) const MAX_REUSE: u64 = 1_000;
pub(crate) const MAX_PEM_BYTES: usize = 65_536;

pub(crate) type TlsTcpStream = tokio_rustls::client::TlsStream<TcpStream>;
pub(crate) type TlsUnixStream = tokio_rustls::client::TlsStream<UnixStream>;

thread_local! {
    static POOL: RefCell<WorkerPool> = const { RefCell::new(WorkerPool::new()) };
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub(crate) enum PoolEndpoint {
    Tcp { host: String, port: u16 },
    Unix { path: String },
}

/// Public-material TLS identity. Trust/client fingerprints are SHA-256 of
/// certificate DERs (never private keys). `None` means no explicit store or
/// client cert, which is distinct from any constructed resource.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub(crate) struct TlsIdentity {
    pub sni: String,
    pub verify: bool,
    pub trust_fp: Option<[u8; 32]>,
    pub client_fp: Option<[u8; 32]>,
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub(crate) struct PoolKey {
    pub endpoint: PoolEndpoint,
    pub tls: Option<TlsIdentity>,
}

pub(crate) enum PooledIo {
    Tcp(TcpStream),
    Unix(UnixStream),
    TlsTcp(TlsTcpStream),
    TlsUnix(TlsUnixStream),
}

pub(crate) struct PooledConnection {
    pub io: PooledIo,
    pub reused_times: u64,
    pub tls: Option<TlsIdentity>,
    idle_since: Instant,
    idle_timeout: Option<Duration>,
}

struct Bucket {
    conns: VecDeque<PooledConnection>,
    pool_size: usize,
}

pub(crate) struct WorkerPool {
    buckets: BTreeMap<PoolKey, Bucket>,
}

impl WorkerPool {
    const fn new() -> Self {
        Self {
            buckets: BTreeMap::new(),
        }
    }

    fn expire(&mut self, now: Instant) {
        let keys: Vec<PoolKey> = self.buckets.keys().cloned().collect();
        let mut empty = Vec::new();
        for key in keys {
            if let Some(bucket) = self.buckets.get_mut(&key) {
                while let Some(front) = bucket.conns.front() {
                    if expired(front, now) {
                        drop(bucket.conns.pop_front());
                    } else {
                        break;
                    }
                }
                if bucket.conns.is_empty() {
                    empty.push(key);
                }
            }
        }
        for key in empty {
            self.buckets.remove(&key);
        }
    }

    fn insert(&mut self, key: PoolKey, conn: PooledConnection, pool_size: usize) {
        let now = Instant::now();
        self.expire(now);
        let pool_size = pool_size.clamp(1, MAX_POOL_SIZE);
        if !self.buckets.contains_key(&key) && self.buckets.len() >= MAX_POOL_KEYS {
            self.evict_oldest_key();
        }
        let bucket = self.buckets.entry(key).or_insert_with(|| Bucket {
            conns: VecDeque::new(),
            pool_size,
        });
        bucket.pool_size = pool_size;
        while bucket.conns.len() >= bucket.pool_size {
            drop(bucket.conns.pop_front());
        }
        bucket.conns.push_back(conn);
    }

    fn evict_oldest_key(&mut self) {
        let oldest = self.buckets.iter().min_by_key(|(_, bucket)| {
            bucket
                .conns
                .front()
                .map(|conn| conn.idle_since)
                .unwrap_or(Instant::now())
        });
        if let Some(key) = oldest.map(|(key, _)| key.clone()) {
            self.buckets.remove(&key);
        }
    }

    fn checkout_key(&mut self, key: &PoolKey) -> Option<PooledConnection> {
        let now = Instant::now();
        self.expire(now);
        let bucket = self.buckets.get_mut(key)?;
        let mut found = None;
        while let Some(mut conn) = bucket.conns.pop_back() {
            if expired(&conn, now)
                || conn.reused_times >= MAX_REUSE
                || !conn.io.is_alive_and_clean()
            {
                continue;
            }
            conn.reused_times = conn.reused_times.saturating_add(1);
            found = Some(conn);
            break;
        }
        let empty = bucket.conns.is_empty();
        if empty {
            self.buckets.remove(key);
        }
        found
    }

    fn checkout_plain(&mut self, endpoint: &PoolEndpoint) -> Option<PooledConnection> {
        self.checkout_key(&PoolKey {
            endpoint: endpoint.clone(),
            tls: None,
        })
    }

    fn checkout_tls(
        &mut self,
        endpoint: &PoolEndpoint,
        tls: &TlsIdentity,
    ) -> Option<PooledConnection> {
        self.checkout_key(&PoolKey {
            endpoint: endpoint.clone(),
            tls: Some(tls.clone()),
        })
    }

    fn clear(&mut self) {
        self.buckets.clear();
    }
}

impl PooledIo {
    pub(crate) fn is_alive_and_clean(&mut self) -> bool {
        match self {
            PooledIo::Tcp(stream) => peek_alive_and_clean(stream.as_raw_fd()),
            PooledIo::Unix(stream) => peek_alive_and_clean(stream.as_raw_fd()),
            PooledIo::TlsTcp(stream) => tls_alive_and_clean(stream),
            PooledIo::TlsUnix(stream) => tls_unix_alive_and_clean(stream),
        }
    }
}

fn expired(conn: &PooledConnection, now: Instant) -> bool {
    conn.idle_timeout
        .is_some_and(|timeout| now.saturating_duration_since(conn.idle_since) >= timeout)
}

fn peek_alive_and_clean(fd: libc::c_int) -> bool {
    let mut buf = [0u8; 1];
    loop {
        let n = unsafe {
            libc::recv(
                fd,
                buf.as_mut_ptr().cast(),
                1,
                libc::MSG_PEEK | libc::MSG_DONTWAIT,
            )
        };
        if n == 0 {
            return false;
        }
        if n > 0 {
            return false;
        }
        let err = std::io::Error::last_os_error();
        match err.kind() {
            ErrorKind::Interrupted => continue,
            ErrorKind::WouldBlock => return true,
            _ => return false,
        }
    }
}

fn rustls_plaintext_or_eof(conn: &mut rustls::ClientConnection) -> bool {
    match conn.reader().into_first_chunk() {
        Ok(_) => true,
        Err(err) if err.kind() == ErrorKind::WouldBlock => false,
        Err(_) => true,
    }
}

fn tls_alive_and_clean(stream: &mut TlsTcpStream) -> bool {
    let (tcp, conn) = stream.get_mut();
    if rustls_plaintext_or_eof(conn) {
        return false;
    }
    peek_alive_and_clean(tcp.as_raw_fd())
}

fn tls_unix_alive_and_clean(stream: &mut TlsUnixStream) -> bool {
    let (unix, conn) = stream.get_mut();
    if rustls_plaintext_or_eof(conn) {
        return false;
    }
    peek_alive_and_clean(unix.as_raw_fd())
}

pub(crate) fn with_pool<R>(f: impl FnOnce(&mut WorkerPool) -> R) -> R {
    POOL.with(|pool| f(&mut pool.borrow_mut()))
}

pub(crate) fn shutdown_current() {
    with_pool(WorkerPool::clear);
}

pub(crate) fn insert_pooled(
    key: PoolKey,
    io: PooledIo,
    reused_times: u64,
    idle_timeout: Option<Duration>,
    pool_size: usize,
) {
    let tls = key.tls.clone();
    with_pool(|pool| {
        pool.insert(
            key,
            PooledConnection {
                io,
                reused_times,
                tls,
                idle_since: Instant::now(),
                idle_timeout,
            },
            pool_size,
        );
    });
}

pub(crate) fn checkout_for_connect(endpoint: &PoolEndpoint) -> Option<PooledConnection> {
    with_pool(|pool| pool.checkout_plain(endpoint))
}

pub(crate) fn checkout_matching_tls(
    endpoint: &PoolEndpoint,
    tls: &TlsIdentity,
) -> Option<PooledConnection> {
    with_pool(|pool| pool.checkout_tls(endpoint, tls))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::fd::AsRawFd;
    use std::sync::{Arc, Mutex};
    use std::thread;

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio test runtime")
    }

    fn pair() -> (TcpStream, std::net::TcpStream) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let client = std::net::TcpStream::connect(addr).expect("client");
        let (server, _) = listener.accept().expect("accept");
        client.set_nonblocking(true).ok();
        let tokio_client =
            TcpStream::from_std(client.try_clone().expect("dup")).expect("tokio client");
        drop(client);
        (tokio_client, server)
    }

    fn plain_key() -> PoolKey {
        PoolKey {
            endpoint: PoolEndpoint::Tcp {
                host: "127.0.0.1".into(),
                port: 1,
            },
            tls: None,
        }
    }

    #[test]
    fn connect_checkout_ignores_tls_keys() {
        let rt = runtime();
        let _guard = rt.enter();
        let (client, server) = pair();
        let mut pool = WorkerPool::new();
        let endpoint = PoolEndpoint::Tcp {
            host: "127.0.0.1".into(),
            port: 1,
        };
        pool.insert(
            PoolKey {
                endpoint: endpoint.clone(),
                tls: Some(TlsIdentity {
                    sni: "localhost".into(),
                    verify: true,
                    trust_fp: Some([1u8; 32]),
                    client_fp: None,
                }),
            },
            PooledConnection {
                io: PooledIo::Tcp(client),
                reused_times: 0,
                tls: None,
                idle_since: Instant::now(),
                idle_timeout: None,
            },
            8,
        );
        assert!(
            pool.checkout_plain(&endpoint).is_none(),
            "connect must not steal a TLS-keyed connection"
        );
        drop(server);
    }

    #[test]
    fn peek_does_not_consume_a_dirty_byte() {
        let rt = runtime();
        let _guard = rt.enter();
        let (client, mut server) = pair();
        server.write_all(b"Z").expect("write dirty");
        server.flush().ok();
        thread::sleep(Duration::from_millis(20));
        let mut io = PooledIo::Tcp(client);
        assert!(!io.is_alive_and_clean());
        let mut peek_buf = [0u8; 1];
        let fd = match &io {
            PooledIo::Tcp(stream) => stream.as_raw_fd(),
            _ => unreachable!(),
        };
        let n = unsafe {
            libc::recv(
                fd,
                peek_buf.as_mut_ptr().cast(),
                1,
                libc::MSG_PEEK | libc::MSG_DONTWAIT,
            )
        };
        assert_eq!(n, 1);
        assert_eq!(peek_buf[0], b'Z');
        drop(io);
        drop(server);
    }

    #[test]
    fn concurrent_checkout_from_one_bucket_is_exclusive() {
        let rt = runtime();
        let _guard = rt.enter();
        let (c1, s1) = pair();
        let pool = Arc::new(Mutex::new(WorkerPool::new()));
        {
            let mut guard = pool.lock().expect("lock");
            guard.insert(
                plain_key(),
                PooledConnection {
                    io: PooledIo::Tcp(c1),
                    reused_times: 0,
                    tls: None,
                    idle_since: Instant::now(),
                    idle_timeout: None,
                },
                8,
            );
        }
        let key = plain_key();
        let a_pool = Arc::clone(&pool);
        let b_pool = Arc::clone(&pool);
        let a_key = key.clone();
        let b_key = key;
        let a = thread::spawn(move || a_pool.lock().unwrap().checkout_key(&a_key).is_some());
        let b = thread::spawn(move || b_pool.lock().unwrap().checkout_key(&b_key).is_some());
        let a_hit = a.join().expect("a");
        let b_hit = b.join().expect("b");
        assert_eq!(
            usize::from(a_hit) + usize::from(b_hit),
            1,
            "exactly one concurrent checkout may win"
        );
        drop(s1);
    }
}
