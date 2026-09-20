# Compatibility and migration

[Documentation](index.md) · [Directives](directives.md) · [API](api/index.md) · [Execution model](execution-model.md) · [Security](security.md) · [Examples](examples/index.md) · [Compatibility](ngx-api-compatibility.md)

This is an OpenResty-*oriented* RustScript API. It does not run Lua and
documents only the hosts in the [API reference](api/index.md) (199 ngx names, plus
pd-vm I/O/HTTP/SQLite). Host-by-host tables, phase sets, and hard caps
live there; this page is the compatibility mapping.

**Built and tested against official NGINX 1.31.4**
(`nginx_version` 1031004), ngx-rust `cda9d8372ccd2e139ef2b03e5806e019e7796187`, and the
pinned `pd-vm` revision in the crate README. Executable directives are
`access_by_rss_file`, `content_by_rss_file`, and `log_by_rss_file`.

## OpenResty documentation baseline

Release A phase lists and API names follow lua-nginx-module **v0.10.29**:

- Tagged README: [lua-nginx-module v0.10.29 Nginx API for Lua](https://github.com/openresty/lua-nginx-module/blob/v0.10.29/README.markdown#nginx-api-for-lua)
- Exact snapshot used when the host manifest was authored: [lua-nginx-module at 8374a7beb642af43715a05a33b2cc277fba29c4d](https://github.com/openresty/lua-nginx-module/blob/8374a7beb642af43715a05a33b2cc277fba29c4d/README.markdown#nginx-api-for-lua)

The same README is also the [Directives](https://github.com/openresty/lua-nginx-module/blob/v0.10.29/README.markdown#directives)
index (`**syntax:**` / `**context:**` / `**phase:**` on each directive).
A split browseable copy is
[OpenResty Reference](https://openresty-reference.readthedocs.io/en/latest/)
([Directives](https://openresty-reference.readthedocs.io/en/latest/Directives/),
[Lua Ngx API](https://openresty-reference.readthedocs.io/en/latest/Lua_Nginx_API/)).

Do not treat later OpenResty APIs, stream-lua, or undocumented Lua
helpers as part of this module.

## Status: supported vs adapted

| Status | Meaning |
| --- | --- |
| `supported` | Same call shape and result kind as lua-nginx-module v0.10.29 |
| `adapted` | Intentional RustScript mapping (namespacing, fixed structs, `null` optionals, bytes vs string) |

The pinned compiler matches host arity exactly. OpenResty optional
trailing arguments are an explicit `null` argument, not an omitted
argument.

## Typed fixed results and dynamic maps

Multi-return OpenResty values use **fixed result structs** at the
RustScript boundary. The current host catalog exposes several of those
return slots as `unknown`, so callers declare the matching struct and
annotate the assignment; field access then uses `.value` / `.flags` /
`.stale` / `.ok` / `.error` / `.forcible`. The VM's internal carrier is
not part of the script-facing type.

Truly **dynamic-key** collections remain `map` fields inside typed
envelopes, such as `DecodedArgsResult.args` and
`RequestHeadersResult.headers`.

```rss
struct DecodedArgsResult {
    args: map,
    truncated: bool
}

struct RequestHeadersResult {
    headers: map,
    truncated: bool
}

struct RequestArgsResult {
    args: map,
    truncated: bool
}

struct SharedGetResult {
    value: unknown?,
    flags: int,
    stale: bool,
    error: string?
}

struct SharedStoreResult {
    ok: bool,
    error: string?,
    forcible: bool
}

struct SharedSafeStoreResult {
    ok: bool,
    error: string?
}

struct SharedIncrResult {
    value: number?,
    error: string?,
    forcible: bool
}

struct SharedNumberResult {
    value: number?,
    error: string?
}
```

Example:

```rss
let added: SharedSafeStoreResult =
    ngx::shared::safe_add(dict, key, 1, WINDOW_SECONDS, null);
if added.ok {
    // ...
}
```

## Namespacing and constants

Lua fields and constants become zero-argument functions:
`ngx.OK` → `ngx::core::ok()`, `ngx.HTTP_GET` → `ngx::http::get()`,
`ngx.config.prefix` → `ngx::config::prefix()`,
`ngx.worker.pid` → `ngx::worker::pid()`. There are no host constants
or dynamic table fields. `ngx.null` is `ngx::core::null_value() -> null`
because `null` is a reserved token.

See [Constants](api/constants.md) and
[cross-cutting adaptations](api/index.md#cross-cutting-adaptations).

## Directives vs lua-nginx-module

| lua-nginx-module | ngx-rustscript |
| --- | --- |
| `*_by_lua_block` / `*_by_lua_file` across many phases | `access_by_rss_file`, `content_by_rss_file`, `log_by_rss_file` only |
| `lua_shared_dict` | `rss_shared_dict` |
| Lua package path / code cache directives | not present; scripts compile at `nginx -t` / reload |
| `lua_need_request_body` | `ngx::req::read_body()` from rewrite/access/content |

Catalog phase names other than `access`, `content`, and `log` document
OpenResty allowlists for later directives. They are not runnable entry
points in this matrix.

## Async I/O / HTTP / SQLite policy

The module enables the pinned pd-vm `async`, `http-client`, and
`sqlite` features. Access and content may import those hosts; log
rejects them. External access is denied until location grants:

- `rss_io_root` / `rss_io_write`
- `rss_http_scheme`, **one** `rss_http_allow <host> <port>` pair per
  location, `rss_http_allow_private`
- `rss_sqlite_root` even for an in-memory database

A second HTTP pair is rejected during configuration. Process I/O and
unsafe SQL remain disabled. Details:
[Security](security.md), [Directives](directives.md),
[standard async hosts](api/standard-async.md).

## Out of scope

Not production hosts in this matrix (do not import them):

- `ngx.exec` / internal redirect (`set_uri jump=true`)
- Light threads and abort callbacks: `ngx::thread::{spawn,wait,kill}`,
  `ngx::on_abort`, `ngx::run_worker_thread`. Configuration compile rejects
  those names in access, content, and log (`UnsupportedImport`). The execution
  contract and upstream `pd-vm` gap are in
  [docs/design/ngx-scheduler.md](design/ngx-scheduler.md).
- `ngx::semaphore::{new,wait,post,count}` **are** production hosts: a
  worker-owned counting semaphore reached through a VM-local
  `resource<ngx.semaphore>` handle, waiting on the generic `HostFuture`
  bridge without any script callable. The bare `ngx::semaphore` namespace
  marker is not a callable and is rejected. Access and content only; Log
  fails configuration compile. Registered count **199**. See
  [semaphore](api/semaphore.md) and
  [docs/design/ngx-scheduler.md](design/ngx-scheduler.md).
- `ngx.timer.{at,every,pending_count,running_count}` are adapted as
  `ngx::timer::{at,every,pending_count,running_count}`. The registration
  hosts consume their callback through `HostParamPassing::TakeOwned` and run
  it later on a timer-owned VM that outlives the creating request. The family
  is bound through the timer-host registry (owned dispatch has no name-binding
  API) and is therefore not part of the 195-name per-name bind list. Each
  registration owns one ordinary `RequestIdentity` whose fake request id is
  tagged out of the request-id space, and every `every` round re-arms under a
  fresh generation so stale wakes cannot start a round early. Access and
  content only; Log fails configuration compile. Registered count **199**
  (191 + 4 semaphore + 4 timer). See [timer](api/timer.md) and
  [docs/design/ngx-scheduler.md](design/ngx-scheduler.md).
- Stream subsystem, `ndk.set_var.*`, Lua `coroutine.*`
- `tcpsock:getsslpointer` / `tcpsock:getsslctx` (no NGINX SSL FFI)
- `print`, `runtime::sleep`, and test hosts `ngx::probe` / `ngx::ready` /
  `ngx::fail`

A timer callback round runs under `NgxPhase::Timer` with a log-only
`AllowedOps` allowlist: log, `ngx::sleep`/`sleep_ms`, cosocket I/O, the shared
dictionary, the semaphore family, and the timer family work, while request,
response, ctx, and control hosts fail closed and are reported through the
worker log. The standard I/O, HTTP, and SQLite policies are per-VM; a callback
VM does not inherit the registering location's policy.

`log_by_rss_file` is the executable Log-phase directive: synchronous
and fail-closed. Config uses a closed positive allowlist (family +
capability + name). Codec is included because OpenResty documents those
APIs for `log_by_lua*`. Response getters are included from the same
OpenResty context tables; setters stay rejected. The handler runs when
NGINX invokes the log phase for the current request (`r->loc_conf`
after rewrite / `error_page`). Subrequests require core
`log_subrequest on` on the subrequest location
(`ngx_http_finalize_request` in NGINX 1.31.4). `Waiting` logs
`log handler cannot wait`, drops the VM, and never arms eventfd, a
timer, or `r->main->count`.

## Maintaining the matrix

Update the relevant [API family page](api/index.md) whenever a
production manifest API, catalog signature, allowed phase, limit, yield
behavior (sync vs Waiting), or semantic adaptation changes. Keep the
inventory on the API index in lockstep. Cross-check
`ngx_host_manifest()`, `ngx_host_catalog()`, and
`production_runtime_host_names()` so no production host is omitted and
no extra name is documented.
