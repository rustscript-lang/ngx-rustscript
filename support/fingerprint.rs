//! Shared identity/freshness fingerprint inputs and algorithm.
//!
//! Used by `build.rs` and tests. This is not a cryptographic MAC.

use std::path::{Path, PathBuf};

#[path = "shell_words.rs"]
mod shell_words;
pub use shell_words::split_shell_words;

pub const FINGERPRINT_SIZE: usize = 32;
/// Local production/build inputs hashed into the cdylib identity fingerprint.
pub const SOURCE_FINGERPRINT_INPUTS: &[&str] = &[
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
];
pub const NGINX_VERSION_HEADER_NAME: &str = "nginx/src/core/nginx.h";
pub const NGINX_AUTO_CONFIG_NAME: &str = "nginx/ngx_auto_config.h";
pub const NGINX_AUTO_HEADERS_NAME: &str = "nginx/ngx_auto_headers.h";
pub const NGINX_MAKEFILE_BINDGEN_NAME: &str = "nginx/Makefile.bindgen";
pub const NGINX_VERSION_HEADER_REL: &str = "src/core/nginx.h";
pub const NGINX_AUTO_CONFIG_REL: &str = "ngx_auto_config.h";
pub const NGINX_AUTO_HEADERS_REL: &str = "ngx_auto_headers.h";
pub const NGINX_MAKEFILE_REL: &str = "Makefile";
pub const SOURCE_FINGERPRINT_MAGIC: &[u8] = b"ngx-rustscript-source-fp-v1\0";
pub const BINDGEN_INPUTS_MAGIC: &str = "ngx-rustscript-bindgen-inputs-v1\n";
pub const PLACEHOLDER_SOURCE: &str = "<NGINX_SOURCE>";
pub const PLACEHOLDER_BUILD: &str = "<NGINX_BUILD>";
pub const PLACEHOLDER_MODULE: &str = "<NGINX_MODULE>";
pub const PLACEHOLDER_PREFIX: &str = "<NGINX_PREFIX>";
const FNV64_OFFSET: u64 = 0xcbf29ce484222325;
const FNV64_PRIME: u64 = 0x100000001b3;

const PATH_ONLY_CONFIGURE_FLAGS: &[&str] = &[
    "--prefix",
    "--sbin-path",
    "--modules-path",
    "--conf-path",
    "--error-log-path",
    "--pid-path",
    "--lock-path",
    "--http-log-path",
    "--http-client-body-temp-path",
    "--http-proxy-temp-path",
    "--http-fastcgi-temp-path",
    "--http-uwsgi-temp-path",
    "--http-scgi-temp-path",
    "--builddir",
    "--add-module",
    "--add-dynamic-module",
    "--user",
    "--group",
    "--build",
];

/// Canonical nginx configured-input files, with stable fingerprint names.
pub fn nginx_fingerprint_paths(
    source_dir: &Path,
    build_dir: &Path,
) -> Vec<(&'static str, PathBuf)> {
    vec![
        (
            NGINX_VERSION_HEADER_NAME,
            source_dir.join(NGINX_VERSION_HEADER_REL),
        ),
        (
            NGINX_AUTO_CONFIG_NAME,
            build_dir.join(NGINX_AUTO_CONFIG_REL),
        ),
        (
            NGINX_AUTO_HEADERS_NAME,
            build_dir.join(NGINX_AUTO_HEADERS_REL),
        ),
        (
            NGINX_MAKEFILE_BINDGEN_NAME,
            build_dir.join(NGINX_MAKEFILE_REL),
        ),
    ]
}

/// Identity/freshness fingerprint (not a cryptographic MAC).
///
/// Canonical bytes:
///   `ngx-rustscript-source-fp-v1\0`
///   then for each input in order:
///     relative path UTF-8, 0x00, u64 little-endian content length, file bytes
///
/// Digest: four FNV-1a-64 lanes concatenated to 32 bytes. Lane `i` is FNV-1a-64
/// over the single prefix byte `i` followed by the canonical bytes, stored
/// little-endian.
pub fn fingerprint_from_parts(parts: &[(&str, &[u8])]) -> [u8; FINGERPRINT_SIZE] {
    let mut canonical = Vec::new();
    canonical.extend_from_slice(SOURCE_FINGERPRINT_MAGIC);
    for (name, bytes) in parts {
        canonical.extend_from_slice(name.as_bytes());
        canonical.push(0);
        canonical.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
        canonical.extend_from_slice(bytes);
    }
    let mut out = [0u8; FINGERPRINT_SIZE];
    for lane in 0..4u8 {
        let mut hash = FNV64_OFFSET;
        hash ^= u64::from(lane);
        hash = hash.wrapping_mul(FNV64_PRIME);
        for &b in &canonical {
            hash ^= u64::from(b);
            hash = hash.wrapping_mul(FNV64_PRIME);
        }
        let start = usize::from(lane) * 8;
        out[start..start + 8].copy_from_slice(&hash.to_le_bytes());
    }
    out
}

pub fn fingerprint_from_owned(owned: &[(String, Vec<u8>)]) -> [u8; FINGERPRINT_SIZE] {
    let parts: Vec<(&str, &[u8])> = owned
        .iter()
        .map(|(name, bytes)| (name.as_str(), bytes.as_slice()))
        .collect();
    fingerprint_from_parts(&parts)
}

pub fn collect_fingerprint_owned(
    crate_root: &Path,
    nginx_source_dir: &Path,
    nginx_build_dir: &Path,
) -> Result<Vec<(String, Vec<u8>)>, String> {
    let mut owned = Vec::new();
    for rel in SOURCE_FINGERPRINT_INPUTS {
        let path = crate_root.join(rel);
        let bytes = std::fs::read(&path)
            .map_err(|err| format!("fingerprint input {}: {err}", path.display()))?;
        owned.push(((*rel).to_string(), bytes));
    }
    let nginx_h = nginx_source_dir.join(NGINX_VERSION_HEADER_REL);
    let auto_config = nginx_build_dir.join(NGINX_AUTO_CONFIG_REL);
    let auto_headers = nginx_build_dir.join(NGINX_AUTO_HEADERS_REL);
    let makefile = nginx_build_dir.join(NGINX_MAKEFILE_REL);

    let nginx_h_bytes = std::fs::read(&nginx_h)
        .map_err(|err| format!("fingerprint input {}: {err}", nginx_h.display()))?;
    owned.push((NGINX_VERSION_HEADER_NAME.to_string(), nginx_h_bytes));

    let auto_config_raw = std::fs::read(&auto_config)
        .map_err(|err| format!("fingerprint input {}: {err}", auto_config.display()))?;
    let auto_config_norm = normalize_ngx_auto_config(
        &auto_config_raw,
        nginx_source_dir,
        nginx_build_dir,
        crate_root,
    )?;
    owned.push((NGINX_AUTO_CONFIG_NAME.to_string(), auto_config_norm));

    let auto_headers_bytes = std::fs::read(&auto_headers)
        .map_err(|err| format!("fingerprint input {}: {err}", auto_headers.display()))?;
    owned.push((NGINX_AUTO_HEADERS_NAME.to_string(), auto_headers_bytes));

    let makefile_raw = std::fs::read(&makefile)
        .map_err(|err| format!("fingerprint input {}: {err}", makefile.display()))?;
    let bindgen = extract_makefile_bindgen_inputs(
        &makefile_raw,
        nginx_source_dir,
        nginx_build_dir,
        crate_root,
    )?;
    owned.push((NGINX_MAKEFILE_BINDGEN_NAME.to_string(), bindgen));
    Ok(owned)
}

pub fn normalize_ngx_auto_config(
    raw: &[u8],
    source_dir: &Path,
    build_dir: &Path,
    module_dir: &Path,
) -> Result<Vec<u8>, String> {
    let text =
        std::str::from_utf8(raw).map_err(|_| "ngx_auto_config.h is not valid UTF-8".to_string())?;
    if raw.contains(&0) {
        return Err("ngx_auto_config.h contains NUL".to_string());
    }
    let mut prefix = extract_c_define_string(text, "NGX_PREFIX");
    let rewritten = rewrite_ngx_configure_line(text)?;
    if prefix.is_none() {
        prefix = extract_c_define_string(&rewritten, "NGX_PREFIX");
    }
    let replaced = replace_machine_paths(
        &rewritten,
        source_dir,
        build_dir,
        module_dir,
        prefix.as_deref(),
    );
    Ok(replaced.into_bytes())
}

fn rewrite_ngx_configure_line(text: &str) -> Result<String, String> {
    let mut out = String::with_capacity(text.len());
    for (idx, line) in text.lines().enumerate() {
        if idx > 0 {
            out.push('\n');
        }
        if let Some(value) = define_c_string(line, "NGX_CONFIGURE") {
            if value.bytes().any(|b| b == 0 || (b < 0x20 && b != b'\t')) {
                return Err("NGX_CONFIGURE contains a control character".to_string());
            }
            let words = split_shell_words(value)?;
            let kept: Vec<String> = words
                .into_iter()
                .filter(|word| !is_path_only_configure_flag(word))
                .collect();
            let serialized = if kept.is_empty() {
                String::new()
            } else {
                format!(" {}", kept.join(" "))
            };
            let indent = line.find('#').map(|i| &line[..i]).unwrap_or("");
            out.push_str(indent);
            out.push_str("#define NGX_CONFIGURE \"");
            out.push_str(&c_escape(&serialized));
            out.push('"');
        } else {
            out.push_str(line);
        }
    }
    if text.ends_with('\n') {
        out.push('\n');
    }
    Ok(out)
}

fn is_path_only_configure_flag(word: &str) -> bool {
    for flag in PATH_ONLY_CONFIGURE_FLAGS {
        if word == *flag || word.starts_with(&format!("{flag}=")) {
            return true;
        }
    }
    false
}

fn c_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            c => out.push(c),
        }
    }
    out
}

fn define_c_string<'a>(line: &'a str, name: &str) -> Option<&'a str> {
    let trimmed = line.trim();
    let rest = trimmed.strip_prefix("#define")?;
    let rest = rest.trim_start();
    let rest = rest.strip_prefix(name)?;
    if !rest.starts_with(|c: char| c.is_whitespace()) {
        return None;
    }
    let rest = rest.trim_start();
    let rest = rest.strip_prefix('"')?;
    let end = rest.rfind('"')?;
    Some(&rest[..end])
}

fn extract_c_define_string(text: &str, name: &str) -> Option<String> {
    for line in text.lines() {
        if let Some(value) = define_c_string(line, name) {
            return Some(value.to_string());
        }
    }
    None
}

fn replace_machine_paths(
    text: &str,
    source_dir: &Path,
    build_dir: &Path,
    module_dir: &Path,
    prefix: Option<&str>,
) -> String {
    let mut replacements = Vec::new();
    push_path_replacements(&mut replacements, source_dir, PLACEHOLDER_SOURCE);
    push_path_replacements(&mut replacements, build_dir, PLACEHOLDER_BUILD);
    push_path_replacements(&mut replacements, module_dir, PLACEHOLDER_MODULE);
    if let Some(prefix) = prefix {
        let trimmed = prefix.trim_end_matches('/');
        if !trimmed.is_empty() {
            replacements.push((format!("{trimmed}/"), format!("{PLACEHOLDER_PREFIX}/")));
            replacements.push((trimmed.to_string(), PLACEHOLDER_PREFIX.to_string()));
            push_path_replacements(&mut replacements, Path::new(trimmed), PLACEHOLDER_PREFIX);
        }
    }
    replacements.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then_with(|| a.0.cmp(&b.0)));
    replacements.dedup_by(|a, b| a.0 == b.0);
    let mut out = text.to_string();
    for (from, to) in replacements {
        if from.len() < 2 {
            continue;
        }
        out = out.replace(&from, &to);
    }
    out
}

fn push_path_replacements(out: &mut Vec<(String, String)>, path: &Path, placeholder: &str) {
    let mut forms = Vec::new();
    forms.push(path.display().to_string());
    if let Ok(canonical) = std::fs::canonicalize(path) {
        forms.push(canonical.display().to_string());
    }
    for form in forms {
        if form.is_empty() {
            continue;
        }
        let trimmed = form.trim_end_matches('/');
        if trimmed.is_empty() {
            continue;
        }
        out.push((format!("{trimmed}/"), format!("{placeholder}/")));
        out.push((trimmed.to_string(), placeholder.to_string()));
    }
}

pub fn extract_makefile_bindgen_inputs(
    raw: &[u8],
    source_dir: &Path,
    build_dir: &Path,
    module_dir: &Path,
) -> Result<Vec<u8>, String> {
    let text = std::str::from_utf8(raw).map_err(|_| "Makefile is not valid UTF-8".to_string())?;
    if raw.contains(&0) {
        return Err("Makefile contains NUL".to_string());
    }
    let (includes, defines) = parse_makefile_bindgen_lines(text)?;
    let mut lines = vec![BINDGEN_INPUTS_MAGIC.trim_end().to_string()];
    for inc in includes {
        lines.push(format!(
            "-I {}",
            canonicalize_include_path(&inc, source_dir, build_dir, module_dir)
        ));
    }
    for (name, value) in defines {
        match value {
            Some(v) => lines.push(format!("-D{name}={v}")),
            None => lines.push(format!("-D{name}")),
        }
    }
    let mut out = lines.join("\n");
    out.push('\n');
    Ok(out.into_bytes())
}

type MakefileDefines = Vec<(String, Option<String>)>;

fn parse_makefile_bindgen_lines(text: &str) -> Result<(Vec<String>, MakefileDefines), String> {
    let mut all_incs = Vec::new();
    let mut cflags_includes = Vec::new();
    let mut defines = Vec::new();
    let mut logical = String::new();
    for raw_line in text.lines() {
        if let Some(part) = raw_line.strip_suffix('\\') {
            logical.push_str(part);
            continue;
        }
        logical.push_str(raw_line);
        if let Some(tail) = logical.strip_prefix("ALL_INCS") {
            parse_makefile_flags(tail, &mut all_incs, &mut defines)?;
        } else if let Some(tail) = logical.strip_prefix("CFLAGS") {
            parse_makefile_flags(tail, &mut cflags_includes, &mut defines)?;
        }
        logical.clear();
    }
    cflags_includes.extend(all_incs);
    Ok((cflags_includes, defines))
}

fn parse_makefile_flags(
    line: &str,
    includes: &mut Vec<String>,
    defines: &mut MakefileDefines,
) -> Result<(), String> {
    let words = split_shell_words(line)?;
    let mut idx = 0;
    while idx < words.len() {
        let word = &words[idx];
        if let Some(inc) = word.strip_prefix("-I") {
            let value = if inc.is_empty() {
                idx += 1;
                words
                    .get(idx)
                    .cloned()
                    .ok_or_else(|| "Makefile -I is missing a path".to_string())?
            } else {
                inc.to_string()
            };
            includes.push(value);
        } else if let Some(def) = word.strip_prefix("-D") {
            let def = if def.is_empty() {
                idx += 1;
                words
                    .get(idx)
                    .cloned()
                    .ok_or_else(|| "Makefile -D is missing a name".to_string())?
            } else {
                def.to_string()
            };
            if let Some((name, value)) = def.split_once('=') {
                defines.push((name.to_string(), Some(value.to_string())));
            } else {
                defines.push((def, None));
            }
        }
        idx += 1;
    }
    Ok(())
}

fn canonicalize_include_path(
    inc: &str,
    source_dir: &Path,
    build_dir: &Path,
    module_dir: &Path,
) -> String {
    let path = Path::new(inc);
    if let Some(mapped) = map_known_dir(path, source_dir, build_dir, module_dir) {
        return mapped;
    }
    if path.is_relative() {
        if let Some(mapped) =
            map_known_dir(&source_dir.join(path), source_dir, build_dir, module_dir)
        {
            return mapped;
        }
    }
    inc.to_string()
}

fn map_known_dir(
    path: &Path,
    source_dir: &Path,
    build_dir: &Path,
    module_dir: &Path,
) -> Option<String> {
    if same_path(path, build_dir) {
        return Some(PLACEHOLDER_BUILD.to_string());
    }
    if same_path(path, source_dir) {
        return Some(PLACEHOLDER_SOURCE.to_string());
    }
    if same_path(path, module_dir) {
        return Some(PLACEHOLDER_MODULE.to_string());
    }
    None
}

fn same_path(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(ca), Ok(cb)) => ca == cb,
        _ => false,
    }
}
