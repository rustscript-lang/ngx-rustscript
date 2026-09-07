//! Artifact validation: ELF identity, fingerprint, and private snapshots.

#[path = "../../support/fingerprint.rs"]
pub mod fingerprint;

use std::fmt;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::temp::{
    DUMP_FILE_MODE, OwnedTempDir, SNAPSHOT_FILE_MODE, SNAPSHOT_READONLY_FILE_MODE, TEMP_DIR_MODE,
    chmod_path, env_path, freeze_snapshot, isolated_prefix, unique_temp_dir,
};
use fingerprint::{
    NGINX_MAKEFILE_BINDGEN_NAME, SOURCE_FINGERPRINT_INPUTS, collect_fingerprint_owned,
    extract_makefile_bindgen_inputs, fingerprint_from_owned, fingerprint_from_parts,
    nginx_fingerprint_paths, normalize_ngx_auto_config,
};

pub const MODULE_CDYLIB_NAME: &str = "libngx_rustscript.so";
const LEGACY_BUILDSYSTEM_MODE_ENV: &str = "NGX_RUSTSCRIPT_BUILDSYSTEM";
pub const REQUIRED_NGINX_EXPORTS: &[&str] =
    &["ngx_modules", "ngx_module_names", "ngx_module_order"];
/// Production cdylib must leave these NGINX symbols undefined (no test stubs).
pub const FORBIDDEN_DEFINED_NGINX_STUBS: &[&str] = &[
    "ngx_conf_full_name",
    "ngx_conf_log_error",
    "ngx_array_push",
    "ngx_log_error_core",
    "ngx_pool_cleanup_add",
    "ngx_palloc",
    "ngx_http_core_module",
];
/// Unique cdylib identity OBJECT. Not a function and not a C string table.
pub const IDENTITY_SYMBOL: &str = "ngx_rustscript_module_identity";
/// Dedicated ELF section that holds the 32-byte identity OBJECT.
pub const IDENTITY_SECTION: &str = ".ngx_rustscript_module_identity";
pub const IDENTITY_SIZE: usize = fingerprint::FINGERPRINT_SIZE;

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
    WrongIdentity { artifact: PathBuf, detail: String },
}

impl fmt::Display for ModuleArtifactError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing(path) => write!(f, "module cdylib not found: {}", path.display()),
            Self::NotRegularFile(path) => {
                write!(f, "module path is not a regular file: {}", path.display())
            }
            Self::WrongIdentity { artifact, detail } => write!(
                f,
                "module cdylib {} is not this NGINX module ({detail})",
                artifact.display()
            ),
        }
    }
}

/// Canonicalize `path` and require the result to be a regular file.
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

pub fn module_exports_required_nginx_symbols(symbols: &[&str]) -> bool {
    REQUIRED_NGINX_EXPORTS
        .iter()
        .all(|required| symbols.iter().any(|symbol| symbol == required))
}

pub fn compute_source_fingerprint_from_dirs(
    crate_root: &Path,
    nginx_source_dir: Option<&Path>,
    nginx_build_dir: Option<&Path>,
) -> Result<[u8; IDENTITY_SIZE], String> {
    let nginx_source_dir =
        nginx_source_dir.ok_or_else(|| "NGINX_SOURCE_DIR must be set".to_string())?;
    let nginx_build_dir =
        nginx_build_dir.ok_or_else(|| "NGINX_BUILD_DIR must be set".to_string())?;
    let owned = collect_fingerprint_owned(crate_root, nginx_source_dir, nginx_build_dir)?;
    Ok(fingerprint_from_owned(&owned))
}

pub fn compute_source_fingerprint(crate_root: &Path) -> Result<[u8; IDENTITY_SIZE], String> {
    compute_source_fingerprint_from_dirs(
        crate_root,
        env_path("NGINX_SOURCE_DIR").as_deref(),
        env_path("NGINX_BUILD_DIR").as_deref(),
    )
}

pub fn chmod_owner_only(path: &Path, mode: u32) -> Result<(), String> {
    chmod_path(path, mode)
}

#[derive(Debug)]
pub struct ModuleSnapshot {
    path: PathBuf,
    _dir: OwnedTempDir,
}

impl std::ops::Deref for ModuleSnapshot {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.path
    }
}

impl AsRef<Path> for ModuleSnapshot {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

/// Copy `candidate` once into a newly created private directory.
///
/// All later `nm` / `readelf` / `objcopy` / `nginx -t` work uses this snapshot
/// path so a later replacement of the source path cannot change the verdict.
pub fn copy_private_snapshot(candidate: &Path) -> Result<ModuleSnapshot, ModuleArtifactError> {
    let dir = unique_temp_dir("snap");
    let dest = dir.join(MODULE_CDYLIB_NAME);
    fs::copy(candidate, &dest).map_err(|err| {
        identity_error(
            candidate.to_path_buf(),
            format!("copy private snapshot: {err}"),
        )
    })?;
    if let Err(detail) = chmod_owner_only(&dest, SNAPSHOT_FILE_MODE) {
        return Err(identity_error(dest, detail));
    }
    Ok(ModuleSnapshot {
        path: dest,
        _dir: dir,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadelfDynsym {
    name: String,
    type_name: String,
    bind: String,
    size: u64,
    value: u64,
    ndx: String,
}

/// Parse one GNU `readelf -W --dyn-syms` symbol line.
///
/// Documented columns after `Num:`: Value Size Type Bind Vis Ndx Name
pub fn parse_readelf_dynsym_line(line: &str) -> Option<ReadelfDynsym> {
    let line = line.trim();
    let (num, rest) = line.split_once(':')?;
    if !num.trim().chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let parts: Vec<&str> = rest.split_whitespace().collect();
    if parts.len() < 7 {
        return None;
    }
    let value = u64::from_str_radix(parts[0], 16).ok()?;
    let size = parts[1].parse::<u64>().ok()?;
    let type_name = parts[2].to_string();
    let bind = parts[3].to_string();
    let ndx = parts[5].to_string();
    let name = parts[6].split('@').next()?.to_string();
    if name.is_empty() {
        return None;
    }
    Some(ReadelfDynsym {
        name,
        type_name,
        bind,
        size,
        value,
        ndx,
    })
}

pub fn readelf_dynsym(path: &Path, name: &str) -> Result<ReadelfDynsym, String> {
    let output = Command::new("readelf")
        .args(["-W", "--dyn-syms"])
        .arg(path)
        .output()
        .map_err(|err| format!("readelf failed: {err}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("readelf rejected the file: {stderr}"));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut found = None;
    for line in stdout.lines() {
        let Some(sym) = parse_readelf_dynsym_line(line) else {
            continue;
        };
        if sym.name != name {
            continue;
        }
        if found.is_some() {
            return Err(format!("duplicate dynsym {name}"));
        }
        found = Some(sym);
    }
    found.ok_or_else(|| format!("dynsym {name} not found"))
}

/// GNU `readelf -W --dyn-syms` must report GLOBAL OBJECT of exact size.
pub fn validate_identity_elf_shape(path: &Path) -> Result<ReadelfDynsym, String> {
    let sym = readelf_dynsym(path, IDENTITY_SYMBOL)?;
    if sym.bind != "GLOBAL" {
        return Err(format!(
            "{IDENTITY_SYMBOL} ELF bind is {}, expected GLOBAL",
            sym.bind
        ));
    }
    if sym.type_name != "OBJECT" {
        return Err(format!(
            "{IDENTITY_SYMBOL} ELF type is {}, expected OBJECT",
            sym.type_name
        ));
    }
    if sym.size != IDENTITY_SIZE as u64 {
        return Err(format!(
            "{IDENTITY_SYMBOL} size is {}, expected {IDENTITY_SIZE}",
            sym.size
        ));
    }
    if sym.ndx == "UND" {
        return Err(format!("{IDENTITY_SYMBOL} is undefined"));
    }
    Ok(sym)
}

pub fn bind_identity_symbol_to_section(
    sym: &ReadelfDynsym,
    section: &ReadelfSection,
) -> Result<(), String> {
    let expected_ndx = section.index.to_string();
    if sym.ndx != expected_ndx {
        return Err(format!(
            "{IDENTITY_SYMBOL} Ndx is {}, expected {expected_ndx} ({IDENTITY_SECTION})",
            sym.ndx
        ));
    }
    let sym_end = sym
        .value
        .checked_add(sym.size)
        .ok_or_else(|| format!("{IDENTITY_SYMBOL} value+size overflows"))?;
    let sec_end = section
        .address
        .checked_add(section.size)
        .ok_or_else(|| format!("{IDENTITY_SECTION} address+size overflows"))?;
    if sym.value < section.address || sym_end > sec_end {
        return Err(format!(
            "{IDENTITY_SYMBOL} range [{:#x}, {sym_end:#x}) is outside {IDENTITY_SECTION} [{:#x}, {sec_end:#x})",
            sym.value, section.address
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadelfSection {
    name: String,
    type_name: String,
    index: u32,
    address: u64,
    size: u64,
}

/// Parse one GNU `readelf -W -S` section line.
///
/// After `[Nr]`, documented fields are Name Type Address Off Size ...
pub fn parse_readelf_section_line(line: &str) -> Option<ReadelfSection> {
    let trimmed = line.trim();
    let (idx_part, rest) = trimmed.split_once(']')?;
    let index = idx_part.rsplit_once('[')?.1.trim().parse::<u32>().ok()?;
    let parts: Vec<&str> = rest.split_whitespace().collect();
    if parts.len() < 5 {
        return None;
    }
    let name = parts[0].to_string();
    let type_name = parts[1].to_string();
    if name.is_empty() || name == "Name" || type_name == "Type" {
        return None;
    }
    let address = u64::from_str_radix(parts[2], 16).ok()?;
    let size = u64::from_str_radix(parts[4], 16).ok()?;
    Some(ReadelfSection {
        name,
        type_name,
        index,
        address,
        size,
    })
}

pub fn readelf_section(path: &Path, name: &str) -> Result<ReadelfSection, String> {
    let output = Command::new("readelf")
        .args(["-W", "-S"])
        .arg(path)
        .output()
        .map_err(|err| format!("readelf failed: {err}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("readelf rejected the file: {stderr}"));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut found = None;
    for line in stdout.lines() {
        let Some(section) = parse_readelf_section_line(line) else {
            continue;
        };
        if section.name != name {
            continue;
        }
        if found.is_some() {
            return Err(format!("duplicate section {name}"));
        }
        found = Some(section);
    }
    found.ok_or_else(|| format!("section {name} not found"))
}

pub fn validate_identity_section(path: &Path) -> Result<ReadelfSection, String> {
    let section = readelf_section(path, IDENTITY_SECTION)?;
    if section.type_name == "NOBITS" {
        return Err(format!("{IDENTITY_SECTION} is NOBITS"));
    }
    if section.type_name != "PROGBITS" {
        return Err(format!(
            "{IDENTITY_SECTION} type is {}, expected PROGBITS",
            section.type_name
        ));
    }
    if section.size != IDENTITY_SIZE as u64 {
        return Err(format!(
            "{IDENTITY_SECTION} size is {}, expected {IDENTITY_SIZE}",
            section.size
        ));
    }
    Ok(section)
}

/// Dump the unique identity section with GNU `objcopy --dump-section`.
///
/// `objcopy` may report status 0 even when it cannot dump a missing/NOBITS
/// section, so existence and exact length are the fail-closed checks. The
/// file is never read unless `metadata.len()` is exactly 32.
pub fn dump_identity_section(so: &Path, dump: &Path) -> Result<[u8; IDENTITY_SIZE], String> {
    // GNU objcopy writes a sibling temp file next to the input. Copy first so a
    // frozen read-only snapshot directory cannot make the dump fail.
    let work_dir = unique_temp_dir("objcopy-work");
    let work_so = work_dir.join(MODULE_CDYLIB_NAME);
    fs::copy(so, &work_so).map_err(|err| format!("copy {} for objcopy: {err}", so.display()))?;
    chmod_owner_only(&work_so, SNAPSHOT_FILE_MODE)?;
    let _ = fs::remove_file(dump);
    let output = Command::new("objcopy")
        .arg(format!(
            "--dump-section={IDENTITY_SECTION}={}",
            dump.display()
        ))
        .arg(&work_so)
        .output()
        .map_err(|err| format!("objcopy failed: {err}"))?;
    let meta = match fs::metadata(dump) {
        Ok(meta) => meta,
        Err(_) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!(
                "objcopy did not dump {IDENTITY_SECTION} (status {}, stderr: {stderr})",
                output.status
            ));
        }
    };
    if meta.len() != IDENTITY_SIZE as u64 {
        let len = meta.len();
        let _ = fs::remove_file(dump);
        return Err(format!(
            "{IDENTITY_SECTION} dump length is {len}, expected {IDENTITY_SIZE}"
        ));
    }
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let _ = fs::remove_file(dump);
        return Err(format!("objcopy rejected the file: {stderr}"));
    }
    let _ = chmod_owner_only(dump, DUMP_FILE_MODE);
    let bytes = fs::read(dump).map_err(|err| format!("read dumped section: {err}"))?;
    bytes
        .try_into()
        .map_err(|_| format!("{IDENTITY_SECTION} dump was not {IDENTITY_SIZE} bytes"))
}

pub fn read_validated_identity(path: &Path) -> Result<[u8; IDENTITY_SIZE], String> {
    let sym = validate_identity_elf_shape(path)?;
    let section = validate_identity_section(path)?;
    bind_identity_symbol_to_section(&sym, &section)?;
    let dump_dir = unique_temp_dir("id-dump");
    let dump = dump_dir.join("ngx_rustscript_module_identity.dump");
    dump_identity_section(path, &dump)
}

pub fn defined_dynamic_symbols(path: &Path) -> Result<Vec<String>, ModuleArtifactError> {
    let output = Command::new("nm")
        .args(["-D", "--defined-only", "--format=posix"])
        .arg(path)
        .output()
        .map_err(|err| ModuleArtifactError::WrongIdentity {
            artifact: path.to_path_buf(),
            detail: format!("nm failed: {err}"),
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(ModuleArtifactError::WrongIdentity {
            artifact: path.to_path_buf(),
            detail: format!("nm rejected the file: {stderr}"),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.split_whitespace().next().map(str::to_string))
        .collect())
}

pub fn identity_error(artifact: PathBuf, detail: String) -> ModuleArtifactError {
    ModuleArtifactError::WrongIdentity { artifact, detail }
}

pub fn validate_snapshot(snapshot: &Path, crate_root: &Path) -> Result<(), ModuleArtifactError> {
    let symbols = defined_dynamic_symbols(snapshot)?;
    let symbol_refs: Vec<&str> = symbols.iter().map(String::as_str).collect();
    if !module_exports_required_nginx_symbols(&symbol_refs) {
        return Err(identity_error(
            snapshot.to_path_buf(),
            format!("missing required exports {REQUIRED_NGINX_EXPORTS:?}"),
        ));
    }
    if let Some(stub) = FORBIDDEN_DEFINED_NGINX_STUBS
        .iter()
        .copied()
        .find(|name| symbols.iter().any(|symbol| symbol == name))
    {
        return Err(identity_error(
            snapshot.to_path_buf(),
            format!("production cdylib must not define NGINX stub symbol {stub}"),
        ));
    }
    let identity = read_validated_identity(snapshot)
        .map_err(|detail| identity_error(snapshot.to_path_buf(), detail))?;
    let expected = compute_source_fingerprint(crate_root).map_err(|detail| {
        identity_error(
            snapshot.to_path_buf(),
            format!("cannot recompute source fingerprint: {detail}"),
        )
    })?;
    if identity != expected {
        return Err(identity_error(
            snapshot.to_path_buf(),
            format!("{IDENTITY_SYMBOL} fingerprint does not match current sources"),
        ));
    }
    Ok(())
}

pub fn validate_module_artifact(
    candidate: &Path,
    crate_root: &Path,
) -> Result<ModuleSnapshot, ModuleArtifactError> {
    let canonical = require_canonical_regular_file(candidate)?;
    let snapshot = copy_private_snapshot(&canonical)?;
    match validate_snapshot(&snapshot, crate_root) {
        Ok(()) => {
            freeze_snapshot(&snapshot)
                .map_err(|detail| identity_error(canonical.clone(), detail))?;
            Ok(snapshot)
        }
        Err(err) => Err(match err {
            ModuleArtifactError::WrongIdentity { detail, .. } => identity_error(canonical, detail),
            other => other,
        }),
    }
}

pub fn module_cdylib() -> ModuleSnapshot {
    let crate_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let candidate = resolve_module_candidate(
        env_path("NGX_RUSTSCRIPT_MODULE").as_deref(),
        env_path("CARGO_TARGET_DIR").as_deref(),
        crate_root,
    );
    validate_module_artifact(&candidate, crate_root).unwrap_or_else(|err| {
        panic!(
            "{err}; build the cdylib first with `cargo build --lib` under the same CARGO_TARGET_DIR"
        )
    })
}

pub const NGX_RUST_STUBS: &str = r#"
void *ngx_modules[] = { 0 };
char *ngx_module_names[] = { "ngx_http_other_module", 0 };
void *ngx_module_order = 0;
"#;

pub const IDENTITY_NOBITS_ASM: &str = r#"
.section .note.GNU-stack,"",@progbits
.section .ngx_rustscript_module_identity,"aw",@nobits
.globl ngx_rustscript_module_identity
.type ngx_rustscript_module_identity, @object
.size ngx_rustscript_module_identity, 32
ngx_rustscript_module_identity:
    .zero 32
"#;

pub fn compile_shared_sources(dir: &Path, stem: &str, files: &[(&str, &str)]) -> PathBuf {
    let mut inputs = Vec::new();
    for (name, code) in files {
        let src = dir.join(name);
        fs::write(&src, code).unwrap();
        inputs.push(src);
    }
    let so = dir.join(format!("lib{stem}.so"));
    let mut cmd = Command::new("cc");
    cmd.args(["-shared", "-fPIC", "-Wl,-z,noexecstack", "-o"])
        .arg(&so);
    for src in &inputs {
        cmd.arg(src);
    }
    let output = cmd.output().expect("failed to spawn cc for module fixture");
    assert!(
        output.status.success(),
        "cc failed to build module fixture:\n{}",
        command_text(&output)
    );
    so
}

fn command_text(output: &std::process::Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

pub fn compile_c_shared(dir: &Path, stem: &str, code: &str) -> PathBuf {
    let name = format!("{stem}.c");
    compile_shared_sources(dir, stem, &[(&name, code)])
}

pub fn c_byte_list(byte: &str, n: usize) -> String {
    std::iter::repeat_n(byte, n).collect::<Vec<_>>().join(",")
}

pub fn identity_object_in_unique_section(initializer: &str, size: usize) -> String {
    format!(
        "{NGX_RUST_STUBS}\n__attribute__((section(\"{IDENTITY_SECTION}\"), used))\nunsigned char {IDENTITY_SYMBOL}[{size}] = {{{initializer}}};\n"
    )
}

pub fn require_generic_ngx_exports(so: &Path) {
    let symbols = defined_dynamic_symbols(so).expect("nm should read the fixture");
    let symbol_refs: Vec<&str> = symbols.iter().map(String::as_str).collect();
    assert!(
        module_exports_required_nginx_symbols(&symbol_refs),
        "fixture must export generic ngx-rust symbols: {symbols:?}"
    );
}

pub fn expect_wrong_identity(err: ModuleArtifactError) -> String {
    assert!(
        matches!(err, ModuleArtifactError::WrongIdentity { .. }),
        "expected WrongIdentity, got {err}"
    );
    err.to_string()
}

pub fn identity_payload_in_unique_section(initializer: &str) -> String {
    format!(
        "__attribute__((section(\"{IDENTITY_SECTION}\"), used))\nunsigned char ngx_rustscript_identity_section_payload[{IDENTITY_SIZE}] = {{{initializer}}};\n"
    )
}

pub fn asm_byte_list(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("0x{b:02x}"))
        .collect::<Vec<_>>()
        .join(",")
}

#[test]
fn built_module_identity_matches_recomputed_source_fingerprint() {
    let crate_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let module = module_cdylib();
    let expected = compute_source_fingerprint(crate_root).expect("hash local production inputs");
    let got = read_validated_identity(&module).expect("identity OBJECT should be readable");
    assert_eq!(got, expected);
}

#[test]
fn built_module_identity_lives_in_unique_named_elf_section() {
    let crate_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let module = module_cdylib();
    let expected = compute_source_fingerprint(crate_root).expect("hash local production inputs");
    let prefix = isolated_prefix("id-section");
    let dump = prefix.join("identity.bin");
    let got = dump_identity_section(&module, &dump)
        .expect("objcopy must dump unique identity section from the built module");
    assert_eq!(got, expected);
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
fn missing_module_file_is_rejected() {
    let prefix = isolated_prefix("missing-so");
    let missing = prefix.join("no-such.so");
    let err = require_canonical_regular_file(&missing).unwrap_err();
    assert!(
        matches!(err, ModuleArtifactError::Missing(_)),
        "expected Missing, got {err}"
    );
}

#[test]
fn directory_is_not_accepted_as_module() {
    let prefix = isolated_prefix("dir-so");
    let err = require_canonical_regular_file(&prefix).unwrap_err();
    assert!(
        matches!(err, ModuleArtifactError::NotRegularFile(_)),
        "expected NotRegularFile, got {err}"
    );
}

#[test]
fn module_path_is_canonicalized_to_a_regular_file() {
    let prefix = isolated_prefix("canon-so");
    let file = prefix.join("mod.so");
    fs::write(&file, b"placeholder").unwrap();
    let messy = prefix.join("conf").join("..").join("mod.so");
    let got = require_canonical_regular_file(&messy).unwrap();
    let expected = fs::canonicalize(&file).unwrap();
    assert_eq!(got, expected);
    assert!(got.is_absolute());
}

#[test]
fn required_nginx_exports_reject_wrong_symbol_sets() {
    assert!(module_exports_required_nginx_symbols(&[
        "ngx_modules",
        "ngx_module_names",
        "ngx_module_order",
    ]));
    assert!(module_exports_required_nginx_symbols(&[
        "unrelated",
        "ngx_module_order",
        "ngx_module_names",
        "ngx_modules",
    ]));
    assert!(!module_exports_required_nginx_symbols(&[]));
    assert!(!module_exports_required_nginx_symbols(&["ngx_modules"]));
    assert!(!module_exports_required_nginx_symbols(&[
        "ngx_modules",
        "ngx_module_names",
    ]));
}
#[test]
fn parse_readelf_dynsym_distinguishes_object_and_func() {
    let object =
        "     7: 0000000000003a40    32 OBJECT  GLOBAL DEFAULT   12 ngx_rustscript_module_identity";
    let func =
        "    40: 0000000000001234    16 FUNC    GLOBAL DEFAULT   13 ngx_rustscript_module_identity";
    let weak =
        "     8: 0000000000003a40    32 OBJECT  WEAK   DEFAULT   12 ngx_rustscript_module_identity";
    let parsed_object = parse_readelf_dynsym_line(object).expect("object line");
    assert_eq!(parsed_object.type_name, "OBJECT");
    assert_eq!(parsed_object.bind, "GLOBAL");
    assert_eq!(parsed_object.size, 32);
    assert_eq!(parsed_object.name, IDENTITY_SYMBOL);
    let parsed_func = parse_readelf_dynsym_line(func).expect("func line");
    assert_eq!(parsed_func.type_name, "FUNC");
    assert_eq!(parsed_func.size, 16);
    let parsed_weak = parse_readelf_dynsym_line(weak).expect("weak line");
    assert_eq!(parsed_weak.bind, "WEAK");
}

#[test]
fn parse_readelf_section_distinguishes_progbits_and_nobits() {
    let progbits = "  [ 9] .ngx_rustscript_module_identity PROGBITS        0000000000003e30 003e30 000020 00  Ao  0   0  1";
    let nobits = "  [20] .ngx_rustscript_module_identity NOBITS          0000000000004038 003020 000020 00  WA  0   0  1";
    let parsed = parse_readelf_section_line(progbits).expect("progbits line");
    assert_eq!(parsed.name, IDENTITY_SECTION);
    assert_eq!(parsed.type_name, "PROGBITS");
    assert_eq!(parsed.index, 9);
    assert_eq!(parsed.address, 0x3e30);
    assert_eq!(parsed.size, IDENTITY_SIZE as u64);
    let parsed_nobits = parse_readelf_section_line(nobits).expect("nobits line");
    assert_eq!(parsed_nobits.type_name, "NOBITS");
    assert_eq!(parsed_nobits.index, 20);
    assert_eq!(parsed_nobits.address, 0x4038);
    assert_eq!(parsed_nobits.size, IDENTITY_SIZE as u64);
}

#[test]
fn identity_symbol_must_use_exact_section_index_and_fit_address_range() {
    let section = ReadelfSection {
        name: IDENTITY_SECTION.to_string(),
        type_name: "PROGBITS".to_string(),
        index: 11,
        address: 0xb8a0,
        size: IDENTITY_SIZE as u64,
    };
    let bound = ReadelfDynsym {
        name: IDENTITY_SYMBOL.to_string(),
        type_name: "OBJECT".to_string(),
        bind: "GLOBAL".to_string(),
        size: IDENTITY_SIZE as u64,
        value: 0xb8a0,
        ndx: "11".to_string(),
    };
    bind_identity_symbol_to_section(&bound, &section).expect("matching Ndx and range");

    let in_data = ReadelfDynsym {
        ndx: "14".to_string(),
        ..bound.clone()
    };
    let err = bind_identity_symbol_to_section(&in_data, &section).expect_err("wrong Ndx");
    assert!(err.contains("Ndx") && err.contains("14") && err.contains("11"));

    let abs = ReadelfDynsym {
        ndx: "ABS".to_string(),
        ..bound.clone()
    };
    let err = bind_identity_symbol_to_section(&abs, &section).expect_err("ABS Ndx");
    assert!(err.contains("ABS"));

    let outside = ReadelfDynsym {
        value: 0xb8a0 - 16,
        ..bound.clone()
    };
    let err = bind_identity_symbol_to_section(&outside, &section).expect_err("outside range");
    assert!(err.contains("outside"));

    let overflow_sym = ReadelfDynsym {
        value: u64::MAX - 8,
        size: 32,
        ..bound.clone()
    };
    let err = bind_identity_symbol_to_section(&overflow_sym, &section).expect_err("sym overflow");
    assert!(err.contains("overflows"));

    let overflow_section = ReadelfSection {
        address: u64::MAX - 8,
        size: 32,
        ..section
    };
    let err = bind_identity_symbol_to_section(&bound, &overflow_section).expect_err("sec overflow");
    assert!(err.contains("overflows"));
}

#[test]
fn source_fingerprint_is_deterministic_and_input_sensitive() {
    let a = fingerprint_from_parts(&[("Cargo.toml", b"[package]\n")]);
    let b = fingerprint_from_parts(&[("Cargo.toml", b"[package]\n")]);
    let c = fingerprint_from_parts(&[("Cargo.toml", b"[package]\nname\n")]);
    let d = fingerprint_from_parts(&[("src/lib.rs", b"[package]\n")]);
    assert_eq!(a, b);
    assert_ne!(a, [0u8; IDENTITY_SIZE]);
    assert_ne!(a, c);
    assert_ne!(a, d);
}

#[test]
fn fingerprint_known_vector_matches_independent_oracle() {
    // Independently computed 4-lane FNV-1a-64 over the documented canonical
    // bytes. Do not regenerate this digest from SOURCE_FINGERPRINT_INPUTS.
    let digest = fingerprint_from_parts(&[
        ("Cargo.toml", b"name = \"ngx-rustscript\"\n"),
        ("src/lib.rs", b"pub fn f() {}\n"),
    ]);
    const EXPECTED: [u8; IDENTITY_SIZE] = [
        0x5d, 0x93, 0xb4, 0xc9, 0x6e, 0x7f, 0x98, 0x82, 0x52, 0x07, 0x99, 0xf1, 0xcd, 0x7a, 0x55,
        0x49, 0x7f, 0x27, 0x28, 0x40, 0xef, 0xfe, 0x9f, 0x6b, 0xac, 0x1a, 0x81, 0x14, 0x24, 0xc9,
        0x63, 0xc2,
    ];
    assert_eq!(digest, EXPECTED);
}

#[test]
fn fingerprint_input_names_match_independently_authored_order() {
    // Independently authored. Do not derive this list from SOURCE_FINGERPRINT_INPUTS.
    const EXPECTED_NAMES: &[&str] = &[
        "Cargo.toml",
        "Cargo.lock",
        "build.rs",
        "src/lib.rs",
        "src/config.rs",
        "src/access.rs",
        "src/vm_pool.rs",
        "config",
        "config.make",
        "auto/rust",
        "nginx/src/core/nginx.h",
        "nginx/ngx_auto_config.h",
        "nginx/ngx_auto_headers.h",
        "nginx/Makefile.bindgen",
    ];
    let crate_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let source = env_path("NGINX_SOURCE_DIR").expect("NGINX_SOURCE_DIR");
    let build = env_path("NGINX_BUILD_DIR").expect("NGINX_BUILD_DIR");
    let owned = collect_fingerprint_owned(crate_root, &source, &build).expect("fingerprint inputs");
    let names: Vec<&str> = owned.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(names, EXPECTED_NAMES);
    let rerun_paths = nginx_fingerprint_paths(&source, &build);
    assert_eq!(rerun_paths.len(), 4);
    assert_eq!(rerun_paths[3].0, NGINX_MAKEFILE_BINDGEN_NAME);
    assert!(rerun_paths[3].1.ends_with("Makefile"));

    let build_rs = fs::read_to_string(crate_root.join("build.rs")).expect("build.rs");
    assert!(
        build_rs.contains("SOURCE_FINGERPRINT_INPUTS"),
        "build.rs rerun list must stay aligned with fingerprint inputs"
    );
    assert!(
        build_rs.contains("auto/rust") || EXPECTED_NAMES.contains(&"auto/rust"),
        "auto/rust must remain in the fingerprint/rerun set"
    );
    assert!(
        !build_rs.contains("nginx_binary_required")
            && !build_rs.contains(LEGACY_BUILDSYSTEM_MODE_ENV),
        "build.rs must not require $NGINX_BUILD_DIR/nginx or consult {LEGACY_BUILDSYSTEM_MODE_ENV}"
    );
    let config = fs::read_to_string(crate_root.join("config")).expect("config");
    assert!(
        !config.contains(LEGACY_BUILDSYSTEM_MODE_ENV),
        "config must not export {LEGACY_BUILDSYSTEM_MODE_ENV}"
    );
}

fn crate_only_fingerprint(crate_root: &Path) -> [u8; IDENTITY_SIZE] {
    let mut owned = Vec::new();
    for rel in SOURCE_FINGERPRINT_INPUTS {
        owned.push(fs::read(crate_root.join(rel)).expect("crate fingerprint input"));
    }
    let parts: Vec<(&str, &[u8])> = SOURCE_FINGERPRINT_INPUTS
        .iter()
        .zip(owned.iter())
        .map(|(rel, bytes)| (*rel, bytes.as_slice()))
        .collect();
    fingerprint_from_parts(&parts)
}

fn write_minimal_nginx_tree(
    source: &Path,
    build: &Path,
    auto_config: &[u8],
    auto_headers: &[u8],
    makefile: &[u8],
) {
    fs::create_dir_all(source.join("src/core")).unwrap();
    fs::create_dir_all(build).unwrap();
    fs::write(
        source.join("src/core/nginx.h"),
        b"#define nginx_version 1030004\n",
    )
    .unwrap();
    fs::write(build.join("ngx_auto_config.h"), auto_config).unwrap();
    fs::write(build.join("ngx_auto_headers.h"), auto_headers).unwrap();
    fs::write(build.join("Makefile"), makefile).unwrap();
}

fn makefile_with_build_inc(build_inc: &str) -> String {
    format!("CFLAGS =  -pipe -O\nALL_INCS = -I src/core \\\n\t-I src/http \\\n\t-I {build_inc}\n")
}

#[test]
fn source_fingerprint_includes_nginx_configured_headers() {
    let crate_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let source = env_path("NGINX_SOURCE_DIR").expect("NGINX_SOURCE_DIR");
    let build = env_path("NGINX_BUILD_DIR").expect("NGINX_BUILD_DIR");
    let got = compute_source_fingerprint(crate_root).expect("hash local production inputs");
    let crate_only = crate_only_fingerprint(crate_root);
    assert_ne!(
        got, crate_only,
        "nginx version header and generated auto headers must be in the digest"
    );

    let nginx_h = fs::read(source.join("src/core/nginx.h")).expect("nginx.h");
    let auto_config = normalize_ngx_auto_config(
        &fs::read(build.join("ngx_auto_config.h")).expect("ngx_auto_config.h"),
        &source,
        &build,
        crate_root,
    )
    .expect("normalize auto config");
    let auto_headers = fs::read(build.join("ngx_auto_headers.h")).expect("ngx_auto_headers.h");
    let makefile = extract_makefile_bindgen_inputs(
        &fs::read(build.join("Makefile")).expect("Makefile"),
        &source,
        &build,
        crate_root,
    )
    .expect("extract bindgen inputs");
    let mut owned = Vec::new();
    for rel in SOURCE_FINGERPRINT_INPUTS {
        owned.push(fs::read(crate_root.join(rel)).expect("crate fingerprint input"));
    }
    owned.push(nginx_h);
    owned.push(auto_config);
    owned.push(auto_headers);
    owned.push(makefile);
    let mut names = SOURCE_FINGERPRINT_INPUTS.to_vec();
    names.extend_from_slice(&[
        "nginx/src/core/nginx.h",
        "nginx/ngx_auto_config.h",
        "nginx/ngx_auto_headers.h",
        NGINX_MAKEFILE_BINDGEN_NAME,
    ]);
    let parts: Vec<(&str, &[u8])> = names
        .iter()
        .zip(owned.iter())
        .map(|(rel, bytes)| (*rel, bytes.as_slice()))
        .collect();
    assert_eq!(got, fingerprint_from_parts(&parts));
}

#[test]
fn fingerprint_changes_for_wrong_nginx_version_header() {
    let pinned = fingerprint_from_parts(&[(
        "nginx/src/core/nginx.h",
        b"#define nginx_version 1030004\n#define NGINX_VERSION \"1.30.4\"\n",
    )]);
    let wrong = fingerprint_from_parts(&[(
        "nginx/src/core/nginx.h",
        b"#define nginx_version 1024000\n#define NGINX_VERSION \"1.24.0\"\n",
    )]);
    assert_ne!(pinned, wrong);
}

#[test]
fn fingerprint_changes_for_wrong_ngx_auto_config() {
    let pinned = fingerprint_from_parts(&[(
        "nginx/ngx_auto_config.h",
        b"#define NGX_HTTP_SSL  1\n#define NGX_HAVE_EPOLL  1\n",
    )]);
    let wrong = fingerprint_from_parts(&[(
        "nginx/ngx_auto_config.h",
        b"#define NGX_HTTP_SSL  1\n#define NGX_HAVE_KQUEUE  1\n",
    )]);
    assert_ne!(pinned, wrong);
}
#[test]
fn nginx_dirs_are_required_for_fingerprint() {
    let crate_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let err = compute_source_fingerprint_from_dirs(crate_root, None, None)
        .expect_err("missing nginx dirs must fail closed");
    assert!(
        err.contains("NGINX_SOURCE_DIR") || err.contains("NGINX_BUILD_DIR"),
        "error should name required nginx dirs, got {err}"
    );
}

#[test]
fn old_buildsystem_mode_env_has_no_effect_on_fingerprint() {
    let crate_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let prefix = isolated_prefix("fp-env-forge");
    let source = prefix.path();
    let build = prefix.join("custom-objs");
    write_minimal_nginx_tree(
        source,
        &build,
        b"#define NGX_HTTP_SSL  1\n#define NGX_CONFIGURE \" --with-compat --with-http_ssl_module\"\n",
        b"#define NGX_HAVE_EPOLL  1\n",
        makefile_with_build_inc("custom-objs").as_bytes(),
    );
    assert!(!build.join("nginx").exists());
    let owned = collect_fingerprint_owned(crate_root, source, &build)
        .expect("fingerprint without nginx binary");
    assert!(
        owned
            .iter()
            .any(|(name, _)| name == "nginx/ngx_auto_config.h"),
        "configured headers must still be hashed"
    );
    let fingerprint_src =
        fs::read_to_string(crate_root.join("support/fingerprint.rs")).expect("fingerprint.rs");
    assert!(
        !fingerprint_src.contains("std::env::var")
            || !fingerprint_src.contains("parse_buildsystem_mode_env"),
        "fingerprint must not trust {LEGACY_BUILDSYSTEM_MODE_ENV}"
    );
    let build_rs = fs::read_to_string(crate_root.join("build.rs")).expect("build.rs");
    assert!(
        !build_rs.contains(LEGACY_BUILDSYSTEM_MODE_ENV),
        "build.rs must ignore {LEGACY_BUILDSYSTEM_MODE_ENV}"
    );
    let config = fs::read_to_string(crate_root.join("config")).expect("config");
    assert!(
        !config.contains(LEGACY_BUILDSYSTEM_MODE_ENV),
        "config must not export {LEGACY_BUILDSYSTEM_MODE_ENV}"
    );
}

#[test]
fn fingerprint_from_configured_headers_does_not_need_nginx_binary() {
    let crate_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let prefix = isolated_prefix("fp-headers-only");
    let build = prefix.join("custom-objs");
    write_minimal_nginx_tree(
        prefix.path(),
        &build,
        b"#define NGX_HTTP_SSL  1\n",
        b"#define NGX_HAVE_EPOLL  1\n",
        makefile_with_build_inc("custom-objs").as_bytes(),
    );
    assert!(
        !build.join("nginx").exists(),
        "fixture must omit the nginx binary to model parallel make"
    );
    let owned = collect_fingerprint_owned(crate_root, prefix.path(), &build)
        .expect("configured headers must fingerprint without nginx binary");
    let names: Vec<&str> = owned.iter().map(|(name, _)| name.as_str()).collect();
    assert!(names.contains(&"nginx/src/core/nginx.h"));
    assert!(names.contains(&"nginx/ngx_auto_config.h"));
    assert!(names.contains(&"nginx/ngx_auto_headers.h"));
    assert!(names.contains(&NGINX_MAKEFILE_BINDGEN_NAME));
}

#[test]
fn custom_builddir_path_is_not_in_fingerprint() {
    let crate_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let prefix = isolated_prefix("fp-builddir-stable");
    let auto_config = b"#define NGX_HTTP_SSL  1\n";
    let auto_headers = b"#define NGX_HAVE_EPOLL  1\n";
    let build_a = prefix.join("objs-a");
    let build_b = prefix.join("machine/path/objs-b");
    write_minimal_nginx_tree(
        prefix.path(),
        &build_a,
        auto_config,
        auto_headers,
        makefile_with_build_inc("objs-a").as_bytes(),
    );
    write_minimal_nginx_tree(
        prefix.path(),
        &build_b,
        auto_config,
        auto_headers,
        makefile_with_build_inc("machine/path/objs-b").as_bytes(),
    );
    let a = collect_fingerprint_owned(crate_root, prefix.path(), &build_a).unwrap();
    let b = collect_fingerprint_owned(crate_root, prefix.path(), &build_b).unwrap();
    assert_eq!(
        fingerprint_from_owned(&a),
        fingerprint_from_owned(&b),
        "machine --builddir path must not enter the stable fingerprint"
    );
    assert_eq!(
        a.iter().map(|(name, _)| name.as_str()).collect::<Vec<_>>(),
        b.iter().map(|(name, _)| name.as_str()).collect::<Vec<_>>()
    );
    assert!(
        !a.iter().any(|(name, bytes)| name.contains("objs-a")
            || name.contains("objs-b")
            || bytes.windows(b"objs-a".len()).any(|w| w == b"objs-a")
            || bytes
                .windows(b"machine/path".len())
                .any(|w| w == b"machine/path")),
        "fingerprint inputs must use stable names, not machine builddir paths"
    );
}

#[test]
fn ngx_auto_config_path_normalization_preserves_abi_flags() {
    let crate_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let prefix = isolated_prefix("fp-ngx-configure");
    let source = prefix.path();
    let build = prefix.join("objs-norm");
    fs::create_dir_all(source).unwrap();
    fs::create_dir_all(&build).unwrap();
    let prefix_a = "/tmp/nginx-prefix-a";
    let prefix_b = "/opt/other/nginx-prefix-b";
    let module_a = "/tmp/mod-a";
    let module_b = crate_root.display().to_string();
    let raw_a = format!(
        "#define NGX_CONFIGURE \" --prefix={prefix_a} --with-compat --with-http_ssl_module --builddir=objs-norm --add-dynamic-module={module_a}\"\n#ifndef NGX_PREFIX\n#define NGX_PREFIX  \"{prefix_a}/\"\n#endif\n#define NGX_HTTP_SSL  1\n"
    );
    let raw_b = format!(
        "#define NGX_CONFIGURE \" --prefix='{prefix_b}' --with-compat --with-http_ssl_module --builddir=/var/tmp/objs-other --add-dynamic-module={module_b}\"\n#ifndef NGX_PREFIX\n#define NGX_PREFIX  \"{prefix_b}/\"\n#endif\n#define NGX_HTTP_SSL  1\n"
    );
    let norm_a = normalize_ngx_auto_config(raw_a.as_bytes(), source, &build, crate_root)
        .expect("normalize a");
    let norm_b = normalize_ngx_auto_config(raw_b.as_bytes(), source, &build, crate_root)
        .expect("normalize b");
    let text_a = String::from_utf8(norm_a.clone()).unwrap();
    let text_b = String::from_utf8(norm_b.clone()).unwrap();
    assert_eq!(
        text_a, text_b,
        "path-only NGX_CONFIGURE data must canonicalize"
    );
    assert!(
        text_a.contains("--with-compat") && text_a.contains("--with-http_ssl_module"),
        "ABI flags must be preserved: {text_a}"
    );
    assert!(
        !text_a.contains(prefix_a)
            && !text_a.contains(prefix_b)
            && !text_a.contains(module_a)
            && !text_a.contains("--builddir")
            && !text_a.contains("--add-dynamic-module")
            && !text_a.contains("--prefix"),
        "path-only flags/values must be stripped, got {text_a}"
    );
    let raw_abi = format!(
        "#define NGX_CONFIGURE \" --prefix={prefix_a} --with-compat --with-http_ssl_module --with-http_v2\"\n#define NGX_PREFIX  \"{prefix_a}/\"\n#define NGX_HTTP_SSL  1\n"
    );
    let norm_abi =
        normalize_ngx_auto_config(raw_abi.as_bytes(), source, &build, crate_root).expect("abi");
    assert_ne!(
        norm_a, norm_abi,
        "an extra ABI flag must change the canonical auto config"
    );
}

#[test]
fn real_configured_trees_fingerprint_equal_iff_abi_matches() {
    let crate_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let source = env_path("NGINX_SOURCE_DIR").expect("NGINX_SOURCE_DIR");
    let build = env_path("NGINX_BUILD_DIR").expect("NGINX_BUILD_DIR");
    let addmod = source.join("objs-addmod");
    let owned_a = collect_fingerprint_owned(crate_root, &source, &build).expect("objs fingerprint");
    if addmod.join("ngx_auto_config.h").is_file() && addmod.join("Makefile").is_file() {
        let owned_b =
            collect_fingerprint_owned(crate_root, &source, &addmod).expect("addmod fingerprint");
        assert_eq!(
            fingerprint_from_owned(&owned_a),
            fingerprint_from_owned(&owned_b),
            "real trees that differ only in prefix/builddir/add-module paths must fingerprint equal"
        );
        let auto_a = owned_a
            .iter()
            .find(|(name, _)| name == "nginx/ngx_auto_config.h")
            .map(|(_, b)| b.as_slice())
            .unwrap();
        assert!(
            !auto_a
                .windows(b"/mnt/TEMP".len())
                .any(|w| w == b"/mnt/TEMP"),
            "canonical auto config must not retain the machine prefix path"
        );
    }
    let mut owned_flag = owned_a.clone();
    for (name, bytes) in &mut owned_flag {
        if name == "nginx/ngx_auto_config.h" {
            let text = String::from_utf8_lossy(bytes).replace(
                "--with-http_ssl_module",
                "--with-http_ssl_module --with-http_v2",
            );
            *bytes = text.into_bytes();
        }
    }
    assert_ne!(
        fingerprint_from_owned(&owned_a),
        fingerprint_from_owned(&owned_flag),
        "changing an ABI-relevant configure flag must change the fingerprint"
    );
}

#[test]
fn generic_ngx_rust_exports_without_identity_are_rejected() {
    let prefix = isolated_prefix("no-identity");
    let so = compile_c_shared(&prefix, "ngx_foreign", NGX_RUST_STUBS);
    require_generic_ngx_exports(&so);
    assert!(
        readelf_dynsym(&so, IDENTITY_SYMBOL).is_err(),
        "fixture must omit the module-specific identity symbol"
    );
    let crate_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let err = validate_module_artifact(&so, crate_root)
        .expect_err("generic ngx-rust exports without identity must not pass");
    let text = expect_wrong_identity(err);
    assert!(
        text.contains(IDENTITY_SYMBOL) || text.contains(IDENTITY_SECTION),
        "error should name the missing identity, got {text}"
    );
}

#[test]
fn identity_symbol_with_wrong_elf_type_is_rejected() {
    let prefix = isolated_prefix("identity-func");
    let code = format!(
        "{NGX_RUST_STUBS}\n__attribute__((section(\"{IDENTITY_SECTION}\"), used))\nvoid {IDENTITY_SYMBOL}(void) {{}}\n"
    );
    let so = compile_c_shared(&prefix, "identity_func", &code);
    require_generic_ngx_exports(&so);
    let shape = readelf_dynsym(&so, IDENTITY_SYMBOL).expect("symbol should exist");
    assert_eq!(shape.type_name, "FUNC");
    let crate_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let err =
        validate_module_artifact(&so, crate_root).expect_err("FUNC identity symbol must not pass");
    let text = expect_wrong_identity(err);
    assert!(
        text.contains("FUNC") && text.contains("OBJECT"),
        "error should report ELF type mismatch, got {text}"
    );
}

#[test]
fn identity_symbol_with_wrong_size_is_rejected() {
    let prefix = isolated_prefix("identity-short");
    let code = identity_object_in_unique_section("0", 16);
    let so = compile_c_shared(&prefix, "identity_short", &code);
    require_generic_ngx_exports(&so);
    let shape = readelf_dynsym(&so, IDENTITY_SYMBOL).expect("symbol should exist");
    assert_eq!(shape.type_name, "OBJECT");
    assert_eq!(shape.size, 16);
    let crate_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let err = validate_module_artifact(&so, crate_root)
        .expect_err("wrong-sized identity OBJECT must not pass");
    let text = expect_wrong_identity(err);
    assert!(
        text.contains("16") && text.contains("32"),
        "error should report size mismatch, got {text}"
    );
}

#[test]
fn valid_identity_shape_with_wrong_fingerprint_is_rejected() {
    let prefix = isolated_prefix("wrong-fingerprint");
    let code =
        identity_object_in_unique_section(&c_byte_list("0xaa", IDENTITY_SIZE), IDENTITY_SIZE);
    let so = compile_c_shared(&prefix, "wrong_fp", &code);
    require_generic_ngx_exports(&so);
    let shape = validate_identity_elf_shape(&so).expect("shape should be valid");
    assert_eq!(shape.size, IDENTITY_SIZE as u64);
    assert_eq!(shape.bind, "GLOBAL");
    let identity = read_validated_identity(&so).expect("bounded dump after shape check");
    assert_eq!(identity, [0xaa; IDENTITY_SIZE]);
    let crate_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let expected = compute_source_fingerprint(crate_root).expect("current sources");
    assert_ne!(identity, expected);
    let err =
        validate_module_artifact(&so, crate_root).expect_err("wrong fingerprint must not pass");
    let text = expect_wrong_identity(err);
    assert!(
        text.contains("fingerprint"),
        "error should say fingerprint failed, got {text}"
    );
    assert!(
        !text.contains("mtime"),
        "fingerprint is authoritative; mtime must not appear, got {text}"
    );
}

#[test]
fn identity_symbol_in_data_is_rejected_even_with_valid_identity_section_bytes() {
    let crate_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let expected = compute_source_fingerprint(crate_root).expect("current sources");
    let init = asm_byte_list(&expected);
    let prefix = isolated_prefix("identity-in-data");
    let code = format!(
        "{NGX_RUST_STUBS}\n{}\nunsigned char {IDENTITY_SYMBOL}[{IDENTITY_SIZE}] = {{{}}};\n",
        identity_payload_in_unique_section(&init),
        init
    );
    let so = compile_c_shared(&prefix, "identity_in_data", &code);
    require_generic_ngx_exports(&so);
    let section = validate_identity_section(&so).expect("unique identity section should exist");
    assert_eq!(section.size, IDENTITY_SIZE as u64);
    let dump = prefix.join("identity.bin");
    let dumped = dump_identity_section(&so, &dump).expect("section bytes can still be valid");
    assert_eq!(dumped, expected);
    let shape = validate_identity_elf_shape(&so).expect("dynsym shape can still look valid");
    assert_eq!(shape.size, IDENTITY_SIZE as u64);
    assert_ne!(
        shape.ndx,
        section.index.to_string(),
        "fixture must place {IDENTITY_SYMBOL} outside {IDENTITY_SECTION} (Ndx {}, section {})",
        shape.ndx,
        section.index
    );
    let err = validate_module_artifact(&so, crate_root)
        .expect_err("identity OBJECT in .data must not pass even with valid section bytes");
    let text = expect_wrong_identity(err);
    assert!(
        text.contains(IDENTITY_SECTION) || text.contains("Ndx") || text.contains("section"),
        "error should report section binding failure, got {text}"
    );
}

#[test]
fn identity_symbol_with_abs_ndx_is_rejected_even_with_valid_identity_section_bytes() {
    let crate_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let expected = compute_source_fingerprint(crate_root).expect("current sources");
    let aa = asm_byte_list(&expected);
    let prefix = isolated_prefix("identity-abs");
    let asm = format!(
        r#"
.section .note.GNU-stack,"",@progbits
.section {IDENTITY_SECTION},"a",@progbits
.globl ngx_rustscript_identity_section_payload
.type ngx_rustscript_identity_section_payload, @object
.size ngx_rustscript_identity_section_payload, {IDENTITY_SIZE}
ngx_rustscript_identity_section_payload:
    .byte {aa}
.globl {IDENTITY_SYMBOL}
.type {IDENTITY_SYMBOL}, @object
.size {IDENTITY_SYMBOL}, {IDENTITY_SIZE}
{IDENTITY_SYMBOL} = 0
"#
    );
    let so = compile_shared_sources(
        &prefix,
        "identity_abs",
        &[("stubs.c", NGX_RUST_STUBS), ("identity_abs.S", &asm)],
    );
    require_generic_ngx_exports(&so);
    let dump = prefix.join("identity.bin");
    let dumped = dump_identity_section(&so, &dump).expect("ABS fixture still has section bytes");
    assert_eq!(dumped, expected);
    let shape = readelf_dynsym(&so, IDENTITY_SYMBOL).expect("ABS identity symbol should exist");
    assert_eq!(shape.ndx, "ABS");
    let err = validate_module_artifact(&so, crate_root)
        .expect_err("ABS identity symbol must not pass even with valid section bytes");
    let text = expect_wrong_identity(err);
    assert!(
        text.contains("ABS") || text.contains(IDENTITY_SECTION) || text.contains("Ndx"),
        "error should report ABS/section binding failure, got {text}"
    );
}

#[test]
fn identity_object_without_unique_section_is_rejected() {
    let prefix = isolated_prefix("no-section");
    let code = format!(
        "{NGX_RUST_STUBS}\nunsigned char {IDENTITY_SYMBOL}[{IDENTITY_SIZE}] = {{{}}};\n",
        c_byte_list("0xaa", IDENTITY_SIZE)
    );
    let so = compile_c_shared(&prefix, "no_section", &code);
    require_generic_ngx_exports(&so);
    let shape = validate_identity_elf_shape(&so).expect("dynsym shape can still be valid");
    assert_eq!(shape.size, IDENTITY_SIZE as u64);
    let dump = prefix.join("identity.bin");
    assert!(
        dump_identity_section(&so, &dump).is_err(),
        "fixture must omit unique section {IDENTITY_SECTION}"
    );
    let crate_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let err = validate_module_artifact(&so, crate_root)
        .expect_err("identity OBJECT outside unique section must not pass");
    let text = expect_wrong_identity(err);
    assert!(
        text.contains(IDENTITY_SECTION),
        "error should name the missing unique section, got {text}"
    );
}

#[test]
fn nobits_identity_section_is_rejected() {
    let prefix = isolated_prefix("nobits");
    let so = compile_shared_sources(
        &prefix,
        "nobits",
        &[
            ("stubs.c", NGX_RUST_STUBS),
            ("identity.S", IDENTITY_NOBITS_ASM),
        ],
    );
    require_generic_ngx_exports(&so);
    let section = readelf_section(&so, IDENTITY_SECTION).expect("named section should exist");
    assert_eq!(section.type_name, "NOBITS");
    let crate_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let err = validate_module_artifact(&so, crate_root)
        .expect_err("NOBITS identity section must not pass");
    let text = expect_wrong_identity(err);
    assert!(
        text.contains("NOBITS"),
        "error should report NOBITS identity section, got {text}"
    );
}

#[test]
fn malformed_elf_is_rejected_without_panic() {
    let crate_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut extreme = vec![0u8; 64];
    extreme[0..4].copy_from_slice(&[0x7f, b'E', b'L', b'F']);
    extreme[4] = 2;
    extreme[5] = 1;
    extreme[6] = 1;
    extreme[16..18].copy_from_slice(&3u16.to_le_bytes());
    extreme[40..48].copy_from_slice(&64u64.to_le_bytes());
    extreme[58..60].copy_from_slice(&0xffffu16.to_le_bytes());
    extreme[60..62].copy_from_slice(&0xffffu16.to_le_bytes());
    let fixtures: &[(&str, &[u8])] = &[
        ("tiny", b"\x7fELF"),
        ("empty", b""),
        ("not-elf", b"this is not an ELF shared object"),
        ("truncated-header", &[0x7f, b'E', b'L', b'F', 2, 1, 1]),
        ("extreme-shnum", extreme.as_slice()),
    ];
    for (label, bytes) in fixtures {
        let prefix = isolated_prefix(&format!("malformed-{label}"));
        let so = prefix.join("bad.so");
        fs::write(&so, bytes).unwrap();
        let err =
            validate_module_artifact(&so, crate_root).expect_err("malformed ELF must fail closed");
        assert!(
            matches!(
                err,
                ModuleArtifactError::WrongIdentity { .. } | ModuleArtifactError::Missing(_)
            ),
            "{label}: expected fail-closed identity error, got {err}"
        );
    }
}

#[test]
fn private_snapshot_ignores_later_source_path_replacement() {
    let prefix = isolated_prefix("replace-src");
    let code =
        identity_object_in_unique_section(&c_byte_list("0xaa", IDENTITY_SIZE), IDENTITY_SIZE);
    let so = compile_c_shared(&prefix, "replace_src", &code);
    let candidate = prefix.join("live.so");
    fs::copy(&so, &candidate).unwrap();
    let snapshot = copy_private_snapshot(&candidate).expect("copy snapshot");
    fs::write(&candidate, b"\x7fELF-replaced-after-copy").unwrap();
    let dumped =
        read_validated_identity(&snapshot).expect("snapshot must keep the copied identity section");
    assert_eq!(dumped, [0xaa; IDENTITY_SIZE]);
    assert!(
        read_validated_identity(&candidate).is_err(),
        "replaced source path must not still look like the module"
    );
    let snap_mode = fs::metadata(&snapshot).unwrap().permissions().mode() & 0o777;
    let dir_mode = fs::metadata(snapshot.parent().unwrap())
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(snap_mode, SNAPSHOT_FILE_MODE);
    assert_eq!(dir_mode, TEMP_DIR_MODE);
}

#[test]
fn validated_snapshot_is_read_only() {
    let snapshot = module_cdylib();
    let snap_mode = fs::metadata(&*snapshot).unwrap().permissions().mode() & 0o777;
    let dir_mode = fs::metadata(snapshot.parent().unwrap())
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(snap_mode, SNAPSHOT_READONLY_FILE_MODE);
    assert_eq!(dir_mode, TEMP_DIR_MODE);
}
