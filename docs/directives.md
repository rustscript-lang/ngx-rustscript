# Directives

[Documentation](index.md) · [Directives](directives.md) · [API](api/index.md) · [Execution model](execution-model.md) · [Security](security.md) · [Examples](examples/index.md) · [Compatibility](ngx-api-compatibility.md)

NGINX configuration directives registered by `ngx-rustscript`. Each
entry uses the lua-nginx-module field layout (syntax, context, phase)
plus inheritance, default, and errors for this module.

Relative paths resolve with NGINX `ngx_conf_full_name` against the conf
prefix (the directory of the config file). Absolute paths are unchanged.
Script files are read once at `nginx -t` / reload, must be regular UTF-8
files, and compile with `compile_source` against the ngx host catalog.

## Index

- [`rss_shared_dict`](#rss_shared_dict)
- [`access_by_rss_file`](#access_by_rss_file)
- [`content_by_rss_file`](#content_by_rss_file)
- [`log_by_rss_file`](#log_by_rss_file)
- [`rss_io_root`](#rss_io_root)
- [`rss_io_write`](#rss_io_write)
- [`rss_http_scheme`](#rss_http_scheme)
- [`rss_http_allow`](#rss_http_allow)
- [`rss_http_allow_private`](#rss_http_allow_private)
- [`rss_sqlite_root`](#rss_sqlite_root)

```nginx
load_module /absolute/path/to/libngx_rustscript.so;

http {
    rss_shared_dict dogs 1m;
    server {
        listen 127.0.0.1:8080;
        location /t {
            access_by_rss_file /absolute/path/to/script.rss;
        }
        location /logged/ {
            log_by_rss_file /absolute/path/to/log.rss;
        }
        location /plain/ {
        }
    }
}
```

---

## rss_shared_dict

**syntax:** *rss_shared_dict &lt;name&gt; &lt;size&gt;*

**context:** *http*

**phase:** *loading-config*

**default:** *none*

**inheritance:** *http-main only; not a location field*

Declares an NGINX shared-memory zone with a slab-backed rbtree/LRU store,
the `init(shm_zone, old_data)` reload contract, and a cross-worker mutex.
Scripts look the zone up with `ngx::shared::dict(name)`. See
[Shared dictionary](api/shared-dict.md).

`TAKE2`. Minimum size is 8k. Names are unique, non-empty, UTF-8, at most
256 bytes.

**errors:**

- `invalid number of arguments in "rss_shared_dict"`
- `rss_shared_dict: zone name is not valid UTF-8`
- `rss_shared_dict: invalid size (minimum 8k)`
- `rss_shared_dict "<name>" is already defined` (duplicate name, same or
  different size)
- `rss_shared_dict: no memory`
- zone-name validation failures from `validate_zone_name`

---

## access_by_rss_file

**syntax:** *access_by_rss_file &lt;path&gt;*

**context:** *location*

**phase:** *access*

**default:** *none* (the location declines the access phase)

**inheritance:** *unset nested locations inherit the parent access
program; an explicit child `access_by_rss_file` overrides this slot
only. Content and log slots are independent.*

Location-only, exactly one argument. Duplicate directives in the same
location are rejected. The file compiles against the access-phase
allowlist: every production host except the five content-only output
hosts (`ngx::send_headers`, `ngx::print`, `ngx::say`, `ngx::flush`,
and `ngx::eof`). That includes timers, codec, time, metadata, constants,
logging, request read/write/body, `ngx.ctx`, response metadata, control,
shared dict, and UDP/TCP cosocket hosts. Bare `print` is not a host
(use `ngx::log`); `runtime::sleep`, unknown imports, and phase-forbidden
imports fail at `nginx -t`. Unknown modules fail compile or import
check. Unconfigured locations keep the original access-phase decline.

Import-free programs run on the worker-local VM pool to `Halted`.
Programs that import pd-vm I/O, HTTP, SQLite, sleep, cosocket, ctx, or
mutation/control hosts bind a request-local VM. See
[Execution model](execution-model.md).

**errors:**

- `duplicate access_by_rss_file directive in this location`
- `invalid number of arguments in "access_by_rss_file"`
- `access_by_rss_file: failed to resolve script path`
- `access_by_rss_file: empty script path`
- `access_by_rss_file: script path is not valid UTF-8`
- `access_by_rss_file: script file not found: …`
- `access_by_rss_file: script path is not a regular file: …`
- `access_by_rss_file: cannot read script …`
- `access_by_rss_file: script is not valid UTF-8: …`
- `access_by_rss_file: failed to compile script: …`
- `access_by_rss_file: script imports are not supported: …`
- `access_by_rss_file: script import <name> is not allowed in the access phase`
- `access_by_rss_file: cannot construct VM for script: …`

Runtime (not `nginx -t`): SQLite without `rss_sqlite_root` logs
`access_by_rss_file: SQLite is disabled until rss_sqlite_root is configured`.

---

## content_by_rss_file

**syntax:** *content_by_rss_file &lt;path&gt;*

**context:** *location*

**phase:** *content*

**default:** *none*

**inheritance:** *unset nested locations inherit the parent content
program; an explicit child overrides this slot only.*

Location-only, exactly one argument. Same one-argument shape as
`access_by_rss_file`. Compiles against the content-phase allowlist,
which includes the five output hosts. Duplicate directives in the same
location are rejected.

**errors:** same shape as `access_by_rss_file`, with
`content_by_rss_file` in the message (`duplicate content_by_rss_file in
this location` / `duplicate content_by_rss_file directive in this
location`).

---

## log_by_rss_file

**syntax:** *log_by_rss_file &lt;path&gt;*

**context:** *location*

**phase:** *log*

**default:** *none*

**inheritance:** *unset nested locations inherit the parent log
program; an explicit child overrides this slot only.*

Location-only, exactly one argument. Compiles under the Log phase
against a closed positive allowlist keyed by family, capability, and
import name.

Accepted hosts: codec (OpenResty documents `ngx.escape_uri` and the
other codec APIs for `log_by_lua*`), time, meta, log, constants,
request reads, `ngx.ctx`, shared-dict, and response getters
(`ngx::status::get`, `ngx::header::get`). OpenResty's `ngx.status` /
`ngx.header.HEADER` context tables include `log_by_lua*` for both read
and write; this module permits getters only.

Rejected at `nginx -t`: sleep, UDP/TCP cosocket, output, control
(`ngx::exit` / `ngx::redirect`), request writes, response setters,
pd-vm I/O / HTTP / SQLite. A future family or capability is rejected
until it is explicitly added.

The handler runs when NGINX invokes the log phase for that request
(current `r->loc_conf` after internal redirect / `error_page`).
Subrequests require core `log_subrequest on` on the subrequest location
(`ngx_http_finalize_request` in NGINX 1.30.4); otherwise NGINX skips
the subrequest log phase.

Synchronous and fail-closed: `Waiting` produces the exact
`log handler cannot wait` error, drops the VM, and never arms eventfd,
a timer, or `r->main->count`. Runtime errors and panics are logged and
never change the already-sent response.

**errors:** same file/compile shape as `access_by_rss_file`, with
`log_by_rss_file` in the message, plus phase-forbidden imports
(including `io::*` / `sqlite::*`).

---

## rss_io_root

**syntax:** *rss_io_root &lt;path&gt;*

**context:** *location*

**phase:** *loading-config*

**default:** *none* (file I/O denied)

**inheritance:** *inherits independently. A child that sets this field
replaces the inherited root.*

Grants pd-vm file access below one canonical root. Process spawning
stays disabled. One `rss_io_root` per location; a second is rejected.
See [Security](security.md) and
[pd-vm standard async hosts](api/standard-async.md).

**errors:**

- `duplicate rss_io_root directive in this location`
- `rss_io_root: failed to resolve path`
- `rss_io_root: invalid path`

---

## rss_io_write

**syntax:** *rss_io_write on | off*

**context:** *location*

**phase:** *loading-config*

**default:** *off*

**inheritance:** *inherits independently.*

Separately controls writes under `rss_io_root`. One flag per location.

**errors:**

- `duplicate rss_io_write directive in this location`
- `rss_io_write: expected on or off`

---

## rss_http_scheme

**syntax:** *rss_http_scheme http | https*

**context:** *location*

**phase:** *loading-config*

**default:** *none* (HTTP client denied)

**inheritance:** *inherits the scheme list independently. A child that
sets any `rss_http_scheme` replaces the inherited list.*

Grants a URL scheme for `http::client::request`. Repeat the directive
to allow both `http` and `https`. Values other than `http` or `https`
are rejected.

**errors:**

- `rss_http_scheme: expected http or https`

---

## rss_http_allow

**syntax:** *rss_http_allow &lt;host&gt; &lt;port&gt;*

**context:** *location*

**phase:** *loading-config*

**default:** *none*

**inheritance:** *inherits independently. A child that sets this field
replaces the inherited pair.*

One `rss_http_allow <host> <port>` pair per location. A second pair is
rejected during configuration (`nginx -t`). That prevents pd-vm's
separate host and port lists from admitting unintended combinations.
Host must be non-empty; port must be a non-zero `u16`.

**errors:**

- `rss_http_allow: invalid host`
- `rss_http_allow: invalid port`
- `duplicate rss_http_allow directive in this location`

---

## rss_http_allow_private

**syntax:** *rss_http_allow_private on | off*

**context:** *location*

**phase:** *loading-config*

**default:** *off*

**inheritance:** *inherits independently.*

Required `on` for loopback/private addresses. Keep `off` for ordinary
public upstreams. One flag per location.

**errors:**

- `duplicate rss_http_allow_private directive in this location`
- `rss_http_allow_private: expected on or off`

---

## rss_sqlite_root

**syntax:** *rss_sqlite_root &lt;path&gt;*

**context:** *location*

**phase:** *loading-config*

**default:** *none* (SQLite disabled)

**inheritance:** *inherits independently.*

Required for any SQLite use, including an in-memory database.
File-backed scripts then pass a non-empty relative database path under
that root. Unsafe SQL remains disabled. pd-vm default
connection/query/result limits remain active. One root per location.

**errors:**

- `duplicate rss_sqlite_root directive in this location`
- `rss_sqlite_root: failed to resolve path`
- `rss_sqlite_root: invalid path`

Runtime without this directive: the phase handler reports that SQLite
is disabled until `rss_sqlite_root` is configured.
