//! `access_by_rss_file` location configuration: resolve, read, compile, bind-check.

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ngx::core::{NGX_CONF_ERROR, NGX_CONF_OK};
use ngx::ffi::{
    NGX_CONF_TAKE1, NGX_HTTP_LOC_CONF, NGX_HTTP_LOC_CONF_OFFSET, NGX_LOG_EMERG, ngx_command_t,
    ngx_conf_t, ngx_uint_t,
};
use ngx::http::{Merge, MergeConfigError};
use ngx::{ngx_conf_log_error, ngx_string};
use vm::{
    CompileSourceFileOptions, Program, SourceFlavor, Vm, compile_source_with_flavor_and_options,
};

use crate::host::{
    HostCapability, HostFamily, HostSyncKind, NgxPhase, ngx_host_catalog, ngx_host_manifest,
};

/// Compiled access-phase program stored on the location config.
#[derive(Default)]
pub struct LocationConf {
    program: Option<Arc<Program>>,
}

impl LocationConf {
    /// True when this location currently holds a compiled program.
    ///
    /// After merge this includes a program inherited from a parent. Duplicate
    /// detection runs in the directive setter before merge, so a child that
    /// did not write the directive still starts unset.
    pub fn is_set(&self) -> bool {
        self.program.is_some()
    }

    /// Compiled program after merge, if any.
    pub fn program(&self) -> Option<&Arc<Program>> {
        self.program.as_ref()
    }

    /// Record a compiled program. A second set in the same location is a duplicate.
    pub fn set_program(&mut self, program: Arc<Program>) -> Result<(), ConfigError> {
        if self.program.is_some() {
            return Err(ConfigError::Duplicate);
        }
        self.program = Some(program);
        Ok(())
    }
}

impl Merge for LocationConf {
    fn merge(&mut self, prev: &Self) -> Result<(), MergeConfigError> {
        if self.program.is_none() {
            self.program = prev.program.clone();
        }
        Ok(())
    }
}

/// Failures while resolving, reading, compiling, or installing a script.
#[derive(Debug)]
pub enum ConfigError {
    EmptyPath,
    MissingFile(PathBuf),
    NotRegularFile(PathBuf),
    Unreadable { path: PathBuf, source: io::Error },
    NotUtf8(PathBuf),
    Compile(String),
    UnsupportedImport(String),
    PhaseForbidden { name: String, phase: NgxPhase },
    Duplicate,
    VmConstruct(String),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyPath => write!(f, "empty script path"),
            Self::MissingFile(path) => write!(f, "script file not found: {}", path.display()),
            Self::NotRegularFile(path) => {
                write!(f, "script path is not a regular file: {}", path.display())
            }
            Self::Unreadable { path, source } => {
                write!(f, "cannot read script {}: {source}", path.display())
            }
            Self::NotUtf8(path) => write!(f, "script is not valid UTF-8: {}", path.display()),
            Self::Compile(msg) => write!(f, "failed to compile script: {msg}"),
            Self::UnsupportedImport(names) => {
                write!(f, "script imports are not supported: {names}")
            }
            Self::PhaseForbidden { name, phase } => {
                write!(
                    f,
                    "script import {name} is not allowed in the {phase} phase"
                )
            }
            Self::Duplicate => write!(f, "duplicate access_by_rss_file in this location"),
            Self::VmConstruct(msg) => write!(f, "cannot construct VM for script: {msg}"),
        }
    }
}

impl std::error::Error for ConfigError {}

/// NGINX-style config path: absolute stays absolute, relative joins `conf_prefix`.
///
/// Production resolution uses `ngx_conf_full_name` (same policy). This helper
/// is the unit-testable form of that rule.
pub fn resolve_rss_path(raw: &str, conf_prefix: &Path) -> PathBuf {
    let path = Path::new(raw);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        conf_prefix.join(path)
    }
}

/// Read a regular UTF-8 file once. Directories, missing paths, and binary junk fail.
pub fn load_rss_file(path: &Path) -> Result<String, ConfigError> {
    if path.as_os_str().is_empty() {
        return Err(ConfigError::EmptyPath);
    }
    let meta = match fs::metadata(path) {
        Ok(meta) => meta,
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            return Err(ConfigError::MissingFile(path.to_path_buf()));
        }
        Err(err) => {
            return Err(ConfigError::Unreadable {
                path: path.to_path_buf(),
                source: err,
            });
        }
    };
    if !meta.is_file() {
        return Err(ConfigError::NotRegularFile(path.to_path_buf()));
    }
    let bytes = fs::read(path).map_err(|err| ConfigError::Unreadable {
        path: path.to_path_buf(),
        source: err,
    })?;
    String::from_utf8(bytes).map_err(|_| ConfigError::NotUtf8(path.to_path_buf()))
}

/// Accept catalogued hosts allowed in the access phase. Reject `print`,
/// `runtime::sleep`, unknown imports, and phase-forbidden imports.
pub fn reject_program_imports(program: &Program) -> Result<(), ConfigError> {
    validate_program_imports_for_phase(program, NgxPhase::Access)
}

/// Check each program import against the handwritten ngx manifest.
pub fn validate_program_imports_for_phase(
    program: &Program,
    phase: NgxPhase,
) -> Result<(), ConfigError> {
    let manifest = ngx_host_manifest();
    let mut unknown = Vec::new();
    let mut forbidden = Vec::new();
    for import in &program.imports {
        match manifest
            .iter()
            .find(|entry| entry.name == import.name.as_str())
        {
            None => unknown.push(import.name.as_str()),
            Some(entry) if !entry.phases.contains(&phase) => {
                forbidden.push(import.name.as_str());
            }
            Some(_) => {}
        }
    }
    if !unknown.is_empty() {
        return Err(ConfigError::UnsupportedImport(unknown.join(", ")));
    }
    if let Some(name) = forbidden.first() {
        return Err(ConfigError::PhaseForbidden {
            name: (*name).to_string(),
            phase,
        });
    }
    Ok(())
}

/// True when the compiled program needs request-owned `AccessExec`.
///
/// Async hosts, RequestCtx, and mutation/control capabilities all require
/// per-request state. Pure-sync request-read programs stay on the pooled path.
pub fn program_requires_request_state(program: &Program) -> bool {
    program.imports.iter().any(|import| {
        ngx_host_manifest().iter().any(|entry| {
            entry.name == import.name.as_str()
                && (entry.sync == HostSyncKind::Async
                    || entry.family == HostFamily::RequestCtx
                    || entry.capabilities.iter().any(|cap| {
                        matches!(
                            cap,
                            HostCapability::RequestWrite
                                | HostCapability::ResponseWrite
                                | HostCapability::Control
                        )
                    }))
        })
    })
}

/// Compile with the ngx host catalog and reject unknown or phase-forbidden imports.
pub fn compile_rss_source(source: &str) -> Result<Arc<Program>, ConfigError> {
    let catalog = Arc::new(ngx_host_catalog());
    let compiled = compile_source_with_flavor_and_options(
        source,
        SourceFlavor::RustScript,
        CompileSourceFileOptions::default().with_host_api_catalog(catalog),
    )
    .map_err(|err| ConfigError::Compile(err.to_string()))?;
    reject_program_imports(&compiled.program)?;
    Vm::try_new(compiled.program.clone())
        .map_err(|err| ConfigError::VmConstruct(err.to_string()))?;
    Ok(Arc::new(compiled.program))
}

/// Resolve-independent load: regular file, UTF-8, compile, import check, VM construct.
pub fn load_and_compile_rss_file(path: &Path) -> Result<Arc<Program>, ConfigError> {
    let source = load_rss_file(path)?;
    compile_rss_source(&source)
}

pub(crate) static mut COMMANDS: [ngx_command_t; 2] = [
    ngx_command_t {
        name: ngx_string!("access_by_rss_file"),
        type_: (NGX_HTTP_LOC_CONF | NGX_CONF_TAKE1) as ngx_uint_t,
        set: Some(set_access_by_rss_file),
        conf: NGX_HTTP_LOC_CONF_OFFSET,
        offset: 0,
        post: std::ptr::null_mut(),
    },
    ngx_command_t::empty(),
];

extern "C" fn set_access_by_rss_file(
    cf: *mut ngx_conf_t,
    _cmd: *mut ngx_command_t,
    conf: *mut core::ffi::c_void,
) -> *mut core::ffi::c_char {
    // SAFETY: NGINX invokes the setter with a live conf, command, and location conf.
    let loc = unsafe { &mut *conf.cast::<LocationConf>() };
    if loc.is_set() {
        ngx_conf_log_error!(
            NGX_LOG_EMERG,
            cf,
            "duplicate access_by_rss_file directive in this location"
        );
        return NGX_CONF_ERROR;
    }

    let args = unsafe { (*(*cf).args).as_slice() };
    if args.len() != 2 {
        ngx_conf_log_error!(
            NGX_LOG_EMERG,
            cf,
            "invalid number of arguments in \"access_by_rss_file\""
        );
        return NGX_CONF_ERROR;
    }

    let mut name = args[1];
    // SAFETY: `ngx_conf_full_name` is the official relative-to-conf-prefix resolver.
    // It implements the same absolute/relative policy as [`resolve_rss_path`].
    let resolved = unsafe { nginx_sys::ngx_conf_full_name((*cf).cycle, &mut name, 1) };
    if resolved != ngx::core::Status::NGX_OK.0 {
        ngx_conf_log_error!(
            NGX_LOG_EMERG,
            cf,
            "access_by_rss_file: failed to resolve script path"
        );
        return NGX_CONF_ERROR;
    }

    let path = match name.to_str() {
        Ok(path) if !path.is_empty() => Path::new(path),
        Ok(_) => {
            ngx_conf_log_error!(NGX_LOG_EMERG, cf, "access_by_rss_file: empty script path");
            return NGX_CONF_ERROR;
        }
        Err(_) => {
            ngx_conf_log_error!(
                NGX_LOG_EMERG,
                cf,
                "access_by_rss_file: script path is not valid UTF-8"
            );
            return NGX_CONF_ERROR;
        }
    };

    match load_and_compile_rss_file(path) {
        Ok(program) => match loc.set_program(program) {
            Ok(()) => NGX_CONF_OK,
            Err(err) => {
                ngx_conf_log_error!(NGX_LOG_EMERG, cf, "access_by_rss_file: {err}");
                NGX_CONF_ERROR
            }
        },
        Err(err) => {
            ngx_conf_log_error!(NGX_LOG_EMERG, cf, "access_by_rss_file: {err}");
            NGX_CONF_ERROR
        }
    }
}
