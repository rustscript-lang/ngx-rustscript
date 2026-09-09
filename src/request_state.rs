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
use vm::{Program, Vm, VmError, VmStatus};

use crate::async_bridge::NgxAsyncBridge;
use crate::host::{
    AllowedOps, NgxPhase, RequestDisposition, RequestLocalCtx, bind_ngx_hosts, is_control_stop,
    with_vm_request_scope_full,
};
use crate::notify::{CompletionPermit, RequestIdentity};

fn with_vm_entry<T>(
    identity: RequestIdentity,
    ctx: *mut RequestLocalCtx,
    disposition: *mut RequestDisposition,
    f: impl FnOnce() -> T,
) -> T {
    with_vm_request_scope_full(
        identity,
        NgxPhase::Access,
        AllowedOps::for_phase(NgxPhase::Access),
        ctx,
        disposition,
        f,
    )
}

/// Result of one access-phase entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccessStatus {
    Ok,
    Again,
    Error500,
    Finalize(i32),
}

impl AccessStatus {
    pub fn as_nginx(self) -> Status {
        match self {
            Self::Ok => Status::NGX_OK,
            Self::Again => Status::NGX_AGAIN,
            Self::Error500 => HTTPStatus::INTERNAL_SERVER_ERROR.into(),
            Self::Finalize(0) => Status::NGX_OK,
            Self::Finalize(-1) => Status::NGX_ERROR,
            Self::Finalize(-5) => Status::NGX_DECLINED,
            Self::Finalize(code) => Status(code as nginx_sys::ngx_int_t),
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
    ctx: RequestLocalCtx,
    disposition: RequestDisposition,
    final_status: Option<AccessStatus>,
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
            ctx: RequestLocalCtx::new(),
            disposition: RequestDisposition::Continue,
            final_status: None,
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

    fn finish_from_disposition(&mut self, from_error: bool) -> AccessStatus {
        self.last_status = Some(VmStatus::Halted);
        let status = match std::mem::replace(&mut self.disposition, RequestDisposition::Continue) {
            RequestDisposition::Continue if from_error => AccessStatus::Error500,
            RequestDisposition::Continue => AccessStatus::Ok,
            RequestDisposition::Finalize(0) => AccessStatus::Ok,
            RequestDisposition::Finalize(code) => AccessStatus::Finalize(code),
            RequestDisposition::ExternalRedirect { status, .. } => AccessStatus::Finalize(status),
        };
        if matches!(status, AccessStatus::Error500) {
            self.state = ExecState::Failed;
        } else {
            self.state = ExecState::Halted;
        }
        self.final_status = Some(status);
        status
    }

    fn finish_from_vm_error(&mut self, err: VmError) -> AccessStatus {
        if is_control_stop(&err) {
            self.finish_from_disposition(false)
        } else {
            self.disposition = RequestDisposition::Continue;
            self.finish_from_disposition(true)
        }
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
            ExecState::Halted => self.final_status.unwrap_or(AccessStatus::Ok),
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
        let identity = self.identity;
        let ctx = &mut self.ctx as *mut RequestLocalCtx;
        let disp = &mut self.disposition as *mut RequestDisposition;
        match with_vm_entry(identity, ctx, disp, || vm.run()) {
            Ok(VmStatus::Halted) => self.finish_from_disposition(false),
            Ok(VmStatus::Waiting(op_id)) => {
                self.last_status = Some(VmStatus::Waiting(op_id));
                self.arm_wait(registry, waiter)
                    .unwrap_or_else(|status| status)
            }
            Ok(VmStatus::Yielded) => {
                self.state = ExecState::Failed;
                AccessStatus::Error500
            }
            Err(err) => self.finish_from_vm_error(err),
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
        let identity = self.identity;
        let disp = &mut self.disposition as *mut RequestDisposition;
        let vm = self.vm.as_mut().ok_or(AccessStatus::Error500)?;
        match with_vm_entry(identity, std::ptr::null_mut(), disp, || {
            vm.poll_waiting_host_op(&mut cx)
        }) {
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
        let identity = self.identity;
        let ctx = &mut self.ctx as *mut RequestLocalCtx;
        let disp = &mut self.disposition as *mut RequestDisposition;
        let vm = self.vm.as_mut().ok_or(AccessStatus::Error500)?;
        match with_vm_entry(identity, ctx, disp, || vm.resume()) {
            Ok(VmStatus::Halted) => Ok(self.finish_from_disposition(false)),
            Ok(VmStatus::Waiting(op_id)) => {
                self.last_status = Some(VmStatus::Waiting(op_id));
                self.arm_wait(registry, waiter)
            }
            Ok(VmStatus::Yielded) => {
                self.state = ExecState::Failed;
                Err(AccessStatus::Error500)
            }
            Err(err) => {
                let status = self.finish_from_vm_error(err);
                if matches!(status, AccessStatus::Error500) {
                    Err(status)
                } else {
                    Ok(status)
                }
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
        let identity = self.identity;
        let Some(vm) = self.vm.as_mut() else {
            return ReadyEffect::Ignored;
        };
        let waker = waiter.waker(identity);
        let mut cx = Context::from_waker(&waker);
        let disp = &mut self.disposition as *mut RequestDisposition;
        match with_vm_entry(identity, std::ptr::null_mut(), disp, || {
            vm.poll_waiting_host_op(&mut cx)
        }) {
            Poll::Pending => ReadyEffect::Ignored,
            Poll::Ready(Ok(())) => {
                registry.remove_exact(self.identity, &self.request_id);
                self.permit = None;
                match self.resume_after_ready(registry, waiter) {
                    Ok(AccessStatus::Ok) | Ok(AccessStatus::Finalize(_)) => ReadyEffect::Halted,
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

    /// Invalidate this wait, then drop the VM/bridge future and ctx map.
    pub fn cleanup(&mut self) -> Option<RequestIdentity> {
        if self.state == ExecState::Cancelled {
            return None;
        }
        if matches!(self.state, ExecState::Halted | ExecState::Failed) {
            self.ctx.clear();
            self.vm = None;
            self.permit = None;
            return None;
        }
        let identity = Some(self.identity);
        self.state = ExecState::Cancelled;
        self.deferred = false;
        self.permit = None;
        self.ctx.clear();
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::compile_rss_source;
    use crate::host::live_owned_entry_count;
    use crate::notify::DEFAULT_MAX_IN_FLIGHT;
    use crate::runtime::WorkerRuntime;
    use std::sync::Arc;

    fn compile(source: &str) -> Arc<Program> {
        compile_rss_source(source).expect("compile with ngx catalog")
    }

    struct Driver {
        worker: WorkerRuntime,
        exec: AccessExec,
        sink: RecordingSink,
        registry: IdentityRegistry<u64>,
    }

    impl Driver {
        fn new(program: Arc<Program>, request_id: u64) -> Self {
            let mut worker = WorkerRuntime::start().unwrap();
            let exec = worker.bind_access_exec(program, request_id).expect("bind");
            Self {
                worker,
                exec,
                sink: RecordingSink::default(),
                registry: IdentityRegistry::with_capacity(DEFAULT_MAX_IN_FLIGHT),
            }
        }

        fn handle(&mut self) -> AccessStatus {
            let status = self.exec.handle_access(&mut self.registry, &self.worker);
            if status == AccessStatus::Again {
                self.exec.commit_suspend();
            }
            status
        }

        fn cleanup(&mut self) -> Option<RequestIdentity> {
            let request_id = self.exec.request_id();
            let identity = self.exec.identity();
            let _ = self.registry.remove_exact(identity, &request_id);
            self.exec.cleanup_wait(&self.worker)
        }
    }

    #[test]
    fn access_exec_live_owned_count_returns_to_baseline_exactly_once() {
        let baseline = live_owned_entry_count();

        {
            let mut driver = Driver::new(compile("use ngx;\nngx::ctx::set(\"k\", 1);\n"), 1);
            assert_eq!(driver.handle(), AccessStatus::Ok);
            assert!(live_owned_entry_count() > baseline);
            assert!(driver.cleanup().is_none());
            assert_eq!(live_owned_entry_count(), baseline);
        }
        assert_eq!(live_owned_entry_count(), baseline);

        {
            let mut driver =
                Driver::new(compile("use ngx;\nngx::ctx::set(\"k\", 1);\n1 / 0;\n"), 2);
            assert_eq!(driver.handle(), AccessStatus::Error500);
            assert!(live_owned_entry_count() > baseline);
            drop(driver);
        }
        assert_eq!(live_owned_entry_count(), baseline);

        let sleeping = compile("use ngx;\nngx::ctx::set(\"k\", 1);\nngx::sleep_ms(80);\n");
        {
            let mut driver = Driver::new(sleeping.clone(), 3);
            assert_eq!(driver.handle(), AccessStatus::Again);
            assert!(live_owned_entry_count() > baseline);
            assert!(driver.cleanup().is_some());
            assert_eq!(live_owned_entry_count(), baseline);
            let identity = driver.exec.identity();
            let effect = driver
                .exec
                .apply_ready(identity, &mut driver.registry, &driver.worker);
            assert_eq!(effect, ReadyEffect::Ignored);
            assert_eq!(live_owned_entry_count(), baseline);
        }
        assert_eq!(live_owned_entry_count(), baseline);

        {
            let mut driver = Driver::new(sleeping, 4);
            assert_eq!(driver.handle(), AccessStatus::Again);
            assert!(live_owned_entry_count() > baseline);
            drop(driver);
        }
        assert_eq!(live_owned_entry_count(), baseline);
    }

    #[test]
    fn second_access_exec_starts_empty_and_returns_live_owned_count_to_baseline() {
        let baseline = live_owned_entry_count();
        let program = compile(
            r#"
use ngx;
if ngx::ctx::get("k") != null {
    1 / 0;
}
ngx::ctx::set("k", 1);
"#,
        );
        let mut first = Driver::new(program.clone(), 7);
        assert_eq!(first.handle(), AccessStatus::Ok);
        assert!(live_owned_entry_count() > baseline);
        assert_eq!(first.handle(), AccessStatus::Ok);
        assert!(first.cleanup().is_none());
        assert_eq!(live_owned_entry_count(), baseline);

        let mut second = Driver::new(program, 8);
        assert_eq!(
            second.handle(),
            AccessStatus::Ok,
            "a new AccessExec must start with empty ctx"
        );
        assert!(live_owned_entry_count() > baseline);
        assert!(second.cleanup().is_none());
        assert_eq!(live_owned_entry_count(), baseline);
    }

    #[test]
    fn completion_host_error_cancel_stale_wake_and_drop_release_ctx_to_baseline() {
        let baseline = live_owned_entry_count();

        let halt = compile("use ngx;\nngx::ctx::set(\"k\", \"done\");\n");
        let mut driver = Driver::new(halt, 3);
        assert_eq!(driver.handle(), AccessStatus::Ok);
        assert!(live_owned_entry_count() > baseline);
        assert!(driver.cleanup().is_none());
        assert_eq!(live_owned_entry_count(), baseline);

        let boom = compile("use ngx;\nngx::ctx::set(\"k\", \"err\");\n1 / 0;\n");
        {
            let mut driver = Driver::new(boom, 4);
            assert_eq!(driver.handle(), AccessStatus::Error500);
            assert!(live_owned_entry_count() > baseline);
            drop(driver);
        }
        assert_eq!(live_owned_entry_count(), baseline);

        let sleeping = compile("use ngx;\nngx::ctx::set(\"k\", \"wait\");\nngx::sleep_ms(80);\n");
        {
            let mut driver = Driver::new(sleeping.clone(), 5);
            assert_eq!(driver.handle(), AccessStatus::Again);
            assert!(live_owned_entry_count() > baseline);
            let identity = driver.exec.identity();
            assert!(driver.cleanup().is_some());
            assert_eq!(live_owned_entry_count(), baseline);
            let effect = driver
                .exec
                .apply_ready(identity, &mut driver.registry, &driver.worker);
            apply_ready_effect(effect, &mut driver.sink);
            assert_eq!(effect, ReadyEffect::Ignored);
            assert_eq!(live_owned_entry_count(), baseline);
            drop(driver);
        }
        assert_eq!(live_owned_entry_count(), baseline);

        {
            let mut driver = Driver::new(sleeping, 6);
            assert_eq!(driver.handle(), AccessStatus::Again);
            assert!(live_owned_entry_count() > baseline);
            drop(driver);
        }
        assert_eq!(live_owned_entry_count(), baseline);
    }
}
