# Sleep

[Documentation](../index.md) · [Directives](../directives.md) · [API](index.md) · [Execution model](../execution-model.md) · [Security](../security.md) · [Examples](../examples/index.md) · [Compatibility](../ngx-api-compatibility.md)

| OpenResty API | RustScript callable | Status | Phases | Yield | Limits | Semantic differences | Acceptance |
| --- | --- | --- | --- | --- | --- | --- | --- |
| `ngx.sleep` | `ngx::sleep(seconds: number) -> null` | adapted | `SLEEP_PHASES` | Waiting | in-flight 1024; rejects negative / NaN / infinity / overflow | Seconds (int or float), not milliseconds. Tokio timer; completion via `HostFuture` -> waker -> eventfd -> worker-main-thread `resume`. Client close during wait is HTTP 499. | `tests/vm_sleep.rs`, `tests/host_catalog.rs`, `tests/access_async.rs`, `tests/lifecycle.rs`, `tests/nginx_integration.py` |
| `ngx.sleep` (legacy ms form) | `ngx::sleep_ms(ms: int) -> null` | adapted | `SLEEP_PHASES` | Waiting | in-flight 1024; `ms >= 0` | Not an OpenResty name. Integer milliseconds for scripts written before the seconds migration. Same Waiting path as `ngx::sleep`. | `tests/vm_sleep.rs`, `tests/access_async.rs`, `tests/request_driver.rs`, `tests/lifecycle.rs` |


---

See also: [API index](index.md) · [Phase sets](index.md#phase-sets) · [Cross-cutting adaptations](index.md#cross-cutting-adaptations) · [Hard caps](index.md#hard-caps)
