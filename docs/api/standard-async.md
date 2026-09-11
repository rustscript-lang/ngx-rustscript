# pd-vm standard async hosts

[Documentation](../index.md) · [Directives](../directives.md) · [API](index.md) · [Execution model](../execution-model.md) · [Security](../security.md) · [Examples](../examples/index.md) · [Compatibility](../ngx-api-compatibility.md)

The module enables the pinned pd-vm `async`, `http-client`, and `sqlite`
features. Access and content scripts may use:

- async file hosts including `io::open`, `io::read_all`, and `io::close`;
- `http::client::request`;
- SQLite hosts including `sqlite::open`, `sqlite::execute`, `sqlite::query`,
  transactions, and `sqlite::close`.

I/O and SQLite remain pd-vm compiler builtins and are dispatched through its
public standard composition. HTTP is merged into the ngx compiler catalog from
pd-vm's public `http_host_catalog()` and exact-bound through the same
composition. All three domains use the existing request-owned VM and eventfd
resume lifecycle. They are rejected in `log_by_rss_file` because that phase
cannot wait.

External access is denied by default. Location grants are:

- `rss_io_root <path>` and `rss_io_write on|off`;
- `rss_http_scheme http|https`, one `rss_http_allow <host> <port>` pair per
  location, and `rss_http_allow_private on|off`;
- `rss_sqlite_root <path>`.

Paths are resolved with `ngx_conf_full_name`. Any SQLite use, including an
in-memory database, requires `rss_sqlite_root`; file-backed scripts then pass a
relative database path under that root. A second `rss_http_allow` in one
location is rejected during configuration, preventing pd-vm's separate host
and port lists from admitting unintended combinations. Process I/O and unsafe
SQL remain disabled. pd-vm's default byte, timeout, connection, row, parameter,
and result limits remain active.

The executable acceptance is
`nginx_async_pipeline_runs_io_http_sqlite_shared_dict_and_json` in
`tests/access_nginx.rs`; the deployable files are
`examples/async-pipeline.rss` and `examples/async-pipeline.nginx.conf`.
Walkthrough: [Async pipeline example](../examples/async-pipeline.md).


---

See also: [API index](index.md) · [Phase sets](index.md#phase-sets) · [Cross-cutting adaptations](index.md#cross-cutting-adaptations) · [Hard caps](index.md#hard-caps)
