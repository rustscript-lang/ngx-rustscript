//! Production host bindings via `pd-host-function`.
//!
//! The pinned macro only accepts `name = "..."` and emits wrappers that resolve
//! `super::{borrow_arg, CallOutcome, ...}` and `super::super::{Vm, Value, VmResult}`.
//! Nested modules below match that layout. There is no handwritten
//! `HostFunction`, argument decode, or yield-based host re-entry.

use vm::Vm;

pub mod catalog;
pub mod context;

pub use catalog::{
    HostCapability, HostFamily, HostManifestEntry, HostSyncKind, NGX_SLEEP_MS_NAME, NGX_SLEEP_NAME,
    NgxPhase, host_allowed_in_phase, manifest_entry, ngx_host_catalog, ngx_host_manifest,
};
pub use context::{
    AllowedOps, RequestExecutionScope, ScopeError, current_allowed_ops, current_identity,
    current_phase, is_active, with_current_request, with_current_request_mut,
};

/// Names bound by [`bind_ngx_hosts`]. Kept next to the explicit bind list so
/// catalog correspondence can be tested without generating dispatch from
/// catalog metadata.
pub fn production_runtime_host_names() -> &'static [&'static str] {
    &[NGX_SLEEP_NAME, NGX_SLEEP_MS_NAME]
}

/// Bind every production host using the macro-generated wrapper.
pub fn bind_ngx_hosts(vm: &mut Vm) {
    vm.bind_static_function(NGX_SLEEP_NAME, layout::helpers::functions::sleep);
    vm.bind_static_function(NGX_SLEEP_MS_NAME, layout::helpers::functions::sleep_ms);
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
        #[allow(unused_imports)]
        pub use super::VmResult;
        #[allow(unused_imports)]
        pub use vm::{
            CallOutcome, CaptureAsyncHostContext, FromVmValue, HostFutureOutput,
            IntoHostCallOutcome, VmError, borrow_arg,
        };

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
            #[pd_host_function(name = "ngx::sleep")]
            pub async fn sleep(seconds: NumberValue) -> VmResult<()> {
                let duration = duration_from_seconds(seconds)?;
                tokio::time::sleep(duration).await;
                Ok(())
            }

            /// Sleeps for `ms` milliseconds and returns `null`.
            ///
            /// `ms` must be a nonnegative integer. The generated wrapper submits
            /// an owned host future; this body only validates and awaits Tokio.
            #[pd_host_function(name = "ngx::sleep_ms")]
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
        assert_eq!(schema.params[0].name, "seconds");
        assert_eq!(schema.params[0].ty, HostTypeSchema::Number);
        assert_eq!(schema.return_type, HostTypeSchema::Null);
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
