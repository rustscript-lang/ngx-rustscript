# Shared dictionary

[Documentation](../index.md) · [Directives](../directives.md) · [API](index.md) · [Execution model](../execution-model.md) · [Security](../security.md) · [Examples](../examples/index.md) · [Compatibility](../ngx-api-compatibility.md)

Directive `rss_shared_dict <name> <size>` is HTTP main only (`TAKE2`).
Minimum size is 8k. Names are unique, non-empty, UTF-8, at most 256
bytes. Duplicate names (same or different size) are rejected. The zone
is NGINX shared memory with a slab-backed rbtree/LRU store, the
`init(shm_zone, old_data)` reload contract, and a cross-worker mutex.
`ngx::shared::dict(name)` returns a typed handle; later methods take
that handle. Multi-return OpenResty values use host-catalog named structs
(`SharedGetResult`, `SharedStoreResult`, `SharedSafeStoreResult`,
`SharedIncrResult`, `SharedNumberResult`). Do not redeclare them in
guest scripts. `SharedGetResult.value` is `unknown?` because stored
values are heterogeneous; numeric results use `number?`.
Truly dynamic-key collections remain `map` fields inside typed envelopes, such
as `DecodedArgsResult.args` and `RequestHeadersResult.headers`.
Optional `exptime` and `flags` are explicit `null`. Values are bool, int,
float, string, or bytes; `set(..., null, ...)` deletes. `add`/`replace` reject
null.

The shared-dict result shapes are:

```rss
struct SharedGetResult {
    value: unknown?,
    flags: int,
    stale: bool,
    error: string?
}

struct SharedStoreResult {
    ok: bool,
    error: string?,
    forcible: bool
}

struct SharedSafeStoreResult {
    ok: bool,
    error: string?
}

struct SharedIncrResult {
    value: number?,
    error: string?,
    forcible: bool
}

struct SharedNumberResult {
    value: number?,
    error: string?
}
```

For example:

```rss
let added: SharedSafeStoreResult =
    ngx::shared::safe_add(dict, key, 1, WINDOW_SECONDS, null);
if added.ok {
    // ...
}
```
Keys are at most 65535 bytes; values at most 65536 bytes.
Empty keys are rejected. LRU eviction reports `forcible`; `safe_*`
never evicts and returns `no memory`. Shared dict hosts do not require
request-owned VMs.

| OpenResty API | RustScript callable | Status | Phases | Limits | Semantic differences | Acceptance |
| --- | --- | --- | --- | --- | --- | --- |
| `ngx.shared.DICT` | `ngx::shared::dict(name: string) -> resource` | adapted | `PHASE_DICT_LOOKUP` | unknown zone is a host error | Typed `ngx.shared.dict` handle, not `ngx.shared.NAME` field | `tests/ngx_shared_dict.rs` |
| `ngx.shared.DICT.get` | `ngx::shared::get(dict, key) -> SharedGetResult` | adapted | `PHASE_DICT_GET` | missing → `{value: null}` | Fields `value`, `flags`, `stale`, `error` | `tests/ngx_shared_dict.rs` |
| `ngx.shared.DICT.get_stale` | `ngx::shared::get_stale(dict, key) -> SharedGetResult` | adapted | `PHASE_DICT_GET` | stale expired values | `stale` is bool | `tests/ngx_shared_dict.rs` |
| `ngx.shared.DICT.set` | `ngx::shared::set(dict, key, value, exptime, flags) -> SharedStoreResult` | adapted | `PHASE_DICT_WRITE` | LRU up to 30 | Fields `ok`, `error`, `forcible` | `tests/ngx_shared_dict.rs` |
| `ngx.shared.DICT.safe_set` | `ngx::shared::safe_set(dict, key, value, exptime, flags) -> SharedSafeStoreResult` | adapted | `PHASE_DICT_WRITE` | no eviction | Fields `ok`, `error` | `tests/ngx_shared_dict.rs` |
| `ngx.shared.DICT.add` | `ngx::shared::add(dict, key, value, exptime, flags) -> SharedStoreResult` | adapted | `PHASE_DICT_WRITE` | exists → error | Fields `ok`, `error`, `forcible` | `tests/ngx_shared_dict.rs` |
| `ngx.shared.DICT.safe_add` | `ngx::shared::safe_add(dict, key, value, exptime, flags) -> SharedSafeStoreResult` | adapted | `PHASE_DICT_WRITE` | no eviction | Fields `ok`, `error` | `tests/ngx_shared_dict.rs` |
| `ngx.shared.DICT.replace` | `ngx::shared::replace(dict, key, value, exptime, flags) -> SharedStoreResult` | adapted | `PHASE_DICT_WRITE` | missing → not found | Fields `ok`, `error`, `forcible` | `tests/ngx_shared_dict.rs` |
| `ngx.shared.DICT.delete` | `ngx::shared::delete(dict, key) -> null` | adapted | `PHASE_DICT_WRITE` | — | Missing delete is success | `tests/ngx_shared_dict.rs` |
| `ngx.shared.DICT.incr` | `ngx::shared::incr(dict, key, value, init, init_ttl) -> SharedIncrResult` | adapted | `PHASE_DICT_WRITE` | numbers only | Fields `value`, `error`, `forcible` | `tests/ngx_shared_dict.rs` |
| `ngx.shared.DICT.ttl` | `ngx::shared::ttl(dict, key) -> SharedNumberResult` | adapted | `PHASE_DICT_WRITE` | never-expire is `0` | Fields `value`, `error` | `tests/ngx_shared_dict.rs` |
| `ngx.shared.DICT.expire` | `ngx::shared::expire(dict, key, exptime) -> SharedSafeStoreResult` | adapted | `PHASE_DICT_WRITE` | — | Fields `ok`, `error` | `tests/ngx_shared_dict.rs` |
| `ngx.shared.DICT.capacity` | `ngx::shared::capacity(dict) -> int` | adapted | `PHASE_DICT_WRITE` | — | Zone size in bytes | `tests/ngx_shared_dict.rs` |
| `ngx.shared.DICT.free_space` | `ngx::shared::free_space(dict) -> int` | adapted | `PHASE_DICT_WRITE` | approximate | Process-local: capacity − used; shm: `pfree * pagesize` | `tests/ngx_shared_dict.rs` |
| `ngx.shared.DICT.lpush` | `ngx::shared::lpush(dict, key, value) -> SharedNumberResult` | adapted | `PHASE_DICT_WRITE` | list len ≤ 65536; no LRU eviction | Fields `value`, `error`; bool/null/map → `bad value type`; scalar conflict → `value not a list` without mutating the node | `tests/ngx_shared_dict_list.rs` |
| `ngx.shared.DICT.rpush` | `ngx::shared::rpush(dict, key, value) -> SharedNumberResult` | adapted | `PHASE_DICT_WRITE` | same | Same result type as lpush; appends | `tests/ngx_shared_dict_list.rs` |
| `ngx.shared.DICT.lpop` | `ngx::shared::lpop(dict, key) -> SharedGetResult` | adapted | `PHASE_DICT_WRITE` | decode before unlink | Fields `value`, `flags`, `stale`, `error`; empty/missing → `{value: null}`; decode failure → `{error: "bad value type"}` and the item stays queued | `tests/ngx_shared_dict_list.rs` |
| `ngx.shared.DICT.rpop` | `ngx::shared::rpop(dict, key) -> SharedGetResult` | adapted | `PHASE_DICT_WRITE` | same | Same result type as lpop; pops tail | `tests/ngx_shared_dict_list.rs` |
| `ngx.shared.DICT.llen` | `ngx::shared::llen(dict, key) -> SharedNumberResult` | adapted | `PHASE_DICT_WRITE` | — | Missing/expired → `{value: 0}`; scalar → `value not a list` | `tests/ngx_shared_dict_list.rs` |
| `ngx.shared.DICT.flush_all` | `ngx::shared::flush_all(dict) -> null` | adapted | `PHASE_DICT_WRITE` | LRU; ≤1024 visits/call | Each call first frees already-expired keys (visit-capped), then marks live LRU keys with `expires=1` (visit-capped), then `expire(0)`. Repeated calls make monotonic progress across >1024 keys; one call does not wipe the zone. | `tests/ngx_shared_dict_list.rs` |
| `ngx.shared.DICT.flush_expired` | `ngx::shared::flush_expired(dict, max) -> int` | adapted | `PHASE_DICT_WRITE` | `max` 0/null still ≤ 1024 results; visits ≤ 1024 | Freed count. Walk stops at the result cap *or* the hard visit cap, whichever comes first. OpenResty `0` is unlimited. | `tests/ngx_shared_dict_list.rs` |
| `ngx.shared.DICT.get_keys` | `ngx::shared::get_keys(dict, max) -> array<string>` | adapted | `PHASE_DICT_WRITE` | read-only; `max` 0/null still ≤ 1024 results; visits ≤ 1024 | Oldest-first, skips expired, never deletes or force-expires. OpenResty `0` returns all keys; this project keeps both a result cap and a visit cap. | `tests/ngx_shared_dict_list.rs` |

List nodes are slab-owned; every mutation is atomic under the shm mutex. Failed item allocation rolls back an empty list. `get`/`incr` on a list return structured errors (`value is a list` / `not a number`) and never overwrite the node. List items are int/float/string/bytes. `get_keys` is read-only under the shm mutex: it never force-expires or deletes, skips expired keys, and stops at both the result cap and a 1024-visit cap. `flush_all` / `flush_expired` likewise bound visits independently of hits. There is no process-local production fallback: `cargo test` uses an in-process zone; the cdylib talks to NGINX shm only.

Cross-worker visibility, atomic incr, and reload reuse are covered by
`tests/shared_dict_nginx.rs`. Config parsing is `tests/shared_dict_config.rs`.


---

See also: [API index](index.md) · [Phase sets](index.md#phase-sets) · [Cross-cutting adaptations](index.md#cross-cutting-adaptations) · [Hard caps](index.md#hard-caps)
