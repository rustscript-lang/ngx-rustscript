# Response metadata

[Documentation](../index.md) · [Directives](../directives.md) · [API](index.md) · [Execution model](../execution-model.md) · [Security](../security.md) · [Examples](../examples/index.md) · [Compatibility](../ngx-api-compatibility.md)

`headers_out` hosts. Getters use capability `ResponseRead`; setters use
`ResponseWrite`. OpenResty v0.10.29 context tables list `log_by_lua*`
for both `ngx.status` and `ngx.header.HEADER` (read and write).
`log_by_rss_file` permits getters only. Setters are rejected after
`header_sent` and at Log config/runtime. Ordinary list plus dedicated
Content-Type/Length/Location/Last-Modified/ETag.

| OpenResty API | RustScript callable | Status | Phases | Limits | Semantic differences | Acceptance |
| --- | --- | --- | --- | --- | --- | --- |
| `ngx.status` | `ngx::status::get() -> int` | adapted | `PHASE_RESPONSE_META` | none | Prefers `err_status`, then `headers_out.status`, then HTTP/0.9 maps to `9`. Unset HTTP/1.x is `0`. Capability `ResponseRead`. Allowed in `log_by_rss_file`. | `tests/ngx_request_mutation.rs`, `tests/log_config.rs` |
| `ngx.status` | `ngx::status::set(status: int) -> ()` | adapted | `PHASE_RESPONSE_META` | 0-999 | Rejected after headers sent. Capability `ResponseWrite`. Rejected by `log_by_rss_file`. | `tests/ngx_request_mutation.rs`, `tests/log_config.rs` |
| `ngx.header.HEADER` | `ngx::header::get(name: string) -> Option<string>` | adapted | `PHASE_RESPONSE_META` | 256-byte token | Missing is `None`; duplicate names yield the first value. Owned copies. Content-Type/Content-Length prefer dedicated fields. Capability `ResponseRead`. Allowed in `log_by_rss_file`. | `tests/ngx_request_mutation.rs`, `tests/log_config.rs` |
| `ngx.header.HEADER` | `ngx::header::set(name: string, value: unknown) -> ()` | adapted | `PHASE_RESPONSE_META` | 8192-byte value | `()` clears. Injection (CRLF/NUL/non-token) rejected. Relative Location does not set the dedicated pointer. New list slots are allocated before old matching names are deactivated. Capability `ResponseWrite`. Rejected by `log_by_rss_file`. | `tests/ngx_request_mutation.rs` |
| — | `ngx::header::set_all(headers: map) -> ()` | adapted | `PHASE_RESPONSE_META` | same | Validate every pair first. Allocate the intended representation, then deactivate previous headers. Validation or allocation failure leaves the logical list unchanged (pool memory is not rolled back). | `tests/ngx_request_mutation.rs` |
| — | `ngx::header::clear() -> ()` | adapted | `PHASE_RESPONSE_META` | — | Hash-zero every output header and reset dedicated fields. | `tests/ngx_request_mutation.rs` |


---

See also: [API index](index.md) · [Phase sets](index.md#phase-sets) · [Cross-cutting adaptations](index.md#cross-cutting-adaptations) · [Hard caps](index.md#hard-caps)
