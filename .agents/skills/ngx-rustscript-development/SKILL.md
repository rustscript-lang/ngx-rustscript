---
name: ngx-rustscript-development
description: Use when developing, debugging, reviewing, or testing ngx-rustscript host functions, async VM execution, NGINX suspension, eventfd wakeups, cancellation, or worker lifecycle.
---

# ngx-rustscript development

## Invariants

- Keep every NGINX request and VM on its worker main thread.
- Tokio futures and wakers may carry only owned `Send + 'static` data and generation-safe request identities.
- Keep `pd-host-function` pinned to the same Git URL and exact revision as `pd-vm`.
- Declare every production host with `#[pd_host_function(name = "namespace::name")]`.
- Use macro-generated argument conversion and callable wrapper for runtime binding.
- The current pinned macro does not expose compiler catalog generation. Keep the separate `HostApiCatalog` declaration as minimal configuration-time import allowlist metadata; never use it to implement runtime dispatch.
- Do not handwrite `HostFunction`, `HostArgsFunction`, `HostFunctionSchema`, argument decoding, or `CallOutcome::Yield` replay for ordinary hosts.
- Inspect the exact pinned macro parser before adding attributes. Preserve the verified module re-exports required by generated code.

A normal async host should contain only validation and awaited business work:

```rust
#[pd_host_function(name = "ngx::sleep")]
async fn ngx_sleep_impl(ms: i64) -> VmResult<()> {
    if ms < 0 {
        return Err(VmError::HostError(
            "ngx::sleep expects non-negative milliseconds".to_string(),
        ));
    }
    tokio::time::sleep(Duration::from_millis(ms as u64)).await;
    Ok(())
}
```

## Generic pull-async bridge

Use one `HostAsyncBridge` for all macro-generated async hosts:

- Store exact `HostOpId -> HostFuture` entries.
- Reject duplicate IDs without replacement.
- Enter the worker Tokio runtime handle while polling.
- Remove entries on ready success, ready error, and cancellation.
- Let VM bridge cancel/reset/drop own future cancellation; never add a host-specific task registry or parallel cancellation protocol.

Drive pending work through VM APIs:

1. `run()` or `resume()` returns `VmStatus::Waiting(op_id)`.
2. Reserve bounded wake capacity.
3. Register the request identity and waker before first poll.
4. Call `poll_waiting_host_op` once to register the future wake source.
5. On `Ready`, unregister, release capacity, and `resume()`.
6. On `Pending`, return the NGINX waiting result.
7. Handle later wakes in fresh NGINX event turns.

Never re-enter the VM from `Wake::wake` or the original access stack.

## Waker and eventfd

A waker contains only an owned notifier, a coalescing flag, and a generation-safe request identity. It must never contain an NGINX request, pool address, VM, raw pointer, or unsafe `Send` wrapper.

- Reserve capacity before exposing the waker.
- Queue identity before writing eventfd.
- Clear coalescing state before polling so a concurrent wake can enqueue again.
- Drain on the worker main thread and validate exact generation before pointer access.
- Ignore stale identities and reclaim capacity exactly once.
- Keep eventfd nonblocking and close-on-exec.
- Preserve the writer/close gate to prevent writes to a reused fd.

## Cancellation and shutdown

Request cleanup order:

1. Mark cancelled/dead.
2. Remove the exact registry identity.
3. Cancel/reset/drop the VM bridge future.
4. Release wake capacity and request state.
5. Ignore queued stale wakes by generation.

Keep close detection on the pinned public NGINX API and finalize confirmed client closes through standard HTTP 499 behavior.

Worker shutdown order:

1. Reject new suspensions.
2. Invalidate notifiers and wakers.
3. Cancel/drop bridge futures and request states.
4. Drop Tokio runtime.
5. Unregister and close eventfd through the fd gate.

Every ready, error, cancellation, stale wake, setup failure, and shutdown path releases bounded capacity exactly once. Reuse a VM only after successful reset reports it reusable.

## Verification

Tests must discriminate the generic design from Yield replay or host-specific coordinators. Cover:

- macro wrapper execution and catalog contract;
- initial pending, wake, ready, and resume;
- immediate completion and consecutive async operations;
- a second async host using the same bridge unchanged;
- duplicate operation IDs and ready/error cleanup;
- cancel/reset, duplicate wake, wake-during-poll, stale generation;
- capacity exhaustion/reclaim, disconnect 499, and worker shutdown.

Run focused tests, then:

```sh
cargo fmt --check
cargo check --all-targets
cargo clippy --all-targets -- -D warnings
cargo test -- --test-threads=1
cargo build --lib
bash tests/verify_add_dynamic.sh
python3 tests/nginx_integration.py --nginx <absolute-nginx-1.30.4> --module <absolute-cdylib>
python3 tests/nginx_integration.py --nginx <absolute-nginx-1.30.4> --module <absolute-cdylib>
```

Primary async evidence is relative completion order: a plain request finishes while a long scripted request remains active.

## Scope guards

Do not add custom ELF sections, fingerprints, `readelf`, `objcopy`, procfs inspection, synthetic eventfd probes, private log protocols, hidden test directives, artifact snapshots, or test-only production branches.

README stays user-facing: purpose, requirements, build/install, NGINX configuration, RustScript examples, runtime limits, tests, and troubleshooting. Keep task history, reviews, plans, and progress outside README.
