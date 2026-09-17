# ngx host descriptors

ngx-rustscript host functions are owned by explicit
`HostModuleDescriptor` values returned from `ngx_host_modules()`. Each
production function has one descriptor source for schema, binding,
adapter, guest resource effects, and private state.

## Authoring

1. Implement the host with `#[pd_host_function(name = "...", contract = crate::host::catalog::<name>_contract)]`.
2. Keep the guest schema in `src/host/catalog.rs` as a zero-argument
   `*_contract()` wrapper around `host_function_contract`.
3. List the generated `*_descriptor` factory on the family module slice
   (`codec_host_module`, `timer_host_module`, …).
4. Install through `HostModuleDescriptor::install_from_catalog`. Do not
   add parallel `HostApiBuilder::function` / `register_exact_*` loops.

Timer `at`/`every` use `HostAdapterDescriptor::Owned` so callback
transfer stays on the owned adapter path. `bind_ngx_hosts` remains a
name-binding compatibility path for tests that do not need owned timer
dispatch; it does not install those adapters.

## Typed resources

Handle types implement `HostResourceType` plus `HostResource`:

| Type | Key |
| --- | --- |
| `SharedDictHandle` | `ngx.shared.dict` |
| `SemaphoreHandle` | `ngx.semaphore` |
| `TcpCosocket` | `ngx.socket.tcp` |
| `UdpCosocket` | `ngx.socket.udp` |
| `TrustedStore` | `ngx.socket.tls.trustedstore` |
| `ClientCert` | `ngx.socket.tls.clientcert` |

Guest resource effects follow descriptor passing modes
(borrow / borrow-mut / take / create). Raw NGINX C ABI integers that
are not VM resources stay out of `Resource<T>`.

## Named structs

Fixed envelopes are catalog `HostTypeSchema::Named` and Rust
`HostNamedStruct` types. Runtime values remain maps.

| Struct | Functions |
| --- | --- |
| `DecodedArgsResult` | `ngx::decode_args` |
| `RequestHeadersResult` | `ngx::req::get_headers` |
| `RequestArgsResult` | `ngx::req::get_uri_args`, `ngx::req::get_post_args` |

This named-envelope conversion is a catalog fingerprint change versus
the previous `Unknown` returns. Guest scripts that already named those
structs keep compiling; inner `args`/`headers` maps stay dynamic.

Shared-dictionary multi-returns stay `Unknown` so guest code can bind
`SharedGetResult<T>` and the other generic result structs. That family
is an intentional dynamic exception, not a `Map(Unknown)` catalog type.

## Intentional dynamic slots

These positions stay `HostTypeSchema::Unknown` because OpenResty accepts
`null` or several runtime shapes at the same argument:

- optional trailing arguments (`max_args`, `max_headers`, `raw`,
  `no_padding`, `escape_type`, `timeout`, `exptime`, `flags`, `jump`,
  `auto_clean`, `init`, `init_ttl`, `count`, `max`, `host`, `data`,
  `value` where OpenResty is untyped)
- open dictionaries (`ngx::encode_args` `args`, header-set maps,
  request ctx payloads)
- shared-dictionary result carriers (generic `value` / `ok` / `error`
  envelopes)

Do not add new `Unknown` or `Map(Unknown)` returns for a fixed outer
shape. Add a named struct and a catalog test instead.
