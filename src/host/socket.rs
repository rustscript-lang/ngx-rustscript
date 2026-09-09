// OpenResty-oriented UDP/TCP cosocket host wrappers.
//
// Drivers, generation identity, DNS, and timeouts live in
// `crate::socket::{udp,tcp}`. These wrappers only bind the pd-host-function
// surface. Async futures still hold no request, pool, VM, or raw pointer.

use pd_host_function::pd_host_function;
use vm::{Value, Vm};

use super::super::VmResult;
use super::{CallOutcome, IntoHostCallOutcome};

use crate::socket::tcp::{
    self, TcpConnectOwnedContext, TcpCosocket, TcpReadOpContext, TcpWriteOpContext,
};
use crate::socket::udp::{self, UdpCosocket, UdpOpContext};

fn ret<T: IntoHostCallOutcome>(value: T) -> VmResult<CallOutcome> {
    Ok(value.into_host_call_outcome())
}

fn host_ctx(error: vm::HostContextError) -> vm::VmError {
    vm::VmError::HostError(error.to_string())
}

/// Allocates an unbound UDP cosocket resource handle.
#[pd_host_function(name = "ngx::socket::udp")]
pub fn udp(vm: &mut Vm) -> VmResult<CallOutcome> {
    let token = vm
        .host_context()
        .push_resource(UdpCosocket::new())
        .map_err(host_ctx)?;
    ret(token.handle().raw() as i64)
}

/// Binds the UDP socket to a local address. `host` is a string or `null`.
#[pd_host_function(name = "ngx::socket::udp::bind")]
pub async fn bind(
    #[pd_host_context] ctx: UdpOpContext,
    _sock: i64,
    host: Value,
    port: i64,
) -> VmResult<()> {
    udp::bind(&ctx, host, port).await
}

/// Sets the default peer, resolving host names through Tokio DNS.
#[pd_host_function(name = "ngx::socket::udp::setpeername")]
pub async fn setpeername(
    #[pd_host_context] ctx: UdpOpContext,
    _sock: i64,
    host: String,
    port: i64,
) -> VmResult<()> {
    udp::setpeername(&ctx, host, port).await
}

/// Sends one datagram to the connected peer.
#[pd_host_function(name = "ngx::socket::udp::send")]
pub async fn send(
    #[pd_host_context] ctx: UdpOpContext,
    _sock: i64,
    data: Value,
) -> VmResult<()> {
    udp::send(&ctx, data).await
}

/// Receives one datagram. `size` is an int or `null` (default 8192).
#[pd_host_function(name = "ngx::socket::udp::receive")]
pub async fn receive(
    #[pd_host_context] ctx: UdpOpContext,
    _sock: i64,
    size: Value,
) -> VmResult<Value> {
    udp::receive(&ctx, size).await
}

/// Closes the UDP resource. A second close is an error.
#[pd_host_function(name = "ngx::socket::udp::close")]
pub fn close(vm: &mut Vm, sock: i64) -> VmResult<CallOutcome> {
    udp::close(vm, sock)?;
    ret(())
}

/// Sets the timeout in milliseconds for subsequent UDP operations.
#[pd_host_function(name = "ngx::socket::udp::settimeout")]
pub fn settimeout(vm: &mut Vm, sock: i64, ms: i64) -> VmResult<CallOutcome> {
    udp::settimeout(vm, sock, ms)?;
    ret(())
}

/// Allocates an unconnected TCP cosocket resource handle.
#[pd_host_function(name = "ngx::socket::tcp")]
pub fn tcp(vm: &mut Vm) -> VmResult<CallOutcome> {
    let token = vm
        .host_context()
        .push_resource(TcpCosocket::new())
        .map_err(host_ctx)?;
    ret(token.handle().raw() as i64)
}

/// Connects an existing TCP resource. IPv4/IPv6 via Tokio DNS; `unix:` paths
/// with port 0 for Unix sockets.
#[pd_host_function(name = "ngx::socket::tcp::connect")]
pub async fn tcp_connect(
    #[pd_host_context] ctx: TcpWriteOpContext,
    _sock: i64,
    host: String,
    port: i64,
) -> VmResult<()> {
    tcp::connect(&ctx, host, port).await
}

/// Convenience constructor: create, connect, return the TCP resource.
#[pd_host_function(name = "ngx::socket::connect")]
pub async fn connect(
    #[pd_host_context] ctx: TcpConnectOwnedContext,
    host: String,
    port: i64,
) -> VmResult<i64> {
    tcp::connect_owned(&ctx, host, port).await
}

/// Sends the full payload, looping until every byte is written.
#[pd_host_function(name = "ngx::socket::tcp::send")]
pub async fn tcp_send(
    #[pd_host_context] ctx: TcpWriteOpContext,
    _sock: i64,
    data: Value,
) -> VmResult<()> {
    tcp::send(&ctx, data).await
}

/// Receives by mode: exact size, `null`/`"*l"` line, or `"*a"` until close.
#[pd_host_function(name = "ngx::socket::tcp::receive")]
pub async fn tcp_receive(
    #[pd_host_context] ctx: TcpReadOpContext,
    _sock: i64,
    mode: Value,
) -> VmResult<Value> {
    tcp::receive(&ctx, mode).await
}

/// Receives at most `max` bytes without waiting for a full buffer.
#[pd_host_function(name = "ngx::socket::tcp::receiveany")]
pub async fn tcp_receiveany(
    #[pd_host_context] ctx: TcpReadOpContext,
    _sock: i64,
    max: i64,
) -> VmResult<Value> {
    tcp::receiveany(&ctx, max).await
}

/// Receives until `delimiter`, exclusive of the delimiter, preserving unread.
#[pd_host_function(name = "ngx::socket::tcp::receiveuntil")]
pub async fn tcp_receiveuntil(
    #[pd_host_context] ctx: TcpReadOpContext,
    _sock: i64,
    delimiter: Value,
) -> VmResult<Value> {
    tcp::receiveuntil(&ctx, delimiter).await
}

/// Closes the TCP resource. A second close is an error.
#[pd_host_function(name = "ngx::socket::tcp::close")]
pub fn tcp_close(vm: &mut Vm, sock: i64) -> VmResult<CallOutcome> {
    tcp::close(vm, sock)?;
    ret(())
}

/// Sets the timeout in milliseconds for subsequent TCP operations.
#[pd_host_function(name = "ngx::socket::tcp::settimeout")]
pub fn tcp_settimeout(vm: &mut Vm, sock: i64, ms: i64) -> VmResult<CallOutcome> {
    tcp::settimeout(vm, sock, ms)?;
    ret(())
}
