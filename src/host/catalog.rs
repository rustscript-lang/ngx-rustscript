//! Handwritten configuration-time metadata for production ngx hosts.
//!
//! The pinned `pd-host-function` macro does not emit catalog, phase, or
//! capability data. This module is the import allowlist and config-time
//! validator. Runtime dispatch stays in [`crate::host::bind_ngx_hosts`].

use std::fmt;

use vm::{HostApiBuilder, HostApiCatalog, HostFunctionSchema, HostParamSchema, HostTypeSchema};

/// Import name compiled into access-phase programs for OpenResty-style sleep.
pub const NGX_SLEEP_NAME: &str = "ngx::sleep";

/// Millisecond sleep kept for existing scripts that used integer delays.
pub const NGX_SLEEP_MS_NAME: &str = "ngx::sleep_ms";

/// NGINX / OpenResty execution phases that can host a script.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NgxPhase {
    Init,
    InitWorker,
    Set,
    Rewrite,
    Access,
    Content,
    HeaderFilter,
    BodyFilter,
    Log,
    Balancer,
    SslCertificate,
    SslSessionFetch,
    SslSessionStore,
    Timer,
}

impl fmt::Display for NgxPhase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Init => "init",
            Self::InitWorker => "init_worker",
            Self::Set => "set",
            Self::Rewrite => "rewrite",
            Self::Access => "access",
            Self::Content => "content",
            Self::HeaderFilter => "header_filter",
            Self::BodyFilter => "body_filter",
            Self::Log => "log",
            Self::Balancer => "balancer",
            Self::SslCertificate => "ssl_certificate",
            Self::SslSessionFetch => "ssl_session_fetch",
            Self::SslSessionStore => "ssl_session_store",
            Self::Timer => "timer",
        })
    }
}

/// Host family grouping used by the manifest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostFamily {
    Timer,
}

/// Whether a host submits a future or returns synchronously.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostSyncKind {
    Sync,
    Async,
}

/// Config-time capability tags. Catalog metadata only; the pinned compiler
/// APIs do not enforce these.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostCapability {
    Timer,
}

/// One production ngx host. Schema fields stay next to the catalog builder.
pub struct HostManifestEntry {
    pub name: &'static str,
    pub family: HostFamily,
    pub sync: HostSyncKind,
    pub phases: &'static [NgxPhase],
    pub capabilities: &'static [HostCapability],
    pub openresty_name: &'static str,
}

const SLEEP_PHASES: &[NgxPhase] = &[
    NgxPhase::Rewrite,
    NgxPhase::Access,
    NgxPhase::Content,
    NgxPhase::Timer,
    NgxPhase::SslCertificate,
    NgxPhase::SslSessionFetch,
];

const TIMER_CAPABILITIES: &[HostCapability] = &[HostCapability::Timer];

const MANIFEST: &[HostManifestEntry] = &[
    HostManifestEntry {
        name: NGX_SLEEP_NAME,
        family: HostFamily::Timer,
        sync: HostSyncKind::Async,
        phases: SLEEP_PHASES,
        capabilities: TIMER_CAPABILITIES,
        openresty_name: "ngx.sleep",
    },
    HostManifestEntry {
        name: NGX_SLEEP_MS_NAME,
        family: HostFamily::Timer,
        sync: HostSyncKind::Async,
        phases: SLEEP_PHASES,
        capabilities: TIMER_CAPABILITIES,
        openresty_name: "ngx.sleep",
    },
];

/// Auditable list of production ngx hosts.
pub fn ngx_host_manifest() -> &'static [HostManifestEntry] {
    MANIFEST
}

fn sleep_schema() -> HostFunctionSchema {
    HostFunctionSchema::with_return(
        NGX_SLEEP_NAME,
        vec![HostParamSchema::value("seconds", HostTypeSchema::Number)],
        HostTypeSchema::Null,
    )
}

fn sleep_ms_schema() -> HostFunctionSchema {
    HostFunctionSchema::with_return(
        NGX_SLEEP_MS_NAME,
        vec![HostParamSchema::value("ms", HostTypeSchema::Int)],
        HostTypeSchema::Null,
    )
}

fn schema_for(name: &str) -> HostFunctionSchema {
    match name {
        NGX_SLEEP_NAME => sleep_schema(),
        NGX_SLEEP_MS_NAME => sleep_ms_schema(),
        other => panic!("ngx host catalog has no schema for {other}"),
    }
}

/// Compiler catalog for production ngx imports.
///
/// This is compiler import-allowlist metadata, not a runtime binding. The
/// pinned `pd-host-function` macro generates the wrapper, argument conversion,
/// and `submit_host_future` call; it does not emit catalog APIs. Bindings stay
/// exclusively macro-generated via [`crate::host::bind_ngx_hosts`].
pub fn ngx_host_catalog() -> HostApiCatalog {
    let mut builder = HostApiBuilder::new();
    for entry in MANIFEST {
        builder.function(schema_for(entry.name));
    }
    builder
        .build()
        .expect("ngx host catalog must be a valid HostApiCatalog")
}

/// True when `name` is a catalogued host allowed in `phase`.
pub fn host_allowed_in_phase(name: &str, phase: NgxPhase) -> bool {
    MANIFEST
        .iter()
        .find(|entry| entry.name == name)
        .is_some_and(|entry| entry.phases.contains(&phase))
}

/// Look up a production manifest entry by import name.
pub fn manifest_entry(name: &str) -> Option<&'static HostManifestEntry> {
    MANIFEST.iter().find(|entry| entry.name == name)
}
