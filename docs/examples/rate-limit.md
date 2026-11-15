# Shared-dict fixed-window rate limiter

[Documentation](../index.md) · [Directives](../directives.md) · [API](../api/shared-dict.md) · [Examples](index.md)

Copy-paste example: `examples/rate-limit.rss` with `examples/rate-limit.nginx.conf`.
The access script counts requests in `rss_shared_dict` shared memory and returns
HTTP 429 with `Retry-After` when a trusted identity exceeds a fixed limit.

Replace every `/absolute/path/to/...` placeholder in the nginx snippet before
`nginx -t` or reload:

| Placeholder | Use |
| --- | --- |
| `/absolute/path/to/libngx_rustscript.so` | cdylib from `cargo build --lib` (`$CARGO_TARGET_DIR/debug/libngx_rustscript.so` or the release path you install) |
| `/absolute/path/to/rate-limit.rss` | this example script |
| `/absolute/path/to/error.log` | error log file |
| `/absolute/path/to/nginx.pid` | pid file |
| `/absolute/path/to/html` | document root containing `index.html` (content after a permitted request) |

The snippet listens on `127.0.0.1:8080` with `worker_processes 2`. Change listen
address, worker count, and zone size as needed.

## Script constants

Edit these three bindings in `examples/rate-limit.rss` together:

| Binding | Meaning |
| --- | --- |
| `LIMIT` | Exact number of requests permitted per identity per window (inclusive). The first `LIMIT` requests return through the access phase; request `LIMIT + 1` is 429. |
| `WINDOW_SECONDS` | Fixed-window length passed as `ngx::shared::safe_add` `exptime`. Units are **seconds** (OpenResty `exptime`), including integers. |
| `RETRY_AFTER` | String form of `WINDOW_SECONDS` written to the `Retry-After` header on 429. Keep it identical to `WINDOW_SECONDS`. |

The zone name `"ratelimit"` in the script must match `rss_shared_dict ratelimit ...`.

## Algorithm

1. Read `$rate_limit_key` via `ngx::var::get(...).unwrap_or("")`. Missing (`None`) or empty fails closed with 503.
2. Resolve `ngx::shared::dict("ratelimit")`. An unknown zone is a host error (500).
3. Establish a new window with `ngx::shared::safe_add(dict, key, 1, WINDOW_SECONDS, ())` so capacity pressure never evicts a live quota:
   - `ok` → this is count `1` for a new window;
   - `error == "exists"` → the window already exists; `ngx::shared::incr(dict, key, 1, (), ())` on that scalar (no `init`, so incr cannot allocate a new key);
   - any other `safe_add` error (including `no memory`) fails closed with 503.
4. Unexpected `incr` `error` or `forcible` fails closed with 503. `forcible` means an LRU victim was deleted; this example never accepts that.
5. If the resulting count is `> LIMIT`, set `Retry-After` and `ngx::exit(ngx::http::status_too_many_requests())` (429).
6. Otherwise the VM halts and the location continues (`try_files` serving `html/index.html` in the snippet).

The script does not log the identity key.

## Fixed-window boundary

This is a **fixed window**, not a sliding window.

- TTL is assigned only when `safe_add` creates the key (`exptime = WINDOW_SECONDS`).
- Subsequent `incr` calls pass `()` for init / init_ttl and keep the original expiry.
- When the key expires, the next request starts a new window at count 1.
- A client that spends `LIMIT` at the end of one window and `LIMIT` again at the
  start of the next can send up to `2 * LIMIT` requests in `WINDOW_SECONDS`
  wall time. That boundary burst is expected of fixed windows. Use a sliding
  window or leaky bucket if you need a smoother ceiling.

`ngx` cached time (`ngx_timeofday`) has millisecond fields and is updated on
the event loop. A one-second window is valid; very short sub-second windows
are more sensitive to event-loop delay.

## Per-worker vs shared memory

`rss_shared_dict` is NGINX shared memory: one slab, one rbtree/LRU, one mutex,
visible to every worker in the master. `worker_processes 2` (or `auto`) still
shares a single counter per key.

A process-local map would allow about `LIMIT * workers` requests. This example
does not do that. Cross-worker `safe_add` / `incr` is atomic under the zone mutex.

Reload (`nginx -s reload`) reuses the zone through NGINX `init(shm_zone, old_data)`
when the name and size are unchanged, so in-window counts survive reload.
Changing the zone size creates a new empty zone.

Graceful stop (`nginx -s quit` / SIGQUIT) tears workers down; the zone lives
only as long as the master cycle. A new master after a full stop starts empty.

## Capacity and fail-closed sizing

| Topic | Policy |
| --- | --- |
| Zone size | HTTP-main `rss_shared_dict ratelimit 10m;` — bytes of slab, minimum 8k. |
| New identity | `safe_add` never evicts. If the slab cannot hold another key, the new request is 503 and every live counter stays intact. |
| Existing identity | `incr` without `init` updates the live scalar. Unexpected `error` / `forcible` is 503. |
| Key / value limits | Keys ≤ 65535 bytes, values ≤ 65536 bytes; empty keys are rejected. |
| LRU victims | Not used. Ordinary allocating `incr(..., init, ...)` / `set` / `add` may evict; this example does not call them to create windows. |
| Unknown zone | `ngx::shared::dict` host error → 500. Config compile does not require the zone to exist. |
| Missing / empty `$rate_limit_key` | 503 before `safe_add`. |

Fail closed means deny on infrastructure failure. It does not mean 429.

Size the zone for the number of identities you expect to be live in one window,
with headroom. Exact bytes per node depend on NGINX slab class, key length, and
integer encoding; this example does not publish a bytes-per-key figure. `10m` is
a conservative starting point for typical deployments. If new identities return
503 while existing windows still have TTL, raise `rss_shared_dict`. Do not switch
to LRU eviction to “make room.”

## Trusted identity (reverse proxy)

The snippet sets:

```nginx
set $rate_limit_key $binary_remote_addr;
```

`$binary_remote_addr` is the immediate peer. That is the safe default.

Do **not** assign `$http_x_forwarded_for`, `$http_x_real_ip`, or any other
client-controlled header to `$rate_limit_key`. A client can rotate those values
and bypass the limit.

Behind a reverse proxy you operate, use `ngx_http_realip` so NGINX **overwrites**
the peer address from a header the proxy sets, after you name the trusted hop:

```nginx
set_real_ip_from 192.0.2.10;    # address or CIDR of the proxy you control
real_ip_header X-Real-IP;       # or X-Forwarded-For, if the proxy overwrites it
real_ip_recursive off;          # overwrite; do not walk a client-supplied list
```

Keep `set $rate_limit_key $binary_remote_addr;` after that — realip has already
replaced the peer. `real_ip_recursive on` is only for a chain of proxies you
listed in `set_real_ip_from`. Never point `set_real_ip_from` at untrusted
clients, and never read `X-Forwarded-For` / `X-Real-IP` directly in this script.

If `$rate_limit_key` is missing or empty, the script fails closed (503).

`$binary_remote_addr` is raw address bytes (4 bytes IPv4, 16 bytes IPv6). The
host adapts non-UTF-8 variable values (including embedded NUL) to a string the
dict can store. Distinct peers remain distinct keys.

## Status behavior

| Outcome | Status | Headers | Access-phase result |
| --- | --- | --- | --- |
| Count `1..=LIMIT` | whatever the location returns (snippet: 200) | none set by the script | VM `Halted`, access continues |
| Count `> LIMIT` | 429 | `Retry-After: WINDOW_SECONDS` (seconds, integer string) | `ngx::exit` HTTP 429 |
| Missing/empty key, `safe_add`/`incr` error, `forcible` | 503 | none | fail closed |
| Unknown / missing zone | 500 | none | host error, fail closed |

`Retry-After` is the configured window length, not the remaining TTL. After a
fixed window that is a close upper bound: the current window expires in at most
`WINDOW_SECONDS`, and the client may retry then.

HTTP 200 from this example is the location's content handler (`try_files`
serving `html/index.html`). Put `access_by_rss_file` in front of `proxy_pass`,
`try_files`, or another **content-phase** handler. Do not put `return` in the
same location: rewrite-phase `return` runs before access and skips the limiter.
Do not `ngx::exit(200)` from the limiter unless you want an empty 200 from
access.
