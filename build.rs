use std::path::{Path, PathBuf};

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
    println!("cargo::rustc-check-cfg=cfg(ngx_features, values(\"rustc-test\"))");
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
    println!("cargo::rerun-if-env-changed=NGX_RUSTSCRIPT_INTEGRATION_HOOKS");
    println!("cargo::rustc-check-cfg=cfg(ngx_rustscript_integration_hooks)");
    // Only the current compile-time opt-in enables hooks. The fingerprint is
    // always emitted so a leftover build-script stdout cannot keep the cfg on
    // after NGX_RUSTSCRIPT_INTEGRATION_HOOKS is unset or set to anything else.
    let hooks_enabled = std::env::var("NGX_RUSTSCRIPT_INTEGRATION_HOOKS").as_deref() == Ok("1");
    println!(
        "cargo::rustc-env=NGX_RUSTSCRIPT_HOOKS_FINGERPRINT={}",
        if hooks_enabled { "1" } else { "0" }
    );
    if hooks_enabled {
        println!("cargo::rustc-cfg=ngx_rustscript_integration_hooks");
    }

    compile_request_shim();

    if cfg!(target_os = "macos") {
        println!("cargo::rustc-link-arg=-undefined");
        println!("cargo::rustc-link-arg=dynamic_lookup");
    }
}

fn compile_request_shim() {
    println!("cargo::rerun-if-changed=src/nginx_request_shim.c");
    println!("cargo::rerun-if-env-changed=DEP_NGINX_INCLUDE");
    println!("cargo::rerun-if-env-changed=DEP_NGINX_CFLAGS");

    let dependency_include = std::env::var_os("DEP_NGINX_INCLUDE").unwrap_or_else(|| {
        panic!("DEP_NGINX_INCLUDE is required to compile src/nginx_request_shim.c")
    });
    let includes = selected_nginx_include_paths(&dependency_include);
    let encoded_includes = std::env::join_paths(&includes)
        .expect("selected NGINX include paths must be representable as DEP_NGINX_INCLUDE");
    println!(
        "cargo::rustc-env=DEP_NGINX_INCLUDE={}",
        encoded_includes.to_string_lossy()
    );
    if let Ok(cflags) = std::env::var("DEP_NGINX_CFLAGS") {
        println!("cargo::rustc-env=DEP_NGINX_CFLAGS={cflags}");
    } else {
        println!("cargo::rustc-env=DEP_NGINX_CFLAGS=");
    }

    let mut build = cc::Build::new();
    build
        .file("src/nginx_request_shim.c")
        .warnings(true)
        .define("NGX_RUSTSCRIPT_NO_C_PUBLIC_ALIAS", None);
    for path in &includes {
        build.include(path);
    }
    if let Ok(cflags) = std::env::var("DEP_NGINX_CFLAGS") {
        for flag in cflags.split_whitespace() {
            build.flag(flag);
        }
    }
    build.compile("ngx_rustscript_request_shim");
}

fn selected_nginx_include_paths(dependency_include: &std::ffi::OsStr) -> Vec<PathBuf> {
    let source = std::env::var_os("NGINX_SOURCE_DIR").map(PathBuf::from);
    let build = std::env::var_os("NGINX_BUILD_DIR").map(PathBuf::from);
    match (source, build) {
        (Some(source), Some(build)) => {
            let makefile = build.join("Makefile");
            println!("cargo::rerun-if-changed={}", makefile.display());
            parse_makefile_includes(&makefile)
                .into_iter()
                .map(|path| {
                    if path.is_absolute() {
                        path
                    } else {
                        source.join(path)
                    }
                })
                .collect()
        }
        _ => std::env::split_paths(dependency_include).collect(),
    }
}

fn parse_makefile_includes(makefile: &Path) -> Vec<PathBuf> {
    let contents = std::fs::read_to_string(makefile).unwrap_or_else(|error| {
        panic!(
            "unable to read NGINX makefile {}: {error}",
            makefile.display()
        )
    });
    let mut includes = Vec::new();
    let mut logical = String::new();
    for raw in contents.lines() {
        if let Some(part) = raw.strip_suffix('\\') {
            logical.push_str(part);
            continue;
        }
        logical.push_str(raw);
        if let Some(fragment) = logical.strip_prefix("ALL_INCS") {
            let mut words = fragment.split_whitespace();
            while let Some(word) = words.next() {
                if let Some(include) = word.strip_prefix("-I") {
                    let include = if include.is_empty() {
                        words.next().expect("-I argument in NGINX Makefile")
                    } else {
                        include
                    };
                    includes.push(PathBuf::from(include));
                }
            }
        }
        logical.clear();
    }
    assert!(
        !includes.is_empty(),
        "NGINX makefile {} did not contain ALL_INCS include paths",
        makefile.display()
    );
    includes
}
