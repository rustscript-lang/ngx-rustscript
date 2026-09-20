# ngx callback / scheduler execution contract

[Documentation](../index.md) · [API](../api/index.md) · [Compatibility](../ngx-api-compatibility.md)

Feasibility gate for the OpenResty-oriented scheduler families:

- `ngx::thread::{spawn,wait,kill}`
- `ngx::timer::{at,every}` and the timer counters `ngx::timer::{pending_count,running_count}` — **landed**, see below
- `ngx::on_abort`
- semaphore (`ngx::semaphore::{new,wait,post,count}`) — **landed**, see below
- `ngx::run_worker_thread`

This document is the lifecycle contract. The remaining callable scheduler
families stay absent: `ngx::thread::{spawn,wait,kill}`, `ngx::on_abort`, and
`ngx::run_worker_thread` are still gated. Two worker-owned families are
landed: the semaphore family (`ngx::semaphore::{new,wait,post,count}`), which
needs no script callable, and the timer family
(`ngx::timer::{at,every,pending_count,running_count}`), whose registration
hosts consume one owned script callback. Together they take the production
surface from the **191** callable baseline to **199** hosts; their lifetime
addenda are in
[Semaphore](#5-semaphore-landed-worker-owned-counting-semaphore) and
[Timer](#7-timer-landed-worker-owned-fake-request-timers).

Pinned `pd-vm` evidence in this file is taken from:

| Pin | Value |
| --- | --- |
| Crate metadata | `Cargo.toml` `rustscript-rev` / `vm` git `rev` |
| `Cargo.lock` | `pd-vm` `source = "git+https://github.com/rustscript-lang/rustscript?rev=87d8c5d5cb42b3381aecadb383c0b7bf34ff204f#87d8c5d5cb42b3381aecadb383c0b7bf34ff204f"` |
| Local Cargo git checkout | `$CARGO_HOME/git/checkouts/rustscript-ab116a0775a56744/87d8c5d` |

Public versus crate-private is taken from that checkout's `src/lib.rs`
re-exports and item visibility. Do not treat crate-private helpers as
embedder API.

## Decision

**Overall public API is insufficient for the remaining callable
families. Keep those fail-closed; the semaphore and timer families are
landed.**

| Family | Decision | Why |
| --- | --- | --- |
| `ngx::thread::{spawn,wait,kill}` | **blocked** | Needs concurrent child frames / independent invocation while the parent VM is live and possibly `Waiting`. Public `Vm::start_callable` / `invoke_callable` / `start_invocation` require a halted VM and a single active invocation. |
| `ngx::timer::{at,every,pending_count,running_count}` | **landed** (199) | The pinned pd-vm exposes `register_owned_timer` / `installed_timer_counts` / `TIMER_CALLBACK_ARG` and a generic owned dispatch that spawns an isolated callback VM. ngx supplies checked seconds, its own exact names/schemas, a worker-owned fake-request registry, and the Timer-phase capability policy. |
| panic / error / shutdown / reload / limits for those families | **landed / covered** for the timer family; **blocked** for the remaining callable families | The landed timer lifecycle ([Timer](#7-timer-landed-worker-owned-fake-request-timers)) covers callback errors (`every` stays eligible after a logged failure), panic isolation, worker `exit_process` (pending callbacks run once with `premature=true`, live callbacks are cancelled, further `every` rounds stop), reload (new workers start with zero timers), and the `TimerConfig` / fake-request caps; validated by `tests/ngx_timer.rs` and `tests/timer_nginx.rs` together with `tests/ngx_scheduler_contract.rs`. `ngx::thread::*`, `ngx::on_abort`, and `ngx::run_worker_thread` still have no public cancellable child-task handle, no generation-safe detached identity, and no exactly-once child completion protocol that composes with the request eventfd bridge. |
| `ngx::on_abort` | **blocked** | Needs a single callable, request-abort timing, and cleanup that cannot re-enter the dying request VM with live FFI pointers. |
| semaphore | **landed** (199) | Worker-owned counter + FIFO waiter queue in a `thread_local` registry, reached through a VM-local `resource<ngx.semaphore>` handle. Waits reuse the generic `HostFuture` + `AccessExec` reservation path and invoke no script callable. That does **not** unlock thread/`on_abort`/`run_worker_thread`. |
| `ngx::run_worker_thread` | **blocked** | Needs a configured pool, module/function ABI, and an owned `Send` value boundary. `Value::Callable` / request resources must not cross the pool. |

The semaphore landing passed the request-resource escape and reload
budgets: the worker registry is `thread_local` (one registry per worker),
ids are never reused, and the current executing VM holds a typed handle entry
in its **own** table. Sync-only programs use a pooled VM; programs that use
async `wait` use request-owned `AccessExec`. Waits capture owned state plus an
owned timeout, and worker `exit_process` closes live semaphores before the
Tokio runtime is dropped.

The timer landing passed them too: registrations live in a
`thread_local` worker registry (never reused, tagged fake request ids,
per-round generations), the callback VM is created by the owned
dispatch with an empty resource table and no request scope, the only
Tokio work is the deadline sleep, and worker `exit_process` runs or
cancels every callback before the Tokio runtime is dropped. Every row
above that is not marked landed is still fail-closed.

Contract tests: `tests/ngx_scheduler_contract.rs` (fail-closed gate),
`tests/ngx_semaphore.rs` (landed semaphore family),
`tests/ngx_timer.rs` and `tests/timer_nginx.rs` (landed timer family).

## Current execution architecture

ngx-rustscript runs one RustScript program per configured phase slot
(`access_by_rss_file`, `content_by_rss_file`, `log_by_rss_file`).

```
nginx worker main thread
  AccessHandler / ContentHandler / LogHandler
    LocationConf.program : Arc<Program>          // config-time, shared
    no request-state imports (pure-sync, including semaphore new/post/count)
        -> worker VM pool
                                             // Vm::new_shared, reset_for_reuse
    async / ctx / cosocket / semaphore wait / mutation / control
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
7. Cosocket / shared-dict / TLS / semaphore objects are `ResourceHandle`
   values in the **current** VM execution-scope table. Script sees them
   as `i64` (`src/socket/udp.rs` `parse_handle`, TCP/TLS equivalents;
   `src/semaphore.rs` `resolve_handle`). Handles are generation-tagged
   integers, not transferable identities: resolving one from another
   VM's table fails closed.

Root execution is `Vm::run` / `Vm::resume`. The module does not use
`ScriptCallback`, `Store`, `Invocation`, or `Vm::invoke_callable` for
production request handling.

## Evidence matrix (pinned `pd-vm` 87d8c5d5cb42b3381aecadb383c0b7bf34ff204f)

Legend: **P** = public (`src/lib.rs` re-export or `pub` item on a public
type). **C** = crate-private / `pub(super)` / `pub(crate)`.

### Values, callables, resources

| Item | Vis | Path | Notes |
| --- | --- | --- | --- |
| `Value` | P | `src/bytecode.rs` | `Null/Int/Float/Bool/String/Bytes/Array/Map/Callable`. **No resource variant.** |
| `SharedCallable = Arc<CallableValue>` | P | `src/bytecode.rs` | Clone shares the Arc. |
| `CallableValue { prototype_id, kind, env }` | P | `src/bytecode.rs` | No Program identity field. Env is `Option<Arc<CallableEnvironment>>` with `pub(crate) cells: Mutex<Vec<SharedCaptureCell>>`. |
| `ExportedCallable { name, local_slot }` | P | `src/bytecode.rs` | Metadata only; not a handle. |
| `HostTypeSchema::Callable { params, result }` | P | `src/host_api.rs` | Catalog type. The ngx catalog declares exactly two Callable params (the `TakeOwned` `callback` of `ngx::timer::{at,every}`) and no Callable returns (enforced by contract tests). |
| `HostTypeSchema::Resource(ResourceTypeKey)` | P | `src/host_api.rs` | Used by UDP/TCP/TLS/shared-dict and by the semaphore handle. Runtime passing is integer handle ids. |
| `ResourceHandle(u64)` | P | `src/vm/resource/handle.rs` | `from_raw` / `raw`. Arena + slot + generation. Table-local. |
| `ResourceTable` | P | `src/vm/resource/table.rs` | Per VM execution scope. Default max 1024. |

`Value` is `Send + Sync` through `Arc` / `Mutex`. That is **not** a
permission to move request-scoped callables or resource ids onto Tokio
or another VM.

### Same-VM callable invocation (public, insufficient)

| Item | Vis | Path | Contract that keeps `ngx.thread` / `on_abort` blocked |
| --- | --- | --- | --- |
| `Vm::resolve_exported_callable(name)` | P | `src/vm/mod.rs` | Reads a local slot after root init. Fails if not `Value::Callable`. |
| `Vm::owns_callable` | C `pub(super)` | `src/vm/mod.rs` | Pointer identity against `instance.owned_callables` (`Weak` / `Arc::ptr_eq`). Not an unforgeable Program-tagged handle. |
| `Vm::queue_callable` | P | `src/vm/mod.rs` | Rejects foreign callables; FIFO on this VM. |
| `Vm::drain_callable_queue` | P | `src/vm/mod.rs` | **"queued callables can only run after the root frame halts"**; Waiting/Yielded abort the drain. Sequential, not concurrent. |
| `Vm::start_callable` | P | `src/vm/mod.rs` | Requires empty `execution_frames` (`"host invocation requires a halted VM"`). May return Waiting/Yielded from `run_internal`. Same-VM only. |
| `Vm::invoke_callable` | P | `src/vm/mod.rs` | Wrapper: Halted result or error. Waiting/Yielded are errors. |
| `Vm::take_callable_result` | P | `src/vm/mod.rs` | Completion queue of **this** VM. |
| `ScriptCallback<Args, Ret>` | P | `src/vm/store.rs` | `call` / `start` need `&mut Store<T>` (the same VM). Registry `Arc<AtomicBool>` invalidates on Store drop. Not the timer mechanism. |
| `Store<T>` | P | `src/vm/store.rs` | Owns one `Vm`. Not a child VM factory. |
| `Invocation<'vm>` | P | `src/vm/invocation.rs` | **Borrows `&'vm mut Vm`**. "single active invocation on a VM". |
| `Vm::start_invocation` | P | `src/vm/invocation.rs` | **"The VM must be halted"**; second invocation rejected; **"bridge host operation is not quiescent"** rejected. Cancel is per this invocation, not a child light-thread. |

These APIs are first-class **same-VM, halted-root, single-invocation**
callbacks. They are not first-class **ngx.thread / `on_abort`**
callbacks, and the landed timer family does **not** use them: its
callback VM is created by the owned-dispatch seam below.

### Async host / wake path (public; reused by the landed semaphore and timer families)

| Item | Vis | Path | Notes |
| --- | --- | --- | --- |
| `HostFuture` | P | `src/vm/async_host/mod.rs` | `Pin<Box<dyn Future<Output = VmResult<HostFutureOutput>> + Send + 'static>>` |
| `HostFutureOutput::{Return, VmCompletion}` | P | same | `VmCompletion` is `FnOnce(&mut Vm) -> VmResult<T> + Send + 'static` and **must run on the originating VM**. |
| `CaptureAsyncHostContext` | P | same | Capture owned `'static` context **before** submit; cannot borrow VM across poll. |
| `Vm::submit_host_future` | P | same | Needs a configured `HostAsyncBridge`. |
| `HostAsyncBridge: Send` | P | `src/vm/host.rs` | `submit_op` / `poll_submitted_op` / `cancel_op`; typed cancellation adds `request_cancel_op` / `poll_cancel_op`. |
| `Vm::set_async_bridge` | P | `src/vm/host.rs` | ngx installs `NgxAsyncBridge`. |
| `Vm::poll_waiting_host_op` | P | `src/vm/host.rs` | Worker-main resume path. |
| `VmStatus::{Halted, Yielded, Waiting(HostOpId)}` | P | `src/vm/mod.rs` | One waiting op per VM run. |
| host streams (`HostStreamDriver`, `submit_callable_stream`) | C | `src/vm/async_host/stream.rs` | Not embedder API. Still same-VM callback continuation. |

### Owned-dispatch seam (public; the landed timer path)

The one public child-VM seam in the pin. It is only reachable from an
owned host function bound through a `HostFunctionRegistry`, and it is
what `ngx::timer::{at,every}` use.

| Item | Vis | Path | Notes |
| --- | --- | --- | --- |
| `OwnedHostCall::spawn_owned_callable_vm(registry, callable, configure)` | P | `src/vm/host.rs` | Fresh child VM that owns the callable and its captured callable graph: new execution instance, stack, resource table, waiting slot, and module-state store. Reuses the same `Arc<Program>` and standard-surface composition; binds hosts from `registry`; copies no frames, locals, resources, waiting op, callback queue, or request-local host state. Rejects foreign callables and resource-bearing captures, including nested ones. |
| `register_owned_timer(call, registry, TIMER_CALLBACK_ARG, delay, interval)` | P | `src/builtins/runtime/timer.rs` | Standard-host adapter seam: validates the owned callback's type and program provenance, spawns the isolated VM, hands a `TimerRegistration` to the installed backend, and restores the callback argument on every failure path (error *and* panic). |
| `TimerBackend` / `TimerConfig` / `TimerRegistration` / `OwnedTimerCallback` | P | same | The backend owns admission and accounting; the callback VM is driven on the embedding's VM thread (`start_reporting` / `poll_reporting` / `cancel`), one round at a time. |
| `installed_timer_counts` / `TIMER_CALLBACK_ARG` | P | same | Adapter-facing count surface and callback argument index. |

`Value` still has no resource variant, and this seam does not make
`Value::Callable` portable: only an owned host dispatch can create the
child VM, and only for a callable the calling VM owns.

### Lifecycle that the remaining scheduler families would have to compose with

| Item | Vis | Path |
| --- | --- | --- |
| `Vm::new_shared(Arc<Program>)` | P | `src/vm/mod.rs` |
| `Vm::reset_for_reuse` / `is_reusable` / `poll_reset_for_reuse` | P | `src/vm/mod.rs` |
| `Vm::shutdown` | P | `src/vm/mod.rs` | Invalidates callback registries, cancels waiting ops, begins resource close, clears frames. |
| `Program` | P | `src/bytecode.rs` | Immutable; `Arc<Program>` is the shareable artifact. |
| `CapabilityProfile` | P | `src/vm/capability.rs` | VM-level capability profile; ngx uses its own `AllowedOps` thread-local scope (`src/host/context.rs`). |

There is **no** public:

- general child-VM / detached scheduler constructor: the owned-dispatch
  seam above only mints a child VM *inside* an owned host function, for
  a callable the calling VM owns, under the host registry's control
- unforgeable callable / function handle distinct from `Value::Callable`
- cancellable frame/task handle that outlives the parent invocation
- owned `Send` value boundary that strips callables and request resources
- thread supervisor, worker-thread pool ABI, or caller-driven detached
  Program runner (the landed timer family needs none of these: its
  worker-owned timer runtime is the only scheduler that exists)

## Roadmap questions

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

### 2. `ngx::timer::{at,every}` — landed detached timer registration

OpenResty `ngx.timer.at` / `every` run after the delay on a timer
context: no request cosocket, no `ngx.req.*` that needs the original
`r`, surviving the request that created them.

**Status: landed.** The pinned pd-vm ships the standard timer host module
plus the generic owned-value dispatch; ngx supplies the seconds-based
adapter, the worker-owned fake-request registry, the `Timer`-phase
policy, and the counts. The full contract is
[7. Timer](#7-timer-landed-worker-owned-fake-request-timers).

Landed mechanism:

- **Owned `TakeOwned` dispatch.** `ngx::timer::{at,every}` take their
  callback argument and hand it to `register_owned_timer`, which
  validates the argument's type and program provenance, spawns the
  callback VM, and restores the argument on every failure path
  (returned error *and* panic), so the guest sees it exactly once.
- **Fresh isolated callback VM.** `OwnedHostCall::spawn_owned_callable_vm`
  builds a brand-new execution instance, stack, resource table, waiting
  slot, and module-state store; it reuses only the immutable
  configuration (same `Arc<Program>`, same standard-surface composition)
  and binds the host functions from the registry. It never copies the
  creating VM's frames, stack, locals, resources, waiting operation,
  callback queue, or request-local host state.
- **Worker-owned backend.** The registration travels as
  `TimerRegistration { delay, interval, max_pending, max_running,
  callback }` to the embedding's `TimerBackend` (ngx: `NgxTimerBackend`);
  `OwnedTimerCallback` retains the callable and its private VM across
  `every` rounds (mutable capture cells persist) and serializes rounds.
  The generic module keeps no state and no counters: the backend is the
  sole admission authority.
- **Identity and wake.** Each registration owns one ordinary
  `RequestIdentity` in the worker `IdentityRegistry` (tagged fake
  request id, per-round generation rotation) and wakes through the
  existing worker eventfd. Only the deadline sleep runs on Tokio, and it
  holds just a `CompletionSender` and that identity.
- **Scope and resources.** Rounds run under `NgxPhase::Timer` with
  `AllowedOps::LOG`; the callback VM has no request `r`, no request
  `ctx`, and no request resource table, so request handles cannot
  resolve in it. The creating request is detached: request cleanup owns
  no timer state.

Historical rationale (rejected; kept for review, **not** the landed
mechanism). The pre-landing design assumed the callback had to run on
the request-owned VM and therefore proposed a detached `Arc<Program>`
runner plus a Program-generation-safe callable handle. That concern was
real for the *then-public* same-VM APIs: `ScriptCallback` is invalidated
when its `Store` drops (`CallbackRegistryOwner` stores `false`),
`Vm::owns_callable` is per-VM pointer identity, and
`HostFutureOutput::VmCompletion` needs `&mut Vm` of the submitting VM.
Cloning the request VM, parking a `Value::Callable` in a Tokio sleep, or
invoking a callback on a reset/pooled request VM after the request ended
stays **rejected**; the landed path never does any of it. The old
"timer `AllowedOps` is `EMPTY`" statement is also historical:
`AllowedOps::for_phase(NgxPhase::Timer)` is `LOG` today, and
`AllowedOps::EMPTY` only applies to catalog phases outside
rewrite/access/content/log.

### 3. Panic / error / shutdown / reload / limits

Landed timer behavior (`src/timer.rs`, `src/host/timer.rs`);
validated by `tests/ngx_timer.rs` and `tests/timer_nginx.rs`:

| Event | Landed timer behavior |
| --- | --- |
| Callback `VmError` | Reported once through the worker cycle log via the backend error sink; the callback VM is recovered for reuse. `at` retires after the failed round; `every` stays eligible. The creating request is not affected (it no longer exists). |
| Callback panic | pd-vm guards callback `start` / `poll` / `resume` with `catch_unwind` and converts the panic into a callback error routed to the same log sink; the worker never unwinds. ngx additionally bounds one round to 64 immediate resumptions and fails a non-progressing round closed. |
| Registration failure | Invalid seconds, a missing/foreign/non-callable callback, a pending-limit or registry-full rejection returns a `HostError` to the script and restores the callback argument; nothing is retained. |
| Client abort | Timers hold no `r`; a 499 or any other request end leaves every registration untouched. A parked semaphore wait still drops with its request (drop guard leaves the FIFO queue). |
| Worker `exit_process` | `WorkerRuntime::shutdown` runs, in order: socket pool shutdown, `semaphore::shutdown_current()` (closes live semaphores and wakes their waiters), `timer::shutdown_current()` (run pending callbacks once with `premature=true`, a pending `every` round is **not** rescheduled; cancel live callbacks, releasing their parked async step exactly once; release every callback VM), then stops acceptance and reclaims queued wakes, then drops the Tokio runtime; the eventfd closes once. |
| Config reload | Nothing is cancelled or migrated: the timer registry is worker-`thread_local`, old workers keep their registrations (their callback VMs hold their own `Arc<Program>`), and new workers start with zero timers. Identities and generations are worker-local, so no registration is reused across workers or programs. |
| Limits | `TimerConfig` ships `max_pending` 1024 / `max_running` 256; both travel in every `TimerRegistration` and the backend enforces them atomically with insertion (pending) and round start (running, a parked callback still counted). The worker fake-request registry itself is capped at 1024 (`MAX_LIVE_TIMER_REQUESTS`, the `IdentityRegistry` capacity), and fake request ids fail closed on exhaustion instead of wrapping. |

Still blocked for the remaining callable families
(`ngx::thread::*`, `ngx::on_abort`, `ngx::run_worker_thread`): no
public cancellable child-task handle, no generation-safe detached
identity, no exactly-once child completion protocol that composes with
the request eventfd bridge. Their required mapping (contract, not
implemented) is:

| Event | Required behavior |
| --- | --- |
| Child / thread script `VmError` | Log; parent `wait` sees a structured error value; request is not 500 unless the parent itself errors. |
| Panic in host future | Existing `catch_unwind` guards in the content handler, the log handler, and the eventfd read handler (`src/content.rs`, `src/log.rs`, `src/runtime.rs`); a child must not unwind the worker. |
| Request abort | Request-owned children die with the request (see `on_abort`). |
| Worker `exit_process` | Cancel request children; drop Tokio runtime; close eventfd once. |
| Config reload | New `Arc<Program>` identity. Old callable handles must fail closed. In-flight work bound to the old Program is cancelled, never rebound. |
| Limits | Live threads still have no worker cap; that is part of the gate. Live semaphores are capped at 1024 (`MAX_LIVE_SEMAPHORES`); in-flight waits at 1024 (`DEFAULT_MAX_IN_FLIGHT`). |

Public API gap (remaining families): no task handle, no
Program-generation on callables, no supervisor distinct from the
request `AccessExec` identity registry (`src/request_state.rs`).

**Expressible now: no for the remaining callable families.** The
semaphore and timer families are landed; see
[5](#5-semaphore-landed-worker-owned-counting-semaphore) and
[7](#7-timer-landed-worker-owned-fake-request-timers).

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

### 5. Semaphore — landed worker-owned counting semaphore

OpenResty `ngx.semaphore` is a worker-local counting semaphore:
`new(n)`, `wait(timeout)`, `post(n)`, `count()`.

This **does not need a script callable**. It matches existing hosts:

- `new` → insert a `Resource` into the current VM table, return
  `ResourceHandle` as `i64` (same pattern as `ngx::socket::udp`).
- `wait` → `async` `pd-host-function` that captures the worker state and
  an owned timeout (never the VM), submits `HostFuture`, parent VM
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

#### Landed contract (199 hosts)

Public callables: `ngx::semaphore::new(count)`,
`ngx::semaphore::wait(sem, timeout)`, `ngx::semaphore::post(sem, count)`,
`ngx::semaphore::count(sem)`. The bare `ngx::semaphore` marker is **not**
a callable. Resource type key `ngx.semaphore`. Access + content only.

Worker lifetime addendum (worker-owned state versus the current executing VM
table):

- The permit counter, the FIFO waiter queue, and the closed flag live in
  `SemaphoreState` inside a `thread_local` worker registry keyed by a
  never-reused id. `new` inserts a `SemaphoreHandle` resource entry into
  the **current** VM table and returns its `i64` handle.
- Waiters capture `Arc<SemaphoreState>` plus an owned timeout through
  `CaptureAsyncHostContext::capture_with_args` on the worker main thread.
  They never hold `r`, pool, `Vm`, `RequestLocalCtx`, or a foreign handle.
- Permits are consumed only at grant, inside the same critical section
  that observes the waiter at the FIFO front. `post` and `grant` wake at
  most `min(permits, queued)` front waiters with registered wakers; each
  grant cascades the next wake. Timeout, cancellation, and drop therefore
  cannot leak or double-release a permit.
- Cleanup is exactly-once and idempotent: the wait future owns a drop
  guard that removes its FIFO entry if it is still queued. Typed
  cancellation uses the existing bridge `request_cancel_op` /
  `poll_cancel_op` protocol; there is no new cancellation protocol, no
  second eventfd, and no scheduler callback engine.
- Closing a semaphore drains its waiter queue while taking and waking each
  registered waker. Closed wait futures return a typed closed error, and
  guard cleanup remains idempotent after the queue is drained.
- Each `new` creates one VM-local handle. Releasing that handle removes the
  worker entry; release is idempotent. This is a single-handle lifecycle
  without a shared handle count. An in-flight wait keeps its own state alive,
  so it can never observe a recycled id.
- Sync-only `new`/`post`/`count` programs stay on the pooled VM path. An
  async `wait` program selects request-owned `AccessExec` through its
  `HostSyncKind::Async` manifest entry. VM reset or drop closes every
  VM-local semaphore handle, so reuse leaves no worker registry entry.
- Worker `exit_process` (`WorkerRuntime::shutdown`) closes every live
  semaphore, wakes every waiter, empties the registry, and only then drops
  the Tokio runtime. Ids are never reused across a worker restart.
- Configuration reload does not touch semaphores: they are worker objects
  with no `Program` identity, and a new `Arc<Program>` cannot rebind a
  live handle. Handles die with their VM table.
Validation, phases, the negative-depth `count` mapping, budgets, and the
full event table are in [docs/api/semaphore.md](../api/semaphore.md) and
[Out of scope](../ngx-api-compatibility.md#out-of-scope).

Decision: **landed as the first worker-owned family.** The remaining
callable families stay fail-closed.

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
pool ABI. A resource `i64` is only ever meaningful against the VM table
that minted it (the semaphore and cosocket families both validate that
way), and a pool thread has no such table, so request handles cannot be
passed in at all.

**Expressible now:** no.

### 7. Timer — landed worker-owned fake-request timers

OpenResty `ngx.timer.at` / `every` run a callback after a delay on a timer
context that survives the creating request. The pinned pd-vm provides the
callback machinery as a standard host module (owned dispatch plus the four
`timer::*` hosts); ngx supplies the worker-side adapter.

Landed contract (199 hosts):

Public callables: `ngx::timer::at(seconds, callback)`,
`ngx::timer::every(seconds, callback)`, `ngx::timer::pending_count()`,
`ngx::timer::running_count()`. The bare `ngx::timer` marker is **not** a
callable. Access + content only; Log fails configuration compile.

- `at`/`every` declare their callback as `fn(bool) -> unknown` with
  `HostParamPassing::TakeOwned` and register an exact owned entry; the
  callback return value is discarded. Seconds are checked into a `Duration`
  (negative, NaN, infinite, and zero `every` intervals are rejected).
- The family binds through `ngx_host_registry` (owned entries have no
  name-binding API): the registry carries every production host binding plus
  the standard composition, and is handed to `register_owned_timer` so the
  callback VM shares the same host surface. It is built from an empty registry
  so a program may import a standard HTTP host (`http::client::request`) next
  to the timer family: the timer entries resolve from the exact ngx catalog,
  and the standard import resolves from the staged standard composition.
- Worker lifetime: one `thread_local` registry holds a bounded pool of fake
  requests (1024). The registry is the same generic
  `request_state::IdentityRegistry` (exact identity lookup, hard capacity) the
  request path uses for `Waiting` identities, and its entries are the only
  owner of the callback VMs. Each accepted registration owns one **ordinary**
  `RequestIdentity` and one callback VM with its own `NgxAsyncBridge`. There is
  no second identity type, no second eventfd, and no second completion queue.
- Identity: the fake request id is drawn from a worker-local monotonic
  fake-request-id allocator; every id is tagged with `FAKE_REQUEST_ID_TAG`
  (the high bit real request ids never set — `crate::access::next_request_id`
  hands out ascending untagged ids from 1), so a timer identity can never
  equal a real request identity and both dispatch paths stay disjoint
  **without** a reserved generation. Fake ids are never reused, and the
  allocator fails closed on exhaustion instead of wrapping. Every `every`
  round rotates its identity: when a round reaches a terminal state the
  completed identity is retired from the registry, a fresh monotonically
  advancing generation (generations start nonzero and only advance) is
  allocated for the same fake request id, and only then is the next deadline
  scheduled. A stale or duplicate wake of a finished round (a coalesced
  duplicate of the deadline wake, or a leftover async-step wake of the round
  that just completed) therefore misses the exact identity of any live
  entry: it cannot start the pending round early, cannot break the interval,
  and cannot overlap rounds.
  The freshly minted identity is held by exactly one deadline task, so nothing
  else can wake it; that is what closes the duplicate-wake race the previous
  fixed-identity design had.
- Scheduling: the deadline task holds only a `CompletionSender` and that
  identity. It wakes through the worker eventfd; the eventfd handler routes
  every ready identity through the timer dispatch on the worker main thread
  before request identities are routed, and identities that are not a live
  registration are ignored. Script bytecode and NGINX FFI stay on the worker
  main thread; there is no second dispatch path.
- Callback phase: every round runs under `NgxPhase::Timer` with
  `AllowedOps::LOG`. Log, `ngx::sleep`/`sleep_ms`, outbound async I/O
  (cosocket), the shared dictionary, the semaphore family, and the timer
  family work; request reads/writes, `ngx.ctx`, response output, and control
  (`ngx::exit`, `ngx::redirect`) fail closed because there is no live request.
  The pd-vm standard I/O, HTTP, and SQLite policies are per-VM and are **not**
  inherited by a callback VM: a `http::client::request` inside a callback fails
  closed with the standard "HTTP host is not configured" error, exactly like an
  unconfigured location. (The callback VM shares the host surface, not another
  VM's policy grants.)
- `every` schedules the next deadline only after terminal completion, so a
  slow callback never overlaps itself and its mutable capture cells persist
  between rounds. A callback error is reported through the worker log, resets
  the callback VM, and (for `every`) leaves the series eligible.
- Counts: `pending_count` counts registrations waiting to begin (or waiting for
  a running slot); `running_count` counts live rounds, including callbacks
  parked on async work. Limits come from the installed `TimerConfig`
  (1024 pending / 256 running by default) and are enforced by the backend
  together with the 1024 fake-request pool bound.
- Shutdown: worker `exit_process` stops acceptance, runs pending callbacks once
  with `premature=true`, cancels live callbacks (releasing their async work),
  prevents further `every` rounds, and releases every callback VM. A reload
  never migrates timers: old workers keep theirs, new workers start empty.
- The creating request owns no timer state: request cleanup leaves the
  registration untouched.

Validation: `tests/ngx_scheduler_contract.rs` (catalog/manifest/phase/owned
argument), `tests/ngx_timer.rs` (unit lifecycle), `tests/timer_nginx.rs`
(real NGINX 1.31.4 lifecycle). `docs/api/timer.md` carries the per-host table.

Decision: **landed as the second worker-owned family.** The remaining
callable families stay fail-closed.

## Required invariants (fail-closed until all hold)

1. **Worker-main VM.** Request VMs never move to Tokio. Detached
   runners, if ever added, are not the request VM.
2. **Waker purity.** Completions enqueue `RequestIdentity` (or a new
   detached identity type that is still not a pointer) then write
   eventfd. No `r`, no `Vm`, no pool, no `ngx_post_event`.
3. **Unforgeable callables.** A handle must not be a raw
   `prototype_id` or a cloned `Value::Callable` that another VM can
   be tricked into running. The landed timer family satisfies this
   through the owned-dispatch provenance check (a foreign callable or
   a resource-bearing capture graph is rejected at registration); a
   handle carrying Program generation is still required by any future
   family that must resolve a callable outside the creating VM.
4. **Child/detached isolation.** Child resource tables do not contain
   parent request handles. Parent request handles cannot be passed as
   `i64` into detached work.
5. **Single Waiting per VM is not a thread scheduler.** Concurrent
   light threads need independent wait slots or independent VMs.
6. **Capability attenuation.** A detached scope starts from an
   explicit, narrower allowlist and never inherits the creating
   request's `AllowedOps::for_phase`. The landed timer scope is
   `NgxPhase::Timer` + `AllowedOps::LOG`; abort and worker-thread
   scopes would have to be at least as narrow.
7. **Reload.** No in-flight work is rebound to a new `Arc<Program>`.
   Landed timers are not cancelled at all: old workers keep their
   registrations against their own immutable program, and new workers
   start empty. A future family that resolves callables by name must
   fail closed on a stale-Program handle.
8. **Exactly-once.** Spawned work completes, errors, or cancels once.
   `on_abort` runs at most once. Timer `every` cancellations do not
   deliver a trailing tick after cancel acknowledgement.
9. **Panic isolation.** A child/timer panic logs and fails that task;
   it does not unwind the nginx worker or the parent request VM.
10. **Log phase.** Scheduler imports stay rejected in Log. Log still
    cannot wait.

The landed families already satisfy these where they apply. Semaphore:
request VMs stay on the worker main thread (1), wait futures are
identity-only and free of request pointers (2), nothing request-scoped
crosses the handle boundary (4), a wait uses the one `Waiting` slot of
its own VM (5), cleanup and completion are exactly-once (8), and the
family is rejected in Log (10). Timer: owned callbacks are provenance
validated and resource-free by construction (3), rounds run under the
narrowed `LOG` scope (6), timer state is never rebound on reload (7),
a failed round is reported once and isolated (9), and the family is
rejected in Log (10).

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

### Rejected pre-landing design: storing a request-VM callback across the delay (historical)

This is the design the current code must never use; it is kept as the
rejected alternative.

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

### Landed detached timer path (owned dispatch → timer-owned VM → eventfd → per-round identity rotation)

```
config Program P0 (immutable, shared by Arc) ────────────────┐
                                                             │
request VM (AccessExec)  ngx::timer::at(seconds, callback)   │
    │ TakeOwned transfer of the callback                     │
    │ register_owned_timer:                                  │
    │   type + program-provenance validation,                │
    │   rollback of the callback argument on any failure     │
    │   → fresh isolated callback VM (new instance, stack,   │
    │     resource table, waiting slot, module state)        │
    │     sharing only Arc<Program> P0 + the host registry   │
    │   → NgxTimerBackend::register: admission under one     │
    │     critical section (pending/running caps + 1024      │
    │     fake-request registry capacity)                    │
    │   → spawn deadline task                                │
    ├─ request continues / ends  (owns no timer state)       │
    ▼                                                        │
worker timer registry (thread_local; ≤1024 fake requests)    │
    TimerEntry { ordinary RequestIdentity(gen, fake|TAG),    │
                 callback VM, interval?, max_running, ... }  │
    Tokio deadline task: sleep(delay)                        │
      → sender.wake_identity(identity)                       │
      (holds the sender + identity; no VM/r/pool/pointer)    │
    ▼                                                        │
worker eventfd → timer::on_worker_event(ready) first, then   │
    request identities; a wake matching no live registration │
    (request id, stale/duplicate retired gen) is ignored     │
    ▼                                                        │
worker main thread, NgxPhase::Timer + AllowedOps::LOG:       │
    Pending → start round (premature=false) with a slot      │
    Live/Waiting → poll the callback VM's own bridge once;   │
      the parked future wakes through the same eventfd       │
      with the round's current identity                      │
    terminal → `at` retires; `every` retires the identity,   │
      mints a fresh generation for the same fake id, then    │
      re-arms                                                │
shutdown  pending → run once premature=true (no reschedule)  │
          live → cancel once (releases the parked async step)│
          → release every callback VM (P0 stays alive via its│
            own Arc<Program> until the VM is dropped)       │
```

### Light thread (not implemented; blocked)

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

Result/error/cancel remove the future from `NgxAsyncBridge`'s table.
Semaphore waits ride this same table, so their timeout, cancellation,
and drop paths reuse this contract rather than adding a second one
(`tests/ngx_semaphore.rs`, `src/semaphore.rs`).

### Landed timer lifecycle (implementation) versus remaining families

| Event | `timer.at` / `timer.every` (landed) | `thread.*` / `on_abort` (blocked) |
| --- | --- | --- |
| Parent Halts / 499 | timer independent: no `r`, nothing to cancel | cancel child / run abort callback once; exactly-once semantics still unspecified |
| Registration rejected | callback argument restored to the guest once; nothing retained | n/a |
| Deadline wake | only the registration's **current** identity is a deadline; a stale or duplicate wake matches no live entry and is ignored | ignore by generation (required) |
| `every` round terminal | next deadline scheduled only after terminal completion, under a fresh monotonically advancing generation for the same fake request id; rounds never overlap; capture cells persist | n/a |
| Worker shutdown | pending → run once with `premature=true`, never rescheduled; live → cancelled once (releases the parked async step); every callback VM released | cancel children / unregister |
| Reload | registrations stay with the old worker and its old `Arc<Program>`; new workers start empty; no migration, no rebinding | old-Program handles must fail closed (not implemented) |
| Panic | `catch_unwind` in the callback driver; reported once through the backend; `at` retires, `every` stays eligible; worker not unwound | fail the child once (not implemented) |

Exactly-once rule: a given task identity delivers at most one terminal
status `{completed, failed, cancelled, killed}`; the landed timer family
retires each round identity exactly once, so a retired generation can
never deliver a second round. Abort callback delivers at most one
invocation.

## Budgets

| Budget | Current | Scheduler implication |
| --- | --- | --- |
| Immediate-ready transitions | 64 per NGINX event turn (`MAX_IMMEDIATE_TRANSITIONS`) | A progress-making chain posts its identity to `ngx_posted_next_events` and continues across arbitrarily many turns. Posted continuations reserve no pending-wait permit. Zero-progress and reentrant continuations fail closed. |
| In-flight HostFuture waits | 1024 / worker (`DEFAULT_MAX_IN_FLIGHT`) | A real `Pending` request wait reserves this permit exactly once through `AccessExec`. Immediately-ready rollover does not. Timer callback concurrency is bounded by `max_running`, independently of this request-wait permit pool. |
| Synchronous idle VM pool | 4 (`MAX_IDLE_VMS`) | Import-free and pure synchronous programs are keyed by exact `Arc<Program>` identity. |
| Reusable request VM pool | 4 (`MAX_IDLE_REUSABLE_REQUEST_VMS`) | Buffered HTTP only after successful halt and quiescent reset. The exact key is program identity + phase + complete standard-host policy + ngx catalog fingerprint + standard catalog fingerprint. Failure, cancel, timeout, pending cleanup, excluded host families, and excess returns are dropped. Fresh request context, bridge, and runtime policy are installed at checkout. Callback VMs never use either request pool. |
| Resource table | pd-vm default 1024 | A callback VM gets its own **empty** table. A request handle cannot resolve in it (wrong table, fails closed); never insert a parent socket into it. |
| `ngx.ctx` | keys/depth/bytes caps | Callables rejected. Detached timer rounds have no request ctx (no ctx pointer in the round's scope). |
| Script call depth | `DEFAULT_MAX_SCRIPT_CALL_DEPTH` 1024 | A callback VM has its own depth; a timer round is not a recursive same-stack call. |
| Codec / body / header caps | see compatibility matrix | Unchanged. |
| Live timers | 1024 pending / 256 running per worker | Shipped `TimerConfig::default()` carried in every `TimerRegistration`; the backend enforces both atomically with insertion (pending) and round start (running, a parked callback still counted). The fake-request registry itself is a 1024-capacity `IdentityRegistry` (`MAX_LIVE_TIMER_REQUESTS`). |
| Proposed live `thread` count | not set | Need a hard cap before enabling spawn (suggested 64/request, 256/worker — **proposal only**, not implemented). |
| Live semaphores | 1024 / worker (`MAX_LIVE_SEMAPHORES`) | Worker-local `Arc<SemaphoreState>` in the worker registry; the VM table holds one handle entry per `new`. No shm; permits are process-local by design. |
| Permits per semaphore | 1000000 (`MAX_SEMAPHORE_COUNT`) | `new`/`post` above the ceiling is a `HostError`; no silent saturation. |

## Worker generation, reload, Program ownership

- Config compile produces `Arc<Program>` stored on `LocationConf`
  (`src/config.rs`). Nested locations inherit or override slots.
- Reload builds new Programs. Old location confs are dropped with the
  old cycle.
- `Vm::new_shared(Arc<Program>)` shares bytecode. Callable **runtime
  instances** are per VM (`owned_callables`).
- Request identities are `(generation, request_id)` (`src/notify.rs`).
  Stale wakes are ignored. The landed timer family reuses exactly that
  type: every registration owns a worker-unique, never-reused
  `RequestIdentity` whose request id is tagged out of the real
  request-id half (`FAKE_REQUEST_ID_TAG`), and every `every` re-arm
  rotates the generation.
- A future callable handle for the **still-blocked** families must
  carry `program_generation` (or `Arc::ptr_eq` on the Program plus a
  retired-set). Reload retires the old Arc. The landed timer family
  does not need one: the callback VM is created by the owned dispatch
  against the creating VM's `Arc<Program>` and holds that Arc for its
  whole lifetime, so an old-worker timer keeps running against the old
  immutable program and is never rebound to a reload's new Program. A
  new worker starts with an empty `thread_local` timer registry.
- Worker `init_process` starts Tokio + eventfd and installs the timer
  runtime (`install_worker_timers`); `exit_process` invalidates
  senders, cancels futures, closes fd once (`src/runtime.rs`). Scheduler
  state hooks **that** shutdown, not a new one. Both landed families
  do: `WorkerRuntime::shutdown` runs `semaphore::shutdown_current()`
  (closes live semaphores and wakes their waiters) before
  `timer::shutdown_current()` (pending callbacks once with
  `premature=true`, live callbacks cancelled, no reschedule); both run
  before the Tokio runtime is dropped.

## Capability attenuation

`AllowedOps` is a worker-thread stack (`src/host/context.rs`):

| Phase | Ops |
| --- | --- |
| rewrite / access / content | read/write request, log, ctx, response read/write, control |
| log | read request, log, ctx, response **read** |
| timer (timer callbacks) | log only (`AllowedOps::LOG`); request/response/ctx/control hosts fail closed |
| other catalog phases | `EMPTY` |

`NgxPhase::Timer` is the scope installed for a timer callback round: it has no
live request, so request-, response-, ctx-, and control-coupled hosts fail
closed while log, `ngx::sleep`/`sleep_ms`, cosocket I/O, the shared dictionary,
the semaphore family, and the timer family work. The scope never inherits the
creating request's `AllowedOps::for_phase`.

Attenuation for the remaining families (proposed; the timer row is
shipped behavior):

| Runner | Request FFI | Cosocket | ctx | Response write | Control | Shared dict | Log | Sleep |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| access/content parent | yes | yes | yes | phase rules | yes | yes | yes | yes |
| `ngx.thread` child | same request, **same** phase ops | yes, but handles must be child-table copies or explicitly moved | shared map needs a defined alias policy | yes | yes | yes | yes | yes |
| `ngx.timer.at/every` (landed) | **no** `r` | timer-owned outbound async work, never request sockets | no request ctx | no | no | yes (worker-level) | yes | yes |
| `ngx.on_abort` | dying `r` only if still live; prefer no sockets | no | read-only or none | no | no | yes | yes | no |
| `run_worker_thread` | no | no | no | no | no | no (or explicit copy-in) | cycle log only | no nginx sleep |

Until a runner can install a scope that is **narrower** than the
creator and cannot be widened by the script, the remaining callable
families stay blocked. The landed timer runner is the shipped example
of such a scope: `NgxPhase::Timer` + `AllowedOps::LOG`, narrowed from
the creating phase (a request-only host fails closed inside a round),
and the script cannot widen it.

The landed semaphore family is not a runner: it invokes no script, so
it needs no narrowed scope. Its hosts run under the access/content
`AllowedOps` already installed for the phase and touch no request state
of their own — `new`/`post`/`count` only borrow the calling VM's
resource table, and `wait` captures owned worker state plus an owned
timeout. Access and content accept the family; Log rejects it at
configuration compile.

## Request resource escape prevention

Escape means a detached or pooled context using:

- `*mut ngx_http_request_t`
- request pool pointers
- `RequestLocalCtx` of the creator
- UDP/TCP/TLS/semaphore `ResourceHandle` minted in a different VM
- `Value::Callable` bound to the request VM
- `HostFutureOutput::VmCompletion` closing over `&mut` request `Vm`

Prevention:

1. The shipped timer registration transfers **only** the owned
   callback plus a checked `Duration`/interval — there is no
   detached-args ABI to validate and no other value crosses. A future
   general detached boundary (thread / `run_worker_thread`) would have
   to be a **deep-owned Send subset**: null/int/float/bool/string/
   bytes/array/map of those, with callables and resource ids rejected
   (never treat `i64` as a handle). That is a proposal, not an
   implemented path.
2. Shipped timer callbacks cannot reach request resources:
   - **provenance**: the callback must belong to the creating VM's
     program; a foreign callable is rejected at registration
     (`callable does not belong to the source vm`);
   - **resource-bearing captures fail closed**, including nested ones:
     a capture whose declared schema contains a resource (through
     arrays, maps, or named types) is rejected before any callback VM
     exists, and the captured callable graph is depth-bounded and
     program-local;
   - **fresh VM**: the callback VM starts with a brand-new execution
     instance, stack, empty resource table, waiting slot, and
     module-state store, and copies no frames, locals, resources,
     waiting op, callback queue, or request-local host state, so no
     request `r`, ctx, or socket table has anywhere to live;
   - **no request FFI**: rounds run with no request pointer, no ctx
     pointer, and no request-owned pool; `NgxPhase::Timer` denies
     request/response/ctx/control hosts;
   - **fake identity only**: the deadline task holds a
     `CompletionSender` and the round's ordinary `RequestIdentity`
     (tagged fake request id), never a pointer.
3. Child request threads (ngx.thread) that **are** allowed sockets
   receive handles minted in the **child** table, or a move API that
   invalidates the parent handle (exactly-once move).
4. Wakers stay identity-only.
5. `ngx::ctx` already rejects callables; keep that.
6. Semaphore hosts resolve the `i64` only through the current executing VM's
   typed table. `new` inserts the entry there, and `post`/`count`/`wait` use
   that same table. A sync-only program may use a pooled VM; a program using
   async `wait` uses request-owned `AccessExec`. The wait future captures
   worker state, not the handle, so it never carries a VM-local token across
   the async boundary.

Current `ResourceHandle::from_raw` will accept any well-shaped `u64`.
That parse step does not transfer ownership across VMs. The landed semaphore
family stays safe because every handle is then resolved through
`Vm::host_context().borrow_resource` in the **calling** VM's own typed table.
A token copied from a foreign or reset VM therefore fails closed
(`resource_handle_wrong_table`) instead of reaching an entry; cosocket handles
resolve the same way.
The remaining callable families cannot ship on the raw integer ABI
alone: they would need an unforgeable identity, because a callable is
not a table entry with an arena and a generation to validate. The
landed timer family avoids the problem entirely: a callable is never
placed in a table — it is transferred once through the owned dispatch
and validated by provenance.

## Panic / error mapping

| Source | Parent request | Landed timer round | Thread pool thread (blocked) |
| --- | --- | --- | --- |
| Script `VmError` | existing 500 / log-phase log-only | reported once through the backend error sink → cycle log; callback VM recovered; `at` retires / `every` stays eligible | return error value to caller |
| HostError | existing | same as `VmError` | return error |
| `Waiting` in log | `LOG_CANNOT_WAIT` | n/a | n/a |
| Panic in script/host on worker main | content and log handlers guard with `catch_unwind`; resume runs inside the guarded eventfd read handler | pd-vm `catch_unwind` in `start` / `poll` / `resume` converts it to a reported callback error; the worker never unwinds | `catch_unwind`; error result |
| Error from a parked future | future error → resume error path | future error → reported through the same backend sink | future error → caller wait |
| Control `ngx::exit` in a callback | n/a | forbidden by attenuation (`AllowedOps::LOG`) | forbidden |

Do not call `ngx_http_finalize_request` from a timer, pool thread, or
host future.

## Proposed minimal upstream `pd-vm` public API (remaining blocked families only)

Scope: `ngx::thread::{spawn,wait,kill}`, `ngx::on_abort`, and
`ngx::run_worker_thread`. The timer family is **not** blocked on this
surface: it landed through the generic owned `TakeOwned` dispatch plus
the standard timer host module (sections
[2](#2-ngxtimeratevery-landed-detached-timer-registration) and
[7](#7-timer-landed-worker-owned-fake-request-timers)) and does not
need a proposed `ChildVm` / `TaskHandle`. These signatures do **not**
exist on the pin; they are the smallest surface that would make the
remaining callable families designable without using crate-private
interpreter state.

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
ngx-rustscript will not add `src/scheduler.rs` or register these
remaining callable hosts. The landed families are independent of this
surface: the semaphore family involves no callable, and the timer
family ships the only two catalogued `Callable` schemas (the
`TakeOwned` `callback` parameters of `ngx::timer::{at,every}`).

## Phased follow-up order

1. **This gate (done by the contract tests).** Fail-closed. No scheduler
   module; the remaining callable families (`ngx::thread::*`,
   `ngx::on_abort`, `ngx::run_worker_thread`) stay absent from the
   199-name manifest. The semaphore and timer families are landed and
   catalogued. Design published.
2. **Upstream `pd-vm`.** Land the minimal public API above. Pin bump
   in ngx-rustscript with an audit against this file.
3. **Semaphore (landed, independent).** Worker-owned resource +
   `HostFuture` wait, no callables. The addendum above covers worker
   lifetime versus the request VM table.
4. **`ngx.on_abort`.** Single handle, exactly-once, attenuated, runs
   on worker main before 499 finalize, cannot wait.
5. **`ngx.timer.at` then `every` (landed).** The pin's standard timer host
   module supplies the owned dispatch and the isolated callback VM; ngx adds
   the fake-request registry, the Timer-phase policy, and the counts. No
   generalized `ChildVm`/`TaskHandle` prerequisite was needed for this path.
6. **`ngx.thread.{spawn,wait,kill}`.** Independent wait slot or child
   VM **bound to the request identity**. Kill/cancel on 499 and
   shutdown.
7. **`ngx.run_worker_thread` last.** Configured pool + `DetachedValue`
   ABI. No request resources.

A later phase must not skip 2 **for the remaining families**. Semaphore
(3) and timer (5) landed without it because they need no generalized
child-VM API — the timer family uses the owned-dispatch seam and the
standard timer host module instead. Both are implemented as
`src/semaphore.rs` + `src/host/semaphore.rs` and `src/timer.rs` +
`src/host/timer.rs`, **not** as a `scheduler` module, and must not be
smuggled in as the callable scheduler engine for the remaining
families.

## Observable fail-closed boundary (this crate)

| Check | Expected |
| --- | --- |
| `ngx_host_manifest().len()` | 199 (191 callable baseline + 4 semaphore + 4 timer hosts) |
| catalog / manifest name lists | same 199 in the same order |
| `production_runtime_host_names` (per-name bind list) | 195; the four timer hosts resolve through the timer-host registry because the owned `ngx::timer::{at,every}` entries have no name-binding API |
| `bind_ngx_hosts` | installs the 4 semaphore hosts; no callable scheduler functions |
| `src/scheduler.rs`, `src/host/scheduler.rs` | absent |
| `src/lib.rs`, `src/host/mod.rs` | no `mod scheduler` |
| `HostFamily` / `HostCapability` | no `Scheduler` variant; the reviewed `Timer` and `Semaphore` variants are the only scheduler-family variants |
| catalog `HostTypeSchema::Callable` | exactly two: the `callback` parameters of `ngx::timer::{at,every}` (declared `TakeOwned`) |
| compile `ngx::thread::spawn`, `ngx::thread::wait`, `ngx::thread::kill`, `ngx::on_abort`, `ngx::run_worker_thread` in Access/Content/Log | `ConfigError::UnsupportedImport` (or the same fail-closed class) at **configuration** compile in every phase, not at request runtime |
| compile `ngx::timer::{at,every,pending_count,running_count}` | Access and Content accepted; Log `ConfigError::PhaseForbidden` at configuration compile; the bare `ngx::timer` marker is never catalogued, never bound, and rejected at configuration compile |
| bare `ngx::semaphore` marker | never catalogued, never bound, rejected at configuration compile |
| semaphore in Access/Content | accepted; `wait` may return `Waiting` |
| semaphore in Log | `ConfigError::PhaseForbidden` at configuration compile (phase set and closed Log allowlist both reject) |
| semaphore in Rewrite / Timer / Init etc. | rejected at configuration compile; catalog phases are exactly `[Access, Content]` |
| Log allowlist | synthetic log-allowlist probe `ngx::future::scheduler` (never a production import) is rejected; scheduler candidates stay rejected, and neither landed family is Log-eligible (`ngx::timer::*` and `ngx::semaphore::*` both fail configuration compile with `PhaseForbidden`) |

Revisit this file before adding any of the still-gated names
(`ngx::thread::*`, `ngx::on_abort`, `ngx::run_worker_thread`) to the manifest;
the semaphore and timer families are already landed and catalogued.
