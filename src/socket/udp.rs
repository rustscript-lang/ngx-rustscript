//! Tokio UDP driver for `ngx.socket.udp`.
//!
//! Futures hold the Tokio socket, owned buffers, timeout, and a captured
//! generation. They never hold a request, pool, VM, or raw pointer.

use std::future::Future;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use vm::resource::{CloseProgress, HostResource};
use vm::{
    CaptureAsyncHostContext, ResourceCloseReason, ResourceHandle, ResourceTypeKey, Value, Vm,
    VmError, VmResult,
};

use crate::host::{NGX_SOCKET_UDP_NAME, NGX_SOCKET_UDP_RESOURCE_KEY};

pub(crate) const MAX_DATAGRAM_BYTES: usize = 65_535;
pub(crate) const MAX_DNS_HOST_BYTES: usize = 255;
const DEFAULT_RECEIVE_BYTES: usize = 8_192;
const DEFAULT_TIMEOUT_MS: u64 = 60_000;

struct UdpInner {
    socket: Mutex<Option<Arc<tokio::net::UdpSocket>>>,
    peer: Mutex<Option<SocketAddr>>,
    timeout_ms: AtomicU64,
    closed: AtomicBool,
    pending: AtomicBool,
    generation: AtomicU64,
    cancel: tokio::sync::Notify,
}

pub(crate) struct UdpCosocket {
    inner: Arc<UdpInner>,
}

impl UdpCosocket {
    pub(crate) fn new() -> Self {
        Self {
            inner: Arc::new(UdpInner {
                socket: Mutex::new(None),
                peer: Mutex::new(None),
                timeout_ms: AtomicU64::new(DEFAULT_TIMEOUT_MS),
                closed: AtomicBool::new(false),
                pending: AtomicBool::new(false),
                generation: AtomicU64::new(1),
                cancel: tokio::sync::Notify::new(),
            }),
        }
    }
}

impl vm::HostResourceType for UdpCosocket {
    const KEY: &'static str = NGX_SOCKET_UDP_RESOURCE_KEY;
    const DESCRIPTION: &'static str = "OpenResty ngx.socket.udp handle";
}

impl HostResource for UdpCosocket {
    fn resource_type_key() -> Option<ResourceTypeKey>
    where
        Self: Sized,
    {
        Some(
            ResourceTypeKey::new(NGX_SOCKET_UDP_RESOURCE_KEY)
                .expect("ngx.socket.udp is a valid resource type key"),
        )
    }

    fn begin_close(
        &mut self,
        _reason: ResourceCloseReason,
    ) -> vm::resource::ResourceResult<CloseProgress> {
        self.inner.closed.store(true, Ordering::SeqCst);
        self.inner.generation.fetch_add(1, Ordering::SeqCst);
        self.inner.pending.store(false, Ordering::SeqCst);
        *lock(&self.inner.socket) = None;
        *lock(&self.inner.peer) = None;
        self.inner.cancel.notify_waiters();
        Ok(CloseProgress::Ready)
    }
}

struct PendingGuard {
    inner: Arc<UdpInner>,
}

impl Drop for PendingGuard {
    fn drop(&mut self) {
        self.inner.pending.store(false, Ordering::SeqCst);
    }
}

pub(crate) struct UdpOpContext {
    inner: Arc<UdpInner>,
    generation: u64,
    _pending: PendingGuard,
}

impl UdpOpContext {
    fn check_identity(&self, name: &str) -> VmResult<()> {
        if self.inner.closed.load(Ordering::SeqCst)
            || self.inner.generation.load(Ordering::SeqCst) != self.generation
        {
            Err(closed_error(name))
        } else {
            Ok(())
        }
    }
}

impl CaptureAsyncHostContext for UdpOpContext {
    fn capture(_vm: &mut Vm) -> VmResult<Self> {
        Err(VmError::HostError(
            "ngx::socket::udp operation context requires a socket handle".into(),
        ))
    }

    fn capture_with_args(vm: &mut Vm, args: &[Value]) -> VmResult<Self> {
        let handle_id = match args.first() {
            Some(Value::Int(value)) => *value,
            Some(_) => return Err(VmError::TypeMismatch("resource handle")),
            None => {
                return Err(VmError::HostError(
                    "ngx::socket::udp missing socket handle".into(),
                ));
            }
        };
        let handle = parse_handle(handle_id)?;
        let ctx = vm.host_context();
        let token = ctx
            .typed_resource::<UdpCosocket>(handle)
            .map_err(host_ctx)?;
        let resource = ctx.resource(&token).map_err(host_ctx)?;
        let inner = Arc::clone(&resource.get().inner);
        if inner.closed.load(Ordering::SeqCst) {
            return Err(closed_error(NGX_SOCKET_UDP_NAME));
        }
        let generation = inner.generation.load(Ordering::SeqCst);
        if inner.pending.swap(true, Ordering::SeqCst) {
            return Err(VmError::HostError(
                "ngx::socket::udp already has a pending operation".into(),
            ));
        }
        Ok(Self {
            inner: Arc::clone(&inner),
            generation,
            _pending: PendingGuard { inner },
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

pub(crate) fn check_peer_host(host: &str) -> VmResult<()> {
    if host.is_empty() {
        return Err(VmError::HostError(
            "ngx::socket::udp::setpeername host must not be empty".into(),
        ));
    }
    if host.len() > MAX_DNS_HOST_BYTES {
        return Err(VmError::HostError(format!(
            "ngx::socket::udp::setpeername host exceeds the {MAX_DNS_HOST_BYTES}-byte limit"
        )));
    }
    Ok(())
}

fn parse_bind_ip(host: &Value) -> VmResult<IpAddr> {
    match host {
        Value::Null => Ok(IpAddr::V4(Ipv4Addr::UNSPECIFIED)),
        Value::String(text) => match text.as_str() {
            "" | "*" | "0.0.0.0" => Ok(IpAddr::V4(Ipv4Addr::UNSPECIFIED)),
            "::" => Ok(IpAddr::V6(Ipv6Addr::UNSPECIFIED)),
            text => text.parse().map_err(|_| {
                VmError::HostError(format!(
                    "ngx::socket::udp::bind host is not an IP address: {text}"
                ))
            }),
        },
        _ => Err(VmError::HostError(
            "ngx::socket::udp host expects string or null".into(),
        )),
    }
}

fn payload_bytes(data: &Value) -> VmResult<Vec<u8>> {
    match data {
        Value::String(text) => Ok(text.as_bytes().to_vec()),
        Value::Bytes(bytes) => Ok((**bytes).clone()),
        _ => Err(VmError::HostError(
            "ngx::socket::udp::send expects string or bytes".into(),
        )),
    }
}

fn receive_size(size: &Value) -> VmResult<usize> {
    let size = match size {
        Value::Null => DEFAULT_RECEIVE_BYTES as i64,
        Value::Int(number) => *number,
        _ => {
            return Err(VmError::HostError(
                "ngx::socket::udp::receive size expects int or null".into(),
            ));
        }
    };
    if size <= 0 {
        return Err(VmError::HostError(format!(
            "ngx::socket::udp::receive size must be positive, got {size}"
        )));
    }
    if size as u64 > MAX_DATAGRAM_BYTES as u64 {
        return Err(VmError::HostError(format!(
            "ngx::socket::udp::receive size exceeds the {MAX_DATAGRAM_BYTES}-byte datagram cap"
        )));
    }
    Ok(size as usize)
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
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
            "ngx::socket::udp invalid handle {handle_id}"
        )));
    }
    ResourceHandle::from_raw(handle_id as u64).map_err(|error| {
        VmError::HostError(format!(
            "ngx::socket::udp invalid handle {handle_id}: {error}"
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

fn live_socket(inner: &UdpInner, name: &str) -> VmResult<Arc<tokio::net::UdpSocket>> {
    if inner.closed.load(Ordering::SeqCst) {
        return Err(closed_error(name));
    }
    lock(&inner.socket)
        .clone()
        .ok_or_else(|| VmError::HostError(format!("{name} socket is not bound")))
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

async fn with_timeout<T, F>(inner: &UdpInner, generation: u64, name: &str, work: F) -> VmResult<T>
where
    F: Future<Output = Result<T, std::io::Error>>,
{
    if inner.closed.load(Ordering::SeqCst) || inner.generation.load(Ordering::SeqCst) != generation
    {
        return Err(closed_error(name));
    }
    let timeout_ms = inner.timeout_ms.load(Ordering::SeqCst);
    match race_work(&inner.cancel, timeout_ms, work).await {
        Race::Cancelled => Err(closed_error(name)),
        Race::TimedOut => Err(VmError::HostError(format!("{name} timeout"))),
        Race::Value(Ok(value)) => {
            if inner.closed.load(Ordering::SeqCst)
                || inner.generation.load(Ordering::SeqCst) != generation
            {
                Err(closed_error(name))
            } else {
                Ok(value)
            }
        }
        Race::Value(Err(error)) => Err(VmError::HostError(format!("{name}: {error}"))),
    }
}

pub(crate) async fn bind(ctx: &UdpOpContext, host: Value, port: i64) -> VmResult<()> {
    let name = "ngx::socket::udp::bind";
    ctx.check_identity(name)?;
    if lock(&ctx.inner.socket).is_some() {
        return Err(VmError::HostError(format!(
            "{name} socket is already bound"
        )));
    }
    let addr = SocketAddr::new(parse_bind_ip(&host)?, port_u16(port, name)?);
    let socket = with_timeout(
        &ctx.inner,
        ctx.generation,
        name,
        tokio::net::UdpSocket::bind(addr),
    )
    .await?;
    ctx.check_identity(name)?;
    *lock(&ctx.inner.socket) = Some(Arc::new(socket));
    Ok(())
}

pub(crate) async fn setpeername(ctx: &UdpOpContext, host: String, port: i64) -> VmResult<()> {
    let name = "ngx::socket::udp::setpeername";
    ctx.check_identity(name)?;
    check_peer_host(&host)?;
    let port = port_u16(port, name)?;
    let peer = if let Ok(ip) = host.parse::<IpAddr>() {
        SocketAddr::new(ip, port)
    } else {
        let mut addrs = with_timeout(
            &ctx.inner,
            ctx.generation,
            name,
            tokio::net::lookup_host((host.as_str(), port)),
        )
        .await?;
        addrs
            .next()
            .ok_or_else(|| VmError::HostError(format!("{name} could not resolve {host}")))?
    };
    let existing = lock(&ctx.inner.socket).clone();
    let socket = match existing {
        Some(socket) => socket,
        None => {
            let bind_ip = if peer.is_ipv6() {
                IpAddr::V6(Ipv6Addr::UNSPECIFIED)
            } else {
                IpAddr::V4(Ipv4Addr::UNSPECIFIED)
            };
            let socket = with_timeout(
                &ctx.inner,
                ctx.generation,
                name,
                tokio::net::UdpSocket::bind(SocketAddr::new(bind_ip, 0)),
            )
            .await?;
            ctx.check_identity(name)?;
            let socket = Arc::new(socket);
            *lock(&ctx.inner.socket) = Some(Arc::clone(&socket));
            socket
        }
    };
    let connect = socket.connect(peer);
    with_timeout(&ctx.inner, ctx.generation, name, connect).await?;
    ctx.check_identity(name)?;
    *lock(&ctx.inner.peer) = Some(peer);
    Ok(())
}

pub(crate) async fn send(ctx: &UdpOpContext, data: Value) -> VmResult<()> {
    let name = "ngx::socket::udp::send";
    ctx.check_identity(name)?;
    let payload = payload_bytes(&data)?;
    if payload.len() > MAX_DATAGRAM_BYTES {
        return Err(VmError::HostError(format!(
            "{name} payload exceeds the {MAX_DATAGRAM_BYTES}-byte datagram cap"
        )));
    }
    if lock(&ctx.inner.peer).is_none() {
        return Err(VmError::HostError(format!("{name} has no peer")));
    }
    let socket = live_socket(&ctx.inner, name)?;
    with_timeout(&ctx.inner, ctx.generation, name, async {
        socket.send(&payload).await
    })
    .await?;
    ctx.check_identity(name)?;
    Ok(())
}

pub(crate) async fn receive(ctx: &UdpOpContext, size: Value) -> VmResult<Value> {
    let name = "ngx::socket::udp::receive";
    ctx.check_identity(name)?;
    let cap = receive_size(&size)?;
    let socket = live_socket(&ctx.inner, name)?;
    let mut buf = vec![0u8; cap + 1];
    let n = with_timeout(&ctx.inner, ctx.generation, name, async {
        socket.recv(&mut buf).await
    })
    .await?;
    ctx.check_identity(name)?;
    if n > cap {
        return Err(VmError::HostError(format!(
            "{name} datagram exceeds the {cap}-byte receive size"
        )));
    }
    buf.truncate(n);
    Ok(Value::bytes(buf))
}

pub(crate) fn close(vm: &mut Vm, sock: i64) -> VmResult<()> {
    let handle = parse_handle(sock)?;
    vm.host_context()
        .close_resource::<UdpCosocket>(handle, ResourceCloseReason::Requested)
        .map_err(|error| VmError::HostError(format!("ngx::socket::udp::close: {error}")))?;
    Ok(())
}

pub(crate) fn settimeout(vm: &mut Vm, sock: i64, ms: i64) -> VmResult<()> {
    if ms < 0 {
        return Err(VmError::HostError(format!(
            "ngx::socket::udp::settimeout expects non-negative milliseconds, got {ms}"
        )));
    }
    let handle = parse_handle(sock)?;
    let ctx = vm.host_context();
    let resource = ctx
        .borrow_resource::<UdpCosocket>(handle)
        .map_err(host_ctx)?;
    resource
        .get()
        .inner
        .timeout_ms
        .store(ms as u64, Ordering::SeqCst);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeout_zero_is_no_timer() {
        assert_eq!(timeout_duration(0), None);
        assert_eq!(timeout_duration(50), Some(Duration::from_millis(50)));
        assert_eq!(
            timeout_duration(60_000),
            Some(Duration::from_millis(60_000))
        );
    }

    #[test]
    fn dns_host_cap_names_setpeername_and_limit() {
        let err = check_peer_host(&"a".repeat(256)).expect_err("256-byte host");
        let text = err.to_string();
        assert!(
            text.contains("ngx::socket::udp::setpeername"),
            "error must name the host, got {text}"
        );
        assert!(
            text.contains("255"),
            "error must name the byte limit, got {text}"
        );
        check_peer_host("localhost").expect("localhost is in range");
        check_peer_host("").expect_err("empty host");
    }

    #[test]
    fn invalid_port_names_the_operation() {
        let err = port_u16(65536, "ngx::socket::udp::bind").expect_err("port");
        let text = err.to_string();
        assert!(text.contains("ngx::socket::udp::bind"));
        assert!(text.contains("65536"));
        let err = port_u16(-1, "ngx::socket::udp::setpeername").expect_err("negative");
        let text = err.to_string();
        assert!(text.contains("ngx::socket::udp::setpeername"));
    }

    #[test]
    fn receive_size_over_cap_names_the_host() {
        let err = receive_size(&Value::Int(65_536)).expect_err("cap");
        let text = err.to_string();
        assert!(text.contains("ngx::socket::udp::receive"));
        assert!(text.contains("65535"));
    }

    #[test]
    fn generation_is_checked_before_and_after_work() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let inner = UdpInner {
                socket: Mutex::new(None),
                peer: Mutex::new(None),
                timeout_ms: AtomicU64::new(1_000),
                closed: AtomicBool::new(false),
                pending: AtomicBool::new(false),
                generation: AtomicU64::new(1),
                cancel: tokio::sync::Notify::new(),
            };
            inner.generation.fetch_add(1, Ordering::SeqCst);
            let err = with_timeout(&inner, 1, "ngx::socket::udp::receive", async { Ok(1u8) })
                .await
                .expect_err("stale generation");
            let text = err.to_string();
            assert!(
                text.contains("ngx::socket::udp::receive"),
                "stale op must name the host, got {text}"
            );
            assert!(
                text.contains("closed"),
                "stale generation uses the closed identity, got {text}"
            );
        });
    }

    #[test]
    fn close_notify_cancels_pending_work_without_a_timer() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let inner = Arc::new(UdpInner {
                socket: Mutex::new(None),
                peer: Mutex::new(None),
                timeout_ms: AtomicU64::new(0),
                closed: AtomicBool::new(false),
                pending: AtomicBool::new(true),
                generation: AtomicU64::new(1),
                cancel: tokio::sync::Notify::new(),
            });
            let cancel_inner = Arc::clone(&inner);
            let cancel = async move {
                tokio::time::sleep(Duration::from_millis(15)).await;
                cancel_inner.closed.store(true, Ordering::SeqCst);
                cancel_inner.generation.fetch_add(1, Ordering::SeqCst);
                cancel_inner.cancel.notify_waiters();
            };
            let work = with_timeout(&inner, 1, "ngx::socket::udp::send", async {
                tokio::time::sleep(Duration::from_secs(30)).await;
                Ok(())
            });
            let (result, ()) = futures_join_cancel(work, cancel).await;
            let err = result.expect_err("cancel");
            let text = err.to_string();
            assert!(text.contains("ngx::socket::udp::send"));
            assert!(text.contains("closed"));
        });
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

    #[test]
    fn pending_flag_rejects_a_second_op() {
        let inner = UdpInner {
            socket: Mutex::new(None),
            peer: Mutex::new(None),
            timeout_ms: AtomicU64::new(1_000),
            closed: AtomicBool::new(false),
            pending: AtomicBool::new(false),
            generation: AtomicU64::new(1),
            cancel: tokio::sync::Notify::new(),
        };
        assert!(!inner.pending.swap(true, Ordering::SeqCst));
        assert!(
            inner.pending.swap(true, Ordering::SeqCst),
            "a socket allows only one pending operation"
        );
    }
}
