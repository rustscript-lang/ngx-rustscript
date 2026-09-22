# ngx-rustscript

NGINX dynamic HTTP module that hosts RustScript (`pd-vm`) at the access,
content, and log phases. Location directives `access_by_rss_file <path>`,
`content_by_rss_file <path>`, and `log_by_rss_file <path>` compile the
script once at configuration time.

Import-free and pure synchronous scripts use a worker-local VM pool.
Request-state scripts use a request-owned `AccessExec` and a fresh request
context; generic host futures return `NGX_AGAIN` only when they are genuinely pending
and resume through the worker eventfd callback. Statically quiescent
request-state programs, including buffered `http::client::request`, key-auth
control flows, and `ngx.ctx`, may reuse a VM after successful `Halted` cleanup.
Their pool key contains the exact compiled `Arc<Program>` identity, phase,
complete standard-host policy, ngx catalog fingerprint, and standard catalog
fingerprint. SQLite, standard I/O, SSE, timers, sleep, cosockets, resource or
unknown host schemas, failures, cancellation, timeouts, pending cleanup, and
over-capacity returns are excluded. The worker retains at most four reusable
request VMs, installs a fresh `NgxAsyncBridge` and runtime policy on checkout,
and clears request `ngx.ctx`, bridge, print sink, futures, resources, and reset
state before return. Client disconnect during a wait finalizes the request as
HTTP 499. Graceful master shutdown cancels in-flight host futures. The log
phase stays synchronous and fail-closed.

## Documentation

- [Documentation index](docs/index.md)
- [Directives](docs/directives.md) — syntax, context, inheritance, defaults, errors
- [API reference](docs/api/index.md) — 199 ngx hosts by family
- [Execution model](docs/execution-model.md)
- [Security and policy](docs/security.md)
- [Examples](docs/examples/index.md)
- [Compatibility](docs/ngx-api-compatibility.md)

## Requirements

| Component | Pin |
| --- | --- |
| [ngx-rust](https://github.com/nginx/ngx-rust) | `cda9d8372ccd2e139ef2b03e5806e019e7796187` (`ngx` / `nginx-sys` 0.5.0) |
| NGINX | Official NGINX 1.31.4 (`nginx_version` 1031004), tarball SHA-256 `e6f20b644a17a643f059ae6467a1971fe2811587d025e071068753a1f1e3b3c3` |
| RustScript / `pd-vm` | [`https://github.com/rustscript-lang/rustscript`](https://github.com/rustscript-lang/rustscript) at `5bd2788b620980e1d2ae6a146e4f7548c68d1882` (Cargo git dependency `package = "pd-vm"` on that immutable rev; no sibling path) |
| Rust toolchain | 1.85.0 or later (ngx-rust MSRV). Verified with rustc 1.94.1 |

Required build tools on Linux:

- C compiler, make, OpenSSL, PCRE2, zlib
- clang and libclang (official `nginx-sys` bindgen)
- Rust 1.85+
- GNU `nm` on Linux for a minimal check that the production cdylib does not
  define test-only NGINX C stubs

## ABI matching

`nginx-sys` generates bindings from the **configured** NGINX source tree. The
module `.so` must be built against the same source and `./configure` flags as
the NGINX binary that loads it. Do not mix another NGINX/OpenResty build
with this module.

Official `nginx-sys` inputs (do not invent extra variables):

- `NGINX_SOURCE_DIR` — absolute path to the NGINX source directory after
  `./configure`
- `NGINX_BUILD_DIR` — absolute path to the build directory (`objs` by default,
  or an official `--builddir=` path)

Only `./configure` is required for bindgen. The fixture below also runs `make`
so `nginx -t` can load the module. An explicit `--builddir=` path is accepted
and validated as a path-shaped configure argument.

The selected build path is `nginx-sys` bindgen against configured official
NGINX 1.31.4. Pre-generated or vendored bindings are not used: they cannot
match ngx-rust `cda9d8372ccd2e139ef2b03e5806e019e7796187` and the selected
generated headers. Install clang and libclang so rust-bindgen can generate
bindings at `cargo build`.

Clear inherited `CFLAGS` / `BINDGEN_EXTRA_CLANG_ARGS` before NGINX
`./configure`. Set both to `-I$NGINX_BUILD_DIR` for Cargo so ngx-rust bindgen,
the request shim, and the ABI probes select the same generated headers.

## Compilation / build / install

```sh
# Run from the crate root. Capture absolute paths before changing directories.
CRATE_ROOT="$PWD"
NGINX_SOURCE_DIR="$CRATE_ROOT/nginx-1.31.4"
NGINX_BUILD_DIR="$NGINX_SOURCE_DIR/objs"

# 1. Official NGINX 1.31.4, verified before extraction.
curl -fL -o nginx-1.31.4.tar.gz https://nginx.org/download/nginx-1.31.4.tar.gz
printf '%s  %s\n' \
  e6f20b644a17a643f059ae6467a1971fe2811587d025e071068753a1f1e3b3c3 \
  nginx-1.31.4.tar.gz | sha256sum --check --strict
tar -xf nginx-1.31.4.tar.gz
cd "$NGINX_SOURCE_DIR"
unset CFLAGS CPPFLAGS BINDGEN_EXTRA_CLANG_ARGS
./configure --with-compat --with-http_ssl_module
make -j"$(nproc)"

# 2. return to the crate and build the cdylib against that exact tree
cd "$CRATE_ROOT"
export NGINX_SOURCE_DIR
export NGINX_BUILD_DIR
export NGX_RUSTSCRIPT_NGINX="$NGINX_BUILD_DIR/nginx"
export CFLAGS="-I$NGINX_BUILD_DIR"
export BINDGEN_EXTRA_CLANG_ARGS="-I$NGINX_BUILD_DIR"
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

Standalone `cargo test` validates the explicit `$NGX_RUSTSCRIPT_NGINX`
binary, `nginx/1.31.4`, and `--with-http_ssl_module` in the
module-load harness before `nginx -t`. Quoted configure paths, `--prefix`,
`--builddir`, `--add-dynamic-module`, and other valid NGINX flags are
accepted.

Tests resolve `${CARGO_TARGET_DIR:-<crate>/target}/debug/libngx_rustscript.so`
(or `NGX_RUSTSCRIPT_MODULE` when set) and pass that absolute path to
`load_module`. A missing or non-regular shared library is rejected. `nm -D
--defined-only` rejects a production cdylib that defines test-only NGINX C
stubs. `nginx -t`, real module load, and HTTP are the acceptance evidence.

Tests require `$NGINX_BUILD_DIR` to provide the selected generated headers and
`$NGX_RUSTSCRIPT_NGINX` to report `nginx/1.31.4` with
`--with-http_ssl_module`. Fixture `nginx.conf.in` interpolates quoted paths;
unsafe bytes (NUL, newline, non-UTF-8) are rejected fail-closed, and
`nginx -t` receives `-p` / `-c` as `OsStr`.

An already configured OpenResty 1.31.4.1 tree may be used as an explicit local
fixture by pointing `NGINX_SOURCE_DIR`, `NGINX_BUILD_DIR`, and
`NGX_RUSTSCRIPT_NGINX` at that same build and setting
`NGX_RUSTSCRIPT_SERVER_VARIANT=openresty`. The Python acceptance runner uses
the corresponding explicit `--server-variant openresty` option. CI and release
artifacts always use the verified official NGINX source above.

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

### Release packages

Pushing a `v*` tag whose version matches `Cargo.toml` creates a GitHub Release.
The Release workflow can also be started manually with an existing `v*` tag.
Linux amd64 releases provide:

- `ngx-rustscript-module-<version>-linux-amd64.tgz`
- `ngx-rustscript-nginx-<version>-nginx-1.31.4-linux-amd64.tgz`
- `nginx-module-rustscript_<version>_amd64.deb`
- `SHA256SUMS`

The module tgz contains `lib/ngx_http_rustscript_module.so`, the README, the
complete documentation tree, and the deployment examples. The NGINX bundle
adds the pinned official NGINX 1.31.4 executable, a ready module-loading configuration,
static pages, temporary directories, and `bin/nginx-rustscript`, which runs the
included NGINX with the extracted directory as its prefix:

```sh
tar -xzf ngx-rustscript-nginx-<version>-nginx-1.31.4-linux-amd64.tgz
cd ngx-rustscript-nginx-<version>-nginx-1.31.4-linux-amd64
bin/nginx-rustscript -t
bin/nginx-rustscript
```

The deb installs the module at
`/usr/lib/nginx/modules/ngx_http_rustscript_module.so`. It does not edit NGINX
configuration or restart NGINX. Add an explicit top-level directive after
confirming that the target is NGINX 1.31.4 with matching generated headers and
matching build flags:

```nginx
load_module /usr/lib/nginx/modules/ngx_http_rustscript_module.so;
```

Verify downloaded assets before installation:

```sh
sha256sum --check SHA256SUMS
dpkg-deb --info nginx-module-rustscript_<version>_amd64.deb
dpkg-deb --contents nginx-module-rustscript_<version>_amd64.deb
```

## Quick start

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

`access_by_rss_file`, `content_by_rss_file`, and `log_by_rss_file` are
location-only, one argument each, and inherit independently. pd-vm I/O,
HTTP, and SQLite start denied: grant `rss_io_root`, `rss_io_write`,
`rss_http_scheme`, one `rss_http_allow <host> <port>` pair per location,
`rss_http_allow_private`, and `rss_sqlite_root` (required even for an
in-memory database). A second HTTP pair is rejected during configuration.
I/O process spawning and unsafe SQLite SQL remain disabled.

Full syntax, inheritance, defaults, and error strings:
[docs/directives.md](docs/directives.md).

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
reports `nginx/1.31.4` with `--with-http_ssl_module`,
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

Documentation links:

```sh
python3 scripts/check_docs_links.py
```

## Troubleshooting

- `dlopen() ... failed` — the module path is wrong, not a regular file, or
  was built against a different NGINX ABI than the binary that loads it.
- `nginx version is ..., expected nginx/1.31.4` — use the selected official
  NGINX binary from `$NGX_RUSTSCRIPT_NGINX`, or explicitly select the local
  OpenResty fixture mode described above.
- bindgen / include errors — install clang and libclang; point
  `NGINX_SOURCE_DIR` / `NGINX_BUILD_DIR` at the configured NGINX 1.31.4 core
  and set `CFLAGS` / `BINDGEN_EXTRA_CLANG_ARGS` to `-I$NGINX_BUILD_DIR`.
- `script imports are not supported` / `failed to compile script` — only
  catalogued ngx hosts allowed in the active directive phase may be imported.
  `access_by_rss_file`, `content_by_rss_file`, and `log_by_rss_file` have
  distinct phase allowlists; see the [API reference](docs/api/index.md).
  Bare `print` and `runtime::sleep` are rejected.
- HTTP 500 from an access script — VM runtime error or unexpected Waiting on
  an import-free program. Check the error log.
- HTTP 499 on a sleep route — the client closed the connection before the
  sleep completed.
- `CARGO_TARGET_DIR` mismatch — `load_module` must receive the cdylib built
  in the same target dir (`NGX_RUSTSCRIPT_MODULE`).

## Related projects

- [ngx-rust](https://github.com/nginx/ngx-rust) — NGINX Rust SDK
- [rustscript](https://github.com/rustscript-lang/rustscript) — compiler and `pd-vm`
