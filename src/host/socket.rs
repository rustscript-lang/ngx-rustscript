// OpenResty-oriented UDP cosocket host wrappers.
//
// The Tokio driver, generation identity, DNS, and timeouts live in
// `crate::socket::udp`. These wrappers only bind the pd-host-function
// surface. Async futures still hold no request, pool, VM, or raw pointer.

use pd_host_function::pd_host_function;
use vm::{Value, Vm};

use super::super::VmResult;
use super::{CallOutcome, IntoHostCallOutcome};

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
