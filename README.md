# ngx-rustscript

NGINX dynamic HTTP module that hosts RustScript (`pd-vm`) at the access
phase. Location directive `access_by_rss_file <path>` compiles the script
once at configuration time and runs it synchronously to `Halted` on each
request. Host sleep, Tokio, eventfd, and request suspension are not
implemented yet.

## Pinned revisions

| Component | Pin |
| --- | --- |
| [ngx-rust](https://github.com/nginx/ngx-rust) | `cda9d8372ccd2e139ef2b03e5806e019e7796187` (`ngx` / `nginx-sys` 0.5.0, `nginx-src` 1.30.0+1.30.4) |
| NGINX | 1.30.4 (`nginx_version` 1030004). Tarball SHA-256 `4261dc90e9e47c1c4041276e9aaa3d48ebe2e664f728e14fa95ae6c67d57a08b` |
| RustScript / `pd-vm` | [`https://github.com/rustscript-lang/rustscript`](https://github.com/rustscript-lang/rustscript) at `4b749862ff1632ef1266d75709da3676de37356c` (Cargo git dependency `package = "pd-vm"` on that immutable rev; no sibling path) |
| Rust toolchain | 1.85.0 or later (ngx-rust MSRV). Verified with rustc 1.94.1 |

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
and validated as a path-shaped configure argument; the machine path is not
hashed into the module identity fingerprint.

The selected required build path is official `nginx-sys` bindgen against this
pinned, configured NGINX 1.30.4 tree. Pre-generated or vendored bindings are
not used: they cannot match ngx-rust `cda9d8372ccd2e139ef2b03e5806e019e7796187`
and NGINX 1.30.4. Install clang and libclang so rust-bindgen can generate
bindings at `cargo build`.

Required build tools on Linux:

- C compiler, make, OpenSSL, PCRE2, zlib
- clang and libclang (official `nginx-sys` bindgen)
- Rust 1.85+
- GNU binutils `nm`, `readelf`, and `objcopy` on a compatible Linux host.
  Tests copy the candidate `.so` once into a private snapshot, then inspect
  that snapshot only. `nm -D --defined-only --format=posix` requires the
  generic ngx-rust exports (`ngx_modules`, `ngx_module_names`,
  `ngx_module_order`). `readelf -W --dyn-syms` requires
  `ngx_rustscript_module_identity` to be bind `GLOBAL`, ELF type `OBJECT`,
  size 32. `readelf -W -S` plus `objcopy --dump-section` of the dedicated
  section `.ngx_rustscript_module_identity` must yield exactly 32 bytes that
  match a fingerprint recomputed from current sources. ELF class and
  endianness follow whatever GNU binutils accepts on the host (not ELF64
  little-endian only). Tests do not `dlopen` the artifact, parse ELF
  headers, or parse unbounded C strings for identity.

Clear inherited `CFLAGS` / `BINDGEN_EXTRA_CLANG_ARGS` before `./configure` and
`cargo build`. A leaked include path from another NGINX tree will change
`ngx_auto_config.h` and break the link.

## Build the NGINX fixture and module

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
is not a dynamic symbol. `build.rs` also embeds a 32-byte identity/freshness
fingerprint as the exported `OBJECT` `ngx_rustscript_module_identity` in ELF
section `.ngx_rustscript_module_identity`. The digest is a deterministic
std-only 4-lane FNV-1a-64 over the crate's local production inputs
(`Cargo.toml`, `Cargo.lock`, `build.rs`, `src/lib.rs`, `src/config.rs`,
`src/access.rs`, `src/vm_pool.rs`, `src/sleep_binding.rs`, `config`,
`config.make`, `auto/rust`) plus pinned NGINX
configured ABI inputs
(`src/core/nginx.h`, generated `ngx_auto_config.h` / `ngx_auto_headers.h`,
and the nginx-sys bindgen-driving `Makefile` `ALL_INCS`/`CFLAGS` extract).
Machine/path-only data in `ngx_auto_config.h` (especially `NGX_CONFIGURE`
`--prefix` / `--builddir` / `--add-dynamic-module` values and `NGX_PREFIX`)
is replaced with stable placeholders before hashing. It is not a
cryptographic MAC. `build.rs` requires `NGINX_SOURCE_DIR` and
`NGINX_BUILD_DIR` and emits `cargo::rerun-if-env-changed` plus
`cargo::rerun-if-changed` for each hashed file. Identity is bound to those
configured source/build ABI inputs only; `build.rs` does not require
`$NGINX_BUILD_DIR/nginx`.

Standalone `cargo test` validates the canonical `$NGINX_BUILD_DIR/nginx`
binary, `nginx/1.30.4`, `--with-compat`, and `--with-http_ssl_module` in the
module-load harness before `nginx -t`. Quoted configure paths, `--prefix`,
`--builddir`, `--add-dynamic-module`, and other valid NGINX flags are
accepted. An official `--builddir=` path is allowed; the machine path is not
part of the identity fingerprint.

Tests resolve `${CARGO_TARGET_DIR:-<crate>/target}/debug/libngx_rustscript.so`
(or `NGX_RUSTSCRIPT_MODULE` when set), copy that candidate once to a unique
owner-only snapshot (RAII-cleaned), freeze it read-only after validation,
and run `nm` / `readelf` / `objcopy` plus the final `nginx -t` against that
snapshot. A missing or wrong shared library is rejected. Identity requires
the dedicated `PROGBITS` section, a `GLOBAL OBJECT` of size 32 whose dynsym
`Ndx` is that unique section index, and a symbol address range inside the
section. The dumped 32 bytes must match a fingerprint the test recomputes
from the same inputs. Fingerprint is authoritative. A different ngx-rust
module that only exports the generic `ngx_modules` / `ngx_module_names` /
`ngx_module_order` symbols is rejected, as are an identity symbol with the
wrong ELF type, bind, size, or section (`ABS` / `.data`), a missing/NOBITS
identity section, and a correctly shaped object with the wrong fingerprint.

Tests also require the selected nginx binary to be canonical
`$NGINX_BUILD_DIR/nginx` reporting `nginx/1.30.4` with `--with-compat` and
`--with-http_ssl_module`. An official `--builddir=` path is accepted; the
machine path is not part of the identity fingerprint. Fixture `nginx.conf.in`
interpolates quoted paths; unsafe bytes (NUL, newline, non-UTF-8) are
rejected fail-closed, and `nginx -t` receives `-p` / `-c` as `OsStr`.

### Optional: NGINX `--add-dynamic-module`

`config`, `config.make`, and `auto/rust` follow ngx-rust
`examples/{config,config.make,auto/rust}` at the same git revision. That path
builds a **staticlib** with `--no-default-features` (no `ngx_modules` table;
NGINX generates it). Use it only when configuring NGINX with
`--add-dynamic-module=/path/to/ngx-rustscript`. `make -j` can compile the
staticlib in parallel with `objs/nginx` because `build.rs` fingerprints
configured ABI inputs and does not wait for the linked binary.

Full configure + parallel make + emitted identity + `nginx -t` is
`tests/verify_add_dynamic.sh` (a standalone script so nested `cargo rustc`
cannot deadlock a parent `cargo test`). After `cargo build --lib`, run:

```sh
export CARGO_TARGET_DIR
export NGINX_SOURCE_DIR
bash tests/verify_add_dynamic.sh
```

## Prove load with `nginx -t`

```sh
export NGX_RUSTSCRIPT_NGINX="$NGINX_BUILD_DIR/nginx"
: "${CARGO_TARGET_DIR:=$CRATE_ROOT/target}"
export CARGO_TARGET_DIR
cargo build --lib
export NGX_RUSTSCRIPT_MODULE="$(readlink -f "$CARGO_TARGET_DIR/debug/libngx_rustscript.so")"
cargo test --test module_load -- --nocapture
```

The tests create an isolated prefix from `tests/fixtures/nginx.conf.in`:

1. `load_module` of a missing path — `nginx -t` fails `dlopen()` of that exact module path in the fixture conf
2. `load_module` of `libngx_rustscript.so` — `nginx -t` reports `test is successful`

Manual check:

```sh
nginx -t -p /path/to/prefix -c conf/nginx.conf
```

## `access_by_rss_file`

Location-only directive, exact one argument:

```nginx
location /t {
    access_by_rss_file /path/to/script.rss;
}
```

Relative paths resolve with NGINX `ngx_conf_full_name` against the conf
prefix (the directory of the config file). Absolute paths are unchanged.
The file is read once at `nginx -t` / reload, must be a regular UTF-8 file,
and is compiled with `compile_source`. Every non-empty `Program.imports`
entry is rejected at configuration time: `print`, `runtime::sleep`,
`ngx::sleep`, and any other host import are not bound in this milestone.
Unknown modules fail compile or import check. Duplicate directives in the
same location are rejected. Unset nested locations inherit the parent
program; an explicit child directive overrides. Unconfigured locations keep
the original access-phase decline.

On each request the access handler leases a worker-thread-local VM via
`Vm::new_shared(Arc<Program>)`, runs to `VmStatus::Halted`, and returns
`NGX_OK` so the content stage proceeds. After `Halted` and after a
recoverable runtime error the VM is reset with `Vm::reset_for_reuse` and
parked only when `Vm::is_reusable` is true. Idle pool size is bounded.
VM errors and unexpected `Yielded` / `Waiting` fail the request with 500.

## pd-vm signatures (locked)

Verified at RustScript `4b749862ff1632ef1266d75709da3676de37356c`. Package
`pd-vm`, lib name `vm`. Later tasks should use these APIs rather than
`Vm::new`. This pin has `Vm::new_shared(Arc<Program>)` rather than
`try_new_shared`; `try_new` only wraps owned `Vm::new` after frame-limit
validation.

```rust
pub fn compile_source(source: &str) -> Result<CompiledProgram, SourceError>;

impl Vm {
    pub fn try_new(program: Program) -> VmResult<Self>;
    pub fn new_shared(program: Arc<Program>) -> Self;
    pub fn run(&mut self) -> VmResult<VmStatus>;
    pub fn resume(&mut self) -> VmResult<VmStatus>;
    pub fn reset_for_reuse(&mut self) -> VmResult<()>;
    pub fn poll_reset_for_reuse(&mut self, cx: &mut Context<'_>) -> Poll<VmResult<()>>;
    pub fn is_reusable(&self) -> bool;
    pub fn scope_reset_pending(&self) -> bool;
    pub fn bind_args_function(
        &mut self,
        name: impl Into<String>,
        function: Box<dyn HostArgsFunction>,
    );
    pub fn bind_static_args_function(
        &mut self,
        name: impl Into<String>,
        function: StaticHostArgsFunction,
    );
}

pub type StaticHostArgsFunction = fn(&[Value]) -> VmResult<CallOutcome>;

pub enum CallOutcome {
    Return(CallReturn),
    Halt,
    Yield,
    Pending(HostOpId),
}

pub enum VmStatus {
    Halted,
    Yielded,
    Waiting(HostOpId),
}

impl HostFunctionRegistry {
    pub fn bind_vm_cached(&self, vm: &mut Vm) -> VmResult<()>;
}
```

Normal termination is `Ok(VmStatus::Halted)`. Host `Yield` maps to
`VmStatus::Yielded`; `resume()` re-enters the same host call. Drop of `Vm`
owns cancellation and resource closure.

## `ngx::sleep` VM contract (locked)

Verified with real `compile_source` / `compile_source_with_flavor_and_options`,
real `Vm`, and `Vm::bind_args_function` at the pin above. Tests live in
`tests/vm_sleep.rs` with fixtures `tests/fixtures/sleep.rss`,
`tests/fixtures/two_sleeps.rss`, and `tests/fixtures/sleep_null.rss`.
A cloneable `SleepCoordinator` is the completion and one-time
`(token, ms)` claim handle. Each VM owns a non-`Clone` `SleepSession`
host binding. Completions are explicit `complete(token)` — not Tokio,
not eventfd, not NGINX timers.

Contract:

- Name `ngx::sleep`. One integer argument `ms` (milliseconds),
  nonnegative, range-checked at the host. Return is deterministic `null`
  (`HostTypeSchema::Null` / `Value::Null`).
- First host entry parses and stores `ms`, registers one in-flight record
  with a checked generation plus operation id, enqueues one claimable
  `(token, ms)` submission, then returns `CallOutcome::Yield`. No
  subsequent script action runs until completion. `claim_submission`
  supplies that duration without reading the VM stack.
- Accidental `vm.resume()` before completion yields again
  (`VmStatus::Yielded`) on the same call IP with the same `[Value::Int(ms)]`
  arguments and must never resubmit.
- After `SleepCoordinator::complete(token)`, `vm.resume()` re-enters the
  same host call, the session consumes that completion exactly once
  (removing the in-flight record), returns `CallOutcome::Return` of
  `null`, and the script reaches `VmStatus::Halted` with a later IP.
  Consume is not part of the coordinator API.
- Two sequential sleeps use distinct tokens and each submit exactly once.
  Status sequence for two sleeps with controlled completion:
  `Yielded`, `Yielded`, `Halted`. Two sessions that share one coordinator
  stay isolated: completing or dropping one does not complete the other.
- Dropping a session invalidates its generation, cancels its
  submissions, and makes a late `complete` fail closed
  (`GenerationInvalid`). Duplicate, unknown, not-completed, exhausted-id,
  and poisoned-mutex errors are distinct. Coordinator state is bounded by
  in-flight operations.
- Config still rejects every import, including `ngx::sleep`. Access-phase
  NGINX/Tokio integration remains pending. A `Yielded` VM is still
  non-reusable and dropped by the worker-local pool.

## Related projects

- [ngx-rust](https://github.com/nginx/ngx-rust) — NGINX Rust SDK
- [rustscript](https://github.com/rustscript-lang/rustscript) — compiler and `pd-vm`
