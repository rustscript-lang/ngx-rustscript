# API reference

[Documentation](../index.md) · [Directives](../directives.md) · [API](index.md) · [Execution model](../execution-model.md) · [Security](../security.md) · [Examples](../examples/index.md) · [Compatibility](../ngx-api-compatibility.md)

This matrix is the authoritative current ngx host surface for
`ngx-rustscript`. It is derived from the production manifest
(`src/host/catalog.rs` `ngx_host_manifest()`), the compiler catalog
(`ngx_host_catalog()`), and the runtime bind list
(`src/host/mod.rs` `production_runtime_host_names()` / `bind_ngx_hosts`).
The manifest and the compiler catalog contain the same **199** names in the
same order; the per-name runtime bind list has **195** ordered non-timer
entries.

The owned `ngx::timer::{at,every}` registrations have no name-binding API, so
the four timer names resolve through the timer-host registry
(`ngx_host_registry`) instead of the per-name bind list; the manifest/catalog
add them on top of the 191 callable baseline and the four semaphore entries
(191 + 4 semaphore = 195 per-name, + 4 timer = 199 total). See
[timer](timer.md).

ngx-rustscript provides an OpenResty-*oriented* RustScript API. It does
does not run Lua and documents only the hosts in this matrix (including UDP
and TCP cosocket).

**Built and tested against OpenResty 1.31.4.1 with NGINX core 1.31.4**
(`nginx_version` 1031004), ngx-rust `cda9d8372ccd2e139ef2b03e5806e019e7796187`, and the
pinned `pd-vm` revision in the crate README. Executable directives are
`access_by_rss_file`, `content_by_rss_file`, and `log_by_rss_file`.

The host-by-host tables are split by family below. Phase allowlists,
cross-cutting adaptations, and hard caps stay on this page so each family
page can cite them without repeating the shared rules.

## Family index

- [Sleep](timer.md)
- [Codec and hash](codec.md)
- [Time](time.md)
- [Metadata, worker, config, and phase](metadata.md)
- [Logging](logging.md)
- [Constants](constants.md)
- [Request and variables](request.md)
- [Request-local context](ctx.md)
- [Response metadata](response.md)
- [Response output](output.md)
- [Control flow](control.md)
- [Shared dictionary](shared-dict.md)
- [UDP cosocket](udp.md)
- [TCP cosocket](tcp.md)
- [Semaphore](semaphore.md)
- [pd-vm standard async hosts](standard-async.md)

pd-vm I/O, HTTP, and SQLite hosts are not part of the 199 ngx names; see
[pd-vm standard async hosts](standard-async.md).

## How to read the matrix

| Field | Meaning |
| --- | --- |
| OpenResty API | lua-nginx-module v0.10.29 name |
| RustScript callable | Production host import name and catalog signature |
| Status | `supported` (same call shape and result kind) or `adapted` (intentional RustScript mapping) |
| Phases | Catalog allowlist. Names are the `NgxPhase` display strings below. Runnable directives are `access_by_rss_file` (`access`), `content_by_rss_file` (`content`), and `log_by_rss_file` (`log`). Config-time import checks use the directive's phase. |
| Yield | `sync` (returns immediately) or `Waiting` (`VmStatus::Waiting`, `NGX_AGAIN`) |
| Limits | Hard caps enforced by the host |
| Semantic differences | Intentional adaptations versus OpenResty Lua |
| Acceptance | Tests that exercise the host or family |

Catalog type labels: `number` (int or float), `int`, `string`, `bytes`,
`bool`, `null`, `unknown` (checked at runtime), `map`, `array<int>`,
`optional<T>` (`T` or `null`), `resource` (typed VM handle).

The pinned compiler matches host arity exactly. OpenResty optional
trailing arguments are an explicit `null` argument, not an omitted
argument.

## Inventory

| Family | Hosts | Yield |
| --- | ---: | --- |
| [Timer (sleep)](timer.md) | 2 | Waiting |
| [Codec / hash](codec.md) | 14 | sync |
| [Time](time.md) | 9 | sync |
| [Metadata / worker / config / phase](metadata.md) | 12 | sync |
| [Logging](logging.md) | 1 | sync |
| [Request-read / var](request.md) | 26 | mixed |
| [Request-local ctx](ctx.md) | 4 | sync |
| [Response metadata](response.md) | 6 | sync |
| [Response output](output.md) | 5 | print/say/send_headers/eof sync; flush async |
| [Control](control.md) | 2 | sync |
| [Shared dict](shared-dict.md) | 22 | sync |
| [Cosocket (UDP)](udp.md) | 7 | mixed |
| [Cosocket (TCP)](tcp.md) | 16 | mixed |
| [Semaphore](semaphore.md) | 4 | `wait` Waiting; `new`/`post`/`count` sync |
| [Timer](timer.md) | 4 | `at`/`every` register and return; callbacks run on a timer-owned VM |
| [Constants](constants.md) | 65 | sync |
| of which HTTP status | 35 | sync |
| **Production total** | **199** | |

Test-only hosts `ngx::probe`, `ngx::ready`, and `ngx::fail` are not in
the production manifest, catalog, or `bind_ngx_hosts`.

Every production host except the five content-phase output hosts
(`ngx::send_headers`, `ngx::print`, `ngx::say`, `ngx::flush`, `ngx::eof`)
is allowed in `access`. Output hosts are content-only and rejected at
config time in access/init/set/rewrite. Unknown imports, phase-
forbidden imports, arity errors, and type errors are distinct
configuration failures. Bare `print` is not a host; use `ngx::print`
or `ngx::log`. `runtime::sleep` is rejected.

## Phase sets

Catalog phase names (exact): `init`, `init_worker`, `set`, `rewrite`,
`access`, `content`, `header_filter`, `body_filter`, `log`, `timer`,
`balancer`, `ssl_certificate`, `ssl_session_fetch`, `ssl_session_store`,
`ssl_client_hello`, `exit_worker`.

`ngx::get_phase()` returns those strings except `ssl_certificate`, which
is reported as OpenResty `ssl_cert`.

Shared sets used by the tables:

| Set | Phases |
| --- | --- |
| `SLEEP_PHASES` | `rewrite`, `access`, `content`, `timer`, `ssl_certificate`, `ssl_session_fetch` |
| `PHASE_SEMAPHORE` | `access`, `content` (fail-closed everywhere else, including `log`) |
| `PHASE_ALL` | `init`, `init_worker`, `set`, `rewrite`, `access`, `content`, `header_filter`, `body_filter`, `log`, `timer`, `balancer`, `ssl_certificate`, `ssl_session_fetch`, `ssl_session_store`, `exit_worker`, `ssl_client_hello` |
| `PHASE_ESCAPE_URI` | same as `PHASE_ALL` |
| `PHASE_UNESCAPE_URI` | `PHASE_ALL` minus `ssl_session_fetch` and `ssl_session_store` |
| `PHASE_ENCODE_ARGS` | `set`, `rewrite`, `access`, `content`, `header_filter`, `body_filter`, `log`, `timer`, `balancer`, `ssl_certificate`, `ssl_client_hello` |
| `PHASE_DECODE_ARGS` | `set`, `rewrite`, `access`, `content`, `header_filter`, `body_filter`, `log`, `timer`, `balancer`, `ssl_certificate`, `ssl_session_fetch`, `ssl_session_store`, `ssl_client_hello` |
| `PHASE_CODEC_MISC` | `set`, `rewrite`, `access`, `content`, `header_filter`, `body_filter`, `log`, `timer`, `balancer`, `ssl_certificate`, `ssl_session_fetch`, `ssl_session_store` |
| `PHASE_CODEC_MISC_SSL_CLIENT_HELLO` | `PHASE_CODEC_MISC` plus `ssl_client_hello` |
| `PHASE_TIME` | `init_worker`, `set`, `rewrite`, `access`, `content`, `header_filter`, `body_filter`, `log`, `timer`, `balancer`, `ssl_certificate`, `ssl_session_fetch`, `ssl_session_store`, `exit_worker`, `ssl_client_hello` |
| `PHASE_CORE_CONST` | `init`, `set`, `rewrite`, `access`, `content`, `header_filter`, `body_filter`, `log`, `timer`, `balancer`, `ssl_certificate`, `ssl_session_fetch`, `ssl_session_store`, `ssl_client_hello` |
| `PHASE_CONFIG_RUNTIME` | `init`, `init_worker`, `set`, `rewrite`, `access`, `content`, `header_filter`, `body_filter`, `log`, `timer`, `exit_worker` |
| `PHASE_CONFIG_BUILD` | `init`, `set`, `rewrite`, `access`, `content`, `header_filter`, `body_filter`, `log`, `timer` |
| `PHASE_WORKER_PIDS` | `set`, `rewrite`, `access`, `content`, `header_filter`, `body_filter`, `log`, `timer`, `exit_worker` |
| `PHASE_WORKER_ID` | `init_worker`, `set`, `rewrite`, `access`, `content`, `header_filter`, `body_filter`, `log`, `timer`, `exit_worker` |
| `PHASE_REQ_GET_METHOD` | `set`, `rewrite`, `access`, `content`, `header_filter`, `body_filter`, `balancer`, `log` |
| `PHASE_REQ_HTTP_VERSION` | `set`, `rewrite`, `access`, `content`, `header_filter` |
| `PHASE_REQ_GET_URI_ARGS` | `set`, `rewrite`, `access`, `content`, `header_filter`, `body_filter`, `log`, `balancer` |
| `PHASE_REQ_GET_POST_ARGS` | `rewrite`, `access`, `content`, `header_filter`, `body_filter`, `log` |
| `PHASE_REQ_GET_HEADERS` | `set`, `rewrite`, `access`, `content`, `header_filter`, `body_filter`, `log` |
| `PHASE_REQ_FLAGS` | `set`, `rewrite`, `access`, `content`, `header_filter`, `body_filter`, `log` |
| `PHASE_HEADERS_SENT` | `set`, `rewrite`, `access`, `content` |
| `PHASE_VAR_GET` | `set`, `rewrite`, `access`, `content`, `header_filter`, `body_filter`, `log`, `balancer` |
| `PHASE_REQ_SET_METHOD` | `set`, `rewrite`, `access`, `content`, `header_filter` |
| `PHASE_REQ_SET_URI` | `set`, `rewrite`, `access`, `content`, `header_filter`, `body_filter` |
| `PHASE_REQ_SET_HEADER` | `set`, `rewrite`, `access`, `content`, `header_filter`, `body_filter`, `log`, `balancer` |
| `PHASE_REQ_READ_BODY` | `rewrite`, `access`, `content` |
| `PHASE_REQ_GET_BODY_DATA` | `rewrite`, `access`, `content`, `log` |
| `PHASE_REQ_GET_BODY_FILE` | `rewrite`, `access`, `content` |
| `PHASE_REQ_SET_BODY` | `rewrite`, `access`, `content` |
| `PHASE_RESPONSE_META` | `set`, `rewrite`, `access`, `content`, `header_filter`, `body_filter`, `log`, `balancer` |
| `PHASE_OUTPUT` | `content` |
| `PHASE_EXIT` | `rewrite`, `access`, `content`, `header_filter`, `timer`, `balancer`, `ssl_certificate`, `ssl_session_fetch`, `ssl_session_store` |
| `PHASE_REDIRECT` | `rewrite`, `access`, `content` |
| `PHASE_CTX` | `set`, `rewrite`, `access`, `content`, `header_filter`, `body_filter`, `log`, `timer`, `balancer`, `ssl_certificate`, `ssl_session_fetch`, `ssl_session_store`, `ssl_client_hello` |
| `PHASE_DICT_LOOKUP` | same as `PHASE_ALL` |
| `PHASE_DICT_GET` | same as `PHASE_CTX` |
| `PHASE_DICT_WRITE` | `init`, `set`, `rewrite`, `access`, `content`, `header_filter`, `body_filter`, `log`, `timer`, `balancer`, `ssl_certificate`, `ssl_session_fetch`, `ssl_session_store`, `ssl_client_hello` |
| `PHASE_SOCKET_UDP` | same as `SLEEP_PHASES` |

## Cross-cutting adaptations

These apply across families. Per-host rows do not repeat them unless a
host adds a further difference.

1. **Namespaced zero-arg functions for Lua fields and constants.** Lua
   `ngx.OK`, `ngx.HTTP_GET`, `ngx.config.prefix`, `ngx.worker.pid` become
   callables such as `ngx::core::ok()`, `ngx::http::get()`,
   `ngx::config::prefix()`, `ngx::worker::pid()`. There are no host
   constants or dynamic table fields.
2. **`ngx.null` is `ngx::core::null_value() -> null`.** `ngx::core::null`
   is not catalogued: `null` is a reserved token.
3. **`ngx.sleep` is seconds.** `ngx::sleep(seconds: number) -> null`
   accepts int or float seconds and rejects negative values, NaN,
   infinity, and duration overflow. Legacy `ngx::sleep_ms(ms: int) -> null`
   keeps integer milliseconds for existing scripts. Both are async.
4. **`ngx.decode_args` result shape.** Returns `{ args, truncated }`
   rather than Lua's multi-return `(table, truncated?)`. `max_args` of
   `null` defaults to 100; `0` is clamped to the hard max 1024 (not
   unbounded); values above 1024 are rejected. Keys without `=` become
   boolean `true`. Repeated keys become arrays. Empty keys are skipped.
   Invalid `%` sequences are left as-is.
5. **Byte versus string results.** Hex `ngx::md5` returns `string`.
   `ngx::md5_bin`, `ngx::sha1_bin`, and `ngx::hmac_sha1` return `bytes`.
   `ngx::decode_base64` / `ngx::decode_base64mime` return `optional<bytes>`
   (`null` on invalid input). `ngx::unescape_uri` returns a string even
   when decoded bytes are not UTF-8 (each byte becomes `char::from`).
6. **Time cache / update boundary.** Time hosts use a process-local
   cached clock, not nginx `ngx_time` / `ngx_timeofday`. Reads reuse the
   snapshot while the event generation is unchanged and the snapshot is
   younger than 1s. Access-phase request execution and later eventfd
   resume/poll turns call `notify_cached_clock_event_boundary` so the
   cache may refresh like nginx `ngx_time_update` on the event loop.
   `ngx::update_time()` always samples the OS (or a test override) and
   replaces the snapshot.
7. **No global `print`.** `print` is not a production host. Log with
   `ngx::log`. Config rejects `print` and `runtime::sleep`.
8. **`ngx.log` request versus cycle.** `ngx::log(level, message)` writes
   the live request log when the worker-thread request scope is active
   and allows `LOG`; otherwise it uses the cycle log. Missing both sinks
   is a host error. Levels outside `NGX_LOG_STDERR..=NGX_LOG_DEBUG` and
   messages containing NUL are host errors. The line is prefixed
   `ngx-rustscript: `. OpenResty varargs are a single string.
9. **Phase and config import enforcement.** `compile_rss_source` compiles
   access scripts against `compile_host_catalog()` — exactly the 199-name
   `ngx_host_catalog()`, with no pd-vm standard family merged in — and then
   `validate_program_imports_for_phase(..., Access)`;
   `compile_rss_source_for_phase` performs the corresponding Content or
   Log validation for `content_by_rss_file` and `log_by_rss_file`. pd-vm
   standard HTTP imports stay dynamic: they compile as ordinary host
   imports against the documented `map` interface, are validated by name
   (`is_standard_host_import`), and bind through the staged standard
   composition at runtime; pd-vm I/O and SQLite are compiler builtins
   dispatched through that composition. Unknown names, phase-forbidden
   names, arity, and type mismatches fail distinctly at `nginx -t` /
   reload. Catalog phase metadata is the allowlist for all three
   executable directives and for documented future phases.
10. **`ngx.worker.pids` semantics.** The array always includes live
    `ngx_pid` when `pid > 0`. Other `ngx_processes` slots are included
    only when `pid > 0` and `channel[0] > 0` (excludes master, helpers,
    and stale entries). Invalid `ngx_process_slot` is ignored. Duplicate
    pids are collapsed. Values come from NGINX public globals, never
    `/proc`.
11. **Optional OpenResty arguments.** Trailing options such as escape
    type, `max_args`, and `no_padding` must be passed as `null` when the
    default is wanted.

## Hard caps

| Cap | Value | Applies to |
| --- | ---: | --- |
| Codec input | 65536 bytes | URI/args/base64/hash/quote inputs |
| Codec output | 196608 bytes | escape/encode/quote outputs |
| `decode_args` default `max_args` | 100 | `null` second argument |
| `decode_args` hard `max_args` | 1024 | explicit cap; `0` clamps to this |
| `encode_args` pairs | 1024 | including array expansion |
| `parse_http_time` input | 128 bytes | over-limit is a host error |
| Async in-flight waits | 1024 (`DEFAULT_MAX_IN_FLIGHT`) | `ngx::sleep` / `ngx::sleep_ms` / UDP ops |
| UDP datagram | 65535 bytes | send payload and receive size |
| UDP DNS host | 255 bytes | `setpeername` host before lookup |
| Idle VM pool | 4 | import-free programs |
| Cached clock stale limit | 1 second | time hosts without `update_time` |
| Request copy / body / raw header | 65536 bytes | `ngx::req::raw_header`, `get_headers` total, `get_uri_args` / `get_post_args` source |
| Header field | 8192 bytes | single `get_headers` name or value |
| Header / args default cap | 100 | `null` `max_headers` / `max_args` |
| Header / args hard cap | 1024 | explicit cap; `0` clamps to this |


---

[Compatibility and migration](../ngx-api-compatibility.md) · [Directives](../directives.md)
