use std::path::{Path, PathBuf};

fn apply_nginx_headers(build: &mut cc::Build) {
    println!("cargo::rerun-if-env-changed=DEP_NGINX_INCLUDE");
    println!("cargo::rerun-if-env-changed=DEP_NGINX_CFLAGS");
    println!("cargo::rerun-if-env-changed=NGINX_SOURCE_DIR");
    println!("cargo::rerun-if-env-changed=NGINX_BUILD_DIR");

    for path in nginx_include_paths() {
        build.include(path);
    }
    if let Ok(cflags) = std::env::var("DEP_NGINX_CFLAGS") {
        for flag in cflags.split_whitespace() {
            build.flag(flag);
        }
    }
}

fn nginx_include_paths() -> Vec<PathBuf> {
    if let (Some(source_dir), Some(build_dir)) = (
        std::env::var_os("NGINX_SOURCE_DIR"),
        std::env::var_os("NGINX_BUILD_DIR"),
    ) {
        let source_dir = PathBuf::from(source_dir);
        let build_dir = PathBuf::from(build_dir);
        let makefile = build_dir.join("Makefile");
        println!("cargo::rerun-if-changed={}", makefile.display());
        return parse_makefile_includes(&makefile)
            .into_iter()
            .map(|path| {
                if path.is_absolute() {
                    path
                } else {
                    source_dir.join(path)
                }
            })
            .collect();
    }
    if let Some(include) = std::env::var_os("DEP_NGINX_INCLUDE") {
        return std::env::split_paths(&include).collect();
    }
    let source_dir = PathBuf::from(std::env::var_os("NGINX_SOURCE_DIR").unwrap_or_else(|| {
        panic!(
            "DEP_NGINX_INCLUDE or NGINX_SOURCE_DIR is required to compile request ABI test helpers"
        )
    }));
    let build_dir = PathBuf::from(std::env::var_os("NGINX_BUILD_DIR").unwrap_or_else(|| {
        panic!(
            "DEP_NGINX_INCLUDE or NGINX_BUILD_DIR is required to compile request ABI test helpers"
        )
    }));
    let makefile = build_dir.join("Makefile");
    println!("cargo::rerun-if-changed={}", makefile.display());
    parse_makefile_includes(&makefile)
        .into_iter()
        .map(|path| {
            if path.is_absolute() {
                path
            } else {
                source_dir.join(path)
            }
        })
        .collect()
}

fn parse_makefile_includes(makefile: &Path) -> Vec<PathBuf> {
    let contents = std::fs::read_to_string(makefile).unwrap_or_else(|err| {
        panic!(
            "unable to read NGINX makefile {}: {err}",
            makefile.display()
        )
    });
    let mut includes = Vec::new();
    let mut line = String::new();
    for raw in contents.lines() {
        if let Some(part) = raw.strip_suffix('\\') {
            line.push_str(part);
            continue;
        }
        line.push_str(raw);
        if let Some(tail) = line.strip_prefix("ALL_INCS") {
            parse_dash_i(tail, &mut includes);
        }
        line.clear();
    }
    if includes.is_empty() {
        panic!(
            "NGINX makefile {} did not contain ALL_INCS include paths",
            makefile.display()
        );
    }
    includes
}

fn parse_dash_i(fragment: &str, includes: &mut Vec<PathBuf>) {
    let mut words = fragment.split_whitespace();
    while let Some(word) = words.next() {
        if let Some(inc) = word.strip_prefix("-I") {
            let value = if inc.is_empty() {
                words.next().expect("-I argument")
            } else {
                inc
            };
            includes.push(PathBuf::from(value));
        }
    }
}

fn main() {
    println!("cargo::rerun-if-changed=stubs.c");
    println!("cargo::rerun-if-changed=request_abi.c");
    cc::Build::new()
        .file("stubs.c")
        .warnings(true)
        .compile("ngx_rustscript_nginx_test_stubs");

    let mut abi = cc::Build::new();
    abi.file("request_abi.c").warnings(true);
    apply_nginx_headers(&mut abi);
    abi.compile("ngx_rustscript_nginx_test_request_abi");
}
