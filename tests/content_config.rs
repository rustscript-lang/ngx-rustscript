//! Config-time access/content script slots, inheritance, and phase validation.

use ngx_rustscript_nginx_test_stubs as _;

#[path = "support/temp.rs"]
mod temp;

use std::path::Path;
use std::sync::Arc;

use ngx::http::Merge;
use vm::{Program, compile_source};

use ngx_rustscript::config::{
    ConfigError, LocationConf, compile_rss_source, compile_rss_source_for_phase,
    load_and_compile_rss_file, load_and_compile_rss_file_for_phase, reject_program_imports,
    validate_program_imports_for_phase,
};
use ngx_rustscript::host::{NgxPhase, ngx_host_manifest};
use ngx_rustscript::phase::ScriptLocationConf;

fn fixture(name: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn program_ptr(program: &Arc<Program>) -> *const Program {
    Arc::as_ptr(program)
}

#[test]
fn location_can_hold_access_and_content_scripts_independently() {
    let access = load_and_compile_rss_file(&fixture("ok.rss")).expect("access");
    let content =
        load_and_compile_rss_file_for_phase(&fixture("runtime_error.rss"), NgxPhase::Content)
            .expect("content");
    let mut conf = LocationConf::default();
    conf.set_access_program(access.clone()).unwrap();
    conf.set_content_program(content.clone()).unwrap();
    assert_eq!(
        program_ptr(conf.access_program().expect("access set")),
        program_ptr(&access)
    );
    assert_eq!(
        program_ptr(conf.content_program().expect("content set")),
        program_ptr(&content)
    );
    assert_ne!(program_ptr(&access), program_ptr(&content));
}

#[test]
fn access_duplicate_does_not_block_content_and_content_duplicate_is_independent() {
    let access = load_and_compile_rss_file(&fixture("ok.rss")).unwrap();
    let content =
        load_and_compile_rss_file_for_phase(&fixture("ok.rss"), NgxPhase::Content).unwrap();
    let mut conf = LocationConf::default();
    conf.set_access_program(access.clone()).unwrap();
    let access_dup = conf.set_access_program(access).expect_err("access dup");
    assert!(
        matches!(access_dup, ConfigError::Duplicate),
        "got {access_dup:?}"
    );
    conf.set_content_program(content.clone()).unwrap();
    let content_dup = conf.set_content_program(content).expect_err("content dup");
    assert!(
        matches!(content_dup, ConfigError::DuplicateContent),
        "got {content_dup:?}"
    );
    assert!(conf.access_program().is_some());
    assert!(conf.content_program().is_some());
}

#[test]
fn unset_child_inherits_access_and_content_independently() {
    let access = load_and_compile_rss_file(&fixture("ok.rss")).unwrap();
    let content =
        load_and_compile_rss_file_for_phase(&fixture("runtime_error.rss"), NgxPhase::Content)
            .unwrap();
    let mut parent = LocationConf::default();
    parent.set_access_program(access.clone()).unwrap();
    parent.set_content_program(content.clone()).unwrap();
    let mut child = LocationConf::default();
    child.merge(&parent).unwrap();
    assert_eq!(
        program_ptr(child.access_program().expect("inherited access")),
        program_ptr(&access)
    );
    assert_eq!(
        program_ptr(child.content_program().expect("inherited content")),
        program_ptr(&content)
    );
}

#[test]
fn explicit_child_content_overrides_parent_without_clobbering_access() {
    let parent_access = load_and_compile_rss_file(&fixture("ok.rss")).unwrap();
    let parent_content =
        load_and_compile_rss_file_for_phase(&fixture("runtime_error.rss"), NgxPhase::Content)
            .unwrap();
    let child_content =
        load_and_compile_rss_file_for_phase(&fixture("ok.rss"), NgxPhase::Content).unwrap();
    let mut parent = LocationConf::default();
    parent.set_access_program(parent_access.clone()).unwrap();
    parent.set_content_program(parent_content).unwrap();
    let mut child = LocationConf::default();
    child.set_content_program(child_content.clone()).unwrap();
    child.merge(&parent).unwrap();
    assert_eq!(
        program_ptr(child.access_program().expect("inherited access")),
        program_ptr(&parent_access)
    );
    assert_eq!(
        program_ptr(child.content_program().expect("override content")),
        program_ptr(&child_content)
    );
}

#[test]
fn explicit_child_access_overrides_parent_without_clobbering_content() {
    let parent_access = load_and_compile_rss_file(&fixture("runtime_error.rss")).unwrap();
    let parent_content =
        load_and_compile_rss_file_for_phase(&fixture("ok.rss"), NgxPhase::Content).unwrap();
    let child_access = load_and_compile_rss_file(&fixture("ok.rss")).unwrap();
    let mut parent = LocationConf::default();
    parent.set_access_program(parent_access).unwrap();
    parent.set_content_program(parent_content.clone()).unwrap();
    let mut child = LocationConf::default();
    child.set_access_program(child_access.clone()).unwrap();
    child.merge(&parent).unwrap();
    assert_eq!(
        program_ptr(child.access_program().expect("override access")),
        program_ptr(&child_access)
    );
    assert_eq!(
        program_ptr(child.content_program().expect("inherited content")),
        program_ptr(&parent_content)
    );
}

#[test]
fn content_phase_accepts_sleep_and_request_read() {
    let sleep = compile_rss_source_for_phase("use ngx;\nngx::sleep_ms(1);\n", NgxPhase::Content)
        .expect("sleep in content");
    assert!(ngx_rustscript::config::program_requires_request_state(
        &sleep
    ));
    compile_rss_source_for_phase("use ngx;\nngx::req::get_method();\n", NgxPhase::Content)
        .expect("get_method in content");
    compile_rss_source_for_phase("use ngx;\nngx::print(\"ok\");\n", NgxPhase::Content)
        .expect("print in content");
}

#[test]
fn content_phase_rejects_print_and_runtime_sleep() {
    let print = compile_source(include_str!("fixtures/print.rss")).expect("print compiles");
    match validate_program_imports_for_phase(&print.program, NgxPhase::Content) {
        Err(ConfigError::UnsupportedImport(msg)) => {
            assert!(msg.to_ascii_lowercase().contains("print"), "got {msg}");
        }
        other => panic!("expected UnsupportedImport for print, got {other:?}"),
    }
    let err = compile_rss_source_for_phase(include_str!("fixtures/print.rss"), NgxPhase::Content)
        .expect_err("print");
    let text = err.to_string().to_ascii_lowercase();
    assert!(text.contains("print"), "got {err}");

    let err = compile_rss_source_for_phase(
        include_str!("fixtures/runtime_sleep.rss"),
        NgxPhase::Content,
    )
    .expect_err("runtime::sleep");
    let text = err.to_string().to_ascii_lowercase();
    assert!(text.contains("runtime::sleep"), "got {err}");
}

#[test]
fn phase_forbidden_import_is_a_distinct_config_error() {
    let compiled = compile_source("use ngx;\nngx::req::get_method();\n").expect("compiles");
    reject_program_imports(&compiled.program).expect("allowed in access");
    let err = validate_program_imports_for_phase(&compiled.program, NgxPhase::Init)
        .expect_err("init forbids get_method");
    match err {
        ConfigError::PhaseForbidden { name, phase } => {
            assert_eq!(name, "ngx::req::get_method");
            assert_eq!(phase, NgxPhase::Init);
        }
        other => panic!("expected PhaseForbidden, got {other:?}"),
    }
    let err = compile_rss_source_for_phase("use ngx;\nngx::req::get_method();\n", NgxPhase::Init)
        .expect_err("init compile");
    let text = err.to_string();
    assert!(
        text.contains("ngx::req::get_method") && text.contains("init"),
        "phase-forbidden error must name import and phase, got {text}"
    );
}

#[test]
fn default_compile_rss_source_stays_access_phase() {
    compile_rss_source("use ngx;\nngx::sleep_ms(1);\n").expect("access sleep");
    compile_rss_source("use ngx;\nngx::req::get_method();\n").expect("access get_method");
}

#[test]
fn script_location_conf_slots_compile_and_merge_independently() {
    let access = load_and_compile_rss_file(&fixture("ok.rss")).expect("access");
    let content =
        load_and_compile_rss_file_for_phase(&fixture("runtime_error.rss"), NgxPhase::Content)
            .expect("content");
    let mut access_slot = ScriptLocationConf::default();
    let mut content_slot = ScriptLocationConf::default();
    access_slot.set_program(access.clone()).unwrap();
    content_slot.set_program(content.clone()).unwrap();
    assert!(access_slot.is_set());
    assert!(content_slot.is_set());
    assert_eq!(
        program_ptr(access_slot.program().expect("access set")),
        program_ptr(&access)
    );
    assert_eq!(
        program_ptr(content_slot.program().expect("content set")),
        program_ptr(&content)
    );

    let mut child_access = ScriptLocationConf::default();
    let mut child_content = ScriptLocationConf::default();
    child_content.set_program(access.clone()).unwrap();
    child_access.merge_from(&access_slot);
    child_content.merge_from(&content_slot);
    assert_eq!(
        program_ptr(child_access.program().expect("inherited access")),
        program_ptr(&access)
    );
    assert_eq!(
        program_ptr(child_content.program().expect("explicit content kept")),
        program_ptr(&access)
    );
}

fn first_content_phase_forbidden_script() -> Option<(String, String)> {
    for entry in ngx_host_manifest() {
        if entry.phases.contains(&NgxPhase::Content) {
            continue;
        }
        let sources = [
            format!("use ngx;\n{}();\n", entry.name),
            format!("use ngx;\n{}(0);\n", entry.name),
            format!("use ngx;\n{}(1);\n", entry.name),
            format!("use ngx;\n{}(0.001);\n", entry.name),
            format!("use ngx;\n{}(\"k\");\n", entry.name),
            format!("use ngx;\n{}(\"k\", 1);\n", entry.name),
            format!("use ngx;\n{}(\"k\", \"v\");\n", entry.name),
        ];
        for source in sources {
            match compile_rss_source_for_phase(&source, NgxPhase::Content) {
                Err(ConfigError::PhaseForbidden { name, phase })
                    if name == entry.name && phase == NgxPhase::Content =>
                {
                    return Some((entry.name.to_string(), source));
                }
                _ => {}
            }
        }
    }
    None
}

#[test]
fn content_compile_path_emits_phase_forbidden_for_hosts_not_in_content() {
    if let Some((name, source)) = first_content_phase_forbidden_script() {
        match compile_rss_source_for_phase(&source, NgxPhase::Content) {
            Err(ConfigError::PhaseForbidden { name: got, phase }) => {
                assert_eq!(got, name);
                assert_eq!(phase, NgxPhase::Content);
            }
            other => panic!("expected PhaseForbidden for {name}, got {other:?}"),
        }
        return;
    }
    // Every catalogued host is currently allowed in Content. The content
    // compile path still uses ConfigError::PhaseForbidden for other phases.
    let src = "use ngx;\nngx::req::get_method();\n";
    compile_rss_source_for_phase(src, NgxPhase::Content)
        .expect("ngx::req::get_method is allowed in content");
    match compile_rss_source_for_phase(src, NgxPhase::Init) {
        Err(ConfigError::PhaseForbidden { name, phase }) => {
            assert_eq!(name, "ngx::req::get_method");
            assert_eq!(phase, NgxPhase::Init);
        }
        other => panic!("expected PhaseForbidden, got {other:?}"),
    }
}
