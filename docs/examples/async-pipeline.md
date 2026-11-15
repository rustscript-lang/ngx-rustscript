# Async I/O, HTTP, SQLite, and shared-dict pipeline

[Documentation](../index.md) · [Directives](../directives.md) · [Security](../security.md) · [API](../api/standard-async.md) · [Examples](index.md)

Files:

- `examples/async-pipeline.rss`
- `examples/async-pipeline.nginx.conf`

The content handler runs one request-owned RustScript VM through this sequence:

1. `io::open` and `io::read_all` load an upstream URL from an allowed file.
2. `http::client::request` fetches that explicitly permitted URL.
3. `sqlite::open` and `sqlite::query` run a bounded query in the configured database root.
4. `ngx::shared::set` records a JSON snapshot in the `pipeline` shared dictionary.
5. `ngx::say` sends a JSON response.

I/O, HTTP, and SQLite reuse pd-vm's public async runtime composition, host extensions, and policy types. The module does not carry a fork of those implementations.

## Prepare the demo

Replace all `/absolute/path/to/...` placeholders in the NGINX config. Create the data directory and URL file:

```sh
mkdir -p /absolute/path/to/pipeline-data
printf '%s' 'http://127.0.0.1:18080/source' \
  > /absolute/path/to/pipeline-data/upstream-url.txt
```

Run a local HTTP upstream on `127.0.0.1:18080`, then validate and start the pinned NGINX 1.31.4 build:

```sh
$NGX_RUSTSCRIPT_NGINX -t -p /absolute/path/to/prefix \
  -c /absolute/path/to/async-pipeline.nginx.conf
$NGX_RUSTSCRIPT_NGINX -p /absolute/path/to/prefix \
  -c /absolute/path/to/async-pipeline.nginx.conf
curl --fail http://127.0.0.1:8080/pipeline
```

The response contains the URL read by I/O, the HTTP status/final URL/UTF-8 body, the typed SQLite query envelope, and the typed shared-dict store result. The SQLite database is created as `pipeline.db` under `rss_sqlite_root`.

## Capability policy

All three external domains start denied per location:

- `rss_io_root <path>` grants file access below one canonical root. `rss_io_write on|off` separately controls writes. Process spawning stays disabled.
- `rss_http_scheme http|https` grants a URL scheme. A location accepts one `rss_http_allow <host> <port>` pair; a second pair is rejected during `nginx -t`. `rss_http_allow_private on` is required for loopback/private addresses and should remain off for ordinary public upstreams.
- `rss_sqlite_root <path>` grants SQLite use, including in-memory databases. File-backed script paths must be non-empty relative paths under that root. Unsafe SQL remains disabled and pd-vm's default connection/query/result limits remain active.

These directives inherit by field into nested locations. A child location that sets a field replaces the inherited value for that field.

`log_by_rss_file` rejects I/O, HTTP, and SQLite at configuration time because the log handler cannot suspend. Access and content scripts use the existing eventfd resume path.

## Value shapes

The example uses explicit host named types throughout. `io::open` and
`io::read_all` return `Result`, so the read is a small `Result`-returning
helper; `http::request::new` builds an owned request resource and
`http::client::request` consumes it into a response resource whose typed
accessors are `http::response::{status,url,body}`. `sqlite::open` takes
`SqliteOpenOptions` with `Option` fields, and `sqlite::query` returns a
`SqliteQueryResult` whose cells are tagged `SqliteValue` records. The shared
store result is `SharedStoreResult` with `Option<string>` error.

Before JSON encoding, the example narrows each value to `int`, `string`, or
`bool`, and converts the HTTP body with `bytes::to_utf8`; `json::encode`
cannot encode `bytes`, so `SqliteValue.blob_value` is projected away.
