//! Access-phase VM driver: `VmStatus::Waiting` suspend/resume.
//!
//! Immediately-ready generic host futures resume in the same NGINX event turn
//! up to a finite transition budget. The first real `Pending` reserves bounded
//! capacity, registers a generation-safe identity, and installs an eventfd
//! waker. Budget exhaustion posts a next-event continuation without consuming
//! wait capacity.

use std::collections::HashMap;
use std::ptr::NonNull;
#[cfg(ngx_rustscript_integration_hooks)]
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll, Waker};

use ngx::core::Status;
#[cfg(ngx_rustscript_integration_hooks)]
use ngx::ffi::NGX_LOG_ERR;
use ngx::http::HTTPStatus;
#[cfg(ngx_rustscript_integration_hooks)]
use ngx::http::Request;
#[cfg(ngx_rustscript_integration_hooks)]
use ngx::ngx_log_error;
use tokio::runtime::Handle;
#[cfg(test)]
use vm::HostOpId;
use vm::{HttpClientLease, Program, Vm, VmError, VmResult, VmStatus};

use crate::async_bridge::NgxAsyncBridge;
use crate::host::{
    AllowedOps, NgxPhase, RequestDisposition, RequestLocalCtx, StandardHostPolicy, is_control_stop,
    with_vm_request_scope_full,
};
use crate::notify::{CompletionPermit, RequestIdentity};
use crate::phase::{PreparedScript, ScriptExecutionMetadata};

#[cfg(ngx_rustscript_integration_hooks)]
static WAIT_EVENTFD_ARMS: AtomicU64 = AtomicU64::new(0);
#[cfg(ngx_rustscript_integration_hooks)]
static WAIT_COUNT_INCS: AtomicU64 = AtomicU64::new(0);
#[cfg(ngx_rustscript_integration_hooks)]
static WAIT_TIMER_ARMS: AtomicU64 = AtomicU64::new(0);

/// Exact fail-closed error when a log-phase VM returns `Waiting`.
pub const LOG_CANNOT_WAIT: &str = "log handler cannot wait";

/// Maximum immediately-ready host transitions driven in one NGINX event turn.
pub const MAX_IMMEDIATE_TRANSITIONS: usize = 64;

#[cfg(test)]
enum ImmediateDrive {
    Halted,
    Pending(HostOpId),
    Budget { op_id: HostOpId, transitions: usize },
    Yielded,
    Failed(VmError),
}

enum WaitArm {
    Pending,
    Ready,
    Failed,
}

#[cfg(test)]
fn drive_immediate_ready(
    initial: VmResult<VmStatus>,
    transition_budget: usize,
    mut poll_waiting: impl FnMut(&mut Context<'_>) -> Poll<VmResult<()>>,
    mut resume: impl FnMut() -> VmResult<VmStatus>,
) -> ImmediateDrive {
    let mut status = match initial {
        Ok(status) => status,
        Err(error) => return ImmediateDrive::Failed(error),
    };
    let mut transitions = 0usize;
    loop {
        match status {
            VmStatus::Halted => return ImmediateDrive::Halted,
            VmStatus::Yielded => return ImmediateDrive::Yielded,
            VmStatus::Waiting(op_id) => {
                if transitions >= transition_budget {
                    return ImmediateDrive::Budget { op_id, transitions };
                }
                let waker = Waker::noop();
                let mut cx = Context::from_waker(waker);
                match poll_waiting(&mut cx) {
                    Poll::Pending => return ImmediateDrive::Pending(op_id),
                    Poll::Ready(Err(error)) => return ImmediateDrive::Failed(error),
                    Poll::Ready(Ok(())) => {
                        transitions = transitions.saturating_add(1);
                        status = match resume() {
                            Ok(status) => status,
                            Err(error) => return ImmediateDrive::Failed(error),
                        };
                    }
                }
            }
        }
    }
}

/// Process-wide wait-arm counters: (eventfd, main-count, timer).
#[cfg(ngx_rustscript_integration_hooks)]
pub fn wait_arm_snapshot() -> (u64, u64, u64) {
    (
        WAIT_EVENTFD_ARMS.load(Ordering::Relaxed),
        WAIT_COUNT_INCS.load(Ordering::Relaxed),
        WAIT_TIMER_ARMS.load(Ordering::Relaxed),
    )
}

#[cfg(ngx_rustscript_integration_hooks)]
pub fn reset_wait_arm_snapshot() {
    WAIT_EVENTFD_ARMS.store(0, Ordering::Relaxed);
    WAIT_COUNT_INCS.store(0, Ordering::Relaxed);
    WAIT_TIMER_ARMS.store(0, Ordering::Relaxed);
}

#[cfg(ngx_rustscript_integration_hooks)]
pub fn record_wait_count_inc() {
    WAIT_COUNT_INCS.fetch_add(1, Ordering::Relaxed);
}

#[cfg(ngx_rustscript_integration_hooks)]
#[allow(dead_code)]
pub fn record_wait_timer_arm() {
    WAIT_TIMER_ARMS.fetch_add(1, Ordering::Relaxed);
}

/// Record `r->main->count` around the body-read lease. Compiled out of ordinary artifacts.
#[cfg(ngx_rustscript_integration_hooks)]
pub(crate) fn proof_body_count(
    raw: *mut nginx_sys::ngx_http_request_t,
    stage: &'static str,
    rc: Option<nginx_sys::ngx_int_t>,
) {
    if std::env::var_os("NGX_RUSTSCRIPT_BODY_COUNT_PROOF").is_none() || raw.is_null() {
        return;
    }
    unsafe extern "C" {
        fn ngx_rustscript_c_request_main_count(r: *const nginx_sys::ngx_http_request_t) -> u32;
    }
    // SAFETY: hook sites run on the worker thread with a live request.
    let count = unsafe { ngx_rustscript_c_request_main_count(raw) };
    let request = unsafe { Request::from_ngx_http_request(raw) };
    let uri = request
        .unparsed_uri()
        .to_str()
        .unwrap_or("-")
        .chars()
        .map(|c| {
            if c.is_ascii_graphic() && c != ',' {
                c
            } else {
                '_'
            }
        })
        .collect::<String>();
    let extra = match rc {
        None => String::new(),
        Some(code) if code == nginx_sys::NGX_OK as nginx_sys::ngx_int_t => " rc=OK".to_string(),
        Some(code) if code == nginx_sys::NGX_AGAIN as nginx_sys::ngx_int_t => {
            " rc=AGAIN".to_string()
        }
        Some(code) if code == nginx_sys::NGX_ERROR as nginx_sys::ngx_int_t => {
            " rc=ERROR".to_string()
        }
        Some(code) if code == nginx_sys::NGX_HTTP_CLIENT_CLOSED_REQUEST as nginx_sys::ngx_int_t => {
            " rc=499".to_string()
        }
        Some(code) if code == nginx_sys::NGX_HTTP_REQUEST_TIME_OUT as nginx_sys::ngx_int_t => {
            " rc=408".to_string()
        }
        Some(code) => format!(" rc={code}"),
    };
    let msg = format!("BODY_COUNT_PROOF stage={stage} count={count} uri={uri}{extra}");
    ngx_log_error!(NGX_LOG_ERR, request.log(), "{msg}");
    if stage == "wait_armed" {
        ngx_log_error!(
            NGX_LOG_ERR,
            request.log(),
            "BODY_WAIT_ARMED count={count} uri={uri}"
        );
    }
}

/// Map a VM result onto the log-phase synchronous fail-closed policy.
pub fn fail_closed_log_status(status: Result<VmStatus, VmError>) -> Result<(), String> {
    match status {
        Ok(VmStatus::Halted) => Ok(()),
        Ok(VmStatus::Waiting(_)) => Err(LOG_CANNOT_WAIT.to_string()),
        Ok(VmStatus::Yielded) => Err("log handler cannot yield".to_string()),
        Err(err) => Err(err.to_string()),
    }
}

fn with_vm_entry<T>(
    identity: RequestIdentity,
    phase: NgxPhase,
    ctx: *mut RequestLocalCtx,
    disposition: *mut RequestDisposition,
    f: impl FnOnce() -> T,
) -> T {
    with_vm_request_scope_full(
        identity,
        phase,
        AllowedOps::for_phase(phase),
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

    /// Inserts `identity` exactly once, without comparing values.
    ///
    /// An identity that is already present is always
    /// [`RegistryError::Duplicate`], and capacity is enforced with
    /// [`RegistryError::Full`] under the same call. Callers that rotate
    /// identities (a fresh generation per round) need "this exact identity is
    /// new" as an insertion precondition rather than a value-equality merge.
    pub fn insert_new(&mut self, identity: RequestIdentity, value: T) -> Result<(), RegistryError> {
        if self.map.contains_key(&identity) {
            return Err(RegistryError::Duplicate);
        }
        if self.map.len() >= self.cap {
            return Err(RegistryError::Full);
        }
        self.map.insert(identity, value);
        Ok(())
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

    pub fn iter_values(&self) -> impl Iterator<Item = &T> {
        self.map.values()
    }
}

impl<T> Default for IdentityRegistry<T> {
    fn default() -> Self {
        Self::with_capacity(crate::notify::DEFAULT_MAX_IN_FLIGHT)
    }
}

/// Installs VM-local policy and worker-owned state after bound construction.
///
/// Host descriptors and resolved calls are already installed by
/// [`PreparedScript::new_vm`]. The immutable artifact comes from
/// [`crate::host::ngx_host_registry`] during preparation; this function only
/// attaches mutable state whose lifetime belongs to the current request or worker.
pub(crate) fn bind_program_hosts(
    vm: &mut Vm,
    prepared: &PreparedScript,
    policy: &StandardHostPolicy,
    http_client: HttpClientLease,
) -> Result<(), String> {
    // `crate::host::ngx_host_registry` prepared the immutable dispatch before
    // this request-local configuration step.
    if crate::timer::program_uses_ngx_timer_hosts(prepared.program()) {
        crate::timer::install_prepared_timer_vm(vm, policy, http_client)?;
    } else {
        crate::host::install_standard_policy(vm, policy, http_client)?;
    }
    Ok(())
}

pub(crate) struct ReusableVmKey {
    pub prepared: std::sync::Arc<PreparedScript>,
    pub phase: NgxPhase,
    pub policy: StandardHostPolicy,
    pub ngx_catalog_abi: vm::HostApiFingerprint,
    pub standard_catalog_abi: vm::HostApiFingerprint,
}

impl ReusableVmKey {
    pub(crate) fn matches(&self, other: &Self) -> bool {
        std::ptr::eq(
            self.prepared.program().as_ref(),
            other.prepared.program().as_ref(),
        ) && self.phase == other.phase
            && self.policy == other.policy
            && self.ngx_catalog_abi == other.ngx_catalog_abi
            && self.standard_catalog_abi == other.standard_catalog_abi
    }
}

/// Wait-slot + waker factory used by [`AccessExec`].
pub trait AsyncWait {
    fn reserve(&self) -> Result<CompletionPermit, SubmitError>;
    fn waker(&self, identity: RequestIdentity) -> Waker;
    fn post_continuation(&self, identity: RequestIdentity) -> Result<(), SubmitError>;
    fn invalidate(&self, identity: RequestIdentity);
    fn forget_invalidated(&self, identity: RequestIdentity);
    fn recycle_finished_exec(&self, _exec: &mut AccessExec) -> bool {
        false
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ExecState {
    New,
    Suspended,
    Posted,
    Halted,
    Failed,
    Cancelled,
}

/// Request-local VM plus generation-safe wait identity.
pub struct AccessExec {
    vm: Option<Vm>,
    reusable_key: Option<ReusableVmKey>,
    request_id: u64,
    identity: RequestIdentity,
    phase: NgxPhase,
    ctx: RequestLocalCtx,
    shared_ctx: Option<NonNull<RequestLocalCtx>>,
    disposition: RequestDisposition,
    final_status: Option<AccessStatus>,
    state: ExecState,
    suspend_committed: bool,
    deferred: bool,
    permit: Option<CompletionPermit>,
    last_status: Option<VmStatus>,
    last_error: Option<String>,
}

impl AccessExec {
    pub fn bind_program(
        program: std::sync::Arc<Program>,
        request_id: u64,
        generation: u64,
        runtime: Handle,
        http_client: HttpClientLease,
    ) -> Result<Self, String> {
        Self::bind_program_in_phase(
            program,
            request_id,
            generation,
            runtime,
            NgxPhase::Access,
            http_client,
        )
    }

    pub fn bind_program_in_phase(
        program: std::sync::Arc<Program>,
        request_id: u64,
        generation: u64,
        runtime: Handle,
        phase: NgxPhase,
        http_client: HttpClientLease,
    ) -> Result<Self, String> {
        Self::bind_program_in_phase_with_policy(
            program,
            request_id,
            generation,
            runtime,
            phase,
            StandardHostPolicy::fail_closed(),
            http_client,
        )
    }

    pub fn bind_program_in_phase_with_policy(
        program: std::sync::Arc<Program>,
        request_id: u64,
        generation: u64,
        runtime: Handle,
        phase: NgxPhase,
        policy: StandardHostPolicy,
        http_client: HttpClientLease,
    ) -> Result<Self, String> {
        let prepared = PreparedScript::prepare(program, ScriptExecutionMetadata::default())?;
        Self::bind_prepared_program_in_phase_with_policy(
            prepared,
            request_id,
            generation,
            runtime,
            phase,
            policy,
            http_client,
        )
    }

    pub(crate) fn bind_prepared_program_in_phase_with_policy(
        prepared: std::sync::Arc<PreparedScript>,
        request_id: u64,
        generation: u64,
        runtime: Handle,
        phase: NgxPhase,
        policy: StandardHostPolicy,
        http_client: HttpClientLease,
    ) -> Result<Self, String> {
        let mut vm = prepared.new_vm()?;
        bind_program_hosts(&mut vm, &prepared, &policy, http_client)?;
        vm.set_async_bridge(Box::new(NgxAsyncBridge::new(runtime)))
            .map_err(|err| err.to_string())?;
        vm.set_runtime_print_sink(|_| {});
        Ok(Self {
            vm: Some(vm),
            reusable_key: None,
            request_id,
            identity: RequestIdentity::new(generation, request_id),
            phase,
            ctx: RequestLocalCtx::new(),
            shared_ctx: None,
            disposition: RequestDisposition::Continue,
            final_status: None,
            state: ExecState::New,
            suspend_committed: false,
            deferred: false,
            permit: None,
            last_status: None,
            last_error: None,
        })
    }

    pub(crate) fn bind_reusable_vm(
        vm: Vm,
        key: ReusableVmKey,
        request_id: u64,
        generation: u64,
    ) -> Self {
        let phase = key.phase;
        Self {
            vm: Some(vm),
            reusable_key: Some(key),
            request_id,
            identity: RequestIdentity::new(generation, request_id),
            phase,
            ctx: RequestLocalCtx::new(),
            shared_ctx: None,
            disposition: RequestDisposition::Continue,
            final_status: None,
            state: ExecState::New,
            suspend_committed: false,
            deferred: false,
            permit: None,
            last_status: None,
            last_error: None,
        }
    }

    pub(crate) fn take_reusable_vm(&mut self) -> Option<(ReusableVmKey, Vm)> {
        if self.state != ExecState::Halted
            || self.last_status != Some(VmStatus::Halted)
            || self.last_error.is_some()
            || self.permit.is_some()
            || self.deferred
        {
            return None;
        }
        let key = self.reusable_key.take()?;
        let vm = self.vm.take()?;
        self.shared_ctx = None;
        self.ctx.clear();
        self.disposition = RequestDisposition::Continue;
        self.final_status = None;
        self.suspend_committed = false;
        Some((key, vm))
    }

    pub fn phase(&self) -> NgxPhase {
        self.phase
    }

    /// Attach this exec to a request-owned `ngx.ctx` store.
    ///
    /// The store must outlive the exec. Access and content on the same request
    /// share one store; VM execution, identity, and disposition stay independent.
    pub fn attach_request_ctx(&mut self, ctx: &mut RequestLocalCtx) {
        self.shared_ctx = Some(NonNull::from(ctx));
    }

    /// Attach using a stable pointer into request-owned module ctx.
    ///
    /// # Safety
    ///
    /// `ctx` must remain valid and unmoved until this exec is dropped.
    pub(crate) fn attach_request_ctx_ptr(&mut self, ctx: *mut RequestLocalCtx) {
        self.shared_ctx = NonNull::new(ctx);
    }

    fn ctx_ptr(&mut self) -> *mut RequestLocalCtx {
        match self.shared_ctx {
            Some(ptr) => ptr.as_ptr(),
            None => std::ptr::addr_of_mut!(self.ctx),
        }
    }

    fn release_owned_ctx(&mut self) {
        if self.shared_ctx.is_none() {
            self.ctx.clear();
        }
    }

    pub fn request_id(&self) -> u64 {
        self.request_id
    }

    pub fn identity(&self) -> RequestIdentity {
        self.identity
    }

    pub fn current_identity(&self) -> Option<RequestIdentity> {
        match self.state {
            ExecState::Suspended | ExecState::Posted => Some(self.identity),
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

    pub fn wait_permit_armed(&self) -> bool {
        self.permit.is_some()
    }

    pub fn vm_is_live(&self) -> bool {
        self.vm.is_some()
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
        if is_control_stop(&err) && self.disposition != RequestDisposition::Continue {
            self.last_error = None;
            self.finish_from_disposition(false)
        } else {
            self.last_error = Some(err.to_string());
            self.disposition = RequestDisposition::Continue;
            self.finish_from_disposition(true)
        }
    }

    pub fn vm_status(&self) -> Option<VmStatus> {
        self.last_status
    }

    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
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
            ExecState::Cancelled | ExecState::Suspended | ExecState::Posted => AccessStatus::Again,
            ExecState::New => self.run_and_arm(registry, waiter),
        }
    }

    fn run_and_arm(
        &mut self,
        registry: &mut IdentityRegistry<u64>,
        waiter: &impl AsyncWait,
    ) -> AccessStatus {
        let identity = self.identity;
        let phase = self.phase;
        let ctx = self.ctx_ptr();
        let disp = std::ptr::addr_of_mut!(self.disposition);
        let initial = match self.vm.as_mut() {
            Some(vm) => with_vm_entry(identity, phase, ctx, disp, || vm.run()),
            None => {
                self.state = ExecState::Failed;
                return AccessStatus::Error500;
            }
        };
        self.drive_status(initial, registry, waiter)
    }

    fn drive_status(
        &mut self,
        initial: VmResult<VmStatus>,
        registry: &mut IdentityRegistry<u64>,
        waiter: &impl AsyncWait,
    ) -> AccessStatus {
        self.drive_status_with_transitions(initial, 0, registry, waiter)
    }

    fn drive_status_with_transitions(
        &mut self,
        mut status: VmResult<VmStatus>,
        mut transitions: usize,
        registry: &mut IdentityRegistry<u64>,
        waiter: &impl AsyncWait,
    ) -> AccessStatus {
        loop {
            let current = match status {
                Ok(status) => status,
                Err(error) => return self.finish_from_vm_error(error),
            };
            match current {
                VmStatus::Halted => return self.finish_from_disposition(false),
                VmStatus::Yielded => {
                    self.last_error = Some("VM yielded while driving async host work".into());
                    self.state = ExecState::Failed;
                    return AccessStatus::Error500;
                }
                VmStatus::Waiting(op_id) => {
                    self.last_status = Some(VmStatus::Waiting(op_id));
                    if transitions >= MAX_IMMEDIATE_TRANSITIONS {
                        return self.post_budget_continuation(transitions, registry, waiter);
                    }
                    let waker = Waker::noop();
                    let mut cx = Context::from_waker(waker);
                    match self.poll_waiting_host_op(&mut cx) {
                        Poll::Ready(Ok(())) => {}
                        Poll::Ready(Err(error)) => return self.finish_from_vm_error(error),
                        Poll::Pending => match self.arm_wait(registry, waiter) {
                            WaitArm::Pending => return AccessStatus::Again,
                            WaitArm::Ready => {}
                            WaitArm::Failed => return AccessStatus::Error500,
                        },
                    }
                    transitions = transitions.saturating_add(1);
                    status = self.resume_vm();
                }
            }
        }
    }

    fn poll_waiting_host_op(&mut self, cx: &mut Context<'_>) -> Poll<VmResult<()>> {
        let identity = self.identity;
        let phase = self.phase;
        let disp = std::ptr::addr_of_mut!(self.disposition);
        match self.vm.as_mut() {
            Some(vm) => with_vm_entry(identity, phase, std::ptr::null_mut(), disp, || {
                vm.poll_waiting_host_op(cx)
            }),
            None => Poll::Ready(Err(VmError::HostError("missing VM".into()))),
        }
    }

    fn resume_vm(&mut self) -> VmResult<VmStatus> {
        let identity = self.identity;
        let phase = self.phase;
        let ctx = self.ctx_ptr();
        let disp = std::ptr::addr_of_mut!(self.disposition);
        match self.vm.as_mut() {
            Some(vm) => with_vm_entry(identity, phase, ctx, disp, || vm.resume()),
            None => Err(VmError::HostError("missing VM".into())),
        }
    }

    fn post_budget_continuation(
        &mut self,
        transitions: usize,
        registry: &mut IdentityRegistry<u64>,
        waiter: &impl AsyncWait,
    ) -> AccessStatus {
        if transitions == 0 || self.state != ExecState::New {
            self.last_error =
                Some("async budget continuation made no progress or re-entered".into());
            self.state = ExecState::Failed;
            return AccessStatus::Error500;
        }
        if registry.insert_new(self.identity, self.request_id).is_err() {
            self.last_error = Some("failed to register async budget continuation".into());
            self.state = ExecState::Failed;
            return AccessStatus::Error500;
        }
        self.state = ExecState::Posted;
        if waiter.post_continuation(self.identity).is_err() {
            registry.remove_exact(self.identity, &self.request_id);
            self.last_error = Some("failed to post async budget continuation".into());
            self.state = ExecState::Failed;
            return AccessStatus::Error500;
        }
        AccessStatus::Again
    }

    /// Run once on the worker main thread. Waiting/yield fail closed without
    /// arming eventfd or retaining the request.
    pub fn run_sync_fail_closed(&mut self) -> Result<(), String> {
        let identity = self.identity;
        let ctx = self.ctx_ptr();
        let disp = std::ptr::addr_of_mut!(self.disposition);
        let vm = match self.vm.as_mut() {
            Some(vm) => vm,
            None => {
                self.state = ExecState::Failed;
                return Err("missing VM".into());
            }
        };
        match with_vm_entry(identity, self.phase, ctx, disp, || vm.run()) {
            Ok(VmStatus::Halted) => {
                // Log phase must not apply disposition as an HTTP outcome.
                self.last_status = Some(VmStatus::Halted);
                self.disposition = RequestDisposition::Continue;
                self.state = ExecState::Halted;
                self.final_status = Some(AccessStatus::Ok);
                self.vm = None;
                self.release_owned_ctx();
                Ok(())
            }
            Ok(VmStatus::Waiting(_)) => {
                self.state = ExecState::Failed;
                let _ = self.cleanup();
                Err(LOG_CANNOT_WAIT.to_string())
            }
            Ok(VmStatus::Yielded) => {
                self.state = ExecState::Failed;
                let _ = self.cleanup();
                Err("log handler cannot yield".into())
            }
            Err(err) => {
                let mapped = fail_closed_log_status(Err(err));
                let _ = self.cleanup();
                mapped
            }
        }
    }

    fn arm_wait(
        &mut self,
        registry: &mut IdentityRegistry<u64>,
        waiter: &impl AsyncWait,
    ) -> WaitArm {
        #[cfg(ngx_rustscript_integration_hooks)]
        if self.phase == NgxPhase::Log {
            WAIT_EVENTFD_ARMS.fetch_add(1, Ordering::Relaxed);
        }
        let permit = match waiter.reserve() {
            Ok(permit) => permit,
            Err(_) => {
                self.last_error = Some("async host wait capacity unavailable".into());
                self.state = ExecState::Failed;
                return WaitArm::Failed;
            }
        };
        if registry.insert_new(self.identity, self.request_id).is_err() {
            drop(permit);
            self.last_error = Some("failed to register async host wait".into());
            self.state = ExecState::Failed;
            return WaitArm::Failed;
        }
        self.permit = Some(permit);
        let waker = waiter.waker(self.identity);
        let mut cx = Context::from_waker(&waker);
        match self.poll_waiting_host_op(&mut cx) {
            Poll::Pending => {
                self.state = ExecState::Suspended;
                WaitArm::Pending
            }
            Poll::Ready(Ok(())) => {
                registry.remove_exact(self.identity, &self.request_id);
                self.permit = None;
                WaitArm::Ready
            }
            Poll::Ready(Err(err)) => {
                registry.remove_exact(self.identity, &self.request_id);
                self.permit = None;
                self.last_error = Some(err.to_string());
                self.state = ExecState::Failed;
                WaitArm::Failed
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
        if !matches!(self.state, ExecState::Suspended | ExecState::Posted) {
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
        if self.state == ExecState::Posted {
            registry.remove_exact(self.identity, &self.request_id);
            self.state = ExecState::New;
            let Some(VmStatus::Waiting(op_id)) = self.last_status else {
                self.last_error =
                    Some("posted async continuation lost its waiting operation".into());
                self.state = ExecState::Failed;
                return ReadyEffect::Failed;
            };
            return match self.drive_status(Ok(VmStatus::Waiting(op_id)), registry, waiter) {
                AccessStatus::Ok | AccessStatus::Finalize(_) => ReadyEffect::Halted,
                AccessStatus::Again => ReadyEffect::WaitingAgain,
                AccessStatus::Error500 => ReadyEffect::Failed,
            };
        }
        if self.state != ExecState::Suspended {
            return ReadyEffect::Ignored;
        }
        let identity = self.identity;
        let waker = waiter.waker(identity);
        let mut cx = Context::from_waker(&waker);
        match self.poll_waiting_host_op(&mut cx) {
            Poll::Pending => ReadyEffect::Ignored,
            Poll::Ready(Ok(())) => {
                registry.remove_exact(self.identity, &self.request_id);
                self.permit = None;
                self.state = ExecState::New;
                let status = self.resume_vm();
                match self.drive_status_with_transitions(status, 1, registry, waiter) {
                    AccessStatus::Ok | AccessStatus::Finalize(_) => ReadyEffect::Halted,
                    AccessStatus::Again => ReadyEffect::WaitingAgain,
                    AccessStatus::Error500 => ReadyEffect::Failed,
                }
            }
            Poll::Ready(Err(err)) => {
                registry.remove_exact(self.identity, &self.request_id);
                self.permit = None;
                self.last_error = Some(err.to_string());
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
            self.release_owned_ctx();
            self.vm = None;
            self.permit = None;
            return None;
        }
        let identity = Some(self.identity);
        self.state = ExecState::Cancelled;
        self.deferred = false;
        self.permit = None;
        self.release_owned_ctx();
        self.vm = None;
        identity
    }

    /// Tombstone, drop the VM/future, then forget the tombstone.
    pub fn cleanup_wait(&mut self, waiter: &impl AsyncWait) -> Option<RequestIdentity> {
        if self.is_finished() {
            if self.state == ExecState::Halted
                && self.reusable_key.is_some()
                && waiter.recycle_finished_exec(self)
            {
                return None;
            }
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
    use crate::host::{StandardHostPolicy, live_owned_entry_count};
    use crate::notify::DEFAULT_MAX_IN_FLIGHT;
    use crate::runtime::WorkerRuntime;
    use std::sync::Arc;

    fn ready_chain_exec(count: usize, worker: &WorkerRuntime) -> AccessExec {
        let mut source = String::from("use ngx;\n");
        for _ in 0..count {
            source.push_str("ngx::ready();\n");
        }
        let program = vm::compile_source_with_flavor_and_options(
            &source,
            vm::SourceFlavor::RustScript,
            vm::CompileSourceFileOptions::default()
                .with_host_api_catalog(Arc::new(crate::host::test_bind_catalog())),
        )
        .expect("compile immediately-ready chain")
        .program;
        let mut vm = Vm::new(program);
        crate::host::bind_test_hosts(&mut vm);
        vm.set_async_bridge(Box::new(NgxAsyncBridge::new(
            worker.handle().expect("worker handle"),
        )))
        .expect("install production async bridge");
        AccessExec {
            vm: Some(vm),
            reusable_key: None,
            request_id: 1,
            identity: RequestIdentity::new(1, 1),
            phase: NgxPhase::Access,
            ctx: RequestLocalCtx::new(),
            shared_ctx: None,
            disposition: RequestDisposition::Continue,
            final_status: None,
            state: ExecState::New,
            suspend_committed: false,
            deferred: false,
            permit: None,
            last_status: None,
            last_error: None,
        }
    }

    #[test]
    fn more_than_1024_ready_transitions_complete_across_state_callbacks() {
        let mut worker = WorkerRuntime::start().expect("worker runtime");
        let mut exec = ready_chain_exec(1_089, &worker);
        let mut registry = IdentityRegistry::with_capacity(DEFAULT_MAX_IN_FLIGHT);

        assert_eq!(
            exec.handle_access(&mut registry, &worker),
            AccessStatus::Again
        );
        exec.commit_suspend();
        let mut callback_turns = 0;
        loop {
            let ready = worker.drain_ready().expect("drain next-event continuation");
            assert_eq!(ready, vec![exec.identity()]);
            callback_turns += 1;
            match exec.apply_ready(exec.identity(), &mut registry, &worker) {
                ReadyEffect::WaitingAgain => continue,
                ReadyEffect::Halted => break,
                effect => panic!("ready chain failed after {callback_turns} turns: {effect:?}"),
            }
        }

        assert!(
            callback_turns > 16,
            "chain must cross the former aggregate cap"
        );
        assert_eq!(exec.handle_access(&mut registry, &worker), AccessStatus::Ok);
        assert_eq!(registry.len(), 0);
        assert_eq!(worker.in_flight().expect("in-flight count"), 0);
        worker.shutdown();
    }

    #[test]
    fn pending_then_immediately_ready_chain_shares_one_event_turn_budget() {
        let mut source = String::from("use ngx;\n");
        for _ in 0..(MAX_IMMEDIATE_TRANSITIONS * 3 + 9) {
            source.push_str("ngx::pending_ready();\n");
        }
        let program = vm::compile_source_with_flavor_and_options(
            &source,
            vm::SourceFlavor::RustScript,
            vm::CompileSourceFileOptions::default()
                .with_host_api_catalog(Arc::new(crate::host::test_bind_catalog())),
        )
        .expect("compile pending-ready chain")
        .program;
        let mut worker = WorkerRuntime::start().expect("worker runtime");
        let mut vm = Vm::new(program);
        crate::host::bind_test_hosts(&mut vm);
        vm.set_async_bridge(Box::new(NgxAsyncBridge::new(
            worker.handle().expect("worker handle"),
        )))
        .expect("install production async bridge");
        let mut exec = AccessExec {
            vm: Some(vm),
            reusable_key: None,
            request_id: 1,
            identity: RequestIdentity::new(1, 1),
            phase: NgxPhase::Access,
            ctx: RequestLocalCtx::new(),
            shared_ctx: None,
            disposition: RequestDisposition::Continue,
            final_status: None,
            state: ExecState::New,
            suspend_committed: false,
            deferred: false,
            permit: None,
            last_status: None,
            last_error: None,
        };
        let mut registry = IdentityRegistry::with_capacity(DEFAULT_MAX_IN_FLIGHT);

        assert_eq!(
            exec.handle_access(&mut registry, &worker),
            AccessStatus::Again
        );
        assert!(!exec.wait_permit_armed());
        exec.commit_suspend();
        for expected in [
            ReadyEffect::WaitingAgain,
            ReadyEffect::WaitingAgain,
            ReadyEffect::Halted,
        ] {
            assert_eq!(worker.drain_ready().unwrap(), vec![exec.identity()]);
            assert_eq!(
                exec.apply_ready(exec.identity(), &mut registry, &worker),
                expected
            );
        }
        assert!(worker.drain_ready().unwrap().is_empty());
        assert_eq!(exec.handle_access(&mut registry, &worker), AccessStatus::Ok);
        assert_eq!(registry.len(), 0);
        assert_eq!(worker.in_flight().unwrap(), 0);
        worker.shutdown();
    }

    fn scripted_immediate_drive(
        budget: usize,
        polls: impl IntoIterator<Item = Poll<VmResult<()>>>,
        resumes: impl IntoIterator<Item = VmResult<VmStatus>>,
    ) -> (ImmediateDrive, usize, usize) {
        let mut polls = polls.into_iter();
        let mut resumes = resumes.into_iter();
        let mut poll_count = 0;
        let mut resume_count = 0;
        let outcome = drive_immediate_ready(
            Ok(VmStatus::Waiting(1)),
            budget,
            |_cx| {
                poll_count += 1;
                polls.next().expect("scripted poll")
            },
            || {
                resume_count += 1;
                resumes.next().expect("scripted resume")
            },
        );
        (outcome, poll_count, resume_count)
    }

    #[test]
    fn reusable_vm_key_isolates_standard_catalog_abi() {
        let program = compile("use ngx;\nngx::ctx::get(\"k\");\n");
        let prepared =
            PreparedScript::prepare(Arc::clone(&program), ScriptExecutionMetadata::default())
                .expect("prepare key program");
        let base = ReusableVmKey {
            prepared: Arc::clone(&prepared),
            phase: NgxPhase::Access,
            policy: StandardHostPolicy::fail_closed(),
            ngx_catalog_abi: crate::host::compile_host_catalog().fingerprint(),
            standard_catalog_abi: vm::standard_host_catalog_fingerprint(),
        };
        let different_abi = ReusableVmKey {
            prepared: Arc::clone(&prepared),
            phase: NgxPhase::Access,
            policy: StandardHostPolicy::fail_closed(),
            ngx_catalog_abi: crate::host::compile_host_catalog().fingerprint(),
            standard_catalog_abi: vm::HostApiCatalog::default().fingerprint(),
        };
        assert!(!base.matches(&different_abi));
    }

    #[test]
    fn reusable_vm_key_isolates_ngx_catalog_abi() {
        let program = compile("use ngx;\nngx::ctx::get(\"k\");\n");
        let prepared =
            PreparedScript::prepare(Arc::clone(&program), ScriptExecutionMetadata::default())
                .expect("prepare key program");
        let base = ReusableVmKey {
            prepared: Arc::clone(&prepared),
            phase: NgxPhase::Access,
            policy: StandardHostPolicy::fail_closed(),
            ngx_catalog_abi: crate::host::compile_host_catalog().fingerprint(),
            standard_catalog_abi: vm::standard_host_catalog_fingerprint(),
        };
        let different_abi = ReusableVmKey {
            prepared: Arc::clone(&prepared),
            phase: NgxPhase::Access,
            policy: StandardHostPolicy::fail_closed(),
            ngx_catalog_abi: vm::HostApiCatalog::default().fingerprint(),
            standard_catalog_abi: vm::standard_host_catalog_fingerprint(),
        };
        assert!(!base.matches(&different_abi));
    }

    #[test]
    fn immediate_ready_drive_coalesces_to_halt_without_pending_arm() {
        let (outcome, polls, resumes) = scripted_immediate_drive(
            MAX_IMMEDIATE_TRANSITIONS,
            [Poll::Ready(Ok(()))],
            [Ok(VmStatus::Halted)],
        );
        assert!(matches!(outcome, ImmediateDrive::Halted));
        assert_eq!(polls, 1);
        assert_eq!(resumes, 1);
    }

    #[test]
    fn real_pending_stops_before_resume_or_another_poll() {
        let (outcome, polls, resumes) = scripted_immediate_drive(
            MAX_IMMEDIATE_TRANSITIONS,
            [Poll::Pending],
            std::iter::empty(),
        );
        assert!(matches!(outcome, ImmediateDrive::Pending(1)));
        assert_eq!(polls, 1);
        assert_eq!(resumes, 0);
    }

    #[test]
    fn chained_immediate_futures_finish_in_one_drive() {
        let (outcome, polls, resumes) = scripted_immediate_drive(
            MAX_IMMEDIATE_TRANSITIONS,
            [
                Poll::Ready(Ok(())),
                Poll::Ready(Ok(())),
                Poll::Ready(Ok(())),
            ],
            [
                Ok(VmStatus::Waiting(2)),
                Ok(VmStatus::Waiting(3)),
                Ok(VmStatus::Halted),
            ],
        );
        assert!(matches!(outcome, ImmediateDrive::Halted));
        assert_eq!(polls, 3);
        assert_eq!(resumes, 3);
    }

    #[test]
    fn transition_budget_posts_before_polling_the_next_future() {
        let (outcome, polls, resumes) = scripted_immediate_drive(
            2,
            [Poll::Ready(Ok(())), Poll::Ready(Ok(()))],
            [Ok(VmStatus::Waiting(2)), Ok(VmStatus::Waiting(3))],
        );
        assert!(matches!(
            outcome,
            ImmediateDrive::Budget {
                op_id: 3,
                transitions: 2
            }
        ));
        assert_eq!(polls, 2);
        assert_eq!(resumes, 2);
    }

    #[test]
    fn immediate_ready_drive_preserves_vm_error() {
        let expected = "scripted host failure";
        let outcome = drive_immediate_ready(
            Err(VmError::HostError(expected.into())),
            MAX_IMMEDIATE_TRANSITIONS,
            |_cx| unreachable!("an initial error must not poll"),
            || unreachable!("an initial error must not resume"),
        );
        let ImmediateDrive::Failed(error) = outcome else {
            panic!("expected failed immediate drive");
        };
        assert!(error.to_string().contains(expected));
    }

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

    fn wait_readable(fd: std::os::unix::io::RawFd) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let mut pollfd = libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            };
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            assert!(
                !remaining.is_zero(),
                "eventfd {fd} was not readable within 10s"
            );
            let timeout_ms = remaining.as_millis().min(1_000) as libc::c_int;
            let rc = unsafe { libc::poll(&mut pollfd, 1, timeout_ms) };
            if rc > 0 {
                assert_ne!(
                    pollfd.revents & (libc::POLLIN | libc::POLLERR | libc::POLLHUP),
                    0,
                    "eventfd {fd} poll returned unexpected revents {}",
                    pollfd.revents
                );
                return;
            }
            if rc < 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                panic!("eventfd {fd} poll failed: {error}");
            }
        }
    }

    fn wait_ready(
        worker: &mut WorkerRuntime,
        exec: &mut AccessExec,
        registry: &mut IdentityRegistry<u64>,
    ) {
        let fd = worker.raw_fd().expect("eventfd");
        wait_readable(fd);
        let ready = worker.drain_ready().unwrap();
        assert_eq!(ready, vec![exec.identity()]);
        let effect = exec.apply_ready(exec.identity(), registry, worker);
        assert_eq!(effect, ReadyEffect::Halted);
    }

    fn ctx_debug(ctx: &RequestLocalCtx, key: &str) -> String {
        format!("{:?}", ctx.get(key))
    }

    fn assert_default_policy_denies(source: &str, expected_error: &str) {
        let program = compile(source);
        let mut worker = WorkerRuntime::start().expect("worker");
        let mut exec = worker
            .bind_phase_exec(program, 92, NgxPhase::Access)
            .expect("bind standard hosts");
        let mut registry = IdentityRegistry::with_capacity(8);
        let status = exec.handle_access(&mut registry, &worker);
        if status == AccessStatus::Again {
            exec.commit_suspend();
            'waits: for _ in 0..8 {
                let fd = worker.raw_fd().expect("eventfd");
                wait_readable(fd);
                for identity in worker.drain_ready().expect("ready identities") {
                    match exec.apply_ready(identity, &mut registry, &worker) {
                        ReadyEffect::Failed => break 'waits,
                        ReadyEffect::WaitingAgain | ReadyEffect::Ignored => {}
                        effect => panic!("expected policy denial, got {effect:?}"),
                    }
                }
            }
        } else {
            assert_eq!(status, AccessStatus::Error500);
        }
        let error = exec.last_error().unwrap_or("missing VM error");
        assert!(
            error.contains(expected_error),
            "expected {expected_error:?} in {error:?}"
        );
        worker.shutdown();
    }

    #[test]
    fn default_standard_host_policy_denies_io() {
        let root = std::env::temp_dir().join(format!(
            "ngx-rustscript-standard-deny-{}-{}",
            std::process::id(),
            crate::access::next_request_id()
        ));
        std::fs::create_dir(&root).expect("create denied root");
        let input = root.join("input.txt");
        std::fs::write(&input, "denied").expect("write denied input");

        assert_default_policy_denies(
            &format!(
                "use io;\nlet handle = io::open({:?}, \"r\");\nio::close(handle);\n",
                input.to_string_lossy()
            ),
            "outside the allowed roots",
        );
        std::fs::remove_dir_all(root).expect("remove denied root");
    }

    #[test]
    fn default_standard_host_policy_denies_http() {
        assert_default_policy_denies(
            "use http;\nhttp::client::request({\"method\": \"GET\", \"url\": \"http://127.0.0.1:9/\"});\n",
            "scheme 'http' is not allowed",
        );
    }

    #[test]
    fn default_standard_host_policy_denies_sqlite() {
        assert_default_policy_denies(
            "use sqlite;\nlet db = sqlite::open({\"path\": \"state.db\", \"mode\": \"read_write_create\"});\nsqlite::close(db);\n",
            "database root is not configured",
        );
    }

    #[test]
    fn request_owned_vm_runs_pd_vm_async_io_under_location_policy() {
        let root = std::env::temp_dir().join(format!(
            "ngx-rustscript-standard-io-{}-{}",
            std::process::id(),
            crate::access::next_request_id()
        ));
        std::fs::create_dir(&root).expect("create IO root");
        let input = root.join("input.txt");
        std::fs::write(&input, "policy-ok").expect("write input");
        let source = format!(
            "use io;\nlet handle = io::open({:?}, \"r\");\nlet text = io::read_all(handle);\nio::close(handle);\nif text != \"policy-ok\" {{ 1 / 0; }}\n",
            input.to_string_lossy()
        );
        let mut policy = StandardHostPolicy::fail_closed();
        policy.io_roots.push(root.to_string_lossy().into_owned());
        let program = compile(&source);
        assert!(
            crate::config::program_requires_request_state(&program),
            "async IO builtin must select request-owned execution:\n{}",
            vm::disassemble_program(&program)
        );
        let mut worker = WorkerRuntime::start().expect("worker");
        let mut exec = worker
            .bind_phase_exec_with_policy(program, 91, NgxPhase::Access, policy)
            .expect("bind standard hosts");
        let mut registry = IdentityRegistry::with_capacity(8);
        assert_eq!(
            exec.handle_access(&mut registry, &worker),
            AccessStatus::Again
        );
        exec.commit_suspend();
        for _ in 0..16 {
            let fd = worker.raw_fd().expect("eventfd");
            wait_readable(fd);
            for identity in worker.drain_ready().expect("ready identities") {
                match exec.apply_ready(identity, &mut registry, &worker) {
                    ReadyEffect::WaitingAgain | ReadyEffect::Ignored => {}
                    ReadyEffect::Halted => {
                        std::fs::remove_dir_all(root).expect("remove IO root");
                        worker.shutdown();
                        return;
                    }
                    effect => panic!("async IO failed: {effect:?}"),
                }
            }
        }
        std::fs::remove_dir_all(root).expect("remove IO root");
        worker.shutdown();
        panic!("async IO exceeded the maximum resume count");
    }

    #[test]
    fn request_owned_access_and_content_share_ctx_with_independent_identities() {
        let mut worker = WorkerRuntime::start().unwrap();
        let mut shared = RequestLocalCtx::new();
        let mut access = worker
            .bind_phase_exec(
                compile("use ngx;\nngx::ctx::set(\"k\", \"from-access\");\nngx::sleep_ms(5);\n"),
                1,
                NgxPhase::Access,
            )
            .expect("bind access");
        let mut content = worker
            .bind_phase_exec(
                compile(
                    "use ngx;\nngx::ctx::set(\"c\", ngx::ctx::get(\"k\"));\nngx::sleep_ms(5);\n",
                ),
                1,
                NgxPhase::Content,
            )
            .expect("bind content");
        assert_ne!(access.identity(), content.identity());
        assert_eq!(access.phase(), NgxPhase::Access);
        assert_eq!(content.phase(), NgxPhase::Content);
        access.attach_request_ctx(&mut shared);
        content.attach_request_ctx(&mut shared);

        let mut registry = IdentityRegistry::with_capacity(8);
        assert_eq!(
            access.handle_access(&mut registry, &worker),
            AccessStatus::Again
        );
        access.commit_suspend();
        wait_ready(&mut worker, &mut access, &mut registry);
        assert_eq!(
            access.handle_access(&mut registry, &worker),
            AccessStatus::Ok
        );
        assert!(access.is_finished());
        assert!(!content.is_finished());
        assert!(
            ctx_debug(&shared, "k").contains("from-access"),
            "access must write the request-owned ngx.ctx: {}",
            ctx_debug(&shared, "k")
        );

        assert_eq!(
            content.handle_access(&mut registry, &worker),
            AccessStatus::Again
        );
        content.commit_suspend();
        wait_ready(&mut worker, &mut content, &mut registry);
        assert_eq!(
            content.handle_access(&mut registry, &worker),
            AccessStatus::Ok
        );
        assert!(content.is_finished());
        assert!(
            ctx_debug(&shared, "c").contains("from-access"),
            "content must read the same ngx.ctx: {}",
            ctx_debug(&shared, "c")
        );
        worker.shutdown();
    }

    #[test]
    fn access_and_content_finalize_independently_on_shared_ctx() {
        let mut worker = WorkerRuntime::start().unwrap();
        let mut shared = RequestLocalCtx::new();
        let mut access = worker
            .bind_phase_exec(
                compile("use ngx;\nngx::ctx::set(\"k\", \"from-access\");\nngx::exit(403);\n"),
                7,
                NgxPhase::Access,
            )
            .expect("bind access");
        let mut content = worker
            .bind_phase_exec(
                compile("use ngx;\nngx::ctx::set(\"c\", ngx::ctx::get(\"k\"));\n"),
                7,
                NgxPhase::Content,
            )
            .expect("bind content");
        access.attach_request_ctx(&mut shared);
        content.attach_request_ctx(&mut shared);
        let mut registry = IdentityRegistry::with_capacity(8);
        assert_eq!(
            access.handle_access(&mut registry, &worker),
            AccessStatus::Finalize(403)
        );
        assert!(access.is_finished());
        assert!(!content.is_finished());
        assert_eq!(
            content.handle_access(&mut registry, &worker),
            AccessStatus::Ok
        );
        assert!(content.is_finished());
        assert!(ctx_debug(&shared, "k").contains("from-access"));
        assert!(ctx_debug(&shared, "c").contains("from-access"));
        worker.shutdown();
    }

    #[test]
    fn content_phase_cancelled_wait_is_ignored_on_stale_wake() {
        let mut worker = WorkerRuntime::start().unwrap();
        let mut exec = worker
            .bind_phase_exec(
                compile("use ngx;\nngx::sleep_ms(80);\n"),
                8,
                NgxPhase::Content,
            )
            .expect("bind content");
        let mut registry = IdentityRegistry::with_capacity(8);
        assert_eq!(
            exec.handle_access(&mut registry, &worker),
            AccessStatus::Again
        );
        exec.commit_suspend();
        let identity = exec.identity();
        let _ = registry.remove_exact(identity, &8);
        assert_eq!(exec.cleanup_wait(&worker), Some(identity));
        let effect = exec.apply_ready(identity, &mut registry, &worker);
        assert_eq!(effect, ReadyEffect::Ignored);
        worker.shutdown();
    }
}
