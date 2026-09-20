# ngx-rustscript documentation

[Documentation](index.md) · [Directives](directives.md) · [API](api/index.md) · [Execution model](execution-model.md) · [Security](security.md) · [Examples](examples/index.md) · [Compatibility](ngx-api-compatibility.md)

NGINX dynamic HTTP module that hosts RustScript (`pd-vm`) at the access,
content, and log phases. Scripts compile once at configuration time.
The crate README is the build and install quick start; this tree is the
directive, API, and runtime reference.

The layout follows the lua-nginx-module information architecture:
directives are one index, APIs are another, with syntax/context/phase
metadata on each directive. The prose is ngx-rustscript's own.

## Start here

| Page | Contents |
| --- | --- |
| [README](../README.md) | Requirements, OpenResty 1.31.4.1 ABI, build, release packages, tests |
| [Directives](directives.md) | `access_by_rss_file`, `content_by_rss_file`, `log_by_rss_file`, shared dict, I/O/HTTP/SQLite policy |
| [API reference](api/index.md) | 199 ngx hosts, split by family, plus pd-vm async I/O/HTTP/SQLite |
| [Execution model](execution-model.md) | Worker runtime, VM pool vs request-owned VM, Waiting/eventfd, log phase |
| [Security and policy](security.md) | Denied-by-default grants, one HTTP allow pair, SQLite root, fail-closed log |
| [Examples](examples/index.md) | Rate limiter, HMAC API-key auth, async pipeline |
| [Compatibility](ngx-api-compatibility.md) | OpenResty v0.10.29 mapping, typed results, out of scope |
| [Host descriptors](host-descriptors.md) | Module descriptors, typed resources, named envelopes, retained Unknown slots |
| [Scheduler design](design/ngx-scheduler.md) | Callback execution contract: blocked thread/`on_abort`/`run_worker_thread` families, plus the landed semaphore and timer families and their lifecycle |

## Runnable phases

| Directive | NGINX phase | Yield |
| --- | --- | --- |
| `access_by_rss_file` | access | sync or `Waiting` → `NGX_AGAIN` |
| `content_by_rss_file` | content | sync or `Waiting` → `NGX_AGAIN` |
| `log_by_rss_file` | log | sync only; `Waiting` is fail-closed |

Catalog phase names other than `access`, `content`, and `log` record
OpenResty allowlists for later directives. They are not runnable entry
points yet. See [phase sets](api/index.md#phase-sets).

## Facts that apply everywhere

- OpenResty **1.31.4.1** with NGINX core **1.31.4** (`nginx_version` 1031004).
- Production ngx host count is **199**: the 191 callable-family baseline
  plus `ngx::semaphore::{new,wait,post,count}` (worker-owned state, no
  script callables) and `ngx::timer::{at,every,pending_count,running_count}`
  (worker-owned fake requests with one owned callback each). Typed fixed result structs
  (`SharedStoreResult`, `DecodedArgsResult`, …) are the script-facing
  envelopes; dynamic-key collections stay `map` fields inside those
  envelopes.
- pd-vm I/O, HTTP, and SQLite start denied. A location may grant one
  `rss_http_allow <host> <port>` pair. `rss_sqlite_root` is required
  even for an in-memory database.
- `log_by_rss_file` never waits and never mutates the already-sent
  response.
