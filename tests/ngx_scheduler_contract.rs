//! Fail-closed scheduler / callable-family gate.
//!
//! Light threads, timer callbacks, and `on_abort` callables, plus
//! `run_worker_thread`, stay absent from the production surface. The
//! worker-owned semaphore family (`ngx::semaphore::{new,wait,post,count}`) is
//! the only landed scheduler-adjacent family: it needs no script callable and
//! keeps the bare `ngx::semaphore` namespace marker unregistered.
//! Configuration compile rejects every remaining candidate in Access, Content,
//! and Log. Do not register the callable families without revisiting
//! `docs/design/ngx-scheduler.md`.

use ngx_rustscript_nginx_test_stubs as _;

use std::collections::HashSet;
use std::path::Path;

use ngx_rustscript::config::{
    ConfigError, compile_rss_source_for_phase, validate_program_imports_for_phase,
};
use ngx_rustscript::host::{
    HostCapability, HostFamily, NgxPhase, bind_ngx_hosts, ngx_host_catalog, ngx_host_manifest,
    production_runtime_host_names,
};
use ngx_rustscript::notify::{CompletionSender, NgxWake, RequestIdentity};
use vm::{
    CompileSourceFileOptions, HostTypeSchema, Program, SourceFlavor, Vm, compile_source,
    compile_source_with_flavor_and_options,
};

/// Exact candidate import list for the still-gated scheduler families.
///
/// The four registered semaphore callables are deliberately absent: they
/// landed with the worker-owned semaphore family. The bare `ngx::semaphore`
/// namespace marker must **not** be registered, so it stays on this list.
const SCHEDULER_CANDIDATES: &[&str] = &[
    "ngx::thread::spawn",
    "ngx::thread::wait",
    "ngx::thread::kill",
    "ngx::timer::at",
    "ngx::timer::every",
    "ngx::timer::pending_count",
    "ngx::timer::running_count",
    "ngx::on_abort",
    "ngx::semaphore",
    "ngx::run_worker_thread",
];

/// The landed non-callable family. Exactly four callables, no marker.
const REGISTERED_SEMAPHORE_HOSTS: &[&str] = &[
    "ngx::semaphore::new",
    "ngx::semaphore::wait",
    "ngx::semaphore::post",
    "ngx::semaphore::count",
];

const CONFIG_PHASES: [NgxPhase; 3] = [NgxPhase::Access, NgxPhase::Content, NgxPhase::Log];

fn candidate_source(name: &str, arity: usize) -> String {
    let args = (0..arity)
        .map(|i| i.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    format!("use ngx;\n{name}({args});\n")
}

fn candidate_arity(index: usize) -> usize {
    index % 3
}

fn assert_config_fail_closed(err: ConfigError, name: &str) {
    match err {
        ConfigError::UnsupportedImport(msg) => {
            assert!(
                msg.contains(name),
                "UnsupportedImport must name {name}, got {msg}"
            );
        }
        ConfigError::Compile(msg) => {
            let lower = msg.to_ascii_lowercase();
            let needle = name.to_ascii_lowercase();
            assert!(
                lower.contains(&needle),
                "Compile fail-closed class must name {name}, got {msg}"
            );
        }
        other => panic!(
            "{name}: expected UnsupportedImport or Compile at configuration compile, got {other:?}"
        ),
    }
}

fn schema_contains_callable(schema: &HostTypeSchema) -> bool {
    match schema {
        HostTypeSchema::Callable { .. } => true,
        HostTypeSchema::Array(inner)
        | HostTypeSchema::Map(inner)
        | HostTypeSchema::Optional(inner) => schema_contains_callable(inner),
        HostTypeSchema::Unknown
        | HostTypeSchema::Null
        | HostTypeSchema::Int
        | HostTypeSchema::Float
        | HostTypeSchema::Number
        | HostTypeSchema::Bool
        | HostTypeSchema::String
        | HostTypeSchema::Bytes
        | HostTypeSchema::Resource(_) => false,
    }
}

fn assert_send<T: Send>() {}
fn assert_sync<T: Sync>() {}

/// Adding any other `HostFamily` must fail this match until the design is
/// revisited. `Semaphore` is the one reviewed, callable-free addition.
fn host_family_has_no_callable_scheduler_variant(family: HostFamily) {
    match family {
        HostFamily::Timer
        | HostFamily::Semaphore
        | HostFamily::Codec
        | HostFamily::Time
        | HostFamily::Meta
        | HostFamily::Log
        | HostFamily::Constant
        | HostFamily::Request
        | HostFamily::RequestCtx
        | HostFamily::Response
        | HostFamily::Control
        | HostFamily::SharedDict
        | HostFamily::Cosocket => {}
    }
}

/// Adding any other `HostCapability` must fail this match until the design is
/// revisited. `Semaphore` is the one reviewed, callable-free addition.
fn host_capability_has_no_callable_scheduler_variant(capability: HostCapability) {
    match capability {
        HostCapability::Timer
        | HostCapability::Semaphore
        | HostCapability::Codec
        | HostCapability::Time
        | HostCapability::Meta
        | HostCapability::Log
        | HostCapability::Constant
        | HostCapability::RequestRead
        | HostCapability::RequestWrite
        | HostCapability::RequestCtx
        | HostCapability::ResponseRead
        | HostCapability::ResponseWrite
        | HostCapability::Control
        | HostCapability::SharedDict
        | HostCapability::Cosocket => {}
    }
}

#[test]
fn production_host_count_stays_exactly_195() {
    assert_eq!(ngx_host_manifest().len(), 195);
    assert_eq!(ngx_host_catalog().functions().len(), 195);
    assert_eq!(production_runtime_host_names().len(), 195);
    assert_eq!(
        ngx_host_manifest().len(),
        191 + REGISTERED_SEMAPHORE_HOSTS.len(),
        "the only addition over the 191 callable-family baseline is the semaphore family"
    );
}

#[test]
fn semaphore_family_is_registered_without_the_namespace_marker() {
    let catalog = ngx_host_catalog();
    let manifest: HashSet<&str> = ngx_host_manifest().iter().map(|entry| entry.name).collect();
    let runtime: HashSet<&str> = production_runtime_host_names().iter().copied().collect();
    for name in REGISTERED_SEMAPHORE_HOSTS {
        assert!(
            catalog.function(name).is_some(),
            "{name} must be in the compiler catalog"
        );
        assert!(manifest.contains(name), "{name} must be in the manifest");
        assert!(runtime.contains(name), "{name} must be bound at runtime");
    }
    assert!(
        catalog.function("ngx::semaphore").is_none(),
        "the ngx::semaphore namespace marker must not be a callable"
    );
    let source = "use ngx;\nlet s = ngx::semaphore::new(1);\nngx::semaphore::wait(s, null);\n";
    for phase in [NgxPhase::Access, NgxPhase::Content] {
        compile_rss_source_for_phase(source, phase)
            .unwrap_or_else(|err| panic!("{phase:?} must accept the semaphore family: {err}"));
    }
}

#[test]
fn semaphore_family_fails_configuration_compile_in_log() {
    let phase = NgxPhase::Log;
    for source in [
        "use ngx;\nngx::semaphore::new(1);\n",
        "use ngx;\nlet s = ngx::semaphore::new(1);\nngx::semaphore::count(s);\nngx::semaphore::post(s, 1);\n",
    ] {
        let err = compile_rss_source_for_phase(source, phase)
            .expect_err("semaphore hosts must fail in Log");
        match err {
            ConfigError::PhaseForbidden { name, phase: got } => {
                assert!(name.starts_with("ngx::semaphore::"), "{name}");
                assert_eq!(got, phase);
            }
            other => panic!("Log must fail configuration compile, got {other:?}"),
        }
    }
}

#[test]
fn scheduler_candidates_are_absent_from_manifest_catalog_and_runtime() {
    let catalog = ngx_host_catalog();
    let manifest: HashSet<&str> = ngx_host_manifest().iter().map(|entry| entry.name).collect();
    let runtime: HashSet<&str> = production_runtime_host_names().iter().copied().collect();

    for name in SCHEDULER_CANDIDATES {
        assert!(
            catalog.function(name).is_none(),
            "{name} must not be in the compiler catalog"
        );
        assert!(
            !manifest.contains(name),
            "{name} must not be in the production manifest"
        );
        assert!(
            !runtime.contains(name),
            "{name} must not reach ordinary artifact runtime imports"
        );
    }
}

#[test]
fn scheduler_candidates_are_rejected_at_configuration_compile_in_access_content_log() {
    for (index, name) in SCHEDULER_CANDIDATES.iter().enumerate() {
        let source = candidate_source(name, candidate_arity(index));
        for phase in CONFIG_PHASES {
            let err = compile_rss_source_for_phase(&source, phase).expect_err(
                "scheduler candidate must fail configuration compile, not reach request runtime",
            );
            assert_config_fail_closed(err, name);
        }
    }
}

#[test]
fn scheduler_candidates_do_not_bind_on_a_production_vm() {
    let compiled = compile_source("let x = 1;\n").expect("import-free program");
    let mut vm = Vm::try_new(compiled.program).expect("construct vm");
    bind_ngx_hosts(&mut vm);
    for name in SCHEDULER_CANDIDATES {
        assert!(
            !vm.has_bound_function(name),
            "bind_ngx_hosts must not install {name}"
        );
    }
    assert!(
        vm.has_bound_function("ngx::sleep"),
        "production bind must still install ngx::sleep"
    );
}

#[test]
fn no_scheduler_production_module_is_registered() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    assert!(
        !root.join("src/scheduler.rs").exists(),
        "src/scheduler.rs must not exist while the callable-family gate is fail-closed"
    );
    assert!(
        !root.join("src/host/scheduler.rs").exists(),
        "src/host/scheduler.rs must not exist while the callable-family gate is fail-closed"
    );

    let lib = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"));
    assert!(
        !lib.contains("mod scheduler"),
        "src/lib.rs must not register a scheduler module"
    );

    let host_mod = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/host/mod.rs"));
    assert!(
        !host_mod.contains("mod scheduler"),
        "src/host/mod.rs must not register a scheduler module"
    );
}

#[test]
fn host_family_and_capability_have_no_callable_scheduler_variant() {
    for entry in ngx_host_manifest() {
        host_family_has_no_callable_scheduler_variant(entry.family);
        for capability in entry.capabilities {
            host_capability_has_no_callable_scheduler_variant(*capability);
        }
    }
}

#[test]
fn production_catalog_has_no_callable_host_schema() {
    for schema in ngx_host_catalog().functions() {
        assert!(
            !schema_contains_callable(&schema.return_type),
            "{} must not return a callable until the scheduler contract is implemented",
            schema.name
        );
        for param in &schema.params {
            assert!(
                !schema_contains_callable(&param.ty),
                "{} parameter {} must not be a callable until the scheduler contract is implemented",
                schema.name,
                param.name
            );
        }
    }
}

#[test]
fn owned_scheduler_boundary_types_are_send_sync() {
    assert_send::<vm::Value>();
    assert_sync::<vm::Value>();
    assert_send::<vm::HostFuture>();
    assert_send::<vm::ResourceHandle>();
    assert_send::<std::sync::Arc<Program>>();
    assert_send::<RequestIdentity>();
    assert_sync::<RequestIdentity>();
    assert_send::<CompletionSender>();
    assert_sync::<CompletionSender>();
    assert_send::<NgxWake>();
    assert_sync::<NgxWake>();
}

#[test]
fn catalog_unknown_scheduler_import_is_unsupported_not_phase_forbidden() {
    let catalog = std::sync::Arc::new(ngx_host_catalog());
    for (index, name) in SCHEDULER_CANDIDATES.iter().enumerate() {
        let compiled = compile_source_with_flavor_and_options(
            &candidate_source(name, candidate_arity(index)),
            SourceFlavor::RustScript,
            CompileSourceFileOptions::default().with_host_api_catalog(catalog.clone()),
        )
        .expect("undeclared ngx member is recorded as an import");
        assert!(
            compiled
                .program
                .imports
                .iter()
                .any(|import| import.name == *name),
            "{name} must appear in Program.imports so config can reject it"
        );
        for phase in CONFIG_PHASES {
            match validate_program_imports_for_phase(&compiled.program, phase) {
                Err(ConfigError::UnsupportedImport(msg)) => {
                    assert!(
                        msg.contains(name),
                        "phase {phase:?} UnsupportedImport={msg}"
                    );
                }
                Err(ConfigError::PhaseForbidden {
                    name: forbidden,
                    phase,
                }) => {
                    panic!(
                        "PhaseForbidden would mean {forbidden} was registered in the manifest (phase {phase:?})"
                    );
                }
                other => panic!("expected UnsupportedImport for {name}, got {other:?}"),
            }
        }
    }
}
