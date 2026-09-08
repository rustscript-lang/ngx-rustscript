//! Production host bindings via `pd-host-function`.
//!
//! The pinned macro only accepts `name = "..."` and emits wrappers that resolve
//! `super::{borrow_arg, CallOutcome, ...}` and `super::super::{Vm, Value, VmResult}`.
//! Nested modules below match that layout. There is no handwritten
//! `HostFunction`, argument decode, or yield-based host re-entry.

use vm::{HostApiBuilder, HostApiCatalog, HostFunctionSchema, HostParamSchema, HostTypeSchema, Vm};

/// Import name compiled into access-phase programs.
pub const NGX_SLEEP_NAME: &str = "ngx::sleep";

/// Compiler catalog for the production `ngx::sleep` import.
///
/// This is compiler import-allowlist metadata, not a runtime binding. The
/// pinned `pd-host-function` macro generates the wrapper, argument conversion,
/// and `submit_host_future` call; it does not emit catalog APIs. Bindings stay
/// exclusively macro-generated via [`bind_ngx_hosts`].
///
/// Parameter names and types match the `#[pd_host_function]` wrapper.
pub fn ngx_host_catalog() -> HostApiCatalog {
    let mut builder = HostApiBuilder::new();
    builder.function(HostFunctionSchema::with_return(
        NGX_SLEEP_NAME,
        vec![HostParamSchema::value("ms", HostTypeSchema::Int)],
        HostTypeSchema::Null,
    ));
    builder
        .build()
        .expect("ngx host catalog must be a valid HostApiCatalog")
}

/// Bind every production host using the macro-generated wrapper.
pub fn bind_ngx_hosts(vm: &mut Vm) {
    vm.bind_static_function(NGX_SLEEP_NAME, layout::helpers::functions::sleep);
}

#[cfg(test)]
pub(crate) fn bind_test_hosts(vm: &mut Vm) {
    bind_ngx_hosts(vm);
    vm.bind_static_function("ngx::probe", layout::helpers::functions::probe);
    vm.bind_static_function("ngx::now", layout::helpers::functions::now);
    vm.bind_static_function("ngx::fail", layout::helpers::functions::fail);
}

mod layout {
    pub use vm::{Value, Vm, VmResult};

    pub mod helpers {
        pub use super::VmResult;
        #[allow(unused_imports)]
        pub use vm::{
            CallOutcome, CaptureAsyncHostContext, HostFutureOutput, IntoHostCallOutcome, VmError,
            borrow_arg,
        };

        pub mod functions {
            use std::time::Duration;

            use super::*;
            use pd_host_function::pd_host_function;

            /// Sleeps for `ms` milliseconds and returns `null`.
            ///
            /// `ms` must be a nonnegative integer. The generated wrapper submits
            /// an owned host future; this body only validates and awaits Tokio.
            #[pd_host_function(name = "ngx::sleep")]
            pub async fn sleep(ms: i64) -> VmResult<()> {
                if ms < 0 {
                    return Err(VmError::HostError(format!(
                        "ngx::sleep expects non-negative milliseconds, got {ms}"
                    )));
                }
                let ms = u64::try_from(ms).map_err(|_| {
                    VmError::HostError(
                        "ngx::sleep duration is not a valid millisecond count".into(),
                    )
                })?;
                tokio::time::sleep(Duration::from_millis(ms)).await;
                Ok(())
            }

            /// Test-only async host sharing the generic bridge.
            #[cfg(test)]
            #[pd_host_function(name = "ngx::probe")]
            pub async fn probe(ms: i64) -> VmResult<i64> {
                sleep_impl(ms).await?;
                Ok(ms)
            }

            /// Test-only immediately-ready async host sharing the generic bridge.
            #[cfg(test)]
            #[pd_host_function(name = "ngx::now")]
            pub async fn now() -> VmResult<i64> {
                Ok(7)
            }

            /// Test-only async host that fails after becoming ready.
            #[cfg(test)]
            #[pd_host_function(name = "ngx::fail")]
            pub async fn fail() -> VmResult<i64> {
                Err(VmError::HostError("ngx::fail".into()))
            }
        }
    }
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
        assert_eq!(schema.params[0].name, "ms");
        assert_eq!(schema.params[0].ty, HostTypeSchema::Int);
        assert_eq!(schema.return_type, HostTypeSchema::Null);
    }

    #[test]
    fn catalog_does_not_include_test_hosts() {
        let catalog = ngx_host_catalog();
        assert!(catalog.function("ngx::probe").is_none());
        assert!(catalog.function("ngx::now").is_none());
        assert!(catalog.function("ngx::fail").is_none());
    }
}
