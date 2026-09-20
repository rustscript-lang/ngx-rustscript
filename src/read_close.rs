//! Client-close detection for a suspended access request.
//!
//! Pinned NGINX 1.31.4 exports `ngx_http_test_reading` (`ngx_http.h`; nginx-sys
//! `cda9d837` binds `pub fn ngx_http_test_reading`). That function is the
//! stock read handler used by `ngx_http_set_write_handler`, auth delay, and
//! `limit_req`. Returning `NGX_AGAIN` leaves `r->read_event_handler =
//! ngx_http_block_reading`, which ignores epoll RDHUP, so this module installs
//! a narrow handler with the Linux/epoll subset of `ngx_http_test_reading`:
//! pending EOF/error flags, `recv(MSG_PEEK)`, EINTR retry, EAGAIN/readable
//! data leave the request suspended, and confirmed close finalizes once with
//! `NGX_HTTP_CLIENT_CLOSED_REQUEST`.

/// Event bits read from `ngx_event_t` / `ngx_connection_t`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EventFlags {
    pub pending_eof: bool,
    pub error: bool,
}

/// One `recv(fd, buf, 1, MSG_PEEK)` outcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PeekResult {
    Eof,
    Bytes(usize),
    Again,
    Interrupted,
    Error(i32),
}

/// Whether the suspended request should be finalized as client-closed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloseCheck {
    Closed,
    Stay,
}

/// Guarded handler outcome. Finalize runs at most once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HandlerAction {
    Ignored,
    Finalized,
    Suspended,
}

/// Once-finalize latch plus event flags for one handler invocation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HandlerState {
    pub already_done: bool,
    pub flags: EventFlags,
}

/// Linux/epoll subset of NGINX 1.31.4 `ngx_http_test_reading`.
///
/// Pending EOF or a connection/event error is confirmed close. Otherwise
/// `peek` is `recv(MSG_PEEK)`: EINTR retries, EAGAIN and readable bytes stay
/// suspended, EOF and hard errors close.
pub fn check_client_close(flags: EventFlags, mut peek: impl FnMut() -> PeekResult) -> CloseCheck {
    if flags.error || flags.pending_eof {
        return CloseCheck::Closed;
    }
    loop {
        match peek() {
            PeekResult::Interrupted => {}
            PeekResult::Again | PeekResult::Bytes(_) => return CloseCheck::Stay,
            PeekResult::Eof | PeekResult::Error(_) => return CloseCheck::Closed,
        }
    }
}

/// Worker-thread handler body: skip if already completed/cancelled, else
/// inspect close and finalize exactly once.
pub fn run_read_close_handler(
    state: &mut HandlerState,
    mut peek: impl FnMut() -> PeekResult,
    finalize: impl FnOnce(),
) -> HandlerAction {
    if state.already_done {
        return HandlerAction::Ignored;
    }
    match check_client_close(state.flags, &mut peek) {
        CloseCheck::Stay => HandlerAction::Suspended,
        CloseCheck::Closed => {
            state.already_done = true;
            finalize();
            HandlerAction::Finalized
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn flags(pending_eof: bool, error: bool) -> EventFlags {
        EventFlags { pending_eof, error }
    }

    fn script(results: &[PeekResult]) -> impl FnMut() -> PeekResult + '_ {
        let i = Cell::new(0);
        move || {
            let idx = i.get();
            i.set(idx + 1);
            results.get(idx).copied().unwrap_or(PeekResult::Again)
        }
    }

    #[test]
    fn eagain_stays_suspended() {
        assert_eq!(
            check_client_close(flags(false, false), script(&[PeekResult::Again])),
            CloseCheck::Stay
        );
    }

    #[test]
    fn readable_data_stays_suspended() {
        assert_eq!(
            check_client_close(flags(false, false), script(&[PeekResult::Bytes(1)])),
            CloseCheck::Stay
        );
    }

    #[test]
    fn eof_is_closed() {
        assert_eq!(
            check_client_close(flags(false, false), script(&[PeekResult::Eof])),
            CloseCheck::Closed
        );
    }

    #[test]
    fn eintr_then_eof_is_closed() {
        assert_eq!(
            check_client_close(
                flags(false, false),
                script(&[PeekResult::Interrupted, PeekResult::Eof])
            ),
            CloseCheck::Closed
        );
    }

    #[test]
    fn hard_recv_error_is_closed() {
        assert_eq!(
            check_client_close(
                flags(false, false),
                script(&[PeekResult::Error(libc::ECONNRESET)])
            ),
            CloseCheck::Closed
        );
    }

    #[test]
    fn pending_eof_is_closed_without_peek() {
        let peeks = Cell::new(0);
        let check = check_client_close(flags(true, false), || {
            peeks.set(peeks.get() + 1);
            PeekResult::Bytes(1)
        });
        assert_eq!(check, CloseCheck::Closed);
        assert_eq!(peeks.get(), 0);
    }

    #[test]
    fn error_flag_is_closed_without_peek() {
        let peeks = Cell::new(0);
        let check = check_client_close(flags(false, true), || {
            peeks.set(peeks.get() + 1);
            PeekResult::Again
        });
        assert_eq!(check, CloseCheck::Closed);
        assert_eq!(peeks.get(), 0);
    }

    #[test]
    fn handler_finalizes_once_on_eof() {
        let finalized = Cell::new(0);
        let mut state = HandlerState {
            already_done: false,
            flags: flags(false, false),
        };
        let first = run_read_close_handler(&mut state, script(&[PeekResult::Eof]), || {
            finalized.set(finalized.get() + 1)
        });
        let second = run_read_close_handler(&mut state, script(&[PeekResult::Eof]), || {
            finalized.set(finalized.get() + 1)
        });
        assert_eq!(first, HandlerAction::Finalized);
        assert_eq!(second, HandlerAction::Ignored);
        assert_eq!(finalized.get(), 1);
        assert!(state.already_done);
    }

    #[test]
    fn already_done_does_not_finalize() {
        let finalized = Cell::new(0);
        let mut state = HandlerState {
            already_done: true,
            flags: flags(true, true),
        };
        let action = run_read_close_handler(&mut state, script(&[PeekResult::Eof]), || {
            finalized.set(finalized.get() + 1)
        });
        assert_eq!(action, HandlerAction::Ignored);
        assert_eq!(finalized.get(), 0);
    }

    #[test]
    fn eagain_handler_leaves_request_suspended() {
        let finalized = Cell::new(0);
        let mut state = HandlerState::default();
        let action = run_read_close_handler(&mut state, script(&[PeekResult::Again]), || {
            finalized.set(finalized.get() + 1)
        });
        assert_eq!(action, HandlerAction::Suspended);
        assert_eq!(finalized.get(), 0);
        assert!(!state.already_done);
    }
}
