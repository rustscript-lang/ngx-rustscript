# Metadata, worker, config, and phase

[Documentation](../index.md) · [Directives](../directives.md) · [API](index.md) · [Execution model](../execution-model.md) · [Security](../security.md) · [Examples](../examples/index.md) · [Compatibility](../ngx-api-compatibility.md)

All rows are sync zero-arg hosts except `ngx::log` (next section).
Cycle-dependent hosts fail with a host error when `ngx_cycle` is null.

| OpenResty API | RustScript callable | Status | Phases | Semantic differences | Acceptance |
| --- | --- | --- | --- | --- | --- |
| `ngx.get_phase` | `ngx::get_phase() -> string` | adapted | `PHASE_ALL` | Reads the worker-thread execution scope. Fails outside a scope. `ssl_certificate` is returned as `ssl_cert`. Executable request handlers report `access`, `content`, or `log` according to the active directive. | `tests/ngx_meta.rs` (`get_phase_reads_execution_scope_and_fails_outside_a_request`), `tests/access_nginx.rs`, `tests/content_nginx.rs`, `tests/log_nginx.rs` |
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


---

See also: [API index](index.md) · [Phase sets](index.md#phase-sets) · [Cross-cutting adaptations](index.md#cross-cutting-adaptations) · [Hard caps](index.md#hard-caps)
