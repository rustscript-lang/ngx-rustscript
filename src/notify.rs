//! Linux eventfd completion bridge and bounded worker-local ready queue.
//!
//! Tokio tasks hold only [`CompletionSender`] and a [`CompletionPermit`]: they
//! enqueue an owned [`SleepToken`] under a mutex, then write the eventfd.
//! They never call NGINX APIs. The worker main thread registers the fd with
//! NGINX (`ngx_get_connection`, then the same add_conn/add split as
//! `ngx_add_channel_event`) and harvests completions into a ready queue for
//! Task 5.

use std::collections::VecDeque;
use std::fmt;
use std::os::unix::io::RawFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use nginx_sys::{
    NGX_USE_EPOLL_EVENT, ngx_close_connection, ngx_connection_t, ngx_cycle_t, ngx_event_actions,
    ngx_event_flags, ngx_free_connection, ngx_get_connection, ngx_int_t, ngx_uint_t,
};

use crate::sleep_binding::SleepToken;

/// Maximum in-flight sleep operations per worker, including pending and ready.
pub const DEFAULT_MAX_IN_FLIGHT: usize = 1024;

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
            Self::QueueFull => write!(f, "sleep completion queue is full"),
            Self::ShutDown => write!(f, "worker runtime is shut down"),
            Self::Poisoned => write!(f, "sleep completion queue mutex is poisoned"),
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
    pub(crate) queue: VecDeque<SleepToken>,
    in_flight: usize,
    cap: usize,
}

#[derive(Clone, Copy)]
enum FdInner {
    Open(RawFd),
    Closed,
}

/// RAII in-flight slot: task -> pending queue -> ready queue.
///
/// Drop releases the slot unless [`CompletionPermit::disarm`] transferred
/// ownership into the completion queues.
pub struct CompletionPermit {
    pending: Arc<Mutex<Pending>>,
    live: bool,
}

impl CompletionPermit {
    fn disarm(&mut self) {
        self.live = false;
    }
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

/// Send-only handle owned by Tokio tasks: SleepToken + duration live on the
/// task; this handle is the completion sender / bridge Arc.
#[derive(Clone)]
pub struct CompletionSender {
    fd: Arc<Mutex<FdInner>>,
    accepting: Arc<AtomicBool>,
    io: Arc<dyn EventFdIo>,
    pending: Arc<Mutex<Pending>>,
}

impl CompletionSender {
    /// Enqueue `token` under the mutex, then write the eventfd.
    ///
    /// The mutex release happens-before the wake write. Completions are never
    /// dropped on a full queue: capacity was reserved at submit. `permit` is
    /// disarmed so the slot stays occupied until the worker takes the ready
    /// token.
    pub fn complete(
        &self,
        token: SleepToken,
        mut permit: CompletionPermit,
    ) -> Result<(), NotifyError> {
        {
            let mut pending = self.pending.lock().map_err(|_| NotifyError::Poisoned)?;
            pending.queue.push_back(token);
            permit.disarm();
        }
        if !self.accepting.load(Ordering::Acquire) {
            return Ok(());
        }
        self.wake()
    }

    fn wake(&self) -> Result<(), NotifyError> {
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
    ready: Mutex<VecDeque<SleepToken>>,
    sender: CompletionSender,
    fd: Arc<Mutex<FdInner>>,
    fd_state: FdState,
    stopped: bool,
}

impl NotifyBridge {
    pub fn new() -> Result<Self, NotifyError> {
        Self::with_capacity(DEFAULT_MAX_IN_FLIGHT)
    }

    pub fn with_capacity(cap: usize) -> Result<Self, NotifyError> {
        Self::with_io(Arc::new(LibcEventFdIo), cap)
    }

    pub fn with_io(io: Arc<dyn EventFdIo>, cap: usize) -> Result<Self, NotifyError> {
        let flags = libc::EFD_NONBLOCK | libc::EFD_CLOEXEC;
        let fd = io.eventfd(0, flags).map_err(NotifyError::EventFdCreate)?;
        let accepting = Arc::new(AtomicBool::new(true));
        let pending = Arc::new(Mutex::new(Pending {
            queue: VecDeque::new(),
            in_flight: 0,
            cap,
        }));
        let fd_inner = Arc::new(Mutex::new(FdInner::Open(fd)));
        let sender = CompletionSender {
            fd: Arc::clone(&fd_inner),
            accepting: Arc::clone(&accepting),
            io: Arc::clone(&io),
            pending: Arc::clone(&pending),
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
        })
    }

    pub fn sender(&self) -> CompletionSender {
        self.sender.clone()
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

    /// Reserve one completion slot before spawning. Rejects when full or shut down.
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

    fn lock_ready(&self) -> Result<std::sync::MutexGuard<'_, VecDeque<SleepToken>>, NotifyError> {
        self.ready.lock().map_err(|_| NotifyError::Poisoned)
    }

    /// Drain eventfd (EINTR retry, EAGAIN = already readable) then move every
    /// pending completion onto the worker-local ready queue.
    pub fn harvest(&self) -> Result<(), NotifyError> {
        let read_result = self.drain_eventfd();
        let mut pending = self.lock_pending()?;
        let mut ready = self.lock_ready()?;
        ready.extend(pending.queue.drain(..));
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

    /// Take every ready token and free their reserved in-flight slots.
    pub fn take_ready(&self) -> Result<Vec<SleepToken>, NotifyError> {
        let mut ready = self.lock_ready()?;
        let out: Vec<SleepToken> = ready.drain(..).collect();
        drop(ready);
        if !out.is_empty() {
            let mut pending = self.lock_pending()?;
            pending.in_flight = pending.in_flight.saturating_sub(out.len());
        }
        Ok(out)
    }

    pub fn pop_ready(&self) -> Result<Option<SleepToken>, NotifyError> {
        let mut ready = self.lock_ready()?;
        let token = ready.pop_front();
        drop(ready);
        if token.is_some() {
            let mut pending = self.lock_pending()?;
            pending.in_flight = pending.in_flight.saturating_sub(1);
        }
        Ok(token)
    }

    pub fn harvest_and_take_ready(&self) -> Result<Vec<SleepToken>, NotifyError> {
        self.harvest()?;
        self.take_ready()
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
    ///
    /// Write and close share one mutex, so a cloned sender cannot write after
    /// the fd is invalidated. NGINX path: `ngx_close_connection` unregisters
    /// and closes while the mutex is held. Local path: `close(2)`. Idempotent.
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
                if let Some(conn) = conn {
                    if !conn.is_null() {
                        unsafe { ngx_close_connection(conn) };
                    }
                } else {
                    let _ = self.io.close(fd);
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

    #[cfg(test)]
    pub(crate) fn pending_arc(&self) -> Arc<Mutex<Pending>> {
        Arc::clone(&self.pending)
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

/// Same split as NGINX `ngx_add_channel_event`: `add_conn` when it is not
/// epoll (epoll `add_conn` is edge-triggered and also arms `EPOLLOUT`).
/// Bindgen does not export `NGX_READ_EVENT`; the pinned epoll path uses
/// `add` with `EPOLLIN`.
unsafe fn add_read_event(conn: *mut ngx_connection_t) -> ngx_int_t {
    let (add_conn, add, flags) = unsafe {
        let actions = &*core::ptr::addr_of!(ngx_event_actions);
        (actions.add_conn, actions.add, ngx_event_flags)
    };
    if let Some(add_conn) = add_conn {
        if (flags & NGX_USE_EPOLL_EVENT as ngx_uint_t) == 0 {
            return unsafe { add_conn(conn) };
        }
    }
    match add {
        Some(add) => {
            let rev = unsafe { (*conn).read };
            unsafe { add(rev, libc::EPOLLIN as ngx_int_t, 0) }
        }
        None => -1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;
    use std::sync::atomic::AtomicUsize;
    use std::thread;
    use std::time::Duration;

    fn token(id: u64) -> SleepToken {
        SleepToken::from_raw(1, id)
    }

    struct SpyIo {
        inner: LibcEventFdIo,
        pending: StdMutex<Option<Arc<Mutex<Pending>>>>,
        len_at_write: AtomicUsize,
        writes: AtomicUsize,
        reads: AtomicUsize,
        closed: AtomicBool,
        write_after_close: AtomicBool,
        read_script: StdMutex<Vec<IoScript>>,
        write_script: StdMutex<Vec<IoScript>>,
    }

    #[derive(Clone, Copy)]
    enum IoScript {
        Err(i32),
        Bytes(usize),
        Real,
    }

    impl SpyIo {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                inner: LibcEventFdIo,
                pending: StdMutex::new(None),
                len_at_write: AtomicUsize::new(0),
                writes: AtomicUsize::new(0),
                reads: AtomicUsize::new(0),
                closed: AtomicBool::new(false),
                write_after_close: AtomicBool::new(false),
                read_script: StdMutex::new(Vec::new()),
                write_script: StdMutex::new(Vec::new()),
            })
        }

        fn bind_pending(&self, pending: Arc<Mutex<Pending>>) {
            *self.pending.lock().expect("spy pending") = Some(pending);
        }
    }

    impl EventFdIo for SpyIo {
        fn eventfd(&self, initval: u32, flags: i32) -> Result<RawFd, i32> {
            self.inner.eventfd(initval, flags)
        }

        fn read(&self, fd: RawFd, buf: &mut [u8]) -> Result<usize, i32> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            let script = self.read_script.lock().expect("read script").pop();
            match script {
                Some(IoScript::Err(err)) => Err(err),
                Some(IoScript::Bytes(n)) => Ok(n.min(buf.len())),
                Some(IoScript::Real) | None => self.inner.read(fd, buf),
            }
        }

        fn write(&self, fd: RawFd, buf: &[u8]) -> Result<usize, i32> {
            self.writes.fetch_add(1, Ordering::SeqCst);
            if self.closed.load(Ordering::SeqCst) {
                self.write_after_close.store(true, Ordering::SeqCst);
            }
            if let Some(pending) = self.pending.lock().expect("spy pending").as_ref() {
                let len = pending.lock().map(|p| p.queue.len()).unwrap_or(0);
                self.len_at_write.store(len, Ordering::SeqCst);
            }
            let script = self.write_script.lock().expect("write script").pop();
            match script {
                Some(IoScript::Err(err)) => Err(err),
                Some(IoScript::Bytes(n)) => Ok(n.min(buf.len())),
                Some(IoScript::Real) | None => self.inner.write(fd, buf),
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
        let bridge =
            NotifyBridge::with_io(Arc::clone(&spy) as Arc<dyn EventFdIo>, cap).expect("eventfd");
        spy.bind_pending(bridge.pending_arc());
        (bridge, spy)
    }

    #[test]
    fn eventfd_is_nonblock_and_cloexec() {
        let bridge = NotifyBridge::new().expect("eventfd");
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
    fn enqueue_happens_before_wake_write() {
        let (bridge, spy) = bridge_with_spy(8);
        let permit = bridge.try_reserve().unwrap();
        bridge.sender().complete(token(1), permit).unwrap();
        assert_eq!(
            spy.len_at_write.load(Ordering::SeqCst),
            1,
            "queue must contain the completion before the wake write"
        );
        assert!(spy.writes.load(Ordering::SeqCst) >= 1);
    }

    #[test]
    fn one_completion_is_harvested_with_payload() {
        let bridge = NotifyBridge::new().unwrap();
        let permit = bridge.try_reserve().unwrap();
        bridge.sender().complete(token(7), permit).unwrap();
        let ready = bridge.harvest_and_take_ready().unwrap();
        assert_eq!(ready, vec![token(7)]);
        assert_eq!(bridge.in_flight().unwrap(), 0);
    }

    #[test]
    fn many_coalesced_completions_drain_together() {
        let (bridge, spy) = bridge_with_spy(8);
        for id in 1..=4 {
            let permit = bridge.try_reserve().unwrap();
            bridge.sender().complete(token(id), permit).unwrap();
        }
        spy.read_script.lock().unwrap().clear();
        let ready = bridge.harvest_and_take_ready().unwrap();
        assert_eq!(
            ready,
            vec![token(1), token(2), token(3), token(4)],
            "one harvest must take every queued completion after coalesced writes"
        );
        assert_eq!(bridge.in_flight().unwrap(), 0);
    }

    #[test]
    fn multiple_completions_per_read_do_not_require_one_read_each() {
        let (bridge, spy) = bridge_with_spy(8);
        for id in 10..=12 {
            let permit = bridge.try_reserve().unwrap();
            bridge.sender().complete(token(id), permit).unwrap();
        }
        let before = spy.reads.load(Ordering::SeqCst);
        let ready = bridge.harvest_and_take_ready().unwrap();
        let reads = spy.reads.load(Ordering::SeqCst) - before;
        assert_eq!(ready.len(), 3);
        assert!(
            reads <= 2,
            "eventfd counter coalesces; harvest should not need one successful read per completion, got {reads}"
        );
    }

    #[test]
    fn capacity_rejects_before_spawn_and_recovers_after_drain() {
        let bridge = NotifyBridge::with_capacity(1).unwrap();
        let permit = bridge.try_reserve().unwrap();
        assert_eq!(
            bridge.try_reserve().map(|_| ()),
            Err(NotifyError::QueueFull)
        );
        bridge.sender().complete(token(1), permit).unwrap();
        let ready = bridge.harvest_and_take_ready().unwrap();
        assert_eq!(ready, vec![token(1)]);
        let permit = bridge.try_reserve().unwrap();
        bridge.sender().complete(token(2), permit).unwrap();
        assert_eq!(bridge.harvest_and_take_ready().unwrap(), vec![token(2)]);
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
    fn eintr_on_write_is_retried() {
        let (bridge, spy) = bridge_with_spy(4);
        spy.write_script.lock().unwrap().push(IoScript::Real);
        spy.write_script
            .lock()
            .unwrap()
            .push(IoScript::Err(libc::EINTR));
        let permit = bridge.try_reserve().unwrap();
        bridge.sender().complete(token(3), permit).unwrap();
        assert_eq!(bridge.harvest_and_take_ready().unwrap(), vec![token(3)]);
    }

    #[test]
    fn eintr_on_read_is_retried() {
        let (bridge, spy) = bridge_with_spy(4);
        let permit = bridge.try_reserve().unwrap();
        bridge.sender().complete(token(4), permit).unwrap();
        spy.read_script.lock().unwrap().push(IoScript::Real);
        spy.read_script
            .lock()
            .unwrap()
            .push(IoScript::Err(libc::EINTR));
        assert_eq!(bridge.harvest_and_take_ready().unwrap(), vec![token(4)]);
    }

    #[test]
    fn eagain_on_read_is_already_readable_and_still_drains_queue() {
        let (bridge, spy) = bridge_with_spy(4);
        let permit = bridge.try_reserve().unwrap();
        bridge.sender().complete(token(5), permit).unwrap();
        spy.read_script
            .lock()
            .unwrap()
            .push(IoScript::Err(libc::EAGAIN));
        assert_eq!(
            bridge.harvest_and_take_ready().unwrap(),
            vec![token(5)],
            "EAGAIN means the counter is already drained; queued completions must still appear"
        );
    }

    #[test]
    fn eagain_on_write_is_already_readable() {
        let (bridge, spy) = bridge_with_spy(4);
        spy.write_script
            .lock()
            .unwrap()
            .push(IoScript::Err(libc::EAGAIN));
        let permit = bridge.try_reserve().unwrap();
        bridge.sender().complete(token(6), permit).unwrap();
        assert_eq!(bridge.harvest_and_take_ready().unwrap(), vec![token(6)]);
    }

    #[test]
    fn short_read_is_a_hard_error_and_does_not_drop_the_completion() {
        let (bridge, spy) = bridge_with_spy(4);
        let permit = bridge.try_reserve().unwrap();
        bridge.sender().complete(token(8), permit).unwrap();
        spy.read_script.lock().unwrap().push(IoScript::Bytes(3));
        let err = bridge.harvest().expect_err("short read");
        assert_eq!(err, NotifyError::EventFdShortRead(3));
        assert_eq!(bridge.take_ready().unwrap(), vec![token(8)]);
    }

    #[test]
    fn eof_read_is_a_hard_error() {
        let (bridge, spy) = bridge_with_spy(4);
        spy.read_script.lock().unwrap().push(IoScript::Bytes(0));
        assert_eq!(bridge.harvest(), Err(NotifyError::EventFdEof));
    }

    #[test]
    fn hard_write_is_surfaced_and_completion_stays_queued() {
        let (bridge, spy) = bridge_with_spy(4);
        spy.write_script
            .lock()
            .unwrap()
            .push(IoScript::Err(libc::EIO));
        let permit = bridge.try_reserve().unwrap();
        assert_eq!(
            bridge.sender().complete(token(9), permit),
            Err(NotifyError::EventFdWrite(libc::EIO))
        );
        spy.read_script
            .lock()
            .unwrap()
            .push(IoScript::Err(libc::EAGAIN));
        assert_eq!(
            bridge.harvest_and_take_ready().unwrap(),
            vec![token(9)],
            "failed wake must not drop the already-enqueued completion"
        );
    }

    #[test]
    fn short_write_is_a_hard_error() {
        let (bridge, spy) = bridge_with_spy(4);
        spy.write_script.lock().unwrap().push(IoScript::Bytes(2));
        let permit = bridge.try_reserve().unwrap();
        assert_eq!(
            bridge.sender().complete(token(11), permit),
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
    fn shutdown_skips_wake_write_but_keeps_enqueued_completion() {
        let (mut bridge, spy) = bridge_with_spy(4);
        let permit = bridge.try_reserve().unwrap();
        bridge.reject_new();
        let writes_before = spy.writes.load(Ordering::SeqCst);
        bridge.sender().complete(token(12), permit).unwrap();
        assert_eq!(
            spy.writes.load(Ordering::SeqCst),
            writes_before,
            "shut down senders must not write a fd that is about to close"
        );
        spy.read_script
            .lock()
            .unwrap()
            .push(IoScript::Err(libc::EAGAIN));
        assert_eq!(bridge.harvest_and_take_ready().unwrap(), vec![token(12)]);
        bridge.shutdown_fd();
    }

    #[test]
    fn complete_after_close_does_not_write_closed_fd() {
        let (mut bridge, spy) = bridge_with_spy(4);
        let permit = bridge.try_reserve().unwrap();
        bridge.shutdown_fd();
        let _ = bridge.sender().complete(token(1), permit);
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
            let permit = bridge.try_reserve().unwrap();
            let sender = sender.clone();
            joins.push(thread::spawn(move || sender.complete(token(id), permit)));
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
    fn poisoned_queue_does_not_panic() {
        let bridge = NotifyBridge::new().unwrap();
        let pending = bridge.pending_arc();
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = pending.lock().expect("lock before poison");
            panic!("test poison");
        }));
        std::panic::set_hook(prev);
        assert_eq!(bridge.try_reserve().map(|_| ()), Err(NotifyError::Poisoned));
        let permit = CompletionPermit {
            pending: bridge.pending_arc(),
            live: true,
        };
        assert_eq!(
            bridge.sender().complete(token(1), permit),
            Err(NotifyError::Poisoned)
        );
        assert_eq!(bridge.harvest(), Err(NotifyError::Poisoned));
        assert_eq!(
            bridge.take_ready().expect("ready mutex is not poisoned"),
            Vec::<SleepToken>::new()
        );
    }

    #[test]
    fn completion_sender_is_send_and_owns_no_nginx_pointers() {
        fn assert_send<T: Send>() {}
        assert_send::<CompletionSender>();
        assert_send::<CompletionPermit>();
        assert_send::<SleepToken>();
        let name = std::any::type_name::<CompletionSender>();
        assert!(
            !name.contains("ngx_connection")
                && !name.contains("ngx_http")
                && !name.contains("ngx_pool")
                && !name.contains("Request"),
            "CompletionSender type must not mention NGINX data: {name}"
        );
        let permit_name = std::any::type_name::<CompletionPermit>();
        assert!(
            !permit_name.contains("ngx_connection")
                && !permit_name.contains("ngx_http")
                && !permit_name.contains("Request"),
            "CompletionPermit type must not mention NGINX data: {permit_name}"
        );
    }
}
