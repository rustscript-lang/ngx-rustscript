# ngx API compatibility matrix

This matrix is the authoritative current ngx host surface for
`ngx-rustscript`. It is derived from the production manifest
(`src/host/catalog.rs` `ngx_host_manifest()`), the compiler catalog
(`ngx_host_catalog()`), and the runtime bind list
(`src/host/mod.rs` `production_runtime_host_names()` / `bind_ngx_hosts`).
Those three lists contain the same **182** names in the same order.

ngx-rustscript provides an OpenResty-*oriented* RustScript API. It does
not run Lua, does not depend on an OpenResty binary, and documents only
the hosts in this matrix (including UDP and TCP cosocket).

**Built and tested against official NGINX 1.30.4** (`nginx_version`
1030004), ngx-rust `cda9d8372ccd2e139ef2b03e5806e019e7796187`, and the
pinned `pd-vm` revision in the crate README. Executable directives are
`access_by_rss_file`, `content_by_rss_file`, and `log_by_rss_file`.

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
| Timer (sleep) | 2 | Waiting |
| Codec / hash | 14 | sync |
| Time | 9 | sync |
| Metadata / worker / config / phase | 12 | sync |
| Logging | 1 | sync |
| Request-read / var | 17 | sync |
| Request-local ctx | 4 | sync |
| Response metadata | 6 | sync |
| Response output | 5 | print/say/send_headers/eof sync; flush async |
| Control | 2 | sync |
| Shared dict | 22 | sync |
| Cosocket (UDP) | 7 | mixed |
| Cosocket (TCP) | 16 | mixed |
| Constants | 65 | sync |
| of which HTTP status | 35 | sync |
| **Production total** | **182** | |

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
| `PHASE_RESPONSE_META` | `set`, `rewrite`, `access`, `content`, `header_filter`, `body_filter`, `log`, `balancer` |
| `PHASE_EXIT` | `rewrite`, `access`, `content`, `header_filter`, `timer`, `balancer`, `ssl_certificate`, `ssl_session_fetch`, `ssl_session_store` |
| `PHASE_REDIRECT` | `rewrite`, `access`, `content` |

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
| Async in-flight waits | 1024 (`DEFAULT_MAX_IN_FLIGHT`) | `ngx::sleep` / `ngx::sleep_ms` / UDP ops |
| UDP datagram | 65535 bytes | send payload and receive size |
| UDP DNS host | 255 bytes | `setpeername` host before lookup |
| Idle VM pool | 4 | import-free programs |
| Cached clock stale limit | 1 second | time hosts without `update_time` |
| Request copy / body / raw header | 65536 bytes | `ngx::req::raw_header`, `get_headers` total, `get_uri_args` / `get_post_args` source |
| Header field | 8192 bytes | single `get_headers` name or value |
| Header / args default cap | 100 | `null` `max_headers` / `max_args` |
| Header / args hard cap | 1024 | explicit cap; `0` clamps to this |

## Sleep

| OpenResty API | RustScript callable | Status | Phases | Yield | Limits | Semantic differences | Acceptance |
| --- | --- | --- | --- | --- | --- | --- | --- |
| `ngx.sleep` | `ngx::sleep(seconds: number) -> null` | adapted | `SLEEP_PHASES` | Waiting | in-flight 1024; rejects negative / NaN / infinity / overflow | Seconds (int or float), not milliseconds. Tokio timer; completion via `HostFuture` -> waker -> eventfd -> worker-main-thread `resume`. Client close during wait is HTTP 499. | `tests/vm_sleep.rs`, `tests/host_catalog.rs`, `tests/access_async.rs`, `tests/lifecycle.rs`, `tests/nginx_integration.py` |
| `ngx.sleep` (legacy ms form) | `ngx::sleep_ms(ms: int) -> null` | adapted | `SLEEP_PHASES` | Waiting | in-flight 1024; `ms >= 0` | Not an OpenResty name. Integer milliseconds for scripts written before the seconds migration. Same Waiting path as `ngx::sleep`. | `tests/vm_sleep.rs`, `tests/access_async.rs`, `tests/request_driver.rs`, `tests/lifecycle.rs` |

## Codec and hash

Shared codec limits: 65536-byte input, 196608-byte output. All rows are sync.

OpenResty v0.10.29 documents these codec APIs for `log_by_lua*`
(`ngx.escape_uri` context table includes `log_by_lua*`; the other codec
rows share that allowlist except as noted). `log_by_rss_file` accepts
them only because that citation exists and each name is on the closed
Log allowlist.

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

### HTTP status (`PHASE_CORE_CONST`) — 35 constants

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

## Request-read and variables

All eleven hosts are **sync**. They copy ngx_str / list / buffer bytes into
owned VM values on the worker main thread and never retain request, pool,
or list pointers. They require a live request scope.

| OpenResty API | RustScript callable | Status | Phases | Limits | Semantic differences | Acceptance |
| --- | --- | --- | --- | --- | --- | --- |
| `ngx.req.get_method` | `ngx::req::get_method() -> string` | supported | `PHASE_REQ_GET_METHOD` | 256-byte method token | Custom methods keep the textual token. | `tests/ngx_request_read.rs` |
| `ngx.req.http_version` | `ngx::req::http_version() -> optional<float>` | adapted | `PHASE_REQ_HTTP_VERSION` | none | `0.9`/`1.0`/`1.1`/`2.0`/`3.0`, or `null` if unrecognized. | `tests/ngx_request_read.rs` |
| `ngx.req.raw_header` | `ngx::req::raw_header(no_request_line: unknown) -> string` | adapted | `PHASE_REQ_HTTP_VERSION` | 65536-byte reconstruction | Reconstructs from the **main** request connection header buffer and busy chain (OpenResty v0.10.29). Validates every pointer/range and rejects HTTP/2 (`r->main->stream`) and HTTP/3 (`http_version` 3.0 or non-null `v3_parse`) using main-request protocol truth. Parser NULs restore to `:` and CR/LF. Preread body after CRLFCRLF is stripped. `no_request_line` is `null`/`false`/`true`. Malformed ranges, null buffers, cycles, and over-cap copies are host errors. | `tests/ngx_request_read.rs` (`raw_header_*`) |
| `ngx.req.get_headers` | `ngx::req::get_headers(max_headers: unknown, raw: unknown) -> map` | adapted | `PHASE_REQ_GET_HEADERS` | default 100; hard 1024; 8192-byte field; 65536-byte total | Returns `{ headers, truncated }`. `raw` `true` keeps original names; otherwise lowercase. Duplicate names become arrays. Empty values kept. `hash==0` skipped. Count/byte caps set `truncated=true` before further copy. Null elts with `nelts>0`, cycles, and impossible metadata are host errors, not silent `truncated=false`. | `tests/ngx_request_read.rs` (`get_headers_*`) |
| `ngx.req.get_uri_args` | `ngx::req::get_uri_args(max_args: unknown) -> map` | adapted | `PHASE_REQ_GET_URI_ARGS` | default 100; hard 1024; 65536-byte query | Returns `{ args, truncated }` via the crate-private `decode_query_args` helper shared with `ngx::decode_args`. Trailing empty `&` at max_args is not truncated. `0` clamps to 1024. | `tests/ngx_request_read.rs`, `tests/ngx_codec.rs` |
| `ngx.req.get_post_args` | `ngx::req::get_post_args(max_args: unknown) -> map` | adapted | `PHASE_REQ_GET_POST_ARGS` | same arg caps; 65536-byte body | Sync parse **only** when `request_body` is already read and in-memory buffers exist. Returns `{ args, truncated }` with the shared decoder. Unread, still-reading, file-backed, discarded, incomplete, or corrupt bodies are structured host errors (async/file read is Task 10). Never initiates body read, I/O, or yield. Stub tests cover already-read buffers; real NGINX already-read success is stub-only until Task 10. | `tests/ngx_request_read.rs` (`get_post_args_*`) |
| `ngx.req.is_internal` | `ngx::req::is_internal() -> bool` | adapted | `PHASE_REQ_FLAGS` | none | Live C `r->internal` via a headers-compiled shim (`ngx_rustscript_request_is_internal`). nginx-sys bindgen bitfield getters are the wrong ABI; neighboring C flags such as `filter_finalize` must not read as internal (`REQ_INTERNAL_REDIR=true`). | `tests/ngx_request_read.rs`, `tests/ngx_request_abi.rs` |
| `ngx.req.start_time` | `ngx::req::start_time() -> float` | supported | `PHASE_REQ_FLAGS` | none | `start_sec + start_msec/1000`. | `tests/ngx_request_read.rs` |
| `ngx.is_subrequest` | `ngx::is_subrequest() -> bool` | supported | `PHASE_REQ_FLAGS` | none | `r != r->main`. Live false-path on main requests (`REQ_SUB=false`). `ngx_http_core_access_phase` skips all access handlers when `r != r->main`, so SSI/mirror/auth_request subrequests cannot run `access_by_rss_file`. True is stub-covered; SSI include still returns `sub-ok` as evidence the subrequest ran. | `tests/ngx_request_read.rs` |
| `ngx.headers_sent` | `ngx::headers_sent() -> bool` | supported | `PHASE_HEADERS_SENT` | none | Live C `r->header_sent` via `ngx_rustscript_request_headers_sent`. Access-phase scripts observe `false`. | `tests/ngx_request_read.rs`, `tests/ngx_request_abi.rs` |
| `ngx.var.VARIABLE` | `ngx::var::get(name: string) -> optional<string>` | adapted | `PHASE_VAR_GET` | 1024-byte name; 65536-byte value | Indexed lookup with `ngx_hash_strlow`. Missing is `null`; present empty is `""`. Rejects empty/NUL names. | `tests/ngx_request_read.rs` (`var_get_*`) |
| `ngx.req.set_method` | `ngx::req::set_method(method_id: int) -> null` | adapted | `PHASE_REQ_SET_METHOD` | OpenResty method IDs only | Pool-owned canonical `method_name` is allocated successfully before numeric/text fields change. Allocation failure leaves method untouched. Unsupported IDs fail before mutation. | `tests/ngx_request_mutation.rs`, `tests/ngx_task7_hardening.rs` |
| `ngx.req.set_uri` | `ngx::req::set_uri(uri: string, jump: unknown) -> null` | adapted | `PHASE_REQ_SET_URI` | 65536-byte URI | `jump` `null`/`false` rewrites `r->uri`, sets `internal`, clears `valid_unparsed_uri`/`valid_location`/`uri_changed`, and calls `ngx_http_set_exten` via the C shim. `jump=true` is rejected **before** mutation. Empty/unsafe bytes (0x00-0x08, 0x0A-0x1F, 0x7F) fail first. | `tests/ngx_request_mutation.rs` |
| `ngx.req.set_uri_args` | `ngx::req::set_uri_args(args: unknown) -> null` | adapted | `PHASE_REQ_SET_URI` | 65536-byte encoded args | String: escape 0x00-0x20 and 0x7F as `%XX`. Map: reuse `encode_args`. Pool-owned `r->args`; `valid_unparsed_uri=0`. | `tests/ngx_request_mutation.rs` |
| `ngx.req.set_header` | `ngx::req::set_header(name: string, value: unknown) -> null` | adapted | `PHASE_REQ_SET_HEADER` | 256-byte token name; 8192-byte value | `null` clears. String/array values. Token + no CRLF/NUL. Validates the list, then Host (`ngx_http_lua_validate_host` port-aware, updates `headers_in.server`, invalidates cached `$host`), Content-Length (`ngx_atoof` before mutation; every value must be a decimal; dedicated `n` uses the first), User-Agent browser flags, and Connection (`close` substring first, then `keep-alive`). Hash-zero then push. Dedicated Host/Content-Length/Content-Type/User-Agent/Connection/Cookie/Referer stay consistent. Allocation/list-push failure rolls back added entries. | `tests/ngx_request_mutation.rs`, `tests/ngx_task7_hardening.rs` |
| `ngx.req.clear_header` | `ngx::req::clear_header(name: string) -> null` | adapted | `PHASE_REQ_SET_HEADER` | same name policy | Hash-zero matching list entries and reset dedicated fields (`content_length_n=-1`, `connection_type=0`). | `tests/ngx_request_mutation.rs` |
| `ngx.var.VARIABLE` write | `ngx::var::set(name: string, value: unknown) -> null` | adapted | `PHASE_VAR_GET` | 1024-byte name; 65536-byte value | Indexed/set_handler write via C shim. Honors CHANGEABLE. `null` → `not_found`. Empty string is present empty. Read-only and unknown names fail. | `tests/ngx_request_mutation.rs` |

## Response metadata

`headers_out` hosts. Getters use capability `ResponseRead`; setters use
`ResponseWrite`. OpenResty v0.10.29 context tables list `log_by_lua*`
for both `ngx.status` and `ngx.header.HEADER` (read and write).
`log_by_rss_file` permits getters only. Setters are rejected after
`header_sent` and at Log config/runtime. Ordinary list plus dedicated
Content-Type/Length/Location/Last-Modified/ETag.

| OpenResty API | RustScript callable | Status | Phases | Limits | Semantic differences | Acceptance |
| --- | --- | --- | --- | --- | --- | --- |
| `ngx.status` | `ngx::status::get() -> int` | adapted | `PHASE_RESPONSE_META` | none | Prefers `err_status`, then `headers_out.status`, then HTTP/0.9 maps to `9`. Unset HTTP/1.x is `0`. Capability `ResponseRead`. Allowed in `log_by_rss_file`. | `tests/ngx_request_mutation.rs`, `tests/log_config.rs` |
| `ngx.status` | `ngx::status::set(status: int) -> null` | adapted | `PHASE_RESPONSE_META` | 0-999 | Rejected after headers sent. Capability `ResponseWrite`. Rejected by `log_by_rss_file`. | `tests/ngx_request_mutation.rs`, `tests/log_config.rs` |
| `ngx.header.HEADER` | `ngx::header::get(name: string) -> unknown` | adapted | `PHASE_RESPONSE_META` | 256-byte token | Missing `null`; one string; duplicates array. Owned copies. Content-Type/Content-Length prefer dedicated fields. Capability `ResponseRead`. Allowed in `log_by_rss_file`. | `tests/ngx_request_mutation.rs`, `tests/log_config.rs` |
| `ngx.header.HEADER` | `ngx::header::set(name: string, value: unknown) -> null` | adapted | `PHASE_RESPONSE_META` | 8192-byte value | `null` clears. Injection (CRLF/NUL/non-token) rejected. Relative Location does not set the dedicated pointer. New list slots are allocated before old matching names are deactivated. Capability `ResponseWrite`. Rejected by `log_by_rss_file`. | `tests/ngx_request_mutation.rs` |
| — | `ngx::header::set_all(headers: map) -> null` | adapted | `PHASE_RESPONSE_META` | same | Validate every pair first. Allocate the intended representation, then deactivate previous headers. Validation or allocation failure leaves the logical list unchanged (pool memory is not rolled back). | `tests/ngx_request_mutation.rs` |
| — | `ngx::header::clear() -> null` | adapted | `PHASE_RESPONSE_META` | — | Hash-zero every output header and reset dedicated fields. | `tests/ngx_request_mutation.rs` |

## Response output

Content-phase body APIs. Capability is `ResponseWrite`. Access and
config phases reject these imports at compile time; `ngx.status` /
`ngx.header` remain allowed in access. Payload is copied on the worker
thread into request-pool buffers and sent through
`ngx_http_output_filter`. Hosts never write the NGINX socket and never
call `ngx_http_finalize_request`. Per-call body cap is 1 MiB; total
response cap is 8 MiB; chunk cap is 4096.

| OpenResty API | RustScript callable | Status | Phases | Limits | Semantic differences | Acceptance |
| --- | --- | --- | --- | --- | --- | --- |
| `ngx.send_headers` | `ngx::send_headers() -> null` | adapted | content | — | Idempotent after `header_sent`. Default status 200 and `ngx_http_set_content_type`. | `tests/ngx_output.rs`, `tests/output_nginx.rs` |
| `ngx.print` | `ngx::print(data: unknown) -> null` | adapted | content | 1 MiB/call | String/bytes/int/float/bool/null/array. Nested maps rejected. Auto-sends headers. HEAD/`header_only` skips body. | `tests/ngx_output.rs`, `tests/output_nginx.rs` |
| `ngx.say` | `ngx::say(data: unknown) -> null` | adapted | content | 1 MiB/call | `print` plus a trailing newline. | `tests/ngx_output.rs`, `tests/output_nginx.rs` |
| `ngx.flush` | `ngx::flush(wait: bool) -> null` | adapted | content | — | Empty flush buf. HTTP/1.0 buffering returns `buffering`. `wait=true` models write backpressure via request-owned Waiting/eventfd (no request pointers in the future). | `tests/ngx_output.rs`, `tests/output_nginx.rs` |
| `ngx.eof` | `ngx::eof() -> null` | adapted | content | — | `last_buf` on the main request, `last_in_chain` always. Post-eof print/say/flush/eof fail. | `tests/ngx_output.rs`, `tests/output_nginx.rs` |

HTTP/1.0 without Content-Length buffers until eof/`send_headers` then
sets `content_length_n`. HTTP/1.1 streams with chunked encoding when
length is unknown. The content handler remains the sole completion
owner and sends last_buf if the script omitted `eof`.

## Control flow

Hosts set request-owned `RequestDisposition` then stop the VM. AccessExec consumes disposition only when it is set. Ordinary HostError cannot forge control. First disposition wins. The access handler is the sole finalizer: it returns the NGINX status; hosts never call `ngx_http_finalize_request`.

| OpenResty API | RustScript callable | Status | Phases | Limits | Semantic differences | Acceptance |
| --- | --- | --- | --- | --- | --- | --- |
| `ngx.exit` | `ngx::exit(status: int) -> null` | adapted | `PHASE_EXIT` | 0, -1, -5, 100-599 | Stops VM immediately. `0` continues the access phase (`NGX_OK`). `-5` is `NGX_DECLINED`. HTTP 201/204 and `>=300` finalize via the access return. HTTP 200-299 except 201/204 send an empty response then return (NGINX does not special-case 200). `NGX_AGAIN`/`NGX_DONE` rejected. | `tests/ngx_control_flow.rs` |
| `ngx.redirect` | `ngx::redirect(uri: string, status: unknown) -> null` | adapted | `PHASE_REDIRECT` | 65536-byte URI; 301/302/303/307/308 | `status` `null` defaults to 302. Validates scope, Continue disposition, headers-not-sent, URI, and status before mutating Location/status/disposition together. First wins. | `tests/ngx_control_flow.rs`, `tests/ngx_task7_hardening.rs` |

`ngx.exec` is **not** a production host. Import is rejected at config time. `set_uri(jump=true)` is a runtime rejection before mutation.

`PHASE_REQ_SET_METHOD`: `set`, `rewrite`, `access`, `content`, `header_filter`.
`PHASE_REQ_SET_URI`: `set`, `rewrite`, `access`, `content`, `header_filter`, `body_filter`.
`PHASE_REQ_SET_HEADER`: `set`, `rewrite`, `access`, `content`, `header_filter`, `body_filter`, `log`, `balancer`.
`PHASE_EXIT`: `rewrite`, `access`, `content`, `header_filter`, `timer`, `balancer`, `ssl_certificate`, `ssl_session_fetch`, `ssl_session_store`.
`PHASE_REDIRECT`: `rewrite`, `access`, `content`.

Mutation and control programs use request-owned `AccessExec` (capability-based, not host-name matching). Pure sync request-read stays pooled.

## Request-local context

`ngx.ctx` is adapted as four explicit hosts rather than a Lua table. The map
is owned by `AccessExec` (request lifetime). Hosts clone deep-owned VM values
out; callables and other non-owning runtime values are rejected. Empty and
embedded-NUL keys are rejected. `delete` returns the removed value, or `null`
if missing (OpenResty uses `ngx.ctx.key = nil`). Configuration phases cannot
import these hosts. Capability is `RequestCtx` only — ctx hosts do not read
the request pointer.

| OpenResty API | RustScript callable | Status | Phases | Limits | Semantic differences | Acceptance |
| --- | --- | --- | --- | --- | --- | --- |
| `ngx.ctx` | `ngx::ctx::get(key: string) -> unknown` | adapted | `PHASE_CTX` | missing → `null` | Explicit get; missing is `null`. | `tests/ngx_request_ctx.rs` |
| `ngx.ctx` | `ngx::ctx::set(key: string, value: unknown) -> null` | adapted | `PHASE_CTX` | 256 keys; 256-byte key; depth 16; 65536 total bytes; 1024 elems/container; 8192-byte scalar | Failed set leaves the previous value intact. Shared aliases are deep-cloned and counted per occurrence. | `tests/ngx_request_ctx.rs` |
| `ngx.ctx` | `ngx::ctx::delete(key: string) -> unknown` | adapted | `PHASE_CTX` | same key policy | Returns the removed value, or `null` if missing. | `tests/ngx_request_ctx.rs` |
| `ngx.ctx` | `ngx::ctx::clear() -> null` | adapted | `PHASE_CTX` | — | Drops every entry. OpenResty would assign a new table. | `tests/ngx_request_ctx.rs` |

`PHASE_CTX` (OpenResty v0.10.29 `ngx.ctx` contexts): `set`, `rewrite`,
`access`, `content`, `header_filter`, `body_filter`, `log`, `timer`,
`balancer`, `ssl_certificate`, `ssl_session_fetch`, `ssl_session_store`,
`ssl_client_hello`. Not `init` / `init_worker` / `exit_worker`.

The map persists across `ngx::sleep` `Waiting`/resume cycles on the same
`AccessExec`, including two sequential waits. Ctx-only scripts that never
wait are also request-owned: every program that imports RequestCtx,
Cosocket, or Async hosts runs on a per-request `AccessExec` rather than
the pooled VM path.
Concurrent requests are isolated. Values are released on cleanup, HostError,
cancellation, and `AccessExec` drop. Ctx hosts run only on the worker main
thread inside VM run/resume; polling the Tokio sleep future receives no ctx
pointer, and a stale wake cannot access a dropped ctx.

## Shared dictionary

Directive `rss_shared_dict <name> <size>` is HTTP main only (`TAKE2`).
Minimum size is 8k. Names are unique, non-empty, UTF-8, at most 256
bytes. Duplicate names (same or different size) are rejected. The zone
is NGINX shared memory with a slab-backed rbtree/LRU store, the
`init(shm_zone, old_data)` reload contract, and a cross-worker mutex.
`ngx::shared::dict(name)` returns a typed handle; later methods take
that handle. Multi-return OpenResty values become maps with fixed keys
(`value` / `flags` / `stale` / `ok` / `error` / `forcible`; incr is
`{value, error, forcible}` only). Optional `exptime` and `flags` are
explicit `null`. Values are bool, int, float, string, or bytes;
`set(..., null, ...)` deletes. `add`/`replace` reject null.
Keys are at most 65535 bytes; values at most 65536 bytes.
Empty keys are rejected. LRU eviction reports `forcible`; `safe_*`
never evicts and returns `no memory`. Shared dict hosts do not require
request-owned VMs.

| OpenResty API | RustScript callable | Status | Phases | Limits | Semantic differences | Acceptance |
| --- | --- | --- | --- | --- | --- | --- |
| `ngx.shared.DICT` | `ngx::shared::dict(name: string) -> SharedDictHandle` | adapted | `PHASE_DICT_LOOKUP` | unknown zone is a host error | Typed handle, not `ngx.shared.NAME` field | `tests/ngx_shared_dict.rs` |
| `ngx.shared.DICT.get` | `ngx::shared::get(dict, key) -> map` | adapted | `PHASE_DICT_GET` | missing → `{value: null}` | Map `{value, flags, stale, error}` | `tests/ngx_shared_dict.rs` |
| `ngx.shared.DICT.get_stale` | `ngx::shared::get_stale(dict, key) -> map` | adapted | `PHASE_DICT_GET` | stale expired values | `stale` is bool | `tests/ngx_shared_dict.rs` |
| `ngx.shared.DICT.set` | `ngx::shared::set(dict, key, value, exptime, flags) -> map` | adapted | `PHASE_DICT_WRITE` | LRU up to 30 | `{ok, error, forcible}` | `tests/ngx_shared_dict.rs` |
| `ngx.shared.DICT.safe_set` | `ngx::shared::safe_set(...)` | adapted | `PHASE_DICT_WRITE` | no eviction | `{ok, error}` | `tests/ngx_shared_dict.rs` |
| `ngx.shared.DICT.add` | `ngx::shared::add(...)` | adapted | `PHASE_DICT_WRITE` | exists → error | `{ok, error, forcible}` | `tests/ngx_shared_dict.rs` |
| `ngx.shared.DICT.safe_add` | `ngx::shared::safe_add(...)` | adapted | `PHASE_DICT_WRITE` | no eviction | `{ok, error}` | `tests/ngx_shared_dict.rs` |
| `ngx.shared.DICT.replace` | `ngx::shared::replace(...)` | adapted | `PHASE_DICT_WRITE` | missing → not found | `{ok, error, forcible}` | `tests/ngx_shared_dict.rs` |
| `ngx.shared.DICT.delete` | `ngx::shared::delete(dict, key) -> null` | adapted | `PHASE_DICT_WRITE` | — | Missing delete is success | `tests/ngx_shared_dict.rs` |
| `ngx.shared.DICT.incr` | `ngx::shared::incr(dict, key, value, init, init_ttl) -> map` | adapted | `PHASE_DICT_WRITE` | numbers only | `{value, error, forcible}` | `tests/ngx_shared_dict.rs` |
| `ngx.shared.DICT.ttl` | `ngx::shared::ttl(dict, key) -> map` | adapted | `PHASE_DICT_WRITE` | never-expire is `0` | `{value, error}` | `tests/ngx_shared_dict.rs` |
| `ngx.shared.DICT.expire` | `ngx::shared::expire(dict, key, exptime) -> map` | adapted | `PHASE_DICT_WRITE` | — | `{ok, error}` | `tests/ngx_shared_dict.rs` |
| `ngx.shared.DICT.capacity` | `ngx::shared::capacity(dict) -> int` | adapted | `PHASE_DICT_WRITE` | — | Zone size in bytes | `tests/ngx_shared_dict.rs` |
| `ngx.shared.DICT.free_space` | `ngx::shared::free_space(dict) -> int` | adapted | `PHASE_DICT_WRITE` | approximate | Process-local: capacity − used; shm: `pfree * pagesize` | `tests/ngx_shared_dict.rs` |
| `ngx.shared.DICT.lpush` | `ngx::shared::lpush(dict, key, value) -> map` | adapted | `PHASE_DICT_WRITE` | list len ≤ 65536; no LRU eviction | `{value: len, error}`; bool/null/map → `bad value type`; scalar conflict → `value not a list` without mutating the node | `tests/ngx_shared_dict_list.rs` |
| `ngx.shared.DICT.rpush` | `ngx::shared::rpush(dict, key, value) -> map` | adapted | `PHASE_DICT_WRITE` | same | Same map as lpush; appends | `tests/ngx_shared_dict_list.rs` |
| `ngx.shared.DICT.lpop` | `ngx::shared::lpop(dict, key) -> map` | adapted | `PHASE_DICT_WRITE` | decode before unlink | `{value, flags, stale, error}`; empty/missing → `{value: null}`; decode failure → `{error: "bad value type"}` and the item stays queued | `tests/ngx_shared_dict_list.rs` |
| `ngx.shared.DICT.rpop` | `ngx::shared::rpop(dict, key) -> map` | adapted | `PHASE_DICT_WRITE` | same | Same map as lpop; pops tail | `tests/ngx_shared_dict_list.rs` |
| `ngx.shared.DICT.llen` | `ngx::shared::llen(dict, key) -> map` | adapted | `PHASE_DICT_WRITE` | — | Missing/expired → `{value: 0}`; scalar → `value not a list` | `tests/ngx_shared_dict_list.rs` |
| `ngx.shared.DICT.flush_all` | `ngx::shared::flush_all(dict) -> null` | adapted | `PHASE_DICT_WRITE` | LRU; ≤1024 visits/call | Each call first frees already-expired keys (visit-capped), then marks live LRU keys with `expires=1` (visit-capped), then `expire(0)`. Repeated calls make monotonic progress across >1024 keys; one call does not wipe the zone. | `tests/ngx_shared_dict_list.rs` |
| `ngx.shared.DICT.flush_expired` | `ngx::shared::flush_expired(dict, max) -> int` | adapted | `PHASE_DICT_WRITE` | `max` 0/null still ≤ 1024 results; visits ≤ 1024 | Freed count. Walk stops at the result cap *or* the hard visit cap, whichever comes first. OpenResty `0` is unlimited. | `tests/ngx_shared_dict_list.rs` |
| `ngx.shared.DICT.get_keys` | `ngx::shared::get_keys(dict, max) -> array<string>` | adapted | `PHASE_DICT_WRITE` | read-only; `max` 0/null still ≤ 1024 results; visits ≤ 1024 | Oldest-first, skips expired, never deletes or force-expires. OpenResty `0` returns all keys; this project keeps both a result cap and a visit cap. | `tests/ngx_shared_dict_list.rs` |

List nodes are slab-owned; every mutation is atomic under the shm mutex. Failed item allocation rolls back an empty list. `get`/`incr` on a list return structured errors (`value is a list` / `not a number`) and never overwrite the node. List items are int/float/string/bytes. `get_keys` is read-only under the shm mutex: it never force-expires or deletes, skips expired keys, and stops at both the result cap and a 1024-visit cap. `flush_all` / `flush_expired` likewise bound visits independently of hits. There is no process-local production fallback: `cargo test` uses an in-process zone; the cdylib talks to NGINX shm only.

Cross-worker visibility, atomic incr, and reload reuse are covered by
`tests/shared_dict_nginx.rs`. Config parsing is `tests/shared_dict_config.rs`.

## UDP cosocket

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
| `udpsock:bind` | `ngx::socket::udp::bind(sock, host, port) -> null` | adapted | `PHASE_SOCKET_UDP` | Waiting | host string/`null`; port 0–65535 | `null`/`*` bind any IPv4. | `tests/ngx_udp_socket.rs`, `tests/socket_lifecycle.rs` |
| `udpsock:setpeername` | `ngx::socket::udp::setpeername(sock, host, port) -> null` | adapted | `PHASE_SOCKET_UDP` | Waiting | host string ≤255 bytes; port 0–65535 | Tokio DNS; auto-bind if needed. | `tests/ngx_udp_socket.rs`, `tests/socket_lifecycle.rs` |
| `udpsock:send` | `ngx::socket::udp::send(sock, data) -> null` | adapted | `PHASE_SOCKET_UDP` | Waiting | 65535-byte payload; string or bytes | Requires `setpeername`. | `tests/ngx_udp_socket.rs`, `tests/socket_lifecycle.rs` |
| `udpsock:receive` | `ngx::socket::udp::receive(sock, size) -> bytes` | adapted | `PHASE_SOCKET_UDP` | Waiting | default 8192; max 65535; `null` size | Oversized datagrams error instead of silent truncate. | `tests/ngx_udp_socket.rs`, `tests/socket_lifecycle.rs` |
| `udpsock:close` | `ngx::socket::udp::close(sock) -> null` | adapted | `PHASE_SOCKET_UDP` | sync | — | Double close is HostError. Cosocket request-owned. | `tests/ngx_udp_socket.rs`, `tests/socket_lifecycle.rs` |
| `udpsock:settimeout` | `ngx::socket::udp::settimeout(sock, ms) -> null` | adapted | `PHASE_SOCKET_UDP` | sync | non-negative ms; `0` = no timer | Applies to subsequent ops; default 60000. | `tests/ngx_udp_socket.rs`, `tests/socket_lifecycle.rs` |

`PHASE_SOCKET_UDP` (OpenResty v0.10.29 `ngx.socket.udp` contexts):
`rewrite`, `access`, `content`, `timer`, `ssl_certificate`,
`ssl_session_fetch`. Same as sleep.

UDP programs are request-owned (`HostFamily::Cosocket`), including the
sync constructor, `close`, and `settimeout`. Timeout, cancel, shutdown,
generation, DNS, IPv6, workers, and handle reuse after close are covered
by `tests/ngx_udp_socket.rs` and `tests/socket_lifecycle.rs`. NGINX
acceptance is `tests/nginx_integration.py`.

## TCP cosocket

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
exact size, `null`/`"*l"` line (CR ignored), and `"*a"` until peer
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
| `tcpsock:connect` | `ngx::socket::tcp::connect(sock, host, port) -> null` | adapted | `PHASE_SOCKET_UDP` | Waiting | host ≤255; `unix:` path ≤107 with port 0; port 0–65535 | Tokio DNS/connect; IPv4/IPv6/Unix. | `tests/ngx_tcp_socket.rs`, `tests/socket_lifecycle.rs` |
| `ngx.socket.connect` | `ngx::socket::connect(host, port) -> resource` | adapted | `PHASE_SOCKET_UDP` | Waiting | same as connect | Creates the TCP resource then connects. | `tests/ngx_tcp_socket.rs` |
| `tcpsock:send` | `ngx::socket::tcp::send(sock, data) -> null` | adapted | `PHASE_SOCKET_UDP` | Waiting | 1048576-byte payload; string or bytes | Partial-write loop until complete. Transport timeout is HostError; the socket stays connected, but HostError aborts the current script/request. | `src/socket/tcp.rs`, `tests/ngx_tcp_socket.rs` |
| `tcpsock:receive` | `ngx::socket::tcp::receive(sock, mode) -> bytes` | adapted | `PHASE_SOCKET_UDP` | Waiting | exact/`null`/`*l`/`*a`; max 1048576 | Line ignores CR. `*a` succeeds on peer close. Transport timeout is HostError; the socket stays connected, but HostError aborts the current script/request. | `src/socket/tcp.rs`, `tests/ngx_tcp_socket.rs` |
| `tcpsock:receiveany` | `ngx::socket::tcp::receiveany(sock, max) -> bytes` | adapted | `PHASE_SOCKET_UDP` | Waiting | 1–1048576 | Returns unread first; one kernel read otherwise. | `tests/ngx_tcp_socket.rs` |
| `tcpsock:receiveuntil` | `ngx::socket::tcp::receiveuntil(sock, delimiter) -> bytes` | adapted | `PHASE_SOCKET_UDP` | Waiting | delimiter 1–256 bytes | Single-shot, exclusive delimiter, unread preserved. | `tests/ngx_tcp_socket.rs` |
| `tcpsock:close` | `ngx::socket::tcp::close(sock) -> null` | adapted | `PHASE_SOCKET_UDP` | sync | — | Double close is HostError. Cosocket request-owned. | `tests/ngx_tcp_socket.rs` |
| `tcpsock:settimeout` | `ngx::socket::tcp::settimeout(sock, ms) -> null` | adapted | `PHASE_SOCKET_UDP` | sync | non-negative ms; `0` = no timer | Applies to subsequent ops; default 60000. | `tests/ngx_tcp_socket.rs` |
| `tcpsock:setkeepalive` | `ngx::socket::tcp::setkeepalive(sock, timeout, pool_size) -> null` | adapted | `PHASE_SOCKET_UDP` | sync | timeout ms (`null`=60000, `0`=never); pool_size `null`=30, 1–256; max 32 keys / 256 conns / 1000 reuse | Worker-local LRU pool keyed by endpoint+TLS identity. Detaches IO from the request so cancel/close cannot drop a pooled socket. Rejects pending ops, unread buffers, EOF, and dirty sockets. | `tests/ngx_tcp_keepalive.rs` |
| `tcpsock:getreusedtimes` | `ngx::socket::tcp::getreusedtimes(sock) -> int` | adapted | `PHASE_SOCKET_UDP` | sync | — | 0 until checkout; closed is HostError. | `tests/ngx_tcp_keepalive.rs` |
| `tcpsock:sslhandshake` | `ngx::socket::tcp::sslhandshake(sock, reused_session, server_name, ssl_verify, send_status_req) -> null` | adapted | `PHASE_SOCKET_UDP` | Waiting | SNI ≤255; verify defaults true | Tokio/rustls only. No NGINX SSL FFI. Session userdata is not returned; `reused_session` session handles are a structured unsupported error. `null`/`true` keep automatic same-identity rustls resumption; `false` disables it. rustls always sends OCSP `status_request`; `send_status_req=false` is unsupported. TLS pool checkout happens only during handshake after endpoint+SNI+verify+trust+client identity match. | `tests/ngx_tcp_tls.rs` |
| — | `ngx::socket::tcp::trustedstore(pem) -> resource` | adapted | `PHASE_SOCKET_UDP` | sync | PEM ≤65536 | Parses PEM into a VM-owned trusted-store resource. PEM is never logged. | `tests/ngx_tcp_tls.rs` |
| `tcpsock:settrustedstore` | `ngx::socket::tcp::settrustedstore(sock, store) -> null` | adapted | `PHASE_SOCKET_UDP` | sync | typed store or `null` | Attaches a trusted-store resource (or clears). Default is system roots then webpki-roots. | `tests/ngx_tcp_tls.rs` |
| — | `ngx::socket::tcp::clientcert(cert, pkey) -> resource` | adapted | `PHASE_SOCKET_UDP` | sync | PEM ≤65536 each | Parses cert+key into a VM-owned resource. Private key is parsed through zeroizing storage and retained only as rustls `PrivateKeyDer`. Cert/key mismatch fails at construction. | `tests/ngx_tcp_tls.rs` |
| `tcpsock:setclientcert` | `ngx::socket::tcp::setclientcert(sock, cert) -> null` | adapted | `PHASE_SOCKET_UDP` | sync | typed clientcert or `null` | Attaches a client-cert resource (or clears). OpenResty two-PEM method is adapted to one typed resource. | `tests/ngx_tcp_tls.rs` |

TCP programs are request-owned (`HostFamily::Cosocket`). Lifecycle,
timeout, cancel, shutdown, DNS, IPv4/IPv6/Unix, concurrent
read+write / duplicate same-direction ops, and handle reuse are covered
by `tests/ngx_tcp_socket.rs` and `tests/socket_lifecycle.rs`. Keepalive
pool, worker isolation, and TLS verification are covered by
`tests/ngx_tcp_keepalive.rs` and `tests/ngx_tcp_tls.rs`. NGINX
acceptance is `tests/nginx_integration.py`.

## Out of scope

Not production hosts in this matrix (do not import them):

- Remaining body APIs (`ngx.req.read_body`)
- `ngx.exec` / internal redirect (`set_uri jump=true`)
- Timer callbacks, light threads
- Stream subsystem, `ndk.set_var.*`, Lua `coroutine.*`
- `tcpsock:getsslpointer` / `tcpsock:getsslctx` (no NGINX SSL FFI)
- `print`, `runtime::sleep`, and test hosts `ngx::probe` / `ngx::ready` /
  `ngx::fail`

Catalog phase names other than `access`, `content`, and `log` document
OpenResty allowlists for later directives. They are not runnable entry
points in this matrix. `log_by_rss_file` is the executable Log-phase
directive: synchronous and fail-closed. Config uses a closed positive
allowlist (family + capability + name). Codec is included because
OpenResty documents those APIs for `log_by_lua*`. Response getters are
included from the same OpenResty context tables; setters stay rejected.
The handler runs when NGINX invokes the log phase for the current
request (`r->loc_conf` after rewrite / `error_page`). Subrequests
require core `log_subrequest on` on the subrequest location
(`ngx_http_finalize_request` in NGINX 1.30.4). `Waiting` logs `log handler cannot wait`,
drops the VM, and never arms eventfd, a timer, or `r->main->count`.

## Maintaining this matrix

Update this file whenever a production manifest API, catalog signature,
allowed phase, limit, yield behavior (sync vs Waiting), or semantic
adaptation changes. Cross-check `ngx_host_manifest()`,
`ngx_host_catalog()`, and `production_runtime_host_names()` so no
production host is omitted and no extra name is documented.
