//! Restricted exact-install negatives for ngx module descriptors.
//!
//! Production uses [`ngx_rustscript::host::ngx_host_registry`] (empty registry
//! plus transactional `install_from_catalog`). These tests prove deny,
//! mismatch, and rollback stay fail-closed.

use ngx_rustscript_nginx_test_stubs as _;

use ngx_rustscript::config::compile_rss_source;
use ngx_rustscript::host::catalog::ngx_host_catalog;
use ngx_rustscript::host::{ngx_host_modules, ngx_host_registry};
use vm::{
    HostApiBuilder, HostBindingDescriptor, HostBindingKind, HostFunctionDescriptor,
    HostFunctionRegistry, HostFunctionSchema, HostParamSchema, HostTypeSchema, Vm,
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
