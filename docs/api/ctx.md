# Request-local context

[Documentation](../index.md) · [Directives](../directives.md) · [API](index.md) · [Execution model](../execution-model.md) · [Security](../security.md) · [Examples](../examples/index.md) · [Compatibility](../ngx-api-compatibility.md)

`ngx.ctx` is adapted as four explicit hosts rather than a Lua table. The map
is owned by `AccessExec` (request lifetime). Values are `string`; a callable
or any non-string value is rejected. Empty and embedded-NUL keys are
rejected. `delete` returns `Option<string>`: `Some(removed)` or
`None` when the key is missing (OpenResty uses `ngx.ctx.key = nil`).
Configuration phases cannot import these hosts. Capability is `RequestCtx`
only — ctx hosts do not read the request pointer.

| OpenResty API | RustScript callable | Status | Phases | Limits | Semantic differences | Acceptance |
| --- | --- | --- | --- | --- | --- | --- |
| `ngx.ctx` | `ngx::ctx::get(key: string) -> Option<string>` | adapted | `PHASE_CTX` | missing → `None` | Explicit get; missing is `None`, a stored empty string is `Some("")`. | `tests/ngx_request_ctx.rs` |
| `ngx.ctx` | `ngx::ctx::set(key: string, value: string) -> ()` | adapted | `PHASE_CTX` | 256 keys; 256-byte key; 8192-byte value | Failed set leaves the previous value intact. | `tests/ngx_request_ctx.rs` |
| `ngx.ctx` | `ngx::ctx::delete(key: string) -> Option<string>` | adapted | `PHASE_CTX` | same key policy | Returns `Some(removed)` or `None` if missing. | `tests/ngx_request_ctx.rs` |
| `ngx.ctx` | `ngx::ctx::clear() -> ()` | adapted | `PHASE_CTX` | — | Drops every entry. OpenResty would assign a new table. | `tests/ngx_request_ctx.rs` |

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


---

See also: [API index](index.md) · [Phase sets](index.md#phase-sets) · [Cross-cutting adaptations](index.md#cross-cutting-adaptations) · [Hard caps](index.md#hard-caps)
