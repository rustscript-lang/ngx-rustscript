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

    let include = std::env::var_os("DEP_NGINX_INCLUDE").unwrap_or_else(|| {
        panic!("DEP_NGINX_INCLUDE is required to compile src/nginx_request_shim.c")
    });
    if let Ok(include) = std::env::var("DEP_NGINX_INCLUDE") {
        println!("cargo::rustc-env=DEP_NGINX_INCLUDE={include}");
    }
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
    for path in std::env::split_paths(&include) {
        build.include(path);
    }
    if let Ok(cflags) = std::env::var("DEP_NGINX_CFLAGS") {
        for flag in cflags.split_whitespace() {
            build.flag(flag);
        }
    }
    build.compile("ngx_rustscript_request_shim");
}
