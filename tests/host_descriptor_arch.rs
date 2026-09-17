//! Architecture guard: ngx hosts are owned by explicit module descriptors.
//!
//! Catalog schemas, exact adapters, resource declarations, and named structs
//! must come from those descriptors. Hand-written parallel `schema_for` /
//! `builder.function` / `register_exact_*` loops for migrated functions are
//! forbidden.

use std::fs;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(path: impl AsRef<Path>) -> String {
    fs::read_to_string(repo_root().join(path)).expect("source file")
}

fn rust_sources_under(dir: &str) -> Vec<(String, String)> {
    let mut files = Vec::new();
    let root = repo_root().join(dir);
    for entry in fs::read_dir(&root).unwrap_or_else(|_| panic!("read {dir}")) {
        let entry = entry.expect("dirent");
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
            let rel = path
                .strip_prefix(repo_root())
                .expect("relative")
                .to_string_lossy()
                .into_owned();
            files.push((rel, fs::read_to_string(path).expect("read rust")));
        }
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));
    files
}

fn pd_host_function_names(source: &str) -> Vec<String> {
    let mut names = Vec::new();
    for line in source.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("#[pd_host_function(name = \"") {
            if let Some(end) = rest.find('"') {
                names.push(rest[..end].to_string());
            }
        }
    }
    names
}

#[test]
fn production_host_functions_are_owned_by_explicit_module_descriptors() {
    let host_mod = read("src/host/mod.rs");
    assert!(
        host_mod.contains("HostModuleDescriptor"),
        "src/host/mod.rs must compose explicit HostModuleDescriptor values"
    );
    assert!(
        host_mod.contains("ngx_host_modules"),
        "src/host/mod.rs must expose ngx_host_modules() as the deterministic composition list"
    );

    let mut production = Vec::new();
    for (path, source) in rust_sources_under("src/host") {
        if path.ends_with("output_stub_tests.rs") {
            continue;
        }
        for name in pd_host_function_names(&source) {
            if name == "ngx::probe" || name == "ngx::ready" || name == "ngx::fail" {
                continue;
            }
            production.push((path.clone(), name));
        }
    }
    assert!(
        production.len() >= 130,
        "expected the production ngx host surface, found {}",
        production.len()
    );

    let composed = format!("{}{}", host_mod, read("src/host/timer.rs"));
    for (path, name) in &production {
        let _ = path;
        assert!(
            composed.contains(&format!("name = \"{name}\""))
                || composed.contains(&format!("\"{name}\""))
                || rust_sources_under("src/host")
                    .iter()
                    .any(|(_, src)| src.contains(name) && src.contains("_descriptor")),
            "{name} must be listed in an explicit module descriptor"
        );
    }
}

#[test]
fn catalog_is_not_built_from_a_parallel_schema_for_table() {
    let catalog = read("src/host/catalog.rs");
    assert!(
        !catalog.contains("fn schema_for("),
        "src/host/catalog.rs must not keep a parallel schema_for table"
    );
    assert!(
        !catalog.contains("builder.function("),
        "ngx catalog must not loop HostApiBuilder::function for migrated hosts"
    );
    assert!(
        !catalog.contains("builder.resource("),
        "resource declarations must come from HostResourceType / module resources, not builder.resource"
    );
}

#[test]
fn registry_installs_descriptors_transactionally_instead_of_binding_loops() {
    let host_mod = read("src/host/mod.rs");
    assert!(
        host_mod.contains("install_from_catalog") || host_mod.contains(".install("),
        "ngx_host_registry must install module descriptors transactionally"
    );
    assert!(
        !host_mod.contains("register_catalog_static("),
        "migrated hosts must not register through a parallel register_catalog_static loop"
    );
    assert!(
        !host_mod.contains("register_catalog_static_args("),
        "migrated hosts must not register through a parallel register_catalog_static_args loop"
    );
}

#[test]
fn timer_owned_adapters_come_from_descriptors() {
    let timer = read("src/host/timer.rs");
    assert!(
        timer.contains("HostOwnedAdapterFactory") || timer.contains("HostAdapterDescriptor::Owned"),
        "timer at/every must select the owned descriptor adapter path"
    );
    assert!(
        timer.contains("HostFunctionDescriptor") || timer.contains("_descriptor"),
        "timer hosts must have descriptor sources"
    );
    assert!(
        !timer.contains("register_exact_owned("),
        "timer must not keep a parallel register_exact_owned loop"
    );
}

#[test]
fn handle_types_declare_host_resource_type() {
    let mut impls = 0usize;
    for (path, source) in [
        ("src/host/shared_dict.rs", read("src/host/shared_dict.rs")),
        ("src/semaphore.rs", read("src/semaphore.rs")),
        ("src/socket/tcp.rs", read("src/socket/tcp.rs")),
        ("src/socket/udp.rs", read("src/socket/udp.rs")),
        ("src/socket/tls.rs", read("src/socket/tls.rs")),
    ] {
        assert!(
            source.contains("impl HostResourceType for")
                || source.contains("impl vm::HostResourceType for"),
            "{path} must implement HostResourceType"
        );
        impls += source.matches("impl HostResourceType for").count()
            + source.matches("impl vm::HostResourceType for").count();
    }
    assert!(
        impls >= 6,
        "expected HostResourceType for dict/semaphore/tcp/udp/tls store/cert, found {impls}"
    );
}
