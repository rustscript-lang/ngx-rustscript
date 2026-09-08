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
}
