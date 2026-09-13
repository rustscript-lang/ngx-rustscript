# Semaphore

[Documentation](../index.md) · [Directives](../directives.md) · [API](index.md) · [Execution model](../execution-model.md) · [Security](../security.md) · [Examples](../examples/index.md) · [Compatibility](../ngx-api-compatibility.md)

`ngx.semaphore` is the first landed scheduler-family host set. It is a
**worker-owned counting semaphore** referenced through a **VM-local
resource handle**; it does not need a script callable, so it does not
unlock `ngx.thread`, `ngx.timer`, `ngx.on_abort`, or
`ngx.run_worker_thread`.

Permit counters and the FIFO waiter queue live in a worker registry
(`thread_local`, one registry per nginx worker). A request VM only holds
a typed `resource<ngx.semaphore>` entry in **its own** resource table,
so a handle is visible to the VM that created it and to nothing else.
Wait futures hold worker state, an owned timeout, and a drop guard —
never a request, pool, `Vm`, `RequestLocalCtx`, or raw pointer. The
future is polled by the existing `NgxAsyncBridge`, and waking a waiter
only calls the eventfd-backed waker of the current request.

| OpenResty API | RustScript callable | Status | Phases | Yield | Limits | Semantic differences | Acceptance |
| --- | --- | --- | --- | --- | --- | --- | --- |
| `ngx.semaphore.new` | `ngx::semaphore::new(count) -> resource` | adapted | `PHASE_SEMAPHORE` | sync | `count` int `0..=1000000`; ≤1024 live semaphores per worker | Worker-owned object, not userdata; the script sees an `i64` handle for its own VM table. | `tests/ngx_semaphore.rs`, `src/semaphore.rs` |
| `sem:wait` | `ngx::semaphore::wait(sem, timeout) -> bool` | adapted | `PHASE_SEMAPHORE` | Waiting | `timeout` seconds; `null` = no timer, `0` = immediate check; shares the 1024 in-flight wait cap | Returns `true` on acquire and `false` on timeout instead of OpenResty's `nil, "timeout"` pair. `null` (not `0`) means "wait forever". FIFO: the oldest waiter wins each permit. | `tests/ngx_semaphore.rs`, `src/semaphore.rs` |
| `sem:post` | `ngx::semaphore::post(sem, count) -> null` | adapted | `PHASE_SEMAPHORE` | sync | `count` int `0..=1000000` or `null` (=1); counter ceiling 1000000 | Posting past the ceiling is a `HostError` instead of silently saturating. `0` wakes nobody. | `tests/ngx_semaphore.rs`, `src/semaphore.rs` |
| `sem:count` | `ngx::semaphore::count(sem) -> int` | adapted | `PHASE_SEMAPHORE` | sync | — | Matches OpenResty: permits ready minus queued waiters, so a negative value is the FIFO wait depth. | `tests/ngx_semaphore.rs`, `src/semaphore.rs` |

`PHASE_SEMAPHORE` is `access` + `content` only. `log_by_rss_file` fails
configuration compile (`PhaseForbidden`) for every semaphore import, and
the closed Log allowlist also rejects the family and capability.

## Validation

- `new(count)`: `count` must be an integer in `0..=1000000`. Missing or
  invalid counts, and a worker already holding 1024 live semaphores, fail
  with a named `HostError` at request time.
- `wait(sem, timeout)`: `timeout` is a number of seconds (fractions
  allowed). `null` waits without arming a timer (wait forever); `0` is an
  immediate, non-blocking check that returns `false` when no permit is
  free; a positive number arms the owned timeout. Negative, `NaN`,
  infinite, and non-number timeouts are rejected before the future is
  submitted. RustScript adds `null` for an unbounded wait; OpenResty and
  RustScript both use `0` for the immediate attempt.
- `post(sem, count)`: `count` is an integer or `null` (one permit).
  Negative, fractional, and non-number counts are rejected. `0` is a
  no-op. `count + current` above 1000000 is a `HostError`.
- `count(sem)`: read-only.
- The catalog types the handle as `resource<ngx.semaphore>`, so an
  integer handle is a **configuration compile** error, not a runtime
  lookup. Closed or released entries still fail closed at runtime.

## Wait, timeout, cancellation, and cleanup

| Event | Required behavior |
| --- | --- |
| Permit available at submit | The initial bridge poll returns Ready; the request never suspends. |
| No permit | The waiter joins the FIFO queue, the request returns `NGX_AGAIN`, and `DEFAULT_MAX_IN_FLIGHT` (1024) is reserved exactly once. |
| `post` from another request / the same worker | At most `min(permits, queued)` **front** waiters with registered wakers are woken; each grant cascades the next wake. |
| Timeout | The wait resolves with `false` after the timeout elapses, releases the reservation, and removes its FIFO entry. No permit is consumed. A `0` timeout resolves as soon as the FIFO front has no permit. |
| Client close / 499 / request end | The bridge drops the future; the drop guard removes the FIFO entry exactly once and the reservation is released. |
| Typed cancellation (`OperationCancelReason`) | Uses the existing bridge `request_cancel_op`/`poll_cancel_op` protocol; no new cancellation protocol or eventfd. |
| Worker `exit_process` | Live semaphores are closed, every waiter is woken, the registry is emptied, and parked waits fail closed. Ids are never reused. |
| Last handle removal | Closing the last VM-local handle removes the worker entry; an in-flight wait keeps its own state alive and cannot observe a recycled identity. |
| Double cleanup | Deregistration is idempotent: a granted waiter already left the queue, and a second release is a no-op. Permits are consumed only at grant time, so cleanup can never leak or double-release one. |

## Budgets

| Budget | Value |
| --- | --- |
| Permits per semaphore | 1000000 (`MAX_SEMAPHORE_COUNT`) |
| Live semaphores per worker | 1024 (`MAX_LIVE_SEMAPHORES`) |
| In-flight semaphore waits | Shared `DEFAULT_MAX_IN_FLIGHT` (1024) per worker |
| Resource table | pd-vm default 1024 entries per VM |

---

See also: [API index](index.md) · [Phase sets](index.md#phase-sets) · [Scheduler design](../design/ngx-scheduler.md) · [Hard caps](index.md#hard-caps)
