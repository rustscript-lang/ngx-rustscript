//! Worker-thread request execution scope: TLS stack, identity checks, and VM wiring.

use ngx_rustscript_nginx_test_stubs as _;

use nginx_sys::ngx_http_request_t;
use ngx_rustscript::config::compile_rss_source;
use ngx_rustscript::host::{
    AllowedOps, NgxPhase, ScopeError, capture_vm_entries, current_allowed_ops, current_identity,
    current_phase, current_request_live, is_active, with_current_request, with_current_request_mut,
    with_request_scope, with_vm_request_scope,
};
use ngx_rustscript::notify::{DEFAULT_MAX_IN_FLIGHT, RequestIdentity};
use ngx_rustscript::request_state::{AccessStatus, IdentityRegistry, ReadyEffect};
use ngx_rustscript::runtime::WorkerRuntime;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use vm::Program;

struct DummyRequest(Box<ngx_http_request_t>);

impl DummyRequest {
    fn new() -> Self {
        Self(Box::new(unsafe { std::mem::zeroed() }))
    }

    fn as_ptr(&mut self) -> *mut ngx_http_request_t {
        &mut *self.0
    }
}

fn access_ops() -> AllowedOps {
    AllowedOps::for_phase(NgxPhase::Access)
}

fn write_ops() -> AllowedOps {
    AllowedOps::REQUEST_READ.union(AllowedOps::REQUEST_WRITE)
}

fn enter_access<R>(
    request: *mut ngx_http_request_t,
    identity: RequestIdentity,
    f: impl FnOnce() -> R,
) -> R {
    with_request_scope(request, identity, NgxPhase::Access, access_ops(), f)
}

#[test]
fn scope_access_outside_vm_execution_fails() {
    let identity = RequestIdentity::new(1, 1);
    assert_eq!(current_identity(), Err(ScopeError::NotInRequestScope));
    assert_eq!(current_request_live(), Err(ScopeError::NotInRequestScope));
    assert_eq!(
        with_current_request(identity, |_| ()),
        Err(ScopeError::NotInRequestScope)
    );
}

#[test]
fn nested_scope_restores_outer_request() {
    let mut outer_req = DummyRequest::new();
    let mut inner_req = DummyRequest::new();
    let outer_id = RequestIdentity::new(1, 10);
    let inner_id = RequestIdentity::new(2, 20);
    let outer_ptr = outer_req.as_ptr();
    let inner_ptr = inner_req.as_ptr();

    enter_access(outer_ptr, outer_id, || {
        assert_eq!(current_identity().unwrap(), outer_id);
        assert_eq!(current_request_live(), Ok(true));
        assert!(with_current_request(outer_id, |_| ()).is_ok());
        enter_access(inner_ptr, inner_id, || {
            assert_eq!(current_identity().unwrap(), inner_id);
            assert_eq!(current_request_live(), Ok(true));
            assert_eq!(
                with_current_request(outer_id, |_| ()),
                Err(ScopeError::IdentityMismatch)
            );
            assert!(with_current_request(inner_id, |_| ()).is_ok());
        });
        assert_eq!(current_identity().unwrap(), outer_id);
        assert_eq!(current_request_live(), Ok(true));
        assert!(with_current_request(outer_id, |_| ()).is_ok());
        assert_eq!(
            with_current_request(inner_id, |_| ()),
            Err(ScopeError::IdentityMismatch)
        );
    });
}

#[test]
fn panic_unwind_restores_outer_and_clears_inner_scope() {
    let mut outer_req = DummyRequest::new();
    let mut inner_req = DummyRequest::new();
    let outer_id = RequestIdentity::new(4, 40);
    let inner_id = RequestIdentity::new(5, 50);
    enter_access(outer_req.as_ptr(), outer_id, || {
        let panicked = std::panic::catch_unwind(AssertUnwindSafe(|| {
            enter_access(inner_req.as_ptr(), inner_id, || {
                assert_eq!(current_identity().unwrap(), inner_id);
                panic!("request scope unwind");
            });
        }));
        assert!(panicked.is_err());
        assert_eq!(current_identity().unwrap(), outer_id);
        assert_eq!(current_phase().unwrap(), NgxPhase::Access);
        assert_eq!(current_request_live(), Ok(true));
        assert!(with_current_request(outer_id, |_| ()).is_ok());
    });
}

#[test]
fn panic_without_outer_scope_clears_tls() {
    let mut req = DummyRequest::new();
    let identity = RequestIdentity::new(6, 60);
    assert!(!is_active());
    let panicked = std::panic::catch_unwind(AssertUnwindSafe(|| {
        enter_access(req.as_ptr(), identity, || {
            assert!(is_active());
            panic!("request scope unwind root");
        });
    }));
    assert!(panicked.is_err());
    assert!(!is_active());
    assert_eq!(current_identity(), Err(ScopeError::NotInRequestScope));
}

#[test]
fn distinct_request_identities_do_not_cross() {
    let mut req = DummyRequest::new();
    let live = RequestIdentity::new(1, 10);
    let other = RequestIdentity::new(1, 11);
    enter_access(req.as_ptr(), live, || {
        assert_eq!(
            with_current_request(other, |_| ()),
            Err(ScopeError::IdentityMismatch)
        );
        assert_eq!(
            with_current_request_mut(other, |_| ()),
            Err(ScopeError::IdentityMismatch)
        );
        assert_eq!(current_identity().unwrap(), live);
    });
}

#[test]
fn stale_generation_is_rejected_before_deref() {
    let mut req = DummyRequest::new();
    let live = RequestIdentity::new(1, 10);
    let stale = RequestIdentity::new(2, 10);
    with_request_scope(req.as_ptr(), live, NgxPhase::Access, write_ops(), || {
        assert_eq!(
            with_current_request(stale, |_| ()),
            Err(ScopeError::StaleGeneration)
        );
        assert_eq!(
            with_current_request_mut(stale, |_| ()),
            Err(ScopeError::StaleGeneration)
        );
        assert_eq!(current_request_live(), Ok(true));
        assert!(with_current_request_mut(live, |_| ()).is_ok());
    });
}

#[test]
fn null_request_pointer_is_rejected_after_identity_check() {
    let identity = RequestIdentity::new(7, 70);
    enter_access(std::ptr::null_mut(), identity, || {
        assert_eq!(current_identity().unwrap(), identity);
        assert_eq!(current_request_live(), Ok(false));
        assert_eq!(
            with_current_request(identity, |_| ()),
            Err(ScopeError::NullRequest)
        );
    });
}

#[test]
fn access_phase_allows_read_and_log_not_write() {
    let mut req = DummyRequest::new();
    let identity = RequestIdentity::new(8, 80);
    enter_access(req.as_ptr(), identity, || {
        let allowed = current_allowed_ops().unwrap();
        assert!(allowed.contains(AllowedOps::REQUEST_READ));
        assert!(allowed.contains(AllowedOps::LOG));
        assert!(!allowed.contains(AllowedOps::REQUEST_WRITE));
        assert!(with_current_request(identity, |_| ()).is_ok());
        assert_eq!(
            with_current_request_mut(identity, |_| ()),
            Err(ScopeError::OpsNotAllowed)
        );
    });
}

#[test]
fn read_access_requires_request_read() {
    let mut req = DummyRequest::new();
    let identity = RequestIdentity::new(18, 180);
    with_request_scope(
        req.as_ptr(),
        identity,
        NgxPhase::Access,
        AllowedOps::LOG,
        || {
            assert_eq!(
                with_current_request(identity, |_| ()),
                Err(ScopeError::OpsNotAllowed)
            );
        },
    );
}

#[test]
fn nested_mutable_request_access_is_rejected() {
    let mut req = DummyRequest::new();
    let identity = RequestIdentity::new(19, 190);
    with_request_scope(
        req.as_ptr(),
        identity,
        NgxPhase::Access,
        write_ops(),
        || {
            let nested =
                with_current_request_mut(identity, |_| with_current_request_mut(identity, |_| ()));
            assert_eq!(nested, Ok(Err(ScopeError::AlreadyBorrowed)));
            assert!(with_current_request_mut(identity, |_| ()).is_ok());
        },
    );
}

#[test]
fn owned_async_capture_cannot_retain_scope_or_request() {
    let mut req = DummyRequest::new();
    let identity = RequestIdentity::new(9, 90);
    let method = enter_access(req.as_ptr(), identity, || {
        assert!(is_active());
        assert_eq!(current_request_live(), Ok(true));
        String::from("GET")
    });
    assert!(!is_active());
    fn assert_send_static<T: Send + 'static>(value: T) -> T {
        value
    }
    let method = assert_send_static(method);
    let future = async move { method };
    let _future = assert_send_static(future);
}

#[test]
fn public_scope_api_forbids_retaining_request_references() {
    let src = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/host/context.rs"));
    assert!(
        src.contains("for<'a> FnOnce(&'a Request)")
            && src.contains("for<'a> FnOnce(&'a mut Request)"),
        "request accessors must use HRTB so ngx::Request cannot outlive the closure"
    );
    assert!(
        !src.contains("pub fn current_request_ptr"),
        "raw request pointers must not be a public API"
    );
    assert!(
        src.contains("pub(crate) fn current_request_ptr")
            || !src.contains("fn current_request_ptr"),
        "internal pointer helper must stay crate-private"
    );
}

#[test]
fn enter_for_vm_rejects_mismatched_parent_identity() {
    let mut req = DummyRequest::new();
    let parent = RequestIdentity::new(12, 120);
    let vm_id = RequestIdentity::new(13, 130);
    enter_access(req.as_ptr(), parent, || {
        assert_eq!(current_request_live(), Ok(true));
        with_vm_request_scope(vm_id, NgxPhase::Access, access_ops(), || {
            assert_eq!(current_identity().unwrap(), vm_id);
            assert_eq!(current_request_live(), Ok(false));
            assert_eq!(
                with_current_request(vm_id, |_| ()),
                Err(ScopeError::NullRequest)
            );
            assert_eq!(
                with_current_request(parent, |_| ()),
                Err(ScopeError::IdentityMismatch)
            );
        });
        assert_eq!(current_identity().unwrap(), parent);
        assert_eq!(current_request_live(), Ok(true));
        assert!(with_current_request(parent, |_| ()).is_ok());
    });
}

#[test]
fn enter_for_vm_uses_matching_outer_pointer_identity_pair() {
    let mut req = DummyRequest::new();
    let identity = RequestIdentity::new(14, 140);
    enter_access(req.as_ptr(), identity, || {
        with_vm_request_scope(identity, NgxPhase::Access, access_ops(), || {
            assert_eq!(current_identity().unwrap(), identity);
            assert_eq!(current_request_live(), Ok(true));
            assert_eq!(current_allowed_ops().unwrap(), access_ops());
            assert!(with_current_request(identity, |_| ()).is_ok());
        });
        assert_eq!(current_identity().unwrap(), identity);
    });
}

fn compile(source: &str) -> Arc<Program> {
    compile_rss_source(source).expect("compile")
}

fn bind_exec(
    program: Arc<Program>,
    request_id: u64,
) -> (WorkerRuntime, ngx_rustscript::request_state::AccessExec) {
    let mut worker = WorkerRuntime::start().unwrap();
    let exec = worker.bind_access_exec(program, request_id).expect("bind");
    (worker, exec)
}

fn assert_vm_entries_match(
    entries: &[ngx_rustscript::host::VmEntryObservation],
    identity: RequestIdentity,
    request_live: bool,
) {
    assert!(
        !entries.is_empty(),
        "VM run/poll/resume must record request-scope observations"
    );
    for entry in entries {
        assert_eq!(entry.identity, identity);
        assert_eq!(entry.phase, NgxPhase::Access);
        assert_eq!(entry.allowed, access_ops());
        assert_eq!(entry.request_live, request_live);
        assert!(entry.allowed.contains(AllowedOps::REQUEST_READ));
        assert!(!entry.allowed.contains(AllowedOps::REQUEST_WRITE));
    }
}

#[test]
fn access_exec_without_outer_scope_does_not_authorize_request() {
    let (worker, mut exec) = bind_exec(compile("let x = 1 + 2;\n"), 1);
    let mut registry = IdentityRegistry::with_capacity(DEFAULT_MAX_IN_FLIGHT);
    let identity = exec.identity();
    assert_ne!(identity, RequestIdentity::new(0, 0));
    let (status, entries) = capture_vm_entries(|| exec.handle_access(&mut registry, &worker));
    assert_eq!(status, AccessStatus::Ok);
    assert_vm_entries_match(&entries, identity, false);
    assert!(!is_active());
    assert_eq!(current_identity(), Err(ScopeError::NotInRequestScope));
}

#[test]
fn access_exec_run_poll_resume_observe_paired_outer_scope() {
    let mut req = DummyRequest::new();
    let (worker, mut exec) = bind_exec(compile("use ngx;\nngx::sleep_ms(0);\n"), 2);
    let mut registry = IdentityRegistry::with_capacity(DEFAULT_MAX_IN_FLIGHT);
    let identity = exec.identity();
    assert_ne!(identity, RequestIdentity::new(0, 0));
    let (status, entries) = enter_access(req.as_ptr(), identity, || {
        capture_vm_entries(|| exec.handle_access(&mut registry, &worker))
    });
    assert_vm_entries_match(&entries, identity, true);
    assert!(!is_active());
    if status == AccessStatus::Again {
        exec.commit_suspend();
        let (effect, ready_entries) = enter_access(req.as_ptr(), identity, || {
            capture_vm_entries(|| exec.apply_ready(identity, &mut registry, &worker))
        });
        assert_vm_entries_match(&ready_entries, identity, true);
        assert!(matches!(
            effect,
            ReadyEffect::Halted | ReadyEffect::WaitingAgain | ReadyEffect::Ignored
        ));
    } else {
        assert_eq!(status, AccessStatus::Ok);
    }
    assert!(!is_active());
}

#[test]
fn access_exec_error_path_clears_scope() {
    let (worker, mut exec) = bind_exec(compile("1 / 0;\n"), 3);
    let mut registry = IdentityRegistry::with_capacity(DEFAULT_MAX_IN_FLIGHT);
    assert_eq!(
        exec.handle_access(&mut registry, &worker),
        AccessStatus::Error500
    );
    assert!(!is_active());
}

#[test]
fn access_exec_generation_mismatch_does_not_retag_pointer() {
    let mut req = DummyRequest::new();
    let (worker, mut exec) = bind_exec(compile("let x = 1 + 2;\n"), 4);
    let mut registry = IdentityRegistry::with_capacity(DEFAULT_MAX_IN_FLIGHT);
    let identity = exec.identity();
    let mismatched =
        RequestIdentity::new(identity.generation().wrapping_add(1), identity.request_id());
    let (status, entries) = enter_access(req.as_ptr(), mismatched, || {
        capture_vm_entries(|| exec.handle_access(&mut registry, &worker))
    });
    assert_eq!(status, AccessStatus::Ok);
    assert_vm_entries_match(&entries, identity, false);
    assert_eq!(current_identity(), Err(ScopeError::NotInRequestScope));
}

#[test]
fn parent_scope_survives_access_exec_vm_entries() {
    let mut req = DummyRequest::new();
    let parent = RequestIdentity::new(11, 110);
    let (worker, mut exec) = bind_exec(compile("let x = 1 + 2;\n"), 5);
    let mut registry = IdentityRegistry::with_capacity(DEFAULT_MAX_IN_FLIGHT);
    enter_access(req.as_ptr(), parent, || {
        assert_eq!(exec.handle_access(&mut registry, &worker), AccessStatus::Ok);
        assert_eq!(current_identity().unwrap(), parent);
        assert!(is_active());
        with_vm_request_scope(exec.identity(), NgxPhase::Access, access_ops(), || {
            assert_eq!(current_identity().unwrap(), exec.identity());
            assert_eq!(current_request_live(), Ok(false));
            assert_eq!(
                with_current_request(parent, |_| ()),
                Err(ScopeError::IdentityMismatch)
            );
        });
        assert_eq!(current_identity().unwrap(), parent);
        assert_eq!(current_request_live(), Ok(true));
    });
}

#[test]
fn production_access_does_not_use_sentinel_identity() {
    let access_src = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/access.rs"));
    assert!(
        !access_src.contains("RequestIdentity::new(0, 0)"),
        "sync pooled access must use a generation-safe identity"
    );
    assert!(
        !access_src.contains("sync_scope_identity"),
        "sentinel RequestIdentity helper must be removed"
    );
}

#[test]
fn internal_enter_helpers_are_crate_private() {
    let src = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/host/context.rs"));
    assert!(
        !src.contains("pub fn enter("),
        "RequestExecutionScope::enter must not be public"
    );
    assert!(
        !src.contains("pub fn reenter_current"),
        "reenter_current must not be public"
    );
    assert!(
        !src.contains("pub fn enter_for_vm"),
        "enter_for_vm must not be public"
    );
}
