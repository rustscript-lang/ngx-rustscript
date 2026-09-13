//! `access_by_rss_file` / `content_by_rss_file` / `log_by_rss_file` location configuration.

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ngx::core::{NGX_CONF_ERROR, NGX_CONF_OK};
use ngx::ffi::{
    NGX_CONF_FLAG, NGX_CONF_TAKE1, NGX_CONF_TAKE2, NGX_HTTP_LOC_CONF, NGX_HTTP_LOC_CONF_OFFSET,
    NGX_HTTP_MAIN_CONF, NGX_LOG_EMERG, ngx_command_t, ngx_conf_t, ngx_uint_t,
};
use ngx::http::{Merge, MergeConfigError};
use ngx::{ngx_conf_log_error, ngx_string};
use vm::{
    CompileSourceFileOptions, Program, SourceFlavor, Vm, compile_source_with_flavor_and_options,
};

use crate::host::{
    HostCapability, HostFamily, HostSyncKind, NgxPhase, StandardHostPolicy, compile_host_catalog,
    host_validates_for_log, is_standard_host_import, ngx_host_manifest,
};
use crate::phase::{DuplicateSlot, ScriptExecutionMetadata, ScriptLocationConf};

/// Per-location compiled scripts. Access, content, and log slots are independent.
#[derive(Default)]
pub struct LocationConf {
    access: ScriptLocationConf,
    content: ScriptLocationConf,
    log: ScriptLocationConf,
    io_roots: Option<Vec<String>>,
    io_write: Option<bool>,
    http_schemes: Option<Vec<String>>,
    http_allow: Option<(String, u16)>,
    http_allow_private: Option<bool>,
    sqlite_root: Option<String>,
}

impl LocationConf {
    /// Compiled access program after merge, if any.
    pub fn access_program(&self) -> Option<&Arc<Program>> {
        self.access.program()
    }

    pub fn access_metadata(&self) -> ScriptExecutionMetadata {
        self.access.metadata()
    }

    /// Compiled content program after merge, if any.
    pub fn content_program(&self) -> Option<&Arc<Program>> {
        self.content.program()
    }

    pub fn content_metadata(&self) -> ScriptExecutionMetadata {
        self.content.metadata()
    }

    /// Compiled log program after merge, if any.
    pub fn log_program(&self) -> Option<&Arc<Program>> {
        self.log.program()
    }

    pub fn log_metadata(&self) -> ScriptExecutionMetadata {
        self.log.metadata()
    }

    /// Record a compiled access program. A second set in the same location is a duplicate.
    pub fn set_access_program(&mut self, program: Arc<Program>) -> Result<(), ConfigError> {
        let metadata = script_execution_metadata(&program);
        self.access
            .set_program_with_metadata(program, metadata)
            .map_err(|DuplicateSlot| ConfigError::Duplicate)
    }

    /// Record a compiled content program. A second set in the same location is a duplicate.
    pub fn set_content_program(&mut self, program: Arc<Program>) -> Result<(), ConfigError> {
        let metadata = script_execution_metadata(&program);
        self.content
            .set_program_with_metadata(program, metadata)
            .map_err(|DuplicateSlot| ConfigError::DuplicateContent)
    }

    /// Record a compiled log program. A second set in the same location is a duplicate.
    pub fn set_log_program(&mut self, program: Arc<Program>) -> Result<(), ConfigError> {
        let metadata = script_execution_metadata(&program);
        self.log
            .set_program_with_metadata(program, metadata)
            .map_err(|DuplicateSlot| ConfigError::DuplicateLog)
    }

    pub fn add_io_root(&mut self, root: String) {
        self.io_roots.get_or_insert_with(Vec::new).push(root);
    }

    pub fn set_io_write(&mut self, allow: bool) {
        self.io_write = Some(allow);
    }

    pub fn add_http_scheme(&mut self, scheme: String) {
        self.http_schemes.get_or_insert_with(Vec::new).push(scheme);
    }

    pub fn try_set_http_allow(&mut self, host: String, port: u16) -> bool {
        if self.http_allow.is_some() {
            return false;
        }
        self.http_allow = Some((host, port));
        true
    }

    pub fn set_http_allow_private(&mut self, allow: bool) {
        self.http_allow_private = Some(allow);
    }

    pub fn set_sqlite_root(&mut self, root: String) {
        self.sqlite_root = Some(root);
    }

    pub fn standard_host_policy(&self) -> StandardHostPolicy {
        let mut policy = StandardHostPolicy::fail_closed();
        policy.io_roots = self.io_roots.clone().unwrap_or_default();
        policy.io_write = self.io_write.unwrap_or(false);
        policy.http_schemes = self.http_schemes.clone().unwrap_or_default();
        if let Some((host, port)) = &self.http_allow {
            policy.http_hosts.push(host.clone());
            policy.http_ports.push(*port);
        }
        policy.http_allow_private = self.http_allow_private.unwrap_or(false);
        policy.sqlite_root = self.sqlite_root.clone();
        policy
    }
}

impl Merge for LocationConf {
    fn merge(&mut self, prev: &Self) -> Result<(), MergeConfigError> {
        self.access.merge_from(&prev.access);
        self.content.merge_from(&prev.content);
        self.log.merge_from(&prev.log);
        if self.io_roots.is_none() {
            self.io_roots.clone_from(&prev.io_roots);
        }
        if self.io_write.is_none() {
            self.io_write = prev.io_write;
        }
        if self.http_schemes.is_none() {
            self.http_schemes.clone_from(&prev.http_schemes);
        }
        if self.http_allow.is_none() {
            self.http_allow.clone_from(&prev.http_allow);
        }
        if self.http_allow_private.is_none() {
            self.http_allow_private = prev.http_allow_private;
        }
        if self.sqlite_root.is_none() {
            self.sqlite_root.clone_from(&prev.sqlite_root);
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
    DuplicateContent,
    DuplicateLog,
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
            Self::DuplicateContent => write!(f, "duplicate content_by_rss_file in this location"),
            Self::DuplicateLog => write!(f, "duplicate log_by_rss_file in this location"),
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct StandardBuiltinUsage {
    io: bool,
    sqlite: bool,
}

fn standard_builtin_usage(program: &Program) -> StandardBuiltinUsage {
    let disassembly = vm::disassemble_program(program);
    StandardBuiltinUsage {
        io: disassembly.contains("builtin io_"),
        sqlite: disassembly.contains("builtin sqlite_"),
    }
}

fn first_standard_builtin(program: &Program) -> Option<String> {
    let usage = standard_builtin_usage(program);
    if usage.io {
        return Some("io::*".into());
    }
    if usage.sqlite {
        return Some("sqlite::*".into());
    }
    None
}

/// Check each program import against the handwritten ngx manifest.
pub fn validate_program_imports_for_phase(
    program: &Program,
    phase: NgxPhase,
) -> Result<(), ConfigError> {
    if phase == NgxPhase::Log {
        if let Some(name) = first_standard_builtin(program) {
            return Err(ConfigError::PhaseForbidden { name, phase });
        }
    }
    let manifest = ngx_host_manifest();
    let mut unknown = Vec::new();
    let mut forbidden = Vec::new();
    for import in &program.imports {
        match manifest
            .iter()
            .find(|entry| entry.name == import.name.as_str())
        {
            None if is_standard_host_import(&import.name) && phase != NgxPhase::Log => {}
            None if is_standard_host_import(&import.name) => {
                forbidden.push(import.name.as_str());
            }
            None => unknown.push(import.name.as_str()),
            Some(entry) if phase == NgxPhase::Log && !host_validates_for_log(entry) => {
                forbidden.push(import.name.as_str());
            }
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

fn request_state_required_by_imports(program: &Program) -> bool {
    program.imports.iter().any(|import| {
        is_standard_host_import(&import.name)
            || ngx_host_manifest().iter().any(|entry| {
                entry.name == import.name.as_str()
                    && (entry.sync == HostSyncKind::Async
                        || entry.family == HostFamily::RequestCtx
                        || entry.family == HostFamily::Cosocket
                        || entry.family == HostFamily::Semaphore
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

fn script_execution_metadata(program: &Program) -> ScriptExecutionMetadata {
    let builtins = standard_builtin_usage(program);
    ScriptExecutionMetadata {
        requires_request_state: builtins.io
            || builtins.sqlite
            || request_state_required_by_imports(program),
        uses_sqlite: builtins.sqlite,
    }
}

/// True when the compiled program needs request-owned `AccessExec`.
///
/// Async hosts, RequestCtx, Cosocket, and mutation/control capabilities
/// all require per-request state. Pure-sync request-read programs stay on
/// the pooled path.
pub fn program_requires_request_state(program: &Program) -> bool {
    script_execution_metadata(program).requires_request_state
}

/// Compile with the ngx host catalog and reject unknown or phase-forbidden imports.
pub fn compile_rss_source(source: &str) -> Result<Arc<Program>, ConfigError> {
    compile_rss_source_for_phase(source, NgxPhase::Access)
}

/// Compile and validate imports against a specific NGINX phase.
pub fn compile_rss_source_for_phase(
    source: &str,
    phase: NgxPhase,
) -> Result<Arc<Program>, ConfigError> {
    let catalog = Arc::new(compile_host_catalog());
    let compiled = compile_source_with_flavor_and_options(
        source,
        SourceFlavor::RustScript,
        CompileSourceFileOptions::default().with_host_api_catalog(catalog),
    )
    .map_err(|err| ConfigError::Compile(err.to_string()))?;
    validate_program_imports_for_phase(&compiled.program, phase)?;
    Vm::try_new(compiled.program.clone())
        .map_err(|err| ConfigError::VmConstruct(err.to_string()))?;
    Ok(Arc::new(compiled.program))
}

/// Resolve-independent load: regular file, UTF-8, compile, import check, VM construct.
pub fn load_and_compile_rss_file(path: &Path) -> Result<Arc<Program>, ConfigError> {
    load_and_compile_rss_file_for_phase(path, NgxPhase::Access)
}

/// Load a script and validate it for the given phase.
pub fn load_and_compile_rss_file_for_phase(
    path: &Path,
    phase: NgxPhase,
) -> Result<Arc<Program>, ConfigError> {
    let source = load_rss_file(path)?;
    compile_rss_source_for_phase(&source, phase)
}

pub(crate) static mut COMMANDS: [ngx_command_t; 11] = [
    ngx_command_t {
        name: ngx_string!("rss_shared_dict"),
        type_: (NGX_HTTP_MAIN_CONF | NGX_CONF_TAKE2) as ngx_uint_t,
        set: Some(set_rss_shared_dict),
        conf: 0,
        offset: 0,
        post: std::ptr::null_mut(),
    },
    ngx_command_t {
        name: ngx_string!("access_by_rss_file"),
        type_: (NGX_HTTP_LOC_CONF | NGX_CONF_TAKE1) as ngx_uint_t,
        set: Some(set_access_by_rss_file),
        conf: NGX_HTTP_LOC_CONF_OFFSET,
        offset: 0,
        post: std::ptr::null_mut(),
    },
    ngx_command_t {
        name: ngx_string!("content_by_rss_file"),
        type_: (NGX_HTTP_LOC_CONF | NGX_CONF_TAKE1) as ngx_uint_t,
        set: Some(set_content_by_rss_file),
        conf: NGX_HTTP_LOC_CONF_OFFSET,
        offset: 0,
        post: std::ptr::null_mut(),
    },
    ngx_command_t {
        name: ngx_string!("log_by_rss_file"),
        type_: (NGX_HTTP_LOC_CONF | NGX_CONF_TAKE1) as ngx_uint_t,
        set: Some(set_log_by_rss_file),
        conf: NGX_HTTP_LOC_CONF_OFFSET,
        offset: 0,
        post: std::ptr::null_mut(),
    },
    ngx_command_t {
        name: ngx_string!("rss_io_root"),
        type_: (NGX_HTTP_LOC_CONF | NGX_CONF_TAKE1) as ngx_uint_t,
        set: Some(set_rss_io_root),
        conf: NGX_HTTP_LOC_CONF_OFFSET,
        offset: 0,
        post: std::ptr::null_mut(),
    },
    ngx_command_t {
        name: ngx_string!("rss_io_write"),
        type_: (NGX_HTTP_LOC_CONF | NGX_CONF_FLAG) as ngx_uint_t,
        set: Some(set_rss_io_write),
        conf: NGX_HTTP_LOC_CONF_OFFSET,
        offset: 0,
        post: std::ptr::null_mut(),
    },
    ngx_command_t {
        name: ngx_string!("rss_http_scheme"),
        type_: (NGX_HTTP_LOC_CONF | NGX_CONF_TAKE1) as ngx_uint_t,
        set: Some(set_rss_http_scheme),
        conf: NGX_HTTP_LOC_CONF_OFFSET,
        offset: 0,
        post: std::ptr::null_mut(),
    },
    ngx_command_t {
        name: ngx_string!("rss_http_allow"),
        type_: (NGX_HTTP_LOC_CONF | NGX_CONF_TAKE2) as ngx_uint_t,
        set: Some(set_rss_http_allow),
        conf: NGX_HTTP_LOC_CONF_OFFSET,
        offset: 0,
        post: std::ptr::null_mut(),
    },
    ngx_command_t {
        name: ngx_string!("rss_http_allow_private"),
        type_: (NGX_HTTP_LOC_CONF | NGX_CONF_FLAG) as ngx_uint_t,
        set: Some(set_rss_http_allow_private),
        conf: NGX_HTTP_LOC_CONF_OFFSET,
        offset: 0,
        post: std::ptr::null_mut(),
    },
    ngx_command_t {
        name: ngx_string!("rss_sqlite_root"),
        type_: (NGX_HTTP_LOC_CONF | NGX_CONF_TAKE1) as ngx_uint_t,
        set: Some(set_rss_sqlite_root),
        conf: NGX_HTTP_LOC_CONF_OFFSET,
        offset: 0,
        post: std::ptr::null_mut(),
    },
    ngx_command_t::empty(),
];

extern "C" fn set_rss_shared_dict(
    cf: *mut ngx_conf_t,
    _cmd: *mut ngx_command_t,
    _conf: *mut core::ffi::c_void,
) -> *mut core::ffi::c_char {
    // SAFETY: NGINX invokes TAKE2 handlers with a live conf and three args.
    let args: &[ngx::ffi::ngx_str_t] = unsafe { (*(*cf).args).as_slice() };
    if args.len() != 3 {
        ngx_conf_log_error!(
            NGX_LOG_EMERG,
            cf,
            "invalid number of arguments in \"rss_shared_dict\""
        );
        return NGX_CONF_ERROR;
    }

    let mut name: nginx_sys::ngx_str_t = args[1];
    let zone_name = match name.to_str() {
        Ok(value) => value.to_string(),
        Err(_) => {
            ngx_conf_log_error!(
                NGX_LOG_EMERG,
                cf,
                "rss_shared_dict: zone name is not valid UTF-8"
            );
            return NGX_CONF_ERROR;
        }
    };
    if let Err(err) = crate::shared_dict::validate_zone_name(&zone_name) {
        ngx_conf_log_error!(NGX_LOG_EMERG, cf, "rss_shared_dict: {err}");
        return NGX_CONF_ERROR;
    }

    let mut size_arg = args[2];
    let size = unsafe { nginx_sys::ngx_parse_size(&raw mut size_arg) };
    if size < crate::shared_dict::MIN_ZONE_BYTES as isize {
        ngx_conf_log_error!(
            NGX_LOG_EMERG,
            cf,
            "rss_shared_dict: invalid size (minimum 8k)"
        );
        return NGX_CONF_ERROR;
    }

    let zone = unsafe {
        nginx_sys::ngx_shared_memory_add(
            cf,
            &raw mut name,
            size as usize,
            core::ptr::addr_of_mut!(crate::ngx_http_rustscript_module).cast(),
        )
    };
    let Some(zone) = (unsafe { zone.as_mut() }) else {
        return NGX_CONF_ERROR;
    };
    if !zone.data.is_null() {
        ngx_conf_log_error!(
            NGX_LOG_EMERG,
            cf,
            "rss_shared_dict \"{zone_name}\" is already defined"
        );
        return NGX_CONF_ERROR;
    }
    #[cfg(not(ngx_features = "rustc-test"))]
    {
        if crate::shared_dict::attach_shm_zone(cf, zone, name).is_err() {
            ngx_conf_log_error!(NGX_LOG_EMERG, cf, "rss_shared_dict: no memory");
            return NGX_CONF_ERROR;
        }
    }
    let _ = zone;
    NGX_CONF_OK
}

extern "C" fn set_rss_io_root(
    cf: *mut ngx_conf_t,
    _cmd: *mut ngx_command_t,
    conf: *mut core::ffi::c_void,
) -> *mut core::ffi::c_char {
    let loc = unsafe { &mut *conf.cast::<LocationConf>() };
    if loc.io_roots.is_some() {
        return duplicate_directive(cf, "rss_io_root");
    }
    let Some(path) = resolve_conf_path(cf, 1, "rss_io_root") else {
        return NGX_CONF_ERROR;
    };
    loc.add_io_root(path);
    NGX_CONF_OK
}

extern "C" fn set_rss_io_write(
    cf: *mut ngx_conf_t,
    _cmd: *mut ngx_command_t,
    conf: *mut core::ffi::c_void,
) -> *mut core::ffi::c_char {
    let loc = unsafe { &mut *conf.cast::<LocationConf>() };
    if loc.io_write.is_some() {
        return duplicate_directive(cf, "rss_io_write");
    }
    let Some(value) = parse_flag(cf, "rss_io_write") else {
        return NGX_CONF_ERROR;
    };
    loc.set_io_write(value);
    NGX_CONF_OK
}

extern "C" fn set_rss_http_scheme(
    cf: *mut ngx_conf_t,
    _cmd: *mut ngx_command_t,
    conf: *mut core::ffi::c_void,
) -> *mut core::ffi::c_char {
    let args: &[ngx::ffi::ngx_str_t] = unsafe { (*(*cf).args).as_slice() };
    let scheme = match args.get(1).and_then(|arg| arg.to_str().ok()) {
        Some("http") => "http",
        Some("https") => "https",
        _ => {
            ngx_conf_log_error!(NGX_LOG_EMERG, cf, "rss_http_scheme: expected http or https");
            return NGX_CONF_ERROR;
        }
    };
    let loc = unsafe { &mut *conf.cast::<LocationConf>() };
    loc.add_http_scheme(scheme.to_string());
    NGX_CONF_OK
}

extern "C" fn set_rss_http_allow(
    cf: *mut ngx_conf_t,
    _cmd: *mut ngx_command_t,
    conf: *mut core::ffi::c_void,
) -> *mut core::ffi::c_char {
    let args: &[ngx::ffi::ngx_str_t] = unsafe { (*(*cf).args).as_slice() };
    let host = match args.get(1).and_then(|arg| arg.to_str().ok()) {
        Some(host) if !host.is_empty() => host.to_string(),
        _ => {
            ngx_conf_log_error!(NGX_LOG_EMERG, cf, "rss_http_allow: invalid host");
            return NGX_CONF_ERROR;
        }
    };
    let port = match args
        .get(2)
        .and_then(|arg| arg.to_str().ok())
        .and_then(|value| value.parse::<u16>().ok())
    {
        Some(port) if port != 0 => port,
        _ => {
            ngx_conf_log_error!(NGX_LOG_EMERG, cf, "rss_http_allow: invalid port");
            return NGX_CONF_ERROR;
        }
    };
    let loc = unsafe { &mut *conf.cast::<LocationConf>() };
    if !loc.try_set_http_allow(host, port) {
        return duplicate_directive(cf, "rss_http_allow");
    }
    NGX_CONF_OK
}

extern "C" fn set_rss_http_allow_private(
    cf: *mut ngx_conf_t,
    _cmd: *mut ngx_command_t,
    conf: *mut core::ffi::c_void,
) -> *mut core::ffi::c_char {
    let loc = unsafe { &mut *conf.cast::<LocationConf>() };
    if loc.http_allow_private.is_some() {
        return duplicate_directive(cf, "rss_http_allow_private");
    }
    let Some(value) = parse_flag(cf, "rss_http_allow_private") else {
        return NGX_CONF_ERROR;
    };
    loc.set_http_allow_private(value);
    NGX_CONF_OK
}

extern "C" fn set_rss_sqlite_root(
    cf: *mut ngx_conf_t,
    _cmd: *mut ngx_command_t,
    conf: *mut core::ffi::c_void,
) -> *mut core::ffi::c_char {
    let loc = unsafe { &mut *conf.cast::<LocationConf>() };
    if loc.sqlite_root.is_some() {
        return duplicate_directive(cf, "rss_sqlite_root");
    }
    let Some(path) = resolve_conf_path(cf, 1, "rss_sqlite_root") else {
        return NGX_CONF_ERROR;
    };
    loc.set_sqlite_root(path);
    NGX_CONF_OK
}

fn parse_flag(cf: *mut ngx_conf_t, directive: &str) -> Option<bool> {
    let args: &[ngx::ffi::ngx_str_t] = unsafe { (*(*cf).args).as_slice() };
    match args.get(1).and_then(|arg| arg.to_str().ok()) {
        Some("on") => Some(true),
        Some("off") => Some(false),
        _ => {
            ngx_conf_log_error!(NGX_LOG_EMERG, cf, "{directive}: expected on or off");
            None
        }
    }
}

fn resolve_conf_path(cf: *mut ngx_conf_t, index: usize, directive: &str) -> Option<String> {
    let args: &[ngx::ffi::ngx_str_t] = unsafe { (*(*cf).args).as_slice() };
    let mut name = *args.get(index)?;
    let resolved = unsafe { nginx_sys::ngx_conf_full_name((*cf).cycle, &mut name, 1) };
    if resolved != ngx::core::Status::NGX_OK.0 {
        ngx_conf_log_error!(NGX_LOG_EMERG, cf, "{directive}: failed to resolve path");
        return None;
    }
    match name.to_str() {
        Ok(path) if !path.is_empty() => Some(path.to_string()),
        _ => {
            ngx_conf_log_error!(NGX_LOG_EMERG, cf, "{directive}: invalid path");
            None
        }
    }
}

fn duplicate_directive(cf: *mut ngx_conf_t, directive: &str) -> *mut core::ffi::c_char {
    ngx_conf_log_error!(
        NGX_LOG_EMERG,
        cf,
        "duplicate {directive} directive in this location"
    );
    NGX_CONF_ERROR
}

extern "C" fn set_access_by_rss_file(
    cf: *mut ngx_conf_t,
    _cmd: *mut ngx_command_t,
    conf: *mut core::ffi::c_void,
) -> *mut core::ffi::c_char {
    set_phase_by_rss_file(cf, conf, NgxPhase::Access, "access_by_rss_file")
}

extern "C" fn set_content_by_rss_file(
    cf: *mut ngx_conf_t,
    _cmd: *mut ngx_command_t,
    conf: *mut core::ffi::c_void,
) -> *mut core::ffi::c_char {
    set_phase_by_rss_file(cf, conf, NgxPhase::Content, "content_by_rss_file")
}

extern "C" fn set_log_by_rss_file(
    cf: *mut ngx_conf_t,
    _cmd: *mut ngx_command_t,
    conf: *mut core::ffi::c_void,
) -> *mut core::ffi::c_char {
    set_phase_by_rss_file(cf, conf, NgxPhase::Log, "log_by_rss_file")
}

fn set_phase_by_rss_file(
    cf: *mut ngx_conf_t,
    conf: *mut core::ffi::c_void,
    phase: NgxPhase,
    directive: &'static str,
) -> *mut core::ffi::c_char {
    // SAFETY: NGINX invokes the setter with a live conf, command, and location conf.
    let loc = unsafe { &mut *conf.cast::<LocationConf>() };
    let already_set = match phase {
        NgxPhase::Content => loc.content_program().is_some(),
        NgxPhase::Log => loc.log_program().is_some(),
        _ => loc.access_program().is_some(),
    };
    if already_set {
        ngx_conf_log_error!(
            NGX_LOG_EMERG,
            cf,
            "duplicate {directive} directive in this location"
        );
        return NGX_CONF_ERROR;
    }

    let args: &[ngx::ffi::ngx_str_t] = unsafe { (*(*cf).args).as_slice() };
    if args.len() != 2 {
        ngx_conf_log_error!(
            NGX_LOG_EMERG,
            cf,
            "invalid number of arguments in \"{directive}\""
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
            "{directive}: failed to resolve script path"
        );
        return NGX_CONF_ERROR;
    }

    let path = match name.to_str() {
        Ok(path) if !path.is_empty() => Path::new(path),
        Ok(_) => {
            ngx_conf_log_error!(NGX_LOG_EMERG, cf, "{directive}: empty script path");
            return NGX_CONF_ERROR;
        }
        Err(_) => {
            ngx_conf_log_error!(
                NGX_LOG_EMERG,
                cf,
                "{directive}: script path is not valid UTF-8"
            );
            return NGX_CONF_ERROR;
        }
    };

    match load_and_compile_rss_file_for_phase(path, phase) {
        Ok(program) => {
            let set = match phase {
                NgxPhase::Content => loc.set_content_program(program),
                NgxPhase::Log => loc.set_log_program(program),
                _ => loc.set_access_program(program),
            };
            match set {
                Ok(()) => NGX_CONF_OK,
                Err(err) => {
                    ngx_conf_log_error!(NGX_LOG_EMERG, cf, "{directive}: {err}");
                    NGX_CONF_ERROR
                }
            }
        }
        Err(err) => {
            ngx_conf_log_error!(NGX_LOG_EMERG, cf, "{directive}: {err}");
            NGX_CONF_ERROR
        }
    }
}
