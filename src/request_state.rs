//! Access-phase VM driver: `VmStatus::Waiting` suspend/resume.
//!
//! Before the initial poll the driver reserves bounded capacity and registers
//! a generation-safe identity plus an eventfd-backed waker. A Ready poll
//! unregisters, releases the slot, and resumes. Pending returns `NGX_AGAIN`.
//! A wake during the initial poll only queues for a later NGINX event turn.

use std::collections::HashMap;
use std::task::{Context, Poll, Waker};

use ngx::core::Status;
use ngx::http::HTTPStatus;
use tokio::runtime::Handle;
use vm::{Program, Vm, VmStatus};

use crate::async_bridge::NgxAsyncBridge;
use crate::host::bind_ngx_hosts;
use crate::notify::{CompletionPermit, RequestIdentity};

/// Result of one access-phase entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccessStatus {
    Ok,
    Again,
    Error500,
}

impl AccessStatus {
    pub fn as_nginx(self) -> Status {
        match self {
            Self::Ok => Status::NGX_OK,
            Self::Again => Status::NGX_AGAIN,
            Self::Error500 => HTTPStatus::INTERNAL_SERVER_ERROR.into(),
        }
    }
}

/// Result of delivering a ready [`RequestIdentity`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadyEffect {
    Halted,
    WaitingAgain,
    Failed,
    Deferred,
    Ignored,
}

/// Recording sink used by unit tests.
#[derive(Clone, Debug, Default)]
pub struct RecordingSink {
    pub retains: usize,
    pub releases: usize,
    pub phase_continues: usize,
}

/// Side effects applied after a ready identity is processed.
pub trait AccessSink {
    fn retain(&mut self);
    fn release(&mut self);
    fn continue_phases(&mut self);
}

impl AccessSink for RecordingSink {
    fn retain(&mut self) {
        self.retains += 1;
    }
    fn release(&mut self) {
        self.releases += 1;
    }
    fn continue_phases(&mut self) {
        self.phase_continues += 1;
    }
}

pub fn apply_ready_effect(effect: ReadyEffect, sink: &mut impl AccessSink) {
    match effect {
        ReadyEffect::Halted | ReadyEffect::Failed => sink.continue_phases(),
        ReadyEffect::WaitingAgain | ReadyEffect::Deferred | ReadyEffect::Ignored => {}
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubmitError {
    Full,
    Closed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegistryError {
    Full,
    Duplicate,
}

/// Bounded identity -> value map with exact-generation lookup.
pub struct IdentityRegistry<T> {
    map: HashMap<RequestIdentity, T>,
    cap: usize,
}

impl<T> IdentityRegistry<T> {
    pub fn with_capacity(cap: usize) -> Self {
        Self {
            map: HashMap::with_capacity(cap),
            cap,
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

    pub fn try_insert(&mut self, identity: RequestIdentity, value: T) -> Result<(), RegistryError>
    where
        T: PartialEq,
    {
        match self.map.get(&identity) {
            Some(existing) if existing == &value => Ok(()),
            Some(_) => Err(RegistryError::Duplicate),
            None if self.map.len() >= self.cap => Err(RegistryError::Full),
            None => {
                self.map.insert(identity, value);
                Ok(())
            }
        }
    }

    pub fn get(&self, identity: RequestIdentity) -> Option<&T> {
        self.map.get(&identity)
    }

    pub fn remove(&mut self, identity: RequestIdentity) -> Option<T> {
        self.map.remove(&identity)
    }

    pub fn remove_exact(&mut self, identity: RequestIdentity, expected: &T) -> Option<T>
    where
        T: PartialEq,
    {
        match self.map.get(&identity) {
            Some(value) if value == expected => self.map.remove(&identity),
            _ => None,
        }
    }

    pub fn clear(&mut self) {
        self.map.clear();
    }
}

impl<T> Default for IdentityRegistry<T> {
    fn default() -> Self {
        Self::with_capacity(crate::notify::DEFAULT_MAX_IN_FLIGHT)
    }
}

/// Wait-slot + waker factory used by [`AccessExec`].
pub trait AsyncWait {
    fn reserve(&self) -> Result<CompletionPermit, SubmitError>;
    fn waker(&self, identity: RequestIdentity) -> Waker;
    fn invalidate(&self, identity: RequestIdentity);
    fn forget_invalidated(&self, identity: RequestIdentity);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ExecState {
    New,
    Suspended,
    Halted,
    Failed,
    Cancelled,
}

/// Request-local VM plus generation-safe wait identity.
pub struct AccessExec {
    vm: Option<Vm>,
    request_id: u64,
    identity: RequestIdentity,
    state: ExecState,
    suspend_committed: bool,
    deferred: bool,
    permit: Option<CompletionPermit>,
    last_status: Option<VmStatus>,
}

impl AccessExec {
    pub fn bind_program(
        program: std::sync::Arc<Program>,
        request_id: u64,
        generation: u64,
        runtime: Handle,
    ) -> Result<Self, String> {
        let mut vm = Vm::new_shared(program);
        bind_ngx_hosts(&mut vm);
        vm.set_async_bridge(Box::new(NgxAsyncBridge::new(runtime)))
            .map_err(|err| err.to_string())?;
        Ok(Self {
            vm: Some(vm),
            request_id,
            identity: RequestIdentity::new(generation, request_id),
            state: ExecState::New,
            suspend_committed: false,
            deferred: false,
            permit: None,
            last_status: None,
        })
    }

    pub fn request_id(&self) -> u64 {
        self.request_id
    }

    pub fn identity(&self) -> RequestIdentity {
        self.identity
    }

    pub fn current_identity(&self) -> Option<RequestIdentity> {
        match self.state {
            ExecState::Suspended => Some(self.identity),
            _ => None,
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.state == ExecState::Cancelled
    }

    pub fn is_finished(&self) -> bool {
        matches!(
            self.state,
            ExecState::Halted | ExecState::Failed | ExecState::Cancelled
        )
    }

    pub fn vm_status(&self) -> Option<VmStatus> {
        self.last_status
    }

    pub fn commit_suspend(&mut self) {
        self.suspend_committed = true;
    }

    pub fn handle_access(
        &mut self,
        registry: &mut IdentityRegistry<u64>,
        waiter: &impl AsyncWait,
    ) -> AccessStatus {
        match self.state {
            ExecState::Halted => AccessStatus::Ok,
            ExecState::Failed => AccessStatus::Error500,
            ExecState::Cancelled | ExecState::Suspended => AccessStatus::Again,
            ExecState::New => self.run_and_arm(registry, waiter),
        }
    }

    fn run_and_arm(
        &mut self,
        registry: &mut IdentityRegistry<u64>,
        waiter: &impl AsyncWait,
    ) -> AccessStatus {
        let vm = match self.vm.as_mut() {
            Some(vm) => vm,
            None => {
                self.state = ExecState::Failed;
                return AccessStatus::Error500;
            }
        };
        match vm.run() {
            Ok(VmStatus::Halted) => {
                self.last_status = Some(VmStatus::Halted);
                self.state = ExecState::Halted;
                AccessStatus::Ok
            }
            Ok(VmStatus::Waiting(op_id)) => {
                self.last_status = Some(VmStatus::Waiting(op_id));
                self.arm_wait(registry, waiter)
                    .unwrap_or_else(|status| status)
            }
            Ok(VmStatus::Yielded) => {
                self.state = ExecState::Failed;
                AccessStatus::Error500
            }
            Err(_) => {
                self.state = ExecState::Failed;
                AccessStatus::Error500
            }
        }
    }

    fn arm_wait(
        &mut self,
        registry: &mut IdentityRegistry<u64>,
        waiter: &impl AsyncWait,
    ) -> Result<AccessStatus, AccessStatus> {
        let permit = waiter.reserve().map_err(|_| {
            self.state = ExecState::Failed;
            AccessStatus::Error500
        })?;
        if let Err(err) = registry.try_insert(self.identity, self.request_id) {
            drop(permit);
            self.state = ExecState::Failed;
            let _ = err;
            return Err(AccessStatus::Error500);
        }
        self.permit = Some(permit);
        let waker = waiter.waker(self.identity);
        let mut cx = Context::from_waker(&waker);
        let vm = self.vm.as_mut().ok_or(AccessStatus::Error500)?;
        match vm.poll_waiting_host_op(&mut cx) {
            Poll::Pending => {
                self.state = ExecState::Suspended;
                self.last_status = match self.last_status {
                    Some(VmStatus::Waiting(op_id)) => Some(VmStatus::Waiting(op_id)),
                    other => other,
                };
                Ok(AccessStatus::Again)
            }
            Poll::Ready(Ok(())) => {
                registry.remove_exact(self.identity, &self.request_id);
                self.permit = None;
                self.resume_after_ready(registry, waiter)
            }
            Poll::Ready(Err(_)) => {
                registry.remove_exact(self.identity, &self.request_id);
                self.permit = None;
                self.state = ExecState::Failed;
                Err(AccessStatus::Error500)
            }
        }
    }

    fn resume_after_ready(
        &mut self,
        registry: &mut IdentityRegistry<u64>,
        waiter: &impl AsyncWait,
    ) -> Result<AccessStatus, AccessStatus> {
        let vm = self.vm.as_mut().ok_or(AccessStatus::Error500)?;
        match vm.resume() {
            Ok(VmStatus::Halted) => {
                self.last_status = Some(VmStatus::Halted);
                self.state = ExecState::Halted;
                Ok(AccessStatus::Ok)
            }
            Ok(VmStatus::Waiting(op_id)) => {
                self.last_status = Some(VmStatus::Waiting(op_id));
                self.arm_wait(registry, waiter)
            }
            Ok(VmStatus::Yielded) | Err(_) => {
                self.state = ExecState::Failed;
                Err(AccessStatus::Error500)
            }
        }
    }

    pub fn apply_ready(
        &mut self,
        identity: RequestIdentity,
        registry: &mut IdentityRegistry<u64>,
        waiter: &impl AsyncWait,
    ) -> ReadyEffect {
        if identity != self.identity {
            return ReadyEffect::Ignored;
        }
        if self.state == ExecState::Cancelled || self.state == ExecState::Halted {
            return ReadyEffect::Ignored;
        }
        if !self.suspend_committed {
            self.deferred = true;
            return ReadyEffect::Deferred;
        }
        if registry.get(identity) != Some(&self.request_id) {
            return ReadyEffect::Ignored;
        }
        self.poll_ready(registry, waiter)
    }

    pub fn flush_deferred(
        &mut self,
        registry: &mut IdentityRegistry<u64>,
        waiter: &impl AsyncWait,
    ) -> ReadyEffect {
        if !self.deferred || self.state == ExecState::Cancelled {
            return ReadyEffect::Ignored;
        }
        self.deferred = false;
        if !self.suspend_committed {
            return ReadyEffect::Deferred;
        }
        self.poll_ready(registry, waiter)
    }

    fn poll_ready(
        &mut self,
        registry: &mut IdentityRegistry<u64>,
        waiter: &impl AsyncWait,
    ) -> ReadyEffect {
        let Some(vm) = self.vm.as_mut() else {
            return ReadyEffect::Ignored;
        };
        let waker = waiter.waker(self.identity);
        let mut cx = Context::from_waker(&waker);
        match vm.poll_waiting_host_op(&mut cx) {
            Poll::Pending => ReadyEffect::Ignored,
            Poll::Ready(Ok(())) => {
                registry.remove_exact(self.identity, &self.request_id);
                self.permit = None;
                match self.resume_after_ready(registry, waiter) {
                    Ok(AccessStatus::Ok) => ReadyEffect::Halted,
                    Ok(AccessStatus::Again) => ReadyEffect::WaitingAgain,
                    _ => ReadyEffect::Failed,
                }
            }
            Poll::Ready(Err(_)) => {
                registry.remove_exact(self.identity, &self.request_id);
                self.permit = None;
                self.state = ExecState::Failed;
                ReadyEffect::Failed
            }
        }
    }

    /// Invalidate this wait, then drop the VM/bridge future.
    pub fn cleanup(&mut self) -> Option<RequestIdentity> {
        if self.state == ExecState::Cancelled {
            return None;
        }
        if matches!(self.state, ExecState::Halted | ExecState::Failed) {
            self.vm = None;
            self.permit = None;
            return None;
        }
        let identity = Some(self.identity);
        self.state = ExecState::Cancelled;
        self.deferred = false;
        self.permit = None;
        self.vm = None;
        identity
    }

    /// Tombstone, drop the VM/future, then forget the tombstone.
    pub fn cleanup_wait(&mut self, waiter: &impl AsyncWait) -> Option<RequestIdentity> {
        if self.is_finished() {
            return self.cleanup();
        }
        let identity = self.identity;
        waiter.invalidate(identity);
        let taken = self.cleanup();
        waiter.forget_invalidated(identity);
        taken
    }
}
