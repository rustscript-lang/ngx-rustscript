# TCP cosocket

[Documentation](../index.md) · [Directives](../directives.md) · [API](index.md) · [Execution model](../execution-model.md) · [Security](../security.md) · [Examples](../examples/index.md) · [Compatibility](../ngx-api-compatibility.md)

`ngx.socket.tcp` is adapted as a typed VM resource plus sixteen explicit
hosts (methods plus convenience `ngx.socket.connect` and TLS credential
constructors). The
constructor returns a `resource` handle (`ngx.socket.tcp`). Async I/O
futures hold only Tokio TCP/Unix stream halves, owned unread buffer,
timeout, generation, and cancel identity — never a request, pool, VM,
or raw pointer. Generation is checked before and after each await and
on close. Request cleanup cancels the pending future and closes the
resource (including the fd). One pending read and one pending write may
run concurrently; a second same-direction op is a structured host
error. Send loops until the full payload is written. Receive modes are
exact size, `()`/`"*l"` line (CR ignored), and `"*a"` until peer
close. `receiveany` returns at most `max` bytes. `receiveuntil` is
exclusive of a bounded delimiter and preserves unread bytes, including
overlap across fragments. Caps: DNS host 255 bytes, Unix path 107
bytes, send/receive/unread buffer 1048576 bytes, delimiter 256 bytes.
Timeout is milliseconds (OpenResty); `0` disables the timer. A transport
timeout on send/receive is a `HostError` (`"{name} timeout"`). The
socket stays connected and reusable (generation and fd survive), but
under current VM policy a `HostError` still aborts the current
script/request (`Error500`). This is not OpenResty's `nil, err`
continuation. Connect timeout is also a `HostError` and drops the
in-progress handshake. DNS and connect use Tokio, not the nginx
resolver. The raw fd is never exposed.

| OpenResty API | RustScript callable | Status | Phases | Yield | Limits | Semantic differences | Acceptance |
| --- | --- | --- | --- | --- | --- | --- | --- |
| `ngx.socket.tcp` | `ngx::socket::tcp() -> resource` | adapted | `PHASE_SOCKET_UDP` | sync | VM resource table | Typed handle, not userdata. Cosocket request-owned even though sync. | `tests/ngx_tcp_socket.rs`, `tests/socket_lifecycle.rs` |
| `tcpsock:connect` | `ngx::socket::tcp::connect(sock, host, port) -> ()` | adapted | `PHASE_SOCKET_UDP` | Waiting | host ≤255; `unix:` path ≤107 with port 0; port 0–65535 | Tokio DNS/connect; IPv4/IPv6/Unix. | `tests/ngx_tcp_socket.rs`, `tests/socket_lifecycle.rs` |
| `ngx.socket.connect` | `ngx::socket::connect(host, port) -> resource` | adapted | `PHASE_SOCKET_UDP` | Waiting | same as connect | Creates the TCP resource then connects. | `tests/ngx_tcp_socket.rs` |
| `tcpsock:send` | `ngx::socket::tcp::send(sock, data) -> ()` | adapted | `PHASE_SOCKET_UDP` | Waiting | 1048576-byte payload; string or bytes | Partial-write loop until complete. Transport timeout is HostError; the socket stays connected, but HostError aborts the current script/request. | `src/socket/tcp.rs`, `tests/ngx_tcp_socket.rs` |
| `tcpsock:receive` | `ngx::socket::tcp::receive(sock, mode) -> bytes` | adapted | `PHASE_SOCKET_UDP` | Waiting | exact/`()`/`*l`/`*a`; max 1048576 | Line ignores CR. `*a` succeeds on peer close. Transport timeout is HostError; the socket stays connected, but HostError aborts the current script/request. | `src/socket/tcp.rs`, `tests/ngx_tcp_socket.rs` |
| `tcpsock:receiveany` | `ngx::socket::tcp::receiveany(sock, max) -> bytes` | adapted | `PHASE_SOCKET_UDP` | Waiting | 1–1048576 | Returns unread first; one kernel read otherwise. | `tests/ngx_tcp_socket.rs` |
| `tcpsock:receiveuntil` | `ngx::socket::tcp::receiveuntil(sock, delimiter) -> bytes` | adapted | `PHASE_SOCKET_UDP` | Waiting | delimiter 1–256 bytes | Single-shot, exclusive delimiter, unread preserved. | `tests/ngx_tcp_socket.rs` |
| `tcpsock:close` | `ngx::socket::tcp::close(sock) -> ()` | adapted | `PHASE_SOCKET_UDP` | sync | — | Double close is HostError. Cosocket request-owned. | `tests/ngx_tcp_socket.rs` |
| `tcpsock:settimeout` | `ngx::socket::tcp::settimeout(sock, ms) -> ()` | adapted | `PHASE_SOCKET_UDP` | sync | non-negative ms; `0` = no timer | Applies to subsequent ops; default 60000. | `tests/ngx_tcp_socket.rs` |
| `tcpsock:setkeepalive` | `ngx::socket::tcp::setkeepalive(sock, timeout, pool_size) -> ()` | adapted | `PHASE_SOCKET_UDP` | sync | timeout ms (`()`=60000, `0`=never); pool_size `()`=30, 1–256; max 32 keys / 256 conns / 1000 reuse | Worker-local LRU pool keyed by endpoint+TLS identity. Detaches IO from the request so cancel/close cannot drop a pooled socket. Rejects pending ops, unread buffers, EOF, and dirty sockets. | `tests/ngx_tcp_keepalive.rs` |
| `tcpsock:getreusedtimes` | `ngx::socket::tcp::getreusedtimes(sock) -> int` | adapted | `PHASE_SOCKET_UDP` | sync | — | 0 until checkout; closed is HostError. | `tests/ngx_tcp_keepalive.rs` |
| `tcpsock:sslhandshake` | `ngx::socket::tcp::sslhandshake(sock, reused_session, server_name, ssl_verify, send_status_req) -> ()` | adapted | `PHASE_SOCKET_UDP` | Waiting | SNI ≤255; verify defaults true | Tokio/rustls only. No NGINX SSL FFI. Session userdata is not returned; `reused_session` session handles are a structured unsupported error. `()`/`true` keep automatic same-identity rustls resumption; `false` disables it. rustls always sends OCSP `status_request`; `send_status_req=false` is unsupported. TLS pool checkout happens only during handshake after endpoint+SNI+verify+trust+client identity match. | `tests/ngx_tcp_tls.rs` |
| — | `ngx::socket::tcp::trustedstore(pem) -> resource` | adapted | `PHASE_SOCKET_UDP` | sync | PEM ≤65536 | Parses PEM into a VM-owned trusted-store resource. PEM is never logged. | `tests/ngx_tcp_tls.rs` |
| `tcpsock:settrustedstore` | `ngx::socket::tcp::settrustedstore(sock, store) -> ()` | adapted | `PHASE_SOCKET_UDP` | sync | typed store resource | Attaches a trusted-store resource. Default is system roots then webpki-roots. | `tests/ngx_tcp_tls.rs` |
| — | `ngx::socket::tcp::clientcert(cert, pkey) -> resource` | adapted | `PHASE_SOCKET_UDP` | sync | PEM ≤65536 each | Parses cert+key into a VM-owned resource. Private key is parsed through zeroizing storage and retained only as rustls `PrivateKeyDer`. Cert/key mismatch fails at construction. | `tests/ngx_tcp_tls.rs` |
| `tcpsock:setclientcert` | `ngx::socket::tcp::setclientcert(sock, cert) -> ()` | adapted | `PHASE_SOCKET_UDP` | sync | typed clientcert resource | Attaches a client-cert resource. OpenResty two-PEM method is adapted to one typed resource. | `tests/ngx_tcp_tls.rs` |

TCP programs are request-owned (`HostFamily::Cosocket`). Lifecycle,
timeout, cancel, shutdown, DNS, IPv4/IPv6/Unix, concurrent
read+write / duplicate same-direction ops, and handle reuse are covered
by `tests/ngx_tcp_socket.rs` and `tests/socket_lifecycle.rs`. Keepalive
pool, worker isolation, and TLS verification are covered by
`tests/ngx_tcp_keepalive.rs` and `tests/ngx_tcp_tls.rs`. NGINX
acceptance is `tests/nginx_integration.py`.


---

See also: [API index](index.md) · [Phase sets](index.md#phase-sets) · [Cross-cutting adaptations](index.md#cross-cutting-adaptations) · [Hard caps](index.md#hard-caps)
