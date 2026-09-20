# Constants

[Documentation](../index.md) · [Directives](../directives.md) · [API](index.md) · [Execution model](../execution-model.md) · [Security](../security.md) · [Examples](../examples/index.md) · [Compatibility](../ngx-api-compatibility.md)

Pinned `pd-host-function` has no host-constant form. Lua fields such as
`ngx.OK` are zero-argument functions. All constant hosts are **adapted**,
**sync**, and have no extra numeric caps.

Integer constants match `nginx-sys` for official NGINX 1.31.4 except
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


---

See also: [API index](index.md) · [Phase sets](index.md#phase-sets) · [Cross-cutting adaptations](index.md#cross-cutting-adaptations) · [Hard caps](index.md#hard-caps)
