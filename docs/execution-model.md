# Execution model

[Documentation](index.md) · [Directives](directives.md) · [API](api/index.md) · [Execution model](execution-model.md) · [Security](security.md) · [Examples](examples/index.md) · [Compatibility](ngx-api-compatibility.md)

How ngx-rustscript runs a compiled script inside an NGINX worker. This
page is the lifecycle reference; host signatures live in the
[API reference](api/index.md). The
[scheduler design](design/ngx-scheduler.md) covers the blocked
thread/`on_abort`/`run_worker_thread` callable families plus the
landed worker-owned semaphore and timer lifecycle/callback contracts.

## Worker process

Worker runtime is created only in NGINX worker `init_process` after
fork, and only when `ngx_process` is `NGX_PROCESS_WORKER` or
`NGX_PROCESS_SINGLE`. Helper processes skip initialization. `nginx -t`,
master, and config never create Tokio threads or the eventfd.
`exit_process` shuts the runtime down and is idempotent.

Tokio features: `rt` + `rt-multi-thread` + `time` + `net` + `sync` +
`io-util`. One worker thread named `ngx-rustscript-async` drives ngx
cosockets and pd-vm async I/O/HTTP host futures. SQLite operations use
pd-vm's scoped worker-backed operation path and resume through the same
eventfd bridge.

## Execution paths

On each request the access (or content) handler leases a
worker-thread-local VM via `Vm::new_shared(Arc<Program>)`.

**Pooled programs without request-state requirements** run to
`VmStatus::Halted` and return `NGX_OK` so the next phase proceeds. This
includes import-free and pure-sync host programs, including sync-only
`ngx::semaphore::new`, `post`, and `count`. After `Halted` and after a
recoverable runtime error the VM is reset with `Vm::reset_for_reuse`
and parked only when `Vm::is_reusable` is true. Idle pool size is 4.
VM errors and unexpected `Waiting` on pooled programs fail the request
with 500.

**Request-state programs** bind request-local execution state when the
compiled script uses pd-vm I/O, HTTP client, SQLite, `ngx::sleep`,
`ngx::sleep_ms`, UDP/TCP cosocket, `ngx.ctx`, an async
`ngx::semaphore::wait`, or mutation/control capabilities (`RequestWrite`,
`ResponseWrite`, `Control`). Pure-sync request-read and sync-only semaphore
programs stay on the synchronous pool path.

A request-state program is statically eligible for VM reuse when none of its
imports or builtins may suspend or retain VM state. This admits buffered
`http::client::request` and synchronous request-context programs, including
key-auth/control flows and programs using `ngx.ctx`, `RequestWrite`, or
`ResponseWrite`. Standard I/O, SQLite, SSE, timers, sleep, cosockets, every
other standard host import, async ngx hosts, resource-bearing host schemas,
and unknown host schemas are excluded. These checks are closed over the whole
compiled program; adding any excluded import disables reuse.

Static admission only selects the reusable request path. A successful
`Halted` execution enters the runtime reuse gate after `reset_for_reuse`, a
ready `poll_reset_for_reuse`, no pending scope reset, `clear_async_bridge`,
`clear_runtime_print_sink`, and `Vm::is_reusable` all succeed. Failure,
cancellation, timeout, pending cleanup, reset failure, and capacity overflow
drop the VM. Every checkout installs a new `NgxAsyncBridge`, print sink,
request identity, raw request pointer scope, empty `RequestLocalCtx`, and
runtime policy.

The reusable-request pool retains at most four idle VMs per worker. Its exact
key is `(Arc<Program> identity, NGINX phase, complete StandardHostPolicy,
ngx catalog fingerprint, standard catalog fingerprint)`. Equal source text in
distinct compiled programs does not share a VM. Access/content phases and
locations with different HTTP, I/O, or SQLite grants remain isolated. Policy
is installed from the selected key on every newly created VM and remains part
of every checkout match.

`pd-vm` pin (see the crate README) provides `Vm::new_shared(Arc<Program>)`,
`HostAsyncBridge`, `Vm::set_async_bridge`, `VmStatus::Waiting`,
`Vm::poll_waiting_host_op`, and `Vm::resume`. `try_new` only wraps owned
`Vm::new` after frame-limit validation. Normal termination is
`Ok(VmStatus::Halted)`. Drop of `Vm` owns cancellation and resource
closure.

## Waiting and eventfd

`Waiting` returns `NGX_AGAIN` without incrementing `r->main->count`.
The eventfd callback polls the waiting host op, resumes the same VM on
the worker main thread, and continues `ngx_http_core_run_phases`
exactly once so the phase handler can return `NGX_OK`. Only a successfully
halted, fully quiescent buffered-HTTP VM may return to the reusable-request
pool.

Async wait capacity is `DEFAULT_MAX_IN_FLIGHT` (1024). Reserve is
rejected when full. Host futures and wakers own only a queue sender and
request identity — no NGINX request/pool/VM pointers cross into Tokio,
and Tokio never calls `ngx_post_event`.

Completions enqueue the request identity under a mutex, then write
`eventfd(2)` created with `EFD_NONBLOCK|EFD_CLOEXEC`. The worker main
thread registers the fd with `ngx_get_connection`. The pinned Linux
epoll path uses `add` with `EPOLLIN`.

One NGINX event turn drives at most 64 immediately-ready host transitions.
Budget exhaustion posts the same generation-safe identity to
`ngx_posted_next_events` without reserving the 1024-slot pending-wait capacity.
Progress-making turns may continue without an aggregate cap until the VM
halts, reaches a real `Pending`, or fails. The driver rejects a continuation
that reports zero transitions or re-enters before the prior turn is retired.

Client close while a wait is in flight is detected with a worker-thread
wrapper around `ngx_http_test_reading`. Confirmed close finalizes once
with `NGX_HTTP_CLIENT_CLOSED_REQUEST` (access log 499), then
request-pool cleanup invalidates the identity and drops the host
future.

Worker `exit_process` rejects new work, invalidates senders/wakers,
cancels/drops host futures, unregisters/closes eventfd once, and clears
worker registries without touching request pointers. Long sleeps do not
delay graceful worker exit (SIGQUIT). Graceful master shutdown cancels
in-flight host futures instead of waiting them out.

## Content output

Content-phase `ngx::send_headers` / `ngx::print` / `ngx::say` /
`ngx::flush` / `ngx::eof` copy payload on the worker thread into
request-pool buffers and send through `ngx_http_output_filter`. Access
rejects those imports at config time. Hosts never write the NGINX
socket and never call `ngx_http_finalize_request`. The content handler
remains the sole completion owner and sends `last_buf` if the script
omitted `eof`.

## Control flow

Hosts set request-owned `RequestDisposition` then stop the VM. The
phase handler is the sole finalizer: it returns the NGINX status.
Ordinary `HostError` cannot forge control. First disposition wins.

## Log phase

`log_by_rss_file` is synchronous and fail-closed. It runs after the
response is complete, never waits, and logs errors without mutating the
already-sent response. `Waiting` logs `log handler cannot wait`, drops
the VM, and never arms eventfd, a timer, or `r->main->count`. See
[`log_by_rss_file`](directives.md#log_by_rss_file).

## Sleep hosts

`ngx::sleep` is a `pd-host-function` async host (`seconds:number -> null`).
`seconds` is an OpenResty-style integer or float; negative, NaN,
infinity, and duration overflow are rejected. `ngx::sleep_ms` is the
millisecond integer form (`ms:int -> null`). The generated wrapper
submits an owned host future; the impl validates the duration and
awaits Tokio. The first host entry returns `VmStatus::Waiting`. Two
sequential sleeps reuse the same generation-safe request identity and
each wait once.

## Time cache

Time hosts use a process-local cached clock, not nginx `ngx_time` /
`ngx_timeofday`. Reads reuse the snapshot while the event generation is
unchanged and the snapshot is younger than 1s. Access-phase request
execution and later eventfd resume/poll turns call
`notify_cached_clock_event_boundary` so the cache may refresh like
nginx `ngx_time_update` on the event loop. `ngx::update_time()` always
samples the OS (or a test override) and replaces the snapshot.
