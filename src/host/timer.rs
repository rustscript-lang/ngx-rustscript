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
    CallOutcome, CallReturn, HostAdapterDescriptor, HostBindingDescriptor, HostBindingKind,
    HostFunctionDescriptor, HostFunctionRegistry, HostModuleDescriptor, HostOwnedAdapterFactory,
    HostOwnedFunction, OwnedHostCall, OwnedHostContext, TIMER_CALLBACK_ARG, Value, Vm, VmError,
    VmResult, installed_timer_counts, register_owned_timer,
};

use crate::host::catalog::{NGX_TIMER_AT_NAME, NGX_TIMER_EVERY_NAME};

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

struct TimerRegisterFactory {
    name: &'static str,
    repeating: bool,
}

impl HostOwnedAdapterFactory for TimerRegisterFactory {
    fn create(&self, context: OwnedHostContext<'_>) -> Box<dyn HostOwnedFunction> {
        Box::new(TimerRegister {
            registry: context.registry().clone(),
            name: self.name,
            repeating: self.repeating,
        })
    }
}

static TIMER_AT_FACTORY: TimerRegisterFactory = TimerRegisterFactory {
    name: NGX_TIMER_AT_NAME,
    repeating: false,
};
static TIMER_EVERY_FACTORY: TimerRegisterFactory = TimerRegisterFactory {
    name: NGX_TIMER_EVERY_NAME,
    repeating: true,
};

fn timer_at_descriptor() -> HostFunctionDescriptor {
    let schema = crate::host::catalog::ngx_timer_at_contract();
    HostFunctionDescriptor {
        schema: schema.clone(),
        binding: HostBindingDescriptor {
            kind: HostBindingKind::Owned,
        },
        effects: Vec::new(),
        adapter: HostAdapterDescriptor::Owned(&TIMER_AT_FACTORY),
        resource_types: Vec::new(),
    }
}

fn timer_every_descriptor() -> HostFunctionDescriptor {
    let schema = crate::host::catalog::ngx_timer_every_contract();
    HostFunctionDescriptor {
        schema: schema.clone(),
        binding: HostBindingDescriptor {
            kind: HostBindingKind::Owned,
        },
        effects: Vec::new(),
        adapter: HostAdapterDescriptor::Owned(&TIMER_EVERY_FACTORY),
        resource_types: Vec::new(),
    }
}

fn timer_pending_count_descriptor() -> HostFunctionDescriptor {
    let schema = crate::host::catalog::ngx_timer_pending_count_contract();
    HostFunctionDescriptor {
        schema: schema.clone(),
        binding: HostBindingDescriptor {
            kind: HostBindingKind::Static,
        },
        effects: Vec::new(),
        adapter: HostAdapterDescriptor::Static(pending_count),
        resource_types: Vec::new(),
    }
}

fn timer_running_count_descriptor() -> HostFunctionDescriptor {
    let schema = crate::host::catalog::ngx_timer_running_count_contract();
    HostFunctionDescriptor {
        schema: schema.clone(),
        binding: HostBindingDescriptor {
            kind: HostBindingKind::Static,
        },
        effects: Vec::new(),
        adapter: HostAdapterDescriptor::Static(running_count),
        resource_types: Vec::new(),
    }
}

const TIMER_HOST_MODULE_FUNCTIONS: &[fn() -> HostFunctionDescriptor] = &[
    timer_at_descriptor,
    timer_every_descriptor,
    timer_pending_count_descriptor,
    timer_running_count_descriptor,
];

/// Explicit `ngx::timer` module: owned `at`/`every` plus static counters.
pub fn timer_host_module() -> HostModuleDescriptor {
    HostModuleDescriptor {
        name: "ngx.timer",
        functions: TIMER_HOST_MODULE_FUNCTIONS,
        resources: &[],
    }
}
