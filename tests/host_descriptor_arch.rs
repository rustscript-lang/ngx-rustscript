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

#[test]
fn production_has_one_descriptor_binding_authority() {
    let host_mod = read("src/host/mod.rs");
    assert!(
        !host_mod.contains("fn ngx_host_bindings("),
        "production must not keep a parallel ngx_host_bindings table"
    );
    assert!(
        !host_mod.contains("enum NgxHostWrapper"),
        "production must not keep a parallel NgxHostWrapper bind table"
    );
    let build = extract_fn(&host_mod, "fn build_ngx_host_registry");
    let build_code: String = build
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        build_code.contains("HostFunctionRegistry::empty()")
            && build_code.contains("set_capability_profile")
            && build_code.contains("transactionally"),
        "ngx_host_registry must install from empty() plus an explicit capability profile:\n{build}"
    );
    assert!(
        !build_code.contains("HostFunctionRegistry::new()"),
        "ngx_host_registry must not seed HTTP adapters from HostFunctionRegistry::new():\n{build}"
    );
    assert!(
        host_mod.contains("install_from_catalog"),
        "ngx_host_registry must install exact catalog adapters"
    );
    assert!(
        host_mod.contains("authorize_registered_builtin_import")
            || host_mod.contains("install_from_catalog"),
        "restricted install must authorize catalogued hosts in the same transaction"
    );

    let bind_ngx = extract_fn(&host_mod, "fn bind_ngx_hosts");
    let bind_production = extract_fn(&host_mod, "fn bind_production_hosts");
    assert!(
        (bind_ngx.contains("ngx_host_registry") && bind_ngx.contains("bind_vm_cached"))
            || (bind_ngx.contains("bind_production_hosts")
                && bind_production.contains("ngx_host_registry")
                && bind_production.contains("bind_vm_cached")),
        "bind_ngx_hosts must be a registry wrapper, not a name/arity bind loop:\n{bind_ngx}\n{bind_production}"
    );
    assert!(
        !bind_ngx.contains("bind_static_function")
            && !bind_ngx.contains("bind_static_args_function")
            && !bind_production.contains("bind_static_function")
            && !bind_production.contains("bind_static_args_function"),
        "bind_ngx_hosts must not name-bind wrappers:\n{bind_ngx}\n{bind_production}"
    );

    let request_state = read("src/request_state.rs");
    let bind_program = extract_fn(&request_state, "fn bind_program_hosts");
    assert!(
        bind_program.contains("ngx_host_registry")
            || bind_program.contains("bind_production_hosts")
            || bind_program.contains("install_timer_vm")
            || bind_program.contains("bind_ngx_hosts"),
        "request VMs must bind through the production registry:\n{bind_program}"
    );
    assert!(
        !bind_program.contains("bind_ngx_hosts(vm)") || bind_ngx.contains("bind_vm_cached"),
        "request VMs must not keep a second name-bind authority"
    );

    let vm_pool = read("src/vm_pool.rs");
    assert!(
        vm_pool.contains("ngx_host_registry")
            || vm_pool.contains("bind_production_hosts")
            || (vm_pool.contains("bind_ngx_hosts") && bind_ngx.contains("bind_vm_cached")),
        "pool VMs must bind through the production registry"
    );
    assert!(
        !vm_pool.contains("bind_static_function"),
        "pool construction must not name-bind hosts"
    );

    let timer = read("src/timer.rs");
    assert!(
        (timer.contains("ngx_host_registry") && timer.contains("bind_vm_cached"))
            || timer.contains("bind_production_hosts"),
        "timer/callback VMs must bind through ngx_host_registry"
    );
}

#[test]
fn worker_http_clients_are_injected_leases_without_per_vm_fallback() {
    let standard = read("src/host/standard.rs");
    let install_policy = extract_fn(&standard, "fn install_standard_policy");
    assert!(
        install_policy.contains("HttpClientLease") && install_policy.contains("configure_http"),
        "standard policy must inject an embedding-owned HTTP lease:\\n{install_policy}"
    );
    assert!(
        !install_policy.contains("HttpWorkerResources::new"),
        "standard policy must not construct a per-VM HTTP resource owner:\\n{install_policy}"
    );

    let runtime = read("src/runtime.rs");
    assert!(
        runtime.contains("HttpWorkerResources") && runtime.contains("http_lease_for"),
        "the worker runtime must own HTTP resources and issue leases"
    );
    let take = extract_fn(&runtime, "fn take");
    assert!(
        !take.contains("HttpWorkerResources::new"),
        "VM pool construction must not create a standalone HTTP client:\\n{take}"
    );

    let request_state = read("src/request_state.rs");
    let bind_program = extract_fn(&request_state, "fn bind_program_hosts");
    assert!(
        bind_program.contains("HttpClientLease"),
        "request VM host bind must receive a worker HTTP lease:\\n{bind_program}"
    );

    let bridge = read("src/async_bridge.rs");
    let notify = read("src/notify.rs");
    let runtime_eventfd = extract_fn(&runtime, "unsafe extern \"C\" fn eventfd_read_handler");
    let runtime_waker = extract_fn(&runtime, "fn waker");
    for (name, source) in [
        ("NgxAsyncBridge", bridge),
        ("notify", notify),
        ("runtime eventfd handler", runtime_eventfd),
        ("runtime eventfd waker", runtime_waker),
    ] {
        assert!(
            !source.contains("HttpConfig")
                && !source.contains("HttpClientLease")
                && !source.contains("HttpWorkerResources")
                && !source.contains("HttpWorkerShutdown")
                && !source.contains("hyper::"),
            "{name} must stay host-agnostic with no HTTP types:\\n{source}"
        );
    }
}

fn extract_fn(source: &str, signature: &str) -> String {
    let start = source
        .find(signature)
        .unwrap_or_else(|| panic!("missing {signature}"));
    let rest = &source[start..];
    let mut depth = 0i32;
    let mut seen_body = false;
    for (idx, ch) in rest.char_indices() {
        match ch {
            '{' => {
                depth += 1;
                seen_body = true;
            }
            '}' => {
                depth -= 1;
                if seen_body && depth == 0 {
                    return rest[..=idx].to_string();
                }
            }
            _ => {}
        }
    }
    rest.to_string()
}
