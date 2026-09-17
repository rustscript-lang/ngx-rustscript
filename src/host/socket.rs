// OpenResty-oriented UDP/TCP cosocket host wrappers.
//
// Drivers, generation identity, DNS, and timeouts live in
// `crate::socket::{udp,tcp}`. These wrappers only bind the pd-host-function
// surface. Async futures still hold no request, pool, VM, or raw pointer.

use pd_host_function::pd_host_function;
use vm::HostModuleDescriptor;
use vm::{Value, Vm};

use super::super::VmResult;
use super::{CallOutcome, IntoHostCallOutcome};

use crate::socket::tcp::{
    self, TcpConnectOwnedContext, TcpCosocket, TcpHandshakeOpContext, TcpReadOpContext,
    TcpWriteOpContext,
};
use crate::socket::tls;
use crate::socket::udp::{self, UdpCosocket, UdpOpContext};

fn ret<T: IntoHostCallOutcome>(value: T) -> VmResult<CallOutcome> {
    Ok(value.into_host_call_outcome())
}

fn host_ctx(error: vm::HostContextError) -> vm::VmError {
    vm::VmError::HostError(error.to_string())
}

/// Allocates an unbound UDP cosocket resource handle.
#[pd_host_function(name = "ngx::socket::udp", contract = crate::host::catalog::ngx_socket_udp_contract)]
pub fn udp(vm: &mut Vm) -> VmResult<CallOutcome> {
    let token = vm
        .host_context()
        .push_resource(UdpCosocket::new())
        .map_err(host_ctx)?;
    ret(token.handle().raw() as i64)
}

/// Binds the UDP socket to a local address. `host` is a string or `null`.
#[pd_host_function(name = "ngx::socket::udp::bind", contract = crate::host::catalog::ngx_socket_udp_bind_contract)]
pub async fn bind(
    #[pd_host_context] ctx: UdpOpContext,
    _sock: i64,
    host: Value,
    port: i64,
) -> VmResult<()> {
    udp::bind(&ctx, host, port).await
}

/// Sets the default peer, resolving host names through Tokio DNS.
#[pd_host_function(name = "ngx::socket::udp::setpeername", contract = crate::host::catalog::ngx_socket_udp_setpeername_contract)]
pub async fn setpeername(
    #[pd_host_context] ctx: UdpOpContext,
    _sock: i64,
    host: String,
    port: i64,
) -> VmResult<()> {
    udp::setpeername(&ctx, host, port).await
}

/// Sends one datagram to the connected peer.
#[pd_host_function(name = "ngx::socket::udp::send", contract = crate::host::catalog::ngx_socket_udp_send_contract)]
pub async fn send(
    #[pd_host_context] ctx: UdpOpContext,
    _sock: i64,
    data: Value,
) -> VmResult<()> {
    udp::send(&ctx, data).await
}

/// Receives one datagram. `size` is an int or `null` (default 8192).
#[pd_host_function(name = "ngx::socket::udp::receive", contract = crate::host::catalog::ngx_socket_udp_receive_contract)]
pub async fn receive(
    #[pd_host_context] ctx: UdpOpContext,
    _sock: i64,
    size: Value,
) -> VmResult<Value> {
    udp::receive(&ctx, size).await
}

/// Closes the UDP resource. A second close is an error.
#[pd_host_function(name = "ngx::socket::udp::close", contract = crate::host::catalog::ngx_socket_udp_close_contract)]
pub fn close(vm: &mut Vm, sock: i64) -> VmResult<CallOutcome> {
    udp::close(vm, sock)?;
    ret(())
}

/// Sets the timeout in milliseconds for subsequent UDP operations.
#[pd_host_function(name = "ngx::socket::udp::settimeout", contract = crate::host::catalog::ngx_socket_udp_settimeout_contract)]
pub fn settimeout(vm: &mut Vm, sock: i64, ms: i64) -> VmResult<CallOutcome> {
    udp::settimeout(vm, sock, ms)?;
    ret(())
}

/// Allocates an unconnected TCP cosocket resource handle.
#[pd_host_function(name = "ngx::socket::tcp", contract = crate::host::catalog::ngx_socket_tcp_contract)]
pub fn tcp(vm: &mut Vm) -> VmResult<CallOutcome> {
    let token = vm
        .host_context()
        .push_resource(TcpCosocket::new())
        .map_err(host_ctx)?;
    ret(token.handle().raw() as i64)
}

/// Connects an existing TCP resource. IPv4/IPv6 via Tokio DNS; `unix:` paths
/// with port 0 for Unix sockets.
#[pd_host_function(name = "ngx::socket::tcp::connect", contract = crate::host::catalog::ngx_socket_tcp_connect_contract)]
pub async fn tcp_connect(
    #[pd_host_context] ctx: TcpWriteOpContext,
    _sock: i64,
    host: String,
    port: i64,
) -> VmResult<()> {
    tcp::connect(&ctx, host, port).await
}

/// Convenience constructor: create, connect, return the TCP resource.
#[pd_host_function(name = "ngx::socket::connect", contract = crate::host::catalog::ngx_socket_connect_contract)]
pub async fn connect(
    #[pd_host_context] ctx: TcpConnectOwnedContext,
    host: String,
    port: i64,
) -> VmResult<i64> {
    tcp::connect_owned(&ctx, host, port).await
}

/// Sends the full payload, looping until every byte is written.
#[pd_host_function(name = "ngx::socket::tcp::send", contract = crate::host::catalog::ngx_socket_tcp_send_contract)]
pub async fn tcp_send(
    #[pd_host_context] ctx: TcpWriteOpContext,
    _sock: i64,
    data: Value,
) -> VmResult<()> {
    tcp::send(&ctx, data).await
}

/// Receives by mode: exact size, `null`/`"*l"` line, or `"*a"` until close.
#[pd_host_function(name = "ngx::socket::tcp::receive", contract = crate::host::catalog::ngx_socket_tcp_receive_contract)]
pub async fn tcp_receive(
    #[pd_host_context] ctx: TcpReadOpContext,
    _sock: i64,
    mode: Value,
) -> VmResult<Value> {
    tcp::receive(&ctx, mode).await
}

/// Receives at most `max` bytes without waiting for a full buffer.
#[pd_host_function(name = "ngx::socket::tcp::receiveany", contract = crate::host::catalog::ngx_socket_tcp_receiveany_contract)]
pub async fn tcp_receiveany(
    #[pd_host_context] ctx: TcpReadOpContext,
    _sock: i64,
    max: i64,
) -> VmResult<Value> {
    tcp::receiveany(&ctx, max).await
}

/// Receives until `delimiter`, exclusive of the delimiter, preserving unread.
#[pd_host_function(name = "ngx::socket::tcp::receiveuntil", contract = crate::host::catalog::ngx_socket_tcp_receiveuntil_contract)]
pub async fn tcp_receiveuntil(
    #[pd_host_context] ctx: TcpReadOpContext,
    _sock: i64,
    delimiter: Value,
) -> VmResult<Value> {
    tcp::receiveuntil(&ctx, delimiter).await
}

/// Closes the TCP resource. A second close is an error.
#[pd_host_function(name = "ngx::socket::tcp::close", contract = crate::host::catalog::ngx_socket_tcp_close_contract)]
pub fn tcp_close(vm: &mut Vm, sock: i64) -> VmResult<CallOutcome> {
    tcp::close(vm, sock)?;
    ret(())
}

/// Sets the timeout in milliseconds for subsequent TCP operations.
#[pd_host_function(name = "ngx::socket::tcp::settimeout", contract = crate::host::catalog::ngx_socket_tcp_settimeout_contract)]
pub fn tcp_settimeout(vm: &mut Vm, sock: i64, ms: i64) -> VmResult<CallOutcome> {
    tcp::settimeout(vm, sock, ms)?;
    ret(())
}

/// Puts a connected TCP/TLS socket into the worker-local keepalive pool.
#[pd_host_function(name = "ngx::socket::tcp::setkeepalive", contract = crate::host::catalog::ngx_socket_tcp_setkeepalive_contract)]
pub fn tcp_setkeepalive(
    vm: &mut Vm,
    sock: i64,
    timeout: Value,
    pool_size: Value,
) -> VmResult<CallOutcome> {
    tcp::setkeepalive(vm, sock, timeout, pool_size)?;
    ret(())
}

/// Returns how many times this socket was reused from the keepalive pool.
#[pd_host_function(name = "ngx::socket::tcp::getreusedtimes", contract = crate::host::catalog::ngx_socket_tcp_getreusedtimes_contract)]
pub fn tcp_getreusedtimes(vm: &mut Vm, sock: i64) -> VmResult<CallOutcome> {
    ret(tcp::getreusedtimes(vm, sock)?)
}

/// Performs a rustls client handshake on a connected TCP socket.
#[pd_host_function(name = "ngx::socket::tcp::sslhandshake", contract = crate::host::catalog::ngx_socket_tcp_sslhandshake_contract)]
pub async fn tcp_sslhandshake(
    #[pd_host_context] ctx: TcpHandshakeOpContext,
    _sock: i64,
    reused_session: Value,
    server_name: Value,
    ssl_verify: Value,
    send_status_req: Value,
) -> VmResult<()> {
    tls::sslhandshake(&ctx, reused_session, server_name, ssl_verify, send_status_req).await
}

/// Installs a typed trusted-store resource on a TCP socket.
#[pd_host_function(name = "ngx::socket::tcp::settrustedstore", contract = crate::host::catalog::ngx_socket_tcp_settrustedstore_contract)]
pub fn tcp_settrustedstore(vm: &mut Vm, sock: i64, store: i64) -> VmResult<CallOutcome> {
    tls::settrustedstore(vm, sock, store)?;
    ret(())
}

/// Installs a typed client-cert resource on a TCP socket.
#[pd_host_function(name = "ngx::socket::tcp::setclientcert", contract = crate::host::catalog::ngx_socket_tcp_setclientcert_contract)]
pub fn tcp_setclientcert(vm: &mut Vm, sock: i64, cert: i64) -> VmResult<CallOutcome> {
    tls::setclientcert(vm, sock, cert)?;
    ret(())
}

/// Parses PEM into a VM-owned trusted-store resource.
#[pd_host_function(name = "ngx::socket::tcp::trustedstore", contract = crate::host::catalog::ngx_socket_tcp_trustedstore_contract)]
pub fn tcp_trustedstore(vm: &mut Vm, pem: Value) -> VmResult<CallOutcome> {
    let store = tls::trustedstore_from_pem(pem)?;
    let token = vm.host_context().push_resource(store).map_err(host_ctx)?;
    ret(token.handle().raw() as i64)
}

/// Parses a client certificate and private key into a VM-owned resource.
#[pd_host_function(name = "ngx::socket::tcp::clientcert", contract = crate::host::catalog::ngx_socket_tcp_clientcert_contract)]
pub fn tcp_clientcert(vm: &mut Vm, cert: Value, pkey: Value) -> VmResult<CallOutcome> {
    let client = tls::clientcert_from_pem(cert, pkey)?;
    let token = vm.host_context().push_resource(client).map_err(host_ctx)?;
    ret(token.handle().raw() as i64)
}


fn tcp_socket_resource() -> vm::HostResourceTypeMeta {
    vm::HostResourceTypeMeta::of::<crate::socket::tcp::TcpCosocket>()
}

fn udp_socket_resource() -> vm::HostResourceTypeMeta {
    vm::HostResourceTypeMeta::of::<crate::socket::udp::UdpCosocket>()
}

fn tls_trustedstore_resource() -> vm::HostResourceTypeMeta {
    vm::HostResourceTypeMeta::of::<crate::socket::tls::TrustedStore>()
}

fn tls_clientcert_resource() -> vm::HostResourceTypeMeta {
    vm::HostResourceTypeMeta::of::<crate::socket::tls::ClientCert>()
}

const SOCKET_HOST_MODULE_FUNCTIONS: &[fn() -> vm::HostFunctionDescriptor] = &[
        udp_descriptor,
        bind_descriptor,
        setpeername_descriptor,
        send_descriptor,
        receive_descriptor,
        close_descriptor,
        settimeout_descriptor,
        tcp_descriptor,
        tcp_connect_descriptor,
        connect_descriptor,
        tcp_send_descriptor,
        tcp_receive_descriptor,
        tcp_receiveany_descriptor,
        tcp_receiveuntil_descriptor,
        tcp_close_descriptor,
        tcp_settimeout_descriptor,
        tcp_setkeepalive_descriptor,
        tcp_getreusedtimes_descriptor,
        tcp_sslhandshake_descriptor,
        tcp_settrustedstore_descriptor,
        tcp_setclientcert_descriptor,
        tcp_trustedstore_descriptor,
        tcp_clientcert_descriptor,
];

pub fn socket_host_module() -> HostModuleDescriptor {
    HostModuleDescriptor {
        name: "ngx.socket",
        functions: SOCKET_HOST_MODULE_FUNCTIONS,
        resources: &[tcp_socket_resource, udp_socket_resource, tls_trustedstore_resource, tls_clientcert_resource],
    }
}
