# Sleep and timers

[Documentation](../index.md) · [Directives](../directives.md) · [API](index.md) · [Execution model](../execution-model.md) · [Security](../security.md) · [Examples](../examples/index.md) · [Compatibility](../ngx-api-compatibility.md)

| OpenResty API | RustScript callable | Status | Phases | Yield | Limits | Semantic differences | Acceptance |
| --- | --- | --- | --- | --- | --- | --- | --- |
| `ngx.sleep` | `ngx::sleep(seconds: number) -> null` | adapted | `SLEEP_PHASES` | Waiting | in-flight 1024; rejects negative / NaN / infinity / overflow | Seconds (int or float), not milliseconds. Tokio timer; completion via `HostFuture` -> waker -> eventfd -> worker-main-thread `resume`. Client close during wait is HTTP 499. | `tests/vm_sleep.rs`, `tests/host_catalog.rs`, `tests/access_async.rs`, `tests/lifecycle.rs`, `tests/nginx_integration.py` |
| `ngx.sleep` (legacy ms form) | `ngx::sleep_ms(ms: int) -> null` | adapted | `SLEEP_PHASES` | Waiting | in-flight 1024; `ms >= 0` | Not an OpenResty name. Integer milliseconds for scripts written before the seconds migration. Same Waiting path as `ngx::sleep`. | `tests/vm_sleep.rs`, `tests/access_async.rs`, `tests/request_driver.rs`, `tests/lifecycle.rs` |


## `ngx::timer` family (landed)

Worker-owned OpenResty-style timers. A registration survives the request that
created it; the callback runs on a timer-owned VM.

| OpenResty API | RustScript callable | Status | Phases | Yield | Limits | Semantic differences | Acceptance |
| --- | --- | --- | --- | --- | --- | --- | --- |
| `ngx.timer.at` | `ngx::timer::at(seconds: number, callback: fn(bool) -> unknown) -> bool` | adapted | access, content | sync (the callback runs later) | checked `Duration`; negative/NaN/infinite rejected; `TimerConfig.max_pending` = 1024, worker pool 1024 | Seconds (int or float), not milliseconds. The callback is consumed (`TakeOwned`). `premature=false` on normal expiry. | `tests/ngx_scheduler_contract.rs`, `tests/ngx_timer.rs`, `tests/timer_nginx.rs` |
| `ngx.timer.every` | `ngx::timer::every(seconds: number, callback: fn(bool) -> unknown) -> bool` | adapted | access, content | sync (the callback runs later) | checked `Duration`; `interval > 0` required; same caps | Rounds serialize: the next deadline is scheduled only after the previous round is terminal, its identity rotates to a fresh generation, and mutable capture cells persist between rounds. | `tests/ngx_timer.rs`, `tests/timer_nginx.rs` |
| `ngx.timer.pending_count` | `ngx::timer::pending_count() -> int` | adapted | access, content | sync | none | Registrations waiting to begin, or waiting for a running slot. | `tests/ngx_timer.rs`, `tests/timer_nginx.rs` |
| `ngx.timer.running_count` | `ngx::timer::running_count() -> int` | adapted | access, content | sync | `TimerConfig.max_running` = 256 | Live callback rounds, including callbacks parked on async host work. | `tests/ngx_timer.rs`, `tests/timer_nginx.rs` |

Binding and lifetime:

- The two registration hosts consume their callback through
  `HostParamPassing::TakeOwned`; they are the only catalogued callables. A
  program that imports a timer host is bound from the timer-host registry
  (`ngx_host_registry`), and callback VMs spawn from that same registry.
- Every accepted registration owns one ordinary `RequestIdentity` and one fake
  request in a bounded worker pool (1024, the capacity of the shared
  `request_state::IdentityRegistry`). The deadline task holds only a
  `CompletionSender` and that identity and wakes through the worker's existing
  eventfd; no second identity, eventfd, or queue exists.
- Identity: fake request ids are worker-unique, tagged out of the real
  request-id space (`timer::FAKE_REQUEST_ID_TAG`), and never reused, so a timer
  identity can never equal a request identity and the two dispatch paths stay
  disjoint without a reserved generation. An `every` round retires its identity
  on completion and re-arms under a fresh monotonically advancing generation for
  the same fake request id, so a stale or duplicate wake of a finished round is
  ignored and can never start the pending round early or break the interval.
- Callback VM: private execution instance, own `NgxAsyncBridge`, script
  bytecode on the worker main thread only.
- Callback phase: `NgxPhase::Timer` with a log-only `AllowedOps` allowlist. Log,
  `ngx::sleep`/`sleep_ms`, outbound async I/O, the shared dictionary, the
  semaphore family, and the timer family work. Request headers/body/raw,
  `ngx.ctx`, response output, and control (`ngx::exit`, `ngx::redirect`) fail
  closed and are reported through the worker log. The standard I/O, HTTP, and
  SQLite policies are per-VM and are not inherited by a callback VM.
- Errors: a callback failure is reported once through the worker log; `at`
  terminates, `every` stays eligible for its next round.
- Shutdown and reload: worker exit runs pending callbacks once with
  `premature=true` (including a pending `every` round, which is not rescheduled),
  cancels live callbacks, stops further `every` rounds, and releases every
  callback VM. A reload does not migrate timers; new workers start with zero
  timers. Creating-request cleanup never cancels a timer.
- Configuration: registration is rejected in `log_by_rss_file` at
  configuration compile (`PhaseForbidden`), and the bare `ngx::timer` marker is
  not a callable.

---

See also: [API index](index.md) · [Phase sets](index.md#phase-sets) · [Cross-cutting adaptations](index.md#cross-cutting-adaptations) · [Hard caps](index.md#hard-caps)
