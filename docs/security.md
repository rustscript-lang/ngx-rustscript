# Security and policy

[Documentation](index.md) · [Directives](directives.md) · [API](api/index.md) · [Execution model](execution-model.md) · [Security](security.md) · [Examples](examples/index.md) · [Compatibility](ngx-api-compatibility.md)

External capability policy for ngx-rustscript. Directive syntax is in
[Directives](directives.md). Host details are in
[pd-vm standard async hosts](api/standard-async.md).

## Denied by default

pd-vm I/O, HTTP client, and SQLite start denied per location until
configuration grants them:

| Domain | Grant | Still disabled |
| --- | --- | --- |
| File I/O | `rss_io_root <path>`, `rss_io_write on\|off` | process spawning |
| HTTP client | `rss_http_scheme http\|https`, one `rss_http_allow <host> <port>` pair, `rss_http_allow_private on\|off` | any host/port not in that single pair |
| SQLite | `rss_sqlite_root <path>` (required even for memory) | unsafe SQL |

These policy fields inherit independently. A child location that sets a
field replaces the inherited value for that field.

Paths resolve with `ngx_conf_full_name`. pd-vm default byte, timeout,
connection, row, parameter, and result limits remain active.

## One HTTP allow pair

A location accepts one `rss_http_allow <host> <port>` pair. A second
pair is rejected during `nginx -t` (`duplicate rss_http_allow directive
in this location`). pd-vm stores schemes, hosts, and ports as separate
lists; a single pair prevents those lists from admitting unintended
host×port combinations.

`rss_http_allow_private on` is required for loopback/private addresses
and should remain off for ordinary public upstreams. Repeat
`rss_http_scheme` to allow both `http` and `https`.

## SQLite root, including memory

Any SQLite use, including an in-memory database, requires
`rss_sqlite_root`. File-backed scripts then pass a non-empty relative
database path under that root. Without the root, the phase handler
reports that SQLite is disabled until `rss_sqlite_root` is configured.

## Log phase cannot suspend

`log_by_rss_file` rejects I/O, HTTP, and SQLite at configuration time
because the log handler cannot wait. Sleep, cosocket, output, control,
request writes, and response setters are also rejected. Access and
content scripts use the eventfd resume path.

The log handler is fail-closed: runtime errors and panics are logged
and never change the already-sent response. `Waiting` logs
`log handler cannot wait` and drops the VM.

## Compile-time import allowlists

`compile_rss_source` compiles access scripts against `compile_host_catalog()`
— exactly the 199-name `ngx_host_catalog()`, with no pd-vm family merged in —
and then `validate_program_imports_for_phase(..., Access)`.
`compile_rss_source_for_phase` performs the corresponding Content or
Log validation. Unknown names, phase-forbidden names, arity, and type
mismatches fail distinctly at `nginx -t` / reload.

pd-vm's standard HTTP imports are not part of that catalog. The pinned pd-vm
HTTP catalog declares its request/response positions as named records
(`HttpRequest` / `HttpResponse` / SSE records) and registers its adapters
byte-for-byte from that same catalog, so a merged compile-time schema could
never both match the documented loose `map` scripts and stay registered.
`http::client::*` therefore compiles dynamically as an ordinary host import
against the documented `map` interface: configuration validates the name
(`is_standard_host_import`), and the pinned pd-vm adapter validates the
concrete request contents at runtime and returns the pinned named-record
contract payload shapes unchanged (duplicate header entries, `HttpHeaderValue`
text/bytes variants). The dynamic standard import binds through the staged
standard composition (I/O and SQLite are compiler builtins dispatched through
that same composition); see [pd-vm standard async hosts](api/standard-async.md).
The standard host domains are rejected in `log_by_rss_file`.

`ngx::timer::{at,every,pending_count,running_count}` is landed: registration
is accepted in Access and Content and rejected in Log at configuration compile
(`PhaseForbidden`); the bare `ngx::timer` marker is not a callable. Timer
callbacks run under a log-only `AllowedOps` allowlist with no live request:
log, sleep, outbound async I/O, the shared dictionary, the semaphore family,
and the timer family itself work; request/response/`ngx.ctx` access fails
closed; and a callback VM does not inherit the creating location's
standard-host grants. `ngx::thread::{spawn,wait,kill}`, `ngx::on_abort`, and
`ngx::run_worker_thread` are not production hosts; configuration compile
rejects those imports. The production
`ngx::semaphore::{new,wait,post,count}` family uses a worker-owned registry:
sync-only calls can run on a pooled VM, while async `wait` uses the
request-owned host-future bridge. Its VM-local handle is closed on reset or
drop, and worker shutdown closes the state and wakes parked waiters.

## Request isolation

Host futures and wakers own only a queue sender and request identity.
No NGINX request/pool/VM pointers cross into Tokio. Cosocket and
read-body futures check a generation before and after each await.
Client disconnect during a wait finalizes HTTP 499 and drops the host
future. `ngx.ctx` values are request-owned, deep-cloned, and isolated
across concurrent requests.

## Shared dictionary

`rss_shared_dict` is NGINX shared memory: one slab, one rbtree/LRU, one
mutex, visible to every worker. `safe_*` never evicts; LRU eviction
reports `forcible`. Fail-closed examples should treat `no memory` and
`forcible` as errors. See [Shared dictionary](api/shared-dict.md) and
the [rate-limiter example](examples/rate-limit.md).
