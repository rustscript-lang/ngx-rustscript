//! Generation-safe NGINX callback adapter.
//!
//! Request-owned operation state lives on `RequestPhaseCtx` and may hold only
//! worker-thread callback metadata (generation, identity, cell, waiting).
//! Completion is an owned Send cell. [`NginxOpFuture`] observers hold only
//! `Arc<cell>` + generation + [`RequestIdentity`] — never a request, VM,
//! module ctx, or raw pointer. Worker-thread NGINX callbacks resolve module
//! ctx and identity first, copy only owned results, complete exactly once, and
//! wake the generic bridge. Cleanup cancels exactly once; late or stale
//! callbacks are no-ops. Body-specific errors and read-handler wrapping live
//! outside this primitive.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

use vm::VmError;

use crate::notify::RequestIdentity;

/// Owned completion outcome. No NGINX pointers and no host-specific errors.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NginxOpOutcome {
    Done,
    Special(i32),
    Cancelled,
}

/// Send completion cell shared by the request-owned op and HostFuture observers.
pub struct NginxOpCell {
    generation: u64,
    identity: RequestIdentity,
    completed: AtomicBool,
    cancelled: AtomicBool,
    outcome: Mutex<Option<NginxOpOutcome>>,
    waker: Mutex<Option<Waker>>,
}

impl NginxOpCell {
    pub fn new(generation: u64, identity: RequestIdentity) -> Self {
        Self {
            generation,
            identity,
            completed: AtomicBool::new(false),
            cancelled: AtomicBool::new(false),
            outcome: Mutex::new(None),
            waker: Mutex::new(None),
        }
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn identity(&self) -> RequestIdentity {
        self.identity
    }

    /// Complete exactly once when generation and identity match and the cell
    /// has not been cancelled. Returns `false` for duplicate/stale/cancelled.
    pub fn complete(
        &self,
        generation: u64,
        identity: RequestIdentity,
        outcome: NginxOpOutcome,
    ) -> bool {
        if self.cancelled.load(Ordering::Acquire) {
            return false;
        }
        if self.generation != generation || self.identity != identity {
            return false;
        }
        if self.completed.swap(true, Ordering::AcqRel) {
            return false;
        }
        if let Ok(mut slot) = self.outcome.lock() {
            *slot = Some(outcome);
        }
        if let Ok(mut waker) = self.waker.lock() {
            if let Some(waker) = waker.take() {
                waker.wake();
            }
        }
        true
    }

    /// Cancel exactly once. A later complete is a no-op.
    pub fn cancel(&self) -> bool {
        if self.cancelled.swap(true, Ordering::AcqRel) {
            return false;
        }
        if self.completed.swap(true, Ordering::AcqRel) {
            return false;
        }
        if let Ok(mut slot) = self.outcome.lock() {
            *slot = Some(NginxOpOutcome::Cancelled);
        }
        if let Ok(mut waker) = self.waker.lock() {
            if let Some(waker) = waker.take() {
                waker.wake();
            }
        }
        true
    }

    pub fn is_completed(&self) -> bool {
        self.completed.load(Ordering::Acquire)
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    fn take_ready(&self) -> Option<NginxOpOutcome> {
        if !self.completed.load(Ordering::Acquire) {
            return None;
        }
        self.outcome.lock().ok().and_then(|mut slot| slot.take())
    }
}

/// HostFuture observer. Must not hold request/VM/module ctx/raw pointers.
pub struct NginxOpFuture {
    cell: Arc<NginxOpCell>,
    generation: u64,
    identity: RequestIdentity,
}

impl NginxOpFuture {
    pub fn observe(cell: Arc<NginxOpCell>, generation: u64, identity: RequestIdentity) -> Self {
        Self {
            cell,
            generation,
            identity,
        }
    }
}

impl Future for NginxOpFuture {
    type Output = Result<NginxOpOutcome, VmError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if self.cell.generation != self.generation || self.cell.identity != self.identity {
            return Poll::Ready(Err(VmError::HostError("nginx op stale wait".to_string())));
        }
        if let Some(outcome) = self.cell.take_ready() {
            return Poll::Ready(Ok(outcome));
        }
        if let Ok(mut waker) = self.cell.waker.lock() {
            *waker = Some(cx.waker().clone());
        }
        if let Some(outcome) = self.cell.take_ready() {
            return Poll::Ready(Ok(outcome));
        }
        Poll::Pending
    }
}

/// Request-owned callback operation. Lives on `RequestPhaseCtx`.
///
/// Holds only worker-thread callback metadata. Read-event wrapping, body
/// buffers, and host error strings are not stored here.
#[derive(Default)]
pub struct NginxOpState {
    generation: u64,
    identity: Option<RequestIdentity>,
    cell: Option<Arc<NginxOpCell>>,
    waiting: bool,
}

impl NginxOpState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn waiting(&self) -> bool {
        self.waiting
    }

    pub fn set_waiting(&mut self, waiting: bool) {
        self.waiting = waiting;
    }

    /// Begin a new operation. Cancels any previous cell exactly once.
    pub fn begin(&mut self, identity: RequestIdentity) -> Arc<NginxOpCell> {
        self.cancel();
        self.generation = self.generation.wrapping_add(1);
        if self.generation == 0 {
            self.generation = 1;
        }
        self.identity = Some(identity);
        let cell = Arc::new(NginxOpCell::new(self.generation, identity));
        self.cell = Some(Arc::clone(&cell));
        self.waiting = true;
        cell
    }

    pub fn observe(&self) -> Option<NginxOpFuture> {
        let cell = self.cell.as_ref()?;
        let identity = self.identity?;
        Some(NginxOpFuture::observe(
            Arc::clone(cell),
            self.generation,
            identity,
        ))
    }

    pub fn cell_completed(&self) -> bool {
        self.cell.as_ref().is_some_and(|cell| cell.is_completed())
    }

    /// Worker-thread callback: resolve identity/generation, complete once, wake.
    pub fn complete_from_callback(&mut self, outcome: NginxOpOutcome) -> bool {
        let Some(cell) = self.cell.clone() else {
            return false;
        };
        let Some(identity) = self.identity else {
            return false;
        };
        let ok = cell.complete(self.generation, identity, outcome);
        if ok {
            self.waiting = false;
        }
        ok
    }

    /// Cleanup: cancel exactly once. Late callbacks no-op.
    pub fn cancel(&mut self) {
        self.waiting = false;
        if let Some(cell) = self.cell.take() {
            cell.cancel();
        }
        self.identity = None;
    }
}

impl Drop for NginxOpState {
    fn drop(&mut self) {
        self.cancel();
    }
}
