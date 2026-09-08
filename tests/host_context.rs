//! Worker-thread request execution scope: TLS stack, identity checks, and VM wiring.

use ngx_rustscript_nginx_test_stubs as _;

use nginx_sys::ngx_http_request_t;
use ngx_rustscript::config::compile_rss_source;
use ngx_rustscript::host::{
    AllowedOps, NgxPhase, RequestExecutionScope, ScopeError, current_allowed_ops, current_identity,
    current_phase, is_active, with_current_request, with_current_request_mut,
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

#[test]
fn scope_access_outside_vm_execution_fails() {
    let identity = RequestIdentity::new(1, 1);
    assert_eq!(current_identity(), Err(ScopeError::NotInRequestScope));
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

    let _outer = RequestExecutionScope::enter(
        outer_ptr,
        outer_id,
        NgxPhase::Access,
        AllowedOps::for_phase(NgxPhase::Access),
    );
    assert_eq!(current_identity().unwrap(), outer_id);
    let outer_seen = with_current_request(outer_id, |req| req as *const _ as usize).unwrap();
    {
        let _inner = RequestExecutionScope::enter(
            inner_ptr,
            inner_id,
            NgxPhase::Access,
            AllowedOps::for_phase(NgxPhase::Access),
        );
        assert_eq!(current_identity().unwrap(), inner_id);
        let inner_seen = with_current_request(inner_id, |req| req as *const _ as usize).unwrap();
        assert_ne!(inner_seen, outer_seen);
        assert_eq!(
            with_current_request(outer_id, |_| ()),
            Err(ScopeError::IdentityMismatch)
        );
    }
    assert_eq!(current_identity().unwrap(), outer_id);
    let restored = with_current_request(outer_id, |req| req as *const _ as usize).unwrap();
    assert_eq!(restored, outer_seen);
}

fn enter_access(
    request: *mut ngx_http_request_t,
    identity: RequestIdentity,
) -> RequestExecutionScope {
    RequestExecutionScope::enter(
        request,
        identity,
        NgxPhase::Access,
        AllowedOps::for_phase(NgxPhase::Access),
    )
}

#[test]
fn panic_unwind_restores_outer_and_clears_inner_scope() {
    let mut outer_req = DummyRequest::new();
    let mut inner_req = DummyRequest::new();
    let outer_id = RequestIdentity::new(4, 40);
    let inner_id = RequestIdentity::new(5, 50);
    let _outer = enter_access(outer_req.as_ptr(), outer_id);
    let panicked = std::panic::catch_unwind(AssertUnwindSafe(|| {
        let _inner = enter_access(inner_req.as_ptr(), inner_id);
        assert_eq!(current_identity().unwrap(), inner_id);
        panic!("request scope unwind");
    }));
    assert!(panicked.is_err());
    assert_eq!(current_identity().unwrap(), outer_id);
    assert_eq!(current_phase().unwrap(), NgxPhase::Access);
}

#[test]
fn panic_without_outer_scope_clears_tls() {
    let mut req = DummyRequest::new();
    let identity = RequestIdentity::new(6, 60);
    assert!(!is_active());
    let panicked = std::panic::catch_unwind(AssertUnwindSafe(|| {
        let _scope = enter_access(req.as_ptr(), identity);
        assert!(is_active());
        panic!("request scope unwind root");
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
    let _scope = enter_access(req.as_ptr(), live);
    assert_eq!(
        with_current_request(other, |_| ()),
        Err(ScopeError::IdentityMismatch)
    );
    assert_eq!(
        with_current_request_mut(other, |_| ()),
        Err(ScopeError::IdentityMismatch)
    );
    assert_eq!(current_identity().unwrap(), live);
}

#[test]
fn stale_generation_is_rejected_before_deref() {
    let mut req = DummyRequest::new();
    let live = RequestIdentity::new(1, 10);
    let stale = RequestIdentity::new(2, 10);
    let _scope = enter_access(req.as_ptr(), live);
    assert_eq!(
        with_current_request(stale, |_| ()),
        Err(ScopeError::StaleGeneration)
    );
    assert_eq!(
        with_current_request_mut(stale, |_| ()),
        Err(ScopeError::StaleGeneration)
    );
    let seen = with_current_request_mut(live, |request| request as *mut _ as usize).unwrap();
    assert_ne!(seen, 0);
}

#[test]
fn null_request_pointer_is_rejected_after_identity_check() {
    let identity = RequestIdentity::new(7, 70);
    let _scope = enter_access(std::ptr::null_mut(), identity);
    assert_eq!(current_identity().unwrap(), identity);
    assert_eq!(
        with_current_request(identity, |_| ()),
        Err(ScopeError::NullRequest)
    );
}

#[test]
fn access_phase_allows_read_and_log_not_write() {
    let mut req = DummyRequest::new();
    let identity = RequestIdentity::new(8, 80);
    let _scope = enter_access(req.as_ptr(), identity);
    let allowed = current_allowed_ops().unwrap();
    assert!(allowed.contains(AllowedOps::REQUEST_READ));
    assert!(allowed.contains(AllowedOps::LOG));
    assert!(!allowed.contains(AllowedOps::REQUEST_WRITE));
}

#[test]
fn async_capture_must_be_owned_and_cannot_keep_scope_across_await() {
    let mut req = DummyRequest::new();
    let identity = RequestIdentity::new(9, 90);
    let method = {
        let _scope = enter_access(req.as_ptr(), identity);
        assert!(is_active());
        String::from("GET")
    };
    assert!(!is_active());
    fn assert_send_static<T: Send + 'static>(value: T) -> T {
        value
    }
    let method = assert_send_static(method);
    let future = async move { method };
    let _future = assert_send_static(future);
}

#[test]
fn reenter_current_nests_then_restores() {
    let mut req = DummyRequest::new();
    let identity = RequestIdentity::new(10, 100);
    let _outer = enter_access(req.as_ptr(), identity);
    let nested = RequestExecutionScope::reenter_current(|| {
        assert_eq!(current_identity().unwrap(), identity);
        current_identity()
    });
    assert_eq!(nested.unwrap(), identity);
    assert_eq!(current_identity().unwrap(), identity);
}

#[test]
fn enter_for_vm_reuses_parent_request_and_restores() {
    let mut req = DummyRequest::new();
    let parent = RequestIdentity::new(12, 120);
    let vm_id = RequestIdentity::new(13, 130);
    let _outer = enter_access(req.as_ptr(), parent);
    let parent_seen = with_current_request(parent, |request| request as *const _ as usize).unwrap();
    {
        let _vm = RequestExecutionScope::enter_for_vm(
            vm_id,
            NgxPhase::Access,
            AllowedOps::for_phase(NgxPhase::Access),
        );
        assert_eq!(current_identity().unwrap(), vm_id);
        let vm_seen = with_current_request(vm_id, |request| request as *const _ as usize).unwrap();
        assert_eq!(vm_seen, parent_seen);
    }
    assert_eq!(current_identity().unwrap(), parent);
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

#[test]
fn access_exec_run_poll_resume_do_not_leave_scope_active() {
    let (worker, mut exec) = bind_exec(compile("let x = 1 + 2;\n"), 1);
    let mut registry = IdentityRegistry::with_capacity(DEFAULT_MAX_IN_FLIGHT);
    assert!(!is_active());
    assert_eq!(exec.handle_access(&mut registry, &worker), AccessStatus::Ok);
    assert!(!is_active());
    assert_eq!(current_identity(), Err(ScopeError::NotInRequestScope));
}

#[test]
fn access_exec_waiting_poll_resume_clear_scope_including_immediate_ready() {
    let (worker, mut exec) = bind_exec(compile("use ngx;\nngx::sleep_ms(0);\n"), 2);
    let mut registry = IdentityRegistry::with_capacity(DEFAULT_MAX_IN_FLIGHT);
    let status = exec.handle_access(&mut registry, &worker);
    assert!(!is_active());
    assert_eq!(current_identity(), Err(ScopeError::NotInRequestScope));
    if status == AccessStatus::Again {
        exec.commit_suspend();
        let effect = exec.apply_ready(exec.identity(), &mut registry, &worker);
        assert!(!is_active());
        assert!(matches!(
            effect,
            ReadyEffect::Halted | ReadyEffect::WaitingAgain | ReadyEffect::Ignored
        ));
    } else {
        assert_eq!(status, AccessStatus::Ok);
    }
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
fn parent_scope_survives_access_exec_vm_entries() {
    let mut req = DummyRequest::new();
    let parent = RequestIdentity::new(11, 110);
    let (worker, mut exec) = bind_exec(compile("let x = 1 + 2;\n"), 4);
    let mut registry = IdentityRegistry::with_capacity(DEFAULT_MAX_IN_FLIGHT);
    let _scope = enter_access(req.as_ptr(), parent);
    assert_eq!(exec.handle_access(&mut registry, &worker), AccessStatus::Ok);
    assert_eq!(current_identity().unwrap(), parent);
    assert!(is_active());
    {
        let _vm = RequestExecutionScope::enter_for_vm(
            exec.identity(),
            NgxPhase::Access,
            AllowedOps::for_phase(NgxPhase::Access),
        );
        assert_eq!(current_identity().unwrap(), exec.identity());
        assert_eq!(
            with_current_request(parent, |_| ()),
            Err(ScopeError::IdentityMismatch)
        );
    }
    assert_eq!(current_identity().unwrap(), parent);
}
