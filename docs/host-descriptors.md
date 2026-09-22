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

Timer `at`/`every` use `HostAdapterDescriptor::Owned`. The restricted
`ngx_host_registry()` is the production descriptor source. At configuration
time, `PreparedScript::prepare` binds the compiled program once into an
`Arc<BoundHostProgram>`. Configured request and pool VMs instantiate from it
through `PreparedScript::new_vm()` -> `Vm::new_bound`; owned timer dispatch
uses the same bound artifact. The registry's `bind_vm_cached` helper is gated
to test/integration fixtures and is not a configured production path.

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

Guest scripts use the host named types. Inner `value` fields on shared
results stay `Unknown` because stored dictionary values are heterogeneous.

## Intentional dynamic slots

Positions that stay `HostTypeSchema::Unknown` or `Map(Unknown)` are
listed exactly in `tests/host_catalog.rs`
(`intentional_dynamic_slots_match_the_allowlist`). Typical reasons:

- optional trailing OpenResty arguments (`null` or several runtime shapes)
- open dictionaries (`ngx::encode_args` `args`, header-set maps, ctx payloads)
- heterogeneous stored shared-dict values and `ngx::header::get` (string,
  string array, or null)

Do not add a new `Unknown` or `Map(Unknown)` return for a fixed outer
shape. Add a named struct, bump `NGX_HOST_CATALOG_SCHEMA_VERSION`, and
update the fingerprint goldens.
