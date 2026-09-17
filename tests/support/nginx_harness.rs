//! Typed, reusable harness for real-NGINX integration tests.
//!
//! The harness owns four layers that used to be copied into every live NGINX
//! test file:
//!
//! * [`NginxConfig`] builds the top-level `main`/`events`/`http`/`server`
//!   skeleton, always quoting interpolated paths through `nginx_quote_path`.
//! * [`NginxHarness`] owns an isolated prefix, allocates a loopback port and
//!   writes fixtures without ever leaving the prefix.
//! * [`RunningNginx`] is RAII: readiness-then-return startup, worker pid
//!   observation, `reload` with worker-replacement deadlines, graceful quit,
//!   kill escalation, diagnostics with master stderr/error log, prefix cleanup.
//! * `ConfigCase`/`HttpCase`/`RequestStep` express named, data-driven cases on
//!   top of those layers for the common `nginx -t` and HTTP scenarios.
//!
//! Layers are additive: complex concurrency, reload and disconnect tests use
//! the lifecycle layer directly, while plain scenarios use the typed cases.

#![allow(dead_code)]

use std::fs;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::artifact::module_cdylib;
use super::nginx::{
    combined, nginx_binary, nginx_process_group_command, nginx_quote_path, nginx_test,
    render_nginx_conf, terminate_process_group,
};
use super::temp::{OwnedTempDir, isolated_prefix};

/// Default `worker_processes` written into the structured skeleton.
pub const DEFAULT_WORKER_PROCESSES: u16 = 1;
/// `worker_connections` used by the structured `events` block.
pub const DEFAULT_WORKER_CONNECTIONS: u16 = 32;
/// Default `error_log` level; verbose enough to diagnose failures from the log.
pub const DEFAULT_ERROR_LOG_LEVEL: &str = "info";
/// Loopback address every generated `listen` directive uses.
pub const LOOPBACK: &str = "127.0.0.1";
/// Prefix-relative path of the generated config inside a harness prefix.
pub const CONFIG_REL_PATH: &str = "conf/nginx.conf";

/// Structured NGINX configuration skeleton.
///
/// Only the parts every live test repeats are structured; module specific
/// directives stay verbatim fragments so the builder never guesses semantics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NginxConfig {
    worker_processes: u16,
    master_process: bool,
    error_log_level: String,
    main_directives: String,
    events_directives: String,
    http_directives: String,
    server_directives: String,
    full_config: Option<String>,
    structured_touched: bool,
}

impl Default for NginxConfig {
    fn default() -> Self {
        Self {
            worker_processes: DEFAULT_WORKER_PROCESSES,
            master_process: false,
            error_log_level: DEFAULT_ERROR_LOG_LEVEL.to_string(),
            main_directives: String::new(),
            events_directives: String::new(),
            http_directives: String::new(),
            server_directives: String::new(),
            full_config: None,
            structured_touched: false,
        }
    }
}

/// Append `fragment` to `out`, indenting every non-empty line by `indent`.
fn push_fragment(out: &mut String, fragment: &str, indent: &str) {
    for line in fragment.trim().lines() {
        let line = line.trim_end();
        if line.is_empty() {
            out.push('\n');
            continue;
        }
        out.push_str(indent);
        out.push_str(line);
        out.push('\n');
    }
}

/// Interpolate `@@MODULE@@`/`@@PREFIX@@`/`@@PORT@@` in a directive fragment.
fn render_fragment(
    fragment: &str,
    prefix: &Path,
    module: &Path,
    port: u16,
) -> Result<String, String> {
    Ok(render_nginx_conf(fragment, prefix, module)?.replace("@@PORT@@", &port.to_string()))
}

impl NginxConfig {
    pub fn new() -> Self {
        Self::default()
    }

    /// `worker_processes` directive value.
    pub fn workers(mut self, workers: u16) -> Self {
        self.worker_processes = workers;
        self.mark_structured();
        self
    }

    /// `master_process on|off`.
    pub fn master_process(mut self, on: bool) -> Self {
        self.master_process = on;
        self.mark_structured();
        self
    }

    /// Level of the prefix-local `error_log`.
    pub fn error_log_level(mut self, level: &str) -> Self {
        self.error_log_level = level.to_string();
        self.mark_structured();
        self
    }

    /// Extra directives in the `main` context.
    pub fn main_directives(mut self, directives: &str) -> Self {
        self.main_directives = directives.to_string();
        self.mark_structured();
        self
    }

    /// Extra directives inside `events { ... }`.
    pub fn events_directives(mut self, directives: &str) -> Self {
        self.events_directives = directives.to_string();
        self.mark_structured();
        self
    }

    /// Extra directives inside `http { ... }`.
    pub fn http_directives(mut self, directives: &str) -> Self {
        self.http_directives = directives.to_string();
        self.mark_structured();
        self
    }

    /// Extra directives inside the generated `server { ... }` block.
    pub fn server_directives(mut self, directives: &str) -> Self {
        self.server_directives = directives.to_string();
        self.mark_structured();
        self
    }

    /// Complete config text; mutually exclusive with every structured field.
    ///
    /// `@@MODULE@@`, `@@PREFIX@@` and `@@PORT@@` interpolate like the shared
    /// `tests/fixtures` templates, so paths stay quoted through
    /// `nginx_quote_path`.
    pub fn full_config(mut self, text: &str) -> Self {
        self.full_config = Some(text.to_string());
        self
    }

    fn mark_structured(&mut self) {
        self.structured_touched = true;
    }

    /// Path of the `pid` file inside `prefix` (written into the config).
    pub fn pid_path(prefix: &Path) -> PathBuf {
        prefix.join("logs/nginx.pid")
    }

    /// Path of the `error_log` file inside `prefix` (written into the config).
    pub fn error_log_path(prefix: &Path) -> PathBuf {
        prefix.join("logs/error.log")
    }

    /// Path of the `access_log` file inside `prefix` (written into the config).
    pub fn access_log_path(prefix: &Path) -> PathBuf {
        prefix.join("logs/access.log")
    }

    /// Render the full config for `module`, `prefix` and `port`.
    ///
    /// Every interpolated path is quoted with `nginx_quote_path`, so callers
    /// never get an unescaped path substitution.
    pub fn render(&self, module: &Path, prefix: &Path, port: u16) -> Result<String, String> {
        if let Some(full) = &self.full_config {
            if self.structured_touched {
                return Err(
                    "full_config cannot be combined with structured NginxConfig fields; \
                     use either full_config or the structured builders"
                        .to_string(),
                );
            }
            let mut text = render_nginx_conf(full, prefix, module)?;
            text = text.replace("@@PORT@@", &port.to_string());
            if !text.ends_with('\n') {
                text.push('\n');
            }
            return Ok(text);
        }

        let module_path = module;
        let module = nginx_quote_path(module_path)?;
        let pid = nginx_quote_path(&Self::pid_path(prefix))?;
        let error_log = nginx_quote_path(&Self::error_log_path(prefix))?;
        let access_log = nginx_quote_path(&Self::access_log_path(prefix))?;
        let master_process = if self.master_process { "on" } else { "off" };
        let main_directives = render_fragment(&self.main_directives, prefix, module_path, port)?;
        let events_directives =
            render_fragment(&self.events_directives, prefix, module_path, port)?;
        let http_directives = render_fragment(&self.http_directives, prefix, module_path, port)?;
        let server_directives =
            render_fragment(&self.server_directives, prefix, module_path, port)?;

        let mut out = String::new();
        out.push_str(&format!("worker_processes {};\n", self.worker_processes));
        out.push_str(&format!(
            "error_log {error_log} {};\n",
            self.error_log_level
        ));
        out.push_str(&format!("pid {pid};\n"));
        out.push_str("daemon off;\n");
        out.push_str(&format!("master_process {master_process};\n"));
        out.push('\n');
        out.push_str(&format!("load_module {module};\n"));
        push_fragment(&mut out, &main_directives, "");
        out.push('\n');
        out.push_str("events {\n");
        out.push_str(&format!(
            "    worker_connections {};\n",
            DEFAULT_WORKER_CONNECTIONS
        ));
        push_fragment(&mut out, &events_directives, "    ");
        out.push_str("}\n\n");
        out.push_str("http {\n");
        out.push_str(&format!("    access_log {access_log};\n"));
        push_fragment(&mut out, &http_directives, "    ");
        out.push_str("    server {\n");
        out.push_str(&format!("        listen {LOOPBACK}:{port};\n"));
        out.push_str("        server_name localhost;\n");
        push_fragment(&mut out, &server_directives, "        ");
        out.push_str("    }\n");
        out.push_str("}\n");
        Ok(out)
    }
}

/// Allocate a loopback port by binding to port 0 and releasing it.
fn free_loopback_port() -> u16 {
    TcpListener::bind((LOOPBACK, 0))
        .unwrap_or_else(|err| panic!("bind loopback for a free port: {err}"))
        .local_addr()
        .unwrap_or_else(|err| panic!("local_addr: {err}"))
        .port()
}

/// Validate a fixture path and resolve it against `prefix`.
///
/// Rejects absolute paths, `.`/`..` components, empty or directory-shaped
/// paths and control characters; the caller only has to check the symlink
/// escape afterwards.
fn resolve_relative_path(prefix: &Path, relative: &str) -> Result<PathBuf, String> {
    if relative.is_empty() {
        return Err("fixture path is empty".to_string());
    }
    if relative.bytes().any(|byte| byte == 0) {
        return Err(format!("fixture path {relative:?} contains NUL"));
    }
    if relative.chars().any(|c| c == '\n' || c == '\r') {
        return Err(format!("fixture path {relative:?} contains a newline"));
    }
    if relative.chars().any(|c| (c as u32) < 0x20) {
        return Err(format!(
            "fixture path {relative:?} contains a control character"
        ));
    }
    if relative.ends_with('/') {
        return Err(format!(
            "fixture path {relative:?} must include a file name, not a directory path"
        ));
    }
    let candidate = Path::new(relative);
    if candidate.is_absolute() {
        return Err(format!(
            "fixture path {relative:?} must stay relative to the harness prefix (absolute path rejected)"
        ));
    }
    for component in candidate.components() {
        match component {
            Component::Normal(_) => {}
            Component::CurDir => {
                return Err(format!(
                    "fixture path {relative:?} must not contain current-directory components"
                ));
            }
            Component::ParentDir => {
                return Err(format!(
                    "fixture path {relative:?} must not contain parent-directory components"
                ));
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(format!(
                    "fixture path {relative:?} must stay relative to the harness prefix (absolute path rejected)"
                ));
            }
        }
    }
    Ok(prefix.join(candidate))
}

/// One isolated NGINX prefix plus the pinned binary, module and port to use.
///
/// The prefix is created on construction and removed by `OwnedTempDir` when the
/// last handle (harness or [`RunningNginx`]) is dropped.
#[derive(Debug, Clone)]
pub struct NginxHarness {
    label: String,
    nginx: PathBuf,
    module: PathBuf,
    port: u16,
    ready_timeout: Duration,
    prefix: Arc<OwnedTempDir>,
}

impl NginxHarness {
    /// Canonical pinned NGINX binary, crate cdylib and a fresh isolated prefix.
    pub fn new(label: &str) -> Self {
        Self::with_binary_and_module(label, nginx_binary(), module_cdylib())
    }

    /// Harness over explicit binary/module paths.
    ///
    /// Used by path-safety tests and by failure-path tests that need a stub
    /// binary; unlike [`NginxHarness::new`] this does not validate the NGINX
    /// version banner or configure arguments.
    pub fn with_binary_and_module(label: &str, nginx: PathBuf, module: PathBuf) -> Self {
        Self {
            label: label.to_string(),
            nginx,
            module,
            port: free_loopback_port(),
            ready_timeout: READY_TIMEOUT,
            prefix: Arc::new(isolated_prefix(label)),
        }
    }

    /// Replace the auto-allocated port (used to force bind conflicts).
    pub fn with_port(mut self, port: u16) -> Self {
        self.port = port;
        self
    }

    /// Replace the readiness timeout used by `start`/`try_start`.
    pub fn with_ready_timeout(mut self, timeout: Duration) -> Self {
        self.ready_timeout = timeout;
        self
    }

    pub fn label(&self) -> &str {
        &self.label
    }

    pub fn nginx(&self) -> &Path {
        &self.nginx
    }

    pub fn module(&self) -> &Path {
        &self.module
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn prefix(&self) -> &Path {
        self.prefix.as_ref()
    }

    /// Absolute path of the generated config inside the prefix.
    pub fn conf_path(&self) -> PathBuf {
        self.prefix().join(CONFIG_REL_PATH)
    }

    /// Write a fixture, creating parent directories inside the prefix.
    ///
    /// The deepest already-existing ancestor must resolve inside the prefix
    /// *before* any directory is created, so a symlinked parent can never be
    /// used to create or write anything outside the prefix.
    pub fn write_file(
        &self,
        relative: &str,
        contents: impl AsRef<[u8]>,
    ) -> Result<PathBuf, String> {
        let path = resolve_relative_path(self.prefix(), relative)?;
        let root = fs::canonicalize(self.prefix()).map_err(|err| {
            format!(
                "canonicalize harness prefix {}: {err}",
                self.prefix().display()
            )
        })?;
        let parent = path
            .parent()
            .ok_or_else(|| format!("fixture path {relative:?} has no parent directory"))?;
        let mut ancestor = Some(parent);
        while let Some(dir) = ancestor {
            if dir.exists() {
                let resolved = fs::canonicalize(dir)
                    .map_err(|err| format!("canonicalize fixture dir {}: {err}", dir.display()))?;
                if !resolved.starts_with(&root) {
                    return Err(format!(
                        "fixture path {relative:?} escapes the harness prefix {} through a symlink ({})",
                        self.prefix().display(),
                        resolved.display()
                    ));
                }
                break;
            }
            ancestor = dir.parent();
        }
        fs::create_dir_all(parent)
            .map_err(|err| format!("create fixture dir {}: {err}", parent.display()))?;
        let resolved = fs::canonicalize(parent)
            .map_err(|err| format!("canonicalize fixture dir {}: {err}", parent.display()))?;
        if !resolved.starts_with(&root) {
            return Err(format!(
                "fixture path {relative:?} escapes the harness prefix {} (symlink {})",
                self.prefix().display(),
                resolved.display()
            ));
        }
        fs::write(&path, contents.as_ref())
            .map_err(|err| format!("write fixture {}: {err}", path.display()))?;
        Ok(path)
    }

    /// Render `config` for this harness (prefix, module and port).
    pub fn render_config(&self, config: &NginxConfig) -> Result<String, String> {
        config.render(&self.module, self.prefix(), self.port)
    }

    /// Render and write `conf/nginx.conf`, returning the written path.
    pub fn try_write_config(&self, config: &NginxConfig) -> Result<PathBuf, String> {
        let text = self.render_config(config)?;
        self.write_file(CONFIG_REL_PATH, text.as_bytes())
    }

    /// Render and write `conf/nginx.conf`, panicking with harness context.
    pub fn write_config(&self, config: &NginxConfig) -> PathBuf {
        self.try_write_config(config)
            .unwrap_or_else(|err| panic!("[{}] write nginx.conf: {err}", self.label))
    }
}

/// Read a log file lossily; missing files yield an empty string.
fn read_log(path: &Path) -> String {
    match fs::read(path) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(_) => String::new(),
    }
}

/// Outcome of `nginx -t` for a written harness config.
#[derive(Debug, Clone)]
pub struct ConfigCheck {
    pub success: bool,
    /// Combined stdout and stderr of `nginx -t`.
    pub output: String,
    pub conf_path: PathBuf,
    pub error_log_path: PathBuf,
    /// Current content of the config's `error_log` file (empty when absent).
    pub error_log: String,
}

impl ConfigCheck {
    /// One-line-per-section report used in failure messages.
    pub fn report(&self) -> String {
        format!(
            "config: {}\nstatus: {}\nstdout/stderr:\n{}\nerror log ({}):\n{}",
            self.conf_path.display(),
            if self.success { "ok" } else { "failed" },
            self.output,
            self.error_log_path.display(),
            self.error_log,
        )
    }

    pub fn require_success(&self) -> Result<(), String> {
        if self.success {
            return Ok(());
        }
        Err(format!("nginx -t rejected the config\n{}", self.report()))
    }

    pub fn require_failure(&self) -> Result<(), String> {
        if self.success {
            return Err(format!(
                "nginx -t accepted a config that must fail\n{}",
                self.report()
            ));
        }
        Ok(())
    }
}

impl NginxHarness {
    /// Run `nginx -t` against the written `conf/nginx.conf`.
    pub fn check_config(&self) -> ConfigCheck {
        let conf_path = self.conf_path();
        if !conf_path.is_file() {
            panic!(
                "[{}] check_config needs a written config; call write_config first ({})",
                self.label,
                conf_path.display()
            );
        }
        let output = nginx_test(&self.nginx, self.prefix());
        let error_log_path = NginxConfig::error_log_path(self.prefix());
        ConfigCheck {
            success: output.status.success(),
            output: combined(&output),
            conf_path,
            error_log: read_log(&error_log_path),
            error_log_path,
        }
    }
}

/// Timeout for the readiness loop (`start`).
///
/// Master can listen before worker `init_process` finishes installing the host
/// registry (~a few seconds in debug). Readiness therefore waits for an HTTP
/// response, not only a TCP accept.
pub const READY_TIMEOUT: Duration = Duration::from_secs(15);
/// Timeout granted to `SIGQUIT` before `terminate` escalates to `SIGKILL`.
pub const GRACEFUL_QUIT_TIMEOUT: Duration = Duration::from_secs(5);
/// Timeout used by `Drop` and `terminate` for the kill escalation.
pub const TERMINATE_TIMEOUT: Duration = Duration::from_secs(5);
const START_POLL_INTERVAL: Duration = Duration::from_millis(20);

/// Name of the captured master stdout/stderr files inside `logs/`.
pub const MASTER_STDERR_REL_PATH: &str = "logs/master.stderr";
pub const MASTER_STDOUT_REL_PATH: &str = "logs/master.stdout";

fn process_group_alive(pgid: i32) -> bool {
    let rc = unsafe { libc::kill(-pgid, 0) };
    rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

fn signal_process_group(pgid: i32, signal: i32) {
    let _ = unsafe { libc::kill(-pgid, signal) };
}

fn loopback_socket(port: u16) -> SocketAddr {
    SocketAddr::from((Ipv4Addr::LOCALHOST, port))
}

/// Direct children of `pid` as listed by `/proc`.
fn child_pids(pid: u32) -> Result<Vec<u32>, String> {
    let path = format!("/proc/{pid}/task/{pid}/children");
    let text = fs::read_to_string(&path).map_err(|err| format!("read {path}: {err}"))?;
    text.split_whitespace()
        .map(|token| {
            token
                .parse::<u32>()
                .map_err(|err| format!("parse child pid {token:?} from {path}: {err}"))
        })
        .collect()
}

/// Live NGINX worker children, excluding cache managers/loaders and helpers.
fn nginx_worker_pids(master_pid: u32) -> Result<Vec<u32>, String> {
    let mut workers = Vec::new();
    for pid in child_pids(master_pid)? {
        let path = format!("/proc/{pid}/cmdline");
        match fs::read(&path) {
            Ok(cmdline) => {
                let title = String::from_utf8_lossy(&cmdline).replace('\0', " ");
                if title.starts_with("nginx: worker process") {
                    workers.push(pid);
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                // The child exited between reading `children` and `cmdline`.
            }
            Err(err) => return Err(format!("read {path}: {err}")),
        }
    }
    workers.sort_unstable();
    Ok(workers)
}

/// Outcome of one successful [`RunningNginx::reload`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReloadOutcome {
    /// Combined stdout/stderr of the `nginx -s reload` command.
    pub command_output: String,
    /// Master pid; a reload must never replace the master.
    pub master_pid: u32,
    /// Worker pids that were live before the reload command ran.
    pub previous_workers: Vec<u32>,
    /// Worker pids that replaced them.
    pub new_workers: Vec<u32>,
}

/// Outcome of one bounded graceful shutdown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuitReport {
    /// Wall time from the `SIGQUIT` request to reaping the child.
    pub elapsed: Duration,
    /// Reaped exit status, or `None` when the child had already exited.
    pub exit_status: Option<ExitStatus>,
    /// `true` when `SIGQUIT` was delivered to a live process group.
    pub signalled: bool,
}

/// A started, readiness-checked NGINX in its own process group.
///
/// Dropping it stops the process group with `SIGQUIT` escalated to `SIGKILL`
/// and releases the prefix (removed once the harness handle is gone too).
#[derive(Debug)]
pub struct RunningNginx {
    label: String,
    port: u16,
    pid: u32,
    prefix: Arc<OwnedTempDir>,
    nginx: PathBuf,
    child: Option<Child>,
    exit: Option<ExitStatus>,
    stderr_log: PathBuf,
    stdout_log: PathBuf,
}

impl RunningNginx {
    pub fn label(&self) -> &str {
        &self.label
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    pub fn prefix(&self) -> &Path {
        self.prefix.as_ref()
    }

    /// Path of the captured master `stderr` (inside the prefix).
    pub fn stderr_log_path(&self) -> &Path {
        &self.stderr_log
    }

    /// Path of the captured master `stdout` (inside the prefix).
    pub fn stdout_log_path(&self) -> &Path {
        &self.stdout_log
    }

    /// Path of the `error_log` file configured by [`NginxConfig`].
    pub fn error_log_path(&self) -> PathBuf {
        NginxConfig::error_log_path(self.prefix())
    }

    /// Absolute path of the config this process was started with.
    pub fn conf_path(&self) -> PathBuf {
        self.prefix().join(CONFIG_REL_PATH)
    }

    fn try_reap(&mut self) -> Option<ExitStatus> {
        if self.exit.is_some() {
            return self.exit;
        }
        let child = self.child.as_mut()?;
        match child.try_wait() {
            Ok(Some(status)) => {
                self.exit = Some(status);
                self.child = None;
                self.exit
            }
            _ => None,
        }
    }

    /// `true` while the process is known to be running (reaps when it exited).
    pub fn is_running(&mut self) -> bool {
        self.try_reap().is_none()
    }

    /// Exit status once the process has been reaped.
    pub fn exit_status(&mut self) -> Option<ExitStatus> {
        self.try_reap()
    }

    /// Diagnostics for a startup/shutdown failure: status, stderr, error log.
    fn failure_report(&self, reason: &str, status: Option<ExitStatus>) -> String {
        let status = match status {
            Some(status) => status.to_string(),
            None => "still running".to_string(),
        };
        format!(
            "[{}] nginx {reason} on 127.0.0.1:{}\n\
             status: {status}\n\
             prefix: {}\n\
             config: {}\n\
             master stderr ({}):\n{}\n\
             error log ({}):\n{}",
            self.label,
            self.port,
            self.prefix().display(),
            self.conf_path().display(),
            self.stderr_log.display(),
            read_log(&self.stderr_log),
            self.error_log_path().display(),
            read_log(&self.error_log_path()),
        )
    }

    /// Wait until the child process is gone, polling `try_wait`.
    fn wait_for_exit(&mut self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            if self.try_reap().is_some() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(START_POLL_INTERVAL);
        }
    }

    /// Ask the process group to shut down gracefully (`SIGQUIT`) and wait.
    ///
    /// The report carries the elapsed time and the reaped exit status, so a
    /// caller can assert that shutdown stayed bounded even while a request was
    /// in flight. A child that outlives `timeout` yields `Err` with the usual
    /// status/stderr/error-log diagnostics instead of a silent success.
    pub fn graceful_quit(&mut self, timeout: Duration) -> Result<QuitReport, String> {
        let started = Instant::now();
        if let Some(status) = self.try_reap() {
            return Ok(QuitReport {
                elapsed: started.elapsed(),
                exit_status: Some(status),
                signalled: false,
            });
        }
        signal_process_group(self.pid as i32, libc::SIGQUIT);
        if self.wait_for_exit(timeout) {
            return Ok(QuitReport {
                elapsed: started.elapsed(),
                exit_status: self.exit,
                signalled: true,
            });
        }
        Err(self.failure_report(
            &format!("did not exit after SIGQUIT within {timeout:?}"),
            None,
        ))
    }

    /// Stop the process group: `SIGQUIT`, then `SIGKILL` after `timeout`.
    pub fn terminate(&mut self, timeout: Duration) {
        let Some(mut child) = self.child.take() else {
            return;
        };
        terminate_process_group(&mut child, timeout);
        self.exit = child.try_wait().ok().flatten();
    }

    /// PIDs of the live worker processes: the direct children of the master.
    ///
    /// A `master_process off` NGINX is a single process and reports none.
    pub fn worker_pids(&self) -> Result<Vec<u32>, String> {
        nginx_worker_pids(self.pid).map_err(|err| {
            format!(
                "[{}] observe workers of master {}: {err}",
                self.label, self.pid
            )
        })
    }

    /// Reload the running configuration: `nginx -s reload`, then wait until the
    /// old workers are replaced by new ones.
    ///
    /// The returned [`ReloadOutcome`] carries the reload command output and the
    /// observed pid change. A command that fails, a master that exits, or a
    /// deadline that passes before the workers are replaced yields `Err` with
    /// the command output, both pid sets, the master status, stderr and the
    /// current error log: reload never reports success without a real worker
    /// replacement.
    pub fn reload(&mut self, timeout: Duration) -> Result<ReloadOutcome, String> {
        if let Some(status) = self.try_reap() {
            return Err(
                self.failure_report("cannot reload: the master already exited", Some(status))
            );
        }
        let observation_deadline = Instant::now() + timeout.min(Duration::from_secs(1));
        let mut previous_workers = self.worker_pids()?;
        loop {
            std::thread::sleep(START_POLL_INTERVAL);
            let observed = self.worker_pids()?;
            if !observed.is_empty() && observed == previous_workers {
                break;
            }
            previous_workers = observed;
            if Instant::now() >= observation_deadline {
                break;
            }
        }
        if previous_workers.is_empty() {
            return Err(format!(
                "[{}] reload needs at least one live worker; master {} has no children\n{}",
                self.label,
                self.pid,
                self.failure_report("no worker to replace", None)
            ));
        }

        let output = Command::new(&self.nginx)
            .arg("-s")
            .arg("reload")
            .arg("-p")
            .arg(self.prefix())
            .arg("-c")
            .arg(CONFIG_REL_PATH)
            .output()
            .map_err(|err| {
                format!(
                    "[{}] spawn nginx -s reload for prefix {}: {err}",
                    self.label,
                    self.prefix().display()
                )
            })?;
        let command_output = combined(&output);
        if !output.status.success() {
            let current_workers = self.worker_pids().unwrap_or_default();
            return Err(format!(
                "[{}] nginx -s reload failed with {}\n\
                 previous workers: {previous_workers:?}\n\
                 current workers: {current_workers:?}\n\
                 command output:\n{command_output}\n{}",
                self.label,
                output.status,
                self.failure_report("reload command failed", None)
            ));
        }

        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self.try_reap() {
                return Err(format!(
                    "[{}] the master exited while reloading\n\
                     command output:\n{command_output}\n{}",
                    self.label,
                    self.failure_report("exited during reload", Some(status))
                ));
            }
            let current_workers = self.worker_pids()?;
            if !current_workers.is_empty()
                && previous_workers
                    .iter()
                    .all(|pid| !current_workers.contains(pid))
            {
                return Ok(ReloadOutcome {
                    command_output,
                    master_pid: self.pid,
                    previous_workers,
                    new_workers: current_workers,
                });
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "[{}] reload did not replace the workers within {timeout:?}\n\
                     previous workers: {previous_workers:?}\n\
                     current workers: {current_workers:?}\n\
                     command output:\n{command_output}\n{}",
                    self.label,
                    self.failure_report("reload deadline passed", None)
                ));
            }
            std::thread::sleep(START_POLL_INTERVAL);
        }
    }
}

impl Drop for RunningNginx {
    fn drop(&mut self) {
        if self.child.is_none() {
            return;
        }
        if !std::thread::panicking() {
            // Normal path: try the graceful stop first, then escalate.
            let _ = self.graceful_quit(GRACEFUL_QUIT_TIMEOUT);
        }
        let Some(mut child) = self.child.take() else {
            return;
        };
        signal_process_group(self.pid as i32, libc::SIGQUIT);
        terminate_process_group(&mut child, TERMINATE_TIMEOUT);
        self.exit = child.try_wait().ok().flatten();
        if process_group_alive(self.pid as i32) && !std::thread::panicking() {
            panic!(
                "[{}] nginx process group {} survived SIGKILL; prefix {} is kept for inspection",
                self.label,
                self.pid,
                self.prefix().display()
            );
        }
    }
}

impl NginxHarness {
    /// Start NGINX foreground in its own process group and wait for readiness.
    pub fn start(&self) -> RunningNginx {
        self.try_start().unwrap_or_else(|err| panic!("{err}"))
    }

    /// `nginx -t` first, then [`NginxHarness::try_start_unchecked`].
    pub fn try_start(&self) -> Result<RunningNginx, String> {
        let check = self.check_config();
        check.require_success()?;
        self.try_start_unchecked()
    }

    /// Start without `nginx -t`; only for tests of startup-failure behaviour.
    pub fn start_unchecked(&self) -> RunningNginx {
        self.try_start_unchecked()
            .unwrap_or_else(|err| panic!("{err}"))
    }

    /// Spawn NGINX, capture master stdout/stderr and wait for readiness.
    pub fn try_start_unchecked(&self) -> Result<RunningNginx, String> {
        let stdout_log = self.prefix().join(MASTER_STDOUT_REL_PATH);
        let stderr_log = self.prefix().join(MASTER_STDERR_REL_PATH);
        let stdout = fs::File::create(&stdout_log)
            .map_err(|err| format!("[{}] create {}: {err}", self.label, stdout_log.display()))?;
        let stderr = fs::File::create(&stderr_log)
            .map_err(|err| format!("[{}] create {}: {err}", self.label, stderr_log.display()))?;

        let child = nginx_process_group_command(&self.nginx)
            .arg("-p")
            .arg(self.prefix())
            .arg("-c")
            .arg(CONFIG_REL_PATH)
            .stdin(Stdio::null())
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr))
            .spawn()
            .map_err(|err| {
                format!(
                    "[{}] spawn nginx {} -p {}: {err}",
                    self.label,
                    self.nginx.display(),
                    self.prefix().display()
                )
            })?;

        let mut running = RunningNginx {
            label: self.label.clone(),
            port: self.port,
            pid: child.id(),
            prefix: Arc::clone(&self.prefix),
            nginx: self.nginx.clone(),
            child: Some(child),
            exit: None,
            stderr_log,
            stdout_log,
        };
        if let Err(reason) = running.wait_for_readiness(self.ready_timeout) {
            let status = running.exit_status();
            return Err(running.failure_report(&reason, status));
        }
        Ok(running)
    }
}

impl RunningNginx {
    /// Poll `try_wait` until the worker answers HTTP, not only TCP.
    ///
    /// NGINX's master listen socket can accept connections before
    /// `init_process` finishes. Latency tests must not start until a worker
    /// has entered its event loop.
    fn wait_for_readiness(&mut self, timeout: Duration) -> Result<(), String> {
        let deadline = Instant::now() + timeout;
        let mut port_ready = false;
        let mut last = String::from("port not yet listening");
        loop {
            if self.try_reap().is_some() {
                return Err("exited before the worker answered HTTP".to_string());
            }
            if !port_ready
                && TcpStream::connect_timeout(&loopback_socket(self.port), START_POLL_INTERVAL)
                    .is_ok()
            {
                port_ready = true;
            }
            if port_ready {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Err(format!(
                        "timed out after {timeout:?} waiting for a worker HTTP response: {last}"
                    ));
                }
                let probe = HttpRequest::get("/")
                    .version(HttpVersion::Http10)
                    .header("Content-Length", "not-a-number")
                    .connect_timeout(Duration::from_millis(200))
                    .read_timeout(remaining.min(Duration::from_secs(2)))
                    .write_timeout(Duration::from_secs(1));
                match self.request(&probe) {
                    Ok(_) => match super::nginx::worker_ready_progress(&self.error_log_path()) {
                        Ok(()) => return Ok(()),
                        Err(err) => last = err,
                    },
                    Err(err) => last = err,
                }
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "timed out after {timeout:?} waiting for a worker HTTP response: {last}"
                ));
            }
            std::thread::sleep(START_POLL_INTERVAL);
        }
    }
}

/// HTTP protocol version used by typed requests and parsed responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HttpVersion {
    Http10,
    #[default]
    Http11,
}

impl HttpVersion {
    fn wire(self) -> &'static str {
        match self {
            Self::Http10 => "HTTP/1.0",
            Self::Http11 => "HTTP/1.1",
        }
    }

    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "HTTP/1.0" => Ok(Self::Http10),
            "HTTP/1.1" => Ok(Self::Http11),
            other => Err(format!("unsupported HTTP version {other:?}")),
        }
    }
}

fn contains_crlf(value: &str) -> bool {
    value.bytes().any(|byte| byte == b'\r' || byte == b'\n')
}

/// One HTTP request sent directly over a loopback `TcpStream`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpRequest {
    pub method: String,
    pub path: String,
    pub version: HttpVersion,
    /// Ordered, case-preserving headers. Lookup/override semantics are
    /// case-insensitive as required by HTTP.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub connect_timeout: Duration,
    pub read_timeout: Duration,
    pub write_timeout: Duration,
}

impl HttpRequest {
    pub fn new(method: &str, path: &str) -> Self {
        Self {
            method: method.to_string(),
            path: path.to_string(),
            version: HttpVersion::Http11,
            headers: Vec::new(),
            body: Vec::new(),
            connect_timeout: Duration::from_secs(3),
            read_timeout: Duration::from_secs(3),
            write_timeout: Duration::from_secs(3),
        }
    }

    pub fn get(path: &str) -> Self {
        Self::new("GET", path)
    }

    pub fn post(path: &str, body: Vec<u8>) -> Self {
        Self::new("POST", path).body(body)
    }

    pub fn version(mut self, version: HttpVersion) -> Self {
        self.version = version;
        self
    }

    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_string(), value.to_string()));
        self
    }

    pub fn body(mut self, body: impl Into<Vec<u8>>) -> Self {
        self.body = body.into();
        self
    }

    pub fn connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = timeout;
        self
    }

    pub fn read_timeout(mut self, timeout: Duration) -> Self {
        self.read_timeout = timeout;
        self
    }

    pub fn write_timeout(mut self, timeout: Duration) -> Self {
        self.write_timeout = timeout;
        self
    }

    fn has_header(&self, needle: &str) -> bool {
        self.headers
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case(needle))
    }

    /// Render request-line, headers and body. Host/Connection/Content-Length
    /// defaults are supplied exactly once unless the caller supplied an
    /// override (header names are compared case-insensitively).
    pub fn to_bytes(&self, port: u16) -> Result<Vec<u8>, String> {
        if self.method.is_empty()
            || self.method.bytes().any(|b| b <= b' ' || b >= 0x7f)
            || contains_crlf(&self.path)
            || self.path.is_empty()
        {
            return Err(format!(
                "unsafe HTTP request line method={:?} path={:?}",
                self.method, self.path
            ));
        }
        let mut out =
            format!("{} {} {}\r\n", self.method, self.path, self.version.wire()).into_bytes();
        for (name, value) in &self.headers {
            if name.is_empty()
                || name.bytes().any(|b| b <= b' ' || b == b':' || b >= 0x7f)
                || contains_crlf(value)
            {
                return Err(format!("unsafe HTTP header {name:?}: {value:?}"));
            }
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(b": ");
            out.extend_from_slice(value.as_bytes());
            out.extend_from_slice(b"\r\n");
        }
        if !self.has_header("Host") {
            out.extend_from_slice(format!("Host: {LOOPBACK}:{port}\r\n").as_bytes());
        }
        if !self.has_header("Connection") {
            out.extend_from_slice(b"Connection: close\r\n");
        }
        if !self.has_header("Content-Length") {
            out.extend_from_slice(format!("Content-Length: {}\r\n", self.body.len()).as_bytes());
        }
        out.extend_from_slice(b"\r\n");
        out.extend_from_slice(&self.body);
        Ok(out)
    }
}

/// Parsed HTTP response, preserving duplicate headers and binary response body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    pub version: HttpVersion,
    pub status: u16,
    pub reason: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn decode_chunked(mut wire: &[u8]) -> Result<Vec<u8>, String> {
    let mut body = Vec::new();
    loop {
        let line_end =
            find_bytes(wire, b"\r\n").ok_or_else(|| "truncated chunk size line".to_string())?;
        let size_text = std::str::from_utf8(&wire[..line_end])
            .map_err(|_| "chunk size is not ASCII/UTF-8".to_string())?;
        let size_text = size_text.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_text, 16)
            .map_err(|_| format!("invalid chunk size {size_text:?}"))?;
        wire = &wire[line_end + 2..];
        if size == 0 {
            // The final chunk may be followed by trailers, terminated by CRLF.
            if wire == b"\r\n" || find_bytes(wire, b"\r\n\r\n").is_some() {
                return Ok(body);
            }
            return Err("truncated chunk trailer terminator".to_string());
        }
        if wire.len() < size + 2 {
            return Err(format!(
                "truncated chunk: declared {size} bytes, only {} remain",
                wire.len().saturating_sub(2)
            ));
        }
        body.extend_from_slice(&wire[..size]);
        if &wire[size..size + 2] != b"\r\n" {
            return Err("chunk data missing CRLF terminator".to_string());
        }
        wire = &wire[size + 2..];
    }
}

impl HttpResponse {
    pub fn parse(wire: &[u8]) -> Result<Self, String> {
        let header_end = find_bytes(wire, b"\r\n\r\n")
            .ok_or_else(|| "HTTP response is missing the header terminator".to_string())?;
        let head = std::str::from_utf8(&wire[..header_end])
            .map_err(|_| "HTTP response headers are not valid UTF-8".to_string())?;
        let mut lines = head.split("\r\n");
        let status_line = lines
            .next()
            .ok_or_else(|| "HTTP response has no status line".to_string())?;
        let mut status_parts = status_line.splitn(3, ' ');
        let version = HttpVersion::parse(
            status_parts
                .next()
                .ok_or_else(|| "HTTP status line has no version".to_string())?,
        )?;
        let status_text = status_parts
            .next()
            .ok_or_else(|| "HTTP status line has no status".to_string())?;
        let status = status_text
            .parse::<u16>()
            .map_err(|_| format!("invalid HTTP status {status_text:?}"))?;
        let reason = status_parts.next().unwrap_or("").to_string();
        let mut headers = Vec::new();
        for line in lines {
            let (name, value) = line
                .split_once(':')
                .ok_or_else(|| format!("malformed HTTP header {line:?}"))?;
            headers.push((name.trim().to_string(), value.trim().to_string()));
        }
        let wire_body = &wire[header_end + 4..];
        let is_chunked = headers.iter().any(|(name, value)| {
            name.eq_ignore_ascii_case("Transfer-Encoding")
                && value
                    .split(',')
                    .any(|coding| coding.trim().eq_ignore_ascii_case("chunked"))
        });
        let body = if is_chunked {
            decode_chunked(wire_body)?
        } else if let Some((_, value)) = headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("Content-Length"))
        {
            let length = value
                .parse::<usize>()
                .map_err(|_| format!("invalid Content-Length {value:?}"))?;
            if wire_body.len() < length {
                return Err(format!(
                    "truncated HTTP body: Content-Length {length}, received {}",
                    wire_body.len()
                ));
            }
            wire_body[..length].to_vec()
        } else {
            wire_body.to_vec()
        };
        Ok(Self {
            version,
            status,
            reason,
            headers,
            body,
        })
    }

    /// All values for `name`, preserving header order and duplicates.
    pub fn header_values(&self, name: &str) -> Vec<&str> {
        self.headers
            .iter()
            .filter(|(header, _)| header.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
            .collect()
    }
}

/// Minimal string matcher shared by config output, response bodies and logs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Matcher {
    Exact(String),
    Contains(String),
    NotContains(String),
}

impl Matcher {
    pub fn exact(value: impl Into<String>) -> Self {
        Self::Exact(value.into())
    }

    pub fn contains(value: impl Into<String>) -> Self {
        Self::Contains(value.into())
    }

    pub fn not_contains(value: impl Into<String>) -> Self {
        Self::NotContains(value.into())
    }

    fn matches(&self, actual: &str) -> bool {
        match self {
            Self::Exact(expected) => actual == expected,
            Self::Contains(expected) => actual.contains(expected),
            Self::NotContains(forbidden) => !actual.contains(forbidden),
        }
    }
}

/// A prefix-relative fixture written before config checking/startup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestFile {
    pub relative_path: String,
    pub contents: Vec<u8>,
}

impl TestFile {
    pub fn text(relative_path: impl Into<String>, contents: impl Into<String>) -> Self {
        Self {
            relative_path: relative_path.into(),
            contents: contents.into().into_bytes(),
        }
    }

    pub fn bytes(relative_path: impl Into<String>, contents: impl Into<Vec<u8>>) -> Self {
        Self {
            relative_path: relative_path.into(),
            contents: contents.into(),
        }
    }
}

/// Named typed `nginx -t` case.
#[derive(Debug, Clone)]
pub struct ConfigCase {
    pub name: String,
    pub files: Vec<TestFile>,
    pub config: NginxConfig,
    pub expected_success: bool,
    /// Every matcher must match the combined stdout/stderr + error log.
    pub output_matchers: Vec<Matcher>,
    /// At least one of these fragments must occur when the list is non-empty.
    pub any_output_fragments: Vec<String>,
}

impl ConfigCase {
    pub fn new(name: impl Into<String>, config: NginxConfig) -> Self {
        Self {
            name: name.into(),
            files: Vec::new(),
            config,
            expected_success: true,
            output_matchers: Vec::new(),
            any_output_fragments: Vec::new(),
        }
    }

    pub fn file(mut self, file: TestFile) -> Self {
        self.files.push(file);
        self
    }

    pub fn expect_success(mut self) -> Self {
        self.expected_success = true;
        self
    }

    pub fn expect_failure(mut self) -> Self {
        self.expected_success = false;
        self
    }

    pub fn output(mut self, matcher: Matcher) -> Self {
        self.output_matchers.push(matcher);
        self
    }

    pub fn output_any<I, S>(mut self, fragments: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.any_output_fragments
            .extend(fragments.into_iter().map(Into::into));
        self
    }
}

fn write_case_files(
    harness: &NginxHarness,
    files: &[TestFile],
    case_name: &str,
) -> Result<(), String> {
    for file in files {
        harness
            .write_file(&file.relative_path, &file.contents)
            .map_err(|err| {
                format!(
                    "case {case_name:?}: write fixture {:?}: {err}",
                    file.relative_path
                )
            })?;
    }
    Ok(())
}

fn assert_matchers(
    case_name: &str,
    subject: &str,
    actual: &str,
    matchers: &[Matcher],
) -> Result<(), String> {
    for matcher in matchers {
        if !matcher.matches(actual) {
            return Err(format!(
                "case {case_name:?}: {subject} is missing required matcher {matcher:?}\n\
                 actual {subject}:\n{actual}"
            ));
        }
    }
    Ok(())
}

/// Run one named config case in a fresh isolated prefix.
pub fn run_config_case(case: &ConfigCase) -> Result<ConfigCheck, String> {
    let harness = NginxHarness::new(&case.name);
    write_case_files(&harness, &case.files, &case.name)?;
    harness
        .try_write_config(&case.config)
        .map_err(|err| format!("case {:?}: render/write config: {err}", case.name))?;
    let check = harness.check_config();
    if check.success != case.expected_success {
        return Err(format!(
            "case {:?}: expected nginx -t {}, got {}\n{}",
            case.name,
            if case.expected_success {
                "success"
            } else {
                "failure"
            },
            if check.success { "success" } else { "failure" },
            check.report()
        ));
    }
    let actual = format!("{}\n{}", check.output, check.error_log);
    if let Err(err) = assert_matchers(
        &case.name,
        "nginx -t output",
        &actual,
        &case.output_matchers,
    ) {
        return Err(format!("{err}\n{}", check.report()));
    }
    if !case.any_output_fragments.is_empty()
        && !case
            .any_output_fragments
            .iter()
            .any(|fragment| actual.contains(fragment))
    {
        return Err(format!(
            "case {:?}: nginx -t output is missing every acceptable fragment {:?}\n{}",
            case.name,
            case.any_output_fragments,
            check.report()
        ));
    }
    Ok(check)
}

/// Run config cases sequentially, preserving result order.
pub fn run_config_cases(cases: &[ConfigCase]) -> Result<Vec<ConfigCheck>, String> {
    cases.iter().map(run_config_case).collect()
}

impl HttpResponse {
    /// First value of `name` (case-insensitive), if any.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.header_values(name).into_iter().next()
    }

    /// Response body decoded as lossy UTF-8.
    pub fn body_text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

impl RunningNginx {
    /// Send one typed HTTP request and parse the complete close-delimited
    /// response. Per-request connect/read/write deadlines come from the request.
    pub fn request(&mut self, request: &HttpRequest) -> Result<HttpResponse, String> {
        if let Some(status) = self.try_reap() {
            return Err(self.failure_report("exited before the HTTP request", Some(status)));
        }
        let mut stream =
            TcpStream::connect_timeout(&loopback_socket(self.port), request.connect_timeout)
                .map_err(|err| {
                    format!(
                        "[{}] connect to 127.0.0.1:{} within {:?}: {err}\n{}",
                        self.label,
                        self.port,
                        request.connect_timeout,
                        self.failure_report("request connect failed", self.exit)
                    )
                })?;
        stream
            .set_read_timeout(Some(request.read_timeout))
            .map_err(|err| format!("set read timeout: {err}"))?;
        stream
            .set_write_timeout(Some(request.write_timeout))
            .map_err(|err| format!("set write timeout: {err}"))?;
        let wire_request = request.to_bytes(self.port)?;
        stream
            .write_all(&wire_request)
            .map_err(|err| format!("[{}] write HTTP request: {err}", self.label))?;
        stream
            .flush()
            .map_err(|err| format!("[{}] flush HTTP request: {err}", self.label))?;
        let mut wire_response = Vec::new();
        stream
            .read_to_end(&mut wire_response)
            .map_err(|err| format!("[{}] read HTTP response: {err}", self.label))?;
        HttpResponse::parse(&wire_response).map_err(|err| {
            format!(
                "[{}] parse HTTP response: {err}\nwire response:\n{}",
                self.label,
                String::from_utf8_lossy(&wire_response)
            )
        })
    }

    /// Poll a prefix-relative log until `needle` appears or `timeout` elapses.
    /// Timeout diagnostics always include the current log content.
    pub fn wait_log_contains(
        &mut self,
        relative_path: &str,
        needle: &str,
        timeout: Duration,
    ) -> Result<String, String> {
        let path = resolve_relative_path(self.prefix(), relative_path)?;
        let deadline = Instant::now() + timeout;
        loop {
            let current = read_log(&path);
            if current.contains(needle) {
                return Ok(current);
            }
            if let Some(status) = self.try_reap() {
                return Err(format!(
                    "[{}] nginx exited ({status}) while waiting for log {} to contain {:?}\n\
                     current log content:\n{}\n{}",
                    self.label,
                    path.display(),
                    needle,
                    current,
                    self.failure_report("exited during log wait", Some(status))
                ));
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "[{}] timed out after {timeout:?} waiting for log {} to contain {:?}\n\
                     current log content:\n{}",
                    self.label,
                    path.display(),
                    needle,
                    current
                ));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

/// Expected log matcher for one request step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogExpectation {
    pub relative_path: String,
    pub matcher: Matcher,
    pub timeout: Duration,
}

/// One request plus response/header/log expectations in a typed HTTP case.
#[derive(Debug, Clone)]
pub struct RequestStep {
    pub request: HttpRequest,
    pub expected_status: Option<u16>,
    pub body_matchers: Vec<Matcher>,
    pub header_matchers: Vec<(String, Matcher)>,
    pub log_expectations: Vec<LogExpectation>,
}

impl RequestStep {
    pub fn new(request: HttpRequest) -> Self {
        Self {
            request,
            expected_status: None,
            body_matchers: Vec::new(),
            header_matchers: Vec::new(),
            log_expectations: Vec::new(),
        }
    }

    pub fn expect_status(mut self, status: u16) -> Self {
        self.expected_status = Some(status);
        self
    }

    pub fn body(mut self, matcher: Matcher) -> Self {
        self.body_matchers.push(matcher);
        self
    }

    pub fn header(mut self, name: impl Into<String>, matcher: Matcher) -> Self {
        self.header_matchers.push((name.into(), matcher));
        self
    }

    pub fn log(
        mut self,
        relative_path: impl Into<String>,
        matcher: Matcher,
        timeout: Duration,
    ) -> Self {
        self.log_expectations.push(LogExpectation {
            relative_path: relative_path.into(),
            matcher,
            timeout,
        });
        self
    }

    pub fn log_contains(
        self,
        relative_path: impl Into<String>,
        needle: impl Into<String>,
        timeout: Duration,
    ) -> Self {
        self.log(relative_path, Matcher::contains(needle), timeout)
    }
}

/// Named data-driven HTTP case: files, config, ordered request steps.
#[derive(Debug, Clone)]
pub struct HttpCase {
    pub name: String,
    pub files: Vec<TestFile>,
    pub config: NginxConfig,
    pub steps: Vec<RequestStep>,
}

impl HttpCase {
    pub fn new(name: impl Into<String>, config: NginxConfig) -> Self {
        Self {
            name: name.into(),
            files: Vec::new(),
            config,
            steps: Vec::new(),
        }
    }

    pub fn file(mut self, file: TestFile) -> Self {
        self.files.push(file);
        self
    }

    pub fn step(mut self, step: RequestStep) -> Self {
        self.steps.push(step);
        self
    }
}

fn response_report(response: &HttpResponse) -> String {
    format!(
        "HTTP/{:?} {} {}\nheaders: {:#?}\nbody ({} bytes):\n{}",
        response.version,
        response.status,
        response.reason,
        response.headers,
        response.body.len(),
        String::from_utf8_lossy(&response.body)
    )
}

fn running_report(running: &mut RunningNginx, reason: &str) -> String {
    let status = running.exit_status();
    running.failure_report(reason, status)
}

fn check_response(
    case_name: &str,
    step_index: usize,
    step: &RequestStep,
    response: &HttpResponse,
) -> Result<(), String> {
    let prefix = format!("case {case_name:?} request step {step_index}");
    if let Some(expected) = step.expected_status {
        if response.status != expected {
            return Err(format!(
                "{prefix}: expected status {expected}, got {}\n{}",
                response.status,
                response_report(response)
            ));
        }
    }
    let body = String::from_utf8_lossy(&response.body);
    assert_matchers(case_name, "response body", &body, &step.body_matchers)
        .map_err(|err| format!("{prefix}: {err}\n{}", response_report(response)))?;
    for (name, matcher) in &step.header_matchers {
        let values = response.header_values(name);
        let actual = values.join("\n");
        if !matcher.matches(&actual) {
            return Err(format!(
                "{prefix}: header {name:?} is missing matcher {matcher:?}; values={values:?}\n{}",
                response_report(response)
            ));
        }
    }
    Ok(())
}

/// Run one HTTP case in a fresh prefix and return responses in step order.
pub fn run_http_case(case: &HttpCase) -> Result<Vec<HttpResponse>, String> {
    let harness = NginxHarness::new(&case.name);
    write_case_files(&harness, &case.files, &case.name)?;
    harness
        .try_write_config(&case.config)
        .map_err(|err| format!("case {:?}: render/write config: {err}", case.name))?;
    let mut running = harness
        .try_start()
        .map_err(|err| format!("case {:?}: start failed\n{err}", case.name))?;
    let mut responses = Vec::with_capacity(case.steps.len());
    for (index, step) in case.steps.iter().enumerate() {
        let response = running.request(&step.request).map_err(|err| {
            format!(
                "case {:?} request step {index} failed: {err}\n{}",
                case.name,
                running_report(&mut running, "HTTP request failed")
            )
        })?;
        if let Err(err) = check_response(&case.name, index, step, &response) {
            return Err(format!(
                "{err}\n{}",
                running_report(&mut running, "response assertion failed")
            ));
        }
        for expectation in &step.log_expectations {
            match &expectation.matcher {
                Matcher::Contains(needle) => {
                    running.wait_log_contains(
                        &expectation.relative_path,
                        needle,
                        expectation.timeout,
                    )?;
                }
                matcher => {
                    let path = resolve_relative_path(running.prefix(), &expectation.relative_path)?;
                    let deadline = Instant::now() + expectation.timeout;
                    loop {
                        let actual = read_log(&path);
                        if matcher.matches(&actual) {
                            break;
                        }
                        if Instant::now() >= deadline {
                            return Err(format!(
                                "case {:?} request step {index}: timed out after {:?} waiting for log {} to match {matcher:?}\ncurrent log content:\n{}",
                                case.name,
                                expectation.timeout,
                                path.display(),
                                actual
                            ));
                        }
                        std::thread::sleep(Duration::from_millis(10));
                    }
                }
            }
        }
        responses.push(response);
    }
    running
        .graceful_quit(GRACEFUL_QUIT_TIMEOUT)
        .map_err(|err| format!("case {:?}: graceful quit failed\n{err}", case.name))?;
    Ok(responses)
}

/// Run HTTP cases sequentially, preserving case and response order.
pub fn run_http_cases(cases: &[HttpCase]) -> Result<Vec<Vec<HttpResponse>>, String> {
    cases.iter().map(run_http_case).collect()
}
