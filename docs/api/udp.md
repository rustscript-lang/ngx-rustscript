# UDP cosocket

[Documentation](../index.md) · [Directives](../directives.md) · [API](index.md) · [Execution model](../execution-model.md) · [Security](../security.md) · [Examples](../examples/index.md) · [Compatibility](../ngx-api-compatibility.md)

`ngx.socket.udp` is adapted as a typed VM resource plus seven explicit
hosts. The constructor returns a `resource` handle (`ngx.socket.udp`).
Async I/O futures hold only a Tokio `UdpSocket`, owned buffer, timeout,
and a generation captured at operation start — never a request, pool, VM,
or raw pointer. Generation is checked before and after each await and on
close. Request cleanup cancels the pending future and closes the
resource (including the UDP fd). Default receive size is 8192 bytes;
datagrams and requested sizes above 65535 bytes are rejected. Timeout is
milliseconds (OpenResty); `0` disables the timer rather than substituting
a year-long deadline. DNS for `setpeername` uses Tokio, not the nginx
resolver, and host names longer than 255 bytes are rejected before lookup.

| OpenResty API | RustScript callable | Status | Phases | Yield | Limits | Semantic differences | Acceptance |
| --- | --- | --- | --- | --- | --- | --- | --- |
| `ngx.socket.udp` | `ngx::socket::udp() -> resource` | adapted | `PHASE_SOCKET_UDP` | sync | VM resource table | Typed handle, not userdata. Cosocket request-owned even though sync. | `tests/ngx_udp_socket.rs`, `tests/socket_lifecycle.rs` |
| `udpsock:bind` | `ngx::socket::udp::bind(sock, host, port) -> ()` | adapted | `PHASE_SOCKET_UDP` | Waiting | host string/`()`; port 0–65535 | `()`/`*` bind any IPv4. | `tests/ngx_udp_socket.rs`, `tests/socket_lifecycle.rs` |
| `udpsock:setpeername` | `ngx::socket::udp::setpeername(sock, host, port) -> ()` | adapted | `PHASE_SOCKET_UDP` | Waiting | host string ≤255 bytes; port 0–65535 | Tokio DNS; auto-bind if needed. | `tests/ngx_udp_socket.rs`, `tests/socket_lifecycle.rs` |
| `udpsock:send` | `ngx::socket::udp::send(sock, data) -> ()` | adapted | `PHASE_SOCKET_UDP` | Waiting | 65535-byte payload; string or bytes | Requires `setpeername`. | `tests/ngx_udp_socket.rs`, `tests/socket_lifecycle.rs` |
| `udpsock:receive` | `ngx::socket::udp::receive(sock, size) -> bytes` | adapted | `PHASE_SOCKET_UDP` | Waiting | default 8192; max 65535; `()` size | Oversized datagrams error instead of silent truncate. | `tests/ngx_udp_socket.rs`, `tests/socket_lifecycle.rs` |
| `udpsock:close` | `ngx::socket::udp::close(sock) -> ()` | adapted | `PHASE_SOCKET_UDP` | sync | — | Double close is HostError. Cosocket request-owned. | `tests/ngx_udp_socket.rs`, `tests/socket_lifecycle.rs` |
| `udpsock:settimeout` | `ngx::socket::udp::settimeout(sock, ms) -> ()` | adapted | `PHASE_SOCKET_UDP` | sync | non-negative ms; `0` = no timer | Applies to subsequent ops; default 60000. | `tests/ngx_udp_socket.rs`, `tests/socket_lifecycle.rs` |

`PHASE_SOCKET_UDP` (OpenResty v0.10.29 `ngx.socket.udp` contexts):
`rewrite`, `access`, `content`, `timer`, `ssl_certificate`,
`ssl_session_fetch`. Same as sleep.

UDP programs are request-owned (`HostFamily::Cosocket`), including the
sync constructor, `close`, and `settimeout`. Timeout, cancel, shutdown,
generation, DNS, IPv6, workers, and handle reuse after close are covered
by `tests/ngx_udp_socket.rs` and `tests/socket_lifecycle.rs`. NGINX
acceptance is `tests/nginx_integration.py`.


---

See also: [API index](index.md) · [Phase sets](index.md#phase-sets) · [Cross-cutting adaptations](index.md#cross-cutting-adaptations) · [Hard caps](index.md#hard-caps)
