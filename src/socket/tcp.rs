//! Tokio TCP/Unix driver for `ngx.socket.tcp`.
//!
//! Futures hold Tokio stream halves, owned buffers, timeout, generation, and
//! cancel identity. They never hold a request, pool, VM, or raw pointer.

use std::future::Future;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use vm::resource::{CloseProgress, HostResource};
use vm::{
    CaptureAsyncHostContext, ResourceCloseReason, ResourceHandle, ResourceTypeKey, Value, Vm,
    VmError, VmResult,
};

use crate::host::{NGX_SOCKET_TCP_NAME, NGX_SOCKET_TCP_RESOURCE_KEY};
use crate::socket::pool::{
    self, DEFAULT_IDLE_TIMEOUT_MS, DEFAULT_POOL_SIZE, MAX_POOL_SIZE, MAX_REUSE, PoolEndpoint,
    PoolKey, PooledConnection, PooledIo, TlsIdentity, TlsTcpStream, TlsUnixStream, insert_pooled,
};

pub(crate) const MAX_DNS_HOST_BYTES: usize = 255;
pub(crate) const MAX_UNIX_PATH_BYTES: usize = 107;
pub(crate) const MAX_SEND_BYTES: usize = 1_048_576;
pub(crate) const MAX_RECEIVE_BYTES: usize = 1_048_576;
pub(crate) const MAX_DELIMITER_BYTES: usize = 256;
pub(crate) const MAX_BUFFER_BYTES: usize = 1_048_576;
const READ_CHUNK_BYTES: usize = 8_192;
const DEFAULT_TIMEOUT_MS: u64 = 60_000;

enum ReadHalf {
    Tcp(tokio::net::tcp::OwnedReadHalf),
    Unix(tokio::net::unix::OwnedReadHalf),
    TlsTcp(tokio::io::ReadHalf<TlsTcpStream>),
    TlsUnix(tokio::io::ReadHalf<TlsUnixStream>),
    #[cfg(test)]
    Test(TestReader),
}

enum WriteHalf {
    Tcp(tokio::net::tcp::OwnedWriteHalf),
    Unix(tokio::net::unix::OwnedWriteHalf),
    TlsTcp(tokio::io::WriteHalf<TlsTcpStream>),
    TlsUnix(tokio::io::WriteHalf<TlsUnixStream>),
    #[cfg(test)]
    Test(TestWriter),
}

#[cfg(test)]
static TEST_STALL_TCP_CONNECT: AtomicBool = AtomicBool::new(false);
#[cfg(test)]
static TEST_STALL_UNIX_CONNECT: AtomicBool = AtomicBool::new(false);
#[cfg(test)]
static TEST_STALL_DNS: AtomicBool = AtomicBool::new(false);
#[cfg(test)]
static STALL_LOCK: Mutex<()> = Mutex::new(());

#[cfg(test)]
struct StallGuard {
    flag: &'static AtomicBool,
    _lock: std::sync::MutexGuard<'static, ()>,
}

#[cfg(test)]
impl StallGuard {
    fn acquire(flag: &'static AtomicBool) -> Self {
        let lock = lock(&STALL_LOCK);
        flag.store(true, Ordering::SeqCst);
        Self { flag, _lock: lock }
    }

    fn tcp_connect() -> Self {
        Self::acquire(&TEST_STALL_TCP_CONNECT)
    }

    fn unix_connect() -> Self {
        Self::acquire(&TEST_STALL_UNIX_CONNECT)
    }

    fn dns() -> Self {
        Self::acquire(&TEST_STALL_DNS)
    }
}

#[cfg(test)]
impl Drop for StallGuard {
    fn drop(&mut self) {
        self.flag.store(false, Ordering::SeqCst);
    }
}

#[cfg(test)]
struct TestReader {
    state: Arc<TestReadState>,
}

#[cfg(test)]
struct TestReadState {
    chunks: Mutex<std::collections::VecDeque<Vec<u8>>>,
    waker: Mutex<Option<std::task::Waker>>,
    reads: AtomicU64,
}

#[cfg(test)]
struct TestWriter {
    state: Arc<TestWriteState>,
}

#[cfg(test)]
struct TestWriteState {
    sink: Mutex<Vec<u8>>,
    waker: Mutex<Option<std::task::Waker>>,
    writes: AtomicU64,
    max_chunk: AtomicU64,
}

#[cfg(test)]
impl TestReadState {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            chunks: Mutex::new(std::collections::VecDeque::new()),
            waker: Mutex::new(None),
            reads: AtomicU64::new(0),
        })
    }

    fn push(&self, chunk: Vec<u8>) {
        lock(&self.chunks).push_back(chunk);
        if let Some(waker) = lock(&self.waker).take() {
            waker.wake();
        }
    }
}

#[cfg(test)]
impl TestWriteState {
    fn new(max_chunk: u64) -> Arc<Self> {
        Arc::new(Self {
            sink: Mutex::new(Vec::new()),
            waker: Mutex::new(None),
            writes: AtomicU64::new(0),
            max_chunk: AtomicU64::new(max_chunk),
        })
    }
}

impl AsyncRead for ReadHalf {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match self.get_mut() {
            ReadHalf::Tcp(half) => Pin::new(half).poll_read(cx, buf),
            ReadHalf::Unix(half) => Pin::new(half).poll_read(cx, buf),
            ReadHalf::TlsTcp(half) => Pin::new(half).poll_read(cx, buf),
            ReadHalf::TlsUnix(half) => Pin::new(half).poll_read(cx, buf),
            #[cfg(test)]
            ReadHalf::Test(reader) => {
                let mut chunks = lock(&reader.state.chunks);
                if let Some(chunk) = chunks.pop_front() {
                    drop(chunks);
                    reader.state.reads.fetch_add(1, Ordering::SeqCst);
                    let n = chunk.len().min(buf.remaining());
                    buf.put_slice(&chunk[..n]);
                    Poll::Ready(Ok(()))
                } else {
                    *lock(&reader.state.waker) = Some(cx.waker().clone());
                    Poll::Pending
                }
            }
        }
    }
}

impl AsyncWrite for WriteHalf {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            WriteHalf::Tcp(half) => Pin::new(half).poll_write(cx, buf),
            WriteHalf::Unix(half) => Pin::new(half).poll_write(cx, buf),
            WriteHalf::TlsTcp(half) => Pin::new(half).poll_write(cx, buf),
            WriteHalf::TlsUnix(half) => Pin::new(half).poll_write(cx, buf),
            #[cfg(test)]
            WriteHalf::Test(writer) => {
                if buf.is_empty() {
                    return Poll::Ready(Ok(0));
                }
                let max = writer.state.max_chunk.load(Ordering::SeqCst) as usize;
                if max == 0 {
                    *lock(&writer.state.waker) = Some(cx.waker().clone());
                    return Poll::Pending;
                }
                let n = buf.len().min(max);
                lock(&writer.state.sink).extend_from_slice(&buf[..n]);
                writer.state.writes.fetch_add(1, Ordering::SeqCst);
                Poll::Ready(Ok(n))
            }
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            WriteHalf::Tcp(half) => Pin::new(half).poll_flush(cx),
            WriteHalf::Unix(half) => Pin::new(half).poll_flush(cx),
            WriteHalf::TlsTcp(half) => Pin::new(half).poll_flush(cx),
            WriteHalf::TlsUnix(half) => Pin::new(half).poll_flush(cx),
            #[cfg(test)]
            WriteHalf::Test(_) => Poll::Ready(Ok(())),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            WriteHalf::Tcp(half) => Pin::new(half).poll_shutdown(cx),
            WriteHalf::Unix(half) => Pin::new(half).poll_shutdown(cx),
            WriteHalf::TlsTcp(half) => Pin::new(half).poll_shutdown(cx),
            WriteHalf::TlsUnix(half) => Pin::new(half).poll_shutdown(cx),
            #[cfg(test)]
            WriteHalf::Test(_) => Poll::Ready(Ok(())),
        }
    }
}

#[derive(Clone)]
pub(crate) struct IoHalves {
    reader: Arc<tokio::sync::Mutex<ReadHalf>>,
    writer: Arc<tokio::sync::Mutex<WriteHalf>>,
}

pub(crate) struct TcpInner {
    pub(crate) io: Mutex<Option<IoHalves>>,
    pub(crate) unread: Mutex<Vec<u8>>,
    pub(crate) timeout_ms: AtomicU64,
    pub(crate) closed: AtomicBool,
    pub(crate) pending_read: AtomicBool,
    pub(crate) pending_write: AtomicBool,
    pub(crate) generation: AtomicU64,
    pub(crate) cancel: tokio::sync::Notify,
    pub(crate) endpoint: Mutex<Option<PoolEndpoint>>,
    pub(crate) tls: Mutex<Option<TlsIdentity>>,
    pub(crate) reused_times: AtomicU64,
    pub(crate) trusted_store: Mutex<Option<Arc<crate::socket::tls::TrustedStoreInner>>>,
    pub(crate) client_cert: Mutex<Option<Arc<crate::socket::tls::ClientCertInner>>>,
}

impl TcpInner {
    pub(crate) fn new() -> Self {
        Self {
            io: Mutex::new(None),
            unread: Mutex::new(Vec::new()),
            timeout_ms: AtomicU64::new(DEFAULT_TIMEOUT_MS),
            closed: AtomicBool::new(false),
            pending_read: AtomicBool::new(false),
            pending_write: AtomicBool::new(false),
            generation: AtomicU64::new(1),
            cancel: tokio::sync::Notify::new(),
            endpoint: Mutex::new(None),
            tls: Mutex::new(None),
            reused_times: AtomicU64::new(0),
            trusted_store: Mutex::new(None),
            client_cert: Mutex::new(None),
        }
    }
}

pub(crate) struct TcpCosocket {
    inner: Arc<TcpInner>,
}

impl TcpCosocket {
    pub(crate) fn new() -> Self {
        Self {
            inner: Arc::new(TcpInner::new()),
        }
    }
}

impl HostResource for TcpCosocket {
    fn resource_type_key() -> Option<ResourceTypeKey>
    where
        Self: Sized,
    {
        Some(
            ResourceTypeKey::new(NGX_SOCKET_TCP_RESOURCE_KEY)
                .expect("ngx.socket.tcp is a valid resource type key"),
        )
    }

    fn begin_close(
        &mut self,
        _reason: ResourceCloseReason,
    ) -> vm::resource::ResourceResult<CloseProgress> {
        self.inner.closed.store(true, Ordering::SeqCst);
        self.inner.generation.fetch_add(1, Ordering::SeqCst);
        self.inner.pending_read.store(false, Ordering::SeqCst);
        self.inner.pending_write.store(false, Ordering::SeqCst);
        *lock(&self.inner.io) = None;
        lock(&self.inner.unread).clear();
        *lock(&self.inner.tls) = None;
        *lock(&self.inner.endpoint) = None;
        *lock(&self.inner.trusted_store) = None;
        *lock(&self.inner.client_cert) = None;
        self.inner.cancel.notify_waiters();
        Ok(CloseProgress::Ready)
    }
}

struct PendingReadGuard {
    inner: Arc<TcpInner>,
}

impl Drop for PendingReadGuard {
    fn drop(&mut self) {
        self.inner.pending_read.store(false, Ordering::SeqCst);
    }
}

struct PendingWriteGuard {
    inner: Arc<TcpInner>,
}

impl Drop for PendingWriteGuard {
    fn drop(&mut self) {
        self.inner.pending_write.store(false, Ordering::SeqCst);
    }
}

pub(crate) struct TcpReadOpContext {
    inner: Arc<TcpInner>,
    generation: u64,
    _pending: PendingReadGuard,
}

pub(crate) struct TcpWriteOpContext {
    inner: Arc<TcpInner>,
    generation: u64,
    _pending: PendingWriteGuard,
}

pub(crate) struct TcpConnectOwnedContext {
    inner: Arc<TcpInner>,
    generation: u64,
    handle_raw: i64,
    _pending: PendingWriteGuard,
}

pub(crate) struct TcpHandshakeOpContext {
    pub(crate) inner: Arc<TcpInner>,
    pub(crate) generation: u64,
    _read: PendingReadGuard,
    _write: PendingWriteGuard,
}

impl TcpReadOpContext {
    fn check_identity(&self, name: &str) -> VmResult<()> {
        check_generation(&self.inner, self.generation, name)
    }
}

impl TcpWriteOpContext {
    fn check_identity(&self, name: &str) -> VmResult<()> {
        check_generation(&self.inner, self.generation, name)
    }
}

impl TcpConnectOwnedContext {
    fn check_identity(&self, name: &str) -> VmResult<()> {
        check_generation(&self.inner, self.generation, name)
    }

    pub(crate) fn handle_raw(&self) -> i64 {
        self.handle_raw
    }
}

impl TcpHandshakeOpContext {
    pub(crate) fn check_identity(&self, name: &str) -> VmResult<()> {
        check_generation(&self.inner, self.generation, name)
    }
}

impl CaptureAsyncHostContext for TcpReadOpContext {
    fn capture(_vm: &mut Vm) -> VmResult<Self> {
        Err(VmError::HostError(
            "ngx::socket::tcp read context requires a socket handle".into(),
        ))
    }

    fn capture_with_args(vm: &mut Vm, args: &[Value]) -> VmResult<Self> {
        let (inner, generation) = capture_socket(vm, args)?;
        if inner.pending_read.swap(true, Ordering::SeqCst) {
            return Err(VmError::HostError(
                "ngx::socket::tcp already has a pending read".into(),
            ));
        }
        Ok(Self {
            inner: Arc::clone(&inner),
            generation,
            _pending: PendingReadGuard { inner },
        })
    }
}

impl CaptureAsyncHostContext for TcpWriteOpContext {
    fn capture(_vm: &mut Vm) -> VmResult<Self> {
        Err(VmError::HostError(
            "ngx::socket::tcp write context requires a socket handle".into(),
        ))
    }

    fn capture_with_args(vm: &mut Vm, args: &[Value]) -> VmResult<Self> {
        let (inner, generation) = capture_socket(vm, args)?;
        if inner.pending_write.swap(true, Ordering::SeqCst) {
            return Err(VmError::HostError(
                "ngx::socket::tcp already has a pending write".into(),
            ));
        }
        Ok(Self {
            inner: Arc::clone(&inner),
            generation,
            _pending: PendingWriteGuard { inner },
        })
    }
}

impl CaptureAsyncHostContext for TcpConnectOwnedContext {
    fn capture(vm: &mut Vm) -> VmResult<Self> {
        let token = vm
            .host_context()
            .push_resource(TcpCosocket::new())
            .map_err(host_ctx)?;
        let handle_raw = token.handle().raw() as i64;
        let inner = {
            let ctx = vm.host_context();
            let resource = ctx.resource(&token).map_err(host_ctx)?;
            Arc::clone(&resource.get().inner)
        };
        if inner.pending_write.swap(true, Ordering::SeqCst) {
            return Err(VmError::HostError(
                "ngx::socket::tcp already has a pending write".into(),
            ));
        }
        let generation = inner.generation.load(Ordering::SeqCst);
        Ok(Self {
            inner: Arc::clone(&inner),
            generation,
            handle_raw,
            _pending: PendingWriteGuard { inner },
        })
    }
}

impl CaptureAsyncHostContext for TcpHandshakeOpContext {
    fn capture(_vm: &mut Vm) -> VmResult<Self> {
        Err(VmError::HostError(
            "ngx::socket::tcp::sslhandshake context requires a socket handle".into(),
        ))
    }

    fn capture_with_args(vm: &mut Vm, args: &[Value]) -> VmResult<Self> {
        let (inner, generation) = capture_socket(vm, args)?;
        if inner.pending_read.swap(true, Ordering::SeqCst) {
            return Err(VmError::HostError(
                "ngx::socket::tcp already has a pending read".into(),
            ));
        }
        if inner.pending_write.swap(true, Ordering::SeqCst) {
            inner.pending_read.store(false, Ordering::SeqCst);
            return Err(VmError::HostError(
                "ngx::socket::tcp already has a pending write".into(),
            ));
        }
        Ok(Self {
            inner: Arc::clone(&inner),
            generation,
            _read: PendingReadGuard {
                inner: Arc::clone(&inner),
            },
            _write: PendingWriteGuard { inner },
        })
    }
}

pub(crate) fn timeout_duration(ms: u64) -> Option<Duration> {
    if ms == 0 {
        None
    } else {
        Some(Duration::from_millis(ms))
    }
}

fn capture_socket(vm: &mut Vm, args: &[Value]) -> VmResult<(Arc<TcpInner>, u64)> {
    let handle_id = match args.first() {
        Some(Value::Int(value)) => *value,
        Some(_) => return Err(VmError::TypeMismatch("resource handle")),
        None => {
            return Err(VmError::HostError(
                "ngx::socket::tcp missing socket handle".into(),
            ));
        }
    };
    let handle = parse_handle(handle_id)?;
    let ctx = vm.host_context();
    let token = ctx
        .typed_resource::<TcpCosocket>(handle)
        .map_err(host_ctx)?;
    let resource = ctx.resource(&token).map_err(host_ctx)?;
    let inner = Arc::clone(&resource.get().inner);
    if inner.closed.load(Ordering::SeqCst) {
        return Err(closed_error(NGX_SOCKET_TCP_NAME));
    }
    let generation = inner.generation.load(Ordering::SeqCst);
    Ok((inner, generation))
}

fn check_generation(inner: &TcpInner, generation: u64, name: &str) -> VmResult<()> {
    if inner.closed.load(Ordering::SeqCst) || inner.generation.load(Ordering::SeqCst) != generation
    {
        Err(closed_error(name))
    } else {
        Ok(())
    }
}

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|err| err.into_inner())
}

fn host_ctx(error: vm::HostContextError) -> VmError {
    VmError::HostError(error.to_string())
}

fn closed_error(name: &str) -> VmError {
    VmError::HostError(format!("{name} socket is closed"))
}

fn parse_handle(handle_id: i64) -> VmResult<ResourceHandle> {
    if handle_id <= 0 {
        return Err(VmError::HostError(format!(
            "ngx::socket::tcp invalid handle {handle_id}"
        )));
    }
    ResourceHandle::from_raw(handle_id as u64).map_err(|error| {
        VmError::HostError(format!(
            "ngx::socket::tcp invalid handle {handle_id}: {error}"
        ))
    })
}

fn port_u16(port: i64, name: &str) -> VmResult<u16> {
    u16::try_from(port).map_err(|_| {
        VmError::HostError(format!(
            "{name} port must be between 0 and 65535, got {port}"
        ))
    })
}

fn check_dns_host(host: &str, name: &str) -> VmResult<()> {
    if host.is_empty() {
        return Err(VmError::HostError(format!("{name} host must not be empty")));
    }
    if host.len() > MAX_DNS_HOST_BYTES {
        return Err(VmError::HostError(format!(
            "{name} host exceeds the {MAX_DNS_HOST_BYTES}-byte limit"
        )));
    }
    Ok(())
}

fn unix_path<'a>(host: &'a str, port: i64, name: &str) -> VmResult<Option<&'a Path>> {
    let Some(path) = host.strip_prefix("unix:") else {
        return Ok(None);
    };
    if port != 0 {
        return Err(VmError::HostError(format!(
            "{name} unix socket port must be 0, got {port}"
        )));
    }
    if path.is_empty() {
        return Err(VmError::HostError(format!(
            "{name} unix socket path must not be empty"
        )));
    }
    if path.len() > MAX_UNIX_PATH_BYTES {
        return Err(VmError::HostError(format!(
            "{name} unix socket path exceeds the {MAX_UNIX_PATH_BYTES}-byte limit"
        )));
    }
    Ok(Some(Path::new(path)))
}

pub(crate) fn payload_bytes(data: &Value, name: &str) -> VmResult<Vec<u8>> {
    match data {
        Value::String(text) => Ok(text.as_bytes().to_vec()),
        Value::Bytes(bytes) => Ok((**bytes).clone()),
        _ => Err(VmError::HostError(format!(
            "{name} expects string or bytes"
        ))),
    }
}

#[derive(Debug)]
enum ReceiveMode {
    Exact(usize),
    Line,
    All,
}

fn receive_mode(mode: &Value) -> VmResult<ReceiveMode> {
    match mode {
        Value::Null => Ok(ReceiveMode::Line),
        Value::Int(size) => {
            if *size <= 0 {
                return Err(VmError::HostError(format!(
                    "ngx::socket::tcp::receive size must be positive, got {size}"
                )));
            }
            if *size as u64 > MAX_RECEIVE_BYTES as u64 {
                return Err(VmError::HostError(format!(
                    "ngx::socket::tcp::receive size exceeds the {MAX_RECEIVE_BYTES}-byte receive cap"
                )));
            }
            Ok(ReceiveMode::Exact(*size as usize))
        }
        Value::String(text) => match text.as_str() {
            "*l" => Ok(ReceiveMode::Line),
            "*a" => Ok(ReceiveMode::All),
            other => Err(VmError::HostError(format!(
                "ngx::socket::tcp::receive pattern must be *l or *a, got {other}"
            ))),
        },
        Value::Bytes(bytes) => match bytes.as_ref().as_slice() {
            b"*l" => Ok(ReceiveMode::Line),
            b"*a" => Ok(ReceiveMode::All),
            other => Err(VmError::HostError(format!(
                "ngx::socket::tcp::receive pattern must be *l or *a, got {}",
                String::from_utf8_lossy(other)
            ))),
        },
        _ => Err(VmError::HostError(
            "ngx::socket::tcp::receive expects int, string, or null".into(),
        )),
    }
}

fn receiveany_max(max: i64) -> VmResult<usize> {
    if max <= 0 {
        return Err(VmError::HostError(format!(
            "ngx::socket::tcp::receiveany max must be positive, got {max}"
        )));
    }
    if max as u64 > MAX_RECEIVE_BYTES as u64 {
        return Err(VmError::HostError(format!(
            "ngx::socket::tcp::receiveany max exceeds the {MAX_RECEIVE_BYTES}-byte receive cap"
        )));
    }
    Ok(max as usize)
}

fn delimiter_bytes(data: &Value) -> VmResult<Vec<u8>> {
    let bytes = match data {
        Value::String(text) => text.as_bytes().to_vec(),
        Value::Bytes(bytes) => (**bytes).clone(),
        _ => {
            return Err(VmError::HostError(
                "ngx::socket::tcp::receiveuntil delimiter expects string or bytes".into(),
            ));
        }
    };
    if bytes.is_empty() {
        return Err(VmError::HostError(
            "ngx::socket::tcp::receiveuntil delimiter must not be empty".into(),
        ));
    }
    if bytes.len() > MAX_DELIMITER_BYTES {
        return Err(VmError::HostError(format!(
            "ngx::socket::tcp::receiveuntil delimiter exceeds the {MAX_DELIMITER_BYTES}-byte limit"
        )));
    }
    Ok(bytes)
}

fn live_io(inner: &TcpInner, name: &str) -> VmResult<IoHalves> {
    if inner.closed.load(Ordering::SeqCst) {
        return Err(closed_error(name));
    }
    lock(&inner.io)
        .clone()
        .ok_or_else(|| VmError::HostError(format!("{name} socket is not connected")))
}

fn drop_io(inner: &TcpInner) {
    *lock(&inner.io) = None;
    lock(&inner.unread).clear();
}

fn take_unread_prefix(inner: &TcpInner, n: usize) -> Vec<u8> {
    let mut unread = lock(&inner.unread);
    let n = n.min(unread.len());
    unread.drain(..n).collect()
}

fn find_delimiter(buf: &[u8], delim: &[u8]) -> Option<usize> {
    if delim.is_empty() || buf.len() < delim.len() {
        return None;
    }
    buf.windows(delim.len()).position(|window| window == delim)
}

fn is_timeout_error(err: &VmError) -> bool {
    matches!(err, VmError::HostError(message) if message.ends_with("timeout"))
}

enum Race<T> {
    Value(T),
    Cancelled,
    TimedOut,
}

async fn race_work<T, F>(cancel: &tokio::sync::Notify, timeout_ms: u64, work: F) -> Race<T>
where
    F: Future<Output = T>,
{
    let mut work = std::pin::pin!(work);
    let mut cancel = std::pin::pin!(cancel.notified());
    if let Some(duration) = timeout_duration(timeout_ms) {
        let mut sleep = std::pin::pin!(tokio::time::sleep(duration));
        std::future::poll_fn(|cx| {
            poll_race(cx, work.as_mut(), cancel.as_mut(), Some(sleep.as_mut()))
        })
        .await
    } else {
        std::future::poll_fn(|cx| {
            poll_race(
                cx,
                work.as_mut(),
                cancel.as_mut(),
                None::<Pin<&mut tokio::time::Sleep>>,
            )
        })
        .await
    }
}

fn poll_race<T, C, S>(
    cx: &mut Context<'_>,
    work: Pin<&mut impl Future<Output = T>>,
    mut cancel: Pin<&mut C>,
    mut sleep: Option<Pin<&mut S>>,
) -> Poll<Race<T>>
where
    C: Future<Output = ()>,
    S: Future<Output = ()>,
{
    if cancel.as_mut().poll(cx).is_ready() {
        return Poll::Ready(Race::Cancelled);
    }
    if let Some(sleep) = sleep.as_mut()
        && sleep.as_mut().poll(cx).is_ready()
    {
        return Poll::Ready(Race::TimedOut);
    }
    match work.poll(cx) {
        Poll::Ready(value) => Poll::Ready(Race::Value(value)),
        Poll::Pending => Poll::Pending,
    }
}

pub(crate) async fn with_timeout<T, F>(
    inner: &TcpInner,
    generation: u64,
    name: &str,
    work: F,
) -> VmResult<T>
where
    F: Future<Output = Result<T, io::Error>>,
{
    check_generation(inner, generation, name)?;
    let timeout_ms = inner.timeout_ms.load(Ordering::SeqCst);
    match race_work(&inner.cancel, timeout_ms, work).await {
        Race::Cancelled => Err(closed_error(name)),
        Race::TimedOut => Err(VmError::HostError(format!("{name} timeout"))),
        Race::Value(Ok(value)) => {
            check_generation(inner, generation, name)?;
            Ok(value)
        }
        Race::Value(Err(error)) => Err(VmError::HostError(format!("{name}: {error}"))),
    }
}

fn store_connected(inner: &TcpInner, reader: ReadHalf, writer: WriteHalf) {
    *lock(&inner.io) = Some(IoHalves {
        reader: Arc::new(tokio::sync::Mutex::new(reader)),
        writer: Arc::new(tokio::sync::Mutex::new(writer)),
    });
    lock(&inner.unread).clear();
}

async fn tcp_connect_socket(addr: SocketAddr) -> io::Result<tokio::net::TcpStream> {
    #[cfg(test)]
    if TEST_STALL_TCP_CONNECT.load(Ordering::SeqCst) {
        std::future::pending::<()>().await;
    }
    tokio::net::TcpStream::connect(addr).await
}

async fn unix_connect_path(path: std::path::PathBuf) -> io::Result<tokio::net::UnixStream> {
    #[cfg(test)]
    if TEST_STALL_UNIX_CONNECT.load(Ordering::SeqCst) {
        std::future::pending::<()>().await;
    }
    tokio::net::UnixStream::connect(path).await
}

async fn lookup_host_addrs(host: String, port: u16) -> io::Result<Vec<SocketAddr>> {
    #[cfg(test)]
    if TEST_STALL_DNS.load(Ordering::SeqCst) {
        std::future::pending::<()>().await;
    }
    tokio::net::lookup_host((host.as_str(), port))
        .await
        .map(|addrs| addrs.collect())
}

pub(crate) async fn connect_inner(
    inner: &TcpInner,
    generation: u64,
    host: String,
    port: i64,
    use_pool: bool,
) -> VmResult<()> {
    let name = "ngx::socket::tcp::connect";
    check_generation(inner, generation, name)?;
    if lock(&inner.io).is_some() {
        return Err(VmError::HostError(format!(
            "{name} socket is already connected"
        )));
    }

    let endpoint = if let Some(path) = unix_path(&host, port, name)? {
        PoolEndpoint::Unix {
            path: path.to_string_lossy().into_owned(),
        }
    } else {
        check_dns_host(&host, name)?;
        PoolEndpoint::Tcp {
            host: host.clone(),
            port: port_u16(port, name)?,
        }
    };

    if use_pool && let Some(conn) = pool::checkout_for_connect(&endpoint) {
        install_pooled(inner, conn, endpoint);
        return Ok(());
    }

    match &endpoint {
        PoolEndpoint::Unix { path } => {
            let path = std::path::PathBuf::from(path);
            let stream = with_timeout(inner, generation, name, unix_connect_path(path))
                .await
                .inspect_err(|_| drop_io(inner))?;
            check_generation(inner, generation, name)?;
            let (reader, writer) = stream.into_split();
            store_connected(inner, ReadHalf::Unix(reader), WriteHalf::Unix(writer));
        }
        PoolEndpoint::Tcp { host, port } => {
            let host = host.clone();
            let port = *port;
            let stream = with_timeout(inner, generation, name, async move {
                if let Ok(ip) = host.parse::<IpAddr>() {
                    return tcp_connect_socket(SocketAddr::new(ip, port)).await;
                }
                let addrs = lookup_host_addrs(host, port).await?;
                let mut last = None;
                for addr in addrs {
                    match tcp_connect_socket(addr).await {
                        Ok(stream) => return Ok(stream),
                        Err(error) => last = Some(error),
                    }
                }
                Err(last.unwrap_or_else(|| {
                    io::Error::new(io::ErrorKind::NotFound, "could not resolve")
                }))
            })
            .await
            .inspect_err(|_| drop_io(inner))?;
            check_generation(inner, generation, name)?;
            let (reader, writer) = stream.into_split();
            store_connected(inner, ReadHalf::Tcp(reader), WriteHalf::Tcp(writer));
        }
    }
    inner.reused_times.store(0, Ordering::SeqCst);
    *lock(&inner.tls) = None;
    *lock(&inner.endpoint) = Some(endpoint);
    Ok(())
}

pub(crate) async fn connect(ctx: &TcpWriteOpContext, host: String, port: i64) -> VmResult<()> {
    connect_inner(&ctx.inner, ctx.generation, host, port, true).await
}

pub(crate) async fn connect_owned(
    ctx: &TcpConnectOwnedContext,
    host: String,
    port: i64,
) -> VmResult<i64> {
    ctx.check_identity("ngx::socket::connect")?;
    connect_inner(&ctx.inner, ctx.generation, host, port, true).await?;
    Ok(ctx.handle_raw())
}

pub(crate) async fn send(ctx: &TcpWriteOpContext, data: Value) -> VmResult<()> {
    let name = "ngx::socket::tcp::send";
    ctx.check_identity(name)?;
    let payload = payload_bytes(&data, name)?;
    if payload.len() > MAX_SEND_BYTES {
        return Err(VmError::HostError(format!(
            "{name} payload exceeds the {MAX_SEND_BYTES}-byte send cap"
        )));
    }
    let io = live_io(&ctx.inner, name)?;
    let mut written = 0usize;
    while written < payload.len() {
        let n = match with_timeout(&ctx.inner, ctx.generation, name, async {
            let mut writer = io.writer.lock().await;
            writer.write(&payload[written..]).await
        })
        .await
        {
            Ok(n) => n,
            Err(error) => {
                drop_io(&ctx.inner);
                return Err(error);
            }
        };
        if n == 0 {
            drop_io(&ctx.inner);
            return Err(VmError::HostError(format!("{name}: connection closed")));
        }
        written += n;
    }
    ctx.check_identity(name)?;
    Ok(())
}

async fn read_more(
    inner: &TcpInner,
    generation: u64,
    name: &str,
    io: &IoHalves,
) -> VmResult<usize> {
    let mut buf = vec![0u8; READ_CHUNK_BYTES];
    let n = match with_timeout(inner, generation, name, async {
        let mut reader = io.reader.lock().await;
        reader.read(&mut buf).await
    })
    .await
    {
        Ok(n) => n,
        Err(error) => {
            if !is_timeout_error(&error) {
                drop_io(inner);
            }
            return Err(error);
        }
    };
    if n > 0 {
        let mut unread = lock(&inner.unread);
        if unread.len().saturating_add(n) > MAX_BUFFER_BYTES {
            drop(unread);
            drop_io(inner);
            return Err(VmError::HostError(format!(
                "{name} unread buffer exceeds the {MAX_BUFFER_BYTES}-byte cap"
            )));
        }
        unread.extend_from_slice(&buf[..n]);
    }
    Ok(n)
}

fn closed_read(name: &str) -> VmError {
    VmError::HostError(format!("{name}: closed"))
}

pub(crate) async fn receive(ctx: &TcpReadOpContext, mode: Value) -> VmResult<Value> {
    let name = "ngx::socket::tcp::receive";
    ctx.check_identity(name)?;
    let mode = receive_mode(&mode)?;
    let io = live_io(&ctx.inner, name)?;
    match mode {
        ReceiveMode::Exact(size) => loop {
            if lock(&ctx.inner.unread).len() >= size {
                return Ok(Value::bytes(take_unread_prefix(&ctx.inner, size)));
            }
            let n = read_more(&ctx.inner, ctx.generation, name, &io).await?;
            if n == 0 {
                return Err(closed_read(name));
            }
        },
        ReceiveMode::Line => loop {
            {
                let unread = lock(&ctx.inner.unread);
                if let Some(idx) = unread.iter().position(|b| *b == b'\n') {
                    let mut line: Vec<u8> = unread[..idx]
                        .iter()
                        .copied()
                        .filter(|b| *b != b'\r')
                        .collect();
                    drop(unread);
                    let _ = take_unread_prefix(&ctx.inner, idx + 1);
                    if line.len() > MAX_RECEIVE_BYTES {
                        return Err(VmError::HostError(format!(
                            "{name} line exceeds the {MAX_RECEIVE_BYTES}-byte receive cap"
                        )));
                    }
                    line.shrink_to_fit();
                    return Ok(Value::bytes(line));
                }
            }
            if lock(&ctx.inner.unread).len() > MAX_RECEIVE_BYTES {
                return Err(VmError::HostError(format!(
                    "{name} line exceeds the {MAX_RECEIVE_BYTES}-byte receive cap"
                )));
            }
            let n = read_more(&ctx.inner, ctx.generation, name, &io).await?;
            if n == 0 {
                return Err(closed_read(name));
            }
        },
        ReceiveMode::All => loop {
            if lock(&ctx.inner.unread).len() > MAX_RECEIVE_BYTES {
                return Err(VmError::HostError(format!(
                    "{name} payload exceeds the {MAX_RECEIVE_BYTES}-byte receive cap"
                )));
            }
            let n = read_more(&ctx.inner, ctx.generation, name, &io).await?;
            if n == 0 {
                let available = lock(&ctx.inner.unread).len();
                let data = take_unread_prefix(&ctx.inner, available);
                if data.len() > MAX_RECEIVE_BYTES {
                    return Err(VmError::HostError(format!(
                        "{name} payload exceeds the {MAX_RECEIVE_BYTES}-byte receive cap"
                    )));
                }
                return Ok(Value::bytes(data));
            }
        },
    }
}

pub(crate) async fn receiveany(ctx: &TcpReadOpContext, max: i64) -> VmResult<Value> {
    let name = "ngx::socket::tcp::receiveany";
    ctx.check_identity(name)?;
    let max = receiveany_max(max)?;
    if !lock(&ctx.inner.unread).is_empty() {
        return Ok(Value::bytes(take_unread_prefix(&ctx.inner, max)));
    }
    let io = live_io(&ctx.inner, name)?;
    let n = read_more(&ctx.inner, ctx.generation, name, &io).await?;
    if n == 0 {
        return Err(closed_read(name));
    }
    Ok(Value::bytes(take_unread_prefix(&ctx.inner, max)))
}

pub(crate) async fn receiveuntil(ctx: &TcpReadOpContext, delimiter: Value) -> VmResult<Value> {
    let name = "ngx::socket::tcp::receiveuntil";
    ctx.check_identity(name)?;
    let delim = delimiter_bytes(&delimiter)?;
    let io = live_io(&ctx.inner, name)?;
    loop {
        {
            let unread = lock(&ctx.inner.unread);
            if let Some(idx) = find_delimiter(&unread, &delim) {
                if idx > MAX_RECEIVE_BYTES {
                    return Err(VmError::HostError(format!(
                        "{name} payload exceeds the {MAX_RECEIVE_BYTES}-byte receive cap"
                    )));
                }
                drop(unread);
                let data = take_unread_prefix(&ctx.inner, idx);
                let _ = take_unread_prefix(&ctx.inner, delim.len());
                return Ok(Value::bytes(data));
            }
        }
        if lock(&ctx.inner.unread).len() > MAX_BUFFER_BYTES {
            return Err(VmError::HostError(format!(
                "{name} unread buffer exceeds the {MAX_BUFFER_BYTES}-byte cap"
            )));
        }
        let n = read_more(&ctx.inner, ctx.generation, name, &io).await?;
        if n == 0 {
            return Err(closed_read(name));
        }
    }
}

pub(crate) fn close(vm: &mut Vm, sock: i64) -> VmResult<()> {
    let handle = parse_handle(sock)?;
    vm.host_context()
        .close_resource::<TcpCosocket>(handle, ResourceCloseReason::Requested)
        .map_err(|error| VmError::HostError(format!("ngx::socket::tcp::close: {error}")))?;
    Ok(())
}

pub(crate) fn settimeout(vm: &mut Vm, sock: i64, ms: i64) -> VmResult<()> {
    if ms < 0 {
        return Err(VmError::HostError(format!(
            "ngx::socket::tcp::settimeout expects non-negative milliseconds, got {ms}"
        )));
    }
    let handle = parse_handle(sock)?;
    let ctx = vm.host_context();
    let resource = ctx
        .borrow_resource::<TcpCosocket>(handle)
        .map_err(host_ctx)?;
    resource
        .get()
        .inner
        .timeout_ms
        .store(ms as u64, Ordering::SeqCst);
    Ok(())
}

pub(crate) fn inner_from_sock(vm: &mut Vm, sock: i64, name: &str) -> VmResult<Arc<TcpInner>> {
    let handle = parse_handle(sock)?;
    let ctx = vm.host_context();
    let resource = ctx
        .borrow_resource::<TcpCosocket>(handle)
        .map_err(host_ctx)?;
    let inner = Arc::clone(&resource.get().inner);
    if inner.closed.load(Ordering::SeqCst) {
        return Err(closed_error(name));
    }
    Ok(inner)
}

pub(crate) fn take_pooled_io(inner: &TcpInner) -> VmResult<PooledIo> {
    if !lock(&inner.unread).is_empty() {
        return Err(VmError::HostError(
            "ngx::socket::tcp::setkeepalive refuses unread buffered data".into(),
        ));
    }
    let io = lock(&inner.io)
        .take()
        .ok_or_else(|| VmError::HostError("ngx::socket::tcp socket is not connected".into()))?;
    let IoHalves { reader, writer } = io;
    let reader = match Arc::try_unwrap(reader) {
        Ok(mutex) => mutex.into_inner(),
        Err(reader) => {
            *lock(&inner.io) = Some(IoHalves { reader, writer });
            return Err(VmError::HostError(
                "ngx::socket::tcp::setkeepalive never pools a pending operation".into(),
            ));
        }
    };
    let writer = match Arc::try_unwrap(writer) {
        Ok(mutex) => mutex.into_inner(),
        Err(writer) => {
            *lock(&inner.io) = Some(IoHalves {
                reader: Arc::new(tokio::sync::Mutex::new(reader)),
                writer,
            });
            return Err(VmError::HostError(
                "ngx::socket::tcp::setkeepalive never pools a pending operation".into(),
            ));
        }
    };
    match (reader, writer) {
        (ReadHalf::Tcp(reader), WriteHalf::Tcp(writer)) => match reader.reunite(writer) {
            Ok(stream) => Ok(PooledIo::Tcp(stream)),
            Err(err) => {
                store_connected(inner, ReadHalf::Tcp(err.0), WriteHalf::Tcp(err.1));
                Err(VmError::HostError(
                    "ngx::socket::tcp::setkeepalive failed to reunite tcp halves".into(),
                ))
            }
        },
        (ReadHalf::Unix(reader), WriteHalf::Unix(writer)) => match reader.reunite(writer) {
            Ok(stream) => Ok(PooledIo::Unix(stream)),
            Err(err) => {
                store_connected(inner, ReadHalf::Unix(err.0), WriteHalf::Unix(err.1));
                Err(VmError::HostError(
                    "ngx::socket::tcp::setkeepalive failed to reunite unix halves".into(),
                ))
            }
        },
        (ReadHalf::TlsTcp(reader), WriteHalf::TlsTcp(writer)) => {
            Ok(PooledIo::TlsTcp(reader.unsplit(writer)))
        }
        (ReadHalf::TlsUnix(reader), WriteHalf::TlsUnix(writer)) => {
            Ok(PooledIo::TlsUnix(reader.unsplit(writer)))
        }
        #[cfg(test)]
        (ReadHalf::Test(reader), WriteHalf::Test(writer)) => {
            store_connected(inner, ReadHalf::Test(reader), WriteHalf::Test(writer));
            Err(VmError::HostError(
                "ngx::socket::tcp::setkeepalive does not pool test sockets".into(),
            ))
        }
        (reader, writer) => {
            store_connected(inner, reader, writer);
            Err(VmError::HostError(
                "ngx::socket::tcp::setkeepalive mismatched io halves".into(),
            ))
        }
    }
}

pub(crate) fn fail_closed(inner: &TcpInner) {
    inner.closed.store(true, Ordering::SeqCst);
    inner.generation.fetch_add(1, Ordering::SeqCst);
    inner.pending_read.store(false, Ordering::SeqCst);
    inner.pending_write.store(false, Ordering::SeqCst);
    drop_io(inner);
    *lock(&inner.tls) = None;
    inner.cancel.notify_waiters();
}

pub(crate) fn install_pooled(inner: &TcpInner, conn: PooledConnection, endpoint: PoolEndpoint) {
    install_pooled_io_only(inner, conn.io);
    inner
        .reused_times
        .store(conn.reused_times, Ordering::SeqCst);
    *lock(&inner.tls) = conn.tls;
    *lock(&inner.endpoint) = Some(endpoint);
}

pub(crate) fn install_pooled_io_only(inner: &TcpInner, io: PooledIo) {
    match io {
        PooledIo::Tcp(stream) => {
            let (reader, writer) = stream.into_split();
            store_connected(inner, ReadHalf::Tcp(reader), WriteHalf::Tcp(writer));
        }
        PooledIo::Unix(stream) => {
            let (reader, writer) = stream.into_split();
            store_connected(inner, ReadHalf::Unix(reader), WriteHalf::Unix(writer));
        }
        PooledIo::TlsTcp(stream) => {
            let (reader, writer) = tokio::io::split(stream);
            store_connected(inner, ReadHalf::TlsTcp(reader), WriteHalf::TlsTcp(writer));
        }
        PooledIo::TlsUnix(stream) => {
            let (reader, writer) = tokio::io::split(stream);
            store_connected(inner, ReadHalf::TlsUnix(reader), WriteHalf::TlsUnix(writer));
        }
    }
}

pub(crate) fn setkeepalive(
    vm: &mut Vm,
    sock: i64,
    timeout: Value,
    pool_size: Value,
) -> VmResult<()> {
    let name = "ngx::socket::tcp::setkeepalive";
    let idle = parse_idle_timeout(&timeout, name)?;
    let size = parse_pool_size(&pool_size, name)?;
    let handle = parse_handle(sock)?;
    enum Outcome {
        Pooled,
        ClosedOk,
        FailClosed(VmError),
        FailLive(VmError),
    }
    let outcome = {
        let ctx = vm.host_context();
        let resource = ctx
            .borrow_resource::<TcpCosocket>(handle)
            .map_err(host_ctx)?;
        let inner = &resource.get().inner;
        if inner.closed.load(Ordering::SeqCst) {
            Outcome::FailLive(closed_error(name))
        } else if inner.pending_read.load(Ordering::SeqCst)
            || inner.pending_write.load(Ordering::SeqCst)
        {
            Outcome::FailLive(VmError::HostError(format!(
                "{name} never pools a pending operation"
            )))
        } else if !lock(&inner.unread).is_empty() {
            Outcome::FailLive(VmError::HostError(format!(
                "{name} refuses unread buffered data"
            )))
        } else {
            match lock(&inner.endpoint).clone() {
                None => Outcome::FailLive(VmError::HostError(format!(
                    "{name} socket is not connected"
                ))),
                Some(endpoint) => {
                    let tls = lock(&inner.tls).clone();
                    let reused = inner.reused_times.load(Ordering::SeqCst);
                    if reused >= MAX_REUSE {
                        drop_io(inner);
                        Outcome::ClosedOk
                    } else {
                        match take_pooled_io(inner) {
                            Ok(mut io) => {
                                if !io.is_alive_and_clean() {
                                    install_pooled_io_only(inner, io);
                                    Outcome::FailLive(VmError::HostError(format!(
                                        "{name} refuses a stale, closed, or dirty socket"
                                    )))
                                } else {
                                    insert_pooled(
                                        PoolKey { endpoint, tls },
                                        io,
                                        reused,
                                        idle,
                                        size,
                                    );
                                    Outcome::Pooled
                                }
                            }
                            Err(error) => {
                                if lock(&inner.io).is_none() {
                                    Outcome::FailClosed(error)
                                } else {
                                    Outcome::FailLive(error)
                                }
                            }
                        }
                    }
                }
            }
        }
    };
    match outcome {
        Outcome::Pooled | Outcome::ClosedOk => {
            vm.host_context()
                .close_resource::<TcpCosocket>(handle, ResourceCloseReason::Requested)
                .map_err(|error| VmError::HostError(format!("{name}: {error}")))?;
            Ok(())
        }
        Outcome::FailClosed(error) => {
            let _ = vm
                .host_context()
                .close_resource::<TcpCosocket>(handle, ResourceCloseReason::Requested);
            Err(error)
        }
        Outcome::FailLive(error) => Err(error),
    }
}

pub(crate) fn getreusedtimes(vm: &mut Vm, sock: i64) -> VmResult<i64> {
    let inner = inner_from_sock(vm, sock, "ngx::socket::tcp::getreusedtimes")?;
    Ok(inner.reused_times.load(Ordering::SeqCst) as i64)
}

fn parse_idle_timeout(value: &Value, name: &str) -> VmResult<Option<Duration>> {
    match value {
        Value::Null => Ok(Some(Duration::from_millis(DEFAULT_IDLE_TIMEOUT_MS))),
        Value::Int(0) => Ok(None),
        Value::Int(ms) if *ms > 0 => Ok(Some(Duration::from_millis(*ms as u64))),
        Value::Int(ms) => Err(VmError::HostError(format!(
            "{name} timeout must be non-negative, got {ms}"
        ))),
        _ => Err(VmError::TypeMismatch("keepalive timeout")),
    }
}

fn parse_pool_size(value: &Value, name: &str) -> VmResult<usize> {
    match value {
        Value::Null => Ok(DEFAULT_POOL_SIZE),
        Value::Int(size) if *size > 0 && (*size as u64) <= MAX_POOL_SIZE as u64 => {
            Ok(*size as usize)
        }
        Value::Int(0) => Err(VmError::HostError(format!(
            "{name} pool size must be greater than zero"
        ))),
        Value::Int(size) => Err(VmError::HostError(format!(
            "{name} pool size {size} exceeds the {MAX_POOL_SIZE} cap"
        ))),
        _ => Err(VmError::TypeMismatch("keepalive pool size")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::io::AsRawFd;

    #[test]
    fn timeout_zero_is_no_timer() {
        assert_eq!(timeout_duration(0), None);
        assert_eq!(timeout_duration(50), Some(Duration::from_millis(50)));
    }

    #[test]
    fn dns_host_cap_names_connect_and_limit() {
        let err = check_dns_host(&"a".repeat(256), "ngx::socket::tcp::connect")
            .expect_err("256-byte host");
        let text = err.to_string();
        assert!(text.contains("ngx::socket::tcp::connect"));
        assert!(text.contains("255"));
        check_dns_host("localhost", "ngx::socket::tcp::connect").expect("localhost");
        check_dns_host("", "ngx::socket::tcp::connect").expect_err("empty");
    }

    #[test]
    fn unix_path_requires_port_zero_and_cap() {
        let err = unix_path("unix:/tmp/x.sock", 80, "ngx::socket::tcp::connect")
            .expect_err("non-zero port");
        assert!(err.to_string().contains("unix socket port must be 0"));
        let path = unix_path("unix:/tmp/x.sock", 0, "ngx::socket::tcp::connect")
            .expect("unix")
            .expect("path");
        assert_eq!(path, Path::new("/tmp/x.sock"));
        assert!(
            unix_path("127.0.0.1", 80, "ngx::socket::tcp::connect")
                .expect("inet")
                .is_none()
        );
        let long = format!("unix:/{}", "a".repeat(108));
        let err = unix_path(&long, 0, "ngx::socket::tcp::connect").expect_err("cap");
        assert!(err.to_string().contains("107"));
    }

    #[test]
    fn receive_size_over_cap_names_the_host() {
        let err = receive_mode(&Value::Int(1_048_577)).expect_err("cap");
        let text = err.to_string();
        assert!(text.contains("ngx::socket::tcp::receive"));
        assert!(text.contains("1048576"));
    }

    #[test]
    fn delimiter_cap_names_the_host() {
        let err = delimiter_bytes(&Value::String("d".repeat(257).into())).expect_err("cap");
        let text = err.to_string();
        assert!(text.contains("ngx::socket::tcp::receiveuntil"));
        assert!(text.contains("256"));
    }

    #[test]
    fn find_delimiter_keeps_overlap_prefix() {
        assert_eq!(find_delimiter(b"xxab", b"abc"), None);
        assert_eq!(find_delimiter(b"xxabcYYY", b"abc"), Some(2));
        assert_eq!(find_delimiter(b"aaa", b"aa"), Some(0));
    }

    #[test]
    fn send_times_out_when_the_peer_does_not_read() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("listener");
        let addr = listener.local_addr().expect("addr");
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept");
            shrink_sock_buf(stream.as_raw_fd(), libc::SO_RCVBUF, 1024);
            std::thread::sleep(Duration::from_secs(2));
            drop(stream);
        });
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .enable_time()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let stream = tokio::net::TcpStream::connect(addr).await.expect("connect");
            shrink_sock_buf(stream.as_raw_fd(), libc::SO_SNDBUF, 1024);
            let sock = TcpCosocket::new();
            let inner = Arc::clone(&sock.inner);
            let (reader, writer) = stream.into_split();
            store_connected(&inner, ReadHalf::Tcp(reader), WriteHalf::Tcp(writer));
            inner.timeout_ms.store(50, Ordering::SeqCst);
            inner.pending_write.store(true, Ordering::SeqCst);
            let ctx = TcpWriteOpContext {
                inner: Arc::clone(&inner),
                generation: inner.generation.load(Ordering::SeqCst),
                _pending: PendingWriteGuard {
                    inner: Arc::clone(&inner),
                },
            };
            let err = send(&ctx, Value::bytes(vec![b'x'; 256 * 1024]))
                .await
                .expect_err("write timeout");
            let text = err.to_string();
            assert!(text.contains("ngx::socket::tcp::send"), "{text}");
            assert!(text.contains("timeout"), "{text}");
        });
        let _ = server.join();
    }

    fn shrink_sock_buf(fd: std::os::unix::io::RawFd, opt: libc::c_int, size: libc::c_int) {
        unsafe {
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                opt,
                (&size as *const libc::c_int).cast(),
                std::mem::size_of::<libc::c_int>() as libc::socklen_t,
            );
        }
    }

    #[test]
    fn receive_all_returns_unread_on_peer_close() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("listener");
        let addr = listener.local_addr().expect("addr");
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            std::io::Write::write_all(&mut stream, b"done").expect("write");
            drop(stream);
        });
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .enable_time()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let stream = tokio::net::TcpStream::connect(addr).await.expect("connect");
            let sock = TcpCosocket::new();
            let inner = Arc::clone(&sock.inner);
            let (reader, writer) = stream.into_split();
            store_connected(&inner, ReadHalf::Tcp(reader), WriteHalf::Tcp(writer));
            inner.timeout_ms.store(200, Ordering::SeqCst);
            inner.pending_read.store(true, Ordering::SeqCst);
            let ctx = TcpReadOpContext {
                inner: Arc::clone(&inner),
                generation: inner.generation.load(Ordering::SeqCst),
                _pending: PendingReadGuard {
                    inner: Arc::clone(&inner),
                },
            };
            let value = receive(&ctx, Value::bytes(b"*a".to_vec()))
                .await
                .expect("all");
            assert_eq!(value, Value::bytes(b"done".to_vec()));
        });
        let _ = server.join();
    }

    #[test]
    fn pending_read_and_write_are_independent() {
        let inner = TcpInner::new();
        assert!(!inner.pending_read.swap(true, Ordering::SeqCst));
        assert!(
            inner.pending_read.swap(true, Ordering::SeqCst),
            "duplicate read must be rejected"
        );
        assert!(
            !inner.pending_write.swap(true, Ordering::SeqCst),
            "a pending read still allows a write"
        );
        assert!(
            inner.pending_write.swap(true, Ordering::SeqCst),
            "duplicate write must be rejected"
        );
    }

    #[test]
    fn generation_is_checked_before_and_after_work() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let inner = TcpInner::new();
            inner.timeout_ms.store(1_000, Ordering::SeqCst);
            inner.generation.fetch_add(1, Ordering::SeqCst);
            let err = with_timeout(&inner, 1, "ngx::socket::tcp::receive", async { Ok(1u8) })
                .await
                .expect_err("stale generation");
            let text = err.to_string();
            assert!(text.contains("ngx::socket::tcp::receive"));
            assert!(text.contains("closed"));
        });
    }

    #[test]
    fn close_notify_cancels_pending_work_without_a_timer() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let inner = Arc::new(TcpInner::new());
            inner.timeout_ms.store(0, Ordering::SeqCst);
            inner.pending_read.store(true, Ordering::SeqCst);
            let cancel_inner = Arc::clone(&inner);
            let cancel = async move {
                tokio::time::sleep(Duration::from_millis(15)).await;
                cancel_inner.closed.store(true, Ordering::SeqCst);
                cancel_inner.generation.fetch_add(1, Ordering::SeqCst);
                cancel_inner.cancel.notify_waiters();
            };
            let work = with_timeout(&inner, 1, "ngx::socket::tcp::receive", async {
                tokio::time::sleep(Duration::from_secs(30)).await;
                Ok(())
            });
            let (result, ()) = futures_join_cancel(work, cancel).await;
            let err = result.expect_err("cancel");
            let text = err.to_string();
            assert!(text.contains("ngx::socket::tcp::receive"));
            assert!(text.contains("closed"));
        });
    }

    fn test_vm() -> Vm {
        let program =
            crate::config::compile_rss_source("use ngx;\nlet x = 1;\n").expect("compile fixture");
        let mut vm = Vm::try_new((*program).clone()).expect("vm");
        crate::host::bind_ngx_hosts(&mut vm);
        vm
    }

    fn push_tcp(vm: &mut Vm) -> i64 {
        let token = vm
            .host_context()
            .push_resource(TcpCosocket::new())
            .expect("push tcp resource");
        token.handle().raw() as i64
    }

    fn write_ctx(inner: &Arc<TcpInner>) -> TcpWriteOpContext {
        inner.pending_write.store(true, Ordering::SeqCst);
        TcpWriteOpContext {
            inner: Arc::clone(inner),
            generation: inner.generation.load(Ordering::SeqCst),
            _pending: PendingWriteGuard {
                inner: Arc::clone(inner),
            },
        }
    }

    fn read_ctx(inner: &Arc<TcpInner>) -> TcpReadOpContext {
        inner.pending_read.store(true, Ordering::SeqCst);
        TcpReadOpContext {
            inner: Arc::clone(inner),
            generation: inner.generation.load(Ordering::SeqCst),
            _pending: PendingReadGuard {
                inner: Arc::clone(inner),
            },
        }
    }

    fn store_test_io(inner: &TcpInner, reader: Arc<TestReadState>, writer: Arc<TestWriteState>) {
        store_connected(
            inner,
            ReadHalf::Test(TestReader { state: reader }),
            WriteHalf::Test(TestWriter { state: writer }),
        );
    }

    fn assert_timeout(err: &VmError, name: &str) {
        let text = err.to_string();
        assert!(text.contains(name), "{text}");
        assert!(text.contains("timeout"), "{text}");
    }

    fn assert_closed(err: &VmError, name: &str) {
        let text = err.to_string();
        assert!(text.contains(name), "{text}");
        assert!(text.contains("closed"), "{text}");
    }

    async fn poll_until_pending<T>(fut: &mut std::pin::Pin<&mut impl Future<Output = T>>) {
        std::future::poll_fn(|cx| match fut.as_mut().poll(cx) {
            Poll::Pending => Poll::Ready(()),
            Poll::Ready(_) => panic!("future completed before it was cancelled"),
        })
        .await;
    }

    fn current_runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .enable_time()
            .build()
            .expect("runtime")
    }

    #[test]
    fn capture_allows_one_read_and_one_write_and_rejects_duplicates() {
        let mut vm = test_vm();
        let handle = push_tcp(&mut vm);
        let args = [Value::Int(handle)];
        let read = TcpReadOpContext::capture_with_args(&mut vm, &args).expect("read");
        let write = TcpWriteOpContext::capture_with_args(&mut vm, &args).expect("write");
        let dup_read = match TcpReadOpContext::capture_with_args(&mut vm, &args) {
            Ok(_) => panic!("duplicate read must fail at capture"),
            Err(err) => err,
        };
        assert!(dup_read.to_string().contains("pending read"), "{dup_read}");
        let dup_write = match TcpWriteOpContext::capture_with_args(&mut vm, &args) {
            Ok(_) => panic!("duplicate write must fail at capture"),
            Err(err) => err,
        };
        assert!(
            dup_write.to_string().contains("pending write"),
            "{dup_write}"
        );
        drop(read);
        TcpReadOpContext::capture_with_args(&mut vm, &args).expect("read after first drop");
        drop(write);
        TcpWriteOpContext::capture_with_args(&mut vm, &args).expect("write after first drop");
    }

    #[test]
    fn pending_read_and_write_run_concurrently_on_a_live_socket() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("listener");
        let addr = listener.local_addr().expect("addr");
        let peer = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut buf = [0u8; 4];
            std::io::Read::read_exact(&mut stream, &mut buf).expect("peer read");
            assert_eq!(&buf, b"ping");
            std::io::Write::write_all(&mut stream, b"pong").expect("peer write");
        });
        current_runtime().block_on(async {
            let mut vm = test_vm();
            let handle = push_tcp(&mut vm);
            let args = [Value::Int(handle)];
            let connect_ctx =
                TcpWriteOpContext::capture_with_args(&mut vm, &args).expect("connect capture");
            connect(&connect_ctx, "127.0.0.1".into(), i64::from(addr.port()))
                .await
                .expect("connect");
            drop(connect_ctx);
            let read = TcpReadOpContext::capture_with_args(&mut vm, &args).expect("read");
            let write = TcpWriteOpContext::capture_with_args(&mut vm, &args).expect("write");
            let send_fut = send(&write, Value::bytes(b"ping".to_vec()));
            let recv_fut = receive(&read, Value::Int(4));
            let (sent, received) = futures_join_cancel(send_fut, recv_fut).await;
            sent.expect("concurrent send");
            assert_eq!(
                received.expect("concurrent receive"),
                Value::bytes(b"pong".to_vec())
            );
        });
        peer.join().expect("peer");
    }

    #[test]
    fn connect_times_out_when_the_handshake_does_not_complete() {
        let _stall = StallGuard::tcp_connect();
        current_runtime().block_on(async {
            let sock = TcpCosocket::new();
            let inner = Arc::clone(&sock.inner);
            inner.timeout_ms.store(50, Ordering::SeqCst);
            let ctx = write_ctx(&inner);
            let started = std::time::Instant::now();
            let err = connect(&ctx, "127.0.0.1".into(), 1)
                .await
                .expect_err("connect must not succeed");
            let elapsed = started.elapsed();
            assert_timeout(&err, "ngx::socket::tcp::connect");
            assert!(
                elapsed >= Duration::from_millis(20),
                "timeout should wait, elapsed {elapsed:?}"
            );
            assert!(
                elapsed < Duration::from_millis(1500),
                "timeout must not hang, elapsed {elapsed:?}"
            );
        });
    }

    #[test]
    fn dns_lookup_times_out_when_resolution_is_stalled() {
        let _stall = StallGuard::dns();
        current_runtime().block_on(async {
            let sock = TcpCosocket::new();
            let inner = Arc::clone(&sock.inner);
            inner.timeout_ms.store(50, Ordering::SeqCst);
            let err = connect(&write_ctx(&inner), "localhost".into(), 80)
                .await
                .expect_err("dns timeout");
            assert_timeout(&err, "ngx::socket::tcp::connect");
        });
    }

    #[test]
    fn unix_connect_times_out_when_the_handshake_is_stalled() {
        let _stall = StallGuard::unix_connect();
        current_runtime().block_on(async {
            let sock = TcpCosocket::new();
            let inner = Arc::clone(&sock.inner);
            inner.timeout_ms.store(50, Ordering::SeqCst);
            let err = connect(&write_ctx(&inner), "unix:/tmp/ngx-stall.sock".into(), 0)
                .await
                .expect_err("unix timeout");
            assert_timeout(&err, "ngx::socket::tcp::connect");
        });
    }

    #[test]
    fn instrumented_writer_forces_partial_writes_and_full_delivery() {
        current_runtime().block_on(async {
            let sock = TcpCosocket::new();
            let inner = Arc::clone(&sock.inner);
            let reader = TestReadState::new();
            let writer = TestWriteState::new(3);
            store_test_io(&inner, reader, Arc::clone(&writer));
            inner.timeout_ms.store(1000, Ordering::SeqCst);
            let payload = b"abcdefghij".to_vec();
            send(&write_ctx(&inner), Value::bytes(payload.clone()))
                .await
                .expect("send");
            assert!(
                writer.writes.load(Ordering::SeqCst) >= 2,
                "expected multiple writes, got {}",
                writer.writes.load(Ordering::SeqCst)
            );
            assert_eq!(*lock(&writer.sink), payload);
        });
    }

    #[test]
    fn receiveuntil_delimiter_overlap_spans_synchronized_reads() {
        current_runtime().block_on(async {
            let sock = TcpCosocket::new();
            let inner = Arc::clone(&sock.inner);
            let reader = TestReadState::new();
            let writer = TestWriteState::new(64);
            store_test_io(&inner, Arc::clone(&reader), writer);
            inner.timeout_ms.store(1000, Ordering::SeqCst);
            reader.push(b"xxab".to_vec());
            let value = {
                let ctx = read_ctx(&inner);
                let mut fut = std::pin::pin!(receiveuntil(&ctx, Value::bytes(b"abc".to_vec())));
                std::future::poll_fn(|cx| match fut.as_mut().poll(cx) {
                    Poll::Ready(ready) => panic!("until completed too early: {ready:?}"),
                    Poll::Pending => {
                        if reader.reads.load(Ordering::SeqCst) >= 1 {
                            Poll::Ready(())
                        } else {
                            Poll::Pending
                        }
                    }
                })
                .await;
                assert_eq!(reader.reads.load(Ordering::SeqCst), 1);
                reader.push(b"cYYY".to_vec());
                fut.await.expect("until")
            };
            assert_eq!(value, Value::bytes(b"xx".to_vec()));
            assert!(reader.reads.load(Ordering::SeqCst) >= 2);
            let rest = receiveany(&read_ctx(&inner), 16).await.expect("rest");
            assert_eq!(rest, Value::bytes(b"YYY".to_vec()));
        });
    }

    #[test]
    fn cancel_aborts_a_stalled_dns_lookup() {
        let _stall = StallGuard::dns();
        current_runtime().block_on(async {
            let sock = TcpCosocket::new();
            let inner = Arc::clone(&sock.inner);
            inner.timeout_ms.store(0, Ordering::SeqCst);
            let ctx = write_ctx(&inner);
            let mut fut = std::pin::pin!(connect(&ctx, "localhost".into(), 80));
            poll_until_pending(&mut fut).await;
            inner.cancel.notify_waiters();
            let err = fut.await.expect_err("dns cancel");
            assert_closed(&err, "ngx::socket::tcp::connect");
        });
    }

    #[test]
    fn cancel_aborts_a_stalled_connect() {
        let _stall = StallGuard::tcp_connect();
        current_runtime().block_on(async {
            let sock = TcpCosocket::new();
            let inner = Arc::clone(&sock.inner);
            inner.timeout_ms.store(0, Ordering::SeqCst);
            let ctx = write_ctx(&inner);
            let mut fut = std::pin::pin!(connect(&ctx, "127.0.0.1".into(), 1));
            poll_until_pending(&mut fut).await;
            inner.cancel.notify_waiters();
            let err = fut.await.expect_err("connect cancel");
            assert_closed(&err, "ngx::socket::tcp::connect");
        });
    }

    #[test]
    fn cancel_aborts_a_stalled_pending_write() {
        current_runtime().block_on(async {
            let sock = TcpCosocket::new();
            let inner = Arc::clone(&sock.inner);
            store_test_io(&inner, TestReadState::new(), TestWriteState::new(0));
            inner.timeout_ms.store(0, Ordering::SeqCst);
            let ctx = write_ctx(&inner);
            let mut fut = std::pin::pin!(send(&ctx, Value::bytes(b"ping".to_vec())));
            poll_until_pending(&mut fut).await;
            inner.cancel.notify_waiters();
            let err = fut.await.expect_err("write cancel");
            assert_closed(&err, "ngx::socket::tcp::send");
        });
    }

    #[test]
    fn timeout_zero_pending_connect_is_cleaned_up_by_cancel() {
        let _stall = StallGuard::tcp_connect();
        current_runtime().block_on(async {
            let sock = TcpCosocket::new();
            let inner = Arc::clone(&sock.inner);
            inner.timeout_ms.store(0, Ordering::SeqCst);
            let ctx = write_ctx(&inner);
            let mut fut = std::pin::pin!(connect(&ctx, "127.0.0.1".into(), 1));
            poll_until_pending(&mut fut).await;
            inner.closed.store(true, Ordering::SeqCst);
            inner.generation.fetch_add(1, Ordering::SeqCst);
            inner.cancel.notify_waiters();
            let err = fut.await.expect_err("timeout 0 cleanup");
            assert_closed(&err, "ngx::socket::tcp::connect");
        });
    }

    #[test]
    fn worker_shutdown_drops_a_pending_connect() {
        let _stall = StallGuard::tcp_connect();
        let mut worker = crate::runtime::WorkerRuntime::start().unwrap();
        let mut exec = worker
            .bind_access_exec(
                crate::config::compile_rss_source(
                    "use ngx;\nlet sock = ngx::socket::tcp();\nngx::socket::tcp::settimeout(sock, 60000);\nngx::socket::tcp::connect(sock, \"127.0.0.1\", 1);\n",
                )
                .expect("compile"),
                1,
            )
            .unwrap();
        let mut registry = crate::request_state::IdentityRegistry::with_capacity(
            crate::notify::DEFAULT_MAX_IN_FLIGHT,
        );
        let started = std::time::Instant::now();
        loop {
            let status = exec.handle_access(&mut registry, &worker);
            if status == crate::request_state::AccessStatus::Again {
                exec.commit_suspend();
                if matches!(exec.vm_status(), Some(vm::VmStatus::Waiting(_)))
                    && started.elapsed() > Duration::from_millis(20)
                {
                    break;
                }
            } else {
                panic!("expected wait, got {status:?}");
            }
            if started.elapsed() > Duration::from_secs(2) {
                panic!("never reached a pending connect");
            }
        }
        assert_eq!(worker.in_flight().unwrap(), 1);
        worker.shutdown();
        assert_eq!(worker.in_flight().unwrap(), 0);
        // Write futures are the same HostFuture type dropped by WorkerRuntime::shutdown
        // (src/runtime.rs). Receive is covered in tests/ngx_tcp_socket.rs; connect is
        // covered here. Pending-write cancellation is proven by the stalled AsyncWrite
        // notify test above.
    }

    async fn futures_join_cancel<A, B>(left: A, right: B) -> (A::Output, B::Output)
    where
        A: Future,
        B: Future,
    {
        let mut left = std::pin::pin!(left);
        let mut right = std::pin::pin!(right);
        let mut left_out = None;
        let mut right_out = None;
        std::future::poll_fn(|cx| {
            if left_out.is_none()
                && let Poll::Ready(value) = left.as_mut().poll(cx)
            {
                left_out = Some(value);
            }
            if right_out.is_none()
                && let Poll::Ready(value) = right.as_mut().poll(cx)
            {
                right_out = Some(value);
            }
            match (left_out.take(), right_out.take()) {
                (Some(left), Some(right)) => Poll::Ready((left, right)),
                (left, right) => {
                    left_out = left;
                    right_out = right;
                    Poll::Pending
                }
            }
        })
        .await
    }
}
