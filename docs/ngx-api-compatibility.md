# Release A ngx API compatibility matrix

This matrix is the authoritative Release A ngx host surface for
`ngx-rustscript`. It is derived from the production manifest
(`src/host/catalog.rs` `ngx_host_manifest()`), the compiler catalog
(`ngx_host_catalog()`), and the runtime bind list
(`src/host/mod.rs` `production_runtime_host_names()` / `bind_ngx_hosts`).
Those three lists contain the same **103** names in the same order.

ngx-rustscript provides an OpenResty-*oriented* RustScript API. It does
not run Lua, does not depend on an OpenResty binary, and does not claim
APIs beyond this Release A set.

**Built and tested against official NGINX 1.30.4** (`nginx_version`
1030004), ngx-rust `cda9d8372ccd2e139ef2b03e5806e019e7796187`, and the
pinned `pd-vm` revision in the crate README. The current executable
directive is `access_by_rss_file` (HTTP access phase only).

## OpenResty documentation baseline

Release A phase lists and API names follow lua-nginx-module **v0.10.29**:

- Tagged README: [lua-nginx-module v0.10.29 Nginx API for Lua](https://github.com/openresty/lua-nginx-module/blob/v0.10.29/README.markdown#nginx-api-for-lua)
- Exact snapshot used when the host manifest was authored: [lua-nginx-module at 8374a7beb642af43715a05a33b2cc277fba29c4d](https://github.com/openresty/lua-nginx-module/blob/8374a7beb642af43715a05a33b2cc277fba29c4d/README.markdown#nginx-api-for-lua)

Do not treat later OpenResty APIs, stream-lua, or undocumented Lua
helpers as part of this module.

## How to read the matrix

| Field | Meaning |
| --- | --- |
| OpenResty API | lua-nginx-module v0.10.29 name |
| RustScript callable | Production host import name and catalog signature |
| Status | `supported` (same call shape and result kind) or `adapted` (intentional RustScript mapping) |
| Phases | Catalog allowlist. Names are the `NgxPhase` display strings below. The current directive only *runs* scripts in `access`; config-time import checks for `access_by_rss_file` use `access`. |
| Yield | `sync` (returns immediately) or `Waiting` (`VmStatus::Waiting`, `NGX_AGAIN`) |
| Limits | Hard caps enforced by the host |
| Semantic differences | Intentional adaptations versus OpenResty Lua |
| Acceptance | Tests that exercise the host or family |

Catalog type labels: `number` (int or float), `int`, `string`, `bytes`,
`bool`, `null`, `unknown` (checked at runtime), `map`, `array<int>`,
`optional<T>` (`T` or `null`).

The pinned compiler matches host arity exactly. OpenResty optional
trailing arguments are an explicit `null` argument, not an omitted
argument.

## Inventory

| Family | Hosts | Yield |
| --- | ---: | --- |
| Timer (sleep) | 2 | Waiting |
| Codec / hash | 14 | sync |
| Time | 9 | sync |
| Metadata / worker / config / phase | 12 | sync |
| Logging | 1 | sync |
| Constants | 65 | sync |
| **Production total** | **103** | |

Test-only hosts `ngx::probe`, `ngx::ready`, and `ngx::fail` are not in
the production manifest, catalog, or `bind_ngx_hosts`.

Every production host is allowed in `access`. Unknown imports, phase-
forbidden imports, arity errors, and type errors are distinct
configuration failures. `print` is not a host; use `ngx::log`.
`runtime::sleep` is rejected.

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
   against `ngx_host_catalog()` and then
   `validate_program_imports_for_phase(..., Access)`. Unknown names,
   phase-forbidden names, arity, and type mismatches fail distinctly at
   `nginx -t` / reload. Catalog phase metadata is the allowlist for
   future directives; only `access` is wired today.
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
| Async in-flight waits | 1024 (`DEFAULT_MAX_IN_FLIGHT`) | `ngx::sleep` / `ngx::sleep_ms` |
| Idle VM pool | 4 | import-free programs |
| Cached clock stale limit | 1 second | time hosts without `update_time` |

## Sleep

| OpenResty API | RustScript callable | Status | Phases | Yield | Limits | Semantic differences | Acceptance |
| --- | --- | --- | --- | --- | --- | --- | --- |
| `ngx.sleep` | `ngx::sleep(seconds: number) -> null` | adapted | `SLEEP_PHASES` | Waiting | in-flight 1024; rejects negative / NaN / infinity / overflow | Seconds (int or float), not milliseconds. Tokio timer; completion via `HostFuture` -> waker -> eventfd -> worker-main-thread `resume`. Client close during wait is HTTP 499. | `tests/vm_sleep.rs`, `tests/host_catalog.rs`, `tests/access_async.rs`, `tests/lifecycle.rs`, `tests/nginx_integration.py` |
| `ngx.sleep` (legacy ms form) | `ngx::sleep_ms(ms: int) -> null` | adapted | `SLEEP_PHASES` | Waiting | in-flight 1024; `ms >= 0` | Not an OpenResty name. Integer milliseconds for scripts written before the seconds migration. Same Waiting path as `ngx::sleep`. | `tests/vm_sleep.rs`, `tests/access_async.rs`, `tests/request_driver.rs`, `tests/lifecycle.rs` |

## Codec and hash

Shared codec limits: 65536-byte input, 196608-byte output. All rows
are sync.

| OpenResty API | RustScript callable | Status | Phases | Limits | Semantic differences | Acceptance |
| --- | --- | --- | --- | --- | --- | --- |
| `ngx.escape_uri` | `ngx::escape_uri(uri: string, escape_type: unknown) -> string` | adapted | `PHASE_ESCAPE_URI` | input/output caps; type `0..=8` | Second arg is `null` (default type 2, URI component) or int `0..=8`. Types match lua-nginx-module bitmaps (0 URI, 1 args, 2 component, 3 HTML, 4 refresh, 5/6 memcached, 7 header name, 8 header value). | `tests/ngx_codec.rs` (`escape_uri_*`), `tests/host_catalog.rs` |
| `ngx.unescape_uri` | `ngx::unescape_uri(uri: string) -> string` | adapted | `PHASE_UNESCAPE_URI` | input cap | `+` becomes space; invalid `%` left as-is; non-UTF-8 bytes kept as `char::from` string. Not allowed in `ssl_session_fetch` / `ssl_session_store`. | `tests/ngx_codec.rs` (`unescape_uri_*`) |
| `ngx.encode_args` | `ngx::encode_args(args: unknown) -> string` | adapted | `PHASE_ENCODE_ARGS` | 1024 pairs; input/output caps | Catalog type is `unknown` because object literals are not `map<unknown>`; runtime requires a map. Array values emit repeated keys. `true` is a bare key; `false`/`null` omitted. Nested maps/arrays-as-single-value rejected. Duplicate map keys overwrite; use arrays for multi-value keys. Not allowed in `init` / `init_worker`. | `tests/ngx_codec.rs` (`encode_args_*`) |
| `ngx.decode_args` | `ngx::decode_args(query: string, max_args: unknown) -> map` | adapted | `PHASE_DECODE_ARGS` | default 100; hard max 1024 | Returns `{ args, truncated }`. See cross-cutting item 4. `max_args` is `null` or int. | `tests/ngx_codec.rs` (`decode_args_*`) |
| `ngx.encode_base64` | `ngx::encode_base64(input: unknown, no_padding: unknown) -> string` | adapted | `PHASE_CODEC_MISC_SSL_CLIENT_HELLO` | input/output caps | `input` is string or bytes. `no_padding` is `null`/`false` or `true`. | `tests/ngx_codec.rs` (`encode_base64_padding_and_decode_variants`) |
| `ngx.decode_base64` | `ngx::decode_base64(text: string) -> optional<bytes>` | adapted | `PHASE_CODEC_MISC_SSL_CLIENT_HELLO` | input cap | Invalid input is `null`, not an error. Padding may be omitted. | `tests/ngx_codec.rs` (`encode_base64_padding_and_decode_variants`) |
| `ngx.decode_base64mime` | `ngx::decode_base64mime(text: string) -> optional<bytes>` | adapted | `PHASE_CODEC_MISC` | input cap | Skips non-alphabet bytes; stops at first `=`. `data_len % 4 == 1` is `null`. **Not** allowed in `ssl_client_hello` (OpenResty v0.10.29 exception). | `tests/ngx_codec.rs` (`decode_base64mime_*`), `tests/host_catalog.rs` |
| `ngx.crc32_short` | `ngx::crc32_short(data: string) -> int` | supported | `PHASE_CODEC_MISC_SSL_CLIENT_HELLO` | input cap | IEEE CRC-32. Currently matches `crc32_long`. | `tests/ngx_codec.rs` (`hash_and_hmac_use_fixed_vectors`) |
| `ngx.crc32_long` | `ngx::crc32_long(data: string) -> int` | supported | `PHASE_CODEC_MISC_SSL_CLIENT_HELLO` | input cap | Same result as `crc32_short` in this release. | `tests/ngx_codec.rs` (`hash_and_hmac_use_fixed_vectors`) |
| `ngx.hmac_sha1` | `ngx::hmac_sha1(secret: string, data: string) -> bytes` | adapted | `PHASE_CODEC_MISC_SSL_CLIENT_HELLO` | input cap on each arg | Raw digest **bytes**, not hex. | `tests/ngx_codec.rs` (`hash_and_hmac_use_fixed_vectors`) |
| `ngx.md5` | `ngx::md5(data: string) -> string` | supported | `PHASE_CODEC_MISC_SSL_CLIENT_HELLO` | input cap | Lowercase hex string. | `tests/ngx_codec.rs` (`hash_and_hmac_use_fixed_vectors`) |
| `ngx.md5_bin` | `ngx::md5_bin(data: string) -> bytes` | adapted | `PHASE_CODEC_MISC_SSL_CLIENT_HELLO` | input cap | Raw 16-byte digest. | `tests/ngx_codec.rs` (`hash_and_hmac_use_fixed_vectors`) |
| `ngx.sha1_bin` | `ngx::sha1_bin(data: string) -> bytes` | adapted | `PHASE_CODEC_MISC_SSL_CLIENT_HELLO` | input cap | Raw 20-byte digest. | `tests/ngx_codec.rs` (`hash_and_hmac_use_fixed_vectors`) |
| `ngx.quote_sql_str` | `ngx::quote_sql_str(value: string) -> string` | supported | `PHASE_CODEC_MISC_SSL_CLIENT_HELLO` | input/output caps | MySQL literal quoting (`\0 \b \n \r \t \Z \\ ' "`). | `tests/ngx_codec.rs` (`quote_sql_str_uses_mysql_escapes`) |

Oversized codec input is rejected without huge allocation:
`tests/ngx_codec.rs` `codec_rejects_oversized_input_without_huge_allocation`.

## Time

All time hosts are sync and use `PHASE_TIME`. Cached-clock behavior is
in cross-cutting item 6.

| OpenResty API | RustScript callable | Status | Limits | Semantic differences | Acceptance |
| --- | --- | --- | --- | --- | --- |
| `ngx.today` | `ngx::today() -> string` | adapted | cached clock | Local `yyyy-mm-dd` from the cached snapshot. | `tests/ngx_time.rs` (`today_localtime_and_utctime_use_fixed_formats`) |
| `ngx.time` | `ngx::time() -> int` | adapted | cached clock | Cached unix **seconds** as `int`, not `ngx::now`. | `tests/ngx_time.rs` (`time_is_int_and_now_is_float_from_the_same_cache`) |
| `ngx.now` | `ngx::now() -> float` | adapted | cached clock; 1s stale | Cached seconds plus milliseconds as `float`. Intra-script reads share the snapshot. | `tests/ngx_time.rs` (`intra_script_now_reads_share_the_cached_snapshot`, clock-boundary tests) |
| `ngx.update_time` | `ngx::update_time() -> null` | adapted | samples OS immediately | Always refreshes the process-local cache. Observable side effect. | `tests/ngx_time.rs` (`update_time_samples_os_immediately_without_event_turn`) |
| `ngx.localtime` | `ngx::localtime() -> string` | adapted | cached clock | Local `yyyy-mm-dd hh:mm:ss`. | `tests/ngx_time.rs` (`today_localtime_and_utctime_use_fixed_formats`) |
| `ngx.utctime` | `ngx::utctime() -> string` | adapted | cached clock | UTC `yyyy-mm-dd hh:mm:ss`. | `tests/ngx_time.rs` (`today_localtime_and_utctime_use_fixed_formats`) |
| `ngx.cookie_time` | `ngx::cookie_time(sec: int) -> string` | supported | none beyond conversion | `Thu, 18-Nov-10 11:27:35 GMT`. | `tests/ngx_time.rs` (`http_cookie_and_parse_time_are_fixed`) |
| `ngx.http_time` | `ngx::http_time(sec: int) -> string` | supported | none beyond conversion | `Thu, 18 Nov 2010 11:27:35 GMT`. | `tests/ngx_time.rs` (`http_cookie_and_parse_time_are_fixed`) |
| `ngx.parse_http_time` | `ngx::parse_http_time(text: string) -> optional<int>` | adapted | 128-byte input | RFC 822 / RFC 850 / asctime. Invalid or pre-1970 is `null`. Over 128 bytes is a host error, not `null`. | `tests/ngx_time.rs` (`parse_http_time_*`) |

## Metadata, worker, config, and phase

All rows are sync zero-arg hosts except `ngx::log` (next section).
Cycle-dependent hosts fail with a host error when `ngx_cycle` is null.

| OpenResty API | RustScript callable | Status | Phases | Semantic differences | Acceptance |
| --- | --- | --- | --- | --- | --- |
| `ngx.get_phase` | `ngx::get_phase() -> string` | adapted | `PHASE_ALL` | Reads the worker-thread execution scope. Fails outside a scope. `ssl_certificate` is returned as `ssl_cert`. Current runtime always runs in `access`. | `tests/ngx_meta.rs` (`get_phase_reads_execution_scope_and_fails_outside_a_request`), `tests/access_nginx.rs` |
| `ngx.config.subsystem` | `ngx::config::subsystem() -> string` | supported | `PHASE_CONFIG_RUNTIME` | Always `"http"`. Stream is out of scope. | `tests/ngx_meta.rs` (`compile_time_config_metadata_is_owned_and_truthful`) |
| `ngx.config.debug` | `ngx::config::debug() -> bool` | supported | `PHASE_CONFIG_RUNTIME` | `ngx::log::DEBUG` of the linked NGINX build. | `tests/ngx_meta.rs` |
| `ngx.config.prefix` | `ngx::config::prefix() -> string` | adapted | `PHASE_CONFIG_RUNTIME` | Owned copy of `ngx_cycle.prefix`. Requires a live cycle. | `tests/ngx_meta.rs`, `tests/access_nginx.rs` |
| `ngx.config.nginx_version` | `ngx::config::nginx_version() -> int` | supported | `PHASE_CONFIG_RUNTIME` | `nginx_sys::nginx_version` (1030004 for 1.30.4). Unlike `nginx_configure`, allowed in `init_worker` and `exit_worker`. | `tests/ngx_meta.rs`, `tests/host_catalog.rs` |
| `ngx.config.nginx_configure` | `ngx::config::nginx_configure() -> string` | supported | `PHASE_CONFIG_BUILD` | `NGX_CONFIGURE` (`--with-compat`, `--with-http_ssl_module`, ...). Not allowed in `init_worker` / `exit_worker`. | `tests/ngx_meta.rs` |
| `ngx.config.ngx_lua_version` | `ngx::config::ngx_rustscript_version() -> int` | adapted | `PHASE_CONFIG_BUILD` | **Not** a forged lua-nginx-module version. Encodes crate 0.x.y as `x * 1000 + y` (0.1.0 reports `1000`). Same phases as `nginx_configure`. | `tests/ngx_meta.rs` |
| `ngx.worker.exiting` | `ngx::worker::exiting() -> bool` | supported | `PHASE_CONFIG_RUNTIME` | Live `ngx_exiting`. | `tests/ngx_meta.rs`, `tests/access_nginx.rs` |
| `ngx.worker.pid` | `ngx::worker::pid() -> int` | supported | `PHASE_CONFIG_RUNTIME` | Live `ngx_pid`. | `tests/ngx_meta.rs`, `tests/access_nginx.rs` |
| `ngx.worker.pids` | `ngx::worker::pids() -> array<int>` | adapted | `PHASE_WORKER_PIDS` | See cross-cutting item 10. No `init` / `init_worker`. | `tests/ngx_meta.rs` (`worker_pids_*`), `tests/access_nginx.rs` |
| `ngx.worker.count` | `ngx::worker::count() -> int` | supported | `PHASE_CONFIG_RUNTIME` | Core `worker_processes` from the live cycle. | `tests/ngx_meta.rs`, `tests/access_nginx.rs` |
| `ngx.worker.id` | `ngx::worker::id() -> int` | supported | `PHASE_WORKER_ID` | Live `ngx_worker`. No `init`. | `tests/ngx_meta.rs`, `tests/access_nginx.rs` |

Live NGINX worker metadata (pid/id/count/prefix/phase/request log) is
accepted in `tests/access_nginx.rs`
`access_phase_logs_worker_metadata_constants_and_request_log`.

## Logging

| OpenResty API | RustScript callable | Status | Phases | Yield | Limits | Semantic differences | Acceptance |
| --- | --- | --- | --- | --- | --- | --- | --- |
| `ngx.log` | `ngx::log(level: int, message: string) -> null` | adapted | `PHASE_ALL` | sync | level `NGX_LOG_STDERR..=NGX_LOG_DEBUG`; no embedded NUL | Single string, not varargs. Request log when the access scope is live and allows `LOG`; otherwise cycle log; error if neither. Prefix `ngx-rustscript: `. No global `print`. | `tests/ngx_meta.rs` (`log_*`), `tests/access_nginx.rs` (`access_phase_logs_*`, `access_phase_rejects_invalid_log_level_and_embedded_nul`) |

## Constant families

Pinned `pd-host-function` has no host-constant form. Lua fields such as
`ngx.OK` are zero-argument functions. All constant hosts are **adapted**,
**sync**, and have no extra numeric caps.

Integer constants match `nginx-sys` for official NGINX 1.30.4 except
status codes that nginx-sys does not export, which are the HTTP values
`402`, `406`, `410`, `426`, and `451`.

Acceptance: `tests/ngx_meta.rs`
(`constant_hosts_are_zero_arg_ints_matching_nginx_sys_values`,
`constant_manifest_uses_openresty_names_and_sync_metadata`),
`tests/access_nginx.rs`.

### Core (`PHASE_CORE_CONST`)

| OpenResty API | RustScript callable | Returns |
| --- | --- | --- |
| `ngx.OK` | `ngx::core::ok() -> int` | `0` |
| `ngx.ERROR` | `ngx::core::error() -> int` | `-1` |
| `ngx.AGAIN` | `ngx::core::again() -> int` | `-2` |
| `ngx.DONE` | `ngx::core::done() -> int` | `-4` |
| `ngx.DECLINED` | `ngx::core::declined() -> int` | `-5` |
| `ngx.null` | `ngx::core::null_value() -> null` | `null` |

### HTTP methods (`PHASE_CORE_CONST`)

Values are NGINX method bitmasks from `nginx-sys` (not HTTP method
strings).

| OpenResty API | RustScript callable |
| --- | --- |
| `ngx.HTTP_GET` | `ngx::http::get() -> int` |
| `ngx.HTTP_HEAD` | `ngx::http::head() -> int` |
| `ngx.HTTP_PUT` | `ngx::http::put() -> int` |
| `ngx.HTTP_POST` | `ngx::http::post() -> int` |
| `ngx.HTTP_DELETE` | `ngx::http::delete() -> int` |
| `ngx.HTTP_OPTIONS` | `ngx::http::options() -> int` |
| `ngx.HTTP_MKCOL` | `ngx::http::mkcol() -> int` |
| `ngx.HTTP_COPY` | `ngx::http::copy() -> int` |
| `ngx.HTTP_MOVE` | `ngx::http::move() -> int` |
| `ngx.HTTP_PROPFIND` | `ngx::http::propfind() -> int` |
| `ngx.HTTP_PROPPATCH` | `ngx::http::proppatch() -> int` |
| `ngx.HTTP_LOCK` | `ngx::http::lock() -> int` |
| `ngx.HTTP_UNLOCK` | `ngx::http::unlock() -> int` |
| `ngx.HTTP_PATCH` | `ngx::http::patch() -> int` |
| `ngx.HTTP_TRACE` | `ngx::http::trace() -> int` |

### HTTP status (`PHASE_CORE_CONST`)

| OpenResty API | RustScript callable | Notes |
| --- | --- | --- |
| `ngx.HTTP_CONTINUE` | `ngx::http::status_continue() -> int` | |
| `ngx.HTTP_SWITCHING_PROTOCOLS` | `ngx::http::status_switching_protocols() -> int` | |
| `ngx.HTTP_OK` | `ngx::http::status_ok() -> int` | 200 |
| `ngx.HTTP_CREATED` | `ngx::http::status_created() -> int` | |
| `ngx.HTTP_ACCEPTED` | `ngx::http::status_accepted() -> int` | |
| `ngx.HTTP_NO_CONTENT` | `ngx::http::status_no_content() -> int` | |
| `ngx.HTTP_PARTIAL_CONTENT` | `ngx::http::status_partial_content() -> int` | |
| `ngx.HTTP_SPECIAL_RESPONSE` | `ngx::http::status_special_response() -> int` | NGINX special-response threshold |
| `ngx.HTTP_MOVED_PERMANENTLY` | `ngx::http::status_moved_permanently() -> int` | |
| `ngx.HTTP_MOVED_TEMPORARILY` | `ngx::http::status_moved_temporarily() -> int` | |
| `ngx.HTTP_SEE_OTHER` | `ngx::http::status_see_other() -> int` | |
| `ngx.HTTP_NOT_MODIFIED` | `ngx::http::status_not_modified() -> int` | |
| `ngx.HTTP_TEMPORARY_REDIRECT` | `ngx::http::status_temporary_redirect() -> int` | |
| `ngx.HTTP_PERMANENT_REDIRECT` | `ngx::http::status_permanent_redirect() -> int` | |
| `ngx.HTTP_BAD_REQUEST` | `ngx::http::status_bad_request() -> int` | |
| `ngx.HTTP_UNAUTHORIZED` | `ngx::http::status_unauthorized() -> int` | |
| `ngx.HTTP_PAYMENT_REQUIRED` | `ngx::http::status_payment_required() -> int` | 402 (not in nginx-sys) |
| `ngx.HTTP_FORBIDDEN` | `ngx::http::status_forbidden() -> int` | |
| `ngx.HTTP_NOT_FOUND` | `ngx::http::status_not_found() -> int` | 404 |
| `ngx.HTTP_NOT_ALLOWED` | `ngx::http::status_not_allowed() -> int` | |
| `ngx.HTTP_NOT_ACCEPTABLE` | `ngx::http::status_not_acceptable() -> int` | 406 (not in nginx-sys) |
| `ngx.HTTP_REQUEST_TIMEOUT` | `ngx::http::status_request_timeout() -> int` | 408 (`NGX_HTTP_REQUEST_TIME_OUT`) |
| `ngx.HTTP_CONFLICT` | `ngx::http::status_conflict() -> int` | |
| `ngx.HTTP_GONE` | `ngx::http::status_gone() -> int` | 410 (not in nginx-sys) |
| `ngx.HTTP_UPGRADE_REQUIRED` | `ngx::http::status_upgrade_required() -> int` | 426 (not in nginx-sys) |
| `ngx.HTTP_TOO_MANY_REQUESTS` | `ngx::http::status_too_many_requests() -> int` | |
| `ngx.HTTP_ILLEGAL` | `ngx::http::status_illegal() -> int` | 451 (not in nginx-sys) |
| `ngx.HTTP_CLOSE` | `ngx::http::status_close() -> int` | NGINX `NGX_HTTP_CLOSE` |
| `ngx.HTTP_INTERNAL_SERVER_ERROR` | `ngx::http::status_internal_server_error() -> int` | 500 |
| `ngx.HTTP_NOT_IMPLEMENTED` | `ngx::http::status_not_implemented() -> int` | |
| `ngx.HTTP_BAD_GATEWAY` | `ngx::http::status_bad_gateway() -> int` | |
| `ngx.HTTP_SERVICE_UNAVAILABLE` | `ngx::http::status_service_unavailable() -> int` | |
| `ngx.HTTP_GATEWAY_TIMEOUT` | `ngx::http::status_gateway_timeout() -> int` | 504 (`NGX_HTTP_GATEWAY_TIME_OUT`) |
| `ngx.HTTP_VERSION_NOT_SUPPORTED` | `ngx::http::status_version_not_supported() -> int` | |
| `ngx.HTTP_INSUFFICIENT_STORAGE` | `ngx::http::status_insufficient_storage() -> int` | |

### Log levels (`PHASE_ALL`)

| OpenResty API | RustScript callable | Typical `nginx-sys` value |
| --- | --- | --- |
| `ngx.STDERR` | `ngx::log_level::stderr() -> int` | 0 |
| `ngx.EMERG` | `ngx::log_level::emerg() -> int` | 1 |
| `ngx.ALERT` | `ngx::log_level::alert() -> int` | 2 |
| `ngx.CRIT` | `ngx::log_level::crit() -> int` | 3 |
| `ngx.ERR` | `ngx::log_level::error() -> int` | 4 |
| `ngx.WARN` | `ngx::log_level::warn() -> int` | 5 |
| `ngx.NOTICE` | `ngx::log_level::notice() -> int` | 6 |
| `ngx.INFO` | `ngx::log_level::info() -> int` | 7 |
| `ngx.DEBUG` | `ngx::log_level::debug() -> int` | 8 |

Use these with `ngx::log(ngx::log_level::error(), "message")`.

## Out of scope for Release A

Not production hosts in this matrix (do not import them):

- Request/response/body/variable/ctx/control APIs (`ngx.req.*`,
  `ngx.var`, `ngx.header`, `ngx.exit`, `ngx.redirect`, `ngx.print` /
  `ngx.say`, ...)
- Shared dict, subrequest, cosocket, timer callbacks, light threads
- Stream subsystem, `ndk.set_var.*`, Lua `coroutine.*`
- `print`, `runtime::sleep`, and test hosts `ngx::probe` / `ngx::ready` /
  `ngx::fail`

Catalog phase names other than `access` document OpenResty allowlists
for later directives. They are not runnable entry points in Release A.

## Maintaining this matrix

Update this file whenever a production manifest API, catalog signature,
allowed phase, limit, yield behavior (sync vs Waiting), or semantic
adaptation changes. Cross-check `ngx_host_manifest()`,
`ngx_host_catalog()`, and `production_runtime_host_names()` so no
production host is omitted and no extra name is documented.
