//! Request-driver state machine for access suspend/resume.
//!
//! Real pd-vm drives sleep Yield/Halt. A recording NGINX sink captures retain,
//! release, and phase continuation. No fake VM, no Tokio, no eventfd.

use ngx_rustscript_nginx_test_stubs as _;

use std::sync::Arc;

use ngx::core::Status;
use ngx_rustscript::config::compile_rss_source;
use ngx_rustscript::notify::DEFAULT_MAX_IN_FLIGHT;
use ngx_rustscript::request_state::{
    AccessExec, AccessStatus, ReadyEffect, RecordingSink, SubmitError, TokenRegistry,
};
use ngx_rustscript::runtime::WorkerRuntime;
use ngx_rustscript::sleep_binding::{SleepCoordinator, SleepSession, SleepToken};
use vm::{Program, VmStatus};

fn compile(source: &str) -> Arc<Program> {
    compile_rss_source(source).expect("compile with ngx::sleep catalog")
}

fn sleep_once(ms: i64) -> Arc<Program> {
    compile(&format!("use ngx;\nngx::sleep({ms});\n"))
}

fn two_sleeps(a: i64, b: i64) -> Arc<Program> {
    compile(&format!("use ngx;\nngx::sleep({a});\nngx::sleep({b});\n"))
}

fn bind_exec(program: Arc<Program>, request_id: u64, coordinator: &SleepCoordinator) -> AccessExec {
    let session = SleepSession::new(coordinator).expect("session");
    AccessExec::bind_program(program, request_id, session, coordinator.clone()).expect("bind")
}

struct FakeSubmit {
    ops: Vec<(SleepToken, i64)>,
    fail: Option<SubmitError>,
}

impl FakeSubmit {
    fn new() -> Self {
        Self {
            ops: Vec::new(),
            fail: None,
        }
    }
}

impl ngx_rustscript::request_state::SleepSubmit for FakeSubmit {
    fn submit(&mut self, token: SleepToken, ms: i64) -> Result<(), SubmitError> {
        if let Some(err) = self.fail {
            return Err(err);
        }
        self.ops.push((token, ms));
        Ok(())
    }
}

struct Driver {
    exec: AccessExec,
    sink: RecordingSink,
    registry: TokenRegistry<u64>,
    submit: FakeSubmit,
}

impl Driver {
    fn new(program: Arc<Program>) -> Self {
        let coordinator = SleepCoordinator::new();
        Self {
            exec: bind_exec(program, 1, &coordinator),
            sink: RecordingSink::default(),
            registry: TokenRegistry::with_capacity(DEFAULT_MAX_IN_FLIGHT),
            submit: FakeSubmit::new(),
        }
    }

    fn handle(&mut self) -> AccessStatus {
        self.exec
            .handle_access(&mut self.registry, &mut self.submit)
    }

    fn ready(&mut self, token: SleepToken) -> ReadyEffect {
        self.exec
            .apply_ready(token, &mut self.registry, &mut self.submit)
    }

    fn apply_effect(&mut self, effect: ReadyEffect) {
        ngx_rustscript::request_state::apply_ready_effect(effect, &mut self.sink);
    }
}

#[test]
fn halted_without_yield_returns_ok_and_does_not_advance_phase() {
    let mut driver = Driver::new(compile("let x = 1 + 2;\n"));
    assert_eq!(driver.handle(), AccessStatus::Ok);
    assert_eq!(driver.sink.retains, 0);
    assert_eq!(driver.sink.releases, 0);
    assert_eq!(driver.sink.phase_continues, 0);
    assert_eq!(driver.exec.vm_status(), Some(VmStatus::Halted));
}

#[test]
fn first_yield_returns_again_without_resume_or_phase_advance() {
    let mut driver = Driver::new(sleep_once(30));
    assert_eq!(driver.handle(), AccessStatus::Again);
    assert_eq!(driver.submit.ops.len(), 1);
    assert_eq!(driver.submit.ops[0].1, 30);
    assert_eq!(driver.registry.len(), 1);
    assert_eq!(driver.sink.retains, 0);
    assert_eq!(driver.sink.releases, 0);
    assert_eq!(driver.sink.phase_continues, 0);
    assert_eq!(driver.exec.vm_status(), Some(VmStatus::Yielded));
}

#[test]
fn early_completion_before_suspend_commit_does_not_resume() {
    let mut driver = Driver::new(sleep_once(0));
    let outcome = driver.exec.run_until_yield_or_halt().expect("run");
    let token = outcome.token().expect("yielded token");
    driver
        .exec
        .register_current(&mut driver.registry)
        .expect("register");
    let effect = driver.ready(token);
    assert_eq!(effect, ReadyEffect::Deferred);
    assert_eq!(driver.sink.phase_continues, 0);
    assert_eq!(driver.exec.vm_status(), Some(VmStatus::Yielded));

    driver.exec.commit_suspend();
    assert_eq!(driver.handle(), AccessStatus::Again);
    assert_eq!(driver.sink.phase_continues, 0);

    let effect = driver
        .exec
        .flush_deferred(&mut driver.registry, &mut driver.submit);
    driver.apply_effect(effect);
    assert_eq!(effect, ReadyEffect::Halted);
    assert_eq!(driver.sink.phase_continues, 1);
    assert_eq!(driver.handle(), AccessStatus::Ok);
    assert_eq!(driver.sink.phase_continues, 1);
}

#[test]
fn normal_completion_resumes_same_vm_and_advances_phase_once() {
    let mut driver = Driver::new(sleep_once(5));
    assert_eq!(driver.handle(), AccessStatus::Again);
    let token = driver.submit.ops[0].0;
    let effect = driver.ready(token);
    driver.apply_effect(effect);
    assert_eq!(effect, ReadyEffect::Halted);
    assert_eq!(driver.sink.phase_continues, 1);
    assert_eq!(driver.sink.retains, 0);
    assert_eq!(driver.sink.releases, 0);
    assert_eq!(driver.registry.len(), 0);
    assert_eq!(driver.handle(), AccessStatus::Ok);
    assert_eq!(driver.sink.phase_continues, 1);
}

#[test]
fn premature_duplicate_callback_is_ignored() {
    let mut driver = Driver::new(sleep_once(1));
    assert_eq!(driver.handle(), AccessStatus::Again);
    let token = driver.submit.ops[0].0;
    let first = driver.ready(token);
    driver.apply_effect(first);
    assert_eq!(first, ReadyEffect::Halted);
    let second = driver.ready(token);
    driver.apply_effect(second);
    assert_eq!(second, ReadyEffect::Ignored);
    assert_eq!(driver.sink.phase_continues, 1);
    assert_eq!(driver.handle(), AccessStatus::Ok);
}

#[test]
fn two_sequential_sleeps_resume_twice_and_advance_phase_once() {
    let mut driver = Driver::new(two_sleeps(1, 2));
    assert_eq!(driver.handle(), AccessStatus::Again);
    assert_eq!(driver.submit.ops.len(), 1);
    assert_eq!(driver.submit.ops[0].1, 1);
    let first = driver.submit.ops[0].0;

    let effect = driver.ready(first);
    driver.apply_effect(effect);
    assert_eq!(effect, ReadyEffect::YieldedAgain);
    assert_eq!(driver.submit.ops.len(), 2);
    assert_eq!(driver.submit.ops[1].1, 2);
    assert_eq!(driver.sink.phase_continues, 0);
    assert_eq!(driver.handle(), AccessStatus::Again);

    let second = driver.submit.ops[1].0;
    assert_ne!(first, second);
    let effect = driver.ready(second);
    driver.apply_effect(effect);
    assert_eq!(effect, ReadyEffect::Halted);
    assert_eq!(driver.sink.phase_continues, 1);
    assert_eq!(driver.handle(), AccessStatus::Ok);
    assert_eq!(driver.sink.phase_continues, 1);
}

#[test]
fn vm_error_on_first_run_is_http_500_without_phase_continue() {
    let mut driver = Driver::new(compile("1 / 0;\n"));
    assert_eq!(driver.handle(), AccessStatus::Error500);
    assert_eq!(driver.sink.phase_continues, 0);
    assert_eq!(driver.sink.retains, 0);
    assert_eq!(driver.sink.releases, 0);
}

#[test]
fn vm_error_on_resume_is_http_500_with_single_phase_continue() {
    let program = compile("use ngx;\nngx::sleep(1);\n1 / 0;\n");
    let mut driver = Driver::new(program);
    assert_eq!(driver.handle(), AccessStatus::Again);
    let token = driver.submit.ops[0].0;
    let effect = driver.ready(token);
    driver.apply_effect(effect);
    assert_eq!(effect, ReadyEffect::Failed);
    assert_eq!(driver.sink.phase_continues, 1);
    assert_eq!(driver.handle(), AccessStatus::Error500);
    assert_eq!(driver.sink.phase_continues, 1);
}

#[test]
fn submission_full_rejects_before_retain() {
    let mut driver = Driver::new(sleep_once(1));
    driver.submit.fail = Some(SubmitError::Full);
    assert_eq!(driver.handle(), AccessStatus::Error500);
    assert_eq!(driver.registry.len(), 0);
    assert_eq!(driver.sink.retains, 0);
    assert_eq!(driver.sink.phase_continues, 0);
}

#[test]
fn submission_closed_rejects_before_retain() {
    let mut driver = Driver::new(sleep_once(1));
    driver.submit.fail = Some(SubmitError::Closed);
    assert_eq!(driver.handle(), AccessStatus::Error500);
    assert_eq!(driver.registry.len(), 0);
    assert_eq!(driver.sink.retains, 0);
    assert_eq!(driver.sink.phase_continues, 0);
}

#[test]
fn unknown_token_is_ignored() {
    let mut driver = Driver::new(sleep_once(1));
    assert_eq!(driver.handle(), AccessStatus::Again);
    let effect = driver.ready(SleepToken::from_raw(99, 99));
    driver.apply_effect(effect);
    assert_eq!(effect, ReadyEffect::Ignored);
    assert_eq!(driver.sink.phase_continues, 0);
    assert_eq!(driver.handle(), AccessStatus::Again);
}

#[test]
fn wrong_generation_token_is_ignored() {
    let mut driver = Driver::new(sleep_once(1));
    assert_eq!(driver.handle(), AccessStatus::Again);
    let token = driver.submit.ops[0].0;
    let forged = SleepToken::from_raw(token.generation().wrapping_add(1), token.op_id());
    let effect = driver.ready(forged);
    driver.apply_effect(effect);
    assert_eq!(effect, ReadyEffect::Ignored);
    assert_eq!(driver.sink.phase_continues, 0);
    assert_eq!(driver.registry.len(), 1);
}

#[test]
fn registry_bound_rejects_when_full() {
    let mut registry = TokenRegistry::<u64>::with_capacity(1);
    registry
        .try_insert(SleepToken::from_raw(1, 1), 1)
        .expect("first");
    let err = registry
        .try_insert(SleepToken::from_raw(1, 2), 2)
        .expect_err("full");
    assert_eq!(err, ngx_rustscript::request_state::RegistryError::Full);
    assert_eq!(registry.len(), 1);
}

#[test]
fn duplicate_registry_insert_fails_and_keeps_original() {
    let mut registry = TokenRegistry::<u64>::with_capacity(8);
    let token = SleepToken::from_raw(1, 1);
    registry.try_insert(token, 1).expect("first");
    let err = registry
        .try_insert(token, 2)
        .expect_err("duplicate must not replace");
    assert_eq!(err, ngx_rustscript::request_state::RegistryError::Duplicate);
    assert_eq!(registry.get(token), Some(&1));
    assert_eq!(registry.len(), 1);
}

#[test]
fn two_requests_sharing_worker_coordinator_receive_distinct_first_tokens() {
    let mut worker = WorkerRuntime::start().unwrap();
    let program = sleep_once(5);
    let mut a = worker.bind_access_exec(Arc::clone(&program), 1).expect("a");
    let mut b = worker.bind_access_exec(program, 2).expect("b");
    let ta = a
        .run_until_yield_or_halt()
        .expect("a run")
        .token()
        .expect("a token");
    let tb = b
        .run_until_yield_or_halt()
        .expect("b run")
        .token()
        .expect("b token");
    assert_ne!(
        ta, tb,
        "overlapping requests must not share SleepToken generation/op_id"
    );
    worker.shutdown();
}

#[test]
fn unregister_one_request_does_not_remove_the_other() {
    let coordinator = SleepCoordinator::new();
    let program = sleep_once(1);
    let mut a = bind_exec(Arc::clone(&program), 1, &coordinator);
    let mut b = bind_exec(program, 2, &coordinator);
    let mut registry = TokenRegistry::with_capacity(8);
    let mut submit = FakeSubmit::new();
    assert_eq!(
        a.handle_access(&mut registry, &mut submit),
        AccessStatus::Again
    );
    assert_eq!(
        b.handle_access(&mut registry, &mut submit),
        AccessStatus::Again
    );
    let ta = a.current_token().expect("a token");
    let tb = b.current_token().expect("b token");
    assert_ne!(ta, tb);
    assert_eq!(registry.remove_exact(ta, &1), Some(1));
    assert_eq!(registry.get(tb), Some(&2));
    assert_eq!(registry.remove_exact(tb, &99), None);
    assert_eq!(registry.get(tb), Some(&2));
    drop(a);
    assert_eq!(registry.get(tb), Some(&2));
}

#[test]
fn wrong_token_cannot_resume_either_shared_request() {
    let coordinator = SleepCoordinator::new();
    let program = sleep_once(1);
    let mut a = bind_exec(Arc::clone(&program), 1, &coordinator);
    let mut b = bind_exec(program, 2, &coordinator);
    let mut registry = TokenRegistry::with_capacity(8);
    let mut submit = FakeSubmit::new();
    assert_eq!(
        a.handle_access(&mut registry, &mut submit),
        AccessStatus::Again
    );
    assert_eq!(
        b.handle_access(&mut registry, &mut submit),
        AccessStatus::Again
    );
    let ta = a.current_token().expect("a token");
    let tb = b.current_token().expect("b token");
    assert_eq!(
        a.apply_ready(tb, &mut registry, &mut submit),
        ReadyEffect::Ignored
    );
    assert_eq!(
        b.apply_ready(ta, &mut registry, &mut submit),
        ReadyEffect::Ignored
    );
    assert_eq!(a.vm_status(), Some(VmStatus::Yielded));
    assert_eq!(b.vm_status(), Some(VmStatus::Yielded));
    assert_eq!(
        a.apply_ready(ta, &mut registry, &mut submit),
        ReadyEffect::Halted
    );
    assert_eq!(
        b.apply_ready(tb, &mut registry, &mut submit),
        ReadyEffect::Halted
    );
}

#[test]
fn registry_capacity_matches_task4_max_in_flight() {
    let registry = TokenRegistry::<u64>::with_capacity(DEFAULT_MAX_IN_FLIGHT);
    assert_eq!(registry.capacity(), DEFAULT_MAX_IN_FLIGHT);
}

#[test]
fn yielded_maps_to_nginx_again() {
    assert_eq!(AccessStatus::Again.as_nginx(), Status::NGX_AGAIN);
    assert_eq!(AccessStatus::Ok.as_nginx(), Status::NGX_OK);
}

#[test]
fn access_again_is_the_phase_engine_wait_code() {
    // ngx_http_core_access_phase: NGX_AGAIN stops the checker without
    // incrementing phase_handler or requiring r->main->count++.
    assert_eq!(Status::NGX_AGAIN.0, -2);
}
