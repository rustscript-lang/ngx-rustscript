//! Real pd-vm Yield re-entry for `ngx::sleep`.
//!
//! Completions are test-controlled. No Tokio, eventfd, or NGINX suspension.

use ngx_rustscript_nginx_test_stubs as _;

use std::sync::{Arc, Mutex};

use ngx_rustscript::sleep_binding::{
    NGX_SLEEP_NAME, SleepCoordinator, SleepOpsError, SleepSession, SleepToken, bind_ngx_sleep,
    ngx_sleep_catalog, sleep_function_schema,
};
use vm::{
    CallOutcome, CallReturn, CompileSourceFileOptions, HostApiBuilder, HostArgsFunction,
    HostFunctionSchema, HostParamSchema, HostTypeSchema, SourceFlavor, Value, Vm, VmError,
    VmStatus, compile_source, compile_source_with_flavor_and_options,
};

fn mark_schema() -> HostFunctionSchema {
    HostFunctionSchema::with_return(
        "ngx::mark",
        vec![HostParamSchema::value("token", HostTypeSchema::Int)],
        HostTypeSchema::Null,
    )
}

fn test_catalog() -> vm::HostApiCatalog {
    let mut builder = HostApiBuilder::new();
    builder.function(sleep_function_schema());
    builder.function(mark_schema());
    builder.build().expect("ngx sleep+mark catalog")
}

fn compile_ngx(source: &str) -> vm::CompiledProgram {
    let catalog = Arc::new(test_catalog());
    compile_source_with_flavor_and_options(
        source,
        SourceFlavor::RustScript,
        CompileSourceFileOptions::default().with_host_api_catalog(catalog),
    )
    .unwrap_or_else(|err| panic!("compile ngx source: {err}"))
}

struct MarkProbe {
    tokens: Arc<Mutex<Vec<i64>>>,
}

impl HostArgsFunction for MarkProbe {
    fn call(&mut self, args: &[vm::Value]) -> vm::VmResult<CallOutcome> {
        let token = match args {
            [Value::Int(token)] => *token,
            _ => {
                return Err(vm::VmError::HostError(
                    "ngx::mark expects one integer token".to_string(),
                ));
            }
        };
        self.tokens.lock().expect("mark probe mutex").push(token);
        Ok(CallOutcome::Return(CallReturn::one(Value::Null)))
    }
}

/// Test-only wrapper that records the exact host `args` on every entry.
struct RecordingSleep {
    inner: SleepSession,
    args: Arc<Mutex<Vec<Vec<Value>>>>,
}

impl HostArgsFunction for RecordingSleep {
    fn call(&mut self, args: &[Value]) -> vm::VmResult<CallOutcome> {
        self.args
            .lock()
            .expect("sleep args log")
            .push(args.to_vec());
        self.inner.call(args)
    }
}

struct SleepHarness {
    vm: Vm,
    coordinator: SleepCoordinator,
    marks: Arc<Mutex<Vec<i64>>>,
    recorded_args: Arc<Mutex<Vec<Vec<Value>>>>,
}

fn bind_sleep_and_mark(
    vm: &mut Vm,
    coordinator: &SleepCoordinator,
    marks: &Arc<Mutex<Vec<i64>>>,
    recorded_args: &Arc<Mutex<Vec<Vec<Value>>>>,
) {
    let session = SleepSession::new(coordinator).expect("sleep session");
    vm.bind_args_function(
        NGX_SLEEP_NAME,
        Box::new(RecordingSleep {
            inner: session,
            args: Arc::clone(recorded_args),
        }),
    );
    vm.bind_args_function(
        "ngx::mark",
        Box::new(MarkProbe {
            tokens: Arc::clone(marks),
        }),
    );
}

fn harness_from_source(source: &str) -> SleepHarness {
    harness_from_source_with(source, SleepCoordinator::new())
}

fn harness_from_source_with(source: &str, coordinator: SleepCoordinator) -> SleepHarness {
    let compiled = compile_ngx(source);
    assert!(
        compiled
            .program
            .imports
            .iter()
            .any(|import| import.name == NGX_SLEEP_NAME),
        "compiled program must import {NGX_SLEEP_NAME}"
    );
    let mut vm = Vm::new(compiled.program);
    let marks = Arc::new(Mutex::new(Vec::new()));
    let recorded_args = Arc::new(Mutex::new(Vec::new()));
    bind_sleep_and_mark(&mut vm, &coordinator, &marks, &recorded_args);
    SleepHarness {
        vm,
        coordinator,
        marks,
        recorded_args,
    }
}

fn marks_of(harness: &SleepHarness) -> Vec<i64> {
    harness.marks.lock().expect("mark probe mutex").clone()
}

fn recorded_args_of(harness: &SleepHarness) -> Vec<Vec<Value>> {
    harness
        .recorded_args
        .lock()
        .expect("sleep args log")
        .clone()
}

fn claim_one(coordinator: &SleepCoordinator) -> (SleepToken, i64) {
    coordinator
        .claim_submission()
        .expect("coordinator lock")
        .expect("queued sleep submission")
}

fn fixture(name: &str) -> &'static str {
    match name {
        "sleep.rss" => include_str!("fixtures/sleep.rss"),
        "two_sleeps.rss" => include_str!("fixtures/two_sleeps.rss"),
        "sleep_null.rss" => include_str!("fixtures/sleep_null.rss"),
        other => panic!("unknown fixture {other}"),
    }
}

#[test]
fn compile_source_still_compiles_import_free_scripts() {
    let compiled = compile_source("let x = 1 + 2;\n").expect("real compile_source");
    assert!(compiled.program.imports.is_empty());
}

#[test]
fn ngx_sleep_catalog_declares_one_int_ms_and_null_return() {
    let schema = sleep_function_schema();
    assert_eq!(schema.name, NGX_SLEEP_NAME);
    assert_eq!(schema.params.len(), 1);
    assert_eq!(schema.params[0].name, "ms");
    assert_eq!(schema.params[0].ty, HostTypeSchema::Int);
    assert_eq!(schema.return_type, HostTypeSchema::Null);
    let catalog = ngx_sleep_catalog();
    assert_eq!(catalog.functions().len(), 1);
    assert_eq!(catalog.functions()[0].name, NGX_SLEEP_NAME);
}

#[test]
fn first_sleep_yields_before_post_action_and_submits_once() {
    let mut harness = harness_from_source(fixture("sleep.rss"));
    let status = harness.vm.run().expect("first run");
    println!("status sequence start: {status:?}");
    assert_eq!(status, VmStatus::Yielded);
    assert_eq!(harness.coordinator.host_entry_count().expect("lock"), 1);
    assert_eq!(harness.coordinator.submission_count().expect("lock"), 1);
    assert_eq!(harness.coordinator.consumption_count().expect("lock"), 0);
    assert_eq!(harness.coordinator.operation_count().expect("lock"), 1);
    let (token, ms) = claim_one(&harness.coordinator);
    assert_eq!(ms, 10);
    assert_eq!(token.op_id(), 1);
    assert_eq!(
        recorded_args_of(&harness),
        vec![vec![Value::Int(10)]],
        "first entry must see the compiled milliseconds argument"
    );
    assert!(
        marks_of(&harness).is_empty(),
        "ngx::mark must not run before sleep completes"
    );
}

#[test]
fn controlled_completion_reenters_once_returns_and_halts() {
    let mut harness = harness_from_source(fixture("sleep.rss"));
    assert_eq!(harness.vm.run().expect("yield"), VmStatus::Yielded);
    let yield_ip = harness.vm.ip();
    let (token, ms) = claim_one(&harness.coordinator);
    assert_eq!(ms, 10);
    harness.coordinator.complete(token).expect("complete");
    let status = harness.vm.resume().expect("resume after complete");
    println!("status after complete resume: {status:?}");
    assert_eq!(status, VmStatus::Halted);
    assert_ne!(
        harness.vm.ip(),
        yield_ip,
        "IP must advance past the sleep call after completion"
    );
    assert_eq!(harness.coordinator.host_entry_count().expect("lock"), 2);
    assert_eq!(harness.coordinator.submission_count().expect("lock"), 1);
    assert_eq!(harness.coordinator.consumption_count().expect("lock"), 1);
    assert_eq!(harness.coordinator.operation_count().expect("lock"), 0);
    assert_eq!(marks_of(&harness), vec![1]);
    assert_eq!(
        recorded_args_of(&harness),
        vec![vec![Value::Int(10)], vec![Value::Int(10)]],
        "completed reentry must see the same milliseconds argument"
    );
}

#[test]
fn sleep_null_fixture_final_value_is_sleep_return_without_mark() {
    let mut harness = harness_from_source(fixture("sleep_null.rss"));
    assert_eq!(harness.vm.run().expect("yield"), VmStatus::Yielded);
    assert!(marks_of(&harness).is_empty());
    let (token, ms) = claim_one(&harness.coordinator);
    assert_eq!(ms, 10);
    harness.coordinator.complete(token).expect("complete");
    assert_eq!(harness.vm.resume().expect("halt"), VmStatus::Halted);
    assert!(
        marks_of(&harness).is_empty(),
        "sleep_null.rss must not call ngx::mark"
    );
    assert_eq!(
        harness.vm.stack(),
        &[Value::Null][..],
        "final observable value must be ngx::sleep Null with no mark call: {:?}",
        harness.vm.stack()
    );
}

#[test]
fn premature_resume_yields_again_without_resubmit_same_ip_and_args() {
    let mut harness = harness_from_source(fixture("sleep.rss"));
    assert_eq!(harness.vm.run().expect("yield"), VmStatus::Yielded);
    let yield_ip = harness.vm.ip();
    let (token, ms) = claim_one(&harness.coordinator);
    assert_eq!(ms, 10);
    let status = harness.vm.resume().expect("premature resume");
    println!("premature resume status: {status:?}");
    assert_eq!(status, VmStatus::Yielded);
    assert_eq!(
        harness.vm.ip(),
        yield_ip,
        "premature resume must stay on the same host-call IP"
    );
    assert_eq!(harness.coordinator.host_entry_count().expect("lock"), 2);
    assert_eq!(harness.coordinator.submission_count().expect("lock"), 1);
    assert_eq!(harness.coordinator.operation_count().expect("lock"), 1);
    assert!(
        harness
            .coordinator
            .claim_submission()
            .expect("lock")
            .is_none(),
        "premature resume must not enqueue a second claimable submission"
    );
    assert_eq!(
        recorded_args_of(&harness),
        vec![vec![Value::Int(10)], vec![Value::Int(10)]],
        "premature reentry must see the same milliseconds argument"
    );
    assert!(marks_of(&harness).is_empty());
    harness.coordinator.complete(token).expect("complete");
    assert_eq!(harness.vm.resume().expect("halt"), VmStatus::Halted);
    assert_ne!(harness.vm.ip(), yield_ip);
    assert_eq!(harness.coordinator.submission_count().expect("lock"), 1);
    assert_eq!(harness.coordinator.consumption_count().expect("lock"), 1);
    assert_eq!(harness.coordinator.operation_count().expect("lock"), 0);
    assert_eq!(marks_of(&harness), vec![1]);
    assert_eq!(
        recorded_args_of(&harness),
        vec![
            vec![Value::Int(10)],
            vec![Value::Int(10)],
            vec![Value::Int(10)]
        ]
    );
}

#[test]
fn two_sleeps_have_distinct_ops_and_each_submit_once() {
    let mut harness = harness_from_source(fixture("two_sleeps.rss"));
    let mut statuses = Vec::new();

    let first = harness.vm.run().expect("first yield");
    statuses.push(first);
    assert_eq!(first, VmStatus::Yielded);
    let ip1 = harness.vm.ip();
    assert_eq!(harness.coordinator.submission_count().expect("lock"), 1);
    assert!(marks_of(&harness).is_empty());
    let (id1, ms1) = claim_one(&harness.coordinator);
    assert_eq!(ms1, 1);

    harness.coordinator.complete(id1).expect("complete 1");
    let second = harness.vm.resume().expect("second yield");
    statuses.push(second);
    assert_eq!(second, VmStatus::Yielded);
    let ip2 = harness.vm.ip();
    assert_ne!(ip1, ip2, "second sleep must be a later call IP");
    assert_eq!(harness.coordinator.submission_count().expect("lock"), 2);
    assert_eq!(harness.coordinator.consumption_count().expect("lock"), 1);
    assert!(marks_of(&harness).is_empty());
    let (id2, ms2) = claim_one(&harness.coordinator);
    assert_eq!(ms2, 2);
    assert_ne!(
        id1, id2,
        "sequential sleeps must use distinct completion tokens"
    );
    assert_ne!(id1.op_id(), id2.op_id());

    harness.coordinator.complete(id2).expect("complete 2");
    let halted = harness.vm.resume().expect("halt");
    statuses.push(halted);
    println!("two_sleeps status sequence: {statuses:?}");
    assert_eq!(
        statuses,
        vec![VmStatus::Yielded, VmStatus::Yielded, VmStatus::Halted]
    );
    assert_eq!(harness.coordinator.submission_count().expect("lock"), 2);
    assert_eq!(harness.coordinator.consumption_count().expect("lock"), 2);
    assert_eq!(harness.coordinator.operation_count().expect("lock"), 0);
    assert_eq!(marks_of(&harness), vec![3]);
}

#[test]
fn two_concurrent_vms_sharing_coordinator_stay_isolated() {
    let coordinator = SleepCoordinator::new();
    let mut left = harness_from_source_with(
        "use ngx;\nngx::sleep(10);\nngx::mark(1);\n",
        coordinator.clone(),
    );
    let mut right = harness_from_source_with(
        "use ngx;\nngx::sleep(20);\nngx::mark(2);\n",
        coordinator.clone(),
    );

    assert_eq!(left.vm.run().expect("left yield"), VmStatus::Yielded);
    let left_ip = left.vm.ip();
    assert_eq!(right.vm.run().expect("right yield"), VmStatus::Yielded);
    let right_ip = right.vm.ip();
    assert_eq!(coordinator.operation_count().expect("lock"), 2);
    assert_eq!(coordinator.submission_count().expect("lock"), 2);

    let (token_a, ms_a) = claim_one(&coordinator);
    let (token_b, ms_b) = claim_one(&coordinator);
    assert_ne!(token_a, token_b);
    assert_ne!(token_a.generation(), token_b.generation());
    let (left_token, left_ms, right_token, right_ms) = if ms_a == 10 && ms_b == 20 {
        (token_a, ms_a, token_b, ms_b)
    } else if ms_a == 20 && ms_b == 10 {
        (token_b, ms_b, token_a, ms_a)
    } else {
        panic!("expected durations 10 and 20, got {ms_a} and {ms_b}");
    };
    assert_eq!(left_ms, 10);
    assert_eq!(right_ms, 20);

    coordinator.complete(right_token).expect("complete right");
    assert_eq!(
        left.vm.resume().expect("left still waiting"),
        VmStatus::Yielded
    );
    assert_eq!(left.vm.ip(), left_ip);
    assert!(marks_of(&left).is_empty());
    assert_eq!(right.vm.resume().expect("right halt"), VmStatus::Halted);
    assert_ne!(right.vm.ip(), right_ip);
    assert_eq!(marks_of(&right), vec![2]);
    assert_eq!(coordinator.operation_count().expect("lock"), 1);

    coordinator.complete(left_token).expect("complete left");
    assert_eq!(left.vm.resume().expect("left halt"), VmStatus::Halted);
    assert_eq!(marks_of(&left), vec![1]);
    assert_eq!(coordinator.operation_count().expect("lock"), 0);
}

#[test]
fn dropping_one_session_invalidates_late_complete_and_clears_ops() {
    let coordinator = SleepCoordinator::new();
    let mut left = harness_from_source_with(
        "use ngx;\nngx::sleep(3);\nngx::mark(1);\n",
        coordinator.clone(),
    );
    let mut right = harness_from_source_with(
        "use ngx;\nngx::sleep(4);\nngx::mark(2);\n",
        coordinator.clone(),
    );
    assert_eq!(left.vm.run().expect("left yield"), VmStatus::Yielded);
    assert_eq!(right.vm.run().expect("right yield"), VmStatus::Yielded);
    let (token_a, ms_a) = claim_one(&coordinator);
    let (token_b, ms_b) = claim_one(&coordinator);
    let (dropped_token, kept_token) = if ms_a == 3 && ms_b == 4 {
        (token_a, token_b)
    } else if ms_a == 4 && ms_b == 3 {
        (token_b, token_a)
    } else {
        panic!("expected durations 3 and 4, got {ms_a} and {ms_b}");
    };

    drop(left);
    assert_eq!(coordinator.operation_count().expect("lock"), 1);
    let err = coordinator
        .complete(dropped_token)
        .expect_err("late complete after drop");
    assert!(
        matches!(err, SleepOpsError::GenerationInvalid(token) if token == dropped_token),
        "got {err:?}"
    );

    coordinator.complete(kept_token).expect("kept complete");
    assert_eq!(right.vm.resume().expect("right halt"), VmStatus::Halted);
    assert_eq!(marks_of(&right), vec![2]);
    assert_eq!(coordinator.operation_count().expect("lock"), 0);
}

#[test]
fn one_time_submission_claim_supplies_duration_without_vm_stack() {
    let mut harness = harness_from_source(fixture("sleep.rss"));
    assert_eq!(harness.vm.run().expect("yield"), VmStatus::Yielded);
    let stack_before = harness.vm.stack().to_vec();
    let (token, ms) = claim_one(&harness.coordinator);
    assert_eq!(ms, 10);
    assert_eq!(
        harness.vm.stack(),
        stack_before.as_slice(),
        "claim must not read or mutate the VM stack"
    );
    assert!(
        harness
            .coordinator
            .claim_submission()
            .expect("lock")
            .is_none()
    );
    harness.coordinator.complete(token).expect("complete");
    assert_eq!(harness.vm.resume().expect("halt"), VmStatus::Halted);
}

#[test]
fn operation_count_returns_to_zero_after_consume_and_drop() {
    let coordinator = SleepCoordinator::new();
    {
        let mut harness = harness_from_source_with(fixture("sleep.rss"), coordinator.clone());
        assert_eq!(harness.vm.run().expect("yield"), VmStatus::Yielded);
        assert_eq!(coordinator.operation_count().expect("lock"), 1);
        let (token, _) = claim_one(&coordinator);
        coordinator.complete(token).expect("complete");
        assert_eq!(harness.vm.resume().expect("halt"), VmStatus::Halted);
        assert_eq!(coordinator.operation_count().expect("lock"), 0);
        assert_eq!(coordinator.consumption_count().expect("lock"), 1);
    }
    assert_eq!(coordinator.operation_count().expect("lock"), 0);

    {
        let mut harness = harness_from_source_with(fixture("sleep.rss"), coordinator.clone());
        assert_eq!(harness.vm.run().expect("yield"), VmStatus::Yielded);
        assert_eq!(coordinator.operation_count().expect("lock"), 1);
        drop(harness);
        assert_eq!(coordinator.operation_count().expect("lock"), 0);
        assert!(
            coordinator.claim_submission().expect("lock").is_none(),
            "drop must cancel unclaimed submissions"
        );
    }
}

#[test]
fn invalid_arity_is_rejected_at_compile_or_runtime() {
    let err = match compile_source_with_flavor_and_options(
        "use ngx;\nngx::sleep();\n",
        SourceFlavor::RustScript,
        CompileSourceFileOptions::default().with_host_api_catalog(Arc::new(test_catalog())),
    ) {
        Ok(_) => panic!("zero-arg ngx::sleep should fail"),
        Err(err) => err,
    };
    let msg = err.to_string().to_ascii_lowercase();
    assert!(
        msg.contains("sleep") || msg.contains("argument") || msg.contains("arity"),
        "arity error should name sleep/argument, got {err}"
    );

    let err = match compile_source_with_flavor_and_options(
        "use ngx;\nngx::sleep(1, 2);\n",
        SourceFlavor::RustScript,
        CompileSourceFileOptions::default().with_host_api_catalog(Arc::new(test_catalog())),
    ) {
        Ok(_) => panic!("two-arg ngx::sleep should fail"),
        Err(err) => err,
    };
    let msg = err.to_string().to_ascii_lowercase();
    assert!(
        msg.contains("sleep") || msg.contains("argument") || msg.contains("arity"),
        "arity error should name sleep/argument, got {err}"
    );
}

#[test]
fn invalid_type_is_rejected_at_compile_or_runtime() {
    let err = match compile_source_with_flavor_and_options(
        "use ngx;\nngx::sleep(\"later\");\n",
        SourceFlavor::RustScript,
        CompileSourceFileOptions::default().with_host_api_catalog(Arc::new(test_catalog())),
    ) {
        Ok(_) => panic!("string ngx::sleep should fail"),
        Err(err) => err,
    };
    let msg = err.to_string().to_ascii_lowercase();
    assert!(
        msg.contains("sleep") || msg.contains("int") || msg.contains("type"),
        "type error should name sleep/int, got {err}"
    );
}

#[test]
fn runtime_binding_rejects_arity_type_and_range_without_submit() {
    let coordinator = SleepCoordinator::new();
    let mut session = SleepSession::new(&coordinator).expect("session");

    let err = session.call(&[]).expect_err("zero args");
    match err {
        VmError::HostError(msg) => {
            let lower = msg.to_ascii_lowercase();
            assert!(
                lower.contains("exactly one") || lower.contains("argument"),
                "got {msg}"
            );
        }
        other => panic!("expected HostError, got {other:?}"),
    }

    let err = session
        .call(&[Value::Int(1), Value::Int(2)])
        .expect_err("two args");
    match err {
        VmError::HostError(msg) => {
            let lower = msg.to_ascii_lowercase();
            assert!(
                lower.contains("exactly one") || lower.contains("argument"),
                "got {msg}"
            );
        }
        other => panic!("expected HostError, got {other:?}"),
    }

    let err = session.call(&[Value::Bool(true)]).expect_err("bool arg");
    match err {
        VmError::HostError(msg) => {
            let lower = msg.to_ascii_lowercase();
            assert!(
                lower.contains("integer") || lower.contains("int"),
                "got {msg}"
            );
        }
        other => panic!("expected HostError, got {other:?}"),
    }

    let err = session.call(&[Value::Int(-1)]).expect_err("negative");
    match err {
        VmError::HostError(msg) => {
            let lower = msg.to_ascii_lowercase();
            assert!(
                lower.contains("non-negative")
                    || lower.contains("negative")
                    || lower.contains("milliseconds"),
                "got {msg}"
            );
        }
        other => panic!("expected HostError, got {other:?}"),
    }

    assert_eq!(coordinator.submission_count().expect("lock"), 0);
    assert_eq!(coordinator.operation_count().expect("lock"), 0);
    assert_eq!(coordinator.host_entry_count().expect("lock"), 4);
}

#[test]
fn negative_milliseconds_fail_closed_without_submit() {
    let mut harness = harness_from_source("use ngx;\nngx::sleep(-1);\nngx::mark(1);\n");
    let err = harness.vm.run().expect_err("negative sleep");
    let msg = err.to_string().to_ascii_lowercase();
    assert!(
        msg.contains("non-negative") || msg.contains("negative") || msg.contains("milliseconds"),
        "range error should name milliseconds, got {err}"
    );
    assert_eq!(harness.coordinator.submission_count().expect("lock"), 0);
    assert_eq!(harness.coordinator.host_entry_count().expect("lock"), 1);
    assert!(marks_of(&harness).is_empty());
}

#[test]
fn duplicate_completion_fails_closed() {
    let mut harness = harness_from_source(fixture("sleep.rss"));
    assert_eq!(harness.vm.run().expect("yield"), VmStatus::Yielded);
    let (token, _) = claim_one(&harness.coordinator);
    harness.coordinator.complete(token).expect("first complete");
    let err = harness
        .coordinator
        .complete(token)
        .expect_err("duplicate complete");
    assert!(
        matches!(err, SleepOpsError::AlreadyCompleted(id) if id == token),
        "got {err:?}"
    );
    assert_eq!(harness.vm.resume().expect("halt"), VmStatus::Halted);
    assert_eq!(harness.coordinator.submission_count().expect("lock"), 1);
    assert_eq!(harness.coordinator.consumption_count().expect("lock"), 1);
}

#[test]
fn unknown_completion_fails_closed() {
    let mut harness = harness_from_source(fixture("sleep.rss"));
    assert_eq!(harness.vm.run().expect("yield"), VmStatus::Yielded);
    let unknown = SleepToken::from_raw(0xDEAD, 0xBEEF);
    let err = harness
        .coordinator
        .complete(unknown)
        .expect_err("unknown complete");
    assert!(
        matches!(err, SleepOpsError::UnknownOperation(token) if token == unknown),
        "got {err:?}"
    );
    assert_eq!(harness.coordinator.submission_count().expect("lock"), 1);
    assert_eq!(harness.coordinator.consumption_count().expect("lock"), 0);
}

#[test]
fn consume_unknown_and_not_completed_are_distinct() {
    let coordinator = SleepCoordinator::new();
    let mut idle = SleepSession::new(&coordinator).expect("idle session");
    let err = idle.consume_current().expect_err("unknown consume");
    assert!(
        matches!(err, SleepOpsError::UnknownOperation(_)),
        "got {err:?}"
    );

    let mut session = SleepSession::new(&coordinator).expect("session");
    assert!(matches!(
        session.call(&[Value::Int(5)]).expect("yield"),
        CallOutcome::Yield
    ));
    let err = session
        .consume_current()
        .expect_err("consume before complete");
    assert!(matches!(err, SleepOpsError::NotCompleted(_)), "got {err:?}");
    assert_eq!(coordinator.operation_count().expect("lock"), 1);
    let (token, ms) = claim_one(&coordinator);
    assert_eq!(ms, 5);
    coordinator.complete(token).expect("complete");
    session.consume_current().expect("host consume");
    assert_eq!(coordinator.operation_count().expect("lock"), 0);
}

#[test]
fn coordinator_does_not_expose_consume() {
    fn assert_complete_only(_: fn(&SleepCoordinator, SleepToken) -> Result<(), SleepOpsError>) {}
    assert_complete_only(SleepCoordinator::complete);
}

#[test]
fn operation_id_overflow_fails_closed_without_submit() {
    let coordinator = SleepCoordinator::new();
    coordinator
        .__test_set_next_op_id(u64::MAX)
        .expect("set next op id");
    let mut session = SleepSession::new(&coordinator).expect("session");
    let err = session.call(&[Value::Int(1)]).expect_err("exhausted op id");
    match err {
        VmError::HostError(msg) => {
            let lower = msg.to_ascii_lowercase();
            assert!(
                lower.contains("exhaust") || lower.contains("operation"),
                "got {msg}"
            );
        }
        other => panic!("expected HostError, got {other:?}"),
    }
    assert_eq!(coordinator.submission_count().expect("lock"), 0);
    assert_eq!(coordinator.operation_count().expect("lock"), 0);
}

#[test]
fn generation_overflow_fails_closed_without_session() {
    let coordinator = SleepCoordinator::new();
    coordinator
        .__test_set_next_generation(u64::MAX)
        .expect("set next generation");
    let err = match SleepSession::new(&coordinator) {
        Err(err) => err,
        Ok(_) => panic!("exhausted generation should not allocate a session"),
    };
    assert!(
        matches!(err, SleepOpsError::OperationIdExhausted),
        "got {err:?}"
    );
    assert_eq!(coordinator.operation_count().expect("lock"), 0);
}

#[test]
fn mutex_poison_returns_error_and_does_not_panic() {
    let coordinator = SleepCoordinator::new();
    let session = SleepSession::new(&coordinator).expect("session");
    coordinator.__test_poison();
    let err = coordinator
        .complete(SleepToken::from_raw(1, 1))
        .expect_err("poison complete");
    assert!(matches!(err, SleepOpsError::Poisoned), "got {err:?}");
    let err = coordinator.claim_submission().expect_err("poison claim");
    assert!(matches!(err, SleepOpsError::Poisoned), "got {err:?}");
    let err = coordinator.operation_count().expect_err("poison count");
    assert!(matches!(err, SleepOpsError::Poisoned), "got {err:?}");
    let mut session = session;
    let err = session.call(&[Value::Int(1)]).expect_err("poison host");
    match err {
        VmError::HostError(_) => {}
        other => panic!("expected HostError, got {other:?}"),
    }
    drop(session);
}

#[test]
fn config_compile_path_still_rejects_ngx_sleep_imports() {
    let err = ngx_rustscript::config::compile_rss_source(include_str!("fixtures/ngx_sleep.rss"))
        .expect_err("config path must still reject ngx::sleep");
    match err {
        ngx_rustscript::config::ConfigError::Compile(msg)
        | ngx_rustscript::config::ConfigError::UnsupportedImport(msg) => {
            let lower = msg.to_ascii_lowercase();
            assert!(
                lower.contains("ngx") || lower.contains("sleep"),
                "error should name ngx::sleep, got {msg}"
            );
        }
        other => panic!("expected compile or unsupported import, got {other:?}"),
    }
}

#[test]
fn bind_ngx_sleep_uses_per_vm_session() {
    let coordinator = SleepCoordinator::new();
    let compiled = compile_ngx(fixture("sleep_null.rss"));
    let mut vm = Vm::new(compiled.program);
    bind_ngx_sleep(&mut vm, &coordinator).expect("bind");
    assert_eq!(vm.run().expect("yield"), VmStatus::Yielded);
    let (token, ms) = claim_one(&coordinator);
    assert_eq!(ms, 10);
    coordinator.complete(token).expect("complete");
    assert_eq!(vm.resume().expect("halt"), VmStatus::Halted);
}
