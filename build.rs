#[path = "support/fingerprint.rs"]
mod fingerprint;

use std::path::PathBuf;

/// Forward `nginx-sys` build metadata into this crate.
///
/// `nginx-sys` must be a direct dependency so Cargo exports `DEP_NGINX_*`.
/// See https://github.com/rust-lang/cargo/issues/3544 and the ngx-rust build.rs.
fn main() {
    println!("cargo::rerun-if-env-changed=DEP_NGINX_FEATURES_CHECK");
    println!(
        "cargo::rustc-check-cfg=cfg(ngx_feature, values({}))",
        std::env::var("DEP_NGINX_FEATURES_CHECK").unwrap_or_else(|_| "any()".to_string())
    );
    println!("cargo::rerun-if-env-changed=DEP_NGINX_FEATURES");
    if let Ok(features) = std::env::var("DEP_NGINX_FEATURES") {
        for feature in features.split(',').map(str::trim) {
            println!("cargo::rustc-cfg=ngx_feature=\"{feature}\"");
        }
    }

    println!("cargo::rerun-if-env-changed=DEP_NGINX_OS_CHECK");
    println!(
        "cargo::rustc-check-cfg=cfg(ngx_os, values({}))",
        std::env::var("DEP_NGINX_OS_CHECK").unwrap_or_else(|_| "any()".to_string())
    );
    println!("cargo::rerun-if-env-changed=DEP_NGINX_OS");
    if let Ok(os) = std::env::var("DEP_NGINX_OS") {
        println!("cargo::rustc-cfg=ngx_os=\"{os}\"");
    }

    println!("cargo::rerun-if-env-changed=DEP_NGINX_VERSION_NUMBER");
    if let Ok(version) = std::env::var("DEP_NGINX_VERSION_NUMBER") {
        let version: u64 = version.parse().unwrap();
        println!("cargo::rustc-check-cfg=cfg(nginx1_21_1)");
        println!("cargo::rustc-check-cfg=cfg(nginx1_25_1)");
        if version >= 1_021_001 {
            println!("cargo::rustc-cfg=nginx1_21_1");
        }
        if version >= 1_025_001 {
            println!("cargo::rustc-cfg=nginx1_25_1");
        }
    }

    println!("cargo::rerun-if-env-changed=DEP_NGINX_BUILD_DIR");
    if let Ok(build_dir) = std::env::var("DEP_NGINX_BUILD_DIR") {
        println!("cargo::rustc-env=DEP_NGINX_BUILD_DIR={build_dir}");
    }

    println!("cargo::rerun-if-env-changed=NGINX_SOURCE_DIR");
    println!("cargo::rerun-if-env-changed=NGINX_BUILD_DIR");

    if cfg!(target_os = "macos") {
        println!("cargo::rustc-link-arg=-undefined");
        println!("cargo::rustc-link-arg=dynamic_lookup");
    }

    write_source_fingerprint();
}

fn require_env_dir(name: &str) -> PathBuf {
    let raw = std::env::var_os(name).unwrap_or_else(|| panic!("{name} must be set"));
    if raw.is_empty() {
        panic!("{name} must be set");
    }
    let path = PathBuf::from(raw);
    if !path.is_dir() {
        panic!("{name} is not a directory: {}", path.display());
    }
    path
}

fn write_source_fingerprint() {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    for rel in fingerprint::SOURCE_FINGERPRINT_INPUTS {
        println!("cargo::rerun-if-changed={rel}");
    }
    let nginx_source_dir = require_env_dir("NGINX_SOURCE_DIR");
    let nginx_build_dir = require_env_dir("NGINX_BUILD_DIR");
    for (_, path) in fingerprint::nginx_fingerprint_paths(&nginx_source_dir, &nginx_build_dir) {
        println!("cargo::rerun-if-changed={}", path.display());
    }

    let owned =
        fingerprint::collect_fingerprint_owned(&manifest_dir, &nginx_source_dir, &nginx_build_dir)
            .unwrap_or_else(|err| panic!("{err}"));
    let digest = fingerprint::fingerprint_from_owned(&owned);
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap())
        .join("ngx_rustscript_source_fingerprint.bin");
    std::fs::write(&out, digest).unwrap_or_else(|err| panic!("write {}: {err}", out.display()));
}
