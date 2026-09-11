# Control flow

[Documentation](../index.md) · [Directives](../directives.md) · [API](index.md) · [Execution model](../execution-model.md) · [Security](../security.md) · [Examples](../examples/index.md) · [Compatibility](../ngx-api-compatibility.md)

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


---

See also: [API index](index.md) · [Phase sets](index.md#phase-sets) · [Cross-cutting adaptations](index.md#cross-cutting-adaptations) · [Hard caps](index.md#hard-caps)
