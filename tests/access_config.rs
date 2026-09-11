//! Config-time RSS path, file, compile, import, arity-shape, and inheritance tests.

use ngx_rustscript_nginx_test_stubs as _;

#[path = "support/temp.rs"]
mod temp;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

use ngx::core::Status;
use ngx::http::Merge;
use vm::{VmStatus, compile_source};

use ngx_rustscript::access::{run_access_program, status_from_vm_run};
use ngx_rustscript::config::{
    ConfigError, LocationConf, compile_rss_source, load_and_compile_rss_file, load_rss_file,
    reject_program_imports, resolve_rss_path,
};
use temp::unique_temp_dir;

fn fixture(name: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn write_rss(dir: &Path, name: &str, body: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    fs::write(&path, body).unwrap();
    path
}

fn assert_unsupported_import(err: ConfigError, needle: &str) {
    match err {
        ConfigError::UnsupportedImport(msg) => {
            let lower = msg.to_ascii_lowercase();
            assert!(
                lower.contains(&needle.to_ascii_lowercase()),
                "import rejection should name {needle}, got {msg}"
            );
        }
        other => panic!("expected UnsupportedImport containing {needle}, got {other:?}"),
    }
}

fn assert_rejected_host(err: ConfigError, needle: &str) {
    match err {
        ConfigError::UnsupportedImport(msg) | ConfigError::Compile(msg) => {
            let lower = msg.to_ascii_lowercase();
            assert!(
                lower.contains(&needle.to_ascii_lowercase()),
                "host rejection should name {needle}, got {msg}"
            );
        }
        other => panic!("expected UnsupportedImport or Compile containing {needle}, got {other:?}"),
    }
}

#[test]
fn resolve_absolute_path_is_unchanged() {
    let conf_prefix = Path::new("/prefix/conf");
    assert_eq!(
        resolve_rss_path("/abs/scripts/ok.rss", conf_prefix),
        Path::new("/abs/scripts/ok.rss")
    );
}

#[test]
fn resolve_relative_path_joins_conf_prefix() {
    let conf_prefix = Path::new("/prefix/conf");
    assert_eq!(
        resolve_rss_path("scripts/ok.rss", conf_prefix),
        Path::new("/prefix/conf/scripts/ok.rss")
    );
}

#[test]
fn load_normal_script_compiles() {
    let program = load_and_compile_rss_file(&fixture("ok.rss")).expect("ok.rss");
    assert!(
        program.imports.is_empty(),
        "sync milestone fixture must not require host imports"
    );
}

#[test]
fn missing_file_fails() {
    let dir = unique_temp_dir("missing-rss");
    let path = dir.join("no-such.rss");
    let err = load_rss_file(&path).expect_err("missing file");
    assert!(matches!(err, ConfigError::MissingFile(_)), "got {err:?}");
}

#[test]
fn directory_fails() {
    let dir = unique_temp_dir("rss-dir");
    let err = load_rss_file(dir.path()).expect_err("directory");
    assert!(matches!(err, ConfigError::NotRegularFile(_)), "got {err:?}");
}

#[test]
fn unreadable_file_fails() {
    let dir = unique_temp_dir("unreadable");
    let path = write_rss(dir.path(), "secret.rss", "let x = 1;\n");
    let mut perms = fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o000);
    fs::set_permissions(&path, perms).unwrap();
    let err = load_rss_file(&path);
    let mut restore = fs::metadata(&path).unwrap().permissions();
    restore.set_mode(0o600);
    fs::set_permissions(&path, restore).unwrap();
    let err = err.expect_err("unreadable");
    assert!(matches!(err, ConfigError::Unreadable { .. }), "got {err:?}");
}

#[test]
fn non_utf8_file_fails() {
    let dir = unique_temp_dir("non-utf8");
    let path = dir.join("bad.rss");
    fs::write(&path, [0xff, 0xfe, b'x']).unwrap();
    let err = load_rss_file(&path).expect_err("non-utf8");
    assert!(matches!(err, ConfigError::NotUtf8(_)), "got {err:?}");
}

#[test]
fn syntax_error_fails_compile() {
    let err = compile_rss_source(include_str!("fixtures/syntax_error.rss")).expect_err("syntax");
    assert!(matches!(err, ConfigError::Compile(_)), "got {err:?}");
}

#[test]
fn print_import_is_rejected() {
    let compiled = compile_source(include_str!("fixtures/print.rss")).expect("print compiles");
    assert!(
        compiled
            .program
            .imports
            .iter()
            .any(|import| import.name == "print"),
        "print must appear in Program.imports"
    );
    assert_unsupported_import(
        reject_program_imports(&compiled.program).expect_err("print"),
        "print",
    );
    assert_rejected_host(
        compile_rss_source(include_str!("fixtures/print.rss")).expect_err("print"),
        "print",
    );
}

#[test]
fn runtime_sleep_import_is_rejected() {
    let compiled =
        compile_source(include_str!("fixtures/runtime_sleep.rss")).expect("sleep compiles");
    assert!(
        compiled
            .program
            .imports
            .iter()
            .any(|import| import.name == "runtime::sleep"),
        "runtime::sleep must appear in Program.imports"
    );
    assert_unsupported_import(
        reject_program_imports(&compiled.program).expect_err("runtime::sleep"),
        "runtime::sleep",
    );
    assert_rejected_host(
        compile_rss_source(include_str!("fixtures/runtime_sleep.rss")).expect_err("runtime::sleep"),
        "runtime::sleep",
    );
}

#[test]
fn ngx_sleep_is_accepted_at_config_time() {
    let program = compile_rss_source(include_str!("fixtures/ngx_sleep.rss")).expect("ngx::sleep");
    assert!(
        ngx_rustscript::config::program_requires_request_state(&program),
        "ngx::sleep import must be recorded"
    );
}

#[test]
fn ctx_only_program_requires_request_owned_exec() {
    let program = compile_rss_source(include_str!("fixtures/ctx_sync.rss")).expect("ctx-only");
    assert!(
        !program.imports.iter().any(|import| {
            ngx_rustscript::host::ngx_host_manifest()
                .iter()
                .any(|entry| {
                    entry.name == import.name.as_str()
                        && entry.sync == ngx_rustscript::host::HostSyncKind::Async
                })
        }),
        "ctx_sync.rss must not import async hosts"
    );
    assert!(
        ngx_rustscript::config::program_requires_request_state(&program),
        "ctx-only scripts must select the request-owned AccessExec path"
    );
}

#[test]
fn import_free_and_codec_programs_stay_on_pool_fast_path() {
    let ok = compile_rss_source(include_str!("fixtures/ok.rss")).expect("ok.rss");
    assert!(
        !ngx_rustscript::config::program_requires_request_state(&ok),
        "import-free programs may stay on the pooled sync path"
    );
    let codec = compile_rss_source("use ngx;\nngx::md5(\"x\");\n").expect("codec");
    assert!(
        !ngx_rustscript::config::program_requires_request_state(&codec),
        "codec-only programs may stay on the pooled sync path"
    );
}

#[test]
fn request_state_routing_uses_manifest_async_or_request_ctx() {
    let config = include_str!("../src/config.rs");
    assert!(
        config.contains("HostFamily::RequestCtx") || config.contains("HostCapability::RequestCtx"),
        "routing must use authoritative RequestCtx manifest metadata, not string matching"
    );
    assert!(
        config.contains("HostFamily::Cosocket"),
        "routing must use Cosocket family metadata for UDP constructor/close/settimeout"
    );
    let shared = compile_rss_source("use ngx;\nngx::shared::dict(\"dogs\");\n").expect("shared");
    assert!(
        !ngx_rustscript::config::program_requires_request_state(&shared),
        "shared dict programs stay on the pooled path"
    );
    assert!(
        config.contains("HostSyncKind::Async"),
        "routing must use authoritative Async manifest metadata"
    );
    let access = include_str!("../src/access.rs");
    assert!(
        access.contains("program_requires_request_state")
            || access.contains("program_needs_request_exec"),
        "access handler must consult request-state routing"
    );
    assert!(
        ngx_rustscript::host::AllowedOps::for_phase(ngx_rustscript::host::NgxPhase::Access)
            .contains(ngx_rustscript::host::AllowedOps::REQUEST_CTX),
        "Access phase AllowedOps must include RequestCtx"
    );
}

#[test]
fn unknown_import_is_rejected_at_config_time() {
    let err = compile_rss_source(include_str!("fixtures/unknown_import.rss"))
        .expect_err("unknown import must fail");
    match err {
        ConfigError::Compile(msg) | ConfigError::UnsupportedImport(msg) => {
            let lower = msg.to_ascii_lowercase();
            assert!(
                lower.contains("unknown")
                    || lower.contains("import")
                    || lower.contains("unknownmod"),
                "error should name the unknown import, got {msg}"
            );
        }
        other => panic!("expected compile or unsupported import, got {other:?}"),
    }
}

#[test]
fn empty_imports_are_accepted() {
    let compiled = compile_source(include_str!("fixtures/ok.rss")).unwrap();
    reject_program_imports(&compiled.program).expect("no imports");
}

#[test]
fn duplicate_directive_in_same_location_is_rejected() {
    let program = load_and_compile_rss_file(&fixture("ok.rss")).unwrap();
    let mut conf = LocationConf::default();
    conf.set_access_program(program.clone()).unwrap();
    let err = conf.set_access_program(program).expect_err("duplicate");
    assert!(matches!(err, ConfigError::Duplicate), "got {err:?}");
}

#[test]
fn unset_child_inherits_parent_program() {
    let parent_program = load_and_compile_rss_file(&fixture("ok.rss")).unwrap();
    let mut parent = LocationConf::default();
    parent.set_access_program(parent_program).unwrap();
    let mut child = LocationConf::default();
    child.merge(&parent).unwrap();
    assert!(
        child.access_program().is_some(),
        "unset child must inherit parent"
    );
}

#[test]
fn explicit_child_overrides_parent_program() {
    let parent_program = load_and_compile_rss_file(&fixture("runtime_error.rss")).unwrap();
    let child_program = load_and_compile_rss_file(&fixture("ok.rss")).unwrap();
    let mut parent = LocationConf::default();
    parent.set_access_program(parent_program).unwrap();
    let mut child = LocationConf::default();
    child.set_access_program(child_program).unwrap();
    child.merge(&parent).unwrap();
    let child_program = child.access_program().expect("override kept");
    run_access_program(child_program).expect("override script must halt");
}

#[test]
fn standard_host_policy_is_fail_closed_and_inherits_per_field() {
    let mut parent = LocationConf::default();
    parent.add_io_root("/srv/rss/input".into());
    parent.set_io_write(true);
    parent.add_http_scheme("https".into());
    parent.add_http_allow("api.example.test".into(), 443);
    parent.set_http_allow_private(false);
    parent.set_sqlite_root("/srv/rss/db".into());

    let mut child = LocationConf::default();
    child.set_io_write(false);
    child.merge(&parent).unwrap();
    let policy = child.standard_host_policy();
    assert_eq!(policy.io_roots, ["/srv/rss/input"]);
    assert!(
        !policy.io_write,
        "explicit child deny overrides parent grant"
    );
    assert_eq!(policy.http_hosts, ["api.example.test"]);
    assert_eq!(policy.http_ports, [443]);
    assert_eq!(policy.http_schemes, ["https"]);
    assert!(!policy.http_allow_private);
    assert_eq!(policy.sqlite_root.as_deref(), Some("/srv/rss/db"));

    let defaults = LocationConf::default().standard_host_policy();
    assert!(defaults.io_roots.is_empty());
    assert!(defaults.http_hosts.is_empty());
    assert!(defaults.http_ports.is_empty());
    assert!(defaults.sqlite_root.is_none());
}

#[test]
fn unconfigured_location_has_no_program() {
    let conf = LocationConf::default();
    assert!(conf.access_program().is_none());
}

#[test]
fn vm_normal_halt() {
    let program = compile_rss_source(include_str!("fixtures/ok.rss")).unwrap();
    let status = run_access_program(&program).expect("run");
    assert_eq!(status, VmStatus::Halted);
    assert_eq!(status_from_vm_run(Ok(status)), Status::NGX_OK);
}

#[test]
fn vm_runtime_error() {
    let program = compile_rss_source(include_str!("fixtures/runtime_error.rss")).unwrap();
    let err = run_access_program(&program).expect_err("division by zero");
    let rendered = err.to_string().to_ascii_lowercase();
    assert!(
        rendered.contains("division") || rendered.contains("zero"),
        "got {err}"
    );
    let status = status_from_vm_run(Err(err));
    assert_ne!(status, Status::NGX_OK);
    assert_ne!(status, Status::NGX_DECLINED);
}

#[test]
fn unexpected_yield_is_request_failure() {
    let status = status_from_vm_run(Ok(VmStatus::Yielded));
    assert_ne!(status, Status::NGX_OK);
    assert_ne!(status, Status::NGX_DECLINED);
}

#[test]
fn empty_path_is_rejected() {
    let err = load_rss_file(Path::new("")).expect_err("empty");
    assert!(
        matches!(err, ConfigError::EmptyPath | ConfigError::MissingFile(_)),
        "got {err:?}"
    );
}

#[test]
fn test_binary_defines_exact_nginx_stub_symbols() {
    let exe = std::env::current_exe().expect("current_exe");
    let output = Command::new("nm")
        .args(["--defined-only", "--format=posix"])
        .arg(&exe)
        .output()
        .expect("nm");
    assert!(output.status.success(), "nm failed on {}", exe.display());
    let stdout = String::from_utf8_lossy(&output.stdout);
    for name in [
        "ngx_conf_full_name",
        "ngx_conf_log_error",
        "ngx_array_push",
        "ngx_log_error_core",
        "ngx_pool_cleanup_add",
        "ngx_palloc",
        "ngx_http_core_module",
        "ngx_http_core_run_phases",
    ] {
        assert!(
            stdout
                .lines()
                .any(|line| line.split_whitespace().next() == Some(name)),
            "test binary must define stub {name}:\n{stdout}"
        );
    }
}
