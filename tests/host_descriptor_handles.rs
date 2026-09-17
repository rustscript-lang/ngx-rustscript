//! Typed-handle and raw-guest-token contract tests.
//!
//! Guest resource effects must match catalog passing modes. Sync methods that
//! do not need `&mut Vm` take `ResourceRef`. Unavoidable raw guest tokens
//! (constructors, async, close) still advertise Resource + mode on the
//! descriptor.

use ngx_rustscript_nginx_test_stubs as _;

use ngx_rustscript::host::{
    NGX_CTX_CLEAR_NAME, NGX_CTX_DELETE_NAME, NGX_CTX_GET_NAME, NGX_CTX_SET_NAME,
    NGX_SEMAPHORE_COUNT_NAME, NGX_SEMAPHORE_NEW_NAME, NGX_SEMAPHORE_POST_NAME,
    NGX_SEMAPHORE_WAIT_NAME, NGX_SHARED_DICT_NAME, NGX_SHARED_GET_NAME,
    NGX_SOCKET_TCP_CLIENTCERT_NAME, NGX_SOCKET_TCP_NAME, NGX_SOCKET_TCP_TRUSTEDSTORE_NAME,
    NGX_SOCKET_UDP_NAME, ngx_host_catalog, ngx_host_modules,
};
use vm::{
    HostEffect, HostFunctionDescriptor, HostParamPassing, HostTypeSchema, ResourceEffect,
    ResourceTypeKey,
};

fn all_descriptors() -> Vec<HostFunctionDescriptor> {
    ngx_host_modules()
        .into_iter()
        .flat_map(|module| module.descriptors())
        .collect()
}

fn descriptor_named(name: &str) -> HostFunctionDescriptor {
    all_descriptors()
        .into_iter()
        .find(|descriptor| descriptor.schema.name == name)
        .unwrap_or_else(|| panic!("missing descriptor {name}"))
}

fn guest_effects(descriptor: &HostFunctionDescriptor) -> Vec<ResourceEffect> {
    descriptor
        .effects
        .iter()
        .filter_map(HostEffect::guest_resource)
        .cloned()
        .collect()
}

fn resource_param_slots(
    schema: &vm::HostFunctionSchema,
) -> Vec<(usize, HostParamPassing, ResourceTypeKey)> {
    schema
        .params
        .iter()
        .enumerate()
        .filter_map(|(index, param)| match &param.ty {
            HostTypeSchema::Resource(key) => Some((index, param.passing, key.clone())),
            _ => None,
        })
        .collect()
}

fn expected_effects(schema: &vm::HostFunctionSchema) -> Vec<ResourceEffect> {
    let mut expected = Vec::new();
    for param in &schema.params {
        if let HostTypeSchema::Resource(key) = &param.ty {
            match param.passing {
                HostParamPassing::Borrow => expected.push(ResourceEffect::borrow(key.clone())),
                HostParamPassing::BorrowMut => {
                    expected.push(ResourceEffect::borrow_mut(key.clone()))
                }
                HostParamPassing::TakeOwned => {
                    expected.push(ResourceEffect::take_owned(key.clone()))
                }
                HostParamPassing::Value => {}
            }
        }
    }
    if let HostTypeSchema::Resource(key) = &schema.return_type {
        expected.push(ResourceEffect::create(key.clone()));
    }
    expected
}

#[test]
fn guest_resource_effects_match_every_descriptor_schema() {
    for descriptor in all_descriptors() {
        let mut expected = expected_effects(&descriptor.schema);
        let mut actual = guest_effects(&descriptor);
        let sort_key = |effect: &ResourceEffect| format!("{effect:?}");
        expected.sort_by_key(|left| sort_key(left));
        actual.sort_by_key(|left| sort_key(left));
        assert_eq!(
            expected, actual,
            "{} guest resource effects must follow the catalog contract",
            descriptor.schema.name
        );
    }
}

#[test]
fn constructors_create_typed_resources() {
    for (name, key) in [
        (NGX_SEMAPHORE_NEW_NAME, "ngx.semaphore"),
        (NGX_SHARED_DICT_NAME, "ngx.shared.dict"),
        (NGX_SOCKET_UDP_NAME, "ngx.socket.udp"),
        (NGX_SOCKET_TCP_NAME, "ngx.socket.tcp"),
        (
            NGX_SOCKET_TCP_TRUSTEDSTORE_NAME,
            "ngx.socket.tls.trustedstore",
        ),
        (NGX_SOCKET_TCP_CLIENTCERT_NAME, "ngx.socket.tls.clientcert"),
    ] {
        let descriptor = descriptor_named(name);
        match &descriptor.schema.return_type {
            HostTypeSchema::Resource(resource) => assert_eq!(resource.as_str(), key, "{name}"),
            other => panic!("{name} must return Resource({key}), got {other:?}"),
        }
        assert!(
            guest_effects(&descriptor)
                .iter()
                .any(|effect| matches!(effect, ResourceEffect::Create { key: created } if created.as_str() == key)),
            "{name} must advertise Create({key})"
        );
    }
}

#[test]
fn semaphore_post_and_count_borrow_typed_handles() {
    for name in [NGX_SEMAPHORE_POST_NAME, NGX_SEMAPHORE_COUNT_NAME] {
        let descriptor = descriptor_named(name);
        let slots = resource_param_slots(&descriptor.schema);
        assert_eq!(slots.len(), 1, "{name} has one semaphore handle slot");
        assert_eq!(slots[0].0, 0);
        assert_eq!(slots[0].1, HostParamPassing::Borrow);
        assert_eq!(slots[0].2.as_str(), "ngx.semaphore");
    }
}

#[test]
fn shared_dict_ops_borrow_typed_handles() {
    let descriptor = descriptor_named(NGX_SHARED_GET_NAME);
    let slots = resource_param_slots(&descriptor.schema);
    assert_eq!(slots[0].1, HostParamPassing::Borrow);
    assert_eq!(slots[0].2.as_str(), "ngx.shared.dict");
}

/// Raw guest token boundaries: function, slot index, passing mode, resource key.
///
/// These Rust signatures still take `i64` because async `ResourceRef` cannot
/// cross suspension, close/`push_resource` need `&mut Vm` (which cannot combine
/// with borrowed resources), or the function is a constructor return token.
const RAW_GUEST_TOKEN_PARAMS: &[(&str, usize, HostParamPassing, &str)] = &[
    (
        NGX_SEMAPHORE_WAIT_NAME,
        0,
        HostParamPassing::Borrow,
        "ngx.semaphore",
    ),
    (
        "ngx::socket::udp::bind",
        0,
        HostParamPassing::Borrow,
        "ngx.socket.udp",
    ),
    (
        "ngx::socket::udp::setpeername",
        0,
        HostParamPassing::Borrow,
        "ngx.socket.udp",
    ),
    (
        "ngx::socket::udp::send",
        0,
        HostParamPassing::Borrow,
        "ngx.socket.udp",
    ),
    (
        "ngx::socket::udp::receive",
        0,
        HostParamPassing::Borrow,
        "ngx.socket.udp",
    ),
    (
        "ngx::socket::udp::close",
        0,
        HostParamPassing::Borrow,
        "ngx.socket.udp",
    ),
    (
        "ngx::socket::udp::settimeout",
        0,
        HostParamPassing::Borrow,
        "ngx.socket.udp",
    ),
    (
        "ngx::socket::tcp::connect",
        0,
        HostParamPassing::Borrow,
        "ngx.socket.tcp",
    ),
    (
        "ngx::socket::tcp::send",
        0,
        HostParamPassing::Borrow,
        "ngx.socket.tcp",
    ),
    (
        "ngx::socket::tcp::receive",
        0,
        HostParamPassing::Borrow,
        "ngx.socket.tcp",
    ),
    (
        "ngx::socket::tcp::receiveany",
        0,
        HostParamPassing::Borrow,
        "ngx.socket.tcp",
    ),
    (
        "ngx::socket::tcp::receiveuntil",
        0,
        HostParamPassing::Borrow,
        "ngx.socket.tcp",
    ),
    (
        "ngx::socket::tcp::close",
        0,
        HostParamPassing::Borrow,
        "ngx.socket.tcp",
    ),
    (
        "ngx::socket::tcp::settimeout",
        0,
        HostParamPassing::Borrow,
        "ngx.socket.tcp",
    ),
    (
        "ngx::socket::tcp::setkeepalive",
        0,
        HostParamPassing::Borrow,
        "ngx.socket.tcp",
    ),
    (
        "ngx::socket::tcp::getreusedtimes",
        0,
        HostParamPassing::Borrow,
        "ngx.socket.tcp",
    ),
    (
        "ngx::socket::tcp::sslhandshake",
        0,
        HostParamPassing::Borrow,
        "ngx.socket.tcp",
    ),
    (
        "ngx::socket::tcp::settrustedstore",
        0,
        HostParamPassing::Borrow,
        "ngx.socket.tcp",
    ),
    (
        "ngx::socket::tcp::settrustedstore",
        1,
        HostParamPassing::Borrow,
        "ngx.socket.tls.trustedstore",
    ),
    (
        "ngx::socket::tcp::setclientcert",
        0,
        HostParamPassing::Borrow,
        "ngx.socket.tcp",
    ),
    (
        "ngx::socket::tcp::setclientcert",
        1,
        HostParamPassing::Borrow,
        "ngx.socket.tls.clientcert",
    ),
];

#[test]
fn raw_guest_token_slots_are_descriptor_tied() {
    let catalog = ngx_host_catalog();
    for (name, slot, passing, key) in RAW_GUEST_TOKEN_PARAMS {
        let schema = catalog
            .function(name)
            .unwrap_or_else(|| panic!("missing {name}"));
        let param = schema
            .params
            .get(*slot)
            .unwrap_or_else(|| panic!("{name} missing slot {slot}"));
        match &param.ty {
            HostTypeSchema::Resource(resource) => {
                assert_eq!(resource.as_str(), *key, "{name} slot {slot}");
            }
            other => panic!("{name} slot {slot} must be Resource({key}), got {other:?}"),
        }
        assert_eq!(param.passing, *passing, "{name} slot {slot} passing mode");
        let descriptor = descriptor_named(name);
        assert!(
            guest_effects(&descriptor)
                .iter()
                .any(|effect| match effect {
                    ResourceEffect::Borrow { key: advertised } => advertised.as_str() == *key,
                    _ => false,
                }),
            "{name} slot {slot} must advertise Borrow({key})"
        );
    }
}

#[test]
fn request_ctx_is_not_a_vm_resource() {
    let catalog = ngx_host_catalog();
    for name in [
        NGX_CTX_GET_NAME,
        NGX_CTX_SET_NAME,
        NGX_CTX_DELETE_NAME,
        NGX_CTX_CLEAR_NAME,
    ] {
        let schema = catalog.function(name).expect(name);
        assert!(
            resource_param_slots(schema).is_empty(),
            "{name} must not take a VM resource handle"
        );
        assert!(
            !matches!(schema.return_type, HostTypeSchema::Resource(_)),
            "{name} must not return a VM resource handle"
        );
    }
}
