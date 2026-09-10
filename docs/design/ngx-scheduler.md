# ngx callback / scheduler execution contract

Feasibility gate for OpenResty-oriented callable families:

- `ngx::thread::{spawn,wait,kill}`
- `ngx::timer::{at,every}`
- `ngx::on_abort`
- semaphore (`ngx::semaphore::{new,wait,post,count}`)
- `ngx::run_worker_thread`

This document is the lifecycle contract. It is **not** permission to
register scheduler hosts. The production surface stays **191** hosts
until an upstream `pd-vm` public API that satisfies the invariants
below is designed, released, and pinned.

Pinned `pd-vm` evidence in this file is taken from:

| Pin | Value |
| --- | --- |
| Crate metadata | `Cargo.toml` `rustscript-rev` / `vm` git `rev` |
| `Cargo.lock` | `pd-vm` `source = "git+https://github.com/rustscript-lang/rustscript?rev=4b749862ff1632ef1266d75709da3676de37356c#4b749862ff1632ef1266d75709da3676de37356c"` |
| Local Cargo git checkout | `~/.cargo/git/checkouts/rustscript-9f3ff51a9d80666b/4b74986` (same rev also present under `rustscript-ab116a0775a56744/4b74986`) |

Public versus crate-private is taken from that checkout's `src/lib.rs`
re-exports and item visibility. Do not treat crate-private helpers as
embedder API.

## Decision

**Overall public API is insufficient for callable families. Keep
fail-closed.**

| Family | Decision | Why |
| --- | --- | --- |
| `ngx::thread::{spawn,wait,kill}` | **blocked** | Needs concurrent child frames / independent invocation while the parent VM is live and possibly `Waiting`. Public `Vm::start_callable` / `invoke_callable` / `start_invocation` require a halted VM and a single active invocation. |
| `ngx::timer::{at,every}` | **blocked** | Needs a detached Program/VM, attenuated capabilities, and a callable that must not retain request `raw` / ctx / parent VM. No public detached-VM or unforgeable callable handle. |
| panic / error / shutdown / reload / limits for those families | **blocked** | No public cancellable child-task handle, no generation-safe detached identity, no exactly-once child completion protocol that composes with the request eventfd bridge. |
| `ngx::on_abort` | **blocked** | Needs a single callable, request-abort timing, and cleanup that cannot re-enter the dying request VM with live FFI pointers. |
| semaphore | **feasible now** (not registered) | Counter + `wait` can reuse generic `HostFuture` + `ResourceHandle` without invoking script callables. That does **not** unlock thread/timer/`on_abort`/`run_worker_thread`. |
| `ngx::run_worker_thread` | **blocked** | Needs a configured pool, module/function ABI, and an owned `Send` value boundary. `Value::Callable` / request resources must not cross the pool. |

Semaphore remaining unregistered is intentional: host count stays 191,
and a semaphore landing must still pass request-resource escape and
reload budgets. Feasible ≠ implemented.

Contract tests: `tests/ngx_scheduler_contract.rs`.

## Current execution architecture

ngx-rustscript runs one RustScript program per configured phase slot
(`access_by_rss_file`, `content_by_rss_file`, `log_by_rss_file`).

```
nginx worker main thread
  AccessHandler / ContentHandler / LogHandler
    LocationConf.program : Arc<Program>          // config-time, shared
    import-free / pure-sync  -> worker VM pool   // Vm::new_shared, reset_for_reuse
    async / ctx / cosocket / mutation / control
                         -> request-owned AccessExec
                              Vm on worker main thread
                              NgxAsyncBridge : HostOpId -> HostFuture
                              Waiting -> NGX_AGAIN
                              waker owns CompletionSender + RequestIdentity
                              eventfd -> worker main -> poll_waiting_host_op
                                       -> Vm::resume -> ngx_http_core_run_phases
    log slot: Waiting is fail-closed ("log handler cannot wait")
```

Invariants already enforced by this module (not optional for scheduler
work):

1. Every NGINX request pointer and every `Vm` stays on the worker main
   thread (`src/access.rs`, `src/content.rs`, `src/log.rs`,
   `src/request_state.rs`, `.agents/skills/ngx-rustscript-development/SKILL.md`).
2. Tokio futures and wakers carry only owned `Send + 'static` data and
   generation-safe `RequestIdentity` (`src/notify.rs`, `src/runtime.rs`).
   They never hold request / pool / VM / raw pointers and never call
   `ngx_post_event`.
3. Outbound I/O hosts use Tokio only. NGINX FFI is request-lifecycle
   integration on the worker main thread.
4. `HostAsyncBridge` (`src/async_bridge.rs` `NgxAsyncBridge`) is the
   single future table. Ready / error / cancel remove the future.
   Capacity `DEFAULT_MAX_IN_FLIGHT` (1024) is reserved exactly once and
   released exactly once.
5. Client close during a wait finalizes HTTP 499. Worker `exit_process`
   cancels in-flight futures and closes eventfd through the fd gate.
6. `ngx::ctx` deep-owns scalars/containers and **rejects**
   `Value::Callable` (`src/host/request_ctx.rs`).
7. Cosocket / shared-dict / TLS objects are `ResourceHandle` values in
   the **current** VM execution-scope table. Script sees them as `i64`
   (`src/socket/udp.rs` `parse_handle`, TCP/TLS equivalents). Handles
   are generation-tagged integers, not transferable identities.

Root execution is `Vm::run` / `Vm::resume`. The module does not use
`ScriptCallback`, `Store`, `Invocation`, or `Vm::invoke_callable` for
production request handling.

## Evidence matrix (pinned `pd-vm` 4b74986)

Legend: **P** = public (`src/lib.rs` re-export or `pub` item on a public
type). **C** = crate-private / `pub(super)` / `pub(crate)`.

### Values, callables, resources

| Item | Vis | Path | Notes |
| --- | --- | --- | --- |
| `Value` | P | `src/bytecode.rs` | `Null/Int/Float/Bool/String/Bytes/Array/Map/Callable`. **No resource variant.** |
| `SharedCallable = Arc<CallableValue>` | P | `src/bytecode.rs` | Clone shares the Arc. |
| `CallableValue { prototype_id, kind, env }` | P | `src/bytecode.rs` | No Program identity field. Env is `Option<Arc<CallableEnvironment>>` with `pub(crate) cells: Mutex<Vec<SharedCaptureCell>>`. |
| `ExportedCallable { name, local_slot }` | P | `src/bytecode.rs` | Metadata only; not a handle. |
| `HostTypeSchema::Callable { params, result }` | P | `src/host_api.rs` | Catalog type. ngx catalog currently has **zero** Callable params/returns (enforced by contract tests). |
| `HostTypeSchema::Resource(ResourceTypeKey)` | P | `src/host_api.rs` | Used by UDP/TCP/TLS/shared-dict. Runtime passing is integer handle ids. |
| `ResourceHandle(u64)` | P | `src/vm/resource/handle.rs` | `from_raw` / `raw`. Arena + slot + generation. Table-local. |
| `ResourceTable` | P | `src/vm/resource/table.rs` | Per VM execution scope. Default max 1024. |

`Value` is `Send + Sync` through `Arc` / `Mutex`. That is **not** a
permission to move request-scoped callables or resource ids onto Tokio
or another VM.

### Same-VM callable invocation (public, insufficient)

| Item | Vis | Path | Contract that blocks ngx.thread / timers |
| --- | --- | --- | --- |
| `Vm::resolve_exported_callable(name)` | P | `src/vm/mod.rs` | Reads a local slot after root init. Fails if not `Value::Callable`. |
| `Vm::owns_callable` | C `pub(super)` | `src/vm/mod.rs` | Pointer identity against `instance.owned_callables` (`Weak` / `Arc::ptr_eq`). Not an unforgeable Program-tagged handle. |
| `Vm::queue_callable` | P | `src/vm/mod.rs` | Rejects foreign callables; FIFO on this VM. |
| `Vm::drain_callable_queue` | P | `src/vm/mod.rs` | **"queued callables can only run after the root frame halts"**; Waiting/Yielded abort the drain. Sequential, not concurrent. |
| `Vm::start_callable` | P | `src/vm/mod.rs` | Requires empty `execution_frames` (`"host invocation requires a halted VM"`). May return Waiting/Yielded from `run_internal`. Same-VM only. |
| `Vm::invoke_callable` | P | `src/vm/mod.rs` | Wrapper: Halted result or error. Waiting/Yielded are errors. |
| `Vm::take_callable_result` | P | `src/vm/mod.rs` | Completion queue of **this** VM. |
| `ScriptCallback<Args, Ret>` | P | `src/vm/store.rs` | `call` / `start` need `&mut Store<T>` (the same VM). Registry `Arc<AtomicBool>` invalidates on Store drop. |
| `Store<T>` | P | `src/vm/store.rs` | Owns one `Vm`. Not a child VM factory. |
| `Invocation<'vm>` | P | `src/vm/invocation.rs` | **Borrows `&'vm mut Vm`**. "single active invocation on a VM". |
| `Vm::start_invocation` | P | `src/vm/invocation.rs` | **"The VM must be halted"**; second invocation rejected; **"bridge host operation is not quiescent"** rejected. Cancel is per this invocation, not a child light-thread. |

These APIs are first-class **same-VM, halted-root, single-invocation**
callbacks. They are not first-class **ngx.thread / ngx.timer**
callbacks.

### Async host / wake path (public, reusable for semaphore only)

| Item | Vis | Path | Notes |
| --- | --- | --- | --- |
| `HostFuture` | P | `src/vm/async_host/mod.rs` | `Pin<Box<dyn Future<Output = VmResult<HostFutureOutput>> + Send + 'static>>` |
| `HostFutureOutput::{Return, VmCompletion}` | P | same | `VmCompletion` is `FnOnce(&mut Vm) -> VmResult<T> + Send + 'static` and **must run on the originating VM**. |
| `CaptureAsyncHostContext` | P | same | Capture owned `'static` context **before** submit; cannot borrow VM across poll. |
| `Vm::submit_host_future` | P | same | Needs a configured `HostAsyncBridge`. |
| `HostAsyncBridge: Send` | P | `src/vm/host.rs` | `submit_op` / `poll_submitted_op` / `cancel_op`. |
| `Vm::set_async_bridge` | P | `src/vm/host.rs` | ngx installs `NgxAsyncBridge`. |
| `Vm::poll_waiting_host_op` | P | `src/vm/host.rs` | Worker-main resume path. |
| `VmStatus::{Halted, Yielded, Waiting(HostOpId)}` | P | `src/vm/mod.rs` | One waiting op per VM run. |
| host streams (`HostStreamDriver`, `submit_callable_stream`) | C | `src/vm/async_host/stream.rs` | Not embedder API. Still same-VM callback continuation. |

### Lifecycle that scheduler families would have to compose with

| Item | Vis | Path |
| --- | --- | --- |
| `Vm::new_shared(Arc<Program>)` | P | `src/vm/mod.rs` |
| `Vm::reset_for_reuse` / `is_reusable` / `poll_reset_for_reuse` | P | `src/vm/mod.rs` |
| `Vm::shutdown` | P | `src/vm/mod.rs` | Invalidates callback registries, cancels waiting ops, begins resource close, clears frames. |
| `Program` | P | `src/bytecode.rs` | Immutable; `Arc<Program>` is the shareable artifact. |
| `CapabilityProfile` | P | `src/vm/capability.rs` | VM-level capability profile; ngx uses its own `AllowedOps` thread-local scope (`src/host/context.rs`). |

There is **no** public:

- child VM constructor that shares a Program but isolates resource tables and request scope
- unforgeable callable / function handle distinct from `Value::Callable`
- cancellable frame/task handle that outlives the parent invocation
- owned `Send` value boundary that strips callables and request resources
- timer/thread supervisor, worker-thread pool ABI, or detached Program runner

## Six roadmap questions

### 1. `ngx::thread::{spawn,wait,kill}` — multi-frame needs and VM expressibility

OpenResty light threads run a function concurrently with the parent
request coroutine, share the request, and are waited/killed by handle.

What ngx-rustscript would need:

- A child execution that can `Waiting` on its own `HostOpId` while the
  parent is also live (possibly also `Waiting`).
- `wait` that suspends the parent via the existing generic HostFuture
  bridge until the child Halted/errors/is killed.
- `kill` that cancels the child's waiting host op and does not resume
  it on a finalized request.

What the pin actually provides:

- One interpreter `Instance` per `Vm` (`src/vm/instance.rs`,
  crate-private). One `execution_frames` stack. One
  `waiting_host_op`. One `InvocationState`.
- Public host invocation **requires halted root**
  (`start_callable` / `start_invocation`).
- Queue drain is serial after halt.
- `Invocation<'vm>` mutably borrows the VM; it cannot be stored in a
  Tokio future or a second request identity.

**Expressible now:** no. Implementing spawn by `invoke_callable` on the
request VM would either block the parent or require inventing a second
scheduler beside `HostAsyncBridge`. That is rejected.

### 2. `ngx::timer::{at,every}` — detached Program / VM / capability / resource ownership

OpenResty `ngx.timer.at` / `every` run after the delay on a timer
context: no request cosocket, no `ngx.req.*` that needs the original
`r`, surviving the request that created them (for `at` after the
request ends, subject to worker lifetime).

What ngx-rustscript would need:

- A **detached** `Arc<Program>` runner that is not the request-owned
  `AccessExec`.
- A callable handle that remains valid after the creating request VM
  is reset, pooled, or dropped — or an explicit reject if the Program
  identity changed (reload).
- Capability attenuation: timer `AllowedOps` today is `EMPTY` for
  non-rewrite/access/content/log (`AllowedOps::for_phase`,
  `src/host/context.rs`). A timer VM must not inherit request
  `REQUEST_WRITE` / `CONTROL` / request `raw`.
- Resource ownership: timer must not receive request UDP/TCP handles
  or `ngx.ctx` maps that were request-owned.

What the pin actually provides:

- `ScriptCallback` dies with `Store` / VM (`CallbackRegistryOwner`
  drop stores `false`).
- `owns_callable` is per-VM pointer identity; dropping the request VM
  drops `owned_callables`.
- `HostFutureOutput::VmCompletion` requires `&mut Vm` of the
  **submitting** VM — unusable after that VM is gone.
- No public child VM.

**Expressible now:** no.

### 3. Panic / error / shutdown / reload / limits

Required mapping (contract, not implemented):

| Event | Required behavior |
| --- | --- |
| Child / timer script `VmError` | Log; parent `wait` sees a structured error value; request is not 500 unless the parent itself errors. |
| Panic in host future | Existing AccessExec catch_unwind path; child/timer must not unwind the worker. |
| Client abort | Request-owned children die with the request (see `on_abort`). Detached timers do **not** hold `r`. |
| Worker `exit_process` | Cancel all detached tasks and request children; drop Tokio runtime; close eventfd once. |
| Config reload | New `Arc<Program>` identity. Old callable handles must fail closed. In-flight detached work bound to the old Program is cancelled; it must not run against the new Program. |
| Limits | Per-worker cap on live threads + timers + semaphores + in-flight waits. Existing wait cap is 1024 (`DEFAULT_MAX_IN_FLIGHT`). A scheduler must not bypass it. |

Public API gap: no task handle, no Program-generation on callables,
no supervisor distinct from the request `AccessExec` identity
registry (`src/request_state.rs`).

**Expressible now:** no for callable families.

### 4. `ngx::on_abort` — single callable, timing, cleanup safety

OpenResty `ngx.on_abort` registers one callback for premature client
abort. ngx-rustscript already detects abort (`ngx_http_test_reading`
wrapper) and finalizes 499, then request-pool cleanup invalidates the
identity and drops the host future.

A script callback would need to run **before** `r` is finalized, on
the worker main thread, with a tightly attenuated scope (log + maybe
shared-dict; no output, no cosocket on the dying request, no second
`on_abort`). It must be exactly-once. It must not be invoked on
graceful worker shutdown unless that is specified as abort.

Public `ScriptCallback` plus request VM re-entry during cleanup is
unsafe: cleanup already drops the VM. Invoking a callable after
`AccessExec` teardown would use a dead VM or a live `r` after
finalize.

**Expressible now:** no.

### 5. Semaphore — reuse of generic `HostFuture`

OpenResty `ngx.semaphore` is a worker-local counting semaphore:
`new(n)`, `wait(timeout)`, `post(n)`, `count()`.

This **does not need a script callable**. It matches existing hosts:

- `new` → insert a `Resource` into the current VM table, return
  `ResourceHandle` as `i64` (same pattern as `ngx::socket::udp`).
- `wait` → `async` `pd-host-function` that captures the handle id and
  an owned Tokio/async wait, submits `HostFuture`, parent VM
  `Waiting`, eventfd resume. Future holds **no** request/VM pointer.
- `post` / `count` → sync, lookup handle in **this** VM table.

`HostFuture: Send + 'static` and `NgxAsyncBridge` already poll that
shape. `CaptureAsyncHostContext` already forbids borrowing the VM
across `.await`.

Why this does **not** unlock callable APIs:

- Semaphore completion is `HostFutureOutput::Return`, not a script
  function call.
- It does not create child frames, detached VMs, or callable handles.
- A wait still occupies the **same** VM's single `Waiting` slot, like
  `ngx::sleep`. It cannot run a callback while waiting.

Decision: **feasible now**, still **not registered**. Registering it
requires a dedicated resource key, worker-local storage that survives
the creating VM (OpenResty semaphores are process/worker objects, not
request objects), reload policy, and a budget. Those are follow-up
work, not this gate.

### 6. `ngx::run_worker_thread` — module/function ABI, owned args/results, configured pool

OpenResty runs a named Lua module function on a `thread_pool` with
copied arguments and copied results.

Needs:

- Configured pool (nginx `thread_pool` or a dedicated Tokio pool).
  Current Tokio runtime is one worker thread named
  `ngx-rustscript-async` for **I/O**, not a user thread pool
  (`src/runtime.rs`).
- A function identity that is a module path + export, not a
  request-VM `Value::Callable`.
- Args/results that are owned and `Send` without callables or
  request resource ids (serialize or deep-copy the scalar/container
  subset).
- No request `raw` / ctx / VM on the pool thread.

`Arc<Program>` is shareable. `Vm` is request/worker-main policy even
if some internals happen to be `Send`. `Value::Callable` is not a
pool ABI. Resource `i64`s are forgeable across tables.

**Expressible now:** no.

## Required invariants (fail-closed until all hold)

1. **Worker-main VM.** Request VMs never move to Tokio. Detached
   runners, if ever added, are not the request VM.
2. **Waker purity.** Completions enqueue `RequestIdentity` (or a new
   detached identity type that is still not a pointer) then write
   eventfd. No `r`, no `Vm`, no pool, no `ngx_post_event`.
3. **Unforgeable callables.** A handle must not be a raw
   `prototype_id` or a cloned `Value::Callable` that another VM can
   be tricked into running. Identity includes Program generation.
4. **Child/detached isolation.** Child resource tables do not contain
   parent request handles. Parent request handles cannot be passed as
   `i64` into detached work.
5. **Single Waiting per VM is not a thread scheduler.** Concurrent
   light threads need independent wait slots or independent VMs.
6. **Capability attenuation.** Timer / abort / worker-thread scopes
   start from `AllowedOps::EMPTY` plus an explicit allowlist. They
   never inherit the creating request's `AllowedOps::for_phase`.
7. **Reload.** Old Program callables fail closed. In-flight detached
   work is cancelled, not rebound.
8. **Exactly-once.** Spawned work completes, errors, or cancels once.
   `on_abort` runs at most once. Timer `every` cancellations do not
   deliver a trailing tick after cancel acknowledgement.
9. **Panic isolation.** A child/timer panic logs and fails that task;
   it does not unwind the nginx worker or the parent request VM.
10. **Log phase.** Scheduler imports stay rejected in Log. Log still
    cannot wait.

## Lifetime / state diagrams (text)

### Request-owned async host (existing; sleep/cosocket)

```
config: Arc<Program> ──────────────────────────────────────────┐
                                                               │
request begin  AccessExec { vm, identity(gen,id), bridge }     │
     │  run()                                                  │
     │  Waiting(op) → reserve cap → register identity+waker    │
     │              → poll once → NGX_AGAIN                    │
     │                                                         │
Tokio future (Send, no r/VM) ── wake ── eventfd                │
     │                                                         │
worker event  harvest identity → gen match → poll_op           │
     │  Ready → resume() → Halted → NGX_OK / finalize          │
     │  cancel/499/shutdown → drop future, release cap once    │
request end  drop VM (not pooled if host-bound)                │
```

### Illegal timer-with-callback on current API

```
request VM  ScriptCallback.clone() ── stored in Tokio sleep ── X
            │                         holds Value::Callable
            │                         later invoke_callable
            ▼
request ends, VM shutdown/reset
            │
timer fires  callback.call(&mut Store) ── registry invalidated
            or worse: &mut Vm that is pooled for another request
            or HostFutureOutput::VmCompletion(&mut dead_vm)
```

### Required detached timer (future, after upstream API)

```
config Program P0 ── handle H(program_gen=P0, fn=f)
request  ngx::timer::at(delay, H, owned_args)
            │
            ├─ request VM continues / ends   (does not own timer VM)
            │
worker timer table  identity T(gen,id)  (not a request identity)
            │  future waits delay (Send, no r)
            │  fire → lease detached Vm::new_shared(P0) if gen live
            │       → attenuated AllowedOps
            │       → invoke H on THAT vm only
            │       → drop detached VM
reload  P0 retired → T cancelled, H fails closed
```

### Required light thread (future)

```
parent VM (request)  spawn(H, args) → TaskHandle TH
     │  may Waiting on its own op
child VM or child frame (independent wait slot)
     │  Waiting on child op
parent wait(TH)  HostFuture until child Halted/error/kill
kill(TH)  cancel child op; no resume after request death
request end  all TH cancelled unless explicitly detached
             (ngx.thread is request-scoped; timers are not)
```

## Cancellation and exactly-once

### Request-owned host future (existing)

| Path | Future removed | Cap released | VM resumed | HTTP |
| --- | --- | --- | --- | --- |
| Ready success | yes | yes | yes | continue |
| Ready error | yes | yes | error path | 500 unless control |
| Client close | yes | yes | no | 499 |
| Worker shutdown | yes | yes | no | worker exit |
| Stale generation wake | ignore | already released | no | n/a |
| Duplicate op id | rejected, no replace | n/a | n/a | config/runtime error |

### Callable families (required; not implementable now)

| Event | `thread.spawn` child | `timer.at` | `timer.every` | `on_abort` |
| --- | --- | --- | --- | --- |
| Parent Halted | cancel child | timer independent | independent | unregister without run |
| Parent 499 | cancel child; abort cb once if registered | independent | independent | run once, then 499 |
| `thread.kill` | cancel once; wait sees killed | n/a | n/a | n/a |
| Timer cancel | n/a | no fire | no further ticks; in-flight tick either runs to completion **or** is cancelled, never both | n/a |
| Reload | cancel | cancel | cancel | unregister |
| Double wake | ignore by generation | ignore | ignore | ignore |
| Panic | fail child once | fail timer once | fail that tick; series continues unless policy says stop | fail abort cb; still finalize 499 |

Exactly-once rule: a given task identity delivers at most one terminal
status `{completed, failed, cancelled, killed}`. Abort callback
delivers at most one invocation.

## Budgets

| Budget | Current | Scheduler implication |
| --- | --- | --- |
| In-flight HostFuture waits | 1024 / worker | Threads waiting, timer fires that invoke async hosts, and semaphore waits **share** this cap unless a new named cap is designed. Do not silent-bypass. |
| Idle VM pool | 4 import-free VMs | Detached timer VMs are not pool VMs. Do not steal pool slots. |
| Resource table | pd-vm default 1024 | Child VM gets its own table. Do not insert parent sockets into it. |
| `ngx.ctx` | keys/depth/bytes caps | Callables rejected. Detached work gets no request ctx. |
| Script call depth | `DEFAULT_MAX_SCRIPT_CALL_DEPTH` 1024 | Child VM has its own depth; spawn is not a recursive same-stack call. |
| Codec / body / header caps | see compatibility matrix | Unchanged. |
| Proposed live `thread` count | not set | Need a hard cap before enabling spawn (suggested 64/request, 256/worker — **proposal only**, not implemented). |
| Proposed live timers | not set | Need a hard cap (suggested 256/worker). |
| Proposed semaphores | not set | Worker-local objects need a cap and shm/heap policy. |

## Worker generation, reload, Program ownership

- Config compile produces `Arc<Program>` stored on `LocationConf`
  (`src/config.rs`). Nested locations inherit or override slots.
- Reload builds new Programs. Old location confs are dropped with the
  old cycle.
- `Vm::new_shared(Arc<Program>)` shares bytecode. Callable **runtime
  instances** are per VM (`owned_callables`).
- Request identities are `(generation, request_id)` (`src/notify.rs`).
  Stale wakes are ignored.
- A future callable handle must carry `program_generation` (or
  `Arc::ptr_eq` on the Program plus a retired-set). Reload retires
  the old Arc.
- Worker `init_process` starts Tokio + eventfd; `exit_process`
  invalidates senders, cancels futures, closes fd once
  (`src/runtime.rs`). Detached scheduler state would have to hook
  **that** shutdown, not a new one.

## Capability attenuation

`AllowedOps` is a worker-thread stack (`src/host/context.rs`):

| Phase | Ops |
| --- | --- |
| rewrite / access / content | read/write request, log, ctx, response read/write, control |
| log | read request, log, ctx, response **read** |
| other catalog phases including `timer` | `EMPTY` |

Today `NgxPhase::Timer` exists in catalog metadata for OpenResty
allowlists (`SLEEP_PHASES` includes it) but **there is no timer
directive** and `for_phase(Timer)` is empty. Sleep in a future timer
VM would still need an explicit scope; it must not reuse Access ops.

Required attenuation table (future):

| Runner | Request FFI | Cosocket | ctx | Response write | Control | Shared dict | Log | Sleep |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| access/content parent | yes | yes | yes | phase rules | yes | yes | yes | yes |
| `ngx.thread` child | same request, **same** phase ops | yes, but handles must be child-table copies or explicitly moved | shared map needs a defined alias policy | yes | yes | yes | yes | yes |
| `ngx.timer.at/every` | **no** `r` | no request sockets; optional new timer-owned sockets later | no request ctx | no | no | yes (worker-level) | yes | yes |
| `ngx.on_abort` | dying `r` only if still live; prefer no sockets | no | read-only or none | no | no | yes | yes | no |
| `run_worker_thread` | no | no | no | no | no | no (or explicit copy-in) | cycle log only | no nginx sleep |

Until a runner can install a scope that is **narrower** than the
creator and cannot be widened by the script, callable families stay
blocked.

## Request resource escape prevention

Escape means a detached or pool context using:

- `*mut ngx_http_request_t`
- request pool pointers
- `RequestLocalCtx` of the creator
- UDP/TCP/TLS `ResourceHandle` minted in the request VM
- `Value::Callable` bound to the request VM
- `HostFutureOutput::VmCompletion` closing over `&mut` request `Vm`

Prevention:

1. Detached args are a **deep-owned Send subset**: null/int/float/bool/
   string/bytes/array/map of those. Reject callable and reject
   resource ids (do not treat `i64` as a handle).
2. Child request threads (ngx.thread) that **are** allowed sockets
   receive handles minted in the **child** table, or a move API that
   invalidates the parent handle (exactly-once move).
3. Wakers stay identity-only.
4. `ngx::ctx` already rejects callables; keep that.

Current `ResourceHandle::from_raw` will accept any well-shaped `u64`.
That is forgeable across VMs. Callable families cannot ship on that
ABI.

## Panic / error mapping

| Source | Parent request | Detached timer | Worker pool thread |
| --- | --- | --- | --- |
| Script `VmError` | existing 500 / log-phase log-only | log; drop timer VM | return error value to caller |
| HostError | existing | log; drop | return error |
| `Waiting` in log | `LOG_CANNOT_WAIT` | n/a | n/a |
| Panic in script/host on worker main | catch_unwind in handlers; 500 | catch_unwind; log; drop | catch_unwind; error result |
| Panic in Tokio future | future error → resume error path | future error → no request | future error → caller wait |
| Control `ngx::exit` in child | defined later; default: child stop, parent continues unless wait observes it | forbidden by attenuation | forbidden |

Do not call `ngx_http_finalize_request` from a timer, pool thread, or
host future.

## Proposed minimal upstream `pd-vm` public API

These signatures do **not** exist on the pin. They are the smallest
surface that would make callable families designable without using
crate-private interpreter state.

```rust
/// Unforgeable callable/function handle. Not `Value::Callable`.
/// Identity includes Program generation. Cannot be minted from an integer.
pub struct ScriptFunctionHandle { /* opaque */ }

impl ScriptFunctionHandle {
    pub fn program_id(&self) -> ProgramId;
}

/// Owned Send subset for cross-VM / pool boundaries.
/// Implementation may be a deep copy or a documented serializer.
/// Must reject callables and resource handles.
pub struct DetachedValue { /* opaque, Send + 'static */ }

impl DetachedValue {
    pub fn from_value_strict(value: &Value) -> Result<Self, VmError>;
    pub fn into_value(self) -> Value; // never yields Callable or Resource
}

/// Child or detached VM. Independent ResourceTable and wait slot.
/// Shares bytecode via Arc<Program> only when ProgramId still live.
pub struct ChildVm { /* opaque; ! shared with request FFI */ }

pub struct ChildVmOptions {
    pub program: Arc<Program>,
    pub capability: CapabilityProfile, // or embedder-installed host subset
}

impl ChildVm {
    pub fn spawn(options: ChildVmOptions) -> Result<Self, VmError>;
    pub fn invoke(
        &mut self,
        function: ScriptFunctionHandle,
        args: DetachedValue,
    ) -> Result<TaskHandle, VmError>;
}

/// Cancellable frame/task handle. Generation-safe. Send.
/// Completes at most once.
pub struct TaskHandle { /* opaque, Send + 'static */ }

impl TaskHandle {
    pub fn id(&self) -> TaskId;
    pub fn cancel(&self, reason: OperationCancelReason) -> Result<(), VmError>;
}

/// Drive a child without borrowing the parent Vm.
pub enum TaskPoll {
    Pending,
    Halted(DetachedValue),
    Failed(VmError),
    Cancelled(OperationCancelReason),
}

impl ChildVm {
    pub fn poll_task(&mut self, task: &TaskHandle, cx: &mut Context<'_>) -> Result<TaskPoll, VmError>;
}

/// Resolve an exported entrypoint to an unforgeable handle after Program load,
/// without requiring a halted request VM.
impl Program {
    pub fn function_handle(&self, export_name: &str) -> Result<ScriptFunctionHandle, VmError>;
}
```

Constraints the embedder will still impose:

- `ChildVm` stays on the worker main thread **or** is a documented
  pool VM that never receives NGINX pointers.
- `TaskHandle` wake uses the existing `HostAsyncBridge` / eventfd
  identity model (extend identity space; do not add a second
  cancellation protocol).
- `HostFutureOutput::VmCompletion` must not be used to invoke
  callables on a different VM than the one that submitted the future.
- `Value::Callable` remains in-VM only.

Until this (or an equivalent reviewed public API) ships in a pin bump,
ngx-rustscript will not add `src/scheduler.rs` or catalog entries.

## Phased follow-up order

1. **This gate (done by the contract tests).** Fail-closed. Host count
   191. No scheduler module. Design published.
2. **Upstream `pd-vm`.** Land the minimal public API above. Pin bump
   in ngx-rustscript with an audit against this file.
3. **Semaphore (optional, independent).** Worker-local resource +
   `HostFuture` wait. Still no callables. New design addendum for
   worker lifetime vs request VM table (OpenResty semaphores outlive
   the creating request).
4. **`ngx.on_abort`.** Single handle, exactly-once, attenuated, runs
   on worker main before 499 finalize, cannot wait.
5. **`ngx.timer.at` then `every`.** Detached `ChildVm`, attenuated
   ops, Program-gen handles, cancel/reload tables.
6. **`ngx.thread.{spawn,wait,kill}`.** Independent wait slot or child
   VM **bound to the request identity**. Kill/cancel on 499 and
   shutdown.
7. **`ngx.run_worker_thread` last.** Configured pool + `DetachedValue`
   ABI. No request resources.

A later phase must not skip 2. Semaphore (3) may precede 2 because it
does not need callables; it still needs its own lifetime addendum and
must not be smuggled in as "scheduler".

## Observable fail-closed boundary (this crate)

| Check | Expected |
| --- | --- |
| `ngx_host_manifest().len()` | 191 |
| catalog / runtime name lists | same 191, no scheduler names |
| `bind_ngx_hosts` | no scheduler functions |
| `src/scheduler.rs` | absent |
| `src/lib.rs` | no `mod scheduler` |
| `HostFamily` / `HostCapability` | no Scheduler variant |
| catalog `HostTypeSchema::Callable` | none |
| compile `ngx::thread::spawn` etc. in Access/Content/Log | `ConfigError::UnsupportedImport` (or the same fail-closed class) at **configuration** compile, not at request runtime |
| Log allowlist | synthetic log-allowlist probe `ngx::future::scheduler` (never a production import) is rejected; scheduler candidates stay rejected |

Revisit this file before adding any of those names to the manifest.
