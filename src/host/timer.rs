// OpenResty-oriented `ngx::timer` host wrappers.
//
// Worker-owned scheduling, fake requests, callback VMs, and shutdown live in
// `crate::timer`. These hosts are thin adapters over pd-vm's public timer
// surface: they validate the seconds argument into a checked `Duration`, keep
// the family's own exact name and schema, and hand the owned call to
// `register_owned_timer`, which performs the callback type/provenance
// validation, the isolated callback VM, the admission limits, the backend
// registration, and the transactional rollback. No timer state is stored here.

use std::time::Duration;

use vm::{
    CallOutcome, CallReturn, HostApiCatalog, HostFunctionRegistry, HostImportSchema,
    HostOwnedFunction, OwnedHostCall, OwnedHostContext, TIMER_CALLBACK_ARG, Value, Vm, VmError,
    VmResult, installed_timer_counts, register_owned_timer,
};

use crate::host::catalog::{
    NGX_TIMER_AT_NAME, NGX_TIMER_EVERY_NAME, NGX_TIMER_PENDING_COUNT_NAME,
    NGX_TIMER_RUNNING_COUNT_NAME,
};

/// One `ngx::timer::{at,every}` registration adapter.
struct TimerRegister {
    registry: HostFunctionRegistry,
    name: &'static str,
    repeating: bool,
}

impl HostOwnedFunction for TimerRegister {
    fn call(&mut self, call: &mut OwnedHostCall<'_>) -> VmResult<CallOutcome> {
        let seconds = call.arg(0).ok_or_else(|| {
            VmError::HostError(format!("{} is missing its seconds argument", self.name))
        })?;
        let delay = checked_seconds(seconds, self.name, self.repeating)?;
        register_owned_timer(
            call,
            &self.registry,
            TIMER_CALLBACK_ARG,
            delay,
            self.repeating.then_some(delay),
        )
    }
}

/// Converts OpenResty seconds into a checked [`Duration`].
///
/// Integers and floats are accepted. Negative, NaN, infinite, and
/// non-representable values are rejected. A repeating registration
/// (`ngx::timer::every`) additionally requires a positive interval, matching
/// OpenResty's rejection of `ngx.timer.every(0, ...)`.
fn checked_seconds(value: &Value, host: &str, repeating: bool) -> VmResult<Duration> {
    let requirement = if repeating {
        "positive"
    } else {
        "non-negative"
    };
    let duration = match value {
        Value::Int(seconds) => {
            if *seconds < 0 {
                return Err(VmError::HostError(format!(
                    "{host} expects a {requirement} second duration, got {seconds}"
                )));
            }
            Duration::from_secs(*seconds as u64)
        }
        Value::Float(seconds) => {
            if seconds.is_nan() {
                return Err(VmError::HostError(format!("{host} rejects NaN seconds")));
            }
            if seconds.is_infinite() {
                return Err(VmError::HostError(format!(
                    "{host} rejects infinite seconds"
                )));
            }
            if *seconds < 0.0 {
                return Err(VmError::HostError(format!(
                    "{host} expects a {requirement} second duration, got {seconds}"
                )));
            }
            Duration::try_from_secs_f64(*seconds)
                .map_err(|_| VmError::HostError(format!("{host} duration overflowed")))?
        }
        other => {
            return Err(VmError::HostError(format!(
                "{host} expects a number of seconds, got {other:?}"
            )));
        }
    };
    if repeating && duration.is_zero() {
        return Err(VmError::HostError(format!(
            "{host} interval must be positive"
        )));
    }
    Ok(duration)
}

/// Reports one installed backend counter as an `int`.
fn timer_count(vm: &mut Vm, pending: bool) -> VmResult<CallOutcome> {
    let counts = installed_timer_counts(vm)?;
    let count = if pending {
        counts.pending
    } else {
        counts.running
    };
    let count = i64::try_from(count)
        .map_err(|_| VmError::HostError("ngx::timer count exceeds int range".to_string()))?;
    Ok(CallOutcome::Return(CallReturn::one(Value::Int(count))))
}

/// `ngx::timer::pending_count()`.
pub(crate) fn pending_count(vm: &mut Vm, _args: &[Value]) -> VmResult<CallOutcome> {
    timer_count(vm, true)
}

/// `ngx::timer::running_count()`.
pub(crate) fn running_count(vm: &mut Vm, _args: &[Value]) -> VmResult<CallOutcome> {
    timer_count(vm, false)
}

/// Registers the four `ngx::timer` hosts against `catalog`.
///
/// `at`/`every` transfer their callback through `HostParamPassing::TakeOwned`,
/// so they register as exact owned entries and can only be bound through the
/// registry path ([`crate::host::ngx_host_registry`]); the counters are exact
/// static hosts. Registration is transactional: a schema or entry failure
/// leaves `registry` unchanged.
pub fn register_timer_hosts(
    registry: &mut HostFunctionRegistry,
    catalog: &HostApiCatalog,
) -> Result<(), String> {
    let mut schemas: Vec<(&'static str, HostImportSchema)> = Vec::new();
    for name in [
        NGX_TIMER_AT_NAME,
        NGX_TIMER_EVERY_NAME,
        NGX_TIMER_PENDING_COUNT_NAME,
        NGX_TIMER_RUNNING_COUNT_NAME,
    ] {
        let found = vm::catalog_import_schemas(catalog, name);
        if found.is_empty() {
            return Err(format!("{name} is missing from the compile catalog"));
        }
        for schema in found {
            schemas.push((name, schema));
        }
    }
    registry
        .transactionally(|staged| {
            for (name, schema) in schemas {
                match name {
                    NGX_TIMER_AT_NAME | NGX_TIMER_EVERY_NAME => {
                        let repeating = name == NGX_TIMER_EVERY_NAME;
                        staged.register_exact_owned(
                            name,
                            2,
                            schema,
                            move |context: OwnedHostContext<'_>| -> Box<dyn HostOwnedFunction> {
                                Box::new(TimerRegister {
                                    registry: context.registry().clone(),
                                    name,
                                    repeating,
                                })
                            },
                        )?;
                    }
                    NGX_TIMER_PENDING_COUNT_NAME => {
                        staged.register_exact_static(name, 0, schema, pending_count)?;
                    }
                    NGX_TIMER_RUNNING_COUNT_NAME => {
                        staged.register_exact_static(name, 0, schema, running_count)?;
                    }
                    _ => {
                        return Err(VmError::HostError(format!("unexpected timer host {name}")));
                    }
                }
            }
            Ok(())
        })
        .map_err(|error| error.to_string())
}
