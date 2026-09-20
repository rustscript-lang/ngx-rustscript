//! Isolated hook-enabled cdylib builds that never reuse `CARGO_TARGET_DIR`.

#![allow(dead_code)]

use std::fs::{self, File, OpenOptions};
use std::os::unix::io::AsRawFd;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime};

use super::artifact::{
    MODULE_CDYLIB_NAME, reject_defined_nginx_stubs, require_canonical_regular_file,
    require_defined_request_shims,
};
use super::temp::env_path;

pub const HOOK_STRINGS: &[&str] = &[
    "NGX_RUSTSCRIPT_TEST_LOG_PANIC",
    "NGX_RUSTSCRIPT_LOG_WAIT_PROOF",
    "LOG_WAIT_PROOF",
    "NGX_RUSTSCRIPT_BODY_COUNT_PROOF",
    "BODY_COUNT_PROOF",
    "BODY_WAIT_ARMED",
    "NGX_RUSTSCRIPT_ASYNC_PROOF",
    "ASYNC_CALLBACK_PROOF",
    "NGX_RUSTSCRIPT_VM_REUSE_PROOF",
    "VM_REUSE_PROOF",
];

pub const HOOK_SOURCE_IDENTS: &[&str] = &[
    "NGX_RUSTSCRIPT_TEST_LOG_PANIC",
    "NGX_RUSTSCRIPT_LOG_WAIT_PROOF",
    "LOG_WAIT_PROOF",
    "maybe_integration_log_panic",
    "log_wait_proof",
    "wait_arm_snapshot",
    "reset_wait_arm_snapshot",
    "record_wait_count_inc",
    "record_wait_timer_arm",
    "WAIT_EVENTFD_ARMS",
    "WAIT_COUNT_INCS",
    "WAIT_TIMER_ARMS",
    "NGX_RUSTSCRIPT_BODY_COUNT_PROOF",
    "BODY_COUNT_PROOF",
    "BODY_WAIT_ARMED",
    "proof_body_count",
    "NGX_RUSTSCRIPT_ASYNC_PROOF",
    "ASYNC_CALLBACK_PROOF",
    "NGX_RUSTSCRIPT_VM_REUSE_PROOF",
    "VM_REUSE_PROOF",
    "log_reuse_pool_proof",
    "reusable_request_vm_pool_stats",
];

const CARGO_BUILD_TIMEOUT: Duration = Duration::from_secs(1500);
const SEQUENCE_LOCK_TIMEOUT: Duration = Duration::from_secs(5400);
const LOCK_POLL: Duration = Duration::from_millis(50);
const PACKAGE_BUILD_DIR_PREFIX: &str = "ngx-rustscript-";
const PACKAGE_BUILD_HASH_LEN: usize = 16;
const FINGERPRINT_ENV_NEW: &str = "cargo::rustc-env=NGX_RUSTSCRIPT_HOOKS_FINGERPRINT=";
const FINGERPRINT_ENV_OLD: &str = "cargo:rustc-env=NGX_RUSTSCRIPT_HOOKS_FINGERPRINT=";
const HOOKS_CFG_NEW: &str = "cargo::rustc-cfg=ngx_rustscript_integration_hooks";
const HOOKS_CFG_OLD: &str = "cargo:rustc-cfg=ngx_rustscript_integration_hooks";

pub fn extra_build_root() -> PathBuf {
    if let Some(dir) = env_path("NGX_RUSTSCRIPT_HOOK_TARGET_DIR") {
        return dir;
    }
    if let Some(dir) = env_path("NGX_RUSTSCRIPT_TASK_TMP") {
        return dir.join("integration-hooks");
    }
    panic!(
        "set NGX_RUSTSCRIPT_HOOK_TARGET_DIR or NGX_RUSTSCRIPT_TASK_TMP to an isolated directory outside CARGO_TARGET_DIR"
    );
}

pub fn production_target_dir() -> PathBuf {
    env_path("CARGO_TARGET_DIR")
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("target"))
}

fn assert_isolated_from_production(target_dir: &Path) {
    let production =
        fs::canonicalize(production_target_dir()).unwrap_or_else(|_| production_target_dir());
    fs::create_dir_all(target_dir).unwrap_or_else(|error| {
        panic!(
            "create isolated hook target {}: {error}",
            target_dir.display()
        )
    });
    let extra = fs::canonicalize(target_dir)
        .unwrap_or_else(|err| panic!("canonicalize {}: {err}", target_dir.display()));
    if extra == production || extra.starts_with(&production) {
        panic!(
            "extra build target {} must not be inside production CARGO_TARGET_DIR {}",
            extra.display(),
            production.display()
        );
    }
}

pub fn artifact_contains_string(path: &Path, needle: &str) -> bool {
    let bytes = fs::read(path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
    let needle = needle.as_bytes();
    if needle.is_empty() {
        return true;
    }
    // SAFETY: `bytes`/`needle` are live borrowed slices; memmem(3) only reads them.
    let found = unsafe {
        libc_memmem(
            bytes.as_ptr().cast(),
            bytes.len(),
            needle.as_ptr().cast(),
            needle.len(),
        )
    };
    !found.is_null()
}

pub fn assert_no_hook_strings(path: &Path) {
    for needle in HOOK_STRINGS {
        assert!(
            !artifact_contains_string(path, needle),
            "{} must not contain hook string {needle}",
            path.display()
        );
    }
}

pub fn assert_has_hook_strings(path: &Path) {
    for needle in HOOK_STRINGS {
        assert!(
            artifact_contains_string(path, needle),
            "{} must contain hook string {needle}",
            path.display()
        );
    }
}

pub fn assert_no_hook_source_idents(path: &Path) {
    for needle in HOOK_SOURCE_IDENTS {
        assert!(
            !artifact_contains_string(path, needle),
            "{} must not contain hook source ident {needle}",
            path.display()
        );
    }
}

#[derive(Debug, Clone)]
pub struct BuildScriptReport {
    pub fingerprint: u8,
    pub hooks_cfg: bool,
    pub output_path: PathBuf,
    pub invoked: SystemTime,
    pub reran_build_script: bool,
}

#[derive(Debug)]
pub struct IsolationArtifacts {
    pub ordinary_debug: PathBuf,
    pub retained_hook_debug: PathBuf,
    pub ordinary_release: PathBuf,
    pub first_ordinary: BuildScriptReport,
    pub hook_enabled: BuildScriptReport,
    pub second_ordinary: BuildScriptReport,
    pub post_hook_release: BuildScriptReport,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct OutputStamp {
    path: PathBuf,
    modified: SystemTime,
    len: u64,
}

struct ExclusiveLock {
    file: File,
}

impl ExclusiveLock {
    fn acquire(path: &Path, timeout: Duration) -> Self {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)
            .unwrap_or_else(|err| panic!("open lock {}: {err}", path.display()));
        let start = Instant::now();
        loop {
            // SAFETY: `file` owns this fd for the lock lifetime; flock(2) is
            // the Unix exclusive-lock primitive on this host.
            let rc = unsafe { libc_flock(file.as_raw_fd(), LOCK_EX | LOCK_NB) };
            if rc == 0 {
                return Self { file };
            }
            if start.elapsed() >= timeout {
                panic!(
                    "timed out after {timeout:?} waiting for isolation lock {}",
                    path.display()
                );
            }
            std::thread::sleep(LOCK_POLL);
        }
    }
}

impl Drop for ExclusiveLock {
    fn drop(&mut self) {
        // SAFETY: unlocking the fd acquired in `acquire`.
        unsafe {
            libc_flock(self.file.as_raw_fd(), LOCK_UN);
        }
    }
}

const LOCK_EX: i32 = 2;
const LOCK_NB: i32 = 4;
const LOCK_UN: i32 = 8;

unsafe extern "C" {
    #[link_name = "flock"]
    fn libc_flock(fd: i32, operation: i32) -> i32;
    #[link_name = "memmem"]
    fn libc_memmem(
        haystack: *const std::ffi::c_void,
        haystacklen: usize,
        needle: *const std::ffi::c_void,
        needlelen: usize,
    ) -> *mut std::ffi::c_void;
}

fn is_ngx_rustscript_package_build_dir(name: &str) -> bool {
    let Some(rest) = name.strip_prefix(PACKAGE_BUILD_DIR_PREFIX) else {
        return false;
    };
    rest.len() == PACKAGE_BUILD_HASH_LEN && rest.bytes().all(|b| b.is_ascii_hexdigit())
}

fn parse_build_script_output(text: &str) -> Result<(u8, bool), String> {
    let mut fingerprint = None;
    let mut hooks_cfg = false;
    for (index, raw) in text.lines().enumerate() {
        let line = raw.trim();
        let value = line
            .strip_prefix(FINGERPRINT_ENV_NEW)
            .or_else(|| line.strip_prefix(FINGERPRINT_ENV_OLD));
        if let Some(value) = value {
            if fingerprint.is_some() {
                return Err(format!(
                    "duplicate NGX_RUSTSCRIPT_HOOKS_FINGERPRINT at line {}",
                    index + 1
                ));
            }
            let parsed = match value {
                "0" => 0,
                "1" => 1,
                other => {
                    return Err(format!(
                        "invalid NGX_RUSTSCRIPT_HOOKS_FINGERPRINT={other} at line {}",
                        index + 1
                    ));
                }
            };
            fingerprint = Some(parsed);
            continue;
        }
        if line == HOOKS_CFG_NEW || line == HOOKS_CFG_OLD {
            if hooks_cfg {
                return Err(format!(
                    "duplicate rustc-cfg ngx_rustscript_integration_hooks at line {}",
                    index + 1
                ));
            }
            hooks_cfg = true;
        }
    }
    let fingerprint = fingerprint.ok_or_else(|| {
        "build-script output missing cargo::rustc-env=NGX_RUSTSCRIPT_HOOKS_FINGERPRINT".to_string()
    })?;
    if (fingerprint == 1) != hooks_cfg {
        return Err(format!(
            "fingerprint={fingerprint} inconsistent with hooks cfg={hooks_cfg}"
        ));
    }
    Ok((fingerprint, hooks_cfg))
}

fn list_package_build_outputs(target_dir: &Path, profile: &str) -> Vec<OutputStamp> {
    let build = target_dir.join(profile).join("build");
    let mut stamps = Vec::new();
    let Ok(entries) = fs::read_dir(&build) else {
        return stamps;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !is_ngx_rustscript_package_build_dir(name) {
            continue;
        }
        let output = entry.path().join("output");
        let Ok(meta) = fs::metadata(&output) else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        stamps.push(OutputStamp {
            path: output,
            modified: meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
            len: meta.len(),
        });
    }
    stamps.sort_by(|a, b| a.path.cmp(&b.path));
    stamps
}

fn output_contains_fingerprint(path: &Path) -> bool {
    fs::read_to_string(path)
        .ok()
        .is_some_and(|text| text.contains("NGX_RUSTSCRIPT_HOOKS_FINGERPRINT="))
}

fn unique_newest_fingerprint_output(stamps: &[OutputStamp]) -> PathBuf {
    let mut with_fp: Vec<&OutputStamp> = stamps
        .iter()
        .filter(|stamp| output_contains_fingerprint(&stamp.path))
        .collect();
    match with_fp.len() {
        1 => with_fp.remove(0).path.clone(),
        0 => panic!(
            "no ngx-rustscript build-script output contains NGX_RUSTSCRIPT_HOOKS_FINGERPRINT"
        ),
        _ => {
            let newest = with_fp
                .iter()
                .map(|stamp| stamp.modified)
                .max()
                .expect("non-empty");
            let mut newest_stamps: Vec<&OutputStamp> = with_fp
                .into_iter()
                .filter(|stamp| stamp.modified == newest)
                .collect();
            if newest_stamps.len() != 1 {
                let paths: Vec<_> = newest_stamps
                    .iter()
                    .map(|stamp| stamp.path.display())
                    .collect();
                panic!(
                    "ambiguous ngx-rustscript build-script output ({} newest fingerprint files): {paths:?}",
                    newest_stamps.len()
                );
            }
            newest_stamps.remove(0).path.clone()
        }
    }
}

fn active_package_build_output(
    target_dir: &Path,
    profile: &str,
    before: &[OutputStamp],
    require_rerun: bool,
) -> PathBuf {
    let after = list_package_build_outputs(target_dir, profile);
    let mut changed: Vec<&OutputStamp> = after
        .iter()
        .filter(|now| match before.iter().find(|old| old.path == now.path) {
            None => true,
            Some(old) => old.modified != now.modified || old.len != now.len,
        })
        .filter(|stamp| output_contains_fingerprint(&stamp.path))
        .collect();
    match changed.len() {
        1 => changed.remove(0).path.clone(),
        0 if require_rerun => panic!(
            "no ngx-rustscript build-script output changed under {}/{profile}/build; cargo did not rerun build.rs or hashed output could not be identified",
            target_dir.display()
        ),
        0 => unique_newest_fingerprint_output(&after),
        n => {
            let paths: Vec<_> = changed.iter().map(|stamp| stamp.path.display()).collect();
            panic!(
                "ambiguous ngx-rustscript build-script output ({n} changed fingerprint files): {paths:?}"
            );
        }
    }
}

fn invoked_mtime(output_path: &Path) -> SystemTime {
    let ts = output_path
        .parent()
        .expect("build-script output has parent")
        .join("invoked.timestamp");
    fs::metadata(&ts)
        .and_then(|meta| meta.modified())
        .unwrap_or_else(|err| panic!("stat {}: {err}", ts.display()))
}

fn cargo_ran_build_script(stderr: &str, stdout: &str) -> bool {
    stderr
        .lines()
        .chain(stdout.lines())
        .any(|line| line.contains("Running") && line.contains("build-script-build"))
}

fn kill_process_group(pid: u32) {
    let _ = Command::new("kill")
        .args(["-KILL", &format!("-{pid}")])
        .status();
}

fn run_command_timed(mut cmd: Command, timeout: Duration, what: &str) -> std::process::Output {
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    cmd.process_group(0);
    let child = cmd
        .spawn()
        .unwrap_or_else(|err| panic!("spawn {what}: {err}"));
    let pid = child.id();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });
    match rx.recv_timeout(timeout) {
        Ok(Ok(output)) => output,
        Ok(Err(err)) => panic!("wait {what}: {err}"),
        Err(_) => {
            kill_process_group(pid);
            let _ = rx.recv_timeout(Duration::from_secs(5));
            panic!("{what} timed out after {timeout:?} (killed pid {pid})");
        }
    }
}

fn cargo_build_lib(
    target_dir: &Path,
    hooks: bool,
    release: bool,
    require_rerun: bool,
) -> (PathBuf, BuildScriptReport) {
    assert_isolated_from_production(target_dir);
    let profile = if release { "release" } else { "debug" };
    let before = list_package_build_outputs(target_dir, profile);
    let mut cmd = Command::new("cargo");
    cmd.arg("build").arg("--lib").arg("-v");
    if release {
        cmd.arg("--release");
    }
    cmd.current_dir(env!("CARGO_MANIFEST_DIR"));
    cmd.env("CARGO_TARGET_DIR", target_dir);
    cmd.env("CARGO_TERM_COLOR", "never");
    cmd.env_remove("NGX_RUSTSCRIPT_MODULE");
    if hooks {
        cmd.env("NGX_RUSTSCRIPT_INTEGRATION_HOOKS", "1");
    } else {
        cmd.env_remove("NGX_RUSTSCRIPT_INTEGRATION_HOOKS");
    }
    let output = run_command_timed(
        cmd,
        CARGO_BUILD_TIMEOUT,
        &format!(
            "cargo build --lib hooks={hooks} release={release} target={}",
            target_dir.display()
        ),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !output.status.success() {
        panic!(
            "cargo build --lib failed (hooks={hooks} release={release} target={}):\n{stdout}\n{stderr}",
            target_dir.display()
        );
    }
    let output_path = active_package_build_output(target_dir, profile, &before, require_rerun);
    let text = fs::read_to_string(&output_path)
        .unwrap_or_else(|err| panic!("read {}: {err}", output_path.display()));
    let (fingerprint, hooks_cfg) = parse_build_script_output(&text)
        .unwrap_or_else(|err| panic!("parse {}: {err}\n{text}", output_path.display()));
    let report = BuildScriptReport {
        fingerprint,
        hooks_cfg,
        invoked: invoked_mtime(&output_path),
        reran_build_script: cargo_ran_build_script(&stderr, &stdout),
        output_path,
    };
    let path = target_dir.join(profile).join(MODULE_CDYLIB_NAME);
    let path = require_canonical_regular_file(&path).unwrap_or_else(|err| panic!("{err}"));
    reject_defined_nginx_stubs(&path).unwrap_or_else(|err| panic!("{err}"));
    require_defined_request_shims(&path).unwrap_or_else(|err| panic!("{err}"));
    (path, report)
}

fn retain_hook_artifact(src: &Path) -> PathBuf {
    let dest_dir = extra_build_root().join("retained-hooks-debug");
    fs::create_dir_all(&dest_dir).unwrap();
    let dest = dest_dir.join(MODULE_CDYLIB_NAME);
    fs::copy(src, &dest).unwrap_or_else(|err| {
        panic!(
            "retain hook artifact {} -> {}: {err}",
            src.display(),
            dest.display()
        )
    });
    require_canonical_regular_file(&dest).unwrap_or_else(|err| panic!("{err}"))
}

fn assert_build_script_reran(prev: &BuildScriptReport, next: &BuildScriptReport, why: &str) {
    assert!(
        next.reran_build_script,
        "{why}: cargo verbose did not run build-script-build\nprev={}\nnext={}",
        prev.output_path.display(),
        next.output_path.display()
    );
    assert_ne!(
        (prev.fingerprint, prev.hooks_cfg),
        (next.fingerprint, next.hooks_cfg),
        "{why}: build-script fingerprint/cfg did not change ({} -> {})",
        prev.output_path.display(),
        next.output_path.display()
    );
    assert!(
        next.invoked >= prev.invoked,
        "{why}: invoked.timestamp moved backwards (prev={:?} next={:?})",
        prev.invoked,
        next.invoked
    );
}

fn cargo_clean_package(target_dir: &Path) {
    assert_isolated_from_production(target_dir);
    for release in [false, true] {
        let mut cmd = Command::new("cargo");
        cmd.args(["clean", "-p", "ngx-rustscript"]);
        if release {
            cmd.arg("--release");
        }
        cmd.current_dir(env!("CARGO_MANIFEST_DIR"));
        cmd.env("CARGO_TARGET_DIR", target_dir);
        cmd.env("CARGO_TERM_COLOR", "never");
        let output = run_command_timed(
            cmd,
            CARGO_BUILD_TIMEOUT,
            &format!(
                "cargo clean -p ngx-rustscript release={release} target={}",
                target_dir.display()
            ),
        );
        if !output.status.success() {
            panic!(
                "cargo clean -p ngx-rustscript failed (release={release} target={}):\n{}\n{}",
                target_dir.display(),
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}

fn run_same_target_isolation_sequence() -> IsolationArtifacts {
    let target_dir = extra_build_root().join("same-target-hooks");
    assert_isolated_from_production(&target_dir);
    let lock_path = extra_build_root().join("same-target-hooks.lock");
    let _lock = ExclusiveLock::acquire(&lock_path, SEQUENCE_LOCK_TIMEOUT);
    cargo_clean_package(&target_dir);

    let (first_artifact, first_ordinary) = cargo_build_lib(&target_dir, false, false, true);
    assert_eq!(
        first_ordinary.fingerprint,
        0,
        "ordinary debug fingerprint must be 0 ({})",
        first_ordinary.output_path.display()
    );
    assert!(
        !first_ordinary.hooks_cfg,
        "ordinary debug rustc cfg must be absent ({})",
        first_ordinary.output_path.display()
    );
    assert_no_hook_strings(&first_artifact);
    assert_no_hook_source_idents(&first_artifact);

    let (hook_artifact, hook_enabled) = cargo_build_lib(&target_dir, true, false, true);
    assert_eq!(
        hook_enabled.fingerprint,
        1,
        "hook-enabled debug fingerprint must be 1 ({})",
        hook_enabled.output_path.display()
    );
    assert!(
        hook_enabled.hooks_cfg,
        "hook-enabled debug rustc cfg must be present ({})",
        hook_enabled.output_path.display()
    );
    assert_has_hook_strings(&hook_artifact);
    assert_build_script_reran(
        &first_ordinary,
        &hook_enabled,
        "hooks=1 rebuild in the same target",
    );
    let retained_hook_debug = retain_hook_artifact(&hook_artifact);
    assert_has_hook_strings(&retained_hook_debug);

    let (ordinary_debug, second_ordinary) = cargo_build_lib(&target_dir, false, false, true);
    assert_eq!(
        second_ordinary.fingerprint,
        0,
        "post-hook ordinary debug fingerprint must return to 0 ({})",
        second_ordinary.output_path.display()
    );
    assert!(
        !second_ordinary.hooks_cfg,
        "post-hook ordinary debug rustc cfg must be absent ({})",
        second_ordinary.output_path.display()
    );
    assert_no_hook_strings(&ordinary_debug);
    assert_no_hook_source_idents(&ordinary_debug);
    assert_has_hook_strings(&retained_hook_debug);
    assert_build_script_reran(
        &hook_enabled,
        &second_ordinary,
        "unset NGX_RUSTSCRIPT_INTEGRATION_HOOKS rebuild in the same target",
    );

    let (ordinary_release, post_hook_release) = cargo_build_lib(&target_dir, false, true, true);
    assert_eq!(
        post_hook_release.fingerprint,
        0,
        "post-hook ordinary release fingerprint must be 0 ({})",
        post_hook_release.output_path.display()
    );
    assert!(
        !post_hook_release.hooks_cfg,
        "post-hook ordinary release rustc cfg must be absent ({})",
        post_hook_release.output_path.display()
    );
    assert_no_hook_strings(&ordinary_release);
    assert_no_hook_source_idents(&ordinary_release);

    IsolationArtifacts {
        ordinary_debug,
        retained_hook_debug,
        ordinary_release,
        first_ordinary,
        hook_enabled,
        second_ordinary,
        post_hook_release,
    }
}

pub fn same_target_isolation_sequence() -> &'static IsolationArtifacts {
    static SEQUENCE: OnceLock<IsolationArtifacts> = OnceLock::new();
    SEQUENCE.get_or_init(run_same_target_isolation_sequence)
}

pub fn hook_enabled_debug_cdylib() -> &'static Path {
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    PATH.get_or_init(|| {
        cargo_build_lib(&extra_build_root().join("hooks-debug"), true, false, false).0
    })
}

pub fn isolated_ordinary_debug_cdylib() -> &'static Path {
    same_target_isolation_sequence().ordinary_debug.as_path()
}

pub fn isolated_ordinary_release_cdylib() -> &'static Path {
    same_target_isolation_sequence().ordinary_release.as_path()
}

pub fn production_profile_cdylib(profile: &str) -> PathBuf {
    let path = production_target_dir()
        .join(profile)
        .join(MODULE_CDYLIB_NAME);
    require_canonical_regular_file(&path).unwrap_or_else(|err| panic!("{err}"))
}

/// Remove items gated by `#[cfg(ngx_rustscript_integration_hooks)]`.
pub fn strip_integration_hook_items(src: &str) -> String {
    const MARKER: &str = "#[cfg(ngx_rustscript_integration_hooks)]";
    let mut out = String::new();
    let mut rest = src;
    while let Some(index) = rest.find(MARKER) {
        out.push_str(&rest[..index]);
        rest = rest[index + MARKER.len()..].trim_start();
        rest = skip_leading_attributes(rest);
        rest = skip_rust_item(rest);
    }
    out.push_str(rest);
    out
}

fn skip_leading_attributes(src: &str) -> &str {
    let mut rest = src.trim_start();
    while rest.starts_with('#') {
        rest = skip_until_balanced(rest, '[', ']');
        rest = rest.trim_start();
    }
    rest
}

fn skip_rust_item(src: &str) -> &str {
    let rest = src.trim_start();
    if rest.is_empty() {
        return rest;
    }
    skip_until_item_end(rest)
}

fn skip_until_balanced(src: &str, open: char, close: char) -> &str {
    let mut depth: i32 = 0;
    let mut in_string = false;
    let mut in_char = false;
    let mut escaped = false;
    for (index, ch) in src.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        if in_char {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '\'' {
                in_char = false;
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '\'' => in_char = true,
            c if c == open => depth += 1,
            c if c == close => {
                depth -= 1;
                if depth == 0 {
                    return &src[index + ch.len_utf8()..];
                }
            }
            _ => {}
        }
    }
    ""
}

fn skip_until_item_end(src: &str) -> &str {
    let mut depth: i32 = 0;
    let mut seen_brace = false;
    let mut in_string = false;
    let mut in_char = false;
    let mut escaped = false;
    for (index, ch) in src.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        if in_char {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '\'' {
                in_char = false;
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '\'' => in_char = true,
            '{' => {
                depth += 1;
                seen_brace = true;
            }
            '}' => {
                depth = depth.saturating_sub(1);
                if seen_brace && depth == 0 {
                    return src[index + 1..].trim_start();
                }
            }
            ';' if depth == 0 => return src[index + 1..].trim_start(),
            _ => {}
        }
    }
    ""
}

#[test]
fn strip_integration_hook_items_removes_gated_fn_and_stmt() {
    let src = r#"
fn keep() {}
#[cfg(ngx_rustscript_integration_hooks)]
fn log_wait_proof() {
    let _ = "LOG_WAIT_PROOF";
}
fn also_keep() {
    #[cfg(ngx_rustscript_integration_hooks)]
    maybe_integration_log_panic();
}
"#;
    let stripped = strip_integration_hook_items(src);
    assert!(stripped.contains("fn keep()"));
    assert!(stripped.contains("fn also_keep()"));
    assert!(!stripped.contains("log_wait_proof"));
    assert!(!stripped.contains("LOG_WAIT_PROOF"));
    assert!(!stripped.contains("maybe_integration_log_panic"));
}

#[test]
fn parse_build_script_output_reads_fingerprint_zero_without_cfg() {
    let text = "\
cargo::rustc-check-cfg=cfg(ngx_rustscript_integration_hooks)
cargo::rustc-env=NGX_RUSTSCRIPT_HOOKS_FINGERPRINT=0
cargo::rustc-cfg=ngx_feature=\"http\"
";
    assert_eq!(parse_build_script_output(text).unwrap(), (0, false));
}

#[test]
fn parse_build_script_output_reads_fingerprint_one_with_cfg() {
    let text = "\
cargo::rustc-check-cfg=cfg(ngx_rustscript_integration_hooks)
cargo::rustc-env=NGX_RUSTSCRIPT_HOOKS_FINGERPRINT=1
cargo::rustc-cfg=ngx_rustscript_integration_hooks
";
    assert_eq!(parse_build_script_output(text).unwrap(), (1, true));
}

#[test]
fn parse_build_script_output_rejects_cfg_without_fingerprint_one() {
    let text = "\
cargo::rustc-check-cfg=cfg(ngx_rustscript_integration_hooks)
cargo::rustc-env=NGX_RUSTSCRIPT_HOOKS_FINGERPRINT=0
cargo::rustc-cfg=ngx_rustscript_integration_hooks
";
    assert!(
        parse_build_script_output(text)
            .unwrap_err()
            .contains("inconsistent")
    );
}

#[test]
fn package_build_dir_name_rejects_stubs_and_short_hashes() {
    assert!(is_ngx_rustscript_package_build_dir(
        "ngx-rustscript-bbe7be654d9d6aab"
    ));
    assert!(!is_ngx_rustscript_package_build_dir(
        "ngx-rustscript-nginx-test-stubs-97eeeea9e0fb6215"
    ));
    assert!(!is_ngx_rustscript_package_build_dir("ngx-rustscript-"));
    assert!(!is_ngx_rustscript_package_build_dir(
        "libc-713a192ecab7dad3"
    ));
}
