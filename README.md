# ngx-rustscript

NGINX dynamic HTTP module that hosts RustScript (`pd-vm`) at the access,
content, and log phases. Location directives `access_by_rss_file <path>`,
`content_by_rss_file <path>`, and `log_by_rss_file <path>` compile the
script once at configuration time.

Import-free scripts run synchronously to `Halted` on a worker-local VM
pool. Scripts that import `ngx::sleep`, `ngx::sleep_ms`,
`ngx::socket::udp*`, or `ngx::socket::tcp*` / `ngx::socket::connect`
bind a request-local VM, return `NGX_AGAIN` while a
generic host future is pending, and resume the same VM from the eventfd
callback. Client disconnect during a wait finalizes the request as HTTP
499. Graceful master shutdown cancels in-flight host futures instead of
waiting them out. `log_by_rss_file` is synchronous and fail-closed: it
runs after the response is complete, never waits, and logs errors
without mutating the already-sent response.

## Requirements

| Component | Pin |
| --- | --- |
| [ngx-rust](https://github.com/nginx/ngx-rust) | `cda9d8372ccd2e139ef2b03e5806e019e7796187` (`ngx` / `nginx-sys` 0.5.0, `nginx-src` 1.30.0+1.30.4) |
| NGINX | 1.30.4 (`nginx_version` 1030004). Tarball SHA-256 `4261dc90e9e47c1c4041276e9aaa3d48ebe2e664f728e14fa95ae6c67d57a08b` |
| RustScript / `pd-vm` | [`https://github.com/rustscript-lang/rustscript`](https://github.com/rustscript-lang/rustscript) at `4b749862ff1632ef1266d75709da3676de37356c` (Cargo git dependency `package = "pd-vm"` on that immutable rev; no sibling path) |
| Rust toolchain | 1.85.0 or later (ngx-rust MSRV). Verified with rustc 1.94.1 |

Required build tools on Linux:

- C compiler, make, OpenSSL, PCRE2, zlib
- clang and libclang (official `nginx-sys` bindgen)
- Rust 1.85+
- GNU `nm` on Linux for a minimal check that the production cdylib does not
  define test-only NGINX C stubs

## ABI matching

`nginx-sys` generates bindings from a **configured** NGINX source tree. The
module `.so` must be built against the same source and `./configure` flags as
the `nginx` binary that loads it. Distro packages often apply ABI-breaking
patches; do not mix a distro `nginx` with this module.

Official `nginx-sys` inputs (do not invent extra variables):

- `NGINX_SOURCE_DIR` — absolute path to the NGINX source directory after
  `./configure`
- `NGINX_BUILD_DIR` — absolute path to the build directory (`objs` by default,
  or an official `--builddir=` path)

Only `./configure` is required for bindgen. The fixture below also runs `make`
so `nginx -t` can load the module. An explicit `--builddir=` path is accepted
and validated as a path-shaped configure argument.

The selected required build path is official `nginx-sys` bindgen against this
pinned, configured NGINX 1.30.4 tree. Pre-generated or vendored bindings are
not used: they cannot match ngx-rust `cda9d8372ccd2e139ef2b03e5806e019e7796187`
and NGINX 1.30.4. Install clang and libclang so rust-bindgen can generate
bindings at `cargo build`.

Clear inherited `CFLAGS` / `BINDGEN_EXTRA_CLANG_ARGS` before `./configure` and
`cargo build`. A leaked include path from another NGINX tree will change
`ngx_auto_config.h` and break the link.

## Compilation / build / install

```sh
# Run from the crate root. Capture absolute paths before changing directories.
CRATE_ROOT="$PWD"
NGINX_SOURCE_DIR="$CRATE_ROOT/nginx-1.30.4"
NGINX_BUILD_DIR="$NGINX_SOURCE_DIR/objs"

# 1. NGINX 1.30.4 with dynamic-module compat
curl -fL -o nginx-1.30.4.tar.gz https://nginx.org/download/nginx-1.30.4.tar.gz
echo '4261dc90e9e47c1c4041276e9aaa3d48ebe2e664f728e14fa95ae6c67d57a08b  nginx-1.30.4.tar.gz' | sha256sum -c
tar -xf nginx-1.30.4.tar.gz
cd "$NGINX_SOURCE_DIR"
unset CFLAGS CPPFLAGS BINDGEN_EXTRA_CLANG_ARGS
./configure --prefix=/usr/local/nginx-1.30.4 --with-compat --with-http_ssl_module
make -j"$(nproc)"

# 2. return to the crate and build the cdylib against that exact tree
cd "$CRATE_ROOT"
export NGINX_SOURCE_DIR
export NGINX_BUILD_DIR
unset CFLAGS CPPFLAGS BINDGEN_EXTRA_CLANG_ARGS
# Use the git CLI when GitHub HTTPS is rewritten to SSH (libgit2 cannot auth).
export CARGO_NET_GIT_FETCH_WITH_CLI=true
# Honor a workspace CARGO_TARGET_DIR; otherwise use <crate>/target.
: "${CARGO_TARGET_DIR:=$CRATE_ROOT/target}"
export CARGO_TARGET_DIR
cargo build --lib

# Absolute module path derived from the same target dir used above.
export NGX_RUSTSCRIPT_MODULE="$(readlink -f "$CARGO_TARGET_DIR/debug/libngx_rustscript.so")"
```

`cargo build --lib` is the supported standalone path. It enables the
`export-modules` feature so the cdylib dynamically exports `ngx_modules`,
`ngx_module_names`, and `ngx_module_order` for `load_module`. The HTTP
module object is named `ngx_http_rustscript_module` inside that table; it
is not a dynamic symbol.

Standalone `cargo test` validates the canonical `$NGINX_BUILD_DIR/nginx`
binary, `nginx/1.30.4`, `--with-compat`, and `--with-http_ssl_module` in the
module-load harness before `nginx -t`. Quoted configure paths, `--prefix`,
`--builddir`, `--add-dynamic-module`, and other valid NGINX flags are
accepted.

Tests resolve `${CARGO_TARGET_DIR:-<crate>/target}/debug/libngx_rustscript.so`
(or `NGX_RUSTSCRIPT_MODULE` when set) and pass that absolute path to
`load_module`. A missing or non-regular shared library is rejected. `nm -D
--defined-only` rejects a production cdylib that defines test-only NGINX C
stubs. `nginx -t`, real module load, and HTTP are the acceptance evidence.

Tests also require the selected nginx binary to be canonical
`$NGINX_BUILD_DIR/nginx` reporting `nginx/1.30.4` with `--with-compat` and
`--with-http_ssl_module`. Fixture `nginx.conf.in` interpolates quoted paths;
unsafe bytes (NUL, newline, non-UTF-8) are rejected fail-closed, and
`nginx -t` receives `-p` / `-c` as `OsStr`.

### Optional: NGINX `--add-dynamic-module`

`config`, `config.make`, and `auto/rust` follow ngx-rust
`examples/{config,config.make,auto/rust}` at the same git revision. That path
builds a **staticlib** with `--no-default-features` (no `ngx_modules` table;
NGINX generates it). Use it only when configuring NGINX with
`--add-dynamic-module=/path/to/ngx-rustscript`.

Full configure + parallel make + `nginx -t` is `tests/verify_add_dynamic.sh`
(a standalone script so nested `cargo rustc` cannot deadlock a parent
`cargo test`). After `cargo build --lib`, run:

```sh
export CARGO_TARGET_DIR
export NGINX_SOURCE_DIR
bash tests/verify_add_dynamic.sh
```

## NGINX configuration and usage

Load the cdylib and attach a script to a location:

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

`access_by_rss_file` is location-only and takes exactly one argument.
`content_by_rss_file` and `log_by_rss_file` are independent location-only
slots with the same one-argument shape. Duplicate directives in the same
location are rejected per slot. Unset nested locations inherit each
parent slot independently; an explicit child directive overrides that
slot only.
`rss_shared_dict <name> <size>` is HTTP-main only and takes exactly two
arguments. Relative paths resolve with NGINX `ngx_conf_full_name` against the conf
prefix (the directory of the config file). Absolute paths are unchanged.
The file is read once at `nginx -t` / reload, must be a regular UTF-8 file,
and is compiled with `compile_source`.

Config accepts every production host in the access phase except the five
content-only output hosts (`ngx::send_headers`, `ngx::print`, `ngx::say`,
`ngx::flush`, and `ngx::eof`). This includes timers, codec, time, metadata,
constants, logging, request read/write/body, `ngx.ctx`, response metadata,
control, shared dict, and UDP/TCP cosocket hosts. Bare `print` is not a host
(use `ngx::log`); `runtime::sleep`, unknown imports, and phase-forbidden
imports are rejected at configuration time. Unknown modules fail compile or
import check. Duplicate directives in the same location are rejected. Unset
nested locations inherit the parent program; an explicit child directive
overrides. Unconfigured locations keep the original access-phase decline.

`log_by_rss_file` compiles under the Log phase against a closed
positive allowlist keyed by family, capability, and import name.
Accepted hosts are codec (OpenResty documents `ngx.escape_uri` and
the other codec APIs for `log_by_lua*`), time, meta, log, constants,
request reads, `ngx.ctx`, shared-dict, and response getters
(`ngx::status::get`, `ngx::header::get`). OpenResty's `ngx.status` /
`ngx.header.HEADER` context tables include `log_by_lua*` for both
read and write; this module permits getters only. Sleep, UDP/TCP
cosocket, output, control (`ngx::exit` / `ngx::redirect`), request
writes, and response setters are rejected at `nginx -t`. A future
family or capability is rejected until it is explicitly added.

The handler runs when NGINX invokes the log phase for that request
(current `r->loc_conf` after internal redirect / `error_page`).
Subrequests require core `log_subrequest on` on the subrequest
location (`ngx_http_finalize_request` in NGINX 1.30.4); otherwise
NGINX skips the subrequest log phase. `Waiting` produces the exact
`log handler cannot wait` error, drops the VM, and never arms
eventfd, a timer, or `r->main->count`. Runtime errors and panics are
logged and never change the already-sent response.

Prove load with an isolated prefix:

```sh
export NGX_RUSTSCRIPT_NGINX="$NGINX_BUILD_DIR/nginx"
: "${CARGO_TARGET_DIR:=$CRATE_ROOT/target}"
export CARGO_TARGET_DIR
cargo build --lib
export NGX_RUSTSCRIPT_MODULE="$(readlink -f "$CARGO_TARGET_DIR/debug/libngx_rustscript.so")"
cargo test --test module_load -- --nocapture
nginx -t -p /path/to/prefix -c conf/nginx.conf
```

## RustScript examples

Complete deployment examples:

- [Shared-dict fixed-window rate limiter](docs/examples/rate-limit.md) — [RSS](examples/rate-limit.rss) / [NGINX config](examples/rate-limit.nginx.conf)
- [HMAC API-key authentication](docs/examples/key-auth.md) — [RSS](examples/key-auth.rss) / [NGINX config](examples/key-auth.nginx.conf) / [secret include template](examples/key-auth-secrets.conf.example)

Import-free access script (runs to `Halted`, then the content phase):

```rss
let x = 1 + 2;
```

One sleep, then the content phase:

```rss
use ngx;
ngx::sleep(0.03);
```

Millisecond sleep for integer delays:

```rss
use ngx;
ngx::sleep_ms(30);
```

Read a request body in `content_by_rss_file` (nullary like OpenResty; `get_post_args` still takes explicit `null` for the max-args adaptation):

```rss
use ngx;
ngx::req::read_body();
let data = ngx::req::get_body_data();
ngx::say(data);
```

Replace the request body in `content_by_rss_file` (memory; `set_body_file` / builder APIs are in the compatibility doc):

```rss
use ngx;
ngx::req::read_body();
ngx::req::set_body_data("replacement");
ngx::say(ngx::req::get_body_data());
```

Two sequential sleeps:

```rss
use ngx;
ngx::sleep(0.01);
ngx::sleep_ms(10);
```

UDP echo to a connected peer:

```rss
use ngx;
let sock = ngx::socket::udp();
ngx::socket::udp::settimeout(sock, 1000);
ngx::socket::udp::setpeername(sock, "127.0.0.1", 53);
ngx::socket::udp::send(sock, "ping");
ngx::socket::udp::receive(sock, null);
ngx::socket::udp::close(sock);
```

TCP echo to a connected peer:

```rss
use ngx;
let sock = ngx::socket::tcp();
ngx::socket::tcp::settimeout(sock, 1000);
ngx::socket::tcp::connect(sock, "127.0.0.1", 8080);
ngx::socket::tcp::send(sock, "ping");
ngx::socket::tcp::receive(sock, 4);
ngx::socket::tcp::close(sock);
```

TCP keepalive (worker-local pool) and TLS handshake (Tokio/rustls):

```rss
use ngx;
let sock = ngx::socket::tcp();
ngx::socket::tcp::settimeout(sock, 1000);
ngx::socket::tcp::connect(sock, "127.0.0.1", 8080);
let store = ngx::socket::tcp::trustedstore(ca_pem);
ngx::socket::tcp::settrustedstore(sock, store);
ngx::socket::tcp::sslhandshake(sock, null, "localhost", true, null);
ngx::socket::tcp::send(sock, "ping");
ngx::socket::tcp::receive(sock, 4);
ngx::socket::tcp::setkeepalive(sock, 60000, 30);
```

`ngx::sleep` is a `pd-host-function` async host (`seconds:number -> null`).
`seconds` is an OpenResty-style integer or float; negative, NaN, infinity,
and duration overflow are rejected. `ngx::sleep_ms` is the millisecond
integer form (`ms:int -> null`). The generated wrapper submits an owned
host future; the impl validates the duration and awaits Tokio. The first
host entry returns `VmStatus::Waiting`. Two sequential sleeps reuse the
same generation-safe request identity and each wait once.

On each request the access handler leases a worker-thread-local VM via
`Vm::new_shared(Arc<Program>)`. Import-free programs run to
`VmStatus::Halted` and return `NGX_OK` so the content phase proceeds. After
`Halted` and after a recoverable runtime error the VM is reset with
`Vm::reset_for_reuse` and parked only when `Vm::is_reusable` is true. Idle
pool size is bounded. VM errors and unexpected `Waiting` on import-free
programs fail the request with 500.

Scripts that imported `ngx::sleep`, `ngx::sleep_ms`, or UDP/TCP cosocket
hosts bind a request-local VM instead of the pool. `Waiting` returns
`NGX_AGAIN` without incrementing `r->main->count`.
The eventfd callback polls the waiting host op, resumes the same VM on the
worker main thread, and continues `ngx_http_core_run_phases` exactly once
so the access handler can return `NGX_OK`. Host-bound waiting VMs are never
returned to the pool.

## Runtime and compatibility limits

The current ngx host surface (signatures, phases, yield behavior,
limits, and OpenResty adaptations) is listed in
[docs/ngx-api-compatibility.md](docs/ngx-api-compatibility.md).
`ngx::thread::{spawn,wait,kill}`, `ngx::timer::{at,every}`,
`ngx::on_abort`, `ngx::semaphore::*`, and `ngx::run_worker_thread` are
not production hosts; configuration compile rejects those imports.
The callback execution contract is
[docs/design/ngx-scheduler.md](docs/design/ngx-scheduler.md).
Content-phase `ngx::send_headers` / `ngx::print` / `ngx::say` /
`ngx::flush` / `ngx::eof` copy payload on the worker thread into
request-pool buffers and send through `ngx_http_output_filter`. Access
rejects those imports at config time.

- Worker runtime is created only in NGINX worker `init_process` after fork,
  and only when `ngx_process` is `NGX_PROCESS_WORKER` or
  `NGX_PROCESS_SINGLE`. Helper processes skip initialization. `nginx -t`,
  master, and config never create Tokio threads or the eventfd.
  `exit_process` shuts the runtime down and is idempotent.
- Tokio features: `rt` + `rt-multi-thread` + `time` + `net` + `sync` + `io-util`
  (UDP/TCP cosocket; no HTTP client). One worker thread named
  `ngx-rustscript-async`. The I/O driver is enabled alongside time.
- Async wait capacity is `DEFAULT_MAX_IN_FLIGHT` (1024). Reserve is rejected
  when full. Host futures and wakers own only a queue sender and request
  identity — no NGINX request/pool/VM pointers cross into Tokio, and Tokio
  never calls `ngx_post_event`.
- Completions enqueue the request identity under a mutex, then write
  `eventfd(2)` created with `EFD_NONBLOCK|EFD_CLOEXEC`. The worker main
  thread registers the fd with `ngx_get_connection`. The pinned Linux epoll
  path uses `add` with `EPOLLIN`.
- Client close while a wait is in flight is detected with a worker-thread
  wrapper around `ngx_http_test_reading`. Confirmed close finalizes once
  with `NGX_HTTP_CLIENT_CLOSED_REQUEST` (access log 499), then request-pool
  cleanup invalidates the identity and drops the host future.
- Worker `exit_process` rejects new work, invalidates senders/wakers,
  cancels/drops host futures, unregisters/closes eventfd once, and clears
  worker registries without touching request pointers. Long sleeps do not
  delay graceful worker exit (SIGQUIT).
- `pd-vm` pin `4b749862ff1632ef1266d75709da3676de37356c`, packages `pd-vm`
  and `pd-host-function`. This pin has `Vm::new_shared(Arc<Program>)`,
  `HostAsyncBridge`, `Vm::set_async_bridge`, `VmStatus::Waiting`,
  `Vm::poll_waiting_host_op`, and `Vm::resume`. `try_new` only wraps owned
  `Vm::new` after frame-limit validation. Normal termination is
  `Ok(VmStatus::Halted)`. Drop of `Vm` owns cancellation and resource
  closure.

## Tests and acceptance

```sh
export NGINX_SOURCE_DIR
export NGINX_BUILD_DIR
export CARGO_TARGET_DIR
export NGX_RUSTSCRIPT_MODULE="$(readlink -f "$CARGO_TARGET_DIR/debug/libngx_rustscript.so")"
cargo test
python3 -m py_compile tests/nginx_integration.py tests/test_nginx_integration_unit.py
python3 tests/test_nginx_integration_unit.py
python3 tests/nginx_integration.py --nginx <absolute pinned nginx> --module <absolute actual cdylib>
bash tests/verify_add_dynamic.sh
```

The standalone runner:

```sh
python3 tests/nginx_integration.py --nginx <absolute pinned nginx> --module <absolute actual cdylib>
```

It checks that the nginx binary and module paths are absolute, that nginx
reports `nginx/1.30.4` with `--with-compat` and `--with-http_ssl_module`,
and that `nginx -t` accepts a valid sleep config while missing,
syntax, unsupported-import, and wrong-arity fixtures fail with those
causes. Against a live isolated master (`master_process on`,
`worker_processes 1`, foreground) it checks that a 2s sleep does not block
`/plain/` (plain returns HTTP 200 first), that one-sleep and two-sleep
routes return distinct 200 bodies, that 12 overlapping requests across two
routes keep per-route bodies, that a client close of a 30s sleep logs
access status 499 and leaves later routes working, and that SIGQUIT of that
master exits successfully while a long sleep is in flight.

Python stdlib only. The runner owns its prefix, config, pid, logs, and
loopback port, and reaps only its own process. Isolated prefixes go under
`tempfile.gettempdir()` (typically `$TMPDIR`). Set `NGX_RUSTSCRIPT_TASK_TMP`
to a directory to place them elsewhere.

## Troubleshooting

- `dlopen() ... failed` — the module path is wrong, not a regular file, or
  was built against a different NGINX ABI than the binary that loads it.
- `nginx version is ..., expected nginx/1.30.4` — use the pinned 1.30.4
  binary from `$NGINX_BUILD_DIR/nginx`, not a distro package.
- bindgen / include errors — unset `CFLAGS`, `CPPFLAGS`, and
  `BINDGEN_EXTRA_CLANG_ARGS`; install clang and libclang; point
  `NGINX_SOURCE_DIR` / `NGINX_BUILD_DIR` at the configured 1.30.4 tree.
- `script imports are not supported` / `failed to compile script` — only
  catalogued ngx hosts allowed in the active directive phase may be imported.
  `access_by_rss_file`, `content_by_rss_file`, and `log_by_rss_file` have
  distinct phase allowlists; see the compatibility matrix. Bare `print` and
  `runtime::sleep` are rejected.
- HTTP 500 from an access script — VM runtime error or unexpected Waiting on
  an import-free program. Check the error log.
- HTTP 499 on a sleep route — the client closed the connection before the
  sleep completed.
- `CARGO_TARGET_DIR` mismatch — `load_module` must receive the cdylib built
  in the same target dir (`NGX_RUSTSCRIPT_MODULE`).

## Related projects

- [ngx-rust](https://github.com/nginx/ngx-rust) — NGINX Rust SDK
- [rustscript](https://github.com/rustscript-lang/rustscript) — compiler and `pd-vm`
