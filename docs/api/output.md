# Response output

[Documentation](../index.md) · [Directives](../directives.md) · [API](index.md) · [Execution model](../execution-model.md) · [Security](../security.md) · [Examples](../examples/index.md) · [Compatibility](../ngx-api-compatibility.md)

Content-phase body APIs. Capability is `ResponseWrite`. Access and
config phases reject these imports at compile time; `ngx.status` /
`ngx.header` remain allowed in access. Payload is copied on the worker
thread into request-pool buffers and sent through
`ngx_http_output_filter`. Hosts never write the NGINX socket and never
call `ngx_http_finalize_request`. Per-call body cap is 1 MiB; total
response cap is 8 MiB; chunk cap is 4096.

| OpenResty API | RustScript callable | Status | Phases | Limits | Semantic differences | Acceptance |
| --- | --- | --- | --- | --- | --- | --- |
| `ngx.send_headers` | `ngx::send_headers() -> ()` | adapted | content | — | Idempotent after `header_sent`. Default status 200 and `ngx_http_set_content_type`. | `tests/ngx_output.rs`, `tests/output_nginx.rs` |
| `ngx.print` | `ngx::print(data: unknown) -> ()` | adapted | content | 1 MiB/call | String/bytes/int/float/bool/`()`/array. Nested maps rejected. Auto-sends headers. HEAD/`header_only` skips body. | `tests/ngx_output.rs`, `tests/output_nginx.rs` |
| `ngx.say` | `ngx::say(data: unknown) -> ()` | adapted | content | 1 MiB/call | `print` plus a trailing newline. | `tests/ngx_output.rs`, `tests/output_nginx.rs` |
| `ngx.flush` | `ngx::flush(wait: bool) -> ()` | adapted | content | — | Empty flush buf. HTTP/1.0 buffering returns `buffering`. `wait=true` models write backpressure via request-owned Waiting/eventfd (no request pointers in the future). | `tests/ngx_output.rs`, `tests/output_nginx.rs` |
| `ngx.eof` | `ngx::eof() -> ()` | adapted | content | — | `last_buf` on the main request, `last_in_chain` always. Post-eof print/say/flush/eof fail. | `tests/ngx_output.rs`, `tests/output_nginx.rs` |

HTTP/1.0 without Content-Length buffers until eof/`send_headers` then
sets `content_length_n`. HTTP/1.1 streams with chunked encoding when
length is unknown. The content handler remains the sole completion
owner and sends last_buf if the script omitted `eof`.


---

See also: [API index](index.md) · [Phase sets](index.md#phase-sets) · [Cross-cutting adaptations](index.md#cross-cutting-adaptations) · [Hard caps](index.md#hard-caps)
