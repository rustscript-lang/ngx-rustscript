//! Worker-main-thread request execution scope.
//!
//! Synchronous request-aware hosts may read the current NGINX request only
//! while a [`RequestExecutionScope`] is live on this worker thread. The guard
//! is a RAII stack: nested entries restore the outer request, and `Drop` runs
//! on panic unwind.
//!
//! Async host futures must capture only owned `Send + 'static` data. Copy
//! method, URI, header, and similar values before `.await`. Do not hold this
//! scope, a request reference, a pool, a VM, or a raw NGINX pointer across
//! `.await`.

use std::cell::RefCell;
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

thread_local! {
    static STACK: RefCell<Vec<ScopeFrame>> = const { RefCell::new(Vec::new()) };
}

/// Worker-thread-only RAII guard. Structurally `!Send` and `!Sync`.
pub struct RequestExecutionScope {
    _not_send_sync: PhantomData<*const ()>,
}

impl RequestExecutionScope {
    /// Push the current request onto the worker-thread scope stack.
    pub fn enter(
        request: *mut ngx_http_request_t,
        identity: RequestIdentity,
        phase: NgxPhase,
        allowed: AllowedOps,
    ) -> Self {
        STACK.with(|stack| {
            stack.borrow_mut().push(ScopeFrame {
                request,
                identity,
                phase,
                allowed,
            });
        });
        Self {
            _not_send_sync: PhantomData,
        }
    }

    /// Nest a copy of the live frame around `f`, or run `f` with no scope.
    pub fn reenter_current<R>(f: impl FnOnce() -> R) -> R {
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
    /// Reuses the live request pointer when an outer access scope already
    /// installed one; otherwise the frame is identity-only.
    pub fn enter_for_vm(identity: RequestIdentity, phase: NgxPhase, allowed: AllowedOps) -> Self {
        let request = current_request_ptr().unwrap_or(std::ptr::null_mut());
        Self::enter(request, identity, phase, allowed)
    }
}

impl Drop for RequestExecutionScope {
    fn drop(&mut self) {
        STACK.with(|stack| {
            let _ = stack.borrow_mut().pop();
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

pub(crate) fn current_request_ptr() -> Option<*mut ngx_http_request_t> {
    current_frame().ok().map(|frame| frame.request)
}

/// Run `f` with the live NGINX request after verifying `expected`.
pub fn with_current_request<R>(
    expected: RequestIdentity,
    f: impl FnOnce(&Request) -> R,
) -> Result<R, ScopeError> {
    let frame = verified_frame(expected)?;
    // SAFETY: worker main thread, identity/generation matched, pointer came
    // from [`RequestExecutionScope::enter`] and the frame is still on the stack.
    let request = unsafe { Request::from_ngx_http_request(frame.request) };
    Ok(f(request))
}

/// Mutable variant of [`with_current_request`].
pub fn with_current_request_mut<R>(
    expected: RequestIdentity,
    f: impl FnOnce(&mut Request) -> R,
) -> Result<R, ScopeError> {
    let frame = verified_frame(expected)?;
    // SAFETY: same as [`with_current_request`]; exclusive because this is the
    // worker main thread and hosts run synchronously inside the VM entry.
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
    #[test]
    fn request_execution_scope_is_not_send_or_sync() {
        super::_compile_time_scope_is_not_send_sync();
    }
}
