# Codec and hash

[Documentation](../index.md) · [Directives](../directives.md) · [API](index.md) · [Execution model](../execution-model.md) · [Security](../security.md) · [Examples](../examples/index.md) · [Compatibility](../ngx-api-compatibility.md)

Shared codec limits: 65536-byte input, 196608-byte output. All rows are sync.

OpenResty v0.10.29 documents these codec APIs for `log_by_lua*`
(`ngx.escape_uri` context table includes `log_by_lua*`; the other codec
rows share that allowlist except as noted). `log_by_rss_file` accepts
them only because that citation exists and each name is on the closed
Log allowlist.

`ngx::decode_args` uses this fixed outer type; its `args` field remains a
dynamic-key map:

```rss
struct DecodedArgsResult {
    args: map,
    truncated: bool
}
```

| OpenResty API | RustScript callable | Status | Phases | Limits | Semantic differences | Acceptance |
| --- | --- | --- | --- | --- | --- | --- |
| `ngx.escape_uri` | `ngx::escape_uri(uri: string, escape_type: unknown) -> string` | adapted | `PHASE_ESCAPE_URI` | input/output caps; type `0..=8` | Second arg is `()` (default type 2, URI component) or int `0..=8`. Types match lua-nginx-module bitmaps (0 URI, 1 args, 2 component, 3 HTML, 4 refresh, 5/6 memcached, 7 header name, 8 header value). | `tests/ngx_codec.rs` (`escape_uri_*`), `tests/host_catalog.rs` |
| `ngx.unescape_uri` | `ngx::unescape_uri(uri: string) -> string` | adapted | `PHASE_UNESCAPE_URI` | input cap | `+` becomes space; invalid `%` left as-is; non-UTF-8 bytes kept as `char::from` string. Not allowed in `ssl_session_fetch` / `ssl_session_store`. | `tests/ngx_codec.rs` (`unescape_uri_*`) |
| `ngx.encode_args` | `ngx::encode_args(args: unknown) -> string` | adapted | `PHASE_ENCODE_ARGS` | 1024 pairs; input/output caps | Catalog type is `unknown` because object literals are not an exact map schema; runtime requires a map. Array values emit repeated keys. `true` is a bare key; `false`/`()` omitted. Nested maps/arrays-as-single-value rejected. Duplicate map keys overwrite; use arrays for multi-value keys. Not allowed in `init` / `init_worker`. | `tests/ngx_codec.rs` (`encode_args_*`) |
| `ngx.decode_args` | `ngx::decode_args(query: string, max_args: unknown) -> DecodedArgsResult` | adapted | `PHASE_DECODE_ARGS` | default 100; hard max 1024 | Returns fields `args: array<ArgEntry>` and `truncated: bool`. See cross-cutting item 4. `max_args` is `()` or int. | `tests/ngx_codec.rs` (`decode_args_*`) |
| `ngx.encode_base64` | `ngx::encode_base64(input: unknown, no_padding: unknown) -> string` | adapted | `PHASE_CODEC_MISC_SSL_CLIENT_HELLO` | input/output caps | `input` is string or bytes. `no_padding` is `()`/`false` or `true`. | `tests/ngx_codec.rs` (`encode_base64_padding_and_decode_variants`) |
| `ngx.decode_base64` | `ngx::decode_base64(text: string) -> Option<bytes>` | adapted | `PHASE_CODEC_MISC_SSL_CLIENT_HELLO` | input cap | Invalid input is `None`, not an error. Padding may be omitted. | `tests/ngx_codec.rs` (`encode_base64_padding_and_decode_variants`) |
| `ngx.decode_base64mime` | `ngx::decode_base64mime(text: string) -> Option<bytes>` | adapted | `PHASE_CODEC_MISC` | input cap | Skips non-alphabet bytes; stops at first `=`. `data_len % 4 == 1` is `None`. **Not** allowed in `ssl_client_hello` (OpenResty v0.10.29 exception). | `tests/ngx_codec.rs` (`decode_base64mime_*`), `tests/host_catalog.rs` |
| `ngx.crc32_short` | `ngx::crc32_short(data: string) -> int` | supported | `PHASE_CODEC_MISC_SSL_CLIENT_HELLO` | input cap | IEEE CRC-32. Currently matches `crc32_long`. | `tests/ngx_codec.rs` (`hash_and_hmac_use_fixed_vectors`) |
| `ngx.crc32_long` | `ngx::crc32_long(data: string) -> int` | supported | `PHASE_CODEC_MISC_SSL_CLIENT_HELLO` | input cap | Same result as `crc32_short` in this release. | `tests/ngx_codec.rs` (`hash_and_hmac_use_fixed_vectors`) |
| `ngx.hmac_sha1` | `ngx::hmac_sha1(secret: string, data: string) -> bytes` | adapted | `PHASE_CODEC_MISC_SSL_CLIENT_HELLO` | input cap on each arg | Raw digest **bytes**, not hex. | `tests/ngx_codec.rs` (`hash_and_hmac_use_fixed_vectors`) |
| `ngx.md5` | `ngx::md5(data: string) -> string` | supported | `PHASE_CODEC_MISC_SSL_CLIENT_HELLO` | input cap | Lowercase hex string. | `tests/ngx_codec.rs` (`hash_and_hmac_use_fixed_vectors`) |
| `ngx.md5_bin` | `ngx::md5_bin(data: string) -> bytes` | adapted | `PHASE_CODEC_MISC_SSL_CLIENT_HELLO` | input cap | Raw 16-byte digest. | `tests/ngx_codec.rs` (`hash_and_hmac_use_fixed_vectors`) |
| `ngx.sha1_bin` | `ngx::sha1_bin(data: string) -> bytes` | adapted | `PHASE_CODEC_MISC_SSL_CLIENT_HELLO` | input cap | Raw 20-byte digest. | `tests/ngx_codec.rs` (`hash_and_hmac_use_fixed_vectors`) |
| `ngx.quote_sql_str` | `ngx::quote_sql_str(value: string) -> string` | supported | `PHASE_CODEC_MISC_SSL_CLIENT_HELLO` | input/output caps | MySQL literal quoting (`\0 \b \n \r \t \Z \\ ' "`). | `tests/ngx_codec.rs` (`quote_sql_str_uses_mysql_escapes`) |

Oversized codec input is rejected without huge allocation:
`tests/ngx_codec.rs` `codec_rejects_oversized_input_without_huge_allocation`.


---

See also: [API index](index.md) · [Phase sets](index.md#phase-sets) · [Cross-cutting adaptations](index.md#cross-cutting-adaptations) · [Hard caps](index.md#hard-caps)
