# pd-vm standard async hosts

[Documentation](../index.md) · [Directives](../directives.md) · [API](index.md) · [Execution model](../execution-model.md) · [Security](../security.md) · [Examples](../examples/index.md) · [Compatibility](../ngx-api-compatibility.md)

The module enables the pinned pd-vm `async`, `http-client`, and `sqlite`
features. This page defines the script-facing interface that ngx-rustscript
supports in `access_by_rss_file` and `content_by_rss_file`.

Calls that perform asynchronous work yield and resume through the module's
eventfd scheduler. All standard host domains are rejected in
`log_by_rss_file`; async calls cannot wait there, and the module keeps the
whole I/O/HTTP/SQLite surface fail-closed in that phase.

Handles are pd-vm resources, not raw integers. A resource is passed by borrow
(`io::read_all(file)`, `http::response::status(response)`) or taken by value
when the call consumes it (`io::close(file)`, `http::client::request(request)`).
A builder that is mutated must be declared `let mut` and passed as
`&mut request`.

## Option and Result

The pinned contract uses the guest types, not the VM's internal null value:

- `Option<T>` is a tagged value: `Some(payload)` or `None`.
  Absence is never `null`.
- `Result<T, E>` is a tagged value: `Result::Ok(payload)` or
  `Result::Err(error)`. `?` propagates inside a `Result`-returning function.
- `()` is the logical Unit: a call with no result, and the value of an empty
  block.

Explicit `null` literals and legacy `T?` annotations are rejected by the
RustScript compiler.

## I/O

Enable file access with `rss_io_root <path>`. Writes additionally require
`rss_io_write on`. Paths must resolve below the configured root. Reads and
writes are each capped at 1 MiB by ngx-rustscript's current policy.

| RustScript callable | Return | Yield | Interface and behavior |
| --- | --- | --- | --- |
| `io::open(path: string, mode: string)` | `Result<io.file, string>` | yes | Modes: `r`, `w`, `a`, `r+`, `w+`, `a+`. Any write-capable mode requires `rss_io_write on`. A policy denial is a guest `Result::Err`. |
| `io::read_all(handle: io.file)` | `Result<string, string>` | yes | Reads all remaining UTF-8 text, then rejects the call if the result exceeds the read limit. |
| `io::read_line(handle: io.file)` | `Result<Option<string>, string>` | yes | End of file is `Ok(None)`; a present but empty line is `Ok(Some(""))`. The single trailing newline is removed. |
| `io::write(handle: io.file, text: string)` | `Result<int, string>` | yes | Writes text and returns the number of bytes written. A partial write is possible. |
| `io::flush(handle: io.file)` | `Result<(), string>` | yes | Flushes writable handles; a readable handle is a successful no-op. |
| `io::close(handle: io.file)` | `Result<(), string>` | yes | Closes the handle and releases it from the request scope. Closed, foreign, or stale handles are rejected. |
| `io::exists(path: string)` | `bool` | yes | Tests a path below `rss_io_root`. |
| `io::popen(command: string, mode: string)` | `Result<io.file, string>` | yes | Exists in the pinned pd-vm surface, but ngx-rustscript always installs `allow_process: false`; scripts cannot grant process execution through an NGINX directive. |

Example:

```rss
use io;

fn read_text(path: string) -> Result<string, string> {
    let file = io::open(path, "r")?;
    let text = io::read_all(file)?;
    io::close(file)?;
    Result::Ok(text)
}

let text = match read_text("input.txt") {
    Result::Ok(text) => text,
    Result::Err(message) => message
};
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

### Request builder and buffered call

| RustScript callable | Return | Yield | Interface |
| --- | --- | --- | --- |
| `http::request::new(method: string, url: string)` | `http.request` | no | Creates an owned request builder. Methods: `GET`, `POST`, `PUT`, `PATCH`, `DELETE`, `HEAD`, `OPTIONS` (case-insensitive). |
| `http::request::set_header(&mut request, name: string, value: string)` | `()` | no | Appends one header. Repeating a name sends duplicates. `Host`, `Content-Length`, `Transfer-Encoding`, and `Connection` are client-managed and rejected. |
| `http::request::set_body_text(&mut request, body: string)` | `()` | no | Sets a UTF-8 request body. |
| `http::request::set_body_bytes(&mut request, body: bytes)` | `()` | no | Sets a binary request body. |
| `http::client::request(request: http.request)` | `http.response` | yes | Sends the owned request and returns the buffered response resource. |

The URL must be absolute and allowed by the location scheme, host, port, and
private-address policy.

### Buffered response

| RustScript callable | Return | Meaning |
| --- | --- | --- |
| `http::response::status(response)` | `int` | HTTP status code. |
| `http::response::url(response)` | `string` | Final URL after redirects. |
| `http::response::body(response)` | `bytes` | Buffered response body. Convert with `bytes::to_utf8` when text is required. |
| `http::response::header_names(response)` | `array<string>` | Header names in normalized order. |
| `http::response::header_values(response, name: string)` | `array<string>` | All values for one header; duplicates are retained. |

Current pd-vm defaults retained by ngx-rustscript are five redirects, a 1 MiB
request body, 100 request headers, a 64 KiB serialized request-header block,
an 8 MiB response body, a 10 second connect timeout, a 30 second request
timeout, and 64 in-flight HTTP operations per VM.

Example:

```rss
use http;
use bytes as builtin_bytes;

let mut request = http::request::new("GET", "http://127.0.0.1:8081/data");
http::request::set_header(&mut request, "accept", "application/json");
let response = http::client::request(request);
let status: int = http::response::status(response);
let body: string = builtin_bytes::to_utf8(http::response::body(response));
let content_types = http::response::header_values(response, "content-type");
```

### Server-sent events

`http::client::sse(request: http.request, on_event: fn(string, string, string, string) -> bool)`
consumes a request builder and returns an `http.sse_summary` resource. The
callback receives the SSE `event`, `data`, `id`, and `retry` fields and returns
`true` to continue or `false` to stop.

| RustScript callable | Return |
| --- | --- |
| `http::sse_summary::outcome(summary)` | `string` |
| `http::sse_summary::status(summary)` | `int` |
| `http::sse_summary::url(summary)` | `string` |
| `http::sse_summary::items(summary)` | `int` |
| `http::sse_summary::bytes_received(summary)` | `int` |
| `http::sse_summary::bytes_sent(summary)` | `int` |
| `http::sse_summary::headers(summary)` | `http.headers` |
| `http::headers::names(headers)` | `array<string>` |
| `http::headers::values(headers, name: string)` | `array<string>` |

This surface is provisional in ngx-rustscript: its NGINX request lifecycle has
not been covered by the module's executable acceptance suite, so the module
does not yet make a lifecycle-compatibility claim for it.

## SQLite

Any SQLite use, including `:memory:`, requires `rss_sqlite_root <path>`. For a
file database, `path` is a non-empty relative path below that root. Unsafe SQL
remains disabled.

| RustScript callable | Return | Yield | Interface |
| --- | --- | --- | --- |
| `sqlite::open(options: SqliteOpenOptions)` | `sqlite.connection` | no | Opens a connection using the options below. |
| `sqlite::execute(connection, sql: string, params: array<SqliteValue>)` | `SqliteExecuteResult` | yes | Executes one parameterized statement. |
| `sqlite::query(connection, sql: string, params: array<SqliteValue>, limits: SqliteLimits)` | `SqliteQueryResult` | yes | Runs a bounded query. Pass `{}` for default limits. |
| `sqlite::transaction(connection, statements: array<SqliteStatement>)` | `array<SqliteTransactionResult>` | yes | Runs ordered statements atomically. |
| `sqlite::close(connection)` | `()` | no | Closes the connection and releases it from the request scope. |

### sqlite::open options

```text
SqliteOpenOptions {
    path: Option<string>,
    mode: Option<string>,
    root: Option<string>,
    limits: Option<SqliteLimits>
}
```

Omitted `Option` fields normalize to `None`. A present value is
`Some(...)`.

| Field | Values | Default |
| --- | --- | --- |
| `path` | Relative file path, or exactly `:memory:` with `mode: "memory"` | required by the runtime |
| `mode` | `memory`, `read_only`, `read_write`, `read_write_create` | `read_write_create` |
| `limits` | Positive integer overrides bounded by the embedding ceilings below | current ceilings |

Supported `limits` keys and current ceilings (each an `Option<int>`):

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

Per-call limits may reduce these ceilings and cannot raise them. Non-positive
values are rejected.

### Parameters

`params` is an array of tagged `SqliteValue` records. Exactly one payload
matches `kind`, except when `kind` is the string `"null"`:

```text
SqliteValue {
    kind: string,             // "null" | "int" | "float" | "text" | "blob"
    int_value: Option<int>,
    float_value: Option<float>,
    text_value: Option<string>,
    blob_value: Option<bytes>
}
```

SQL should use SQLite placeholders such as `?1`.

### Execute result

```text
SqliteExecuteResult {
    rows_affected: int,
    last_insert_rowid: int
}
```

### Query limits and result

The `limits` argument accepted by `sqlite::query` is the same bounded
`SqliteLimits` schema as `sqlite::open`. The query path applies `max_rows`,
`max_columns`, `max_result_bytes`, and `max_statement_bytes`. Other accepted
keys do not change an already-open connection or the connection-owned parameter
and operation ceilings. Values cannot exceed the connection's limits. The result
is:

```text
SqliteQueryResult {
    columns: array<string>,
    rows: array<SqliteRow>,   // SqliteRow { cells: array<SqliteValue> }
    truncated: bool,
    next_cursor: Option<int>  // most recent returned first-column integer, when any
}
```

`next_cursor` retains the first-column integer from the most recent returned
row whose first column is an `int`; later rows with another first-column type
do not clear it. It is not the next unread row, and its presence is independent
of `truncated`.

### Transaction statements and result

Each entry in `statements` is a `SqliteStatement`:

```text
SqliteStatement {
    sql: string,
    params: Option<array<SqliteValue>>,
    query: Option<bool>,
    limits: Option<SqliteLimits>
}
```

`params` defaults to an empty array; `query` defaults to `false`. When `query`
is false, the corresponding result carries an execute result. When true, it
carries a query result:

```text
SqliteTransactionResult {
    kind: string,
    execute: Option<SqliteExecuteResult>,
    query: Option<SqliteQueryResult>
}
```

The call returns those records in statement order and commits only if all
statements succeed.

Example:

```rss
use sqlite;

let db = sqlite::open({
    path: Some("pipeline.db"),
    mode: Some("read_write_create")
});
let created: SqliteExecuteResult = sqlite::execute(
    db,
    "CREATE TABLE IF NOT EXISTS events (body TEXT NOT NULL)",
    []
);
let inserted: SqliteExecuteResult = sqlite::execute(
    db,
    "INSERT INTO events(body) VALUES (?1)",
    [{ kind: "text", text_value: Some("payload") }]
);
let rows: SqliteQueryResult = sqlite::query(
    db,
    "SELECT body FROM events ORDER BY rowid DESC LIMIT 10",
    [],
    { max_rows: Some(10) }
);
sqlite::close(db);
```

## Runtime and policy summary

I/O, HTTP, and SQLite are pd-vm standard host modules with typed resource
handles. Their schemas are part of the module's compile catalog as well as the
runtime composition, so a script is type-checked against the exact contract the
registry installs: no handle position is dynamic, and no result is a
`map`/`unknown`/`null` carrier. ngx-rustscript does not modify RustScript or
pd-vm core types.

The executable end-to-end acceptance is
`nginx_async_pipeline_runs_io_http_sqlite_shared_dict_and_json` in
`tests/access_nginx.rs`, and the timer + HTTP combination is covered by
`http_standard_import_and_ngx_timer_run_together_through_the_registry` in
`tests/timer_nginx.rs`. Deployable files are
`examples/async-pipeline.rss` and `examples/async-pipeline.nginx.conf`.
Walkthrough: [Async pipeline example](../examples/async-pipeline.md).

---

See also: [API index](index.md) · [Directives](../directives.md) · [Execution model](../execution-model.md) · [Security](../security.md)
