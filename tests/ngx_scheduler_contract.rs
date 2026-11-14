//! Fail-closed scheduler / callable-family gate.
//!
//! Light threads, `on_abort` callables, and `run_worker_thread` stay absent
//! from the production surface. Two worker-owned families are landed:
//!
//! - `ngx::semaphore::{new,wait,post,count}` — callable-free;
//! - `ngx::timer::{at,every,pending_count,running_count}` — registration hosts
//!   consume their callback through `HostParamPassing::TakeOwned`.
//!
//! Both families bind through [`ngx_host_registry`]. Bare namespace markers
//! (`ngx::semaphore`, `ngx::timer`) stay unregistered. Configuration compile
//! rejects every remaining candidate in Access, Content, and Log. Do not
//! register the callable families without revisiting `docs/design/ngx-scheduler.md`.

use ngx_rustscript_nginx_test_stubs as _;

use std::collections::HashSet;
use std::path::Path;

use ngx_rustscript::config::{
    ConfigError, compile_rss_source, compile_rss_source_for_phase,
    validate_program_imports_for_phase,
};
use ngx_rustscript::host::{
    HostCapability, HostFamily, NgxPhase, bind_ngx_hosts, ngx_host_catalog, ngx_host_manifest,
    ngx_host_registry, production_runtime_host_names,
};
use ngx_rustscript::notify::{CompletionSender, NgxWake, RequestIdentity};
use vm::{
    CompileSourceFileOptions, HostTypeSchema, Program, SourceFlavor, Vm, compile_source,
    compile_source_with_flavor_and_options,
};

/// Exact candidate import list for the still-gated scheduler families.
///
/// The landed semaphore and timer callables are deliberately absent; their four
/// and two bare namespace markers stay on this list.
const SCHEDULER_CANDIDATES: &[&str] = &[
    "ngx::thread::spawn",
    "ngx::thread::wait",
    "ngx::thread::kill",
    "ngx::on_abort",
    "ngx::semaphore",
    "ngx::timer",
    "ngx::run_worker_thread",
];

/// The landed non-callable family. Exactly four callables, no marker.
const REGISTERED_SEMAPHORE_HOSTS: &[&str] = &[
    "ngx::semaphore::new",
    "ngx::semaphore::wait",
    "ngx::semaphore::post",
    "ngx::semaphore::count",
];

/// The landed callable family. Exactly four callables, no marker.
const REGISTERED_TIMER_HOSTS: &[&str] = &[
    "ngx::timer::at",
    "ngx::timer::every",
    "ngx::timer::pending_count",
    "ngx::timer::running_count",
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
        | HostTypeSchema::Optional(inner)
        | HostTypeSchema::Option(inner) => schema_contains_callable(inner),
        HostTypeSchema::Result { ok, err } => {
            schema_contains_callable(ok) || schema_contains_callable(err)
        }
        HostTypeSchema::Named { fields, .. } => fields
            .iter()
            .any(|field| schema_contains_callable(&field.ty)),
        HostTypeSchema::Unknown
        | HostTypeSchema::Null
        | HostTypeSchema::Unit
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
/// revisited. `Semaphore` is the reviewed, callable-free addition.
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
/// revisited. `Semaphore` is the reviewed, callable-free addition.
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
fn production_host_count_reaches_199_with_the_timer_family() {
    assert_eq!(ngx_host_manifest().len(), 199);
    assert_eq!(ngx_host_catalog().functions().len(), 199);
    assert_eq!(
        production_runtime_host_names().len(),
        199,
        "the production name list is the full catalog including timers"
    );
    assert_eq!(
        ngx_host_manifest().len(),
        191 + REGISTERED_SEMAPHORE_HOSTS.len() + REGISTERED_TIMER_HOSTS.len(),
        "the only additions over the 191 callable-family baseline are the \
         semaphore and timer families"
    );
    let manifest: HashSet<&str> = ngx_host_manifest().iter().map(|entry| entry.name).collect();
    let runtime: HashSet<&str> = production_runtime_host_names().iter().copied().collect();
    assert_eq!(
        runtime, manifest,
        "production runtime names must equal the manifest"
    );
}

/// Positional (ordered) parity of the production name sources.
///
/// `ngx_host_manifest`, `ngx_host_catalog`, and
/// `production_runtime_host_names` are compared entry-for-entry. The
/// restricted descriptor registry contains every catalogued name, including
/// the timer family. A swapped or transcribed entry fails the comparison.
#[test]
fn manifest_catalog_bindings_and_runtime_names_are_ordered_and_partitioned() {
    let manifest: Vec<&str> = ngx_host_manifest().iter().map(|entry| entry.name).collect();
    let catalog_owned = ngx_host_catalog();
    let catalog: Vec<&str> = catalog_owned
        .functions()
        .iter()
        .map(|schema| schema.name.as_str())
        .collect();
    assert_eq!(
        catalog, manifest,
        "the compiler catalog must follow the manifest order exactly"
    );
    assert_eq!(
        production_runtime_host_names(),
        manifest.as_slice(),
        "the documented runtime name list must repeat the catalog order"
    );

    let registry = ngx_host_registry().expect("production host registry");
    for name in &manifest {
        assert!(
            registry.contains_name(name),
            "{name} must resolve through ngx_host_registry"
        );
    }
    for name in REGISTERED_TIMER_HOSTS {
        assert!(
            registry.contains_name(name),
            "{name} must resolve through the same production registry"
        );
    }

    let mut swapped_catalog = catalog.clone();
    swapped_catalog.swap(0, 1);
    assert_ne!(
        swapped_catalog, manifest,
        "a swapped catalog entry must fail the ordered parity check"
    );
    let mut swapped_runtime = production_runtime_host_names().to_vec();
    swapped_runtime.swap(0, 1);
    assert_ne!(
        swapped_runtime.as_slice(),
        manifest.as_slice(),
        "a swapped runtime name must fail against the manifest"
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
fn timer_family_is_registered_without_the_namespace_marker() {
    let catalog = ngx_host_catalog();
    let manifest: HashSet<&str> = ngx_host_manifest().iter().map(|entry| entry.name).collect();
    for name in REGISTERED_TIMER_HOSTS {
        assert!(
            catalog.function(name).is_some(),
            "{name} must be in the compiler catalog"
        );
        assert!(manifest.contains(name), "{name} must be in the manifest");
    }
    assert!(
        catalog.function("ngx::timer").is_none(),
        "the bare ngx::timer namespace marker must not be a callable"
    );
    let source = "use ngx;\nngx::timer::at(0.05, |premature| null);\n";
    for phase in [NgxPhase::Access, NgxPhase::Content] {
        compile_rss_source_for_phase(source, phase)
            .unwrap_or_else(|err| panic!("{phase:?} must accept the timer family: {err}"));
    }
}

#[test]
fn timer_family_fails_configuration_compile_in_log() {
    let phase = NgxPhase::Log;
    for source in [
        "use ngx;\nngx::timer::at(0.05, |premature| null);\n",
        "use ngx;\nngx::timer::every(0.05, |premature| null);\n",
        "use ngx;\nngx::timer::pending_count();\n",
        "use ngx;\nngx::timer::running_count();\n",
    ] {
        let err =
            compile_rss_source_for_phase(source, phase).expect_err("timer hosts must fail in Log");
        match err {
            ConfigError::PhaseForbidden { name, phase: got } => {
                assert!(name.starts_with("ngx::timer::"), "{name}");
                assert_eq!(got, phase);
            }
            other => panic!("Log must fail configuration compile, got {other:?}"),
        }
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

    let md5_program = compile_rss_source("use ngx;\nngx::md5(\"x\");\n").expect("md5 program");
    let mut md5_vm = Vm::new_shared(md5_program);
    bind_ngx_hosts(&mut md5_vm);
    md5_vm
        .run()
        .expect("production bind must execute imported ngx::md5");
    for name in SCHEDULER_CANDIDATES {
        assert!(
            !md5_vm.has_bound_function(name),
            "production bind must not install {name}"
        );
    }
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
fn callable_schema_is_limited_to_timer_registration_hosts() {
    for schema in ngx_host_catalog().functions() {
        let registration_host = REGISTERED_TIMER_HOSTS
            .iter()
            .take(2)
            .any(|name| *name == schema.name);
        assert!(
            !schema_contains_callable(&schema.return_type),
            "{} must not return a callable",
            schema.name
        );
        for param in &schema.params {
            if registration_host && param.name == "callback" {
                assert!(
                    schema_contains_callable(&param.ty),
                    "{} callback must be a callable",
                    schema.name
                );
            } else {
                assert!(
                    !schema_contains_callable(&param.ty),
                    "{} parameter {} must not be a callable",
                    schema.name,
                    param.name
                );
            }
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
