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
(the 191 ngx hosts plus pd-vm's HTTP import catalog) and then
`validate_program_imports_for_phase(..., Access)`.
`compile_rss_source_for_phase` performs the corresponding Content or
Log validation. Unknown names, phase-forbidden names, arity, and type
mismatches fail distinctly at `nginx -t` / reload.

`ngx::thread::{spawn,wait,kill}`, `ngx::timer::{at,every}`,
`ngx::on_abort`, `ngx::semaphore::*`, and `ngx::run_worker_thread` are
not production hosts; configuration compile rejects those imports.

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
