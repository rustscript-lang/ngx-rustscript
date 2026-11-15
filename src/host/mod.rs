//! Production host bindings via `pd-host-function`.
//!
//! The pinned macro only accepts `name = "..."` and emits wrappers that resolve
//! `super::{borrow_arg, CallOutcome, ...}` and `super::super::{Vm, Value, VmResult}`.
//! Nested modules below match that layout. There is no handwritten
//! `HostFunction`, argument decode, or yield-based host re-entry.

use vm::{Vm, VmError};

pub mod catalog;
pub(crate) mod context;
pub(crate) mod standard;
pub(crate) mod timer;

pub use catalog::is_standard_host_import;
pub use catalog::{
    HostCapability, HostFamily, HostManifestEntry, HostSyncKind, LOG_ALLOWED_NAMES,
    NGX_COOKIE_TIME_NAME, NGX_CRC32_LONG_NAME, NGX_CRC32_SHORT_NAME, NGX_CTX_CLEAR_NAME,
    NGX_CTX_DELETE_NAME, NGX_CTX_GET_NAME, NGX_CTX_SET_NAME, NGX_DECODE_ARGS_NAME,
    NGX_DECODE_BASE64_NAME, NGX_DECODE_BASE64MIME_NAME, NGX_ENCODE_ARGS_NAME,
    NGX_ENCODE_BASE64_NAME, NGX_EOF_NAME, NGX_ESCAPE_URI_NAME, NGX_EXIT_NAME, NGX_FLUSH_NAME,
    NGX_HEADER_CLEAR_NAME, NGX_HEADER_GET_NAME, NGX_HEADER_SET_ALL_NAME, NGX_HEADER_SET_NAME,
    NGX_HEADERS_SENT_NAME, NGX_HMAC_SHA1_NAME, NGX_HOST_CATALOG_FINGERPRINT,
    NGX_HOST_CATALOG_FINGERPRINT_V1, NGX_HOST_CATALOG_FINGERPRINT_V2,
    NGX_HOST_CATALOG_SCHEMA_VERSION, NGX_HTTP_TIME_NAME, NGX_IS_SUBREQUEST_NAME,
    NGX_LOCALTIME_NAME, NGX_MD5_BIN_NAME, NGX_MD5_NAME, NGX_NOW_NAME, NGX_PARSE_HTTP_TIME_NAME,
    NGX_PRINT_NAME, NGX_QUOTE_SQL_STR_NAME, NGX_REDIRECT_NAME, NGX_REQ_APPEND_BODY_NAME,
    NGX_REQ_CLEAR_HEADER_NAME, NGX_REQ_DISCARD_BODY_NAME, NGX_REQ_FINISH_BODY_NAME,
    NGX_REQ_GET_BODY_DATA_NAME, NGX_REQ_GET_BODY_FILE_NAME, NGX_REQ_GET_HEADERS_NAME,
    NGX_REQ_GET_METHOD_NAME, NGX_REQ_GET_POST_ARGS_NAME, NGX_REQ_GET_URI_ARGS_NAME,
    NGX_REQ_HTTP_VERSION_NAME, NGX_REQ_INIT_BODY_NAME, NGX_REQ_IS_INTERNAL_NAME,
    NGX_REQ_RAW_HEADER_NAME, NGX_REQ_READ_BODY_NAME, NGX_REQ_SET_BODY_DATA_NAME,
    NGX_REQ_SET_BODY_FILE_NAME, NGX_REQ_SET_HEADER_NAME, NGX_REQ_SET_METHOD_NAME,
    NGX_REQ_SET_URI_ARGS_NAME, NGX_REQ_SET_URI_NAME, NGX_REQ_START_TIME_NAME, NGX_SAY_NAME,
    NGX_SEMAPHORE_COUNT_NAME, NGX_SEMAPHORE_NEW_NAME, NGX_SEMAPHORE_POST_NAME,
    NGX_SEMAPHORE_RESOURCE_KEY, NGX_SEMAPHORE_WAIT_NAME, NGX_SEND_HEADERS_NAME, NGX_SHA1_BIN_NAME,
    NGX_SHARED_ADD_NAME, NGX_SHARED_CAPACITY_NAME, NGX_SHARED_DELETE_NAME, NGX_SHARED_DICT_NAME,
    NGX_SHARED_EXPIRE_NAME, NGX_SHARED_FLUSH_ALL_NAME, NGX_SHARED_FLUSH_EXPIRED_NAME,
    NGX_SHARED_FREE_SPACE_NAME, NGX_SHARED_GET_KEYS_NAME, NGX_SHARED_GET_NAME,
    NGX_SHARED_GET_STALE_NAME, NGX_SHARED_INCR_NAME, NGX_SHARED_LLEN_NAME, NGX_SHARED_LPOP_NAME,
    NGX_SHARED_LPUSH_NAME, NGX_SHARED_REPLACE_NAME, NGX_SHARED_RPOP_NAME, NGX_SHARED_RPUSH_NAME,
    NGX_SHARED_SAFE_ADD_NAME, NGX_SHARED_SAFE_SET_NAME, NGX_SHARED_SET_NAME, NGX_SHARED_TTL_NAME,
    NGX_SLEEP_MS_NAME, NGX_SLEEP_NAME, NGX_SOCKET_CONNECT_NAME, NGX_SOCKET_TCP_CLIENTCERT_NAME,
    NGX_SOCKET_TCP_CLOSE_NAME, NGX_SOCKET_TCP_CONNECT_NAME, NGX_SOCKET_TCP_GETREUSEDTIMES_NAME,
    NGX_SOCKET_TCP_NAME, NGX_SOCKET_TCP_RECEIVE_NAME, NGX_SOCKET_TCP_RECEIVEANY_NAME,
    NGX_SOCKET_TCP_RECEIVEUNTIL_NAME, NGX_SOCKET_TCP_RESOURCE_KEY, NGX_SOCKET_TCP_SEND_NAME,
    NGX_SOCKET_TCP_SETCLIENTCERT_NAME, NGX_SOCKET_TCP_SETKEEPALIVE_NAME,
    NGX_SOCKET_TCP_SETTIMEOUT_NAME, NGX_SOCKET_TCP_SETTRUSTEDSTORE_NAME,
    NGX_SOCKET_TCP_SSLHANDSHAKE_NAME, NGX_SOCKET_TCP_TRUSTEDSTORE_NAME,
    NGX_SOCKET_TLS_CLIENTCERT_RESOURCE_KEY, NGX_SOCKET_TLS_TRUSTEDSTORE_RESOURCE_KEY,
    NGX_SOCKET_UDP_BIND_NAME, NGX_SOCKET_UDP_CLOSE_NAME, NGX_SOCKET_UDP_NAME,
    NGX_SOCKET_UDP_RECEIVE_NAME, NGX_SOCKET_UDP_RESOURCE_KEY, NGX_SOCKET_UDP_SEND_NAME,
    NGX_SOCKET_UDP_SETPEERNAME_NAME, NGX_SOCKET_UDP_SETTIMEOUT_NAME, NGX_STATUS_GET_NAME,
    NGX_STATUS_SET_NAME, NGX_TIME_NAME, NGX_TODAY_NAME, NGX_UNESCAPE_URI_NAME,
    NGX_UPDATE_TIME_NAME, NGX_UTCTIME_NAME, NGX_VAR_GET_NAME, NGX_VAR_SET_NAME, NgxPhase,
    TIMER_CALLBACK_DENIED_CAPABILITIES, TIMER_CALLBACK_PHASE, host_allowed_in_phase,
    host_allowed_in_timer_callback, host_validates_for_log, log_allowlist_accepts, manifest_entry,
    ngx_host_catalog, ngx_host_manifest, timer_callback_schema,
};

pub use catalog::{
    NGX_TIMER_AT_NAME, NGX_TIMER_EVERY_NAME, NGX_TIMER_PENDING_COUNT_NAME,
    NGX_TIMER_RUNNING_COUNT_NAME,
};

pub use context::{
    AllowedOps, RequestDisposition, ScopeError, VmEntryObservation, capture_vm_entries,
    current_allowed_ops, current_identity, current_phase, current_request_live, is_active,
    with_current_ctx, with_current_ctx_mut, with_current_disposition_mut, with_current_request,
    with_current_request_and_disposition_mut, with_current_request_mut,
    with_current_request_mut_ops, with_current_request_ops, with_request_scope,
    with_request_scope_ctx, with_request_scope_full, with_vm_request_scope,
    with_vm_request_scope_ctx, with_vm_request_scope_full,
};
#[cfg(test)]
pub(crate) use layout::helpers::request_ctx::live_owned_entry_count;
pub use layout::helpers::request_ctx::{
    MAX_CTX_CONTAINER_ELEMS, MAX_CTX_DEPTH, MAX_CTX_KEY_BYTES, MAX_CTX_KEYS, MAX_CTX_SCALAR_BYTES,
    MAX_CTX_TOTAL_BYTES, RequestLocalCtx,
};
pub use standard::{StandardHostPolicy, install_standard_policy, install_standard_runtime};

/// Override the OS clock sampled by time hosts. Used by tests and by
/// `notify_cached_clock_event_boundary` to model nginx's event-loop refresh
/// without sleeping.
pub fn override_os_clock(sec: i64, msec: u32) {
    layout::helpers::time::override_os_clock(sec, msec);
}

/// Clear a previous [`override_os_clock`] so the next snapshot reads the OS.
pub fn clear_os_clock_override() {
    layout::helpers::time::clear_os_clock_override();
}

/// Mark an event/request-like boundary so the cached clock is eligible to
/// refresh, matching nginx `ngx_time_update` on the event loop.
pub fn notify_cached_clock_event_boundary() {
    layout::helpers::time::notify_cached_clock_event_boundary();
}

pub(crate) use layout::helpers::body::BodyMutationState;
pub(crate) use layout::helpers::output::OutputState;

pub fn finish_response(raw: *mut nginx_sys::ngx_http_request_t) -> nginx_sys::ngx_int_t {
    layout::helpers::output::finish_response(raw)
}

pub fn finish_response_on_error(raw: *mut nginx_sys::ngx_http_request_t) -> nginx_sys::ngx_int_t {
    layout::helpers::output::finish_response_on_error(raw)
}

pub fn finish_response_on_finalize(
    raw: *mut nginx_sys::ngx_http_request_t,
) -> nginx_sys::ngx_int_t {
    layout::helpers::output::finish_response_on_finalize(raw)
}

pub(crate) fn cleanup_output_slot(
    raw: *mut nginx_sys::ngx_http_request_t,
    slot: &mut Option<OutputState>,
) {
    layout::helpers::output::cleanup_output_slot(raw, slot)
}
pub fn cleanup_output(raw: *mut nginx_sys::ngx_http_request_t) {
    layout::helpers::output::cleanup_output(raw)
}

/// Test hook: run the request-level flush write callback.
pub fn fire_flush_write_handler(raw: *mut nginx_sys::ngx_http_request_t) {
    layout::helpers::output::fire_flush_write_handler(raw)
}

/// VM-stop sentinel used by `ngx::exit` / `ngx::redirect`.
///
/// The pinned VM has no control-flow error variant, so AccessExec uses this
/// private HostError text plus a request-owned non-Continue disposition.
/// AccessExec consumes the sentinel only when `disposition != Continue`.
/// An arbitrary HostError with identical or user-shaped text is treated as a
/// normal host failure (500) unless that disposition is already set.
/// Ordinary HostError cannot forge a disposition.
pub(crate) const CONTROL_STOP_MESSAGE: &str = "\u{1}ngx::control-stop";

pub(crate) fn control_stop_error() -> VmError {
    VmError::HostError(CONTROL_STOP_MESSAGE.to_string())
}

pub(crate) fn is_control_stop(err: &VmError) -> bool {
    matches!(err, VmError::HostError(message) if message == CONTROL_STOP_MESSAGE)
}

/// Production ngx host names, in catalog/manifest order.
///
/// This is the single name list for the descriptor registry. Every entry is
/// installed by [`ngx_host_registry`] through exact catalog adapters.
pub fn production_runtime_host_names() -> &'static [&'static str] {
    static NAMES: std::sync::OnceLock<Vec<&'static str>> = std::sync::OnceLock::new();
    NAMES
        .get_or_init(|| {
            catalog::ngx_host_manifest()
                .iter()
                .map(|entry| entry.name)
                .collect()
        })
        .as_slice()
}

/// Compiler catalog for production ngx scripts.
///
/// It carries the ngx host surface plus pd-vm's standard host catalog (I/O,
/// HTTP, and SQLite). The standard families must be part of the *compile*
/// catalog: their handles are resources, and a name that the compiler resolves
/// through the legacy builtin table instead would carry an `int` carrier that
/// the resource-typed sibling hosts reject. The same catalog is what
/// [`install_standard_runtime`] installs at runtime through
/// `standard_composition()`, so compile-time and registered schemas agree.
pub fn compile_host_catalog() -> vm::HostApiCatalog {
    static CATALOG: std::sync::OnceLock<vm::HostApiCatalog> = std::sync::OnceLock::new();
    CATALOG
        .get_or_init(|| {
            let mut builder = vm::HostApiBuilder::new();
            for catalog in [
                catalog::ngx_host_catalog(),
                (*vm::standard_host_catalog()).clone(),
            ] {
                for resource in catalog.resources() {
                    builder.resource(resource.clone());
                }
                for schema in catalog.structs() {
                    builder.named_struct(schema.clone());
                }
                for function in catalog.functions() {
                    builder.function(function.clone());
                }
            }
            builder
                .build()
                .expect("merged ngx + standard host catalog must validate")
        })
        .clone()
}

/// Bind the calling VM through the production descriptor registry.
///
/// Catalog-compiled programs resolve hosts by exact adapter. There is no
/// name/arity fallback table.
pub fn bind_production_hosts(vm: &mut Vm) -> Result<(), String> {
    ngx_host_registry()?
        .bind_vm_cached(vm)
        .map_err(|error| format!("bind ngx host registry: {error}"))
}

/// Bind the calling VM through [`ngx_host_registry`].
///
/// Tests and examples that model production use this wrapper; the only
/// binding authority is the restricted catalog registry.
pub fn bind_ngx_hosts(vm: &mut Vm) {
    bind_production_hosts(vm).expect("bind ngx host registry");
}

/// Names of the `ngx::timer` family.
///
/// These hosts are catalogued and installed by [`ngx_host_registry`] with
/// every other production ngx host. `ngx::timer::{at,every}` consume their
/// callback through `HostParamPassing::TakeOwned`.
pub fn ngx_timer_host_names() -> &'static [&'static str] {
    &[
        NGX_TIMER_AT_NAME,
        NGX_TIMER_EVERY_NAME,
        NGX_TIMER_PENDING_COUNT_NAME,
        NGX_TIMER_RUNNING_COUNT_NAME,
    ]
}

/// Restricted production registry for every catalogued ngx host.
///
/// Request VMs, pool VMs, and timer/callback VMs all bind through
/// [`vm::HostFunctionRegistry::bind_vm_cached`]. The registry is built once per
/// process and cloned per bind; a clone shares the immutable entry table,
/// capability profile, and standard-surface composition, and reuses its
/// host-binding plan cache. The same registry is handed to
/// `register_owned_timer` through `OwnedHostContext`.
pub fn ngx_host_registry() -> Result<vm::HostFunctionRegistry, String> {
    static REGISTRY: std::sync::OnceLock<Result<vm::HostFunctionRegistry, String>> =
        std::sync::OnceLock::new();
    REGISTRY.get_or_init(build_ngx_host_registry).clone()
}

fn build_ngx_host_registry() -> Result<vm::HostFunctionRegistry, String> {
    let catalog = compile_host_catalog();
    // Start empty and install ngx and standard adapters from the same merged
    // compile catalog, including HTTP, so imports and adapters share schemas.
    // The standard capability profile restricts which hosts are available.
    let mut registry = vm::HostFunctionRegistry::empty();
    registry.set_capability_profile(standard_capability_profile());
    registry.set_standard_composition(vm::standard_composition());
    registry
        .transactionally(|registry| {
            for module in ngx_host_modules() {
                module
                    .install_from_catalog(registry, &catalog)
                    .map_err(|error| {
                        vm::VmError::HostError(format!("install {}: {error}", module.name))
                    })?;
            }
            // Standard families (io/http/sqlite) are part of the compile
            // catalog, so their adapters must be installed from that same
            // snapshot: a compiled standard import carries the exact schema
            // fingerprint and has no name-only fallback.
            for module in vm::standard_catalog_modules() {
                module
                    .install_from_catalog(registry, &catalog)
                    .map_err(|error| {
                        vm::VmError::HostError(format!("install {}: {error}", module.name))
                    })?;
            }
            Ok(())
        })
        .map_err(|error| error.to_string())?;
    Ok(registry)
}

fn standard_capability_profile() -> vm::CapabilityProfile {
    let mut builder = vm::CapabilityProfile::builder();
    for catalog in [
        vm::http_host_catalog(),
        vm::io_host_catalog(),
        vm::sqlite_host_catalog(),
    ] {
        for function in catalog.functions() {
            builder = builder.allow_host_import(function.name.clone());
            if let Some(builtin) = vm::BuiltinFunction::from_namespaced_name(&function.name) {
                builder = builder.allow_builtin(builtin);
            }
        }
    }
    for spec in vm::builtin_namespace_specs() {
        for member in spec.members {
            let name = format!("{}::{}", spec.namespace, member.name);
            if let Some(builtin) = vm::BuiltinFunction::from_namespaced_name(&name) {
                builder = builder.allow_builtin(builtin);
            }
        }
    }
    builder.build()
}

#[cfg(test)]
pub(crate) fn test_hosts_module() -> vm::HostModuleDescriptor {
    layout::helpers::functions::test_hosts_module()
}

#[cfg(test)]
pub(crate) fn test_bind_catalog() -> vm::HostApiCatalog {
    let mut builder = vm::HostApiBuilder::new();
    let test_hosts = test_hosts_module().catalog().expect("test host catalog");
    for catalog in [
        ngx_host_catalog(),
        (*vm::standard_host_catalog()).clone(),
        test_hosts,
    ] {
        for resource in catalog.resources() {
            builder.resource(resource.clone());
        }
        for schema in catalog.structs() {
            builder.named_struct(schema.clone());
        }
        for function in catalog.functions() {
            builder.function(function.clone());
        }
    }
    builder.build().expect("merged ngx+test catalog")
}

#[cfg(test)]
pub(crate) fn bind_test_hosts(vm: &mut Vm) {
    let catalog = test_bind_catalog();
    let mut registry = vm::HostFunctionRegistry::empty();
    registry.set_capability_profile(standard_capability_profile());
    registry.set_standard_composition(vm::standard_composition());
    registry
        .transactionally(|registry| {
            for module in ngx_host_modules() {
                module
                    .install_from_catalog(registry, &catalog)
                    .map_err(|error| {
                        vm::VmError::HostError(format!("install {}: {error}", module.name))
                    })?;
            }
            for module in vm::standard_catalog_modules() {
                module
                    .install_from_catalog(registry, &catalog)
                    .map_err(|error| {
                        vm::VmError::HostError(format!("install {}: {error}", module.name))
                    })?;
            }
            test_hosts_module()
                .install_from_catalog(registry, &catalog)
                .map_err(|error| vm::VmError::HostError(format!("install ngx.test: {error}")))?;
            Ok(())
        })
        .expect("install test hosts");
    registry
        .bind_vm_cached(vm)
        .expect("bind test host registry");
}

mod layout {
    #[allow(unused_imports)]
    pub use vm::{Value, Vm, VmError, VmResult, resource};

    pub mod helpers {
        #[allow(unused_imports)]
        pub use super::VmResult;
        #[allow(unused_imports)]
        pub use vm::{
            CallOutcome, CaptureAsyncHostContext, FromVmValue, HostFutureOutput,
            IntoHostCallOutcome, VmError, arg, borrow_arg, resource, take_arg,
        };

        /// Canonical guest `Option::Some` carrier (`{ tag: true, payload }`).
        #[allow(dead_code)]
        pub fn option_some(value: impl Into<vm::Value>) -> vm::Value {
            vm::option::some_carrier(value.into())
        }

        /// Canonical guest `Option::None` carrier (`{ tag: false }`).
        #[allow(dead_code)]
        pub fn option_none() -> vm::Value {
            vm::option::none_carrier()
        }

        /// Guest `Option` of an optional value: `Some(payload)` or `None`.
        #[allow(dead_code)]
        pub fn option_value(value: Option<vm::Value>) -> vm::Value {
            match value {
                Some(value) => option_some(value),
                None => option_none(),
            }
        }

        pub mod codec {
            include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/host/codec.rs"));
        }
        pub mod time {
            include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/host/time.rs"));
        }
        pub mod meta {
            include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/host/meta.rs"));
        }
        pub mod log {
            include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/host/log.rs"));
        }
        pub mod request {
            include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/host/request.rs"));
        }
        pub mod body {
            include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/host/body.rs"));
        }
        pub mod variables {
            include!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/src/host/variables.rs"
            ));
        }
        pub mod response_meta {
            include!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/src/host/response_meta.rs"
            ));
        }
        pub mod request_ctx {
            include!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/src/host/request_ctx.rs"
            ));
        }
        pub mod shared_dict {
            include!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/src/host/shared_dict.rs"
            ));
        }
        pub mod socket {
            include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/host/socket.rs"));
        }
        pub mod semaphore {
            include!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/src/host/semaphore.rs"
            ));
        }
        pub mod output {
            include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/host/output.rs"));
        }

        pub mod functions {
            use std::time::Duration;

            use super::*;
            use pd_host_function::pd_host_function;
            use vm::{FromVmValue, Value};

            /// Integer or float number accepted by `ngx::sleep`.
            ///
            /// Named `NumberValue` so the pinned macro records a `number` schema
            /// label. pd-vm does not re-export its internal `NumberValue` type.
            #[derive(Clone, Copy, Debug)]
            pub(crate) enum NumberValue {
                Int(i64),
                Float(f64),
            }

            impl FromVmValue<'_> for NumberValue {
                fn from_vm_value(value: &Value, _label: &str) -> VmResult<Self> {
                    match value {
                        Value::Int(number) => Ok(Self::Int(*number)),
                        Value::Float(number) => Ok(Self::Float(*number)),
                        _ => Err(VmError::TypeMismatch("number")),
                    }
                }
            }

            fn duration_from_seconds(seconds: NumberValue) -> VmResult<Duration> {
                match seconds {
                    NumberValue::Int(value) => {
                        if value < 0 {
                            return Err(VmError::HostError(format!(
                                "ngx::sleep expects non-negative seconds, got {value}"
                            )));
                        }
                        Ok(Duration::from_secs(value as u64))
                    }
                    NumberValue::Float(value) => {
                        if value.is_nan() {
                            return Err(VmError::HostError(
                                "ngx::sleep rejects NaN seconds".to_string(),
                            ));
                        }
                        if value.is_infinite() {
                            return Err(VmError::HostError(
                                "ngx::sleep rejects infinite seconds".to_string(),
                            ));
                        }
                        if value < 0.0 {
                            return Err(VmError::HostError(format!(
                                "ngx::sleep expects non-negative seconds, got {value}"
                            )));
                        }
                        Duration::try_from_secs_f64(value).map_err(|_| {
                            VmError::HostError("ngx::sleep duration overflowed".to_string())
                        })
                    }
                }
            }

            /// Sleeps for `seconds` and returns `null`.
            ///
            /// `seconds` is an OpenResty-style number (integer or float). The
            /// generated wrapper submits an owned host future; this body only
            /// validates and awaits Tokio.
            #[pd_host_function(name = "ngx::sleep", contract = crate::host::catalog::ngx_sleep_contract)]
            pub async fn sleep(seconds: NumberValue) -> VmResult<()> {
                let duration = duration_from_seconds(seconds)?;
                tokio::time::sleep(duration).await;
                Ok(())
            }

            /// Sleeps for `ms` milliseconds and returns `null`.
            ///
            /// `ms` must be a nonnegative integer. The generated wrapper submits
            /// an owned host future; this body only validates and awaits Tokio.
            #[pd_host_function(name = "ngx::sleep_ms", contract = crate::host::catalog::ngx_sleep_ms_contract)]
            pub async fn sleep_ms(ms: i64) -> VmResult<()> {
                if ms < 0 {
                    return Err(VmError::HostError(format!(
                        "ngx::sleep_ms expects non-negative milliseconds, got {ms}"
                    )));
                }
                let ms = u64::try_from(ms).map_err(|_| {
                    VmError::HostError(
                        "ngx::sleep_ms duration is not a valid millisecond count".into(),
                    )
                })?;
                tokio::time::sleep(Duration::from_millis(ms)).await;
                Ok(())
            }

            /// Test-only async host sharing the generic bridge.
            #[cfg(test)]
            #[pd_host_function(name = "ngx::probe")]
            pub async fn probe(ms: i64) -> VmResult<i64> {
                sleep_ms_impl(ms).await?;
                Ok(ms)
            }

            /// Test-only immediately-ready async host. Named `ngx::ready` so it
            /// cannot shadow production `ngx::now`.
            #[cfg(test)]
            #[pd_host_function(name = "ngx::ready")]
            pub async fn ready() -> VmResult<i64> {
                Ok(7)
            }

            /// Test-only host whose future is pending on its first poll and
            /// immediately ready when the driver arms the production waker.
            #[cfg(test)]
            #[pd_host_function(name = "ngx::pending_ready")]
            pub async fn pending_ready() -> VmResult<i64> {
                let mut first = true;
                std::future::poll_fn(move |_cx| {
                    if first {
                        first = false;
                        std::task::Poll::Pending
                    } else {
                        std::task::Poll::Ready(Ok(7))
                    }
                })
                .await
            }

            /// Test-only async host that fails after becoming ready.
            #[cfg(test)]
            #[pd_host_function(name = "ngx::fail")]
            pub async fn fail() -> VmResult<i64> {
                Err(VmError::HostError("ngx::fail".into()))
            }

            const FUNCTIONS_HOST_MODULE_FUNCTIONS: &[fn() -> vm::HostFunctionDescriptor] =
                &[sleep_descriptor, sleep_ms_descriptor];

            pub fn functions_host_module() -> vm::HostModuleDescriptor {
                vm::HostModuleDescriptor {
                    name: "ngx.sleep",
                    functions: FUNCTIONS_HOST_MODULE_FUNCTIONS,
                    resources: &[],
                }
            }

            #[cfg(test)]
            const TEST_HOST_MODULE_FUNCTIONS: &[fn() -> vm::HostFunctionDescriptor] = &[
                probe_descriptor,
                ready_descriptor,
                pending_ready_descriptor,
                fail_descriptor,
            ];

            #[cfg(test)]
            pub fn test_hosts_module() -> vm::HostModuleDescriptor {
                vm::HostModuleDescriptor {
                    name: "ngx.test",
                    functions: TEST_HOST_MODULE_FUNCTIONS,
                    resources: &[],
                }
            }
        }
    }
}

/// Deterministic ngx host module composition.
///
/// Catalog schemas, exact adapters, resource types, and named structs are
/// derived from these descriptors. `bind_ngx_hosts` is a thin wrapper around
/// [`ngx_host_registry`] so request, pool, and timer VMs share one restricted
/// exact-descriptor install.
pub fn ngx_host_modules() -> Vec<vm::HostModuleDescriptor> {
    vec![
        layout::helpers::functions::functions_host_module(),
        layout::helpers::codec::codec_host_module(),
        layout::helpers::time::time_host_module(),
        layout::helpers::meta::meta_host_module(),
        layout::helpers::log::log_host_module(),
        layout::helpers::request::request_host_module(),
        layout::helpers::body::body_host_module(),
        layout::helpers::variables::variables_host_module(),
        layout::helpers::response_meta::response_meta_host_module(),
        layout::helpers::request_ctx::request_ctx_host_module(),
        layout::helpers::shared_dict::shared_dict_host_module(),
        layout::helpers::socket::socket_host_module(),
        layout::helpers::semaphore::semaphore_host_module(),
        layout::helpers::output::output_host_module(),
        crate::host::timer::timer_host_module(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use vm::HostTypeSchema;

    #[test]
    fn catalog_matches_macro_sleep_schema() {
        let catalog = ngx_host_catalog();
        let schema = catalog
            .function(NGX_SLEEP_NAME)
            .expect("ngx::sleep is catalogued");
        assert_eq!(schema.name, NGX_SLEEP_NAME);
        assert_eq!(schema.params.len(), 1);
        assert_eq!(schema.params[0].name, "seconds");
        assert_eq!(schema.params[0].ty, HostTypeSchema::Number);
        assert_eq!(schema.return_type, HostTypeSchema::Unit);
    }

    #[test]
    fn catalog_matches_macro_sleep_ms_schema() {
        let catalog = ngx_host_catalog();
        let schema = catalog
            .function(NGX_SLEEP_MS_NAME)
            .expect("ngx::sleep_ms is catalogued");
        assert_eq!(schema.name, NGX_SLEEP_MS_NAME);
        assert_eq!(schema.params.len(), 1);
        assert_eq!(schema.params[0].name, "ms");
        assert_eq!(schema.params[0].ty, HostTypeSchema::Int);
        assert_eq!(schema.return_type, HostTypeSchema::Unit);
    }

    #[test]
    fn catalog_does_not_include_test_hosts() {
        let catalog = ngx_host_catalog();
        assert!(catalog.function("ngx::probe").is_none());
        assert!(catalog.function("ngx::fail").is_none());
        assert!(catalog.function("ngx::ready").is_none());
        assert!(catalog.function(NGX_NOW_NAME).is_some());
    }

    #[test]
    fn control_stop_sentinel_requires_non_continue_disposition() {
        let err = control_stop_error();
        assert!(is_control_stop(&err));
        let forged = VmError::HostError(CONTROL_STOP_MESSAGE.to_string());
        assert!(is_control_stop(&forged));
        let shaped = VmError::HostError("ngx::control-stop".into());
        assert!(!is_control_stop(&shaped));
        assert!(!is_control_stop(&VmError::HostError("boom".into())));
        let continue_disp = RequestDisposition::Continue;
        assert!(!(is_control_stop(&err) && continue_disp != RequestDisposition::Continue));
        let finalize = RequestDisposition::Finalize(403);
        assert!(is_control_stop(&err) && finalize != RequestDisposition::Continue);
    }
}
