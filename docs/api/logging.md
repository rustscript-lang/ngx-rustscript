# Logging

[Documentation](../index.md) · [Directives](../directives.md) · [API](index.md) · [Execution model](../execution-model.md) · [Security](../security.md) · [Examples](../examples/index.md) · [Compatibility](../ngx-api-compatibility.md)

| OpenResty API | RustScript callable | Status | Phases | Yield | Limits | Semantic differences | Acceptance |
| --- | --- | --- | --- | --- | --- | --- | --- |
| `ngx.log` | `ngx::log(level: int, message: string) -> null` | adapted | `PHASE_ALL` | sync | level `NGX_LOG_STDERR..=NGX_LOG_DEBUG`; no embedded NUL | Single string, not varargs. Request log when a request execution scope is live and allows `LOG`; otherwise cycle log; error if neither. Prefix `ngx-rustscript: `. No global `print`. | `tests/ngx_meta.rs` (`log_*`), `tests/access_nginx.rs` (`access_phase_logs_*`, `access_phase_rejects_invalid_log_level_and_embedded_nul`), `tests/log_nginx.rs` |


---

See also: [API index](index.md) · [Phase sets](index.md#phase-sets) · [Cross-cutting adaptations](index.md#cross-cutting-adaptations) · [Hard caps](index.md#hard-caps)
