//! `ngx::sleep` host contract and request-local Yield re-entry state.
//!
//! Locked against pd-vm `4b749862ff1632ef1266d75709da3676de37356c`:
//! - compile via `compile_source_with_flavor_and_options` plus a
//!   [`HostApiCatalog`] that declares `ngx::sleep(ms: int) -> null`
//! - bind with [`Vm::bind_args_function`] / [`HostArgsFunction`]
//! - first entry submits one logical operation and returns [`CallOutcome::Yield`]
//! - `Vm::resume` re-enters the same host call with the same arguments
//! - completion is consumed exactly once, then [`CallOutcome::Return`] of `null`
//!
//! Shared completion state lives on a cloneable [`SleepCoordinator`]. Each VM
//! or request owns a non-`Clone` [`SleepSession`] that holds the in-flight
//! token locally. A later request adapter can clone the coordinator without
//! sharing a current call slot. Completions are explicit; there is no Tokio,
//! eventfd, NGINX pointer, or process-global table. [`HostArgsFunction`]
//! requires `Send`, so the coordinator is `Arc<Mutex<_>>` rather than a
//! `Send` wrapper around NGINX data.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::sync::{Arc, Mutex};

use vm::{
    CallOutcome, CallReturn, HostApiBuilder, HostApiCatalog, HostArgsFunction, HostFunctionSchema,
    HostParamSchema, HostTypeSchema, Value, Vm, VmError, VmResult,
};

/// Catalog / import name for the sleep host.
pub const NGX_SLEEP_NAME: &str = "ngx::sleep";
/// Single integer milliseconds parameter.
pub const NGX_SLEEP_MS_PARAM: &str = "ms";

/// Compiler schema: one nonnegative `int` argument, `null` return.
pub fn sleep_function_schema() -> HostFunctionSchema {
    HostFunctionSchema::with_return(
        NGX_SLEEP_NAME,
        vec![HostParamSchema::value(
            NGX_SLEEP_MS_PARAM,
            HostTypeSchema::Int,
        )],
        HostTypeSchema::Null,
    )
}

/// Catalog containing only [`NGX_SLEEP_NAME`]. Config still rejects this import.
pub fn ngx_sleep_catalog() -> HostApiCatalog {
    let mut builder = HostApiBuilder::new();
    builder.function(sleep_function_schema());
    builder
        .build()
        .expect("ngx::sleep catalog is a valid host API")
}

/// Generation plus operation identity for one in-flight sleep.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SleepToken {
    generation: u64,
    op_id: u64,
}

impl SleepToken {
    /// Assemble a token. Production callers receive tokens from
    /// [`SleepCoordinator::claim_submission`].
    pub const fn from_raw(generation: u64, op_id: u64) -> Self {
        Self { generation, op_id }
    }

    /// Session generation that issued this operation.
    pub const fn generation(self) -> u64 {
        self.generation
    }

    /// Coordinator-local operation identity. Never reused.
    pub const fn op_id(self) -> u64 {
        self.op_id
    }
}

/// Fail-closed completion / consumption errors for one [`SleepCoordinator`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SleepOpsError {
    UnknownOperation(SleepToken),
    NotCompleted(SleepToken),
    AlreadyCompleted(SleepToken),
    GenerationInvalid(SleepToken),
    OperationIdExhausted,
    Poisoned,
}

impl fmt::Display for SleepOpsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownOperation(token) => {
                write!(
                    f,
                    "unknown sleep operation generation {} id {}",
                    token.generation, token.op_id
                )
            }
            Self::NotCompleted(token) => write!(
                f,
                "sleep operation generation {} id {} is not completed",
                token.generation, token.op_id
            ),
            Self::AlreadyCompleted(token) => write!(
                f,
                "duplicate completion for sleep operation generation {} id {}",
                token.generation, token.op_id
            ),
            Self::GenerationInvalid(token) => {
                write!(f, "sleep generation {} is invalid", token.generation)
            }
            Self::OperationIdExhausted => write!(f, "sleep operation id space exhausted"),
            Self::Poisoned => write!(f, "sleep coordinator mutex is poisoned"),
        }
    }
}

impl std::error::Error for SleepOpsError {}

impl From<SleepOpsError> for VmError {
    fn from(err: SleepOpsError) -> Self {
        VmError::HostError(err.to_string())
    }
}

struct SleepOp {
    completed: bool,
}

struct CoordinatorInner {
    next_generation: u64,
    next_op_id: u64,
    live_generations: BTreeSet<u64>,
    ops: BTreeMap<SleepToken, SleepOp>,
    submissions: VecDeque<(SleepToken, i64)>,
    host_entries: usize,
    submission_count: usize,
    consumption_count: usize,
}

/// Cloneable thread-safe completion / claim handle shared by sessions.
#[derive(Clone)]
pub struct SleepCoordinator {
    inner: Arc<Mutex<CoordinatorInner>>,
}

impl SleepCoordinator {
    /// Empty coordinator. Generations and operation IDs start at 1.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(CoordinatorInner {
                next_generation: 0,
                next_op_id: 0,
                live_generations: BTreeSet::new(),
                ops: BTreeMap::new(),
                submissions: VecDeque::new(),
                host_entries: 0,
                submission_count: 0,
                consumption_count: 0,
            })),
        }
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, CoordinatorInner>, SleepOpsError> {
        self.inner.lock().map_err(|_| SleepOpsError::Poisoned)
    }

    /// Host entries, including invalid arguments and premature resumes.
    pub fn host_entry_count(&self) -> Result<usize, SleepOpsError> {
        Ok(self.lock()?.host_entries)
    }

    /// Logical operations submitted. Premature resume does not increment this.
    pub fn submission_count(&self) -> Result<usize, SleepOpsError> {
        Ok(self.lock()?.submission_count)
    }

    /// Completions consumed by a host re-entry.
    pub fn consumption_count(&self) -> Result<usize, SleepOpsError> {
        Ok(self.lock()?.consumption_count)
    }

    /// In-flight operations that have been submitted and not yet consumed or cancelled.
    pub fn operation_count(&self) -> Result<usize, SleepOpsError> {
        Ok(self.lock()?.ops.len())
    }

    /// One-time claim of the next queued `(token, ms)` submission.
    ///
    /// Duration is taken from the host binding's first entry, not from the VM
    /// stack. A claimed item is not returned again.
    pub fn claim_submission(&self) -> Result<Option<(SleepToken, i64)>, SleepOpsError> {
        Ok(self.lock()?.submissions.pop_front())
    }

    /// Mark `token` complete. Unknown, stale-generation, and duplicate
    /// completions fail closed.
    pub fn complete(&self, token: SleepToken) -> Result<(), SleepOpsError> {
        let mut inner = self.lock()?;
        match inner.generation_status(token.generation) {
            GenerationStatus::NeverIssued => return Err(SleepOpsError::UnknownOperation(token)),
            GenerationStatus::Invalid => return Err(SleepOpsError::GenerationInvalid(token)),
            GenerationStatus::Live => {}
        }
        let Some(op) = inner.ops.get_mut(&token) else {
            return Err(SleepOpsError::UnknownOperation(token));
        };
        if op.completed {
            return Err(SleepOpsError::AlreadyCompleted(token));
        }
        op.completed = true;
        Ok(())
    }

    fn allocate_generation(&self) -> Result<u64, SleepOpsError> {
        let mut inner = self.lock()?;
        let generation = inner
            .next_generation
            .checked_add(1)
            .ok_or(SleepOpsError::OperationIdExhausted)?;
        inner.next_generation = generation;
        inner.live_generations.insert(generation);
        Ok(generation)
    }

    fn note_host_entry(&self) -> Result<(), SleepOpsError> {
        let mut inner = self.lock()?;
        inner.host_entries = inner.host_entries.saturating_add(1);
        Ok(())
    }

    fn register_and_enqueue(&self, generation: u64, ms: i64) -> Result<SleepToken, SleepOpsError> {
        let mut inner = self.lock()?;
        if !inner.live_generations.contains(&generation) {
            return Err(SleepOpsError::GenerationInvalid(SleepToken::from_raw(
                generation, 0,
            )));
        }
        let op_id = inner
            .next_op_id
            .checked_add(1)
            .ok_or(SleepOpsError::OperationIdExhausted)?;
        inner.next_op_id = op_id;
        let token = SleepToken { generation, op_id };
        inner.ops.insert(token, SleepOp { completed: false });
        inner.submissions.push_back((token, ms));
        inner.submission_count = inner.submission_count.saturating_add(1);
        Ok(token)
    }

    fn query_reenter(&self, token: SleepToken) -> Result<Reenter, SleepOpsError> {
        let mut inner = self.lock()?;
        let completed = inner.ops.get(&token).map(|op| op.completed);
        match completed {
            Some(true) => {
                inner.ops.remove(&token);
                inner.submissions.retain(|(queued, _)| *queued != token);
                inner.consumption_count = inner.consumption_count.saturating_add(1);
                Ok(Reenter::Consumed)
            }
            Some(false) => Ok(Reenter::YieldAgain),
            None => Err(match inner.generation_status(token.generation) {
                GenerationStatus::Invalid => SleepOpsError::GenerationInvalid(token),
                GenerationStatus::Live | GenerationStatus::NeverIssued => {
                    SleepOpsError::UnknownOperation(token)
                }
            }),
        }
    }

    fn consume_host(&self, token: SleepToken) -> Result<(), SleepOpsError> {
        let mut inner = self.lock()?;
        match inner.generation_status(token.generation) {
            GenerationStatus::NeverIssued => return Err(SleepOpsError::UnknownOperation(token)),
            GenerationStatus::Invalid => return Err(SleepOpsError::GenerationInvalid(token)),
            GenerationStatus::Live => {}
        }
        let Some(op) = inner.ops.get(&token) else {
            return Err(SleepOpsError::UnknownOperation(token));
        };
        if !op.completed {
            return Err(SleepOpsError::NotCompleted(token));
        }
        inner.ops.remove(&token);
        inner.submissions.retain(|(queued, _)| *queued != token);
        inner.consumption_count = inner.consumption_count.saturating_add(1);
        Ok(())
    }

    fn invalidate_generation(&self, generation: u64) -> Result<(), SleepOpsError> {
        let mut inner = self.lock()?;
        inner.live_generations.remove(&generation);
        inner.ops.retain(|token, _| token.generation != generation);
        inner
            .submissions
            .retain(|(token, _)| token.generation != generation);
        Ok(())
    }

    /// Test hook: set the next operation id so the following submit can overflow.
    #[doc(hidden)]
    pub fn __test_set_next_op_id(&self, id: u64) -> Result<(), SleepOpsError> {
        self.lock()?.next_op_id = id;
        Ok(())
    }

    /// Test hook: set the next generation so the following session can overflow.
    #[doc(hidden)]
    pub fn __test_set_next_generation(&self, id: u64) -> Result<(), SleepOpsError> {
        self.lock()?.next_generation = id;
        Ok(())
    }

    /// Test hook: poison the coordinator mutex.
    #[doc(hidden)]
    pub fn __test_poison(&self) {
        let inner = Arc::clone(&self.inner);
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = inner.lock().expect("lock before test poison");
            panic!("test poison");
        }));
        std::panic::set_hook(previous);
    }
}

impl Default for SleepCoordinator {
    fn default() -> Self {
        Self::new()
    }
}

enum GenerationStatus {
    Live,
    Invalid,
    NeverIssued,
}

impl CoordinatorInner {
    fn generation_status(&self, generation: u64) -> GenerationStatus {
        if self.live_generations.contains(&generation) {
            GenerationStatus::Live
        } else if generation == 0 || generation > self.next_generation {
            GenerationStatus::NeverIssued
        } else {
            GenerationStatus::Invalid
        }
    }
}

enum Reenter {
    Consumed,
    YieldAgain,
}

/// Per-VM / per-request `ngx::sleep` binding. Not `Clone`.
pub struct SleepSession {
    coordinator: SleepCoordinator,
    generation: u64,
    current: Option<(SleepToken, i64)>,
}

impl SleepSession {
    /// Register a never-reused generation on `coordinator`.
    pub fn new(coordinator: &SleepCoordinator) -> Result<Self, SleepOpsError> {
        let generation = coordinator.allocate_generation()?;
        Ok(Self {
            coordinator: coordinator.clone(),
            generation,
            current: None,
        })
    }

    /// Host-only consume of the session's current operation.
    pub fn consume_current(&mut self) -> Result<(), SleepOpsError> {
        let Some((token, _)) = self.current else {
            return Err(SleepOpsError::UnknownOperation(SleepToken::from_raw(
                self.generation,
                0,
            )));
        };
        self.coordinator.consume_host(token)?;
        self.current = None;
        Ok(())
    }

    fn host_call(&mut self, args: &[Value]) -> VmResult<CallOutcome> {
        self.coordinator.note_host_entry()?;
        if let Some((token, _)) = self.current {
            return self.reenter(token);
        }
        let ms = parse_sleep_millis(args)?;
        let token = self.coordinator.register_and_enqueue(self.generation, ms)?;
        self.current = Some((token, ms));
        Ok(CallOutcome::Yield)
    }

    fn reenter(&mut self, token: SleepToken) -> VmResult<CallOutcome> {
        match self.coordinator.query_reenter(token)? {
            Reenter::YieldAgain => Ok(CallOutcome::Yield),
            Reenter::Consumed => {
                self.current = None;
                Ok(sleep_return())
            }
        }
    }
}

impl Drop for SleepSession {
    fn drop(&mut self) {
        let _ = self.coordinator.invalidate_generation(self.generation);
        self.current = None;
    }
}

impl HostArgsFunction for SleepSession {
    fn call(&mut self, args: &[Value]) -> VmResult<CallOutcome> {
        self.host_call(args)
    }
}

fn parse_sleep_millis(args: &[Value]) -> VmResult<i64> {
    match args {
        [Value::Int(millis)] if *millis >= 0 => Ok(*millis),
        [Value::Int(millis)] => Err(VmError::HostError(format!(
            "ngx::sleep expects non-negative milliseconds, got {millis}"
        ))),
        [_] => Err(VmError::HostError(
            "ngx::sleep expects one integer milliseconds argument".to_string(),
        )),
        _ => Err(VmError::HostError(format!(
            "ngx::sleep expects exactly one argument, got {}",
            args.len()
        ))),
    }
}

fn sleep_return() -> CallOutcome {
    CallOutcome::Return(CallReturn::one(Value::Null))
}

/// Bind [`NGX_SLEEP_NAME`] onto `vm` with a fresh [`SleepSession`].
pub fn bind_ngx_sleep(vm: &mut Vm, coordinator: &SleepCoordinator) -> Result<(), SleepOpsError> {
    let session = SleepSession::new(coordinator)?;
    vm.bind_args_function(NGX_SLEEP_NAME, Box::new(session));
    Ok(())
}
