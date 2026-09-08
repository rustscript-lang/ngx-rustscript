//! Request-local access execution state and worker-local token registry.
//!
//! NGINX 1.30.4 `ngx_http_core_access_phase` treats `NGX_AGAIN` as the wait
//! code: the checker returns `NGX_OK` and stops the phase engine without
//! advancing `phase_handler`. That path does not increment `r->main->count`;
//! the connection still owns the request. Resume continues the same VM, then
//! `ngx_http_core_run_phases` exactly once so the access handler can return
//! `NGX_OK` (or 500). Completions that arrive before the handler commits to
//! suspend are deferred and never resume the VM on the access stack.

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use ngx::core::Status;
use ngx::http::HTTPStatus;
use vm::{Program, Vm, VmStatus};

use crate::notify::DEFAULT_MAX_IN_FLIGHT;
use crate::sleep_binding::{NGX_SLEEP_NAME, SleepCoordinator, SleepSession, SleepToken};

/// Access-handler return mapped onto the phase engine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccessStatus {
    Ok,
    Again,
    Declined,
    Error500,
}

impl AccessStatus {
    pub fn as_nginx(self) -> Status {
        match self {
            Self::Ok => Status::NGX_OK,
            Self::Again => Status::NGX_AGAIN,
            Self::Declined => Status::NGX_DECLINED,
            Self::Error500 => HTTPStatus::INTERNAL_SERVER_ERROR.into(),
        }
    }
}

/// Result of delivering a ready [`SleepToken`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadyEffect {
    Ignored,
    Deferred,
    Halted,
    YieldedAgain,
    Failed,
}

/// Fail-closed registry errors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegistryError {
    Full,
    Duplicate,
}

impl fmt::Display for RegistryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Full => write!(f, "request sleep registry is full"),
            Self::Duplicate => write!(f, "sleep token is already registered"),
        }
    }
}

impl std::error::Error for RegistryError {}

/// Fail-closed submit errors matching Task 4 capacity / shutdown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubmitError {
    Full,
    Closed,
}

impl fmt::Display for SubmitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Full => write!(f, "sleep completion queue is full"),
            Self::Closed => write!(f, "worker runtime is shut down"),
        }
    }
}

impl std::error::Error for SubmitError {}

/// One-shot submit of a claimed `(token, ms)` pair.
pub trait SleepSubmit {
    fn submit(&mut self, token: SleepToken, ms: i64) -> Result<(), SubmitError>;
}

/// Recording NGINX action sink for unit tests.
#[derive(Default, Debug)]
pub struct RecordingSink {
    pub retains: usize,
    pub releases: usize,
    pub phase_continues: usize,
}

/// Apply phase-engine side effects after [`AccessExec::apply_ready`].
///
/// Halted and Failed schedule exactly one `ngx_http_core_run_phases`. Yield
/// wait uses `NGX_AGAIN` and does not retain (`r->main->count` stays 1).
pub fn apply_ready_effect(effect: ReadyEffect, sink: &mut RecordingSink) {
    match effect {
        ReadyEffect::Halted | ReadyEffect::Failed => {
            sink.phase_continues = sink.phase_continues.saturating_add(1);
        }
        ReadyEffect::Ignored | ReadyEffect::Deferred | ReadyEffect::YieldedAgain => {}
    }
}

/// Bounded worker-local token map. Capacity matches Task 4 in-flight.
pub struct TokenRegistry<T> {
    cap: usize,
    map: HashMap<SleepToken, T>,
}

impl<T> TokenRegistry<T> {
    pub fn with_capacity(cap: usize) -> Self {
        Self {
            cap,
            map: HashMap::new(),
        }
    }

    pub fn capacity(&self) -> usize {
        self.cap
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn try_insert(&mut self, token: SleepToken, value: T) -> Result<(), RegistryError>
    where
        T: PartialEq,
    {
        match self.map.get(&token) {
            Some(existing) if existing == &value => Ok(()),
            Some(_) => Err(RegistryError::Duplicate),
            None if self.map.len() >= self.cap => Err(RegistryError::Full),
            None => {
                self.map.insert(token, value);
                Ok(())
            }
        }
    }

    pub fn get(&self, token: SleepToken) -> Option<&T> {
        self.map.get(&token)
    }

    pub fn remove(&mut self, token: SleepToken) -> Option<T> {
        self.map.remove(&token)
    }

    /// Remove `token` only when it still maps to `expected`.
    pub fn remove_exact(&mut self, token: SleepToken, expected: &T) -> Option<T>
    where
        T: PartialEq,
    {
        match self.map.get(&token) {
            Some(existing) if existing == expected => self.map.remove(&token),
            _ => None,
        }
    }

    /// Drop every mapping without inspecting values. Worker exit uses this
    /// so leftover request pointers are never dereferenced.
    pub fn clear(&mut self) {
        self.map.clear();
    }
}

impl<T> Default for TokenRegistry<T> {
    fn default() -> Self {
        Self::with_capacity(DEFAULT_MAX_IN_FLIGHT)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ExecState {
    New,
    Halted,
    Failed,
    Yielded,
    Suspended,
    Cancelled,
}

/// First VM run outcome used by the early-completion test.
pub struct RunOutcome {
    token: Option<SleepToken>,
}

impl RunOutcome {
    pub fn token(&self) -> Option<SleepToken> {
        self.token
    }
}

/// Request-local VM, sleep coordinator, and suspend/resume state.
pub struct AccessExec {
    vm: Option<Vm>,
    coordinator: SleepCoordinator,
    request_id: u64,
    current: Option<(SleepToken, i64)>,
    state: ExecState,
    suspend_committed: bool,
    deferred: Option<SleepToken>,
}

impl AccessExec {
    /// Bind `program` with a request-local, non-`Clone` [`SleepSession`].
    ///
    /// `session` and `coordinator` must come from the same worker-wide
    /// coordinator. Access setup obtains them from
    /// [`crate::runtime::WorkerRuntime::open_access_sleep`].
    pub fn bind_program(
        program: Arc<Program>,
        request_id: u64,
        session: SleepSession,
        coordinator: SleepCoordinator,
    ) -> Result<Self, String> {
        let mut vm = Vm::new_shared(program);
        vm.bind_args_function(NGX_SLEEP_NAME, Box::new(session));
        Ok(Self {
            vm: Some(vm),
            coordinator,
            request_id,
            current: None,
            state: ExecState::New,
            suspend_committed: false,
            deferred: None,
        })
    }

    pub fn request_id(&self) -> u64 {
        self.request_id
    }

    pub fn current_token(&self) -> Option<SleepToken> {
        self.current.map(|(token, _)| token)
    }

    pub fn vm_status(&self) -> Option<VmStatus> {
        match self.state {
            ExecState::Halted => Some(VmStatus::Halted),
            ExecState::Yielded | ExecState::Suspended => Some(VmStatus::Yielded),
            ExecState::Failed | ExecState::New | ExecState::Cancelled => None,
        }
    }

    pub fn is_cancelled(&self) -> bool {
        matches!(self.state, ExecState::Cancelled)
    }

    /// Halted, Failed, or Cancelled: do not finalize again on client close.
    pub fn is_finished(&self) -> bool {
        matches!(
            self.state,
            ExecState::Halted | ExecState::Failed | ExecState::Cancelled
        )
    }

    /// Request-pool cleanup entry. Commits Cancelled (or stays Halted/Failed)
    /// before the caller unregisters, aborts, or drops. Idempotent.
    pub fn cleanup(&mut self) -> Option<SleepToken> {
        let token = self.current.take().map(|(token, _)| token);
        self.deferred = None;
        self.suspend_committed = false;
        self.vm = None;
        match self.state {
            ExecState::Halted | ExecState::Failed | ExecState::Cancelled => {}
            ExecState::New | ExecState::Yielded | ExecState::Suspended => {
                self.state = ExecState::Cancelled;
            }
        }
        token
    }

    /// Run the VM until Halted, Yielded, or error. Does not submit or commit.
    pub fn run_until_yield_or_halt(&mut self) -> Result<RunOutcome, String> {
        let vm = self.vm.as_mut().ok_or_else(|| "missing VM".to_string())?;
        match vm.run() {
            Ok(VmStatus::Halted) => {
                self.vm = None;
                self.state = ExecState::Halted;
                Ok(RunOutcome { token: None })
            }
            Ok(VmStatus::Yielded) => {
                let (token, ms) = self
                    .coordinator
                    .claim_submission()
                    .map_err(|err| err.to_string())?
                    .ok_or_else(|| "yielded without sleep submission".to_string())?;
                self.current = Some((token, ms));
                self.state = ExecState::Yielded;
                Ok(RunOutcome { token: Some(token) })
            }
            Ok(status) => {
                self.fail_local();
                Err(format!("unexpected VM status {status:?}"))
            }
            Err(err) => {
                self.fail_local();
                Err(err.to_string())
            }
        }
    }

    pub fn register_current(
        &mut self,
        registry: &mut TokenRegistry<u64>,
    ) -> Result<(), RegistryError> {
        let Some((token, _)) = self.current else {
            return Ok(());
        };
        registry.try_insert(token, self.request_id)
    }

    pub fn commit_suspend(&mut self) {
        if matches!(self.state, ExecState::Yielded) {
            self.suspend_committed = true;
            self.state = ExecState::Suspended;
        }
    }

    /// Access-handler entry. Never resumes a yielded VM on this stack.
    pub fn handle_access(
        &mut self,
        registry: &mut TokenRegistry<u64>,
        submit: &mut impl SleepSubmit,
    ) -> AccessStatus {
        match self.state {
            ExecState::Halted => AccessStatus::Ok,
            ExecState::Failed => AccessStatus::Error500,
            ExecState::Suspended | ExecState::Yielded | ExecState::Cancelled => AccessStatus::Again,
            ExecState::New => match self.run_until_yield_or_halt() {
                Ok(outcome) if outcome.token.is_some() => {
                    if self.arm_suspend(registry, submit).is_err() {
                        AccessStatus::Error500
                    } else {
                        AccessStatus::Again
                    }
                }
                Ok(_) => AccessStatus::Ok,
                Err(_) => AccessStatus::Error500,
            },
        }
    }

    fn arm_suspend(
        &mut self,
        registry: &mut TokenRegistry<u64>,
        submit: &mut impl SleepSubmit,
    ) -> Result<(), AccessArmError> {
        let (token, ms) = self.current.ok_or(AccessArmError::Failed)?;
        if self.register_current(registry).is_err() {
            self.fail_local();
            return Err(AccessArmError::Failed);
        }
        if let Err(err) = submit.submit(token, ms) {
            registry.remove_exact(token, &self.request_id);
            self.current = None;
            let _ = err;
            self.fail_local();
            return Err(AccessArmError::Failed);
        }
        self.commit_suspend();
        Ok(())
    }

    pub fn apply_ready(
        &mut self,
        token: SleepToken,
        registry: &mut TokenRegistry<u64>,
        submit: &mut impl SleepSubmit,
    ) -> ReadyEffect {
        if matches!(
            self.state,
            ExecState::Cancelled | ExecState::Halted | ExecState::Failed
        ) {
            return ReadyEffect::Ignored;
        }
        match registry.get(token) {
            Some(&id) if id == self.request_id => {}
            Some(_) => return ReadyEffect::Ignored,
            None => return ReadyEffect::Ignored,
        }
        let Some((current, _)) = self.current else {
            return ReadyEffect::Ignored;
        };
        if current != token {
            return ReadyEffect::Ignored;
        }
        if !self.suspend_committed {
            self.deferred = Some(token);
            return ReadyEffect::Deferred;
        }
        self.resume_after_complete(token, registry, submit)
    }

    pub fn flush_deferred(
        &mut self,
        registry: &mut TokenRegistry<u64>,
        submit: &mut impl SleepSubmit,
    ) -> ReadyEffect {
        let Some(token) = self.deferred.take() else {
            return ReadyEffect::Ignored;
        };
        self.apply_ready(token, registry, submit)
    }

    fn resume_after_complete(
        &mut self,
        token: SleepToken,
        registry: &mut TokenRegistry<u64>,
        submit: &mut impl SleepSubmit,
    ) -> ReadyEffect {
        match self.coordinator.complete(token) {
            Ok(()) => {}
            Err(_) => return ReadyEffect::Ignored,
        }
        let Some(vm) = self.vm.as_mut() else {
            self.fail_resume(registry);
            return ReadyEffect::Failed;
        };
        match vm.resume() {
            Ok(VmStatus::Halted) => {
                self.finish_halted(registry);
                ReadyEffect::Halted
            }
            Ok(VmStatus::Yielded) => match self.claim_next(registry, submit) {
                Ok(()) => ReadyEffect::YieldedAgain,
                Err(_) => {
                    self.fail_resume(registry);
                    ReadyEffect::Failed
                }
            },
            Ok(_) | Err(_) => {
                self.fail_resume(registry);
                ReadyEffect::Failed
            }
        }
    }

    fn claim_next(
        &mut self,
        registry: &mut TokenRegistry<u64>,
        submit: &mut impl SleepSubmit,
    ) -> Result<(), AccessArmError> {
        if let Some((old, _)) = self.current.take() {
            registry.remove_exact(old, &self.request_id);
        }
        let (token, ms) = self
            .coordinator
            .claim_submission()
            .map_err(|_| AccessArmError::Failed)?
            .ok_or(AccessArmError::Failed)?;
        self.current = Some((token, ms));
        self.state = ExecState::Yielded;
        self.suspend_committed = false;
        self.arm_suspend(registry, submit)
    }

    fn finish_halted(&mut self, registry: &mut TokenRegistry<u64>) {
        if let Some((token, _)) = self.current.take() {
            registry.remove_exact(token, &self.request_id);
        }
        self.deferred = None;
        self.vm = None;
        self.state = ExecState::Halted;
    }

    fn fail_local(&mut self) {
        self.current = None;
        self.deferred = None;
        self.vm = None;
        self.state = ExecState::Failed;
    }

    fn fail_resume(&mut self, registry: &mut TokenRegistry<u64>) {
        if let Some((token, _)) = self.current.take() {
            registry.remove_exact(token, &self.request_id);
        }
        self.fail_local();
    }
}

enum AccessArmError {
    Failed,
}

impl Drop for AccessExec {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}
