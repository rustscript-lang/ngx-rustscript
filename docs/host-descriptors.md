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

Timer `at`/`every` use `HostAdapterDescriptor::Owned`. Request, pool, and
timer VMs install through `ngx_host_registry()` (restricted transactional
exact catalog). `bind_ngx_hosts` is a thin wrapper around that registry
and is not a second name/arity binding authority.

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
(borrow / borrow-mut / take / create). Request-local `ngx::ctx` is not a
VM resource: payloads stay request-owned maps.

Sync methods that do not need `&mut Vm` take `ResourceRef` (shared-dict
ops, `ngx::semaphore::{post,count}`). Constructors still return a raw
guest token because `push_resource` requires `&mut Vm`. Async methods and
`close` keep a raw guest token: `ResourceRef` cannot cross suspension,
and close cannot combine `&mut Vm` with a borrowed resource. Those
slots still advertise `Resource` + passing mode on the catalog contract.

## Named structs

Fixed envelopes are catalog `HostTypeSchema::Named` and Rust
`HostNamedStruct` types. Runtime values remain maps.

| Struct | Functions |
| --- | --- |
| `DecodedArgsResult` | `ngx::decode_args` |
| `RequestHeadersResult` | `ngx::req::get_headers` |
| `RequestArgsResult` | `ngx::req::get_uri_args`, `ngx::req::get_post_args` |
| `SharedGetResult` | `ngx::shared::{get,get_stale,lpop,rpop}` |
| `SharedStoreResult` | `ngx::shared::{set,add,replace}` |
| `SharedSafeStoreResult` | `ngx::shared::{safe_set,safe_add,expire}` |
| `SharedIncrResult` | `ngx::shared::incr` |
| `SharedNumberResult` | `ngx::shared::{ttl,lpush,rpush,llen}` |

Guest scripts use the host named types. Shared dictionary values are tagged
`SharedValue` records and absence is `None`.

## Intentional dynamic parameters

Only host **parameters** may stay `HostTypeSchema::Unknown`; they are listed
exactly in `tests/host_catalog.rs`
(`exact_dynamic_unknown_and_map_slots_are_allowlisted`). Typical reasons:

- optional OpenResty trailing arguments (`()` or several runtime shapes)
- open dictionaries (`ngx::encode_args` `args`, header-set maps)
- mixed printable or buffer payloads (`print`, `say`, socket sends)

No host **output** may carry `Unknown`, `Null`, or the legacy `Optional`
schema: RustScript rejects such a return at compile time. Represent absence
with `Option<T>`, a void result with `()`, a fixed shape with a named struct,
and an open-keyed bag with a typed entry array. Add a named struct, bump
`NGX_HOST_CATALOG_SCHEMA_VERSION`, and update the fingerprint goldens.
