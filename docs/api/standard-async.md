# pd-vm standard async hosts

[Documentation](../index.md) · [Directives](../directives.md) · [API](index.md) · [Execution model](../execution-model.md) · [Security](../security.md) · [Examples](../examples/index.md) · [Compatibility](../ngx-api-compatibility.md)

The module enables the pinned pd-vm `async`, `http-client`, and `sqlite`
features. This page defines the script-facing interface that ngx-rustscript
supports in `access_by_rss_file` and `content_by_rss_file`.

Calls that perform asynchronous work yield and resume through the module's
eventfd scheduler. The tables identify synchronous exceptions. All standard
host domains are rejected in `log_by_rss_file`; async calls cannot wait there,
and the module keeps the whole I/O/HTTP/SQLite surface fail-closed in that
phase.

## I/O

Enable file access with `rss_io_root <path>`. Writes additionally require
`rss_io_write on`. Paths must resolve below the configured root. Reads and
writes are each capped at 1 MiB by ngx-rustscript's current policy.

| RustScript callable | Return | Yield | Interface and behavior |
| --- | --- | --- | --- |
| `io::open(path: string, mode: string)` | `int` handle | yes | Modes: `r`, `w`, `a`, `r+`, `w+`, `a+`. Any write-capable mode requires `rss_io_write on`. |
| `io::read_all(handle: int)` | `string` | yes | Reads all remaining UTF-8 text, then rejects the call if the result exceeds the read limit. |
| `io::read_line(handle: int)` | `string` | yes | Reads one line, including its line ending when present, up to the read limit. |
| `io::write(handle: int, text: string)` | `int` | yes | Writes text and returns the number of bytes written. A partial write is possible. |
| `io::flush(handle: int)` | `bool` | yes | Flushes writable handles and returns `true`; a readable handle is a successful no-op. |
| `io::close(handle: int)` | `bool` | yes | Closes the handle and returns `true`. Closed, foreign, or stale handles are rejected. |
| `io::exists(path: string)` | `bool` | yes | Tests a path below `rss_io_root`. |

`io::popen(command: string, mode: string) -> int` exists in the pinned pd-vm
builtin surface, but ngx-rustscript always installs `allow_process: false`.
Scripts cannot grant process execution through an NGINX directive, so this
call is outside the supported ngx-rustscript interface.

Example:

```rss
use io;

let file: int = io::open("input.txt", "r");
let text: string = io::read_all(file);
io::close(file);
```

## HTTP client

Enable HTTP with all of the following location grants:

```nginx
rss_http_scheme http;
rss_http_allow 127.0.0.1 8081;
rss_http_allow_private on;
```

A location may contain one `rss_http_allow <host> <port>` pair. Repeat
`rss_http_scheme` to grant both `http` and `https`. Loopback and private
addresses require `rss_http_allow_private on`.

### http::client::request

```text
http::client::request(request: map) -> map
```

The request object has this interface. The pinned pd-vm adapter validates the
concrete contents at runtime, so the accepted payload shapes are the pinned
contract's: a header list (duplicates are preserved in order) and a discriminated
body.

| Field | Type | Required | Meaning |
| --- | --- | --- | --- |
| `method` | `string` | yes | `GET`, `POST`, `PUT`, `PATCH`, `DELETE`, `HEAD`, or `OPTIONS`; matching is case-insensitive. |
| `url` | `string` | yes | Absolute URL allowed by the location scheme, host, port, and private-address policy. |
| `headers` | `array` of `{ name: string, value: string }` | no | Header entries in order. Repeating one name sends duplicate headers; `Host`, `Content-Length`, `Transfer-Encoding`, and `Connection` are client-managed and rejected here. |
| `body` | `{ kind: "text", text: string }` or `{ kind: "bytes", bytes: bytes }` or `null` | no | Request payload variant. Omitted and `null` mean no body. |

The response is a dynamic pd-vm map with these fixed top-level keys; response
header entries keep the pd-vm `HttpHeaderValue` text/bytes payload variant so
duplicate headers survive.

| Field | Type | Meaning |
| --- | --- | --- |
| `status` | `int` | HTTP status code. |
| `headers` | `array` of `{ name: string, value: { kind: "text", text: string } \| { kind: "bytes", bytes: bytes } }` | Response headers in order, duplicates preserved. |
| `body` | `bytes` | Buffered response body. Convert with `bytes::to_utf8` when text is required. |
| `url` | `string` | Final URL after redirects. |

Current pd-vm defaults retained by ngx-rustscript are five redirects, a 1 MiB
request body, 100 request headers, a 64 KiB serialized request-header block,
an 8 MiB response body, a 10 second connect timeout, a 30 second request
timeout, and 64 in-flight HTTP operations per VM.

Example:

```rss
use http;
use bytes as builtin_bytes;

let response: map = http::client::request({
    method: "GET",
    url: "http://127.0.0.1:8081/data",
    headers: { "accept": "application/json" },
    body: null
});
let status: int = response["status"];
let body_bytes: bytes = response["body"];
let body: string = builtin_bytes::to_utf8(body_bytes);
```

The pinned catalog also exposes and binds
`http::client::sse(request: map, on_event: callable(map) -> map) -> map` in
access and content scripts. This surface is provisional in ngx-rustscript:
its NGINX request lifecycle has not been covered by the module's executable
acceptance suite, so the module does not yet make a lifecycle-compatibility
claim for it.

## SQLite

Any SQLite use, including `:memory:`, requires `rss_sqlite_root <path>`. For a
file database, `path` is a non-empty relative path below that root. Unsafe SQL
remains disabled.

| RustScript callable | Return | Yield | Interface |
| --- | --- | --- | --- |
| `sqlite::open(options: map)` | `int` handle | no | Opens a connection using the options below. |
| `sqlite::execute(db: int, sql: string, params: array)` | `map` | yes | Executes one parameterized statement. |
| `sqlite::query(db: int, sql: string, params: array, limits: map)` | `map` | yes | Runs a bounded query. Pass `{}` for default limits. |
| `sqlite::transaction(db: int, statements: array<map>)` | `array<map>` | yes | Runs ordered statements atomically. |
| `sqlite::close(db: int)` | `null` | no | Closes the connection. Closed, foreign, or stale handles are rejected. |

### sqlite::open options

```text
{
    path: string,
    mode: string?,
    limits: map?
}
```

| Field | Values | Default |
| --- | --- | --- |
| `path` | Relative file path, or exactly `:memory:` with `mode: "memory"` | required |
| `mode` | `memory`, `read_only`, `read_write`, `read_write_create` | `read_write_create` |
| `limits` | Positive integer overrides bounded by the embedding ceilings below | current ceilings |

Supported `limits` keys and current ceilings:

- `max_connections`: 16
- `max_statements`: 128
- `max_rows`: 1000
- `max_columns`: 128
- `max_result_bytes`: 4 MiB
- `max_statement_bytes`: 1 MiB
- `max_parameters`: 128
- `max_parameter_bytes`: 1 MiB
- `max_pending_operations`: 32
- `max_transaction_ms`: 5000
- `busy_timeout_ms`: 5000

Per-call limits may reduce these ceilings and cannot raise them. Unknown limit
keys and zero or negative values are rejected.

### Parameters

`params` is an array whose entries may be `null`, `int`, `float`, `string`,
or `bytes`. SQL should use SQLite placeholders such as `?1`.

### Execute result

```text
{
    rows_affected: int,
    last_insert_rowid: int
}
```

### Query limits and result

The `limits` map accepted by `sqlite::query` is parsed by the same bounded
schema as `sqlite::open`. The query path applies `max_rows`, `max_columns`,
`max_result_bytes`, and `max_statement_bytes`. Other accepted keys do not
change an already-open connection or the connection-owned parameter and
operation ceilings. Values cannot exceed the connection's limits. The result
map is:

```text
{
    columns: array<string>,
    rows: array<array<unknown>>,
    truncated: bool,
    next_cursor: int? // most recent returned first-column integer, when any
}
```

`next_cursor` retains the first-column integer from the most recent returned
row whose first column is an `int`; later rows with another first-column type
do not clear it. It is not the next unread row, and its presence is independent
of `truncated`. A row cell may be `null`, `int`, `float`, `string`, or `bytes`.

### Transaction statements and result

Each entry in `statements` has this interface:

```text
{
    sql: string,
    params: array?,
    query: bool?,
    limits: map?
}
```

`params` defaults to `[]`; `query` defaults to `false`. When `query` is false,
the corresponding result is an execute result map. When true, it is a query
result map. The call returns those maps in statement order and commits only if
all statements succeed.

Example:

```rss
use sqlite;

let db: int = sqlite::open({
    path: "pipeline.db",
    mode: "read_write_create",
    limits: {}
});
let created: map = sqlite::execute(
    db,
    "CREATE TABLE IF NOT EXISTS events (body TEXT NOT NULL)",
    []
);
let inserted: map = sqlite::execute(
    db,
    "INSERT INTO events(body) VALUES (?1)",
    { "payload" }
);
let rows: map = sqlite::query(
    db,
    "SELECT body FROM events ORDER BY rowid DESC LIMIT 10",
    [],
    { max_rows: 10 }
);
sqlite::close(db);
```

## Runtime and policy summary

I/O and SQLite are pd-vm compiler builtins dispatched through its public
standard composition. HTTP is a pd-vm standard host whose dynamic `map`
interface is what ngx scripts are written against: the compile catalog leaves
it dynamic, configuration validates the name and phase, and the pd-vm adapter
is bound through the standard composition (the per-name path for request VMs,
and the same composition staged by the timer-host registry for owned-dispatch
programs). These maps and handles follow the pinned pd-vm public contract;
ngx-rustscript does not modify RustScript or pd-vm core types.

The executable end-to-end acceptance is
`nginx_async_pipeline_runs_io_http_sqlite_shared_dict_and_json` in
`tests/access_nginx.rs`, and the timer + HTTP combination is covered by
`http_standard_import_and_ngx_timer_run_together_through_the_registry` in
`tests/timer_nginx.rs`. Deployable files are
`examples/async-pipeline.rss` and `examples/async-pipeline.nginx.conf`.
Walkthrough: [Async pipeline example](../examples/async-pipeline.md).

---

See also: [API index](index.md) · [Directives](../directives.md) · [Execution model](../execution-model.md) · [Security](../security.md)
