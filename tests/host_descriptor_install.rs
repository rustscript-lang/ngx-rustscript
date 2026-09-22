//! Restricted exact-install negatives for ngx module descriptors.
//!
//! Production uses [`ngx_rustscript::host::ngx_host_registry`] (restricted
//! registry plus transactional `install_from_catalog` and exact authorization).
//! These tests prove deny, mismatch, and rollback stay fail-closed.

use ngx_rustscript_nginx_test_stubs as _;

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Instant;

use ngx_rustscript::config::compile_rss_source;
use ngx_rustscript::host::catalog::ngx_host_catalog;
use ngx_rustscript::host::{bind_registry_for_test, ngx_host_modules, ngx_host_registry};
use vm::{
    HostApiBuilder, HostBindingDescriptor, HostBindingKind, HostFunctionDescriptor,
    HostFunctionRegistry, HostFunctionSchema, HostParamSchema, HostResourceTypeMeta,
    HostStructField, HostTypeSchema, ResourceTypeKey, ResourceTypeSchema, TypeSchema, Vm,
};

fn codec_module() -> vm::HostModuleDescriptor {
    ngx_host_modules()
        .into_iter()
        .find(|module| module.name == "ngx.codec")
        .expect("ngx.codec module")
}

#[test]
fn production_registry_installs_every_catalogued_name() {
    let catalog = ngx_host_catalog();
    let registry = ngx_host_registry().expect("ngx host registry");
    for schema in catalog.functions() {
        assert!(
            registry.contains_name(&schema.name),
            "installed registry must contain {}",
            schema.name
        );
    }
}

#[test]
fn production_registry_is_restricted_and_authorizes_only_catalogued_hosts() {
    let catalog = ngx_host_catalog();
    let deny_program = compile_rss_source(r#"use ngx; ngx::escape_uri("a", null);"#)
        .expect("ngx::escape_uri should compile against the catalog");
    let mut denied = Vm::new_shared(deny_program);
    let error = HostFunctionRegistry::restricted()
        .bind_vm_cached(&mut denied)
        .expect_err("restricted registry must deny ngx hosts before install");
    let message = error.to_string();
    assert!(
        message.contains("unbound")
            || message.contains("capability")
            || message.contains("not allow")
            || message.contains("denied"),
        "restricted deny-before message: {message}"
    );

    let registry = ngx_host_registry().expect("production registry");
    let allow_program = compile_rss_source(r#"use ngx; ngx::escape_uri("a", null);"#)
        .expect("ngx::escape_uri should compile against the catalog");
    let mut allowed = Vm::new_shared(allow_program);
    registry
        .bind_vm_cached(&mut allowed)
        .expect("catalogued ngx::escape_uri must bind after exact install");
    allowed
        .run()
        .expect("ngx::escape_uri must execute after exact registry bind");

    let unrelated = compile_rss_source("let x = 1;\n").expect("import-free program");
    let mut unrelated_vm = Vm::new_shared(unrelated);
    registry
        .bind_vm_cached(&mut unrelated_vm)
        .expect("import-free programs still bind");
    unrelated_vm.run().expect("import-free program runs");
    assert!(
        !registry.contains_name("vendor::evil") && !registry.contains_name("ngx::not_a_host"),
        "unlisted names must stay out of the production registry"
    );
    assert!(
        catalog.function("ngx::escape_uri").is_some(),
        "deny-before/allow-after uses a real catalogued host"
    );
}

#[test]
fn request_pool_and_timer_bind_through_the_same_descriptor_registry() {
    let registry = ngx_host_registry().expect("production registry");
    let catalog = ngx_host_catalog();

    let request_program = compile_rss_source(r#"use ngx; ngx::md5("x");"#).expect("md5 program");
    let mut request_vm = Vm::new_shared(request_program);
    registry
        .bind_vm_cached(&mut request_vm)
        .expect("request VM bind");
    request_vm.run().expect("request VM executes ngx::md5");

    let pool_program = compile_rss_source(r#"use ngx; ngx::md5("x");"#).expect("md5 program");
    let mut pool_vm = Vm::new_shared(pool_program);
    ngx_rustscript::host::bind_registry_for_test(&mut pool_vm).expect("bind test registry");
    pool_vm.run().expect("pool VM executes ngx::md5");

    let timer_program = compile_rss_source(
        r#"
        use ngx;
        ngx::timer::pending_count();
        "#,
    )
    .expect("timer program");
    let mut timer_vm = Vm::new_shared(timer_program);
    registry
        .bind_vm_cached(&mut timer_vm)
        .expect("timer VM bind");
    ngx_rustscript::timer::install_timer_runtime_on_vm(&mut timer_vm);
    timer_vm
        .run()
        .expect("timer VM executes ngx::timer::pending_count");
    assert!(registry.contains_name("ngx::md5"));

    for schema in catalog.functions() {
        assert!(
            registry.contains_name(&schema.name),
            "one registry must own {}",
            schema.name
        );
    }
}

#[test]
fn restricted_registry_denies_undeclared_ngx_admission() {
    let program = compile_rss_source(r#"use ngx; ngx::escape_uri("a", null);"#)
        .expect("ngx::escape_uri should compile against the catalog");
    let mut vm = Vm::new_shared(program);
    let error = HostFunctionRegistry::restricted()
        .bind_vm_cached(&mut vm)
        .expect_err("restricted registry must deny undeclared ngx hosts");
    let message = error.to_string();
    assert!(
        message.contains("unbound")
            || message.contains("capability")
            || message.contains("restricted")
            || message.contains("not allow")
            || message.contains("undeclared")
            || message.contains("denied"),
        "restricted deny message: {message}"
    );
}

#[test]
fn skewed_caller_catalog_rolls_back_without_partial_install() {
    let module = codec_module();
    let mut skewed = HostApiBuilder::new();
    skewed.function(HostFunctionSchema::with_return(
        "ngx::escape_uri",
        vec![HostParamSchema::value("uri", HostTypeSchema::Int)],
        HostTypeSchema::String,
    ));
    let skewed = skewed.build().expect("skewed catalog validates");
    let mut registry = HostFunctionRegistry::empty();
    let error = module
        .install_from_catalog(&mut registry, &skewed)
        .expect_err("a catalog that disagrees with descriptors must fail");
    assert!(
        !registry.contains_name("ngx::escape_uri"),
        "rejected install must leave the registry unchanged: {error}"
    );
}

#[test]
fn binding_adapter_mismatch_fails_closed() {
    let mut descriptors: Vec<HostFunctionDescriptor> = codec_module().descriptors();
    let first = descriptors
        .first_mut()
        .expect("ngx.codec has at least one descriptor");
    first.binding = HostBindingDescriptor {
        kind: HostBindingKind::Owned,
    };
    let mut registry = HostFunctionRegistry::empty();
    let error = vm::HostModuleDescriptor::install_descriptors(&mut registry, &descriptors)
        .expect_err("binding/adapter mismatch must fail closed");
    assert!(
        error.to_string().contains("mismatch") || error.to_string().contains("binding"),
        "{error}"
    );
    assert!(
        !registry.contains_name("ngx::escape_uri"),
        "mismatch must not install a partial codec module"
    );
}

#[test]
fn duplicate_module_install_does_not_leak_a_second_declaration() {
    let catalog = ngx_host_catalog();
    let module = codec_module();
    let mut registry = HostFunctionRegistry::empty();
    module
        .install_from_catalog(&mut registry, &catalog)
        .expect("first codec install");
    let error = module
        .install_from_catalog(&mut registry, &catalog)
        .expect_err("duplicate declaration must fail");
    let message = error.to_string();
    assert!(
        message.contains("duplicate") || message.contains("already"),
        "duplicate install message: {message}"
    );
}

struct RegistrySnapshot {
    names: Vec<String>,
    named_structs: BTreeMap<String, String>,
    allows_escape_uri: bool,
    plan: *const (),
}

fn catalog_names_in(registry: &HostFunctionRegistry) -> Vec<String> {
    let catalog = ngx_host_catalog();
    let mut names: Vec<String> = catalog
        .functions()
        .iter()
        .filter(|schema| registry.contains_name(&schema.name))
        .map(|schema| schema.name.clone())
        .collect();
    for extra in [
        "ngx::probe",
        "ngx::fail",
        "vendor::evil",
        "ngx::decode_args_conflict",
    ] {
        if registry.contains_name(extra) {
            names.push(extra.to_string());
        }
    }
    names.sort();
    names
}

fn allows_escape_uri(registry: &HostFunctionRegistry) -> bool {
    let program = compile_rss_source(r#"use ngx; ngx::escape_uri("a", null);"#)
        .expect("ngx::escape_uri compiles");
    let mut vm = Vm::new_shared(program);
    registry.bind_vm_cached(&mut vm).is_ok()
}

fn snapshot(registry: &HostFunctionRegistry) -> RegistrySnapshot {
    let mut named_structs = BTreeMap::new();
    for (name, schema) in registry.named_struct_schemas() {
        named_structs.insert(name.clone(), format!("{schema:?}"));
    }
    let plan = registry
        .prepare_shared_plan(&[])
        .expect("empty-import plan");
    RegistrySnapshot {
        names: catalog_names_in(registry),
        named_structs,
        allows_escape_uri: allows_escape_uri(registry),
        plan: Arc::as_ptr(&plan) as *const (),
    }
}

fn assert_snapshot_eq(before: &RegistrySnapshot, after: &RegistrySnapshot, why: &str) {
    assert_eq!(before.names, after.names, "{why}: entries");
    assert_eq!(
        before.named_structs, after.named_structs,
        "{why}: named structs"
    );
    assert_eq!(
        before.allows_escape_uri, after.allows_escape_uri,
        "{why}: capabilities"
    );
    assert_eq!(before.plan, after.plan, "{why}: generation token");
}

fn semaphore_key() -> ResourceTypeKey {
    ResourceTypeKey::new("ngx.semaphore").expect("semaphore resource key")
}

fn resource_meta<T: 'static>(description: &str) -> HostResourceTypeMeta {
    HostResourceTypeMeta {
        schema: ResourceTypeSchema::new(semaphore_key(), description),
        type_id: std::any::TypeId::of::<T>(),
        type_name: std::any::type_name::<T>(),
    }
}

struct SemaphoreTypeA;
struct SemaphoreTypeB;

#[test]
fn skewed_caller_catalog_snapshot_is_unchanged() {
    let module = codec_module();
    let mut skewed = HostApiBuilder::new();
    skewed.function(HostFunctionSchema::with_return(
        "ngx::escape_uri",
        vec![HostParamSchema::value("uri", HostTypeSchema::Int)],
        HostTypeSchema::String,
    ));
    let skewed = skewed.build().expect("skewed catalog validates");
    let mut registry = HostFunctionRegistry::restricted();
    let before = snapshot(&registry);
    module
        .install_from_catalog(&mut registry, &skewed)
        .expect_err("a catalog that disagrees with descriptors must fail");
    assert_snapshot_eq(&before, &snapshot(&registry), "skewed catalog rollback");
}

#[test]
fn binding_adapter_mismatch_snapshot_is_unchanged() {
    let mut descriptors: Vec<HostFunctionDescriptor> = codec_module().descriptors();
    let first = descriptors
        .first_mut()
        .expect("ngx.codec has at least one descriptor");
    first.binding = HostBindingDescriptor {
        kind: HostBindingKind::Owned,
    };
    let mut registry = HostFunctionRegistry::restricted();
    let before = snapshot(&registry);
    vm::HostModuleDescriptor::install_descriptors(&mut registry, &descriptors)
        .expect_err("binding/adapter mismatch must fail closed");
    assert_snapshot_eq(&before, &snapshot(&registry), "adapter mismatch rollback");
}

#[test]
fn named_struct_shape_conflict_rolls_back() {
    let catalog = ngx_host_catalog();
    let module = codec_module();
    let mut registry = HostFunctionRegistry::restricted();
    module
        .install_from_catalog(&mut registry, &catalog)
        .expect("codec install");
    let before = snapshot(&registry);

    let mut descriptors = module.descriptors();
    for descriptor in &mut descriptors {
        if descriptor.schema.name == "ngx::escape_uri" {
            descriptor.schema.return_type = HostTypeSchema::named_struct(
                "DecodedArgsResult",
                vec![HostStructField::new("wrong", HostTypeSchema::Int)],
            );
        }
    }
    vm::HostModuleDescriptor::install_descriptors(&mut registry, &descriptors)
        .expect_err("named struct shape conflict must fail");
    assert_snapshot_eq(&before, &snapshot(&registry), "named struct shape rollback");

    let mut conflict = HashMap::new();
    conflict.insert("DecodedArgsResult".to_string(), TypeSchema::Int);
    let error = registry.install_named_struct_schemas(conflict);
    assert!(error.is_err(), "registry named-struct shape conflict");
    assert_snapshot_eq(
        &before,
        &snapshot(&registry),
        "named struct registry shape rollback",
    );
}

#[test]
fn resource_type_and_doc_conflicts_roll_back() {
    let mut registry = HostFunctionRegistry::restricted();
    let before = snapshot(&registry);

    let mut type_conflict = codec_module().descriptors();
    type_conflict[0]
        .resource_types
        .push(resource_meta::<SemaphoreTypeA>("semaphore type A"));
    type_conflict[0]
        .resource_types
        .push(resource_meta::<SemaphoreTypeB>("semaphore type B"));
    vm::HostModuleDescriptor::install_descriptors(&mut registry, &type_conflict)
        .expect_err("resource type_id conflict must fail");
    assert_snapshot_eq(&before, &snapshot(&registry), "resource type rollback");

    let mut doc_conflict = codec_module().descriptors();
    doc_conflict[0]
        .resource_types
        .push(resource_meta::<SemaphoreTypeA>("docs A"));
    doc_conflict[0]
        .resource_types
        .push(resource_meta::<SemaphoreTypeA>("docs B"));
    vm::HostModuleDescriptor::install_descriptors(&mut registry, &doc_conflict)
        .expect_err("resource documentation conflict must fail");
    assert_snapshot_eq(&before, &snapshot(&registry), "resource doc rollback");
}

#[test]
fn first_bind_after_registry_install_stays_off_the_request_path() {
    use std::time::Duration;

    ngx_host_registry().expect("registry install");
    let program = compile_rss_source(r#"use ngx; ngx::sleep_ms(1);"#).expect("compile");
    let mut vm = Vm::new_shared(program);
    let started = Instant::now();
    bind_registry_for_test(&mut vm).expect("bind test registry");
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_millis(20),
        "bind after worker-init registry install must not block a request: {elapsed:?}"
    );
}
