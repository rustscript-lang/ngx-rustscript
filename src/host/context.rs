//! Worker-main-thread request execution scope.
//!
//! Synchronous request-aware hosts may read the current NGINX request only
//! while a request execution scope is live on this worker thread. Public
//! entry is closure-scoped: nested entries restore the outer request, and
//! `Drop` runs on panic unwind.
//!
//! Async host futures must capture only owned `Send + 'static` data. Copy
//! method, URI, header, and similar values before `.await`. Do not hold a
//! request reference, a pool, a VM, or a raw NGINX pointer across `.await`.

use std::cell::{Cell, RefCell};
use std::marker::PhantomData;

use nginx_sys::ngx_http_request_t;
use ngx::http::Request;

use crate::host::catalog::NgxPhase;
use crate::notify::RequestIdentity;

/// Operations a request-aware host may perform while a scope is active.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AllowedOps {
    bits: u32,
}

impl AllowedOps {
    pub const EMPTY: Self = Self { bits: 0 };
    pub const REQUEST_READ: Self = Self { bits: 1 << 0 };
    pub const REQUEST_WRITE: Self = Self { bits: 1 << 1 };
    pub const LOG: Self = Self { bits: 1 << 2 };

    pub const fn union(self, other: Self) -> Self {
        Self {
            bits: self.bits | other.bits,
        }
    }

    pub const fn contains(self, other: Self) -> bool {
        self.bits & other.bits == other.bits
    }

    /// Default host operations for a script phase. Access currently allows
    /// request reads and logging; mutation APIs arrive in later tasks.
    pub fn for_phase(phase: NgxPhase) -> Self {
        match phase {
            NgxPhase::Rewrite | NgxPhase::Access | NgxPhase::Content => {
                Self::REQUEST_READ.union(Self::LOG)
            }
            _ => Self::EMPTY,
        }
    }
}

/// Failure when a host inspects the current request outside a valid scope.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScopeError {
    /// No VM `run` / `poll_waiting_host_op` / `resume` stack is active.
    NotInRequestScope,
    /// The caller asked for a different request id than the live scope.
    IdentityMismatch,
    /// The request id matches but the generation does not.
    StaleGeneration,
    /// The live scope has a null request pointer.
    NullRequest,
    /// The live scope does not allow the requested read or write.
    OpsNotAllowed,
    /// Mutable request access is already active on this thread.
    AlreadyBorrowed,
}

impl std::fmt::Display for ScopeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotInRequestScope => {
                write!(f, "ngx request host called outside request execution scope")
            }
            Self::IdentityMismatch => write!(
                f,
                "ngx request host identity does not match the current scope"
            ),
            Self::StaleGeneration => {
                write!(f, "ngx request host identity generation is stale")
            }
            Self::NullRequest => write!(f, "ngx request execution scope has a null request"),
            Self::OpsNotAllowed => {
                write!(f, "ngx request host is not allowed by the current scope")
            }
            Self::AlreadyBorrowed => {
                write!(f, "ngx request host cannot create nested mutable aliases")
            }
        }
    }
}

impl std::error::Error for ScopeError {}

#[derive(Clone, Copy)]
struct ScopeFrame {
    request: *mut ngx_http_request_t,
    identity: RequestIdentity,
    phase: NgxPhase,
    allowed: AllowedOps,
}

#[derive(Clone, Copy)]
enum RequestBorrow {
    None,
    Shared(u32),
    Exclusive,
}

thread_local! {
    static STACK: RefCell<Vec<ScopeFrame>> = const { RefCell::new(Vec::new()) };
    static VM_CAPTURE: RefCell<Option<Vec<VmEntryObservation>>> = const { RefCell::new(None) };
    static REQUEST_BORROW: Cell<RequestBorrow> = const { Cell::new(RequestBorrow::None) };
}

/// Snapshot of a VM `run` / `poll_waiting_host_op` / `resume` entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VmEntryObservation {
    pub identity: RequestIdentity,
    pub phase: NgxPhase,
    pub allowed: AllowedOps,
    pub request_live: bool,
}

/// Worker-thread-only RAII guard. Structurally `!Send` and `!Sync`.
pub(crate) struct RequestExecutionScope {
    _not_send_sync: PhantomData<*const ()>,
    depth: usize,
}

impl RequestExecutionScope {
    /// Push the current request onto the worker-thread scope stack.
    pub(crate) fn enter(
        request: *mut ngx_http_request_t,
        identity: RequestIdentity,
        phase: NgxPhase,
        allowed: AllowedOps,
    ) -> Self {
        let depth = STACK.with(|stack| {
            let mut stack = stack.borrow_mut();
            stack.push(ScopeFrame {
                request,
                identity,
                phase,
                allowed,
            });
            stack.len()
        });
        Self {
            _not_send_sync: PhantomData,
            depth,
        }
    }

    /// Nest a copy of the live frame around `f`, or run `f` with no scope.
    #[allow(dead_code)]
    pub(crate) fn reenter_current<R>(f: impl FnOnce() -> R) -> R {
        let frame = STACK.with(|stack| stack.borrow().last().copied());
        match frame {
            Some(frame) => {
                let _scope = Self::enter(frame.request, frame.identity, frame.phase, frame.allowed);
                f()
            }
            None => f(),
        }
    }

    /// Enter a VM `run` / `poll_waiting_host_op` / `resume` frame.
    ///
    /// Uses the outer request pointer only when that frame has the exact same
    /// generation-safe identity and a non-null pointer. A mismatched parent
    /// identity never authorizes its pointer under the VM identity.
    pub(crate) fn enter_for_vm(
        identity: RequestIdentity,
        phase: NgxPhase,
        allowed: AllowedOps,
    ) -> Self {
        let request = matching_outer_request(identity);
        record_vm_entry(identity, phase, allowed, !request.is_null());
        Self::enter(request, identity, phase, allowed)
    }
}

fn matching_outer_request(identity: RequestIdentity) -> *mut ngx_http_request_t {
    match current_frame() {
        Ok(frame) if frame.identity == identity && !frame.request.is_null() => frame.request,
        _ => std::ptr::null_mut(),
    }
}

/// Install a production request scope for the duration of `f`.
pub fn with_request_scope<R>(
    request: *mut ngx_http_request_t,
    identity: RequestIdentity,
    phase: NgxPhase,
    allowed: AllowedOps,
    f: impl FnOnce() -> R,
) -> R {
    let _scope = RequestExecutionScope::enter(request, identity, phase, allowed);
    f()
}

/// Install a VM entry scope for the duration of `f`.
pub fn with_vm_request_scope<R>(
    identity: RequestIdentity,
    phase: NgxPhase,
    allowed: AllowedOps,
    f: impl FnOnce() -> R,
) -> R {
    let _scope = RequestExecutionScope::enter_for_vm(identity, phase, allowed);
    f()
}

fn record_vm_entry(
    identity: RequestIdentity,
    phase: NgxPhase,
    allowed: AllowedOps,
    request_live: bool,
) {
    VM_CAPTURE.with(|capture| {
        if let Some(entries) = capture.borrow_mut().as_mut() {
            entries.push(VmEntryObservation {
                identity,
                phase,
                allowed,
                request_live,
            });
        }
    });
}

/// Record VM entry snapshots while `f` runs.
pub fn capture_vm_entries<R>(f: impl FnOnce() -> R) -> (R, Vec<VmEntryObservation>) {
    VM_CAPTURE.with(|capture| {
        *capture.borrow_mut() = Some(Vec::new());
    });
    let result = f();
    let entries = VM_CAPTURE.with(|capture| capture.borrow_mut().take().unwrap_or_default());
    (result, entries)
}

impl Drop for RequestExecutionScope {
    fn drop(&mut self) {
        STACK.with(|stack| {
            let mut stack = stack.borrow_mut();
            while stack.len() >= self.depth {
                stack.pop();
            }
        });
    }
}

fn current_frame() -> Result<ScopeFrame, ScopeError> {
    STACK.with(|stack| {
        stack
            .borrow()
            .last()
            .copied()
            .ok_or(ScopeError::NotInRequestScope)
    })
}

fn verified_frame(expected: RequestIdentity) -> Result<ScopeFrame, ScopeError> {
    let frame = current_frame()?;
    if frame.identity.request_id() != expected.request_id() {
        return Err(ScopeError::IdentityMismatch);
    }
    if frame.identity.generation() != expected.generation() {
        return Err(ScopeError::StaleGeneration);
    }
    if frame.request.is_null() {
        return Err(ScopeError::NullRequest);
    }
    Ok(frame)
}

struct BorrowGuard {
    prev: RequestBorrow,
}

impl Drop for BorrowGuard {
    fn drop(&mut self) {
        REQUEST_BORROW.with(|cell| cell.set(self.prev));
    }
}

fn acquire_shared() -> Result<BorrowGuard, ScopeError> {
    REQUEST_BORROW.with(|cell| {
        let prev = cell.get();
        match prev {
            RequestBorrow::Exclusive => Err(ScopeError::AlreadyBorrowed),
            RequestBorrow::None => {
                cell.set(RequestBorrow::Shared(1));
                Ok(BorrowGuard { prev })
            }
            RequestBorrow::Shared(n) => {
                cell.set(RequestBorrow::Shared(n.saturating_add(1)));
                Ok(BorrowGuard { prev })
            }
        }
    })
}

fn acquire_exclusive() -> Result<BorrowGuard, ScopeError> {
    REQUEST_BORROW.with(|cell| {
        let prev = cell.get();
        match prev {
            RequestBorrow::None => {
                cell.set(RequestBorrow::Exclusive);
                Ok(BorrowGuard { prev })
            }
            RequestBorrow::Shared(_) | RequestBorrow::Exclusive => Err(ScopeError::AlreadyBorrowed),
        }
    })
}

/// True while at least one request execution scope is live on this thread.
pub fn is_active() -> bool {
    STACK.with(|stack| !stack.borrow().is_empty())
}

/// Identity of the current request, if a scope is active.
pub fn current_identity() -> Result<RequestIdentity, ScopeError> {
    current_frame().map(|frame| frame.identity)
}

/// Phase recorded by the live scope.
pub fn current_phase() -> Result<NgxPhase, ScopeError> {
    current_frame().map(|frame| frame.phase)
}

/// Allowed operations recorded by the live scope.
pub fn current_allowed_ops() -> Result<AllowedOps, ScopeError> {
    current_frame().map(|frame| frame.allowed)
}

/// Whether the live scope holds a non-null request pointer.
pub fn current_request_live() -> Result<bool, ScopeError> {
    current_frame().map(|frame| !frame.request.is_null())
}

#[allow(dead_code)] // crate-private helper; tests assert it never returns Some(null)
pub(crate) fn current_request_ptr() -> Option<*mut ngx_http_request_t> {
    current_frame().ok().and_then(|frame| {
        if frame.request.is_null() {
            None
        } else {
            Some(frame.request)
        }
    })
}

/// Run `f` with the live NGINX request after verifying `expected`.
///
/// The HRTB keeps the `ngx::Request` borrow inside `f`, so a request
/// reference cannot be stored past the closure or across `.await`.
pub fn with_current_request<R>(
    expected: RequestIdentity,
    f: impl for<'a> FnOnce(&'a Request) -> R,
) -> Result<R, ScopeError> {
    let frame = verified_frame(expected)?;
    if !frame.allowed.contains(AllowedOps::REQUEST_READ) {
        return Err(ScopeError::OpsNotAllowed);
    }
    let _borrow = acquire_shared()?;
    // SAFETY: worker main thread, identity/generation matched, pointer came
    // from [`RequestExecutionScope::enter`] and the frame is still on the stack.
    let request = unsafe { Request::from_ngx_http_request(frame.request) };
    Ok(f(request))
}

/// Mutable variant of [`with_current_request`].
///
/// Nested mutable (or mixed shared/mutable) accessors are rejected so this
/// cannot create aliasing `&mut Request` values.
pub fn with_current_request_mut<R>(
    expected: RequestIdentity,
    f: impl for<'a> FnOnce(&'a mut Request) -> R,
) -> Result<R, ScopeError> {
    let frame = verified_frame(expected)?;
    if !frame.allowed.contains(AllowedOps::REQUEST_WRITE) {
        return Err(ScopeError::OpsNotAllowed);
    }
    let _borrow = acquire_exclusive()?;
    // SAFETY: same as [`with_current_request`]; exclusive because [`acquire_exclusive`]
    // rejected any overlapping request borrow on this thread.
    let request = unsafe { Request::from_ngx_http_request(frame.request) };
    Ok(f(request))
}

macro_rules! assert_not_impl_any {
    ($x:ty: $($t:path),+ $(,)?) => {{
        #[allow(dead_code)]
        struct Invalid;
        trait AmbiguousIfImpl<A> {
            fn some_item() {}
        }
        impl<T: ?Sized> AmbiguousIfImpl<()> for T {}
        $( {
            #[allow(dead_code)]
            struct UniqueSized;
            impl<T: ?Sized + $t> AmbiguousIfImpl<UniqueSized> for T {}
        } )+
        let _ = <$x as AmbiguousIfImpl<_>>::some_item;
    }};
}

fn _compile_time_scope_is_not_send_sync() {
    assert_not_impl_any!(RequestExecutionScope: Send, Sync);
}

#[cfg(test)]
mod not_send_sync {
    use super::*;
    use crate::host::catalog::NgxPhase;
    use crate::notify::RequestIdentity;

    #[test]
    fn request_execution_scope_is_not_send_or_sync() {
        super::_compile_time_scope_is_not_send_sync();
    }

    #[test]
    fn current_request_ptr_never_returns_some_null() {
        let identity = RequestIdentity::new(1, 1);
        let _scope = RequestExecutionScope::enter(
            std::ptr::null_mut(),
            identity,
            NgxPhase::Access,
            AllowedOps::for_phase(NgxPhase::Access),
        );
        assert_eq!(current_request_ptr(), None);
        assert_eq!(current_request_live(), Ok(false));
    }

    #[test]
    fn out_of_order_drop_clears_inner_and_outer() {
        let identity = RequestIdentity::new(2, 2);
        let outer = RequestExecutionScope::enter(
            std::ptr::null_mut(),
            identity,
            NgxPhase::Access,
            AllowedOps::for_phase(NgxPhase::Access),
        );
        let inner = RequestExecutionScope::enter(
            std::ptr::null_mut(),
            RequestIdentity::new(3, 3),
            NgxPhase::Access,
            AllowedOps::for_phase(NgxPhase::Access),
        );
        assert!(is_active());
        drop(outer);
        assert!(!is_active());
        drop(inner);
        assert!(!is_active());
        assert_eq!(current_identity(), Err(ScopeError::NotInRequestScope));
    }
}
