# Time

[Documentation](../index.md) · [Directives](../directives.md) · [API](index.md) · [Execution model](../execution-model.md) · [Security](../security.md) · [Examples](../examples/index.md) · [Compatibility](../ngx-api-compatibility.md)

All time hosts are sync and use `PHASE_TIME`. Cached-clock behavior is
in cross-cutting item 6.

| OpenResty API | RustScript callable | Status | Limits | Semantic differences | Acceptance |
| --- | --- | --- | --- | --- | --- |
| `ngx.today` | `ngx::today() -> string` | adapted | cached clock | Local `yyyy-mm-dd` from the cached snapshot. | `tests/ngx_time.rs` (`today_localtime_and_utctime_use_fixed_formats`) |
| `ngx.time` | `ngx::time() -> int` | adapted | cached clock | Cached unix **seconds** as `int`, not `ngx::now`. | `tests/ngx_time.rs` (`time_is_int_and_now_is_float_from_the_same_cache`) |
| `ngx.now` | `ngx::now() -> float` | adapted | cached clock; 1s stale | Cached seconds plus milliseconds as `float`. Intra-script reads share the snapshot. | `tests/ngx_time.rs` (`intra_script_now_reads_share_the_cached_snapshot`, clock-boundary tests) |
| `ngx.update_time` | `ngx::update_time() -> ()` | adapted | samples OS immediately | Always refreshes the process-local cache. Observable side effect. | `tests/ngx_time.rs` (`update_time_samples_os_immediately_without_event_turn`) |
| `ngx.localtime` | `ngx::localtime() -> string` | adapted | cached clock | Local `yyyy-mm-dd hh:mm:ss`. | `tests/ngx_time.rs` (`today_localtime_and_utctime_use_fixed_formats`) |
| `ngx.utctime` | `ngx::utctime() -> string` | adapted | cached clock | UTC `yyyy-mm-dd hh:mm:ss`. | `tests/ngx_time.rs` (`today_localtime_and_utctime_use_fixed_formats`) |
| `ngx.cookie_time` | `ngx::cookie_time(sec: int) -> string` | supported | none beyond conversion | `Thu, 18-Nov-10 11:27:35 GMT`. | `tests/ngx_time.rs` (`http_cookie_and_parse_time_are_fixed`) |
| `ngx.http_time` | `ngx::http_time(sec: int) -> string` | supported | none beyond conversion | `Thu, 18 Nov 2010 11:27:35 GMT`. | `tests/ngx_time.rs` (`http_cookie_and_parse_time_are_fixed`) |
| `ngx.parse_http_time` | `ngx::parse_http_time(text: string) -> Option<int>` | adapted | 128-byte input | RFC 822 / RFC 850 / asctime. Invalid or pre-1970 is `Option::None`. Over 128 bytes is a host error, not `null`. | `tests/ngx_time.rs` (`parse_http_time_*`) |


---

See also: [API index](index.md) · [Phase sets](index.md#phase-sets) · [Cross-cutting adaptations](index.md#cross-cutting-adaptations) · [Hard caps](index.md#hard-caps)
