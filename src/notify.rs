//! Linux eventfd wake bridge and bounded worker-local identity queue.
//!
//! Generic wakers hold only a [`CompletionSender`] and a
//! [`RequestIdentity`]. They enqueue the identity under a mutex, then write
//! the eventfd. They never call NGINX APIs and never hold VM/pool/request
//! pointers. The worker main thread registers the fd with NGINX and harvests
//! identities for exact-generation lookup.

use std::collections::{HashSet, VecDeque};
use std::fmt;
use std::os::unix::io::RawFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Wake, Waker};

use nginx_sys::{
    NGX_USE_EPOLL_EVENT, ngx_close_connection, ngx_connection_t, ngx_cycle_t, ngx_event_actions,
    ngx_event_flags, ngx_free_connection, ngx_get_connection, ngx_int_t, ngx_post_event,
    ngx_posted_next_events, ngx_uint_t,
};

/// Maximum in-flight async host waits per worker.
pub const DEFAULT_MAX_IN_FLIGHT: usize = 1024;

/// Generation-safe request identity. Never a pointer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RequestIdentity {
    generation: u64,
    request_id: u64,
}

impl RequestIdentity {
    pub fn new(generation: u64, request_id: u64) -> Self {
        Self {
            generation,
            request_id,
        }
    }

    pub fn generation(self) -> u64 {
        self.generation
    }

    pub fn request_id(self) -> u64 {
        self.request_id
    }
}

/// Fail-closed notify / queue errors. Never panics on mutex poison.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotifyError {
    QueueFull,
    ShutDown,
    Poisoned,
    EventFdCreate(i32),
    EventFdRead(i32),
    EventFdWrite(i32),
    EventFdShortRead(usize),
    EventFdShortWrite(usize),
    EventFdEof,
    GetConnection,
    AddEvent,
}

impl fmt::Display for NotifyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::QueueFull => write!(f, "async host wait queue is full"),
            Self::ShutDown => write!(f, "worker runtime is shut down"),
            Self::Poisoned => write!(f, "async host wait queue mutex is poisoned"),
            Self::EventFdCreate(err) => write!(f, "eventfd create failed: errno {err}"),
            Self::EventFdRead(err) => write!(f, "eventfd read failed: errno {err}"),
            Self::EventFdWrite(err) => write!(f, "eventfd write failed: errno {err}"),
            Self::EventFdShortRead(n) => write!(f, "eventfd short read of {n} bytes"),
            Self::EventFdShortWrite(n) => write!(f, "eventfd short write of {n} bytes"),
            Self::EventFdEof => write!(f, "eventfd returned EOF"),
            Self::GetConnection => write!(f, "ngx_get_connection failed for eventfd"),
            Self::AddEvent => write!(f, "ngx_add_event failed for eventfd"),
        }
    }
}

impl std::error::Error for NotifyError {}

/// Injected eventfd syscalls so tests can force EINTR / EAGAIN / short / hard errors.
pub trait EventFdIo: Send + Sync {
    fn eventfd(&self, initval: u32, flags: i32) -> Result<RawFd, i32>;
    fn read(&self, fd: RawFd, buf: &mut [u8]) -> Result<usize, i32>;
    fn write(&self, fd: RawFd, buf: &[u8]) -> Result<usize, i32>;
    fn close(&self, fd: RawFd) -> Result<(), i32>;
    fn fcntl(&self, fd: RawFd, cmd: i32) -> Result<i32, i32>;
}

/// Production libc adapter.
#[derive(Debug, Default, Clone, Copy)]
pub struct LibcEventFdIo;

fn last_errno() -> i32 {
    std::io::Error::last_os_error()
        .raw_os_error()
        .unwrap_or(libc::EIO)
}

impl EventFdIo for LibcEventFdIo {
    fn eventfd(&self, initval: u32, flags: i32) -> Result<RawFd, i32> {
        let fd = unsafe { libc::eventfd(initval, flags) };
        if fd < 0 { Err(last_errno()) } else { Ok(fd) }
    }

    fn read(&self, fd: RawFd, buf: &mut [u8]) -> Result<usize, i32> {
        let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
        if n < 0 {
            Err(last_errno())
        } else {
            Ok(n as usize)
        }
    }

    fn write(&self, fd: RawFd, buf: &[u8]) -> Result<usize, i32> {
        let n = unsafe { libc::write(fd, buf.as_ptr().cast(), buf.len()) };
        if n < 0 {
            Err(last_errno())
        } else {
            Ok(n as usize)
        }
    }

    fn close(&self, fd: RawFd) -> Result<(), i32> {
        let n = unsafe { libc::close(fd) };
        if n < 0 { Err(last_errno()) } else { Ok(()) }
    }

    fn fcntl(&self, fd: RawFd, cmd: i32) -> Result<i32, i32> {
        let n = unsafe { libc::fcntl(fd, cmd) };
        if n < 0 { Err(last_errno()) } else { Ok(n) }
    }
}

pub(crate) struct Pending {
    pub(crate) queue: VecDeque<RequestIdentity>,
    queued: HashSet<RequestIdentity>,
    in_flight: usize,
    cap: usize,
    cancelled: HashSet<RequestIdentity>,
}

#[derive(Clone, Copy)]
enum FdInner {
    Open(RawFd),
    Closed,
}

/// RAII in-flight slot held by the waiting request until Ready/error/cancel.
pub struct CompletionPermit {
    pending: Arc<Mutex<Pending>>,
    live: bool,
}

impl Drop for CompletionPermit {
    fn drop(&mut self) {
        if !self.live {
            return;
        }
        self.live = false;
        if let Ok(mut pending) = self.pending.lock() {
            pending.in_flight = pending.in_flight.saturating_sub(1);
        }
    }
}

/// Send/Sync handle owned by generic wakers: this handle is the queue sender
/// / eventfd writer. Duplicate wakes are coalesced in the identity queue.
#[derive(Clone)]
pub struct CompletionSender {
    fd: Arc<Mutex<FdInner>>,
    accepting: Arc<AtomicBool>,
    io: Arc<dyn EventFdIo>,
    pending: Arc<Mutex<Pending>>,
    wake_error: Arc<Mutex<Option<NotifyError>>>,
}

impl CompletionSender {
    /// Queue `identity` before writing the eventfd. Duplicate identities are
    /// coalesced. Never silently drops a first wake. Invalidated identities
    /// are ignored. After `reject_new`, wakes do not enqueue.
    pub fn wake_identity(&self, identity: RequestIdentity) -> Result<(), NotifyError> {
        if !self.accepting.load(Ordering::Acquire) {
            return Ok(());
        }
        {
            let mut pending = self.pending.lock().map_err(|_| NotifyError::Poisoned)?;
            if !self.accepting.load(Ordering::Acquire) {
                return Ok(());
            }
            if pending.cancelled.contains(&identity) {
                return Ok(());
            }
            if !pending.queued.insert(identity) {
                return Ok(());
            }
            pending.queue.push_back(identity);
        }
        match self.write_eventfd() {
            Ok(()) => Ok(()),
            Err(err) => {
                self.note_wake_error(err);
                Err(err)
            }
        }
    }

    fn note_wake_error(&self, err: NotifyError) {
        if matches!(err, NotifyError::ShutDown) {
            return;
        }
        if let Ok(mut slot) = self.wake_error.lock() {
            *slot = Some(err);
        }
    }

    fn take_wake_error(&self) -> Option<NotifyError> {
        self.wake_error.lock().ok().and_then(|mut slot| slot.take())
    }

    fn write_eventfd(&self) -> Result<(), NotifyError> {
        let buf = 1u64.to_ne_bytes();
        let guard = self.fd.lock().map_err(|_| NotifyError::Poisoned)?;
        let FdInner::Open(fd) = *guard else {
            return Ok(());
        };
        loop {
            match self.io.write(fd, &buf) {
                Ok(8) => return Ok(()),
                Ok(n) => return Err(NotifyError::EventFdShortWrite(n)),
                Err(err) if err == libc::EINTR => continue,
                Err(err) if err == libc::EAGAIN || err == libc::EWOULDBLOCK => return Ok(()),
                Err(err) => return Err(NotifyError::EventFdWrite(err)),
            }
        }
    }
}

/// Eventfd-backed waker: owned sender and request identity.
pub struct NgxWake {
    sender: CompletionSender,
    identity: RequestIdentity,
}

impl Wake for NgxWake {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        if let Err(err) = self.sender.wake_identity(self.identity) {
            self.sender.note_wake_error(err);
        }
    }
}

/// Build a Send/Sync waker that queues `identity` then writes eventfd.
pub fn eventfd_waker(sender: CompletionSender, identity: RequestIdentity) -> Waker {
    Waker::from(Arc::new(NgxWake { sender, identity }))
}

enum FdState {
    Local,
    Nginx { conn: *mut ngx_connection_t },
    Closed,
}

/// Worker-main-thread notify bridge. Not sent to Tokio.
pub struct NotifyBridge {
    io: Arc<dyn EventFdIo>,
    accepting: Arc<AtomicBool>,
    pending: Arc<Mutex<Pending>>,
    ready: Mutex<VecDeque<RequestIdentity>>,
    sender: CompletionSender,
    fd: Arc<Mutex<FdInner>>,
    fd_state: FdState,
    stopped: bool,
    #[cfg(test)]
    eventfd_lifecycle: Option<crate::test_support::EventFdLifecycleToken>,
    #[cfg(test)]
    // Serialize eventfd-bearing unit tests with tests that mutate NGINX globals.
    _global_test_guard: crate::test_support::GlobalNginxTestGuard,
}

impl NotifyBridge {
    pub fn new() -> Result<Self, NotifyError> {
        Self::with_capacity(DEFAULT_MAX_IN_FLIGHT)
    }

    pub fn with_capacity(cap: usize) -> Result<Self, NotifyError> {
        Self::with_io(Arc::new(LibcEventFdIo), cap)
    }

    pub fn with_io(io: Arc<dyn EventFdIo>, cap: usize) -> Result<Self, NotifyError> {
        #[cfg(test)]
        let global_test_guard = crate::test_support::global_ngx_test_guard();
        let flags = libc::EFD_NONBLOCK | libc::EFD_CLOEXEC;
        let fd = io.eventfd(0, flags).map_err(NotifyError::EventFdCreate)?;
        #[cfg(test)]
        let eventfd_lifecycle = crate::test_support::record_eventfd_created(fd);
        let accepting = Arc::new(AtomicBool::new(true));
        let pending = Arc::new(Mutex::new(Pending {
            queue: VecDeque::new(),
            queued: HashSet::new(),
            in_flight: 0,
            cap,
            cancelled: HashSet::new(),
        }));
        let fd_inner = Arc::new(Mutex::new(FdInner::Open(fd)));
        let sender = CompletionSender {
            fd: Arc::clone(&fd_inner),
            accepting: Arc::clone(&accepting),
            io: Arc::clone(&io),
            pending: Arc::clone(&pending),
            wake_error: Arc::new(Mutex::new(None)),
        };
        Ok(Self {
            io,
            accepting,
            pending,
            ready: Mutex::new(VecDeque::new()),
            sender,
            fd: fd_inner,
            fd_state: FdState::Local,
            stopped: false,
            #[cfg(test)]
            eventfd_lifecycle,
            #[cfg(test)]
            _global_test_guard: global_test_guard,
        })
    }

    pub fn sender(&self) -> CompletionSender {
        self.sender.clone()
    }

    /// Queue an owner-thread continuation without reserving async wait capacity.
    ///
    /// Under NGINX the registered read event is posted to the next-event queue,
    /// preventing a budget-limited immediate-ready chain from re-entering the
    /// current drain turn. Local/unit runtimes consume the ready queue directly.
    pub fn post_continuation(&self, identity: RequestIdentity) -> Result<(), NotifyError> {
        if !self.accepting.load(Ordering::Acquire) {
            return Err(NotifyError::ShutDown);
        }
        let event = match self.fd_state {
            FdState::Local => None,
            FdState::Nginx { conn } if !conn.is_null() => Some(unsafe { (*conn).read }),
            FdState::Nginx { .. } | FdState::Closed => return Err(NotifyError::ShutDown),
        };
        {
            let mut pending = self.lock_pending()?;
            if pending.cancelled.contains(&identity) || !pending.queued.insert(identity) {
                return Ok(());
            }
            let mut ready = self.lock_ready()?;
            ready.push_back(identity);
        }
        if let Some(event) = event {
            if event.is_null() {
                return Err(NotifyError::ShutDown);
            }
            unsafe {
                ngx_post_event(event, core::ptr::addr_of_mut!(ngx_posted_next_events));
            }
        }
        Ok(())
    }

    pub fn raw_fd(&self) -> Option<RawFd> {
        match self.fd.lock() {
            Ok(guard) => match *guard {
                FdInner::Open(fd) => Some(fd),
                FdInner::Closed => None,
            },
            Err(_) => None,
        }
    }

    pub fn capacity(&self) -> Result<usize, NotifyError> {
        Ok(self.lock_pending()?.cap)
    }

    pub fn in_flight(&self) -> Result<usize, NotifyError> {
        Ok(self.lock_pending()?.in_flight)
    }

    /// Reserve one wait slot before the initial poll. Rejects when full or shut down.
    pub fn try_reserve(&self) -> Result<CompletionPermit, NotifyError> {
        if !self.accepting.load(Ordering::Acquire) {
            return Err(NotifyError::ShutDown);
        }
        let mut pending = self.lock_pending()?;
        if !self.accepting.load(Ordering::Acquire) {
            return Err(NotifyError::ShutDown);
        }
        if pending.in_flight >= pending.cap {
            return Err(NotifyError::QueueFull);
        }
        pending.in_flight = pending.in_flight.saturating_add(1);
        Ok(CompletionPermit {
            pending: Arc::clone(&self.pending),
            live: true,
        })
    }

    fn lock_pending(&self) -> Result<std::sync::MutexGuard<'_, Pending>, NotifyError> {
        self.pending.lock().map_err(|_| NotifyError::Poisoned)
    }

    fn lock_ready(
        &self,
    ) -> Result<std::sync::MutexGuard<'_, VecDeque<RequestIdentity>>, NotifyError> {
        self.ready.lock().map_err(|_| NotifyError::Poisoned)
    }

    /// Drain eventfd (EINTR retry, EAGAIN = already readable) then move every
    /// pending identity onto the worker-local ready queue, coalescing dups.
    pub fn harvest(&self) -> Result<(), NotifyError> {
        let read_result = self.drain_eventfd();
        let mut pending = self.lock_pending()?;
        let mut ready = self.lock_ready()?;
        for identity in pending.queue.drain(..) {
            if !ready.iter().any(|queued| *queued == identity) {
                ready.push_back(identity);
            }
        }
        drop(ready);
        drop(pending);
        read_result
    }

    fn drain_eventfd(&self) -> Result<(), NotifyError> {
        let guard = match self.fd.lock() {
            Ok(guard) => guard,
            Err(_) => return Err(NotifyError::Poisoned),
        };
        let FdInner::Open(fd) = *guard else {
            return Ok(());
        };
        let mut saw_hard = None;
        loop {
            let mut buf = [0u8; 8];
            match self.io.read(fd, &mut buf) {
                Ok(8) => continue,
                Ok(0) => {
                    saw_hard = Some(NotifyError::EventFdEof);
                    break;
                }
                Ok(n) => {
                    saw_hard = Some(NotifyError::EventFdShortRead(n));
                    break;
                }
                Err(err) if err == libc::EINTR => continue,
                Err(err) if err == libc::EAGAIN || err == libc::EWOULDBLOCK => break,
                Err(err) => {
                    saw_hard = Some(NotifyError::EventFdRead(err));
                    break;
                }
            }
        }
        match saw_hard {
            Some(err) => Err(err),
            None => Ok(()),
        }
    }

    /// Take every ready identity. Capacity is owned by [`CompletionPermit`].
    pub fn take_ready(&self) -> Result<Vec<RequestIdentity>, NotifyError> {
        let mut pending = self.lock_pending()?;
        let mut ready = self.lock_ready()?;
        let taken: Vec<_> = ready.drain(..).collect();
        for identity in &taken {
            pending.queued.remove(identity);
        }
        Ok(taken)
    }

    pub fn pop_ready(&self) -> Result<Option<RequestIdentity>, NotifyError> {
        let mut pending = self.lock_pending()?;
        let identity = self.lock_ready()?.pop_front();
        if let Some(identity) = identity {
            pending.queued.remove(&identity);
        }
        Ok(identity)
    }

    pub fn harvest_and_take_ready(&self) -> Result<Vec<RequestIdentity>, NotifyError> {
        let (ready, err) = self.harvest_take();
        match err {
            Some(err) if ready.is_empty() => Err(err),
            _ => Ok(ready),
        }
    }

    /// Move queued identities onto the ready list even when eventfd drain
    /// fails, then report the drain/wake error alongside them.
    pub(crate) fn harvest_take(&self) -> (Vec<RequestIdentity>, Option<NotifyError>) {
        let harvest_err = self.harvest().err();
        let wake_err = self.sender.take_wake_error();
        match self.take_ready() {
            Ok(ready) => (ready, harvest_err.or(wake_err)),
            Err(err) => (Vec::new(), harvest_err.or(wake_err).or(Some(err))),
        }
    }

    /// Register read readiness with NGINX. Must run on the worker main thread.
    ///
    /// Exact APIs: `ngx_get_connection`, then the same `add_conn`/`add`
    /// split as `ngx_add_channel_event`. Generated bindings export
    /// `NGX_USE_EPOLL_EVENT` but not the `NGX_READ_EVENT` C macro, so the
    /// pinned Linux epoll path uses `add` with `EPOLLIN` (readable).
    ///
    /// # Safety
    ///
    /// `cycle` must be the live worker `ngx_cycle_t` passed to `init_process`.
    /// `handler` must remain valid for the lifetime of the registered connection.
    pub unsafe fn register_nginx(
        &mut self,
        cycle: *mut ngx_cycle_t,
        handler: nginx_sys::ngx_event_handler_pt,
    ) -> Result<(), NotifyError> {
        if !matches!(self.fd_state, FdState::Local) {
            return Err(NotifyError::ShutDown);
        }
        let fd = self.raw_fd().ok_or(NotifyError::ShutDown)?;
        // SAFETY: `cycle` is the live worker cycle passed to init_process.
        let cycle_ref = unsafe { &*cycle };
        let log = cycle_ref.log;
        let conn = unsafe { ngx_get_connection(fd, log) };
        if conn.is_null() {
            return Err(NotifyError::GetConnection);
        }
        unsafe {
            (*conn).pool = cycle_ref.pool;
            let rev = (*conn).read;
            let wev = (*conn).write;
            (*rev).log = log;
            (*wev).log = log;
            (*rev).set_channel(1);
            (*wev).set_channel(1);
            (*rev).handler = handler;
            // Non-epoll `add_conn` may arm write. eventfd is writable; a NULL
            // write handler would SIGSEGV if that write event fires.
            (*wev).handler = Some(empty_write_handler);
        }
        let rc = unsafe { add_read_event(conn) };
        if rc != 0 {
            unsafe { ngx_free_connection(conn) };
            return Err(NotifyError::AddEvent);
        }
        self.fd_state = FdState::Nginx { conn };
        Ok(())
    }

    /// Stop accepting, then close the fd only after callers cannot write.
    pub fn shutdown_fd(&mut self) {
        self.accepting.store(false, Ordering::Release);
        if self.stopped {
            return;
        }
        self.stopped = true;
        let conn = match std::mem::replace(&mut self.fd_state, FdState::Closed) {
            FdState::Local => None,
            FdState::Nginx { conn } => Some(conn),
            FdState::Closed => return,
        };
        if let Ok(mut guard) = self.fd.lock() {
            if let FdInner::Open(fd) = std::mem::replace(&mut *guard, FdInner::Closed) {
                #[cfg(test)]
                let close_succeeded = match conn {
                    Some(conn) if !conn.is_null() => {
                        unsafe { ngx_close_connection(conn) };
                        true
                    }
                    Some(_) => false,
                    None => self.io.close(fd).is_ok(),
                };
                #[cfg(not(test))]
                match conn {
                    Some(conn) if !conn.is_null() => unsafe { ngx_close_connection(conn) },
                    Some(_) => {}
                    None => {
                        let _ = self.io.close(fd);
                    }
                }
                #[cfg(test)]
                if let Some(lifecycle) = self.eventfd_lifecycle.as_ref() {
                    lifecycle.record_close(fd, close_succeeded);
                }
            }
        }
    }

    pub fn reject_new(&self) {
        self.accepting.store(false, Ordering::Release);
    }

    pub fn is_accepting(&self) -> bool {
        self.accepting.load(Ordering::Acquire)
    }

    /// Invalidate `identity` so later wakes are ignored until
    /// [`Self::forget_invalidated`].
    pub fn invalidate(&self, identity: RequestIdentity) -> Result<bool, NotifyError> {
        let mut pending = self.lock_pending()?;
        pending.cancelled.insert(identity);
        let mut found = false;
        pending.queue.retain(|queued| {
            if *queued == identity {
                found = true;
                false
            } else {
                true
            }
        });
        let mut ready = self.lock_ready()?;
        ready.retain(|queued| {
            if *queued == identity {
                found = true;
                false
            } else {
                true
            }
        });
        pending.queued.remove(&identity);
        Ok(found)
    }

    /// Drop the invalidate tombstone after the VM/future for `identity` has
    /// been dropped. Late stale wakes are ignored by generation lookup.
    pub fn forget_invalidated(&self, identity: RequestIdentity) -> Result<(), NotifyError> {
        self.lock_pending()?.cancelled.remove(&identity);
        Ok(())
    }

    /// Drop queued identities. Worker exit uses this after invalidating
    /// senders so leftover wakes never resume a request.
    pub fn reclaim_all(&self) -> Result<(), NotifyError> {
        let mut pending = self.lock_pending()?;
        let mut ready = self.lock_ready()?;
        pending.queue.clear();
        pending.queued.clear();
        ready.clear();
        pending.in_flight = 0;
        pending.cancelled.clear();
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn pending_arc(&self) -> Arc<Mutex<Pending>> {
        Arc::clone(&self.pending)
    }

    #[cfg(test)]
    pub(crate) fn cancelled_len(&self) -> Result<usize, NotifyError> {
        Ok(self.lock_pending()?.cancelled.len())
    }

    #[cfg(test)]
    pub(crate) fn fcntl(&self, cmd: i32) -> Result<i32, NotifyError> {
        let guard = self.fd.lock().map_err(|_| NotifyError::Poisoned)?;
        let FdInner::Open(fd) = *guard else {
            return Err(NotifyError::ShutDown);
        };
        self.io.fcntl(fd, cmd).map_err(NotifyError::EventFdRead)
    }
}

impl Drop for NotifyBridge {
    fn drop(&mut self) {
        self.shutdown_fd();
    }
}

unsafe extern "C" fn empty_write_handler(_ev: *mut nginx_sys::ngx_event_t) {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AddReadAction {
    AddConn,
    AddEpollIn,
}

/// Same split as NGINX `ngx_add_channel_event`: `add_conn` when it is not
/// epoll (epoll `add_conn` is edge-triggered and also arms `EPOLLOUT`).
fn add_read_action(has_add_conn: bool, use_epoll: bool) -> AddReadAction {
    if has_add_conn && !use_epoll {
        AddReadAction::AddConn
    } else {
        AddReadAction::AddEpollIn
    }
}

/// Bindgen does not export `NGX_READ_EVENT`; the pinned epoll path uses
/// `add` with `EPOLLIN`.
unsafe fn add_read_event(conn: *mut ngx_connection_t) -> ngx_int_t {
    let (add_conn, add, flags) = unsafe {
        let actions = &*core::ptr::addr_of!(ngx_event_actions);
        (actions.add_conn, actions.add, ngx_event_flags)
    };
    let use_epoll = (flags & NGX_USE_EPOLL_EVENT as ngx_uint_t) != 0;
    match add_read_action(add_conn.is_some(), use_epoll) {
        AddReadAction::AddConn => unsafe { add_conn.expect("add_conn present")(conn) },
        AddReadAction::AddEpollIn => match add {
            Some(add) => {
                let rev = unsafe { (*conn).read };
                unsafe { add(rev, libc::EPOLLIN as ngx_int_t, 0) }
            }
            None => -1,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicI32, AtomicUsize};
    use std::thread;
    use std::time::Duration;

    fn identity(id: u64) -> RequestIdentity {
        RequestIdentity::new(1, id)
    }

    enum IoScript {
        Real,
        Err(i32),
        Bytes(usize),
    }

    struct SpyIo {
        inner: LibcEventFdIo,
        writes: AtomicUsize,
        write_after_close: AtomicBool,
        write_script: Mutex<Vec<IoScript>>,
        read_script: Mutex<Vec<IoScript>>,
        closed: AtomicBool,
        eventfd_flags: AtomicI32,
    }

    impl SpyIo {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                inner: LibcEventFdIo,
                writes: AtomicUsize::new(0),
                write_after_close: AtomicBool::new(false),
                write_script: Mutex::new(Vec::new()),
                read_script: Mutex::new(Vec::new()),
                closed: AtomicBool::new(false),
                eventfd_flags: AtomicI32::new(0),
            })
        }
    }

    impl EventFdIo for SpyIo {
        fn eventfd(&self, initval: u32, flags: i32) -> Result<RawFd, i32> {
            self.eventfd_flags.store(flags, Ordering::SeqCst);
            self.inner.eventfd(initval, flags)
        }

        fn read(&self, fd: RawFd, buf: &mut [u8]) -> Result<usize, i32> {
            let script = self.read_script.lock().unwrap().pop();
            match script {
                Some(IoScript::Err(err)) => Err(err),
                Some(IoScript::Bytes(n)) => Ok(n),
                _ => self.inner.read(fd, buf),
            }
        }

        fn write(&self, fd: RawFd, buf: &[u8]) -> Result<usize, i32> {
            if self.closed.load(Ordering::SeqCst) {
                self.write_after_close.store(true, Ordering::SeqCst);
            }
            self.writes.fetch_add(1, Ordering::SeqCst);
            let script = self.write_script.lock().unwrap().pop();
            match script {
                Some(IoScript::Err(err)) => Err(err),
                Some(IoScript::Bytes(n)) => Ok(n),
                _ => self.inner.write(fd, buf),
            }
        }

        fn close(&self, fd: RawFd) -> Result<(), i32> {
            self.closed.store(true, Ordering::SeqCst);
            self.inner.close(fd)
        }

        fn fcntl(&self, fd: RawFd, cmd: i32) -> Result<i32, i32> {
            self.inner.fcntl(fd, cmd)
        }
    }

    fn bridge_with_spy(cap: usize) -> (NotifyBridge, Arc<SpyIo>) {
        let spy = SpyIo::new();
        let bridge = NotifyBridge::with_io(Arc::clone(&spy) as Arc<dyn EventFdIo>, cap).unwrap();
        (bridge, spy)
    }

    #[test]
    fn eventfd_is_nonblock_and_cloexec() {
        let (bridge, spy) = bridge_with_spy(1);
        assert_eq!(
            spy.eventfd_flags.load(Ordering::SeqCst),
            libc::EFD_NONBLOCK | libc::EFD_CLOEXEC
        );
        let fl = bridge.fcntl(libc::F_GETFL).expect("F_GETFL");
        assert_ne!(fl & libc::O_NONBLOCK, 0, "EFD_NONBLOCK must set O_NONBLOCK");
        let fdfl = bridge.fcntl(libc::F_GETFD).expect("F_GETFD");
        assert_ne!(
            fdfl & libc::FD_CLOEXEC,
            0,
            "EFD_CLOEXEC must set FD_CLOEXEC"
        );
    }

    #[test]
    fn add_read_event_matches_nginx_channel_split() {
        assert_eq!(
            add_read_action(true, true),
            AddReadAction::AddEpollIn,
            "epoll must use add(rev, EPOLLIN), not add_conn"
        );
        assert_eq!(
            add_read_action(true, false),
            AddReadAction::AddConn,
            "non-epoll must use add_conn"
        );
        assert_eq!(add_read_action(false, true), AddReadAction::AddEpollIn);
        assert_eq!(add_read_action(false, false), AddReadAction::AddEpollIn);
    }

    #[test]
    fn dropped_permit_releases_capacity() {
        let bridge = NotifyBridge::with_capacity(1).unwrap();
        {
            let _permit = bridge.try_reserve().unwrap();
            assert_eq!(
                bridge.try_reserve().map(|_| ()),
                Err(NotifyError::QueueFull)
            );
            assert_eq!(bridge.in_flight().unwrap(), 1);
        }
        assert_eq!(bridge.in_flight().unwrap(), 0);
        let _permit = bridge.try_reserve().unwrap();
    }

    #[test]
    fn coalesced_duplicate_wake_enqueues_once() {
        let bridge = NotifyBridge::new().unwrap();
        let waker = eventfd_waker(bridge.sender(), identity(3));
        waker.wake_by_ref();
        waker.wake_by_ref();
        waker.wake_by_ref();
        assert_eq!(bridge.harvest_and_take_ready().unwrap(), vec![identity(3)]);
    }

    #[test]
    fn cached_waker_repeated_wake_after_harvest_enqueues_again() {
        let bridge = NotifyBridge::new().unwrap();
        let waker = eventfd_waker(bridge.sender(), identity(3));
        waker.wake_by_ref();
        assert_eq!(bridge.harvest_and_take_ready().unwrap(), vec![identity(3)]);
        waker.wake_by_ref();
        waker.wake_by_ref();
        assert_eq!(
            bridge.harvest_and_take_ready().unwrap(),
            vec![identity(3)],
            "a cached waker must be able to wake again after harvest"
        );
    }

    #[test]
    fn wake_between_harvest_and_owner_take_remains_coalesced() {
        let bridge = NotifyBridge::new().unwrap();
        let sender = bridge.sender();
        sender.wake_identity(identity(3)).unwrap();
        bridge.harvest().unwrap();

        sender.wake_identity(identity(3)).unwrap();

        assert_eq!(bridge.take_ready().unwrap(), vec![identity(3)]);
        assert!(bridge.harvest_and_take_ready().unwrap().is_empty());
    }

    #[test]
    fn same_identity_can_wake_after_owner_consumes_harvested_ready() {
        let bridge = NotifyBridge::new().unwrap();
        let sender = bridge.sender();
        sender.wake_identity(identity(3)).unwrap();
        bridge.harvest().unwrap();
        assert_eq!(bridge.take_ready().unwrap(), vec![identity(3)]);

        sender.wake_identity(identity(3)).unwrap();

        assert_eq!(bridge.harvest_and_take_ready().unwrap(), vec![identity(3)]);
    }

    #[test]
    fn many_coalesced_identities_drain_together() {
        let bridge = NotifyBridge::new().unwrap();
        for id in 1..=4 {
            bridge.sender().wake_identity(identity(id)).unwrap();
        }
        assert_eq!(
            bridge.harvest_and_take_ready().unwrap(),
            vec![identity(1), identity(2), identity(3), identity(4)],
            "one harvest must take every queued identity after coalesced writes"
        );
    }

    #[test]
    fn queue_identity_before_eventfd_write() {
        let (bridge, spy) = bridge_with_spy(4);
        spy.write_script
            .lock()
            .unwrap()
            .push(IoScript::Err(libc::EIO));
        let err = bridge.sender().wake_identity(identity(9)).unwrap_err();
        assert_eq!(err, NotifyError::EventFdWrite(libc::EIO));
        spy.read_script
            .lock()
            .unwrap()
            .push(IoScript::Err(libc::EAGAIN));
        assert_eq!(
            bridge.harvest_and_take_ready().unwrap(),
            vec![identity(9)],
            "failed wake must not drop the already-enqueued identity"
        );
    }

    #[test]
    fn eintr_on_write_is_retried() {
        let (bridge, spy) = bridge_with_spy(4);
        spy.write_script.lock().unwrap().push(IoScript::Real);
        spy.write_script
            .lock()
            .unwrap()
            .push(IoScript::Err(libc::EINTR));
        bridge.sender().wake_identity(identity(3)).unwrap();
        assert_eq!(bridge.harvest_and_take_ready().unwrap(), vec![identity(3)]);
    }

    #[test]
    fn eintr_on_read_is_retried() {
        let (bridge, spy) = bridge_with_spy(4);
        bridge.sender().wake_identity(identity(4)).unwrap();
        spy.read_script.lock().unwrap().push(IoScript::Real);
        spy.read_script
            .lock()
            .unwrap()
            .push(IoScript::Err(libc::EINTR));
        assert_eq!(bridge.harvest_and_take_ready().unwrap(), vec![identity(4)]);
    }

    #[test]
    fn eagain_on_read_is_already_readable_and_still_drains_queue() {
        let (bridge, spy) = bridge_with_spy(4);
        bridge.sender().wake_identity(identity(5)).unwrap();
        spy.read_script
            .lock()
            .unwrap()
            .push(IoScript::Err(libc::EAGAIN));
        assert_eq!(
            bridge.harvest_and_take_ready().unwrap(),
            vec![identity(5)],
            "EAGAIN means the counter is already drained; queued identities must still appear"
        );
    }

    #[test]
    fn eagain_on_write_is_already_readable() {
        let (bridge, spy) = bridge_with_spy(4);
        spy.write_script
            .lock()
            .unwrap()
            .push(IoScript::Err(libc::EAGAIN));
        bridge.sender().wake_identity(identity(6)).unwrap();
        assert_eq!(bridge.harvest_and_take_ready().unwrap(), vec![identity(6)]);
    }

    #[test]
    fn short_read_is_a_hard_error_and_does_not_drop_the_identity() {
        let (bridge, spy) = bridge_with_spy(4);
        bridge.sender().wake_identity(identity(8)).unwrap();
        spy.read_script.lock().unwrap().push(IoScript::Bytes(3));
        let err = bridge.harvest().expect_err("short read");
        assert_eq!(err, NotifyError::EventFdShortRead(3));
        assert_eq!(bridge.take_ready().unwrap(), vec![identity(8)]);
    }

    #[test]
    fn short_read_harvest_take_returns_identities_then_error() {
        let (bridge, spy) = bridge_with_spy(4);
        bridge.sender().wake_identity(identity(8)).unwrap();
        spy.read_script.lock().unwrap().push(IoScript::Bytes(3));
        let (ready, err) = bridge.harvest_take();
        assert_eq!(ready, vec![identity(8)]);
        assert_eq!(err, Some(NotifyError::EventFdShortRead(3)));
    }

    #[test]
    fn eof_read_is_a_hard_error() {
        let (bridge, spy) = bridge_with_spy(4);
        spy.read_script.lock().unwrap().push(IoScript::Bytes(0));
        assert_eq!(bridge.harvest(), Err(NotifyError::EventFdEof));
    }

    #[test]
    fn short_write_is_a_hard_error() {
        let (bridge, spy) = bridge_with_spy(4);
        spy.write_script.lock().unwrap().push(IoScript::Bytes(2));
        assert_eq!(
            bridge.sender().wake_identity(identity(11)),
            Err(NotifyError::EventFdShortWrite(2))
        );
    }

    #[test]
    fn submit_after_shutdown_is_rejected() {
        let mut bridge = NotifyBridge::new().unwrap();
        bridge.reject_new();
        assert_eq!(bridge.try_reserve().map(|_| ()), Err(NotifyError::ShutDown));
        bridge.shutdown_fd();
        assert_eq!(bridge.try_reserve().map(|_| ()), Err(NotifyError::ShutDown));
        bridge.shutdown_fd();
    }

    #[test]
    fn shutdown_skips_wake_write_and_does_not_enqueue() {
        let (mut bridge, spy) = bridge_with_spy(4);
        bridge.reject_new();
        let writes_before = spy.writes.load(Ordering::SeqCst);
        bridge.sender().wake_identity(identity(12)).unwrap();
        assert_eq!(
            spy.writes.load(Ordering::SeqCst),
            writes_before,
            "shut down senders must not write a fd that is about to close"
        );
        spy.read_script
            .lock()
            .unwrap()
            .push(IoScript::Err(libc::EAGAIN));
        assert!(
            bridge.harvest_and_take_ready().unwrap().is_empty(),
            "wake after reject_new must not enqueue"
        );
        bridge.shutdown_fd();
    }

    #[test]
    fn wake_after_reclaim_does_not_enqueue() {
        let bridge = NotifyBridge::new().unwrap();
        bridge.sender().wake_identity(identity(1)).unwrap();
        bridge.reject_new();
        bridge.reclaim_all().unwrap();
        bridge.sender().wake_identity(identity(2)).unwrap();
        assert!(
            bridge.harvest_and_take_ready().unwrap().is_empty(),
            "shutdown reclaim must not be followed by a new enqueue"
        );
    }

    #[test]
    fn complete_after_close_does_not_write_closed_fd() {
        let (mut bridge, spy) = bridge_with_spy(4);
        bridge.shutdown_fd();
        let _ = bridge.sender().wake_identity(identity(1));
        assert!(
            !spy.write_after_close.load(Ordering::SeqCst),
            "cloned sender must not write after close"
        );
        assert_eq!(spy.writes.load(Ordering::SeqCst), 0);
        assert!(bridge.raw_fd().is_none());
    }

    #[test]
    fn shutdown_write_race_never_writes_after_close() {
        let (mut bridge, spy) = bridge_with_spy(8);
        let sender = bridge.sender();
        let mut joins = Vec::new();
        for id in 1..=8 {
            let sender = sender.clone();
            joins.push(thread::spawn(move || sender.wake_identity(identity(id))));
        }
        thread::sleep(Duration::from_millis(1));
        bridge.shutdown_fd();
        for join in joins {
            let _ = join.join();
        }
        assert!(
            !spy.write_after_close.load(Ordering::SeqCst),
            "write and close are exclusive; a cloned sender must not write a closed fd"
        );
    }

    #[test]
    fn invalidate_drops_stale_identity() {
        let bridge = NotifyBridge::new().unwrap();
        bridge.sender().wake_identity(identity(1)).unwrap();
        assert!(bridge.invalidate(identity(1)).unwrap());
        assert!(bridge.harvest_and_take_ready().unwrap().is_empty());
        bridge.sender().wake_identity(identity(1)).unwrap();
        assert!(bridge.harvest_and_take_ready().unwrap().is_empty());
        bridge.forget_invalidated(identity(1)).unwrap();
        assert_eq!(bridge.cancelled_len().unwrap(), 0);
    }

    #[test]
    fn repeated_invalidate_cleanup_does_not_grow_tombstones() {
        let bridge = NotifyBridge::new().unwrap();
        for id in 0..64 {
            let identity = identity(id);
            bridge.invalidate(identity).unwrap();
            assert_eq!(bridge.cancelled_len().unwrap(), 1);
            bridge.forget_invalidated(identity).unwrap();
            assert_eq!(bridge.cancelled_len().unwrap(), 0);
        }
        assert_eq!(bridge.cancelled_len().unwrap(), 0);
    }

    #[test]
    fn poisoned_queue_does_not_panic() {
        let bridge = NotifyBridge::new().unwrap();
        let pending = bridge.pending_arc();
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let _ = std::panic::catch_unwind(AssertUnwindSafe(|| {
            let _guard = pending.lock().expect("lock before poison");
            panic!("test poison");
        }));
        std::panic::set_hook(prev);
        assert_eq!(bridge.try_reserve().map(|_| ()), Err(NotifyError::Poisoned));
        assert_eq!(
            bridge.sender().wake_identity(identity(1)),
            Err(NotifyError::Poisoned)
        );
        assert_eq!(bridge.harvest(), Err(NotifyError::Poisoned));
        assert_eq!(bridge.take_ready(), Err(NotifyError::Poisoned));
    }

    #[test]
    fn waker_is_send_sync_and_owns_no_nginx_pointers() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<CompletionSender>();
        assert_send_sync::<RequestIdentity>();
        assert_send_sync::<NgxWake>();
        for name in [
            std::any::type_name::<CompletionSender>(),
            std::any::type_name::<NgxWake>(),
            std::any::type_name::<RequestIdentity>(),
        ] {
            assert!(
                !name.contains("ngx_connection")
                    && !name.contains("ngx_http")
                    && !name.contains("ngx_pool")
                    && !name.contains("*mut")
                    && !name.contains("ngx_cycle"),
                "waker/identity must not mention NGINX/VM data: {name}"
            );
        }
    }

    use std::panic::AssertUnwindSafe;
}
