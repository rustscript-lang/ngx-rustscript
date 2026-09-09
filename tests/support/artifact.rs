//! Resolve the built module cdylib for `load_module`.
//!
//! Tests build the exact crate cdylib under `CARGO_TARGET_DIR` and pass that
//! absolute path to NGINX. A minimal `nm` check rejects production artifacts
//! that define test-only NGINX C stubs.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::temp::env_path;

pub const MODULE_CDYLIB_NAME: &str = "libngx_rustscript.so";

/// Production cdylib must leave these NGINX symbols undefined (no test stubs).
pub const FORBIDDEN_DEFINED_NGINX_STUBS: &[&str] = &[
    "ngx_conf_full_name",
    "ngx_conf_log_error",
    "ngx_array_push",
    "ngx_log_error_core",
    "ngx_pool_cleanup_add",
    "ngx_palloc",
    "ngx_http_core_module",
    "ngx_get_connection",
    "ngx_free_connection",
    "ngx_close_connection",
    "ngx_event_actions",
    "ngx_event_flags",
    "ngx_process",
    "ngx_http_core_run_phases",
    "ngx_http_test_reading",
    "ngx_http_block_reading",
    "ngx_handle_read_event",
    "ngx_posted_events",
    "ngx_pid",
    "ngx_worker",
    "ngx_exiting",
    "ngx_cycle",
    "ngx_core_module",
    "ngx_processes",
    "ngx_http_get_variable",
    "ngx_list_push",
    "ngx_pnalloc",
    "ngx_hash_find",
    "ngx_hash_strlow",
    "ngx_http_parse_time",
    "ngx_parse_http_time",
    "ngx_http_set_exten",
    "ngx_http_send_header",
    "ngx_http_send_special",
];

/// Production cdylib/add-dynamic module must define these C ABI shims.
pub const REQUIRED_DEFINED_REQUEST_SHIMS: &[&str] = &[
    "ngx_rustscript_request_is_internal",
    "ngx_rustscript_request_headers_sent",
    "ngx_rustscript_request_reading_body",
    "ngx_rustscript_request_discard_body",
];

/// Test-only C setters must never appear in production artifacts.
pub const FORBIDDEN_DEFINED_REQUEST_TEST_SETTERS: &[&str] = &[
    "ngx_rustscript_test_request_new",
    "ngx_rustscript_test_request_free",
    "ngx_rustscript_test_request_sizeof",
    "ngx_rustscript_test_request_set_internal",
    "ngx_rustscript_test_request_set_filter_finalize",
    "ngx_rustscript_test_request_set_error_page",
    "ngx_rustscript_test_request_set_header_sent",
    "ngx_rustscript_test_request_set_reading_body",
    "ngx_rustscript_test_request_set_discard_body",
];

pub fn resolve_module_candidate(
    explicit_module: Option<&Path>,
    cargo_target_dir: Option<&Path>,
    crate_root: &Path,
) -> PathBuf {
    if let Some(path) = explicit_module {
        return path.to_path_buf();
    }
    let target_dir = match cargo_target_dir {
        Some(dir) => dir.to_path_buf(),
        None => crate_root.join("target"),
    };
    target_dir.join("debug").join(MODULE_CDYLIB_NAME)
}

#[derive(Debug)]
pub enum ModuleArtifactError {
    Missing(PathBuf),
    NotRegularFile(PathBuf),
    DefinedStub { artifact: PathBuf, stub: String },
}

impl fmt::Display for ModuleArtifactError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing(path) => write!(f, "module cdylib not found: {}", path.display()),
            Self::NotRegularFile(path) => {
                write!(f, "module path is not a regular file: {}", path.display())
            }
            Self::DefinedStub { artifact, stub } => write!(
                f,
                "module cdylib {} defines test NGINX stub {stub}",
                artifact.display()
            ),
        }
    }
}

pub fn require_canonical_regular_file(path: &Path) -> Result<PathBuf, ModuleArtifactError> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(_) => return Err(ModuleArtifactError::Missing(path.to_path_buf())),
    };
    if !metadata.is_file() {
        return Err(ModuleArtifactError::NotRegularFile(path.to_path_buf()));
    }
    let canonical =
        fs::canonicalize(path).map_err(|_| ModuleArtifactError::Missing(path.to_path_buf()))?;
    let canonical_meta =
        fs::metadata(&canonical).map_err(|_| ModuleArtifactError::Missing(canonical.clone()))?;
    if !canonical_meta.is_file() {
        return Err(ModuleArtifactError::NotRegularFile(canonical));
    }
    Ok(canonical)
}

pub fn defined_dynamic_symbols(path: &Path) -> Result<Vec<String>, ModuleArtifactError> {
    let output = Command::new("nm")
        .args(["-D", "--defined-only", "--format=posix"])
        .arg(path)
        .output()
        .map_err(|_| ModuleArtifactError::Missing(path.to_path_buf()))?;
    if !output.status.success() {
        return Err(ModuleArtifactError::NotRegularFile(path.to_path_buf()));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.split_whitespace().next().map(str::to_string))
        .collect())
}

pub fn reject_defined_nginx_stubs(path: &Path) -> Result<(), ModuleArtifactError> {
    let symbols = defined_dynamic_symbols(path)?;
    if let Some(stub) = FORBIDDEN_DEFINED_NGINX_STUBS
        .iter()
        .copied()
        .chain(FORBIDDEN_DEFINED_REQUEST_TEST_SETTERS.iter().copied())
        .find(|name| symbols.iter().any(|symbol| symbol == name))
    {
        return Err(ModuleArtifactError::DefinedStub {
            artifact: path.to_path_buf(),
            stub: stub.to_string(),
        });
    }
    Ok(())
}

#[derive(Debug)]
pub enum MissingShimError {
    Missing { artifact: PathBuf, shim: String },
}

impl fmt::Display for MissingShimError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing { artifact, shim } => write!(
                f,
                "module cdylib {} is missing required request shim {shim}",
                artifact.display()
            ),
        }
    }
}

pub fn require_defined_request_shims(path: &Path) -> Result<(), MissingShimError> {
    let symbols = defined_dynamic_symbols(path).map_err(|_| MissingShimError::Missing {
        artifact: path.to_path_buf(),
        shim: REQUIRED_DEFINED_REQUEST_SHIMS[0].to_string(),
    })?;
    if let Some(shim) = REQUIRED_DEFINED_REQUEST_SHIMS
        .iter()
        .copied()
        .find(|name| !symbols.iter().any(|symbol| symbol == name))
    {
        return Err(MissingShimError::Missing {
            artifact: path.to_path_buf(),
            shim: shim.to_string(),
        });
    }
    Ok(())
}

/// Absolute path of the cdylib built in this `CARGO_TARGET_DIR`.
pub fn module_cdylib() -> PathBuf {
    let crate_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let candidate = resolve_module_candidate(
        env_path("NGX_RUSTSCRIPT_MODULE").as_deref(),
        env_path("CARGO_TARGET_DIR").as_deref(),
        crate_root,
    );
    let path = require_canonical_regular_file(&candidate).unwrap_or_else(|err| {
        panic!(
            "{err}; build the cdylib first with `cargo build --lib` under the same CARGO_TARGET_DIR"
        )
    });
    reject_defined_nginx_stubs(&path).unwrap_or_else(|err| panic!("{err}"));
    require_defined_request_shims(&path).unwrap_or_else(|err| panic!("{err}"));
    path
}

#[test]
fn resolves_module_under_explicit_cargo_target_dir() {
    let crate_root = Path::new("/crate");
    let target = Path::new("/slot/target");
    let got = resolve_module_candidate(None, Some(target), crate_root);
    assert_eq!(
        got,
        PathBuf::from("/slot/target/debug/libngx_rustscript.so")
    );
}

#[test]
fn resolves_module_under_crate_target_when_cargo_target_dir_absent() {
    let crate_root = Path::new("/crate");
    let got = resolve_module_candidate(None, None, crate_root);
    assert_eq!(
        got,
        PathBuf::from("/crate/target/debug/libngx_rustscript.so")
    );
}

#[test]
fn explicit_module_path_overrides_cargo_target_dir() {
    let crate_root = Path::new("/crate");
    let target = Path::new("/slot/target");
    let explicit = Path::new("/elsewhere/libngx_rustscript.so");
    let got = resolve_module_candidate(Some(explicit), Some(target), crate_root);
    assert_eq!(got, explicit);
}

#[test]
fn production_cdylib_does_not_define_test_nginx_stubs() {
    let module = module_cdylib();
    reject_defined_nginx_stubs(&module).expect("production cdylib must not export test C stubs");
}

#[test]
fn production_cdylib_defines_request_bitfield_shims() {
    let module = module_cdylib();
    require_defined_request_shims(&module)
        .expect("production cdylib must export request bitfield shims");
}
